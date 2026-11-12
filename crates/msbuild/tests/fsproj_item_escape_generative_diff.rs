//! Generative differential: item specs over an **escape-bearing** alphabet,
//! against the real MSBuild evaluator (the oracle's `items` op — what
//! `-getItem:Compile` reports, but through the resident oracle, so the sweep
//! pays .NET startup once per binary rather than once per case).
//!
//! ## Why this exists
//!
//! The escaped-value refactor (`docs/completed/msbuild-escaped-value-plan.md`) moved every
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
//! (`%25`), authored padding, live globs beside and around those escapes, and
//! property splices of values carrying the same, then asserts **certain
//! implies exact** against the real evaluator.
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
//! and the resolver-backed one (`route_item_through_resolver`) that globs and
//! applies `Exclude`. The resolver is the shipped one
//! (`borzoi_msbuild::glob_resolver::resolve`, what the LSP passes), because the
//! seam between it and the evaluator is where an escape is most easily decoded
//! at the wrong moment: a fragment decoded before the resolver classifies it
//! globs a literal star, and one split after decoding splits on a literal `;`.
//!
//! MSBuild applies an escaped wildcard in an `Exclude` two ways — as a literal
//! against a literal include, as a wildcard against a glob include — so the
//! alphabet holds both kinds of include and files the two readings tell apart
//! (`axb.fs` matches `a*b.fs` as a pattern, not as a name).
//!
//! Inputs are a fixed-seed sweep, so a failure reproduces exactly.

mod common;

use borzoi_msbuild::{glob_resolver, parse_fsproj_with_imports};
use common::{Oracle, SplitMix64};
use std::collections::HashMap;

/// Files laid down on disk, named to exercise the characters MSBuild escapes.
/// A literal include need not exist for MSBuild to capture it, but an `Exclude`
/// comparison, a `%2a` literal star and a live glob are far more convincing
/// against real files. `axb.fs` and `ab.fs` are what `a*b.fs` matches as a
/// *pattern* and not as a name; `a;bq.fs` is what `a%3b*.fs` globs.
const FILES: &[&str] = &[
    "plain.fs", "a b.fs", "a;b.fs", "a;bq.fs", "a*b.fs", "axb.fs", "ab.fs", "pct%.fs", "z.fs",
];

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
    // Live globs: a plain one, one whose literal part carries an escaped `;`
    // (a `;` in the pattern, not a split), one beside an escaped space.
    "*.fs",
    "a%3b*.fs",
    "a%20?.fs",
    // A live star beside an escaped one: MSBuild takes the whole fragment
    // literally, as the one item `a**.fs`.
    "a%2a*.fs",
    // A live star alone, whose matches include the file named `a*b.fs`.
    "a*b.fs",
    // Property splices: the value carries the escapes, and the reserved seed
    // carries whatever the project's own directory contains.
    "$(Esc)",
    "$(Esc);z.fs",
    "$(MSBuildProjectDirectory)/plain.fs",
    "$(Pct)25.fs",
];

/// Property values the splices above resolve to.
const PROP_VALUES: &[&str] = &[
    "a%20b.fs", "a%3bb.fs", "a%2ab.fs", "plain.fs", "pct%", "a%2a*.fs",
];

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

    let resolver: &borzoi_msbuild::GlobResolver<'_> = &glob_resolver::resolve;
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
        .items(&xml, &proj, "Compile", &[])
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

