use std::sync::{Arc, Mutex, PoisonError};

use crate::client::LicenseClient;
use crate::crypto::base64_encode;
use crate::device_id::{DeviceIdDescription, DeviceIdResolver};
use crate::error::{Error, Result};
use crate::http::HttpTransport;
use crate::inventory::InventoryClient;
use crate::resolver::MoonbaseDeviceIdResolver;
use crate::store::{LicenseStore, MemoryLicenseStore};
use crate::sub_product::SubProductCatalog;
use crate::time::age;
use crate::types::{ActivationMethod, ActivationRequest, License, LicensingOptions};
use crate::url::trim_trailing_slashes;
use crate::validator::LicenseValidator;

/// The SDK's entry point: activation, validation, offline licenses, revocation,
/// add-ons and storage, for one product.
///
/// Every method blocks on I/O, so call them from a worker thread (or
/// `spawn_blocking`) in a UI or async app. `Licensing` is `Send + Sync`; share it
/// behind an `Arc`.
///
/// ```no_run
/// use std::{thread, time::Duration};
/// use moonbase::{ActivationMethod, Licensing, LicensingOptions};
///
/// # fn main() -> moonbase::Result<()> {
/// let licensing = Licensing::new(LicensingOptions::new(
///     "https://demo.moonbase.sh",
///     "demo-app",
///     include_str!("../tests/fixtures/signing-public.pem"),
/// ))?;
///
/// let request = licensing.request_activation(ActivationMethod::Online)?;
/// println!("Open {}", request.browser_url);
/// let license = loop {
///     thread::sleep(Duration::from_secs(2));
///     if let Some(license) = licensing.get_requested_activation(&request)? {
///         break license;
///     }
/// };
/// licensing.store().store_local_license(&license)?;
/// # Ok(())
/// # }
/// ```
pub struct Licensing {
    options: LicensingOptions,
    store: Arc<dyn LicenseStore>,
    device_ids: Arc<dyn DeviceIdResolver>,
    transport: Arc<dyn HttpTransport>,
    validator: Arc<LicenseValidator>,
    client: LicenseClient,
    validate_mutex: Mutex<()>,
}

/// Builds a [`Licensing`] with a custom store, device id resolver or transport.
#[must_use]
pub struct LicensingBuilder {
    options: LicensingOptions,
    store: Option<Arc<dyn LicenseStore>>,
    device_ids: Option<Arc<dyn DeviceIdResolver>>,
    transport: Option<Arc<dyn HttpTransport>>,
}

impl LicensingBuilder {
    /// Where the validated license is kept. Defaults to memory only: nothing is
    /// persisted unless you choose a store, such as a
    /// [`FileLicenseStore`](crate::FileLicenseStore). Pass the store itself, or an
    /// `Arc` of one you keep a handle to.
    pub fn store(mut self, store: impl LicenseStore + 'static) -> Self {
        self.store = Some(Arc::new(store));
        self
    }

    /// How this machine is identified. Defaults to
    /// [`MoonbaseDeviceIdResolver`], the cross-SDK fingerprint. Pass the resolver
    /// itself, or an `Arc` of one.
    pub fn device_id_resolver(mut self, resolver: impl DeviceIdResolver + 'static) -> Self {
        self.device_ids = Some(Arc::new(resolver));
        self
    }

    /// How requests are sent. Defaults to [`UreqTransport`](crate::UreqTransport)
    /// when the `ureq-transport` feature is on, and is required otherwise. Pass the
    /// transport itself, or an `Arc` of one.
    pub fn transport(mut self, transport: impl HttpTransport + 'static) -> Self {
        self.transport = Some(Arc::new(transport));
        self
    }

    pub fn build(self) -> Result<Licensing> {
        let mut options = self.options;
        options.endpoint = trim_trailing_slashes(&options.endpoint).to_owned();
        if options.endpoint.is_empty() {
            return Err(Error::Configuration("Moonbase endpoint is required".into()));
        }
        if options.product_id.is_empty() {
            return Err(Error::Configuration("Moonbase product_id is required".into()));
        }
        if options.public_key.is_empty() {
            return Err(Error::Configuration("Moonbase public_key is required".into()));
        }

        let store = self.store.unwrap_or_else(|| Arc::new(MemoryLicenseStore::new()));
        let device_ids = self.device_ids.unwrap_or_else(|| Arc::new(MoonbaseDeviceIdResolver::new()));
        let transport = match self.transport {
            Some(transport) => transport,
            None => default_transport()?,
        };
        let validator = Arc::new(LicenseValidator::new(&options, device_ids.clone())?);
        let client = LicenseClient::new(options.clone(), device_ids.clone(), validator.clone(), transport.clone());

        Ok(Licensing { options, store, device_ids, transport, validator, client, validate_mutex: Mutex::new(()) })
    }
}

