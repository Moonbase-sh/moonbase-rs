//! The headless activation state machine behind the plugin's commands.
//!
//! Network and file work runs on worker threads. Every state-changing entry point
//! bumps a generation counter, and a worker's result only lands if the generation
//! is still the one it started under, so a slow request can never clobber a newer
//! state (a cancel, a fresh activation, a deactivation). Every change is emitted
//! as [`STATE_CHANGED_EVENT`] with the new [`LicenseState`].
//!
//! Once started it also watches the license with no network traffic: a license
//! whose `exp` passes or whose offline grace period runs out is re-checked (and
//! locked if it has ended) without a restart, and a license another instance or
//! process activates, refreshes or removes is picked up within a few seconds. A
//! change is emitted, too, when an add-on runs out while the license carries on,
//! and when a day goes by on a countdown to when the license or an add-on ends.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use moonbase::version::update_available;
use moonbase::{
    ActivationMethod, ActivationRequest, DownloadTarget, Error as SdkError, FileLicenseStore, License, LicenseStore,
    Licensing, Platform, ReleaseAccess, ReleaseNotCovered, can_download,
};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::error::{Error, Result};
use crate::models::{
    AddOnsPhase, AddOnsState, DeviceInfo, Failure, LicenseState, LicenseView, LockReason, NotCovered,
    PendingActivation, Status, UpdateHint, UpdateInfo,
};
use crate::ui::Ui;

/// Emitted with a [`LicenseState`] payload whenever anything in it changes.
pub const STATE_CHANGED_EVENT: &str = "moonbase://state-changed";

/// Poll every 2 seconds: the user is switching back from the browser, so faster
/// gains nothing (and the SDK holds each request to one poll per second anyway).
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// How long after opening an add-on's buy link a return to the app still counts
/// as coming back from the purchase.
const PURCHASE_WINDOW: Duration = Duration::from_secs(60 * 60);

pub(crate) struct Settings {
    pub grace_period: Duration,
    pub check_interval: Duration,
    pub application_version: Option<String>,
    pub open_browser: bool,
    pub auto_activate_sub_products: bool,
}

/// What the watch last saw of the license file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    exists: bool,
    len: u64,
    modified: Option<SystemTime>,
}

/// A license that is valid, but doesn't cover this version of the app.
#[derive(Debug, Clone)]
struct NotCoveredLicense {
    license: License,
    app_version: String,
    release_access: Option<ReleaseAccess>,
    upgrade_url: Option<String>,
}

impl NotCoveredLicense {
    fn from_refusal(refusal: &ReleaseNotCovered, fallback: &License) -> Self {
        let license = refusal.license().cloned().unwrap_or_else(|| fallback.clone());
        Self {
            app_version: refusal.app_version().to_owned(),
            release_access: refusal.release_access().cloned().or_else(|| license.release_access.clone()),
            upgrade_url: refusal.upgrade_url().map(str::to_owned),
            license,
        }
    }

    fn view(&self) -> NotCovered {
        NotCovered {
            license: LicenseView::from(&self.license),
            app_version: self.app_version.clone(),
            release_access: self.release_access.clone(),
            upgrade_url: self.upgrade_url.clone(),
        }
    }
}

/// Where this device stands with its license: one of these at a time, so no two
/// can contradict each other.
#[derive(Debug, Clone)]
enum Standing {
    /// No license, and why, when there was one.
    Unlicensed(Option<LockReason>),
    /// A license is in force.
    Licensed(License),
    /// A trial that ended, kept for display.
    TrialExpired(License),
    /// A license that doesn't cover this version of the app: kept, but locked.
    NotCovered(NotCoveredLicense),
}

impl Default for Standing {
    fn default() -> Self {
        Self::Unlicensed(None)
    }
}

impl Standing {
    /// The license in force.
    fn license(&self) -> Option<&License> {
        match self {
            Self::Licensed(license) => Some(license),
            _ => None,
        }
    }

    /// The license this device holds: the one in force, or one that doesn't cover
    /// this version of the app.
    fn held(&self) -> Option<&License> {
        match self {
            Self::Licensed(license) => Some(license),
            Self::NotCovered(not_covered) => Some(&not_covered.license),
            Self::Unlicensed(_) | Self::TrialExpired(_) => None,
        }
    }

    fn not_covered(&self) -> Option<&NotCoveredLicense> {
        match self {
            Self::NotCovered(not_covered) => Some(not_covered),
            _ => None,
        }
    }

    fn lock_reason(&self) -> Option<LockReason> {
        match self {
            Self::Unlicensed(reason) => *reason,
            Self::Licensed(_) => None,
            Self::TrialExpired(_) => Some(LockReason::Expired),
            Self::NotCovered(_) => Some(LockReason::NotCovered),
        }
    }
}

#[derive(Default)]
struct Inner {
    loading: bool,
    standing: Standing,
    /// From the click until the activation completes or is cancelled.
    activation: Option<PendingActivation>,
    pending: Option<ActivationRequest>,
    /// The generation of the deactivation in flight, so only that one clears it.
    deactivating: Option<u64>,
    error: Option<Failure>,
    refreshes_in_flight: usize,
    last_grace_check: Option<Instant>,
    file_stamp: Option<FileStamp>,
    /// Where to buy the upgrade the update screen last offered.
    update_upgrade_url: Option<String>,

    add_ons: AddOnsState,
    /// Bumped by every list, and when the license they were listed for goes, so a
    /// list that lands late is dropped.
    add_ons_generation: u64,
    /// Numbers each add-on activation or trial, and the one `busy_id` belongs to.
    add_on_calls: u64,
    add_on_call: u64,
    /// Add-ons that ran out and were re-checked for, once each.
    lapses_checked: HashSet<String>,
    /// Set once an add-on's buy link opened, until the app comes back to the front.
    purchase_opened_at: Option<Instant>,
    /// When the app last went to the background since.
    left_for_purchase: Option<Instant>,
    /// The ends the screens count down to, and the days to each as of the last
    /// watch tick.
    countdown: (Vec<SystemTime>, Vec<u64>),
}

struct Shared<R: Runtime> {
    app: AppHandle<R>,
    licensing: Arc<Licensing>,
    store: FileLicenseStore,
    settings: Settings,
    inner: Mutex<Inner>,
    generation: AtomicU64,
    licensed: AtomicBool,
    shutdown: AtomicBool,
    /// Taken around snapshot-and-send, so events leave in the order their
    /// snapshots were taken.
    emit_lock: Mutex<()>,
    /// Held by a re-check and an add-on call for the whole of their work, from the
    /// request until the answer is applied, so answers land here in the order they
    /// reached the store. The store's own lock, which they also hold from the
    /// request until the write, orders them against other instances.
    online_calls: Mutex<()>,
    /// The active add-ons as last emitted, so the watch can tell when one ran out.
    emitted_sub_products: Mutex<Vec<String>>,
    /// The built-in activation window, unless the app turned it off.
    ui: Option<Ui>,
}

/// The plugin's license controller, managed as Tauri state. Reach it from Rust
/// with [`MoonbaseExt::moonbase`](crate::MoonbaseExt::moonbase) to gate features or
/// drive activation without the webview.
///
/// Every method that talks to Moonbase or the disk blocks; call those from a
/// worker thread or `spawn_blocking`. [`state`](Self::state),
/// [`is_licensed`](Self::is_licensed), [`license`](Self::license) and
/// [`has_sub_product`](Self::has_sub_product) never block on I/O.
pub struct Moonbase<R: Runtime>(Arc<Shared<R>>);

impl<R: Runtime> Clone for Moonbase<R> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

enum Activity {
    StartActivation,
    Activation,
    Deactivation,
    OfflineActivation,
}

fn busy_status(status: u16) -> bool {
    status == 408 || status == 429 || status >= 500
}

