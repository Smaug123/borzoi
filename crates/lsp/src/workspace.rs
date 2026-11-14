//! Per-file project lookup + symbol-set resolution.
//!
//! The LSP needs to know which preprocessor symbols are active for each
//! open file so its diagnostics agree with what the F# compiler would
//! produce. The symbol set comes from the file's owning `.fsproj`'s
//! evaluated `$(DefineConstants)`, plus the default `COMPILED` (matching
//! FCS's `Driver/fsc.fs:514`).
//!
//! [`Workspace`] is the front door:
//!
//! - [`Workspace::symbols_for`] given a file path, returns the active
//!   symbol set as a [`HashSet<String>`]. Lazily evaluates the owning
//!   `.fsproj` on first lookup and caches the result.
//! - [`Workspace::owning_project`] finds the project that owns a file: the
//!   nearest ancestor project that lists it, else the first ancestor
//!   directory holding a `.fsproj` (`find_owning_project`). Multiple
//!   `.fsproj`s in the same directory are tie-broken alphabetically for
//!   determinism.
//!
//! The cache needs no file watching: each evaluation keeps every file-system
//! query it and the reads preparing it made, with the answers, and within a
//! dispatch-loop generation the first lookup of a cached evaluation confirms
//! them ([`Workspace::validate`], through [`crate::read_checks`]). One whose
//! answers moved is evaluated afresh. Reads strictly inside an installed SDK,
//! pack, workload manifest or package version are not confirmed: an installer
//! never changes a version in place. A watch event is a hint
//! ([`Workspace::hint`]): it names paths to ask again whatever their stamps
//! say, and a structural one ([`Workspace::note_structural_event`]) makes the
//! next confirmation of every evaluation cover the installed contents it
//! trusts too. The one input no evaluation records, NuGet's settings, is
//! covered by its watch event alone ([`Workspace::drop_every_evaluation`]).
//!
//! SDK resolution is wired through [`SdkDiscovery`]: each project gets
//! its own discovery context (built from a [`SdkDiscoveryEnv`] held on
//! the workspace) and the resulting resolver is handed to
//! `parse_fsproj_with_imports`. When discovery fails — most commonly
//! because `$DOTNET_ROOT` isn't set and no `dotnet` is on `$PATH` — we
//! log to stderr and fall back to evaluating the project without an SDK
//! resolver. The msbuild evaluator reports `SdkResolverAbsent` for
//! `<Project Sdk="...">` in that case (which the `.fsproj` buffer's
//! diagnostics render with the discovery error, see
//! [`crate::fsproj_diagnostics`]), but evaluation still completes and the
//! body's own `<DefineConstants>` are still collected.

use crate::inbox::{Superseded, Watch, never_superseded};
use crate::observed::{EvaluationCounts, Unobserved};
use crate::read_checks::{Check, ReadChecks, a_fact};
use crate::vfs::Nanos;
use borzoi_msbuild::observed::{
    self as msbuild_observed, Listed, Observation, Observer, Read, ReadCounts, ReadSet,
};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use borzoi_cst::language_version::LanguageVersion;
use borzoi_msbuild::{
    GlobResolver, ParsedProject, ResolvedItem, SdkResolution, SdkResolver,
    parse_fsproj_with_imports, target_frameworks,
};

use crate::paths::{lexically_normalize, paths_equal};
use crate::project_deferral::ProjectEvaluation;
use crate::project_graph::{
    Edge, EdgeKind, NodeResult, NodeTfm, ProjectGraph, ProjectKind, build_graph, classify,
};
use crate::sdk_discovery::{SdkDiscovery, SdkDiscoveryEnv};
use crate::semantic::ProjectId;
use crate::tfm_policy::{self, caller_owns_target_framework, seed_target_framework_global};
use borzoi_msbuild::ItemMetadataValue;

const COMPILED: &str = "COMPILED";
const EDITING: &str = "EDITING";
const INTERACTIVE: &str = "INTERACTIVE";

/// The implicit preprocessor symbols FCS's *service* parser
/// (`FSharpChecker.ParseFile`) defines for a file of the given kind, before any
/// project `DefineConstants` are layered on:
///
/// - a compiled `.fs` / `.fsi` → `COMPILED` + `EDITING`
/// - a `.fsx` / `.fsscript`    → `INTERACTIVE` + `EDITING`
///
/// `EDITING` is in both because we analyse source *for editing* (an LSP) — the
/// parse FCS performs in an IDE — not `fsc`'s batch compile. (`COMPILED` mirrors
/// `Driver/fsc.fs:514`; `INTERACTIVE` is the script counterpart.)
///
/// Public so the server's non-`file:` buffer fallback and the FCS
/// differentials share it rather than hand-building an incomplete set.
pub fn implicit_symbols(is_script: bool) -> HashSet<String> {
    let mut s = HashSet::new();
    s.insert(EDITING.to_string());
    s.insert(if is_script { INTERACTIVE } else { COMPILED }.to_string());
    s
}

/// Whether `file` is an F# script (`.fsx`), which FCS parses with
/// `isInteractive` — flipping the implicit define from `COMPILED` to
/// `INTERACTIVE`. Matched case-insensitively, as FCS checks suffixes
/// (`EndsWithOrdinalIgnoreCase`).
///
/// FCS also treats `.fsscript` as a script, but the LSP's source dispatch only
/// routes `.fs`/`.fsi`/`.fsx`, so a `.fsscript` file never reaches here; it is
/// deliberately not matched rather than recognised-but-unreachable.
fn is_script_path(file: &Path) -> bool {
    file.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("fsx"))
}

/// Lazy cache mapping owning-`.fsproj` path → evaluated [`ParsedProject`]
/// (or `None` if evaluation failed). One instance lives in the LSP server
/// state; it grows as the user opens files from new projects.
/// A generation of [`Workspace`]'s project-evaluation cache; see
/// [`Workspace::projects_generation`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProjectsGeneration(u64);

pub struct Workspace {
    /// Keyed by [`ProjectId`], whose spelling is what was evaluated. The
    /// evaluation is `None` when it failed for any reason (IO, malformed
    /// XML, reserved-property collision in `extra_properties`) — caching
    /// the failure means we don't re-attempt on every keystroke. Each entry
    /// carries what it read, and is confirmed against the disk once per
    /// generation that reads it ([`Self::validate`]).
    projects: HashMap<ProjectId, CachedEvaluation>,
    /// Bumped whenever an evaluation the cache served is dropped (by a
    /// validation that finds an entry's reads moved, or by
    /// [`Self::drop_every_evaluation`]), whenever a file's owner moves, and
    /// whenever a structural watch event arrives
    /// ([`Self::note_structural_event`]). So a caller that read several
    /// evaluations across a span of time can tell whether they all came from
    /// one generation of the cache — see [`Self::projects_generation`].
    projects_generation: ProjectsGeneration,
    /// Confirms the evaluations' reads once per generation, by stamps, so
    /// confirming an unchanged file or directory costs one `stat`. The
    /// assembly envs' reads are confirmed through it too
    /// ([`Self::confirm_env`]), so a directory or file both read is `stat`ed
    /// once per generation.
    checker: ReadChecks,
    /// What confirming assembly envs has cost the open generation: the part
    /// of `checker`'s counts that [`Self::confirm_env`] spent.
    env_counts: Cell<ReadCounts>,
    /// MSBuild evaluations run in the open generation.
    evaluated: Cell<u64>,
    /// The checks of each evaluation dropped and not yet evaluated again,
    /// held until it is: its reads are mostly the new evaluation's, and a
    /// check the checker still holds hands its witness to the equal read
    /// of the new one ([`ReadChecks::check`]), so the new evaluation's first
    /// confirmation asks again only what moved. At most one entry per
    /// project, released when the project is evaluated again.
    retired: HashMap<ProjectId, Vec<Check>>,
    /// Each file's owner ([`Self::owning_project`]) as a walk inside a
    /// generation found it, keyed by the file's spelling, with what the walk
    /// read. Confirmed once per generation that asks for it.
    owners: HashMap<PathBuf, Owner>,
    /// The directory listings the ownership walks read, one per directory,
    /// which every owner whose walk read it shares; its counts are the
    /// generation's ownership counts.
    dir_listings: DirListings,
    /// The identity the next cached evaluation gets
    /// ([`CachedEvaluation::id`]).
    next_evaluation: Cell<u64>,
    /// SDK-resolution environment used to build a [`SdkDiscovery`] per
    /// project on first evaluation. Held by value so tests can supply
    /// a hermetic env without mutating the host's environment.
    env: SdkDiscoveryEnv,
    /// Additional MSBuild global properties supplied by an integration
    /// harness. These override the LSP's default `Configuration`/`Platform`
    /// seeds and are cached with the workspace instance.
    extra_build_properties: HashMap<String, String>,
}

/// A project's evaluation plus the .NET install root the SDK resolver
/// actually resolved its `<Project Sdk=...>` from. The two are cached
/// together because the install root is a *by-product of the same
/// resolution* that produced `parsed`: recovering it independently (e.g.
/// re-probing a bare `Microsoft.NET.Sdk`) can disagree with the SDK the
/// project really resolved against in a multi-root `global.json`
/// `sdk.paths` workspace, sending the assembly env's framework-pack lookup
/// to the wrong install.
#[derive(Debug)]
struct EvaluatedProject {
    parsed: ParsedProject,
    /// Install root (the directory holding `packs/`) of the entry project's
    /// **own** SDK — recovered from [`ParsedProject::resolved_sdk_root`].
    /// `None` when the entry declares no SDK of its own (SDK-less entry, or
    /// the resolver was disabled / failed); the consumer
    /// ([`Workspace::dotnet_root_for_project`]) then falls back to the probe.
    sdk_install_root: Option<PathBuf>,
    /// The target framework `parsed` was evaluated under (fsproj 3.3c,
    /// `docs/completed/fsproj-tfm-selection-plan.md` E1/E2): the caller-seeded or
    /// body-written `TargetFramework` when non-empty, else the first-declared
    /// `<TargetFrameworks>` entry (under which the project was re-evaluated
    /// in a second pass), else `None` (no TFM declared). Recorded so the
    /// assembly-env layer can select the same TFM's assets target — the E5
    /// coherence invariant — via [`Workspace::target_framework_for_project`].
    chosen_tfm: Option<String>,
    /// The declared TFM list ([`target_frameworks`]) **as the first pass saw
    /// it** — i.e. before any first-declared re-evaluation. Load-bearing for
    /// the graph resolver's multi-target detection: an outer-gated plural
    /// (`<TargetFrameworks Condition="'$(TargetFramework)' == ''">`, the
    /// arcade idiom) vanishes from the re-evaluated `parsed`'s properties, so
    /// reading the list off `parsed` would under-count.
    declared_tfms: Vec<String>,
    /// The first pass's body-written non-empty `TargetFramework`, i.e.
    /// whether the project pins its own singular TFM
    /// (`select_target_framework` case 2). Distinguishes "TFM known" from
    /// "first-declared default" — `chosen_tfm` alone cannot.
    body_target_framework: Option<String>,
    /// Whether the body-singular TFM's provenance is **untrusted**: the
    /// first pass's `TargetFramework` is unpinned or SDK-tainted
    /// ([`ParsedProject::property_provenance_untrusted`]), or the body
    /// value still holds unexpanded `$(...)`. The real build may then
    /// select a different TFM, so [`resolve_node_uncached`] must not report
    /// the node as [`NodeTfm::Known`] under it (the env fold would locate
    /// that TFM's output on trust) nor trust its evaluated output name
    /// (which may itself be TFM-gated), and
    /// [`Workspace::target_framework_for_project`] must decline rather than
    /// let the entry select an assets target (and seed the producer-TFM
    /// recovery) under it. Deliberately NOT extended to the
    /// plural `TargetFrameworks`: an outer-gated plural (the arcade idiom)
    /// is unpinned by construction, and its consumer — the TFM-invariant
    /// intersection — never trusts a single branch anyway. A
    /// caller-supplied global `TargetFramework` is immune too: globals
    /// out-rank body writes, and the caller's value needs no provenance.
    tfm_untrusted: bool,
    /// The document can overwrite the `TargetFramework` global we seed to serve
    /// its inner build (`<Project TreatAsLocalProperty="TargetFramework">`), so
    /// **no evaluation of it is an inner build**: MSBuild evaluates in document
    /// order, so a gated group above such an override contributes the seed's
    /// values while everything after it runs under the override's. `parsed` is
    /// therefore the *outer dispatch* build, whose one virtue is that it is at
    /// least internally consistent, and every TFM-derived conclusion is
    /// withheld.
    ///
    /// Withheld includes the reference list, which
    /// [`Self::tfm_untrusted`] alone does not cover.
    /// That flag's consumers keep pass 1's edges on the argument
    /// that any TFM-dependent edge read the unpinned `TargetFramework` and so
    /// already flipped `project_references_uncertain` — true when the singular
    /// is unpinned, false here: a multi-targeted document never *writes* the
    /// singular, so pass 1 decides `'$(TargetFramework)' == ''` cleanly under
    /// the environment model and captures an outer-build-only edge as fact.
    /// See `an_override_document_does_not_publish_outer_build_only_edges`.
    ///
    /// Zero projects in the pinned F# corpus or the local NuGet cache opt
    /// `TargetFramework` out (15 opt out `RepoRoot`, 2 `OutDir`, 1 each
    /// `WasmNativeWorkload` and `RestoreAdditionalProjectSources`), so this
    /// declines nothing anyone has been observed to write.
    not_an_inner_build: bool,
}

/// What one generation of a [`Workspace`] observed
/// ([`Workspace::end_generation`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceCounts {
    /// Reading project evaluations.
    pub evaluations: EvaluationCounts,
    /// Finding the projects that own files ([`Workspace::owning_project`]).
    pub ownership: ReadCounts,
    /// Confirming cached assembly envs ([`Workspace::confirm_env`]).
    pub envs: ReadCounts,
}

/// One cached project evaluation and what it depends on.
#[derive(Debug)]
struct CachedEvaluation {
    /// Which evaluation this is: no two entries the workspace caches share
    /// one, so an ownership answer that consulted this entry can tell
    /// whether the entry it would consult now is the same.
    id: EvaluationId,
    evaluated: Option<EvaluatedProject>,
    /// The reads to confirm before serving `evaluated` again: everything the
    /// evaluation and the reads preparing it observed ([`Evaluation::reads`]),
    /// less the immutable contents (`Evaluation::checks_and_exempt`).
    checks: Vec<Check>,
    /// The reads of immutable contents it trusts rather than confirms (D3's
    /// axiom), confirmed only after a structural watch event
    /// ([`Self::event_since`]): an event is what covers an in-place change
    /// to an installed version.
    exempt: Vec<Check>,
    /// A structural watch event has arrived since the entry was last
    /// confirmed ([`Workspace::note_structural_event`]), so its next
    /// confirmation covers `exempt` too.
    event_since: bool,
    /// Where the immutable contents its reads may include live, which an env
    /// built from this project's references exempts too
    /// ([`Workspace::env_checks`]).
    immutable: ImmutableRoots,
    /// Every read the evaluation depends on, the immutable contents
    /// included: what an env whose graph walk consulted this entry depends
    /// on through it (`project_graph_impl`).
    reads: ReadSet,
    /// The generation that last confirmed `checks` (or that evaluated it);
    /// `None` when it was evaluated outside a generation.
    validated_in: Option<u64>,
}

/// The identity of one cached evaluation (`CachedEvaluation::id`): what a
/// value derived from an evaluation records, so that it can tell whether the
/// evaluation it was derived from is the one the workspace serves now
/// ([`Workspace::evaluation_id`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvaluationId(u64);

/// A file's owner as one ownership walk found it, and everything that answer
/// depends on: the listings of the directories it read, and the evaluations
/// whose membership verdicts it consulted. While every listing holds and
/// every consulted evaluation is still the entry cached, the walk would find
/// the same owner again: it is a function of those listings and verdicts,
/// and a verdict is a function of the evaluation and the file.
#[derive(Debug)]
struct Owner {
    project: Option<PathBuf>,
    /// The directories the walk listed, each with the listing it read.
    listings: Vec<(PathBuf, ListingId)>,
    consulted: Vec<(ProjectId, EvaluationId)>,
    /// The generation that last confirmed the answer (or that walked).
    validated_in: u64,
}

/// The directory listings the ownership walks read: one per directory,
/// confirmed at most once per generation and shared by every owner whose
/// walk read it, so a sweep that asks for every file of a directory keeps
/// one copy of its listing, not one per file.
#[derive(Debug, Default)]
struct DirListings {
    /// Lists the directories, and confirms the listings by the
    /// directories' stamps.
    checks: ReadChecks,
    by_dir: HashMap<PathBuf, DirListing>,
    /// The identity the next listing gets.
    next: u64,
}

/// One directory's listing, and the projects it holds.
#[derive(Debug)]
struct DirListing {
    id: ListingId,
    check: Check,
    /// The `.fsproj` entries, sorted ([`fsproj_entries`]).
    projects: Vec<PathBuf>,
    /// The generation that last confirmed it (or that listed it).
    confirmed_in: u64,
}

/// The identity of one listing a [`DirListings`] made: a directory listed
/// again gets a new one, so an owner can tell whether the listing it read
/// is the one that stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ListingId(u64);

impl DirListings {
    /// The `.fsproj`s in `dir`, sorted by name, and the listing they come
    /// from: inside a generation, the remembered listing when it still
    /// holds, and otherwise a new one (stamped before it is listed:
    /// [`ReadChecks::list`]), remembered. Outside a generation nothing
    /// confirms a listing, so `dir` is listed afresh and nothing is
    /// remembered.
    fn projects_in(&mut self, dir: &Path) -> (Vec<PathBuf>, ListingId) {
        let generation = self.checks.generation();
        if let Some(generation) = generation
            && let Some(id) = self.current(dir, generation)
        {
            return (self.by_dir[dir].projects.clone(), id);
        }
        let check = self.checks.list(dir);
        let projects = fsproj_entries(dir, &check.read().observation);
        let id = ListingId(self.next);
        self.next += 1;
        if let Some(generation) = generation {
            self.by_dir.insert(
                dir.to_path_buf(),
                DirListing {
                    id,
                    check,
                    projects: projects.clone(),
                    confirmed_in: generation,
                },
            );
        }
        (projects, id)
    }

    /// The remembered listing of `dir`, once confirmed for `generation`;
    /// `None` when there is none, or it no longer holds, in which case it is
    /// forgotten.
    fn current(&mut self, dir: &Path, generation: u64) -> Option<ListingId> {
        let listing = self.by_dir.get_mut(dir)?;
        if listing.confirmed_in != generation {
            if !self.checks.holds(&listing.check) {
                self.by_dir.remove(dir);
                return None;
            }
            listing.confirmed_in = generation;
        }
        Some(listing.id)
    }

    /// Forget every listing.
    fn forget(&mut self) {
        self.by_dir.clear();
    }
}

/// What [`evaluate_project`] produced and read.
struct Evaluation {
    evaluated: Option<EvaluatedProject>,
    /// Every file-system query the result depends on, with its answer: the
    /// `.fsproj` read and SDK discovery's, and each evaluation pass's own
    /// read set ([`ParsedProject::read_set`]). A pass that fails
    /// (`ParseError`) fails on the source text and the caller's arguments
    /// alone, so its reads are not needed.
    reads: ReadSet,
    /// Where the immutable contents its reads may include live.
    immutable: ImmutableRoots,
}

impl Evaluation {
    /// The confirming half of [`Self::checks_and_exempt`].
    #[cfg(test)]
    fn checks(&self, checker: &ReadChecks, committed: impl Fn(&Path) -> bool) -> Vec<Check> {
        self.checks_and_exempt(checker, committed).0
    }

    /// The reads a cached copy must confirm: every read, except those
    /// inside a version directory some read showed to exist
    /// ([`VersionDirs::containing`]), and for each such version, that it
    /// still does. A read inside a version no read showed to exist (an
    /// `Exists` probe of a version not installed) is confirmed like any
    /// other, since installing the version moves it.
    ///
    /// A package under the NuGet folder is extracted in place, and the
    /// extraction is complete only once it writes `.nupkg.metadata`; an
    /// evaluation that ran during one may have seen part of it. So a package
    /// version is trusted only where `committed` says its extraction was
    /// complete before the evaluation began ([`nuget_committed_before`]).
    ///
    /// Beside them, the reads it trusts instead, which a structural watch
    /// event makes the next confirmation cover too. Each is `checker`'s
    /// check of the read ([`ReadChecks::check`]), so a read some held check
    /// shares starts with its witness.
    fn checks_and_exempt(
        &self,
        checker: &ReadChecks,
        committed: impl Fn(&Path) -> bool,
    ) -> (Vec<Check>, Vec<Check>) {
        partition_reads(self.reads.reads(), &self.immutable, committed).checks(checker)
    }
}

/// A read set split by [`partition_reads`].
#[derive(Debug, PartialEq, Eq)]
struct Partition<'r> {
    /// The reads confirmed in every generation that reads the value.
    confirmed: Vec<&'r Read>,
    /// For each version directory whose contents are trusted, that it still
    /// exists: confirmed with `confirmed`.
    versions: Vec<Read>,
    /// The reads of trusted contents, confirmed only after a watch event.
    exempt: Vec<&'r Read>,
}

impl Partition<'_> {
    /// The confirmed reads with the version directories', and the exempt
    /// reads, each as `checker`'s check of it.
    fn checks(&self, checker: &ReadChecks) -> (Vec<Check>, Vec<Check>) {
        let confirmed = self
            .confirmed
            .iter()
            .copied()
            .chain(&self.versions)
            .map(|read| checker.check(read))
            .collect();
        let exempt = self.exempt.iter().map(|read| checker.check(read)).collect();
        (confirmed, exempt)
    }
}

/// [`Evaluation::checks_and_exempt`]' rule over any read set: every read is
/// confirmed, except those inside a version directory under `immutable`
/// that some read showed to exist (and, for a package version, that
/// `committed` says was extracted before the reads began), which are
/// exempt; and for each such version, that it still exists is confirmed.
/// `committed` is asked once per package version.
fn partition_reads<'r>(
    reads: &'r [Read],
    immutable: &ImmutableRoots,
    committed: impl Fn(&Path) -> bool,
) -> Partition<'r> {
    let versions = immutable.versions(reads);
    let is_package = |version: &Path| immutable.nuget.iter().any(|root| version.starts_with(root));
    // The version directory each read lies strictly inside, found once.
    let containing: Vec<Option<std::borrow::Cow<Path>>> = reads
        .iter()
        .map(|read| versions.containing(&read.path))
        .collect();
    // Each version some read was answered inside, once, keyed by its
    // spelling, which is the same for every read under one root.
    let mut verdicts: HashMap<&std::ffi::OsStr, bool> = HashMap::new();
    for (read, version) in reads.iter().zip(&containing) {
        if let Some(version) = version
            && answered(&read.observation)
            && !verdicts.contains_key(version.as_os_str())
        {
            let shown = !is_package(version) || committed(version);
            verdicts.insert(version.as_os_str(), shown);
        }
    }
    // What a version holds is immutable; an I/O error reading it is
    // not, so only an answer or an absence is trusted.
    let (exempt, confirmed): (Vec<_>, Vec<_>) =
        reads.iter().zip(&containing).partition(|(read, version)| {
            a_fact(&read.observation)
                && version
                    .as_deref()
                    .is_some_and(|version| verdicts.get(version.as_os_str()) == Some(&true))
        });
    let mut shown: Vec<&std::ffi::OsStr> = verdicts
        .iter()
        .filter(|(_, shown)| **shown)
        .map(|(version, _)| *version)
        .collect();
    shown.sort();
    Partition {
        confirmed: confirmed.into_iter().map(|(read, _)| read).collect(),
        versions: shown
            .into_iter()
            .map(|version| Read {
                // A read inside it succeeded, so it resolved to a directory.
                path: PathBuf::from(version),
                observation: Observation::Kind(Ok(msbuild_observed::EntryKind::Dir)),
            })
            .collect(),
        exempt: exempt.into_iter().map(|(read, _)| read).collect(),
    }
}

/// What a cached assembly env confirms before it is served again
/// ([`Workspace::env_checks`], [`Workspace::confirm_env`]), and what a
/// watch event must name to drop it at once ([`EnvChecks::named_by`]).
#[derive(Debug)]
pub struct EnvChecks {
    /// The [`ReadChecks`] the checks' witnesses belong to.
    checker: u64,
    /// The reads confirmed in every generation that reads the env.
    checks: Vec<Check>,
    /// The reads of installed contents the env trusts rather than confirms
    /// (D3's axiom), confirmed only after a watch event
    /// ([`Workspace::confirm_env`]'s `after_event`): an event is what covers
    /// an in-place change to an installed version.
    exempt: Vec<Check>,
    /// Every path whose stamp the resolution read (a DLL), as [`path_key`]
    /// spells it.
    stamped: HashSet<String>,
}

impl EnvChecks {
    /// The reads confirmed before the env is served again.
    pub fn reads(&self) -> impl Iterator<Item = &Read> {
        self.checks.iter().map(Check::read)
    }

    /// Whether a watch event naming `path`, as spelt, names a file whose
    /// stamp the env's resolution read: a DLL. What drops the env at once,
    /// inside a generation or out of one; it is the cover for a file system
    /// whose stamps do not move (D1's "not covered"), for which it can only
    /// match the spelling the reads used. A stamp is the one observation an
    /// event cannot have asked again: confirming it is a `stat`, which on
    /// such a file system answers as before. Every other read an event names
    /// is asked again in the next confirmation ([`Workspace::hint`]): a file
    /// is read again and compared, and a path's kind, canonical spelling and
    /// directory listing are asked again. Correctness elsewhere rests on
    /// confirmation, which an event widens to the installed contents
    /// ([`Workspace::confirm_env`]), whatever path it names.
    pub fn named_by(&self, path: &Path) -> bool {
        self.stamped.contains(&path_key(path))
    }
}

/// `path` as [`EnvChecks`] compares paths: lexically normal, and folded
/// where the platform folds case ([`crate::vfs`]'s buffer key).
fn path_key(path: &Path) -> String {
    crate::paths::path_dedup_key(&lexically_normalize(path))
}

/// Whether the package version at `version` under the NuGet folder was
/// extracted completely before `started`: its `.nupkg.metadata`, which NuGet
/// writes last, is there with a stamp no later than [`crate::vfs::RACY_WINDOW`]
/// before `started`. Never outside a generation, where there is no `started`.
fn nuget_committed_before(checker: &ReadChecks, version: &Path, started: Option<Nanos>) -> bool {
    started.is_some_and(|started| {
        checker
            .stamp(&version.join(".nupkg.metadata"))
            .is_some_and(|stamp| !stamp.is_racy(started))
    })
}

/// `total` less `part`, field by field.
fn minus(total: ReadCounts, part: ReadCounts) -> ReadCounts {
    ReadCounts {
        opens: total.opens - part.opens,
        stats: total.stats - part.stats,
        listings: total.listings - part.listings,
        canonicalizations: total.canonicalizations - part.canonicalizations,
    }
}

/// Whether a query got an answer, rather than failing.
fn answered(observation: &Observation) -> bool {
    match observation {
        Observation::Text(answer) => answer.is_ok(),
        Observation::Kind(answer) | Observation::LinkKind(answer) => answer.is_ok(),
        Observation::Listing(answer) => answer.is_ok(),
        Observation::Canonical(answer) => answer.is_ok(),
        Observation::Stamp(answer) => answer.is_ok(),
    }
}

/// The install roots whose versioned directories hold immutable contents
/// (`docs/incremental-lsp-plan.md`, D3): every .NET install root the
/// evaluation's SDK discovery consulted or its SDK resolved from, and the
/// NuGet global packages folder it resolves SDK references from.
#[derive(Clone, Debug, Default)]
struct ImmutableRoots {
    dotnet: Vec<PathBuf>,
    nuget: Vec<PathBuf>,
}

impl ImmutableRoots {
    /// The version directory `path` lies in or names, and how many
    /// components of `path` lie below it. The version directories are
    /// `<dotnet>/sdk/<version>`, `<dotnet>/packs/<pack>/<version>` and
    /// `<dotnet>/sdk-manifests/<band>/<manifest>/<version>` under a .NET
    /// install root, and `<id>/<version>` under the NuGet global packages
    /// folder; a version starts with an ASCII digit. `path` is taken
    /// lexically normal ([`lexically_normalize`]).
    fn version_of(&self, path: &Path) -> Option<(PathBuf, usize)> {
        // The names of `path` below `root`: the first four, and how many in
        // all; `None` unless `path` lies under `root` and every component
        // below it is a name.
        let (root, names, depth, below) = self.version_layout(|root| {
            let mut names = [std::ffi::OsStr::new(""); 4];
            let mut count = 0;
            for component in path.strip_prefix(root).ok()?.components() {
                let Component::Normal(name) = component else {
                    return None;
                };
                if let Some(slot) = names.get_mut(count) {
                    *slot = name;
                }
                count += 1;
            }
            Some((names, count))
        })?;
        let dir = names[..depth]
            .iter()
            .fold(root.to_path_buf(), |dir, name| dir.join(name));
        Some((dir, below))
    }

    /// [`Self::version_of`] for a path spelt as its components
    /// ([`spelt_as_components`]), under roots all so spelt: the same answer,
    /// found by comparing bytes, with no component parsed. The version
    /// directory is then a leading part of `path`, which is returned as the
    /// length of its spelling.
    fn version_of_spelt(&self, path: &[u8]) -> Option<(usize, usize)> {
        let (root, names, depth, below) = self.version_layout(|root| {
            let rest = below_spelt(path, root.as_os_str().as_encoded_bytes())?;
            let mut names = [std::ffi::OsStr::new(""); 4];
            let mut count = 0;
            for name in rest
                .split(|byte| *byte == b'/')
                .filter(|name| !name.is_empty())
            {
                if let Some(slot) = names.get_mut(count) {
                    *slot = name_of(name);
                }
                count += 1;
            }
            Some((names, count))
        })?;
        let root = root.as_os_str().len();
        // Each name with the separator before it; the root `/` ends in one.
        let names: usize = names[..depth].iter().map(|name| name.len() + 1).sum();
        Some((if root == 1 { names } else { root + names }, below))
    }

    /// The version directory a path lies in or names, given `below`, which
    /// says for a root the first four names of the path below it and how
    /// many there are ([`Self::version_of`]): the root it lies under, the
    /// names below the root, how many of them the version directory takes,
    /// and how many of the path's lie below it.
    fn version_layout<'p>(
        &self,
        below: impl Fn(&Path) -> Option<([&'p std::ffi::OsStr; 4], usize)>,
    ) -> Option<(&Path, [&'p std::ffi::OsStr; 4], usize, usize)> {
        let version = |name: &std::ffi::OsStr| {
            name.as_encoded_bytes()
                .first()
                .is_some_and(u8::is_ascii_digit)
        };
        for root in &self.dotnet {
            let Some((names, count)) = below(root) else {
                continue;
            };
            let depth = match names {
                [sdk, v, ..] if count >= 2 && sdk == "sdk" && version(v) => 2,
                [packs, _, v, _] if count >= 3 && packs == "packs" && version(v) => 3,
                [manifests, _, _, v]
                    if count >= 4 && manifests == "sdk-manifests" && version(v) =>
                {
                    4
                }
                _ => continue,
            };
            return Some((root, names, depth, count - depth));
        }
        for root in &self.nuget {
            let Some((names, count)) = below(root) else {
                continue;
            };
            if count >= 2 && version(names[1]) {
                return Some((root, names, 2, count - 2));
            }
        }
        None
    }

    /// Every version directory `reads` reach: those under the roots, and
    /// the spelling a canonicalisation in `reads` of a version directory
    /// itself resolved it to (an install whose version directories are
    /// symlinks into another tree, as Nix's combined installs are, is read
    /// through both spellings). Only the version directory's own
    /// canonicalisation names its other spelling: a symlink below a version
    /// can lead anywhere.
    fn versions<'r>(&self, reads: impl IntoIterator<Item = &'r Read>) -> VersionDirs<'_> {
        let spelt = |path: &Path| spelt_as_components(path.as_os_str().as_encoded_bytes());
        // A path is placed among the roots by its bytes only when they are
        // all spelt as their components too.
        let roots_spelt = self
            .dotnet
            .iter()
            .chain(&self.nuget)
            .all(|root| spelt(root));
        let aliases: Vec<PathBuf> = reads
            .into_iter()
            .filter_map(|read| {
                let Observation::Canonical(Ok(canonical)) = &read.observation else {
                    return None;
                };
                let bytes = read.path.as_os_str().as_encoded_bytes();
                let (dir, below) = if roots_spelt && spelt_as_components(bytes) {
                    let (len, below) = self.version_of_spelt(bytes)?;
                    (PathBuf::from(name_of(&bytes[..len])), below)
                } else {
                    self.version_of(&lexically_normalize(&read.path))?
                };
                let installed = self.dotnet.iter().any(|root| dir.starts_with(root));
                (below == 0 && installed && *canonical != dir).then(|| canonical.clone())
            })
            .collect();
        VersionDirs {
            roots: self,
            bytes: roots_spelt && aliases.iter().all(|alias| spelt(alias)),
            aliases,
        }
    }
}

/// The version directories an evaluation's reads reach ([`ImmutableRoots::versions`]).
struct VersionDirs<'a> {
    roots: &'a ImmutableRoots,
    aliases: Vec<PathBuf>,
    /// Every root and alias is spelt as its components
    /// ([`spelt_as_components`]), so a path so spelt is placed among them by
    /// its bytes.
    bytes: bool,
}

impl VersionDirs<'_> {
    /// The version directory a read of `path` lies strictly inside, if it
    /// does, which makes the read one of an immutable content: `path`, taken
    /// lexically normal, lies strictly inside a version directory. An
    /// installer writes a new version beside the old ones and never changes
    /// one in place, so whatever an evaluation saw inside one (a file's
    /// text, what a path names, a listing, an absence) stands for as long as
    /// the version does. The axiom covers what a version links to as well:
    /// a `..` below a version is taken lexically.
    ///
    /// What installing or removing a version does change is the listing of
    /// the directory holding the versions, and whether a version directory
    /// exists. Neither path lies strictly inside a version, so both are
    /// confirmed like any other read. So are the band and manifest
    /// directories of `sdk-manifests`, which a workload update writes into,
    /// and a version directory's canonical spelling, which an alias rests on.
    fn containing<'p>(&'p self, path: &'p Path) -> Option<std::borrow::Cow<'p, Path>> {
        use std::borrow::Cow;
        // Most reads are spelt as their components, and placing those by
        // their bytes saves parsing every root's and alias's components for
        // each; the version directory is then a leading part of the path.
        let bytes = path.as_os_str().as_encoded_bytes();
        if self.bytes && spelt_as_components(bytes) {
            if let Some((len, below)) = self.roots.version_of_spelt(bytes)
                && below > 0
            {
                return Some(Cow::Borrowed(Path::new(name_of(&bytes[..len]))));
            }
            return self
                .aliases
                .iter()
                .find(|alias| {
                    below_spelt(bytes, alias.as_os_str().as_encoded_bytes())
                        .is_some_and(|rest| !rest.is_empty())
                })
                .map(|alias| Cow::Borrowed(alias.as_path()));
        }
        // Without a `..`, a path compares by its components as its lexically
        // normal spelling does, so only a path with one is spelt again.
        let normal;
        let path = if path.components().any(|c| c == Component::ParentDir) {
            normal = lexically_normalize(path);
            normal.as_path()
        } else {
            path
        };
        if let Some((version, below)) = self.roots.version_of(path)
            && below > 0
        {
            return Some(Cow::Owned(version));
        }
        self.aliases
            .iter()
            .find(|alias| path.starts_with(alias) && path != alias.as_path())
            .map(|alias| Cow::Borrowed(alias.as_path()))
    }
}

/// Whether `path` is spelt as its components: on Unix, absolute, every
/// separator single, no `.` or `..` name and no trailing
/// separator (the root `/` alone excepted). A path so spelt lies under
/// another so spelt, component by component, exactly when its bytes start
/// with the other's and a separator or nothing follows ([`below_spelt`]).
fn spelt_as_components(path: &[u8]) -> bool {
    cfg!(unix)
        && path.first() == Some(&b'/')
        && (path.len() == 1
            || (path.last() != Some(&b'/')
                && path[1..]
                    .split(|byte| *byte == b'/')
                    .all(|name| !name.is_empty() && name != b"." && name != b"..")))
}

/// A name of a path spelt as its components ([`spelt_as_components`]), as
/// the bytes between two separators.
#[cfg(unix)]
fn name_of(name: &[u8]) -> &std::ffi::OsStr {
    std::os::unix::ffi::OsStrExt::from_bytes(name)
}

/// No path is spelt as its components off Unix ([`spelt_as_components`]).
#[cfg(not(unix))]
fn name_of(_: &[u8]) -> &std::ffi::OsStr {
    unreachable!("no path is spelt as its components off Unix")
}

/// What of `path` lies below `prefix`, both spelt as their components
/// ([`spelt_as_components`]): the names after the separator that follows
/// `prefix`, empty when the two are one path; `None` when `path` does not
/// lie under `prefix`.
fn below_spelt<'p>(path: &'p [u8], prefix: &[u8]) -> Option<&'p [u8]> {
    let rest = path.strip_prefix(prefix)?;
    if prefix == b"/" || rest.is_empty() {
        return Some(rest);
    }
    rest.strip_prefix(b"/")
}

