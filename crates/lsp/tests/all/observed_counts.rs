//! The server's source reads, counted per generation by `borzoi::vfs`
//! (`docs/incremental-lsp-plan.md`, Stages 2a and 2b).
//!
//! A real dispatch loop runs on its own thread over an in-memory connection,
//! sending every generation it ends to a channel the test reads
//! ([`Vfs::log_generations`](borzoi::vfs::Vfs::log_generations)). The client opens two Compile items of a generated
//! `N`-item project, warms the fold, then types into a trailing comment of
//! one of them and asks for its semantic tokens. The cold fold opens each of
//! the `N − 2` items that are not open in a buffer; after that, every request
//! that reads the fold confirms each of them with one `stat` and opens none,
//! because nothing on disk moved.
//!
//! Finding the project that owns a file lists the directories from the file's
//! up to its project's (`Workspace::owning_project`), and the answer is
//! remembered and confirmed in each later generation by one `stat` of each
//! of those directories: a pull of a file `d` directories below its project
//! pins that.
//!
//! The project is written just before the server starts, so every stamp would
//! be racy ([`borzoi::vfs::RACY_WINDOW`]) and re-read in every generation; the
//! server's clock is set ahead ([`Clock::HostAhead`]) so the files read as
//! old, which is what a real workspace's are. The same run on the host clock
//! is the control: every request then re-reads them.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use borzoi::observed::Generation;
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::server::{SERVER_STACK_BYTES, State, run_with_fetcher};
use borzoi::uri::UriExt as _;
use borzoi::vfs::{Clock, RACY_WINDOW};
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
    fn start(root: &Path, pull: bool, clock: Clock) -> Client {
        let (server_side, conn) = Connection::memory();
        let (log_sender, log) = mpsc::channel();
        let root = root.to_path_buf();
        let server = thread::Builder::new()
            .stack_size(SERVER_STACK_BYTES)
            .spawn(move || {
                let mut state = State::new();
                state.docs.log_generations(log_sender);
                state.clock = clock;
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

/// What one stretch of messages opened and `stat`ed to read sources, how many
/// MSBuild evaluations it ran, and how many files it opened to confirm the
/// cached evaluation (`borzoi::workspace::Workspace::validate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Reads {
    opens: u64,
    stats: u64,
    evaluated: u64,
    validation_opens: u64,
}

/// What a keystroke and the requests after it read.
#[derive(Debug, PartialEq, Eq)]
struct Counted {
    /// The cold `semanticTokens` request on the edited document.
    cold_tokens: Reads,
    /// The comment keystroke, then `semanticTokens` on the edited document.
    keystroke_and_tokens: Reads,
    /// `semanticTokens` on the other open document, after that.
    other_tokens: Reads,
}

/// [`Counted`], and the paths the other document's request `stat`ed to
/// confirm the cached evaluation, which depend on how deep the temporary
/// directory sits.
type Run = (Counted, u64);

/// Counts summed over the generations recorded since `from`, checking that
/// they are numbered consecutively and that there are at least `messages` of
/// them (one per message sent, before any the server's own requests cost).
fn reads_since(client: &mut Client, from: usize, messages: usize) -> (Reads, usize, u64) {
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
    let reads = Reads {
        opens: new.iter().map(|g| g.counts.opens).sum(),
        stats: new.iter().map(|g| g.counts.stats).sum(),
        evaluated: new.iter().map(|g| g.evaluations.evaluated).sum(),
        validation_opens: new.iter().map(|g| g.evaluations.validation.opens).sum(),
    };
    let validation_stats = new.iter().map(|g| g.evaluations.validation.stats).sum();
    (reads, generations.len(), validation_stats)
}

fn run(n: usize, edited: usize, other: usize, pull: bool, clock: Clock) -> Run {
    let tmp = TempDir::new().unwrap();
    let (items, mut texts) = project(tmp.path(), n);
    let mut client = Client::start(tmp.path(), pull, clock);
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
    let (cold_tokens, mark, _) = reads_since(&mut client, mark, 1);

    texts[edited].push_str("// typed");
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri(edited).as_str(), "version": 1 },
            "contentChanges": [{ "text": texts[edited] }],
        }),
    );
    tokens(&mut client, edited);
    let (keystroke_and_tokens, mark, _) = reads_since(&mut client, mark, 2);

    tokens(&mut client, other);
    let (other_tokens, _, validation_stats) = reads_since(&mut client, mark, 1);

    client.shut_down();
    (
        Counted {
            cold_tokens,
            keystroke_and_tokens,
            other_tokens,
        },
        validation_stats,
    )
}

