//! Cross-process deduplication of online checks: several processes sharing one
//! license file make a single network call between them.
//!
//! The test binary re-runs itself as the child processes, so this works the same
//! on Windows as on POSIX, with no fork().

mod common;

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use common::*;
use moonbase::{
    FileLicenseStore, HttpRequest, HttpResponse, HttpTransport, LicenseStore, Licensing, StaticDeviceIdResolver,
};
use serde_json::json;

const CHILD_ENV: &str = "MOONBASE_DEDUP_CHILD";

/// Appends a line to a shared file, under a lock, for every request sent.
struct CountingFileTransport {
    counter: PathBuf,
    body: String,
}

impl HttpTransport for CountingFileTransport {
    fn send(&self, _request: &HttpRequest) -> moonbase::Result<HttpResponse> {
        let mut file = OpenOptions::new().read(true).create(true).append(true).open(&self.counter).unwrap();
        file.lock().unwrap();
        writeln!(file, "sent").unwrap();
        file.unlock().unwrap();
        Ok(HttpResponse::new(200, self.body.clone()))
    }
}

fn licensing(store: &PathBuf, counter: PathBuf, body: String) -> Licensing {
    Licensing::builder(options())
        .store(Arc::new(FileLicenseStore::new(store)))
        .device_id_resolver(Arc::new(StaticDeviceIdResolver::new("Test Device", DEVICE_ID)))
        .transport(Arc::new(CountingFileTransport { counter, body }))
        .build()
        .unwrap()
}

#[test]
fn validate_online_deduplicates_across_processes() {
    if let Ok(dir) = std::env::var(CHILD_ENV) {
        let dir = PathBuf::from(dir);
        let stale = std::fs::read_to_string(dir.join("stale.jwt")).unwrap();
        let refreshed = std::fs::read_to_string(dir.join("refreshed.jwt")).unwrap();
        let licensing = licensing(&dir.join("license.mb"), dir.join("calls.log"), refreshed);
        licensing.validate_token_online(&stale).unwrap();
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let stale = make_token(&with(claims(), "validated", json!(now_seconds() - 10 * 60)));
    let refreshed = make_token(&with(claims(), "validated", json!(now_seconds())));
    std::fs::write(dir.path().join("stale.jwt"), &stale).unwrap();
    std::fs::write(dir.path().join("refreshed.jwt"), &refreshed).unwrap();

    // Seed the shared file with the stale license, so every child starts on the
    // "must check online" path.
    let seeding = licensing(&dir.path().join("license.mb"), dir.path().join("unused.log"), String::new());
    seeding.store().store_local_license(&seeding.validate_token_local(&stale).unwrap()).unwrap();

    let children: Vec<_> = (0..6)
        .map(|_| {
            Command::new(std::env::current_exe().unwrap())
                .args(["validate_online_deduplicates_across_processes", "--exact", "--test-threads=1"])
                .env(CHILD_ENV, dir.path())
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }

    let calls = std::fs::read_to_string(dir.path().join("calls.log")).unwrap_or_default();
    assert_eq!(calls.lines().count(), 1, "exactly one process may reach the network");
    let stored = FileLicenseStore::new(dir.path().join("license.mb")).load_local_license().unwrap().unwrap();
    assert_eq!(stored.token, refreshed);
}
