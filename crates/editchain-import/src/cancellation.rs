//! Cooperative cancellation shared by source capture and helper execution.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ImportError;

/// A cloneable, one-way cancellation signal for one import attempt.
#[derive(Debug, Clone, Default)]
pub struct ImportCancellation(Arc<AtomicBool>);

impl ImportCancellation {
    /// Cancel this import and every clone of the signal.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Check whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Check cancellation before another bounded unit of capture or derivation.
    ///
    /// # Errors
    ///
    /// Returns `Cancelled` with the original source path after cancellation.
    pub fn check(&self, source: &Path) -> Result<(), ImportError> {
        if self.is_cancelled() {
            Err(ImportError::Cancelled {
                path: source.to_path_buf(),
            })
        } else {
            Ok(())
        }
    }
}
