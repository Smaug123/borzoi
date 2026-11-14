//! Per-project parsing and (later stages) name resolution against
//! `borzoi-sema`. The `docs`-aware companion of
//! [`crate::workspace::Workspace`]: the workspace owns the `.fsproj` cache,
//! [`SemanticState`] owns whatever needs to see editor-buffer overlays on top.
//!
//! Stage 2 surfaces only [`SemanticState::parses_for_project`] — the
//! Compile-ordered `Vec<ImplFile>` for a project, preferring the in-memory
//! buffer of each member file over its on-disk text. Stage 3 adds an
//! `AssemblyEnv` cache; Stage 4 the orchestrator that runs
//! [`borzoi_sema::resolve_project`] and caches the result. Every cache is
//! confirmed against its inputs when read (`docs/incremental-lsp-plan.md`,
//! Stage 7), so nothing here is told when an input changes.

use crate::inbox::{Superseded, Watch, never_superseded};
use crate::observed::Unobserved;
use crate::parses::{ParseMemo, SourceParse};
use crate::vfs::Vfs;
use borzoi_cst::parser::FileKind;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use borzoi_assembly::doc_id::DocIdDialect;
use borzoi_assembly::{AssemblyIdentity, Ecma335Assembly, EcmaView, Entity, ImportError, Version};
use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_msbuild::observed::{self as msbuild_observed, EntryKind, Observer, ReadSet};
use borzoi_msbuild::{ItemKind, UntrustedCause};
use borzoi_sema::{
    AbbreviationVisibility, AssemblyEnv, InferredProject, ProjectFile, ResolvedProject, SourceFile,
    SyntaxRecovery,
};

use crate::assembly_cache::AssemblyCache;
use crate::cst_panic_safe::{parse_sig_with_symbols, parse_with_symbols};
use crate::paths::{lexically_normalize, paths_equal};
use crate::project_assets::{
    resolve_assemblies_for_tfm, resolve_assemblies_root_only, resolve_transitive_project_tfms,
};
use crate::project_deferral::{FoldOutcome, FoldRefusal};
use crate::project_graph::{NodeTfm, ProjectGraph, ProjectKind};
use crate::restore::{RestoreOutcome, restore_to_scratch_assemblies};
use crate::sdk_discovery::SdkDiscoveryEnv;
use crate::sidecar_manager::SidecarManager;
use crate::workspace::{EnvChecks, EvaluationId, ProjectsGeneration, ServedTfm, Workspace};
use crate::xml_doc::inherit::{Expansion, expand};
use crate::xml_doc::key::DocTarget;
use crate::xml_doc::lookup::{DocLookup, DocSources};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ReferencedAssemblyProjection {
    entities: Vec<Entity>,
    /// [`AssemblyProjectionSkips::fsharp_abbreviations_unknowable`](borzoi_assembly::AssemblyProjectionSkips::fsharp_abbreviations_unknowable) for this
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
    /// [`AssemblyProjectionSkips::fsharp_extension_index_unknowable`](borzoi_assembly::AssemblyProjectionSkips::fsharp_extension_index_unknowable) for this DLL —
    /// its F#-native extension-member index could not be built (absent/undecodable
    /// pickle). Folded into the env's per-assembly extension-knowability so a broken
    /// FSharp.Core pickle (abbreviation-exempt but extension-blind) still defers the
    /// name-keyed gate. `#[serde(default)]` for old cache entries.
    #[serde(default)]
    fsharp_extension_index_unknowable: bool,
    /// [`AssemblyProjectionSkips::fsharp_signature_non_authoritative`](borzoi_assembly::AssemblyProjectionSkips::fsharp_signature_non_authoritative) for this DLL —
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
    /// The DLL's manifest identity (`EcmaView::identity`), carried so the env can
    /// register a **rootless** projection's DLL name for referenced-CCU
    /// uniqueness: with no surviving roots the identity cannot be derived from an
    /// entity, and a same-named sibling would otherwise let an abbreviation target
    /// bind into the wrong DLL (issue #150 / codex P2). `#[serde(default)]` so an
    /// old cache entry (predating this field) deserialises as `None` — the env
    /// then falls back to the first root, i.e. the pre-fix behaviour.
    #[serde(default)]
    manifest_identity: Option<AssemblyIdentity>,
    /// The DLL's forwarder `ExportedType` rows (`EcmaView::type_forwarders`) —
    /// how an abbreviation-target chase continues out of a facade
    /// (`netstandard`). `#[serde(default)]` for old cache entries.
    #[serde(default)]
    type_forwarders: Vec<borzoi_assembly::TypeForwarder>,
    /// The DLL's [`DocIdDialect`] (`EcmaView::doc_id_dialect`), or `None` when it
    /// could not be read — which declines every member's documentation key.
    /// Deliberately not `#[serde(default)]`: an entry that predates the field
    /// must miss rather than read as unknown-but-cached.
    doc_id_dialect: Option<DocIdDialect>,
    /// Whether the manifest references other assemblies
    /// (`EcmaView::assembly_refs`), which decides the core library Roslyn's
    /// special types bind into. `None` when unread (a reader panic).
    has_assembly_references: Option<bool>,
    /// `EcmaView::qualified_names`; `None` when unread.
    qualified_names: Option<Vec<String>>,
}

/// Why one referenced DLL contributed nothing to the [`AssemblyEnv`]. The two
/// grades cost *different* things, which is the whole reason to distinguish
/// them: only one of them makes the rest of the env unsafe to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipCause {
    /// The file provably holds no ECMA-335 metadata — its PE header declares no
    /// CLI data directory ([`ImportError::NotAManagedAssembly`]). Native DLLs
    /// (`msdia140.dll`, `git2-*.dll`) ride along in NuGet packages and land in
    /// reference position; `fsc` refuses them too. Nothing is lost and nothing
    /// becomes uncertain: a file with no manifest has no assembly name to
    /// collide with a referenced CCU, declares no type that could shadow one,
    /// and carries no assembly-level `[<AutoOpen>]`.
    NotAManagedAssembly,
    /// Every other skip: unreadable bytes, a layout the reader does not support,
    /// an enumeration error, a reader panic. Metadata is *there* and we could
    /// not read it, so this DLL's manifest name and types are **unknown** — it
    /// could be the assembly a referenced CCU names, could declare a type that
    /// shadows one we did read, and could carry an assembly-level
    /// `[<AutoOpen>]`. Marks the env's identity set incomplete.
    UnknownIdentity,
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
/// text the parser saw — buffer if open, disk otherwise. A `.fs` item parses
/// under the implementation grammar, a `.fsi` under the signature grammar
/// ([`SourceFile`]), and each entry carries its
/// [`QualifiedNameOfFile`](borzoi_sema::QualifiedNameOfFile) (the sig ↔ impl
/// pairing key, computed over the whole Compile order). A file that couldn't
/// be read or didn't parse to its grammar's root is omitted; the file's index
/// in `files`/`paths`/`texts` is therefore not necessarily its index in the
/// original `items` list (D5 "under-resolve, never wrong").
///
/// `texts` is also what makes a cached `ProjectParses` checkable: each is the
/// [`Vfs`]'s own allocation for its item, so
/// `SemanticState::validate_cached_fold` can ask the `Vfs` for every item
/// again and compare by pointer, falling back to the bytes.
#[derive(Debug, Clone)]
pub struct ProjectParses {
    pub files: Vec<ProjectFile>,
    pub paths: Vec<PathBuf>,
    pub texts: Vec<Arc<str>>,
}

/// A project's cached [`ProjectParses`], what they read from the project's
/// evaluation, the evaluation last found to give that, and the generation
/// in which their texts were last found current
/// ([`SemanticState::validate_cached_fold`]).
#[derive(Debug)]
struct CachedParses {
    parses: ProjectParses,
    /// Everything the parses read from the evaluation ([`ParseInputs`]):
    /// they stand while the evaluation the workspace serves gives these.
    inputs: ParseInputs,
    /// The project evaluation last found to give `inputs`
    /// ([`Workspace::evaluation_id`]). While the workspace serves it, the
    /// inputs need no comparing; a new evaluation is compared once.
    evaluation: Option<EvaluationId>,
    /// The [`Vfs::generation`] that last validated `parses`; `None` before
    /// any has, or when the last validation ran outside a generation.
    validated_in: Option<u64>,
}

impl ProjectParses {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }
}

/// One Compile item's parse as the fold reads it (see
/// [`SemanticState::file_parses`], which holds a list of these per path): the
/// memo's parse ([`ParseMemo`]), and what the fold derived from it, stored
/// with the one further input that decided it, the owning project's
/// `LangVersion` *provenance* trust. A [`build_parses`] cache hit requires
/// the parse's inputs (source text, `#if` symbols, language version) and the
/// trust to match, so a variant is a pure function of its inputs: it can
/// never be served stale, nor bypass the version-boundary gate.
/// The trust matters because a hit *skips* that gate, whose accept/refuse
/// verdict depends on `lang_version_untrusted` — and one source file shared by
/// two projects can carry different provenance at the same effective version, so
/// a trusted project's accepted parse must not satisfy an untrusted one. See
/// [`SemanticState::file_parses`].
#[derive(Debug)]
struct CachedParse {
    /// The parse: its text, `#if` symbols and language version are the rest
    /// of the key.
    parse: Arc<SourceParse>,
    /// Whether the owning project's `LangVersion` provenance was untrusted when
    /// this tree was accepted. Part of the key because a hit skips the
    /// version-boundary gate, whose verdict is a function of this flag (not of
    /// the file's text): without it, a trusted project's accepted parse of a
    /// straddling file could wrongly satisfy an untrusted project that a cold
    /// build refuses.
    lang_version_untrusted: bool,
    /// The allocation of `parse`'s text the `Vfs` last handed out, so that the
    /// next comparison with it is a pointer test.
    text: Arc<str>,
    /// The reusable parsed file — impl or signature, per the path's extension
    /// (a syntax handle onto `parse`'s tree; a hit clones it — an `Arc` bump,
    /// not a re-parse).
    file: SourceFile,
    /// What the parser had to recover from in `file`. Cached beside the tree
    /// because the `Parse` that produced both is dropped here, and a hit that
    /// served the tree without it would leave the fold unable to prove any
    /// declaration parsed cleanly — silently costing every annotation in the
    /// file its type.
    recovery: SyntaxRecovery,
}

/// The previous Compile-order fold of a project, kept so the *next* fold can be
/// computed incrementally ([`borzoi_sema::resolve_project_incremental`]) rather
/// than cold. Stored per project in [`SemanticState::prev_resolved`].
///
/// Holds the project's **full** Compile list of [`ProjectFile`]s at fold time
/// (so the incremental fold can compare each new file against the tree that
/// produced its previous resolution, by handle identity, and re-derive the
/// sig ↔ impl pairing over the whole list — `resolved` covers only its
/// folded prefix), the result, and the assembly env [`Arc`] the fold ran
/// against. The env Arc is the reuse *precondition witness*: reusing a
/// file's resolution is sound only against the same env it was resolved with, so
/// [`SemanticState::resolved_project_for`] reuses this only when the current env
/// is [`Arc::ptr_eq`] to [`Self::env`]. A rebuilt env — its inputs moved, an
/// event dropped it, or a transient-sidecar recovery — is a fresh Arc, so the pointer check fails and the fold falls back to cold. This
/// makes "same env" machine-checked rather than a reasoned invariant.
/// One project's inference fold ([`SemanticState::inferred_prefix_for`]),
/// with the env it was inferred against. It answers for a later request only
/// while the env is the same allocation, and only for the leading files whose
/// resolution is too ([`InferredProject::shared_prefix`]): a file's types
/// depend on its resolution, the env, and what every earlier file published.
/// Past those, the next fold reuses what the cutoff allows
/// ([`borzoi_sema::infer_project_reusing`]).
#[derive(Debug)]
struct CachedInference {
    env: Arc<AssemblyEnv>,
    inferred: Arc<InferredProject>,
}

#[derive(Debug)]
struct PrevFold {
    files: Vec<ProjectFile>,
    resolved: Arc<ResolvedProject>,
    env: Arc<AssemblyEnv>,
}

/// The semantic-layer caches that depend on editor-buffer overlays. Held on
/// [`crate::server::State`] alongside the [`Workspace`] so handlers can
/// thread the docs map through. Mutation is single-threaded — same
/// assumption the request loop already makes.
#[derive(Debug, Default)]
pub struct SemanticState {
    /// Per-project compile-ordered parses, keyed by [`ProjectId`].
    ///
    /// **Validated, not invalidated** (`docs/incremental-lsp-plan.md`, Stage
    /// 7): every read of an entry first checks that the evaluation the
    /// workspace serves gives it the inputs it was built from
    /// ([`ParseInputs`], compared once per new evaluation), and, once per
    /// generation, that every Compile item's text is the one it was built
    /// from ([`Self::validate_cached_fold`]). So an edit, a write on disk
    /// with or without a watch event, and a moved evaluation are all seen by
    /// the next read, and nothing has to call an invalidator.
    project_parses: HashMap<ProjectId, CachedParses>,
    /// The last fold refusal per project, keyed by [`ProjectId`], with the
    /// project evaluation the refused fold read.
    ///
    /// Written **only** by [`Self::fold`], the one place the fold runs, so it
    /// cannot disagree with what the fold actually did. A refusal speaks for
    /// the project only while the workspace serves the evaluation it was
    /// refused under ([`Self::fold_outcome`] reads
    /// [`FoldOutcome::Unknown`] otherwise): reporting it against a project
    /// evaluated since would describe a project that no longer exists in that
    /// form. A refused project caches no parses, so every request that needs
    /// its fold folds it again and records what it finds.
    ///
    /// Read (not drained) by [`Self::fold_outcome`], which is what lets the
    /// shell's message be a function of current state rather than of the event
    /// that provoked it.
    observed_fold_refusals: HashMap<ProjectId, (FoldRefusal, Option<EvaluationId>)>,
    /// Every source's parses ([`ParseMemo`]): the fold, the syntactic
    /// diagnostics and the single-file handlers each parse through it, so a
    /// text read by all of them is parsed once.
    parses: ParseMemo,
    /// Per-**file** fold cache — the parsed [`ImplFile`]s for each Compile item
    /// and what the fold derived from each, keyed by path. Lets
    /// [`build_parses`] reuse every unchanged file's tree (a syntax handle
    /// clone) and ask [`Self::parses`] only for the file that changed, so a
    /// keystroke no longer re-parses the whole Compile order.
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
    /// Correctness never rests on eviction: the input comparison in
    /// `build_parses` re-parses anything whose inputs no longer match. The
    /// table holds one variant per path and distinct settings tuple the
    /// session has folded.
    file_parses: HashMap<PathBuf, Vec<CachedParse>>,
    /// Per-project flattened name index over referenced-assembly entities,
    /// keyed by the **full value-input set** `build_assembly_env` reads: the
    /// [`ProjectId`] *and* the canonicalised `dotnet_root` it was
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
    /// **Validated, not invalidated** (`docs/incremental-lsp-plan.md`, Stage
    /// 6b): each entry carries what building it read (each DLL's stamp,
    /// [`EnvEntry::contents`]) and what resolving its references read
    /// ([`EnvEntry::resolution`]), each less the contents of the installed
    /// versions it trusts, and the first lookup of it in a generation
    /// confirms them ([`Workspace::confirm_env`]). An entry a DLL of which
    /// moved is dropped, with every fold against its env, and built afresh.
    /// One whose resolution's reads moved is resolved again, and kept if its
    /// references resolve to the inputs it was built from ([`EnvInputs`]:
    /// the follow-up "cutoffs on the evaluation's consumed outputs"), so a
    /// `.fsproj` edit that moves no reference, no TFM and no restore output
    /// (a define, a comment) keeps the env. Editor edits never reach it: the
    /// env reads only disk state. A watch event drops exactly the entries
    /// whose reads it names ([`Self::hint_assembly_paths`]), which covers an
    /// in-place rewrite inside an installed version and a file system whose
    /// stamps do not move.
    ///
    /// An entry whose resolution consulted a reader that records nothing (an
    /// on-demand restore, the C# sidecar: [`EnvReads::unrecorded`]) is never
    /// trusted on its recorded reads alone. It is confirmed as any other is,
    /// and dropped besides by everything that dropped every env before reads
    /// were recorded ([`Self::invalidate_assembly_state`]): a structural or
    /// referenced-assembly (`.dll`, `.cs`, `.csproj`) watch event, and a
    /// moved [`Workspace::projects_generation`]
    /// ([`Self::sync_with_evaluations`]). The
    /// sidecar's cache is content-addressed, so that rebuild is cheap. The
    /// one exception is a *transient* sidecar transport failure, which
    /// [`Self::assembly_env_for_project`] deliberately does not cache so the
    /// next request retries.
    ///
    /// Keyed by `(project, dotnet_root, served-TFM verdict)`. The verdict
    /// selects which assets target the env was built from (fsproj 3.3c plan
    /// E3; 3.3d round 19 for the tri-state — `NoneDeclared` and `Untrusted`
    /// build different envs, so the full [`ServedTfm`] is the key, not its
    /// `Option` projection); it is a function of the project today
    /// (first-declared — [`Workspace::served_tfm_for_project`]), so the
    /// extra key component is structural preparation for a TFM-override
    /// policy and makes "different TFM ⇒ different env" machine-checkable.
    assembly_envs: HashMap<(ProjectId, Option<PathBuf>, ServedTfm), EnvEntry>,
    /// Per-project resolved sema output: the Compile-order fold of
    /// [`borzoi_sema::resolve_project`] over the project's [`ProjectParses`] against its
    /// [`AssemblyEnv`]. Wrapped in [`Arc`] so handlers can return it without
    /// holding `SemanticState` borrowed across a long query. The resolution
    /// is a function of the parses and the env, so it is dropped whenever
    /// its parses are.
    ///
    /// Served only once [`Self::validate_cached_fold`] has confirmed the
    /// parses it was folded over (it drops the fold with them) and the env's
    /// entry confirms the env (`Self::env_still_served`).
    ///
    /// Stored **paired with the exact [`AssemblyEnv`] the fold resolved
    /// against**. `Resolution::Entity` / `Resolution::Member` handles index into
    /// that env, and a re-fetched env can differ (a recovered C# sidecar DLL can
    /// sort earlier, shifting handles), so a consumer classifying those handles
    /// must use *this* env — see [`Self::resolved_project_and_env_for`].
    resolved_projects: HashMap<ProjectId, (Arc<ResolvedProject>, Arc<AssemblyEnv>)>,
    /// Per-project inference fold over the deepest resolved prefix inferred so
    /// far ([`Self::inferred_prefix_for`]). Never served stale — every hit
    /// re-checks the resolved files and env by pointer — and kept across edits
    /// so the next fold reuses the unchanged leading files. Cleared with the
    /// resolved projects to bound memory.
    inferred_projects: HashMap<ProjectId, CachedInference>,
    /// Per-project *previous* fold, kept for incremental re-resolution (stage 2).
    /// Unlike `resolved_projects`, which goes with its parses, this
    /// **survives** an edit, so after a keystroke [`Self::resolved_project_for`] can fold the
    /// project incrementally: reuse the per-file [`borzoi_sema::ResolvedFile`]s of
    /// files whose parse tree is unchanged (identity-shared through the stage-1
    /// per-file parse cache) and whose threaded prefix is unchanged, re-resolving
    /// only what the edit touched. See [`PrevFold`] for the assembly-env
    /// [`Arc::ptr_eq`] precondition that keeps the reuse sound.
    ///
    /// Cleared per env when an env is dropped (`Self::drop_env`): the env is
    /// a new Arc after that anyway, so a stale entry could never be reused —
    /// clearing is for memory, not correctness. `PrevFold::resolved` shares the same `Arc` as the matching
    /// `resolved_projects` entry, so the extra state is one `Vec<ImplFile>` of
    /// syntax handles per project.
    prev_resolved: HashMap<ProjectId, PrevFold>,
    /// Running tally of folds that took the *incremental* path (a reusable
    /// [`PrevFold`] whose env matched). A silently-cold fold — a fold that
    /// should reuse but doesn't — is a *performance* regression, not a
    /// correctness one, so no result comparison would catch it; the tests read
    /// this to assert the incremental path actually fires after an edit. Also a
    /// natural telemetry hook. Monotone; never reset.
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
    /// How many files the *most recent* inference fold
    /// ([`Self::inferred_prefix_for`]) reused rather than inferred afresh (0 for
    /// a cold one). The inference counterpart of
    /// [`Self::last_fold_reused_files`]: a body edit that changes no export's
    /// type should reuse every file but the edited one, which no comparison
    /// with a cold fold can see. Untouched by a request the cached fold
    /// answers outright.
    last_inference_reused_files: usize,
    /// A `workspace/semanticTokens/refresh` is owed to the client: a change (a
    /// text-sync edit, a watched change, a cached value found stale, an env
    /// dropped) may have staled an already-open buffer's tokens, but the
    /// client only re-requests the buffer it touched. Set at the change, not
    /// at a later fold, so a change with no following fold (a `didClose`
    /// restoring disk text, a watched-file change) still refreshes
    /// ([`Self::owe_token_refresh`]). Drained by
    /// [`Self::take_wants_refresh`] in the dispatch loop after each request *and*
    /// notification (it owns the connection and checks `refreshSupport`). A plain
    /// bool: the refresh is workspace-wide and idempotent, so coalescing several
    /// changes into one refresh is correct — and it can't loop, since the
    /// refresh's own re-requests are ordinary requests, not changes.
    wants_refresh: bool,
    /// The [`Workspace::projects_generation`] last seen
    /// ([`Self::sync_with_evaluations`]), `None` before the first. When it
    /// moves, the envs with an unrecorded reader go, as they did when a
    /// moved generation dropped every cache here; every other cache derived
    /// from an evaluation records the evaluation it was derived from.
    evaluations_seen: Option<ProjectsGeneration>,
    /// The portable-PDB *metadata image* (embedded, or a validated sidecar) for
    /// each referenced DLL go-to-definition has navigated into, keyed by DLL
    /// path, beside the DLL's stamp when the image was read. `None` caches the
    /// negative result (a DLL with no usable PDB) so a repeat lookup doesn't
    /// re-read it. Wrapped in [`Arc`] so a cache hit is a refcount bump, not a
    /// copy of the (multi-hundred-KB) image.
    ///
    /// Without this, every go-to-definition into a referenced member re-reads
    /// the whole owning DLL (FSharp.Core is ~2.4 MB) and re-extracts its PDB.
    /// An entry answers only while the DLL's stamp is the one recorded, and
    /// one recorded within the racy window of the DLL's last change is never
    /// served ([`Self::pdb_image`]). A sidecar `.pdb` is used only when it
    /// matches the DLL's debug directory, but its presence is not in the key:
    /// one added beside an unchanged DLL is seen after a referenced-assembly
    /// or structural watch event, or a moved evaluation, which clear the map
    /// ([`Self::invalidate_assembly_state`]).
    pdb_images: HashMap<PathBuf, PdbImage>,
    /// What XML-documentation lookup caches: the parsed sidecar `.xml` files
    /// hover has read, each validated against the file's `(size, mtime)` on
    /// every use (see [`DocFileCache`](crate::xml_doc::lookup::DocFileCache)),
    /// and the per-assembly key censuses and env-wide ID indices, each
    /// validated by env identity — so correctness does not depend on any
    /// invalidation.
    xml_doc: DocSources,
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
    /// `build_assembly_env`). Never spawned unless
    /// [`Self::csharp_project_references_enabled`]; left un-spawned for the
    /// common F#-only workspace even then; degrades to under-resolution when
    /// the sidecar is unavailable (D5). Not a
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
    /// Whether a C# `<ProjectReference>` may be read through the C# sidecar.
    /// Off by default — the sidecar loads each referenced `.csproj` with a
    /// design-time build, which executes that project's MSBuild targets (its
    /// `Directory.Build.targets` included) and loads its analyzers and source
    /// generators in-process, and it writes its output under
    /// `obj/borzoi/csharp-sidecar/`. Like on-demand restore, that is a
    /// *workspace trust* decision the host opts into explicitly (the server
    /// reads it from the `enableCSharpProjectReferences` initialization
    /// option), and only a host that trusts the workspace should set it. When
    /// off, the sidecar is never spawned and every C# edge grades as
    /// [`ReferenceOutcome::Unread`] with [`CSHARP_REFERENCES_DISABLED`], so the
    /// env is marked incomplete rather than silently missing the reference.
    csharp_project_references_enabled: bool,
    /// How many entries an env's name table may hold before the env is
    /// dropped ([`Self::drop_env_past_name_cap`]).
    name_table_cap: NameTableCap,
    /// Envs built in the open generation ([`Self::end_generation`]).
    envs_built: u64,
    /// Cached envs kept in the open generation by resolving their
    /// references again to the inputs they were built from
    /// ([`Self::keep_if_resolved_alike`]).
    envs_resolved_again: u64,
    /// Resolutions of a project's references for an env in the open
    /// generation ([`Self::resolve_references`]).
    env_resolutions: u64,
    /// Compile-order folds computed in the open generation
    /// ([`Self::end_generation`]).
    folds: u64,
    /// A project's cached parses that validation found stale under the
    /// evaluation they were built from, kept until the rebuild that follows
    /// compares them with what it read, to decide whether the change owes a
    /// refresh (`Self::owe_refresh_for_unannounced`).
    superseded: HashMap<ProjectId, CachedParses>,
}

/// What the semantic layer did in one generation
/// ([`SemanticState::end_generation`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SemanticCounts {
    /// What the parse memo did.
    pub parses: crate::parses::ParseCounts,
    /// Assembly envs built.
    pub envs_built: u64,
    /// Cached assembly envs kept by resolving their references again to the
    /// inputs they were built from.
    pub envs_resolved_again: u64,
    /// Resolutions of a project's references, for an env built or kept.
    pub env_resolutions: u64,
    /// Compile-order folds computed, cold or incremental.
    pub folds: u64,
}

/// The most entries, names, paths and types together, an env's tables hold
/// past a fold before the env is dropped (`docs/sema-interning-plan.md`, D2
/// and D4). The tables only grow, by every distinct name and path a fold
/// against the env meets — each prefix of a name as it is typed among them,
/// and each new arrangement of names already held — and every distinct type
/// an inference against it publishes, so a long-lived server bounds them by
/// rebuilding the env, and with it fresh tables, once a resolution fold or an
/// inference fold leaves them past this. A whole 11.8 MB source tree interns
/// about 40,000 names, so the default is reached only by a long session of
/// edits. The tables then stay below the cap plus one fold's entries.
#[derive(Debug, Clone, Copy)]
struct NameTableCap(usize);

impl Default for NameTableCap {
    fn default() -> Self {
        NameTableCap(1 << 20)
    }
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

