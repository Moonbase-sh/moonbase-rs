//! The built-in activation UI, from the "Solstice Activation" design, as its own
//! window or as an overlay that locks one of the app's windows.
//!
//! As a window, the plugin serves the page itself over its own `moonbase://` scheme.
//! As an overlay, the same UI is injected into the app's webviews and mounted in a
//! closed shadow root over the app's content. Either way the plugin grants the UI
//! its commands at runtime, so an app gets a finished, themed activation flow
//! without writing any frontend code. It presents itself when the app is locked,
//! and whenever the app asks.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
#[cfg(desktop)]
use std::time::{Duration, Instant};

use base64::Engine as _;
use moonbase::ReleaseAccess;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::http::{Request, Response, StatusCode, header};
use tauri::{AppHandle, Manager, Runtime};

use crate::controller::Moonbase;
use crate::error::{Error, Result};
use crate::models::{LicenseState, Status};

/// The window's label. Capabilities, events and `get_webview_window` use it.
pub const WINDOW_LABEL: &str = "moonbase-activation";
pub(crate) const SCHEME: &str = "moonbase";

/// Where the footer's moonbase mark links to.
const MOONBASE_SITE: &str = "https://moonbase.sh";
#[cfg(desktop)]
const WIDTH: f64 = 660.0;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const UI_CSS: &str = include_str!("../ui/ui.css");
const UI_JS: &str = include_str!("../ui/ui.js");
const WINDOW_JS: &str = include_str!("../ui/window.js");
const OVERLAY_JS: &str = include_str!("../ui/overlay.js");

// Everything the page loads is its own; the IPC origins are Tauri's.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                   connect-src ipc: http://ipc.localhost";

/// A line on the trial screen: something the trial includes, or does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrialFeature {
    pub label: String,
    pub included: bool,
}

/// Copy overrides. Leave a field `None` for the built-in text, several of which
/// name your product and manufacturer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiStrings {
    /// Default: "Activate {product}".
    pub welcome_title: Option<String>,
    /// Default: "Unlock {product} through your {manufacturer} account, or take it for a spin with a free trial."
    pub welcome_body: Option<String>,
    /// Default: "Activate online".
    pub activate_online: Option<String>,
    /// Default: "No internet? Activate offline".
    pub activate_offline: Option<String>,
    /// Shown on the welcome screen instead of the welcome copy once the app has
    /// lost its license, one pair per reason, so the customer learns why.
    pub deactivated_title: Option<String>,
    pub deactivated_body: Option<String>,
    pub license_expired_title: Option<String>,
    pub license_expired_body: Option<String>,
    pub license_invalid_title: Option<String>,
    pub license_invalid_body: Option<String>,
    pub license_unverified_title: Option<String>,
    pub license_unverified_body: Option<String>,
    /// The note on the trial-expired screen about what stops working.
    pub expired_notice: Option<String>,
    /// The upgrade screen, for a license that doesn't cover this version of the
    /// app. Default: "Version {version} needs an upgrade".
    pub not_covered_title: Option<String>,
    /// Default: what the license covers, e.g. "Your updates ended on 1 June 2026.
    /// Your license covers releases up to 2.3.0."
    pub not_covered_body: Option<String>,
}

/// A screen of the built-in UI to open on, with
/// [`Moonbase::present`](crate::Moonbase::present). Without one it shows whatever the
/// license state calls for. In the frontend, the same names as strings:
/// `"license"`, `"update"`, `"offline"`, `"addons"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum Screen {
    /// The license details, or what the state calls for when there is no license.
    License,
    /// The update screen, for a newer release than the app.
    Update,
    /// Offline activation: the machine file and the license file.
    Offline,
    /// The product's add-ons, from a full license.
    #[serde(rename = "addons")]
    AddOns,
}

/// Where the activation UI appears.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Presentation {
    /// Its own window, beside the app, which stays usable meanwhile. The default.
    #[default]
    Window,
    /// Over the content of one of the app's windows, which is blurred and inert
    /// behind it: the app is locked for as long as it has no license. Once licensed
    /// the overlay can be dismissed, and opened again on demand.
    Overlay {
        /// The label of the window to lock.
        window: String,
    },
}

impl Presentation {
    /// An overlay over the app's `main` window.
    pub fn overlay() -> Self {
        Self::overlay_on("main")
    }

    /// An overlay over the window with this label.
    pub fn overlay_on(window: impl Into<String>) -> Self {
        Self::Overlay { window: window.into() }
    }
}

#[derive(Debug, Clone)]
enum Logo {
    Svg(String),
    Png(Vec<u8>),
}

