//! Deterministic per-message counts for the server's edit loop: the work one
//! keystroke costs, from `didChange` through the requests a client sends after
//! it, on a real restored project. `docs/incremental-lsp-plan.md` states each of
//! its stages' gains in these units.
//!
//! The real dispatch loop ([`borzoi::server::run_with_fetcher`]) runs on its own
//! thread, as `main` runs it, over an in-memory connection. This thread is the
//! client: it opens an early Compile item `E` and the last one `L`, makes one
//! cold pass of requests, then types into a trailing line comment of `E`, one
//! character per keystroke, and after each keystroke sends what a pull client
//! sends: diagnostics for both open documents, each echoing the `resultId` of
//! the last report so an unchanged document is answered `unchanged`, semantic
//! tokens for both (the
//! server asks for a `workspace/semanticTokens/refresh` after every edit, so a
//! client re-requests every visible buffer), and a hover in `L`. Every message
//! is followed by an ordering barrier, so the server has finished with it, and
//! with its replies to any refresh request the client answered, before the
//! counters are read. A comment edit changes no file's exports, so it is the
//! edit an incremental server should answer most cheaply.
//!
//! Each row counts **every thread but this client thread**: the server thread,
//! and the workers it starts to read DLLs in parallel when it builds an env.
//! Nothing else runs: the SourceLink fetch pool is not built (no fetcher), and
//! an in-memory connection has no IO threads.
//!
//! - **instructions**: the process's retired instructions
//!   (`proc_pid_rusage`, which keeps the counts of threads that have exited)
//!   less this client thread's own (`thread_selfcounts`).
//! - **allocations** and **bytes allocated**: a counting wrapper around the
//!   system allocator that skips the client thread (a reallocation
//!   counts as an allocation of the new size, as in `borzoi-sema`'s
//!   `parse_counts`).
//!
//! - **opens** and **stats**: the files the server opened to read source
//!   text (each followed by an `fstat` of the open file), and the paths it
//!   `stat`ed to confirm what an earlier message read, as `borzoi::vfs`
//!   counts them per generation and reports in its [`GENERATION_EVENT`]. The
//!   plan counts file-system calls inside the lint-enforced modules that
//!   perform them (`docs/incremental-lsp-plan.md`, D3), where a test can
//!   assert them on any platform; the server's other reads
//!   (`borzoi::observed::Unobserved`) are not counted yet.
//! - **e.open**, **e.stat**, **e.list**, **e.canon** and **e.reads**: what
//!   the MSBuild evaluations the message ran observed, as each one's
//!   `ParsedProject::read_counts` reports it (files opened, paths `stat`ed,
//!   directories listed, paths canonicalised), and the size of their read
//!   sets, from [`EVALUATION_EVENT`].
//! - **v.open**, **v.stat**, **v.list** and **v.canon**: what confirming the
//!   cached evaluations cost, which every generation that reads one does
//!   once (`borzoi::workspace::Workspace::validate`): files read again,
//!   paths `stat`ed, directories listed and paths canonicalised, from each
//!   [`GENERATION_EVENT`].
//! - **o.list** and **o.stat**: what finding the projects that own files
//!   observed (`borzoi::workspace::Workspace::owning_project`): directories
//!   listed and paths `stat`ed, from each [`GENERATION_EVENT`].
//!
//! The counts describe the project fold only if the requests take it, and a
//! handler that falls back to single-file analysis still answers. So the run
//! fails unless every token and hover request opens the server's
//! [`PROJECT_LOOKUP_SPAN`] for the measured project (observed by a `tracing`
//! subscriber interested in little else), and fails on any project-deferral
//! toast.
//!
//! Instruction counts need macOS; elsewhere that column reads `n/a`. Wall time
//! is not reported: the machines these run on are loaded. The instruction
//! counter includes kernel mode, so a row that reads files (`semanticTokens E`,
//! the cold rows) carries a kernel component that depends on whether the
//! kernel still caches those files' vnodes. It can add up to about half again,
//! and it is bimodal under other processes' file churn. Rows without file I/O
//! repeat to within 0.2%. Each keystroke row reports the minimum and the
//! maximum over the keystrokes.
//!
//! Run it under `nix develop`: the env the server builds depends on the
//! `dotnet` and packs it finds, and outside the devshell it builds a
//! different one.
//!
//! ```sh
//! nix develop -c cargo run --release -p borzoi --example edit_counts
//! ```
//!
//! * `EDIT_COUNTS_PROJECT`: the `.fsproj` to edit in. It must be restored
//!   (`obj/project.assets.json`). Default: `WoofWare.Incremental` from the
//!   whole-project gate's corpus cache
//!   (`${BORZOI_PROJECT_CORPUS_CACHE:-~/.cache/borzoi/project-corpus}/v2`).
//! * `EDIT_COUNTS_EARLY` / `EDIT_COUNTS_LATE`: the Compile indices of `E` and
//!   `L`. Default: the first `.fs` item at or after an eighth of the way
//!   through, and the last `.fs` item.
//! * `EDIT_COUNTS_KEYSTROKES` (default 5): how many keystrokes to type.
//! * `EDIT_COUNTS_CLIENT` (`pull`, the default, or `push`): the diagnostic
//!   model the client advertises. A push client is sent diagnostics from
//!   `didChange` itself and sends no diagnostic requests.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;

