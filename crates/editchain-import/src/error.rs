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
        /// Path to the source file.
        path: PathBuf,
        /// Expected file size from the cursor.
        expected_size: u64,
        /// Actual file size on disk.
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
        /// The conflicting UUID string.
        uuid: String,
        /// Hash of the existing content.
        existing_hash: [u8; 32],
        /// Hash of the incoming content.
        incoming_hash: [u8; 32],
    },
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Json(e) => write!(f, "JSON error: {e}"),
            Self::SourceGenerationChanged {
                path,
                expected_size,
                actual_size,
            } => {
                write!(
                    f,
                    "source generation changed for {}: expected {} bytes, got {}",
                    path.display(),
                    expected_size,
                    actual_size
                )
            }
            Self::CursorStore(msg) => write!(f, "cursor store: {msg}"),
            Self::OpSink(msg) => write!(f, "op sink: {msg}"),
            Self::BlobSink(msg) => write!(f, "blob sink: {msg}"),
            Self::UuidCollision { uuid, .. } => {
                write!(f, "UUID collision for {uuid}: different content")
            }
        }
    }
}

impl std::error::Error for ImportError {}

impl From<std::io::Error> for ImportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for ImportError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
