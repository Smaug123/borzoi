//! End-to-end JSON-RPC tests against [`borzoi::server::run`].
//!
//! Drives the server over an in-memory `lsp_server::Connection` pair: one
//! half runs the actual dispatch loop on a background thread, the other
//! half is the "client" sending requests and receiving responses. Earlier
//! handler tests called `handle` directly and bypassed serialisation; these
//! tests round-trip through `Request`/`Response` JSON to catch shape
//! mismatches between the rust types and the wire format.
//!
//! Single-test discipline because spinning up a server thread per test is
//! cheap but not free; the per-handler tests pin the *algorithm*, this
//! file pins the *wire contract*: every advertised capability answers,
//! every nothing-to-show response is `Ok(null)` not an error envelope.

use std::thread;
use std::time::Duration;

use borzoi::server::{State, run, server_capabilities};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument, Exit,
    Notification as NotificationTrait, PublishDiagnostics,
};
use lsp_types::request::{
    DocumentDiagnosticRequest, DocumentSymbolRequest, GotoDefinition, HoverRequest, References,
    Request as RequestTrait, SemanticTokensFullRequest, Shutdown, WorkspaceDiagnosticRefresh,
    WorkspaceDiagnosticRequest, WorkspaceSymbolRequest,
};
use lsp_types::{
    ClientCapabilities, DiagnosticClientCapabilities, DiagnosticServerCapabilities,
    DiagnosticWorkspaceClientCapabilities, DidChangeTextDocumentParams,
    DidChangeWatchedFilesClientCapabilities, DidChangeWatchedFilesParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentDiagnosticParams,
    DocumentDiagnosticReport, DocumentDiagnosticReportResult, DocumentSymbolClientCapabilities,
    DocumentSymbolParams, DocumentSymbolResponse, FileChangeType, FileEvent, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, Location, MarkupKind, OneOf,
    PartialResultParams, Position, PublishDiagnosticsClientCapabilities, PublishDiagnosticsParams,
    ReferenceContext, ReferenceParams, RegistrationParams, SemanticTokenType, SemanticTokensParams,
    SemanticTokensResult, SemanticTokensServerCapabilities, ServerCapabilities, SymbolKind,
    TextDocumentClientCapabilities, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams, Url, VersionedTextDocumentIdentifier,
    WorkDoneProgressParams, WorkspaceClientCapabilities, WorkspaceDiagnosticParams,
    WorkspaceDiagnosticReportResult, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};

