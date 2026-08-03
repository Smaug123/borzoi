//! End-to-end: a project whose evaluation the LSP declines produces a
//! `window/showMessage` on the wire saying why.
//!
//! The pure decision-and-wording lives in `borzoi::project_deferral` and is
//! property-tested there. What these tests pin is the wiring around it, which
//! is where the feature was previously broken: *when* the message is sent (a
//! source buffer, a `.fsproj` buffer, a re-evaluation after a watched change),
//! and that it is sent at most once per project until something changes.
//!
//! Driven over an in-memory `lsp_server::Connection` against the real dispatch
//! loop, with pull-diagnostic capabilities so the only server-initiated
//! notification in flight is the one under test.

use std::thread;
use std::time::Duration;

use borzoi::server::{State, run};
use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidOpenTextDocument, Exit,
    Notification as NotificationTrait, ShowMessage,
};
use lsp_types::request::{
    DocumentSymbolRequest, GotoDefinition, Request as RequestTrait, Shutdown,
};
use lsp_types::{
    ClientCapabilities, DiagnosticClientCapabilities, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidOpenTextDocumentParams, DocumentSymbolParams, FileChangeType,
    FileEvent, GotoDefinitionParams, PartialResultParams, Position, ShowMessageParams,
    TextDocumentClientCapabilities, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams, Url, VersionedTextDocumentIdentifier,
    WorkDoneProgressParams,
};
use tempfile::TempDir;

/// A pull-diagnostic client: `publish_diagnostics` returns early for it, so the
/// receiver carries only the deferral message and request responses.
fn pull_caps() -> ClientCapabilities {
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            diagnostic: Some(DiagnosticClientCapabilities::default()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

struct Server {
    client: Connection,
    thread: Option<thread::JoinHandle<()>>,
    next_id: i32,
}

impl Server {
    fn start() -> Self {
        let (server, client) = Connection::memory();
        let thread = thread::spawn(move || {
            let mut state = State::default();
            state.set_client_capabilities(pull_caps());
            run(server, state).expect("server::run terminated cleanly");
        });
        Server {
            client,
            thread: Some(thread),
            next_id: 0,
        }
    }

    fn notify<N: NotificationTrait>(&self, params: N::Params)
    where
        N::Params: serde::Serialize,
    {
        self.client
            .sender
            .send(Message::Notification(Notification {
                method: N::METHOD.to_string(),
                params: serde_json::to_value(params).expect("serialise params"),
            }))
            .expect("send notification");
    }

    fn open(&self, uri: &Url, text: &str, language_id: &str) {
        self.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: language_id.to_string(),
                version: 1,
                text: text.to_string(),
            },
        });
    }

    fn change(&self, uri: &Url, text: &str) {
        self.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.to_string(),
            }],
        });
    }

    /// Drive one request through and drain messages until its response arrives,
    /// returning the notifications seen on the way.
    ///
    /// The dispatch loop is serial, so any notification the server emitted while
    /// handling the *preceding* messages is already queued ahead of this
    /// response. That makes "no message was sent" a positive observation rather
    /// than a timeout — a sleep-and-hope would pass just as readily on a server
    /// that had simply not got round to sending it yet.
    fn barrier(&mut self, uri: &Url) -> Vec<Notification> {
        let id = RequestId::from(self.next_id);
        self.next_id += 1;
        self.client
            .sender
            .send(Message::Request(Request {
                id: id.clone(),
                method: DocumentSymbolRequest::METHOD.to_string(),
                params: serde_json::to_value(DocumentSymbolParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                })
                .expect("serialise params"),
            }))
            .expect("send barrier request");
        let mut seen = Vec::new();
        loop {
            match self
                .client
                .receiver
                .recv_timeout(Duration::from_secs(10))
                .expect("a response within 10s")
            {
                Message::Notification(not) => seen.push(not),
                Message::Response(resp) if resp.id == id => return seen,
                other => panic!("unexpected message while awaiting the barrier: {other:?}"),
            }
        }
    }

    /// Like [`Self::barrier`] but with a request that actually folds the
    /// project (`textDocument/definition` resolves through the Compile order),
    /// so a refusal the fold alone can discover is observed before the response
    /// comes back. `documentSymbol` is single-file and would not provoke one.
    fn fold_barrier(&mut self, uri: &Url) -> Vec<Notification> {
        let id = RequestId::from(self.next_id);
        self.next_id += 1;
        self.client
            .sender
            .send(Message::Request(Request {
                id: id.clone(),
                method: GotoDefinition::METHOD.to_string(),
                params: serde_json::to_value(GotoDefinitionParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: Position {
                            line: 1,
                            character: 4,
                        },
                    },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                })
                .expect("serialise params"),
            }))
            .expect("send fold barrier request");
        let mut seen = Vec::new();
        loop {
            match self
                .client
                .receiver
                .recv_timeout(Duration::from_secs(10))
                .expect("a response within 10s")
            {
                Message::Notification(not) => seen.push(not),
                Message::Response(resp) if resp.id == id => return seen,
                other => panic!("unexpected message while awaiting the barrier: {other:?}"),
            }
        }
    }

    fn show_messages(notifications: Vec<Notification>) -> Vec<String> {
        notifications
            .into_iter()
            .filter(|not| not.method == ShowMessage::METHOD)
            .map(|not| {
                serde_json::from_value::<ShowMessageParams>(not.params)
                    .expect("deserialise ShowMessageParams")
                    .message
            })
            .collect()
    }

    /// The deferral messages emitted after a request that folds the project.
    fn deferral_messages_after_fold(&mut self, uri: &Url) -> Vec<String> {
        let seen = self.fold_barrier(uri);
        Self::show_messages(seen)
    }

    /// The deferral messages emitted since the last barrier.
    fn deferral_messages(&mut self, uri: &Url) -> Vec<String> {
        self.barrier(uri)
            .into_iter()
            .filter(|not| not.method == ShowMessage::METHOD)
            .map(|not| {
                serde_json::from_value::<ShowMessageParams>(not.params)
                    .expect("deserialise ShowMessageParams")
                    .message
            })
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let id = RequestId::from(self.next_id + 1000);
        let _ = self.client.sender.send(Message::Request(Request {
            id,
            method: Shutdown::METHOD.to_string(),
            params: serde_json::Value::Null,
        }));
        // Drain until the loop stops; a queued notification must not be mistaken
        // for the shutdown response.
        while let Ok(msg) = self.client.receiver.recv_timeout(Duration::from_secs(2)) {
            if matches!(msg, Message::Response(_)) {
                break;
            }
        }
        let _ = self.client.sender.send(Message::Notification(Notification {
            method: Exit::METHOD.to_string(),
            params: serde_json::Value::Null,
        }));
        if let Some(handle) = self.thread.take()
            && let Err(err) = handle.join()
        {
            eprintln!("server thread panicked during shutdown: {err:?}");
        }
    }
}

