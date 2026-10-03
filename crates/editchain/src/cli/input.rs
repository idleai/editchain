//! File and stdin adapters; JSON objects, arrays and JSONL share one decoder.

use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::mpsc,
};

use serde::de::DeserializeOwned;

use super::error::{Failure, Result};

#[derive(Debug, Clone, clap::Args)]
pub(super) struct Input {
    /// Input file, or - for stdin (JSON, an array, or JSONL).
    #[arg(long, default_value = "-")]
    pub input: PathBuf,
}

pub(super) fn reader(path: &Path) -> Result<Box<dyn Read>> {
    if path == Path::new("-") {
        Ok(Box::new(io::stdin()))
    } else {
        Ok(Box::new(File::open(path)?))
    }
}

pub(super) fn bytes(path: &Path) -> Result<Vec<u8>> {
    let maximum = u64::try_from(editchain_sync::MAX_OBJECT_BYTES)
        .map_err(|error| Failure::input(error.to_string()))?;
    let mut bytes = Vec::new();
    let _count = reader(path)?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(Failure::input("input exceeds the 64 MiB object limit"));
    }
    Ok(bytes)
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

pub(super) fn stdin_chunks() -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::sync_channel(2);
    let _reader = std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        loop {
            let mut bytes = vec![0; 64 * 1024];
            let result = input.read(&mut bytes).map(|count| {
                bytes.truncate(count);
                bytes
            });
            let ended = !result.as_ref().is_ok_and(|bytes| !bytes.is_empty());
            if sender.send(result).is_err() || ended {
                break;
            }
        }
    });
    receiver
}
