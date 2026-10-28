//! Add-ons (sub-products): the license token's `sp` claim, the add-on endpoints,
//! the add-on list, and how `Licensing` stores what they return.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use common::*;
use moonbase::{
    Error, HttpRequest, HttpResponse, HttpTransport, License, LicenseStore, LicenseValidator, Licensing,
    MemoryLicenseStore, StaticDeviceIdResolver, StoreLockGuard, SubProductTrialStatus,
};
use serde_json::{Value, json};

fn validator() -> LicenseValidator {
    LicenseValidator::new(&options(), Arc::new(StaticDeviceIdResolver::new("Test Device", DEVICE_ID))).unwrap()
}

fn token_with(extra: Value) -> String {
    let mut claims = claims();
    for (key, value) in extra.as_object().unwrap() {
        claims[key] = value.clone();
    }
    make_token(&claims)
}

fn problem(status: u16, error_type: &str, detail: &str) -> HttpResponse {
    let mut body = json!({"title": "Refused", "detail": detail});
    if !error_type.is_empty() {
        body["errorType"] = json!(error_type);
    }
    HttpResponse::new(status, body.to_string())
}

fn query_of(url: &str) -> &str {
    url.split_once('?').map_or("", |(_, query)| query)
}

fn sample_catalog() -> Value {
    json!({
        "currency": "EUR",
        "subProducts": [
            {
                "id": "strings",
                "product": {
                    "id": "demo-app.strings",
                    "name": "Strings",
                    "tagline": "Bowed and plucked",
                    "description": null,
                    "website": null,
                    "iconUrl": "https://cdn.example.com/strings.png",
                    "owned": false,
                    "subscribed": false,
                    "defaultVariation": {"id": "one-off", "name": "Strings", "originalPrice": {"EUR": 49}, "price": {"EUR": 39.5, "USD": 42}, "hasDiscount": true, "recurrence": {"type": "OneOff"}},
                    "variations": [
                        {"id": "one-off", "name": "Strings", "originalPrice": {"EUR": 49}, "price": {"EUR": 39.5}, "hasDiscount": true, "recurrence": {"type": "OneOff"}},
                        {"id": "monthly", "name": "Strings monthly", "originalPrice": {"EUR": 5}, "price": {"EUR": 5}, "hasDiscount": false, "recurrence": {"type": "Recurring", "cycleLength": "Monthly"}}
                    ]
                },
                "includedInLicense": true,
                "active": false,
                "revoked": false,
                "trial": {"status": "Available", "numberOfDays": 7, "requiresNewsletterOptIn": false},
                "buyUrl": "https://demo.moonbase.sh/buy/demo-app.strings"
            },
            {
                "id": "pack",
                "product": {"id": "demo-app.pack", "name": "Pack", "tagline": "", "owned": true, "subscribed": false},
                "includedInLicense": true,
                "active": true,
                "expiresAt": "2027-01-01T00:00:00Z",
                "seats": {"total": 2, "used": 1, "remaining": 1},
                "trial": {"status": "SomethingNew", "requiresNewsletterOptIn": false}
            },
            {
                "id": "keys",
                "product": {"id": "demo-app.keys", "name": "Keys", "tagline": "", "owned": false, "subscribed": false},
                "includedInLicense": true,
                "active": true,
                "revoked": false,
                "expiresAt": "2027-02-01T12:30:00.1234567",
                "trial": {"status": "Active", "numberOfDays": 14, "expiresAt": "2027-02-01T14:30:00+02:00", "requiresNewsletterOptIn": false}
            }
        ]
    })
}

// -- The license token's add-ons ----------------------------------------------------

