//! The Moonbase client API: activation requests, polling, online validation,
//! revocation and add-ons, and how each response maps to an [`Error`].

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::device_id::DeviceIdResolver;
use crate::error::{ApiError, Error, Result};
use crate::http::{HttpRequest, HttpResponse, HttpTransport, Method};
use crate::release_access::{ReleaseAccess, ReleaseNotCovered};
use crate::sub_product::{SubProductCatalog, parse_catalog};
use crate::time::parse_api_timestamp;
use crate::types::{ActivationMethod, ActivationRequest, License, LicensingOptions, Platform};
use crate::url::{append_query, trim_trailing_slashes, url_encode};
use crate::validator::{LicenseValidator, decode_jwt_json, split_jwt};
use crate::version::parse_app_version;

/// This SDK's version, as sent in the `User-Agent`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Every transport assembles headers into lines, so a CR or LF in a caller's
/// `client_info` would inject a header. Control characters become spaces, runs of
/// whitespace collapse, and the result is trimmed and capped, so the `User-Agent`
/// stays a well-formed, bounded token list however many layers appended to it.
pub(crate) fn sanitize_client_info(value: &str) -> String {
    // Generous for a stack of "Name/Version (comment)" segments, and well under
    // the header limits proxies enforce.
    const MAX_LENGTH: usize = 256;

    let mut result = String::new();
    for character in value.chars() {
        if character.is_ascii_control() || character == ' ' {
            if !result.is_empty() && !result.ends_with(' ') {
                result.push(' ');
            }
        } else {
            result.push(character);
        }
        if result.len() >= MAX_LENGTH {
            break;
        }
    }
    // A multi-byte character can carry the string just past the cap.
    while result.len() > MAX_LENGTH {
        result.pop();
    }
    result.trim_end_matches(' ').to_owned()
}

pub(crate) fn endpoint_path(options: &LicensingOptions, path: &str) -> String {
    format!("{}{path}", trim_trailing_slashes(&options.endpoint))
}

fn license_path(options: &LicensingOptions, action: &str) -> String {
    endpoint_path(options, &format!("/api/client/licenses/{}/{action}", url_encode(&options.product_id)))
}

fn sub_products_path(options: &LicensingOptions) -> String {
    license_path(options, "sub-products")
}

/// `action` is `activate` or `trial`. The id goes in as given: the server takes the
/// add-on's own part of its product ID or its whole ID.
fn sub_product_path(options: &LicensingOptions, sub_product_id: &str, action: &str) -> String {
    format!("{}/{}/{action}", sub_products_path(options), url_encode(sub_product_id))
}

fn client_query(options: &LicensingOptions) -> BTreeMap<String, String> {
    let mut query = BTreeMap::from([("format".to_owned(), "JWT".to_owned())]);
    if options.platform != Platform::Unknown {
        query.insert("platform".into(), options.platform.as_str().into());
    }
    if let Some(version) = &options.application_version {
        query.insert("appVersion".into(), version.clone());
    }
    for (key, value) in &options.metadata {
        if !value.is_empty() {
            query.insert(format!("meta[{key}]"), value.clone());
        }
    }
    // Only sent when off, the server's default.
    if !options.auto_activate_sub_products {
        query.insert("autoActivateSubProducts".into(), "false".into());
    }
    query
}

pub(crate) fn default_headers(options: &LicensingOptions, content_type: Option<&str>) -> BTreeMap<String, String> {
    let mut user_agent = format!("moonbase-rs/{VERSION}");
    if let Some(client_info) = &options.client_info {
        // Sanitise before the emptiness check, so a segment that is only
        // whitespace or control characters leaves no dangling separator.
        let client_info = sanitize_client_info(client_info);
        if !client_info.is_empty() {
            user_agent.push(' ');
            user_agent.push_str(&client_info);
        }
    }
    let mut headers = BTreeMap::from([
        ("Accept".to_owned(), "application/json, application/jwt, text/plain".to_owned()),
        ("User-Agent".to_owned(), user_agent),
        ("x-mb-client".to_owned(), "moonbase-rs".to_owned()),
    ]);
    if let Some(content_type) = content_type {
        headers.insert("Content-Type".into(), content_type.into());
    }
    headers
}