    /// How many files the most recent inference fold reused (see
    /// [`Self::last_inference_reused_files`]).
    pub fn last_inference_reused_files(&self) -> usize {
        self.last_inference_reused_files
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
    ///
    /// The assembly envs are not keyed on this flag, so changing it drops
    /// them, as [`Self::set_csharp_project_references_enabled`] does.
    pub fn set_on_demand_restore_enabled(&mut self, enabled: bool) {
        if self.on_demand_restore_enabled != enabled {
            self.on_demand_restore_enabled = enabled;
            self.drop_every_env();
        }
    }

    /// Start a dispatch-loop generation: the parse memo's counts, the envs
    /// built and the folds computed start again from zero.
    pub fn begin_generation(&mut self) {
        self.parses.begin_generation();
        self.envs_built = 0;
        self.envs_resolved_again = 0;
        self.env_resolutions = 0;
        self.folds = 0;
    }

    /// End the generation, returning what the parse memo did, and how many
    /// envs were built and folds computed in it.
    pub fn end_generation(&mut self) -> SemanticCounts {
        SemanticCounts {
            parses: self.parses.end_generation(),
            envs_built: std::mem::take(&mut self.envs_built),
            envs_resolved_again: std::mem::take(&mut self.envs_resolved_again),
            env_resolutions: std::mem::take(&mut self.env_resolutions),
            folds: std::mem::take(&mut self.folds),
        }
    }

    /// Opt into reading C# `<ProjectReference>`s through the C# sidecar (the
    /// `enableCSharpProjectReferences` initialization option). The sidecar
    /// executes each referenced C# project's MSBuild targets and loads its
    /// analyzers and source generators, so only a host that trusts the
    /// workspace should enable it. Off by [`Self::new`], so tests, library
    /// embeddings, and un-opted hosts never spawn the sidecar; each C# edge
    /// then grades as unread and leaves the env incomplete.
    ///
    /// The assembly envs are not keyed on this flag, so changing it drops them
    /// (and everything resolved against them): an env cached under one setting
    /// must not be served under the other.
    pub fn set_csharp_project_references_enabled(&mut self, enabled: bool) {
        if self.csharp_project_references_enabled != enabled {
            self.csharp_project_references_enabled = enabled;
            self.drop_every_env();
        }
    }

    /// Something an open buffer's semantic tokens may read changed: a source
    /// buffer was opened, edited or closed, or a watch event reported a
    /// source or a project file changed. The caches need nothing (each
    /// confirms its inputs when read), but an open buffer other than the one
    /// touched may now be shown stale tokens, and the client re-asks only
    /// for the one it touched, so a `workspace/semanticTokens/refresh` is
    /// owed.
    pub fn owe_token_refresh(&mut self) {
        self.wants_refresh = true;
    }

    /// Forget `project`'s cached parses and fold, as if it had never been
    /// folded: for a test whose subject is the rebuild from the per-file
    /// caches. Nothing in the server forgets them; a read confirms them.
    #[cfg(test)]
    fn forget_fold(&mut self, project: &Path) {
        let key = ProjectId::new(project);
        self.project_parses.remove(&key);
        self.resolved_projects.remove(&key);
    }

    /// The parse memo every reader of a source parses through.
    pub fn parses_mut(&mut self) -> &mut ParseMemo {
        &mut self.parses
    }

    /// Drop every assembly env whose resolution consulted a reader that
    /// records nothing ([`EnvReads::unrecorded`]: an on-demand restore, the
    /// C# sidecar), with every fold against it. Nothing records what those
    /// readers read, so an event is what covers it: this is called for a
    /// referenced-assembly watch event (a `.dll` rewritten by a sibling
    /// rebuild or a restore, a `.cs`/`.csproj` edit that changes what the C#
    /// sidecar would emit), a structural one, and a moved
    /// [`Workspace::projects_generation`] ([`Self::sync_with_evaluations`]),
    /// each of which once dropped every env. An env whose reads are all
    /// recorded is not touched: what it read is confirmed before it is
    /// served again, and an event naming a DLL whose stamp it read drops it
    /// already ([`Self::hint_assembly_paths`]). The on-disk [`AssemblyCache`]
    /// needs no invalidation — its entries are `(size, mtime)`-validated per
    /// DLL.
    pub fn invalidate_assembly_state(&mut self) {
        let unrecorded: Vec<Arc<AssemblyEnv>> = self
            .assembly_envs
            .values()
            .filter(|entry| !entry.cached.reads.unrecorded.is_empty())
            .map(|entry| Arc::clone(&entry.cached.env))
            .collect();
        for env in &unrecorded {
            self.drop_env(env);
        }
        // A cached PDB image rests on the DLL's stamp and on the sidecar
        // `.pdb` the DLL names, whose stamp it does not record: a symbol file
        // added beside an unchanged DLL reaches the cache only through an
        // event of this class.
        self.pdb_images.clear();
        // An env built afresh for every request (a transient sidecar
        // failure, never cached) may answer differently now, and an open
        // buffer's tokens with it.
        self.wants_refresh = true;
    }

    /// Watch events named `paths`: drop every assembly env that read the
    /// stamp of one of them as spelt ([`EnvChecks::named_by`]: a DLL), with
    /// every fold against it.
    /// Every other env stands, and its next confirmation inside a generation
    /// covers the installed contents it trusts too, whatever the events
    /// named: an event is what covers an in-place change to an installed
    /// version (D3).
    pub fn hint_assembly_paths(&mut self, paths: &[PathBuf]) {
        for entry in self.assembly_envs.values_mut() {
            entry.event_since = true;
        }
        let named: Vec<Arc<AssemblyEnv>> = self
            .assembly_envs
            .values()
            .filter(|entry| {
                paths
                    .iter()
                    .any(|path| entry.resolution.named_by(path) || entry.contents.named_by(path))
            })
            .map(|entry| Arc::clone(&entry.cached.env))
            .collect();
        for env in &named {
            tracing::info!("a watch event named an input of a cached assembly env; dropping it");
            self.drop_env(env);
        }
    }

    /// Drop the cached env `env`, and every fold against it. The folds hold
    /// handles into the env, and `resolved_projects` is served without
    /// asking for the env again, so a fold must not outlive its env's entry.
    /// A dropped env may have answered what an open buffer's tokens show, so
    /// a workspace refresh is owed.
    fn drop_env(&mut self, env: &Arc<AssemblyEnv>) {
        self.assembly_envs
            .retain(|_, entry| !Arc::ptr_eq(&entry.cached.env, env));
        self.resolved_projects
            .retain(|_, (_, folded)| !Arc::ptr_eq(folded, env));
        self.inferred_projects
            .retain(|_, cached| !Arc::ptr_eq(&cached.env, env));
        self.prev_resolved
            .retain(|_, prev| !Arc::ptr_eq(&prev.env, env));
        self.wants_refresh = true;
    }

    /// Drop every assembly env and every fold against one: for a change of
    /// a setting the envs are not keyed on, or of an input none of them
    /// records (NuGet's settings, which SDK discovery reads directly:
    /// [`crate::server::State::apply_watched_changes`]).
    pub fn drop_every_env(&mut self) {
        self.resolved_projects.clear();
        self.inferred_projects.clear();
        self.prev_resolved.clear();
        self.assembly_envs.clear();
        self.wants_refresh = true;
    }

    /// The portable-PDB image for the referenced DLL at `dll`, cached for the
    /// server lifetime (the `pdb_images` map) under the DLL's stamp. On a miss
    /// (no entry, or the DLL's stamp moved) it runs `compute` (the IO +
    /// embedded-or-sidecar selection the handler owns) and caches the result —
    /// including a `None`, so a DLL with no usable PDB isn't re-read on every
    /// go-to-definition. A stamp taken within the racy window of the DLL's
    /// last change is not cached under: a same-size rewrite in that window
    /// would leave it unchanged. `compute` is injected so this cache stays
    /// unaware of the PDB-sourcing logic (which lives in the definition
    /// handler).
    pub fn pdb_image(
        &mut self,
        dll: &Path,
        compute: impl FnOnce() -> Option<Arc<[u8]>>,
    ) -> Option<Arc<[u8]>> {
        let stamp = msbuild_observed::stamp(&Unobserved::GoToDefinition.msbuild_observer(), dll);
        if let Ok(stamp) = &stamp
            && let Some((cached, image)) = self.pdb_images.get(dll)
            && cached == stamp
        {
            return image.clone();
        }
        let image = compute();
        match stamp {
            Ok(stamp) if !stamp.racy => {
                self.pdb_images
                    .insert(dll.to_path_buf(), (stamp, image.clone()));
            }
            _ => {
                self.pdb_images.remove(dll);
            }
        }
        image
    }

    /// The XML documentation of a referenced-assembly symbol, read from the
    /// `.xml` beside the DLL `env` read it from (cached per file; see
    /// [`crate::xml_doc::lookup`]), with its `<inheritdoc>` elements expanded
    /// as Roslyn's IDE expands them where that can be done exactly
    /// ([`crate::xml_doc::inherit`]). `Err` is the lookup's reason for having
    /// no entry at all.
    pub fn xml_doc(
        &mut self,
        env: &Arc<AssemblyEnv>,
        target: DocTarget,
    ) -> Result<Expansion, DocLookup> {
        let entry = self.xml_doc.locate(env, target)?;
        Ok(expand(&mut self.xml_doc, env, target, entry))
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
    /// - **C# graph edges (`.csproj`)** are consulted via the sidecar, only
    ///   when the host opted in
    ///   ([`Self::set_csharp_project_references_enabled`]): it emits a Roslyn
    ///   metadata DLL per referenced project (and its transitive C# closure),
    ///   which are folded in. When the host has not opted in, the sidecar is
    ///   never spawned and each C# edge is unread, which marks the env
    ///   incomplete. Degrades to under-resolution when the sidecar is
    ///   unavailable, a build fails (see [`SidecarManager`]), or the edge is
    ///   too new for any restore to have recorded a producer TFM.
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
        // The caller chose the root, so choosing it read nothing here.
        never_superseded(self.assembly_env_for_project_retryable(
            project,
            dotnet_root,
            ReadSet::default(),
            target_framework,
            workspace,
            self.restore_env(workspace),
            &Watch::unwatched(),
        ))
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
        self.reference_set_for_project(
            project,
            dotnet_root,
            target_framework,
            workspace,
            &Observer::new(),
        )
        .dlls
    }

    /// [`Self::reference_dlls_for_project`]'s whole resolution: the DLLs and
    /// what became of each declared project-reference edge.
    fn reference_set_for_project(
        &mut self,
        project: &Path,
        dotnet_root: Option<&Path>,
        target_framework: &ServedTfm,
        workspace: &Workspace,
        observer: &Observer,
    ) -> ReferenceSet {
        // Observability / differential surface: it diffs against MSBuild's
        // `ReferencePath` on *restored* projects, so it reads the assets file
        // like the assets path does and never drives the on-demand restore
        // (`None` restore environment).
        self.resolve_references(
            project,
            dotnet_root,
            target_framework,
            workspace,
            None,
            observer,
        )
    }

    /// Resolve `project`'s references under the key's root and TFM,
    /// recording every query in `observer`: the reference **edges** from the
    /// parsed `<ProjectReference>` graph (plan E1), evaluated fresh off-cache
    /// so they reflect current disk — never from `project.assets.json`,
    /// which may lag the fsproj in both directions — and the artifacts from
    /// the assets file. Producer-TFM recovery (fsproj 3.3c Phase 2b rooted
    /// at the entry) runs first because it feeds the graph walk too: each
    /// node is evaluated under the TFM NuGet selected for it, so a
    /// `$(TargetFramework)`-gated `<ProjectReference>` in a multi-targeted
    /// dependency contributes the edges the real build would.
    fn resolve_references(
        &mut self,
        project: &Path,
        dotnet_root: Option<&Path>,
        target_framework: &ServedTfm,
        workspace: &Workspace,
        restore_env: Option<&SdkDiscoveryEnv>,
        observer: &Observer,
    ) -> ReferenceSet {
        let _span =
            tracing::info_span!("resolve_references", project = %project.display()).entered();
        self.env_resolutions += 1;
        let recovered_ref_tfms = env_ref_tfms(project, target_framework.as_deref(), observer);
        let graph =
            workspace.project_graph_with_producer_tfms(project, &recovered_ref_tfms, observer);
        let ref_targets = graph_ref_targets(&graph, project, observer);
        // Borrow `sidecar` (mut) as a disjoint field — it drives any C#
        // `.csproj` references this project has, lazily spawning on first
        // need, when the host opted into them.
        resolve_reference_dlls(
            project,
            dotnet_root,
            target_framework,
            CsharpReferences::gated(self.csharp_project_references_enabled, &mut self.sidecar),
            &ref_targets,
            &recovered_ref_tfms,
            restore_env,
            observer,
        )
    }

    /// [`Self::reference_dlls_for_project`], and what resolving them read:
    /// the read set the env built from them would carry, less the DLLs'
    /// own stamps, which the env build records as it reads each one. The
    /// read-set soundness tests hold it to the file system.
    pub fn reference_resolution_for_project(
        &mut self,
        project: &Path,
        dotnet_root: Option<&Path>,
        target_framework: &ServedTfm,
        workspace: &Workspace,
    ) -> (Vec<PathBuf>, EnvReads) {
        let observer = Observer::new();
        let set = self.reference_set_for_project(
            project,
            dotnet_root,
            target_framework,
            workspace,
            &observer,
        );
        let reads = EnvReads {
            reads: observer.finish().0,
            unrecorded: set.unrecorded,
        };
        (set.dlls, reads)
    }

    /// [`Self::assembly_env_for_project`] plus a *retryable* flag: `true` when a
    /// transient C# sidecar failure left the env incomplete and un-cached, so a
    /// caller that builds its own derived cache from this env (e.g.
    /// [`Self::resolved_project_for`]) must skip caching too and let the next
    /// request retry. A cache *hit* is always `false` — only stable envs are
    /// ever cached.
    ///
    /// Stops, once `watch` sees a notification, between resolving the
    /// references and building the env: the two units of a miss.
    #[allow(clippy::too_many_arguments)]
    fn assembly_env_for_project_retryable(
        &mut self,
        project: &Path,
        dotnet_root: Option<&Path>,
        root_reads: ReadSet,
        target_framework: &ServedTfm,
        workspace: &Workspace,
        restore_env: Option<&SdkDiscoveryEnv>,
        watch: &Watch,
    ) -> Result<(Arc<AssemblyEnv>, bool), Superseded> {
        let (cached, retryable) = self.cached_assembly_env(
            project,
            dotnet_root,
            root_reads,
            target_framework,
            workspace,
            restore_env,
            watch,
        )?;
        Ok((cached.env, retryable))
    }

    /// The reference DLLs the env this state *serves* for `project` was built
    /// from, and a *retryable* flag: `true` when a transient C# sidecar failure
    /// left the env incomplete, so nothing was cached and the next call may
    /// resolve differently.
    ///
    /// Distinct from [`Self::reference_dlls_for_project`], which re-resolves from
    /// scratch every call: this one comes out of the cache entry **alongside the
    /// env**, so a consumer that needs both — an oracle fed the reference set
    /// while its subject resolves against the env, as `borzoi-corpus-diff` does —
    /// cannot get them from two resolutions that disagree. Two resolutions can
    /// genuinely differ: a transient C# sidecar failure degrades one and a retry
    /// recovers the other, and that is exactly what `retryable` reports. Nothing
    /// is cached in that case, so a caller demanding one reference set must
    /// **refuse** the project rather than pair a set with an env built from
    /// another.
    pub fn env_reference_dlls_for_project(
        &mut self,
        project: &Path,
        dotnet_root: Option<&Path>,
        target_framework: &ServedTfm,
        workspace: &Workspace,
    ) -> (Vec<PathBuf>, bool) {
        let restore_env = self.restore_env(workspace);
        let (cached, retryable) = never_superseded(self.cached_assembly_env(
            project,
            dotnet_root,
            ReadSet::default(),
            target_framework,
            workspace,
            restore_env,
            &Watch::unwatched(),
        ));
        (cached.dlls.to_vec(), retryable)
    }

    /// What the resolution of the env this state serves for `project` read
    /// (its cache entry's [`EnvReads`]), and the *retryable* flag of
    /// [`Self::env_reference_dlls_for_project`].
    pub fn env_reads_for_project(
        &mut self,
        project: &Path,
        dotnet_root: Option<&Path>,
        target_framework: &ServedTfm,
        workspace: &Workspace,
    ) -> (Arc<EnvReads>, bool) {
        let restore_env = self.restore_env(workspace);
        let (cached, retryable) = never_superseded(self.cached_assembly_env(
            project,
            dotnet_root,
            ReadSet::default(),
            target_framework,
            workspace,
            restore_env,
            &Watch::unwatched(),
        ));
        (cached.reads, retryable)
    }

    /// What the env this state serves for `project` confirms before it is
    /// served again (its resolution's reads, less the contents of the
    /// installed versions it trusts: `Workspace::env_checks`), or `None`
    /// when it is not cached (a transient C# sidecar failure). The read-set
    /// soundness tests hold it to the file system.
    pub fn env_confirmed_reads_for_project(
        &mut self,
        project: &Path,
        dotnet_root: Option<&Path>,
        target_framework: &ServedTfm,
        workspace: &Workspace,
    ) -> Option<Vec<msbuild_observed::Read>> {
        let restore_env = self.restore_env(workspace);
        let (cached, _) = never_superseded(self.cached_assembly_env(
            project,
            dotnet_root,
            ReadSet::default(),
            target_framework,
            workspace,
            restore_env,
            &Watch::unwatched(),
        ));
        self.assembly_envs
            .values()
            .find(|entry| Arc::ptr_eq(&entry.cached.env, &cached.env))
            .map(|entry| {
                entry
                    .resolution
                    .reads()
                    .chain(entry.contents.reads())
                    .cloned()
                    .collect()
            })
    }

    /// The cache entry for `project`'s env: the env and the reference-DLL list
    /// it was built from, which are one resolution's two outputs and are cached
    /// as one value so they cannot drift apart.
    #[allow(clippy::too_many_arguments)]
    fn cached_assembly_env(
        &mut self,
        project: &Path,
        dotnet_root: Option<&Path>,
        root_reads: ReadSet,
        target_framework: &ServedTfm,
        workspace: &Workspace,
        restore_env: Option<&SdkDiscoveryEnv>,
        watch: &Watch,
    ) -> Result<(CachedAssemblyEnv, bool), Superseded> {
        // Key on the three *value* inputs the resolution is run under. Without
        // the root in the key, a lookup made before the SDK root is known
        // (`None`, or a wrong root) would cache an empty env under the project
        // path and hand it back to every later call with the real root —
        // permanently disabling assembly resolution for this project (D5:
        // under-resolve, never wrong). The TFM verdict selects the assets
        // target (plan E3) — the full [`ServedTfm`], not its `Option`
        // projection, because `NoneDeclared` and `Untrusted` build different
        // envs and must not share an entry. The graph-sourced reference
        // edges are *not* in the key: like every other input of the
        // resolution they are read fresh from disk on a miss, and a `.fsproj`
        // edit reaches this cache the same way a restore does — through the
        // resolution's read set, which records both. When that moves, the
        // references are resolved again, and the env is kept if they resolve
        // to the same [`EnvInputs`].
        // The env's references and TFM come from project evaluations, which
        // the caller confirmed for this generation in reading `dotnet_root`
        // and `target_framework`, and its read set holds what they read. An
        // env with an unrecorded reader goes when the workspace has dropped
        // an evaluation since.
        self.sync_with_evaluations(workspace.projects_generation());
        let id = ProjectId::new(project);
        // Built, restored and reported as the key spells it, so the entry holds
        // exactly the env of the project it names.
        let project = id.as_path();
        // Every query the resolution makes is recorded here, with what
        // choosing its key read: the canonical spelling of the root, and
        // `root_reads`, what choosing the root read (a probe for a project
        // that records no SDK root). A cached fold's fast path never computes
        // the key again, so the resolution's confirmation is what notices it
        // would move.
        let resolving = Observer::new();
        let key = (
            id.clone(),
            dotnet_root.map(|root| {
                msbuild_observed::canonicalize(&resolving, root)
                    .unwrap_or_else(|_| root.to_path_buf())
            }),
            target_framework.clone(),
        );
        let lookup = self.confirmed_env(&key, workspace);
        if let EnvLookup::Served(cached) = lookup {
            return Ok((cached, false));
        }
        // A package the resolution reads is trusted as installed only if it
        // was complete before the resolution began.
        let started = workspace.generation_time();
        let refs = self.resolve_references(
            project,
            dotnet_root,
            target_framework,
            workspace,
            restore_env,
            &resolving,
        );
        let (resolution_reads, counts) = resolving.finish();
        let resolution_reads = resolution_reads.union(root_reads);
        if matches!(lookup, EnvLookup::ResolutionMoved)
            && let Some(kept) = self.keep_if_resolved_alike(
                &key,
                dotnet_root,
                &refs,
                resolution_reads.clone(),
                counts,
                started,
                workspace,
            )
        {
            return Ok((kept, false));
        }
        // Before building: the resolution is not cached, so a run that
        // stops here resolves again; an entry the comparison found moved is
        // already dropped.
        watch.check()?;
        Ok(self.build_and_cache(
            key,
            dotnet_root,
            &refs,
            resolution_reads,
            counts,
            started,
            workspace,
        ))
    }

    /// Build the env `refs` describe (resolved under `key`, reading
    /// `resolution_reads`, from the generation time `started`), and cache
    /// it under `key` unless a transient C# sidecar failure makes it
    /// retryable.
    #[allow(clippy::too_many_arguments)]
    fn build_and_cache(
        &mut self,
        key: (ProjectId, Option<PathBuf>, ServedTfm),
        dotnet_root: Option<&Path>,
        refs: &ReferenceSet,
        resolution_reads: ReadSet,
        counts: msbuild_observed::ReadCounts,
        started: Option<crate::vfs::Nanos>,
        workspace: &Workspace,
    ) -> (CachedAssemblyEnv, bool) {
        let id = key.0.clone();
        let project = id.as_path();
        let contents = Observer::new();
        let (env, dlls) = build_assembly_env(project, refs, &self.assembly_cache, &contents);
        let (contents_reads, contents_counts) = contents.finish();
        let reads = EnvReads {
            reads: resolution_reads.clone().union(contents_reads.clone()),
            unrecorded: refs.unrecorded.clone(),
        };
        crate::observed::report_env_resolution(counts + contents_counts, &reads);
        self.envs_built += 1;
        let cached = CachedAssemblyEnv {
            env: Arc::new(env),
            dlls: dlls.into(),
            reads: Arc::new(reads),
        };
        // A *transient* C# sidecar transport failure (`retryable`) is the only
        // reason not to cache: the handle was dropped for respawn, so the next
        // request should re-attempt the C# metadata rather than be served this
        // degraded env forever. Every *stable* result — an assets-file env, a
        // successful on-demand restore, or a stable degradation (no restore
        // environment, a cold-cache restore decline, a genuine build error) —
        // caches, so we don't rebuild (or re-restore) on every call. A later
        // manual `dotnet restore` reaches this cache through its read set,
        // which records `project.assets.json` (as absent, before it).
        if refs.retryable {
            tracing::info!(
                project = %project.display(),
                "not caching assembly env after a transient C# sidecar failure; will retry on next request"
            );
        } else {
            let resolution = workspace.env_checks(
                &id,
                dotnet_root,
                &refs.package_folders,
                &resolution_reads,
                started,
            );
            let contents = workspace.env_checks(
                &id,
                dotnet_root,
                &refs.package_folders,
                &contents_reads,
                started,
            );
            self.assembly_envs.insert(
                key,
                EnvEntry {
                    cached: cached.clone(),
                    inputs: EnvInputs::of(refs),
                    resolution,
                    contents,
                    contents_reads,
                    // Read in this generation, so confirmed for it.
                    confirmed_in: workspace.generation(),
                    event_since: false,
                },
            );
        }
        (cached, refs.retryable)
    }

    /// The cached env under `key`, its resolution's reads having moved, kept
    /// if resolving its references again (`refs`, which read `reads`) gives
    /// the inputs it was built from: its checks of what the resolution read
    /// become those of `reads`, its unrecorded readers those `refs` names
    /// (which decide what drops it besides its checks:
    /// [`Self::invalidate_assembly_state`]), and it is confirmed for the
    /// generation. Otherwise the entry is dropped, with every fold against
    /// its env, and `None` returned.
    #[allow(clippy::too_many_arguments)]
    fn keep_if_resolved_alike(
        &mut self,
        key: &(ProjectId, Option<PathBuf>, ServedTfm),
        dotnet_root: Option<&Path>,
        refs: &ReferenceSet,
        reads: ReadSet,
        counts: msbuild_observed::ReadCounts,
        started: Option<crate::vfs::Nanos>,
        workspace: &Workspace,
    ) -> Option<CachedAssemblyEnv> {
        let _span =
            tracing::info_span!(ENV_RESOLVED_AGAIN_SPAN, project = %key.0.as_path().display())
                .entered();
        let entry = self.assembly_envs.get_mut(key)?;
        if refs.retryable || EnvInputs::of(refs) != entry.inputs {
            let env = Arc::clone(&entry.cached.env);
            tracing::info!(
                project = %key.0.as_path().display(),
                "an assembly env's references resolve differently; building it again"
            );
            self.drop_env(&env);
            return None;
        }
        entry.resolution =
            workspace.env_checks(&key.0, dotnet_root, &refs.package_folders, &reads, started);
        let reads = EnvReads {
            reads: reads.union(entry.contents_reads.clone()),
            unrecorded: refs.unrecorded.clone(),
        };
        crate::observed::report_env_resolution(counts, &reads);
        entry.cached.reads = Arc::new(reads);
        if workspace.generation().is_some() {
            entry.confirmed_in = workspace.generation();
            entry.event_since = false;
        }
        self.envs_resolved_again += 1;
        Some(entry.cached.clone())
    }

    /// The cached env under `key`, as far as confirming it for the open
    /// generation can say ([`Workspace::confirm_env`]). An entry confirmed
    /// earlier in the generation is served. Otherwise what building it read
    /// (its DLLs' stamps) is confirmed, and an entry for which that fails
    /// is dropped, with every fold against its env; then what resolving its
    /// references read, and an entry for which that fails is left for the
    /// caller to resolve again and compare
    /// ([`Self::keep_if_resolved_alike`]).
    fn confirmed_env(
        &mut self,
        key: &(ProjectId, Option<PathBuf>, ServedTfm),
        workspace: &Workspace,
    ) -> EnvLookup {
        let generation = workspace.generation();
        let Some(entry) = self.assembly_envs.get_mut(key) else {
            return EnvLookup::Absent;
        };
        // A generation number is this workspace's: one under another
        // workspace says nothing about these checks.
        let confirmed = generation.is_some()
            && entry.confirmed_in == generation
            && !entry.event_since
            && workspace.owns(&entry.contents);
        if confirmed {
            return EnvLookup::Served(entry.cached.clone());
        }
        if !workspace.confirm_env(&entry.contents, entry.event_since) {
            let env = Arc::clone(&entry.cached.env);
            tracing::info!(
                project = %key.0.as_path().display(),
                "a DLL an assembly env holds moved; building it again"
            );
            self.drop_env(&env);
            return EnvLookup::Absent;
        }
        if !workspace.confirm_env(&entry.resolution, entry.event_since) {
            return EnvLookup::ResolutionMoved;
        }
        // Outside a generation nothing was confirmed: the record of the
        // last generation that did stands.
        if generation.is_some() {
            entry.confirmed_in = generation;
            entry.event_since = false;
        }
        EnvLookup::Served(entry.cached.clone())
    }

    /// Whether `env`, which a cached fold of `project` was folded against,
    /// is still the env its entry serves, once confirmed for the open
    /// generation ([`Self::confirmed_env`]). An env with no entry (dropped,
    /// or never cached) is not: the fold is dropped with it.
    ///
    /// An entry whose resolution's reads moved is resolved again. Its key
    /// (the dotnet root and the served TFM) was chosen from the project's
    /// evaluation, which may have moved with them, so the key is chosen
    /// again first: an env under another key than the one the project
    /// would choose now is not served, whatever it holds.
    ///
    /// Stops, once `watch` sees a notification, before resolving again and
    /// before building the env that replaces one resolved differently: each
    /// is a unit.
    fn env_still_served(
        &mut self,
        env: &Arc<AssemblyEnv>,
        project: &Path,
        workspace: &mut Workspace,
        watch: &Watch,
    ) -> Result<FoldEnv, Superseded> {
        let key = self
            .assembly_envs
            .iter()
            .find(|(_, entry)| Arc::ptr_eq(&entry.cached.env, env))
            .map(|(key, _)| key.clone());
        let mut replaced = None;
        let served = match key {
            None => false,
            Some(key) => match self.confirmed_env(&key, workspace) {
                EnvLookup::Served(_) => true,
                EnvLookup::Absent => false,
                EnvLookup::ResolutionMoved => {
                    let (dotnet_root, root_reads) =
                        workspace.dotnet_root_and_reads_for_project(project);
                    let target_framework = workspace.served_tfm_for_project(project);
                    let resolving = Observer::new();
                    let chosen = (
                        key.0.clone(),
                        dotnet_root.as_deref().map(|root| {
                            msbuild_observed::canonicalize(&resolving, root)
                                .unwrap_or_else(|_| root.to_path_buf())
                        }),
                        target_framework.clone(),
                    );
                    if chosen != key {
                        false
                    } else {
                        // Resolving again is a unit, and nothing has changed
                        // yet: a run that stops here finds the entry as it
                        // was.
                        watch.check()?;
                        let workspace: &Workspace = workspace;
                        let restore_env = self.restore_env(workspace);
                        let started = workspace.generation_time();
                        let refs = self.resolve_references(
                            key.0.as_path(),
                            dotnet_root.as_deref(),
                            &target_framework,
                            workspace,
                            restore_env,
                            &resolving,
                        );
                        let (reads, counts) = resolving.finish();
                        let reads = reads.union(root_reads);
                        let kept = self
                            .keep_if_resolved_alike(
                                &key,
                                dotnet_root.as_deref(),
                                &refs,
                                reads.clone(),
                                counts,
                                started,
                                workspace,
                            )
                            .is_some();
                        if !kept {
                            // The comparison dropped the entry and every fold
                            // against its env, owing their refresh: building
                            // the new one is the next unit.
                            if let Err(superseded) = watch.check() {
                                self.drop_env(env);
                                return Err(superseded);
                            }
                            // The fold against the old env goes; the env its
                            // refold reads is built here, from the resolution
                            // just made, rather than resolved again, and
                            // handed to the refold whether or not it is
                            // cached (a transient failure is retried by the
                            // next request, not by this one).
                            let (rebuilt, retryable) = self.build_and_cache(
                                key.clone(),
                                dotnet_root.as_deref(),
                                &refs,
                                reads,
                                counts,
                                started,
                                workspace,
                            );
                            replaced = Some((rebuilt.env, retryable));
                        }
                        kept
                    }
                }
            },
        };
        if served {
            return Ok(FoldEnv::Served);
        }
        self.drop_env(env);
        Ok(match replaced {
            Some((env, retryable)) => FoldEnv::Replaced(env, retryable),
            None => FoldEnv::Gone,
        })
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
    /// Cached (project + env) keyed by [`ProjectId`], and served while its
    /// parses and env are confirmed (`Self::validate_cached_fold`). Folds the
    /// whole project; for a single-file request that only needs a prefix, prefer
    /// [`Self::resolved_prefix_and_env_for`].
    pub fn resolved_project_and_env_for(
        &mut self,
        project: &Path,
        workspace: &mut Workspace,
        docs: &Vfs,
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
        docs: &Vfs,
    ) -> Option<(Arc<ResolvedProject>, Arc<AssemblyEnv>)> {
        never_superseded(self.resolved_prefix_and_env_watched(
            project,
            up_to_index,
            workspace,
            docs,
            &Watch::unwatched(),
        ))
    }

    /// [`Self::resolved_prefix_and_env_for`], stopping at a checkpoint once
    /// `watch` sees a notification ([`crate::inbox`]). No unit of work begins
    /// without one just before it: before returning a cached prefix (its
    /// caller may begin one), before the parses (whose rebuild may evaluate
    /// the project again) and each Compile item's parse
    /// ([`Self::parses_for_project_watched`]), before the env, and before the
    /// fold. Each unit is cached before the next checkpoint, so a run that
    /// stops keeps what it computed; what follows the last unit is the
    /// caller's, up to the loop's checkpoint after the handler.
    pub fn resolved_prefix_and_env_watched(
        &mut self,
        project: &Path,
        up_to_index: usize,
        workspace: &mut Workspace,
        docs: &Vfs,
        watch: &Watch,
    ) -> Result<Option<ResolvedPrefix>, Superseded> {
        let key = ProjectId::new(project);
        let project = key.as_path();
        let lookup_span = tracing::info_span!(
            "semantic.project_lookup",
            project = %project.display(),
            scope = if up_to_index == usize::MAX { "full" } else { "prefix" },
            cache_hit = tracing::field::Empty,
            requested_files = tracing::field::Empty,
            cached_files = tracing::field::Empty,
        );
        let _lookup_guard = lookup_span.enter();
        self.validate_cached_fold(&key, workspace, docs);
        let cached_files = self
            .resolved_projects
            .get(&key)
            .map_or(0, |(resolved, _env)| resolved.len());
        lookup_span.record("cached_files", cached_files as i64);

        // Fast path — **before any `Workspace`/SDK probing**. `dotnet_root_for_project`
        // evaluates the project and, with no recorded SDK root, re-runs SDK
        // discovery against the filesystem, so a repeat request that already has
        // its answer must not pay that. Size the request against the
        // *already-cached* parses and return a cached prefix that covers it — both
        // are plain map lookups. The env it was folded against is confirmed for
        // the generation by its entry, found by identity, not by key: an env
        // whose inputs moved drops its entry and every fold against it
        // (`Self::drop_env`), and the request falls through to rebuild both.
        // If the parses aren't cached, neither is the fold (they are built and
        // dropped together), so this correctly falls through.
        let mut replaced = None;
        if let Some(cached) = self.project_parses.get(&key) {
            let want_len = up_to_index.saturating_add(1).min(cached.parses.files.len());
            lookup_span.record("requested_files", want_len as i64);
            if let Some((resolved, env)) = self.resolved_projects.get(&key)
                && resolved.len() >= want_len
            {
                let (resolved, env) = (Arc::clone(resolved), Arc::clone(env));
                match self.env_still_served(&env, project, workspace, watch)? {
                    FoldEnv::Served => {
                        lookup_span.record("cache_hit", true);
                        watch.check()?;
                        return Ok(Some((resolved, env)));
                    }
                    FoldEnv::Replaced(env, retryable) => replaced = Some((env, retryable)),
                    FoldEnv::Gone => {}
                }
            }
        }
        lookup_span.record("cache_hit", false);

        // Slow path: (re-)evaluate parses + env and fold the prefix.
        //
        // Use `dotnet_root_for_project` (not `env().dotnet_root`): the latter is
        // the raw `$DOTNET_ROOT` env var; the former runs the proper SDK
        // discovery — `$PATH` fallback, `global.json` `sdk.paths` overrides —
        // so a workspace with no `$DOTNET_ROOT` but `dotnet` on `$PATH` still
        // gets framework-pack DLLs into the assembly env.
        let (dotnet_root, root_reads) = workspace.dotnet_root_and_reads_for_project(project);
        let dotnet_root = dotnet_root.as_deref();
        // The served-TFM verdict, read **once** and fed to both resolution
        // inputs (fsproj 3.3c, plan E5): the parses are already coherent by
        // construction (the evaluation itself was seeded with this value),
        // and the env selects the same TFM's assets target. Sourcing both
        // from this single read is what enforces the coherence invariant —
        // a project parsed under net8.0's defines must never resolve against
        // net10.0's assemblies.
        let target_framework = workspace.served_tfm_for_project(project);
        watch.check()?;

        // Parses — None if the Compile set is untrustworthy or it failed to
        // evaluate. Cloning a `ProjectParses` is cheap (the `ImplFile`s are syntax
        // handles, `texts` is `Arc<str>` per file). The file count clamps the slice.
        let Some(parses) = self.parses_for_project_watched(project, workspace, docs, watch)? else {
            return Ok(None);
        };
        let parses = parses.clone();
        watch.check()?;
        // Resolve `0..want_len`; `up_to_index` is a valid Compile index (or
        // `usize::MAX` for "the whole project"), so `+1` (saturating) then clamp.
        let want_len = up_to_index.saturating_add(1).min(parses.files.len());
        lookup_span.record("requested_files", want_len as i64);
        // Assembly env — always returns *some* env (empty if anything is
        // missing). Caches a separate slot. `retryable` is set when a
        // transient C# sidecar failure left the env incomplete and un-cached.
        // Passes the restore environment (when the trust opt-in is on) so an
        // assets-absent project is resolved by an on-demand `dotnet restore`
        // (see [`crate::restore`]); the `&Workspace` entry point does the
        // same, so they agree.
        let restore_env = self.restore_env(workspace);
        // The env the fast path built in finding the cached fold's env moved
        // is the one this generation resolves to: reuse it.
        let (env, retryable) = match replaced {
            Some(replaced) => replaced,
            None => self.assembly_env_for_project_retryable(
                project,
                dotnet_root,
                root_reads,
                &target_framework,
                workspace,
                restore_env,
                watch,
            )?,
        };
        watch.check()?;
        // The fold horizon: `.file(up_to_index)` and everything it can
        // reference (files `0..up_to_index`), nothing after. The *whole*
        // Compile list is still handed to sema — the sig ↔ impl pairing must
        // derive from it, so a signature whose implementation lies past the
        // horizon stays paired and a prefix fold answers exactly like the
        // full fold's prefix (codex round 2).
        let files = &parses.files;
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
            // whose inputs moved or a transient-sidecar recovery, is a fresh
            // Arc, so the pointer check fails and we fold cold). See [`PrevFold`].
            let reusable_prev = self
                .prev_resolved
                .get(&key)
                .filter(|prev| Arc::ptr_eq(&prev.env, &env));
            took_incremental = reusable_prev.is_some();
            let resolve_span = tracing::info_span!(
                "resolve_project",
                files = want_len as i64,
                incremental = took_incremental,
                reused_files = tracing::field::Empty,
            );
            let _resolve_guard = resolve_span.enter();
            let answer = match reusable_prev {
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
                        borzoi_sema::resolve_project_files_prefix_incremental_labeled(
                            &prev.files,
                            prev.resolved.as_ref(),
                            files,
                            want_len,
                            &labels,
                            &env,
                        )
                    };
                    #[cfg(not(feature = "otel"))]
                    let (resolved, reused) = borzoi_sema::resolve_project_files_prefix_incremental(
                        &prev.files,
                        prev.resolved.as_ref(),
                        files,
                        want_len,
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
                        borzoi_sema::resolve_project_files_prefix_labeled(
                            files, want_len, &labels, &env,
                        )
                    };
                    #[cfg(not(feature = "otel"))]
                    let resolved = borzoi_sema::resolve_project_files_prefix(files, want_len, &env);
                    (Arc::new(resolved), 0)
                }
            };
            resolve_span.record("reused_files", answer.1 as i64);
            answer
        };
        if took_incremental {
            self.incremental_folds += 1;
        }
        self.folds += 1;
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
            // only added state is the folded prefix's parsed files (syntax handles).
            let keep_deeper = self
                .prev_resolved
                .get(&key)
                .is_some_and(|p| p.resolved.len() > want_len && Arc::ptr_eq(&p.env, &env));
            if !keep_deeper {
                self.prev_resolved.insert(
                    key.clone(),
                    PrevFold {
                        // The FULL Compile list (syntax handles — cheap), not
                        // just the folded prefix: the incremental fold
                        // re-derives the sig ↔ impl pairing from it, and a
                        // pairing computed over a slice could differ.
                        files: files.to_vec(),
                        resolved: Arc::clone(&resolved),
                        env: Arc::clone(&env),
                    },
                );
            }
            self.drop_env_past_name_cap(&key, &env);
        }
        Ok(Some((resolved, env)))
    }

    /// Drop `env`, and every fold of `key`'s against it, once its tables
    /// are past [`Self::name_table_cap`]. The next request then builds a fresh
    /// env, whose fold is cold through the existing [`Arc::ptr_eq`] checks.
    fn drop_env_past_name_cap(&mut self, key: &ProjectId, env: &Arc<AssemblyEnv>) {
        let entries = env.interned_count();
        if entries <= self.name_table_cap.0 {
            return;
        }
        tracing::info!(
            project = %key.as_path().display(),
            entries,
            cap = self.name_table_cap.0,
            "dropping an assembly env whose tables are past their cap"
        );
        self.assembly_envs
            .retain(|_, entry| !Arc::ptr_eq(&entry.cached.env, env));
        self.resolved_projects.remove(key);
        self.inferred_projects.remove(key);
        self.prev_resolved.remove(key);
    }

    /// The Compile **prefix** up to and including `up_to_index`, resolved and
    /// **inferred** in Compile order ([`borzoi_sema::infer_project`]), with the
    /// env both ran against ([`Self::resolved_prefix_and_env_for`]). A file's
    /// inference reads what every earlier file published for its top-level
    /// values, so a single-file request infers the whole prefix — once: the
    /// fold is cached, and a later fold against the same env reuses each file
    /// whose resolved file is the very one it was inferred from and whose
    /// earlier files contributed what they did before
    /// ([`borzoi_sema::infer_project_reusing`]).
    ///
    /// `None` when the project cannot be resolved; the inferred project covers
    /// the same files as the resolved one returned beside it.
    pub fn inferred_prefix_for(
        &mut self,
        project: &Path,
        up_to_index: usize,
        workspace: &mut Workspace,
        docs: &Vfs,
    ) -> Option<(Arc<ResolvedProject>, Arc<AssemblyEnv>, Arc<InferredProject>)> {
        never_superseded(self.inferred_prefix_watched(
            project,
            up_to_index,
            workspace,
            docs,
            &Watch::unwatched(),
        ))
    }

    /// [`Self::inferred_prefix_for`], stopping at the checkpoints of
    /// [`Self::resolved_prefix_and_env_watched`] and at one before the
    /// inference.
    pub fn inferred_prefix_watched(
        &mut self,
        project: &Path,
        up_to_index: usize,
        workspace: &mut Workspace,
        docs: &Vfs,
        watch: &Watch,
    ) -> Result<Option<InferredPrefix>, Superseded> {
        let Some((resolved, env)) =
            self.resolved_prefix_and_env_watched(project, up_to_index, workspace, docs, watch)?
        else {
            return Ok(None);
        };
        let key = ProjectId::new(project);
        let want = up_to_index.saturating_add(1).min(resolved.len());
        let cached = self
            .inferred_projects
            .get(&key)
            .filter(|cached| Arc::ptr_eq(&cached.env, &env))
            .map(|cached| Arc::clone(&cached.inferred));
        if let Some(cached) = &cached
            && cached.shared_prefix(&resolved) >= want
        {
            return Ok(Some((resolved, env, Arc::clone(cached))));
        }
        let Some(files) = self
            .project_parses
            .get(&key)
            .map(|cached| cached.parses.files.clone())
        else {
            return Ok(None);
        };
        // Before the inference: the resolved prefix may have come from the
        // cache, with no checkpoint since the lookup began.
        watch.check()?;
        let inferred = {
            let _span = tracing::info_span!("infer_project").entered();
            let (inferred, reused) = match &cached {
                // Folded against this very env, so each file whose inputs and
                // preceding contributions are unchanged carries over.
                Some(prev) => {
                    borzoi_sema::infer_project_reusing_with_reuse(&files, &resolved, &env, prev)
                }
                None => (
                    borzoi_sema::infer_project(&files, &resolved, &env),
                    Vec::new(),
                ),
            };
            self.last_inference_reused_files = reused.iter().filter(|r| **r).count();
            Arc::new(inferred)
        };
        self.inferred_projects.insert(
            key.clone(),
            CachedInference {
                env: Arc::clone(&env),
                inferred: Arc::clone(&inferred),
            },
        );
        // Inference publishes its types into the env's type table, which the
        // cap bounds with the name table.
        self.drop_env_past_name_cap(&key, &env);
        Ok(Some((resolved, env, inferred)))
    }

    /// The [`ResolvedProject`] for `project` — the env-less view of
    /// [`Self::resolved_project_and_env_for`]. Prefer the paired method whenever
    /// you will classify or render a `Resolution::Entity` / `Resolution::Member`
    /// (their handles are only meaningful against the env the fold used).
    pub fn resolved_project_for(
        &mut self,
        project: &Path,
        workspace: &mut Workspace,
        docs: &Vfs,
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
        docs: &Vfs,
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
            .get(&ProjectId::new(project))
            .map(|(resolved, _env)| resolved.len())
    }

    /// The Compile-ordered parses for the project at `project`, preferring
    /// the in-memory buffer of each member file over its on-disk text.
    /// Returns `None` when the project failed to evaluate or evaluated only
    /// partially — sema can't fold over a Compile list we don't fully trust
    /// (matches [`Workspace::owning_project`]'s `Membership::Unknown`
    /// discipline).
    ///
    /// `docs` is the server's [`Vfs`] ([`crate::server::State::docs`]): each
    /// item's open buffer, else its disk text ([`Vfs::text_of`]). A cached
    /// answer is validated against it first (`Self::validate_cached_fold`).
    /// A file we
    /// can read neither from the buffer nor from disk is skipped — never an
    /// error, never a panic.
    pub fn parses_for_project<'a>(
        &'a mut self,
        project: &Path,
        workspace: &mut Workspace,
        docs: &Vfs,
    ) -> Option<&'a ProjectParses> {
        never_superseded(self.parses_for_project_watched(
            project,
            workspace,
            docs,
            &Watch::unwatched(),
        ))
    }

    /// [`Self::parses_for_project`], stopping at a checkpoint before each
    /// Compile item it parses once `watch` sees a notification
    /// ([`crate::inbox`]). The items parsed by then stay in the per-file
    /// caches, so the next call parses only the rest.
    pub fn parses_for_project_watched<'a>(
        &'a mut self,
        project: &Path,
        workspace: &mut Workspace,
        docs: &Vfs,
        watch: &Watch,
    ) -> Result<Option<&'a ProjectParses>, Superseded> {
        let key = ProjectId::new(project);
        self.validate_cached_fold(&key, workspace, docs);
        if !self.project_parses.contains_key(&key) {
            let stale = self.superseded.remove(&key);
            let (parses, inputs) = match self.fold(project, workspace, docs, watch) {
                Ok(Ok(folded)) => folded,
                Err(Superseded) => {
                    // Settled with every other stale fold when the loop
                    // stops the request ([`Self::settle_stale_folds`]).
                    if let Some(stale) = stale {
                        self.superseded.insert(key, stale);
                    }
                    return Err(Superseded);
                }
                Ok(Err(_)) => {
                    // A fold that dropped a stale one and is now refused
                    // changes what every open buffer shows.
                    if stale.is_some() {
                        self.wants_refresh = true;
                    }
                    return Ok(None);
                }
            };
            if let Some(stale) = &stale {
                self.owe_refresh_for_unannounced(stale, &parses, docs);
            }
            self.project_parses.insert(
                key.clone(),
                CachedParses {
                    parses,
                    inputs,
                    // The fold read the evaluation the workspace serves now.
                    evaluation: workspace.evaluation_id(&key),
                    // Every text was read through `docs` just now.
                    validated_in: docs.generation(),
                },
            );
        }
        Ok(self.project_parses.get(&key).map(|cached| &cached.parses))
    }

    /// Drop every env with an unrecorded reader
    /// ([`Self::invalidate_assembly_state`]) when `evaluations` is not the
    /// [`Workspace::projects_generation`] last seen: the workspace has
    /// dropped an evaluation, a file's owner has moved, or a structural watch
    /// event has arrived, any of which once dropped every env, and nothing
    /// else covers what such an env's unrecorded reader read. Every other
    /// cache records the evaluation it was derived from and is confirmed
    /// when read. Idempotent for one generation.
    pub fn sync_with_evaluations(&mut self, evaluations: ProjectsGeneration) {
        match self.evaluations_seen {
            Some(seen) if seen == evaluations => {}
            // Nothing has been computed yet, so nothing is stale.
            None => self.evaluations_seen = Some(evaluations),
            Some(_) => {
                self.invalidate_assembly_state();
                self.evaluations_seen = Some(evaluations);
            }
        }
    }

    /// Drop `key`'s cached parses and resolved fold if the evaluation the
    /// workspace serves no longer gives them the inputs they were built from
    /// ([`ParseInputs`]), or any Compile item's text is no longer the text
    /// they were built from. Called by every read of a cached fold; after
    /// the first call in a generation it costs one lookup of the
    /// evaluation's identity.
    ///
    /// First, `key`'s evaluation is confirmed for the generation
    /// ([`Workspace::validate`]), which evaluates it afresh if its reads
    /// moved. The parses read the evaluation only through [`ParseInputs`]
    /// (the Compile items in order, the `#if` symbols, the language version
    /// and its provenance), so a new evaluation is compared with them once,
    /// and one that gives the same inputs keeps the parses and the fold
    /// (`docs/incremental-lsp-plan.md`, "Follow-up: cutoffs on the
    /// evaluation's consumed outputs").
    ///
    /// Each item is asked of `docs` again: the open buffer, or a disk read the
    /// [`Vfs`] confirms with one `stat` per generation and answers with the
    /// allocation it gave before. So an unchanged item compares by pointer,
    /// and only a re-read compares bytes. Comparing texts rather than stamps
    /// keeps the fold when an item is rewritten to the text it had.
    fn validate_cached_fold(&mut self, key: &ProjectId, workspace: &mut Workspace, docs: &Vfs) {
        workspace.validate(key);
        self.sync_with_evaluations(workspace.projects_generation());
        let Some(cached) = self.project_parses.get_mut(key) else {
            return;
        };
        // The cutoff: an evaluation run again stands in for the one the
        // parses were found to agree with when it gives them the same
        // inputs, compared once per evaluation.
        let same_inputs = cached.evaluation == workspace.evaluation_id(key) || {
            let same =
                ParseInputs::of(key.as_path(), workspace).is_ok_and(|now| now == cached.inputs);
            if same {
                cached.evaluation = workspace.evaluation_id(key);
            }
            same
        };
        let generation = docs.generation();
        let current = same_inputs
            && ((generation.is_some() && cached.validated_in == generation)
                || cached
                    .parses
                    .paths
                    .iter()
                    .zip(&cached.parses.texts)
                    .all(|(path, text)| {
                        docs.text_of(path)
                            .is_some_and(|now| Arc::ptr_eq(&now, text) || *now == **text)
                    }));
        if current {
            cached.validated_in = generation;
            return;
        }
        tracing::debug!(
            project = %key.as_path().display(),
            "what a cached fold read from its evaluation, or a Compile item's text, moved; dropping it"
        );
        let stale = self.project_parses.remove(key);
        self.resolved_projects.remove(key);
        self.observed_fold_refusals.remove(key);
        if same_inputs {
            // Whether a refresh is owed is decided by the rebuild, which
            // reads every item's text anyway (`Self::parses_for_project`).
            self.superseded
                .extend(stale.map(|stale| (key.clone(), stale)));
        } else {
            // A moved evaluation is announced by the moved projects
            // generation, but the refresh is owed again here, for inputs
            // that moved, rather than reasoned about.
            self.wants_refresh = true;
        }
    }

    /// Owe a semantic-token refresh unless every Compile item whose text
    /// moved between `stale` and `fresh` (folded under the same
    /// [`ParseInputs`], so over one Compile list) moved to the text an open buffer holds now.
    /// A fold reads a buffer in preference to the disk, so such a move is
    /// the buffer's opening or one of its edits since `stale` was last
    /// confirmed, and each of those owed the refresh when it was synced
    /// ([`Self::owe_token_refresh`]). A move to disk text (a buffer closed,
    /// a write with a watch event or without one) owes it here, so an
    /// unwatched write is never taken for one the client was told of.
    fn owe_refresh_for_unannounced(
        &mut self,
        stale: &CachedParses,
        fresh: &ProjectParses,
        docs: &Vfs,
    ) {
        let synced_from_a_buffer =
            |path: &Path, now: &Arc<str>| synced_from_a_buffer(docs, path, now);
        let unannounced = stale.parses.paths != fresh.paths
            || stale
                .parses
                .paths
                .iter()
                .zip(stale.parses.texts.iter().zip(&fresh.texts))
                .filter(|(_, (before, now))| !(Arc::ptr_eq(before, now) || ***before == ***now))
                .any(|(path, (_, now))| !synced_from_a_buffer(path, now));
        if unannounced {
            self.wants_refresh = true;
        }
    }

    /// Owe a semantic-token refresh for every fold found stale whose rebuild
    /// has not finished, and forget them. The loop calls this whenever it
    /// stops a request: the rebuild may never finish (a request stopped by its
    /// own cancellation is refused, not run again), wherever the stop came,
    /// before the rebuild or during it. Deciding as the rebuild would
    /// (comparing each item's text with the one the stale fold read) would
    /// read every remaining item before the notification is handled, so the
    /// refresh is owed
    /// whether or not a buffer announced the change: at most one more than a
    /// finished rebuild would owe, never one fewer.
    pub fn settle_stale_folds(&mut self) {
        if !self.superseded.is_empty() {
            self.superseded.clear();
            self.wants_refresh = true;
        }
    }

    /// The one place the fold runs, so that **every** outcome is observed no
    /// matter which caller provoked it.
    ///
    /// The outcome is recorded for [`Self::fold_outcome`] to report. Without
    /// this, a refusal introduced *after* the file was opened — an edit that
    /// makes a Compile item straddle a language-version shape boundary, a sibling
    /// source deleted on disk — would be reached only from a handler, which has
    /// no connection to the client, and the project would go quiet with no
    /// message. A success clears any prior observation.
    ///
    /// A fold stopped at a checkpoint ([`Superseded`]) has no outcome: it
    /// neither refused nor succeeded, and what was observed before stands.
    fn fold(
        &mut self,
        project: &Path,
        workspace: &mut Workspace,
        docs: &Vfs,
        watch: &Watch,
    ) -> Result<Result<(ProjectParses, ParseInputs), FoldRefusal>, Superseded> {
        let key = ProjectId::new(project);
        let outcome = match ParseInputs::of(key.as_path(), workspace) {
            Ok(inputs) => build_parses(
                key.as_path(),
                &inputs,
                docs,
                &mut self.parses,
                &mut self.file_parses,
                watch,
            )?
            .map(|parses| (parses, inputs)),
            Err(refusal) => Err(refusal),
        };
        match &outcome {
            Ok(_) => {
                // A refusal recovering changes every open buffer's tokens
                // from single-file to folded. An edit or a watch event owes
                // that refresh itself; an unwatched disk write the fold
                // itself noticed has nothing else to owe it.
                if self.observed_fold_refusals.remove(&key).is_some() {
                    self.wants_refresh = true;
                }
            }
            Err(refusal) => {
                let evaluation = workspace.evaluation_id(&key);
                self.observed_fold_refusals
                    .insert(key, (refusal.clone(), evaluation));
            }
        }
        Ok(outcome)
    }

    /// What the most recent fold of `project` did, when it ran under
    /// `evaluation` (the evaluation the workspace serves now:
    /// [`Workspace::evaluation_id`]), or [`FoldOutcome::Unknown`] if no fold
    /// has run under it.
    ///
    /// Read as last confirmed, as the deferral refresh reads the evaluation
    /// itself: a Compile item that moved since the fold read it is noticed
    /// by the next request that needs the fold, which folds a refused
    /// project again (it caches no parses) and validates a folded one. The
    /// report is corrected then, not before.
    ///
    /// The shell reads this to explain a project that has gone quiet. Reading
    /// rather than draining is deliberate: the message is then a pure function
    /// of current state, so a fold that recovers stops being reported *because
    /// there is nothing to report*, not because some other code path remembered
    /// to undo an earlier notification.
    pub fn fold_outcome(
        &self,
        project: &ProjectId,
        evaluation: Option<EvaluationId>,
    ) -> FoldOutcome<'_> {
        match self.observed_fold_refusals.get(project) {
            Some((refusal, refused_under)) if *refused_under == evaluation => {
                FoldOutcome::Refused(refusal)
            }
            Some(_) => FoldOutcome::Unknown,
            None if self
                .project_parses
                .get(project)
                .is_some_and(|cached| cached.evaluation == evaluation) =>
            {
                FoldOutcome::Succeeded
            }
            None => FoldOutcome::Unknown,
        }
    }
}