/// Branding and behaviour for the built-in activation window.
///
/// ```no_run
/// use tauri_plugin_moonbase::UiConfig;
///
/// let ui = UiConfig::new()
///     .product_name("Solstice")
///     .manufacturer_name("Helio Audio")
///     .accent("#186cdc")
///     .trial_feature("Full DSP engine, unrestricted", true)
///     .trial_feature("Use in commercial releases", false);
/// ```
#[derive(Debug, Clone)]
pub struct UiConfig {
    pub(crate) enabled: bool,
    product_name: Option<String>,
    manufacturer_name: Option<String>,
    accent: String,
    logo: Option<Logo>,
    trial_features: Vec<TrialFeature>,
    trial_length_days: u32,
    show_moonbase_badge: bool,
    strings: UiStrings,
    show_addons: bool,
    pub(crate) auto_present_on_lock: bool,
    pub(crate) auto_present_update: bool,
    reduce_motion: bool,
    window_title: Option<String>,
    pub(crate) presentation: Presentation,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            product_name: None,
            manufacturer_name: None,
            accent: "#186cdc".into(),
            logo: None,
            trial_features: Vec::new(),
            trial_length_days: 14,
            show_moonbase_badge: true,
            strings: UiStrings::default(),
            show_addons: false,
            auto_present_on_lock: true,
            auto_present_update: true,
            reduce_motion: false,
            window_title: None,
            presentation: Presentation::Window,
        }
    }
}

impl UiConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// No built-in window: drive your own UI from the state event and commands.
    pub fn disabled() -> Self {
        Self { enabled: false, ..Self::default() }
    }

    /// Shown throughout the UI. Defaults to the app's product name.
    pub fn product_name(mut self, name: impl Into<String>) -> Self {
        self.product_name = Some(name.into());
        self
    }

    /// Shown under the product name, and as the account the customer signs in to.
    pub fn manufacturer_name(mut self, name: impl Into<String>) -> Self {
        self.manufacturer_name = Some(name.into());
        self
    }

    /// The one brand colour: buttons, progress, step numbers. Any CSS colour.
    /// Defaults to Moonbase blue, `#186cdc`.
    pub fn accent(mut self, color: impl Into<String>) -> Self {
        self.accent = color.into();
        self
    }

    /// The product mark in the header, as SVG markup. Without one, a sun mark is
    /// drawn in the accent colour.
    pub fn logo_svg(mut self, svg: impl Into<String>) -> Self {
        self.logo = Some(Logo::Svg(svg.into()));
        self
    }

    /// The product mark in the header, as PNG bytes. Supply at least 76 px square.
    pub fn logo_png(mut self, png: impl Into<Vec<u8>>) -> Self {
        self.logo = Some(Logo::Png(png.into()));
        self
    }

    /// Add a line to the trial screen's feature list.
    pub fn trial_feature(mut self, label: impl Into<String>, included: bool) -> Self {
        self.trial_features.push(TrialFeature { label: label.into(), included });
        self
    }

    /// The trial length shown when the license does not say (an endless trial
    /// shows no countdown). Default 14.
    pub fn trial_length_days(mut self, days: u32) -> Self {
        self.trial_length_days = days.max(1);
        self
    }

    /// The "Licensing secured by moonbase" footer, whose Moonbase mark links to
    /// moonbase.sh. On by default.
    pub fn show_moonbase_badge(mut self, show: bool) -> Self {
        self.show_moonbase_badge = show;
        self
    }

    pub fn strings(mut self, strings: UiStrings) -> Self {
        self.strings = strings;
        self
    }

    /// Add an "Add-ons" row to the license details, opening a screen that lists
    /// the product's add-ons from Moonbase with what the customer can do with each:
    /// try it, buy it, or activate it on this computer. Off by default; turn it on
    /// when the product has add-ons. The add-ons a license includes unlock either
    /// way: check them with [`Moonbase::has_sub_product`].
    pub fn show_addons(mut self, show: bool) -> Self {
        self.show_addons = show;
        self
    }

    /// Present the window whenever the app is locked: when it starts without a
    /// license, and whenever the license is lost while it runs. On by default.
    pub fn auto_present_on_lock(mut self, present: bool) -> Self {
        self.auto_present_on_lock = present;
        self
    }

    /// Present the update screen when the app starts with a newer, unskipped
    /// release available. On by default.
    pub fn auto_present_update(mut self, present: bool) -> Self {
        self.auto_present_update = present;
        self
    }

    /// Skip the screen transitions, the success pop and the glow. The system's
    /// reduced-motion setting is honoured either way.
    pub fn reduce_motion(mut self, reduce: bool) -> Self {
        self.reduce_motion = reduce;
        self
    }

    /// The window title. Defaults to the product name.
    pub fn window_title(mut self, title: impl Into<String>) -> Self {
        self.window_title = Some(title.into());
        self
    }

    /// Show the UI in its own window (the default), or as an overlay that locks one
    /// of the app's windows until it is licensed:
    ///
    /// ```no_run
    /// # use tauri_plugin_moonbase::{Presentation, UiConfig};
    /// let ui = UiConfig::new().presentation(Presentation::overlay());
    /// ```
    pub fn presentation(mut self, presentation: Presentation) -> Self {
        self.presentation = presentation;
        self
    }

    /// The label of the window an overlay locks, if the UI is an overlay.
    pub(crate) fn overlay_window(&self) -> Option<&str> {
        match &self.presentation {
            Presentation::Overlay { window } => Some(window),
            Presentation::Window => None,
        }
    }
}