pub(crate) fn build_request(
    options: &LicensingOptions,
    method: Method,
    url: String,
    headers: BTreeMap<String, String>,
    body: String,
) -> HttpRequest {
    HttpRequest {
        method,
        url,
        headers,
        connect_timeout: options.http_connect_timeout,
        request_timeout: options.http_request_timeout,
        body,
    }
}

#[derive(Debug, Default)]
pub(crate) struct ProblemDetails {
    title: String,
    detail: String,
    error_type: String,
}

/// An HTML or XML page where JSON or a token belongs: a proxy, CDN or captive
/// portal answered instead of the API.
pub(crate) fn looks_like_markup(body: &str) -> bool {
    body.trim_ascii_start().starts_with('<')
}

/// The model-validation 400 carries no detail, only a field-to-messages map. One
/// message (the first field by name) is what a developer needs to see.
fn first_validation_error(problem: &Value) -> String {
    let Some(errors) = problem.get("errors").and_then(Value::as_object) else {
        return String::new();
    };
    for (field, messages) in errors {
        if let Some(message) = messages.as_array().and_then(|list| list.first()).and_then(Value::as_str) {
            return if field.is_empty() { message.to_owned() } else { format!("{field}: {message}") };
        }
    }
    String::new()
}

pub(crate) fn parse_problem(body: &str) -> ProblemDetails {
    // Long enough for any message the API writes, short enough that an error page
    // nobody parsed cannot become an error message.
    const MAX_PLAIN_TEXT_LENGTH: usize = 200;

    let mut result = ProblemDetails::default();
    if body.is_empty() {
        return result;
    }
    let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);

    match serde_json::from_str::<Value>(body) {
        // BadRequest("...") on an endpoint that otherwise answers JSON, e.g.
        // "Trials can not be revoked".
        Ok(Value::String(message)) => result.detail = message,
        Ok(problem @ Value::Object(_)) => {
            result.title = text(&problem, "title").unwrap_or_default();
            result.detail = text(&problem, "detail").unwrap_or_default();
            if result.detail.is_empty() {
                result.detail = first_validation_error(&problem);
            }
            if result.detail.is_empty() && result.title.is_empty() {
                // The API gateway's own 502 and 504 bodies, e.g. {"message":"Endpoint
                // request timed out"}. Worth quoting, but not the API's verdict.
                result.detail = text(&problem, "message").unwrap_or_default();
            }
            result.error_type = text(&problem, "errorType").unwrap_or_default();
        }
        Ok(_) => {}
        Err(_) => {
            // Not JSON. A short line of plain text is still worth quoting, since the
            // same bare-string answers come back as text/plain to some clients.
            let text = body.trim_ascii();
            if !text.is_empty()
                && text.len() <= MAX_PLAIN_TEXT_LENGTH
                && !looks_like_markup(text)
                && !text.contains(['\r', '\n'])
            {
                result.detail = text.to_owned();
            }
        }
    }
    result
}

fn problem_message(status: u16, problem: &ProblemDetails) -> String {
    if !problem.detail.is_empty() {
        problem.detail.clone()
    } else if !problem.title.is_empty() {
        problem.title.clone()
    } else {
        format!("Moonbase API request failed with HTTP {status}")
    }
}

/// A `Retry-After` given in seconds, the form the rate limiter in front of the API
/// sends with its 429. An HTTP-date is ignored: nothing in front of the API sends
/// one.
pub(crate) fn retry_after(response: &HttpResponse) -> Option<Duration> {
    let text = response.header("retry-after")?.trim_ascii();
    // Nine digits is over thirty years, so the conversion cannot overflow.
    if text.is_empty() || text.len() > 9 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().map(Duration::from_secs)
}

