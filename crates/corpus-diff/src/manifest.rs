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
//! Every line is `<key><TAB><value>` ([`Manifest::from_keyed`]), so independent
//! changes merge key by key (`tools/manifest-merge.sh`). For that, no line sums
//! over our verdicts: a count of matches changes with every item it counts, so
//! any two changes to one file's items would conflict on it. Our verdicts are
//! listed item by item, and the only counts are of *inputs* — what the oracle
//! reported, and which records the handler probes were chosen from — which move
//! only when the corpus, the oracle or the sampling does. What each line says:
//!
//! - `<project>` → `comparable assets=…` or `skipped <why>`. A project that
//!   stops being comparable moves its line; one the oracle cannot type-check
//!   *grades nothing*, so it is `skipped fcs-errors` and its errors are listed
//!   (`<file>:<line>:<col> fcs-error` → `FS<n>`) rather than hidden.
//! - `<file>` → `compared records=… definitions=…` for each Compile file the
//!   oracle reported on — how many records it reported, and how many of those
//!   are its own defining occurrences — or `unreported` for one it said nothing
//!   about.
//! - `<file>:<start>-<end> "<name>"` → the outcome of every graded record that
//!   is not a match through the resolver, and of every record set aside or
//!   item of ours the oracle is silent about: deferrals with what was served
//!   and which guard declined, matches through the attribute or member
//!   surface, ambiguous ranges, shadowed constructor records, our or-pattern
//!   aliases, and so on. Several records at one range and name (FCS reports an
//!   attribute's name and its constructor there) are one line, their outcomes
//!   its values.
//!
//! So every oracle record that is neither a defining occurrence nor listed is
//! a match through the resolver, and a file's matches are its `records` less
//! its `definitions` less its listed records. That is checked here, file by
//! file, before the manifest is rendered ([`ManifestError::Unaccounted`]);
//! divergences are listed too, but the runner's zero-divergence gate fails
//! before the manifest is consulted, so regenerating it cannot bless one.
//!
//! Projects are keyed relative to a corpus root
//! ([`borzoi_oracle_harness::corpus_key`]), so the manifest names the same item
//! on every host the corpus is materialised on. Every `<file>` above is written
//! `<project> <path>`, the path relative to the project's directory (or
//! `root:<path>` for a file outside it): two projects may compile one file, and
//! each project's results for it are separate facts.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use borzoi_oracle_harness::corpus_key::{Positions, corpus_relative};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_sema::DeferredReason;

use crate::deferral_shape::DeferralCause;
use crate::handler_diff::{HandlerReport, PerturbedSkip, Probe, Variant, consistent};
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
    /// A compared file's graded items do not add up to the records the oracle
    /// reported in it, so the manifest could not say that every unlisted record
    /// is a match.
    Unaccounted {
        file: PathBuf,
        records: usize,
        items: usize,
    },
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
            Self::Unaccounted {
                file,
                records,
                items,
            } => write!(
                f,
                "{}: the oracle reported {records} records (definitions included) \
                 but the comparator graded {items}",
                file.display()
            ),
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

/// A file's key within one project's entries: `<project key> <file>`, where
/// `<file>` is relative to the project's directory, or `root:<key>` for a file
/// outside it (a linked source). Scoped by the project because two projects may
/// compile the same file, and a regression in one offset by an improvement in
/// the other must still move a line.
fn file_key(
    root: &Path,
    project: &Path,
    project_key: &str,
    path: &Path,
) -> Result<String, ManifestError> {
    let file = match project.parent() {
        Some(dir) if path.starts_with(dir) => corpus_relative(dir, path),
        _ => format!("root:{}", key(root, path)?),
    };
    Ok(format!("{project_key} {file}"))
}

/// The `(key, value)` items a manifest is built from ([`Manifest::from_keyed`]).
type Items = Vec<(String, String)>;

/// Whether `outcome` is one the oracle's record came to in the forward pass,
/// rather than an item of ours the oracle is silent about.
fn of_an_oracle_record(outcome: ItemOutcome) -> bool {
    match outcome {
        ItemOutcome::Match(..)
        | ItemOutcome::Deferral { .. }
        | ItemOutcome::Divergence(_)
        | ItemOutcome::SetAside(_) => true,
        ItemOutcome::UnoracledDefinition
        | ItemOutcome::UnoracledOrPatternAlias
        | ItemOutcome::ReverseDivergence => false,
    }
}

