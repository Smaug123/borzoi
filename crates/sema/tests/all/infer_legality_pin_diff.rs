//! A legality error is tree-neutral end to end: FCS, pinned to a language
//! version below a feature's, type-checks a file using the feature exactly as
//! it does at the feature's version, and inference over our tree at the old pin
//! agrees with it (`docs/ast-versioning-plan.md`, D10).
//!
//! The parser classes a gate's diagnostic `Legality` when the feature table
//! says FCS reports it over the tree it builds at every version
//! (`CheckLanguageFeatureAndRecover`). [`SyntaxRecovery::of`] reads no
//! `Legality` error, so annotations in a declaration holding one still commit.
//! That is only sound if the error really leaves FCS's tree, and so its typed
//! tree, alone; the parser's own tests (`borzoi-cst`'s `diagnostic_classes`)
//! can only show that *our* tree does not move. This differential asks FCS.
//!
//! For each fixture, at the version just below the feature's:
//!
//! - our parse reports only `Legality` errors naming the feature, over the tree
//!   it parses to at the feature's version, where it is clean;
//! - FCS's parse reports only FS3350 at the old pin (for a feature in
//!   [`ORACLE_PREDATES`], nothing), its check nothing more, and neither
//!   anything at the feature's version, and every expression and binder type
//!   it reports is the same at both;
//! - every type inference commits at the old pin is FCS's there.
//!
//! The fixtures put the gated construct inside an annotated declaration, so a
//! reading that counted the legality error as a recovery would decline the
//! annotation; the test asserts that some do, or it would not be testing the
//! class at all.
//!
//! Which features need a fixture is read off the generated feature table:
//! every one FCS consults while parsing whose gate is `Legality`, less those
//! the parser does not gate yet ([`NOT_GATED`]).

use std::sync::Arc;

use borzoi_cst::language_version::{FcsStage, LanguageFeature, LanguageVersion};
use borzoi_cst::parser::{DiagnosticClass, FileKind, Parse, ParseOptions, parse_with_options};
use borzoi_cst::syntax::{AstNode, ImplFile, TextRange, TextSize};
use borzoi_sema::{ProjectItems, SyntaxRecovery, infer_file, resolve_file};

use crate::common::{
    FcsTypeMap, GradedDumps, full_bcl_env, invoke_fcs_dump_at_langversion, temp_fs_file,
};

/// One source per feature, each binding an annotated `v` whose declaration
/// holds the gated construct.
const FIXTURES: &[(LanguageFeature, &str)] = &[
    (
        LanguageFeature::AttributesToRightOfModuleKeyword,
        "module M\nmodule [<AutoOpen>] N =\n    let v : int = 1\n",
    ),
    (
        LanguageFeature::InterfacesWithAbstractStaticMembers,
        "module M\ntype I =\n    static abstract M: int -> int\nlet v : int = 1\n",
    ),
    (
        LanguageFeature::ConstraintIntersectionOnFlexibleTypes,
        "module M\nlet v : int =\n    let h (x: #System.IDisposable & #System.IComparable) = x.Dispose()\n    1\n",
    ),
    (
        LanguageFeature::WhileBang,
        "module M\nlet v : int =\n    let a = async { while! async { return false } do () }\n    1\n",
    ),
    (
        LanguageFeature::AllowTypedLetUseAndBang,
        "module M\nlet v : int =\n    let a =\n        async {\n            let! x: int = async { return 1 }\n            return x\n        }\n    1\n",
    ),
    (
        LanguageFeature::PreprocessorElif,
        "module M\nlet v : int =\n#if FOO\n    1\n#elif BAR\n    2\n#else\n    3\n#endif\n",
    ),
];

/// `Legality` features FCS consults while parsing that the parser does not
/// gate, so no legality error exists to grade.
const NOT_GATED: &[(LanguageFeature, &str)] = &[
    (
        LanguageFeature::AccessorFunctionShorthand,
        "`_.M` is accepted at every version until its gate goes live",
    ),
    (
        LanguageFeature::BetterAnonymousRecordParsing,
        "`>|}` and `@>|}` split at every version",
    ),
    (
        LanguageFeature::ScopedNowarn,
        "`#nowarn` and `#warnon` are not consumed",
    ),
    (
        LanguageFeature::ParsedHashDirectiveArgumentNonQuotes,
        "its FS3350 is not reported",
    ),
];

