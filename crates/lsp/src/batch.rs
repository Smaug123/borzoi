//! Coalescing in the dispatch loop (`docs/incremental-lsp-plan.md`, D6 and
//! Stage 8): what to do with the messages the loop has read and not yet
//! handled, decided as a pure function of those messages.
//!
//! The loop ([`crate::server::run_inbox`]) reads every message waiting in its
//! inbox ([`crate::inbox`]) before it handles one, and again before each
//! message it handles after that. The messages it holds are one **epoch**: it
//! starts with the first message read while nothing was pending and ends when
//! every message read since has been handled. [`plan`] orders the epoch's messages
//! and decides each one's fate; the loop executes the first step whose message
//! it has not yet handled, reads what has arrived since, plans again, and so
//! on.
//!
//! The plan, for the messages before the epoch's first `shutdown` request or
//! `exit` notification (its **live** part):
//!
//! 1. Every notification and every response to a server request, in arrival
//!    order. A text sync of a document (`didOpen`, `didChange`, `didClose`)
//!    that a later live text sync of the same URI supersedes is applied but
//!    publishes no diagnostics: the later one publishes for the text that
//!    stands.
//! 2. Then every request, in arrival order, except that a request whose run
//!    stopped at a checkpoint ([`Restart`]) waits behind every request the
//!    epoch held when it stopped. One that a later live `$/cancelRequest`
//!    names is answered `RequestCancelled` without running. Otherwise one
//!    that names a document (`params.textDocument.uri`) which a later live
//!    text sync changes is answered `ContentModified` without running: its
//!    positions count a text the client no longer holds. Every other request
//!    runs. "Later" is by arrival, whether or not the request was restarted.
//!
//! A request therefore runs only after every notification the loop has read,
//! wherever that notification sat in the epoch, so the texts it reads (its own
//! document's and every other document's its answer depends on) are the newest
//! the client has sent. Its own document's text is still the one its positions
//! count, because a later change to it would have refused it.
//!
//! From the barrier on (and for the whole epoch once `shutdown` was
//! acknowledged in an earlier one) the lifecycle rules apply as they did to
//! one message at a time: a `shutdown` is acknowledged, every other request is
//! refused `InvalidRequest`, every notification but `exit` is dropped, a
//! response is handled, and `exit` ends the session; nothing after `exit` is
//! read.
//!
//! **A request that is running** stops at its next checkpoint when a
//! notification arrives ([`crate::inbox`], Stage 9), and is unhandled again:
//! the loop reads what arrived, plans again with the request as a
//! [`Restart`], and refuses it or runs it again. The requests that arrived
//! while it ran go first: they are the client's newer interest, and usually
//! cheaper than the run they interrupted. A `workspace/diagnostic` sweep in
//! flight is not stopped; its interleaving with later messages is described
//! in [`crate::handlers::workspace_diagnostic`].

use std::collections::HashMap;

use lsp_server::{Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    Cancel, DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Exit,
    Notification as NotificationTrait,
};
use lsp_types::request::{Request as RequestTrait, Shutdown};
use lsp_types::{CancelParams, NumberOrString, Uri};

use crate::vfs::DocumentKey;

/// A message the loop has read, parsed as far as [`plan`] and the handler
/// that applies it need.
#[derive(Debug)]
pub enum Incoming {
    /// A text sync, with its parameters parsed.
    Sync(TextSync),
    /// `$/cancelRequest`, naming the request it cancels; `None` when its
    /// parameters do not parse.
    Cancel(Option<RequestId>),
    /// The `exit` notification.
    Exit,
    /// Any other notification (watched-file events, `didSave`, a method the
    /// server ignores).
    Notification(Notification),
    /// A text-sync notification whose parameters do not parse, or a
    /// `didChange` with no content change: applying it does nothing.
    Ignored,
    /// A request other than `shutdown`, with the document its parameters
    /// name, if they name one.
    Request {
        request: Request,
        document: Option<DocumentKey>,
    },
    /// The `shutdown` request.
    Shutdown(RequestId),
    /// The client's response to a request the server sent.
    Response(Response),
}

/// A notification that changes one document's text.
#[derive(Debug)]
pub enum TextSync {
    /// `textDocument/didOpen`: the client's text for `uri` is now `text`.
    Open { uri: Uri, text: String },
    /// `textDocument/didChange` (full sync): the client's text for `uri` is
    /// now `text`.
    Change { uri: Uri, text: String },
    /// `textDocument/didClose`: `uri` reads the disk again.
    Close { uri: Uri },
}

