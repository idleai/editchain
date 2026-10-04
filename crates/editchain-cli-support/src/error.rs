//! Stable process outcomes shared by all commands.

use std::{fmt, io};

/// A command outcome with a stable process exit code.
pub type Result<T> = std::result::Result<T, Failure>;

/// A diagnostic and its process exit code.
#[derive(Debug)]
pub struct Failure {
    /// Process exit status.
    pub code: u8,
    /// Human-readable diagnostic.
    pub message: String,
}

impl Failure {
    /// Construct a failure with an explicit exit status.
    pub fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Report invalid user input with exit status 2.
    pub fn input(message: impl Into<String>) -> Self {
        Self::new(2, message)
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        let code = [
            (io::ErrorKind::InvalidInput, 2),
            (io::ErrorKind::NotFound, 3),
            (io::ErrorKind::InvalidData, 4),
            (io::ErrorKind::UnexpectedEof, 4),
            (io::ErrorKind::WouldBlock, 5),
            (io::ErrorKind::Interrupted, 130),
        ]
        .into_iter()
        .find_map(|(kind, code)| (error.kind() == kind).then_some(code))
        .unwrap_or(1);
        Self::new(code, error.to_string())
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        if let Some(kind) = error.io_error_kind() {
            io::Error::new(kind, error).into()
        } else {
            Self::input(error.to_string())
        }
    }
}

impl From<Box<dyn std::error::Error>> for Failure {
    fn from(error: Box<dyn std::error::Error>) -> Self {
        let error = match error.downcast::<io::Error>() {
            Ok(error) => return (*error).into(),
            Err(error) => error,
        };
        Self::new(1, error.to_string())
    }
}
