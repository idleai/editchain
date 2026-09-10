//! Row badges, activity icons, session provenance, and bundle presentation.

use super::group_label_text;
use super::markdown::markdown_plain_line;
use crate::app::row_input::RowInput;
use editchain_protocol::ParentRelationKind;

/// The frozen outcome-badge visibility switch (default off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct BadgeOptions {
    pub(crate) show_success_outcome: bool,
}

// --- Badge / chrome layer ---------------------------------------------------

/// One assembled row tag (DOM-ready exact class list, text, and labels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChromeItem {
    /// Exact production class list (e.g. `"rel-badge rel-subagent"`).
    pub(crate) classes: String,
    /// Display text (relation glyph + label, outcome, prefix, or count).
    pub(crate) text: String,
    /// Hover title.
    pub(crate) title: String,
    /// `aria-label` where production supplies one.
    pub(crate) aria_label: Option<String>,
}

impl ChromeItem {
    pub(super) fn new(
        classes: &str,
        text: &str,
        title: &str,
        aria_label: Option<&str>,
    ) -> ChromeItem {
        ChromeItem {
            classes: classes.to_owned(),
            text: text.to_owned(),
            title: title.to_owned(),
            aria_label: aria_label.map(str::to_owned),
        }
    }
}

/// The `REL_LABELS` table (structural parent-relation kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelationKind {
    Subagent,
    Reconnect,
    Fork,
}

impl RelationKind {
    pub(crate) fn class(self) -> &'static str {
        match self {
            RelationKind::Subagent => "rel-subagent",
            RelationKind::Reconnect => "rel-reconnect",
            RelationKind::Fork => "rel-fork",
        }
    }

    pub(crate) fn glyph(self) -> &'static str {
        match self {
            RelationKind::Subagent => "↳",
            RelationKind::Reconnect => "↩",
            RelationKind::Fork => "⇉",
        }
    }

    pub(crate) fn text(self) -> &'static str {
        match self {
            RelationKind::Subagent => "subagent",
            RelationKind::Reconnect => "return",
            RelationKind::Fork => "fork",
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            RelationKind::Subagent => "Starts a subagent branch",
            RelationKind::Reconnect => "Completion returns into the subagent branch",
            RelationKind::Fork => "Branches off the target row at a fork boundary",
        }
    }
}

/// `relationBadges` — compact badges for a row's parent relations, de-duped.
pub(crate) fn relation_badges(row: &RowInput) -> Vec<ChromeItem> {
    let relations = &row.source.parent_relations;
    let mut seen: Vec<RelationKind> = Vec::new();
    let mut out = Vec::new();
    for relation in relations {
        let kind = match relation.kind {
            ParentRelationKind::Subagent => RelationKind::Subagent,
            ParentRelationKind::Reconnect => RelationKind::Reconnect,
            ParentRelationKind::Fork => RelationKind::Fork,
            ParentRelationKind::ProducedCommit | ParentRelationKind::Unknown => continue,
        };
        if seen.contains(&kind) {
            continue;
        }
        seen.push(kind);
        out.push(ChromeItem::new(
            &format!("rel-badge {}", kind.class()),
            &format!("{} {}", kind.glyph(), kind.text()),
            kind.title(),
            Some(kind.title()),
        ));
    }
    out
}

/// One self-contained VS Code Codicon used at the start of structured Content.
///
/// The SVG geometry is compiled into the WASM renderer so icons remain visible
/// in a sandboxed webview without relying on workbench-global icon fonts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityIcon {
    Work,
    Agent,
    User,
    Plan,
    Explore,
    Execute,
    Change,
    Verify,
    Diagnose,
    Coordinate,
    SourceControl,
    External,
    System,
}