/// A closed store answers every request with 410 `StoreClosed`, whatever was
/// asked. The `errorType` is Moonbase's own extension, which is what makes it safe
/// to treat as final: a 410 from anything else carries none.
fn store_closed(problem: &ProblemDetails) -> Option<Error> {
    (problem.error_type == "StoreClosed").then(|| {
        Error::StoreClosed(if problem.detail.is_empty() {
            "This store has closed.".to_owned()
        } else {
            problem.detail.clone()
        })
    })
}

fn api_error(response: &HttpResponse, problem: ProblemDetails) -> Error {
    ApiError {
        status_code: response.status,
        message: problem_message(response.status, &problem),
        title: problem.title,
        detail: problem.detail,
        retry_after: retry_after(response),
    }
    .into()
}

/// The shared mapping for the license endpoints. A 400 is a definitive refusal of
/// what was sent; everything else, including rate limiting and 5xx failures,
/// stays an [`ApiError`] that is worth retrying.
///
/// That includes a 403 or 404, although `/validate` and `/revoke` send those for a
/// license that no longer exists or changed hands. Their body is stock ASP.NET
/// ProblemDetails with no `errorType`, which a misrouted request gets from any
/// other ASP.NET service too, and reading it as final would skip the offline grace
/// period and lock a valid license over a routing fault.
pub(crate) fn error_for_problem(response: &HttpResponse) -> Error {
    let problem = parse_problem(&response.body);
    if let Some(closed) = store_closed(&problem) {
        return closed;
    }
    if problem.error_type == "LicenseExpired" {
        return Error::LicenseExpired(problem_message(response.status, &problem));
    }
    if response.status == 400 {
        // Older servers said "expired" without an errorType. Only trusted on a 400,
        // so a gateway's 5xx page mentioning an expired session can never pass for
        // a license verdict.
        let message = problem_message(response.status, &problem);
        if problem.title.contains("expired") || problem.detail.contains("expired") {
            return Error::LicenseExpired(message);
        }
        return Error::LicenseInvalid(message);
    }
    api_error(response, problem)
}

/// The problem as a JSON value, when the body is a JSON object.
fn problem_object(body: &str) -> Option<Value> {
    serde_json::from_str::<Value>(body).ok().filter(Value::is_object)
}

/// A string field of a problem, or `None` when it is missing, blank or not a
/// string: the extensions a problem carries are read leniently, so a change to one
/// never hides the error itself.
pub(crate) fn problem_string(problem: &Value, key: &str) -> Option<String> {
    problem.get(key).and_then(Value::as_str).filter(|value| !value.trim_ascii().is_empty()).map(str::to_owned)
}

/// What a license covers, as the API describes it in a `ReleaseNotCovered`
/// problem or a product's `releaseAccess`, or `None` when it says nothing about the
/// dates or majors covered.
pub(crate) fn release_access_from(owner: &Value) -> Option<ReleaseAccess> {
    let value = owner.get("releaseAccess").filter(|value| value.is_object())?;
    let access = ReleaseAccess {
        // Left out, like any field of the problem that can't be read.
        updates_until: problem_string(value, "updatesUntil").and_then(|until| parse_api_timestamp(&until).ok()),
        up_to_major: value.get("upToMajor").and_then(Value::as_u64),
        up_to_version: problem_string(value, "upToVersion"),
    };
    (access.updates_until.is_some() || access.up_to_major.is_some()).then_some(access)
}

/// `application_version` as Moonbase reads it (`MAJOR.MINOR.PATCH`), or as given
/// when it can't be read that way.
fn app_version_text(options: &LicensingOptions) -> String {
    let Some(version) = &options.application_version else {
        return String::new();
    };
    parse_app_version(version).map_or_else(|| version.clone(), |parsed| parsed.to_string())
}

/// The inventory endpoints never judge the license: a token they cannot use just
/// makes the request anonymous. So every failure but a closed store, and a release
/// the customer's licenses don't cover, is an [`ApiError`] carrying the status.
pub(crate) fn error_for_inventory_problem(response: &HttpResponse) -> Error {
    let problem = parse_problem(&response.body);
    if let Some(closed) = store_closed(&problem) {
        return closed;
    }
    if problem.error_type == "ReleaseNotCovered" {
        let body = problem_object(&response.body);
        return Error::ReleaseNotCovered(Box::new(ReleaseNotCovered::new(
            if problem.detail.is_empty() { "Your license doesn't cover this version".into() } else { problem.detail },
            None,
            String::new(),
            body.as_ref().and_then(release_access_from),
            body.as_ref().and_then(|body| problem_string(body, "upgradeUrl")),
        )));
    }
    api_error(response, problem)
}

