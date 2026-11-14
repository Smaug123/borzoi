//! Coalescing in the dispatch loop (`docs/incremental-lsp-plan.md`, Stage 8),
//! held to the same messages handled one at a time.
//!
//! Two sessions over one world: **coalescing**, handed each generated batch
//! of messages at once ([`Dispatcher::admit`], as the loop reads everything
//! waiting before it handles anything), and **sequential**, handed the same
//! messages one per batch, which is the loop as it behaved before Stage 8.
//! After every batch:
//!
//! - **Exactly once.** Every request before an `exit` has exactly one answer,
//!   and nothing else is answered.
//! - **Its fate is the specification's.** A request a later `$/cancelRequest`
//!   in the batch names is answered `RequestCancelled`; otherwise one whose
//!   document a later text sync in the batch changes is answered
//!   `ContentModified`; one after `shutdown` is refused `InvalidRequest`;
//!   every other one is answered with a result. The model states this per
//!   message by scanning the batch, from the file each URI names.
//! - **A refusal runs nothing, and identical requests run once.** The
//!   coalescing session opens one `lsp.request` span per request it answers
//!   with a result, except that requests of a shareable method with equal
//!   parameters open one between them, and none for a refusal.
//! - **A superseded sync publishes nothing.** The coalescing session computes
//!   a document's push diagnostics (`publish_diagnostics`) exactly as often as
//!   the sequential one less once per `didOpen` or `didChange` that a later
//!   sync of the same URI in the batch supersedes.
//! - **No answer is computed against a superseded text.** Every answer with a
//!   result equals the sequential session's answer to the same request asked
//!   after the whole batch: the texts it read are the newest the client sent.
//!   That holds for an answer shared from another request's run as for one
//!   computed for the request itself.
//! - **The final state is the sequential one.** The diagnostics each session
//!   last published per URI agree, and so does every handler's answer at
//!   every identifier of every open document, every document's diagnostics,
//!   and `workspace/symbol`.
//!
//! Some batches are instead handed to the coalescing session in chunks, with
//! steps run between them, as the loop reads messages that arrive while it
//! works: those batches are held to exactly once and to the final state.
//!
//! Others arrive **while a request runs** (Stage 9): a prefix is handed over,
//! and the rest is sent through the inbox at a generated checkpoint of
//! whichever request is running then, as the input thread hands over a
//! message that arrives mid-request. The running request stops at that
//! checkpoint and is planned again with the rest. Such a batch is held to
//! exactly once and to the final state, and every request answered after
//! the rest arrived is held to the whole batch's specification: its fate,
//! and, answered with a result, the sequential session's answer after the
//! whole batch.
//!
//! The world is one SDK-less project of three sources, each reading the one
//! before it (`A.x`, `B.y`), so an edit to one changes what requests on the
//! others answer; `A` is also opened under a second URI, path-equal to the
//! first. Disk writes land between batches, seen by both sessions.
//!
//! The default run is a fixed seed and case count; set
//! `BORZOI_COALESCING_CASES` and `BORZOI_COALESCING_SEED` for a soak.

use crate::common::host_fs::HostPath as _;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use borzoi::inbox::{CHECKPOINT_EVENT, STOPPED_EVENT};
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::server::{Dispatcher, Progress, SERVER_STACK_BYTES, State};
use borzoi::uri::UriExt as _;
use borzoi::workspace::Workspace;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{ClientCapabilities, Uri};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestError, TestRng, TestRunner};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// The world.
// ---------------------------------------------------------------------------

/// A source of the project, in Compile order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Src {
    A,
    B,
    C,
}

const SOURCES: [Src; 3] = [Src::A, Src::B, Src::C];

impl Src {
    fn file(self) -> &'static str {
        match self {
            Src::A => "A.fs",
            Src::B => "B.fs",
            Src::C => "C.fs",
        }
    }

    /// The texts a source can hold, on disk or in a buffer. Each version of
    /// `A` gives `x` another type or position, which moves what hover,
    /// definition and references answer in `B` and `C`; some versions do not
    /// parse, which moves the diagnostics.
    fn versions(self) -> &'static [&'static str] {
        match self {
            Src::A => &[
                "module A\n\nlet x = 1\n",
                "module A\n\nlet x = \"one\"\n",
                "module A\n\n// moved\n\nlet x = 1.0\n",
                "module A\n\nlet x = [ 1 ]\nlet w = x\n",
                "module A\n\nlet x = (\n",
            ],
            Src::B => &[
                "module B\n\nlet y = A.x\n",
                "module B\n\nlet y = A.x\nlet v = y\n",
                "module B\n\n\nlet y = (A.x, 2)\n",
                "module B\n\nlet y = A.x +\n",
                "module B\n\nlet y = 1\n",
            ],
            Src::C => &[
                "module C\n\nlet z = B.y\n",
                "module C\n\nlet z = (B.y, A.x)\n",
                "module C\n\nlet z = B.\n",
                "module C\n\n// c\nlet z = B.y\n",
            ],
        }
    }

    fn text(self, version: usize) -> &'static str {
        let versions = self.versions();
        versions[version % versions.len()]
    }
}

/// A document the client can open: each source, and `A` again under a second
/// URI path-equal to the first (by case on a case-insensitive platform, by an
/// escaped letter elsewhere).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Doc {
    Src(Src),
    AliasA,
}

const DOCS: [Doc; 4] = [
    Doc::Src(Src::A),
    Doc::Src(Src::B),
    Doc::Src(Src::C),
    Doc::AliasA,
];

impl Doc {
    /// The source whose file this document names.
    fn src(self) -> Src {
        match self {
            Doc::Src(src) => src,
            Doc::AliasA => Src::A,
        }
    }
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    /// What each source holds on disk.
    disk: BTreeMap<Src, usize>,
}

impl World {
    fn new() -> World {
        let dir = tempfile::Builder::new()
            .prefix("dispatch-coalescing")
            .tempdir()
            .expect("a temporary directory");
        // Canonical, so the server's spelling of every path is the test's.
        let root = dir.path().host_canonicalize().expect("canonical root");
        let project = root.join("P");
        std::fs::create_dir_all(&project).unwrap();
        let includes: String = SOURCES
            .iter()
            .map(|s| format!("    <Compile Include=\"{}\" />\n", s.file()))
            .collect();
        std::fs::write(
            project.join("P.fsproj"),
            format!("<Project>\n  <ItemGroup>\n{includes}  </ItemGroup>\n</Project>\n"),
        )
        .unwrap();
        let mut world = World {
            _dir: dir,
            root,
            disk: BTreeMap::new(),
        };
        for src in SOURCES {
            world.write(src, 0);
        }
        world
    }

    fn path(&self, src: Src) -> PathBuf {
        self.root.join("P").join(src.file())
    }

    fn write(&mut self, src: Src, version: usize) {
        std::fs::write(self.path(src), src.text(version)).unwrap();
        self.disk.insert(src, version);
    }

    fn uri(&self, doc: Doc) -> String {
        let uri = Uri::from_file_path(self.path(doc.src())).unwrap();
        match doc {
            Doc::Src(_) => uri.as_str().to_string(),
            Doc::AliasA => {
                let spelling = uri.as_str();
                let (dir, file) = spelling.rsplit_once('/').unwrap();
                assert_eq!(file, "A.fs");
                if cfg!(any(windows, target_os = "macos")) {
                    format!("{dir}/a.fs")
                } else {
                    format!("{dir}/%41.fs")
                }
            }
        }
    }

    fn project_uri(&self) -> String {
        Uri::from_file_path(self.root.join("P").join("P.fsproj"))
            .unwrap()
            .as_str()
            .to_string()
    }
}

// ---------------------------------------------------------------------------
// Scripts.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Hover,
    Definition,
    References,
    Completion,
    SemanticTokens,
    DocumentSymbol,
    Diagnostic,
}

/// A client action. Each becomes one message when the script is played; what
/// message depends on what the client holds then.
#[derive(Clone, Debug)]
enum Op {
    /// `didOpen` if the client has the document closed, else `didChange`.
    Sync(Doc, usize),
    /// `didClose` if the client has the document open, else `didSave`.
    Close(Doc),
    /// `didSave`, which the server ignores.
    Save(Doc),
    /// A watched-file event naming a source, or the project file (`None`).
    Watched(Option<Src>),
    /// A request on a document, at its `n`-th identifier.
    Request(Kind, Doc, usize),
    WorkspaceSymbol,
    /// `workspace/diagnostic`, which starts a sweep.
    WorkspaceDiagnostic,
    /// Cancel the `n`-th latest request this batch has sent so far, or, if
    /// it has sent none, one an earlier batch sent.
    CancelRecent(usize),
    /// Cancel an id no request carries.
    CancelUnknown,
    /// Send the `n`-th latest document request this batch has sent again,
    /// with the same method and parameters and a new id; `workspace/symbol`
    /// if it has sent none.
    Repeat(usize),
    Shutdown,
    Exit,
}

#[derive(Clone, Debug)]
struct Batch {
    /// Disk writes before the batch.
    writes: Vec<(Src, usize)>,
    ops: Vec<Op>,
    /// When non-empty, the coalescing session is handed the batch in chunks of
    /// these sizes (the last takes the rest), each followed by this many steps.
    chunks: Vec<(usize, usize)>,
    /// When set (and `chunks` is empty), the coalescing session is handed the
    /// first `prefix` messages, and the rest arrives at its `checkpoint`th
    /// checkpoint from then on, while a request runs.
    mid_run: Option<(usize, usize)>,
}

#[derive(Clone)]
struct Case {
    push: bool,
    batches: Vec<Batch>,
}

impl fmt::Debug for Case {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "push: {}", self.push)?;
        for (i, batch) in self.batches.iter().enumerate() {
            writeln!(f, "batch {i}: writes {:?}", batch.writes)?;
            if !batch.chunks.is_empty() {
                writeln!(f, "  chunks {:?}", batch.chunks)?;
            }
            if let Some(mid_run) = batch.mid_run {
                writeln!(f, "  mid-run (prefix, checkpoint) {mid_run:?}")?;
            }
            for op in &batch.ops {
                writeln!(f, "  {op:?}")?;
            }
        }
        Ok(())
    }
}

fn doc_strategy() -> impl Strategy<Value = Doc> {
    prop_oneof![
        3 => Just(Doc::Src(Src::A)),
        3 => Just(Doc::Src(Src::B)),
        2 => Just(Doc::Src(Src::C)),
        1 => Just(Doc::AliasA),
    ]
}

fn kind_strategy() -> impl Strategy<Value = Kind> {
    prop_oneof![
        3 => Just(Kind::Hover),
        2 => Just(Kind::Definition),
        1 => Just(Kind::References),
        1 => Just(Kind::Completion),
        2 => Just(Kind::SemanticTokens),
        1 => Just(Kind::DocumentSymbol),
        2 => Just(Kind::Diagnostic),
    ]
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        8 => (doc_strategy(), 0usize..5).prop_map(|(d, v)| Op::Sync(d, v)),
        2 => doc_strategy().prop_map(Op::Close),
        1 => doc_strategy().prop_map(Op::Save),
        1 => proptest::option::weighted(0.7, prop_oneof![Just(Src::A), Just(Src::B), Just(Src::C)])
            .prop_map(Op::Watched),
        10 => (kind_strategy(), doc_strategy(), 0usize..6).prop_map(|(k, d, n)| Op::Request(k, d, n)),
        1 => Just(Op::WorkspaceSymbol),
        1 => Just(Op::WorkspaceDiagnostic),
        3 => (0usize..3).prop_map(Op::CancelRecent),
        1 => Just(Op::CancelUnknown),
        4 => (0usize..3).prop_map(Op::Repeat),
    ]
}

fn batch_strategy(last: bool) -> impl Strategy<Value = Batch> {
    let src = prop_oneof![Just(Src::A), Just(Src::B), Just(Src::C)];
    let tail = if last {
        // The last batch may end the session.
        prop_oneof![
            8 => Just(vec![]),
            1 => proptest::collection::vec(op_strategy(), 0..3)
                .prop_map(|mut after| { after.insert(0, Op::Shutdown); after }),
            1 => proptest::collection::vec(op_strategy(), 0..3).prop_map(|mut after| {
                after.insert(0, Op::Shutdown);
                after.push(Op::Exit);
                after.push(Op::WorkspaceSymbol);
                after
            }),
            1 => Just(vec![Op::Exit, Op::WorkspaceSymbol]),
        ]
        .boxed()
    } else {
        Just(vec![]).boxed()
    };
    (
        proptest::collection::vec((src, 0usize..5), 0..2),
        proptest::collection::vec(op_strategy(), 1..14),
        tail,
        prop_oneof![
            3 => Just((vec![], None)),
            1 => proptest::collection::vec((1usize..5, 0usize..4), 1..4)
                .prop_map(|chunks| (chunks, None)),
            3 => (1usize..8, 0usize..4).prop_map(|mid_run| (vec![], Some(mid_run))),
        ],
    )
        .prop_map(|(writes, mut ops, tail, (chunks, mid_run))| {
            ops.extend(tail);
            Batch {
                writes,
                ops,
                chunks,
                mid_run,
            }
        })
}

