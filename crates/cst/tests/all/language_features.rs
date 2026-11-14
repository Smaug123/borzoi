//! The feature table, `crates/cst/language-features.tsv`, against the F#
//! compiler source it transcribes: the pinned `fsharp-src` tree under
//! `BORZOI_CORPUS`.
//!
//! The table is hand-edited, and everything version-dependent in the parser and
//! the typed facades is generated from it (`docs/ast-versioning-plan.md`, "The
//! feature table"). These tests make a compiler bump surface every change to
//! FCS's feature set mechanically, instead of leaving it to whoever reads the
//! release notes:
//!
//! - every `LanguageFeature` case FCS declares has exactly one row, with the
//!   version FCS's `features` map gives it and the name its FS3350 message
//!   uses, and FCS's version ladder is [`LanguageVersion`]'s;
//! - each row's `fcs_sites` equals a census of `LanguageFeature.X` in the
//!   files of FCS's parse pipeline, so a new parse-time check of an old feature
//!   shows up as well as a new feature;
//! - the live rows are exactly what the parser consults.
//!
//! Each failure says what to change in the table. Reading the source is cheap,
//! so these run in the normal suite.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use borzoi_astgen::features::{FeatureRow, Modelling, Stage, feature_table, sites_cell};
use borzoi_cst::language_version::LanguageVersion;

use crate::common::corpus_root;

/// The files of FCS's parse pipeline, under `src/Compiler`: the lexer, the
/// `#if` parser, the parser, and everything under `SyntaxTree/` (the
/// lex-filter, the lexer and parser helpers, the `#nowarn` scopes, and the
/// post-parse directive checks in `SyntaxTreeOps.fs`), plus the lexer buffer
/// and the driver that runs the post-parse checks. A directory is walked
/// whole, so a new file in it is censused without a change here.
const PIPELINE: &[&str] = &[
    "lex.fsl",
    "pppars.fsy",
    "pars.fsy",
    "SyntaxTree",
    "Facilities/prim-lexing.fs",
    "Facilities/prim-lexing.fsi",
    "Driver/ParseAndCheckInputs.fs",
];

const TABLE: &str = "crates/cst/language-features.tsv";

fn compiler_src() -> PathBuf {
    corpus_root().join("src/Compiler")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The lines of `text` from the first that starts with `start` up to, not
/// including, the next that starts with `end`.
fn section<'a>(text: &'a str, start: &str, end: &str) -> Vec<&'a str> {
    let mut lines = text
        .lines()
        .skip_while(|l| !l.trim_start().starts_with(start));
    assert!(
        lines.next().is_some(),
        "LanguageFeatures.fs has no line starting {start:?}; the test's reader needs updating"
    );
    lines
        .take_while(|l| !l.trim_start().starts_with(end))
        .collect()
}

/// The identifier at the start of `text`.
fn ident(text: &str) -> &str {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    &text[..end]
}

/// What `LanguageFeatures.fs` says about the features and versions.
struct FcsFeatures {
    /// The `LanguageFeature` union's cases, in declaration order.
    cases: Vec<String>,
    /// The `features` map: case → version, spelt as the table spells it.
    versions: BTreeMap<String, String>,
    /// `GetFeatureString`: case → its `FSComp.SR` resource name.
    resources: BTreeMap<String, String>,
    /// The `languageVersions` set, ascending, spelt as `LanguageVersion`
    /// displays it.
    ladder: Vec<String>,
    /// `defaultVersion`, spelt likewise.
    default: String,
    /// The version `IsVersionSupported` compares against, spelt likewise: the
    /// lowest one FCS checks without `SKIP_VERSION_SUPPORTED_CHECK`.
    supported_min: String,
}

/// The table's spelling of an FCS decimal: `4.6m` → `4.6`, `9999m` →
/// `preview`.
fn version_text(decimal: &str) -> String {
    let decimal = decimal.trim().trim_end_matches('m');
    if decimal == "9999" {
        "preview".to_string()
    } else {
        decimal.to_string()
    }
}

