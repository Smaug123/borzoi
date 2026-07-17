//! The LSP request/notification dispatch loop and the [`State`] it threads.
//!
//! Carved out of `main.rs` so the dispatch is reachable from integration tests
//! (which drive an in-memory [`Connection`] rather than stdio). `main.rs`
//! shrinks to the stdio bootstrap (`Connection::stdio()` + `initialize` +
//! [`run`]); everything else lives here.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use borzoi_cst::language_version::LanguageVersion;
use lsp_server::{Connection, ExtractError, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument, Exit,
    Notification as NotificationTrait, PublishDiagnostics, ShowMessage,
};
use lsp_types::request::{
    Completion, DocumentDiagnosticRequest, DocumentSymbolRequest, GotoDefinition, HoverRequest,
    References, RegisterCapability, Request as RequestTrait, SemanticTokensFullRequest, Shutdown,
    WorkspaceDiagnosticRequest, WorkspaceSymbolRequest,
};
use lsp_types::{
    ClientCapabilities, FileChangeType, FileEvent, MessageType, ShowMessageParams, Url,
    WorkspaceFolder,
};

use lsp_types::{
    CompletionOptions, DiagnosticOptions, DiagnosticServerCapabilities,
    DidChangeWatchedFilesRegistrationOptions, FileSystemWatcher, GlobPattern,
    HoverProviderCapability, OneOf, Registration, RegistrationParams, SemanticTokensFullOptions,
    SemanticTokensOptions, SemanticTokensServerCapabilities, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind,
};

use crate::diagnostics::FileDiagnostics;
use crate::goto_source::SourceFetcher;
use crate::handlers::definition::{
    DefinitionOutcome, PendingFetch, default_source_fetcher, location_for_pending, url_location,
};
use crate::publish::PublishState;
use crate::semantic::SemanticState;
use crate::workspace::Workspace;
use crate::{diagnostics, fsproj_diagnostics};

/// Worker threads servicing deferred SourceLink fetches, and the depth of the
/// bounded queue feeding them. Cold fetches are rare (one per source file, then
/// disk-cached), so a small pool suffices; a full queue replies `null` rather
/// than blocking the dispatch loop (saturation degrades to "no result, retry").
const FETCH_WORKERS: usize = 4;
const FETCH_QUEUE_DEPTH: usize = 64;

/// The capabilities this server advertises at `initialize`. Lives here (not
/// in `main.rs`) so integration tests can audit it directly without
/// re-implementing the literal.
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        // Member (`recv.`) completion (Stage 3.3b). `.` is the sole trigger
        // character — completion fires only in a member-access position, so a
        // client that respects trigger characters won't request it elsewhere.
        // `resolve_provider: false` — every item is fully built up front (no
        // `completionItem/resolve` round-trip).
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            resolve_provider: Some(false),
            ..CompletionOptions::default()
        }),
        document_symbol_provider: Some(OneOf::Left(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        // Lexical syntax highlighting via `textDocument/semanticTokens/full`.
        // The legend is owned by the handler so the advertised token-type
        // order and the indices it emits can't drift; `range`/delta are not
        // offered — full-document tokens are enough for eyeballing a file.
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: crate::handlers::semantic_tokens::legend(),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                range: None,
                work_done_progress_options: Default::default(),
            },
        )),
        // Pull diagnostics (`textDocument/diagnostic` + `workspace/diagnostic`).
        // `inter_file_dependencies` is `true` because a `#line N "f"` directive
        // lets an edit in one buffer change `f`'s diagnostics. The `identifier`
        // namespaces our reports in clients that manage several diagnostic
        // sources.
        diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
            identifier: Some("borzoi".to_string()),
            inter_file_dependencies: true,
            workspace_diagnostics: true,
            work_done_progress_options: Default::default(),
        })),
        ..Default::default()
    }
}

/// Mutable server state threaded through every request and notification.
pub struct State {
    pub docs: HashMap<Url, String>,
    /// Per-file preprocessor symbol resolution. Looks up the owning
    /// `.fsproj` on first request for each new project and caches the
    /// evaluated `$(DefineConstants)` (including SDK-supplied defines
    /// when `$DOTNET_ROOT` or a `dotnet` on `$PATH` is available) for
    /// the lifetime of the server. The fsproj-buffer diagnostic path
    /// reads the workspace's [`crate::sdk_discovery::SdkDiscoveryEnv`]
    /// via [`Workspace::env`] so its SDK resolution agrees with the
    /// symbol-resolution one.
    pub workspace: Workspace,
    /// Cross-file `#line` publish bookkeeping: which diagnostics each document
    /// currently contributes to each target URI, so targets that drop out are
    /// cleared and shared targets publish the union (see [`PublishState`]).
    pub publish: PublishState,
    /// Per-project parses and (later) name resolution, layered on top of the
    /// workspace's `.fsproj` cache so editor buffers can overlay disk text.
    /// Invalidated by [`Self::invalidate_owning_project`] on text-sync.
    pub semantic: SemanticState,
    /// Capabilities the client advertised at `initialize`. `None` means we
    /// have not yet received an `initialize` (tests, or pre-handshake).
    /// Handlers read this to negotiate response shapes — most notably
    /// [`Self::supports_hierarchical_document_symbols`].
    client_capabilities: Option<ClientCapabilities>,
    /// Filesystem roots the client opened at `initialize` (from
    /// `workspaceFolders`, else the deprecated `rootUri`). `workspace/diagnostic`
    /// walks these for `.fsproj` files to enumerate the project-wide diagnostic
    /// set. Empty until `initialize` (tests, or a client that opened no folder)
    /// — in which case a workspace pull simply reports nothing.
    workspace_roots: Vec<PathBuf>,
    /// Owning-project paths we've already shown a "Compile set is untrustworthy"
    /// `window/showMessage` for. Dedupes the notice to once per project per
    /// server session, so opening file after file in the same project doesn't
    /// re-toast. See [`warn_compile_uncertainty`].
    warned_uncertain_projects: HashSet<PathBuf>,
}

impl State {
    pub fn new() -> Self {
        Self {
            docs: HashMap::new(),
            workspace: Workspace::new(),
            publish: PublishState::new(),
            semantic: SemanticState::new(),
            client_capabilities: None,
            workspace_roots: Vec::new(),
            warned_uncertain_projects: HashSet::new(),
        }
    }

    /// Record the capabilities the client advertised at `initialize`. Called
    /// from `main.rs` once per server lifetime; tests can call it directly to
    /// opt into specific client behaviours.
    pub fn set_client_capabilities(&mut self, caps: ClientCapabilities) {
        self.client_capabilities = Some(caps);
    }

    /// Record the workspace roots the client opened at `initialize`. Called
    /// from `main.rs`; tests set them directly to drive `workspace/diagnostic`.
    pub fn set_workspace_roots(&mut self, roots: Vec<PathBuf>) {
        self.workspace_roots = roots;
    }

    /// The workspace roots `workspace/diagnostic` enumerates under.
    pub fn workspace_roots(&self) -> &[PathBuf] {
        &self.workspace_roots
    }

    /// Whether the client advertised support for the hierarchical
    /// `DocumentSymbol[]` response shape. When `false` (including when the
    /// client never advertised any documentSymbol capability — the spec
    /// default), `textDocument/documentSymbol` must return the flat
    /// `SymbolInformation[]` shape instead.
    pub fn supports_hierarchical_document_symbols(&self) -> bool {
        self.client_capabilities
            .as_ref()
            .and_then(|c| c.text_document.as_ref())
            .and_then(|td| td.document_symbol.as_ref())
            .and_then(|ds| ds.hierarchical_document_symbol_support)
            .unwrap_or(false)
    }

    /// The active preprocessor symbol set for a buffer. Mirrors
    /// `symbols_for_uri` in the diagnostic path — `.fs`/`.fsi`/`.fsx`
    /// buffers under an evaluable `.fsproj` pick up its `DefineConstants`;
    /// orphan or non-`file:` buffers fall back to the implicit set for their
    /// kind. Mutating because [`Workspace::symbols_for`] populates its project
    /// cache on first lookup.
    pub fn symbols_for_uri(&mut self, uri: &Url) -> HashSet<String> {
        match uri.to_file_path() {
            Ok(path) => self.workspace.symbols_for(&path),
            Err(()) => implicit_symbols_for_uri(uri),
        }
    }

    /// The F# language version for a buffer — the companion to
    /// [`Self::symbols_for_uri`] for the parse calls. A `file:` buffer takes its
    /// owning project's version ([`Workspace::lang_version_for`]); a non-`file:`
    /// buffer gets [`LanguageVersion::Preview`] (no project context, so don't
    /// guess-flag). For navigation handlers the parse tree does not depend on
    /// this — the language gate is diagnostic-only — but threading the real
    /// version keeps every parse call consistent.
    pub fn lang_version_for_uri(&mut self, uri: &Url) -> LanguageVersion {
        match uri.to_file_path() {
            Ok(path) => self.workspace.lang_version_for(&path),
            Err(()) => LanguageVersion::Preview,
        }
    }

    /// Drop the semantic caches for **every** cached project that lists this
    /// URI's path in its compile-order parses. Wired into every text-sync
    /// notification (`didOpen`/`didChange`/`didClose`) so the next semantic
    /// query re-folds against the up-to-date buffer overlay.
    ///
    /// Two design choices worth pinning:
    ///
    /// - **Skips non-source URIs** (anything outside `.fs`/`.fsi`/`.fsx`): a
    ///   `.fsproj` text-sync can't change a sema cache (those caches are
    ///   keyed on source files), and a `.fsproj` URI under text-sync would
    ///   not name a source file in any project anyway.
    /// - **Walks every cached project**, not just the one
    ///   [`Workspace::owning_project`] would pick: a shared source file can
    ///   sit in multiple projects' `<Compile>` lists (the link case), and
    ///   `owning_project` only returns one. Invalidating just that one
    ///   leaves stale parses in every other cached project that sees the
    ///   file. See [`crate::semantic::SemanticState::invalidate_file`].
    pub fn invalidate_owning_project(&mut self, uri: &Url) {
        if !matches!(path_extension(uri).as_deref(), Some("fs" | "fsi" | "fsx")) {
            return;
        }
        let Ok(path) = uri.to_file_path() else {
            return;
        };
        self.semantic.invalidate_file(&path);
    }

