//! Local RS256 license validation.

use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{Map, Value};

use crate::crypto::{RsaPublicKey, base64url_decode};
use crate::device_id::DeviceIdResolver;
use crate::error::{Error, Result};
use crate::fingerprint_spec::{DeviceIdSource, DeviceIdStamp, parse_device_id_stamp};
use crate::release_access::{ReleaseAccess, ReleaseNotCovered};
use crate::sub_product::sub_products_from_claim;
use crate::time::{from_epoch_seconds, from_epoch_seconds_clamped, parse_iso8601_utc};
use crate::types::{ActivationMethod, License, LicensingOptions, Product, User};
use crate::version::parse_app_version;

fn invalid(message: impl Into<String>) -> Error {
    Error::LicenseInvalid(message.into())
}

pub(crate) fn split_jwt(token: &str) -> Result<[&str; 3]> {
    let mut parts = token.split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(header), Some(payload), Some(signature), None)
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty() =>
        {
            Ok([header, payload, signature])
        }
        _ => Err(invalid("License token is malformed")),
    }
}

pub(crate) fn decode_jwt_json(encoded: &str, label: &str) -> Result<Value> {
    let bytes =
        base64url_decode(encoded).map_err(|error| invalid(format!("License token {label} is malformed: {error}")))?;
    serde_json::from_slice(&bytes).map_err(|error| invalid(format!("License token {label} is malformed: {error}")))
}

fn has_audience(payload: &Value, expected: &str) -> bool {
    match payload.get("aud") {
        Some(Value::String(audience)) => audience == expected,
        Some(Value::Array(audiences)) => audiences.iter().any(|item| item.as_str() == Some(expected)),
        _ => false,
    }
}

fn require_string(payload: &Value, key: &str) -> Result<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("License token is missing string claim {key}")))
}

fn optional_string(payload: &Value, key: &str) -> Result<Option<String>> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid(format!("License token claim is not a string: {key}"))),
    }
}

fn require_boolish(payload: &Value, key: &str) -> Result<bool> {
    match payload.get(key) {
        None => Err(invalid(format!("License token is missing boolean claim {key}"))),
        Some(Value::Bool(value)) => Ok(*value),
        Some(Value::String(text)) if text == "true" || text == "True" => Ok(true),
        Some(Value::String(text)) if text == "false" || text == "False" => Ok(false),
        Some(_) => Err(invalid(format!("License token claim is not boolean: {key}"))),
    }
}

fn require_integer(payload: &Value, key: &str) -> Result<i64> {
    payload
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| invalid(format!("License token is missing integer claim {key}")))
}

fn optional_integer(payload: &Value, key: &str) -> Result<Option<i64>> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            value.as_i64().map(Some).ok_or_else(|| invalid(format!("License token claim is not an integer: {key}")))
        }
    }
}

/// An integer claim, or a string holding one. Anything else reads as absent rather
/// than failing the license, since a later release may change its shape.
fn lenient_integer(payload: &Value, key: &str) -> Option<i64> {
    match payload.get(key)? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => {
            let digits = text.strip_prefix('-').unwrap_or(text);
            // Eighteen digits cannot overflow, and no claim this reads needs more.
            if digits.is_empty() || digits.len() > 18 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            text.parse().ok()
        }
        _ => None,
    }
}

/// Which versions the license covers, from its `l:rel:until` (seconds since the
/// epoch), `l:rel:major` and `l:rel:upto` claims, or `None` when it covers every
/// version. Read leniently: a claim that can't be read is left out rather than
/// failing the license, and Moonbase refuses a version it doesn't cover anyway.
fn read_release_access(payload: &Value) -> Option<ReleaseAccess> {
    let mut access = ReleaseAccess {
        // Held to the years 1 to 9999 and what this platform's clock holds, rather
        // than dropped: dropping the end of updates would leave a major limit
        // alone, covering fewer versions than Moonbase does.
        updates_until: lenient_integer(payload, "l:rel:until").map(from_epoch_seconds_clamped),
        up_to_major: lenient_integer(payload, "l:rel:major").and_then(|major| u64::try_from(major).ok()),
        up_to_version: None,
    };
    if access.updates_until.is_none() && access.up_to_major.is_none() {
        // As Moonbase reads a token: without an end to updates or a highest major,
        // it covers every release, so an l:rel:upto alone means nothing.
        return None;
    }
    access.up_to_version = payload
        .get("l:rel:upto")
        .and_then(Value::as_str)
        .filter(|version| parse_app_version(version).is_some())
        .map(str::to_owned);
    Some(access)
}

