//! Add-ons: sub-products of the licensed product (with a product ID like
//! `my-app.drum-kit`) that the merchant includes in its license. The license
//! reports them for this device, and the API lists them with prices, ownership,
//! seats and trials.

use std::collections::BTreeMap;
use std::time::SystemTime;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::time::{from_epoch_seconds_clamped, parse_api_timestamp, parse_iso8601_utc, serde_iso8601};

/// Seats across all of the customer's licenses for an add-on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubProductSeats {
    pub total: i64,
    pub used: i64,
    pub remaining: i64,
}

/// One of the product's add-ons that the merchant includes in its license, as the
/// license reports it for this device. To decide whether to unlock it, use
/// [`License::has_sub_product`](crate::License::has_sub_product), which also checks
/// `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubProduct {
    /// The add-on's own part of its product ID: `drum-kit` for `my-app.drum-kit`.
    pub id: String,
    /// Whether it may be unlocked on this device, as of when the license was
    /// issued: the device holds a seat of it, or its trial has not run out. An
    /// add-on the customer owns, but with every seat taken elsewhere, is listed and
    /// not active.
    pub active: bool,
    /// A trial backs it rather than a license the customer owns.
    pub trial: bool,
    /// Its seat on this device was revoked, by the customer or the merchant, so
    /// validating does not take it back: offer
    /// [`Licensing::activate_sub_product`](crate::Licensing::activate_sub_product).
    pub revoked: bool,
    /// When it ends. `None` when it never does.
    #[serde(serialize_with = "serde_iso8601::option::serialize", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<SystemTime>,
    /// `None` in offline licenses and for trials.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seats: Option<SubProductSeats>,
}

// Lenient readers for the optional parts of an add-on, in a license token, a
// license file or an API answer. A part that is absent, null or of another type
// reads as not there, so a later release that adds to these shapes is no reason to
// fail.

fn optional_flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn optional_text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn number_as_i64(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_f64().map(|number| number as i64))
}

fn optional_seats(value: &Value, key: &str) -> Option<SubProductSeats> {
    let seats = value.get(key).filter(|seats| seats.is_object())?;
    Some(SubProductSeats {
        total: number_as_i64(seats.get("total")?)?,
        used: number_as_i64(seats.get("used")?)?,
        remaining: number_as_i64(seats.get("remaining")?)?,
    })
}

impl SubProduct {
    /// An entry of the token's `sp` claim, `{id, active, trial, revoked?, exp?,
    /// seats?}`, or `None` when it has no id.
    pub(crate) fn from_claim(entry: &Value) -> Option<Self> {
        Some(Self {
            id: entry.get("id")?.as_str()?.to_owned(),
            active: optional_flag(entry, "active"),
            trial: optional_flag(entry, "trial"),
            revoked: optional_flag(entry, "revoked"),
            expires_at: entry.get("exp").and_then(number_as_i64).map(from_epoch_seconds_clamped),
            seats: optional_seats(entry, "seats"),
        })
    }

    /// An entry of the license file's `subProducts`, or `None` when it can't be
    /// read, including an `expiresAt` that isn't a time.
    fn from_stored(entry: &Value) -> Option<Self> {
        let expires_at = match entry.get("expiresAt") {
            Some(Value::String(text)) => Some(parse_iso8601_utc(text).ok()?),
            _ => None,
        };
        Some(Self {
            id: entry.get("id")?.as_str()?.to_owned(),
            active: optional_flag(entry, "active"),
            trial: optional_flag(entry, "trial"),
            revoked: optional_flag(entry, "revoked"),
            expires_at,
            seats: optional_seats(entry, "seats"),
        })
    }
}

/// The token's `sp` claim: the add-ons included in the product's license. An entry
/// this release cannot read is skipped rather than failing the whole license,
/// since a later release may add to its shape.
pub(crate) fn sub_products_from_claim(payload: &Value) -> Vec<SubProduct> {
    payload
        .get("sp")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter(|entry| entry.is_object()).filter_map(SubProduct::from_claim).collect())
        .unwrap_or_default()
}

/// The license file's `subProducts`. Absent in a license stored before add-ons
/// were reported. An entry that can't be read is skipped rather than failing the
/// license, which a store would then delete as corrupt.
pub(crate) fn lenient_sub_products<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<SubProduct>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value.as_array().map(|entries| entries.iter().filter_map(SubProduct::from_stored).collect()).unwrap_or_default())
}

/// How a price recurs: `"OneOff"`, or `"Recurring"` with how often it renews
/// (`"Daily"`, `"Weekly"`, `"Monthly"`, `"Quarterly"` or `"Yearly"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubProductRecurrence {
    #[serde(rename = "type")]
    pub kind: String,
    /// `None` for `"OneOff"`.
    pub cycle_length: Option<String>,
}

