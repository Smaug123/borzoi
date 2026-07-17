//! Differential tests: our `Condition` evaluator (`condition.rs`) vs the real
//! MSBuild evaluator via `tools/msbuild-condition-oracle`.
//!
//! The asserted property is *certain-implies-exact*, not equality: our
//! [`Outcome::Unsupported`](borzoi_msbuild::test_support::Outcome) is a
//! deliberate fail-safe superset (it fires both when MSBuild would *reject* a
//! condition as illegal and when the condition is legal-but-uses-grammar we
//! don't model). So the sound contract is one-directional — whenever we commit
//! to a boolean, MSBuild must agree with that exact boolean; when we say
//! `Unsupported` we make no claim. See
//! [`check_certain_implies_exact`](common::check_certain_implies_exact).
//!
//! Inputs here are deterministic (fixed-seed SplitMix64 plus a hand-written
//! corner list) so a failure reproduces exactly and the oracle batch stays
//! stable run-to-run; the *random-input* exploration lives in
//! `condition_properties.rs`.
//!
//! Each case spawns nothing per-eval — the oracle evaluates conditions
//! in-process against the SDK's MSBuild — but the harness does `dotnet build`
//! the oracle once, so this runs under `nix develop` like the sibling
//! `dotnet`-driven differentials.

mod common;

use common::{Oracle, SplitMix64, Verdict, check_certain_implies_exact, gen_bool_expr, gen_props};

