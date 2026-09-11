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
    /// Observed Unix milliseconds, independent of logical ordering.
    ///
    /// Logical counters do not identify wall time. Zero in either wall-clock
    /// variant is the legacy undated marker, so it also returns `None`.
    #[must_use]
    pub const fn observed_unix_ms(&self) -> Option<u64> {
        match self {
            Self::None | Self::Lamport(_) | Self::UnixMs(0) | Self::Hybrid { ms: 0, .. } => None,
            Self::UnixMs(ms) | Self::Hybrid { ms, .. } => Some(*ms),
        }
    }

    /// Numeric ordering value for consumers that deliberately compare clock
    /// domains. This is not a timestamp; use [`Self::observed_unix_ms`] for
    /// observed time. `None` sorts as zero under this numeric policy.
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