fn case_strategy() -> impl Strategy<Value = Case> {
    (any::<bool>(), 1usize..4).prop_flat_map(|(push, n)| {
        let batches: Vec<_> = (0..n).map(|i| batch_strategy(i + 1 == n).boxed()).collect();
        batches.prop_map(move |batches| Case { push, batches })
    })
}

// ---------------------------------------------------------------------------
// The client, as a model.
// ---------------------------------------------------------------------------

/// What the client holds: the open documents' texts.
#[derive(Default, Clone)]
struct ClientModel {
    open: BTreeMap<Doc, usize>,
}

/// The identifier positions of `text`, as (line, UTF-16 column).
fn identifiers(text: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for (line, content) in text.lines().enumerate() {
        let mut chars = content.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c.is_alphabetic() || c == '_' {
                out.push((line as u32, content[..i].encode_utf16().count() as u32));
                while chars
                    .peek()
                    .is_some_and(|(_, c)| c.is_alphanumeric() || *c == '_')
                {
                    chars.next();
                }
            }
        }
    }
    out
}

/// What the model knows of a played request.
#[derive(Clone)]
struct Req {
    id: RequestId,
    doc: Option<Src>,
    shutdown: bool,
    sweep: bool,
}

/// A played message, with what the model knows of it.
struct Played {
    message: Message,
    /// For a request: its id, the document it names (by file), and whether
    /// it is a `shutdown` or a `workspace/diagnostic`.
    request: Option<Req>,
    /// For a text sync: the URI and the file it changes, and whether it
    /// publishes (an open or a change, not a close).
    sync: Option<(String, Src, bool)>,
    /// For a cancellation: the id it names.
    cancels: Option<RequestId>,
    exit: bool,
}

struct Player<'w> {
    world: &'w World,
    client: ClientModel,
    next_id: u64,
    /// Every request id sent, by earlier batches too.
    sent: Vec<RequestId>,
    /// The document requests this batch has sent: method, parameters and
    /// document.
    asked: Vec<(String, Value, Doc)>,
}

impl Player<'_> {
    fn text(&self, doc: Doc) -> &'static str {
        match self.client.open.get(&doc) {
            Some(&v) => doc.src().text(v),
            None => doc.src().text(self.world.disk[&doc.src()]),
        }
    }

    fn request(&mut self, method: &str, params: Value, doc: Option<Doc>) -> Played {
        let id = RequestId::from(format!("q{}", self.next_id));
        self.next_id += 1;
        self.sent.push(id.clone());
        Played {
            message: Message::Request(Request {
                id: id.clone(),
                method: method.to_string(),
                params,
            }),
            request: Some(Req {
                id,
                doc: doc.map(Doc::src),
                shutdown: method == "shutdown",
                sweep: method == "workspace/diagnostic",
            }),
            sync: None,
            cancels: None,
            exit: false,
        }
    }

    fn notification(method: &str, params: Value) -> Played {
        Played {
            message: Message::Notification(Notification {
                method: method.to_string(),
                params,
            }),
            request: None,
            sync: None,
            cancels: None,
            exit: method == "exit",
        }
    }

    fn play(&mut self, op: &Op, batch_start: usize) -> Played {
        let w = self.world;
        match *op {
            Op::Sync(doc, version) => {
                let uri = w.uri(doc);
                let text = doc.src().text(version);
                let mut played = if self.client.open.contains_key(&doc) {
                    Player::notification(
                        "textDocument/didChange",
                        json!({
                            "textDocument": { "uri": uri, "version": 1 },
                            "contentChanges": [{ "text": text }],
                        }),
                    )
                } else {
                    Player::notification(
                        "textDocument/didOpen",
                        json!({ "textDocument": {
                            "uri": uri, "languageId": "fsharp", "version": 0, "text": text,
                        }}),
                    )
                };
                self.client.open.insert(doc, version);
                played.sync = Some((uri, doc.src(), true));
                played
            }
            Op::Close(doc) if self.client.open.remove(&doc).is_some() => {
                let uri = w.uri(doc);
                let mut played = Player::notification(
                    "textDocument/didClose",
                    json!({ "textDocument": { "uri": uri } }),
                );
                played.sync = Some((uri, doc.src(), false));
                played
            }
            Op::Close(doc) | Op::Save(doc) => Player::notification(
                "textDocument/didSave",
                json!({ "textDocument": { "uri": w.uri(doc) } }),
            ),
            Op::Watched(src) => {
                let uri = src.map_or_else(|| w.project_uri(), |s| w.uri(Doc::Src(s)));
                Player::notification(
                    "workspace/didChangeWatchedFiles",
                    json!({ "changes": [{ "uri": uri, "type": 2 }] }),
                )
            }
            Op::Request(kind, doc, n) => {
                let uri = w.uri(doc);
                let ids = identifiers(self.text(doc));
                let (line, character) = ids[n % ids.len()];
                let position = json!({ "line": line, "character": character });
                let document = json!({ "uri": uri });
                let (method, params) = match kind {
                    Kind::Hover => (
                        "textDocument/hover",
                        json!({ "textDocument": document, "position": position }),
                    ),
                    Kind::Definition => (
                        "textDocument/definition",
                        json!({ "textDocument": document, "position": position }),
                    ),
                    Kind::References => (
                        "textDocument/references",
                        json!({
                            "textDocument": document, "position": position,
                            "context": { "includeDeclaration": true },
                        }),
                    ),
                    Kind::Completion => (
                        "textDocument/completion",
                        json!({ "textDocument": document, "position": position }),
                    ),
                    Kind::SemanticTokens => (
                        "textDocument/semanticTokens/full",
                        json!({ "textDocument": document }),
                    ),
                    Kind::DocumentSymbol => (
                        "textDocument/documentSymbol",
                        json!({ "textDocument": document }),
                    ),
                    Kind::Diagnostic => (
                        "textDocument/diagnostic",
                        json!({ "textDocument": document }),
                    ),
                };
                self.asked.push((method.to_string(), params.clone(), doc));
                self.request(method, params, Some(doc))
            }
            Op::Repeat(n) => match self.asked.iter().rev().nth(n % self.asked.len().max(1)) {
                Some((method, params, doc)) => {
                    let (method, params, doc) = (method.clone(), params.clone(), *doc);
                    self.request(&method, params, Some(doc))
                }
                None => self.request("workspace/symbol", json!({ "query": "" }), None),
            },
            Op::WorkspaceSymbol => self.request("workspace/symbol", json!({ "query": "" }), None),
            Op::WorkspaceDiagnostic => self.request(
                "workspace/diagnostic",
                json!({ "previousResultIds": [] }),
                None,
            ),
            Op::CancelRecent(n) => {
                let recent = &self.sent[batch_start..];
                let target = if recent.is_empty() {
                    self.sent.iter().rev().nth(n)
                } else {
                    recent.iter().rev().nth(n % recent.len())
                };
                let id = target.cloned().unwrap_or_else(|| RequestId::from(-1));
                let mut played = Player::notification(
                    "$/cancelRequest",
                    json!({ "id": serde_json::to_value(&id).unwrap() }),
                );
                played.cancels = Some(id);
                played
            }
            Op::CancelUnknown => {
                let mut played =
                    Player::notification("$/cancelRequest", json!({ "id": "never-sent" }));
                played.cancels = Some(RequestId::from("never-sent".to_string()));
                played
            }
            Op::Shutdown => self.request("shutdown", Value::Null, None),
            Op::Exit => Player::notification("exit", Value::Null),
        }
    }
}

/// A request's fate, as the specification states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    Answered,
    Cancelled,
    ContentModified,
    AfterShutdown,
    Shutdown,
    /// A `workspace/diagnostic` that ran, and that a later one in the batch,
    /// or the `shutdown` ending its live part, answered `ServerCancelled`.
    Superseded,
    /// A `workspace/diagnostic` that ran and that the `exit` ending the live
    /// part abandons unanswered, as it abandons any sweep in flight.
    Abandoned,
    /// After `exit`: never read.
    Unread,
}

impl Fate {
    /// Whether the request runs (opens an `lsp.request` span).
    fn runs(self) -> bool {
        matches!(self, Fate::Answered | Fate::Superseded | Fate::Abandoned)
    }

    /// How many answers it gets.
    fn answers(self) -> usize {
        usize::from(!matches!(self, Fate::Abandoned | Fate::Unread))
    }
}

/// The index of a batch's first `shutdown` or `exit`, from which the lifecycle
/// rules apply; 0 once `shutdown` was acknowledged before it.
fn barrier(batch: &[Played], shut_down: bool) -> usize {
    if shut_down {
        0
    } else {
        batch
            .iter()
            .position(|p| p.exit || p.request.as_ref().is_some_and(|r| r.shutdown))
            .unwrap_or(batch.len())
    }
}

/// Each request's fate in a batch handed over whole, by scanning the batch:
/// `shut_down` is whether `shutdown` was acknowledged before it.
fn fates(batch: &[Played], shut_down: bool) -> BTreeMap<RequestId, Fate> {
    let barrier = barrier(batch, shut_down);
    // The fate of a live request, sweeps aside.
    let refused = |i: usize, req: &Req| {
        let later = &batch[i + 1..barrier];
        if later.iter().any(|p| p.cancels.as_ref() == Some(&req.id)) {
            Some(Fate::Cancelled)
        } else if req.doc.is_some_and(|doc| {
            later
                .iter()
                .any(|p| p.sync.as_ref().is_some_and(|s| s.1 == doc))
        }) {
            Some(Fate::ContentModified)
        } else {
            None
        }
    };
    let ends_with_exit = batch.get(barrier).is_some_and(|p| p.exit);
    let mut out = BTreeMap::new();
    let mut exited = false;
    for (i, played) in batch.iter().enumerate() {
        exited |= played.exit;
        let Some(req) = &played.request else {
            continue;
        };
        let fate = if exited {
            Fate::Unread
        } else if req.shutdown {
            Fate::Shutdown
        } else if i >= barrier {
            Fate::AfterShutdown
        } else if let Some(fate) = refused(i, req) {
            fate
        } else if req.sweep {
            let superseded = batch[i + 1..barrier].iter().enumerate().any(|(j, p)| {
                p.request
                    .as_ref()
                    .is_some_and(|r| r.sweep && refused(i + 1 + j, r).is_none())
            });
            if superseded || (barrier < batch.len() && !ends_with_exit) {
                Fate::Superseded
            } else if ends_with_exit {
                Fate::Abandoned
            } else {
                Fate::Answered
            }
        } else {
            Fate::Answered
        };
        out.insert(req.id.clone(), fate);
    }
    out
}

