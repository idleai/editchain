//! Contracts shared by import writers and history readers.

use editchain_index as _;
use std::error::Error;
use std::fmt::Debug;
use std::fs;

use crc as _;
use editchain_core::{BlobRef, ContentId};
use editchain_store::{BlobPreviewResolution, BlobReader, BlobResolution, BlobStore};
use postcard as _;
use proptest as _;
use serde as _;

type TestResult = Result<(), Box<dyn Error>>;

fn check(condition: bool, message: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn equal<T: PartialEq + Debug>(actual: &T, expected: &T) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("expected {expected:?}, got {actual:?}").into())
    }
}

fn reference(bytes: &[u8]) -> Result<BlobRef, Box<dyn Error>> {
    Ok(BlobRef {
        id: ContentId::Hash256(blake3::hash(bytes).into()),
        len: u32::try_from(bytes.len())?,
    })
}

#[test]
fn reopened_readers_share_writer_addresses_and_preserve_missing_open_state() -> TestResult {
    let temp = tempfile::tempdir()?;
    let chain = temp.path().join("chain");
    let blob_dir = chain.join("blobs");
    let before = BlobReader::open(&chain)?;
    check(!chain.exists(), "opening a reader must not create a chain")?;
    check(
        BlobStore::open_read_only(&blob_dir)?.is_none(),
        "missing blob directory",
    )?;
    let bytes = b"immutable shared blob";
    let blob = reference(bytes)?;
    equal(&before.resolve(&blob), &BlobResolution::Missing)?;
    let mut writer = BlobStore::new(&blob_dir)?;
    writer.write(bytes)?;
    writer.write(bytes)?;
    equal(&writer.len()?, &1)?;
    // Opening before the directory exists preserves that observed availability.
    equal(&before.resolve(&blob), &BlobResolution::Missing)?;
    let reopened = BlobReader::open(&chain)?;
    equal(
        &reopened.resolve(&blob),
        &BlobResolution::Found(bytes.to_vec()),
    )?;
    equal(&reopened.resolve_content(blob.id), &Some(bytes.to_vec()))?;
    // A caller's large limit must not require an equally large allocation.
    equal(
        &reopened.preview(&blob, usize::MAX),
        &BlobPreviewResolution::Found(bytes.to_vec()),
    )?;
    equal(
        &reopened.preview(&blob, 0),
        &BlobPreviewResolution::Found(Vec::new()),
    )?;
    Ok(())
}

#[test]
fn previews_defer_hashing_but_full_reads_and_rewrites_reject_corruption() -> TestResult {
    let temp = tempfile::tempdir()?;
    let mut writer = BlobStore::new(temp.path().join("blobs"))?;
    let original = b"original";
    let corrupted = b"modified";
    let blob = reference(original)?;
    writer.write(original)?;
    let reader = BlobReader::open(temp.path())?;
    let hash = blake3::hash(original);
    let path = writer.path_for(hash.as_bytes());
    fs::write(&path, corrupted)?;
    // Previews only check declared length; full reads also validate BLAKE3.
    equal(
        &reader.preview(&blob, 3),
        &BlobPreviewResolution::Found(b"mod".to_vec()),
    )?;
    equal(&reader.resolve(&blob), &BlobResolution::Corrupt)?;
    equal(&writer.resolve(&blob), &BlobResolution::Corrupt)?;
    equal(&reader.resolve_content(blob.id), &None)?;
    check(
        writer.write(original).is_err(),
        "reject corrupt existing blobs",
    )?;
    equal(&fs::read(&path)?, &corrupted.to_vec())?;
    fs::write(&path, b"short")?;
    equal(&reader.preview(&blob, 3), &BlobPreviewResolution::Corrupt)?;
    equal(&reader.resolve(&blob), &BlobResolution::Corrupt)?;
    Ok(())
}

#[test]
fn unsupported_addresses_and_invalid_blob_directories_stay_distinct() -> TestResult {
    let temp = tempfile::tempdir()?;
    let reader = BlobReader::open(temp.path())?;
    let unsupported = BlobRef {
        id: ContentId::Hash128([0; 16]),
        len: 0,
    };
    equal(&reader.resolve(&unsupported), &BlobResolution::Unresolvable)?;
    equal(
        &reader.preview(&unsupported, 1),
        &BlobPreviewResolution::Unresolvable,
    )?;
    equal(&reader.resolve_content(unsupported.id), &None)?;
    fs::write(temp.path().join("blobs"), b"not a directory")?;
    check(
        BlobReader::open(temp.path()).is_err(),
        "reject non-directory blob paths",
    )?;
    check(
        BlobStore::open_read_only(temp.path().join("blobs")).is_err(),
        "reject non-directory store paths",
    )?;
    Ok(())
}
