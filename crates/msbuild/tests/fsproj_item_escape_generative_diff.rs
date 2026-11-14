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

use crate::common::parse_fsproj_with_imports;
use borzoi_msbuild::{DiagnosticKind, glob_resolver};
use common::{Oracle, SplitMix64};
use std::collections::HashMap;

/// Files laid down on disk, named to exercise the characters MSBuild escapes.
/// A literal include need not exist for MSBuild to capture it, but an `Exclude`
/// comparison, a `%2a` literal star and a live glob are far more convincing
/// against real files. `axb.fs` and `ab.fs` are what `a*b.fs` matches as a
/// *pattern* and not as a name; `a;bq.fs` is what `a%3b*.fs` globs.
const FILES: &[&str] = &[
    "plain.fs",
    "a b.fs",
    "a;b.fs",
    "a;bq.fs",
    "a*b.fs",
    "axb.fs",
    "ab.fs",
    "pct%.fs",
    "z.fs",
    // Extensionless, so `*.*` (all files, to MSBuild) and `*.fs` differ.
    "ab",
    // Below the project, so recursive globs and `..` spellings reach it.
    "sub/c.fs",
    // A directory a glob include can be rooted inside, below an exclude's base.
    "sub/Deep/d.fs",
    // A name a wildcard directory segment (`*/a%3bb.fs`) would reach.
    "sub/a;b.fs",
    // A hidden directory, which the SDK's `**/.*/**` exclude skips.
    ".hidden/h.fs",
    // An output file, which the SDK's `bin\/**` exclude skips.
    "bin/x.fs",
    // A capitalised name, so the case of a pattern's filename part matters.
    "Up.fs",
    // Names outside the resolver's allow-list, kept in a sibling of the
    // project directory so only the fragments that name it meet them (two
    // non-ASCII matches leave .NET's order between them, which would decline
    // every recursive glob that reached both): a case pair MSBuild's
    // `OrdinalIgnoreCase` folds, and a character two UTF-16 units wide (one
    // `?` to us, two to MSBuild). The `:` specs need no file: a literal
    // include is captured whether or not it exists, and one on disk would
    // decline every recursive glob that walks past it.
    "../odd/\u{dc}.txt",
    "../odd/\u{1f98b}.txt",
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
    // Spellings of one name. MSBuild compares literal names as full paths
    // (`./ab.fs` is `ab.fs`), except a name holding a `*`, `?` or `:`, which
    // it compares as written.
    "./ab.fs",
    "sub/../ab.fs",
    "sub/../*.fs",
    "sub/../a%2ab.fs",
    "sub/*.fs",
    "./a%2ab.fs",
    "$(MSBuildProjectDirectory)/a%2ab.fs",
    "$(MSBuildProjectDirectory)/ab.fs",
    // The all-files reading of `*.*`, flat and recursive, live and escaped.
    "*.*",
    "**/*.*",
    "sub/*.*",
    "%2a.%2a",
    "**/*.fs",
    // A final `**` names the files below a directory, not the directory.
    "sub/**",
    "**",
    // A wildcard directory segment: modelled on an include alone, refused
    // where it meets excludes.
    "*/c.fs",
    "*/a%3bb.fs",
    "a%3b*/**",
    // The SDK's default excludes, which every SDK project with default items
    // routes through the resolver: hidden directories, and the output
    // directory as the SDK spells it.
    "**/.*/**",
    "bin\\/**",
    // Absolute spellings of globs, which an exclude with a wildcard directory
    // part must match in anchoring.
    "$(MSBuildProjectDirectory)/sub/*.fs",
    "$(MSBuildProjectDirectory)/**/*.fs",
    "ab",
    // A glob rooted inside a directory a `**` exclude names (#615).
    "sub/Deep/*.fs",
    "**/Deep/**",
    // Case variants. MSBuild ignores ASCII case against a literal include,
    // directories included, on every host. Against a glob include the resolver
    // declines where case would decide the answer (#607).
    "PLAIN.FS",
    "up.fs",
    "SUB/c.fs",
    "sub/C.FS",
    "$(MSBuildProjectDirectory)/SUB/c.fs",
];

