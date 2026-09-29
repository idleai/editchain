//! Contracts shared by import writers and history readers.

use editchain_index_pages as _;
use std::error::Error;
use std::fmt::Debug;
use std::fs;

use crc as _;
use editchain_core::{BlobRef, ContentId};
use editchain_store::{BlobPreviewResolution, BlobReader, BlobResolution, BlobStorage, BlobStore};
use postcard as _;
use proptest as _;
use serde as _;
use serde_json as _;

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
fn readers_observe_late_blobs_without_changing_the_reference() -> TestResult {
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
    // A long-lived reader sees blobs even if its directory was initially absent.
    equal(
        &before.resolve(&blob),
        &BlobResolution::Found(bytes.to_vec()),
    )?;
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

#[test]
fn concurrent_blob_writers_ignore_interrupted_temporary_files() -> TestResult {
    let temp = tempfile::tempdir()?;
    let directory = temp.path().join("nested/chain/blobs");
    let writer = BlobStore::new(&directory)?;
    let bytes = b"identical writers publish one complete binary blob\0\xff";
    let blob = reference(bytes)?;
    let abandoned = directory.join("orphan.tmp.1234");
    fs::write(&abandoned, bytes.get(..7).ok_or("prefix")?)?;
    equal(&writer.resolve(&blob), &BlobResolution::Missing)?;
    equal(&writer.len()?, &0)?;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers = (0..8)
        .map(|_| {
            let mut writer = writer.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                let _wait = barrier.wait();
                writer.write(bytes)
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().map_err(|_panic| "blob worker panicked")??;
    }
    equal(&writer.len()?, &1)?;
    equal(
        &writer.resolve(&blob),
        &BlobResolution::Found(bytes.to_vec()),
    )?;
    equal(
        &fs::read(abandoned)?,
        &bytes.get(..7).ok_or("prefix")?.to_vec(),
    )?;
    Ok(())
}

#[test]
fn blob_batches_preserve_order_retries_and_concurrent_publication() -> TestResult {
    let temp = tempfile::tempdir()?;
    let mut writer = BlobStore::new(temp.path().join("blobs"))?;
    let values: Vec<_> = (0..150)
        .map(|n| format!("exact blob {n}\0").into_bytes())
        .collect();
    let mut payloads: Vec<_> = values.iter().map(Vec::as_slice).collect();
    payloads.push(values.first().ok_or("fixture")?);
    let expected = payloads
        .iter()
        .map(|bytes| reference(bytes))
        .collect::<Result<Vec<_>, _>>()?;
    equal(&writer.put_batch(&[])?, &Vec::new())?;
    equal(&writer.put_batch(&payloads)?, &expected)?;
    equal(&writer.put_batch(&payloads)?, &expected)?;
    equal(&writer.len()?, &150)?;
    let reader = BlobReader::open(temp.path())?;
    for (reference, bytes) in expected.iter().zip(&payloads) {
        equal(
            &reader.resolve(reference),
            &BlobResolution::Found(bytes.to_vec()),
        )?;
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let mut writer = writer.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                let _wait = barrier.wait();
                writer.put_batch(&[
                    b"concurrent batch\xff",
                    b"concurrent batch\xff",
                    b"another batch blob",
                ])
            })
        })
        .collect();
    for worker in workers {
        let references = worker.join().map_err(|_panic| "batch worker panicked")??;
        equal(&references.len(), &3)?;
    }
    equal(&writer.len()?, &152)?;
    Ok(())
}

#[test]
fn failed_blob_batch_keeps_conflicting_bytes_and_replays_its_committed_prefix() -> TestResult {
    let temp = tempfile::tempdir()?;
    let mut writer = BlobStore::new(temp.path().join("blobs"))?;
    let values: Vec<_> = (0..80).map(|n| format!("batch {n}").into_bytes()).collect();
    let payloads: Vec<_> = values.iter().map(Vec::as_slice).collect();
    let broken = payloads.get(70).ok_or("fixture")?;
    let path = writer.path_for(blake3::hash(broken).as_bytes());
    fs::write(&path, b"conflicting bytes")?;
    check(
        writer.put_batch(&payloads).is_err(),
        "a later cohort cannot overwrite conflicting content",
    )?;
    equal(&fs::read(&path)?, &b"conflicting bytes".to_vec())?;
    let first = payloads.first().ok_or("fixture")?;
    equal(
        &writer.resolve(&reference(first)?),
        &BlobResolution::Found(first.to_vec()),
    )?;
    fs::remove_file(path)?;
    let references = writer.put_batch(&payloads)?;
    equal(&references.len(), &80)?;
    equal(&writer.len()?, &80)?;
    check(
        fs::read_dir(writer.dir())?.all(|entry| {
            entry.is_ok_and(|entry| !entry.file_name().to_string_lossy().contains(".tmp."))
        }),
        "failed and successful cohorts clean their unaddressed temporary files",
    )?;
    Ok(())
}