/// How many `publish_diagnostics` computations each URI is spared in a batch
/// handed over whole: one per open or change a later live sync of the same URI
/// supersedes.
fn spared_publishes(batch: &[Played], shut_down: bool) -> BTreeMap<String, usize> {
    let barrier = barrier(batch, shut_down);
    let mut out = BTreeMap::new();
    for (i, played) in batch[..barrier].iter().enumerate() {
        if let Some((uri, _, true)) = &played.sync
            && batch[i + 1..barrier]
                .iter()
                .any(|p| p.sync.as_ref().is_some_and(|s| &s.0 == uri))
        {
            *out.entry(uri.clone()).or_default() += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// A session.
// ---------------------------------------------------------------------------

/// What a `tracing` subscriber saw the dispatcher do: requests run, and push
/// diagnostics computed per URI.
#[derive(Default, Debug)]
struct Seen {
    requests_run: usize,
    publishes: BTreeMap<String, usize>,
    /// Checkpoints passed.
    checkpoints: usize,
    /// Runs that stopped while they were to answer other requests too.
    shared_stops: usize,
}

struct Counter(Arc<Mutex<Seen>>, Option<Arc<Mutex<MidRun>>>);

/// A batch's rest, waiting to be sent through the inbox at a checkpoint.
struct MidRun {
    /// Checkpoints to pass before sending.
    countdown: usize,
    /// A file to write, with what, just before the rest is sent.
    write: Option<(PathBuf, &'static str)>,
    rest: Option<Vec<Message>>,
    input: borzoi::inbox::InboxSender,
    client: crossbeam_channel::Receiver<Message>,
    /// How many messages the server had sent the client when the rest was
    /// sent: those were sent before it arrived.
    sent_before: Option<usize>,
}

impl MidRun {
    /// At a checkpoint: send the rest once the countdown runs out.
    fn checkpoint(&mut self) {
        if self.rest.is_none() {
            return;
        }
        if self.countdown > 0 {
            self.countdown -= 1;
            return;
        }
        self.sent_before = Some(self.client.len());
        if let Some((path, text)) = self.write.take() {
            std::fs::write(path, text).unwrap();
        }
        for message in self.rest.take().unwrap() {
            self.input.send(message).expect("the inbox is alive");
        }
    }
}

struct UriField(Option<String>);

impl tracing::field::Visit for UriField {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "uri" {
            self.0 = Some(value.to_string());
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn fmt::Debug) {}
}

/// The `shared` field of a [`STOPPED_EVENT`]: how many other requests the
/// stopped run was to answer.
struct SharedField(u64);

impl tracing::field::Visit for SharedField {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "shared" {
            self.0 = value;
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn fmt::Debug) {}
}

impl tracing::Subscriber for Counter {
    fn register_callsite(
        &self,
        metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        if self.enabled(metadata) {
            tracing::subscriber::Interest::always()
        } else {
            tracing::subscriber::Interest::sometimes()
        }
    }
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        (metadata.is_span() && matches!(metadata.name(), "lsp.request" | "publish_diagnostics"))
            || (metadata.is_event() && matches!(metadata.name(), CHECKPOINT_EVENT | STOPPED_EVENT))
    }
    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut seen = self.0.lock().unwrap();
        if span.metadata().name() == "lsp.request" {
            seen.requests_run += 1;
        } else {
            let mut uri = UriField(None);
            span.record(&mut uri);
            *seen
                .publishes
                .entry(uri.0.expect("publish_diagnostics names its uri"))
                .or_default() += 1;
        }
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if event.metadata().name() == STOPPED_EVENT {
            let mut shared = SharedField(0);
            event.record(&mut shared);
            if shared.0 > 0 {
                self.0.lock().unwrap().shared_stops += 1;
            }
            return;
        }
        self.0.lock().unwrap().checkpoints += 1;
        if let Some(mid_run) = &self.1 {
            mid_run.lock().unwrap().checkpoint();
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

struct Session {
    dispatcher: Dispatcher,
    server: Connection,
    client: Connection,
    exited: bool,
    next_probe: u64,
    /// The diagnostics last published per URI, less the history-dependent
    /// `version`.
    published: BTreeMap<String, Value>,
    /// The server's requests not yet acknowledged.
    owed_acks: Vec<RequestId>,
    /// The inbox's sending side: a message sent here reaches a request that
    /// is running, as one the input thread hands over does.
    input: borzoi::inbox::InboxSender,
    /// Requests whose runs stopped at a checkpoint.
    stops: usize,
    /// The methods of every request the server sent the client.
    server_requests: Vec<String>,
}

impl Session {
    fn start(root: &Path, push: bool) -> Session {
        let (server, client) = Connection::memory();
        let mut state = State::new();
        // Hermetic: no `dotnet` is found, so no env is built from the host's
        // packs. The project is SDK-less, so it evaluates and folds all the
        // same.
        state.workspace = Workspace::with_env(SdkDiscoveryEnv {
            dotnet_root: Some(root.join("no-dotnet")),
            ..SdkDiscoveryEnv::default()
        });
        let mut caps = json!({
            "workspace": { "semanticTokens": { "refreshSupport": true } },
            "textDocument": {
                "publishDiagnostics": {},
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            },
        });
        if !push {
            caps["textDocument"]["diagnostic"] = json!({});
            caps["workspace"]["diagnostic"] = json!({ "refreshSupport": true });
        }
        let caps: ClientCapabilities = serde_json::from_value(caps).unwrap();
        state.set_client_capabilities(caps);
        state.set_workspace_roots(vec![root.to_path_buf()]);
        let (input, inbox) = borzoi::inbox::channel(borzoi::server::INPUT_BUFFER);
        let dispatcher = Dispatcher::start(&server, inbox, state, None);
        Session {
            dispatcher,
            input,
            server,
            client,
            exited: false,
            next_probe: 0,
            published: BTreeMap::new(),
            owed_acks: Vec::new(),
            stops: 0,
            server_requests: Vec::new(),
        }
    }

    /// Hand `messages` over as one batch, unless the session has ended, and
    /// run until nothing read is left to handle (or `exit`), then the sweep
    /// in flight to its end. Returns the answers to the client's requests.
    fn deliver(&mut self, messages: Vec<Message>) -> BTreeMap<RequestId, Vec<Response>> {
        self.deliver_in(messages, &[])
    }

    /// As [`Self::deliver`], handing `messages` over in chunks of the given
    /// sizes, each followed by the given number of steps.
    fn deliver_in(
        &mut self,
        messages: Vec<Message>,
        chunks: &[(usize, usize)],
    ) -> BTreeMap<RequestId, Vec<Response>> {
        if !self.exited {
            let mut rest = messages.into_iter();
            for &(size, steps) in chunks {
                self.dispatcher.admit(rest.by_ref().take(size));
                for _ in 0..steps {
                    if !matches!(self.step(), Progress::Handled | Progress::Stopped) {
                        break;
                    }
                }
                if self.exited {
                    break;
                }
            }
            if !self.exited {
                self.dispatcher.admit(rest);
                self.run_out();
            }
            while !self.exited && self.dispatcher.sweep_in_flight() {
                self.dispatcher.step_sweep(&self.server).unwrap();
            }
        }
        self.absorb()
    }

    /// Step until nothing read is left to handle, or `exit`.
    fn run_out(&mut self) {
        while matches!(self.step(), Progress::Handled | Progress::Stopped) {}
    }

    /// Hand over the first `prefix` of `messages`, and send the rest through
    /// the inbox at the `checkpoint`th checkpoint from then on (see
    /// [`MidRun`]), then run until nothing is left, as [`Self::deliver`]
    /// does. `seen` counts as [`Counter`] does. Returns the answers, and,
    /// when the rest arrived mid-request, the requests answered before it.
    fn deliver_mid_run(
        &mut self,
        mut messages: Vec<Message>,
        (prefix, checkpoint): (usize, usize),
        seen: Arc<Mutex<Seen>>,
    ) -> (
        BTreeMap<RequestId, Vec<Response>>,
        Option<BTreeSet<RequestId>>,
    ) {
        if self.exited {
            return (self.absorb(), None);
        }
        let rest = messages.split_off(prefix.min(messages.len()));
        let mid_run = Arc::new(Mutex::new(MidRun {
            countdown: checkpoint,
            write: None,
            rest: Some(rest),
            input: self.input.clone(),
            client: self.client.receiver.clone(),
            sent_before: None,
        }));
        crate::common::with_subscriber(Counter(seen, Some(Arc::clone(&mid_run))), || {
            self.dispatcher.admit(messages);
            self.run_out();
            // Fewer checkpoints than the countdown: the rest arrives between
            // steps instead.
            let late = mid_run.lock().unwrap().rest.take();
            if let Some(late) = late
                && !self.exited
            {
                self.dispatcher.admit(late);
                self.run_out();
            }
            while !self.exited && self.dispatcher.sweep_in_flight() {
                self.dispatcher.step_sweep(&self.server).unwrap();
            }
        });
        let sent_before = mid_run.lock().unwrap().sent_before;
        let mut before = BTreeSet::new();
        let mut answers: BTreeMap<RequestId, Vec<Response>> = BTreeMap::new();
        for (position, message) in self
            .client
            .receiver
            .try_iter()
            .collect::<Vec<_>>()
            .into_iter()
            .enumerate()
        {
            if let (Message::Response(response), Some(sent_before)) = (&message, sent_before)
                && position < sent_before
            {
                before.insert(response.id.clone());
            }
            self.absorb_one(message, &mut answers);
        }
        (answers, sent_before.map(|_| before))
    }

    fn step(&mut self) -> Progress {
        let progress = self.dispatcher.step(&self.server).unwrap();
        match progress {
            Progress::Exit => self.exited = true,
            Progress::Stopped => self.stops += 1,
            Progress::Handled | Progress::Idle => {}
        }
        progress
    }

    /// Read everything the server sent: answers are returned, diagnostics
    /// recorded, the server's own requests queued for acknowledgement.
    fn absorb(&mut self) -> BTreeMap<RequestId, Vec<Response>> {
        let mut answers: BTreeMap<RequestId, Vec<Response>> = BTreeMap::new();
        for message in self.client.receiver.try_iter().collect::<Vec<_>>() {
            self.absorb_one(message, &mut answers);
        }
        answers
    }

    fn absorb_one(&mut self, message: Message, answers: &mut BTreeMap<RequestId, Vec<Response>>) {
        match message {
            Message::Response(response) => {
                answers
                    .entry(response.id.clone())
                    .or_default()
                    .push(response);
            }
            Message::Request(request) => {
                self.server_requests.push(request.method);
                self.owed_acks.push(request.id);
            }
            Message::Notification(n) if n.method == "textDocument/publishDiagnostics" => {
                let uri = n.params["uri"].as_str().unwrap().to_string();
                let mut view = n.params.clone();
                view.as_object_mut().unwrap().remove("version");
                self.published.insert(uri, view);
            }
            Message::Notification(_) => {}
        }
    }

    /// Acknowledge every request the server sent, one per batch, until it
    /// sends no more.
    fn settle(&mut self) {
        for _ in 0..8 {
            if self.owed_acks.is_empty() || self.exited {
                return;
            }
            let acks: Vec<Message> = self
                .owed_acks
                .drain(..)
                .map(|id| {
                    Message::Response(Response {
                        id,
                        response_result: Ok(Value::Null),
                    })
                })
                .collect();
            for ack in acks {
                let stray = self.deliver(vec![ack]);
                assert!(
                    stray.is_empty(),
                    "an acknowledgement was answered: {stray:?}"
                );
            }
        }
        panic!("the server kept sending requests");
    }

    /// Ask one request on its own and return its answer.
    fn ask(&mut self, method: &str, params: Value) -> Value {
        let id = RequestId::from(format!("probe{}", self.next_probe));
        self.next_probe += 1;
        let mut answers = self.deliver(vec![Message::Request(Request {
            id: id.clone(),
            method: method.to_string(),
            params,
        })]);
        let mut answer = answers.remove(&id).expect("a probe is answered");
        assert_eq!(answer.len(), 1, "a probe is answered once");
        assert!(answers.is_empty(), "only the probe is answered");
        serde_json::to_value(
            answer
                .remove(0)
                .response_result
                .map_err(|e| (e.code, e.message)),
        )
        .unwrap()
    }
}

// ---------------------------------------------------------------------------
// The property.
// ---------------------------------------------------------------------------

/// What the generated cases exercised, so the run can show it was not
/// vacuous.
#[derive(Default, Debug)]
struct Coverage {
    answered: usize,
    cancelled: usize,
    content_modified: usize,
    after_shutdown: usize,
    /// Answers with a result that a later text sync of another document in
    /// the same batch moved: the case the reordering exists for.
    moved_by_a_later_sync: usize,
    spared_publishes: usize,
    superseded_sweeps: usize,
    chunked_batches: usize,
    /// Batches whose rest arrived while a request ran, and of those, the
    /// ones in which a run stopped.
    mid_run_batches: usize,
    mid_run_stops: usize,
    /// Requests answered after a mid-run arrival and held to the whole
    /// batch's specification, and those answered with a result.
    after_arrival: usize,
    after_arrival_with_a_result: usize,
    /// Answers with a result taken from another request's run, in batches
    /// handed over whole.
    shared_answers: usize,
    /// Runs that stopped while they were to answer other requests too.
    shared_stops: usize,
}

/// What a request must equal another in to share its run, by the model: a
/// method whose handler answers from its parameters and the texts it reads,
/// synchronously, and the parameters themselves.
fn share_key(message: &Message) -> Option<(String, Value)> {
    const SHAREABLE: [&str; 7] = [
        "textDocument/hover",
        "textDocument/completion",
        "textDocument/documentSymbol",
        "textDocument/references",
        "workspace/symbol",
        "textDocument/semanticTokens/full",
        "textDocument/diagnostic",
    ];
    match message {
        Message::Request(request) if SHAREABLE.contains(&request.method.as_str()) => {
            Some((request.method.clone(), request.params.clone()))
        }
        _ => None,
    }
}

fn fail(message: String) -> TestCaseError {
    TestCaseError::fail(message)
}

fn run_case(case: &Case, coverage: &RefCell<Coverage>) -> Result<(), TestCaseError> {
    let mut world = World::new();
    let mut coalescing = Session::start(&world.root, case.push);
    let mut sequential = Session::start(&world.root, case.push);
    let mut client = ClientModel::default();
    let mut next_id = 0;
    let mut sent = Vec::new();
    let mut shut_down = false;
    for (b, batch) in case.batches.iter().enumerate() {
        for &(src, version) in &batch.writes {
            world.write(src, version);
        }
        let mut player = Player {
            world: &world,
            client: client.clone(),
            next_id,
            sent: std::mem::take(&mut sent),
            asked: Vec::new(),
        };
        let batch_start = player.sent.len();
        let played: Vec<Played> = batch
            .ops
            .iter()
            .map(|op| player.play(op, batch_start))
            .collect();
        client = player.client;
        next_id = player.next_id;
        sent = player.sent;
        let messages = |played: &[Played]| -> Vec<Message> {
            played.iter().map(|p| p.message.clone()).collect()
        };
        let whole = batch.chunks.is_empty() && batch.mid_run.is_none();
        let expected = fates(&played, shut_down);

        let seen = Arc::new(Mutex::new(Seen::default()));
        let stops_before = coalescing.stops;
        let (answers, answered_before_arrival) = match batch.mid_run {
            Some(mid_run) if batch.chunks.is_empty() => {
                coalescing.deliver_mid_run(messages(&played), mid_run, Arc::clone(&seen))
            }
            _ => {
                let answers =
                    crate::common::with_subscriber(Counter(Arc::clone(&seen), None), || {
                        coalescing.deliver_in(messages(&played), &batch.chunks)
                    });
                (answers, None)
            }
        };
        let stopped = coalescing.stops - stops_before;
        coverage.borrow_mut().shared_stops += seen.lock().unwrap().shared_stops;
        if whole && stopped > 0 {
            return Err(fail(format!(
                "batch {b}: a batch handed over whole stopped {stopped} runs, with nothing arriving"
            )));
        }
        let seen_coalescing = std::mem::take(&mut *seen.lock().unwrap());
        let mut sequential_answers = BTreeMap::new();
        crate::common::with_subscriber(Counter(Arc::clone(&seen), None), || {
            for message in messages(&played) {
                sequential_answers.extend(sequential.deliver(vec![message]));
            }
        });
        let seen_sequential = std::mem::take(&mut *seen.lock().unwrap());

        // Exactly once, in both sessions. A batch handed over whole is held
        // to the specification's count; in chunks, which sweeps have started
        // when `exit` arrives depends on the schedule, so a sweep in a batch
        // that exits may be abandoned unanswered, and every other request
        // read before `exit` is answered once.
        let sweeps: BTreeSet<&RequestId> = played
            .iter()
            .filter_map(|p| p.request.as_ref().filter(|r| r.sweep).map(|r| &r.id))
            .collect();
        let exits = played.iter().any(|p| p.exit);
        for (session, answers) in [
            ("coalescing", &answers),
            ("sequential", &sequential_answers),
        ] {
            for (id, fate) in &expected {
                let n = answers.get(id).map_or(0, Vec::len);
                let allowed: &[usize] = match (session, whole) {
                    _ if *fate == Fate::Unread => &[0],
                    ("coalescing", true) if fate.answers() == 0 => &[0],
                    ("coalescing", false) if exits && sweeps.contains(id) => &[0, 1],
                    _ => &[1],
                };
                if !allowed.contains(&n) {
                    return Err(fail(format!(
                        "batch {b}: the {session} session answered {id} {n} times, not {allowed:?}: {:?}",
                        answers.get(id)
                    )));
                }
            }
            if let Some(stray) = answers.keys().find(|id| !expected.contains_key(id)) {
                return Err(fail(format!(
                    "batch {b}: the {session} session answered {stray}, which no request in the batch carries"
                )));
            }
        }

        if whole {
            // Each request's fate is the specification's.
            for (id, fate) in &expected {
                let Some(answer) = answers.get(id).map(|a| &a[0]) else {
                    continue;
                };
                let code = answer.response_result.as_ref().err().map(|e| e.code);
                let want = match fate {
                    Fate::Answered | Fate::Shutdown => None,
                    Fate::Cancelled => Some(lsp_server::ErrorCode::RequestCanceled as i32),
                    Fate::ContentModified => Some(lsp_server::ErrorCode::ContentModified as i32),
                    Fate::AfterShutdown => Some(lsp_server::ErrorCode::InvalidRequest as i32),
                    Fate::Superseded => Some(lsp_server::ErrorCode::ServerCancelled as i32),
                    Fate::Abandoned | Fate::Unread => {
                        unreachable!("an abandoned or unread request is not answered")
                    }
                };
                if code != want {
                    return Err(fail(format!(
                        "batch {b}: {id} should be {fate:?}, but was answered {answer:?}"
                    )));
                }
                let mut c = coverage.borrow_mut();
                match fate {
                    Fate::Answered => c.answered += 1,
                    Fate::Cancelled => c.cancelled += 1,
                    Fate::ContentModified => c.content_modified += 1,
                    Fate::AfterShutdown => c.after_shutdown += 1,
                    Fate::Superseded => c.superseded_sweeps += 1,
                    Fate::Shutdown | Fate::Abandoned | Fate::Unread => {}
                }
            }
            // A refusal runs nothing, and the requests answered with a
            // result that are identical share one run.
            let mut alone = 0;
            let mut shared: Vec<(String, Value)> = Vec::new();
            let mut shareable_answers = 0;
            for played in &played {
                let Some(req) = &played.request else {
                    continue;
                };
                let Some(&fate) = expected.get(&req.id) else {
                    continue;
                };
                match share_key(&played.message) {
                    Some(key) if fate == Fate::Answered => {
                        shareable_answers += 1;
                        if !shared.contains(&key) {
                            shared.push(key);
                        }
                    }
                    _ => alone += usize::from(fate.runs()),
                }
            }
            let ran = alone + shared.len();
            coverage.borrow_mut().shared_answers += shareable_answers - shared.len();
            if seen_coalescing.requests_run != ran {
                return Err(fail(format!(
                    "batch {b}: the coalescing session ran {} requests, but the specification runs {ran}",
                    seen_coalescing.requests_run
                )));
            }
            // A superseded sync publishes nothing; every other one does.
            let spared = spared_publishes(&played, shut_down);
            coverage.borrow_mut().spared_publishes += spared.values().sum::<usize>();
            let uris: BTreeSet<&String> = seen_sequential
                .publishes
                .keys()
                .chain(seen_coalescing.publishes.keys())
                .collect();
            for uri in uris {
                let got = seen_coalescing.publishes.get(uri).copied().unwrap_or(0);
                let sequential = seen_sequential.publishes.get(uri).copied().unwrap_or(0);
                let want = sequential.checked_sub(spared.get(uri).copied().unwrap_or(0));
                if Some(got) != want {
                    return Err(fail(format!(
                        "batch {b}: the coalescing session diagnosed {uri} {got} times; \
                         the sequential one {sequential} times, spared {:?}",
                        spared.get(uri)
                    )));
                }
            }
        }
        shut_down |= expected.values().any(|f| *f == Fate::Shutdown);

        coalescing.settle();
        sequential.settle();
        if coalescing.exited != sequential.exited {
            return Err(fail(format!("batch {b}: only one session exited")));
        }
        if coalescing.published != sequential.published {
            return Err(fail(format!(
                "batch {b}: the published diagnostics differ:\ncoalescing {:#?}\nsequential {:#?}",
                coalescing.published, sequential.published
            )));
        }
        if shut_down || coalescing.exited {
            break;
        }

        // No answer with a result was computed against a superseded text: it
        // is the answer to the same request asked after the whole batch.
        if whole {
            for played in &played {
                let (Message::Request(request), Some(Req { id, .. })) =
                    (&played.message, &played.request)
                else {
                    continue;
                };
                let Some(Ok(result)) = answers.get(id).map(|a| &a[0].response_result) else {
                    continue;
                };
                let now = sequential.ask(&request.method, request.params.clone());
                if now != json!({ "Ok": result }) {
                    return Err(fail(format!(
                        "batch {b}: {id} ({}) was answered {result} against superseded text; \
                         after the batch it is {now}",
                        request.method
                    )));
                }
                let at_its_place = sequential_answers.get(id).map(|a| &a[0].response_result);
                if !matches!(at_its_place, Some(Ok(at)) if at == result) {
                    coverage.borrow_mut().moved_by_a_later_sync += 1;
                }
            }
        } else if let Some(before) = &answered_before_arrival {
            // The rest arrived while a request ran: every request answered
            // after it is answered as if the batch had been handed over
            // whole, and against the newest texts.
            let mut c = coverage.borrow_mut();
            c.mid_run_batches += 1;
            if stopped > 0 {
                c.mid_run_stops += 1;
            }
            drop(c);
            for played in &played {
                let (Message::Request(request), Some(Req { id, sweep, .. })) =
                    (&played.message, &played.request)
                else {
                    continue;
                };
                // A sweep's answer depends on which sweeps had started when
                // the rest arrived.
                if *sweep || before.contains(id) {
                    continue;
                }
                let Some(answer) = answers.get(id).map(|a| &a[0]) else {
                    continue;
                };
                let fate = expected[id];
                let code = answer.response_result.as_ref().err().map(|e| e.code);
                let want = match fate {
                    Fate::Answered | Fate::Shutdown => None,
                    Fate::Cancelled => Some(lsp_server::ErrorCode::RequestCanceled as i32),
                    Fate::ContentModified => Some(lsp_server::ErrorCode::ContentModified as i32),
                    Fate::AfterShutdown => Some(lsp_server::ErrorCode::InvalidRequest as i32),
                    Fate::Superseded | Fate::Abandoned | Fate::Unread => continue,
                };
                if code != want {
                    return Err(fail(format!(
                        "batch {b}: {id}, answered after the rest arrived mid-request, should be \
                         {fate:?}, but was answered {answer:?}"
                    )));
                }
                coverage.borrow_mut().after_arrival += 1;
                let Ok(result) = &answer.response_result else {
                    continue;
                };
                if fate == Fate::Shutdown {
                    continue;
                }
                let now = sequential.ask(&request.method, request.params.clone());
                if now != json!({ "Ok": result }) {
                    return Err(fail(format!(
                        "batch {b}: {id} ({}), answered after the rest arrived mid-request, was \
                         answered {result} against superseded text; after the batch it is {now}",
                        request.method
                    )));
                }
                coverage.borrow_mut().after_arrival_with_a_result += 1;
            }
        } else {
            coverage.borrow_mut().chunked_batches += 1;
        }

        // The final state is the sequential one.
        let probes = probes(&world, &client);
        for (method, params) in probes {
            let got = coalescing.ask(&method, params.clone());
            let want = sequential.ask(&method, params.clone());
            if got != want {
                return Err(fail(format!(
                    "batch {b}: after the batch, {method} {params} answers {got} in the \
                     coalescing session and {want} in the sequential one"
                )));
            }
        }
        coalescing.settle();
        sequential.settle();
    }
    Ok(())
}

/// Every request worth comparing after a batch: each document's diagnostics,
/// each open document's tokens and symbols, hover, definition and references
/// at each of its identifiers, and `workspace/symbol`.
fn probes(world: &World, client: &ClientModel) -> Vec<(String, Value)> {
    let mut out = vec![("workspace/symbol".to_string(), json!({ "query": "" }))];
    for doc in DOCS {
        let document = json!({ "uri": world.uri(doc) });
        out.push((
            "textDocument/diagnostic".to_string(),
            json!({ "textDocument": document }),
        ));
        let Some(&version) = client.open.get(&doc) else {
            continue;
        };
        for method in [
            "textDocument/semanticTokens/full",
            "textDocument/documentSymbol",
        ] {
            out.push((method.to_string(), json!({ "textDocument": document })));
        }
        for (line, character) in identifiers(doc.src().text(version)) {
            let position = json!({ "line": line, "character": character });
            for method in ["textDocument/hover", "textDocument/definition"] {
                out.push((
                    method.to_string(),
                    json!({ "textDocument": document, "position": position }),
                ));
            }
            out.push((
                "textDocument/references".to_string(),
                json!({
                    "textDocument": document, "position": position,
                    "context": { "includeDeclaration": true },
                }),
            ));
        }
    }
    out
}

const DEFAULT_CASES: u32 = 48;
const DEFAULT_SEED: u64 = 0xc0a1_e5ce;

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .map(|v| v.parse().unwrap_or_else(|_| panic!("{name} is a number")))
}

fn runner(cases: u32, seed: u64) -> TestRunner {
    let config = Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    };
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes))
}

/// Run `f` on a thread with the dispatch loop's stack, as the server runs.
fn on_server_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(SERVER_STACK_BYTES)
        .spawn(f)
        .expect("spawn")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// Cases found against the property's own oracle, run before the generated
/// ones.
fn pinned_cases() -> Vec<Case> {
    vec![
        // Chunked: both sweeps start before the cancellation arrives, so the
        // second answers the first `ServerCancelled`, and `exit` abandons the
        // second; handed over whole, the first would be abandoned instead
        // (found by Codex).
        Case {
            push: false,
            batches: vec![Batch {
                writes: vec![],
                ops: vec![
                    Op::WorkspaceDiagnostic,
                    Op::WorkspaceDiagnostic,
                    Op::CancelRecent(0),
                    Op::Exit,
                ],
                chunks: vec![(2, 2)],
                mid_run: None,
            }],
        },
    ]
}

/// For all generated interleavings of text syncs, requests and
/// cancellations, each batch handed over at once is answered as the
/// specification says, against the newest texts, and leaves the state the
/// same messages handled one at a time leave (see the module docs).
#[test]
fn a_coalesced_batch_agrees_with_its_messages_one_at_a_time() {
    on_server_stack(|| {
        let cases = env_u64("BORZOI_COALESCING_CASES").map_or(DEFAULT_CASES, |n| n as u32);
        let seed = env_u64("BORZOI_COALESCING_SEED").unwrap_or(DEFAULT_SEED);
        let coverage = RefCell::new(Coverage::default());
        for case in pinned_cases() {
            if let Err(err) = run_case(&case, &coverage) {
                panic!("pinned case failed: {err}\n\n{case:?}");
            }
        }
        let result = runner(cases, seed).run(&case_strategy(), |case| run_case(&case, &coverage));
        let coverage = coverage.into_inner();
        eprintln!("seed {seed}: {coverage:#?}");
        match result {
            Ok(()) => {}
            Err(TestError::Fail(message, case)) => {
                panic!("seed {seed}: {message}\n\nminimal script:\n{case:?}")
            }
            Err(err) => panic!("seed {seed}: {err}"),
        }
        // Non-vacuity: every fate occurred, an answer moved because a later
        // sync of another document ran before it, a superseded sync was
        // spared its diagnostics, and some batch arrived in chunks.
        assert!(
            coverage.answered > 0
                && coverage.cancelled > 0
                && coverage.content_modified > 0
                && coverage.after_shutdown > 0
                && coverage.moved_by_a_later_sync > 0
                && coverage.spared_publishes > 0
                && coverage.chunked_batches > 0
                && coverage.mid_run_stops > 0
                && coverage.after_arrival_with_a_result > 0
                && coverage.shared_answers > 0
                && coverage.shared_stops > 0,
            "the generated cases left a property unexercised: {coverage:#?}"
        );
    });
}

// ---------------------------------------------------------------------------
// Pinned cases.
// ---------------------------------------------------------------------------

fn request(id: &str, method: &str, params: Value) -> Message {
    Message::Request(Request {
        id: RequestId::from(id.to_string()),
        method: method.to_string(),
        params,
    })
}

fn notification(method: &str, params: Value) -> Message {
    Message::Notification(Notification {
        method: method.to_string(),
        params,
    })
}

fn did_open(uri: &str, text: &str) -> Message {
    notification(
        "textDocument/didOpen",
        json!({ "textDocument": {
            "uri": uri, "languageId": "fsharp", "version": 0, "text": text,
        }}),
    )
}

fn did_change(uri: &str, text: &str) -> Message {
    notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 1 },
            "contentChanges": [{ "text": text }],
        }),
    )
}

