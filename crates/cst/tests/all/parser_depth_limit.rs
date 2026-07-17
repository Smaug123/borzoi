//! The parser is hand-written recursive descent with no stack-depth guard
//! prior to this. On pathologically nested input it would recurse until the
//! thread stack overflowed — which **aborts the process** (a stack overflow
//! does not unwind, so the LSP's `catch_unwind` parser wrapper cannot catch
//! it). Verified pre-guard with a standalone probe: nested `if`/parens/CE/tuple
//! at depth ~2000–8000 exited with SIGABRT (rc 134) on an 8 MiB stack.
//!
//! This suite locks in the bounded behaviour: past [`MAX_PARSE_DEPTH`] the
//! parser emits a single recovery error and stops descending, turning a process
//! abort into a characterised parse error. Every nesting *family* is covered,
//! because the recursion re-enters at several chokepoints — the expression main
//! cycle (`parse_minus_expr` + `parse_atomic_expr`), the prefix chains that
//! re-enter them (`-`/`&` via minus, `!` via atomic), the `:=`/`elif` tail
//! continuations, the type / measure recursions, and the pattern atom / `::`
//! climb — so each path is exercised here.
//!
//! Parses run on a thread sized like the LSP's 8 MiB main thread (see
//! [`probe_lsp_stack`]): below the threshold the parser caps recursion and
//! returns the one collapsed depth error, and reaching that cap is shown to be
//! safe on the deployment-sized stack. A broken or missing guard fails these
//! tests either by aborting (overflow, for the deep non-delimited recursions) or
//! by parsing `BREACH`-deep with no error (the error-count assertion).
//!
//! Scope note: this guards the *parser*'s recursive descent. The lex-filter
//! (`hw_token_fetch`) has its own, separate recursion on nested *delimiters*
//! that runs before the parser; bounding that is tracked separately.

use borzoi_cst::parser::parse;

/// The `Send`-able facts about a parse, extracted on the parse thread (a
/// `Parse` holds rowan nodes, which are not `Send`, so it can't cross the
/// thread boundary itself).
struct Probe {
    error_messages: Vec<String>,
    roundtrip_ok: bool,
}

/// Deep enough to trip the guard for every construct. Several chokepoints stack,
/// so the `MAX_PARSE_DEPTH` (512) *counter* is reached at a shallower *nesting*
/// depth — empirically ≤ ~850 across all constructs — so 1024 fires with margin.
/// Kept modest on purpose: the per-construct tests below run at this depth, and
/// a far-larger value (8000+) made the suite dominate `cargo test` for no extra
/// coverage (the guard caps recursion at 512 regardless). It is also well under
/// the depth at which the separately-tracked lex-filter delimiter recursion
/// overflows an 8 MiB stack (~6000), so these tests isolate the parser guard.
const BREACH: usize = 1024;

/// Comfortably below the guard's threshold, so a *valid* construct nested this
/// deep must parse with zero errors — the guard must not false-fire on
/// legitimately (if unusually) nested code.
const LEGAL: usize = 64;

/// Run `parse` on a thread whose stack matches the LSP's main thread (8 MiB —
/// `server.rs::run` processes messages there, no `thread::spawn`/tokio). The
/// Rust test harness otherwise gives each test only ~2 MiB. This both keeps the
/// parser off the small default test stack *and* validates directly that
/// reaching the depth cap is safe on the real deployment stack: if `MAX_PARSE_DEPTH`
/// were miscalibrated (too high for 8 MiB), these tests would abort here rather
/// than return.
fn probe_lsp_stack(src: String) -> Probe {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let parsed = parse(&src);
            Probe {
                roundtrip_ok: parsed.root.text() == src.as_str(),
                error_messages: parsed.errors.iter().map(|e| e.message.clone()).collect(),
            }
        })
        .expect("spawn parse thread")
        .join()
        .expect("parse thread overflowed its stack (depth guard missing/ineffective)")
}

/// Assert a breaching input produces exactly the one collapsed depth error and
/// still round-trips losslessly.
fn assert_breaches(src: String) {
    let probe = probe_lsp_stack(src);
    assert!(
        probe.roundtrip_ok,
        "depth-limit recovery must stay lossless"
    );
    assert_eq!(
        probe.error_messages.len(),
        1,
        "a breaching parse should collapse to one depth error, got {:?}",
        probe.error_messages
    );
    assert!(
        probe.error_messages[0].contains("too deep"),
        "expected the depth-limit message, got {:?}",
        probe.error_messages[0]
    );
}