impl ActivityIcon {
    /// Stable Codicon name exposed to DOM/test probes.
    pub(crate) fn name(self) -> &'static str {
        match self {
            ActivityIcon::Work => "layers",
            ActivityIcon::Agent => "robot",
            ActivityIcon::User => "account",
            ActivityIcon::Plan => "checklist",
            ActivityIcon::Explore => "search",
            ActivityIcon::Execute => "tools",
            ActivityIcon::Change => "edit",
            ActivityIcon::Verify => "pass",
            ActivityIcon::Diagnose => "bug",
            ActivityIcon::Coordinate => "type-hierarchy",
            ActivityIcon::SourceControl => "source-control",
            ActivityIcon::External => "link-external",
            ActivityIcon::System => "settings",
        }
    }

    /// Source view box for this Codicon's vector geometry.
    pub(crate) fn view_box(self) -> &'static str {
        match self {
            ActivityIcon::SourceControl => "0 0 24 24",
            ActivityIcon::Work
            | ActivityIcon::Agent
            | ActivityIcon::User
            | ActivityIcon::Plan
            | ActivityIcon::Explore
            | ActivityIcon::Execute
            | ActivityIcon::Change
            | ActivityIcon::Verify
            | ActivityIcon::Diagnose
            | ActivityIcon::Coordinate
            | ActivityIcon::External
            | ActivityIcon::System => "0 0 16 16",
        }
    }

    /// Vector paths that make up the icon.
    pub(crate) fn paths(self) -> &'static [ActivityIconPath] {
        match self {
            ActivityIcon::Work => &[
                ActivityIconPath {
                    d: r"M8 8.99993C7.819 8.99993 7.643 8.95093 7.486 8.85793L2.486 5.85693C2.186 5.67793 2 5.34893 2 4.99993C2 4.65093 2.187 4.32093 2.486 4.14193L7.486 1.14293C7.789 0.95693 8.207 0.95493 8.517 1.14493L13.513 4.14293C13.813 4.32293 13.999 4.65093 13.999 4.99993C13.999 5.34893 13.812 5.67893 13.513 5.85793L8.513 8.85693C8.357 8.95093 8.181 8.99993 8 8.99993ZM8 1.99993L3 4.99993L8 7.99993L13 4.99993L8 1.99993Z",
                    even_odd: false,
                },
                ActivityIconPath {
                    d: r"M2.146 6.9873L8 10.5003L13.854 6.9873C13.946 7.1413 14 7.3173 14 7.5003C14 7.8493 13.814 8.1783 13.514 8.3583L8.514 11.3573C8.357 11.4513 8.181 11.5003 8 11.5003C7.819 11.5003 7.642 11.4513 7.486 11.3583L2.486 8.35731C2.187 8.17931 2 7.8503 2 7.5003C2 7.3163 2.054 7.1403 2.146 6.9873Z",
                    even_odd: false,
                },
                ActivityIconPath {
                    d: r"M2.146 9.4873L8 13.0003L13.854 9.4873C13.946 9.6413 14 9.8173 14 10.0003C14 10.3493 13.814 10.6783 13.514 10.8583L8.514 13.8573C8.357 13.9513 8.181 14.0003 8 14.0003C7.819 14.0003 7.642 13.9513 7.486 13.8583L2.486 10.8573C2.187 10.6793 2 10.3503 2 10.0003C2 9.8163 2.054 9.6403 2.146 9.4873Z",
                    even_odd: false,
                },
            ],
            ActivityIcon::Agent => &[ActivityIconPath {
                d: r"M12 9H4C3.173 9 2.5 9.673 2.5 10.5V11C2.5 11.123 2.562 14 8 14C13.438 14 13.5 11.123 13.5 11V10.5C13.5 9.673 12.827 9 12 9ZM12.5 10.991C12.497 11.073 12.372 13 8 13C3.628 13 3.503 11.073 3.5 11V10.5C3.5 10.224 3.724 10 4 10H12C12.276 10 12.5 10.224 12.5 10.5V10.991ZM5.5 8H10.5C11.327 8 12 7.327 12 6.5V3.5C12 2.673 11.327 2 10.5 2H8.5V1.5C8.5 1.224 8.276 1 8 1C7.724 1 7.5 1.224 7.5 1.5V2H5.5C4.673 2 4 2.673 4 3.5V6.5C4 7.327 4.673 8 5.5 8ZM5 3.5C5 3.224 5.224 3 5.5 3H10.5C10.776 3 11 3.224 11 3.5V6.5C11 6.776 10.776 7 10.5 7H5.5C5.224 7 5 6.776 5 6.5V3.5ZM5.75 5C5.75 4.586 6.086 4.25 6.5 4.25C6.914 4.25 7.25 4.586 7.25 5C7.25 5.414 6.914 5.75 6.5 5.75C6.086 5.75 5.75 5.414 5.75 5ZM8.75 5C8.75 4.586 9.086 4.25 9.5 4.25C9.914 4.25 10.25 4.586 10.25 5C10.25 5.414 9.914 5.75 9.5 5.75C9.086 5.75 8.75 5.414 8.75 5Z",
                even_odd: false,
            }],
            ActivityIcon::User => &[ActivityIconPath {
                d: r"M8 2C4.686 2 2 4.686 2 8C2 11.314 4.686 14 8 14C11.314 14 14 11.314 14 8C14 4.686 11.314 2 8 2ZM1 8C1 4.134 4.134 1 8 1C11.866 1 15 4.134 15 8C15 11.866 11.866 15 8 15C4.134 15 1 11.866 1 8ZM8 12.25C9.933 12.25 11.5 11.036 11.5 9.214C11.5 8.543 10.956 8 10.286 8H5.715C5.044 8 4.501 8.544 4.501 9.214C4.501 11.035 6.068 12.25 8.001 12.25H8ZM8 7.25C9.036 7.25 9.875 6.411 9.875 5.375C9.875 4.339 9.036 3.5 8 3.5C6.964 3.5 6.125 4.339 6.125 5.375C6.125 6.411 6.964 7.25 8 7.25Z",
                even_odd: false,
            }],
            ActivityIcon::Plan => &[ActivityIconPath {
                d: r"M4.85401 2.146C5.04901 2.341 5.04901 2.658 4.85401 2.853L2.85401 4.853C2.65901 5.048 2.34201 5.048 2.14701 4.853L1.14701 3.853C0.952013 3.658 0.952013 3.341 1.14701 3.146C1.34201 2.951 1.65901 2.951 1.85401 3.146L2.50001 3.792L4.14601 2.146C4.34101 1.951 4.65901 1.951 4.85401 2.146ZM14.5 4H6.50001C6.22401 4 6.00001 3.776 6.00001 3.5C6.00001 3.224 6.22401 3 6.50001 3H14.5C14.776 3 15 3.224 15 3.5C15 3.776 14.776 4 14.5 4ZM4.85401 11.146C5.04901 11.341 5.04901 11.658 4.85401 11.853L2.85401 13.853C2.65901 14.048 2.34201 14.048 2.14701 13.853L1.14701 12.853C0.952013 12.658 0.952013 12.341 1.14701 12.146C1.34201 11.951 1.65901 11.951 1.85401 12.146L2.50001 12.792L4.14601 11.146C4.34101 10.951 4.65901 10.951 4.85401 11.146ZM14.5 13H6.50001C6.22401 13 6.00001 12.776 6.00001 12.5C6.00001 12.224 6.22401 12 6.50001 12H14.5C14.776 12 15 12.224 15 12.5C15 12.776 14.776 13 14.5 13ZM4.85401 6.646C5.04901 6.841 5.04901 7.158 4.85401 7.353L2.85401 9.353C2.65901 9.548 2.34201 9.548 2.14701 9.353L1.14701 8.353C0.952013 8.158 0.952013 7.841 1.14701 7.646C1.34201 7.451 1.65901 7.451 1.85401 7.646L2.50001 8.292L4.14601 6.646C4.34101 6.451 4.65901 6.451 4.85401 6.646ZM14.5 8.5H6.50001C6.22401 8.5 6.00001 8.276 6.00001 8C6.00001 7.724 6.22401 7.5 6.50001 7.5H14.5C14.776 7.5 15 7.724 15 8C15 8.276 14.776 8.5 14.5 8.5Z",
                even_odd: false,
            }],
            ActivityIcon::Explore => &[ActivityIconPath {
                d: r"M10.0195 10.7266C9.06578 11.5217 7.83875 12 6.5 12C3.46243 12 1 9.53757 1 6.5C1 3.46243 3.46243 1 6.5 1C9.53757 1 12 3.46243 12 6.5C12 7.83875 11.5217 9.06578 10.7266 10.0195L13.8535 13.1464C14.0488 13.3417 14.0488 13.6583 13.8535 13.8536C13.6583 14.0488 13.3417 14.0488 13.1464 13.8536L10.0195 10.7266ZM11 6.5C11 4.01472 8.98528 2 6.5 2C4.01472 2 2 4.01472 2 6.5C2 8.98528 4.01472 11 6.5 11C8.98528 11 11 8.98528 11 6.5Z",
                even_odd: false,
            }],
            ActivityIcon::Execute => &[ActivityIconPath {
                d: r"M5.66901 0.999997C5.52101 0.945997 5.34701 0.968997 5.21401 1.062C5.08101 1.155 5.00201 1.308 5.00201 1.47V3.286C5.00201 3.561 4.77701 3.786 4.50201 3.786C4.22701 3.786 4.00201 3.561 4.00201 3.286V1.47C4.00201 1.308 3.92301 1.156 3.79001 1.062C3.65801 0.967997 3.48501 0.945997 3.33501 0.999997C1.93901 1.495 1.00201 2.816 1.00201 4.287C1.00201 5.646 1.79201 6.876 3.00201 7.449V13.5C3.00201 14.327 3.67501 15 4.50201 15C5.32901 15 6.00201 14.327 6.00201 13.5V7.449C7.21201 6.876 8.00201 5.646 8.00201 4.287C8.00201 2.816 7.06401 1.495 5.66901 0.999997ZM5.33601 6.644C5.13601 6.714 5.00201 6.904 5.00201 7.116V13.501C5.00201 13.776 4.77701 14.001 4.50201 14.001C4.22701 14.001 4.00201 13.776 4.00201 13.501V7.116C4.00201 6.904 3.86801 6.715 3.66801 6.644C2.67201 6.292 2.00201 5.345 2.00201 4.288C2.00201 3.496 2.38501 2.765 3.00201 2.301V3.288C3.00201 4.115 3.67501 4.788 4.50201 4.788C5.32901 4.788 6.00201 4.115 6.00201 3.288V2.301C6.61901 2.765 7.00201 3.496 7.00201 4.288C7.00201 5.346 6.33201 6.293 5.33601 6.644ZM13.5 8H13.002V4.118L13.449 3.223C13.509 3.105 13.518 2.967 13.476 2.841L12.976 1.341C12.908 1.137 12.716 0.998997 12.501 0.998997H10.501C10.286 0.998997 10.095 1.137 10.026 1.341L9.52601 2.841C9.48401 2.967 9.49401 3.105 9.55301 3.223L10 4.118V8H9.50001C9.22401 8 9.00001 8.224 9.00001 8.5V12.5C9.00001 13.879 10.121 15 11.5 15C12.879 15 14 13.879 14 12.5V8.5C14 8.224 13.776 8 13.5 8ZM10.862 2.001H12.141L12.461 2.963L12.054 3.777C12.02 3.846 12.001 3.923 12.001 4.001V8.001H11.001V4.001C11.001 3.924 10.983 3.847 10.949 3.777L10.542 2.963L10.862 2.001ZM13.002 12.5C13.002 13.327 12.329 14 11.502 14C10.675 14 10.002 13.327 10.002 12.5V9H13.002V12.5Z",
                even_odd: false,
            }],
            ActivityIcon::Change => &[ActivityIconPath {
                d: r"M14.236 1.76386C13.2123 0.740172 11.5525 0.740171 10.5289 1.76386L2.65722 9.63549C2.28304 10.0097 2.01623 10.4775 1.88467 10.99L1.01571 14.3755C0.971767 14.5467 1.02148 14.7284 1.14646 14.8534C1.27144 14.9783 1.45312 15.028 1.62432 14.9841L5.00978 14.1151C5.52234 13.9836 5.99015 13.7168 6.36433 13.3426L14.236 5.47097C15.2596 4.44728 15.2596 2.78755 14.236 1.76386ZM11.236 2.47097C11.8691 1.8378 12.8957 1.8378 13.5288 2.47097C14.162 3.10413 14.162 4.1307 13.5288 4.76386L12.75 5.54269L10.4571 3.24979L11.236 2.47097ZM9.75002 3.9569L12.0429 6.24979L5.65722 12.6355C5.40969 12.883 5.10023 13.0595 4.76117 13.1465L2.19447 13.8053L2.85327 11.2386C2.9403 10.8996 3.1168 10.5901 3.36433 10.3426L9.75002 3.9569Z",
                even_odd: false,
            }],
            ActivityIcon::Verify => &[
                ActivityIconPath {
                    d: r"M10.6484 5.64648C10.8434 5.45148 11.1605 5.45148 11.3555 5.64648C11.5498 5.84137 11.5499 6.15766 11.3555 6.35254L7.35547 10.3525C7.25747 10.4495 7.12898 10.499 7.00098 10.499C6.87299 10.499 6.74545 10.4505 6.64746 10.3525L4.64746 8.35254C4.45247 8.15754 4.45248 7.84148 4.64746 7.64648C4.84246 7.45148 5.15949 7.45148 5.35449 7.64648L7 9.29199L10.6465 5.64648H10.6484Z",
                    even_odd: false,
                },
                ActivityIconPath {
                    d: r"M8 1C11.86 1 15 4.14 15 8C15 11.86 11.86 15 8 15C4.14 15 1 11.86 1 8C1 4.14 4.14 1 8 1ZM8 2C4.691 2 2 4.691 2 8C2 11.309 4.691 14 8 14C11.309 14 14 11.309 14 8C14 4.691 11.309 2 8 2Z",
                    even_odd: true,
                },
            ],
            ActivityIcon::Diagnose => &[ActivityIconPath {
                d: r"M14.5 8H13V6C13 5.63 12.898 5.283 12.722 4.985L13.853 3.854C14.048 3.659 14.048 3.342 13.853 3.147C13.658 2.952 13.341 2.952 13.146 3.147L12.015 4.278C11.717 4.102 11.37 4 11 4C11 2.346 9.654 1 8 1C6.346 1 5 2.346 5 4C4.63 4 4.283 4.102 3.985 4.278L2.854 3.147C2.659 2.952 2.342 2.952 2.147 3.147C1.952 3.342 1.952 3.659 2.147 3.854L3.278 4.985C3.102 5.283 3 5.63 3 6V8H1.5C1.224 8 1 8.224 1 8.5C1 8.776 1.224 9 1.5 9H3C3 10.199 3.424 11.3 4.13 12.163L2.396 13.897C2.201 14.092 2.201 14.409 2.396 14.604C2.494 14.702 2.622 14.75 2.75 14.75C2.878 14.75 3.006 14.701 3.104 14.604L4.838 12.87C5.7 13.576 6.802 14 8.001 14C9.2 14 10.301 13.576 11.164 12.87L12.898 14.604C12.996 14.702 13.124 14.75 13.252 14.75C13.38 14.75 13.508 14.701 13.606 14.604C13.801 14.409 13.801 14.092 13.606 13.897L11.872 12.163C12.578 11.301 13.002 10.199 13.002 9H14.502C14.778 9 15.002 8.776 15.002 8.5C15.002 8.224 14.778 8 14.502 8H14.5ZM8 2C9.103 2 10 2.897 10 4H6C6 2.897 6.897 2 8 2ZM12 9C12 11.206 10.206 13 8 13C5.794 13 4 11.206 4 9V6C4 5.449 4.448 5 5 5H11C11.552 5 12 5.449 12 6V9Z",
                even_odd: false,
            }],
            ActivityIcon::Coordinate => &[ActivityIconPath {
                d: r"M8 1C6.61929 1 5.5 2.11929 5.5 3.5C5.5 4.7093 6.35863 5.71806 7.4995 5.94989V6.99994H5.36684C4.61209 6.99994 4.00024 7.61178 4.00024 8.36653V10.05C2.859 10.2815 2 11.2904 2 12.5C2 13.8807 3.11929 15 4.5 15C5.88071 15 7 13.8807 7 12.5C7 11.2906 6.14124 10.2818 5.00024 10.0501V8.36653C5.00024 8.16407 5.16437 7.99994 5.36684 7.99994H10.6337C10.8361 7.99994 11.0002 8.16407 11.0002 8.36653V10.05C9.859 10.2815 9 11.2904 9 12.5C9 13.8807 10.1193 15 11.5 15C12.8807 15 14 13.8807 14 12.5C14 11.2906 13.1412 10.2818 12.0002 10.0501V8.36653C12.0002 7.61178 11.3884 6.99994 10.6337 6.99994H8.4995V5.95009C9.64087 5.71865 10.5 4.70966 10.5 3.5C10.5 2.11929 9.38071 1 8 1ZM6.5 3.5C6.5 2.67157 7.17157 2 8 2C8.82843 2 9.5 2.67157 9.5 3.5C9.5 4.32843 8.82843 5 8 5C7.17157 5 6.5 4.32843 6.5 3.5ZM3 12.5C3 11.6716 3.67157 11 4.5 11C5.32843 11 6 11.6716 6 12.5C6 13.3284 5.32843 14 4.5 14C3.67157 14 3 13.3284 3 12.5ZM11.5 11C12.3284 11 13 11.6716 13 12.5C13 13.3284 12.3284 14 11.5 14C10.6716 14 10 13.3284 10 12.5C10 11.6716 10.6716 11 11.5 11Z",
                even_odd: false,
            }],
            ActivityIcon::SourceControl => &[ActivityIconPath {
                d: r"M21 8.25C21 6.1815 19.3185 4.5 17.25 4.5C15.1815 4.5 13.5 6.1815 13.5 8.25C13.5 10.023 14.739 11.5035 16.395 11.892C16.116 12.819 15.2655 13.5 14.25 13.5H9.75C8.9025 13.5 8.1285 13.7925 7.5 14.268V7.4235C9.21 7.0755 10.5 5.5605 10.5 3.75C10.5 1.6815 8.8185 0 6.75 0C4.6815 0 3 1.6815 3 3.75C3 5.562 4.29 7.0755 6 7.4235V16.575C4.29 16.923 3 18.438 3 20.2485C3 22.317 4.6815 23.9985 6.75 23.9985C8.8185 23.9985 10.5 22.317 10.5 20.2485C10.5 18.4755 9.261 16.995 7.605 16.6065C7.884 15.6795 8.7345 14.9985 9.75 14.9985H14.25C16.0845 14.9985 17.61 13.6725 17.931 11.9295C19.674 11.607 21 10.0845 21 8.25ZM4.5 3.75C4.5 2.5095 5.5095 1.5 6.75 1.5C7.9905 1.5 9 2.5095 9 3.75C9 4.9905 7.9905 6 6.75 6C5.5095 6 4.5 4.9905 4.5 3.75ZM9 20.25C9 21.4905 7.9905 22.5 6.75 22.5C5.5095 22.5 4.5 21.4905 4.5 20.25C4.5 19.0095 5.5095 18 6.75 18C7.9905 18 9 19.0095 9 20.25ZM17.25 10.5C16.0095 10.5 15 9.4905 15 8.25C15 7.0095 16.0095 6 17.25 6C18.4905 6 19.5 7.0095 19.5 8.25C19.5 9.4905 18.4905 10.5 17.25 10.5Z",
                even_odd: false,
            }],
            ActivityIcon::External => &[ActivityIconPath {
                d: r"M15 9.5V12.5C15 13.879 13.879 15 12.5 15H3.5C2.121 15 1 13.879 1 12.5V3.5C1 2.121 2.121 1 3.5 1H6.5C6.776 1 7 1.224 7 1.5C7 1.776 6.776 2 6.5 2H3.5C2.673 2 2 2.673 2 3.5V12.5C2 13.327 2.673 14 3.5 14H12.5C13.327 14 14 13.327 14 12.5V9.5C14 9.224 14.224 9 14.5 9C14.776 9 15 9.224 15 9.5ZM14.5 1H9.5C9.224 1 9 1.224 9 1.5C9 1.776 9.224 2 9.5 2H13.293L9.147 6.146C8.952 6.341 8.952 6.658 9.147 6.853C9.245 6.951 9.373 6.999 9.501 6.999C9.629 6.999 9.757 6.95 9.855 6.853L14.001 2.707V6.5C14.001 6.776 14.225 7 14.501 7C14.777 7 15.001 6.776 15.001 6.5V1.5C15.001 1.224 14.777 1 14.501 1H14.5Z",
                even_odd: false,
            }],
            ActivityIcon::System => &[ActivityIconPath {
                d: r"M6 9.5C6.93191 9.5 7.71496 10.1374 7.93699 11L13.5 11C13.7761 11 14 11.2239 14 11.5C14 11.7455 13.8231 11.9496 13.5899 11.9919L13.5 12L7.93673 12.001C7.71435 12.8631 6.93155 13.5 6 13.5C5.06845 13.5 4.28565 12.8631 4.06327 12.001L2.5 12C2.22386 12 2 11.7761 2 11.5C2 11.2545 2.17688 11.0504 2.41012 11.0081L2.5 11L4.06301 11C4.28504 10.1374 5.06809 9.5 6 9.5ZM6 10.5C5.44772 10.5 5 10.9477 5 11.5C5 12.0523 5.44772 12.5 6 12.5C6.55228 12.5 7 12.0523 7 11.5C7 10.9477 6.55228 10.5 6 10.5ZM10 2.5C10.9319 2.5 11.715 3.13738 11.937 3.99998L13.5 4C13.7761 4 14 4.22386 14 4.5C14 4.74546 13.8231 4.94961 13.5899 4.99194L13.5 5L11.9367 5.00102C11.7144 5.86312 10.9316 6.5 10 6.5C9.06845 6.5 8.28565 5.86312 8.06327 5.00102L2.5 5C2.22386 5 2 4.77614 2 4.5C2 4.25454 2.17688 4.05039 2.41012 4.00806L2.5 4L8.06301 3.99998C8.28504 3.13738 9.06809 2.5 10 2.5ZM10 3.5C9.44772 3.5 9 3.94772 9 4.5C9 5.05228 9.44772 5.5 10 5.5C10.5523 5.5 11 5.05228 11 4.5C11 3.94772 10.5523 3.5 10 3.5Z",
                even_odd: false,
            }],
        }
    }

    /// Resolve the Content icon for one stable Activity classification.
    fn from_label(label: &str) -> Option<ActivityIcon> {
        match label {
            "work" => Some(ActivityIcon::Work),
            "agent" => Some(ActivityIcon::Agent),
            "user" => Some(ActivityIcon::User),
            "plan" => Some(ActivityIcon::Plan),
            "explore" => Some(ActivityIcon::Explore),
            "tooluse" => Some(ActivityIcon::Execute),
            "change" => Some(ActivityIcon::Change),
            "verify" => Some(ActivityIcon::Verify),
            "diagnose" => Some(ActivityIcon::Diagnose),
            "coordinate" => Some(ActivityIcon::Coordinate),
            "git" => Some(ActivityIcon::SourceControl),
            "external" => Some(ActivityIcon::External),
            "meta" | "system" => Some(ActivityIcon::System),
            _ => None,
        }
    }
}

