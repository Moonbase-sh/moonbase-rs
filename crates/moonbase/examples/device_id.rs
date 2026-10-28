//! Print this machine's Moonbase device id and how it was derived, as JSON.
//!
//! The diagnostic to run when a customer reports "this license is not for this
//! device": the output is safe to paste into a support ticket, because it names
//! the identity parameters that contributed but never their values.
//!
//! It is also how CI proves cross-SDK parity: the fingerprint-parity workflow runs
//! it next to `@moonbase.sh/licensing` on the same runner and requires the two
//! device ids to match. The output has the same shape as moonbase-cpp's
//! `moonbase_device_id`.
//!
//! Exits 0 even when the machine has no usable identity: that is a legitimate
//! answer, reported as an `error` field.

use moonbase::fingerprint_spec::DeviceIdSource;
use moonbase::{DeviceIdResolver, Error, MoonbaseDeviceIdResolver};
use serde_json::json;

fn main() {
    let resolver = MoonbaseDeviceIdResolver::new();
    let mut out = json!({ "deviceName": resolver.device_name() });

    match resolver.describe_device() {
        Ok(Some(described)) => {
            out["deviceId"] = json!(described.device_id);
            out["version"] = json!(described.version);
            out["platform"] = json!(described.platform);
            out["source"] = json!(match described.source {
                DeviceIdSource::DeviceName => "deviceName",
                DeviceIdSource::Scoped => "scoped",
                DeviceIdSource::Identity => "identity",
            });
            out["paramNames"] = json!(described.param_names);
        }
        Ok(None) => unreachable!("the default resolver always describes itself"),
        Err(Error::InsufficientDeviceIdentity { platform, reason }) => {
            out["error"] = json!("DeviceIdentityUnavailable");
            out["message"] = json!(format!("Could not identify this device (platform: {platform}): {reason}"));
            out["platform"] = json!(platform);
        }
        Err(error) => {
            out["error"] = json!(format!("{:?}", error.error_type()));
            out["message"] = json!(error.to_string());
        }
    }

    println!("{}", serde_json::to_string_pretty(&out).expect("JSON values always serialize"));
}
