//! Results on stdout, diagnostics on stderr, and explicit stream framing.

use std::io::{self, BufWriter, Write};

use serde::Serialize;
use serde_json::Value;

use super::error::{Failure, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Format {
    Human,
    Json,
    Jsonl,
}

pub(crate) struct Output {
    format: Format,
    writer: BufWriter<io::Stdout>,
    stream: bool,
    first: bool,
}

impl Output {
    pub(crate) fn new(format: Format) -> Self {
        Self {
            format,
            writer: BufWriter::new(io::stdout()),
            stream: false,
            first: true,
        }
    }

    pub(crate) fn begin_stream(&mut self) -> Result<()> {
        self.stream = true;
        if self.format == Format::Json {
            self.writer.write_all(b"[\n").map_err(output_error)?;
        }
        Ok(())
    }

    pub(crate) fn emit(&mut self, value: &impl Serialize) -> Result<()> {
        match self.format {
            Format::Human => {
                human(&mut self.writer, &serde_json::to_value(value)?, 0).map_err(output_error)?;
            }
            Format::Json => {
                if self.stream && !self.first {
                    self.writer.write_all(b",\n").map_err(output_error)?;
                }
                serde_json::to_writer_pretty(&mut self.writer, value).map_err(json_error)?;
                if !self.stream {
                    self.writer.write_all(b"\n").map_err(output_error)?;
                }
            }
            Format::Jsonl => {
                serde_json::to_writer(&mut self.writer, value).map_err(json_error)?;
                self.writer.write_all(b"\n").map_err(output_error)?;
            }
        }
        self.first = false;
        self.writer.flush().map_err(output_error)?;
        Ok(())
    }

    pub(crate) fn emit_query(
        &mut self,
        queries: &editchain_engine::queries::ChainQueries,
        value: &impl Serialize,
    ) -> Result<()> {
        if self.format != Format::Human {
            return self.emit(value);
        }
        let mut value = serde_json::to_value(value)?;
        abbreviate(&mut value, "", queries)?;
        self.emit(&value)
    }

    pub(crate) fn raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes).map_err(output_error)?;
        self.writer.flush().map_err(output_error)?;
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        if self.stream && self.format == Format::Json {
            self.writer.write_all(b"\n]\n").map_err(output_error)?;
        }
        self.writer.flush().map_err(output_error)?;
        Ok(())
    }
}

fn human(writer: &mut impl Write, value: &Value, depth: usize) -> io::Result<()> {
    let indent = "  ".repeat(depth);
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                write!(writer, "{indent}{key}:")?;
                if value.is_object() || value.is_array() {
                    writeln!(writer)?;
                    human(writer, value, depth.saturating_add(1))?;
                } else {
                    writeln!(writer, " {}", scalar(value))?;
                }
            }
        }
        Value::Array(items) => {
            if items.is_empty() {
                writeln!(writer, "{indent}(empty)")?;
            } else if let Some(bytes) = items
                .iter()
                .map(|item| item.as_u64().and_then(|value| u8::try_from(value).ok()))
                .collect::<Option<Vec<_>>>()
            {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    writeln!(writer, "{indent}{text:?}")?;
                } else {
                    writeln!(writer, "{indent}{bytes:02x?}")?;
                }
                return Ok(());
            }
            for item in items {
                human(writer, item, depth)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            writeln!(writer, "{indent}{}", scalar(value))?;
        }
    }
    Ok(())
}

fn scalar(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

pub(crate) fn diagnostic(message: &str) -> io::Result<()> {
    writeln!(io::stderr().lock(), "editchain: {message}")
}

fn output_error(error: io::Error) -> Failure {
    if error.kind() == io::ErrorKind::BrokenPipe {
        Failure::new(0, "result pipe closed")
    } else {
        error.into()
    }
}

fn json_error(error: serde_json::Error) -> Failure {
    if error.io_error_kind() == Some(io::ErrorKind::BrokenPipe) {
        Failure::new(0, "result pipe closed")
    } else {
        error.into()
    }
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
