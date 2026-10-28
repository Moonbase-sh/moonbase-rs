//! How this machine is identified to Moonbase.
//!
//! A license token carries a `sig` claim equal to the device id, recomputed and
//! compared on every local validation. The default
//! [`MoonbaseDeviceIdResolver`](crate::MoonbaseDeviceIdResolver) follows the
//! cross-SDK fingerprint spec, so a license activated by any conforming Moonbase
//! SDK validates here and vice versa. Custom resolvers are compared literally and
//! need not follow the spec.

use std::sync::{Arc, OnceLock};

use serde::Serialize;

use crate::error::Result;
use crate::fingerprint_spec::DeviceIdSource;

/// A device id plus the provenance needed to reason about it: safe to log, show in
/// an about box, or attach to a support ticket.
///
/// Parameter *names* only, deliberately. Their values are hardware serial numbers,
/// and an unsalted hash of one is no safer to publish than the value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceIdDescription {
    pub device_id: String,
    /// Fingerprint spec version that produced it.
    pub version: u32,
    pub platform: String,
    pub source: DeviceIdSource,
    /// Names of the identity parameters that went into the material, in order.
    pub param_names: Vec<String>,
}

/// Identifies this machine to Moonbase.
pub trait DeviceIdResolver: Send + Sync {
    /// A human-readable label sent alongside the device id at activation. Not part
    /// of the hashed material (except in the opt-in host-name fallback), so it can
    /// change freely without invalidating a license.
    fn device_name(&self) -> String;

    /// The device id this machine binds on activation.
    fn device_id(&self) -> Result<String>;

    /// Does this machine also answer to a device id it used to have?
    ///
    /// Separate from [`device_id`](Self::device_id), which stays single-valued:
    /// what a device binds on activation and what a validator accepts are
    /// different questions. The default is no history.
    fn accepts_device_id(&self, _device_id: &str) -> bool {
        false
    }

    /// Provenance for diagnostics, when the resolver can explain itself.
    fn describe_device(&self) -> Result<Option<DeviceIdDescription>> {
        Ok(None)
    }
}

/// A shared resolver is a resolver, so one can be handed to the SDK and kept too.
impl<T: DeviceIdResolver + ?Sized> DeviceIdResolver for std::sync::Arc<T> {
    fn device_name(&self) -> String {
        (**self).device_name()
    }

    fn device_id(&self) -> Result<String> {
        (**self).device_id()
    }

    fn accepts_device_id(&self, device_id: &str) -> bool {
        (**self).accepts_device_id(device_id)
    }

    fn describe_device(&self) -> Result<Option<DeviceIdDescription>> {
        (**self).describe_device()
    }
}

/// A fixed identity. Useful in tests, and for apps that source the device id from
/// somewhere else entirely.
#[derive(Debug, Clone)]
pub struct StaticDeviceIdResolver {
    name: String,
    id: String,
}

impl StaticDeviceIdResolver {
    pub fn new(name: impl Into<String>, id: impl Into<String>) -> Self {
        Self { name: name.into(), id: id.into() }
    }
}

impl DeviceIdResolver for StaticDeviceIdResolver {
    fn device_name(&self) -> String {
        self.name.clone()
    }

    fn device_id(&self) -> Result<String> {
        Ok(self.id.clone())
    }
}

/*
 * A note for anyone tempted to add a resolver that remembers a previously
 * computed device id on disk, to survive a transient read failure:
 *
 * It cannot be done safely at this layer. Any such cache is an unsigned file in
 * the application's own storage, so the attacker controls both its contents *and*
 * whether the fresh read is degraded. Replaying a recorded id when the read looks
 * weaker reduces to "write the id you want, then break one source": a scriptable
 * license bypass. The memoization in MoonbaseDeviceIdResolver is process-lifetime
 * only. It is not persistence.
 */

/// Binds the current fingerprint while still recognising ids this device was
/// bound to before: the migration path off an older algorithm without a flag day.
///
/// [`device_id`](DeviceIdResolver::device_id) always returns the *current*
/// resolver's id, so every new activation binds the current algorithm. The
/// historical resolvers are consulted only when a validator is deciding whether to
/// accept an already-issued license, lazily, once, on the first mismatch. A
/// historical resolver that fails is skipped: it just cannot vouch for the license.
///
/// ```
/// use std::sync::Arc;
/// use moonbase::{MigratingDeviceIdResolver, MoonbaseDeviceIdResolver, StaticDeviceIdResolver};
///
/// let resolver = MigratingDeviceIdResolver::new(
///     Arc::new(MoonbaseDeviceIdResolver::new()),
///     vec![Arc::new(StaticDeviceIdResolver::new("Studio Mac", "id-from-an-older-scheme"))],
/// );
/// ```
pub struct MigratingDeviceIdResolver {
    current: Arc<dyn DeviceIdResolver>,
    previous: Vec<Arc<dyn DeviceIdResolver>>,
    previous_ids: OnceLock<Vec<String>>,
}

impl MigratingDeviceIdResolver {
    pub fn new(current: Arc<dyn DeviceIdResolver>, previous: Vec<Arc<dyn DeviceIdResolver>>) -> Self {
        Self { current, previous, previous_ids: OnceLock::new() }
    }
}

impl DeviceIdResolver for MigratingDeviceIdResolver {
    fn device_name(&self) -> String {
        self.current.device_name()
    }

    fn device_id(&self) -> Result<String> {
        self.current.device_id()
    }

    fn accepts_device_id(&self, device_id: &str) -> bool {
        // An empty id is what a historical resolver that could not read anything
        // reduces to. It must never match, or a machine with no identity would
        // accept a license bound to another such machine.
        if device_id.is_empty() {
            return false;
        }
        self.previous_ids
            .get_or_init(|| {
                self.previous
                    .iter()
                    .filter_map(|resolver| resolver.device_id().ok())
                    .filter(|id| !id.is_empty())
                    .collect()
            })
            .iter()
            .any(|previous| previous == device_id)
    }

    fn describe_device(&self) -> Result<Option<DeviceIdDescription>> {
        self.current.describe_device()
    }
}

impl std::fmt::Debug for MigratingDeviceIdResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigratingDeviceIdResolver").field("previous", &self.previous.len()).finish_non_exhaustive()
    }
}
