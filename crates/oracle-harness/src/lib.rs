//! The long-lived oracle child: a `<tool> …-batch` process driven as a
//! request/response loop.
//!
//! The differential tests diff against out-of-process oracles (`fcs-dump`,
//! `msbuild-condition-oracle`, `nuget-oracle`). Each costs ~150–300 ms of .NET +
//! FCS startup and the per-case tests make thousands of calls, so the oracle is
//! kept resident and driven in lock-step rather than respawned per case.
//!
//! The *one-shot* primitive ([`BoundedCommand`]) and the process-global spawn lock
//! live in [`borzoi_spawn`], shared with the LSP library — the lock is only
//! sound if every launch in the process takes the same one, and an LSP test binary
//! spawns children from both. This crate adds only what is specific to a resident
//! oracle: the request/response loop, and recovering from an oracle that wedges
//! mid-conversation.
//!
//! It also carries [`panic_silence`], which is not about oracles at all: it is
//! what the harnesses share for suppressing *expected* panic messages now that
//! each crate's cases live in one test binary and a hook swap would race.
//!
//! Test-only: a `dev-dependency` of the harnesses, never of shipping code.

pub mod module_tree;
pub mod panic_silence;

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

pub use borzoi_spawn::{BoundedCommand, ChildFailure, default_timeout};

/// How a resident oracle failed to answer a request. Distinct from the oracle
/// answering *unsuccessfully* (an `{"error": …}` payload, a parse failure the
/// caller expects): those are results. This is the conversation itself breaking.
#[derive(Debug)]
pub enum BatchFailure {
    /// No answer within the budget: the oracle is alive but silent (an internal
    /// FCS/MSBuild deadlock, observed in the wild), or a request too large for the
    /// pipe buffer could not even be delivered to a child that stopped reading.
    Wedged {
        /// The budget it blew.
        after: Duration,
    },
    /// The oracle closed its stdout without answering — it crashed.
    Crashed,
    /// Writing the request failed outright.
    Stdin(std::io::Error),
}

impl std::fmt::Display for BatchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BatchFailure::Wedged { after } => write!(
                f,
                "no answer within {after:?} — the oracle wedged (alive but silent); \
                 killed it. Raise BORZOI_CHILD_TIMEOUT_SECS if the machine is \
                 merely loaded (e.g. sibling worktrees running suites concurrently)"
            ),
            BatchFailure::Crashed => {
                write!(f, "the oracle closed stdout before answering (it crashed)")
            }
            BatchFailure::Stdin(e) => write!(f, "writing the request to the oracle failed: {e}"),
        }
    }
}

impl std::error::Error for BatchFailure {}
use borzoi_spawn::{in_thread, recv_bounded, spawn_serialised};

/// A long-lived `<oracle> …-batch` child driven as a request/response loop: write
/// one request line, read exactly the one response line it produces.
///
/// The oracles all cost ~150–300 ms of .NET + FCS startup, and the per-case
/// differential tests make thousands of calls, so paying that once per *test
/// binary* rather than once per case is the difference between a usable suite and
/// an unusable one. The children flush one record per request without needing
/// their stdin closed, which is what makes the lock-step loop possible.
///
/// The child's stdout is drained by a reader thread feeding a channel, so
/// [`request`](Self::request) waits on it with a **timeout** rather than blocking
/// forever. If no answer arrives the oracle has wedged (observed in the wild:
/// alive, ~no CPU, never answers); it is killed, reaped, respawned, and the
/// request retried on the fresh child, up to [`attempts`](Self::with_factory)
/// times, and only then does it panic. A respawn discards the whole stdout
/// channel, so a late line from a dead child can never be mistaken for the answer
/// to a later request.
pub struct BatchChild {
    /// Builds a fresh oracle command (without stdio, which `spawn_io` configures).
    /// Stored so a wedged child can be replaced in place; also the seam the
    /// recovery tests inject fake oracles through.
    make_command: Box<dyn FnMut() -> Command + Send>,
    /// What the child is, for panic messages.
    what: String,
    /// Per-attempt budget for one response.
    timeout: Duration,
    /// How many times to (re)spawn-and-try before giving up.
    attempts: u32,
    io: ChildIo,
}

/// The live child and its pipes: `stdin` to send requests, plus the `rx` end of
/// the channel fed by the stdout reader thread.
///
/// `stdin` is an [`Option`] because each write happens on a worker thread that
/// owns the pipe for the duration (see [`BatchChild::try_request`]) and hands it
/// back on completion. It is `None` only while a write is in flight, or
/// permanently if that write wedged — in which case this whole `ChildIo` is
/// replaced by a respawn.
struct ChildIo {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<String>,
}

impl BatchChild {
    /// Drive `program …args` as a batch oracle, with the [`default_timeout`] per
    /// attempt and one retry on a fresh child.
    pub fn spawn<S: AsRef<OsStr>>(program: S, args: &[&str]) -> Self {
        let program = program.as_ref().to_os_string();
        let what = format!("{} {}", program.to_string_lossy(), args.join(" "));
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        let factory = move || {
            let mut c = Command::new(&program);
            c.args(&args);
            c
        };
        Self::with_factory(Box::new(factory), what, default_timeout(), 2)
    }