/// The workspace's served-TFM verdict for an entry project
/// ([`Workspace::served_tfm_for_project`], fsproj 3.3d round 19). The
/// assembly-env layer keys its assets-target selection on this, and the
/// three states degrade differently — which is why the env consumes the
/// tri-state rather than the [`Option`] projection
/// ([`Workspace::target_framework_for_project`]):
///
/// - [`ServedTfm::Tfm`]: a trustworthy chosen TFM (caller-owned global,
///   trusted body singular, or first-declared) — select exactly that
///   assets target; a restore without it errors into the empty env rather
///   than serving a different TFM's assemblies (plan E6).
/// - [`ServedTfm::NoneDeclared`]: the project declares no TFM (or failed to
///   evaluate) — the restore is the *only* evidence, so requiring a
///   single-target restore and serving its sole target is sound.
/// - [`ServedTfm::Untrusted`]: the evaluation's TFM exists but its
///   provenance is untrusted. The evidence *conflicts*: an evaluated value
///   we declined to serve, and a restore that may lag it (assets files are
///   explicitly allowed to be stale). Nothing proves the real build's
///   target — the sole-target fallback could fold an unrelated TFM's
///   assemblies precisely when the declined guess was right — so the env
///   serves nothing (D5: under-resolve, never wrong).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ServedTfm {
    /// A trustworthy chosen TFM: select its assets target (E3/E6).
    Tfm(String),
    /// No TFM declared (or evaluation failed): single-target-restore
    /// fallback.
    NoneDeclared,
    /// A TFM whose provenance is untrusted: serve no assemblies at all.
    Untrusted,
}

impl ServedTfm {
    /// The TFM string when one is trustworthily chosen, else `None` — the
    /// shape consumers outside the assets-target selection want (e.g. the
    /// producer-TFM recovery, which is merely *absent* rather than
    /// dangerous under both TFM-less states).
    pub fn as_deref(&self) -> Option<&str> {
        match self {
            ServedTfm::Tfm(tfm) => Some(tfm),
            ServedTfm::NoneDeclared | ServedTfm::Untrusted => None,
        }
    }
}

impl Default for Workspace {
    /// Empty cache, empty env — no `$DOTNET_ROOT`, no `$PATH`, no
    /// `global.json` walk. Projects with `<Project Sdk="...">` evaluate
    /// as if the SDK were unavailable (the body's defines still
    /// surface; the SDK's do not). Production code should use
    /// [`Workspace::new`] so the LSP picks up the host's real SDK
    /// install.
    fn default() -> Self {
        Self::with_env(SdkDiscoveryEnv::default())
    }
}

impl Workspace {
    /// Construct a workspace whose SDK discovery uses the current
    /// process environment ([`SdkDiscoveryEnv::from_process_env`]).
    pub fn new() -> Self {
        Self::with_env(SdkDiscoveryEnv::from_process_env())
    }

    /// Construct a workspace with an explicit SDK discovery environment.
    /// Used by tests to avoid leaking host env vars into project
    /// evaluation; production code should use [`Workspace::new`].
    pub fn with_env(env: SdkDiscoveryEnv) -> Self {
        Self::with_env_and_extra_build_properties(env, HashMap::new())
    }

    /// Construct a workspace with explicit SDK discovery and additional
    /// MSBuild global properties. Runtime LSP code should usually use
    /// [`Workspace::new`]; this exists for corpus/integration harnesses that
    /// need to evaluate projects under a documented build profile.
    pub fn with_env_and_extra_build_properties(
        env: SdkDiscoveryEnv,
        extra_build_properties: HashMap<String, String>,
    ) -> Self {
        Self {
            projects: HashMap::new(),
            projects_generation: ProjectsGeneration(0),
            checker: ReadChecks::default(),
            env_counts: Cell::new(ReadCounts::default()),
            evaluated: Cell::new(0),
            retired: HashMap::new(),
            owners: HashMap::new(),
            dir_listings: DirListings::default(),
            next_evaluation: Cell::new(0),
            env,
            extra_build_properties,
        }
    }

    /// The SDK discovery environment in use. Borrowed by the fsproj-buffer
    /// diagnostic path (which needs to build its own `SdkDiscovery` for
    /// the entry project) so it sees the same `$DOTNET_ROOT`/`$PATH` the
    /// workspace's symbol resolution does. Without this accessor the
    /// two paths could diverge — a workspace constructed via
    /// [`Workspace::with_env`] for tests would still hit the host env
    /// when the fsproj diagnostics fired.
    pub fn env(&self) -> &SdkDiscoveryEnv {
        &self.env
    }

    /// The MSBuild build-global bag every project evaluation in this workspace
    /// starts from: the `dotnet build` defaults (`default_build_properties`)
    /// overlaid with whatever the caller pinned at construction.
    ///
    /// Exposed for the same reason as [`Workspace::env`], and for the same
    /// class of divergence (plan E7): the `.fsproj`-buffer diagnostic path
    /// evaluates the *buffer* rather than the file, so it cannot go through
    /// the project cache — but it must start from the identical globals, or a
    /// caller who pinned `Configuration=Release` sees it honoured in
    /// resolution and ignored in the squiggles.
    ///
    /// `TargetFramework` is deliberately absent: it is per-project, not
    /// workspace-global, and each surface seeds it from its own evaluation via
    /// `tfm_policy::tfm_choice`.
    pub fn build_properties(&self) -> HashMap<String, String> {
        build_properties(&self.extra_build_properties)
    }

    /// Drop every cached project evaluation and every remembered owner, so
    /// that each is evaluated (or walked) afresh on its next lookup, and end
    /// the [`Self::projects_generation`].
    ///
    /// For the one input of an evaluation that nothing records: NuGet's
    /// settings, which choose the packages folder SDK discovery resolves
    /// versioned SDKs from (`borzoi_nuget::settings`, read directly). A watch
    /// event naming a `nuget.config` is all that covers a change to them
    /// ([`crate::server::State::apply_watched_changes`]).
    pub fn drop_every_evaluation(&mut self) {
        for (project, dropped) in std::mem::take(&mut self.projects) {
            self.retire(project, dropped);
        }
        self.owners.clear();
        self.dir_listings.forget();
        self.projects_generation.0 += 1;
    }

    /// Hold the checks of `dropped`, the evaluation of `project` just
    /// dropped, until `project` is evaluated again ([`Self::retired`]).
    fn retire(&mut self, project: ProjectId, dropped: CachedEvaluation) {
        let mut checks = dropped.checks;
        checks.extend(dropped.exempt);
        self.retired.insert(project, checks);
    }

    /// A structural watch event arrived (a project file, an import, a
    /// `global.json`, an assets file): the next confirmation of every cached
    /// evaluation covers the installed contents it trusts as well as what it
    /// always confirms, and the [`Self::projects_generation`] ends, which
    /// tells a consumer reading evaluations across a span of time (the
    /// `workspace/diagnostic` sweep) that the disk may have moved under it.
    ///
    /// Nothing is dropped: an evaluation whose reads still get their answers
    /// is the one a fresh evaluation would give. Confirming the installed
    /// contents rather than matching the event's paths against them is what
    /// covers an in-place change inside an installed version (D3) under any
    /// spelling the client reports, as an env's confirmation does after any
    /// event ([`Self::confirm_env`]'s `after_event`).
    pub fn note_structural_event(&mut self) {
        for cached in self.projects.values_mut() {
            cached.event_since = true;
        }
        self.projects_generation.0 += 1;
    }

    /// Watch events named `paths`: ask what the evaluations, the envs and the
    /// ownership walks read at each again, whatever the stamps say
    /// ([`ReadChecks::hint`]). That covers, for the paths an event names, a
    /// file system whose stamps do not move.
    pub fn hint<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) {
        let paths: Vec<&Path> = paths.into_iter().collect();
        self.checker.hint(paths.iter().copied());
        self.dir_listings.checks.hint(paths.iter().copied());
    }

    /// The identity of the evaluation of `project` the workspace serves now,
    /// as last confirmed, without confirming it: `None` when none is cached.
    /// A value derived from an evaluation records the identity it was
    /// derived from, and stands only while this is it.
    pub fn evaluation_id(&self, project: &ProjectId) -> Option<EvaluationId> {
        self.projects.get(project).map(|cached| cached.id)
    }

    /// Which generation of the project-evaluation cache lookups are served
    /// from now. Two lookups observed under one generation read the same
    /// on-disk project state — the cache is the only path to an evaluation,
    /// and a generation ends whenever an evaluation it served is dropped (a
    /// validation that finds its reads moved, or
    /// [`Self::drop_every_evaluation`]), whenever a file's owner it served
    /// moves ([`Self::owning_project`]), and whenever a structural watch
    /// event arrives ([`Self::note_structural_event`]) — so a consumer
    /// accumulating facts across many lookups (the stepped
    /// `workspace/diagnostic` sweep) checks it to know its facts are
    /// mutually consistent.
    pub fn projects_generation(&self) -> ProjectsGeneration {
        self.projects_generation
    }

    /// Start a dispatch-loop generation at time `now`
    /// ([`crate::vfs`]): from now until [`Self::end_generation`], the first
    /// lookup of each cached evaluation confirms its reads against the disk,
    /// and every later lookup in the generation trusts that answer.
    ///
    /// Outside a generation (the server's work before its loop starts, and
    /// code that drives the workspace directly, as unit tests do) a cached
    /// evaluation is served as it is.
    pub fn begin_generation(&mut self, now: Nanos) {
        self.checker.begin_generation(now);
        self.dir_listings.checks.begin_generation(now);
        self.evaluated.set(0);
        self.env_counts.set(ReadCounts::default());
    }

    /// End the generation, returning what reading evaluations, finding the
    /// projects that own files and confirming assembly envs cost it.
    pub fn end_generation(&mut self) -> WorkspaceCounts {
        let envs = self.env_counts.take();
        WorkspaceCounts {
            evaluations: EvaluationCounts {
                evaluated: self.evaluated.get(),
                validation: minus(self.checker.end_generation(), envs),
            },
            ownership: self.dir_listings.checks.end_generation(),
            envs,
        }
    }

    /// Whether `checks` were made by this workspace's checker, whose
    /// directories their witnesses name: only then do they hold through
    /// [`Self::confirm_env`], or does a generation number of this
    /// workspace's say anything about them.
    pub fn owns(&self, checks: &EnvChecks) -> bool {
        checks.checker == self.checker.id()
    }

    /// The open generation's number, or `None` between generations: a
    /// value confirmed in it ([`Self::confirm_env`]) need not be confirmed
    /// again until the next.
    pub fn generation(&self) -> Option<u64> {
        self.checker.generation()
    }

    /// The open generation's time, or `None` between generations. Read
    /// before a computation whose reads [`Self::env_checks`] will turn into
    /// checks: a package it read is trusted only if it was complete before
    /// then.
    pub fn generation_time(&self) -> Option<Nanos> {
        self.checker.now()
    }

    /// What an assembly env resolved for `project` must confirm before it is
    /// served again, given what resolving it read (`reads`) from the
    /// generation time `started` ([`Self::generation_time`]): every read
    /// except the contents of the installed versions it trusts, by the rule
    /// a cached evaluation follows (`Evaluation::checks_and_exempt`). The installed
    /// versions are those under the dotnet root the env was resolved
    /// against, under every package folder its assets file names
    /// (`package_folders`: NuGet extracts a package version into
    /// `<id>/<version>/` there and never changes it in place, whichever
    /// settings chose the folder), and under every root and the NuGet folder
    /// `project`'s cached evaluation names, if there is one.
    pub fn env_checks(
        &self,
        project: &ProjectId,
        dotnet_root: Option<&Path>,
        package_folders: &[PathBuf],
        reads: &ReadSet,
        started: Option<Nanos>,
    ) -> EnvChecks {
        let mut immutable = self
            .projects
            .get(project)
            .map(|cached| cached.immutable.clone())
            .unwrap_or_default();
        immutable
            .dotnet
            .extend(dotnet_root.map(lexically_normalize));
        immutable.nuget.extend(
            package_folders
                .iter()
                .filter(|folder| folder.is_absolute())
                .map(|folder| lexically_normalize(folder)),
        );
        let checker = &self.checker;
        let reads = reads.reads();
        // The commit markers' stamps are the env's to pay for, not the
        // evaluations'.
        let before = checker.counts_so_far();
        let partition = partition_reads(reads, &immutable, |version| {
            nuget_committed_before(checker, version, started)
        });
        if checker.generation().is_some() {
            let spent = minus(checker.counts_so_far(), before);
            self.env_counts.set(self.env_counts.get() + spent);
        }
        let (checks, exempt) = partition.checks(checker);
        EnvChecks {
            checker: checker.id(),
            exempt,
            checks,
            stamped: reads
                .iter()
                .filter(|read| matches!(read.observation, Observation::Stamp(_)))
                .map(|read| path_key(&read.path))
                .collect(),
        }
    }

    /// Whether every read `checks` names still gets its recorded answer, as
    /// of the open generation: confirmed through the checker the
    /// evaluations' reads are, so what both read is asked once. Outside a
    /// generation nothing is confirmed, and an env is served as it is, as a
    /// cached evaluation is. Inside one, checks this workspace did not make
    /// never hold.
    ///
    /// `after_event`: a watch event has arrived since the env was last
    /// confirmed, so the installed contents it trusts are confirmed too. An
    /// installer never changes an installed version in place (D3), and a
    /// watch event is what covers one that does; confirming them, rather
    /// than matching the event's path to them, leaves the spelling the
    /// event used (another case, a symlink, a separator) out of it.
    pub fn confirm_env(&self, checks: &EnvChecks, after_event: bool) -> bool {
        if self.checker.generation().is_none() {
            return true;
        }
        if !self.owns(checks) {
            return false;
        }
        let before = self.checker.counts_so_far();
        let holds = checks.checks.iter().all(|check| self.checker.holds(check))
            && (!after_event || checks.exempt.iter().all(|check| self.checker.holds(check)));
        let spent = minus(self.checker.counts_so_far(), before);
        self.env_counts.set(self.env_counts.get() + spent);
        holds
    }

    /// Confirm the cached evaluation of `project` for the open generation:
    /// every read it depends on (bar the immutable contents it was told it
    /// may skip, see `Evaluation::checks_and_exempt`, unless a structural watch event
    /// has arrived since it was last confirmed) is asked again, once per
    /// generation, and an entry any of whose answers moved is dropped, so the
    /// lookup that follows evaluates afresh. Dropping it ends the
    /// [`Self::projects_generation`]. Nothing happens outside a generation, or
    /// for a project not cached.
    pub fn validate(&mut self, project: &ProjectId) {
        let Some(generation) = self.checker.generation() else {
            return;
        };
        let Some(cached) = self.projects.get_mut(project) else {
            return;
        };
        if cached.validated_in == Some(generation) && !cached.event_since {
            return;
        }
        let checker = &self.checker;
        let holds = cached.checks.iter().all(|check| checker.holds(check))
            && (!cached.event_since || cached.exempt.iter().all(|check| checker.holds(check)));
        if holds {
            cached.validated_in = Some(generation);
            cached.event_since = false;
        } else {
            tracing::info!(
                project = %project.as_path().display(),
                "a project evaluation's inputs moved; evaluating it again"
            );
            if let Some(dropped) = self.projects.remove(project) {
                self.retire(project.clone(), dropped);
            }
            self.projects_generation.0 += 1;
        }
    }

    /// The target framework the project at `project_path` was evaluated
    /// under, or `None` when it declares no TFM or failed to evaluate. This
    /// is the **single source of truth** for the served-TFM policy (fsproj
    /// 3.3c, plan E5): the parse side agrees by construction (the evaluation
    /// itself was seeded with this value), and the assembly env must key its
    /// assets-target selection on it so a project's files are never parsed
    /// under one TFM's defines while its types resolve against another TFM's
    /// assemblies.
    ///
    /// A TFM whose provenance is untrusted reads as `None` here; consumers
    /// that must distinguish that decline from "no TFM declared" (the
    /// assembly-env layer — the two degrade differently) use
    /// [`Workspace::served_tfm_for_project`] instead, of which this is the
    /// two-state projection.
    pub fn target_framework_for_project(&mut self, project_path: &Path) -> Option<String> {
        match self.served_tfm_for_project(project_path) {
            ServedTfm::Tfm(tfm) => Some(tfm),
            ServedTfm::NoneDeclared | ServedTfm::Untrusted => None,
        }
    }

    /// The target framework the project's evaluation actually **ran under** —
    /// the one its `DefineConstants` and Compile items came from.
    ///
    /// Distinct from [`Workspace::served_tfm_for_project`], and the distinction
    /// is load-bearing. That one answers "may the *assembly env* key assets
    /// selection on this?", and declines an untrusted TFM outright; this one
    /// answers "which inner build did we parse?", which has an answer even
    /// then, because the parse still had to pick something (the untrusted arm
    /// keeps pass 1 and lets each read flip its own `*_uncertain` flag). The
    /// two agree whenever the verdict is [`ServedTfm::Tfm`].
    ///
    /// This is the currency the *parse-side* coherence invariant is stated in
    /// (plan E7): the `.fsproj`-buffer diagnostics must describe this
    /// evaluation, not the publish-side verdict — see
    /// `buffer_diagnostics_follow_the_workspace_parsed_tfm`.
    pub fn parsed_tfm_for_project(&mut self, project_path: &Path) -> Option<String> {
        self.evaluated(project_path)
            .and_then(|e| e.chosen_tfm.clone())
    }

    /// The full served-TFM verdict for the project at `project_path` — the
    /// input the assembly-env layer keys its assets-target selection on
    /// (fsproj 3.3d round 19). [`ServedTfm`] documents the three states and
    /// their degradation semantics; the untrusted arm exists because the
    /// real build may select a different TFM than the evaluated one
    /// (`EvaluatedProject::tfm_untrusted` — a body `TargetFramework` written
    /// under a gate the evaluator couldn't pin), so serving the evaluated
    /// value would let the env pick that TFM's assets target on trust (and
    /// seed the whole producer-TFM recovery from it). A caller-supplied
    /// non-empty `TargetFramework` global is immune — the caller owns the
    /// choice and its value needs no provenance, the same ordering
    /// `resolve_node_uncached` applies to graph nodes.
    ///
    /// Coherence with the parses survives observably: any parse input that
    /// leans on the untrusted `TargetFramework` has already flipped its own
    /// uncertainty flag (`define_constants_uncertain` / `items_uncertain`),
    /// refusing the fold outright.
    pub fn served_tfm_for_project(&mut self, project_path: &Path) -> ServedTfm {
        let caller_owns_tfm = caller_owns_target_framework(&self.extra_build_properties);
        match self.evaluated(project_path) {
            // A failed evaluation has no TFM evidence at all — same
            // degradation as "no TFM declared" (the pre-3.3c behaviour).
            None => ServedTfm::NoneDeclared,
            Some(e) => {
                // The caller-owned immunity is about *provenance* — the
                // caller's value needs none. It cannot extend to an evaluation
                // the document was free to conduct under a different TFM than
                // the one supplied, so `not_an_inner_build` declines either way.
                if e.not_an_inner_build {
                    return ServedTfm::Untrusted;
                }
                if e.tfm_untrusted && !caller_owns_tfm {
                    return ServedTfm::Untrusted;
                }
                match &e.chosen_tfm {
                    Some(tfm) => ServedTfm::Tfm(tfm.clone()),
                    None => ServedTfm::NoneDeclared,
                }
            }
        }
    }

    /// The dotnet **install root** (the directory containing `sdk/` and
    /// `packs/`) that the fsproj evaluator actually resolved this project's
    /// SDK from. The semantic layer's assembly env looks up framework packs
    /// under it, so it must be the install the project really built against.
    ///
    /// **Single source of truth.** Evaluating the project records the install
    /// root of the SDK its `<Project Sdk=...>` resolved to (the cached
    /// `EvaluatedProject::sdk_install_root`); we return that recorded root
    /// rather than re-deriving one. Re-deriving — probing a bare
    /// `Microsoft.NET.Sdk` — is what the old implementation did, and in a
    /// multi-root `global.json` `sdk.paths` workspace it can land on a
    /// *different* root than the project's actual SDK (a differently-named or
    /// differently-versioned SDK living under another root), silently pointing
    /// framework resolution at the wrong install.
    ///
    /// Falls back to `probe_dotnet_root` only when evaluation recorded no SDK
    /// — a project with no `<Project Sdk=...>`, or one whose resolver was
    /// disabled / failed (offline, no `$DOTNET_ROOT`). `None` when even the
    /// fallback can't name a root.
    pub fn dotnet_root_for_project(&mut self, project_path: &Path) -> Option<PathBuf> {
        self.dotnet_root_and_reads_for_project(project_path).0
    }

    /// [`Self::dotnet_root_for_project`], with what choosing it read beyond
    /// the project's evaluation (whose reads its consumers record already):
    /// the probe's queries, for a project that records no SDK root. An env
    /// keyed on the root records them (`SemanticState`'s
    /// `cached_assembly_env`), so a cached fold whose fast path never asks
    /// for the root again still notices when it would move.
    pub fn dotnet_root_and_reads_for_project(
        &mut self,
        project_path: &Path,
    ) -> (Option<PathBuf>, ReadSet) {
        if let Some(root) = self
            .evaluated(project_path)
            .and_then(|e| e.sdk_install_root.clone())
        {
            return (Some(root), ReadSet::default());
        }
        let observer = Observer::new();
        let root = self.probe_dotnet_root(project_path, &observer);
        (root, observer.finish().0)
    }

    /// Best-effort install-root guess for a project that recorded no SDK
    /// resolution (no `<Project Sdk=...>`, or evaluation failed). Probes a
    /// bare `Microsoft.NET.Sdk` — present in every modern .NET install — and
    /// recovers the install root from the returned `SdkPaths::root` (the *SDK
    /// import directory*: layout `<install_root>/sdk/<ver>/Sdks/<name>/Sdk`,
    /// so the install root is five `parent()` calls up). Falls through to
    /// `roots().first()` when probing fails, then `None`.
    ///
    /// This is a guess, used only when the authoritative recorded root is
    /// absent — when an SDK *was* resolved, [`Self::dotnet_root_for_project`]
    /// uses that instead, so the probe's multi-root ambiguity never bites.
    fn probe_dotnet_root(&self, project_path: &Path, observer: &Observer) -> Option<PathBuf> {
        let disc = SdkDiscovery::for_project(project_path, &self.env, observer).ok()?;
        if let Ok(SdkResolution::Single(paths)) = disc.resolve("Microsoft.NET.Sdk", observer)
            && let Some(install_root) = install_root_from_sdk_path(&paths.root)
        {
            return Some(install_root.to_path_buf());
        }
        disc.roots().first().cloned()
    }

    /// The active preprocessor symbol set for `file`.
    ///
    /// - A script (`.fsx`) gets the script implicit set (`INTERACTIVE`+
    ///   `EDITING`) — FCS resolves scripts by extension and applies no sibling
    ///   project's options — *unless* a project **conclusively** compiles it
    ///   (an explicit `<Compile Include="…fsx">`, which the SDK never globs), in
    ///   which case it gets that project's compiled set. Mere proximity to an
    ///   `.fsproj` does not count: `owning_project`'s nearest-ancestor fallback
    ///   would otherwise claim a standalone script.
    /// - A compiled file (`.fs`/`.fsi`) under a project gets that project's set
    ///   ([`Self::symbols_for_project`]): `COMPILED`+`EDITING` plus its
    ///   `$(DefineConstants)` (best-effort, so a not-yet-listed `.fs` still
    ///   picks up the nearest project's defines). An orphan compiled file, or
    ///   one whose owning `.fsproj` failed to evaluate, gets the compiled base.
    pub fn symbols_for(&mut self, file: &Path) -> HashSet<String> {
        if is_script_path(file) {
            if let Some(project_path) = self.compiling_project(file) {
                return self.symbols_for_project(&project_path);
            }
            return implicit_symbols(true);
        }
        match self.owning_project(file) {
            Some(project_path) => self.symbols_for_project(&project_path),
            None => implicit_symbols(false),
        }
    }

    /// The project whose *declines* affect `file` — what
    /// [`crate::project_deferral`] reports on.
    ///
    /// Looser than [`Self::compiling_project`], and deliberately: that one asks
    /// "may I serve this script under the project's settings?", which needs a
    /// conclusive `Member`. This asks "will the handlers try to serve this file
    /// through that project, and could they fail?" — and they will, for anything
    /// the ancestor walk selects. The one exclusion is a definite `NotMember`
    /// for a script, which is a standalone `.fsx` sitting under an unrelated
    /// `.fsproj`: telling it that project's problems would be nonsense.
    ///
    /// An *inconclusive* membership (`Unknown`, from an `items_uncertain`
    /// project) therefore stays in scope. That is exactly the case where the
    /// handlers do select the project and `build_parses` then refuses on the
    /// same uncertainty, so it is the case that most needs explaining.
    pub fn reporting_project(&mut self, file: &Path) -> Option<PathBuf> {
        let project_path = self.owning_project(file)?;
        if is_script_path(file)
            && matches!(self.membership(&project_path, file), Membership::NotMember)
        {
            return None;
        }
        Some(project_path)
    }

    /// The project whose settings `file` is served under, or `None` when it is
    /// served standalone.
    ///
    /// This is [`Self::owning_project`] with the **script guard**: a `.fsx` is
    /// claimed only by a project that *conclusively* compiles it (an explicit
    /// `<Compile Include="…fsx">`, which the SDK never globs). Mere proximity
    /// does not count — `owning_project`'s nearest-ancestor fallback would
    /// otherwise hand a standalone script to whichever `.fsproj` happens to sit
    /// up-tree, and then serve it that project's defines, its language version,
    /// and (via [`crate::project_deferral`]) *its* problems, none of which have
    /// anything to do with the script.
    ///
    /// Every caller that answers "what does this file belong to?" should use
    /// this rather than `owning_project`, which answers the narrower "which
    /// project's directory encloses it?".
    pub fn compiling_project(&mut self, file: &Path) -> Option<PathBuf> {
        let project_path = self.owning_project(file)?;
        if is_script_path(file)
            && !matches!(self.membership(&project_path, file), Membership::Member)
        {
            return None;
        }
        Some(project_path)
    }

    /// [`Self::symbols_for`], refined by a *linking project* the caller knows
    /// enumerates `file` in its `<Compile>` list (the `workspace/diagnostic`
    /// sweep reads each file out of a project it has just evaluated, so the
    /// owner is in hand even when `file` sits outside the project's directory
    /// and [`Self::owning_project`]'s ancestor walk cannot find it).
    ///
    /// Precedence:
    ///
    /// 1. A **conclusive ancestor owner** (a Member verdict from the walk)
    ///    wins, so the answer agrees with plain [`Self::symbols_for`] — and
    ///    with `textDocument/diagnostic` — wherever that already resolves
    ///    ownership.
    /// 2. Otherwise, a **conclusive membership in `linking_project`** wins:
    ///    its evaluated `<Compile>` list contains `file` and is trustworthy
    ///    (`!items_uncertain`), so its `DefineConstants` govern. An uncertain
    ///    or failed evaluation proves nothing (the listed item may be stale
    ///    under an unapplied `<Compile Remove>`) and donates nothing.
    /// 3. Otherwise, fall back to [`Self::symbols_for`]'s heuristic exactly.
    pub fn symbols_for_linked(&mut self, file: &Path, linking_project: &Path) -> HashSet<String> {
        match self.linked_owner(file, linking_project) {
            Some(project_path) => self.symbols_for_project(&project_path),
            None => self.symbols_for(file),
        }
    }

    /// [`Self::lang_version_for`], refined by a linking project — the same
    /// precedence as [`Self::symbols_for_linked`], so a linked file's `#if`
    /// branches and its language-version feature gates come from the same
    /// project.
    pub fn lang_version_for_linked(
        &mut self,
        file: &Path,
        linking_project: &Path,
    ) -> LanguageVersion {
        match self.linked_owner(file, linking_project) {
            Some(project_path) => self.lang_version_for_project(&project_path),
            None => self.lang_version_for(file),
        }
    }

    /// The project whose settings govern `file` given that `linking_project`
    /// enumerates it: `Some(linking_project)` exactly when no ancestor project
    /// conclusively owns `file` **and** `linking_project`'s membership verdict
    /// is [`Membership::Member`]. `None` means "defer to the plain per-file
    /// resolution" — either because the ancestor walk already found a
    /// conclusive owner (which must win, for agreement with the non-linked
    /// paths) or because the linking project's item list proves nothing.
    pub(crate) fn linked_owner(&mut self, file: &Path, linking_project: &Path) -> Option<PathBuf> {
        if let Some(owner) = self.owning_project(file)
            && matches!(self.membership(&owner, file), Membership::Member)
        {
            return None;
        }
        matches!(self.membership(linking_project, file), Membership::Member)
            .then(|| linking_project.to_path_buf())
    }

    /// The active preprocessor symbol set for the project at `project_path` —
    /// `{COMPILED, EDITING} ∪ DefineConstants`. Cached the same way
    /// [`Self::symbols_for`] is. Returns the implicit `{COMPILED, EDITING}` set
    /// when the project failed to evaluate.
    ///
    /// `<Compile>` items are compiled `.fs`/`.fsi`, never scripts, so the base
    /// is the non-script implicit set.
    ///
    /// Used by the project-fold path (`semantic` module), which already knows
    /// the owning project and only needs the symbol set to parse each
    /// `<Compile>` file under the right `#if` branches.
    pub fn symbols_for_project(&mut self, project_path: &Path) -> HashSet<String> {
        let mut symbols = implicit_symbols(false);
        self.extend_with_define_constants(project_path, &mut symbols);
        symbols
    }

    /// The F# language version for `file`, mirroring [`Self::symbols_for`]: a
    /// file with an owning project gets that project's version
    /// ([`Self::lang_version_for_project`]); an orphan — no owning project, or a
    /// standalone script — gets [`LanguageVersion::Preview`] (every feature on).
    ///
    /// The orphan default is deliberately the *permissive* one: with no project
    /// context we cannot know the intended version, so we decline to guess-flag
    /// (e.g. report a spurious `#elif` feature error on a scratch buffer). A file
    /// that *does* resolve to a project, by contrast, gets that project's version
    /// — defaulting to FCS's 10.0 when the project sets none, which correctly
    /// flags an `#elif` in a project that never opted into F# 11.
    pub fn lang_version_for(&mut self, file: &Path) -> LanguageVersion {
        if is_script_path(file) {
            if let Some(project_path) = self.owning_project(file)
                && matches!(self.membership(&project_path, file), Membership::Member)
            {
                return self.lang_version_for_project(&project_path);
            }
            return LanguageVersion::Preview;
        }
        match self.owning_project(file) {
            Some(project_path) => self.lang_version_for_project(&project_path),
            None => LanguageVersion::Preview,
        }
    }

    /// The F# language version the project at `project_path` selects, for the
    /// language-version feature gate (today: `#elif`). Resolves the evaluated
    /// `<LangVersion>` via [`LanguageVersion::from_lang_version_text`]; an unset
    /// value falls back to [`LanguageVersion::DEFAULT`] (FCS's default, 10.0), as
    /// does an unrecognised one (logged). A project that failed to evaluate also
    /// yields `DEFAULT`.
    ///
    /// **Provenance is deliberately not consulted here** (unlike
    /// [`Workspace::served_tfm_for_project`], 3.3d round 19): an untrusted
    /// `LangVersion` has no safe fallback to decline *to*. No version is
    /// uniformly permissive — `Preview` raises the strict-indentation and
    /// invalid-decls-in-types severities
    /// (`StrictIndentation` / `ErrorOnInvalidDeclsInTypeDefinitions`, see
    /// [`LanguageFeature`](borzoi_cst::language_version::LanguageFeature)), and
    /// `DEFAULT` discards
    /// an evaluated value that is still the best single guess (the gate the
    /// walker couldn't pin usually *does* run). Untrusted provenance is the
    /// exception rather than the rule: an ordinary SDK project's is trusted,
    /// because the SDK's own `LangVersion` writes sit under gates the walker
    /// decides (`sdk_project_fold_e2e::ordinary_sdk_projects_have_trusted_langversion_provenance`),
    /// so it marks a project whose own (or imported) write sits under a gate
    /// the walker cannot pin. The version does shape the *token
    /// stream* (the lex-filter's `relaxWhitespace2` pops at F# 6 and its
    /// strict-indentation push decision at F# 8); the cross-file fold
    /// covers that exactly, refusing only untrusted provenance × a file
    /// whose parse shape provably depends on the version
    /// (`Parse::shape_depends_on_language_version` in `build_parses`), while
    /// this accessor keeps serving the best-guess value to the single-file
    /// surface.
    pub fn lang_version_for_project(&mut self, project_path: &Path) -> LanguageVersion {
        let Some(parsed) = self.project(project_path) else {
            return LanguageVersion::DEFAULT;
        };
        let Some(text) = parsed.lang_version.as_deref() else {
            return LanguageVersion::DEFAULT;
        };
        match LanguageVersion::from_lang_version_text(text) {
            Some(v) => v,
            None => {
                crate::log_warn!(
                    "unrecognised <LangVersion>; parsing as the default version",
                    value = text,
                    project = project_path.display(),
                    default_version = LanguageVersion::DEFAULT
                );
                LanguageVersion::DEFAULT
            }
        }
    }

    /// Add the project's evaluated `$(DefineConstants)` to `symbols`. A no-op if
    /// the project at `project_path` failed to evaluate (its failure is cached).
    fn extend_with_define_constants(&mut self, project_path: &Path, symbols: &mut HashSet<String>) {
        if let Some(parsed) = self.project(project_path) {
            for sym in &parsed.define_constants {
                symbols.insert(sym.clone());
            }
        }
    }

    /// The `.fsproj` that owns `file`, preferring **membership over
    /// proximity**. Climb `file`'s ancestor directories nearest-first; the
    /// first project whose evaluated `<Compile>` list contains `file` wins, so
    /// a project higher up that *links* a lower file (e.g.
    /// `<Compile Include="src/sub/Foo.fs"/>` in a repo-root project) can claim
    /// it when the nearer directory's project does not list it. When several
    /// ancestor projects list `file` (rare), the closest one wins.
    ///
    /// Membership is only *conclusive* for a project that evaluated
    /// **completely**. A project we could only evaluate partially (unresolved
    /// SDK/imports, `TargetFramework`-gated items, …) or not at all yields
    /// `Membership::Unknown`: its `items` list is unreliable in both directions
    /// (a real item may have been dropped, and a listed one may be stale), so
    /// it neither proves nor disproves ownership. We never climb past a
    /// directory holding such a project to hand ownership to a farther one,
    /// nor let its listed items claim the file — we stop and fall back to
    /// `find_owning_project`'s nearest-ancestor, alphabetically-first rule,
    /// exactly as the pre-refinement heuristic would.
    ///
    /// When no ancestor project conclusively lists `file` (e.g. a brand-new
    /// file not yet added to its project), the same fallback applies. This
    /// therefore only ever *refines* the heuristic's pick — it never returns
    /// `None` where the heuristic returned `Some`.
    ///
    /// The climb stops at the first listing (or inconclusive) directory, so
    /// the common case (a file its own project lists) reads no further than
    /// that project; only a file every ancestor *completely* excludes pays the
    /// full walk to the filesystem root. Evaluations populate the project
    /// cache as a side effect, so the immediate `symbols_for` follow-up is a
    /// cache hit.
    ///
    /// Inside a generation the answer is remembered per file, with the
    /// listings the walk read and the evaluations it consulted
    /// (`docs/incremental-lsp-plan.md`, D4). The first ask in each later
    /// generation confirms it: every listing by its directory's stamp
    /// ([`ReadChecks::list`]), and every consulted evaluation by its own
    /// validation and its identity. One that moved is walked again, and a
    /// walk that finds another owner than before ends the
    /// [`Self::projects_generation`], since what was derived from the old
    /// owner (what the client is shown for the file) is stale. Outside a
    /// generation every ask walks.
    ///
    /// Out of scope: a project that links `file` but is *not* an ancestor
    /// directory (a sibling linking a shared file). Closing it *in general*
    /// needs a workspace-wide project index, which was explored and shelved
    /// (`docs/workspace-index-plan.md` has the post-mortem and the current
    /// status). Where a caller already holds the linking project in hand —
    /// the `workspace/diagnostic` sweep, which enumerates each file from a
    /// just-evaluated project — [`Self::symbols_for_linked`] refines the
    /// answer; every other path degrades such a file to the implicit symbol
    /// set for its kind.
    pub fn owning_project(&mut self, file: &Path) -> Option<PathBuf> {
        never_superseded(self.owning_project_watched(file, &Watch::unwatched()))
    }

    /// [`Self::owning_project`], stopping at a checkpoint before each
    /// evaluation it may run (each project a remembered answer consulted,
    /// each candidate the walk weighs) once `watch` sees a notification
    /// ([`crate::inbox`]). The evaluations run by then stay cached; a
    /// remembered answer the walk was replacing is kept, so the next walk
    /// still notices the owner moved.
    pub fn owning_project_watched(
        &mut self,
        file: &Path,
        watch: &Watch,
    ) -> Result<Option<PathBuf>, Superseded> {
        let Some(generation) = self.dir_listings.checks.generation() else {
            // Outside a generation nothing confirms an answer, so nothing
            // is remembered.
            return Ok(self.walk_for_owner(file, 0, watch)?.project);
        };
        // The answer the last walk found, when it no longer stands.
        let moved_from = if self.owners.contains_key(file) {
            if self.owner_holds(file, generation, watch)? {
                return Ok(self.owners[file].project.clone());
            }
            self.owners.remove(file)
        } else {
            None
        };
        let walked = match self.walk_for_owner(file, generation, watch) {
            Ok(walked) => walked,
            Err(Superseded) => {
                if let Some(before) = moved_from {
                    self.owners.insert(file.to_path_buf(), before);
                }
                return Err(Superseded);
            }
        };
        let moved_from = moved_from.map(|owner| owner.project);
        let project = walked.project.clone();
        if moved_from.is_some_and(|before| before != project) {
            // What the client was shown for this file came from another
            // project: everything derived from the old answer is stale.
            tracing::info!(
                file = %file.display(),
                "a file's owning project moved; dropping what was derived from it"
            );
            self.projects_generation.0 += 1;
        }
        self.owners.insert(file.to_path_buf(), walked);
        Ok(project)
    }

    /// Whether the remembered owner of `file` still stands in `generation`:
    /// confirmed in it already, or every listing its walk read holds and
    /// every evaluation it consulted is still the entry cached, once
    /// confirmed for this generation ([`Self::validate`]).
    fn owner_holds(
        &mut self,
        file: &Path,
        generation: u64,
        watch: &Watch,
    ) -> Result<bool, Superseded> {
        let Some(owner) = self.owners.get(file) else {
            return Ok(false);
        };
        if owner.validated_in == generation {
            return Ok(true);
        }
        let listings = &mut self.dir_listings;
        if !owner
            .listings
            .iter()
            .all(|(dir, id)| listings.current(dir, generation) == Some(*id))
        {
            return Ok(false);
        }
        let consulted = owner.consulted.clone();
        for (project, id) in &consulted {
            // Confirming may evaluate the project again.
            watch.check()?;
            self.validate(project);
            if self.projects.get(project).map(|cached| cached.id) != Some(*id) {
                return Ok(false);
            }
        }
        if let Some(owner) = self.owners.get_mut(file) {
            owner.validated_in = generation;
        }
        Ok(true)
    }

    /// The ownership walk itself: [`Self::owning_project`]'s rule, reading
    /// every directory's listing through [`Self::dir_listings`] and
    /// recording each evaluation whose verdict it consulted.
    fn walk_for_owner(
        &mut self,
        file: &Path,
        generation: u64,
        watch: &Watch,
    ) -> Result<Owner, Superseded> {
        let mut listings = Vec::new();
        let mut consulted = Vec::new();
        let mut dir = file.parent();
        let mut owner = None;
        'climb: while let Some(d) = dir {
            let mut inconclusive = false;
            let (projects, listing) = self.dir_listings.projects_in(d);
            listings.push((d.to_path_buf(), listing));
            for cand in projects {
                // Weighing a candidate may evaluate it.
                watch.check()?;
                let project = ProjectId::new(&cand);
                let membership = self.membership(&cand, file);
                if let Some(cached) = self.projects.get(&project) {
                    consulted.push((project, cached.id));
                }
                match membership {
                    Membership::Member => {
                        owner = Some(Some(cand));
                        break 'climb;
                    }
                    Membership::Unknown => inconclusive = true,
                    Membership::NotMember => {}
                }
            }
            // No conclusive member here. If any project in this directory was
            // inconclusive, we can't safely prefer a farther project over it,
            // so stop refining and defer to the heuristic.
            if inconclusive {
                break;
            }
            dir = d.parent();
        }
        let project = match owner {
            Some(member) => member,
            None => find_owning_project(file, &mut self.dir_listings, &mut listings),
        };
        Ok(Owner {
            project,
            listings,
            consulted,
            validated_in: generation,
        })
    }

    /// The evaluated project at `project_path`, lazily computed and cached
    /// (keyed by [`ProjectId`]). `None` when evaluation failed; the
    /// failure is cached too, so we don't re-read on every keystroke.
    pub fn project(&mut self, project_path: &Path) -> Option<&ParsedProject> {
        self.evaluated(project_path).map(|e| &e.parsed)
    }

    /// Everything the LSP's per-project decline decisions read, as one value —
    /// see [`ProjectEvaluation`]. This is the *only* supported way to build one:
    /// assembling it by hand from [`Self::project`] loses `not_an_inner_build`,
    /// and a capability declined on a fact the explainer cannot see is a
    /// capability that goes away silently.
    pub fn project_evaluation(&mut self, project_path: &Path) -> ProjectEvaluation<'_> {
        self.project_evaluation_of(&ProjectId::new(project_path))
    }

    /// [`Self::project_evaluation`] for a project already identified. The
    /// deferral refresh runs after every dispatched message and already holds
    /// [`ProjectId`]s, so re-deriving them would be pure cost there.
    pub fn project_evaluation_of(&mut self, project: &ProjectId) -> ProjectEvaluation<'_> {
        self.validate(project);
        self.project_evaluation_as_last_confirmed(project)
    }

    /// [`Self::project_evaluation_of`] without confirming a cached evaluation
    /// for the open generation: what the last generation that read it found.
    ///
    /// For the deferral refresh, which runs after every dispatched message: a
    /// message that read the project has confirmed it already, and one that
    /// did not (a `didChange`, a refresh reply) would otherwise pay a
    /// validation for a report. A report built on a project whose files moved
    /// unwatched is corrected by the next message that reads the project.
    pub fn project_evaluation_as_last_confirmed(
        &mut self,
        project: &ProjectId,
    ) -> ProjectEvaluation<'_> {
        self.project_evaluation_and_id_as_last_confirmed(project).0
    }

    /// [`Self::project_evaluation_as_last_confirmed`], with the identity of
    /// the evaluation it describes ([`Self::evaluation_id`]): what a value
    /// derived from an evaluation is checked against before it is reported
    /// beside it.
    pub fn project_evaluation_and_id_as_last_confirmed(
        &mut self,
        project: &ProjectId,
    ) -> (ProjectEvaluation<'_>, Option<EvaluationId>) {
        self.cached_or_evaluated(project);
        let id = self.evaluation_id(project);
        let evaluation = match self
            .projects
            .get(project)
            .and_then(|c| c.evaluated.as_ref())
        {
            Some(evaluated) => ProjectEvaluation::Evaluated {
                parsed: &evaluated.parsed,
                not_an_inner_build: evaluated.not_an_inner_build,
            },
            None => ProjectEvaluation::Failed,
        };
        (evaluation, id)
    }

    /// The cached [`EvaluatedProject`] for `project_path` — the parse plus the
    /// SDK install root the evaluator resolved — lazily computed and cached
    /// (keyed by [`ProjectId`]). `None` when evaluation failed; the
    /// failure is cached too.
    fn evaluated(&mut self, project_path: &Path) -> Option<&EvaluatedProject> {
        self.evaluated_of(&ProjectId::new(project_path))
    }

    /// [`Self::evaluated`] for a project already identified. Evaluates the
    /// spelling the key names, so `$(MSBuildProjectDirectory)` and every joined
    /// `<Compile>` include are spelt as the buffers that reach the project by
    /// that path are, and the entry can never hold another spelling's
    /// evaluation.
    fn evaluated_of(&mut self, project: &ProjectId) -> Option<&EvaluatedProject> {
        self.validate(project);
        self.cached_or_evaluated(project)
    }

    /// The cached evaluation of `project` as it is, without confirming it for
    /// the open generation; evaluated, as [`Self::evaluated_of`] would, if
    /// none is cached.
    fn cached_or_evaluated(&mut self, project: &ProjectId) -> Option<&EvaluatedProject> {
        let env = &self.env;
        let extra_build_properties = &self.extra_build_properties;
        let evaluated = &self.evaluated;
        let generation = self.checker.generation();
        // Read before the evaluation starts, as the generation's time is.
        let started = self.checker.now();
        let checker = &self.checker;
        let next_evaluation = &self.next_evaluation;
        let retired = &mut self.retired;
        self.projects
            .entry(project.clone())
            .or_insert_with(|| {
                let evaluation =
                    evaluate_project(project.as_path(), env, extra_build_properties, evaluated);
                let id = EvaluationId(next_evaluation.get());
                next_evaluation.set(id.0 + 1);
                // The dropped evaluation's checks are held until the new
                // one's are made, so that the reads the two share keep
                // their witnesses.
                let (checks, exempt) = evaluation.checks_and_exempt(checker, |version| {
                    nuget_committed_before(checker, version, started)
                });
                retired.remove(project);
                CachedEvaluation {
                    id,
                    checks,
                    exempt,
                    event_since: false,
                    immutable: evaluation.immutable,
                    reads: evaluation.reads,
                    evaluated: evaluation.evaluated,
                    // Read in this generation, so confirmed for it.
                    validated_in: generation,
                }
            })
            .evaluated
            .as_ref()
    }

    /// Whether the project at `project_path` compiles `file`, as far as we can
    /// conclude. Only a project that evaluated **completely** yields a verdict;
    /// a partial or failed evaluation is [`Membership::Unknown`] in *both*
    /// directions:
    ///
    /// - `Membership::Member` — the project evaluated completely and `file` is
    ///   in its resolved `<Compile>` list.
    /// - `Membership::NotMember` — the project evaluated completely and `file`
    ///   is absent, so the absence is authoritative.
    /// - `Membership::Unknown` — the project's Compile set is untrustworthy
    ///   (`items_uncertain`) or it didn't evaluate at all. The `items` list is
    ///   unreliable in both directions: a missing item may have been dropped by
    ///   a condition/operation the parser couldn't model, and a *present* item
    ///   may be stale (e.g. a `<Compile Remove>` is diagnosed but not applied,
    ///   leaving the item behind). So we can conclude neither membership nor
    ///   non-membership.
    ///
    /// We gate on `items_uncertain`, not `is_partial`: a project whose only
    /// divergences are harmless (undefined properties / skipped `<Target>`s in
    /// imported SDK files) still has a faithful Compile set, so its membership
    /// verdict *is* authoritative — gating on `is_partial` would make every
    /// real SDK project inconclusive.
    fn membership(&mut self, project_path: &Path, file: &Path) -> Membership {
        match self.project(project_path) {
            // Untrustworthy Compile set first: its items prove nothing.
            Some(p) if p.items_uncertain => Membership::Unknown,
            Some(p) if project_contains(&p.items, file) => Membership::Member,
            Some(_) => Membership::NotMember,
            None => Membership::Unknown,
        }
    }

    /// The transitive inter-project dependency graph rooted at `entry` (an F#
    /// project), built from each project's resolved `<ProjectReference>` items.
    /// See [`crate::project_graph`] for the graph shape and traversal rules
    /// (recurse only through `.fsproj`; `.csproj` is a terminal boundary owned
    /// by the C# sidecar).
    ///
    /// **Evaluates every node fresh and leaves the project cache untouched**,
    /// except that the entry reads its cached evaluation when this
    /// generation has confirmed it (the same evaluation, already shown
    /// current). Unlike [`Workspace::project`], this does not populate
    /// `Workspace.projects`; every other node is parsed from disk via
    /// `evaluate_project`. That is load-bearing for its consumer, the `.fsproj`
    /// reference-cycle diagnostics:
    /// - **No pinning.** A `.fsproj` diagnostic must not pin the project cache —
    ///   there is no file-watch guarantee, so a cached evaluation could later be
    ///   served stale to `.fs` diagnostics (see
    ///   `fsproj_sync_does_not_pin_the_project_cache`). Building the graph
    ///   off-cache keeps that invariant.
    /// - **Freshness.** The graph must reflect current disk so it can't disagree
    ///   with the buffer-derived reference diagnostics. The consumer only builds
    ///   it when the entry buffer matches disk, so a fresh disk read of the
    ///   entry equals the buffer.
    ///
    /// Re-evaluating the closure on each call is acceptable: cycle diagnostics
    /// fire only on `.fsproj` open/change/pull, not per source keystroke.
    pub fn project_graph(&self, entry: &Path) -> ProjectGraph {
        self.project_graph_impl(
            entry,
            &BTreeMap::new(),
            GraphWalkPurpose::DeclaredStructure,
            &Unobserved::FsprojDiagnostics.msbuild_observer(),
        )
    }

    /// [`Workspace::project_graph`] with each node's evaluation seeded by the
    /// producer TFM NuGet's restore selected for it. `producer_tfms` is keyed
    /// by **canonicalised** project path (the shape
    /// [`crate::project_assets::resolve_transitive_project_tfms`] — and the
    /// semantic layer's recovery on top of it — produces); the map is
    /// best-effort (empty before a restore).
    ///
    /// Seeding matters when a multi-targeted dependency gates a
    /// `<ProjectReference>` on `$(TargetFramework)`: the real build evaluates
    /// that condition in an inner pass under the TFM its *consumer* selected,
    /// so walking the node under a guessed TFM could follow an edge the build
    /// wouldn't (or miss one it would). This walk therefore maintains the
    /// invariant that **no edge whose presence depends on an unknown TFM
    /// choice is followed**: a multi-targeted node the map doesn't cover
    /// keeps only its TFM-invariant edges
    /// (`GraphWalkPurpose::CompileClosure`). The env fold — whose
    /// reference set is pinned against MSBuild's — is the consumer; the
    /// `.fsproj` cycle diagnostics keep [`Workspace::project_graph`]'s
    /// first-declared walk (they predate any restore and report on declared
    /// structure).
    ///
    /// Every query the walk makes is recorded in `observer`: its own probes,
    /// the read set of each node it evaluates, and, for the entry when this
    /// generation has confirmed its cached evaluation, the reads that
    /// confirm it (its read set less the contents of installed versions).
    pub fn project_graph_with_producer_tfms(
        &self,
        entry: &Path,
        producer_tfms: &BTreeMap<PathBuf, String>,
        observer: &Observer,
    ) -> ProjectGraph {
        self.project_graph_impl(
            entry,
            producer_tfms,
            GraphWalkPurpose::CompileClosure,
            observer,
        )
    }

    fn project_graph_impl(
        &self,
        entry: &Path,
        producer_tfms: &BTreeMap<PathBuf, String>,
        purpose: GraphWalkPurpose,
        observer: &Observer,
    ) -> ProjectGraph {
        // The entry is resolved exactly once, with its own path: the builder
        // never re-resolves a visited node, and every non-entry resolve comes
        // through an already-normalised edge target. Compare normalised so an
        // entry spelled with `.`/`..` still recognises itself.
        let entry_key = lexically_normalize(entry);
        // The entry's own cached evaluation, when this generation has
        // confirmed it and it is of exactly this spelling: the walk's
        // seedless evaluation of the entry is the same computation, so it
        // reads the cache rather than evaluating again.
        let generation = self.checker.generation();
        let entry_id = ProjectId::new(entry);
        let confirmed_entry = self
            .projects
            .get(&entry_id)
            .filter(|cached| generation.is_some() && cached.validated_in == generation)
            .filter(|_| entry_id.as_path() == entry)
            .inspect(|cached| {
                observer.absorb((cached.reads.clone(), ReadCounts::default()));
            })
            .map(|cached| cached.evaluated.as_ref());
        build_graph(entry, |path| {
            // The map's keys are canonicalised; graph paths are only
            // lexically normalised. A path that fails to canonicalise (not
            // on disk) can't be in the map — fall through to no seed.
            let seed_tfm = msbuild_observed::canonicalize(observer, path)
                .ok()
                .and_then(|canon| producer_tfms.get(&canon))
                .map(String::as_str);
            let is_entry = lexically_normalize(path) == entry_key;
            resolve_node_uncached(
                path,
                NodeContext {
                    env: &self.env,
                    extra_build_properties: &self.extra_build_properties,
                    evaluated: &self.evaluated,
                },
                seed_tfm,
                purpose,
                is_entry,
                confirmed_entry.filter(|_| is_entry && path == entry),
                observer,
            )
        })
    }
}

