//! Differential tests: our `$(…)` property-value expansion
//! ([`substitute`](borzoi_msbuild::test_support::substitute)) vs the real
//! MSBuild evaluator via the `tools/msbuild-condition-oracle` `expand` op.
//!
//! The asserted property is *certain-implies-exact*, the expansion analogue of
//! the condition differential: whenever our `substitute` reports **zero
//! issues** it has committed to the expanded string, and MSBuild must produce
//! that byte-identical string (and not error). Any
//! [`Issue`](borzoi_msbuild::test_support::Issue) — an undefined reference
//! or an unsupported expression — withdraws the claim; partiality is the
//! fail-safe superset over both MSBuild errors and legal-but-unmodelled shapes.
//! See [`check_expand_certain_implies_exact`](common::check_expand_certain_implies_exact).
//!
//! This is Stage 1 of `docs/completed/property-expression-plan.md`: the harness runs
//! against *today's* string-prefix evaluator. Every D2 row from that plan gets
//! a corner here, tagged with the verdict today's evaluator lands in —
//! `Exact` for shapes it already reduces, `Partial` for the ones the general
//! parser (Stages 2–3) will light up. Those `Partial` rows flip to `Exact` as
//! their evaluators land; a reviewer sees exactly which corner each stage
//! turns on.
//!
//! Inputs are deterministic (fixed-seed SplitMix64 plus the hand corner list),
//! and the oracle evaluates in-process, so a failure reproduces exactly.

mod common;

use common::{
    ExpandVerdict, Oracle, SplitMix64, check_expand_certain_implies_exact, gen_expand_props,
    gen_expand_value, gen_grammar_value,
};

