//! Differential test: our `%XX` decoder
//! ([`unescape`](borzoi_msbuild::test_support::unescape)) vs the real
//! MSBuild evaluator via the `tools/msbuild-condition-oracle` `expand` op.
//!
//! Stage E0 of `docs/msbuild-escaped-value-plan.md`. The decoder is the
//! foundation the whole escaped-value refactor stands on: E1 will store every
//! property value escaped and unescape exactly once at each point of use, so a
//! decoder that is subtly wrong becomes a *wrong committed value* everywhere at
//! once. Pin it against MSBuild before wiring anything to it.
//!
//! **The asserted property.** For a property body that engages no expansion
//! machinery, MSBuild's evaluated value *is* the unescaped body:
//! `Project.GetPropertyValue` returns `EscapingUtilities.UnescapeAll` of the
//! stored text (`ProjectProperty.cs:89`). So for every generated `s`, if the
//! oracle answers at all, MSBuild's answer must equal `unescape(s)` — byte for
//! byte, no partiality, no fail-safe channel. This differential *cannot*
//! decline; a mismatch is a bug in the decoder.
//!
//! **What is deliberately out of the generated space**, because it belongs to a
//! different layer and a different differential:
//!
//! - `$(…)`, `@(…)`, `%(…)` references — the expansion machinery, covered by
//!   `property_expr_diff.rs`. The sigils themselves (`$`, `@`, `%`, parens)
//!   *are* generated; only the two-character openers are filtered out.
//! - leading/trailing whitespace — MSBuild's XML layer, covered by
//!   `fsproj_property_table_diff.rs` (the `expand` op is structurally blind to
//!   it: it hands MSBuild a property *body*).
//! - **backslashes** — MSBuild's unix-only path fixup, which this differential
//!   found on its first run and which is *not* an escaping behaviour at all.
//!   On non-Windows hosts `Expander` runs every expanded piece through
//!   `FileUtilities.MaybeAdjustFilePath`: a value containing `\` whose first
//!   segment (after `\`→`/` conversion *and* slash-run collapsing) exists as a
//!   directory **relative to the MSBuild process's working directory** is
//!   rewritten. So `<Out>obj\Debug\</Out>` evaluates to `obj/Debug/` when
//!   `obj/` exists and stays `obj\Debug\` when it does not (oracle-pinned
//!   2026-07-12). We commit both verbatim. That is a live wrong-value class,
//!   tracked as its own item in `docs/msbuild-escaped-value-plan.md` — it needs
//!   its own branch, because the fixup depends on process state the LSP does
//!   not share with MSBuild, so the answer is probably to decline rather than
//!   to model. Keeping `\` out of this alphabet keeps E0 about the decoder.
//!
//! Inputs are deterministic (fixed-seed SplitMix64 plus the hand corner list),
//! so a failure reproduces exactly. The random exploration of the algebraic
//! laws (`unescape(escape(s)) == s`) lives in the module's own proptests.

mod common;

use borzoi_msbuild::test_support::{escape, unescape};
use common::{Oracle, SplitMix64};

/// Characters that make the decoder work for its living: the `%` itself, hex
/// digits in both cases, near-miss non-hex, the other eight reserved
/// characters (so `escape`'s output alphabet is in the space too), and
/// non-ASCII (the UTF-16-char-not-UTF-8-byte question).
const ALPHABET: &[&str] = &[
    "%", "%", "%", // over-represented: escapes are what we are testing
    "0", "1", "2", "5", "a", "b", "e", "f", "A", "F", "9", "3", "c", //
    "z", "g", "x", "-", "_", ".", "/", " ", "#", "&", // NB: no `\` — see above
    "*", "?", "@", "$", "(", ")", ";", "'", // the other eight reserved chars
    "é", "â", "→", // non-ASCII
];

fn gen_value(rng: &mut SplitMix64) -> String {
    let len = 1 + rng.below(12);
    (0..len).map(|_| *rng.pick(ALPHABET)).collect()
}

/// A body MSBuild would read as an expansion (or as malformed) engages a layer
/// this differential is not about. Everything else — including bare `$`, `@`,
/// `%`, `(` and `)` — stays in the space.
fn engages_expansion(s: &str) -> bool {
    s.contains("$(") || s.contains("@(") || s.contains("%(")
}

/// The XML layer trims nothing, but the `expand` op cannot see it; keep the
/// space clear of the edges where the two layers are confusable.
fn touches_xml_layer(s: &str) -> bool {
    s.trim() != s
}