/// A project whose Compile group is gated on a condition the evaluator can't
/// reduce — the shape the real-world census found most often, and one that
/// produced no message at all before this feature.
const UNCERTAIN_PROJECT: &str = r#"<Project>
  <ItemGroup Condition="Exists($([MSBuild]::GetPathOfFileAbove('Directory.Build.props')))">
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>"#;

const CERTAIN_PROJECT: &str = r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Include="B.fs" />
  </ItemGroup>
</Project>"#;

/// Lay out a project directory. Returns the temp dir plus the `.fsproj` URI.
fn project_dir(fsproj: &str) -> (TempDir, Url) {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("Demo.fsproj");
    std::fs::write(&proj, fsproj).unwrap();
    std::fs::write(tmp.path().join("A.fs"), "module A\nlet a = 1\n").unwrap();
    std::fs::write(tmp.path().join("B.fs"), "module B\nlet b = 2\n").unwrap();
    let uri = Url::from_file_path(&proj).unwrap();
    (tmp, uri)
}

fn source_uri(tmp: &TempDir, name: &str) -> Url {
    Url::from_file_path(tmp.path().join(name)).unwrap()
}

#[test]
fn opening_a_source_file_reports_its_project_s_deferral() {
    let (tmp, _proj) = project_dir(UNCERTAIN_PROJECT);
    let a = source_uri(&tmp, "A.fs");
    let mut server = Server::start();
    server.open(&a, "module A\nlet a = 1\n", "fsharp");
    let messages = server.deferral_messages(&a);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("Demo.fsproj"), "{}", messages[0]);
    // The *cause*, not merely the fact — this is what the previous
    // implementation could not produce for this project.
    assert!(
        messages[0].contains("GetPathOfFileAbove"),
        "{}",
        messages[0]
    );
    assert!(
        messages[0].contains("single-file analysis"),
        "{}",
        messages[0]
    );
}