impl TextSync {
    /// The URI whose text this changes.
    pub fn uri(&self) -> &Uri {
        match self {
            TextSync::Open { uri, .. } | TextSync::Change { uri, .. } | TextSync::Close { uri } => {
                uri
            }
        }
    }
}

impl Incoming {
    /// Parse `message`. A text sync's parameters are parsed here, once, and
    /// the handler applies the parsed value, so planning can never disagree
    /// with the handler about whether, and to which document, a sync applies.
    pub fn read(message: Message) -> Incoming {
        match message {
            Message::Request(request) if request.method == Shutdown::METHOD => {
                Incoming::Shutdown(request.id)
            }
            Message::Request(request) => {
                let document = named_document(&request);
                Incoming::Request { request, document }
            }
            Message::Response(response) => Incoming::Response(response),
            Message::Notification(notification) => read_notification(notification),
        }
    }
}

/// What [`plan`] reads of a message: its kind, and the URI, document or
/// request id that decide its fate. Kept after the message itself is handled,
/// since a handled text sync still refuses an earlier request it superseded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    /// A text sync of `uri`.
    Sync {
        uri: Uri,
        document: DocumentKey,
    },
    /// `$/cancelRequest` naming `id` (`None` when its parameters do not
    /// parse).
    Cancel(Option<RequestId>),
    Exit,
    /// Any other notification.
    Notification,
    /// A notification that applies nothing ([`Incoming::Ignored`]).
    Ignored,
    /// A request other than `shutdown`.
    Request {
        id: RequestId,
        document: Option<DocumentKey>,
    },
    Shutdown,
    Response,
}

impl Incoming {
    /// What [`plan`] reads of this message.
    pub fn shape(&self) -> Shape {
        match self {
            Incoming::Sync(sync) => Shape::Sync {
                uri: sync.uri().clone(),
                document: DocumentKey::of(sync.uri()),
            },
            Incoming::Cancel(id) => Shape::Cancel(id.clone()),
            Incoming::Exit => Shape::Exit,
            Incoming::Notification(_) => Shape::Notification,
            Incoming::Ignored => Shape::Ignored,
            Incoming::Request { request, document } => Shape::Request {
                id: request.id.clone(),
                document: document.clone(),
            },
            Incoming::Shutdown(_) => Shape::Shutdown,
            Incoming::Response(_) => Shape::Response,
        }
    }
}

fn read_notification(notification: Notification) -> Incoming {
    match notification.method.as_str() {
        Exit::METHOD => Incoming::Exit,
        Cancel::METHOD => Incoming::Cancel(
            serde_json::from_value::<CancelParams>(notification.params)
                .ok()
                .map(|params| match params.id {
                    NumberOrString::Number(n) => RequestId::from(n),
                    NumberOrString::String(s) => RequestId::from(s),
                }),
        ),
        DidOpenTextDocument::METHOD => {
            match notification.extract::<<DidOpenTextDocument as NotificationTrait>::Params>(
                DidOpenTextDocument::METHOD,
            ) {
                Ok(params) => Incoming::Sync(TextSync::Open {
                    uri: params.text_document.uri,
                    text: params.text_document.text,
                }),
                Err(_) => Incoming::Ignored,
            }
        }
        DidChangeTextDocument::METHOD => {
            match notification.extract::<<DidChangeTextDocument as NotificationTrait>::Params>(
                DidChangeTextDocument::METHOD,
            ) {
                // FULL sync: the last change holds the whole document.
                Ok(params) => match params.content_changes.into_iter().next_back() {
                    Some(change) => Incoming::Sync(TextSync::Change {
                        uri: params.text_document.uri,
                        text: change.text,
                    }),
                    None => Incoming::Ignored,
                },
                Err(_) => Incoming::Ignored,
            }
        }
        DidCloseTextDocument::METHOD => {
            match notification.extract::<<DidCloseTextDocument as NotificationTrait>::Params>(
                DidCloseTextDocument::METHOD,
            ) {
                Ok(params) => Incoming::Sync(TextSync::Close {
                    uri: params.text_document.uri,
                }),
                Err(_) => Incoming::Ignored,
            }
        }
        _ => Incoming::Notification(notification),
    }
}

