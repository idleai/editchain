//! Deterministic, bounded session provenance labels for history rows.

use editchain_core::{Op, OpId, OpKind, Payload, ScopeRef};
use editchain_protocol::SessionMetaDto;
use std::collections::HashMap;

/// Collect the bounded session-provenance labels the history UI is allowed to
/// surface. Provider metadata records remain authoritative; this index only
/// avoids making the webview parse provider JSON or repeat large payloads.
#[must_use]
pub(super) fn session_metadata_index(ops: &[Op]) -> HashMap<String, SessionMetaDto> {
    type Rank = (u8, u64, u16, OpId);
    #[derive(Default)]
    struct RankedMetadata {
        metadata: SessionMetaDto,
        title_rank: Option<Rank>,
        model_rank: Option<Rank>,
        agent_rank: Option<Rank>,
    }

    fn merge_field(
        target: &mut Option<String>,
        target_rank: &mut Option<Rank>,
        incoming: Option<String>,
        rank: Rank,
    ) {
        if incoming.is_some() && target_rank.is_none_or(|current| rank >= current) {
            *target = incoming;
            *target_rank = Some(rank);
        }
    }

    let mut by_group = HashMap::<String, RankedMetadata>::new();
    for op in ops {
        let ScopeRef::Session(session_id) = op.scope else {
            continue;
        };
        let Some((found, title_priority)) = session_metadata_from_op(op) else {
            continue;
        };
        let entry = by_group
            .entry(format!("session:{}", session_id.0))
            .or_default();
        let base_rank = (0, op.clock.as_u64(), op.clock.sub(), op.id);
        merge_field(
            &mut entry.metadata.session_title,
            &mut entry.title_rank,
            found.session_title,
            (title_priority, base_rank.1, base_rank.2, base_rank.3),
        );
        merge_field(
            &mut entry.metadata.model_provider,
            &mut entry.model_rank,
            found.model_provider,
            base_rank,
        );
        merge_field(
            &mut entry.metadata.agent_nickname,
            &mut entry.agent_rank,
            found.agent_nickname,
            base_rank,
        );
    }
    by_group
        .into_iter()
        .map(|(group, mut ranked)| {
            if ranked
                .metadata
                .session_title
                .as_deref()
                .is_some_and(|title| {
                    ranked
                        .metadata
                        .agent_nickname
                        .as_deref()
                        .is_some_and(|agent| title.eq_ignore_ascii_case(agent))
                })
            {
                ranked.metadata.agent_nickname = None;
            }
            (group, ranked.metadata)
        })
        .collect()
}

/// Parse one bounded provider metadata import. The returned priority keeps an
/// explicit custom title ahead of an AI-generated fallback regardless of the
/// records' source ordering.
#[must_use]
fn session_metadata_from_op(op: &Op) -> Option<(SessionMetaDto, u8)> {
    let OpKind::Import(import) = &op.kind else {
        return None;
    };
    let Payload::Inline(raw) = &import.raw_ref else {
        return None;
    };
    let value = serde_json::from_slice::<serde_json::Value>(raw).ok()?;
    let record_type = value.get("type").and_then(serde_json::Value::as_str)?;
    let display_field = |source: &serde_json::Value, name: &str| {
        source
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    let (metadata, title_priority) = match record_type {
        "session_meta" => {
            let payload = value.get("payload")?;
            (
                SessionMetaDto {
                    session_title: None,
                    model_provider: display_field(payload, "model_provider"),
                    agent_nickname: display_field(payload, "agent_nickname"),
                },
                0,
            )
        }
        "custom-title" => (
            SessionMetaDto {
                session_title: display_field(&value, "customTitle"),
                ..SessionMetaDto::default()
            },
            2,
        ),
        "ai-title" => (
            SessionMetaDto {
                session_title: display_field(&value, "aiTitle"),
                ..SessionMetaDto::default()
            },
            1,
        ),
        "agent-name" => (
            SessionMetaDto {
                agent_nickname: display_field(&value, "agentName"),
                ..SessionMetaDto::default()
            },
            0,
        ),
        "session_title" => (
            SessionMetaDto {
                session_title: display_field(&value, "title"),
                ..SessionMetaDto::default()
            },
            2,
        ),
        _ => return None,
    };
    (metadata.session_title.is_some()
        || metadata.model_provider.is_some()
        || metadata.agent_nickname.is_some())
    .then_some((metadata, title_priority))
}
