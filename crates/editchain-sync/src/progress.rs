//! Credential-free synchronization work, separate from durable receipt counts.

use serde::Serialize;

/// One direction's stable inventory check. Checked records may have unavailable
/// content, so completing a check is not itself a claim of complete hydration.
#[derive(Debug, Default, Clone, Serialize)]
pub struct CheckProgress {
    /// Connection-local pass number; zero before the first request.
    pub pass: u64,
    /// Exact record count in the consent-filtered, frozen inventory.
    /// Absent until the first inventory page arrives.
    pub total_records: Option<u64>,
    /// Records whose page has finished checking records and referenced content.
    pub checked_records: u64,
    /// The last page has finished, including all requested content responses.
    pub complete: bool,
    /// Content requests that could not be satisfied in this pass.
    pub unavailable: u64,
}

/// One partially downloaded object. These bytes are not durable receipts.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    /// Whether the object holds revision content rather than a record.
    pub content: bool,
    /// Bytes buffered locally, not yet validated and saved.
    pub received_bytes: u64,
    /// Length advertised by the first chunk; absent before that chunk.
    pub total_bytes: Option<u64>,
}

/// Current check work and durable, connection-local transfer counts.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Progress {
    /// Space/version negotiation has succeeded over an authenticated channel.
    pub accepted: bool,
    /// An incoming reconciliation round is in progress.
    pub synchronizing: bool,
    /// Completed incoming inventory rounds, not a claim about future edits.
    pub rounds: u64,
    /// Exact records durably received on this connection.
    pub records: u64,
    /// Content blobs durably received on this connection.
    pub blobs: u64,
    /// Sent records acknowledged as durable by the authenticated remote peer.
    pub sent_records: u64,
    /// Sent blobs acknowledged as durable by the authenticated remote peer.
    pub sent_blobs: u64,
    /// Missing content responses in the current or last incoming round.
    pub unavailable: u64,
    /// This device's check of the peer's shared history.
    pub incoming: CheckProgress,
    /// The peer's confirmed check of this device's shared history.
    pub outgoing: CheckProgress,
    /// Known records in this page still to be saved, including staged bytes.
    pub pending_records: u64,
    /// Known content objects queued or being downloaded. More may be discovered.
    pub pending_blobs: u64,
    /// Buffered progress of the current object; never counted as saved data.
    pub download: Option<DownloadProgress>,
}