/// Assert a legitimately-nested (below-threshold) input parses cleanly — the
/// guard must be invisible to real code.
fn assert_clean(src: String) {
    let probe = probe_lsp_stack(src);
    assert!(probe.roundtrip_ok, "must round-trip");
    assert!(
        probe.error_messages.is_empty(),
        "valid depth-{LEGAL} input must not trip the depth guard: {:?}",
        probe.error_messages
    );
}

// ---- expression families ------------------------------------------------

fn nested_parens(n: usize) -> String {
    format!("let x = {}0{}", "(".repeat(n), ")".repeat(n))
}

fn nested_if(n: usize) -> String {
    let mut s = String::from("let x = ");
    for _ in 0..n {
        s.push_str("if true then ");
    }
    s.push('0');
    for _ in 0..n {
        s.push_str(" else 0");
    }
    s
}

fn nested_ce(n: usize) -> String {
    let mut s = String::from("let x = ");
    for _ in 0..n {
        s.push_str("seq { ");
    }
    s.push_str("yield 0");
    for _ in 0..n {
        s.push_str(" }");
    }
    s
}

fn nested_tuple_parens(n: usize) -> String {
    format!("let x = {}0{}", "(0,".repeat(n), ")".repeat(n))
}

/// Right-associative `::` — recurses through `parse_pratt_expr(rbp)`.
fn cons_chain_expr(n: usize) -> String {
    let mut s = String::from("let x = ");
    s.push_str(&"0::".repeat(n));
    s.push_str("[]");
    s
}

/// Left-associative `+` — the Pratt loop handles this iteratively, so the stack
/// stays shallow and the guard must NOT fire however long the chain is.
fn flat_add_chain(n: usize) -> String {
    let mut s = String::from("let x = 0");
    s.push_str(&" + 0".repeat(n));
    s
}

#[test]
fn deep_parens_expr_breaches() {
    assert_breaches(nested_parens(BREACH));
}

#[test]
fn deep_if_expr_breaches() {
    assert_breaches(nested_if(BREACH));
}

#[test]
fn deep_computation_expr_breaches() {
    assert_breaches(nested_ce(BREACH));
}

#[test]
fn deep_tuple_expr_breaches() {
    assert_breaches(nested_tuple_parens(BREACH));
}

#[test]
fn deep_cons_expr_breaches() {
    assert_breaches(cons_chain_expr(BREACH));
}

#[test]
fn legal_depth_parens_expr_is_clean() {
    assert_clean(nested_parens(LEGAL));
}

#[test]
fn long_flat_add_chain_does_not_false_fire() {
    // A long left-assoc chain: iterative, so the stack stays shallow and the
    // guard must not fire however long it is.
    let probe = probe_lsp_stack(flat_add_chain(BREACH));
    assert!(
        probe.error_messages.iter().all(|m| !m.contains("too deep")),
        "left-assoc chain must not trip the depth guard: {:?}",
        probe.error_messages
    );
}

// ---- type families ------------------------------------------------------

fn nested_generic_type(n: usize) -> String {
    format!("type T = {}int{}", "list<".repeat(n), ">".repeat(n))
}

fn nested_fun_type(n: usize) -> String {
    let mut s = String::from("type T = ");
    s.push_str(&"int->".repeat(n));
    s.push_str("int");
    s
}

#[test]
fn deep_generic_type_breaches() {
    assert_breaches(nested_generic_type(BREACH));
}

#[test]
fn deep_fun_type_breaches() {
    assert_breaches(nested_fun_type(BREACH));
}

#[test]
fn legal_depth_generic_type_is_clean() {
    assert_clean(nested_generic_type(LEGAL));
}

// ---- pattern families ---------------------------------------------------

fn nested_paren_pat(n: usize) -> String {
    format!("let {}x{} = 0", "(".repeat(n), ")".repeat(n))
}

fn nested_tuple_pat(n: usize) -> String {
    format!("let {}a,b{} = 0", "(".repeat(n), ")".repeat(n))
}

/// Right-associative cons pattern at a `match` clause head — recurses through
/// `climb_pat_tail`.
fn cons_chain_pat(n: usize) -> String {
    let mut s = String::from("let f y = match y with ");
    s.push_str(&"a::".repeat(n));
    s.push_str("rest -> 0 | _ -> 1");
    s
}

#[test]
fn deep_paren_pat_breaches() {
    assert_breaches(nested_paren_pat(BREACH));
}

