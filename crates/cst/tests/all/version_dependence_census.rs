//! Which corpus files parse differently at different language versions, and
//! how (`docs/ast-versioning-plan.md`, D10).
//!
//! Walks the corpus rooted at `BORZOI_CORPUS`, as `parser_corpus` does, and
//! parses every file at every [`LanguageVersion`]. Each version's parse is
//! compared with the preview one's and graded by the first of these that
//! holds:
//!
//! * `S` — the tree differs;
//! * `R` — the tree is the same, but a [`DiagnosticClass::Recovery`]
//!   diagnostic differs;
//! * `L` — only [`DiagnosticClass::Legality`] diagnostics differ;
//! * `.` — the parse is the same.
//!
//! A file's verdict is one letter per numbered version, oldest first (the
//! manifest's `(columns)` line names them). A verdict with an `S` or an `R` is
//! a version-dependent file: a parse at one version may not read as one at
//! another would. One with an `S` or an `R` from the support floor up (the
//! manifest's `(supported-from)` line, [`LanguageVersion::SUPPORTED_MIN`]) is
//! one a parse at a guessed version at or above the floor may misread, and
//! `diagnostics_are_version_invariant` must say so. One with only `L`s is a file whose newer constructs are faithfully
//! gated at older versions over an unchanged tree, and which a guess may read
//! all the same. Every file whose verdict is not all `.` is listed in
//! `tests/manifests/version_dependence_census.txt`, two-sided like the other
//! corpus manifests:
//!
//! ```text
//! BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-cst --test all version_dependence_census:: -- --ignored
//! ```
//!
//! Three properties are asserted on every file first, which regeneration
//! cannot bless:
//!
//! * a legality diagnostic at a version names a feature that version lacks;
//! * two versions that agree on every feature whose gate is `Recovery` (the
//!   feature table's `Reshape` and `Severity` rows) parse to the same tree
//!   with the same recovery diagnostics, so no `Diagnose` gate moves either
//!   corpus-wide;
//! * `diagnostics_are_version_invariant`, which parses at two or three
//!   versions only, agrees with the verdict read off all of them, at every
//!   guess: from the guess up when it is below the support floor (a guess at
//!   4.6 is the whole ladder), and from the floor up otherwise.
//!
//! Symbols are empty, as in `parser_corpus`.

use std::collections::HashSet;
use std::ops::Range;
use std::path::Path;

use borzoi_cst::language_version::{LanguageFeature, LanguageVersion};
use borzoi_cst::parser::{
    DiagnosticClass, FileKind, Parse, ParseDiagnostic, ParseOptions,
    diagnostics_are_version_invariant, parse_with_options,
};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_oracle_harness::par_map::par_map;

use crate::common::corpus_manifest::{
    check_manifest, corpus_count_item, corpus_relative, regenerate_ignored, sort_by_corpus_key,
};
use crate::common::{collect_fsharp_corpus_files, corpus_root, read_corpus_source};

/// The versions a verdict grades, oldest first; each is compared with
/// [`LanguageVersion::Preview`].
const GRADED: [LanguageVersion; 9] = LanguageVersion::NUMBERED;

/// What one file's census found.
enum Outcome {
    NonUtf8,
    Graded {
        /// One letter per [`GRADED`] version.
        verdict: String,
        /// Breaches of the asserted properties.
        breaches: Vec<String>,
    },
}

/// The features whose gates report `Recovery` diagnostics: two versions that
/// support the same ones are in the same band.
fn recovery_features() -> Vec<LanguageFeature> {
    LanguageFeature::ALL
        .into_iter()
        .filter(|&f| DiagnosticClass::of_gate(f) == DiagnosticClass::Recovery)
        .collect()
}

fn band(lang: LanguageVersion, recovery_features: &[LanguageFeature]) -> Vec<bool> {
    recovery_features
        .iter()
        .map(|&f| lang.supports(f))
        .collect()
}

fn same_recovery(a: &Parse, b: &Parse) -> bool {
    a.recovery_errors().eq(b.recovery_errors()) && a.recovery_warnings().eq(b.recovery_warnings())
}

/// A parse's recovery errors and warnings, with the parse's own version
/// written out of their messages: a gate's FS3350 names the version it was
/// parsed at (`Feature … is not available in F# 6.0`), so two versions in one
/// band report it in words that differ only there.
fn recovery_up_to_version(p: &Parse) -> [Vec<(Range<usize>, String)>; 2] {
    let pinned = format!("F# {}.", p.lang);
    let normalise = |ds: Vec<&ParseDiagnostic>| {
        ds.into_iter()
            .map(|d| (d.span.clone(), d.message.replace(&pinned, "F# <pinned>.")))
            .collect()
    };
    [
        normalise(p.recovery_errors().collect()),
        normalise(p.recovery_warnings().collect()),
    ]
}

fn grade(p: &Parse, preview: &Parse) -> char {
    if !p.root.structurally_eq(&preview.root) {
        'S'
    } else if !same_recovery(p, preview) {
        'R'
    } else if p.errors != preview.errors || p.warnings != preview.warnings {
        'L'
    } else {
        '.'
    }
}

/// Whether a verdict moves the tree or a `Recovery` diagnostic at `from` or
/// any later version (preview, the baseline, is one).
fn dependent_from(verdict: &str, from: LanguageVersion) -> bool {
    GRADED
        .iter()
        .zip(verdict.chars())
        .any(|(&lang, c)| lang >= from && matches!(c, 'S' | 'R'))
}

