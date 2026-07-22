use serde::{Deserialize, Serialize};

use crate::ids::{ChainId, PathId, SessionId, TurnId};

/// Scoping reference for an operation.
///
/// Operations can be scoped to a chain, session, turn, or file.
/// `None` means the operation is global or un-scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ScopeRef {
    /// Global or un-scoped operation.
    #[default]
    None,
    /// Scoped to a chain.
    Chain(ChainId),
    /// Scoped to a session.
    Session(SessionId),
    /// Scoped to a turn.
    Turn(TurnId),
    /// Scoped to a file.
    File(PathId),
}

impl ScopeRef {
    /// Returns a u8 discriminant for quick filtering.
    #[must_use]
    pub const fn discriminant(&self) -> u8 {
        match self {
            Self::None => 0,
            Self::Chain(_) => 1,
            Self::Session(_) => 2,
            Self::Turn(_) => 3,
            Self::File(_) => 4,
        }
    }
}