/// One way an add-on is sold.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubProductPricing {
    pub id: String,
    pub name: String,
    /// What it costs before discounts, by currency code.
    pub original_price: BTreeMap<String, f64>,
    /// What it costs now, by currency code, after any discount.
    pub price: BTreeMap<String, f64>,
    pub has_discount: bool,
    pub recurrence: Option<SubProductRecurrence>,
}

/// The add-on, with its prices when it is for sale, and whether the customer has it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubProductCatalogProduct {
    /// The add-on's whole product ID: `my-app.drum-kit`.
    pub id: String,
    pub name: String,
    pub tagline: Option<String>,
    pub description: Option<String>,
    pub website: Option<String>,
    pub icon_url: Option<String>,
    pub owned: bool,
    pub subscribed: bool,
    /// The price to show first. `None` when it is not for sale.
    pub default_variation: Option<SubProductPricing>,
    /// Every way it is sold. Empty when it is not for sale.
    pub variations: Vec<SubProductPricing>,
}

/// Where an add-on's trial stands on this device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SubProductTrialStatus {
    /// It offers trials, but not to this device now: it is not included in the
    /// license, the customer owns it, the license is a trial or an offline license,
    /// or the trial asks for a newsletter opt-in the customer has not given. Also
    /// what a status this version does not know reads as.
    #[default]
    Unavailable,
    /// [`Licensing::request_sub_product_trial`](crate::Licensing::request_sub_product_trial)
    /// can start one now.
    Available,
    /// The device's trial is running.
    Active,
    /// The device had its trial, and it has ended.
    Expired,
}

impl SubProductTrialStatus {
    fn parse(value: &str) -> Self {
        match value {
            "Available" => Self::Available,
            "Active" => Self::Active,
            "Expired" => Self::Expired,
            _ => Self::Unavailable,
        }
    }
}

/// An add-on's trial on this device, or whether one can be started.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubProductTrialOffer {
    pub status: SubProductTrialStatus,
    /// How long a trial lasts. `None` when its trials never expire.
    pub number_of_days: Option<u32>,
    /// When the device's trial ends. `None` when it has none, or one that never
    /// expires.
    #[serde(serialize_with = "serde_iso8601::option::serialize")]
    pub expires_at: Option<SystemTime>,
    /// The trial needs the customer to opt in to the newsletter, which they have not.
    pub requires_newsletter_opt_in: bool,
}

/// One of the product's add-ons as
/// [`Licensing::list_sub_products`](crate::Licensing::list_sub_products) reports it
/// for this device.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubProductCatalogEntry {
    /// The add-on's own part of its product ID, as the license's `sub_products`
    /// names it.
    pub id: String,
    pub product: SubProductCatalogProduct,
    /// Whether it is included in the product's license, so the license reports it.
    pub included_in_license: bool,
    /// Whether to unlock it on this device. For an add-on included in the license,
    /// what [`License::has_sub_product`](crate::License::has_sub_product) would say.
    /// For any other, whether the customer owns or subscribes to it.
    pub active: bool,
    /// Its seat on this device was revoked, so validating does not take it back:
    /// offer [`Licensing::activate_sub_product`](crate::Licensing::activate_sub_product).
    pub revoked: bool,
    /// When the license or trial behind it ends, for one included in the license.
    #[serde(serialize_with = "serde_iso8601::option::serialize")]
    pub expires_at: Option<SystemTime>,
    /// Seats across the customer's licenses for it, for one included in the
    /// license that they own.
    pub seats: Option<SubProductSeats>,
    /// Its trial on this device, or whether one can be started. `None` when it
    /// offers none.
    pub trial: Option<SubProductTrialOffer>,
    /// Where the customer buys it, when it is for sale. Add `?variation={id}` for
    /// one of its variations.
    pub buy_url: Option<String>,
}

/// The product's add-ons that the customer can do something with: those they own
/// or subscribe to, those on sale, and those whose trial can be started or is
/// running on this device.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubProductCatalog {
    /// The currency to show prices in, as the store suggests it for the customer.
    pub currency: String,
    pub sub_products: Vec<SubProductCatalogEntry>,
}

// The add-on list. Required parts fail the whole answer, so a broken one fails as
// a whole; optional ones are read leniently, and a trial status this version does
// not know reads as unavailable, since the server may add statuses.

type Parsed<T> = Result<T, String>;

fn required<'a>(value: &'a Value, key: &str) -> Parsed<&'a Value> {
    value.get(key).ok_or_else(|| format!("missing {key}"))
}

fn required_text(value: &Value, key: &str) -> Parsed<String> {
    required(value, key)?.as_str().map(str::to_owned).ok_or_else(|| format!("{key} is not a string"))
}

