//! Release notes, installer downloads and the semver check behind update prompts.

mod common;

use common::*;
use moonbase::{Error, HttpResponse, InventoryClient, License, Method, ReleaseInfo, can_download};

fn inventory(
    responses: impl IntoIterator<Item = HttpResponse>,
) -> (InventoryClient, std::sync::Arc<RecordingTransport>) {
    let transport = RecordingTransport::new(responses);
    (InventoryClient::new(options(), transport.clone()), transport)
}

#[test]
fn get_release_queries_the_product_endpoint_with_the_license_token() {
    let (client, transport) = inventory([ok(
        r#"{"id":"demo-app","name":"Solstice","version":"2.4.0","releaseDescription":"Line one\nLine two","downloads":[{"name":"Solstice-2.4.0.dmg","platform":"Mac"}]}"#,
    )]);
    let info = client.get_release("2.4.0", "the-token").unwrap();
    assert_eq!(info.version, "2.4.0");
    assert_eq!(info.description, "Line one\nLine two");
    assert!(info.has_downloads);

    let request = &transport.requests()[0];
    assert_eq!(request.method, Method::Get);
    assert!(request.url.starts_with("https://demo.moonbase.sh/api/customer/inventory/products/demo-app?"));
    assert!(request.url.contains("version=2.4.0"));
    assert!(request.url.contains("includeManifests=false"));
    assert_eq!(request.headers["Authorization"], "LicenseToken the-token");
    assert_eq!(request.headers["x-mb-client"], "moonbase-rs");
    assert!(request.headers["User-Agent"].contains("tauri-plugin-moonbase/9.9"));
}

#[test]
fn get_release_parses_the_access_control_flags_and_tolerates_missing_notes() {
    let (client, _) = inventory([ok(
        r#"{"version":"2.4.0","downloadsNeedsUser":true,"downloadsNeedsOwnership":true,"downloads":[{}]}"#,
    )]);
    let info = client.get_release("2.4.0", "tok").unwrap();
    assert!(info.downloads_need_user && info.downloads_need_ownership);

    let (client, transport) = inventory([ok(r#"{"id":"demo-app","name":"Solstice","currentVersion":"2.4.0"}"#)]);
    let info = client.get_release("", "tok").unwrap();
    assert_eq!(info.version, "2.4.0");
    assert!(info.description.is_empty());
    assert!(!info.has_downloads);
    // An empty version omits the parameter.
    assert!(!transport.requests()[0].url.contains("version="));
}

#[test]
fn get_download_url_resolves_the_latest_installer() {
    let (client, transport) = inventory([ok(
        r#"{"location":"https://s3.example.com/bucket/key?X-Amz-Signature=abc&response-content-disposition=attachment%3B%20filename%3D%22Solstice-2.4.0.dmg%22"}"#,
    )]);
    let target = client.get_download_url("Mac", "the-token").unwrap();
    assert!(target.url.contains("s3.example.com"));
    assert_eq!(target.filename, "Solstice-2.4.0.dmg");

    let request = &transport.requests()[0];
    assert!(
        request
            .url
            .starts_with("https://demo.moonbase.sh/api/customer/inventory/products/demo-app/download/Mac/latest?")
    );
    assert!(request.url.contains("redirect=false"));
    assert_eq!(request.headers["Authorization"], "LicenseToken the-token");
}

#[test]
fn get_download_url_fails_retryably_when_there_is_no_installer() {
    let (client, _) = inventory([HttpResponse::new(404, r#"{"title":"No download for the given platform found"}"#)]);
    let error = client.get_download_url("Linux", "tok").unwrap_err();
    assert!(matches!(error, Error::Api(api) if api.status_code() == 404));
}

#[test]
fn can_download_enforces_the_access_control_flags() {
    let license = |trial: bool, user: &str| -> License {
        let fixture = fixture([]);
        let mut license = fixture.licensing.validate_token_local(&make_token(&claims())).unwrap();
        license.trial = trial;
        license.issued_to.id = user.into();
        license
    };
    let full = license(false, "user-1");
    let trial_owned = license(true, "user-1");
    let trial_anonymous = license(true, "00000000-0000-0000-0000-000000000000");

    let anonymous = ReleaseInfo::default();
    let customers = ReleaseInfo { downloads_need_user: true, ..ReleaseInfo::default() };
    let owners = ReleaseInfo { downloads_need_user: true, downloads_need_ownership: true, ..ReleaseInfo::default() };

    assert!(
        can_download(&full, &anonymous)
            && can_download(&trial_owned, &anonymous)
            && can_download(&trial_anonymous, &anonymous)
    );
    assert!(can_download(&full, &customers) && can_download(&trial_owned, &customers));
    assert!(!can_download(&trial_anonymous, &customers));
    assert!(can_download(&full, &owners));
    assert!(!can_download(&trial_owned, &owners) && !can_download(&trial_anonymous, &owners));
}

#[test]
fn the_released_version_claim_drives_update_checks() {
    let license = fixture([]).licensing.validate_token_local(&make_token(&claims())).unwrap();
    let released = license.licensed_product.current_release_version.unwrap();
    assert!(moonbase::version::update_available("1.2.2", &released));
    assert!(!moonbase::version::update_available("1.2.3", &released));
}
