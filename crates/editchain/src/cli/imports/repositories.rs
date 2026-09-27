//! Connect the shared Git catalog to the importer's repository lookup interface.

use std::{io, path::Path};

use editchain_engine::RepositoryId;
use editchain_git::RepositoryCatalog;
use editchain_import::{codex::RepositoryLookup, ImportError};

#[derive(Debug)]
pub(super) struct Repositories(RepositoryCatalog);

impl Repositories {
    pub(super) fn discover(root: &Path) -> Result<Self, ImportError> {
        let catalog = match RepositoryCatalog::discover(root) {
            Ok(catalog) => catalog,
            // Archived sources can outlive the machine that recorded them.
            Err(error) if error.kind() == io::ErrorKind::NotFound => RepositoryCatalog::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self(catalog))
    }
}

impl RepositoryLookup for Repositories {
    fn repository_for_cwd(&self, cwd: &Path) -> Result<Option<RepositoryId>, ImportError> {
        if !self.0.is_complete() {
            let issues = self
                .0
                .issues()
                .iter()
                .map(|issue| format!("{}: {}", issue.path.display(), issue.message))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ImportError::OpSink(format!(
                "incomplete repository catalog: {issues}"
            )));
        }
        Ok(self
            .0
            .repository_for_path(cwd)
            .map(|repository| repository.id))
    }
}
