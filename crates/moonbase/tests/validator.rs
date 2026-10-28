//! Local token validation.

mod common;

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use common::*;
use moonbase::{
    ActivationMethod, DeviceIdResolver, Error, ErrorType, LicenseValidator, LicensingOptions,
    MigratingDeviceIdResolver, StaticDeviceIdResolver,
};
use serde_json::json;

fn validator_for(public_key: &str, device_id: &str) -> LicenseValidator {
    let options = LicensingOptions::new("https://demo.moonbase.sh", "demo-app", public_key).account_id("tenant-1");
    LicenseValidator::new(&options, Arc::new(StaticDeviceIdResolver::new("Test Device", device_id))).unwrap()
}

fn validator() -> LicenseValidator {
    validator_for(PUBLIC_KEY, DEVICE_ID)
}

#[test]
fn a_valid_token_is_parsed_into_a_license() {
    let claims = claims();
    let token = make_token(&claims);
    let license = validator().validate_token(&token).unwrap();

    assert_eq!(license.id, "license-123");
    assert_eq!(license.activation_id, "activation-123");
    assert!(!license.trial);
    assert_eq!(license.method, ActivationMethod::Online);
    assert_eq!(license.licensed_product.id, "demo-app");
    assert_eq!(license.licensed_product.name, "Demo Product");
    assert_eq!(license.licensed_product.current_release_version.as_deref(), Some("1.2.3"));
    assert_eq!(license.licensed_product.properties["tier"], "pro");
    assert_eq!(license.issued_to.email, "jane@example.com");
    assert_eq!(license.issued_to.properties["company"], "Acme");
    assert_eq!(license.owned_sub_product_ids, ["demo-app-pro", "demo-app-extra"]);
    assert!(license.owns_sub_product("demo-app-pro"));
    assert_eq!(license.subscription_id.as_deref(), Some("subscription-123"));
    assert_eq!(license.properties["seats"], 3);
    // No p:seats claims, so the license properties' "seats" stands in.
    assert_eq!(license.seat_count, Some(3));
    assert_eq!(license.seats_used, None);
    let epoch = |key: &str| UNIX_EPOCH + Duration::from_secs(claims[key].as_u64().unwrap());
    assert_eq!(license.issued_at, epoch("iat"));
    assert_eq!(license.expires_at, Some(epoch("exp")));
    assert_eq!(license.validated_at, epoch("validated"));
    assert_eq!(license.token, token);
}

#[test]
fn seat_claims_are_read_from_the_tokens_own_claims_first() {
    let claims = with(with(claims(), "p:seats:total", json!(5)), "p:seats:used", json!(2));
    let license = validator().validate_token(&make_token(&claims)).unwrap();
    assert_eq!((license.seat_count, license.seats_used), (Some(5), Some(2)));
}

#[test]
fn surrounding_whitespace_is_ignored() {
    let token = make_token(&claims());
    let license = validator().validate_token(&format!("\n  {token} \r\n")).unwrap();
    assert_eq!(license.token, token);
}

#[test]
fn a_public_key_is_accepted_in_every_common_text_shape() {
    let pem = PUBLIC_KEY.replace("\r\n", "\n");
    let lines: Vec<&str> = pem.lines().filter(|line| !line.is_empty()).collect();
    let body: String = lines.iter().filter(|line| !line.contains("-----")).copied().collect();
    let shapes = [
        ("multi-line PEM", pem.clone()),
        ("CRLF PEM", lines.iter().map(|line| format!("{line}\r\n")).collect()),
        ("indented PEM", lines.iter().map(|line| format!("    {line}\n")).collect()),
        ("single-line PEM, spaces", lines.join(" ")),
        ("single-line PEM, no separators", lines.concat()),
        ("base64 DER", body),
        ("PKCS#1 PEM", PUBLIC_KEY_PKCS1.to_owned()),
    ];
    let token = make_token(&claims());
    for (shape, key) in shapes {
        let license = validator_for(&key, DEVICE_ID).validate_token(&token);
        assert!(license.is_ok(), "{shape}: {license:?}");
    }
}

#[test]
fn the_validation_time_can_fall_back_to_the_legacy_ver_claim() {
    let claims = with(without(claims(), "validated"), "ver", json!("2026-01-02T03:04:05.678Z"));
    let license = validator().validate_token(&make_token(&claims)).unwrap();
    assert_eq!(license.validated_at, UNIX_EPOCH + Duration::from_secs(1_767_323_045));
}