/// Modern client capabilities: hierarchical documentSymbol opt-in. The
/// integration tests run as a "good" client so handlers return their
/// richer response shapes; the negotiation fallback to flat symbols is
/// pinned separately in `handlers_document_symbol.rs`.
fn hierarchical_caps() -> ClientCapabilities {
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            document_symbol: Some(DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A client that advertises both diagnostic protocols, as VS Code does. The
/// presence of `textDocument.diagnostic` selects pull; its inner booleans only
/// refine which pull features the client supports.
fn pull_diagnostic_caps() -> ClientCapabilities {
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            diagnostic: Some(DiagnosticClientCapabilities::default()),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities::default()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A pull-diagnostic client that accepts the global refresh request used after
/// watched diagnostic inputs change.
fn pull_diagnostic_refresh_caps() -> ClientCapabilities {
    let mut caps = pull_diagnostic_caps();
    caps.workspace = Some(WorkspaceClientCapabilities {
        diagnostic: Some(DiagnosticWorkspaceClientCapabilities {
            refresh_support: Some(true),
        }),
        ..Default::default()
    });
    caps
}

/// A live in-memory server: spawns the dispatch loop on a background
/// thread, holds the client half of the connection plus a handle to the
/// thread so the test can drive requests and gracefully shut down.
struct Server {
    client: Connection,
    thread: Option<thread::JoinHandle<()>>,
    next_id: i32,
}

impl Server {
    /// Spin up a server preloaded with the given buffers (`uri → text`)
    /// and the hierarchical-documentSymbol client capability. The server
    /// thread runs until the test sends `shutdown` + `exit`.
    ///
    /// `State` isn't `Send` (rowan's red layer is per-thread), so the test
    /// hands the docs across by value and builds state *inside* the
    /// spawned closure.
    fn start(initial_docs: Vec<(Url, String)>) -> Self {
        Self::start_with_caps(initial_docs, hierarchical_caps())
    }

    /// Like [`Self::start`] but with explicit client capabilities — used to
    /// exercise capability-gated behaviour (e.g. file-watcher registration).
    fn start_with_caps(initial_docs: Vec<(Url, String)>, caps: ClientCapabilities) -> Self {
        let (server, client) = Connection::memory();
        let thread = thread::spawn(move || {
            let mut state = State::default();
            state.set_client_capabilities(caps);
            for (uri, text) in initial_docs {
                state.docs.insert(uri, text);
            }
            run(server, state).expect("server::run terminated cleanly");
        });
        Server {
            client,
            thread: Some(thread),
            next_id: 0,
        }
    }

    /// Send a typed request and wait for the matched response. Asserts the
    /// response has no `error` field and returns its `result` deserialised
    /// as `R::Result`. The Option-or-not semantics live in the method's own
    /// `Result` type (every handler returns `Option<X>`, so a `null` answer
    /// deserialises to `None`).
    fn request<R>(&mut self, params: R::Params) -> R::Result
    where
        R: RequestTrait,
        R::Params: serde::Serialize,
        R::Result: serde::de::DeserializeOwned,
    {
        let id = self.fresh_id();
        let req = Request {
            id: id.clone(),
            method: R::METHOD.to_string(),
            params: serde_json::to_value(params).expect("serialise request params"),
        };
        self.client
            .sender
            .send(Message::Request(req))
            .expect("send request");
        let resp = self
            .client
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("receive response within 5s");
        match resp {
            Message::Response(Response {
                id: rid,
                result,
                error,
            }) => {
                assert_eq!(rid, id, "response id mismatched the request");
                assert!(
                    error.is_none(),
                    "expected Ok response, got error: {error:?}"
                );
                let value = result.expect("response must carry a result");
                serde_json::from_value(value).expect("deserialise result")
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    /// Send a typed client notification. A subsequent request acts as an
    /// ordering barrier: because the server loop is serial, any notification it
    /// emits while handling this message arrives before that request's response.
    fn notify<N>(&self, params: N::Params)
    where
        N: NotificationTrait,
        N::Params: serde::Serialize,
    {
        let notif = Notification {
            method: N::METHOD.to_string(),
            params: serde_json::to_value(params).expect("serialise notification params"),
        };
        self.client
            .sender
            .send(Message::Notification(notif))
            .expect("send notification");
    }

    /// Receive one typed server notification.
    fn receive_notification<N>(&self) -> N::Params
    where
        N: NotificationTrait,
        N::Params: serde::de::DeserializeOwned,
    {
        let msg = self
            .client
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("receive notification within 5s");
        match msg {
            Message::Notification(not) => {
                assert_eq!(not.method, N::METHOD, "notification method mismatched");
                serde_json::from_value(not.params).expect("deserialise notification params")
            }
            other => panic!("expected Notification, got {other:?}"),
        }
    }

    fn fresh_id(&mut self) -> RequestId {
        let id = self.next_id;
        self.next_id += 1;
        RequestId::from(id)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Best-effort graceful shutdown. The server's `handle_shutdown` will
        // respond to `shutdown`, then wait for `exit`. Once it returns, the
        // dispatch loop exits and the thread joins.
        let id = self.fresh_id();
        let shutdown = Request {
            id,
            method: Shutdown::METHOD.to_string(),
            params: serde_json::Value::Null,
        };
        let _ = self.client.sender.send(Message::Request(shutdown));
        // Drain the shutdown response (we don't check it on drop).
        let _ = self.client.receiver.recv_timeout(Duration::from_secs(2));
        let exit = Notification {
            method: Exit::METHOD.to_string(),
            params: serde_json::Value::Null,
        };
        let _ = self.client.sender.send(Message::Notification(exit));
        if let Some(handle) = self.thread.take()
            && let Err(err) = handle.join()
        {
            // Don't panic in drop (would mask an earlier test failure);
            // surface via stderr instead.
            eprintln!("server thread panicked during shutdown: {err:?}");
        }
    }
}

fn doc_position_params(uri: &Url, line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: Position { line, character },
    }
}

fn diagnostic_params(uri: &Url) -> DocumentDiagnosticParams {
    DocumentDiagnosticParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        identifier: None,
        previous_result_id: None,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

fn workspace_diagnostic_params() -> WorkspaceDiagnosticParams {
    WorkspaceDiagnosticParams {
        identifier: None,
        previous_result_ids: Vec::new(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

fn did_open(uri: &Url, text: &str) -> DidOpenTextDocumentParams {
    DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: uri.clone(),
            language_id: "fsharp".to_string(),
            version: 1,
            text: text.to_string(),
        },
    }
}

fn did_change(uri: &Url, version: i32, text: &str) -> DidChangeTextDocumentParams {
    DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.clone(),
            version,
        },
        content_changes: vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_string(),
        }],
    }
}

// ---- capability audit ----

/// All four sema-backed providers must be advertised. The end-to-end
/// audit guard against accidental capability deletion.
#[test]
fn server_advertises_every_sema_backed_provider() {
    let caps = server_capabilities();
    let json = serde_json::to_value(&caps).expect("serialise");
    // Round-trip through JSON catches shapes that would otherwise be
    // hidden by the strongly-typed `ServerCapabilities`.
    let parsed: ServerCapabilities = serde_json::from_value(json).expect("deserialise");
    assert!(parsed.hover_provider.is_some(), "hover_provider unset");
    // Member (`recv.`) completion (Stage 3.3b), triggered on `.`.
    let completion = parsed
        .completion_provider
        .expect("completion_provider unset");
    assert_eq!(
        completion.trigger_characters.as_deref(),
        Some(&[".".to_string()][..]),
        "completion trigger characters unset / wrong"
    );
    assert!(
        matches!(parsed.document_symbol_provider, Some(OneOf::Left(true))),
        "document_symbol_provider unset / wrong shape"
    );
    assert!(
        matches!(parsed.definition_provider, Some(OneOf::Left(true))),
        "definition_provider unset / wrong shape"
    );
    assert!(
        matches!(parsed.references_provider, Some(OneOf::Left(true))),
        "references_provider unset / wrong shape"
    );
    assert!(
        matches!(parsed.workspace_symbol_provider, Some(OneOf::Left(true))),
        "workspace_symbol_provider unset / wrong shape"
    );
    assert!(
        parsed.text_document_sync.is_some(),
        "text_document_sync unset"
    );
    // Pull diagnostics: advertised with inter-file dependencies (the `#line`
    // relocation) and workspace-wide pull (`workspace/diagnostic`).
    match parsed.diagnostic_provider {
        Some(DiagnosticServerCapabilities::Options(opts)) => {
            assert!(
                opts.inter_file_dependencies,
                "diagnostic_provider must declare inter_file_dependencies"
            );
            assert!(
                opts.workspace_diagnostics,
                "workspace_diagnostics must be advertised"
            );
        }
        other => panic!("diagnostic_provider unset / wrong shape: {other:?}"),
    }
}

/// End-to-end over the wire: a `semanticTokens/full` request on an open `.fs`
/// buffer round-trips through the real dispatch and JSON serialisation, and the
/// identifier `f` in `let f a = a` comes back classified as a function — proving
/// the semantic layer reaches the wire, not just the `handle` unit tests.
#[test]
fn semantic_tokens_full_classifies_identifiers_over_the_wire() {
    let uri = Url::parse("inmemory:///Tokens.fs").unwrap();
    let mut server = Server::start(vec![(uri.clone(), "let f a = a\n".to_string())]);
    let result: Option<SemanticTokensResult> =
        server.request::<SemanticTokensFullRequest>(SemanticTokensParams {
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            text_document: TextDocumentIdentifier { uri },
        });
    let Some(SemanticTokensResult::Tokens(tokens)) = result else {
        panic!("expected a full token set, got {result:?}");
    };

    // The `token_type` field is an index into the advertised legend; read the
    // `function` index from the capability rather than hard-coding it, so this
    // test can't silently drift from the legend the server actually publishes.
    let legend = match server_capabilities().semantic_tokens_provider {
        Some(SemanticTokensServerCapabilities::SemanticTokensOptions(opts)) => opts.legend,
        other => panic!("semantic tokens capability unset / wrong shape: {other:?}"),
    };
    let function_idx = legend
        .token_types
        .iter()
        .position(|t| *t == SemanticTokenType::FUNCTION)
        .expect("FUNCTION advertised in the legend") as u32;

    // Decode the delta-encoded stream to absolute (line, col, len, type) tuples.
    let mut line = 0u32;
    let mut col = 0u32;
    let mut decoded = Vec::new();
    for t in &tokens.data {
        if t.delta_line == 0 {
            col += t.delta_start;
        } else {
            line += t.delta_line;
            col = t.delta_start;
        }
        decoded.push((line, col, t.length, t.token_type));
    }
    assert!(
        decoded
            .iter()
            .any(|&(l, c, _, ty)| l == 0 && c == 4 && ty == function_idx),
        "`f` (line 0, col 4) should be a function; got {decoded:?}"
    );
}

/// A client that supports dynamic registration for watched files.
fn watched_files_caps() -> ClientCapabilities {
    ClientCapabilities {
        workspace: Some(WorkspaceClientCapabilities {
            did_change_watched_files: Some(DidChangeWatchedFilesClientCapabilities {
                dynamic_registration: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// When the client supports dynamic registration, the server proactively sends
/// a `client/registerCapability` for `workspace/didChangeWatchedFiles` at the
/// start of the loop, so the client knows to watch the project / source files.
#[test]
fn registers_file_watchers_when_client_supports_it() {
    let server = Server::start_with_caps(vec![], watched_files_caps());
    // The registration is the first message the server sends, before handling
    // any request.
    let msg = server
        .client
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("a registration message within 5s");
    let req = match msg {
        Message::Request(req) => req,
        other => panic!("expected client/registerCapability first, got {other:?}"),
    };
    assert_eq!(req.method, "client/registerCapability");
    let params: RegistrationParams =
        serde_json::from_value(req.params).expect("registration params");
    assert_eq!(params.registrations.len(), 1, "{params:?}");
    assert_eq!(
        params.registrations[0].method,
        "workspace/didChangeWatchedFiles"
    );
}

/// The mirror of the above: the default test client (no watched-files
/// capability) gets no registration — the first message it receives is the
/// response to its own request, not an unsolicited `client/registerCapability`.
#[test]
fn no_file_watcher_registration_without_capability() {
    let uri = Url::parse("inmemory:///A.fs").unwrap();
    let mut server = Server::start(vec![(uri.clone(), "let x = 1\n".to_string())]);
    // A normal request round-trips cleanly; if a stray registration had been
    // sent first, `request` would observe a `Request` where it expects a
    // `Response` and panic.
    let resp: DocumentDiagnosticReportResult =
        server.request::<DocumentDiagnosticRequest>(DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri },
            identifier: None,
            previous_result_id: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        });
    assert!(matches!(
        resp,
        DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(_))
    ));
}

/// Regression: a late response to the fire-and-forget `client/registerCapability`
/// arriving during the `shutdown`→`exit` window must not break shutdown. (The
/// previous `Connection::handle_shutdown` rejected any response there, erroring
/// `run`.) Driven raw, bypassing the `Server` harness, so the interleaving is
/// exact.
#[test]
fn late_registration_response_does_not_break_shutdown() {
    let (server_conn, client) = Connection::memory();
    let handle = thread::spawn(move || {
        let mut state = State::default();
        state.set_client_capabilities(watched_files_caps());
        run(server_conn, state)
    });

    // 1. The server sends its registration request first; capture its id.
    let reg_id = match client
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("registration request")
    {
        Message::Request(req) => {
            assert_eq!(req.method, "client/registerCapability");
            req.id
        }
        other => panic!("expected the registration request, got {other:?}"),
    };

    // 2. Client requests shutdown; the server acknowledges.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: Shutdown::METHOD.to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    match client
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("shutdown response")
    {
        Message::Response(_) => {}
        other => panic!("expected the shutdown response, got {other:?}"),
    }

    // 3. The late registration response lands in the shutdown→exit window...
    client
        .sender
        .send(Message::Response(Response {
            id: reg_id,
            result: Some(serde_json::Value::Null),
            error: None,
        }))
        .unwrap();
    // 4. ...then exit.
    client
        .sender
        .send(Message::Notification(Notification {
            method: Exit::METHOD.to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    // The server must have terminated cleanly, not errored on the late response.
    let result = handle.join().expect("server thread joined");
    assert!(result.is_ok(), "run errored during shutdown: {result:?}");
}

/// Lifecycle: once `shutdown` is acknowledged, a further request is rejected
/// with an error (and not serviced) until `exit`.
#[test]
fn requests_after_shutdown_are_rejected() {
    let (server_conn, client) = Connection::memory();
    let handle = thread::spawn(move || {
        let mut state = State::default();
        state.set_client_capabilities(hierarchical_caps());
        run(server_conn, state)
    });

    // shutdown → ok ack
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: Shutdown::METHOD.to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    match client
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("shutdown response")
    {
        Message::Response(r) => assert!(r.error.is_none(), "{r:?}"),
        other => panic!("expected shutdown response, got {other:?}"),
    }

    // A request after shutdown must come back as an error, not a serviced
    // result. (Params are never parsed — the reject happens before dispatch.)
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/hover".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    match client
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("post-shutdown response")
    {
        Message::Response(r) => {
            assert_eq!(r.id, RequestId::from(2));
            assert!(
                r.error.is_some(),
                "a request after shutdown must be rejected: {r:?}"
            );
        }
        other => panic!("expected an error response, got {other:?}"),
    }

    client
        .sender
        .send(Message::Notification(Notification {
            method: Exit::METHOD.to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    assert!(handle.join().expect("joined").is_ok());
}

// ---- golden-path round trips ----

#[test]
fn document_symbol_round_trip() {
    let uri = Url::parse("inmemory:///A.fs").unwrap();
    let src = "let foo = 1\nlet bar x = x\n";
    let mut server = Server::start(vec![(uri.clone(), src.to_string())]);

    let resp: DocumentSymbolResponse = server
        .request::<DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .expect("a documentSymbol response");
    let nested = match resp {
        DocumentSymbolResponse::Nested(syms) => syms,
        DocumentSymbolResponse::Flat(_) => panic!("expected hierarchical response"),
    };
    let names: Vec<_> = nested.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["foo", "bar"]);
    assert_eq!(nested[0].kind, SymbolKind::VARIABLE);
    assert_eq!(nested[1].kind, SymbolKind::FUNCTION);
}

#[test]
fn definition_round_trip() {
    let uri = Url::parse("inmemory:///A.fs").unwrap();
    let src = "let x = 1\nlet y = x\n";
    let mut server = Server::start(vec![(uri.clone(), src.to_string())]);

    let resp: GotoDefinitionResponse = server
        .request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: doc_position_params(&uri, 1, 8), // use of x
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .expect("a definition response");
    let loc = match resp {
        GotoDefinitionResponse::Scalar(loc) => loc,
        other => panic!("expected scalar response, got {other:?}"),
    };
    assert_eq!(loc.uri, uri);
    assert_eq!(loc.range.start.character, 4); // binder `x` at column 4
}

#[test]
fn references_round_trip() {
    let uri = Url::parse("inmemory:///A.fs").unwrap();
    let src = "let x = 1\nlet y = x + x\n";
    let mut server = Server::start(vec![(uri.clone(), src.to_string())]);

    let locs: Vec<Location> = server
        .request::<References>(ReferenceParams {
            text_document_position: doc_position_params(&uri, 0, 4), // binder x
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        })
        .expect("a references response");
    assert_eq!(locs.len(), 3, "{locs:#?}");
    assert!(locs.iter().all(|l| l.uri == uri));
}

#[test]
fn workspace_symbol_round_trip() {
    let uri = Url::parse("inmemory:///A.fs").unwrap();
    let src = "let foo = 1\nlet bar x = x\n";
    let mut server = Server::start(vec![(uri.clone(), src.to_string())]);

    let resp: WorkspaceSymbolResponse = server
        .request::<WorkspaceSymbolRequest>(WorkspaceSymbolParams {
            query: "foo".to_string(),
            partial_result_params: PartialResultParams::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .expect("a workspaceSymbol response");
    let flat = match resp {
        WorkspaceSymbolResponse::Flat(symbols) => symbols,
        WorkspaceSymbolResponse::Nested(_) => panic!("expected the flat response shape"),
    };
    // The query filters to `foo`; `bar` is excluded.
    assert_eq!(flat.len(), 1, "{flat:#?}");
    assert_eq!(flat[0].name, "foo");
    assert_eq!(flat[0].kind, SymbolKind::VARIABLE);
    assert_eq!(flat[0].location.uri, uri);
}

#[test]
fn document_diagnostic_round_trip() {
    let uri = Url::parse("inmemory:///A.fs").unwrap();
    // An orphan `#endif` is a structural directive error, reported regardless
    // of the active symbol set (so no `.fsproj` is needed).
    let mut server = Server::start(vec![(uri.clone(), "#endif\n".to_string())]);

    let resp: DocumentDiagnosticReportResult =
        server.request::<DocumentDiagnosticRequest>(DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            identifier: None,
            previous_result_id: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        });
    let report = match resp {
        DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) => full,
        other => panic!("expected a Full report, got {other:?}"),
    };
    assert!(
        !report.full_document_diagnostic_report.items.is_empty(),
        "an unmatched #endif must surface as a pulled diagnostic: {report:?}"
    );
    assert!(
        report.related_documents.is_none(),
        "no #line relocation in this fixture"
    );

    // Echoing the result_id back (nothing changed) yields an `Unchanged` report
    // over the wire.
    let result_id = report
        .full_document_diagnostic_report
        .result_id
        .expect("a Full report carries a result_id");
    let resp: DocumentDiagnosticReportResult =
        server.request::<DocumentDiagnosticRequest>(DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri },
            identifier: None,
            previous_result_id: Some(result_id),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        });
    assert!(
        matches!(
            resp,
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Unchanged(_))
        ),
        "echoing the result_id must yield Unchanged: {resp:?}"
    );
}

/// Regression for #112: once a client advertises the pull model, no document
/// lifecycle or watched-file notification may make the server publish the same
/// diagnostics as a push. Each pull request below is also an ordering barrier:
/// the request helper would observe and reject any earlier unsolicited publish.
#[test]
fn pull_client_never_receives_publish_diagnostics() {
    let uri = Url::parse("inmemory:///A.fs").unwrap();
    let mut server = Server::start_with_caps(vec![], pull_diagnostic_caps());

    server.notify::<DidOpenTextDocument>(did_open(&uri, "#endif\n"));
    let opened = server.request::<DocumentDiagnosticRequest>(diagnostic_params(&uri));
    let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(opened)) = opened
    else {
        panic!("opening an invalid buffer must produce a Full pull report");
    };
    assert!(
        !opened.full_document_diagnostic_report.items.is_empty(),
        "the pull response must contain the diagnostic"
    );

    server.notify::<DidChangeTextDocument>(did_change(&uri, 2, "let x = 1\n"));
    let changed = server.request::<DocumentDiagnosticRequest>(diagnostic_params(&uri));
    let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(changed)) = changed
    else {
        panic!("changing to a clean buffer must produce a Full pull report");
    };
    assert!(changed.full_document_diagnostic_report.items.is_empty());

    server.notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
        changes: vec![FileEvent {
            uri: Url::parse("file:///workspace/App.fsproj").unwrap(),
            typ: FileChangeType::CHANGED,
        }],
    });
    let watched = server.request::<DocumentDiagnosticRequest>(diagnostic_params(&uri));
    assert!(matches!(
        watched,
        DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(_))
    ));

    server.notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
    });
    let closed = server.request::<DocumentDiagnosticRequest>(diagnostic_params(&uri));
    let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(closed)) = closed
    else {
        panic!("a closed in-memory buffer must produce an empty Full pull report");
    };
    assert!(closed.full_document_diagnostic_report.items.is_empty());
}

