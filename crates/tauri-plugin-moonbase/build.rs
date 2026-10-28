const COMMANDS: &[&str] = &[
    "state",
    "activate",
    "cancel_activation",
    "refresh",
    "deactivate",
    "forget",
    "device_token",
    "activate_offline",
    "dismiss_error",
    "device",
    "check_update",
    "update_download_url",
    "present",
    "close_ui",
    "ui_ready",
    "ui_resize",
    "save_machine_file",
    "download_update",
    "reveal_update",
    "skip_update",
    "open_link",
    "load_add_ons",
    "refresh_add_ons",
    "activate_sub_product",
    "request_sub_product_trial",
    "buy_sub_product",
    "upgrade_license",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).global_api_script_path("./api-iife.js").build();
}
