//! The `workspace/diagnostic` sweep against the real dispatch loop: it must
//! never hold the loop hostage.
//!
//! The sweep's run time is proportional to everything under the client's
//! workspace roots, which the server does not choose; a root pinned one
//! directory too high made it run for ~15 minutes. So the loop steps the sweep
//! one unit at a time and looks at the connection between units, and these
//! tests pin what that buys: an interactive request queued behind a sweep is
//! answered before the sweep is, a `$/cancelRequest` ends it, a newer sweep or
//! a `shutdown` supersedes it, and a hang-up or `exit` abandons it.
//!
//! Every message is queued on the in-memory connection *before* the server
//! thread starts, so the server sees the whole script at once and each
//! ordering below is deterministic rather than a race against the sweep.

use std::path::Path;
use std::thread;
use std::time::Duration;

use borzoi::server::{State, run};
use borzoi::uri::UriExt as _;
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    Cancel, DidChangeWatchedFiles, Exit, Notification as NotificationTrait,
};
use lsp_types::request::{
    HoverRequest, Request as RequestTrait, Shutdown, WorkspaceDiagnosticRefresh,
    WorkspaceDiagnosticRequest,
};
use lsp_types::{
    CancelParams, ClientCapabilities, DiagnosticClientCapabilities,
    DiagnosticWorkspaceClientCapabilities, DidChangeWatchedFilesParams, FileChangeType, FileEvent,
    HoverParams, NumberOrString, PartialResultParams, Position, TextDocumentClientCapabilities,
    TextDocumentIdentifier, TextDocumentPositionParams, Uri, WorkDoneProgressParams,
    WorkspaceClientCapabilities, WorkspaceDiagnosticParams, WorkspaceDiagnosticReportResult,
    WorkspaceDocumentDiagnosticReport,
};
use tempfile::TempDir;

/// Enough projects that the sweep is many steps long, so "answered before the
/// sweep" cannot be satisfied by a sweep that happens to finish in one step.
const PROJECTS: usize = 24;