/// The document a request's parameters name (`params.textDocument.uri`), as
/// every document-scoped request in the protocol spells it. Read from the
/// parameters whatever the method, so a request the server does not know
/// yet, or one whose other parameters are malformed, is still refused when
/// its document changes under it.
fn named_document(request: &Request) -> Option<DocumentKey> {
    let uri = request.params.get("textDocument")?.get("uri")?;
    let uri: Uri = serde_json::from_value(uri.clone()).ok()?;
    Some(DocumentKey::of(&uri))
}

/// Why a request is answered without running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A later `$/cancelRequest` in the epoch names it.
    Cancelled,
    /// A later text sync in the epoch changes the document it names.
    ContentModified,
    /// It arrived after `shutdown`.
    AfterShutdown,
}

/// A request whose run stopped at a checkpoint ([`crate::inbox::Superseded`]):
/// the message at `index`, and how many messages the epoch held when it
/// stopped. It runs again after every request among those `held`, and before
/// any read after them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Restart {
    pub index: usize,
    pub held: usize,
}

/// One thing the loop does, naming the epoch message it does it to by index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Apply the text sync at `index`, publishing its diagnostics to a push
    /// client only when `diagnose`.
    Sync { index: usize, diagnose: bool },
    /// Handle the notification at `index` (a cancellation, a watched-file
    /// event, any other).
    Notify { index: usize },
    /// Handle the client's response at `index`.
    Reply { index: usize },
    /// Run the request at `index` and answer it.
    Run { index: usize },
    /// Answer the request at `index` without running it.
    Refuse { index: usize, refusal: Refusal },
    /// Acknowledge the `shutdown` request at `index`.
    Shutdown { index: usize },
    /// End the session (the `exit` at `index`).
    Exit { index: usize },
}

impl Step {
    /// The epoch message this step handles.
    pub fn index(self) -> usize {
        match self {
            Step::Sync { index, .. }
            | Step::Notify { index }
            | Step::Reply { index }
            | Step::Run { index }
            | Step::Refuse { index, .. }
            | Step::Shutdown { index }
            | Step::Exit { index } => index,
        }
    }
}

