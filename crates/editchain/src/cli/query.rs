//! Query arguments map directly to the viewer-independent query facade.

use std::path::Path;

use editchain_engine::{
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
    /// Exclusive operation-ID cursor (node:boot:seq).
    #[arg(long, value_parser = input::op_id)]
    after: Option<OpId>,
    /// Maximum candidate records to inspect (1..1000).
    #[arg(long, default_value = "100")]
    limit: usize,
    /// Shared `IndexKey` as JSON, e.g. '{"Actor":7}' or '{"Session":9}'.
    #[arg(long, value_parser = input::json::<IndexKey>)]
    key: Option<IndexKey>,
}

impl Page {
    fn request(&self) -> PageRequest {
        PageRequest {
            after: self.after,
            limit: self.limit,
        }
    }
}

#[derive(Debug, clap::Subcommand)]
pub(super) enum Command {
    /// Read import derivations, logical items, exact copies and incomplete sources.
    ImportState,
    /// Page accepted immutable operations with exact record references.
    History(Page),
    /// Literal, case-sensitive search with explicit unavailable content.
    Search {
        text: String,
        #[command(flatten)]
        page: Page,
    },
    /// Look up an accepted, missing or conflicted operation.
    Operation {
        #[arg(value_parser = input::op_id)]
        id: OpId,
    },
    /// Read all exact byte representations for an identity, including conflicts.
    Variants {
        #[arg(value_parser = input::op_id)]
        id: OpId,
    },
    /// Resolve one recorded field; --raw writes exact bytes.
    Content {
        #[arg(value_parser = input::op_id)]
        id: OpId,
        /// Shared `ContentField` name or JSON, e.g. `MessageContent` or '{"GitLiveRef":0}'.
        #[arg(long, value_parser = field)]
        field: ContentField,
        #[arg(long)]
        raw: bool,
    },
    /// Compare the recorded before/after snapshots of one file revision.
    Diff {
        #[arg(value_parser = input::op_id)]
        id: OpId,
    },
    /// Compare any two fields, supplied as shared `ContentQuery` JSON objects.
    Compare {
        #[arg(long, value_parser = input::json::<ContentQuery>)]
        before: ContentQuery,
        #[arg(long, value_parser = input::json::<ContentQuery>)]
        after: ContentQuery,
    },
    /// Read an operation's metadata: actor/session records, direct parents and relationships.
    Meta {
        #[arg(value_parser = input::op_id)]
        id: OpId,
    },
    /// Walk recorded causal parents, reporting missing/conflicted records and a frontier.
    Ancestors {
        #[arg(value_parser = input::op_id)]
        id: OpId,
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
        .map_err(|error| error.to_string())
}

pub(super) fn run(chain: &Path, command: Command, output: &mut Output) -> Result<()> {
    require_chain(chain)?;
    let mut queries = ChainQueries::open(chain)?;
    let _changes = queries.refresh()?;
    match command {
        Command::ImportState => output.emit(&queries.import_state()?),
        Command::History(page) => output.emit(&queries.history(page.key, page.request())?),
        Command::Search { text, page } => {
            let result = queries.search(&text, page.key, page.request())?;
            output.emit(&result)?;
            if result.unavailable.is_empty() {
                Ok(())
            } else {
                Err(Failure::new(
                    3,
                    "search has unavailable content; inspect unavailable and next_after",
                ))
            }
        }
        Command::Operation { id } => emit_lookup(&queries.operation(id)?, output),
        Command::Variants { id } => {
            let variants = queries.record_variants(id)?;
            output.emit(&variants)?;
            if variants.is_empty() {
                Err(Failure::new(3, "operation not recorded"))
            } else {
                Ok(())
            }
        }
        Command::Content { id, field, raw } => {
            let result = queries.content(ContentQuery {
                operation: id,
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
                output.emit(&result)?;
            }
            content_status(&result)
        }
        Command::Diff { id } => {
            let result = queries.diff(id)?;
            emit_lookup(&result, output)?;
            if let Lookup::Found(diff) = result {
                value_status(&diff.before.value)?;
                value_status(&diff.after.value)?;
            }
            Ok(())
        }
        Command::Compare { before, after } => {
            let result = queries.compare(before, after)?;
            output.emit(&result)?;
            content_status(&result.before)?;
            content_status(&result.after)
        }
        Command::Meta { id } => emit_lookup(&queries.operation_meta(id)?, output),
        Command::Ancestors { id, limit } => output.emit(&queries.ancestors(id, limit)?),
        Command::Relationships { entity, page } => {
            reject_key(&page)?;
            output.emit(&queries.relationships(entity, page.request())?)
        }
        Command::Git { query, page } => {
            reject_key(&page)?;
            output.emit(&queries.git(query, page.request())?)
        }
        Command::Annotations(page) => filtered(
            &queries,
            &page,
            |kind| matches!(kind, OpKind::Note(_)),
            output,
        ),
        Command::Reflections(page) => filtered(
            &queries,
            &page,
            |kind| matches!(kind, OpKind::Reflection(_)),
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
    let mut result = queries.history(page.key, page.request())?;
    result.items.retain(|entry| keep(&entry.operation.kind));
    output.emit(&result)
}

fn reject_key(page: &Page) -> Result<()> {
    if page.key.is_some() {
        Err(Failure::input(
            "--key applies to history, search, annotations and reflections",
        ))
    } else {
        Ok(())
    }
}

fn emit_lookup<T: Serialize>(result: &Lookup<T>, output: &mut Output) -> Result<()> {
    output.emit(result)?;
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
