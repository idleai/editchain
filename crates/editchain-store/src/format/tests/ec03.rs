//! Fixed wire fixtures and adversarial, valid-header EC03 inputs.

use crate::format::ec03::checksum;
use crate::format::scan::{PageScanner, ScanItem};
use crate::format::{decode_ec03, encode_ec03, Ec03Frame, FrameError, MAX_FRAME_BYTES};

const GOLDEN: &[u8] = include_bytes!("fixtures/ec03-v2.bin");

fn header_crc(bytes: &mut [u8]) -> Result<(), Box<dyn std::error::Error>> {
    let crc = checksum(bytes.get(..28).ok_or("header")?);
    bytes
        .get_mut(28..32)
        .ok_or("checksum")?
        .copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

fn frame_crc(bytes: &mut [u8]) -> Result<(), Box<dyn std::error::Error>> {
    let end = bytes.len().checked_sub(4).ok_or("frame")?;
    let crc = checksum(bytes.get(..end).ok_or("frame bytes")?);
    bytes
        .get_mut(end..)
        .ok_or("frame checksum")?
        .copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "test assertions report mismatches while fixture I/O errors propagate"
)]
fn fixed_wire_and_every_interruption_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = Ec03Frame::new(42);
    frame.add_record(0xa5, b"abc".to_vec());
    assert_eq!(encode_ec03(&frame)?, GOLDEN);
    assert_eq!(decode_ec03(GOLDEN)?, frame);
    for cut in 0..GOLDEN.len() {
        let prefix = GOLDEN.get(..cut).ok_or("prefix")?;
        assert_eq!(decode_ec03(prefix), Err(FrameError::Incomplete));
        assert!(
            !PageScanner::new(prefix).any(|item| matches!(item, Ok(ScanItem::Record(_)))),
            "uncommitted records at byte {cut}"
        );
    }
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "test assertions report mismatches while fixture I/O errors propagate"
)]
fn valid_headers_cannot_bypass_versions_lengths_counts_or_checksums(
) -> Result<(), Box<dyn std::error::Error>> {
    for (offset, replacement, expected) in [
        (
            4_usize,
            99_u32.to_le_bytes().to_vec(),
            FrameError::Unsupported,
        ),
        (6, 0_u16.to_le_bytes().to_vec(), FrameError::Unsupported),
        (8, 0_u32.to_le_bytes().to_vec(), FrameError::Length),
        (
            8,
            MAX_FRAME_BYTES.saturating_add(1).to_le_bytes().to_vec(),
            FrameError::Length,
        ),
        (12, u32::MAX.to_le_bytes().to_vec(), FrameError::Length),
        (24, 1_u32.to_le_bytes().to_vec(), FrameError::Unsupported),
    ] {
        let mut bytes = GOLDEN.to_vec();
        bytes
            .get_mut(offset..offset.saturating_add(replacement.len()))
            .ok_or("field")?
            .copy_from_slice(&replacement);
        header_crc(&mut bytes)?;
        frame_crc(&mut bytes)?;
        assert_eq!(decode_ec03(&bytes), Err(expected));
    }
    let mut corrupt = GOLDEN.to_vec();
    *corrupt.get_mut(37).ok_or("payload")? ^= 1;
    frame_crc(&mut corrupt)?;
    assert_eq!(
        decode_ec03(&corrupt),
        Err(FrameError::Checksum),
        "per-record checksum survives a forged frame checksum"
    );
    let mut trailing = GOLDEN.to_vec();
    trailing.insert(trailing.len().saturating_sub(4), 0);
    let length = u32::try_from(trailing.len())?;
    trailing
        .get_mut(8..12)
        .ok_or("length")?
        .copy_from_slice(&length.to_le_bytes());
    header_crc(&mut trailing)?;
    frame_crc(&mut trailing)?;
    assert_eq!(
        decode_ec03(&trailing),
        Err(FrameError::Length),
        "all declared payload bytes must be consumed"
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "test assertions report mismatches while fixture I/O errors propagate"
)]
fn a_damaged_frame_never_exposes_a_valid_prefix_record() -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = Ec03Frame::new(0);
    frame.add_record(0, b"valid first record".to_vec());
    frame.add_record(0, b"damaged second record".to_vec());
    let mut bytes = encode_ec03(&frame)?;
    *bytes.last_mut().ok_or("frame checksum")? ^= 1;
    assert!(!PageScanner::new(&bytes).any(|item| matches!(item, Ok(ScanItem::Record(_)))));
    Ok(())
}
