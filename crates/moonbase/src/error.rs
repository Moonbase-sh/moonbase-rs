use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::release_access::ReleaseNotCovered;

/// The SDK's result type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What kind of failure an [`Error`] is, as a plain value that can be persisted,
/// logged or compared.
///
/// The variants match the error types every other Moonbase SDK reports. New
/// variants are appended, never inserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ErrorType {
    ApiError,
    LicenseInvalid,
    LicenseExpired,
    StorageError,
    ConfigurationError,
    OperationNotSupported,
    /// The license is valid but bound to a different device, or to an older
    /// fingerprint version.
    LicenseDeviceMismatch,
    /// No stable hardware identifier could be read, so no device id exists.
    DeviceIdentityUnavailable,
    /// The server will never complete a browser activation request: it expired,
    /// was cancelled, or was refused.
    ActivationRequestEnded,
    /// The store the product is sold through has closed its Moonbase account.
    StoreClosed,
    /// The customer has no license of an add-on with a seat free for this device.
    NoEligibleLicense,
    /// The license is valid but doesn't cover this version of the app.
    ReleaseNotCovered,
}

/// A request that got no definitive answer.
///
/// The API was unreachable ([`status_code`](Self::status_code) is 0), rate limited
/// (429), failing (5xx), or something other than the API answered, such as a
/// captive portal. Worth retrying later, which is why online validation falls back
/// on the offline grace period when it gets one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub(crate) status_code: u16,
    pub(crate) message: String,
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) retry_after: Option<Duration>,
}

impl ApiError {
    /// An error with no HTTP status: the server could not be reached at all.
    ///
    /// Custom [`HttpTransport`](crate::HttpTransport)s return this when no
    /// response arrives (DNS, a refused connection, TLS, a timeout).
    pub fn unreachable(message: impl Into<String>) -> Self {
        Self::new(0, message)
    }

    pub fn new(status_code: u16, message: impl Into<String>) -> Self {
        Self { status_code, message: message.into(), title: String::new(), detail: String::new(), retry_after: None }
    }

    /// The HTTP status, or 0 when no response arrived.
    pub fn status_code(&self) -> u16 {
        self.status_code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// The problem's `title`, when the server sent ProblemDetails.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The problem's `detail`, when the server sent one.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// How long the server asked to be left alone, from a `Retry-After` header
    /// given in seconds. Moonbase sends one with its 429 rate-limit response.
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// Reading or writing the stored license failed, or its lock: a
/// [`LicenseStore`](crate::LicenseStore)'s own trouble, which says nothing about the
/// license. The I/O failure behind it, when there was one, is its
/// [`source`](std::error::Error::source), and [`io_error`](Self::io_error) hands it
/// over for a look at its kind.
#[derive(Debug, Clone)]
pub struct StorageError {
    message: String,
    source: Option<Arc<std::io::Error>>,
}

impl StorageError {
    /// A failure with no I/O error behind it, as a custom store reports one.
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), source: None }
    }

    /// An I/O failure, kept as the source. `message` says what was being done.
    pub fn io(message: impl Into<String>, source: std::io::Error) -> Self {
        Self { message: message.into(), source: Some(Arc::new(source)) }
    }

    /// What was being done, without the I/O error's own words.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The I/O failure behind it, when there was one.
    pub fn io_error(&self) -> Option<&std::io::Error> {
        self.source.as_deref()
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|error| error as &(dyn std::error::Error + 'static))
    }
}

