//! License stores: round trips, atomic replacement, and the cross-process lock.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use moonbase::{
    ActivationMethod, Error, FileLicenseStore, License, LicenseStore, MemoryLicenseStore, Product, ReleaseAccess,
    SubProduct, SubProductSeats, User,
};
use serde_json::json;

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn sample_license() -> License {
    License {
        id: "license-123".into(),
        activation_id: "activation-123".into(),
        trial: false,
        method: ActivationMethod::Online,
        licensed_product: Product {
            id: "demo-app".into(),
            name: "Demo Product".into(),
            current_release_version: Some("1.2.3".into()),
            properties: json!({"tier": "pro"}),
        },
        issued_to: User {
            id: "user-123".into(),
            name: "Jane Developer".into(),
            email: "jane@example.com".into(),
            properties: json!({"company": "Acme"}),
        },
        issued_at: at(1_700_000_000),
        expires_at: Some(at(1_800_000_000)),
        validated_at: at(1_750_000_000),
        owned_sub_product_ids: vec!["demo-app-pro".into()],
        sub_products: vec![
            SubProduct {
                id: "pack".into(),
                active: true,
                trial: false,
                revoked: false,
                expires_at: None,
                seats: Some(SubProductSeats { total: 3, used: 1, remaining: 2 }),
            },
            SubProduct {
                id: "strings".into(),
                active: true,
                trial: true,
                revoked: false,
                expires_at: Some(at(1_798_761_600)),
                seats: None,
            },
            SubProduct { id: "keys".into(), active: false, trial: false, revoked: true, expires_at: None, seats: None },
        ],
        subscription_id: Some("subscription-123".into()),
        seat_count: Some(3),
        seats_used: Some(1),
        release_access: Some(ReleaseAccess {
            updates_until: Some(at(1_780_272_000)),
            up_to_major: None,
            up_to_version: Some("2.3.0".into()),
        }),
        properties: json!({"seats": 3, "features": ["export"]}),
        token: "header.payload.signature".into(),
    }
}

#[test]
fn the_memory_store_round_trips_and_deletes() {
    let store = MemoryLicenseStore::new();
    assert!(store.load_local_license().unwrap().is_none());
    store.store_local_license(&sample_license()).unwrap();
    assert_eq!(store.load_local_license().unwrap(), Some(sample_license()));
    store.delete_local_license().unwrap();
    assert!(store.load_local_license().unwrap().is_none());
}

#[test]
fn the_memory_store_lock_blocks_other_threads_but_not_the_holder() {
    let store = Arc::new(MemoryLicenseStore::new());
    let guard = store.lock_for_update().unwrap();
    // Re-entrant on the holding thread.
    store.store_local_license(&sample_license()).unwrap();
    assert!(store.load_local_license().unwrap().is_some());

    let deleted = Arc::new(AtomicBool::new(false));
    let deleting = {
        let (store, deleted) = (store.clone(), deleted.clone());
        thread::spawn(move || {
            store.delete_local_license().unwrap();
            deleted.store(true, Ordering::SeqCst);
        })
    };
    thread::sleep(Duration::from_millis(100));
    assert!(!deleted.load(Ordering::SeqCst), "a delete must wait for the update lock");
    drop(guard);
    deleting.join().unwrap();
    assert!(store.load_local_license().unwrap().is_none());
}

#[test]
fn the_file_store_round_trips_in_the_cpp_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/license.mb");
    let store = FileLicenseStore::new(&path);

    assert!(store.load_local_license().unwrap().is_none());
    store.store_local_license(&sample_license()).unwrap();
    assert_eq!(store.load_local_license().unwrap(), Some(sample_license()));

    // The same JSON as moonbase-cpp's file_license_store, so the two can share a file.
    let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(written["activationId"], "activation-123");
    assert_eq!(written["activationMethod"], "Online");
    assert_eq!(written["issuedAt"], "2023-11-14T22:13:20Z");
    assert_eq!(written["expiresAt"], "2027-01-15T08:00:00Z");
    assert_eq!(written["product"]["currentReleaseVersion"], "1.2.3");
    assert_eq!(written["issuedTo"]["email"], "jane@example.com");
    assert_eq!(written["seatCount"], 3);

    store.delete_local_license().unwrap();
    assert!(!path.exists());
    // Deleting nothing is not an error.
    store.delete_local_license().unwrap();
}

