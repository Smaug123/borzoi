//! `textDocument/definition` — jump from a use to its defining binder.
//!
//! Resolves the cursor's [`Resolution`] against the project's
//! `ResolvedProject` (or a single-file fallback for orphan / partial-project
//! buffers) and emits a [`Location`] pointing at the defining range:
//!
//! - [`Resolution::Local`] → the binder in this file.
//! - [`Resolution::Item`] → the binder in the file that exported it, looked
//!   up through `ResolvedProject::item_def`.
//! - [`Resolution::Member`] (a referenced-assembly *method*) →
//!   `assembly_member_location`: the method's source via its DLL's portable
//!   PDB — *embedded* in the DLL, or a *sidecar* `.pdb` beside it (`pdb_image_for`)
//!   — yielding a `file://` for embedded (or, behind `sourcelink-fetch`, fetched)
//!   source, or the SourceLink URL when fetching is off. A non-method member
//!   still → `Ok(None)` (no source mapping yet).
//! - [`Resolution::Entity`] (a referenced-assembly *type* or *module*) →
//!   `assembly_entity_location`: navigates to the entity's first source-mapped
//!   method through the same PDB machinery, so go-to-definition on `Gen` or
//!   `NonNull` lands at the declaration. `Ok(None)` if the type has no
//!   source-mapped method.
//! - [`Resolution::Deferred`] / [`Resolution::Unresolved`] → `Ok(None)`.
//!   D5 says we say nothing rather than guess.
//!
//! The handler is conservatively *silent* on every failure mode: no cursor
//! resolution, no owning project, no resolvable URL — all map to `None`,
//! never an LSP error envelope.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use borzoi_assembly::Member;
use borzoi_assembly::pdb::embedded_portable_pdb;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, EntityHandle, MemberIndex, ProjectItems, Resolution, infer_file, resolve_file,
};
use lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, Location, Position, Range, Url};

use crate::cst_panic_safe::parse_with_symbols;
use crate::goto_source::{
    DefinitionDocument, DefinitionSource, SourceFetcher, SourcePlan, SourceTarget,
    definition_document_in_pdb, definition_source_in_pdb, entity_definition_document_in_pdb,
    entity_definition_source_in_pdb, plan_source, sidecar_pdb_matches, sidecar_pdb_name,
};
use crate::handlers::{preferred_uri, range_to_lsp, smallest_resolution_at};
use crate::paths::{lexically_normalize, paths_equal};
use crate::position::position_to_offset;
use crate::semantic::{ProjectParses, SemanticState};
use crate::server::State;
use crate::workspace::Workspace;

/// A go-to-definition either resolved to a response now, or deferred because it
/// needs a SourceLink fetch the dispatch shell must perform off the request
/// thread (see [`PendingFetch`]). Keeping the fetch a *value* the handler emits
/// — rather than an effect it performs inline — is what lets the ~1 s network
/// fetch run on a worker while the request loop stays responsive.
pub enum DefinitionOutcome {
    /// Resolved synchronously (`None` = no useful location → `Ok(null)`).
    Ready(Option<GotoDefinitionResponse>),
    /// A cold `Remote` source: the shell either fetches it on the pool (writing
    /// `dest`, replying with that file location) or — with no fetcher/pool, or a
    /// full queue, or a failed fetch — surfaces its URL for the client to open.
    Deferred(PendingFetch),
}

/// A cold SourceLink fetch the dispatch shell must perform: fetch `url`, write it
/// to `dest` (the cache path [`crate::goto_source::plan_source`] already
/// computed), and reply with a file location at `line`/`column`. All-owned, so
/// it crosses to a worker thread (`State` is not `Send`).
#[derive(Debug, Clone)]
pub struct PendingFetch {
    pub url: String,
    pub dest: PathBuf,
    pub line: u32,
    pub column: u32,
}

/// A resolved referenced-assembly definition: an immediately-available location,
/// or a cold remote source the shell must fetch.
enum Located {
    Ready(Location),
    Deferred(PendingFetch),
}