/// The script that lets the UI mount as an overlay in the app's webviews: the UI,
/// its stylesheet, and the mount logic, dormant until the plugin mounts it.
pub(crate) fn overlay_script() -> String {
    let css = serde_json::to_string(UI_CSS).expect("a string serializes");
    format!("(function () {{\nconst CSS = {css};\n{UI_JS}\n{OVERLAY_JS}\n}})();")
}

/// How the update download is going, pushed to the window as it changes.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DownloadState {
    phase: DownloadPhase,
    progress: Option<f64>,
    path: Option<String>,
    error: Option<String>,
    /// For `NotCovered`: what the customer's licenses cover, and where to upgrade.
    release_access: Option<ReleaseAccess>,
    upgrade_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum DownloadPhase {
    #[default]
    Idle,
    Downloading,
    Done,
    Failed,
    /// Moonbase refused the download: the customer's licenses don't cover it.
    NotCovered,
}

/// What `ui_ready` tells the page: the screen it was presented for, if any.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct UiOpened {
    screen: Option<Screen>,
}

/// The window's runtime state, held by the controller.
pub(crate) struct Ui {
    pub(crate) config: UiConfig,
    endpoint: String,
    state_file: PathBuf,
    pending_screen: Mutex<Option<Screen>>,
    creating: Mutex<bool>,
    granted: Mutex<bool>,
    overlay_open: Mutex<bool>,
    last_licensed: Mutex<Option<bool>>,
    download: Mutex<DownloadState>,
    downloaded: Mutex<Option<PathBuf>>,
}

fn guard<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Ui {
    pub(crate) fn new(config: UiConfig, endpoint: String, license_file: &Path) -> Self {
        let mut state_file = license_file.as_os_str().to_owned();
        state_file.push(".state.json");
        Self {
            config,
            endpoint,
            state_file: state_file.into(),
            pending_screen: Mutex::default(),
            creating: Mutex::new(false),
            granted: Mutex::new(false),
            overlay_open: Mutex::new(false),
            last_licensed: Mutex::default(),
            download: Mutex::default(),
            downloaded: Mutex::default(),
        }
    }

    pub(crate) fn product_name<R: Runtime>(&self, app: &AppHandle<R>) -> String {
        self.config.product_name.clone().unwrap_or_else(|| app.package_info().name.clone())
    }

    pub(crate) fn manufacturer_name(&self) -> Option<&str> {
        self.config.manufacturer_name.as_deref()
    }

    fn activation_host(&self) -> String {
        tauri::Url::parse(&self.endpoint)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| self.endpoint.clone())
    }

    /// The client state file beside the license (skipped update versions), with
    /// unknown keys preserved so an older build never drops a newer one's.
    fn read_state(&self) -> Value {
        std::fs::read(&self.state_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))
    }

    fn is_skipped(&self, version: &str) -> bool {
        self.read_state()["ignoredUpdates"]
            .as_array()
            .is_some_and(|versions| versions.iter().any(|skipped| skipped.as_str() == Some(version)))
    }

    fn skip(&self, version: &str) -> Result<()> {
        let mut state = self.read_state();
        if !state["ignoredUpdates"].is_array() {
            state["ignoredUpdates"] = json!([]);
        }
        let versions = state["ignoredUpdates"].as_array_mut().expect("just made an array");
        if !versions.iter().any(|skipped| skipped.as_str() == Some(version)) {
            versions.push(json!(version));
        }
        let text = serde_json::to_string_pretty(&state).map_err(|error| Error::Internal(error.to_string()))?;
        std::fs::write(&self.state_file, text)
            .map_err(|error| Error::Internal(format!("Couldn't save the skipped update: {error}")))
    }

    fn page_config<R: Runtime>(&self, app: &AppHandle<R>) -> Value {
        let product = self.product_name(app);
        // The window loads a PNG logo from the plugin's scheme; an overlay cannot, so
        // it gets the bytes. An SVG logo is inlined either way.
        let (logo, logo_svg) = match &self.config.logo {
            Some(Logo::Svg(svg)) => (None, Some(svg.clone())),
            Some(Logo::Png(png)) if self.config.overlay_window().is_some() => {
                (Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png))), None)
            }
            Some(Logo::Png(_)) => (Some("logo".to_owned()), None),
            None => (None, None),
        };
        json!({
            "productName": product,
            "manufacturerName": self.config.manufacturer_name,
            "accent": self.config.accent,
            "logo": logo,
            "logoSvg": logo_svg,
            "trialFeatures": self.config.trial_features,
            "trialLengthDays": self.config.trial_length_days,
            "showBadge": self.config.show_moonbase_badge,
            "strings": self.config.strings,
            "showAddOns": self.config.show_addons,
            "reduceMotion": self.config.reduce_motion,
            "activationHost": self.activation_host(),
            "requestFileName": format!("{}.dt", file_safe(&product)),
            "os": std::env::consts::OS,
        })
    }
}

