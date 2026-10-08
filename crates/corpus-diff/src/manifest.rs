//! The run's outcome as an exact manifest: one line per project, per compared
//! file and per non-matching item, sorted (`borzoi_oracle_harness::manifest`).
//!
//! The pinned corpus is fixed by revision and both sides are deterministic, so
//! what each project, file and oracle record comes to is a fixed fact. Checking
//! that fact in and comparing a run against it **exactly** leaves no slack for a
//! regression to hide in. A one-sided bound cannot do that: a change that makes
//! the LSP serve nothing at all keeps the divergence count at zero and passes
//! any divergence ceiling, and a movement inside a count (one use gained,
//! another lost) is invisible however tight the bound.
//!
//! What the lines say:
//!
//! - `<project> comparable assets=…` or `<project> skipped <why>`. A project
//!   that stops being comparable moves its line; one the oracle cannot
//!   type-check *grades nothing*, so it is `skipped fcs-errors` and its errors
//!   are listed (`<file>:<line>:<col> fcs-error FS<n>`) rather than hidden.
//! - `<file> compared match=… assembly-match=… attribute=… member=…
//!   definitions=…` for each Compile file the oracle reported on, or
//!   `<file> unreported` for one it said nothing about. Matches are counted, not
//!   listed: every *non*-match is listed below, so no movement is lost.
//! - `<file>:<start>-<end> "<name>" <outcome>` for every graded item that is not
//!   a match, and every oracle record set aside — deferrals with what was served
//!   and which guard declined, ambiguous ranges, shadowed constructor records,
//!   our or-pattern aliases the oracle is silent about, and so on. FCS's own
//!   defining occurrences are counted per file instead (`definitions=`): they
//!   are not uses, and nothing of ours is graded against them.
//!
//! A manifest records state, so it cannot say a state is wrong. Divergences are
//! listed too, but the runner's zero-divergence gate fails before the manifest
//! is consulted, so regenerating it cannot bless one.
//!
//! Files and projects are keyed relative to a corpus root
//! ([`borzoi_oracle_harness::corpus_key`]), so the manifest names the same item
//! on every host the corpus is materialised on.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use borzoi_oracle_harness::corpus_key::{Positions, corpus_relative};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_sema::DeferredReason;

use crate::{
    AnswerSurface, Graded, ItemOutcome, LoadSkip, ProjectAssetsStatus, ProjectRecord, ProjectSkip,
    ProjectVerdict, ServedDecline, SetAside,
};