/// Run the goto-definition handler. Resolves synchronously except for a cold
/// referenced-assembly SourceLink source, which is returned as
/// [`DefinitionOutcome::Deferred`] for the shell to fetch off the request loop.
/// Any failure to find a useful location is `Ready(None)` — `Ok(null)` to the
/// client, never an error envelope.
pub fn handle(state: &mut State, params: GotoDefinitionParams) -> DefinitionOutcome {
    let pos = params.text_document_position_params.position;
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .clone();
    let Some(text) = state.docs.get(&uri).cloned() else {
        return DefinitionOutcome::Ready(None);
    };
    let byte = position_to_offset(&text, pos);

    match project_definition(state, &uri, byte) {
        Some(Located::Deferred(pending)) => DefinitionOutcome::Deferred(pending),
        Some(Located::Ready(loc)) => {
            DefinitionOutcome::Ready(Some(GotoDefinitionResponse::Scalar(loc)))
        }
        // Fallback: parse the buffer in isolation. Resolves locals/params even
        // when the file is orphan (no project), the project failed to evaluate,
        // or the file's path didn't match any Compile item.
        None => DefinitionOutcome::Ready(
            single_file_definition(state, &uri, &text, byte).map(GotoDefinitionResponse::Scalar),
        ),
    }
}

/// Build the go-to-definition response for a fetched source file: the worker
/// thread calls this after writing `dest`, reusing the exact 1-based→0-based
/// position conversion the synchronous path uses (`location_for_target`).
pub fn location_for_pending(dest: &Path, line: u32, column: u32) -> Option<GotoDefinitionResponse> {
    let loc = location_for_target(SourceTarget::File {
        path: dest.to_path_buf(),
        line,
        column,
    })?;
    Some(GotoDefinitionResponse::Scalar(loc))
}

/// The default SourceLink fetcher for the dispatch pool: the real network
/// fetcher under `sourcelink-fetch`, else `None`. The handler always emits a
/// `Deferred` description for a cold `Remote` source; with no fetcher the shell
/// builds no pool and surfaces the URL instead of fetching (see
/// [`crate::server::run_with_fetcher`]).
#[cfg(feature = "sourcelink-fetch")]
pub fn default_source_fetcher() -> Option<Arc<dyn SourceFetcher>> {
    Some(Arc::new(MinreqFetcher))
}

/// See the feature-gated variant above; this is the default (no network).
#[cfg(not(feature = "sourcelink-fetch"))]
pub fn default_source_fetcher() -> Option<Arc<dyn SourceFetcher>> {
    None
}

/// Build a go-to-definition response pointing at a SourceLink **URL** — the
/// fallback the dispatch shell surfaces when no fetcher is configured, the queue
/// is full, or the fetch failed — for the client to open the source directly.
///
/// Only **HTTPS** URLs are surfaced: the URL comes from the untrusted PDB, so we
/// apply the same scheme policy the fetcher does (`is_https`) rather than ask the
/// client to open an arbitrary-scheme URI. A rejected scheme yields `None` (→
/// `null`), matching the pre-async behaviour where the fetcher refused it.
pub fn url_location(url: &str, line: u32, column: u32) -> Option<GotoDefinitionResponse> {
    if !is_https(url) {
        return None;
    }
    let loc = location_for_target(SourceTarget::Url {
        url: url.to_string(),
        line,
        column,
    })?;
    Some(GotoDefinitionResponse::Scalar(loc))
}

/// Turn a resolved [`DefinitionSource`] into a [`Located`]: plan where the source
/// is (no network), then either it is ready (embedded / cache-hit) or — for a
/// cold `Remote` source — a [`Located::Deferred`] *description*. The handler
/// never decides whether to fetch: the dispatch shell, which alone knows whether
/// a fetcher/pool is configured, either fetches the description or surfaces its
/// URL (so the decision can't disagree with the actual runtime fetcher).
fn locate_source(source: DefinitionSource) -> Option<Located> {
    match plan_source_with_fallback(source, &source_cache_dir(), &ephemeral_source_dir())? {
        SourcePlan::Ready(target) => location_for_target(target).map(Located::Ready),
        SourcePlan::NeedsFetch {
            url,
            dest,
            line,
            column,
        } => Some(Located::Deferred(PendingFetch {
            url,
            dest,
            line,
            column,
        })),
    }
}

