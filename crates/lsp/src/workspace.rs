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
//! - [`find_owning_project`] is the pure ancestor walk — `file`'s
//!   directory first, then its parents, returning the first directory
//!   that contains at least one `.fsproj`. Multiple `.fsproj`s in the
//!   same directory are tie-broken alphabetically for determinism.
//!
//! No file-system watching: edits to a `.fsproj` while the server is
//! running do *not* invalidate the cache (the next LSP restart picks
//! them up). Smarter invalidation is deferred until we wire the LSP to
//! `workspace/didChangeWatchedFiles`.
//!
//! SDK resolution is wired through [`SdkDiscovery`]: each project gets
//! its own discovery context (built from a [`SdkDiscoveryEnv`] held on
//! the workspace) and the resulting resolver is handed to
//! `parse_fsproj_with_imports`. When discovery fails — most commonly
//! because `$DOTNET_ROOT` isn't set and no `dotnet` is on `$PATH` — we
//! log to stderr and fall back to evaluating the project without an SDK
//! resolver. The msbuild evaluator surfaces an
//! `UnsupportedConstruct` diagnostic for `<Project Sdk="...">` in that
//! case, but evaluation still completes and the body's own
//! `<DefineConstants>` are still collected.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use borzoi_cst::language_version::LanguageVersion;
use borzoi_msbuild::{
    GlobResolver, ParsedProject, ResolvedItem, SdkResolution, SdkResolver,
    parse_fsproj_with_imports, target_frameworks,
};

