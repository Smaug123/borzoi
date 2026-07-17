//! Serialised, bounded child-process launching — the one place in the workspace
//! a child is spawned.
//!
//! Two hazards live here, and both are process-global, which is why this is a
//! crate rather than a helper in each caller.
//!
//! **The descriptor leak.** On macOS there is no `pipe2`, so `std::process`
//! creates each stdio pipe with `pipe()` and only afterwards flips `FD_CLOEXEC`
//! on the two ends. A spawn on another thread inside that window inherits the raw
//! descriptors. A long-lived inheritor — the C# sidecar, a batch oracle child, an
//! MSBuild node-reuse worker — then holds a duplicate of a short-lived child's
//! stdout write end for its own lifetime, and the `read()` draining that child's
//! output never sees EOF: the call hangs forever even though the child exited.
//! (Observed 2026-07-10 as the CST crate's `parser_diff_ifdef` suite wedging in
//! `Command::output`; `sample`/`lsof` showed a concurrently-spawned batch child
//! holding the exited one-shot child's stdout write end.)
//!
//! The exclusion only works if **every** launch in the process shares **one**
//! critical section. Two locks are no lock: the LSP library spawning its sidecar
//! under one mutex while a test harness spawns `fcs-dump` under another leaves
//! exactly the window this is meant to close. So the lock is defined once, here,
//! and everything that spawns — the LSP library, the differential-test
//! harnesses, the oracle drivers — goes through these wrappers. The workspace
//! `clippy.toml` bans direct `Command::{spawn,status,output}` to enforce it
//! mechanically.
//!
//! Only the launch is serialised. The lock is a leaf — nothing else is acquired
//! while it is held, and it never spans a wait or a read — so children still run
//! concurrently and there is no ordering hazard.
//!
//! **The unbounded wait.** A child that never answers (a wedged FCS, a `dotnet`
//! on a broken toolchain) turns a test suite or an LSP request into a hang, which
//! is strictly worse than a failure: a run that stops is diagnosable, one that
//! hangs is not. [`BoundedCommand`] therefore runs every child under a deadline,
//! and that deadline covers *collecting the output*, not merely the child's exit —
//! see its docs for why the difference matters.

use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// The process-global spawn critical section. See the module docs: it must be the
/// only one in the process, or it is not an exclusion at all.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// How often a bounded run polls the child for exit. Coarse enough to stay
/// invisible in profiles, fine enough that a normal sub-second child adds no
/// perceptible latency.
const EXIT_POLL: Duration = Duration::from_millis(20);

/// [`Command::spawn`] with the process-global spawn lock held across the
/// pipe-creation + spawn window.
// The one legitimate direct `spawn` in the workspace — see the module docs and
// clippy.toml.
#[allow(clippy::disallowed_methods)]
pub fn spawn_serialised(cmd: &mut Command) -> io::Result<Child> {
    let _guard = SPAWN_LOCK.lock().expect("spawn lock poisoned");
    cmd.spawn()
}

/// [`Command::status`] over [`spawn_serialised`]: stdio inherited, the wait
/// outside the lock. Even a pipe-less launch must be serialised — its descendants
/// (an MSBuild node-reuse worker off a `dotnet build` lives ~15 minutes) would
/// otherwise inherit whatever raw pipe fds another thread's spawn had in flight.
pub fn status_serialised(cmd: &mut Command) -> io::Result<ExitStatus> {
    spawn_serialised(cmd)?.wait()
}

/// [`Command::output`] over [`spawn_serialised`]: `output`'s stdio defaults (null
/// stdin, captured stdout/stderr), the spawn locked, the wait + drain outside it.
/// Callers must not have configured stdio themselves.
///
/// Unbounded: prefer [`output_bounded`] or [`BoundedCommand`] unless the child is
/// one whose hanging you are willing to inherit.
pub fn output_serialised(cmd: &mut Command) -> io::Result<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn_serialised(cmd)?.wait_with_output()
}

/// [`output_serialised`] with a deadline: if the child has not exited *and been
/// collected* within `timeout` it is killed and `ErrorKind::TimedOut` returned.
///
/// The `io::Result`-shaped face of [`BoundedCommand`], for callers that just want
/// `Command::output` that cannot hang.
pub fn output_bounded(cmd: Command, timeout: Duration) -> io::Result<Output> {
    BoundedCommand::new(cmd)
        .timeout(timeout)
        .run()
        .map_err(io::Error::from)
}

/// How a child failed to produce an answer. Distinct from the child answering
/// *unsuccessfully* (a non-zero exit): that is the caller's business to judge.
/// This is the launch itself breaking.
#[derive(Debug)]
pub enum ChildFailure {
    /// The command could not be spawned at all.
    Spawn(io::Error),
    /// The child outlived its deadline — either it never exited, or it exited but
    /// its pipes stayed open past the budget because a descendant still holds
    /// them. It has been killed and reaped.
    Wedged {
        /// The budget it blew.
        after: Duration,
    },
    /// Input could not be delivered: the child stopped reading and then exited
    /// *successfully*. Its answer therefore covers only a prefix of what it was
    /// asked — a truncated result the caller has no way to detect, so it is a
    /// failure rather than an `Output`.
    Stdin(io::Error),
}