use borzoi::observed::{EVALUATION_EVENT, GENERATION_EVENT};
use borzoi::semantic::ProjectId;
use borzoi::server::{SERVER_STACK_BYTES, State, run_with_fetcher};
use borzoi::uri::UriExt as _;
use borzoi::workspace::Workspace;
use borzoi_msbuild::ItemKind;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{ClientCapabilities, Uri};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Which thread is counted.
// ---------------------------------------------------------------------------

thread_local! {
    /// Set on the client thread, which is the one thread not counted: the
    /// server thread and any worker it starts (the env build reads DLLs on a
    /// pool of them) are. A `const` initialiser
    /// with no destructor, so reading it from inside the allocator or a libc
    /// wrapper can neither allocate nor recurse.
    static CLIENT: Cell<bool> = const { Cell::new(false) };
}

fn counted() -> bool {
    !CLIENT.try_with(Cell::get).unwrap_or(true)
}

// ---------------------------------------------------------------------------
// Allocations.
// ---------------------------------------------------------------------------

/// The system allocator, counting calls and requested bytes on the counted
/// thread.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

fn note_allocation(bytes: usize) {
    if counted() {
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(bytes as u64, Relaxed);
    }
}

// SAFETY: every method forwards to `System` with its arguments unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_allocation(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // A reallocation is an allocation of the new block: counting it is what
        // makes a growing `Vec` visible.
        note_allocation(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

// ---------------------------------------------------------------------------
// Instructions.
// ---------------------------------------------------------------------------

/// Instructions retired by the whole process, and by the calling thread.
#[cfg(target_os = "macos")]
fn instructions() -> Option<(u64, u64)> {
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
        fn getpid() -> i32;
        fn thread_selfcounts(kind: i32, buffer: *mut u64, bytes: usize) -> i32;
    }
    /// `RUSAGE_INFO_V4`, whose struct is a 16-byte UUID followed by `u64`s.
    const RUSAGE_INFO_V4: i32 = 4;
    /// `ri_instructions`'s index among those `u64`s.
    const RI_INSTRUCTIONS: usize = 29;
    /// `thread_selfcounts`'s instructions-and-cycles request.
    const THSC_CPI: i32 = 1;
    let mut rusage = [0u64; 64];
    // SAFETY: `rusage` is larger than `rusage_info_v4` (37 words).
    let rc = unsafe { proc_pid_rusage(getpid(), RUSAGE_INFO_V4, rusage.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let mut own = [0u64; 2];
    // SAFETY: `own` holds the two counters `THSC_CPI` writes.
    let rc = unsafe { thread_selfcounts(THSC_CPI, own.as_mut_ptr(), size_of_val(&own)) };
    (rc == 0).then_some((rusage[2 + RI_INSTRUCTIONS], own[0]))
}

#[cfg(not(target_os = "macos"))]
fn instructions() -> Option<(u64, u64)> {
    None
}

// ---------------------------------------------------------------------------
// Which path a request took.
// ---------------------------------------------------------------------------

/// The measured project's spelling, as the server keys it (`ProjectId`).
static PROJECT_SPELLING: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// `semantic.project_lookup` spans the server thread opened for the measured
/// project, and for any other.
static LOOKUPS_OF_PROJECT: AtomicU64 = AtomicU64::new(0);
static LOOKUPS_OF_OTHERS: AtomicU64 = AtomicU64::new(0);

/// The one span that proves a request reached the project fold: every
/// semantic handler opens it (`SemanticState::resolved_prefix_and_env_for`)
/// only after the file's owning project was found and its parses accepted.
/// A handler that falls back to single-file analysis, for any reason, never
/// opens it, so its responses (which may still hold tokens or a hover) cannot
/// pass for the fold's.
const PROJECT_LOOKUP_SPAN: &str = "semantic.project_lookup";

/// The span around building an env from its reference DLLs, whose `count`
/// field is how many it was handed. A restored project always references at
/// least its framework, so an env built from none means the references failed
/// to resolve (an unreadable assets file, a missing pack), and every fold over
/// it measures a server with no assemblies, which no toast reports.
const ENV_BUILD_SPAN: &str = "build_env_from_dll_paths";

/// Envs built, and of those, envs built from no DLLs.
static ENV_BUILDS: AtomicU64 = AtomicU64::new(0);
static EMPTY_ENV_BUILDS: AtomicU64 = AtomicU64::new(0);

/// Files opened and paths `stat`ed by every generation that has ended, from
/// each [`GENERATION_EVENT`]'s `opens` and `stats`.
static OPENS: AtomicU64 = AtomicU64::new(0);
static STATS: AtomicU64 = AtomicU64::new(0);

/// What every MSBuild evaluation observed, from each [`EVALUATION_EVENT`]:
/// opens, stats, listings, canonicalisations, and the read-set size.
static EVALUATION: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
const EVALUATION_FIELDS: [&str; 5] = ["opens", "stats", "listings", "canonicalizations", "reads"];

/// What confirming cached evaluations cost every generation that has ended,
/// from each [`GENERATION_EVENT`] (opens, stats, listings and
/// canonicalisations), and what finding the projects that own files cost it
/// (listings and stats).
static VALIDATION: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
const VALIDATION_FIELDS: [&str; 6] = [
    "v_opens",
    "v_stats",
    "v_listings",
    "v_canonicalizations",
    "o_listings",
    "o_stats",
];

/// A `tracing` subscriber interested in [`PROJECT_LOOKUP_SPAN`],
/// [`ENV_BUILD_SPAN`], [`GENERATION_EVENT`] and [`EVALUATION_EVENT`] alone. Every
/// other callsite is registered as never interesting, so it costs what it
/// costs with no subscriber, and observing allocates nothing.
struct FoldObserver;

/// Compares what a field formats to with an expected string, without
/// allocating.
struct Matches<'a> {
    rest: &'a str,
    ok: bool,
}

impl std::fmt::Write for Matches<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        match self.rest.strip_prefix(s) {
            Some(rest) if self.ok => self.rest = rest,
            _ => self.ok = false,
        }
        Ok(())
    }
}