/// The manifest of `projects`, keyed relative to `root`.
pub fn project_corpus_manifest(
    projects: &[ProjectRecord],
    root: &Path,
) -> Result<Manifest, ManifestError> {
    let mut items = Items::new();
    for record in projects {
        let project = key(root, &record.project)?;
        match &record.verdict {
            ProjectVerdict::Comparable {
                assets,
                sources,
                comparison,
                handlers,
            } => {
                handler_entries(root, record, &project, sources, handlers, &mut items)?;
                items.push((
                    project.clone(),
                    format!("comparable assets={}", assets_label(assets)),
                ));
                let positions: HashMap<&Path, Positions<'_>> = sources
                    .iter()
                    .map(|(path, text)| (path.as_path(), Positions::new(text)))
                    .collect();
                // Per file, the forward pass's items: every oracle record's.
                let mut graded: HashMap<&Path, usize> = HashMap::new();
                for item in &comparison.ledger {
                    if of_an_oracle_record(item.outcome) {
                        *graded.entry(item.file.as_path()).or_default() += 1;
                    }
                    // Unlisted: a match through the resolver, and a defining
                    // occurrence; `records` and `definitions` account for both.
                    if matches!(
                        item.outcome,
                        ItemOutcome::Match(_, AnswerSurface::Resolver)
                            | ItemOutcome::SetAside(SetAside::Definition)
                    ) {
                        continue;
                    }
                    let at = positions.get(item.file.as_path()).ok_or_else(|| {
                        ManifestError::Entry(format!(
                            "ledger item in {}, which is not a source of {}",
                            item.file.display(),
                            record.project.display()
                        ))
                    })?;
                    items.push((
                        format!(
                            "{}:{} {:?}",
                            file_key(root, &record.project, &project, &item.file)?,
                            span(at, item.range),
                            item.name
                        ),
                        outcome_label(item.outcome),
                    ));
                }
                let compared: HashMap<&Path, &crate::ComparedFile> = comparison
                    .compared_files
                    .iter()
                    .map(|c| (c.path.as_path(), c))
                    .collect();
                for (path, _) in sources {
                    let file = file_key(root, &record.project, &project, path)?;
                    let items_here = graded.remove(path.as_path()).unwrap_or(0);
                    items.push((
                        file,
                        match compared.get(path.as_path()) {
                            Some(c) => {
                                if items_here != c.records {
                                    return Err(ManifestError::Unaccounted {
                                        file: path.clone(),
                                        records: c.records,
                                        items: items_here,
                                    });
                                }
                                format!(
                                    "compared records={} definitions={}",
                                    c.records, c.definitions
                                )
                            }
                            // A ledger item in a file the forward pass never
                            // compared would be a comparator defect; its items
                            // are listed, and this says it was not compared.
                            None if items_here > 0 => "uncompared-with-items".to_string(),
                            None => "unreported".to_string(),
                        },
                    ));
                }
            }
            ProjectVerdict::Skipped(skip) => {
                items.push((project.clone(), format!("skipped {}", skip_label(skip))));
                if let ProjectSkip::FcsErrors { files } = skip {
                    for file in files {
                        let erroring = file_key(root, &record.project, &project, &file.path)?;
                        for error in &file.errors {
                            items.push((
                                format!(
                                    "{erroring}:{}:{} fcs-error",
                                    error.range.start.line,
                                    error.range.start.col + 1
                                ),
                                format!("FS{:04}", error.error_number),
                            ));
                        }
                    }
                }
            }
        }
    }
    Manifest::from_keyed(items).map_err(|e| ManifestError::Entry(e.to_string()))
}

