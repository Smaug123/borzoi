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
//! A source is parsed once per text (Stage 5): a keystroke's diagnostics
//! parse the edited buffer through the parse memo, and the fold the next
//! request builds reads that parse rather than parsing again.
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
use borzoi::parses::ParseCounts;
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

/// What the parse memo did for each stretch of messages after a comment
/// keystroke in `edited`, in a project of `n` items with `other` open too:
/// the keystroke, then (a pull client) the edited document's diagnostics,
/// then the edited document's semantic tokens, then the other's.
fn keystroke_parses(n: usize, edited: usize, other: usize, pull: bool) -> Vec<ParseCounts> {
    let tmp = TempDir::new().unwrap();
    let (items, mut texts) = project(tmp.path(), n);
    let mut client = Client::start(tmp.path(), pull, settled());
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
    let request = |client: &mut Client, method: &str, i: usize| {
        client.request(
            method,
            json!({ "textDocument": { "uri": uri(i).as_str() } }),
        );
        client.settle();
    };
    // Warm everything: the fold, and both documents' diagnostics.
    request(&mut client, "textDocument/semanticTokens/full", edited);
    if pull {
        request(&mut client, "textDocument/diagnostic", edited);
        request(&mut client, "textDocument/diagnostic", other);
    }
    let parses_since = |client: &mut Client, from: usize| {
        let generations = client.generations();
        let counts = generations[from..]
            .iter()
            .fold(ParseCounts::default(), |sum, g| ParseCounts {
                parsed: sum.parsed + g.parses.parsed,
                reused: sum.reused + g.parses.reused,
            });
        (counts, generations.len())
    };
    let mut stretches = Vec::new();
    let mark = client.generations().len();
    texts[edited].push_str("// typed");
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri(edited).as_str(), "version": 1 },
            "contentChanges": [{ "text": texts[edited] }],
        }),
    );
    client.settle();
    let (counts, mut mark) = parses_since(&mut client, mark);
    stretches.push(counts);
    if pull {
        request(&mut client, "textDocument/diagnostic", edited);
        let (counts, next) = parses_since(&mut client, mark);
        stretches.push(counts);
        mark = next;
    }
    for i in [edited, other] {
        request(&mut client, "textDocument/semanticTokens/full", i);
        let (counts, next) = parses_since(&mut client, mark);
        stretches.push(counts);
        mark = next;
    }
    client.shut_down();
    stretches
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, ..ProptestConfig::default() })]

    /// A keystroke parses the edited text once. Its diagnostics parse it,
    /// from `didChange` for a push client and from the diagnostic request
    /// for a pull client, and the fold the edited document's semantic tokens
    /// build reuses that parse; the other document's request reads the fold
    /// built for the first and parses nothing.
    #[test]
    fn a_keystroke_parses_the_edited_text_once((n, edited, other, pull) in cases()) {
        let parsed_once = ParseCounts { parsed: 1, reused: 0 };
        let reused_once = ParseCounts { parsed: 0, reused: 1 };
        let nothing = ParseCounts::default();
        let want = if pull {
            vec![nothing, parsed_once, reused_once, nothing]
        } else {
            vec![parsed_once, reused_once, nothing]
        };
        prop_assert_eq!(keystroke_parses(n, edited, other, pull), want);
    }
}

/// What the parse memo did for each request on a buffer no project owns,
/// after a pull of its diagnostics parsed it: hover, definition, references,
/// document symbols, semantic tokens and workspace symbols, each answered by
/// single-file analysis.
fn orphan_request_parses() -> Vec<(&'static str, ParseCounts)> {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("A.fs");
    let text = "module A\n\nlet a = 1\nlet b = a + 1\n";
    std::fs::write(&file, text).unwrap();
    let mut client = Client::start(tmp.path(), true, settled());
    let uri = Uri::from_file_path(&file).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": {
            "uri": uri.as_str(), "languageId": "fsharp", "version": 0, "text": text,
        }}),
    );
    client.settle();
    let document = json!({ "uri": uri.as_str() });
    let at_use = json!({ "textDocument": document, "position": { "line": 3, "character": 8 } });
    let requests: Vec<(&'static str, Value)> = vec![
        (
            "textDocument/diagnostic",
            json!({ "textDocument": document }),
        ),
        ("textDocument/hover", at_use.clone()),
        ("textDocument/definition", at_use.clone()),
        (
            "textDocument/references",
            json!({
                "textDocument": document,
                "position": { "line": 3, "character": 8 },
                "context": { "includeDeclaration": true },
            }),
        ),
        (
            "textDocument/documentSymbol",
            json!({ "textDocument": document }),
        ),
        (
            "textDocument/semanticTokens/full",
            json!({ "textDocument": document }),
        ),
        ("workspace/symbol", json!({ "query": "a" })),
    ];
    let mut stretches = Vec::new();
    for (method, params) in requests {
        let mark = client.generations().len();
        client.request(method, params);
        client.settle();
        let counts = client.generations()[mark..]
            .iter()
            .fold(ParseCounts::default(), |sum, g| ParseCounts {
                parsed: sum.parsed + g.parses.parsed,
                reused: sum.reused + g.parses.reused,
            });
        stretches.push((method, counts));
    }
    client.shut_down();
    stretches
}

