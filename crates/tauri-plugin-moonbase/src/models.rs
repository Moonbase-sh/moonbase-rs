//! What the plugin hands the frontend. Every type serializes to camelCase JSON.

use std::time::SystemTime;

use moonbase::{
    ActivationMethod, DeviceIdDescription, ErrorType, License, Product, ReleaseAccess, SubProduct,
    SubProductCatalogEntry, User, serde_iso8601,
};
use serde::Serialize;
use serde_json::Value;

/// Where the license stands. Drive your UI off this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// Validating the stored license at startup.
    Loading,
    /// No license on this device. [`LicenseState::lock_reason`] says why, when
    /// there was one.
    Unlicensed,
    /// A browser activation is in progress. A license held from before (a trial
    /// being unlocked) stays in force until the new one lands.
    Activating,
    /// A full license is loaded.
    Licensed,
    /// A trial license is loaded.
    Trial,
    /// The trial has ended. [`LicenseState::expired_trial`] has it, for display.
    TrialExpired,
    /// The license is valid, but doesn't cover this version of the app, such as
    /// one whose updates ended before it came out. The app is locked, and the
    /// license stays stored: [`LicenseState::not_covered`] has it, what it covers,
    /// and where to upgrade.
    NotCovered,
}

/// Why the app lost its license, so the UI can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LockReason {
    /// Deactivated or forgotten on this device, here or in another instance.
    Deactivated,
    /// The license reached its end date.
    Expired,
    /// Moonbase no longer accepts it: revoked, or bound to another device.
    Invalid,
    /// It could not be checked online within the offline grace period.
    Unverified,
    /// It doesn't cover this version of the app. It stays stored; see
    /// [`LicenseState::not_covered`].
    NotCovered,
}

/// The browser activation in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingActivation {
    /// The link the customer finishes activation at. `None` until Moonbase has
    /// created the request; show it (or a copy button) for when no browser opened.
    pub browser_url: Option<String>,
    pub method: ActivationMethod,
    /// The link is here but the browser couldn't be opened for it (no default
    /// browser, say): show it, or a copy button, front and centre.
    pub browser_open_failed: bool,
}

/// A license that is valid, but doesn't cover the version of the app that runs.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotCovered {
    /// The license, as [`LicenseState::license`] shows one: still stored, and good
    /// for the versions it covers.
    pub license: LicenseView,
    /// The version it doesn't cover, as `MAJOR.MINOR.PATCH`; empty when unknown.
    pub app_version: String,
    /// What it covers.
    pub release_access: Option<ReleaseAccess>,
    /// Where the customer buys the upgrade or renewal. Only Moonbase knows it, so
    /// it is `None` when the license was judged on this device (offline, or
    /// checked online moments ago), and when the store sells none. Without it,
    /// `upgradeLicense` starts an online activation, whose page offers the upgrade.
    pub upgrade_url: Option<String>,
}

/// Where the add-on list is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AddOnsPhase {
    /// Not loaded.
    #[default]
    Idle,
    /// Listing; `addOns` keeps what an earlier load found.
    Loading,
    /// Listed, or failed (`error` set).
    Ready,
}

/// The product's add-ons the customer can do something with, as Moonbase lists
/// them for this device, and the add-on an activation or trial is running for.
/// Loaded by `loadAddOns`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddOnsState {
    pub phase: AddOnsPhase,
    /// The currency to show prices in.
    pub currency: Option<String>,
    pub add_ons: Vec<SubProductCatalogEntry>,
    /// Why listing failed, for the customer.
    pub error: Option<String>,
    /// The add-on an activation or trial is running for.
    pub busy_id: Option<String>,
    /// Why the last activation or trial failed, for the customer.
    pub action_error: Option<String>,
}

/// The last operation that failed, worded for the customer, with the SDK's own
/// message for logs and support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub message: String,
    pub detail: String,
    pub kind: Option<ErrorType>,
}

/// A newer release than the running app, from the license's `p:rel` claim. No
/// network involved; call `checkUpdate` for the notes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateHint {
    pub current_version: String,
    pub latest_version: String,
}