fn split_claim_list(value: Option<String>) -> Vec<String> {
    value.map(|list| list.split(',').filter(|item| !item.is_empty()).map(str::to_owned).collect()).unwrap_or_default()
}

fn require_validation_time(payload: &Value) -> Result<SystemTime> {
    if let Some(epoch) = optional_integer(payload, "validated")? {
        return Ok(from_epoch_seconds(epoch));
    }
    if let Some(iso) = optional_string(payload, "ver")? {
        return parse_iso8601_utc(&iso)
            .map_err(|error| invalid(format!("License token validation timestamp is malformed: {error}")));
    }
    Err(invalid("License token is missing validation timestamp"))
}

fn object_claim_or_empty(payload: &Value, key: &str) -> Result<Value> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(Value::Object(Map::new())),
        Some(value @ Value::Object(_)) => Ok(value.clone()),
        Some(_) => Err(invalid(format!("License token claim is not an object: {key}"))),
    }
}

/// A factual note when the binding's stamp differs from what this SDK computes.
///
/// Deliberately conditional about the machine. Reaching this point means nothing
/// reproduced the bound id, so the stamp says only which algorithm created the
/// binding, never that it was *this* machine: a token copied from another
/// computer carries exactly the same relationship.
fn describe_version_difference(expected: &DeviceIdStamp, bound: Option<&DeviceIdStamp>) -> String {
    let expected_version = expected.version;
    if let Some(bound) = bound.filter(|bound| bound.version > expected_version) {
        return format!(
            "The binding was created by device fingerprint v{}, which is newer than the v{expected_version} this SDK \
             computes. Update the SDK rather than re-activating, which would rebind the device to the older algorithm.",
            bound.version
        );
    }
    let bound_version = match bound {
        Some(bound) => format!("device fingerprint v{}", bound.version),
        None => "an SDK predating versioned device fingerprints".to_owned(),
    };
    format!(
        "The binding was created by {bound_version}, while this SDK computes v{expected_version}, so this may instead \
         be the same machine bound under the older algorithm. Re-activate to find out, or configure a \
         MigratingDeviceIdResolver to keep accepting the previous id."
    )
}

/// Same version, different source tag: the ids were built from different *kinds*
/// of identity, so they were never going to match even on one machine.
fn describe_source_difference(expected: &DeviceIdStamp, bound: &DeviceIdStamp) -> String {
    if bound.source == Some(DeviceIdSource::Scoped) {
        return "The binding uses an app-scoped device identity, which cannot be compared with the id this SDK \
                computes, not even on the same device. Re-activate here to bind this build."
            .to_owned();
    }
    if expected.source == Some(DeviceIdSource::Scoped) {
        return "This SDK computes an app-scoped device identity, which cannot be compared with the one the binding \
                carries, not even on the same device. Re-activate here to bind this app."
            .to_owned();
    }
    // An unrecognised tag can only have come from a newer SDK.
    if bound.source.is_none() || expected.source.is_none() {
        let unknown = if bound.source.is_none() { bound } else { expected };
        return format!(
            "The binding carries the device identity tag \"{}\", which this SDK does not recognise. It was created by \
             a newer Moonbase SDK, so update rather than re-activating.",
            unknown.source_tag
        );
    }
    if bound.source == Some(DeviceIdSource::DeviceName) {
        return "The binding was created from the host-name fallback, while this SDK reads hardware identity, so \
                this may instead be the same machine bound while no hardware identity could be read. Re-activate to \
                find out."
            .to_owned();
    }
    "The binding was created from hardware identity, while this SDK has fallen back to the host name. Check why \
     hardware identity cannot be read here rather than re-activating, which would rebind the device to the weaker id."
        .to_owned()
}

