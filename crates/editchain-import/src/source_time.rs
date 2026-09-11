//! Source timestamps shared by provider capture and Git reconciliation.

use time::{format_description::well_known::Rfc3339, OffsetDateTime};

/// Parse a complete RFC 3339 timestamp to Unix milliseconds.
///
/// Explicit offsets are applied and fractional seconds are truncated to
/// milliseconds. Missing, malformed, or pre-epoch instants remain unknown.
/// Following `time`'s Unix-time convention, a valid leap-second input maps to
/// the preceding second's final millisecond. Existing stored clocks are not
/// reparsed or rewritten by this adapter.
#[must_use]
pub fn parse_source_time(timestamp: &str) -> Option<u64> {
    let parsed = OffsetDateTime::parse(timestamp, &Rfc3339).ok()?;
    u64::try_from(parsed.unix_timestamp_nanos().div_euclid(1_000_000)).ok()
}