#[test]
fn the_file_store_reads_a_license_without_optional_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("license.mb");
    std::fs::write(
        &path,
        r#"{"id":"l","activationId":"a","trial":true,"activationMethod":"offline",
            "product":{"id":"p","name":"P"},"issuedTo":{"id":"u","name":"U","email":"e"},
            "issuedAt":"2024-01-01T00:00:00Z","validatedAt":"2024-01-01T00:00:00Z","token":"t"}"#,
    )
    .unwrap();
    let license = FileLicenseStore::new(&path).load_local_license().unwrap().unwrap();
    assert_eq!(license.method, ActivationMethod::Offline);
    assert!(license.expires_at.is_none());
    assert!(license.seat_count.is_none());
    assert_eq!(license.properties, json!({}));
}

#[test]
fn the_file_store_removes_an_unparseable_license_and_reports_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("license.mb");
    std::fs::write(&path, "{not json").unwrap();
    let store = FileLicenseStore::new(&path);
    assert!(store.load_local_license().unwrap().is_none());
    assert!(!path.exists());
    // A fresh activation can be written in its place.
    store.store_local_license(&sample_license()).unwrap();
    assert_eq!(store.load_local_license().unwrap().unwrap().id, "license-123");
}

#[test]
fn readers_never_catch_the_license_half_written() {
    // Readers do not take the lock, and a load deletes a file it cannot parse, so
    // a reader that caught a write half done would delete the license.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("license.mb");
    let small = sample_license();
    let mut large = sample_license();
    large.token = "x".repeat(64 * 1024);

    let writer = FileLicenseStore::new(&path);
    writer.store_local_license(&small).unwrap();

    let done = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let misses = Arc::new(AtomicUsize::new(0));
    let reading = {
        let (path, done, reads, misses) = (path.clone(), done.clone(), reads.clone(), misses.clone());
        thread::spawn(move || {
            let reader = FileLicenseStore::new(path);
            while !done.load(Ordering::SeqCst) {
                if !matches!(reader.load_local_license(), Ok(Some(_))) {
                    misses.fetch_add(1, Ordering::SeqCst);
                }
                reads.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(2));
            }
        })
    };

    let mut index = 0;
    while reads.load(Ordering::SeqCst) < 100 && index < 100_000 {
        writer.store_local_license(if index % 2 == 0 { &large } else { &small }).unwrap();
        index += 1;
    }
    done.store(true, Ordering::SeqCst);
    reading.join().unwrap();

    assert!(reads.load(Ordering::SeqCst) > 0);
    assert_eq!(misses.load(Ordering::SeqCst), 0);
    // Nothing left beside it but the license itself.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn a_hard_linked_license_stays_shared() {
    // Renaming a new file into place would replace only the name written through.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("license.mb");
    let alias = dir.path().join("alias.mb");
    FileLicenseStore::new(&path).store_local_license(&sample_license()).unwrap();
    std::fs::hard_link(&path, &alias).unwrap();

    let mut updated = sample_license();
    updated.id = "license-456".into();
    FileLicenseStore::new(&alias).store_local_license(&updated).unwrap();
    assert_eq!(FileLicenseStore::new(&path).load_local_license().unwrap().unwrap().id, "license-456");
}

#[cfg(unix)]
#[test]
fn a_symlinked_license_path_is_written_through() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real.mb");
    let link = dir.path().join("license.mb");
    FileLicenseStore::new(&target).store_local_license(&sample_license()).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let mut updated = sample_license();
    updated.id = "license-456".into();
    FileLicenseStore::new(&link).store_local_license(&updated).unwrap();

    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(FileLicenseStore::new(&target).load_local_license().unwrap().unwrap().id, "license-456");
}