    /// Apply a batch of `workspace/didChangeWatchedFiles` changes to the caches
    /// and return the open-document URIs whose diagnostics the shell must
    /// republish — empty unless a **structural** file changed.
    ///
    /// - A `Structural` change (a `.fsproj` / `Directory.Build.*` /
    ///   `global.json` / `project.assets.json`) invalidates the *whole*
    ///   project-evaluation cache and *all* semantic caches (broad-but-correct;
    ///   see `docs/completed/file-watch-invalidation-plan.md` W2), and requests a
    ///   republish of every open buffer — its `DefineConstants` may have moved.
    /// - A `Source` change invalidates just the semantic caches that list the
    ///   file (no republish: an unopened source edit can't change an open
    ///   buffer's lexer/parser diagnostics).
    /// - An `AssemblyInput` change (a `.dll` rewritten by a sibling rebuild or
    ///   restore, a `.cs`/`.csproj` edit feeding the C# sidecar) drops the
    ///   referenced-assembly caches
    ///   ([`crate::semantic::SemanticState::invalidate_assembly_state`]) while
    ///   keeping project evaluation; no republish (binaries can't change an
    ///   open buffer's lexer/parser diagnostics either).
    ///
    /// Pure of IO beyond cache mutation, so it is testable without a
    /// [`Connection`]; the shell does the actual publishing.
    pub fn apply_watched_changes(&mut self, changes: &[FileEvent]) -> Vec<Url> {
        let mut structural = false;
        let mut assembly_input = false;
        for change in changes {
            match classify_change(&change.uri, change.typ) {
                ChangeClass::Structural => structural = true,
                ChangeClass::Source => {
                    if let Ok(path) = change.uri.to_file_path() {
                        self.semantic.invalidate_file(&path);
                    }
                }
                ChangeClass::AssemblyInput => assembly_input = true,
                ChangeClass::Ignored => {}
            }
        }
        if structural {
            // Subsumes the assembly-input invalidation: `invalidate_all`
            // clears the assembly state too.
            self.workspace.invalidate_projects();
            self.semantic.invalidate_all();
            self.docs.keys().cloned().collect()
        } else {
            if assembly_input {
                self.semantic.invalidate_assembly_state();
            }
            Vec::new()
        }
    }

    /// The `client/registerCapability` params that install our
    /// `workspace/didChangeWatchedFiles` file watchers, or `None` when the
    /// client didn't advertise dynamic registration for it. In that case the
    /// client must watch via its own static configuration (or the notification
    /// never arrives) — [`Self::apply_watched_changes`] is correct either way.
    /// Sent once at the start of [`run`].
    pub fn watched_files_registration(&self) -> Option<RegistrationParams> {
        let supported = self
            .client_capabilities
            .as_ref()
            .and_then(|c| c.workspace.as_ref())
            .and_then(|w| w.did_change_watched_files.as_ref())
            .and_then(|d| d.dynamic_registration)
            .unwrap_or(false);
        supported.then(|| RegistrationParams {
            registrations: vec![watched_files_registration_entry()],
        })
    }
}

/// The single `workspace/didChangeWatchedFiles` [`Registration`] this server
/// installs: three glob watchers covering the project-structure, F# source,
/// and referenced-assembly-input files [`State::apply_watched_changes`] reacts
/// to. `kind: None` keeps the LSP default (create | change | delete), which is
/// exactly what we want — a source create/delete drives glob-membership
/// invalidation, a content change drives the rest. The `{}` groups keep it to
/// three watchers rather than one per extension.
fn watched_files_registration_entry() -> Registration {
    let watcher = |glob: &str| FileSystemWatcher {
        glob_pattern: GlobPattern::String(glob.to_string()),
        kind: None,
    };
    let options = DidChangeWatchedFilesRegistrationOptions {
        watchers: vec![
            watcher("**/*.{fsproj,props,targets,fs,fsi,fsx}"),
            watcher(
                "**/{global.json,project.assets.json,packages.lock.json,nuget.config,NuGet.Config}",
            ),
            // Referenced-assembly inputs: sibling rebuild / restore outputs,
            // and the C# sources the sidecar compiles.
            watcher("**/*.{dll,cs,csproj}"),
        ],
    };
    Registration {
        id: "borzoi/watched-files".to_string(),
        method: DidChangeWatchedFiles::METHOD.to_string(),
        register_options: Some(
            serde_json::to_value(options).expect("watcher registration options serialise"),
        ),
    }
}

/// What a `workspace/didChangeWatchedFiles` change affects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeClass {
    /// A project-structure file: the project evaluation and every semantic
    /// cache built from it may be stale.
    Structural,
    /// An F# source file: only the semantic caches that list it are stale.
    Source,
    /// An input to the referenced-assembly layer: a `.dll` (a sibling
    /// project's rebuilt output, a package-cache write), or a `.cs`/`.csproj`
    /// whose change alters what the C# sidecar would emit. The assembly envs
    /// (and everything folded against them) may be stale; project evaluation
    /// (defines, Compile order) is not.
    AssemblyInput,
    /// Anything we don't react to.
    Ignored,
}

/// Classify a changed file for `workspace/didChangeWatchedFiles`. Pure (path
/// text + change type only); filename / extension matching is case-insensitive,
/// mirroring [`path_extension`]. `global.json` / `project.assets.json` are
/// matched by full name (their `.json` extension is otherwise `Ignored`);
/// `Directory.Build.{props,targets}` fall through to the `.props` / `.targets`
/// extension arm. A source file's create/delete is `Structural` (it moves a
/// glob-expanded `<Compile>` set); a source content change is `Source`.
fn classify_change(uri: &Url, typ: FileChangeType) -> ChangeClass {
    let name = uri
        .path()
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    // `packages.lock.json` is an input to the on-demand restore decision
    // (`crate::restore` declines when one exists); `nuget.config` (any casing —
    // `name` is lowercased) still steers restore via its `fallbackPackageFolders`
    // even under our source/cache overrides. Both must invalidate the assembly
    // env, so treat them structurally like the assets file they sit beside.
    if name == "global.json"
        || name == "project.assets.json"
        || name == "packages.lock.json"
        || name == "nuget.config"
    {
        return ChangeClass::Structural;
    }
    match Path::new(&name).extension().and_then(OsStr::to_str) {
        Some("fsproj" | "props" | "targets") => ChangeClass::Structural,
        // A source file's creation or deletion changes a project's
        // glob-expanded `<Compile>` set (`Include="*.fs"`), so it is
        // structural; only a content change leaves the Compile set intact.
        Some("fs" | "fsi" | "fsx") => {
            if typ == FileChangeType::CREATED || typ == FileChangeType::DELETED {
                ChangeClass::Structural
            } else {
                ChangeClass::Source
            }
        }
        // A `.csproj`'s *content* only feeds the C# sidecar (F# project
        // evaluation never reads it), but its *existence* is checked by an
        // open `.fsproj`'s `<ProjectReference>` diagnostics
        // (`fsproj_diagnostics::reference_problem`), so create/delete must
        // take the structural path and republish — mirroring the `.fs` arm.
        Some("csproj") => {
            if typ == FileChangeType::CREATED || typ == FileChangeType::DELETED {
                ChangeClass::Structural
            } else {
                ChangeClass::AssemblyInput
            }
        }
        // Referenced-assembly inputs, any change type: a created/deleted DLL
        // moves the located-output / under-resolution boundary just as a
        // rewrite does, and a `.cs` create/delete changes the sidecar's
        // compile set — neither is consulted by any open buffer's
        // diagnostics, so no republish is needed.
        Some("dll" | "cs") => ChangeClass::AssemblyInput,
        _ => ChangeClass::Ignored,
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

/// The dispatch loop with the production SourceLink fetcher
/// ([`default_source_fetcher`] — present under `sourcelink-fetch`, else `None`).
/// See [`run_with_fetcher`] for the loop itself.
pub fn run(connection: Connection, state: State) -> Result<(), Box<dyn Error + Send + Sync>> {
    run_with_fetcher(connection, state, default_source_fetcher())
}

/// The dispatch loop: pull messages off the connection and route them to the
/// request / notification handlers. Returns cleanly when the peer sends
/// `shutdown` then `exit`, or hangs up.
///
/// `state` is owned for the lifetime of the loop. `main.rs` constructs it
/// (and stamps it with the client's capabilities) before calling in; tests
/// drive their own state directly through the handlers rather than this
/// loop.
///
/// Shutdown is handled inline (rather than via `Connection::handle_shutdown`)
/// because we send an outstanding `client/registerCapability` request: a late
/// response to it can land in the `shutdown`→`exit` window, and
/// `handle_shutdown` rejects any `Message::Response` there as unexpected. The
/// inline loop just ignores responses until `exit` arrives.
///
/// `fetcher` is the SourceLink fetcher for the deferred-fetch pool — the seam
/// tests use to inject a fake (or barrier) fetcher and exercise deferral without
/// a network. `fetcher = None` (the no-`sourcelink-fetch` default) builds no
/// pool, and go-to-definition never defers, so behaviour is identical to before.
pub fn run_with_fetcher(
    connection: Connection,
    mut state: State,
    fetcher: Option<Arc<dyn SourceFetcher>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Ask the client to watch the project-structure / source files we react to
    // (only when it supports dynamic registration). Fire-and-forget: the
    // client's response to this request is ignored below.
    if let Some(params) = state.watched_files_registration() {
        send_request::<RegisterCapability>(&connection, params);
    }
    // The deferred-SourceLink-fetch worker pool, built only when a fetcher is
    // configured — so the default no-`sourcelink-fetch` build (and every
    // fetch-free test) spawns zero threads. Dropped on every return path below,
    // which closes the queue and joins the workers (see `FetchPool::drop`).
    let pool = fetcher
        .map(|fetcher| FetchPool::new(&connection, fetcher, FETCH_WORKERS, FETCH_QUEUE_DEPTH));
    // Once the client has sent `shutdown`, the LSP lifecycle forbids servicing
    // further requests/notifications (only `exit` is honoured), so we stop
    // dispatching and don't mutate caches or publish. We track this ourselves
    // rather than using `Connection::handle_shutdown` because the latter rejects
    // the (possibly late) `client/registerCapability` response that arrives in
    // the shutdown→exit window.
    let mut shutting_down = false;
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if req.method == Shutdown::METHOD {
                    shutting_down = true;
                    // Acknowledge with a `null` result; the spec then expects an
                    // `exit` notification, which ends the loop below.
                    connection
                        .sender
                        .send(Message::Response(ok_response(req.id, &())))?;
                } else if shutting_down {
                    // Requests after `shutdown` are invalid per the lifecycle.
                    connection.sender.send(Message::Response(err_response(
                        req.id,
                        lsp_server::ErrorCode::InvalidRequest,
                        "received a request after shutdown".to_string(),
                    )))?;
                } else {
                    match handle_request(&mut state, req) {
                        // Synchronous request: reply immediately, as before.
                        Dispatch::Reply(response) => {
                            connection.sender.send(Message::Response(response))?;
                        }
                        // Cold SourceLink fetch: hand to the pool, which replies
                        // out-of-band when it completes. The loop never blocks on
                        // the network.
                        Dispatch::Defer { id, pending } => {
                            dispatch_fetch(&connection, pool.as_ref(), id, pending)?;
                        }
                    }
                }
            }
            Message::Notification(not) => {
                if not.method == Exit::METHOD {
                    return Ok(());
                }
                // Drop every other notification once shutting down — no cache
                // mutation or diagnostic publishing after `shutdown`.
                if !shutting_down {
                    handle_notification(&mut state, &connection, not);
                }
            }
            Message::Response(_) => {
                // The only request we send is the `client/registerCapability`
                // above; its response carries nothing we act on — and, crucially,
                // a late one must not abort an in-progress shutdown.
            }
        }
    }

    Ok(())
}