#[track_caller]
fn check(oracle: &mut Oracle, value: &str) {
    let Some(msbuild) = oracle.expand(value, &[]) else {
        panic!(
            "MSBuild refused to evaluate the literal property body {value:?}; \
             a body with no expansion in it cannot fail, so either the \
             generator has drifted into the expansion space or this is a real \
             surprise worth pinning"
        );
    };
    assert_eq!(
        unescape(value),
        msbuild,
        "decoder disagrees with MSBuild on {value:?}"
    );
}

/// The corners the port's source reading turns on, each an assertion about a
/// specific line of `EscapingUtilities.cs` — confirmed here against the real
/// evaluator rather than against my reading of it.
#[test]
fn hand_picked_corners() {
    let mut oracle = Oracle::spawn();
    let corners = [
        // Plain decoding, both hex cases (`TryDecodeHexDigit`, line 31).
        "a%20b",
        "%3B",
        "%3b",
        "B%65ta",
        "1%2E0",
        // Decoded output is never re-scanned (the `UnescapeAll` append loop).
        "%2525",
        "%252520",
        // The next-`%` scan resumes one past the previous `%`, so a failed
        // decode leaves a literal `%` that the next char can follow into an
        // escape.
        "%%41",
        "%zz",
        "%2",
        "%",
        "100%",
        // The composed escape the walker differential caught: `100%` + `100%`.
        "100%100%",
        // One `%XX` is one UTF-16 char, not a UTF-8 byte (line 112).
        "%e2",
        "%E2",
        "%c3%a2",
        // Escaped metacharacters are inert as *text* (their inertness in item
        // specs is E4's differential; here we pin only what they decode to).
        "a%3Bb",
        "a%2Ab",
        "a%2ab",
        "%24(NotAProperty)",
        // An *escaped* backslash is invisible to the unix path fixup, which
        // proves the fixup runs on the escaped text, before unescaping: `.\x`
        // would be rewritten to `./x` (the directory `.` exists), but `.%5cx`
        // comes back with the backslash intact. E1 relies on this ordering —
        // the escaped domain is upstream of every other transformation.
        ".%5cx",
        "%5cx",
        // Decoded control characters and the NUL corner.
        "%09",
        "%0A",
        // Non-ASCII either side of an escape.
        "é%20â",
    ];
    for value in corners {
        assert!(!engages_expansion(value) && !touches_xml_layer(value));
        check(&mut oracle, value);
    }
}

/// Every string `escape` can produce must decode back to the input — the law,
/// but asserted against *MSBuild's* decoder rather than our own, which is the
/// part the in-module proptest cannot do.
#[test]
fn escape_output_round_trips_through_msbuild() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0x5c1c_e5ca_9ed0_1234);
    let mut checked = 0usize;
    for _ in 0..200 {
        let raw = gen_value(&mut rng);
        let escaped = escape(&raw);
        if engages_expansion(&escaped) || touches_xml_layer(&escaped) {
            continue;
        }
        let msbuild = oracle
            .expand(&escaped, &[])
            .expect("an escaped literal body cannot fail to evaluate");
        assert_eq!(
            raw, msbuild,
            "escape({raw:?}) = {escaped:?} did not round-trip through MSBuild"
        );
        checked += 1;
    }
    // `escape` never emits `$(`/`@(`/`%(` (it escapes all three sigils *and*
    // the parens), so nothing should have been filtered out. If this bound ever
    // trips, the filter is eating the space the test is meant to cover.
    assert!(
        checked >= 190,
        "only {checked}/200 escaped values reached the oracle"
    );
}

/// The generated sweep: arbitrary strings over the decoder's working alphabet.
#[test]
fn generated_values_decode_exactly() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0x0e5c_a9ed_2026_0712);
    let mut checked = 0usize;
    let mut skipped = 0usize;
    for _ in 0..600 {
        let value = gen_value(&mut rng);
        if engages_expansion(&value) || touches_xml_layer(&value) {
            skipped += 1;
            continue;
        }
        check(&mut oracle, &value);
        checked += 1;
    }
    // A sanity bound in the mould of the other harnesses' certain-fraction
    // floors: if the filters were to swallow most of the space the test would
    // pass by testing nothing.
    assert!(
        checked >= 400,
        "only {checked} of 600 generated values reached the oracle ({skipped} filtered)"
    );
}
