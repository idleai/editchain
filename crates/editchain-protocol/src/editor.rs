//! Versioned editor observations, independent of history-view snapshots.

use serde::{Deserialize, Serialize};

/// One durable, replayable batch from a workspace recorder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordEditorEvents {
    /// Absolute workspace root.
    pub workspace_path: String,
    /// Chain location, relative to the workspace or absolute.
    pub chain_dir: String,
    /// Ordered observations; identities remain unchanged on retry.
    pub events: Vec<EditorEvent>,
}

/// One observation in the `vscode.editor` source stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorEvent {
    /// Schema version, currently one.
    pub schema: u32,
    /// Random recorder incarnation, shared by all events until restart.
    pub session: String,
    /// Strictly increasing, one-based identity within the incarnation.
    pub sequence: u64,
    /// Observer wall time. Duration measurements use a monotonic clock.
    pub time_ms: u64,
    /// Observation payload. Window focus is deliberately not recorded.
    pub event: EditorEventKind,
}

/// Exact identity of an observed buffer revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorDocument {
    /// Document incarnation, distinct across close/reopen and language changes.
    pub id: String,
    /// Full document URI, including the scheme.
    pub uri: String,
    /// Workspace-relative file path; absent for untitled buffers.
    pub path: Option<String>,
    /// VS Code buffer version.
    pub version: u64,
}

/// A zero-based half-open UTF-16 range, as emitted by VS Code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorRange {
    /// Start line and UTF-16 column.
    pub start: [u32; 2],
    /// End line and UTF-16 column.
    pub end: [u32; 2],
}

/// One replacement in the original emitted order, against the evolving buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorChange {
    /// Offset in UTF-16 code units, not Rust byte offsets.
    pub offset: u32,
    /// Replaced length in UTF-16 code units.
    pub length: u32,
    /// Inserted text.
    pub text: String,
}

/// Stable editor observations and explicit human-work indicators.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditorEventKind {
    /// Independently observed Git context; this is not a workspace snapshot.
    WorkspaceContext {
        /// Observer wall time for the Git read.
        observed_ms: u64,
        /// Workspace root at observation time, never the later query location.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_path: Option<String>,
        /// Worktrees discovered within this workspace.
        repositories: Vec<editchain_core::human::HumanGitContext>,
    },
    /// Recorder policy and runtime version.
    TrackingStarted {
        /// Minimum continuous exposure used as a reading indicator.
        dwell_ms: u64,
        /// VS Code version.
        vscode_version: String,
    },
    /// Normal recorder shutdown.
    TrackingStopped,
    /// An explicit capture limitation or missing interval.
    TrackingGap {
        /// Human-readable reason, without file contents.
        reason: String,
    },
    /// Initial or recovered buffer content.
    DocumentSnapshot {
        /// Revision identity.
        document: EditorDocument,
        /// Exact text, including unsaved changes.
        text: String,
    },
    /// An observed text change; authorship is a separate indicator.
    DocumentChanged {
        /// Destination revision.
        document: EditorDocument,
        /// Source revision number.
        before_version: u64,
        /// Exact source content.
        before: String,
        /// Exact destination content.
        after: String,
        /// Raw changes in emitted order.
        changes: Vec<EditorChange>,
        /// Stable reason: undo, redo, or absent.
        reason: Option<String>,
    },
    /// Human intent inferred from keyboard selection or undo/redo.
    HumanEdit {
        /// Earlier document-change sequence in this recorder incarnation.
        change: u64,
        /// Observable basis, not a verified author identity.
        signal: String,
    },
    /// Saved revision. An edit does not imply a save.
    DocumentSaved {
        /// Saved buffer identity.
        document: EditorDocument,
    },
    /// Explicit file or directory rename.
    DocumentRenamed {
        /// Previous workspace-relative path.
        from: String,
        /// Destination workspace-relative path.
        to: String,
    },
    /// A text tab opened; this alone does not indicate exposure.
    EditorOpened {
        /// Tab identity, distinguishing split views.
        editor: String,
        /// Document URI.
        uri: String,
    },
    /// A text tab closed.
    EditorClosed {
        /// Previously opened tab identity.
        editor: String,
        /// Document URI.
        uri: String,
    },
    /// The active text editor changed.
    EditorActivated {
        /// Active document, absent when no tracked editor is active.
        document: Option<EditorDocument>,
    },
    /// Cursor or selection movement.
    SelectionChanged {
        /// Exact buffer revision.
        document: EditorDocument,
        /// Visible editor identity.
        editor: String,
        /// Selections in API order.
        ranges: Vec<EditorRange>,
        /// Whether the API reported keyboard input.
        keyboard: bool,
    },
    /// Viewport geometry changed; the API does not identify a scroll cause.
    VisibleRangesChanged {
        /// Exact buffer revision.
        document: EditorDocument,
        /// Visible editor identity.
        editor: String,
        /// Disjoint visible ranges; folded gaps remain excluded.
        ranges: Vec<EditorRange>,
    },
    /// Continuous exposure in an active editor while the window is foreground.
    CodeExposure {
        /// Exact buffer revision.
        document: EditorDocument,
        /// Visible editor identity.
        editor: String,
        /// Ranges visible throughout this interval.
        ranges: Vec<EditorRange>,
        /// Wall time at interval start.
        started_ms: u64,
        /// Monotonic elapsed duration, bounded by the recorder heartbeat.
        duration_ms: u64,
    },
}

