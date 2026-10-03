//! Query arguments map directly to the viewer-independent query facade.

use std::path::Path;

use editchain_engine::IdQuery;
use editchain_engine::{
    queries::IdResolution,
    queries::{
        ChainQueries, ContentField, ContentQuery, ContentResult, ContentValue, EntityRef, GitQuery,
        IndexKey, Lookup, PageRequest,
    },
    OpId, OpKind,
};
use serde::Serialize;

use super::{
    error::{Failure, Result},
    input,
    output::Output,
    require_chain,
};

#[derive(Debug, clap::Args)]
pub(super) struct Page {
    /// Exclusive full operation ID or an unambiguous hexadecimal prefix.
    #[arg(long)]
    after: Option<IdQuery>,
    /// Maximum candidate records to inspect (1..1000).
    #[arg(long, default_value = "100")]
    limit: usize,
    /// Shared `IndexKey` as JSON, e.g. '{"Actor":7}' or '{"Session":9}'.
    #[arg(long, value_parser = input::json::<IndexKey>, conflicts_with_all = ["item", "session", "turn", "kind"])]
    key: Option<IndexKey>,
    /// Operation type: Session, Turn, Message, Tool, File, Commit, Note, Author, Link, Original.
    #[arg(long, value_parser = kind_name)]
    kind: Option<editchain_engine::activity::KindName>,
    /// Logical item ID or unique prefix; returns every recorded update.
    #[arg(long)]
    item: Option<IdQuery>,
    /// Full logical session ID or unique prefix.
    #[arg(long)]
    session: Option<IdQuery>,
    /// Full logical turn ID or unique prefix.
    #[arg(long)]
    turn: Option<IdQuery>,
}

impl Page {
    fn selected_key(&self, queries: &ChainQueries) -> Result<Option<IndexKey>> {
        if let Some(query) = &self.item {
            return Ok(Some(IndexKey::Item(resolve_item(queries, query)?)));
        }
        if let Some(query) = &self.turn {
            return Ok(Some(IndexKey::TurnItem(resolve_item(queries, query)?)));
        }
        if let Some(query) = &self.session {
            return Ok(Some(IndexKey::SessionItem(resolve_item(queries, query)?)));
        }
        Ok(self.key.or(self.kind.map(IndexKey::Kind)))
    }