/// The handler differential's lines for one comparable project
/// ([`crate::handler_diff`]). None counts probe answers: what each probe should
/// answer follows from the comparator's verdict on its record
/// ([`consistent`]), so the lines are the probes that did not, and the inputs
/// that decide which probes were asked.
///
/// - `<file> handlers <copy>` → `selected=<n>` — per probed file and copy
///   (`plain` or `perturbed`), how many of its oracle records the copy asks
///   everything of by a rule that reads only the oracle and the text
///   ([`crate::handler_diff::Selected`]);
/// - `<file>:<span> "<name>" handlers <copy> <handler> <probe>` → `<outcome>
///   expected <verdict>` — every probe whose answer is not the one the
///   comparator's verdict on its record implies;
/// - `<project> handlers perturbed` → `skipped <why>`, with the oracle's errors
///   on the copy, when the perturbed run could not be made;
/// - `<file> handlers` → `skipped owned-by-another-project` for a source the
///   server answers as another project's, which is not probed;
/// - `<file>:<span> "<name>" hover-type <copy>` → `<class> <verdict> at
///   <probes>` — every record whose hover describes its symbol with a verdict
///   other than its class's expected one
///   ([`crate::handler_diff::SymbolClass::expected_verdict`]), with the probes
///   that came to it. A type lost from a hover lists its record here. A hover
///   that describes no symbol is a probe whose outcome is not the expected
///   one, listed above.
///
/// A perturbed probe's span is in the perturbed copy's text.
fn handler_entries(
    root: &Path,
    record: &ProjectRecord,
    project: &str,
    sources: &[(PathBuf, std::sync::Arc<str>)],
    handlers: &HandlerReport,
    items: &mut Items,
) -> Result<(), ManifestError> {
    let plain: HashMap<&Path, Positions<'_>> = sources
        .iter()
        .map(|(path, text)| (path.as_path(), Positions::new(text)))
        .collect();
    let perturbed: HashMap<&Path, Positions<'_>> = sources
        .iter()
        .zip(&handlers.perturbed_texts)
        .map(|((path, _), text)| (path.as_path(), Positions::new(text)))
        .collect();
    // A listed hover-type record, and the probes that came to its verdict.
    type Listed = (String, &'static str, &'static str, String);
    let mut type_listed: BTreeMap<Listed, Vec<Probe>> = BTreeMap::new();
    for probe in &handlers.probes {
        let file = file_key(root, &record.project, project, &probe.file)?;
        let positions = match probe.variant {
            Variant::Plain => &plain,
            Variant::Perturbed => &perturbed,
        };
        let at = || {
            positions.get(probe.file.as_path()).ok_or_else(|| {
                ManifestError::Entry(format!(
                    "a handler probe in {}, which is not a source of {}",
                    probe.file.display(),
                    record.project.display()
                ))
            })
        };
        if let Some(grade) = probe.hover_type
            && grade.listed()
        {
            type_listed
                .entry((
                    format!("{file}:{} {:?}", span(at()?, probe.range), probe.name),
                    probe.variant.label(),
                    grade.class.label(),
                    grade.verdict.label(),
                ))
                .or_default()
                .push(probe.probe);
        }
        if consistent(probe.handler, probe.expectation, probe.outcome) {
            continue;
        }
        items.push((
            format!(
                "{file}:{} {:?} handlers {} {} {}",
                span(at()?, probe.range),
                probe.name,
                probe.variant.label(),
                probe.handler.label(),
                probe.probe.label(),
            ),
            format!(
                "{} expected {}",
                probe.outcome.label(),
                probe.expectation.label()
            ),
        ));
    }
    for (path, selected) in &handlers.selected {
        let file = file_key(root, &record.project, project, path)?;
        items.push((
            format!("{file} handlers {}", Variant::Plain.label()),
            format!("selected={}", selected.plain),
        ));
        if handlers.perturbed_skip.is_none() {
            items.push((
                format!("{file} handlers {}", Variant::Perturbed.label()),
                format!("selected={}", selected.perturbed),
            ));
        }
    }
    for ((record, variant, class, verdict), mut probes) in type_listed {
        probes.sort_unstable();
        let probes: Vec<&str> = probes.into_iter().map(|p| p.label()).collect();
        items.push((
            format!("{record} hover-type {variant}"),
            format!("{class} {verdict} at {}", probes.join(",")),
        ));
    }
    for path in &handlers.owned_elsewhere {
        items.push((
            format!(
                "{} handlers",
                file_key(root, &record.project, project, path)?
            ),
            "skipped owned-by-another-project".to_string(),
        ));
    }
    match &handlers.perturbed_skip {
        None => {}
        Some(PerturbedSkip::Fcs(_)) => {
            items.push((
                format!("{project} handlers perturbed"),
                "skipped fcs-failed".to_string(),
            ));
        }
        Some(PerturbedSkip::FcsErrors(errors)) => {
            items.push((
                format!("{project} handlers perturbed"),
                format!("skipped fcs-errors errors={}", errors.len()),
            ));
            for (path, line, col, number) in errors {
                items.push((
                    format!(
                        "{}:{line}:{col} handlers perturbed fcs-error",
                        file_key(root, &record.project, project, path)?
                    ),
                    format!("FS{number:04}"),
                ));
            }
        }
    }
    Ok(())
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
        ItemOutcome::Match(graded, surface) => format!(
            "{}-match{}",
            graded_label(graded),
            match surface {
                AnswerSurface::Resolver => "",
                AnswerSurface::Attribute => " attribute",
                AnswerSurface::Member => " member",
            }
        ),
        ItemOutcome::Deferral {
            graded,
            served,
            cause,
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
            // A guard's label always carries its `@tier`, so neither a shape
            // nor `unexplained` can be read as one.
            let cause = match cause {
                DeferralCause::Guard(site) => {
                    format!("{}@{}", site.cause.label(), site.tier.label())
                }
                DeferralCause::Shape(shape) => shape.label().to_string(),
                DeferralCause::Unexplained => "unexplained".to_string(),
            };
            format!("{}-deferral {served} {cause}", graded_label(graded))
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