/// Structural changes and external source-content edits can stale diagnostics
/// across the whole workspace. Pull clients that advertise refresh support are
/// asked to re-request them, even when the server has no open buffers to
/// republish.
#[test]
fn watched_diagnostic_inputs_request_diagnostic_refresh() {
    for changed_uri in ["file:///workspace/App.fsproj", "file:///workspace/Other.fs"] {
        let server = Server::start_with_caps(vec![], pull_diagnostic_refresh_caps());

        server.notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
            changes: vec![FileEvent {
                uri: Url::parse(changed_uri).unwrap(),
                typ: FileChangeType::CHANGED,
            }],
        });

        let request = match server
            .client
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("diagnostic refresh request within 5s")
        {
            Message::Request(request) => request,
            other => panic!("expected workspace/diagnostic/refresh, got {other:?}"),
        };
        assert_eq!(request.method, WorkspaceDiagnosticRefresh::METHOD);
        assert_eq!(request.params, serde_json::Value::Null);
        server
            .client
            .sender
            .send(Message::Response(Response {
                id: request.id,
                result: Some(serde_json::Value::Null),
                error: None,
            }))
            .unwrap();
    }
}

/// A structural change does not send the request to a client without refresh
/// support. The workspace pull is an ordering barrier that would encounter any
/// stray reverse request first.
#[test]
fn diagnostic_refresh_is_capability_gated() {
    let mut server = Server::start_with_caps(vec![], pull_diagnostic_caps());
    server.notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
        changes: vec![FileEvent {
            uri: Url::parse("file:///workspace/App.fsproj").unwrap(),
            typ: FileChangeType::CHANGED,
        }],
    });
    let report = server.request::<WorkspaceDiagnosticRequest>(workspace_diagnostic_params());
    assert!(matches!(report, WorkspaceDiagnosticReportResult::Report(_)));
}

