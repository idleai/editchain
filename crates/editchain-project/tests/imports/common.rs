//! Import-to-view fixture helpers.
#[path = "../../../editchain-import/tests/common/mod.rs"]
mod capture;
pub(crate) use capture::*;

use editchain_core::{NoteRelationship, OpId, OpKind};

/// Exact resolved endpoints, coalescing the annotations for each supporting fact.
pub(crate) fn relationship_edges(
    projection: &editchain_project::HistoryProjection,
    relationship: NoteRelationship,
) -> std::collections::BTreeSet<(OpId, OpId)> {
    projection
        .relationship_notes()
        .values()
        .flatten()
        .filter_map(|op| {
            let OpKind::Note(note) = &op.kind else {
                return None;
            };
            (note.relationship == relationship).then_some((op, note))
        })
        .flat_map(|(op, note)| {
            op.parents.iter().flat_map(move |source| {
                note.target_ids.iter().map(move |target| (*source, *target))
            })
        })
        .collect()
}