#[test]
fn trial_tokens_use_trial_properties_and_accept_a_string_flag() {
    let claims = with(
        with(without(claims(), "l:properties"), "trial", json!("true")),
        "t:properties",
        json!({"days": 14, "allowed_hosts": ["localhost", "demo.example.com"]}),
    );
    let license = validator().validate_token(&make_token(&claims)).unwrap();
    assert!(license.trial);
    assert_eq!(license.properties["days"], 14);
    assert_eq!(license.properties["allowed_hosts"][1], "demo.example.com");
}

#[test]
fn custom_properties_carry_array_values() {
    let claims = with(
        with(
            with(claims(), "p:properties", json!({"tier": "pro", "regions": ["us", "eu", "ap"]})),
            "u:properties",
            json!({"company": "Acme", "roles": ["admin", "ops"]}),
        ),
        "l:properties",
        json!({
            "seats": 3,
            "features": ["export", "sso"],
            "contacts": [{"name": "Jane", "email": "jane@example.com"}, {"name": "John", "email": "john@example.com"}],
        }),
    );
    let license = validator().validate_token(&make_token(&claims)).unwrap();
    assert_eq!(license.licensed_product.properties["regions"][2], "ap");
    assert_eq!(license.issued_to.properties["roles"][0], "admin");
    assert_eq!(license.properties["features"].as_array().unwrap().len(), 2);
    assert_eq!(license.properties["contacts"][1]["email"], "john@example.com");
}

#[test]
fn an_endless_trial_has_no_expiry() {
    let license =
        validator().validate_token(&make_token(&with(without(claims(), "exp"), "trial", json!(true)))).unwrap();
    assert!(license.expires_at.is_none());
    assert!(!license.is_expired());
}

#[test]
fn invalid_tokens_are_rejected() {
    let rs256 = json!({"alg": "RS256", "typ": "JWT"});
    let signed = make_token(&claims());
    let signature_at = signed.rfind('.').unwrap();
    let cases: Vec<(&str, String)> = vec![
        ("malformed", "not-a-token".into()),
        ("four parts", format!("{signed}.extra")),
        ("unsupported algorithm", make_token_with(Signer::Moonbase, &claims(), &json!({"alg": "HS256"}))),
        ("algorithm none", make_token_with(Signer::Moonbase, &claims(), &json!({"alg": "none"}))),
        ("wrong signature", make_token_with(Signer::SomeoneElse, &claims(), &rs256)),
        ("wrong audience", make_token(&with(claims(), "aud", json!("other-product")))),
        ("wrong issuer", make_token(&with(claims(), "iss", json!("other-tenant")))),
        ("unknown activation method", make_token(&with(claims(), "method", json!("Sideways")))),
        (
            "malformed legacy validation timestamp",
            make_token(&with(without(claims(), "validated"), "ver", json!("2026-02-30T12:34:56Z"))),
        ),
        ("malformed signature encoding", format!("{}not@base64", &signed[..=signature_at])),
        ("missing claim", make_token(&without(claims(), "l:id"))),
        ("non-integer iat", make_token(&with(claims(), "iat", json!(1.5)))),
        ("properties not an object", make_token(&with(claims(), "p:properties", json!([1])))),
        ("tampered payload", {
            let parts: Vec<&str> = signed.split('.').collect();
            format!("{}.{}.{}", parts[0], b64url(with(claims(), "trial", json!(true)).to_string().as_bytes()), parts[2])
        }),
    ];
    for (case, token) in cases {
        let error = validator().validate_token(&token).unwrap_err();
        assert!(matches!(error, Error::LicenseInvalid(_)), "{case}: {error:?}");
    }

    let audience_list = with(claims(), "aud", json!(["other", "demo-app"]));
    assert!(validator().validate_token(&make_token(&audience_list)).is_ok());
}

#[test]
fn an_expired_token_is_expired_unless_explicitly_allowed() {
    let token = make_token(&with(claims(), "exp", json!(now_seconds() - 10)));
    assert!(matches!(validator().validate_token(&token).unwrap_err(), Error::LicenseExpired(_)));
    assert!(validator().validate_token_allow_expired(&token).unwrap().is_expired());
}

