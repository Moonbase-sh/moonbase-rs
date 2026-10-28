//! Where a validated license is kept between runs.
//!
//! A store is a cache, not a credential: the SDK re-derives every field from the
//! signed token rather than trusting what it read back.

use std::fs;
use std::io::Write as _;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, ThreadId};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{Result, StorageError};
use crate::file_lock::FileLock;
use crate::types::License;

trait Held {}
impl<T> Held for T {}

/// Exclusive access to a store for as long as it is held, across processes when
/// the store is backed by something shared, such as a file. Returned by
/// [`LicenseStore::lock_for_update`].
#[must_use = "the lock is released as soon as the guard is dropped"]
pub struct StoreLockGuard<'a>(#[allow(dead_code)] Box<dyn Held + 'a>);

impl<'a> StoreLockGuard<'a> {
    /// Wrap whatever keeps the lock held; dropping the guard drops it.
    pub fn new<T: 'a>(held: T) -> Self {
        Self(Box::new(held))
    }
}

impl std::fmt::Debug for StoreLockGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreLockGuard")
    }
}

/// Persists the validated license.
pub trait LicenseStore: Send + Sync {
    fn load_local_license(&self) -> Result<Option<License>>;
    fn store_local_license(&self, license: &License) -> Result<()>;
    fn delete_local_license(&self) -> Result<()>;

    /// Take an exclusive lock spanning the load/validate/store critical section in
    /// [`Licensing::validate_token_online`](crate::Licensing::validate_token_online)
    /// and [`Licensing::revoke_activation`](crate::Licensing::revoke_activation).
    ///
    /// `None` when the store needs no coordination; the SDK then relies on its
    /// in-process mutex only. The holding thread must still be able to call the
    /// store's other methods.
    fn lock_for_update(&self) -> Result<Option<StoreLockGuard<'_>>> {
        Ok(None)
    }

    /// The file backing the store, if there is one, so an integration can notice
    /// another process changing it.
    fn file_path(&self) -> Option<&Path> {
        None
    }
}

/// A shared store is a store, so one can be handed to the SDK and kept too.
impl<T: LicenseStore + ?Sized> LicenseStore for Arc<T> {
    fn load_local_license(&self) -> Result<Option<License>> {
        (**self).load_local_license()
    }

    fn store_local_license(&self, license: &License) -> Result<()> {
        (**self).store_local_license(license)
    }

    fn delete_local_license(&self) -> Result<()> {
        (**self).delete_local_license()
    }

    fn lock_for_update(&self) -> Result<Option<StoreLockGuard<'_>>> {
        (**self).lock_for_update()
    }

    fn file_path(&self) -> Option<&Path> {
        (**self).file_path()
    }
}

/// A lock the holding thread can re-enter: a thread inside
/// [`LicenseStore::lock_for_update`] can still load and store.
#[derive(Default)]
struct ReentrantGate {
    state: Mutex<(Option<ThreadId>, usize)>,
    released: Condvar,
}

struct GateTicket<'a> {
    gate: &'a ReentrantGate,
    // Released on the thread that entered.
    _not_send: PhantomData<*const ()>,
}

impl ReentrantGate {
    fn enter(&self) -> GateTicket<'_> {
        let me = thread::current().id();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            match state.0 {
                None => {
                    *state = (Some(me), 1);
                    break;
                }
                Some(owner) if owner == me => {
                    state.1 += 1;
                    break;
                }
                Some(_) => state = self.released.wait(state).unwrap_or_else(PoisonError::into_inner),
            }
        }
        GateTicket { gate: self, _not_send: PhantomData }
    }
}

impl Drop for GateTicket<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.1 -= 1;
        if state.1 == 0 {
            state.0 = None;
            self.gate.released.notify_all();
        }
    }
}

/// Keeps the license in memory for the life of the process. The default, so
/// nothing is persisted unless you choose a store.
#[derive(Default)]
pub struct MemoryLicenseStore {
    gate: ReentrantGate,
    value: Mutex<Option<License>>,
}

impl MemoryLicenseStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl LicenseStore for MemoryLicenseStore {
    fn load_local_license(&self) -> Result<Option<License>> {
        let _ticket = self.gate.enter();
        Ok(self.value.lock().unwrap_or_else(PoisonError::into_inner).clone())
    }

    fn store_local_license(&self, license: &License) -> Result<()> {
        let _ticket = self.gate.enter();
        *self.value.lock().unwrap_or_else(PoisonError::into_inner) = Some(license.clone());
        Ok(())
    }

    fn delete_local_license(&self) -> Result<()> {
        let _ticket = self.gate.enter();
        *self.value.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }

    /// Serializes the validate-then-persist section against a delete on another
    /// thread, which could otherwise land between the decision to persist and the
    /// write, and resurrect a license the user just cleared.
    fn lock_for_update(&self) -> Result<Option<StoreLockGuard<'_>>> {
        Ok(Some(StoreLockGuard::new(self.gate.enter())))
    }
}

impl std::fmt::Debug for MemoryLicenseStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryLicenseStore").finish_non_exhaustive()
    }
}

/// Persists the validated license as JSON in a file, in the same format as the C++
/// SDK's `file_license_store`.
///
/// Writes go to a temporary file beside the license that is then renamed into
/// place, so a reader sees the old license or the new one and never half of one.
/// Instances sharing the file take turns in `validate_token_online` through a
/// `<path>.lock` sidecar, so one may wait for another's network check; call
/// [`cancel_lock_waits`](Self::cancel_lock_waits) when shutting down.
///
/// Clones share the cancellation flag.
#[derive(Debug, Clone)]
pub struct FileLicenseStore {
    path: PathBuf,
    lock_waits_cancelled: Arc<AtomicBool>,
}

