use serde::{Deserialize, Serialize};

/// Clock value for causal ordering.
///
/// Embedded devices may use Lamport clocks or Unix milliseconds.
/// Hybrid clocks provide sub-millisecond ordering within the same ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Clock {
    /// No clock information.
    None,
    /// Lamport logical clock.
    Lamport(u64),
    /// Unix milliseconds timestamp.
    UnixMs(u64),
    /// Hybrid clock: Unix ms + monotonic counter for sub-ms ordering.
    Hybrid { ms: u64, ctr: u16 },
}

impl Default for Clock {
    fn default() -> Self {
        Clock::None
    }
}

impl Clock {
    /// Returns the clock value as a u64 for ordering purposes.
    /// For `None`, returns 0 (always ordered before any real clock).
    pub fn as_u64(&self) -> u64 {
        match self {
            Clock::None => 0,
            Clock::Lamport(v) => *v,
            Clock::UnixMs(v) => *v,
            Clock::Hybrid { ms, .. } => *ms,
        }
    }

    /// Returns the sub-clock discriminator (ctr for Hybrid, 0 otherwise).
    pub fn sub(&self) -> u16 {
        match self {
            Clock::Hybrid { ctr, .. } => *ctr,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_ordering() {
        let a = Clock::UnixMs(100);
        let b = Clock::UnixMs(200);
        assert!(a < b);

        let c = Clock::Hybrid { ms: 100, ctr: 0 };
        let d = Clock::Hybrid { ms: 100, ctr: 1 };
        assert!(c < d);
    }
}