#[test]
fn the_license_reads_the_add_ons_its_token_reports() {
    let trial_ends = now_seconds() + 3600;
    let license = validator()
        .validate_token(&token_with(json!({"sp": [
            {"id": "pack", "active": true, "trial": false, "seats": {"total": 3, "used": 1, "remaining": 2}},
            {"id": "strings", "active": true, "trial": true, "exp": trial_ends},
            {"id": "brass", "active": false, "trial": false, "seats": {"total": 1, "used": 1, "remaining": 0}},
            {"id": "keys", "active": false, "trial": false, "revoked": true},
        ]})))
        .unwrap();

    assert_eq!(license.sub_products.len(), 4);
    let pack = &license.sub_products[0];
    assert_eq!(pack.id, "pack");
    assert!(pack.active && !pack.trial && !pack.revoked);
    assert_eq!(pack.expires_at, None);
    let seats = pack.seats.unwrap();
    assert_eq!((seats.total, seats.used, seats.remaining), (3, 1, 2));

    let strings = &license.sub_products[1];
    assert!(strings.trial);
    assert_eq!(epoch_seconds(strings.expires_at.unwrap()), trial_ends);
    assert_eq!(strings.seats, None);

    assert!(license.sub_products[3].revoked);

    assert!(license.has_sub_product("pack"));
    assert!(license.has_sub_product("demo-app.strings"));
    assert!(!license.has_sub_product("brass"), "owned, but every seat is taken elsewhere");
    assert!(!license.has_sub_product("keys"), "revoked on this device");
    assert!(!license.has_sub_product("unknown"));
    // The ownership list is unchanged and separate.
    assert_eq!(license.owned_sub_product_ids, ["demo-app-pro", "demo-app-extra"]);
}

#[test]
fn an_add_on_whose_trial_ran_out_since_the_license_was_issued_does_not_unlock() {
    let ended = now_seconds() - 60;
    let license = validator()
        .validate_token(&token_with(json!({"sp": [{"id": "strings", "active": true, "trial": true, "exp": ended}]})))
        .unwrap();
    assert!(!license.has_sub_product("strings"));
    // As of a moment before it ended, it did.
    let before = license.sub_products[0].expires_at.unwrap() - std::time::Duration::from_secs(1);
    assert!(license.has_sub_product_at("strings", before));
}

#[test]
fn an_add_on_entry_the_sdk_cannot_read_is_skipped_not_the_license() {
    let license = validator()
        .validate_token(&token_with(json!({"sp": [
            {"active": true},
            "pack",
            {"id": 5, "active": true},
            {"id": "strings", "active": true, "trial": "yes", "future": {"field": 1}, "seats": {"total": 2}},
        ]})))
        .unwrap();
    assert_eq!(license.sub_products.len(), 1);
    assert!(license.has_sub_product("strings"));
    assert!(!license.sub_products[0].trial, "not a boolean");
    assert_eq!(license.sub_products[0].seats, None, "incomplete");
}

#[test]
fn a_license_without_add_ons_reports_none() {
    let none = validator().validate_token(&token_with(json!({}))).unwrap();
    assert!(none.sub_products.is_empty());
    assert!(!none.has_sub_product("pack"));
    let not_a_list = validator().validate_token(&token_with(json!({"sp": {"id": "pack"}}))).unwrap();
    assert!(not_a_list.sub_products.is_empty());
}

#[test]
fn find_sub_product_names_an_add_on_by_its_own_id_or_its_whole_id() {
    let license = validator().validate_token(&token_with(json!({"sp": [{"id": "strings", "active": true}]}))).unwrap();
    assert!(license.find_sub_product("strings").is_some());
    assert_eq!(license.find_sub_product("demo-app.strings"), Some(&license.sub_products[0]));
    // Another product's add-on of the same name is not this one.
    assert_eq!(license.find_sub_product("other-app.strings"), None);
    assert_eq!(license.find_sub_product("demo-app."), None);
    assert_eq!(license.find_sub_product(""), None);
}