#[test]
fn a_trustworthy_project_produces_no_message() {
    let (tmp, _proj) = project_dir(CERTAIN_PROJECT);
    let a = source_uri(&tmp, "A.fs");
    let mut server = Server::start();
    server.open(&a, "module A\nlet a = 1\n", "fsharp");
    assert_eq!(server.deferral_messages(&a), Vec::<String>::new());
}

/// An open `.fsproj`, on its own, produces no toast.
///
/// Deliberate, and the third narrowing this feature took under review. The
/// buffer's problems already surface as span-anchored diagnostics on its own
/// text every keystroke — better feedback than a toast, since they point at the
/// offending element instead of naming it. Meanwhile evaluating the project here
/// would populate `Workspace`'s memo from *disk* through a path text-sync does
/// not invalidate, pinning a stale Compile list for the server's lifetime.
///
/// The toast's job is the one squiggles cannot do: telling a *source* buffer why
/// its project went quiet. So the project enters scope when one of its source
/// files is open — pinned by
/// `an_open_fsproj_does_not_suppress_its_project_s_fold_refusal` below.
#[test]
fn an_fsproj_buffer_alone_produces_no_toast() {
    let (_tmp, proj) = project_dir(UNCERTAIN_PROJECT);
    let mut server = Server::start();
    server.open(&proj, UNCERTAIN_PROJECT, "xml");
    assert_eq!(server.deferral_messages(&proj), Vec::<String>::new());
}

/// A standalone `.fsx` beside an unrelated `.fsproj` must not be told that
/// project's problems. `owning_project`'s nearest-ancestor fallback would claim
/// it; `compiling_project` requires the project to *conclusively* compile the
/// script, which the SDK never globs a `.fsx` into.
#[test]
fn a_standalone_script_is_not_told_a_neighbouring_projects_problems() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("Demo.fsproj"), UNCERTAIN_PROJECT).unwrap();
    std::fs::write(tmp.path().join("A.fs"), "module A\nlet a = 1\n").unwrap();
    std::fs::write(tmp.path().join("Script.fsx"), "let x = 1\n").unwrap();
    let script = source_uri(&tmp, "Script.fsx");

    let mut server = Server::start();
    server.open(&script, "let x = 1\n", "fsharp");
    assert_eq!(
        server.deferral_messages(&script),
        Vec::<String>::new(),
        "a standalone script has no project, so nothing is declined for it"
    );
}

/// …while a `.fsx` a project *explicitly* compiles does belong to it, and hears
/// about its problems like any other source file.
#[test]
fn a_compile_listed_script_is_told_its_projects_problems() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Demo.fsproj"),
        r#"<Project>
  <ItemGroup>
    <Compile Include="Script.fsx" />
    <Compile Include="Gone.fs" />
  </ItemGroup>
</Project>"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("Script.fsx"), "let x = 1\n").unwrap();
    let script = source_uri(&tmp, "Script.fsx");

    let mut server = Server::start();
    server.open(&script, "let x = 1\n", "fsharp");
    let messages = server.deferral_messages(&script);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("Gone.fs"), "{}", messages[0]);
}

#[test]
fn the_message_fires_once_per_project_not_once_per_file() {
    let (tmp, _proj) = project_dir(UNCERTAIN_PROJECT);
    let a = source_uri(&tmp, "A.fs");
    let b = source_uri(&tmp, "B.fs");
    let mut server = Server::start();
    server.open(&a, "module A\nlet a = 1\n", "fsharp");
    assert_eq!(server.deferral_messages(&a).len(), 1);
    server.open(&b, "module B\nlet b = 2\n", "fsharp");
    assert_eq!(
        server.deferral_messages(&b),
        Vec::<String>::new(),
        "a second file in the same project must not re-toast"
    );
}

/// The dedup must not outlive the evaluation it deduped. A user who edits the
/// `.fsproj` and introduces a *different* problem has to be told about that
/// one — the previous code marked the project for the whole session, so the
/// second problem was silent forever.
#[test]
fn editing_the_project_lets_a_new_reason_be_reported() {
    let (tmp, proj) = project_dir(UNCERTAIN_PROJECT);
    let a = source_uri(&tmp, "A.fs");
    let mut server = Server::start();
    server.open(&a, "module A\nlet a = 1\n", "fsharp");
    let first = server.deferral_messages(&a);
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].contains("GetPathOfFileAbove"), "{}", first[0]);

    // Swap the cause on disk, then tell the server the file changed.
    std::fs::write(
        tmp.path().join("Demo.fsproj"),
        r#"<Project>
  <Import Project="extra.props" />
  <ItemGroup><Compile Include="A.fs" /></ItemGroup>
</Project>"#,
    )
    .unwrap();
    server.notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
        changes: vec![FileEvent {
            uri: proj.clone(),
            typ: FileChangeType::CHANGED,
        }],
    });
    let second = server.deferral_messages(&a);
    assert_eq!(second.len(), 1, "{second:?}");
    assert!(
        second[0].contains("extra.props") && second[0].contains("no such file"),
        "the new cause must be reported, got {}",
        second[0]
    );
    assert_ne!(
        first[0], second[0],
        "the session-long dedup would have re-shown the stale reason"
    );
}

