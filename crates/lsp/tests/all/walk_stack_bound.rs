//! The server answers about the tallest tree the parser returns on half of
//! [`SERVER_STACK_BYTES`], the stack its request loop runs on.
//!
//! A stack overflow anywhere in the loop aborts the server with every open
//! document. `borzoi-sema`'s `walk_stack_bound` holds resolution and inference
//! to half of the stack; this holds the whole loop to it, for the same inputs
//! ([`cases`](borzoi_oracle_harness::repetition_shapes::cases)). Each case is
//! the one file of a project on disk whose assembly env holds a real
//! `System.Runtime.dll`, so every request takes the project path an editor's
//! does. [`run`] serves it, over the protocol, on a thread with half of
//! [`SERVER_STACK_BYTES`]: the file is opened, its diagnostics published and
//! pulled, and semantic tokens, symbols, and hover, definition, references and
//! completion at its deepest token are asked for. As in sema's twin, the case
//! binds the names and operators its shape uses
//! ([`PRELUDE`](borzoi_oracle_harness::repetition_shapes::PRELUDE)), so
//! inference walks every chain to the bottom rather than declining an
//! unresolved operator at the top. Ahead of that, an implementation file
//! declares [`PROBE_DECL`], whose member access completion is asked about too
//! and must answer with `String`'s members: that is the check that completion
//! reaches the tree at all.
//!
//! A case whose shape alone is shallow is not put to the server
//! ([`SHALLOW`]). Each case runs in a child copy of this binary
//! ([`stack_probe`](borzoi_oracle_harness::stack_probe)), so an overflow names
//! its case instead of aborting the suite.

use std::sync::mpsc;
use std::time::Duration;

use borzoi::server::{SERVER_STACK_BYTES, run};
use borzoi_cst::parser::{MAX_TREE_HEIGHT, Parse, parse, parse_sig};
use borzoi_cst::syntax::SyntaxNode;
use borzoi_cst::syntax::{NodeOrToken, WalkEvent};
use borzoi_oracle_harness::par_map::par_map;
use borzoi_oracle_harness::repetition_shapes::{Case, Form, Kind, cases, tallest_clean_count};
use borzoi_oracle_harness::stack_probe::{self, Outcome};
use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::notification::{DidOpenTextDocument, Exit, Notification as NotificationTrait};
use lsp_types::request::{
    Completion, DocumentDiagnosticRequest, DocumentSymbolRequest, GotoDefinition, HoverRequest,
    References, Request as RequestTrait, SemanticTokensFullRequest, Shutdown,
};
use lsp_types::{
    CompletionParams, DidOpenTextDocumentParams, DocumentDiagnosticParams, DocumentSymbolParams,
    GotoDefinitionParams, HoverParams, PartialResultParams, Position, ReferenceContext,
    ReferenceParams, SemanticTokensParams, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Uri, WorkDoneProgressParams,
};

use crate::common::runtime_project_state_in;

/// The child entry's full test path, for [`stack_probe::run_case`].
const CHILD: &str = "walk_stack_bound::probe_child";

/// The variable that hands a child its case.
const CASE_VAR: &str = "BORZOI_LSP_WALK_STACK_CASE";

/// How long one request may take. A debug build answers about a
/// thousand-level tree slowly, and a wedged server is still a failure.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// The declaration an implementation file opens with, ahead of the shape:
/// completion inside its `.Length` must offer `String`'s members.
const PROBE_DECL: &str = "let zzProbe = \"hi\".Length\n";

/// A column inside [`PROBE_DECL`]'s `Length`, on the file's first line.
const PROBE_COMPLETION_COLUMN: usize = "let zzProbe = \"hi\".Le".len();

/// A case whose shape alone parses to a tree shallower than this is not put
/// to the server: the shape does not grow the tree, so it adds nothing to
/// what the module stack (and every other shape) already puts to it, and
/// `borzoi-sema`'s twin still walks it. The shapes it skips are the flat
/// repeats (a long dotted path, many top-level `let`s), which cost the server
/// most to answer about.
const SHALLOW: usize = 64;

/// The deepest token put to the server must sit this close to the parser's
/// [`MAX_TREE_HEIGHT`], or the cases do not reach the height the bound is
/// about.
const REACH: usize = MAX_TREE_HEIGHT - 128;

fn parse_kind(kind: Kind, src: &str) -> Parse {
    match kind {
        Kind::Impl => parse(src),
        Kind::Sig => parse_sig(src),
    }
}

/// Whether `case`'s file opens with [`PROBE_DECL`]: where it opens with the
/// case's prelude of bindings too, which is wherever a declaration can stand
/// ahead of the shape.
fn takes_probe(case: &Case) -> bool {
    !case.prelude().is_empty()
}

/// What one case opens, and where it asks.
struct Session {
    src: String,
    kind: Kind,
    /// The node height of the file's tree.
    height: usize,
    /// The node height of the shape's tree alone, at the same length.
    shape_height: usize,
    /// The deepest token's start, and how many nodes sit above it.
    deepest: (Position, usize),
    probe: bool,
}

