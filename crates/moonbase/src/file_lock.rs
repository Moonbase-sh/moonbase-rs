use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::error::{Result, StorageError};

const MAX_PAUSE: Duration = Duration::from_millis(20);

/// An exclusive lock on a dedicated sidecar file, held until dropped.
///
/// The sidecar (`<license>.lock`) is created on first use and never deleted, so
/// its inode outlives the license file: locking the license itself would orphan
/// the lock on POSIX the moment `delete_local_license` unlinked it, and a sibling
/// could then lock a fresh inode at the same path.
///
/// The lock is requested without blocking and retried every few milliseconds
/// while someone else holds it, which can be for the length of their network
/// check. A blocking request could not be called off, so an app closing while it
/// waited would hang until that check finished. Setting `give_up` ends the wait
/// with [`Error::Storage`](crate::Error::Storage) instead.
#[derive(Debug)]
pub(crate) struct FileLock {
    file: File,
}

impl FileLock {
    pub(crate) fn acquire(path: &Path, give_up: &AtomicBool) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            // Failure is non-fatal here: the open below reports it more clearly.
            let _ = std::fs::create_dir_all(parent);
        }
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(|error| {
                StorageError::io(format!("Could not open license lock file {}", path.display()), error)
            })?;

        let mut pause = Duration::from_millis(1);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(error)) => {
                    return Err(StorageError::io(format!("Could not lock {}", path.display()), error).into());
                }
            }
            if give_up.load(Ordering::Acquire) {
                return Err(StorageError::new("Stopped waiting for the license file lock").into());
            }
            thread::sleep(pause);
            pause = (pause * 2).min(MAX_PAUSE);
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
