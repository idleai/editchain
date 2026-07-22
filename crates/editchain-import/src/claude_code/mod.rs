//! Claude Code session import — discover, read, parse, and normalize.

/// Session file discovery in the Claude Code data directory.
pub mod discover;
/// Envelope parsing for Claude Code JSONL records.
pub mod envelope;
/// Normalization of envelopes into editchain operations.
pub mod normalize;
/// Streaming reader for session files.
pub mod reader;