/// Project-level lookup: full `ResolvedProject` against the project the URI
/// belongs to. Returns `None` if the URI isn't in any project, the project
/// failed to evaluate, the URI isn't a Compile item, or the cursor has no
/// resolvable resolution. A referenced-assembly source not yet cached resolves
/// to [`Located::Deferred`] for the shell to fetch.
fn project_definition(state: &mut State, uri: &Url, byte: usize) -> Option<Located> {
    let path = uri.to_file_path().ok()?;
    let project = state.workspace.owning_project(&path)?;
    let State {
        semantic,
        workspace,
        docs,
        ..
    } = state;
    let resolved = semantic.resolved_project_for(&project, workspace, docs)?;
    // Clone the parses (cheap: `ImplFile`s are rowan handles, `texts` is
    // `Arc<str>`) so the `&mut semantic` borrow can drop and we can keep
    // using the data while the `Arc<ResolvedProject>` is in hand.
    let parses = semantic
        .parses_for_project(&project, workspace, docs)?
        .clone();

    let file_idx = find_file_idx(&parses, &path)?;
    let file = resolved.file(file_idx);
    // The resolver's answer first; where it leaves a member-name as
    // `Deferred(QualifiedAccess)` (a `recv.Name` inference resolves), fall back to
    // inference's member-resolution side-table (Stage 3.3b), so go-to-definition on
    // the member name behaves like a resolver-resolved `Resolution::Member`.
    let res = match smallest_resolution_at(file, byte) {
        Some(res) if !matches!(res, Resolution::Deferred(_)) => res,
        deferred_or_none => {
            let dotnet_root = workspace.dotnet_root_for_project(&project);
            let target_framework = workspace.served_tfm_for_project(&project);
            let env = semantic.assembly_env_for_project(
                &project,
                dotnet_root.as_deref(),
                &target_framework,
                workspace,
            );
            let inferred = {
                let _span = tracing::info_span!("infer_file").entered();
                infer_file(&parses.files[file_idx], file, &env)
            };
            match smallest_member_resolution_at(&inferred, byte) {
                Some(member_res) => member_res,
                // No member resolution: honour the resolver's (deferred / absent)
                // verdict — D5 silence, unless it was a concrete resolution.
                None => deferred_or_none?,
            }
        }
    };
    // A referenced-assembly member or entity resolves to source via its DLL's
    // portable PDB (the `resolved`/`parses` borrows above are an `Arc` + owned
    // clone, so `semantic`/`workspace` are free to borrow again here).
    if let Resolution::Member { parent, idx } = res {
        return assembly_member_location(semantic, workspace, &project, parent, idx);
    }
    if let Resolution::Entity(handle) = res {
        return assembly_entity_location(semantic, workspace, &project, handle);
    }
    location_for_resolution(&parses, file_idx, &resolved, res, uri, docs).map(Located::Ready)
}

/// The smallest inference-recorded member resolution containing `byte` — the
/// go-to-definition analogue of hover's
/// [`smallest_member_resolution_with_range`](crate::handlers::smallest_member_resolution_with_range),
/// dropping the range (definition navigates, it doesn't scope a tooltip).
fn smallest_member_resolution_at(
    inferred: &borzoi_sema::InferredFile,
    byte: usize,
) -> Option<Resolution> {
    crate::handlers::smallest_member_resolution_with_range(inferred, byte).map(|(_, res)| res)
}

/// Locate the source of a referenced-assembly **method**: obtain its owning
/// DLL's portable PDB ([`pdb_image_for`] — embedded or sidecar), compute where
/// the source is ([`definition_source_in_pdb`](crate::goto_source::definition_source_in_pdb)),
/// and materialise it to a [`Location`] — a `file://` for embedded (and, behind the
/// `sourcelink-fetch` feature, fetched) source, or the source URL itself when
/// fetching is off. `None` for a non-method member, missing provenance, or any
/// read/PDB failure (D5: say nothing rather than guess).
fn assembly_member_location(
    semantic: &mut SemanticState,
    workspace: &mut Workspace,
    project: &Path,
    parent: EntityHandle,
    idx: MemberIndex,
) -> Option<Located> {
    let dotnet_root = workspace.dotnet_root_for_project(project);
    let target_framework = workspace.served_tfm_for_project(project);
    let env = semantic.assembly_env_for_project(
        project,
        dotnet_root.as_deref(),
        &target_framework,
        workspace,
    );

    // Only methods carry a `MethodDef` token + PDB sequence point to navigate to.
    let token = match env.member_at(parent, idx) {
        Member::Method(m) => m.metadata_token,
        _ => return None,
    };
    let dll = env.assembly_path(parent)?;
    let pdb_image = semantic.pdb_image(dll, || {
        let bytes = std::fs::read(dll).ok()?;
        Some(Arc::<[u8]>::from(pdb_image_for(dll, &bytes)?))
    })?;
    let source = definition_source_in_pdb(&pdb_image, token).ok()??;
    locate_source(source)
}