/// One vector path inside a Content icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActivityIconPath {
    pub(crate) d: &'static str,
    pub(crate) even_odd: bool,
}

/// Provider-neutral activities with concise labels for the dedicated Activity
/// column. Conversation rows are refined to `agent` or `user` from their
/// author; the remaining values are presentation labels rather than badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityKind {
    Work,
    Conversation,
    Plan,
    Explore,
    Execute,
    Change,
    Verify,
    Diagnose,
    Coordinate,
    SourceControl,
    External,
    System,
}

impl ActivityKind {
    pub(crate) fn from_wire(kind: &str) -> Option<ActivityKind> {
        match kind {
            "work" => Some(ActivityKind::Work),
            "conversation" => Some(ActivityKind::Conversation),
            "plan" => Some(ActivityKind::Plan),
            "explore" => Some(ActivityKind::Explore),
            "execute" => Some(ActivityKind::Execute),
            "change" => Some(ActivityKind::Change),
            "verify" => Some(ActivityKind::Verify),
            "diagnose" => Some(ActivityKind::Diagnose),
            "coordinate" => Some(ActivityKind::Coordinate),
            "source_control" => Some(ActivityKind::SourceControl),
            "external" => Some(ActivityKind::External),
            "system" => Some(ActivityKind::System),
            _ => None,
        }
    }

