//! Supplying `ANDROID_ID` to the device id resolver.
//!
//! Everything else in this SDK reads the machine on its own. Android is the one
//! exception: `Settings.Secure.getString` needs a `ContentResolver`, which needs
//! the application `Context`, and a native library has no supported way to get
//! one by itself. So the host reads it, typically from a Kotlin plugin or over JNI,
//! and hands the SDK a source once during startup:
//!
//! ```ignore
//! moonbase::android::set_android_id_source(|| read_android_id_over_jni());
//! ```
//!
//! The value must come from `Settings.Secure.getString(resolver, ANDROID_ID)`.
//! The static field `Settings.Secure.ANDROID_ID` is the key name `"android_id"`,
//! identical on every device; the spec's `^[0-9a-f]{1,16}$` rule rejects it, so
//! that mistake surfaces as insufficient identity rather than as one device id for
//! the whole install base.
//!
//! Until a source is set, Android resolves to
//! [`Error::InsufficientDeviceIdentity`](crate::Error::InsufficientDeviceIdentity),
//! the honest answer for a device the SDK cannot identify.

use std::sync::{PoisonError, RwLock};

type Source = Box<dyn Fn() -> Option<String> + Send + Sync>;

static SOURCE: RwLock<Option<Source>> = RwLock::new(None);

/// Set where the resolver reads `ANDROID_ID` from. Returning `None` (Android can,
/// before the user is set up) is treated as absent, and the next call asks again.
pub fn set_android_id_source(source: impl Fn() -> Option<String> + Send + Sync + 'static) {
    *SOURCE.write().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(source));
}

/// The id, lowercased as the spec requires, or empty.
pub(crate) fn read_android_id() -> String {
    let source = SOURCE.read().unwrap_or_else(PoisonError::into_inner);
    source.as_ref().and_then(|source| source()).map(|id| id.to_ascii_lowercase()).unwrap_or_default()
}