/// Whether a span's `project` field names the measured project.
struct ProjectField(Option<bool>);

impl tracing::field::Visit for ProjectField {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "project" {
            let expected = PROJECT_SPELLING.get().map_or("", String::as_str);
            let mut matches = Matches {
                rest: expected,
                ok: true,
            };
            let _ = std::fmt::write(&mut matches, format_args!("{value:?}"));
            self.0 = Some(matches.ok && matches.rest.is_empty());
        }
    }
}

/// A generation event's `opens` and `stats` fields, and its validation
/// fields in [`VALIDATION_FIELDS`] order.
#[derive(Default)]
struct ReadFields {
    opens: Option<u64>,
    stats: Option<u64>,
    validation: [Option<u64>; 6],
}

impl tracing::field::Visit for ReadFields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "opens" => self.opens = Some(value),
            "stats" => self.stats = Some(value),
            name => {
                if let Some(i) = VALIDATION_FIELDS.iter().position(|v| *v == name) {
                    self.validation[i] = Some(value);
                }
            }
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

/// An evaluation event's fields, in [`EVALUATION_FIELDS`] order.
#[derive(Default)]
struct EvaluationFields([Option<u64>; 5]);

impl tracing::field::Visit for EvaluationFields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if let Some(i) = EVALUATION_FIELDS
            .iter()
            .position(|name| *name == field.name())
        {
            self.0[i] = Some(value);
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

/// An env build's `count` field.
struct DllCount(Option<u64>);

impl tracing::field::Visit for DllCount {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "count" {
            self.0 = Some(value);
        }
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if field.name() == "count" {
            self.0 = u64::try_from(value).ok();
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

impl tracing::Subscriber for FoldObserver {
    fn register_callsite(
        &self,
        metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        if self.enabled(metadata) {
            tracing::subscriber::Interest::always()
        } else {
            tracing::subscriber::Interest::never()
        }
    }
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        (metadata.is_span() && matches!(metadata.name(), PROJECT_LOOKUP_SPAN | ENV_BUILD_SPAN))
            || (metadata.is_event()
                && matches!(metadata.name(), GENERATION_EVENT | EVALUATION_EVENT))
    }
    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        if counted() && span.metadata().name() == ENV_BUILD_SPAN {
            let mut count = DllCount(None);
            span.record(&mut count);
            ENV_BUILDS.fetch_add(1, Relaxed);
            if count.0 == Some(0) {
                EMPTY_ENV_BUILDS.fetch_add(1, Relaxed);
            }
        } else if counted() {
            let mut field = ProjectField(None);
            span.record(&mut field);
            let counter = match field.0 {
                Some(true) => &LOOKUPS_OF_PROJECT,
                _ => &LOOKUPS_OF_OTHERS,
            };
            counter.fetch_add(1, Relaxed);
        }
        // Nothing is recorded per span, so one id serves them all.
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if counted() && event.metadata().name() == EVALUATION_EVENT {
            let mut fields = EvaluationFields::default();
            event.record(&mut fields);
            for (counter, value) in EVALUATION.iter().zip(fields.0) {
                counter.fetch_add(
                    value.expect("an evaluation event carries every field"),
                    Relaxed,
                );
            }
        } else if counted() {
            let mut fields = ReadFields::default();
            event.record(&mut fields);
            OPENS.fetch_add(
                fields.opens.expect("a generation event carries `opens`"),
                Relaxed,
            );
            STATS.fetch_add(
                fields.stats.expect("a generation event carries `stats`"),
                Relaxed,
            );
            for (counter, value) in VALIDATION.iter().zip(fields.validation) {
                counter.fetch_add(
                    value.expect("a generation event carries every validation field"),
                    Relaxed,
                );
            }
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

// ---------------------------------------------------------------------------
// One message's cost.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Cost {
    /// Project-fold lookups for the measured project, and for any other
    /// (see [`PROJECT_LOOKUP_SPAN`]). Checked, never reported.
    lookups: (u64, u64),
    instructions: Option<u64>,
    allocations: u64,
    allocated_bytes: u64,
    opens: u64,
    stats: u64,
    /// The MSBuild evaluations' counts, in [`EVALUATION_FIELDS`] order.
    evaluation: [u64; 5],
    /// The validations' and the ownership walks' counts, in
    /// [`VALIDATION_FIELDS`] order.
    validation: [u64; 6],
}

impl Cost {
    fn add(self, other: Cost) -> Cost {
        Cost {
            lookups: (
                self.lookups.0 + other.lookups.0,
                self.lookups.1 + other.lookups.1,
            ),
            instructions: self
                .instructions
                .zip(other.instructions)
                .map(|(a, b)| a + b),
            allocations: self.allocations + other.allocations,
            allocated_bytes: self.allocated_bytes + other.allocated_bytes,
            opens: self.opens + other.opens,
            stats: self.stats + other.stats,
            evaluation: std::array::from_fn(|i| self.evaluation[i] + other.evaluation[i]),
            validation: std::array::from_fn(|i| self.validation[i] + other.validation[i]),
        }
    }
}

/// A reading of every counter.
struct Reading {
    lookups: (u64, u64),
    instructions: Option<(u64, u64)>,
    allocations: u64,
    allocated_bytes: u64,
    opens: u64,
    stats: u64,
    evaluation: [u64; 5],
    validation: [u64; 6],
}

impl Reading {
    fn now() -> Reading {
        Reading {
            lookups: (
                LOOKUPS_OF_PROJECT.load(Relaxed),
                LOOKUPS_OF_OTHERS.load(Relaxed),
            ),
            instructions: instructions(),
            allocations: ALLOCATIONS.load(Relaxed),
            allocated_bytes: ALLOCATED_BYTES.load(Relaxed),
            opens: OPENS.load(Relaxed),
            stats: STATS.load(Relaxed),
            evaluation: std::array::from_fn(|i| EVALUATION[i].load(Relaxed)),
            validation: std::array::from_fn(|i| VALIDATION[i].load(Relaxed)),
        }
    }

    /// What the server thread did since `self`: the process's instructions
    /// less this thread's own.
    fn since(&self, later: &Reading) -> Cost {
        Cost {
            lookups: (
                later.lookups.0 - self.lookups.0,
                later.lookups.1 - self.lookups.1,
            ),
            instructions: self
                .instructions
                .zip(later.instructions)
                .map(|((process0, own0), (process1, own1))| (process1 - process0) - (own1 - own0)),
            allocations: later.allocations - self.allocations,
            allocated_bytes: later.allocated_bytes - self.allocated_bytes,
            opens: later.opens - self.opens,
            stats: later.stats - self.stats,
            evaluation: std::array::from_fn(|i| later.evaluation[i] - self.evaluation[i]),
            validation: std::array::from_fn(|i| later.validation[i] - self.validation[i]),
        }
    }
}

// ---------------------------------------------------------------------------
// The client.
// ---------------------------------------------------------------------------

/// How long any one message may take before the run is called wedged.
const RECV_TIMEOUT: Duration = Duration::from_secs(600);

/// Barrier rounds one settle may take before the server is called looping:
/// each round answers at most one server→client refresh, and the server owes
/// at most one of each kind.
const MAX_SETTLE_ROUNDS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Push,
    Pull,
}

impl Mode {
    /// What VS Code advertises for the parts of the protocol the server reads.
    fn caps(self) -> ClientCapabilities {
        let mut caps = json!({
            "textDocument": {
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                "publishDiagnostics": {},
            },
            "workspace": { "semanticTokens": { "refreshSupport": true } },
        });
        if self == Mode::Pull {
            caps["textDocument"]["diagnostic"] = json!({});
            caps["workspace"]["diagnostic"] = json!({ "refreshSupport": true });
        }
        serde_json::from_value(caps).expect("client capabilities deserialise")
    }
}

struct Client {
    conn: Connection,
    server: Option<std::thread::JoinHandle<()>>,
    next_id: u64,
    /// Every `window/showMessage` the server has sent. Its only toast is the
    /// project-deferral report (`server::refresh_project_deferrals`): the
    /// project's fold, or its reference edges, were declined, so the requests
    /// fell back to single-file analysis and their counts describe that path.
    toasts: Vec<String>,
}

impl Client {
    fn start(mode: Mode, root: PathBuf) -> Client {
        let (server_side, conn) = Connection::memory();
        let server = std::thread::Builder::new()
            .name("borzoi".into())
            .stack_size(SERVER_STACK_BYTES)
            .spawn(move || {
                let mut state = State::new();
                state.set_client_capabilities(mode.caps());
                state.set_workspace_roots(vec![root]);
                run_with_fetcher(server_side, state, None).expect("the server loop ends cleanly");
            })
            .expect("spawn the server thread");
        Client {
            conn,
            server: Some(server),
            next_id: 0,
            toasts: Vec::new(),
        }
    }

    fn send(&self, msg: Message) {
        self.conn.sender.send(msg).expect("the server is listening");
    }

    /// Send a request and wait for its response, answering any request the
    /// server sends first as a client would. Returns how many such requests
    /// were answered.
    fn request(&mut self, method: &str, params: Value) -> (Response, usize) {
        let id = RequestId::from(format!("c{}", self.next_id));
        self.next_id += 1;
        self.send(Message::Request(Request {
            id: id.clone(),
            method: method.to_string(),
            params,
        }));
        let mut answered = 0;
        loop {
            let msg = self
                .conn
                .receiver
                .recv_timeout(RECV_TIMEOUT)
                .unwrap_or_else(|e| panic!("no response to {method}: {e}"));
            match msg {
                Message::Response(resp) if resp.id == id => return (resp, answered),
                Message::Response(resp) => panic!("unsolicited response {resp:?}"),
                // `workspace/semanticTokens/refresh`, `workspace/diagnostic/refresh`,
                // `client/registerCapability`: acknowledged with `null`.
                Message::Request(req) => {
                    answered += 1;
                    self.send(Message::Response(Response {
                        id: req.id,
                        response_result: Ok(Value::Null),
                    }));
                }
                Message::Notification(not) if not.method == "window/showMessage" => {
                    self.toasts.push(not.params["message"].to_string());
                }
                // Published diagnostics: what the client would show.
                Message::Notification(_) => {}
            }
        }
    }

    /// Wait until the server has processed everything sent so far, including
    /// the client's replies to its refresh requests. An unknown method is
    /// answered `MethodNotFound` without touching any cache.
    fn settle(&mut self) {
        for _ in 0..MAX_SETTLE_ROUNDS {
            let (_, answered) = self.request("borzoi/editCounts/barrier", Value::Null);
            if answered == 0 {
                return;
            }
        }
        panic!("the server kept sending requests after {MAX_SETTLE_ROUNDS} barriers");
    }

    /// Send `step` and settle, returning what the server thread spent on it,
    /// and a request's result.
    fn measure(&mut self, step: &Step) -> (Cost, Value) {
        let before = Reading::now();
        let result = match step {
            Step::Notify { method, params } => {
                self.send(Message::Notification(Notification {
                    method: method.to_string(),
                    params: params.clone(),
                }));
                Value::Null
            }
            Step::Request { method, params, .. } => {
                let (resp, _) = self.request(method, params.clone());
                resp.response_result
                    .unwrap_or_else(|err| panic!("{method} failed: {err:?}"))
            }
            Step::Barrier => Value::Null,
        };
        self.settle();
        let cost = before.since(&Reading::now());
        if let Step::Request {
            method,
            folds: true,
            ..
        } = step
        {
            assert!(
                cost.lookups.0 > 0,
                "{method} never reached the project fold, so its counts would measure a fallback"
            );
        }
        assert_eq!(
            cost.lookups.1, 0,
            "the server folded a project other than the measured one"
        );
        assert_eq!(
            EMPTY_ENV_BUILDS.load(Relaxed),
            0,
            "an env was built from no DLLs: the project's references did not resolve \
             (is it restored?), so these counts would measure a server without assemblies"
        );
        assert!(
            self.toasts.is_empty(),
            "the server declined project-wide analysis, so these counts would measure \
             its single-file fallback: {:?}",
            self.toasts
        );
        (cost, result)
    }

    fn shut_down(mut self) {
        self.request("shutdown", Value::Null);
        self.send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: Value::Null,
        }));
        self.server
            .take()
            .expect("started")
            .join()
            .expect("the server thread exits");
    }
}

