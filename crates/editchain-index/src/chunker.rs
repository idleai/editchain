//! Deterministic overlapping UTF-8 text ranges, independent of source identity.

use std::io;
use std::ops::Range;

/// Validated chunk sizes, using the existing estimate of four bytes per token.
#[derive(Debug, Clone, Copy)]
pub struct ChunkOptions {
    window_bytes: usize,
    stride_bytes: usize,
}

impl ChunkOptions {
    /// Require a positive window and an overlap strictly smaller than it.
    ///
    /// # Errors
    ///
    /// Rejects invalid or unrepresentable sizes before chunking any text.
    pub fn new(window_tokens: u32, overlap_tokens: u32) -> Result<Self, io::Error> {
        if window_tokens == 0 || overlap_tokens >= window_tokens {
            return Err(io::Error::other(
                "chunk overlap must be smaller than a positive window",
            ));
        }
        let window_bytes = usize::try_from(window_tokens)
            .ok()
            .and_then(|value| value.checked_mul(4))
            .ok_or_else(|| io::Error::other("chunk window is too large"))?;
        let stride_bytes = window_tokens
            .checked_sub(overlap_tokens)
            .and_then(|value| usize::try_from(value).ok())
            .and_then(|value| value.checked_mul(4))
            .ok_or_else(|| io::Error::other("chunk stride is too large"))?;
        Ok(Self {
            window_bytes,
            stride_bytes,
        })
    }
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            window_bytes: 3072,
            stride_bytes: 2688,
        }
    }
}

/// Borrowed text ranges whose endpoints are already valid UTF-8 boundaries.
#[derive(Debug)]
pub struct TextChunks<'a> {
    text: &'a str,
    options: ChunkOptions,
    start: usize,
}

/// Chunk text lazily without allocating chunk records or source identifiers.
#[must_use]
pub const fn chunk_text(text: &str, options: ChunkOptions) -> TextChunks<'_> {
    TextChunks {
        text,
        options,
        start: 0,
    }
}

impl Iterator for TextChunks<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start >= self.text.len() {
            return None;
        }
        let start = self.start;
        let end = self.text.floor_char_boundary(
            start
                .saturating_add(self.options.window_bytes)
                .min(self.text.len()),
        );
        self.start = if end == self.text.len() {
            end
        } else {
            self.text.floor_char_boundary(
                start
                    .saturating_add(self.options.stride_bytes)
                    .min(self.text.len()),
            )
        };
        Some(start..end)
    }
}