fn read_fcs_features(src: &Path) -> FcsFeatures {
    let text = read(&src.join("Facilities/LanguageFeatures.fs"));

    let cases: Vec<String> = section(&text, "type LanguageFeature =", "///")
        .into_iter()
        .filter_map(|l| l.trim().strip_prefix('|'))
        .map(|l| ident(l.trim()).to_string())
        .collect();

    // `static let languageVersion47 = 4.7m` and friends; the aliases
    // (`defaultVersion = languageVersion100`) resolve through the map.
    let mut constants: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("static let ") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(" = ") else {
            continue;
        };
        let value = value.split("//").next().unwrap().trim();
        let resolved = if value.ends_with('m') && value.starts_with(|c: char| c.is_ascii_digit()) {
            version_text(value)
        } else if let Some(v) = constants.get(value) {
            v.clone()
        } else {
            continue;
        };
        constants.insert(name.trim().to_string(), resolved);
    }

    let mut versions = BTreeMap::new();
    for line in section(
        &text,
        "static let features =",
        "static let defaultLanguageVersion",
    ) {
        let Some(rest) = line.trim().strip_prefix("LanguageFeature.") else {
            continue;
        };
        let (case, version) = rest
            .split_once(',')
            .unwrap_or_else(|| panic!("unreadable features-map entry {line:?}"));
        let version = ident(version.trim());
        let text = constants
            .get(version)
            .unwrap_or_else(|| panic!("features-map entry {line:?} names an unknown version"));
        versions.insert(case.to_string(), text.clone());
    }

    let ladder: Vec<String> = section(
        &text,
        "static let languageVersions =",
        "static let features",
    )
    .into_iter()
    .filter_map(|l| constants.get(l.trim()))
    .cloned()
    .collect();
    let default = constants
        .get("defaultVersion")
        .expect("LanguageFeatures.fs defines defaultVersion")
        .clone();
    let supported_min = section(
        &text,
        "static member IsVersionSupported",
        "/// Get a list of valid strings",
    )
    .into_iter()
    .find_map(|l| l.trim().strip_prefix("langVersion >= "))
    .and_then(|rest| constants.get(ident(rest)))
    .expect("`IsVersionSupported` compares `langVersion >= <version>`; the test's reader needs updating")
    .clone();

    let get_feature_string = section(
        &text,
        "static member GetFeatureString",
        "static member GetFeatureVersionString",
    )
    .join(" ");
    let mut resources = BTreeMap::new();
    for arm in get_feature_string.split("| LanguageFeature.").skip(1) {
        let case = ident(arm).to_string();
        let resource = arm
            .split_once("FSComp.SR.")
            .map(|(_, r)| ident(r).to_string())
            .unwrap_or_else(|| panic!("GetFeatureString arm for {case} names no FSComp.SR string"));
        resources.insert(case, resource);
    }

    FcsFeatures {
        cases,
        versions,
        resources,
        ladder,
        default,
        supported_min,
    }
}

/// `FSComp.txt`'s text for a resource, as FCS returns it. Lines are
/// `[number,]name,"text"`, and FCS's resource generator
/// (`FSharpEmbedResourceText.fs`) passes the quoted text through
/// `String.Format` with no arguments, so the text FCS prints is that call's
/// result: see [`format_without_arguments`].
fn fscomp_text(fscomp: &str, resource: &str) -> Option<String> {
    fscomp.lines().find_map(|line| {
        let rest = line
            .split_once(',')
            .filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit()))
            .map_or(line, |(_, rest)| rest);
        let text = rest.strip_prefix(resource)?.strip_prefix(",\"")?;
        let text = text.strip_suffix('"').unwrap_or(text);
        Some(format_without_arguments(text).unwrap_or_else(|| {
            panic!(
                "FSComp.txt's {resource} text {text:?} has a brace String.Format rejects \
                 with no arguments, so FCS's build would fail on it; the test's reader \
                 needs updating"
            )
        }))
    })
}

/// .NET's `String.Format(text)` with no arguments: `{{` is `{`, `}}` is
/// `}`, and any other brace opens a format item (which has no argument to
/// name) or is unbalanced, both of which throw. `None` where it would throw.
fn format_without_arguments(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if (c == '{' || c == '}') && chars.next_if_eq(&c).is_none() {
            return None;
        }
        out.push(c);
    }
    Some(out)
}

#[test]
fn format_without_arguments_matches_string_format() {
    assert_eq!(format_without_arguments("plain").as_deref(), Some("plain"));
    assert_eq!(
        format_without_arguments("builder {{ }}").as_deref(),
        Some("builder { }")
    );
    assert_eq!(format_without_arguments("{{{{}}").as_deref(), Some("{{}"));
    for rejected in ["{0}", "{", "}", "a } b", "{{{"] {
        assert_eq!(format_without_arguments(rejected), None, "{rejected:?}");
    }
}

/// A ready-to-paste row for a feature the table lacks.
fn new_row(case: &str, version: &str, display: &str, sites: &[(String, usize)]) -> String {
    let middle = if sites.is_empty() {
        "check\t-\tUnmodelled(checker-only)\t-".to_string()
    } else {
        "<fcs_stage>\t<fcs_effect>\t<borzoi>\t<engaged>".to_string()
    };
    format!(
        "{case}\t{version}\t{middle}\t{}\t{display}\t",
        sites_cell(sites)
    )
}