use crate::paths::{lexically_normalize, paths_equal};
use crate::project_graph::{
    Edge, EdgeKind, NodeResult, NodeTfm, ProjectGraph, ProjectKind, build_graph, classify,
};
use crate::sdk_discovery::{SdkDiscovery, SdkDiscoveryEnv};
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
/// `pub(crate)` so the server's non-`file:` buffer fallback shares it rather
/// than hand-building a now-incomplete `{COMPILED}` set.
pub(crate) fn implicit_symbols(is_script: bool) -> HashSet<String> {
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
pub struct Workspace {
    /// Keyed by the canonicalised project path. The value is `None`
    /// when evaluation failed for any reason (IO, malformed XML,
    /// reserved-property collision in `extra_properties`) — caching
    /// the failure means we don't re-attempt on every keystroke.
    projects: HashMap<PathBuf, Option<EvaluatedProject>>,
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
    /// `docs/fsproj-tfm-selection-plan.md` E1/E2): the caller-seeded or
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

    /// Drop every cached project evaluation, forcing re-evaluation from disk on
    /// the next lookup. Used by `workspace/didChangeWatchedFiles` when a
    /// `.fsproj` / `Directory.Build.*` / `global.json` changes on disk: the SDK
    /// environment (`env`) is unchanged — only the cached parses are stale. A
    /// broad clear (rather than per-project) keeps invalidation obviously
    /// correct when a `Directory.Build.*` change affects a whole subtree; the
    /// cache refills lazily on the next lookup.
    pub fn invalidate_projects(&mut self) {
        self.projects.clear();
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
        if let Some(root) = self
            .evaluated(project_path)
            .and_then(|e| e.sdk_install_root.clone())
        {
            return Some(root);
        }
        self.probe_dotnet_root(project_path)
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
    fn probe_dotnet_root(&self, project_path: &Path) -> Option<PathBuf> {
        let disc = SdkDiscovery::for_project(project_path, &self.env).ok()?;
        if let Ok(SdkResolution::Single(paths)) = disc.resolve("Microsoft.NET.Sdk")
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
            if let Some(project_path) = self.owning_project(file)
                && matches!(self.membership(&project_path, file), Membership::Member)
            {
                return self.symbols_for_project(&project_path);
            }
            return implicit_symbols(true);
        }
        match self.owning_project(file) {
            Some(project_path) => self.symbols_for_project(&project_path),
            None => implicit_symbols(false),
        }
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
    /// ([`LanguageVersion::strict_indentation_is_error`] /
    /// `reports_invalid_decls_in_type_definitions`), and `DEFAULT` discards
    /// an evaluated value that is still the best single guess (the gate the
    /// walker couldn't pin usually *does* run). And the generic provenance
    /// mark could not gate anything alone anyway: a real SDK's own
    /// conditional LangVersion default trips it for **every** project
    /// (probed, dotnet 10.0.301, even with a cleanly body-pinned value) —
    /// see `sdk_project_fold_e2e`. The version does shape the *token
    /// stream* (the lex-filter threads `strictIndentation` into its
    /// SeqBlock push decision at the F# 8 boundary); the cross-file fold
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
    /// [`find_owning_project`]'s nearest-ancestor, alphabetically-first rule,
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
        let mut dir = file.parent();
        while let Some(d) = dir {
            let mut inconclusive = false;
            for cand in all_fsproj_in(d) {
                match self.membership(&cand, file) {
                    Membership::Member => return Some(cand),
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
        find_owning_project(file)
    }

    /// The evaluated project at `project_path`, lazily computed and cached
    /// (keyed by canonicalised path). `None` when evaluation failed; the
    /// failure is cached too, so we don't re-read on every keystroke.
    pub fn project(&mut self, project_path: &Path) -> Option<&ParsedProject> {
        self.evaluated(project_path).map(|e| &e.parsed)
    }

    /// The cached [`EvaluatedProject`] for `project_path` — the parse plus the
    /// SDK install root the evaluator resolved — lazily computed and cached
    /// (keyed by canonicalised path). `None` when evaluation failed; the
    /// failure is cached too.
    fn evaluated(&mut self, project_path: &Path) -> Option<&EvaluatedProject> {
        let key =
            std::fs::canonicalize(project_path).unwrap_or_else(|_| project_path.to_path_buf());
        let env = &self.env;
        let extra_build_properties = &self.extra_build_properties;
        self.projects
            .entry(key)
            .or_insert_with(|| evaluate_project(project_path, env, extra_build_properties))
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
    /// **Evaluates every node fresh and leaves the project cache untouched.**
    /// Unlike [`Workspace::project`], this does not read or populate
    /// `Workspace.projects`; each node is parsed from disk via
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
        self.project_graph_impl(entry, &BTreeMap::new(), GraphWalkPurpose::DeclaredStructure)
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
    pub fn project_graph_with_producer_tfms(
        &self,
        entry: &Path,
        producer_tfms: &BTreeMap<PathBuf, String>,
    ) -> ProjectGraph {
        self.project_graph_impl(entry, producer_tfms, GraphWalkPurpose::CompileClosure)
    }

    fn project_graph_impl(
        &self,
        entry: &Path,
        producer_tfms: &BTreeMap<PathBuf, String>,
        purpose: GraphWalkPurpose,
    ) -> ProjectGraph {
        // The entry is resolved exactly once, with its own path: the builder
        // never re-resolves a visited node, and every non-entry resolve comes
        // through an already-normalised edge target. Compare normalised so an
        // entry spelled with `.`/`..` still recognises itself.
        let entry_key = lexically_normalize(entry);
        build_graph(entry, |path| {
            // The map's keys are canonicalised; graph paths are only
            // lexically normalised. A path that fails to canonicalise (not
            // on disk) can't be in the map — fall through to no seed.
            let seed_tfm = std::fs::canonicalize(path)
                .ok()
                .and_then(|canon| producer_tfms.get(&canon))
                .map(String::as_str);
            resolve_node_uncached(
                path,
                &self.env,
                &self.extra_build_properties,
                seed_tfm,
                purpose,
                lexically_normalize(path) == entry_key,
            )
        })
    }
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
/// contains a `.fsproj`. Returns the absolute path of the chosen project
/// file, or `None` when no ancestor contains any `.fsproj`.
///
/// On ties (a single directory containing more than one `.fsproj`), the
/// alphabetically-first filename wins. This is the membership-blind
/// heuristic; [`Workspace::owning_project`] layers `<Compile>`-membership on
/// top and uses this only as the fallback when no candidate lists `file`.
pub fn find_owning_project(file: &Path) -> Option<PathBuf> {
    // Start from the file's parent (the file itself may not exist).
    let mut dir = file.parent()?;
    loop {
        if let Some(found) = all_fsproj_in(dir).into_iter().next() {
            return Some(found);
        }
        dir = dir.parent()?;
    }
}

/// All `.fsproj` directly in `dir`, sorted by filename for determinism.
/// Errors from `read_dir` (no permission, not a directory, etc.) yield an
/// empty list — the caller treats "no `.fsproj` here" the same way.
fn all_fsproj_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut hits: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("fsproj"))
        .collect();
    hits.sort();
    hits
}

/// The node's effective output-file base name from one evaluation
/// ([`ProjectNode::output_name`]): the trusted evaluated `$(TargetName)`
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

/// Read and evaluate `project_path`. Returns `None` on any failure
/// (IO, XML, or evaluation) — the caller caches the `None` so we don't
/// retry on every keystroke.
///
/// Builds a per-project [`SdkDiscovery`] from `env`; on
/// [`crate::sdk_discovery::DiscoveryError`] (e.g. missing `$DOTNET_ROOT`,
/// malformed `global.json`) we log and fall through to evaluating
/// without a resolver, mirroring the behaviour callers had before
/// Stage 8b.
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
    env: &SdkDiscoveryEnv,
    extra_build_properties: &HashMap<String, String>,
    seed_tfm: Option<&str>,
    purpose: GraphWalkPurpose,
    is_entry: bool,
) -> NodeResult {
    if !path.exists() {
        return NodeResult::NotFound;
    }
    if classify(path) != ProjectKind::FSharp {
        return NodeResult::resolved(Vec::new());
    }
    // Outer (seedless) evaluation first: its first-pass view is the source of
    // truth for what the project currently declares, and its edges are the
    // first-declared inner build's (via `select_target_framework`).
    let Some(outer) = evaluate_project(path, env, extra_build_properties) else {
        return NodeResult::resolved(Vec::new());
    };
    let outer_edges = edges_of(&outer, purpose, is_entry);
    if caller_owns_target_framework(extra_build_properties) {
        return NodeResult::Resolved {
            edges: outer_edges,
            tfm: match &outer.chosen_tfm {
                Some(tfm) => NodeTfm::Known(tfm.clone()),
                None => NodeTfm::NoneDeclared,
            },
            output_name: evaluated_output_name(path, &outer),
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
        };
    }
    if let Some(body) = &outer.body_target_framework {
        // The project pins its own TFM: current truth, whatever any old
        // restore recorded.
        return NodeResult::Resolved {
            edges: outer_edges,
            tfm: NodeTfm::Known(body.clone()),
            output_name: evaluated_output_name(path, &outer),
        };
    }
    let declared = &outer.declared_tfms;
    if declared.len() <= 1 {
        // Single-target (or no TFM declared at all): there is only one build
        // the project can produce; the seed adds nothing.
        return NodeResult::Resolved {
            edges: outer_edges,
            tfm: match declared.first() {
                Some(sole) => NodeTfm::Known(sole.clone()),
                None => NodeTfm::NoneDeclared,
            },
            output_name: evaluated_output_name(path, &outer),
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
                    output_name: evaluated_output_name(path, &outer),
                };
            }
            let mut map = extra_build_properties.clone();
            seed_target_framework_global(&mut map, current);
            return match evaluate_project(path, env, &map) {
                Some(inner) => NodeResult::Resolved {
                    edges: edges_of(&inner, purpose, is_entry),
                    tfm: NodeTfm::Known(current.clone()),
                    // The name must come from the same (inner) evaluation as
                    // the edges: `<AssemblyName>` may itself be TFM-gated.
                    output_name: evaluated_output_name(path, &inner),
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
            for tfm in declared.iter().skip(1) {
                let mut map = extra_build_properties.clone();
                seed_target_framework_global(&mut map, tfm);
                let Some(branch) = evaluate_project(path, env, &map) else {
                    return NodeResult::Resolved {
                        edges: Vec::new(),
                        tfm: NodeTfm::Unresolved,
                        output_name: None,
                    };
                };
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
                if surviving.is_empty() {
                    break;
                }
            }
            NodeResult::Resolved {
                edges: surviving,
                tfm: NodeTfm::Unresolved,
                output_name: None,
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
    if purpose == GraphWalkPurpose::CompileClosure && evaluated.parsed.project_references_uncertain
    {
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

fn evaluate_project(
    project_path: &Path,
    env: &SdkDiscoveryEnv,
    extra_build_properties: &HashMap<String, String>,
) -> Option<EvaluatedProject> {
    let _span =
        tracing::info_span!("evaluate_project", project = %project_path.display()).entered();
    let source = std::fs::read_to_string(project_path).ok()?;
    let disc = SdkDiscovery::for_project(project_path, env)
        .inspect_err(|err| {
            crate::log_warn!(
                "SDK discovery failed; evaluating without resolver",
                project = project_path.display(),
                error = err
            );
        })
        .ok();
    let extras = build_properties(extra_build_properties);
    let parsed = parse_with_optional_sdk(
        &source,
        project_path,
        &extras,
        &env.build_environment,
        disc.as_ref(),
    )?;
    // First-pass views, captured before any first-declared re-evaluation
    // (`select_target_framework` case 3 replaces `parsed` with an inner-build
    // pass in which an outer-gated `<TargetFrameworks>` may no longer exist).
    let declared_tfms = target_frameworks(&parsed);
    let body_target_framework = lookup_property_ci(&parsed.properties, "TargetFramework")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from);
    // Deliberately the singular only: an outer-gated PLURAL (the arcade
    // idiom, `<TargetFrameworks Condition="'$(TargetFramework)' == ''">`)
    // is unpinned by construction — its gate reads the then-undefined
    // `TargetFramework` — yet the TFM-invariant intersection consumes the
    // declared list without trusting any single branch, so distrusting it
    // here would break the idiom for nothing. The body singular, by
    // contrast, is consumed as an authoritative `NodeTfm::Known`.
    let tfm_untrusted = parsed.property_provenance_untrusted("TargetFramework")
        || body_target_framework
            .as_deref()
            .is_some_and(|v| v.contains("$("));
    let (parsed, chosen_tfm) = select_target_framework(
        parsed,
        &source,
        project_path,
        &extras,
        &env.build_environment,
        disc.as_ref(),
        tfm_untrusted,
    );
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
    Some(EvaluatedProject {
        parsed,
        sdk_install_root,
        chosen_tfm,
        declared_tfms,
        body_target_framework,
        tfm_untrusted,
    })
}

/// Pick the target framework to serve this project under, re-evaluating with
/// it seeded when that changes the answer (fsproj 3.3c, plan E1/E2).
///
/// Policy: first-declared. Precisely, the chosen TFM is
///
/// 1. the caller-seeded `TargetFramework` global when present (any casing,
///    any value — `None` when it's empty). The caller owns the choice: an
///    empty read-only global is an explicit "no TFM", and re-seeding would
///    both override that input and trip the evaluator's case-insensitive
///    duplicate-key validation, failing the whole evaluation;
/// 2. else the body-written `<TargetFramework>` when non-empty. MSBuild's
///    outer/inner gate is `'$(TargetFrameworks)' != '' and
///    '$(TargetFramework)' == ''`, so a non-empty singular is a single-target
///    build even when the plural is also set. Pass 1 already evaluated under
///    it — no second pass;
/// 3. else the **first** `target_frameworks()` entry, under which the project
///    is re-evaluated with `TargetFramework` seeded as a read-only global —
///    exactly what an MSBuild inner build does — so `$(TargetFramework)`-gated
///    defines and Compile items become evaluable instead of flipping the
///    `*_uncertain` flags. NOT taken when pass 1's `TargetFramework` is
///    `tfm_untrusted` (an unpinned empty singular alongside the plural): the
///    real build may be a *single* build under a value we can't see, so
///    seeding would evaluate the gated defines/items cleanly under a choice
///    [`Workspace::target_framework_for_project`] declines to serve — pairing
///    first-declared parses with whatever the env's no-TFM fallback selects
///    (the E5 incoherence). Keeping pass 1 lets every read of the unpinned
///    `TargetFramework` flip its own `*_uncertain` flag instead;
/// 4. else `None` (no TFM declared anywhere): keep pass 1 unchanged.
///
/// The one extra parse in case 3 happens once per multi-targeted project and
/// is cached with the evaluation.
fn select_target_framework(
    pass1: ParsedProject,
    source: &str,
    project_path: &Path,
    extras: &HashMap<String, String>,
    environment: &HashMap<String, String>,
    disc: Option<&SdkDiscovery>,
    tfm_untrusted: bool,
) -> (ParsedProject, Option<String>) {
    if let Some(value) = lookup_property_ci(extras, "TargetFramework") {
        let trimmed = value.trim();
        let chosen = (!trimmed.is_empty()).then(|| trimmed.to_string());
        return (pass1, chosen);
    }
    let body_tf = lookup_property_ci(&pass1.properties, "TargetFramework")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from);
    if body_tf.is_some() {
        return (pass1, body_tf);
    }
    if tfm_untrusted {
        return (pass1, None);
    }
    let declared = target_frameworks(&pass1);
    let Some(first) = declared.first().cloned() else {
        return (pass1, None);
    };
    let mut seeded = extras.clone();
    seeded.insert("TargetFramework".to_string(), first.clone());
    match parse_with_optional_sdk(source, project_path, &seeded, environment, disc) {
        Some(pass2) => (pass2, Some(first)),
        // Same source, same resolver, and the seed key can't collide (the
        // caller-global case returned above) — so this arm shouldn't be
        // reachable. Degrade to the unseeded view rather than failing the
        // whole project.
        None => (pass1, None),
    }
}

/// Whether the caller's extra build properties pin a **non-empty**
/// `TargetFramework` global — the caller then owns the TFM choice, and its
/// value needs no provenance (globals out-rank body writes). An EMPTY
/// caller-supplied `TargetFramework` is the outer (dispatch) build, not a
/// TFM choice — the SDK's inner-build gate is exactly
/// `'$(TargetFramework)' == ''` — so it does not count as ownership and
/// falls through to the normal declared-TFM classification (in
/// [`resolve_node_uncached`], reading it as ownership would classify a
/// multi-targeted node `NoneDeclared` and let the output locator fold a
/// lone stale variant). Shared between [`resolve_node_uncached`] and
/// [`Workspace::target_framework_for_project`] so the graph-node and
/// entry-side provenance gates cannot drift.
fn caller_owns_target_framework(extra_build_properties: &HashMap<String, String>) -> bool {
    extra_build_properties
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("TargetFramework") && !v.trim().is_empty())
}

/// Seed `TargetFramework` as a build global for an inner-build (per-TFM)
/// evaluation, **replacing** any case-insensitively equal existing key.
/// MSBuild global-property names compare OrdinalIgnoreCase and the
/// evaluator's input validation rejects case-insensitive duplicates, so a
/// caller-supplied differently-cased key (e.g. an explicitly empty
/// `targetframework`, which deliberately falls through to per-TFM
/// evaluation) must be displaced, not joined — a duplicate fails the whole
/// branch evaluation and even TFM-invariant edges would vanish.
fn seed_target_framework_global(map: &mut HashMap<String, String>, tfm: &str) {
    map.retain(|k, _| !k.eq_ignore_ascii_case("TargetFramework"));
    map.insert("TargetFramework".to_string(), tfm.to_string());
}

/// Case-insensitive property lookup (MSBuild property names compare
/// OrdinalIgnoreCase; both `extra_properties` keys and
/// [`ParsedProject::properties`] preserve the source spelling).
fn lookup_property_ci<'a>(map: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// MSBuild-default build globals used when evaluating a project for
/// LSP diagnostics. The SDK's own `*.props` predicate `DefineConstants`
/// on these (e.g. `DEBUG` is gated on `$(Configuration) == 'Debug'`),
/// so passing an empty bag would silently drop those symbols and
/// produce diagnostics against the wrong `#if` branches.
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
/// served TFM (first-declared — `docs/fsproj-tfm-selection-plan.md` E1/E2)
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
) -> Option<ParsedProject> {
    // Glob resolution is independent of SDK discovery, so both arms get the
    // filesystem-backed resolver. It borrows nothing, so it lives for the
    // whole function.
    let glob_resolver: &GlobResolver<'_> = &crate::glob_resolver::resolve;
    match disc {
        Some(d) => {
            let resolver: &SdkResolver<'_> = &|name| d.resolve(name);
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

        assert_eq!(find_owning_project(&file), Some(proj));
    }

    #[test]
    fn find_owning_project_walks_up_to_parent_directory() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Sample.fsproj");
        let file = tmp.path().join("nested").join("deep").join("Lib.fs");
        write_file(&proj, &fsproj_with_defines(""));
        write_file(&file, "let x = 1");

        assert_eq!(find_owning_project(&file), Some(proj));
    }

    #[test]
    fn find_owning_project_returns_none_when_no_fsproj_in_tree() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("Lib.fs");
        write_file(&file, "let x = 1");

        // The temp dir's ancestors (system temp, then `/tmp`, then `/`)
        // are extremely unlikely to contain a `.fsproj`. If they do, this
        // assertion catches it as a test-environment issue.
        assert_eq!(find_owning_project(&file), None);
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

        assert_eq!(find_owning_project(&file), Some(alpha));
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

        assert_eq!(find_owning_project(&file), Some(inner_proj));
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

        assert_eq!(find_owning_project(&file), None);
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
        assert_eq!(find_owning_project(&file), Some(alpha));

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
        assert_eq!(ws.owning_project(&file), find_owning_project(&file));
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
        assert_eq!(find_owning_project(&file), Some(other));

        let mut ws = Workspace::default();
        assert_eq!(ws.owning_project(&file), Some(root));
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
        assert_eq!(ws.owning_project(&file), find_owning_project(&file));
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

        let tfms = BTreeMap::from([(std::fs::canonicalize(&b).unwrap(), "net8.0".to_string())]);
        let seeded = ws.project_graph_with_producer_tfms(&a, &tfms);
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
        let seeded = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let seeded = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let graph = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let graph = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let unseeded = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let tfms = BTreeMap::from([(std::fs::canonicalize(&b).unwrap(), "net8.0".to_string())]);
        let seeded = ws.project_graph_with_producer_tfms(&a, &tfms);
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
        let seeded = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let stale = BTreeMap::from([(std::fs::canonicalize(&b).unwrap(), "net9.0".to_string())]);
        let graph = ws.project_graph_with_producer_tfms(&a, &stale);
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
                <ProjectReference Remove="../C/C.fsproj" />
              </ItemGroup>
            </Project>"#,
        );
        write_file(&c, &fsproj_with_refs(&[]));

        let ws = Workspace::default();
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
        let compile_closure = ws.project_graph_with_producer_tfms(&a, &BTreeMap::new());
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
            link: None,
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
            link: None,
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
            // `sdk_resolver=None`. The msbuild evaluator then emits an
            // `UnsupportedConstruct` for the `Sdk="..."` attribute but
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
}
