//! # Moonbase licensing for Rust
//!
//! License activation for desktop apps and audio plugins, against
//! [Moonbase](https://moonbase.sh): browser and offline activation, local RS256
//! validation with an offline grace period, revocation, add-ons, licenses that
//! cover only some versions, pluggable storage and HTTP, and the cross-SDK device
//! fingerprint, so a license activated by any Moonbase SDK validates here too.
//!
//! The Rust port of [moonbase-cpp](https://github.com/Moonbase-sh/moonbase-cpp).
//!
//! ```no_run
//! use std::{thread, time::Duration};
//! use moonbase::{ActivationMethod, FileLicenseStore, Licensing, LicensingOptions};
//!
//! # fn main() -> moonbase::Result<()> {
//! let options = LicensingOptions::new(
//!     "https://your-tenant.moonbase.sh",
//!     "your-product",
//!     "-----BEGIN PUBLIC KEY-----\n...\n-----END PUBLIC KEY-----",
//! );
//! let licensing = Licensing::builder(options)
//!     .store(FileLicenseStore::new("license.mb"))
//!     .build()?;
//!
//! // On startup: validate what is stored, re-checking online when it is due.
//! if let Some(license) = licensing.load_and_validate()? {
//!     println!("Licensed to {}", license.issued_to.email);
//!     return Ok(());
//! }
//!
//! // Otherwise activate through the browser; the license is stored once it lands.
//! let request = licensing.request_activation(ActivationMethod::Online)?;
//! println!("Open {}", request.browser_url);
//! let license = loop {
//!     thread::sleep(Duration::from_secs(2));
//!     if let Some(license) = licensing.complete_activation(&request)? {
//!         break license;
//!     }
//! };
//! println!("Activated for {}", license.issued_to.email);
//! # Ok(())
//! # }
//! ```
//!
//! ## What the SDK guarantees, and what it leaves to you
//!
//! It answers one question soundly: is this license valid, for this product, on
//! this machine, right now? Tokens are RS256 JWTs verified against the public key
//! you embed with the algorithm pinned, bound to your product by audience, and to
//! this machine by a device id recomputed from hardware on every validation. It
//! ships no anti-debugging, obfuscation or tamper detection: hardening only works
//! when it is specific to your binary. See `docs/security.md`.

#![warn(missing_debug_implementations)]

mod client;
mod crypto;
mod device_id;
mod error;
mod file_lock;
pub mod fingerprint_spec;
mod http;
mod inventory;
mod licensing;
mod platform;
mod release_access;
mod resolver;
mod store;
mod sub_product;
mod time;
mod types;
mod url;
mod validator;
pub mod version;

pub use crate::client::{LicenseClient, VERSION};
pub use crate::device_id::{DeviceIdDescription, DeviceIdResolver, MigratingDeviceIdResolver, StaticDeviceIdResolver};
pub use crate::error::{ApiError, Error, ErrorType, Result, StorageError};
#[cfg(feature = "ureq-transport")]
pub use crate::http::UreqTransport;
pub use crate::http::{HttpRequest, HttpResponse, HttpTransport, Method};
pub use crate::inventory::{DownloadTarget, InventoryClient, ReleaseInfo, can_download};
pub use crate::licensing::{Licensing, LicensingBuilder};
pub use crate::release_access::{ReleaseAccess, ReleaseNotCovered};
pub use crate::resolver::{
    DeviceIdFallback, DeviceIdentity, DeviceIdentityReader, MoonbaseDeviceIdResolver, MoonbaseDeviceIdResolverOptions,
};
pub use crate::store::{FileLicenseStore, LicenseStore, MemoryLicenseStore, StoreLockGuard};
pub use crate::sub_product::{
    SubProduct, SubProductCatalog, SubProductCatalogEntry, SubProductCatalogProduct, SubProductPricing,
    SubProductRecurrence, SubProductSeats, SubProductTrialOffer, SubProductTrialStatus,
};
pub use crate::time::serde_iso8601;
pub use crate::types::{ActivationMethod, ActivationRequest, License, LicensingOptions, Platform, Product, User};
pub use crate::validator::LicenseValidator;

/// Supplying `ANDROID_ID` on Android, where native code cannot read it alone.
#[cfg(target_os = "android")]
pub use crate::platform::android;