    fn keep(&self, queries: &ChainQueries, operation: &editchain_engine::Op) -> Result<bool> {
        let OpKind::Activity(record) = &operation.kind else {
            return Ok(self.kind.is_none()
                && self.item.is_none()
                && self.session.is_none()
                && self.turn.is_none());
        };
        if self.kind.is_some_and(|kind| record.kind.name() != kind) {
            return Ok(false);
        }
        for (query, actual) in [
            (&self.item, Some(record.item)),
            (&self.session, record.session),
            (&self.turn, record.turn),
        ] {
            if let Some(query) = query {
                if actual != Some(resolve_item(queries, query)?) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn request(&self, queries: &ChainQueries) -> Result<PageRequest> {
        Ok(PageRequest {
            after: self
                .after
                .as_ref()
                .map(|query| resolve(queries, query))
                .transpose()?,
            limit: self.limit,
        })
    }
}

fn kind_name(value: &str) -> std::result::Result<editchain_engine::activity::KindName, String> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|error| error.to_string())
}

fn resolve_item(
    queries: &ChainQueries,
    id: &IdQuery,
) -> Result<editchain_engine::activity::ItemId> {
    match queries.resolve_item(id)? {
        IdResolution::Found(id) => Ok(editchain_engine::activity::ItemId(id)),
        IdResolution::Missing => Err(Failure::new(3, "logical item prefix is not recorded")),
        IdResolution::Ambiguous(_) => Err(Failure::input(
            "logical item prefix is ambiguous; use more digits",
        )),
    }
}

#[derive(Debug, clap::Subcommand)]
pub(super) enum Command {
    /// Page accepted immutable operations with exact record references.
    History(Page),
    /// Literal, case-sensitive search with explicit unavailable content.
    Search {
        text: String,
        #[command(flatten)]
        page: Page,
    },
    /// Look up an accepted, missing or conflicted operation.
    Operation { id: IdQuery },
    /// Read all exact byte representations for an identity, including conflicts.
    Variants { id: IdQuery },
    /// Resolve one recorded field; --raw writes exact bytes.
    Content {
        id: IdQuery,
        /// Shared `ContentField` name or JSON, e.g. `MessageContent` or '{"GitLiveRef":0}'.
        #[arg(long, value_parser = field)]
        field: ContentField,
        #[arg(long)]
        raw: bool,
    },
    /// Compare the recorded before/after snapshots of one file revision.
    Diff { id: IdQuery },
    /// Compare any two fields, supplied as shared `ContentQuery` JSON objects.
    Compare {
        #[arg(long, value_parser = input::json::<ContentQuery>)]
        before: ContentQuery,
        #[arg(long, value_parser = input::json::<ContentQuery>)]
        after: ContentQuery,
    },
    /// Read an operation's metadata: actor/session records, direct parents and relationships.
    Meta { id: IdQuery },
    /// Walk recorded causal parents, reporting missing/conflicted records and a frontier.
    Ancestors {
        id: IdQuery,
        #[arg(long, default_value = "100")]
        limit: usize,
    },
    /// Page recorded causal, annotation, session and Git relationships.
    Relationships {
        /// Optional shared `EntityRef` JSON.
        #[arg(long, value_parser = input::json::<EntityRef>)]
        entity: Option<EntityRef>,
        #[command(flatten)]
        page: Page,
    },
    /// Page recorded Git observations using a shared `GitQuery` JSON object.
    Git {
        #[arg(long, value_parser = input::json::<GitQuery>)]
        query: GitQuery,
        #[command(flatten)]
        page: Page,
    },
    /// Page stored annotation operations, without interpreting their meaning.
    Annotations(Page),
    /// Page stored reflection operations, without interpreting their meaning.
    Reflections(Page),
}

fn field(value: &str) -> std::result::Result<ContentField, String> {
    serde_json::from_str(value)
        .or_else(|_| serde_json::from_value(serde_json::Value::String(value.to_owned())))
        .or_else(|_| {
            serde_json::from_str::<editchain_engine::activity::Field>(value)
                .or_else(|_| serde_json::from_value(serde_json::Value::String(value.to_owned())))
                .map(ContentField::Record)
        })
        .map_err(|error| error.to_string())
}

pub(super) fn run(chain: &Path, command: Command, output: &mut Output) -> Result<()> {
    require_chain(chain)?;
    let mut queries = ChainQueries::open(chain)?;
    let _changes = queries.refresh()?;
    match command {
        Command::History(page) => {
            let mut result =
                queries.history(page.selected_key(&queries)?, page.request(&queries)?)?;
            let mut retained = Vec::new();
            for entry in result.items {
                if page.keep(&queries, &entry.operation)? {
                    retained.push(entry);
                }
            }
            result.items = retained;
            output.emit_query(&queries, &result)
        }
        Command::Search { text, page } => {
            let mut result =
                queries.search(&text, page.selected_key(&queries)?, page.request(&queries)?)?;
            let mut hits = Vec::new();
            for hit in result.hits {
                if let Lookup::Found(entry) = queries.operation(hit.record_ref.operation)? {
                    if page.keep(&queries, &entry.operation)? {
                        hits.push(hit);
                    }
                }
            }
            result.hits = hits;
            let mut unavailable = Vec::new();
            for content in result.unavailable {
                if let Lookup::Found(entry) = queries.operation(content.record_ref.operation)? {
                    if page.keep(&queries, &entry.operation)? {
                        unavailable.push(content);
                    }
                }
            }
            result.unavailable = unavailable;
            output.emit_query(&queries, &result)?;
            if result.unavailable.is_empty() {
                Ok(())
            } else {
                Err(Failure::new(
                    3,
                    "search has unavailable content; inspect unavailable and next_after",
                ))
            }
        }
        Command::Operation { id } => emit_lookup(
            &queries.operation(resolve(&queries, &id)?)?,
            output,
            &queries,
        ),
        Command::Variants { id } => {
            let variants = queries.record_variants(resolve(&queries, &id)?)?;
            output.emit_query(&queries, &variants)?;
            if variants.is_empty() {
                Err(Failure::new(3, "operation not recorded"))
            } else {
                Ok(())
            }
        }
        Command::Content { id, field, raw } => {
            let result = queries.content(ContentQuery {
                operation: resolve(&queries, &id)?,
                field,
            })?;
            if raw {
                content_status(&result)?;
                if let Lookup::Found(content) = &result {
                    if let ContentValue::Available(bytes) = &content.value {
                        output.raw(bytes)?;
                    }
                }
            } else {
                output.emit_query(&queries, &result)?;
            }
            content_status(&result)
        }
        Command::Diff { id } => {
            let result = queries.diff(resolve(&queries, &id)?)?;
            emit_lookup(&result, output, &queries)?;
            if let Lookup::Found(diff) = result {
                value_status(&diff.before.value)?;
                value_status(&diff.after.value)?;
            }
            Ok(())
        }
        Command::Compare { before, after } => {
            let result = queries.compare(before, after)?;
            output.emit_query(&queries, &result)?;
            content_status(&result.before)?;
            content_status(&result.after)
        }
        Command::Meta { id } => emit_lookup(
            &queries.operation_meta(resolve(&queries, &id)?)?,
            output,
            &queries,
        ),
        Command::Ancestors { id, limit } => output.emit_query(
            &queries,
            &queries.ancestors(resolve(&queries, &id)?, limit)?,
        ),
        Command::Relationships { entity, page } => {
            reject_key(&page)?;
            output.emit_query(
                &queries,
                &queries.relationships(entity, page.request(&queries)?)?,
            )
        }
        Command::Git { query, page } => {
            reject_key(&page)?;
            output.emit_query(&queries, &queries.git(query, page.request(&queries)?)?)
        }
        Command::Annotations(page) => filtered(
            &queries,
            &page,
            |kind| {
                matches!(kind, OpKind::Note(_))
                    || matches!(kind, OpKind::Activity(record) if matches!(record.kind, editchain_engine::activity::Kind::Note(_)))
            },
            output,
        ),
        Command::Reflections(page) => filtered(
            &queries,
            &page,
            |kind| {
                matches!(kind, OpKind::Reflection(_))
                    || matches!(kind, OpKind::Activity(record) if matches!(&record.kind, editchain_engine::activity::Kind::Message(message) if message.category == editchain_engine::activity::MessageKind::Summary))
            },
            output,
        ),
    }
}

fn filtered(
    queries: &ChainQueries,
    page: &Page,
    keep: impl Fn(&OpKind) -> bool,
    output: &mut Output,
) -> Result<()> {
    let mut result = queries.history(page.selected_key(queries)?, page.request(queries)?)?;
    let mut items = Vec::new();
    for entry in result.items {
        if keep(&entry.operation.kind) && page.keep(queries, &entry.operation)? {
            items.push(entry);
        }
    }
    result.items = items;
    output.emit_query(queries, &result)
}

fn reject_key(page: &Page) -> Result<()> {
    if page.key.is_some()
        || page.kind.is_some()
        || page.item.is_some()
        || page.session.is_some()
        || page.turn.is_some()
    {
        Err(Failure::input(
            "record filters apply to history, search, annotations and reflections",
        ))
    } else {
        Ok(())
    }
}

fn emit_lookup<T: Serialize>(
    result: &Lookup<T>,
    output: &mut Output,
    queries: &ChainQueries,
) -> Result<()> {
    output.emit_query(queries, result)?;
    lookup_status(result)
}

fn lookup_status<T>(result: &Lookup<T>) -> Result<()> {
    match result {
        Lookup::Found(_) => Ok(()),
        Lookup::Missing => Err(Failure::new(3, "operation not recorded")),
        Lookup::Conflicted(_) => Err(Failure::new(4, "operation is conflicted; inspect variants")),
    }
}

fn content_status(result: &Lookup<ContentResult>) -> Result<()> {
    lookup_status(result)?;
    if let Lookup::Found(content) = result {
        value_status(&content.value)?;
    }
    Ok(())
}

fn value_status(value: &ContentValue) -> Result<()> {
    match value {
        ContentValue::Available(_) => Ok(()),
        ContentValue::Corrupt => Err(Failure::new(4, "content is corrupt")),
        ContentValue::Missing | ContentValue::NotRecorded | ContentValue::Unresolvable => Err(
            Failure::new(3, "content is unavailable; inspect the result status"),
        ),
    }
}

fn resolve(queries: &ChainQueries, query: &IdQuery) -> Result<OpId> {
    match queries.resolve_id(query)? {
        IdResolution::Found(id) => Ok(id),
        IdResolution::Missing => Err(Failure::new(3, "operation prefix not recorded")),
        IdResolution::Ambiguous(ids) => Err(Failure::input(format!(
            "ambiguous operation prefix; use more digits (matches include {})",
            ids.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}