    /// Construct over an arbitrary command factory and bounds — the seam the
    /// recovery tests use to inject fake oracles (responder / wedge /
    /// wedge-then-recover). `make_command` must yield a command *without* stdio
    /// configured; the spawn sets the pipes itself.
    pub fn with_factory(
        mut make_command: Box<dyn FnMut() -> Command + Send>,
        what: impl Into<String>,
        timeout: Duration,
        attempts: u32,
    ) -> Self {
        assert!(attempts >= 1, "need at least one attempt");
        let io = Self::spawn_io(&mut *make_command);
        BatchChild {
            make_command,
            what: what.into(),
            timeout,
            attempts,
            io,
        }
    }

    /// Spawn the child with piped stdin/stdout and start the stdout reader thread.
    /// The thread exits when the child closes stdout (EOF or kill) or the receiver
    /// is dropped, so it never outlives its child.
    ///
    /// stderr is inherited: the oracles' diagnostics are worth seeing live, and an
    /// inherited pipe is one that cannot fill and wedge the child.
    fn spawn_io(make_command: &mut dyn FnMut() -> Command) -> ChildIo {
        let mut cmd = make_command();
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = spawn_serialised(&mut cmd).expect("spawn batch oracle");
        let stdin = child.stdin.take().expect("child stdin piped");
        let stdout = child.stdout.take().expect("child stdout piped");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break, // child closed stdout or errored
                    Ok(_) => {
                        if tx.send(line).is_err() {
                            break; // request side gone
                        }
                    }
                }
            }
        });
        ChildIo {
            child,
            stdin: Some(stdin),
            rx,
        }
    }

    /// Send one `request` line and return the single response line.
    ///
    /// Callers must serialise their round-trips (the shared children live behind a
    /// [`Mutex`](std::sync::Mutex)): a request and its response are matched
    /// *positionally*, so overlapping round-trips would cross their answers.
    ///
    /// A wedged or crashed oracle is killed, reaped, and respawned, and the request
    /// retried on the fresh child, up to `attempts` times; only then does it panic.
    pub fn request(&mut self, request: &str) -> String {
        let mut last = None;
        for attempt in 1..=self.attempts {
            match self.try_request(request) {
                Ok(line) => return line,
                Err(why) => {
                    last = Some(why);
                    // Discard the wedged/dead child (reaping it, so no zombie)
                    // before the next attempt talks to a fresh process.
                    let _ = self.io.child.kill();
                    let _ = self.io.child.wait();
                    if attempt < self.attempts {
                        self.io = Self::spawn_io(&mut *self.make_command);
                    }
                }
            }
        }
        panic!(
            "{} failed {} time(s) answering {request:?}: {} (see the child's stderr above)",
            self.what,
            self.attempts,
            last.expect("a failed attempt records why"),
        );
    }

    /// One write-then-wait round-trip against the current child, the whole
    /// round-trip on one deadline.
    ///
    /// The *write* is bounded too, not just the read. A request larger than the
    /// pipe buffer against an oracle that has stopped reading blocks in the write
    /// itself — before the response deadline starts — so a blocking write here
    /// would strand the caller upstream of all the recovery logic below. Requests
    /// are arbitrary strings (the msbuild oracle sends JSON carrying whole property
    /// sets), so their size is not something to bet the guarantee on.
    ///
    /// The write therefore happens on a worker, which hands the pipe back when it
    /// completes. If it does not complete in time we never take the pipe back —
    /// the caller kills and respawns the child, and a fresh [`ChildIo`] brings a
    /// fresh stdin with it, so the abandoned worker's next write fails and it
    /// exits.
    fn try_request(&mut self, request: &str) -> Result<String, BatchFailure> {
        let deadline = Instant::now() + self.timeout;
        let wedged = || BatchFailure::Wedged {
            after: self.timeout,
        };

        let mut stdin = self
            .io
            .stdin
            .take()
            .expect("stdin is restored after each write");
        let line = request.to_string();
        let written = in_thread(move || {
            let r = writeln!(stdin, "{line}").and_then(|()| stdin.flush());
            (stdin, r)
        });
        match recv_bounded(&written, deadline) {
            // The write landed (or failed outright); either way the pipe is ours
            // again for the next request.
            Some((stdin, r)) => {
                self.io.stdin = Some(stdin);
                r.map_err(BatchFailure::Stdin)?;
            }
            None => return Err(wedged()),
        }

        match self
            .io
            .rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(line) => Ok(line),
            Err(RecvTimeoutError::Timeout) => Err(wedged()),
            Err(RecvTimeoutError::Disconnected) => Err(BatchFailure::Crashed),
        }
    }
}

/// Kill and reap the child when the driver goes away.
///
/// Dropping a [`Child`] does *not* kill it — the oracle would be left running,
/// holding its pipes, until the test process exited. That is invisible for the
/// long-lived children owned by a `static`, but a `BatchChild` with a scoped
/// lifetime (one per test, as the msbuild and nuget oracles are) would otherwise
/// leak a .NET process per test.
impl Drop for BatchChild {
    fn drop(&mut self) {
        let _ = self.io.child.kill();
        let _ = self.io.child.wait();
    }
}
