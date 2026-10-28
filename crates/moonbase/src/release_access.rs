//! Licenses that cover only some versions of the app: what such a license covers,
//! whether it covers a version, and what a refusal for one it doesn't carries.
//! The rule is the one Moonbase and every other Moonbase SDK apply;
//! `tests/vectors/release-access-vectors.json` pins it down case by case.

use std::fmt;
use std::time::SystemTime;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::time::{parse_iso8601_utc, serde_iso8601};
use crate::types::License;
use crate::version::parse_app_version;

/// Which versions of the app a license covers, when it doesn't cover every one.
///
/// A license bought outright can cover the releases published while its updates
/// lasted ("12 months of updates"), or every version up to a major ("versions up
/// to 2.x"). It never expires because of it; it just doesn't cover the versions
/// after that. The two limits add up rather than narrow each other: a license with
/// both covers a version that either one covers. Check a version with
/// [`covers`](Self::covers), or [`License::covers_version`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAccess {
    /// Releases published until then are covered, and every version while it has
    /// not passed.
    #[serde(serialize_with = "serde_iso8601::option::serialize", skip_serializing_if = "Option::is_none")]
    pub updates_until: Option<SystemTime>,
    /// Every version whose major is at most this is covered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub up_to_major: Option<u64>,
    /// Once `updates_until` has passed, the last version published before it
    /// (`MAJOR.MINOR.PATCH`), covered along with every older one. `None` while
    /// updates last, and when nothing was published before they ended (then every
    /// version is covered).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub up_to_version: Option<String>,
}

impl ReleaseAccess {
    /// Whether this covers a version of the app, judged by the system clock.
    pub fn covers(&self, version: &str) -> bool {
        self.covers_at(version, SystemTime::now())
    }

    /// Whether this covers a version of the app at `now`: the same rule Moonbase
    /// applies when validating.
    ///
    /// A version is covered when its major is at most `up_to_major`, or while
    /// `updates_until` has not passed, or, once it has, when it is at most
    /// `up_to_version` (every version, when nothing was published before updates
    /// ended). Without `updates_until` or `up_to_major` every version is covered.
    /// A version that can't be read (see
    /// [`parse_app_version`](crate::version::parse_app_version)) is not checked, so
    /// it counts as covered.
    pub fn covers_at(&self, version: &str, now: SystemTime) -> bool {
        // As Moonbase reads a license token: without an end to updates or a highest
        // major, it covers every release.
        if self.updates_until.is_none() && self.up_to_major.is_none() {
            return true;
        }
        let Some(app_version) = parse_app_version(version) else {
            return true;
        };
        if self.up_to_major.is_some_and(|major| app_version.major <= major) {
            return true;
        }
        let Some(until) = self.updates_until else {
            return false;
        };
        if now <= until {
            return true;
        }
        // With nothing published by the end of the updates there is no version to
        // judge by, and locking out someone who paid is worse than letting them run
        // whatever came after.
        match self.up_to_version.as_deref().and_then(parse_app_version) {
            Some(last_covered) => app_version <= last_covered,
            None => true,
        }
    }

    /// Read leniently from the license file. A license is judged by its token, so
    /// a field that can't be read is left out rather than costing the stored
    /// license.
    pub(crate) fn from_stored(value: &Value) -> Self {
        Self {
            updates_until: value
                .get("updatesUntil")
                .and_then(Value::as_str)
                .and_then(|text| parse_iso8601_utc(text).ok()),
            up_to_major: value.get("upToMajor").and_then(Value::as_u64),
            up_to_version: value.get("upToVersion").and_then(Value::as_str).map(str::to_owned),
        }
    }
}

impl<'de> Deserialize<'de> for ReleaseAccess {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_stored(&Value::deserialize(deserializer)?))
    }
}

/// The license file's `releaseAccess`, or `None` when it is absent or not an object.
pub(crate) fn lenient_release_access<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ReleaseAccess>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value.is_object().then(|| ReleaseAccess::from_stored(&value)))
}

/// A license that is valid, but doesn't cover the version of the app that runs: it
/// covers only the releases published while its updates lasted, or versions up to
/// some major. Carried by [`Error::ReleaseNotCovered`](crate::Error::ReleaseNotCovered).
///
/// Not a reason to delete the stored license: it still covers the versions it did,
/// so the customer can go back to one of them, or upgrade and keep going. Say what
/// it covers ([`release_access`](Self::release_access)) and offer the upgrade
/// ([`upgrade_url`](Self::upgrade_url)).
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseNotCovered {
    message: String,
    license: Option<License>,
    app_version: String,
    release_access: Option<ReleaseAccess>,
    upgrade_url: Option<String>,
}

impl ReleaseNotCovered {
    pub(crate) fn new(
        message: String,
        license: Option<License>,
        app_version: String,
        release_access: Option<ReleaseAccess>,
        upgrade_url: Option<String>,
    ) -> Self {
        let release_access =
            release_access.or_else(|| license.as_ref().and_then(|license| license.release_access.clone()));
        Self { message, license, app_version, release_access, upgrade_url }
    }

    /// Why, in Moonbase's words when it refused, else the SDK's.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The license, still good for the versions it covers. When Moonbase refused it
    /// on validation, the copy it issued along with the refusal: store it in place
    /// of the old one, as [`Licensing::validate_token_online`](crate::Licensing::validate_token_online)
    /// does. `None` when a download was refused.
    pub fn license(&self) -> Option<&License> {
        self.license.as_ref()
    }

    /// The version that isn't covered, as `MAJOR.MINOR.PATCH`: the app's, as
    /// Moonbase read it. Empty when a download was refused, which names none.
    pub fn app_version(&self) -> &str {
        &self.app_version
    }

    /// What the license covers: as Moonbase described it when it refused, else
    /// what the license says.
    pub fn release_access(&self) -> Option<&ReleaseAccess> {
        self.release_access.as_ref()
    }

    /// Where the customer buys the upgrade or renewal. Only Moonbase knows it, so
    /// it is `None` when the device refused, and when the store sells none.
    pub fn upgrade_url(&self) -> Option<&str> {
        self.upgrade_url.as_deref()
    }
}

impl fmt::Display for ReleaseNotCovered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