/// A name usable as a file name everywhere: path separators and the characters
/// Windows refuses become dashes.
fn file_safe(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_control() || r#"\/:*?"<>|"#.contains(c) { '-' } else { c }).collect();
    let cleaned = cleaned.trim().trim_matches('.').to_owned();
    if cleaned.is_empty() { "license".into() } else { cleaned }
}

/// `dir/name`, or `dir/name (2)` and so on when that is taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), format!(".{extension}")),
        _ => (name.to_owned(), String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){extension}")))
        .find(|path| !path.exists())
        .expect("some suffix is free")
}

fn respond(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store");
    if content_type.starts_with("text/html") {
        builder = builder.header("Content-Security-Policy", CSP);
    }
    builder.body(body).unwrap_or_else(|_| Response::new(Vec::new()))
}

/// Serve the window's page over the plugin's scheme.
pub(crate) fn serve<R: Runtime>(app: &AppHandle<R>, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let not_found = || respond(StatusCode::NOT_FOUND, "text/plain", b"Not found".to_vec());
    let Some(moonbase) = app.try_state::<Moonbase<R>>() else {
        return not_found();
    };
    let Some(ui) = moonbase.ui() else {
        return not_found();
    };
    match request.uri().path() {
        "/" | "/index.html" => respond(StatusCode::OK, "text/html; charset=utf-8", INDEX_HTML.as_bytes().to_vec()),
        "/ui.css" => respond(StatusCode::OK, "text/css; charset=utf-8", UI_CSS.as_bytes().to_vec()),
        "/ui.js" => respond(StatusCode::OK, "text/javascript; charset=utf-8", UI_JS.as_bytes().to_vec()),
        "/window.js" => respond(StatusCode::OK, "text/javascript; charset=utf-8", WINDOW_JS.as_bytes().to_vec()),
        "/config.js" => {
            let script = format!("window.__MOONBASE_UI__ = {};", ui.page_config(app));
            respond(StatusCode::OK, "text/javascript; charset=utf-8", script.into_bytes())
        }
        "/logo" => match &ui.config.logo {
            Some(Logo::Svg(svg)) => respond(StatusCode::OK, "image/svg+xml", svg.as_bytes().to_vec()),
            Some(Logo::Png(png)) => respond(StatusCode::OK, "image/png", png.clone()),
            None => not_found(),
        },
        _ => not_found(),
    }
}

/// Let the UI call the plugin. Plugins cannot ship a capability file of their own,
/// so this one is added at runtime, scoped to the window the UI is in, just before
/// it first appears.
fn grant_ui_permissions<R: Runtime>(app: &AppHandle<R>, window: &str) -> Result<()> {
    let mut capability = tauri::ipc::CapabilityBuilder::new(format!("moonbase-ui-{window}"))
        .window(window)
        .local(true)
        .permission("moonbase:default");
    if window == WINDOW_LABEL {
        capability = capability.permission("core:window:allow-start-dragging");
    }
    app.add_capability(capability).map_err(Error::from)
}

fn eval_in<R: Runtime>(app: &AppHandle<R>, window: &str, script: String) {
    let Some(window) = app.get_webview_window(window) else {
        return;
    };
    if let Err(error) = window.eval(script) {
        log::debug!("moonbase: couldn't update the activation UI: {error}");
    }
}

impl<R: Runtime> Moonbase<R> {
    fn require_ui(&self) -> Result<&Ui> {
        self.ui().ok_or_else(|| Error::Internal("The built-in activation window is turned off".into()))
    }

    /// Hand the UI something new, wherever it is showing.
    fn push(&self, function: &str, payload: &impl Serialize) {
        let Some(ui) = self.ui() else {
            return;
        };
        let Ok(json) = serde_json::to_string(payload) else {
            return;
        };
        match ui.config.overlay_window() {
            Some(window) => {
                if *guard(&ui.overlay_open) {
                    let script = format!("window.__moonbaseOverlay && window.__moonbaseOverlay.{function}({json})");
                    eval_in(self.app(), window, script);
                }
            }
            None => {
                let script = format!("window.__moonbase && window.__moonbase.{function}({json})");
                eval_in(self.app(), WINDOW_LABEL, script);
            }
        }
    }

    fn grant_once(&self, ui: &Ui, window: &str) -> Result<()> {
        let mut granted = guard(&ui.granted);
        if !*granted {
            grant_ui_permissions(self.app(), window)?;
            *granted = true;
        }
        Ok(())
    }

    /// Mount the overlay in `window`, or bring it to `screen` if it is up. A window
    /// that does not exist yet (the app is still starting) gets it when its page
    /// loads.
    fn mount_overlay(&self, ui: &Ui, window: &str, screen: Option<Screen>) -> Result<()> {
        let Some(target) = self.app().get_webview_window(window) else {
            *guard(&ui.pending_screen) = screen;
            log::debug!("moonbase: no window labelled \"{window}\" yet; the overlay mounts when it loads");
            return Ok(());
        };
        let screen = screen.or_else(|| guard(&ui.pending_screen).take());
        let config = ui.page_config(self.app());
        let state = serde_json::to_string(&self.state()).map_err(|error| Error::Internal(error.to_string()))?;
        let screen = serde_json::to_string(&screen).map_err(|error| Error::Internal(error.to_string()))?;
        target
            .eval(format!("window.__moonbaseOverlay && window.__moonbaseOverlay.mount({config}, {state}, {screen})"))?;
        #[cfg(desktop)]
        {
            let _ = target.unminimize();
            let _ = target.show();
            let _ = target.set_focus();
        }
        Ok(())
    }

