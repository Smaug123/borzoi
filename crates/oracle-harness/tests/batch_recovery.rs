//! Recovery behaviour of [`BatchChild`].
//!
//! These tests never touch a real oracle: they drive `BatchChild` over fake `sh`
//! "oracles" to pin the timeout + respawn-retry logic added after a real
//! `cargo test` run hung for ~5h on a *wedged* oracle — a child that stayed
//! alive, burned ~no CPU, and never answered (an internal FCS deadlock). A bare
//! `read_line` has no bound, so the whole suite hung; this code must instead
//! recover, or fail loudly, within a bounded time.
//!
//! (Ported from `crates/cst/tests/fcs_batch_recovery.rs` when the harness moved
//! here to be shared with the msbuild and nuget oracles, which had each
//! re-derived the unbounded version.)
#![cfg(unix)]

use std::process::Command;
use std::time::Duration;

use borzoi_oracle_harness::BatchChild;
use tempfile::tempdir;

/// A healthy fake oracle: echoes `ok:<request>` for each input line, exits on EOF.
fn responder() -> Command {
    let mut c = Command::new("sh");
    c.arg("-c")
        .arg(r#"while IFS= read -r line; do printf 'ok:%s\n' "$line"; done"#);
    c
}

/// The happy path: one request, one response line.
#[test]
fn batch_happy_path_returns_response() {
    let mut child = BatchChild::with_factory(
        Box::new(responder),
        "fake-oracle",
        Duration::from_secs(5),
        2,
    );
    assert_eq!(child.request("/tmp/foo.fs").trim_end(), "ok:/tmp/foo.fs");
}

/// Requests are answered *positionally*, so a child driven repeatedly must keep
/// pairing each response with its own request — a late or dropped line would show
/// up here as a shifted answer.
#[test]
fn batch_pairs_each_response_with_its_request() {
    let mut child = BatchChild::with_factory(
        Box::new(responder),
        "fake-oracle",
        Duration::from_secs(5),
        2,
    );
    for i in 0..20 {
        let req = format!("req-{i}");
        assert_eq!(child.request(&req).trim_end(), format!("ok:{req}"));
    }
}

/// A *transient* wedge recovers: the first spawn wedges, the request times out,
/// the child is respawned, and the second (healthy) child answers — exactly the
/// flake observed in the wild, which passed on a fresh process.
#[test]
fn batch_recovers_from_a_transient_wedge() {
    // The marker file makes only the *first* spawn wedge; the second responds.
    let dir = tempdir().unwrap();
    let marker = dir.path().join("spawned-once");
    let marker_s = marker.to_str().unwrap().to_string();
    let factory = move || {
        let mut c = Command::new("sh");
        c.arg("-c")
            .arg(
                r#"if [ -e "$M" ]; then
                       while IFS= read -r line; do printf 'ok:%s\n' "$line"; done
                   else
                       : > "$M"; IFS= read -r l; IFS= read -r x
                   fi"#,
            )
            .env("M", &marker_s);
        c
    };
    let mut child = BatchChild::with_factory(
        Box::new(factory),
        "fake-oracle",
        Duration::from_millis(300),
        2,
    );
    assert_eq!(child.request("/tmp/bar.fs").trim_end(), "ok:/tmp/bar.fs");
}

/// A crash (stdout closed without answering) is also recovered from: the first
/// spawn exits immediately, the second responds.
#[test]
fn batch_recovers_from_a_crash() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("crashed-once");
    let marker_s = marker.to_str().unwrap().to_string();
    let factory = move || {
        let mut c = Command::new("sh");
        c.arg("-c")
            .arg(
                // First spawn exits 1 immediately (closes stdout → crash);
                // second spawn is a healthy responder.
                r#"if [ -e "$M" ]; then
                       while IFS= read -r line; do printf 'ok:%s\n' "$line"; done
                   else
                       : > "$M"; exit 1
                   fi"#,
            )
            .env("M", &marker_s);
        c
    };
    let mut child =
        BatchChild::with_factory(Box::new(factory), "fake-oracle", Duration::from_secs(5), 2);
    assert_eq!(child.request("/tmp/qux.fs").trim_end(), "ok:/tmp/qux.fs");
}

/// The *write* has to be bounded too, not just the read.
///
/// A request larger than the pipe buffer (~64 KiB) against an oracle that has
/// stopped reading blocks in the write itself — before the response deadline ever
/// starts ticking. The retry machinery sits downstream of that, so it never runs:
/// the suite hangs with all the recovery logic intact and unreached. Requests are
/// arbitrary strings (the msbuild oracle sends JSON carrying whole property sets),
/// so "requests are always small" is not an invariant worth betting the guarantee
/// on.
#[test]
#[should_panic(expected = "unreading-oracle")]
fn batch_bounds_a_write_to_an_oracle_that_never_reads() {
    let factory = || {
        let mut c = Command::new("sh");
        // Never reads stdin, never answers, stays alive: its stdin pipe fills and
        // stays full.
        c.arg("-c").arg("sleep 600");
        c
    };
    let mut child = BatchChild::with_factory(
        Box::new(factory),
        "unreading-oracle",
        Duration::from_millis(200),
        2,
    );
    child.request(&"x".repeat(1 << 20));
}

/// A *permanent* wedge is not survivable — but it must fail loudly rather than
/// hang. After exhausting its attempts, `request` panics naming the oracle.
///
/// (Asserting on the *wall-clock* bound here would be flaky under a saturated
/// machine, and was removed once before for that reason; the bound is covered in
/// `bounded.rs`, where the timeout is the only thing being timed.)
#[test]
#[should_panic(expected = "wedging-oracle")]
fn batch_gives_up_loudly_on_a_permanent_wedge() {
    let factory = || {
        let mut c = Command::new("sh");
        // Reads nothing, answers nothing, stays alive: the wedge.
        c.arg("-c").arg("sleep 600");
        c
    };
    let mut child = BatchChild::with_factory(
        Box::new(factory),
        "wedging-oracle",
        Duration::from_millis(200),
        2,
    );
    child.request("/tmp/never.fs");
}