fn props(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// Hand-picked corners covering `docs/completed/property-expression-plan.md` §D2. Each
/// pins the verdict today's evaluator commits to; the internal oracle
/// round-trip pins MSBuild's agreement on the `Exact` ones.
#[test]
fn hand_picked_corners() {
    let mut oracle = Oracle::spawn();

    // `[MSBuild]::EnsureTrailingSlash` is pinned on unix hosts only; the
    // evaluator declines on Windows (separator semantics unverified against the
    // oracle), landing those corners in `Partial` there.
    let ets = if cfg!(windows) {
        ExpandVerdict::Partial
    } else {
        ExpandVerdict::Exact
    };
    // Same unix-only pin for `[System.IO.Path]::IsPathRooted`.
    let ipr = ets;
    // `[MSBuild]::IsOSPlatform` declines on hosts outside the verified
    // mapping (macOS/linux/windows/freebsd); every CI/dev host is in it.
    let osp = ExpandVerdict::Exact;

    type Case = (&'static str, Vec<(String, String)>, ExpandVerdict);
    let cases: &[Case] = &[
        // --- Plain literals and property references (supported today) --------
        ("hello world", props(&[]), ExpandVerdict::Exact),
        ("x$(Foo)y", props(&[("Foo", "Z")]), ExpandVerdict::Exact),
        (
            "$(Foo)/$(Bar)",
            props(&[("Foo", "a"), ("Bar", "b")]),
            ExpandVerdict::Exact,
        ),
        // An undefined reference expands to "" on both sides — but the `Undefined`
        // issue withdraws our claim, so it is `Partial`, not `Exact`.
        ("$(Undefined)", props(&[]), ExpandVerdict::Partial),
        ("x$(Undefined)y", props(&[]), ExpandVerdict::Partial),
        // --- Scanning: quoted parens don't close the expression (D2) ---------
        // `find_balanced_close` is quote-aware today; these already commit.
        (
            "$(Foo.Contains(')'))",
            props(&[("Foo", "ab)c")]),
            ExpandVerdict::Exact,
        ),
        (
            "$(Foo.Contains(')'))",
            props(&[("Foo", "abc")]),
            ExpandVerdict::Exact,
        ),
        (
            "$(Foo.Contains('('))",
            props(&[("Foo", "a(b")]),
            ExpandVerdict::Exact,
        ),
        // A marker delivered only via a nested `$()` in the argument is an
        // ordinary substring — extent + recursion into the arg both exercised.
        (
            "$(Foo.Contains('$(N)'))",
            props(&[("Foo", "a-b"), ("N", "-")]),
            ExpandVerdict::Exact,
        ),
        // --- String bool methods (supported today) ---------------------------
        (
            "$(Foo.StartsWith('a'))",
            props(&[("Foo", "abc")]),
            ExpandVerdict::Exact,
        ),
        // A literal `$(` inside a quoted argument is not a (failed) reference —
        // the call still evaluates. Regression for the scanner over-abort.
        (
            "$(Foo.Contains('$('))",
            props(&[("Foo", "a$(b")]),
            ExpandVerdict::Exact,
        ),
        // --- `[MSBuild]::Version*` comparison family (supported today) -------
        // Missing components read as 0 (`1.0` == `1.0.0`), leading `v` stripped,
        // and both literal and `$(…)`-expanded / unquoted-literal args commit.
        // Malformed args (empty/non-numeric/>4 components) error in MSBuild, so
        // they are covered by the decline unit test, not here.
        (
            "$([MSBuild]::VersionEquals('1.0','1.0.0'))",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        (
            "$([MSBuild]::VersionEquals('v1.2','1.2'))",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        (
            "$([MSBuild]::VersionGreaterThanOrEquals('$(V)','10.0'))",
            props(&[("V", "10.0.1")]),
            ExpandVerdict::Exact,
        ),
        (
            "$([MSBuild]::VersionLessThan('9.0','10.0'))",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        // Prerelease/metadata suffix dropped at the first `-`/`+`.
        (
            "$([MSBuild]::VersionEquals('10.0.100-preview.1','10.0.100'))",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        // A nested property function/member in a needle stays Unsupported —
        // MSBuild's handling is byzantine (these error). Covers inner-quote and
        // no-inner-quote (static function) forms. A bare `$(N)` ref still works.
        (
            "$(Foo.Contains('$(Bar.Contains('a'))'))",
            props(&[("Foo", "xTruey"), ("Bar", "a")]),
            ExpandVerdict::Partial,
        ),
        (
            "$(Foo.TrimStart('x').Contains('$([MSBuild]::IsRunningFromVisualStudio())'))",
            props(&[("Foo", "xabc")]),
            ExpandVerdict::Partial,
        ),
        (
            "$(Foo.Contains('$(Bar)'))",
            props(&[("Foo", "xyesz"), ("Bar", "yes")]),
            ExpandVerdict::Exact,
        ),
        // Same bare-ref rule for path-function args: a nested function is
        // declined (MSBuild rejects it); a bare `$(Dir)` ref still combines.
        (
            "$([System.IO.Path]::Combine('$([MSBuild]::IsRunningFromVisualStudio())','b'))",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        (
            "$([System.IO.Path]::Combine('$(Dir)','b'))",
            props(&[("Dir", "/a")]),
            ExpandVerdict::Exact,
        ),
        // `IsRunningFromVisualStudio()` is a bool → renders `False`; a chained
        // string member is a type error MSBuild rejects, so we must not commit.
        (
            "$([MSBuild]::IsRunningFromVisualStudio())",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        (
            "$([MSBuild]::IsRunningFromVisualStudio().Contains('f'))",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        // `( )` is one whitespace arg → a zero-arg intrinsic rejects it.
        (
            "$([MSBuild]::IsRunningFromVisualStudio( ))",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        // Empty `TrimStart('')` trims whitespace in .NET; we decline (never
        // commit to the no-op that would diverge on a whitespace-led value).
        (
            "$(Ws.TrimStart(''))",
            props(&[("Ws", "  abc")]),
            ExpandVerdict::Partial,
        ),
        (
            "$(Foo.EndsWith('c'))",
            props(&[("Foo", "abc")]),
            ExpandVerdict::Exact,
        ),
        // --- TFM-inference intrinsics (supported today) ----------------------
        (
            "$([MSBuild]::GetTargetFrameworkVersion('net8.0'))",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        (
            "$([MSBuild]::GetTargetFrameworkIdentifier('net8.0'))",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        (
            "$(TargetFramework.TrimStart('vV'))",
            props(&[("TargetFramework", "v8.0")]),
            ExpandVerdict::Exact,
        ),
        // --- Dotted property names are member access, never names (D2) -------
        // MSBuild *errors* on these (MSB4184/MSB5016); today we mis-read `A.B`
        // as a property name → `Undefined` → `Partial` (Stage 2 re-parses it as
        // member access, still `Partial`). Either way no over-commit.
        ("$(A.B)", props(&[]), ExpandVerdict::Partial),
        // --- Stage 3: the new pinned evaluators (now Exact) ------------------
        // Paren-less instance property `.Length` → int.
        (
            "$(Foo.Length)",
            props(&[("Foo", "abc")]),
            ExpandVerdict::Exact,
        ),
        // Non-ASCII receiver declines `.Length` (UTF-16 vs scalar) → Partial.
        (
            "$(Foo.Length)",
            props(&[("Foo", "café")]),
            ExpandVerdict::Partial,
        ),
        // String `.Split(char-set)[index]`.
        (
            "$(V.Split('-')[0])",
            props(&[("V", "10.1.300-beta.1")]),
            ExpandVerdict::Exact,
        ),
        // Quoted-paren separator inside Split (scanning + Split together).
        (
            "$(V.Split(')')[0])",
            props(&[("V", "a)b")]),
            ExpandVerdict::Exact,
        ),
        // Multi-char set (empty entries kept) and array `.Length`.
        (
            "$(V.Split('-_')[1])",
            props(&[("V", "a-b_c")]),
            ExpandVerdict::Exact,
        ),
        (
            "$(V.Split('--')[1])",
            props(&[("V", "a--b")]),
            ExpandVerdict::Exact,
        ),
        (
            "$(V.Split('-').Length)",
            props(&[("V", "a-b-c")]),
            ExpandVerdict::Exact,
        ),
        // Terminal array (`System.String[]`) and out-of-range index → declined.
        (
            "$(V.Split('-'))",
            props(&[("V", "a-b")]),
            ExpandVerdict::Partial,
        ),
        (
            "$(V.Split('-')[9])",
            props(&[("V", "a-b")]),
            ExpandVerdict::Partial,
        ),
        // String indexer → char; out-of-range and `Char.Length` declined.
        ("$(Foo[0])", props(&[("Foo", "abc")]), ExpandVerdict::Exact),
        (
            "$(Foo[9])",
            props(&[("Foo", "abc")]),
            ExpandVerdict::Partial,
        ),
        (
            "$(Foo[0].Length)",
            props(&[("Foo", "abc")]),
            ExpandVerdict::Partial,
        ),
        // `[System.Version]::Parse(x).Major/.Minor/.Build`.
        (
            "$([System.Version]::Parse('10.1.300').Major)",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        (
            "$([System.Version]::Parse('10.1.300').Minor)",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        (
            "$([System.Version]::Parse('10.1.300').Build)",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        // Absent Build is -1; terminal Version renders the joined components.
        (
            "$([System.Version]::Parse('1.2').Build)",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        (
            "$([System.Version]::Parse('1.02.3'))",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        // MSBuild-error version shapes stay Partial (declined, not committed).
        (
            "$([System.Version]::Parse('10').Major)",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        (
            "$([System.Version]::Parse('1.2.3.4.5').Major)",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        (
            "$([System.Version]::Parse('2147483648.1').Major)",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        (
            "$([System.Version]::Parse('1.2').Revision)",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        // `[MSBuild]::EnsureTrailingSlash`, incl. backslash normalisation.
        // Unix-only: the evaluator declines on Windows (unverified separator
        // semantics), so the verdict is `Partial` there.
        ("$([MSBuild]::EnsureTrailingSlash('/a/b'))", props(&[]), ets),
        (
            "$([MSBuild]::EnsureTrailingSlash('/a/b/'))",
            props(&[]),
            ets,
        ),
        ("$([MSBuild]::EnsureTrailingSlash('a\\b'))", props(&[]), ets),
        ("$([MSBuild]::EnsureTrailingSlash(''))", props(&[]), ets),
        // A nested string-yielding `$(…)` in a string arg is admitted; a nested
        // non-string (int/array) is declined (MSBuild errors — no coercion).
        (
            "$([System.Version]::Parse('$(V.TrimStart('v'))').Major)",
            props(&[("V", "v8.0")]),
            ExpandVerdict::Exact,
        ),
        (
            "$(P.Contains('$(V.Length)'))",
            props(&[("P", "z"), ("V", "abc")]),
            ExpandVerdict::Partial,
        ),
        (
            "$(P.Contains('$(V.Split('-'))'))",
            props(&[("P", "z"), ("V", "a-b")]),
            ExpandVerdict::Partial,
        ),
        // The F# SDK's FSharpCoreMaximumMajorVersion derivation, verbatim.
        (
            "$([System.Version]::Parse('$(FSCorePackageVersion.Split('-')[0])').Major)",
            props(&[("FSCorePackageVersion", "10.1.300")]),
            ExpandVerdict::Exact,
        ),
        (
            "$([System.Version]::Parse('$(FSCorePackageVersion.Split('-')[0])').Major)",
            props(&[("FSCorePackageVersion", "10.1.300-beta.1")]),
            ExpandVerdict::Exact,
        ),
        // `[System.IO.Path]::IsPathRooted` — unix-only pin (declined on
        // Windows, hence `ipr`), incl. the surprising MSBuild-level
        // backslash rooting and the leading-space defeat.
        ("$([System.IO.Path]::IsPathRooted('/a/b'))", props(&[]), ipr),
        ("$([System.IO.Path]::IsPathRooted('a/b'))", props(&[]), ipr),
        ("$([System.IO.Path]::IsPathRooted(''))", props(&[]), ipr),
        ("$([System.IO.Path]::IsPathRooted(' /a'))", props(&[]), ipr),
        // A *leading* backslash reaching `IsPathRooted` declines: MSBuild's path
        // fixup runs on the *escaped* text, so a live `\a` is adjusted to `/a`
        // and rooted (True) while an escaped `%5ca` stays `\a` and is not (False)
        // — a split we cannot reproduce, since both arrive as `\a` in our value
        // model. Declining is fail-safe; the escaped forms are in the sweep.
        (
            "$([System.IO.Path]::IsPathRooted('\\a'))",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        // A *non-leading* backslash does not change rootedness — the unix fixup
        // and the escaped-vs-live split only ever flip the leading character —
        // so these commit `False` (oracle 2026-07-13, both cwds). Path-fixup
        // keystone (`docs/msbuild-unix-path-fixup-plan.md` P3).
        (
            "$([System.IO.Path]::IsPathRooted('obj\\'))",
            props(&[]),
            ipr,
        ),
        ("$([System.IO.Path]::IsPathRooted('a\\b'))", props(&[]), ipr),
        (
            "$([System.IO.Path]::IsPathRooted('C:\\a'))",
            props(&[]),
            ipr,
        ),
        (
            "$([System.IO.Path]::IsPathRooted('//server/share'))",
            props(&[]),
            ipr,
        ),
        // The SDK's own shape: an *unquoted* property-reference argument
        // (`Microsoft.Common.props` gates MSBuildProjectExtensionsPath
        // handling on exactly this). The `obj\` default is the keystone case —
        // a non-leading backslash arriving through a `$(…)` boundary.
        (
            "$([System.IO.Path]::IsPathRooted($(Dir)))",
            props(&[("Dir", "obj/")]),
            ipr,
        ),
        (
            "$([System.IO.Path]::IsPathRooted($(Dir)))",
            props(&[("Dir", "obj\\")]),
            ipr,
        ),
        (
            "$([System.IO.Path]::IsPathRooted($(Dir)))",
            props(&[("Dir", "/abs/obj/")]),
            ipr,
        ),
        // `[MSBuild]::IsOSPlatform` — the host mapping is compile-time,
        // so both sides answer for the same machine.
        ("$([MSBuild]::IsOSPlatform('osx'))", props(&[]), osp),
        ("$([MSBuild]::IsOSPlatform('OSX'))", props(&[]), osp),
        ("$([MSBuild]::IsOSPlatform('macos'))", props(&[]), osp),
        ("$([MSBuild]::IsOSPlatform('darwin'))", props(&[]), osp),
        ("$([MSBuild]::IsOSPlatform('linux'))", props(&[]), osp),
        ("$([MSBuild]::IsOSPlatform('windows'))", props(&[]), osp),
        ("$([MSBuild]::IsOSPlatform('freebsd'))", props(&[]), osp),
        (
            "$([MSBuild]::IsOSPlatform('garbage name'))",
            props(&[]),
            osp,
        ),
        (
            "$([MSBuild]::IsOSPlatform(''))",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        // `[MSBuild]::AreFeaturesEnabled` — the change-wave threshold
        // property (`MSBuildDisableFeaturesFromVersion`) is *reserved*:
        // the oracle cannot inject it, and project writes are MSB4004.
        // The harness therefore only exercises the declining side (the
        // walker sees the name undefined until Stage C.2's toolset
        // seeding); the sentinel-value pins live as unit tests in
        // `properties/expr.rs`, citing per-value oracle probes.
        (
            "$([MSBuild]::AreFeaturesEnabled('17.10'))",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        (
            "$([MSBuild]::AreFeaturesEnabled('banana'))",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        // Unicode-padded waves: MSBuild's value parse is ASCII-whitespace-only,
        // so these are project *errors*. They land in `Partial` for two
        // independent reasons here (the threshold is unseeded *and* the padding
        // declines); the committed-side pin lives in the `expr.rs` unit test,
        // which can seed the reserved sentinel the oracle refuses to inject.
        (
            "$([MSBuild]::AreFeaturesEnabled('17.10\u{a0}'))",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        (
            "$([MSBuild]::AreFeaturesEnabled('\u{2003}17.10'))",
            props(&[]),
            ExpandVerdict::Partial,
        ),
        // Unicode whitespace in the *expression source* around an argument is
        // fine in MSBuild (its expander is Unicode-tolerant there) — only the
        // value-level parse above is ASCII-bound. Both sides must agree.
        (
            "$(Foo.Contains(\u{a0}'a'))",
            props(&[("Foo", "abc")]),
            ExpandVerdict::Exact,
        ),
        // --- %XX escapes reaching the expression evaluator ---------------
        // MSBuild unescapes `%` + two hex digits before a property function
        // sees the text — literal arguments, spliced property values, and
        // receivers alike. Stage E3 models that (unescape receiver/args in,
        // escape result out), so these commit exactly instead of declining.
        (
            "$([System.IO.Path]::IsPathRooted('%2fabc'))",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        ("$([MSBuild]::IsOSPlatform('%6fSX'))", props(&[]), osp),
        (
            "$([MSBuild]::EnsureTrailingSlash('a%20b'))",
            props(&[]),
            ets,
        ),
        (
            "$([System.IO.Path]::IsPathRooted($(Esc)))",
            props(&[("Esc", "%2fabc")]),
            ipr,
        ),
        (
            "$(Esc.Length)",
            props(&[("Esc", "a%20b")]),
            ExpandVerdict::Exact,
        ),
        // An escape pair composed by expansion (`%` + spliced `20`) is
        // unescaped by MSBuild too — composed escaped, unescaped once.
        (
            "$([MSBuild]::EnsureTrailingSlash('a%$(N)b'))",
            props(&[("N", "20")]),
            ets,
        ),
        // The path-argument layer (`eval_exact_path_arg`) takes its argument to
        // its point of use, unescaping exactly once — which is what MSBuild does
        // before the function runs — so a *spliced* escape now commits the right
        // path (`Combine` of `a%2fb` and `b` is `a/b/b`) instead of declining.
        // The literal-text form below still declines: the expression evaluator's
        // own entry guard is E3's to remove.
        (
            "$([System.IO.Path]::Combine(`$(Esc)`,`b`))",
            props(&[("Esc", "a%2fb")]),
            ExpandVerdict::Exact,
        ),
        (
            "$([System.IO.Path]::Combine('a%2fb','c'))",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        // A `Combine` *result* is `\`→`/` converted unconditionally on unix
        // (oracle 2026-07-13, both cwds), so a backslash-bearing part commits
        // the slash form rather than declining. Path-fixup keystone
        // (`docs/msbuild-unix-path-fixup-plan.md` P3).
        (
            "$([System.IO.Path]::Combine('a\\b','c'))",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        (
            "$([System.IO.Path]::Combine('$(Dir)','obj\\'))",
            props(&[("Dir", "/repo/proj")]),
            ExpandVerdict::Exact,
        ),
        // A `%` not followed by two hex digits is literal in MSBuild and
        // still commits.
        (
            "$(Pct.Length)",
            props(&[("Pct", "100%")]),
            ExpandVerdict::Exact,
        ),
        (
            "$([MSBuild]::EnsureTrailingSlash('a%zb'))",
            props(&[]),
            ExpandVerdict::Exact,
        ),
        // --- Backtick / double-quote string literals ----------------------
        // MSBuild accepts `'`, `` ` ``, and `"` interchangeably as string
        // delimiters, and the SDK's own targets use backticks
        // (`IsOSPlatform(`Windows`)` in
        // `Microsoft.NET.RuntimeIdentifierInference.targets`).
        ("$([MSBuild]::IsOSPlatform(`Windows`))", props(&[]), osp),
        ("$([MSBuild]::IsOSPlatform(\"osx\"))", props(&[]), osp),
        (
            "$(Foo.Contains(`x`))",
            props(&[("Foo", "axb")]),
            ExpandVerdict::Exact,
        ),
        // A `$(…)` splice inside a backtick literal, and quote characters
        // of the *other* kinds as ordinary text inside a literal.
        (
            "$([MSBuild]::EnsureTrailingSlash(`$(Dir)b`))",
            props(&[("Dir", "a/")]),
            ets,
        ),
        (
            "$(Foo.Contains(`'`))",
            props(&[("Foo", "a'b")]),
            ExpandVerdict::Exact,
        ),
        (
            "$(Foo.Contains('`'))",
            props(&[("Foo", "a`b")]),
            ExpandVerdict::Exact,
        ),
        // --- IsOSPlatform: non-ASCII spelling ------------------------------
        // MSBuild matches under invariant uppercasing (`oſx` → `OSX`,
        // True on macOS); we compare ASCII-only, so non-ASCII declines.
        (
            "$([MSBuild]::IsOSPlatform('o\u{17f}x'))",
            props(&[]),
            ExpandVerdict::Partial,
        ),
    ];

    let mut exact = 0;
    let mut partial = 0;
    for (value, props, expected) in cases {
        let got = check_expand_certain_implies_exact(&mut oracle, value, props);
        assert_eq!(
            got, *expected,
            "our expansion landed in {got:?}, expected {expected:?} for {value:?}"
        );
        match got {
            ExpandVerdict::Exact => exact += 1,
            ExpandVerdict::Partial => partial += 1,
        }
    }
    // Sanity: the corner list exercises both branches (guards against the
    // evaluator regressing to all-`Partial`, which would make the differential
    // vacuous).
    assert!(
        exact >= 8 && partial >= 8,
        "corners: {exact} exact, {partial} partial"
    );
}

/// Fixed-seed generative sweep over literal/property-ref/supported-function
/// shapes. Every generated value must satisfy certain-implies-exact; a coverage
/// floor guards against the corpus degrading to all-`Partial` (which would pass
/// the sweep while testing nothing on the committed side).
#[test]
fn fixed_seed_sweep() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0x1337_c0de_f00d);

    const CASES: usize = 3000;
    let mut exact = 0usize;
    let mut partial = 0usize;

    for _ in 0..CASES {
        let value = gen_expand_value(&mut rng);
        let props = gen_expand_props(&mut rng);
        match check_expand_certain_implies_exact(&mut oracle, &value, &props) {
            ExpandVerdict::Exact => exact += 1,
            ExpandVerdict::Partial => partial += 1,
        }
    }

    eprintln!("expand sweep: {exact} exact, {partial} partial / {CASES}");
    // Anti-vacuity floor: a healthy corpus commits to a concrete expansion a
    // large fraction of the time (most segments are literals / defined refs).
    assert!(exact >= 500, "too few committed (exact) cases: {exact}");
    assert!(partial >= 100, "too few partial cases: {partial}");
}

/// Grammar/extent acceptance sweep: adversarial *structural* expressions
/// (static calls, member chains, indexers, nested-quote arguments) that stress
/// the parser's scanner and dispatch. The contract is the same
/// certain-implies-exact — the point here is that the parser finds the right
/// extent and reduces only what the dispatch tables pin, never over-committing
/// on a shape MSBuild would evaluate differently or reject. Most cases are
/// `Partial` today (Stage 2 doesn't yet evaluate `Split`/indexers/`Version`/…);
/// Stage 3 turns the pinned ones `Exact` under this same sweep.
#[test]
fn grammar_acceptance_sweep() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0xace_1105_9a11_c0de);

    const CASES: usize = 3000;
    let mut exact = 0usize;
    let mut partial = 0usize;

    for _ in 0..CASES {
        let value = gen_grammar_value(&mut rng);
        let props = gen_expand_props(&mut rng);
        match check_expand_certain_implies_exact(&mut oracle, &value, &props) {
            ExpandVerdict::Exact => exact += 1,
            ExpandVerdict::Partial => partial += 1,
        }
    }

    eprintln!("grammar sweep: {exact} exact, {partial} partial / {CASES}");
    // Structural shapes are dominated by not-yet-evaluated members, so this
    // sweep is mostly `Partial` — but the pinned chains (TFM inference,
    // `Split(...)[i]`, `Version::Parse(...).Major`, `EnsureTrailingSlash`, and
    // nested string-yielding args) must commit a healthy fraction, guarding
    // against the parser/evaluator silently refusing everything.
    assert!(exact >= 150, "too few committed (exact) cases: {exact}");
    assert!(partial >= 1000, "too few partial cases: {partial}");
}
