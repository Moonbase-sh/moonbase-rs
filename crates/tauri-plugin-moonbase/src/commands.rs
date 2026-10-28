//! The commands the frontend invokes as `plugin:moonbase|<name>`. Every one runs on
//! the blocking pool: the controller can wait on the license file's lock, which
//! another process may hold for a whole network check, and that must never block
//! the main thread.

use moonbase::{ActivationMethod, DownloadTarget};
use tauri::{AppHandle, Runtime, command};

use crate::MoonbaseExt as _;
use crate::controller::Moonbase;
use crate::error::{Error, Result};
use crate::models::{DeviceInfo, LicenseState, UpdateInfo};
use crate::ui::{Screen, UiOpened};

async fn blocking<R: Runtime, T: Send + 'static>(
    moonbase: &Moonbase<R>,
    work: impl FnOnce(Moonbase<R>) -> Result<T> + Send + 'static,
) -> Result<T> {
    let moonbase = moonbase.clone();
    tauri::async_runtime::spawn_blocking(move || work(moonbase))
        .await
        .map_err(|error| Error::Internal(format!("The licensing worker stopped: {error}")))?
}

#[command]
pub(crate) async fn state<R: Runtime>(app: AppHandle<R>) -> Result<LicenseState> {
    blocking(app.moonbase(), |moonbase| Ok(moonbase.state())).await
}

#[command]
pub(crate) async fn activate<R: Runtime>(app: AppHandle<R>, offline: Option<bool>) -> Result<LicenseState> {
    let method = if offline.unwrap_or(false) { ActivationMethod::Offline } else { ActivationMethod::Online };
    blocking(app.moonbase(), move |moonbase| moonbase.activate(method)).await
}

#[command]
pub(crate) async fn cancel_activation<R: Runtime>(app: AppHandle<R>) -> Result<LicenseState> {
    blocking(app.moonbase(), |moonbase| Ok(moonbase.cancel_activation())).await
}

#[command]
pub(crate) async fn refresh<R: Runtime>(app: AppHandle<R>, force: Option<bool>) -> Result<LicenseState> {
    blocking(app.moonbase(), move |moonbase| moonbase.refresh(force.unwrap_or(true))).await
}

#[command]
pub(crate) async fn deactivate<R: Runtime>(app: AppHandle<R>) -> Result<LicenseState> {
    blocking(app.moonbase(), |moonbase| moonbase.deactivate()).await
}

#[command]
pub(crate) async fn forget<R: Runtime>(app: AppHandle<R>) -> Result<LicenseState> {
    blocking(app.moonbase(), |moonbase| Ok(moonbase.forget())).await
}

#[command]
pub(crate) async fn device_token<R: Runtime>(app: AppHandle<R>) -> Result<String> {
    blocking(app.moonbase(), |moonbase| moonbase.device_token()).await
}

#[command]
pub(crate) async fn activate_offline<R: Runtime>(app: AppHandle<R>, license_file: String) -> Result<LicenseState> {
    blocking(app.moonbase(), move |moonbase| moonbase.activate_offline(&license_file)).await
}

#[command]
pub(crate) async fn dismiss_error<R: Runtime>(app: AppHandle<R>) -> Result<LicenseState> {
    blocking(app.moonbase(), |moonbase| Ok(moonbase.dismiss_error())).await
}

#[command]
pub(crate) async fn device<R: Runtime>(app: AppHandle<R>) -> Result<DeviceInfo> {
    blocking(app.moonbase(), |moonbase| Ok(moonbase.device())).await
}

#[command]
pub(crate) async fn check_update<R: Runtime>(app: AppHandle<R>) -> Result<UpdateInfo> {
    blocking(app.moonbase(), |moonbase| moonbase.check_update()).await
}

#[command]
pub(crate) async fn update_download_url<R: Runtime>(app: AppHandle<R>) -> Result<DownloadTarget> {
    blocking(app.moonbase(), |moonbase| moonbase.update_download_url()).await
}

#[command]
pub(crate) async fn present<R: Runtime>(app: AppHandle<R>, screen: Option<Screen>) -> Result<()> {
    blocking(app.moonbase(), move |moonbase| moonbase.present(screen)).await
}

#[command]
pub(crate) async fn close_ui<R: Runtime>(app: AppHandle<R>) -> Result<()> {
    blocking(app.moonbase(), |moonbase| moonbase.close_ui()).await
}

#[command]
pub(crate) async fn ui_ready<R: Runtime>(app: AppHandle<R>, height: f64) -> Result<UiOpened> {
    blocking(app.moonbase(), move |moonbase| moonbase.ui_ready(height)).await
}

#[command]
pub(crate) async fn ui_resize<R: Runtime>(app: AppHandle<R>, height: f64) -> Result<()> {
    blocking(app.moonbase(), move |moonbase| moonbase.ui_resize(height)).await
}

#[command]
pub(crate) async fn save_machine_file<R: Runtime>(app: AppHandle<R>) -> Result<Option<String>> {
    blocking(app.moonbase(), |moonbase| {
        moonbase.save_machine_file().map(|path| path.map(|path| path.display().to_string()))
    })
    .await
}

#[command]
pub(crate) async fn download_update<R: Runtime>(app: AppHandle<R>) -> Result<()> {
    blocking(app.moonbase(), |moonbase| moonbase.download_update()).await
}

#[command]
pub(crate) async fn reveal_update<R: Runtime>(app: AppHandle<R>) -> Result<()> {
    blocking(app.moonbase(), |moonbase| moonbase.reveal_update()).await
}

#[command]
pub(crate) async fn skip_update<R: Runtime>(app: AppHandle<R>, version: String) -> Result<()> {
    blocking(app.moonbase(), move |moonbase| moonbase.skip_update(&version)).await
}

#[command]
pub(crate) async fn open_link<R: Runtime>(app: AppHandle<R>, url: String) -> Result<()> {
    blocking(app.moonbase(), move |moonbase| moonbase.open_link(&url)).await
}

#[command]
pub(crate) async fn load_add_ons<R: Runtime>(app: AppHandle<R>) -> Result<LicenseState> {
    blocking(app.moonbase(), |moonbase| moonbase.load_add_ons()).await
}

#[command]
pub(crate) async fn refresh_add_ons<R: Runtime>(app: AppHandle<R>) -> Result<LicenseState> {
    blocking(app.moonbase(), |moonbase| moonbase.refresh_add_ons()).await
}

#[command]
pub(crate) async fn activate_sub_product<R: Runtime>(app: AppHandle<R>, id: String) -> Result<LicenseState> {
    blocking(app.moonbase(), move |moonbase| moonbase.activate_sub_product(&id)).await
}

#[command]
pub(crate) async fn request_sub_product_trial<R: Runtime>(app: AppHandle<R>, id: String) -> Result<LicenseState> {
    blocking(app.moonbase(), move |moonbase| moonbase.request_sub_product_trial(&id)).await
}

#[command]
pub(crate) async fn buy_sub_product<R: Runtime>(app: AppHandle<R>, id: String) -> Result<()> {
    blocking(app.moonbase(), move |moonbase| moonbase.buy_sub_product(&id)).await
}

#[command]
pub(crate) async fn upgrade_license<R: Runtime>(app: AppHandle<R>) -> Result<LicenseState> {
    blocking(app.moonbase(), |moonbase| moonbase.upgrade_license()).await
}