/// Clients without `textDocument.diagnostic` retain the complete stateful push
/// lifecycle: publish on open/change/watched structural changes, and clear on
/// close. This is the compatibility half of the one-delivery-mode invariant.
#[test]
fn push_client_retains_publish_diagnostics_lifecycle() {
    let uri = Url::parse("inmemory:///A.fs").unwrap();
    let server = Server::start_with_caps(vec![], hierarchical_caps());

    server.notify::<DidOpenTextDocument>(did_open(&uri, "#endif\n"));
    let opened = server.receive_notification::<PublishDiagnostics>();
    assert_eq!(opened.uri, uri);
    assert!(!opened.diagnostics.is_empty());

    server.notify::<DidChangeTextDocument>(did_change(&uri, 2, "let x = 1\n"));
    let changed = server.receive_notification::<PublishDiagnostics>();
    assert_eq!(changed.uri, uri);
    assert!(changed.diagnostics.is_empty());

    server.notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
        changes: vec![FileEvent {
            uri: Url::parse("file:///workspace/App.fsproj").unwrap(),
            typ: FileChangeType::CHANGED,
        }],
    });
    let watched = server.receive_notification::<PublishDiagnostics>();
    assert_eq!(watched.uri, uri);
    assert!(watched.diagnostics.is_empty());

    server.notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
    });
    let closed: PublishDiagnosticsParams = server.receive_notification::<PublishDiagnostics>();
    assert_eq!(closed.uri, uri);
    assert!(closed.diagnostics.is_empty());
}