/// Whether `now`, the text `path` reads, is the open buffer's: the buffer's
/// opening or one of its edits, each of which owed the refresh when it was
/// synced ([`SemanticState::owe_token_refresh`]).
fn synced_from_a_buffer(docs: &Vfs, path: &Path, now: &Arc<str>) -> bool {
    docs.buffer_uri(path)
        .and_then(|uri| docs.get(uri))
        .is_some_and(|buffer| Arc::ptr_eq(buffer, now))
}

/// Everything a project's parses read from its evaluation: every input of
/// [`build_parses`] that is not a source text
/// (`docs/incremental-lsp-plan.md`, "Follow-up: cutoffs on the evaluation's
/// consumed outputs").
///
/// [`build_parses`] is handed this and the [`Vfs`], never the workspace,
/// so it can read nothing else of the evaluation: two evaluations that give
/// equal inputs give equal parses of equal texts. A cached fold therefore
/// outlives an evaluation run again to the same values (a comment or a
/// property nothing reads added to the `.fsproj`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParseInputs {
    /// The Compile items (`CompileBefore`, `Compile`, `CompileAfter`), in
    /// the order the evaluation lists them: the fold's order.
    items: Vec<PathBuf>,
    /// The `#if` symbols every item is parsed under.
    symbols: HashSet<String>,
    /// The language version every item is parsed at.
    lang: LanguageVersion,
    /// Why the evaluation's `LangVersion` is untrusted, if it is: the
    /// version-boundary gate runs exactly when it is, and a refusal it
    /// makes names it.
    lang_version_untrust: Option<UntrustedCause>,
}

impl ParseInputs {
    /// What the evaluation of `project` the workspace serves now gives the
    /// parses, or the refusal of a project whose evaluation declines the
    /// fold.
    fn of(project: &Path, workspace: &mut Workspace) -> Result<ParseInputs, FoldRefusal> {
        let symbols = workspace.symbols_for_project(project);
        let lang = workspace.lang_version_for_project(project);
        // Every exit below is a *value*, not a bare `None`: a refusal the user is
        // never told about is the defect `crate::project_deferral` exists to close,
        // and only a typed refusal makes "we went quiet" and "we said why" the same
        // decision at every exit, not just the first.
        if crate::project_deferral::evaluation_declines_project_fold(
            workspace.project_evaluation(project),
        ) {
            return Err(FoldRefusal::ProjectEvaluation);
        }
        let parsed = workspace
            .project(project)
            .expect("the evaluation gate above accepted this project");
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
        //
        // Both are asked through `crate::project_deferral::evaluation_declines_project_fold`
        // above, so that the refusal and the editor message the user gets for it are
        // the same decision.
        // `LangVersion` provenance is the third axis, gated per *source shape*
        // below rather than here: the language version shapes the token stream
        // (the lex-filter's `relaxWhitespace2` pops at the F# 6 boundary and its
        // strict-indentation push decision at F# 8), so an unknowable version can
        // mis-shape the fold. An ordinary SDK project's provenance is trusted (the
        // SDK's own `LangVersion` writes sit under gates the walker decides —
        // `sdk_project_fold_e2e::ordinary_sdk_projects_have_trusted_langversion_provenance`),
        // so this axis bites only on a project whose `LangVersion` write sits under
        // a gate the walker cannot pin. Even there the mark alone would refuse
        // every file, however version-invariant; instead each parse reports
        // whether its shape actually depends on the version
        // (`Parse::shape_depends_on_language_version`, whose `false` *proves*
        // version-invariance), and the fold refuses only the intersection:
        // untrusted provenance × a file that genuinely straddles the boundary.
        let lang_version_untrust = parsed.property_untrusted_cause("LangVersion").cloned();
        // Accept every Compile flavour `ParsedProject.items` produces:
        // `[CompileBefore, Compile, CompileAfter]` — F#-specific item kinds the
        // msbuild parser already orders for us. Excluding any of them would
        // silently drop real source from projects like FSharp.Core that depend
        // on `<CompileBefore>` / `<CompileAfter>`. `ProjectReference` is *not*
        // in `items` (it lives on `project_references`); even so, we filter
        // defensively in case the parser's split ever loosens.
        let items: Vec<PathBuf> = parsed
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
        Ok(ParseInputs {
            items,
            symbols,
            lang,
            lang_version_untrust,
        })
    }
}

