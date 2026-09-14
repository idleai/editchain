//! Version negotiation and opaque opened-view identities.

use serde::{Deserialize, Serialize};

use crate::{ErrorCode, RequestBody, ServiceError};

/// Protocol version requiring snapshot-bound requests and typed result decoding.
pub const PROTOCOL_VERSION: u32 = 2;

/// Opaque source/view identity. Clients compare and echo it without interpreting it.
///
/// The empty default only permits decoding old requests into an explicit protocol
/// error. It never authorizes access to a current snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotId(String);

impl SnapshotId {
    /// Wrap an identity produced by the snapshot owner.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exact wire representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether an old request omitted its snapshot identity.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Typed Open/Refresh handshake. Open request fields remain compatible with
/// older services so clients can check this version before sending new fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenResponse {
    /// Service protocol version; absent in legacy responses.
    #[serde(default)]
    pub protocol_version: u32,
    /// Opaque identity required on every following request.
    #[serde(default)]
    pub snapshot_id: SnapshotId,
    /// Additive capability for presentation identities and `LocateRows`.
    #[serde(default)]
    pub live_updates: bool,
    /// Retained live topology. Absent for the immutable historical Activity view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<crate::LiveBaseline>,
    /// Workspace path opened by the service.
    #[serde(default)]
    pub workspace: String,
    /// Chain path requested by the client.
    #[serde(default)]
    pub chain: String,
    /// Discovered repository count.
    #[serde(default)]
    pub repos: usize,
    /// Projected graph node count before presentation expansion.
    #[serde(default)]
    pub nodes: u64,
    /// Accepted operation count; descriptive, never used as snapshot identity.
    #[serde(default)]
    pub chain_generation: u64,
    /// Derived-cache outcome for this open.
    #[serde(default)]
    pub render_snapshot: String,
    /// Detailed service diagnostics; optional extensions do not alter negotiation.
    #[serde(default)]
    pub diagnostics: serde_json::Value,
    /// Human-readable integrity/availability diagnostics.
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl OpenResponse {
    /// Verify the service contract before sending snapshot-dependent requests.
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedProtocol` for old/new incompatible services or a
    /// missing opened snapshot identity.
    pub fn validate(&self) -> Result<(), ServiceError> {
        if self.protocol_version != PROTOCOL_VERSION || self.snapshot_id.is_empty() {
            return Err(ServiceError::new(
                ErrorCode::UnsupportedProtocol,
                "The history viewer requires service protocol version 2 with snapshot support.",
            ));
        }
        Ok(())
    }
}

/// A detail result tied to the snapshot that advertised its identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotResult<T> {
    /// Exact opened snapshot that handled this request.
    pub snapshot_id: SnapshotId,
    /// Existing flat result fields, retained for compatible readers.
    #[serde(flatten)]
    pub value: T,
}

impl RequestBody {
    /// Snapshot named by a request, or none for the Open/Refresh handshake.
    #[must_use]
    pub const fn snapshot_id(&self) -> Option<&SnapshotId> {
        match self {
            Self::RecordEditorEvents(_)
            | Self::GetHumanWork(_)
            | Self::GetEditorContext(_)
            | Self::Open(_)
            | Self::OpenLive(_)
            | Self::OpenLivePaged(_)
            | Self::SyncLive(_)
            | Self::Refresh(_) => None,
            Self::ToggleLive(request) => Some(&request.snapshot_id),
            Self::ViewportLive(request) => Some(&request.snapshot_id),
            Self::GetWindow(request) => Some(&request.snapshot_id),
            Self::LocateRows(request) => Some(&request.snapshot_id),
            Self::FindInHistory(request) => Some(&request.snapshot_id),
            Self::GetNodeDetails(request) => Some(&request.snapshot_id),
            Self::ResolveObject(request) => Some(&request.snapshot_id),
            Self::GetFileDiff(request) => Some(&request.snapshot_id),
        }
    }
}