#[test]
fn a_stored_license_reads_without_add_ons_it_cant_make_sense_of() {
    let license = validator().validate_token(&make_token(&claims())).unwrap();
    let mut stored = serde_json::to_value(&license).unwrap();

    // Stored before add-ons were reported.
    stored.as_object_mut().unwrap().remove("subProducts");
    assert!(serde_json::from_value::<License>(stored.clone()).unwrap().sub_products.is_empty());

    // An entry that can't be read is dropped, the license kept.
    stored["subProducts"] = json!([
        {"active": true},
        {"id": "pack", "active": true, "expiresAt": "not a time"},
        {"id": "strings", "active": true},
    ]);
    let read = serde_json::from_value::<License>(stored).unwrap();
    assert_eq!(read.sub_products.len(), 1);
    assert_eq!(read.sub_products[0].id, "strings");
}

// -- The add-on endpoints -----------------------------------------------------------

#[test]
fn activate_sub_product_posts_the_license_to_the_add_ons_activation() {
    let fresh = token_with(json!({"sp": [{"id": "strings", "active": true, "trial": false}]}));
    let f = fixture([ok(fresh.clone())]);

    let license = f.licensing.client().activate_sub_product("current-token", "strings").unwrap();

    let request = &f.transport.requests()[0];
    assert_eq!(request.method, moonbase::Method::Post);
    assert!(
        request.url.starts_with("https://demo.moonbase.sh/api/client/licenses/demo-app/sub-products/strings/activate?")
    );
    let query = query_of(&request.url);
    for part in ["format=JWT", "platform=Mac", "appVersion=1.2.3", "meta%5Bchannel%5D=test"] {
        assert!(query.contains(part), "{part} in {query}");
    }
    assert!(!query.contains("autoActivateSubProducts"));
    assert_eq!(request.body, "current-token");
    assert_eq!(request.headers["Content-Type"], "text/plain");
    assert_eq!(request.headers["x-mb-client"], "moonbase-rs");

    assert_eq!(license.token, fresh);
    assert!(license.has_sub_product("strings"));
}

#[test]
fn request_sub_product_trial_posts_the_license_to_the_add_ons_trial() {
    let fresh =
        token_with(json!({"sp": [{"id": "strings", "active": true, "trial": true, "exp": now_seconds() + 86400}]}));
    let f = fixture([ok(fresh)]);

    let license = f.licensing.client().request_sub_product_trial("current-token", "demo-app.strings").unwrap();

    let request = &f.transport.requests()[0];
    // A whole product ID goes in as given; the server takes either form.
    assert!(
        request
            .url
            .starts_with("https://demo.moonbase.sh/api/client/licenses/demo-app/sub-products/demo-app.strings/trial?")
    );
    assert_eq!(request.body, "current-token");
    assert!(license.sub_products[0].trial);
}

#[test]
fn an_add_on_id_is_escaped_in_the_path() {
    let f = fixture([ok(make_token(&claims()))]);
    f.licensing.client().activate_sub_product("current-token", "a b/c").unwrap();
    assert!(f.transport.requests()[0].url.contains("/sub-products/a%20b%2Fc/activate?"));
}

