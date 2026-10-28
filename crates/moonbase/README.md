# moonbase

License activation for desktop apps against [Moonbase](https://moonbase.sh): browser
and offline activation, local RS256 validation with an offline grace period,
revocation, trials, add-ons, licenses that cover only some versions, update checks,
pluggable storage and HTTP, and the cross-SDK device fingerprint, so a license
activated through any Moonbase SDK validates here too.

The Rust port of [moonbase-cpp](https://github.com/Moonbase-sh/moonbase-cpp).

```rust
use std::{thread, time::Duration};
use moonbase::{ActivationMethod, FileLicenseStore, Licensing, LicensingOptions};

let licensing = Licensing::builder(LicensingOptions::new(
    "https://your-tenant.moonbase.sh",
    "your-product",
    PUBLIC_KEY_PEM,
))
.store(FileLicenseStore::new("license.mb"))
.build()?;

// Validate what is stored, re-checking online when it is due.
if let Some(license) = licensing.load_and_validate()? {
    println!("Licensed to {}", license.issued_to.email);
} else {
    // Otherwise activate through the browser; the license is stored once it lands.
    let request = licensing.request_activation(ActivationMethod::Online)?;
    println!("Open {}", request.browser_url);
    let license = loop {
        thread::sleep(Duration::from_secs(2));
        if let Some(license) = licensing.complete_activation(&request)? {
            break license;
        }
    };
    println!("Activated for {}", license.issued_to.email);
}
```

The API is blocking; call it from a worker thread or `spawn_blocking` in async code.

- [Guide](https://github.com/Moonbase-sh/moonbase-rs/blob/main/docs/core-sdk.md)
- [Device identity](https://github.com/Moonbase-sh/moonbase-rs/blob/main/docs/device-identity.md)
- [Security](https://github.com/Moonbase-sh/moonbase-rs/blob/main/docs/security.md)
- For Tauri apps: [`tauri-plugin-moonbase`](https://github.com/Moonbase-sh/moonbase-rs/tree/main/crates/tauri-plugin-moonbase)

## Examples

```sh
cargo run -p moonbase --example activation   # activate from a terminal against the demo store
cargo run -p moonbase --example device_id    # this machine's device id, as JSON
```

License: MIT