/// Enqueue a deferred SourceLink fetch on the pool. When there is no pool (the
/// no-`sourcelink-fetch` build, or a `run_with_fetcher(None)`) or the bounded
/// queue is full, fall back to **surfacing the SourceLink URL** for the client
/// to open — never blocking the dispatch loop, and never replying `null` when a
/// usable URL exists. Whether to fetch is the shell's call (it owns the pool),
/// not the handler's: the handler always emits the `Deferred` description and
/// this decides, so the two can't disagree with the actual runtime fetcher.
fn dispatch_fetch(
    connection: &Connection,
    pool: Option<&FetchPool>,
    id: RequestId,
    pending: PendingFetch,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Try the pool; a successful submit means a worker will reply out-of-band.
    let rejected = match pool {
        Some(pool) => match pool.submit(FetchJob { id, pending }) {
            Ok(()) => return Ok(()),
            Err(job) => job,
        },
        None => FetchJob { id, pending },
    };
    let location = url_location(
        &rejected.pending.url,
        rejected.pending.line,
        rejected.pending.column,
    );
    connection
        .sender
        .send(Message::Response(ok_response(rejected.id, &location)))?;
    Ok(())
}

/// A deferred SourceLink fetch dispatched to the pool.
#[derive(Debug)]
struct FetchJob {
    id: RequestId,
    pending: PendingFetch,
}

/// A small fixed pool of worker threads performing SourceLink fetches off the
/// dispatch loop. Each worker owns a `Message` sender clone and the shared
/// fetcher; it fetches, writes the cache, and sends the definition response
/// itself. Jobs arrive on a bounded channel; a shared `Mutex<Receiver>`
/// distributes them (the classic shared-queue thread pool).
struct FetchPool {
    /// `Option` only so [`Drop`] can close the channel (drop the sender) before
    /// joining the workers.
    tx: Option<SyncSender<FetchJob>>,
    /// Latched on shutdown so workers discard *queued* jobs (finishing only
    /// their in-flight fetch) instead of draining the whole backlog before they
    /// can exit — otherwise a full queue of slow fetches would stall shutdown
    /// for many timeout periods.
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl FetchPool {
    fn new(
        connection: &Connection,
        fetcher: Arc<dyn SourceFetcher>,
        workers: usize,
        capacity: usize,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<FetchJob>(capacity);
        let rx = Arc::new(Mutex::new(rx));
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let workers = (0..workers)
            .map(|_| {
                let rx = Arc::clone(&rx);
                let sender = connection.sender.clone();
                let fetcher = Arc::clone(&fetcher);
                let shutdown = Arc::clone(&shutdown);
                std::thread::spawn(move || {
                    loop {
                        // Hold the lock only across `recv`; the classic shared-
                        // queue handoff (one worker waits in `recv` at a time,
                        // the rest block on the lock, so a job goes to whoever is
                        // waiting and processing happens lock-free).
                        let job = {
                            let guard = rx.lock().unwrap_or_else(|e| e.into_inner());
                            guard.recv()
                        };
                        let Ok(job) = job else {
                            // Sender dropped (pool shutting down) → exit.
                            return;
                        };
                        // Shutting down: drop this (and, as each worker hits the
                        // same check, every other) queued job rather than run its
                        // ~1 s fetch, so `join` waits only on in-flight work.
                        if shutdown.load(std::sync::atomic::Ordering::Acquire) {
                            return;
                        }
                        let response = perform_fetch(&job, fetcher.as_ref());
                        // Re-check shutdown: if `exit` was processed while this
                        // fetch was in flight, drop the reply rather than emit
                        // JSON-RPC output after `exit`.
                        if shutdown.load(std::sync::atomic::Ordering::Acquire) {
                            return;
                        }
                        // A hung-up connection just means the server is gone;
                        // mirror the existing `_ = send` fire-and-forget pattern.
                        let _ = sender.send(Message::Response(response));
                    }
                })
            })
            .collect();
        FetchPool {
            tx: Some(tx),
            shutdown,
            workers,
        }
    }

    /// Enqueue a job, handing it back if the bounded queue is full (or the
    /// channel is closed) so the caller can surface the URL instead of blocking.
    fn submit(&self, job: FetchJob) -> Result<(), FetchJob> {
        match &self.tx {
            Some(tx) => tx.try_send(job).map_err(|e| match e {
                TrySendError::Full(job) | TrySendError::Disconnected(job) => job,
            }),
            None => Err(job),
        }
    }
}

impl Drop for FetchPool {
    fn drop(&mut self) {
        // Latch shutdown (workers discard queued jobs), close the channel (idle
        // workers' `recv` returns `Err`), then join — bounding shutdown to at
        // most one in-flight fetch per worker, not the whole queued backlog.
        // Joining (not detaching) keeps tests from leaking threads across runs.
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        self.tx.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Perform one deferred fetch and build its response: fetch the source, write it
/// to the cache, and produce the definition location — falling back to the
/// SourceLink **URL** on any failure (fetch/write error), so a failed fetch
/// still gives the client something openable rather than nothing.
fn perform_fetch(job: &FetchJob, fetcher: &dyn SourceFetcher) -> Response {
    let location = fetch_location(&job.pending, fetcher)
        .or_else(|| url_location(&job.pending.url, job.pending.line, job.pending.column));
    ok_response(job.id.clone(), &location)
}

fn fetch_location(
    pending: &PendingFetch,
    fetcher: &dyn SourceFetcher,
) -> Option<lsp_types::GotoDefinitionResponse> {
    let bytes = fetcher.fetch(&pending.url).ok()?;
    crate::goto_source::write_if_absent(&pending.dest, &bytes).ok()?;
    location_for_pending(&pending.dest, pending.line, pending.column)
}

/// One dispatched request's outcome: a response to send now, or a deferred
/// go-to-definition whose SourceLink fetch the shell performs on the fetch pool,
/// replying out-of-band when it completes. Responses are correlated by id, so
/// out-of-order delivery (a later request answered before the deferred one) is
/// spec-legal and expected.
pub enum Dispatch {
    Reply(Response),
    Defer {
        id: RequestId,
        pending: PendingFetch,
    },
}

/// Route a request: go-to-definition may defer (a cold SourceLink fetch, ~1 s of
/// network the dispatch loop must not block on); every other request replies
/// synchronously.
pub fn handle_request(state: &mut State, req: Request) -> Dispatch {
    let _span = tracing::info_span!("lsp.request", method = %req.method).entered();
    if req.method == GotoDefinition::METHOD {
        let id = req.id.clone();
        return match extract::<GotoDefinition>(req) {
            Ok((id, params)) => match crate::handlers::definition::handle(state, params) {
                DefinitionOutcome::Ready(resp) => Dispatch::Reply(ok_response(id, &resp)),
                DefinitionOutcome::Deferred(pending) => Dispatch::Defer { id, pending },
            },
            Err(err) => Dispatch::Reply(err_response(
                id,
                lsp_server::ErrorCode::InvalidParams,
                format!("definition params: {err:?}"),
            )),
        };
    }
    Dispatch::Reply(handle_request_sync(state, req))
}

/// The synchronous request handlers — everything except a *deferred*
/// go-to-definition (which `handle_request` intercepts above). Always produces a
/// response to send immediately.
fn handle_request_sync(state: &mut State, req: Request) -> Response {
    let id = req.id.clone();
    match req.method.as_str() {
        HoverRequest::METHOD => match extract::<HoverRequest>(req) {
            Ok((id, params)) => {
                let resp = crate::handlers::hover::handle(state, params);
                ok_response(id, &resp)
            }
            Err(err) => err_response(
                id,
                lsp_server::ErrorCode::InvalidParams,
                format!("hover params: {err:?}"),
            ),
        },
        Completion::METHOD => match extract::<Completion>(req) {
            Ok((id, params)) => {
                let resp = crate::handlers::completion::handle(state, params);
                ok_response(id, &resp)
            }
            Err(err) => err_response(
                id,
                lsp_server::ErrorCode::InvalidParams,
                format!("completion params: {err:?}"),
            ),
        },
        DocumentSymbolRequest::METHOD => match extract::<DocumentSymbolRequest>(req) {
            Ok((id, params)) => {
                let resp = crate::handlers::document_symbol::handle(state, params);
                ok_response(id, &resp)
            }
            Err(err) => err_response(
                id,
                lsp_server::ErrorCode::InvalidParams,
                format!("documentSymbol params: {err:?}"),
            ),
        },
        References::METHOD => match extract::<References>(req) {
            Ok((id, params)) => {
                let resp = crate::handlers::references::handle(state, params);
                ok_response(id, &resp)
            }
            Err(err) => err_response(
                id,
                lsp_server::ErrorCode::InvalidParams,
                format!("references params: {err:?}"),
            ),
        },
        WorkspaceSymbolRequest::METHOD => match extract::<WorkspaceSymbolRequest>(req) {
            Ok((id, params)) => {
                let resp = crate::handlers::workspace_symbol::handle(state, params);
                ok_response(id, &resp)
            }
            Err(err) => err_response(
                id,
                lsp_server::ErrorCode::InvalidParams,
                format!("workspaceSymbol params: {err:?}"),
            ),
        },
        SemanticTokensFullRequest::METHOD => match extract::<SemanticTokensFullRequest>(req) {
            Ok((id, params)) => {
                let resp = crate::handlers::semantic_tokens::handle(state, params);
                ok_response(id, &resp)
            }
            Err(err) => err_response(
                id,
                lsp_server::ErrorCode::InvalidParams,
                format!("semanticTokens params: {err:?}"),
            ),
        },
        DocumentDiagnosticRequest::METHOD => match extract::<DocumentDiagnosticRequest>(req) {
            Ok((id, params)) => {
                let resp = crate::handlers::diagnostic::handle(state, params);
                ok_response(id, &resp)
            }
            Err(err) => err_response(
                id,
                lsp_server::ErrorCode::InvalidParams,
                format!("diagnostic params: {err:?}"),
            ),
        },
        WorkspaceDiagnosticRequest::METHOD => match extract::<WorkspaceDiagnosticRequest>(req) {
            Ok((id, params)) => {
                let resp = crate::handlers::workspace_diagnostic::handle(state, params);
                ok_response(id, &resp)
            }
            Err(err) => err_response(
                id,
                lsp_server::ErrorCode::InvalidParams,
                format!("workspace diagnostic params: {err:?}"),
            ),
        },
        method => err_response(
            id,
            lsp_server::ErrorCode::MethodNotFound,
            format!("unhandled request: {method}"),
        ),
    }
}

pub fn handle_notification(state: &mut State, conn: &Connection, not: Notification) {
    let _span = tracing::info_span!("lsp.notification", method = %not.method).entered();
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            if let Ok(params) = extract_notification::<DidOpenTextDocument>(not) {
                let uri = params.text_document.uri;
                state.docs.insert(uri.clone(), params.text_document.text);
                state.invalidate_owning_project(&uri);
                publish_diagnostics(conn, state, &uri);
                warn_compile_uncertainty(conn, state, &uri);
            }
        }
        DidChangeTextDocument::METHOD => {
            if let Ok(params) = extract_notification::<DidChangeTextDocument>(not) {
                // FULL sync: exactly one change holding the full document text.
                if let Some(change) = params.content_changes.into_iter().next_back() {
                    let uri = params.text_document.uri;
                    state.docs.insert(uri.clone(), change.text);
                    state.invalidate_owning_project(&uri);
                    publish_diagnostics(conn, state, &uri);
                }
            }
        }
        DidCloseTextDocument::METHOD => {
            if let Ok(params) = extract_notification::<DidCloseTextDocument>(not) {
                let uri = params.text_document.uri;
                state.docs.remove(&uri);
                state.invalidate_owning_project(&uri);
                // Clear this document's own diagnostics and any it had
                // relocated onto other files via `#line` directives.
                for params in state.publish.plan_close(&uri) {
                    send_notification::<PublishDiagnostics>(conn, params);
                }
            }
        }
        DidChangeWatchedFiles::METHOD => {
            if let Ok(params) = extract_notification::<DidChangeWatchedFiles>(not) {
                // Invalidate the affected caches, then republish every open
                // buffer the structural change may have moved (their
                // `DefineConstants` are recomputed from the now-fresh project).
                for uri in state.apply_watched_changes(&params.changes) {
                    publish_diagnostics(conn, state, &uri);
                }
            }
        }
        _ => {}
    }
}

fn publish_diagnostics(conn: &Connection, state: &mut State, uri: &Url) {
    let _span = tracing::info_span!("publish_diagnostics", uri = %uri).entered();
    let Some(text) = state.docs.get(uri).cloned() else {
        return;
    };
    let Some(groups) = grouped_for_uri(uri, &text, &mut state.workspace) else {
        return;
    };
    // The pure planner turns the per-file partition into the exact set of
    // notifications — the document's own set plus any cross-file targets it
    // feeds or has stopped feeding — keeping the per-URI publish state correct.
    for params in state.publish.plan(uri, groups) {
        send_notification::<PublishDiagnostics>(conn, params);
    }
}

/// Show a one-time `window/showMessage` when the source file's owning project
/// has a Compile item gated on a condition we couldn't evaluate — the
/// correctness carve-out from the msbuild evaluator
/// ([`borzoi_msbuild::CompileConditionUncertainty`]).
///
/// This is the user-facing half of the `items_uncertain` gate: when a Compile
/// item's inclusion is undecidable, [`crate::semantic`] conservatively falls
/// back to single-file resolution (so go-to-definition into referenced
/// assemblies, cross-file resolution, etc. go quiet for this project). Without
/// a message that looks like a silent failure; with one, the user learns *why*
/// and what to fix. Deduped per owning project for the session via
/// [`State::warned_uncertain_projects`] so it fires at most once, not on every
/// `.fs` open. Only `.fs`/`.fsi`/`.fsx` buffers under an evaluable project can
/// trigger it; everything else returns early.
fn warn_compile_uncertainty(conn: &Connection, state: &mut State, uri: &Url) {
    if !matches!(path_extension(uri).as_deref(), Some("fs" | "fsi" | "fsx")) {
        return;
    }
    let Ok(path) = uri.to_file_path() else {
        return;
    };
    let Some(project) = state.workspace.owning_project(&path) else {
        return;
    };
    if state.warned_uncertain_projects.contains(&project) {
        return;
    }
    // Build the message inside a scope so the immutable `workspace` borrow ends
    // before we touch `warned_uncertain_projects` and send. `None`/empty means
    // there's nothing to surface — leave the project *unmarked* so a later
    // re-evaluation that does turn up an uncertainty can still warn.
    let message = {
        let Some(parsed) = state.workspace.project(&project) else {
            return;
        };
        if parsed.compile_condition_uncertainties.is_empty() {
            return;
        }
        compile_uncertainty_message(&project, &parsed.compile_condition_uncertainties)
    };
    state.warned_uncertain_projects.insert(project);
    send_notification::<ShowMessage>(
        conn,
        ShowMessageParams {
            typ: MessageType::WARNING,
            message,
        },
    );
}

/// Render the editor message for a project whose Compile set we couldn't trust
/// because of [`borzoi_msbuild::CompileConditionUncertainty`]s. Pure
/// (no IO) so it's unit-testable; the caller does the IO and dedup.
fn compile_uncertainty_message(
    project: &std::path::Path,
    uncertainties: &[borzoi_msbuild::CompileConditionUncertainty],
) -> String {
    use borzoi_msbuild::CompileConditionReason;

    let name = project
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| project.display().to_string());