/// A hover on `B`'s use of `A.x` (line 2, column 10 of every version that
/// has one).
fn hover_b(world: &World, id: &str) -> Message {
    request(
        id,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": world.uri(Doc::Src(Src::B)) },
            "position": { "line": 2, "character": 10 },
        }),
    )
}

fn error_code(answers: &BTreeMap<RequestId, Vec<Response>>, id: &str) -> Option<i32> {
    let answer = &answers[&RequestId::from(id.to_string())];
    assert_eq!(answer.len(), 1, "{id} is answered once: {answer:?}");
    answer[0].response_result.as_ref().err().map(|e| e.code)
}

const CANCELLED: Option<i32> = Some(lsp_server::ErrorCode::RequestCanceled as i32);
const CONTENT_MODIFIED: Option<i32> = Some(lsp_server::ErrorCode::ContentModified as i32);

/// The loop reads every message waiting before it handles one: queued before
/// the loop starts, a request cancelled after it and one whose document then
/// changes are refused without running, as one message at a time could not
/// refuse them.
#[test]
fn the_loop_reads_every_waiting_message_before_it_handles_one() {
    on_server_stack(|| {
        let world = World::new();
        let b = world.uri(Doc::Src(Src::B));
        let (server, client) = Connection::memory();
        // Into the inbox itself, as the input thread hands messages over, so
        // all of it is waiting when the loop starts.
        let (input, inbox) = borzoi::inbox::channel(borzoi::server::INPUT_BUFFER);
        for message in [
            did_open(&b, Src::B.text(0)),
            hover_b(&world, "h1"),
            notification("$/cancelRequest", json!({ "id": "h1" })),
            hover_b(&world, "h2"),
            did_change(&b, Src::B.text(1)),
            hover_b(&world, "h3"),
            request("s", "shutdown", Value::Null),
            notification("exit", Value::Null),
        ] {
            input.send(message).unwrap();
        }
        let mut state = State::new();
        state.workspace = Workspace::with_env(SdkDiscoveryEnv {
            dotnet_root: Some(world.root.join("no-dotnet")),
            ..SdkDiscoveryEnv::default()
        });
        borzoi::server::run_inbox(&server, inbox, state, None).expect("the loop ends cleanly");
        let mut answers: BTreeMap<RequestId, Vec<Response>> = BTreeMap::new();
        for message in client.receiver.try_iter() {
            if let Message::Response(response) = message {
                answers
                    .entry(response.id.clone())
                    .or_default()
                    .push(response);
            }
        }
        assert_eq!(error_code(&answers, "h1"), CANCELLED);
        assert_eq!(error_code(&answers, "h2"), CONTENT_MODIFIED);
        assert_eq!(error_code(&answers, "h3"), None);
        assert_eq!(error_code(&answers, "s"), None);
        assert_eq!(answers.len(), 4, "{answers:?}");
    });
}

