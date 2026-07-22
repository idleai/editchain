use serde::{Deserialize, Serialize};

/// Clock value for causal ordering.
///
/// Embedded devices may use Lamport clocks or Unix milliseconds.
/// Hybrid clocks provide sub-millisecond ordering within the same ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum Clock {
    /// No clock information.
    #[default]
    None,
    /// Lamport logical clock.
    Lamport(u64),
    /// Unix milliseconds timestamp.
    UnixMs(u64),
    /// Hybrid clock: Unix ms + monotonic counter for sub-ms ordering.
    Hybrid {
        /// Unix milliseconds.
        ms: u64,
        /// Monotonic counter for sub-ms ordering.
        ctr: u16,
    },
}

impl Clock {
    /// Returns the clock value as a u64 for ordering purposes.
    /// For `None`, returns 0 (always ordered before any real clock).
    #[must_use]
    pub const fn as_u64(&self) -> u64 {
        match self {
            Self::None => 0,
            Self::Lamport(v) | Self::UnixMs(v) => *v,
            Self::Hybrid { ms, .. } => *ms,
        }
    }

    /// Returns the sub-clock discriminator (ctr for Hybrid, 0 otherwise).
    #[must_use]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Only Hybrid has a sub-clock discriminator; all other variants return 0"
    )]
    pub const fn sub(&self) -> u16 {
        match self {
            Self::Hybrid { ctr, .. } => *ctr,
            _ => 0,
        }
    }
}
