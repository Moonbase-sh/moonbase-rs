//! The client API, and every failure shape the API and what sits in front of it
//! can produce.

mod common;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use common::*;
use moonbase::{
    ActivationMethod, Error, ErrorType, HttpResponse, InventoryClient, LicensingOptions, MemoryLicenseStore, Method,
};
use serde_json::{Value, json};

// ASP.NET's NotFound() as the API serves it.
const API_NOT_FOUND: &str = r#"{"type":"https://tools.ietf.org/html/rfc9110#section-15.5.5","title":"Not Found","status":404,"traceId":"00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"}"#;

// What the API answers every request to a closed store with.
const STORE_CLOSED: &str = r#"{"type":"https://tools.ietf.org/html/rfc9110#section-15.5.11","title":"Store closed","status":410,"detail":"This store has closed.","errorType":"StoreClosed"}"#;

fn status(code: u16, body: &str) -> HttpResponse {
    HttpResponse::new(code, body)
}

fn api_status(error: Error) -> u16 {
    match error {
        Error::Api(api) => api.status_code(),
        other => panic!("expected an API error, got {other:?}"),
    }
}

fn body_json(body: &str) -> Value {
    serde_json::from_str(body).unwrap()
}

#[test]
fn request_activation_posts_device_information_and_parses_the_response() {
    let f = fixture([activation_response()]);
    let response = f.licensing.client().request_activation(ActivationMethod::Online).unwrap();

    assert_eq!(response.id, "request-123");
    assert!(response.request_url.contains("request-123"));
    assert!(response.browser_url.contains("activate"));
    assert_eq!(response.method, ActivationMethod::Online);

    let requests = f.transport.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, Method::Post);
    assert!(request.url.starts_with("https://demo.moonbase.sh/api/client/activations/demo-app/request?"));
    for part in ["format=JWT", "method=Online", "platform=Mac", "appVersion=1.2.3", "meta%5Bchannel%5D=test"] {
        assert!(request.url.contains(part), "{} lacks {part}", request.url);
    }
    assert_eq!(request.headers["Content-Type"], "application/json");
    assert_eq!(request.headers["x-mb-client"], "moonbase-rs");
    assert!(request.headers["User-Agent"].starts_with(&format!("moonbase-rs/{} ", moonbase::VERSION)));
    assert!(request.headers["User-Agent"].ends_with("tauri-plugin-moonbase/9.9 (Tauri 2; TestOS)"));
    assert_eq!(request.connect_timeout, Duration::from_millis(1234));
    assert_eq!(request.request_timeout, Duration::from_millis(5678));

    let body = body_json(&request.body);
    assert_eq!(body["deviceName"], "Test Device");
    assert_eq!(body["deviceSignature"], "device-id");
}

#[test]
fn client_info_reads_outermost_last_and_cannot_inject_headers() {
    let user_agent = |client_info: &str| {
        let f = fixture_with(
            options().client_info(client_info),
            [activation_response()],
            Arc::new(MemoryLicenseStore::new()),
            "Test Device",
            DEVICE_ID,
        );
        f.licensing.request_activation(ActivationMethod::Online).unwrap();
        f.transport.requests()[0].headers["User-Agent"].clone()
    };
    let base = format!("moonbase-rs/{}", moonbase::VERSION);

    let layered = user_agent("tauri-plugin-moonbase/1.0 (Tauri 2) MyApp/4.1.0");
    assert!(layered.find("tauri-plugin-moonbase/1.0") < layered.find("MyApp/4.1.0"));

    let injected = user_agent("MyApp/4.1.0\r\nX-Injected: 1");
    assert!(!injected.contains(['\r', '\n']));
    assert!(injected.ends_with("MyApp/4.1.0 X-Injected: 1"));

    assert!(user_agent("MyApp/4.1.0\0hidden").ends_with("MyApp/4.1.0 hidden"));
    assert_eq!(user_agent("  \r\n\t "), base);
    assert!(user_agent("  MyApp/4.1.0   Wrapper/2.0  ").ends_with(" MyApp/4.1.0 Wrapper/2.0"));

    let capped = user_agent(&"x".repeat(1000));
    assert!(capped.starts_with(&base));
    assert!(capped.len() < base.len() + 1 + 257);
}

