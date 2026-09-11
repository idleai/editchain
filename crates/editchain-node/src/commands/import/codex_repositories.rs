//! Repository-catalog adapter for the source-format importer.

use std::io;
use std::path::Path;

use editchain_core::RepositoryId;
use editchain_git::RepositoryCatalog;
use editchain_import::codex::RepositoryLookup;
use editchain_import::ImportError;

#[derive(Debug)]
pub(super) struct ImportRepositories(RepositoryCatalog);

impl ImportRepositories {
    pub(super) fn discover(workspace: &Path) -> Result<Self, ImportError> {
        match RepositoryCatalog::discover(workspace) {
            Ok(catalog) => Ok(Self(catalog)),
            // Archives may name an unavailable workspace. There is no live
            // repository identity to attach, but raw capture remains valid.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(Self(RepositoryCatalog::default()))
            }
            Err(error) => Err(ImportError::Io(error)),
        }
    }
}

impl RepositoryLookup for ImportRepositories {
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

#[cfg(test)]
mod tests {
    use super::*;
    use editchain_git::{repository_id_from_path, RepositoryDiscovery};

    fn git(root: &Path, arguments: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn catalog_adapter_preserves_nearest_worktree_identity_and_reports_partial_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "--quiet", "repo"]);
        git(root, &["init", "--quiet", "repo-copy"]);
        let repository = root.join("repo");
        git(
            &repository,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        );
        git(
            &repository,
            &["worktree", "add", "--quiet", "-b", "linked", "../linked"],
        );
        git(&repository, &["init", "--quiet", "nested"]);
        std::fs::create_dir_all(repository.join("src/deep")).unwrap();
        let catalog = ImportRepositories::discover(root).unwrap();
        for (cwd, marker) in [
            (repository.join("src/deep"), repository.join(".git")),
            (repository.join("nested"), repository.join("nested/.git")),
            (root.join("repo-copy"), root.join("repo-copy/.git")),
            (root.join("linked"), root.join("linked/.git")),
        ] {
            assert_eq!(
                catalog.repository_for_cwd(&cwd).unwrap(),
                Some(repository_id_from_path(&marker))
            );
        }
        let linked = RepositoryDiscovery::from_path(&root.join("linked")).unwrap();
        assert!(linked.is_linked_worktree());
        assert_ne!(linked.id, repository_id_from_path(&linked.common_dir));
        assert_eq!(catalog.repository_for_cwd(Path::new("repo")).unwrap(), None);
        assert_eq!(catalog.repository_for_cwd(root).unwrap(), None);
        assert_eq!(
            catalog.repository_for_cwd(&root.join("missing")).unwrap(),
            None
        );
        git(&repository, &["init", "--quiet", ".hidden-repository"]);
        assert_eq!(
            catalog
                .repository_for_cwd(&repository.join(".hidden-repository"))
                .unwrap(),
            None,
            "an uncataloged inner repository cannot inherit the outer identity"
        );
        std::fs::create_dir_all(root.join("broken")).unwrap();
        std::fs::write(
            root.join("broken/.git"),
            "gitdir: /unavailable-object-database\n",
        )
        .unwrap();
        let partial = ImportRepositories::discover(root).unwrap();
        assert!(matches!(
            partial.repository_for_cwd(&repository),
            Err(ImportError::OpSink(_))
        ));
    }
}