/// The portable-PDB *metadata image* for a referenced DLL: its **embedded** PDB
/// when the build embeds one (FSharp.Core), else a **sidecar** `.pdb` next to
/// the DLL — the common NuGet shape (FsUnit, most packages ship a separate
/// `.pdb`). The sidecar is accepted only when its
/// [`PortablePdb::id`](borzoi_assembly::pdb::PortablePdb::id) matches the
/// DLL's CodeView id, so a stale `.pdb` left beside a rebuilt assembly is
/// rejected (D5: say nothing rather than send the cursor to a wrong line).
///
/// `None` when there is neither an embedded PDB nor a matching sidecar (or any
/// read/parse failure) — the IO shell around the pure
/// [`crate::goto_source::sidecar_pdb_name`] /
/// [`crate::goto_source::sidecar_pdb_matches`] cores. `pub` so the
/// embedded-vs-sidecar selection can be exercised on a real DLL on disk.
pub fn pdb_image_for(dll_path: &Path, dll_bytes: &[u8]) -> Option<Vec<u8>> {
    if let Some(image) = embedded_portable_pdb(dll_bytes).ok()? {
        return Some(image);
    }
    // No embedded PDB: follow the CodeView pointer to a sidecar `.pdb` beside
    // the DLL (only its file name is trusted — the recorded path is the absent
    // build machine's), and accept it only if its id matches.
    let sidecar = dll_path.parent()?.join(sidecar_pdb_name(dll_bytes)?);
    let bytes = std::fs::read(sidecar).ok()?;
    sidecar_pdb_matches(dll_bytes, &bytes).then_some(bytes)
}

/// *Where* a referenced-assembly **method** is defined — its document + 1-based
/// line — read from the owning DLL's PDB (embedded or sidecar), even when the
/// source text itself can't be obtained (no embedded source, no SourceLink, as
/// with FsUnit). This is the "say where it is rather than show nothing" path:
/// hover renders it, so a symbol whose source the LSP can't open still reports
/// its origin. `None` for a non-method member or any read/PDB failure.
///
/// Goes through `semantic`'s PDB-image cache (keyed by DLL path) — hover fires on
/// every mouse-over, so reading and re-extracting the whole owning DLL each time
/// would be wasteful; the cache is shared with the go-to-definition path.
pub fn member_definition_document(
    semantic: &mut SemanticState,
    env: &AssemblyEnv,
    parent: EntityHandle,
    idx: MemberIndex,
) -> Option<DefinitionDocument> {
    let token = match env.member_at(parent, idx) {
        Member::Method(m) => m.metadata_token,
        _ => return None,
    };
    let dll = env.assembly_path(parent)?;
    let pdb_image = semantic.pdb_image(dll, || {
        let bytes = std::fs::read(dll).ok()?;
        Some(Arc::<[u8]>::from(pdb_image_for(dll, &bytes)?))
    })?;
    definition_document_in_pdb(&pdb_image, token).ok()?
}

/// *Where* a referenced-assembly **entity** (type/module) is defined — the
/// document + line of its first source-mapped method — the entity counterpart of
/// [`member_definition_document`]. `None` for an entity with no source-mapped
/// method or any read/PDB failure. Shares the PDB-image cache via `semantic`.
pub fn entity_definition_document(
    semantic: &mut SemanticState,
    env: &AssemblyEnv,
    handle: EntityHandle,
) -> Option<DefinitionDocument> {
    let tokens = &env.entity(handle).method_def_tokens;
    if tokens.is_empty() {
        return None;
    }
    let dll = env.assembly_path(handle)?;
    let pdb_image = semantic.pdb_image(dll, || {
        let bytes = std::fs::read(dll).ok()?;
        Some(Arc::<[u8]>::from(pdb_image_for(dll, &bytes)?))
    })?;
    entity_definition_document_in_pdb(&pdb_image, tokens).ok()?
}

