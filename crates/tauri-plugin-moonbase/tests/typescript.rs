//! `guest-js/index.d.ts` is written by hand. These keep it honest: every field the
//! plugin sends the frontend, and every value of its string unions, must appear in
//! it, so a field added in Rust and forgotten there fails here.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, UNIX_EPOCH};

use moonbase::fingerprint_spec::DeviceIdSource;
use moonbase::{
    ActivationMethod, DeviceIdDescription, ErrorType, Product, ReleaseAccess, SubProduct, SubProductCatalogEntry,
    SubProductCatalogProduct, SubProductPricing, SubProductRecurrence, SubProductSeats, SubProductTrialOffer,
    SubProductTrialStatus, User,
};
use serde::Serialize;
use serde_json::{Value, json};
use tauri_plugin_moonbase::{
    AddOnsPhase, AddOnsState, DeviceInfo, Failure, LicenseState, LicenseView, LockReason, NotCovered,
    PendingActivation, Status, UpdateHint, UpdateInfo,
};

const TYPES: &str = include_str!("../guest-js/index.d.ts");

/// Fields whose keys are data rather than names: custom properties, and prices by
/// currency.
const MAPS: &[&str] = &["properties", "originalPrice", "price"];

fn keys(value: &Value, into: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                into.insert(key.clone());
                if !MAPS.contains(&key.as_str()) {
                    keys(value, into);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| keys(item, into)),
        _ => {}
    }
}

fn assert_typed(value: &impl Serialize) {
    let mut found = BTreeSet::new();
    keys(&serde_json::to_value(value).unwrap(), &mut found);
    let missing: Vec<_> = found
        .iter()
        .filter(|key| !TYPES.contains(&format!("{key}:")) && !TYPES.contains(&format!("{key}?:")))
        .collect();
    assert!(missing.is_empty(), "guest-js/index.d.ts has no field for {missing:?}");
}

fn assert_union_has(value: &impl Serialize) {
    let name = serde_json::to_value(value).unwrap();
    let name = name.as_str().unwrap();
    assert!(TYPES.contains(&format!("'{name}'")), "guest-js/index.d.ts has no '{name}'");
}

fn license() -> LicenseView {
    let time = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    LicenseView {
        id: "license".into(),
        activation_id: "activation".into(),
        trial: false,
        method: ActivationMethod::Online,
        licensed_product: Product {
            id: "app".into(),
            name: "App".into(),
            current_release_version: Some("2.0.0".into()),
            properties: json!({"anything": 1}),
        },
        issued_to: User { id: "u".into(), name: "U".into(), email: "u@example.com".into(), properties: json!({}) },
        issued_at: time,
        expires_at: Some(time),
        validated_at: time,
        owned_sub_product_ids: vec!["pack".into()],
        sub_products: vec![SubProduct {
            id: "pack".into(),
            active: true,
            trial: true,
            revoked: false,
            expires_at: Some(time),
            seats: Some(SubProductSeats { total: 2, used: 1, remaining: 1 }),
        }],
        subscription_id: Some("s".into()),
        seat_count: Some(2),
        seats_used: Some(1),
        release_access: Some(ReleaseAccess {
            updates_until: Some(time),
            up_to_major: Some(2),
            up_to_version: Some("2.3.0".into()),
        }),
        properties: json!({"anything": 1}),
        days_remaining: Some(3),
    }
}

fn add_on() -> SubProductCatalogEntry {
    let pricing = SubProductPricing {
        id: "v".into(),
        name: "Pack".into(),
        original_price: BTreeMap::from([("EUR".into(), 39.0)]),
        price: BTreeMap::from([("EUR".into(), 29.0)]),
        has_discount: true,
        recurrence: Some(SubProductRecurrence { kind: "Recurring".into(), cycle_length: Some("Monthly".into()) }),
    };
    SubProductCatalogEntry {
        id: "pack".into(),
        product: SubProductCatalogProduct {
            id: "app.pack".into(),
            name: "Pack".into(),
            tagline: Some("t".into()),
            description: Some("d".into()),
            website: Some("w".into()),
            icon_url: Some("i".into()),
            owned: false,
            subscribed: false,
            default_variation: Some(pricing.clone()),
            variations: vec![pricing],
        },
        included_in_license: true,
        active: false,
        revoked: false,
        expires_at: Some(UNIX_EPOCH),
        seats: Some(SubProductSeats { total: 1, used: 1, remaining: 0 }),
        trial: Some(SubProductTrialOffer {
            status: SubProductTrialStatus::Available,
            number_of_days: Some(7),
            expires_at: Some(UNIX_EPOCH),
            requires_newsletter_opt_in: false,
        }),
        buy_url: Some("https://example.com".into()),
    }
}