#[test]
fn a_token_for_another_device_is_a_device_mismatch_and_still_invalid() {
    let error = validator().validate_token(&make_token(&claims_for("other-device"))).unwrap_err();
    assert_eq!(error.error_type(), ErrorType::LicenseDeviceMismatch);
    assert!(error.is_license_invalid());
    assert!(error.is_license_rejection());
    // Custom resolver ids are compared literally: no fingerprint-version talk.
    assert_eq!(error.to_string(), "This license is not for this device");
}

fn mismatch(expected: &str, bound: &str) -> String {
    validator_for(PUBLIC_KEY, expected).validate_token(&make_token(&claims_for(bound))).unwrap_err().to_string()
}

#[test]
fn a_stamp_difference_is_explained_without_claiming_machine_continuity() {
    let v2 = format!("mbd2_{}", "a".repeat(64));
    let v1 = format!("mbd1_{}", "b".repeat(64));
    let v7 = format!("mbd7_{}", "b".repeat(64));

    assert_eq!(mismatch(&v2, &format!("mbd2_{}", "e".repeat(64))), "This license is not for this device");

    for bound in
        [format!("mbd2_{}", "z".repeat(64)), format!("mbd2_{}", "A".repeat(64)), format!("mbd2_{}", "a".repeat(63))]
    {
        assert!(mismatch(&v2, &bound).contains("predating versioned device fingerprints"), "{bound}");
    }

    let scoped = format!("mbd2s_{}", "c".repeat(64));
    let bound_is_scoped = mismatch(&v2, &scoped);
    assert!(bound_is_scoped.contains("The binding uses an app-scoped"));
    assert!(!bound_is_scoped.contains("may instead be the same machine"));
    let we_are_scoped = mismatch(&scoped, &v2);
    assert!(we_are_scoped.contains("This SDK computes an app-scoped"));
    assert!(we_are_scoped.contains("bind this app"));

    let unknown_tag = mismatch(&v2, &format!("mbd2x_{}", "c".repeat(64)));
    assert!(unknown_tag.contains("\"x\""));
    assert!(unknown_tag.contains("newer Moonbase SDK"));
    assert!(!unknown_tag.contains("predating"));

    assert!(mismatch(&v2, &format!("mbd2n_{}", "c".repeat(64))).contains("host-name fallback, while this SDK reads"));
    assert!(
        mismatch(&format!("mbd2n_{}", "a".repeat(64)), &format!("mbd2_{}", "c".repeat(64)))
            .contains("fallen back to the host name")
    );

    let older = mismatch(&v2, &v1);
    assert!(older.starts_with("This license is not for this device. "));
    assert!(older.contains("v1"));
    assert!(older.contains("may instead be the same machine"));
    assert!(older.contains("MigratingDeviceIdResolver"));

    assert!(mismatch(&v2, &"b".repeat(64)).contains("predating versioned device fingerprints"));

    let newer = mismatch(&v2, &v7);
    assert!(newer.contains("Update the SDK"));
    assert!(!newer.contains("Re-activate to find out"));

    assert_eq!(mismatch("my-own-device-id", &v1), "This license is not for this device");
}

#[test]
fn a_migrating_resolver_accepts_a_historical_binding_but_not_a_foreign_one() {
    let current = format!("mbd2_{}", "a".repeat(64));
    let legacy = "b".repeat(64);
    let options = LicensingOptions::new("https://demo.moonbase.sh", "demo-app", PUBLIC_KEY).account_id("tenant-1");
    let resolver: Arc<dyn DeviceIdResolver> = Arc::new(MigratingDeviceIdResolver::new(
        Arc::new(StaticDeviceIdResolver::new("Studio Mac", current.clone())),
        vec![Arc::new(StaticDeviceIdResolver::new("Studio Mac", legacy.clone()))],
    ));
    let validator = LicenseValidator::new(&options, resolver.clone()).unwrap();

    assert_eq!(validator.validate_token(&make_token(&claims_for(&legacy))).unwrap().id, "license-123");
    assert!(validator.validate_token(&make_token(&claims_for(&current))).is_ok());
    let foreign = make_token(&claims_for(&format!("mbd2_{}", "f".repeat(64))));
    assert!(matches!(validator.validate_token(&foreign).unwrap_err(), Error::DeviceMismatch(_)));
    // New activations always bind the current id.
    assert_eq!(resolver.device_id().unwrap(), current);
}
