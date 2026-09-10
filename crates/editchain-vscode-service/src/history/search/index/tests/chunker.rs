#![doc = "Validated deterministic chunk ranges. Content policy is tested in service."]

use super::super::{chunk_text, ChunkOptions};
use editchain_core as _;
use tantivy as _;

#[test]
fn chunk_short_text() {
    assert_eq!(
        chunk_text("short", ChunkOptions::default()).collect::<Vec<_>>(),
        vec![0..5]
    );
    assert!(chunk_text("", ChunkOptions::default()).next().is_none());
}

#[test]
fn invalid_chunk_options_are_rejected_before_text_is_chunked() {
    for (window, overlap) in [(0, 0), (0, 1), (1, 1), (1, 2), (768, 768)] {
        assert!(ChunkOptions::new(window, overlap).is_err());
    }
    assert!(ChunkOptions::new(1, 0).is_ok());
}

#[test]
fn returned_utf8_ranges_progress_cover_text_and_respect_window() {
    let text = "abc🦀é漢字 xyz ".repeat(1000);
    for (window, overlap) in [(1, 0), (2, 1), (768, 96), (768, 0)] {
        let options = ChunkOptions::new(window, overlap).unwrap();
        let chunks: Vec<_> = chunk_text(&text, options).collect();
        assert_eq!(chunks.first().unwrap().start, 0);
        assert_eq!(chunks.last().unwrap().end, text.len());
        let mut previous = 0..0;
        for range in &chunks {
            assert!(text.get(range.clone()).is_some());
            assert!(range.start < range.end);
            assert!(
                range.end.saturating_sub(range.start)
                    <= usize::try_from(window).unwrap().saturating_mul(4)
            );
            assert!(range.start <= previous.end, "no skipped text at {range:?}");
            if previous.end != 0 {
                assert!(range.start > previous.start);
            }
            previous = range.clone();
        }
        assert_eq!(chunks, chunk_text(&text, options).collect::<Vec<_>>());
    }
}