/// `LanguageFeature.X` uses per feature and file in the parse pipeline,
/// outside `//` comments.
fn census(src: &Path) -> BTreeMap<String, Vec<(String, usize)>> {
    let mut files = Vec::new();
    for entry in PIPELINE {
        let path = src.join(entry);
        if path.is_dir() {
            collect_files(&path, &mut files);
        } else {
            assert!(
                path.is_file(),
                "{} is gone from the compiler source: find where its work moved and \
                 update `PIPELINE` in this test",
                path.display(),
            );
            files.push(path);
        }
    }
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for file in files {
        let rel = file
            .strip_prefix(src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for line in read(&file).lines() {
            let mut rest = line.split("//").next().unwrap();
            while let Some(at) = rest.find("LanguageFeature.") {
                rest = &rest[at + "LanguageFeature.".len()..];
                let case = ident(rest);
                if !case.is_empty() {
                    *counts
                        .entry(case.to_string())
                        .or_default()
                        .entry(rel.clone())
                        .or_default() += 1;
                }
            }
        }
    }
    counts
        .into_iter()
        .map(|(case, per_file)| (case, per_file.into_iter().collect()))
        .collect()
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("read a directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_files(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| ["fs", "fsi", "fsl", "fsy"].contains(&e.to_str().unwrap_or("")))
        {
            out.push(path);
        }
    }
}

fn report(what: &str, failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{TABLE} disagrees with the compiler source on {what} ({} row(s)). Edit the \
         table, then run `cargo run -p borzoi-astgen --example generate`:\n\n{}",
        failures.len(),
        failures.join("\n\n"),
    );
}

/// Every FCS feature has one row, with FCS's version and FS3350 name, and the
/// table has no row FCS lacks.
#[test]
fn every_fcs_feature_has_a_row_with_its_version() {
    let src = compiler_src();
    let fcs = read_fcs_features(&src);
    let fscomp = read(&src.join("FSComp.txt"));
    let sites = census(&src);
    let rows = feature_table();
    let by_name: BTreeMap<&str, &FeatureRow> = rows.iter().map(|r| (r.name.as_str(), r)).collect();
    assert!(
        fcs.cases.len() > 50,
        "read only {} LanguageFeature cases from LanguageFeatures.fs; the test's reader \
         needs updating",
        fcs.cases.len(),
    );

    let mut failures = Vec::new();
    for case in &fcs.cases {
        let Some(version) = fcs.versions.get(case) else {
            failures.push(format!(
                "FCS declares `LanguageFeature.{case}` but its `features` map has no entry, \
                 so FCS never enables it; give it a row once the map does"
            ));
            continue;
        };
        let display = fcs
            .resources
            .get(case)
            .and_then(|r| fscomp_text(&fscomp, r))
            .unwrap_or_else(|| panic!("no FSComp.txt text for LanguageFeature.{case}"));
        match by_name.get(case.as_str()) {
            None => {
                let found = sites.get(case).map_or(&[][..], Vec::as_slice);
                let decide = if found.is_empty() {
                    ""
                } else {
                    "\nThe parse pipeline consults it, so decide its stage, effect, borzoi and \
                     engaged cells: see docs/ast-versioning-plan.md, \"The feature table\"."
                };
                failures.push(format!(
                    "FCS has `LanguageFeature.{case}` ({version}); add a row:\n  {}{decide}",
                    new_row(case, version, &display, found),
                ));
            }
            Some(row) => {
                if &row.introduced != version {
                    failures.push(format!(
                        "{case}: the table says {}, FCS's `features` map says {version}; set \
                         `introduced` to {version}. If the row is live this moves a gate and \
                         possibly a facade: re-run parser_diff_feature_gates.",
                        row.introduced,
                    ));
                }
                if row.display_name != display {
                    failures.push(format!(
                        "{case}: display_name is {:?}, FSComp.txt says {display:?}; set it to \
                         that (it is the name FS3350 messages use).",
                        row.display_name,
                    ));
                }
            }
        }
    }
    for row in &rows {
        if !fcs.cases.contains(&row.name) {
            failures.push(format!(
                "{}: FCS no longer declares this feature; remove its row (and whatever gate \
                 or interval it drove).",
                row.name,
            ));
        }
    }
    report("its features", failures);

    let ours: Vec<String> = LanguageVersion::NUMBERED
        .iter()
        .map(|v| v.to_string())
        .collect();
    assert_eq!(
        fcs.ladder, ours,
        "FCS's `languageVersions` are not `LanguageVersion::NUMBERED`: a version was added or \
         removed. Add or remove the `LanguageVersion` variant (and `from_lang_version_text`'s \
         spelling of it), and astgen's `VERSIONS` in tools/astgen/src/features.rs.",
    );
    assert_eq!(
        fcs.default,
        LanguageVersion::DEFAULT.to_string(),
        "FCS's `defaultVersion` moved: update `LanguageVersion::DEFAULT` and \
         `from_lang_version_text`'s aliases",
    );
    assert_eq!(
        fcs.supported_min,
        LanguageVersion::SUPPORTED_MIN.to_string(),
        "FCS's `IsVersionSupported` floor moved: update `LanguageVersion::SUPPORTED_MIN` (the \
         `borzoi` crate's `the_support_floor_is_fcs_own` holds the oracle's FCS to it too)",
    );
}

