//! The reference scanner must never under-report.
//!
//! [`property_references`](borzoi_msbuild::test_support::property_references)
//! is what every trust/uncertainty scan in the crate is built on: given a
//! write's value body, an item spec or a condition leaf, it names the
//! properties that text depends on. A name it *misses* is a property whose
//! untrustworthiness never reaches the value derived from it — so we commit a
//! value we cannot stand behind, which is a certain-implies-exact violation
//! that no differential can see (a wrong decline and a right decline look
//! identical to an oracle).
//!
//! Over-reporting is free: it costs a spurious decline. So the asserted
//! property is one-sided containment, not equality.
//!
//! **The read detector needs no instrumentation.** If changing property `P`'s
//! value changes what `substitute` produces, then the expansion read `P`. That
//! is a black-box oracle for "was this property read", and it sees through
//! member accesses, argument splices and nesting alike — which is the point,
//! since those are exactly where a hand-rolled scanner drifts from the real
//! parser.
//!
//! Scoped deliberately to **value bodies**. The condition layer's scan is a
//! whole-parsed-tree walk precisely *because* `eval_bool` short-circuits, so an
//! evaluation-order read set would under-report there; that obligation is a
//! different shape and is pinned by unit tests in `condition.rs`.

mod common;

use borzoi_msbuild::test_support::{PropertyMap, property_references, substitute};
use common::{CONTROLLED_PROPERTY_NAMES, SplitMix64, gen_grammar_value};

/// Two values chosen to be discriminated by every member the expression
/// evaluator supports: different lengths (`.Length`), different prefixes
/// (`.StartsWith`/`.Contains`), different dot-structure (`.Split`, and
/// `System.Version` parsing for `.Major`/`.Minor`/`.Build`).
const BASE: &str = "net8.0";
const PERTURBED: &str = "wxyz9.1.2";

fn map_with(perturbed: Option<&str>) -> PropertyMap {
    let mut m = PropertyMap::new();
    for name in CONTROLLED_PROPERTY_NAMES {
        let value = match perturbed {
            Some(p) if p.eq_ignore_ascii_case(name) => PERTURBED,
            _ => BASE,
        };
        m.insert(*name, value);
    }
    m
}

/// Names whose value demonstrably moves the expansion's output.
fn names_read(body: &str) -> Vec<&'static str> {
    let base = substitute(body, &map_with(None)).0;
    CONTROLLED_PROPERTY_NAMES
        .iter()
        .copied()
        .filter(|name| substitute(body, &map_with(Some(name))).0 != base)
        .collect()
}

fn assert_reports_every_read(body: &str) {
    let reported = property_references(body);
    for name in names_read(body) {
        assert!(
            reported.iter().any(|r| r.eq_ignore_ascii_case(name)),
            "value body {body:?} reads {name:?} (its value moves the expansion), \
             but the reference scan reported {reported:?} — a taint on {name:?} \
             would not reach the value derived from it",
        );
    }
}

#[test]
fn every_property_the_expansion_reads_is_reported() {
    // The generator already spells the shapes that break a hand-rolled
    // scanner: `PARENLESS` contributes `.Length`/`.Major`, `METHODS`
    // contributes `.ToString`/`.Substring` alongside the allow-listed five,
    // and arguments nest `$(…)` inside quotes of all three delimiters.
    let mut rng = SplitMix64(0x9e37_79b9_7f4a_7c15);
    for _ in 0..20_000 {
        assert_reports_every_read(&gen_grammar_value(&mut rng));
    }
}

/// The concrete shapes that motivated this harness. Kept as named cases so a
/// regression names itself rather than surfacing as a generated blob.
#[test]
fn member_access_does_not_launder_the_receiver() {
    for body in [
        // Paren-less members: the receiver is read whatever follows the dot.
        "$(Configuration.Length)",
        "$(Configuration.Major)",
        "$(Configuration.Bogus)",
        // Method calls, allow-listed and not.
        "$(Configuration.ToString())",
        "$(Configuration.Contains('net'))",
        "$(Configuration.TrimStart('n'))",
        "$(Configuration.Split('.')[0])",
        // Chained, indexed, and nested-argument shapes.
        "$(Configuration.Split('.')[0].Length)",
        "$(Platform.Contains('$(Configuration)'))",
        "$([System.IO.Path]::Combine('$(Configuration)','x'))",
        // Whitespace spellings the condition tokeniser accepts.
        "$( Configuration.Length )",
        "$(Configuration.Contains ('net'))",
    ] {
        assert_reports_every_read(body);
    }
}

/// The detector must actually detect: if `names_read` were vacuously empty the
/// containment above would pass no matter how broken the scan was. This is the
/// non-vacuity check that keeps the harness honest.
#[test]
fn the_read_detector_is_not_vacuous() {
    for body in [
        "$(Configuration)",
        "$(Configuration.Length)",
        "$(Configuration.ToString())",
    ] {
        assert_eq!(
            names_read(body),
            vec!["Configuration"],
            "the perturbation detector must see {body:?} reading Configuration",
        );
    }
    // And it must not fire on a body that reads nothing.
    assert!(names_read("literal text, no references").is_empty());
}
