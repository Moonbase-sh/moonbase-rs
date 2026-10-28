use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::release_access::{ReleaseAccess, lenient_release_access};
use crate::sub_product::{SubProduct, lenient_sub_products};
use crate::time::serde_iso8601;

/// How a license was activated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActivationMethod {
    /// Bound through the API, and re-validated against it.
    #[default]
    #[serde(alias = "online")]
    Online,
    /// Permanent and validated locally for good. Never re-validated online and
    /// never revocable.
    #[serde(alias = "offline")]
    Offline,
}

impl ActivationMethod {
    /// The name the Moonbase API uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "Online",
            Self::Offline => "Offline",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "Online" | "online" => Ok(Self::Online),
            "Offline" | "offline" => Ok(Self::Offline),
            other => Err(Error::LicenseInvalid(format!("Unknown activation method: {other}"))),
        }
    }
}

impl fmt::Display for ActivationMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The platform reported to Moonbase with each request, for analytics and
/// platform-specific downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    Unknown,
    Windows,
    Linux,
    Mac,
}

impl Platform {
    /// The platform this binary was compiled for.
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Unknown
        }
    }

    /// The name the Moonbase API uses, e.g. for the latest-download endpoint.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Linux => "Linux",
            Self::Mac => "Mac",
            Self::Unknown => "Unknown",
        }
    }
}

impl Default for Platform {
    fn default() -> Self {
        Self::current()
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

/// The licensed product, from the token's `p:*` claims.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub id: String,
    pub name: String,
    /// The product's currently released version (`p:rel`), when Moonbase knows it.
    /// Compare it with your own version to offer an update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_release_version: Option<String>,
    #[serde(default = "empty_object")]
    pub properties: Value,
}

/// The customer the license was issued to, from the token's `u:*` claims.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub name: String,
    pub email: String,
    #[serde(default = "empty_object")]
    pub properties: Value,
}

/// A validated license.
///
/// Every field is derived from the signed token. Branch on what the validator
/// returned, never on fields read back out of a store: the store is a cache, not a
/// credential.
///
/// Serializes to the same JSON as the C++ SDK's license file, so the two can share
/// one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct License {
    /// The license id (`l:id`).
    pub id: String,
    /// The activation id (`id`): one per activated device.
    pub activation_id: String,
    pub trial: bool,
    #[serde(rename = "activationMethod")]
    pub method: ActivationMethod,
    #[serde(rename = "product")]
    pub licensed_product: Product,
    pub issued_to: User,
    #[serde(with = "serde_iso8601")]
    pub issued_at: SystemTime,
    /// When the license ends, if it does. An endless trial carries none.
    #[serde(default, with = "serde_iso8601::option", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<SystemTime>,
    /// When the API last vouched for this license.
    #[serde(with = "serde_iso8601")]
    pub validated_at: SystemTime,
    /// Every add-on the customer owns, by its own part of its ID, whether or not it
    /// is included in the license. Ownership only: it says nothing about this
    /// device. For add-ons included in the license, see
    /// [`sub_products`](Self::sub_products).
    #[serde(default)]
    pub owned_sub_product_ids: Vec<String>,
    /// The add-ons included in the product's license that the customer owns or
    /// trials, each with whether it may be unlocked on this device. Empty for a
    /// trial license and for products without such add-ons. Unlock one with
    /// [`has_sub_product`](Self::has_sub_product).
    #[serde(default, deserialize_with = "lenient_sub_products")]
    pub sub_products: Vec<SubProduct>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    /// Total activation seats, when the token says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat_count: Option<i64>,
    /// Seats currently in use, when the token says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seats_used: Option<i64>,
    /// Which versions of the app the license covers; `None` when it covers every
    /// version, which is every license but one bought with limited updates. See
    /// [`covers_version`](Self::covers_version).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient_release_access")]
    pub release_access: Option<ReleaseAccess>,
    /// The license's custom properties (the trial's, for a trial).
    #[serde(default = "empty_object")]
    pub properties: Value,
    /// The raw JWT.
    pub token: String,
}

impl License {
    /// Has `expires_at` passed?
    pub fn is_expired(&self) -> bool {
        self.expires_at.is_some_and(|expires| expires < SystemTime::now())
    }

    /// Does the customer own this sub-product? Ownership only, included in the
    /// license or not; whether to unlock an add-on on this device is
    /// [`has_sub_product`](Self::has_sub_product).
    pub fn owns_sub_product(&self, sub_product_id: &str) -> bool {
        self.owned_sub_product_ids.iter().any(|id| id == sub_product_id)
    }

    /// The add-on as the license lists it. Takes the add-on's own part of its
    /// product ID (`drum-kit`) or its whole ID (`my-app.drum-kit`).
    pub fn find_sub_product(&self, sub_product_id: &str) -> Option<&SubProduct> {
        let own_id = sub_product_id
            .strip_prefix(self.licensed_product.id.as_str())
            .and_then(|rest| rest.strip_prefix('.'))
            .filter(|rest| !rest.is_empty())
            .unwrap_or(sub_product_id);
        self.sub_products.iter().find(|entry| entry.id == own_id)
    }

    /// Whether to unlock one of the product's add-ons on this device: the license
    /// lists it as active, and it has not expired since the license was issued,
    /// which matters for an offline license. Takes the add-on's own part of its
    /// product ID or its whole ID.
    ///
    /// ```
    /// # fn gate(license: &moonbase::License) {
    /// if license.has_sub_product("drum-kit") {
    ///     // unlock the drum kit
    /// }
    /// # }
    /// ```
    pub fn has_sub_product(&self, sub_product_id: &str) -> bool {
        self.has_sub_product_at(sub_product_id, SystemTime::now())
    }

