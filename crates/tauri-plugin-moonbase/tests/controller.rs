//! The plugin's state machine, on Tauri's mock runtime with a scripted Moonbase.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use moonbase::{
    ActivationMethod, FileLicenseStore, HttpRequest, HttpResponse, HttpTransport, LicenseStore, Licensing,
    LicensingOptions, StaticDeviceIdResolver,
};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};
use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};
use tauri_plugin_moonbase::{Builder, LicenseState, LockReason, MoonbaseExt, Status, UiConfig};

const PUBLIC_KEY: &str = include_str!("fixtures/signing-public.pem");
const SIGNING_KEY: &[u8] = include_bytes!("fixtures/signing-private.pk8");

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

fn token(claims: &Value) -> String {
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let input = format!("{}.{}", b64(br#"{"alg":"RS256","typ":"JWT"}"#), b64(claims.to_string().as_bytes()));
    let key = RsaKeyPair::from_pkcs8(SIGNING_KEY).unwrap();
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), input.as_bytes(), &mut signature).unwrap();
    format!("{input}.{}", b64(&signature))
}

fn claims() -> Value {
    json!({
        "iss": "tenant-1", "aud": "demo-app", "iat": now() - 60, "exp": now() + 30 * 86_400,
        "id": "activation-123", "l:id": "license-123", "trial": false, "method": "Online",
        "sig": "device-id", "validated": now() - 30,
        "p:id": "demo-app", "p:name": "Demo Product", "p:rel": "1.2.3",
        "u:id": "user-123", "u:name": "Jane Developer", "u:email": "jane@example.com",
    })
}

fn with(mut claims: Value, key: &str, value: Value) -> Value {
    claims[key] = value;
    claims
}

#[derive(Default)]
struct ScriptedMoonbase {
    requests: Mutex<Vec<HttpRequest>>,
    responses: Mutex<VecDeque<HttpResponse>>,
}

impl ScriptedMoonbase {
    fn new(responses: impl IntoIterator<Item = HttpResponse>) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::default(), responses: Mutex::new(responses.into_iter().collect()) })
    }

    fn sent(&self) -> Vec<String> {
        self.requests.lock().unwrap().iter().map(|request| request.url.clone()).collect()
    }
}

impl HttpTransport for ScriptedMoonbase {
    fn send(&self, request: &HttpRequest) -> moonbase::Result<HttpResponse> {
        self.requests.lock().unwrap().push(request.clone());
        self.responses.lock().unwrap().pop_front().ok_or_else(|| moonbase::ApiError::unreachable("offline").into())
    }
}

fn app(dir: &Path, moonbase: Arc<ScriptedMoonbase>) -> tauri::App<MockRuntime> {
    app_running(dir, moonbase, "1.0.0")
}

/// The app at `version`, for licenses that cover only some versions.
fn app_running(dir: &Path, moonbase: Arc<ScriptedMoonbase>, version: &str) -> tauri::App<MockRuntime> {
    mock_builder()
        .plugin(
            Builder::new("https://demo.moonbase.sh", "demo-app", PUBLIC_KEY)
                .account_id("tenant-1")
                .application_version(version)
                .license_file(dir.join("license.mb"))
                .device_id_resolver(Arc::new(StaticDeviceIdResolver::new("Test Device", "device-id")))
                .transport(moonbase)
                .open_browser(false)
                // The state machine, not the window: the mock runtime has no
                // webviews to present it in.
                .ui(UiConfig::disabled())
                .build(),
        )
        .build(mock_context(noop_assets()))
        .expect("the plugin starts")
}

/// Put a license in the file, as an earlier run would have, whatever device it
/// is bound to.
fn store(dir: &Path, token: &str) {
    let payload = token.split('.').nth(1).unwrap();
    let claims: Value =
        serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
    let licensing = Licensing::builder(LicensingOptions::new("https://demo.moonbase.sh", "demo-app", PUBLIC_KEY))
        .device_id_resolver(Arc::new(StaticDeviceIdResolver::new("x", claims["sig"].as_str().unwrap())))
        .transport(ScriptedMoonbase::new([]))
        .build()
        .unwrap();
    let license = licensing.validate_token_local_allow_expired(token).unwrap();
    FileLicenseStore::new(dir.join("license.mb")).store_local_license(&license).unwrap();
}