#[cfg(feature = "ureq-transport")]
fn default_transport() -> Result<Arc<dyn HttpTransport>> {
    Ok(Arc::new(crate::http::UreqTransport::new()))
}

#[cfg(not(feature = "ureq-transport"))]
fn default_transport() -> Result<Arc<dyn HttpTransport>> {
    Err(Error::Configuration("An HTTP transport is required (built without the ureq-transport feature)".into()))
}

impl Licensing {
    /// A `Licensing` with the default in-memory store, device id resolver and
    /// transport.
    pub fn new(options: LicensingOptions) -> Result<Self> {
        Self::builder(options).build()
    }

    pub fn builder(options: LicensingOptions) -> LicensingBuilder {
        LicensingBuilder { options, store: None, device_ids: None, transport: None }
    }

    /// Start a browser activation. Send the user to the returned
    /// [`browser_url`](ActivationRequest::browser_url), then poll
    /// [`get_requested_activation`](Self::get_requested_activation) every 2 to 3
    /// seconds until it yields a license.
    ///
    /// Pass [`ActivationMethod::Offline`] to have the same browser flow mint an
    /// *offline* license: permanent, validated locally for good, and not
    /// revocable. Use it when the machine has network now but will not later. The
    /// product must allow offline activations, or this fails with
    /// [`Error::LicenseInvalid`] reading "Product does not allow offline
    /// activations".
    pub fn request_activation(&self, method: ActivationMethod) -> Result<ActivationRequest> {
        self.client.request_activation(method)
    }

    /// Poll a browser activation: `Ok(None)` while it is pending.
    ///
    /// The SDK sends at most one poll per second for each request; a call sooner
    /// returns `Ok(None)` without contacting the API, as does one before a
    /// `Retry-After` the server sent has passed (capped at 5 minutes).
    ///
    /// [`Error::ActivationRequestEnded`] means the request expired (after an hour),
    /// was cancelled, or was refused: stop polling and start a new one. An
    /// [`Error::Api`] is worth retrying on the next poll.
    pub fn get_requested_activation(&self, activation: &ActivationRequest) -> Result<Option<License>> {
        self.client.get_requested_activation(activation)
    }

    /// Poll a browser activation as
    /// [`get_requested_activation`](Self::get_requested_activation) does, and store
    /// the license once it lands: `Ok(None)` while it is pending, and the stored
    /// license after.
    ///
    /// ```no_run
    /// # fn run(licensing: &moonbase::Licensing) -> moonbase::Result<()> {
    /// use std::{thread, time::Duration};
    /// use moonbase::ActivationMethod;
    ///
    /// let request = licensing.request_activation(ActivationMethod::Online)?;
    /// println!("Open {}", request.browser_url);
    /// let license = loop {
    ///     thread::sleep(Duration::from_secs(2));
    ///     if let Some(license) = licensing.complete_activation(&request)? {
    ///         break license;
    ///     }
    /// };
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Stored under the store's lock, as a re-check stores its answer. Storing is
    /// best-effort, as it is there: the seat is taken by now, so the license is
    /// returned whether or not it reached the store.
    pub fn complete_activation(&self, activation: &ActivationRequest) -> Result<Option<License>> {
        self.complete_activation_with(activation, || true)
    }

    /// [`complete_activation`](Self::complete_activation), asking `should_persist`
    /// under the store's lock just before the write, e.g. so an activation the user
    /// cancelled meanwhile is not stored. The license is returned either way.
    pub fn complete_activation_with(
        &self,
        activation: &ActivationRequest,
        should_persist: impl Fn() -> bool,
    ) -> Result<Option<License>> {
        let Some(license) = self.client.get_requested_activation(activation)? else {
            return Ok(None);
        };
        self.store_under_lock(&license, should_persist);
        Ok(Some(license))
    }

