//! Stable process outcomes shared by all commands.

use std::{fmt, io};

pub(crate) type Result<T> = std::result::Result<T, Failure>;

#[derive(Debug)]
pub(crate) struct Failure {
    pub code: u8,
    pub message: String,
}

impl Failure {
    pub(crate) fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn input(message: impl Into<String>) -> Self {
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
