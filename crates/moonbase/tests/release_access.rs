//! Licenses that cover only some versions of the app: the shared conformance
//! vectors, reading the `l:rel:*` claims, and how the validator, the client,
//! `Licensing` and the inventory client treat a version a license doesn't cover.
//!
//! `tests/vectors/release-access-vectors.json` is vendored verbatim from
//! `@moonbase.sh/licensing`. Moonbase and every SDK must agree on every case.

mod common;

use std::sync::Arc;

use common::*;
use moonbase::{
    Error, HttpResponse, InventoryClient, License, LicenseValidator, LicensingOptions, MemoryLicenseStore,
    ReleaseAccess, StaticDeviceIdResolver, can_download,
};
use serde_json::{Value, json};

const YEAR: i64 = 365 * 24 * 3600;
const UPGRADE_URL: &str = "https://demo.moonbase.sh/buy/demo-app?variation=v1&license=license-123";

fn vectors() -> Value {
    serde_json::from_str(include_str!("vectors/release-access-vectors.json")).expect("the vector file is JSON")
}

fn vector_time(value: &Value) -> Option<std::time::SystemTime> {
    value.as_str().map(iso)
}

fn validator_for(app_version: Option<&str>, enforce: bool) -> LicenseValidator {
    let mut options = LicensingOptions::new("https://demo.moonbase.sh", "demo-app", PUBLIC_KEY)
        .account_id("tenant-1")
        .enforce_release_access(enforce);
    options.application_version = app_version.map(str::to_owned);
    LicenseValidator::new(&options, Arc::new(StaticDeviceIdResolver::new("device-name", DEVICE_ID))).unwrap()
}

fn validator() -> LicenseValidator {
    validator_for(None, true)
}

/// Up to 2.x, the shape of a "versions up to a major" license.
fn up_to_major_two() -> Value {
    with(claims(), "l:rel:major", json!(2))
}

/// Updates ended a year ago, after 2.3.0 came out.
fn updates_ended() -> Value {
    with(with(claims(), "l:rel:until", json!(now_seconds() - YEAR)), "l:rel:upto", json!("2.3.0"))
}

fn validated_ago(claims: Value, seconds: i64) -> Value {
    with(claims, "validated", json!(now_seconds() - seconds))
}

fn facade_for(app_version: &str) -> Fixture {
    fixture_with(
        options().application_version(app_version),
        [],
        Arc::new(MemoryLicenseStore::new()),
        "Test Device",
        DEVICE_ID,
    )
}

fn facade() -> Fixture {
    facade_for("3.0.0")
}

fn not_covered_problem(fresh_token: Option<&str>) -> HttpResponse {
    let mut problem = json!({
        "title": "Version not covered",
        "status": 400,
        "detail": "The license doesn't cover version 3.0.0",
        "errorType": "ReleaseNotCovered",
        "appVersion": "3.0.0",
        "upgradeUrl": UPGRADE_URL,
        "releaseAccess": {"updatesUntil": "2026-06-01T00:00:00Z", "upToVersion": "2.3.0"},
    });
    if let Some(token) = fresh_token {
        problem["license"] = json!(token);
    }
    HttpResponse::new(400, problem.to_string())
}

fn refusal(error: Error) -> Box<moonbase::ReleaseNotCovered> {
    match error {
        Error::ReleaseNotCovered(refusal) => refusal,
        other => panic!("expected ReleaseNotCovered, got {other:?}"),
    }
}

#[test]
fn covers_version_agrees_with_every_vector() {
    let vectors = vectors();
    assert_eq!(vectors["version"], 1);
    let cases = vectors["cases"].as_array().unwrap();
    assert!(!cases.is_empty());

    for case in cases {
        let access = ReleaseAccess {
            updates_until: vector_time(&case["updatesUntil"]),
            up_to_major: case["upToMajor"].as_u64(),
            up_to_version: case["upToVersion"].as_str().map(str::to_owned),
        };
        let now = vector_time(&case["now"]).unwrap();
        assert_eq!(
            access.covers_at(case["version"].as_str().unwrap(), now),
            case["covered"].as_bool().unwrap(),
            "{}",
            case["description"]
        );
    }
}

