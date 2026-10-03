//! Cooperative cancellation of bounded command-line operations.

use std::{
    io,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[derive(Debug, Clone, Default)]
pub(super) struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub(super) fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub(super) fn check(&self, source: &Path) -> io::Result<()> {
        if self.is_cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                format!("operation interrupted: {}", source.display()),
            ))
        } else {
            Ok(())
        }
    }
}