    pub(crate) fn text(self) -> &'static str {
        match self {
            ActivityKind::Work => "work",
            ActivityKind::Conversation => "agent",
            ActivityKind::Plan => "plan",
            ActivityKind::Explore => "explore",
            ActivityKind::Execute => "tooluse",
            ActivityKind::Change => "change",
            ActivityKind::Verify => "verify",
            ActivityKind::Diagnose => "diagnose",
            ActivityKind::Coordinate => "coordinate",
            ActivityKind::SourceControl => "git",
            ActivityKind::External => "external",
            ActivityKind::System => "meta",
        }
    }
}

/// One resolved value for the dedicated Activity column. Every real row gets
/// a non-empty label; placeholders intentionally retain the empty default.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RowClassification {
    /// Compact visual label (`tooluse`, `git`, `agent`, `user`, or a wire fallback).
    pub(crate) label: String,
    /// Field used to resolve the label (`activity_kind`, `kind`, etc.).
    pub(crate) source: String,
    /// Hover/accessibility description retaining the semantic display value.
    pub(crate) title: String,
}

impl RowClassification {
    pub(super) fn new(label: &str, source: &str, wire_value: &str) -> RowClassification {
        let source_title = match source {
            "activity_kind" => "Activity",
            "kind" => "Kind",
            "record_role" => "Record role",
            "is_system" => "System classification",
            "git_oid" => "Git identity",
            _ => "Classification",
        };
        let title = if label == "meta" {
            "Activity: meta".to_owned()
        } else {
            format!("{source_title}: {wire_value}")
        };
        RowClassification {
            label: label.to_owned(),
            source: source.to_owned(),
            title,
        }
    }

