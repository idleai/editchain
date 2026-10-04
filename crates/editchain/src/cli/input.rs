//! Engine record decoders over shared bounded CLI inputs.

use super::error::Result;
pub(super) use editchain_cli_support::input::{bytes, reader, stdin_chunks};
use serde::de::DeserializeOwned;
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, clap::Args)]
pub(super) struct Input {
    /// Input file, or - for stdin (JSON, an array, or JSONL).
    #[arg(long, default_value = "-")]
    pub input: PathBuf,
}

pub(super) fn records<T: DeserializeOwned>(
    path: &Path,
    mut consume: impl FnMut(T) -> Result<()>,
) -> Result<()> {
    let input = io::BufReader::new(reader(path)?);
    for value in serde_json::Deserializer::from_reader(input).into_iter::<serde_json::Value>() {
        let value = value?;
        if let serde_json::Value::Array(values) = value {
            for value in values {
                consume(serde_json::from_value(value)?)?;
            }
        } else {
            consume(serde_json::from_value(value)?)?;
        }
    }
    Ok(())
}

pub(super) fn json<T: DeserializeOwned>(value: &str) -> std::result::Result<T, String> {
    serde_json::from_str(value).map_err(|error| error.to_string())
}