#[test]
fn workspace_diagnostic_round_trip() {
    // The harness configures no workspace roots, so the wire contract we pin
    // here is "a well-formed empty Report" (enumeration over real project trees
    // is covered by the handler's unit tests).
    let mut server = Server::start(vec![]);

    let resp: WorkspaceDiagnosticReportResult =
        server.request::<WorkspaceDiagnosticRequest>(workspace_diagnostic_params());
    match resp {
        WorkspaceDiagnosticReportResult::Report(report) => assert!(report.items.is_empty()),
        other => panic!("expected an (empty) Report, got {other:?}"),
    }
}

#[test]
fn hover_round_trip() {
    let uri = Url::parse("inmemory:///A.fs").unwrap();
    let src = "let foo = 1\n";
    let mut server = Server::start(vec![(uri.clone(), src.to_string())]);

    let hover: Hover = server
        .request::<HoverRequest>(HoverParams {
            text_document_position_params: doc_position_params(&uri, 0, 4),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .expect("a hover response");
    match hover.contents {
        HoverContents::Markup(m) => {
            assert_eq!(m.kind, MarkupKind::Markdown);
            // `foo` is typed `int` from its literal RHS, surfaced on the hover.
            assert_eq!(m.value, "`foo : int` — value");
        }
        other => panic!("expected markup contents, got {other:?}"),
    }
}

// ---- D5 soundness pin ----

/// A file with only an unresolvable name. The handlers that *navigate* or
/// *fabricate* a result must still answer `null`/empty rather than guess — D5:
/// pre-Phase-3 we can't resolve `nowhere`, so we don't send the cursor anywhere.
/// Hover is the one exception: it now *explains* why definition is unavailable
/// (it doesn't claim what `nowhere` is), which is honest, not a guess.
#[test]
fn unresolvable_cursor_yields_null_for_navigating_handlers() {
    let uri = Url::parse("inmemory:///A.fs").unwrap();
    let src = "let _ = nowhere\n";
    let mut server = Server::start(vec![(uri.clone(), src.to_string())]);

    // Definition — still null: we won't send the cursor to a guessed location.
    let def = server.request::<GotoDefinition>(GotoDefinitionParams {
        text_document_position_params: doc_position_params(&uri, 0, 9),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    });
    assert!(def.is_none(), "definition should be null, got {def:?}");

    // Hover — no longer null: it explains *why* go-to-definition finds nothing,
    // without fabricating what the symbol is.
    let hover = server
        .request::<HoverRequest>(HoverParams {
            text_document_position_params: doc_position_params(&uri, 0, 9),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .expect("hover explains the unresolved name rather than returning null");
    match hover.contents {
        HoverContents::Markup(m) => {
            assert!(
                m.value.starts_with("**No definition available**"),
                "got {:?}",
                m.value
            );
        }
        other => panic!("expected markup contents, got {other:?}"),
    }

    // References — empty list, not null, so the client clears its panel.
    let refs: Vec<Location> = server
        .request::<References>(ReferenceParams {
            text_document_position: doc_position_params(&uri, 0, 9),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        })
        .expect("references returns Some(empty), never None");
    assert!(refs.is_empty(), "references should be empty, got {refs:?}");

    // documentSymbol — the document still has *one* binding (`_`) that
    // would resolve, but `_` is a wildcard, not exported. The outline is
    // empty rather than `null`; this pins both halves of the spec.
    //
    // We deliberately don't pin the response *variant* here: an empty `[]`
    // is ambiguous under `DocumentSymbolResponse`'s `#[serde(untagged)]`
    // attribute (Flat is listed first, so serde picks it on the empty
    // case). The contract under test is that the outline is empty, not
    // which JSON variant the empty list deserialised as.
    let syms: DocumentSymbolResponse = server
        .request::<DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .expect("documentSymbol returns Some, never None");
    let count = match syms {
        DocumentSymbolResponse::Nested(s) => s.len(),
        DocumentSymbolResponse::Flat(s) => s.len(),
    };
    assert_eq!(count, 0, "outline should be empty");
}
