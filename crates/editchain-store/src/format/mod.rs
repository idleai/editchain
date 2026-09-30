//! Operation encoding and supported EC02/EC03 durable formats.
//!
//! Writers and format readers share these explicit entry points. Bounded
//! concatenated-page scanning and its diagnostics remain internal to storage.

pub(crate) mod ec03;
mod frame;
mod page;
pub(crate) mod scan;

pub use ec03::{
    decode_ec03, encode_ec03, Ec03Frame, FrameError, EC03_FORMAT_VERSION, MAX_FRAME_BYTES,
};
pub use frame::{decode_op, detect_format, encode_op, encoded_op_len, migrate_record, FrameFormat};
pub use page::{decode_page, encode_page, Page, PageEncodeError, Record};
pub use scan::MAX_RECORD_BYTES;

#[cfg(test)]
mod tests;