/// Hand-picked corners: one per finding the three review rounds turned up, and
/// one per reading of an escaped wildcard MSBuild's source distinguishes
/// (`ItemSpec.BuildItemFragments`, `EngineFileUtilities.IsValidExclude`,
/// `FileSpecMatcherTester.Parse`), so
/// each is pinned against the real evaluator rather than against my reading of
/// it.
#[test]
fn hand_picked_corners() {
    let mut oracle = Oracle::spawn();
    let corner = |esc: &str, include: &str, exclude: Option<&str>| Fixture {
        esc: esc.to_string(),
        pct: "pct%".to_string(),
        includes: vec![include.to_string()],
        exclude: exclude.map(str::to_string),
    };
    let cases = [
        // An escaped `;` is data: one item named `a;b.fs`, not two.
        corner("plain.fs", "a%3bb.fs", None),
        // …including when it arrives through a property splice.
        corner("a%3bb.fs", "$(Esc)", None),
        // An escaped `*` is a literal star, not a wildcard.
        corner("plain.fs", "a%2ab.fs", None),
        // An escaped space is part of the filename.
        corner("plain.fs", "a%20b.fs", None),
        // Authored padding is padding; the escaped space above is not.
        corner("plain.fs", "  plain.fs  ", None),
        // An `Exclude` must find what it names, escaped or not.
        corner("plain.fs", "a%20b.fs;z.fs", Some("a%20b.fs")),
        // `%25` decodes to a percent, and the decoded output is not re-scanned.
        corner("plain.fs", "pct%25.fs", None),
        // A live star beside an escaped one is literal, whole: `a**.fs`.
        corner("plain.fs", "a%2a*.fs;z.fs", None),
        // …and so is a spliced one.
        corner("a%2a*.fs", "$(Esc)", None),
        // An escaped `;` in a glob is a `;` in the pattern, not a split.
        corner("plain.fs", "a%3b*.fs", None),
        // Against a glob, an escaped-wildcard exclude is decoded and then a
        // wildcard: `a%2ab.fs` excludes `axb.fs` and `ab.fs` too.
        corner("plain.fs", "*.fs", Some("a%2ab.fs")),
        // Against a glob, an exclude with live and escaped wildcards is
        // dropped: it excludes nothing.
        corner("plain.fs", "*.fs", Some("a%2a*.fs")),
        // Against literals, the same escaped-wildcard exclude is a name: it
        // excludes the file called `a*b.fs` and not `axb.fs`.
        corner("plain.fs", "axb.fs;a%2ab.fs;ab.fs", Some("a%2ab.fs")),
        // Against literals, a live-wildcard exclude is a pattern.
        corner("plain.fs", "axb.fs;a%2ab.fs", Some("a*b.fs")),
        // Against a literal `a**.fs`, an exclude with both kinds is a name.
        corner("plain.fs", "a%2a*.fs;z.fs", Some("a%2a*.fs")),
        // An escaped `;` in an exclude is data against a glob…
        corner("plain.fs", "*.fs", Some("a%3bb.fs")),
        // …and a live one splits the exclude, so it names neither half.
        corner("plain.fs", "a%3bb.fs;z.fs", Some("a;b.fs")),
        // An escaped `?` is a literal, against a literal include.
        corner("plain.fs", "a%3fb.fs;axb.fs", Some("a%3fb.fs")),
    ];
    // Each corner must commit through the resolver seam, which globs and
    // applies `Exclude`; a decline there would leave unexercised exactly the
    // seam this harness guards. The literal fast path declines globs and
    // excludes by design, so it is checked wherever it commits.
    for fixture in &cases {
        check_with(&mut oracle, fixture, false);
        assert!(
            check_with(&mut oracle, fixture, true),
            "the corner declined through the resolver seam:\n{}",
            fixture.xml()
        );
    }
}

/// The sweep. Every case that commits is checked against MSBuild; the floor
/// keeps a decline-everything model from passing vacuously.
#[test]
fn generated_item_specs_are_certain_implies_exact() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0xe5ca_9ed1_7ea5);
    let mut fast_committed = 0usize;
    let mut declined_through_resolver = Vec::new();
    const CASES: usize = 200;
    for _ in 0..CASES {
        let fixture = Fixture::generate(&mut rng);
        if check_with(&mut oracle, &fixture, false) {
            fast_committed += 1;
        }
        if !check_with(&mut oracle, &fixture, true) {
            declined_through_resolver.push(fixture.xml());
        }
    }
    // The floor exists so a decline-everything model cannot pass vacuously.
    // Through the resolver seam the whole generated space is modelled: every
    // escape, live glob and `Exclude` in the alphabet commits, so a single
    // decline there is a regression. The literal fast path declines every
    // glob and every `Exclude` by design (it has no resolver to hand them
    // to), so only its commits are counted.
    eprintln!(
        "escaped item specs: {}/{CASES} committed through the resolver seam, \
         {fast_committed}/{CASES} through the literal fast path",
        CASES - declined_through_resolver.len()
    );
    assert!(
        declined_through_resolver.is_empty(),
        "{} generated cases declined through the resolver seam; the first:\n{}",
        declined_through_resolver.len(),
        declined_through_resolver[0]
    );
    assert!(
        fast_committed > 0,
        "the literal fast path committed nothing — its seam is not being exercised"
    );
}
