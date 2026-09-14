//! Unsigned local attribution and admission-time ordering across recorder sessions.

use editchain_core::{human::HumanIdentity, ActorId, Op, OpId, ScopeRef};
use editchain_protocol::editor::EditorEvent;
use editchain_store::{format::encode_op, BlobStore, IndexedChain};
use std::collections::BTreeMap;

pub(super) fn actor(identity: &HumanIdentity) -> ActorId {
    ActorId(editchain_import::derive_node_id(&format!("human:unsigned:{}", identity.guid)).0)
}

pub(super) fn scope(identity: &HumanIdentity) -> ScopeRef {
    ScopeRef::Session(editchain_import::derive_session_id(&format!(
        "vscode.human:{}:{}",
        identity.guid, identity.stream
    )))
}

/// Repair acknowledged payloads before bootstrapping their ordered derivations.
pub(super) fn repair(
    events: &[EditorEvent],
    chain: &IndexedChain,
    blobs: &mut BlobStore,
) -> super::Result<()> {
    for event in events {
        // New observations have nothing to repair. Avoid encoding and hashing
        // their full snapshots a second time just to look up a recorder ID.
        if let Some(retained) = chain.get(super::event_id(event)?) {
            let raw =
                serde_json::to_vec(&serde_json::json!({"source":"vscode.editor", "event":event}))?;
            let mut expected = super::event_op(event, &raw)?;
            if event.identity.is_some() {
                expected.parents = retained.parents.clone();
            }
            if encode_op(&expected)? != encode_op(retained)? {
                return Err("editor identity reused with different content".into());
            }
            blobs.write(&raw)?;
        }
    }
    Ok(())
}

/// Recorder-local sequence is independent of the shared identity's parent path.
pub(super) fn validate_sequence(
    event: &EditorEvent,
    op: &Op,
    chain: &IndexedChain,
    staged: &BTreeMap<OpId, Op>,
) -> super::Result<()> {
    if event.sequence == 1 {
        return Ok(());
    }
    let previous = OpId {
        seq: event.sequence.saturating_sub(1),
        ..op.id
    };
    let previous = staged
        .get(&previous)
        .or_else(|| chain.get(previous))
        .ok_or("editor stream has a sequence gap; replay the pending outbox first")?;
    if previous.actor != op.actor || previous.scope != op.scope {
        return Err("editor identity cannot change within a recorder session".into());
    }
    Ok(())
}