/// Ahead of the host's clock by more than the racy window, so the files the
/// test wrote read as old.
fn settled() -> Clock {
    Clock::HostAhead(RACY_WINDOW * 30)
}

/// Behind the host's clock, so every stamp of a file the test wrote is racy.
fn racy() -> Clock {
    Clock::HostBehind(RACY_WINDOW * 30)
}

fn cases() -> impl Strategy<Value = (usize, usize, usize, bool)> {
    (3usize..10)
        .prop_flat_map(|n| (Just(n), 0..n, 0..n - 1))
        .prop_map(|(n, edited, other)| (n, edited, if other >= edited { other + 1 } else { other }))
        .prop_flat_map(|(n, edited, other)| (Just(n), Just(edited), Just(other), any::<bool>()))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// The cold semantic-tokens request opens every Compile item that is not
    /// open in a buffer, exactly once. After that, nothing on disk moves: a
    /// comment keystroke and the request after it open nothing and `stat`
    /// each unbuffered item once, and so does a second document's request,
    /// which validates the cached fold.
    ///
    /// The project was evaluated when its documents opened, and no request
    /// after that evaluates it again: the cold request's env build reads the
    /// confirmed evaluation rather than evaluating the entry afresh, and
    /// every later request confirms it by `stat` alone (some `stat`, so the
    /// confirmation runs), opening nothing.
    #[test]
    fn after_the_cold_fold_a_keystroke_stats_and_opens_nothing(
        (n, edited, other, pull) in cases(),
    ) {
        let n_minus_2 = (n - 2) as u64;
        // A non-unix host's stamps have no change time, so they are always
        // racy (`borzoi::vfs::Stamp::ctime`) and every request re-reads.
        let warm = if cfg!(unix) {
            Reads { opens: 0, stats: n_minus_2, evaluated: 0, validation_opens: 0 }
        } else {
            Reads { opens: n_minus_2, stats: 0, evaluated: 0, validation_opens: 1 }
        };
        let (counted, validation_stats) = run(n, edited, other, pull, settled());
        prop_assert_eq!(
            counted,
            Counted {
                cold_tokens: Reads {
                    opens: n_minus_2,
                    stats: 0,
                    evaluated: 0,
                    validation_opens: warm.validation_opens,
                },
                keystroke_and_tokens: warm,
                other_tokens: warm,
            }
        );
        prop_assert!(validation_stats > 0, "the cached evaluation was not confirmed");
    }

    /// The control: with every stamp racy, every request that reads the fold
    /// re-reads each unbuffered item, and each generation that reads the
    /// evaluation re-reads the project file it read, so the counts above are
    /// the racy rule's to make and not an artefact of a server that never
    /// looks.
    #[test]
    fn with_racy_stamps_every_fold_reading_request_re_reads(
        (n, edited, other, pull) in cases(),
    ) {
        let n_minus_2 = (n - 2) as u64;
        let (counted, _) = run(n, edited, other, pull, racy());
        prop_assert_eq!(
            Counted {
                cold_tokens: Reads { validation_opens: 0, ..counted.cold_tokens },
                keystroke_and_tokens: Reads { validation_opens: 0, ..counted.keystroke_and_tokens },
                other_tokens: Reads { validation_opens: 0, ..counted.other_tokens },
            },
            Counted {
                cold_tokens: Reads { opens: n_minus_2, stats: 0, evaluated: 0, validation_opens: 0 },
                keystroke_and_tokens: Reads { opens: n_minus_2, stats: 0, evaluated: 0, validation_opens: 0 },
                other_tokens: Reads { opens: n_minus_2, stats: 0, evaluated: 0, validation_opens: 0 },
            }
        );
        prop_assert!(
            counted.other_tokens.validation_opens > 0,
            "a racy project file was confirmed without reading it: {:?}",
            counted
        );
    }
}