/// A root holding [`PROJECTS`] SDK-less projects, each compiling one file.
fn wide_root() -> TempDir {
    let tmp = TempDir::new().unwrap();
    for i in 0..PROJECTS {
        let dir = tmp.path().join(format!("P{i:02}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join(format!("P{i:02}.fsproj")),
            format!("<Project><ItemGroup><Compile Include=\"F{i:02}.fs\" /></ItemGroup></Project>"),
        )
        .unwrap();
        std::fs::write(dir.join(format!("F{i:02}.fs")), "let x = 1\n").unwrap();
    }
    tmp
}

fn request<R: RequestTrait>(id: &str, params: R::Params) -> Message
where
    R::Params: serde::Serialize,
{
    Message::Request(Request {
        id: RequestId::from(id.to_string()),
        method: R::METHOD.to_string(),
        params: serde_json::to_value(params).unwrap(),
    })
}

fn notification<N: NotificationTrait>(params: N::Params) -> Message
where
    N::Params: serde::Serialize,
{
    Message::Notification(Notification {
        method: N::METHOD.to_string(),
        params: serde_json::to_value(params).unwrap(),
    })
}

fn sweep(id: &str) -> Message {
    request::<WorkspaceDiagnosticRequest>(
        id,
        WorkspaceDiagnosticParams {
            identifier: None,
            previous_result_ids: Vec::new(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        },
    )
}

fn shutdown(id: &str) -> Message {
    request::<Shutdown>(id, ())
}

fn exit() -> Message {
    notification::<Exit>(())
}

fn cancel(id: &str) -> Message {
    notification::<Cancel>(CancelParams {
        id: NumberOrString::String(id.to_string()),
    })
}

/// Pull-diagnostic capabilities, with `workspace/diagnostic/refresh` support.
fn pull_refresh_caps() -> ClientCapabilities {
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            diagnostic: Some(DiagnosticClientCapabilities::default()),
            ..Default::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            diagnostic: Some(DiagnosticWorkspaceClientCapabilities {
                refresh_support: Some(true),
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// How a script ends once it has been queued.
enum End {
    /// The script itself ends the session (it carries `exit`).
    InScript,
    /// After the response to this id arrives, send `shutdown` and `exit`. For
    /// a script whose sweep must run to completion: a `shutdown` queued up
    /// front would cancel it.
    AfterReplyTo(&'static str),
    /// Drop the client's sending half, so the server sees end-of-input after
    /// the script's last message.
    HangUp,
}

/// Queue `script` on a fresh in-memory connection, then run the server loop
/// over it with `root` as the only workspace root and `docs` open, ending the
/// session as `end` says.
///
/// Returns every message the server sent, in order.
fn run_script(
    root: &Path,
    caps: ClientCapabilities,
    docs: Vec<(Uri, String)>,
    script: Vec<Message>,
    end: End,
) -> Vec<Message> {
    let (server, client) = Connection::memory();
    for msg in script {
        client.sender.send(msg).unwrap();
    }
    let Connection {
        sender: client_sender,
        receiver: client_receiver,
    } = client;
    // Kept alive unless hanging up, so end-of-input is never what ends the
    // loop on the other paths.
    let client_sender = match end {
        End::HangUp => {
            drop(client_sender);
            None
        }
        _ => Some(client_sender),
    };
    let root = root.to_path_buf();
    let handle = thread::spawn(move || {
        let mut state = State::default();
        state.set_client_capabilities(caps);
        state.set_workspace_roots(vec![root]);
        for (uri, text) in docs {
            state.docs.insert(uri, text);
        }
        run(server, state).expect("the server loop ends cleanly");
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut sent = Vec::new();
    if let End::AfterReplyTo(id) = end {
        let id = RequestId::from(id.to_string());
        loop {
            let msg = client_receiver
                .recv_deadline(deadline)
                .unwrap_or_else(|e| panic!("no reply to {id:?} within 60 s ({e}): {sent:#?}"));
            let done = matches!(&msg, Message::Response(r) if r.id == id);
            sent.push(msg);
            if done {
                break;
            }
        }
        let client_sender = client_sender.as_ref().unwrap();
        client_sender.send(shutdown("shutdown")).unwrap();
        client_sender.send(exit()).unwrap();
    }
    // Join with a deadline: a loop that never notices the end of its script
    // must fail this test, not hang the suite.
    while !handle.is_finished() {
        assert!(
            std::time::Instant::now() < deadline,
            "the server loop did not end within 60 s of its script"
        );
        thread::sleep(Duration::from_millis(10));
    }
    handle.join().expect("the server thread did not panic");
    sent.extend(client_receiver.try_iter());
    sent
}

/// The responses among `sent`, in order.
fn responses(sent: &[Message]) -> Vec<&Response> {
    sent.iter()
        .filter_map(|m| match m {
            Message::Response(r) => Some(r),
            _ => None,
        })
        .collect()
}

fn position_of(responses: &[&Response], id: &str) -> usize {
    let id = RequestId::from(id.to_string());
    let hits: Vec<usize> = responses
        .iter()
        .enumerate()
        .filter_map(|(i, r)| (r.id == id).then_some(i))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "exactly one response to {id:?}: {responses:#?}"
    );
    hits[0]
}

fn response<'a>(responses: &[&'a Response], id: &str) -> &'a Response {
    responses[position_of(responses, id)]
}

/// The error code of `id`'s response, panicking on a success.
fn error_code(responses: &[&Response], id: &str) -> i32 {
    match &response(responses, id).response_result {
        Err(e) => e.code,
        Ok(v) => panic!("expected {id:?} to fail, got the result {v}"),
    }
}

/// The number of per-file items in `id`'s successful report.
fn report_len(responses: &[&Response], id: &str) -> usize {
    let value = response(responses, id)
        .response_result
        .clone()
        .unwrap_or_else(|e| panic!("expected {id:?} to succeed, got {e:?}"));
    let WorkspaceDiagnosticReportResult::Report(report) =
        serde_json::from_value::<WorkspaceDiagnosticReportResult>(value).unwrap()
    else {
        panic!("expected a full report for {id:?}");
    };
    report
        .items
        .iter()
        .filter(|item| matches!(item, WorkspaceDocumentDiagnosticReport::Full(_)))
        .count()
}

/// Each project contributes its `.fsproj` and its one source file.
const FULL_REPORT_ITEMS: usize = 2 * PROJECTS;

#[test]
fn a_hover_queued_behind_a_sweep_is_answered_first() {
    let root = wide_root();
    // Outside the swept root, so the hover is no part of the sweep's work.
    let elsewhere = TempDir::new().unwrap();
    let uri = Uri::from_file_path(elsewhere.path().join("Open.fs")).unwrap();
    let hover = request::<HoverRequest>(
        "hover",
        HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position {
                    line: 0,
                    character: 4,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        },
    );
    let sent = run_script(
        root.path(),
        ClientCapabilities::default(),
        vec![(uri, "let x = 1\n".to_string())],
        vec![sweep("sweep"), hover],
        End::AfterReplyTo("sweep"),
    );
    let responses = responses(&sent);
    assert!(
        position_of(&responses, "hover") < position_of(&responses, "sweep"),
        "the hover must not wait for the sweep: {responses:#?}"
    );
    assert!(
        response(&responses, "hover").response_result.is_ok(),
        "{responses:#?}"
    );
    // Interleaving does not truncate the sweep: it still reports everything.
    assert_eq!(report_len(&responses, "sweep"), FULL_REPORT_ITEMS);
}

#[test]
fn cancel_request_ends_the_sweep() {
    let root = wide_root();
    let sent = run_script(
        root.path(),
        ClientCapabilities::default(),
        Vec::new(),
        vec![
            sweep("sweep"),
            cancel("sweep"),
            shutdown("shutdown"),
            exit(),
        ],
        End::InScript,
    );
    let responses = responses(&sent);
    assert_eq!(
        error_code(&responses, "sweep"),
        ErrorCode::RequestCanceled as i32
    );
    assert!(position_of(&responses, "sweep") < position_of(&responses, "shutdown"));
}

/// A `$/cancelRequest` naming some other id leaves the sweep running.
#[test]
fn cancelling_another_request_leaves_the_sweep_alone() {
    let root = wide_root();
    let sent = run_script(
        root.path(),
        ClientCapabilities::default(),
        Vec::new(),
        vec![sweep("sweep"), cancel("other")],
        End::AfterReplyTo("sweep"),
    );
    let responses = responses(&sent);
    assert_eq!(report_len(&responses, "sweep"), FULL_REPORT_ITEMS);
}

#[test]
fn a_newer_sweep_supersedes_the_one_in_flight() {
    let root = wide_root();
    let sent = run_script(
        root.path(),
        ClientCapabilities::default(),
        Vec::new(),
        vec![sweep("old"), sweep("new")],
        End::AfterReplyTo("new"),
    );
    let responses = responses(&sent);
    assert_eq!(
        error_code(&responses, "old"),
        ErrorCode::ServerCancelled as i32
    );
    // The client asked again itself, so it is not told to retrigger.
    let data = response(&responses, "old")
        .response_result
        .as_ref()
        .unwrap_err()
        .data
        .clone();
    assert_eq!(data, Some(serde_json::json!({ "retriggerRequest": false })));
    assert_eq!(report_len(&responses, "new"), FULL_REPORT_ITEMS);
}

/// `shutdown` answers the sweep (as server-cancelled) before acknowledging, so
/// the client is never left with a request the server will not answer.
#[test]
fn shutdown_cancels_the_sweep_in_flight() {
    let root = wide_root();
    let sent = run_script(
        root.path(),
        ClientCapabilities::default(),
        Vec::new(),
        vec![sweep("sweep"), shutdown("shutdown"), exit()],
        End::InScript,
    );
    let responses = responses(&sent);
    assert_eq!(
        error_code(&responses, "sweep"),
        ErrorCode::ServerCancelled as i32
    );
    assert!(position_of(&responses, "sweep") < position_of(&responses, "shutdown"));
}

/// The peer hanging up mid-sweep ends the loop without finishing the sweep:
/// nobody is left to read the report. (An orphaned server whose editor has
/// exited is the case this pins.)
#[test]
fn hang_up_abandons_the_sweep() {
    let root = wide_root();
    let sent = run_script(
        root.path(),
        ClientCapabilities::default(),
        Vec::new(),
        vec![sweep("sweep")],
        End::HangUp,
    );
    assert!(
        responses(&sent).is_empty(),
        "the sweep must be abandoned, not completed: {sent:#?}"
    );
}

#[test]
fn exit_abandons_the_sweep() {
    let root = wide_root();
    let sent = run_script(
        root.path(),
        ClientCapabilities::default(),
        Vec::new(),
        vec![sweep("sweep"), exit()],
        End::InScript,
    );
    assert!(
        responses(&sent).is_empty(),
        "the sweep must be abandoned, not completed: {sent:#?}"
    );
}

/// A diagnostic input that changes while the sweep is in flight may have
/// staled items the sweep already computed, so the refresh it owes is sent
/// *after* the sweep's reply: a refresh that overtook the reply would tell the
/// client to re-pull before it held the stale report, and could be lost.
#[test]
fn a_refresh_owed_mid_sweep_follows_the_reply() {
    let root = wide_root();
    let changed = Uri::from_file_path(root.path().join("P00").join("F00.fs")).unwrap();
    let watched = notification::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
        changes: vec![FileEvent {
            uri: changed,
            typ: FileChangeType::CHANGED,
        }],
    });
    let sent = run_script(
        root.path(),
        pull_refresh_caps(),
        Vec::new(),
        vec![sweep("sweep"), watched],
        End::AfterReplyTo("sweep"),
    );
    let reply = sent
        .iter()
        .position(
            |m| matches!(m, Message::Response(r) if r.id == RequestId::from("sweep".to_string())),
        )
        .unwrap_or_else(|| panic!("no reply to the sweep: {sent:#?}"));
    let refresh = sent
        .iter()
        .position(
            |m| matches!(m, Message::Request(r) if r.method == WorkspaceDiagnosticRefresh::METHOD),
        )
        .unwrap_or_else(|| panic!("the watched change owed a refresh: {sent:#?}"));
    assert!(
        reply < refresh,
        "the refresh must follow the sweep's reply: {sent:#?}"
    );
}