/// …and the converse: fixing the project stops the message, rather than
/// re-toasting the stale reason on every watched change.
#[test]
fn fixing_the_project_stops_the_message() {
    let (tmp, proj) = project_dir(UNCERTAIN_PROJECT);
    let a = source_uri(&tmp, "A.fs");
    let mut server = Server::start();
    server.open(&a, "module A\nlet a = 1\n", "fsharp");
    assert_eq!(server.deferral_messages(&a).len(), 1);

    std::fs::write(tmp.path().join("Demo.fsproj"), CERTAIN_PROJECT).unwrap();
    server.notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
        changes: vec![FileEvent {
            uri: proj.clone(),
            typ: FileChangeType::CHANGED,
        }],
    });
    assert_eq!(server.deferral_messages(&a), Vec::<String>::new());
}

/// The fold declines for reasons the `.fsproj` evaluation cannot see. A Compile
/// item that isn't on disk is the common one: the evaluation is perfectly
/// trustworthy — it faithfully reports the item the document lists — and the
/// fold still hard-fails, because a hole in an order-sensitive fold can bind a
/// later reference to the wrong entity. Before the fold reported a typed
/// refusal, this project lost cross-file analysis in silence.
#[test]
fn a_missing_compile_item_is_reported_even_though_the_project_evaluates_cleanly() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Demo.fsproj"),
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Include="Missing.fs" />
  </ItemGroup>
</Project>"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("A.fs"), "module A\nlet a = 1\n").unwrap();
    let a = source_uri(&tmp, "A.fs");

    let mut server = Server::start();
    server.open(&a, "module A\nlet a = 1\n", "fsharp");
    let messages = server.deferral_messages(&a);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("Missing.fs"), "{}", messages[0]);
    assert!(messages[0].contains("can't be read"), "{}", messages[0]);
    assert!(
        messages[0].contains("single-file analysis"),
        "{}",
        messages[0]
    );
}

/// The same project, once the file exists, folds and says nothing — so the test
/// above is measuring the missing file rather than something ambient.
#[test]
fn the_same_project_is_quiet_once_the_compile_item_exists() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Demo.fsproj"),
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Include="Missing.fs" />
  </ItemGroup>
</Project>"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("A.fs"), "module A\nlet a = 1\n").unwrap();
    std::fs::write(tmp.path().join("Missing.fs"), "module M\nlet m = 1\n").unwrap();
    let a = source_uri(&tmp, "A.fs");

    let mut server = Server::start();
    server.open(&a, "module A\nlet a = 1\n", "fsharp");
    assert_eq!(server.deferral_messages(&a), Vec::<String>::new());
}

/// A refusal introduced *after* the file was opened must still be reported.
///
/// The report follows the first request that actually needs the fold, not the
/// keystroke: the fold is reached from request handlers, which have no
/// connection to the client, so the refusal is observed there and drained by
/// the shell afterwards. Folding on every edit just to check would be far more
/// expensive and would toast before anything had failed. Without the drain,
/// "delete a sibling source, then go to definition" fell back to single-file
/// analysis in silence.
#[test]
fn a_refusal_introduced_by_a_later_change_is_still_reported() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Demo.fsproj"),
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Include="B.fs" />
  </ItemGroup>
</Project>"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("A.fs"), "module A\nlet a = 1\n").unwrap();
    std::fs::write(tmp.path().join("B.fs"), "module B\nlet b = 2\n").unwrap();
    let a = source_uri(&tmp, "A.fs");

    let mut server = Server::start();
    server.open(&a, "module A\nlet a = 1\n", "fsharp");
    assert_eq!(
        server.deferral_messages(&a),
        Vec::<String>::new(),
        "the project is sound at open time"
    );

    // The sibling Compile item disappears, and the buffer is edited — which
    // drops the cached fold, so the next fold hits the hole.
    std::fs::remove_file(tmp.path().join("B.fs")).unwrap();
    server.change(&a, "module A\nlet a = 2\n");
    assert_eq!(
        server.deferral_messages(&a),
        Vec::<String>::new(),
        "a single-file request provokes no fold, so nothing has failed yet"
    );
    let messages = server.deferral_messages_after_fold(&a);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("B.fs"), "{}", messages[0]);
    assert!(messages[0].contains("can't be read"), "{}", messages[0]);
}