/// A text sync the loop has already applied still refuses an earlier request
/// it superseded when the plan is made again for a message read later.
#[test]
fn a_handled_change_still_refuses_the_request_it_superseded() {
    on_server_stack(|| {
        let world = World::new();
        let b = world.uri(Doc::Src(Src::B));
        let mut session = Session::start(&world.root, false);
        session.deliver(vec![did_open(&b, Src::B.text(0))]);
        session
            .dispatcher
            .admit([hover_b(&world, "h1"), did_change(&b, Src::B.text(1))]);
        // Notifications come first: this applies the change.
        assert_eq!(session.step(), Progress::Handled);
        let answers = session.deliver(vec![request(
            "h2",
            "workspace/symbol",
            json!({ "query": "" }),
        )]);
        assert_eq!(error_code(&answers, "h1"), CONTENT_MODIFIED);
        assert_eq!(error_code(&answers, "h2"), None);
    });
}

/// A change read while earlier messages are still being handled refuses a
/// pending request on its document.
#[test]
fn a_change_read_mid_epoch_refuses_a_pending_request() {
    on_server_stack(|| {
        let world = World::new();
        let b = world.uri(Doc::Src(Src::B));
        let mut session = Session::start(&world.root, false);
        session
            .dispatcher
            .admit([did_open(&b, Src::B.text(0)), hover_b(&world, "h1")]);
        assert_eq!(session.step(), Progress::Handled);
        let answers = session.deliver(vec![did_change(&b, Src::B.text(1))]);
        assert_eq!(error_code(&answers, "h1"), CONTENT_MODIFIED);
    });
}

/// A request on one document runs after a later change to another that its
/// answer reads: the hover on `B`'s `A.x` reports the type the newest `A`
/// gives it, not the one `A` had when the hover was sent.
#[test]
fn a_request_reads_the_newest_text_of_every_other_document() {
    on_server_stack(|| {
        let world = World::new();
        let a = world.uri(Doc::Src(Src::A));
        let b = world.uri(Doc::Src(Src::B));
        let mut session = Session::start(&world.root, false);
        session.deliver(vec![
            did_open(&a, Src::A.text(0)),
            did_open(&b, Src::B.text(0)),
        ]);
        let before = session.ask(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": b },
                "position": { "line": 2, "character": 10 },
            }),
        );
        assert!(before.to_string().contains("int"), "{before}");
        let answers = session.deliver(vec![hover_b(&world, "h1"), did_change(&a, Src::A.text(1))]);
        let hover = answers[&RequestId::from("h1".to_string())][0]
            .response_result
            .as_ref()
            .expect("the hover runs")
            .to_string();
        assert!(hover.contains("string"), "{hover}");
    });
}

/// A push client is sent the diagnostics of the text that stands, once: the
/// change a later one supersedes is applied and publishes nothing.
#[test]
fn a_superseded_change_publishes_nothing() {
    on_server_stack(|| {
        let world = World::new();
        let a = world.uri(Doc::Src(Src::A));
        let mut session = Session::start(&world.root, true);
        session.deliver(vec![did_open(&a, Src::A.text(0))]);
        session.settle();
        session.client.receiver.try_iter().for_each(drop);
        session.dispatcher.admit([
            did_change(&a, Src::A.text(4)),
            did_change(&a, Src::A.text(1)),
        ]);
        while session.step() == Progress::Handled {}
        let published: Vec<Value> = session
            .client
            .receiver
            .try_iter()
            .filter_map(|m| match m {
                Message::Notification(n) if n.method == "textDocument/publishDiagnostics" => {
                    Some(n.params)
                }
                _ => None,
            })
            .collect();
        assert_eq!(published.len(), 1, "{published:#?}");
        assert_eq!(published[0]["uri"], json!(a));
        assert_eq!(published[0]["diagnostics"], json!([]), "{published:#?}");
    });
}

/// The loop reads its inbox again before each message it handles: a
/// change that arrives while it handles an earlier message refuses a request
/// it had read before the change arrived. The change is sent through the
/// inbox once the first step is done, as the input thread hands over a
/// message that arrives then.
#[test]
fn the_loop_reads_again_before_each_message_it_handles() {
    on_server_stack(|| {
        let world = World::new();
        let b = world.uri(Doc::Src(Src::B));
        let mut session = Session::start(&world.root, false);
        session
            .dispatcher
            .admit([did_open(&b, Src::B.text(0)), hover_b(&world, "h1")]);
        assert_eq!(session.step(), Progress::Handled, "the open");
        session.input.send(did_change(&b, Src::B.text(1))).unwrap();
        session.run_out();
        let answers = session.absorb();
        assert_eq!(error_code(&answers, "h1"), CONTENT_MODIFIED);
    });
}