fn wait_for(app: &tauri::App<MockRuntime>, what: &str, done: impl Fn(&LicenseState) -> bool) -> LicenseState {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = app.moonbase().state();
        if done(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}; last state: {state:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn settled(app: &tauri::App<MockRuntime>) -> LicenseState {
    wait_for(app, "startup", |state| state.status != Status::Loading)
}

#[test]
fn starts_unlicensed_without_a_stored_license() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), ScriptedMoonbase::new([]));
    let state = settled(&app);
    assert_eq!(state.status, Status::Unlicensed);
    assert_eq!(state.lock_reason, None);
    assert!(!state.licensed && !app.moonbase().is_licensed());
}

#[test]
fn a_stored_license_loads_at_startup_without_its_token_reaching_the_frontend() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&claims()));
    let moonbase = ScriptedMoonbase::new([]);
    let app = app(dir.path(), moonbase.clone());

    let state = settled(&app);
    assert_eq!(state.status, Status::Licensed);
    assert!(app.moonbase().is_licensed());
    let license = state.license.unwrap();
    assert_eq!(license.issued_to.email, "jane@example.com");
    assert_eq!(license.days_remaining, Some(30));
    // What the frontend gets carries no token anywhere.
    let sent = serde_json::to_string(&app.moonbase().state()).unwrap();
    assert!(!sent.contains("\"token\"") && !sent.contains(&stored_token(dir.path())));
    // Validated 30 seconds ago, so no online check was due.
    assert!(moonbase.sent().is_empty());
    // The license says 1.2.3 is out, and this is 1.0.0.
    assert_eq!(state.update.unwrap().latest_version, "1.2.3");
}

#[test]
fn the_frontends_license_is_the_sdks_without_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let claims = with(
        with(up_to_major(2), "sp", json!([{"id": "pack", "active": true, "trial": false}])),
        "s:id",
        json!("subscription-1"),
    );
    store(dir.path(), &token(&claims));
    let app = app(dir.path(), ScriptedMoonbase::new([]));
    let view = serde_json::to_value(settled(&app).license.unwrap()).unwrap();

    // The same JSON the SDK writes to the license file, so a UI can read either,
    // minus the token, plus the countdown.
    let mut expected = serde_json::to_value(app.moonbase().license().unwrap()).unwrap();
    expected.as_object_mut().unwrap().remove("token");
    expected["daysRemaining"] = json!(30);
    assert_eq!(view, expected);
}

#[test]
fn a_browser_activation_runs_through_to_a_stored_license() {
    let dir = tempfile::tempdir().unwrap();
    let moonbase = ScriptedMoonbase::new([
        HttpResponse::new(
            200,
            r#"{"id":"r1","request":"https://demo.moonbase.sh/api/client/activations/r1","browser":"https://demo.moonbase.sh/activate?r1"}"#,
        ),
        HttpResponse::new(200, token(&claims())),
    ]);
    let app = app(dir.path(), moonbase.clone());
    settled(&app);

    let state = app.moonbase().activate(ActivationMethod::Online).unwrap();
    assert_eq!(state.status, Status::Activating);
    assert_eq!(state.activation.unwrap().browser_url.as_deref(), Some("https://demo.moonbase.sh/activate?r1"));

    let state = wait_for(&app, "the activation to land", |state| state.status == Status::Licensed);
    assert!(state.activation.is_none());
    assert!(app.moonbase().is_licensed());
    assert_eq!(
        FileLicenseStore::new(dir.path().join("license.mb")).load_local_license().unwrap().unwrap().id,
        "license-123"
    );
    assert!(moonbase.sent()[0].contains("/api/client/activations/demo-app/request?"));
}

