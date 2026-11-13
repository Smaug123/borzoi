//! The server's source reads, counted per generation by `borzoi::observed`
//! (`docs/incremental-lsp-plan.md`, Stage 2a).
//!
//! A real dispatch loop runs on its own thread over an in-memory connection,
//! sending every generation it ends to a channel the test reads
//! ([`Sources::log_generations`](borzoi::observed::Sources::log_generations)).
//! The client opens two Compile items of a generated
//! `N`-item project, warms the fold, then types into a trailing comment of
//! one of them and asks for its semantic tokens. The fold re-reads every
//! Compile item, and the two that are open come from their buffers, so the
//! keystroke and the request open exactly `N − 2` files between them. This
//! pins today's behaviour: Stage 2b's `Vfs` takes the count to 0.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use borzoi::observed::Generation;
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::server::{SERVER_STACK_BYTES, State, run_with_fetcher};
use borzoi::uri::UriExt as _;
use borzoi::workspace::Workspace;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{ClientCapabilities, Uri};
use proptest::prelude::*;
use serde_json::{Value, json};
use tempfile::TempDir;

const RECV_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_SETTLE_ROUNDS: usize = 8;

struct Client {
    conn: Connection,
    server: Option<thread::JoinHandle<()>>,
    next_id: u64,
    /// The generations the server has ended, as it sends them.
    log: Receiver<Generation>,
    /// The generations read from `log` so far.
    generations: Vec<Generation>,
}

impl Client {
    fn start(root: &Path, pull: bool) -> Client {
        let (server_side, conn) = Connection::memory();
        let (log_sender, log) = mpsc::channel();
        let root = root.to_path_buf();
        let server = thread::Builder::new()
            .stack_size(SERVER_STACK_BYTES)
            .spawn(move || {
                let mut state = State::new();
                state.docs.log_generations(log_sender);
                // Hermetic: no `dotnet` is found, so no env is built from the
                // host's packs. The project is SDK-less, so it evaluates and
                // folds all the same.
                state.workspace = Workspace::with_env(SdkDiscoveryEnv {
                    dotnet_root: Some(root.join("no-dotnet")),
                    ..SdkDiscoveryEnv::default()
                });
                let mut caps = json!({
                    "workspace": { "semanticTokens": { "refreshSupport": true } },
                    "textDocument": { "publishDiagnostics": {} },
                });
                if pull {
                    caps["textDocument"]["diagnostic"] = json!({});
                }
                let caps: ClientCapabilities = serde_json::from_value(caps).unwrap();
                state.set_client_capabilities(caps);
                state.set_workspace_roots(vec![root]);
                run_with_fetcher(server_side, state, None).expect("the loop ends cleanly");
            })
            .expect("spawn the server");
        Client {
            conn,
            server: Some(server),
            next_id: 0,
            log,
            generations: Vec::new(),
        }
    }

    fn notify(&self, method: &str, params: Value) {
        self.conn
            .sender
            .send(Message::Notification(Notification {
                method: method.to_string(),
                params,
            }))
            .unwrap();
    }

    /// Send a request and wait for its response, acknowledging any request
    /// the server sends first. Returns the result, and how many server
    /// requests were acknowledged.
    fn request(&mut self, method: &str, params: Value) -> (Value, usize) {
        let id = RequestId::from(format!("c{}", self.next_id));
        self.next_id += 1;
        self.conn
            .sender
            .send(Message::Request(Request {
                id: id.clone(),
                method: method.to_string(),
                params,
            }))
            .unwrap();
        let mut answered = 0;
        loop {
            match self.conn.receiver.recv_timeout(RECV_TIMEOUT).unwrap() {
                Message::Response(resp) if resp.id == id => {
                    return (resp.response_result.unwrap_or(Value::Null), answered);
                }
                Message::Response(resp) => panic!("unsolicited response {resp:?}"),
                Message::Request(req) => {
                    answered += 1;
                    self.conn
                        .sender
                        .send(Message::Response(Response {
                            id: req.id,
                            response_result: Ok(Value::Null),
                        }))
                        .unwrap();
                }
                Message::Notification(_) => {}
            }
        }
    }

    /// Wait until the server has handled everything sent so far, including
    /// the acknowledgements of its own requests.
    fn settle(&mut self) {
        for _ in 0..MAX_SETTLE_ROUNDS {
            if self.request("borzoi/test/barrier", Value::Null).1 == 0 {
                return;
            }
        }
        panic!("the server kept sending requests");
    }

    /// The generations the server has ended so far: every one before the
    /// response last received, since the loop ends a message's generation
    /// before it reads the next message.
    fn generations(&mut self) -> &[Generation] {
        self.generations.extend(self.log.try_iter());
        &self.generations
    }