/// Fragments outside the resolver's allow-list: each must decline through the
/// resolver seam, never match the plain way. Drawn at a low rate
/// ([`gen_spec`]), so the modelled space still dominates the sweep and its
/// commit floor stays meaningful.
const OUTSIDE_ALLOW_LIST: &[&str] = &[
    // Patterns MSBuild's file matcher refuses: an embedded `**`, live or
    // decoded from escapes.
    "a**b.fs",
    "a%2a%2ab.fs",
    "../odd/*.txt",
    "../odd/?.txt",
    "../odd/\u{dc}.txt",
    "../odd/\u{fc}.txt",
    "odd/a:b.fs",
    "$(MSBuildProjectDirectory)/odd/a:b.fs",
    "*.fs/",
    "*.fs.",
    "sub/../*.fs/",
];

/// Property values the splices above resolve to.
const PROP_VALUES: &[&str] = &[
    "a%20b.fs", "a%3bb.fs", "a%2ab.fs", "plain.fs", "pct%", "a%2a*.fs",
];

/// A fragment built from a small grammar of path shapes — a fixed directory,
/// a wildcard directory, a filename, each optional but the last — rather than
/// picked from [`FRAGMENTS`]. The hand-written list holds the shapes someone
/// thought of; this crosses fixed include roots with wildcard exclude
/// directories, which is where #615's wrong commit lived and which no listed
/// pair reached.
fn structural(rng: &mut SplitMix64) -> String {
    const FIXED: &[&str] = &[
        "sub",
        "sub/Deep",
        ".hidden",
        "bin",
        "sub/..",
        "$(MSBuildProjectDirectory)",
        "$(MSBuildProjectDirectory)/sub",
    ];
    const WILD: &[&str] = &[
        "**", "*", "D*", ".*", "s*", "**/Deep", "*/Deep", "**/*", "Deep/**",
    ];
    const NAME: &[&str] = &["*.fs", "*.*", "*", "d.fs", "c.fs", "x.fs", "?.fs", "**"];
    let mut parts: Vec<&str> = Vec::new();
    if rng.below(2) == 0 {
        parts.push(rng.pick(FIXED));
    }
    if rng.below(2) == 0 {
        parts.push(rng.pick(WILD));
    }
    parts.push(rng.pick(NAME));
    parts.join("/")
}

fn gen_spec(rng: &mut SplitMix64) -> String {
    let n = 1 + rng.below(2);
    (0..n)
        .map(|_| {
            if rng.below(12) == 0 {
                rng.pick(OUTSIDE_ALLOW_LIST).to_string()
            } else if rng.below(4) == 0 {
                structural(rng)
            } else {
                rng.pick(FRAGMENTS).to_string()
            }
        })
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
/// MSBuild's `FullPath` resolves `.` and `..` lexically (`sub/../ab.fs` is
/// reported as `ab.fs`), while our identity keeps the spelling it was joined
/// from; both name the same file, so both sides are resolved the same way.
fn norm(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split(['/', '\\']) {
        match seg {
            "." => {}
            ".." if out.len() > 1 => {
                out.pop();
            }
            _ => out.push(seg),
        }
    }
    out.join("/")
}

/// What one seam made of a fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// It committed, and MSBuild agreed exactly (or the check panicked).
    Committed,
    /// The resolver declined, naming why (a `GlobDecline`): a pattern MSBuild's
    /// file matcher refuses, a spec or name outside its allow-list, an order
    /// it cannot reproduce. Intended: the resolver commits only inside the
    /// shapes it models. Any *other* diagnostic through the resolver seam is a
    /// decline the seam should not make.
    ResolverDeclined,
    /// It declined for any other reason.
    Declined,
}