/// The add-on endpoints answer about two things: the license the request carried,
/// and the add-on it asked for. Only an `errorType` about the license makes a 400 a
/// verdict on it, since an app may lock on that. Every other refusal is about the
/// add-on, and leaves the license as good as it was: `NoEligibleLicense` when the
/// customer has no seat of it to give, and [`Error::OperationNotSupported`] for an
/// add-on that can't be activated or tried from this license, such as one not
/// included in it, or a trial asked for from an offline license.
fn error_for_sub_product_problem(response: &HttpResponse) -> Error {
    let problem = parse_problem(&response.body);
    if let Some(closed) = store_closed(&problem) {
        return closed;
    }
    let message = problem_message(response.status, &problem);
    if problem.error_type == "LicenseExpired" {
        return Error::LicenseExpired(message);
    }
    if response.status == 400 {
        return match problem.error_type.as_str() {
            "LicenseInvalid" | "LicenseRevoked" | "LicenseActivationRevoked" => Error::LicenseInvalid(message),
            "NoEligibleLicense" => Error::NoEligibleLicense(message),
            _ => Error::OperationNotSupported(message),
        };
    }
    api_error(response, problem)
}

/// A 400 from the activation poll means the request will never complete. Kept
/// away from [`error_for_problem`]'s text match, which would report "Activation
/// request has expired" as an expired license; only an explicit `LicenseExpired`
/// code still means that.
fn error_for_activation_poll_problem(response: &HttpResponse) -> Error {
    if response.status != 400 {
        return error_for_problem(response);
    }
    let problem = parse_problem(&response.body);
    if let Some(closed) = store_closed(&problem) {
        return closed;
    }
    let reason = if problem.detail.is_empty() { &problem.title } else { &problem.detail };
    if problem.error_type == "LicenseExpired" {
        return Error::LicenseExpired(if reason.is_empty() {
            "The license has expired".to_owned()
        } else {
            reason.clone()
        });
    }
    Error::ActivationRequestEnded(if reason.is_empty() {
        "Activation request can no longer be completed (HTTP 400)".to_owned()
    } else {
        format!("Activation request can no longer be completed: {reason}")
    })
}

/// A 2xx whose body is not a JWT did not come from the Moonbase API: a captive
/// portal or a proxy answered in its place. That says nothing about the license,
/// so it must not reach the validator, whose `LicenseInvalid` would skip the
/// offline grace period and lock out a user who merely joined a network that
/// intercepts traffic.
fn ensure_token_response(response: &HttpResponse) -> Result<()> {
    let is_token = split_jwt(response.body.trim_ascii())
        .and_then(|[header, _, _]| decode_jwt_json(header, "header"))
        .is_ok_and(|header| header.is_object());
    if is_token {
        return Ok(());
    }
    Err(ApiError::new(
        response.status,
        format!(
            "Moonbase API answered HTTP {} without a license token; a proxy or captive portal may have intercepted \
             the request",
            response.status
        ),
    )
    .into())
}

/// Hard floor between polls of one activation request, protecting the API from
/// callers polling from a fast UI timer. Deliberately not configurable.
pub(crate) const ACTIVATION_POLL_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// The longest `Retry-After` the poll honours, so a bad header cannot stall an
/// activation for good.
pub(crate) const ACTIVATION_POLL_MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Spaces polls of the same activation request at least
/// [`ACTIVATION_POLL_MIN_INTERVAL`] apart. Each request has its own slot, so
/// polling several requests in turn never starves the later ones. The timestamp
/// is taken before the request goes out, so a poll still in flight on another
/// thread counts too. Also honours a `Retry-After` an earlier poll was told.
#[derive(Debug, Default)]
pub(crate) struct ActivationPollGate {
    state: Mutex<PollGateState>,
}