    pub(super) fn session_summary() -> RowClassification {
        RowClassification {
            label: "session".to_owned(),
            source: "session_summary".to_owned(),
            title: "Session summary".to_owned(),
        }
    }
}

/// Whether a wire token is useful as a visible classification fallback.
fn classification_token(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed != "unknown").then_some(trimmed)
}

/// Turn a provider-neutral wire token into a compact display fallback.
fn classification_fallback(value: &str) -> String {
    value.trim().replace('_', "-")
}

/// Resolve the stable Activity-column value. Semantic activity wins, except
/// that Git identity is authoritative even for sparse/older rows; then kind,
/// record role, and a final `other` label guarantee a populated real row.
pub(crate) fn row_classification(row: &RowInput) -> RowClassification {
    let kind = row.source.kind.as_str();
    let activity = row.activity_kind.as_str();
    let git_oid = row.source.git_oid.as_deref().unwrap_or_default();
    if !git_oid.is_empty() || kind == "git" {
        return if activity == "source_control" {
            RowClassification::new("git", "activity_kind", "source_control")
        } else if !git_oid.is_empty() {
            RowClassification::new("git", "git_oid", git_oid)
        } else {
            RowClassification::new("git", "kind", "git")
        };
    }

    if let Some(activity_kind) = ActivityKind::from_wire(activity.trim()) {
        let label = if activity_kind == ActivityKind::Conversation
            && matches!(row.source.author.trim(), "human" | "user")
        {
            "user"
        } else {
            activity_kind.text()
        };
        return RowClassification::new(label, "activity_kind", activity.trim());
    }

    if row.source.is_system {
        return RowClassification::new("meta", "is_system", "true");
    }
    if let Some(kind) = classification_token(kind) {
        return RowClassification::new(&classification_fallback(kind), "kind", kind);
    }

    let role = row.record_role.as_str();
    if let Some(role) = classification_token(role) {
        return RowClassification::new(&classification_fallback(role), "record_role", role);
    }

    RowClassification::new("other", "fallback", "other")
}