impl fmt::Display for ChildFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChildFailure::Spawn(e) => write!(f, "could not spawn the child: {e}"),
            ChildFailure::Wedged { after } => write!(
                f,
                "no answer within {after:?} — the child wedged (or a descendant \
                 held its pipes open past the budget); killed it"
            ),
            ChildFailure::Stdin(e) => write!(
                f,
                "the child stopped reading its input and exited successfully, so \
                 it only answered about part of the question: {e}"
            ),
        }
    }
}

impl std::error::Error for ChildFailure {}

impl From<ChildFailure> for io::Error {
    fn from(e: ChildFailure) -> io::Error {
        match e {
            ChildFailure::Spawn(io) | ChildFailure::Stdin(io) => io,
            ChildFailure::Wedged { .. } => io::Error::new(io::ErrorKind::TimedOut, e.to_string()),
        }
    }
}

/// Where a child's stderr goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stderr {
    /// Piped and drained into [`Output::stderr`].
    Capture,
    /// Straight to ours, for chatty children whose progress is worth watching
    /// live. [`Output::stderr`] is then empty.
    Inherit,
}

/// A [`Command`] run to completion under a deadline, spawned through
/// [`spawn_serialised`], with its output pipes drained concurrently and its stdin
/// streamed from a thread.
///
/// The replacement for [`Command::output`], [`Command::status`] and
/// [`Child::wait_with_output`]. Unlike those it cannot hang, and unlike them it
/// cannot deadlock: because both output pipes are drained by dedicated threads
/// while stdin is fed by a third, no combination of input and output size can wedge
/// the parent against the child. (Writing stdin synchronously with the output pipes
/// undrained is the classic form of that bug — the child fills its stdout buffer,
/// stops reading, and both sides block. It needs a large input to show up, which is
/// exactly when you least want to debug it.)
///
/// The deadline covers *collecting the output*, not just the child's exit. A child
/// can exit promptly while a descendant it spawned still holds the inherited pipes
/// (`dotnet build` leaves MSBuild worker nodes behind and does exactly this), so
/// the pipe never reaches EOF; waiting on the child under a deadline and only then
/// blocking on the drain would be an unbounded wait hiding behind a bounded one.
///
/// ```no_run
/// # use std::process::Command;
/// # use borzoi_spawn::BoundedCommand;
/// let out = BoundedCommand::new(Command::new("fcs-dump"))
///     .stdin_lines(["a.fs".to_string(), "b.fs".to_string()])
///     .run()
///     .expect("fcs-dump");
/// assert!(out.status.success());
/// ```
pub struct BoundedCommand {
    cmd: Command,
    stdin: Vec<u8>,
    stderr: Stderr,
    timeout: Duration,
}

impl BoundedCommand {
    /// Bound `cmd` by [`default_timeout`], with no stdin and captured stderr.
    pub fn new(cmd: Command) -> Self {
        BoundedCommand {
            cmd,
            stdin: Vec::new(),
            stderr: Stderr::Capture,
            timeout: default_timeout(),
        }
    }

    /// Feed these lines (each newline-terminated) to the child's stdin, then close
    /// it so the child's read loop ends.
    ///
    /// Without this the child gets an immediately-empty stdin.
    pub fn stdin_lines<I>(self, lines: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        let mut buf = Vec::new();
        for l in lines {
            buf.extend_from_slice(l.as_bytes());
            buf.push(b'\n');
        }
        self.stdin_bytes(buf)
    }

    /// Feed these bytes to the child's stdin verbatim, then close it.
    ///
    /// Written from a dedicated thread while both output pipes are drained by
    /// others, so an input larger than the pipe buffer cannot deadlock against the
    /// child's interleaved output — which is the whole reason to route stdin
    /// through here rather than writing it yourself.
    pub fn stdin_bytes(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = bytes;
        self
    }

    /// Let the child's stderr through to ours instead of capturing it.
    /// [`Output::stderr`] is then empty.
    pub fn inherit_stderr(mut self) -> Self {
        self.stderr = Stderr::Inherit;
        self
    }

