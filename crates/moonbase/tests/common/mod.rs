#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use moonbase::{
    HttpRequest, HttpResponse, HttpTransport, LicenseStore, Licensing, LicensingOptions, MemoryLicenseStore, Platform,
    StaticDeviceIdResolver,
};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};

pub const PUBLIC_KEY: &str = include_str!("../fixtures/signing-public.pem");
pub const PUBLIC_KEY_PKCS1: &str = include_str!("../fixtures/signing-public-pkcs1.pem");
pub const OTHER_PUBLIC_KEY: &str = include_str!("../fixtures/other-public.pem");
const SIGNING_KEY: &[u8] = include_bytes!("../fixtures/signing-private.pk8");
const OTHER_KEY: &[u8] = include_bytes!("../fixtures/other-private.pk8");

pub const DEVICE_ID: &str = "device-id";

/// Which key signs a test token.
#[derive(Clone, Copy)]
pub enum Signer {
    Moonbase,
    SomeoneElse,
}

pub fn now_seconds() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

pub fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn make_token_with(signer: Signer, payload: &Value, header: &Value) -> String {
    let key = RsaKeyPair::from_pkcs8(match signer {
        Signer::Moonbase => SIGNING_KEY,
        Signer::SomeoneElse => OTHER_KEY,
    })
    .expect("fixture keys are valid PKCS#8");
    let signing_input = format!("{}.{}", b64url(header.to_string().as_bytes()), b64url(payload.to_string().as_bytes()));
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), signing_input.as_bytes(), &mut signature).expect("signing works");
    format!("{signing_input}.{}", b64url(&signature))
}

pub fn make_token(payload: &Value) -> String {
    make_token_with(Signer::Moonbase, payload, &json!({"alg": "RS256", "typ": "JWT"}))
}

/// The claims a Moonbase license token carries, valid for an hour and validated
/// half a minute ago.
pub fn claims_for(device_id: &str) -> Value {
    let now = now_seconds();
    json!({
        "iss": "tenant-1",
        "aud": "demo-app",
        "iat": now - 60,
        "nbf": now - 60,
        "exp": now + 3600,
        "id": "activation-123",
        "l:id": "license-123",
        "trial": false,
        "method": "Online",
        "sig": device_id,
        "validated": now - 30,
        "p:id": "demo-app",
        "p:name": "Demo Product",
        "p:rel": "1.2.3",
        "sp:owned": "demo-app-pro,demo-app-extra",
        "s:id": "subscription-123",
        "u:id": "user-123",
        "u:name": "Jane Developer",
        "u:email": "jane@example.com",
        "p:properties": {"tier": "pro"},
        "u:properties": {"company": "Acme"},
        "l:properties": {"seats": 3},
    })
}

pub fn claims() -> Value {
    claims_for(DEVICE_ID)
}

pub fn with(mut claims: Value, key: &str, value: Value) -> Value {
    claims[key] = value;
    claims
}

pub fn without(mut claims: Value, key: &str) -> Value {
    claims.as_object_mut().unwrap().remove(key);
    claims
}

/// Records every request and answers from a queue. An empty queue fails the
/// request the way an unreachable server would.
#[derive(Default)]
pub struct RecordingTransport {
    pub requests: Mutex<Vec<HttpRequest>>,
    pub responses: Mutex<VecDeque<HttpResponse>>,
}

impl RecordingTransport {
    pub fn new(responses: impl IntoIterator<Item = HttpResponse>) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::default(), responses: Mutex::new(responses.into_iter().collect()) })
    }

    pub fn push(&self, response: HttpResponse) {
        self.responses.lock().unwrap().push_back(response);
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl HttpTransport for RecordingTransport {
    fn send(&self, request: &HttpRequest) -> moonbase::Result<HttpResponse> {
        self.requests.lock().unwrap().push(request.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| moonbase::ApiError::unreachable("No mock HTTP response queued").into())
    }
}

pub fn options() -> LicensingOptions {
    let mut options = LicensingOptions::new("https://demo.moonbase.sh/", "demo-app", PUBLIC_KEY)
        .account_id("tenant-1")
        .platform(Platform::Mac)
        .application_version("1.2.3")
        .client_info("tauri-plugin-moonbase/9.9 (Tauri 2; TestOS)")
        .metadata("channel", "test")
        .http_timeouts(Duration::from_millis(1234), Duration::from_millis(5678));
    options.online_validation_min_interval = Duration::from_secs(5 * 60);
    options
}

pub struct Fixture {
    pub licensing: Licensing,
    pub transport: Arc<RecordingTransport>,
    pub store: Arc<dyn LicenseStore>,
}

pub fn fixture_with(
    options: LicensingOptions,
    responses: impl IntoIterator<Item = HttpResponse>,
    store: Arc<dyn LicenseStore>,
    device_name: &str,
    device_id: &str,
) -> Fixture {
    let transport = RecordingTransport::new(responses);
    let licensing = Licensing::builder(options)
        .store(store.clone())
        .device_id_resolver(Arc::new(StaticDeviceIdResolver::new(device_name, device_id)))
        .transport(transport.clone())
        .build()
        .expect("the fixture configuration is valid");
    Fixture { licensing, transport, store }
}

pub fn fixture(responses: impl IntoIterator<Item = HttpResponse>) -> Fixture {
    fixture_with(options(), responses, Arc::new(MemoryLicenseStore::new()), "Test Device", DEVICE_ID)
}

pub fn ok(body: impl Into<String>) -> HttpResponse {
    HttpResponse::new(200, body)
}

pub fn activation_response() -> HttpResponse {
    ok(
        r#"{"id":"request-123","request":"https://demo.moonbase.sh/api/client/activations/request-123?format=JWT","browser":"https://demo.moonbase.sh/activate?token=request-123"}"#,
    )
}

pub fn poll_request() -> moonbase::ActivationRequest {
    moonbase::ActivationRequest {
        id: "request-123".into(),
        request_url: "https://demo.moonbase.sh/api/client/activations/request-123?format=JWT".into(),
        browser_url: String::new(),
        method: moonbase::ActivationMethod::Online,
    }
}

/// A `YYYY-MM-DDTHH:MM:SSZ` time, for comparing what the SDK read.
pub fn iso(text: &str) -> SystemTime {
    let number = |range: std::ops::Range<usize>| text[range].parse::<i64>().expect("a well-formed test time");
    let (year, month, day) = (number(0..4), number(5..7), number(8..10));
    // Howard Hinnant's days_from_civil.
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + number(11..13) * 3_600 + number(14..16) * 60 + number(17..19);
    if seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(seconds as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs())
    }
}

pub fn epoch_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_secs() as i64,
        Err(before) => -(before.duration().as_secs() as i64),
    }
}