#[derive(Debug, Default)]
struct PollGateState {
    last_poll: HashMap<String, Instant>,
    hold_until: Option<Instant>,
}

impl ActivationPollGate {
    pub(crate) fn try_claim(&self, request_url: &str, now: Instant) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.hold_until.is_some_and(|hold| now < hold) {
            return false;
        }
        // Polls older than the interval hold nothing back; dropping them keeps the
        // map to the requests polled within the last interval.
        state.last_poll.retain(|_, polled| now.saturating_duration_since(*polled) < ACTIVATION_POLL_MIN_INTERVAL);
        if state.last_poll.contains_key(request_url) {
            return false;
        }
        state.last_poll.insert(request_url.to_owned(), now);
        true
    }

    /// Holds back every poll until the server's `Retry-After` has passed. One hold
    /// covers all requests, because the rate limit counts requests per client
    /// address, not per activation request.
    pub(crate) fn back_off(&self, retry_after: Duration, now: Instant) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let until = now + retry_after.min(ACTIVATION_POLL_MAX_BACKOFF);
        state.hold_until = Some(state.hold_until.map_or(until, |hold| hold.max(until)));
    }
}

/// The raw client API. Most apps use [`Licensing`](crate::Licensing), which wraps
/// this with storage, throttling and the offline grace period.
///
/// Clones share the activation poll floor, so a copy cannot double the poll rate.
#[derive(Clone)]
pub struct LicenseClient {
    options: Arc<LicensingOptions>,
    device_ids: Arc<dyn DeviceIdResolver>,
    validator: Arc<LicenseValidator>,
    transport: Arc<dyn HttpTransport>,
    poll_gate: Arc<ActivationPollGate>,
}

impl LicenseClient {
    pub fn new(
        options: LicensingOptions,
        device_ids: Arc<dyn DeviceIdResolver>,
        validator: Arc<LicenseValidator>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self { options: Arc::new(options), device_ids, validator, transport, poll_gate: Arc::default() }
    }

    /// Start a browser activation. See
    /// [`Licensing::request_activation`](crate::Licensing::request_activation).
    pub fn request_activation(&self, method: ActivationMethod) -> Result<ActivationRequest> {
        let options = &*self.options;
        // Only the request endpoint accepts "method"; /validate and /revoke do not.
        let mut query = client_query(options);
        query.insert("method".into(), method.as_str().into());
        let url = append_query(
            &endpoint_path(options, &format!("/api/client/activations/{}/request", url_encode(&options.product_id))),
            &query,
        );

        // The API refuses a blank device name or id outright. The name is only a
        // label, so stand in for one the host could not supply. The id is the
        // binding itself, and a resolver that returns none is misconfigured.
        let mut device_name = self.device_ids.device_name();
        if device_name.trim().is_empty() {
            device_name = "Unknown device".into();
        }
        let device_id = self.device_ids.device_id()?;
        if device_id.trim().is_empty() {
            return Err(Error::Configuration("The device id resolver returned an empty device id".into()));
        }

        let payload = serde_json::json!({
            "deviceName": device_name,
            "deviceSignature": device_id,
        });
        let request = build_request(
            options,
            Method::Post,
            url,
            default_headers(options, Some("application/json")),
            payload.to_string(),
        );

        let response = self.transport.send(&request)?;
        if !response.is_success() {
            return Err(self.error_for_license_problem(&response));
        }

        let mut activation: ActivationRequest = serde_json::from_str(&response.body).map_err(|error| {
            Error::from(ApiError::new(response.status, format!("Could not parse activation response: {error}")))
        })?;
        // The response carries only id/request/browser, so record what we asked for.
        activation.method = method;
        Ok(activation)
    }