#[test]
fn deep_tuple_pat_breaches() {
    assert_breaches(nested_tuple_pat(BREACH));
}

#[test]
fn deep_cons_pat_breaches() {
    assert_breaches(cons_chain_pat(BREACH));
}

#[test]
fn legal_depth_paren_pat_is_clean() {
    assert_clean(nested_paren_pat(LEGAL));
}

// ---- secondary self-recursions -----------------------------------------
//
// These recurse *below* the main chokepoints (`parse_minus_expr`,
// `parse_atomic_type`'s `#`-branch, the `elif` tail, the measure `/` reciprocal)
// and so need their own guard on the recursive call — without it they bypass the
// depth counter and still overflow. (Generators space the operators so the lexer
// doesn't fold e.g. `----` into one `Op("----")` token.)

/// Prefix chain `- - - … 0` — `parse_minus_expr` operand self-recursion.
fn prefix_chain(n: usize) -> String {
    let mut s = String::from("let x = ");
    s.push_str(&"- ".repeat(n));
    s.push('0');
    s
}

/// `#####…int` — `parse_atomic_type`'s flexible-constraint self-recursion.
fn hash_type(n: usize) -> String {
    format!("type T = {}int", "#".repeat(n))
}

/// `if … then … elif … then … elif … else …` — the `elif`-tail self-recursion.
fn elif_chain(n: usize) -> String {
    let mut s = String::from("let x = if true then 0 ");
    for _ in 0..n {
        s.push_str("elif true then 0 ");
    }
    s.push_str("else 0");
    s
}

/// `1.0</ / / … m>` — `parse_measure_operand`'s leading-`/` reciprocal recursion.
fn measure_reciprocals(n: usize) -> String {
    format!("let x = 1.0<{}m>", "/ ".repeat(n))
}

#[test]
fn deep_prefix_chain_breaches() {
    assert_breaches(prefix_chain(BREACH));
}

#[test]
fn deep_hash_type_breaches() {
    assert_breaches(hash_type(BREACH));
}

#[test]
fn deep_elif_chain_breaches() {
    assert_breaches(elif_chain(BREACH));
}

#[test]
fn deep_measure_reciprocals_breaches() {
    assert_breaches(measure_reciprocals(BREACH));
}

// ---- main-cycle / tail-continuation paths that bypass `parse_pratt_expr` ----
//
// These were the recursion paths the first guard placement missed: prefixes
// that re-enter `parse_minus_expr` (`&`) or `parse_atomic_expr` (`!`) rather
// than the Pratt level, the right-associative `:=` tail continuation, and nested
// parenthesised types / measures. Guarding the minus + atomic chokepoints and
// the `:=` RHS / measure-paren recursions covers them.

/// Right-associative `:=` chain — recurses through `continue_assign_expr`'s RHS.
fn assign_chain(n: usize) -> String {
    let mut s = String::from("let x = ");
    s.push_str(&"a := ".repeat(n));
    s.push('0');
    s
}

/// `& & … x` address-of chain — `parse_address_of` re-enters `parse_minus_expr`.
fn address_of_chain(n: usize) -> String {
    let mut s = String::from("let x = ");
    s.push_str(&"& ".repeat(n));
    s.push('x');
    s
}

/// `! ! … x` deref chain — `parse_prefix_op_app` re-enters `parse_atomic_expr`.
fn bang_chain(n: usize) -> String {
    let mut s = String::from("let x = ");
    s.push_str(&"! ".repeat(n));
    s.push('x');
    s
}

/// `type T = ((((int))))` — parenthesised types, `parse_type` recursion.
fn paren_type(n: usize) -> String {
    format!("type T = {}int{}", "(".repeat(n), ")".repeat(n))
}

/// `1.0<((((m))))>` — parenthesised measures, `parse_measure_type_atom` cycle.
fn paren_measure(n: usize) -> String {
    format!("let x = 1.0<{}m{}>", "(".repeat(n), ")".repeat(n))
}

#[test]
fn deep_assign_chain_breaches() {
    assert_breaches(assign_chain(BREACH));
}

#[test]
fn deep_address_of_chain_breaches() {
    assert_breaches(address_of_chain(BREACH));
}

#[test]
fn deep_bang_chain_breaches() {
    assert_breaches(bang_chain(BREACH));
}

#[test]
fn deep_paren_type_breaches() {
    assert_breaches(paren_type(BREACH));
}

#[test]
fn deep_paren_measure_breaches() {
    assert_breaches(paren_measure(BREACH));
}
