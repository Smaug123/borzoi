//! Generative differential: item specs over an **escape-bearing** alphabet,
//! against the real MSBuild evaluator (the oracle's `items` op — what
//! `-getItem:Compile` reports, but through the resident oracle, so the sweep
//! pays .NET startup once per binary rather than once per case).
//!
//! ## Why this exists
//!
//! The escaped-value refactor (`docs/msbuild-escaped-value-plan.md`) moved every
//! property and item value into MSBuild's escaped domain, where the rule is
//! *scan and split before you decode; trim in the domain; decode at the leaf*.
//! Getting that rule right at one leaf and wrong at another is invisible to the
//! type system — both spellings compile — and three rounds of review found nine
//! such leaves, one at a time. Every one of them is a **wrong commit**: a
//! silently split item list, an `Update` that matches nothing, an `Exclude` that
//! excludes nothing, a padded filename probed without its padding.
//!
//! A reviewer finding those one per round is not a strategy. This harness finds
//! them mechanically: it generates item specs containing escaped delimiters
//! (`%3b`), escaped wildcards (`%2a`), escaped spaces (`%20`), escaped percents
//! (`%25`), authored padding, and property splices of values carrying the same,
//! then asserts **certain implies exact** against the real evaluator.
//!
//! ## The asserted property
//!
//! Whenever our parse commits — no diagnostics, and the Compile capture is not
//! marked uncertain — our ordered Compile identities must equal MSBuild's,
//! exactly. A decline makes no claim (the fail-safe channel), so the harness
//! also asserts a floor on how often we *do* commit: a model that declined
//! everything would pass vacuously.
//!
//! Every fixture runs through **both** production seams: the literal fast path,
//! and the resolver-backed one (`route_item_through_resolver`) that applies
//! `Exclude`. Running only the first is a coverage hole dressed up as a decline —
//! it was one, until a reviewer pointed at it.
//!
//! Live globs are outside the generated space (the pattern language is the glob
//! resolver's, not this crate's). An *escaped* `%2a` is very much in scope: it
//! must **not** be classified as a glob, because it is a literal star in a
//! filename, and MSBuild captures the one file.
//!
//! Inputs are a fixed-seed sweep, so a failure reproduces exactly.

mod common;

use borzoi_msbuild::{GlobRequest, parse_fsproj_with_imports};
use common::{Oracle, SplitMix64};
use std::collections::HashMap;
use std::path::PathBuf;

/// Files laid down on disk, named to exercise the characters MSBuild escapes.
/// A literal include need not exist for MSBuild to capture it, but an `Exclude`
/// comparison and a `%2a` literal star are far more convincing against real
/// files.
const FILES: &[&str] = &["plain.fs", "a b.fs", "a;b.fs", "a*b.fs", "pct%.fs", "z.fs"];

/// Item-spec fragments. Each escaped spelling names one of the files above, so
/// a decode-order bug shows up as a *wrong identity* rather than a missing file.
const FRAGMENTS: &[&str] = &[
    "plain.fs",
    "z.fs",
    // Escaped space: names `a b.fs`. Decoding at the wrong moment loses the file.
    "a%20b.fs",
    // Escaped semicolon: **data**, not a delimiter — one item named `a;b.fs`.
    "a%3bb.fs",
    // Escaped star: a literal star in the filename, not a wildcard.
    "a%2ab.fs",
    // Escaped percent: `pct%.fs`. `%25` must not be re-scanned after decoding.
    "pct%25.fs",
    // A bare percent that cannot start an escape stays literal.
    "pct%.fs",
    // Authored padding, which MSBuild trims — unlike an escaped space.
    "  plain.fs  ",
    // A real two-element list, so the harness sees splitting work as well as
    // not-work.
    "plain.fs;z.fs",
    // Property splices: the value carries the escapes, and the reserved seed
    // carries whatever the project's own directory contains.
    "$(Esc)",
    "$(Esc);z.fs",
    "$(MSBuildProjectDirectory)/plain.fs",
    "$(Pct)25.fs",
];

/// Property values the splices above resolve to.
const PROP_VALUES: &[&str] = &["a%20b.fs", "a%3bb.fs", "a%2ab.fs", "plain.fs", "pct%"];

fn gen_spec(rng: &mut SplitMix64) -> String {
    let n = 1 + rng.below(2);
    (0..n)
        .map(|_| *rng.pick(FRAGMENTS))
        .collect::<Vec<_>>()
        .join(";")
}