/// Single-file analysis reads the parse the diagnostics made: after the pull
/// parses the buffer, no request on it parses again, and each reads a parse
/// the memo holds.
#[test]
fn single_file_requests_reuse_the_diagnostics_parse() {
    let stretches = orphan_request_parses();
    let (first, rest) = stretches.split_first().unwrap();
    assert_eq!(
        first.1.parsed, 1,
        "the pull parses the buffer: {stretches:?}"
    );
    for (method, counts) in rest {
        assert!(
            counts.parsed == 0 && counts.reused > 0,
            "{method} parsed again or read no parse: {stretches:?}"
        );
    }
}

/// A watch event a client sends after a save (`docs/incremental-lsp-plan.md`,
/// Stage 7), and the change, if any, the save made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Saved {
    /// Another project's `.fsproj`, which nothing of `P`'s reads, reported
    /// changed without being written.
    UnrelatedProject,
    /// A source created beside the other project, reported created.
    UnrelatedSource,
    /// `P`'s own `.fsproj`, written back with the bytes it held.
    OwnProjectUnchanged,
    /// A Compile item no buffer holds, written back with the bytes it held.
    SourceUnchanged,
    /// `P`'s own `.fsproj`, given a define: the control, which must
    /// evaluate it again.
    OwnProjectChanged,
}

/// What a generation's work beyond confirming came to: MSBuild evaluations
/// run, sources parsed, assembly envs built and Compile-order folds computed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Recomputed {
    evaluated: u64,
    parsed: u64,
    envs_built: u64,
    folds: u64,
}

/// Open two items of an `n`-item project `P` beside a one-item project `Q`,
/// warm both documents' folds, make the save `saved` describes and send its
/// watch event, then ask for both documents' semantic tokens (and, from a
/// pull client, their diagnostics): what that recomputed.
fn after_a_save(n: usize, edited: usize, other: usize, pull: bool, saved: Saved) -> Recomputed {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("P");
    let q = tmp.path().join("Q");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::create_dir_all(&q).unwrap();
    let (items, texts) = project(&p, n);
    std::fs::write(
        q.join("Q.fsproj"),
        "<Project>\n  <ItemGroup>\n    <Compile Include=\"Q.fs\" />\n  </ItemGroup>\n</Project>\n",
    )
    .unwrap();
    std::fs::write(q.join("Q.fs"), "module Q\n\nlet q = 0\n").unwrap();
    let mut client = Client::start(tmp.path(), pull, settled());
    let uri = |i: usize| Uri::from_file_path(&items[i]).unwrap();
    for i in [edited, other] {
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": uri(i).as_str(), "languageId": "fsharp", "version": 0, "text": texts[i],
            }}),
        );
    }
    let requests = |client: &mut Client| {
        for i in [edited, other] {
            if pull {
                client.request(
                    "textDocument/diagnostic",
                    json!({ "textDocument": { "uri": uri(i).as_str() } }),
                );
            }
            let (result, _) = client.request(
                "textDocument/semanticTokens/full",
                json!({ "textDocument": { "uri": uri(i).as_str() } }),
            );
            assert!(
                result["data"].as_array().is_some_and(|d| !d.is_empty()),
                "semantic tokens for item {i}: {result}"
            );
        }
        client.settle();
    };
    requests(&mut client);

    let rewrite = |path: &Path, bytes: &[u8]| std::fs::write(path, bytes).unwrap();
    let fsproj = p.join("P.fsproj");
    let closed = (0..n)
        .find(|i| ![edited, other].contains(i))
        .expect("a third item");
    let (path, typ) = match saved {
        Saved::UnrelatedProject => (q.join("Q.fsproj"), 2),
        Saved::UnrelatedSource => {
            std::fs::write(q.join("New.fs"), "module New\n").unwrap();
            (q.join("New.fs"), 1)
        }
        Saved::OwnProjectUnchanged => {
            rewrite(&fsproj, &crate::common::host_fs::read(&fsproj).unwrap());
            (fsproj, 2)
        }
        Saved::SourceUnchanged => {
            rewrite(&items[closed], texts[closed].as_bytes());
            (items[closed].clone(), 2)
        }
        Saved::OwnProjectChanged => {
            let text = crate::common::host_fs::read_to_string(&fsproj)
                .unwrap()
                .replace(
                "<Project>\n",
                "<Project>\n  <PropertyGroup><DefineConstants>SAVED</DefineConstants></PropertyGroup>\n",
            );
            rewrite(&fsproj, text.as_bytes());
            (fsproj, 2)
        }
    };
    let mark = client.generations().len();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{
            "uri": Uri::from_file_path(&path).unwrap().as_str(),
            "type": typ,
        }]}),
    );
    requests(&mut client);
    let generations = &client.generations()[mark..];
    let recomputed = Recomputed {
        evaluated: generations.iter().map(|g| g.evaluations.evaluated).sum(),
        parsed: generations.iter().map(|g| g.parses.parsed).sum(),
        envs_built: generations.iter().map(|g| g.envs.built).sum(),
        folds: generations.iter().map(|g| g.folds).sum(),
    };
    client.shut_down();
    recomputed
}

