//! Select provider-owned files while preserving the original cursor roots.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use editchain_import::{
    codex::{CodexDiscoveryRequest, HelperCommand},
    discover_import_files,
    human::HumanImportRequest,
    DiscoveryRequest, ImportFile, ImportOptions, ImportSource,
};
use glob::{MatchOptions, Pattern};
use serde::Deserialize;

use super::super::{repositories::Repositories, Args, Failure, Provider, Result};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    sources: Vec<Specification>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Specification {
    provider: Provider,
    input: PathBuf,
    workspace: Option<String>,
    recorded_root: Option<String>,
    #[serde(default)]
    glob: Vec<String>,
    #[serde(default)]
    paths: Vec<PathBuf>,
}

enum Request {
    Claude(DiscoveryRequest),
    Codex {
        repositories: Repositories,
        paths: Vec<PathBuf>,
    },
    Human(HumanImportRequest),
}

pub(super) struct Prepared {
    pub(super) provider: Provider,
    pub(super) root: PathBuf,
    pub(super) workspace: String,
    pub(super) files: Vec<ImportFile>,
    request: Request,
}

impl Prepared {
    pub(super) fn with_source<T>(
        &self,
        helper: &HelperCommand,
        run: impl FnOnce(ImportSource<'_>) -> Result<T>,
    ) -> Result<T> {
        match &self.request {
            Request::Claude(request) => run(ImportSource::Claude(request)),
            Request::Human(request) => run(ImportSource::Human(request)),
            Request::Codex {
                repositories,
                paths,
            } => {
                let request = CodexDiscoveryRequest {
                    workspace_path: self.workspace.clone().into(),
                    raw_root: self.root.clone(),
                    selected_paths: paths.clone(),
                    repositories,
                };
                run(ImportSource::Codex {
                    request: &request,
                    helper,
                })
            }
        }
    }
}

pub(super) fn helper(args: &Args) -> HelperCommand {
    HelperCommand::new(
        args.codex_helper
            .clone()
            .unwrap_or_else(|| "codex-session-exporter".into()),
        args.codex_helper_arg.clone(),
    )
}

pub(super) fn prepare(chain: &Path, args: &Args, options: &ImportOptions) -> Result<Vec<Prepared>> {
    let specifications = if let Some(path) = &args.selection.manifest {
        options.cancellation.check(path)?;
        let mut manifest: Manifest = serde_json::from_reader(std::fs::File::open(path)?)?;
        if manifest.schema != 1 || manifest.sources.is_empty() {
            return Err(Failure::input(
                "bulk manifest requires schema 1 and at least one source",
            ));
        }
        for source in &mut manifest.sources {
            if source.input.is_relative() {
                source.input = path.parent().unwrap_or(Path::new(".")).join(&source.input);
            }
        }
        manifest.sources
    } else {
        vec![Specification {
            provider: args.provider,
            input: args
                .input
                .clone()
                .ok_or_else(|| Failure::input("bulk import requires --input"))?,
            workspace: Some(args.workspace.clone()),
            recorded_root: args.recorded_root.clone(),
            glob: args.selection.glob.clone(),
            paths: args.codex_rollout.clone(),
        }]
    };
    specifications
        .into_iter()
        .map(|specification| prepare_source(chain, args, options, specification))
        .collect()
}

fn prepare_source(
    chain: &Path,
    args: &Args,
    options: &ImportOptions,
    spec: Specification,
) -> Result<Prepared> {
    if !spec.input.is_dir() {
        return Err(Failure::input(format!(
            "bulk input must be a directory: {}",
            spec.input.display()
        )));
    }
    if spec.recorded_root.is_some() && spec.provider != Provider::Human {
        return Err(Failure::input("recorded_root requires the human provider"));
    }
    let root = std::fs::canonicalize(&spec.input)?;
    let workspace = spec.workspace.unwrap_or_else(|| args.workspace.clone());
    let patterns = spec
        .glob
        .iter()
        .map(|pattern| Pattern::new(pattern).map_err(|error| Failure::input(error.to_string())))
        .collect::<Result<Vec<_>>>()?;
    let paths = spec
        .paths
        .iter()
        .map(|path| {
            let selected = if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            };
            let selected = std::fs::canonicalize(selected)?;
            if !selected.starts_with(&root) {
                return Err(Failure::input(
                    "selected bulk file is outside its source root",
                ));
            }
            Ok(selected)
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let request = match spec.provider {
        Provider::Claude => Request::Claude(DiscoveryRequest {
            workspace_path: workspace.clone().into(),
            sessions_dir: root.clone(),
            chain_dir: chain.to_owned(),
        }),
        Provider::Codex => Request::Codex {
            repositories: Repositories::discover(Path::new(&workspace))?,
            paths: paths.iter().cloned().collect(),
        },
        Provider::Human => Request::Human(HumanImportRequest {
            source: root.clone(),
            recorded_root: spec.recorded_root,
        }),
    };
    let mut prepared = Prepared {
        provider: spec.provider,
        root,
        workspace,
        files: Vec::new(),
        request,
    };
    let files = prepared.with_source(&helper(args), |source| {
        Ok(discover_import_files(source, options)?)
    })?;
    let matching = MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    let mut found = BTreeSet::new();
    for file in files {
        options.cancellation.check(file.path())?;
        let path = std::fs::canonicalize(file.path())?;
        if !paths.is_empty() && !paths.contains(&path) {
            continue;
        }
        let _inserted = found.insert(path);
        let relative = file
            .path()
            .strip_prefix(&prepared.root)
            .map_err(|error| Failure::input(error.to_string()))?;
        if patterns.is_empty()
            || patterns.iter().any(|pattern| {
                let candidate = if Path::new(pattern.as_str()).is_absolute() {
                    file.path()
                } else {
                    relative
                };
                pattern.matches_path_with(candidate, matching)
            })
        {
            prepared.files.push(file);
        }
    }
    if !paths.is_subset(&found) {
        return Err(Failure::input(
            "selected path is not a discoverable file for this provider",
        ));
    }
    if prepared.files.is_empty() && (!patterns.is_empty() || !paths.is_empty()) {
        return Err(Failure::input(
            "bulk selection matched no provider source files",
        ));
    }
    Ok(prepared)
}