/// The `OUTCOME_LABELS` whitelist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutcomeKind {
    Success,
    Warning,
    Failure,
    Cancelled,
}

impl OutcomeKind {
    pub(crate) fn from_wire(kind: &str) -> Option<OutcomeKind> {
        match kind {
            "success" => Some(OutcomeKind::Success),
            "warning" => Some(OutcomeKind::Warning),
            "failure" => Some(OutcomeKind::Failure),
            "cancelled" => Some(OutcomeKind::Cancelled),
            _ => None,
        }
    }

    pub(crate) fn class(self) -> &'static str {
        match self {
            OutcomeKind::Success => "outcome-success",
            OutcomeKind::Warning => "outcome-warning",
            OutcomeKind::Failure => "outcome-failure",
            OutcomeKind::Cancelled => "outcome-neutral",
        }
    }

    pub(crate) fn text(self) -> &'static str {
        match self {
            OutcomeKind::Success => "ok",
            OutcomeKind::Warning => "warn",
            OutcomeKind::Failure => "✕",
            OutcomeKind::Cancelled => "cancelled",
        }
    }

    pub(crate) fn aria(self) -> &'static str {
        match self {
            OutcomeKind::Failure => "failed",
            other @ (OutcomeKind::Success | OutcomeKind::Warning | OutcomeKind::Cancelled) => {
                other.text()
            }
        }
    }
}

/// `outcomeBadge` with the frozen visibility switches.
pub(crate) fn outcome_badge(row: &RowInput, options: BadgeOptions) -> Option<ChromeItem> {
    let wire_outcome = row.outcome.as_str();
    if wire_outcome == "success" && !options.show_success_outcome {
        return None;
    }
    let outcome = OutcomeKind::from_wire(wire_outcome)?;
    Some(ChromeItem::new(
        &format!("out-badge {}", outcome.class()),
        outcome.text(),
        &format!("outcome: {wire_outcome}"),
        Some(&format!("outcome: {}", outcome.aria())),
    ))
}

/// `relationBadges` + a consequential outcome badge. Structural relations
/// take over the semantic-tag slot; Activity remains in its own column.
fn relation_chrome(row: &RowInput, options: BadgeOptions) -> Vec<ChromeItem> {
    let mut items = relation_badges(row);
    if !items.is_empty() {
        let outcome = row.outcome.as_str();
        if matches!(outcome, "warning" | "failure" | "cancelled") {
            if let Some(badge) = outcome_badge(row, options) {
                items.push(badge);
            }
        }
    }
    items
}

/// `rowSemanticChrome` — restrained row tags. Activity is rendered in its own
/// grid column and is intentionally absent here.
pub(crate) fn row_semantic_chrome(
    row: &RowInput,
    is_bundle: bool,
    options: BadgeOptions,
) -> Vec<ChromeItem> {
    if is_bundle {
        return bundle_chrome(row, options);
    }
    let relations = relation_chrome(row, options);
    if !relations.is_empty() {
        return relations;
    }
    let mut items = Vec::new();
    if let Some(outcome) = outcome_badge(row, options) {
        items.push(outcome);
    }
    items
}

// --- Activity work-unit / bundle / promotion layer --------------------------

/// The raw `work_unit` payload of one Activity row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct WorkUnitData {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) count: Option<u64>,
    pub(crate) is_start: bool,
    pub(crate) is_end: bool,
}

/// The additive `session_summary` payload present on one Activity row per
/// session group.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SessionSummaryData {
    pub(crate) count: Option<u64>,
}

/// Compact two-line header metadata for a work-unit start row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct WorkUnitHeader {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) show_count: bool,
    pub(crate) count_text: String,
    pub(crate) count_title: String,
    pub(crate) title_only: bool,
}

/// `WORK_UNIT_FALLBACK_LABELS` — short human labels keyed by activity kind.
fn work_unit_fallback(activity_kind: &str) -> Option<&'static str> {
    match activity_kind {
        "work" => Some("Work"),
        "execute" => Some("Run"),
        "change" => Some("Change"),
        "verify" => Some("Verify"),
        "plan" => Some("Plan"),
        "explore" => Some("Explore"),
        "diagnose" => Some("Diagnose"),
        "coordinate" => Some("Coordinate"),
        "source_control" => Some("Git"),
        "external" => Some("External"),
        "system" => Some("Meta"),
        _ => None,
    }
}

/// `workUnitOf` — the row's Activity work-unit payload (never on sub-ops).
pub(crate) fn work_unit_of(row: &RowInput) -> Option<WorkUnitData> {
    if row.source.is_subop {
        return None;
    }
    row.work_unit.clone()
}