/// Build the compile-ordered parses for a fresh cache miss from what the
/// evaluation gives them (`inputs`) and the texts `docs` holds. Pure
/// (modulo disk reads for files not in `docs`).
///
/// Stops at a checkpoint before each Compile item once `watch` sees a
/// notification: every item parsed by then is in `file_parses`, so the next
/// build finds it there.
fn build_parses(
    project: &Path,
    inputs: &ParseInputs,
    docs: &Vfs,
    parses: &mut ParseMemo,
    file_parses: &mut HashMap<PathBuf, Vec<CachedParse>>,
    watch: &Watch,
) -> Result<Result<ProjectParses, FoldRefusal>, Superseded> {
    let _span = tracing::info_span!("build_parses", project = %project.display()).entered();
    let ParseInputs {
        items: includes,
        symbols,
        lang,
        lang_version_untrust,
    } = inputs;
    let lang = *lang;
    let lang_version_untrusted = lang_version_untrust.is_some();

    let mut files = Vec::with_capacity(includes.len());
    let mut paths = Vec::with_capacity(includes.len());
    let mut texts = Vec::with_capacity(includes.len());
    let mut recoveries = Vec::with_capacity(includes.len());

    let _parse_all_span =
        tracing::info_span!("parse_compile_items", count = includes.len()).entered();
    for include in includes.iter().cloned() {
        // A checkpoint before each item: each one before it is cached.
        watch.check()?;
        let _file_span =
            tracing::info_span!("parse_compile_item", file = %include.display()).entered();
        // **Hard fail on any hole.** Sema's fold is order-sensitive: a later
        // file may reference (and shadow) earlier exports. If we silently
        // skipped a Compile item the queried file depended on, its references
        // could bind to a same-named assembly entity behind the missing
        // module — a *wrong* go-to-def, not just under-resolution. Per
        // correctness-over-availability we refuse the project; handlers fall
        // back to single-file `resolve_file` for orphan-style queries.
        let Some(text) = docs.text_of(&include) else {
            tracing::warn!(
                file = %include.display(),
                "Compile item unreadable from buffer and disk; refusing to resolve project"
            );
            return Ok(Err(FoldRefusal::UnreadableCompileItem { file: include }));
        };
        // A `.fsi` Compile item parses under the signature grammar
        // (`docs/fsi-signature-restriction-plan.md` Stage 1); everything else
        // is an implementation file.
        let signature = is_signature_file(&include);
        let file_kind = if signature {
            FileKind::Sig
        } else {
            FileKind::Impl
        };
        // Per-file fold cache: reuse a variant whose every deciding input — the
        // source text, the `#if` symbols, the language version, and the project's
        // `LangVersion`-provenance trust — matches. A hit is a handle clone
        // that also *skips* the version-boundary gate below, which is why
        // `lang_version_untrusted` is part of the match (it is that gate's one
        // non-parse input). A miss falls through to the parse memo (and, for a
        // straddle-flagged file, re-runs the gate) and records a variant. The
        // input comparison is what keeps a stale variant from being served, so no
        // invalidation hook is load-bearing.
        //
        // The text compares by pointer first: the `Vfs` hands out one
        // allocation per unchanged file. A byte-equal hit adopts the `Vfs`'s
        // allocation, so the next comparison is a pointer test again.
        if let Some(hit) = file_parses.get_mut(&include).and_then(|variants| {
            variants
                .iter_mut()
                .find(|c| {
                    c.parse.lang() == lang
                        && c.parse.file_kind() == file_kind
                        && c.lang_version_untrusted == lang_version_untrusted
                        && c.parse.symbols() == symbols
                        && (Arc::ptr_eq(&c.text, &text) || *c.text == *text)
                })
                .map(|c| {
                    c.text = Arc::clone(&text);
                    (c.file.clone(), c.recovery.clone())
                })
        }) {
            let (source_file, recovery) = hit;
            files.push(source_file);
            paths.push(include);
            texts.push(text);
            recoveries.push(recovery);
            continue;
        }
        // The memo may already hold this parse: the syntactic diagnostics of
        // an edited buffer parse it before the fold does.
        let source = parses.parse(Some(&include), &text, file_kind, symbols, lang);
        let Some(parse) = source.parse() else {
            // The wrapper returns `None` only when the CST parser panicked.
            // Logged inside the wrapper.
            tracing::warn!(
                file = %include.display(),
                "Compile item triggered parser panic; refusing to resolve project"
            );
            return Ok(Err(FoldRefusal::ParserPanic { file: include }));
        };
        // The intersection gate promised above: this file MAY parse to a
        // differently shaped tree at another language version (a decision the
        // lex-filter makes differently across the F# 6 `relaxWhitespace2` or
        // the F# 8 strict-indentation boundary), and the project's
        // `LangVersion` provenance can't tell us which version the real build
        // uses. The flag is a sound *over*-approximation (an EOF-anchored push
        // — `module M =` at end of file, a common mid-edit state — differs as
        // a stack operation but reconverges via the EOF cascade), so acting on
        // the flag alone would flicker an untrusted project's fold off while
        // the user types. Verify: re-parse
        // at the representative of every *other* shape band (those gates are
        // the only shape inputs, so one version per band decides) and refuse
        // only genuine divergence — folding then could export bindings from a
        // tree the compiler never sees; handlers fall back to single-file
        // resolution (best-guess version, accepted best-effort). The extra
        // parses run only for flagged files in untrusted-provenance projects.
        if let Some(untrust) = &lang_version_untrust
            && parse.shape_depends_on_language_version
        {
            let own_band = lang.shape_representative();
            for other_side in LanguageVersion::SHAPE_REPRESENTATIVES {
                if other_side == own_band {
                    continue;
                }
                let other = if signature {
                    parse_sig_with_symbols(&text, symbols, other_side)
                } else {
                    parse_with_symbols(&text, symbols, other_side)
                };
                let Some(other) = other else {
                    tracing::warn!(
                        file = %include.display(),
                        "Compile item triggered parser panic; refusing to resolve project"
                    );
                    return Ok(Err(FoldRefusal::ParserPanic { file: include }));
                };
                // `structurally_eq` compares shape, kinds and token text, an
                // allocation-free tree comparison; both directions are pinned by the mid-edit
                // (reconverges, folds) and straddling (differs, refuses) tests.
                if !other.root.structurally_eq(&parse.root) {
                    tracing::info!(
                        project = %project.display(),
                        file = %include.display(),
                        other_version = %other_side,
                        "refusing project parses: LangVersion provenance is untrusted and \
                         this file parses to a different tree at another language version"
                    );
                    return Ok(Err(FoldRefusal::LanguageVersionShape {
                        file: include,
                        cause: untrust.clone(),
                    }));
                }
            }
        }
        // Before `parse.root` is moved into the `SourceFile`: the errors and the
        // tree describe the same parse, and only here are both still in hand.
        //
        // Under untrusted `LangVersion` provenance the version is a guess, and
        // a guess cannot prove a clean read of a file whose diagnostics are
        // version-gated — the tree is identical either side of the threshold,
        // so the shape gate above never fires on it.
        let recovery = if lang_version_untrusted {
            SyntaxRecovery::of_guessed_version(parse, &text, symbols, file_kind)
        } else {
            SyntaxRecovery::of(parse)
        };
        let source_file = if signature {
            match SigFile::cast(parse.root.clone()) {
                Some(sig) => SourceFile::Sig(sig),
                None => {
                    tracing::warn!(
                        file = %include.display(),
                        "Compile item parsed to a non-SIG_FILE root; refusing to resolve project"
                    );
                    return Ok(Err(FoldRefusal::UnexpectedParseRoot { file: include }));
                }
            }
        } else {
            match ImplFile::cast(parse.root.clone()) {
                Some(impl_file) => SourceFile::Impl(impl_file),
                None => {
                    tracing::warn!(
                        file = %include.display(),
                        "Compile item parsed to a non-IMPL_FILE root; refusing to resolve project"
                    );
                    return Ok(Err(FoldRefusal::UnexpectedParseRoot { file: include }));
                }
            }
        };
        // Cache miss resolved: record the freshly parsed tree as this path's
        // variant for the current settings-tuple. Replace the same-tuple variant
        // if present (a new text after an edit — no growth on repeated edits),
        // else append (a linked file under another project's settings — kept
        // alongside so neither project's build thrashes the other's).
        let variant = CachedParse {
            parse: Arc::clone(&source),
            lang_version_untrusted,
            text: Arc::clone(&text),
            file: source_file.clone(),
            recovery: recovery.clone(),
        };
        let variants = file_parses.entry(include.clone()).or_default();
        if let Some(slot) = variants.iter_mut().find(|c| {
            c.parse.lang() == lang
                && c.parse.file_kind() == file_kind
                && c.lang_version_untrusted == lang_version_untrusted
                && c.parse.symbols() == symbols
        }) {
            *slot = variant;
        } else {
            variants.push(variant);
        }
        files.push(source_file);
        paths.push(include);
        texts.push(text);
        recoveries.push(recovery);
    }
    drop(_parse_all_span);

    // The sig ↔ impl pairing key, derived over the *whole* Compile order (the
    // deduplication half of FCS's `QualifiedNameOfFile` is stateful across
    // files, so it cannot be computed — or cached — per file).
    let qnofs = borzoi_sema::qualified_names(&files, &paths);
    let files = files
        .into_iter()
        .zip(qnofs)
        .zip(recoveries)
        .map(|((file, qnof), recovery)| ProjectFile::new(file, qnof, recovery))
        .collect();

    Ok(Ok(ProjectParses {
        files,
        paths,
        texts,
    }))
}

/// A Compile prefix resolved, with the env it was resolved against
/// ([`SemanticState::resolved_prefix_and_env_watched`]).
pub type ResolvedPrefix = (Arc<ResolvedProject>, Arc<AssemblyEnv>);

/// A Compile prefix resolved and inferred, with the env both ran against
/// ([`SemanticState::inferred_prefix_watched`]).
pub type InferredPrefix = (Arc<ResolvedProject>, Arc<AssemblyEnv>, Arc<InferredProject>);

/// An [`AssemblyEnv`] and the reference-DLL list it was built from: one
/// resolution's two outputs, cached as one value so nothing can pair a
/// reference set with an env built from a *different* resolution (see
/// [`SemanticState::env_reference_dlls_for_project`]).
#[derive(Debug, Clone)]
struct CachedAssemblyEnv {
    env: Arc<AssemblyEnv>,
    dlls: Arc<[PathBuf]>,
    /// What the resolution that built `env` read.
    reads: Arc<EnvReads>,
}

/// A cached portable-PDB image ([`SemanticState::pdb_images`]): the DLL's
/// stamp when it was read, and the image, or `None` for a DLL with none.
type PdbImage = (msbuild_observed::FileStamp, Option<Arc<[u8]>>);

/// One cached env and what it depends on
/// ([`SemanticState::assembly_envs`]): what it was built from, what
/// resolving that read, and what building the env from it read.
#[derive(Debug)]
struct EnvEntry {
    cached: CachedAssemblyEnv,
    /// What the env was built from besides its DLLs' bytes.
    inputs: EnvInputs,
    /// What to confirm before trusting `inputs` again: what resolving them
    /// read (the graph walk and the evaluations behind it, the assets
    /// files, the packs, the output search), less the contents of the
    /// installed versions it trusts. When one moves, the references are
    /// resolved again and compared with `inputs`.
    resolution: EnvChecks,
    /// What to confirm before serving `cached` again: what building it from
    /// `inputs` read (each DLL's stamp), less the installed contents it
    /// trusts.
    contents: EnvChecks,
    /// The read set behind `contents`, which joins a new resolution's in
    /// `cached.reads`.
    contents_reads: ReadSet,
    /// The generation that last confirmed `checks` (or that built the env);
    /// `None` when it was built outside a generation.
    confirmed_in: Option<u64>,
    /// A watch event arrived since the env was last confirmed inside a
    /// generation, so the next confirmation covers the installed contents
    /// it trusts too ([`Workspace::confirm_env`]'s `after_event`).
    event_since: bool,
}

/// What became of the env a cached fold was folded against
/// ([`SemanticState::env_still_served`]).
#[derive(Debug)]
enum FoldEnv {
    /// Still the env its key serves: the fold stands.
    Served,
    /// Its references resolve differently now: this env was built from that
    /// resolution (and cached unless it is retryable, the flag beside it),
    /// and the fold must be folded again against it.
    Replaced(Arc<AssemblyEnv>, bool),
    /// No longer served, and nothing built in its place.
    Gone,
}

/// What [`SemanticState::confirmed_env`] found of a cached env.
#[derive(Debug)]
enum EnvLookup {
    /// Confirmed for the open generation (or served as it is outside one).
    Served(CachedAssemblyEnv),
    /// Not cached, or dropped because a DLL it holds moved.
    Absent,
    /// What building it read holds and what resolving its references read
    /// does not: resolve them again, and keep the env if they resolve to the
    /// inputs it was built from ([`SemanticState::keep_if_resolved_alike`]).
    ResolutionMoved,
}

/// The span a resolution run to confirm a cached env opens
/// (`SemanticState::keep_if_resolved_alike`), whether it keeps the env or
/// not.
pub const ENV_RESOLVED_AGAIN_SPAN: &str = "semantic.env_resolved_again";

/// What one resolution of an assembly env read
/// (`docs/incremental-lsp-plan.md`, D3, Stage 6a): every query it recorded,
/// with its answer, and the readers that answered part of it without
/// recording what they read.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EnvReads {
    /// The recorded queries: the project-graph walk's probes and the read
    /// sets of the evaluations it ran or confirmed, the assets files, the
    /// framework packs and packages, the output-DLL search, and each DLL's
    /// stamp.
    pub reads: ReadSet,
    /// The readers whose queries `reads` lacks: [`UNRECORDED_RESTORE`] and
    /// [`UNRECORDED_CSHARP`]. Empty exactly when `reads` is everything the
    /// resolution depends on.
    pub unrecorded: Vec<&'static str>,
}

/// An on-demand restore ran, and read a warm package cache and a scratch
/// directory it deletes (`crate::restore`, an `Unobserved::Restore` reader),
/// whether it answered the resolution's artifacts or declined.
pub const UNRECORDED_RESTORE: &str = "an on-demand restore's scratch output";

/// The C# sidecar was asked for a C# reference, and read the C# sources
/// itself (the `CSharpSources` input class, unwitnessed by design), whether
/// it delivered metadata or not.
pub const UNRECORDED_CSHARP: &str = "the C# sources the sidecar compiled";

/// Everything an assembly env is built from besides the bytes of the DLLs
/// it reads: the outputs of its reference resolution that
/// [`build_assembly_env`] reads (`docs/incremental-lsp-plan.md`, "Follow-up:
/// cutoffs on the evaluation's consumed outputs").
///
/// The env is a function of these and of each DLL's bytes, which its
/// contents checks confirm by stamp: [`build_assembly_env`] is handed this
/// and the assembly cache, never the resolution, the workspace or the
/// evaluations behind it. So a cached env whose resolution's reads moved
/// (the `.fsproj` edited, a referenced project's evaluation run again, an
/// assets file rewritten) is resolved again, and kept when the resolution
/// gives equal inputs ([`SemanticState::confirmed_env`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvInputs {
    /// The reference DLLs, in fold order: an assembly's identity in the env
    /// is its position.
    dlls: Vec<PathBuf>,
    /// Some declared reference could not be read
    /// ([`ReferenceOutcome::Unread`]), which marks the env incomplete.
    incomplete: bool,
}

impl EnvInputs {
    fn of(refs: &ReferenceSet) -> EnvInputs {
        EnvInputs {
            dlls: refs.dlls.clone(),
            incomplete: refs.unread().next().is_some(),
        }
    }
}

/// Build the assembly env for `project` from its resolved references
/// (`refs`). Always returns *some* env — empty if the resolution declined
/// (D5 degrades to under-resolution) — and the DLLs it holds. It reads
/// nothing of `refs` but its [`EnvInputs`].
fn build_assembly_env(
    project: &Path,
    refs: &ReferenceSet,
    cache: &AssemblyCache,
    observer: &Observer,
) -> (AssemblyEnv, Vec<PathBuf>) {
    let _span = tracing::info_span!("build_assembly_env", project = %project.display()).entered();
    // A declared reference we **tried and failed to read** leaves its identity
    // and types unknown, which is the same hazard a skipped DLL poses and the
    // one `identities_incomplete` models — but it never reaches
    // `build_env_from_dll_paths`, because the failure happened before there was
    // a path to hand it. So the mark is applied here, where the reference-side
    // outcomes live, rather than inside the projector.
    let unread: Vec<(&Path, &str)> = refs.unread().collect();
    if !unread.is_empty() {
        tracing::warn!(
            project = %project.display(),
            references = ?unread,
            "project references could not be read; the env's identity set is incomplete"
        );
    }
    // Unbuilt references are logged but deliberately not an incompleteness —
    // see `ReferenceOutcome::NotBuilt` for the trade.
    let not_built: Vec<&Path> = refs.not_built().collect();
    if !not_built.is_empty() {
        tracing::info!(
            project = %project.display(),
            references = ?not_built,
            "project references have no built output; their types are unavailable until built"
        );
    }
    build_env(&EnvInputs::of(refs), cache, observer)
}

/// The env [`EnvInputs`] describe, and the DLLs it holds (those of
/// `inputs.dlls` that could be read).
fn build_env(
    inputs: &EnvInputs,
    cache: &AssemblyCache,
    observer: &Observer,
) -> (AssemblyEnv, Vec<PathBuf>) {
    let (mut env, kept) =
        build_env_from_dll_paths(inputs.dlls.iter().map(PathBuf::as_path), cache, observer);
    if inputs.incomplete {
        // Both marks, exactly as `build_env_from_dll_paths` applies them to the
        // equivalent whole-DLL loss. An unread reference could carry an
        // assembly-level `[<AutoOpen>]` reaching any namespace, or a C#-style
        // `[<Extension>]` class, and the OV-6 gate treats an unmarked surface as
        // complete — so marking only the identity set would leave the
        // bare-value, attribute and overload gates committing past it.
        env.mark_referenced_assemblies_incomplete();
        env.mark_extension_surface_unknowable();
    }
    (env, kept)
}

/// What one declared `<ProjectReference>` edge contributed to the env.
///
/// Every edge in [`GraphRefTargets`] yields exactly one of these. That
/// conservation is the point of the type: an omission used to be *inferred*
/// from a shortfall — a reference that resolved to no DLL simply left nothing
/// behind, indistinguishable from one that was never declared — so nothing
/// downstream could tell that a reference was missing at all. It is the same
/// shape the per-path `SkipCause` replaced in [`build_env_from_dll_paths`],
/// and for the same reason: a count cannot say *which* thing is absent, or why.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReferenceOutcome {
    /// The edge contributed these DLLs. A C# edge contributes its whole
    /// transitive closure, so this is not always a single path; an F# one is.
    Resolved(Vec<PathBuf>),
    /// We looked where this project's output should be and found nothing —
    /// usually because nobody has built it. See [`OutputLocation::Absent`] for
    /// what "should be" can miss.
    ///
    /// Its types are unknown, exactly as an unread DLL's are — and this
    /// deliberately **does not** mark the env incomplete, which is unsound and
    /// is a decision rather than an oversight. Every project reference is
    /// unbuilt in a freshly cloned, restored-but-unbuilt workspace, so grading
    /// this with the rest would take the whole project's referenced-assembly
    /// surface — the BCL included — dark until someone runs a build, to guard
    /// against a sibling project shadowing a framework type. The durable fix is
    /// to resolve project references from *source* (what FCS does), which
    /// removes the choice; until then the trade is stated, not hidden.
    NotBuilt,
    /// We tried to read the reference and could not: a sidecar build or
    /// transport failure, no producer TFM, no trustworthy TFM or output-name
    /// verdict. Its identity and types are unknown, which is the hazard
    /// [`AssemblyEnv::identities_incomplete`] models, so this marks the env
    /// incomplete and every assembly reading defers.
    ///
    /// Unlike [`Self::NotBuilt`] there is nothing the user can obviously do,
    /// and no reason to expect the state to clear on its own — declining is
    /// both sound and the only honest answer.
    ///
    /// `transient` marks the ones that *may* clear without any edit: an I/O
    /// error while scanning the output tree, as against a verdict we will keep
    /// declining to trust. The resulting env must not be cached, or the
    /// filesystem recovers and every assembly reading goes on deferring until
    /// some unrelated watched file happens to invalidate the entry.
    Unread { why: &'static str, transient: bool },
}

/// One declared project-reference edge and what it contributed. See
/// [`ReferenceOutcome`] for why every edge gets an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReferenceEdge {
    /// The referenced project file, canonicalised as the graph walk saw it.
    project: PathBuf,
    outcome: ReferenceOutcome,
}

/// Everything [`resolve_reference_dlls`] found: the DLL list the env folds, and
/// one [`ReferenceEdge`] per declared project-reference edge saying what became
/// of it.
struct ReferenceSet {
    /// The composed reference DLLs, in fold order (package, framework, F# refs,
    /// C# refs) — the order matters, because [`build_env_from_dll_paths`]
    /// assigns each assembly an id by position.
    dlls: Vec<PathBuf>,
    /// One entry per declared edge, in `fsharp` then `csharp` order.
    edges: Vec<ReferenceEdge>,
    /// See [`build_assembly_env`]'s *retryable*.
    retryable: bool,
    /// The readers whose queries the resolution could not record
    /// ([`EnvReads::unrecorded`]).
    unrecorded: Vec<&'static str>,
    /// The package folders the assets file the packages came from names
    /// ([`crate::project_assets::ResolvedAssemblies::package_folders`]).
    package_folders: Vec<PathBuf>,
}

impl ReferenceSet {
    /// The resolution declined before it reached the project-reference edges —
    /// an untrusted TFM verdict, no dotnet root, no assets file, a failed
    /// resolve. The env is empty, so it makes no assembly reading at all and
    /// there is nothing for a missing reference to be wrong about; claiming
    /// edges here would only invent a degradation nobody can observe.
    fn declined(retryable: bool) -> Self {
        ReferenceSet {
            dlls: Vec::new(),
            edges: Vec::new(),
            retryable,
            unrecorded: Vec::new(),
            package_folders: Vec::new(),
        }
    }

    /// The references we tried to read and could not, with the cause — the
    /// grade that makes the env's identity set incomplete.
    fn unread(&self) -> impl Iterator<Item = (&Path, &'static str)> {
        self.edges.iter().filter_map(|e| match e.outcome {
            ReferenceOutcome::Unread { why, .. } => Some((e.project.as_path(), why)),
            ReferenceOutcome::Resolved(_) | ReferenceOutcome::NotBuilt => None,
        })
    }

    /// Whether any edge failed for a reason that may clear on its own, so the
    /// env built from this set must not be cached.
    fn has_transient_failure(&self) -> bool {
        self.edges.iter().any(|e| {
            matches!(
                e.outcome,
                ReferenceOutcome::Unread {
                    transient: true,
                    ..
                }
            )
        })
    }

    /// The referenced projects nobody has built yet. Deliberately *not* an
    /// incompleteness (see [`ReferenceOutcome::NotBuilt`]); surfaced so the user
    /// can be told which project to build.
    fn not_built(&self) -> impl Iterator<Item = &Path> {
        self.edges.iter().filter_map(|e| match e.outcome {
            ReferenceOutcome::NotBuilt => Some(e.project.as_path()),
            ReferenceOutcome::Resolved(_) | ReferenceOutcome::Unread { .. } => None,
        })
    }
}

