//! Every parse-time row of the feature table, held to the
//! [`DiagnosticClass`] its gate's diagnostics carry
//! (`docs/ast-versioning-plan.md`, D9 and D10).
//!
//! A row whose FCS effect is `Diagnose` names a feature FCS parses the same
//! way at every version, reporting an error below its version over the tree it
//! builds anyway. Its gate's diagnostics are `Legality`, and that class is a
//! promise consumers act on: `SyntaxRecovery` reads no `Legality` error, and
//! `diagnostics_are_version_invariant` leaves them out. So for each such row a
//! source using the feature must parse, at every version below the feature's,
//! to the tree it parses to at the feature's own version, with the same
//! recovery diagnostics and with nothing more than `Legality` errors naming
//! that feature.
//!
//! A row whose effect is `Reshape` or `Severity` names a feature whose tree or
//! other diagnostics FCS changes with the version, so its gate's diagnostics are
//! `Recovery` and must never read as `Legality`.
//!
//! The fixtures are keyed by feature, and the table decides which features need
//! one: every live row FCS consults while parsing. A row that goes live, or
//! moves from one effect to another, fails here until it has a fixture graded
//! by its new class.

use std::collections::{BTreeMap, HashSet};

use borzoi_astgen::features::{Effect, Stage, feature_table};
use borzoi_cst::language_version::{LanguageFeature, LanguageVersion};
use borzoi_cst::parser::{
    DiagnosticClass, FileKind, Parse, ParseDiagnostic, ParseOptions, parse_with_options,
};

/// Sources that use each feature: a `Diagnose` row's are clean at the
/// feature's version, and the others need only move across it. A feature may
/// have several, one per construct its gate covers.
const FIXTURES: &[(LanguageFeature, &str)] = &[
    (
        LanguageFeature::RelaxWhitespace,
        "module M\ntype T() =\n    static member P\n        with get() =\n      1\n",
    ),
    (
        LanguageFeature::DotlessFloat32Literal,
        "module M\nlet x = 1f\n",
    ),
    (
        LanguageFeature::RelaxWhitespace2,
        "let x = (\n    1\n)\nlet y = 2\n",
    ),
    (
        LanguageFeature::AttributesToRightOfModuleKeyword,
        "module [<AutoOpen>] M\nlet x = 1\n",
    ),
    (
        LanguageFeature::AttributesToRightOfModuleKeyword,
        "module M\nmodule [<AutoOpen>] N =\n    let x = 1\n",
    ),
    (
        LanguageFeature::InterfacesWithAbstractStaticMembers,
        "module M\ntype I =\n    static abstract M: int -> int\n",
    ),
    (LanguageFeature::StrictIndentation, "match x with\n"),
    (
        LanguageFeature::ConstraintIntersectionOnFlexibleTypes,
        "module M\nlet f (x: #System.IDisposable & #System.IComparable) = ()\n",
    ),
    (
        LanguageFeature::ConstraintIntersectionOnFlexibleTypes,
        "module M\nlet f<'T & #System.IDisposable> (x: 'T) = ()\n",
    ),
    (
        LanguageFeature::WhileBang,
        "module M\nlet f () = async { while! async { return false } do () }\n",
    ),
    (
        LanguageFeature::NullnessChecking,
        "module M\nlet f (x : System.String | null) = 1\n",
    ),
    (
        LanguageFeature::ParsedHashDirectiveArgumentNonQuotes,
        "module M\n#nowarn 40\nlet x = 1\n",
    ),
    (
        LanguageFeature::AllowTypedLetUseAndBang,
        "module M\nlet f () =\n    async {\n        let! x: int = async { return 1 }\n        return x\n    }\n",
    ),
    (
        LanguageFeature::AllowTypedLetUseAndBang,
        "module M\nlet f () = async { return 1: int }\n",
    ),
    (
        LanguageFeature::ErrorOnInvalidDeclsInTypeDefinitions,
        "module M\ntype Outer =\n    type Nested = int\n",
    ),
    (
        LanguageFeature::PreprocessorElif,
        "module M\n#if FOO\nlet a = 1\n#elif BAR\nlet a = 2\n#endif\n",
    ),
];

/// `Diagnose` rows whose gate borzoi does not report, with the reason. Their
/// fixtures must still parse to one tree and one set of diagnostics at every
/// version, so that reporting them later is a matter of classing the new
/// diagnostics, not of discovering a tree that moves.
const NOT_REPORTED: &[(LanguageFeature, &str)] = &[(
    LanguageFeature::ParsedHashDirectiveArgumentNonQuotes,
    "its FS3350 and FS0203 are not reported; Directive::warning_numbers reads the feature",
)];

const LADDER: [LanguageVersion; 10] = [
    LanguageVersion::V4_6,
    LanguageVersion::V4_7,
    LanguageVersion::V5_0,
    LanguageVersion::V6_0,
    LanguageVersion::V7_0,
    LanguageVersion::V8_0,
    LanguageVersion::V9_0,
    LanguageVersion::V10_0,
    LanguageVersion::V11_0,
    LanguageVersion::Preview,
];

fn parse_at(source: &str, lang: LanguageVersion) -> Parse {
    parse_with_options(
        source,
        ParseOptions {
            file_kind: FileKind::Impl,
            symbols: &HashSet::new(),
            lang,
        },
    )
}

fn feature_named(name: &str) -> Option<LanguageFeature> {
    LanguageFeature::ALL
        .into_iter()
        .find(|f| format!("{f:?}") == name)
}