/// What every node of a graph walk is evaluated with: the workspace's SDK
/// discovery environment and build properties, and its count of evaluations
/// run.
#[derive(Clone, Copy)]
struct NodeContext<'a> {
    env: &'a SdkDiscoveryEnv,
    extra_build_properties: &'a HashMap<String, String>,
    evaluated: &'a Cell<u64>,
}

/// What a graph walk is *for* — the two consumers need different edge
/// semantics, because one turns edges into folded DLLs and the other into
/// squiggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphWalkPurpose {
    /// The `.fsproj` diagnostics walk: report on the project's declared
    /// structure. Every `<ProjectReference>` is followed (a
    /// `ReferenceOutputAssembly="false"` ref is still a build dependency —
    /// its target must exist and cycles through it are still build cycles),
    /// and a multi-targeted node with no pinned TFM keeps its historical
    /// first-declared evaluation.
    DeclaredStructure,
    /// The assembly-env fold's walk: reconstruct the **compile-reference
    /// closure**, where fabricating an edge folds DLLs the compiler never
    /// sees. Each edge's contribution is classified by [`compile_edge_kind`]
    /// (build-only refs dropped; the entry's own `ExcludeAssets=compile`
    /// refs kept output-only), and a multi-targeted node with no pinned TFM
    /// keeps only the edge contributions present under **every** declared
    /// TFM — those are taken by the real build regardless of which TFM its
    /// consumer selects (D5: under-resolve, never wrong).
    CompileClosure,
}

/// The conclusion of a [`Workspace::membership`] check — three-valued because
/// a partial/failed project's `items` is authoritative in neither direction.
enum Membership {
    Member,
    NotMember,
    Unknown,
}

/// Walk `file`'s ancestor directories looking for the closest one that
/// contains a `.fsproj`, reading each directory's listing through
/// `dir_listings` and adding it to `listings`. Returns the absolute path of
/// the chosen project file, or `None` when no ancestor contains any
/// `.fsproj`.
///
/// On ties (a single directory containing more than one `.fsproj`), the
/// alphabetically-first filename wins. This is the membership-blind
/// heuristic; [`Workspace::owning_project`] layers `<Compile>`-membership on
/// top and uses this only as the fallback when no candidate lists `file`.
fn find_owning_project(
    file: &Path,
    dir_listings: &mut DirListings,
    listings: &mut Vec<(PathBuf, ListingId)>,
) -> Option<PathBuf> {
    // Start from the file's parent (the file itself may not exist).
    let mut dir = file.parent()?;
    loop {
        let (projects, listing) = dir_listings.projects_in(dir);
        listings.push((dir.to_path_buf(), listing));
        if let Some(found) = projects.into_iter().next() {
            return Some(found);
        }
        dir = dir.parent()?;
    }
}

/// The `.fsproj` entries of `dir`'s listing `observation`, as paths in
/// `dir`, sorted by name. A listing that failed (no permission, not a
/// directory) and an entry it could not read yield no project: the walk
/// treats "no `.fsproj` here" the same way.
fn fsproj_entries(dir: &Path, observation: &Observation) -> Vec<PathBuf> {
    let mut hits: Vec<PathBuf> = match observation {
        Observation::Listing(Ok(entries)) => entries
            .iter()
            .filter_map(|entry| match entry {
                Listed::Entry { name, .. } => Some(dir.join(name)),
                Listed::Unreadable(_) => None,
            })
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("fsproj"))
            .collect(),
        _ => Vec::new(),
    };
    hits.sort();
    hits
}

/// The node's effective output-file base name from one evaluation
/// ([`ProjectNode::output_name`](crate::project_graph::ProjectNode::output_name)): the trusted evaluated `$(TargetName)`
/// (defaulting to `$(AssemblyName)` —
/// [`borzoi_msbuild::ParsedProject::target_name`]), else the
/// project-file stem (MSBuild's default, `$(MSBuildProjectName)`). `None`
/// when the evaluator couldn't trust the value's provenance
/// ([`ItemMetadataValue::Unknown`]) — locating an output DLL by a guessed
/// name can fold a stale or unrelated assembly.
fn evaluated_output_name(path: &Path, evaluated: &EvaluatedProject) -> Option<String> {
    match &evaluated.parsed.target_name {
        ItemMetadataValue::Known(Some(name)) => Some(name.clone()),
        ItemMetadataValue::Known(None) => Some(path.file_stem()?.to_string_lossy().into_owned()),
        ItemMetadataValue::Unknown => None,
    }
}

/// The graph builder's filesystem-backed resolver, evaluating each project
/// **fresh** (no caching — see [`Workspace::project_graph`]):
/// [`NodeResult::NotFound`] if the file is absent, otherwise its
/// `<ProjectReference>` edges (each target lexically normalised, with the item's
/// XML span). A project that exists but fails to evaluate resolves to an empty
/// edge list — it's a node in the closure we just can't see past — matching the
/// `NodeResult` contract.
///
/// Only `.fsproj` projects are parsed for their edges. The builder also asks
/// about `.csproj` targets purely to check existence (it never recurses into
/// them — the sidecar owns the C# subtree), so an existing non-F# project
/// short-circuits to an empty edge list rather than paying to parse C# project
/// XML through the fsproj evaluator.
///
/// `seed_tfm` is the producer TFM a restore selected for this node
/// ([`Workspace::project_graph_with_producer_tfms`]). The node's edges are
/// read under the first of these whose TFM claim holds against the project's
/// **current** on-disk state (restore data may be stale):
///
/// 1. a caller-supplied global `TargetFramework` in `extra_build_properties`
///    — the caller owns that choice (`select_target_framework`'s precedence);
/// 2. the project's own body-written singular `TargetFramework` — current
///    truth, whatever an old restore recorded;
/// 3. `seed_tfm`, **only if the project still declares it** — injected as the
///    `TargetFramework` global, exactly what an MSBuild inner build does. A
///    stale seed (the producer retargeted since the consumer's last restore)
///    would evaluate `$(TargetFramework)`-gated edges under a TFM the real
///    build can no longer select;
/// 4. else the TFM is genuinely unknown for a multi-targeted node, and
///    `purpose` decides (see [`GraphWalkPurpose`]). The declared list is
///    read from the evaluation's **first pass**
///    ([`EvaluatedProject::declared_tfms`]) — an outer-gated plural is
///    invisible after the internal first-declared re-evaluation.
///
/// A [`GraphWalkPurpose::CompileClosure`] walk additionally classifies each
/// edge's contribution (see [`compile_edge_kind`], which needs `is_entry` —
/// MSBuild treats an asset exclusion on the consumer's own reference
/// differently from one buried in the closure) and reports each node's TFM
/// outcome as a [`NodeTfm`] for the env fold's output-DLL location.
fn resolve_node_uncached(
    path: &Path,
    context: NodeContext<'_>,
    seed_tfm: Option<&str>,
    purpose: GraphWalkPurpose,
    is_entry: bool,
    cached: Option<Option<&EvaluatedProject>>,
    observer: &Observer,
) -> NodeResult {
    let NodeContext {
        env,
        extra_build_properties,
        evaluated,
    } = context;
    if !msbuild_observed::exists(observer, path) {
        return NodeResult::NotFound;
    }
    if classify(path) != ProjectKind::FSharp {
        return NodeResult::resolved(Vec::new());
    }
    // Every evaluation the node makes goes through here, so each one's read
    // set is recorded, whichever branch below it serves (the seeded inner
    // pass, and every remaining TFM of a multi-targeted project, included).
    let evaluate_recorded = |properties: &HashMap<String, String>| {
        let evaluation = evaluate_project(path, env, properties, evaluated);
        observer.absorb((evaluation.reads, ReadCounts::default()));
        evaluation.evaluated
    };
    // Outer (seedless) evaluation first: its first-pass view is the source of
    // truth for what the project currently declares, and its edges are the
    // first-declared inner build's (via `select_target_framework`). `cached`
    // is that evaluation when the caller already holds it confirmed.
    let fresh;
    let outer = match cached {
        Some(cached) => cached,
        None => {
            fresh = evaluate_recorded(extra_build_properties);
            fresh.as_ref()
        }
    };
    let Some(outer) = outer else {
        return NodeResult::resolved(Vec::new());
    };
    let outer_edges = edges_of(outer, purpose, is_entry);
    let outer_suppressed = references_suppressed(outer, purpose);
    if outer.not_an_inner_build {
        // Ahead of the caller-owned fast path below, for the reason
        // `Workspace::served_tfm_for_project` states: caller ownership excuses
        // the *provenance* of a value, not an evaluation the document was free
        // to conduct under a different TFM than the one supplied.
        //
        // `Unresolved`, never `NoneDeclared`: the latter tells the env "this
        // project declares no TFM, so its sole restored output is the one",
        // which for a multi-targeted project licenses folding whichever stale
        // TFM directory is lying around. The output name declines with it, as
        // for any TFM-unresolved node.
        return NodeResult::Resolved {
            edges: outer_edges,
            tfm: NodeTfm::Unresolved,
            output_name: None,
            references_uncertain: outer_suppressed,
        };
    }
    if caller_owns_target_framework(extra_build_properties) {
        return NodeResult::Resolved {
            edges: outer_edges,
            tfm: match &outer.chosen_tfm {
                Some(tfm) => NodeTfm::Known(tfm.clone()),
                None => NodeTfm::NoneDeclared,
            },
            output_name: evaluated_output_name(path, outer),
            references_uncertain: outer_suppressed,
        };
    }
    if outer.tfm_untrusted {
        // The body `TargetFramework` leans on a value the real build may
        // resolve differently (written under a gate we couldn't pin, or
        // still holding `$(...)`): it is not current truth, and a seed
        // validated against declarations shaped by it proves nothing. The
        // node's TFM is genuinely unknown, so the env fold must skip its
        // output (a Known verdict would locate — and could fold — a stale
        // wrong-TFM DLL) and its evaluated output name (possibly TFM-gated
        // itself) declines with it. The EDGES are kept: any TFM-dependent
        // edge reads the unpinned `TargetFramework` and has already flipped
        // `project_references_uncertain` (emptying the compile walk's
        // `edges_of`), so an edge that survived is TFM-invariant.
        return NodeResult::Resolved {
            edges: outer_edges,
            tfm: NodeTfm::Unresolved,
            output_name: None,
            references_uncertain: outer_suppressed,
        };
    }
    if let Some(body) = &outer.body_target_framework {
        // The project pins its own TFM: current truth, whatever any old
        // restore recorded.
        return NodeResult::Resolved {
            edges: outer_edges,
            tfm: NodeTfm::Known(body.clone()),
            output_name: evaluated_output_name(path, outer),
            references_uncertain: outer_suppressed,
        };
    }
    let declared = &outer.declared_tfms;
    if declared.len() <= 1 {
        // Single-target (or no TFM declared at all): there is only one build
        // the project can produce, so the seed adds nothing — *provided the
        // evaluation in hand is that build*.
        //
        // The label therefore comes from `chosen_tfm`, which describes what the
        // evaluation ran under, and never from the declaration alone. The two
        // agree except when the caller pinned an explicitly **empty**
        // `TargetFramework` global: that is the outer dispatch build, so a
        // `Known(sole)` label would pair the inner build's identity with the
        // outer build's `output_name` and reference list, both of which may be
        // TFM-gated. A declared-but-not-evaluated TFM is `Unresolved` — we
        // cannot say — rather than `NoneDeclared`, which would tell the env the
        // project's sole restored output is the one to fold.
        let tfm = match (&outer.chosen_tfm, declared.first()) {
            (Some(chosen), _) => NodeTfm::Known(chosen.clone()),
            (None, Some(_)) => NodeTfm::Unresolved,
            (None, None) => NodeTfm::NoneDeclared,
        };
        let names_the_build = matches!(tfm, NodeTfm::Known(_) | NodeTfm::NoneDeclared);
        return NodeResult::Resolved {
            edges: outer_edges,
            output_name: names_the_build
                .then(|| evaluated_output_name(path, outer))
                .flatten(),
            tfm,
            references_uncertain: outer_suppressed,
        };
    }
    if let Some(seed) = seed_tfm {
        if let Some(current) = declared.iter().find(|d| d.eq_ignore_ascii_case(seed)) {
            // The outer evaluation already IS this inner build when the seed
            // matches the first-declared choice.
            if outer
                .chosen_tfm
                .as_deref()
                .is_some_and(|c| c.eq_ignore_ascii_case(seed))
            {
                return NodeResult::Resolved {
                    edges: outer_edges,
                    tfm: NodeTfm::Known(current.clone()),
                    output_name: evaluated_output_name(path, outer),
                    references_uncertain: outer_suppressed,
                };
            }
            let mut map = extra_build_properties.clone();
            seed_target_framework_global(&mut map, current);
            return match evaluate_recorded(&map) {
                // The label describes the evaluation, as everywhere else: this
                // seeded pass may itself turn out not to be an inner build,
                // and only *it* can tell us — a `TreatAsLocalProperty` reached
                // through a `$(TargetFramework)`-conditioned import is
                // invisible to the first-declared `outer` above.
                Some(inner) if inner.not_an_inner_build => NodeResult::Resolved {
                    edges: edges_of(&inner, purpose, is_entry),
                    tfm: NodeTfm::Unresolved,
                    output_name: None,
                    references_uncertain: references_suppressed(&inner, purpose),
                },
                Some(inner) => NodeResult::Resolved {
                    edges: edges_of(&inner, purpose, is_entry),
                    tfm: NodeTfm::Known(current.clone()),
                    // The name must come from the same (inner) evaluation as
                    // the edges: `<AssemblyName>` may itself be TFM-gated.
                    output_name: evaluated_output_name(path, &inner),
                    // The inner evaluation is where these edges came from,
                    // so it is the one whose suppression applies.
                    references_uncertain: references_suppressed(&inner, purpose),
                },
                None => NodeResult::resolved(Vec::new()),
            };
        }
        tracing::info!(
            project = %path.display(),
            seed,
            declared = ?declared,
            "restored producer TFM is no longer declared by the project; treating the node as unseeded"
        );
    }
    // From here the node's TFM is unresolved, so its output-assembly name is
    // too: `<AssemblyName>` may be TFM-gated, and no single evaluation's
    // value is the one the real build would use. The env fold skips
    // TFM-unresolved nodes anyway; a `None` name keeps the two verdicts
    // consistent.
    match purpose {
        GraphWalkPurpose::DeclaredStructure => NodeResult::Resolved {
            edges: outer_edges,
            tfm: NodeTfm::Unresolved,
            output_name: None,
            references_uncertain: outer_suppressed,
        },
        GraphWalkPurpose::CompileClosure => {
            // `outer` is the first-declared inner pass; re-evaluate under
            // each remaining declared TFM and keep only the edges (by target)
            // present in all — those are taken by the real build no matter
            // which TFM its consumer selected. A branch that fails to
            // evaluate proves nothing, so nothing survives (E4: a node we
            // can't see past; under-resolve). An edge's *kind* is intersected
            // too: only a contribution present under every TFM is invariant,
            // so a target that any branch demotes to OutputOnly (e.g. a
            // TFM-conditioned `ExcludeAssets`) survives as OutputOnly.
            let mut surviving = outer_edges;
            // Any branch whose list was suppressed leaves the intersection
            // short of edges the real build may take, whichever TFM it picks.
            let mut any_suppressed = outer_suppressed;
            for tfm in declared.iter().skip(1) {
                let mut map = extra_build_properties.clone();
                seed_target_framework_global(&mut map, tfm);
                let Some(branch) = evaluate_recorded(&map) else {
                    return NodeResult::Resolved {
                        edges: Vec::new(),
                        tfm: NodeTfm::Unresolved,
                        output_name: None,
                        // A branch we could not evaluate takes the whole
                        // intersection with it, so the emptied list is a
                        // suppression like any other — not a project that
                        // references nothing.
                        references_uncertain: true,
                    };
                };
                any_suppressed |= references_suppressed(&branch, purpose);
                let mut branch_kinds: HashMap<PathBuf, EdgeKind> = HashMap::new();
                for e in edges_of(&branch, purpose, is_entry) {
                    branch_kinds
                        .entry(e.target)
                        .and_modify(|k| {
                            // Two references to one target within a branch:
                            // the transparent one wins (its flow happens
                            // regardless of the other's exclusion).
                            if e.kind == EdgeKind::Full {
                                *k = EdgeKind::Full;
                            }
                        })
                        .or_insert(e.kind);
                }
                surviving.retain_mut(|e| match branch_kinds.get(&e.target) {
                    None => false,
                    Some(EdgeKind::Full) => true,
                    Some(EdgeKind::OutputOnly) => {
                        e.kind = EdgeKind::OutputOnly;
                        true
                    }
                });
                // Deliberately no early exit once `surviving` empties. The
                // loop is no longer collecting edges by then, but it is still
                // collecting the *suppression* signal, and a later branch
                // whose list the evaluator could not trust — or which fails to
                // evaluate at all — is exactly what must not be missed. The
                // retain below is a no-op on an empty vector, so the only cost
                // is the remaining evaluations.
            }
            NodeResult::Resolved {
                edges: surviving,
                tfm: NodeTfm::Unresolved,
                output_name: None,
                references_uncertain: any_suppressed,
            }
        }
    }
}

/// The `<ProjectReference>` edges of an evaluated project, each target
/// lexically normalised, in document order. A [`GraphWalkPurpose::CompileClosure`]
/// walk keeps only compile references, with each edge's contribution decided
/// by [`compile_edge_kind`]; the declared-structure walk keeps everything as
/// [`EdgeKind::Full`] (a build-only dependency still has existence and cycle
/// semantics).
/// Whether [`edges_of`] *suppressed* this evaluation's `<ProjectReference>`
/// list rather than finding it empty — the condition the emptying is gated on,
/// read back so the node can carry it
/// ([`ProjectNode::references_uncertain`](crate::project_graph::ProjectNode::references_uncertain)).
///
/// Kept beside `edges_of` because it must stay the same condition: a walk that
/// drops edges without marking them leaves the env fold counting a set those
/// references were already filtered out of, so it sees no shortfall to report.
fn references_suppressed(evaluated: &EvaluatedProject, purpose: GraphWalkPurpose) -> bool {
    // The project-side half of the condition is
    // [`ProjectEvaluation::drops_reference_edges`], shared with the code that
    // *explains* the dropped edges to the user rather than restated here: a walk
    // that drops edges on a condition the message doesn't know about takes the
    // capability away in silence. (`not_an_inner_build` is exactly such a
    // condition — nothing in the evaluation flags it, because a multi-targeted
    // document never writes the singular `TargetFramework` and so decides
    // `'$(TargetFramework)' == ''` perfectly cleanly.)
    //
    // The purpose gate stays here: it is about why *this caller* is walking, not
    // about the project, so it has no place in a per-project verdict.
    purpose == GraphWalkPurpose::CompileClosure
        && ProjectEvaluation::Evaluated {
            parsed: &evaluated.parsed,
            not_an_inner_build: evaluated.not_an_inner_build,
        }
        .drops_reference_edges()
}

fn edges_of(evaluated: &EvaluatedProject, purpose: GraphWalkPurpose, is_entry: bool) -> Vec<Edge> {
    // The captured list may claim references the real build strips — an
    // unmodelled `<ProjectReference Update/Remove>` that may run (behind
    // any gate we couldn't trust: its own, its group's, an undecided
    // `<Choose>`, an unfollowed import), an `<ItemDefinitionGroup>`
    // metadata default, or an Include kept only by an untrusted gate; see
    // `ParsedProject::project_references_uncertain` for the probed
    // catalogue. Folding from it would fabricate. The node becomes one we
    // can't see past (E4; D5: under-resolve, never wrong). The
    // declared-structure walk keeps reporting on the declared elements.
    if references_suppressed(evaluated, purpose) {
        return Vec::new();
    }
    evaluated
        .parsed
        .project_references
        .iter()
        .filter_map(|item| {
            let kind = match purpose {
                GraphWalkPurpose::DeclaredStructure => EdgeKind::Full,
                GraphWalkPurpose::CompileClosure => compile_edge_kind(item, is_entry)?,
            };
            Some(Edge {
                target: lexically_normalize(&item.include),
                span: item.span.clone(),
                kind,
            })
        })
        .collect()
}