/// Read the explicit whole-session marker emitted on the session's true newest
/// visible row. Work-unit identity is intentionally irrelevant: Codex mixes
/// turn-scoped rows with session-scoped lifecycle rows.
pub(crate) fn session_summary_of(row: &RowInput) -> Option<SessionSummaryData> {
    if row.source.is_subop {
        return None;
    }
    row.session_summary.clone()
}

/// `workUnitTitle` — DTO title's first meaningful line, else human fallbacks.
pub(crate) fn work_unit_title(row: &RowInput) -> String {
    if let Some(wu) = work_unit_of(row) {
        if let Some(title) = wu.title {
            let first_line = title
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .split('\n')
                .map(markdown_plain_line)
                .find(|line| !line.is_empty());
            if let Some(first) = first_line {
                return first;
            }
        }
    }
    let activity_kind = row.activity_kind.as_str();
    if let Some(fallback) = work_unit_fallback(activity_kind) {
        return fallback.to_owned();
    }
    let kind = row.source.kind.as_str();
    if kind == "message" || kind == "command" {
        return "Request".to_owned();
    }
    group_label_text(&row.source.group, None, None)
}

/// `workUnitCountText` — human count text for a unit header.
pub(crate) fn work_unit_count_text(count: u64) -> String {
    format!("{count} entr{}", if count == 1 { "y" } else { "ies" })
}

/// `showWorkUnitCount` — keep grouping counts sparse.
pub(crate) fn show_work_unit_count(row: &RowInput, wu: &WorkUnitData) -> bool {
    wu.count.is_some_and(|count| count > 1) && row.activity_kind != "source_control"
}

/// `workUnitCountTitle` — tooltip explaining what the count measures.
pub(crate) fn work_unit_count_title(count: u64) -> String {
    format!("{} grouped in this activity", work_unit_count_text(count))
}

/// Tooltip for the whole-session count shown on a session-summary row.
pub(crate) fn session_summary_count_title(count: u64) -> String {
    format!("{} in this session", work_unit_count_text(count))
}

/// One session-provenance chip (`sessionMetaValues` item).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionChip {
    pub(crate) class: String,
    pub(crate) label: String,
    pub(crate) title: String,
}

/// `sessionMetaValues` — the small, display-safe session provenance.
pub(crate) fn session_meta_values(row: &RowInput) -> Vec<SessionChip> {
    let mut out = Vec::new();
    let Some(meta) = &row.source.session_meta else {
        return out;
    };
    if let Some(provider) = meta.model_provider.as_deref() {
        let trimmed = provider.trim();
        if !trimmed.is_empty() {
            out.push(SessionChip {
                class: "session-chip session-chip-model".to_owned(),
                label: trimmed.to_owned(),
                title: "Model provider".to_owned(),
            });
        }
    }
    if let Some(agent) = meta.agent_nickname.as_deref() {
        let trimmed = agent.trim();
        if !trimmed.is_empty() {
            out.push(SessionChip {
                class: "session-chip session-chip-agent".to_owned(),
                label: trimmed.to_owned(),
                title: "Agent".to_owned(),
            });
        }
    }
    out
}

/// `sessionMetaDescription` — accessible prose for the visible chips.
pub(crate) fn session_meta_description(row: &RowInput) -> String {
    session_meta_values(row)
        .iter()
        .map(|chip| format!("{} {}", chip.title, chip.label))
        .collect::<Vec<String>>()
        .join(", ")
}

/// The typed Activity-bundle kinds the renderer understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BundleKind {
    WorkGroup,
    ExecuteRun,
    PlanRepeat,
}

impl BundleKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            BundleKind::WorkGroup => "work-group",
            BundleKind::ExecuteRun => "execute-run",
            BundleKind::PlanRepeat => "plan-repeat",
        }
    }
}

/// `activityBundleKind` — the recognized typed bundle kind of a row.
pub(crate) fn activity_bundle_kind(row: &RowInput) -> Option<BundleKind> {
    row.bundle_kind
}

/// `isActivityBundle` — any recognized typed bundle row.
#[cfg(test)]
pub(crate) fn is_activity_bundle(row: &RowInput) -> bool {
    activity_bundle_kind(row).is_some()
}

/// `isExecuteRunBundle`.
pub(crate) fn is_execute_run_bundle(row: &RowInput) -> bool {
    activity_bundle_kind(row) == Some(BundleKind::ExecuteRun)
}

/// `isPlanRepeatBundle`.
#[cfg(test)]
pub(crate) fn is_plan_repeat_bundle(row: &RowInput) -> bool {
    activity_bundle_kind(row) == Some(BundleKind::PlanRepeat)
}

/// Typed bundle row metadata (count only from the DTO, never the summary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BundleInfo {
    pub(crate) kind: BundleKind,
    pub(crate) member_count: Option<u64>,
    pub(crate) count_text: String,
    pub(crate) status_success: bool,
}

/// `bundleCountText` — one concise label for a typed bundle row.
pub(crate) fn bundle_count_text(row: &RowInput) -> String {
    let Some(kind) = activity_bundle_kind(row) else {
        return String::new();
    };
    let Some(member_count) = row.bundle_count else {
        return String::new();
    };
    if kind == BundleKind::WorkGroup {
        return format!(
            "{member_count} activit{}",
            if member_count == 1 { "y" } else { "ies" }
        );
    }
    if kind == BundleKind::PlanRepeat {
        return format!(
            "{member_count} update{}",
            if member_count == 1 { "" } else { "s" }
        );
    }
    let command = row.source.kind == "command";
    if command {
        format!(
            "{member_count} command{}",
            if member_count == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "{member_count} tool step{}",
            if member_count == 1 { "" } else { "s" }
        )
    }
}

/// `bundleChrome` — count label plus the execute-specific status glyph.
pub(crate) fn bundle_chrome(row: &RowInput, _options: BadgeOptions) -> Vec<ChromeItem> {
    let count_text = bundle_count_text(row);
    if count_text.is_empty() {
        return Vec::new();
    }
    let success = is_execute_run_bundle(row) && row.outcome == "success";
    let title = if success {
        format!("{count_text}, completed")
    } else {
        count_text.clone()
    };
    let mut items = vec![ChromeItem::new("bundle-count", &count_text, &title, None)];
    if success {
        items.push(ChromeItem::new(
            "bundle-status bundle-status-success",
            "✓",
            "completed",
            Some("completed"),
        ));
    }
    items
}

