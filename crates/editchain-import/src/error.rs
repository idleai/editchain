use std::path::PathBuf;

/// Errors that can occur during import.
#[derive(Debug)]
pub enum ImportError {
    /// IO error reading a source file.
    Io(std::io::Error),
    /// JSON parse error on a line.
    Json(serde_json::Error),
    /// A configured input or execution resource bound was exceeded.
    ResourceLimit {
        /// Source being processed.
        path: PathBuf,
        /// Resource whose bound was exceeded.
        resource: &'static str,
        /// Configured maximum.
        limit: u64,
    },
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
    /// The Codex helper process could not be spawned.
    HelperSpawn {
        /// Helper program path.
        program: String,
        /// Underlying spawn error.
        source: std::io::Error,
    },
    /// The Codex helper process exited unsuccessfully (or was killed).
    HelperFailed {
        /// Rollout file being processed.
        path: PathBuf,
        /// Helper program path.
        program: String,
        /// Exit code (`None` when terminated by a signal).
        exit_code: Option<i32>,
        /// Captured stderr from the helper.
        stderr: String,
    },
    /// The helper emitted a projection record that violates the editchain-v1
    /// schema, or the projection stream is structurally inconsistent with the
    /// raw rollout file.
    ProjectionProtocol {
        /// Rollout file being processed.
        path: PathBuf,
        /// Human-readable description of the violation.
        detail: String,
    },
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Json(e) => write!(f, "JSON error: {e}"),
            Self::ResourceLimit {
                path,
                resource,
                limit,
            } => {
                write!(f, "{} exceeded {resource} limit {limit}", path.display())
            }
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
            Self::HelperSpawn { program, source } => {
                write!(f, "codex helper {program:?} could not be spawned: {source}")
            }
            Self::HelperFailed {
                path,
                program,
                exit_code,
                stderr,
            } => {
                write!(
                    f,
                    "codex helper {program:?} failed for {} (exit {exit_code:?}): {stderr}",
                    path.display()
                )
            }
            Self::ProjectionProtocol { path, detail } => {
                write!(
                    f,
                    "editchain-v1 projection error for {}: {detail}",
                    path.display()
                )
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

impl From<crate::ids::IdError> for ImportError {
    fn from(e: crate::ids::IdError) -> Self {
        Self::OpSink(e.to_string())
    }
}