/// Each row's `fcs_sites` is a census of the parse pipeline, and a row the
/// pipeline never consults is a `check` row.
#[test]
fn fcs_sites_match_a_census_of_the_parse_pipeline() {
    let src = compiler_src();
    let census = census(&src);
    let rows = feature_table();
    let mut failures = Vec::new();
    for row in &rows {
        let found = census.get(&row.name).map_or(&[][..], Vec::as_slice);
        if row.sites != found {
            failures.push(format!(
                "{}: fcs_sites is {}, the parse pipeline has {}; set it to that. A new or \
                 moved check can change what FCS does below {}: revisit the row's fcs_stage, \
                 fcs_effect and borzoi.",
                row.name,
                sites_cell(&row.sites),
                sites_cell(found),
                row.introduced,
            ));
        }
        let check_only = row.stages == [Stage::Check];
        if found.is_empty() && !check_only {
            failures.push(format!(
                "{}: the parse pipeline never mentions it, so its fcs_stage must be `check` \
                 (and fcs_effect `-`), not {:?}",
                row.name, row.stages,
            ));
        }
    }
    for case in census.keys() {
        if !rows.iter().any(|r| &r.name == case) {
            failures.push(format!(
                "the parse pipeline consults `LanguageFeature.{case}`, which has no row"
            ));
        }
    }
    report("the parse pipeline's feature checks", failures);
}

/// The features the parser consults directly — `supports(LanguageFeature::X)`
/// or a `gate(LanguageFeature::X, …)` — are live rows, and every live row
/// with an item other than `Kind` (which the parser reaches through the
/// generated `kind_feature`) is consulted.
#[test]
fn live_rows_are_the_features_the_parser_consults() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let generated = src_dir.join("syntax/generated");
    let mut files = Vec::new();
    collect_rust_files(&src_dir, &generated, &mut files);
    let mut consulted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in &files {
        let text = read(file);
        for pattern in ["supports(LanguageFeature::", "gate(LanguageFeature::"] {
            for (at, _) in text.match_indices(pattern) {
                let case = ident(&text[at + pattern.len()..]).to_string();
                let rel = file.strip_prefix(&src_dir).unwrap().display().to_string();
                consulted.entry(case).or_default().insert(rel);
            }
        }
    }
    let rows = feature_table();
    let mut failures = Vec::new();
    // A `Positional` detector is a function of the tree-walk gates, or a
    // lex-filter gate described as `lexfilter …`.
    let feature_gates = read(&src_dir.join("parser/feature_gates.rs"));
    for row in rows.iter().filter(|r| r.is_live()) {
        for m in &row.borzoi {
            if let Modelling::Positional(detector) = m
                && !detector.starts_with("lexfilter ")
                && !feature_gates.contains(&format!("fn {detector}("))
            {
                failures.push(format!(
                    "{}: Positional({detector}) names no function in parser/feature_gates.rs; \
                     name the detector that finds the construct (or `lexfilter …` for a \
                     lex-filter gate)",
                    row.name,
                ));
            }
        }
    }
    for (case, sites) in &consulted {
        match rows.iter().find(|r| &r.name == case) {
            Some(row) if row.is_live() => {}
            Some(row) => failures.push(format!(
                "{case} is consulted in {sites:?} but its row is not live ({:?}); set \
                 `engaged` to live and give it a Positional, Shape or Trivia item",
                row.engaged,
            )),
            None => failures.push(format!("{case} is consulted in {sites:?} but has no row")),
        }
    }
    for row in rows.iter().filter(|r| r.is_live()) {
        let direct = row.borzoi.iter().any(|m| {
            matches!(
                m,
                Modelling::Positional(_) | Modelling::Shape | Modelling::Trivia
            )
        });
        if direct && !consulted.contains_key(&row.name) {
            failures.push(format!(
                "{} is live with {:?}, but nothing in crates/cst/src consults it through \
                 `supports(LanguageFeature::{0})` or `gate(LanguageFeature::{0}, …)`; wire the \
                 gate or mark the row deferred(…)",
                row.name, row.borzoi,
            ));
        }
    }
    report("which features the parser consults", failures);
}

fn collect_rust_files(dir: &Path, skip: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("read a directory entry").path();
        if path == skip {
            continue;
        }
        if path.is_dir() {
            collect_rust_files(&path, skip, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