impl Session {
    /// Build `case`'s file: [`PROBE_DECL`] where it fits, then the shape at
    /// the length its form asks for.
    fn of(case: &Case) -> Session {
        let kind = case.shape.kind;
        let probe = takes_probe(case);
        let prefix = if probe { PROBE_DECL } else { "" };
        let n = match case.form {
            Form::Long => case.long(),
            Form::TallestClean => tallest_clean_count(case, |body| {
                parse_kind(kind, &format!("{prefix}{body}"))
                    .errors
                    .is_empty()
            }),
        };
        let src = format!("{prefix}{}", case.source(n));
        let parsed = parse_kind(kind, &src);
        let (deepest_offset, deepest_depth) = deepest_token(&parsed.root);
        Session {
            height: tree_height(&parsed.root),
            shape_height: tree_height(&parse_kind(kind, &case.shape.source(n)).root),
            deepest: (position_of(&src, deepest_offset), deepest_depth),
            src,
            kind,
            probe,
        }
    }
}

/// The node height of `root`, the root counting as 1. Iterative, so a tall
/// tree cannot overflow it.
fn tree_height(root: &SyntaxNode) -> usize {
    let (mut depth, mut height) = (0usize, 0usize);
    for event in root.preorder() {
        match event {
            WalkEvent::Enter(_) => {
                depth += 1;
                height = height.max(depth);
            }
            WalkEvent::Leave(_) => depth -= 1,
        }
    }
    height
}

/// The start offset of the deepest non-trivia token with text, and how many
/// nodes sit above it.
fn deepest_token(root: &SyntaxNode) -> (usize, usize) {
    let (mut depth, mut best) = (0usize, (0usize, 0usize));
    for event in root.preorder_with_tokens() {
        match event {
            WalkEvent::Enter(NodeOrToken::Node(_)) => depth += 1,
            WalkEvent::Leave(NodeOrToken::Node(_)) => depth -= 1,
            WalkEvent::Enter(NodeOrToken::Token(t))
                if depth > best.1 && !t.kind().is_trivia() && !t.text().is_empty() =>
            {
                best = (usize::from(t.text_range().start()), depth);
            }
            WalkEvent::Enter(NodeOrToken::Token(_)) | WalkEvent::Leave(NodeOrToken::Token(_)) => {}
        }
    }
    best
}

/// The LSP position of byte `offset` in ASCII `src`, where a byte column is a
/// UTF-16 one.
fn position_of(src: &str, offset: usize) -> Position {
    let before = &src[..offset];
    let line = before.matches('\n').count();
    let column = before.rfind('\n').map_or(offset, |nl| offset - nl - 1);
    Position::new(line as u32, column as u32)
}

/// The client half of an in-memory connection to a server thread.
struct Client {
    conn: Connection,
    next_id: i32,
}

impl Client {
    /// Send a request and wait for its result, skipping the notifications the
    /// server sends meanwhile.
    fn request<R: RequestTrait>(&mut self, params: R::Params) -> serde_json::Value {
        let id = RequestId::from(self.next_id);
        self.next_id += 1;
        let req = Request {
            id: id.clone(),
            method: R::METHOD.to_string(),
            params: serde_json::to_value(params).expect("serialise request params"),
        };
        self.conn
            .sender
            .send(Message::Request(req))
            .expect("send request");
        loop {
            match self.conn.receiver.recv_timeout(REQUEST_TIMEOUT) {
                Ok(Message::Response(resp)) if resp.id == id => {
                    return resp.response_result.unwrap_or_else(|error| {
                        panic!("{} answered with an error: {error:?}", R::METHOD)
                    });
                }
                Ok(Message::Response(resp)) => panic!("unexpected response {resp:?}"),
                Ok(Message::Request(_) | Message::Notification(_)) => {}
                Err(e) => panic!("no answer to {} within {REQUEST_TIMEOUT:?}: {e}", R::METHOD),
            }
        }
    }

    fn notify<N: NotificationTrait>(&self, params: N::Params) {
        let not = Notification {
            method: N::METHOD.to_string(),
            params: serde_json::to_value(params).expect("serialise notification params"),
        };
        self.conn
            .sender
            .send(Message::Notification(not))
            .expect("send notification");
    }
}

fn position_params(uri: &Uri, position: Position) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position,
    }
}

fn completion(client: &mut Client, uri: &Uri, position: Position) -> serde_json::Value {
    client.request::<Completion>(CompletionParams {
        text_document_position: position_params(uri, position),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: None,
    })
}