/// Behind `lsp-server`'s stdio transport, which hands each message over a
/// rendezvous channel, the loop still finds a burst waiting: the input
/// buffer takes every message the moment it is decoded. The client's whole
/// burst is taken before the loop starts (the sender could not finish
/// otherwise), and every request a later cancellation names is refused.
#[test]
fn a_burst_behind_a_rendezvous_transport_is_planned_together() {
    on_server_stack(|| {
        let world = World::new();
        let b = world.uri(Doc::Src(Src::B));
        let (upstream, from_client) = crossbeam_channel::bounded::<Message>(0);
        let (to_client, client) = crossbeam_channel::unbounded::<Message>();
        let inbox = borzoi::inbox::forward(from_client, borzoi::server::INPUT_BUFFER);
        const PAIRS: usize = 32;
        let mut burst = vec![did_open(&b, Src::B.text(0))];
        for i in 0..PAIRS {
            burst.push(hover_b(&world, &format!("h{i}")));
            burst.push(notification(
                "$/cancelRequest",
                json!({ "id": format!("h{i}") }),
            ));
        }
        burst.push(request("s", "shutdown", Value::Null));
        burst.push(notification("exit", Value::Null));
        let sender = std::thread::spawn(move || {
            for message in burst {
                upstream.send(message).unwrap();
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !sender.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "nothing took the burst from the rendezvous channel"
            );
            std::thread::yield_now();
        }
        sender.join().expect("the sender ends cleanly");
        let mut state = State::new();
        state.workspace = Workspace::with_env(SdkDiscoveryEnv {
            dotnet_root: Some(world.root.join("no-dotnet")),
            ..SdkDiscoveryEnv::default()
        });
        let connection = Connection {
            sender: to_client,
            receiver: crossbeam_channel::never(),
        };
        borzoi::server::run_inbox(&connection, inbox, state, None).expect("the loop ends cleanly");
        let mut answers: BTreeMap<RequestId, Vec<Response>> = BTreeMap::new();
        for message in client.try_iter() {
            if let Message::Response(response) = message {
                answers
                    .entry(response.id.clone())
                    .or_default()
                    .push(response);
            }
        }
        for i in 0..PAIRS {
            assert_eq!(error_code(&answers, &format!("h{i}")), CANCELLED, "h{i}");
        }
        assert_eq!(error_code(&answers, "s"), None);
    });
}

// ---------------------------------------------------------------------------
// Requests that stop (Stage 9).
// ---------------------------------------------------------------------------

/// The spans that begin a unit of work whose result is cached, after which a
/// running request passes a checkpoint (`docs/incremental-lsp-plan.md`,
/// Stage 9). An env is two: resolving its references, and building it from
/// them (reading their DLLs, `build_env_from_dll_paths`, inside it).
const UNIT_SPANS: [&str; 6] = [
    "evaluate_project",
    "parse_compile_item",
    "resolve_references",
    "build_assembly_env",
    "resolve_project",
    "infer_project",
];

/// What [`StopAtSpan`] saw.
#[derive(Default, Debug)]
struct SpanTrace {
    /// Every span the first run began, by name, while it ran its handler
    /// (inside its `lsp.request` span).
    points: Vec<&'static str>,
    /// The index, among `points`, at which the notification was sent.
    sent_at: Option<usize>,
    /// Units begun after the notification was sent and before the loop
    /// finished the run it stopped ([`borzoi::inbox::STOPPED_EVENT`], the
    /// stopped step's last act).
    begun_after: Vec<&'static str>,
    /// `lsp.request` spans begun: one per run.
    runs: usize,
    /// The unit spans open now, by id: a unit begun inside another is part
    /// of it.
    open_units: Vec<u64>,
    next_id: u64,
    in_first_run: bool,
    stopped: usize,
}

/// The id [`StopAtSpan`] gives the `lsp.request` span, so that it can tell
/// when the handler has returned; every other span gets 1.
const REQUEST_SPAN_ID: u64 = 2;

/// A subscriber that sends a notification the server ignores as the `at`th
/// span the request's first run begins inside its handler does, and records
/// which units begin before the stopped step ends.
struct StopAtSpan {
    at: Option<usize>,
    /// What is sent: a notification the server ignores, unless a test asks
    /// for another (a cancellation of the request).
    sends: Message,
    input: borzoi::inbox::InboxSender,
    trace: Arc<Mutex<SpanTrace>>,
}

impl tracing::Subscriber for StopAtSpan {
    fn register_callsite(
        &self,
        metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        if self.enabled(metadata) {
            tracing::subscriber::Interest::always()
        } else {
            tracing::subscriber::Interest::sometimes()
        }
    }
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.is_span()
            || (metadata.is_event() && metadata.name() == borzoi::inbox::STOPPED_EVENT)
    }
    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut trace = self.trace.lock().unwrap();
        let name = span.metadata().name();
        if name == "lsp.request" {
            trace.runs += 1;
            trace.in_first_run = trace.runs == 1;
            return tracing::span::Id::from_u64(REQUEST_SPAN_ID);
        }
        let unit = UNIT_SPANS.contains(&name);
        if trace.sent_at.is_some() && trace.stopped == 0 && unit && trace.open_units.is_empty() {
            trace.begun_after.push(name);
        }
        if trace.in_first_run {
            let index = trace.points.len();
            trace.points.push(name);
            if self.at == Some(index) {
                trace.sent_at = Some(index);
                self.input.send(self.sends.clone()).unwrap();
            }
        }
        if unit {
            // Ids above the request's, one per unit span, so closing one is
            // seen.
            trace.next_id += 1;
            let id = REQUEST_SPAN_ID + trace.next_id;
            trace.open_units.push(id);
            return tracing::span::Id::from_u64(id);
        }
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, _: &tracing::Event<'_>) {
        self.trace.lock().unwrap().stopped += 1;
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn try_close(&self, id: tracing::span::Id) -> bool {
        let id = id.into_u64();
        let mut trace = self.trace.lock().unwrap();
        if id == REQUEST_SPAN_ID {
            trace.in_first_run = false;
        }
        trace.open_units.retain(|open| *open != id);
        true
    }
}

/// A notification the server ignores.
fn ignored() -> Message {
    notification("$/borzoi-test/ignored", Value::Null)
}

/// A cancellation of the request `id`.
fn cancel(id: &str) -> Message {
    notification("$/cancelRequest", json!({ "id": id }))
}

/// A scenario for [`a_request_stops_for_a_notification_wherever_it_arrives`]:
/// what a fresh session is sent first, unstopped, and the request it is then
/// asked.
struct LatencyScenario {
    name: &'static str,
    setup: Vec<Message>,
    /// Project files rewritten after the setup, with no watch event, so the
    /// request evaluates each again.
    rewrite: Vec<PathBuf>,
    /// Files given new contents after the setup, with no watch event.
    replace: Vec<(PathBuf, String)>,
    request: Message,
}

fn latency_scenarios(world: &World) -> Vec<LatencyScenario> {
    let a = world.uri(Doc::Src(Src::A));
    let c = world.uri(Doc::Src(Src::C));
    let opens: Vec<Message> = SOURCES
        .iter()
        .map(|&src| did_open(&world.uri(Doc::Src(src)), src.text(0)))
        .collect();
    let hover_c = request(
        "hover",
        "textDocument/hover",
        json!({
            "textDocument": { "uri": c },
            "position": { "line": 2, "character": 10 },
        }),
    );
    let tokens_c = |id: &str| {
        request(
            id,
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": c } }),
        )
    };
    // A second project beside `P`, opened, so `workspace/symbol` parses two.
    let q = world.root.join("Q");
    std::fs::create_dir_all(&q).unwrap();
    std::fs::write(
        q.join("Q.fsproj"),
        "<Project>\n  <ItemGroup>\n    <Compile Include=\"D.fs\" />\n  </ItemGroup>\n</Project>\n",
    )
    .unwrap();
    std::fs::write(q.join("D.fs"), "module D\n\nlet d = 1\n").unwrap();
    let d = Uri::from_file_path(q.join("D.fs"))
        .unwrap()
        .as_str()
        .to_string();
    // And a third, `R`, for the scenario whose projects all evaluate again.
    let r = world.root.join("R");
    std::fs::create_dir_all(&r).unwrap();
    std::fs::write(
        r.join("R.fsproj"),
        "<Project>\n  <ItemGroup>\n    <Compile Include=\"F.fs\" />\n  </ItemGroup>\n</Project>\n",
    )
    .unwrap();
    std::fs::write(r.join("F.fs"), "module F\n\nlet f = 1\n").unwrap();
    let f = Uri::from_file_path(r.join("F.fs"))
        .unwrap()
        .as_str()
        .to_string();
    // A directory of three projects that compile nothing, beside a source
    // they all fail to claim: its ownership walk weighs (evaluates) each.
    let s_dir = world.root.join("S");
    std::fs::create_dir_all(&s_dir).unwrap();
    for name in ["S1", "S2", "S3"] {
        std::fs::write(
            s_dir.join(format!("{name}.fsproj")),
            "<Project>\n  <ItemGroup>\n  </ItemGroup>\n</Project>\n",
        )
        .unwrap();
    }
    std::fs::write(s_dir.join("X.fs"), "module X\n\nlet x = 1\n").unwrap();
    let x = Uri::from_file_path(s_dir.join("X.fs"))
        .unwrap()
        .as_str()
        .to_string();
    vec![
        LatencyScenario {
            name: "a hover whose ownership walk evaluates three projects again",
            setup: vec![did_open(&x, "module X\n\nlet x = 1\n")],
            rewrite: ["S1", "S2", "S3"]
                .iter()
                .map(|name| s_dir.join(format!("{name}.fsproj")))
                .collect(),
            replace: Vec::new(),
            request: request(
                "hover",
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": x },
                    "position": { "line": 2, "character": 4 },
                }),
            ),
        },
        LatencyScenario {
            name: "a hover over a cached fold whose project file gained a comment \
                   (the env's references resolved again, alike)",
            setup: opens.iter().cloned().chain([hover_c.clone()]).collect(),
            rewrite: vec![world.root.join("P").join("P.fsproj")],
            replace: Vec::new(),
            request: hover_c.clone(),
        },
        LatencyScenario {
            name: "a hover over a cached fold whose project gained a reference to a project \
                   with no output (the env's references resolved again, evaluating it)",
            setup: opens.iter().cloned().chain([hover_c.clone()]).collect(),
            rewrite: Vec::new(),
            replace: vec![(
                world.root.join("P").join("P.fsproj"),
                format!(
                    "<Project>\n  <ItemGroup>\n{}  </ItemGroup>\n  <ItemGroup>\n    \
                     <ProjectReference Include=\"../Q/Q.fsproj\" />\n  </ItemGroup>\n</Project>\n",
                    SOURCES
                        .iter()
                        .map(|s| format!("    <Compile Include=\"{}\" />\n", s.file()))
                        .collect::<String>()
                ),
            )],
            request: hover_c.clone(),
        },
        LatencyScenario {
            name: "a cold hover (parses, env, fold, inference)",
            setup: opens.clone(),
            rewrite: Vec::new(),
            replace: Vec::new(),
            request: hover_c.clone(),
        },
        LatencyScenario {
            name: "cold references (the whole project, then its scan)",
            setup: opens.clone(),
            rewrite: Vec::new(),
            replace: Vec::new(),
            request: request(
                "references",
                "textDocument/references",
                json!({
                    "textDocument": { "uri": c },
                    "position": { "line": 2, "character": 4 },
                    "context": { "includeDeclaration": true },
                }),
            ),
        },
        LatencyScenario {
            name: "cold semantic tokens",
            setup: opens.clone(),
            rewrite: Vec::new(),
            replace: Vec::new(),
            request: tokens_c("tokens"),
        },
        LatencyScenario {
            name: "a hover over a cached fold (inference only)",
            setup: opens.iter().cloned().chain([tokens_c("warm")]).collect(),
            rewrite: Vec::new(),
            replace: Vec::new(),
            request: hover_c,
        },
        LatencyScenario {
            name: "workspace symbols over three projects that all evaluate again",
            setup: opens
                .iter()
                .cloned()
                .chain([
                    did_open(&d, "module D\n\nlet d = 1\n"),
                    did_open(&f, "module F\n\nlet f = 1\n"),
                ])
                .collect(),
            rewrite: vec![
                world.root.join("P").join("P.fsproj"),
                q.join("Q.fsproj"),
                r.join("R.fsproj"),
            ],
            replace: Vec::new(),
            request: request("symbols", "workspace/symbol", json!({ "query": "" })),
        },
        LatencyScenario {
            name: "workspace symbols over two projects",
            setup: opens
                .iter()
                .cloned()
                .chain([
                    did_open(&d, "module D\n\nlet d = 1\n"),
                    did_open(&a, Src::A.text(0)),
                ])
                .collect(),
            rewrite: Vec::new(),
            replace: Vec::new(),
            request: request("symbols", "workspace/symbol", json!({ "query": "" })),
        },
    ]
}

/// One fresh session: `scenario`'s setup, then its request, with a
/// notification sent as the `at`th span of its handler begins.
fn run_latency_case(
    world: &World,
    scenario: &LatencyScenario,
    at: Option<usize>,
) -> (SpanTrace, Value) {
    // Put back after the case, so each case starts from the same disk.
    let originals: Vec<(PathBuf, String)> = scenario
        .replace
        .iter()
        .map(|(path, _)| path)
        .chain(&scenario.rewrite)
        .map(|path| {
            (
                path.clone(),
                crate::common::host_fs::read_to_string(path).unwrap(),
            )
        })
        .collect();
    let mut session = Session::start(&world.root, false);
    session.deliver(scenario.setup.clone());
    session.settle();
    for (path, text) in &scenario.replace {
        std::fs::write(path, text).unwrap();
    }
    for project in &scenario.rewrite {
        // A text this session has not evaluated.
        static REWRITES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = REWRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let text = crate::common::host_fs::read_to_string(project).unwrap();
        let text = text.split("<!--").next().unwrap().trim_end().to_string();
        std::fs::write(project, format!("{text}\n<!-- {n} -->\n")).unwrap();
    }
    let trace = Arc::new(Mutex::new(SpanTrace::default()));
    let subscriber = StopAtSpan {
        at,
        sends: ignored(),
        input: session.input.clone(),
        trace: Arc::clone(&trace),
    };
    let mut answers = crate::common::with_subscriber(subscriber, || {
        session.deliver(vec![scenario.request.clone()])
    });
    let Message::Request(request) = &scenario.request else {
        unreachable!()
    };
    let mut answer = answers
        .remove(&request.id)
        .expect("the request is answered");
    assert_eq!(answer.len(), 1, "answered once: {answer:?}");
    let answer =
        serde_json::to_value(answer.remove(0).response_result.map_err(|e| e.code)).unwrap();
    let trace = std::mem::take(&mut *trace.lock().unwrap());
    for (path, text) in originals {
        std::fs::write(path, text).unwrap();
    }
    (trace, answer)
}

