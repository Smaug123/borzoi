//! The lex-filter's offside `undentation_limit` was tail-recursive *down the
//! offside stack*. Since each delimiter / offside opener pushes ~2 contexts, a
//! deeply nested `((((…))))` (or `seq { seq { … } }`, `fun a -> fun a -> …`, …)
//! overflowed the stack **before the parser ran** — an uncatchable process
//! abort, distinct from (and not protected by) the parser's recursion-depth
//! guard. Converting `undentation_limit` to a loop removes that recursion with
//! no behaviour change.
//!
//! These tests drive the lex-filter directly (no parser) on a deliberately
//! small 1 MiB stack: pre-fix this recursion overflowed ~875 deep on 1 MiB
//! (≈7000 on the LSP's real 8 MiB main thread); with the loop it is effectively
//! iterative, so even a tiny stack handles thousands of levels. Reaching the
//! assertions at all is the proof — a regression to the recursive form would
//! overflow and abort the test binary here.

use borzoi_cst::lexer::lex;
use borzoi_cst::lexfilter::filter;

/// Comfortably past the ~875-deep 1 MiB overflow threshold of the old recursive
/// form, but cheap to lex (the offside walk is O(1)-amortised per push via the
/// `undentation_skip` cache; see `extreme_nesting_lexes_in_linear_time`).
const DEEP: usize = 6000;

/// Count the filtered tokens for `src` on a 1 MiB stack. With the iterative
/// `undentation_limit` this completes for any depth; the recursive form would
/// overflow (abort) well before `DEEP`.
fn lexfilter_token_count(src: String) -> usize {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(move || filter(&src, lex(&src)).count())
        .expect("spawn lex-filter thread")
        .join()
        .expect("lex-filter overflowed its stack (undentation_limit not iterative?)")
}

fn wrapped(pre: &str, post: &str, n: usize) -> String {
    format!("let x = {}0{}", pre.repeat(n), post.repeat(n))
}

#[test]
fn deep_parens_do_not_overflow_lexfilter() {
    assert!(lexfilter_token_count(wrapped("(", ")", DEEP)) > 0);
}

#[test]
fn deep_brackets_do_not_overflow_lexfilter() {
    assert!(lexfilter_token_count(wrapped("[", "]", DEEP)) > 0);
}

#[test]
fn deep_arrays_do_not_overflow_lexfilter() {
    assert!(lexfilter_token_count(wrapped("[| ", " |]", DEEP)) > 0);
}

#[test]
fn deep_computation_exprs_do_not_overflow_lexfilter() {
    let mut s = String::from("let x = ");
    for _ in 0..DEEP {
        s.push_str("seq { ");
    }
    s.push_str("yield 0");
    for _ in 0..DEEP {
        s.push_str(" }");
    }
    assert!(lexfilter_token_count(s) > 0);
}

#[test]
fn deep_lambdas_do_not_overflow_lexfilter() {
    let mut s = String::from("let x = ");
    for _ in 0..DEEP {
        s.push_str("fun a -> ");
    }
    s.push('0');
    assert!(lexfilter_token_count(s) > 0);
}

#[test]
fn deep_if_then_else_do_not_overflow_lexfilter() {
    let mut s = String::from("let x = ");
    for _ in 0..DEEP {
        s.push_str("if true then ");
    }
    s.push('0');
    for _ in 0..DEEP {
        s.push_str(" else 0");
    }
    assert!(lexfilter_token_count(s) > 0);
}

/// Regression guard for the offside-walk complexity. Before the
/// `undentation_skip` cache, `undentation_limit` re-walked the offside stack on
/// every push (O(depth) per push → O(n²) over the file); with the cache it is
/// O(1)-amortised, so the filter is linear. At this depth a quadratic walk does
/// ~10^9 steps and takes many seconds (a debug build, minutes — caught by the
/// CI timeout), while the linear walk finishes in well under a second. We don't
/// assert a wall-clock bound (flaky), but completing quickly *is* the signal;
/// the exact equivalence to the stepwise walk is covered by the
/// `undentation_cache_matches_reference` property test.
#[test]
fn extreme_nesting_lexes_in_linear_time() {
    // 60k-deep — far past anything the O(n²) walk could chew through promptly,
    // trivial for the O(1)-amortised one. Run on the default test stack: the
    // lex-filter is iterative, so depth costs no call stack.
    let n = 60_000;
    assert!(filter(&wrapped("(", ")", n), lex(&wrapped("(", ")", n))).count() > 0);
}