/// Promotion rails: strong accent or quiet rail, never a badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromotedKind {
    Failure,
    Warning,
    Cancelled,
    Change,
    Verify,
    Rail,
}

impl PromotedKind {
    pub(crate) fn classes(self) -> &'static str {
        match self {
            PromotedKind::Failure => "row-promoted row-promoted-failure",
            PromotedKind::Warning => "row-promoted row-promoted-warning",
            PromotedKind::Cancelled => "row-promoted row-promoted-cancelled",
            PromotedKind::Change => "row-promoted row-promoted-change",
            PromotedKind::Verify => "row-promoted row-promoted-verify",
            PromotedKind::Rail => "row-promoted row-promoted-rail",
        }
    }
}

/// `promotedClasses` for the Activity view (sub-ops are never promoted).
pub(crate) fn promoted_kind(row: &RowInput) -> Option<PromotedKind> {
    let promoted = !row.source.is_subop && row.source.promoted;
    if !promoted {
        return None;
    }
    match row.outcome.as_str() {
        "failure" => Some(PromotedKind::Failure),
        "warning" => Some(PromotedKind::Warning),
        "cancelled" => Some(PromotedKind::Cancelled),
        _ => match row.activity_kind.as_str() {
            "change" => Some(PromotedKind::Change),
            "verify" => Some(PromotedKind::Verify),
            _ => Some(PromotedKind::Rail),
        },
    }
}

/// Resolve the semantic icon used by an ordinary Content row. The explicit
/// activity taxonomy is authoritative; kind/sub-op fallbacks keep older and
/// forward-compatible rows inside the same visual grammar.
pub(super) fn content_icon(row: &RowInput) -> ActivityIcon {
    if let Some(icon) = ActivityIcon::from_label(&row_classification(row).label) {
        return icon;
    }
    match row.source.subop_kind.as_deref().unwrap_or_default() {
        "edit" => return ActivityIcon::Change,
        "msg" => {
            return if matches!(row.source.author.trim(), "human" | "user") {
                ActivityIcon::User
            } else {
                ActivityIcon::Agent
            };
        }
        "tool_result" => return ActivityIcon::Execute,
        "meta" | "" => {}
        _ => return ActivityIcon::System,
    }
    match row.source.kind.as_str() {
        "git" => ActivityIcon::SourceControl,
        "file" => ActivityIcon::Change,
        "message" => {
            if matches!(row.source.author.trim(), "human" | "user") {
                ActivityIcon::User
            } else {
                ActivityIcon::Agent
            }
        }
        "reflection" => ActivityIcon::Plan,
        "command" | "tool" | "tool_result" => ActivityIcon::Execute,
        "error" => ActivityIcon::Diagnose,
        "import" if row.record_role == "artifact" => ActivityIcon::Change,
        "import" if row.record_role == "narrative" => ActivityIcon::Plan,
        _ => ActivityIcon::System,
    }
}

/// Humanize a forward-compatible kind token without inventing provider
/// semantics (`future_kind` -> `Future kind`).
fn humanize_content_kind(value: &str) -> String {
    let words = value
        .trim()
        .split(|c: char| c == '_' || c == '-' || c.is_whitespace())
        .filter(|word| !word.is_empty())
        .collect::<Vec<&str>>()
        .join(" ");
    let mut chars = words.chars();
    let Some(first) = chars.next() else {
        return "Activity".to_owned();
    };
    first.to_uppercase().chain(chars).collect()
}

/// Resolve the optional compact title between Content's icon and authored
/// summary. Commit, message, and aggregate work rows omit the redundant noun;
/// tool wrappers can provide a more useful concrete tool name for less obvious
/// activity.
pub(super) fn content_title(row: &RowInput) -> String {
    let kind = row.source.kind.as_str();
    let role = row.record_role.as_str();
    if kind == "git" || !row.source.git_oid.as_deref().unwrap_or_default().is_empty() {
        return String::new();
    }
    if kind == "tool" {
        if let Some(tool_name) = &row.tool_label {
            return tool_name.clone();
        }
        return if role == "result" {
            "Tool result".to_owned()
        } else {
            "Tool".to_owned()
        };
    }
    match kind {
        "message" | "work-group" => String::new(),
        "command" if role == "result" => "Command output".to_owned(),
        "command" => "Command".to_owned(),
        "tool_result" => "Tool result".to_owned(),
        "file" => "Change".to_owned(),
        "reflection" => "Reflection".to_owned(),
        "error" => "Error".to_owned(),
        "note" => "Note".to_owned(),
        "import" => "Import".to_owned(),
        "token_usage_record" => "Token usage".to_owned(),
        "token_count" => "Token count".to_owned(),
        "world_state" => "World state".to_owned(),
        "turn_context" => "Turn context".to_owned(),
        "task_complete" => "Task complete".to_owned(),
        "session_title" => "Session title".to_owned(),
        "session_meta" => "Session metadata".to_owned(),
        "turn_aborted" => "Turn aborted".to_owned(),
        "execute-run" => "Run".to_owned(),
        "plan-repeat" => "Plan".to_owned(),
        "" => match role {
            "narrative" => String::new(),
            "action" => "Action".to_owned(),
            "result" => "Result".to_owned(),
            "artifact" => "Artifact".to_owned(),
            "lifecycle" => "Metadata".to_owned(),
            "echo" => "Echo".to_owned(),
            _ => "Activity".to_owned(),
        },
        other => humanize_content_kind(other),
    }
}

pub(super) fn content_heading(row: &RowInput) -> ContentHeading {
    ContentHeading {
        icon: content_icon(row),
        title: content_title(row),
    }
}

/// Semantic lead for a structured Content cell. Obvious row kinds may leave
/// `title` empty so the icon leads directly into the subtitle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContentHeading {
    pub(crate) icon: ActivityIcon,
    pub(crate) title: String,
}