/// Fixtures whose feature error the oracle's FCS (the `FSharp.Compiler.Service`
/// package `tools/fcs-dump` references) does not report, though the compiler
/// the feature table transcribes does. Their types are still graded, but the
/// oracle cannot show the error is one it type-checks past, so each must
/// draw no parse error from it, and fails here once the oracle reports one.
const ORACLE_PREDATES: &[(LanguageFeature, &str)] = &[(
    LanguageFeature::PreprocessorElif,
    "the oracle's FCS reports nothing for `#elif` at 10.0",
)];

/// The numbered version just below `feature`'s.
fn old_pin(feature: LanguageFeature) -> LanguageVersion {
    LanguageVersion::NUMBERED
        .into_iter()
        .rfind(|&v| v < feature.introduced())
        .expect("no feature is legal at every version")
}

fn parse_at(source: &str, lang: LanguageVersion) -> Parse {
    parse_with_options(
        source,
        ParseOptions {
            file_kind: FileKind::Impl,
            symbols: &Default::default(),
            lang,
        },
    )
}

fn key(range: TextRange) -> (usize, usize) {
    (
        u32::from(range.start()) as usize,
        u32::from(range.end()) as usize,
    )
}

/// FS3350, FCS's error for a feature newer than the language version.
const FEATURE_ERROR: u32 = 3350;

/// FCS's expression and binder types for `source` checked at `lang`, with
/// its check errors, and the codes of its parse errors.
fn fcs(source: &str, lang: LanguageVersion) -> (GradedDumps, Vec<u32>) {
    let path = temp_fs_file("infer_legality_pin", source);
    let lang = lang.to_string();
    let types = invoke_fcs_dump_at_langversion("types", &path, &lang);
    let binders = invoke_fcs_dump_at_langversion("binder-types", &path, &lang);
    let parse_errors = invoke_fcs_dump_at_langversion("parse-errors", &path, &lang);
    let _ = std::fs::remove_file(&path);
    let parsed = parse_at(source, LanguageVersion::Preview);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let dumps = GradedDumps::read(&types, &binders, source, Some(&file)).expect("placeable");
    let parse_errors: serde_json::Value =
        serde_json::from_str(&parse_errors).expect("parse-errors payload");
    let parse_codes = parse_errors["Errors"]
        .as_array()
        .expect("an Errors array")
        .iter()
        .map(|e| e["Code"].as_u64().expect("a code") as u32)
        .collect();
    (dumps, parse_codes)
}

/// What inference commits over `parsed` under `recovery`: expression types and
/// binder types by range, and the type of `v`, if it committed one.
fn ours(parsed: &Parse, recovery: &SyntaxRecovery) -> (FcsTypeMap, FcsTypeMap, Option<String>) {
    let file = ImplFile::cast(parsed.root.clone()).expect("impl file");
    let env = full_bcl_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, recovery);
    let inferred = infer_file(
        &file,
        &resolved,
        env,
        &borzoi_sema::PrecedingTypes::default(),
    );
    let types = inferred
        .types()
        .iter()
        .map(|(range, ty)| (key(*range), ty.render(env)))
        .collect();
    let mut v = None;
    let binders = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| {
            let def = resolved.def(*id);
            if def.spelling(env) == "v" {
                v = Some(ty.render(env));
            }
            (key(def.range), ty.render(env))
        })
        .collect();
    (types, binders, v)
}

