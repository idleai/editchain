//! Serialize the baseline by borrowing its topology, without a JSON value tree.

use super::{LiveWorkspace, Result};
use editchain_protocol::{OpenResponse, SnapshotId};
use serde::{ser::SerializeSeq as _, Serialize, Serializer};

#[derive(Serialize)]
struct Reply<T> {
    id: u64,
    body: Body<T>,
}

#[derive(Serialize)]
enum Body<T> {
    Ok(T),
}

#[derive(Serialize)]
struct Opened<'a> {
    #[serde(flatten)]
    metadata: OpenResponse,
    live: Baseline<'a>,
}

#[derive(Serialize)]
struct Baseline<'a> {
    paged: bool,
    epoch: &'a SnapshotId,
    revision: u64,
    total: u64,
    blocks: Blocks<'a>,
}

struct Blocks<'a>(&'a LiveWorkspace);

impl Serialize for Blocks<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(None)?;
        for (_, block) in self
            .0
            .blocks
            .iter()
            .take(if self.0.paged() { 0 } else { usize::MAX })
        {
            sequence.serialize_element(&block.meta)?;
        }
        sequence.end()
    }
}

impl LiveWorkspace {
    pub(crate) fn encode_opened(&self, id: u64) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&Reply {
            id,
            body: Body::Ok(Opened {
                metadata: self.open_metadata(),
                live: Baseline {
                    paged: self.paged(),
                    epoch: &self.epoch,
                    revision: self.revision,
                    total: self.total(),
                    blocks: Blocks(self),
                },
            }),
        })?)
    }
}
