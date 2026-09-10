//! Stable error codes shared by the service and its clients.

use serde::{Deserialize, Deserializer, Serialize};

/// Machine-readable reason that a request could not be completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Invalid identity, coordinate, or request limit.
    InvalidInput,
    /// No workspace has been opened.
    NoWorkspace,
    /// The request belongs to an obsolete source or view snapshot.
    StaleSnapshot,
    /// An exact requested object is unavailable.
    UnavailableObject,
    /// The client and service protocol versions are incompatible.
    UnsupportedProtocol,
    /// A transport, storage, or unexpected internal failure.
    #[serde(other)]
    Internal,
}

/// An actionable service error carried inside the existing `Error` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceError {
    /// Stable category for client recovery decisions.
    pub code: ErrorCode,
    /// Human-readable explanation.
    pub message: String,
}

impl ServiceError {
    /// Construct a typed failure.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Preserve typed failures at a transport boundary; classify other errors.
    #[must_use]
    pub fn from_error(error: &(dyn std::error::Error + 'static)) -> Self {
        error
            .downcast_ref::<Self>()
            .cloned()
            .unwrap_or_else(|| Self::new(ErrorCode::Internal, error.to_string()))
    }
}

impl<'de> Deserialize<'de> for ServiceError {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireError {
            Structured { code: ErrorCode, message: String },
            Legacy(String),
        }
        Ok(match WireError::deserialize(deserializer)? {
            WireError::Structured { code, message } => Self { code, message },
            WireError::Legacy(message) => Self::new(ErrorCode::Internal, message),
        })
    }
}

impl core::fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ServiceError {}