/// The plan for an epoch's messages, given as their [`Shape`]s in arrival
/// order, in the order to execute it (see the module docs). `restarts` are
/// the requests whose runs stopped, the last stop of each counting.
/// `shutting_down` is whether `shutdown` was acknowledged before the epoch
/// began. Every request before the first `exit` gets exactly one step; an
/// [`Shape::Ignored`] message, and a notification after `shutdown`, get none.
pub fn plan(messages: &[Shape], restarts: &[Restart], shutting_down: bool) -> Vec<Step> {
    let barrier = if shutting_down {
        0
    } else {
        messages
            .iter()
            .position(|m| matches!(m, Shape::Shutdown | Shape::Exit))
            .unwrap_or(messages.len())
    };
    let live = &messages[..barrier];

    // The last live text sync of each URI and of each document, and the last
    // live cancellation of each request id: a message is superseded exactly
    // when one of these lies after it.
    let mut last_sync_of_uri: HashMap<&Uri, usize> = HashMap::new();
    let mut last_sync_of_document: HashMap<&DocumentKey, usize> = HashMap::new();
    let mut last_cancel_of: HashMap<&RequestId, usize> = HashMap::new();
    for (index, message) in live.iter().enumerate() {
        match message {
            Shape::Sync { uri, document } => {
                last_sync_of_uri.insert(uri, index);
                last_sync_of_document.insert(document, index);
            }
            Shape::Cancel(Some(id)) => {
                last_cancel_of.insert(id, index);
            }
            _ => {}
        }
    }

    let mut steps = Vec::with_capacity(messages.len());
    for (index, message) in live.iter().enumerate() {
        match message {
            Shape::Sync { uri, .. } => steps.push(Step::Sync {
                index,
                diagnose: last_sync_of_uri[uri] == index,
            }),
            Shape::Cancel(_) | Shape::Notification => steps.push(Step::Notify { index }),
            Shape::Response => steps.push(Step::Reply { index }),
            Shape::Ignored | Shape::Request { .. } => {}
            Shape::Exit | Shape::Shutdown => {
                unreachable!("the live part ends before the first `exit` or `shutdown`")
            }
        }
    }
    // A request runs in arrival order, or, once restarted, between the
    // requests read before it stopped and those read after: twice its index
    // plus one, against twice the number held when it stopped.
    let mut held_at_restart: HashMap<usize, usize> = HashMap::new();
    for restart in restarts {
        held_at_restart.insert(restart.index, restart.held);
    }
    let mut requests: Vec<usize> = live
        .iter()
        .enumerate()
        .filter(|(_, message)| matches!(message, Shape::Request { .. }))
        .map(|(index, _)| index)
        .collect();
    requests.sort_by_key(|&index| {
        let turn = held_at_restart
            .get(&index)
            .map_or(2 * index + 1, |&held| 2 * held);
        (turn, index)
    });
    for index in requests {
        if let Shape::Request { id, document } = &live[index] {
            let after = |last: Option<&usize>| last.is_some_and(|&last| last > index);
            let refusal = if after(last_cancel_of.get(id)) {
                Some(Refusal::Cancelled)
            } else if document
                .as_ref()
                .is_some_and(|document| after(last_sync_of_document.get(document)))
            {
                Some(Refusal::ContentModified)
            } else {
                None
            };
            steps.push(match refusal {
                Some(refusal) => Step::Refuse { index, refusal },
                None => Step::Run { index },
            });
        }
    }

    for (index, message) in messages.iter().enumerate().skip(barrier) {
        match message {
            Shape::Exit => {
                steps.push(Step::Exit { index });
                break;
            }
            Shape::Shutdown => steps.push(Step::Shutdown { index }),
            Shape::Request { .. } => steps.push(Step::Refuse {
                index,
                refusal: Refusal::AfterShutdown,
            }),
            Shape::Response => steps.push(Step::Reply { index }),
            Shape::Sync { .. } | Shape::Cancel(_) | Shape::Notification | Shape::Ignored => {}
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uri::UriExt as _;
    use proptest::prelude::*;
    use serde_json::json;

    /// What a generated message is, with small alphabets of documents and
    /// ids so that supersessions and cancellations collide often.
    #[derive(Clone, Debug)]
    enum Gen {
        Open(u8),
        Change(u8),
        Close(u8),
        Watched,
        Ignored,
        Cancel(Option<u8>),
        Request { id: u8, document: Option<u8> },
        Shutdown(u8),
        Response,
        Exit,
    }

    /// Documents 0 and 1 are one file under two URIs (by case on a
    /// case-insensitive platform, by an escaped letter elsewhere); 2 is
    /// another file; 3 is not a `file:` URI.
    fn uri(document: u8) -> Uri {
        let spelling = match document {
            0 => "file:///p/A.fs",
            1 if cfg!(any(windows, target_os = "macos")) => "file:///p/a.fs",
            1 => "file:///p/%41.fs",
            2 => "file:///p/B.fs",
            _ => "untitled:Untitled-1",
        };
        Uri::parse(spelling).unwrap()
    }

    fn message(shape: &Gen) -> Message {
        let notification = |method: &str, params| {
            Message::Notification(Notification {
                method: method.to_string(),
                params,
            })
        };
        match *shape {
            Gen::Open(d) => notification(
                "textDocument/didOpen",
                json!({ "textDocument": {
                    "uri": uri(d).as_str(), "languageId": "fsharp", "version": 0, "text": "x",
                }}),
            ),
            Gen::Change(d) => notification(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri(d).as_str(), "version": 1 },
                    "contentChanges": [{ "text": "y" }],
                }),
            ),
            Gen::Close(d) => notification(
                "textDocument/didClose",
                json!({ "textDocument": { "uri": uri(d).as_str() } }),
            ),
            Gen::Watched => notification(
                "workspace/didChangeWatchedFiles",
                json!({ "changes": [{ "uri": uri(0).as_str(), "type": 2 }] }),
            ),
            // A `didChange` with no content change.
            Gen::Ignored => notification(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri(0).as_str(), "version": 1 },
                    "contentChanges": [],
                }),
            ),
            Gen::Cancel(Some(id)) => {
                notification("$/cancelRequest", json!({ "id": format!("r{id}") }))
            }
            Gen::Cancel(None) => notification("$/cancelRequest", json!({ "id": null })),
            Gen::Request { id, document } => Message::Request(Request {
                id: RequestId::from(format!("r{id}")),
                method: "textDocument/hover".to_string(),
                params: match document {
                    Some(d) => json!({
                        "textDocument": { "uri": uri(d).as_str() },
                        "position": { "line": 0, "character": 0 },
                    }),
                    None => json!({ "query": "" }),
                },
            }),
            Gen::Shutdown(id) => Message::Request(Request {
                id: RequestId::from(format!("r{id}")),
                method: "shutdown".to_string(),
                params: json!(null),
            }),
            Gen::Response => Message::Response(Response {
                id: RequestId::from("s".to_string()),
                response_result: Ok(json!(null)),
            }),
            Gen::Exit => notification("exit", json!(null)),
        }
    }

    fn generated() -> impl Strategy<Value = Gen> {
        let document = 0u8..4;
        let id = 0u8..6;
        prop_oneof![
            4 => document.clone().prop_map(Gen::Open),
            6 => document.clone().prop_map(Gen::Change),
            2 => document.clone().prop_map(Gen::Close),
            1 => Just(Gen::Watched),
            1 => Just(Gen::Ignored),
            3 => proptest::option::weighted(0.9, id.clone()).prop_map(Gen::Cancel),
            8 => (id.clone(), proptest::option::weighted(0.8, document))
                .prop_map(|(id, document)| Gen::Request { id, document }),
            1 => id.prop_map(Gen::Shutdown),
            1 => Just(Gen::Response),
            1 => Just(Gen::Exit),
        ]
    }

    /// The plan's specification, stated message by message with no index
    /// structures: each message's fate, by scanning the rest of the epoch.
    /// A restarted request (its index and the messages held when it stopped,
    /// the last stop counting) takes its turn just before the first message
    /// read after it stopped.
    fn reference(
        shapes: &[Gen],
        restarts: &[(usize, usize)],
        shutting_down: bool,
    ) -> Vec<(usize, &'static str)> {
        let same_document = |a: u8, b: u8| DocumentKey::of(&uri(a)) == DocumentKey::of(&uri(b));
        let syncs = |shape: &Gen| match *shape {
            Gen::Open(d) | Gen::Change(d) | Gen::Close(d) => Some(d),
            _ => None,
        };
        let barrier = if shutting_down {
            0
        } else {
            shapes
                .iter()
                .position(|s| matches!(s, Gen::Shutdown(_) | Gen::Exit))
                .unwrap_or(shapes.len())
        };
        let later = |i: usize| &shapes[i + 1..barrier];
        let mut out = Vec::new();
        for (i, shape) in shapes[..barrier].iter().enumerate() {
            match shape {
                Gen::Open(d) | Gen::Change(d) | Gen::Close(d) => {
                    let superseded = later(i).iter().filter_map(syncs).any(|e| uri(e) == uri(*d));
                    out.push((i, if superseded { "sync quietly" } else { "sync" }));
                }
                Gen::Watched | Gen::Cancel(_) => out.push((i, "notify")),
                Gen::Response => out.push((i, "reply")),
                _ => {}
            }
        }
        let turn = |i: usize| {
            let held = restarts
                .iter()
                .rev()
                .find(|(index, _)| *index == i)
                .map(|(_, held)| *held);
            match held {
                Some(held) => held as f64 - 0.5,
                None => i as f64,
            }
        };
        let mut requests: Vec<usize> = (0..barrier)
            .filter(|&i| matches!(shapes[i], Gen::Request { .. }))
            .collect();
        requests.sort_by(|&a, &b| turn(a).total_cmp(&turn(b)));
        for i in requests {
            if let Gen::Request { id, document } = shapes[i] {
                let cancelled = later(i)
                    .iter()
                    .any(|s| matches!(*s, Gen::Cancel(Some(c)) if c == id));
                let modified = document.is_some_and(|d| {
                    later(i)
                        .iter()
                        .filter_map(syncs)
                        .any(|e| same_document(e, d))
                });
                out.push((
                    i,
                    if cancelled {
                        "cancelled"
                    } else if modified {
                        "content modified"
                    } else {
                        "run"
                    },
                ));
            }
        }
        for (i, shape) in shapes.iter().enumerate().skip(barrier) {
            match shape {
                Gen::Exit => {
                    out.push((i, "exit"));
                    break;
                }
                Gen::Shutdown(_) => out.push((i, "shutdown")),
                Gen::Request { .. } => out.push((i, "after shutdown")),
                Gen::Response => out.push((i, "reply")),
                _ => {}
            }
        }
        out
    }

    fn describe(step: Step) -> (usize, &'static str) {
        let what = match step {
            Step::Sync { diagnose: true, .. } => "sync",
            Step::Sync {
                diagnose: false, ..
            } => "sync quietly",
            Step::Notify { .. } => "notify",
            Step::Reply { .. } => "reply",
            Step::Run { .. } => "run",
            Step::Refuse {
                refusal: Refusal::Cancelled,
                ..
            } => "cancelled",
            Step::Refuse {
                refusal: Refusal::ContentModified,
                ..
            } => "content modified",
            Step::Refuse {
                refusal: Refusal::AfterShutdown,
                ..
            } => "after shutdown",
            Step::Shutdown { .. } => "shutdown",
            Step::Exit { .. } => "exit",
        };
        (step.index(), what)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 2_000, ..ProptestConfig::default() })]

        /// [`plan`] equals its specification, stated as a scan per message:
        /// the same steps, in the same order, with the same fates.
        #[test]
        fn the_plan_is_its_specification(
            shapes in proptest::collection::vec(generated(), 0..24),
            stops in proptest::collection::vec((0usize..24, 0usize..24), 0..4),
            shutting_down in proptest::bool::weighted(0.1),
        ) {
            let messages: Vec<Shape> =
                shapes.iter().map(|s| Incoming::read(message(s)).shape()).collect();
            // A stop names a request and a later point of the epoch; stops
            // that name no request are dropped, as the loop never makes one.
            let restarts: Vec<(usize, usize)> = stops
                .into_iter()
                .filter(|(index, _)| {
                    matches!(shapes.get(*index), Some(Gen::Request { .. }))
                })
                .map(|(index, held)| (index, (index + 1 + held).min(shapes.len())))
                .collect();
            let as_restarts: Vec<Restart> = restarts
                .iter()
                .map(|&(index, held)| Restart { index, held })
                .collect();
            let steps: Vec<_> = plan(&messages, &as_restarts, shutting_down)
                .into_iter()
                .map(describe)
                .collect();
            prop_assert_eq!(steps, reference(&shapes, &restarts, shutting_down));
        }
    }

    /// A request's own document is read from its parameters whatever the
    /// method, and only from `textDocument.uri`.
    #[test]
    fn a_request_names_the_document_of_its_text_document_parameter() {
        let request = |method: &str, params| Request {
            id: RequestId::from(1),
            method: method.to_string(),
            params,
        };
        let a = DocumentKey::of(&uri(0));
        for method in [
            "textDocument/hover",
            "textDocument/semanticTokens/full",
            "textDocument/diagnostic",
            "borzoi/unknown",
        ] {
            assert_eq!(
                named_document(&request(
                    method,
                    json!({ "textDocument": { "uri": uri(0).as_str() } })
                )),
                Some(a.clone()),
                "{method}"
            );
        }
        assert_eq!(
            named_document(&request("workspace/symbol", json!({ "query": "x" }))),
            None
        );
        assert_eq!(
            named_document(&request(
                "textDocument/hover",
                json!({ "textDocument": { "uri": 3 } })
            )),
            None
        );
    }

    /// A restarted request goes behind the requests the epoch held when it
    /// stopped, and ahead of those read after; its fate is still decided by
    /// what arrived after it, so a change to its document refuses it.
    #[test]
    fn a_restarted_request_waits_behind_the_requests_read_while_it_ran() {
        let shapes = [
            Gen::Request {
                id: 0,
                document: Some(2),
            },
            Gen::Change(0),
            Gen::Request {
                id: 1,
                document: Some(0),
            },
            Gen::Request {
                id: 2,
                document: Some(2),
            },
        ];
        let messages: Vec<Shape> = shapes
            .iter()
            .map(|s| Incoming::read(message(s)).shape())
            .collect();
        assert_eq!(
            plan(&messages, &[Restart { index: 0, held: 3 }], false),
            vec![
                Step::Sync {
                    index: 1,
                    diagnose: true
                },
                Step::Run { index: 2 },
                Step::Run { index: 0 },
                Step::Run { index: 3 },
            ]
        );
    }

    /// A malformed or empty sync is ignored, as its handler ignores it, so
    /// it supersedes nothing.
    #[test]
    fn a_sync_that_applies_nothing_supersedes_nothing() {
        let shapes = [
            Gen::Change(0),
            Gen::Request {
                id: 0,
                document: Some(0),
            },
            Gen::Ignored,
        ];
        let messages: Vec<Shape> = shapes
            .iter()
            .map(|s| Incoming::read(message(s)).shape())
            .collect();
        assert_eq!(
            plan(&messages, &[], false),
            vec![
                Step::Sync {
                    index: 0,
                    diagnose: true
                },
                Step::Run { index: 1 },
            ]
        );
    }
}
