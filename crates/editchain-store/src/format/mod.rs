//! Operation encoding and supported EC02/EC03 durable formats.
//!
//! Writers and format readers share these explicit entry points. Bounded
//! concatenated-page scanning and its diagnostics remain internal to storage.

mod frame;
mod page;
pub(crate) mod scan;

pub use frame::{
    decode_ec03, decode_op, detect_format, encode_ec03, encode_op, encoded_op_len, Ec03Frame,
    FrameFormat, EC03_FORMAT_VERSION,
};
pub use page::{decode_page, encode_page, Page, PageEncodeError, Record};
pub use scan::MAX_RECORD_BYTES;

#[cfg(test)]
mod tests;