/// The DLLs `edges` contributed, sorted and deduped — two edges can reach the
/// same file (a diamond in the C# closure), and the env must fold it once.
fn edge_dlls(edges: &[ReferenceEdge]) -> Vec<PathBuf> {
    let mut dlls: Vec<PathBuf> = edges
        .iter()
        .filter_map(|e| match &e.outcome {
            ReferenceOutcome::Resolved(paths) => Some(paths.iter().cloned()),
            ReferenceOutcome::NotBuilt | ReferenceOutcome::Unread { .. } => None,
        })
        .flatten()
        .collect();
    dlls.sort();
    dlls.dedup();
    dlls
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
    /// Nodes whose own `<ProjectReference>` list the walk **suppressed**
    /// ([`crate::project_graph::ProjectNode::references_uncertain`]), rather
    /// than finding empty.
    ///
    /// Their references were dropped before this set was built, so they are
    /// absent from [`Self::fsharp`] and [`Self::csharp`] and no per-edge
    /// outcome can be produced for them — the conservation count would agree
    /// with itself over a set they had already been filtered out of. Naming
    /// the *suppressing node* is what survives the loss: one
    /// [`ReferenceOutcome::Unread`] per such node says an unknown number of
    /// assemblies is missing, which is exactly the claim the incompleteness
    /// mark needs.
    suppressed: Vec<PathBuf>,
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
fn graph_ref_targets(graph: &ProjectGraph, entry: &Path, observer: &Observer) -> GraphRefTargets {
    let entry_key = lexically_normalize(entry);
    let mut targets = GraphRefTargets::default();
    for node in &graph.nodes {
        match node.kind {
            ProjectKind::FSharp => {
                // Recorded for the entry too: the entry's own suppressed
                // references are missing from the env just as a sibling's are.
                if node.references_uncertain {
                    targets.suppressed.push(canonicalise(&node.path, observer));
                }
                if !paths_equal(&node.path, &entry_key) {
                    targets.fsharp.push(FsharpRefTarget {
                        path: canonicalise(&node.path, observer),
                        tfm: node.tfm.clone(),
                        output_name: node.output_name.clone(),
                    });
                }
            }
            ProjectKind::CSharp => targets.csharp.push(canonicalise(&node.path, observer)),
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
#[allow(clippy::too_many_arguments)]
fn resolve_reference_dlls(
    project: &Path,
    dotnet_root: Option<&Path>,
    target_framework: &ServedTfm,
    sidecar: CsharpReferences<'_>,
    ref_targets: &GraphRefTargets,
    recovered_ref_tfms: &BTreeMap<PathBuf, String>,
    restore_env: Option<&SdkDiscoveryEnv>,
    observer: &Observer,
) -> ReferenceSet {
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
        return ReferenceSet::declined(false);
    }
    let Some(dotnet_root) = dotnet_root else {
        tracing::info!(
            project = %project.display(),
            "no dotnet_root; assembly env defaults to empty"
        );
        return ReferenceSet::declined(false);
    };
    let assets_path = match project.parent() {
        Some(dir) => dir.join("obj").join("project.assets.json"),
        None => {
            tracing::info!(
                project = %project.display(),
                "project path has no parent; assembly env defaults to empty"
            );
            return ReferenceSet::declined(false);
        }
    };
    // Assets file present → read restore output (the authoritative path).
    // Absent → run an on-demand `dotnet restore` to a scratch dir and read that
    // (see [`crate::restore`]): the real restore is exact by construction, and
    // reading its (scratch) assets yields the same [`ResolvedAssemblies`] the
    // present-file branch does, so cross-project F#/C# edges resolve in both
    // cases. A cold cache / timeout / no restore environment declines to today's
    // empty env.
    let mut unrecorded = Vec::new();
    let resolved = if msbuild_observed::is_file(observer, &assets_path) {
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
                Some(tfm) => resolve_assemblies_for_tfm(&assets_path, dotnet_root, tfm, observer),
                None => resolve_assemblies_root_only(&assets_path, dotnet_root, observer),
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
                return ReferenceSet::declined(false);
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
        // A restore that runs reads what it reads unrecorded, whatever it
        // concludes: a decline for a package missing from the warm cache
        // depends on that cache as a success does.
        if restore_env.is_some() {
            unrecorded.push(UNRECORDED_RESTORE);
        }
        match restore_to_scratch_assemblies(project, dotnet_root, target_framework, restore_env) {
            RestoreOutcome::Resolved(resolved) => resolved,
            RestoreOutcome::Declined => {
                tracing::info!(
                    project = %project.display(),
                    assets = %assets_path.display(),
                    "no project.assets.json and the on-demand restore declined; \
                     assembly env defaults to empty"
                );
                return ReferenceSet {
                    unrecorded,
                    ..ReferenceSet::declined(false)
                };
            }
            RestoreOutcome::TransientFailure => {
                // Don't cache: a spawn error / timeout / wedge may clear, and the
                // scratch dir is gone with no watched file to trigger a refresh,
                // so `retryable` is the only way a later request tries again.
                tracing::info!(
                    project = %project.display(),
                    "on-demand restore failed transiently; not caching, will retry"
                );
                return ReferenceSet {
                    unrecorded,
                    ..ReferenceSet::declined(true)
                };
            }
        }
    };
    // F# `<ProjectReference>` graph targets: fold each referenced project's
    // built output DLL (`bin/<config>/<tfm>/<TargetName>.dll`) into the same
    // env, so cross-project go-to-definition / references resolve into a
    // sibling F# project's types. A reference whose output isn't built is
    // silently skipped (D5: under-resolve, never wrong) — the same graceful
    // degradation the un-restored and unparseable-DLL paths already use.
    let fsharp_edges = fsharp_project_ref_edges(
        &ref_targets.fsharp,
        &resolved.project_ref_assembly_names,
        observer,
    );
    let ref_dlls = edge_dlls(&fsharp_edges);
    if !ref_dlls.is_empty() {
        tracing::info!(
            project = %project.display(),
            count = ref_dlls.len(),
            "including F# project-reference output DLLs in the assembly env"
        );
    }
    // C# `<ProjectReference>` graph targets: when the host opted in, the
    // sidecar emits a Roslyn metadata DLL for each (plus its transitive C#
    // closure); fold those in too. Each edge is unread when the host has not
    // opted in, the sidecar is unavailable, or a build fails (D5); a transport
    // failure additionally marks the env non-cacheable so the next request
    // retries against a respawned sidecar rather than serving this incomplete
    // one.
    let csharp_references_enabled = matches!(sidecar, CsharpReferences::Enabled(_));
    let (csharp_edges, retryable) = csharp_project_ref_edges(
        sidecar,
        project,
        dotnet_root,
        recovered_ref_tfms,
        &ref_targets.csharp,
        &resolved.project_ref_tfms,
        observer,
    );
    // The sidecar is consulted for any C# reference when the host enabled
    // it, and what it reads is unrecorded whatever it answers: a build that
    // fails on a compile error depends on the sources as one that succeeds.
    if csharp_references_enabled && !ref_targets.csharp.is_empty() {
        unrecorded.push(UNRECORDED_CSHARP);
    }
    let csharp_ref_dlls = edge_dlls(&csharp_edges);
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
    // One outcome per node whose references were suppressed. There is nothing
    // to look for — the edges are gone — so this is a pure incompleteness
    // claim, and `transient: false` because only an edit to that project can
    // change it.
    let suppressed_edges: Vec<ReferenceEdge> = ref_targets
        .suppressed
        .iter()
        .map(|project| ReferenceEdge {
            project: project.clone(),
            outcome: ReferenceOutcome::Unread {
                why: "the project's own reference list could not be trusted",
                transient: false,
            },
        })
        .collect();
    let edges: Vec<ReferenceEdge> = fsharp_edges
        .into_iter()
        .chain(csharp_edges)
        .chain(suppressed_edges)
        .collect();
    // Conservation: every edge the graph walk *retained* is accounted for, so a
    // reference that contributed nothing is recorded as missing rather than
    // inferred from a shortfall — the distinction the whole grade rests on. A
    // `debug_assert` rather than a hard error: a miscount would mean a
    // reference silently stopped being tracked, which the suite must catch, but
    // in release the right degradation is to serve what we have.
    //
    // A project whose `<ProjectReference>`s the evaluator could not trust has
    // its *entire* edge list emptied by `workspace::edges_of` before
    // `GraphRefTargets` is built. Those references cannot be counted — they are
    // gone — so the node that lost them is counted instead
    // ([`GraphRefTargets::suppressed`]), one outcome each.
    debug_assert_eq!(
        edges.len(),
        ref_targets.fsharp.len() + ref_targets.csharp.len() + ref_targets.suppressed.len(),
        "every retained project-reference edge, and every node whose edges were \
         suppressed, must yield exactly one outcome"
    );
    // A transient F# output-scan failure has no `retryable` channel of its own —
    // that flag came from the sidecar — but it needs one for the same reason:
    // the env it produced is incomplete for a cause that may clear, and caching
    // it would keep every assembly reading deferring long after the filesystem
    // recovered.
    let set = ReferenceSet {
        dlls,
        edges,
        retryable,
        unrecorded,
        package_folders: resolved.package_folders,
    };
    ReferenceSet {
        retryable: set.retryable || set.has_transient_failure(),
        ..set
    }
}

/// Whether the env build may read C# `<ProjectReference>`s, and the sidecar it
/// reads them with. The sidecar is reachable only through
/// [`Self::Enabled`], so a build handed [`Self::Disabled`] cannot spawn it.
enum CsharpReferences<'a> {
    /// The host has not opted in
    /// ([`SemanticState::set_csharp_project_references_enabled`]): every C#
    /// edge is unread with [`CSHARP_REFERENCES_DISABLED`].
    Disabled,
    /// The host opted in: C# edges are read through this sidecar.
    Enabled(&'a mut SidecarManager),
}

impl<'a> CsharpReferences<'a> {
    /// The sidecar when `enabled`, otherwise [`Self::Disabled`].
    fn gated(enabled: bool, sidecar: &'a mut SidecarManager) -> Self {
        if enabled {
            CsharpReferences::Enabled(sidecar)
        } else {
            CsharpReferences::Disabled
        }
    }
}

/// Why a C# `<ProjectReference>` is unread when the host has not opted into
/// them: reading one runs its MSBuild targets, analyzers and generators.
const CSHARP_REFERENCES_DISABLED: &str = "C# project references are off; reading one executes \
     its MSBuild targets, analyzers and source generators, so a host that trusts the workspace \
     enables them with the `enableCSharpProjectReferences` initialization option";

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
/// One [`ReferenceEdge`] per `csproj_refs` entry, in input order, plus the
/// *retryable* flag. A C# reference is built on demand by the sidecar, so
/// [`ReferenceOutcome::NotBuilt`] never arises here — every failure is a
/// failure to *read*, and grades as [`ReferenceOutcome::Unread`].
///
/// With [`CsharpReferences::Disabled`] there is no sidecar to drive, so
/// nothing is spawned and nothing is written: every edge is unread with
/// [`CSHARP_REFERENCES_DISABLED`].
fn csharp_project_ref_edges(
    sidecar: CsharpReferences<'_>,
    project: &Path,
    dotnet_root: &Path,
    recovered: &BTreeMap<PathBuf, String>,
    csproj_refs: &[PathBuf],
    base_tfms: &BTreeMap<PathBuf, String>,
    observer: &Observer,
) -> (Vec<ReferenceEdge>, bool) {
    if csproj_refs.is_empty() {
        return (Vec::new(), false);
    }
    let unread = |why| {
        (
            csproj_refs
                .iter()
                .map(|csproj| ReferenceEdge {
                    project: csproj.clone(),
                    outcome: ReferenceOutcome::Unread {
                        why,
                        transient: false,
                    },
                })
                .collect(),
            false,
        )
    };
    let sidecar = match sidecar {
        CsharpReferences::Enabled(sidecar) => sidecar,
        CsharpReferences::Disabled => return unread(CSHARP_REFERENCES_DISABLED),
    };
    let Some(workspace_root) = project.parent() else {
        // Nowhere to publish the sidecar's output, so *no* C# reference can be
        // read — every edge is unread, not silently absent.
        return unread("the entry project path has no parent directory");
    };
    let dotnet_exe = dotnet_exe_for(dotnet_root);

    let mut edges: Vec<ReferenceEdge> = Vec::with_capacity(csproj_refs.len());
    let mut retryable = false;
    for csproj in csproj_refs {
        let Some(tfm) = recovered.get(csproj).or_else(|| base_tfms.get(csproj)) else {
            tracing::warn!(
                csproj = %csproj.display(),
                "no producer TFM known for C# project reference (not yet restored?); skipping"
            );
            edges.push(ReferenceEdge {
                project: csproj.clone(),
                outcome: ReferenceOutcome::Unread {
                    why: "no producer TFM known",
                    transient: false,
                },
            });
            continue;
        };
        let tfm = tfm.as_str();
        // Closure-wide TFM map for per-node workspace construction. Best-effort:
        // a partial restore (the ref's own `project.assets.json` missing) makes
        // this error; the sidecar accepts an empty map and falls back to `tfm`.
        let project_tfms =
            resolve_transitive_project_tfms(csproj, tfm, observer).unwrap_or_default();
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
        // `CsharpMetadata::dlls` is empty on *any* failure and a successful
        // build always emits at least the entry's own metadata DLL, so an empty
        // list is the sidecar reporting that it could not deliver this
        // reference — a transport failure, a build error, or an unavailable
        // sidecar.
        edges.push(ReferenceEdge {
            project: csproj.clone(),
            outcome: if meta.dlls.is_empty() {
                ReferenceOutcome::Unread {
                    why: "the sidecar produced no metadata assembly",
                    transient: false,
                }
            } else {
                ReferenceOutcome::Resolved(meta.dlls)
            },
        });
    }
    (edges, retryable)
}

/// Phase 2b rooted at the **entry** project (fsproj 3.3c, plan E3): the
/// entry's own `project.assets.json` records every closure node (NuGet
/// flattens `<ProjectReference>` transitively) with its base `framework`
/// field, and `pick_producer_tfm` recovers each producer's platform-qualified
/// TFM from the producer's declared list. Keys are canonicalised to match
/// [`ResolvedAssemblies::project_ref_tfms`](crate::project_assets::ResolvedAssemblies::project_ref_tfms)
/// / `project_ref_assembly_names`.
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
fn env_ref_tfms(
    entry_project: &Path,
    entry_tfm: Option<&str>,
    observer: &Observer,
) -> BTreeMap<PathBuf, String> {
    let mut map = entry_ref_tfms(entry_project, entry_tfm, observer);
    if let Some(tfm) = entry_tfm {
        map.insert(canonicalise(entry_project, observer), tfm.to_string());
    }
    map
}

fn entry_ref_tfms(
    entry_project: &Path,
    entry_tfm: Option<&str>,
    observer: &Observer,
) -> BTreeMap<PathBuf, String> {
    let Some(tfm) = entry_tfm else {
        return BTreeMap::new();
    };
    match resolve_transitive_project_tfms(entry_project, tfm, observer) {
        Ok(map) => map
            .into_iter()
            .map(|(path, tfm)| (canonicalise(&path, observer), tfm))
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
    if Unobserved::CSharpSidecar.is_file(&candidate) {
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
/// ([`ResolvedAssemblies::project_ref_assembly_names`](crate::project_assets::ResolvedAssemblies::project_ref_assembly_names)). The assets name
/// is only a fallback because it records the **AssemblyName**, not the
/// file name: probed (dotnet 10.0.301, 2026-07-10), a `TargetName`-renamed
/// producer's assets say `bin/placeholder/<AssemblyName>.dll` while the
/// file on disk is `<TargetName>.dll` — the two coincide except under an
/// explicit `TargetName` override, exactly the case the graph name gets
/// right. A ref with neither is skipped outright rather than guessed by
/// project-file stem: a renamed producer may leave a stale stem-named DLL
/// on disk, and folding it would fabricate (D5: under-resolve, never wrong).
/// One [`ReferenceEdge`] per input target, in input order — see
/// [`ReferenceOutcome`] for why an edge that contributes nothing still gets an
/// entry, and which grade each refusal earns.
fn fsharp_project_ref_edges(
    project_refs: &[FsharpRefTarget],
    assembly_names: &BTreeMap<PathBuf, String>,
    observer: &Observer,
) -> Vec<ReferenceEdge> {
    project_refs
        .iter()
        .map(|t| ReferenceEdge {
            project: t.path.clone(),
            outcome: fsharp_project_ref_outcome(t, assembly_names, observer),
        })
        .collect()
}

fn fsharp_project_ref_outcome(
    t: &FsharpRefTarget,
    assembly_names: &BTreeMap<PathBuf, String>,
    observer: &Observer,
) -> ReferenceOutcome {
    // `graph_ref_targets` only files `ProjectKind::FSharp` nodes here, so this
    // is belt-and-braces — but a builder change that broke it must not silently
    // drop an edge, which is exactly what conservation is for.
    if !is_fsharp_project(&t.path) {
        return ReferenceOutcome::Unread {
            why: "not an F# project",
            transient: false,
        };
    }
    let Some(output_name) = t
        .output_name
        .as_deref()
        .or_else(|| assembly_names.get(t.path.as_path()).map(String::as_str))
    else {
        tracing::info!(
            fsproj = %t.path.display(),
            "project reference without a trustworthy output-assembly name; skipping its output"
        );
        return ReferenceOutcome::Unread {
            why: "no trustworthy output-assembly name",
            transient: false,
        };
    };
    let located = match &t.tfm {
        NodeTfm::Known(tfm) => locate_fsharp_output_dll(&t.path, Some(tfm), output_name, observer),
        NodeTfm::NoneDeclared => locate_fsharp_output_dll(&t.path, None, output_name, observer),
        NodeTfm::Unresolved | NodeTfm::NotEvaluated => {
            tracing::info!(
                fsproj = %t.path.display(),
                verdict = ?t.tfm,
                "project reference without a trustworthy TFM verdict; skipping its output"
            );
            return ReferenceOutcome::Unread {
                why: "no trustworthy TFM verdict",
                transient: false,
            };
        }
    };
    // A trustworthy name and TFM, and a `bin` tree we read and found nothing
    // in: the project simply has not been built. That is the one grade which
    // does *not* mark the env incomplete, so it must be a proven absence —
    // anything the locator could not decide grades as unread instead.
    match located {
        OutputLocation::Found(dll) => ReferenceOutcome::Resolved(vec![dll]),
        OutputLocation::Absent => ReferenceOutcome::NotBuilt,
        OutputLocation::Undecidable { why, transient } => {
            ReferenceOutcome::Unread { why, transient }
        }
    }
}

/// Given a referenced F# project file, find its built output DLL under
/// `<project_dir>/bin/<config>/<tfm>/<output_name>.dll`. Returns `None`
/// when the project hasn't been built (no matching DLL on disk).
///
/// `output_name` is the producer's resolved output name — the caller
/// recovers it from the entry's assets file or the graph node's own
/// evaluation ([`fsharp_project_ref_outcome`] documents the precedence) and
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
    observer: &Observer,
) -> OutputLocation {
    let mut dll_name = std::ffi::OsString::from(output_name);
    dll_name.push(".dll");
    let Some(project_dir) = fsproj.parent() else {
        return OutputLocation::Undecidable {
            why: "the project path has no parent directory",
            transient: false,
        };
    };
    let bin = project_dir.join("bin");

    // `bin/<config>/<tfm>/<stem>.dll` — collect every built variant on disk.
    // TFM directory names are compared case-insensitively, matching MSBuild's
    // case-insensitive property comparison on the path segment it writes.
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut tfm_dirs: Vec<String> = Vec::new();
    let Some(configs) = child_dirs(&bin, observer) else {
        return OutputLocation::Undecidable {
            why: "the bin directory could not be read",
            transient: true,
        };
    };
    for config in configs {
        let Some(tfm_dirs_on_disk) = child_dirs(&config, observer) else {
            return OutputLocation::Undecidable {
                why: "a bin subdirectory could not be read",
                transient: true,
            };
        };
        for tfm_dir in tfm_dirs_on_disk {
            let Some(dir_name) = tfm_dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some(tfm) = producer_tfm
                && !dir_name.eq_ignore_ascii_case(tfm)
            {
                continue;
            }
            let dll = tfm_dir.join(&dll_name);
            // `Path::is_file` folds a failed `stat` into `false`, which would
            // put an output we could not examine into the proven-absence
            // bucket. Ask for the metadata so a non-`NotFound` error is a
            // refusal to decide instead.
            match msbuild_observed::kind(observer, &dll) {
                Ok(EntryKind::File) => {
                    candidates.push(dll);
                    tfm_dirs.push(dir_name.to_ascii_lowercase());
                }
                Ok(_) => {}
                Err(err) if err.is_absent() => {}
                Err(err) => {
                    tracing::info!(dll = %dll.display(), error = %err, "cannot stat a candidate output");
                    return OutputLocation::Undecidable {
                        why: "a candidate output could not be examined",
                        transient: true,
                    };
                }
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
        return OutputLocation::Undecidable {
            why: "several TFM variants built and none selected",
            transient: false,
        };
    }
    candidates.sort();
    match candidates
        .iter()
        .find(|p| path_has_debug_config(p))
        .cloned()
        .or_else(|| candidates.into_iter().next())
    {
        Some(dll) => OutputLocation::Found(dll),
        None => OutputLocation::Absent,
    }
}

/// Where a referenced F# project's built output is, or why we cannot say.
///
/// [`Self::Absent`] is the load-bearing one: it is the only outcome that grades
/// as [`ReferenceOutcome::NotBuilt`], which leaves the env *complete*. It means
/// **the standard layout held no matching output** — not that no output exists.
/// A project whose `OutDir` / `OutputPath` / `AppendTargetFrameworkToOutputPath`
/// puts its assembly elsewhere is built and still lands here, because deciding
/// otherwise needs that project's evaluated output path, which the graph node
/// does not carry.
///
/// That is deliberately inside the trade `NotBuilt` already makes: the grade is
/// "we did not fold this reference and are choosing to carry on", never "we
/// proved there is nothing to fold". A custom layout is one more way into that
/// same accepted bucket, not a new kind of hole — the shadow it risks is the
/// one the grade was created to accept.
///
/// What must *not* land in it is a reference we hit an **error** looking for.
/// That is a different claim — we could not look, rather than looked and found
/// nothing — and it is the collapse the reference-edge conservation exists to
/// prevent, one layer up. Hence [`Self::Undecidable`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum OutputLocation {
    /// The DLL to fold.
    Found(PathBuf),
    /// Searched the standard layout without error and found no matching output.
    Absent,
    /// Could not decide. `transient` separates an I/O error while enumerating
    /// the tree — which may clear on its own, so the env must not be cached —
    /// from a stable undecidability like several TFM variants built with
    /// nothing to select between them. Grades as
    /// [`ReferenceOutcome::Unread`].
    Undecidable { why: &'static str, transient: bool },
}

/// The immediate subdirectories of `dir`, sorted for determinism.
///
/// `Some(vec![])` when `dir` simply does not exist — the ordinary shape of an
/// unbuilt project. `None` when *anything* went wrong while enumerating it: the
/// caller must not read that as "nothing is there", because it is the opposite —
/// something may be and we could not see it.
///
/// Every step can fail, not just the open: a `ReadDir` item is itself a
/// `Result` (a directory mutated mid-walk, an unreadable entry), and
/// `Path::is_dir` folds a failed `stat` into `false`. Discarding either would
/// hide the matching directory and make an unread tree look empty, so both
/// propagate.
fn child_dirs(dir: &Path, observer: &Observer) -> Option<Vec<PathBuf>> {
    let entries = match msbuild_observed::read_dir(observer, dir) {
        Ok(entries) => entries,
        Err(err) if err.is_absent() => return Some(Vec::new()),
        Err(err) => {
            tracing::info!(dir = %dir.display(), error = %err, "cannot read a bin subdirectory");
            return None;
        }
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::info!(dir = %dir.display(), error = %err, "cannot read a directory entry");
                return None;
            }
        };
        let path = entry.path;
        match msbuild_observed::kind(observer, &path) {
            Ok(EntryKind::Dir) => dirs.push(path),
            Ok(_) => {}
            // A dangling symlink, or an entry unlinked between the `read_dir`
            // and the `stat`. Nothing is there to have been missed, so skip it
            // — aborting would turn one stale link into a whole reference we
            // claim not to have read, and from there into a globally
            // incomplete env.
            Err(err) if err.is_absent() => {}
            Err(err) => {
                tracing::info!(path = %path.display(), error = %err, "cannot stat a directory entry");
                return None;
            }
        }
    }
    dirs.sort();
    Some(dirs)
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

/// The compiler's *unified* reference set among same-named candidate assemblies.
///
/// The CLR binder and `fsc`'s metadata importer unify assemblies that share an
/// identity's `(name, public-key token)` to the **highest version**. A
/// lower-versioned duplicate still lands on the compiler's reference path (RAR
/// leaves both there, emitting only the MSB3243 "choosing … arbitrarily"
/// warning), but the importer resolves every type through the highest version,
/// so a type unique to the lower one is *unresolvable* (`fsc` reports FS0039) —
/// verified with a same-name/different-version probe. Dropping the lower
/// duplicate here models exactly that: it can only make unreachable what `fsc`
/// already treats as unreachable, never hide a type `fsc` would resolve.
///
/// The motivating case: a project that transitively references the legacy
/// `System.Runtime` 4.x package restores its `ref/…/System.Runtime.dll` contract
/// assembly into the compile closure *beside* the framework pack's
/// `System.Runtime` 6.0.0.0 — same name, same PKT, lower version. Both *define*
/// `System.String` (the 4.x one is a reference/contract assembly, not a pure
/// forwarder), so the abbreviation-target chase for a bare `string` cannot pin a
/// unique `System.Runtime` and declines; unifying to the framework's 6.0.0.0
/// removes the ambiguity, matching what `fsc` imports.
///
/// borzoi assembles its closure straight from restore output (`package_dlls`) ∪
/// the framework pack ∪ project-reference outputs, without MSBuild's conflict
/// pass, so it must apply the unification here. Otherwise it hands
/// [`AssemblyEnv`] two DLLs of one simple name, which the referenced-CCU
/// uniqueness rule ([`AssemblyEnv::resolve_abbreviation_target`]) can only
/// *decline*: every abbreviation-target chase that hops through the shared name
/// dies — a bare `string` never reaches `System.String`, so it surfaces as "No
/// definition available".
///
/// Returns a keep-mask aligned to `identities`. An entry is dropped only when a
/// **strictly higher** version shares its `(name, PKT)` key. Deliberately
/// conservative at the edges:
///
/// - **equal versions are all kept** — a byte-identical duplicate-reference pair
///   stays two distinct DLLs, so the provenance handling of issue #150 (which
///   tells such siblings apart by load position, not identity) is untouched;
/// - **an unreadable identity (`None`) is always kept** — with no key it cannot
///   be unified against anything, and dropping it would be guessing;
/// - **public-key token is part of the key** — two assemblies sharing a simple
///   name but signed differently are genuinely distinct (a third-party assembly
///   coincidentally named like a framework one), so neither supersedes the
///   other.
fn unified_reference_keep_mask(identities: &[Option<AssemblyIdentity>]) -> Vec<bool> {
    let mut max_version: HashMap<(&str, Option<[u8; 8]>), Version> = HashMap::new();
    for id in identities.iter().flatten() {
        let key = (id.name.as_str(), id.public_key_token);
        max_version
            .entry(key)
            .and_modify(|v| *v = (*v).max(id.version))
            .or_insert(id.version);
    }
    identities
        .iter()
        .map(|id| match id {
            None => true,
            Some(id) => {
                let key = (id.name.as_str(), id.public_key_token);
                // The key was just inserted from this very entry, so the lookup
                // always hits; `>=` keeps every entry tied at the group max.
                id.version >= max_version[&key]
            }
        })
        .collect()
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
///
/// Public so the `resolve_real_project_diff` integration test builds its
/// `AssemblyEnv` through the *exact* runtime path — forwarders, manifest
/// identities and the version-unification drop included — rather than a
/// hand-rolled copy that silently drifts (the drift that hid every BCL
/// abbreviation-target chase from that gate until this was shared).
pub fn build_env_from_dll_paths<'a>(
    dlls: impl Iterator<Item = &'a Path>,
    cache: &AssemblyCache,
    observer: &Observer,
) -> (AssemblyEnv, Vec<PathBuf>) {
    let paths: Vec<&Path> = dlls.collect();
    let _span = tracing::info_span!("build_env_from_dll_paths", count = paths.len()).entered();
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(paths.len().max(1));

    // Single DLL (or a platform reporting one core): no threading overhead.
    let probed: Vec<(usize, Result<ReferencedAssemblyProjection, SkipCause>)> = if workers <= 1 {
        paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let _span =
                    tracing::info_span!("enumerate_dll_type_defs", dll = %p.display()).entered();
                (i, enumerate_dll_type_defs_cached(cache, p, observer))
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
                        // An observer per worker: one is not shared across
                        // threads, so each joins its reads to the caller's.
                        let local_observer = Observer::new();
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
                            local.push((
                                i,
                                enumerate_dll_type_defs_cached(cache, path, &local_observer),
                            ));
                        }
                        (local, local_observer.finish())
                    })
                })
                .collect();
            // Each per-DLL call is already panic-safe (`catch_reader_panic`), so a
            // worker cannot unwind; `.join().ok()` is belt-and-braces — a worker
            // panic drops only its results, never crashes the request loop (D5).
            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .flat_map(|(local, reads)| {
                    observer.absorb(reads);
                    local
                })
                .collect()
        })
    };

    // One outcome per input path, in input order. A path missing from `probed`
    // means its worker's results were dropped (only reachable if a worker
    // unwound past `catch_reader_panic`, i.e. never in practice) — we learned
    // nothing about that DLL, which is exactly [`SkipCause::UnknownIdentity`].
    // Reading the outcome per *path* rather than inferring it from a length
    // difference is what lets the two skip grades below be told apart at all.
    let mut outcomes: Vec<Option<Result<ReferencedAssemblyProjection, SkipCause>>> =
        (0..paths.len()).map(|_| None).collect();
    for (i, outcome) in probed {
        outcomes[i] = Some(outcome);
    }

    // A skip whose cause leaves the DLL's identity and types **unknown** poisons
    // every later reading: its manifest name is missing from the registry, so
    // referenced-CCU uniqueness cannot be decided (a same-named sibling could be
    // the intended CCU — issue #150 / codex P2), and it could declare a type or
    // an assembly-level `[<AutoOpen>]` that shadows one we did read. A file that
    // is provably *not a managed assembly* costs none of that
    // ([`SkipCause::NotAManagedAssembly`]) and so does not count here.
    //
    // Computed *before* the version-unification drop below: that drop is a
    // deliberate exclusion of a DLL whose identity we *do* know, not a projection
    // failure, so it must not read as an incomplete registry (which would make
    // referenced-CCU uniqueness decline wholesale and defeat the very chase the
    // drop enables).
    let identity_unknown: Vec<&Path> = outcomes
        .iter()
        .zip(&paths)
        .filter(|(o, _)| matches!(o, None | Some(Err(SkipCause::UnknownIdentity))))
        .map(|(_, p)| *p)
        .collect();
    // One project-level line naming the whole set: each DLL's own cause is
    // already logged where it was decided, and what matters here is the fact
    // about the *env* those causes add up to.
    if !identity_unknown.is_empty() {
        tracing::warn!(
            dlls = ?identity_unknown,
            "referenced assemblies whose identity and types are unknown; \
             the env's identity set is incomplete"
        );
    }
    let identities_incomplete = !identity_unknown.is_empty();
    let mut indexed: Vec<(usize, PathBuf, ReferencedAssemblyProjection)> = outcomes
        .into_iter()
        .zip(&paths)
        .enumerate()
        .filter_map(|(i, (outcome, path))| Some((i, path.to_path_buf(), outcome?.ok()?)))
        .collect();
    // MSBuild's conflict resolution / the CLR binder unify same-named assemblies
    // to the highest version before the compiler sees them; borzoi builds its
    // closure without that pass, so apply it now (see
    // [`unified_reference_keep_mask`]).
    {
        let identities: Vec<Option<AssemblyIdentity>> = indexed
            .iter()
            .map(|(_, _, p)| p.manifest_identity.clone())
            .collect();
        let keep = unified_reference_keep_mask(&identities);
        let mut kept = keep.iter();
        indexed.retain(|(_, path, projection)| {
            let keep = *kept.next().expect("mask aligned to indexed");
            if !keep {
                tracing::info!(
                    dll = %path.display(),
                    superseded = ?projection.manifest_identity,
                    "dropping a lower-version duplicate assembly (unified to the highest version)"
                );
            }
            keep
        });
    }
    // The OV-6 gate's extension surface is **globally** unknowable when a DLL's
    // AutoOpen list could not be read (an unknown auto-open could bring an
    // extension into any namespace) OR a DLL's contents are unknown entirely:
    // either forces the gate to defer wholesale. A **dropped type**, by
    // contrast, is namespace-scoped uncertainty (below).
    let extension_surface_unknowable =
        identities_incomplete || indexed.iter().any(|(_, _, p)| p.auto_opens_unreadable);
    // Collect the namespaces of every dropped type across all DLLs.
    let dropped_type_namespaces: Vec<Vec<String>> = indexed
        .iter()
        .flat_map(|(_, _, p)| p.dropped_type_namespaces.iter().cloned())
        .collect();
    // The set the env is actually built from — the caller's list minus the
    // unified-away duplicates. A caller that hands these references on (the
    // differential harnesses hand them to FCS) must pass *this* list: a
    // reference we dropped is one the compiler's importer never resolves
    // through, so passing the raw list makes the other side read an assembly we
    // do not have. The measured shape: a legacy `system.runtime/4.3.1` contract
    // assembly beside the framework pack's 8.0 — with both in FCS's `-r:` set,
    // type-checking `WoofWare.PawPrint`'s main library never terminates
    // (100%+ CPU, RSS past 21 GB in five minutes).
    let kept: Vec<PathBuf> = indexed.iter().map(|(_, path, _)| path.clone()).collect();
    let assemblies = indexed
        .into_iter()
        .map(|(_, path, projection)| {
            let visibility = projection.abbreviation_visibility();
            borzoi_sema::AssemblyProjectionInput {
                path,
                roots: projection.entities,
                abbreviation_visibility: visibility,
                extension_index_unknowable: projection.fsharp_extension_index_unknowable,
                signature_non_authoritative: projection.fsharp_signature_non_authoritative,
                auto_opens: projection.assembly_auto_opens,
                manifest_identity: projection.manifest_identity,
                type_forwarders: projection.type_forwarders,
                doc_id_dialect: projection.doc_id_dialect,
                has_assembly_references: projection.has_assembly_references,
                qualified_names: projection.qualified_names,
            }
        })
        .collect();
    let mut env = AssemblyEnv::from_assemblies_with_projection_knowability(assemblies);
    if extension_surface_unknowable {
        env.mark_extension_surface_unknowable();
    }
    if identities_incomplete {
        env.mark_referenced_assemblies_incomplete();
    }
    for namespace in dropped_type_namespaces {
        env.mark_namespace_dropped_type(namespace);
    }
    (env, kept)
}

/// [`enumerate_dll_type_defs`] through the on-disk cache: a hit returns the
/// stored projection (skipping the read+parse+project); a miss computes it and
/// (best-effort) stores the result for the next warm start. A disabled cache
/// (the default) always misses and never writes, so this is exactly
/// [`enumerate_dll_type_defs`] plus an off-by-default fast path. The cached value
/// is identical to a fresh computation, so the env is byte-identical either way.
/// [`AssemblyCache::get_or_populate`] brackets the read+parse with a `stat` so a
/// DLL overwritten mid-compute is never persisted with mismatched metadata.
///
/// Only a *projection* is cached, so a cache hit is always a success and the
/// [`SkipCause`] is only ever produced by a live computation.
fn enumerate_dll_type_defs_cached(
    cache: &AssemblyCache,
    path: &Path,
    observer: &Observer,
) -> Result<ReferencedAssemblyProjection, SkipCause> {
    // A cache hit depends on the DLL as of this stamp, taken before the cache
    // is asked: the hit is the projection of the DLL at the stamp the cache
    // checks, and a write after this one moves the stamp recorded here. A miss
    // reads the DLL, and depends on the bytes that read saw, which it records
    // with its own stamp (and, for a racy one, their digest); the stamp taken
    // here would only add a racy stamp no digest confirms.
    let before = Observer::new();
    let stamp = msbuild_observed::stamp(&before, path);
    // A racy stamp speaks for the DLL only when it was taken, so a cache
    // answer resting on it could never be confirmed again: read the DLL,
    // whose racy stamp carries a digest of what was read.
    if stamp.is_ok_and(|stamp| stamp.racy) {
        observer.absorb((std::iter::empty().collect(), before.finish().1));
        return enumerate_dll_type_defs(path, observer);
    }
    let mut cause = None;
    let mut read = false;
    let hit = cache.get_or_populate(path, || {
        read = true;
        match enumerate_dll_type_defs(path, observer) {
            Ok(projection) => Some(projection),
            Err(why) => {
                cause = Some(why);
                None
            }
        }
    });
    let (stamped, counts) = before.finish();
    if read {
        // The `stat` was made all the same.
        observer.absorb((std::iter::empty().collect(), counts));
    } else {
        observer.absorb((stamped, counts));
    }
    match hit {
        Some(projection) => Ok(projection),
        // A miss runs the closure, which sets `cause` on every failing path; a
        // hit returns `Some`. So `None` here implies the closure ran and failed.
        None => Err(cause.expect("a cache miss runs the computation")),
    }
}

/// Read, parse, and enumerate the type definitions of one referenced DLL.
/// Errs — with a logged warning — on *any* failure (unreadable file,
/// unparseable PE, an `enumerate_type_defs` error, or a reader panic at either
/// stage) so the caller skips just this DLL and keeps the others. Never
/// propagates an error or a panic into the env build.
///
/// The [`SkipCause`] separates the one refusal that is a fact about the *input*
/// — the file is not a managed assembly — from every refusal that is a fact
/// about *this reader*. Only the latter leaves anything unknown.
fn enumerate_dll_type_defs(
    path: &Path,
    observer: &Observer,
) -> Result<ReferencedAssemblyProjection, SkipCause> {
    let bytes = match msbuild_observed::read_stamped(observer, path) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(dll = %path.display(), error = %err, "failed to read DLL; skipping");
            return Err(SkipCause::UnknownIdentity);
        }
    };
    // `parse` drives the owned ECMA-335 reader over arbitrary
    // external bytes; a malformed-enough DLL (corrupt package cache, native
    // image, truncated download) can make it panic on an out-of-range table
    // index rather than return `Err`, so it runs panic-safely — the same
    // policy the CST parser path uses (see [`crate::cst_panic_safe`]).
    let parsed = catch_reader_panic(path, "parse", || Ecma335Assembly::parse(&bytes))
        .ok_or(SkipCause::UnknownIdentity)?;
    match parsed {
        Ok(view) => enumerate_view_catching(path, &view).ok_or(SkipCause::UnknownIdentity),
        // Not an assembly at all: logged at `debug`, not `warn`, because native
        // files ride along in NuGet packages routinely and nothing is lost.
        Err(ImportError::NotAManagedAssembly) => {
            tracing::debug!(dll = %path.display(), "not a managed assembly; skipping");
            Err(SkipCause::NotAManagedAssembly)
        }
        Err(err) => {
            tracing::warn!(dll = %path.display(), error = %err, "failed to parse DLL; skipping");
            Err(SkipCause::UnknownIdentity)
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
            // A forwarder read failing degrades to "no forwarders" — that only
            // costs abbreviation-chase declines, never a wrong target.
            let type_forwarders = catch_reader_panic(path, "type_forwarders", || {
                view.type_forwarders()
            })
            .and_then(|r| match r {
                Ok(forwarders) => Some(forwarders),
                Err(err) => {
                    tracing::warn!(
                        dll = %path.display(),
                        error = %err,
                        "failed to read type forwarders; treating the assembly as forwarding nothing"
                    );
                    None
                }
            })
            .unwrap_or_default();
            // Unreadable degrades to unknown, which declines member doc keys:
            // a guessed dialect could attach another member's documentation.
            let doc_id_dialect = catch_reader_panic(path, "doc_id_dialect", || {
                view.doc_id_dialect()
            })
            .and_then(|r| match r {
                Ok(dialect) => Some(dialect),
                Err(err) => {
                    tracing::warn!(
                        dll = %path.display(),
                        error = %err,
                        "failed to read the doc-ID dialect; declining member documentation keys"
                    );
                    None
                }
            });
            let has_assembly_references =
                catch_reader_panic(path, "assembly_refs", || !view.assembly_refs().is_empty());
            let qualified_names =
                catch_reader_panic(path, "qualified_names", || view.qualified_names()).flatten();
            Some(ReferencedAssemblyProjection {
                entities: types,
                fsharp_abbreviations_unknowable: skipped.fsharp_abbreviations_unknowable,
                assembly_auto_opens,
                auto_opens_unreadable,
                dropped_type_namespaces,
                fsharp_extension_index_unknowable: skipped.fsharp_extension_index_unknowable,
                fsharp_signature_non_authoritative: skipped.fsharp_signature_non_authoritative,
                // Captured even when `types` is empty (rootless), so the env can
                // still count this DLL's name for referenced-CCU uniqueness.
                manifest_identity: Some(view.identity().clone()),
                type_forwarders,
                doc_id_dialect,
                has_assembly_references,
                qualified_names,
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

/// A file's identity *as a file*: its real path, or the literal path when
/// `canonicalize` fails (e.g. the file doesn't exist on disk yet).
///
/// For joining paths from two sources that spell one file differently — the
/// graph walk's lexical spellings against the paths NuGet recorded in an assets
/// file. Never for identifying a *project*: two spellings of one `.fsproj` can
/// be two different MSBuild projects (see [`ProjectId`]).
fn canonicalise(path: &Path, observer: &Observer) -> PathBuf {
    msbuild_observed::canonicalize(observer, path).unwrap_or_else(|_| path.to_path_buf())
}

/// A project, identified by the spelling it is evaluated under: the caller's
/// path, made absolute and lexically normalised, with no symlink resolved.
///
/// MSBuild evaluates a project as it is spelt, not as the file it names.
/// `$(MSBuildProjectDirectory)` is the spelling's directory, every relative
/// `<Compile>` and `<ProjectReference>` is joined to it, and the search for
/// `Directory.Build.props`, `Directory.Packages.props` and the like walks *its*
/// ancestors. So a directory reached through a symlink has different ancestors
/// from its real path, and a project file that is itself a symlink compiles the
/// sources beside the link, not beside its target: two spellings of one file
/// can be two projects, and `dotnet` treats them so. Identifying a project by
/// its real path merges them, and whichever spelling is evaluated first is then
/// served for both.
///
/// Every per-project cache is keyed by this, and the project is evaluated by
/// [`Self::as_path`], so a key always names exactly the evaluation it holds.
/// Lexical normalisation loses nothing, since MSBuild normalises a project's
/// path the same way (`..` is textual, as in [`lexically_normalize`]).
///
/// Equality folds case where the platform does
/// ([`crate::paths::path_dedup_key`]), matching
/// [`crate::paths::paths_equal`], which the rest of the LSP uses for the same
/// question. Two spellings differing only in case search the same directories,
/// and a restore keeps a project's references under either (probed on macOS).
#[derive(Debug, Clone)]
pub struct ProjectId {
    path: PathBuf,
    identity: String,
}

impl PartialEq for ProjectId {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

impl Eq for ProjectId {}

impl std::hash::Hash for ProjectId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.identity.hash(state);
    }
}

impl ProjectId {
    pub fn new(path: &Path) -> Self {
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let path = lexically_normalize(&absolute);
        ProjectId {
            identity: crate::paths::path_dedup_key(&path),
            path,
        }
    }

    /// The platform-folded identity, as text.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// The spelling to evaluate, restore and report this project by.
    pub fn as_path(&self) -> &Path {
        &self.path
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path.display())
    }
}

/// True for an F# signature file (`.fsi`). The extension match is
/// case-insensitive so a `.FSI` on a case-insensitive filesystem still counts.
fn is_signature_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("fsi"))
}