/// Locate the source of a referenced-assembly **type or module**: navigate to
/// one of its methods (the lowest-rid one with a sequence point) via the owning
/// DLL's portable PDB, then materialise that to a [`Location`] — exactly as
/// [`assembly_member_location`] does for a single method, but over the entity's
/// *physical* method-token set ([`borzoi_sema::AssemblyEnv::entity`]'s
/// `method_def_tokens`). That physical set, not the resolution-oriented
/// `members`, is what lets a union/record whose only source-mapped method is a
/// projection-dropped accessor still be navigated. `None` for an entity with no
/// source-mapped method, missing provenance, or any read/PDB failure (D5: say
/// nothing rather than guess).
fn assembly_entity_location(
    semantic: &mut SemanticState,
    workspace: &mut Workspace,
    project: &Path,
    handle: EntityHandle,
) -> Option<Located> {
    let dotnet_root = workspace.dotnet_root_for_project(project);
    let target_framework = workspace.served_tfm_for_project(project);
    let env = semantic.assembly_env_for_project(
        project,
        dotnet_root.as_deref(),
        &target_framework,
        workspace,
    );

    let tokens = &env.entity(handle).method_def_tokens;
    if tokens.is_empty() {
        return None; // No methods → no PDB sequence point to navigate to.
    }
    let dll = env.assembly_path(handle)?;
    let pdb_image = semantic.pdb_image(dll, || {
        let bytes = std::fs::read(dll).ok()?;
        Some(Arc::<[u8]>::from(pdb_image_for(dll, &bytes)?))
    })?;
    let source = entity_definition_source_in_pdb(&pdb_image, tokens).ok()??;
    locate_source(source)
}

/// Sub-namespace leaf for materialised sources under the shared cache root — a
/// sibling of the assembly-projection cache's `entities`.
const SOURCES_LEAF: &str = "sources";

/// The persistent on-disk cache directory for materialised referenced-assembly
/// sources, governed by the same `BORZOI_LSP_CACHE_DIR` knob as the
/// assembly-projection cache (a sibling `sources` sub-namespace under the shared
/// cache root). Persisting here — rather than in the OS temp dir — means a
/// fetched SourceLink source survives a reboot or temp-cleaner sweep instead of
/// being re-downloaded.
fn source_cache_dir() -> PathBuf {
    resolve_source_cache_dir(
        std::env::var_os(crate::assembly_cache::CACHE_DIR_ENV),
        crate::assembly_cache::default_cache_root(),
    )
}

/// Pure resolver for [`source_cache_dir`] (the env value and shared default root
/// injected, so it is unit-testable without touching process env):
/// - empty `BORZOI_LSP_CACHE_DIR` (caching disabled) → an ephemeral temp
///   dir;
/// - an explicit root → its `sources` sub-namespace;
/// - unset → the shared default root's `sources` leaf, or an ephemeral temp dir
///   when there is no rootable location (no `XDG_CACHE_HOME`/`HOME`).
///
/// Unlike the assembly cache, this never resolves to "disabled": the editor
/// opens the cached file directly, so *some* writable directory must exist.
/// "Disabled" therefore degrades to an ephemeral temp dir (the pre-existing
/// behaviour), keeping go-to-definition working while simply not persisting
/// across reboots.
fn resolve_source_cache_dir(
    cache_dir_env: Option<std::ffi::OsString>,
    default_root: Option<PathBuf>,
) -> PathBuf {
    match cache_dir_env {
        Some(v) if v.is_empty() => ephemeral_source_dir(),
        Some(v) => PathBuf::from(v).join(SOURCES_LEAF),
        None => default_root
            .map(|root| root.join(SOURCES_LEAF))
            .unwrap_or_else(ephemeral_source_dir),
    }
}

/// The ephemeral (OS temp) fallback directory for materialised sources — used
/// when caching is disabled or unrooted, and as the retry location when a write
/// to the persistent cache fails (e.g. a read-only cache dir), so an embedded
/// source (which has no URL to surface) can always be opened.
fn ephemeral_source_dir() -> PathBuf {
    std::env::temp_dir().join("borzoi").join(SOURCES_LEAF)
}

/// Plan a source in the persistent cache (`primary`), retrying in `fallback` if
/// planning fails. Only *embedded* source writes during planning (a remote
/// source's fetch/write is deferred to the worker), so this is the guard that a
/// read-only persistent cache dir can't swallow an embedded definition — which,
/// unlike a remote one, has no URL to fall back on. `None` only if both
/// locations fail (e.g. an unwritable temp dir too).
fn plan_source_with_fallback(
    source: DefinitionSource,
    primary: &Path,
    fallback: &Path,
) -> Option<SourcePlan> {
    match plan_source(source.clone(), primary) {
        Ok(plan) => Some(plan),
        Err(_) => plan_source(source, fallback).ok(),
    }
}

/// A [`SourceFetcher`] backed by `minreq` (a small synchronous HTTP client, TLS
/// via rustls). Compiled only with the `sourcelink-fetch` feature;
/// [`default_source_fetcher`] hands it to the dispatch pool.
#[cfg(feature = "sourcelink-fetch")]
struct MinreqFetcher;