#[test]
fn a_failed_activation_start_explains_itself_to_the_customer() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), ScriptedMoonbase::new([]));
    settled(&app);

    assert!(app.moonbase().activate(ActivationMethod::Online).is_err());
    let state = app.moonbase().state();
    assert_eq!(state.status, Status::Unlicensed);
    let error = state.error.unwrap();
    assert!(error.message.starts_with("Couldn't reach Moonbase to start activation"));
    assert!(app.moonbase().dismiss_error().error.is_none());
}

#[test]
fn cancelling_an_activation_stops_the_poll() {
    let dir = tempfile::tempdir().unwrap();
    let moonbase = ScriptedMoonbase::new([HttpResponse::new(
        200,
        r#"{"id":"r1","request":"https://demo.moonbase.sh/api/client/activations/r1","browser":"https://demo.moonbase.sh/activate?r1"}"#,
    )]);
    let app = app(dir.path(), moonbase.clone());
    settled(&app);

    app.moonbase().activate(ActivationMethod::Online).unwrap();
    assert_eq!(app.moonbase().cancel_activation().status, Status::Unlicensed);
    std::thread::sleep(Duration::from_millis(2500));
    // Only the request went out; the cancelled poll never did.
    assert_eq!(moonbase.sent().len(), 1);
}

#[test]
fn deactivating_frees_the_seat_and_says_why_the_app_locked() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&claims()));
    let moonbase = ScriptedMoonbase::new([HttpResponse::new(200, "")]);
    let app = app(dir.path(), moonbase.clone());
    settled(&app);

    let state = app.moonbase().deactivate().unwrap();
    assert_eq!(state.status, Status::Unlicensed);
    assert_eq!(state.lock_reason, Some(LockReason::Deactivated));
    assert!(moonbase.sent()[0].contains("/api/client/licenses/demo-app/revoke"));
    assert!(!dir.path().join("license.mb").exists());
}

#[test]
fn deactivating_while_offline_keeps_the_license() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&claims()));
    let app = app(dir.path(), ScriptedMoonbase::new([]));
    settled(&app);

    assert!(app.moonbase().deactivate().is_err());
    let state = app.moonbase().state();
    assert_eq!(state.status, Status::Licensed);
    assert!(!state.busy);
    assert!(state.error.unwrap().message.starts_with("Couldn't reach Moonbase to deactivate"));
    assert!(dir.path().join("license.mb").exists());
}

#[test]
fn a_trial_is_forgotten_locally_rather_than_revoked() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&with(claims(), "trial", json!(true))));
    let moonbase = ScriptedMoonbase::new([]);
    let app = app(dir.path(), moonbase.clone());
    assert_eq!(settled(&app).status, Status::Trial);

    let state = app.moonbase().deactivate().unwrap();
    assert_eq!(state.lock_reason, Some(LockReason::Deactivated));
    assert!(moonbase.sent().is_empty());
    assert!(!dir.path().join("license.mb").exists());
}

#[test]
fn an_ended_trial_shows_as_expired_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&with(with(claims(), "trial", json!(true)), "exp", json!(now() - 60))));
    let app = app(dir.path(), ScriptedMoonbase::new([]));

    let state = settled(&app);
    assert_eq!(state.status, Status::TrialExpired);
    assert_eq!(state.lock_reason, Some(LockReason::Expired));
    assert_eq!(state.expired_trial.unwrap().days_remaining, Some(0));
    assert!(!state.licensed);
}

#[test]
fn a_license_bound_to_another_device_locks_as_invalid() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&with(claims(), "sig", json!("other-device"))));
    let app = app(dir.path(), ScriptedMoonbase::new([]));

    let state = settled(&app);
    assert_eq!(state.status, Status::Unlicensed);
    assert_eq!(state.lock_reason, Some(LockReason::Invalid));
}