#[test]
fn the_token_claims_read_into_the_same_answer() {
    let validator = validator();
    for case in vectors()["cases"].as_array().unwrap() {
        let mut claims = claims();
        if let Some(until) = vector_time(&case["updatesUntil"]) {
            claims["l:rel:until"] = json!(epoch_seconds(until));
        }
        if !case["upToMajor"].is_null() {
            claims["l:rel:major"] = case["upToMajor"].clone();
        }
        if !case["upToVersion"].is_null() {
            claims["l:rel:upto"] = case["upToVersion"].clone();
        }
        let license = validator.validate_token(&make_token(&claims)).unwrap();
        let now = vector_time(&case["now"]).unwrap();
        assert_eq!(
            license.covers_version_at(case["version"].as_str().unwrap(), now),
            case["covered"].as_bool().unwrap(),
            "{}",
            case["description"]
        );
    }
}

#[test]
fn a_license_without_release_access_claims_covers_every_version() {
    let license = validator().validate_token(&make_token(&claims())).unwrap();
    assert_eq!(license.release_access, None);
    assert!(license.covers_version("99.0.0"));
}

#[test]
fn release_access_claims_are_read_from_numbers_or_strings_holding_them() {
    let claims = with(
        with(with(claims(), "l:rel:until", json!("1780272000")), "l:rel:major", json!("2")),
        "l:rel:upto",
        json!("2.3.0"),
    );
    let access = validator().validate_token(&make_token(&claims)).unwrap().release_access.unwrap();
    assert_eq!(access.updates_until, Some(iso("2026-06-01T00:00:00Z")));
    assert_eq!(access.up_to_major, Some(2));
    assert_eq!(access.up_to_version.as_deref(), Some("2.3.0"));
}

#[test]
fn a_release_access_claim_that_cant_be_read_is_left_out_not_a_failed_license() {
    let validator = validator();
    let parse = |claims: &Value| validator.validate_token(&make_token(claims)).unwrap().release_access;

    // Neither limit can be read, so nothing is limited, and upto alone means nothing.
    let mut claims = with(with(claims(), "l:rel:until", json!("soon")), "l:rel:upto", json!("2.3.0"));
    for major in [json!(2.5), json!(-1), json!(true), Value::Null] {
        claims["l:rel:major"] = major;
        assert_eq!(parse(&claims), None, "{}", claims["l:rel:major"]);
    }

    let limited = parse(&with(up_to_major_two(), "l:rel:upto", json!("the latest"))).unwrap();
    assert_eq!(limited.up_to_major, Some(2));
    assert_eq!(limited.up_to_version, None);
}

#[test]
fn release_access_survives_the_stores_json_round_trip() {
    let license = validator().validate_token(&make_token(&updates_ended())).unwrap();
    let access = license.release_access.clone().unwrap();

    let restored: License = serde_json::from_value(serde_json::to_value(&license).unwrap()).unwrap();
    let restored_access = restored.release_access.unwrap();
    assert_eq!(restored_access.updates_until, access.updates_until);
    assert_eq!(restored_access.up_to_major, None);
    assert_eq!(restored_access.up_to_version.as_deref(), Some("2.3.0"));

    let unlimited = serde_json::to_value(validator().validate_token(&make_token(&claims())).unwrap()).unwrap();
    assert!(unlimited.get("releaseAccess").is_none());
    assert_eq!(serde_json::from_value::<License>(unlimited).unwrap().release_access, None);
}

#[test]
fn a_stored_release_access_field_that_cant_be_read_costs_nothing_but_itself() {
    let mut stored =
        serde_json::to_value(validator().validate_token(&make_token(&up_to_major_two())).unwrap()).unwrap();
    stored["releaseAccess"]["updatesUntil"] = json!("+294247-01-10T04:00:54Z");
    stored["releaseAccess"]["upToVersion"] = json!(5);

    let access = serde_json::from_value::<License>(stored).unwrap().release_access.unwrap();
    assert_eq!(access.up_to_major, Some(2));
    assert_eq!(access.updates_until, None);
    assert_eq!(access.up_to_version, None);
}