fn failure(activity: Activity, error: &SdkError) -> Failure {
    let message = match (activity, error) {
        (_, SdkError::InsufficientDeviceIdentity { .. }) => {
            "This computer can't be identified, so it can't be activated. Contact the developer for help.".into()
        }
        (Activity::Deactivation, SdkError::StoreClosed(_)) => {
            "This store has closed, so this computer can't be deactivated.".into()
        }
        (_, SdkError::StoreClosed(_)) => "This store has closed, so activation isn't available.".into(),

        (Activity::StartActivation, SdkError::Api(api)) if busy_status(api.status_code()) => {
            "Moonbase couldn't start activation right now. Try again in a minute.".into()
        }
        (Activity::StartActivation, SdkError::Api(_)) => {
            "Couldn't reach Moonbase to start activation. Check your internet connection and try again.".into()
        }
        (Activity::StartActivation, error) if error.is_license_rejection() => {
            "Activation isn't available for this product right now. If this keeps happening, contact the developer."
                .into()
        }

        (Activity::Activation, SdkError::ActivationRequestEnded(_)) => {
            "This activation expired or was cancelled. Activate again to continue.".into()
        }
        (Activity::Activation, SdkError::LicenseExpired(_)) => {
            "This license has expired, so it can't be activated.".into()
        }
        (Activity::Activation, error) if error.is_license_invalid() => {
            "Activation was rejected. If this keeps happening, contact the developer.".into()
        }

        (Activity::Deactivation, SdkError::Api(api)) if busy_status(api.status_code()) => {
            "Moonbase couldn't deactivate right now. Try again in a minute.".into()
        }
        (Activity::Deactivation, SdkError::Api(_)) => {
            "Couldn't reach Moonbase to deactivate. Try again when you're online.".into()
        }

        (Activity::OfflineActivation, SdkError::ReleaseNotCovered(_)) => {
            "That license doesn't cover this version.".into()
        }
        (Activity::OfflineActivation, _) => "That license file isn't valid for this device.".into(),

        // Nobody anticipated it, so the most useful thing is its own words.
        (Activity::StartActivation, error) => format!("Activation couldn't start: {error}"),
        (Activity::Activation, error) => format!("Activation failed: {error}"),
        (Activity::Deactivation, error) => format!("Couldn't deactivate: {error}"),
    };
    Failure { message, detail: error.to_string(), kind: Some(error.error_type()) }
}

/// Copy for an add-on request that got no answer about the add-on: the
/// connection, Moonbase, or this machine. `lead` says what failed ("Couldn't
/// activate Drum Kit"), and `not_found` what a 404 means for this request.
fn add_on_failure(error: &SdkError, lead: &str, not_found: &str) -> String {
    match error {
        SdkError::Api(api) if api.status_code() == 404 => not_found.into(),
        SdkError::Api(api) if busy_status(api.status_code()) => {
            "Moonbase couldn't do that right now. Try again in a minute.".into()
        }
        SdkError::Api(_) => "Couldn't reach Moonbase. Check your internet connection and try again.".into(),
        SdkError::StoreClosed(_) => "This store has closed, so add-ons aren't available.".into(),
        SdkError::InsufficientDeviceIdentity { .. } => {
            "This computer can't be identified. Contact the developer for help.".into()
        }
        error if error.is_license_rejection() || matches!(error, SdkError::Configuration(_)) => {
            format!("{lead}. If this keeps happening, contact the developer.")
        }
        error => format!("{lead}: {error}"),
    }
}

/// Activation can never complete after these; anything else is retried.
fn ends_activation(error: &SdkError) -> bool {
    error.is_license_rejection()
        || matches!(
            error,
            SdkError::ActivationRequestEnded(_)
                | SdkError::StoreClosed(_)
                | SdkError::InsufficientDeviceIdentity { .. }
                | SdkError::Configuration(_)
        )
}

fn age(time: SystemTime) -> Duration {
    SystemTime::now().duration_since(time).unwrap_or(Duration::ZERO)
}

/// Whole days from now until `when`, rounded up, and 0 once it has passed.
fn days_until(when: SystemTime) -> u64 {
    when.duration_since(SystemTime::now()).map_or(0, |left| left.as_secs().div_ceil(86_400))
}

/// The add-ons to unlock on this device as of `now`, by their own part of their
/// product ID.
fn active_sub_products(license: &License, now: SystemTime) -> Vec<String> {
    let mut active: Vec<String> = Vec::new();
    for entry in &license.sub_products {
        if license.has_sub_product_at(&entry.id, now) && !active.contains(&entry.id) {
            active.push(entry.id.clone());
        }
    }
    active
}

/// What validating the stored license at startup concluded.
#[derive(Default)]
struct StartupOutcome {
    license: Option<License>,
    expired_trial: Option<License>,
    not_covered: Option<NotCoveredLicense>,
    delete_expired_offline: bool,
    reason: Option<LockReason>,
}

/// Take a stored license judged on this device alone, as the SDK's own fallbacks
/// take one: only when it covers this version of the app. One that doesn't stays
/// on disk, and locks.
fn accept_locally(licensing: &Licensing, license: License, outcome: &mut StartupOutcome) {
    match licensing.validator().ensure_covers_application_version(&license) {
        Ok(()) => outcome.license = Some(license),
        Err(SdkError::ReleaseNotCovered(refusal)) => {
            // Kept on disk: it still covers the versions it did.
            log::info!("moonbase: the stored license doesn't cover this version: {refusal}");
            outcome.not_covered = Some(NotCoveredLicense::from_refusal(&refusal, &license));
        }
        Err(error) => {
            log::warn!("moonbase: stored license rejected: {error}");
            outcome.reason = Some(LockReason::Invalid);
        }
    }
}

fn check_stored_license(
    licensing: &Licensing,
    grace_period: Duration,
    still_current: impl Fn() -> bool,
) -> StartupOutcome {
    let mut outcome = StartupOutcome::default();
    let stored = match licensing.store().load_local_license() {
        Ok(Some(stored)) => stored,
        Ok(None) => return outcome,
        Err(error) => {
            log::warn!("moonbase: loading the stored license failed: {error}");
            return outcome;
        }
    };

    // Peek without failing on a past `exp`, to tell an expired offline license
    // (dead for good) from an online one (refreshable) and from one that cannot be
    // trusted at all.
    let peek = match licensing.validate_token_local_allow_expired(&stored.token) {
        Ok(peek) => peek,
        Err(SdkError::InsufficientDeviceIdentity { .. }) => {
            // Nothing is wrong with the token: this machine cannot identify itself.
            log::warn!("moonbase: could not identify this device, so the stored license could not be checked");
            return outcome;
        }
        Err(error) => {
            // Tampered, or bound to another device: locked, but left on disk.
            log::warn!("moonbase: stored license rejected: {error}");
            outcome.reason = Some(LockReason::Invalid);
            return outcome;
        }
    };

    if peek.method == ActivationMethod::Offline {
        if peek.is_expired() {
            log::info!("moonbase: the stored offline license has expired; removing it");
            outcome.delete_expired_offline = true;
            outcome.reason = Some(LockReason::Expired);
        } else {
            accept_locally(licensing, peek, &mut outcome);
        }
        return outcome;
    }

    // Expired or not: one that has may have been renewed while the app was closed,
    // which the SDK asks Moonbase about before it says so. Persist the refresh only
    // while nothing has superseded the check, so a license the user cleared
    // meanwhile is not written back.
    match licensing.validate_token_online_with(&stored.token, still_current) {
        Ok(license) => outcome.license = Some(license),
        Err(SdkError::ReleaseNotCovered(refusal)) => {
            // Good, but not for this version: locked, and kept on disk (the SDK
            // already stored the copy Moonbase sent).
            log::info!("moonbase: the stored license doesn't cover this version: {refusal}");
            outcome.not_covered = Some(NotCoveredLicense::from_refusal(&refusal, &peek));
        }
        Err(SdkError::LicenseExpired(_)) => {
            outcome.reason = Some(LockReason::Expired);
            if peek.trial {
                outcome.expired_trial = Some(peek);
            }
        }
        Err(error) if error.is_license_invalid() => {
            log::warn!("moonbase: re-validating the stored license failed: {error}");
            outcome.reason = Some(LockReason::Invalid);
        }
        Err(SdkError::Storage(error)) => {
            // Our own file lock failed, which says nothing about the license: treat
            // it like being offline, as the SDK's fallback within the grace period
            // does, version check included. (The token passed local validation, its
            // expiry and device binding, to get here.)
            log::warn!("moonbase: re-validating the stored license failed: {error}");
            if age(peek.validated_at) <= grace_period {
                accept_locally(licensing, peek, &mut outcome);
            } else {
                outcome.reason = Some(LockReason::Unverified);
            }
        }
        Err(error) => {
            // Unreachable past the grace period.
            log::warn!("moonbase: re-validating the stored license failed: {error}");
            outcome.reason = Some(LockReason::Unverified);
        }
    }
    outcome
}

/// What an add-on activation or trial came to, once applied.
enum AddOnOutcome {
    Applied {
        relist: bool,
    },
    /// The license it was asked from was deactivated, cleared or replaced since.
    Changed,
    Failed {
        error: SdkError,
        recheck: bool,
    },
}

const LICENSE_CHANGED: &str = "Your license changed while this was in progress.";

