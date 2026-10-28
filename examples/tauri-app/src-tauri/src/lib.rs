//! Aurora: a palette generator whose export is a paid feature, licensed through
//! Moonbase.
//!
//! The licensing lives in `tauri-plugin-moonbase`: registering it below gives the
//! app a finished activation window (online and offline activation, trials,
//! license details, updates, and an upgrade screen for a license that doesn't
//! cover this version) that presents itself whenever the app is locked. What this
//! file adds is the app's own side of it: branding that window, and gating the
//! paid feature in Rust.

use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_moonbase::{MoonbaseExt, Presentation, UiConfig};

/// The public demo store, so the example activates out of the box. Point these at
/// your own store and product from the Moonbase dashboard.
const MOONBASE_ENDPOINT: &str = "https://demo.moonbase.sh";
const MOONBASE_PRODUCT_ID: &str = "demo-app";
const MOONBASE_PUBLIC_KEY: &str = include_str!("../moonbase-public-key.pem");

/// The paid feature: save the palette as CSS custom properties.
///
/// Gated here, in Rust, on the plugin's lock-free flag: the check the webview
/// cannot skip. Without a license it brings up the activation window instead.
#[tauri::command]
async fn export_palette(app: AppHandle, colors: Vec<String>) -> Result<Option<String>, String> {
    if !app.moonbase().is_licensed() {
        let _ = app.moonbase().present(None);
        return Err("Exporting palettes needs a license.".into());
    }
    if colors.is_empty() || !colors.iter().all(|color| is_hex_color(color)) {
        return Err("That palette doesn't look right.".into());
    }

    let Some(path) = app
        .dialog()
        .file()
        .set_title("Export palette")
        .set_file_name("palette.css")
        .add_filter("CSS", &["css"])
        .blocking_save_file()
    else {
        return Ok(None);
    };
    let path = path.into_path().map_err(|error| error.to_string())?;

    let mut css = String::from(":root {\n");
    for (index, color) in colors.iter().enumerate() {
        css.push_str(&format!("  --aurora-{}: {color};\n", index + 1));
    }
    css.push_str("}\n");
    std::fs::write(&path, css).map_err(|error| format!("Couldn't save the palette: {error}"))?;
    Ok(Some(path.display().to_string()))
}

fn is_hex_color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Aurora's shuffling is free, so by default activation is its own window and the
    // app stays usable. Run with AURORA_PRESENTATION=overlay to see the other
    // option: the activation screen over the main window, locking the app until it
    // is licensed.
    let presentation = match std::env::var("AURORA_PRESENTATION").as_deref() {
        Ok("overlay") => Presentation::overlay(),
        _ => Presentation::Window,
    };
    let activation_window = UiConfig::new()
        .presentation(presentation)
        .product_name("Aurora")
        .manufacturer_name("Halcyon Tools")
        .accent("#3f8cff")
        .logo_svg(include_str!("../icons/icon.svg"))
        .trial_feature("Unlimited palettes and colour locks", true)
        .trial_feature("Export palettes as CSS", true)
        .trial_feature("Gradient export", false)
        .trial_feature("Use in commercial work", false);
    // A product with add-ons (sub-products the store includes in its license) can
    // list them from the license details, to try, buy and activate, and gate them
    // with `app.moonbase().has_sub_product("...")`. The demo store sells none for
    // this product, so the screen stays off here:
    // let activation_window = activation_window.show_addons(true);

    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().level(log::LevelFilter::Info).build())
        .plugin(tauri_plugin_opener::init())
        // Also gives the activation window a native "Save" dialog for the
        // offline machine file.
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_moonbase::Builder::new(MOONBASE_ENDPOINT, MOONBASE_PRODUCT_ID, MOONBASE_PUBLIC_KEY)
                .ui(activation_window)
                .build(),
        )
        .invoke_handler(tauri::generate_handler![export_palette])
        .run(tauri::generate_context!())
        .expect("error while running Aurora");
}
