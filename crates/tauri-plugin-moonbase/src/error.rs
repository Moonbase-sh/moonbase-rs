use serde::ser::SerializeStruct as _;
use serde::{Serialize, Serializer};

/// What a plugin command can fail with. Serialized for the frontend as
/// `{ kind, message }`, where `kind` is the SDK's error type (`"apiError"`,
/// `"licenseInvalid"`, ...) or one of the plugin's own (`"notLicensed"`,
/// `"busy"`, `"internal"`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Moonbase(#[from] moonbase::Error),

    /// The operation needs a license and none is loaded.
    #[error("No license is loaded")]
    NotLicensed,

    /// Another operation that changes the license is still running.
    #[error("{0}")]
    Busy(String),

    #[error(transparent)]
    Tauri(#[from] tauri::Error),

    #[error("{0}")]
    Internal(String),
}

impl Error {
    fn kind(&self) -> String {
        match self {
            Self::Moonbase(error) => serde_json::to_value(error.error_type())
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".into()),
            Self::NotLicensed => "notLicensed".into(),
            Self::Busy(_) => "busy".into(),
            Self::Tauri(_) | Self::Internal(_) => "internal".into(),
        }
    }
}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("Error", 2)?;
        out.serialize_field("kind", &self.kind())?;
        out.serialize_field("message", &self.to_string())?;
        out.end()
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
