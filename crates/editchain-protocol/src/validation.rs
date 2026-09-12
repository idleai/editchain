//! Resource and coordinate bounds enforced before executing a request.

use crate::{ErrorCode, RequestBody, ServiceError};

/// Maximum encoded request frame; checked before allocating its payload.
pub const MAX_REQUEST_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// Maximum expanded rows requested in one page.
pub const MAX_WINDOW_ROWS: u64 = 10_000;
/// Maximum ranked visible matches requested by a client.
pub const MAX_SEARCH_RESULTS: usize = 1_000;
/// Maximum UTF-8 query size in bytes.
pub const MAX_QUERY_BYTES: usize = 16_384;
/// Greatest integer that can cross JavaScript without precision loss.
const MAX_EXACT_COORDINATE: u64 = 9_007_199_254_740_991;

impl RequestBody {
    /// Validate resource limits before any allocation, projection, or search.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for unsupported limits or inexact coordinates.
    pub fn validate(&self) -> Result<(), ServiceError> {
        match self {
            Self::SyncLive(request) => {
                if request.epoch.is_empty() || request.after_revision > MAX_EXACT_COORDINATE {
                    return Err(invalid("invalid live epoch or revision"));
                }
                if request.codex.as_ref().is_some_and(|codex| {
                    codex.paths.len() > 32 || codex.paths.iter().any(|path| path.len() > 16384)
                }) {
                    return Err(invalid("live capture exceeds source batch bounds"));
                }
            }
            Self::LocateRows(request) => {
                if request.keys.len() > 2000 || request.keys.iter().any(|key| key.len() > 2048) {
                    return Err(invalid(
                        "anchor lookup exceeds 2000 keys or 2048 bytes per key",
                    ));
                }
            }
            Self::GetWindow(request) => {
                if request.limit == 0 || request.limit > MAX_WINDOW_ROWS {
                    return Err(invalid("window limit must be between 1 and 10000"));
                }
                if request
                    .offset
                    .checked_add(request.limit)
                    .is_none_or(|end| end > MAX_EXACT_COORDINATE)
                {
                    return Err(invalid("window coordinates exceed the exact integer range"));
                }
            }
            Self::FindInHistory(request) => {
                if request.top_k == 0 || request.top_k > MAX_SEARCH_RESULTS {
                    return Err(invalid("search limit must be between 1 and 1000"));
                }
                if request.query.len() > MAX_QUERY_BYTES {
                    return Err(invalid("search query exceeds 16384 bytes"));
                }
            }
            Self::Open(_)
            | Self::OpenLive(_)
            | Self::Refresh(_)
            | Self::GetNodeDetails(_)
            | Self::ResolveObject(_)
            | Self::GetFileDiff(_) => {}
        }
        Ok(())
    }
}

fn invalid(message: &str) -> ServiceError {
    ServiceError::new(ErrorCode::InvalidInput, message)
}