    /// [`has_sub_product`](Self::has_sub_product) as of `now`.
    pub fn has_sub_product_at(&self, sub_product_id: &str, now: SystemTime) -> bool {
        self.find_sub_product(sub_product_id)
            .is_some_and(|entry| entry.active && entry.expires_at.is_none_or(|expires| expires > now))
    }

    /// Whether the license covers a version of the app. One without
    /// [`release_access`](Self::release_access) covers every version. Use it to
    /// check a version other than the running one, such as an update before
    /// offering it.
    pub fn covers_version(&self, version: &str) -> bool {
        self.covers_version_at(version, SystemTime::now())
    }

    /// [`covers_version`](Self::covers_version) as of `now`.
    pub fn covers_version_at(&self, version: &str, now: SystemTime) -> bool {
        self.release_access.as_ref().is_none_or(|access| access.covers_at(version, now))
    }
}

/// A browser activation in progress. Send the user to
/// [`browser_url`](Self::browser_url), then poll with
/// [`Licensing::get_requested_activation`](crate::Licensing::get_requested_activation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationRequest {
    pub id: String,
    #[serde(rename = "request")]
    pub request_url: String,
    #[serde(rename = "browser")]
    pub browser_url: String,
    /// What this request was started with. Set from the argument to
    /// `request_activation`; the API response never carries it.
    #[serde(rename = "activationMethod", default)]
    pub method: ActivationMethod,
}

/// Connection and validation settings.
///
/// ```
/// let options = moonbase::LicensingOptions::new(
///     "https://demo.moonbase.sh",
///     "demo-app",
///     "-----BEGIN PUBLIC KEY-----\n...",
/// );
/// assert_eq!(options.online_validation_min_interval.as_secs(), 300);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LicensingOptions {
    /// Your Moonbase storefront, e.g. `https://your-tenant.moonbase.sh`.
    pub endpoint: String,
    pub product_id: String,
    /// The product's RSA public key: PEM (SPKI or PKCS#1), or bare base64 DER.
    pub public_key: String,
    /// Pins the token issuer when set.
    pub account_id: Option<String>,
    pub platform: Platform,
    /// Reported with every request, and also the version a license has to cover
    /// when it covers only some (see [`License::release_access`]): Moonbase refuses
    /// to validate one that doesn't, and so does the validator on the device unless
    /// [`enforce_release_access`](Self::enforce_release_access) is off. A version
    /// that can't be read (see [`parse_app_version`](crate::version::parse_app_version))
    /// is not checked.
    pub application_version: Option<String>,
    /// Whether the validator refuses a license that doesn't cover
    /// `application_version`, with [`Error::ReleaseNotCovered`], judged by the
    /// device clock. Moonbase refuses such a license when validating online either
    /// way. Default on.
    pub enforce_release_access: bool,
    /// Identifies a higher-level integration built on the SDK, such as the Tauri
    /// plugin. Appended to the `User-Agent` after `moonbase-rs/<version>`; each
    /// layer appends its own space-separated segment, outermost last. Control
    /// characters are stripped and the result capped at 256 characters.
    pub client_info: Option<String>,
    /// Sent as `meta[<key>]` query parameters with license requests.
    pub metadata: BTreeMap<String, String>,
    /// Whether activating and validating the product also takes a seat of each
    /// add-on included in its license that the customer owns, so the license's
    /// `sub_products` report it. Turn it off to take seats yourself with
    /// [`Licensing::activate_sub_product`](crate::Licensing::activate_sub_product).
    /// Default on.
    pub auto_activate_sub_products: bool,
    pub http_connect_timeout: Duration,
    pub http_request_timeout: Duration,
    /// The longest an online-activated license may go without a successful online
    /// check. Within it, a failure that is not a verdict falls back to the local
    /// result. Default 7 days.
    pub online_validation_grace_period: Duration,
    /// If the license was validated more recently than this,
    /// [`Licensing::validate_token_online`](crate::Licensing::validate_token_online)
    /// skips the API. Default 5 minutes.
    pub online_validation_min_interval: Duration,
}

impl LicensingOptions {
    pub fn new(endpoint: impl Into<String>, product_id: impl Into<String>, public_key: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            product_id: product_id.into(),
            public_key: public_key.into(),
            account_id: None,
            platform: Platform::current(),
            application_version: None,
            enforce_release_access: true,
            client_info: None,
            metadata: BTreeMap::new(),
            auto_activate_sub_products: true,
            http_connect_timeout: Duration::from_secs(10),
            http_request_timeout: Duration::from_secs(30),
            online_validation_grace_period: Duration::from_secs(7 * 24 * 60 * 60),
            online_validation_min_interval: Duration::from_secs(5 * 60),
        }
    }

    pub fn account_id(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    pub fn application_version(mut self, version: impl Into<String>) -> Self {
        self.application_version = Some(version.into());
        self
    }

    pub fn enforce_release_access(mut self, enforce: bool) -> Self {
        self.enforce_release_access = enforce;
        self
    }

    pub fn auto_activate_sub_products(mut self, auto_activate: bool) -> Self {
        self.auto_activate_sub_products = auto_activate;
        self
    }

    pub fn client_info(mut self, client_info: impl Into<String>) -> Self {
        self.client_info = Some(client_info.into());
        self
    }

    pub fn platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    pub fn http_timeouts(mut self, connect: Duration, request: Duration) -> Self {
        self.http_connect_timeout = connect;
        self.http_request_timeout = request;
        self
    }

    pub fn online_validation_grace_period(mut self, grace: Duration) -> Self {
        self.online_validation_grace_period = grace;
        self
    }

    pub fn online_validation_min_interval(mut self, interval: Duration) -> Self {
        self.online_validation_min_interval = interval;
        self
    }
}
