//! The customer inventory endpoints (`/api/customer/inventory/...`): a release's
//! notes, and an authenticated installer download for the running platform.
//! Authenticates with the license token through the
//! `Authorization: LicenseToken <jwt>` scheme.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use crate::client::{
    build_request, default_headers, endpoint_path, error_for_inventory_problem, problem_string, release_access_from,
};
use crate::error::{ApiError, Error, Result};
use crate::http::{HttpTransport, Method};
use crate::release_access::ReleaseAccess;
use crate::types::{License, LicensingOptions};
use crate::url::{append_query, url_decode, url_encode};

/// A release's plain-text notes, plus enough to know an installer exists and who
/// may download it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseInfo {
    /// The release version, as the server echoes it back.
    pub version: String,
    /// Release notes; empty when there are none.
    pub description: String,
    pub has_downloads: bool,
    /// Downloading requires an authenticated customer.
    pub downloads_need_user: bool,
    /// Downloading requires owning or subscribing to the product.
    pub downloads_need_ownership: bool,
    /// Whether the customer the license token belongs to may download this
    /// release, as Moonbase decides it with every rule the release has: ownership,
    /// customer groups, and the versions their licenses cover. `None` from an older
    /// Moonbase that doesn't say, when [`can_download`] works it out from the flags
    /// above instead.
    pub downloads_allowed: Option<bool>,
    /// Whether the customer's licenses cover this release. False only when
    /// Moonbase says they don't, for a license that covers only some versions;
    /// downloading it is then refused with [`Error::ReleaseNotCovered`].
    pub release_covered: bool,
    /// What the customer's licenses cover, when that isn't every release.
    pub release_access: Option<ReleaseAccess>,
    /// Where to buy the upgrade or renewal, while the store sells one.
    pub upgrade_url: Option<String>,
}

impl Default for ReleaseInfo {
    fn default() -> Self {
        Self {
            version: String::new(),
            description: String::new(),
            has_downloads: false,
            downloads_need_user: false,
            downloads_need_ownership: false,
            downloads_allowed: None,
            release_covered: true,
            release_access: None,
            upgrade_url: None,
        }
    }
}

/// A ready-to-fetch installer URL. The link is presigned and short-lived (the
/// backend expires it after about 15 minutes).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadTarget {
    pub url: String,
    /// A best-effort file name parsed from the URL. Fall back to your own when it
    /// is empty.
    pub filename: String,
}

const NIL_GUID: &str = "00000000-0000-0000-0000-000000000000";

/// May `license` download the installer for a release?
///
/// Moonbase says so itself ([`ReleaseInfo::downloads_allowed`]), with every rule
/// the release has. Without that answer, from an older Moonbase, this works out
/// the common rules as Moonbase would: a full license owns the product and a trial
/// does not; an authenticated customer has a real user id, while an anonymous
/// trial carries the nil GUID; and an owner gets only a release their licenses
/// cover. A rule beyond those, such as a release limited to a group of customers,
/// is then left to the download itself to refuse.
pub fn can_download(license: &License, info: &ReleaseInfo) -> bool {
    if !info.release_covered {
        return false;
    }
    if let Some(allowed) = info.downloads_allowed {
        return allowed;
    }
    let owns = !license.trial;
    let has_user = !license.issued_to.id.is_empty() && license.issued_to.id != NIL_GUID;
    !(info.downloads_need_ownership && !owns) && !(info.downloads_need_user && !has_user)
}

/// Best-effort installer file name from a presigned URL's content-disposition (S3
/// puts it in the `response-content-disposition` query parameter).
pub(crate) fn filename_from_download_url(url: &str) -> String {
    let decoded = url_decode(url);
    let Some(at) = decoded.find("filename") else {
        return String::new();
    };
    let mut rest = &decoded[at + "filename".len()..];
    // RFC 5987 "filename*=".
    rest = rest.strip_prefix('*').unwrap_or(rest);
    let Some(equals) = rest.find('=') else {
        return String::new();
    };
    rest = &rest[equals + 1..];
    // Drop an RFC 5987 charset prefix like "UTF-8''".
    if let Some(quote) = rest.find("''").filter(|&quote| quote < 12) {
        rest = &rest[quote + 2..];
    }

    let mut name = String::new();
    let mut quoted = false;
    for character in rest.chars() {
        if character == '"' {
            if quoted {
                break;
            }
            quoted = true;
            continue;
        }
        if !quoted && matches!(character, ';' | '&' | ' ') {
            break;
        }
        name.push(character);
    }
    name
}

/// Client for the inventory endpoints.
#[derive(Clone)]
pub struct InventoryClient {
    options: Arc<LicensingOptions>,
    transport: Arc<dyn HttpTransport>,
}