fn saves() -> impl Strategy<Value = Saved> {
    prop_oneof![
        Just(Saved::UnrelatedProject),
        Just(Saved::UnrelatedSource),
        Just(Saved::OwnProjectUnchanged),
        Just(Saved::SourceUnchanged),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// A watch event names a path the disk moved at, and is a hint
    /// (`docs/incremental-lsp-plan.md`, Stage 7): when nothing the project's
    /// evaluation, env, parses or fold read has changed, the requests after
    /// it evaluate nothing, parse nothing, build no env and fold nothing. An
    /// event once dropped every evaluation and every fold (a project-file
    /// event), or the folds of every project listing the file (a source
    /// event), whether or not anything had moved.
    #[test]
    fn a_save_that_changes_no_input_recomputes_nothing(
        (n, edited, other, pull) in cases(),
        saved in saves(),
    ) {
        prop_assume!(n >= 3);
        prop_assert_eq!(
            after_a_save(n, edited, other, pull, saved),
            Recomputed::default(),
            "{:?}",
            saved
        );
    }

    /// The control: a save that gives `P` a define evaluates it again, once,
    /// and folds it again, so the counts above are the inputs' to make and
    /// not a server that never looks.
    #[test]
    fn a_save_that_changes_the_project_recomputes_it(
        (n, edited, other, pull) in cases(),
    ) {
        let recomputed = after_a_save(n, edited, other, pull, Saved::OwnProjectChanged);
        prop_assert_eq!(recomputed.evaluated, 1, "{:?}", recomputed);
        prop_assert!(recomputed.folds >= 1, "{:?}", recomputed);
    }
}

/// How many `workspace/semanticTokens/refresh` requests the server sent
/// after each step, when the step is a keystroke in an open item and then a
/// request for its tokens, or a closed item rewritten on disk with no watch
/// event and then the same request.
fn refreshes(n: usize, edited: usize, other: usize, pull: bool) -> (usize, usize, usize, usize) {
    let tmp = TempDir::new().unwrap();
    let (items, mut texts) = project(tmp.path(), n);
    let mut client = Client::start(tmp.path(), pull, settled());
    let uri = |i: usize| Uri::from_file_path(&items[i]).unwrap();
    for i in [edited, other] {
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": uri(i).as_str(), "languageId": "fsharp", "version": 0, "text": texts[i],
            }}),
        );
    }
    let tokens = |client: &mut Client, i: usize| {
        client
            .request(
                "textDocument/semanticTokens/full",
                json!({ "textDocument": { "uri": uri(i).as_str() } }),
            )
            .1
    };
    tokens(&mut client, edited);
    tokens(&mut client, other);
    client.settle();
    // A barrier answers every refresh the server sent before it.
    let barrier = |client: &mut Client| client.request("borzoi/test/barrier", Value::Null).1;

    texts[edited].push_str("// typed");
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri(edited).as_str(), "version": 1 },
            "contentChanges": [{ "text": texts[edited] }],
        }),
    );
    let after_keystroke = barrier(&mut client);
    let after_its_tokens = tokens(&mut client, edited) + barrier(&mut client);

    let closed = (0..n)
        .find(|i| ![edited, other].contains(i))
        .expect("a third item");
    std::fs::write(&items[closed], format!("{}// written\n", texts[closed])).unwrap();
    let after_write = barrier(&mut client);
    let after_write_tokens = tokens(&mut client, edited) + barrier(&mut client);
    client.shut_down();
    (
        after_keystroke,
        after_its_tokens,
        after_write,
        after_write_tokens,
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, ..ProptestConfig::default() })]

    /// A keystroke owes one semantic-token refresh, at the keystroke, and
    /// the request that then finds the fold stale owes none: the change was
    /// announced. A closed item rewritten on disk with no watch event owes
    /// one, from the request that finds it, since nothing announced it.
    #[test]
    fn each_change_owes_one_refresh((n, edited, other, pull) in cases()) {
        prop_assume!(n >= 3);
        prop_assert_eq!(refreshes(n, edited, other, pull), (1, 0, 0, 1));
    }
}

