//! Initialize a new edit chain.

use std::path::PathBuf;

use crate::segment::SegmentStore;
use editchain_codec::frame::encode_op;
use editchain_codec::page::Page;
use editchain_core::{clock, op, parents, scope, tags, ActorId, NodeId, Op, OpId};

/// Run the `init` command.
///
/// # Errors
///
/// Returns an error if the chain directory cannot be created or the
/// initial operation cannot be written.
#[expect(
    clippy::needless_pass_by_value,
    clippy::print_stdout,
    reason = "CLI command; path consumed by design"
)]
pub fn run(path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    println!("Initialized editchain at: {}", path.display());
    // Write a ChainStart operation as the first record
    let start_op = Op {
        id: OpId::new(NodeId(0), 0, 0),
        parents: parents::ParentSet::None,
        actor: ActorId(0),
        clock: clock::Clock::None,
        scope: scope::ScopeRef::None,
        tags: tags::Tags::NONE,
        kind: op::OpKind::ChainStart(op::ChainStart {
            name: b"editchain".to_vec(),
            version: 2,
        }),
    };
    let encoded = encode_op(&start_op)?;
    let mut page = Page::new(0);
    page.add_record(0, encoded);
    let mut store = SegmentStore::open(&path)?;
    store.append_page(&page)?;
    println!("Wrote ChainStart operation.");
    Ok(())
}
