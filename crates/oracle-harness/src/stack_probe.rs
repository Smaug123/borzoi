//! Cases that may overflow the stack, each run in a child copy of the test
//! binary.
//!
//! A stack overflow aborts the process, and `catch_unwind` cannot recover it.
//! A test that drives a recursive walk at its limit in-process therefore turns
//! a regression into an abort of the whole test binary, naming nothing and
//! hiding every other result. Here each case runs in a fresh child process
//! instead, so the parent learns which cases overflowed and keeps going.
//!
//! The child is the same test binary, asked to run one `#[ignore]`d test (the
//! *child entry*) with an environment variable naming the case:
//!
//! ```ignore
//! #[test]
//! #[ignore = "child-process entry for the stack probes"]
//! fn probe_child() {
//!     let Some(case) = stack_probe::requested_case(PROBE_VAR) else {
//!         return;
//!     };
//!     let facts = run_the_case_on_a_sized_thread(&case);
//!     stack_probe::report(&facts);
//! }
//! ```
//!
//! and the parent calls [`run_case`] with that test's path. Run from the
//! parent's own sweep (`--ignored`), the entry finds no variable and returns.

use std::io::Write;
use std::process::Command;
use std::time::Duration;

use borzoi_spawn::BoundedCommand;

/// The prefix of the one stdout line a child reports its case's facts on.
const REPORT_PREFIX: &str = "stack-probe-report: ";

/// What Rust's runtime prints on a stack overflow, just before it aborts.
const OVERFLOW_MESSAGE: &str = "has overflowed its stack";

/// How long one child may take. A case walks a deep input in a debug build,
/// typically beside a sibling on every core, and takes seconds when the
/// machine is idle and minutes when it is not; this bounds a wedge, not
/// latency.
const CHILD_TIMEOUT: Duration = Duration::from_secs(900);

/// How one case ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The case ran to the end and reported this.
    Survived(String),
    /// The case overflowed its stack.
    Overflowed,
}

/// In a child: the case this process was spawned to run, as the parent spelled
/// it to [`run_case`], read from `var`; `None` in the parent, where the
/// variable is unset.
pub fn requested_case(var: &str) -> Option<String> {
    std::env::var(var).ok()
}

/// In a child: report the case's facts to the parent, which receives them as
/// [`Outcome::Survived`]. Call it once, after the case has run.
///
/// # Panics
///
/// Panics if `facts` spans more than one line.
pub fn report(facts: &str) {
    assert!(!facts.contains('\n'), "a report is one line: {facts:?}");
    let mut out = std::io::stdout().lock();
    writeln!(out, "{REPORT_PREFIX}{facts}").expect("write the report");
    out.flush().expect("flush the report");
}

/// In the parent: run case `case` in a child copy of this test binary, whose
/// `child_test` (the full path of the child entry, `module::test`) reads it
/// from `var` with [`requested_case`]. How a case is spelled is the caller's
/// business: an index into its own list, plus whatever else the child needs.
///
/// # Panics
///
/// Panics if the child fails for any reason but a stack overflow, or exits
/// without a [`report`] (a `child_test` that names no test runs nothing and
/// exits cleanly, so a missing report is how a wrong path shows up).
pub fn run_case(child_test: &str, var: &str, case: &str) -> Outcome {
    let exe = std::env::current_exe().expect("locate the running test binary");
    let mut cmd = Command::new(exe);
    cmd.args([
        "--exact",
        child_test,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(var, case);
    let out = BoundedCommand::new(cmd)
        .timeout(CHILD_TIMEOUT)
        .run()
        .unwrap_or_else(|e| panic!("stack probe {child_test} case {case}: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains(OVERFLOW_MESSAGE) && !out.status.success() {
        return Outcome::Overflowed;
    }
    // libtest prints `test <name> ... ` without a newline before the test
    // runs, so the report may share its line.
    let report = stdout
        .lines()
        .find_map(|line| line.split_once(REPORT_PREFIX).map(|(_, facts)| facts));
    match report {
        Some(facts) if out.status.success() => Outcome::Survived(facts.to_string()),
        _ => panic!(
            "stack probe {child_test} case {case} neither reported nor overflowed \
             ({}):\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
            out.status
        ),
    }
}

/// Run `f` on a fresh thread with a `stack_bytes` stack, returning its result
/// or re-raising its panic.
pub fn on_stack<T: Send + 'static>(
    stack_bytes: usize,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let handle = std::thread::Builder::new()
        .name("stack probe".into())
        .stack_size(stack_bytes)
        .spawn(f)
        .expect("spawn a stack-probe thread");
    match handle.join() {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VAR: &str = "BORZOI_STACK_PROBE_SELF_TEST";
    const CHILD: &str = "stack_probe::tests::child";

    /// Recurse `depth` times through a frame that cannot be optimised away.
    fn recurse(depth: usize) -> usize {
        let frame = std::hint::black_box([depth as u8; 1024]);
        if depth == 0 {
            usize::from(frame[0])
        } else {
            recurse(depth - 1) + usize::from(std::hint::black_box(frame)[1])
        }
    }

    /// Child entry: recurse as deep as the case says on a 1 MiB stack.
    #[test]
    #[ignore = "child-process entry for the stack_probe self-test"]
    fn child() {
        let Some(case) = requested_case(VAR) else {
            return;
        };
        let depth: usize = case.parse().expect("a depth");
        let sum = on_stack(1 << 20, move || recurse(depth));
        report(&sum.to_string());
    }

    #[test]
    fn a_shallow_case_reports_and_a_deep_one_overflows() {
        // 1 + 2 + … + 10: each level adds its own depth.
        assert_eq!(run_case(CHILD, VAR, "10"), Outcome::Survived("55".into()));
        assert_eq!(run_case(CHILD, VAR, "100000"), Outcome::Overflowed);
    }

    #[test]
    #[should_panic(expected = "neither reported nor overflowed")]
    fn a_child_entry_that_names_no_test_is_a_failure() {
        run_case("stack_probe::tests::no_such_test", VAR, "10");
    }
}