    // Collect the distinct undefined property names (first-seen order) and note
    // whether any condition was outright unmodeled.
    let mut props: Vec<&str> = Vec::new();
    let mut any_unsupported = false;
    for u in uncertainties {
        match &u.reason {
            CompileConditionReason::UndefinedProperties(names) => {
                for n in names {
                    if !props.contains(&n.as_str()) {
                        props.push(n);
                    }
                }
            }
            CompileConditionReason::Unsupported => any_unsupported = true,
        }
    }

    let n = uncertainties.len();
    let item_clause = if n == 1 {
        "1 Compile item is".to_string()
    } else {
        format!("{n} Compile items are")
    };
    let mut detail = String::new();
    if !props.is_empty() {
        detail.push_str(&format!(
            " (unresolved propert{}: {})",
            if props.len() == 1 { "y" } else { "ies" },
            props.join(", ")
        ));
    }
    if any_unsupported {
        if detail.is_empty() {
            detail.push_str(" (unmodeled condition syntax)");
        } else {
            detail.push_str(" plus unmodeled condition syntax");
        }
    }
    format!(
        "{name}: {item_clause} gated on a condition I couldn't evaluate{detail}, \
         so I can't tell which files compile. Project-wide features \
         (go-to-definition into referenced assemblies, cross-file resolution) \
         fall back to single-file resolution here."
    )
}

/// Pick the right diagnostic producer for the URI's file extension, returning
/// the per-file partition the publish planner consumes.
///
/// `.fs` / `.fsi` / `.fsx` → token-stream diagnostics (lexer, symbol-aware)
/// concatenated with parser structural diagnostics, both resolved against
/// the active preprocessor symbol set from the workspace, then **grouped by
/// the virtual file** each diagnostic's `#line` directive relocates it onto
/// ([`diagnostics::grouped_diagnostics`]). All three are lexed by the same F#
/// lexer; the parser runs panic-safely on top. A `.fsi` is parsed under the
/// *signature* grammar ([`diagnostics::SourceKind::Signature`]) so a body-less
/// member signature isn't mis-flagged as an implementation "expected `=`"
/// error; `.fs` / `.fsx` use the implementation grammar.
///
/// `.fsproj` → fsproj-parser diagnostics in a single same-file group (no
/// `#line` handling), requiring the URL to convert to a filesystem path (the
/// parser needs it to seed MSBuild reserved properties and to anchor the
/// `global.json` walk). Non-file URLs silently skip — there's nothing useful
/// we can do without a path.
///
/// Anything else → a single empty same-file group, so a previous diagnostic
/// publish is cleared.
pub(crate) fn grouped_for_uri(
    uri: &Url,
    text: &str,
    workspace: &mut Workspace,
) -> Option<Vec<FileDiagnostics>> {
    grouped_for_uri_linked(uri, text, workspace, None)
}

/// [`grouped_for_uri`] with an optional *linking project* — a `.fsproj` the
/// caller knows enumerates this file in its `<Compile>` list, refining the
/// symbol-set and language-version resolution for a file the ancestor walk
/// cannot place ([`Workspace::symbols_for_linked`]). Only the
/// `workspace/diagnostic` sweep has one in hand; every other caller goes
/// through [`grouped_for_uri`].
pub(crate) fn grouped_for_uri_linked(
    uri: &Url,
    text: &str,
    workspace: &mut Workspace,
    linking_project: Option<&Path>,
) -> Option<Vec<FileDiagnostics>> {
    match path_extension(uri).as_deref() {
        Some(ext @ ("fs" | "fsi" | "fsx")) => {
            let symbols = {
                let _s = tracing::info_span!("symbols_for_uri").entered();
                symbols_for_uri(uri, workspace, linking_project)
            };
            let lang = lang_version_for_uri(uri, workspace, linking_project);
            // `.fsi` is a signature file (specifications, no bodies); `.fs` /
            // `.fsx` are implementation files. `path_extension` lowercases, so
            // `A.FSI` routes here too.
            let kind = if ext == "fsi" {
                diagnostics::SourceKind::Signature
            } else {
                diagnostics::SourceKind::Implementation
            };
            Some(diagnostics::grouped_diagnostics(text, &symbols, kind, lang))
        }
        Some("fsproj") => {
            let path: PathBuf = uri.to_file_path().ok()?;
            let mut diagnostics = fsproj_diagnostics::diagnostics_for(text, &path, workspace.env());
            // Graph-derived cycle diagnostics (consumer #3 stage 3.2) describe the
            // *on-disk* project graph, so we only emit them when the buffer
            // matches disk. An unsaved `.fsproj` edit must not surface a stale
            // cycle (or one anchored on an edited/added `<ProjectReference>`) —
            // its cycles appear once saved, refreshed via `didChangeWatchedFiles`.
            // `project_graph` evaluates the closure fresh off-cache (so this
            // diagnostic neither pins nor reads the project cache, and the graph
            // matches the buffer since buffer == disk); the diagnostics are
            // entry-anchored, so they land in this same-file group.
            if std::fs::read_to_string(&path).is_ok_and(|disk| disk == text) {
                let graph = workspace.project_graph(&path);
                diagnostics.extend(fsproj_diagnostics::graph_diagnostics(text, &path, &graph));
            }
            Some(vec![FileDiagnostics {
                file: None,
                diagnostics,
            }])
        }
        _ => Some(vec![FileDiagnostics {
            file: None,
            diagnostics: Vec::new(),
        }]),
    }
}

/// Resolve the active preprocessor symbol set for a buffer. Thin wrapper
/// over the workspace-only inputs the diagnostic dispatch happens to have on
/// hand; identical to [`State::symbols_for_uri`] otherwise, except that a
/// caller-supplied `linking_project` refines the resolution
/// ([`Workspace::symbols_for_linked`]).
fn symbols_for_uri(
    uri: &Url,
    workspace: &mut Workspace,
    linking_project: Option<&Path>,
) -> HashSet<String> {
    match uri.to_file_path() {
        Ok(path) => match linking_project {
            Some(project) => workspace.symbols_for_linked(&path, project),
            None => workspace.symbols_for(&path),
        },
        Err(()) => implicit_symbols_for_uri(uri),
    }
}

/// Resolve the F# language version for a buffer, mirroring [`symbols_for_uri`]
/// (including the `linking_project` refinement): a `file:` buffer takes its
/// owning project's version ([`Workspace::lang_version_for`]); a non-`file:`
/// buffer (`untitled:`, virtual FS) has no project context, so it gets
/// [`LanguageVersion::Preview`] — every feature on, the same don't-guess-flag
/// default `lang_version_for` uses for orphans.
fn lang_version_for_uri(
    uri: &Url,
    workspace: &mut Workspace,
    linking_project: Option<&Path>,
) -> LanguageVersion {
    match uri.to_file_path() {
        Ok(path) => match linking_project {
            Some(project) => workspace.lang_version_for_linked(&path, project),
            None => workspace.lang_version_for(&path),
        },
        Err(()) => LanguageVersion::Preview,
    }
}

