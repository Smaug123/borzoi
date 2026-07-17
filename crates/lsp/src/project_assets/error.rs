use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
pub enum ProjectAssetsError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    MultipleOrNoTargets {
        found: Vec<String>,
    },
    TargetForTfmMissing {
        tfm: String,
        found: Vec<String>,
    },
    PackageFolderMissing,
    LibraryEntryMissing {
        name_version: String,
    },
    ProjectRefMissingPath {
        name_version: String,
    },
    MissingTransitiveAssets {
        project_path: PathBuf,
    },
    FrameworkPackNotFound {
        name: String,
        searched: PathBuf,
    },
    FrameworkRefForTfmMissing {
        name: String,
        tfm: String,
    },
    /// A `type: "project"` target entry had no `framework` field, so the
    /// producer's TFM is unknown. Real NuGet always emits this for
    /// project references; absence means the assets file was hand-edited,
    /// was written by a tool we don't model, or was restored against an
    /// older NuGet version. The resolver refuses to guess.
    ProjectRefUnresolved {
        name_version: String,
    },
    /// Phase 2b platform-suffix recovery surfaced a mismatch: the
    /// producer's declared TFMs (its own `project.frameworks` keys) do
    /// not include one whose base matches the producer TFM NuGet recorded
    /// in the consumer's assets file — or multiple match but none has
    /// the same platform suffix as the consumer. Either the producer's
    /// `<TargetFrameworks>` was edited since `dotnet restore` last ran,
    /// or the consumer's assets file was hand-edited. Re-run restore.
    RestoreMismatch {
        producer_path: PathBuf,
        consumer_tfm: String,
        base_producer_tfm: String,
        producer_declared: Vec<String>,
    },
    /// Caller passed a producer-declared-TFMs map to
    /// `transitive_project_tfms` that omits an entry for a closure node.
    /// Programming error: callers must supply declared TFMs for every
    /// producer csproj in the closure (Phase 3 wiring loads them
    /// transitively from `obj/project.assets.json` on disk).
    ProducerAssetsNotProvided {
        producer_path: PathBuf,
    },
}

impl fmt::Display for ProjectAssetsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProjectAssetsError::Io { path, source } => {
                write!(f, "I/O error reading {}: {source}", path.display())
            }
            ProjectAssetsError::Json { path, source } => {
                write!(f, "JSON parse error in {}: {source}", path.display())
            }
            ProjectAssetsError::MultipleOrNoTargets { found } => {
                if found.is_empty() {
                    write!(f, "project.assets.json has no targets")
                } else {
                    write!(
                        f,
                        "project.assets.json has multiple targets ({}); caller must select one",
                        found.join(", ")
                    )
                }
            }
            ProjectAssetsError::TargetForTfmMissing { tfm, found } => {
                write!(
                    f,
                    "project lists TFM {tfm} but targets has only [{}]",
                    found.join(", ")
                )
            }
            ProjectAssetsError::PackageFolderMissing => {
                write!(
                    f,
                    "project.assets.json has no packageFolders entries (NuGet cache unresolved)"
                )
            }
            ProjectAssetsError::LibraryEntryMissing { name_version } => {
                write!(
                    f,
                    "target entry {name_version} has no matching libraries entry"
                )
            }
            ProjectAssetsError::ProjectRefMissingPath { name_version } => {
                write!(
                    f,
                    "project reference {name_version} has no `path` field in its target entry"
                )
            }
            ProjectAssetsError::MissingTransitiveAssets { project_path } => {
                write!(
                    f,
                    "transitive project reference {} has no obj/project.assets.json (was it restored?)",
                    project_path.display()
                )
            }
            ProjectAssetsError::FrameworkPackNotFound { name, searched } => {
                write!(
                    f,
                    "framework reference pack {name} not found under {}",
                    searched.display()
                )
            }
            ProjectAssetsError::FrameworkRefForTfmMissing { name, tfm } => {
                write!(
                    f,
                    "no installed version of framework pack {name} contains ref/{tfm}"
                )
            }
            ProjectAssetsError::ProjectRefUnresolved { name_version } => {
                write!(
                    f,
                    "project reference {name_version} has no `framework` field in its target entry (was it restored?)"
                )
            }
            ProjectAssetsError::RestoreMismatch {
                producer_path,
                consumer_tfm,
                base_producer_tfm,
                producer_declared,
            } => {
                write!(
                    f,
                    "producer {} declares [{}] but consumer (TFM {consumer_tfm}) recorded base producer TFM {base_producer_tfm}; re-run `dotnet restore`",
                    producer_path.display(),
                    producer_declared.join(", "),
                )
            }
            ProjectAssetsError::ProducerAssetsNotProvided { producer_path } => {
                write!(
                    f,
                    "no declared-TFMs entry provided for producer {}",
                    producer_path.display()
                )
            }
        }
    }
}

impl std::error::Error for ProjectAssetsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProjectAssetsError::Io { source, .. } => Some(source),
            ProjectAssetsError::Json { source, .. } => Some(source),
            _ => None,
        }
    }
}