// ---------------------------------------------------------------------------
// The script.
// ---------------------------------------------------------------------------

/// One measured message (each is followed by [`Client::settle`]).
enum Step {
    Notify {
        method: &'static str,
        params: Value,
    },
    Request {
        method: &'static str,
        params: Value,
        /// The request must reach the measured project's fold
        /// ([`PROJECT_LOOKUP_SPAN`]); otherwise it is answered by a fallback
        /// and its counts describe a different server.
        folds: bool,
    },
    /// Nothing but the settle: what every other row's barrier costs.
    Barrier,
}

struct Doc {
    uri: Uri,
    text: String,
}

fn did_open(doc: &Doc) -> Step {
    Step::Notify {
        method: "textDocument/didOpen",
        params: json!({ "textDocument": {
            "uri": doc.uri.as_str(), "languageId": "fsharp", "version": 0, "text": doc.text,
        }}),
    }
}

fn did_change(doc: &Doc, version: i32) -> Step {
    Step::Notify {
        method: "textDocument/didChange",
        params: json!({
            "textDocument": { "uri": doc.uri.as_str(), "version": version },
            "contentChanges": [{ "text": doc.text }],
        }),
    }
}

fn semantic_tokens(doc: &Doc) -> Step {
    Step::Request {
        method: "textDocument/semanticTokens/full",
        params: json!({ "textDocument": { "uri": doc.uri.as_str() } }),
        folds: true,
    }
}