/// Implicit preprocessor symbols for a non-`file:` buffer (`untitled:`, virtual
/// FS): there is no path to resolve an owning project, but the URI extension
/// still tells us the file kind, so hand back the same implicit set
/// [`Workspace::symbols_for`] would (`INTERACTIVE`+`EDITING` for a `.fsx`,
/// `COMPILED`+`EDITING` otherwise) instead of a now-incomplete `{COMPILED}`.
fn implicit_symbols_for_uri(uri: &Url) -> HashSet<String> {
    let is_script = path_extension(uri).as_deref() == Some("fsx");
    crate::workspace::implicit_symbols(is_script)
}

/// Best-effort extension extraction from the URL's path portion. We
/// avoid `to_file_path` here so non-`file://` URIs still get their
/// extension checked (and dispatch to the lexer for `.fs` content
/// served from, e.g., a virtual filesystem). Lowercased so
/// `.FsProj` and `.fsproj` are equivalent. `pub(crate)` so the diagnostic
/// handler can gate `result_id` caching on the file type.
pub(crate) fn path_extension(uri: &Url) -> Option<String> {
    let path = uri.path();
    let last = path.rsplit('/').next()?;
    let ext = std::path::Path::new(last)
        .extension()
        .and_then(OsStr::to_str)?;
    Some(ext.to_ascii_lowercase())
}

/// The filesystem roots to enumerate for `workspace/diagnostic`, from the
/// `initialize` params: each `workspaceFolders` entry that names a local path,
/// or — when none do — the (deprecated) `rootUri`. Non-`file:` URIs are
/// dropped. Pure, so `main.rs` extracts roots without `State` and tests can pin
/// the precedence.
pub fn workspace_roots_from_init(
    workspace_folders: Option<&[WorkspaceFolder]>,
    root_uri: Option<&Url>,
) -> Vec<PathBuf> {
    if let Some(folders) = workspace_folders {
        let roots: Vec<PathBuf> = folders
            .iter()
            .filter_map(|f| f.uri.to_file_path().ok())
            .collect();
        if !roots.is_empty() {
            return roots;
        }
    }
    root_uri
        .and_then(|u| u.to_file_path().ok())
        .into_iter()
        .collect()
}

fn send_notification<N>(conn: &Connection, params: N::Params)
where
    N: NotificationTrait,
    N::Params: serde::Serialize,
{
    let notif = Notification {
        method: N::METHOD.to_string(),
        params: serde_json::to_value(params).expect("notification params serialise"),
    };
    // Send failure means the peer hung up; the main loop will exit on its
    // own when the receiver iterator ends, so we don't propagate here.
    let _ = conn.sender.send(Message::Notification(notif));
}

/// Send a server→client request. The only caller is the
/// `client/registerCapability` at the start of [`run`]; we don't await the
/// response (the dispatch loop drops it), so the id is a fixed, descriptive
/// string rather than a counter.
fn send_request<R>(conn: &Connection, params: R::Params)
where
    R: RequestTrait,
    R::Params: serde::Serialize,
{
    let request = Request {
        id: RequestId::from("borzoi/register-watched-files".to_string()),
        method: R::METHOD.to_string(),
        params: serde_json::to_value(params).expect("request params serialise"),
    };
    let _ = conn.sender.send(Message::Request(request));
}

fn ok_response<T: serde::Serialize>(id: RequestId, value: &T) -> Response {
    Response {
        id,
        result: Some(serde_json::to_value(value).expect("response serialises")),
        error: None,
    }
}

fn err_response(id: RequestId, code: lsp_server::ErrorCode, message: String) -> Response {
    Response {
        id,
        result: None,
        error: Some(lsp_server::ResponseError {
            code: code as i32,
            message,
            data: None,
        }),
    }
}

fn extract<R>(req: Request) -> Result<(RequestId, R::Params), ExtractError<Request>>
where
    R: RequestTrait,
    R::Params: serde::de::DeserializeOwned,
{
    req.extract(R::METHOD)
}

