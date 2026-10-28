//! # Moonbase licensing for Tauri
//!
//! License activation for a Tauri 2 app, built on the [`moonbase`] SDK. Register
//! the plugin with your store's details and it validates the stored license at
//! startup, runs browser and offline activation, trials and deactivation, watches
//! for expiry, and tells your frontend about every change:
//!
//! ```no_run
//! # const PUBLIC_KEY: &str = "";
//! tauri::Builder::default()
//!     .plugin(tauri_plugin_opener::init())
//!     .plugin(
//!         tauri_plugin_moonbase::Builder::new("https://your-tenant.moonbase.sh", "your-product", PUBLIC_KEY)
//!             .build(),
//!     )
//! #   ;
//! ```
//!
//! From the frontend, with `"withGlobalTauri": true`:
//!
//! ```js
//! const { moonbase } = window.__TAURI__;
//! moonbase.onStateChanged((state) => render(state));
//! render(await moonbase.state());
//! await moonbase.activate(); // opens the browser, then polls
//! ```
//!
//! Or invoke the commands directly: `invoke('plugin:moonbase|state')`. Grant them
//! with the `moonbase:default` permission in your capability file.
//!
//! Gate features, and add-ons, in Rust with [`MoonbaseExt`]:
//!
//! ```no_run
//! use tauri_plugin_moonbase::MoonbaseExt;
//! # fn export(app: &tauri::AppHandle) {
//! if !app.moonbase().is_licensed() {
//!     return;
//! }
//! if app.moonbase().has_sub_product("gradients") {
//!     // the add-on is unlocked on this device
//! }
//! # }
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use moonbase::{
    DeviceIdFallback, DeviceIdResolver, FileLicenseStore, HttpTransport, Licensing, LicensingOptions,
    MoonbaseDeviceIdResolver, MoonbaseDeviceIdResolverOptions,
};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::webview::PageLoadEvent;
use tauri::{Manager, RunEvent, Runtime, WindowEvent};

mod commands;
mod controller;
mod error;
mod models;
mod ui;

pub use crate::controller::{Moonbase, STATE_CHANGED_EVENT};
pub use crate::error::{Error, Result};
pub use crate::models::{
    AddOnsPhase, AddOnsState, DeviceInfo, Failure, LicenseState, LicenseView, LockReason, NotCovered,
    PendingActivation, Status, UpdateHint, UpdateInfo,
};
pub use crate::ui::{Presentation, Screen, TrialFeature, UiConfig, UiStrings, WINDOW_LABEL};
pub use moonbase;

/// The plugin's version, as reported in the `User-Agent`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Access the plugin's [`Moonbase`] controller from an `App`, `AppHandle`,
/// `Window` or anything else that implements [`Manager`].
pub trait MoonbaseExt<R: Runtime> {
    fn moonbase(&self) -> &Moonbase<R>;
}

impl<R: Runtime, T: Manager<R>> MoonbaseExt<R> for T {
    fn moonbase(&self) -> &Moonbase<R> {
        self.state::<Moonbase<R>>().inner()
    }
}

/// Configures and builds the plugin.
#[must_use]
pub struct Builder {
    options: LicensingOptions,
    license_file: Option<PathBuf>,
    device_id_resolver: Option<Arc<dyn DeviceIdResolver>>,
    transport: Option<Arc<dyn HttpTransport>>,
    allow_device_name_fallback: bool,
    open_browser: bool,
    ui: UiConfig,
}

impl Builder {
    /// Your store (`https://<tenant>.moonbase.sh`), the product id, and the
    /// product's public key (PEM or base64 DER), all from the Moonbase dashboard.
    pub fn new(endpoint: impl Into<String>, product_id: impl Into<String>, public_key: impl Into<String>) -> Self {
        Self {
            options: LicensingOptions::new(endpoint, product_id, public_key),
            license_file: None,
            device_id_resolver: None,
            transport: None,
            allow_device_name_fallback: false,
            open_browser: true,
            ui: UiConfig::default(),
        }
    }