fn required_flag(value: &Value, key: &str) -> Parsed<bool> {
    required(value, key)?.as_bool().ok_or_else(|| format!("{key} is not a boolean"))
}

/// A time in an API answer, which must read if it is there at all: a time that
/// can't be read is a broken answer, not a time that never comes.
fn optional_api_time(value: &Value, key: &str) -> Parsed<Option<SystemTime>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => parse_api_timestamp(text).map(Some).map_err(|_| format!("{key} is not a time")),
        Some(_) => Err(format!("{key} is not a time")),
    }
}

fn price_by_currency(value: &Value, key: &str) -> Parsed<BTreeMap<String, f64>> {
    let prices = required(value, key)?;
    Ok(prices
        .as_object()
        .map(|prices| {
            prices
                .iter()
                .filter_map(|(currency, amount)| amount.as_f64().map(|amount| (currency.clone(), amount)))
                .collect()
        })
        .unwrap_or_default())
}

fn parse_recurrence(value: &Value) -> Parsed<SubProductRecurrence> {
    Ok(SubProductRecurrence { kind: required_text(value, "type")?, cycle_length: optional_text(value, "cycleLength") })
}

fn parse_pricing(value: &Value) -> Parsed<SubProductPricing> {
    Ok(SubProductPricing {
        id: required_text(value, "id")?,
        name: required_text(value, "name")?,
        original_price: price_by_currency(value, "originalPrice")?,
        price: price_by_currency(value, "price")?,
        has_discount: required_flag(value, "hasDiscount")?,
        recurrence: match value.get("recurrence") {
            Some(recurrence) if recurrence.is_object() => Some(parse_recurrence(recurrence)?),
            _ => None,
        },
    })
}

fn parse_product(value: &Value) -> Parsed<SubProductCatalogProduct> {
    Ok(SubProductCatalogProduct {
        id: required_text(value, "id")?,
        name: required_text(value, "name")?,
        tagline: optional_text(value, "tagline"),
        description: optional_text(value, "description"),
        website: optional_text(value, "website"),
        icon_url: optional_text(value, "iconUrl"),
        owned: required_flag(value, "owned")?,
        subscribed: required_flag(value, "subscribed")?,
        default_variation: match value.get("defaultVariation") {
            None | Some(Value::Null) => None,
            Some(pricing) => Some(parse_pricing(pricing)?),
        },
        variations: match value.get("variations") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(variations)) => variations.iter().map(parse_pricing).collect::<Parsed<_>>()?,
            Some(_) => return Err("variations is not a list".into()),
        },
    })
}

fn parse_trial(value: &Value) -> Parsed<SubProductTrialOffer> {
    Ok(SubProductTrialOffer {
        status: SubProductTrialStatus::parse(value.get("status").and_then(Value::as_str).unwrap_or_default()),
        number_of_days: value
            .get("numberOfDays")
            .and_then(number_as_i64)
            .map(|days| u32::try_from(days.max(0)).unwrap_or(u32::MAX)),
        expires_at: optional_api_time(value, "expiresAt")?,
        requires_newsletter_opt_in: optional_flag(value, "requiresNewsletterOptIn"),
    })
}

fn parse_entry(value: &Value) -> Parsed<SubProductCatalogEntry> {
    Ok(SubProductCatalogEntry {
        id: required_text(value, "id")?,
        product: parse_product(required(value, "product")?)?,
        included_in_license: required_flag(value, "includedInLicense")?,
        active: required_flag(value, "active")?,
        revoked: optional_flag(value, "revoked"),
        expires_at: optional_api_time(value, "expiresAt")?,
        seats: optional_seats(value, "seats"),
        trial: match value.get("trial") {
            Some(trial) if trial.is_object() => Some(parse_trial(trial)?),
            _ => None,
        },
        buy_url: optional_text(value, "buyUrl").filter(|url| !url.is_empty()),
    })
}

pub(crate) fn parse_catalog(value: &Value) -> Parsed<SubProductCatalog> {
    let entries = required(value, "subProducts")?.as_array().ok_or("subProducts is not a list")?;
    Ok(SubProductCatalog {
        currency: required_text(value, "currency")?,
        sub_products: entries.iter().map(parse_entry).collect::<Parsed<_>>()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_sp_claim_is_read_leniently() {
        let read = sub_products_from_claim(&json!({"sp": [
            {"active": true},
            "pack",
            {"id": 5, "active": true},
            {"id": "strings", "active": true, "trial": "yes", "future": {"field": 1}, "seats": {"total": 2}},
        ]}));
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, "strings");
        assert!(read[0].active);
        assert!(!read[0].trial, "not a boolean");
        assert_eq!(read[0].seats, None, "incomplete");

        assert!(sub_products_from_claim(&json!({})).is_empty());
        assert!(sub_products_from_claim(&json!({"sp": {"id": "pack"}})).is_empty());
    }
}
