# tauri-plugin-moonbase

[Moonbase](https://moonbase.sh) license activation for Tauri 2 apps. Register the
plugin and it validates the stored license at startup, runs browser and offline
activation, trials, deactivation, add-ons and update downloads, locks with an upgrade
offer when a license doesn't cover the running version, watches for expiry and for
other instances, and presents a finished, themeable activation window whenever the app
is locked.

```rust
use tauri_plugin_moonbase::UiConfig;

tauri::Builder::default()
    .plugin(tauri_plugin_opener::init())
    .plugin(
        tauri_plugin_moonbase::Builder::new(ENDPOINT, PRODUCT_ID, PUBLIC_KEY)
            .ui(UiConfig::new()
                .product_name("Solstice")
                .manufacturer_name("Helio Audio")
                .accent("#186cdc")
                .trial_feature("Full DSP engine, unrestricted", true))
            .build(),
    )
```

To lock the app rather than open a window beside it, present the same UI as an overlay
over the app's window, which stays blurred and inert until it is licensed:

```rust
.ui(UiConfig::new().product_name("Solstice").presentation(Presentation::overlay()))
```

Gate paid features in Rust, where the webview cannot skip the check, and open the
UI on demand:

```rust
use tauri_plugin_moonbase::MoonbaseExt;

if !app.moonbase().is_licensed() {
    app.moonbase().present(None)?;
    return Err("Exporting needs a license.".into());
}
if app.moonbase().has_sub_product("drum-kit") {
    // an add-on the license includes, unlocked on this device
}
```

`UiConfig::show_addons(true)` adds a screen to try, buy and activate the product's
add-ons.

Prefer your own UI? `UiConfig::disabled()` turns the window off; drive yours from the
`moonbase://state-changed` event and the commands, granted with `moonbase:default`:

```js
const { moonbase } = window.__TAURI__ // with "withGlobalTauri": true, or import from guest-js/

await moonbase.onStateChanged(render)
render(await moonbase.state())
await moonbase.activate()
```

- [Guide](https://github.com/Moonbase-sh/moonbase-rs/blob/main/docs/tauri.md)
- [Example app](https://github.com/Moonbase-sh/moonbase-rs/tree/main/examples/tauri-app)
- [Frontend bindings and types](guest-js/)

License: MIT