fn props(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// Hand-picked corners: each pins both our evaluator's committed outcome and
/// (via the internal oracle round-trip) MSBuild's agreement. Every shape the
/// `condition.rs` unit tests pin one-by-one gets a guaranteed seat here,
/// regardless of what the generator happens to produce.
#[test]
fn hand_picked_corners() {
    let mut oracle = Oracle::spawn();

    // (condition, props, the outcome branch we must land in)
    type Case = (&'static str, Vec<(String, String)>, Verdict);
    let cases: &[Case] = &[
        // Bare booleans and negation.
        ("true", props(&[]), Verdict::True),
        ("false", props(&[]), Verdict::False),
        ("!false", props(&[]), Verdict::True),
        ("!true", props(&[]), Verdict::False),
        // String equality (ordinal, case-insensitive).
        ("'a' == 'a'", props(&[]), Verdict::True),
        ("'a' == 'b'", props(&[]), Verdict::False),
        ("'ABC' == 'abc'", props(&[]), Verdict::True),
        ("'a' != 'b'", props(&[]), Verdict::True),
        // MSBuild boolean vocabulary: 'on'/'yes'/'true' are all mutually equal.
        ("'on' == 'yes'", props(&[]), Verdict::True),
        ("'off' == 'no'", props(&[]), Verdict::True),
        ("'on' == 'off'", props(&[]), Verdict::False),
        // Numeric (double) comparison.
        ("1 < 2", props(&[]), Verdict::True),
        ("2 <= 2", props(&[]), Verdict::True),
        ("3 > 10", props(&[]), Verdict::False),
        ("3.14 > 3", props(&[]), Verdict::True),
        // Hex is a 32-bit reinterpretation.
        ("0x10 == 16", props(&[]), Verdict::True),
        // Version vs number: major-only, version wins ties.
        ("6.0.0.0 > 6", props(&[]), Verdict::True),
        ("1.2.3 < 1.3", props(&[]), Verdict::True),
        // Exponent form is NOT a double, so this is string comparison.
        ("'1e2' == '100'", props(&[]), Verdict::False),
        // Property substitution.
        (
            "'$(Configuration)' == 'Debug'",
            props(&[("Configuration", "Debug")]),
            Verdict::True,
        ),
        (
            "'$(Configuration)' != 'Debug'",
            props(&[("Configuration", "Release")]),
            Verdict::True,
        ),
        // The is-it-set idiom: undefined property expands to empty.
        ("'$(Undefined)' == ''", props(&[]), Verdict::True),
        ("'$(Undefined)' != ''", props(&[]), Verdict::False),
        // The oracle's internal result lives in the item namespace, so a
        // condition referencing its name (or `R`, its former property name) as
        // a *property* sees nothing — these must match our empty expansion.
        ("'$(_ConditionResult)' == ''", props(&[]), Verdict::True),
        ("'$(R)' == ''", props(&[]), Verdict::True),
        ("'$(R)' == 'x'", props(&[("R", "x")]), Verdict::True),
        // Logical combinators and precedence.
        ("1 == 1 And 2 == 2", props(&[]), Verdict::True),
        ("1 == 1 And 2 == 3", props(&[]), Verdict::False),
        ("1 == 2 Or 3 == 3", props(&[]), Verdict::True),
        (
            "(1 == 2 Or 3 == 3) And 'a' == 'a'",
            props(&[]),
            Verdict::True,
        ),
        // Standalone scalar coerced to bool: a bare property reference,
        // bare boolean-vocabulary word, and quoted literal.
        ("$(Flag)", props(&[("Flag", "true")]), Verdict::True),
        ("$(Flag)", props(&[("Flag", "false")]), Verdict::False),
        // Whitespace around a *simple* reference is MSBuild-illegal (only the
        // function forms tolerate it), so we must not commit to Flag's value.
        (
            "$( Flag )",
            props(&[("Flag", "true")]),
            Verdict::Unsupported,
        ),
        ("!$(Flag)", props(&[("Flag", "false")]), Verdict::True),
        ("on", props(&[]), Verdict::True),
        ("no", props(&[]), Verdict::False),
        ("!off", props(&[]), Verdict::True),
        ("'true'", props(&[]), Verdict::True),
        // Bare words are unquoted simple-string operands.
        ("Release == 'Release'", props(&[]), Verdict::True),
        ("on == 'x'", props(&[]), Verdict::False),
        ("$(P) == 'y'", props(&[("P", "y")]), Verdict::True),
        // true/false as unquoted comparison operands (SDK container shape).
        ("$(B) == false", props(&[("B", "False")]), Verdict::True),
        ("false == $(B)", props(&[("B", "False")]), Verdict::True),
        // String instance methods: capital True/False, ordinal & case-sensitive.
        (
            "$(V.Contains('{'))",
            props(&[("V", "8.0.0")]),
            Verdict::False,
        ),
        (
            "$(V.StartsWith('8'))",
            props(&[("V", "8.0.0")]),
            Verdict::True,
        ),
        // A raw item/metadata opener anywhere in the condition text is
        // MSBuild-illegal (its scanner lexes `@(`/`%(` before expansion), so
        // the up-front raw-source scan refuses regardless of where it sits.
        (
            "$(V.Contains('@('))",
            props(&[("V", "x")]),
            Verdict::Unsupported,
        ),
        (
            "HasTrailingSlash('@(x)/')",
            props(&[]),
            Verdict::Unsupported,
        ),
        ("'%(Identity)' == 'x'", props(&[]), Verdict::Unsupported),
        // A `%XX` escape (percent + two hex digits) is *unescaped* by MSBuild at
        // the operand leaf — `'%74rue' == 'true'` is true — and we model that
        // now (stage E2 of `docs/msbuild-escaped-value-plan.md`). These used to
        // degrade the whole condition: fail-safe, but it cost every gate an
        // escape appeared in. The oracle round-trip is what makes committing them
        // safe rather than brave.
        ("'%74rue' == 'true'", props(&[]), Verdict::True),
        ("'a%20b' == 'a b'", props(&[]), Verdict::True),
        // …including an escape reaching the operand through a property value.
        ("'$(P0)' == 'a b'", props(&[("P0", "a%20b")]), Verdict::True),
        // A `%` **outside** quotes is not an escape: MSBuild's scanner rejects it
        // outright (MSB4090), which maps to Unsupported. Committing a boolean for
        // a condition MSBuild refuses to evaluate would be a wrong gate.
        ("%74rue", props(&[]), Verdict::Unsupported),
        ("1%2E0 > 0.5", props(&[]), Verdict::Unsupported),
        // A bare `%` with any other suffix stays literal: committed as
        // ordinary string comparison, and MSBuild must agree.
        ("'100%' == '100%'", props(&[]), Verdict::True),
        ("'a%zz' == 'true'", props(&[]), Verdict::False),
        // But a marker delivered only via `$()` substitution is never
        // re-lexed by MSBuild, so it stays an ordinary substring and we must
        // still commit to the exact boolean MSBuild computes.
        (
            "$(V.Contains('$(N)'))",
            props(&[("V", "a@(b"), ("N", "@(")]),
            Verdict::True,
        ),
        (
            "$(V.Contains('-')) == false",
            props(&[("V", "8.0.100")]),
            Verdict::True,
        ),
        // HasTrailingSlash built-in (pure; trims the expanded argument).
        ("HasTrailingSlash('bin/Debug/')", props(&[]), Verdict::True),
        ("HasTrailingSlash('bin/Debug')", props(&[]), Verdict::False),
        (
            "!HasTrailingSlash('$(OutDir)')",
            props(&[("OutDir", "obj")]),
            Verdict::True,
        ),
        // The F# SDK's FSharpCoreMaximumMajorVersion gate, end-to-end.
        (
            "'$(S)' == 'true' and '$(V)' != '' and !$(V.Contains('{'))",
            props(&[("S", "true"), ("V", "8.0.0")]),
            Verdict::True,
        ),
    ];

    let mut trues = 0;
    let mut falses = 0;
    for (condition, props, expected) in cases {
        let got = check_certain_implies_exact(&mut oracle, condition, props);
        assert_eq!(
            got, *expected,
            "our evaluator landed in {got:?}, expected {expected:?} for {condition:?}"
        );
        match got {
            Verdict::True => trues += 1,
            Verdict::False => falses += 1,
            Verdict::Unsupported => {}
        }
    }
    // Sanity: the corner list actually exercises both committed branches
    // (guards against the whole evaluator regressing to one constant).
    assert!(
        trues >= 5 && falses >= 5,
        "corners: {trues} true, {falses} false"
    );
}

/// Fixed-seed generative sweep. Every generated `(condition, props)` case must
/// satisfy certain-implies-exact; a coverage floor guards against the corpus
/// silently degrading to all-`Unsupported` (which would make the whole sweep
/// vacuously pass while testing nothing).
#[test]
fn fixed_seed_sweep() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0x5eed_c0ffee);

    const CASES: usize = 4000;
    let mut trues = 0usize;
    let mut falses = 0usize;
    let mut unsupported = 0usize;

    for _ in 0..CASES {
        let depth = rng.below(3) as u32; // 0..=2 nesting levels.
        let condition = gen_bool_expr(&mut rng, depth);
        let props = gen_props(&mut rng);
        match check_certain_implies_exact(&mut oracle, &condition, &props) {
            Verdict::True => trues += 1,
            Verdict::False => falses += 1,
            Verdict::Unsupported => unsupported += 1,
        }
    }

    eprintln!("sweep: {trues} true, {falses} false, {unsupported} unsupported / {CASES}");
    // Anti-vacuity floors: a healthy corpus commits to *both* booleans a few
    // hundred times each (observed ~340/~390 at this seed). Far below the
    // split, so a generator tweak won't trip them, but a collapse of the
    // evaluator to a constant — or to all-`Unsupported` — would.
    assert!(trues >= 250, "too few committed-true cases: {trues}");
    assert!(falses >= 250, "too few committed-false cases: {falses}");
}
