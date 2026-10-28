//! The whole activation flow against a real Moonbase store: request, poll,
//! fulfil as a trial, poll again, and re-validate online.
//!
//! Off by default. Run with `MOONBASE_LIVE_TESTS=1`; it uses the public demo store
//! unless `MOONBASE_ENDPOINT`, `MOONBASE_PRODUCT_ID`, `MOONBASE_PUBLIC_KEY` and
//! `MOONBASE_ACCOUNT_ID` say otherwise.

#![cfg(feature = "ureq-transport")]

use std::collections::BTreeMap;
use std::env;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use moonbase::{
    ActivationMethod, HttpRequest, HttpTransport, Licensing, LicensingOptions, Method, Platform,
    StaticDeviceIdResolver, UreqTransport,
};

const DEMO_PUBLIC_KEY: &str = "-----BEGIN RSA PUBLIC KEY-----
MIIBCgKCAQEAutOqeUiPMgYjAwQ53CyKhJSqojr2bejce0CshQi9Hd8mNZbkoROx
oS56eIzehFSlX4YwHnF47AR1+fPOe7Q33Cgzd6d9xqksiMH7sWK2mADIlB66vZdW
uk3Me0UMB22Biy1RQbSRMivu79MxCofsympoL/5CFjJLd1u37kxjuRWVLjJS84Rr
3L2W7R7Exnno/giC+L/Dv711mjgstmtlAQm5ZINvFvoLA1eFTDs6nlCs3dpJSiq3
fsBUMT9FtudzS5As54jeT/8MB66fJJ0A1LQ/v5CW8ACQYseFSIoOKErD3xU7QLIJ
ERUn++6CVMPvZo67jVbTY+GCXYfW4gGVZQIDAQAB
-----END RSA PUBLIC KEY-----";

fn env_or(name: &str, fallback: &str) -> String {
    env::var(name).ok().filter(|value| !value.is_empty()).unwrap_or_else(|| fallback.to_owned())
}

#[test]
fn live_activation_flow() {
    if !matches!(env::var("MOONBASE_LIVE_TESTS").as_deref(), Ok("1" | "true" | "TRUE")) {
        return;
    }

    let mut options = LicensingOptions::new(
        env_or("MOONBASE_ENDPOINT", "https://demo.moonbase.sh"),
        env_or("MOONBASE_PRODUCT_ID", "demo-app"),
        env_or("MOONBASE_PUBLIC_KEY", DEMO_PUBLIC_KEY),
    )
    .platform(Platform::Unknown)
    .application_version("0.1.0-test")
    .metadata("suite", "moonbase-rs")
    // Every validate_token_online call goes to the API instead of throttling.
    .online_validation_min_interval(Duration::ZERO);
    if let Ok(account) = env::var("MOONBASE_ACCOUNT_ID") {
        options.account_id = Some(account);
    }

    let unique = format!("moonbase-rs-test-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs());
    let transport = Arc::new(UreqTransport::new());
    let licensing = Licensing::builder(options.clone())
        .device_id_resolver(Arc::new(StaticDeviceIdResolver::new("Moonbase Rust Test", unique)))
        .transport(transport.clone())
        .build()
        .unwrap();

    let activation = licensing.request_activation(ActivationMethod::Online).unwrap();
    assert!(!activation.id.is_empty());
    assert!(!activation.request_url.is_empty());
    assert!(!activation.browser_url.is_empty());
    assert!(licensing.get_requested_activation(&activation).unwrap().is_none());

    // Fulfil it as a trial, as a customer clicking "Start trial" in the browser would.
    let fulfil = HttpRequest {
        method: Method::Post,
        url: format!("{}/api/customer/activations/{}/trial", options.endpoint.trim_end_matches('/'), activation.id),
        headers: BTreeMap::from([
            ("Accept".to_owned(), "application/json".to_owned()),
            ("User-Agent".to_owned(), "moonbase-rs-live-test".to_owned()),
            ("x-mb-client".to_owned(), "moonbase-rs".to_owned()),
        ]),
        connect_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(30),
        body: String::new(),
    };
    let response = transport.send(&fulfil).unwrap();
    assert!((200..300).contains(&response.status), "{}: {}", response.status, response.body);

    let mut fulfilled = None;
    for _ in 0..20 {
        thread::sleep(Duration::from_secs(1));
        if let Some(license) = licensing.get_requested_activation(&activation).unwrap() {
            fulfilled = Some(license);
            break;
        }
    }
    let license = fulfilled.expect("the trial activation was fulfilled");
    assert!(license.trial);
    assert_eq!(license.licensed_product.id, options.product_id);

    let revalidated = licensing.validate_token_online(&license.token).unwrap();
    assert_eq!(revalidated.id, license.id);
    assert_eq!(revalidated.activation_id, license.activation_id);
    assert!(revalidated.validated_at >= license.validated_at);
}