    /// Poll a browser activation. See
    /// [`Licensing::get_requested_activation`](crate::Licensing::get_requested_activation).
    pub fn get_requested_activation(&self, activation: &ActivationRequest) -> Result<Option<License>> {
        if !self.poll_gate.try_claim(&activation.request_url, Instant::now()) {
            return Ok(None);
        }

        let request = build_request(
            &self.options,
            Method::Get,
            activation.request_url.clone(),
            default_headers(&self.options, None),
            String::new(),
        );
        let response = self.transport.send(&request)?;

        // A 404 reads as pending too: the API reads requests with eventual
        // consistency, so a fresh one can briefly answer 404, and Moonbase's 404
        // cannot be told apart from one a proxy sent.
        if response.status == 204 || response.status == 404 {
            return Ok(None);
        }
        if !response.is_success() {
            if let Some(wait) = retry_after(&response).filter(|_| matches!(response.status, 429 | 503)) {
                self.poll_gate.back_off(wait, Instant::now());
            }
            return Err(error_for_activation_poll_problem(&response));
        }
        ensure_token_response(&response)?;
        // Moonbase activates only a license that covers the version the request
        // named, and a seat is taken by now, so the device doesn't judge it again.
        self.validator.validate_token_ignoring_release_access(&response.body).map(Some)
    }

    /// Re-validate a token against the API, unconditionally. Most callers want the
    /// throttled, grace-period-aware
    /// [`Licensing::validate_token_online`](crate::Licensing::validate_token_online).
    pub fn validate_token_online(&self, token: &str) -> Result<License> {
        let response = self.post_token(&license_path(&self.options, "validate"), token)?;
        if !response.is_success() {
            return Err(self.error_for_license_problem(&response));
        }
        ensure_token_response(&response)?;
        self.validator.validate_token(&response.body)
    }

    /// Release this activation's seat on the server.
    pub fn revoke_activation(&self, token: &str) -> Result<()> {
        let response = self.post_token(&license_path(&self.options, "revoke"), token)?;
        if !response.is_success() {
            return Err(self.error_for_license_problem(&response));
        }
        // Success is an empty 200. A web page in its place means something in
        // between answered, and the seat may well still be taken.
        if looks_like_markup(&response.body) {
            return Err(ApiError::new(
                response.status,
                format!(
                    "Moonbase API answered HTTP {} with a web page; a proxy or captive portal may have intercepted \
                     the request",
                    response.status
                ),
            )
            .into());
        }
        Ok(())
    }

    /// Take a seat of one of the product's add-ons for the device the license token
    /// is bound to, and return that license again, reporting the add-on in
    /// `sub_products`. Neither stores it nor checks the token locally first;
    /// [`Licensing::activate_sub_product`](crate::Licensing::activate_sub_product)
    /// does both.
    ///
    /// Fails with [`Error::NoEligibleLicense`] when the customer has no seat of the
    /// add-on to give, [`Error::OperationNotSupported`] when it can't be activated
    /// from this license, and [`Error::LicenseInvalid`] or
    /// [`Error::LicenseExpired`] when the license itself is no longer good.
    pub fn activate_sub_product(&self, token: &str, sub_product_id: &str) -> Result<License> {
        self.post_for_sub_product(token, sub_product_id, "activate")
    }

    /// Start the add-on's trial on the device the license token is bound to (or
    /// take a seat of it, when the customer owns it), and return that license
    /// again. Fails as [`activate_sub_product`](Self::activate_sub_product) does.
    pub fn request_sub_product_trial(&self, token: &str, sub_product_id: &str) -> Result<License> {
        self.post_for_sub_product(token, sub_product_id, "trial")
    }

    /// The product's add-ons the customer can do something with, for the device
    /// the license or trial token is bound to. Changes nothing. Fails as
    /// [`activate_sub_product`](Self::activate_sub_product) does, and with an
    /// [`ApiError`] for an answer that isn't the add-on list.
    pub fn list_sub_products(&self, token: &str) -> Result<SubProductCatalog> {
        let request = build_request(
            &self.options,
            Method::Post,
            sub_products_path(&self.options),
            default_headers(&self.options, Some("text/plain")),
            token.to_owned(),
        );
        let response = self.transport.send(&request)?;
        if !response.is_success() {
            return Err(error_for_sub_product_problem(&response));
        }
        serde_json::from_str::<Value>(&response.body)
            .map_err(|error| error.to_string())
            .and_then(|value| parse_catalog(&value))
            .map_err(|error| ApiError::new(response.status, format!("Could not parse the add-on list: {error}")).into())
    }

