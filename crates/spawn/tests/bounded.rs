//! [`BoundedCommand`]'s two guarantees: a child that never answers is killed and
//! reported (rather than hanging the suite), and no size of input or output can
//! deadlock the parent against the child.
//!
//! These drive fake `sh`/`cat` "oracles", never a real one.
#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use borzoi_spawn::{BoundedCommand, ChildFailure};

/// The plain case: run a command, get its output.
#[test]
fn runs_a_command_and_returns_its_output() {
    let mut c = Command::new("sh");
    c.arg("-c").arg("printf 'hello\\n'; printf 'oops\\n' >&2");
    let out = BoundedCommand::new(c).run().expect("sh runs");

    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "oops\n");
}

/// stdin lines are delivered, and the pipe is closed afterwards so the child's
/// read loop terminates (otherwise `cat` would never exit and we'd time out).
#[test]
fn feeds_stdin_lines_and_closes_the_pipe() {
    let out = BoundedCommand::new(Command::new("cat"))
        .stdin_lines(["one".to_string(), "two".to_string()])
        .run()
        .expect("cat runs");

    assert_eq!(String::from_utf8_lossy(&out.stdout), "one\ntwo\n");
}

/// A child that exits non-zero has still *answered*: that's `Ok`, and judging the
/// status is the caller's business. (Only the harness breaking is an
/// `OracleFailure`.)
#[test]
fn a_nonzero_exit_is_an_answer_not_a_failure() {
    let mut c = Command::new("sh");
    c.arg("-c").arg("exit 3");
    let out = BoundedCommand::new(c).run().expect("sh runs");

    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(3));
}

/// The wedge — the failure this crate exists for. A child that never exits must
/// be killed and reported *within its budget*, not waited on forever.
#[test]
fn a_wedged_child_is_killed_and_reported_within_its_budget() {
    let mut c = Command::new("sh");
    c.arg("-c").arg("sleep 600");

    let started = Instant::now();
    let err = BoundedCommand::new(c)
        .timeout(Duration::from_millis(300))
        .run()
        .expect_err("a child that never exits must not be waited on");
    let waited = started.elapsed();

    assert!(
        matches!(err, ChildFailure::Wedged { .. }),
        "expected a wedge, got {err:?}"
    );
    // Bounded, and loose enough not to flake on a loaded machine: the point is
    // that it returned at all rather than blocking for the child's full 600 s.
    assert!(waited < Duration::from_secs(60), "took {waited:?}");
}

/// The pipe deadlock, which is the parent's fault and therefore preventable.
///
/// A naive driver writes stdin synchronously while stdout sits piped and
/// undrained: the child fills its stdout buffer, stops reading stdin, and both
/// sides block forever. It needs an input bigger than a pipe buffer (~64 KiB) to
/// show up — so sweep sizes from comfortably under one to comfortably over
/// several, echoing each line straight back so stdin and stdout are both large.
///
/// `cat` is the harshest such child: it never drains stdin ahead of its output.
#[test]
fn large_stdin_echoed_to_stdout_cannot_deadlock() {
    for lines in [1, 1_000, 50_000, 200_000] {
        let input: Vec<String> = (0..lines)
            .map(|i| format!("line-{i}-{}", "x".repeat(40)))
            .collect();
        let expected: String = input.iter().map(|l| format!("{l}\n")).collect();

        let out = BoundedCommand::new(Command::new("cat"))
            .stdin_lines(input)
            .timeout(Duration::from_secs(60))
            .run()
            .unwrap_or_else(|e| panic!("cat deadlocked or wedged at {lines} lines: {e}"));

        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            expected,
            "cat round-trip lost data at {lines} lines"
        );
    }
}

/// `run_ok` is the "any failure is a harness bug" shorthand: it panics loudly,
/// naming the child, rather than returning.
#[test]
#[should_panic(expected = "the-oracle")]
fn run_ok_panics_loudly_naming_the_child() {
    let mut c = Command::new("sh");
    c.arg("-c").arg("exit 1");
    BoundedCommand::new(c).run_ok("the-oracle");
}

/// The deadline must cover *collecting the output*, not just the child's exit.
///
/// A child can exit promptly while a surviving descendant still holds the
/// inherited stdout — `dotnet build` does exactly this, leaving MSBuild worker
/// nodes behind. The pipe then never reaches EOF, so a driver that waits for the
/// child and only *then* joins its drain threads has an unbounded wait hiding
/// behind a bounded one: the deadline passes, the child is long gone, and the
/// read blocks on the descendant regardless.
#[test]
fn a_descendant_holding_stdout_open_cannot_outlive_the_deadline() {
    let mut c = Command::new("sh");
    // The parent exits immediately; the backgrounded child inherits stdout and
    // sits on it for far longer than the budget below.
    c.arg("-c").arg("sleep 120 & exit 0");

    let started = Instant::now();
    let err = BoundedCommand::new(c)
        .timeout(Duration::from_millis(500))
        .run()
        .expect_err("stdout is still held open, so the output can't be collected");
    let waited = started.elapsed();

    assert!(
        matches!(err, ChildFailure::Wedged { .. }),
        "expected a wedge, got {err:?}"
    );
    assert!(
        waited < Duration::from_secs(60),
        "waited {waited:?} — the deadline didn't cover the drain"
    );
}

/// Undelivered stdin must not pass for a successful run.
///
/// If the child stops reading and exits *successfully* while input is still being
/// written, the run is not a good one: the oracle answered about a prefix of what
/// it was asked. Silently returning that `Output` would hand a truncated
/// differential result to a caller that has no way to tell — so the write error is
/// preserved and surfaced.
///
/// (A child that exits *unsuccessfully* keeps reporting its exit status and
/// stderr, which is the better diagnostic; the broken pipe there is a symptom,
/// not the cause.)
#[test]
fn stdin_that_could_not_be_delivered_is_not_silently_dropped() {
    let mut c = Command::new("sh");
    // Reads one line of many, then exits 0 — so the rest of the input hits a
    // closed pipe. The input is far larger than a pipe buffer, so the write
    // genuinely fails rather than sitting in the kernel unread.
    c.arg("-c").arg("IFS= read -r l; exit 0");
    let lines: Vec<String> = (0..200_000).map(|i| format!("line-{i}")).collect();

    let err = BoundedCommand::new(c)
        .stdin_lines(lines)
        .timeout(Duration::from_secs(30))
        .run()
        .expect_err("the child never received most of its input");

    assert!(
        matches!(err, ChildFailure::Stdin(_)),
        "expected an undelivered-stdin failure, got {err:?}"
    );
}

/// ...but a child that exits non-zero still reports *that*, rather than the
/// broken pipe its death caused. The exit status and stderr are the useful
/// diagnostic; the failed write is a consequence of it.
#[test]
fn a_failing_child_reports_its_exit_status_not_the_broken_pipe() {
    let mut c = Command::new("sh");
    c.arg("-c").arg("printf 'boom\\n' >&2; exit 7");
    let lines: Vec<String> = (0..200_000).map(|i| format!("line-{i}")).collect();

    let out = BoundedCommand::new(c)
        .stdin_lines(lines)
        .timeout(Duration::from_secs(30))
        .run()
        .expect("a non-zero exit is an answer, not a harness failure");

    assert_eq!(out.status.code(), Some(7));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "boom\n");
}