/// The refreshes two announcement lifecycles owe (found by Codex): a watch
/// event for a closed item that changed nothing, then the same item
/// rewritten with no event; and a keystroke in a source two cached projects
/// compile, then a request on each project.
#[test]
fn an_announcement_speaks_only_for_changes_after_the_last_confirmation() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("P");
    let q = tmp.path().join("Q");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::create_dir_all(&q).unwrap();
    let (items, mut texts) = project(&p, 4);
    std::fs::write(
        q.join("Q.fsproj"),
        "<Project>\n  <ItemGroup>\n    <Compile Include=\"../P/F0.fs\" />\n    <Compile Include=\"Q.fs\" />\n  </ItemGroup>\n</Project>\n",
    )
    .unwrap();
    let q_text = "module Q\n\nlet q = F0.v0 + 1\n";
    std::fs::write(q.join("Q.fs"), q_text).unwrap();
    let mut client = Client::start(tmp.path(), true, settled());
    let open = |client: &Client, path: &Path, text: &str| {
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": Uri::from_file_path(path).unwrap().as_str(),
                "languageId": "fsharp", "version": 0, "text": text,
            }}),
        );
    };
    let tokens = |client: &mut Client, path: &Path| {
        let (result, answered) = client.request(
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": Uri::from_file_path(path).unwrap().as_str() } }),
        );
        assert!(
            result["data"].as_array().is_some_and(|d| !d.is_empty()),
            "semantic tokens for {}: {result}",
            path.display()
        );
        answered
    };
    let barrier = |client: &mut Client| client.request("borzoi/test/barrier", Value::Null).1;
    open(&client, &items[0], &texts[0]);
    open(&client, &items[1], &texts[1]);
    open(&client, &q.join("Q.fs"), q_text);
    tokens(&mut client, &items[1]);
    tokens(&mut client, &q.join("Q.fs"));
    client.settle();

    // A watch event for the closed `F3` that changed nothing owes its
    // refresh; the request after it finds nothing moved.
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": Uri::from_file_path(&items[3]).unwrap().as_str(), "type": 2 }] }),
    );
    assert_eq!(barrier(&mut client), 1, "the event owes a refresh");
    assert_eq!(tokens(&mut client, &items[1]) + barrier(&mut client), 0);
    // `F3` rewritten with no event: the old announcement does not speak for it.
    std::fs::write(&items[3], format!("{}// written\n", texts[3])).unwrap();
    assert_eq!(
        tokens(&mut client, &items[1]) + barrier(&mut client),
        1,
        "an unannounced change after an announcement owes a refresh"
    );

    // A keystroke in `F0`, which both projects compile: one refresh, and
    // neither project's request owes another.
    texts[0].push_str("// typed");
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": Uri::from_file_path(&items[0]).unwrap().as_str(), "version": 1 },
            "contentChanges": [{ "text": texts[0] }],
        }),
    );
    assert_eq!(barrier(&mut client), 1, "the keystroke owes a refresh");
    assert_eq!(
        tokens(&mut client, &items[1]) + barrier(&mut client),
        0,
        "P"
    );
    assert_eq!(
        tokens(&mut client, &q.join("Q.fs")) + barrier(&mut client),
        0,
        "Q"
    );
    client.shut_down();
}
