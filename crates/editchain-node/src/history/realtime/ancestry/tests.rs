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
        operations: vec![operation(current, current.checked_sub(1).filter(|n| *n > 0)).into()],
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
        branch.operations = vec![operation(5, Some(2)).into()];
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

#[test]
fn sibling_spawns_replace_inherited_git_and_completion_keeps_both_real_parents() {
    use editchain_project::live::topology::{RelationChanges, RelationEdge};
    let mut projection = LiveProjection::default();
    let _changes = projection.apply(
        vec![
            operation(1, None),
            operation(2, Some(1)),
            operation(3, Some(2)),
            operation(4, Some(3)),
            operation(5, Some(4)),
            operation(10, None),
            operation(11, Some(10)),
            operation(12, Some(11)),
            operation(20, None),
            operation(21, Some(20)),
        ],
        &[],
    );
    let mut ancestry = Ancestry::default();
    for input in [
        row("main", 1, 1),
        row("spawn", 2, 2),
        row("next", 3, 3),
        row("wait", 4, 5),
        row("child-a", 11, 12),
        row("child-b", 21, 21),
    ] {
        ancestry.put(&input, &projection);
    }
    for root in [10, 20] {
        let link = GitLink {
            source: id(root),
            target_repo: editchain_core::RepositoryId(1),
            target_oid: editchain_core::GitOid::from_hex(
                "1111111111111111111111111111111111111111",
            )
            .unwrap(),
            kind: GitLinkKind::BasedOn,
        };
        let mut proof = operation(root + 100, None);
        proof.kind = OpKind::GitLink(link);
        ancestry.observe_links(&[proof], &[]);
    }
    let _before = ancestry.changed(&projection);
    let spawns: Vec<_> = [10, 20]
        .map(|root| RelationEdge {
            anchor: id(root),
            target: id(2),
            spawn: true,
        })
        .into();
    ancestry.observe_relationships(RelationChanges {
        added: spawns.clone(),
        removed: Vec::new(),
    });
    let branches = ancestry.changed(&projection);
    assert_parent(&branches, "child-a", "spawn");
    assert_parent(&branches, "child-b", "spawn");
    let complete = RelationEdge {
        anchor: id(5),
        target: id(12),
        spawn: false,
    };
    ancestry.observe_relationships(RelationChanges {
        added: vec![complete],
        removed: Vec::new(),
    });
    let merged = ancestry.changed(&projection);
    assert!(
        merged
            .iter()
            .any(|(key, parents)| key == "wait"
                && parents == &["child-a".to_owned(), "next".to_owned()]),
        "{merged:?}"
    );
    ancestry.observe_relationships(RelationChanges {
        added: Vec::new(),
        removed: vec![complete],
    });
    assert_parent(&ancestry.changed(&projection), "wait", "next");
    ancestry.observe_relationships(RelationChanges {
        added: Vec::new(),
        removed: spawns,
    });
    let retracted = ancestry.changed(&projection);
    assert!(retracted
        .iter()
        .filter(|(key, _)| key.starts_with("child-"))
        .all(|(_, parents)| parents.len() == 1 && parents.first().unwrap().starts_with("git:")));
}