#[cfg(test)]
mod tests {
    use crate::observed::host_fs::HostPath as _;
    use std::collections::HashSet;

    use proptest::prelude::*;

    /// A spelling of `/proj/<segments>`: each segment optionally preceded by a
    /// redundant `.` and a cancelling `noise/..` pair, and optionally
    /// upper-cased.
    fn spelling(segments: &[(String, bool, bool)]) -> PathBuf {
        let mut path = PathBuf::from("/proj");
        for (segment, noisy, upper) in segments {
            if *noisy {
                path.push(".");
                path.push("noise");
                path.push("..");
            }
            path.push(if *upper {
                segment.to_ascii_uppercase()
            } else {
                segment.clone()
            });
        }
        path
    }

    fn segments() -> impl Strategy<Value = Vec<(String, bool, bool)>> {
        prop::collection::vec(("[a-c]{1,2}", any::<bool>(), any::<bool>()), 1..4)
    }

    proptest! {
        /// Two spellings are one project exactly when the rest of the LSP
        /// treats them as one file: membership matches a buffer to a
        /// `<Compile>` item by this same relation, so a project and the
        /// sources it lists are identified alike. Exercised off disk, where no
        /// symlink can intervene.
        #[test]
        fn project_identity_is_lexical_path_equality(a in segments(), b in segments()) {
            let (a, b) = (spelling(&a), spelling(&b));
            prop_assert_eq!(
                ProjectId::new(&a) == ProjectId::new(&b),
                crate::paths::paths_equal(&lexically_normalize(&a), &lexically_normalize(&b)),
                "{:?} vs {:?}", a, b
            );
            // The identity is what a map keys on, so it decides the same way.
            prop_assert_eq!(
                ProjectId::new(&a) == ProjectId::new(&b),
                ProjectId::new(&a).identity() == ProjectId::new(&b).identity()
            );
        }
    }

    /// A project reached through a symlinked directory is not the project at
    /// its real path: MSBuild searches different ancestors for each.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_spelling_is_its_own_project() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("App.fsproj"), "<Project />").unwrap();
        std::os::unix::fs::symlink(&real, tmp.path().join("link")).unwrap();