/// What a `<ProjectReference>` contributes to the consumer's **compile**
/// closure, or `None` if nothing. MSBuild ground truth (dotnet 10 probes,
/// 2026-07, A→D→E fixtures):
///
/// - A `ReferenceOutputAssembly` that does not compare `true` is a
///   build-order-only dependency: nothing lands in `ReferencePath`, not even
///   transitively → `None`. "Compares true" is the common targets'
///   `'%(ReferenceOutputAssembly)'=='true'` after an empty value defaults to
///   `true` — MSBuild `==`, i.e. the boolean vocabulary, untrimmed (probed,
///   dotnet 10.0.301, 2026-07-10: `on`/`yes`/`!false`/`TRUE` keep the DLL on
///   `ReferencePath`; `false`/`no`/`off`/`0`/`1`/`" true "`/`" false "`
///   remove it — see [`borzoi_msbuild::msbuild_boolean`]).
/// - Otherwise the compile assets flow through the edge iff `compile` is in
///   `IncludeAssets ∖ ExcludeAssets ∖ PrivateAssets` — where `IncludeAssets`
///   defaults to everything, and `PrivateAssets` (default
///   `contentfiles;analyzers;build`, which doesn't cover compile) only
///   applies on **non-entry** edges: it governs the flow to the *owner's*
///   consumers, and the entry has none in this walk. Flow →
///   [`EdgeKind::Full`].
/// - When compile does not flow but the edge is the **entry's own**, the
///   target's output still lands in `ReferencePath` — the build adds direct
///   `<ProjectReference>` outputs itself; the asset filters shape only what
///   flows *through* the reference (probed for `ExcludeAssets="compile"`,
///   `IncludeAssets="runtime"`, and even `IncludeAssets="none"`) →
///   [`EdgeKind::OutputOnly`].
/// - On a **non-entry** edge, stopped flow is fully opaque to the entry: the
///   target's own output does not flow up either (the probe: A→D normal,
///   D→E excluded leaves A referencing only D; likewise
///   `PrivateAssets="all"`) → `None`.
/// - Stopped flow on an entry edge to a **C#** target is also `None`: a
///   `.csproj` boundary node's contract is "the sidecar expands its whole
///   transitive subtree" (see `GraphRefTargets::csharp`), which would
///   fabricate exactly the references the filter removes. Dropping
///   under-resolves only the target's own DLL (D5: under-resolve, never
///   wrong).
///
/// Metadata whose resolution is [`ItemMetadataValue::Unknown`] is read
/// conservatively: an unknown `ReferenceOutputAssembly` may be `false` in the
/// real build (nothing safe to fold — drop the edge), while unknown asset
/// filters can only affect what flows *through* the edge, never the entry's
/// direct output (probed for `none`/`runtime`/`compile` filters), so an
/// entry edge degrades to [`EdgeKind::OutputOnly`] and a non-entry edge
/// drops.
///
/// Anything else — including known-absent metadata — is a normal
/// [`EdgeKind::Full`] compile reference, matching MSBuild's `''!='false'`
/// treatment. An **explicitly empty** asset filter (`IncludeAssets=""`, or
/// an empty child clearing an earlier value) is the same as absent, not an
/// empty allow-list: probed (dotnet 10, 2026-07-10) — A→Mid with
/// `IncludeAssets=""` and Mid→Leaf still puts both Mid and Leaf on
/// `ReferencePath`, identical to the unfiltered control — so collapsing
/// unset/empty/cleared into [`ItemMetadataValue::Known`]`(None)` is exact
/// (pinned live by `fsharp_empty_include_assets_matches_msbuild`).
fn compile_edge_kind(item: &ResolvedItem, is_entry: bool) -> Option<EdgeKind> {
    // Significant-but-unmodelled P2P metadata makes the edge's contribution
    // unknowable: probed (dotnet 10, 2026-07-10), `BuildReference="false"`
    // and `Targets="Clean"` remove the target from `ReferencePath` even on
    // the entry's own edge with a prebuilt DLL, and the `Set*` /
    // property-list names change which build of the target the real
    // compiler sees. Nothing is safe to fold (D5: drop, under-resolve).
    if item.unmodelled_reference_metadata {
        return None;
    }
    let build_only = match &item.reference_output_assembly {
        ItemMetadataValue::Unknown => return None,
        // Absent/empty: the common targets default it to `true` before the
        // comparison below.
        ItemMetadataValue::Known(None) => false,
        // The output lands on `ReferencePath` only under the common targets'
        // `'%(ReferenceOutputAssembly)'=='true'` — an MSBuild `==`, decided
        // through the boolean vocabulary (untrimmed, so `" true "` does NOT
        // compare true; see [`borzoi_msbuild::msbuild_boolean`]).
        ItemMetadataValue::Known(Some(v)) => borzoi_msbuild::msbuild_boolean(v) != Some(true),
    };
    if build_only {
        return None;
    }
    // Three-valued (`None` = unknowable): compile flows through the edge iff
    // it is included, not excluded, and (on non-entry edges) not private.
    let known = |value: &ItemMetadataValue, absent_default: bool| match value {
        ItemMetadataValue::Unknown => None,
        ItemMetadataValue::Known(None) => Some(absent_default),
        ItemMetadataValue::Known(Some(v)) => Some(asset_list_covers_compile(v)),
    };
    let included = known(&item.include_assets, true);
    let excluded = known(&item.exclude_assets, false);
    let private = if is_entry {
        // Governs flow to the *owner's* consumers; the entry has none here.
        Some(false)
    } else {
        known(&item.private_assets, false)
    };
    let flows = match (included, excluded, private) {
        // A known blocker blocks regardless of the other filters.
        (Some(false), _, _) | (_, Some(true), _) | (_, _, Some(true)) => Some(false),
        (Some(true), Some(false), Some(false)) => Some(true),
        _ => None,
    };
    match flows {
        Some(true) => Some(EdgeKind::Full),
        // Blocked or unknowable flow: the entry's own edge still references
        // the target's direct output (the build adds it outside the asset
        // flow), so it degrades to OutputOnly rather than vanishing.
        Some(false) | None => {
            if is_entry && classify(Path::new(&item.include)) == ProjectKind::FSharp {
                Some(EdgeKind::OutputOnly)
            } else {
                None
            }
        }
    }
}

/// Whether a `;`-separated NuGet asset-kind list (`IncludeAssets` /
/// `ExcludeAssets` / `PrivateAssets` metadata) covers the `compile` asset —
/// either naming it or naming `all`.
pub(crate) fn asset_list_covers_compile(v: &str) -> bool {
    v.split(';')
        .map(str::trim)
        .any(|part| part.eq_ignore_ascii_case("compile") || part.eq_ignore_ascii_case("all"))
}

/// Read and evaluate `project_path`, and say what that read. The evaluation
/// is `None` on any failure (IO, XML, or evaluation) — the caller caches the
/// `None` so we don't retry on every keystroke. Each MSBuild evaluation pass
/// it runs is counted in `evaluated`.
///
/// Builds a per-project [`SdkDiscovery`] from `env`; on
/// [`crate::sdk_discovery::DiscoveryError`] (e.g. missing `$DOTNET_ROOT`,
/// malformed `global.json`) we log and evaluate without a resolver. The
/// user sees the error on the `.fsproj` buffer, whose diagnostics run their
/// own discovery ([`crate::fsproj_diagnostics::diagnostics_for`]).
fn evaluate_project(
    project_path: &Path,
    env: &SdkDiscoveryEnv,
    extra_build_properties: &HashMap<String, String>,
    evaluated: &Cell<u64>,
) -> Evaluation {
    let _span =
        tracing::info_span!("evaluate_project", project = %project_path.display()).entered();
    // The reads that prepare the evaluation: the project's text, and SDK
    // discovery's `global.json` walk and `dotnet` probes.
    let preparation = Observer::new();
    let source = msbuild_observed::read_to_string(&preparation, project_path)
        .map(crate::disk_text::without_bom);
    let Ok(source) = source else {
        return Evaluation {
            evaluated: None,
            reads: preparation.finish().0,
            immutable: ImmutableRoots::default(),
        };
    };
    let disc = SdkDiscovery::for_project(project_path, env, &preparation)
        .inspect_err(|err| {
            crate::log_warn!(
                "SDK discovery failed; evaluating without resolver",
                project = project_path.display(),
                error = err
            );
        })
        .ok();
    let mut immutable = disc
        .as_ref()
        .map_or_else(ImmutableRoots::default, |disc| ImmutableRoots {
            dotnet: disc
                .roots()
                .iter()
                .cloned()
                .chain(disc.user_dotnet_root().map(Path::to_path_buf))
                .collect(),
            nuget: disc
                .nuget_packages()
                .map(Path::to_path_buf)
                .into_iter()
                .collect(),
        });
    let reads = preparation.finish().0;
    let extras = build_properties(extra_build_properties);
    let Some(parsed) = parse_with_optional_sdk(
        &source,
        project_path,
        &extras,
        &env.build_environment,
        disc.as_ref(),
        evaluated,
    ) else {
        return Evaluation {
            evaluated: None,
            reads,
            immutable,
        };
    };
    let reads = reads.union(parsed.read_set.clone());
    // First-pass views, captured before any first-declared re-evaluation
    // (a `TfmChoice::Reseed` replaces `parsed` with an inner-build pass in
    // which an outer-gated `<TargetFrameworks>` may no longer exist).
    let declared_tfms = target_frameworks(&parsed);
    let body_target_framework = tfm_policy::body_target_framework(&parsed);
    let pass1_untrusted = tfm_policy::tfm_untrusted(&parsed);
    let served = select_target_framework(
        parsed,
        &source,
        project_path,
        &extras,
        &env.build_environment,
        disc.as_ref(),
        evaluated,
    );
    // The second pass read on its own account, whether or not it is served.
    let reads = reads.union(served.pass2_reads.unwrap_or_default());
    let parsed = served.parsed;
    let chosen_tfm = served.chosen_tfm;
    // Pass 1's provenance verdict cannot see a write that only pass 2 performs,
    // so an override widens it — and the override additionally withholds every
    // other TFM-derived conclusion (see `EvaluatedProject::not_an_inner_build`).
    let not_an_inner_build = served.not_an_inner_build;
    let tfm_untrusted = pass1_untrusted || not_an_inner_build;
    // The evaluator reports the `SdkPaths::root` of the entry project's own SDK
    // (`ParsedProject::resolved_sdk_root`); recover the install root from it.
    // This is the single source of truth for an entry-SDK project — see
    // `Workspace::dotnet_root_for_project`. `None` propagates (SDK-less entry,
    // or the path shape didn't match the known SDK layout), and the consumer
    // falls back to the probe.
    let sdk_install_root = parsed
        .resolved_sdk_root
        .as_deref()
        .and_then(install_root_from_sdk_path)
        .map(Path::to_path_buf);
    // The install the SDK resolved from, as the evaluation spelt its reads
    // there (a root SDK discovery names may reach it through a symlink).
    immutable.dotnet.extend(sdk_install_root.clone());
    Evaluation {
        evaluated: Some(EvaluatedProject {
            parsed,
            sdk_install_root,
            chosen_tfm,
            declared_tfms,
            body_target_framework,
            tfm_untrusted,
            not_an_inner_build,
        }),
        reads,
        immutable,
    }
}

/// Mark an outer dispatch build served in place of a discarded inner one as
/// uncertain on every axis a `$(TargetFramework)` gate can reach.
///
/// Its `DefineConstants`, its Compile items and its `<ProjectReference>`s are
/// the *outer* build's, and no inner build has them — content gated on
/// `'$(TargetFramework)' == ''` is reached here and nowhere else. Nothing in the
/// evaluation says so on its own: a multi-targeted document never writes the
/// singular, so that gate is decided cleanly under the evaluator's environment
/// model, with no diagnostic and no `*_uncertain` flag.
///
/// Setting the flags the consumers already read is deliberate, rather than
/// giving each of them a second predicate to consult. `*_uncertain` already
/// means "this list may not be what the real build sees, so do not fold from
/// it", which is exactly the claim, and every consumer that must respect it has
/// been audited for that meaning already — `parses_for_project` refuses the
/// whole fold on the first two (`semantic.rs`), degrading to single-file
/// analysis.
fn discarded_inner_build(mut parsed: ParsedProject) -> ParsedProject {
    parsed.define_constants_uncertain = true;
    parsed.items_uncertain = true;
    parsed.project_references_uncertain = true;
    parsed
}

/// The evaluation to serve for one project, and what is known about the TFM it
/// ran under. Produced by [`select_target_framework`].
struct ServedEvaluation {
    parsed: ParsedProject,
    /// The TFM `parsed` was evaluated under — what its defines and Compile
    /// items came from. Published as [`EvaluatedProject::chosen_tfm`].
    chosen_tfm: Option<String>,
    /// A pass-2 `TreatAsLocalProperty` override fired, so no evaluation of this
    /// document is an inner build — see
    /// [`EvaluatedProject::not_an_inner_build`], which this becomes.
    not_an_inner_build: bool,
    /// The read set of the seeded second pass, when one ran, whether or not
    /// it is the one served: a discarded pass decided `not_an_inner_build`.
    pass2_reads: Option<ReadSet>,
}

/// Apply [`tfm_policy::tfm_choice`]'s decision: re-evaluate `pass1`'s project
/// with the chosen TFM seeded when the decision asks for it, and return the
/// evaluation to serve alongside what is known about its TFM.
///
/// The policy itself — which TFM, and when a second pass is needed — lives in
/// [`tfm_policy`], shared with the `.fsproj`-buffer diagnostics path so the two
/// surfaces cannot drift (plan E5/E7). This function is only the effect.
fn select_target_framework(
    pass1: ParsedProject,
    source: &str,
    project_path: &Path,
    extras: &HashMap<String, String>,
    environment: &HashMap<String, String>,
    disc: Option<&SdkDiscovery>,
    evaluated: &Cell<u64>,
) -> ServedEvaluation {
    let choice = tfm_policy::tfm_choice(&pass1, extras);
    let Some(seed) = choice.reseed() else {
        // A *caller-supplied* global carries the name too, and the same opt-out
        // lets the document overwrite it — so pass 1 already is the mixture,
        // with no second pass involved. The caller's ownership of the choice is
        // a reason to skip provenance checks on its value, not a reason to
        // believe an evaluation that did not honour it.
        let overridable = matches!(
            tfm_policy::reseed_outcome(&pass1),
            tfm_policy::ReseedOutcome::Overridable
        );
        // Presence, not ownership: `caller_owns_target_framework` asks whether
        // the caller made a TFM *choice*, and an empty global deliberately is
        // not one — but it is still a global of that name for the opt-out to
        // unprotect, and the document can overwrite it just the same.
        let caller_supplied_global =
            tfm_policy::lookup_property_ci(extras, "TargetFramework").is_some();
        if overridable && caller_supplied_global {
            return ServedEvaluation {
                parsed: discarded_inner_build(pass1),
                chosen_tfm: None,
                not_an_inner_build: true,
                pass2_reads: None,
            };
        }
        let chosen_tfm = choice.served().map(str::to_string);
        return ServedEvaluation {
            parsed: pass1,
            chosen_tfm,
            not_an_inner_build: false,
            pass2_reads: None,
        };
    };
    let mut seeded = extras.clone();
    tfm_policy::seed_target_framework_global(&mut seeded, seed);
    let seed = seed.to_string();
    match parse_with_optional_sdk(source, project_path, &seeded, environment, disc, evaluated) {
        Some(pass2) => match tfm_policy::reseed_outcome(&pass2) {
            tfm_policy::ReseedOutcome::AsSeeded => ServedEvaluation {
                pass2_reads: Some(pass2.read_set.clone()),
                parsed: pass2,
                chosen_tfm: Some(seed),
                not_an_inner_build: false,
            },
            // The document opts `TargetFramework` out of global protection,
            // so the seeded pass may be a chimera: values above an override
            // came from the seed, values below from the override. Serving it
            // would publish a mixture as though it were one build. Keep
            // **pass 1** — the outer dispatch build, which is at least
            // internally consistent — and withhold every TFM-derived
            // conclusion (`EvaluatedProject::not_an_inner_build`).
            //
            // Discarded rather than flagged, deliberately: a flagged chimera
            // stays reachable, so every surface that reads the evaluation has
            // to decide independently whether it may trust a mixture — the
            // TFM, the output name, the node label, the reference list. A
            // discarded one is unreachable and no such decision exists.
            tfm_policy::ReseedOutcome::Overridable => ServedEvaluation {
                parsed: discarded_inner_build(pass1),
                chosen_tfm: None,
                not_an_inner_build: true,
                pass2_reads: Some(pass2.read_set),
            },
        },
        // Same source, same resolver, and the seed key can't collide (a
        // caller-owned global never yields `Reseed`) — so this arm shouldn't
        // be reachable. Degrade to the unseeded view rather than failing the
        // whole project.
        None => ServedEvaluation {
            parsed: pass1,
            chosen_tfm: None,
            not_an_inner_build: false,
            pass2_reads: None,
        },
    }
}

/// MSBuild-default build globals used when evaluating a project for
/// LSP diagnostics. The SDK's own `*.props` predicate `DefineConstants`
/// on these (e.g. `DEBUG` is gated on `$(Configuration) == 'Debug'`),
/// so passing an empty bag would silently drop those symbols and
/// produce diagnostics against the wrong `#if` branches. The
/// `.fsproj`-buffer surface needs them for a second reason: without
/// them the most common condition shape in real `.fsproj` files —
/// `Condition="'$(Configuration)|$(Platform)' == 'Debug|AnyCPU'"` —
/// emits an `UndefinedProperty` diagnostic on every clean buffer.
///
/// Intentionally a tiny set. Each addition trades a class of false
/// positives for the risk of evaluating user conditions against a value
/// the user didn't pick.
///
/// Today the LSP only ever evaluates as `Configuration=Debug,
/// Platform=AnyCPU` — the F# editor flow most users want. A future
/// follow-up may surface this as an LSP initialisation option (or read
/// `launchSettings.json`-style hints from the workspace), but
/// hard-coding the Debug defaults is the established convention for
/// IDE-style consumers (FCS does the same in `FSharpProjectOptions`).
///
/// `TargetFramework` is *not* seeded here because it is per-project, not
/// workspace-global: [`select_target_framework`] picks each project's
/// served TFM (first-declared — `docs/completed/fsproj-tfm-selection-plan.md` E1/E2)
/// and re-evaluates with it seeded when the project multi-targets.
fn default_build_properties() -> HashMap<String, String> {
    let mut p = HashMap::new();
    p.insert(
        "Configuration".to_string(),
        crate::BUILD_CONFIGURATION.to_string(),
    );
    p.insert("Platform".to_string(), "AnyCPU".to_string());
    p
}