/// Whether a verdict moves the tree or a `Recovery` diagnostic at any version.
fn dependent(verdict: &str) -> bool {
    dependent_from(verdict, LanguageVersion::MIN)
}

/// Whether a verdict moves the tree or a `Recovery` diagnostic at a version a
/// supported compiler checks at.
fn dependent_when_supported(verdict: &str) -> bool {
    dependent_from(verdict, LanguageVersion::SUPPORTED_MIN)
}

fn census_file(path: &Path, src: &str, recovery_features: &[LanguageFeature]) -> Outcome {
    let file_kind = if path.extension().and_then(|s| s.to_str()) == Some("fsi") {
        FileKind::Sig
    } else {
        FileKind::Impl
    };
    let symbols = HashSet::new();
    let parse_at = |lang| {
        parse_with_options(
            src,
            ParseOptions {
                file_kind,
                symbols: &symbols,
                lang,
            },
        )
    };
    let parses: Vec<Parse> = GRADED
        .into_iter()
        .chain([LanguageVersion::Preview])
        .map(parse_at)
        .collect();
    let (preview, graded) = parses.split_last().expect("the ladder is not empty");
    let mut breaches = Vec::new();
    for p in &parses {
        for d in p.errors.iter().chain(&p.warnings) {
            if let DiagnosticClass::Legality { feature } = d.class
                && p.lang.supports(feature)
            {
                breaches.push(format!(
                    "a legality diagnostic at {} names {feature:?}, which it supports: {d:?}",
                    p.lang
                ));
            }
        }
    }
    for (i, a) in parses.iter().enumerate() {
        for b in &parses[i + 1..] {
            if band(a.lang, recovery_features) == band(b.lang, recovery_features)
                && !(a.root.structurally_eq(&b.root)
                    && recovery_up_to_version(a) == recovery_up_to_version(b))
            {
                breaches.push(format!(
                    "{} and {} differ only in Diagnose features, but parse differently",
                    a.lang, b.lang
                ));
            }
        }
    }
    let verdict: String = graded.iter().map(|p| grade(p, preview)).collect();
    // Each guess at or below the floor names a range of its own, the lowest
    // the whole ladder; preview stands for the guesses above the floor.
    for guess in GRADED
        .into_iter()
        .filter(|&g| g <= LanguageVersion::SUPPORTED_MIN)
        .chain([LanguageVersion::Preview])
    {
        let from = guess.min(LanguageVersion::SUPPORTED_MIN);
        if diagnostics_are_version_invariant(src, &symbols, file_kind, guess)
            == dependent_from(&verdict, from)
        {
            breaches.push(format!(
                "diagnostics_are_version_invariant at a guess of {guess} says {}, but the \
                 verdict is {verdict}",
                !dependent_from(&verdict, from)
            ));
        }
    }
    Outcome::Graded { verdict, breaches }
}

#[test]
#[ignore = "parses the corpus at every language version; run with --ignored under nix develop"]
fn census() {
    let root = corpus_root();
    let mut files = collect_fsharp_corpus_files(&root)
        .unwrap_or_else(|err| panic!("walk F# corpus under {}: {err}", root.display()));
    assert!(!files.is_empty(), "no .fs/.fsi/.fsx files under {root:?}");
    sort_by_corpus_key(&root, &mut files);
    let recovery_features = recovery_features();

    let outcomes = par_map(files.len(), |i| {
        let path = &files[i];
        match read_corpus_source(path) {
            Ok(src) => census_file(path, &src, &recovery_features),
            Err(err) if err.is_non_utf8() => Outcome::NonUtf8,
            Err(err) => panic!("{err}"),
        }
    });

    let mut entries = vec![
        corpus_count_item(files.len()),
        (
            "(columns)".to_string(),
            GRADED
                .iter()
                .map(LanguageVersion::to_string)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        (
            "(supported-from)".to_string(),
            LanguageVersion::SUPPORTED_MIN.to_string(),
        ),
    ];
    let mut breaches = Vec::new();
    let (mut dependent_files, mut dependent_when_supported_files, mut legality_only) =
        (0usize, 0usize, 0usize);
    for (path, outcome) in files.iter().zip(outcomes) {
        let key = corpus_relative(&root, path);
        match outcome {
            Outcome::NonUtf8 => entries.push((key, "non-utf8".to_string())),
            Outcome::Graded {
                verdict,
                breaches: found,
            } => {
                breaches.extend(found.into_iter().map(|b| format!("{key}: {b}")));
                if dependent(&verdict) {
                    dependent_files += 1;
                } else if verdict.contains('L') {
                    legality_only += 1;
                }
                if dependent_when_supported(&verdict) {
                    dependent_when_supported_files += 1;
                }
                if verdict.chars().any(|c| c != '.') {
                    entries.push((key, verdict));
                }
            }
        }
    }
    eprintln!(
        "{} files | {dependent_files} version-dependent, {dependent_when_supported_files} of \
         them from {} up | {legality_only} differing in legality diagnostics only",
        files.len(),
        LanguageVersion::SUPPORTED_MIN,
    );
    assert!(
        breaches.is_empty(),
        "{} breaches of the census's properties:\n{}",
        breaches.len(),
        breaches.join("\n")
    );
    let manifest = Manifest::from_keyed(entries).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check_manifest(
        "version_dependence_census",
        &manifest,
        &regenerate_ignored("version_dependence_census"),
    );
}