    /// The stored license, validated as
    /// [`validate_token_online`](Self::validate_token_online) validates one: on the
    /// device, and online when it is due, storing the refreshed copy. `Ok(None)`
    /// when nothing is stored. What to do at startup.
    ///
    /// The stored license stays where it is whatever the answer, so the app
    /// decides what a refusal means:
    ///
    /// ```no_run
    /// # fn unlock(_: moonbase::License) {}
    /// # fn run(licensing: &moonbase::Licensing) -> moonbase::Result<()> {
    /// use moonbase::Error;
    ///
    /// match licensing.load_and_validate() {
    ///     Ok(Some(license)) => unlock(license),
    ///     Ok(None) => { /* not activated: offer to */ }
    ///     // Valid, but not for this version: keep it, and offer the upgrade.
    ///     Err(Error::ReleaseNotCovered(refusal)) => { /* refusal.upgrade_url() */ }
    ///     // Revoked, bound elsewhere, or a trial or subscription that ended.
    ///     Err(error) if error.is_license_rejection() => licensing.store().delete_local_license()?,
    ///     // No answer past the grace period: locked until it can be checked.
    ///     Err(error) => return Err(error),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn load_and_validate(&self) -> Result<Option<License>> {
        let Some(stored) = self.store.load_local_license()? else {
            return Ok(None);
        };
        self.validate_token_online(&stored.token).map(Some)
    }

    /// Read an offline license file's contents as
    /// [`read_offline_license`](Self::read_offline_license) does, and store the
    /// license. Whitespace around the token is ignored.
    ///
    /// A file for this device that doesn't cover `application_version` is stored
    /// too, since it still covers the versions it did; the
    /// [`Error::ReleaseNotCovered`] returned carries it. Storing is best-effort, as
    /// in [`complete_activation`](Self::complete_activation).
    pub fn import_offline_license(&self, contents: &str) -> Result<License> {
        match self.read_offline_license(contents.trim()) {
            Ok(license) => {
                self.store_under_lock(&license, || true);
                Ok(license)
            }
            Err(Error::ReleaseNotCovered(refusal)) => {
                if let Some(license) = refusal.license() {
                    self.store_under_lock(license, || true);
                }
                Err(Error::ReleaseNotCovered(refusal))
            }
            Err(error) => Err(error),
        }
    }

    /// Store a license under the locks a re-check holds, so it lands between other
    /// instances' checks rather than inside one. Best-effort: a license that is
    /// valid stays valid whether or not it reached the store.
    fn store_under_lock(&self, license: &License, should_persist: impl Fn() -> bool) {
        let _in_process = self.validate_mutex.lock().unwrap_or_else(PoisonError::into_inner);
        if let Ok(_cross_process) = self.store.lock_for_update()
            && should_persist()
        {
            let _ = self.store.store_local_license(license);
        }
    }

    /// Validate a token locally, with no network: signature, audience, issuer,
    /// device and expiry, and, for a license that covers only some versions,
    /// `application_version` ([`Error::ReleaseNotCovered`]).
    pub fn validate_token_local(&self, token: &str) -> Result<License> {
        self.validator.validate_token(token)
    }

    /// Like [`validate_token_local`](Self::validate_token_local), but neither a past
    /// `exp` nor a version the license doesn't cover is an error, so an aged-out
    /// token can still be inspected.
    pub fn validate_token_local_allow_expired(&self, token: &str) -> Result<License> {
        self.validator.validate_token_allow_expired(token)
    }

    /// The device token ("machine file") for the file-based offline activation: a
    /// base64 JSON descriptor of this device and product. Write it to a file
    /// (conventionally `.dt`) that the customer exchanges at
    /// `https://<tenant>.moonbase.sh/activate` for a license file, which
    /// [`read_offline_license`](Self::read_offline_license) then reads back.
    pub fn generate_device_token(&self) -> Result<String> {
        let payload = serde_json::json!({
            "id": self.device_ids.device_id()?,
            "name": self.device_ids.device_name(),
            "productId": self.options.product_id,
            // The Moonbase API expects this to always be "JWT".
            "format": "JWT",
        });
        Ok(base64_encode(payload.to_string().as_bytes()))
    }

    /// Read back the offline license the customer downloaded (the raw JWT in the
    /// license file). Validated locally, including `application_version`, and
    /// required to have been issued through offline activation. Offline licenses
    /// are never re-validated online.
    ///
    /// An [`Error::ReleaseNotCovered`] carries the license: this device's, for the
    /// versions it covers, so it is safe to store.
    pub fn read_offline_license(&self, token: &str) -> Result<License> {
        // The version is judged last, so a refusal for it only ever carries an
        // offline license for this device.
        let license = self.validator.validate_token_ignoring_release_access(token)?;
        if license.method != ActivationMethod::Offline {
            return Err(Error::LicenseInvalid("License token is not an offline-activated license".into()));
        }
        self.validator.ensure_covers_application_version(&license)?;
        Ok(license)
    }

    /// Validate a token locally, then re-validate it against the API when needed,
    /// and persist the refreshed license.
    ///
    /// - Validated more recently than `online_validation_min_interval`: the API is
    ///   skipped, so this is cheap to call often.
    /// - A failure that is not a verdict (offline, 5xx, rate limiting, a closed
    ///   store) falls back to the local result while the license is within
    ///   `online_validation_grace_period`, and propagates beyond it.
    /// - A verdict ([`Error::is_license_rejection`]) always propagates.
    /// - Offline-activated licenses never contact the API.
    /// - A license that doesn't cover `application_version` fails with
    ///   [`Error::ReleaseNotCovered`]. Moonbase judges that for an online license,
    ///   since it knows of an upgrade bought since the token was issued, which the
    ///   device cannot; the device judges it only when Moonbase isn't asked (an
    ///   offline license, the min-interval throttle, the grace period). When
    ///   Moonbase refused, the fresh copy it sent along is persisted first, as a
    ///   refreshed license is: it still covers the versions it did.
    ///
    /// Concurrent callers, in this process or in others sharing a
    /// [`FileLicenseStore`](crate::FileLicenseStore), take turns, and a later one
    /// picks up the license an earlier one just refreshed instead of asking again.
    pub fn validate_token_online(&self, token: &str) -> Result<License> {
        self.validate_token_online_with(token, || true)
    }

    /// [`validate_token_online`](Self::validate_token_online), asking
    /// `should_persist` immediately before writing the refreshed license, while the
    /// store lock is still held. Return false to skip the write but still get the
    /// refreshed license, e.g. when the user pressed "Deactivate" while this
    /// re-validation was in flight.
    pub fn validate_token_online_with(&self, token: &str, should_persist: impl Fn() -> bool) -> Result<License> {
        let local = self.validator.validate_token_ignoring_release_access(token)?;
        if local.method == ActivationMethod::Offline {
            self.validator.ensure_covers_application_version(&local)?;
            return Ok(local);
        }

        // Layered: a process-local mutex for same-process callers, the store's lock
        // across processes, and a reload under the lock so a waiter sees the fresh
        // validated_at the previous holder wrote and short-circuits on it.
        let _in_process = self.validate_mutex.lock().unwrap_or_else(PoisonError::into_inner);
        let _cross_process = self.store.lock_for_update()?;

        let mut freshest = local;
        // Two copies validated at the same time can still differ: an add-on call
        // answers with the activation's last validation time rather than a new one.
        // Nothing in them says which is current, so the server is asked.
        let mut undecided = false;
        // A corrupt or unparseable stored entry must not block validation.
        if let Ok(Some(stored)) = self.store.load_local_license()
            && stored.activation_id == freshest.activation_id
            && let Ok(candidate) = self.validator.validate_token_ignoring_release_access(&stored.token)
        {
            if candidate.validated_at > freshest.validated_at {
                freshest = candidate;
            } else if candidate.validated_at == freshest.validated_at && candidate.token != freshest.token {
                undecided = true;
            }
        }

        // Another instance stored an offline license for this activation since the
        // caller's token was issued. It is permanent and never checked online, so
        // it is judged here as an offline token passed in is, rather than sent
        // back to the API on the caller's older online token.
        if freshest.method == ActivationMethod::Offline {
            self.validator.ensure_covers_application_version(&freshest)?;
            return Ok(freshest);
        }

        let age = age(freshest.validated_at);
        let grace = self.options.online_validation_grace_period;
        // The throttle may only skip the API inside the grace period too, or a
        // min_interval longer than the grace period would silently extend it.
        if !undecided && age < self.options.online_validation_min_interval && age <= grace {
            self.validator.ensure_covers_application_version(&freshest)?;
            return Ok(freshest);
        }

        match self.client.validate_token_online(token) {
            Ok(refreshed) => {
                // Persist so sibling instances see the fresh validated_at. Best-effort:
                // we already have a valid license to return.
                if should_persist() {
                    let _ = self.store.store_local_license(&refreshed);
                }
                Ok(refreshed)
            }
            Err(Error::ReleaseNotCovered(refusal)) => {
                // The copy Moonbase sent along still covers the versions it did.
                if let Some(license) = refusal.license()
                    && should_persist()
                {
                    let _ = self.store.store_local_license(license);
                }
                Err(Error::ReleaseNotCovered(refusal))
            }
            Err(error) if error.is_license_rejection() => Err(error),
            // A closed store included: it is not signed, so it relaxes nothing.
            Err(_) if age <= grace => {
                self.validator.ensure_covers_application_version(&freshest)?;
                Ok(freshest)
            }
            Err(error) => Err(error),
        }
    }

    /// Take a seat of one of the product's add-ons for this device, from the
    /// license the device holds for the product, and return that license again,
    /// reporting the add-on in [`License::sub_products`]. The add-on must be
    /// included in the product's license. `sub_product_id` is the add-on's own part
    /// of its product ID (`drum-kit`) or its whole ID (`my-app.drum-kit`).
    ///
    /// For apps that take add-on seats themselves
    /// ([`auto_activate_sub_products`](LicensingOptions::auto_activate_sub_products)
    /// off), and for an add-on whose seat on this device was revoked
    /// ([`revoked`](crate::SubProduct::revoked)), which validating does not take
    /// back.
    ///
    /// The returned license is stored, as
    /// [`validate_token_online`](Self::validate_token_online) stores its own: under
    /// the same locks, held from before the request until the write, so another
    /// call here or in another instance can't store an older answer over it.
    ///
    /// Fails with [`Error::OperationNotSupported`] for a trial license without
    /// contacting the API, [`Error::NoEligibleLicense`] when the customer has no
    /// seat of the add-on to give (they don't own it, or every seat is taken), and
    /// [`Error::OperationNotSupported`] when the server refuses the add-on for this
    /// license. [`Error::LicenseInvalid`] or [`Error::LicenseExpired`] mean the
    /// license itself is no longer good. Other failures are [`Error::Api`] and
    /// worth retrying.
    pub fn activate_sub_product(&self, token: &str, sub_product_id: &str) -> Result<License> {
        self.activate_sub_product_with(token, sub_product_id, || true)
    }

    /// [`activate_sub_product`](Self::activate_sub_product), asking `should_persist`
    /// under the store's lock just before the write. Return false to skip it.
    pub fn activate_sub_product_with(
        &self,
        token: &str,
        sub_product_id: &str,
        should_persist: impl Fn() -> bool,
    ) -> Result<License> {
        self.require_license_for_sub_products(token)?;
        self.call_and_store(|| self.client.activate_sub_product(token, sub_product_id), should_persist)
    }

    /// Start the trial of one of the product's add-ons on this device, from the
    /// license the device holds for the product, and return that license again,
    /// reporting the trial in [`License::sub_products`]. The add-on must be
    /// included in the product's license and offer trials, and the license must be
    /// an online license rather than a trial. A customer who owns the add-on gets a
    /// seat of it instead, as [`activate_sub_product`](Self::activate_sub_product)
    /// does. A device gets one trial of each add-on: asking again returns the
    /// license with the trial it had, expired or not.
    ///
    /// Stores the result and fails as `activate_sub_product` does.
    pub fn request_sub_product_trial(&self, token: &str, sub_product_id: &str) -> Result<License> {
        self.request_sub_product_trial_with(token, sub_product_id, || true)
    }

    /// [`request_sub_product_trial`](Self::request_sub_product_trial), asking
    /// `should_persist` under the store's lock just before the write.
    pub fn request_sub_product_trial_with(
        &self,
        token: &str,
        sub_product_id: &str,
        should_persist: impl Fn() -> bool,
    ) -> Result<License> {
        self.require_license_for_sub_products(token)?;
        self.call_and_store(|| self.client.request_sub_product_trial(token, sub_product_id), should_persist)
    }

    /// List the product's add-ons for this device, from the license or trial the
    /// device holds for the product: each with its prices when it is for sale,
    /// whether the customer owns it, whether to unlock it here, and its trial,
    /// including whether
    /// [`request_sub_product_trial`](Self::request_sub_product_trial) can start one
    /// now. Changes nothing, so it can back a store or upgrade screen.
    ///
    /// Checks the token locally first. Fails as
    /// [`activate_sub_product`](Self::activate_sub_product) does, apart from the
    /// local trial check, and with an [`Error::Api`] for an answer that isn't the
    /// list.
    pub fn list_sub_products(&self, token: &str) -> Result<SubProductCatalog> {
        // Not against the app's version: add-ons are managed on the license
        // whatever version runs, as revoking is.
        self.validator.validate_token_ignoring_release_access(token)?;
        self.client.list_sub_products(token)
    }

    /// Add-ons are activated and tried from a license: the server refuses a trial,
    /// so this saves the round trip and says so plainly.
    fn require_license_for_sub_products(&self, token: &str) -> Result<()> {
        if self.validator.validate_token_ignoring_release_access(token)?.trial {
            return Err(Error::OperationNotSupported(
                "Add-ons are activated and tried from a license, not from a trial".into(),
            ));
        }
        Ok(())
    }

    /// Make an add-on call and store the license it returns, holding the locks
    /// `validate_token_online` holds from before the request until the write. The
    /// server marks an add-on call's answer with the activation's last validation
    /// time, not a new one, so the order answers reach the store in is the only
    /// order they have: holding the locks makes that the order the server gave them
    /// in. Storing is best-effort, as there: the license is good whether or not it
    /// reached the store.
    fn call_and_store(
        &self,
        call: impl FnOnce() -> Result<License>,
        should_persist: impl Fn() -> bool,
    ) -> Result<License> {
        let _in_process = self.validate_mutex.lock().unwrap_or_else(PoisonError::into_inner);
        let _cross_process = self.store.lock_for_update()?;
        let license = call()?;
        if should_persist() {
            let _ = self.store.store_local_license(&license);
        }
        Ok(license)
    }

    /// Free this device's seat: tells the server, then removes the matching license
    /// from the store.
    ///
    /// Only meaningful for online-activated paid licenses; offline and trial
    /// licenses fail with [`Error::OperationNotSupported`] without contacting the
    /// API. There is no grace-period fallback: revocation is one-shot. A token that
    /// has aged out locally can still be revoked, since its seat stays allocated
    /// until it is.
    pub fn revoke_activation(&self, token: &str) -> Result<()> {
        let local = self.validator.validate_token_allow_expired(token)?;
        if local.method == ActivationMethod::Offline {
            return Err(Error::OperationNotSupported("Offline-activated licenses cannot be revoked".into()));
        }
        if local.trial {
            return Err(Error::OperationNotSupported("Trial licenses cannot be revoked".into()));
        }

        self.client.revoke_activation(token)?;

        // Best-effort: the seat is already free, so a local I/O failure must not
        // read as a failed revoke. Under the update lock, so a sibling's in-flight
        // re-validation cannot write the license back between the load and the
        // delete.
        if let Ok(_guard) = self.store.lock_for_update()
            && let Ok(Some(stored)) = self.store.load_local_license()
            && stored.activation_id == local.activation_id
        {
            let _ = self.store.delete_local_license();
        }
        Ok(())
    }

    /// How this machine's device id was derived, for diagnostics: the id, spec
    /// version, platform and the *names* of the contributing parameters, never
    /// their values. `None` when a custom resolver does not describe itself.
    pub fn describe_device(&self) -> Result<Option<DeviceIdDescription>> {
        self.device_ids.describe_device()
    }

    /// A client for release notes and installer downloads, sharing this instance's
    /// options and transport.
    pub fn inventory(&self) -> InventoryClient {
        InventoryClient::new(self.options.clone(), self.transport.clone())
    }

    pub fn store(&self) -> &dyn LicenseStore {
        &*self.store
    }

    pub fn client(&self) -> &LicenseClient {
        &self.client
    }

    pub fn validator(&self) -> &LicenseValidator {
        &self.validator
    }

    pub fn device_resolver(&self) -> &dyn DeviceIdResolver {
        &*self.device_ids
    }

    /// The options in effect, with the endpoint normalized.
    pub fn options(&self) -> &LicensingOptions {
        &self.options
    }
}

impl std::fmt::Debug for LicensingBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LicensingBuilder")
            .field("endpoint", &self.options.endpoint)
            .field("product_id", &self.options.product_id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Licensing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Licensing")
            .field("endpoint", &self.options.endpoint)
            .field("product_id", &self.options.product_id)
            .finish_non_exhaustive()
    }
}