/// The cancellation latency, over every point a request's handler passes: a
/// notification that arrives as any span of the handler begins (the
/// handler's own, each unit's, and every span inside them) stops the request
/// once, no unit of work begins before the stopped step ends (the
/// notification is handled next), and the request then answers as it does
/// unstopped. Swept over every such span of each [`latency_scenarios`]
/// scenario: cold requests, cached folds whose inference, ownership, project
/// files or env references moved, and `workspace/symbol` over several
/// projects.
#[test]
fn a_request_stops_for_a_notification_wherever_it_arrives() {
    on_server_stack(|| {
        let world = World::new();
        for scenario in latency_scenarios(&world) {
            let (unstopped, want) = run_latency_case(&world, &scenario, None);
            assert!(
                unstopped.points.iter().any(|p| UNIT_SPANS.contains(p)),
                "{}: the request begins a unit: {unstopped:?}",
                scenario.name
            );
            assert_eq!(unstopped.stopped, 0);
            assert!(
                unstopped
                    .points
                    .iter()
                    .filter(|p| **p == "evaluate_project")
                    .count()
                    >= scenario.rewrite.len(),
                "{}: the request evaluates each rewritten project again: {unstopped:?}",
                scenario.name
            );
            for at in 0..unstopped.points.len() {
                let (trace, got) = run_latency_case(&world, &scenario, Some(at));
                let point = unstopped.points[at];
                assert_eq!(
                    trace.sent_at,
                    Some(at),
                    "{}: the notification was sent at {point} ({at}): {trace:?}",
                    scenario.name
                );
                assert_eq!(
                    trace.stopped, 1,
                    "{}: the request stopped once, for the notification at {point} ({at}): \
                     {trace:?}",
                    scenario.name
                );
                assert!(
                    trace.begun_after.is_empty(),
                    "{}: after the notification at {point} ({at}), {:?} began before the \
                     stopped step ended (its spans unstopped: {:?})",
                    scenario.name,
                    trace.begun_after,
                    unstopped.points
                );
                assert_eq!(
                    got, want,
                    "{}: stopped at {point} ({at}), the request answers as it does unstopped",
                    scenario.name
                );
            }
        }
    });
}

/// Stage 9's gate, in small: a change and a pull of its document arrive while
/// a request on another document runs. The request stops, the change is
/// applied, the pull is answered first, and the request is answered after,
/// against the new text.
#[test]
fn an_edit_that_arrives_mid_request_is_answered_before_it() {
    on_server_stack(|| {
        let world = World::new();
        let a = world.uri(Doc::Src(Src::A));
        let c = world.uri(Doc::Src(Src::C));
        // `C` reads `A.x` at line 2, column 16.
        let hover_c = || {
            request(
                "hover",
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": c },
                    "position": { "line": 2, "character": 16 },
                }),
            )
        };
        let mut session = Session::start(&world.root, false);
        session.deliver(vec![
            did_open(&a, Src::A.text(0)),
            did_open(&c, Src::C.text(1)),
        ]);
        session.settle();
        let rest = vec![
            did_change(&a, Src::A.text(1)),
            request(
                "pull",
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": a } }),
            ),
        ];
        // The rest arrives at the hover's first checkpoint.
        let mid_run = Arc::new(Mutex::new(MidRun {
            countdown: 0,
            write: None,
            rest: Some(rest),
            input: session.input.clone(),
            client: session.client.receiver.clone(),
            sent_before: None,
        }));
        let seen = Arc::new(Mutex::new(Seen::default()));
        crate::common::with_subscriber(Counter(seen, Some(Arc::clone(&mid_run))), || {
            session.dispatcher.admit([hover_c()]);
            session.run_out();
        });
        assert_eq!(mid_run.lock().unwrap().sent_before, Some(0));
        assert_eq!(session.stops, 1, "the hover stopped");
        let answered: Vec<Response> = session
            .client
            .receiver
            .try_iter()
            .filter_map(|message| match message {
                Message::Response(response) => Some(response),
                _ => None,
            })
            .collect();
        let ids: Vec<String> = answered.iter().map(|r| r.id.to_string()).collect();
        assert_eq!(
            ids,
            ["\"pull\"", "\"hover\""],
            "the pull is answered before the hover it interrupted, each once"
        );
        let mut fresh = Session::start(&world.root, false);
        fresh.deliver(vec![
            did_open(&a, Src::A.text(1)),
            did_open(&c, Src::C.text(1)),
        ]);
        let Message::Request(hover) = hover_c() else {
            unreachable!()
        };
        let want = fresh.ask(&hover.method, hover.params);
        let got = serde_json::to_value(
            answered[1]
                .response_result
                .clone()
                .map_err(|e| (e.code, e.message)),
        )
        .unwrap();
        assert_eq!(got, want, "the hover reads the text the change brought");
    });
}

/// Every response the session's client has received, in order.
fn responses(session: &Session) -> Vec<Response> {
    session
        .client
        .receiver
        .try_iter()
        .filter_map(|message| match message {
            Message::Response(response) => Some(response),
            _ => None,
        })
        .collect()
}

/// A response as the probes compare it.
fn outcome(response: &Response) -> Value {
    serde_json::to_value(
        response
            .response_result
            .clone()
            .map_err(|e| (e.code, e.message)),
    )
    .unwrap()
}

/// A hover on `C` at its `A.x` (line 2, column 16 of `C`'s second version).
fn hover_c(world: &World, id: &str) -> Message {
    request(
        id,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": world.uri(Doc::Src(Src::C)) },
            "position": { "line": 2, "character": 16 },
        }),
    )
}

/// Identical requests in one batch run once between them, and each is
/// answered, under its own id, with what it answers asked alone. Requests that
/// differ in a parameter, or name the document under another spelling, or
/// whose method is not shareable (a definition may be answered by a fetch),
/// run on their own.
#[test]
fn identical_requests_in_a_batch_share_one_run() {
    on_server_stack(|| {
        let world = World::new();
        let a = world.uri(Doc::Src(Src::A));
        let alias = world.uri(Doc::AliasA);
        let c = world.uri(Doc::Src(Src::C));
        let mut session = Session::start(&world.root, false);
        session.deliver(vec![
            did_open(&a, Src::A.text(0)),
            did_open(&c, Src::C.text(1)),
        ]);
        session.settle();
        let at = |uri: &str, line: u32, character: u32| {
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            })
        };
        let tokens = json!({ "textDocument": { "uri": c } });
        let batch = vec![
            hover_c(&world, "h1"),
            request("t1", "textDocument/semanticTokens/full", tokens.clone()),
            hover_c(&world, "h2"),
            request("z", "textDocument/hover", at(&c, 2, 4)),
            request("d1", "textDocument/definition", at(&c, 2, 16)),
            request("t2", "textDocument/semanticTokens/full", tokens.clone()),
            request("d2", "textDocument/definition", at(&c, 2, 16)),
            request("a1", "textDocument/hover", at(&a, 2, 4)),
            request("a2", "textDocument/hover", at(&alias, 2, 4)),
            hover_c(&world, "h3"),
        ];
        let seen = Arc::new(Mutex::new(Seen::default()));
        let answers = crate::common::with_subscriber(Counter(Arc::clone(&seen), None), || {
            session.deliver(batch)
        });
        assert_eq!(
            seen.lock().unwrap().requests_run,
            7,
            "one run for the three hovers at `A.x`, one for both token requests, and one each \
             for the hover at `z`, the two definitions and the two spellings of `A`"
        );
        let answer = |id: &str| {
            let got = &answers[&RequestId::from(id.to_string())];
            assert_eq!(got.len(), 1, "{id} is answered once");
            assert!(got[0].response_result.is_ok(), "{id}: {:?}", got[0]);
            outcome(&got[0])
        };
        assert_eq!(answers.len(), 10, "every request is answered: {answers:?}");
        let Message::Request(hover) = hover_c(&world, "probe") else {
            unreachable!()
        };
        let alone = session.ask(&hover.method, hover.params);
        assert_ne!(alone, json!({ "Ok": null }), "the hover finds `A.x`");
        for id in ["h1", "h2", "h3"] {
            assert_eq!(answer(id), alone, "{id} answers as the hover asked alone");
        }
        let alone = session.ask("textDocument/semanticTokens/full", tokens);
        for id in ["t1", "t2"] {
            assert_eq!(answer(id), alone, "{id} answers as the request asked alone");
        }
        for id in ["z", "d1", "d2", "a1", "a2"] {
            answer(id);
        }
    });
}

/// A shared run that a notification stops answers none of the requests it
/// was to answer: each is planned again, behind the requests that came with
/// the notification, and is then answered once, against the new text.
#[test]
fn a_shared_run_that_stops_answers_its_requests_after_the_notification() {
    on_server_stack(|| {
        let world = World::new();
        let a = world.uri(Doc::Src(Src::A));
        let c = world.uri(Doc::Src(Src::C));
        let mut session = Session::start(&world.root, false);
        session.deliver(vec![
            did_open(&a, Src::A.text(0)),
            did_open(&c, Src::C.text(1)),
        ]);
        session.settle();
        let rest = vec![
            did_change(&a, Src::A.text(1)),
            request(
                "pull",
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": a } }),
            ),
        ];
        let mid_run = Arc::new(Mutex::new(MidRun {
            countdown: 0,
            write: None,
            rest: Some(rest),
            input: session.input.clone(),
            client: session.client.receiver.clone(),
            sent_before: None,
        }));
        let seen = Arc::new(Mutex::new(Seen::default()));
        crate::common::with_subscriber(
            Counter(Arc::clone(&seen), Some(Arc::clone(&mid_run))),
            || {
                session
                    .dispatcher
                    .admit([hover_c(&world, "h1"), hover_c(&world, "h2")]);
                session.run_out();
            },
        );
        assert_eq!(mid_run.lock().unwrap().sent_before, Some(0));
        assert_eq!(session.stops, 1, "the shared run stopped");
        assert_eq!(seen.lock().unwrap().shared_stops, 1);
        let answered = responses(&session);
        let ids: Vec<String> = answered.iter().map(|r| r.id.to_string()).collect();
        assert_eq!(
            ids,
            ["\"pull\"", "\"h1\"", "\"h2\""],
            "the pull is answered before the hovers it interrupted, each once"
        );
        let mut fresh = Session::start(&world.root, false);
        fresh.deliver(vec![
            did_open(&a, Src::A.text(1)),
            did_open(&c, Src::C.text(1)),
        ]);
        let Message::Request(hover) = hover_c(&world, "probe") else {
            unreachable!()
        };
        let want = fresh.ask(&hover.method, hover.params);
        for response in &answered[1..] {
            assert_eq!(
                outcome(response),
                want,
                "{} reads the text the change brought",
                response.id
            );
        }
    });
}

/// A request that a shared run was to answer, cancelled while that run
/// runs, is refused; the run stops, and runs again for the rest.
#[test]
fn a_request_cancelled_while_its_shared_run_runs_is_refused_alone() {
    on_server_stack(|| {
        let world = World::new();
        let a = world.uri(Doc::Src(Src::A));
        let c = world.uri(Doc::Src(Src::C));
        let mut session = Session::start(&world.root, false);
        session.deliver(vec![
            did_open(&a, Src::A.text(0)),
            did_open(&c, Src::C.text(1)),
        ]);
        session.settle();
        let mid_run = Arc::new(Mutex::new(MidRun {
            countdown: 0,
            write: None,
            rest: Some(vec![notification("$/cancelRequest", json!({ "id": "h2" }))]),
            input: session.input.clone(),
            client: session.client.receiver.clone(),
            sent_before: None,
        }));
        let seen = Arc::new(Mutex::new(Seen::default()));
        crate::common::with_subscriber(
            Counter(Arc::clone(&seen), Some(Arc::clone(&mid_run))),
            || {
                session.dispatcher.admit([
                    hover_c(&world, "h1"),
                    hover_c(&world, "h2"),
                    hover_c(&world, "h3"),
                ]);
                session.run_out();
            },
        );
        assert_eq!(session.stops, 1, "the shared run stopped");
        let answered = responses(&session);
        let ids: Vec<String> = answered.iter().map(|r| r.id.to_string()).collect();
        assert_eq!(ids, ["\"h1\"", "\"h3\"", "\"h2\""], "each answered once");
        assert_eq!(
            answered[2].response_result.as_ref().err().map(|e| e.code),
            Some(lsp_server::ErrorCode::RequestCanceled as i32)
        );
        let Message::Request(hover) = hover_c(&world, "probe") else {
            unreachable!()
        };
        let want = session.ask(&hover.method, hover.params);
        assert_eq!(outcome(&answered[0]), want);
        assert_eq!(outcome(&answered[1]), want);
    });
}