/// A `textDocument/diagnostic` pull echoing the `resultId` of the client's
/// last report for `doc`, as VS Code does: the server answers `unchanged`
/// without lexing or parsing when nothing it keys the report on has moved.
fn diagnostic(doc: &Doc, result_ids: &BTreeMap<String, String>) -> Step {
    Step::Request {
        method: "textDocument/diagnostic",
        params: json!({
            "textDocument": { "uri": doc.uri.as_str() },
            "previousResultId": result_ids.get(doc.uri.as_str()),
        }),
        folds: false,
    }
}

/// Remember the `resultId` a diagnostic report carries, to echo next time.
fn remember_result_id(result_ids: &mut BTreeMap<String, String>, doc: &Doc, report: &Value) {
    if let Some(id) = report["resultId"].as_str() {
        result_ids.insert(doc.uri.as_str().to_string(), id.to_string());
    }
}

fn hover(doc: &Doc, (line, character): (u32, u32)) -> Step {
    Step::Request {
        method: "textDocument/hover",
        params: json!({
            "textDocument": { "uri": doc.uri.as_str() },
            "position": { "line": line, "character": character },
        }),
        folds: true,
    }
}

/// The position of the name of the first `let` binding in `text`, at any
/// indentation, in UTF-16 code units. `None` when there is no such binding;
/// [`main`] also checks that the server answers a hover there, since a
/// position on a keyword measures the single-file fallback, not the fold.
fn hover_position(text: &str) -> Option<(u32, u32)> {
    const MODIFIERS: [&str; 6] = ["rec", "inline", "private", "internal", "public", "mutable"];
    for (line, content) in text.lines().enumerate() {
        let indent = content.len() - content.trim_start().len();
        let Some(mut rest) = content[indent..].strip_prefix("let ") else {
            continue;
        };
        loop {
            rest = rest.trim_start();
            let word_end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            if MODIFIERS.contains(&&rest[..word_end]) {
                rest = &rest[word_end..];
            } else {
                break;
            }
        }
        if !rest.starts_with(|c: char| c.is_alphabetic() || c == '_') {
            continue;
        }
        let column = content.len() - rest.len();
        let character = content[..column].encode_utf16().count();
        return Some((line as u32, character as u32));
    }
    None
}

