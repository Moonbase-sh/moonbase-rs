//! Activate through the browser from a terminal, and keep the license in
//! `license.mb` in the working directory.
//!
//! ```sh
//! MOONBASE_ENDPOINT=https://demo.moonbase.sh \
//! MOONBASE_PRODUCT_ID=demo-app \
//! MOONBASE_PUBLIC_KEY="$(cat public-key.pem)" \
//! cargo run -p moonbase --example activation
//! ```
//!
//! Without the variables it runs against Moonbase's public demo store.

use std::env;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use moonbase::{ActivationMethod, FileLicenseStore, Licensing, LicensingOptions};

const DEMO_PUBLIC_KEY: &str = "-----BEGIN RSA PUBLIC KEY-----
MIIBCgKCAQEAutOqeUiPMgYjAwQ53CyKhJSqojr2bejce0CshQi9Hd8mNZbkoROx
oS56eIzehFSlX4YwHnF47AR1+fPOe7Q33Cgzd6d9xqksiMH7sWK2mADIlB66vZdW
uk3Me0UMB22Biy1RQbSRMivu79MxCofsympoL/5CFjJLd1u37kxjuRWVLjJS84Rr
3L2W7R7Exnno/giC+L/Dv711mjgstmtlAQm5ZINvFvoLA1eFTDs6nlCs3dpJSiq3
fsBUMT9FtudzS5As54jeT/8MB66fJJ0A1LQ/v5CW8ACQYseFSIoOKErD3xU7QLIJ
ERUn++6CVMPvZo67jVbTY+GCXYfW4gGVZQIDAQAB
-----END RSA PUBLIC KEY-----";

fn env_or(name: &str, fallback: &str) -> String {
    env::var(name).ok().filter(|value| !value.is_empty()).unwrap_or_else(|| fallback.to_owned())
}

fn run() -> moonbase::Result<()> {
    let mut options = LicensingOptions::new(
        env_or("MOONBASE_ENDPOINT", "https://demo.moonbase.sh"),
        env_or("MOONBASE_PRODUCT_ID", "demo-app"),
        env_or("MOONBASE_PUBLIC_KEY", DEMO_PUBLIC_KEY),
    );
    if let Ok(version) = env::var("MOONBASE_APP_VERSION") {
        options.application_version = Some(version);
    }
    if let Ok(account) = env::var("MOONBASE_ACCOUNT_ID") {
        options.account_id = Some(account);
    }

    let licensing = Licensing::builder(options).store(FileLicenseStore::new("license.mb")).build()?;

    if let Some(license) = licensing.load_and_validate()? {
        println!(
            "Existing {} license is valid for {}",
            if license.trial { "trial" } else { "full" },
            license.issued_to.email
        );
        return Ok(());
    }

    let request = licensing.request_activation(ActivationMethod::Online)?;
    println!("Open this URL to activate:\n{}\n", request.browser_url);

    let license = loop {
        thread::sleep(Duration::from_secs(2));
        // Stores the license once it lands.
        match licensing.complete_activation(&request) {
            Ok(Some(license)) => break license,
            Ok(None) => println!("Waiting for activation..."),
            // Worth retrying: the API was unreachable, busy or rate limiting.
            Err(moonbase::Error::Api(error)) => println!("Still waiting ({error})"),
            Err(error) => return Err(error),
        }
    };

    println!("Stored license for {}", license.issued_to.email);
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