impl<R: Runtime> Moonbase<R> {
    pub(crate) fn new(
        app: AppHandle<R>,
        licensing: Licensing,
        store: FileLicenseStore,
        settings: Settings,
        ui: Option<Ui>,
    ) -> Self {
        Self(Arc::new(Shared {
            app,
            licensing: Arc::new(licensing),
            store,
            settings,
            inner: Mutex::new(Inner::default()),
            generation: AtomicU64::new(0),
            licensed: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            emit_lock: Mutex::new(()),
            online_calls: Mutex::new(()),
            emitted_sub_products: Mutex::default(),
            ui,
        }))
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.0.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn online_turn(&self) -> MutexGuard<'_, ()> {
        self.0.online_calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) -> u64 {
        self.0.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn is_current(&self, generation: u64) -> bool {
        self.0.generation.load(Ordering::SeqCst) == generation
    }

    fn emit(&self) {
        let _ordered = self.0.emit_lock.lock().unwrap_or_else(PoisonError::into_inner);
        let state = self.state();
        state
            .active_sub_products
            .clone_into(&mut self.0.emitted_sub_products.lock().unwrap_or_else(PoisonError::into_inner));
        if let Err(error) = self.0.app.emit(STATE_CHANGED_EVENT, &state) {
            log::warn!("moonbase: could not emit the license state: {error}");
        }
        self.after_state_change(&state);
    }

    pub(crate) fn app(&self) -> &AppHandle<R> {
        &self.0.app
    }

    pub(crate) fn ui(&self) -> Option<&Ui> {
        self.0.ui.as_ref()
    }

    /// The SDK instance behind the plugin, for anything the plugin does not wrap.
    pub fn licensing(&self) -> &Licensing {
        &self.0.licensing
    }

    /// Is there a valid license right now? Lock-free, so cheap enough to check
    /// anywhere you gate a feature.
    pub fn is_licensed(&self) -> bool {
        self.0.licensed.load(Ordering::Acquire)
    }

    /// The license in force, if any.
    pub fn license(&self) -> Option<License> {
        self.inner().standing.license().cloned()
    }

    /// Whether to unlock an add-on on this device: the license in force lists it
    /// as active and it hasn't expired. Takes the add-on's own part of its product
    /// ID (`"drum-kit"`) or its whole ID (`"my-app.drum-kit"`). False while no
    /// license is in force.
    pub fn has_sub_product(&self, sub_product_id: &str) -> bool {
        self.inner().standing.license().is_some_and(|license| license.has_sub_product(sub_product_id))
    }

    /// The add-ons [`has_sub_product`](Self::has_sub_product) is true for right
    /// now, by their own part of their product ID.
    pub fn active_sub_products(&self) -> Vec<String> {
        self.inner()
            .standing
            .license()
            .map(|license| active_sub_products(license, SystemTime::now()))
            .unwrap_or_default()
    }

    /// Where the license is kept.
    pub fn license_file(&self) -> PathBuf {
        self.0.store.path().to_owned()
    }

    /// A snapshot of the license state, as the UI sees it.
    pub fn state(&self) -> LicenseState {
        let inner = self.inner();
        let status = if inner.loading {
            Status::Loading
        } else if inner.activation.is_some() {
            Status::Activating
        } else {
            match &inner.standing {
                Standing::Licensed(license) if license.trial => Status::Trial,
                Standing::Licensed(_) => Status::Licensed,
                Standing::TrialExpired(_) => Status::TrialExpired,
                Standing::NotCovered(_) => Status::NotCovered,
                Standing::Unlicensed(_) => Status::Unlicensed,
            }
        };
        let license = inner.standing.license();
        let update = match (license, &self.0.settings.application_version) {
            (Some(license), Some(current)) => license
                .licensed_product
                .current_release_version
                .as_ref()
                .filter(|latest| update_available(current, latest))
                .map(|latest| UpdateHint { current_version: current.clone(), latest_version: latest.clone() }),
            _ => None,
        };
        LicenseState {
            status,
            licensed: license.is_some(),
            license: license.map(LicenseView::from),
            expired_trial: match &inner.standing {
                Standing::TrialExpired(trial) => Some(LicenseView::from(trial)),
                _ => None,
            },
            not_covered: inner.standing.not_covered().map(NotCoveredLicense::view),
            lock_reason: inner.standing.lock_reason(),
            activation: inner.activation.clone(),
            busy: inner.deactivating.is_some(),
            refreshing: inner.refreshes_in_flight > 0,
            error: inner.error.clone(),
            update,
            active_sub_products: license
                .map(|license| active_sub_products(license, SystemTime::now()))
                .unwrap_or_default(),
            add_ons: inner.add_ons.clone(),
        }
    }

    /// Whether `copy` is an older copy of the activation held, licensed or not
    /// covered.
    fn is_older_than_held(inner: &Inner, copy: &License) -> bool {
        inner
            .standing
            .held()
            .is_some_and(|held| held.activation_id == copy.activation_id && copy.validated_at < held.validated_at)
    }

    fn reset_add_ons(inner: &mut Inner) {
        inner.add_ons_generation += 1;
        inner.add_ons = AddOnsState::default();
        inner.add_on_call = 0;
        inner.lapses_checked.clear();
        inner.purchase_opened_at = None;
        inner.left_for_purchase = None;
    }

    fn set_standing(&self, inner: &mut Inner, standing: Standing) {
        let same_activation = matches!(
            (inner.standing.license(), standing.license()),
            (Some(current), Some(next)) if current.activation_id == next.activation_id
        );
        self.0.licensed.store(standing.license().is_some(), Ordering::Release);
        // The add-ons were listed for the activation that just went or was
        // replaced, along with anything still on its way for it.
        if !same_activation {
            Self::reset_add_ons(inner);
        }
        inner.standing = standing;
    }

    fn set_license(&self, inner: &mut Inner, license: License) {
        self.set_standing(inner, Standing::Licensed(license));
    }

    /// Drop the license and say why. The file stays, so the next launch checks it
    /// again.
    fn lock(&self, inner: &mut Inner, reason: Option<LockReason>) {
        self.set_standing(inner, Standing::Unlicensed(reason));
    }

    fn show_trial_expired(&self, inner: &mut Inner, trial: License) {
        self.set_standing(inner, Standing::TrialExpired(trial));
    }

    /// Lock like an ended trial, but keep the license stored: it still covers the
    /// versions it did, and an upgrade makes it cover this one.
    fn show_not_covered(&self, inner: &mut Inner, not_covered: NotCoveredLicense) {
        self.set_standing(inner, Standing::NotCovered(not_covered));
        inner.error = None;
    }

    /// Stop polling and drop everything in flight: their results land on nothing.
    fn end_activation_flows(&self, inner: &mut Inner) {
        inner.activation = None;
        inner.pending = None;
        inner.deactivating = None;
        Self::reset_add_ons(inner);
        self.bump();
    }

    fn stamp_file(&self) -> FileStamp {
        match std::fs::metadata(self.0.store.path()) {
            Ok(metadata) if metadata.is_file() => {
                FileStamp { exists: true, len: metadata.len(), modified: metadata.modified().ok() }
            }
            _ => FileStamp { exists: false, len: 0, modified: None },
        }
    }

    /// Whether the store still holds this activation. If it no longer does, another
    /// instance deactivated or replaced it after a request went out, and writing
    /// that request's answer would bring a removed license back.
    fn stores_activation(store: &dyn LicenseStore, activation_id: &str) -> bool {
        store.load_local_license().ok().flatten().is_some_and(|stored| stored.activation_id == activation_id)
    }

    fn delete_stored(&self, inner: &mut Inner, only_activation: Option<&str>) {
        let store = self.0.licensing.store();
        let result = store.lock_for_update().and_then(|_guard| match only_activation {
            Some(activation_id) => match store.load_local_license()? {
                Some(stored) if stored.activation_id == activation_id => store.delete_local_license(),
                _ => Ok(()),
            },
            None => store.delete_local_license(),
        });
        if let Err(error) = result {
            log::warn!("moonbase: couldn't delete the stored license: {error}");
        }
        inner.file_stamp = Some(self.stamp_file());
    }

    pub(crate) fn start(&self) {
        {
            let mut inner = self.inner();
            inner.loading = true;
            // The first look is taken before the load, so a change that lands
            // while it runs is still seen.
            inner.file_stamp = Some(self.stamp_file());
        }
        let generation = self.bump();

        let this = self.clone();
        thread::spawn(move || {
            let outcome =
                check_stored_license(&this.0.licensing, this.0.settings.grace_period, || this.is_current(generation));
            let mut inner = this.inner();
            inner.loading = false;
            // Superseded: the user did something while the check ran.
            if this.is_current(generation) {
                if outcome.delete_expired_offline {
                    this.delete_stored(&mut inner, None);
                }
                if let Some(trial) = outcome.expired_trial {
                    this.show_trial_expired(&mut inner, trial);
                } else if let Some(not_covered) = outcome.not_covered {
                    this.show_not_covered(&mut inner, not_covered);
                } else if let Some(license) = outcome.license {
                    this.set_license(&mut inner, license);
                } else {
                    this.lock(&mut inner, outcome.reason);
                }
            }
            drop(inner);
            this.emit();
        });

        let this = self.clone();
        thread::spawn(move || {
            while !this.0.shutdown.load(Ordering::Acquire) {
                thread::sleep(WATCH_INTERVAL);
                this.watch();
            }
        });
    }

    pub(crate) fn shut_down(&self) {
        self.0.shutdown.store(true, Ordering::Release);
        self.bump();
        // A worker waiting its turn on the license lock while another instance
        // re-validates stops waiting, rather than hold up the exit.
        self.0.store.cancel_lock_waits();
    }

    /// Start a browser activation: create the request, open the browser, and poll
    /// in the background until it completes. Returns once the request exists, with
    /// its link in [`LicenseState::activation`].
    ///
    /// [`ActivationMethod::Offline`] mints an offline license through the same
    /// browser flow, for a machine that has network now but will not later.
    pub fn activate(&self, method: ActivationMethod) -> Result<LicenseState> {
        let generation = {
            let mut inner = self.inner();
            if inner.deactivating.is_some() {
                return Err(Error::Busy("A deactivation is in progress".into()));
            }
            inner.activation = Some(PendingActivation { browser_url: None, method, browser_open_failed: false });
            inner.pending = None;
            inner.error = None;
            self.bump()
        };
        self.emit();

        let result = self.0.licensing.request_activation(method);

        let mut inner = self.inner();
        if !self.is_current(generation) {
            drop(inner);
            return Ok(self.state());
        }
        let request = match result {
            Ok(request) => request,
            Err(error) => {
                log::warn!("moonbase: request_activation failed: {error}");
                inner.activation = None;
                inner.error = Some(failure(Activity::StartActivation, &error));
                drop(inner);
                self.emit();
                return Err(error.into());
            }
        };
        inner.pending = Some(request.clone());
        inner.activation = Some(PendingActivation {
            browser_url: Some(request.browser_url.clone()),
            method,
            browser_open_failed: false,
        });
        drop(inner);

        if self.0.settings.open_browser
            && let Err(error) = self.open_url(&request.browser_url)
        {
            // Windows with no default browser set, say. The screen then offers the
            // link to copy, for any browser on this computer or another.
            log::warn!("moonbase: couldn't open the browser for activation ({error}); the state carries the link");
            let mut inner = self.inner();
            if self.is_current(generation)
                && let Some(activation) = &mut inner.activation
            {
                activation.browser_open_failed = true;
            }
        }
        self.spawn_poll(generation, request);
        self.emit();
        Ok(self.state())
    }

    /// Open a link in the browser: through the opener plugin's own handle when the
    /// app registered it (which also covers mobile), and the desktop default
    /// browser otherwise.
    pub(crate) fn open_url(&self, url: &str) -> std::result::Result<(), String> {
        let opened = match self.0.app.try_state::<tauri_plugin_opener::Opener<R>>() {
            Some(opener) => opener.open_url(url, None::<String>),
            None => tauri_plugin_opener::open_url(url, None::<&str>),
        };
        opened.map_err(|error| error.to_string())
    }

    fn spawn_poll(&self, generation: u64, request: ActivationRequest) {
        let this = self.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(POLL_INTERVAL);
                if this.0.shutdown.load(Ordering::Acquire) || !this.is_current(generation) {
                    return;
                }
                // Stored by the SDK under the store's lock, unless superseded first.
                match this.0.licensing.complete_activation_with(&request, || this.is_current(generation)) {
                    Ok(None) => {}
                    Ok(Some(license)) => return this.on_activated(generation, license),
                    Err(error) if ends_activation(&error) => {
                        log::warn!("moonbase: activation rejected during polling: {error}");
                        let mut inner = this.inner();
                        if this.is_current(generation) {
                            inner.activation = None;
                            inner.pending = None;
                            inner.error = Some(failure(Activity::Activation, &error));
                        }
                        drop(inner);
                        return this.emit();
                    }
                    // Unreachable, rate limited, failing: keep waiting.
                    Err(error) => log::debug!("moonbase: activation poll failed, still waiting: {error}"),
                }
            }
        });
    }

    fn on_activated(&self, generation: u64, license: License) {
        let mut inner = self.inner();
        if !self.is_current(generation) {
            return;
        }
        self.end_activation_flows(&mut inner);
        inner.file_stamp = Some(self.stamp_file());
        self.set_license(&mut inner, license);
        inner.error = None;
        drop(inner);
        self.emit();
    }

    /// Stop waiting for a browser activation. A license held from before stays.
    pub fn cancel_activation(&self) -> LicenseState {
        let mut inner = self.inner();
        // Nothing to cancel: bumping the generation would supersede unrelated work.
        if inner.activation.is_none() {
            return self.state_with(inner);
        }
        inner.activation = None;
        inner.pending = None;
        inner.error = None;
        self.bump();
        drop(inner);
        self.emit();
        self.state()
    }

    /// Re-check the license against Moonbase and refresh its entitlements in
    /// place: call it after a purchase so new features and add-ons load without a
    /// restart. `force` skips the online-validation throttle.
    ///
    /// A revoked or lapsed license locks, and so does one that cannot be checked
    /// once the offline grace period has run out; within it a network failure
    /// keeps the license and returns the error. No-op for offline licenses, and
    /// while an activation is in progress (it brings a fresh license of its own).
    ///
    /// With [`Status::NotCovered`] it re-checks the license that doesn't cover this
    /// version, so once the customer has bought the upgrade the app unlocks.
    pub fn refresh(&self, force: bool) -> Result<LicenseState> {
        let (generation, current, not_covered_before) = {
            let mut inner = self.inner();
            if inner.activation.is_some() {
                return Ok(self.state_with(inner));
            }
            if inner.deactivating.is_some() {
                return Err(Error::Busy("A deactivation is in progress".into()));
            }
            let Some(current) = inner.standing.held().cloned() else {
                return Err(Error::NotLicensed);
            };
            if current.method == ActivationMethod::Offline {
                return Ok(self.state_with(inner));
            }
            inner.refreshes_in_flight += 1;
            (self.bump(), current, inner.standing.license().is_none())
        };
        if not_covered_before {
            self.emit(); // `refreshing` shows on the upgrade screen
        }

        let _turn = self.online_turn();
        let store = self.0.licensing.store();
        // Asked under the store's lock, just before an answer is stored: not when
        // this check was superseded (deactivate and forget move the generation on
        // first, and delete under the same lock after), and not over another
        // activation stored meanwhile.
        let still_current = || self.is_current(generation) && Self::stores_activation(store, &current.activation_id);

        let result = if force {
            // Past the throttle, straight to the API, but holding the store's lock
            // from the request until the answer is stored, as the SDK does for its
            // own checks: answers then reach the store in the order the server gave
            // them, whichever instance asked.
            store.lock_for_update().and_then(|_guard| {
                let result = self.0.licensing.client().validate_token_online(&current.token);
                let answer = match &result {
                    Ok(refreshed) => Some(refreshed),
                    // The copy Moonbase sent with the refusal still covers the
                    // versions it did, so it is stored the same way.
                    Err(SdkError::ReleaseNotCovered(refusal)) => refusal.license(),
                    Err(_) => None,
                };
                if let Some(answer) = answer
                    && still_current()
                    && let Err(error) = store.store_local_license(answer)
                {
                    log::warn!("moonbase: couldn't persist the refreshed license: {error}");
                }
                result
            })
        } else {
            // Throttled and grace-period aware, and stores what it got the same way.
            self.0.licensing.validate_token_online_with(&current.token, still_current)
        };

        let mut inner = self.inner();
        inner.refreshes_in_flight -= 1;
        if !self.is_current(generation) {
            drop(inner);
            if not_covered_before {
                self.emit(); // `refreshing` may have cleared
            }
            return Ok(self.state());
        }
        // A throttled check can answer with the very license it was asked about.
        // When the license held here has moved on since (an add-on call landed,
        // say), that answer is the older of the two: keep what is held. A forced
        // check always reached the server, after anything held here, so its answer
        // applies even when it matches the license it was asked about.
        if let Ok(refreshed) = &result
            && !force
            && refreshed.token == current.token
            && inner
                .standing
                .license()
                .is_some_and(|held| held.activation_id == refreshed.activation_id && held.token != current.token)
        {
            return Ok(self.state_with(inner));
        }
        let outcome = match result {
            Ok(refreshed) => {
                inner.file_stamp = Some(self.stamp_file());
                if not_covered_before {
                    inner.error = None;
                }
                self.set_license(&mut inner, refreshed);
                Ok(())
            }
            Err(SdkError::ReleaseNotCovered(refusal)) => {
                let not_covered = NotCoveredLicense::from_refusal(&refusal, &current);
                if Self::is_older_than_held(&inner, &not_covered.license) {
                    // A newer copy arrived while this answer was on its way, such
                    // as a sibling's after the upgrade, and outranks it.
                    log::debug!("moonbase: dropped a re-validation answer older than the license held: {refusal}");
                } else {
                    log::info!("moonbase: the license doesn't cover this version on re-validation: {refusal}");
                    self.show_not_covered(&mut inner, not_covered);
                }
                Ok(())
            }
            Err(SdkError::LicenseExpired(message)) if current.trial => {
                log::info!("moonbase: the trial ended on re-validation: {message}");
                self.show_trial_expired(&mut inner, current);
                Ok(())
            }
            Err(error) if !error.is_license_rejection() && inner.standing.not_covered().is_some() => {
                // "Check again" on the upgrade screen got no answer. The app is
                // locked already, so it just says so and stays put.
                log::warn!("moonbase: couldn't re-check the license that doesn't cover this version: {error}");
                inner.error = Some(Failure {
                    message: "Couldn't reach Moonbase to check your license. Try again when you're online.".into(),
                    detail: error.to_string(),
                    kind: Some(error.error_type()),
                });
                Err(error)
            }
            Err(error) if error.is_license_rejection() => {
                log::warn!("moonbase: license rejected on re-validation: {error}");
                let reason = if matches!(error, SdkError::LicenseExpired(_)) {
                    LockReason::Expired
                } else {
                    LockReason::Invalid
                };
                self.lock(&mut inner, Some(reason));
                Ok(())
            }
            Err(error) if age(current.validated_at) > self.0.settings.grace_period => {
                log::warn!("moonbase: couldn't re-validate within the offline grace period: {error}");
                self.lock(&mut inner, Some(LockReason::Unverified));
                Ok(())
            }
            // A network blip must not lock anyone out.
            Err(error) => Err(error),
        };
        drop(inner);
        self.emit();
        outcome.map(|()| self.state()).map_err(Error::from)
    }

    fn state_with(&self, inner: MutexGuard<'_, Inner>) -> LicenseState {
        drop(inner);
        self.state()
    }

    /// Free this device's seat and lock. Offline and trial licenses cannot be
    /// revoked, so those are forgotten locally instead. When Moonbase cannot be
    /// reached the license is kept, and the error says so. Also hands back the seat
    /// of a license that doesn't cover this version of the app.
    pub fn deactivate(&self) -> Result<LicenseState> {
        let (generation, license) = {
            let mut inner = self.inner();
            let Some(license) = inner.standing.held().cloned() else {
                return Err(Error::NotLicensed);
            };
            if license.method == ActivationMethod::Offline || license.trial {
                drop(inner);
                return Ok(self.forget());
            }
            if inner.deactivating.is_some() {
                return Err(Error::Busy("A deactivation is already in progress".into()));
            }
            // The activation's poll would be superseded and strand it.
            if inner.activation.is_some() {
                return Err(Error::Busy("Finish or cancel the activation first".into()));
            }
            inner.error = None;
            let generation = self.bump();
            inner.deactivating = Some(generation);
            (generation, license)
        };
        self.emit();

        let result = self.0.licensing.revoke_activation(&license.token);

        let mut inner = self.inner();
        if inner.deactivating == Some(generation) {
            inner.deactivating = None;
        }
        if !self.is_current(generation) {
            drop(inner);
            self.emit();
            return Ok(self.state());
        }
        let outcome = match result {
            // A verdict means the server will never accept the token again, so
            // forgetting it here is all that is left to do.
            Ok(()) | Err(SdkError::OperationNotSupported(_)) => Ok(()),
            Err(error) if error.is_license_rejection() => Ok(()),
            Err(error) => {
                log::warn!("moonbase: revoke_activation failed, license kept: {error}");
                inner.error = Some(failure(Activity::Deactivation, &error));
                Err(error)
            }
        };
        if outcome.is_ok() {
            self.delete_stored(&mut inner, Some(&license.activation_id));
            self.lock(&mut inner, Some(LockReason::Deactivated));
        }
        drop(inner);
        self.emit();
        outcome.map(|()| self.state()).map_err(Error::from)
    }

    /// Forget the license on this device only, without telling Moonbase.
    pub fn forget(&self) -> LicenseState {
        let mut inner = self.inner();
        self.end_activation_flows(&mut inner);
        self.delete_stored(&mut inner, None);
        self.lock(&mut inner, Some(LockReason::Deactivated));
        inner.error = None;
        drop(inner);
        self.emit();
        self.state()
    }

    /// The device token ("machine file") for activating a machine with no network:
    /// save it as a `.dt` file the customer exchanges at
    /// `https://<tenant>.moonbase.sh/activate` for a license file, then pass that
    /// file's contents to [`activate_offline`](Self::activate_offline).
    pub fn device_token(&self) -> Result<String> {
        self.0.licensing.generate_device_token().map_err(Error::from)
    }

    /// Activate from the contents of an offline license file.
    ///
    /// A license file for this device that doesn't cover this version of the app
    /// is stored anyway, as it still covers the versions it did, and the app locks
    /// with [`Status::NotCovered`]; the error says so.
    pub fn activate_offline(&self, license_file_contents: &str) -> Result<LicenseState> {
        // Stored by the SDK, a file that doesn't cover this version included.
        match self.0.licensing.import_offline_license(license_file_contents) {
            Ok(license) => {
                let mut inner = self.inner();
                self.end_activation_flows(&mut inner);
                inner.file_stamp = Some(self.stamp_file());
                self.set_license(&mut inner, license);
                inner.error = None;
                drop(inner);
                self.emit();
                Ok(self.state())
            }
            Err(SdkError::ReleaseNotCovered(refusal)) if refusal.license().is_some() => {
                // This device's license, for the versions it covers: kept, but locked.
                log::info!("moonbase: the offline license doesn't cover this version: {refusal}");
                let license = refusal.license().cloned().expect("checked above");
                let mut inner = self.inner();
                self.end_activation_flows(&mut inner);
                inner.file_stamp = Some(self.stamp_file());
                self.show_not_covered(&mut inner, NotCoveredLicense::from_refusal(&refusal, &license));
                drop(inner);
                self.emit();
                Err(SdkError::ReleaseNotCovered(refusal).into())
            }
            Err(error) => {
                log::warn!("moonbase: offline license file rejected: {error}");
                self.inner().error = Some(failure(Activity::OfflineActivation, &error));
                self.emit();
                Err(error.into())
            }
        }
    }

    /// Offer the upgrade the app needs. With [`Status::NotCovered`], open the
    /// upgrade page Moonbase named for the license or, without one, start an online
    /// activation, whose browser page offers the upgrade where the store sells one
    /// and activates this device once a license covers this version. Otherwise open
    /// the upgrade page [`check_update`](Self::check_update) found for a new
    /// version the license doesn't cover. After the purchase,
    /// [`refresh`](Self::refresh) picks up the upgraded license.
    pub fn upgrade_license(&self) -> Result<LicenseState> {
        let (url, not_covered) = {
            let inner = self.inner();
            match inner.standing.not_covered() {
                Some(not_covered) => (not_covered.upgrade_url.clone(), true),
                None => (inner.update_upgrade_url.clone(), false),
            }
        };
        let Some(url) = url else {
            if not_covered {
                // Only Moonbase knows where to buy the upgrade. Its activation page
                // offers it to a signed-in customer, then activates this device.
                return self.activate(ActivationMethod::Online);
            }
            return Err(Error::Internal("There is no upgrade to offer".into()));
        };
        if let Err(error) = self.open_url(&url) {
            log::warn!("moonbase: couldn't open the browser for the upgrade: {error}");
            if not_covered {
                self.inner().error = Some(Failure {
                    message: format!("Couldn't open your browser. Upgrade at {url}"),
                    detail: error.clone(),
                    kind: None,
                });
                self.emit();
            }
            return Err(Error::Internal(format!("Couldn't open the browser: {error}")));
        }
        Ok(self.state())
    }

    /// Clear the last failure, e.g. when the user dismisses its message.
    pub fn dismiss_error(&self) -> LicenseState {
        self.inner().error = None;
        self.emit();
        self.state()
    }

    /// This device, for diagnostics.
    pub fn device(&self) -> DeviceInfo {
        let resolver = self.0.licensing.device_resolver();
        let (description, error) = match resolver.describe_device() {
            Ok(description) => (description, None),
            Err(error) => (None, Some(error.to_string())),
        };
        DeviceInfo { name: resolver.device_name(), platform: Platform::current().as_str().into(), description, error }
    }

    /// Whether a newer release than this app is out, with its notes when it is.
    /// Only the notes need the network. For a customer whose licenses don't cover
    /// the new version, says so and where to upgrade.
    pub fn check_update(&self) -> Result<UpdateInfo> {
        let license = self.license().ok_or(Error::NotLicensed)?;
        let current = self.0.settings.application_version.clone().unwrap_or_default();
        let latest = license.licensed_product.current_release_version.clone();
        let mut info = UpdateInfo {
            available: latest.as_deref().is_some_and(|latest| update_available(&current, latest)),
            current_version: current,
            latest_version: latest.clone(),
            release_notes: String::new(),
            has_downloads: false,
            can_download: false,
            covered: true,
            release_access: None,
            upgrade_url: None,
        };
        if let (true, Some(latest)) = (info.available, latest) {
            let release = self.0.licensing.inventory().get_release(&latest, &license.token)?;
            // Moonbase judges this by every rule the release has, and by all of the
            // customer's licenses, as validating the new version does: the seat
            // moves to one that covers it.
            info.can_download = release.has_downloads && can_download(&license, &release);
            info.has_downloads = release.has_downloads;
            info.release_notes = release.description;
            info.covered = release.release_covered;
            info.release_access = release.release_access;
            info.upgrade_url.clone_from(&release.upgrade_url);
            self.inner().update_upgrade_url = release.upgrade_url;
        }
        Ok(info)
    }

    /// A short-lived download link for the latest installer for this platform.
    ///
    /// Fails with [`moonbase::Error::ReleaseNotCovered`], with the upgrade link,
    /// when the customer's licenses don't cover the release.
    pub fn update_download_url(&self) -> Result<DownloadTarget> {
        let license = self.license().ok_or(Error::NotLicensed)?;
        match self.0.licensing.inventory().get_download_url(Platform::current().as_str(), &license.token) {
            Ok(target) => Ok(target),
            Err(SdkError::ReleaseNotCovered(refusal)) => {
                if let Some(url) = refusal.upgrade_url() {
                    self.inner().update_upgrade_url = Some(url.to_owned());
                }
                Err(SdkError::ReleaseNotCovered(refusal).into())
            }
            Err(error) => Err(error.into()),
        }
    }

    // -- Add-ons: sub-products the merchant includes in the product's license. The
    // license in force says which ones this device may unlock; the list says what
    // else the customer can do: try one, buy one, or take a seat of one they own.

    /// List the product's add-ons for this device into [`LicenseState::add_ons`],
    /// with their prices, ownership and trials, for a store or upgrade screen.
    ///
    /// Can take a seat: with
    /// [`auto_activate_sub_products`](crate::Builder::auto_activate_sub_products) on
    /// (the default), when the list shows an add-on the customer owns, with a seat
    /// free, that the license doesn't hold yet (one bought since the license was
    /// last checked), the license is re-checked once, which takes that seat as
    /// every check does, and the add-ons are listed again. With the option off it
    /// only reads.
    pub fn load_add_ons(&self) -> Result<LicenseState> {
        self.list_add_ons(true)
    }

    fn list_add_ons(&self, pick_up_seats: bool) -> Result<LicenseState> {
        let (generation, token) = {
            let mut inner = self.inner();
            let Some(token) = inner.standing.license().map(|license| license.token.clone()) else {
                return Err(Error::NotLicensed);
            };
            inner.add_ons_generation += 1;
            inner.add_ons.phase = AddOnsPhase::Loading;
            inner.add_ons.error = None;
            (inner.add_ons_generation, token)
        };
        self.emit();

        let result = self.0.licensing.list_sub_products(&token);

        let (recheck, take_seat) = {
            let mut inner = self.inner();
            if inner.add_ons_generation != generation {
                // Superseded, or the license went.
                return Ok(self.state_with(inner));
            }
            inner.add_ons.phase = AddOnsPhase::Ready;
            let refused = match &result {
                Ok(catalog) => {
                    inner.add_ons.currency = Some(catalog.currency.clone());
                    inner.add_ons.add_ons.clone_from(&catalog.sub_products);
                    inner.add_ons.error = None;
                    false
                }
                Err(error) => {
                    log::warn!("moonbase: listing the add-ons failed: {error}");
                    inner.add_ons.error = Some(match error {
                        SdkError::LicenseExpired(_) => "Your license has expired.".into(),
                        error if error.is_license_invalid() => {
                            "Your license is no longer valid on this computer.".into()
                        }
                        error => add_on_failure(
                            error,
                            "Couldn't load the add-ons",
                            "Moonbase couldn't find the add-ons for this license.",
                        ),
                    });
                    error.is_license_rejection()
                }
            };
            // Bought since the license was last checked: checking it takes the
            // seat, as it would at the next launch. Once, so an add-on the server
            // still won't seat can't start a loop.
            let take_seat = result.is_ok() && pick_up_seats && self.lists_seat_to_take(&inner);
            (refused && inner.deactivating.is_none(), take_seat)
        };
        self.emit();

        // The license itself was refused: re-check it, which locks as a refresh
        // does when the server stands by that.
        if recheck && let Err(error) = self.refresh(true) {
            log::debug!("moonbase: the re-check after a refused add-on list failed: {error}");
        }
        if take_seat {
            log::info!("moonbase: an add-on the customer owns has a seat free; re-checking the license to take it");
            return self.refresh_and_list();
        }
        result.map(|_| self.state()).map_err(Error::from)
    }

    fn lists_seat_to_take(&self, inner: &Inner) -> bool {
        let Some(license) = inner.standing.license() else {
            return false;
        };
        if !self.0.settings.auto_activate_sub_products || license.trial || license.method != ActivationMethod::Online {
            return false;
        }
        let now = SystemTime::now();
        inner.add_ons.add_ons.iter().any(|entry| {
            let owned = entry.product.owned || entry.product.subscribed;
            if !entry.included_in_license || !owned || entry.revoked {
                return false;
            }
            // On a seat here already: nothing to take. A trial still unlocking one
            // the customer has since bought gives way to a seat at the next check.
            let held = license.find_sub_product(&entry.id);
            if held.is_some_and(|held| !held.trial) && license.has_sub_product_at(&entry.id, now) {
                return false;
            }
            // A seat is free, or the list says this device has one that the license
            // doesn't show (an answer that was never stored, say).
            entry.active || entry.seats.is_some_and(|seats| seats.remaining > 0)
        })
    }

    /// Re-check the license, so an add-on bought since takes its seat, then list
    /// the add-ons again: what "Bought an add-on? Refresh" does after a purchase in
    /// the browser.
    pub fn refresh_add_ons(&self) -> Result<LicenseState> {
        self.inner().add_ons.action_error = None;
        self.refresh_and_list()
    }

    fn refresh_and_list(&self) -> Result<LicenseState> {
        let offline = {
            let mut inner = self.inner();
            // Not over a deactivation on its way, which settles the license either way.
            let Some(license) = inner.standing.license() else {
                return Ok(self.state_with(inner));
            };
            if inner.deactivating.is_some() {
                return Ok(self.state_with(inner));
            }
            let offline = license.method == ActivationMethod::Offline;
            if !offline {
                inner.add_ons.phase = AddOnsPhase::Loading;
                inner.add_ons.error = None;
            }
            offline
        };
        // An offline license is never re-validated; its add-ons are just listed.
        if !offline {
            self.emit();
            // The license was just checked, so the list that follows has no seat
            // left to take that checking again would.
            if let Err(error) = self.refresh(true) {
                log::debug!("moonbase: the re-check before listing the add-ons failed: {error}");
            }
            if self.inner().standing.license().is_none() {
                return Ok(self.state());
            }
        }
        self.list_add_ons(false)
    }

    /// Take a seat of an add-on the customer owns for this device, from the full
    /// license in force: one whose seat was revoked here, or any, with
    /// [`auto_activate_sub_products`](crate::Builder::auto_activate_sub_products)
    /// off. Stores the license that comes back, and lists the add-ons again if they
    /// were loaded.
    ///
    /// One add-on at a time; why one failed is also in
    /// [`AddOnsState::action_error`](crate::AddOnsState::action_error), worded for
    /// the customer. A license the server no longer accepts is re-checked, and
    /// locks as [`refresh`](Self::refresh) would.
    pub fn activate_sub_product(&self, sub_product_id: &str) -> Result<LicenseState> {
        self.run_sub_product_action(sub_product_id, false)
    }

    /// Start the trial of an add-on the customer doesn't own on this device, from
    /// the full license in force (owning it, they get a seat instead). Otherwise as
    /// [`activate_sub_product`](Self::activate_sub_product).
    pub fn request_sub_product_trial(&self, sub_product_id: &str) -> Result<LicenseState> {
        self.run_sub_product_action(sub_product_id, true)
    }

    fn add_on_name(inner: &Inner, sub_product_id: &str) -> Option<String> {
        inner
            .add_ons
            .add_ons
            .iter()
            .find(|entry| entry.id == sub_product_id || entry.product.id == sub_product_id)
            .map(|entry| entry.product.name.clone())
    }

    fn run_sub_product_action(&self, sub_product_id: &str, trial: bool) -> Result<LicenseState> {
        let (call, token, activation_id, name) = {
            let mut inner = self.inner();
            let refusal = match inner.standing.license() {
                None => Some((format!("Activate {} first.", self.product_name()), Error::NotLicensed)),
                Some(license) if license.trial => {
                    let message = "Add-ons are activated from a full license, not a trial.";
                    Some((message.to_owned(), SdkError::OperationNotSupported(message.into()).into()))
                }
                Some(_) => None,
            };
            if let Some((message, error)) = refusal {
                inner.add_ons.action_error = Some(message);
                drop(inner);
                self.emit();
                return Err(error);
            }
            if inner.add_ons.busy_id.is_some() {
                return Err(Error::Busy("Another add-on is still being set up. Try again in a moment.".into()));
            }
            let license = inner.standing.license().expect("checked above");
            let (token, activation_id) = (license.token.clone(), license.activation_id.clone());
            let name = Self::add_on_name(&inner, sub_product_id);
            inner.add_on_calls += 1;
            inner.add_on_call = inner.add_on_calls;
            inner.add_ons.busy_id = Some(sub_product_id.to_owned());
            inner.add_ons.action_error = None;
            (inner.add_on_call, token, activation_id, name)
        };
        self.emit();

        let outcome = {
            let _turn = self.online_turn();
            let store = self.0.licensing.store();
            // The SDK holds the store's lock from the request until it stores the
            // answer, so answers reach the store in the order the server gave them,
            // whichever instance asked. Only over a stored copy of this activation:
            // one cleared, deactivated or replaced meanwhile is left as it is.
            let still_this_license = || Self::stores_activation(store, &activation_id);
            let result = if trial {
                self.0.licensing.request_sub_product_trial_with(&token, sub_product_id, still_this_license)
            } else {
                self.0.licensing.activate_sub_product_with(&token, sub_product_id, still_this_license)
            };

            let mut inner = self.inner();
            // Unless the license went in the meantime, which cleared it already.
            if inner.add_on_call == call {
                inner.add_on_call = 0;
                inner.add_ons.busy_id = None;
            }
            // Only onto the license it was asked from: one deactivated, cleared or
            // replaced since keeps what it has now.
            let same_license = inner.standing.license().is_some_and(|license| license.activation_id == activation_id);
            match result {
                Ok(license) if same_license && license.activation_id == activation_id => {
                    inner.file_stamp = Some(self.stamp_file());
                    self.set_license(&mut inner, license);
                    AddOnOutcome::Applied { relist: inner.add_ons.phase != AddOnsPhase::Idle }
                }
                Ok(_) => AddOnOutcome::Changed,
                Err(error) => {
                    log::warn!(
                        "moonbase: {} failed: {error}",
                        if trial { "request_sub_product_trial" } else { "activate_sub_product" }
                    );
                    if same_license {
                        let subject = name.clone().unwrap_or_else(|| "This add-on".into());
                        let object = name.clone().unwrap_or_else(|| "this add-on".into());
                        inner.add_ons.action_error = Some(match &error {
                            SdkError::NoEligibleLicense(_) => format!(
                                "No seat of {object} is free for this computer. Free one in your {} account, or buy one.",
                                self.account_name()
                            ),
                            SdkError::OperationNotSupported(_) if trial => {
                                format!("{subject} can't be tried on this computer.")
                            }
                            SdkError::OperationNotSupported(_) => {
                                format!("{subject} can't be activated on this computer.")
                            }
                            SdkError::LicenseExpired(_) => "Your license has expired.".into(),
                            error if error.is_license_invalid() => {
                                "Your license is no longer valid on this computer.".into()
                            }
                            error => add_on_failure(
                                error,
                                &if trial {
                                    format!("Couldn't start the trial of {object}")
                                } else {
                                    format!("Couldn't activate {object}")
                                },
                                &format!("Moonbase couldn't find {object}."),
                            ),
                        });
                        let recheck = error.is_license_rejection() && inner.deactivating.is_none();
                        AddOnOutcome::Failed { error, recheck }
                    } else {
                        AddOnOutcome::Changed
                    }
                }
            }
        };
        self.emit();

        match outcome {
            AddOnOutcome::Applied { relist } => {
                // Show what changed.
                if relist && let Err(error) = self.list_add_ons(false) {
                    log::debug!("moonbase: listing the add-ons again failed: {error}");
                }
                Ok(self.state())
            }
            AddOnOutcome::Changed => Err(Error::Busy(LICENSE_CHANGED.into())),
            AddOnOutcome::Failed { error, recheck } => {
                // The license itself was refused: re-check it, which locks as a
                // refresh does when the server stands by that.
                if recheck && let Err(error) = self.refresh(true) {
                    log::debug!("moonbase: the re-check after a refused add-on failed: {error}");
                }
                Err(error.into())
            }
        }
    }

    /// Open the add-on's buy link from [`LicenseState::add_ons`] in the browser.
    /// When the app then goes to the background (the customer is in the browser)
    /// and comes back to the front, the add-ons are refreshed as
    /// [`refresh_add_ons`](Self::refresh_add_ons) does, so one just bought unlocks
    /// without a click.
    pub fn buy_sub_product(&self, sub_product_id: &str) -> Result<()> {
        let (url, name) = {
            let inner = self.inner();
            inner
                .add_ons
                .add_ons
                .iter()
                .find(|entry| entry.id == sub_product_id || entry.product.id == sub_product_id)
                .and_then(|entry| entry.buy_url.clone().map(|url| (url, entry.product.name.clone())))
                .ok_or_else(|| Error::Internal("That add-on isn't for sale here".into()))?
        };
        if let Err(error) = self.open_url(&url) {
            log::warn!("moonbase: couldn't open the browser to buy an add-on: {error}");
            self.inner().add_ons.action_error = Some(format!("Couldn't open your browser to buy {name}."));
            self.emit();
            return Err(Error::Internal(format!("Couldn't open the browser: {error}")));
        }
        let mut inner = self.inner();
        inner.purchase_opened_at = Some(Instant::now());
        inner.left_for_purchase = None;
        Ok(())
    }

    /// One of the app's windows gained or lost focus: notice the customer coming
    /// back from a purchase in the browser.
    pub(crate) fn on_focus_changed(&self, focused: bool) {
        let mut inner = self.inner();
        let Some(opened) = inner.purchase_opened_at else {
            return;
        };
        if opened.elapsed() > PURCHASE_WINDOW {
            // Whatever happened, it wasn't a purchase coming back.
            inner.purchase_opened_at = None;
            inner.left_for_purchase = None;
            return;
        }
        if !focused {
            inner.left_for_purchase = Some(Instant::now());
            return;
        }
        // Back. Moving between the app's own windows passes through here too, in a
        // blink, and a deactivation on its way settles the license first.
        let away = inner.left_for_purchase.is_some_and(|left| left.elapsed() >= Duration::from_secs(1));
        if !away || inner.standing.license().is_none() || inner.deactivating.is_some() {
            return;
        }
        inner.purchase_opened_at = None;
        inner.left_for_purchase = None;
        drop(inner);
        log::info!("moonbase: back from the browser after opening an add-on's buy link; refreshing the add-ons");
        let this = self.clone();
        thread::spawn(move || {
            if let Err(error) = this.refresh_add_ons() {
                log::debug!("moonbase: refreshing the add-ons failed: {error}");
            }
        });
    }

    fn product_name(&self) -> String {
        self.ui().map_or_else(|| self.0.app.package_info().name.clone(), |ui| ui.product_name(&self.0.app))
    }

    fn account_name(&self) -> String {
        self.ui().and_then(Ui::manufacturer_name).unwrap_or("Moonbase").to_owned()
    }

    // -- The license watch: local checks only (a stat of the license file, a read
    // when it changed, and a look at the clock), so no request goes out unless a
    // license or an add-on has actually run out.

    fn watch(&self) {
        if self.inner().loading {
            return;
        }
        self.check_license_file();
        if !self.check_sub_products() {
            self.check_deadlines();
        }
        self.check_countdowns();
    }

    fn check_license_file(&self) {
        let stamp = self.stamp_file();
        // Taken with the stamp, so a forget or deactivate that lands while the file
        // is read and validated below keeps the copy from being adopted.
        let generation = self.0.generation.load(Ordering::SeqCst);
        {
            let mut inner = self.inner();
            if inner.file_stamp == Some(stamp) {
                return;
            }
            // Only a file that was there and went away means another instance
            // removed it. One that never got written must not lock this one.
            let existed = inner.file_stamp.is_some_and(|previous| previous.exists);
            inner.file_stamp = Some(stamp);
            if !stamp.exists {
                if existed {
                    self.on_license_file_removed(inner);
                }
                return;
            }
        }

        // Read the file itself rather than through the store: the store lock can be
        // held by another process for a whole network check, and a load deletes a
        // file it cannot parse, which is what a file caught mid-write looks like. A
        // torn read just fails here; the write that completes it changes the stamp.
        let Ok(on_disk) = std::fs::read(self.0.store.path())
            .map_err(|_| ())
            .and_then(|bytes| serde_json::from_slice::<License>(&bytes).map_err(|_| ()))
        else {
            return;
        };
        // Our own write, or a sibling's that changed nothing.
        if self.inner().standing.held().is_some_and(|held| held.token == on_disk.token) {
            return;
        }
        let (stored, not_covered) = match self.0.licensing.validate_token_local(&on_disk.token) {
            Ok(stored) => (stored, None),
            // Good, but not for this version: typically a sibling's answer from
            // Moonbase, stored along with the refusal. Taken like any stored
            // license, except that it locks.
            Err(SdkError::ReleaseNotCovered(refusal)) => match refusal.license() {
                Some(license) => (license.clone(), Some(NotCoveredLicense::from_refusal(&refusal, license))),
                None => return,
            },
            // Expired, bound elsewhere or tampered with: nothing this instance can use.
            Err(_) => return,
        };
        // Unverified for longer than the grace period: startup would only take it
        // after an online check, so neither does this. Whoever re-validates it
        // writes a fresh copy.
        if stored.method != ActivationMethod::Offline && age(stored.validated_at) > self.0.settings.grace_period {
            return;
        }
        match not_covered {
            Some(not_covered) => self.adopt_not_covered(not_covered, generation),
            None => self.adopt_stored_license(stored, generation),
        }
    }

    fn on_license_file_removed(&self, mut inner: MutexGuard<'_, Inner>) {
        if inner.standing.license().is_none() {
            // The license that didn't cover this version is gone as well.
            if inner.standing.not_covered().is_none() {
                return;
            }
            log::info!("moonbase: the license file was removed by another instance or process");
            if inner.activation.is_some() {
                // The activation in progress carries on and brings a license of its own.
                self.lock(&mut inner, Some(LockReason::Deactivated));
            } else {
                // A "Check again" in flight must not write it back.
                self.end_activation_flows(&mut inner);
                self.lock(&mut inner, Some(LockReason::Deactivated));
            }
            drop(inner);
            return self.emit();
        }
        log::info!("moonbase: the license file was removed by another instance or process; locking");
        if inner.activation.is_some() {
            // The user is activating here (typically unlocking a trial): lock, but
            // leave the flow running. When it lands it brings its own license.
            self.lock(&mut inner, Some(LockReason::Deactivated));
        } else {
            self.end_activation_flows(&mut inner);
            self.lock(&mut inner, Some(LockReason::Deactivated));
        }
        drop(inner);
        self.emit();
    }

    fn adopt_stored_license(&self, stored: License, generation: u64) {
        let mut inner = self.inner();
        if !self.is_current(generation) {
            // Superseded while the file was read. Look again on the next tick.
            inner.file_stamp = None;
            return;
        }
        // Two processes' refreshes can land out of order; never trade the copy held
        // for an older one. That includes one Moonbase said doesn't cover this
        // version, which only a newer copy undoes.
        if Self::is_older_than_held(&inner, &stored) {
            return;
        }
        if let Some(current) = inner.standing.license()
            && current.activation_id == stored.activation_id
        {
            // A newer copy of the same activation is taken quietly.
            self.set_license(&mut inner, stored);
            drop(inner);
            return self.emit();
        }
        // A different activation, made or replaced elsewhere, or the upgrade of one
        // that didn't cover this version. Anything in flight here belongs to the
        // old one and must not land on top of it.
        log::info!("moonbase: picked up a license stored by another instance or process");
        self.end_activation_flows(&mut inner);
        self.set_license(&mut inner, stored);
        drop(inner);
        self.emit();
    }

    fn adopt_not_covered(&self, mut not_covered: NotCoveredLicense, generation: u64) {
        let mut inner = self.inner();
        if !self.is_current(generation) {
            inner.file_stamp = None;
            return;
        }
        // As adopt_stored_license: never an older copy of the activation held here.
        // An activation in progress brings a license of its own.
        if Self::is_older_than_held(&inner, &not_covered.license) || inner.activation.is_some() {
            return;
        }
        let same_activation =
            inner.standing.held().is_some_and(|held| held.activation_id == not_covered.license.activation_id);
        log::info!("moonbase: another instance or process found that the license doesn't cover this version; locking");
        // The file holds no upgrade link, so keep the one Moonbase gave here.
        if same_activation
            && not_covered.upgrade_url.is_none()
            && let Some(held) = inner.standing.not_covered()
        {
            not_covered.upgrade_url.clone_from(&held.upgrade_url);
        }
        // A different activation's in-flight work must not land on top of it.
        if !same_activation {
            self.end_activation_flows(&mut inner);
        }
        self.show_not_covered(&mut inner, not_covered);
        drop(inner);
        self.emit();
    }

    /// An add-on's trial (or subscription) can run out while the license carries
    /// on, which changes what the app may unlock without a new token. True when it
    /// started a re-check.
    fn check_sub_products(&self) -> bool {
        let now = SystemTime::now();
        let (changed, ran_out) = {
            let mut inner = self.inner();
            let Some(license) = inner.standing.license().cloned() else {
                return false;
            };
            let active = active_sub_products(&license, now);
            let changed = *self.0.emitted_sub_products.lock().unwrap_or_else(PoisonError::into_inner) != active;
            for id in &active {
                // Unlocked again: may be checked again later.
                inner.lapses_checked.remove(id);
            }
            // One whose time ran out may have been bought or renewed meanwhile,
            // which the server turns into a seat at the next check: check now, once
            // for each, as an expiring license is checked.
            let mut ran_out = Vec::new();
            if license.method == ActivationMethod::Online
                && inner.deactivating.is_none()
                && inner.refreshes_in_flight == 0
                && inner.activation.is_none()
            {
                for entry in &license.sub_products {
                    if entry.active
                        && entry.expires_at.is_some_and(|expires| expires <= now)
                        && !inner.lapses_checked.contains(&entry.id)
                    {
                        ran_out.push(entry.id.clone());
                    }
                }
                inner.lapses_checked.extend(ran_out.iter().cloned());
            }
            (changed, !ran_out.is_empty())
        };
        if changed {
            self.emit();
        }
        if ran_out {
            log::info!("moonbase: an add-on ran out; re-checking the license in case it was bought or renewed");
            if let Err(error) = self.refresh(true) {
                log::debug!("moonbase: the add-on re-check failed: {error}");
            }
        }
        ran_out
    }

    fn check_deadlines(&self) {
        let mut inner = self.inner();
        // Not while a refresh is on its way: another would supersede it.
        let Some(license) = inner.standing.license().cloned() else {
            return;
        };
        if inner.deactivating.is_some() || inner.refreshes_in_flight > 0 {
            return;
        }
        let expired = license.expires_at.is_some_and(|expires| expires <= SystemTime::now());
        let offline = license.method == ActivationMethod::Offline;
        let past_grace = !offline && age(license.validated_at) > self.0.settings.grace_period;
        if !expired && !past_grace {
            return;
        }

        if inner.activation.is_some() {
            // Typically a trial being unlocked. A re-check would supersede the
            // activation's own requests, so stop honouring the license and leave
            // the flow alone: it replaces the license when it lands.
            log::info!("moonbase: the license ran out while an activation was in progress; locking");
            if expired && license.trial {
                self.show_trial_expired(&mut inner, license);
            } else {
                self.lock(&mut inner, Some(if expired { LockReason::Expired } else { LockReason::Unverified }));
            }
            drop(inner);
            return self.emit();
        }

        if offline {
            // Never re-validated, and dead for good once past its `exp`.
            log::info!("moonbase: the offline license has expired; removing it");
            self.bump();
            self.delete_stored(&mut inner, None);
            self.lock(&mut inner, Some(LockReason::Expired));
            drop(inner);
            return self.emit();
        }

        if !expired {
            // Spaced out, so a skewed clock cannot turn the watch into a request
            // every tick.
            let now = Instant::now();
            if inner.last_grace_check.is_some_and(|last| now - last < self.0.settings.check_interval) {
                return;
            }
            inner.last_grace_check = Some(now);
        }
        drop(inner);

        // A passed `exp` gets one request, as a subscription may have renewed or a
        // trial been extended or bought, and locks (a trial shows as expired)
        // unless that brings a license still running; past the grace period the
        // server gets one more chance too.
        if let Err(error) = self.refresh(false) {
            log::debug!("moonbase: the deadline re-check failed: {error}");
        }
    }

    /// The UI counts down to when the license and its add-ons end ("5 days left"),
    /// and nothing reports a day going by: emit when one of those counts moves, so
    /// a screen left open keeps up. Not when what ends has changed: whatever
    /// changed it emitted already.
    fn check_countdowns(&self) {
        let mut inner = self.inner();
        let mut ends: Vec<SystemTime> = Vec::new();
        if let Some(license) = inner.standing.license() {
            ends.extend(license.expires_at);
            ends.extend(license.sub_products.iter().filter_map(|entry| entry.expires_at));
        }
        for entry in &inner.add_ons.add_ons {
            ends.extend(entry.expires_at);
            ends.extend(entry.trial.as_ref().and_then(|trial| trial.expires_at));
        }
        let days: Vec<u64> = ends.iter().copied().map(days_until).collect();
        let day_went_by = ends == inner.countdown.0 && days != inner.countdown.1;
        inner.countdown = (ends, days);
        drop(inner);
        if day_went_by {
            self.emit();
        }
    }
}

impl<R: Runtime> std::fmt::Debug for Moonbase<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Moonbase")
            .field("license_file", &self.0.store.path())
            .field("licensed", &self.is_licensed())
            .finish_non_exhaustive()
    }
}