/// The client's own file-system reads, which the server never sees. The
/// crate's `clippy.toml` bans the `std` calls outside `borzoi::observed`, the
/// module that counts the server's.
#[allow(clippy::disallowed_methods)]
mod client_fs {
    use std::path::Path;

    pub fn is_file(path: &Path) -> bool {
        path.is_file()
    }

    /// The file's length, or 0 if it cannot be read.
    pub fn len(path: &Path) -> u64 {
        std::fs::metadata(path).map_or(0, |m| m.len())
    }

    /// The file's text as a client holds it: without a byte-order mark.
    pub fn read_text(path: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(path).map(borzoi::disk_text::without_bom)
    }
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .map(|v| v.parse().unwrap_or_else(|_| panic!("{name} is a number")))
}

fn default_project() -> PathBuf {
    let cache = std::env::var_os("BORZOI_PROJECT_CORPUS_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set"))
                .join(".cache/borzoi/project-corpus")
        });
    cache.join(
        "v2/project-corpus/Smaug123/WoofWare.Incremental/WoofWare.Incremental/WoofWare.Incremental.fsproj",
    )
}

// ---------------------------------------------------------------------------
// The report.
// ---------------------------------------------------------------------------

fn columns(cost: &Cost) -> String {
    let instructions = cost
        .instructions
        .map_or_else(|| "n/a".to_string(), |i| i.to_string());
    let [e_opens, e_stats, e_listings, e_canon, e_reads] = cost.evaluation;
    let [v_opens, v_stats, v_listings, v_canon, o_listings, o_stats] = cost.validation;
    format!(
        "{instructions:>14} {:>10} {:>13} {:>6} {:>6} {e_opens:>7} {e_stats:>7} {e_listings:>7} {e_canon:>7} {e_reads:>7} {v_opens:>7} {v_stats:>7} {v_listings:>7} {v_canon:>7} {o_listings:>7} {o_stats:>7}",
        cost.allocations, cost.allocated_bytes, cost.opens, cost.stats,
    )
}