    fn post_for_sub_product(&self, token: &str, sub_product_id: &str, action: &str) -> Result<License> {
        if sub_product_id.trim().is_empty() {
            return Err(Error::Configuration("An add-on id is required".into()));
        }
        let response = self.post_token(&sub_product_path(&self.options, sub_product_id, action), token)?;
        if !response.is_success() {
            return Err(error_for_sub_product_problem(&response));
        }
        ensure_token_response(&response)?;
        // Not judged against the app's version: the add-on's seat or trial is taken
        // by now, and the license is judged when it is validated.
        self.validator.validate_token_ignoring_release_access(&response.body)
    }

    /// The license endpoints' failures, as [`error_for_problem`] maps them, except
    /// that `ReleaseNotCovered` is read first: the license is good, just not for
    /// this version, and the problem says what it covers, where to upgrade and, on
    /// validation, carries the license freshly issued.
    fn error_for_license_problem(&self, response: &HttpResponse) -> Error {
        match problem_object(&response.body) {
            Some(problem) if problem_string(&problem, "errorType").as_deref() == Some("ReleaseNotCovered") => {
                self.release_not_covered(&problem)
            }
            _ => error_for_problem(response),
        }
    }

    fn release_not_covered(&self, problem: &Value) -> Error {
        // Checked like any license this client returns, except against the app's
        // version, which would only refuse it again. Best effort: the refusal stands
        // without it, and a token that fails verification is not one to hand the
        // app to store.
        let license = problem_string(problem, "license")
            .and_then(|token| self.validator.validate_token_ignoring_release_access(&token).ok());
        let app_version = problem_string(problem, "appVersion").unwrap_or_else(|| app_version_text(&self.options));
        let message = problem_string(problem, "detail")
            .unwrap_or_else(|| format!("The license doesn't cover version {app_version}"));
        Error::ReleaseNotCovered(Box::new(ReleaseNotCovered::new(
            message,
            license,
            app_version,
            release_access_from(problem),
            problem_string(problem, "upgradeUrl"),
        )))
    }

    fn post_token(&self, path: &str, token: &str) -> Result<HttpResponse> {
        let request = build_request(
            &self.options,
            Method::Post,
            append_query(path, &client_query(&self.options)),
            default_headers(&self.options, Some("text/plain")),
            token.to_owned(),
        );
        self.transport.send(&request)
    }
}

impl std::fmt::Debug for LicenseClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LicenseClient")
            .field("endpoint", &self.options.endpoint)
            .field("product_id", &self.options.product_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_info_is_sanitised_and_capped() {
        assert_eq!(sanitize_client_info("  a/1\r\nX-Injected: yes\t b/2  "), "a/1 X-Injected: yes b/2");
        assert_eq!(sanitize_client_info("\n\t "), "");
        assert_eq!(sanitize_client_info(&"x".repeat(1000)).len(), 256);
        assert!(sanitize_client_info(&"é".repeat(300)).len() <= 256);
    }

    #[test]
    fn poll_gate_spaces_each_request_separately() {
        let gate = ActivationPollGate::default();
        let start = Instant::now();
        assert!(gate.try_claim("a", start));
        assert!(!gate.try_claim("a", start + Duration::from_millis(500)));
        assert!(gate.try_claim("b", start + Duration::from_millis(500)));
        assert!(gate.try_claim("a", start + Duration::from_millis(1000)));
    }

    #[test]
    fn poll_gate_backs_off_everything_and_caps_the_wait() {
        let gate = ActivationPollGate::default();
        let start = Instant::now();
        gate.back_off(Duration::from_secs(3600), start);
        assert!(!gate.try_claim("a", start + Duration::from_secs(299)));
        assert!(gate.try_claim("a", start + Duration::from_secs(300)));
    }
}