struct Fixture {
    esc: String,
    pct: String,
    includes: Vec<String>,
    exclude: Option<String>,
}

impl Fixture {
    fn generate(rng: &mut SplitMix64) -> Fixture {
        let includes = (0..1 + rng.below(2)).map(|_| gen_spec(rng)).collect();
        // An `Exclude` half the time, drawn from the same alphabet — the leaf
        // where escaped-versus-decoded comparison silently fails to exclude.
        let exclude = (rng.below(2) == 0).then(|| gen_spec(rng));
        Fixture {
            esc: (*rng.pick(PROP_VALUES)).to_string(),
            pct: "pct%".to_string(),
            includes,
            exclude,
        }
    }

    fn xml(&self) -> String {
        let mut s = String::from("<Project>\n  <PropertyGroup>\n");
        s.push_str(&format!("    <Esc>{}</Esc>\n", self.esc));
        s.push_str(&format!("    <Pct>{}</Pct>\n", self.pct));
        s.push_str("  </PropertyGroup>\n  <ItemGroup>\n");
        for include in &self.includes {
            match &self.exclude {
                Some(exclude) => s.push_str(&format!(
                    "    <Compile Include=\"{include}\" Exclude=\"{exclude}\" />\n"
                )),
                None => s.push_str(&format!("    <Compile Include=\"{include}\" />\n")),
            }
        }
        s.push_str("  </ItemGroup>\n</Project>\n");
        s
    }
}

/// Lexical normalisation only: several generated identities name files that do
/// not exist (an escape decoded at the wrong moment is exactly such a name), and
/// `canonicalize` would erase that difference by failing on both sides.
fn norm(path: &str) -> String {
    path.replace('\\', "/")
}

/// A minimal stand-in for the LSP's glob resolver, so the resolver-backed paths
/// — `route_item_through_resolver`, and `Exclude` — actually run. Without one,
/// every `Exclude` fixture declines and the whole seam is untested, which is a
/// coverage hole dressed up as a decline.
///
/// It receives text that has already **left** the escaped domain (the evaluator
/// decodes each fragment at the seam, declining any whose decoded form would
/// carry a `;`/`*`/`?` past the classification that already happened), so it is
/// free to split and glob naively — which is exactly what the real resolver does,
/// and exactly why that decline exists.
fn resolve_globs(request: &GlobRequest<'_>) -> Vec<PathBuf> {
    let resolve = |spec: &str| -> Vec<PathBuf> {
        let joined = request.base_dir.join(spec);
        if !spec.contains(['*', '?']) {
            return vec![joined];
        }
        // Only the trivial `dir/*.ext` shape is generated; a literal-star
        // filename never reaches here as a wildcard, which is the point.
        let (dir, pattern) = match joined.parent().zip(joined.file_name()) {
            Some((d, f)) => (d.to_path_buf(), f.to_string_lossy().into_owned()),
            None => return Vec::new(),
        };
        let Some(suffix) = pattern.strip_prefix('*') else {
            return Vec::new();
        };
        let mut hits: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(suffix))
            .collect();
        hits.sort();
        hits
    };

    let excluded: Vec<PathBuf> = request.excludes.iter().flat_map(|e| resolve(e)).collect();
    request
        .include
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .flat_map(resolve)
        .filter(|p| !excluded.contains(p))
        .collect()
}

/// `with_resolver` selects the seam under test: with one, includes route through
/// `route_item_through_resolver` and `Exclude` is applied; without one, the
/// literal fast path runs. Both are production paths, and both decode.
fn check_with(oracle: &mut Oracle, fixture: &Fixture, with_resolver: bool) -> bool {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
    for file in FILES {
        // `*` and `?` are illegal in Windows filenames, so the file cannot be
        // laid down there. Nothing is lost: MSBuild captures a *literal* include
        // whether or not the file exists, so `a%2ab.fs` still tests the thing
        // that matters — that an escaped star is data, not a wildcard. The file
        // exists on unix only to make the case realistic.
        if cfg!(windows) && file.contains(['*', '?']) {
            continue;
        }
        std::fs::write(root.join(file), "module M\n").expect("write source");
    }
    let proj = root.join("Demo.fsproj");
    let xml = fixture.xml();
    std::fs::write(&proj, &xml).expect("write project");

    let resolver: &borzoi_msbuild::GlobResolver<'_> = &resolve_globs;
    let parsed = parse_fsproj_with_imports(
        &xml,
        &proj,
        &HashMap::new(),
        &common::oracle_environment(),
        None,
        with_resolver.then_some(resolver),
    )
    .expect("well-formed XML parses");

    // A diagnostic withdraws the claim — the fail-safe channel. No claim, no
    // check.
    if !parsed.diagnostics.is_empty() || parsed.items_uncertain {
        return false;
    }

    let ours: Vec<String> = parsed
        .items
        .iter()
        .map(|i| norm(&i.include.to_string_lossy()))
        .collect();
    let theirs: Vec<String> = oracle
        .items(&xml, &proj, "Compile")
        .expect("MSBuild evaluates these documents")
        .iter()
        .map(|p| norm(p))
        .collect();

    assert_eq!(
        ours, theirs,
        "certain-implies-exact violated.\nproject:\n{xml}\nours:   {ours:#?}\ntheirs: {theirs:#?}"
    );
    true
}