/// Everything the SDK can fail with.
///
/// For calls to the API, the variant says whether trying again can help:
///
/// | Variant | Meaning | What to do |
/// | --- | --- | --- |
/// | [`Api`](Self::Api) | No definitive answer | Retry later |
/// | [`LicenseInvalid`](Self::LicenseInvalid) | Refused for good: bad signature or product, revoked | Lock, offer to activate again |
/// | [`DeviceMismatch`](Self::DeviceMismatch) | Genuine, but bound to another device | Lock, offer to activate again |
/// | [`ReleaseNotCovered`](Self::ReleaseNotCovered) | Valid, but not for this version of the app | Lock, keep the license, offer the upgrade |
/// | [`LicenseExpired`](Self::LicenseExpired) | A trial ended, or a subscription lapsed | Lock, offer to buy |
/// | [`StoreClosed`](Self::StoreClosed) | The merchant closed their account; no verdict on the license | Treat it like being offline |
/// | [`ActivationRequestEnded`](Self::ActivationRequestEnded) | A browser activation can never complete | Start a new request |
/// | [`NoEligibleLicense`](Self::NoEligibleLicense) | No seat of an add-on to give; no verdict on the license | Offer to buy the add-on, or free a seat |
/// | [`Configuration`](Self::Configuration) | Bad options, or an empty device id | Fix the setup |
///
/// [`is_license_rejection`](Self::is_license_rejection) groups the definitive
/// verdicts. Those are the only failures the offline grace period never covers.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("{0}")]
    Configuration(String),

    #[error(transparent)]
    Api(#[from] ApiError),

    #[error("{0}")]
    LicenseInvalid(String),

    /// The token verified, but its `sig` claim is not this device's id and no
    /// historical resolver recognised it.
    ///
    /// A [`is_license_invalid`](Self::is_license_invalid) error, deliberately: the
    /// offline grace period keys off that, and a mismatch that escaped it would let
    /// a license copied from another machine keep running for the whole window.
    #[error("{0}")]
    DeviceMismatch(String),

    /// The license is valid, but doesn't cover the version of the app that runs
    /// ([`LicensingOptions::application_version`](crate::LicensingOptions::application_version)):
    /// it covers only the releases published while its updates lasted, or versions
    /// up to some major. Moonbase refuses to validate it for that version, and the
    /// validator refuses it on the device the same way unless
    /// [`enforce_release_access`](crate::LicensingOptions::enforce_release_access)
    /// is off. Downloading a release the customer's licenses don't cover is refused
    /// with it too.
    ///
    /// A [`is_license_invalid`](Self::is_license_invalid) error, as
    /// [`DeviceMismatch`](Self::DeviceMismatch) is, so code that refuses to run on
    /// an invalid license refuses here too, and the offline grace period can't keep
    /// a version running that the license doesn't cover. But don't delete the
    /// stored license for it: match it first, keep the license, and offer the
    /// upgrade.
    #[error("{0}")]
    ReleaseNotCovered(Box<ReleaseNotCovered>),

    #[error("{0}")]
    LicenseExpired(String),

    /// The merchant has closed their Moonbase account (410 `StoreClosed`). Not a
    /// license verdict: it says nothing against the license, so treat it like being
    /// offline.
    #[error("{0}")]
    StoreClosed(String),

    /// The server will never complete this browser activation. Stop polling and
    /// start over with [`Licensing::request_activation`](crate::Licensing::request_activation).
    #[error("{0}")]
    ActivationRequestEnded(String),

    /// The device fingerprint had nothing machine-specific to hash.
    ///
    /// Either no parameter could be read, or only model-level ones could (vendor,
    /// product and board names, shared by every unit of a product line). Hashing
    /// either would hand a whole class of machines the same device id. Opt into the
    /// weaker host-name id with
    /// [`DeviceIdFallback::DeviceName`](crate::DeviceIdFallback::DeviceName).
    #[error("Could not identify this device (platform: {platform}): {reason}")]
    InsufficientDeviceIdentity { platform: String, reason: String },

    /// Two fingerprint parameters shared a name, which the material grammar cannot
    /// express. Unreachable from the built-in readers, so it always means a
    /// caller-supplied parameter list is wrong.
    #[error("Duplicate fingerprint parameter name: {0}")]
    DuplicateFingerprintParameter(String),

    #[error(transparent)]
    Storage(#[from] StorageError),

    #[error("{0}")]
    OperationNotSupported(String),

    /// Taking a seat of an add-on failed because the customer has none to give:
    /// they don't own it, or every seat of it is taken on other devices. Offer to
    /// buy it, or to free a seat in their account. Not a verdict on the license,
    /// which is as good as it was.
    #[error("{0}")]
    NoEligibleLicense(String),
}

impl Error {
    pub(crate) fn insufficient_identity(platform: impl Into<String>) -> Self {
        Self::InsufficientDeviceIdentity {
            platform: platform.into(),
            reason: "no identity parameter could be read".into(),
        }
    }

    pub fn error_type(&self) -> ErrorType {
        match self {
            Self::Configuration(_) | Self::DuplicateFingerprintParameter(_) => ErrorType::ConfigurationError,
            Self::Api(_) => ErrorType::ApiError,
            Self::LicenseInvalid(_) => ErrorType::LicenseInvalid,
            Self::DeviceMismatch(_) => ErrorType::LicenseDeviceMismatch,
            Self::ReleaseNotCovered(_) => ErrorType::ReleaseNotCovered,
            Self::LicenseExpired(_) => ErrorType::LicenseExpired,
            Self::StoreClosed(_) => ErrorType::StoreClosed,
            Self::ActivationRequestEnded(_) => ErrorType::ActivationRequestEnded,
            Self::InsufficientDeviceIdentity { .. } => ErrorType::DeviceIdentityUnavailable,
            Self::Storage(_) => ErrorType::StorageError,
            Self::OperationNotSupported(_) => ErrorType::OperationNotSupported,
            Self::NoEligibleLicense(_) => ErrorType::NoEligibleLicense,
        }
    }

    /// The license itself was refused: invalid, bound to another device, or not
    /// for this version of the app.
    pub fn is_license_invalid(&self) -> bool {
        matches!(self, Self::LicenseInvalid(_) | Self::DeviceMismatch(_) | Self::ReleaseNotCovered(_))
    }

    /// A definitive verdict against the license: invalid, bound elsewhere, not for
    /// this version, or expired. The offline grace period never covers these.
    pub fn is_license_rejection(&self) -> bool {
        self.is_license_invalid() || matches!(self, Self::LicenseExpired(_))
    }

    /// What a refusal for a version the license doesn't cover carries, when this
    /// is one.
    pub fn as_release_not_covered(&self) -> Option<&ReleaseNotCovered> {
        match self {
            Self::ReleaseNotCovered(refusal) => Some(refusal),
            _ => None,
        }
    }

    /// The API error, when this is one.
    pub fn as_api_error(&self) -> Option<&ApiError> {
        match self {
            Self::Api(error) => Some(error),
            _ => None,
        }
    }
}
