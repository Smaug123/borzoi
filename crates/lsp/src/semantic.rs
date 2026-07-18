//! Per-project parsing and (later stages) name resolution against
//! `borzoi-sema`. The `docs`-aware companion of
//! [`crate::workspace::Workspace`]: the workspace owns the `.fsproj` cache,
//! [`SemanticState`] owns whatever needs to see editor-buffer overlays on top.
//!
//! Stage 2 surfaces only [`SemanticState::parses_for_project`] — the
//! Compile-ordered `Vec<ImplFile>` for a project, preferring the in-memory
//! buffer of each member file over its on-disk text. Stage 3 adds an
//! `AssemblyEnv` cache; Stage 4 the orchestrator that runs
//! [`borzoi_sema::resolve_project`] and caches the result. Each stage
//! invalidates strictly by project path; sema's prefix-monotone fold makes
//! sub-project incrementality a later optimisation, not a v1 requirement.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity};
use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_msbuild::ItemKind;
use borzoi_sema::{AbbreviationVisibility, AssemblyEnv, ResolvedProject};
use lsp_types::Url;

use crate::assembly_cache::AssemblyCache;
use crate::cst_panic_safe::parse_with_symbols;
use crate::paths::{lexically_normalize, paths_equal};
use crate::project_assets::{
    resolve_assemblies_for_tfm, resolve_assemblies_root_only, resolve_transitive_project_tfms,
};
use crate::project_graph::{NodeTfm, ProjectGraph, ProjectKind};
use crate::restore::{RestoreOutcome, restore_to_scratch_assemblies};
use crate::sdk_discovery::SdkDiscoveryEnv;
use crate::sidecar_manager::SidecarManager;
use crate::workspace::{ServedTfm, Workspace};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ReferencedAssemblyProjection {
    entities: Vec<Entity>,
    /// [`AssemblyProjectionSkips::fsharp_abbreviations_unknowable`] for this
    /// DLL, carried through the on-disk cache so a hit reproduces the same
    /// [`AbbreviationVisibility`] a fresh enumeration would compute.
    fsharp_abbreviations_unknowable: bool,
    /// The DLL's assembly-level `[<assembly: AutoOpen("…")>]` paths
    /// (`EcmaView::assembly_auto_opens`, manifest order), carried through the
    /// on-disk cache like the flag above. Feeds the env's implicit opens —
    /// FSharp.Core's list is what makes `printfn` resolve.
    assembly_auto_opens: Vec<String>,
    /// Whether reading this DLL's `assembly_auto_opens` **failed** (implicit opens
    /// unknown) — feeds `AssemblyEnv::mark_extension_surface_unknowable` (global: an
    /// unknown auto-open could bring an extension into any namespace). `#[serde(default)]`
    /// so an old cache entry (a successful read) deserialises as `false`.
    #[serde(default)]
    auto_opens_unreadable: bool,
    /// [`AssemblyProjectionSkips::fsharp_extension_index_unknowable`] for this DLL —
    /// its F#-native extension-member index could not be built (absent/undecodable
    /// pickle). Folded into the env's per-assembly extension-knowability so a broken
    /// FSharp.Core pickle (abbreviation-exempt but extension-blind) still defers the
    /// name-keyed gate. `#[serde(default)]` for old cache entries.
    #[serde(default)]
    fsharp_extension_index_unknowable: bool,
    /// [`AssemblyProjectionSkips::fsharp_signature_non_authoritative`] for this DLL —
    /// its host F# pickle was not authoritative (absent/undecodable, or a
    /// `--standalone` image with foreign CCUs), so its `EntityKind::Module` markers
    /// are IL heuristics FCS does not share. Folded into the env so semantic-token
    /// classification declines module kinds for such an assembly. `#[serde(default)]`
    /// for old cache entries.
    #[serde(default)]
    fsharp_signature_non_authoritative: bool,
    /// The enclosing namespaces of the types this DLL **dropped** (undecodable —
    /// possibly a C#-style `[<Extension>]` class the entity tree no longer shows).
    /// Fed to `AssemblyEnv::mark_namespace_dropped_type` so the OV-6 gate treats each
    /// as possibly-extension-bearing — *namespace-scoped*, so a file whose in-scope
    /// namespaces had no drop still commits. `#[serde(default)]` for old cache entries.
    #[serde(default)]
    dropped_type_namespaces: Vec<Vec<String>>,
}

impl ReferencedAssemblyProjection {
    fn abbreviation_visibility(&self) -> AbbreviationVisibility {
        if self.fsharp_abbreviations_unknowable {
            AbbreviationVisibility::Unknowable
        } else {
            AbbreviationVisibility::Modelled
        }
    }
}

/// The per-project parses sema folds over. One entry per `<Compile>` file the
/// project lists, in source order; `paths[i]` is the file's absolute path
/// (matching `ParsedProject.items[i].include`) and `texts[i]` is whatever
/// text the parser saw — buffer if open, disk otherwise. A file that
/// couldn't be read or didn't parse to an [`ImplFile`] is omitted; the file's
/// index in `files`/`paths`/`texts` is therefore not necessarily its index in
/// the original `items` list (D5 "under-resolve, never wrong").
#[derive(Debug, Clone)]
pub struct ProjectParses {
    pub files: Vec<ImplFile>,
    pub paths: Vec<PathBuf>,
    pub texts: Vec<Arc<str>>,
}

impl ProjectParses {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }
}

/// One cached parse **variant** of a Compile item (see
/// [`SemanticState::file_parses`], which holds a list of these per path), stored
/// with the exact inputs that decided it — not only the parse inputs (source
/// text, `#if` symbols, language version) but the owning project's `LangVersion`
/// *provenance* trust. A [`build_parses`] cache hit requires **every** one to
/// match, so a variant is a pure function of its inputs: it can never be served
/// stale, nor bypass the version-boundary gate.
/// The last input matters because a hit *skips* that gate, whose accept/refuse
/// verdict depends on `lang_version_untrusted` — and one source file shared by
/// two projects can carry different provenance at the same effective version, so
/// a trusted project's accepted parse must not satisfy an untrusted one. See
/// [`SemanticState::file_parses`].
#[derive(Debug)]
struct CachedParse {
    /// The `#if` symbols the tree was parsed under.
    symbols: HashSet<String>,
    /// The language version the tree was parsed under.
    lang: LanguageVersion,
    /// Whether the owning project's `LangVersion` provenance was untrusted when
    /// this tree was accepted. Part of the key because a hit skips the
    /// version-boundary gate, whose verdict is a function of this flag (not of
    /// the file's text): without it, a trusted project's accepted parse of a
    /// straddling file could wrongly satisfy an untrusted project that a cold
    /// build refuses.
    lang_version_untrusted: bool,
    /// The exact source text that produced `file`.
    text: Arc<str>,
    /// The reusable parsed impl file (a rowan handle; a hit clones it — an `Arc`
    /// bump, not a re-parse).
    file: ImplFile,
}

/// The previous Compile-order fold of a project, kept so the *next* fold can be
/// computed incrementally ([`borzoi_sema::resolve_project_incremental`]) rather
/// than cold. Stored per project in [`SemanticState::prev_resolved`].
///
/// Holds the exact `ImplFile`s that were folded (so the incremental fold can
/// compare each new file against the tree that produced its previous
/// resolution, by rowan identity), the result, and the assembly env [`Arc`] the
/// fold ran against. The env Arc is the reuse *precondition witness*: reusing a
/// file's resolution is sound only against the same env it was resolved with, so
/// [`SemanticState::resolved_project_for`] reuses this only when the current env
/// is [`Arc::ptr_eq`] to [`Self::env`]. A rebuilt env — structural
/// invalidation, a referenced-DLL change, or a transient-sidecar recovery — is a
/// fresh Arc, so the pointer check fails and the fold falls back to cold. This
/// makes "same env" machine-checked rather than a reasoned invariant.
#[derive(Debug)]
struct PrevFold {
    files: Vec<ImplFile>,
    resolved: Arc<ResolvedProject>,
    env: Arc<AssemblyEnv>,
}

/// The semantic-layer caches that depend on editor-buffer overlays. Held on
/// [`crate::server::State`] alongside the [`Workspace`] so handlers can
/// thread the docs map through. Mutation is single-threaded — same
/// assumption the request loop already makes.
#[derive(Debug, Default)]
pub struct SemanticState {
    /// Per-project compile-ordered parses, keyed by canonicalised project
    /// path. Invalidated by [`Self::invalidate_project`] on text-sync and by
    /// [`Self::invalidate_all`] when `workspace/didChangeWatchedFiles` reports
    /// a structural (`.fsproj` / assets / import) change.
    project_parses: HashMap<PathBuf, ProjectParses>,
    /// Per-**file** parse cache — the parsed [`ImplFile`]s for each Compile item,
    /// keyed by path. Lets [`build_parses`] re-parse only the file that changed on
    /// a single-file edit and reuse every other file's tree (a rowan handle
    /// clone), so a keystroke no longer re-parses the whole Compile order.
    /// Distinct from `project_parses`, which caches the assembled per-project
    /// [`ProjectParses`] and is dropped in full on any text-sync; this survives,
    /// keyed per file.
    ///
    /// A path maps to a small list of [`CachedParse`] **variants** — one per
    /// distinct parse-input set (`symbols` / `lang` / `lang_version_untrusted`).
    /// Normally one; a source file linked into two projects with different
    /// settings keeps a variant each, so alternating between the projects never
    /// re-parses the shared file (the single-entry-per-path design would thrash,
    /// each build evicting the other's entry). A fresh parse for an *existing*
    /// settings-tuple — a new text after an edit — replaces that tuple's variant,
    /// so repeated edits of a file don't grow its list.
    ///
    /// Cleared by [`Self::invalidate_all`] on a structural change purely to bound
    /// memory — correctness never rests on eviction, since the input comparison
    /// in `build_parses` re-parses anything whose inputs no longer match.
    file_parses: HashMap<PathBuf, Vec<CachedParse>>,
    /// Per-project flattened name index over referenced-assembly entities,
    /// keyed by the **full value-input set** `build_assembly_env` reads: the
    /// canonicalised project path *and* the canonicalised `dotnet_root` it was
    /// resolved against (`None` when no root was supplied). Built by reading
    /// the parsed `<ProjectReference>` graph (the edges) plus
    /// `project.assets.json` (the artifacts), and feeding each resolved DLL
    /// through `Ecma335Assembly::parse`. Wrapped in [`Arc`] so handlers can
    /// return the env without keeping `SemanticState` borrowed across longer
    /// queries.
    ///
    /// The `dotnet_root` belongs in the key because the env is a function of
    /// it (framework-pack resolution): a first lookup made before the root is
    /// known caches an empty env, and keying on the project alone would hand
    /// that stale empty env to every later lookup with the real root —
    /// silently disabling assembly resolution for the rest of the run.
    ///
    /// **Never invalidated by text-sync**: the env reads only disk state (the
    /// `.fsproj` closure's `<ProjectReference>` edges, `project.assets.json`,
    /// package/framework DLLs, and — since 3.3b — the sidecar's C#-metadata
    /// DLLs) that editor edits can't change. Disk-side
    /// changes arrive via `workspace/didChangeWatchedFiles` instead: a
    /// structural change (`.fsproj` / assets / `global.json`) clears it through
    /// [`Self::invalidate_all`], and a referenced-assembly input change (a
    /// `.dll` rewritten by a sibling rebuild or restore, a `.cs`/`.csproj`
    /// edit feeding the sidecar) through [`Self::invalidate_assembly_state`]
    /// (the sidecar cache is content-addressed, so the rebuild after
    /// invalidation is cheap). The one exception is a *transient* sidecar
    /// transport failure, which [`Self::assembly_env_for_project`]
    /// deliberately does not cache so the next request retries.
    ///
    /// Keyed by `(project, dotnet_root, served-TFM verdict)`. The verdict
    /// selects which assets target the env was built from (fsproj 3.3c plan
    /// E3; 3.3d round 19 for the tri-state — `NoneDeclared` and `Untrusted`
    /// build different envs, so the full [`ServedTfm`] is the key, not its
    /// `Option` projection); it is a function of the project today
    /// (first-declared — [`Workspace::served_tfm_for_project`]), so the
    /// extra key component is structural preparation for a TFM-override
    /// policy and makes "different TFM ⇒ different env" machine-checkable.
    assembly_envs: HashMap<(PathBuf, Option<PathBuf>, ServedTfm), Arc<AssemblyEnv>>,
    /// Per-project resolved sema output: the Compile-order fold of
    /// [`borzoi_sema::resolve_project`] over the project's [`ProjectParses`] against its
    /// [`AssemblyEnv`]. Wrapped in [`Arc`] so handlers can return it without
    /// holding `SemanticState` borrowed across a long query. Invalidated
    /// alongside the parses on every text-sync notification — the resolution
    /// is a function of the parses and the env, so a parses change forces a
    /// re-fold.
    ///
    /// Stored **paired with the exact [`AssemblyEnv`] the fold resolved
    /// against**. `Resolution::Entity` / `Resolution::Member` handles index into
    /// that env, and a re-fetched env can differ (a recovered C# sidecar DLL can
    /// sort earlier, shifting handles), so a consumer classifying those handles
    /// must use *this* env — see [`Self::resolved_project_and_env_for`].
    resolved_projects: HashMap<PathBuf, (Arc<ResolvedProject>, Arc<AssemblyEnv>)>,
    /// Per-project *previous* fold, kept for incremental re-resolution (stage 2).
    /// Unlike `resolved_projects` — which every text-sync drops — this **survives**
    /// text-sync, so after a keystroke [`Self::resolved_project_for`] can fold the
    /// project incrementally: reuse the per-file [`borzoi_sema::ResolvedFile`]s of
    /// files whose parse tree is unchanged (identity-shared through the stage-1
    /// per-file parse cache) and whose threaded prefix is unchanged, re-resolving
    /// only what the edit touched. See [`PrevFold`] for the assembly-env
    /// [`Arc::ptr_eq`] precondition that keeps the reuse sound.
    ///
    /// Cleared wholesale by [`Self::invalidate_all`] / [`Self::invalidate_assembly_state`]
    /// (a structural or referenced-assembly change): after those the env is a new
    /// Arc anyway, so a stale entry could never be reused — clearing is for memory,
    /// not correctness. `PrevFold::resolved` shares the same `Arc` as the matching
    /// `resolved_projects` entry, so the extra state is one `Vec<ImplFile>` of
    /// rowan handles per project.
    prev_resolved: HashMap<PathBuf, PrevFold>,
    /// Running tally of folds that took the *incremental* path (a reusable
    /// [`PrevFold`] whose env matched). A silently-cold fold — a fold that
    /// should reuse but doesn't — is a *performance* regression, not a
    /// correctness one, so no result comparison would catch it; the tests read
    /// this to assert the incremental path actually fires after an edit. Also a
    /// natural telemetry hook. Monotone; never reset by invalidation.
    incremental_folds: usize,
    /// How many per-file [`borzoi_sema::ResolvedFile`]s the *most recent* fold
    /// reused verbatim (0 for a cold fold — it reuses nothing). Where
    /// [`Self::incremental_folds`] only says the incremental path was *taken*,
    /// this says how much it actually saved — a body edit to an early file should
    /// reuse all but the touched file(s). Read by the tests to assert reuse
    /// genuinely happened, and a natural profiling signal. Tracked identically in
    /// both builds (the `otel` labelled fold returns the same reuse vector), so a
    /// test's assertion holds regardless of feature flags.
    last_fold_reused_files: usize,
    /// A `workspace/semanticTokens/refresh` is owed to the client: an invalidation
    /// (a text-sync edit, or a watched structural / referenced-assembly change)
    /// may have staled an already-open buffer's tokens, but the client only
    /// re-requests the buffer it touched. Set by **every invalidator** — at the
    /// invalidation, not at a later fold, so an invalidation with no following
    /// fold (a `didClose` restoring disk text, a watched-file change, a project
    /// that now evaluates partially) still refreshes. Drained by
    /// [`Self::take_wants_refresh`] in the dispatch loop after each request *and*
    /// notification (it owns the connection and checks `refreshSupport`). A plain
    /// bool: the refresh is workspace-wide and idempotent, so coalescing several
    /// invalidations into one refresh is correct — and it can't loop, since the
    /// refresh's own re-requests are ordinary requests, not invalidations.
    wants_refresh: bool,
    /// The portable-PDB *metadata image* (embedded, or a validated sidecar) for
    /// each referenced DLL go-to-definition has navigated into, keyed by DLL
    /// path. `None` caches the negative result (a DLL with no usable PDB) so a
    /// repeat lookup doesn't re-read it. Wrapped in [`Arc`] so a cache hit is a
    /// refcount bump, not a copy of the (multi-hundred-KB) image.
    ///
    /// Without this, every go-to-definition into a referenced member re-reads
    /// the whole owning DLL (FSharp.Core is ~2.4 MB) and re-extracts its PDB.
    /// Like `assembly_envs`, the image is a function of on-disk artifacts the
    /// editor can't change, so it is never invalidated by text-sync — only by
    /// [`Self::invalidate_all`] / [`Self::invalidate_assembly_state`] on a
    /// watched structural or referenced-assembly change.
    pdb_images: HashMap<PathBuf, Option<Arc<[u8]>>>,
    /// On-disk cache of each referenced DLL's projected entities, so a warm
    /// server restart skips the parse+project that dominates a cold env build.
    /// **Disabled by default** (so tests and un-opted consumers stay off-disk);
    /// the server enables it from the environment via
    /// [`Self::set_assembly_cache`]. Consulted by `build_env_from_dll_paths`;
    /// orthogonal to the in-memory `assembly_envs` memoisation (which it feeds
    /// on the first, cold build).
    assembly_cache: AssemblyCache,
    /// The single reused C# sidecar process, lazily spawned the first time a
    /// project's assembly env needs a `.csproj` reference's metadata (see
    /// `build_assembly_env`). Left un-spawned for the common F#-only workspace;
    /// degrades to under-resolution when the sidecar is unavailable (D5). Not a
    /// cache to invalidate — the sidecar re-derives its own content-addressed
    /// metadata per `buildMetadata`, so it survives text-sync and
    /// `didChangeWatchedFiles`; only dropping `SemanticState` tears it down.
    sidecar: SidecarManager,
    /// Whether an assets-absent project may be resolved by an **on-demand
    /// `dotnet restore`** ([`crate::restore`]). Off by default — restore executes
    /// the project's MSBuild targets (arbitrary code), so it is a *workspace
    /// trust* decision the host opts into explicitly (the server reads it from
    /// the `enableOnDemandRestore` initialization option). When off, an
    /// assets-absent project degrades to today's empty env (single-file editing)
    /// exactly as before.
    on_demand_restore_enabled: bool,
}