#[test]
fn offline_activation_reads_a_license_file_for_this_device() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), ScriptedMoonbase::new([]));
    settled(&app);

    let error = app.moonbase().activate_offline(&token(&claims())).unwrap_err();
    assert!(error.to_string().contains("not an offline-activated license"));
    assert_eq!(app.moonbase().state().error.unwrap().message, "That license file isn't valid for this device.");

    let offline = token(&with(claims(), "method", json!("Offline")));
    let state = app.moonbase().activate_offline(&format!("\n{offline}\n")).unwrap();
    assert_eq!(state.status, Status::Licensed);
    assert!(state.error.is_none());
    assert!(dir.path().join("license.mb").exists());
    assert!(!app.moonbase().device_token().unwrap().is_empty());
}

#[test]
fn a_license_removed_by_another_instance_locks_this_one() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&claims()));
    let app = app(dir.path(), ScriptedMoonbase::new([]));
    assert_eq!(settled(&app).status, Status::Licensed);

    std::fs::remove_file(dir.path().join("license.mb")).unwrap();
    let state = wait_for(&app, "the watch to notice", |state| state.status == Status::Unlicensed);
    assert_eq!(state.lock_reason, Some(LockReason::Deactivated));
}

#[test]
fn a_license_activated_by_another_instance_is_picked_up() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), ScriptedMoonbase::new([]));
    assert_eq!(settled(&app).status, Status::Unlicensed);

    store(dir.path(), &token(&claims()));
    wait_for(&app, "the watch to adopt it", |state| state.status == Status::Licensed);
}

#[test]
fn a_stray_cancel_during_startup_does_not_drop_the_license() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&claims()));
    let app = app(dir.path(), ScriptedMoonbase::new([]));
    // Nothing is activating, so this must not supersede the startup check.
    app.moonbase().cancel_activation();
    assert_eq!(settled(&app).status, Status::Licensed);
}

#[test]
fn deactivating_waits_for_an_activation_to_finish_or_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&claims()));
    let moonbase = ScriptedMoonbase::new([HttpResponse::new(
        200,
        r#"{"id":"r1","request":"https://demo.moonbase.sh/api/client/activations/r1","browser":"https://demo.moonbase.sh/activate?r1"}"#,
    )]);
    let app = app(dir.path(), moonbase.clone());
    assert_eq!(settled(&app).status, Status::Licensed);

    // Converting to an offline license: revoking now would strand the activation.
    app.moonbase().activate(ActivationMethod::Offline).unwrap();
    assert!(matches!(app.moonbase().deactivate(), Err(tauri_plugin_moonbase::Error::Busy(_))));
    let state = app.moonbase().state();
    assert_eq!(state.status, Status::Activating);
    assert!(state.licensed);
    assert_eq!(moonbase.sent().len(), 1, "no revoke went out");
}

#[test]
fn an_unusable_lock_file_reads_like_being_offline() {
    let dir = tempfile::tempdir().unwrap();
    // Due an online check, but the lock that guards it cannot be opened.
    store(dir.path(), &token(&with(claims(), "validated", json!(now() - 3600))));
    std::fs::create_dir(dir.path().join("license.mb.lock")).unwrap();
    let app = app(dir.path(), ScriptedMoonbase::new([]));

    let state = settled(&app);
    assert_eq!(state.status, Status::Licensed);
    assert_eq!(state.lock_reason, None);
}

// -- Licenses that cover only some versions -----------------------------------------

const UPGRADE_URL: &str = "https://demo.moonbase.sh/buy/demo-app?variation=v1&license=license-123";

/// A license that covers every version up to a major.
fn up_to_major(major: u64) -> Value {
    with(claims(), "l:rel:major", json!(major))
}

fn stored_token(dir: &Path) -> String {
    FileLicenseStore::new(dir.join("license.mb")).load_local_license().unwrap().unwrap().token
}