/// Hard timeout for a SourceLink fetch. The fetch runs on the single LSP request
/// dispatch loop, so it must be bounded — a slow/stalled server times out into
/// an `Err` (→ go-to-definition yields nothing) rather than blocking every later
/// request indefinitely (correctness/availability: bound the uncertainty).
#[cfg(feature = "sourcelink-fetch")]
const SOURCELINK_FETCH_TIMEOUT_SECS: u64 = 10;

/// Cap on a fetched source body. The URL is PDB-controlled, so a hostile (or
/// broken) endpoint mustn't be able to exhaust memory/disk: the body is read
/// lazily and the fetch fails once it exceeds this. 16 MiB dwarfs any real source
/// file.
#[cfg(feature = "sourcelink-fetch")]
const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;

/// Cap on the response headers / status line `minreq` buffers *before* the body
/// loop — without these a hostile endpoint could exhaust memory in the metadata
/// it reads first. Generous for any legitimate server.
#[cfg(feature = "sourcelink-fetch")]
const MAX_HEADERS_BYTES: usize = 64 * 1024;
#[cfg(feature = "sourcelink-fetch")]
const MAX_STATUS_LINE_BYTES: usize = 4 * 1024;

/// Whether `url`'s scheme is `https://` (case-insensitive). The PDB SourceLink
/// map is untrusted data, so both the fetcher ([`MinreqFetcher`]) and the
/// URL-surfacing fallback ([`url_location`]) refuse any other scheme — we never
/// fetch, nor hand the client to open, an `http://` / `file://` / arbitrary URL.
fn is_https(url: &str) -> bool {
    url.as_bytes()
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case(b"https://"))
}

#[cfg(feature = "sourcelink-fetch")]
impl SourceFetcher for MinreqFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        // The URL comes from the referenced assembly's PDB (untrusted data), so
        // refuse anything but HTTPS — never issue a plaintext request even if the
        // SourceLink map asks for `http://`.
        if !is_https(url) {
            return Err(format!("refusing non-HTTPS SourceLink URL: {url}"));
        }
        // `with_max_redirects(0)`: don't let a redirect downgrade to `http://` or
        // otherwise escape the HTTPS check above (SourceLink raw URLs are served
        // directly, so no legitimate redirect is lost). Stream the body
        // (`send_lazy`) so an oversized response is rejected before it is fully
        // buffered, not after.
        let response = minreq::get(url)
            .with_header("User-Agent", "borzoi")
            .with_timeout(SOURCELINK_FETCH_TIMEOUT_SECS)
            .with_max_redirects(0)
            .with_max_headers_size(MAX_HEADERS_BYTES)
            .with_max_status_line_length(MAX_STATUS_LINE_BYTES)
            .send_lazy()
            .map_err(|e| format!("fetch {url}: {e}"))?;
        if response.status_code != 200 {
            return Err(format!("fetch {url}: HTTP {}", response.status_code));
        }
        let mut body = Vec::new();
        for piece in response {
            let (byte, _) = piece.map_err(|e| format!("fetch {url}: {e}"))?;
            if body.len() >= MAX_SOURCE_BYTES {
                return Err(format!(
                    "fetch {url}: source exceeds the {MAX_SOURCE_BYTES}-byte cap"
                ));
            }
            body.push(byte);
        }
        Ok(body)
    }
}

/// Turn a materialised [`SourceTarget`] into an LSP [`Location`] at its 1-based
/// position (converted to LSP's 0-based, zero-width range).
fn location_for_target(target: SourceTarget) -> Option<Location> {
    let (uri, line, column) = match target {
        SourceTarget::File { path, line, column } => {
            (Url::from_file_path(&path).ok()?, line, column)
        }
        SourceTarget::Url { url, line, column } => (Url::parse(&url).ok()?, line, column),
    };
    let pos = Position {
        line: line.saturating_sub(1),
        character: column.saturating_sub(1),
    };
    Some(Location {
        uri,
        range: Range {
            start: pos,
            end: pos,
        },
    })
}

/// Path → index lookup against `parses.paths`. Both sides are normalised
/// lexically and compared under the platform's filesystem case sensitivity
/// (same rule [`crate::workspace::Workspace::owning_project`] uses), so a
/// `lib.fs` cursor lands on a project's `Lib.fs` member on macOS/Windows.
fn find_file_idx(parses: &ProjectParses, path: &Path) -> Option<usize> {
    let target = lexically_normalize(path);
    parses
        .paths
        .iter()
        .position(|p| paths_equal(&lexically_normalize(p), &target))
}