    /// Pin the token issuer to your Moonbase account id.
    pub fn account_id(mut self, account_id: impl Into<String>) -> Self {
        self.options.account_id = Some(account_id.into());
        self
    }

    /// The version reported to Moonbase and compared with the latest release, and
    /// the version a license that covers only some versions has to cover.
    /// Defaults to the app's own version from `tauri.conf.json`.
    pub fn application_version(mut self, version: impl Into<String>) -> Self {
        self.options.application_version = Some(version.into());
        self
    }

    /// Whether a license that doesn't cover the app's version is refused on this
    /// device too, when Moonbase isn't asked (an offline license, or within the
    /// check interval). Moonbase refuses one when validating online either way.
    /// On by default.
    pub fn enforce_release_access(mut self, enforce: bool) -> Self {
        self.options.enforce_release_access = enforce;
        self
    }

    /// Whether activating and validating also takes a seat of each add-on (a
    /// sub-product the merchant includes in this product's license) that the
    /// customer owns, so the license reports it. On by default. Turn it off to take
    /// seats yourself with [`Moonbase::activate_sub_product`]; the add-ons screen
    /// then offers "Activate" for each one owned.
    pub fn auto_activate_sub_products(mut self, auto_activate: bool) -> Self {
        self.options.auto_activate_sub_products = auto_activate;
        self
    }