fn header() -> String {
    format!(
        "{:<34} {:>14} {:>10} {:>13} {:>6} {:>6} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "message",
        "instructions",
        "allocs",
        "alloc-bytes",
        "opens",
        "stats",
        "e.open",
        "e.stat",
        "e.list",
        "e.canon",
        "e.reads",
        "v.open",
        "v.stat",
        "v.list",
        "v.canon",
        "o.list",
        "o.stat"
    )
}

/// The cheapest and dearest of `costs`, counter by counter.
fn min_max(costs: &[Cost]) -> (Cost, Cost) {
    let pick = |f: fn(u64, u64) -> u64| {
        let mut costs = costs.iter().copied();
        let first = costs.next().expect("at least one keystroke");
        costs.fold(first, |a, b| Cost {
            lookups: (f(a.lookups.0, b.lookups.0), f(a.lookups.1, b.lookups.1)),
            instructions: a.instructions.zip(b.instructions).map(|(x, y)| f(x, y)),
            allocations: f(a.allocations, b.allocations),
            allocated_bytes: f(a.allocated_bytes, b.allocated_bytes),
            opens: f(a.opens, b.opens),
            stats: f(a.stats, b.stats),
            evaluation: std::array::from_fn(|i| f(a.evaluation[i], b.evaluation[i])),
            validation: std::array::from_fn(|i| f(a.validation[i], b.validation[i])),
        })
    };
    (pick(u64::min), pick(u64::max))
}

