//! Walk the full commit history of a repository and resolve each commit.
//!
//! Usage: `cargo run -p editchain-git --example walk_history -- <workspace>`
//!
//! Demonstrates live discovery + resolution against real git history.

#![expect(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::map_unwrap_or,
    clippy::print_stdout,
    clippy::wildcard_enum_match_arm,
    reason = "Example binary; prints progress and uses ergonomic helpers"
)]

// Crate-level dependency markers (used by Cargo for feature resolution).
use gix_object as _;
use sha2 as _;
use tempfile as _;

use std::path::PathBuf;

use editchain_core::{GitObjectFormat, GitOid, Payload};
use editchain_git::{discover_repositories, resolve_commit, RepositoryHandle};

fn main() {
    let workspace = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let discoveries = discover_repositories(&workspace).expect("discover repositories");
    println!(
        "discovered {} repo(s) in {}",
        discoveries.len(),
        workspace.display()
    );
    for d in &discoveries {
        println!(
            "  repo id={} path={} worktree={}",
            d.id.0,
            d.path.display(),
            d.is_worktree
        );
    }

    for discovery in &discoveries {
        // Skip linked worktree pointer files; open the main repo dir.
        let open_path = if discovery.is_worktree {
            // A `.git` file points at the real gitdir; gix can open the parent.
            discovery
                .path
                .parent()
                .unwrap_or(&discovery.path)
                .to_path_buf()
        } else {
            discovery.path.clone()
        };
        let Ok(repo) = gix::open(&open_path) else {
            println!("  (could not open {})", open_path.display());
            continue;
        };
        let handle = RepositoryHandle {
            repo,
            discovery: discovery.clone(),
        };

        let Ok(head) = handle.repo.head() else {
            println!("  (no HEAD)");
            continue;
        };
        let Some(head_id) = head.id() else {
            println!("  (unborn HEAD)");
            continue;
        };

        // Walk all ancestors of HEAD.
        let Ok(walk) = head_id.ancestors().all() else {
            println!("  (could not walk history)");
            continue;
        };

        let mut count = 0usize;
        for info in walk.flatten() {
            let git_oid = gix_oid_to_core(&info.id);
            match resolve_commit(&handle, &git_oid) {
                Ok(res) => {
                    count += 1;
                    let msg = match &res.commit.message {
                        Payload::Inline(b) => String::from_utf8_lossy(b),
                        _ => "<blob>".into(),
                    };
                    let summary = msg.lines().next().unwrap_or("");
                    let author = match &res.commit.author.name {
                        Payload::Inline(b) => String::from_utf8_lossy(b),
                        _ => "<unknown>".into(),
                    };
                    println!(
                        "  {:.7} parents={} author={} msg={}",
                        git_oid.to_hex(),
                        res.commit.parents.len(),
                        author,
                        summary
                    );
                }
                Err(e) => println!("  {:.7} ERROR: {e}", git_oid.to_hex()),
            }
        }
        println!("  resolved {count} commit(s) in repo {}", discovery.id.0);
    }
}

/// Convert a `gix_hash::ObjectId` to an `editchain_core::GitOid`.
#[expect(
    clippy::indexing_slicing,
    clippy::match_same_arms,
    reason = "SHA-1/SHA-256 digests are at most 32 bytes; Kind is non-exhaustive so a wildcard fallback is required"
)]
fn gix_oid_to_core(id: &gix::hash::ObjectId) -> GitOid {
    let format = match id.kind() {
        gix::hash::Kind::Sha1 => GitObjectFormat::Sha1,
        gix::hash::Kind::Sha256 => GitObjectFormat::Sha256,
        _ => GitObjectFormat::Sha256,
    };
    let mut bytes = [0u8; 32];
    bytes[..id.as_bytes().len()].copy_from_slice(id.as_bytes());
    GitOid { format, bytes }
}