#[test]
fn an_add_on_call_without_an_add_on_id_is_refused_without_contacting_the_api() {
    let f = fixture([]);
    assert!(matches!(f.licensing.client().activate_sub_product("current-token", ""), Err(Error::Configuration(_))));
    assert!(matches!(
        f.licensing.client().request_sub_product_trial("current-token", "  "),
        Err(Error::Configuration(_))
    ));
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn an_add_on_call_that_gets_no_license_token_back_is_retryable() {
    let f = fixture([ok("<html><body>Sign in to the Wi-Fi</body></html>")]);
    let error = f.licensing.client().activate_sub_product("current-token", "strings").unwrap_err();
    assert_eq!(error.as_api_error().map(|api| api.status_code()), Some(200), "{error:?}");
}

#[test]
fn the_add_on_endpoints_tell_a_refused_add_on_from_a_refused_license() {
    type Call = fn(&moonbase::LicenseClient) -> Result<(), Error>;
    let endpoints: [(&str, Call); 3] = [
        ("activate", |client| client.activate_sub_product("token", "strings").map(drop)),
        ("trial", |client| client.request_sub_product_trial("token", "strings").map(drop)),
        ("list", |client| client.list_sub_products("token").map(drop)),
    ];

    for (name, call) in endpoints {
        let f = fixture([]);
        let client = f.licensing.client();

        // No seat to give: about the add-on, so not a license verdict.
        f.transport.push(problem(400, "NoEligibleLicense", "No license with available activations found"));
        let error = call(client).unwrap_err();
        assert!(
            matches!(&error, Error::NoEligibleLicense(message) if message == "No license with available activations found"),
            "{name}: {error:?}"
        );
        assert_eq!(error.error_type(), moonbase::ErrorType::NoEligibleLicense);
        assert!(!error.is_license_rejection(), "{name}");

        // A refusal without a code is about the add-on too.
        f.transport.push(problem(400, "", "'Strings' is not included in the license for 'Demo App'"));
        let error = call(client).unwrap_err();
        assert!(
            matches!(&error, Error::OperationNotSupported(message) if message == "'Strings' is not included in the license for 'Demo App'"),
            "{name}: {error:?}"
        );

        // The license itself refused.
        for code in ["LicenseInvalid", "LicenseRevoked", "LicenseActivationRevoked"] {
            f.transport.push(problem(400, code, "Refused"));
            assert!(matches!(call(client), Err(Error::LicenseInvalid(_))), "{name}: {code}");
        }
        f.transport.push(problem(400, "LicenseExpired", "Refused"));
        assert!(matches!(call(client), Err(Error::LicenseExpired(_))), "{name}");

        // No verdict at all.
        f.transport.push(problem(404, "", "Refused"));
        assert!(matches!(call(client), Err(Error::Api(_))), "{name}");
        f.transport.push(HttpResponse::new(503, r#"{"message":"Service Unavailable"}"#));
        assert!(matches!(call(client), Err(Error::Api(_))), "{name}");
        f.transport.push(problem(410, "StoreClosed", "Refused"));
        assert!(matches!(call(client), Err(Error::StoreClosed(_))), "{name}");
    }
}

#[test]
fn auto_activate_sub_products_off_asks_the_server_not_to_take_add_on_seats() {
    let f = fixture_with(
        options().auto_activate_sub_products(false),
        [],
        Arc::new(MemoryLicenseStore::new()),
        "Test Device",
        DEVICE_ID,
    );
    let client = f.licensing.client();
    f.transport.push(ok(make_token(&claims())));
    client.validate_token_online("current-token").unwrap();
    f.transport.push(activation_response());
    client.request_activation(moonbase::ActivationMethod::Online).unwrap();
    f.transport.push(ok(make_token(&claims())));
    client.activate_sub_product("current-token", "strings").unwrap();

    let requests = f.transport.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert!(query_of(&request.url).contains("autoActivateSubProducts=false"), "{}", request.url);
    }

    // On, the server's default, it is left out.
    let on = fixture([ok(make_token(&claims()))]);
    on.licensing.client().validate_token_online("current-token").unwrap();
    assert!(!query_of(&on.transport.requests()[0].url).contains("autoActivateSubProducts"));
}

// -- The add-on list ----------------------------------------------------------------

#[test]
fn list_sub_products_posts_the_license_and_reads_the_add_on_list() {
    let f = fixture([ok(sample_catalog().to_string())]);
    let catalog = f.licensing.client().list_sub_products("current-token").unwrap();

    let request = &f.transport.requests()[0];
    assert_eq!(request.method, moonbase::Method::Post);
    assert_eq!(request.url, "https://demo.moonbase.sh/api/client/licenses/demo-app/sub-products");
    assert_eq!(request.body, "current-token");
    assert_eq!(request.headers["Content-Type"], "text/plain");

    assert_eq!(catalog.currency, "EUR");
    assert_eq!(catalog.sub_products.len(), 3);

    let strings = &catalog.sub_products[0];
    assert_eq!(strings.id, "strings");
    assert_eq!(strings.product.id, "demo-app.strings");
    assert_eq!(strings.product.name, "Strings");
    assert_eq!(strings.product.tagline.as_deref(), Some("Bowed and plucked"));
    assert_eq!(strings.product.description, None);
    assert_eq!(strings.product.icon_url.as_deref(), Some("https://cdn.example.com/strings.png"));
    assert!(strings.included_in_license && !strings.active);
    let price = strings.product.default_variation.as_ref().unwrap();
    assert_eq!(price.price["EUR"], 39.5);
    assert_eq!(price.price["USD"], 42.0);
    assert_eq!(price.original_price["EUR"], 49.0);
    assert!(price.has_discount);
    assert_eq!(strings.product.variations.len(), 2);
    let monthly = strings.product.variations[1].recurrence.as_ref().unwrap();
    assert_eq!((monthly.kind.as_str(), monthly.cycle_length.as_deref()), ("Recurring", Some("Monthly")));
    assert_eq!(strings.product.variations[0].recurrence.as_ref().unwrap().cycle_length, None);
    let trial = strings.trial.as_ref().unwrap();
    assert_eq!(trial.status, SubProductTrialStatus::Available);
    assert_eq!(trial.number_of_days, Some(7));
    assert_eq!(trial.expires_at, None);
    assert_eq!(strings.buy_url.as_deref(), Some("https://demo.moonbase.sh/buy/demo-app.strings"));

    let pack = &catalog.sub_products[1];
    assert!(pack.active && pack.product.owned);
    assert!(!pack.revoked, "absent");
    assert_eq!(pack.product.default_variation, None);
    assert!(pack.product.variations.is_empty());
    assert_eq!(pack.buy_url, None);
    assert_eq!(pack.expires_at, Some(iso("2027-01-01T00:00:00Z")));
    assert_eq!(pack.seats.map(|seats| (seats.total, seats.remaining)), Some((2, 1)));
    // A status this version does not know.
    assert_eq!(pack.trial.as_ref().unwrap().status, SubProductTrialStatus::Unavailable);
    assert_eq!(pack.trial.as_ref().unwrap().number_of_days, None);

    let keys = &catalog.sub_products[2];
    assert_eq!(keys.trial.as_ref().unwrap().status, SubProductTrialStatus::Active);
    // A time without a zone is UTC; one with an offset is moved to UTC.
    assert_eq!(keys.expires_at, Some(iso("2027-02-01T12:30:00Z")));
    assert_eq!(keys.trial.as_ref().unwrap().expires_at, Some(iso("2027-02-01T12:30:00Z")));
}

#[test]
fn an_answer_that_isnt_the_add_on_list_is_retryable() {
    for body in [
        "<html><body>Sign in to the Wi-Fi</body></html>",
        r#"{"subProducts":[]}"#,
        r#"{"currency":"EUR","subProducts":[{"id":"strings"}]}"#,
        r#"{"currency":"EUR","subProducts":[{"id":"pack","product":{"id":"demo-app.pack","name":"Pack","owned":true,"subscribed":false},"includedInLicense":true,"active":true,"expiresAt":"next year"}]}"#,
    ] {
        let f = fixture([ok(body)]);
        let error = f.licensing.client().list_sub_products("current-token").unwrap_err();
        let api = error.as_api_error().unwrap_or_else(|| panic!("{body}: {error:?}"));
        assert_eq!(api.status_code(), 200);
        assert!(api.message().starts_with("Could not parse the add-on list"), "{}", api.message());
    }
}

#[test]
fn an_empty_add_on_list_is_a_list() {
    let f = fixture([ok(r#"{"currency":"USD","subProducts":[]}"#)]);
    let catalog = f.licensing.client().list_sub_products("current-token").unwrap();
    assert_eq!(catalog.currency, "USD");
    assert!(catalog.sub_products.is_empty());
}

// -- Licensing ----------------------------------------------------------------------

#[test]
fn licensing_stores_the_license_an_add_on_call_returns() {
    let fresh = token_with(json!({"sp": [{"id": "strings", "active": true, "trial": false}]}));
    for trial in [false, true] {
        let f = fixture([ok(fresh.clone())]);
        let token = make_token(&claims());
        let license = if trial {
            f.licensing.request_sub_product_trial(&token, "strings").unwrap()
        } else {
            f.licensing.activate_sub_product(&token, "strings").unwrap()
        };
        assert!(license.has_sub_product("strings"));
        assert_eq!(f.store.load_local_license().unwrap().unwrap().token, fresh);
    }
}

#[test]
fn licensing_leaves_the_store_alone_when_should_persist_says_no() {
    let f = fixture([ok(make_token(&claims()))]);
    let asked = std::cell::Cell::new(false);
    f.licensing
        .activate_sub_product_with(&make_token(&claims()), "strings", || {
            asked.set(true);
            false
        })
        .unwrap();
    assert!(asked.get());
    assert_eq!(f.store.load_local_license().unwrap(), None);
}

#[test]
fn licensing_refuses_add_on_calls_from_a_trial_without_contacting_the_api() {
    let f = fixture([]);
    let trial = token_with(json!({"trial": true}));
    assert!(matches!(f.licensing.activate_sub_product(&trial, "strings"), Err(Error::OperationNotSupported(_))));
    assert!(matches!(f.licensing.request_sub_product_trial(&trial, "strings"), Err(Error::OperationNotSupported(_))));
    assert_eq!(f.transport.request_count(), 0);
}

#[test]
fn licensing_lists_add_ons_from_a_license_or_a_trial() {
    let f = fixture([ok(sample_catalog().to_string()), ok(sample_catalog().to_string())]);
    assert_eq!(f.licensing.list_sub_products(&make_token(&claims())).unwrap().sub_products.len(), 3);
    assert_eq!(f.licensing.list_sub_products(&token_with(json!({"trial": true}))).unwrap().sub_products.len(), 3);
    assert_eq!(f.store.load_local_license().unwrap(), None, "listing changes nothing");
}

#[test]
fn licensing_checks_the_token_locally_before_any_add_on_call() {
    let f = fixture([]);
    let foreign = token_with(json!({"sig": "another-device"}));
    assert!(matches!(f.licensing.activate_sub_product(&foreign, "strings"), Err(Error::DeviceMismatch(_))));
    assert!(matches!(f.licensing.request_sub_product_trial(&foreign, "strings"), Err(Error::DeviceMismatch(_))));
    assert!(matches!(f.licensing.list_sub_products(&foreign), Err(Error::DeviceMismatch(_))));
    assert_eq!(f.transport.request_count(), 0);
}

/// A memory store that counts how deep its update lock is held.
#[derive(Default)]
struct LockWatchingStore {
    inner: MemoryLicenseStore,
    depth: Arc<AtomicI32>,
}

struct Depth(Arc<AtomicI32>);

impl Drop for Depth {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl LicenseStore for LockWatchingStore {
    fn load_local_license(&self) -> moonbase::Result<Option<License>> {
        self.inner.load_local_license()
    }
    fn store_local_license(&self, license: &License) -> moonbase::Result<()> {
        self.inner.store_local_license(license)
    }
    fn delete_local_license(&self) -> moonbase::Result<()> {
        self.inner.delete_local_license()
    }
    fn lock_for_update(&self) -> moonbase::Result<Option<StoreLockGuard<'_>>> {
        let inner = self.inner.lock_for_update()?;
        self.depth.fetch_add(1, Ordering::SeqCst);
        Ok(Some(StoreLockGuard::new((inner, Depth(self.depth.clone())))))
    }
}

/// Notes how deep the store's lock is held whenever a request goes out.
struct DepthNotingTransport {
    inner: Arc<RecordingTransport>,
    depth: Arc<AtomicI32>,
    at_send: std::sync::Mutex<Vec<i32>>,
}

impl HttpTransport for DepthNotingTransport {
    fn send(&self, request: &HttpRequest) -> moonbase::Result<HttpResponse> {
        self.at_send.lock().unwrap().push(self.depth.load(Ordering::SeqCst));
        self.inner.send(request)
    }
}

#[test]
fn an_add_on_call_holds_the_stores_lock_from_its_request_until_its_answer_is_stored() {
    let store = Arc::new(LockWatchingStore::default());
    let fresh = token_with(json!({"sp": [{"id": "drum-kit", "active": true, "trial": false}]}));
    let transport = Arc::new(DepthNotingTransport {
        inner: RecordingTransport::new([ok(fresh.clone())]),
        depth: store.depth.clone(),
        at_send: Default::default(),
    });
    let licensing = Licensing::builder(options())
        .store(store.clone())
        .device_id_resolver(Arc::new(StaticDeviceIdResolver::new("Test Device", DEVICE_ID)))
        .transport(transport.clone())
        .build()
        .unwrap();

    let asked_under_lock = std::cell::Cell::new(false);
    licensing
        .activate_sub_product_with(&make_token(&claims()), "drum-kit", || {
            asked_under_lock.set(store.depth.load(Ordering::SeqCst) == 1);
            true
        })
        .unwrap();

    // So another instance's validation, which takes the same lock, can't read the
    // store before this answer is in it, or store an older one after it.
    assert_eq!(*transport.at_send.lock().unwrap(), [1]);
    assert!(asked_under_lock.get());
    assert_eq!(store.depth.load(Ordering::SeqCst), 0);
    assert_eq!(store.load_local_license().unwrap().unwrap().token, fresh);
}

/// An add-on call answers with the activation's last validation time, so the copy
/// it stored and the one from before it tie, and nothing in them says which is
/// current.
fn tied_copies() -> (String, String, String) {
    let validated = now_seconds() - 30;
    let drum_kit = json!([{"id": "drum-kit", "active": true, "trial": false}]);
    let before = token_with(json!({"validated": validated}));
    let with_add_on = token_with(json!({"validated": validated, "sp": drum_kit}));
    let checked = token_with(json!({"validated": now_seconds(), "sp": drum_kit}));
    (before, with_add_on, checked)
}

#[test]
fn a_validation_that_finds_a_tied_copy_asks_the_server_for_a_caller_holding_the_older_one() {
    let (before, with_add_on, checked) = tied_copies();
    let f = fixture([ok(with_add_on), ok(checked.clone())]);
    f.licensing.activate_sub_product(&before, "drum-kit").unwrap();

    let validated = f.licensing.validate_token_online(&before).unwrap();
    assert_eq!(validated.token, checked);
    assert!(validated.has_sub_product("drum-kit"));
    assert_eq!(f.transport.request_count(), 2);
    assert_eq!(f.store.load_local_license().unwrap().unwrap().token, checked);
}

#[test]
fn a_validation_that_finds_a_tied_copy_asks_the_server_for_a_caller_holding_a_newer_unstored_one() {
    let (before, with_add_on, checked) = tied_copies();
    let f = fixture([ok(with_add_on), ok(checked.clone())]);
    f.store.store_local_license(&f.licensing.validate_token_local(&before).unwrap()).unwrap();
    let newer = f.licensing.client().activate_sub_product(&before, "drum-kit").unwrap(); // stores nothing

    let validated = f.licensing.validate_token_online(&newer.token).unwrap();
    assert!(validated.has_sub_product("drum-kit"), "not the store's older copy");
    assert_eq!(f.transport.request_count(), 2);
    assert_eq!(f.store.load_local_license().unwrap().unwrap().token, checked);
}

#[test]
fn a_validation_that_finds_the_same_copy_stored_stays_within_the_throttle() {
    let f = fixture([]);
    let token = make_token(&claims());
    f.store.store_local_license(&f.licensing.validate_token_local(&token).unwrap()).unwrap();
    assert_eq!(f.licensing.validate_token_online(&token).unwrap().token, token);
    assert_eq!(f.transport.request_count(), 0);
}