/// Serve `session` from a server whose loop runs on a `stack_bytes` thread,
/// and ask every question that reads its tree.
fn exercise_server(session: Session, stack_bytes: usize) {
    let project = tempfile::TempDir::new().expect("a project directory");
    let root = project.path().to_path_buf();
    let file_name = match session.kind {
        Kind::Impl => "Probe.fs",
        Kind::Sig => "Probe.fsi",
    };
    let (server, client) = Connection::memory();
    let (uri_tx, uri_rx) = mpsc::channel();
    let src = session.src.clone();
    // The state is built on the server's thread: it holds rowan trees, which
    // are not `Send`.
    let server = std::thread::Builder::new()
        .name("server".into())
        .stack_size(stack_bytes)
        .spawn(move || {
            let (state, uris) = runtime_project_state_in(&root, &[(file_name, &src)]);
            uri_tx.send(uris[0].clone()).expect("hand the URI over");
            run(server, state).expect("server loop");
        })
        .expect("spawn the server thread");
    let uri: Uri = uri_rx.recv().expect("the server thread built its project");
    let mut client = Client {
        conn: client,
        next_id: 0,
    };
    client.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: uri.clone(),
            language_id: "fsharp".into(),
            version: 1,
            text: session.src.clone(),
        },
    });
    let doc = TextDocumentIdentifier { uri: uri.clone() };
    client.request::<DocumentDiagnosticRequest>(DocumentDiagnosticParams {
        text_document: doc.clone(),
        identifier: None,
        previous_result_id: None,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    });
    client.request::<SemanticTokensFullRequest>(SemanticTokensParams {
        text_document: doc.clone(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    });
    client.request::<DocumentSymbolRequest>(DocumentSymbolParams {
        text_document: doc,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    });
    let deepest = session.deepest.0;
    client.request::<HoverRequest>(HoverParams {
        text_document_position_params: position_params(&uri, deepest),
        work_done_progress_params: WorkDoneProgressParams::default(),
    });
    client.request::<GotoDefinition>(GotoDefinitionParams {
        text_document_position_params: position_params(&uri, deepest),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    });
    client.request::<References>(ReferenceParams {
        text_document_position: position_params(&uri, deepest),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration: true,
        },
    });
    completion(&mut client, &uri, deepest);
    if session.probe {
        let answer = completion(
            &mut client,
            &uri,
            Position::new(0, PROBE_COMPLETION_COLUMN as u32),
        );
        assert!(
            answer.to_string().contains("\"Length\""),
            "completion inside the probe's `.Length` must offer String's members, so that \
             it is known to have reached the tree; it answered {answer}"
        );
    }
    client.request::<Shutdown>(());
    client.notify::<Exit>(());
    server.join().expect("the server thread panicked");
    drop(project);
}

/// A case spelled for the child: its index and the stack to run the loop on.
fn spell_case(index: usize, stack_bytes: usize) -> String {
    format!("{index} {stack_bytes}")
}

/// Child entry: serve one case from a server on the stack the parent asked
/// for, and report the deepest token it asked about (or that it skipped the
/// case as shallow).
#[test]
#[ignore = "child-process entry for walk_stack_bound"]
fn probe_child() {
    let Some(spec) = stack_probe::requested_case(CASE_VAR) else {
        return;
    };
    let (index, stack_bytes) = spec.split_once(' ').expect("index and stack");
    let case = cases()[index.parse::<usize>().expect("a case index")];
    let stack_bytes: usize = stack_bytes.parse().expect("a stack size");
    // The file is built, and its tallest clean length searched for, on a
    // thread of the server's full stack: the parser's own recursion is
    // calibrated for that, and only the session is under test here.
    let session = stack_probe::on_stack(SERVER_STACK_BYTES, move || Session::of(&case));
    if session.shape_height < SHALLOW {
        stack_probe::report(&format!("shallow {}", session.height));
        return;
    }
    let deepest = session.deepest.1;
    exercise_server(session, stack_bytes);
    stack_probe::report(&format!("answered {deepest}"));
}

#[test]
fn every_request_fits_half_the_server_stack() {
    let cases = cases();
    let outcomes = par_map(cases.len(), |i| {
        stack_probe::run_case(CHILD, CASE_VAR, &spell_case(i, SERVER_STACK_BYTES / 2))
    });
    let mut overflowed = Vec::new();
    let mut deepest_asked = 0;
    for (case, outcome) in cases.iter().zip(&outcomes) {
        match outcome {
            Outcome::Overflowed => overflowed.push(case.name()),
            Outcome::Survived(facts) => {
                if let Some(depth) = facts.strip_prefix("answered ") {
                    let depth: usize = depth.parse().expect("a depth");
                    deepest_asked = deepest_asked.max(depth);
                }
            }
        }
    }
    assert!(
        overflowed.is_empty(),
        "the server overflowed a {} MiB stack, half of SERVER_STACK_BYTES, answering \
         about these cases: some walk's cost per tree level has outgrown the margin:\n{}",
        (SERVER_STACK_BYTES / 2) >> 20,
        overflowed.join("\n")
    );
    assert!(
        deepest_asked >= REACH,
        "the deepest token asked about sat {deepest_asked} nodes down, short of {REACH}: \
         the cases do not reach the height the bound is about"
    );
}
