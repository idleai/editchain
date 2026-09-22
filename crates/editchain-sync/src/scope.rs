//! Durable export consent, independent of receipt provenance and device approval.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read as _};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use editchain_store::durable::atomic_write;
use serde::{Deserialize, Serialize};

use crate::{invalid, RecordKey};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scope {
    pub version: u16,
    pub space: String,
    pub excluded: BTreeSet<RecordKey>,
    pub received: BTreeSet<RecordKey>,
    pub received_blobs: BTreeSet<[u8; 32]>,
    /// Exact records this device retained locally before a peer independently
    /// supplied the same bytes. Kept separate from `received` so that export
    /// permission and blob gating never imply peer authorship.
    ///
    /// Version-1 ledgers written before this field existed default to empty.
    /// Their received-but-formerly-excluded entries are ambiguous and are not
    /// retroactively attributed to this device.
    #[serde(default)]
    pub local: BTreeSet<RecordKey>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub cutoff: Option<Cutoff>,
}

impl Scope {
    pub(crate) fn read(root: &Path) -> io::Result<Self> {
        const LIMIT: u64 = 128 * 1024 * 1024;
        let file = fs::File::open(root.join("multiplayer/scope.json"))?;
        if file.metadata()?.len() > LIMIT {
            return Err(invalid("scope metadata exceeds limit"));
        }
        let mut bytes = Vec::new();
        let _read = file.take(LIMIT.saturating_add(1)).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).map_err(io::Error::other)? > LIMIT {
            return Err(invalid("scope metadata exceeds limit"));
        }
        let scope: Self =
            serde_json::from_slice(&bytes).map_err(|_error| invalid("invalid scope metadata"))?;
        if scope.version != 1 {
            return Err(invalid("unsupported scope version"));
        }
        crate::replica::validate_space(&scope.space)?;
        Ok(scope)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cutoff {
    pub first_segment: u32,
    pub selected_at_ms: u64,
}

/// Effective outgoing sharing policy. Incoming history follows the sender's policy.
#[derive(Debug, Serialize)]
pub struct SharingScope {
    /// Durable collaboration space.
    pub space: String,
    /// `all`, `from_now`, or a preserved `legacy_from_now` exclusion baseline.
    pub mode: &'static str,
    /// Time the current cutoff was selected; display only, never a record filter.
    pub cutoff_ms: Option<u64>,
    /// Consent generation; live workers from earlier generations must reconnect.
    pub revision: u64,
    /// False if a policy change was interrupted and must be explicitly retried.
    pub active: bool,
    /// Exact exclusions retained from older versions without a cutoff timestamp.
    pub legacy_excluded_records: usize,
}

impl Scope {
    pub(crate) fn summary(&self) -> SharingScope {
        SharingScope {
            space: self.space.clone(),
            mode: if self.cutoff.is_some() {
                "from_now"
            } else if self.excluded.is_empty() {
                "all"
            } else {
                "legacy_from_now"
            },
            cutoff_ms: self.cutoff.as_ref().map(|value| value.selected_at_ms),
            revision: self.revision,
            active: true,
            legacy_excluded_records: self.excluded.len(),
        }
    }

    pub(crate) fn excludes(&self, key: RecordKey, segment: u32) -> bool {
        self.excluded.contains(&key) || self.before_cutoff(segment)
    }

    pub(crate) fn before_cutoff(&self, segment: u32) -> bool {
        self.cutoff
            .as_ref()
            .is_some_and(|value| segment < value.first_segment)
    }

    /// Called under the writer lock. Publish the small fence first so a crash
    /// before the ledger write closes old transfers and prevents an unsafe reopen.
    /// Repeating the explicit selection repairs an interrupted policy write.
    pub(crate) fn select(&mut self, root: &Path, first_segment: Option<u32>) -> io::Result<()> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("scope revision exhausted"))?;
        self.cutoff = first_segment
            .map(|first_segment| {
                let selected_at_ms = u64::try_from(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(io::Error::other)?
                        .as_millis(),
                )
                .map_err(io::Error::other)?;
                Ok::<_, io::Error>(Cutoff {
                    first_segment,
                    selected_at_ms,
                })
            })
            .transpose()?;
        self.excluded.clear();
        atomic_write(
            &root.join("multiplayer/scope-revision"),
            &self.revision.to_le_bytes(),
        )
    }
}

#[derive(Debug)]
pub(crate) struct ScopeChanged;
impl std::fmt::Display for ScopeChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("sharing scope changed; reconnect")
    }
}
impl std::error::Error for ScopeChanged {}

pub(crate) fn changed() -> io::Error {
    io::Error::other(ScopeChanged)
}

pub(crate) fn ensure_revision(root: &Path, expected: u64) -> io::Result<()> {
    let mut file = match fs::File::open(root.join("multiplayer/scope-revision")) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound && expected == 0 => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(changed()),
        Err(error) => return Err(error),
    };
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes)?;
    if file.read(&mut [0])? != 0 {
        return Err(invalid("invalid scope revision"));
    }
    if u64::from_le_bytes(bytes) != expected {
        return Err(changed());
    }
    Ok(())
}
