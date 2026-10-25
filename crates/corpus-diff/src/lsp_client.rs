//! A client for the real LSP server, driven over the protocol.
//!
//! The server's dispatch loop ([`borzoi::server::run_with_fetcher`]) runs on its
//! own thread behind an in-memory [`Connection`], exactly as `main` runs it
//! behind stdio. Everything this client sends is JSON built by hand — positions
//! are bare numbers, documents are URIs — and everything it reads back is JSON,
//! so a request goes through the server's own parameter decoding, dispatch,
//! position conversion and response encoding. Nothing of the handlers'
//! internals is called.
//!
//! No SourceLink fetcher is configured, so a go-to-definition into a referenced
//! assembly's remote source answers with the source URL rather than touching
//! the network.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use borzoi::server::{State, client_capabilities_from_initialize, run_with_fetcher};
use borzoi::workspace::Workspace;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::Url;
use serde_json::{Value, json};

/// How long one request may take before the session is declared wedged. A
/// handler answers from caches the first request of a project warms, so this is
/// sized for that first, cold request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// A running server and the client end of its connection.
pub struct LspClient {
    client: Connection,
    thread: Option<thread::JoinHandle<()>>,
    next_id: i32,
}

/// What a request came back with: its `result`, or the error the server sent.
pub type Answer = Result<Value, String>;

impl LspClient {
    /// Start a server whose workspace is `workspace` (built by the caller's
    /// closure on the server thread, since server state is not `Send`), and
    /// announce a client that pulls its diagnostics, so opening a document
    /// computes none.
    pub fn start(workspace: impl FnOnce() -> Workspace + Send + 'static) -> Self {
        let (server, client) = Connection::memory();
        let initialize = json!({
            "processId": null,
            "rootUri": null,
            "capabilities": {
                "textDocument": {
                    "diagnostic": {},
                    "hover": { "contentFormat": ["markdown"] },
                },
            },
        });
        let capabilities = client_capabilities_from_initialize(&initialize)
            .expect("the initialize object is well-formed");
        let thread = thread::spawn(move || {
            let mut state = State::new();
            state.workspace = workspace();
            state.set_client_capabilities(capabilities);
            run_with_fetcher(server, state, None).expect("the server loop ends cleanly");
        });
        Self {
            client,
            thread: Some(thread),
            next_id: 0,
        }
    }

    /// `textDocument/didOpen` for `path` with `text`.
    pub fn open(&self, path: &Path, text: &str) {
        let uri = file_uri(path);
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "fsharp",
                    "version": 1,
                    "text": text,
                },
            }),
        );
    }

    /// Send `method` with `params` and wait for its response, skipping the
    /// notifications and server-to-client requests the server sends meanwhile.
    pub fn request(&mut self, method: &str, params: Value) -> Answer {
        let id = RequestId::from(self.next_id);
        self.next_id += 1;
        self.client
            .sender
            .send(Message::Request(Request {
                id: id.clone(),
                method: method.to_string(),
                params,
            }))
            .expect("the server is listening");
        loop {
            let message = self
                .client
                .receiver
                .recv_timeout(REQUEST_TIMEOUT)
                .unwrap_or_else(|e| {
                    panic!("no answer to {method} within {REQUEST_TIMEOUT:?}: {e}")
                });
            match message {
                Message::Response(Response {
                    id: rid,
                    result,
                    error,
                }) if rid == id => {
                    return match error {
                        Some(error) => Err(format!("{}: {}", error.code, error.message)),
                        None => Ok(result.unwrap_or(Value::Null)),
                    };
                }
                Message::Response(_) | Message::Notification(_) | Message::Request(_) => {}
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        self.client
            .sender
            .send(Message::Notification(Notification {
                method: method.to_string(),
                params,
            }))
            .expect("the server is listening");
    }
}

/// How long a server gets to stop once told to.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

impl Drop for LspClient {
    /// `shutdown` then `exit`, and the server thread joined — if it stops in
    /// time. Bounded, and never a panic, because this also runs while unwinding
    /// from a request that timed out: a server wedged in a handler cannot read
    /// either message, and waiting on it would turn that timeout back into a
    /// hang.
    fn drop(&mut self) {
        let id = RequestId::from(self.next_id);
        let _ = self.client.sender.send(Message::Request(Request {
            id,
            method: "shutdown".to_string(),
            params: Value::Null,
        }));
        let _ = self.client.sender.send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: Value::Null,
        }));
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        while !thread.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if !thread.is_finished() {
            eprintln!(
                "the LSP server did not stop within {SHUTDOWN_TIMEOUT:?}; leaving it running"
            );
        } else if let Err(err) = thread.join() {
            eprintln!("the LSP server thread panicked: {err:?}");
        }
    }
}

/// The `file://` URI a client names `path` by.
pub fn file_uri(path: &Path) -> String {
    Url::from_file_path(path)
        .unwrap_or_else(|()| panic!("{} has no file URI", path.display()))
        .to_string()
}