#[cfg(unix)]
#[test]
fn a_license_restricted_to_its_owner_stays_that_way() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("license.mb");
    let store = FileLicenseStore::new(&path);
    store.store_local_license(&sample_license()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    store.store_local_license(&sample_license()).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn a_folder_that_takes_no_new_files_still_lets_the_license_be_rewritten() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("locked");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("license.mb");
    let store = FileLicenseStore::new(&path);
    store.store_local_license(&sample_license()).unwrap();

    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut updated = sample_license();
    updated.id = "license-456".into();
    let result = store.store_local_license(&updated);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Root ignores the folder's mode, so only the outcome is checked.
    result.unwrap();
    assert_eq!(store.load_local_license().unwrap().unwrap().id, "license-456");
}

#[cfg(unix)]
#[test]
fn an_unwritable_location_is_a_storage_error_naming_the_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("read-only");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    let path = folder.join("Björn/license.mb");
    let result = FileLicenseStore::new(&path).store_local_license(&sample_license());
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();

    if let Err(error) = result {
        // Says where, and keeps the I/O failure as its source.
        let Error::Storage(storage) = &error else { panic!("{error:?}") };
        assert!(storage.message().contains("Björn"), "{error:?}");
        assert_eq!(storage.io_error().map(std::io::Error::kind), Some(std::io::ErrorKind::PermissionDenied));
        assert!(std::error::Error::source(&error).is_some());
    }
}

/// How many threads were ever inside the lock at once.
fn max_concurrency_under_lock(path: &std::path::Path, mutate: bool) -> usize {
    let inside = Arc::new(AtomicUsize::new(0));
    let max_inside = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let (path, inside, max_inside, barrier) =
                (path.to_owned(), inside.clone(), max_inside.clone(), barrier.clone());
            thread::spawn(move || {
                // Separate instances, as two app instances or processes would be.
                let store = FileLicenseStore::new(path);
                barrier.wait();
                let guard = store.lock_for_update().unwrap();
                assert!(guard.is_some());
                let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                max_inside.fetch_max(now, Ordering::SeqCst);
                if mutate {
                    // Unlinking the license must not orphan the lock.
                    let _ = store.delete_local_license();
                    thread::sleep(Duration::from_millis(20));
                    let _ = store.store_local_license(&sample_license());
                } else {
                    thread::sleep(Duration::from_millis(40));
                }
                inside.fetch_sub(1, Ordering::SeqCst);
            })
        })
        .collect();
    threads.into_iter().for_each(|thread| thread.join().unwrap());
    max_inside.load(Ordering::SeqCst)
}

#[test]
fn the_file_lock_serializes_concurrent_acquirers() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(max_concurrency_under_lock(&dir.path().join("license.mb"), false), 1);
}

#[test]
fn the_file_lock_survives_the_license_being_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("license.mb");
    FileLicenseStore::new(&path).store_local_license(&sample_license()).unwrap();
    assert_eq!(max_concurrency_under_lock(&path, true), 1);
}

#[test]
fn cancel_lock_waits_ends_a_wait_for_a_lock_held_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("license.mb");
    let sibling = FileLicenseStore::new(&path);
    let store = FileLicenseStore::new(&path);
    let held = sibling.lock_for_update().unwrap();

    let gave_up = Arc::new(AtomicBool::new(false));
    let waiting = {
        let (store, gave_up) = (store.clone(), gave_up.clone());
        thread::spawn(move || {
            if let Err(Error::Storage(_)) = store.lock_for_update() {
                gave_up.store(true, Ordering::SeqCst);
            }
        })
    };
    thread::sleep(Duration::from_millis(100));
    // A clone shares the flag, so cancelling any copy ends the wait.
    store.clone().cancel_lock_waits();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !waiting.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert!(waiting.is_finished());
    drop(held);
    waiting.join().unwrap();
    assert!(gave_up.load(Ordering::SeqCst));

    // Later calls do not wait for a taken lock either, but still take a free one.
    let held = sibling.lock_for_update().unwrap();
    assert!(matches!(store.lock_for_update(), Err(Error::Storage(_))));
    drop(held);
    assert!(store.lock_for_update().unwrap().is_some());
}
