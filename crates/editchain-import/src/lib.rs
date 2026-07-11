//! Editchain import adapters — Claude Code history parsers.
//!
//! This crate provides deterministic, idempotent import of Claude Code session
//! files into editchain operations. Every physical JSONL line is preserved as
//! a raw `ImportOp`; normalized operations (messages, tools, commands, files)
//! are derived alongside.

pub mod error;
pub mod ids;
pub mod model;
pub mod sink;
pub mod cursor;
pub mod import;

pub mod claude_code;

pub use error::*;
pub use ids::*;
pub use model::*;
pub use sink::*;
pub use cursor::*;