#[test]
fn every_parse_time_legality_feature_has_a_fixture() {
    let needed: Vec<LanguageFeature> = LanguageFeature::ALL
        .into_iter()
        .filter(|&f| {
            f.fcs_stages() != [FcsStage::Check]
                && DiagnosticClass::of_gate(f) == DiagnosticClass::Legality { feature: f }
        })
        .collect();
    let mut failures = Vec::new();
    for f in &needed {
        let fixtures = FIXTURES.iter().filter(|(g, _)| g == f).count();
        let exempt = NOT_GATED.iter().any(|(g, _)| g == f);
        if (fixtures > 0) == exempt {
            failures.push(format!(
                "{f:?} needs exactly one of a fixture or a NOT_GATED reason ({fixtures} fixtures, \
                 exempt: {exempt})"
            ));
        }
    }
    for (f, _) in ORACLE_PREDATES {
        if !FIXTURES.iter().any(|(g, _)| g == f) {
            failures.push(format!("{f:?} is in ORACLE_PREDATES but has no fixture"));
        }
    }
    for (f, _) in FIXTURES.iter().chain(NOT_GATED) {
        if !needed.contains(f) {
            failures.push(format!(
                "{f:?} is not a parse-time Legality feature; remove it"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn fcs_types_a_legality_gated_file_as_it_does_at_the_features_version() {
    let mut failures = Vec::new();
    let (mut committed, mut old_reading_declines) = (0usize, 0usize);
    for &(feature, source) in FIXTURES {
        let (old, new) = (old_pin(feature), feature.introduced());
        let mut fail = |what: String| failures.push(format!("{feature:?}, {source:?}: {what}"));

        let at_old = parse_at(source, old);
        let at_new = parse_at(source, new);
        let legality = DiagnosticClass::Legality { feature };
        if at_old.errors.is_empty() || at_old.errors.iter().any(|e| e.class != legality) {
            fail(format!(
                "our parse at {old} must report only {legality:?} errors: {:?}",
                at_old.errors
            ));
        }
        if !at_new.errors.is_empty() || !at_old.root.structurally_eq(&at_new.root) {
            fail(format!(
                "our parse at {new} must be clean and the tree at {old}'s: {:?}",
                at_new.errors
            ));
        }

        let (fcs_old, parsed_old) = fcs(source, old);
        let (fcs_new, parsed_new) = fcs(source, new);
        if ORACLE_PREDATES.iter().any(|(f, _)| *f == feature) {
            if !parsed_old.is_empty() {
                fail(format!(
                    "FCS's parse at {old} now reports {parsed_old:?}; take the feature out of \
                     ORACLE_PREDATES"
                ));
            }
        } else if parsed_old.is_empty() || parsed_old.iter().any(|&code| code != FEATURE_ERROR) {
            fail(format!(
                "FCS's parse at {old} must report only FS3350: {parsed_old:?}"
            ));
        }
        if fcs_old.errors.iter().any(|e| e.code != FEATURE_ERROR) {
            fail(format!(
                "FCS's check at {old} must report nothing but FS3350: {:?}",
                fcs_old.errors
            ));
        }
        if !parsed_new.is_empty() || !fcs_new.errors.is_empty() {
            fail(format!(
                "FCS at {new} must be clean: {parsed_new:?} {:?}",
                fcs_new.errors
            ));
        }
        if fcs_old.types != fcs_new.types || fcs_old.binders != fcs_new.binders {
            fail(format!(
                "FCS types the file differently at {old} and {new}:\n  types {:?}\n  vs    {:?}\n  \
                 binders {:?}\n  vs      {:?}",
                fcs_old.types, fcs_new.types, fcs_old.binders, fcs_new.binders
            ));
        }

        let (types, binders, v) = ours(&at_old, &SyntaxRecovery::of(&at_old));
        for (map, theirs, what) in [
            (&types, &fcs_old.types, "expression"),
            (&binders, &fcs_old.binders, "binder"),
        ] {
            for (range, ty) in map {
                if fcs_old.is_ungraded(*range) {
                    continue;
                }
                committed += 1;
                if theirs.get(range) != Some(ty) {
                    fail(format!(
                        "we commit the {what} type `{ty}` at {range:?} (`{}`), FCS at {old} says \
                         {:?}",
                        &source[range.0..range.1],
                        theirs.get(range)
                    ));
                }
            }
        }
        if v.is_none() {
            fail(format!("`v`'s annotation must commit at {old}"));
        }

        let every_error = SyntaxRecovery::Reported(Arc::from(
            at_old
                .errors
                .iter()
                .map(|e| {
                    TextRange::new(
                        TextSize::new(e.span.start as u32),
                        TextSize::new(e.span.end as u32),
                    )
                })
                .collect::<Vec<_>>(),
        ));
        if ours(&at_old, &every_error).2.is_none() {
            old_reading_declines += 1;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(committed > 0, "nothing committed, so nothing was graded");
    assert!(
        old_reading_declines > 0,
        "no fixture's `v` declines when the legality error is read as a recovery, so the \
         fixtures do not exercise the class"
    );
}