/// What finding the owner of one file `depth` directories below its project
/// cost a pull client's second diagnostic request for it, after a first that
/// warmed everything: an `unchanged` report, or, after a keystroke in the
/// file, a `full` one. The listings and `stat`s of the ownership walks and
/// of confirming what they remembered.
fn second_pull_ownership(depth: usize, keystroke: bool, clock: Clock) -> (u64, u64) {
    let tmp = TempDir::new().unwrap();
    let dir = (0..depth).fold(tmp.path().to_path_buf(), |dir, i| dir.join(format!("d{i}")));
    std::fs::create_dir_all(&dir).unwrap();
    let relative: String = (0..depth).map(|i| format!("d{i}/")).collect();
    std::fs::write(
        tmp.path().join("P.fsproj"),
        format!("<Project>\n  <ItemGroup>\n    <Compile Include=\"{relative}A.fs\" />\n  </ItemGroup>\n</Project>\n"),
    )
    .unwrap();
    let file = dir.join("A.fs");
    let text = "module A\n\nlet a = 1\n";
    std::fs::write(&file, text).unwrap();
    let mut client = Client::start(tmp.path(), true, clock);
    let uri = Uri::from_file_path(&file).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": {
            "uri": uri.as_str(), "languageId": "fsharp", "version": 0, "text": text,
        }}),
    );
    client.settle();
    let (first, _) = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri.as_str() } }),
    );
    client.settle();
    let result_id = first["resultId"].as_str().expect("a result id").to_string();
    if keystroke {
        client.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri.as_str(), "version": 1 },
                "contentChanges": [{ "text": format!("{text}// typed") }],
            }),
        );
        client.settle();
    }
    let mark = client.generations().len();
    let (second, _) = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri.as_str() }, "previousResultId": result_id }),
    );
    let kind = if keystroke { "full" } else { "unchanged" };
    assert_eq!(second["kind"], kind, "{second}");
    // The loop answers before it ends the request's generation.
    client.settle();
    let new = &client.generations()[mark..];
    let counts = (
        new.iter().map(|g| g.ownership.listings).sum(),
        new.iter().map(|g| g.ownership.stats).sum(),
    );
    client.shut_down();
    counts
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 8, ..ProptestConfig::default() })]

    /// A pull walks for the file's owner two or four times (twice for the
    /// result id's symbols and language version, and twice more for a
    /// `full` report's diagnostics), and an earlier generation has already
    /// walked and remembered the answer (`docs/incremental-lsp-plan.md`,
    /// Stage 4b). So the pull lists nothing, and confirms the answer by one
    /// `stat` of each directory the walk read: those from the file's up to
    /// its project's, where the project lists the file.
    #[test]
    fn a_pull_confirms_its_owner_by_one_stat_per_directory(
        depth in 0usize..4,
        keystroke in any::<bool>(),
    ) {
        let directories = depth as u64 + 1;
        prop_assert_eq!(second_pull_ownership(depth, keystroke, settled()), (0, directories));
    }

    /// The control: with every stamp racy, no witness is trusted, so the
    /// pull lists each of those directories again, once, and stamps it.
    #[test]
    fn with_racy_stamps_a_pull_lists_its_owners_directories_once(
        depth in 0usize..4,
        keystroke in any::<bool>(),
    ) {
        let directories = depth as u64 + 1;
        prop_assert_eq!(
            second_pull_ownership(depth, keystroke, racy()),
            (directories, directories)
        );
    }
}