/// Translate a project-level [`Resolution`] to an LSP [`Location`].
/// `file_idx` is the cursor's file in `parses` — used so a same-file
/// [`Resolution::Local`] preserves the *request* URI rather than
/// reconstructing one from the project's compile-item path (which can differ
/// in casing or contain `..` segments).
fn location_for_resolution(
    parses: &ProjectParses,
    file_idx: usize,
    resolved: &borzoi_sema::ResolvedProject,
    res: Resolution,
    request_uri: &Url,
    docs: &HashMap<Url, String>,
) -> Option<Location> {
    match res {
        Resolution::Local(id) => {
            // Same file as the cursor: use the request URI verbatim. LSP
            // clients key documents by URI string, so reconstructing from
            // the compile item's path (which may differ in casing or contain
            // `..` after a `Link`) would jump to a duplicate / stale URI
            // even though the file is identical.
            let def = resolved.file(file_idx).def(id);
            Some(Location {
                uri: request_uri.clone(),
                range: range_to_lsp(&parses.texts[file_idx], def.range),
            })
        }
        Resolution::Item(_) => {
            let (other_idx, def) = resolved.item_def(res)?;
            // Cross-file: prefer the URI of an already-open buffer pointing
            // at the same path, so a follow-up textDocument/* request keys
            // off the URI the client originally sent. Fall back to
            // constructing a fresh `file://` URI from the path otherwise.
            let uri = preferred_uri(&parses.paths[other_idx], docs)?;
            Some(Location {
                uri,
                range: range_to_lsp(&parses.texts[other_idx], def.range),
            })
        }
        // Unreachable: `project_definition` intercepts referenced-assembly
        // entities and members *before* this function (routing them to
        // `assembly_entity_location` / `assembly_member_location`, which read
        // the DLL's portable-PDB source). The arm is kept only to make the match
        // exhaustive — there is no `file://` def range to translate to here.
        Resolution::Entity(_) | Resolution::Member { .. } => None,
        Resolution::Deferred(_) | Resolution::Unresolved => None,
    }
}

