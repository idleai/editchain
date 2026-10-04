//! Engine-specific query abbreviations over shared CLI output.

use super::error::Result;
pub(crate) use editchain_cli_support::output::{Format, Output};
use serde::Serialize;
use serde_json::Value;
use std::io::{self, Write};

pub(crate) fn diagnostic(message: &str) -> io::Result<()> {
    writeln!(io::stderr().lock(), "editchain: {message}")
}

pub(crate) fn emit_query(
    output: &mut Output,
    queries: &editchain_engine::queries::ChainQueries,
    value: &impl Serialize,
) -> Result<()> {
    if output.format() != Format::Human {
        return output.emit(value);
    }
    let mut value = serde_json::to_value(value)?;
    abbreviate(&mut value, "", queries)?;
    output.emit(&value)
}

fn abbreviate(
    value: &mut Value,
    field: &str,
    queries: &editchain_engine::queries::ChainQueries,
) -> Result<()> {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                abbreviate(value, name, queries)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                abbreviate(value, field, queries)?;
            }
        }
        Value::String(text)
            if matches!(
                field,
                "id" | "operation"
                    | "Operation"
                    | "root"
                    | "frontier"
                    | "source"
                    | "imported_record"
                    | "next_after"
                    | "One"
                    | "Two"
                    | "target_ids"
                    | "incarnation"
                    | "outputs"
                    | "representative"
                    | "incomplete_sources"
            ) =>
        {
            if let Some(id) = editchain_engine::OpId::from_display_str(text) {
                *text = queries.index().short_id(id)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}