fn describe_stamp_difference(expected: &str, bound: &str) -> Option<String> {
    // A custom resolver's id is compared literally: nothing to say about stamps.
    let expected = parse_device_id_stamp(expected)?;
    let bound = parse_device_id_stamp(bound);
    match &bound {
        None => Some(describe_version_difference(&expected, None)),
        Some(stamp) if stamp.version != expected.version => Some(describe_version_difference(&expected, Some(stamp))),
        Some(stamp) if stamp.source_tag != expected.source_tag => Some(describe_source_difference(&expected, stamp)),
        Some(_) => None,
    }
}

/// Explain a `sig` mismatch. Leads with the only certain thing, that the bound id
/// is not this device's, and appends the stamp difference when there is one.
pub(crate) fn device_mismatch_error(expected: &str, bound: &str) -> Error {
    const DETAIL: &str = "This license is not for this device";
    Error::DeviceMismatch(match describe_stamp_difference(expected, bound) {
        Some(note) => format!("{DETAIL}. {note}"),
        None => DETAIL.to_owned(),
    })
}

/// Validates license tokens locally: signature, audience, issuer, claims, expiry
/// and device binding.
pub struct LicenseValidator {
    product_id: String,
    account_id: Option<String>,
    application_version: Option<String>,
    enforce_release_access: bool,
    device_ids: Arc<dyn DeviceIdResolver>,
    key: RsaPublicKey,
}

impl LicenseValidator {
    pub fn new(options: &LicensingOptions, device_ids: Arc<dyn DeviceIdResolver>) -> Result<Self> {
        Ok(Self {
            product_id: options.product_id.clone(),
            account_id: options.account_id.clone(),
            application_version: options.application_version.clone(),
            enforce_release_access: options.enforce_release_access,
            device_ids,
            key: RsaPublicKey::parse(&options.public_key)?,
        })
    }

    /// Check a token on the device, with no network: its signature, audience,
    /// issuer, expiry, that it is bound to this device, and that it covers
    /// `application_version` (unless `enforce_release_access` is off), failing
    /// with [`Error::ReleaseNotCovered`] when it doesn't.
    pub fn validate_token(&self, token: &str) -> Result<License> {
        let license = self.validate(token, false)?;
        self.ensure_covers_application_version(&license)?;
        Ok(license)
    }

    /// Like [`validate_token`](Self::validate_token), but neither a past `exp` nor a
    /// version the license doesn't cover is an error. For operations like revoke,
    /// where the seat may still be allocated server-side after the local token has
    /// aged out. Every other check applies.
    pub fn validate_token_allow_expired(&self, token: &str) -> Result<License> {
        self.validate(token, true)
    }

    /// Like [`validate_token`](Self::validate_token), but without the release
    /// access check: for a license Moonbase is about to judge itself, which may
    /// answer with a token that covers more, such as after the customer bought an
    /// upgrade.
    pub fn validate_token_ignoring_release_access(&self, token: &str) -> Result<License> {
        self.validate(token, false)
    }

    /// Fail with [`Error::ReleaseNotCovered`] when the license doesn't cover
    /// `application_version`, judged by the device clock. Passes when
    /// `enforce_release_access` is off, when no `application_version` is set, and
    /// when it can't be read as a version.
    pub fn ensure_covers_application_version(&self, license: &License) -> Result<()> {
        if !self.enforce_release_access {
            return Ok(());
        }
        let Some(version) = &self.application_version else {
            return Ok(());
        };
        let Some(parsed) = parse_app_version(version) else {
            return Ok(());
        };
        if license.covers_version(version) {
            return Ok(());
        }
        let app_version = parsed.to_string();
        Err(Error::ReleaseNotCovered(Box::new(ReleaseNotCovered::new(
            format!("The license doesn't cover version {app_version}"),
            Some(license.clone()),
            app_version,
            None,
            None,
        ))))
    }