#[test]
fn every_field_of_the_state_is_typed() {
    let state = LicenseState {
        status: Status::Licensed,
        licensed: true,
        license: Some(license()),
        expired_trial: Some(license()),
        not_covered: Some(NotCovered {
            license: license(),
            app_version: "3.0.0".into(),
            release_access: Some(ReleaseAccess { updates_until: None, up_to_major: Some(2), up_to_version: None }),
            upgrade_url: Some("https://example.com".into()),
        }),
        lock_reason: Some(LockReason::Expired),
        activation: Some(PendingActivation {
            browser_url: Some("https://example.com".into()),
            method: ActivationMethod::Online,
            browser_open_failed: true,
        }),
        busy: false,
        refreshing: false,
        error: Some(Failure { message: "m".into(), detail: "d".into(), kind: Some(ErrorType::ApiError) }),
        update: Some(UpdateHint { current_version: "1.0.0".into(), latest_version: "2.0.0".into() }),
        active_sub_products: vec!["pack".into()],
        add_ons: AddOnsState {
            phase: AddOnsPhase::Ready,
            currency: Some("EUR".into()),
            add_ons: vec![add_on()],
            error: Some("e".into()),
            busy_id: Some("pack".into()),
            action_error: Some("a".into()),
        },
    };
    assert_typed(&state);
}

#[test]
fn every_field_of_the_other_answers_is_typed() {
    assert_typed(&UpdateInfo {
        available: true,
        current_version: "1.0.0".into(),
        latest_version: Some("2.0.0".into()),
        release_notes: "n".into(),
        has_downloads: true,
        can_download: false,
        covered: false,
        release_access: Some(ReleaseAccess {
            updates_until: Some(UNIX_EPOCH),
            up_to_major: Some(1),
            up_to_version: None,
        }),
        upgrade_url: Some("https://example.com".into()),
    });
    assert_typed(&DeviceInfo {
        name: "Studio".into(),
        platform: "Mac".into(),
        description: Some(DeviceIdDescription {
            device_id: "mbd2_x".into(),
            version: 2,
            platform: "macos".into(),
            source: DeviceIdSource::Identity,
            param_names: vec!["IOPlatformUUID".into()],
        }),
        error: Some("e".into()),
    });
}

#[test]
fn every_value_of_the_string_unions_is_typed() {
    // A match with no catch-all, so a new variant doesn't compile until it is
    // listed here, and checked.
    for status in [
        Status::Loading,
        Status::Unlicensed,
        Status::Activating,
        Status::Licensed,
        Status::Trial,
        Status::TrialExpired,
        Status::NotCovered,
    ] {
        match status {
            Status::Loading
            | Status::Unlicensed
            | Status::Activating
            | Status::Licensed
            | Status::Trial
            | Status::TrialExpired
            | Status::NotCovered => assert_union_has(&status),
        }
    }
    for reason in [
        LockReason::Deactivated,
        LockReason::Expired,
        LockReason::Invalid,
        LockReason::Unverified,
        LockReason::NotCovered,
    ] {
        match reason {
            LockReason::Deactivated
            | LockReason::Expired
            | LockReason::Invalid
            | LockReason::Unverified
            | LockReason::NotCovered => assert_union_has(&reason),
        }
    }
    for phase in [AddOnsPhase::Idle, AddOnsPhase::Loading, AddOnsPhase::Ready] {
        assert_union_has(&phase);
    }
    for status in [
        SubProductTrialStatus::Unavailable,
        SubProductTrialStatus::Available,
        SubProductTrialStatus::Active,
        SubProductTrialStatus::Expired,
    ] {
        assert_union_has(&status);
    }
    for source in [DeviceIdSource::Identity, DeviceIdSource::DeviceName, DeviceIdSource::Scoped] {
        assert_union_has(&source);
    }
    for kind in [
        ErrorType::ApiError,
        ErrorType::LicenseInvalid,
        ErrorType::LicenseExpired,
        ErrorType::StorageError,
        ErrorType::ConfigurationError,
        ErrorType::OperationNotSupported,
        ErrorType::LicenseDeviceMismatch,
        ErrorType::DeviceIdentityUnavailable,
        ErrorType::ActivationRequestEnded,
        ErrorType::StoreClosed,
        ErrorType::NoEligibleLicense,
        ErrorType::ReleaseNotCovered,
    ] {
        assert_union_has(&kind);
    }
    for screen in [
        tauri_plugin_moonbase::Screen::License,
        tauri_plugin_moonbase::Screen::Update,
        tauri_plugin_moonbase::Screen::Offline,
        tauri_plugin_moonbase::Screen::AddOns,
    ] {
        assert_union_has(&screen);
    }
}