    /// A page finished loading in one of the app's webviews: put the overlay back if
    /// that page replaced one that showed it (a reload, or a navigation).
    pub(crate) fn on_page_load(&self, label: &str) {
        let Some(ui) = self.ui() else {
            return;
        };
        if ui.config.overlay_window() != Some(label) || !*guard(&ui.overlay_open) {
            return;
        }
        if let Err(error) = self.mount_overlay(ui, label, None) {
            log::warn!("moonbase: couldn't restore the activation overlay: {error}");
        }
    }

    /// Called after every state change: keep the window current, and present it
    /// when the app locks or opens with an update.
    pub(crate) fn after_state_change(&self, state: &LicenseState) {
        let Some(ui) = self.ui() else {
            return;
        };
        self.push("update", state);
        if state.status == Status::Loading {
            return;
        }
        let previous = guard(&ui.last_licensed).replace(state.licensed);
        if !state.licensed && previous != Some(false) && ui.config.auto_present_on_lock {
            self.present_quietly(None);
        } else if previous.is_none()
            && state.licensed
            && ui.config.auto_present_update
            && let Some(update) = &state.update
            && !ui.is_skipped(&update.latest_version)
        {
            self.present_quietly(Some(Screen::Update));
        }
    }

    fn present_quietly(&self, screen: Option<Screen>) {
        if let Err(error) = self.present(screen) {
            log::warn!("moonbase: couldn't present the activation UI: {error}");
        }
    }