/// `with_resolver` selects the seam under test: with one, includes route through
/// `route_item_through_resolver` and `Exclude` is applied; without one, the
/// literal fast path runs. Both are production paths, and both decode.
fn check_with(oracle: &mut Oracle, fixture: &Fixture, with_resolver: bool) -> Outcome {
    let dir = tempfile::TempDir::new().expect("tempdir");
    // The project sits one level down, so `../odd/` is a sibling no glob
    // under the project walks into.
    let root = borzoi_msbuild::observed::host_fs::canonicalize(dir.path())
        .expect("canonicalize")
        .join("proj");
    std::fs::create_dir_all(&root).expect("mkdir");
    for file in FILES {
        // `*` and `?` are illegal in Windows filenames, so the file cannot be
        // laid down there. Nothing is lost: MSBuild captures a *literal* include
        // whether or not the file exists, so `a%2ab.fs` still tests the thing
        // that matters — that an escaped star is data, not a wildcard. The file
        // exists on unix only to make the case realistic.
        if cfg!(windows) && file.contains(['*', '?']) {
            continue;
        }
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().expect("a file has a parent")).expect("mkdir");
        std::fs::write(path, "module M\n").expect("write source");
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
        let refused_pattern = !parsed.diagnostics.is_empty()
            && parsed
                .diagnostics
                .iter()
                .all(|d| matches!(d.kind, DiagnosticKind::GlobDeclined { .. }));
        return if refused_pattern {
            Outcome::ResolverDeclined
        } else {
            Outcome::Declined
        };
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
    Outcome::Committed
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
        // A name holding a `*` is compared as written, not as a full path:
        // the absolute spelling of `a*b.fs` does not exclude it…
        corner(
            "plain.fs",
            "a%2ab.fs",
            Some("$(MSBuildProjectDirectory)/a%2ab.fs"),
        ),
        corner("plain.fs", "./a%2ab.fs", Some("a%2ab.fs")),
        // …while an ordinary name is compared as a full path.
        corner(
            "plain.fs",
            "ab.fs",
            Some("$(MSBuildProjectDirectory)/ab.fs"),
        ),
        corner("plain.fs", "sub/../ab.fs;./ab.fs", Some("ab.fs")),
        // A filename part of `*.*` is every file, extensionless ones too,
        // decoded from escapes as well as live…
        corner("plain.fs", "*", Some("%2a.%2a")),
        corner("plain.fs", "*.*", None),
        corner("plain.fs", "ab;axb.fs", Some("*.*")),
        corner("plain.fs", "**/*.*", None),
        // …except as the filename-only `**/*.*` against a literal include,
        // which needs a dot.
        corner("plain.fs", "ab;axb.fs", Some("**/*.*")),
        // A pattern MSBuild's file matcher refuses (an embedded `**`, live
        // or decoded) declines by name: `ResolverDeclined`.
        corner("plain.fs", "*.fs", Some("a%2a%2ab.fs")),
        corner("plain.fs", "*.fs", Some("a**b.fs")),
        corner("plain.fs", "a**b.fs", None),
        // A glob include relates its excludes by spelling when either has a
        // `.` or `..` segment, so these decline by name; the same exclude
        // against a literal include is a full path, and commits.
        corner("plain.fs", "a%3b*.fs", Some("./a%2ab.fs")),
        corner("plain.fs", "*.fs", Some("./ab.fs")),
        corner("plain.fs", "./*.fs", Some("a*b.fs")),
        corner("plain.fs", "ab.fs;axb.fs", Some("./ab.fs")),
        // A wildcard exclude's fixed directory is a full path against a
        // literal include (`sub/../*.fs` excludes `plain.fs`), and a spelling
        // against a glob include (it excludes nothing from `*.fs`).
        corner("plain.fs", "  plain.fs  ", Some("sub/../*.fs")),
        corner("plain.fs", "*.fs", Some("sub/../*.fs")),
        // Outside the allow-list, each of these once committed a list MSBuild
        // disagrees with; now each declines by name.
        corner("plain.fs", "plain.fs", Some("sub/../*.fs/")),
        corner(
            "plain.fs",
            "$(MSBuildProjectDirectory)/odd/a:b.fs",
            Some("odd/a:b.fs/"),
        ),
        corner(
            "plain.fs",
            "../odd/\u{1f98b}.txt",
            Some("sub/../../odd/?.txt"),
        ),
        corner("plain.fs", "a%3bb.fs", Some("*.fs.")),
        corner(
            "plain.fs",
            "../odd/a%3b\u{dc}.txt",
            Some("../odd/a%3b\u{fc}.txt"),
        ),
        corner("plain.fs", "../odd/?.txt", None),
        // Round three: a wildcard directory segment, and a final `**`.
        corner(
            "plain.fs",
            "$(MSBuildProjectDirectory)/sub/*.fs",
            Some("*/a%3bb.fs"),
        ),
        corner("plain.fs", "a%3b*/**", None),
        // The SDK's `**/.*/**` exclude: modelled against a glob under the
        // project (the corpus holds such projects), declined by name against
        // one whose `..` it would otherwise take.
        corner("plain.fs", "**/*.fs", Some("**/.*/**;bin\\/**")),
        corner("plain.fs", "sub/../*.fs", Some("**/.*/**")),
        corner("plain.fs", "sub/**", None),
        corner("plain.fs", "**", Some("sub/**")),
        corner(
            "plain.fs",
            "$(MSBuildProjectDirectory)/**/*.fs",
            Some("sub/**"),
        ),
        // An absolute exclude meets a relative glob by path.
        corner(
            "plain.fs",
            "*.fs",
            Some("$(MSBuildProjectDirectory)/a*b.fs"),
        ),
        // A directory an exclude names after `**` is taken only where the
        // include's walk enters it (#615): `sub/Deep/*.fs` starts inside
        // `Deep`, so MSBuild keeps its files and the resolver declines.
        corner("plain.fs", "sub/Deep/*.fs", Some("**/Deep/**")),
        corner("plain.fs", "sub/**/*.fs", Some("**/Deep/**")),
        corner("plain.fs", "**/*.fs", Some("**/Deep/**")),
        corner("plain.fs", "*/Deep/*.fs", Some("**/Deep/**")),
        corner("plain.fs", "sub/Deep/*.fs", Some("**/d.fs")),
        corner("plain.fs", "sub/Deep/*.fs", Some("sub/D*/d.fs")),
        corner("plain.fs", "sub/Deep/*.fs", Some("*/Deep/d.fs")),
    ];
    // Each corner must commit through the resolver seam, which globs and
    // applies `Exclude`; a decline there would leave unexercised exactly the
    // seam this harness guards. The literal fast path declines globs and
    // excludes by design, so it is checked wherever it commits.
    for fixture in &cases {
        check_with(&mut oracle, fixture, false);
        let outcome = check_with(&mut oracle, fixture, true);
        assert_ne!(
            outcome,
            Outcome::Declined,
            "the corner declined through the resolver seam:\n{}",
            fixture.xml()
        );
    }
}