fn main() {
    CLIENT.set(true);
    let project = std::env::var_os("EDIT_COUNTS_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(default_project);
    let project = std::path::absolute(&project).expect("an absolute project path");
    assert!(
        client_fs::is_file(&project),
        "{} does not exist (materialise the corpus with `bash tools/ci/project-corpus-gate.sh`)",
        project.display()
    );
    let mode = match std::env::var("EDIT_COUNTS_CLIENT").as_deref() {
        Ok("push") => Mode::Push,
        Ok("pull") | Err(_) => Mode::Pull,
        Ok(other) => panic!("EDIT_COUNTS_CLIENT is `pull` or `push`, not {other:?}"),
    };
    let keystrokes = env_usize("EDIT_COUNTS_KEYSTROKES").unwrap_or(5);
    assert!(keystrokes > 0, "EDIT_COUNTS_KEYSTROKES must be positive");

    // The Compile order, read by an evaluator of the client's own (the
    // server's is not shared, and evaluating here costs the server nothing).
    let compile: Vec<PathBuf> = Workspace::new()
        .project(&project)
        .expect("the project evaluates")
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
    let is_impl = |i: usize| compile[i].extension().is_some_and(|x| x == "fs");
    let early = env_usize("EDIT_COUNTS_EARLY").unwrap_or_else(|| {
        (compile.len() / 8..compile.len())
            .find(|&i| is_impl(i))
            .expect("an .fs item")
    });
    let late = env_usize("EDIT_COUNTS_LATE").unwrap_or_else(|| {
        (0..compile.len())
            .rev()
            .find(|&i| is_impl(i))
            .expect("an .fs item")
    });
    assert!(
        early < late,
        "E (index {early}) must precede L (index {late})"
    );
    let bytes: u64 = compile.iter().map(|p| client_fs::len(p)).sum();
    let doc = |index: usize| {
        let path = &compile[index];
        Doc {
            uri: Uri::from_file_path(path).expect("a file URI"),
            text: client_fs::read_text(path).expect("read a Compile item"),
        }
    };
    let mut e = doc(early);
    let l = doc(late);
    let relative = |p: &Path| {
        p.strip_prefix(project.parent().expect("a project directory"))
            .unwrap_or(p)
            .display()
            .to_string()
    };
    println!("project {}", project.display());
    println!(
        "{} Compile items, {bytes} bytes; E = [{early}] {}, L = [{late}] {}; {mode:?} client, {keystrokes} keystrokes",
        compile.len(),
        relative(&compile[early]),
        relative(&compile[late]),
    );
    println!();

    let root = project
        .parent()
        .and_then(Path::parent)
        .expect("a repository directory")
        .to_path_buf();
    PROJECT_SPELLING
        .set(ProjectId::new(&project).as_path().display().to_string())
        .expect("set once");
    tracing::subscriber::set_global_default(FoldObserver)
        .expect("no other subscriber is installed");

    let mut client = Client::start(mode, root);
    let hover_at = hover_position(&l.text)
        .unwrap_or_else(|| panic!("{} has no `let` binding to hover", relative(&compile[late])));

    println!("{}", header());
    let mut result_ids = BTreeMap::new();
    let once = |client: &mut Client, label: &str, step: Step| {
        let (cost, result) = client.measure(&step);
        println!("{label:<34} {}", columns(&cost));
        result
    };
    once(&mut client, "didOpen E", did_open(&e));
    once(&mut client, "didOpen L", did_open(&l));
    if mode == Mode::Pull {
        let report = once(
            &mut client,
            "diagnostic E (cold)",
            diagnostic(&e, &result_ids),
        );
        remember_result_id(&mut result_ids, &e, &report);
        let report = once(
            &mut client,
            "diagnostic L (cold)",
            diagnostic(&l, &result_ids),
        );
        remember_result_id(&mut result_ids, &l, &report);
    }
    // Each request must take the path being measured: tokens from the
    // project fold, and a hover that finds something. A `null` answer is the
    // single-file fallback, whose counts would describe a different server.
    for (label, doc) in [
        ("semanticTokens E (cold)", &e),
        ("semanticTokens L (cold)", &l),
    ] {
        let tokens = once(&mut client, label, semantic_tokens(doc));
        assert!(
            tokens["data"]
                .as_array()
                .is_some_and(|data| !data.is_empty()),
            "{label} returned no tokens: {tokens}"
        );
    }
    assert!(
        ENV_BUILDS.load(Relaxed) > 0,
        "the cold pass built no env, so the project's references never resolved"
    );
    let answer = once(&mut client, "hover L (cold)", hover(&l, hover_at));
    assert!(
        !answer.is_null(),
        "hover L at {hover_at:?} found nothing, so it measures the single-file fallback"
    );
    println!();

    // One keystroke: what a pull client sends after it, in order. The barrier
    // row is a bare settle, the floor every other row includes; it is not
    // part of the keystroke's total.
    let mut barriers = Vec::new();
    let mut rows: BTreeMap<usize, (&str, Vec<Cost>)> = BTreeMap::new();
    let mut totals = Vec::new();
    e.text.push_str("\n// edit_counts ");
    for k in 0..keystrokes {
        barriers.push(client.measure(&Step::Barrier).0);
        e.text.push('x');
        let mut total: Option<Cost> = None;
        let mut record = |i: usize, label: &'static str, cost: Cost| {
            rows.entry(i)
                .or_insert_with(|| (label, Vec::new()))
                .1
                .push(cost);
            total = Some(total.map_or(cost, |t: Cost| t.add(cost)));
        };
        let mut i = 0;
        let mut next = |client: &mut Client, label: &'static str, step: Step| {
            let (cost, result) = client.measure(&step);
            record(i, label, cost);
            i += 1;
            result
        };
        next(&mut client, "didChange E", did_change(&e, k as i32 + 1));
        if mode == Mode::Pull {
            let report = next(&mut client, "diagnostic E", diagnostic(&e, &result_ids));
            remember_result_id(&mut result_ids, &e, &report);
            let report = next(&mut client, "diagnostic L", diagnostic(&l, &result_ids));
            remember_result_id(&mut result_ids, &l, &report);
        }
        next(&mut client, "semanticTokens E", semantic_tokens(&e));
        next(&mut client, "semanticTokens L", semantic_tokens(&l));
        let answer = next(&mut client, "hover L", hover(&l, hover_at));
        assert!(
            !answer.is_null(),
            "hover L found nothing after keystroke {k}"
        );
        drop(next);
        drop(record);
        totals.push(total.expect("a keystroke sends something"));
    }
    println!("per keystroke (min, then max, over {keystrokes}):");
    let (min, max) = min_max(&barriers);
    println!("{:<34} {}", "barrier min", columns(&min));
    println!("{:<34} {}", "barrier max", columns(&max));
    for (label, costs) in rows.values() {
        let (min, max) = min_max(costs);
        println!("{:<34} {}", format!("{label} min"), columns(&min));
        println!("{:<34} {}", format!("{label} max"), columns(&max));
    }
    let (min, max) = min_max(&totals);
    println!("{:<34} {}", "keystroke total min", columns(&min));
    println!("{:<34} {}", "keystroke total max", columns(&max));

    client.shut_down();
}
