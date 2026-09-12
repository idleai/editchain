//! Source-control-style file row content, status, and accessibility labels.

use crate::app::row_input::RowInput;
use editchain_protocol::{FileChangeSource, FileChangeStatus};

/// Source-control status for a rendered file row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileRowStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unknown,
}

impl FileRowStatus {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Unknown => "?",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Modified => "Modified",
            Self::Deleted => "Deleted",
            Self::Renamed => "Renamed",
            Self::Copied => "Copied",
            Self::TypeChanged => "Type changed",
            Self::Unknown => "Changed",
        }
    }

    pub(crate) const fn class(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::TypeChanged => "type-changed",
            Self::Unknown => "unknown",
        }
    }
}

/// Column-aligned SCM content for one expandable Git, agent, or human edit child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileContent {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) directory: String,
    pub(crate) status: FileRowStatus,
    pub(crate) source: String,
    pub(crate) fidelity: String,
    pub(crate) binary: bool,
    pub(crate) partial: bool,
    pub(crate) title: String,
    pub(crate) aria_label: String,
}

pub(super) fn file_content(row: &RowInput) -> Option<FileContent> {
    let change = row.source.file_change.as_ref()?;
    let path = change.path.trim().replace('\\', "/");
    if path.is_empty() {
        return None;
    }
    let (directory, name) = path.rsplit_once('/').map_or_else(
        || (String::new(), path.clone()),
        |(directory, name)| (directory.to_owned(), name.to_owned()),
    );
    let status = match change.status {
        FileChangeStatus::Added => FileRowStatus::Added,
        FileChangeStatus::Modified => FileRowStatus::Modified,
        FileChangeStatus::Deleted => FileRowStatus::Deleted,
        FileChangeStatus::Renamed => FileRowStatus::Renamed,
        FileChangeStatus::Copied => FileRowStatus::Copied,
        FileChangeStatus::TypeChanged => FileRowStatus::TypeChanged,
        FileChangeStatus::Unknown => FileRowStatus::Unknown,
    };
    let source = match change.source {
        FileChangeSource::Git => "git",
        FileChangeSource::Agent => "agent",
        FileChangeSource::Human => "human",
        FileChangeSource::Unknown => "unknown",
    }
    .to_owned();
    let binary = change.binary;
    let partial = change.partial;
    let fidelity = if binary {
        "binary"
    } else if partial {
        "recorded"
    } else {
        ""
    }
    .to_owned();
    let old_path = change.old_path.as_deref().filter(|old| !old.is_empty());
    let mut title = format!("{} · {path}", status.label());
    if let Some(old_path) = old_path {
        title.push_str(" ← ");
        title.push_str(old_path);
    }
    if binary {
        title.push_str(" · binary content");
    } else if partial {
        title.push_str(" · recorded edit (partial file evidence)");
    } else if source == "git" {
        title.push_str(" · exact Git blobs");
    }
    let source_label = if source == "git" {
        "Git commit"
    } else if source == "human" {
        "human edit"
    } else if partial {
        "recorded agent edit"
    } else {
        "agent edit"
    };
    let fidelity_label = if binary {
        ", binary content"
    } else if partial {
        ", partial file evidence"
    } else {
        ""
    };
    let aria_label = format!(
        "{} {path}, {source_label}{fidelity_label}; open diff",
        status.label()
    );
    Some(FileContent {
        path,
        name,
        directory,
        status,
        source,
        fidelity,
        binary,
        partial,
        title,
        aria_label,
    })
}