impl SemanticState {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many folds have taken the incremental path so far (the
    /// `incremental_folds` tally). A telemetry / test hook: after an edit and
    /// a re-fold this rises, confirming the reuse machinery is engaged rather
    /// than silently falling back to a cold fold.
    pub fn incremental_fold_count(&self) -> usize {
        self.incremental_folds
    }

    /// How many files the most recent fold reused verbatim (see
    /// [`Self::last_fold_reused_files`]). Zero for a cold fold; for an
    /// incremental fold after a localized edit it should be nearly the whole
    /// project. Not tracked under the `otel` feature.
    pub fn last_fold_reused_files(&self) -> usize {
        self.last_fold_reused_files
    }

    /// Take (and clear) the pending `workspace/semanticTokens/refresh` flag — a
    /// fold since the last check found an edit that changed cross-file state, so
    /// the client should re-request tokens for its open buffers. Called by the
    /// dispatch loop after each request; the caller still gates on the client
    /// advertising `refreshSupport`.
    pub fn take_wants_refresh(&mut self) -> bool {
        std::mem::take(&mut self.wants_refresh)
    }

    /// Install the on-disk assembly-projection cache (the server's opt-in, from
    /// [`AssemblyCache::from_env`]). Left [`AssemblyCache::disabled`] by
    /// [`Self::new`], so only the real server touches disk.
    pub fn set_assembly_cache(&mut self, cache: AssemblyCache) {
        self.assembly_cache = cache;
    }

    /// Opt into on-demand `dotnet restore` for assets-absent projects (the
    /// `enableOnDemandRestore` initialization option). Off by [`Self::new`], so
    /// tests, library embeddings, and un-opted hosts never execute a project's
    /// restore targets; the assets-absent path then degrades to an empty env.
    pub fn set_on_demand_restore_enabled(&mut self, enabled: bool) {
        self.on_demand_restore_enabled = enabled;
    }

    /// Drop this project's compile-order parses and its resolved-project
    /// output. Called from every text-sync notification for any URI owned by
    /// the project, since v1 invalidation is whole-project — sema's
    /// prefix-monotone fold makes sub-project re-resolution a future
    /// optimisation.
    ///
    /// Does **not** invalidate the assembly env: editor edits can't change
    /// `project.assets.json` or referenced DLLs. Disk-side changes to those
    /// arrive through `workspace/didChangeWatchedFiles`
    /// ([`Self::invalidate_all`] / [`Self::invalidate_assembly_state`]).
    pub fn invalidate_project(&mut self, project: &Path) {
        let key = canonicalise(project);
        self.project_parses.remove(&key);
        self.resolved_projects.remove(&key);
        // This edit may have staled an open later buffer's tokens; the client
        // only re-requests the buffer it touched, so owe a workspace refresh.
        self.wants_refresh = true;
    }

    /// Drop **all** semantic caches — parses, resolved projects, *and* assembly
    /// envs. Used by `workspace/didChangeWatchedFiles` when a project-structure
    /// file changes on disk. Unlike [`Self::invalidate_project`] /
    /// [`Self::invalidate_file`] this also clears `assembly_envs`: a `.fsproj`
    /// or `project.assets.json` edit can change the referenced-assembly set,
    /// which editor text-sync never can.
    pub fn invalidate_all(&mut self) {
        self.project_parses.clear();
        self.file_parses.clear();
        self.resolved_projects.clear();
        self.prev_resolved.clear();
        self.assembly_envs.clear();
        self.pdb_images.clear();
        // A structural change can stale open buffers' tokens; owe a workspace
        // refresh (the next drain sends it, even with no following fold).
        self.wants_refresh = true;
    }

    /// Drop every cache derived from **referenced-assembly bytes** — the
    /// assembly envs, the resolved projects folded against them, and the
    /// portable-PDB images — while keeping `project_parses` (defines and
    /// Compile order are functions of project evaluation, not of binaries).
    /// `workspace/didChangeWatchedFiles` calls this for the referenced-assembly
    /// input class: a `.dll` rewritten by a sibling rebuild or a restore, or a
    /// `.cs`/`.csproj` edit that changes what the C# sidecar would emit. The
    /// next request re-resolves against the new binaries instead of serving
    /// the stale env for the rest of the server's life. Broad, not targeted
    /// (file-watch plan W2: broad-but-correct; everything rebuilds lazily).
    /// The on-disk [`AssemblyCache`] needs no invalidation — its entries are
    /// `(size, mtime)`-validated per DLL.
    pub fn invalidate_assembly_state(&mut self) {
        self.resolved_projects.clear();
        self.prev_resolved.clear();
        self.assembly_envs.clear();
        self.pdb_images.clear();
        // A referenced-assembly change can stale open buffers' cross-assembly
        // tokens; owe a workspace refresh (sent on the next drain).
        self.wants_refresh = true;
    }

    /// The portable-PDB image for the referenced DLL at `dll`, cached for the
    /// server lifetime (the `pdb_images` map). On a miss it runs `compute`
    /// (the IO + embedded-or-sidecar selection the handler owns) and caches the
    /// result — including a `None`, so a DLL with no usable PDB isn't re-read on
    /// every go-to-definition. `compute` is injected so this cache stays unaware
    /// of the PDB-sourcing logic (which lives in the definition handler).
    pub fn pdb_image(
        &mut self,
        dll: &Path,
        compute: impl FnOnce() -> Option<Arc<[u8]>>,
    ) -> Option<Arc<[u8]>> {
        if let Some(cached) = self.pdb_images.get(dll) {
            return cached.clone();
        }
        let image = compute();
        self.pdb_images.insert(dll.to_path_buf(), image.clone());
        image
    }

    /// Drop the caches for **every** cached project that lists `file` in its
    /// compile-order parses — not just the one `Workspace::owning_project`
    /// would single out. A shared source file can sit in multiple projects'
    /// `<Compile>` lists (the link case `fsproj-consumption-plan.md`
    /// flagged as out-of-scope for ownership); a text-sync against that
    /// file invalidates the buffer overlay for every project that sees it,
    /// so a stale fold in a sibling project doesn't surface in a later
    /// definition / references query.
    ///
    /// Walks `project_parses` rather than `Workspace`: we only need to
    /// invalidate projects we already have semantic state for, and
    /// inspecting the live cache avoids re-evaluating projects from disk.
    pub fn invalidate_file(&mut self, file: &Path) {
        let target = lexically_normalize(file);
        let to_drop: Vec<PathBuf> = self
            .project_parses
            .iter()
            .filter(|(_, parses)| {
                parses
                    .paths
                    .iter()
                    .any(|p| paths_equal(&lexically_normalize(p), &target))
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in to_drop {
            self.project_parses.remove(&key);
            self.resolved_projects.remove(&key);
        }
        // This edit may have staled an open later buffer's tokens; the client
        // only re-requests the buffer it touched, so owe a workspace refresh.
        self.wants_refresh = true;
    }

    /// The flattened `AssemblyEnv` for the project at `project` — a name
    /// index over every type defined by the project's resolved reference
    /// DLLs, each parsed through [`Ecma335Assembly::parse`].
    ///
    /// Two disk sources compose (project-graph plan E1/E2), each answering a
    /// different question:
    /// - **Edges** (which projects this one references): the parsed
    ///   `<ProjectReference>` graph, [`Workspace::project_graph`] — the
    ///   editor-current intent, authoritative even when it disagrees with a
    ///   stale restore.
    /// - **Artifacts** (the DLLs and producer TFMs backing those edges plus
    ///   the package/framework set): `obj/project.assets.json` via the LSP's
    ///   existing [`crate::project_assets::resolve_assemblies_root_only`].
    ///   The root-only resolver is deliberate: assets contribute only
    ///   artifacts here, and the transitive walker would error on a sibling
    ///   whose own `obj/project.assets.json` is missing — discarding the root
    ///   project's package and framework DLLs along the way. Reading just the
    ///   root keeps cross-assembly resolution alive under partial restores.
    ///
    /// **Degradation rules** (D5: under-resolve, never wrong, never panic):
    /// - No `project.assets.json` (un-restored project) → empty env, cached.
    /// - `resolve_assemblies_root_only` fails (malformed assets, missing
    ///   framework pack) → empty env, cached.
    /// - A specific DLL fails to read, parse, or enumerate — or panics the
    ///   reader mid-parse — → skipped with a logged warning; the env still
    ///   contains every *other* DLL's types. One bad reference (an unsupported
    ///   signature, an incompatible F# pickle version, a corrupt package-cache
    ///   file) never discards the rest. Host-pickle decode gaps inside an
    ///   otherwise readable F# DLL are a narrower degradation: the assembly
    ///   view records skipped F# overlays and still returns the ECMA-derived
    ///   types.
    /// - **F# graph edges**: each referenced `.fsproj`'s built output DLL
    ///   (`bin/<config>/<tfm>/<TargetName>.dll`) is folded in when present,
    ///   so cross-project resolution works once the sibling is built — even
    ///   before a restore records the edge. A missing/unbuilt output is
    ///   skipped (under-resolve). An edge present only in stale assets (the
    ///   `<ProjectReference>` was removed) is **not** folded: that would be
    ///   fabrication, not degradation.
    /// - **C# graph edges (`.csproj`)** are consulted via the sidecar: it
    ///   emits a Roslyn metadata DLL per referenced project (and its transitive
    ///   C# closure), which are folded in. Degrades to under-resolution when the
    ///   sidecar is unavailable, a build fails (see [`SidecarManager`]), or the
    ///   edge is too new for any restore to have recorded a producer TFM.
    ///
    /// Cached for the server lifetime (no `didChangeWatchedFiles` yet);
    /// returns an [`Arc`] so callers can hold the env beyond the borrow on
    /// `self`. `workspace` is only read on a cache miss (to build the graph).
    pub fn assembly_env_for_project(
        &mut self,
        project: &Path,
        dotnet_root: Option<&Path>,
        target_framework: &ServedTfm,
        workspace: &Workspace,
    ) -> Arc<AssemblyEnv> {
        // Both this `&Workspace` entry point and `resolved_project_for` drive the
        // on-demand restore (gated on the trust opt-in): unlike the old in-house
        // resolve — which needed `&mut` to read the cached evaluation — restore
        // works from the project path and the environment alone, so there's no
        // asymmetry to work around; both pass the same restore environment and
        // cache the same result.
        self.assembly_env_for_project_retryable(
            project,
            dotnet_root,
            target_framework,
            workspace,
            self.restore_env(workspace),
        )
        .0
    }

    /// The restore environment to hand the assets-absent path: `Some` only when
    /// on-demand restore is opted into ([`Self::set_on_demand_restore_enabled`]);
    /// `None` otherwise, so the path declines to an empty env without executing
    /// the project's restore targets.
    fn restore_env<'a>(&self, workspace: &'a Workspace) -> Option<&'a SdkDiscoveryEnv> {
        self.on_demand_restore_enabled.then(|| workspace.env())
    }

    /// The reference-DLL set [`Self::assembly_env_for_project`] would build its
    /// env from: package + framework DLLs from the assets file, F#
    /// `<ProjectReference>` output DLLs, and C# `<ProjectReference>` sidecar
    /// metadata DLLs, in that order. Same degradation rules as the env build
    /// (D5: anything unresolvable is absent, never fabricated).
    ///
    /// This is the *observability* surface for the composed set: the
    /// `dotnet build` reference-set differential oracle
    /// (`reference_set_msbuild_diff.rs`) diffs it against MSBuild's
    /// `ReferencePath`, and it is the natural "dump my references" debugging
    /// hook. Uncached — each call re-resolves (rebuilding the project graph,
    /// and possibly driving the sidecar), so it observes what a fresh env
    /// build would see rather than a cached one.
    pub fn reference_dlls_for_project(
        &mut self,
        project: &Path,
        dotnet_root: Option<&Path>,
        target_framework: &ServedTfm,
        workspace: &Workspace,
    ) -> Vec<PathBuf> {
        let recovered_ref_tfms = env_ref_tfms(project, target_framework.as_deref());
        let graph = workspace.project_graph_with_producer_tfms(project, &recovered_ref_tfms);
        let ref_targets = graph_ref_targets(&graph, project);
        // Observability / differential surface: it diffs against MSBuild's
        // `ReferencePath` on *restored* projects, so it reads the assets file
        // like the assets path does and never drives the on-demand restore
        // (`None` restore environment).
        resolve_reference_dlls(
            project,
            dotnet_root,
            target_framework,
            &mut self.sidecar,
            &ref_targets,
            &recovered_ref_tfms,
            None,
        )
        .0
    }

    /// [`Self::assembly_env_for_project`] plus a *retryable* flag: `true` when a
    /// transient C# sidecar failure left the env incomplete and un-cached, so a
    /// caller that builds its own derived cache from this env (e.g.
    /// [`Self::resolved_project_for`]) must skip caching too and let the next
    /// request retry. A cache *hit* is always `false` — only stable envs are
    /// ever cached.
    fn assembly_env_for_project_retryable(
        &mut self,
        project: &Path,
        dotnet_root: Option<&Path>,
        target_framework: &ServedTfm,
        workspace: &Workspace,
        restore_env: Option<&SdkDiscoveryEnv>,
    ) -> (Arc<AssemblyEnv>, bool) {
        // Key on the three *value* inputs `build_assembly_env` reads. Without
        // the root in the key, a lookup made before the SDK root is known
        // (`None`, or a wrong root) would cache an empty env under the project
        // path and hand it back to every later call with the real root —
        // permanently disabling assembly resolution for this project (D5:
        // under-resolve, never wrong). The TFM verdict selects the assets
        // target (plan E3) — the full [`ServedTfm`], not its `Option`
        // projection, because `NoneDeclared` and `Untrusted` build different
        // envs and must not share an entry. The graph-sourced reference
        // edges are *not* in the key: like
        // every other env input they are read fresh from disk on a miss, and a
        // `.fsproj` edit reaches this cache the same way a restore does —
        // through [`Self::invalidate_all`].
        let key = (
            canonicalise(project),
            dotnet_root.map(canonicalise),
            target_framework.clone(),
        );
        if let Some(env) = self.assembly_envs.get(&key) {
            return (Arc::clone(env), false);
        }
        // The reference EDGES come from the parsed `<ProjectReference>` graph
        // (plan E1), evaluated fresh off-cache so they reflect current disk —
        // never from `project.assets.json`, which may lag the fsproj in both
        // directions. Producer-TFM recovery (fsproj 3.3c Phase 2b rooted at
        // the entry) runs first because it feeds the graph walk too: each
        // node is evaluated under the TFM NuGet selected for it, so a
        // `$(TargetFramework)`-gated `<ProjectReference>` in a multi-targeted
        // dependency contributes the edges the real build would. Only done on
        // a miss: both re-read the closure from disk, which would be wasteful
        // per lookup.
        let recovered_ref_tfms = env_ref_tfms(project, target_framework.as_deref());
        let graph = workspace.project_graph_with_producer_tfms(project, &recovered_ref_tfms);
        let ref_targets = graph_ref_targets(&graph, project);
        // Borrow `assembly_cache` (shared) and `sidecar` (mut) as disjoint
        // fields — the sidecar drives any C# `.csproj` references this project
        // has, lazily spawning on first need.
        let (env, retryable) = build_assembly_env(
            project,
            dotnet_root,
            target_framework,
            &self.assembly_cache,
            &mut self.sidecar,
            &ref_targets,
            &recovered_ref_tfms,
            restore_env,
        );
        let env = Arc::new(env);
        // A *transient* C# sidecar transport failure (`retryable`) is the only
        // reason not to cache: the handle was dropped for respawn, so the next
        // request should re-attempt the C# metadata rather than be served this
        // degraded env forever. Every *stable* result — an assets-file env, a
        // successful on-demand restore, or a stable degradation (no restore
        // environment, a cold-cache restore decline, a genuine build error) —
        // caches, so we don't rebuild (or re-restore) on every call. A later
        // manual `dotnet restore` reaches this cache through
        // [`Self::invalidate_assembly_state`] when its `project.assets.json`
        // change is observed.
        if retryable {
            tracing::info!(
                project = %project.display(),
                "not caching assembly env after a transient C# sidecar failure; will retry on next request"
            );
        } else {
            self.assembly_envs.insert(key, Arc::clone(&env));
        }
        (env, retryable)
    }

    /// The [`ResolvedProject`] for `project` **paired with the exact
    /// [`AssemblyEnv`] the fold resolved against**: sema's Compile-order fold
    /// over the project's [`ProjectParses`] against that env. This is the top of
    /// the semantic stack — go-to-definition, find-references, hover, and the
    /// semantic-token classifier all read from a `ResolvedProject`.
    ///
    /// A consumer that inspects a `Resolution::Entity` / `Resolution::Member`
    /// — whose handles index into an `AssemblyEnv` — **must** use the env
    /// returned here, not one re-fetched from [`Self::assembly_env_for_project`]:
    /// on a transient C# sidecar failure the fold runs against an un-cached env
    /// *and* the resolved project is left un-cached (the next request retries),
    /// so a rebuilt env can order its DLLs differently and shift the handles the
    /// resolution already recorded — a wrong class, or an out-of-range panic.
    ///
    /// Returns `None` only when [`Self::parses_for_project`] does, i.e. when
    /// the project failed to evaluate or evaluated partially. Callers that
    /// want a useful answer for orphan / partial-project files should fall
    /// back to running [`borzoi_sema::resolve_file`] on the single
    /// buffer at hand — see the handler-level discipline in Stage 5+.
    ///
    /// `dotnet_root` is taken from [`Workspace::dotnet_root_for_project`] and
    /// passed through to [`Self::assembly_env_for_project`]; it's `None` when
    /// SDK discovery turns nothing up (most commonly in tests).
    ///
    /// Cached (project + env) keyed on the canonicalised project path.
    /// Invalidated by [`Self::invalidate_project`] on any text-sync. Folds the
    /// whole project; for a single-file request that only needs a prefix, prefer
    /// [`Self::resolved_prefix_and_env_for`].
    pub fn resolved_project_and_env_for(
        &mut self,
        project: &Path,
        workspace: &mut Workspace,
        docs: &HashMap<Url, String>,
    ) -> Option<(Arc<ResolvedProject>, Arc<AssemblyEnv>)> {
        // `usize::MAX` clamps to the project size, i.e. the whole project.
        self.resolved_prefix_and_env_for(project, usize::MAX, workspace, docs)
    }

    /// Like [`Self::resolved_project_and_env_for`], but folds only the Compile
    /// **prefix** up to and including `up_to_index` — the returned
    /// [`ResolvedProject`] covers `.file(up_to_index)` (`len() > up_to_index`)
    /// but may be shorter than the whole project.
    ///
    /// Sound because F# is order-sensitive: `.file(k)`'s resolution depends only
    /// on files `0..=k`, so a single-file request (semantic tokens, hover,
    /// definition of a *use*) never needs the suffix. This spares the suffix fold
    /// — most of the project when the edited file is early in the Compile order.
    /// A request that genuinely needs every file (find-references) passes
    /// `usize::MAX` via [`Self::resolved_project_and_env_for`].
    ///
    /// `resolved_projects[key]` holds the **deepest prefix folded since the last
    /// edit**; it is shared with the full method and grows to the deepest
    /// requester. A request within the cached prefix is a hit; a deeper one
    /// extends it (incrementally, reusing what's there).
    pub fn resolved_prefix_and_env_for(
        &mut self,
        project: &Path,
        up_to_index: usize,
        workspace: &mut Workspace,
        docs: &HashMap<Url, String>,
    ) -> Option<(Arc<ResolvedProject>, Arc<AssemblyEnv>)> {
        let key = canonicalise(project);

        // Fast path — **before any `Workspace`/SDK probing**. `dotnet_root_for_project`
        // can spawn `dotnet --info` under a long deadline (asdf/mise/Nix-style
        // `dotnet` wrappers with no recorded SDK root), so a repeat request that
        // already has its answer must not pay that. Size the request against the
        // *already-cached* parses and return a cached prefix that covers it — both
        // are plain map lookups. It was folded against the current env (a rebuilt
        // env drops the entry via `invalidate_assembly_state`), so no env re-check
        // is needed. If the parses aren't cached, neither is the fold (they are
        // built and dropped together), so this correctly falls through.
        if let Some(cached_parses) = self.project_parses.get(&key) {
            let want_len = up_to_index.saturating_add(1).min(cached_parses.files.len());
            if let Some((resolved, env)) = self.resolved_projects.get(&key)
                && resolved.len() >= want_len
            {
                return Some((Arc::clone(resolved), Arc::clone(env)));
            }
        }

        // Slow path: (re-)evaluate parses + env and fold the prefix.
        //
        // Use `dotnet_root_for_project` (not `env().dotnet_root`): the latter is
        // the raw `$DOTNET_ROOT` env var; the former runs the proper SDK
        // discovery — `$PATH` fallback, `global.json` `sdk.paths` overrides —
        // so a workspace with no `$DOTNET_ROOT` but `dotnet` on `$PATH` still
        // gets framework-pack DLLs into the assembly env.
        let dotnet_root = workspace.dotnet_root_for_project(project);
        let dotnet_root = dotnet_root.as_deref();
        // The served-TFM verdict, read **once** and fed to both resolution
        // inputs (fsproj 3.3c, plan E5): the parses are already coherent by
        // construction (the evaluation itself was seeded with this value),
        // and the env selects the same TFM's assets target. Sourcing both
        // from this single read is what enforces the coherence invariant —
        // a project parsed under net8.0's defines must never resolve against
        // net10.0's assemblies.
        let target_framework = workspace.served_tfm_for_project(project);

        // Parses — None if the Compile set is untrustworthy or it failed to
        // evaluate. Cloning a `ProjectParses` is cheap (the `ImplFile`s are rowan
        // handles, `texts` is `Arc<str>` per file). The file count clamps the slice.
        let parses = self.parses_for_project(project, workspace, docs)?.clone();
        // Resolve `0..want_len`; `up_to_index` is a valid Compile index (or
        // `usize::MAX` for "the whole project"), so `+1` (saturating) then clamp.
        let want_len = up_to_index.saturating_add(1).min(parses.files.len());
        // Assembly env — always returns *some* env (empty if anything is
        // missing). Caches a separate slot. `retryable` is set when a
        // transient C# sidecar failure left the env incomplete and un-cached.
        // Passes the restore environment (when the trust opt-in is on) so an
        // assets-absent project is resolved by an on-demand `dotnet restore`
        // (see [`crate::restore`]); the `&Workspace` entry point does the
        // same, so they agree.
        let restore_env = self.restore_env(workspace);
        let (env, retryable) = self.assembly_env_for_project_retryable(
            project,
            dotnet_root,
            &target_framework,
            workspace,
            restore_env,
        );
        // The Compile prefix to fold: `.file(up_to_index)` and everything it can
        // reference (files `0..up_to_index`), nothing after.
        let files = &parses.files[..want_len];
        // 3. Fold — incrementally when a previous fold of this project is
        //    reusable, cold otherwise — and cache unless the env was a
        //    transient-failure degradation: caching the resolved project then
        //    would pin the missing C# types past the sidecar respawn (the env
        //    cache being skipped alone doesn't help, since hover/definition read
        //    from `resolved_projects`).
        // Set inside the fold block; read after (once the immutable `self`
        // borrow the reuse check holds has been released) to bump the counter.
        let took_incremental;
        let (resolved, reused_files): (Arc<ResolvedProject>, usize) = {
            // Reuse the previous fold only when it ran against the *same* env
            // `Arc` — the incremental fold keeps files' resolutions verbatim, so
            // they must have been resolved against this exact env (a rebuilt env,
            // from invalidation or a transient-sidecar recovery, is a fresh Arc,
            // so the pointer check fails and we fold cold). See [`PrevFold`].
            let reusable_prev = self
                .prev_resolved
                .get(&key)
                .filter(|prev| Arc::ptr_eq(&prev.env, &env));
            took_incremental = reusable_prev.is_some();
            let _span = tracing::info_span!(
                "resolve_project",
                files = want_len,
                incremental = took_incremental,
            )
            .entered();
            match reusable_prev {
                // Incremental: reuse per-file results the edit can't have changed,
                // re-resolving only what it touched. Returns exactly what a cold
                // fold would (asserted by sema's `incremental ≡ batch` differential).
                // Under `otel`, the labelled variant keeps per-Compile-item
                // attribution for the files the edit forced to re-resolve (and the
                // per-file spans carry the reuse, so no aggregate count is tracked
                // there); otherwise we take the reuse-reporting variant.
                Some(prev) => {
                    #[cfg(feature = "otel")]
                    let (resolved, reused) = {
                        let labels: Vec<String> = parses.paths[..want_len]
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect();
                        borzoi_sema::resolve_project_incremental_labeled(
                            &prev.files,
                            prev.resolved.as_ref(),
                            files,
                            &labels,
                            &env,
                        )
                    };
                    #[cfg(not(feature = "otel"))]
                    let (resolved, reused) = borzoi_sema::resolve_project_incremental_with_reuse(
                        &prev.files,
                        prev.resolved.as_ref(),
                        files,
                        &env,
                    );
                    (Arc::new(resolved), reused.iter().filter(|&&r| r).count())
                }
                // Cold fold. Under `otel`, use the path-labelled variant so each
                // `resolve_file` span is attributable to its Compile item; the
                // label vector is built only in that build (the default path
                // allocates nothing). A cold fold reuses nothing.
                None => {
                    #[cfg(feature = "otel")]
                    let resolved = {
                        let labels: Vec<String> = parses.paths[..want_len]
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect();
                        borzoi_sema::resolve_project_labeled(files, &labels, &env)
                    };
                    #[cfg(not(feature = "otel"))]
                    let resolved = borzoi_sema::resolve_project(files, &env);
                    (Arc::new(resolved), 0)
                }
            }
        };
        if took_incremental {
            self.incremental_folds += 1;
        }
        self.last_fold_reused_files = reused_files;
        if retryable {
            tracing::info!(
                project = %project.display(),
                "not caching resolved project after a transient C# sidecar failure; will retry on next request"
            );
            // Don't store `prev_resolved` either: a transient-failure env is a
            // fresh Arc every call, so it could never satisfy the `ptr_eq` reuse
            // gate anyway — storing it would only pin a degraded fold's memory.
        } else {
            self.resolved_projects
                .insert(key.clone(), (Arc::clone(&resolved), Arc::clone(&env)));
            // The incremental base is the **deepest** same-env fold, not
            // necessarily this one: a shallow prefix fold (a semantic-token
            // request for an early file) must not discard a deeper pre-edit base,
            // or the next full-project request (hover / completion / references)
            // would re-`resolve_file` the whole unchanged suffix instead of
            // reusing it. Keeping a deeper base is sound — its entry for the
            // edited file is stale, but the incremental fold recomputes any file
            // whose tree changed (`same_tree`) and reuses the rest. A different
            // env drops the old base (its handles wouldn't match); a shorter
            // existing base is replaced. Shares the `resolved`/`env` Arcs; the
            // only added state is the folded prefix's parsed files (rowan handles).
            let keep_deeper = self
                .prev_resolved
                .get(&key)
                .is_some_and(|p| p.resolved.len() > want_len && Arc::ptr_eq(&p.env, &env));
            if !keep_deeper {
                self.prev_resolved.insert(
                    key,
                    PrevFold {
                        files: files.to_vec(),
                        resolved: Arc::clone(&resolved),
                        env: Arc::clone(&env),
                    },
                );
            }
        }
        Some((resolved, env))
    }

    /// The [`ResolvedProject`] for `project` — the env-less view of
    /// [`Self::resolved_project_and_env_for`]. Prefer the paired method whenever
    /// you will classify or render a `Resolution::Entity` / `Resolution::Member`
    /// (their handles are only meaningful against the env the fold used).
    pub fn resolved_project_for(
        &mut self,
        project: &Path,
        workspace: &mut Workspace,
        docs: &HashMap<Url, String>,
    ) -> Option<Arc<ResolvedProject>> {
        self.resolved_project_and_env_for(project, workspace, docs)
            .map(|(resolved, _env)| resolved)
    }

    /// Like [`Self::resolved_project_for`], but folds only the Compile **prefix**
    /// up to and including `up_to_index` — the env-less view of
    /// [`Self::resolved_prefix_and_env_for`], whose soundness argument it shares.
    /// The returned [`ResolvedProject`] covers `.file(up_to_index)`. Prefer this
    /// for a single-file request whose answer depends only on `0..=up_to_index`
    /// (definition / completion of a *use*): the referenced def, or any in-scope
    /// completion candidate, is declared at an index `<= up_to_index`, so the
    /// suffix fold is pure waste.
    pub fn resolved_prefix_for(
        &mut self,
        project: &Path,
        up_to_index: usize,
        workspace: &mut Workspace,
        docs: &HashMap<Url, String>,
    ) -> Option<Arc<ResolvedProject>> {
        self.resolved_prefix_and_env_for(project, up_to_index, workspace, docs)
            .map(|(resolved, _env)| resolved)
    }

    /// The number of Compile-order files in the cached resolved fold for
    /// `project`, if one is cached — the *depth* of the retained prefix. A sliced
    /// single-file request for file `k` caches only `k + 1`; a full request
    /// (find-references) caches the whole project. Lets a test pin that a handler
    /// folded only the prefix, not the whole project — the point of the slice.
    pub fn cached_resolved_len(&self, project: &Path) -> Option<usize> {
        self.resolved_projects
            .get(&canonicalise(project))
            .map(|(resolved, _env)| resolved.len())
    }

    /// The Compile-ordered parses for the project at `project`, preferring
    /// the in-memory buffer of each member file over its on-disk text.
    /// Returns `None` when the project failed to evaluate or evaluated only
    /// partially — sema can't fold over a Compile list we don't fully trust
    /// (matches [`Workspace::owning_project`]'s `Membership::Unknown`
    /// discipline).
    ///
    /// `docs` is the live buffer map ([`crate::server::State::docs`]); a
    /// missing entry falls through to `std::fs::read_to_string`. A file we
    /// can read neither from the buffer nor from disk is skipped — never an
    /// error, never a panic.
    pub fn parses_for_project<'a>(
        &'a mut self,
        project: &Path,
        workspace: &mut Workspace,
        docs: &HashMap<Url, String>,
    ) -> Option<&'a ProjectParses> {
        let key = canonicalise(project);
        if !self.project_parses.contains_key(&key) {
            let parses = build_parses(project, workspace, docs, &mut self.file_parses)?;
            self.project_parses.insert(key.clone(), parses);
        }
        self.project_parses.get(&key)
    }
}