/// The same reason never re-toasts, however many times it is observed — the
/// dedup is keyed on the message, so a project that stays broken stays quiet
/// after saying so once.
#[test]
fn a_persisting_reason_is_reported_only_once() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Demo.fsproj"),
        r#"<Project><ItemGroup><Compile Include="A.fs" /><Compile Include="Gone.fs" /></ItemGroup></Project>"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("A.fs"), "module A\nlet a = 1\n").unwrap();
    let a = source_uri(&tmp, "A.fs");

    let mut server = Server::start();
    server.open(&a, "module A\nlet a = 1\n", "fsharp");
    assert_eq!(server.deferral_messages(&a).len(), 1);
    for i in 0..3 {
        server.change(&a, &format!("module A\nlet a = {i}\n"));
        assert_eq!(
            server.deferral_messages(&a),
            Vec::<String>::new(),
            "edit {i} re-toasted the same reason"
        );
    }
}

/// The fold-refusal equivalent: a project that refuses, recovers, then refuses
/// the same way again must report each time it breaks. This is what a
/// state-derived message buys — the recovery clears the record because the
/// recomputed message is "nothing", not because anything remembered to undo the
/// earlier notification.
#[test]
fn a_recovered_fold_reports_again_when_it_breaks_the_same_way() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Demo.fsproj"),
        r#"<Project><ItemGroup><Compile Include="A.fs" /><Compile Include="B.fs" /></ItemGroup></Project>"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("A.fs"), "module A\nlet a = 1\n").unwrap();
    std::fs::write(tmp.path().join("B.fs"), "module B\nlet b = 2\n").unwrap();
    let a = source_uri(&tmp, "A.fs");

    let mut server = Server::start();
    server.open(&a, "module A\nlet a = 1\n", "fsharp");
    assert_eq!(server.deferral_messages(&a), Vec::<String>::new());

    // Break it.
    std::fs::remove_file(tmp.path().join("B.fs")).unwrap();
    server.change(&a, "module A\nlet a = 2\n");
    let first = server.deferral_messages_after_fold(&a);
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].contains("B.fs"), "{}", first[0]);

    // Fix it: the message stops, and the record of it is dropped.
    std::fs::write(tmp.path().join("B.fs"), "module B\nlet b = 2\n").unwrap();
    server.change(&a, "module A\nlet a = 3\n");
    assert_eq!(
        server.deferral_messages_after_fold(&a),
        Vec::<String>::new()
    );

    // Break it the same way again: reported, not deduped against the stale one.
    std::fs::remove_file(tmp.path().join("B.fs")).unwrap();
    server.change(&a, "module A\nlet a = 4\n");
    let again = server.deferral_messages_after_fold(&a);
    assert_eq!(
        again, first,
        "the same problem, recurring after a recovery, must be reported again"
    );
}

/// A fold refusal is reported even when the project's own `.fsproj` buffer is
/// open. When the message was built from the buffer instead of from state, an
/// open `.fsproj` whose text evaluated cleanly *suppressed* the fold's refusal
/// — recreating exactly the silent fallback this whole change exists to remove,
/// and only for the user who had opened the project file to investigate.
#[test]
fn an_open_fsproj_does_not_suppress_its_project_s_fold_refusal() {
    let tmp = TempDir::new().unwrap();
    let fsproj = r#"<Project><ItemGroup><Compile Include="A.fs" /><Compile Include="Gone.fs" /></ItemGroup></Project>"#;
    std::fs::write(tmp.path().join("Demo.fsproj"), fsproj).unwrap();
    std::fs::write(tmp.path().join("A.fs"), "module A\nlet a = 1\n").unwrap();
    let proj = Url::from_file_path(tmp.path().join("Demo.fsproj")).unwrap();
    let a = source_uri(&tmp, "A.fs");

    let mut server = Server::start();
    // The project file is opened *first* — the ordering that hid the refusal.
    server.open(&proj, fsproj, "xml");
    let _ = server.deferral_messages(&proj);
    server.open(&a, "module A\nlet a = 1\n", "fsharp");
    let messages = server.deferral_messages(&a);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("Gone.fs"), "{}", messages[0]);
}