    fn validate(&self, token: &str, allow_expired: bool) -> Result<License> {
        let token = token.trim_ascii();
        let [encoded_header, encoded_payload, encoded_signature] = split_jwt(token)?;
        let header = decode_jwt_json(encoded_header, "header")?;
        let payload = decode_jwt_json(encoded_payload, "payload")?;

        if header.get("alg").and_then(Value::as_str) != Some("RS256") {
            return Err(invalid("License token algorithm is not supported"));
        }

        let signature = base64url_decode(encoded_signature)
            .map_err(|error| invalid(format!("License token signature is malformed: {error}")))?;
        let signing_input = &token[..encoded_header.len() + 1 + encoded_payload.len()];
        self.key.verify_rs256(signing_input.as_bytes(), &signature)?;

        if !has_audience(&payload, &self.product_id) {
            return Err(invalid("License token audience does not match the configured product"));
        }
        if let Some(account_id) = &self.account_id
            && payload.get("iss").and_then(Value::as_str) != Some(account_id.as_str())
        {
            return Err(invalid("License token issuer does not match the configured account"));
        }

        let trial = require_boolish(&payload, "trial")?;
        let properties = object_claim_or_empty(&payload, if trial { "t:properties" } else { "l:properties" })?;

        // Seat counts. The backend issues p:seats:total / p:seats:used; fall back
        // to alternative spellings and to the license properties.
        let seat_from_properties = |key: &str| properties.get(key).and_then(Value::as_i64);
        let seat_count = match optional_integer(&payload, "p:seats:total")? {
            Some(count) => Some(count),
            None => optional_integer(&payload, "seats")?.or_else(|| seat_from_properties("seats")),
        };
        let seats_used = match optional_integer(&payload, "p:seats:used")? {
            Some(used) => Some(used),
            None => optional_integer(&payload, "seatsUsed")?.or_else(|| seat_from_properties("seatsUsed")),
        };

        let license = License {
            activation_id: require_string(&payload, "id")?,
            id: require_string(&payload, "l:id")?,
            trial,
            method: ActivationMethod::parse(&require_string(&payload, "method")?)?,
            issued_at: from_epoch_seconds(require_integer(&payload, "iat")?),
            expires_at: optional_integer(&payload, "exp")?.map(from_epoch_seconds),
            validated_at: require_validation_time(&payload)?,
            licensed_product: Product {
                id: require_string(&payload, "p:id")?,
                name: require_string(&payload, "p:name")?,
                current_release_version: optional_string(&payload, "p:rel")?,
                properties: object_claim_or_empty(&payload, "p:properties")?,
            },
            owned_sub_product_ids: split_claim_list(optional_string(&payload, "sp:owned")?),
            sub_products: sub_products_from_claim(&payload),
            subscription_id: optional_string(&payload, "s:id")?,
            issued_to: User {
                id: require_string(&payload, "u:id")?,
                name: require_string(&payload, "u:name")?,
                email: require_string(&payload, "u:email")?,
                properties: object_claim_or_empty(&payload, "u:properties")?,
            },
            seat_count,
            seats_used,
            release_access: read_release_access(&payload),
            properties,
            token: token.to_owned(),
        };

        if !allow_expired && license.is_expired() {
            return Err(Error::LicenseExpired("License has expired".into()));
        }

        let expected = self.device_ids.device_id()?;
        let actual = require_string(&payload, "sig")?;
        // The literal comparison first, so an app without a migration pays nothing.
        // Only on a mismatch does a MigratingDeviceIdResolver get the chance to
        // vouch for an id this machine used to be bound to.
        if actual != expected && !self.device_ids.accepts_device_id(&actual) {
            return Err(device_mismatch_error(&expected, &actual));
        }

        Ok(license)
    }
}

impl std::fmt::Debug for LicenseValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LicenseValidator")
            .field("product_id", &self.product_id)
            .field("account_id", &self.account_id)
            .field("application_version", &self.application_version)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}
