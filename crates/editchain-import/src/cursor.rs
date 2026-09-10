use std::io::{Read, SeekFrom};
use std::path::{Component, Path};

use editchain_core::NodeId;

use crate::error::ImportError;
use crate::ids::{derive_keyed_source_stream, derive_source_stream, hash_raw};
use crate::sink::{CursorStore, CursorValue};

/// Version of the provider-relative cursor-key contract.
pub const SOURCE_CURSOR_KEY_VERSION: u32 = 1;

/// Cursor lookup and source-node identity resolved for one physical source.
#[derive(Debug, Clone)]
pub struct ResolvedSourceCursor {
    /// Provider-relative key used for all future cursor writes/lookups.
    pub canonical_key: String,
    /// Key that supplied the current cursor/generation. This is the canonical
    /// key except during one-time migration from an absolute-path cursor.
    pub state_key: String,
    /// Existing cursor, when this source has been imported before.
    pub cursor: Option<CursorValue>,
    /// Stable node that owns raw and derived operation IDs for this source.
    pub source_node: NodeId,
}

impl ResolvedSourceCursor {
    /// Whether this lookup is migrating an old absolute-path cursor.
    #[must_use]
    pub fn migrates_legacy_key(&self) -> bool {
        self.canonical_key != self.state_key
    }
}

/// Build a root-independent cursor key from an exact provider-relative path.
///
/// The relative path is hashed as bytes so separators in a host path never
/// become cursor-store syntax. Moving an unchanged sessions tree between an
/// archive and a live provider root therefore retains source identity, while a
/// move *inside* that tree remains a distinct physical source descriptor.
///
/// # Errors
///
/// Returns [`ImportError::CursorStore`] if `source` is not lexically beneath
/// `root` or if the relative path contains non-normal components.
pub fn canonical_source_key(
    provider: &str,
    root: &Path,
    source: &Path,
) -> Result<String, ImportError> {
    let relative = source.strip_prefix(root).map_err(|_strip_error| {
        ImportError::CursorStore(format!(
            "source {} is outside discovery root {}",
            source.display(),
            root.display()
        ))
    })?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ImportError::CursorStore(format!(
            "source {} has no normal provider-relative path beneath {}",
            source.display(),
            root.display()
        )));
    }
    let relative_hash = blake3::hash(relative.as_os_str().as_encoded_bytes());
    Ok(format!(
        "editchain:source-cursor:v{SOURCE_CURSOR_KEY_VERSION}:{provider}:{relative_hash}"
    ))
}

/// Resolve a canonical cursor while preserving IDs created by legacy imports.
///
/// Canonical provider-relative state wins when present. Otherwise the exact
/// historical absolute-path key is checked once. A migrated legacy cursor uses
/// its original path-derived node; a genuinely new source uses the portable
/// keyed source-stream derivation.
///
/// # Errors
///
/// Returns a cursor-store error from key derivation or lookup.
pub fn resolve_source_cursor(
    cursors: &dyn CursorStore,
    provider: &str,
    root: &Path,
    source: &Path,
    workspace_path: &str,
) -> Result<ResolvedSourceCursor, ImportError> {
    let canonical_key = canonical_source_key(provider, root, source)?;
    if let Some(cursor) = cursors.get_cursor(&canonical_key)? {
        let source_node = cursor
            .source_node
            .unwrap_or_else(|| derive_keyed_source_stream(&canonical_key, 0).node);
        return Ok(ResolvedSourceCursor {
            state_key: canonical_key.clone(),
            canonical_key,
            cursor: Some(cursor),
            source_node,
        });
    }

    let legacy_key = source.to_string_lossy().to_string();
    if let Some(cursor) = cursors.get_cursor(&legacy_key)? {
        let source_node = cursor
            .source_node
            .unwrap_or_else(|| derive_source_stream(workspace_path, &legacy_key, 0).node);
        return Ok(ResolvedSourceCursor {
            canonical_key,
            state_key: legacy_key,
            cursor: Some(cursor),
            source_node,
        });
    }

    let source_node = derive_keyed_source_stream(&canonical_key, 0).node;
    Ok(ResolvedSourceCursor {
        state_key: canonical_key.clone(),
        canonical_key,
        cursor: None,
        source_node,
    })
}

/// Check whether a source file has changed since the last cursor was written.
///
/// Returns `Ok(true)` when the accepted prefix is byte-identical and no new
/// complete line exists, `Ok(false)` when that exact prefix is followed by at
/// least one new complete line, and `Err` when accepted bytes differ or are
/// missing. File size alone never establishes continuity.
///
/// A successful check upgrades a legacy cursor hash to direct BLAKE3 over
/// exactly `0..byte_offset`. A legacy rolling hash that cannot prove the prefix
/// is conservatively treated as a new source generation.
///
/// # Errors
///
/// Returns `ImportError::Io` if the file cannot be read, or
/// `ImportError::SourceGenerationChanged` if its accepted prefix changed.
pub fn check_file_generation(path: &Path, cursor: &mut CursorValue) -> Result<bool, ImportError> {
    let (lines, _bytes, proposed) = crate::source_read::read_session_file(path, Some(cursor))?;
    // This compatibility API checks continuity without accepting new records.
    // Importers use SourceReadPlan directly, retaining the captured source.
    cursor.file_size = proposed.file_size;
    cursor.content_hash_version = 1;
    if cursor.byte_offset == 0 {
        cursor.content_hash = hash_raw(&[]);
    }
    Ok(lines.is_empty())
}

/// Read new bytes from a file starting at the given offset.
///
/// Returns the new bytes and their cumulative hash.
///
/// # Errors
///
/// Returns `ImportError::Io` if the file cannot be opened or read.
pub fn read_new_bytes(path: &Path, offset: u64) -> Result<(Vec<u8>, [u8; 32]), ImportError> {
    let mut file = std::fs::File::open(path).map_err(ImportError::Io)?;
    let _: u64 =
        std::io::Seek::seek(&mut file, SeekFrom::Start(offset)).map_err(ImportError::Io)?;

    let mut new_bytes = Vec::new();
    let _: usize = file.read_to_end(&mut new_bytes).map_err(ImportError::Io)?;

    let hash = hash_raw(&new_bytes);
    Ok((new_bytes, hash))
}

/// Split raw bytes into complete JSONL lines, deferring a partial final line.
///
/// Returns (`complete_lines`, `partial_line_bytes`).
#[must_use]
#[expect(
    clippy::indexing_slicing,
    reason = "slice indices are bounded by data.len() via iteration"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "i + 1 is bounded by data.len() since we iterate over data"
)]
pub fn split_lines(data: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut lines = Vec::new();
    let mut start = 0;

    for (i, &byte) in data.iter().enumerate() {
        if byte == b'\n' {
            let end = i + 1;
            lines.push(data[start..end].to_vec());
            start = end;
        }
    }

    let remainder = data[start..].to_vec();
    (lines, remainder)
}
