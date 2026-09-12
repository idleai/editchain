use super::*;
use editchain_core::{ActorId, Clock, MessageOp, NodeId, ParentSet, Payload, ScopeRef, Tags};

fn id(seq: u64) -> OpId {
    OpId::new(NodeId(1), 0, seq)
}

fn operation(seq: u64, parent: Option<u64>) -> Op {
    Op {
        id: id(seq),
        parents: parent.map_or(ParentSet::None, |parent| ParentSet::One(id(parent))),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"activity".to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

fn row(key: &str, first: u64, current: u64) -> LiveRow {
    LiveRow {
        task: None,
        key: key.into(),
        anchor: id(current),
        incarnation: id(first),
        operations: vec![operation(
            current,
            current.checked_sub(1).filter(|n| *n > 0),
        )],
    }
}

fn assert_parent(changes: &[(String, Vec<String>)], key: &str, parent: &str) {
    assert!(
        changes
            .iter()
            .any(|(changed, parents)| { changed == key && parents == &[parent.to_owned()] }),
        "{key} must continue through {parent}: {changes:?}"
    );
}

#[test]
fn tool_completion_does_not_fork_the_next_activity_away_from_intervening_commands() {
    for incremental in [false, true] {
        let mut projection = LiveProjection::default();
        let mut ancestry = Ancestry::default();
        let ops = vec![
            operation(1, None),
            operation(2, Some(1)),
            operation(3, Some(2)),
            operation(4, Some(3)),
        ];
        if incremental {
            let _changes = projection.apply(ops.iter().take(2).cloned().collect(), &[]);
            ancestry.put(&row("tool", 1, 1), &projection);
            ancestry.put(&row("command", 2, 2), &projection);
            let _changes = ancestry.changed(&projection);
            let _changes = projection.apply(ops.into_iter().skip(2).collect(), &[]);
        } else {
            let _changes = projection.apply(ops, &[]);
            ancestry.put(&row("command", 2, 2), &projection);
        }
        ancestry.put(&row("tool", 1, 3), &projection);
        ancestry.put(&row("next", 4, 4), &projection);
        assert_parent(&ancestry.changed(&projection), "next", "command");

        // A real second child remains a branch, even within the same session.
        let _changes = projection.apply(vec![operation(5, Some(2))], &[]);
        let mut branch = row("branch", 5, 5);
        branch.operations = vec![operation(5, Some(2))];
        ancestry.put(&branch, &projection);
        assert_parent(&ancestry.changed(&projection), "branch", "command");

        // Hiding a member lifts its continuation through the unchanged source path.
        ancestry.remove("command");
        let changes = ancestry.changed(&projection);
        assert_parent(&changes, "next", "tool");
        assert_parent(&changes, "branch", "tool");
    }
}

#[test]
fn produced_commit_references_the_tool_result_without_changing_chronological_flow() {
    let mut projection = LiveProjection::default();
    let _changes = projection.apply(
        vec![
            operation(1, None),
            operation(2, Some(1)),
            operation(3, Some(2)),
            operation(4, Some(3)),
        ],
        &[],
    );
    let mut ancestry = Ancestry::default();
    ancestry.put(&row("tool", 1, 3), &projection);
    ancestry.put(&row("command", 2, 2), &projection);
    ancestry.put(&row("next", 4, 4), &projection);
    let link = GitLink {
        source: id(3),
        target_repo: editchain_core::RepositoryId(1),
        target_oid: editchain_core::GitOid::from_hex("1111111111111111111111111111111111111111")
            .unwrap(),
        kind: GitLinkKind::ProducedBy,
    };
    let commit = link.target_key().to_string();
    let mut proof = operation(10, None);
    proof.kind = OpKind::GitLink(link);
    ancestry.observe_links(std::slice::from_ref(&proof), &[]);
    let changes = ancestry.changed(&projection);
    assert_parent(&changes, &commit, "tool");
    assert_parent(&changes, "next", "command");

    ancestry.remove("tool");
    assert_parent(&ancestry.changed(&projection), &commit, "command");
    ancestry.put(&row("tool", 1, 3), &projection);
    assert_parent(&ancestry.changed(&projection), &commit, "tool");
    ancestry.observe_links(&[], &[proof.id]);
    assert!(ancestry
        .changed(&projection)
        .iter()
        .any(|(key, parents)| key == &commit && parents.is_empty()));
}