/// A snapshot of everything the UI needs, sent with every `moonbase://state-changed`
/// event and returned by every command that changes it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseState {
    pub status: Status,
    /// Is there a valid license right now? Gate features on this.
    pub licensed: bool,
    /// The license in force.
    pub license: Option<LicenseView>,
    /// The trial that ended, with [`Status::TrialExpired`].
    pub expired_trial: Option<LicenseView>,
    pub not_covered: Option<NotCovered>,
    pub lock_reason: Option<LockReason>,
    pub activation: Option<PendingActivation>,
    /// A deactivation is in flight.
    pub busy: bool,
    /// A re-check of the license is waiting for Moonbase.
    pub refreshing: bool,
    pub error: Option<Failure>,
    pub update: Option<UpdateHint>,
    /// The add-ons to unlock on this device right now, by their own part of their
    /// product ID: the license lists them as active and they haven't expired.
    pub active_sub_products: Vec<String>,
    pub add_ons: AddOnsState,
}

/// This device, for an about box or a "copy diagnostics" button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub name: String,
    /// `"Mac"`, `"Windows"`, `"Linux"` or `"Unknown"`.
    pub platform: String,
    /// How the device id was derived: the id, the spec version and the *names*
    /// of the parameters that went into it, never their values.
    pub description: Option<DeviceIdDescription>,
    /// Why there is no description: this machine has no readable identity.
    pub error: Option<String>,
}

/// The release behind an available update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub available: bool,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_notes: String,
    pub has_downloads: bool,
    /// Whether Moonbase lets the customer download the installer (see
    /// [`moonbase::can_download`]).
    pub can_download: bool,
    /// False when Moonbase says the customer's licenses don't cover the new
    /// version (`can_download` is false then too): it needs an upgrade.
    pub covered: bool,
    /// What the customer's licenses cover, when that isn't every release.
    pub release_access: Option<ReleaseAccess>,
    /// Where to buy the upgrade or renewal, while the store sells one.
    pub upgrade_url: Option<String>,
}

/// A license as the frontend sees it: everything the SDK's [`License`] carries but
/// the raw token, which the webview has no use for and is better off never
/// holding, plus `days_remaining`. Serializes as the SDK's license JSON does.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseView {
    pub id: String,
    pub activation_id: String,
    pub trial: bool,
    #[serde(rename = "activationMethod")]
    pub method: ActivationMethod,
    #[serde(rename = "product")]
    pub licensed_product: Product,
    pub issued_to: User,
    #[serde(with = "serde_iso8601")]
    pub issued_at: SystemTime,
    #[serde(with = "serde_iso8601::option", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<SystemTime>,
    #[serde(with = "serde_iso8601")]
    pub validated_at: SystemTime,
    pub owned_sub_product_ids: Vec<String>,
    pub sub_products: Vec<SubProduct>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seat_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seats_used: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_access: Option<ReleaseAccess>,
    pub properties: Value,
    /// Whole days until `expires_at`, rounded up, and 0 once it has passed. `None`
    /// when the license never expires.
    pub days_remaining: Option<u64>,
}

impl From<&License> for LicenseView {
    fn from(license: &License) -> Self {
        // Spelled out rather than spread, so a field added to `License` is a
        // decision here, not a leak.
        let License {
            id,
            activation_id,
            trial,
            method,
            licensed_product,
            issued_to,
            issued_at,
            expires_at,
            validated_at,
            owned_sub_product_ids,
            sub_products,
            subscription_id,
            seat_count,
            seats_used,
            release_access,
            properties,
            token: _,
        } = license.clone();
        let days_remaining = expires_at.map(|expires| match expires.duration_since(SystemTime::now()) {
            Ok(left) => left.as_secs().div_ceil(86_400),
            Err(_) => 0,
        });
        Self {
            id,
            activation_id,
            trial,
            method,
            licensed_product,
            issued_to,
            issued_at,
            expires_at,
            validated_at,
            owned_sub_product_ids,
            sub_products,
            subscription_id,
            seat_count,
            seats_used,
            release_access,
            properties,
            days_remaining,
        }
    }
}