        let through_link = ProjectId::new(&tmp.path().join("link").join("App.fsproj"));
        assert_ne!(through_link, ProjectId::new(&real.join("App.fsproj")));
        assert_eq!(
            through_link.as_path(),
            tmp.path().join("link").join("App.fsproj"),
            "evaluated as spelt"
        );
    }

    use super::*;
    use crate::uri::UriExt as _;
    use borzoi_assembly::{
        Access, AssemblyIdentity, AssemblyProjectionSkips, EntityKind, FSharpResource,
        FsharpOverlayKind, ImportError, SkippedFsharpOverlay, UnionCases, Version,
    };
    use borzoi_sema::test_support::names::{rekeyed, rekeyed_inference};
    use lsp_types::Uri;
    use std::fs;
    use tempfile::TempDir;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn ident_vp(name: &str, major: u16, pkt: Option<[u8; 8]>) -> AssemblyIdentity {
        AssemblyIdentity {
            name: name.to_string(),
            version: Version {
                major,
                minor: 0,
                build: 0,
                revision: 0,
            },
            public_key_token: pkt,
        }
    }

    const PKT_A: Option<[u8; 8]> = Some([176, 63, 95, 127, 17, 213, 10, 58]);
    const PKT_B: Option<[u8; 8]> = Some([204, 123, 19, 255, 205, 45, 221, 81]);

    /// The set [`build_env_from_dll_paths`] hands back is the one the env was
    /// built from, not the one it was given: a DLL it could not project is not
    /// in it.
    ///
    /// That distinction is the whole point of returning it. A caller that
    /// passes the references on — the differentials hand them to FCS — must not
    /// pass a reference we dropped, or the other side reads an assembly we do
    /// not have. With a superseded duplicate corelib in FCS's `-r:` set,
    /// type-checking `WoofWare.PawPrint`'s main library never terminated.
    #[test]
    fn the_returned_reference_set_is_the_one_the_env_was_built_from() {
        let tmp = tempfile::tempdir().unwrap();
        let unreadable = tmp.path().join("unreadable.dll");
        std::fs::write(&unreadable, b"MZ but nothing else").unwrap();
        let native = tmp.path().join("native.dll");
        std::fs::write(&native, native_pe_image()).unwrap();

        let (_, kept) = build_env_from_dll_paths(
            [unreadable.as_path(), native.as_path()].into_iter(),
            &AssemblyCache::disabled(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert!(
            kept.is_empty(),
            "neither DLL projects, so neither is in the set the env was built from: {kept:?}"
        );
    }

    #[test]
    fn unified_reference_mask_drops_lower_version_of_same_name_and_pkt() {
        // The motivating case: `System.Runtime` 4.1.0.0 (a package facade) beside
        // `System.Runtime` 6.0.0.0 (the framework pack), same PKT. The lower one
        // is superseded — without this drop the referenced-CCU chase declines.
        let ids = [
            Some(ident_vp("System.Runtime", 4, PKT_A)),
            Some(ident_vp("FSharp.Core", 6, PKT_A)),
            Some(ident_vp("System.Runtime", 6, PKT_A)),
        ];
        assert_eq!(
            unified_reference_keep_mask(&ids),
            vec![false, true, true],
            "the lower-version System.Runtime is dropped; the winner and unrelated DLLs stay",
        );
    }

    #[test]
    fn unified_reference_mask_keeps_equal_version_duplicates() {
        // Byte-identical duplicate references (issue #150) share name, PKT AND
        // version — no strictly-higher sibling, so both survive and sema's
        // provenance handling tells them apart as before.
        let ids = [
            Some(ident_vp("Dup", 1, PKT_A)),
            Some(ident_vp("Dup", 1, PKT_A)),
        ];
        assert_eq!(unified_reference_keep_mask(&ids), vec![true, true]);
    }

    #[test]
    fn unified_reference_mask_keeps_distinct_public_key_tokens() {
        // Same simple name, different signing key: genuinely distinct assemblies
        // (e.g. a third-party DLL coincidentally named like a framework one), so
        // neither supersedes the other regardless of version.
        let ids = [
            Some(ident_vp("Shared", 1, PKT_A)),
            Some(ident_vp("Shared", 9, PKT_B)),
        ];
        assert_eq!(unified_reference_keep_mask(&ids), vec![true, true]);
    }

    #[test]
    fn unified_reference_mask_keeps_unreadable_identities() {
        // A DLL whose identity could not be read has no key to unify on; keep it
        // (dropping would be guessing) — and it never supersedes a real one.
        let ids = [None, Some(ident_vp("Lib", 1, PKT_A)), None];
        assert_eq!(unified_reference_keep_mask(&ids), vec![true, true, true]);
    }

    proptest::proptest! {
        #[test]
        fn unified_reference_mask_is_sound(
            // Small alphabets so collisions are frequent.
            entries in proptest::collection::vec(
                (
                    proptest::option::of(proptest::sample::select(vec!["A", "B", "C"])),
                    0u16..4,
                    proptest::sample::select(vec![PKT_A, PKT_B, None]),
                ),
                0..12,
            ),
        ) {
            let ids: Vec<Option<AssemblyIdentity>> = entries
                .iter()
                .map(|(name, major, pkt)| {
                    name.map(|n| ident_vp(n, *major, *pkt))
                })
                .collect();
            let mask = unified_reference_keep_mask(&ids);
            proptest::prop_assert_eq!(mask.len(), ids.len());

            // The per-key max version, computed independently of the function.
            use std::collections::HashMap;
            let mut group_max: HashMap<(&str, Option<[u8; 8]>), Version> = HashMap::new();
            for id in ids.iter().flatten() {
                let key = (id.name.as_str(), id.public_key_token);
                let e = group_max.entry(key).or_insert(id.version);
                if id.version > *e { *e = id.version; }
            }

            for (id, &kept) in ids.iter().zip(&mask) {
                match id {
                    // Property 1: an unreadable identity is always kept.
                    None => proptest::prop_assert!(kept, "None identity must be kept"),
                    Some(id) => {
                        let key = (id.name.as_str(), id.public_key_token);
                        let is_group_max = id.version == group_max[&key];
                        // Property 2: kept iff at the group max — nothing below the
                        // max survives, everything at it does.
                        proptest::prop_assert_eq!(
                            kept, is_group_max,
                            "entry kept iff its version is the group max"
                        );
                    }
                }
            }

            // Property 3: every group keeps at least one entry (never erase a name).
            for key in group_max.keys() {
                let kept_in_group = ids.iter().zip(&mask).any(|(id, &kept)| {
                    kept && id.as_ref().is_some_and(|id|
                        (id.name.as_str(), id.public_key_token) == *key)
                });
                proptest::prop_assert!(kept_in_group, "each (name, PKT) group keeps its max");
            }

            // Property 4: idempotent — re-running on the survivors keeps them all.
            let survivors: Vec<Option<AssemblyIdentity>> = ids
                .iter()
                .zip(&mask)
                .filter(|&(_, &k)| k)
                .map(|(id, _)| id.clone())
                .collect();
            proptest::prop_assert!(
                unified_reference_keep_mask(&survivors).iter().all(|&k| k),
                "the survivor set is a fixed point"
            );
        }
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
            .parses_for_project(&proj, &mut ws, &Vfs::new())
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
                .parses_for_project(&proj, &mut ws, &Vfs::new())
                .expect("project parses");
            assert!(text_contains(&parses.texts[0], "disk"));
        }

        // Now overlay a buffer: the cached parses confirm their texts when
        // read, so the next lookup rebuilds over it.
        let mut docs = Vfs::new();
        docs.insert(
            Uri::from_file_path(&file).unwrap(),
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
            sema.parses_for_project(&proj, &mut ws, &Vfs::new())
                .is_none()
        );
    }

    /// `docs/fsi-signature-restriction-plan.md`: a project listing a `.fsi`
    /// signature **folds**. The signature parses under the signature grammar
    /// and occupies its Compile slot with no exports of its own; its exposed
    /// surface rides the paired implementation's slot with signature
    /// identity (Stage 2), while the implementation's own resolutions are
    /// untouched (pinned by sema's `resolve_signatures` /
    /// `resolve_signature_exports` groups).
    #[test]
    fn project_with_fsi_signature_folds() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fsi", "A.fs", "B.fs"]));
        write(&tmp.path().join("A.fsi"), "module A\nval publicFoo : int\n");
        write(
            &tmp.path().join("A.fs"),
            "module A\nlet publicFoo = 1\nlet hiddenFoo = 2\n",
        );
        write(
            &tmp.path().join("B.fs"),
            "module B\nlet use1 = A.publicFoo\nlet use2 = A.hiddenFoo\n",
        );

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let parses = sema
            .parses_for_project(&proj, &mut ws, &Vfs::new())
            .expect("a .fsi-bearing project folds (Stage 1)")
            .clone();
        assert_eq!(parses.len(), 3, "the .fsi keeps its Compile slot");
        assert!(
            parses.files[0].file.as_sig().is_some(),
            "the .fsi parses under the signature grammar"
        );
        assert!(parses.files[1].file.as_impl().is_some());

        let (resolved, env) = sema
            .resolved_project_and_env_for(&proj, &mut ws, &Vfs::new())
            .expect("a .fsi-bearing project resolves (Stage 1)");
        assert_eq!(resolved.len(), 3);
        // The signature slot owns no exports of its own; its surface rides
        // the implementation's slot (Stage 2).
        assert!(resolved.file(0).exports().is_empty());
        // The implementation's item range: its own two items (conclusion 2 —
        // its internal resolutions are untouched) plus the signature's one
        // appended export.
        let names: Vec<_> = resolved
            .file(1)
            .exports()
            .iter()
            .map(|e| e.name(&env).to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "publicFoo".to_string(),
                "hiddenFoo".to_string(),
                "publicFoo".to_string(),
            ]
        );
    }

    /// The namespace-headed shape pairs through the filename-derived
    /// `QualifiedNameOfFile` (`A.fsi` / `A.fs` → `A`), so it folds too —
    /// probes G/G2 of the plan's sweep.
    #[test]
    fn project_with_namespace_headed_fsi_signature_folds() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fsi", "A.fs"]));
        write(
            &tmp.path().join("A.fsi"),
            "namespace N\n\nmodule A =\n    val publicFoo : int\n",
        );
        write(
            &tmp.path().join("A.fs"),
            "namespace N\n\nmodule A =\n    let publicFoo = 1\n    let hiddenFoo = 2\n",
        );

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let parses = sema
            .parses_for_project(&proj, &mut ws, &Vfs::new())
            .expect("a namespace-headed .fsi project folds (Stage 1)")
            .clone();
        assert_eq!(parses.len(), 2);
        assert!(parses.files[0].file.as_sig().is_some());
        assert!(
            sema.resolved_project_for(&proj, &mut ws, &Vfs::new())
                .is_some()
        );
    }

    /// Codex finding (Stage 7 re-review): the `.fsi` extension check is
    /// case-insensitive. On Windows/macOS a project can
    /// legitimately list `A.FSI` and the filesystem treats it as the same
    /// file — it must parse under the *signature* grammar (a case-sensitive
    /// check would route it to the impl grammar and leak hidden symbols).
    #[test]
    fn project_with_uppercase_fsi_signature_folds_as_signature() {
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
        let parses = sema
            .parses_for_project(&proj, &mut ws, &Vfs::new())
            .expect("an uppercase .FSI project folds (Stage 1)")
            .clone();
        assert!(
            parses.files[0].file.as_sig().is_some(),
            "A.FSI must parse under the signature grammar"
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
            sema.parses_for_project(&proj, &mut ws, &Vfs::new())
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
            .parses_for_project(&proj, &mut ws, &Vfs::new())
            .expect("trustworthy Compile set must still yield parses");
        assert_eq!(parses.paths.len(), 1);
    }

    /// A cache *hit* serves the recovery record beside the tree it serves.
    ///
    /// The `Parse` dies inside [`build_parses`], so a hit that returned only the
    /// tree would hand the fold [`SyntaxRecovery::Unretained`] — and nothing
    /// would go red, because `Unretained` is the safe reading: the file's
    /// annotations would simply stop producing types, on the second request and
    /// every one after it. That is the whole failure mode of caching a
    /// knowability, so it is pinned rather than argued.
    #[test]
    fn a_cache_hit_still_carries_the_parse_errors() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        let file = tmp.path().join("Lib.fs");
        write(&proj, &fsproj(&["Lib.fs"]));
        // Clean: the point is that `Reported` survives, and `Reported([])` is
        // the reading that licenses a commitment.
        write(&file, "module M\nlet v : System.String = failwith \"\"\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let cold = sema
            .parses_for_project(&proj, &mut ws, &Vfs::new())
            .expect("first parses")
            .files[0]
            .recovery
            .clone();
        // Forget the *project* memo so the rebuild runs, while leaving the
        // per-file `CachedParse` table intact and the text untouched — which is
        // what makes the rebuild take the hit branch rather than re-parse.
        sema.forget_fold(&proj);
        let hit = sema
            .parses_for_project(&proj, &mut ws, &Vfs::new())
            .expect("rebuilt parses")
            .files[0]
            .recovery
            .clone();

        assert_eq!(
            cold,
            SyntaxRecovery::Reported([].into()),
            "a clean file must prove itself clean on a cold parse"
        );
        assert_eq!(hit, cold, "the cache hit must serve the same reading");
    }

    /// A guessed language version cannot prove a clean read of a file whose
    /// recovery diagnostics are version-gated.
    ///
    /// A nullness annotation is a recovery below F# 9 (FCS parses it
    /// differently there) and silent at or above it, from a byte-identical tree
    /// of ours — so the version-boundary gate, which compares tree *shape*,
    /// never fires and the guessed parse records `Reported([])`. Read as proof,
    /// that licenses publishing a type for an annotation the real build parses
    /// otherwise. The file must read `Unretained` instead. A legality error is
    /// different: `#elif` is an error below F# 11 over the tree FCS builds at
    /// every version, so a guess on either side reads that tree, and the file
    /// keeps its reading.
    #[test]
    fn a_version_gated_recovery_is_unretained_under_a_guessed_version() {
        use borzoi_cst::language_version::LanguageVersion;
        use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};

        let parse = |source, lang| {
            parse_with_options(
                source,
                ParseOptions {
                    file_kind: FileKind::Impl,
                    symbols: &HashSet::new(),
                    lang,
                },
            )
        };
        let guess = |source| {
            SyntaxRecovery::of_guessed_version(
                &parse(source, LanguageVersion::Preview),
                source,
                &HashSet::new(),
                FileKind::Impl,
            )
        };
        // The premises, restated locally so this test fails loudly if a gate
        // ever changes class rather than passing vacuously.
        for (source, below, recovery) in [
            (
                "module M\nlet f (x : System.String | null) = 1\n",
                LanguageVersion::V8_0,
                true,
            ),
            (
                "module M\n#if FOO\nlet a = 1\n#elif BAR\nlet a = 2\n#endif\n",
                LanguageVersion::V10_0,
                false,
            ),
        ] {
            let (guessed, rejected) = (
                parse(source, LanguageVersion::Preview),
                parse(source, below),
            );
            assert!(guessed.errors.is_empty(), "clean at the guessed version");
            assert!(
                !rejected.errors.is_empty()
                    && rejected.errors.iter().all(|e| e.is_recovery() == recovery),
                "and rejected one threshold down: {:?}",
                rejected.errors
            );
            assert_eq!(
                guessed.root.structure(),
                rejected.root.structure(),
                "with the same tree, which is why the shape gate cannot see it"
            );
        }

        assert_eq!(
            guess("module M\nlet f (x : System.String | null) = 1\n"),
            SyntaxRecovery::Unretained,
            "a guessed version proves nothing about a version-gated recovery"
        );
        assert_eq!(
            guess("module M\n#if FOO\nlet a = 1\n#elif BAR\nlet a = 2\n#endif\n"),
            SyntaxRecovery::Reported([].into()),
            "a legality error leaves the reading as it is"
        );
        // The contrast: ordinary source is version-invariant, so a guess is
        // still proof and the feature stays on for it.
        let ordinary = parse_with_options(
            "module M\nlet v : System.String = failwith \"\"\n",
            ParseOptions {
                file_kind: FileKind::Impl,
                symbols: &HashSet::new(),
                lang: LanguageVersion::Preview,
            },
        );
        assert_eq!(
            SyntaxRecovery::of_guessed_version(
                &ordinary,
                "module M\nlet v : System.String = failwith \"\"\n",
                &HashSet::new(),
                FileKind::Impl,
            ),
            SyntaxRecovery::Reported([].into()),
        );
    }

    /// A cached fold is served while its Compile items' texts are unchanged,
    /// and is dropped once one moves on disk, with or without an
    /// invalidation: every read validates it against the `Vfs`.
    #[test]
    fn a_cached_fold_holds_until_its_text_moves() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        let file = tmp.path().join("Lib.fs");
        write(&proj, &fsproj(&["Lib.fs"]));
        write(&file, "let first = 1\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();

        let first = Arc::clone(
            &sema
                .parses_for_project(&proj, &mut ws, &Vfs::new())
                .expect("first parses")
                .texts[0],
        );
        let again = Arc::clone(
            &sema
                .parses_for_project(&proj, &mut ws, &Vfs::new())
                .expect("cached parses")
                .texts[0],
        );
        // Outside a generation every read allocates afresh, so the same
        // allocation can only be the cached entry's.
        assert!(
            Arc::ptr_eq(&first, &again),
            "an unchanged text is served from the cache"
        );

        // Edit the on-disk file, but don't invalidate.
        fs::write(&file, "let second = 2\n").unwrap();
        {
            let parses = sema
                .parses_for_project(&proj, &mut ws, &Vfs::new())
                .expect("revalidated parses");
            assert!(
                text_contains(&parses.texts[0], "second"),
                "expected the disk edit, got {:?}",
                parses.texts[0]
            );
        }

        // A rebuild from nothing gives the same.
        sema.forget_fold(&proj);
        let parses = sema
            .parses_for_project(&proj, &mut ws, &Vfs::new())
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
            .parses_for_project(&proj, &mut ws, &Vfs::new())
            .expect("project parses");
        let env = AssemblyEnv::default();
        let resolved = resolve_file(
            parses.files[0]
                .file
                .as_impl()
                .expect("an impl Compile item"),
            &ProjectItems::default(),
            &env,
            &parses.files[0].recovery,
        );
        let names: Vec<_> = resolved
            .exports()
            .iter()
            .map(|e| e.name(&env).to_string())
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
            .parses_for_project(&proj, &mut ws, &Vfs::new())
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
        let mut docs = Vfs::new();
        docs.insert(
            Uri::from_file_path(&opened).unwrap(),
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
        let raw = crate::observed::host_fs::read_to_string(&fixture).unwrap();
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
    /// a lex-filter decision across the F# 6 or F# 8 boundary).
    /// Version-invariant files keep folding under the same untrusted
    /// provenance, so a project with an unpinnable `LangVersion` write loses
    /// only the files whose tree the version actually moves. (An ordinary SDK
    /// project's provenance is trusted and never reaches this gate —
    /// `sdk_project_fold_e2e`.)
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
            sema.parses_for_project(&proj, &mut ws, &Vfs::new())
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
            sema.parses_for_project(&plain, &mut ws, &Vfs::new())
                .is_some(),
            "a version-invariant parse is provably safe under any LangVersion"
        );

        // The flag over-approximates: an EOF-anchored version-gated push (a
        // common mid-edit state — `module M =` at end of file) sets it, yet
        // the EOF force-closure cascade reconverges to identical trees on
        // both sides of the boundary. The fold must verify genuine
        // divergence before refusing, or every untrusted-provenance project
        // would lose resolution while the user types.
        let midedit = tmp.path().join("MidEdit/P.fsproj");
        write(
            &midedit,
            &gated_lang_version(r#"<ItemGroup><Compile Include="D.fs" /></ItemGroup>"#),
        );
        write(&tmp.path().join("MidEdit/D.fs"), "module M =\n");
        assert!(
            sema.parses_for_project(&midedit, &mut ws, &Vfs::new())
                .is_some(),
            "a flagged parse that reconverges to the same tree is provably \
             safe and must fold"
        );

        // Straddling file but a cleanly pinned version: the version is known
        // (an unconditional body write), so the shape dependence is resolved
        // and the fold proceeds.
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
            sema.parses_for_project(&pinned, &mut ws, &Vfs::new())
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
        let docs = Vfs::new();
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

        let recovered = entry_ref_tfms(
            &entry,
            Some("net8.0-windows"),
            &borzoi_msbuild::observed::Observer::new(),
        );
        let canon_cs = crate::observed::host_fs::canonicalize(&cs).unwrap();
        assert_eq!(
            recovered.get(&canon_cs).map(String::as_str),
            Some("net8.0-windows"),
            "{recovered:?}"
        );

        // Pin the failure this closes: the base-TFM guess cannot find the
        // producer's target at all.
        assert!(
            resolve_transitive_project_tfms(
                &cs,
                "net8.0",
                &borzoi_msbuild::observed::Observer::new()
            )
            .is_err(),
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
        assert!(
            entry_ref_tfms(&entry, None, &borzoi_msbuild::observed::Observer::new()).is_empty()
        );
        assert!(
            entry_ref_tfms(
                &entry,
                Some("net8.0"),
                &borzoi_msbuild::observed::Observer::new()
            )
            .is_empty()
        );
    }

    /// A project that records no SDK root takes the env's dotnet root from a
    /// probe of a bare `Microsoft.NET.Sdk` (`Workspace::dotnet_root_for_project`),
    /// which the cached fold's fast path never runs again. So the env records
    /// what the probe read, and an SDK installed in an earlier `global.json`
    /// `sdk.paths` root, which moves the probe's answer, drops the env and
    /// the fold with it: the next lookup resolves against the new root
    /// (found by Codex, where a structural watch event had dropped the fold).
    #[test]
    fn an_env_records_what_choosing_its_dotnet_root_read() {
        let tmp = TempDir::new().unwrap();
        let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        let install = |dotnet: &Path| {
            let sdk = dotnet.join("sdk/9.0.100/Sdks/Microsoft.NET.Sdk/Sdk");
            std::fs::create_dir_all(&sdk).unwrap();
            std::fs::write(sdk.join("Sdk.props"), "<Project/>").unwrap();
            std::fs::write(sdk.join("Sdk.targets"), "<Project/>").unwrap();
        };
        std::fs::create_dir_all(&first).unwrap();
        install(&second);
        write(
            &root.join("global.json"),
            r#"{ "sdk": { "paths": ["first", "second"] } }"#,
        );
        let proj = root.join("app/P.fsproj");
        write(&proj, &fsproj(&["A.fs"]));
        write(&root.join("app/A.fs"), "module A\nlet x = 1\n");

        let mut ws = Workspace::with_env(crate::sdk_discovery::SdkDiscoveryEnv::default());
        let mut sema = SemanticState::new();
        let docs = Vfs::new();
        let old = crate::vfs::Nanos(i128::MAX / 2);
        let fold = |ws: &mut Workspace, sema: &mut SemanticState| {
            ws.begin_generation(old);
            let (_, env) = sema
                .resolved_project_and_env_for(&proj, ws, &docs)
                .expect("the project folds");
            ws.end_generation();
            env
        };
        assert_eq!(ws.dotnet_root_for_project(&proj), Some(second.clone()));
        let before = fold(&mut ws, &mut sema);
        assert!(
            Arc::ptr_eq(&before, &fold(&mut ws, &mut sema)),
            "a cached fold"
        );
        install(&first);
        assert_eq!(ws.dotnet_root_for_project(&proj), Some(first.clone()));
        let after = fold(&mut ws, &mut sema);
        assert!(
            !Arc::ptr_eq(&before, &after),
            "the env chosen by the old probe answer was served"
        );
    }

    /// The envs are keyed on neither trust opt-in, so turning either on or
    /// off drops every env: one resolved without an on-demand restore must
    /// not be served once a restore may run, nor the reverse.
    #[test]
    fn a_trust_opt_in_change_drops_every_env() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&[]));
        let ws = Workspace::default();
        let mut sema = SemanticState::new();
        let env = |sema: &mut SemanticState| {
            sema.assembly_env_for_project(&proj, None, &ServedTfm::NoneDeclared, &ws)
        };
        let first = env(&mut sema);
        assert!(Arc::ptr_eq(&first, &env(&mut sema)));
        sema.set_on_demand_restore_enabled(true);
        let restoring = env(&mut sema);
        assert!(
            !Arc::ptr_eq(&first, &restoring),
            "the restore opt-in kept an env"
        );
        sema.set_csharp_project_references_enabled(true);
        assert!(
            !Arc::ptr_eq(&restoring, &env(&mut sema)),
            "the C# opt-in kept an env"
        );
    }

    /// The broad invalidations — a moved projects generation (a dropped
    /// evaluation, a moved owner, a structural watch event:
    /// [`SemanticState::sync_with_evaluations`]), a referenced-assembly
    /// watch event ([`SemanticState::invalidate_assembly_state`]) — drop an
    /// env whose resolution consulted a reader that records nothing (here,
    /// the C# sidecar for a C# reference), which nothing else covers, and keep
    /// one whose reads are all recorded, which is confirmed against the disk
    /// before it is served again.
    #[test]
    fn the_broad_invalidations_drop_only_an_env_with_an_unrecorded_reader() {
        let tmp = TempDir::new().unwrap();
        let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
        let fsharp_only = root.join("F/F.fsproj");
        write(&fsharp_only, &fsproj(&[]));
        let app = root.join("App/App.fsproj");
        write(
            &app,
            "<Project><ItemGroup><ProjectReference Include=\"../Lib/Lib.csproj\" /></ItemGroup></Project>",
        );
        write(&root.join("Lib/Lib.csproj"), "<Project />");
        write_app_assets(app.parent().unwrap(), false);
        let dotnet = root.join("dotnet");
        std::fs::create_dir_all(&dotnet).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        sema.set_csharp_project_references_enabled(true);
        let env = |sema: &mut SemanticState, ws: &Workspace, project: &Path| {
            sema.assembly_env_for_project(project, Some(&dotnet), &ServedTfm::NoneDeclared, ws)
        };
        env(&mut sema, &ws, &app);
        let unrecorded = |sema: &mut SemanticState, ws: &Workspace, project: &Path| {
            sema.env_reads_for_project(project, Some(&dotnet), &ServedTfm::NoneDeclared, ws)
                .0
                .unrecorded
                .clone()
        };
        assert_eq!(unrecorded(&mut sema, &ws, &app), vec![UNRECORDED_CSHARP]);
        assert!(unrecorded(&mut sema, &ws, &fsharp_only).is_empty());

        type Invalidation = (&'static str, fn(&mut SemanticState, &mut Workspace));
        let invalidations: [Invalidation; 2] = [
            ("a moved projects generation", |sema, ws| {
                ws.note_structural_event();
                sema.sync_with_evaluations(ws.projects_generation());
            }),
            ("invalidate_assembly_state", |sema, _| {
                sema.invalidate_assembly_state();
            }),
        ];
        for (name, invalidate) in invalidations {
            let app_before = env(&mut sema, &ws, &app);
            let fsharp_only_before = env(&mut sema, &ws, &fsharp_only);
            invalidate(&mut sema, &mut ws);
            assert!(
                !Arc::ptr_eq(&app_before, &env(&mut sema, &ws, &app)),
                "{name} kept an env the C# sidecar answered part of"
            );
            assert!(
                Arc::ptr_eq(&fsharp_only_before, &env(&mut sema, &ws, &fsharp_only)),
                "{name} dropped an env whose reads are all recorded"
            );
        }
    }

    // ---- graph_ref_targets: graph-sourced reference edges ----

    fn graph_node(path: &Path, kind: ProjectKind) -> crate::project_graph::ProjectNode {
        crate::project_graph::ProjectNode {
            path: lexically_normalize(path),
            kind,
            references: Vec::new(),
            tfm: crate::project_graph::NodeTfm::NotEvaluated,
            output_name: None,
            references_uncertain: false,
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
        let targets = graph_ref_targets(&graph, entry, &borzoi_msbuild::observed::Observer::new());
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
        let targets = graph_ref_targets(&graph, &entry, &borzoi_msbuild::observed::Observer::new());
        assert_eq!(
            targets.fsharp,
            vec![FsharpRefTarget {
                path: crate::observed::host_fs::canonicalize(&dep).unwrap(),
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
                crate::observed::host_fs::read(&dll).expect("read MiniLibFs.dll fixture")
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
        for entry in crate::observed::host_fs::read_dir(src)
            .unwrap_or_else(|e| panic!("read {}: {e}", src.display()))
        {
            let entry = entry.expect("read fixture tree entry");
            let name = entry.file_name();
            let from = entry.path();
            let to = dst.join(&name);
            if from.host_is_dir() {
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
        let found = locate_fsharp_output_dll(
            &proj,
            None,
            "Lib",
            &borzoi_msbuild::observed::Observer::new(),
        )
        .expect_found("built DLL is located");
        assert!(found.ends_with("bin/Release/net10.0/Lib.dll"), "{found:?}");
    }

    #[test]
    fn locate_output_dll_none_when_unbuilt() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Lib.fsproj");
        write(&proj, "<Project />");
        // No bin/ at all.
        assert!(!matches!(
            locate_fsharp_output_dll(
                &proj,
                None,
                "Lib",
                &borzoi_msbuild::observed::Observer::new()
            ),
            OutputLocation::Found(_)
        ));
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

        let found = locate_fsharp_output_dll(
            &proj,
            None,
            "Lib",
            &borzoi_msbuild::observed::Observer::new(),
        )
        .expect_found("a DLL is located");
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
        assert!(!matches!(
            locate_fsharp_output_dll(
                &proj,
                None,
                "Lib",
                &borzoi_msbuild::observed::Observer::new()
            ),
            OutputLocation::Found(_)
        ));
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
        assert!(!matches!(
            locate_fsharp_output_dll(
                &proj,
                None,
                "Lib",
                &borzoi_msbuild::observed::Observer::new()
            ),
            OutputLocation::Found(_)
        ));
        // …but the recovered name finds the override's output.
        let found = locate_fsharp_output_dll(
            &proj,
            None,
            "Renamed",
            &borzoi_msbuild::observed::Observer::new(),
        )
        .expect_found("located");
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
        assert!(!matches!(
            locate_fsharp_output_dll(
                &proj,
                None,
                "Renamed",
                &borzoi_msbuild::observed::Observer::new()
            ),
            OutputLocation::Found(_)
        ));
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
            !matches!(
                locate_fsharp_output_dll(
                    &proj,
                    None,
                    "Lib",
                    &borzoi_msbuild::observed::Observer::new()
                ),
                OutputLocation::Found(_)
            ),
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

        let found = locate_fsharp_output_dll(
            &proj,
            Some("net8.0"),
            "Lib",
            &borzoi_msbuild::observed::Observer::new(),
        )
        .expect_found("located");
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
        assert!(!matches!(
            locate_fsharp_output_dll(
                &proj,
                Some("net8.0"),
                "Lib",
                &borzoi_msbuild::observed::Observer::new()
            ),
            OutputLocation::Found(_)
        ));
    }

    impl OutputLocation {
        /// The located DLL, panicking with `msg` on any other outcome — for the
        /// tests that are about *which* output is chosen.
        fn expect_found(self, msg: &str) -> PathBuf {
            match self {
                OutputLocation::Found(dll) => dll,
                other => panic!("{msg}: {other:?}"),
            }
        }
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

    /// The DLLs the F# reference edges contributed — what the pre-conservation
    /// `fsharp_project_ref_dlls` returned, for the tests that are about *which
    /// output* a ref resolves to rather than about the grade it earns.
    fn fsharp_ref_paths(
        project_refs: &[FsharpRefTarget],
        assembly_names: &BTreeMap<PathBuf, String>,
    ) -> Vec<PathBuf> {
        edge_dlls(&fsharp_project_ref_edges(
            project_refs,
            assembly_names,
            &borzoi_msbuild::observed::Observer::new(),
        ))
    }

    /// The single outcome of a one-edge resolution.
    fn fsharp_ref_outcome(
        project_refs: &[FsharpRefTarget],
        assembly_names: &BTreeMap<PathBuf, String>,
    ) -> ReferenceOutcome {
        let edges = fsharp_project_ref_edges(
            project_refs,
            assembly_names,
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(edges.len(), 1, "helper is for a single edge");
        edges.into_iter().next().expect("one edge").outcome
    }

    #[test]
    fn fsharp_project_ref_dlls_uses_the_node_tfm() {
        let tmp = TempDir::new().unwrap();
        let proj = built_fsproj(&tmp.path().join("Fs"), "Fs", "Debug", "net10.0", b"ten");
        let eight = tmp.path().join("Fs/bin/Debug/net8.0/Fs.dll");
        write(&eight, "");
        std::fs::write(&eight, b"eight").unwrap();

        let dlls = fsharp_ref_paths(
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
        let dlls = fsharp_ref_paths(&[ref_target(proj, NodeTfm::Unresolved)], &BTreeMap::new());
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
        let dlls = fsharp_ref_paths(&[ref_target(proj, NodeTfm::NotEvaluated)], &BTreeMap::new());
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
        let dlls = fsharp_ref_paths(&refs, &BTreeMap::new());
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
        let dlls = fsharp_ref_paths(
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
        let dlls = fsharp_ref_paths(&refs, &BTreeMap::new());
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
        let dlls = fsharp_ref_paths(&refs, &BTreeMap::new());
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
        let dlls = fsharp_ref_paths(&refs, &names);
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
        let dlls = fsharp_ref_paths(&refs, &names);
        assert_eq!(dlls.len(), 1, "{dlls:?}");
        assert!(
            dlls[0].ends_with("Fs/bin/Debug/net10.0/FromAssets.dll"),
            "{:?}",
            dlls[0]
        );
    }

    // ---- reference-edge conservation and grading --------------------------

    /// `child_dirs` must separate "not there" from "could not look", because
    /// only the first is evidence that a project is unbuilt.
    #[test]
    fn child_dirs_separates_an_absent_tree_from_an_unreadable_one() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            child_dirs(
                &tmp.path().join("nope"),
                &borzoi_msbuild::observed::Observer::new()
            ),
            Some(Vec::new()),
            "an absent directory is a proven absence"
        );
        // A plain file where a directory was expected: `read_dir` fails with
        // something other than `NotFound`, which is the portable stand-in for
        // any tree we cannot enumerate (a permission bit would need root-aware
        // test setup to be reliable).
        let not_a_dir = tmp.path().join("bin");
        write(&not_a_dir, "");
        assert_eq!(
            child_dirs(&not_a_dir, &borzoi_msbuild::observed::Observer::new()),
            None,
            "a tree we cannot read must not read as empty"
        );
    }

    /// An unread reference whose cause may clear on its own must also make the
    /// env non-cacheable, or the filesystem recovers and every assembly reading
    /// goes on deferring until something unrelated invalidates the entry.
    #[test]
    fn a_transient_scan_failure_makes_the_env_non_cacheable() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Lib").join("Lib.fsproj");
        write(&proj, "<Project />");
        write(&tmp.path().join("Lib").join("bin"), "");

        let edges = fsharp_project_ref_edges(
            &[ref_target(proj, NodeTfm::Known("net10.0".into()))],
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        let set = ReferenceSet {
            dlls: Vec::new(),
            edges,
            retryable: false,
            unrecorded: Vec::new(),
            package_folders: Vec::new(),
        };
        assert_eq!(set.unread().count(), 1);
        assert!(
            set.has_transient_failure(),
            "an I/O error while scanning is the kind that may clear"
        );

        // A verdict we decline to trust is *stable*: retrying changes nothing,
        // so it must not force a re-resolve on every request.
        let stable = tmp.path().join("Stable").join("Stable.fsproj");
        write(&stable, "<Project />");
        let set = ReferenceSet {
            dlls: Vec::new(),
            edges: fsharp_project_ref_edges(
                &[ref_target(stable, NodeTfm::Unresolved)],
                &BTreeMap::new(),
                &borzoi_msbuild::observed::Observer::new(),
            ),
            retryable: false,
            unrecorded: Vec::new(),
            package_folders: Vec::new(),
        };
        assert_eq!(set.unread().count(), 1);
        assert!(!set.has_transient_failure());
    }

    /// A `bin` tree we cannot enumerate is **unread**, not unbuilt.
    ///
    /// The locator answers `None` for several unrelated reasons, and reading
    /// that single `None` as "not built" would put an unreadable reference in
    /// the one grade that leaves the env complete — the same collapse of
    /// distinct causes into one absence that the edge conservation exists to
    /// prevent, one layer down.
    #[test]
    fn an_unreadable_bin_tree_is_unread_not_unbuilt() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("Lib").join("Lib.fsproj");
        write(&proj, "<Project />");
        write(&tmp.path().join("Lib").join("bin"), "");

        assert!(
            matches!(
                locate_fsharp_output_dll(
                    &proj,
                    Some("net10.0"),
                    "Lib",
                    &borzoi_msbuild::observed::Observer::new()
                ),
                OutputLocation::Undecidable { .. }
            ),
            "an unreadable bin tree is undecidable, not a proven absence"
        );
        assert!(matches!(
            fsharp_ref_outcome(
                &[ref_target(proj, NodeTfm::Known("net10.0".into()))],
                &BTreeMap::new()
            ),
            ReferenceOutcome::Unread { .. }
        ));
    }

    /// Several TFM variants built and nothing to select between them is
    /// likewise undecidable, not unbuilt: the outputs are right there.
    #[test]
    fn an_ambiguous_multi_tfm_output_is_unread_not_unbuilt() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("Lib");
        let proj = root.join("Lib.fsproj");
        write(&proj, "<Project />");
        write(&root.join("bin/Debug/net10.0/Lib.dll"), "ten");
        write(&root.join("bin/Debug/net8.0/Lib.dll"), "eight");

        assert!(
            matches!(
                locate_fsharp_output_dll(
                    &proj,
                    None,
                    "Lib",
                    &borzoi_msbuild::observed::Observer::new()
                ),
                OutputLocation::Undecidable { .. }
            ),
            "two built TFMs and no producer TFM cannot be resolved"
        );
        assert!(matches!(
            fsharp_ref_outcome(&[ref_target(proj, NodeTfm::NoneDeclared)], &BTreeMap::new()),
            ReferenceOutcome::Unread { .. }
        ));
    }

    /// Every declared edge yields exactly one outcome, whatever became of it.
    ///
    /// This is the property the whole grade rests on: before it, a reference
    /// that resolved to no DLL left nothing behind and was indistinguishable
    /// from one that was never declared, so nothing downstream could tell a
    /// reference was missing. A count of paths cannot say *which* is absent —
    /// a C# edge contributes a whole closure, an F# one contributes one path or
    /// none — which is exactly why the omission went unnoticed.
    #[test]
    fn every_declared_edge_yields_exactly_one_outcome() {
        let tmp = TempDir::new().unwrap();
        // A spread over every F# grade at once: built, unbuilt, untrustworthy
        // verdict, no output name, and a non-F# path the filing should never
        // produce.
        let built = built_fsproj(&tmp.path().join("Built"), "Built", "Debug", "net10.0", b"x");
        let unbuilt = tmp.path().join("Unbuilt").join("Unbuilt.fsproj");
        write(&unbuilt, "<Project />");
        let stray = tmp.path().join("Stray").join("Stray.csproj");
        write(&stray, "<Project />");
        let refs = vec![
            ref_target(built, NodeTfm::Known("net10.0".into())),
            ref_target(unbuilt.clone(), NodeTfm::Known("net10.0".into())),
            ref_target(unbuilt.clone(), NodeTfm::Unresolved),
            FsharpRefTarget {
                path: unbuilt,
                tfm: NodeTfm::Known("net10.0".into()),
                output_name: None,
            },
            ref_target(stray, NodeTfm::Known("net10.0".into())),
        ];

        let edges = fsharp_project_ref_edges(
            &refs,
            &BTreeMap::new(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            edges.len(),
            refs.len(),
            "one outcome per declared edge, got {edges:?}"
        );
        for (edge, target) in edges.iter().zip(&refs) {
            assert_eq!(edge.project, target.path, "outcomes stay in edge order");
        }
    }

    /// The grades themselves, since only [`ReferenceOutcome::Unread`] makes the
    /// env incomplete and confusing two of them is a soundness change.
    #[test]
    fn an_unbuilt_reference_grades_apart_from_an_unreadable_one() {
        let tmp = TempDir::new().unwrap();
        let built = built_fsproj(&tmp.path().join("Built"), "Built", "Debug", "net10.0", b"x");
        let unbuilt = tmp.path().join("Unbuilt").join("Unbuilt.fsproj");
        write(&unbuilt, "<Project />");

        assert!(matches!(
            fsharp_ref_outcome(
                &[ref_target(built, NodeTfm::Known("net10.0".into()))],
                &BTreeMap::new()
            ),
            ReferenceOutcome::Resolved(_)
        ));
        // A trustworthy name and TFM, but nobody has built it: the one grade
        // that leaves the env complete.
        assert_eq!(
            fsharp_ref_outcome(
                &[ref_target(
                    unbuilt.clone(),
                    NodeTfm::Known("net10.0".into())
                )],
                &BTreeMap::new()
            ),
            ReferenceOutcome::NotBuilt
        );
        // An untrustworthy verdict is a different thing entirely — we cannot
        // say what this reference even is.
        assert!(matches!(
            fsharp_ref_outcome(
                &[ref_target(unbuilt.clone(), NodeTfm::Unresolved)],
                &BTreeMap::new()
            ),
            ReferenceOutcome::Unread { .. }
        ));
        assert!(matches!(
            fsharp_ref_outcome(
                &[FsharpRefTarget {
                    path: unbuilt,
                    tfm: NodeTfm::Known("net10.0".into()),
                    output_name: None,
                }],
                &BTreeMap::new()
            ),
            ReferenceOutcome::Unread { .. }
        ));
    }

    /// A node whose reference list was suppressed reaches
    /// [`GraphRefTargets::suppressed`], including the entry project itself —
    /// its own dropped references are missing from the env exactly as a
    /// sibling's are, even though the entry contributes no output DLL of its
    /// own.
    #[test]
    fn graph_ref_targets_records_suppressed_nodes() {
        let entry = Path::new("/p/App/App.fsproj");
        let dep = Path::new("/p/LibA/LibA.fsproj");
        let clean = Path::new("/p/LibB/LibB.fsproj");
        let suppressed = |path: &Path| {
            let mut node = graph_node(path, ProjectKind::FSharp);
            node.references_uncertain = true;
            node
        };
        let graph = ProjectGraph {
            nodes: vec![
                suppressed(entry),
                suppressed(dep),
                graph_node(clean, ProjectKind::FSharp),
            ],
            problems: Vec::new(),
        };

        let targets = graph_ref_targets(&graph, entry, &borzoi_msbuild::observed::Observer::new());
        assert_eq!(
            targets.suppressed,
            vec![PathBuf::from(entry), PathBuf::from(dep)],
            "both suppressed nodes are recorded; a clean one is not"
        );
        // The entry is still excluded from the *reference* set — it is not one
        // of its own references — so the two lists are independent.
        assert_eq!(
            targets.fsharp.iter().map(|t| &t.path).collect::<Vec<_>>(),
            vec![&PathBuf::from(dep), &PathBuf::from(clean)]
        );
    }

    /// A suppressed node earns exactly one [`ReferenceOutcome::Unread`], which
    /// is what makes the env's identity set incomplete. There is no edge to
    /// attach it to — the references are gone — so it is the *losing node*
    /// that is named.
    #[test]
    fn a_suppressed_node_makes_the_identity_set_incomplete() {
        let suppressed = PathBuf::from("/p/LibA/LibA.fsproj");
        let edges: Vec<ReferenceEdge> = [suppressed.clone()]
            .iter()
            .map(|project| ReferenceEdge {
                project: project.clone(),
                outcome: ReferenceOutcome::Unread {
                    why: "the project's own reference list could not be trusted",
                    transient: false,
                },
            })
            .collect();
        let set = ReferenceSet {
            dlls: Vec::new(),
            edges,
            retryable: false,
            unrecorded: Vec::new(),
            package_folders: Vec::new(),
        };
        assert_eq!(
            set.unread().collect::<Vec<_>>(),
            [(
                suppressed.as_path(),
                "the project's own reference list could not be trusted"
            )]
        );
        assert_eq!(set.not_built().count(), 0);
        assert!(
            !set.has_transient_failure(),
            "only an edit to that project can change the verdict, so the env is cacheable"
        );
    }

    /// The behavioural claim: an `Unread` reference marks the env's identity set
    /// incomplete — so the seal fires and every assembly reading defers — while
    /// an unbuilt one leaves it complete.
    ///
    /// Driven through `ReferenceSet` rather than `build_assembly_env`, which
    /// would need a restored project and a live sidecar; the mark is a pure
    /// function of the outcomes, and this pins that function.
    #[test]
    fn only_an_unread_reference_makes_the_identity_set_incomplete() {
        let edge = |project: &str, outcome| ReferenceEdge {
            project: PathBuf::from(project),
            outcome,
        };
        let set = |edges| ReferenceSet {
            dlls: Vec::new(),
            edges,
            retryable: false,
            unrecorded: Vec::new(),
            package_folders: Vec::new(),
        };

        let unbuilt = set(vec![edge("A.fsproj", ReferenceOutcome::NotBuilt)]);
        assert_eq!(unbuilt.unread().count(), 0);
        assert_eq!(
            unbuilt.not_built().collect::<Vec<_>>(),
            [Path::new("A.fsproj")]
        );

        let unread = set(vec![
            edge("A.fsproj", ReferenceOutcome::NotBuilt),
            edge(
                "B.csproj",
                ReferenceOutcome::Unread {
                    why: "no producer TFM known",
                    transient: false,
                },
            ),
        ]);
        assert_eq!(
            unread.unread().collect::<Vec<_>>(),
            [(Path::new("B.csproj"), "no producer TFM known")],
            "an unread reference must be reported even alongside an unbuilt one"
        );

        // A declined resolution claims neither: the env is empty, so it makes no
        // assembly reading for a missing reference to be wrong about.
        let declined = ReferenceSet::declined(false);
        assert_eq!(declined.unread().count(), 0);
        assert_eq!(declined.not_built().count(), 0);
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

    /// A resolution that consults an unrecording reader says so whatever the
    /// reader concludes: an on-demand restore that declines (here, with no
    /// `dotnet` to run), and C# references the sidecar is enabled for that
    /// deliver nothing (here, with no producer TFM known), each leave the
    /// resolution depending on what that reader would read.
    #[test]
    fn a_consulted_unrecording_reader_is_named_whatever_it_concludes() {
        let tmp = TempDir::new().unwrap();
        let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
        let project = root.join("App/App.fsproj");
        write(&project, "<Project />");
        let dotnet = root.join("dotnet");
        std::fs::create_dir_all(&dotnet).unwrap();
        let restore_env = SdkDiscoveryEnv {
            dotnet_root: Some(root.join("no-dotnet")),
            ..SdkDiscoveryEnv::default()
        };
        let declined = resolve_reference_dlls(
            &project,
            Some(&dotnet),
            &ServedTfm::NoneDeclared,
            CsharpReferences::Disabled,
            &GraphRefTargets::default(),
            &BTreeMap::new(),
            Some(&restore_env),
            &Observer::new(),
        );
        assert!(
            declined.dlls.is_empty(),
            "precondition: the restore declines"
        );
        assert_eq!(declined.unrecorded, vec![UNRECORDED_RESTORE]);

        write_app_assets(project.parent().unwrap(), false);
        let mut sidecar = SidecarManager::default();
        let targets = GraphRefTargets {
            csharp: vec![root.join("Lib/Lib.csproj")],
            ..GraphRefTargets::default()
        };
        let unread = resolve_reference_dlls(
            &project,
            Some(&dotnet),
            &ServedTfm::NoneDeclared,
            CsharpReferences::Enabled(&mut sidecar),
            &targets,
            &BTreeMap::new(),
            None,
            &Observer::new(),
        );
        assert_eq!(
            unread.unread().count(),
            1,
            "precondition: the C# reference delivers nothing"
        );
        assert_eq!(unread.unrecorded, vec![UNRECORDED_CSHARP]);
        let disabled = resolve_reference_dlls(
            &project,
            Some(&dotnet),
            &ServedTfm::NoneDeclared,
            CsharpReferences::Disabled,
            &targets,
            &BTreeMap::new(),
            None,
            &Observer::new(),
        );
        assert!(
            disabled.unrecorded.is_empty(),
            "a disabled sidecar reads nothing: {:?}",
            disabled.unrecorded
        );
    }

    /// An env kept across a resolution run again carries that resolution's
    /// unrecorded readers, which decide what drops it besides its checks
    /// ([`SemanticState::invalidate_assembly_state`]). The project has no
    /// framework pack to resolve, so its assets file resolves to no DLL, and
    /// so does the on-demand restore that runs, and declines, once the
    /// assets file is deleted: the env is built from the same inputs, kept,
    /// and must name the restore.
    #[test]
    fn a_kept_env_names_the_unrecorded_readers_of_the_resolution_that_kept_it() {
        let tmp = TempDir::new().unwrap();
        let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
        let project = root.join("App/App.fsproj");
        write(&project, "<Project />");
        write_app_assets(project.parent().unwrap(), false);
        let dotnet = root.join("dotnet");
        std::fs::create_dir_all(&dotnet).unwrap();
        let mut ws = Workspace::with_env(crate::sdk_discovery::SdkDiscoveryEnv {
            dotnet_root: Some(root.join("no-dotnet")),
            ..crate::sdk_discovery::SdkDiscoveryEnv::default()
        });
        let mut sema = SemanticState::new();
        sema.set_on_demand_restore_enabled(true);
        let old = crate::vfs::Nanos(i128::MAX / 2);
        let generation = |ws: &mut Workspace, sema: &mut SemanticState| {
            ws.begin_generation(old);
            sema.begin_generation();
            let env = sema.assembly_env_for_project(
                &project,
                Some(&dotnet),
                &ServedTfm::NoneDeclared,
                ws,
            );
            let (reads, _) =
                sema.env_reads_for_project(&project, Some(&dotnet), &ServedTfm::NoneDeclared, ws);
            let counts = sema.end_generation();
            ws.end_generation();
            (env, reads.unrecorded.clone(), counts)
        };
        let (before, unrecorded, _) = generation(&mut ws, &mut sema);
        assert!(unrecorded.is_empty(), "{unrecorded:?}");
        std::fs::remove_file(project.parent().unwrap().join("obj/project.assets.json")).unwrap();
        let (after, unrecorded, counts) = generation(&mut ws, &mut sema);
        assert!(
            Arc::ptr_eq(&before, &after) && counts.envs_resolved_again == 1,
            "precondition: the env was kept by resolving again: {counts:?}"
        );
        assert_eq!(unrecorded, vec![UNRECORDED_RESTORE]);
    }

    /// An env whose references are resolved again after an edit to its
    /// project that moves nothing else it read (a comment), and kept, starts
    /// with the witnesses of the resolution it replaces and of the project's
    /// evaluation: the next confirmation of its resolution costs what
    /// confirming an unchanged env does, and asks nothing again (#733). The
    /// project file is rewritten in place, so no directory's stamp moves.
    ///
    /// The resolution's absence probes reach the temporary directory's
    /// ancestors, where concurrent tests make and remove directories, and a
    /// witness that sees one moved asks again, as it must; so the scenario
    /// is run until a run sees nothing else move. Without the shared
    /// witnesses no run can.
    #[test]
    fn an_env_resolved_again_starts_with_the_witnesses_it_shares() {
        let attempt = || {
            let tmp = TempDir::new().unwrap();
            let root = crate::observed::host_fs::canonicalize(tmp.path()).unwrap();
            let project = root.join("App/App.fsproj");
            write(&project, "<Project />");
            write_app_assets(project.parent().unwrap(), false);
            let dotnet = root.join("dotnet");
            std::fs::create_dir_all(&dotnet).unwrap();
            let mut ws = Workspace::with_env(crate::sdk_discovery::SdkDiscoveryEnv {
                dotnet_root: Some(root.join("no-dotnet")),
                ..crate::sdk_discovery::SdkDiscoveryEnv::default()
            });
            let mut sema = SemanticState::new();
            let mut at = 0;
            let mut generation = |ws: &mut Workspace, sema: &mut SemanticState| {
                // Old enough that no stamp is racy, so witnesses are used.
                ws.begin_generation(crate::vfs::Nanos(i128::MAX / 2 + at));
                at += 1;
                sema.begin_generation();
                let env = sema.assembly_env_for_project(
                    &project,
                    Some(&dotnet),
                    &ServedTfm::NoneDeclared,
                    ws,
                );
                let counts = sema.end_generation();
                (env, counts, ws.end_generation())
            };
            let (built, _, _) = generation(&mut ws, &mut sema);
            // The first confirmation takes the witnesses; the second is what
            // confirming an unchanged env costs.
            generation(&mut ws, &mut sema);
            let (_, _, steady) = generation(&mut ws, &mut sema);
            assert!(steady.envs.stats > 0, "{steady:?}");

            write(&project, "<Project><!-- c --></Project>");
            let (kept, counts, _) = generation(&mut ws, &mut sema);
            assert!(
                Arc::ptr_eq(&built, &kept) && counts.envs_resolved_again == 1,
                "precondition: the env was kept by resolving again: {counts:?}"
            );
            let (_, _, first) = generation(&mut ws, &mut sema);
            (first.envs, steady.envs)
        };
        let mut runs = Vec::new();
        while runs.len() < 5 && !runs.last().is_some_and(|(first, steady)| first == steady) {
            runs.push(attempt());
        }
        assert!(
            runs.iter().any(|(first, steady)| first == steady),
            "the new resolution's first confirmation, against a steady one: {runs:?}"
        );
    }

    /// End-to-end: a sibling whose own `<ProjectReference>` list the evaluator
    /// cannot trust leaves the env's identity set **incomplete**, so the seal
    /// fires and no assembly reading commits.
    ///
    /// The sibling itself resolves fine — its DLL is built and folded — which
    /// is exactly why this needs the node mark: nothing about the sibling's
    /// own edge is missing, and the loss is entirely in the references it
    /// dropped, which never became nodes at all.
    #[test]
    fn a_suppressed_reference_list_makes_the_env_incomplete() {
        let dll_bytes = minilibfs_dll_bytes();

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let sibling_dir = root.join("MiniLibFs");
        std::fs::create_dir_all(&sibling_dir).unwrap();
        built_fsproj(&sibling_dir, "MiniLibFs", "Release", "net10.0", &dll_bytes);
        // `Update` on a `<ProjectReference>` is unmodelled, so the captured
        // list may claim references the real build strips: the whole list is
        // suppressed.
        write(
            &sibling_dir.join("MiniLibFs.fsproj"),
            r#"<Project>
              <ItemGroup>
                <ProjectReference Include="../Other/Other.fsproj" />
                <ProjectReference Update="../Other/Other.fsproj" Private="false" />
              </ItemGroup>
            </Project>"#,
        );

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
            "the sibling's own output still folds — the loss is its dropped references"
        );
        assert!(
            env.identities_incomplete(),
            "a suppressed reference list leaves an unknown number of assemblies \
             missing, so the env must not claim to be complete"
        );
    }

    /// A restored `App.fsproj` whose one `<ProjectReference>` is the C# project
    /// `Cs/Cs.csproj` (recorded in the assets file with its producer TFM, so the
    /// env build reaches the sidecar), and a fake SDK root whose `dotnet` is a
    /// stub that creates a sentinel file whenever anything runs it. Returns
    /// `(root, app, dotnet_root, sentinel)`; `root` owns the tree.
    #[cfg(unix)]
    fn csharp_ref_fixture_with_stub_dotnet() -> (TempDir, PathBuf, PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        write(&root.join("Cs/Cs.csproj"), "<Project />");
        let app = root.join("App.fsproj");
        write(&app, &fsproj_with_refs(&["Cs/Cs.csproj"]));
        let pkgs = root.join("pkgs");
        fs::create_dir_all(&pkgs).unwrap();
        let assets = serde_json::json!({
            "version": 3,
            "targets": { "net10.0": {
                "Cs/1.0.0": { "type": "project", "framework": ".NETCoreApp,Version=v10.0" }
            } },
            "libraries": {
                "Cs/1.0.0": { "type": "project", "path": "Cs/Cs.csproj", "msbuildProject": "Cs/Cs.csproj" }
            },
            "packageFolders": { pkgs.to_str().unwrap(): {} },
            "project": { "frameworks": { "net10.0": {} } }
        });
        write(
            &root.join("obj/project.assets.json"),
            &serde_json::to_string(&assets).unwrap(),
        );

        let dotnet_root = root.join("dotnet");
        let sentinel = root.join("dotnet-was-run");
        let stub = dotnet_root.join("dotnet");
        write(
            &stub,
            &format!(
                "#!/bin/sh\ntouch '{}'\nexit 1\n",
                sentinel.to_str().expect("utf-8 temp path")
            ),
        );
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        (tmp, app, dotnet_root, sentinel)
    }

    /// The C# sidecar runs a design-time build of each referenced `.csproj` —
    /// its targets, analyzers and generators — so with
    /// `enableCSharpProjectReferences` off (the default) the env build must
    /// never start a process for it, nor write under `obj/borzoi/`. The C#
    /// edge is still a declared reference whose types are unknown, so the env
    /// is incomplete rather than silently missing it.
    #[cfg(unix)]
    #[test]
    fn csharp_references_off_by_default_spawn_nothing_and_leave_the_env_incomplete() {
        let (tmp, app, dotnet_root, sentinel) = csharp_ref_fixture_with_stub_dotnet();

        let mut sema = SemanticState::new();
        let env = sema.assembly_env_for_project(
            &app,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );

        assert!(
            !sentinel.host_exists(),
            "the sidecar's `dotnet` ran although C# project references are off"
        );
        assert!(
            !tmp.path().join("obj/borzoi").host_exists(),
            "something wrote under obj/borzoi/ although C# project references are off"
        );
        assert!(
            env.identities_incomplete(),
            "an unread C# reference must mark the env incomplete, not vanish"
        );

        // The same resolution, edge by edge: the C# edge is unread, naming the
        // option that would read it.
        let refs = sema.reference_set_for_project(
            &app,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            refs.edges,
            [ReferenceEdge {
                project: crate::observed::host_fs::canonicalize(tmp.path().join("Cs/Cs.csproj"))
                    .unwrap(),
                outcome: ReferenceOutcome::Unread {
                    why: CSHARP_REFERENCES_DISABLED,
                    transient: false,
                },
            }]
        );
        assert!(CSHARP_REFERENCES_DISABLED.contains("`enableCSharpProjectReferences`"));
        assert!(!refs.retryable);
        assert!(!sentinel.host_exists());
        assert!(!tmp.path().join("obj/borzoi").host_exists());
    }

    /// The control that keeps the test above from passing vacuously: on the
    /// same fixture with the option on, the env build does run the stub
    /// `dotnet` as the sidecar host. The stub exits without answering, so the
    /// edge is unread for that reason instead.
    #[cfg(unix)]
    #[test]
    fn csharp_references_opted_into_drive_the_sidecar() {
        let (tmp, app, dotnet_root, sentinel) = csharp_ref_fixture_with_stub_dotnet();

        let mut sema = SemanticState::new();
        sema.set_csharp_project_references_enabled(true);
        let env = sema.assembly_env_for_project(
            &app,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );

        assert!(
            sentinel.host_exists(),
            "with C# project references on, the env build must start the sidecar"
        );
        assert!(env.identities_incomplete());
        let refs = sema.reference_set_for_project(
            &app,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert_eq!(
            refs.edges,
            [ReferenceEdge {
                project: crate::observed::host_fs::canonicalize(tmp.path().join("Cs/Cs.csproj"))
                    .unwrap(),
                outcome: ReferenceOutcome::Unread {
                    why: "the sidecar produced no metadata assembly",
                    transient: false,
                },
            }]
        );
    }

    /// The env cache is not keyed on the option, so changing it must drop the
    /// envs built under the old setting rather than serve them.
    #[cfg(unix)]
    #[test]
    fn changing_the_csharp_references_option_drops_cached_envs() {
        let (_tmp, app, dotnet_root, sentinel) = csharp_ref_fixture_with_stub_dotnet();

        let mut sema = SemanticState::new();
        let off = sema.assembly_env_for_project(
            &app,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(!sentinel.host_exists());

        sema.set_csharp_project_references_enabled(true);
        let on = sema.assembly_env_for_project(
            &app,
            Some(&dotnet_root),
            &ServedTfm::NoneDeclared,
            &Workspace::default(),
        );
        assert!(
            !Arc::ptr_eq(&off, &on),
            "the env built with C# references off was served after turning them on"
        );
        assert!(sentinel.host_exists(), "the rebuilt env drives the sidecar");
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
        fn doc_id_dialect(&self) -> Result<borzoi_assembly::doc_id::DocIdDialect, ImportError> {
            Ok(borzoi_assembly::doc_id::DocIdDialect::Metadata)
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
            union_cases: UnionCases::Unknowable,
            union_case_makers: Vec::new(),
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
            metadata_name: Some(name.to_string()),
            marker_compiled_name: None,
            is_no_equality: false,
            is_no_comparison: false,
            is_structural_equality: false,
            is_structural_comparison: false,
            is_allow_null_literal: false,
            is_no_pia_local: false,
            obsolete: None,
            compiler_message: None,
            experimental: None,
            default_member: None,
            compiler_feature_required: vec![],
            source_name: None,
            custom_attrs: vec![],
            abbreviation_target: None,
            definition_range: None,
            member_measures: borzoi_assembly::MemberMeasures::NotFSharp,
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
        // A path that doesn't exist: the read fails and the DLL is skipped. We
        // learned nothing about it, so it is an unknown identity — the grade
        // that poisons the env, never the "not managed" exemption.
        let missing = Path::new("/definitely/not/a/real/path/nope.dll");
        assert_eq!(
            enumerate_dll_type_defs(missing, &borzoi_msbuild::observed::Observer::new()).err(),
            Some(SkipCause::UnknownIdentity)
        );

        // A file full of garbage bytes: the parser rejects it and the DLL is
        // skipped — no panic escapes. Garbage is refused before the PE headers
        // can declare anything, so it too is an unknown identity.
        let tmp = TempDir::new().unwrap();
        let junk = tmp.path().join("junk.dll");
        write(&junk, "this is not a PE file at all");
        assert_eq!(
            enumerate_dll_type_defs(&junk, &borzoi_msbuild::observed::Observer::new()).err(),
            Some(SkipCause::UnknownIdentity)
        );
    }

    /// A PE32 image whose optional header declares all 16 data directories and
    /// leaves every one of them zero — the shape of a native DLL. `Machine` and
    /// `Characteristics` are left zero: nothing before the CLI data directory
    /// reads them, which is the point (the classification is a fact about the
    /// *declared directory table*, not about the rest of the image).
    fn native_pe_image() -> Vec<u8> {
        const E_LFANEW: usize = 0x40;
        const SIZE_OPTIONAL: u16 = 224; // 96 standard/NT fields + 16 × 8 directories
        let mut image = vec![0u8; E_LFANEW + 4 + 20 + SIZE_OPTIONAL as usize];
        image[0..2].copy_from_slice(b"MZ");
        image[0x3C..0x40].copy_from_slice(&(E_LFANEW as u32).to_le_bytes());
        image[E_LFANEW..E_LFANEW + 4].copy_from_slice(b"PE\0\0");
        let coff = E_LFANEW + 4;
        // NumberOfSections stays 0, so the section walk after the directory read
        // has nothing to do.
        image[coff + 16..coff + 18].copy_from_slice(&SIZE_OPTIONAL.to_le_bytes());
        let optional = coff + 20;
        image[optional..optional + 2].copy_from_slice(&0x10Bu16.to_le_bytes()); // PE32
        // NumberOfRvaAndSizes: the last field before the directory array, at
        // offset 92 of a PE32 optional header.
        image[optional + 92..optional + 96].copy_from_slice(&16u32.to_le_bytes());
        image
    }

    /// A file that declares no CLI data directory is not an assembly we failed
    /// to read — it is not an assembly. Nothing about the env becomes uncertain,
    /// so it must not mark the identity set incomplete; a DLL we genuinely could
    /// not read must.
    ///
    /// This is what makes the wholesale "defer every assembly reading under an
    /// incomplete projection" policy affordable: native files ride along in
    /// NuGet packages and land in reference position routinely, and counting
    /// them as losses would take a project's whole referenced-assembly surface
    /// dark for a `git2-*.dll`.
    #[test]
    fn a_native_pe_in_reference_position_leaves_the_identity_set_complete() {
        let tmp = TempDir::new().unwrap();

        let native = tmp.path().join("native.dll");
        std::fs::write(&native, native_pe_image()).unwrap();
        // Pin the premise: these bytes reach the classification under test via
        // the projector's positive determination, not via some earlier refusal
        // that would make the assertions below pass for the wrong reason.
        assert_eq!(
            Ecma335Assembly::parse(&crate::observed::host_fs::read(&native).unwrap()).err(),
            Some(ImportError::NotAManagedAssembly)
        );
        assert_eq!(
            enumerate_dll_type_defs(&native, &borzoi_msbuild::observed::Observer::new()).err(),
            Some(SkipCause::NotAManagedAssembly)
        );

        let (env, _) = build_env_from_dll_paths(
            [native.as_path()].into_iter(),
            &AssemblyCache::disabled(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert!(
            !env.identities_incomplete(),
            "a file with no CLI directory has no manifest name to hide"
        );

        // The contrast case: a *managed* image we could not read leaves its
        // identity unknown, and the env says so.
        let unreadable = tmp.path().join("unreadable.dll");
        std::fs::write(&unreadable, b"MZ but nothing else").unwrap();
        let (env, _) = build_env_from_dll_paths(
            [native.as_path(), unreadable.as_path()].into_iter(),
            &AssemblyCache::disabled(),
            &borzoi_msbuild::observed::Observer::new(),
        );
        assert!(
            env.identities_incomplete(),
            "a DLL we could not read could be any assembly, including a referenced CCU"
        );
    }

    // ---- resolved_project_for ----

    #[test]
    fn resolved_project_resolves_qualified_cross_file_ref() {
        // Two-file project: `Other` references `Shared.foo`. The fold should
        // make the cross-file qualified reference resolve to an `Item` whose
        // `item_def` points back at file 0's `foo` binder.
        use borzoi_cst::syntax::TextRange;
        use borzoi_sema::Resolution;

        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs"]));
        let a_src = "module Shared\nlet foo = 1\n";
        let b_src = "module Other\nlet bar = Shared.foo\n";
        write(&tmp.path().join("A.fs"), a_src);
        write(&tmp.path().join("B.fs"), b_src);

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let (resolved, env) = sema
            .resolved_project_and_env_for(&proj, &mut ws, &Vfs::new())
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
        assert_eq!(def.spelling(&env), "foo");
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
            sema.resolved_project_for(&proj, &mut ws, &Vfs::new())
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
            .resolved_project_for(&proj, &mut ws, &Vfs::new())
            .expect("first fold");
        let second = sema
            .resolved_project_for(&proj, &mut ws, &Vfs::new())
            .expect("cached fold");
        assert!(
            Arc::ptr_eq(&first, &second),
            "expected the same Arc on a cache hit"
        );

        // An edit forces a re-fold; the new Arc differs.
        let mut docs = Vfs::new();
        docs.insert(
            Uri::from_file_path(&file).unwrap(),
            "let x = 2\n".to_string(),
        );
        let third = sema
            .resolved_project_for(&proj, &mut ws, &docs)
            .expect("re-fold after an edit");
        assert!(
            !Arc::ptr_eq(&first, &third),
            "expected a fresh Arc after an edit"
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

        let mut docs = Vfs::new();
        docs.insert(
            Uri::from_file_path(&file).unwrap(),
            "let buffer = 1\n".to_string(),
        );

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let (resolved, env) = sema
            .resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("resolved project");
        let names: Vec<_> = resolved
            .file(0)
            .exports()
            .iter()
            .map(|e| e.name(&env).to_string())
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
        let a_uri = Uri::from_file_path(tmp.path().join("A.fs")).unwrap();
        let b_uri = Uri::from_file_path(tmp.path().join("B.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs = Vfs::new();
        sema.resolved_project_for(&proj, &mut ws, &docs)
            .expect("warm initial");

        // Body-only edit to B: `A.x` -> `(A.x)`. B's export `y` is byte-identical
        // (same path / ItemId / accessibility), so the fold recomputes only B and
        // reuses A (unchanged prefix) and C (export-neutral suffix).
        docs.insert(b_uri.clone(), "module B\nlet y = (A.x)\n".to_string());
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
        let (warm, warm_env) = sema
            .resolved_project_and_env_for(&proj, &mut ws, &docs)
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
        let (cold, cold_env) = cold_sema
            .resolved_project_and_env_for(&proj, &mut cold_ws, &docs)
            .expect("cold final");
        assert_eq!(
            cold_sema.incremental_fold_count(),
            0,
            "a first fold has no prev base and must be cold"
        );

        // Each fold holds ids of its own env's name table, so the warm one is
        // compared in the cold env's ids.
        assert_eq!(
            rekeyed(&warm, &warm_env, &cold_env),
            *cold,
            "the incremental fold must equal a cold fold of the same buffer state"
        );
    }

    /// The inference fold, driven through the real caches, reuses every file
    /// but the edited one after an edit that changes no export's type, and
    /// infers the suffix afresh after one that changes an export's type and
    /// not its name, which the resolver still reuses the suffix across. Each
    /// result equals a cold fold's.
    #[test]
    fn incremental_inference_reuses_what_the_cutoff_allows() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs", "C.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
        write(&tmp.path().join("B.fs"), "module B\nlet y = A.x\n");
        write(&tmp.path().join("C.fs"), "module C\nlet z = B.y\n");
        let a_uri = Uri::from_file_path(tmp.path().join("A.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs = Vfs::new();
        sema.inferred_prefix_for(&proj, 2, &mut ws, &docs)
            .expect("warm initial");
        assert_eq!(
            sema.last_inference_reused_files(),
            0,
            "a first fold is cold"
        );

        // A cold fold builds a fresh env, so the warm fold is compared in the
        // cold one's table.
        let assert_matches_cold =
            |warm: &(Arc<ResolvedProject>, Arc<AssemblyEnv>, Arc<InferredProject>), docs: &Vfs| {
                let mut ws = Workspace::default();
                let mut sema = SemanticState::new();
                let (_, cold_env, cold) = sema
                    .inferred_prefix_for(&proj, 2, &mut ws, docs)
                    .expect("cold fold");
                assert_eq!(rekeyed_inference(&warm.2, &warm.1, &cold_env), *cold);
            };

        docs.insert(a_uri.clone(), "module A\nlet x = (1)\n".to_string());
        let warm = sema
            .inferred_prefix_for(&proj, 2, &mut ws, &docs)
            .expect("fold after a body edit");
        assert_eq!(
            sema.last_fold_reused_files(),
            2,
            "the resolver reuses B and C"
        );
        assert_eq!(
            sema.last_inference_reused_files(),
            2,
            "A's contribution is unchanged, so B and C are reused"
        );
        assert_matches_cold(&warm, &docs);

        docs.insert(a_uri, "module A\nlet x = \"s\"\n".to_string());
        let warm = sema
            .inferred_prefix_for(&proj, 2, &mut ws, &docs)
            .expect("fold after a type-changing edit");
        assert_eq!(
            sema.last_fold_reused_files(),
            2,
            "the resolver reuses B and C"
        );
        assert_eq!(
            sema.last_inference_reused_files(),
            0,
            "A's `x` changed type, so B and C are inferred afresh"
        );
        assert_matches_cold(&warm, &docs);
    }

    /// A stale-suffix guard: removing a cross-file export must invalidate the
    /// downstream file's reuse. `B` references `A.x`; after `A.x` is removed the
    /// incrementally-refolded `B` must no longer resolve the reference to a
    /// (now-nonexistent) `Item` — proving the suffix was recomputed, not reused.
    #[test]
    fn incremental_fold_drops_stale_cross_file_reference() {
        use borzoi_cst::syntax::TextRange;
        use borzoi_sema::Resolution;

        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
        let b_src = "module B\nlet y = A.x\n";
        write(&tmp.path().join("B.fs"), b_src);
        let a_uri = Uri::from_file_path(tmp.path().join("A.fs")).unwrap();

        let range = {
            let start = b_src.find("A.x").unwrap();
            TextRange::new((start as u32).into(), ((start + "A.x".len()) as u32).into())
        };

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let warm0 = sema
            .resolved_project_for(&proj, &mut ws, &Vfs::new())
            .expect("warm");
        assert!(
            matches!(
                warm0.file(1).resolution_at(range),
                Some(Resolution::Item(_))
            ),
            "precondition: A.x resolves cross-file to an Item"
        );

        // Remove A.x through a buffer overlay and re-fold incrementally.
        let mut docs = Vfs::new();
        docs.insert(a_uri, "module A\nlet other = 1\n".to_string());
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

    /// An env whose name table a fold leaves past the cap is dropped: the next
    /// fold, after an edit that would otherwise reuse the previous one, runs
    /// cold against a freshly built env. Under a cap the table stays within,
    /// the same edits fold incrementally against one env. The first edit
    /// rearranges names the table already holds, so it grows the table by
    /// paths alone, which the cap counts too.
    #[test]
    fn an_env_past_its_name_cap_is_rebuilt_and_folded_cold() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs", "B.fs"]));
        let a_src = "module A\nmodule B =\n    module C =\n        let x = 1\n";
        write(&tmp.path().join("A.fs"), a_src);
        let b_src = "module D\nlet y = A.B.C.x\n";
        write(&tmp.path().join("B.fs"), b_src);
        let b_uri = Uri::from_file_path(tmp.path().join("B.fs")).unwrap();
        let b_edit = "module D\nmodule A =\n    module D =\n        let z = 2\nlet y = (A.B.C.x)\n";
        let buffers =
            |text: &str| -> Vfs { std::iter::once((b_uri.clone(), text.to_string())).collect() };

        // Fold, edit and fold, edit back and fold: the entries after the
        // first two folds, whether the last two shared an env, and how many
        // folds were incremental.
        let fold_thrice = |cap: usize| {
            let mut ws = Workspace::default();
            let mut sema = SemanticState::new();
            sema.name_table_cap = NameTableCap(cap);
            let (_, first) = sema
                .resolved_project_and_env_for(&proj, &mut ws, &Vfs::new())
                .expect("first fold");
            let first_entries = first.interned_count();
            let (_, second) = sema
                .resolved_project_and_env_for(&proj, &mut ws, &buffers(b_edit))
                .expect("second fold");
            let second_entries = second.interned_count();
            let (_, third) = sema
                .resolved_project_and_env_for(&proj, &mut ws, &buffers(b_src))
                .expect("third fold");
            (
                first_entries,
                second_entries,
                Arc::ptr_eq(&second, &third),
                sema.incremental_fold_count(),
            )
        };

        let (first, second, same_env, incremental) = fold_thrice(1 << 20);
        assert!(
            second > first,
            "the edit's new module paths grow the table: {first} -> {second}"
        );
        assert!(same_env, "within the cap the env is kept");
        assert_eq!(
            incremental, 2,
            "within the cap each edit folds incrementally"
        );

        // The first fold stays within the cap, and the edit's paths, over
        // names the table already held, take it past: the second fold still
        // reuses the first, and the third starts again.
        let (_, _, same_env, incremental) = fold_thrice(first);
        assert!(!same_env, "past the cap the env is rebuilt");
        assert_eq!(incremental, 1, "past the cap the next edit folds cold");
    }

    /// Inference publishes into the env's type table, which the cap bounds
    /// with the name table: an inference that takes the tables past the cap
    /// drops the env, so the next request builds a fresh one, where under a
    /// cap the tables stay within the env is kept.
    #[test]
    fn an_env_an_inference_takes_past_its_cap_is_rebuilt() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["A.fs"]));
        write(&tmp.path().join("A.fs"), "module A\nlet x = (1, \"s\")\n");

        // Resolve, then infer, then resolve again: the entries after the
        // resolution and after the inference, and whether the two
        // resolutions shared an env.
        let resolve_infer_resolve = |cap: usize| {
            let mut ws = Workspace::default();
            let mut sema = SemanticState::new();
            sema.name_table_cap = NameTableCap(cap);
            let docs = Vfs::new();
            let (_, before) = sema
                .resolved_project_and_env_for(&proj, &mut ws, &docs)
                .expect("resolution");
            let resolved_entries = before.interned_count();
            let (_, inferred_env, _) = sema
                .inferred_prefix_for(&proj, 0, &mut ws, &docs)
                .expect("inference");
            let inferred_entries = inferred_env.interned_count();
            let (_, after) = sema
                .resolved_project_and_env_for(&proj, &mut ws, &docs)
                .expect("resolution again");
            (
                resolved_entries,
                inferred_entries,
                Arc::ptr_eq(&before, &after),
            )
        };

        let (resolved, inferred, same_env) = resolve_infer_resolve(1 << 20);
        assert!(
            inferred > resolved,
            "the inference's published types grow the tables: {resolved} -> {inferred}"
        );
        assert!(same_env, "within the cap the env is kept");

        let (_, _, same_env) = resolve_infer_resolve(resolved);
        assert!(
            !same_env,
            "past the cap after an inference the env is rebuilt"
        );
    }

    /// A dropped env (here, every env: [`SemanticState::drop_every_env`])
    /// drops the incremental base, so the next fold is cold — the env it would reuse against is
    /// gone. Guards the `Arc::ptr_eq` env precondition at the cache-lifecycle
    /// level.
    #[test]
    fn a_dropped_env_forces_a_cold_refold() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        write(&proj, &fsproj(&["Lib.fs"]));
        write(&tmp.path().join("Lib.fs"), "module L\nlet x = 1\n");

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let docs = Vfs::new();
        sema.resolved_project_for(&proj, &mut ws, &docs)
            .expect("initial");
        assert_eq!(
            sema.prev_resolved.len(),
            1,
            "the initial fold stores an incremental base"
        );

        sema.drop_every_env();
        assert_eq!(
            sema.prev_resolved.len(),
            0,
            "dropping the env clears the incremental base"
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
        let docs = Vfs::new();

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
        let (p1, warm_env) = sema
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
        let (full, cold_env) = cold_sema
            .resolved_project_and_env_for(&proj, &mut cold_ws, &docs)
            .expect("full fold");
        assert_eq!(full.len(), 3);
        // Each fold holds ids of its own env's name table, so the prefix is
        // compared in the cold env's ids.
        let p1 = rekeyed(&p1, &warm_env, &cold_env);
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
        assert_eq!(rekeyed(&p0b, &warm_env, &cold_env).file(0), full.file(0));

        // The full method folds the whole project and still agrees.
        let (whole, _) = sema
            .resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("whole project");
        assert_eq!(whole.len(), 3);
        assert_eq!(rekeyed(&whole, &warm_env, &cold_env).file(2), full.file(2));
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
        let c_uri = Uri::from_file_path(tmp.path().join("C.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs = Vfs::new();

        // Warm: request file 2 (C) — folds prefix [0, 1, 2], not D.
        let (p, _) = sema
            .resolved_prefix_and_env_for(&proj, 2, &mut ws, &docs)
            .expect("warm prefix up to 2");
        assert_eq!(p.len(), 3, "prefix up to C excludes D");

        // Body-only edit to C, then request C's prefix again.
        docs.insert(c_uri, "module C\nlet z = (B.y)\n".to_string());
        let ((p2, _), trace) = crate::test_trace::capture(|| {
            sema.resolved_prefix_and_env_for(&proj, 2, &mut ws, &docs)
                .expect("prefix up to 2 after edit")
        });
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
        let lookup = trace.only_span("semantic.project_lookup");
        assert_eq!(lookup.field("cache_hit"), Some("false"));
        assert_eq!(lookup.field("requested_files"), Some("3"));
        assert_eq!(lookup.i64_field("requested_files"), Some(3));
        assert_eq!(lookup.i64_field("cached_files"), Some(0));
        let fold = trace.only_span("resolve_project");
        assert_eq!(fold.field("incremental"), Some("true"));
        assert_eq!(fold.field("reused_files"), Some("2"));
        assert_eq!(fold.i64_field("files"), Some(3));
        assert_eq!(fold.i64_field("reused_files"), Some(2));
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
        let a_uri = Uri::from_file_path(tmp.path().join("A.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs = Vfs::new();

        // Warm the FULL project (as a hover/references request would).
        let (full, _) = sema
            .resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("warm full");
        assert_eq!(full.len(), 4);

        // Body-only edit to A (file 0), then a *prefix* token request for A —
        // folds only file 0, but must keep the len-4 base.
        docs.insert(a_uri, "module A\nlet x = (1)\n".to_string());
        let (p, _) = sema
            .resolved_prefix_and_env_for(&proj, 0, &mut ws, &docs)
            .expect("prefix up to 0 after edit");
        assert_eq!(p.len(), 1, "the token request folds only the edited file");

        // Now a full-project request: it must reuse B, C, D (unchanged across the
        // export-neutral edit) and re-resolve only A — not re-fold the suffix.
        let (full2, warm_env) = sema
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
        let (cold, cold_env) = cold_sema
            .resolved_project_and_env_for(&proj, &mut cold_ws, &docs)
            .expect("cold");
        assert_eq!(rekeyed(&full2, &warm_env, &cold_env), *cold);
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
        let a_uri = Uri::from_file_path(tmp.path().join("A.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs = Vfs::new();

        // Warm the whole project (as an open editor would): a cold fold, no refresh.
        sema.resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("warm");
        assert!(!sema.take_wants_refresh(), "a cold fold flags no refresh");

        // Edit A, with its text sync, then a token request for A itself (an
        // incremental fold), which finds the fold stale at the buffer's new
        // text: the sync announced it, so the fold owes nothing more.
        docs.insert(a_uri, "module A\nlet x = 1\nlet extra = 2\n".to_string());
        sema.owe_token_refresh();
        assert!(sema.take_wants_refresh(), "the edit flags a refresh");
        assert!(!sema.take_wants_refresh(), "take clears the flag");
        sema.resolved_prefix_and_env_for(&proj, 0, &mut ws, &docs)
            .expect("prefix A after edit");
        assert!(
            !sema.take_wants_refresh(),
            "the fold owes no second refresh for the change its sync announced"
        );

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
        let a_uri = Uri::from_file_path(tmp.path().join("A.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs = Vfs::new();
        sema.resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("warm");
        assert!(
            !sema.take_wants_refresh(),
            "the cold warm fold flags nothing"
        );

        // Body-only edit to A: `1` -> `(1)`, and its text sync. Still flags a
        // refresh (conservative).
        docs.insert(a_uri, "module A\nlet x = (1)\n".to_string());
        sema.owe_token_refresh();
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
        let docs = Vfs::new();

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
        let a_uri = Uri::from_file_path(tmp.path().join("A.fs")).unwrap();

        let mut ws = Workspace::default();
        let mut sema = SemanticState::new();
        let mut docs = Vfs::new();
        sema.resolved_project_and_env_for(&proj, &mut ws, &docs)
            .expect("warm");
        assert!(!sema.take_wants_refresh());

        // A dropped env takes the incremental base with it, then a text edit.
        sema.drop_every_env();
        assert!(sema.take_wants_refresh(), "a dropped env owes a refresh");
        docs.insert(a_uri, "module A\nlet x = 1\nlet extra = 2\n".to_string());
        sema.owe_token_refresh();
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

        // No fold anywhere — just what the notification path records.
        sema.owe_token_refresh();
        assert!(
            sema.take_wants_refresh(),
            "a fold-less source change still owes a refresh"
        );

        // A moved projects generation / referenced-assembly change likewise.
        let mut ws = Workspace::default();
        sema.sync_with_evaluations(ws.projects_generation());
        ws.note_structural_event();
        sema.sync_with_evaluations(ws.projects_generation());
        assert!(
            sema.take_wants_refresh(),
            "a moved projects generation owes a refresh"
        );
        sema.invalidate_assembly_state();
        assert!(
            sema.take_wants_refresh(),
            "invalidate_assembly_state owes a refresh"
        );
    }

    /// The PDB image cache computes once per DLL stamp and caches a negative
    /// result, recomputes once the DLL is rewritten, and caches nothing for a
    /// DLL it cannot stamp, or whose stamp is racy.
    #[test]
    fn pdb_image_cache_computes_once_per_stamp_and_caches_the_negative() {
        let tmp = TempDir::new().unwrap();
        let dll = tmp.path().join("A.dll");
        let other = tmp.path().join("B.dll");
        write(&dll, "a");
        write(&other, "b");
        let mut s = SemanticState::default();
        let calls = std::cell::Cell::new(0);
        let img: Arc<[u8]> = Arc::from(vec![1u8, 2, 3]);
        let count = |s: &mut SemanticState, dll: &Path, image: Option<Arc<[u8]>>| {
            s.pdb_image(dll, || {
                calls.set(calls.get() + 1);
                image
            })
        };

        // Freshly written, the stamp is racy: nothing is cached.
        count(&mut s, &dll, Some(img.clone()));
        count(&mut s, &dll, Some(img.clone()));
        assert_eq!(calls.get(), 2, "a racy stamp was cached under");

        std::thread::sleep(
            msbuild_observed::STAMP_RACY_WINDOW + std::time::Duration::from_millis(200),
        );
        let first = count(&mut s, &dll, Some(img.clone()));
        assert_eq!(first.as_deref(), Some(&[1u8, 2, 3][..]));
        // A second lookup is served from the cache — `compute` must not run.
        let second = s.pdb_image(&dll, || panic!("cache hit should not recompute"));
        assert_eq!(second.as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(calls.get(), 3);

        // A DLL with no PDB caches the `None` so it isn't re-read each time.
        assert!(count(&mut s, &other, None).is_none());
        assert!(
            s.pdb_image(&other, || panic!("negative result should be cached"))
                .is_none()
        );
        assert_eq!(calls.get(), 4);

        // A rewritten DLL is a new stamp: computed again.
        write(&dll, "a rebuilt dll");
        count(&mut s, &dll, None);
        assert_eq!(calls.get(), 5, "a rewritten DLL was served its old image");

        // A DLL that cannot be stamped is never cached.
        let missing = tmp.path().join("missing.dll");
        count(&mut s, &missing, None);
        count(&mut s, &missing, None);
        assert_eq!(calls.get(), 7);

        // A referenced-assembly watch event clears the cache: the sidecar
        // `.pdb` a DLL names is not in the key.
        assert!(s.pdb_image(&other, || panic!("still cached")).is_none());
        s.invalidate_assembly_state();
        count(&mut s, &other, None);
        assert_eq!(
            calls.get(),
            8,
            "a referenced-assembly event left a cached image"
        );
    }
}