fn recovery(diagnostics: &[ParseDiagnostic]) -> Vec<&ParseDiagnostic> {
    diagnostics.iter().filter(|d| d.is_recovery()).collect()
}

fn legality(diagnostics: &[ParseDiagnostic]) -> Vec<&ParseDiagnostic> {
    diagnostics.iter().filter(|d| !d.is_recovery()).collect()
}

/// The class of every feature's gate is the one its row's `fcs_effect`
/// gives, read straight from the table.
#[test]
fn the_class_of_a_gate_is_its_rows_effect() {
    assert_eq!(
        LADDER[..9],
        LanguageVersion::NUMBERED,
        "LADDER is the numbered versions and preview"
    );
    for row in feature_table() {
        let feature = feature_named(&row.name).expect("every row is a LanguageFeature");
        let expected = if row.effects.iter().all(|&e| e == Effect::Diagnose) {
            DiagnosticClass::Legality { feature }
        } else {
            DiagnosticClass::Recovery
        };
        assert_eq!(DiagnosticClass::of_gate(feature), expected, "{}", row.name);
    }
}

/// Every live row FCS consults while parsing has a fixture, and every fixture
/// names such a row.
#[test]
fn every_parse_time_row_has_a_fixture() {
    let needed: BTreeMap<String, LanguageFeature> = feature_table()
        .into_iter()
        .filter(|row| row.is_live() && row.stages != [Stage::Check])
        .map(|row| {
            let f = feature_named(&row.name).expect("every row is a LanguageFeature");
            (row.name, f)
        })
        .collect();
    let mut failures = Vec::new();
    for (name, feature) in &needed {
        if !FIXTURES.iter().any(|(f, _)| f == feature) {
            failures.push(format!(
                "{name} is a live parse-time row with no fixture; add a source using it to FIXTURES"
            ));
        }
    }
    for (feature, source) in FIXTURES {
        if !needed.values().any(|f| f == feature) {
            failures.push(format!(
                "the fixture {source:?} names {feature:?}, which is not a live parse-time row"
            ));
        }
    }
    for (feature, _) in NOT_REPORTED {
        if DiagnosticClass::of_gate(*feature) == DiagnosticClass::Recovery {
            failures.push(format!(
                "{feature:?} is in NOT_REPORTED, which is for Diagnose rows"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The promise each class makes, over every fixture at every version.
#[test]
fn each_gate_keeps_the_promise_of_its_class() {
    let mut failures = Vec::new();
    for &(feature, source) in FIXTURES {
        let introduced = feature.introduced();
        let at = parse_at(source, introduced);
        let parses: Vec<(LanguageVersion, Parse)> = LADDER
            .into_iter()
            .map(|lang| (lang, parse_at(source, lang)))
            .collect();
        let mut fail = |lang: LanguageVersion, what: String| {
            failures.push(format!("{feature:?} at {lang}, {source:?}: {what}"));
        };
        match DiagnosticClass::of_gate(feature) {
            class @ DiagnosticClass::Legality { .. } => {
                let reported = !NOT_REPORTED.iter().any(|(f, _)| *f == feature);
                if !at.errors.is_empty() || !at.warnings.is_empty() {
                    fail(
                        introduced,
                        format!(
                            "the fixture must be clean at the feature's version: {:?} {:?}",
                            at.errors, at.warnings
                        ),
                    );
                }
                for (lang, p) in &parses {
                    let lang = *lang;
                    if !p.root.structurally_eq(&at.root) {
                        fail(
                            lang,
                            format!("the tree differs from the tree at {introduced}"),
                        );
                    }
                    if recovery(&p.errors) != recovery(&at.errors)
                        || recovery(&p.warnings) != recovery(&at.warnings)
                    {
                        fail(
                            lang,
                            format!(
                                "the recovery diagnostics differ from those at {introduced}: \
                                 {:?} {:?}",
                                p.errors, p.warnings
                            ),
                        );
                    }
                    let gated = legality(&p.errors);
                    if let Some(other) = gated.iter().find(|d| d.class != class) {
                        fail(
                            lang,
                            format!("a legality error names another feature: {other:?}"),
                        );
                    }
                    if !legality(&p.warnings).is_empty() {
                        fail(
                            lang,
                            format!("a warning is a legality one: {:?}", p.warnings),
                        );
                    }
                    let expect_gate = reported && lang < introduced;
                    if gated.is_empty() == expect_gate {
                        fail(
                            lang,
                            format!(
                                "expected {} legality error: {:?}",
                                if expect_gate { "a" } else { "no" },
                                p.errors
                            ),
                        );
                    }
                }
            }
            DiagnosticClass::Recovery => {
                let mut moves = false;
                for (lang, p) in &parses {
                    let lang = *lang;
                    if let Some(d) = legality(&p.errors)
                        .into_iter()
                        .chain(legality(&p.warnings))
                        .next()
                    {
                        fail(
                            lang,
                            format!("a recovery gate reported a legality diagnostic: {d:?}"),
                        );
                    }
                    moves |= lang < introduced
                        && (!p.root.structurally_eq(&at.root)
                            || p.errors != at.errors
                            || p.warnings != at.warnings);
                }
                if !moves {
                    fail(
                        introduced,
                        "the fixture parses the same at every version below the feature's, so it \
                         does not exercise the gate"
                            .to_string(),
                    );
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
