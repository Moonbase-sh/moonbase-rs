//! The default device id resolver: the Moonbase device fingerprint spec, v2.
//!
//! Builds the `moonbase:fingerprint:v2` material from native hardware identifiers
//! (`IOPlatformUUID` via IOKit on macOS, `machine-id` plus world-readable DMI on
//! Linux, SMBIOS on Windows) and stamps its SHA-256 as `mbd2_<hex>`. No subprocess
//! is spawned and no root-only file is read, so the id is the same elevated or
//! not, and the resolver works inside an App Sandbox.

use std::sync::{Arc, Mutex, PoisonError};

use crate::device_id::{DeviceIdDescription, DeviceIdResolver};
use crate::error::{Error, Result};
use crate::fingerprint_spec::{self, DeviceIdSource, Parameter};
use crate::platform;

/// What a single read of the machine produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub params: Vec<Parameter>,
    pub device_name: String,
}

/// Overrides how the resolver reads the machine. Primarily for testing.
pub type DeviceIdentityReader = Arc<dyn Fn() -> DeviceIdentity + Send + Sync>;

/// What to do when no hardware identity is readable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DeviceIdFallback {
    /// Fail with [`Error::InsufficientDeviceIdentity`]. The default.
    #[default]
    None,
    /// Hash the host name instead, producing a deliberately weaker id stamped
    /// `mbd2n_`. Opt-in, because a host name is user-renameable, duplicated across
    /// imaged machines, and regenerated on every container start.
    ///
    /// **Ignored on iOS and Android**, where the host name is identical on every
    /// device: the fallback would give a whole install base one id.
    DeviceName,
}

#[derive(Clone, Default)]
pub struct MoonbaseDeviceIdResolverOptions {
    pub fallback: DeviceIdFallback,
    /// Overrides the identity source. Primarily for testing.
    pub reader: Option<DeviceIdentityReader>,
    /// Overrides the detected platform tag. Primarily for testing.
    pub platform: Option<String>,
}

impl std::fmt::Debug for MoonbaseDeviceIdResolverOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoonbaseDeviceIdResolverOptions")
            .field("fallback", &self.fallback)
            .field("reader", &self.reader.as_ref().map(|_| "custom"))
            .field("platform", &self.platform)
            .finish()
    }
}

#[derive(Default)]
struct Memo {
    identity: Option<DeviceIdentity>,
    description: Option<DeviceIdDescription>,
}

/// The cross-SDK device id: what every Moonbase SDK computes on this machine.
///
/// The machine is read at most once per successful id, and the result kept for the
/// life of the resolver, so the name and the id sent with an activation always
/// come from the same read. A read that yields no identity is not kept: the next
/// call reads again, because absence can be transient (an iOS device that has not
/// been unlocked since boot, a sandbox briefly refusing a firmware read).
pub struct MoonbaseDeviceIdResolver {
    options: MoonbaseDeviceIdResolverOptions,
    platform: String,
    memo: Mutex<Memo>,
}

impl Default for MoonbaseDeviceIdResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl MoonbaseDeviceIdResolver {
    pub fn new() -> Self {
        Self::with_options(MoonbaseDeviceIdResolverOptions::default())
    }

    pub fn with_options(options: MoonbaseDeviceIdResolverOptions) -> Self {
        let platform = options.platform.clone().unwrap_or_else(|| fingerprint_spec::platform_tag().to_owned());
        Self { options, platform, memo: Mutex::new(Memo::default()) }
    }

    /// Read what this machine offers, bypassing the resolver's memoization.
    ///
    /// The returned parameters are raw values, hardware serials among them. Do not
    /// log or transmit them.
    pub fn read_host_identity() -> DeviceIdentity {
        DeviceIdentity { params: platform::read_identity_params(), device_name: Self::read_host_name() }
    }

    /// The host name, with a trailing `.local` removed on macOS.
    pub fn read_host_name() -> String {
        platform::read_host_name()
    }

    fn read(&self) -> DeviceIdentity {
        match &self.options.reader {
            Some(reader) => reader(),
            None => Self::read_host_identity(),
        }
    }

    fn describe(&self, params: &[Parameter], source: DeviceIdSource) -> Result<DeviceIdDescription> {
        let material = fingerprint_spec::build_fingerprint_material(&self.platform, params)?;
        Ok(DeviceIdDescription {
            device_id: fingerprint_spec::fingerprint_device_id(&material, source),
            version: fingerprint_spec::VERSION,
            platform: self.platform.clone(),
            source,
            param_names: fingerprint_spec::canonicalize_params(params)?.into_iter().map(|(name, _)| name).collect(),
        })
    }

    fn description(&self) -> Result<DeviceIdDescription> {
        let mut memo = self.memo.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(description) = &memo.description {
            return Ok(description.clone());
        }

        let identity = match memo.identity.take() {
            Some(identity) => identity,
            None => self.read(),
        };
        // iOS and Android offer only app-scoped identifiers, and the stamp has to
        // say so, or the server would compare them with hardware ids.
        let source = match self.platform.as_str() {
            "ios" | "android" => DeviceIdSource::Scoped,
            _ => DeviceIdSource::Identity,
        };
        let described = match self.describe(&identity.params, source) {
            Err(Error::InsufficientDeviceIdentity { .. }) if self.options.fallback == DeviceIdFallback::DeviceName => {
                let fallback = [("deviceName".to_owned(), identity.device_name.clone())];
                self.describe(&fallback, DeviceIdSource::DeviceName)
            }
            other => other,
        };

        match described {
            Ok(description) => {
                memo.identity = Some(identity);
                memo.description = Some(description.clone());
                Ok(description)
            }
            // Keep the name for device_name(), but read the machine afresh next
            // time an id is asked for.
            Err(error) => {
                memo.identity = None;
                Err(error)
            }
        }
    }
}

impl DeviceIdResolver for MoonbaseDeviceIdResolver {
    /// Never fails: a machine with no readable identity still has to be able to
    /// label itself.
    fn device_name(&self) -> String {
        let mut memo = self.memo.lock().unwrap_or_else(PoisonError::into_inner);
        if memo.identity.is_none() {
            memo.identity = Some(self.read());
        }
        memo.identity.as_ref().map(|identity| identity.device_name.clone()).unwrap_or_default()
    }

    fn device_id(&self) -> Result<String> {
        self.description().map(|description| description.device_id)
    }

    fn describe_device(&self) -> Result<Option<DeviceIdDescription>> {
        self.description().map(Some)
    }
}

impl std::fmt::Debug for MoonbaseDeviceIdResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoonbaseDeviceIdResolver")
            .field("platform", &self.platform)
            .field("fallback", &self.options.fallback)
            .finish_non_exhaustive()
    }
}