    fn shut_down(mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        self.server.take().unwrap().join().unwrap();
    }
}

/// An `n`-item SDK-less project whose every item uses the one before it: the
/// items' paths and texts.
fn project(dir: &Path, n: usize) -> (Vec<PathBuf>, Vec<String>) {
    let items: Vec<PathBuf> = (0..n).map(|i| dir.join(format!("F{i}.fs"))).collect();
    let includes: String = (0..n)
        .map(|i| format!("    <Compile Include=\"F{i}.fs\" />\n"))
        .collect();
    std::fs::write(
        dir.join("P.fsproj"),
        format!("<Project>\n  <ItemGroup>\n{includes}  </ItemGroup>\n</Project>\n"),
    )
    .unwrap();
    let texts: Vec<String> = (0..n)
        .map(|i| match i {
            0 => "module F0\n\nlet v0 = 0\n".to_string(),
            _ => format!("module F{i}\n\nlet v{i} = F{}.v{} + 1\n", i - 1, i - 1),
        })
        .collect();
    for (item, text) in items.iter().zip(&texts) {
        std::fs::write(item, text).unwrap();
    }
    (items, texts)
}

/// What a keystroke and the requests after it opened.
#[derive(Debug, PartialEq, Eq)]
struct Counted {
    /// The cold `semanticTokens` request on the edited document.
    cold_tokens: u64,
    /// The comment keystroke, then `semanticTokens` on the edited document.
    keystroke_and_tokens: u64,
    /// `semanticTokens` on the other open document, after that.
    other_tokens: u64,
}

/// Opens summed over the generations recorded since `from`, checking that
/// they are numbered consecutively and that there are at least `messages` of
/// them (one per message sent, before any the server's own requests cost).
fn opens_since(client: &mut Client, from: usize, messages: usize) -> (u64, usize) {
    let generations = client.generations();
    let new = &generations[from..];
    assert!(
        new.len() >= messages,
        "{messages} messages were handled in only {} generations: {new:?}",
        new.len()
    );
    for pair in generations[from.saturating_sub(1)..].windows(2) {
        assert_eq!(
            pair[1].number,
            pair[0].number + 1,
            "generations are consecutive"
        );
    }
    (new.iter().map(|g| g.counts.opens).sum(), generations.len())
}

fn run(n: usize, edited: usize, other: usize, pull: bool) -> Counted {
    let tmp = TempDir::new().unwrap();
    let (items, mut texts) = project(tmp.path(), n);
    let mut client = Client::start(tmp.path(), pull);
    let uri = |i: usize| Uri::from_file_path(&items[i]).unwrap();
    for i in [edited, other] {
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": uri(i).as_str(), "languageId": "fsharp", "version": 0, "text": texts[i],
            }}),
        );
    }
    client.settle();
    let tokens = |client: &mut Client, i: usize| {
        let (result, _) = client.request(
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": uri(i).as_str() } }),
        );
        assert!(
            result["data"].as_array().is_some_and(|d| !d.is_empty()),
            "semantic tokens for item {i}: {result}"
        );
        client.settle();
    };

    let mark = client.generations().len();
    tokens(&mut client, edited);
    let (cold_tokens, mark) = opens_since(&mut client, mark, 1);

    texts[edited].push_str("// typed");
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri(edited).as_str(), "version": 1 },
            "contentChanges": [{ "text": texts[edited] }],
        }),
    );
    tokens(&mut client, edited);
    let (keystroke_and_tokens, mark) = opens_since(&mut client, mark, 2);

    tokens(&mut client, other);
    let (other_tokens, _) = opens_since(&mut client, mark, 1);

    client.shut_down();
    Counted {
        cold_tokens,
        keystroke_and_tokens,
        other_tokens,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// A comment keystroke followed by a semantic-tokens request opens every
    /// Compile item that is not open in a buffer, exactly once; so does the
    /// cold request; and a second document's request, answered from the
    /// cached fold, opens nothing.
    #[test]
    fn a_keystroke_and_a_tokens_request_open_every_unbuffered_item(
        (n, edited, other) in (3usize..10).prop_flat_map(|n| {
            (Just(n), 0..n, 0..n - 1)
        }).prop_map(|(n, edited, other)| {
            (n, edited, if other >= edited { other + 1 } else { other })
        }),
        pull in any::<bool>(),
    ) {
        let n_minus_2 = (n - 2) as u64;
        prop_assert_eq!(
            run(n, edited, other, pull),
            Counted {
                cold_tokens: n_minus_2,
                keystroke_and_tokens: n_minus_2,
                other_tokens: 0,
            }
        );
    }
}