    /// Override the [`default_timeout`] budget — for a child that is legitimately
    /// slow rather than wedged (a cold `dotnet build`, a whole-project type-check).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run to completion, or kill the child and report [`ChildFailure::Wedged`].
    ///
    /// A non-zero exit is *success* here: the child answered, and what its answer
    /// means is the caller's to judge (see [`run_ok`](BoundedCommand::run_ok) for
    /// the common "any failure is a bug" case).
    pub fn run(mut self) -> Result<Output, ChildFailure> {
        self.cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(match self.stderr {
                Stderr::Capture => Stdio::piped(),
                Stderr::Inherit => Stdio::inherit(),
            });
        let mut child = spawn_serialised(&mut self.cmd).map_err(ChildFailure::Spawn)?;

        // One deadline for the whole run. Three threads, so no pipe can
        // back-pressure into a deadlock: one feeding stdin, one draining stdout,
        // one draining stderr. Each owns its pipe and drops it (closing that end)
        // when done, and each reports over a channel so it can be collected under
        // the deadline rather than joined unconditionally.
        let deadline = Instant::now() + self.timeout;
        let wedged = || ChildFailure::Wedged {
            after: self.timeout,
        };

        let mut stdin = child.stdin.take().expect("stdin piped");
        let bytes = std::mem::take(&mut self.stdin);
        let writer = in_thread(move || {
            // `stdin` drops at the end of this closure, closing the pipe so the
            // child sees EOF.
            stdin.write_all(&bytes).err()
        });
        let out_rx = in_thread_drain(child.stdout.take().expect("stdout piped"));
        let err_rx = self
            .stderr
            .eq(&Stderr::Capture)
            .then(|| in_thread_drain(child.stderr.take().expect("stderr piped")));

        let Some(status) = wait_bounded(&mut child, deadline) else {
            // Killing closes the pipes, so the drain threads finish and the
            // writer's next write fails: nothing is left dangling.
            let _ = child.kill();
            let _ = child.wait();
            return Err(wedged());
        };

        // The child is gone and reaped. Its pipes are at EOF *unless* a surviving
        // descendant still holds them, so these collections stay on the clock.
        let stdout = recv_bounded(&out_rx, deadline).ok_or_else(wedged)?;
        let stderr = match err_rx {
            Some(rx) => recv_bounded(&rx, deadline).ok_or_else(wedged)?,
            None => Vec::new(),
        };
        let unwritten = recv_bounded(&writer, deadline).ok_or_else(wedged)?;

        // Input we could not deliver means the child answered about a *prefix* of
        // what it was asked — a truncated result the caller cannot detect. Report
        // it, but only when the child claims to have succeeded: a child that exited
        // non-zero is better described by its status and stderr, of which the broken
        // pipe is merely a symptom.
        if let Some(e) = unwritten
            && status.success()
        {
            return Err(ChildFailure::Stdin(e));
        }

        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    /// [`run`](BoundedCommand::run), panicking unless the child exited
    /// successfully — the common case in tests and fixtures, where any failure of
    /// the child is a bug rather than a result. `what` names it in the panic.
    pub fn run_ok(self, what: impl fmt::Display) -> Output {
        let timeout = self.timeout;
        let out = self
            .run()
            .unwrap_or_else(|e| panic!("{what} failed (budget {timeout:?}): {e}"));
        assert!(
            out.status.success(),
            "{what} exited with {:?}\nstdout: {}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        out
    }
}

/// Default budget for one child interaction.
///
/// Overridable via `BORZOI_CHILD_TIMEOUT_SECS` for slow or heavily loaded
/// machines (several test suites in sibling worktrees at once will do it).
/// Deliberately enormous relative to a healthy interaction — a warm per-snippet
/// FCS parse is milliseconds, a cold start paying JIT + FCS init is a few seconds.
/// This is not a latency policy; it is the line past which we conclude the child is
/// never going to answer.
pub fn default_timeout() -> Duration {
    std::env::var("BORZOI_CHILD_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(120))
}

/// Run `f` on its own thread, delivering its result over a channel.
///
/// A channel rather than a [`JoinHandle`](thread::JoinHandle) because a join cannot
/// be bounded, and the whole point is to stop waiting once the deadline has passed.
/// A thread abandoned that way stays blocked on its pipe until the process exits —
/// the price of not being able to kill a descendant we never spawned — but it is a
/// leaked *thread*, not a hung caller.
pub fn in_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx
}

/// Read a pipe to EOF on its own thread, so it can never fill and block the child.
fn in_thread_drain<R: Read + Send + 'static>(mut pipe: R) -> Receiver<Vec<u8>> {
    in_thread(move || {
        let mut buf = Vec::new();
        // An error mid-drain (the child was killed) still yields what we got.
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

/// Take a thread's result, or `None` if the deadline passes first.
pub fn recv_bounded<T>(rx: &Receiver<T>, deadline: Instant) -> Option<T> {
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()
}

/// Wait for `child` until `deadline`, returning `None` if it outlives it.
///
/// [`Child`] has no timed wait, so poll [`Child::try_wait`].
fn wait_bounded(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            // A wait error means we can't reap it; treat as wedged and let the
            // caller kill it.
            Err(_) => return None,
            Ok(None) => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        thread::sleep(left.min(EXIT_POLL));
    }
}