#[test]
fn request_activation_asks_for_an_offline_license() {
    let f = fixture([activation_response()]);
    let response = f.licensing.request_activation(ActivationMethod::Offline).unwrap();

    // The response carries no method, so the client records what it asked for.
    assert_eq!(response.method, ActivationMethod::Offline);
    let request = &f.transport.requests()[0];
    assert!(request.url.contains("method=Offline"));
    assert!(!request.url.contains("method=Online"));
    assert_eq!(body_json(&request.body)["deviceSignature"], "device-id");
}

#[test]
fn request_activation_reports_a_product_that_disallows_offline_activations() {
    let f = fixture([status(400, r#"{"title":"Not allowed","detail":"Product does not allow offline activations"}"#)]);
    let error = f.licensing.request_activation(ActivationMethod::Offline).unwrap_err();
    assert!(
        matches!(&error, Error::LicenseInvalid(message) if message == "Product does not allow offline activations")
    );
}

#[test]
fn request_activation_fails_retryably_for_server_errors() {
    let f = fixture([status(500, r#"{"title":"Failure","detail":"Backend failed"}"#)]);
    let error = f.licensing.request_activation(ActivationMethod::Online).unwrap_err();
    assert_eq!(api_status(error), 500);
}

#[test]
fn request_activation_stands_in_for_a_blank_device_name() {
    let f = fixture_with(options(), [activation_response()], Arc::new(MemoryLicenseStore::new()), "  ", DEVICE_ID);
    f.licensing.request_activation(ActivationMethod::Online).unwrap();
    let body = body_json(&f.transport.requests()[0].body);
    assert_eq!(body["deviceName"], "Unknown device");
    assert_eq!(body["deviceSignature"], "device-id");
}

#[test]
fn request_activation_refuses_an_empty_device_id_without_contacting_the_api() {
    let f = fixture_with(options(), [], Arc::new(MemoryLicenseStore::new()), "Test Device", " ");
    let error = f.licensing.request_activation(ActivationMethod::Online).unwrap_err();
    assert_eq!(error.error_type(), ErrorType::ConfigurationError);
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn a_validation_400_quotes_its_first_field_error() {
    let f = fixture([status(
        400,
        r#"{"type":"https://tools.ietf.org/html/rfc9110#section-15.5.1","title":"One or more validation errors occurred.","status":400,"errors":{"deviceName":["The DeviceName field is required."]}}"#,
    )]);
    let error = f.licensing.request_activation(ActivationMethod::Online).unwrap_err();
    assert!(
        matches!(&error, Error::LicenseInvalid(message) if message == "deviceName: The DeviceName field is required.")
    );
}

#[test]
fn the_poll_returns_none_while_pending_or_missing() {
    // A fresh client per status: a second poll would be held back by the floor.
    for code in [204, 404] {
        let f = fixture([status(code, "")]);
        assert!(f.licensing.get_requested_activation(&poll_request()).unwrap().is_none());
        let requests = f.transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, Method::Get);
        assert_eq!(requests[0].connect_timeout, Duration::from_millis(1234));
        assert_eq!(requests[0].request_timeout, Duration::from_millis(5678));
    }
}

#[test]
fn the_poll_keeps_waiting_through_a_404_whoever_sent_it() {
    for body in [API_NOT_FOUND, "", "<html>Not Found</html>"] {
        let f = fixture([status(404, body)]);
        assert!(f.licensing.get_requested_activation(&poll_request()).unwrap().is_none());
    }
}

#[test]
fn clones_of_a_client_share_the_poll_floor() {
    let f = fixture([status(204, "")]);
    let copy = f.licensing.client().clone();
    assert!(f.licensing.client().get_requested_activation(&poll_request()).unwrap().is_none());
    assert!(copy.get_requested_activation(&poll_request()).unwrap().is_none());
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn the_poll_validates_a_fulfilled_token() {
    let f = fixture([ok(make_token(&claims()))]);
    let license = f.licensing.get_requested_activation(&poll_request()).unwrap().unwrap();
    assert_eq!(license.id, "license-123");
}

#[test]
fn a_400_from_the_poll_ends_the_request_with_the_servers_reason() {
    let cases = [
        (
            r#"{"title":"Invalid state","detail":"Activation request was cancelled"}"#,
            "Activation request can no longer be completed: Activation request was cancelled",
        ),
        // What the API answers once a request has waited an hour unfulfilled. Not
        // an expired license, whatever the text says.
        (
            r#"{"title":"Invalid state","status":400,"detail":"This activation request has expired. Start the activation again from the app."}"#,
            "Activation request can no longer be completed: This activation request has expired. Start the \
             activation again from the app.",
        ),
        ("", "Activation request can no longer be completed (HTTP 400)"),
    ];
    for (body, expected) in cases {
        let f = fixture([status(400, body)]);
        let error = f.licensing.get_requested_activation(&poll_request()).unwrap_err();
        assert!(matches!(&error, Error::ActivationRequestEnded(message) if message == expected), "{error:?}");
        assert_eq!(error.error_type(), ErrorType::ActivationRequestEnded);
        assert!(!error.is_license_invalid());
    }
}

#[test]
fn an_explicit_license_expired_from_the_poll_is_an_expired_license() {
    let f = fixture([status(400, r#"{"errorType":"LicenseExpired","detail":"The license has expired"}"#)]);
    let error = f.licensing.get_requested_activation(&poll_request()).unwrap_err();
    assert!(matches!(error, Error::LicenseExpired(_)));
}

#[test]
fn server_errors_from_the_poll_stay_retryable() {
    let f = fixture([status(503, r#"{"title":"Service Unavailable"}"#)]);
    assert_eq!(api_status(f.licensing.get_requested_activation(&poll_request()).unwrap_err()), 503);
}

#[test]
fn a_rate_limited_poll_is_retryable_and_backs_off() {
    let f = fixture([status(429, "").with_header("Retry-After", "60")]);
    let request = poll_request();
    let Error::Api(error) = f.licensing.get_requested_activation(&request).unwrap_err() else {
        panic!("expected an API error");
    };
    assert_eq!(error.status_code(), 429);
    assert_eq!(error.retry_after(), Some(Duration::from_secs(60)));

    // Past the per-request floor, but not the Retry-After: answered "not yet"
    // without going out. Had it, the empty queue would have failed it.
    thread::sleep(Duration::from_millis(1050));
    assert!(f.licensing.get_requested_activation(&request).unwrap().is_none());
    assert_eq!(f.transport.request_count(), 1);
}

#[test]
fn a_rate_limited_request_carries_the_servers_retry_after() {
    for header in ["Retry-After", "retry-after"] {
        let f = fixture([status(429, "").with_header(header, "60")]);
        let Error::Api(error) = f.licensing.request_activation(ActivationMethod::Online).unwrap_err() else {
            panic!("expected an API error");
        };
        assert_eq!(error.status_code(), 429);
        assert_eq!(error.retry_after(), Some(Duration::from_secs(60)));
    }
    for value in ["Wed, 21 Oct 2026 07:28:00 GMT", "-1", "soon", "99999999999999999999"] {
        let f = fixture([status(429, "").with_header("Retry-After", value)]);
        let Error::Api(error) = f.licensing.request_activation(ActivationMethod::Online).unwrap_err() else {
            panic!("expected an API error");
        };
        assert_eq!(error.retry_after(), None, "{value}");
    }
}

#[test]
fn validate_online_posts_the_token_and_validates_the_refreshed_one() {
    let refreshed = make_token(&claims());
    let f = fixture([ok(refreshed.clone())]);
    let license = f.licensing.client().validate_token_online("original.jwt.token").unwrap();
    assert_eq!(license.id, "license-123");
    assert_eq!(license.token, refreshed);

    let request = &f.transport.requests()[0];
    assert_eq!(request.method, Method::Post);
    assert!(request.url.starts_with("https://demo.moonbase.sh/api/client/licenses/demo-app/validate?"));
    // "method" is only accepted on the request endpoint.
    assert!(!request.url.contains("method="));
    for part in ["format=JWT", "platform=Mac", "appVersion=1.2.3", "meta%5Bchannel%5D=test"] {
        assert!(request.url.contains(part));
    }
    assert_eq!(request.headers["Content-Type"], "text/plain");
    assert_eq!(request.body, "original.jwt.token");
}

#[test]
fn validate_online_revalidates_the_refreshed_token_locally() {
    let f = fixture([ok(make_token(&claims_for("other-device")))]);
    let error = f.licensing.client().validate_token_online("token").unwrap_err();
    assert!(matches!(error, Error::DeviceMismatch(_)));
    assert!(error.is_license_invalid());
}

#[test]
fn a_well_formed_but_wrongly_signed_token_is_still_an_invalid_license() {
    let f = fixture([ok(make_token_with(Signer::SomeoneElse, &claims(), &json!({"alg": "RS256", "typ": "JWT"})))]);
    assert!(matches!(f.licensing.client().validate_token_online("token").unwrap_err(), Error::LicenseInvalid(_)));
}

#[test]
fn license_verdicts_map_to_their_errors() {
    type IsExpected = fn(&Error) -> bool;
    let cases: [(&str, IsExpected, &str); 6] = [
        (
            r#"{"errorType":"LicenseExpired","detail":"The license has expired"}"#,
            |e| matches!(e, Error::LicenseExpired(_)),
            "The license has expired",
        ),
        (
            r#"{"title":"Invalid state","detail":"License has expired","status":400,"errorType":"LicenseExpired"}"#,
            |e| matches!(e, Error::LicenseExpired(_)),
            "License has expired",
        ),
        (
            r#"{"title":"Invalid","detail":"Token is not valid"}"#,
            |e| matches!(e, Error::LicenseInvalid(_)),
            "Token is not valid",
        ),
        // DomainErrorCode 4 is LicenseInvalid; the API sends names, never numbers.
        (
            r#"{"title":"Invalid","detail":"License is not valid","errorType":4}"#,
            |e| matches!(e, Error::LicenseInvalid(_)),
            "License is not valid",
        ),
        (
            r#"{"title":"Invalid state","detail":"License has been revoked","status":400,"errorType":"LicenseRevoked"}"#,
            |e| matches!(e, Error::LicenseInvalid(_)),
            "License has been revoked",
        ),
        (
            r#"{"title":"Not allowed","detail":"License has been revoked","status":400,"errorType":"LicenseActivationRevoked"}"#,
            |e| matches!(e, Error::LicenseInvalid(_)),
            "License has been revoked",
        ),
    ];
    for (body, is_expected, message) in cases {
        let f = fixture([status(400, body)]);
        let error = f.licensing.client().validate_token_online("token").unwrap_err();
        assert!(is_expected(&error), "{body} gave {error:?}");
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn a_403_or_404_on_validate_or_revoke_stays_retryable_even_as_problem_details() {
    let f = fixture([status(404, API_NOT_FOUND)]);
    assert_eq!(api_status(f.licensing.client().validate_token_online("token").unwrap_err()), 404);

    let f = fixture([status(404, API_NOT_FOUND)]);
    assert_eq!(api_status(f.licensing.client().revoke_activation("token").unwrap_err()), 404);

    let f = fixture([status(403, r#"{"title":"Not allowed","detail":"User does not own license","status":403}"#)]);
    let error = f.licensing.client().revoke_activation("token").unwrap_err();
    assert_eq!(error.to_string(), "User does not own license");
    assert_eq!(api_status(error), 403);
}

#[test]
fn a_404_the_api_did_not_write_stays_retryable() {
    for body in ["", "<html><body>Not Found</body></html>"] {
        let f = fixture([status(404, body)]);
        let error = f.licensing.client().validate_token_online("token").unwrap_err();
        assert_eq!(error.to_string(), "Moonbase API request failed with HTTP 404");
        assert_eq!(api_status(error), 404);
    }
}

#[test]
fn a_proxy_block_page_stays_retryable() {
    let f = fixture([status(403, "<!DOCTYPE html><html><body>Blocked by policy</body></html>")]);
    assert_eq!(api_status(f.licensing.client().validate_token_online("token").unwrap_err()), 403);
}

#[test]
fn a_200_that_is_not_a_token_is_retryable_not_an_invalid_license() {
    let portal = "<html><head><title>Hotel Wi-Fi</title></head><body>Sign in</body></html>";

    let f = fixture([ok(portal)]);
    assert_eq!(api_status(f.licensing.client().validate_token_online("token").unwrap_err()), 200);

    let f = fixture([ok(portal)]);
    assert_eq!(api_status(f.licensing.get_requested_activation(&poll_request()).unwrap_err()), 200);

    let f = fixture([ok(portal)]);
    assert_eq!(api_status(f.licensing.client().revoke_activation("token").unwrap_err()), 200);
}

#[test]
fn a_bare_string_400_surfaces_its_message() {
    for body in [r#""Trials can not be revoked""#, "Trials can not be revoked"] {
        let f = fixture([status(400, body)]);
        let error = f.licensing.client().revoke_activation("token").unwrap_err();
        assert!(matches!(&error, Error::LicenseInvalid(message) if message == "Trials can not be revoked"));
    }
}

#[test]
fn gateway_and_server_failures_stay_retryable_and_say_what_they_can() {
    let f = fixture([status(504, r#"{"message":"Endpoint request timed out"}"#)]);
    let error = f.licensing.client().validate_token_online("token").unwrap_err();
    assert_eq!(error.to_string(), "Endpoint request timed out");
    assert_eq!(api_status(error), 504);

    let f = fixture([status(500, "")]);
    let error = f.licensing.client().validate_token_online("token").unwrap_err();
    assert_eq!(error.to_string(), "Moonbase API request failed with HTTP 500");

    // Only a 400 can say a license expired.
    let f = fixture([status(503, "Upstream session expired")]);
    assert_eq!(api_status(f.licensing.client().validate_token_online("token").unwrap_err()), 503);
}

#[test]
fn revoke_posts_the_token_to_the_revoke_endpoint() {
    let f = fixture([ok("")]);
    f.licensing.client().revoke_activation("original.jwt.token").unwrap();
    let request = &f.transport.requests()[0];
    assert_eq!(request.method, Method::Post);
    assert!(request.url.starts_with("https://demo.moonbase.sh/api/client/licenses/demo-app/revoke?"));
    assert!(!request.url.contains("method="));
    assert_eq!(request.headers["Content-Type"], "text/plain");
    assert_eq!(request.body, "original.jwt.token");
}

#[test]
fn every_endpoint_reports_a_closed_store() {
    let expect_closed = |error: Error| {
        assert!(matches!(&error, Error::StoreClosed(message) if message == "This store has closed."), "{error:?}");
        assert_eq!(error.error_type(), ErrorType::StoreClosed);
        assert!(!error.is_license_rejection());
    };

    expect_closed(
        fixture([status(410, STORE_CLOSED)]).licensing.request_activation(ActivationMethod::Online).unwrap_err(),
    );
    // Must end the poll, not read as a retryable failure to keep polling through.
    expect_closed(
        fixture([status(410, STORE_CLOSED)]).licensing.get_requested_activation(&poll_request()).unwrap_err(),
    );
    expect_closed(fixture([status(410, STORE_CLOSED)]).licensing.client().validate_token_online("t").unwrap_err());
    expect_closed(fixture([status(410, STORE_CLOSED)]).licensing.client().revoke_activation("t").unwrap_err());

    let transport = RecordingTransport::new([status(410, STORE_CLOSED), status(410, STORE_CLOSED)]);
    let inventory = InventoryClient::new(LicensingOptions::new("https://demo.moonbase.sh", "demo-app", ""), transport);
    expect_closed(inventory.get_release("", "token").unwrap_err());
    expect_closed(inventory.get_download_url("Mac", "token").unwrap_err());
}

#[test]
fn a_410_that_is_not_the_apis_store_closed_stays_retryable() {
    let f = fixture([status(410, "<html><body>Gone</body></html>")]);
    assert_eq!(api_status(f.licensing.client().validate_token_online("token").unwrap_err()), 410);
}

#[test]
fn inventory_failures_are_api_errors_with_the_status_never_a_verdict() {
    let options = LicensingOptions::new("https://demo.moonbase.sh", "demo-app", "");

    let transport = RecordingTransport::new([status(
        400,
        r#"{"title":"Invalid version","detail":"Version 'x' is not a valid semver in the form 'MAJOR.MINOR.PATCH'","status":400}"#,
    )]);
    let Error::Api(error) = InventoryClient::new(options.clone(), transport).get_release("x", "token").unwrap_err()
    else {
        panic!("the inventory endpoints never refuse the license");
    };
    assert_eq!(error.status_code(), 400);
    assert_eq!(error.title(), "Invalid version");

    let transport = RecordingTransport::new([status(
        403,
        r#"{"title":"No access","detail":"User does not have access to download this product","status":403}"#,
    )]);
    let error = InventoryClient::new(options, transport).get_download_url("Mac", "token").unwrap_err();
    assert_eq!(error.to_string(), "User does not have access to download this product");
    assert_eq!(api_status(error), 403);
}