impl FileLicenseStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock_waits_cancelled: Arc::new(AtomicBool::new(false)) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// For shutting down: from here on [`lock_for_update`](LicenseStore::lock_for_update)
    /// fails with [`Error::Storage`](crate::Error::Storage) instead of waiting for a lock another instance
    /// holds, and a call already waiting gives up the same way. A free lock is
    /// still taken. Callable from any thread; it cannot be undone.
    pub fn cancel_lock_waits(&self) {
        self.lock_waits_cancelled.store(true, Ordering::Release);
    }

    fn lock_path(&self) -> PathBuf {
        let mut path = self.path.clone().into_os_string();
        path.push(".lock");
        path.into()
    }

    /// Write `contents` beside the license and rename it over the license. False,
    /// with the temporary file removed again, if any step fails or the rename
    /// would change more than the contents.
    fn replace_with_temp_file(&self, contents: &str) -> bool {
        // Follow a symlinked license to the file itself, so the link keeps pointing
        // at the license instead of being replaced by a copy of it.
        let target = match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.file_type().is_symlink() => match fs::canonicalize(&self.path) {
                Ok(target) => target,
                // A dangling link: writing in place creates its target.
                Err(_) => return false,
            },
            _ => self.path.clone(),
        };

        // Only POSIX permissions and link counts are carried over; on Windows the
        // former are just the read-only flag.
        #[cfg(unix)]
        let existing = fs::metadata(&target).ok();
        // A license also known by another name (a hard link) would keep the old
        // contents there.
        #[cfg(unix)]
        if existing.as_ref().is_some_and(|metadata| {
            use std::os::unix::fs::MetadataExt as _;
            metadata.nlink() != 1
        }) {
            return false;
        }

        let mut temp = target.clone().into_os_string();
        temp.push(temp_suffix());
        let temp = PathBuf::from(temp);

        let written = (|| -> std::io::Result<()> {
            let mut file = fs::File::create(&temp)?;
            // Keep the license's permissions, so one restricted to its owner stays
            // that way, and set them before the contents land.
            #[cfg(unix)]
            if let Some(metadata) = &existing {
                fs::set_permissions(&temp, metadata.permissions())?;
            }
            file.write_all(contents.as_bytes())?;
            file.flush()
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temp);
            return false;
        }

        if retry_while_in_use(|| fs::rename(&temp, &target)).is_ok() {
            return true;
        }
        let _ = fs::remove_file(&temp);
        false
    }
}

impl LicenseStore for FileLicenseStore {
    fn load_local_license(&self) -> Result<Option<License>> {
        let bytes = match retry_while_in_use(|| fs::read(&self.path)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(StorageError::io(
                    format!("Could not open local license file for reading: {}", self.path.display()),
                    error,
                )
                .into());
            }
        };

        match serde_json::from_slice::<License>(&bytes) {
            Ok(license) => Ok(Some(license)),
            Err(_) => {
                // A corrupt license file is useless and would fail every load,
                // blocking re-activation. Remove it, best-effort, and report none,
                // so a fresh activation can be written in its place.
                let _ = fs::remove_file(&self.path);
                Ok(None)
            }
        }
    }

    fn store_local_license(&self, license: &License) -> Result<()> {
        if let Some(parent) = self.path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|error| {
                StorageError::io(format!("Could not create license directory {}", parent.display()), error)
            })?;
        }

        let contents = serde_json::to_string_pretty(license)
            .map_err(|error| StorageError::new(format!("Could not serialize the license: {error}")))?;

        if self.replace_with_temp_file(&contents) {
            return Ok(());
        }

        // Where that fails, as in a folder that takes no new files but lets the
        // license itself be rewritten, write it in place.
        let mut file = fs::File::create(&self.path).map_err(|error| {
            StorageError::io(format!("Could not open local license file for writing: {}", self.path.display()), error)
        })?;
        file.write_all(contents.as_bytes()).and_then(|()| file.flush()).map_err(|error| {
            StorageError::io(format!("Could not write local license file: {}", self.path.display()), error).into()
        })
    }

    fn delete_local_license(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(StorageError::io(format!("Could not delete local license file {}", self.path.display()), error)
                    .into())
            }
        }
    }

    fn lock_for_update(&self) -> Result<Option<StoreLockGuard<'_>>> {
        let lock = FileLock::acquire(&self.lock_path(), &self.lock_waits_cancelled)?;
        Ok(Some(StoreLockGuard::new(lock)))
    }

    fn file_path(&self) -> Option<&Path> {
        Some(&self.path)
    }
}

/// Windows will not open or replace a file while another handle holds it in a
/// conflicting way: a sibling reading the license does for a moment, and so may a
/// virus scanner. There, a failed attempt is retried a few times; elsewhere it
/// runs once.
fn retry_while_in_use<T>(mut attempt: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let attempts = if cfg!(windows) { 10 } else { 1 };
    let mut pause = Duration::from_millis(1);
    let mut tried = 1;
    loop {
        match attempt() {
            Err(error) if tried < attempts && error.kind() != std::io::ErrorKind::NotFound => {
                thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(100));
                tried += 1;
            }
            result => return result,
        }
    }
}

/// Unique to one write, so writers never share a temporary file, whether in
/// different processes, threads, or two plugins loaded into one host.
fn temp_suffix() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos());
    format!(".{}-{nanos}-{}.tmp", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed))
}