/// Why a run could not be rendered as a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// A project or file sits outside the root the manifest is keyed against,
    /// so it has no host-independent name.
    OutsideRoot { path: PathBuf, root: PathBuf },
    /// An item could not be written as a manifest entry.
    Entry(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutsideRoot { path, root } => write!(
                f,
                "{} is outside the manifest root {}, so it has no manifest key",
                path.display(),
                root.display()
            ),
            Self::Entry(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ManifestError {}

fn key(root: &Path, path: &Path) -> Result<String, ManifestError> {
    if !path.starts_with(root) {
        return Err(ManifestError::OutsideRoot {
            path: path.to_path_buf(),
            root: root.to_path_buf(),
        });
    }
    Ok(corpus_relative(root, path))
}

/// The manifest of `projects`, keyed relative to `root`.
pub fn project_corpus_manifest(
    projects: &[ProjectRecord],
    root: &Path,
) -> Result<Manifest, ManifestError> {
    let mut entries = Vec::new();
    for record in projects {
        let project = key(root, &record.project)?;
        match &record.verdict {
            ProjectVerdict::Comparable {
                assets,
                sources,
                comparison,
            } => {
                entries.push(format!(
                    "{project} comparable assets={}",
                    assets_label(assets)
                ));
                let positions: HashMap<&Path, Positions<'_>> = sources
                    .iter()
                    .map(|(path, text)| (path.as_path(), Positions::new(text)))
                    .collect();
                let mut per_file: BTreeMap<&Path, FileCounts> = BTreeMap::new();
                for path in &comparison.compared_files {
                    per_file.entry(path.as_path()).or_default();
                }
                for item in &comparison.ledger {
                    let counts = per_file.entry(item.file.as_path()).or_default();
                    match item.outcome {
                        ItemOutcome::Match(graded, surface) => counts.matched(graded, surface),
                        ItemOutcome::SetAside(SetAside::Definition) => counts.definitions += 1,
                        outcome => {
                            let at = positions.get(item.file.as_path()).ok_or_else(|| {
                                ManifestError::Entry(format!(
                                    "ledger item in {}, which is not a source of {}",
                                    item.file.display(),
                                    record.project.display()
                                ))
                            })?;
                            entries.push(format!(
                                "{}:{} {:?} {}",
                                key(root, &item.file)?,
                                span(at, item.range),
                                item.name,
                                outcome_label(outcome)
                            ));
                        }
                    }
                }
                for (path, _) in sources {
                    let file = key(root, path)?;
                    entries.push(match per_file.get(path.as_path()) {
                        Some(counts) if comparison.compared_files.contains(path) => {
                            format!("{file} compared {}", counts.render())
                        }
                        // A ledger item in a file the forward pass never
                        // compared would be a comparator defect; render it
                        // rather than lose it.
                        Some(counts) => format!("{file} uncompared-with-items {}", counts.render()),
                        None => format!("{file} unreported"),
                    });
                }
            }
            ProjectVerdict::Skipped(skip) => {
                entries.push(format!("{project} skipped {}", skip_label(skip)));
                if let ProjectSkip::FcsErrors { files } = skip {
                    for file in files {
                        let file_key = key(root, &file.path)?;
                        for error in &file.errors {
                            entries.push(format!(
                                "{file_key}:{}:{} fcs-error FS{:04}",
                                error.range.start.line,
                                error.range.start.col + 1,
                                error.error_number
                            ));
                        }
                    }
                }
            }
        }
    }
    Manifest::from_counted(entries).map_err(|e| ManifestError::Entry(e.to_string()))
}

/// `line:col-col` for a range on one line, `line:col-line:col` otherwise. The
/// end is kept, not just the start: a qualifier's record and the whole path's
/// record start at the same byte (`Shared` and `Shared.foo`).
fn span(at: &Positions<'_>, (start, end): (usize, usize)) -> String {
    let (start, end) = (at.at(start), at.at(end));
    match (start.split_once(':'), end.split_once(':')) {
        (Some((start_line, _)), Some((end_line, end_col))) if start_line == end_line => {
            format!("{start}-{end_col}")
        }
        _ => format!("{start}-{end}"),
    }
}

#[derive(Debug, Default)]
struct FileCounts {
    matches: usize,
    assembly_matches: usize,
    attribute: usize,
    member: usize,
    definitions: usize,
}

impl FileCounts {
    fn matched(&mut self, graded: Graded, surface: AnswerSurface) {
        match graded {
            Graded::Project => self.matches += 1,
            Graded::Assembly => self.assembly_matches += 1,
        }
        match surface {
            AnswerSurface::Resolver => {}
            AnswerSurface::Attribute => self.attribute += 1,
            AnswerSurface::Member => self.member += 1,
        }
    }

    fn render(&self) -> String {
        format!(
            "match={} assembly-match={} attribute={} member={} definitions={}",
            self.matches, self.assembly_matches, self.attribute, self.member, self.definitions
        )
    }
}

fn assets_label(assets: &ProjectAssetsStatus) -> String {
    match assets {
        ProjectAssetsStatus::Resolved {
            package_dlls,
            framework_dlls,
            project_refs,
            ..
        } => format!(
            "resolved packages={package_dlls} frameworks={framework_dlls} project-refs={project_refs}"
        ),
        other => other.kind().to_string(),
    }
}

fn skip_label(skip: &ProjectSkip) -> String {
    match skip {
        ProjectSkip::Load(load) => match load {
            LoadSkip::ProjectEvaluationFailed => "load project-evaluation-failed",
            LoadSkip::ItemsUncertain { .. } => "load items-uncertain",
            LoadSkip::DefineConstantsUncertain { .. } => "load define-constants-uncertain",
            LoadSkip::TooManyFiles { .. } => "load too-many-files",
            LoadSkip::SemanticUnavailable => "load semantic-unavailable",
            LoadSkip::ReferenceSetUnstable => "load reference-set-unstable",
        }
        .to_string(),
        ProjectSkip::FcsInvoke => "fcs-invoke-failed".to_string(),
        ProjectSkip::FcsParse => "fcs-output-unparseable".to_string(),
        ProjectSkip::FcsErrors { files } => format!(
            "fcs-errors files={} errors={}",
            files.len(),
            files.iter().map(|f| f.errors.len()).sum::<usize>()
        ),
    }
}

fn graded_label(graded: Graded) -> &'static str {
    match graded {
        Graded::Project => "project",
        Graded::Assembly => "assembly",
    }
}

fn outcome_label(outcome: ItemOutcome) -> String {
    match outcome {
        ItemOutcome::Match(graded, _) => format!("{}-match", graded_label(graded)),
        ItemOutcome::Deferral {
            graded,
            served,
            site,
        } => {
            let served = match served {
                ServedDecline::Unrecorded => "unrecorded",
                ServedDecline::Unresolved => "unresolved",
                ServedDecline::Deferred(reason) => match reason {
                    DeferredReason::UnboundName => "unbound-name",
                    DeferredReason::QualifiedAccess => "qualified-access",
                    DeferredReason::ShadowableType => "shadowable-type",
                    DeferredReason::IncompleteAssemblies => "incomplete-assemblies",
                },
            };
            let site = match site {
                Some(site) => format!("{}@{}", site.cause.label(), site.tier.label()),
                None => "unattributed".to_string(),
            };
            format!("{}-deferral {served} {site}", graded_label(graded))
        }
        ItemOutcome::Divergence(graded) => format!("{}-divergence", graded_label(graded)),
        ItemOutcome::SetAside(kind) => match kind {
            SetAside::Definition => "fcs-definition",
            SetAside::ZeroWidth => "zero-width",
            SetAside::CompilerGenerated => "compiler-generated",
            SetAside::NonProjectDeclaration => "non-project-declaration",
            SetAside::OutOfProjectDeclaration => "out-of-project-declaration",
            SetAside::NoOracleDeclaration => "no-oracle-declaration",
            SetAside::AmbiguousOracleRange => "ambiguous-oracle-range",
            SetAside::ShadowedConstructorUse => "shadowed-constructor",
        }
        .to_string(),
        ItemOutcome::UnoracledDefinition => "unoracled-definition".to_string(),
        ItemOutcome::UnoracledOrPatternAlias => "unoracled-or-pattern-alias".to_string(),
        ItemOutcome::ReverseDivergence => "reverse-divergence".to_string(),
    }
}