/// Single-file fallback: parse the buffer in isolation and answer for
/// locals / parameters / same-file top-level bindings. Nothing else can be
/// resolved without project context: cross-file `Item`s have no `paths` to
/// translate to, and `Entity`/`Member` can't appear without an
/// `AssemblyEnv`.
fn single_file_definition(
    state: &mut State,
    uri: &Url,
    text: &str,
    byte: usize,
) -> Option<Location> {
    let symbols = state.symbols_for_uri(uri);
    let lang = state.lang_version_for_uri(uri);
    let parse = parse_with_symbols(text, &symbols, lang)?;
    let file = ImplFile::cast(parse.root)?;
    let resolved = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());
    let res = smallest_resolution_at(&resolved, byte)?;
    let def = resolved.resolved_def(res)?;
    Some(Location {
        uri: uri.clone(),
        range: range_to_lsp(text, def.range),
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn source_cache_dir_uses_the_persistent_root_by_default() {
        // Unset env + a shared cache root → the `sources` sub-namespace under it
        // (a sibling of the assembly cache's `entities`), so a fetched source
        // survives a reboot / temp-cleaner sweep instead of living in OS temp.
        let dir = resolve_source_cache_dir(None, Some(PathBuf::from("/home/u/.cache/borzoi")));
        assert_eq!(dir, PathBuf::from("/home/u/.cache/borzoi/sources"));
    }

    #[test]
    fn source_cache_dir_honours_an_explicit_root() {
        // `BORZOI_LSP_CACHE_DIR=/custom/cache` → `/custom/cache/sources`,
        // keeping sources out of the assembly cache's entries under that root.
        let dir = resolve_source_cache_dir(Some(OsString::from("/custom/cache")), None);
        assert_eq!(dir, PathBuf::from("/custom/cache/sources"));
    }

    #[test]
    fn disabled_or_unrooted_falls_back_to_an_ephemeral_dir() {
        // Empty env (caching disabled) ignores any default root and degrades to
        // the ephemeral temp dir; so does unset-with-no-root. A file must exist
        // for the editor to open, so go-to-definition keeps working — the source
        // simply isn't persisted across reboots.
        let disabled =
            resolve_source_cache_dir(Some(OsString::new()), Some(PathBuf::from("/ignored")));
        assert_eq!(disabled, ephemeral_source_dir());
        assert_eq!(resolve_source_cache_dir(None, None), ephemeral_source_dir());
    }

    #[test]
    fn embedded_write_falls_back_when_the_primary_dir_is_unwritable() {
        // Force the primary cache dir to be unwritable *portably* (no chmod, so
        // it holds even as root): put a regular file where the cache path's
        // ancestor must be a directory, so `create_dir_all` under it fails.
        // Embedded source must then still resolve — via the ephemeral fallback —
        // because, unlike a remote source, it has no URL to surface instead.
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"not a dir").unwrap();
        let primary = blocker.join("cache"); // parent is a file ⇒ create_dir_all errors
        let fallback = tmp.path().join("fallback");

        let source = DefinitionSource::Embedded {
            document: r"C:\src\Lib.fs".into(),
            text: "let x = 1\n".into(),
            line: 3,
            column: 5,
        };
        match plan_source_with_fallback(source, &primary, &fallback)
            .expect("embedded source resolves via the fallback dir")
        {
            SourcePlan::Ready(SourceTarget::File { path, line, column }) => {
                assert_eq!((line, column), (3, 5));
                assert!(
                    path.starts_with(&fallback),
                    "written under the fallback dir: {path:?}"
                );
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "let x = 1\n");
            }
            other => panic!("expected Ready(File) via fallback, got {other:?}"),
        }
    }

    #[test]
    fn remote_plans_needsfetch_under_the_primary_dir() {
        // A remote source doesn't write during planning (its fetch/write is
        // deferred to the worker), so it plans under the primary dir
        // unconditionally — the fallback is only for a failed embedded write.
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        let fallback = tmp.path().join("fallback");
        let source = DefinitionSource::Remote {
            document: r"D:\repo\printf.fs".into(),
            url: "https://example.com/printf.fs".into(),
            line: 42,
            column: 7,
        };
        match plan_source_with_fallback(source, &primary, &fallback).unwrap() {
            SourcePlan::NeedsFetch { dest, .. } => assert!(
                dest.starts_with(&primary),
                "remote dest under the primary dir: {dest:?}"
            ),
            other => panic!("expected NeedsFetch under primary, got {other:?}"),
        }
    }

    #[test]
    fn file_target_becomes_a_file_uri_at_zero_based_position() {
        let path = std::env::temp_dir().join("borzoi-test").join("Lib.fs");
        let target = SourceTarget::File {
            path: path.clone(),
            line: 42,
            column: 7,
        };
        let loc = location_for_target(target).expect("file target → location");
        assert_eq!(loc.uri.scheme(), "file");
        assert_eq!(loc.uri.to_file_path().unwrap(), path);
        // 1-based (42, 7) → 0-based (41, 6); zero-width range.
        let expected = Position {
            line: 41,
            character: 6,
        };
        assert_eq!(loc.range.start, expected);
        assert_eq!(loc.range.end, expected);
    }

    #[test]
    fn url_target_becomes_the_url_verbatim() {
        let target = SourceTarget::Url {
            url: "https://example.com/repo/printf.fs".into(),
            line: 1,
            column: 1,
        };
        let loc = location_for_target(target).expect("url target → location");
        assert_eq!(loc.uri.as_str(), "https://example.com/repo/printf.fs");
        assert_eq!(
            loc.range.start,
            Position {
                line: 0,
                character: 0
            }
        );
    }

    /// The fetcher refuses a non-HTTPS SourceLink URL *before* any network I/O —
    /// so this needs neither the network nor a feature-gated network round-trip.
    #[cfg(feature = "sourcelink-fetch")]
    #[test]
    fn minreq_fetcher_refuses_non_https_urls() {
        let err = MinreqFetcher
            .fetch("http://example.com/printf.fs")
            .expect_err("plaintext URL must be refused");
        assert!(err.contains("non-HTTPS"), "got: {err}");
    }

    /// The real `minreq` fetcher actually retrieves source over HTTPS. Network +
    /// feature-only, so it's `#[ignore]`d (CI/the offline sandbox skip it); run
    /// with `cargo test -p borzoi --features sourcelink-fetch -- --ignored`.
    #[cfg(feature = "sourcelink-fetch")]
    #[test]
    #[ignore = "requires network"]
    fn minreq_fetcher_retrieves_source_over_https() {
        // A stable, public raw URL (the F# repo's licence).
        let bytes = MinreqFetcher
            .fetch("https://raw.githubusercontent.com/dotnet/fsharp/main/License.txt")
            .expect("fetch over https");
        assert!(!bytes.is_empty(), "fetched body should be non-empty");
        assert!(
            String::from_utf8_lossy(&bytes).contains("MIT"),
            "the F# License.txt is the MIT licence"
        );
    }
}