/// Build the compile-ordered parses for a fresh cache miss. Pure (modulo
/// disk reads for files not in `docs` and the workspace's own caching).
fn build_parses(
    project: &Path,
    workspace: &mut Workspace,
    docs: &HashMap<Url, String>,
    file_parses: &mut HashMap<PathBuf, Vec<CachedParse>>,
) -> Option<ProjectParses> {
    let _span = tracing::info_span!("build_parses", project = %project.display()).entered();
    let symbols = workspace.symbols_for_project(project);
    let lang = workspace.lang_version_for_project(project);
    let parsed = workspace.project(project)?;
    // Gate on `items_uncertain`, *not* `is_partial`: we only need the Compile
    // *order* to be trustworthy to fold over it. `is_partial` flips for any
    // divergence from MSBuild — including the undefined properties and skipped
    // `<Target>`s every real SDK project's imported targets emit, none of which
    // change the Compile set — so gating on it would refuse essentially every
    // real project. `items_uncertain` is the narrow signal: a Compile item or
    // its group couldn't be included/excluded faithfully (see
    // `Workspace::membership`'s `Membership::Unknown` arm, gated the same way).
    // When it's set we refuse and let handlers fall back to single-file
    // resolution rather than feed sema a misleading Compile order.
    //
    // `define_constants_uncertain` is the second axis sema needs: each file is
    // parsed under the project's `$(DefineConstants)` `#if` symbols, so if those
    // are unreliable (a user define gated on an unresolved property — e.g. a
    // multi-targeted `'$(TargetFramework)' == …` condition) we'd fold files
    // under the wrong branches and export the wrong bindings. Refuse there too.
    if parsed.items_uncertain || parsed.define_constants_uncertain {
        return None;
    }
    // `LangVersion` provenance is the third axis, gated per *source shape*
    // below rather than here: the language version shapes the token stream
    // (the lex-filter's strict-indentation push decision at the F# 8
    // boundary), so an unknowable version can mis-shape the fold — but the
    // provenance mark alone can't gate wholesale, because a real SDK's own
    // conditional LangVersion default trips it for EVERY project (probed,
    // dotnet 10.0.301, even with a cleanly body-pinned value —
    // `sdk_project_fold_e2e` pins that plain projects must fold). Instead,
    // each parse reports whether its shape actually depends on the version
    // (`Parse::shape_depends_on_language_version`, whose `false` *proves*
    // version-invariance), and the fold refuses only the intersection:
    // untrusted provenance × a file that genuinely straddles the boundary.
    let lang_version_untrusted = parsed.property_provenance_untrusted("LangVersion");
    // Snapshot the include paths before we hand `workspace` back out for
    // disk reads / overlay lookups — `ParsedProject` is borrowed from the
    // workspace and we can't keep that borrow live across the inner loop's
    // mutable use of `workspace` (there's none today, but this keeps the
    // borrow shape predictable for later stages).
    // Accept every Compile flavour `ParsedProject.items` produces:
    // `[CompileBefore, Compile, CompileAfter]` — F#-specific item kinds the
    // msbuild parser already orders for us. Excluding any of them would
    // silently drop real source from projects like FSharp.Core that depend
    // on `<CompileBefore>` / `<CompileAfter>`. `ProjectReference` is *not*
    // in `items` (it lives on `project_references`); even so, we filter
    // defensively in case the parser's split ever loosens.
    let includes: Vec<PathBuf> = parsed
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                ItemKind::Compile | ItemKind::CompileBefore | ItemKind::CompileAfter
            )
        })
        .map(|item| item.include.clone())
        .collect();

    // Signature files (`.fsi`) hide implementation detail the paired `.fs`
    // exposes, but the CST parser has no signature-file model — every file
    // parses to an `IMPL_FILE`, its sole file-root node.
    // Folding a `.fsi`-bearing project would feed sema the raw `.fs`, so
    // `resolve_project` would export bindings the signature deliberately hides
    // (a later file could resolve `Foo.hidden` that F# rejects). Until
    // signatures are modelled, refuse the whole project and let handlers fall
    // back to single-file resolution — same "under-resolve, never wrong"
    // discipline as the `items_uncertain` arm above (D5).
    if let Some(sig) = includes.iter().find(|p| is_signature_file(p)) {
        tracing::info!(
            project = %project.display(),
            signature = %sig.display(),
            "refusing project parses: F# signature files (.fsi) are not yet modelled"
        );
        return None;
    }

    let mut files = Vec::with_capacity(includes.len());
    let mut paths = Vec::with_capacity(includes.len());
    let mut texts = Vec::with_capacity(includes.len());

    let _parse_all_span =
        tracing::info_span!("parse_compile_items", count = includes.len()).entered();
    for include in includes {
        let _file_span =
            tracing::info_span!("parse_compile_item", file = %include.display()).entered();
        // **Hard fail on any hole.** Sema's fold is order-sensitive: a later
        // file may reference (and shadow) earlier exports. If we silently
        // skipped a Compile item the queried file depended on, its references
        // could bind to a same-named assembly entity behind the missing
        // module — a *wrong* go-to-def, not just under-resolution. Per
        // correctness-over-availability we refuse the project; handlers fall
        // back to single-file `resolve_file` for orphan-style queries.
        let Some(text) = read_text(&include, docs) else {
            tracing::warn!(
                file = %include.display(),
                "Compile item unreadable from buffer and disk; refusing to resolve project"
            );
            return None;
        };
        // Per-file parse cache: reuse a variant whose every deciding input — the
        // source text, the `#if` symbols, the language version, and the project's
        // `LangVersion`-provenance trust — matches. A hit is a rowan handle clone
        // that also *skips* the version-boundary gate below, which is why
        // `lang_version_untrusted` is part of the match (it is that gate's one
        // non-parse input). A miss falls through to parse (and, for a
        // straddle-flagged file, re-run the gate) and records a variant. The
        // input comparison is what keeps a stale variant from being served, so no
        // invalidation hook is load-bearing.
        if let Some(hit) = file_parses.get(&include).and_then(|variants| {
            variants
                .iter()
                .find(|c| {
                    c.lang == lang
                        && c.lang_version_untrusted == lang_version_untrusted
                        && c.symbols == symbols
                        && *c.text == *text
                })
                .map(|c| (c.file.clone(), Arc::clone(&c.text)))
        }) {
            let (impl_file, text) = hit;
            files.push(impl_file);
            paths.push(include);
            texts.push(text);
            continue;
        }
        let Some(parse) = parse_with_symbols(&text, &symbols, lang) else {
            // `parse_with_symbols` returns `None` only when the CST parser
            // panicked. Logged inside the wrapper.
            tracing::warn!(
                file = %include.display(),
                "Compile item triggered parser panic; refusing to resolve project"
            );
            return None;
        };
        // The intersection gate promised above: this file MAY parse to a
        // differently shaped tree at another language version (an offside
        // version-gated push — the F# 8 strict-indentation boundary), and
        // the project's `LangVersion` provenance can't tell us which version
        // the real build uses. The flag is a sound *over*-approximation
        // (an EOF-anchored push — `module M =` at end of file, a common
        // mid-edit state — differs as a stack operation but reconverges via
        // the EOF cascade), and real SDK projects are always
        // provenance-tainted, so acting on the flag alone would flicker the
        // fold off while the user types. Verify: re-parse once from the
        // *other* side of the boundary (strictness is the only shape input,
        // so one representative per side decides) and refuse only genuine
        // divergence — folding then could export bindings from a tree the
        // compiler never sees; handlers fall back to single-file resolution
        // (best-guess version, accepted best-effort). The extra parse runs
        // only for flagged files in untrusted-provenance projects.
        if lang_version_untrusted && parse.shape_depends_on_language_version {
            let other_side = if lang.strict_indentation_is_error() {
                LanguageVersion::V7_0
            } else {
                LanguageVersion::DEFAULT
            };
            let Some(other) = parse_with_symbols(&text, &symbols, other_side) else {
                tracing::warn!(
                    file = %include.display(),
                    "Compile item triggered parser panic; refusing to resolve project"
                );
                return None;
            };
            // Green-node equality is recursively structural (rowan compares
            // header + children, tokens by kind + text — verified against
            // the vendored source), so this is an allocation-free tree
            // comparison; both directions are pinned by the mid-edit
            // (reconverges, folds) and straddling (differs, refuses) tests.
            if other.root.green() != parse.root.green() {
                tracing::info!(
                    project = %project.display(),
                    file = %include.display(),
                    "refusing project parses: LangVersion provenance is untrusted and \
                     this file parses to a different tree across the F# 8 boundary"
                );
                return None;
            }
        }
        let Some(impl_file) = ImplFile::cast(parse.root) else {
            tracing::warn!(
                file = %include.display(),
                "Compile item parsed to a non-IMPL_FILE root; refusing to resolve project"
            );
            return None;
        };
        // Cache miss resolved: record the freshly parsed tree as this path's
        // variant for the current settings-tuple. Replace the same-tuple variant
        // if present (a new text after an edit — no growth on repeated edits),
        // else append (a linked file under another project's settings — kept
        // alongside so neither project's build thrashes the other's).
        let text: Arc<str> = Arc::from(text);
        let variant = CachedParse {
            symbols: symbols.clone(),
            lang,
            lang_version_untrusted,
            text: Arc::clone(&text),
            file: impl_file.clone(),
        };
        let variants = file_parses.entry(include.clone()).or_default();
        if let Some(slot) = variants.iter_mut().find(|c| {
            c.lang == lang
                && c.lang_version_untrusted == lang_version_untrusted
                && c.symbols == symbols
        }) {
            *slot = variant;
        } else {
            variants.push(variant);
        }
        files.push(impl_file);
        paths.push(include);
        texts.push(text);
    }

    Some(ProjectParses {
        files,
        paths,
        texts,
    })
}

/// Build the assembly env for a fresh cache miss. Always returns *some*
/// env — empty if anything went wrong (D5 degrades to under-resolution).
///
/// The `bool` is *retryable*: `true` when a C# sidecar **transport** failure
/// left the env incomplete but a retry (after respawn) might succeed, so the
/// caller should not cache the result. Every other degradation is stable
/// (`false`) — an un-restored project, a genuine build error, an unbuilt F#
/// ref — and is safe to cache.
#[allow(clippy::too_many_arguments)]
fn build_assembly_env(
    project: &Path,
    dotnet_root: Option<&Path>,
    target_framework: &ServedTfm,
    cache: &AssemblyCache,
    sidecar: &mut SidecarManager,
    ref_targets: &GraphRefTargets,
    recovered_ref_tfms: &BTreeMap<PathBuf, String>,
    restore_env: Option<&SdkDiscoveryEnv>,
) -> (AssemblyEnv, bool) {
    let _span = tracing::info_span!("build_assembly_env", project = %project.display()).entered();
    let (dlls, retryable) = resolve_reference_dlls(
        project,
        dotnet_root,
        target_framework,
        sidecar,
        ref_targets,
        recovered_ref_tfms,
        restore_env,
    );
    (
        build_env_from_dll_paths(dlls.iter().map(PathBuf::as_path), cache),
        retryable,
    )
}

/// The project-reference targets the env fold consumes, derived from the
/// **parsed** `<ProjectReference>` graph (plan E1: the parsed edge set is
/// authoritative; `project.assets.json` is a post-restore artifact that can
/// lag the fsproj in both directions and never supplies edges).
#[derive(Debug, Default, PartialEq, Eq)]
struct GraphRefTargets {
    /// Every `.fsproj` node in the entry's transitive F# closure, excluding
    /// the entry itself — each contributes its built output DLL, located
    /// under the walk's per-node TFM verdict ([`NodeTfm`]).
    fsharp: Vec<FsharpRefTarget>,
    /// Every `.csproj` boundary node (a direct C# edge of some F# node in the
    /// closure). The sidecar expands each one's own transitive subtree, so the
    /// boundary set is exactly the set to drive it with — never the interior.
    csharp: Vec<PathBuf>,
}

/// One F# node of the closure, as the env fold consumes it: the
/// canonicalised project path, the walk's TFM verdict, and the node's
/// evaluated output-assembly name
/// ([`crate::project_graph::ProjectNode::output_name`] — the fallback when
/// the entry's assets file doesn't cover the ref).
#[derive(Debug, PartialEq, Eq)]
struct FsharpRefTarget {
    path: PathBuf,
    tfm: NodeTfm,
    output_name: Option<String>,
}

/// Project [`graph`](Workspace::project_graph) nodes → [`GraphRefTargets`],
/// in the graph's deterministic discovery order.
///
/// Paths are canonicalised (falling back to the graph's lexically-normalised
/// path when the target doesn't resolve on disk) to match the keys of the
/// assets-derived producer-TFM maps, which canonicalise theirs.
///
/// A project reachable only *through* a `.csproj` is invisible here by
/// construction — the graph never recurses into C#. Its C# subtree is the
/// sidecar's domain; an **F#** project behind a C# boundary is a known
/// under-resolution (the sidecar emits only C# metadata; D5). Graph
/// *problems* (cycles, missing targets, unsupported kinds) are the
/// diagnostics layer's business, not the fold's: a problematic target is
/// simply not a node, so nothing here is fabricated from it.
fn graph_ref_targets(graph: &ProjectGraph, entry: &Path) -> GraphRefTargets {
    let entry_key = lexically_normalize(entry);
    let mut targets = GraphRefTargets::default();
    for node in &graph.nodes {
        match node.kind {
            ProjectKind::FSharp => {
                if !paths_equal(&node.path, &entry_key) {
                    targets.fsharp.push(FsharpRefTarget {
                        path: canonicalise(&node.path),
                        tfm: node.tfm.clone(),
                        output_name: node.output_name.clone(),
                    });
                }
            }
            ProjectKind::CSharp => targets.csharp.push(canonicalise(&node.path)),
            // Never a node (an unsupported-kind edge is a GraphProblem), but
            // stay exhaustive so a builder change can't silently fold one.
            ProjectKind::Other => {}
        }
    }
    targets
}

/// The composed reference-DLL set for `project`: the assets file's package +
/// framework DLLs, the F# `<ProjectReference>` output DLLs, and the C#
/// `<ProjectReference>` sidecar metadata DLLs, in that order. This is
/// **exactly** the list [`build_assembly_env`] folds into the env — split out
/// so the reference-set differential oracle (and any debugging surface) can
/// observe the set itself rather than the parsed env, whose per-DLL
/// degradations would hide composition bugs.
///
/// `ref_targets` carries the project-reference **edges**, sourced from the
/// parsed graph ([`graph_ref_targets`]); the assets file contributes only
/// artifacts (package/framework DLLs and producer TFMs).
/// `recovered_ref_tfms` is [`entry_ref_tfms`]'s per-producer map (fsproj
/// 3.3c Phase 2b), computed by the caller because it also seeds the graph
/// walk that produced `ref_targets`; empty (no chosen TFM / partial restore)
/// degrades each ref path to its pre-3.3c behaviour.
///
/// Degradations follow [`build_assembly_env`]'s rules (D5): no root / no
/// assets / a failed resolve → empty. The `bool` is the *retryable* flag
/// ([`build_assembly_env`] documents it).
fn resolve_reference_dlls(
    project: &Path,
    dotnet_root: Option<&Path>,
    target_framework: &ServedTfm,
    sidecar: &mut SidecarManager,
    ref_targets: &GraphRefTargets,
    recovered_ref_tfms: &BTreeMap<PathBuf, String>,
    restore_env: Option<&SdkDiscoveryEnv>,
) -> (Vec<PathBuf>, bool) {
    // An untrusted TFM verdict serves nothing (3.3d round 19). This is NOT
    // the `NoneDeclared` fallback below: with no declared TFM the restore is
    // the only evidence and its sole target is sound, but here the evidence
    // *conflicts* — an evaluated TFM we declined to serve, and a restore that
    // may lag it — so a sole target could be an unrelated TFM's assemblies
    // exactly when the declined guess was right (D5: under-resolve, never
    // wrong). Even the graph-sourced F#/C# ref DLLs decline with it: their
    // per-producer verdicts were recovered relative to this entry, and an
    // env with references but no package/framework set would resolve against
    // a skewed world anyway.
    if matches!(target_framework, ServedTfm::Untrusted) {
        tracing::info!(
            project = %project.display(),
            "entry TFM provenance is untrusted; assembly env defaults to empty"
        );
        return (Vec::new(), false);
    }
    let Some(dotnet_root) = dotnet_root else {
        tracing::info!(
            project = %project.display(),
            "no dotnet_root; assembly env defaults to empty"
        );
        return (Vec::new(), false);
    };
    let assets_path = match project.parent() {
        Some(dir) => dir.join("obj").join("project.assets.json"),
        None => {
            tracing::info!(
                project = %project.display(),
                "project path has no parent; assembly env defaults to empty"
            );
            return (Vec::new(), false);
        }
    };
    // Assets file present → read restore output (the authoritative path).
    // Absent → run an on-demand `dotnet restore` to a scratch dir and read that
    // (see [`crate::restore`]): the real restore is exact by construction, and
    // reading its (scratch) assets yields the same [`ResolvedAssemblies`] the
    // present-file branch does, so cross-project F#/C# edges resolve in both
    // cases. A cold cache / timeout / no restore environment declines to today's
    // empty env.
    let resolved = if assets_path.is_file() {
        // Root-only enumeration: the assets file supplies only *artifacts* here
        // (package/framework DLLs and per-producer TFMs — the edges are
        // `ref_targets`'), and chasing the transitive closure would error on a
        // sibling project that hasn't been restored yet, dropping the root
        // project's package / framework DLLs along with it. Reading just the root
        // file gives us exactly the DLLs we want and degrades gracefully under
        // partial restores. A chosen TFM selects the matching assets target
        // (multi-TFM restores have several — plan E3); with none declared, fall
        // back to requiring a single-target restore. A missing target for the
        // chosen TFM errors into the empty-env degradation below rather than
        // serving a different TFM's assemblies (plan E6). The untrusted verdict
        // already returned empty above.
        let resolve_result = {
            let _span = tracing::info_span!("resolve_assemblies_root_only").entered();
            // `Untrusted` returned empty above, so `as_deref` is `None` exactly
            // for `NoneDeclared` here.
            match target_framework.as_deref() {
                Some(tfm) => resolve_assemblies_for_tfm(&assets_path, dotnet_root, tfm),
                None => resolve_assemblies_root_only(&assets_path, dotnet_root),
            }
        };
        match resolve_result {
            Ok(r) => r,
            Err(err) => {
                tracing::warn!(
                    project = %project.display(),
                    assets = %assets_path.display(),
                    error = %err,
                    "resolve_assemblies_root_only failed; assembly env defaults to empty"
                );
                return (Vec::new(), false);
            }
        }
    } else {
        // Residual (under-resolution): `ref_targets` was built from the graph
        // walk *before* this restore, and with no assets file the walk leaves a
        // multi-target F# reference's producer TFM unresolved. The restore's
        // `project_ref_tfms` here would pin it, but it is not fed back into the
        // walk, so such a reference's output DLL may be omitted. Single-target
        // refs resolve from the node's own TFM; and this is never *worse* than
        // the assets-present path degrades under a partial restore.
        match restore_to_scratch_assemblies(project, dotnet_root, target_framework, restore_env) {
            RestoreOutcome::Resolved(resolved) => resolved,
            RestoreOutcome::Declined => {
                tracing::info!(
                    project = %project.display(),
                    assets = %assets_path.display(),
                    "no project.assets.json and the on-demand restore declined; \
                     assembly env defaults to empty"
                );
                return (Vec::new(), false);
            }
            RestoreOutcome::TransientFailure => {
                // Don't cache: a spawn error / timeout / wedge may clear, and the
                // scratch dir is gone with no watched file to trigger a refresh,
                // so `retryable` is the only way a later request tries again.
                tracing::info!(
                    project = %project.display(),
                    "on-demand restore failed transiently; not caching, will retry"
                );
                return (Vec::new(), true);
            }
        }
    };
    // F# `<ProjectReference>` graph targets: fold each referenced project's
    // built output DLL (`bin/<config>/<tfm>/<TargetName>.dll`) into the same
    // env, so cross-project go-to-definition / references resolve into a
    // sibling F# project's types. A reference whose output isn't built is
    // silently skipped (D5: under-resolve, never wrong) — the same graceful
    // degradation the un-restored and unparseable-DLL paths already use.
    let ref_dlls =
        fsharp_project_ref_dlls(&ref_targets.fsharp, &resolved.project_ref_assembly_names);
    if !ref_dlls.is_empty() {
        tracing::info!(
            project = %project.display(),
            count = ref_dlls.len(),
            "including F# project-reference output DLLs in the assembly env"
        );
    }
    // C# `<ProjectReference>` graph targets: the sidecar emits a Roslyn
    // metadata DLL for each (plus its transitive C# closure); fold those in
    // too. Degrades to nothing when the sidecar is unavailable or a build
    // fails (D5); a transport failure additionally marks the env non-cacheable
    // so the next request retries against a respawned sidecar rather than
    // serving this incomplete one.
    let (csharp_ref_dlls, retryable) = csharp_project_ref_dlls(
        sidecar,
        project,
        dotnet_root,
        recovered_ref_tfms,
        &ref_targets.csharp,
        &resolved.project_ref_tfms,
    );
    if !csharp_ref_dlls.is_empty() {
        tracing::info!(
            project = %project.display(),
            count = csharp_ref_dlls.len(),
            "including C# project-reference metadata DLLs in the assembly env"
        );
    }
    let dlls: Vec<PathBuf> = resolved
        .package_dlls
        .into_iter()
        .chain(resolved.framework_dlls)
        .chain(ref_dlls)
        .chain(csharp_ref_dlls)
        .collect();
    (dlls, retryable)
}

/// The C# metadata DLLs for the graph's `.csproj` boundary targets
/// (`csproj_refs`), via the sidecar. For each we drive one `buildMetadata`
/// (the sidecar expands that ref's transitive C# closure itself), then dedup
/// the resulting paths — a project reached through two direct refs is
/// content-addressed to the same DLL. Returns empty (never errors) when there
/// are no C# refs, the sidecar is unavailable, or a build fails (D5).
///
/// The `bool` is *retryable*: `true` if any ref hit a sidecar transport failure
/// (so the caller should not cache the resulting env — see
/// [`SidecarManager::metadata_dlls_for_csproj`]).
///
/// The sidecar publishes under `<workspace_root>/obj/borzoi/…`; we use
/// the entry project's directory. A ref with no known producer TFM — added to
/// the fsproj but never yet restored, or a canonicalisation gap — is skipped
/// rather than guessed (under-resolve until the next restore).
///
/// **Producer-TFM recovery (fsproj 3.3c, plan E3).** NuGet records each
/// project ref's `framework` on the consumer's target entry in *base* form —
/// the platform suffix is absent (`net8.0`, never `net8.0-windows`; see
/// [`crate::project_assets::enumerate::Reference::ProjectRef`]). `recovered`
/// is [`entry_ref_tfms`]'s per-producer map (Phase 2b rooted at the entry);
/// a ref it doesn't cover falls back to `base_tfms` — the entry assets'
/// base-form record (exact in the common no-suffix case).
fn csharp_project_ref_dlls(
    sidecar: &mut SidecarManager,
    project: &Path,
    dotnet_root: &Path,
    recovered: &BTreeMap<PathBuf, String>,
    csproj_refs: &[PathBuf],
    base_tfms: &BTreeMap<PathBuf, String>,
) -> (Vec<PathBuf>, bool) {
    if csproj_refs.is_empty() {
        return (Vec::new(), false);
    }
    let Some(workspace_root) = project.parent() else {
        return (Vec::new(), false);
    };
    let dotnet_exe = dotnet_exe_for(dotnet_root);

    let mut dlls: Vec<PathBuf> = Vec::new();
    let mut retryable = false;
    for csproj in csproj_refs {
        let Some(tfm) = recovered.get(csproj).or_else(|| base_tfms.get(csproj)) else {
            tracing::warn!(
                csproj = %csproj.display(),
                "no producer TFM known for C# project reference (not yet restored?); skipping"
            );
            continue;
        };
        let tfm = tfm.as_str();
        // Closure-wide TFM map for per-node workspace construction. Best-effort:
        // a partial restore (the ref's own `project.assets.json` missing) makes
        // this error; the sidecar accepts an empty map and falls back to `tfm`.
        let project_tfms = resolve_transitive_project_tfms(csproj, tfm).unwrap_or_default();
        let meta = sidecar.metadata_dlls_for_csproj(
            &dotnet_exe,
            dotnet_root,
            workspace_root,
            csproj,
            crate::BUILD_CONFIGURATION,
            tfm,
            &project_tfms,
        );
        retryable |= meta.retryable;
        dlls.extend(meta.dlls);
    }
    dlls.sort();
    dlls.dedup();
    (dlls, retryable)
}