/// How MSBuild's matching treats case, from the oracle table on #607.
///
/// Against a literal include, every exclude ignores ASCII case, directories
/// included, on every host (`FileSpecMatcherTester` compares names
/// `OrdinalIgnoreCase` and its `GlobPattern` folds the whole path). Those
/// corners must commit, and match MSBuild.
///
/// Against a glob include the file matcher's rule turns on which directories
/// exist and on the include's walk root, which the resolver does not model, so
/// those corners may decline by name; where one commits it must match.
#[test]
fn case_corners() {
    let mut oracle = Oracle::spawn();
    let corner = |include: &str, exclude: Option<&str>| Fixture {
        esc: "plain.fs".to_string(),
        pct: "pct%".to_string(),
        includes: vec![include.to_string()],
        exclude: exclude.map(str::to_string),
    };
    let literal = [
        corner("plain.fs;z.fs", Some("PLAIN.FS")),
        corner("sub/c.fs;z.fs", Some("SUB/c.fs")),
        corner("sub/c.fs;z.fs", Some("SUB/*.fs")),
        corner("sub/c.fs;z.fs", Some("S*/c.fs")),
        corner("plain.fs;z.fs", Some("**/PLAIN.fs")),
        corner("plain.fs;z.fs", Some("$(MSBuildProjectDirectory)/P*")),
        corner("plain.fs;z.fs", Some("$(MSBuildProjectDirectory)/PLAIN.fs")),
    ];
    for fixture in &literal {
        assert_eq!(
            check_with(&mut oracle, fixture, true),
            Outcome::Committed,
            "a literal-include case corner did not commit:\n{}",
            fixture.xml()
        );
    }
    let glob = [
        corner("sub/*.fs", Some("sub/C.fs")),
        corner("sub/*.fs", Some("SUB/c.fs")),
        corner("**/*.fs", Some("SUB/c.fs")),
        corner("**/*.fs", Some("S*/c.fs")),
        corner("**/*.fs", Some("SUB/*.fs")),
        corner("**/*.fs", Some("**/C.fs")),
        corner("*.fs", Some("$(MSBuildProjectDirectory)/PLAIN.fs")),
        corner("*.fs", Some("u*")),
        corner("SUB/*.fs", None),
        corner("S*/*.fs", None),
        corner("u*.fs", None),
        corner("*.FS", None),
        corner("sub/**/C.fs", None),
    ];
    for fixture in &glob {
        assert_ne!(
            check_with(&mut oracle, fixture, true),
            Outcome::Declined,
            "{}",
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
    let mut resolver_committed = 0usize;
    let mut declined_through_resolver = Vec::new();
    // The oracle answers in well under a millisecond a case, so the sweep runs
    // thousands: a rare pairing (an include rooted inside a directory a `**`
    // exclude names, #615) needs that many to turn up.
    const CASES: usize = 2000;
    for _ in 0..CASES {
        let fixture = Fixture::generate(&mut rng);
        if check_with(&mut oracle, &fixture, false) == Outcome::Committed {
            fast_committed += 1;
        }
        match check_with(&mut oracle, &fixture, true) {
            Outcome::Committed => resolver_committed += 1,
            Outcome::ResolverDeclined => {}
            Outcome::Declined => declined_through_resolver.push(fixture.xml()),
        }
    }
    // The floor exists so a decline-everything model cannot pass vacuously.
    // Through the resolver seam a decline must be the resolver's own, named
    // (a `GlobDecline`); any other is a regression. The named ones come from
    // the low-rate `OUTSIDE_ALLOW_LIST` pool and from globs that reach the
    // `../odd/` names, and run near a quarter of the sweep across seeds, so the
    // floor of 65% commits catches the resolver widening its refusals into
    // the modelled space. The literal fast
    // path declines every glob and every `Exclude` by design (it has no
    // resolver to hand them to), so only its commits are counted.
    eprintln!(
        "escaped item specs: {resolver_committed}/{CASES} committed through the resolver \
         seam ({} named declines), {fast_committed}/{CASES} through the literal fast path",
        CASES - resolver_committed - declined_through_resolver.len()
    );
    assert!(
        declined_through_resolver.is_empty(),
        "{} generated cases declined through the resolver seam; the first:\n{}",
        declined_through_resolver.len(),
        declined_through_resolver[0]
    );
    assert!(
        resolver_committed * 100 >= CASES * 65,
        "only {resolver_committed}/{CASES} committed through the resolver seam"
    );
    assert!(
        fast_committed > 0,
        "the literal fast path committed nothing — its seam is not being exercised"
    );
}
