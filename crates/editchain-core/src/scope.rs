use serde::{Deserialize, Serialize};

use crate::ids::{ChainId, PathId, SessionId, TurnId};

/// Scoping reference for an operation.
///
/// Operations can be scoped to a chain, session, turn, or file.
/// `None` means the operation is global or un-scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeRef {
    None,
    Chain(ChainId),
    Session(SessionId),
    Turn(TurnId),
    File(PathId),
}

impl Default for ScopeRef {
    fn default() -> Self {
        ScopeRef::None
    }
}

impl ScopeRef {
    /// Returns a u8 discriminant for quick filtering.
    pub fn discriminant(&self) -> u8 {
        match self {
            ScopeRef::None => 0,
            ScopeRef::Chain(_) => 1,
            ScopeRef::Session(_) => 2,
            ScopeRef::Turn(_) => 3,
            ScopeRef::File(_) => 4,
        }
    }
}