/// Phase 2b rooted at the **entry** project (fsproj 3.3c, plan E3): the
/// entry's own `project.assets.json` records every closure node (NuGet
/// flattens `<ProjectReference>` transitively) with its base `framework`
/// field, and `pick_producer_tfm` recovers each producer's platform-qualified
/// TFM from the producer's declared list. Keys are canonicalised to match
/// [`ResolvedAssemblies::project_refs`] / `project_ref_tfms`.
///
/// Best-effort: empty when the entry's TFM is unknown (no `chosen_tfm`) or
/// the resolve fails (partial restore, stale assets) — the caller falls back
/// to the base TFM per ref, so this is never worse than the status quo (D5).
///
/// Why not `resolve_transitive_project_tfms(csproj, entry_tfm)`: the second
/// argument is looked up in the *first* argument's own assets targets, which
/// are keyed by the csproj's TFMs — the entry's TFM is not generally among
/// them. The consumer whose assets can name the direct C# ref is the entry.
/// The producer-TFM map the env fold seeds its graph walk with and backs its
/// per-ref TFM lookups from: [`entry_ref_tfms`]'s recovery, plus the entry
/// itself under its chosen TFM. The entry's TFM is known **by construction**
/// (it is the same value that seeded the parses — coherence plan E5), so it
/// must never fall to the walk's TFM-invariant-edges fallback: a recovery
/// that fails outright (a partial restore missing one producer's assets
/// file) would otherwise demote a multi-targeted entry and drop its own
/// `$(TargetFramework)`-gated `<ProjectReference>`s. A *successful* recovery
/// already contains the entry (the transitive map includes its root); this
/// makes the seed unconditional *given a TFM at all*: an entry whose TFM
/// provenance is untrusted arrives here as `None`
/// ([`ServedTfm::as_deref`] on the [`Workspace::served_tfm_for_project`]
/// verdict, 3.3d round 19) and seeds nothing — consistent with the walk,
/// whose untrusted-TFM demotion fires before any seed is consulted (and
/// moot for the env fold, whose untrusted arm serves no DLLs at all).
fn env_ref_tfms(entry_project: &Path, entry_tfm: Option<&str>) -> BTreeMap<PathBuf, String> {
    let mut map = entry_ref_tfms(entry_project, entry_tfm);
    if let Some(tfm) = entry_tfm {
        map.insert(canonicalise(entry_project), tfm.to_string());
    }
    map
}

fn entry_ref_tfms(entry_project: &Path, entry_tfm: Option<&str>) -> BTreeMap<PathBuf, String> {
    let Some(tfm) = entry_tfm else {
        return BTreeMap::new();
    };
    match resolve_transitive_project_tfms(entry_project, tfm) {
        Ok(map) => map
            .into_iter()
            .map(|(path, tfm)| (std::fs::canonicalize(&path).unwrap_or(path), tfm))
            .collect(),
        Err(err) => {
            tracing::info!(
                project = %entry_project.display(),
                tfm,
                error = %err,
                "entry-rooted producer-TFM recovery unavailable; C# refs fall back to base TFMs"
            );
            BTreeMap::new()
        }
    }
}

/// The `dotnet` executable to launch the sidecar with: prefer the one inside
/// the resolved SDK root, else fall back to `dotnet` on `PATH`. (`start_sidecar`
/// spawns `dotnet <sidecar.dll>`.)
fn dotnet_exe_for(dotnet_root: &Path) -> PathBuf {
    let exe = if cfg!(windows) {
        "dotnet.exe"
    } else {
        "dotnet"
    };
    let candidate = dotnet_root.join(exe);
    if candidate.is_file() {
        candidate
    } else {
        PathBuf::from(exe)
    }
}

/// Locate the on-disk output DLLs for the **F#** project references in
/// `project_refs` (the graph's F# closure — [`GraphRefTargets::fsharp`]).
/// A `.csproj` or other reference kind that slips in is ignored — C#
/// references are the sidecar's domain. A reference whose output isn't built
/// is silently skipped (D5: under-resolve, never wrong). The result is
/// deduplicated and deterministically ordered so the env (and every
/// `AssemblyId → path` mapping it induces) is stable across builds.
///
/// Each ref carries the graph walk's per-node TFM verdict (fsproj 3.3c/3.3d,
/// plan E5): a [`NodeTfm::Known`] ref must resolve to *that* TFM's output —
/// never another variant's — so the env stays coherent with the entry's
/// chosen TFM; a [`NodeTfm::Unresolved`] ref (a multi-declaring producer no
/// restore has pinned) is skipped outright — even a *single* built variant
/// may be a stale build of a TFM the real build wouldn't select — and so is
/// a [`NodeTfm::NotEvaluated`] one (for an F# node that means evaluation
/// *failed*, so nothing connects an on-disk output to the current source).
///
/// Each ref's output name is, in order: the graph node's own evaluated
/// name ([`FsharpRefTarget::output_name`] — the trusted `$(TargetName)`,
/// which is what MSBuild actually writes to `bin/`), else the entry assets
/// file's recorded producer name
/// ([`ResolvedAssemblies::project_ref_assembly_names`]). The assets name
/// is only a fallback because it records the **AssemblyName**, not the
/// file name: probed (dotnet 10.0.301, 2026-07-10), a `TargetName`-renamed
/// producer's assets say `bin/placeholder/<AssemblyName>.dll` while the
/// file on disk is `<TargetName>.dll` — the two coincide except under an
/// explicit `TargetName` override, exactly the case the graph name gets
/// right. A ref with neither is skipped outright rather than guessed by
/// project-file stem: a renamed producer may leave a stale stem-named DLL
/// on disk, and folding it would fabricate (D5: under-resolve, never wrong).
fn fsharp_project_ref_dlls(
    project_refs: &[FsharpRefTarget],
    assembly_names: &BTreeMap<PathBuf, String>,
) -> Vec<PathBuf> {
    let mut dlls: Vec<PathBuf> = project_refs
        .iter()
        .filter(|t| is_fsharp_project(&t.path))
        .filter_map(|t| {
            let Some(output_name) = t
                .output_name
                .as_deref()
                .or_else(|| assembly_names.get(t.path.as_path()).map(String::as_str))
            else {
                tracing::info!(
                    fsproj = %t.path.display(),
                    "project reference without a trustworthy output-assembly name; skipping its output"
                );
                return None;
            };
            match &t.tfm {
                NodeTfm::Known(tfm) => {
                    locate_fsharp_output_dll(&t.path, Some(tfm), output_name)
                }
                NodeTfm::NoneDeclared => locate_fsharp_output_dll(&t.path, None, output_name),
                NodeTfm::Unresolved | NodeTfm::NotEvaluated => {
                    tracing::info!(
                        fsproj = %t.path.display(),
                        verdict = ?t.tfm,
                        "project reference without a trustworthy TFM verdict; skipping its output"
                    );
                    None
                }
            }
        })
        .collect();
    dlls.sort();
    dlls.dedup();
    dlls
}

/// Given a referenced F# project file, find its built output DLL under
/// `<project_dir>/bin/<config>/<tfm>/<output_name>.dll`. Returns `None`
/// when the project hasn't been built (no matching DLL on disk).
///
/// `output_name` is the producer's resolved output name — the caller
/// recovers it from the entry's assets file or the graph node's own
/// evaluation ([`fsharp_project_ref_dlls`] documents the precedence) and
/// never guesses: a producer whose real output name doesn't match simply
/// isn't located, degrading to under-resolution rather than pulling in an
/// unrelated DLL.
///
/// When `producer_tfm` is known (fsproj 3.3c, plan E5/E6), only that TFM's
/// output qualifies — the consumer's parse is evaluated under the entry TFM
/// NuGet paired with this producer TFM, so serving another variant could
/// resolve against types the selected target doesn't have. An unbuilt
/// selected TFM skips the ref (under-resolve, never cross-resolve), even
/// when other variants exist.
///
/// Without one, outputs under **several distinct TFM directories** are
/// ambiguous — picking one could serve a variant NuGet's restore wouldn't
/// select (an unrestored multi-TFM sibling is the live case) — so the ref is
/// skipped (under-resolve, never wrong). Outputs under a *single* TFM pick
/// deterministically across configs — a `Debug` config first (the editing
/// default), then the lexicographically-smallest path — so a warm rebuild
/// picks the same DLL and the env stays byte-stable.
fn locate_fsharp_output_dll(
    fsproj: &Path,
    producer_tfm: Option<&str>,
    output_name: &str,
) -> Option<PathBuf> {
    let mut dll_name = std::ffi::OsString::from(output_name);
    dll_name.push(".dll");
    let bin = fsproj.parent()?.join("bin");

    // `bin/<config>/<tfm>/<stem>.dll` — collect every built variant on disk.
    // TFM directory names are compared case-insensitively, matching MSBuild's
    // case-insensitive property comparison on the path segment it writes.
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut tfm_dirs: Vec<String> = Vec::new();
    for config in child_dirs(&bin) {
        for tfm_dir in child_dirs(&config) {
            let Some(dir_name) = tfm_dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some(tfm) = producer_tfm
                && !dir_name.eq_ignore_ascii_case(tfm)
            {
                continue;
            }
            let dll = tfm_dir.join(&dll_name);
            if dll.is_file() {
                candidates.push(dll);
                tfm_dirs.push(dir_name.to_ascii_lowercase());
            }
        }
    }
    tfm_dirs.sort();
    tfm_dirs.dedup();
    if producer_tfm.is_none() && tfm_dirs.len() > 1 {
        tracing::info!(
            fsproj = %fsproj.display(),
            tfms = ?tfm_dirs,
            "several TFM variants built and no producer TFM known; skipping the ref rather than guessing"
        );
        return None;
    }
    candidates.sort();
    candidates
        .iter()
        .find(|p| path_has_debug_config(p))
        .cloned()
        .or_else(|| candidates.into_iter().next())
}

/// The immediate subdirectories of `dir`, sorted for determinism. Empty when
/// `dir` is absent or unreadable (an unbuilt project has no `bin/`).
fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// True when any path component is the served build-configuration directory
/// ([`crate::BUILD_CONFIGURATION`], case-insensitive, matching MSBuild's
/// case-insensitive `$(Configuration)`).
fn path_has_debug_config(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.eq_ignore_ascii_case(crate::BUILD_CONFIGURATION))
    })
}

/// True for an F# project file (`.fsproj`), case-insensitively.
fn is_fsharp_project(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("fsproj"))
}

/// Fold the resolved reference DLLs into an [`AssemblyEnv`] **per DLL**: each
/// is read, parsed, and enumerated independently, and a DLL that fails (or
/// panics the reader) is skipped while the rest are retained. This is the D5
/// "under-resolve, never wrong, never crash" discipline at the granularity the
/// env needs — enumerating the whole slice in one fallible call would let a
/// single unsupported DLL discard the types of every other valid reference.
///
/// The per-DLL read+parse+enumerate is CPU-bound and independent, and a project
/// references the whole BCL (~150–170 DLLs) — measured ~85–150 ms serially, the
/// bulk of a cold project resolve. So it is fanned out across the available
/// cores with a work-stealing cursor (the big DLLs, e.g. `System.Private.CoreLib`,
/// don't stall one fixed chunk). **Order is preserved**: results carry their
/// original index and are re-sorted before [`AssemblyEnv::from_assemblies`],
/// which assigns each assembly an id by position — so the env (and thus every
/// go-to-definition `AssemblyId → path` mapping) is byte-identical to the serial
/// build, just faster.
///
/// Builds via [`AssemblyEnv::from_assemblies`] so each entity keeps the path of
/// the DLL it came from: go-to-definition into a referenced member reads that
/// DLL's portable PDB for the source location (see `handlers::definition`).
fn build_env_from_dll_paths<'a>(
    dlls: impl Iterator<Item = &'a Path>,
    cache: &AssemblyCache,
) -> AssemblyEnv {
    let paths: Vec<&Path> = dlls.collect();
    let _span = tracing::info_span!("build_env_from_dll_paths", count = paths.len()).entered();
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(paths.len().max(1));

    // Single DLL (or a platform reporting one core): no threading overhead.
    let mut indexed: Vec<(usize, PathBuf, ReferencedAssemblyProjection)> = if workers <= 1 {
        paths
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let _span =
                    tracing::info_span!("enumerate_dll_type_defs", dll = %p.display()).entered();
                enumerate_dll_type_defs_cached(cache, p).map(|t| (i, p.to_path_buf(), t))
            })
            .collect()
    } else {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let next = &next;
        let paths = &paths;
        // Propagate the parent span into each worker thread so its per-DLL
        // spans nest under `build_env_from_dll_paths` in the trace instead of
        // showing up as unparented roots — `std::thread::scope` doesn't
        // inherit `tracing`'s thread-local current-span context on its own.
        let parent = tracing::Span::current();
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    let parent = parent.clone();
                    s.spawn(move || {
                        let _entered = parent.enter();
                        let mut local = Vec::new();
                        loop {
                            // Work-stealing: each worker claims the next index, so
                            // a large DLL doesn't stall a fixed chunk.
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(&path) = paths.get(i) else { break };
                            let _span = tracing::info_span!(
                                "enumerate_dll_type_defs",
                                dll = %path.display()
                            )
                            .entered();
                            if let Some(types) = enumerate_dll_type_defs_cached(cache, path) {
                                local.push((i, path.to_path_buf(), types));
                            }
                        }
                        local
                    })
                })
                .collect();
            // Each per-DLL call is already panic-safe (`catch_reader_panic`), so a
            // worker cannot unwind; `.join().ok()` is belt-and-braces — a worker
            // panic drops only its results, never crashes the request loop (D5).
            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .flatten()
                .collect()
        })
    };

    // Restore input order so `from_assemblies` assigns the same `AssemblyId`s the
    // serial build would (skipped DLLs simply leave gaps, exactly as before).
    indexed.sort_by_key(|(i, ..)| *i);
    // The OV-6 gate's extension surface is **globally** unknowable when a DLL's
    // AutoOpen list could not be read (an unknown auto-open could bring an extension
    // into any namespace) OR a DLL was **skipped entirely** (`indexed` shorter than
    // the input `paths`): either forces the gate to defer wholesale. A **dropped
    // type**, by contrast, is namespace-scoped uncertainty (below).
    let extension_surface_unknowable =
        indexed.len() < paths.len() || indexed.iter().any(|(_, _, p)| p.auto_opens_unreadable);
    // Collect the namespaces of every dropped type across all DLLs.
    let dropped_type_namespaces: Vec<Vec<String>> = indexed
        .iter()
        .flat_map(|(_, _, p)| p.dropped_type_namespaces.iter().cloned())
        .collect();
    let assemblies = indexed
        .into_iter()
        .map(|(_, path, projection)| {
            let visibility = projection.abbreviation_visibility();
            (
                path,
                projection.entities,
                visibility,
                projection.fsharp_extension_index_unknowable,
                projection.fsharp_signature_non_authoritative,
                projection.assembly_auto_opens,
            )
        })
        .collect();
    let mut env = AssemblyEnv::from_assemblies_with_projection_knowability(assemblies);
    if extension_surface_unknowable {
        env.mark_extension_surface_unknowable();
    }
    for namespace in dropped_type_namespaces {
        env.mark_namespace_dropped_type(namespace);
    }
    env
}

/// [`enumerate_dll_type_defs`] through the on-disk cache: a hit returns the
/// stored projection (skipping the read+parse+project); a miss computes it and
/// (best-effort) stores the result for the next warm start. A disabled cache
/// (the default) always misses and never writes, so this is exactly
/// [`enumerate_dll_type_defs`] plus an off-by-default fast path. The cached value
/// is identical to a fresh computation, so the env is byte-identical either way.
/// [`AssemblyCache::get_or_populate`] brackets the read+parse with a `stat` so a
/// DLL overwritten mid-compute is never persisted with mismatched metadata.
fn enumerate_dll_type_defs_cached(
    cache: &AssemblyCache,
    path: &Path,
) -> Option<ReferencedAssemblyProjection> {
    cache.get_or_populate(path, || enumerate_dll_type_defs(path))
}

/// Read, parse, and enumerate the type definitions of one referenced DLL.
/// Returns `None` — with a logged warning — on *any* failure (unreadable file,
/// unparseable PE, an `enumerate_type_defs` error, or a reader panic at either
/// stage) so the caller skips just this DLL and keeps the others. Never
/// propagates an error or a panic into the env build.
fn enumerate_dll_type_defs(path: &Path) -> Option<ReferencedAssemblyProjection> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(dll = %path.display(), error = %err, "failed to read DLL; skipping");
            return None;
        }
    };
    // `parse` drives the owned ECMA-335 reader over arbitrary
    // external bytes; a malformed-enough DLL (corrupt package cache, native
    // image, truncated download) can make it panic on an out-of-range table
    // index rather than return `Err`, so it runs panic-safely — the same
    // policy the CST parser path uses (see [`crate::cst_panic_safe`]).
    match catch_reader_panic(path, "parse", || Ecma335Assembly::parse(&bytes))? {
        Ok(view) => enumerate_view_catching(path, &view),
        Err(err) => {
            tracing::warn!(dll = %path.display(), error = %err, "failed to parse DLL; skipping");
            None
        }
    }
}

/// Enumerate a parsed view's type definitions panic-safely. `None` (logged) on
/// an enumeration error (an unsupported signature, an incompatible F# pickle
/// version, an ECMA/pickle merge contradiction) or a reader panic. Generic over
/// [`EcmaView`] so the per-DLL degradation is unit-testable with fake views
/// that error or panic on demand.
///
/// Whole types the projector dropped (their own shape undecodable) are
/// *reported* here — one warning per dropped type — rather than silently
/// vanishing: a user whose go-to-definition misses a type can find out why
/// from the log. The kept types are still returned; a drop is a per-type
/// degradation, not a DLL failure.
fn enumerate_view_catching<V: EcmaView>(
    path: &Path,
    view: &V,
) -> Option<ReferencedAssemblyProjection> {
    match catch_reader_panic(path, "enumerate", || view.enumerate_type_defs_with_skips())? {
        Ok((types, skipped)) => {
            for skip in &skipped.dropped_types {
                tracing::warn!(
                    dll = %path.display(),
                    r#type = %skip.name,
                    reason = %skip.reason,
                    "dropped an undecodable type from the assembly projection"
                );
            }
            for skip in &skipped.skipped_fsharp_overlays {
                tracing::warn!(
                    dll = %path.display(),
                    resource = %skip.resource_name,
                    overlays = ?skip.overlays,
                    reason = %skip.reason,
                    "skipped an F# signature-pickle overlay"
                );
            }
            // A manifest whose AutoOpen list cannot be read does not skip the DLL,
            // but its implicit-open surface is *unknown*: record that so the OV-6
            // extension gate defers (a trusted-empty list would let it commit an
            // intrinsic overload past an extension this DLL auto-opens).
            let read = catch_reader_panic(path, "assembly_auto_opens", || {
                view.assembly_auto_opens()
            })
            .and_then(|r| match r {
                Ok(paths) => Some(paths),
                Err(err) => {
                    tracing::warn!(
                        dll = %path.display(),
                        error = %err,
                        "failed to read assembly-level AutoOpen attributes; treating implicit opens as unknowable"
                    );
                    None
                }
            });
            let auto_opens_unreadable = read.is_none();
            let assembly_auto_opens = read.unwrap_or_default();
            // The OV-6 gate cannot see an extension on a **dropped** type (it may be
            // a C#-style `[<Extension>]` class), so record each dropped type's
            // enclosing namespace as possibly-extension-bearing (namespace-scoped,
            // so unrelated files still commit).
            let dropped_type_namespaces = skipped
                .dropped_types
                .iter()
                .map(|d| d.enclosing_namespace())
                .collect();
            Some(ReferencedAssemblyProjection {
                entities: types,
                fsharp_abbreviations_unknowable: skipped.fsharp_abbreviations_unknowable,
                assembly_auto_opens,
                auto_opens_unreadable,
                dropped_type_namespaces,
                fsharp_extension_index_unknowable: skipped.fsharp_extension_index_unknowable,
                fsharp_signature_non_authoritative: skipped.fsharp_signature_non_authoritative,
            })
        }
        Err(err) => {
            tracing::warn!(dll = %path.display(), error = %err, "failed to enumerate DLL types; skipping");
            None
        }
    }
}

/// Run one call into the foreign assembly reader panic-safely. A reader panic
/// must degrade to "skip this DLL" (D5: never crash the server), never unwind
/// through the LSP request loop — the same guard [`crate::cst_panic_safe`]
/// puts around the CST parser. Returns `None`, logging `path` and `stage`, on
/// panic.
fn catch_reader_panic<T>(path: &Path, stage: &str, op: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(op)) {
        Ok(value) => Some(value),
        Err(_) => {
            tracing::warn!(dll = %path.display(), stage, "assembly reader panicked; skipping DLL");
            None
        }
    }
}

/// Pick the text for `path`: prefer the editor buffer if open, else read
/// from disk. `None` if neither is available.
///
/// Buffer lookup is by **platform path equality**, not by exact `Url` match:
/// on case-insensitive filesystems (Windows, macOS) the client may open
/// `lib.fs` while the project lists `Lib.fs`, and the buffer must still win
/// over disk. Matches the rule [`crate::workspace::Workspace::owning_project`]
/// already uses, so an open buffer that decides project ownership also
/// supplies the text we fold. O(N) over open `docs`; N is small in practice.
fn read_text(path: &Path, docs: &HashMap<Url, String>) -> Option<String> {
    let target = lexically_normalize(path);
    for (url, text) in docs {
        if let Ok(doc_path) = url.to_file_path()
            && paths_equal(&lexically_normalize(&doc_path), &target)
        {
            return Some(text.clone());
        }
    }
    std::fs::read_to_string(path).ok()
}