impl RecordEditorEvents {
    pub(super) fn validate(&self) -> Result<(), crate::ServiceError> {
        let invalid = |message| crate::ServiceError::new(crate::ErrorCode::InvalidInput, message);
        if self.events.is_empty() || self.events.len() > 128 {
            return Err(invalid("editor batch must contain 1..128 events"));
        }
        for event in &self.events {
            if event.schema != 1
                || event.sequence == 0
                || event.sequence > 9_007_199_254_740_991
                || event.session.len() != 36
                || !event
                    .session
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
            {
                return Err(invalid(
                    "unsupported editor schema or invalid event identity",
                ));
            }
            if (event.sequence == 1)
                != matches!(event.event, EditorEventKind::TrackingStarted { .. })
            {
                return Err(invalid("editor stream must begin with tracking_started"));
            }
            event.validate_content().map_err(invalid)?;
            if let EditorEventKind::HumanEdit { change, .. } = event.event {
                if change == 0 || change >= event.sequence {
                    return Err(invalid("human edit must refer to an earlier observation"));
                }
            }
        }
        Ok(())
    }
}

impl EditorEvent {
    fn validate_content(&self) -> Result<(), &'static str> {
        self.validate_document()?;
        match &self.event {
            EditorEventKind::WorkspaceContext {
                repositories,
                workspace_path,
                ..
            } => {
                if workspace_path.as_ref().is_some_and(|root| {
                    root.len() > 4096 || !std::path::Path::new(root).is_absolute()
                }) || repositories.len() > 64
                    || repositories.iter().any(|repo| {
                        repo.repository.parse::<u64>().is_err()
                            || repo.root.len() > 4096
                            || !std::path::Path::new(&repo.root).is_absolute()
                            || repo.head.as_ref().is_some_and(|head| {
                                editchain_core::GitOid::from_hex(head).is_none()
                            })
                    })
                {
                    return Err("invalid recorded Git context");
                }
            }
            EditorEventKind::TrackingStarted { dwell_ms, .. } => {
                if !(500..=30000).contains(dwell_ms) {
                    return Err("invalid reading dwell threshold");
                }
            }
            EditorEventKind::DocumentChanged {
                document,
                before_version,
                before,
                after,
                changes,
                reason,
            } => {
                if *before_version >= document.version
                    || changes.is_empty()
                    || changes.len() > 10000
                    || reason
                        .as_deref()
                        .is_some_and(|reason| !matches!(reason, "undo" | "redo"))
                {
                    return Err("invalid document change revision or reason");
                }
                let mut text: Vec<_> = before.encode_utf16().collect();
                for change in changes {
                    let start =
                        usize::try_from(change.offset).map_err(|_error| "invalid edit offset")?;
                    let end = start
                        .checked_add(
                            usize::try_from(change.length)
                                .map_err(|_error| "invalid edit length")?,
                        )
                        .ok_or("edit overflow")?;
                    if start > end || end > text.len() {
                        return Err("edit outside source buffer");
                    }
                    drop(text.splice(start..end, change.text.encode_utf16()));
                    if text.len() > 1_048_576 {
                        return Err("editor buffer exceeds capture limit");
                    }
                }
                if !text.iter().copied().eq(after.encode_utf16()) {
                    return Err("editor changes do not replay to recorded after text");
                }
            }
            EditorEventKind::HumanEdit { signal, .. } => {
                if !matches!(signal.as_str(), "keyboard_selection" | "undo" | "redo") {
                    return Err("unknown human edit signal");
                }
            }
            EditorEventKind::SelectionChanged { ranges, .. }
            | EditorEventKind::VisibleRangesChanged { ranges, .. }
            | EditorEventKind::CodeExposure { ranges, .. } => {
                for range in ranges {
                    if range.start > range.end
                        || range.end.into_iter().any(|value| value > 1_048_576)
                    {
                        return Err("invalid editor range");
                    }
                }
                if let EditorEventKind::CodeExposure { duration_ms, .. } = self.event {
                    if duration_ms > 60000 {
                        return Err("exposure exceeds heartbeat bound");
                    }
                }
            }
            EditorEventKind::TrackingStopped
            | EditorEventKind::TrackingGap { .. }
            | EditorEventKind::DocumentSnapshot { .. }
            | EditorEventKind::DocumentSaved { .. }
            | EditorEventKind::DocumentRenamed { .. }
            | EditorEventKind::EditorOpened { .. }
            | EditorEventKind::EditorClosed { .. }
            | EditorEventKind::EditorActivated { .. } => {}
        }
        Ok(())
    }

    fn validate_document(&self) -> Result<(), &'static str> {
        let document = match &self.event {
            EditorEventKind::DocumentSnapshot { document, .. }
            | EditorEventKind::DocumentChanged { document, .. }
            | EditorEventKind::DocumentSaved { document }
            | EditorEventKind::SelectionChanged { document, .. }
            | EditorEventKind::VisibleRangesChanged { document, .. }
            | EditorEventKind::CodeExposure { document, .. } => Some(document),
            EditorEventKind::EditorActivated { document } => document.as_ref(),
            EditorEventKind::WorkspaceContext { .. }
            | EditorEventKind::TrackingStarted { .. }
            | EditorEventKind::TrackingStopped
            | EditorEventKind::TrackingGap { .. }
            | EditorEventKind::HumanEdit { .. }
            | EditorEventKind::DocumentRenamed { .. }
            | EditorEventKind::EditorOpened { .. }
            | EditorEventKind::EditorClosed { .. } => None,
        };
        if document.is_some_and(|document| {
            document.id.is_empty()
                || document.id.len() > 256
                || document.path.as_ref().is_some_and(|path| {
                    let parsed = std::path::Path::new(path);
                    path.is_empty()
                        || path.len() > 4096
                        || parsed.is_absolute()
                        || parsed
                            .components()
                            .any(|part| part == std::path::Component::ParentDir)
                })
        }) {
            return Err("invalid editor document identity or workspace-relative path");
        }
        Ok(())
    }
}