/// A request read while an identical one runs is not answered from that
/// run, which may have read the disk before the request was sent: here `A`
/// is rewritten on disk, with no notification, after the hover on `C` has
/// computed its answer and before it is sent, and the identical hover sent
/// then reads the new `A`. A request on `B` waits behind the first hover, so
/// the epoch holds the handled hover beside the identical one read after it.
#[test]
fn a_request_read_while_an_identical_one_runs_runs_again() {
    on_server_stack(|| {
        let world = World::new();
        let c = world.uri(Doc::Src(Src::C));
        let start = |world: &World| {
            let mut session = Session::start(&world.root, false);
            session.deliver(vec![did_open(&c, Src::C.text(1))]);
            session.settle();
            session
        };
        // The same run on a session of its own: its checkpoints, and its
        // answer with `A` as it is.
        let mut dry = start(&world);
        let seen = Arc::new(Mutex::new(Seen::default()));
        let before = crate::common::with_subscriber(Counter(Arc::clone(&seen), None), || {
            let mut answers = dry.deliver(vec![hover_c(&world, "dry")]);
            outcome(&answers.remove(&RequestId::from("dry".to_string())).unwrap()[0])
        });
        let checkpoints = seen.lock().unwrap().checkpoints;
        assert!(checkpoints > 0);

        let mut session = start(&world);
        // At the last checkpoint, the one after the handler: the answer is
        // computed, and a request stops nothing.
        let mid_run = Arc::new(Mutex::new(MidRun {
            countdown: checkpoints - 1,
            write: Some((world.path(Src::A), Src::A.text(1))),
            rest: Some(vec![hover_c(&world, "h2")]),
            input: session.input.clone(),
            client: session.client.receiver.clone(),
            sent_before: None,
        }));
        let seen = Arc::new(Mutex::new(Seen::default()));
        crate::common::with_subscriber(
            Counter(Arc::clone(&seen), Some(Arc::clone(&mid_run))),
            || {
                session.dispatcher.admit([
                    hover_c(&world, "h1"),
                    request(
                        "s",
                        "textDocument/documentSymbol",
                        json!({ "textDocument": { "uri": world.uri(Doc::Src(Src::B)) } }),
                    ),
                ]);
                session.run_out();
            },
        );
        assert_eq!(
            mid_run.lock().unwrap().sent_before,
            Some(0),
            "sent before h1 was answered"
        );
        assert_eq!(session.stops, 0, "a request stops nothing");
        assert_eq!(seen.lock().unwrap().requests_run, 3);
        let answered = responses(&session);
        let ids: Vec<String> = answered.iter().map(|r| r.id.to_string()).collect();
        assert_eq!(ids, ["\"h1\"", "\"s\"", "\"h2\""]);
        assert_eq!(outcome(&answered[0]), before, "h1 read `A` as it was");
        let after = start(&world).ask("textDocument/hover", {
            let Message::Request(hover) = hover_c(&world, "probe") else {
                unreachable!()
            };
            hover.params
        });
        assert_ne!(before, after, "the rewrite moves the hover");
        assert_eq!(outcome(&answered[2]), after, "h2 reads the rewritten `A`");
    });
}

/// What the loop holds is bounded: however far ahead the client is, the
/// epoch takes at most [`borzoi::server::MAX_PENDING`] messages, and the rest
/// wait in the inbox until it ends.
#[test]
fn the_epoch_holds_at_most_its_bound() {
    on_server_stack(|| {
        let world = World::new();
        let (server, _client) = Connection::memory();
        let (input, inbox) = borzoi::inbox::channel(2 * borzoi::server::MAX_PENDING);
        let mut state = State::new();
        state.workspace = Workspace::with_env(SdkDiscoveryEnv {
            dotnet_root: Some(world.root.join("no-dotnet")),
            ..SdkDiscoveryEnv::default()
        });
        let mut dispatcher = Dispatcher::start(&server, inbox, state, None);
        let ahead = borzoi::server::MAX_PENDING + 100;
        for i in 0..ahead {
            input
                .send(request(
                    &format!("r{i}"),
                    "borzoi-test/unknown",
                    Value::Null,
                ))
                .unwrap();
        }
        assert_eq!(dispatcher.step(&server).unwrap(), Progress::Handled);
        assert_eq!(dispatcher.pending_len(), borzoi::server::MAX_PENDING);
        let mut handled = 1;
        let mut epochs_ended = 0;
        while handled < ahead {
            match dispatcher.step(&server).unwrap() {
                Progress::Handled => handled += 1,
                // The epoch ended; the next step reads the rest.
                Progress::Idle => epochs_ended += 1,
                other => panic!("an unknown request answers MethodNotFound: {other:?}"),
            }
            assert!(dispatcher.pending_len() <= borzoi::server::MAX_PENDING);
            assert!(epochs_ended <= 1, "the rest fits in one more epoch");
        }
        assert_eq!(epochs_ended, 1, "the epoch ended once at its bound");
    });
}

/// A request that stops still owes what it would have owed finishing: a
/// closed Compile item rewritten on disk with no watch event stales the
/// fold, and the request that finds it stale owes the client a
/// semantic-token refresh, once, wherever it stops after finding it (before
/// the rebuild of the parses or during it) and whether it then runs again (a
/// notification the server ignores) or is refused (its own cancellation,
/// after which no rebuild finishes). A request refused before it looked at
/// the fold owes nothing, as one never sent would not. Swept over every span
/// of the handler, for semantic tokens and references (whose fold is found
/// stale before its parses are asked for).
#[test]
fn a_stopped_rebuild_still_owes_the_refresh_for_an_unannounced_change() {
    /// The request a round asks, by its id, on `c`.
    fn asked(id: &str, c: &str) -> Message {
        match id {
            "tokens" => request(
                id,
                "textDocument/semanticTokens/full",
                json!({ "textDocument": { "uri": c } }),
            ),
            _ => request(
                id,
                "textDocument/references",
                json!({
                    "textDocument": { "uri": c },
                    "position": { "line": 2, "character": 4 },
                    "context": { "includeDeclaration": true },
                }),
            ),
        }
    }
    /// One session: `A` and `C` open and both requests warm, `B` rewritten
    /// on disk, then the request `id` with `sends` at the `stop`th span of
    /// its handler. The trace, how many token refreshes the server sent for
    /// the request, and its error code.
    fn round(id: &str, stop: Option<usize>, sends: Message) -> (SpanTrace, usize, Option<i32>) {
        let mut world = World::new();
        let a = world.uri(Doc::Src(Src::A));
        let c = world.uri(Doc::Src(Src::C));
        let mut session = Session::start(&world.root, false);
        session.deliver(vec![
            did_open(&a, Src::A.text(0)),
            did_open(&c, Src::C.text(0)),
        ]);
        session.settle();
        session.deliver(vec![asked("warm-references", &c), asked("tokens", &c)]);
        session.settle();
        session.server_requests.clear();
        // `B` is closed: no sync announces this.
        world.write(Src::B, 1);
        let trace = Arc::new(Mutex::new(SpanTrace::default()));
        let subscriber = StopAtSpan {
            at: stop,
            sends,
            input: session.input.clone(),
            trace: Arc::clone(&trace),
        };
        let answers =
            crate::common::with_subscriber(subscriber, || session.deliver(vec![asked(id, &c)]));
        let code = error_code(&answers, id);
        session.settle();
        let trace = std::mem::take(&mut *trace.lock().unwrap());
        let refreshes = session
            .server_requests
            .iter()
            .filter(|method| *method == "workspace/semanticTokens/refresh")
            .count();
        (trace, refreshes, code)
    }
    on_server_stack(|| {
        let mut stops_during_rebuild = 0;
        for id in ["tokens", "references"] {
            let (unstopped, refreshes, code) = round(id, None, ignored());
            assert_eq!(code, None);
            assert_eq!(
                refreshes, 1,
                "{id}: the unannounced change owes one token refresh"
            );
            assert!(
                unstopped.points.contains(&"parse_compile_item"),
                "{id}: the request rebuilds the parses: {unstopped:?}"
            );
            // The fold is confirmed (and found stale) as the lookup begins,
            // or, for tokens, before the parses are built.
            let found = unstopped
                .points
                .iter()
                .position(|p| matches!(*p, "semantic.project_lookup" | "build_parses"))
                .expect("the request confirms the fold");
            for at in 0..unstopped.points.len() {
                let point = unstopped.points[at];
                if unstopped.points[..at].contains(&"parse_compile_item") {
                    stops_during_rebuild += 1;
                }
                for (sends, want) in [(ignored(), None), (cancel(id), CANCELLED)] {
                    let owed = usize::from(want.is_none() || at >= found);
                    let (stopped, refreshes, code) = round(id, Some(at), sends);
                    assert_eq!(stopped.stopped, 1, "{id} at {point} ({at}): {stopped:?}");
                    assert_eq!(code, want, "{id} at {point} ({at})");
                    assert_eq!(
                        refreshes,
                        owed,
                        "{id}, stopped at {point} ({at}) and then {}: the unannounced change \
                         owes {owed} token refresh",
                        if want.is_some() {
                            "refused"
                        } else {
                            "run again"
                        }
                    );
                }
            }
        }
        assert!(stops_during_rebuild > 0, "some stop came during a rebuild");
    });
}

/// A request that re-evaluated a project, stopped and refused for its own
/// cancellation, still leaves the client told what the moved evaluation may
/// have staled: a pull client is sent the diagnostic refresh it would have
/// been sent had the request finished (Codex).
#[test]
fn a_request_cancelled_mid_evaluation_still_reports_the_moved_evaluation() {
    fn round(stop: Option<usize>) -> (SpanTrace, usize, Option<i32>) {
        let world = World::new();
        let c = world.uri(Doc::Src(Src::C));
        let hover_c = || {
            request(
                "hover",
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": c },
                    "position": { "line": 2, "character": 10 },
                }),
            )
        };
        let mut session = Session::start(&world.root, false);
        session.deliver(vec![did_open(&c, Src::C.text(0))]);
        session.settle();
        session.deliver(vec![hover_c()]);
        session.settle();
        session.server_requests.clear();
        // The project gains a define, with no watch event.
        let project = world.root.join("P").join("P.fsproj");
        let text = crate::common::host_fs::read_to_string(&project).unwrap();
        std::fs::write(
            &project,
            text.replace(
                "<Project>",
                "<Project>\n  <PropertyGroup><DefineConstants>MOVED</DefineConstants></PropertyGroup>",
            ),
        )
        .unwrap();
        let trace = Arc::new(Mutex::new(SpanTrace::default()));
        let subscriber = StopAtSpan {
            at: stop,
            sends: cancel("hover"),
            input: session.input.clone(),
            trace: Arc::clone(&trace),
        };
        let answers =
            crate::common::with_subscriber(subscriber, || session.deliver(vec![hover_c()]));
        let code = error_code(&answers, "hover");
        session.settle();
        let trace = std::mem::take(&mut *trace.lock().unwrap());
        let refreshes = session
            .server_requests
            .iter()
            .filter(|method| *method == "workspace/diagnostic/refresh")
            .count();
        (trace, refreshes, code)
    }
    on_server_stack(|| {
        let (unstopped, refreshes, code) = round(None);
        assert_eq!(code, None);
        assert_eq!(
            refreshes, 1,
            "the moved evaluation owes a diagnostic refresh"
        );
        let evaluation = unstopped
            .points
            .iter()
            .position(|p| *p == "evaluate_project")
            .expect("the hover evaluates the project again");
        let (stopped, refreshes, code) = round(Some(evaluation));
        assert_eq!(stopped.stopped, 1, "the hover stopped: {stopped:?}");
        assert_eq!(code, CANCELLED);
        assert_eq!(
            refreshes, 1,
            "the hover stopped for its cancellation, and the moved evaluation still owes a \
             diagnostic refresh"
        );
    });
}