#[test]
fn an_end_of_updates_past_what_the_clock_holds_is_held_to_its_limit_not_dropped() {
    let validator = validator();
    let parse = |claims: &Value| validator.validate_token(&make_token(claims)).unwrap();

    // Dropping it would leave the major limit alone and refuse 3.0.0, which
    // Moonbase covers while updates last.
    let mut claims = with(up_to_major_two(), "l:rel:until", json!(10_413_792_000_i64)); // 2300-01-01
    let distant = parse(&claims);
    assert!(distant.release_access.as_ref().unwrap().updates_until.is_some());
    assert!(distant.covers_version("3.0.0"));

    claims["l:rel:until"] = json!(i64::MAX);
    assert!(parse(&claims).covers_version("3.0.0"));

    // The other way, the end has long passed.
    claims["l:rel:until"] = json!(-i64::MAX);
    claims["l:rel:upto"] = json!("2.3.0");
    let ancient = parse(&claims);
    assert!(!ancient.covers_version("3.0.0"));
    assert!(ancient.covers_version("2.0.0"));

    // Still a date the store can write and read back.
    let restored: License = serde_json::from_value(serde_json::to_value(&distant).unwrap()).unwrap();
    assert!(restored.release_access.unwrap().updates_until.is_some());
}

#[test]
fn the_validator_refuses_a_version_the_license_doesnt_cover() {
    let token = make_token(&up_to_major_two());

    let error = validator_for(Some("v3.0.0-beta.2"), true).validate_token(&token).unwrap_err();
    assert_eq!(error.error_type(), moonbase::ErrorType::ReleaseNotCovered);
    assert_eq!(error.to_string(), "The license doesn't cover version 3.0.0");
    // So existing checks keep refusing to run, and the grace period can't relax it.
    assert!(error.is_license_invalid() && error.is_license_rejection());
    let refusal = refusal(error);
    assert_eq!(refusal.app_version(), "3.0.0");
    assert_eq!(refusal.license().unwrap().id, "license-123");
    assert_eq!(refusal.release_access().unwrap().up_to_major, Some(2));
    // Only Moonbase knows where to buy the upgrade.
    assert_eq!(refusal.upgrade_url(), None);

    assert!(validator_for(Some("2.9.9"), true).validate_token(&token).is_ok());
    assert!(validator_for(Some("nightly"), true).validate_token(&token).is_ok());
    assert!(validator_for(None, true).validate_token(&token).is_ok());
    assert!(validator_for(Some("3.0.0"), false).validate_token(&token).is_ok());

    let validator = validator_for(Some("3.0.0"), true);
    let ignoring = validator.validate_token_ignoring_release_access(&token).unwrap();
    assert!(validator.validate_token_allow_expired(&token).is_ok());
    assert!(matches!(validator.ensure_covers_application_version(&ignoring), Err(Error::ReleaseNotCovered(_))));
}

#[test]
fn updates_that_last_cover_every_version_and_end_on_the_device_clock() {
    let lasting = with(claims(), "l:rel:until", json!(now_seconds() + 3600));
    assert!(validator_for(Some("9.0.0"), true).validate_token(&make_token(&lasting)).is_ok());

    let ended = make_token(&updates_ended());
    assert!(matches!(validator_for(Some("2.3.1"), true).validate_token(&ended), Err(Error::ReleaseNotCovered(_))));
    assert!(validator_for(Some("2.3.0"), true).validate_token(&ended).is_ok());
}