    /// Where the license is kept. Defaults to `license.mb` in the app's data
    /// directory.
    pub fn license_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.license_file = Some(path.into());
        self
    }

    /// How long an online-activated license keeps working without a successful
    /// online check. Default 7 days.
    pub fn grace_period(mut self, grace: Duration) -> Self {
        self.options.online_validation_grace_period = grace;
        self
    }

    /// The least time between online checks of the license. Default 5 minutes.
    pub fn check_interval(mut self, interval: Duration) -> Self {
        self.options.online_validation_min_interval = interval;
        self
    }

    pub fn http_timeouts(mut self, connect: Duration, request: Duration) -> Self {
        self.options = self.options.http_timeouts(connect, request);
        self
    }

    /// An extra `User-Agent` segment for a layer built on top of this plugin, such
    /// as `"MyFramework/2.0"`. Appended after the plugin's own.
    pub fn client_info(mut self, client_info: impl Into<String>) -> Self {
        self.options.client_info = Some(client_info.into());
        self
    }

    /// Sent as `meta[<key>]` with license requests.
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.options = self.options.metadata(key, value);
        self
    }

    /// Override how this machine is identified. Defaults to the cross-SDK
    /// [`MoonbaseDeviceIdResolver`], so a license activated through any Moonbase
    /// SDK on this machine validates here too. Pass the resolver itself, or an
    /// `Arc` of one.
    pub fn device_id_resolver(mut self, resolver: impl DeviceIdResolver + 'static) -> Self {
        self.device_id_resolver = Some(Arc::new(resolver));
        self
    }

    /// Send requests through your own [`HttpTransport`], e.g. one that shares the
    /// app's proxy settings. Defaults to the SDK's bundled transport. Pass the
    /// transport itself, or an `Arc` of one.
    pub fn transport(mut self, transport: impl HttpTransport + 'static) -> Self {
        self.transport = Some(Arc::new(transport));
        self
    }

    /// Accept a deliberately weaker device id (a hash of the host name, stamped
    /// `mbd2n_`) on machines with no readable hardware identity, instead of
    /// refusing to activate. Ignored with a custom resolver.
    pub fn allow_device_name_fallback(mut self, allow: bool) -> Self {
        self.allow_device_name_fallback = allow;
        self
    }

    /// Whether `activate` opens the browser itself. On by default; turn it off to
    /// open [`PendingActivation::browser_url`] yourself.
    pub fn open_browser(mut self, open: bool) -> Self {
        self.open_browser = open;
        self
    }

    /// Brand and tune the built-in activation window, or turn it off with
    /// [`UiConfig::disabled`] to drive your own UI from the state event.
    pub fn ui(mut self, ui: UiConfig) -> Self {
        self.ui = ui;
        self
    }

    pub fn build<R: Runtime>(self) -> TauriPlugin<R> {
        // An overlay mounts inside the app's own pages, so its code rides along in
        // every webview, dormant until the plugin mounts it.
        let overlay_script = (self.ui.enabled && self.ui.overlay_window().is_some()).then(ui::overlay_script);
        let mut plugin = PluginBuilder::new("moonbase");
        if let Some(script) = overlay_script {
            plugin = plugin.js_init_script(script);
        }
        plugin
            .invoke_handler(tauri::generate_handler![
                commands::state,
                commands::activate,
                commands::cancel_activation,
                commands::refresh,
                commands::deactivate,
                commands::forget,
                commands::device_token,
                commands::activate_offline,
                commands::dismiss_error,
                commands::device,
                commands::check_update,
                commands::update_download_url,
                commands::present,
                commands::close_ui,
                commands::ui_ready,
                commands::ui_resize,
                commands::save_machine_file,
                commands::download_update,
                commands::reveal_update,
                commands::skip_update,
                commands::open_link,
                commands::load_add_ons,
                commands::refresh_add_ons,
                commands::activate_sub_product,
                commands::request_sub_product_trial,
                commands::buy_sub_product,
                commands::upgrade_license,
            ])
            .register_uri_scheme_protocol(ui::SCHEME, |context, request| ui::serve(context.app_handle(), &request))
            .setup(move |app, _api| {
                let mut options = self.options;
                if options.application_version.is_none() {
                    options.application_version = Some(app.package_info().version.to_string());
                }
                let segment =
                    format!("tauri-plugin-moonbase/{VERSION} (Tauri {}; {})", tauri::VERSION, std::env::consts::OS);
                options.client_info = Some(match options.client_info.take() {
                    Some(outer) => format!("{segment} {outer}"),
                    None => segment,
                });

                let license_file = match self.license_file {
                    Some(path) => path,
                    None => app.path().app_data_dir()?.join("license.mb"),
                };
                let store = FileLicenseStore::new(license_file);
                let resolver: Arc<dyn DeviceIdResolver> = self.device_id_resolver.unwrap_or_else(|| {
                    Arc::new(MoonbaseDeviceIdResolver::with_options(MoonbaseDeviceIdResolverOptions {
                        fallback: if self.allow_device_name_fallback {
                            DeviceIdFallback::DeviceName
                        } else {
                            DeviceIdFallback::None
                        },
                        ..Default::default()
                    }))
                });

                let settings = controller::Settings {
                    grace_period: options.online_validation_grace_period,
                    check_interval: options.online_validation_min_interval,
                    application_version: options.application_version.clone(),
                    open_browser: self.open_browser,
                    auto_activate_sub_products: options.auto_activate_sub_products,
                };
                let mut licensing = Licensing::builder(options).store(store.clone()).device_id_resolver(resolver);
                if let Some(transport) = self.transport {
                    licensing = licensing.transport(transport);
                }
                let licensing = licensing.build()?;

                let ui =
                    self.ui.enabled.then(|| ui::Ui::new(self.ui, licensing.options().endpoint.clone(), store.path()));
                let moonbase = Moonbase::new(app.clone(), licensing, store, settings, ui);
                moonbase.start();
                app.manage(moonbase);
                Ok(())
            })
            .on_page_load(|webview, payload| {
                if payload.event() == PageLoadEvent::Finished
                    && let Some(moonbase) = webview.try_state::<Moonbase<R>>()
                {
                    moonbase.on_page_load(webview.label());
                }
            })
            .on_event(|app, event| {
                let Some(moonbase) = app.try_state::<Moonbase<R>>() else {
                    return;
                };
                match event {
                    RunEvent::Exit => moonbase.shut_down(),
                    // Coming back from the browser after buying an add-on.
                    RunEvent::WindowEvent { event: WindowEvent::Focused(focused), .. } => {
                        moonbase.on_focus_changed(*focused);
                    }
                    _ => {}
                }
            })
            .build()
    }
}

impl std::fmt::Debug for Builder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builder")
            .field("endpoint", &self.options.endpoint)
            .field("product_id", &self.options.product_id)
            .field("license_file", &self.license_file)
            .finish_non_exhaustive()
    }
}