impl InventoryClient {
    pub fn new(options: LicensingOptions, transport: Arc<dyn HttpTransport>) -> Self {
        Self { options: Arc::new(options), transport }
    }

    fn product_path(&self) -> String {
        endpoint_path(
            &self.options,
            &format!("/api/customer/inventory/products/{}", url_encode(&self.options.product_id)),
        )
    }

    fn get(&self, url: String, license_token: &str) -> Result<Value> {
        let mut headers = default_headers(&self.options, None);
        if !license_token.is_empty() {
            headers.insert("Authorization".into(), format!("LicenseToken {license_token}"));
        }
        let request = build_request(&self.options, Method::Get, url, headers, String::new());
        let response = self.transport.send(&request)?;
        if !response.is_success() {
            return Err(error_for_inventory_problem(&response));
        }
        serde_json::from_str(&response.body).map_err(|error| {
            Error::from(ApiError::new(response.status, format!("Could not parse inventory response: {error}")))
        })
    }

    /// The notes for a release. An empty `version` asks for the current one.
    ///
    /// Every failure is an [`ApiError`] with the HTTP status. These endpoints never
    /// refuse the license itself: a token they cannot use just makes the request
    /// anonymous, so a private product then answers 404. The result says whether
    /// Moonbase lets the customer download the release (`downloads_allowed`), and,
    /// for a customer whose licenses don't cover every release, whether they cover
    /// this one (`release_covered`) and where to upgrade.
    pub fn get_release(&self, version: &str, license_token: &str) -> Result<ReleaseInfo> {
        let mut query = BTreeMap::from([("includeManifests".to_owned(), "false".to_owned())]);
        if !version.is_empty() {
            query.insert("version".into(), version.into());
        }
        let json = self.get(append_query(&self.product_path(), &query), license_token)?;

        let text = |key: &str| json.get(key).and_then(Value::as_str).map(str::to_owned);
        let flag = |key: &str| json.get(key).and_then(Value::as_bool).unwrap_or(false);
        // Present only when the customer's licenses don't cover every release.
        let access = json.get("releaseAccess").filter(|access| access.is_object());
        Ok(ReleaseInfo {
            version: text("version").or_else(|| text("currentVersion")).unwrap_or_default(),
            description: text("releaseDescription").unwrap_or_default(),
            has_downloads: json
                .get("downloads")
                .and_then(Value::as_array)
                .is_some_and(|downloads| !downloads.is_empty()),
            downloads_need_user: flag("downloadsNeedsUser"),
            downloads_need_ownership: flag("downloadsNeedsOwnership"),
            downloads_allowed: json.get("downloadsAllowed").and_then(Value::as_bool),
            release_covered: access
                .and_then(|access| access.get("coversRelease"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
            release_access: access.and_then(|_| release_access_from(&json)),
            upgrade_url: access.and_then(|access| problem_string(access, "upgradeUrl")),
        })
    }

    /// Resolve the latest installer for a platform (the backend's platform name,
    /// e.g. `"Mac"` or `"Windows"`; see [`Platform::as_str`](crate::Platform::as_str))
    /// to a presigned URL.
    ///
    /// Fails with an [`ApiError`]: 404 when there is no installer for the platform,
    /// 403 when this license may not download it. Fails with
    /// [`Error::ReleaseNotCovered`], with the upgrade link, when the customer's
    /// licenses don't cover the release.
    pub fn get_download_url(&self, platform_name: &str, license_token: &str) -> Result<DownloadTarget> {
        let path = format!("{}/download/{}/latest", self.product_path(), url_encode(platform_name));
        let query = BTreeMap::from([("redirect".to_owned(), "false".to_owned())]);
        let json = self.get(append_query(&path, &query), license_token)?;
        let url = json
            .get("location")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::from(ApiError::new(200, "Could not parse download response: missing location")))?
            .to_owned();
        Ok(DownloadTarget { filename: filename_from_download_url(&url), url })
    }
}

impl std::fmt::Debug for InventoryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InventoryClient").field("endpoint", &self.options.endpoint).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::filename_from_download_url;

    #[test]
    fn reads_the_filename_out_of_a_presigned_url() {
        assert_eq!(
            filename_from_download_url(
                "https://s3/x?response-content-disposition=attachment%3B%20filename%3D%22Solstice%201.0.dmg%22&X-Amz=1"
            ),
            "Solstice 1.0.dmg"
        );
        assert_eq!(
            filename_from_download_url(
                "https://s3/x?response-content-disposition=attachment%3B+filename*%3DUTF-8''Solstice.pkg&a=b"
            ),
            "Solstice.pkg"
        );
        assert_eq!(filename_from_download_url("https://s3/x?a=b"), "");
    }
}