#[test]
fn a_stored_offline_license_that_doesnt_cover_this_version_locks_but_stays() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&with(up_to_major(2), "method", json!("Offline"))));
    let moonbase = ScriptedMoonbase::new([]);
    let app = app_running(dir.path(), moonbase.clone(), "3.0.0");

    let state = settled(&app);
    assert_eq!(state.status, Status::NotCovered);
    assert_eq!(state.lock_reason, Some(LockReason::NotCovered));
    assert!(!state.licensed && !app.moonbase().is_licensed());
    let not_covered = state.not_covered.unwrap();
    assert_eq!(not_covered.app_version, "3.0.0");
    assert_eq!(not_covered.release_access.unwrap().up_to_major, Some(2));
    // Only Moonbase knows where to buy the upgrade.
    assert_eq!(not_covered.upgrade_url, None);
    assert_eq!(not_covered.license.id, "license-123");
    // Kept: it still covers the versions it did.
    assert!(dir.path().join("license.mb").exists());
    assert!(moonbase.sent().is_empty());
}

#[test]
fn moonbase_refusing_the_version_locks_with_the_upgrade_link_and_keeps_the_fresh_copy() {
    let dir = tempfile::tempdir().unwrap();
    // Due an online check.
    store(dir.path(), &token(&with(up_to_major(2), "validated", json!(now() - 3600))));
    let fresh = token(&up_to_major(2));
    let problem = json!({
        "title": "Version not covered", "status": 400, "detail": "The license doesn't cover version 3.0.0",
        "errorType": "ReleaseNotCovered", "appVersion": "3.0.0", "upgradeUrl": UPGRADE_URL,
        "releaseAccess": {"upToMajor": 2}, "license": fresh,
    });
    let app = app_running(dir.path(), ScriptedMoonbase::new([HttpResponse::new(400, problem.to_string())]), "3.0.0");

    let state = settled(&app);
    assert_eq!(state.status, Status::NotCovered);
    assert_eq!(state.not_covered.unwrap().upgrade_url.as_deref(), Some(UPGRADE_URL));
    assert_eq!(stored_token(dir.path()), fresh);
}

#[test]
fn checking_again_unlocks_once_the_upgrade_has_gone_through() {
    let dir = tempfile::tempdir().unwrap();
    // Validated moments ago, so judged on this device.
    store(dir.path(), &token(&up_to_major(2)));
    let upgraded = token(&up_to_major(3));
    let moonbase = ScriptedMoonbase::new([HttpResponse::new(200, upgraded.clone())]);
    let app = app_running(dir.path(), moonbase.clone(), "3.0.0");
    assert_eq!(settled(&app).status, Status::NotCovered);
    assert!(moonbase.sent().is_empty());

    let state = app.moonbase().refresh(true).unwrap();
    assert_eq!(state.status, Status::Licensed);
    assert!(state.not_covered.is_none() && state.lock_reason.is_none() && !state.refreshing);
    assert!(app.moonbase().is_licensed());
    assert!(moonbase.sent()[0].contains("/api/client/licenses/demo-app/validate"));
    assert_eq!(stored_token(dir.path()), upgraded);
}

#[test]
fn checking_again_without_an_answer_says_so_and_stays_locked() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&up_to_major(2)));
    let app = app_running(dir.path(), ScriptedMoonbase::new([]), "3.0.0");
    assert_eq!(settled(&app).status, Status::NotCovered);

    assert!(app.moonbase().refresh(true).is_err());
    let state = app.moonbase().state();
    assert_eq!(state.status, Status::NotCovered);
    assert!(!state.refreshing);
    assert!(state.error.unwrap().message.starts_with("Couldn't reach Moonbase to check your license"));
    assert!(dir.path().join("license.mb").exists());
}

