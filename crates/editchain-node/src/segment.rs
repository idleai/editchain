use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap as _;
use dirs as _;
use editchain_core as _;
use editchain_embed as _;
use editchain_import as _;
use editchain_index as _;
use editchain_query as _;
use serde as _;
use serde_json as _;

use editchain_codec::page::{decode_page, encode_page, Page};

/// Directory layout for segment storage.
///
/// ```text
/// .editchain/<chain>/
///   000000.eclog
///   000001.eclog
///   blobs/<content-id>
/// ```
#[derive(Debug)]
pub struct SegmentStore {
    /// Path to the chain directory.
    pub chain_dir: PathBuf,
    /// Next segment sequence number.
    next_seq: u32,
}

impl SegmentStore {
    /// Open or create a chain directory.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the chain directory cannot be created or read.
    pub fn open(chain_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let chain_dir = chain_dir.into();
        fs::create_dir_all(&chain_dir)?;

        // Determine the next segment sequence number.
        let next_seq = find_next_segment(&chain_dir)?;

        Ok(Self {
            chain_dir,
            next_seq,
        })
    }

    /// Append a page of operations to the current segment.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the segment file cannot be opened or written.
    pub fn append_page(&mut self, page: &Page) -> io::Result<()> {
        let path = self.current_segment_path();
        let encoded = encode_page(page);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(&encoded)?;
        Ok(())
    }

    /// Read all pages from all segments in order.
    ///
    /// # Errors
    ///
    /// Returns an IO error if any segment file cannot be read.
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "Segment file reading; offsets bounded by buffer length checks"
    )]
    pub fn read_all(&self) -> io::Result<Vec<Page>> {
        let mut pages = Vec::new();
        let mut seq = 0u32;

        loop {
            let path = self.segment_path(seq);
            if !path.exists() {
                break;
            }
            let bytes = fs::read(&path)?;
            // A segment file may contain multiple concatenated pages.
            let mut offset = 0;
            while offset < bytes.len() {
                if let Some(page) = decode_page(&bytes[offset..]) {
                    let encoded_len = encoded_page_len(&bytes[offset..]);
                    pages.push(page);
                    offset += encoded_len;
                } else {
                    break; // partial trailing page (power-loss)
                }
            }
            seq += 1;
        }

        Ok(pages)
    }

    /// Rotate to a new segment file.
    ///
    /// # Errors
    ///
    /// This operation is infallible but returns `io::Result` for future-proofing.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "next_seq is a bounded counter"
    )]
    pub const fn rotate(&mut self) -> io::Result<()> {
        self.next_seq += 1;
        Ok(())
    }

    /// Path to the current segment file.
    fn current_segment_path(&self) -> PathBuf {
        self.segment_path(self.next_seq)
    }

    fn segment_path(&self, seq: u32) -> PathBuf {
        let filename = format!("{seq:06}.eclog");
        self.chain_dir.join(filename)
    }
}

/// Compute the encoded length of a page from its bytes.
/// Reads the magic + `page_seq` (8 bytes) then scans records.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "Binary page parsing; offsets are bounded by buffer length checks"
)]
fn encoded_page_len(bytes: &[u8]) -> usize {
    if bytes.len() < 8 {
        return bytes.len();
    }
    let mut offset = 8;
    while offset + 4 <= bytes.len() {
        let len =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4])) as usize;
        offset += 4;
        if offset + 1 + len > bytes.len() {
            break;
        }
        offset += 1 + len;
    }
    offset
}

/// Find the next available segment sequence number.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "seq is a bounded u32 counter from file parsing"
)]
fn find_next_segment(dir: &Path) -> io::Result<u32> {
    let mut max_seq = 0u32;

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if let Some(name_str) = name.to_str() {
            if name_str.to_lowercase().ends_with(".eclog") {
                if let Some(seq_str) = name_str.strip_suffix(".eclog") {
                    if let Ok(seq) = seq_str.parse::<u32>() {
                        if seq >= max_seq {
                            max_seq = seq + 1;
                        }
                    }
                }
            }
        }
    }

    Ok(max_seq)
}
