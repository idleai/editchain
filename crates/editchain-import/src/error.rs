use std::path::PathBuf;

/// Errors that can occur during import.
#[derive(Debug)]
pub enum ImportError {
    /// IO error reading a source file.
    Io(std::io::Error),
    /// JSON parse error on a line.
    Json(serde_json::Error),
    /// A source file was truncated or rewritten (new generation detected).
    SourceGenerationChanged {
        path: PathBuf,
        expected_size: u64,
        actual_size: u64,
    },
    /// A cursor store operation failed.
    CursorStore(String),
    /// An op sink operation failed.
    OpSink(String),
    /// A blob sink operation failed.
    BlobSink(String),
    /// UUID collision: same external UUID with different content.
    UuidCollision {
        uuid: String,
        existing_hash: [u8; 32],
        incoming_hash: [u8; 32],
    },
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Io(e) => write!(f, "IO error: {}", e),
            ImportError::Json(e) => write!(f, "JSON error: {}", e),
            ImportError::SourceGenerationChanged { path, expected_size, actual_size } => {
                write!(f, "source generation changed for {}: expected {} bytes, got {}", path.display(), expected_size, actual_size)
            }
            ImportError::CursorStore(msg) => write!(f, "cursor store: {}", msg),
            ImportError::OpSink(msg) => write!(f, "op sink: {}", msg),
            ImportError::BlobSink(msg) => write!(f, "blob sink: {}", msg),
            ImportError::UuidCollision { uuid, .. } => {
                write!(f, "UUID collision for {}: different content", uuid)
            }
        }
    }
}

impl std::error::Error for ImportError {}

impl From<std::io::Error> for ImportError {
    fn from(e: std::io::Error) -> Self {
        ImportError::Io(e)
    }
}

impl From<serde_json::Error> for ImportError {
    fn from(e: serde_json::Error) -> Self {
        ImportError::Json(e)
    }
}