#[test]
fn validate_token_online_reads_release_not_covered_with_the_license_moonbase_issued_along() {
    let f = facade();
    let stale = make_token(&validated_ago(updates_ended(), 10 * 60)); // past the 5 minute throttle
    let fresh = make_token(&validated_ago(updates_ended(), 0));
    f.transport.push(not_covered_problem(Some(&fresh)));

    let refusal = refusal(f.licensing.validate_token_online(&stale).unwrap_err());
    assert_eq!(refusal.message(), "The license doesn't cover version 3.0.0");
    assert_eq!(refusal.app_version(), "3.0.0");
    assert_eq!(refusal.upgrade_url(), Some(UPGRADE_URL));
    let access = refusal.release_access().unwrap();
    assert_eq!(access.updates_until, Some(iso("2026-06-01T00:00:00Z")));
    assert_eq!(access.up_to_version.as_deref(), Some("2.3.0"));
    assert_eq!(refusal.license().unwrap().token, fresh);

    // Kept, not deleted: it still covers the versions it did.
    assert_eq!(f.store.load_local_license().unwrap().unwrap().token, fresh);
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn validate_token_online_leaves_the_store_alone_on_release_not_covered_when_told_not_to_persist() {
    let f = facade();
    f.transport.push(not_covered_problem(Some(&make_token(&validated_ago(updates_ended(), 0)))));
    let stale = make_token(&validated_ago(updates_ended(), 10 * 60));
    assert!(matches!(f.licensing.validate_token_online_with(&stale, || false), Err(Error::ReleaseNotCovered(_))));
    assert_eq!(f.store.load_local_license().unwrap(), None);
}

#[test]
fn moonbase_judges_an_online_license_so_an_upgrade_bought_since_the_token_was_issued_counts() {
    let f = facade();
    let stale = make_token(&validated_ago(updates_ended(), 10 * 60)); // doesn't cover 3.0.0 on the device
    let upgraded = make_token(&validated_ago(with(claims(), "l:rel:until", json!(now_seconds() + YEAR)), 0));
    f.transport.push(ok(upgraded.clone()));

    assert_eq!(f.licensing.validate_token_online(&stale).unwrap().token, upgraded);
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn the_device_judges_the_version_within_the_min_interval() {
    let f = facade();
    let result = f.licensing.validate_token_online(&make_token(&updates_ended()));
    assert!(matches!(result, Err(Error::ReleaseNotCovered(_))));
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn the_device_judges_the_version_when_moonbase_is_unreachable_within_the_grace_period() {
    let f = facade();
    // No queued response: the transport fails, as an unreachable server does.
    let result = f.licensing.validate_token_online(&make_token(&validated_ago(updates_ended(), 10 * 60)));
    assert!(matches!(result, Err(Error::ReleaseNotCovered(_))));
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn the_device_judges_the_version_of_an_offline_license() {
    let f = facade();
    let offline = with(up_to_major_two(), "method", json!("Offline"));
    let token = make_token(&offline);
    assert!(matches!(f.licensing.validate_token_online(&token), Err(Error::ReleaseNotCovered(_))));
    let refusal = refusal(f.licensing.read_offline_license(&token).unwrap_err());
    // This device's own license, for the versions it covers: safe to store.
    assert_eq!(refusal.license().unwrap().method, moonbase::ActivationMethod::Offline);
    assert_eq!(f.transport.request_count(), 0);

    assert!(facade_for("2.4.0").licensing.read_offline_license(&token).is_ok());

    // An online token passed off as a license file is refused as such, not handed
    // back to be stored because it doesn't cover the version.
    let error = f.licensing.read_offline_license(&make_token(&up_to_major_two())).unwrap_err();
    assert!(
        matches!(&error, Error::LicenseInvalid(message) if message == "License token is not an offline-activated license")
    );
}

#[test]
fn an_activation_moonbase_fulfilled_is_handed_over_whatever_the_device_makes_of_the_version() {
    let f = facade();
    f.transport.push(ok(make_token(&up_to_major_two())));
    // A seat is taken by now, so dropping the license here would strand it.
    let activated = f.licensing.get_requested_activation(&poll_request()).unwrap().unwrap();
    assert!(activated.release_access.is_some());
}

#[test]
fn a_license_that_doesnt_cover_this_version_can_still_be_deactivated() {
    let f = facade();
    f.transport.push(ok(""));
    f.licensing.revoke_activation(&make_token(&up_to_major_two())).unwrap();
    assert!(f.transport.requests()[0].url.contains("/revoke"));
}

#[test]
fn add_ons_are_managed_on_a_license_whatever_version_runs() {
    let f = facade();
    let token = make_token(&up_to_major_two()); // doesn't cover 3.0.0
    f.transport.push(ok(token.clone()));

    // The seat is taken by the time the answer comes, so it is handed over and
    // stored rather than refused for the version.
    assert_eq!(f.licensing.activate_sub_product(&token, "drum-kit").unwrap().token, token);
    assert!(f.transport.requests()[0].url.contains("/sub-products/drum-kit/activate"));
    assert_eq!(f.store.load_local_license().unwrap().unwrap().token, token);
}

#[test]
fn a_release_not_covered_problem_with_a_license_that_fails_verification_leaves_it_out() {
    let f = facade();
    let foreign = make_token_with(Signer::SomeoneElse, &up_to_major_two(), &json!({"alg": "RS256", "typ": "JWT"}));
    f.transport.push(not_covered_problem(Some(&foreign)));

    let refusal =
        refusal(f.licensing.validate_token_online(&make_token(&validated_ago(up_to_major_two(), 600))).unwrap_err());
    assert!(refusal.license().is_none());
    assert!(refusal.upgrade_url().is_some());
    assert_eq!(f.store.load_local_license().unwrap(), None);
}

#[test]
fn a_release_not_covered_problem_whose_extensions_have_the_wrong_shape_still_refuses() {
    let f = facade();
    f.transport.push(HttpResponse::new(
        400,
        r#"{"errorType":"ReleaseNotCovered","releaseAccess":"2.x","upgradeUrl":5,"appVersion":null}"#,
    ));
    let refusal =
        refusal(f.licensing.validate_token_online(&make_token(&validated_ago(up_to_major_two(), 600))).unwrap_err());
    assert_eq!(refusal.message(), "The license doesn't cover version 3.0.0");
    assert_eq!(refusal.app_version(), "3.0.0");
    assert_eq!(refusal.upgrade_url(), None);
    assert!(refusal.release_access().is_none());
}

#[test]
fn a_release_not_covered_date_without_an_offset_is_utc() {
    let f = facade();
    f.transport.push(HttpResponse::new(
        400,
        r#"{"errorType":"ReleaseNotCovered","releaseAccess":{"updatesUntil":"2026-06-01T00:00:00.000"}}"#,
    ));
    let refusal =
        refusal(f.licensing.validate_token_online(&make_token(&validated_ago(up_to_major_two(), 600))).unwrap_err());
    assert_eq!(refusal.release_access().unwrap().updates_until, Some(iso("2026-06-01T00:00:00Z")));
}

fn inventory(responses: impl IntoIterator<Item = HttpResponse>) -> InventoryClient {
    InventoryClient::new(
        LicensingOptions::new("https://demo.moonbase.sh", "demo-app", PUBLIC_KEY),
        RecordingTransport::new(responses),
    )
}

#[test]
fn get_release_says_when_the_customers_licenses_dont_cover_the_release() {
    let client = inventory([
        ok(
            r#"{"id":"demo-app","name":"Solstice","version":"3.0.0","downloadsNeedsOwnership":true,"downloads":[{"name":"Solstice-3.0.0.dmg","platform":"Mac"}],"releaseAccess":{"upToMajor":2,"coversRelease":false,"latestCoveredVersion":"2.9.0","upgradeUrl":"https://demo.moonbase.sh/buy/demo-app?variation=v1&license=l1"}}"#,
        ),
        ok(r#"{"id":"demo-app","name":"Solstice","version":"2.9.0","downloads":[]}"#),
    ]);

    let info = client.get_release("3.0.0", "tok").unwrap();
    assert!(!info.release_covered);
    assert_eq!(info.release_access.as_ref().unwrap().up_to_major, Some(2));
    assert_eq!(info.upgrade_url.as_deref(), Some("https://demo.moonbase.sh/buy/demo-app?variation=v1&license=l1"));

    let owner = validator().validate_token(&make_token(&claims())).unwrap();
    assert!(!can_download(&owner, &info));

    let unlimited = client.get_release("2.9.0", "tok").unwrap();
    assert!(unlimited.release_covered);
    assert_eq!(unlimited.release_access, None);
    assert_eq!(unlimited.upgrade_url, None);
}

#[test]
fn a_download_the_customers_licenses_dont_cover_is_refused_with_the_upgrade_link() {
    let client = inventory([HttpResponse::new(
        403,
        r#"{"title":"Not covered by your license","status":403,"detail":"Your license doesn't cover this version","errorType":"ReleaseNotCovered","releaseAccess":{"upToMajor":2,"coversRelease":false,"upgradeUrl":"https://demo.moonbase.sh/buy/demo-app?variation=v1&license=l1"},"upgradeUrl":"https://demo.moonbase.sh/buy/demo-app?variation=v1&license=l1"}"#,
    )]);

    let refusal = refusal(client.get_download_url("Mac", "tok").unwrap_err());
    assert_eq!(refusal.message(), "Your license doesn't cover this version");
    assert_eq!(refusal.upgrade_url(), Some("https://demo.moonbase.sh/buy/demo-app?variation=v1&license=l1"));
    assert_eq!(refusal.release_access().unwrap().up_to_major, Some(2));
    assert!(refusal.license().is_none());
    assert_eq!(refusal.app_version(), "");
}