#[test]
fn an_unusable_lock_file_still_judges_the_version() {
    let dir = tempfile::tempdir().unwrap();
    // Due an online check, which can't be made because the lock that guards it
    // cannot be opened, so the license on disk is judged on this device.
    store(dir.path(), &token(&with(up_to_major(2), "validated", json!(now() - 3600))));
    std::fs::create_dir(dir.path().join("license.mb.lock")).unwrap();
    let moonbase = ScriptedMoonbase::new([]);
    let app = app_running(dir.path(), moonbase.clone(), "3.0.0");

    let state = settled(&app);
    assert_eq!(state.status, Status::NotCovered);
    assert!(!state.licensed && !app.moonbase().is_licensed());
    assert_eq!(state.not_covered.unwrap().app_version, "3.0.0");
    assert!(moonbase.sent().is_empty());
    assert!(dir.path().join("license.mb").exists());

    // A version it does cover still opens, as before.
    let covered = tempfile::tempdir().unwrap();
    store(covered.path(), &token(&with(up_to_major(2), "validated", json!(now() - 3600))));
    std::fs::create_dir(covered.path().join("license.mb.lock")).unwrap();
    let app = app_running(covered.path(), ScriptedMoonbase::new([]), "2.4.0");
    assert_eq!(settled(&app).status, Status::Licensed);
}

#[test]
fn a_license_that_doesnt_cover_this_version_can_still_be_deactivated() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&up_to_major(2)));
    let moonbase = ScriptedMoonbase::new([HttpResponse::new(200, "")]);
    let app = app_running(dir.path(), moonbase.clone(), "3.0.0");
    assert_eq!(settled(&app).status, Status::NotCovered);

    let state = app.moonbase().deactivate().unwrap();
    assert_eq!(state.status, Status::Unlicensed);
    assert_eq!(state.lock_reason, Some(LockReason::Deactivated));
    assert!(state.not_covered.is_none());
    assert!(moonbase.sent()[0].contains("/revoke"));
    assert!(!dir.path().join("license.mb").exists());
}

#[test]
fn an_offline_license_file_that_doesnt_cover_this_version_is_kept_but_locks() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_running(dir.path(), ScriptedMoonbase::new([]), "3.0.0");
    settled(&app);

    let error = app.moonbase().activate_offline(&token(&with(up_to_major(2), "method", json!("Offline")))).unwrap_err();
    assert!(matches!(error, tauri_plugin_moonbase::Error::Moonbase(moonbase::Error::ReleaseNotCovered(_))));
    let state = app.moonbase().state();
    assert_eq!(state.status, Status::NotCovered);
    assert!(state.error.is_none(), "the upgrade screen explains it");
    assert!(dir.path().join("license.mb").exists());
}

#[test]
fn a_new_version_the_license_doesnt_cover_offers_the_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&claims())); // 1.2.3 is out; this is 1.0.0
    let release = json!({
        "version": "1.2.3", "releaseDescription": "- Faster", "downloadsNeedsOwnership": true,
        "downloads": [{"name": "Demo-1.2.3.dmg", "platform": "Mac"}],
        "releaseAccess": {"upToMajor": 0, "coversRelease": false, "upgradeUrl": UPGRADE_URL},
    });
    let app = app(dir.path(), ScriptedMoonbase::new([HttpResponse::new(200, release.to_string())]));
    settled(&app);

    let info = app.moonbase().check_update().unwrap();
    assert!(info.available && !info.covered && !info.can_download);
    assert_eq!(info.upgrade_url.as_deref(), Some(UPGRADE_URL));
    assert_eq!(info.release_access.unwrap().up_to_major, Some(0));
}

// -- Add-ons ------------------------------------------------------------------------

fn catalog(entries: Value) -> HttpResponse {
    HttpResponse::new(200, json!({"currency": "EUR", "subProducts": entries}).to_string())
}

fn strings_on_trial_offer() -> Value {
    json!({
        "id": "strings",
        "product": {
            "id": "demo-app.strings", "name": "Strings", "owned": false, "subscribed": false,
            "defaultVariation": {"id": "v", "name": "Strings", "originalPrice": {"EUR": 39}, "price": {"EUR": 39}, "hasDiscount": false},
        },
        "includedInLicense": true, "active": false,
        "trial": {"status": "Available", "numberOfDays": 7},
        "buyUrl": "https://demo.moonbase.sh/buy/demo-app.strings",
    })
}

fn pack_owned_with_a_free_seat() -> Value {
    json!({
        "id": "pack",
        "product": {"id": "demo-app.pack", "name": "Pack", "owned": true, "subscribed": false},
        "includedInLicense": true, "active": false,
        "seats": {"total": 2, "used": 1, "remaining": 1},
    })
}