/// Hand-picked corners: one per finding the three review rounds turned up, so
/// each is pinned against the real evaluator rather than against my reading of
/// it.
#[test]
fn hand_picked_corners() {
    let mut oracle = Oracle::spawn();
    let cases = [
        // An escaped `;` is data: one item named `a;b.fs`, not two.
        ("plain.fs", vec!["a%3bb.fs".to_string()], None),
        // …including when it arrives through a property splice.
        ("a%3bb.fs", vec!["$(Esc)".to_string()], None),
        // An escaped `*` is a literal star, not a wildcard.
        ("plain.fs", vec!["a%2ab.fs".to_string()], None),
        // An escaped space is part of the filename.
        ("plain.fs", vec!["a%20b.fs".to_string()], None),
        // Authored padding is padding; the escaped space above is not.
        ("plain.fs", vec!["  plain.fs  ".to_string()], None),
        // An `Exclude` must find what it names, escaped or not.
        (
            "plain.fs",
            vec!["a%20b.fs;z.fs".to_string()],
            Some("a%20b.fs".to_string()),
        ),
        // `%25` decodes to a percent, and the decoded output is not re-scanned.
        ("plain.fs", vec!["pct%25.fs".to_string()], None),
    ];
    let mut committed = 0usize;
    let total = cases.len() * 2;
    for (esc, includes, exclude) in cases {
        let has_exclude = exclude.is_some();
        let fixture = Fixture {
            esc: esc.to_string(),
            pct: "pct%".to_string(),
            includes,
            exclude,
        };
        // Both seams: the literal fast path, and the resolver-backed one that
        // applies `Exclude`. Each decodes, and each has been wrong at least once.
        for with_resolver in [false, true] {
            if check_with(&mut oracle, &fixture, with_resolver) {
                committed += 1;
            } else if has_exclude && with_resolver {
                panic!(
                    "the Exclude corner declined with a resolver present — the seam                      this harness exists to guard is not being exercised"
                );
            }
        }
    }
    assert!(
        committed >= total - 1,
        "only {committed}/{total} corners committed — the model is declining shapes it should model"
    );
}

/// The sweep. Every case that commits is checked against MSBuild; the floor
/// keeps a decline-everything model from passing vacuously.
#[test]
fn generated_item_specs_are_certain_implies_exact() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0xe5ca_9ed1_7ea5);
    let mut committed = 0usize;
    const CASES: usize = 60;
    for _ in 0..CASES {
        let fixture = Fixture::generate(&mut rng);
        for with_resolver in [false, true] {
            if check_with(&mut oracle, &fixture, with_resolver) {
                committed += 1;
            }
        }
    }
    // The floor exists so a decline-everything model cannot pass vacuously. Each
    // fixture runs through both seams, so the denominator is twice the case
    // count, and there are exactly two families of decline in the space — both
    // known, both fail-safe, and neither an escape bug:
    //
    // - an `Exclude` reaching the *no-resolver* seam, which this evaluator
    //   declines whatever its escapes look like; and
    // - an escaped `;`/`*` fragment reaching the *resolver* seam, which declines
    //   because the resolver re-splits and re-globs downstream of us. That is the
    //   decline stage E4 of `docs/msbuild-escaped-value-plan.md` removes, by
    //   handing the resolver a fragment list it never re-scans — and when it
    //   does, this floor should rise. A *fall* is a regression.
    let total = CASES * 2;
    eprintln!("escaped item specs: {committed}/{total} committed");
    assert!(
        committed * 2 >= total,
        "only {committed}/{total} committed — certain-implies-exact is passing vacuously"
    );
}
