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
    embed_manifest_for_tests();
}

/// The plugin's test executables load Tauri and the dialog plugin, which import
/// from Common Controls 6. An app selects that version through the manifest
/// tauri-build embeds; a test executable has none, so Windows loads version 5 and
/// the test dies at startup with STATUS_ENTRYPOINT_NOT_FOUND. As Tauri's own crate
/// does for its tests, embed one, in this repository only: link arguments reach
/// only this package's executables, which no app builds.
fn embed_manifest_for_tests() {
    println!("cargo:rerun-if-env-changed=MOONBASE_RS_WORKSPACE");
    let in_repository = std::env::var("MOONBASE_RS_WORKSPACE").is_ok_and(|value| value == "true");
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    if !in_repository || !windows_msvc {
        return;
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/windows-test-manifest.xml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
}