fn extract_notification<N>(not: Notification) -> Result<N::Params, ExtractError<Notification>>
where
    N: NotificationTrait,
    N::Params: serde::de::DeserializeOwned,
{
    not.extract(N::METHOD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::FileChangeType;
    use std::fs;
    use tempfile::TempDir;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn event(uri: Url, typ: FileChangeType) -> FileEvent {
        FileEvent { uri, typ }
    }

    #[test]
    fn compile_uncertainty_message_names_project_and_unresolved_properties() {
        use borzoi_msbuild::{
            CompileConditionReason, CompileConditionUncertainty, DiagnosticOrigin,
        };
        let u = [CompileConditionUncertainty {
            condition: "'$(Foo)' == 'bar'".into(),
            reason: CompileConditionReason::UndefinedProperties(vec!["Foo".into()]),
            span: 0..0,
            origin: DiagnosticOrigin::Buffer,
        }];
        let msg = compile_uncertainty_message(Path::new("/x/Proj.fsproj"), &u);
        assert!(msg.contains("Proj.fsproj"), "{msg}");
        assert!(msg.contains("1 Compile item is"), "{msg}");
        assert!(msg.contains("unresolved property: Foo"), "{msg}");
        assert!(msg.contains("single-file"), "{msg}");
    }

    #[test]
    fn compile_uncertainty_message_pluralises_and_dedupes_properties() {
        use borzoi_msbuild::{
            CompileConditionReason, CompileConditionUncertainty, DiagnosticOrigin,
        };
        let mk = |names: Vec<&str>| CompileConditionUncertainty {
            condition: "c".into(),
            reason: CompileConditionReason::UndefinedProperties(
                names.into_iter().map(str::to_string).collect(),
            ),
            span: 0..0,
            origin: DiagnosticOrigin::Buffer,
        };
        let u = [mk(vec!["Foo"]), mk(vec!["Foo", "Bar"])];
        let msg = compile_uncertainty_message(Path::new("/x/P.fsproj"), &u);
        assert!(msg.contains("2 Compile items are"), "{msg}");
        // "Foo" appears once despite two uncertainties referencing it.
        assert!(msg.contains("unresolved properties: Foo, Bar"), "{msg}");
    }

    #[test]
    fn compile_uncertainty_message_handles_unsupported_only() {
        use borzoi_msbuild::{
            CompileConditionReason, CompileConditionUncertainty, DiagnosticOrigin,
        };
        let u = [CompileConditionUncertainty {
            condition: "Exists('x')".into(),
            reason: CompileConditionReason::Unsupported,
            span: 0..0,
            origin: DiagnosticOrigin::Buffer,
        }];
        let msg = compile_uncertainty_message(Path::new("/x/P.fsproj"), &u);
        assert!(msg.contains("unmodeled condition syntax"), "{msg}");
    }

    #[test]
    fn non_file_buffer_gets_implicit_defines_by_kind() {
        // A non-`file:` buffer can't resolve a project, but must still get the
        // implicit defines for its kind — not a bare `{COMPILED}`.
        assert_eq!(
            implicit_symbols_for_uri(&url("untitled:Untitled-1.fs")),
            HashSet::from(["COMPILED".to_string(), "EDITING".to_string()]),
        );
        assert_eq!(
            implicit_symbols_for_uri(&url("untitled:Untitled-1.fsx")),
            HashSet::from(["INTERACTIVE".to_string(), "EDITING".to_string()]),
        );
    }

    fn changed(uri: Url) -> FileEvent {
        event(uri, FileChangeType::CHANGED)
    }

    /// An SDK-less `.fsproj` that defines `define` and compiles `compile`.
    fn fsproj_define_compile(define: &str, compile: &str) -> String {
        format!(
            r#"<Project>
              <PropertyGroup><DefineConstants>{define}</DefineConstants></PropertyGroup>
              <ItemGroup><Compile Include="{compile}" /></ItemGroup>
            </Project>"#
        )
    }

    #[test]
    fn invalidate_owning_project_skips_fsproj_uris() {
        // A `.fsproj` text-sync must not populate the workspace project
        // cache. Concretely: starting from an empty `State`, sending an
        // invalidate for a `.fsproj` URI leaves the cache empty; a `.fs`
        // URI in the same directory triggers the lookup that would
        // populate it.
        let tmp = tempfile::TempDir::new().unwrap();
        let proj_path = tmp.path().join("App.fsproj");
        std::fs::write(&proj_path, "<Project/>").unwrap();
        let mut state = State::default();
        let project_uri = Url::from_file_path(&proj_path).unwrap();
        state.invalidate_owning_project(&project_uri);
        // Inspect the workspace's project cache via a real test path. The
        // cache is private, but `Workspace::project` exposes the effect:
        // calling it again now returns the same `ParsedProject` (or
        // freshly evaluates one if it hadn't been cached). We assert that
        // the project was *not* pre-populated by the no-op invalidate by
        // pre-mutating disk between calls and observing the *new* content
        // through `symbols_for`. After invalidate, write a fresh
        // `DefineConstants`; the next `symbols_for` should see it (would
        // be cached-stale if invalidate had populated the cache).
        std::fs::write(
            &proj_path,
            r#"<Project><PropertyGroup><DefineConstants>POST_SYNC</DefineConstants></PropertyGroup></Project>"#,
        )
        .unwrap();
        // A sibling `.fs` file shares the project; `symbols_for` evaluates
        // for the *first* time here.
        let fs_path = tmp.path().join("Lib.fs");
        std::fs::write(&fs_path, "").unwrap();
        let symbols = state.workspace.symbols_for(&fs_path);
        assert!(
            symbols.contains("POST_SYNC"),
            "expected POST_SYNC in {symbols:?}; the .fsproj text-sync must \
             not have pre-populated the project cache"
        );
    }

    #[test]
    fn advertises_semantic_tokens_full() {
        // The capability must be present (else clients never send
        // `textDocument/semanticTokens/full`) and its legend must be the one
        // the handler emits indices against.
        let caps = server_capabilities();
        let provider = caps
            .semantic_tokens_provider
            .expect("semantic tokens advertised");
        let lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(opts) = provider
        else {
            panic!("expected inline SemanticTokensOptions");
        };
        assert!(matches!(
            opts.full,
            Some(lsp_types::SemanticTokensFullOptions::Bool(true))
        ));
        assert_eq!(
            opts.legend,
            crate::handlers::semantic_tokens::legend(),
            "advertised legend must match the handler's"
        );
    }

    #[test]
    fn extension_lowercases() {
        assert_eq!(
            path_extension(&url("file:///tmp/App.FsProj")).as_deref(),
            Some("fsproj")
        );
        assert_eq!(
            path_extension(&url("file:///tmp/Library.FS")).as_deref(),
            Some("fs")
        );
    }

    fn folder(uri: &str) -> WorkspaceFolder {
        WorkspaceFolder {
            uri: url(uri),
            name: "f".to_string(),
        }
    }

    #[test]
    fn workspace_roots_prefer_folders_over_root_uri() {
        let folders = [folder("file:///ws/a"), folder("file:///ws/b")];
        let root = url("file:///ws/legacy");
        let roots = workspace_roots_from_init(Some(&folders), Some(&root));
        assert_eq!(
            roots,
            vec![PathBuf::from("/ws/a"), PathBuf::from("/ws/b")],
            "workspaceFolders win over the deprecated rootUri"
        );
    }

    #[test]
    fn workspace_roots_fall_back_to_root_uri() {
        let root = url("file:///ws/legacy");
        // Absent folders, and an empty folder list, both fall back to rootUri.
        assert_eq!(
            workspace_roots_from_init(None, Some(&root)),
            vec![PathBuf::from("/ws/legacy")]
        );
        assert_eq!(
            workspace_roots_from_init(Some(&[]), Some(&root)),
            vec![PathBuf::from("/ws/legacy")]
        );
    }

    #[test]
    fn workspace_roots_drop_non_file_uris() {
        // A non-`file:` folder URI yields no root; with no fallback, none.
        let folders = [folder("untitled:Untitled-1")];
        assert!(workspace_roots_from_init(Some(&folders), None).is_empty());
        assert!(workspace_roots_from_init(None, None).is_empty());
    }

    #[test]
    fn extension_handles_no_extension() {
        assert_eq!(path_extension(&url("file:///tmp/Makefile")), None);
        assert_eq!(path_extension(&url("file:///")), None);
    }

    #[test]
    fn extension_skips_directory_dots() {
        // `/foo.bar/baz` — the extension is taken from the last segment
        // (`baz`), which has none. A naive "rfind('.')" would return
        // `bar/baz` and break dispatch for files inside dotted dirs.
        assert_eq!(path_extension(&url("file:///foo.bar/baz")), None);
    }

    /// Assert the dispatch produced a single same-file group (no cross-file
    /// `#line` relocation) and return its diagnostics.
    fn only_same_file(groups: Vec<FileDiagnostics>) -> Vec<lsp_types::Diagnostic> {
        assert_eq!(groups.len(), 1, "expected one group: {groups:#?}");
        assert_eq!(groups[0].file, None, "expected the same-file group");
        groups.into_iter().next().unwrap().diagnostics
    }

    #[test]
    fn dispatch_routes_fs_to_lexer() {
        let mut ws = Workspace::default();
        let groups = grouped_for_uri(
            &url("file:///tmp/Library.fs"),
            "let x = \"unterminated",
            &mut ws,
        )
        .expect("dispatch returned Some");
        let diags = only_same_file(groups);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert!(diags[0].message.contains("unterminated"));
    }

    #[test]
    fn dispatch_routes_fsi_and_fsx_to_lexer() {
        // Regression: a previous version of the dispatch only matched
        // `Some("fs")`, which cleared diagnostics for `.fsi` (signature) and
        // `.fsx` (script) buffers instead of lexing them. The lexer's the
        // same for all three, so each must surface the lexer's "unterminated"
        // error. `.fsi` parses under the *signature* grammar (where a bare
        // `let` is not a valid specification), so it additionally carries a
        // parser diagnostic — hence we assert the lexer error is *present*,
        // not that it is the *only* diagnostic. `.fsx` is a script
        // (implementation grammar), where `let x = …` is valid, so it stays a
        // lone lexer error.
        let mut ws = Workspace::default();
        for uri in [
            "file:///tmp/Library.fsi",
            "file:///tmp/script.fsx",
            "file:///tmp/Mixed.FSI",
        ] {
            let groups =
                grouped_for_uri(&url(uri), "let x = \"unterminated", &mut ws).expect("Some");
            let diags = only_same_file(groups);
            assert!(
                diags.iter().any(|d| d.message.contains("unterminated")),
                "{uri}: expected the lexer's unterminated-string diagnostic: {diags:#?}"
            );
        }
    }

    /// A `.fsi` buffer is parsed under the *signature* grammar, where a
    /// body-less member signature (`member Name : string`) is valid. The impl
    /// parser, by contrast, rejects it ("expected `=` after binding pattern")
    /// and — when a doc comment precedes the next member — pins that error on
    /// the *docstring*. Routing `.fsi` to the sig parser removes that spurious
    /// squiggle (the sig parser instead reports one honest "type signatures …
    /// not yet supported" diagnostic until phase 10.14 lands).
    #[test]
    fn dispatch_routes_fsi_to_sig_parser() {
        let mut ws = Workspace::default();
        let src = "module M\n\
                   type Foo =\n\
                   \x20   /// blah\n\
                   \x20   member Name : string\n\
                   \x20   /// docs\n\
                   \x20   member Other : int\n";

        // `.fsi` → signature parser: no impl-only "expected `=`" squiggle, and
        // nothing lands on a docstring line.
        let fsi = only_same_file(
            grouped_for_uri(&url("file:///tmp/Library.fsi"), src, &mut ws).expect("Some"),
        );
        assert!(
            !fsi.iter().any(|d| d.message.contains("expected `=`")),
            "the sig parser must not emit the impl-only member-body error: {fsi:#?}"
        );
        let docstring_lines = [2u32, 4]; // 0-based: `/// blah`, `/// docs`
        assert!(
            !fsi.iter()
                .any(|d| docstring_lines.contains(&d.range.start.line)),
            "no diagnostic may land on a docstring line: {fsi:#?}"
        );

        // Contrast: the *same* text as a `.fs` implementation file still hits
        // the impl member parser, which rejects the body-less member.
        let fs = only_same_file(
            grouped_for_uri(&url("file:///tmp/Library.fs"), src, &mut ws).expect("Some"),
        );
        assert!(
            fs.iter().any(|d| d.message.contains("expected `=`")),
            "the impl parser still rejects a body-less member: {fs:#?}"
        );
    }

    #[test]
    fn dispatch_routes_fsproj_to_fsproj_parser() {
        let mut ws = Workspace::default();
        // Malformed XML triggers the single-error path in
        // fsproj_diagnostics. We don't need an SDK installed for that.
        let groups = grouped_for_uri(
            &url("file:///tmp/App.fsproj"),
            "<Project this is broken",
            &mut ws,
        )
        .expect("dispatch returned Some");
        let diags = only_same_file(groups);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert!(diags[0].message.contains("malformed XML"));
    }

    #[test]
    fn dispatch_returns_empty_for_unknown_extension() {
        let mut ws = Workspace::default();
        let groups =
            grouped_for_uri(&url("file:///tmp/notes.txt"), "anything", &mut ws).expect("Some");
        assert!(only_same_file(groups).is_empty());
    }

    /// End-to-end (graph stage 3.2, Stage B): a `.fsproj` in a reference cycle
    /// surfaces a `ReferenceCycle` WARNING on the open buffer — the graph is
    /// built from the two on-disk projects and anchored on the entry's own edge.
    #[test]
    fn fsproj_reference_cycle_surfaces_as_a_diagnostic() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A.fsproj");
        let b = tmp.path().join("B.fsproj");
        let a_text =
            r#"<Project><ItemGroup><ProjectReference Include="B.fsproj" /></ItemGroup></Project>"#;
        fs::write(&a, a_text).unwrap();
        fs::write(
            &b,
            r#"<Project><ItemGroup><ProjectReference Include="A.fsproj" /></ItemGroup></Project>"#,
        )
        .unwrap();

        let mut ws = Workspace::default();
        let groups = grouped_for_uri(&Url::from_file_path(&a).unwrap(), a_text, &mut ws)
            .expect("dispatch returned Some");
        let diags = only_same_file(groups);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("project reference cycle")),
            "expected a cycle warning on A.fsproj: {diags:#?}"
        );
    }

    /// A dirty buffer (diverged from disk) must not surface the on-disk cycle:
    /// graph diagnostics reflect the saved project graph.
    #[test]
    fn fsproj_cycle_suppressed_when_buffer_diverges_from_disk() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A.fsproj");
        let b = tmp.path().join("B.fsproj");
        fs::write(
            &a,
            r#"<Project><ItemGroup><ProjectReference Include="B.fsproj" /></ItemGroup></Project>"#,
        )
        .unwrap();
        fs::write(
            &b,
            r#"<Project><ItemGroup><ProjectReference Include="A.fsproj" /></ItemGroup></Project>"#,
        )
        .unwrap();

        // The open buffer has edited A's reference away from B — no cycle here.
        let buffer =
            r#"<Project><ItemGroup><ProjectReference Include="C.fsproj" /></ItemGroup></Project>"#;
        let mut ws = Workspace::default();
        let groups =
            grouped_for_uri(&Url::from_file_path(&a).unwrap(), buffer, &mut ws).expect("Some");
        let diags = only_same_file(groups);
        assert!(
            diags
                .iter()
                .all(|d| !d.message.contains("project reference cycle")),
            "a diverged buffer must not surface the on-disk cycle: {diags:#?}"
        );
    }

    /// A no-watcher client: after a prior lookup, the `.fsproj` is edited on disk
    /// to introduce a cycle (and the buffer kept in sync) with *no*
    /// `didChangeWatchedFiles`. The next lookup must still surface the cycle —
    /// `project_graph` evaluates the closure fresh off-cache, so a stale cache
    /// entry can't hide the edit.
    #[test]
    fn fsproj_cycle_reflects_disk_edit_without_a_watcher_event() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("A.fsproj");
        let b = tmp.path().join("B.fsproj");
        let c = tmp.path().join("C.fsproj");
        // B → A closes a cycle once A points back at B; C is acyclic.
        fs::write(
            &b,
            r#"<Project><ItemGroup><ProjectReference Include="A.fsproj" /></ItemGroup></Project>"#,
        )
        .unwrap();
        fs::write(&c, r#"<Project></Project>"#).unwrap();

        // Prior lookup: A → C, no cycle. This caches A's evaluation.
        let a_acyclic =
            r#"<Project><ItemGroup><ProjectReference Include="C.fsproj" /></ItemGroup></Project>"#;
        fs::write(&a, a_acyclic).unwrap();
        let mut ws = Workspace::default();
        let url_a = Url::from_file_path(&a).unwrap();
        let first = only_same_file(grouped_for_uri(&url_a, a_acyclic, &mut ws).expect("Some"));
        assert!(
            first
                .iter()
                .all(|d| !d.message.contains("project reference cycle")),
            "A → C is acyclic: {first:#?}"
        );

        // Edit A on disk to reference B (cycle), keep the buffer in sync, and do
        // NOT call invalidate_projects — the no-watcher case the fix targets.
        let a_cyclic =
            r#"<Project><ItemGroup><ProjectReference Include="B.fsproj" /></ItemGroup></Project>"#;
        fs::write(&a, a_cyclic).unwrap();
        let second = only_same_file(grouped_for_uri(&url_a, a_cyclic, &mut ws).expect("Some"));
        assert!(
            second
                .iter()
                .any(|d| d.message.contains("project reference cycle")),
            "the disk edit (A → B → A) must surface despite the stale cache: {second:#?}"
        );
    }

    #[test]
    fn dispatch_skips_non_file_fsproj_url() {
        // `untitled:` schemes (common in editors for unsaved buffers)
        // can't convert to a filesystem path; skip rather than guess.
        let mut ws = Workspace::default();
        let result = grouped_for_uri(&url("untitled:Untitled-1.fsproj"), "<Project/>", &mut ws);
        assert!(result.is_none(), "expected None for non-file URL");
    }

    #[test]
    fn fsproj_sync_does_not_pin_the_project_cache() {
        // A `.fsproj` text-sync must NOT reach `owning_project`: that call
        // evaluates and caches the project (and its siblings) from disk as a
        // side effect, and the workspace project cache has no file-watch
        // invalidation. If a client opens/edits `App.fsproj` before touching
        // any `.fs` file, that side-effecting read would pin the cache to the
        // old on-disk `DefineConstants`/Compile list for the server's
        // lifetime. Regression test for the premature `owning_project` read.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("App.fsproj");
        let file = tmp.path().join("Lib.fs");
        fs::write(&proj, fsproj_define_compile("FOO", "Lib.fs")).unwrap();
        fs::write(&file, "let x = 1\n").unwrap();

        let mut state = State::new();
        // The client opens / edits the project file first.
        state.invalidate_owning_project(&Url::from_file_path(&proj).unwrap());

        // The project then changes on disk (e.g. the user saves a new define).
        fs::write(&proj, fsproj_define_compile("BAR", "Lib.fs")).unwrap();

        // A later source lookup must see the *current* disk state, proving the
        // `.fsproj` sync did not prematurely pin the stale defines.
        let symbols = state.symbols_for_uri(&Url::from_file_path(&file).unwrap());
        assert!(
            symbols.contains("BAR") && !symbols.contains("FOO"),
            "fsproj sync pinned the cache to stale defines: {symbols:?}"
        );
    }

    #[test]
    fn fs_sync_still_invalidates_the_owning_project() {
        // The gate that skips `.fsproj` must not also skip source files: a
        // `.fs` text-sync still has to drop the owning project's semantic
        // parses so the next query re-folds against the buffer overlay.
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("P.fsproj");
        let file = tmp.path().join("Lib.fs");
        fs::write(&proj, fsproj_define_compile("", "Lib.fs")).unwrap();
        fs::write(&file, "let disk = 1\n").unwrap();

        let mut state = State::new();

        // Prime the per-project parses from disk.
        {
            let parses = state
                .semantic
                .parses_for_project(&proj, &mut state.workspace, &state.docs)
                .expect("primed parses");
            assert!(parses.texts[0].contains("disk"));
        }

        // Overlay a buffer and sync the `.fs` file.
        let file_url = Url::from_file_path(&file).unwrap();
        state
            .docs
            .insert(file_url.clone(), "let buffer = 1\n".to_string());
        state.invalidate_owning_project(&file_url);

        // The next query must re-fold and see the buffer text.
        let parses = state
            .semantic
            .parses_for_project(&proj, &mut state.workspace, &state.docs)
            .expect("rebuilt parses");
        assert!(
            parses.texts[0].contains("buffer") && !parses.texts[0].contains("disk"),
            "fs sync did not invalidate the owning project: {:?}",
            parses.texts[0]
        );
    }

    // ----- workspace/didChangeWatchedFiles -----

    #[test]
    fn classify_change_buckets_by_name_and_extension() {
        use ChangeClass::*;
        use FileChangeType as T;
        // A content change (the common case).
        let c = |u: &str| classify_change(&url(u), T::CHANGED);
        assert_eq!(c("file:///p/App.fsproj"), Structural);
        assert_eq!(c("file:///p/Directory.Build.props"), Structural);
        assert_eq!(c("file:///p/Directory.Build.targets"), Structural);
        assert_eq!(c("file:///p/global.json"), Structural);
        assert_eq!(c("file:///p/obj/project.assets.json"), Structural);
        assert_eq!(c("file:///p/packages.lock.json"), Structural);
        assert_eq!(c("file:///p/nuget.config"), Structural);
        assert_eq!(c("file:///p/NuGet.Config"), Structural);
        assert_eq!(c("file:///p/Custom.props"), Structural);
        assert_eq!(c("file:///p/Lib.fs"), Source);
        assert_eq!(c("file:///p/Sig.fsi"), Source);
        assert_eq!(c("file:///p/Script.fsx"), Source);
        assert_eq!(c("file:///p/bin/Debug/net10.0/Lib.dll"), AssemblyInput);
        assert_eq!(c("file:///p/Greeter.cs"), AssemblyInput);
        assert_eq!(c("file:///p/Lib.csproj"), AssemblyInput);
        assert_eq!(c("file:///p/README.md"), Ignored);
        assert_eq!(c("file:///p/Makefile"), Ignored);
        // Case-insensitive name / extension matching.
        assert_eq!(c("file:///p/App.FSPROJ"), Structural);
        assert_eq!(c("file:///p/GLOBAL.JSON"), Structural);
        assert_eq!(c("file:///p/Lib.FS"), Source);
        assert_eq!(c("file:///p/Lib.DLL"), AssemblyInput);
        // `.dll` / `.cs` stay assembly inputs for any change type — a
        // created/deleted DLL moves the located-output boundary like a
        // rewrite, and a `.cs` create/delete changes the sidecar's compile
        // set (never the F# glob-membership that makes `.fs` structural).
        assert_eq!(
            classify_change(&url("file:///p/New.dll"), T::CREATED),
            AssemblyInput
        );
        assert_eq!(
            classify_change(&url("file:///p/Gone.cs"), T::DELETED),
            AssemblyInput
        );
        // A `.csproj` existence change is structural (an open `.fsproj`'s
        // reference diagnostics check the target exists); a content change
        // only feeds the sidecar.
        assert_eq!(
            classify_change(&url("file:///p/Lib.csproj"), T::CREATED),
            Structural
        );
        assert_eq!(
            classify_change(&url("file:///p/Lib.csproj"), T::DELETED),
            Structural
        );
        // Create / delete of a source file is structural (glob membership);
        // structural files stay structural for any change type.
        assert_eq!(
            classify_change(&url("file:///p/New.fs"), T::CREATED),
            Structural
        );
        assert_eq!(
            classify_change(&url("file:///p/Gone.fs"), T::DELETED),
            Structural
        );
        assert_eq!(
            classify_change(&url("file:///p/App.fsproj"), T::CREATED),
            Structural
        );
        assert_eq!(
            classify_change(&url("file:///p/README.md"), T::DELETED),
            Ignored
        );
    }

    /// The headline fix: a `.fsproj` edited on disk is re-evaluated after the
    /// watched-file change, so `symbols_for` sees the new `DefineConstants`
    /// rather than the stale cached set. The inverse of
    /// `fsproj_sync_does_not_pin_the_project_cache` (which proves the cache is
    /// real by never priming it); here we prime it, then prove invalidation
    /// refreshes it.
    #[test]
    fn watched_fsproj_change_refreshes_stale_defines() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("App.fsproj");
        let file = tmp.path().join("Lib.fs");
        fs::write(&proj, fsproj_define_compile("ALPHA", "Lib.fs")).unwrap();
        fs::write(&file, "let x = 1\n").unwrap();

        let mut state = State::default();
        // Prime the project cache: ALPHA is seen.
        assert!(state.workspace.symbols_for(&file).contains("ALPHA"));

        // The project changes on disk; the cache is now stale.
        fs::write(&proj, fsproj_define_compile("BETA", "Lib.fs")).unwrap();
        assert!(
            state.workspace.symbols_for(&file).contains("ALPHA"),
            "cache still holds the stale value before invalidation"
        );

        // Deliver the watched-file change for the project.
        let republish =
            state.apply_watched_changes(&[changed(Url::from_file_path(&proj).unwrap())]);
        assert!(republish.is_empty(), "no open buffers to republish here");

        let symbols = state.workspace.symbols_for(&file);
        assert!(
            symbols.contains("BETA") && !symbols.contains("ALPHA"),
            "defines must refresh after the watched change: {symbols:?}"
        );
    }

    #[test]
    fn structural_change_requests_republish_of_open_buffers() {
        let mut state = State::default();
        let open = url("file:///p/Open.fs");
        state.docs.insert(open.clone(), "let x = 1\n".to_string());

        let republish = state.apply_watched_changes(&[changed(url("file:///p/App.fsproj"))]);
        assert_eq!(
            republish,
            vec![open],
            "a structural change republishes open buffers"
        );
    }

    #[test]
    fn source_only_change_requests_no_republish() {
        let mut state = State::default();
        state
            .docs
            .insert(url("file:///p/Open.fs"), "let x = 1\n".to_string());

        // An unopened source file changing on disk can't alter an open buffer's
        // lexer/parser diagnostics, so nothing is republished.
        let republish = state.apply_watched_changes(&[changed(url("file:///p/Other.fs"))]);
        assert!(republish.is_empty());
    }

    #[test]
    fn created_source_file_is_structural() {
        let mut state = State::default();
        let open = url("file:///p/Open.fs");
        state.docs.insert(open.clone(), "let x = 1\n".to_string());

        // A *created* source file changes glob-expanded Compile sets, so it
        // takes the structural path (and republishes open buffers).
        let republish =
            state.apply_watched_changes(&[event(url("file:///p/New.fs"), FileChangeType::CREATED)]);
        assert_eq!(republish, vec![open], "source create is structural");
    }

    /// End-to-end: a `*.fs` Compile glob picks up a newly created source file
    /// after its watched `Created` event — the case the targeted source
    /// invalidation alone would miss (the project cache would stay stale).
    #[test]
    fn created_source_refreshes_globbed_compile_set() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("App.fsproj");
        fs::write(
            &proj,
            "<Project><ItemGroup><Compile Include=\"*.fs\" /></ItemGroup></Project>",
        )
        .unwrap();
        fs::write(tmp.path().join("Lib.fs"), "let x = 1\n").unwrap();

        let mut state = State::default();
        // Prime: only Lib.fs matches the glob.
        let primed: Vec<PathBuf> = state
            .workspace
            .project(&proj)
            .unwrap()
            .items
            .iter()
            .map(|i| i.include.clone())
            .collect();
        assert!(primed.iter().any(|p| p.ends_with("Lib.fs")), "{primed:?}");
        assert!(!primed.iter().any(|p| p.ends_with("New.fs")), "{primed:?}");

        // Create a new source file and deliver its watched `Created` event.
        let new = tmp.path().join("New.fs");
        fs::write(&new, "let y = 2\n").unwrap();
        state.apply_watched_changes(&[event(
            Url::from_file_path(&new).unwrap(),
            FileChangeType::CREATED,
        )]);

        let after: Vec<PathBuf> = state
            .workspace
            .project(&proj)
            .unwrap()
            .items
            .iter()
            .map(|i| i.include.clone())
            .collect();
        assert!(
            after.iter().any(|p| p.ends_with("New.fs")),
            "the glob-expanded Compile set must pick up the created file: {after:?}"
        );
    }

    /// The headline for the referenced-assembly class: a watched `.dll` change
    /// (a sibling project rebuilt, a package cache updated) drops the cached
    /// assembly env, so the next lookup re-resolves against the new binaries.
    /// Cache identity is observed by `Arc::ptr_eq` — the un-resolvable project
    /// here caches a (stable, empty) env, which is exactly enough to see the
    /// hit/miss behaviour without real DLL fixtures.
    #[test]
    fn watched_dll_change_drops_the_assembly_env_cache() {
        let mut state = State::default();
        let proj = PathBuf::from("/p/App.fsproj");
        let first = state.semantic.assembly_env_for_project(
            &proj,
            None,
            &crate::workspace::ServedTfm::NoneDeclared,
            &state.workspace,
        );
        let hit = state.semantic.assembly_env_for_project(
            &proj,
            None,
            &crate::workspace::ServedTfm::NoneDeclared,
            &state.workspace,
        );
        assert!(
            Arc::ptr_eq(&first, &hit),
            "second lookup must be a cache hit"
        );

        let republish = state
            .apply_watched_changes(&[changed(url("file:///p/sibling/bin/Debug/net10.0/Lib.dll"))]);
        assert!(
            republish.is_empty(),
            "a referenced-assembly change cannot alter an open buffer's \
             lexer/parser diagnostics; nothing republishes"
        );

        let rebuilt = state.semantic.assembly_env_for_project(
            &proj,
            None,
            &crate::workspace::ServedTfm::NoneDeclared,
            &state.workspace,
        );
        assert!(
            !Arc::ptr_eq(&first, &rebuilt),
            "the assembly env must be re-resolved after a watched DLL change"
        );
    }

    /// A `.csproj` *existence* change is structural: an open `.fsproj`'s
    /// `<ProjectReference>` diagnostics check the target file's existence on
    /// disk, so creating (or deleting) the referenced `.csproj` must republish
    /// the open buffer — where a content change (assembly-input class) need
    /// not.
    #[test]
    fn created_csproj_republishes_open_buffers() {
        let mut state = State::default();
        let open = url("file:///p/App.fsproj");
        state.docs.insert(open.clone(), "<Project />".to_string());

        let republish = state
            .apply_watched_changes(&[event(url("file:///p/Lib.csproj"), FileChangeType::CREATED)]);
        assert_eq!(
            republish,
            vec![open],
            "a csproj existence change can fix/break an open fsproj's \
             reference diagnostics, so it must republish"
        );
    }

    /// A referenced-assembly change is narrower than a structural one: the
    /// project-evaluation cache (defines, Compile order) doesn't depend on
    /// binaries, so it must survive.
    #[test]
    fn watched_dll_change_keeps_the_project_evaluation_cache() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("App.fsproj");
        let file = tmp.path().join("Lib.fs");
        fs::write(&proj, fsproj_define_compile("ALPHA", "Lib.fs")).unwrap();
        fs::write(&file, "let x = 1\n").unwrap();

        let mut state = State::default();
        assert!(state.workspace.symbols_for(&file).contains("ALPHA"));
        // Rewrite the project on disk; the cache is stale but a DLL event must
        // NOT be the thing that refreshes it (that is the structural class's
        // job) — staleness here is the *proof* the cache survived.
        fs::write(&proj, fsproj_define_compile("BETA", "Lib.fs")).unwrap();

        state.apply_watched_changes(&[changed(url("file:///p/bin/Lib.dll"))]);
        assert!(
            state.workspace.symbols_for(&file).contains("ALPHA"),
            "a DLL change must not clear the project-evaluation cache"
        );
    }

    // ----- watcher registration (Stage 2) -----

    fn caps_with_watched_files(dynamic: Option<bool>) -> ClientCapabilities {
        ClientCapabilities {
            workspace: Some(lsp_types::WorkspaceClientCapabilities {
                did_change_watched_files: Some(
                    lsp_types::DidChangeWatchedFilesClientCapabilities {
                        dynamic_registration: dynamic,
                        ..Default::default()
                    },
                ),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn no_watcher_registration_without_client_support() {
        // No capabilities at all, capability present but unset, and capability
        // explicitly `false` all yield no registration.
        assert!(State::default().watched_files_registration().is_none());

        let mut state = State::default();
        state.set_client_capabilities(caps_with_watched_files(None));
        assert!(state.watched_files_registration().is_none());

        let mut state = State::default();
        state.set_client_capabilities(caps_with_watched_files(Some(false)));
        assert!(state.watched_files_registration().is_none());
    }

    #[test]
    fn watcher_registration_when_client_supports_it() {
        let mut state = State::default();
        state.set_client_capabilities(caps_with_watched_files(Some(true)));

        let params = state.watched_files_registration().expect("a registration");
        assert_eq!(params.registrations.len(), 1);
        let reg = &params.registrations[0];
        assert_eq!(reg.method, "workspace/didChangeWatchedFiles");

        let options: lsp_types::DidChangeWatchedFilesRegistrationOptions =
            serde_json::from_value(reg.register_options.clone().expect("options")).unwrap();
        let globs: Vec<String> = options
            .watchers
            .iter()
            .map(|w| match &w.glob_pattern {
                lsp_types::GlobPattern::String(s) => s.clone(),
                other => panic!("expected a string glob, got {other:?}"),
            })
            .collect();
        // The globs cover project structure (`.fsproj`, `global.json`), F#
        // source (`.fsx`, and thus `.fs`/`.fsi`), and referenced-assembly
        // inputs (`.dll`, and thus `.cs`/`.csproj`).
        assert!(globs.iter().any(|g| g.contains("fsproj")), "{globs:?}");
        assert!(globs.iter().any(|g| g.contains("fsx")), "{globs:?}");
        assert!(globs.iter().any(|g| g.contains("global.json")), "{globs:?}");
        assert!(globs.iter().any(|g| g.contains("dll")), "{globs:?}");
    }

    // ---- deferred SourceLink fetch pool ------------------------------------

    use std::time::Duration;

    /// A `SourceFetcher` returning canned bytes (or an error), so the pool's
    /// fetch→write→reply path is exercised without a network.
    struct CannedFetcher(Result<Vec<u8>, String>);
    impl SourceFetcher for CannedFetcher {
        fn fetch(&self, _url: &str) -> Result<Vec<u8>, String> {
            self.0.clone()
        }
    }

    fn pending_at(dest: std::path::PathBuf) -> PendingFetch {
        PendingFetch {
            url: "https://example.com/src.fs".into(),
            dest,
            line: 4,
            column: 2,
        }
    }

    fn recv_response(conn: &Connection) -> Response {
        match conn
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("a response within 5s")
        {
            Message::Response(r) => r,
            other => panic!("expected a response, got {other:?}"),
        }
    }

    /// The `uri` of a `textDocument/definition` scalar-location response.
    fn response_uri(resp: &Response) -> String {
        let value = resp.result.clone().expect("a result");
        let loc: lsp_types::Location =
            serde_json::from_value(value).expect("a scalar Location result");
        loc.uri.to_string()
    }

    #[test]
    fn fetch_pool_fetches_writes_and_replies_with_a_location() {
        let (server, client) = Connection::memory();
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("src.fs");
        let fetcher: Arc<dyn SourceFetcher> = Arc::new(CannedFetcher(Ok(b"let x = 1\n".to_vec())));
        let pool = FetchPool::new(&server, fetcher, 2, 8);

        pool.submit(FetchJob {
            id: RequestId::from(7),
            pending: pending_at(dest.clone()),
        })
        .expect("queue has room");

        let resp = recv_response(&client);
        assert_eq!(resp.id, RequestId::from(7));
        assert!(resp.error.is_none());
        // A real location (Scalar), not the null `result`.
        assert_ne!(resp.result, Some(serde_json::Value::Null));
        // The fetch was written to the cache path the worker was handed.
        assert_eq!(std::fs::read(&dest).unwrap(), b"let x = 1\n");
    }

    #[test]
    fn fetch_pool_surfaces_the_url_when_the_fetch_fails() {
        let (server, client) = Connection::memory();
        let fetcher: Arc<dyn SourceFetcher> = Arc::new(CannedFetcher(Err("boom".into())));
        let pool = FetchPool::new(&server, fetcher, 1, 8);

        pool.submit(FetchJob {
            id: RequestId::from(9),
            pending: pending_at("/definitely/not/written.fs".into()),
        })
        .expect("queue has room");

        // A failed fetch falls back to the SourceLink URL, not `null` — the
        // client can still open it.
        let resp = recv_response(&client);
        assert_eq!(resp.id, RequestId::from(9));
        assert!(resp.error.is_none());
        assert_eq!(response_uri(&resp), "https://example.com/src.fs");
    }

    /// A `SourceFetcher` that signals when it enters `fetch` then blocks until
    /// the test releases it — lets a test pin a worker as "busy" so the queue
    /// can be filled deterministically.
    struct GateFetcher {
        entered: SyncSender<()>,
        gate: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl SourceFetcher for GateFetcher {
        fn fetch(&self, _url: &str) -> Result<Vec<u8>, String> {
            let _ = self.entered.send(());
            // Block until the test drops the gate sender (releasing `recv`).
            let _ = self.gate.lock().unwrap().recv();
            Ok(Vec::new())
        }
    }

    #[test]
    fn dispatch_fetch_surfaces_the_url_immediately_when_the_queue_is_full() {
        let (server, client) = Connection::memory();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel::<()>(4);
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let fetcher: Arc<dyn SourceFetcher> = Arc::new(GateFetcher {
            entered: entered_tx,
            gate: Mutex::new(gate_rx),
        });
        // One worker, a 1-deep queue.
        let pool = FetchPool::new(&server, fetcher, 1, 1);

        // Job A occupies the only worker (it blocks in `fetch`); wait until it
        // has actually entered the fetch so the queue is known-empty.
        pool.submit(FetchJob {
            id: RequestId::from(1),
            pending: pending_at("/a.fs".into()),
        })
        .expect("A enqueued");
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker entered fetch");
        // Job B fills the single queue slot.
        pool.submit(FetchJob {
            id: RequestId::from(2),
            pending: pending_at("/b.fs".into()),
        })
        .expect("B fills the queue");
        // Job C overflows → dispatch surfaces the URL *now*, without blocking.
        dispatch_fetch(
            &server,
            Some(&pool),
            RequestId::from(3),
            pending_at("/c.fs".into()),
        )
        .expect("send ok");

        let resp = recv_response(&client);
        assert_eq!(resp.id, RequestId::from(3));
        assert_eq!(response_uri(&resp), "https://example.com/src.fs");

        // Release the worker so the pool shuts down (joins) cleanly on drop.
        drop(gate_tx);
    }

    #[test]
    fn dispatch_fetch_surfaces_the_url_when_no_pool_exists() {
        // The no-`sourcelink-fetch` build (or `run_with_fetcher(None)`): the
        // handler still defers, and dispatch surfaces the SourceLink URL rather
        // than dropping the id or replying `null`.
        let (server, client) = Connection::memory();
        dispatch_fetch(
            &server,
            None,
            RequestId::from(3),
            pending_at("/c.fs".into()),
        )
        .expect("send ok");
        let resp = recv_response(&client);
        assert_eq!(resp.id, RequestId::from(3));
        assert_eq!(response_uri(&resp), "https://example.com/src.fs");
    }

    #[test]
    fn dispatch_fetch_does_not_surface_a_non_https_url() {
        // The PDB SourceLink map is untrusted: a non-HTTPS URL must yield `null`
        // (as it did pre-async, when the fetcher refused it), never ask the
        // client to open an `http://` / arbitrary-scheme URI.
        let (server, client) = Connection::memory();
        let pending = PendingFetch {
            url: "http://evil.example/x.fs".into(),
            dest: "/x.fs".into(),
            line: 1,
            column: 1,
        };
        dispatch_fetch(&server, None, RequestId::from(5), pending).expect("send ok");
        let resp = recv_response(&client);
        assert_eq!(resp.id, RequestId::from(5));
        assert_eq!(resp.result, Some(serde_json::Value::Null));
    }
}
