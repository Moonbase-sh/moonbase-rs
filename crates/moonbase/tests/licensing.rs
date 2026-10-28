//! `Licensing`: throttling, the offline grace period, offline licenses, revocation
//! and the cross-instance deduplication of online checks.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use common::*;
use moonbase::{
    ActivationMethod, Error, HttpRequest, HttpResponse, HttpTransport, License, LicenseStore, Licensing,
    MemoryLicenseStore, StaticDeviceIdResolver, StoreLockGuard,
};
use serde_json::json;

const MINUTE: i64 = 60;
const ONE_YEAR: i64 = 365 * 24 * 60 * 60;

const STORE_CLOSED: &str =
    r#"{"title":"Store closed","status":410,"detail":"This store has closed.","errorType":"StoreClosed"}"#;

fn validated_ago(seconds: i64) -> serde_json::Value {
    with(claims(), "validated", json!(now_seconds() - seconds))
}

fn fixture_options(configure: impl FnOnce(&mut moonbase::LicensingOptions)) -> Fixture {
    let mut options = options();
    configure(&mut options);
    fixture_with(options, [], Arc::new(MemoryLicenseStore::new()), "Test Device", DEVICE_ID)
}

#[test]
fn skips_the_api_within_the_min_interval() {
    let f = fixture([]);
    let license = f.licensing.validate_token_online(&make_token(&validated_ago(30))).unwrap();
    assert_eq!(license.id, "license-123");
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn calls_the_api_after_the_min_interval_and_returns_the_refreshed_license() {
    let refreshed = make_token(&validated_ago(0));
    let f = fixture([ok(refreshed.clone())]);
    let license = f.licensing.validate_token_online(&make_token(&validated_ago(10 * MINUTE))).unwrap();
    assert_eq!(license.token, refreshed);
    assert!(f.transport.requests()[0].url.contains("/api/client/licenses/demo-app/validate"));
    // Persisted, so sibling instances see the new validated_at.
    assert_eq!(f.store.load_local_license().unwrap().unwrap().token, refreshed);
}

#[test]
fn tolerates_transport_failures_within_the_grace_period() {
    let f = fixture([]);
    let stale = make_token(&validated_ago(10 * MINUTE));
    let license = f.licensing.validate_token_online(&stale).unwrap();
    assert_eq!(license.token, stale);
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn fails_when_the_transport_fails_and_grace_has_elapsed() {
    let f = fixture_options(|o| {
        o.online_validation_grace_period = Duration::from_secs(60);
        o.online_validation_min_interval = Duration::from_secs(10);
    });
    let error = f.licensing.validate_token_online(&make_token(&validated_ago(5 * MINUTE))).unwrap_err();
    assert_eq!(error.as_api_error().unwrap().status_code(), 0);
}

#[test]
fn propagates_definitive_verdicts_regardless_of_grace() {
    for (body, expired) in [
        (r#"{"title":"Invalid","detail":"bad"}"#, false),
        (r#"{"errorType":"LicenseExpired","detail":"expired"}"#, true),
    ] {
        let f = fixture_options(|o| o.online_validation_grace_period = Duration::from_secs(ONE_YEAR as u64));
        f.transport.push(HttpResponse::new(400, body));
        let error = f.licensing.validate_token_online(&make_token(&validated_ago(10 * MINUTE))).unwrap_err();
        assert!(error.is_license_rejection());
        assert_eq!(matches!(error, Error::LicenseExpired(_)), expired);
    }
}

#[test]
fn rides_out_what_only_looks_like_a_verdict_within_grace() {
    for response in [
        HttpResponse::new(404, ""),
        HttpResponse::new(
            404,
            r#"{"type":"https://tools.ietf.org/html/rfc9110#section-15.5.5","title":"Not Found","status":404}"#,
        ),
        HttpResponse::new(200, "<html><body>Sign in to continue</body></html>"),
        HttpResponse::new(429, "").with_header("Retry-After", "60"),
        HttpResponse::new(500, ""),
    ] {
        let f = fixture([response]);
        let stale = make_token(&validated_ago(10 * MINUTE));
        assert_eq!(f.licensing.validate_token_online(&stale).unwrap().token, stale);
        assert_eq!(f.transport.request_count(), 1);
    }
}

#[test]
fn a_closed_stores_offline_license_replaces_the_online_one_and_is_never_sent_again() {
    let f = fixture_options(|o| o.online_validation_grace_period = Duration::from_secs(3600));

    // Bought outright (no exp), and last checked a year ago.
    let online = with(without(claims(), "exp"), "validated", json!(now_seconds() - ONE_YEAR));
    let online_token = make_token(&online);
    // What a closed store answers: the same license, offline, signed with its key.
    let offline = with(with(without(online, "p:rel"), "method", json!("Offline")), "validated", json!(now_seconds()));
    let offline_token = make_token(&offline);
    f.transport.push(ok(offline_token.clone()));

    let license = f.licensing.validate_token_online(&online_token).unwrap();
    assert_eq!(license.method, ActivationMethod::Offline);
    assert_eq!(license.token, offline_token);
    assert!(license.expires_at.is_none());
    assert_eq!(f.store.load_local_license().unwrap().unwrap().token, offline_token);

    // From here on it is checked on the device alone.
    assert_eq!(f.licensing.validate_token_online(&offline_token).unwrap().token, offline_token);
    // An instance still holding the online token picks up the stored offline one.
    assert_eq!(f.licensing.validate_token_online(&online_token).unwrap().token, offline_token);
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn an_offline_license_another_instance_stored_is_taken_without_asking_the_api() {
    let f = fixture([]);
    // The caller still holds the activation's online token, last checked a month
    // ago, past the grace period. Another instance has since stored an offline
    // license for the activation, minted days ago: past the min interval too.
    let online_token = make_token(&validated_ago(30 * 24 * 3600));
    let offline_token = make_token(&with(validated_ago(10 * 24 * 3600), "method", json!("Offline")));
    f.store.store_local_license(&f.licensing.validate_token_local(&offline_token).unwrap()).unwrap();

    // Permanent and never checked online, so neither the network nor the grace
    // period has a say.
    let license = f.licensing.validate_token_online(&online_token).unwrap();
    assert_eq!(license.method, ActivationMethod::Offline);
    assert_eq!(license.token, offline_token);
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn an_offline_license_another_instance_stored_is_judged_against_the_version() {
    let f = fixture_options(|o| o.application_version = Some("3.0.0".into()));
    let online_token = make_token(&validated_ago(30 * 24 * 3600));
    let offline = with(with(validated_ago(10 * 24 * 3600), "method", json!("Offline")), "l:rel:major", json!(2));
    let stored = f.licensing.validate_token_local_allow_expired(&make_token(&offline)).unwrap();
    f.store.store_local_license(&stored).unwrap();

    let error = f.licensing.validate_token_online(&online_token).unwrap_err();
    assert!(matches!(&error, Error::ReleaseNotCovered(refusal) if refusal.license().unwrap().token == stored.token));
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn a_closed_store_reads_like_being_offline() {
    let f = fixture_options(|o| o.online_validation_grace_period = Duration::from_secs(3600));
    f.transport.push(HttpResponse::new(410, STORE_CLOSED));
    let token = make_token(&validated_ago(10 * MINUTE));
    assert_eq!(f.licensing.validate_token_online(&token).unwrap().token, token);

    // Past grace, the closed store is reported, and nothing is relaxed.
    let f = fixture_options(|o| o.online_validation_grace_period = Duration::from_secs(3600));
    f.transport.push(HttpResponse::new(410, STORE_CLOSED));
    let error = f.licensing.validate_token_online(&make_token(&validated_ago(2 * 3600))).unwrap_err();
    assert!(matches!(error, Error::StoreClosed(_)));
}

#[test]
fn never_contacts_the_api_for_offline_tokens() {
    let f = fixture([]);
    let token = make_token(&with(validated_ago(ONE_YEAR), "method", json!("Offline")));
    assert_eq!(f.licensing.validate_token_online(&token).unwrap().method, ActivationMethod::Offline);
    // Nor once one has expired: nothing renews it.
    let expired = make_token(&with(with(claims(), "method", json!("Offline")), "exp", json!(now_seconds() - MINUTE)));
    assert!(matches!(f.licensing.validate_token_online(&expired), Err(Error::LicenseExpired(_))));
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn runs_local_validation_before_any_http_call() {
    let f = fixture([]);
    let token = make_token(&with(claims_for("other-device"), "validated", json!(now_seconds() - 10 * MINUTE)));
    assert!(f.licensing.validate_token_online(&token).unwrap_err().is_license_invalid());
    // An expired one is asked about only once the rest of it holds up.
    let token = make_token(&with(claims_for("other-device"), "exp", json!(now_seconds() - MINUTE)));
    assert!(f.licensing.validate_token_online(&token).unwrap_err().is_license_invalid());
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn the_min_interval_cannot_extend_past_the_grace_period() {
    let f = fixture_options(|o| {
        o.online_validation_grace_period = Duration::from_secs(60);
        o.online_validation_min_interval = Duration::from_secs(3600);
    });
    // Past grace, within the throttle: it must ask the API, and propagate.
    assert!(f.licensing.validate_token_online(&make_token(&validated_ago(120))).is_err());
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn honours_a_custom_min_interval() {
    let f = fixture_options(|o| o.online_validation_min_interval = Duration::from_secs(1));
    f.transport.push(ok(make_token(&validated_ago(0))));
    f.licensing.validate_token_online(&make_token(&validated_ago(30))).unwrap();
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn prefers_the_freshest_stored_license_when_reloading_under_the_lock() {
    let f = fixture([]);
    let fresh = make_token(&validated_ago(10));
    f.store.store_local_license(&f.licensing.validate_token_local(&fresh).unwrap()).unwrap();

    let license = f.licensing.validate_token_online(&make_token(&validated_ago(10 * MINUTE))).unwrap();
    assert_eq!(license.token, fresh);
    assert_eq!(f.transport.request_count(), 0);
}

/// Claims that expired `ago` seconds ago, last validated `validated` seconds ago.
fn expired(ago: i64, validated: i64) -> serde_json::Value {
    with(validated_ago(validated), "exp", json!(now_seconds() - ago))
}

#[test]
fn an_expired_license_is_checked_with_moonbase_whatever_the_throttle() {
    // Checked two minutes ago, inside the min interval, and expired since: only
    // Moonbase knows whether the subscription renewed meanwhile.
    let token = make_token(&expired(MINUTE, 2 * MINUTE));
    let renewed = make_token(&validated_ago(0));
    let f = fixture([ok(renewed.clone())]);

    assert_eq!(f.licensing.validate_token_online(&token).unwrap().token, renewed);
    let requests = f.transport.requests();
    assert!(requests[0].url.contains("/api/client/licenses/demo-app/validate"));
    assert_eq!(requests[0].body, token);
    assert_eq!(f.store.load_local_license().unwrap().unwrap().token, renewed);
}

#[test]
fn an_expiry_stands_when_moonbase_confirms_it_or_cant_be_asked() {
    let confirmed = HttpResponse::new(400, r#"{"errorType":"LicenseExpired","detail":"The license has expired"}"#);
    for response in
        [Some(confirmed), Some(HttpResponse::new(500, "")), Some(HttpResponse::new(410, STORE_CLOSED)), None]
    {
        // Well inside the grace period, which rides out an outage but never an expiry.
        let f = fixture(response);
        let error = f.licensing.validate_token_online(&make_token(&expired(MINUTE, 10 * MINUTE))).unwrap_err();
        assert!(matches!(error, Error::LicenseExpired(_)), "{error:?}");
        assert_eq!(f.transport.request_count(), 1);
        assert_eq!(f.store.load_local_license().unwrap(), None);
    }
}

#[test]
fn an_expiry_stands_when_the_store_cant_be_locked() {
    let store = Arc::new(TrackingStore { fail_lock: true, ..TrackingStore::default() });
    let f = fixture_with(options(), [], store, "Test Device", DEVICE_ID);
    let error = f.licensing.validate_token_online(&make_token(&expired(MINUTE, 10 * MINUTE))).unwrap_err();
    assert!(matches!(error, Error::LicenseExpired(_)), "{error:?}");
    // A license that hasn't expired reports the store's failure.
    let error = f.licensing.validate_token_online(&make_token(&validated_ago(10 * MINUTE))).unwrap_err();
    assert!(matches!(error, Error::Storage(_)), "{error:?}");
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn an_expired_copy_takes_the_renewal_another_instance_stored() {
    let f = fixture([]);
    let renewed = make_token(&validated_ago(10));
    f.store.store_local_license(&f.licensing.validate_token_local(&renewed).unwrap()).unwrap();

    let license = f.licensing.validate_token_online(&make_token(&expired(MINUTE, 2 * 3600))).unwrap();
    assert_eq!(license.token, renewed);
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn should_persist_decides_whether_the_refresh_is_written() {
    for persist in [false, true] {
        let refreshed = make_token(&validated_ago(0));
        let f = fixture([ok(refreshed.clone())]);
        let calls = AtomicUsize::new(0);
        let license = f
            .licensing
            .validate_token_online_with(&make_token(&validated_ago(10 * MINUTE)), || {
                calls.fetch_add(1, Ordering::SeqCst);
                persist
            })
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(license.token, refreshed);
        assert_eq!(f.store.load_local_license().unwrap().is_some(), persist);
    }

    // Not asked on the throttle's fast path.
    let f = fixture([]);
    let calls = AtomicUsize::new(0);
    f.licensing
        .validate_token_online_with(&make_token(&validated_ago(30)), || {
            calls.fetch_add(1, Ordering::SeqCst);
            true
        })
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Thread-safe, and always answers with the same body.
struct CountingTransport {
    count: AtomicUsize,
    body: String,
}

impl HttpTransport for CountingTransport {
    fn send(&self, _request: &HttpRequest) -> moonbase::Result<HttpResponse> {
        self.count.fetch_add(1, Ordering::SeqCst);
        // Long enough that every other thread is queued behind this one.
        thread::sleep(Duration::from_millis(50));
        Ok(HttpResponse::new(200, self.body.clone()))
    }
}

#[test]
fn deduplicates_concurrent_in_process_callers() {
    let transport = Arc::new(CountingTransport { count: AtomicUsize::new(0), body: make_token(&validated_ago(0)) });
    let licensing = Arc::new(
        Licensing::builder(options())
            .device_id_resolver(Arc::new(StaticDeviceIdResolver::new("Test Device", DEVICE_ID)))
            .transport(transport.clone())
            .build()
            .unwrap(),
    );
    let stale = make_token(&validated_ago(10 * MINUTE));

    let barrier = Arc::new(Barrier::new(16));
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let (licensing, barrier, stale) = (licensing.clone(), barrier.clone(), stale.clone());
            thread::spawn(move || {
                barrier.wait();
                licensing.validate_token_online(&stale).map(|license| license.id)
            })
        })
        .collect();
    for thread in threads {
        assert_eq!(thread.join().unwrap().unwrap(), "license-123");
    }
    assert_eq!(transport.count.load(Ordering::SeqCst), 1);
}

/// Counts `lock_for_update` calls.
#[derive(Default)]
struct TrackingStore {
    locks: AtomicUsize,
    value: Mutex<Option<License>>,
    fail_io: bool,
    fail_lock: bool,
}

impl LicenseStore for TrackingStore {
    fn load_local_license(&self) -> moonbase::Result<Option<License>> {
        if self.fail_io {
            return Err(moonbase::StorageError::new("load failed").into());
        }
        Ok(self.value.lock().unwrap().clone())
    }

    fn store_local_license(&self, license: &License) -> moonbase::Result<()> {
        *self.value.lock().unwrap() = Some(license.clone());
        Ok(())
    }

    fn delete_local_license(&self) -> moonbase::Result<()> {
        if self.fail_io {
            return Err(moonbase::StorageError::new("delete failed").into());
        }
        *self.value.lock().unwrap() = None;
        Ok(())
    }

    fn lock_for_update(&self) -> moonbase::Result<Option<StoreLockGuard<'_>>> {
        self.locks.fetch_add(1, Ordering::SeqCst);
        if self.fail_lock {
            return Err(moonbase::StorageError::new("lock failed").into());
        }
        Ok(Some(StoreLockGuard::new(())))
    }
}

#[test]
fn takes_the_store_lock_once_per_online_check_and_never_for_offline_tokens() {
    let store = Arc::new(TrackingStore::default());
    let f = fixture_with(options(), [], store.clone(), "Test Device", DEVICE_ID);

    // Inside the throttle window the lock is still taken: that is exactly where two
    // racing processes need to see each other's freshly persisted validated_at.
    f.licensing.validate_token_online(&make_token(&validated_ago(30))).unwrap();
    assert_eq!(store.locks.load(Ordering::SeqCst), 1);

    f.licensing.validate_token_online(&make_token(&with(claims(), "method", json!("Offline")))).unwrap();
    assert_eq!(store.locks.load(Ordering::SeqCst), 1);
}

#[test]
fn request_activation_defaults_to_online_and_can_ask_for_offline() {
    let f = fixture([activation_response()]);
    assert_eq!(f.licensing.request_activation(ActivationMethod::Online).unwrap().method, ActivationMethod::Online);
    assert!(f.transport.requests()[0].url.contains("method=Online"));

    let f = fixture([activation_response()]);
    let request = f.licensing.request_activation(ActivationMethod::Offline).unwrap();
    assert_eq!(request.method, ActivationMethod::Offline);

    // The backend mints a token carrying method: Offline once the user finishes.
    f.transport.push(ok(make_token(&with(validated_ago(ONE_YEAR), "method", json!("Offline")))));
    let license = f.licensing.get_requested_activation(&request).unwrap().unwrap();
    assert_eq!(license.method, ActivationMethod::Offline);

    // From here it behaves like any other offline license: validated locally
    // however stale, and never revocable. Neither call reaches the API.
    assert_eq!(f.licensing.validate_token_online(&license.token).unwrap().method, ActivationMethod::Offline);
    assert!(matches!(f.licensing.revoke_activation(&license.token).unwrap_err(), Error::OperationNotSupported(_)));
    assert_eq!(f.transport.request_count(), 2);
}

#[test]
fn polls_inside_the_minimum_interval_are_answered_locally() {
    let f = fixture([HttpResponse::new(204, "")]);
    let request = poll_request();
    for _ in 0..3 {
        assert!(f.licensing.get_requested_activation(&request).unwrap().is_none());
    }
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn polls_again_once_the_minimum_interval_has_passed() {
    let f = fixture([HttpResponse::new(204, ""), ok(make_token(&claims()))]);
    let request = poll_request();
    assert!(f.licensing.get_requested_activation(&request).unwrap().is_none());
    thread::sleep(Duration::from_millis(1050));
    assert_eq!(f.licensing.get_requested_activation(&request).unwrap().unwrap().id, "license-123");
    assert_eq!(f.transport.request_count(), 2);
}

#[test]
fn polling_two_requests_in_turn_lets_both_reach_the_server() {
    let f = fixture([HttpResponse::new(204, ""), HttpResponse::new(204, "")]);
    let first = poll_request();
    let mut second = poll_request();
    second.request_url = "https://demo.moonbase.sh/api/client/activations/request-456".into();

    assert!(f.licensing.get_requested_activation(&first).unwrap().is_none());
    assert!(f.licensing.get_requested_activation(&second).unwrap().is_none());
    assert!(f.licensing.get_requested_activation(&first).unwrap().is_none());
    assert!(f.licensing.get_requested_activation(&second).unwrap().is_none());
    assert_eq!(f.transport.request_count(), 2);

    thread::sleep(Duration::from_millis(1050));
    f.transport.push(HttpResponse::new(204, ""));
    f.transport.push(ok(make_token(&claims())));
    assert!(f.licensing.get_requested_activation(&first).unwrap().is_none());
    assert!(f.licensing.get_requested_activation(&second).unwrap().is_some());
    assert_eq!(f.transport.requests()[3].url, second.request_url);
}

#[test]
fn a_failed_poll_still_counts_against_the_interval() {
    let f = fixture([]);
    let request = poll_request();
    assert!(f.licensing.get_requested_activation(&request).is_err());
    assert!(f.licensing.get_requested_activation(&request).unwrap().is_none());
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn concurrent_polls_inside_the_interval_send_one_request() {
    let f = Arc::new(fixture([HttpResponse::new(204, "")]));
    let request = poll_request();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let (f, request) = (f.clone(), request.clone());
            thread::spawn(move || {
                let _ = f.licensing.get_requested_activation(&request);
            })
        })
        .collect();
    threads.into_iter().for_each(|thread| thread.join().unwrap());
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn the_device_token_is_a_base64_json_descriptor_of_the_device_and_product() {
    let f = fixture([]);
    let token = f.licensing.generate_device_token().unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(token).unwrap()).unwrap();
    assert_eq!(json, json!({"id": "device-id", "name": "Test Device", "productId": "demo-app", "format": "JWT"}));
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn reads_back_an_offline_license_for_this_device_only() {
    let f = fixture([]);
    let license = f.licensing.read_offline_license(&make_token(&with(claims(), "method", json!("Offline")))).unwrap();
    assert_eq!(license.method, ActivationMethod::Offline);
    assert_eq!(license.issued_to.email, "jane@example.com");

    let error = f.licensing.read_offline_license(&make_token(&claims())).unwrap_err();
    assert!(matches!(error, Error::LicenseInvalid(_)));

    let foreign = with(claims_for("other-device"), "method", json!("Offline"));
    assert!(f.licensing.read_offline_license(&make_token(&foreign)).unwrap_err().is_license_invalid());
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn allow_expired_surfaces_an_expired_offline_license_instead_of_failing() {
    let f = fixture([]);
    let token = make_token(&with(with(claims(), "method", json!("Offline")), "exp", json!(now_seconds() - 10)));
    assert!(matches!(f.licensing.validate_token_local(&token).unwrap_err(), Error::LicenseExpired(_)));

    let peek = f.licensing.validate_token_local_allow_expired(&token).unwrap();
    assert_eq!(peek.method, ActivationMethod::Offline);
    assert!(peek.expires_at.unwrap() < SystemTime::now());
}

#[test]
fn revoke_refuses_offline_and_trial_tokens_without_contacting_the_api() {
    let f = fixture([]);
    for claims in [with(claims(), "method", json!("Offline")), with(claims(), "trial", json!(true))] {
        assert!(matches!(
            f.licensing.revoke_activation(&make_token(&claims)).unwrap_err(),
            Error::OperationNotSupported(_)
        ));
    }
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn revoke_calls_the_api_and_clears_the_matching_local_license() {
    let f = fixture([ok("")]);
    let token = make_token(&claims());
    f.store.store_local_license(&f.licensing.validate_token_local(&token).unwrap()).unwrap();

    f.licensing.revoke_activation(&token).unwrap();
    assert!(f.transport.requests()[0].url.contains("/api/client/licenses/demo-app/revoke"));
    assert!(f.store.load_local_license().unwrap().is_none());
}

#[test]
fn revoke_leaves_an_unrelated_stored_license_alone() {
    let f = fixture([ok("")]);
    let stored = make_token(&with(claims(), "id", json!("activation-A")));
    f.store.store_local_license(&f.licensing.validate_token_local(&stored).unwrap()).unwrap();

    f.licensing.revoke_activation(&make_token(&with(claims(), "id", json!("activation-B")))).unwrap();
    assert_eq!(f.store.load_local_license().unwrap().unwrap().activation_id, "activation-A");
}

#[test]
fn revoke_still_posts_when_the_local_token_has_expired() {
    // The seat stays allocated server-side until the revoke reaches the API.
    let f = fixture([ok("")]);
    f.licensing.revoke_activation(&make_token(&with(claims(), "exp", json!(now_seconds() - 60)))).unwrap();
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn revoke_succeeds_even_if_local_cleanup_fails_and_cleans_up_under_the_lock() {
    let store = Arc::new(TrackingStore { fail_io: true, ..TrackingStore::default() });
    let f = fixture_with(options(), [ok("")], store.clone(), "Test Device", DEVICE_ID);
    // The seat is free the moment the API answers 200; a local failure must not
    // read as a failed revoke.
    f.licensing.revoke_activation(&make_token(&claims())).unwrap();
    assert_eq!(store.locks.load(Ordering::SeqCst), 1);
}

#[test]
fn configuration_is_checked_up_front() {
    for (endpoint, product, key) in
        [("", "demo-app", PUBLIC_KEY), ("https://x", "", PUBLIC_KEY), ("https://x", "p", "")]
    {
        let error = Licensing::new(moonbase::LicensingOptions::new(endpoint, product, key)).unwrap_err();
        assert!(matches!(error, Error::Configuration(_)), "{error:?}");
    }
    let error = Licensing::new(moonbase::LicensingOptions::new("https://x", "p", "not a key")).unwrap_err();
    assert!(matches!(error, Error::Configuration(_)));
    // A trailing slash on the endpoint is normalized away.
    let licensing = Licensing::new(moonbase::LicensingOptions::new("https://x//", "p", PUBLIC_KEY)).unwrap();
    assert_eq!(licensing.options().endpoint, "https://x");
}

#[test]
fn licensing_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Licensing>();
}

// -- Lifecycle helpers --------------------------------------------------------------

#[test]
fn load_and_validate_answers_for_what_is_stored() {
    let f = fixture([]);
    assert_eq!(f.licensing.load_and_validate().unwrap(), None);

    // Validated moments ago: no request.
    let token = make_token(&validated_ago(30));
    f.store.store_local_license(&f.licensing.validate_token_local(&token).unwrap()).unwrap();
    assert_eq!(f.licensing.load_and_validate().unwrap().unwrap().token, token);
    assert_eq!(f.transport.request_count(), 0);

    // Due a check: the refreshed copy is returned and stored.
    let due = make_token(&validated_ago(10 * MINUTE));
    f.store.store_local_license(&f.licensing.validate_token_local(&due).unwrap()).unwrap();
    let refreshed = make_token(&validated_ago(0));
    f.transport.push(ok(refreshed.clone()));
    assert_eq!(f.licensing.load_and_validate().unwrap().unwrap().token, refreshed);
    assert_eq!(f.store.load_local_license().unwrap().unwrap().token, refreshed);
}

#[test]
fn load_and_validate_checks_an_expired_license_with_moonbase() {
    // A subscription that renewed, and a trial that was bought, while the app was
    // closed: Moonbase answers for both with the license as it is now.
    for (stored, answer) in [
        (expired(24 * 3600, 3 * 24 * 3600), validated_ago(0)),
        (
            with(expired(24 * 3600, 3 * 24 * 3600), "trial", json!(true)),
            with(validated_ago(0), "id", json!("activation-456")),
        ),
    ] {
        let answer = make_token(&answer);
        let f = fixture([ok(answer.clone())]);
        let stored = f.licensing.validate_token_local_allow_expired(&make_token(&stored)).unwrap();
        f.store.store_local_license(&stored).unwrap();

        assert_eq!(f.licensing.load_and_validate().unwrap().unwrap().token, answer);
        assert_eq!(f.store.load_local_license().unwrap().unwrap().token, answer);
        assert_eq!(f.transport.request_count(), 1);
    }
}

#[test]
fn load_and_validate_leaves_a_refused_license_where_it_is() {
    let f = fixture([HttpResponse::new(400, r#"{"errorType":"LicenseRevoked","detail":"Revoked"}"#)]);
    let due = make_token(&validated_ago(10 * MINUTE));
    f.store.store_local_license(&f.licensing.validate_token_local(&due).unwrap()).unwrap();
    assert!(matches!(f.licensing.load_and_validate(), Err(Error::LicenseInvalid(_))));
    assert!(f.store.load_local_license().unwrap().is_some(), "the app decides");
}

#[test]
fn complete_activation_stores_the_license_once_it_lands() {
    let token = make_token(&claims());
    let f = fixture([HttpResponse::new(204, ""), ok(token.clone())]);

    assert_eq!(f.licensing.complete_activation(&poll_request()).unwrap(), None);
    assert_eq!(f.store.load_local_license().unwrap(), None);

    // The SDK spaces polls of one request a second apart.
    thread::sleep(Duration::from_millis(1100));
    let license = f.licensing.complete_activation(&poll_request()).unwrap().unwrap();
    assert_eq!(license.token, token);
    assert_eq!(f.store.load_local_license().unwrap().unwrap().token, token);
}

#[test]
fn complete_activation_hands_over_a_license_it_was_told_not_to_store() {
    let token = make_token(&claims());
    let f = fixture([ok(token.clone())]);
    let license = f.licensing.complete_activation_with(&poll_request(), || false).unwrap().unwrap();
    assert_eq!(license.token, token);
    assert_eq!(f.store.load_local_license().unwrap(), None);
}

#[test]
fn import_offline_license_stores_a_license_file_for_this_device() {
    let f = fixture([]);
    let offline = make_token(&with(claims(), "method", json!("Offline")));
    let license = f.licensing.import_offline_license(&format!("\n{offline}\n")).unwrap();
    assert_eq!(license.method, ActivationMethod::Offline);
    assert_eq!(f.store.load_local_license().unwrap().unwrap().token, offline);

    // An online token is not a license file, and nothing of it is stored.
    let f = fixture([]);
    assert!(matches!(f.licensing.import_offline_license(&make_token(&claims())), Err(Error::LicenseInvalid(_))));
    assert_eq!(f.store.load_local_license().unwrap(), None);
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn import_offline_license_stores_a_file_that_doesnt_cover_this_version() {
    let f = fixture_options(|o| o.application_version = Some("3.0.0".into()));
    let offline = make_token(&with(with(claims(), "method", json!("Offline")), "l:rel:major", json!(2)));
    assert!(matches!(f.licensing.import_offline_license(&offline), Err(Error::ReleaseNotCovered(_))));
    // It still covers the versions it did.
    assert_eq!(f.store.load_local_license().unwrap().unwrap().token, offline);
}