    /// Show the activation window, creating it if needed, and bring it to the
    /// front; or, with [`Presentation::Overlay`], mount the overlay over its
    /// window. `screen` opens it on a particular [`Screen`]; without one it shows
    /// whatever the license state calls for.
    pub fn present(&self, screen: Option<Screen>) -> Result<()> {
        let ui = self.require_ui()?;
        if let Some(window) = ui.config.overlay_window() {
            self.grant_once(ui, window)?;
            *guard(&ui.overlay_open) = true;
            return self.mount_overlay(ui, window, screen);
        }
        #[cfg(not(desktop))]
        {
            let _ = (ui, screen);
            Err(Error::Internal("The built-in activation window is desktop-only".into()))
        }
        #[cfg(desktop)]
        {
            if let Some(window) = self.app().get_webview_window(WINDOW_LABEL) {
                if let Some(screen) = screen {
                    self.push("show", &screen);
                }
                let _ = window.unminimize();
                window.show()?;
                window.set_focus()?;
                return Ok(());
            }
            {
                let mut creating = guard(&ui.creating);
                if *creating {
                    *guard(&ui.pending_screen) = screen;
                    return Ok(());
                }
                *creating = true;
            }
            *guard(&ui.pending_screen) = screen;
            if let Err(error) = self.grant_once(ui, WINDOW_LABEL) {
                *guard(&ui.creating) = false;
                return Err(error);
            }
            let title = ui.config.window_title.clone().unwrap_or_else(|| ui.product_name(self.app()));
            let this = self.clone();
            // Off the calling thread: on Windows, creating a webview from the main
            // thread inside a synchronous handler deadlocks.
            std::thread::spawn(move || {
                let built = build_window(this.app(), &title);
                if let Some(ui) = this.ui() {
                    *guard(&ui.creating) = false;
                }
                match built {
                    Ok(()) => {
                        // Shown when the page reports its size; this covers a page
                        // that never does.
                        std::thread::sleep(Duration::from_millis(1500));
                        if let Some(window) = this.app().get_webview_window(WINDOW_LABEL)
                            && !window.is_visible().unwrap_or(true)
                        {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    Err(error) => log::warn!("moonbase: couldn't open the activation window: {error}"),
                }
            });
            Ok(())
        }
    }

    /// Close the activation window, or take the overlay down, if it is showing.
    pub fn close_ui(&self) -> Result<()> {
        let Some(ui) = self.ui() else {
            return Ok(());
        };
        match ui.config.overlay_window() {
            Some(window) => {
                *guard(&ui.overlay_open) = false;
                eval_in(self.app(), window, "window.__moonbaseOverlay && window.__moonbaseOverlay.unmount()".into());
            }
            None => {
                if let Some(window) = self.app().get_webview_window(WINDOW_LABEL) {
                    window.close()?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn ui_ready(&self, height: f64) -> Result<UiOpened> {
        let ui = self.require_ui()?;
        self.ui_resize(height)?;
        #[cfg(desktop)]
        if let Some(window) = self.app().get_webview_window(WINDOW_LABEL) {
            window.center()?;
            window.show()?;
            window.set_focus()?;
        }
        Ok(UiOpened { screen: guard(&ui.pending_screen).take() })
    }

    pub(crate) fn ui_resize(&self, height: f64) -> Result<()> {
        #[cfg(desktop)]
        if let Some(window) = self.app().get_webview_window(WINDOW_LABEL) {
            let height = height.clamp(240.0, 900.0);
            window.set_size(tauri::LogicalSize::new(WIDTH, height))?;
        }
        #[cfg(not(desktop))]
        let _ = height;
        Ok(())
    }

    /// Never prompt for this release again; a newer one still prompts.
    pub fn skip_update(&self, version: &str) -> Result<()> {
        self.require_ui()?.skip(version)
    }

    /// Open a link from the window in the browser: the pending activation, the
    /// store, or Moonbase's site from the footer. Nothing else, so the command
    /// cannot be used to open arbitrary URLs.
    pub(crate) fn open_link(&self, url: &str) -> Result<()> {
        let ui = self.require_ui()?;
        let parsed = tauri::Url::parse(url).map_err(|_| Error::Internal("Not a link".into()))?;
        let store = tauri::Url::parse(&ui.endpoint).ok();
        let same_origin = store.is_some_and(|store| store.origin() == parsed.origin());
        let pending =
            self.state().activation.and_then(|activation| activation.browser_url).is_some_and(|pending| pending == url);
        let moonbase = url == MOONBASE_SITE && ui.config.show_moonbase_badge;
        if !(same_origin || pending || moonbase) {
            return Err(Error::Internal("That link can't be opened from here".into()));
        }
        self.open_url(url).map_err(|error| Error::Internal(format!("Couldn't open the browser: {error}")))
    }

    fn reveal(&self, path: &Path) {
        let revealed = match self.app().try_state::<tauri_plugin_opener::Opener<R>>() {
            Some(opener) => opener.reveal_item_in_dir(path),
            None => tauri_plugin_opener::reveal_item_in_dir(path),
        };
        if let Err(error) = revealed {
            log::debug!("moonbase: couldn't reveal {}: {error}", path.display());
        }
    }

    fn downloads_dir(&self) -> Result<PathBuf> {
        let path = self.app().path();
        path.download_dir().or_else(|_| path.temp_dir()).map_err(Error::from)
    }

    /// Save the machine file for offline activation: through a save dialog when
    /// the app has registered `tauri-plugin-dialog`, otherwise into Downloads,
    /// revealed in the file manager. `None` when the user cancels.
    pub fn save_machine_file(&self) -> Result<Option<PathBuf>> {
        let ui = self.require_ui()?;
        let token = self.device_token()?;
        let name = format!("{}.dt", file_safe(&ui.product_name(self.app())));

        #[cfg(desktop)]
        if let Some(dialog) = self.app().try_state::<tauri_plugin_dialog::Dialog<R>>() {
            let mut picker = dialog
                .file()
                .set_title("Save machine file")
                .set_file_name(&name)
                .add_filter("Moonbase machine file", &["dt"]);
            let parent = ui.config.overlay_window().unwrap_or(WINDOW_LABEL);
            if let Some(window) = self.app().get_webview_window(parent) {
                picker = picker.set_parent(&window);
            }
            let Some(picked) = picker.blocking_save_file() else {
                return Ok(None);
            };
            let path = picked.into_path().map_err(|error| Error::Internal(error.to_string()))?;
            std::fs::write(&path, token)
                .map_err(|error| Error::Internal(format!("Couldn't save the machine file: {error}")))?;
            return Ok(Some(path));
        }

        let dir = self.downloads_dir()?;
        let path = unique_path(&dir, &name);
        std::fs::write(&path, token)
            .map_err(|error| Error::Internal(format!("Couldn't save the machine file: {error}")))?;
        self.reveal(&path);
        Ok(Some(path))
    }

    fn set_download(&self, ui: &Ui, next: DownloadState) {
        *guard(&ui.download) = next.clone();
        self.push("download", &next);
    }

    /// Download the latest installer for this platform into Downloads, reporting
    /// progress to the window, then reveal it.
    pub fn download_update(&self) -> Result<()> {
        let ui = self.require_ui()?;
        if guard(&ui.download).phase == DownloadPhase::Downloading {
            return Ok(());
        }
        self.set_download(ui, DownloadState { phase: DownloadPhase::Downloading, ..DownloadState::default() });
        match self.fetch_installer(ui) {
            Ok(path) => {
                self.reveal(&path);
                let shown = path.display().to_string();
                *guard(&ui.downloaded) = Some(path);
                self.set_download(
                    ui,
                    DownloadState {
                        phase: DownloadPhase::Done,
                        progress: Some(1.0),
                        path: Some(shown),
                        ..DownloadState::default()
                    },
                );
                Ok(())
            }
            Err(Error::Moonbase(moonbase::Error::ReleaseNotCovered(refusal))) => {
                // The screen swaps the download for the upgrade.
                log::info!("moonbase: the update download was refused: {refusal}");
                self.set_download(
                    ui,
                    DownloadState {
                        phase: DownloadPhase::NotCovered,
                        release_access: refusal.release_access().cloned(),
                        upgrade_url: refusal.upgrade_url().map(str::to_owned),
                        ..DownloadState::default()
                    },
                );
                Err(Error::Moonbase(moonbase::Error::ReleaseNotCovered(refusal)))
            }
            Err(error) => {
                log::warn!("moonbase: the update download failed: {error}");
                self.set_download(
                    ui,
                    DownloadState {
                        phase: DownloadPhase::Failed,
                        error: Some(format!("The download failed: {error}")),
                        ..DownloadState::default()
                    },
                );
                Err(error)
            }
        }
    }

    fn fetch_installer(&self, ui: &Ui) -> Result<PathBuf> {
        use std::io::{Read, Write};

        let target = self.update_download_url()?;
        let product = ui.product_name(self.app());
        let name = Some(file_safe(&target.filename))
            .filter(|name| name != "license")
            .unwrap_or_else(|| format!("{} installer", file_safe(&product)));
        let path = unique_path(&self.downloads_dir()?, &name);
        let mut partial = path.as_os_str().to_owned();
        partial.push(".part");
        let partial = PathBuf::from(partial);

        let agent =
            ureq::Agent::config_builder().timeout_connect(Some(std::time::Duration::from_secs(15))).build().new_agent();
        let response = agent.get(&target.url).call().map_err(|error| Error::Internal(error.to_string()))?;
        let total = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|total| *total > 0);
        let mut reader = response.into_body().into_reader();
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&partial)?;
            let mut buffer = vec![0u8; 64 * 1024];
            let mut received = 0u64;
            #[cfg(desktop)]
            let mut last_report = Instant::now();
            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                file.write_all(&buffer[..read])?;
                received += read as u64;
                #[cfg(desktop)]
                if last_report.elapsed() > Duration::from_millis(120) {
                    last_report = Instant::now();
                    self.set_download(
                        ui,
                        DownloadState {
                            phase: DownloadPhase::Downloading,
                            progress: total.map(|total| (received as f64 / total as f64).min(1.0)),
                            ..DownloadState::default()
                        },
                    );
                }
            }
            file.flush()?;
            drop(file);
            std::fs::rename(&partial, &path)
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&partial);
            return Err(Error::Internal(error.to_string()));
        }
        Ok(path)
    }

    /// Show the downloaded installer in the file manager again.
    pub fn reveal_update(&self) -> Result<()> {
        let ui = self.require_ui()?;
        let path =
            guard(&ui.downloaded).clone().ok_or_else(|| Error::Internal("Nothing has been downloaded".into()))?;
        self.reveal(&path);
        Ok(())
    }
}

#[cfg(desktop)]
fn build_window<R: Runtime>(app: &AppHandle<R>, title: &str) -> Result<()> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    // Custom schemes are `scheme://localhost` where the webview supports them, and
    // `http://scheme.localhost` on Windows.
    let url = if cfg!(windows) { "http://moonbase.localhost/index.html" } else { "moonbase://localhost/index.html" };
    let url = url.parse().map_err(|_| Error::Internal("Invalid window URL".into()))?;

