//! Proptest fuzzer for the `Condition` evaluator against the real MSBuild
//! evaluator (`tools/msbuild-condition-oracle`).
//!
//! Same *certain-implies-exact* contract as `condition_diff.rs` (see its module
//! docs and [`common::check_certain_implies_exact`]): whenever our evaluator
//! commits to a boolean, MSBuild must agree with that exact boolean; an
//! `Unsupported` makes no claim. The deterministic fixed-seed sweep lives next
//! door; this file adds proptest's *shrinking* — when a divergence exists,
//! proptest reduces it to a minimal `(condition, props)` witness, which is far
//! more useful for diagnosis than whatever 60-character soup first tripped it.
//!
//! One oracle child is shared across all cases (it is a pure, deterministic
//! function of `(condition, props)`, so reuse is safe and amortises the
//! `dotnet build` + process spawn). Runs under `nix develop` like the sibling
//! differential, since the harness builds the oracle on first use.

mod common;

use std::sync::{Mutex, OnceLock};

use common::{CONTROLLED_PROPERTY_NAMES, Oracle, check_certain_implies_exact};
use proptest::prelude::*;

/// The single shared oracle child. `OnceLock` builds/spawns it once; the
/// `Mutex` serialises the lock-step request/response protocol across proptest
/// cases (which run on one thread anyway, but the guard makes that explicit
/// and keeps the borrow scoped).
fn oracle() -> &'static Mutex<Oracle> {
    static ORACLE: OnceLock<Mutex<Oracle>> = OnceLock::new();
    ORACLE.get_or_init(|| Mutex::new(Oracle::spawn()))
}

/// Scalar operands spanning MSBuild's numeric / version / boolean / string
/// corners — the same zoo the fixed-seed generator draws from.
fn operand() -> impl Strategy<Value = String> {
    const OPERANDS: &[&str] = &[
        // Numerics: decimal, signed, dot-led, hex (valid + invalid), overflow.
        "0",
        "1",
        "42",
        "-1",
        "+2",
        "3.14",
        "+2.5",
        "-0.5",
        ".5",
        "1.",
        "0x10",
        "0xFF",
        "0xg",
        "2147483647",
        "2147483648",
        "1e2",
        "100",
        // Version-shaped dotted numbers.
        "6",
        "6.0",
        "6.0.0.0",
        "1.2",
        "1.2.3",
        "1.2.3.4",
        "10.0",
        "01",
        // Boolean vocabulary (bare).
        "true",
        "false",
        "True",
        "FALSE",
        "on",
        "off",
        "yes",
        "no",
        // Quoted strings, some empty / whitespace / holding `$(…)`.
        "'abc'",
        "''",
        "'On'",
        "'yes'",
        "'6.0'",
        "' 2.5 '",
        "'net8.0'",
        "'1e2'",
        "'100'",
        "'$(P0)'",
        "'$(Foo)'",
        "'$(Undefined)'",
        "'x$(P1)y'",
        // Bare property references.
        "$(P0)",
        "$(P1)",
        "$(Foo)",
        "$(Bar)",
        "$(Undefined)",
    ];
    proptest::sample::select(OPERANDS).prop_map(str::to_string)
}

/// A single comparison `lhs OP rhs`, any of the six operators.
fn comparison() -> impl Strategy<Value = String> {
    let op = proptest::sample::select(&["==", "!=", "<", "<=", ">", ">="][..]);
    (operand(), op, operand()).prop_map(|(l, op, r)| format!("{l} {op} {r}"))
}

/// A bare boolean literal usable as a whole condition (`Condition="on"`).
fn bool_literal() -> impl Strategy<Value = String> {
    proptest::sample::select(&["true", "false", "True", "FALSE", "on", "off", "yes", "no"][..])
        .prop_map(str::to_string)
}

/// Arbitrary condition strings: comparisons and bool literals combined with
/// `!`, `And`, `Or`, and parentheses to a bounded depth.
fn condition() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![3 => comparison(), 1 => bool_literal()];
    leaf.prop_recursive(3, 24, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|e| format!("!({e})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a} And {b}")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a} Or {b}")),
            inner.prop_map(|e| format!("({e})")),
        ]
    })
}

/// A property map over a unique subset of [`CONTROLLED_PROPERTY_NAMES`] with
/// values from a small pool overlapping the operand corners. Names are unique
/// (a JSON object can't carry duplicate keys, and our `PropertyMap` is
/// last-write-wins) so the two sides always see identical property state.
fn props() -> impl Strategy<Value = Vec<(String, String)>> {
    const VALUES: &[&str] = &[
        "Debug", "Release", "net8.0", "6.0", "6.0.0.0", "1", "0", "true", "false", "on", "", "x",
        "100",
    ];
    let names = proptest::sample::subsequence(
        CONTROLLED_PROPERTY_NAMES.to_vec(),
        0..=CONTROLLED_PROPERTY_NAMES.len(),
    );
    names.prop_flat_map(|names| {
        let n = names.len();
        (
            Just(names),
            proptest::collection::vec(proptest::sample::select(VALUES), n),
        )
            .prop_map(|(names, values)| {
                names
                    .into_iter()
                    .map(str::to_string)
                    .zip(values.into_iter().map(str::to_string))
                    .collect()
            })
    })
}

proptest! {
    // No `proptest-regressions/` anchor exists for a `tests/`-only binary, and
    // the sibling fixed-seed sweep is the durable regression net; keep failures
    // in-run only (matches the assembly crate's `fail_loud` convention). More
    // cases than the 256 default: each is a sub-millisecond in-process oracle
    // round-trip, so the exploration is nearly free.
    #![proptest_config(ProptestConfig { cases: 1024, failure_persistence: None, ..ProptestConfig::default() })]

    /// Every fuzzed `(condition, props)` must satisfy certain-implies-exact.
    /// A soundness violation panics inside the checker; proptest shrinks it to
    /// a minimal witness.
    #[test]
    fn certain_implies_exact(condition in condition(), props in props()) {
        // Recover a poisoned guard rather than `expect`-ing: the *first*
        // violating case panics while holding this guard, poisoning the mutex.
        // The panic fires only in the assertion *after* the oracle round-trip
        // has fully completed, so the `Oracle`'s stdin/stdout are in a clean
        // request/response boundary and the child is reusable. Re-`expect`ing
        // would instead panic-on-poison on every shrink candidate, before it
        // reached the oracle — defeating the shrinking this file exists for.
        let mut guard = oracle().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        check_certain_implies_exact(&mut guard, &condition, &props);
    }
}