#[test]
fn add_ons_are_listed_and_a_trial_unlocks_one() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&claims()));
    let trial = token(&with(
        claims(),
        "sp",
        json!([{"id": "strings", "active": true, "trial": true, "exp": now() + 7 * 86_400}]),
    ));
    let moonbase = ScriptedMoonbase::new([
        catalog(json!([strings_on_trial_offer()])),
        HttpResponse::new(200, trial.clone()),
        catalog(json!([strings_on_trial_offer()])),
    ]);
    let app = app(dir.path(), moonbase.clone());
    assert!(settled(&app).active_sub_products.is_empty());

    let state = app.moonbase().load_add_ons().unwrap();
    assert_eq!(state.add_ons.phase, tauri_plugin_moonbase::AddOnsPhase::Ready);
    assert_eq!(state.add_ons.currency.as_deref(), Some("EUR"));
    assert_eq!(state.add_ons.add_ons[0].product.name, "Strings");

    let state = app.moonbase().request_sub_product_trial("strings").unwrap();
    assert!(app.moonbase().has_sub_product("strings"));
    assert_eq!(state.active_sub_products, ["strings"]);
    assert_eq!(state.add_ons.busy_id, None);
    assert_eq!(stored_token(dir.path()), trial);
    let sent = moonbase.sent();
    assert_eq!(sent.len(), 3, "listed, tried, and listed again: {sent:?}");
    assert!(sent[1].contains("/api/client/licenses/demo-app/sub-products/strings/trial?"));
}

#[test]
fn no_seat_of_an_add_on_says_so_and_keeps_the_license() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&claims()));
    let no_seat = json!({"title": "Refused", "detail": "No license with available activations found", "errorType": "NoEligibleLicense"});
    let app = app(
        dir.path(),
        ScriptedMoonbase::new([
            catalog(json!([strings_on_trial_offer()])),
            HttpResponse::new(400, no_seat.to_string()),
        ]),
    );
    settled(&app);
    app.moonbase().load_add_ons().unwrap();

    let error = app.moonbase().activate_sub_product("strings").unwrap_err();
    assert!(matches!(error, tauri_plugin_moonbase::Error::Moonbase(moonbase::Error::NoEligibleLicense(_))));
    let state = app.moonbase().state();
    assert_eq!(state.status, Status::Licensed);
    assert_eq!(state.add_ons.busy_id, None);
    assert!(state.add_ons.action_error.unwrap().starts_with("No seat of Strings is free for this computer"));
}

#[test]
fn listing_takes_a_seat_bought_since_the_license_was_checked() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&claims()));
    let seated = token(&with(claims(), "sp", json!([{"id": "pack", "active": true, "trial": false}])));
    let moonbase = ScriptedMoonbase::new([
        catalog(json!([pack_owned_with_a_free_seat()])),
        HttpResponse::new(200, seated),
        catalog(json!([pack_owned_with_a_free_seat()])),
    ]);
    let app = app(dir.path(), moonbase.clone());
    settled(&app);

    app.moonbase().load_add_ons().unwrap();
    assert!(app.moonbase().has_sub_product("demo-app.pack"));
    let sent = moonbase.sent();
    assert_eq!(sent.len(), 3, "listed, checked once, and listed again: {sent:?}");
    assert!(sent[1].contains("/validate"));
}

#[test]
fn add_ons_are_activated_from_a_full_license_only() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path(), &token(&with(claims(), "trial", json!(true))));
    let moonbase = ScriptedMoonbase::new([]);
    let app = app(dir.path(), moonbase.clone());
    settled(&app);

    assert!(app.moonbase().activate_sub_product("pack").is_err());
    let state = app.moonbase().state();
    assert_eq!(state.add_ons.action_error.as_deref(), Some("Add-ons are activated from a full license, not a trial."));
    assert!(moonbase.sent().is_empty());
}