    let mut builder = WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::CustomProtocol(url))
        .title(title)
        .inner_size(WIDTH, 560.0)
        .resizable(false)
        .maximizable(false)
        .visible(false)
        .focused(true)
        .center()
        .theme(Some(tauri::Theme::Dark))
        .background_color(tauri::window::Color(8, 11, 19, 255))
        // The page takes dropped license files itself.
        .disable_drag_drop_handler();
    #[cfg(target_os = "macos")]
    {
        builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);
    }
    if let Some(main) = app.get_webview_window("main") {
        builder = builder.parent(&main)?;
    }
    builder.build()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(file_safe("Solstice"), "Solstice");
        assert_eq!(file_safe("A/B: C?"), "A-B- C-");
        assert_eq!(file_safe(" .. "), "license");
    }

    #[test]
    fn a_taken_path_gets_a_numbered_sibling() {
        let dir = std::env::temp_dir().join(format!("moonbase-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Solstice.dt"), "").unwrap();
        assert_eq!(unique_path(&dir, "Solstice.dt"), dir.join("Solstice (2).dt"));
        assert_eq!(unique_path(&dir, "Other.dt"), dir.join("Other.dt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skipped_versions_survive_and_keep_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("moonbase-ui-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ui = Ui::new(UiConfig::new(), "https://demo.moonbase.sh".into(), &dir.join("license.mb"));
        std::fs::write(&ui.state_file, r#"{"fromANewerBuild":true}"#).unwrap();
        assert!(!ui.is_skipped("2.0.0"));
        ui.skip("2.0.0").unwrap();
        ui.skip("2.0.0").unwrap();
        assert!(ui.is_skipped("2.0.0"));
        assert!(!ui.is_skipped("2.1.0"));
        let saved: Value = serde_json::from_slice(&std::fs::read(&ui.state_file).unwrap()).unwrap();
        assert_eq!(saved["fromANewerBuild"], true);
        assert_eq!(saved["ignoredUpdates"], json!(["2.0.0"]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod serving {
    use moonbase::{ApiError, HttpRequest, HttpResponse, HttpTransport, StaticDeviceIdResolver};
    use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};

    use super::*;

    struct Offline;

    impl HttpTransport for Offline {
        fn send(&self, _request: &HttpRequest) -> moonbase::Result<HttpResponse> {
            Err(ApiError::unreachable("offline").into())
        }
    }

    fn app(dir: &Path, ui: UiConfig) -> tauri::App<MockRuntime> {
        mock_builder()
            .plugin(
                crate::Builder::new(
                    "https://helio.moonbase.sh",
                    "solstice",
                    include_str!("../tests/fixtures/signing-public.pem"),
                )
                .license_file(dir.join("license.mb"))
                .device_id_resolver(StaticDeviceIdResolver::new("Studio", "device-id"))
                .transport(Offline)
                // The mock runtime has no webviews to present the window in.
                .ui(ui.auto_present_on_lock(false))
                .build(),
            )
            .build(mock_context(noop_assets()))
            .unwrap()
    }

    fn get(app: &tauri::App<MockRuntime>, path: &str) -> Response<Vec<u8>> {
        let request = Request::builder().uri(format!("moonbase://localhost{path}")).body(Vec::new()).unwrap();
        serve(app.handle(), &request)
    }

    fn text(response: &Response<Vec<u8>>) -> String {
        String::from_utf8(response.body().clone()).unwrap()
    }

    #[test]
    fn the_page_is_served_with_a_strict_content_security_policy() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path(), UiConfig::new());
        let page = get(&app, "/index.html");
        assert_eq!(page.status(), StatusCode::OK);
        assert!(page.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
        let csp = page.headers()["Content-Security-Policy"].to_str().unwrap();
        assert!(csp.contains("script-src 'self'") && csp.contains("connect-src ipc: http://ipc.localhost"));
        assert!(text(&page).contains("config.js"));
        for path in ["/", "/ui.css", "/ui.js", "/window.js"] {
            assert_eq!(get(&app, path).status(), StatusCode::OK, "{path}");
        }
        assert_eq!(get(&app, "/../Cargo.toml").status(), StatusCode::NOT_FOUND);
        assert_eq!(get(&app, "/logo").status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn the_config_carries_the_branding() {
        let dir = tempfile::tempdir().unwrap();
        let ui = UiConfig::new()
            .product_name("Solstice")
            .manufacturer_name("Helio Audio")
            .accent("#4f46e5")
            .logo_svg("<svg xmlns=\"http://www.w3.org/2000/svg\"/>")
            .trial_feature("Full DSP engine", true)
            .show_addons(true);
        let app = app(dir.path(), ui);

        let script = text(&get(&app, "/config.js"));
        let json: Value =
            serde_json::from_str(script.trim_start_matches("window.__MOONBASE_UI__ = ").trim_end_matches(';')).unwrap();
        assert_eq!(json["productName"], "Solstice");
        assert_eq!(json["manufacturerName"], "Helio Audio");
        assert_eq!(json["accent"], "#4f46e5");
        // An SVG logo is inlined, so it never needs loading.
        assert_eq!(json["logo"], Value::Null);
        assert!(json["logoSvg"].as_str().unwrap().starts_with("<svg"));
        assert_eq!(json["activationHost"], "helio.moonbase.sh");
        assert_eq!(json["requestFileName"], "Solstice.dt");
        assert_eq!(json["trialFeatures"][0]["label"], "Full DSP engine");
        assert_eq!(json["showAddOns"], true);

        let logo = get(&app, "/logo");
        assert_eq!(logo.headers()[header::CONTENT_TYPE], "image/svg+xml");
    }

    #[test]
    fn an_overlay_gets_its_logo_inline_and_its_code_injected() {
        let dir = tempfile::tempdir().unwrap();
        let png = vec![0x89, b'P', b'N', b'G'];
        let ui = UiConfig::new().presentation(Presentation::overlay()).logo_png(png);
        assert_eq!(ui.overlay_window(), Some("main"));
        let app = app(dir.path(), ui);
        let moonbase = app.state::<Moonbase<MockRuntime>>();
        let config = moonbase.ui().unwrap().page_config(app.handle());
        // The app's page cannot load from the plugin's scheme, so the bytes travel along.
        assert_eq!(config["logo"], "data:image/png;base64,iVBORw==");

        let script = overlay_script();
        assert!(script.starts_with("(function () {"));
        assert!(script.contains("const CSS = \"/*"));
        assert!(script.contains("function createActivationUI"));
        assert!(script.contains("__moonbaseOverlay"));
        assert_eq!(Presentation::overlay_on("editor"), Presentation::Overlay { window: "editor".into() });
        assert_eq!(UiConfig::new().overlay_window(), None);
    }

    #[test]
    fn a_disabled_window_serves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path(), UiConfig::disabled());
        assert_eq!(get(&app, "/index.html").status(), StatusCode::NOT_FOUND);
    }
}