fn build_properties(extra_build_properties: &HashMap<String, String>) -> HashMap<String, String> {
    let mut properties = HashMap::new();
    for (key, value) in default_build_properties() {
        if !contains_property_case_insensitive(extra_build_properties, &key) {
            properties.insert(key, value);
        }
    }
    properties.extend(
        extra_build_properties
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    properties
}

fn contains_property_case_insensitive(properties: &HashMap<String, String>, name: &str) -> bool {
    properties.keys().any(|key| key.eq_ignore_ascii_case(name))
}

/// Call [`parse_fsproj_with_imports`] with or without an SDK resolver
/// depending on whether `disc` is `Some`. Split out so the resolver
/// closure can live on the stack in the same scope as the call — that
/// keeps the closure's borrow of `disc` bounded by the call itself,
/// avoiding the need to box a `dyn Fn`.
fn parse_with_optional_sdk(
    source: &str,
    project_path: &Path,
    extras: &HashMap<String, String>,
    environment: &HashMap<String, String>,
    disc: Option<&SdkDiscovery>,
    evaluated: &Cell<u64>,
) -> Option<ParsedProject> {
    evaluated.set(evaluated.get() + 1);
    // Glob resolution is independent of SDK discovery, so both arms get the
    // filesystem-backed resolver. It borrows nothing, so it lives for the
    // whole function.
    let glob_resolver: &GlobResolver<'_> = &borzoi_msbuild::glob_resolver::resolve;
    match disc {
        Some(d) => {
            let resolver: &SdkResolver<'_> = &|name, observer| d.resolve(name, observer);
            parse_fsproj_with_imports(
                source,
                project_path,
                extras,
                environment,
                Some(resolver),
                Some(glob_resolver),
            )
            .ok()
        }
        None => parse_fsproj_with_imports(
            source,
            project_path,
            extras,
            environment,
            None,
            Some(glob_resolver),
        )
        .ok(),
    }
    .inspect(crate::observed::report_evaluation)
}

/// Recover the dotnet install root from an `SdkPaths::root`.
///
/// Layout (created by every modern .NET install + verified by the
/// `sdk_discovery::tests::install_stub_sdk` helper):
///
/// ```text
/// <install_root>/sdk/<version>/Sdks/<sdk_name>/Sdk     ← SdkPaths::root
/// <install_root>/packs/...                              ← what `resolve_assemblies` needs
/// ```
///
/// The install root is five `parent()` calls up. Returns `None` if the path
/// shape doesn't match (a defensive check; the SDK resolver guarantees the
/// shape, but verifying it makes a future layout change a graceful failure
/// instead of a wrong-directory crash).
fn install_root_from_sdk_path(sdk_path: &Path) -> Option<&Path> {
    if sdk_path.file_name()? != "Sdk" {
        return None;
    }
    let sdks_parent = sdk_path.parent()?.parent()?; // <sdk_name> → Sdks
    if sdks_parent.file_name()? != "Sdks" {
        return None;
    }
    let sdk_dir = sdks_parent.parent()?.parent()?; // <version> → sdk
    if sdk_dir.file_name()? != "sdk" {
        return None;
    }
    sdk_dir.parent()
}

/// Whether `file` is one of `items`' resolved `<Compile>` includes — the
/// machine-checkable definition of "this project owns this file" the F#
/// compiler uses, replacing the directory heuristic's guess.
///
/// Comparison is **lexical** (via [`lexically_normalize`]), not
/// [`std::fs::canonicalize`]: the msbuild parser passes literal includes
/// through whether or not they exist on disk, so a freshly-`<Compile>`d file
/// the user hasn't created yet must still count as owned. Both sides are
/// already absolute — `item.include` is joined onto the project directory by
/// the parser, and callers pass an absolute `file` — so normalisation only
/// folds `.`/`..`/separator spelling, never resolves symlinks. Two paths that
/// differ only by symlink therefore compare unequal; acceptable because F#
/// Compile includes are lexical relative paths in practice (see
/// `docs/fsproj-consumption-plan.md` decision C3).
///
/// Case sensitivity follows the platform's default filesystem (see
/// [`paths_equal`]): MSBuild/F# treat `Lib.fs` and `lib.fs` as the same file
/// on Windows and macOS, so we must too, or `symbols_for` would fall back to
/// the wrong project on a mere casing difference.
fn project_contains(items: &[ResolvedItem], file: &Path) -> bool {
    let target = lexically_normalize(file);
    items
        .iter()
        .any(|item| paths_equal(&lexically_normalize(&item.include), &target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::fs;
    use tempfile::TempDir;

    /// [`find_owning_project`], listing through listings nobody keeps.
    fn heuristic(file: &Path) -> Option<PathBuf> {
        find_owning_project(file, &mut DirListings::default(), &mut Vec::new())
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    // ---- install_root_from_sdk_path ----

    #[test]
    fn install_root_from_sdk_path_recovers_dotnet_root() {
        // The shape `SdkDiscovery::resolve` returns is
        // `<install_root>/sdk/<version>/Sdks/<sdk_name>/Sdk`. Recovery is
        // five `parent()` calls up; assertion proves the dotnet install
        // root (where `packs/` lives) — not the SDK import directory —
        // comes back.
        let sdk_path = Path::new("/usr/share/dotnet/sdk/10.0.401/Sdks/Microsoft.NET.Sdk/Sdk");
        assert_eq!(
            install_root_from_sdk_path(sdk_path),
            Some(Path::new("/usr/share/dotnet"))
        );
    }

    #[test]
    fn install_root_from_sdk_path_returns_none_on_unexpected_layout() {
        // A defensive failure case: if the resolver ever returns a path with
        // a different shape, return None rather than silently lopping off
        // arbitrary parent directories.
        assert_eq!(
            install_root_from_sdk_path(Path::new("/usr/share/dotnet/sdk/10.0.401")),
            None
        );
        assert_eq!(
            install_root_from_sdk_path(Path::new(
                "/usr/share/dotnet/somewhere/else/Microsoft.NET.Sdk/Sdk"
            )),
            None
        );
        assert_eq!(install_root_from_sdk_path(Path::new("/")), None);
    }

    /// Minimal SDK-less fsproj. The msbuild evaluator accepts a `<Project>`
    /// root without an `Sdk` attribute (it just doesn't get SDK property
    /// contributions). Good enough for `DefineConstants` round-tripping.
    fn fsproj_with_defines(defines: &str) -> String {
        format!(
            r#"<Project>
              <PropertyGroup>
                <DefineConstants>{defines}</DefineConstants>
              </PropertyGroup>
            </Project>"#
        )
    }

    /// Minimal SDK-less fsproj that compiles exactly one file. `include` is a
    /// literal (no glob), so the parser resolves it without the glob resolver,
    /// joining it onto the project directory.
    fn fsproj_with_compile(include: &str) -> String {
        format!(
            r#"<Project>
              <ItemGroup>
                <Compile Include="{include}" />
              </ItemGroup>
            </Project>"#
        )
    }

    /// SDK-less fsproj with the given `<ProjectReference>` includes (relative
    /// to the project's own directory), in document order.
    fn fsproj_with_refs(refs: &[&str]) -> String {
        let items: String = refs
            .iter()
            .map(|r| format!("                <ProjectReference Include=\"{r}\" />\n"))
            .collect();
        format!(
            r#"<Project>
              <ItemGroup>
{items}              </ItemGroup>
            </Project>"#
        )
    }

    fn fsproj_with_define(define: &str) -> String {
        format!(
            r#"<Project>
              <PropertyGroup><DefineConstants>{define}</DefineConstants></PropertyGroup>
            </Project>"#
        )
    }

    fn fsproj_with_lang(lang: &str) -> String {
        format!(
            r#"<Project>
              <PropertyGroup><LangVersion>{lang}</LangVersion></PropertyGroup>
            </Project>"#
        )
    }

    #[test]
    fn lang_version_for_project_reads_declared_version() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(&proj, &fsproj_with_lang("11.0"));
        let mut ws = Workspace::default();
        assert_eq!(ws.lang_version_for_project(&proj), LanguageVersion::V11_0);
    }

    #[test]
    fn lang_version_for_project_resolves_preview_alias() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(&proj, &fsproj_with_lang("preview"));
        let mut ws = Workspace::default();
        assert_eq!(ws.lang_version_for_project(&proj), LanguageVersion::Preview);
    }

    #[test]
    fn lang_version_for_project_absent_is_fcs_default_10() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(&proj, &fsproj_with_defines("")); // no <LangVersion>
        let mut ws = Workspace::default();
        assert_eq!(ws.lang_version_for_project(&proj), LanguageVersion::DEFAULT);
    }

    #[test]
    fn lang_version_for_project_unrecognised_falls_back_to_default() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(&proj, &fsproj_with_lang("totally-bogus"));
        let mut ws = Workspace::default();
        assert_eq!(ws.lang_version_for_project(&proj), LanguageVersion::DEFAULT);
    }

    /// An *untrusted-provenance* `<LangVersion>` (written under a gate the
    /// evaluator couldn't pin) still serves its evaluated value — see
    /// `lang_version_for_project`'s doc: with no uniformly-permissive
    /// version to decline to (Preview *raises* strict-indentation /
    /// invalid-decls severities), the evaluated value remains the best
    /// single guess, and a wrong one only shades diagnostics, never
    /// fabricates resolution.
    #[test]
    fn untrusted_lang_version_still_serves_the_evaluated_value() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup Condition="'$(DefineConstants)' == ''">
                <LangVersion>5.0</LangVersion>
              </PropertyGroup>
            </Project>"#,
        );
        let mut ws = Workspace::default();
        assert_eq!(ws.lang_version_for_project(&proj), LanguageVersion::V5_0);
    }

    #[test]
    fn lang_version_for_orphan_file_is_preview() {
        // No owning project → we cannot know the version, so don't guess-flag.
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("Orphan.fs");
        write_file(&file, "let x = 1\n");
        let mut ws = Workspace::default();
        assert_eq!(ws.lang_version_for(&file), LanguageVersion::Preview);
    }

    // ---- TFM selection (fsproj 3.3c stage 1, plan E1/E2/E5) ----

    /// SDK-less multi-targeted fsproj with one `$(TargetFramework)`-gated
    /// `<DefineConstants>` group per (gate, symbol) pair.
    fn fsproj_multi_tfm(tfms: &str, gated_defines: &[(&str, &str)]) -> String {
        let groups: String = gated_defines
            .iter()
            .map(|(gate, symbol)| {
                format!(
                    "              <PropertyGroup Condition=\"'$(TargetFramework)' == '{gate}'\">\n                <DefineConstants>{symbol}</DefineConstants>\n              </PropertyGroup>\n"
                )
            })
            .collect();
        format!(
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>{tfms}</TargetFrameworks>
              </PropertyGroup>
{groups}            </Project>"#
        )
    }

    #[test]
    fn multi_target_project_serves_first_declared_tfm() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            &fsproj_multi_tfm("net8.0;net10.0", &[("net8.0", "EIGHT"), ("net10.0", "TEN")]),
        );
        let mut ws = Workspace::default();
        let symbols = ws.symbols_for_project(&proj);
        assert!(
            symbols.contains("EIGHT"),
            "first-declared TFM's gated define must apply: {symbols:?}"
        );
        assert!(
            !symbols.contains("TEN"),
            "the other TFM's branch must be off: {symbols:?}"
        );
        assert_eq!(
            ws.target_framework_for_project(&proj),
            Some("net8.0".to_string())
        );
    }

    #[test]
    fn multi_target_project_defines_are_certain_under_the_seed() {
        // The whole point of pass 2: the gated group's condition becomes
        // evaluable, so `define_constants_uncertain` stops flipping and the
        // project can fold (semantic::parses_for_project gates on it).
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            &fsproj_multi_tfm("net8.0;net10.0", &[("net8.0", "EIGHT")]),
        );
        let mut ws = Workspace::default();
        let parsed = ws.project(&proj).expect("evaluates");
        assert!(
            !parsed.define_constants_uncertain,
            "seeded TargetFramework must make the gate evaluable: {:?}",
            parsed.diagnostics
        );
    }

    #[test]
    fn multi_target_project_compile_items_fold() {
        // A $(TargetFramework)-gated <Compile> resolves under the chosen TFM
        // instead of flipping items_uncertain.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
              </PropertyGroup>
              <ItemGroup Condition="'$(TargetFramework)' == 'net8.0'">
                <Compile Include="Eight.fs" />
              </ItemGroup>
              <ItemGroup Condition="'$(TargetFramework)' == 'net10.0'">
                <Compile Include="Ten.fs" />
              </ItemGroup>
            </Project>"#,
        );
        let mut ws = Workspace::default();
        let parsed = ws.project(&proj).expect("evaluates");
        assert!(
            !parsed.items_uncertain,
            "{:?}",
            parsed.compile_condition_uncertainties
        );
        let includes: Vec<_> = parsed
            .items
            .iter()
            .map(|i| i.include.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(includes, vec!["Eight.fs".to_string()]);
    }

    #[test]
    fn single_tfm_project_records_chosen_tfm() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup>
            </Project>"#,
        );
        let mut ws = Workspace::default();
        assert_eq!(
            ws.target_framework_for_project(&proj),
            Some("net10.0".to_string())
        );
    }

    #[test]
    fn no_tfm_project_records_none() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(&proj, &fsproj_with_defines("FOO"));
        let mut ws = Workspace::default();
        assert_eq!(ws.target_framework_for_project(&proj), None);
        // ... and the rest of the evaluation is unaffected.
        assert!(ws.symbols_for_project(&proj).contains("FOO"));
    }

    #[test]
    fn caller_supplied_target_framework_overrides_first_declared() {
        // A harness that seeds TargetFramework owns the choice: no pass-2
        // re-seed, and the recorded TFM is the caller's.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            &fsproj_multi_tfm("net8.0;net10.0", &[("net8.0", "EIGHT"), ("net10.0", "TEN")]),
        );
        let extra = HashMap::from([("TargetFramework".to_string(), "net10.0".to_string())]);
        let mut ws =
            Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);
        let symbols = ws.symbols_for_project(&proj);
        assert!(symbols.contains("TEN"), "{symbols:?}");
        assert!(!symbols.contains("EIGHT"), "{symbols:?}");
        assert_eq!(
            ws.target_framework_for_project(&proj),
            Some("net10.0".to_string())
        );
    }

    #[test]
    fn caller_supplied_empty_target_framework_disables_seeding() {
        // An empty read-only global is the caller explicitly saying "no TFM".
        // Re-seeding would override that input (and a naive insert would trip
        // the evaluator's case-insensitive duplicate-key validation).
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            &fsproj_multi_tfm("net8.0;net10.0", &[("net8.0", "EIGHT")]),
        );
        let extra = HashMap::from([("TargetFramework".to_string(), String::new())]);
        let mut ws =
            Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);
        assert!(
            ws.project(&proj).is_some(),
            "evaluation must not fail on the caller's empty TargetFramework"
        );
        let symbols = ws.symbols_for_project(&proj);
        assert!(!symbols.contains("EIGHT"), "{symbols:?}");
        assert_eq!(ws.target_framework_for_project(&proj), None);
    }

    #[test]
    fn caller_supplied_target_framework_key_is_case_insensitive() {
        // MSBuild property names compare OrdinalIgnoreCase; a differently-cased
        // caller key must be honoured, not duplicated (validate_inputs rejects
        // case-insensitive duplicate extra_properties keys, and a cached None
        // would permanently disable the project).
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            &fsproj_multi_tfm("net8.0;net10.0", &[("net10.0", "TEN")]),
        );
        let extra = HashMap::from([("targetframework".to_string(), "net10.0".to_string())]);
        let mut ws =
            Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);
        assert!(ws.project(&proj).is_some(), "evaluation must succeed");
        assert!(ws.symbols_for_project(&proj).contains("TEN"));
        assert_eq!(
            ws.target_framework_for_project(&proj),
            Some("net10.0".to_string())
        );
    }

    #[test]
    fn single_tfm_body_and_plural_both_set_prefers_singular() {
        // MSBuild: a non-empty $(TargetFramework) makes it a single-target
        // build even when <TargetFrameworks> is also set.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
                <TargetFramework>net10.0</TargetFramework>
              </PropertyGroup>
            </Project>"#,
        );
        let mut ws = Workspace::default();
        assert_eq!(
            ws.target_framework_for_project(&proj),
            Some("net10.0".to_string())
        );
    }

    /// An entry-project body `TargetFramework` written under a gate the
    /// evaluator couldn't pin is not current truth — the real build may
    /// select a different TFM — so the served-TFM accessor must decline
    /// rather than hand the assembly-env layer a value to select an assets
    /// target on trust. The entry-side mirror of `resolve_node_uncached`'s
    /// `NodeTfm::Unresolved` demotion (3.3d round 18): same flag, different
    /// consumer.
    #[test]
    fn untrusted_body_tfm_declines_target_framework_for_project() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup Condition="'$(DefineConstants)' == ''">
                <TargetFramework>net8.0</TargetFramework>
              </PropertyGroup>
            </Project>"#,
        );
        let mut ws = Workspace::default();
        assert_eq!(
            ws.target_framework_for_project(&proj),
            None,
            "an untrusted body TargetFramework must not be served"
        );
    }

    /// The unpinned singular blocks even the first-declared fallback: an
    /// empty `TargetFramework` written under an unpinnable gate means the
    /// real build may be a *single* build under a value we can't see, so
    /// first-declared-of-the-plural proves nothing (the same reasoning as
    /// `resolve_node_uncached`'s demotion, which fires before the declared
    /// list is consulted).
    #[test]
    fn untrusted_empty_singular_blocks_first_declared_fallback() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup Condition="'$(DefineConstants)' == ''">
                <TargetFramework></TargetFramework>
              </PropertyGroup>
              <PropertyGroup>
                <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
              </PropertyGroup>
            </Project>"#,
        );
        let mut ws = Workspace::default();
        assert_eq!(ws.target_framework_for_project(&proj), None);
    }

    /// The parse side of the untrusted-empty-singular case must not commit
    /// to the first-declared TFM either: seeding pass 2 would evaluate
    /// `$(TargetFramework)`-gated defines cleanly under a choice we just
    /// declined to serve, pairing net8.0-parsed sources with whatever the
    /// env's no-TFM fallback selects (the E5 incoherence the accessor's
    /// decline exists to prevent). Without the seed, the gated define reads
    /// the *unpinned* pass-1 `TargetFramework` and flips
    /// `define_constants_uncertain`, refusing the fold outright.
    #[test]
    fn untrusted_empty_singular_does_not_seed_first_declared_parses() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup Condition="'$(DefineConstants)' == ''">
                <TargetFramework></TargetFramework>
              </PropertyGroup>
              <PropertyGroup>
                <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
              </PropertyGroup>
              <PropertyGroup Condition="'$(TargetFramework)' == 'net8.0'">
                <DefineConstants>EIGHT</DefineConstants>
              </PropertyGroup>
            </Project>"#,
        );
        let mut ws = Workspace::default();
        let parsed = ws.project(&proj).expect("evaluates");
        assert!(
            parsed.define_constants_uncertain,
            "a TFM-gated define must read the unpinned TargetFramework, not a \
             first-declared seed we declined to serve"
        );
        assert!(
            !parsed.define_constants.contains(&"EIGHT".to_string()),
            "{:?}",
            parsed.define_constants
        );
    }

    /// A caller-supplied `TargetFramework` global is immune to body-write
    /// provenance: globals out-rank body writes, and the caller's value
    /// needs no provenance — the same ordering `resolve_node_uncached`
    /// applies (`caller_owns_tfm` before the untrusted check).
    #[test]
    fn caller_supplied_tfm_is_immune_to_untrusted_body_writes() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup Condition="'$(DefineConstants)' == ''">
                <TargetFramework>net8.0</TargetFramework>
              </PropertyGroup>
            </Project>"#,
        );
        let extra = HashMap::from([("TargetFramework".to_string(), "net10.0".to_string())]);
        let mut ws =
            Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);
        assert_eq!(
            ws.target_framework_for_project(&proj),
            Some("net10.0".to_string())
        );
    }

    /// A caller-supplied **empty** `TargetFramework` global is the outer
    /// dispatch build, not a TFM choice, so `chosen_tfm` is `None` while the
    /// project still declares its sole TFM. The node is then `Unresolved`, and
    /// both of the other two verdicts would be wrong in opposite directions:
    /// `NoneDeclared` tells the env the project's lone restored output is the
    /// one to fold, and `Known(sole)` pairs the inner build's identity with an
    /// `output_name` and reference list computed under the *outer* build, both
    /// of which may be TFM-gated.
    #[test]
    fn an_empty_tfm_global_leaves_the_sole_declaration_unresolved() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            "<Project><PropertyGroup><TargetFrameworks>net8.0</TargetFrameworks></PropertyGroup></Project>",
        );
        let extra = HashMap::from([("TargetFramework".to_string(), String::new())]);
        let mut ws =
            Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);

        assert_eq!(ws.parsed_tfm_for_project(&proj), None);
        let graph = ws.project_graph(&proj);
        let node = graph
            .nodes
            .iter()
            .find(|n| paths_equal(&n.path, &proj))
            .expect("entry node");
        assert_eq!(node.tfm, NodeTfm::Unresolved);
        assert_eq!(node.output_name, None, "declines with the TFM");
    }

    /// An override document whose `<ProjectReference>` is gated on
    /// `'$(TargetFramework)' == ''` — an edge only the **outer dispatch build**
    /// takes, which every real inner build strips.
    ///
    /// This is the shape that makes "serve pass 1 and mark it untrusted"
    /// insufficient on its own. `TargetFramework` is never *written* by a
    /// multi-targeted document, so pass 1 decides that gate cleanly under the
    /// evaluator's environment model — no diagnostic, no
    /// `project_references_uncertain` — and the edge is captured as fact. The
    /// compile closure would then follow a project the real build never
    /// references, and fold its assemblies.
    #[test]
    fn an_override_document_does_not_publish_outer_build_only_edges() {
        let tmp = TempDir::new().unwrap();
        let dep = tmp.path().join("Dep.fsproj");
        write_file(&dep, "<Project><PropertyGroup/></Project>");
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project TreatAsLocalProperty="TargetFramework">
              <PropertyGroup>
                <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
              </PropertyGroup>
              <PropertyGroup>
                <TargetFramework Condition="'$(TargetFramework)' != ''">net10.0</TargetFramework>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="Dep.fsproj" Condition="'$(TargetFramework)' == ''" />
              </ItemGroup>
            </Project>"#,
        );
        let ws = Workspace::default();

        // The *compile closure* walk — the one whose edges become the env
        // fold's assemblies. (`project_graph`'s declared-structure walk
        // deliberately keeps reporting the elements the document declares; it
        // feeds cycle diagnostics, not assembly resolution.)
        let graph = ws.project_graph_with_producer_tfms(
            &proj,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        let node = graph
            .nodes
            .iter()
            .find(|n| paths_equal(&n.path, &proj))
            .expect("entry node");
        assert!(
            node.references.is_empty(),
            "an outer-build-only edge must not reach the compile closure: {:?}",
            node.references
        );
        assert!(
            node.references_uncertain,
            "and the emptied list must say it was suppressed, not that the project \
             references nothing"
        );
    }

    /// A multi-targeted project that opts `TargetFramework` out of global
    /// read-only-ness and overwrites it once the seed is non-empty — a write
    /// pass 1 cannot see, because pass 1 has no seed. `outer_gate` wraps that
    /// write in a group the evaluator cannot pin.
    /// An override whose *value* the evaluator refuses (`@(Tfms)` is an item
    /// reference, and we have no item evaluator). MSBuild still performs the
    /// assignment; our walk drops the name from the property table entirely.
    ///
    /// So the served evaluation is a chimera exactly as for a plain override,
    /// but the property table looks identical to a document that never
    /// overrode anything — absence there conflates "no write" with "a write we
    /// refused". The decline is therefore keyed on the document's
    /// `TreatAsLocalProperty` opt-out, which is a syntactic fact and states the
    /// same thing whatever the write turns out to be.
    #[test]
    fn an_override_the_evaluator_refuses_still_declines() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            &fsproj_pass2_override("net8.0;net10.0", false, "@(Tfms)"),
        );
        let mut ws = Workspace::default();

        assert_eq!(ws.served_tfm_for_project(&proj), ServedTfm::Untrusted);
        assert_eq!(ws.parsed_tfm_for_project(&proj), None);
    }

    /// A *caller-supplied* `TargetFramework` global is subject to the same
    /// opt-out as one we seed ourselves: `TreatAsLocalProperty` lets the
    /// document overwrite whichever global carries the name, and the caller's
    /// ownership of the choice is a reason to skip *provenance* checks, not a
    /// reason to believe an evaluation that did not honour it.
    ///
    /// Otherwise the graph's producer-TFM seeding and any harness constructed
    /// with `with_env_and_extra_build_properties` would report the TFM they
    /// asked for while the defines and items came from another.
    #[test]
    fn a_caller_owned_tfm_declines_when_the_document_can_overwrite_it() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            &fsproj_pass2_override("net8.0;net10.0", false, "net10.0"),
        );
        let extra = HashMap::from([("TargetFramework".to_string(), "net8.0".to_string())]);
        let mut ws =
            Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);

        assert_eq!(
            ws.served_tfm_for_project(&proj),
            ServedTfm::Untrusted,
            "the caller asked for net8.0 and the document wrote net10.0"
        );
        assert_eq!(ws.parsed_tfm_for_project(&proj), None);
    }

    /// The graph node's mirror of
    /// `a_caller_owned_tfm_declines_when_the_document_can_overwrite_it`: the
    /// walk has its own caller-owned fast path, and it must consult
    /// `not_an_inner_build` too.
    ///
    /// `NodeTfm::NoneDeclared` would be the wrong decline here — it tells the
    /// env "this project declares no TFM, so its sole restored output is the
    /// one", which for a multi-targeted project licenses folding whichever
    /// stale TFM directory happens to be lying around. `Unresolved` is the
    /// verdict that means "we cannot say", and it folds nothing.
    #[test]
    fn a_caller_owned_graph_node_declines_when_the_document_can_overwrite_it() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            &fsproj_pass2_override("net8.0;net10.0", false, "net10.0"),
        );
        let extra = HashMap::from([("TargetFramework".to_string(), "net8.0".to_string())]);
        let ws = Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);

        let graph = ws.project_graph_with_producer_tfms(
            &proj,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        let node = graph
            .nodes
            .iter()
            .find(|n| paths_equal(&n.path, &proj))
            .expect("entry node");
        assert_eq!(node.tfm, NodeTfm::Unresolved);
        assert_eq!(
            node.output_name, None,
            "a TFM-unresolved node's per-TFM output name cannot be pinned either"
        );
    }

    /// A producer-seeded node is labelled by *its own* evaluation, so an
    /// opt-out only that evaluation can see still declines.
    ///
    /// The first-declared pass (`outer`) cannot see this one: the
    /// `TreatAsLocalProperty` root is imported behind a
    /// `'$(TargetFramework)' == 'net10.0'` gate, so it is reached only when the
    /// restore's producer TFM seeds `net10.0`. The node's TFM and output name
    /// come from the seeded evaluation, so they must decline with it —
    /// otherwise a restore selecting that TFM folds a DLL from a build whose
    /// own seed was overwritten.
    #[test]
    fn a_producer_seeded_node_declines_on_its_own_evaluation() {
        let tmp = TempDir::new().unwrap();
        write_file(
            &tmp.path().join("Local.props"),
            r#"<Project TreatAsLocalProperty="TargetFramework">
              <PropertyGroup>
                <TargetFramework Condition="'$(TargetFramework)' != ''">net8.0</TargetFramework>
              </PropertyGroup>
            </Project>"#,
        );
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
              </PropertyGroup>
              <Import Project="Local.props" Condition="'$(TargetFramework)' == 'net10.0'" />
            </Project>"#,
        );
        let ws = Workspace::default();
        // The map's keys are *canonicalised* (`project_graph_impl`), which on
        // macOS differs from lexical normalisation: `/tmp` is a symlink to
        // `/private/tmp`, so a lexically-normalised key silently never matches
        // and the node is walked unseeded.
        let producers = BTreeMap::from([(
            crate::observed::host_fs::canonicalize(&proj).unwrap(),
            "net10.0".to_string(),
        )]);

        let graph = ws.project_graph_with_producer_tfms(
            &proj,
            &producers,
            &borzoi_msbuild::observed::Observer::new(),
        );
        let node = graph
            .nodes
            .iter()
            .find(|n| paths_equal(&n.path, &proj))
            .expect("entry node");
        assert_eq!(
            node.tfm,
            NodeTfm::Unresolved,
            "the seeded evaluation overwrote its own seed"
        );
        assert_eq!(node.output_name, None, "declines with the TFM");
    }

    /// The parse-side twin of
    /// `an_override_document_does_not_publish_outer_build_only_edges`: defines
    /// and Compile items gated on `'$(TargetFramework)' == ''` belong to the
    /// outer dispatch build, and no inner build has them.
    ///
    /// The served pass *is* that outer build, and nothing in it flags itself —
    /// a multi-targeted document never writes the singular, so that gate is
    /// decided cleanly under the environment model, with no diagnostic and no
    /// `*_uncertain` flag. Discarding the seeded pass without marking the one
    /// that replaces it would therefore hand the semantic layer an outer-only
    /// `DefineConstants` as fact.
    ///
    /// The assertion is on the flags rather than on `symbols_for_project`,
    /// because the flags are the operative control: `parses_for_project`
    /// refuses the whole fold on `items_uncertain ||
    /// define_constants_uncertain` (`semantic.rs`), degrading to single-file
    /// analysis. `symbols_for_project` deliberately still reports the raw set —
    /// that is the standing contract for every untrusted project, not something
    /// this shape changes.
    #[test]
    fn an_override_document_publishes_no_outer_build_only_symbols() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project TreatAsLocalProperty="TargetFramework">
              <PropertyGroup>
                <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
              </PropertyGroup>
              <PropertyGroup Condition="'$(TargetFramework)' == ''">
                <DefineConstants>OUTER_ONLY</DefineConstants>
              </PropertyGroup>
              <PropertyGroup>
                <TargetFramework Condition="'$(TargetFramework)' != ''">net10.0</TargetFramework>
              </PropertyGroup>
            </Project>"#,
        );
        let mut ws = Workspace::default();

        let parsed = ws.project(&proj).expect("evaluates");
        assert!(
            parsed.define_constants_uncertain,
            "outer-only defines must not be served as fact"
        );
        assert!(parsed.items_uncertain, "nor the outer build's Compile set");
        assert!(
            parsed.project_references_uncertain,
            "nor its reference list"
        );
    }

    /// An **empty** caller-supplied global is still a global the document can
    /// overwrite. `caller_owns_target_framework` is the wrong question here —
    /// it asks "did the caller make a TFM *choice*", and an empty global is
    /// deliberately not one — where what matters is only whether a global of
    /// that name is present for `TreatAsLocalProperty` to unprotect.
    ///
    /// Without this the document overwrites the empty seed, the evaluation runs
    /// under the write, and we report `NoneDeclared` with the items and defines
    /// still marked certain.
    #[test]
    fn an_empty_caller_global_is_still_overwritable() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            &fsproj_pass2_override("net8.0;net10.0", false, "net10.0"),
        );
        let extra = HashMap::from([("TargetFramework".to_string(), String::new())]);
        let mut ws =
            Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);

        assert_eq!(ws.served_tfm_for_project(&proj), ServedTfm::Untrusted);
        let parsed = ws.project(&proj).expect("evaluates");
        assert!(parsed.define_constants_uncertain);
        assert!(parsed.items_uncertain);
    }

    /// `TreatAsLocalProperty` may itself be computed: MSBuild expands `$(…)` in
    /// the attribute and honours the result (probed, dotnet 10.0.301 — with
    /// `LocalNames=TargetFramework` supplied as a global, the body write beats
    /// the seed). We record the attribute's raw text, so an entry we cannot
    /// resolve to a name is exactly the "we did not look" case, and it declines
    /// like any other opt-out.
    ///
    /// Over-conservative when the property expands to some other name — but
    /// only ever in the declining direction, and it costs nothing measurable:
    /// no real project opts `TargetFramework` out at all, computed or literal.
    #[test]
    fn a_computed_opt_out_declines_because_we_cannot_read_it() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        write_file(
            &proj,
            r#"<Project TreatAsLocalProperty="$(LocalNames)">
              <PropertyGroup>
                <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
              </PropertyGroup>
              <PropertyGroup>
                <TargetFramework Condition="'$(TargetFramework)' != ''">net10.0</TargetFramework>
              </PropertyGroup>
            </Project>"#,
        );
        let mut ws = Workspace::default();

        assert_eq!(ws.served_tfm_for_project(&proj), ServedTfm::Untrusted);
        assert_eq!(ws.parsed_tfm_for_project(&proj), None);
    }

    fn fsproj_pass2_override(declared: &str, outer_gate: bool, value: &str) -> String {
        let (open, close) = if outer_gate {
            (
                "<PropertyGroup Condition=\"'$(DefineConstants)' == ''\">",
                "</PropertyGroup>",
            )
        } else {
            ("<PropertyGroup>", "</PropertyGroup>")
        };
        format!(
            r#"<Project TreatAsLocalProperty="TargetFramework">
              <PropertyGroup>
                <TargetFrameworks>{declared}</TargetFrameworks>
              </PropertyGroup>
              {open}
                <TargetFramework Condition="'$(TargetFramework)' != ''">{value}</TargetFramework>
              {close}
            </Project>"#
        )
    }

    /// A pass-2 `TreatAsLocalProperty` override is served like any other body
    /// write: on its own provenance. Pass 1's verdict cannot cover it — the
    /// write does not exist there — so `evaluate_project` widens with pass 2's,
    /// and *only* when an override actually fired (the generic provenance seam
    /// fires unconditionally for real SDK projects, so an unguarded consult
    /// would decline every multi-targeted project — `msbuild-trust-audit` §2).
    ///
    /// Every surface that publishes a TFM must publish the *same* one for the
    /// same document, because they are all describing one evaluation. There are
    /// three: what the parse ran under, what the assembly env may key assets
    /// selection on, and how the project-graph node is labelled for a consumer
    /// locating its output.
    ///
    /// "Agrees" is commit-or-decline, not equality. Each surface may decline —
    /// and the graph declines a *multi*-targeted node outright absent a restore
    /// seed, whatever any override did, because which TFM a consumer would
    /// reference is genuinely open (rows 1-3, and equally true of a plain
    /// multi-targeted project). What none of them may do is commit to a TFM
    /// other than the one the parse ran under.
    ///
    /// One table over all three rather than three per-surface tests, because
    /// `TreatAsLocalProperty="TargetFramework"` is cross-cutting: it lets a
    /// document overwrite the inner-build seed, and every surface downstream of
    /// that seed has to answer for it. A per-surface test only ever asks one of
    /// them, so it can pass while the others are wrong; enumerating the
    /// surfaces against one fixture asks all of them at once, which is the
    /// property worth having for the next cross-cutting shape.
    ///
    /// **Every override declines, on every surface, whatever it wrote.** The
    /// rows differ in what the document says and agree in what we serve, which
    /// is the point: the seeded pass is discarded, the outer dispatch build is
    /// served in its place, and no TFM is reported for it (see
    /// [`tfm_policy::reseed_outcome`] for why no single TFM describes such a
    /// pass). The variety that remains — a clean override, an unpinned one, one
    /// that clears the TFM, one over a sole declaration — exists so that any
    /// implementation which *starts* distinguishing them again fails here.
    #[test]
    fn every_tfm_surface_agrees_on_a_pass_two_override() {
        struct Case {
            declared: &'static str,
            outer_gate: bool,
            value: &'static str,
            served: ServedTfm,
            /// The TFM the served parse ran under — what its defines, Compile
            /// items and `.fsproj` buffer diagnostics describe (plan E7).
            /// `None` throughout here: the served parse is the outer dispatch
            /// build, which runs under no TFM at all.
            ran_under: Option<&'static str>,
            node: NodeTfm,
        }
        let cases = [
            Case {
                declared: "net8.0;net10.0",
                outer_gate: false,
                value: "net10.0",
                served: ServedTfm::Untrusted,
                ran_under: None,
                node: NodeTfm::Unresolved,
            },
            Case {
                declared: "net8.0;net10.0",
                outer_gate: true,
                value: "net10.0",
                served: ServedTfm::Untrusted,
                ran_under: None,
                node: NodeTfm::Unresolved,
            },
            Case {
                declared: "net8.0;net10.0",
                outer_gate: false,
                value: "",
                served: ServedTfm::Untrusted,
                ran_under: None,
                node: NodeTfm::Unresolved,
            },
            // A *sole* declaration takes the graph's single-target arm, which
            // labelled the node from the declaration rather than from the
            // evaluation its edges came from.
            Case {
                declared: "net8.0",
                outer_gate: false,
                value: "net10.0",
                served: ServedTfm::Untrusted,
                ran_under: None,
                node: NodeTfm::Unresolved,
            },
        ];
        // Crossed with whether the *caller* supplied the `TargetFramework`
        // global. Both `served_tfm_for_project` and `resolve_node_uncached`
        // keep a caller-owned fast path that answers before consulting the
        // untrusted state, and each has to decline ahead of it — caller
        // ownership excuses a value's provenance, not an evaluation the
        // document was free to conduct under a different TFM. Crossing the axis
        // in here asks every surface both ways at once.
        for caller_seeded in [false, true] {
            for case in &cases {
                let tmp = TempDir::new().unwrap();
                let proj = tmp.path().join("Sample.fsproj");
                write_file(
                    &proj,
                    &fsproj_pass2_override(case.declared, case.outer_gate, case.value),
                );
                let extra = if caller_seeded {
                    HashMap::from([("TargetFramework".to_string(), "net8.0".to_string())])
                } else {
                    HashMap::new()
                };
                let mut ws = Workspace::with_env_and_extra_build_properties(
                    SdkDiscoveryEnv::default(),
                    extra,
                );
                let label = format!(
                    "declared={} outer_gate={} value={:?} caller_seeded={caller_seeded}",
                    case.declared, case.outer_gate, case.value
                );

                assert_eq!(ws.served_tfm_for_project(&proj), case.served, "{label}");
                assert_eq!(
                    ws.parsed_tfm_for_project(&proj).as_deref(),
                    case.ran_under,
                    "{label}"
                );
                let graph = ws.project_graph_with_producer_tfms(
                    &proj,
                    &BTreeMap::new(),
                    &borzoi_msbuild::observed::Observer::new(),
                );
                let node = graph
                    .nodes
                    .iter()
                    .find(|n| paths_equal(&n.path, &proj))
                    .unwrap_or_else(|| panic!("entry node missing: {label}"));
                assert_eq!(node.tfm, case.node, "{label}");
            }
        }
    }

    proptest! {
        /// E1 determinism: whatever the declared list, the served TFM is the
        /// first entry, only its gated define applies, and the choice is a
        /// pure function of the project (re-evaluation agrees).
        #[test]
        fn chosen_tfm_is_always_first_declared(
            tfms in proptest::collection::vec("net(1[0-2]|[5-9])\\.0", 1..4)
        ) {
            // Deduplicate while preserving order: duplicate TFM entries would
            // make "which gated group fired" ambiguous.
            let mut seen = HashSet::new();
            let tfms: Vec<String> = tfms.into_iter().filter(|t| seen.insert(t.clone())).collect();
            let gated: Vec<(String, String)> = tfms
                .iter()
                .enumerate()
                .map(|(i, t)| (t.clone(), format!("SYM_{i}")))
                .collect();
            let gated_refs: Vec<(&str, &str)> =
                gated.iter().map(|(g, s)| (g.as_str(), s.as_str())).collect();

            let tmp = TempDir::new().unwrap();
            let proj = tmp.path().join("Sample.fsproj");
            write_file(&proj, &fsproj_multi_tfm(&tfms.join(";"), &gated_refs));

            let mut ws = Workspace::default();
            prop_assert_eq!(ws.target_framework_for_project(&proj), Some(tfms[0].clone()));
            let symbols = ws.symbols_for_project(&proj);
            for (i, _) in tfms.iter().enumerate() {
                let sym = format!("SYM_{i}");
                prop_assert_eq!(symbols.contains(&sym), i == 0, "symbol {}: {:?}", sym, symbols);
            }
            // Determinism: a fresh workspace picks the same TFM.
            let mut ws2 = Workspace::default();
            prop_assert_eq!(ws2.target_framework_for_project(&proj), Some(tfms[0].clone()));
        }

        /// Provenance is the only difference: the same body
        /// `TargetFramework` value is served when written cleanly and
        /// declined when its write sits behind a gate the evaluator
        /// couldn't pin.
        #[test]
        fn served_tfm_tracks_body_write_provenance(
            tfm in "net(1[0-2]|[5-9])\\.0",
            gated in proptest::bool::ANY,
        ) {
            let group = if gated {
                r#"<PropertyGroup Condition="'$(DefineConstants)' == ''">"#
            } else {
                "<PropertyGroup>"
            };
            let tmp = TempDir::new().unwrap();
            let proj = tmp.path().join("Sample.fsproj");
            write_file(
                &proj,
                &format!(
                    "<Project>\n  {group}\n    <TargetFramework>{tfm}</TargetFramework>\n  </PropertyGroup>\n</Project>"
                ),
            );
            let mut ws = Workspace::default();
            let expected = if gated { None } else { Some(tfm) };
            prop_assert_eq!(ws.target_framework_for_project(&proj), expected);
        }
    }

    #[test]
    fn lang_version_for_file_in_project_uses_project_version() {
        // A file resolves to its (proximity-)owning project's version.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        let file = tmp.path().join("Lib.fs");
        write_file(&proj, &fsproj_with_lang("8.0"));
        write_file(&file, "let x = 1\n");
        let mut ws = Workspace::default();
        assert_eq!(ws.lang_version_for(&file), LanguageVersion::V8_0);
    }

    /// The normalised paths of a graph's nodes, in discovery order.
    fn node_paths(graph: &crate::project_graph::ProjectGraph) -> Vec<PathBuf> {
        graph.nodes.iter().map(|n| n.path.clone()).collect()
    }

    #[test]
    fn find_owning_project_finds_sibling() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        let file = tmp.path().join("Lib.fs");
        write_file(&proj, &fsproj_with_defines(""));
        write_file(&file, "let x = 1");

        assert_eq!(heuristic(&file), Some(proj));
    }

    #[test]
    fn find_owning_project_walks_up_to_parent_directory() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        let file = tmp.path().join("nested").join("deep").join("Lib.fs");
        write_file(&proj, &fsproj_with_defines(""));
        write_file(&file, "let x = 1");

        assert_eq!(heuristic(&file), Some(proj));
    }

    #[test]
    fn find_owning_project_returns_none_when_no_fsproj_in_tree() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("Lib.fs");
        write_file(&file, "let x = 1");

        // The temp dir's ancestors (system temp, then `/tmp`, then `/`)
        // are extremely unlikely to contain a `.fsproj`. If they do, this
        // assertion catches it as a test-environment issue.
        assert_eq!(heuristic(&file), None);
    }

    #[test]
    fn find_owning_project_picks_alphabetically_on_ties() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("Lib.fs");
        let zeta = tmp.path().join("Zeta.fsproj");
        let alpha = tmp.path().join("Alpha.fsproj");
        write_file(&file, "");
        write_file(&zeta, &fsproj_with_defines(""));
        write_file(&alpha, &fsproj_with_defines(""));

        assert_eq!(heuristic(&file), Some(alpha));
    }

    #[test]
    fn find_owning_project_prefers_closest_ancestor() {
        // Both `outer/` and `outer/inner/` have a `.fsproj`; the file
        // lives in `outer/inner/sub/`. The inner project wins.
        let tmp = TempDir::new().unwrap();
        let outer_proj = tmp.path().join("Outer.fsproj");
        let inner_proj = tmp.path().join("inner").join("Inner.fsproj");
        let file = tmp.path().join("inner").join("sub").join("Lib.fs");
        write_file(&outer_proj, &fsproj_with_defines(""));
        write_file(&inner_proj, &fsproj_with_defines(""));
        write_file(&file, "");

        assert_eq!(heuristic(&file), Some(inner_proj));
    }

    #[test]
    fn find_owning_project_only_matches_fsproj_extension() {
        // `.csproj`/`.vcxproj`/etc. are not F# projects and must not
        // claim the file (otherwise the LSP would feed wrong defines).
        let tmp = TempDir::new().unwrap();
        let csproj = tmp.path().join("Other.csproj");
        let file = tmp.path().join("Lib.fs");
        write_file(&csproj, "<Project/>");
        write_file(&file, "");

        assert_eq!(heuristic(&file), None);
    }

    // ----- Stage 1.1: membership-aware `Workspace::owning_project` -----

    #[test]
    fn owning_project_prefers_member_over_alphabetical_first() {
        // The regression flip: the alphabetically-first project does NOT list
        // the file; a later one does. `find_owning_project` would pick `Alpha`;
        // `owning_project` must pick `Zeta` because it actually compiles it.
        let tmp = TempDir::new().unwrap();
        let alpha = tmp.path().join("Alpha.fsproj");
        let zeta = tmp.path().join("Zeta.fsproj");
        let file = tmp.path().join("Lib.fs");
        write_file(&alpha, &fsproj_with_compile("Other.fs"));
        write_file(&zeta, &fsproj_with_compile("Lib.fs"));
        write_file(&file, "let x = 1");

        // Sanity: the membership-blind heuristic picks the wrong one here.
        assert_eq!(heuristic(&file), Some(alpha));

        let mut ws = Workspace::default();
        assert_eq!(ws.owning_project(&file), Some(zeta));
    }

    /// The reviewer's scenario: a client opens `lib.fs`, the owning project
    /// lists it as `Lib.fs`, and another project sorts first. On a
    /// case-insensitive filesystem the casing difference must not defeat
    /// membership. Gated to those platforms because on case-sensitive ones the
    /// two names genuinely are different files.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn owning_project_matches_listed_file_despite_case() {
        let tmp = TempDir::new().unwrap();
        let alpha = tmp.path().join("Alpha.fsproj");
        let zeta = tmp.path().join("Zeta.fsproj");
        write_file(&alpha, &fsproj_with_compile("Other.fs"));
        write_file(&zeta, &fsproj_with_compile("Lib.fs"));
        write_file(&tmp.path().join("Lib.fs"), "let x = 1");

        // Client opens the file with different casing than the project lists.
        let opened = tmp.path().join("lib.fs");
        let mut ws = Workspace::default();
        assert_eq!(ws.owning_project(&opened), Some(zeta));
    }

    #[test]
    fn owning_project_falls_back_to_alphabetical_when_no_member() {
        // Neither project lists `Lib.fs` (it's brand new). We must not return
        // `None`; we fall back to the nearest-ancestor, alphabetically-first
        // rule — exactly `find_owning_project`.
        let tmp = TempDir::new().unwrap();
        let alpha = tmp.path().join("Alpha.fsproj");
        let zeta = tmp.path().join("Zeta.fsproj");
        let file = tmp.path().join("Lib.fs");
        write_file(&alpha, &fsproj_with_compile("Other.fs"));
        write_file(&zeta, &fsproj_with_compile("Another.fs"));
        write_file(&file, "");

        let mut ws = Workspace::default();
        assert_eq!(ws.owning_project(&file), Some(alpha));
        assert_eq!(ws.owning_project(&file), heuristic(&file));
    }

    // ----- `symbols_for_linked` / `lang_version_for_linked` -----

    /// SDK-less fsproj listing one `<Compile>` include, with `DefineConstants`
    /// and `<LangVersion>` set — the two settings the linked-resolution APIs
    /// donate.
    fn fsproj_linking(include: &str, defines: &str, lang: &str) -> String {
        format!(
            r#"<Project>
              <PropertyGroup>
                <DefineConstants>{defines}</DefineConstants>
                <LangVersion>{lang}</LangVersion>
              </PropertyGroup>
              <ItemGroup>
                <Compile Include="{include}" />
              </ItemGroup>
            </Project>"#
        )
    }

    #[test]
    fn symbols_for_linked_uses_linking_project_on_ancestor_miss() {
        // `Shared/Foo.fs` has no `.fsproj` anywhere in its ancestor chain;
        // `ProjA` links it from a sibling directory. The plain resolution
        // degrades to the implicit set; the linked resolution donates ProjA's
        // defines and language version.
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("Shared").join("Foo.fs");
        let proj = tmp.path().join("ProjA").join("ProjA.fsproj");
        write_file(&file, "");
        write_file(&proj, &fsproj_linking("../Shared/Foo.fs", "FOO", "5.0"));

        let mut ws = Workspace::default();
        assert!(
            !ws.symbols_for(&file).contains("FOO"),
            "ancestor walk misses"
        );
        assert!(ws.symbols_for_linked(&file, &proj).contains("FOO"));
        assert_eq!(
            ws.lang_version_for_linked(&file, &proj),
            LanguageVersion::V5_0
        );
    }

    #[test]
    fn symbols_for_linked_defers_to_conclusive_ancestor_owner() {
        // `Shared/Shared.fsproj` conclusively owns `Foo.fs` (defines BAR);
        // `ProjA` also links it (defines FOO). The ancestor Member must win, so
        // the answer agrees with the plain resolution — and with
        // `textDocument/diagnostic`.
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("Shared").join("Foo.fs");
        let owner = tmp.path().join("Shared").join("Shared.fsproj");
        let linker = tmp.path().join("ProjA").join("ProjA.fsproj");
        write_file(&file, "");
        write_file(&owner, &fsproj_linking("Foo.fs", "BAR", "9.0"));
        write_file(&linker, &fsproj_linking("../Shared/Foo.fs", "FOO", "5.0"));

        let mut ws = Workspace::default();
        let linked = ws.symbols_for_linked(&file, &linker);
        assert!(
            linked.contains("BAR") && !linked.contains("FOO"),
            "{linked:?}"
        );
        assert_eq!(ws.symbols_for_linked(&file, &linker), ws.symbols_for(&file));
        assert_eq!(
            ws.lang_version_for_linked(&file, &linker),
            LanguageVersion::V9_0
        );
    }

    #[test]
    fn symbols_for_linked_ignores_non_member_linking_project() {
        // The claimed linking project conclusively does *not* list the file —
        // a stale or wrong hint. Fall back to the plain resolution rather than
        // donating unrelated defines.
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("Shared").join("Foo.fs");
        let proj = tmp.path().join("ProjA").join("ProjA.fsproj");
        write_file(&file, "");
        write_file(&proj, &fsproj_linking("Other.fs", "FOO", "5.0"));

        let mut ws = Workspace::default();
        assert_eq!(ws.symbols_for_linked(&file, &proj), ws.symbols_for(&file));
        assert_eq!(
            ws.lang_version_for_linked(&file, &proj),
            ws.lang_version_for(&file)
        );
    }

    #[test]
    fn symbols_for_linked_ignores_items_uncertain_linking_project() {
        // The linking project lists the file, but a `<Compile Remove>` behind
        // an unevaluable property-function condition means the list may retain
        // an item MSBuild would drop — Membership::Unknown, so it donates
        // nothing.
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("Shared").join("Foo.fs");
        let proj = tmp.path().join("ProjA").join("ProjA.fsproj");
        write_file(&file, "");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup>
                <DefineConstants>FOO</DefineConstants>
              </PropertyGroup>
              <ItemGroup>
                <Compile Include="../Shared/Foo.fs" />
                <Compile Remove="Nothing.fs" Condition="$([System.String]::IsNullOrEmpty(''))" />
              </ItemGroup>
            </Project>"#,
        );

        let mut ws = Workspace::default();
        assert!(
            ws.project(&proj).expect("evaluates").items_uncertain,
            "precondition: the Remove's condition must be unevaluable"
        );
        assert!(!ws.symbols_for_linked(&file, &proj).contains("FOO"));
    }

    proptest! {
        /// Whichever single project lists the file is chosen, regardless of
        /// where its filename sorts among the candidates.
        #[test]
        fn owning_project_picks_the_listing_project_regardless_of_order(
            count in 2usize..=4,
            member in 0usize..4,
        ) {
            let member = member % count;
            let tmp = TempDir::new().unwrap();
            for j in 0..count {
                let compile = if j == member { "Lib.fs" } else { "Other.fs" };
                fs::write(tmp.path().join(format!("P{j}.fsproj")), fsproj_with_compile(compile))
                    .unwrap();
            }
            let file = tmp.path().join("Lib.fs");
            fs::write(&file, "").unwrap();

            let mut ws = Workspace::default();
            prop_assert_eq!(
                ws.owning_project(&file),
                Some(tmp.path().join(format!("P{member}.fsproj")))
            );
        }
    }

    // ----- Stage 1.2: keep-climbing for higher linking projects -----

    #[test]
    fn owning_project_climbs_to_higher_project_that_lists_the_file() {
        // `Root.fsproj` links a nested file; the nearer `src/Other.fsproj`
        // does not list it. The ancestor walk must climb past `src/` to find
        // the project that actually compiles `Foo.fs`.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("Root.fsproj");
        let other = tmp.path().join("src").join("Other.fsproj");
        let file = tmp.path().join("src").join("sub").join("Foo.fs");
        write_file(&root, &fsproj_with_compile("src/sub/Foo.fs"));
        write_file(&other, &fsproj_with_compile("Bar.fs"));
        write_file(&file, "let x = 1");

        // The membership-blind heuristic stops at the nearer (wrong) project.
        assert_eq!(heuristic(&file), Some(other));

        let mut ws = Workspace::default();
        assert_eq!(ws.owning_project(&file), Some(root));
    }

    /// The walk lists every directory from the file's up to the first that
    /// holds a project listing it, each once, and stops there.
    #[test]
    fn the_ownership_walk_lists_each_directory_up_to_the_listing_project() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("Root.fsproj");
        let file = tmp.path().join("src").join("sub").join("Foo.fs");
        write_file(&root, &fsproj_with_compile("src/sub/Foo.fs"));
        write_file(&file, "let x = 1");

        let mut ws = Workspace::default();
        let walked = crate::inbox::never_superseded(ws.walk_for_owner(
            &file,
            0,
            &crate::inbox::Watch::unwatched(),
        ));
        assert_eq!(walked.project, Some(root.clone()));
        let listed: Vec<&Path> = walked
            .listings
            .iter()
            .map(|(dir, _)| dir.as_path())
            .collect();
        assert_eq!(
            listed,
            [
                tmp.path().join("src").join("sub").as_path(),
                tmp.path().join("src").as_path(),
                tmp.path(),
            ]
        );
        assert_eq!(
            walked.consulted,
            [(
                ProjectId::new(&root),
                ws.projects[&ProjectId::new(&root)].id
            )]
        );
    }

    /// A file no project lists climbs to the file system's root, then falls
    /// back to the nearest project, listing the directories up to it again:
    /// every listing of both passes is kept.
    #[test]
    fn the_ownership_walks_fallback_keeps_its_listings_too() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("Root.fsproj");
        let file = tmp.path().join("src").join("sub").join("New.fs");
        write_file(&root, &fsproj_with_compile("Other.fs"));
        write_file(&file, "let x = 1");

        let mut ws = Workspace::default();
        let walked = crate::inbox::never_superseded(ws.walk_for_owner(
            &file,
            0,
            &crate::inbox::Watch::unwatched(),
        ));
        assert_eq!(walked.project, Some(root));
        let climbed = file.parent().unwrap().ancestors().count();
        assert_eq!(walked.listings.len(), climbed + 3);
    }

    /// Inside generations the answer is remembered: the generation that
    /// walks lists each directory once (stamping it first), and a later one
    /// confirms the answer by one `stat` of each directory, however often it
    /// asks.
    #[test]
    fn an_owner_is_walked_once_and_confirmed_by_stamps() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("Root.fsproj");
        let file = tmp.path().join("sub").join("Foo.fs");
        write_file(&root, &fsproj_with_compile("sub/Foo.fs"));
        write_file(&file, "let x = 1");

        let mut ws = Workspace::default();
        let old = Nanos(i128::MAX / 2);
        let generation = |ws: &mut Workspace, asks: usize| {
            ws.begin_generation(old);
            for _ in 0..asks {
                assert_eq!(ws.owning_project(&file), Some(root.clone()));
            }
            ws.end_generation().ownership
        };
        let walked = generation(&mut ws, 1);
        assert_eq!((walked.listings, walked.stats), (2, 2), "{walked:?}");
        for asks in [1, 3] {
            let confirmed = generation(&mut ws, asks);
            assert_eq!(
                (confirmed.listings, confirmed.stats),
                (0, 2),
                "{confirmed:?}"
            );
        }
        // Outside a generation nothing is remembered or confirmed: every
        // ask walks.
        let projects_generation = ws.projects_generation();
        assert_eq!(ws.owning_project(&file), Some(root.clone()));
        assert_eq!(ws.projects_generation(), projects_generation);
    }

    /// A watch event drops an env at once only for a file whose stamp the
    /// env's resolution read (a DLL): a stamp is the one read a hint cannot
    /// ask again, since on a file system whose stamps do not move a `stat`
    /// answers as before. A file it read whole, or a directory it listed, is
    /// asked again by the hint instead, so naming one drops nothing.
    #[test]
    fn an_event_names_an_env_only_through_a_stamp_it_read() {
        let tmp = TempDir::new().unwrap();
        let dll = tmp.path().join("Lib.dll");
        let props = tmp.path().join("Lib.props");
        write_file(&dll, "not an assembly");
        write_file(&props, "<Project />");
        let observer = Observer::new();
        msbuild_observed::stamp(&observer, &dll).unwrap();
        msbuild_observed::read_to_string(&observer, &props).unwrap();
        msbuild_observed::read_dir(&observer, tmp.path()).unwrap();
        let (reads, _) = observer.finish();
        let ws = Workspace::default();
        let checks = ws.env_checks(&ProjectId::new(&props), None, &[], &reads, None);
        assert!(checks.named_by(&dll), "a DLL whose stamp the env read");
        assert!(!checks.named_by(&props), "a file the env read whole");
        assert!(
            !checks.named_by(&tmp.path().join("New.dll")),
            "an entry of a directory the env listed"
        );
    }

    /// A watch event names a path whose directory's stamp may not have moved
    /// (D1's "not covered"): the next generation asks again what the
    /// ownership walk and the evaluation read there, whatever the stamps say.
    /// A hint naming an entry of `sub/` lists `sub/` again and leaves the
    /// project's directory to its stamp; a hint naming the project file
    /// reads it again.
    #[test]
    fn a_hint_asks_again_what_was_read_at_the_paths_it_names() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("Root.fsproj");
        let file = tmp.path().join("sub").join("Foo.fs");
        write_file(&root, &fsproj_with_compile("sub/Foo.fs"));
        write_file(&file, "let x = 1");

        let mut ws = Workspace::default();
        let old = Nanos(i128::MAX / 2);
        let generation = |ws: &mut Workspace| {
            ws.begin_generation(old);
            assert_eq!(ws.owning_project(&file), Some(root.clone()));
            ws.end_generation()
        };
        generation(&mut ws);
        generation(&mut ws);
        let settled = generation(&mut ws);
        assert_eq!(settled.ownership.listings, 0, "{settled:?}");
        assert_eq!(settled.evaluations.validation.opens, 0, "{settled:?}");

        ws.hint([tmp.path().join("sub").join("New.fs").as_path()]);
        let hinted = generation(&mut ws);
        assert_eq!(hinted.ownership.listings, 1, "{hinted:?}");
        assert_eq!(hinted.evaluations.validation.opens, 0, "{hinted:?}");

        ws.hint([root.as_path()]);
        let hinted = generation(&mut ws);
        assert_eq!(hinted.ownership.listings, 1, "{hinted:?}");
        assert_eq!(hinted.evaluations.validation.opens, 1, "{hinted:?}");
        assert_eq!(generation(&mut ws).ownership.listings, 0);
    }

    /// The owners of every file in a directory share its listing: a
    /// generation asking for all of them lists each directory once, and
    /// the workspace keeps one listing per directory, not one per file.
    #[test]
    fn the_owners_of_a_directorys_files_share_its_listing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("Root.fsproj");
        let files: Vec<PathBuf> = (0..10)
            .map(|i| tmp.path().join("sub").join(format!("F{i}.fs")))
            .collect();
        let includes: String = (0..10)
            .map(|i| format!("<Compile Include=\"sub/F{i}.fs\" />"))
            .collect();
        write_file(
            &root,
            &format!("<Project><ItemGroup>{includes}</ItemGroup></Project>"),
        );
        for file in &files {
            write_file(file, "let x = 1");
        }

        let mut ws = Workspace::default();
        ws.begin_generation(Nanos(i128::MAX / 2));
        for file in &files {
            assert_eq!(ws.owning_project(file), Some(root.clone()));
        }
        let counts = ws.end_generation().ownership;
        assert_eq!(counts.listings, 2, "{counts:?}");
        assert_eq!(ws.owners.len(), 10);
        assert_eq!(ws.dir_listings.by_dir.len(), 2);
    }

    /// Outside a generation nothing confirms an answer, so none is served
    /// from memory: a project created between two asks is seen, whether or
    /// not a generation remembered the file's owner before.
    #[test]
    fn outside_a_generation_every_ask_walks() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("P.fsproj");
        let o = tmp.path().join("O.fsproj");
        let file = tmp.path().join("Foo.fs");
        write_file(&p, &fsproj_with_compile("Foo.fs"));
        write_file(&file, "let x = 1");

        let mut ws = Workspace::default();
        ws.begin_generation(Nanos(i128::MAX / 2));
        assert_eq!(ws.owning_project(&file), Some(p.clone()));
        ws.end_generation();
        assert_eq!(ws.owning_project(&file), Some(p.clone()));
        write_file(&o, &fsproj_with_compile("Foo.fs"));
        assert_eq!(ws.owning_project(&file), Some(o.clone()));
        fs::remove_file(&o).unwrap();
        assert_eq!(ws.owning_project(&file), Some(p));
    }

    #[test]
    fn owning_project_nearest_member_wins_over_farther_member() {
        // Both an ancestor `Root.fsproj` and the nearer `src/Inner.fsproj`
        // list the same file. Proximity breaks the tie: the nearer one wins.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("Root.fsproj");
        let inner = tmp.path().join("src").join("Inner.fsproj");
        let file = tmp.path().join("src").join("Foo.fs");
        write_file(&root, &fsproj_with_compile("src/Foo.fs"));
        write_file(&inner, &fsproj_with_compile("Foo.fs"));
        write_file(&file, "let x = 1");

        let mut ws = Workspace::default();
        assert_eq!(ws.owning_project(&file), Some(inner));
    }

    #[test]
    fn owning_project_does_not_climb_past_a_partial_nearer_project() {
        // The nearer project can't be fully evaluated (an unresolved
        // `<Import>` makes it partial), so the absence of `Foo.fs` from its
        // `items` is NOT proof it doesn't compile it — the import it couldn't
        // follow might contribute it. We must not let the complete higher
        // project that lists `Foo.fs` steal ownership; fall back to the
        // nearer project, exactly as the pre-refinement heuristic would.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("Root.fsproj");
        let near = tmp.path().join("src").join("Near.fsproj");
        let file = tmp.path().join("src").join("Foo.fs");
        // Complete, and explicitly lists the file.
        write_file(&root, &fsproj_with_compile("src/Foo.fs"));
        // Partial (unresolved import); lists a different file literally.
        write_file(
            &near,
            r#"<Project>
              <Import Project="Missing.props" />
              <ItemGroup>
                <Compile Include="Other.fs" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&file, "let x = 1");

        let mut ws = Workspace::default();
        assert_eq!(ws.owning_project(&file), Some(near));
    }

    #[test]
    fn owning_project_does_not_trust_a_partial_projects_listed_item() {
        // Presence in a partial project's `items` is not authoritative either:
        // the msbuild parser diagnoses-and-skips operations (e.g.
        // `<Compile Remove>`) and can't follow every import, so a *listed* file
        // may not actually be compiled. A farther partial project that lists
        // the file must therefore NOT steal ownership from the nearer complete
        // one; we fall back to the heuristic rather than trust the item.
        let tmp = TempDir::new().unwrap();
        let near = tmp.path().join("src").join("Near.fsproj");
        let root = tmp.path().join("Root.fsproj");
        let file = tmp.path().join("src").join("Foo.fs");
        // Complete; authoritatively does not list Foo.fs.
        write_file(&near, &fsproj_with_compile("Other.fs"));
        // Partial (unresolved import) yet lists Foo.fs — membership unknown.
        write_file(
            &root,
            r#"<Project>
              <Import Project="Missing.props" />
              <ItemGroup>
                <Compile Include="src/Foo.fs" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&file, "let x = 1");

        let mut ws = Workspace::default();
        assert_ne!(ws.owning_project(&file), Some(root));
        assert_eq!(ws.owning_project(&file), heuristic(&file));
    }

    // ----- Stage 3.1: `Workspace::project_graph` over real fsproj trees -----

    use crate::project_graph::{GraphProblem, ProjectKind};

    #[test]
    fn project_graph_walks_fsharp_chain() {
        // A → B → C, all F#: three nodes in DFS document order, no problems.
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(&b, &fsproj_with_refs(&["../C/C.fsproj"]));
        write_file(&c, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let graph = ws.project_graph(&a);

        assert!(graph.problems.is_empty(), "{:?}", graph.problems);
        assert_eq!(
            node_paths(&graph),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
            ],
        );
        assert!(graph.nodes.iter().all(|n| n.kind == ProjectKind::FSharp));
    }

    #[test]
    fn project_graph_uses_explicit_build_properties() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        write_file(
            &a,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../B/B.fsproj" Condition="'$(DISABLE_ARCADE)' == 'true'" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&b, &fsproj_with_refs(&[]));
        let extra = HashMap::from([("DISABLE_ARCADE".to_string(), "true".to_string())]);

        let ws = Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);
        let graph = ws.project_graph(&a);

        assert!(graph.problems.is_empty(), "{:?}", graph.problems);
        assert_eq!(
            node_paths(&graph),
            vec![lexically_normalize(&a), lexically_normalize(&b)]
        );
    }

    /// Producer-TFM seeding (3.3d): a multi-targeted dependency gating its
    /// `<ProjectReference>`s on `$(TargetFramework)` contributes exactly the
    /// edges of the TFM its consumer selected. Unseeded, `B` evaluates under
    /// its first-declared TFM (net10.0) and the walk follows `D`, missing
    /// `C`; seeded with the recovered producer TFM (net8.0), `C`
    /// materialises and `D` — an edge the real build would not take — must
    /// not be walked (folding it would be fabrication, not degradation).
    #[test]
    fn project_graph_seeds_producer_tfms_for_conditional_edges() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let d = tmp.path().join("D/D.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net10.0;net8.0</TargetFrameworks>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" Condition="'$(TargetFramework)' == 'net8.0'" />
                <ProjectReference Include="../D/D.fsproj" Condition="'$(TargetFramework)' == 'net10.0'" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));
        write_file(&d, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let unseeded = ws.project_graph(&a);
        assert_eq!(
            node_paths(&unseeded),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&d),
            ],
        );

        let tfms = BTreeMap::from([(
            crate::observed::host_fs::canonicalize(&b).unwrap(),
            "net8.0".to_string(),
        )]);
        let seeded = ws.project_graph_with_producer_tfms(
            &a,
            &tfms,
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&seeded),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
            ],
        );
    }

    /// A multi-targeted node the producer-TFM map does *not* cover (an
    /// unrestored edge, or a partial restore emptying the whole recovery)
    /// contributes only its **TFM-invariant** edges to the seeded walk: an
    /// edge present under every declared TFM is taken by the real build
    /// regardless of which TFM its consumer selects, while a
    /// `$(TargetFramework)`-gated edge evaluated under a *guessed* TFM could
    /// be one the build never takes — the env fold would turn it into folded
    /// DLLs (fabrication). Here B's edge to C is unconditional (kept) and its
    /// edge to D is gated on B's own first-declared TFM (dropped — even
    /// though the first-declared guess would have followed it). The
    /// diagnostics walk ([`Workspace::project_graph`]) keeps the historical
    /// first-declared behaviour and sees D.
    #[test]
    fn unseeded_multi_target_node_keeps_only_tfm_invariant_edges() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let d = tmp.path().join("D/D.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net10.0;net8.0</TargetFrameworks>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" />
                <ProjectReference Include="../D/D.fsproj" Condition="'$(TargetFramework)' == 'net10.0'" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));
        write_file(&d, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let seeded = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&seeded),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
            ],
        );

        let diagnostics_walk = ws.project_graph(&a);
        assert_eq!(
            node_paths(&diagnostics_walk),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
                lexically_normalize(&d),
            ],
        );
    }

    /// `<ProjectReference>` metadata the P2P protocol treats as significant
    /// but this walk does not model — `BuildReference="false"` (probed: the
    /// target vanishes from `ReferencePath` even prebuilt), `Targets`, the
    /// `Set*` evaluation mutators — makes the edge's contribution
    /// unknowable: the compile-closure walk must drop it (D5) while the
    /// declared-structure walk keeps reporting on the element.
    #[test]
    fn unmodelled_significant_reference_metadata_drops_the_compile_edge() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        write_file(
            &a,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../B/B.fsproj" BuildReference="false" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&b, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let compile = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile),
            vec![lexically_normalize(&a)],
            "the suppressed edge must not be walked for the compile closure"
        );
        let declared = ws.project_graph(&a);
        assert_eq!(
            node_paths(&declared),
            vec![lexically_normalize(&a), lexically_normalize(&b)],
            "the declared-structure walk keeps the element"
        );
    }

    /// A caller-supplied global `TargetFramework` that is EMPTY is the outer
    /// (dispatch) build, not a TFM choice: the SDK's inner-build gate is
    /// exactly `'$(TargetFramework)' == ''`. Treating it as caller-owned
    /// would classify a multi-targeted dependency as
    /// [`NodeTfm::NoneDeclared`] and take its first-declared gated edges on
    /// trust — the env fold could then locate a lone stale TFM variant
    /// instead of applying the unresolved-multi-TFM skip.
    #[test]
    fn empty_target_framework_global_does_not_own_the_tfm_choice() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let d = tmp.path().join("D/D.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net10.0;net8.0</TargetFrameworks>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" />
                <ProjectReference Include="../D/D.fsproj" Condition="'$(TargetFramework)' == 'net10.0'" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));
        write_file(&d, &fsproj_with_refs(&[]));

        let extra = HashMap::from([("TargetFramework".to_string(), String::new())]);
        let ws = Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);
        let seeded = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&seeded),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
            ],
            "only the TFM-invariant edge survives an unowned multi-TFM node"
        );
        let b_node = seeded
            .nodes
            .iter()
            .find(|n| n.path == lexically_normalize(&b))
            .expect("B is in the graph");
        assert_eq!(
            b_node.tfm,
            NodeTfm::Unresolved,
            "a multi-declaring node with no real TFM choice must not read as NoneDeclared"
        );
    }

    /// The walk carries each node's effective output-assembly name from the
    /// same evaluation as its TFM verdict: an `<AssemblyName>` override
    /// evaluates to the override, no override to the project-file stem, and
    /// an override whose provenance can't be pinned (written under an
    /// untrusted gate) to `None` — the env fold must decline that node's
    /// output rather than guess a name (a graph-only ref located by stem can
    /// fold a stale pre-rename DLL).
    #[test]
    fn graph_nodes_carry_the_evaluated_output_name() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let renamed = tmp.path().join("Renamed/Renamed.fsproj");
        let filed = tmp.path().join("Filed/Filed.fsproj");
        let plain = tmp.path().join("Plain/Plain.fsproj");
        let gated = tmp.path().join("Gated/Gated.fsproj");
        write_file(
            &a,
            &fsproj_with_refs(&[
                "../Renamed/Renamed.fsproj",
                "../Filed/Filed.fsproj",
                "../Plain/Plain.fsproj",
                "../Gated/Gated.fsproj",
            ]),
        );
        write_file(
            &renamed,
            r#"<Project>
              <PropertyGroup>
                <AssemblyName>Custom.Output</AssemblyName>
              </PropertyGroup>
            </Project>"#,
        );
        write_file(
            &filed,
            r#"<Project>
              <PropertyGroup>
                <AssemblyName>Identity</AssemblyName>
                <TargetName>FileName</TargetName>
              </PropertyGroup>
            </Project>"#,
        );
        write_file(&plain, &fsproj_with_refs(&[]));
        write_file(
            &gated,
            r#"<Project>
              <PropertyGroup Condition="'$(DefineConstants)' == ''">
                <AssemblyName>MaybeRenamed</AssemblyName>
              </PropertyGroup>
            </Project>"#,
        );

        let ws = Workspace::default();
        let graph = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        let name_of = |p: &Path| {
            graph
                .nodes
                .iter()
                .find(|n| n.path == lexically_normalize(p))
                .unwrap_or_else(|| panic!("{} is in the graph", p.display()))
                .output_name
                .clone()
        };
        assert_eq!(name_of(&renamed).as_deref(), Some("Custom.Output"));
        assert_eq!(
            name_of(&filed).as_deref(),
            Some("FileName"),
            "an explicit TargetName names the output file, not AssemblyName \
             (probed: Identity/FileName builds FileName.dll)"
        );
        assert_eq!(name_of(&plain).as_deref(), Some("Plain"));
        assert_eq!(
            name_of(&gated),
            None,
            "an AssemblyName written under an untrusted gate must decline"
        );
    }

    /// A project whose `<ProjectReference>` list the evaluator could not trust
    /// has that list **emptied** by the compile walk. The node must record
    /// that, because an emptied list is otherwise indistinguishable from a
    /// project that genuinely references nothing — and the env fold's
    /// conservation count would then agree with itself over a set those
    /// references had already been filtered out of, leaving the env claiming
    /// completeness while an unknown number of assemblies is missing.
    ///
    /// `Update` on a `<ProjectReference>` is the probed catalogue's simplest
    /// entry: unmodelled, so the captured list may claim references the real
    /// build strips.
    #[test]
    fn a_node_records_that_its_reference_list_was_suppressed() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" />
                <ProjectReference Update="../C/C.fsproj" Private="false" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let graph = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        let node = |p: &Path| {
            graph
                .nodes
                .iter()
                .find(|n| n.path == lexically_normalize(p))
                .unwrap_or_else(|| panic!("{} is in the graph", p.display()))
        };
        let b_node = node(&b);
        assert!(
            b_node.references_uncertain,
            "B's reference list was suppressed and the node must say so"
        );
        assert!(
            b_node.references.is_empty(),
            "the suppression is what the mark exists to record"
        );
        assert!(
            !node(&a).references_uncertain,
            "A's own list is trustworthy; the mark is per-node"
        );
    }

    /// The suppression signal is collected from **every** declared TFM, not
    /// just the ones reached before the invariant-edge intersection empties.
    /// Here the first two branches share no reference, so `surviving` is empty
    /// by the time the third — whose list the evaluator cannot trust — is
    /// reached; stopping there would report the node's references as
    /// trustworthy and drop the incompleteness the consumer needs.
    #[test]
    fn suppression_is_collected_from_every_declared_tfm() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        // Disjoint references under the first two TFMs empty the intersection;
        // the third adds an unmodelled `Update`, which suppresses the list.
        write_file(
            &b,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net10.0;net9.0;net8.0</TargetFrameworks>
              </PropertyGroup>
              <ItemGroup Condition="'$(TargetFramework)' == 'net10.0'">
                <ProjectReference Include="../Ten/Ten.fsproj" />
              </ItemGroup>
              <ItemGroup Condition="'$(TargetFramework)' == 'net9.0'">
                <ProjectReference Include="../Nine/Nine.fsproj" />
              </ItemGroup>
              <ItemGroup Condition="'$(TargetFramework)' == 'net8.0'">
                <ProjectReference Include="../Eight/Eight.fsproj" />
                <ProjectReference Update="../Eight/Eight.fsproj" Private="false" />
              </ItemGroup>
            </Project>"#,
        );

        let ws = Workspace::default();
        let graph = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        let b_node = graph
            .nodes
            .iter()
            .find(|n| n.path == lexically_normalize(&b))
            .expect("B is in the graph");
        assert!(
            b_node.references.is_empty(),
            "no reference survives every TFM, which is what empties the intersection"
        );
        assert!(
            b_node.references_uncertain,
            "the last TFM's list was suppressed and the walk must still have seen it"
        );
    }

    /// A body `TargetFramework` whose write sits behind an untrusted gate is
    /// NOT current truth — the real build may select a different TFM — so
    /// the walk must not report the node as [`NodeTfm::Known`] under it (the
    /// env fold would locate that TFM's output on trust and could fold a
    /// stale wrong-TFM DLL) and must not trust its evaluated output name
    /// (which may itself be TFM-gated). The node's *edges* are kept: any
    /// TFM-dependent edge reads the unpinned `TargetFramework` and already
    /// trips `project_references_uncertain`, so edges that survive are
    /// TFM-invariant.
    #[test]
    fn untrusted_body_target_framework_is_not_authoritative() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <PropertyGroup Condition="'$(DefineConstants)' == ''">
                <TargetFramework>net8.0</TargetFramework>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let graph = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&graph),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
            ],
            "the TFM-invariant edge to C survives"
        );
        let b_node = graph
            .nodes
            .iter()
            .find(|n| n.path == lexically_normalize(&b))
            .expect("B is in the graph");
        assert_eq!(
            b_node.tfm,
            NodeTfm::Unresolved,
            "an untrusted body TargetFramework must not read as Known"
        );
        assert_eq!(
            b_node.output_name, None,
            "the output name from an untrusted-TFM evaluation must decline"
        );
    }

    /// MSBuild global-property names compare OrdinalIgnoreCase, and the
    /// evaluator's input validation rejects case-insensitive duplicate keys.
    /// A caller-supplied **differently-cased** empty `targetframework`
    /// deliberately falls through to per-TFM evaluation (see
    /// [`empty_target_framework_global_does_not_own_the_tfm_choice`]) — but
    /// each per-TFM branch seeds `TargetFramework`, which must *replace* the
    /// caller's key, not join it: a duplicate fails every branch evaluation
    /// and even TFM-invariant edges (C here) vanish. Exercises both
    /// insertion sites: the unseeded intersection walk and the
    /// producer-seeded re-evaluation.
    #[test]
    fn cased_empty_target_framework_global_still_evaluates_tfm_branches() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let d = tmp.path().join("D/D.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net10.0;net8.0</TargetFrameworks>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" />
                <ProjectReference Include="../D/D.fsproj" Condition="'$(TargetFramework)' == 'net10.0'" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));
        write_file(&d, &fsproj_with_refs(&[]));

        let extra = HashMap::from([("targetframework".to_string(), String::new())]);
        let ws = Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);

        // Unseeded: the net8.0 branch of the intersection must evaluate, so
        // the TFM-invariant edge to C survives.
        let unseeded = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&unseeded),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
            ],
            "the TFM-invariant edge must survive per-TFM branch evaluation"
        );

        // Seeded: the producer-TFM re-evaluation must also replace the key.
        let tfms = BTreeMap::from([(
            crate::observed::host_fs::canonicalize(&b).unwrap(),
            "net8.0".to_string(),
        )]);
        let seeded = ws.project_graph_with_producer_tfms(
            &a,
            &tfms,
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&seeded),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
            ],
            "the seeded re-evaluation must not fail on a case-insensitive duplicate"
        );
    }

    /// An **outer-gated** plural — `<TargetFrameworks
    /// Condition="'$(TargetFramework)' == ''">` (the arcade-style idiom) — is
    /// only visible in the unseeded outer evaluation: the first-TFM inner
    /// re-evaluation makes the condition false and the plural vanishes. The
    /// multi-target detection must therefore read the *outer* pass's declared
    /// list, or an unseeded node would skip the TFM-invariant intersection
    /// and leak its first-declared branch's gated edge (D here).
    #[test]
    fn unseeded_outer_gated_plural_still_intersects_edges() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let d = tmp.path().join("D/D.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks Condition="'$(TargetFramework)' == ''">net10.0;net8.0</TargetFrameworks>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" />
                <ProjectReference Include="../D/D.fsproj" Condition="'$(TargetFramework)' == 'net10.0'" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));
        write_file(&d, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let seeded = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&seeded),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
            ],
        );
    }

    /// A seed is honoured only while the project still **declares** that TFM:
    /// a producer retargeted after its consumer's last restore leaves a stale
    /// TFM in the recovery map, and injecting it would evaluate
    /// `$(TargetFramework)`-gated edges under a TFM the real build can no
    /// longer select (here: `net9.0`, which B used to target — its gated edge
    /// to D must not resurrect). The invalid seed demotes the node to the
    /// TFM-invariant intersection instead.
    #[test]
    fn stale_producer_tfm_seed_is_not_injected() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let d = tmp.path().join("D/D.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net10.0;net8.0</TargetFrameworks>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" />
                <ProjectReference Include="../D/D.fsproj" Condition="'$(TargetFramework)' == 'net9.0'" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));
        write_file(&d, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let stale = BTreeMap::from([(
            crate::observed::host_fs::canonicalize(&b).unwrap(),
            "net9.0".to_string(),
        )]);
        let graph = ws.project_graph_with_producer_tfms(
            &a,
            &stale,
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&graph),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
            ],
        );
    }

    /// A `ReferenceOutputAssembly="false"` `<ProjectReference>` is a build
    /// dependency, not a compile reference: the compile-closure walk drops it
    /// and its subtree entirely (MSBuild probe: nothing lands in
    /// `ReferencePath`). An `ExcludeAssets=compile` reference from the
    /// **entry** keeps the target's own output — MSBuild's build adds the
    /// direct output itself; the exclusion only stops what would flow
    /// *through* the reference — so the target is a node but its subtree is
    /// not. The declared-structure walk keeps everything (existence and
    /// cycle semantics still hold for the build).
    #[test]
    fn compile_closure_walk_drops_non_compile_edges() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let d = tmp.path().join("D/D.fsproj");
        write_file(
            &a,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../B/B.fsproj" ReferenceOutputAssembly="false" />
                <ProjectReference Include="../C/C.fsproj">
                  <ExcludeAssets>compile</ExcludeAssets>
                </ProjectReference>
              </ItemGroup>
            </Project>"#,
        );
        write_file(&b, &fsproj_with_refs(&[]));
        write_file(&c, &fsproj_with_refs(&["../D/D.fsproj"]));
        write_file(&d, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![lexically_normalize(&a), lexically_normalize(&c)],
            "C's own output is referenced (direct output survives \
             ExcludeAssets=compile); B and C's subtree are not"
        );

        let declared = ws.project_graph(&a);
        assert_eq!(
            node_paths(&declared),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
                lexically_normalize(&d),
            ],
        );
    }

    /// `ExcludeAssets=compile` on a **non-entry** node's reference is fully
    /// opaque to the entry: not even the target's own output flows up
    /// (MSBuild probe: A→D normal + D→E excluded leaves A referencing only
    /// D — D compiles against E, A does not).
    #[test]
    fn compile_closure_drops_excluded_subtree_of_transitive_nodes() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" ExcludeAssets="compile" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![lexically_normalize(&a), lexically_normalize(&b)],
        );
    }

    /// Diamond (MSBuild probe): the entry excludes compile assets on its own
    /// edge to C but C is also reachable through a transparent path (A→X→C).
    /// Any transparent path wins — C's subtree (E) flows — and this must hold
    /// even though the excluded edge is declared *first*.
    #[test]
    fn compile_closure_excluded_target_reachable_transparently_keeps_subtree() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let e = tmp.path().join("E/E.fsproj");
        let x = tmp.path().join("X/X.fsproj");
        write_file(
            &a,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" ExcludeAssets="compile" />
                <ProjectReference Include="../X/X.fsproj" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&x, &fsproj_with_refs(&["../C/C.fsproj"]));
        write_file(&c, &fsproj_with_refs(&["../E/E.fsproj"]));
        write_file(&e, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&x),
                lexically_normalize(&c),
                lexically_normalize(&e),
            ],
        );
    }

    /// Diamond where **every** path to C excludes compile assets (the
    /// entry's own edge, and X's transitive edge): C still contributes its
    /// own output — the entry's direct edge keeps it (MSBuild probe) — but
    /// its subtree does not, and X's excluded edge contributes nothing at
    /// all (non-entry exclusion is opaque).
    #[test]
    fn compile_closure_all_excluded_paths_keep_direct_output_only() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let e = tmp.path().join("E/E.fsproj");
        let x = tmp.path().join("X/X.fsproj");
        write_file(
            &a,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" ExcludeAssets="compile" />
                <ProjectReference Include="../X/X.fsproj" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(
            &x,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" ExcludeAssets="compile" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&["../E/E.fsproj"]));
        write_file(&e, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&x),
                lexically_normalize(&c),
            ],
        );
    }

    /// An `ExcludeAssets=compile` edge from the entry to a **C#** target is
    /// dropped from the compile closure entirely: a C# boundary node's
    /// contract is "the sidecar expands its whole transitive subtree", which
    /// would fabricate exactly the references the exclusion removes. Dropping
    /// under-resolves only the target's own DLL (D5: under-resolve, never
    /// wrong).
    #[test]
    fn compile_closure_drops_excluded_csharp_edges_even_from_the_entry() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let c = tmp.path().join("C/C.csproj");
        write_file(
            &a,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../C/C.csproj" ExcludeAssets="compile" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, "<Project></Project>");

        let ws = Workspace::default();
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(node_paths(&compile_closure), vec![lexically_normalize(&a)]);
    }

    /// `PrivateAssets` covering `compile` on a **non-entry** node's reference
    /// stops the flow to the entry (MSBuild probe: D compiles against E but
    /// A never sees E) — following it would *fabricate* a reference. On the
    /// **entry's own** edge it is irrelevant: it governs what flows to the
    /// entry's consumers, and the entry has none in this walk.
    #[test]
    fn compile_closure_honours_private_assets_on_non_entry_edges_only() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let d = tmp.path().join("D/D.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" PrivateAssets="all" />
                <ProjectReference Include="../D/D.fsproj" PrivateAssets="compile" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));
        write_file(&d, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![lexically_normalize(&a), lexically_normalize(&b)],
            "a privately-referenced target must not flow to the entry"
        );

        // The same metadata on the entry's own edge changes nothing.
        write_file(
            &a,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../B/B.fsproj" PrivateAssets="all" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&b, &fsproj_with_refs(&["../C/C.fsproj"]));
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
            ],
        );
    }

    /// `IncludeAssets` without `compile` behaves exactly like
    /// `ExcludeAssets=compile` (MSBuild probes: `runtime` and even `none`
    /// still leave the direct output on `ReferencePath`): output-only from
    /// the entry, fully opaque from a non-entry node.
    #[test]
    fn compile_closure_include_assets_without_compile_stops_the_flow() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        write_file(
            &a,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../B/B.fsproj" IncludeAssets="runtime" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&b, &fsproj_with_refs(&["../C/C.fsproj"]));
        write_file(&c, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![lexically_normalize(&a), lexically_normalize(&b)],
            "entry edge: B's own output survives, nothing flows through"
        );

        // Non-entry: the same metadata makes the target invisible entirely.
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" IncludeAssets="none" />
              </ItemGroup>
            </Project>"#,
        );
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![lexically_normalize(&a), lexically_normalize(&b)],
        );

        // An IncludeAssets list that *does* cover compile flows normally.
        write_file(
            &b,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" IncludeAssets="compile;runtime" />
              </ItemGroup>
            </Project>"#,
        );
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![
                lexically_normalize(&a),
                lexically_normalize(&b),
                lexically_normalize(&c),
            ],
        );
    }

    /// An unmodelled `<ProjectReference Update/Remove>` leaves the captured
    /// list claiming references the real build may strip (probe: a `Remove`
    /// empties `ReferencePath`) — the compile-closure walk must refuse the
    /// whole node's edge set (under-resolve) rather than fold from it, while
    /// the declared-structure walk keeps reporting on the declared elements.
    #[test]
    fn compile_closure_distrusts_mutated_project_reference_lists() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" />
                <ProjectReference Remove="../C/*.fsproj" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![lexically_normalize(&a), lexically_normalize(&b)],
            "a mutated reference list must not contribute compile edges"
        );

        // The entry's own mutated list is refused the same way.
        write_file(
            &a,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../B/B.fsproj" />
                <ProjectReference Update="../B/B.fsproj" ReferenceOutputAssembly="false" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&b, &fsproj_with_refs(&[]));
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(node_paths(&compile_closure), vec![lexically_normalize(&a)]);

        let declared = ws.project_graph(&a);
        assert_eq!(
            node_paths(&declared),
            vec![lexically_normalize(&a), lexically_normalize(&b)],
            "the declared-structure walk keeps reporting on declared refs"
        );
    }

    /// Metadata we cannot evaluate (here: an `Exists(...)` condition, outside
    /// the evaluator's subset) may take effect in the real build. An unknown
    /// `ReferenceOutputAssembly` may make the reference build-order-only —
    /// nothing is safe to fold. Unknown asset filters can only stop what
    /// flows *through* the edge, so the entry's own edge keeps the target's
    /// direct output (OutputOnly) and a non-entry edge drops.
    #[test]
    fn compile_closure_reads_unknown_metadata_conservatively() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        write_file(
            &a,
            r#"<Project>
              <PropertyGroup>
                <ToolLock>abc</ToolLock>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../B/B.fsproj">
                  <ReferenceOutputAssembly Condition="$(ToolLock.FooBar('x'))">false</ReferenceOutputAssembly>
                </ProjectReference>
              </ItemGroup>
            </Project>"#,
        );
        write_file(&b, &fsproj_with_refs(&[]));
        write_file(&c, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![lexically_normalize(&a)],
            "unknown ReferenceOutputAssembly: not even the output is safe"
        );

        // Unknown ExcludeAssets on the entry's own edge: the direct output
        // is referenced regardless of how the filter resolves.
        write_file(
            &a,
            r#"<Project>
              <PropertyGroup>
                <ToolLock>abc</ToolLock>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../B/B.fsproj">
                  <ExcludeAssets Condition="$(ToolLock.FooBar('x'))">compile</ExcludeAssets>
                </ProjectReference>
              </ItemGroup>
            </Project>"#,
        );
        write_file(&b, &fsproj_with_refs(&["../C/C.fsproj"]));
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![lexically_normalize(&a), lexically_normalize(&b)],
            "unknown ExcludeAssets on the entry edge: output only, no subtree"
        );

        // The same unknown filter on a non-entry edge drops the target.
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(
            &b,
            r#"<Project>
              <PropertyGroup>
                <ToolLock>abc</ToolLock>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj">
                  <ExcludeAssets Condition="$(ToolLock.FooBar('x'))">compile</ExcludeAssets>
                </ProjectReference>
              </ItemGroup>
            </Project>"#,
        );
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![lexically_normalize(&a), lexically_normalize(&b)],
        );
    }

    /// The unseeded multi-targeted **entry** intersects its edges across its
    /// declared TFMs; an edge present under every TFM but compile-excluded
    /// under one of them contributes only what is TFM-invariant: the target's
    /// own output (every branch references it), never its subtree (the
    /// net8.0 branch doesn't flow it). Taking the first-declared branch's
    /// kind (Full) would leak C's subtree into the closure.
    #[test]
    fn unseeded_entry_merges_edge_kinds_across_tfm_branches() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let d = tmp.path().join("D/D.fsproj");
        write_file(
            &a,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net10.0;net8.0</TargetFrameworks>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj">
                  <ExcludeAssets Condition="'$(TargetFramework)' == 'net8.0'">compile</ExcludeAssets>
                </ProjectReference>
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&["../D/D.fsproj"]));
        write_file(&d, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let compile_closure = ws.project_graph_with_producer_tfms(
            &a,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            node_paths(&compile_closure),
            vec![lexically_normalize(&a), lexically_normalize(&c)],
        );
    }

    /// A `<ProjectReference>` item with the given `ReferenceOutputAssembly`
    /// resolution (`None` = metadata absent) and otherwise-default metadata.
    fn project_reference_item(reference_output_assembly: Option<&str>) -> ResolvedItem {
        ResolvedItem {
            kind: ItemKind::ProjectReference,
            include: PathBuf::from("/proj/B/B.fsproj"),
            reference_output_assembly: match reference_output_assembly {
                Some(v) => ItemMetadataValue::known(v),
                None => ItemMetadataValue::ABSENT,
            },
            exclude_assets: ItemMetadataValue::ABSENT,
            include_assets: ItemMetadataValue::ABSENT,
            private_assets: ItemMetadataValue::ABSENT,
            unmodelled_reference_metadata: false,
            span: 0..0,
        }
    }

    /// The P2P protocol admits a reference's output onto `ReferencePath`
    /// only under `'%(ReferenceOutputAssembly)'=='true'` (after an empty
    /// value is defaulted to `true`) — an MSBuild `==`, so the boolean
    /// vocabulary counts as true but `"0"`/`"1"`/padded spellings do not.
    /// Probed (dotnet 10.0.301, 2026-07-10, prebuilt target, entry edge):
    /// `on`/`yes`/`!false`/`TRUE` keep the DLL on `ReferencePath`;
    /// `0`/`1`/`off`/`no`/`" true "`/`" false "` remove it. Dropping only
    /// the literal `false` would fold a DLL the compiler never sees for
    /// every other non-true spelling.
    #[test]
    fn reference_output_assembly_uses_msbuild_boolean_comparison() {
        let flows = |value: Option<&str>| compile_edge_kind(&project_reference_item(value), true);
        for v in [
            None,
            Some("true"),
            Some("TRUE"),
            Some("on"),
            Some("yes"),
            Some("!false"),
        ] {
            assert_eq!(flows(v), Some(EdgeKind::Full), "{v:?} must compare true");
        }
        for v in [
            Some("false"),
            Some("no"),
            Some("off"),
            Some("0"),
            Some("1"),
            Some(" true "),
            Some(" false "),
        ] {
            assert_eq!(flows(v), None, "{v:?} must be build-order-only");
        }
    }

    #[test]
    fn project_graph_treats_csproj_as_terminal() {
        // A → {B.fsproj, C.csproj}. C is a C# boundary: recorded, not recursed.
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.csproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj", "../C/C.csproj"]));
        write_file(&b, &fsproj_with_refs(&[]));
        write_file(&c, "<Project></Project>");

        let ws = Workspace::default();
        let graph = ws.project_graph(&a);

        assert!(graph.problems.is_empty(), "{:?}", graph.problems);
        let csharp: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == ProjectKind::CSharp)
            .collect();
        assert_eq!(csharp.len(), 1);
        assert_eq!(csharp[0].path, lexically_normalize(&c));
        assert!(csharp[0].references.is_empty());
    }

    #[test]
    fn project_graph_reports_missing_csproj_reference() {
        // A → C.csproj where C.csproj does not exist. A missing C# reference is
        // reported (existence is checked) even though we never recurse into C#.
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        write_file(&a, &fsproj_with_refs(&["../C/C.csproj"]));
        // C.csproj intentionally not created.

        let ws = Workspace::default();
        let graph = ws.project_graph(&a);

        let gone = lexically_normalize(&tmp.path().join("C/C.csproj"));
        let entry = lexically_normalize(&a);
        assert!(
            matches!(
                graph.problems.as_slice(),
                [GraphProblem::NotFound { referrer, target, .. }]
                    if *referrer == entry && *target == gone
            ),
            "{:?}",
            graph.problems
        );
        // Only the entry is a node; the missing csproj is not.
        assert_eq!(node_paths(&graph), vec![entry]);
    }

    #[test]
    fn project_graph_reports_missing_reference() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        write_file(&a, &fsproj_with_refs(&["../Gone/Gone.fsproj"]));
        // Gone.fsproj intentionally not created.

        let ws = Workspace::default();
        let graph = ws.project_graph(&a);

        let gone = lexically_normalize(&tmp.path().join("Gone/Gone.fsproj"));
        let entry = lexically_normalize(&a);
        assert!(
            matches!(
                graph.problems.as_slice(),
                [GraphProblem::NotFound { referrer, target, .. }]
                    if *referrer == entry && *target == gone
            ),
            "{:?}",
            graph.problems
        );
        // Only the entry is a node; the missing target is not.
        assert_eq!(node_paths(&graph), vec![lexically_normalize(&a)]);
    }

    #[test]
    fn project_graph_reports_cycle() {
        // A → B → A.
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(&b, &fsproj_with_refs(&["../A/A.fsproj"]));

        let ws = Workspace::default();
        let graph = ws.project_graph(&a);

        let cycles = graph
            .problems
            .iter()
            .filter(|p| matches!(p, GraphProblem::Cycle { .. }))
            .count();
        assert_eq!(cycles, 1, "{:?}", graph.problems);
        assert_eq!(graph.nodes.len(), 2);
    }

    #[test]
    fn project_graph_dedups_diamond() {
        // A → {B, C}; B → D; C → D. D appears once.
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        let c = tmp.path().join("C/C.fsproj");
        let d = tmp.path().join("D/D.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj", "../C/C.fsproj"]));
        write_file(&b, &fsproj_with_refs(&["../D/D.fsproj"]));
        write_file(&c, &fsproj_with_refs(&["../D/D.fsproj"]));
        write_file(&d, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let graph = ws.project_graph(&a);

        assert!(graph.problems.is_empty(), "{:?}", graph.problems);
        let d_norm = lexically_normalize(&d);
        let d_count = graph.nodes.iter().filter(|n| n.path == d_norm).count();
        assert_eq!(d_count, 1);
        assert_eq!(graph.nodes.len(), 4);
    }

    #[test]
    fn project_graph_does_not_pin_the_project_cache() {
        // Building the graph must not populate `Workspace.projects`: the
        // `.fsproj` cycle diagnostics that consume it have no file-watch
        // guarantee, so a pinned evaluation could later be served stale to `.fs`
        // diagnostics (the graph-path analogue of
        // `server::tests::fsproj_sync_does_not_pin_the_project_cache`). We prove
        // it by editing a *referenced* project on disk after the walk and
        // observing the next `project` lookup sees the new content.
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A/A.fsproj");
        let b = tmp.path().join("B/B.fsproj");
        write_file(&a, &fsproj_with_refs(&["../B/B.fsproj"]));
        write_file(&b, &fsproj_with_define("FOO"));

        let mut ws = Workspace::default();
        // Walking the graph visits and evaluates B (A's reference).
        let graph = ws.project_graph(&a);
        assert_eq!(graph.nodes.len(), 2, "{:?}", graph.nodes);

        // B changes on disk (e.g. the user saves a new define), no watcher event.
        write_file(&b, &fsproj_with_define("BAR"));

        // The first cache lookup of B must evaluate fresh — proving the walk
        // left B uncached. (A cached graph walk would pin the stale FOO.)
        let defines = ws
            .project(&b)
            .map(|p| p.define_constants.clone())
            .unwrap_or_default();
        assert!(
            defines.iter().any(|d| d == "BAR") && !defines.iter().any(|d| d == "FOO"),
            "project_graph pinned B's evaluation: {defines:?}"
        );
    }

    // (The file-outside-project default is covered by
    // `symbols_for_compiled_file_includes_editing` and
    // `symbols_for_script_uses_interactive_not_compiled` above.)

    #[test]
    fn symbols_for_returns_define_constants_plus_implicit() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        let file = tmp.path().join("Lib.fs");
        write_file(&proj, &fsproj_with_defines("FOO;BAR"));
        write_file(&file, "");

        let mut ws = Workspace::new();
        let symbols = ws.symbols_for(&file);
        assert_eq!(
            symbols,
            HashSet::from([
                "COMPILED".to_string(),
                "EDITING".to_string(),
                "FOO".to_string(),
                "BAR".to_string(),
            ])
        );
    }

    #[test]
    fn explicit_build_properties_drive_project_evaluation() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        let file = tmp.path().join("Lib.fs");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup>
                <DefineConstants>BASE</DefineConstants>
                <DefineConstants Condition="'$(DISABLE_ARCADE)' == 'true'">$(DefineConstants);NO_ARCADE</DefineConstants>
                <DefineConstants Condition="'$(Configuration)' == 'Release'">$(DefineConstants);RELEASE_BUILD</DefineConstants>
                <DefineConstants Condition="'$(Platform)' == 'x64'">$(DefineConstants);X64_BUILD</DefineConstants>
              </PropertyGroup>
              <ItemGroup><Compile Include="Lib.fs" /></ItemGroup>
            </Project>"#,
        );
        write_file(&file, "");
        let extra = HashMap::from([
            ("DISABLE_ARCADE".to_string(), "true".to_string()),
            ("configuration".to_string(), "Release".to_string()),
            ("PLATFORM".to_string(), "x64".to_string()),
        ]);

        let mut ws =
            Workspace::with_env_and_extra_build_properties(SdkDiscoveryEnv::default(), extra);
        let symbols = ws.symbols_for(&file);

        assert!(symbols.contains("BASE"), "got {symbols:?}");
        assert!(
            symbols.contains("NO_ARCADE"),
            "DISABLE_ARCADE=true did not affect evaluation: {symbols:?}"
        );
        assert!(
            symbols.contains("RELEASE_BUILD"),
            "extra configuration must override the Debug default case-insensitively: {symbols:?}"
        );
        assert!(
            symbols.contains("X64_BUILD"),
            "extra PLATFORM must override the AnyCPU default case-insensitively: {symbols:?}"
        );
    }

    #[test]
    fn symbols_for_compiled_file_includes_editing() {
        // FCS's service parser defines COMPILED *and* EDITING for a compiled
        // `.fs`/`.fsi` — we analyse for editing (an LSP), so we must match.
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("Lib.fs");
        write_file(&file, "");

        let mut ws = Workspace::new();
        let symbols = ws.symbols_for(&file);
        assert_eq!(
            symbols,
            HashSet::from(["COMPILED".to_string(), "EDITING".to_string()]),
            "a compiled .fs must get COMPILED + EDITING, got {symbols:?}"
        );
    }

    #[test]
    fn symbols_for_script_uses_interactive_not_compiled() {
        // A `.fsx` script is parsed `isInteractive`, so FCS defines
        // INTERACTIVE + EDITING and *not* COMPILED.
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("Script.fsx");
        write_file(&file, "");

        let mut ws = Workspace::new();
        let symbols = ws.symbols_for(&file);
        assert_eq!(
            symbols,
            HashSet::from(["INTERACTIVE".to_string(), "EDITING".to_string()]),
            "a .fsx must get INTERACTIVE + EDITING (no COMPILED), got {symbols:?}"
        );
    }

    #[test]
    fn symbols_for_script_beside_project_stays_interactive() {
        // A `.fsx` under a project directory is not a `<Compile>` member (SDK
        // projects glob `.fs`), and FCS resolves scripts by extension anyway. It
        // must stay INTERACTIVE+EDITING — never the project's COMPILED or
        // DefineConstants.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        let file = tmp.path().join("Script.fsx");
        write_file(&proj, &fsproj_with_defines("FOO"));
        write_file(&file, "");

        let mut ws = Workspace::new();
        let symbols = ws.symbols_for(&file);
        assert_eq!(
            symbols,
            HashSet::from(["INTERACTIVE".to_string(), "EDITING".to_string()]),
            "a script beside a project must stay INTERACTIVE+EDITING, got {symbols:?}"
        );
    }

    #[test]
    fn symbols_for_script_listed_as_compile_item_is_compiled() {
        // The rare case: a `.fsx` *explicitly* in `<Compile>` is conclusively
        // compiled, so it gets the project's COMPILED set + DefineConstants,
        // not the script INTERACTIVE set.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        let file = tmp.path().join("Script.fsx");
        write_file(
            &proj,
            r#"<Project>
              <PropertyGroup><DefineConstants>FOO</DefineConstants></PropertyGroup>
              <ItemGroup><Compile Include="Script.fsx" /></ItemGroup>
            </Project>"#,
        );
        write_file(&file, "");

        let mut ws = Workspace::new();
        let symbols = ws.symbols_for(&file);
        assert_eq!(
            symbols,
            HashSet::from([
                "COMPILED".to_string(),
                "EDITING".to_string(),
                "FOO".to_string(),
            ]),
            "a <Compile>-listed .fsx must be treated as compiled, got {symbols:?}"
        );
    }

    #[test]
    fn symbols_for_script_suffix_is_case_insensitive() {
        // FCS matches script suffixes case-insensitively, so `.FSX` is still a
        // script and must get INTERACTIVE, not COMPILED.
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("Script.FSX");
        write_file(&file, "");

        let mut ws = Workspace::new();
        let symbols = ws.symbols_for(&file);
        assert_eq!(
            symbols,
            HashSet::from(["INTERACTIVE".to_string(), "EDITING".to_string()]),
            "a .FSX script must still get INTERACTIVE + EDITING, got {symbols:?}"
        );
    }

    #[test]
    fn symbols_for_caches_evaluation_result() {
        // Evaluate once, then mutate the fsproj on disk. A second
        // lookup must return the *cached* value, proving we didn't
        // re-read the file.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        let file = tmp.path().join("Lib.fs");
        write_file(&proj, &fsproj_with_defines("FOO"));
        write_file(&file, "");

        let mut ws = Workspace::new();
        let first = ws.symbols_for(&file);
        assert!(first.contains("FOO"));

        // Disk now says BAR, but the cache says FOO.
        fs::write(&proj, fsproj_with_defines("BAR")).unwrap();
        let second = ws.symbols_for(&file);
        assert!(
            second.contains("FOO") && !second.contains("BAR"),
            "expected cached FOO, got {second:?}"
        );
    }

    #[test]
    fn symbols_for_caches_evaluation_failure() {
        // A malformed `.fsproj` evaluates to `None`; the failure is
        // cached. A subsequent call returns just the implicit symbol set.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Broken.fsproj");
        let file = tmp.path().join("Lib.fs");
        write_file(&proj, "<Project><NotClosed>");
        write_file(&file, "");

        let implicit = HashSet::from(["COMPILED".to_string(), "EDITING".to_string()]);

        let mut ws = Workspace::new();
        let symbols = ws.symbols_for(&file);
        assert_eq!(symbols, implicit);

        // Even after fixing the fsproj, the cached `None` wins. (This
        // is the documented trade-off: no file-watch invalidation.)
        fs::write(&proj, fsproj_with_defines("FOO")).unwrap();
        let symbols_again = ws.symbols_for(&file);
        assert_eq!(symbols_again, implicit);
    }

    #[test]
    fn symbols_for_handles_whitespace_and_empty_segments() {
        // The msbuild evaluator already trims and drops empties — we
        // just check the LSP-side surfacing doesn't accidentally
        // introduce noise.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        let file = tmp.path().join("Lib.fs");
        write_file(&proj, &fsproj_with_defines("  FOO ;; BAR  "));
        write_file(&file, "");

        let mut ws = Workspace::new();
        let symbols = ws.symbols_for(&file);
        assert_eq!(
            symbols,
            HashSet::from([
                "COMPILED".to_string(),
                "EDITING".to_string(),
                "FOO".to_string(),
                "BAR".to_string(),
            ])
        );
    }

    proptest! {
        /// Idempotence: calling [`Workspace::symbols_for`] twice in a row
        /// for the same file returns the same set. This is the load-bearing
        /// invariant the diagnostics path relies on — a flickering symbol
        /// set would produce flickering diagnostics.
        #[test]
        fn symbols_for_is_idempotent(
            defines in r"[A-Z_][A-Z0-9_]{0,7}(;[A-Z_][A-Z0-9_]{0,7}){0,4}",
        ) {
            let tmp = TempDir::new().unwrap();
            let proj = tmp.path().join("P.fsproj");
            let file = tmp.path().join("Lib.fs");
            fs::write(&proj, fsproj_with_defines(&defines)).unwrap();
            fs::write(&file, "").unwrap();

            let mut ws = Workspace::new();
            let first = ws.symbols_for(&file);
            let second = ws.symbols_for(&file);
            prop_assert_eq!(first, second);
        }
    }

    // ----- Stage 1.0: `project_contains` membership predicate -----

    use borzoi_msbuild::ItemKind;

    /// A `Compile` item with the given resolved include path. The other fields
    /// don't affect membership.
    fn compile_item(include: PathBuf) -> ResolvedItem {
        ResolvedItem {
            kind: ItemKind::Compile,
            include,
            reference_output_assembly: ItemMetadataValue::ABSENT,
            exclude_assets: ItemMetadataValue::ABSENT,
            include_assets: ItemMetadataValue::ABSENT,
            private_assets: ItemMetadataValue::ABSENT,
            unmodelled_reference_metadata: false,
            span: 0..0,
        }
    }

    /// An absolute path under a fixed root, built from plain segments (never
    /// `.`/`..`, so its spelling is unambiguous before we add noise).
    fn abs_path(segments: &[String]) -> PathBuf {
        let mut p = PathBuf::from("/proj");
        for s in segments {
            p.push(s);
        }
        p
    }

    /// The same location as [`abs_path`] but with a redundant `.` and a
    /// cancelling `noise/..` pair woven in before each segment, so it
    /// normalises back to `abs_path(segments)`.
    fn noisy_spelling(segments: &[String]) -> PathBuf {
        let mut p = PathBuf::from("/proj");
        for s in segments {
            p.push(".");
            p.push("noise");
            p.push("..");
            p.push(s);
        }
        p
    }

    /// 1–3 plain path segments, each 1–4 lowercase letters.
    fn path_segments() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec("[a-z]{1,4}", 1..4)
    }

    proptest! {
        /// Self-membership: every include a project lists is owned by it.
        #[test]
        fn project_contains_each_of_its_includes(segs in path_segments()) {
            let path = abs_path(&segs);
            let items = vec![compile_item(path.clone())];
            prop_assert!(project_contains(&items, &path));
        }

        /// Spelling invariance: a path differing only by `.`/`..`/separator
        /// spelling from a listed include is still owned.
        #[test]
        fn project_contains_is_spelling_invariant(segs in path_segments()) {
            let items = vec![compile_item(abs_path(&segs))];
            prop_assert!(project_contains(&items, &noisy_spelling(&segs)));
        }

        /// Non-membership: a path that doesn't match any include (under the
        /// platform's path equality) is not owned.
        #[test]
        fn project_contains_rejects_non_members(
            listed in path_segments(),
            other in path_segments(),
        ) {
            prop_assume!(!paths_equal(&abs_path(&listed), &abs_path(&other)));
            let items = vec![compile_item(abs_path(&listed))];
            prop_assert!(!project_contains(&items, &abs_path(&other)));
        }
    }

    #[test]
    fn paths_equal_follows_platform_case_sensitivity() {
        let lower = Path::new("/proj/lib.fs");
        let upper = Path::new("/proj/Lib.fs");
        // Same spelling and genuinely-different files behave the same anywhere.
        assert!(paths_equal(lower, lower));
        assert!(!paths_equal(lower, Path::new("/proj/other.fs")));
        // Casing-only differences follow the default filesystem.
        if cfg!(any(windows, target_os = "macos")) {
            assert!(paths_equal(lower, upper));
        } else {
            assert!(!paths_equal(lower, upper));
        }
    }

    #[test]
    fn project_contains_matches_case_per_platform() {
        let items = vec![compile_item(PathBuf::from("/proj/Lib.fs"))];
        assert!(project_contains(&items, Path::new("/proj/Lib.fs")));
        assert_eq!(
            project_contains(&items, Path::new("/proj/lib.fs")),
            cfg!(any(windows, target_os = "macos")),
        );
    }

    /// Stage 8b.1 — SDK wiring tests.
    ///
    /// Stub a `$DOTNET_ROOT` layout in a tempdir so `SdkDiscovery` can
    /// resolve a fictional SDK name without touching the host's real
    /// install. Keeps the tests hermetic.
    /// The memoised owner against a fresh walk (`docs/incremental-lsp-plan.md`,
    /// Stage 4b): for all generated trees of projects and the changes made to
    /// them between generations, the answer a long-lived workspace gives
    /// inside a generation equals the one a fresh workspace's walk gives.
    #[cfg(unix)]
    mod ownership_memo {
        use super::*;
        use crate::vfs::{Clock, RACY_WINDOW};

        /// Directories a project can sit in, below the tree's root.
        const DIRS: [&str; 3] = ["", "a", "a/b"];
        /// Project file names; two in one directory tie, alphabetically.
        const NAMES: [&str; 2] = ["A.fsproj", "B.fsproj"];
        /// The files whose owners are asked for.
        const FILES: [&str; 4] = ["x.fs", "a/x.fs", "a/b/x.fs", "a/b/y.fs"];

        #[derive(Clone, Debug)]
        enum Content {
            /// A project compiling these of [`FILES`] (by index).
            Lists(Vec<usize>),
            /// Malformed XML: the evaluation fails, so membership is unknown.
            Broken,
        }

        #[derive(Clone, Debug)]
        enum Op {
            /// Write the project at `(dir, name)`, or delete it.
            Project(usize, usize, Option<Content>),
            /// Create or delete a source beside the projects, which moves a
            /// directory's listing and no owner.
            Source(usize, bool),
            /// Begin a generation and ask for the owners of these files.
            Ask(Vec<usize>),
        }

        fn content() -> impl Strategy<Value = Content> {
            prop_oneof![
                4 => prop::collection::vec(0..FILES.len(), 0..3).prop_map(Content::Lists),
                1 => Just(Content::Broken),
            ]
        }

        fn op() -> impl Strategy<Value = Op> {
            prop_oneof![
                4 => (0..DIRS.len(), 0..NAMES.len(), prop::option::weighted(0.7, content()))
                    .prop_map(|(d, n, c)| Op::Project(d, n, c)),
                1 => (0..DIRS.len(), any::<bool>()).prop_map(|(d, b)| Op::Source(d, b)),
                4 => prop::collection::vec(0..FILES.len(), 1..4).prop_map(Op::Ask),
            ]
        }

        /// The project's text, its includes spelt absolute under `root`, so
        /// a project anywhere can list a file anywhere in the tree.
        fn render(content: &Content, root: &Path) -> String {
            match content {
                Content::Lists(files) => {
                    let items: String = files
                        .iter()
                        .map(|&f| {
                            format!("<Compile Include=\"{}\" />", root.join(FILES[f]).display())
                        })
                        .collect();
                    format!("<Project><ItemGroup>{items}</ItemGroup></Project>")
                }
                Content::Broken => "<Project><ItemGroup>".to_string(),
            }
        }

        /// What a run saw, for the non-vacuity check.
        #[derive(Default, Debug)]
        struct Seen {
            /// Asks answered by confirming what an earlier generation
            /// remembered.
            confirmed: usize,
            /// Asks whose remembered answer no longer stood, and walked.
            rewalked: usize,
            /// Answers that differed from the one the same file got in the
            /// generation before.
            moved: usize,
        }

        fn run(ops: &[Op], seen: &mut Seen) -> Result<(), TestCaseError> {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path().join("t");
            for dir in DIRS {
                fs::create_dir_all(root.join(dir)).unwrap();
            }
            let mut ws = Workspace::default();
            let mut last: Vec<Option<Option<PathBuf>>> = vec![None; FILES.len()];
            for op in ops {
                match op {
                    Op::Project(d, n, content) => {
                        let path = root.join(DIRS[*d]).join(NAMES[*n]);
                        match content {
                            Some(content) => fs::write(&path, render(content, &root)).unwrap(),
                            None => {
                                let _ = fs::remove_file(&path);
                            }
                        }
                    }
                    Op::Source(d, create) => {
                        let path = root.join(DIRS[*d]).join("other.fs");
                        if *create {
                            fs::write(&path, "").unwrap();
                        } else {
                            let _ = fs::remove_file(&path);
                        }
                    }
                    Op::Ask(files) => {
                        ws.begin_generation(Clock::HostAhead(RACY_WINDOW * 30).now());
                        for &f in files {
                            let file = root.join(FILES[f]);
                            let before = ws.owners.get(&file).map(|o| o.listings.as_ptr());
                            let remembered = ws.owning_project(&file);
                            match (before, ws.owners.get(&file).map(|o| o.listings.as_ptr())) {
                                (Some(before), Some(after)) if before == after => {
                                    seen.confirmed += 1
                                }
                                (Some(_), _) => seen.rewalked += 1,
                                (None, _) => {}
                            }
                            let fresh = Workspace::default().owning_project(&file);
                            prop_assert_eq!(&remembered, &fresh, "the owner of {}", FILES[f]);
                            if last[f].as_ref().is_some_and(|before| *before != fresh) {
                                seen.moved += 1;
                            }
                            last[f] = Some(fresh);
                        }
                        ws.end_generation();
                        continue;
                    }
                }
                // Longer than a coarse file system's timestamp tick, so the
                // directory's stamp moves; the generations run ahead of the
                // files' clock, so no stamp is racy and the witnesses are
                // what confirms an answer.
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            Ok(())
        }

        fn ops() -> impl Strategy<Value = Vec<Op>> {
            prop::collection::vec(op(), 1..24)
        }

        proptest! {
            #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

            #[test]
            fn the_remembered_owner_is_a_fresh_walks(ops in ops()) {
                run(&ops, &mut Seen::default())?;
            }
        }

        /// The generator reaches what the property is about: answers confirmed
        /// from what an earlier generation remembered, remembered answers
        /// that no longer stood, and owners that moved.
        #[test]
        fn the_generator_reaches_remembered_and_moved_owners() {
            use proptest::strategy::ValueTree as _;
            let mut runner = proptest::test_runner::TestRunner::deterministic();
            let mut seen = Seen::default();
            for _ in 0..24 {
                let ops = ops().new_tree(&mut runner).unwrap().current();
                run(&ops, &mut seen).unwrap();
            }
            assert!(
                seen.confirmed > 20 && seen.rewalked > 20 && seen.moved > 10,
                "{seen:?}"
            );
        }
    }

    mod sdk {
        use super::*;

        /// Write
        /// `{dotnet_root}/sdk/{version}/Sdks/{sdk_name}/Sdk/Sdk.{props,targets}`.
        /// `sdk_props_body` is spliced into the `Sdk.props` file so callers
        /// can have the SDK contribute properties; `Sdk.targets` is always
        /// the empty `<Project/>` stub (we don't exercise the trailing
        /// splice in these tests).
        fn install_stub_sdk(
            dotnet_root: &Path,
            version: &str,
            sdk_name: &str,
            sdk_props_body: &str,
        ) {
            let sdk_root = dotnet_root
                .join("sdk")
                .join(version)
                .join("Sdks")
                .join(sdk_name)
                .join("Sdk");
            fs::create_dir_all(&sdk_root).unwrap();
            fs::write(sdk_root.join("Sdk.props"), sdk_props_body).unwrap();
            fs::write(sdk_root.join("Sdk.targets"), "<Project/>").unwrap();
        }

        /// Hermetic env: explicit `dotnet_root`, everything else `None`.
        /// In particular `search_path: None` blocks the `$PATH` probe
        /// from picking up a real `dotnet` if one is installed on the
        /// host running the tests.
        fn hermetic_env(dotnet_root: PathBuf) -> SdkDiscoveryEnv {
            SdkDiscoveryEnv {
                dotnet_root: Some(dotnet_root),
                ..SdkDiscoveryEnv::default()
            }
        }

        #[test]
        fn sdk_supplied_define_constants_surface_in_symbols() {
            // The Sdk.props contributes `FROM_SDK`. With the resolver
            // wired through, that symbol must appear in `symbols_for`.
            // Pre-8b.1 this test fails because `evaluate_project` calls
            // `parse_fsproj_with_imports` with `sdk_resolver=None`, so
            // the SDK splice never happens.
            let tmp = TempDir::new().unwrap();
            let dotnet = tmp.path().join("dotnet");
            install_stub_sdk(
                &dotnet,
                "8.0.401",
                "Test.Sdk",
                "<Project><PropertyGroup><DefineConstants>FROM_SDK</DefineConstants></PropertyGroup></Project>",
            );
            let project_dir = tmp.path().join("proj");
            let proj = project_dir.join("Sample.fsproj");
            let file = project_dir.join("Lib.fs");
            write_file(&proj, r#"<Project Sdk="Test.Sdk"></Project>"#);
            write_file(&file, "");

            let mut ws = Workspace::with_env(hermetic_env(dotnet));
            let symbols = ws.symbols_for(&file);
            assert!(
                symbols.contains("FROM_SDK"),
                "expected FROM_SDK in symbols, got {symbols:?}"
            );
            assert!(symbols.contains(COMPILED));
        }

        #[test]
        fn body_can_append_to_sdk_supplied_define_constants() {
            // The SDK contributes `FROM_SDK`; the project body appends
            // `BODY_ADDED` via the customary
            // `$(DefineConstants);BODY_ADDED` idiom. Both must end up
            // in the symbol set — proves the SDK splice happens *before*
            // body evaluation (otherwise `$(DefineConstants)` would
            // expand to empty and `FROM_SDK` would be dropped).
            let tmp = TempDir::new().unwrap();
            let dotnet = tmp.path().join("dotnet");
            install_stub_sdk(
                &dotnet,
                "8.0.401",
                "Test.Sdk",
                "<Project><PropertyGroup><DefineConstants>FROM_SDK</DefineConstants></PropertyGroup></Project>",
            );
            let project_dir = tmp.path().join("proj");
            let proj = project_dir.join("Sample.fsproj");
            let file = project_dir.join("Lib.fs");
            write_file(
                &proj,
                r#"<Project Sdk="Test.Sdk">
                  <PropertyGroup>
                    <DefineConstants>$(DefineConstants);BODY_ADDED</DefineConstants>
                  </PropertyGroup>
                </Project>"#,
            );
            write_file(&file, "");

            let mut ws = Workspace::with_env(hermetic_env(dotnet));
            let symbols = ws.symbols_for(&file);
            assert!(
                symbols.contains("FROM_SDK") && symbols.contains("BODY_ADDED"),
                "expected both SDK and body defines, got {symbols:?}"
            );
        }

        #[test]
        fn missing_dotnet_root_falls_back_to_body_only_defines() {
            // No DOTNET_ROOT and no PATH means `SdkDiscovery::for_project`
            // returns `MissingDotnetRoot`. We log and pass
            // `sdk_resolver=None`. The msbuild evaluator then emits
            // `SdkResolverAbsent` for the `Sdk="..."` attribute but
            // still walks the body — so `BODY_ONLY` survives, `FROM_SDK`
            // (which would have come from the SDK) does not.
            let tmp = TempDir::new().unwrap();
            let proj = tmp.path().join("Sample.fsproj");
            let file = tmp.path().join("Lib.fs");
            write_file(
                &proj,
                r#"<Project Sdk="Microsoft.NET.Sdk">
                  <PropertyGroup><DefineConstants>BODY_ONLY</DefineConstants></PropertyGroup>
                </Project>"#,
            );
            write_file(&file, "");

            let mut ws = Workspace::with_env(SdkDiscoveryEnv::default());
            let symbols = ws.symbols_for(&file);
            assert!(symbols.contains(COMPILED));
            assert!(
                symbols.contains("BODY_ONLY"),
                "body defines should survive a failed SDK lookup, got {symbols:?}"
            );
        }

        #[test]
        fn unresolved_sdk_name_falls_back_to_body_only_defines() {
            // DOTNET_ROOT is set but no SDK by that name is installed.
            // `SdkDiscovery::for_project` succeeds (it only sniffs the
            // root, doesn't probe SDK names); the resolver closure is
            // handed to the parser, which calls it, which returns
            // `SdkResolveError::NotFound`. The walker emits
            // `SdkNotFound` and skips the splice; the body still walks.
            let tmp = TempDir::new().unwrap();
            let dotnet = tmp.path().join("dotnet");
            fs::create_dir_all(dotnet.join("sdk")).unwrap();
            let proj = tmp.path().join("Sample.fsproj");
            let file = tmp.path().join("Lib.fs");
            write_file(
                &proj,
                r#"<Project Sdk="Definitely.Not.Installed.Sdk">
                  <PropertyGroup><DefineConstants>BODY_ONLY</DefineConstants></PropertyGroup>
                </Project>"#,
            );
            write_file(&file, "");

            let mut ws = Workspace::with_env(hermetic_env(dotnet));
            let symbols = ws.symbols_for(&file);
            assert!(symbols.contains("BODY_ONLY"));
        }

        #[test]
        fn configuration_debug_default_resolves_debug_conditioned_defines() {
            // The SDK's real `DEBUG` symbol is gated on
            // `'$(Configuration)' == 'Debug'`. If we evaluated with an
            // empty global bag, that arm would silently drop. The
            // hard-coded `Configuration=Debug` default seeds the
            // evaluator the same way an IDE-style `dotnet build`
            // would.
            let tmp = TempDir::new().unwrap();
            let proj = tmp.path().join("Sample.fsproj");
            let file = tmp.path().join("Lib.fs");
            write_file(
                &proj,
                r#"<Project>
                  <PropertyGroup Condition="'$(Configuration)' == 'Debug'">
                    <DefineConstants>DEBUG_BUILD</DefineConstants>
                  </PropertyGroup>
                </Project>"#,
            );
            write_file(&file, "");

            // Empty env is fine — no SDK resolution is needed; this
            // test is purely about the build-globals defaulting.
            let mut ws = Workspace::with_env(SdkDiscoveryEnv::default());
            let symbols = ws.symbols_for(&file);
            assert!(
                symbols.contains("DEBUG_BUILD"),
                "Debug-conditioned define should surface under the default Configuration, got {symbols:?}"
            );
        }

        #[test]
        fn release_conditioned_defines_do_not_surface_under_debug_default() {
            // Negative half of the previous test: a Release-only define
            // must NOT surface under the Debug default. Pins the
            // "we evaluate as Debug, not as some union of configs"
            // behaviour.
            let tmp = TempDir::new().unwrap();
            let proj = tmp.path().join("Sample.fsproj");
            let file = tmp.path().join("Lib.fs");
            write_file(
                &proj,
                r#"<Project>
                  <PropertyGroup Condition="'$(Configuration)' == 'Release'">
                    <DefineConstants>RELEASE_ONLY</DefineConstants>
                  </PropertyGroup>
                </Project>"#,
            );
            write_file(&file, "");

            let mut ws = Workspace::with_env(SdkDiscoveryEnv::default());
            let symbols = ws.symbols_for(&file);
            assert!(
                !symbols.contains("RELEASE_ONLY"),
                "Release-conditioned define should not surface under Debug, got {symbols:?}"
            );
        }

        #[test]
        fn sdkless_projects_unaffected_by_sdk_env() {
            // A bare `<Project>` never invokes the resolver. Whether
            // DOTNET_ROOT is set or not, the symbol set is just
            // `{COMPILED, EDITING} ∪ body defines`. Pins the no-regression
            // guarantee from Stage 8.
            let tmp = TempDir::new().unwrap();
            let dotnet = tmp.path().join("dotnet");
            install_stub_sdk(&dotnet, "8.0.401", "Test.Sdk", "<Project/>");
            let proj = tmp.path().join("Sample.fsproj");
            let file = tmp.path().join("Lib.fs");
            write_file(&proj, &fsproj_with_defines("FOO"));
            write_file(&file, "");

            let mut ws = Workspace::with_env(hermetic_env(dotnet));
            let symbols = ws.symbols_for(&file);
            assert_eq!(
                symbols,
                HashSet::from([
                    "COMPILED".to_string(),
                    "EDITING".to_string(),
                    "FOO".to_string()
                ])
            );
        }

        // ---- dotnet_root_for_project: single source of truth ----

        /// Regression: in a multi-root `global.json` `sdk.paths` workspace,
        /// the install root reported for a project must be the root the
        /// evaluator actually resolved the project's `<Project Sdk=...>`
        /// from — not a re-probe of a bare `Microsoft.NET.Sdk`, which can
        /// live under a *different* root. Here the project's `Custom.Sdk`
        /// is installed only under root B, while a bare `Microsoft.NET.Sdk`
        /// is installed only under root A, listed first. The old re-probe
        /// returned A, so the assembly env looked for framework `packs/`
        /// under the wrong install.
        #[test]
        fn dotnet_root_for_project_uses_the_sdk_the_project_resolved() {
            let tmp = TempDir::new().unwrap();
            let ws_dir = tmp.path().join("ws");
            let root_a = ws_dir.join("dotnet_a");
            let root_b = ws_dir.join("dotnet_b");
            // Bare Microsoft.NET.Sdk only under A; the project's Custom.Sdk
            // only under B. Disjoint names so each resolves to exactly one root.
            install_stub_sdk(&root_a, "9.0.100", "Microsoft.NET.Sdk", "<Project/>");
            install_stub_sdk(&root_b, "9.0.100", "Custom.Sdk", "<Project/>");
            // sdk.paths lists A before B, so a bare probe lands on A.
            write_file(
                &ws_dir.join("global.json"),
                r#"{ "sdk": { "paths": [ "dotnet_a", "dotnet_b" ] } }"#,
            );
            let proj = ws_dir.join("proj").join("App.fsproj");
            write_file(&proj, r#"<Project Sdk="Custom.Sdk"></Project>"#);
            write_file(&proj.parent().unwrap().join("Lib.fs"), "");

            // Empty env: roots come purely from sdk.paths (the host root is
            // optional when sdk.paths is present), so no host dotnet leaks in.
            let mut ws = Workspace::with_env(SdkDiscoveryEnv::default());
            let root = ws.dotnet_root_for_project(&proj);
            assert_eq!(
                root.as_deref(),
                Some(root_b.as_path()),
                "expected the install root Custom.Sdk resolved from (B), got {root:?}"
            );
        }

        /// The common case — a single install with a bare `Microsoft.NET.Sdk`
        /// — keeps reporting that install root. The recorded root *is* the one
        /// the evaluator resolved; here it's the only install present, so the
        /// single-source-of-truth path agrees with the old probe.
        #[test]
        fn dotnet_root_for_project_common_microsoft_sdk_reports_install_root() {
            let tmp = TempDir::new().unwrap();
            let dotnet = tmp.path().join("dotnet");
            install_stub_sdk(&dotnet, "9.0.100", "Microsoft.NET.Sdk", "<Project/>");
            let proj = tmp.path().join("proj").join("App.fsproj");
            write_file(&proj, r#"<Project Sdk="Microsoft.NET.Sdk"></Project>"#);
            write_file(&proj.parent().unwrap().join("Lib.fs"), "");

            let mut ws = Workspace::with_env(hermetic_env(dotnet.clone()));
            assert_eq!(
                ws.dotnet_root_for_project(&proj).as_deref(),
                Some(dotnet.as_path())
            );
        }

        /// A project with no `<Project Sdk=...>` records no SDK resolution, so
        /// the reported root comes from the best-effort probe fallback — here
        /// the single install's bare `Microsoft.NET.Sdk`. Pins that the
        /// fallback still answers when there is nothing authoritative to use.
        #[test]
        fn dotnet_root_for_project_without_sdk_falls_back_to_probe() {
            let tmp = TempDir::new().unwrap();
            let dotnet = tmp.path().join("dotnet");
            install_stub_sdk(&dotnet, "9.0.100", "Microsoft.NET.Sdk", "<Project/>");
            let proj = tmp.path().join("proj").join("App.fsproj");
            write_file(&proj, r#"<Project></Project>"#);
            write_file(&proj.parent().unwrap().join("Lib.fs"), "");

            let mut ws = Workspace::with_env(hermetic_env(dotnet.clone()));
            assert_eq!(
                ws.dotnet_root_for_project(&proj).as_deref(),
                Some(dotnet.as_path())
            );
        }
    }

    /// Stage 3b: a cached evaluation carries what it read, and is confirmed
    /// against the disk once per generation that reads it.
    mod validation {
        use super::*;
        use borzoi_msbuild::observed::soundness::{self, Edit, Perturbation, Why};

        /// A stub `Microsoft.NET.Sdk` of `version` under `dotnet`, whose
        /// `Sdk.props` defines `define`.
        fn install_sdk(dotnet: &Path, version: &str, define: &str) {
            let sdk = dotnet.join(format!("sdk/{version}/Sdks/Microsoft.NET.Sdk/Sdk"));
            fs::create_dir_all(&sdk).unwrap();
            fs::write(
                sdk.join("Sdk.props"),
                format!(
                    "<Project><PropertyGroup><DefineConstants>$(DefineConstants);{define}</DefineConstants></PropertyGroup></Project>"
                ),
            )
            .unwrap();
            fs::write(sdk.join("Sdk.targets"), "<Project />").unwrap();
        }

        /// A world with an SDK project that imports, globs, and sits under a
        /// `Directory.Build.props`; its `.fsproj`, and the SDK discovery
        /// environment. With `on_path`, the dotnet root is found by probing
        /// `$PATH` rather than named by `$DOTNET_ROOT`.
        fn world(root: &Path, on_path: bool) -> (PathBuf, SdkDiscoveryEnv) {
            let dotnet = root.join("dotnet");
            install_sdk(&dotnet, "10.0.100", "SDK_ONE");
            // The launcher `$PATH` discovery looks for.
            write_file(
                &dotnet.join(if cfg!(windows) {
                    "dotnet.exe"
                } else {
                    "dotnet"
                }),
                "",
            );
            write_file(
                &root.join("repo/Directory.Build.props"),
                "<Project><PropertyGroup><DefineConstants>$(DefineConstants);DBP</DefineConstants></PropertyGroup></Project>",
            );
            write_file(
                &root.join("repo/common.props"),
                "<Project><PropertyGroup><DefineConstants>$(DefineConstants);COMMON</DefineConstants></PropertyGroup></Project>",
            );
            let project = root.join("repo/App/App.fsproj");
            write_file(
                &project,
                r#"<Project Sdk="Microsoft.NET.Sdk">
  <Import Project="../common.props" Condition="Exists('../common.props')" />
  <ItemGroup><Compile Include="*.fs" /></ItemGroup>
</Project>"#,
            );
            write_file(&root.join("repo/App/A.fs"), "module A\n");
            let env = if on_path {
                SdkDiscoveryEnv {
                    search_path: Some(dotnet.clone().into_os_string()),
                    ..SdkDiscoveryEnv::default()
                }
            } else {
                SdkDiscoveryEnv {
                    dotnet_root: Some(dotnet),
                    ..SdkDiscoveryEnv::default()
                }
            };
            (project, env)
        }

        /// What a cached evaluation is: the served pass, with the whole read
        /// set behind it. A failed evaluation is the evaluation of an empty
        /// `<Project />`, which no perturbation of the world evaluates to,
        /// with the reads behind the failure.
        fn evaluation(project: &Path, env: &SdkDiscoveryEnv) -> ParsedProject {
            let evaluation = evaluate_project(project, env, &HashMap::new(), &Cell::new(0));
            let parsed = match evaluation.evaluated {
                Some(evaluated) => evaluated.parsed,
                None => borzoi_msbuild::parse_fsproj(
                    "<Project />",
                    project,
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap(),
            };
            ParsedProject {
                read_set: evaluation.reads,
                ..parsed
            }
        }

        /// The read-set soundness property of `docs/incremental-lsp-plan.md`
        /// (D7), through the LSP: for every perturbation of the world that
        /// leaves every read the workspace recorded standing (the `.fsproj`
        /// text and SDK discovery's reads included, beside each pass's own),
        /// evaluating again gives the same evaluation. Installing a newer SDK
        /// moves it, so the sweep is not vacuous.
        #[test]
        fn the_workspace_read_set_is_sound() {
            for on_path in [false, true] {
                let tmp = TempDir::new().unwrap();
                let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
                let (project, env) = world(&root, on_path);
                let newer = root.join("dotnet/sdk/10.0.200/Sdks/Microsoft.NET.Sdk/Sdk");
                let extra = [
                    Perturbation {
                        why: Why::Extra,
                        edits: vec![
                            Edit::Write(
                                newer.join("Sdk.props"),
                                b"<Project><PropertyGroup><DefineConstants>NEWER</DefineConstants></PropertyGroup></Project>".to_vec(),
                            ),
                            Edit::Write(newer.join("Sdk.targets"), b"<Project />".to_vec()),
                        ],
                    },
                    Perturbation {
                        why: Why::Extra,
                        edits: vec![Edit::Write(
                            root.join("repo/global.json"),
                            br#"{ "sdk": { "version": "9.0.100", "rollForward": "disable" } }"#
                                .to_vec(),
                        )],
                    },
                ];
                let tally = soundness::sweep(&root, &|| evaluation(&project, &env), &extra);
                assert_eq!(
                    tally.moved(Why::Extra),
                    2,
                    "an SDK install and a global.json pin each move the evaluation \
                     (on_path = {on_path}): {tally:#?}"
                );
            }
        }

        /// The soundness property for what a cached evaluation confirms, its
        /// checks ([`Evaluation::checks_and_exempt`]), rather than all it read: over
        /// every perturbation of a project tree outside the installs (so
        /// the immutable-content axiom holds), an evaluation whose checks
        /// all stand is the evaluation a fresh one would be. The tree holds
        /// the SDK project of [`world`], and a multi-targeted one whose
        /// seeded pass imports a file that opts out of the seed (so that
        /// pass is discarded) and probes for an import that is not there.
        #[test]
        fn the_workspace_checks_are_sound_outside_the_installs() {
            let tmp = TempDir::new().unwrap();
            let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
            let (sdk_project, env) = world(&root, false);
            let multi = root.join("repo/Multi/Multi.fsproj");
            write_file(
                &multi,
                r#"<Project>
  <PropertyGroup><TargetFrameworks>net8.0;net9.0</TargetFrameworks></PropertyGroup>
  <Import Project="tfm.props" Condition="'$(TargetFramework)' == 'net8.0'" />
  <Import Project="maybe.props" Condition="Exists('maybe.props')" />
  <ItemGroup><Compile Include="M.fs" /></ItemGroup>
</Project>"#,
            );
            write_file(
                &root.join("repo/Multi/tfm.props"),
                r#"<Project TreatAsLocalProperty="TargetFramework" />"#,
            );
            write_file(&root.join("repo/Multi/M.fs"), "module M\n");
            for project in [sdk_project, multi] {
                let checked = || {
                    let evaluation =
                        evaluate_project(&project, &env, &HashMap::new(), &Cell::new(0));
                    let read_set: ReadSet = evaluation
                        .checks(&ReadChecks::default(), |_| false)
                        .into_iter()
                        .map(|check| check.read().clone())
                        .collect();
                    ParsedProject {
                        read_set,
                        ..evaluation_of(&project, evaluation.evaluated)
                    }
                };
                let tally = soundness::sweep(&root.join("repo"), &checked, &[]);
                assert!(
                    tally.moved(Why::Rewrite) > 0 && tally.moved(Why::CreateAbsent) > 0,
                    "{}: {tally:#?}",
                    project.display()
                );
            }
        }

        /// A version's canonical spelling is an alias for it only when the
        /// version directory itself was canonicalised: a symlink below a
        /// version leads somewhere that is not that version.
        #[test]
        fn only_a_version_directory_canonicalised_gives_an_alias() {
            let roots = ImmutableRoots {
                dotnet: vec![PathBuf::from("/d")],
                nuget: vec![],
            };
            let canonical = |path: &str, to: &str| Read {
                path: PathBuf::from(path),
                observation: Observation::Canonical(Ok(PathBuf::from(to))),
            };
            let below = [canonical("/d/sdk/1.0.0/Sdks/X/Sdk", "/repo/Sdks/X/Sdk")];
            let versions = roots.versions(below.iter());
            assert_eq!(versions.containing(Path::new("/repo/App.fsproj")), None);
            let itself = [canonical("/d/sdk/1.0.0", "/real/sdk/1.0.0")];
            let versions = roots.versions(itself.iter());
            assert_eq!(
                versions
                    .containing(Path::new("/real/sdk/1.0.0/x.props"))
                    .map(std::borrow::Cow::into_owned),
                Some(PathBuf::from("/real/sdk/1.0.0"))
            );
        }

        /// A package version's contents are trusted only once its
        /// extraction was committed (`.nupkg.metadata` written) before the
        /// evaluation began: an evaluation racing an extraction confirms
        /// what it read inside it, so finishing the extraction is noticed
        /// (Codex).
        #[test]
        fn a_package_is_trusted_only_once_committed_before_the_evaluation() {
            let tmp = TempDir::new().unwrap();
            let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
            let nuget = root.join("nuget");
            let version = nuget.join("pkg/1.0.0");
            write_file(&version.join("Sdk/Sdk.props"), "<Project />");
            let observer = Observer::new();
            let _ = msbuild_observed::read_to_string(&observer, &version.join("Sdk/Sdk.props"));
            let _ = msbuild_observed::kind(&observer, &version.join("Sdk/Sdk.targets"));
            let evaluation = Evaluation {
                evaluated: None,
                reads: observer.finish().0,
                immutable: ImmutableRoots {
                    dotnet: vec![],
                    nuget: vec![nuget.clone()],
                },
            };
            let inside = |checks: &[Check]| {
                checks
                    .iter()
                    .filter(|check| check.read().path.starts_with(&version))
                    .map(|check| check.read().path.clone())
                    .collect::<Vec<_>>()
            };
            let mut checker = ReadChecks::default();
            let mut checks_at = |at: Nanos| {
                checker.begin_generation(at);
                let started = checker.now();
                let checks =
                    evaluation.checks(&checker, |v| nuget_committed_before(&checker, v, started));
                checker.end_generation();
                inside(&checks)
            };
            // Mid-extraction: no marker, so both reads are confirmed.
            assert_eq!(checks_at(Nanos(i128::MAX / 2)).len(), 2);
            write_file(&version.join(".nupkg.metadata"), "{}");
            // Committed just now: the evaluation may have raced it.
            let now = Nanos::of(std::time::SystemTime::now());
            assert_eq!(checks_at(now).len(), 2);
            // Committed long before: the reads inside are trusted, and the
            // version's existence is what is confirmed. A host whose stamps
            // carry no change time can never show the marker settled
            // (`crate::vfs::Stamp::is_racy`), so there the reads stay.
            let expected = if cfg!(unix) {
                vec![version.clone()]
            } else {
                inside(&evaluation.checks(&ReadChecks::default(), |_| false))
            };
            assert_eq!(checks_at(Nanos(i128::MAX / 2)), expected);
        }

        /// Inside a version shown to exist, an answer or an absence is
        /// trusted, but a read that failed otherwise is confirmed: the
        /// failure can clear (Codex).
        #[test]
        fn a_failed_read_inside_a_version_is_confirmed() {
            let failure = |kind: std::io::ErrorKind| {
                msbuild_observed::Failure::of(&std::io::Error::from(kind))
            };
            let read = |path: &str, observation: Observation| Read {
                path: PathBuf::from(path),
                observation,
            };
            let reads = [
                read(
                    "/d/sdk/1.0.0/Sdk.props",
                    Observation::Text(Err(failure(std::io::ErrorKind::PermissionDenied))),
                ),
                read(
                    "/d/sdk/1.0.0/Sdk.targets",
                    Observation::Text(Ok(msbuild_observed::TextDigest::of("<Project />"))),
                ),
                read(
                    "/d/sdk/1.0.0/absent.props",
                    Observation::Kind(Err(failure(std::io::ErrorKind::NotFound))),
                ),
                read(
                    "/d/sdk/1.0.0/Sdks",
                    Observation::Listing(Ok(vec![msbuild_observed::Listed::Unreadable(
                        msbuild_observed::Failure::of(&std::io::Error::other("transient")),
                    )])),
                ),
            ];
            let evaluation = Evaluation {
                evaluated: None,
                reads: reads.iter().cloned().collect(),
                immutable: ImmutableRoots {
                    dotnet: vec![PathBuf::from("/d")],
                    nuget: vec![],
                },
            };
            let mut checked: Vec<PathBuf> = evaluation
                .checks(&ReadChecks::default(), |_| true)
                .iter()
                .map(|check| check.read().path.clone())
                .collect();
            checked.sort();
            assert_eq!(
                checked,
                [
                    "/d/sdk/1.0.0",
                    "/d/sdk/1.0.0/Sdk.props",
                    "/d/sdk/1.0.0/Sdks"
                ]
                .map(PathBuf::from)
            );
        }

        /// The served pass of `evaluated`, or for a failed evaluation the
        /// evaluation of an empty `<Project />`.
        fn evaluation_of(project: &Path, evaluated: Option<EvaluatedProject>) -> ParsedProject {
            match evaluated {
                Some(evaluated) => evaluated.parsed,
                None => borzoi_msbuild::parse_fsproj(
                    "<Project />",
                    project,
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap(),
            }
        }

        /// Which reads are exempt from validation as immutable contents.
        #[test]
        fn immutable_contents_lie_strictly_inside_a_version() {
            let roots = ImmutableRoots {
                dotnet: vec![PathBuf::from("/d")],
                nuget: vec![PathBuf::from("/n")],
            };
            let cases = [
                ("/d/sdk/10.0.100/Sdks/X/Sdk/Sdk.props", true),
                ("/d/sdk/10.0.100/absent.props", true),
                ("/d/packs/Microsoft.NETCore.App.Ref/10.0.0/data/x", true),
                (
                    "/d/sdk-manifests/10.0.100/microsoft.net.sdk.ios/26.0.1/WorkloadManifest.json",
                    true,
                ),
                (
                    "/n/fsharp.core/9.0.100/lib/netstandard2.1/FSharp.Core.dll",
                    true,
                ),
                // The directories holding the versions, and the versions
                // themselves: an install or an uninstall moves them.
                ("/d/sdk", false),
                ("/d/sdk/10.0.100", false),
                ("/d/packs/Microsoft.NETCore.App.Ref", false),
                ("/d/packs/Microsoft.NETCore.App.Ref/10.0.0", false),
                ("/n/fsharp.core", false),
                ("/n/fsharp.core/9.0.100", false),
                // A workload update writes into a band and a manifest.
                ("/d/sdk-manifests/10.0.100/microsoft.net.sdk.ios", false),
                (
                    "/d/sdk-manifests/10.0.100/microsoft.net.sdk.ios/WorkloadManifest.json",
                    false,
                ),
                // Not a version.
                ("/d/sdk/preview/x.props", false),
                ("/d/packs/X/latest/x", false),
                ("/d/metadata/workloads/10.0.100/userlocal", false),
                // A `..` is taken lexically: one that stays inside the
                // version, and ones that leave it.
                ("/d/sdk/10.0.100/Sdks/X/Sdk/../../Y/Sdk/Sdk.props", true),
                ("/d/sdk/10.0.100/Sdks/X/Sdk/../../../..", false),
                ("/d/sdk/10.0.100/../../elsewhere/x", false),
                // Not under a root.
                ("/elsewhere/sdk/10.0.100/x", false),
                // The canonical spelling of a version a read resolved.
                ("/real/sdk/10.0.100/Sdks/X/Sdk/Sdk.props", true),
                ("/real/sdk/10.0.100", false),
                ("/real/sdk/10.0.200/x", false),
            ];
            // A canonicalisation that resolved a version to another tree.
            let read = Read {
                path: PathBuf::from("/d/sdk/10.0.100"),
                observation: Observation::Canonical(Ok(PathBuf::from("/real/sdk/10.0.100"))),
            };
            let versions = roots.versions(std::slice::from_ref(&read));
            for (path, expected) in cases {
                assert_eq!(
                    versions.containing(Path::new(path)).is_some(),
                    expected,
                    "{path}"
                );
            }
        }

        /// The workspace serves a cached evaluation as it is outside a
        /// generation, confirms it once per generation inside one, and
        /// evaluates again, ending the projects generation, once a project
        /// file it read has moved.
        #[test]
        fn a_moved_project_file_is_noticed_in_the_next_generation() {
            let tmp = TempDir::new().unwrap();
            let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
            let (_, env) = world(&root, false);
            let file = root.join("repo/App/A.fs");
            let mut ws = Workspace::with_env(env);
            let at = |ws: &mut Workspace| {
                // Old enough that no stamp is racy, so an unchanged file is
                // confirmed by `stat` alone.
                ws.begin_generation(Nanos(i128::MAX / 2));
            };
            at(&mut ws);
            assert!(ws.symbols_for(&file).contains("COMMON"));
            let first = ws.end_generation().evaluations;
            assert!(first.evaluated > 0);
            let generation = ws.projects_generation();

            at(&mut ws);
            assert!(ws.symbols_for(&file).contains("COMMON"));
            assert!(ws.symbols_for(&file).contains("COMMON"));
            let confirmed = ws.end_generation().evaluations;
            assert_eq!(confirmed.evaluated, 0, "{confirmed:?}");
            assert!(confirmed.validation.stats > 0, "{confirmed:?}");
            assert_eq!(ws.projects_generation(), generation);

            fs::remove_file(root.join("repo/common.props")).unwrap();
            // Outside a generation the cache is served as it is.
            assert!(ws.symbols_for(&file).contains("COMMON"));
            at(&mut ws);
            assert!(!ws.symbols_for(&file).contains("COMMON"));
            let moved = ws.end_generation().evaluations;
            assert!(moved.evaluated > 0, "{moved:?}");
            assert_ne!(ws.projects_generation(), generation);
        }

        /// [`ImmutableRoots::version_of`] as it was first written: `path`
        /// spelt lexically normal, its names below each root collected.
        fn version_of_reference(roots: &ImmutableRoots, path: &Path) -> Option<(PathBuf, usize)> {
            let path = lexically_normalize(path);
            let version = |name: &std::ffi::OsStr| {
                name.as_encoded_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_digit)
            };
            let below = |root: &Path| -> Option<Vec<&std::ffi::OsStr>> {
                path.strip_prefix(root)
                    .ok()?
                    .components()
                    .map(|component| match component {
                        Component::Normal(name) => Some(name),
                        _ => None,
                    })
                    .collect()
            };
            roots
                .dotnet
                .iter()
                .find_map(|root| {
                    let names = below(root)?;
                    let depth = match names.as_slice() {
                        [sdk, v, ..] if *sdk == "sdk" && version(v) => 2,
                        [packs, _, v, ..] if *packs == "packs" && version(v) => 3,
                        [m, _, _, v, ..] if *m == "sdk-manifests" && version(v) => 4,
                        _ => return None,
                    };
                    let dir = names[..depth]
                        .iter()
                        .fold(root.clone(), |dir, name| dir.join(name));
                    Some((dir, names.len() - depth))
                })
                .or_else(|| {
                    roots
                        .nuget
                        .iter()
                        .find_map(|root| match below(root)?.as_slice() {
                            [id, v, rest @ ..] if version(v) => {
                                Some((root.join(id).join(v), rest.len()))
                            }
                            _ => None,
                        })
                })
        }

        /// [`ImmutableRoots::versions`]' aliases as they were first found.
        fn aliases_reference(roots: &ImmutableRoots, reads: &[Read]) -> Vec<PathBuf> {
            reads
                .iter()
                .filter_map(|read| {
                    let Observation::Canonical(Ok(canonical)) = &read.observation else {
                        return None;
                    };
                    let (dir, below) = version_of_reference(roots, &read.path)?;
                    let installed = roots.dotnet.iter().any(|root| dir.starts_with(root));
                    (below == 0 && installed && *canonical != dir).then(|| canonical.clone())
                })
                .collect()
        }

        /// [`VersionDirs::containing`] as it was first written, among
        /// `roots` and the reference's `aliases`. The references
        /// [`a_version_is_found_as_the_reference_finds_it`] and
        /// [`reads_are_partitioned_as_the_reference_partitions_them`] hold
        /// the allocation-free ones to.
        fn containing_reference(
            roots: &ImmutableRoots,
            aliases: &[PathBuf],
            path: &Path,
        ) -> Option<PathBuf> {
            if let Some((version, below)) = version_of_reference(roots, path)
                && below > 0
            {
                return Some(version);
            }
            let path = lexically_normalize(path);
            aliases
                .iter()
                .find(|alias| path.starts_with(alias) && path != **alias)
                .cloned()
        }

        proptest! {
            #![proptest_config(ProptestConfig { cases: 2048, ..ProptestConfig::default() })]

            /// The version directory a read lies in, found without spelling
            /// the path again or collecting its names (by its bytes, where
            /// it and every root are spelt as their components), is the one
            /// the reference finds, for every spelling: `.` and `..`,
            /// doubled and trailing separators, a version alias, names that
            /// are almost a version layout, and a root spelt otherwise.
            #[test]
            fn a_version_is_found_as_the_reference_finds_it(
                prefix in prop::sample::select(vec![
                    "", "/d", "/n/d", "/n", "/real", "/d/sdk", "/d/packs/p", "/n/d/packs/p",
                    "/d/sdk-manifests/b/m", "/n/pkg", "/n/d/sdk", "/real/sdk", "/d/sdk/1.0",
                    // Spellings that start with a root's bytes and are not under it.
                    "/dd/sdk", "/n/dx/sdk", "/nn/pkg", "/real/sdk/1.00",
                ]),
                names in prop::collection::vec(
                    prop::sample::select(vec![
                        "d", "n", "sdk", "packs", "sdk-manifests", "1.0", "2", "x", "pkg", ".",
                        "..", "", "real",
                    ]),
                    0..8,
                ),
                trailing in any::<bool>(),
                root_spelt_otherwise in 0..3usize,
            ) {
                // A root not spelt as its components places every path by
                // its components.
                let (first, second) = match root_spelt_otherwise {
                    0 => ("/d", "/n/d"),
                    1 => ("/d", "/n/./d/"),
                    _ => ("/d/", "/n/d"),
                };
                let roots = ImmutableRoots {
                    dotnet: vec![PathBuf::from(first), PathBuf::from(second)],
                    nuget: vec![PathBuf::from("/n")],
                };
                let alias = Read {
                    path: PathBuf::from("/d/sdk/1.0"),
                    observation: Observation::Canonical(Ok(PathBuf::from("/real/sdk/1.0"))),
                };
                let versions = roots.versions(std::slice::from_ref(&alias));
                let aliases = aliases_reference(&roots, std::slice::from_ref(&alias));
                prop_assert_eq!(&versions.aliases, &aliases);
                let path = PathBuf::from(format!(
                    "{prefix}/{}{}",
                    names.join("/"),
                    if trailing { "/" } else { "" }
                ));
                prop_assert_eq!(
                    versions.containing(&path).map(std::borrow::Cow::into_owned),
                    containing_reference(&roots, &aliases, &path),
                    "{:?}",
                    path
                );
            }
        }

        /// [`partition_reads`] as it was first written: every read's version
        /// found twice, the versions shown kept as a set of paths, and the
        /// exempt reads found by hashing the confirmed ones.
        fn partition_reference(
            reads: &[Read],
            immutable: &ImmutableRoots,
            committed: impl Fn(&Path) -> bool,
        ) -> (Vec<Read>, Vec<Read>) {
            let aliases = aliases_reference(immutable, reads);
            let containing = |path: &Path| containing_reference(immutable, &aliases, path);
            let is_package =
                |version: &Path| immutable.nuget.iter().any(|root| version.starts_with(root));
            let shown: std::collections::BTreeSet<PathBuf> = reads
                .iter()
                .filter(|read| answered(&read.observation))
                .filter_map(|read| containing(&read.path))
                .filter(|version| !is_package(version) || committed(version))
                .collect();
            let trusted = |read: &&Read| {
                a_fact(&read.observation)
                    && containing(&read.path).is_some_and(|version| shown.contains(&version))
            };
            let confirming: Vec<Read> = reads
                .iter()
                .filter(|read| !trusted(read))
                .cloned()
                .chain(shown.iter().map(|version| Read {
                    path: version.clone(),
                    observation: Observation::Kind(Ok(msbuild_observed::EntryKind::Dir)),
                }))
                .collect();
            let confirmed: HashSet<&Read> = confirming.iter().collect();
            let exempt = reads
                .iter()
                .filter(|read| !confirmed.contains(read))
                .cloned()
                .collect();
            (confirming, exempt)
        }

        proptest! {
            #![proptest_config(ProptestConfig { cases: 1024, ..ProptestConfig::default() })]

            /// The read partition a cached evaluation and an env confirm by
            /// is the reference's for every read set: reads inside and
            /// outside versions, under every spelling, answered, absent and
            /// failed, under package versions committed or not, and through
            /// a version alias, with an install root spelt as its components
            /// or otherwise.
            #[test]
            fn reads_are_partitioned_as_the_reference_partitions_them(
                reads in prop::collection::vec(
                    (
                        prop::sample::select(vec![
                            "/d/sdk/1.0/x", "/d/sdk/1.0", "/d/sdk/1.0/./y", "/d//sdk/1.0/z",
                            "/d/sdk/2/x/", "/d/sdk/x/y", "/d/packs/p/1.0/a", "/d/packs/p",
                            "/d/sdk-manifests/b/m/3/w", "/n/pkg/1.0/lib", "/n/pkg/2.0/lib",
                            "/n/other/1.5/x", "/n/pkg", "/real/sdk/1.0/x", "/real/sdk/1.0",
                            "/repo/App.fsproj", "/d/sdk/1.0/../2/x", "/dd/sdk/1.0/x",
                            "/nn/pkg/1.0/x", "/real/sdk/1.00/x",
                        ]),
                        0usize..7,
                    ),
                    0..12,
                ),
                uncommitted in prop::sample::subsequence(vec!["/n/pkg/1.0", "/n/pkg/2.0", "/n/other/1.5"], 0..=3),
                aliased in any::<bool>(),
                dotnet_root in prop::sample::select(vec!["/d", "/d/", "/./d"]),
            ) {
                let failure = |kind: std::io::ErrorKind| {
                    msbuild_observed::Failure::of(&std::io::Error::from(kind))
                };
                let mut reads: Vec<Read> = reads
                    .into_iter()
                    .map(|(path, observation)| Read {
                        path: PathBuf::from(path),
                        observation: match observation {
                            0 => Observation::Text(Ok(msbuild_observed::TextDigest::of(""))),
                            1 => Observation::Text(Err(failure(std::io::ErrorKind::NotFound))),
                            2 => Observation::Text(Err(failure(std::io::ErrorKind::PermissionDenied))),
                            3 => Observation::Kind(Ok(msbuild_observed::EntryKind::File)),
                            4 => Observation::Kind(Err(failure(std::io::ErrorKind::NotFound))),
                            5 => Observation::Listing(Ok(vec![msbuild_observed::Listed::Unreadable(
                                failure(std::io::ErrorKind::Other),
                            )])),
                            _ => Observation::Listing(Ok(Vec::new())),
                        },
                    })
                    .collect();
                if aliased {
                    reads.push(Read {
                        path: PathBuf::from("/d/sdk/1.0"),
                        observation: Observation::Canonical(Ok(PathBuf::from("/real/sdk/1.0"))),
                    });
                }
                let reads: ReadSet = reads.into_iter().collect();
                let immutable = ImmutableRoots {
                    dotnet: vec![PathBuf::from(dotnet_root)],
                    nuget: vec![PathBuf::from("/n")],
                };
                let committed = |version: &Path| !uncommitted.iter().any(|u| Path::new(u) == version);
                let partition = partition_reads(reads.reads(), &immutable, committed);
                let (mut confirming, exempt) =
                    partition_reference(reads.reads(), &immutable, committed);
                let mut confirmed: Vec<Read> = partition
                    .confirmed
                    .iter()
                    .copied()
                    .chain(&partition.versions)
                    .cloned()
                    .collect();
                confirmed.sort();
                confirming.sort();
                prop_assert_eq!(confirmed, confirming);
                let partitioned: Vec<Read> = partition.exempt.into_iter().cloned().collect();
                prop_assert_eq!(partitioned, exempt);
            }
        }

        /// A project evaluated again after an edit that moves none of its
        /// other reads (a comment) starts with the dropped evaluation's
        /// witnesses: its first confirmation costs what a confirmation of an
        /// unchanged evaluation does, the directories' `stat`s and nothing
        /// asked again (#733). Without them it would ask every read again,
        /// walking each one's path. The project file is rewritten in place,
        /// so no directory's stamp moves.
        ///
        /// The evaluation's absence probes reach the temporary directory's
        /// ancestors, where concurrent tests make and remove directories,
        /// and a witness that sees one moved asks again, as it must; so the
        /// scenario is run until a run sees nothing else move. Without the
        /// dropped evaluation's witnesses no run can.
        #[test]
        fn a_project_evaluated_again_starts_with_the_dropped_ones_witnesses() {
            let attempt = || {
                let tmp = TempDir::new().unwrap();
                let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
                let (project, env) = world(&root, false);
                let file = root.join("repo/App/A.fs");
                let mut ws = Workspace::with_env(env);
                let mut at = 0;
                let mut generation = |ws: &mut Workspace| {
                    // Old enough that no stamp is racy, so witnesses are used.
                    ws.begin_generation(Nanos(i128::MAX / 2 + at));
                    at += 1;
                    assert!(ws.symbols_for(&file).contains("COMMON"));
                    ws.end_generation().evaluations
                };
                assert_eq!(generation(&mut ws).evaluated, 1);
                // The first confirmation takes the witnesses; the second is
                // what confirming an unchanged evaluation costs.
                generation(&mut ws);
                let steady = generation(&mut ws);
                assert_eq!(steady.evaluated, 0, "{steady:?}");
                assert!(steady.validation.stats > 0, "{steady:?}");

                let text = crate::observed::host_fs::read_to_string(&project).unwrap();
                fs::write(
                    &project,
                    text.replace("<ItemGroup>", "<!-- c --><ItemGroup>"),
                )
                .unwrap();
                assert_eq!(generation(&mut ws).evaluated, 1);
                (generation(&mut ws), steady)
            };
            let mut runs = Vec::new();
            while runs.len() < 5 && !runs.last().is_some_and(|(first, steady)| first == steady) {
                runs.push(attempt());
            }
            assert!(
                runs.iter().any(|(first, steady)| first == steady),
                "the new evaluation's first confirmation, against a steady one: {runs:?}"
            );
        }

        /// A file only the second, seeded pass reads (an import gated on the
        /// target framework a multi-targeted project is re-evaluated under)
        /// is one the cached evaluation depends on.
        #[test]
        fn a_file_only_the_seeded_pass_reads_is_confirmed() {
            let tmp = TempDir::new().unwrap();
            let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
            let project = root.join("App/App.fsproj");
            write_file(
                &project,
                r#"<Project>
  <PropertyGroup><TargetFrameworks>net8.0;net9.0</TargetFrameworks></PropertyGroup>
  <Import Project="tfm.props" Condition="'$(TargetFramework)' == 'net8.0'" />
  <ItemGroup><Compile Include="A.fs" /></ItemGroup>
</Project>"#,
            );
            let props = |define: &str| {
                format!(
                    "<Project><PropertyGroup><DefineConstants>$(DefineConstants);{define}</DefineConstants></PropertyGroup></Project>"
                )
            };
            write_file(&root.join("App/tfm.props"), &props("ONE"));
            write_file(&root.join("App/A.fs"), "module A\n");
            let file = root.join("App/A.fs");
            let mut ws = Workspace::default();
            ws.begin_generation(Nanos(i128::MAX / 2));
            assert!(ws.symbols_for(&file).contains("ONE"));
            ws.end_generation();
            write_file(&root.join("App/tfm.props"), &props("TWO"));
            ws.begin_generation(Nanos(i128::MAX / 2 + 1));
            assert!(ws.symbols_for(&file).contains("TWO"));
            ws.end_generation();
        }

        /// A read inside a version directory is trusted only while that
        /// version is shown to exist: an `Exists`-guarded import of a file in
        /// a version not yet installed sees the install.
        #[test]
        fn a_version_installed_after_the_evaluation_is_seen() {
            let tmp = TempDir::new().unwrap();
            let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
            let (_, env) = world(&root, false);
            let import = root.join("dotnet/packs/P/1.0.0/x.props");
            let project = root.join("repo/Lib/Lib.fsproj");
            write_file(
                &project,
                &format!(
                    r#"<Project>
  <Import Project="{0}" Condition="Exists('{0}')" />
  <ItemGroup><Compile Include="L.fs" /></ItemGroup>
</Project>"#,
                    import.display()
                ),
            );
            write_file(&root.join("repo/Lib/L.fs"), "module L\n");
            let file = root.join("repo/Lib/L.fs");
            let mut ws = Workspace::with_env(env);
            ws.begin_generation(Nanos(i128::MAX / 2));
            assert!(!ws.symbols_for(&file).contains("INSTALLED"));
            ws.end_generation();
            write_file(
                &import,
                "<Project><PropertyGroup><DefineConstants>$(DefineConstants);INSTALLED</DefineConstants></PropertyGroup></Project>",
            );
            ws.begin_generation(Nanos(i128::MAX / 2 + 1));
            assert!(ws.symbols_for(&file).contains("INSTALLED"));
            ws.end_generation();
        }

        /// The reads of a pass whose evaluation is discarded still decide
        /// the entry: a seeded pass that meets a `TreatAsLocalProperty`
        /// opt-out makes the target framework untrusted, and removing the
        /// opt-out trusts it again.
        #[test]
        fn a_discarded_pass_reads_are_confirmed() {
            let tmp = TempDir::new().unwrap();
            let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
            let project = root.join("App/App.fsproj");
            write_file(
                &project,
                r#"<Project>
  <PropertyGroup><TargetFrameworks>net8.0;net9.0</TargetFrameworks></PropertyGroup>
  <Import Project="tfm.props" Condition="'$(TargetFramework)' == 'net8.0'" />
</Project>"#,
            );
            let props = root.join("App/tfm.props");
            write_file(
                &props,
                r#"<Project TreatAsLocalProperty="TargetFramework" />"#,
            );
            let mut ws = Workspace::default();
            ws.begin_generation(Nanos(i128::MAX / 2));
            assert_eq!(ws.served_tfm_for_project(&project), ServedTfm::Untrusted);
            ws.end_generation();
            write_file(&props, "<Project />");
            ws.begin_generation(Nanos(i128::MAX / 2 + 1));
            assert_eq!(
                ws.served_tfm_for_project(&project),
                ServedTfm::Tfm("net8.0".to_string())
            );
            ws.end_generation();
        }

        /// The graph walk reads the entry's cached evaluation only when the
        /// open generation has confirmed it: a walk that comes first in a
        /// generation evaluates the entry as it is on disk.
        #[test]
        fn the_graph_walk_reads_only_a_confirmed_entry() {
            let tmp = TempDir::new().unwrap();
            let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
            let a = root.join("A/A.fsproj");
            let b = root.join("B/B.fsproj");
            write_file(&a, "<Project />");
            write_file(&b, "<Project />");
            let mut ws = Workspace::default();
            ws.begin_generation(Nanos(i128::MAX / 2));
            assert!(ws.project(&a).is_some());
            assert_eq!(ws.project_graph(&a).nodes.len(), 1);
            ws.end_generation();
            write_file(
                &a,
                r#"<Project><ItemGroup><ProjectReference Include="../B/B.fsproj" /></ItemGroup></Project>"#,
            );
            ws.begin_generation(Nanos(i128::MAX / 2 + 1));
            assert_eq!(ws.project_graph(&a).nodes.len(), 2);
            ws.end_generation();
        }

        /// A compile-closure walk that reads the confirmed entry rather than
        /// evaluating it records the reads that confirm it, so the env's
        /// read set still names the entry's project file; and it records the
        /// read set of every node it evaluates afresh.
        #[test]
        fn the_graph_walk_records_the_confirmed_entry_and_each_evaluated_node() {
            let tmp = TempDir::new().unwrap();
            let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
            let a = root.join("A/A.fsproj");
            let b = root.join("B/B.fsproj");
            write_file(
                &a,
                r#"<Project><ItemGroup><ProjectReference Include="../B/B.fsproj" /></ItemGroup></Project>"#,
            );
            write_file(&b, "<Project />");
            let mut ws = Workspace::default();
            ws.begin_generation(Nanos(i128::MAX / 2));
            assert!(ws.project(&a).is_some());
            let evaluated_before = ws.evaluated.get();
            let observer = Observer::new();
            let graph = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new(), &observer);
            assert_eq!(graph.nodes.len(), 2);
            assert_eq!(
                ws.evaluated.get(),
                evaluated_before + 1,
                "precondition: the walk evaluates B alone, reading A's confirmed entry"
            );
            ws.end_generation();
            let (reads, _) = observer.finish();
            for project in [&a, &b] {
                assert!(
                    reads.reads().iter().any(|read| read.path == *project
                        && matches!(read.observation, Observation::Text(Ok(_)))),
                    "the walk's read set does not name {project:?}: {reads:?}"
                );
            }
        }

        /// A rewrite of a file inside an SDK version is not looked for: the
        /// read is an immutable content. Installing a version beside it is.
        #[test]
        fn an_sdk_version_is_trusted_but_its_listing_is_confirmed() {
            let tmp = TempDir::new().unwrap();
            let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
            let (_, env) = world(&root, false);
            let file = root.join("repo/App/A.fs");
            let mut ws = Workspace::with_env(env);
            let generation = |ws: &mut Workspace, at: i128| ws.begin_generation(Nanos(at));
            generation(&mut ws, i128::MAX / 2);
            assert!(ws.symbols_for(&file).contains("SDK_ONE"));
            ws.end_generation();

            install_sdk(&root.join("dotnet"), "10.0.100", "REWRITTEN");
            generation(&mut ws, i128::MAX / 2 + 1);
            let symbols = ws.symbols_for(&file);
            assert!(
                symbols.contains("SDK_ONE") && !symbols.contains("REWRITTEN"),
                "{symbols:?}"
            );
            ws.end_generation();

            install_sdk(&root.join("dotnet"), "10.0.200", "SDK_TWO");
            generation(&mut ws, i128::MAX / 2 + 2);
            assert!(ws.symbols_for(&file).contains("SDK_TWO"));
            ws.end_generation();
        }
    }
}