/// Cache-key canonicalisation. Falls back to the literal path when
/// `canonicalize` fails (e.g. the file doesn't exist on disk yet); matches
/// the convention `Workspace::project` already uses.
fn canonicalise(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// True for an F# signature file (`.fsi`). The extension match is
/// case-insensitive so a `.FSI` on a case-insensitive filesystem still counts.
fn is_signature_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("fsi"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use borzoi_assembly::{
        Access, AssemblyIdentity, AssemblyProjectionSkips, EntityKind, FSharpResource,
        FsharpOverlayKind, ImportError, SkippedFsharpOverlay, Version,
    };
    use std::fs;
    use tempfile::TempDir;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    /// A minimal SDK-less fsproj that compiles the given files in source
    /// order. Literal includes (no glob), so the parser resolves them
    /// without the glob resolver.
    fn fsproj(includes: &[&str]) -> String {
        let items: String = includes
            .iter()
            .map(|i| format!("                <Compile Include=\"{i}\" />\n"))
            .collect();
        format!(
            r#"<Project>
              <ItemGroup>
{items}              </ItemGroup>
            </Project>"#
        )
    }

    /// A minimal SDK-less fsproj with the given `<ProjectReference>` includes
    /// (relative to the project directory) and no Compile items.
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

    #[test]
    fn compile_order_is_preserved() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs"]));
        write(&tmp.path().join("A.fs"), "let a = 1\n");
        write(&tmp.path().join("B.fs"), "let b = 2\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let parses = sema
            .parses_for_project(&proj, &mut ws, &HashMap::new())
            .expect("project parses");
        assert_eq!(parses.len(), 2);
        assert!(parses.paths[0].ends_with("A.fs"));
        assert!(parses.paths[1].ends_with("B.fs"));
    }

    #[test]
    fn buffer_overrides_disk_text() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        let file = tmp.path().join("Lib.fs");
        write(&proj, &fsproj(&["Lib.fs"]));
        write(&file, "let disk = 1\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();

        // First lookup with no buffer: disk wins.
        {
            let parses = sema
                .parses_for_project(&proj, &mut ws, &HashMap::new())
                .expect("project parses");
            assert!(text_contains(&parses.texts[0], "disk"));
        }

        // Now overlay a buffer and force a re-build (invalidate first).
        sema.invalidate_project(&proj);
        let mut docs = HashMap::new();
        docs.insert(
            Url::from_file_path(&file).unwrap(),
            "let buffer = 1\n".to_string(),
        );
        let parses = sema
            .parses_for_project(&proj, &mut ws, &docs)
            .expect("project parses");
        assert!(text_contains(&parses.texts[0], "buffer"));
        assert!(!text_contains(&parses.texts[0], "disk"));
    }

    #[test]
    fn missing_file_yields_none() {
        // Hard-fail discipline: an unreadable Compile item blanks the whole
        // project. Quietly omitting the file would let a later file's
        // reference to its symbols silently bind to a same-named assembly
        // entity — a wrong go-to-def, not just under-resolution. Handlers
        // fall back to single-file `resolve_file` for the orphan case.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["Missing.fs", "Present.fs"]));
        write(&tmp.path().join("Present.fs"), "let p = 1\n");
        // Missing.fs is intentionally absent.

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        assert!(
            sema.parses_for_project(&proj, &mut ws, &HashMap::new())
                .is_none()
        );
    }

    /// Signature files hide implementation symbols: `A.fsi` may export only
    /// `publicFoo` while `A.fs` also defines `hiddenFoo`. Sema doesn't model
    /// signatures yet, so it would treat both as implementation files and
    /// publish `hiddenFoo` as a cross-file Item — a *wrong*
    /// definition/references/hover answer for a `B.fs` reference to it. We
    /// refuse the project fold instead and let handlers fall back to single-
    /// file resolution.
    #[test]
    fn project_with_fsi_signature_yields_none() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fsi", "A.fs"]));
        write(&tmp.path().join("A.fsi"), "module A\nval publicFoo : int\n");
        write(
            &tmp.path().join("A.fs"),
            "module A\nlet publicFoo = 1\nlet hiddenFoo = 2\n",
        );

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        assert!(
            sema.parses_for_project(&proj, &mut ws, &HashMap::new())
                .is_none(),
            "a project listing a .fsi signature must under-resolve to None \
             until signatures are modelled"
        );
        assert!(
            sema.resolved_project_for(&proj, &mut ws, &HashMap::new())
                .is_none()
        );
    }

    /// Codex finding (Stage 7 re-review): the `.fsi` extension check must
    /// be case-insensitive. On Windows/macOS a project can legitimately list
    /// `A.FSI` (or any other casing) and the filesystem treats it as the
    /// same file. A case-sensitive check would miss it and the fold would
    /// continue, surfacing the impl file's hidden symbols cross-project.
    #[test]
    fn project_with_uppercase_fsi_signature_yields_none() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.FSI", "A.fs"]));
        write(&tmp.path().join("A.FSI"), "module A\nval publicFoo : int\n");
        write(
            &tmp.path().join("A.fs"),
            "module A\nlet publicFoo = 1\nlet hiddenFoo = 2\n",
        );

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        assert!(
            sema.parses_for_project(&proj, &mut ws, &HashMap::new())
                .is_none()
        );
    }

    #[test]
    fn partial_project_yields_none() {
        // An unresolved `<Import>` makes the project partial; we refuse
        // rather than fold over an unreliable Compile order.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(
            &proj,
            r#"<Project>
              <Import Project="Missing.props" />
              <ItemGroup>
                <Compile Include="Lib.fs" />
              </ItemGroup>
            </Project>"#,
        );
        write(&tmp.path().join("Lib.fs"), "let x = 1\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        assert!(
            sema.parses_for_project(&proj, &mut ws, &HashMap::new())
                .is_none()
        );
    }

    #[test]
    fn partial_but_trustworthy_compile_set_still_yields_parses() {
        // A `<Target>` and an undefined property flip `is_partial` — exactly the
        // noise every real SDK project's imported targets emit — but neither
        // touches the Compile set. We must still fold over it rather than fall
        // back to single-file resolution (the bug this gate change fixes).
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(
            &proj,
            r#"<Project>
              <Target Name="Stamp" />
              <PropertyGroup><Foo>$(Undefined)</Foo></PropertyGroup>
              <ItemGroup>
                <Compile Include="Lib.fs" />
              </ItemGroup>
            </Project>"#,
        );
        write(&tmp.path().join("Lib.fs"), "let x = 1\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        {
            let parsed = ws.project(&proj).expect("evaluates");
            assert!(parsed.is_partial, "Target + undefined property diverge");
            assert!(!parsed.items_uncertain, "but the Compile set is intact");
        }
        let parses = sema
            .parses_for_project(&proj, &mut ws, &HashMap::new())
            .expect("trustworthy Compile set must still yield parses");
        assert_eq!(parses.paths.len(), 1);
    }

    #[test]
    fn caches_until_invalidated() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        let file = tmp.path().join("Lib.fs");
        write(&proj, &fsproj(&["Lib.fs"]));
        write(&file, "let first = 1\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();

        {
            let parses = sema
                .parses_for_project(&proj, &mut ws, &HashMap::new())
                .expect("first parses");
            assert!(text_contains(&parses.texts[0], "first"));
        }

        // Edit the on-disk file, but don't invalidate.
        fs::write(&file, "let second = 2\n").unwrap();
        {
            let parses = sema
                .parses_for_project(&proj, &mut ws, &HashMap::new())
                .expect("cached parses");
            assert!(
                text_contains(&parses.texts[0], "first"),
                "expected cached `first`, got {:?}",
                parses.texts[0]
            );
        }

        // After invalidation the on-disk edit shows up.
        sema.invalidate_project(&proj);
        let parses = sema
            .parses_for_project(&proj, &mut ws, &HashMap::new())
            .expect("rebuilt parses");
        assert!(text_contains(&parses.texts[0], "second"));
    }

    #[test]
    fn preprocessor_symbols_from_define_constants_are_applied() {
        // `DefineConstants=DEBUG` should drive `#if DEBUG`'s active branch
        // through `parse_with_symbols`. We assert the chosen branch by
        // inspecting which top-level binding parses out.
        use borzoi_sema::{AssemblyEnv, ProjectItems, resolve_file};

        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        let file = tmp.path().join("Lib.fs");
        write(
            &proj,
            r#"<Project>
              <PropertyGroup><DefineConstants>DEBUG</DefineConstants></PropertyGroup>
              <ItemGroup><Compile Include="Lib.fs" /></ItemGroup>
            </Project>"#,
        );
        write(&file, "#if DEBUG\nlet on = 1\n#else\nlet off = 1\n#endif\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let parses = sema
            .parses_for_project(&proj, &mut ws, &HashMap::new())
            .expect("project parses");
        let resolved = resolve_file(
            &parses.files[0],
            &ProjectItems::default(),
            &AssemblyEnv::default(),
        );
        let names: Vec<_> = resolved
            .exports()
            .iter()
            .map(|e| e.name().to_string())
            .collect();
        assert_eq!(names, vec!["on".to_string()]);
    }

    fn text_contains(text: &Arc<str>, needle: &str) -> bool {
        text.as_ref().contains(needle)
    }

    #[test]
    fn compile_before_and_after_kinds_are_folded() {
        // FSharp.Core-style projects use `<CompileBefore>` (and rarely
        // `<CompileAfter>`); `ParsedProject.items` already orders them
        // [CompileBefore, Compile, CompileAfter] for us. The fold must not
        // filter them out, or downstream resolution silently misses real
        // source files.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(
            &proj,
            r#"<Project>
              <ItemGroup>
                <CompileBefore Include="Pre.fs" />
                <Compile Include="Main.fs" />
                <CompileAfter Include="Post.fs" />
              </ItemGroup>
            </Project>"#,
        );
        write(&tmp.path().join("Pre.fs"), "let pre = 1\n");
        write(&tmp.path().join("Main.fs"), "let main = 1\n");
        write(&tmp.path().join("Post.fs"), "let post = 1\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let parses = sema
            .parses_for_project(&proj, &mut ws, &HashMap::new())
            .expect("project parses");

        assert_eq!(parses.len(), 3, "{:?}", parses.paths);
        let names: Vec<_> = parses
            .paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["Pre.fs", "Main.fs", "Post.fs"]);
    }

    /// On case-insensitive filesystems (macOS / Windows) the editor may open
    /// the file under a different casing than the `.fsproj` lists. The
    /// buffer must still win over disk, mirroring `Workspace::owning_project`'s
    /// same-platform behaviour.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn buffer_overrides_disk_despite_casing() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        let listed = tmp.path().join("Lib.fs");
        write(&proj, &fsproj(&["Lib.fs"]));
        write(&listed, "let disk = 1\n");

        // Open the buffer under a different casing.
        let opened = tmp.path().join("lib.fs");
        let mut docs = HashMap::new();
        docs.insert(
            Url::from_file_path(&opened).unwrap(),
            "let buffer = 1\n".to_string(),
        );

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let parses = sema
            .parses_for_project(&proj, &mut ws, &docs)
            .expect("project parses");
        assert!(
            text_contains(&parses.texts[0], "buffer"),
            "expected buffer text, got {:?}",
            parses.texts[0]
        );
    }

    // ---- assembly_env_for_project ----

    #[test]
    fn assembly_env_no_dotnet_root_returns_empty() {
        // Without a `dotnet_root` the resolver has no way to find framework
        // packs; we degrade to an empty env rather than guess.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&[]));

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(env.is_empty(), "{}", env.len());
    }

    #[test]
    fn assembly_env_no_assets_file_returns_empty() {
        // Un-restored project: `obj/project.assets.json` is absent.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&[]));
        let dotnet_root = tmp.path().join("dotnet");
        std::fs::create_dir_all(&dotnet_root).unwrap();

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &proj,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(env.is_empty(), "{}", env.len());
    }

    #[test]
    fn assembly_env_corrupt_assets_returns_empty() {
        // Assets file present but malformed JSON: `resolve_assemblies` fails,
        // we degrade to empty rather than crash.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&[]));
        let assets = tmp.path().join("obj").join("project.assets.json");
        write(&assets, "{ this is not valid json");
        let dotnet_root = tmp.path().join("dotnet");
        std::fs::create_dir_all(&dotnet_root).unwrap();

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &proj,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(env.is_empty(), "{}", env.len());
    }

    /// A DLL the resolver hands us turns out to be unreadable as a PE — common
    /// when the fixture stubs out empty files (matches
    /// `project_assets_integration::end_to_end_against_fcs_dump_fixture`,
    /// which also `touch`es zero-byte `.dll` placeholders). The per-DLL skip
    /// rule keeps the env construction alive — empty here because there's
    /// nothing else to add.
    ///
    /// Hermetic by construction: the checked-in fixture's `packageFolders`
    /// points at the developer's real `~/.nuget/packages`, so copying it
    /// verbatim would let `resolve_assemblies` hand back *real* package DLLs on
    /// any machine that has them restored — and a future reader that learns to
    /// read those signatures would flip this test red. We rewrite
    /// `packageFolders` to a temp cache and stub every package DLL there as
    /// empty bytes, so the only DLLs in play are zero-byte placeholders this
    /// test owns.
    #[test]
    fn assembly_env_skips_unparseable_dlls() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/project_assets/single_tfm.json");
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&[]));

        // Repoint `packageFolders` at a temp cache so resolution can never
        // reach the developer's real `~/.nuget`. The other fields the resolver
        // reads (targets/libraries/project.frameworks) are left untouched.
        let nuget = tmp.path().join("nuget");
        let raw = fs::read_to_string(&fixture).unwrap();
        let mut doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let mut folders = serde_json::Map::new();
        folders.insert(
            nuget.to_str().unwrap().to_string(),
            serde_json::Value::Object(Default::default()),
        );
        doc["packageFolders"] = serde_json::Value::Object(folders);
        let assets = tmp.path().join("obj").join("project.assets.json");
        write(&assets, &serde_json::to_string(&doc).unwrap());

        // Stub the framework pack the fixture references, plus the package DLL
        // paths it pulls in — empty bytes so `Ecma335Assembly::parse` rejects
        // them. The package paths mirror the fixture's `libraries[..].path` +
        // each `compile` key; a path we miss here just falls to the per-DLL
        // read-failure skip, which keeps the test empty (and hermetic) anyway.
        for rel in [
            "fsharp.compiler.service/43.12.204/lib/netstandard2.0/FSharp.Compiler.Service.dll",
            "fsharp.compiler.service/43.12.204/lib/netstandard2.0/FSharp.DependencyManager.Nuget.dll",
            "fsharp.core/10.1.204/lib/netstandard2.1/FSharp.Core.dll",
            "fsharp.systemtextjson/1.4.36/lib/netstandard2.0/FSharp.SystemTextJson.dll",
        ] {
            let dll = nuget.join(rel);
            std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
            std::fs::write(&dll, b"").unwrap();
        }
        let dotnet_root = tmp.path().join("dotnet");
        let pack = dotnet_root.join("packs/Microsoft.NETCore.App.Ref/10.0.0/ref/net10.0");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("System.dll"), b"").unwrap();
        std::fs::write(pack.join("System.Runtime.dll"), b"").unwrap();

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &proj,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        // Every DLL was unparseable; env is empty but we didn't panic.
        assert!(env.is_empty(), "{}", env.len());
    }

    #[test]
    fn assembly_env_is_cached_across_calls() {
        // Twice in a row over the same project: the same `Arc` comes back.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&[]));

        let mut sema = SemanticState::new();
        let first = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        let second = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(Arc::ptr_eq(&first, &second), "cache miss on repeat lookup");
    }

    /// The env is a pure function of `(project, dotnet_root)` — both inputs
    /// `build_assembly_env` reads. A first lookup made before the SDK root is
    /// known (`None`, or a wrong root) must not poison later lookups with the
    /// real root: keying on the project alone would hand the stale empty env
    /// back forever, silently disabling assembly resolution for the rest of
    /// the server's life. Distinct roots must miss; the same root must hit.
    #[test]
    fn assembly_env_cache_keys_on_dotnet_root() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&[]));
        let root = tmp.path().join("dotnet");
        std::fs::create_dir_all(&root).unwrap();

        let mut sema = SemanticState::new();
        // No-root lookup first, then the real root: the second call must
        // rebuild rather than reuse the no-root entry.
        let without_root = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        let with_root = sema.assembly_env_for_project(
            &proj,
            Some(&root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(
            !Arc::ptr_eq(&without_root, &with_root),
            "None and Some(root) shared a cache entry: a no-root miss would \
             permanently mask later resolution with the real root"
        );
        // The root genuinely participates in the key (not a miss-everything
        // switch): repeating the same root hits the cache.
        let with_root_again = sema.assembly_env_for_project(
            &proj,
            Some(&root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(
            Arc::ptr_eq(&with_root, &with_root_again),
            "same (project, root) should hit the cache"
        );
    }

    /// The env cache keys on the chosen TFM (fsproj 3.3c stage 2, plan E3):
    /// a different TFM selects a different assets target, so it must be a
    /// different env — sharing an entry would serve one TFM's assemblies
    /// against another TFM's parse.
    #[test]
    fn assembly_env_cache_keys_on_target_framework() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&[]));

        let mut sema = SemanticState::new();
        let untargeted = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        let net8 = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::Tfm("net8.0".to_string()),
            &Workspace::default(),
        );
        assert!(
            !Arc::ptr_eq(&untargeted, &net8),
            "None and Some(tfm) shared a cache entry"
        );
        let net10 = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::Tfm("net10.0".to_string()),
            &Workspace::default(),
        );
        assert!(
            !Arc::ptr_eq(&net8, &net10),
            "distinct TFMs shared a cache entry"
        );
        let net8_again = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::Tfm("net8.0".to_string()),
            &Workspace::default(),
        );
        assert!(
            Arc::ptr_eq(&net8, &net8_again),
            "same (project, root, tfm) should hit the cache"
        );
    }

    /// Write a minimal assets file with one package (and its compile DLL)
    /// per `(tfm, package_name)` target. The package DLL paths need not
    /// exist: `pick_existing` falls back to the primary `packageFolders`
    /// candidate, which is exactly what the reference-set observability
    /// surface reports.
    fn write_assets_with_packages(path: &Path, nuget: &Path, tfms_and_pkgs: &[(&str, &str)]) {
        let mut targets = serde_json::Map::new();
        let mut libraries = serde_json::Map::new();
        let mut frameworks = serde_json::Map::new();
        for (tfm, pkg) in tfms_and_pkgs {
            let name_version = format!("{pkg}/1.0.0");
            let mut compile = serde_json::Map::new();
            compile.insert(
                format!("lib/netstandard2.0/{pkg}.dll"),
                serde_json::json!({}),
            );
            let mut target = serde_json::Map::new();
            target.insert(
                name_version.clone(),
                serde_json::json!({ "type": "package", "compile": compile }),
            );
            targets.insert((*tfm).to_string(), serde_json::Value::Object(target));
            libraries.insert(
                name_version,
                serde_json::json!({
                    "type": "package",
                    "path": format!("{}/1.0.0", pkg.to_lowercase()),
                }),
            );
            frameworks.insert((*tfm).to_string(), serde_json::json!({}));
        }
        let doc = serde_json::json!({
            "version": 3,
            "targets": targets,
            "libraries": libraries,
            "packageFolders": { nuget.to_str().unwrap(): {} },
            "project": { "frameworks": frameworks },
        });
        write(path, &serde_json::to_string_pretty(&doc).unwrap());
    }

    /// The gated `<TargetFramework>` fixture the entry-side provenance tests
    /// share: the write may not run in a real build, so the value is not
    /// current truth.
    const UNTRUSTED_TFM_FSPROJ: &str = r#"<Project>
      <PropertyGroup Condition="'$(DefineConstants)' == ''">
        <TargetFramework>net8.0</TargetFramework>
      </PropertyGroup>
    </Project>"#;

    /// The entry-side provenance gate, end to end (3.3d round 19): an entry
    /// whose body `TargetFramework` sits behind an unpinnable gate must not
    /// select that TFM's assets target — the reference set degrades to
    /// empty (D5: under-resolve, never wrong) instead of serving the
    /// untrusted TFM's assemblies.
    #[test]
    fn untrusted_entry_tfm_does_not_select_an_assets_target() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, UNTRUSTED_TFM_FSPROJ);
        write_assets_with_packages(
            &tmp.path().join("obj/project.assets.json"),
            &tmp.path().join("nuget"),
            &[("net8.0", "Pkg.Eight"), ("net9.0", "Pkg.Nine")],
        );
        let dotnet_root = tmp.path().join("dotnet");
        std::fs::create_dir_all(&dotnet_root).unwrap();

        let mut ws = Workspace::default();
        let tfm = ws.served_tfm_for_project(&proj);
        assert_eq!(tfm, ServedTfm::Untrusted);
        let mut sema = SemanticState::new();
        let dlls = sema.reference_dlls_for_project(&proj, Some(&dotnet_root), &tfm, &ws);
        assert!(
            dlls.is_empty(),
            "no target is provably the real build's: {dlls:?}"
        );
    }

    /// An untrusted TFM is **not** "no TFM declared": with no declared TFM
    /// the restore is the only evidence and the single-target fallback is
    /// sound, but under an untrusted TFM the evidence *conflicts* — an
    /// evaluated value we declined to serve, and a restore that may lag it
    /// (the gate the walker couldn't pin usually does run, so a sole
    /// stale `net9.0` target against an evaluated `net8.0` would fold an
    /// unrelated TFM's assemblies). Nothing proves the real target, so the
    /// env is empty (D5) — where the genuinely-TFM-less project (second
    /// half) keeps the sole-target fallback.
    #[test]
    fn untrusted_entry_tfm_does_not_fall_back_to_a_single_target_restore() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, UNTRUSTED_TFM_FSPROJ);
        write_assets_with_packages(
            &tmp.path().join("obj/project.assets.json"),
            &tmp.path().join("nuget"),
            &[("net9.0", "Pkg.Nine")],
        );
        let dotnet_root = tmp.path().join("dotnet");
        std::fs::create_dir_all(&dotnet_root).unwrap();

        let mut ws = Workspace::default();
        let tfm = ws.served_tfm_for_project(&proj);
        assert_eq!(tfm, ServedTfm::Untrusted);
        let mut sema = SemanticState::new();
        let dlls = sema.reference_dlls_for_project(&proj, Some(&dotnet_root), &tfm, &ws);
        assert!(
            dlls.is_empty(),
            "a sole target that may lag the untrusted evaluation proves nothing: {dlls:?}"
        );

        // The contrast case: NO TFM declared anywhere — the restore is the
        // only evidence, and the sole target serves.
        let bare = tmp.path().join("Bare/Bare.fsproj");
        write(&bare, &fsproj(&[]));
        write_assets_with_packages(
            &tmp.path().join("Bare/obj/project.assets.json"),
            &tmp.path().join("nuget"),
            &[("net9.0", "Pkg.Nine")],
        );
        let tfm = ws.served_tfm_for_project(&bare);
        assert_eq!(tfm, ServedTfm::NoneDeclared);
        let dlls = sema.reference_dlls_for_project(&bare, Some(&dotnet_root), &tfm, &ws);
        assert!(
            dlls.iter()
                .any(|d| d.file_name().is_some_and(|n| n == "Pkg.Nine.dll")),
            "the TFM-less project's sole restored target still serves: {dlls:?}"
        );
    }

    /// The version-shape-sensitivity fold gate (3.3d round 19 follow-up): a
    /// project whose `<LangVersion>` provenance is untrusted refuses the fold
    /// **only** when some file's parse shape actually depends on the language
    /// version (`Parse::shape_depends_on_language_version` — an offside
    /// version-gated context push, the F# 8 strict-indentation boundary).
    /// Version-invariant files keep folding under the same untrusted
    /// provenance — which is what keeps real SDK projects folding, since a
    /// real SDK's conditional LangVersion default marks the provenance for
    /// every project (`sdk_project_fold_e2e`).
    #[test]
    fn untrusted_lang_version_refuses_the_fold_only_for_straddling_files() {
        // The straddling fixture: verified by the cst crate's
        // `shape_sensitivity` tests to parse to *different trees* at F# 7 vs
        // F# 10 (an EOF-anchored MatchClauses push, offside of the top-level
        // block).
        let straddling = "match x with\n";
        let gated_lang_version = |body: &str| {
            format!(
                r#"<Project>
              <PropertyGroup Condition="'$(DefineConstants)' == ''">
                <LangVersion>7.0</LangVersion>
              </PropertyGroup>
              {body}
            </Project>"#
            )
        };

        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Straddling/P.fsproj");
        write(
            &proj,
            &gated_lang_version(r#"<ItemGroup><Compile Include="A.fs" /></ItemGroup>"#),
        );
        write(&tmp.path().join("Straddling/A.fs"), straddling);

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        assert!(
            sema.parses_for_project(&proj, &mut ws, &HashMap::new())
                .is_none(),
            "unknowable version × version-dependent parse shape must refuse the fold"
        );

        // Same untrusted provenance, version-invariant file: folds.
        let plain = tmp.path().join("Plain/P.fsproj");
        write(
            &plain,
            &gated_lang_version(r#"<ItemGroup><Compile Include="B.fs" /></ItemGroup>"#),
        );
        write(&tmp.path().join("Plain/B.fs"), "let b = 1\n");
        assert!(
            sema.parses_for_project(&plain, &mut ws, &HashMap::new())
                .is_some(),
            "a version-invariant parse is provably safe under any LangVersion"
        );

        // The flag over-approximates: an EOF-anchored version-gated push (a
        // common mid-edit state — `module M =` at end of file) sets it, yet
        // the EOF force-closure cascade reconverges to identical trees on
        // both sides of the boundary. The fold must verify genuine
        // divergence before refusing, or every real SDK project (always
        // provenance-tainted) would lose resolution while the user types.
        let midedit = tmp.path().join("MidEdit/P.fsproj");
        write(
            &midedit,
            &gated_lang_version(r#"<ItemGroup><Compile Include="D.fs" /></ItemGroup>"#),
        );
        write(&tmp.path().join("MidEdit/D.fs"), "module M =\n");
        assert!(
            sema.parses_for_project(&midedit, &mut ws, &HashMap::new())
                .is_some(),
            "a flagged parse that reconverges to the same tree is provably \
             safe and must fold"
        );

        // Straddling file but a cleanly pinned version: the version is known
        // (SDK-less fixture — no SDK taint), so the shape dependence is
        // resolved and the fold proceeds.
        let pinned = tmp.path().join("Pinned/P.fsproj");
        write(
            &pinned,
            r#"<Project>
              <PropertyGroup><LangVersion>7.0</LangVersion></PropertyGroup>
              <ItemGroup><Compile Include="C.fs" /></ItemGroup>
            </Project>"#,
        );
        write(&tmp.path().join("Pinned/C.fs"), straddling);
        assert!(
            sema.parses_for_project(&pinned, &mut ws, &HashMap::new())
                .is_some(),
            "a trustworthily pinned version resolves the shape dependence"
        );
    }

    /// End to end (fsproj 3.3c): a multi-targeted project with a
    /// `$(TargetFramework)`-gated define folds — before 3.3c the gate flipped
    /// `define_constants_uncertain` and `parses_for_project` refused the whole
    /// project, dropping it to single-file fallback.
    #[test]
    fn multi_target_project_folds_under_first_declared_tfm() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(
            &proj,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
              </PropertyGroup>
              <PropertyGroup Condition="'$(TargetFramework)' == 'net8.0'">
                <DefineConstants>EIGHT</DefineConstants>
              </PropertyGroup>
              <ItemGroup><Compile Include="A.fs" /></ItemGroup>
            </Project>"#,
        );
        write(
            &tmp.path().join("A.fs"),
            "#if EIGHT\nlet fromEight = 1\n#else\nlet fromTen = 2\n#endif\n",
        );

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let docs = HashMap::new();
        assert!(
            sema.parses_for_project(&proj, &mut ws, &docs).is_some(),
            "multi-TFM project must fold under the chosen TFM"
        );
        assert!(sema.resolved_project_for(&proj, &mut ws, &docs).is_some());
        assert_eq!(
            ws.target_framework_for_project(&proj),
            Some("net8.0".to_string())
        );
    }

    // ---- entry-rooted C#-ref TFM recovery (fsproj 3.3c stage 2, plan E3) ----

    /// Write a minimal assets file: `targets`/`project.frameworks` for `tfm`,
    /// with the given project-kind entries (`(name_version, framework,
    /// msbuild_project_rel)`).
    fn write_assets_with_project_refs(path: &Path, tfm: &str, refs: &[(&str, &str, &str)]) {
        let mut target = serde_json::Map::new();
        let mut libraries = serde_json::Map::new();
        for (name_version, framework, rel) in refs {
            target.insert(
                (*name_version).to_string(),
                serde_json::json!({ "type": "project", "framework": framework }),
            );
            libraries.insert(
                (*name_version).to_string(),
                serde_json::json!({ "type": "project", "msbuildProject": rel, "path": rel }),
            );
        }
        let doc = serde_json::json!({
            "version": 3,
            "targets": { tfm: target },
            "libraries": libraries,
            "packageFolders": {},
            "project": { "frameworks": { tfm: {} } },
        });
        write(path, &serde_json::to_string_pretty(&doc).unwrap());
    }

    /// The 3.3b residual, closed: a platform-qualified consumer's assets
    /// record the C# producer's `framework` without the platform suffix
    /// (`.NETCoreApp,Version=v8.0`, never `net8.0-windows`). Rooting Phase 2b
    /// at the entry recovers the producer's real target from its own declared
    /// TFM list — verified to fail against the base-TFM guess (a
    /// `net8.0` lookup in the producer's `net8.0-windows`-only assets errors).
    #[test]
    fn entry_ref_tfms_recovers_platform_suffix() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let entry = root.join("Fs/Fs.fsproj");
        let cs = root.join("Cs/Cs.csproj");
        write(&entry, &fsproj(&[]));
        write(&cs, "<Project></Project>");
        write_assets_with_project_refs(
            &root.join("Fs/obj/project.assets.json"),
            "net8.0-windows",
            &[("Cs/1.0.0", ".NETCoreApp,Version=v8.0", "../Cs/Cs.csproj")],
        );
        // The producer's own assets declare the platform-qualified TFM.
        write_assets_with_project_refs(
            &root.join("Cs/obj/project.assets.json"),
            "net8.0-windows",
            &[],
        );

        let recovered = entry_ref_tfms(&entry, Some("net8.0-windows"));
        let canon_cs = fs::canonicalize(&cs).unwrap();
        assert_eq!(
            recovered.get(&canon_cs).map(String::as_str),
            Some("net8.0-windows"),
            "{recovered:?}"
        );

        // Pin the failure this closes: the base-TFM guess cannot find the
        // producer's target at all.
        assert!(
            resolve_transitive_project_tfms(&cs, "net8.0").is_err(),
            "base-TFM lookup in the producer's own assets was expected to miss"
        );
    }

    #[test]
    fn entry_ref_tfms_without_entry_tfm_or_assets_is_empty() {
        // No chosen TFM → no recovery (callers fall back to the base TFM);
        // same for a missing/unreadable entry assets file (partial restore).
        let tmp = TempDir::new().unwrap();
        let entry = tmp.path().join("Fs/Fs.fsproj");
        write(&entry, &fsproj(&[]));
        assert!(entry_ref_tfms(&entry, None).is_empty());
        assert!(entry_ref_tfms(&entry, Some("net8.0")).is_empty());
    }

    /// `invalidate_project` only clears the compile-order parses, not the
    /// assembly env. The env reads only disk artifacts the editor cannot edit;
    /// re-reading every framework DLL on every keystroke would be a measurable
    /// regression and the env wouldn't change anyway.
    #[test]
    fn invalidate_project_does_not_drop_assembly_env() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&[]));

        let mut sema = SemanticState::new();
        let before = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        sema.invalidate_project(&proj);
        let after = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(
            Arc::ptr_eq(&before, &after),
            "invalidate_project dropped the assembly env"
        );
    }

    #[test]
    fn invalidate_all_drops_assembly_env() {
        // The contrast with `invalidate_project`: a `didChangeWatchedFiles`
        // structural change must drop the assembly env (a `.fsproj` /
        // `project.assets.json` edit can change the referenced set), so the
        // next lookup rebuilds a fresh `Arc`.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&[]));

        let mut sema = SemanticState::new();
        let before = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        sema.invalidate_all();
        let after = sema.assembly_env_for_project(
            &proj,
            None,
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(
            !Arc::ptr_eq(&before, &after),
            "invalidate_all must drop the assembly env so it rebuilds"
        );
    }

    // ---- graph_ref_targets: graph-sourced reference edges ----

    fn graph_node(path: &Path, kind: ProjectKind) -> crate::project_graph::ProjectNode {
        crate::project_graph::ProjectNode {
            path: lexically_normalize(path),
            kind,
            references: Vec::new(),
            tfm: crate::project_graph::NodeTfm::NotEvaluated,
            output_name: None,
        }
    }

    /// F# closure nodes land in `fsharp` (minus the entry), C# boundary nodes
    /// in `csharp`; graph problems are ignored (they are the diagnostics
    /// layer's business — a problematic target is not a node, so nothing can
    /// be folded from it).
    #[test]
    fn graph_ref_targets_splits_kinds_and_excludes_entry() {
        let entry = Path::new("/p/App/App.fsproj");
        let graph = ProjectGraph {
            nodes: vec![
                graph_node(entry, ProjectKind::FSharp),
                graph_node(Path::new("/p/LibA/LibA.fsproj"), ProjectKind::FSharp),
                graph_node(Path::new("/p/CsLib/CsLib.csproj"), ProjectKind::CSharp),
            ],
            problems: vec![crate::project_graph::GraphProblem::Cycle {
                referrer: PathBuf::from("/p/LibA/LibA.fsproj"),
                target: lexically_normalize(entry),
                span: 0..0,
            }],
        };
        let targets = graph_ref_targets(&graph, entry);
        // The fixture paths don't exist, so canonicalisation falls back to the
        // node paths themselves. The node's TFM verdict rides along verbatim.
        assert_eq!(
            targets.fsharp,
            vec![FsharpRefTarget {
                path: PathBuf::from("/p/LibA/LibA.fsproj"),
                tfm: crate::project_graph::NodeTfm::NotEvaluated,
                output_name: None,
            }]
        );
        assert_eq!(targets.csharp, vec![PathBuf::from("/p/CsLib/CsLib.csproj")]);
    }

    /// Targets are canonicalised so lookups into the assets-derived
    /// producer-TFM maps (whose keys are `fs::canonicalize`d) hit. On macOS
    /// the tempdir sits behind a `/var → /private/var` symlink, which is
    /// exactly the mismatch this guards against; elsewhere the assertion
    /// still pins "output equals `fs::canonicalize`".
    #[test]
    fn graph_ref_targets_canonicalises_existing_targets() {
        let tmp = TempDir::new().unwrap();
        let dep = tmp.path().join("Lib/Lib.fsproj");
        write(&dep, "<Project />");
        let entry = tmp.path().join("App/App.fsproj");
        let graph = ProjectGraph {
            nodes: vec![
                graph_node(&entry, ProjectKind::FSharp),
                graph_node(&dep, ProjectKind::FSharp),
            ],
            problems: Vec::new(),
        };
        let targets = graph_ref_targets(&graph, &entry);
        assert_eq!(
            targets.fsharp,
            vec![FsharpRefTarget {
                path: fs::canonicalize(&dep).unwrap(),
                tfm: crate::project_graph::NodeTfm::NotEvaluated,
                output_name: None,
            }]
        );
        assert!(targets.csharp.is_empty());
    }

    // ---- F# project-reference output-DLL resolution ----

    /// Build the shared `MiniLibFs` F# assembly fixture (owned by the
    /// `borzoi-assembly` crate) on demand and return its DLL bytes.
    ///
    /// The DLL is a `dotnet build` output: it is git-ignored and therefore
    /// absent on a clean checkout (e.g. CI), so we cannot read it from a
    /// committed path — we build it here.
    ///
    /// The build happens in a *private copy* of the fixture directory rather
    /// than in the committed checkout. `cargo test` runs test binaries in
    /// parallel, and the assembly crate's own tests build this same fixture;
    /// two `dotnet build` processes sharing the fixture's `obj/`/`bin/` race
    /// and one exits non-zero. Copying `MiniLibFs` (which has no
    /// `ProjectReference`, so its own directory suffices) into a `TempDir`
    /// gives this build disjoint writable state, so no cross-process lock is
    /// possible or needed. The bytes are cached in a `OnceLock` so the build
    /// runs once per test binary; the `TempDir` is dropped once the bytes are
    /// read, so nothing is left behind.
    fn minilibfs_dll_bytes() -> Vec<u8> {
        static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        BYTES
            .get_or_init(|| {
                let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../assembly/tests/fixtures/assembly/MiniLibFs");
                let workspace = TempDir::new().expect("create MiniLibFs build temp dir");
                let project = workspace.path().join("MiniLibFs");
                copy_tree_no_build_dirs(&source, &project);
                let mut build = std::process::Command::new("dotnet");
                build
                    .args(["build", "-c", "Release", "--nologo"])
                    .arg(&project);
                let status = crate::spawn::status_serialised(&mut build)
                    .expect("spawn dotnet build for MiniLibFs fixture");
                assert!(status.success(), "dotnet build MiniLibFs fixture failed");
                let dll = project.join("bin/Release/net10.0/MiniLibFs.dll");
                std::fs::read(&dll).expect("read MiniLibFs.dll fixture")
            })
            .clone()
    }

    /// Recursively copy `src` into `dst`, skipping any `bin`/`obj` directory.
    ///
    /// A copied `obj/` bakes absolute paths from the original location into
    /// `project.assets.json` and the MSBuild caches, which would make the
    /// copy build incrementally against the wrong tree; dropping the build
    /// dirs yields a pristine source tree that builds from scratch.
    fn copy_tree_no_build_dirs(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap_or_else(|e| panic!("create {}: {e}", dst.display()));
        for entry in fs::read_dir(src).unwrap_or_else(|e| panic!("read {}: {e}", src.display())) {
            let entry = entry.expect("read fixture tree entry");
            let name = entry.file_name();
            let from = entry.path();
            let to = dst.join(&name);
            if from.is_dir() {
                if name == "bin" || name == "obj" {
                    continue;
                }
                copy_tree_no_build_dirs(&from, &to);
            } else {
                fs::copy(&from, &to)
                    .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", from.display(), to.display()));
            }
        }
    }

    /// Lay down a fake built F# project at `dir`/`<name>.fsproj` whose
    /// `bin/<config>/<tfm>/<name>.dll` holds `bytes`. Returns the `.fsproj`
    /// path. The `.fsproj` file itself is created (empty) so a caller that
    /// canonicalises the reference target succeeds.
    fn built_fsproj(dir: &Path, name: &str, config: &str, tfm: &str, dll_bytes: &[u8]) -> PathBuf {
        let proj = dir.join(format!("{name}.fsproj"));
        write(&proj, "<Project />");
        let dll = dir
            .join("bin")
            .join(config)
            .join(tfm)
            .join(format!("{name}.dll"));
        write(&dll, "");
        std::fs::write(&dll, dll_bytes).unwrap();
        proj
    }

    #[test]
    fn locate_output_dll_finds_built_dll() {
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(tmp.path(), "Lib", "Release", "net10.0", b"stub");
        let found = locate_fsharp_output_dll(&proj, None, "Lib").expect("built DLL is located");
        assert!(found.ends_with("bin/Release/net10.0/Lib.dll"), "{found:?}");
    }

    #[test]
    fn locate_output_dll_none_when_unbuilt() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Lib.fsproj");
        write(&proj, "<Project />");
        // No bin/ at all.
        assert!(locate_fsharp_output_dll(&proj, None, "Lib").is_none());
    }

    #[test]
    fn locate_output_dll_prefers_debug_over_release() {
        // Both configs built: the editing default (Debug) wins deterministically.
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(tmp.path(), "Lib", "Release", "net10.0", b"rel");
        // Add a Debug variant next to it.
        let debug = tmp.path().join("bin/Debug/net10.0/Lib.dll");
        write(&debug, "");
        std::fs::write(&debug, b"dbg").unwrap();

        let found = locate_fsharp_output_dll(&proj, None, "Lib").expect("a DLL is located");
        assert!(
            found.ends_with("bin/Debug/net10.0/Lib.dll"),
            "expected the Debug build to win, got {found:?}"
        );
    }

    #[test]
    fn locate_output_dll_ignores_unrelated_dlls() {
        // The bin dir holds a copied dependency (`Other.dll`) but not this
        // project's own output: matching by stem must not grab the stranger.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Lib.fsproj");
        write(&proj, "<Project />");
        let other = tmp.path().join("bin/Debug/net10.0/Other.dll");
        write(&other, "");
        assert!(locate_fsharp_output_dll(&proj, None, "Lib").is_none());
    }

    /// The 3.3a `<AssemblyName>`-override fix: when the caller recovered the
    /// producer's evaluated output name from the assets file, the locator
    /// looks for *that* DLL — the project-file stem no longer has to match.
    #[test]
    fn locate_output_dll_uses_recovered_assembly_name() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Lib.fsproj");
        write(&proj, "<Project />");
        let renamed = tmp.path().join("bin/Debug/net10.0/Renamed.dll");
        write(&renamed, "");

        // Stem-based lookup misses (there is no Lib.dll)…
        assert!(locate_fsharp_output_dll(&proj, None, "Lib").is_none());
        // …but the recovered name finds the override's output.
        let found = locate_fsharp_output_dll(&proj, None, "Renamed").expect("located");
        assert!(
            found.ends_with("bin/Debug/net10.0/Renamed.dll"),
            "{found:?}"
        );
    }

    /// The recovered assembly name *replaces* the stem — it must not fall back
    /// to a stem-named DLL when the named output is absent (that DLL is some
    /// other project's copied dependency, not this producer's output).
    #[test]
    fn locate_output_dll_recovered_name_never_falls_back_to_stem() {
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(tmp.path(), "Lib", "Debug", "net10.0", b"stub");
        assert!(locate_fsharp_output_dll(&proj, None, "Renamed").is_none());
    }

    /// Without a producer TFM, outputs under *several distinct* TFM
    /// directories are ambiguous: picking one could serve a variant NuGet's
    /// restore wouldn't select (the codex 3.3d review's second finding — an
    /// unrestored multi-TFM sibling). Skip the ref instead (under-resolve,
    /// never wrong).
    #[test]
    fn locate_output_dll_skips_ambiguous_tfms_without_producer_tfm() {
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(tmp.path(), "Lib", "Debug", "net10.0", b"ten");
        let eight = tmp.path().join("bin/Debug/net8.0/Lib.dll");
        write(&eight, "");
        std::fs::write(&eight, b"eight").unwrap();
        assert!(
            locate_fsharp_output_dll(&proj, None, "Lib").is_none(),
            "two TFM variants with no producer TFM must skip, not guess"
        );
    }

    /// fsproj 3.3c (plan E5 for F# refs): when the producer's selected TFM is
    /// known, the ref lookup must pick *that* TFM's output — not the
    /// lexicographically-first variant. `net10.0` sorts before `net8.0`, so
    /// the blind pick would take the wrong DLL here.
    #[test]
    fn locate_output_dll_prefers_the_producer_tfm() {
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(tmp.path(), "Lib", "Debug", "net10.0", b"ten");
        let eight = tmp.path().join("bin/Debug/net8.0/Lib.dll");
        write(&eight, "");
        std::fs::write(&eight, b"eight").unwrap();

        let found = locate_fsharp_output_dll(&proj, Some("net8.0"), "Lib").expect("located");
        assert!(
            found.ends_with("bin/Debug/net8.0/Lib.dll"),
            "expected the producer TFM's build, got {found:?}"
        );
    }

    /// fsproj 3.3c (plan E6 for F# refs): a known producer TFM whose output
    /// isn't built must skip — never serve another TFM's assembly against a
    /// parse evaluated under the selected one.
    #[test]
    fn locate_output_dll_skips_when_producer_tfm_output_absent() {
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(tmp.path(), "Lib", "Debug", "net10.0", b"ten");
        assert!(locate_fsharp_output_dll(&proj, Some("net8.0"), "Lib").is_none());
    }

    /// A [`FsharpRefTarget`] whose graph-carried name is the project-file
    /// stem — the shape the walk produces for a producer without an
    /// `<AssemblyName>` override.
    fn ref_target(path: PathBuf, tfm: NodeTfm) -> FsharpRefTarget {
        let output_name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
        FsharpRefTarget {
            path,
            tfm,
            output_name,
        }
    }

    #[test]
    fn fsharp_project_ref_dlls_uses_the_node_tfm() {
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(&tmp.path().join("Fs"), "Fs", "Debug", "net10.0", b"ten");
        let eight = tmp.path().join("Fs/bin/Debug/net8.0/Fs.dll");
        write(&eight, "");
        std::fs::write(&eight, b"eight").unwrap();

        let dlls = fsharp_project_ref_dlls(
            &[ref_target(proj, NodeTfm::Known("net8.0".to_string()))],
            &BTreeMap::new(),
        );
        assert_eq!(dlls.len(), 1, "{dlls:?}");
        assert!(
            dlls[0].ends_with("Fs/bin/Debug/net8.0/Fs.dll"),
            "{:?}",
            dlls[0]
        );
    }

    /// A [`NodeTfm::Unresolved`] ref is skipped even though a (single) output
    /// exists on disk: it may be a stale build of a TFM the real build would
    /// not select (codex 3.3d review, round 5).
    #[test]
    fn fsharp_project_ref_dlls_skips_unresolved_tfm() {
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(&tmp.path().join("Fs"), "Fs", "Debug", "net10.0", b"ten");
        let dlls =
            fsharp_project_ref_dlls(&[ref_target(proj, NodeTfm::Unresolved)], &BTreeMap::new());
        assert!(dlls.is_empty(), "{dlls:?}");
    }

    /// A [`NodeTfm::NotEvaluated`] **F#** ref is skipped too (codex 3.3d
    /// review, round 6): for an F# node it means the project exists but could
    /// not be evaluated (malformed XML, evaluation failure), so nothing about
    /// it — its declared TFMs, its assembly name, whether the current build
    /// would even succeed — is known. A stale on-disk output proves none of
    /// that; folding it would trust a build we cannot connect to the current
    /// source.
    #[test]
    fn fsharp_project_ref_dlls_skips_not_evaluated() {
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(&tmp.path().join("Fs"), "Fs", "Debug", "net10.0", b"ten");
        let dlls =
            fsharp_project_ref_dlls(&[ref_target(proj, NodeTfm::NotEvaluated)], &BTreeMap::new());
        assert!(dlls.is_empty(), "{dlls:?}");
    }

    #[test]
    fn fsharp_project_ref_dlls_skips_csproj() {
        // A C# reference with a built output is ignored (sidecar's domain);
        // only the F# reference's output is returned.
        let tmp = TempDir::new().unwrap();
        let fs_proj = built_fsproj(&tmp.path().join("Fs"), "Fs", "Debug", "net10.0", b"fs");
        let cs_proj = tmp.path().join("Cs").join("Cs.csproj");
        write(&cs_proj, "<Project />");
        let cs_dll = tmp.path().join("Cs/bin/Debug/net10.0/Cs.dll");
        write(&cs_dll, "");

        let refs = vec![
            ref_target(cs_proj.clone(), NodeTfm::NotEvaluated),
            ref_target(fs_proj.clone(), NodeTfm::NoneDeclared),
        ];
        let dlls = fsharp_project_ref_dlls(&refs, &BTreeMap::new());
        assert_eq!(dlls.len(), 1, "{dlls:?}");
        assert!(
            dlls[0].ends_with("Fs/bin/Debug/net10.0/Fs.dll"),
            "{:?}",
            dlls[0]
        );
    }

    #[test]
    fn fsharp_project_ref_dlls_dedups() {
        // The same F# project listed twice contributes its output DLL once.
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(&tmp.path().join("Fs"), "Fs", "Debug", "net10.0", b"fs");
        let dlls = fsharp_project_ref_dlls(
            &[
                ref_target(proj.clone(), NodeTfm::NoneDeclared),
                ref_target(proj, NodeTfm::NoneDeclared),
            ],
            &BTreeMap::new(),
        );
        assert_eq!(dlls.len(), 1, "{dlls:?}");
    }

    /// A graph-only ref (absent from the entry's assets file — an un-restored
    /// edge) whose producer overrides `<AssemblyName>`: the walk carries the
    /// evaluated name, so the fold locates the renamed DLL and must NOT fold
    /// the stale stem-named one a pre-rename build left behind.
    #[test]
    fn fsharp_project_ref_dlls_uses_the_graph_carried_output_name() {
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(
            &tmp.path().join("Fs"),
            "Fs",
            "Debug",
            "net10.0",
            b"stale-stem",
        );
        let renamed = tmp.path().join("Fs/bin/Debug/net10.0/Renamed.dll");
        write(&renamed, "");
        std::fs::write(&renamed, b"renamed").unwrap();

        let refs = vec![FsharpRefTarget {
            path: proj,
            tfm: NodeTfm::Known("net10.0".to_string()),
            output_name: Some("Renamed".to_string()),
        }];
        let dlls = fsharp_project_ref_dlls(&refs, &BTreeMap::new());
        assert_eq!(dlls.len(), 1, "{dlls:?}");
        assert!(
            dlls[0].ends_with("Fs/bin/Debug/net10.0/Renamed.dll"),
            "the stale stem-named DLL must not be folded: {:?}",
            dlls[0]
        );
    }

    /// No assets entry AND no trustworthy graph name (an `<AssemblyName>`
    /// whose provenance the evaluator couldn't pin): the ref is declined
    /// outright — a stem guess could fold a stale pre-rename DLL (D5:
    /// under-resolve, never wrong).
    #[test]
    fn fsharp_project_ref_dlls_declines_without_a_trustworthy_name() {
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(
            &tmp.path().join("Fs"),
            "Fs",
            "Debug",
            "net10.0",
            b"stale-stem",
        );
        let refs = vec![FsharpRefTarget {
            path: proj,
            tfm: NodeTfm::Known("net10.0".to_string()),
            output_name: None,
        }];
        let dlls = fsharp_project_ref_dlls(&refs, &BTreeMap::new());
        assert!(dlls.is_empty(), "{dlls:?}");
    }

    /// The graph node's evaluated `$(TargetName)` wins over the assets
    /// file's recorded name: the assets record the **AssemblyName**
    /// (probed — a `TargetName`-renamed producer's assets say
    /// `bin/placeholder/<AssemblyName>.dll` while the file on disk is
    /// `<TargetName>.dll`), so under an explicit `TargetName` override only
    /// the graph name matches what MSBuild wrote to `bin/`.
    #[test]
    fn fsharp_project_ref_dlls_prefers_the_graph_evaluated_name() {
        let tmp = TempDir::new().unwrap();
        // `Identity.dll` also exists (a stale copy of what the assets-name
        // lookup would fetch); the TargetName-named file must win.
        let proj = built_fsproj(&tmp.path().join("Fs"), "Fs", "Debug", "net10.0", b"stale");
        let stale = tmp.path().join("Fs/bin/Debug/net10.0/Identity.dll");
        write(&stale, "");
        std::fs::write(&stale, b"assets-named").unwrap();
        let real = tmp.path().join("Fs/bin/Debug/net10.0/FsFileName.dll");
        write(&real, "");
        std::fs::write(&real, b"target-named").unwrap();

        let names = BTreeMap::from([(proj.clone(), "Identity".to_string())]);
        let refs = vec![FsharpRefTarget {
            path: proj,
            tfm: NodeTfm::Known("net10.0".to_string()),
            output_name: Some("FsFileName".to_string()),
        }];
        let dlls = fsharp_project_ref_dlls(&refs, &names);
        assert_eq!(dlls.len(), 1, "{dlls:?}");
        assert!(
            dlls[0].ends_with("Fs/bin/Debug/net10.0/FsFileName.dll"),
            "{:?}",
            dlls[0]
        );
    }

    /// A ref whose own evaluation couldn't pin a name still degrades to the
    /// assets-recorded one (restore-real data) before declining outright.
    #[test]
    fn fsharp_project_ref_dlls_falls_back_to_the_assets_name() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Fs/Fs.fsproj");
        write(&proj, "<Project />");
        let recorded = tmp.path().join("Fs/bin/Debug/net10.0/FromAssets.dll");
        write(&recorded, "");

        let names = BTreeMap::from([(proj.clone(), "FromAssets".to_string())]);
        let refs = vec![FsharpRefTarget {
            path: proj,
            tfm: NodeTfm::Known("net10.0".to_string()),
            output_name: None,
        }];
        let dlls = fsharp_project_ref_dlls(&refs, &names);
        assert_eq!(dlls.len(), 1, "{dlls:?}");
        assert!(
            dlls[0].ends_with("Fs/bin/Debug/net10.0/FromAssets.dll"),
            "{:?}",
            dlls[0]
        );
    }

    /// Write `<root>/obj/project.assets.json` for a single-target `net10.0`
    /// project, optionally recording a project-type reference to
    /// `MiniLibFs/MiniLibFs.fsproj` — the shape NuGet writes after a restore
    /// that saw such a `<ProjectReference>`. Also lays down the (empty)
    /// package folder the assets point at.
    fn write_app_assets(root: &Path, with_minilib_ref: bool) {
        let pkgs = root.join("pkgs");
        std::fs::create_dir_all(&pkgs).unwrap();
        let (targets, libraries) = if with_minilib_ref {
            (
                serde_json::json!({
                    "MiniLibFs/1.0.0": {
                        "type": "project",
                        "framework": ".NETCoreApp,Version=v10.0",
                        "compile": { "bin/placeholder/MiniLibFs.dll": {} }
                    }
                }),
                serde_json::json!({
                    "MiniLibFs/1.0.0": {
                        "type": "project",
                        "path": "MiniLibFs/MiniLibFs.fsproj",
                        "msbuildProject": "MiniLibFs/MiniLibFs.fsproj"
                    }
                }),
            )
        } else {
            (serde_json::json!({}), serde_json::json!({}))
        };
        let assets = serde_json::json!({
            "version": 3,
            "targets": { "net10.0": targets },
            "libraries": libraries,
            "packageFolders": { pkgs.to_str().unwrap(): {} },
            "project": { "frameworks": { "net10.0": {} } }
        });
        write(
            &root.join("obj").join("project.assets.json"),
            &serde_json::to_string(&assets).unwrap(),
        );
    }

    /// End-to-end: a restored project whose `.fsproj` declares an F#
    /// `<ProjectReference>` gains that sibling's types in its `AssemblyEnv`.
    /// Uses the real `MiniLibFs.dll` fixture (namespace `MiniLibFs`) as the
    /// referenced project's built output.
    #[test]
    fn assembly_env_includes_fsharp_project_reference_types() {
        let dll_bytes = minilibfs_dll_bytes();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // The referenced, *built* sibling F# project.
        let sibling_dir = root.join("MiniLibFs");
        std::fs::create_dir_all(&sibling_dir).unwrap();
        built_fsproj(&sibling_dir, "MiniLibFs", "Release", "net10.0", &dll_bytes);

        // The consuming project declares the ref and is "restored" (assets
        // record it too). A dummy dotnet_root (no framework packs) is enough —
        // we only care that the project-ref output DLL flows into the env.
        let proj = root.join("App.fsproj");
        write(&proj, &fsproj_with_refs(&["MiniLibFs/MiniLibFs.fsproj"]));
        let dotnet_root = root.join("dotnet");
        std::fs::create_dir_all(&dotnet_root).unwrap();
        write_app_assets(root, true);

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &proj,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(
            env.has_namespace(&["MiniLibFs".to_string()]),
            "expected the referenced project's `MiniLibFs` namespace in the env; \
             env len = {}",
            env.len()
        );
    }

    /// Graph-sourced edges (plan E1/E2): a project reference recorded in a
    /// *stale* `project.assets.json` but no longer declared in the `.fsproj`
    /// must NOT fold. The parsed graph is the authoritative edge set; serving
    /// a removed sibling's types would be fabrication (worse than
    /// under-resolution — D5 "never wrong").
    #[test]
    fn assembly_env_omits_ref_removed_from_fsproj() {
        let dll_bytes = minilibfs_dll_bytes();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let sibling_dir = root.join("MiniLibFs");
        std::fs::create_dir_all(&sibling_dir).unwrap();
        built_fsproj(&sibling_dir, "MiniLibFs", "Release", "net10.0", &dll_bytes);

        // The fsproj declares NO <ProjectReference>; only the stale assets
        // file does (restore predates the edit that removed the edge).
        let proj = root.join("App.fsproj");
        write(&proj, &fsproj(&[]));
        let dotnet_root = root.join("dotnet");
        std::fs::create_dir_all(&dotnet_root).unwrap();
        write_app_assets(root, true);

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &proj,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(
            !env.has_namespace(&["MiniLibFs".to_string()]),
            "a project ref present only in stale assets must not fold into the \
             env; env len = {}",
            env.len()
        );
    }

    /// The other drift direction: a `<ProjectReference>` added to the
    /// `.fsproj` but not yet restored (absent from `project.assets.json`)
    /// folds the already-built sibling's output immediately — the parsed graph
    /// is the edge source; assets only back artifacts (packages, frameworks,
    /// producer TFMs).
    #[test]
    fn assembly_env_includes_ref_not_yet_restored() {
        let dll_bytes = minilibfs_dll_bytes();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let sibling_dir = root.join("MiniLibFs");
        std::fs::create_dir_all(&sibling_dir).unwrap();
        built_fsproj(&sibling_dir, "MiniLibFs", "Release", "net10.0", &dll_bytes);

        // The fsproj declares the ref; the assets file (still valid — the
        // entry itself is restored) predates the edit and doesn't record it.
        let proj = root.join("App.fsproj");
        write(&proj, &fsproj_with_refs(&["MiniLibFs/MiniLibFs.fsproj"]));
        let dotnet_root = root.join("dotnet");
        std::fs::create_dir_all(&dotnet_root).unwrap();
        write_app_assets(root, false);

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &proj,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(
            env.has_namespace(&["MiniLibFs".to_string()]),
            "a `<ProjectReference>` declared in the fsproj must fold its built \
             sibling even before a restore records it; env len = {}",
            env.len()
        );
    }

    /// A sibling that **declares** multi-targeting but has no producer TFM
    /// (unrestored edge) must not fold even when exactly one output variant
    /// exists on disk: that single directory can be a stale build of the
    /// *other* TFM (here net10.0, while a net8.0 consumer's restore would
    /// select net8.0). The built-variant count is not evidence of which TFM
    /// the build would pick — only the declaration is.
    #[test]
    fn assembly_env_skips_unresolved_multi_target_ref() {
        let dll_bytes = minilibfs_dll_bytes();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let sibling_dir = root.join("MiniLibFs");
        std::fs::create_dir_all(&sibling_dir).unwrap();
        built_fsproj(&sibling_dir, "MiniLibFs", "Debug", "net10.0", &dll_bytes);
        // Overwrite the stub fsproj with a multi-targeting one: only the
        // net10.0 variant is built, and no restore recorded a producer TFM.
        write(
            &sibling_dir.join("MiniLibFs.fsproj"),
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
              </PropertyGroup>
            </Project>"#,
        );

        let proj = root.join("App.fsproj");
        write(&proj, &fsproj_with_refs(&["MiniLibFs/MiniLibFs.fsproj"]));
        let dotnet_root = root.join("dotnet");
        std::fs::create_dir_all(&dotnet_root).unwrap();
        write_app_assets(root, false);

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &proj,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(
            !env.has_namespace(&["MiniLibFs".to_string()]),
            "a multi-declaring sibling with no known producer TFM must not \
             fold its lone (possibly wrong-TFM) output; env len = {}",
            env.len()
        );
    }

    /// A `<ProjectReference ReferenceOutputAssembly="false">` is a build
    /// dependency, not a compile reference: MSBuild builds the target but
    /// never puts its output on the compiler's reference path, so folding it
    /// would expose types the compiler cannot see (fabrication).
    #[test]
    fn assembly_env_omits_reference_output_assembly_false_refs() {
        let dll_bytes = minilibfs_dll_bytes();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let sibling_dir = root.join("MiniLibFs");
        std::fs::create_dir_all(&sibling_dir).unwrap();
        built_fsproj(&sibling_dir, "MiniLibFs", "Release", "net10.0", &dll_bytes);

        let proj = root.join("App.fsproj");
        write(
            &proj,
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="MiniLibFs/MiniLibFs.fsproj" ReferenceOutputAssembly="false" />
              </ItemGroup>
            </Project>"#,
        );
        let dotnet_root = root.join("dotnet");
        std::fs::create_dir_all(&dotnet_root).unwrap();
        write_app_assets(root, false);

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &proj,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(
            !env.has_namespace(&["MiniLibFs".to_string()]),
            "a ReferenceOutputAssembly=false ref is not a compile reference \
             and must not fold; env len = {}",
            env.len()
        );
    }

    /// The *entry* is always seeded with its own chosen TFM (codex 3.3d
    /// review, round 3): that value is known by construction — it seeded the
    /// parses — so a multi-targeted entry's own `$(TargetFramework)`-gated
    /// `<ProjectReference>` must be walked even when assets-based
    /// producer-TFM recovery **fails outright** (here: the referenced
    /// sibling's own assets file is missing — a partial restore — which
    /// errors the whole transitive recovery into an empty map; a
    /// *successful* recovery already contains the entry, so only the failure
    /// path exercises the explicit seed). Without it, the
    /// TFM-invariant-edges fallback would drop the gated ref — an
    /// under-resolution of an edge whose TFM is not actually in doubt.
    #[test]
    fn assembly_env_seeds_the_entry_with_its_chosen_tfm() {
        let dll_bytes = minilibfs_dll_bytes();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let sibling_dir = root.join("MiniLibFs");
        std::fs::create_dir_all(&sibling_dir).unwrap();
        built_fsproj(&sibling_dir, "MiniLibFs", "Release", "net8.0", &dll_bytes);
        // Deliberately NO MiniLibFs/obj/project.assets.json: the partial
        // restore that errors the transitive recovery.

        // Multi-targeted entry whose ref to the sibling only exists under
        // the chosen (first-declared) TFM.
        let proj = root.join("App.fsproj");
        write(
            &proj,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="MiniLibFs/MiniLibFs.fsproj" Condition="'$(TargetFramework)' == 'net8.0'" />
              </ItemGroup>
            </Project>"#,
        );
        let pkgs = root.join("pkgs");
        std::fs::create_dir_all(&pkgs).unwrap();
        let assets = serde_json::json!({
            "version": 3,
            "targets": {
                "net8.0": {
                    "MiniLibFs/1.0.0": {
                        "type": "project",
                        "framework": ".NETCoreApp,Version=v8.0"
                    }
                }
            },
            "libraries": {
                "MiniLibFs/1.0.0": {
                    "type": "project",
                    "path": "MiniLibFs/MiniLibFs.fsproj",
                    "msbuildProject": "MiniLibFs/MiniLibFs.fsproj"
                }
            },
            "packageFolders": { pkgs.to_str().unwrap(): {} },
            "project": { "frameworks": { "net8.0": {} } }
        });
        write(
            &root.join("obj").join("project.assets.json"),
            &serde_json::to_string(&assets).unwrap(),
        );
        let dotnet_root = root.join("dotnet");
        std::fs::create_dir_all(&dotnet_root).unwrap();

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &proj,
            Some(&dotnet_root),
            &ServedTfm::Tfm("net8.0".to_string()),
            &Workspace::default(),
        );
        assert!(
            env.has_namespace(&["MiniLibFs".to_string()]),
            "the entry's own chosen-TFM-gated ref must be walked even with an \
             empty producer-TFM recovery; env len = {}",
            env.len()
        );
    }

    /// Producer-TFM seeding reaches the graph walk (codex 3.3d review): a
    /// multi-targeted dependency whose `<ProjectReference>` is gated on
    /// `$(TargetFramework)` must have its edges read under the TFM NuGet's
    /// restore selected for it (recovered from the assets closure), not its
    /// own first-declared TFM. Here `B` declares `net10.0;net8.0` but the
    /// entry consumes it at net8.0, and B's edge to `C` exists only at
    /// net8.0 — so C's types must fold. An unseeded walk evaluates B at
    /// net10.0 and never sees C.
    #[test]
    fn assembly_env_walks_conditional_edges_under_the_producer_tfm() {
        let dll_bytes = minilibfs_dll_bytes();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // C: the transitive dep whose types prove the edge was walked. Its
        // DLL carries the `MiniLibFs` namespace (the manifest, not the file
        // stem, is what the env indexes).
        let c_dir = root.join("C");
        std::fs::create_dir_all(&c_dir).unwrap();
        built_fsproj(&c_dir, "C", "Debug", "net8.0", &dll_bytes);

        // B: multi-targets with a net8.0-only edge to C. Unbuilt (its own
        // output is skipped — only the edge matters here). Its assets declare
        // net8.0 so entry-rooted producer-TFM recovery can select it.
        let b = root.join("B/B.fsproj");
        write(
            &b,
            r#"<Project>
              <PropertyGroup>
                <TargetFrameworks>net10.0;net8.0</TargetFrameworks>
              </PropertyGroup>
              <ItemGroup>
                <ProjectReference Include="../C/C.fsproj" Condition="'$(TargetFramework)' == 'net8.0'" />
              </ItemGroup>
            </Project>"#,
        );
        write_assets_with_project_refs(&root.join("B/obj/project.assets.json"), "net8.0", &[]);

        // App: consumes B at net8.0; its assets record the B ref's base
        // framework so recovery maps B → net8.0.
        let proj = root.join("App/App.fsproj");
        write(&proj, &fsproj_with_refs(&["../B/B.fsproj"]));
        let pkgs = root.join("pkgs");
        std::fs::create_dir_all(&pkgs).unwrap();
        let assets = serde_json::json!({
            "version": 3,
            "targets": {
                "net8.0": {
                    "B/1.0.0": { "type": "project", "framework": ".NETCoreApp,Version=v8.0" }
                }
            },
            "libraries": {
                "B/1.0.0": { "type": "project", "path": "../B/B.fsproj", "msbuildProject": "../B/B.fsproj" }
            },
            "packageFolders": { pkgs.to_str().unwrap(): {} },
            "project": { "frameworks": { "net8.0": {} } }
        });
        write(
            &root.join("App/obj/project.assets.json"),
            &serde_json::to_string(&assets).unwrap(),
        );
        let dotnet_root = root.join("dotnet");
        std::fs::create_dir_all(&dotnet_root).unwrap();

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &proj,
            Some(&dotnet_root),
            &ServedTfm::Tfm("net8.0".to_string()),
            &Workspace::default(),
        );
        assert!(
            env.has_namespace(&["MiniLibFs".to_string()]),
            "B's net8.0-conditional edge to C must be walked under B's \
             recovered producer TFM; env len = {}",
            env.len()
        );
    }

    // ---- per-DLL assembly-env degradation ----
    //
    // The reader is hardened enough that fuzzing real DLLs doesn't surface a
    // genuine parse/enumerate panic, so the panic paths are exercised with a
    // fake `EcmaView` that panics on demand. A *caught* panic still runs the
    // default panic hook, printing a backtrace to the test's captured stderr;
    // that's expected and hidden unless the test fails.

    /// What a [`FakeView`] does when its type defs are enumerated — the three
    /// per-DLL outcomes the env build must each survive.
    enum Enumerate {
        Ok(Vec<Entity>),
        OkWithSkips(Vec<Entity>, AssemblyProjectionSkips),
        Err,
        Panic,
    }

    /// A fake [`EcmaView`] whose enumeration succeeds, errors, or panics on
    /// demand. Only the enumeration is load-bearing here; the other methods
    /// return trivial values.
    struct FakeView {
        identity: AssemblyIdentity,
        enumerate: Enumerate,
    }

    fn fake_identity() -> AssemblyIdentity {
        AssemblyIdentity {
            name: "Fake".to_string(),
            version: Version {
                major: 1,
                minor: 0,
                build: 0,
                revision: 0,
            },
            public_key_token: None,
        }
    }

    impl FakeView {
        fn new(enumerate: Enumerate) -> Self {
            FakeView {
                identity: fake_identity(),
                enumerate,
            }
        }
    }

    impl EcmaView for FakeView {
        fn identity(&self) -> &AssemblyIdentity {
            &self.identity
        }
        fn assembly_refs(&self) -> Vec<AssemblyIdentity> {
            vec![]
        }
        fn enumerate_type_defs_with_skips(
            &self,
        ) -> Result<(Vec<Entity>, AssemblyProjectionSkips), ImportError> {
            match &self.enumerate {
                Enumerate::Ok(entities) => {
                    Ok((entities.clone(), AssemblyProjectionSkips::default()))
                }
                Enumerate::OkWithSkips(entities, skips) => Ok((entities.clone(), skips.clone())),
                Enumerate::Err => Err(ImportError::UnsupportedSignature {
                    detail: "fake unsupported signature".to_string(),
                }),
                Enumerate::Panic => panic!("fake reader panic during enumeration"),
            }
        }
        fn assembly_auto_opens(&self) -> Result<Vec<String>, ImportError> {
            Ok(vec![])
        }
        fn fsharp_resources(&self) -> Result<Vec<FSharpResource>, ImportError> {
            Ok(vec![])
        }
    }

    /// A minimal public, non-generic top-level class entity at
    /// `namespace::name`. Every model field not load-bearing for the index is
    /// defaulted; built in one place (not a 25-field literal per test) so a new
    /// `Entity` field is a one-line edit here.
    fn minimal_entity(namespace: &[&str], name: &str) -> Entity {
        Entity {
            extension_member_names: Vec::new(),
            union_case_names: None,
            static_extension_member_names: Vec::new(),
            is_extension_container: false,
            assembly: fake_identity(),
            namespace: namespace.iter().map(|s| s.to_string()).collect(),
            name: name.to_string(),
            kind: EntityKind::Class,
            access: Access::Public,
            generic_parameters: vec![],
            base_type: None,
            interfaces: vec![],
            members: vec![],
            skipped_members: vec![],
            method_def_tokens: vec![],
            is_sealed: false,
            nested_types: vec![],
            is_readonly: false,
            is_byref_like: false,
            is_struct: false,
            is_auto_open: false,
            is_require_qualified_access: false,
            is_no_equality: false,
            is_no_comparison: false,
            is_structural_equality: false,
            is_structural_comparison: false,
            is_allow_null_literal: false,
            obsolete: None,
            experimental: None,
            default_member: None,
            compiler_feature_required: vec![],
            source_name: None,
            custom_attrs: vec![],
            abbreviation_target: None,
        }
    }

    #[test]
    fn catch_reader_panic_converts_panic_to_none() {
        let path = Path::new("fake.dll");
        // A panicking op degrades to `None` rather than unwinding the caller.
        let panicked: Option<()> = catch_reader_panic(path, "parse", || panic!("boom"));
        assert!(panicked.is_none(), "a reader panic must degrade to None");
        // A non-panicking op passes its value straight through.
        let ok = catch_reader_panic(path, "parse", || 7u32);
        assert_eq!(ok, Some(7), "a successful op must pass its value through");
    }

    #[test]
    fn enumerate_view_catching_skips_error_and_panic_views() {
        let path = Path::new("fake.dll");

        let ok = FakeView::new(Enumerate::Ok(vec![minimal_entity(&["Demo"], "Thing")]));
        let got = enumerate_view_catching(path, &ok).expect("Ok view enumerates");
        assert_eq!(got.entities.len(), 1);

        let erred = FakeView::new(Enumerate::Err);
        assert!(
            enumerate_view_catching(path, &erred).is_none(),
            "an enumerate error must skip the DLL, not propagate"
        );

        let panicked = FakeView::new(Enumerate::Panic);
        assert!(
            enumerate_view_catching(path, &panicked).is_none(),
            "an enumerate panic must skip the DLL, not crash"
        );
    }

    #[test]
    fn enumerate_view_catching_preserves_types_when_fsharp_overlay_is_skipped() {
        let path = Path::new("fake.dll");
        let skips = AssemblyProjectionSkips {
            dropped_types: vec![],
            skipped_fsharp_overlays: vec![SkippedFsharpOverlay {
                resource_name: "FSharpSignatureData.Fake".to_string(),
                overlays: vec![
                    FsharpOverlayKind::SourceName,
                    FsharpOverlayKind::Extension,
                    FsharpOverlayKind::Measure,
                ],
                reason: "fake pickle decode failure".to_string(),
            }],
            fsharp_abbreviations_unknowable: true,
            fsharp_extension_index_unknowable: true,
            fsharp_signature_non_authoritative: true,
        };
        let view = FakeView::new(Enumerate::OkWithSkips(
            vec![minimal_entity(&["Demo"], "Thing")],
            skips,
        ));

        let got = enumerate_view_catching(path, &view)
            .expect("an overlay skip is recorded degradation, not a DLL failure");
        assert_eq!(got.entities.len(), 1);
        assert_eq!(got.entities[0].name, "Thing");
        assert_eq!(
            got.abbreviation_visibility(),
            AbbreviationVisibility::Unknowable,
            "a decode-failed assembly's visibility must survive into the projection"
        );
    }

    /// The headline regression for per-DLL degradation: a view that errors and
    /// a view that panics, placed on *both* sides of a good view, must not
    /// discard the good view's types. The old all-or-nothing `from_views` lost
    /// everything on the first failure. Also pins that the per-DLL fold keeps
    /// each surviving entity's **source-DLL provenance** (what go-to-definition
    /// reads to find the member's portable PDB).
    #[test]
    fn one_failing_view_does_not_discard_the_others() {
        let views = [
            (Path::new("bad.dll"), FakeView::new(Enumerate::Err)),
            (
                Path::new("good.dll"),
                FakeView::new(Enumerate::Ok(vec![minimal_entity(&["Demo"], "Thing")])),
            ),
            (Path::new("panics.dll"), FakeView::new(Enumerate::Panic)),
        ];
        // Mirror `build_env_from_dll_paths`' per-DLL fold (which reads + parses
        // before enumerating; here the views are pre-built), tagging survivors
        // with their source path via `from_assemblies`.
        let mut assemblies: Vec<(PathBuf, Vec<Entity>, AbbreviationVisibility, Vec<String>)> =
            Vec::new();
        for (dll, v) in &views {
            if let Some(projection) = enumerate_view_catching(dll, v) {
                let visibility = projection.abbreviation_visibility();
                assemblies.push((
                    dll.to_path_buf(),
                    projection.entities,
                    visibility,
                    projection.assembly_auto_opens,
                ));
            }
        }
        let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(assemblies);
        let handle = env
            .lookup_type(&["Demo".to_string()], "Thing", 0)
            .expect("the good view's type must survive failing/panicking siblings");
        assert_eq!(env.len(), 1, "only the good view contributes");
        assert_eq!(
            env.assembly_path(handle),
            Some(Path::new("good.dll")),
            "the surviving entity keeps its source-DLL path"
        );
    }

    #[test]
    fn enumerate_dll_type_defs_skips_unreadable_and_garbage() {
        // A path that doesn't exist: the read fails and the DLL is skipped.
        let missing = Path::new("/definitely/not/a/real/path/nope.dll");
        assert!(enumerate_dll_type_defs(missing).is_none());

        // A file full of garbage bytes: the parser rejects it and the DLL is
        // skipped — no panic escapes.
        let tmp = TempDir::new().unwrap();
        let junk = tmp.path().join("junk.dll");
        write(&junk, "this is not a PE file at all");
        assert!(enumerate_dll_type_defs(&junk).is_none());
    }

    // ---- resolved_project_for ----

    #[test]
    fn resolved_project_resolves_qualified_cross_file_ref() {
        // Two-file project: `Other` references `Shared.foo`. The fold should
        // make the cross-file qualified reference resolve to an `Item` whose
        // `item_def` points back at file 0's `foo` binder.
        use borzoi_sema::Resolution;
        use rowan::TextRange;

        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs"]));
        let a_src = "module Shared\nlet foo = 1\n";
        let b_src = "module Other\nlet bar = Shared.foo\n";
        write(&tmp.path().join("A.fs"), a_src);
        write(&tmp.path().join("B.fs"), b_src);

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let resolved = sema
            .resolved_project_for(&proj, &mut ws, &HashMap::new())
            .expect("resolved project");
        assert_eq!(resolved.len(), 2);

        // The `Shared.foo` use is in file 1; find its range and look it up.
        let needle = "Shared.foo";
        let start = b_src.find(needle).unwrap();
        let range = TextRange::new(
            (start as u32).into(),
            ((start + needle.len()) as u32).into(),
        );
        let res = resolved
            .file(1)
            .resolution_at(range)
            .expect("a resolution at Shared.foo");
        assert!(matches!(res, Resolution::Item(_)), "{res:?}");

        let (file_idx, def) = resolved.item_def(res).expect("the item's def");
        assert_eq!(file_idx, 0);
        assert_eq!(def.name, "foo");
    }

    #[test]
    fn resolved_project_partial_evaluation_yields_none() {
        // Partial project (unresolved import) → `parses_for_project` already
        // refuses; the orchestrator must propagate that as `None`.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(
            &proj,
            r#"<Project>
              <Import Project="Missing.props" />
              <ItemGroup>
                <Compile Include="Lib.fs" />
              </ItemGroup>
            </Project>"#,
        );
        write(&tmp.path().join("Lib.fs"), "let x = 1\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        assert!(
            sema.resolved_project_for(&proj, &mut ws, &HashMap::new())
                .is_none()
        );
    }

    #[test]
    fn resolved_project_caches_until_invalidated() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        let file = tmp.path().join("Lib.fs");
        write(&proj, &fsproj(&["Lib.fs"]));
        write(&file, "let x = 1\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();

        let first = sema
            .resolved_project_for(&proj, &mut ws, &HashMap::new())
            .expect("first fold");
        let second = sema
            .resolved_project_for(&proj, &mut ws, &HashMap::new())
            .expect("cached fold");
        assert!(
            Arc::ptr_eq(&first, &second),
            "expected the same Arc on a cache hit"
        );

        // A text-sync invalidation forces a re-fold; the new Arc differs.
        sema.invalidate_project(&proj);
        let third = sema
            .resolved_project_for(&proj, &mut ws, &HashMap::new())
            .expect("re-fold after invalidate");
        assert!(
            !Arc::ptr_eq(&first, &third),
            "expected a fresh Arc after invalidate_project"
        );
    }

    #[test]
    fn resolved_project_reflects_buffer_overlay() {
        // Disk has `let disk = 1`; the editor buffer has `let buffer = 1`.
        // After overlaying via `docs`, the project's exports name `buffer`,
        // not `disk` — proving the fold ran against the buffer text.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        let file = tmp.path().join("Lib.fs");
        write(&proj, &fsproj(&["Lib.fs"]));
        write(&file, "let disk = 1\n");

        let mut docs = HashMap::new();
        docs.insert(
            Url::from_file_path(&file).unwrap(),
            "let buffer = 1\n".to_string(),
        );

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let resolved = sema
            .resolved_project_for(&proj, &mut ws, &docs)
            .expect("resolved project");
        let names: Vec<_> = resolved
            .file(0)
            .exports()
            .iter()
            .map(|e| e.name().to_string())
            .collect();
        assert_eq!(names, vec!["buffer".to_string()]);
    }

    // ---- incremental resolution (stage 2) ----

    /// The incremental fold, driven through the real caches (parse cache +
    /// text-sync invalidation), returns exactly what a cold fold of the same
    /// buffers would — and it *reuses* the right files. A genuinely body-only
    /// edit (exports byte-identical) recomputes only the edited file and reuses
    /// the rest; an export-changing edit recomputes the suffix. Both are asserted
    /// via the per-fold reuse count, not merely that the incremental path fired.
    #[test]
    fn incremental_fold_matches_cold_after_edits() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs", "C.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
        write(&tmp.path().join("B.fs"), "module B\nlet y = A.x\n");
        write(&tmp.path().join("C.fs"), "module C\nlet z = B.y\n");
        let a_uri = Url::from_file_path(tmp.path().join("A.fs")).unwrap();
        let b_uri = Url::from_file_path(tmp.path().join("B.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs: HashMap<Url, String> = HashMap::new();
        sema.resolved_project_for(&proj, &mut ws, &docs)
            .expect("warm initial");

        // Body-only edit to B: `A.x` -> `(A.x)`. B's export `y` is byte-identical
        // (same path / ItemId / accessibility), so the fold recomputes only B and
        // reuses A (unchanged prefix) and C (export-neutral suffix).
        docs.insert(b_uri.clone(), "module B\nlet y = (A.x)\n".to_string());
        sema.invalidate_project(&proj);
        sema.resolved_project_for(&proj, &mut ws, &docs)
            .expect("rebuild after body edit");
        assert_eq!(sema.incremental_fold_count(), 1, "incremental path taken");
        assert_eq!(
            sema.last_fold_reused_files(),
            2,
            "a body-only edit to B reuses A and C, recomputing only B"
        );

        // Export-changing edit to A: item bases shift, so B and C must be
        // recomputed too — nothing after A is reused.
        docs.insert(
            a_uri.clone(),
            "module A\nlet w = 0\nlet x = 1\n".to_string(),
        );
        sema.invalidate_project(&proj);
        let warm = sema
            .resolved_project_for(&proj, &mut ws, &docs)
            .expect("rebuild after export edit");
        assert_eq!(
            sema.incremental_fold_count(),
            2,
            "incremental path taken again"
        );
        assert_eq!(
            sema.last_fold_reused_files(),
            0,
            "an export change to A recomputes the whole suffix"
        );

        // Cold: same final buffers, fresh caches.
        let mut cold_ws = Workspace::default();
        let mut cold_sema = SemanticState::new();
        let cold = cold_sema
            .resolved_project_for(&proj, &mut cold_ws, &docs)
            .expect("cold final");
        assert_eq!(
            cold_sema.incremental_fold_count(),
            0,
            "a first fold has no prev base and must be cold"
        );

        assert_eq!(
            *warm, *cold,
            "the incremental fold must equal a cold fold of the same buffer state"
        );
    }

    /// A stale-suffix guard: removing a cross-file export must invalidate the
    /// downstream file's reuse. `B` references `A.x`; after `A.x` is removed the
    /// incrementally-refolded `B` must no longer resolve the reference to a
    /// (now-nonexistent) `Item` — proving the suffix was recomputed, not reused.
    #[test]
    fn incremental_fold_drops_stale_cross_file_reference() {
        use borzoi_sema::Resolution;
        use rowan::TextRange;

        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
        let b_src = "module B\nlet y = A.x\n";
        write(&tmp.path().join("B.fs"), b_src);
        let a_uri = Url::from_file_path(tmp.path().join("A.fs")).unwrap();

        let range = {
            let start = b_src.find("A.x").unwrap();
            TextRange::new((start as u32).into(), ((start + "A.x".len()) as u32).into())
        };

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let warm0 = sema
            .resolved_project_for(&proj, &mut ws, &HashMap::new())
            .expect("warm");
        assert!(
            matches!(
                warm0.file(1).resolution_at(range),
                Some(Resolution::Item(_))
            ),
            "precondition: A.x resolves cross-file to an Item"
        );

        // Remove A.x through a buffer overlay and re-fold incrementally.
        let mut docs = HashMap::new();
        docs.insert(a_uri, "module A\nlet other = 1\n".to_string());
        sema.invalidate_project(&proj);
        let warm1 = sema
            .resolved_project_for(&proj, &mut ws, &docs)
            .expect("re-fold");
        assert_eq!(
            sema.incremental_fold_count(),
            1,
            "the re-fold took the incremental path"
        );
        let res = warm1.file(1).resolution_at(range);
        assert!(
            !matches!(res, Some(Resolution::Item(_))),
            "stale cross-file Item survived the removal of A.x: {res:?}"
        );
    }

    /// A referenced-assembly change ([`SemanticState::invalidate_assembly_state`])
    /// drops the incremental base, so the next fold is cold — the env it would
    /// reuse against is gone. Guards the `Arc::ptr_eq` env precondition at the
    /// cache-lifecycle level.
    #[test]
    fn assembly_state_invalidation_forces_cold_refold() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["Lib.fs"]));
        write(&tmp.path().join("Lib.fs"), "module L\nlet x = 1\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let docs = HashMap::new();
        sema.resolved_project_for(&proj, &mut ws, &docs)
            .expect("initial");
        assert_eq!(
            sema.prev_resolved.len(),
            1,
            "the initial fold stores an incremental base"
        );

        sema.invalidate_assembly_state();
        assert_eq!(
            sema.prev_resolved.len(),
            0,
            "assembly invalidation clears the incremental base"
        );

        let before = sema.incremental_fold_count();
        sema.resolved_project_for(&proj, &mut ws, &docs)
            .expect("re-fold");
        assert_eq!(
            sema.incremental_fold_count(),
            before,
            "a fold with no prev base must be cold, not incremental"
        );
        assert_eq!(
            sema.prev_resolved.len(),
            1,
            "the cold re-fold repopulates the incremental base"
        );
    }

    // ---- Compile-index resolution slice ----

    /// A single-file request folds only the Compile **prefix** up to that file:
    /// `resolved_prefix_and_env_for(project, k)` returns a project of length
    /// exactly `k + 1` (the suffix is never folded), whose per-file results are
    /// identical to a full cold fold's. A deeper request extends the prefix; a
    /// shallower one is served from the (now deeper) cache.
    #[test]
    fn resolved_prefix_folds_only_up_to_the_requested_index() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs", "C.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
        write(&tmp.path().join("B.fs"), "module B\nlet y = A.x\n");
        write(&tmp.path().join("C.fs"), "module C\nlet z = B.y\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let docs = HashMap::new();

        // Request file 0 (A): folds only file 0, so C and B are never resolved.
        let (p0, _) = sema
            .resolved_prefix_and_env_for(&proj, 0, &mut ws, &docs)
            .expect("prefix up to 0");
        assert_eq!(
            p0.len(),
            1,
            "a request for file 0 folds only the prefix [0]"
        );

        // Request file 1 (B): extends the cached prefix to length 2.
        let (p1, _) = sema
            .resolved_prefix_and_env_for(&proj, 1, &mut ws, &docs)
            .expect("prefix up to 1");
        assert_eq!(
            p1.len(),
            2,
            "a request for file 1 extends the prefix to [0, 1]"
        );

        // The prefix's per-file results equal a full cold fold's.
        let mut cold_ws = Workspace::default();
        let mut cold_sema = SemanticState::new();
        let full = cold_sema
            .resolved_project_for(&proj, &mut cold_ws, &docs)
            .expect("full fold");
        assert_eq!(full.len(), 3);
        assert_eq!(
            p1.file(0),
            full.file(0),
            "prefix file 0 matches the full fold"
        );
        assert_eq!(
            p1.file(1),
            full.file(1),
            "prefix file 1 matches the full fold"
        );

        // A request within the cached prefix is a hit — it serves the (deeper)
        // cached project rather than re-folding.
        let (p0b, _) = sema
            .resolved_prefix_and_env_for(&proj, 0, &mut ws, &docs)
            .expect("prefix up to 0, cached");
        assert!(
            !p0b.is_empty(),
            "a shallow request is served by the deeper cached prefix"
        );
        assert_eq!(p0b.file(0), full.file(0));

        // The full method folds the whole project and still agrees.
        let (whole, _) = sema
            .resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("whole project");
        assert_eq!(whole.len(), 3);
        assert_eq!(whole.file(2), full.file(2));
    }

    /// The slice composes with stage-2 incremental reuse: after a body-only edit
    /// to an early file, a prefix request re-resolves only the edited file and
    /// reuses the rest of the prefix — and never folds the suffix.
    #[test]
    fn prefix_request_after_edit_reuses_the_unchanged_prefix() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs", "C.fs", "D.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
        write(&tmp.path().join("B.fs"), "module B\nlet y = A.x\n");
        write(&tmp.path().join("C.fs"), "module C\nlet z = B.y\n");
        write(&tmp.path().join("D.fs"), "module D\nlet w = C.z\n");
        let c_uri = Url::from_file_path(tmp.path().join("C.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs: HashMap<Url, String> = HashMap::new();

        // Warm: request file 2 (C) — folds prefix [0, 1, 2], not D.
        let (p, _) = sema
            .resolved_prefix_and_env_for(&proj, 2, &mut ws, &docs)
            .expect("warm prefix up to 2");
        assert_eq!(p.len(), 3, "prefix up to C excludes D");

        // Body-only edit to C, then request C's prefix again.
        docs.insert(c_uri, "module C\nlet z = (B.y)\n".to_string());
        sema.invalidate_project(&proj);
        let (p2, _) = sema
            .resolved_prefix_and_env_for(&proj, 2, &mut ws, &docs)
            .expect("prefix up to 2 after edit");
        assert_eq!(
            p2.len(),
            3,
            "still only the prefix [0, 1, 2]; D never folded"
        );
        assert_eq!(
            sema.incremental_fold_count(),
            1,
            "the re-fold was incremental"
        );
        assert_eq!(
            sema.last_fold_reused_files(),
            2,
            "A and B reused; only the edited C recomputed (D was never in scope)"
        );
    }

    /// A shallow prefix request must not discard a deeper incremental base: after
    /// a full fold, an early-file edit, and a *prefix* token request for that
    /// file, a later full-project request must still reuse the unchanged suffix
    /// (only the edited file re-resolves) rather than re-folding it from scratch.
    #[test]
    fn shallow_prefix_request_keeps_the_deeper_incremental_base() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs", "C.fs", "D.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
        write(&tmp.path().join("B.fs"), "module B\nlet y = A.x\n");
        write(&tmp.path().join("C.fs"), "module C\nlet z = B.y\n");
        write(&tmp.path().join("D.fs"), "module D\nlet w = C.z\n");
        let a_uri = Url::from_file_path(tmp.path().join("A.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs: HashMap<Url, String> = HashMap::new();

        // Warm the FULL project (as a hover/references request would).
        let (full, _) = sema
            .resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("warm full");
        assert_eq!(full.len(), 4);

        // Body-only edit to A (file 0), then a *prefix* token request for A —
        // folds only file 0, but must keep the len-4 base.
        docs.insert(a_uri, "module A\nlet x = (1)\n".to_string());
        sema.invalidate_project(&proj);
        let (p, _) = sema
            .resolved_prefix_and_env_for(&proj, 0, &mut ws, &docs)
            .expect("prefix up to 0 after edit");
        assert_eq!(p.len(), 1, "the token request folds only the edited file");

        // Now a full-project request: it must reuse B, C, D (unchanged across the
        // export-neutral edit) and re-resolve only A — not re-fold the suffix.
        let (full2, _) = sema
            .resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("full after prefix");
        assert_eq!(full2.len(), 4);
        assert_eq!(
            sema.last_fold_reused_files(),
            3,
            "the deeper base survived the shallow request: B, C, D reused, only A re-resolved"
        );
        // And it still equals a cold fold of the same buffers.
        let mut cold_ws = Workspace::default();
        let mut cold_sema = SemanticState::new();
        let cold = cold_sema
            .resolved_project_for(&proj, &mut cold_ws, &docs)
            .expect("cold");
        assert_eq!(*full2, *cold);
    }

    // ---- semantic-tokens refresh signal ----

    /// An edit flags a refresh (an already-open later buffer must re-request
    /// tokens) — but only **once** per edit: the refresh's own re-requests fold
    /// incrementally again, and must not flag another refresh (the loop guard).
    #[test]
    fn edit_flags_a_refresh_once_and_re_requests_do_not_loop() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs", "C.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
        write(&tmp.path().join("B.fs"), "module B\nlet y = A.x\n");
        write(&tmp.path().join("C.fs"), "module C\nlet z = B.y\n");
        let a_uri = Url::from_file_path(tmp.path().join("A.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs: HashMap<Url, String> = HashMap::new();

        // Warm the whole project (as an open editor would): a cold fold, no refresh.
        sema.resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("warm");
        assert!(!sema.take_wants_refresh(), "a cold fold flags no refresh");

        // Edit A, then a token request for A itself (an incremental fold).
        docs.insert(a_uri, "module A\nlet x = 1\nlet extra = 2\n".to_string());
        sema.invalidate_project(&proj);
        sema.resolved_prefix_and_env_for(&proj, 0, &mut ws, &docs)
            .expect("prefix A after edit");
        assert!(sema.take_wants_refresh(), "the edit flags a refresh");
        assert!(!sema.take_wants_refresh(), "take clears the flag");

        // The refresh makes the client re-request the later buffer B; that
        // incremental fold must NOT flag another refresh.
        sema.resolved_prefix_and_env_for(&proj, 1, &mut ws, &docs)
            .expect("prefix B after refresh");
        assert!(
            !sema.take_wants_refresh(),
            "one refresh per edit — the re-requests must not loop"
        );
    }

    /// The refresh is *conservative*: even a body-only edit (which can't change a
    /// later buffer's tokens) flags a refresh, because deciding precisely would
    /// mean re-deriving the whole downstream projection. The cost is bounded — the
    /// client diffs the re-requested tokens and re-renders only what changed.
    #[test]
    fn any_edit_flags_a_refresh_conservatively() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs", "C.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
        write(&tmp.path().join("B.fs"), "module B\nlet y = A.x\n");
        write(&tmp.path().join("C.fs"), "module C\nlet z = B.y\n");
        let a_uri = Url::from_file_path(tmp.path().join("A.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs: HashMap<Url, String> = HashMap::new();
        sema.resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("warm");
        assert!(
            !sema.take_wants_refresh(),
            "the cold warm fold flags nothing"
        );

        // Body-only edit to A: `1` -> `(1)`. Still flags a refresh (conservative).
        docs.insert(a_uri, "module A\nlet x = (1)\n".to_string());
        sema.invalidate_project(&proj);
        sema.resolved_prefix_and_env_for(&proj, 0, &mut ws, &docs)
            .expect("prefix A after body edit");
        assert!(
            sema.take_wants_refresh(),
            "any incremental fold (any edit) conservatively flags a refresh"
        );
    }

    /// The refresh tracks *invalidation*, not fold reuse: extending a cached
    /// prefix to a deeper file (a hover / definition after the token request) is
    /// an incremental fold with **no** intervening edit, and must not refresh.
    #[test]
    fn extending_a_prefix_without_an_edit_flags_no_refresh() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs", "C.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
        write(&tmp.path().join("B.fs"), "module B\nlet y = A.x\n");
        write(&tmp.path().join("C.fs"), "module C\nlet z = B.y\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let docs = HashMap::new();

        // Token request for the first file: a short prefix, cold, no refresh.
        sema.resolved_prefix_and_env_for(&proj, 0, &mut ws, &docs)
            .expect("prefix A");
        assert!(!sema.take_wants_refresh());

        // Now a deeper request (hover in C) *extends* the prefix — an incremental
        // fold — but nothing was edited, so no refresh is owed.
        sema.resolved_prefix_and_env_for(&proj, 2, &mut ws, &docs)
            .expect("extend to C");
        assert!(
            !sema.take_wants_refresh(),
            "extending a prefix without an edit must not refresh"
        );
    }

    /// The refresh survives a **cold** fold: after a structural invalidation
    /// clears the incremental base, a text edit folds cold — yet an open later
    /// buffer is still stale and must be refreshed. (`took_incremental` would miss
    /// this; the invalidation-set dirty flag doesn't.)
    #[test]
    fn edit_after_structural_invalidation_still_refreshes() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs", "C.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
        write(&tmp.path().join("B.fs"), "module B\nlet y = A.x\n");
        write(&tmp.path().join("C.fs"), "module C\nlet z = B.y\n");
        let a_uri = Url::from_file_path(tmp.path().join("A.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs: HashMap<Url, String> = HashMap::new();
        sema.resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("warm");
        assert!(!sema.take_wants_refresh());

        // A structural change clears the incremental base, then a text edit.
        sema.invalidate_all();
        docs.insert(a_uri, "module A\nlet x = 1\nlet extra = 2\n".to_string());
        sema.invalidate_project(&proj);
        // This fold is cold (no prev base), but the edit still owes a refresh.
        sema.resolved_prefix_and_env_for(&proj, 0, &mut ws, &docs)
            .expect("cold fold after structural invalidation + edit");
        assert!(
            sema.take_wants_refresh(),
            "a cold fold after an edit must still refresh open later buffers"
        );
    }

    /// An invalidation that is *never* followed by a fold still owes a refresh —
    /// a `didClose` restoring disk text, or a watched-file change, only touches
    /// the caches, but an open later buffer's tokens can still go stale. The flag
    /// is set at the invalidation, not deferred to a (possibly-absent) fold.
    #[test]
    fn invalidation_without_a_fold_still_owes_a_refresh() {
        let mut sema = SemanticState::new();
        assert!(!sema.take_wants_refresh(), "nothing owed initially");

        // No fold anywhere — just the invalidation the notification path performs.
        sema.invalidate_file(Path::new("/some/project/A.fs"));
        assert!(
            sema.take_wants_refresh(),
            "a fold-less invalidation still owes a refresh"
        );

        // A structural / referenced-assembly change likewise.
        sema.invalidate_all();
        assert!(sema.take_wants_refresh(), "invalidate_all owes a refresh");
        sema.invalidate_assembly_state();
        assert!(
            sema.take_wants_refresh(),
            "invalidate_assembly_state owes a refresh"
        );
    }

    #[test]
    fn pdb_image_cache_computes_once_and_caches_the_negative() {
        let mut s = SemanticState::default();
        let calls = std::cell::Cell::new(0);
        let dll = Path::new("/pkg/A.dll");
        let img: Arc<[u8]> = Arc::from(vec![1u8, 2, 3]);

        let first = s.pdb_image(dll, || {
            calls.set(calls.get() + 1);
            Some(img.clone())
        });
        assert_eq!(first.as_deref(), Some(&[1u8, 2, 3][..]));
        // A second lookup is served from the cache — `compute` must not run.
        let second = s.pdb_image(dll, || panic!("cache hit should not recompute"));
        assert_eq!(second.as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(calls.get(), 1);

        // A DLL with no PDB caches the `None` so it isn't re-read each time.
        let other = Path::new("/pkg/B.dll");
        assert!(
            s.pdb_image(other, || {
                calls.set(calls.get() + 1);
                None
            })
            .is_none()
        );
        assert!(
            s.pdb_image(other, || panic!("negative result should be cached"))
                .is_none()
        );
        assert_eq!(calls.get(), 2);

        // `invalidate_all` (a restore / `.fsproj` change) drops the cache.
        s.invalidate_all();
        assert_eq!(
            s.pdb_image(dll, || {
                calls.set(calls.get() + 1);
                Some(img.clone())
            })
            .as_deref(),
            Some(&[1u8, 2, 3][..])
        );
        assert_eq!(calls.get(), 3);
    }
}
