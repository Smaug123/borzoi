//! Differential test: the **walker's evaluated property table** vs the real
//! MSBuild evaluator, over whole `.fsproj` documents.
//!
//! The crate already diffs conditions (`condition_diff.rs`) and property-value
//! expansion (`property_expr_diff.rs`) against the oracle. Both take their
//! input *after* MSBuild's XML layer has run: the expansion harness hands
//! MSBuild a property **body**, anchored between non-whitespace sentinels
//! precisely so the XML layer is the identity. That makes both harnesses
//! structurally blind to everything the XML layer does — insignificant
//! whitespace, entity decoding, CDATA, comment-split text — which is exactly
//! where a run of hand-found wrong-commits lived (PR #890, review rounds 6-9:
//! a whitespace-only `<P> </P>` stored as `" "` where MSBuild has `""`, so
//! `'$(P)' == ''` committed the wrong branch).
//!
//! This harness closes that hole by handing **the document itself** to both
//! sides, byte for byte, and comparing the evaluated property table.
//!
//! The contract is the walker's, and it is the usual one:
//!
//! - We **committed** a property (it is in `ParsedProject::properties`) ⟹
//!   MSBuild must evaluate the same document to the byte-identical value.
//! - We **dropped** it (an unmodellable body, an unevaluable expansion) ⟹ no
//!   claim. Partiality is the fail-safe superset.
//! - MSBuild **rejects** the document ⟹ we must not have committed anything
//!   for it.
//!
//! Inputs are deterministic (a fixed-seed SplitMix64 over an adversarial body
//! alphabet, plus the hand corner list), so a failure reproduces exactly.

mod common;

use std::collections::HashMap;
use std::path::Path;

use borzoi_msbuild::{parse_fsproj, parse_fsproj_with_imports};
use common::{Oracle, SplitMix64};
use tempfile::TempDir;

/// Property names the generator may define. Disjoint from MSBuild reserved
/// names and from anything the process environment is likely to hold — MSBuild
/// folds the environment in as properties, and a collision would make the two
/// sides disagree for reasons unrelated to the XML layer.
const NAMES: &[&str] = &["Alpha", "Beta", "Gamma", "Delta"];

/// The body alphabet. This is the whole point of the harness, so it reaches
/// well past what the walker models — under certain-implies-exact a decline is
/// free, and only a *wrong commit* fails, so there is no reason to restrict the
/// inputs to shapes we understand.
///
/// `%XX` escapes are deliberately included: MSBuild unescapes them in the
/// evaluated value, and whether the walker models that or degrades, committing
/// the raw text would be a wrong commit — which is exactly the kind of thing
/// this harness exists to catch.
const BODIES: &[&str] = &[
    // Plain content, and the whitespace shapes the XML layer treats specially.
    "",
    "x",
    "a b",
    " ",
    "  ",
    "\t",
    "\r\n  ",
    "  x  ",
    // Comment-split text: MSBuild drops each whitespace-only text *node*, so
    // this is `x`, not `  x` — a per-node rule a whole-value collapse gets
    // wrong.
    "  <!-- c -->x",
    "a<!-- c --> ",
    "<!-- c --> <!-- d -->",
    "A<!-- c -->B",
    // Entity-encoded text. MSBuild is self-inconsistent on whitespace here
    // (`&#32;` is kept, `&#x20;` is dropped), so those must degrade.
    "&#32;",
    "&#x20;",
    "&#9;",
    "&#160;",
    "&amp;",
    "a&amp;b",
    "&lt;x&gt;",
    // CDATA: content is verbatim, and adjacent literal whitespace is still
    // insignificant.
    "<![CDATA[ ]]>",
    "<![CDATA[x]]>",
    " <![CDATA[ ]]> ",
    "<![CDATA[a<b]]>",
    // Substitution, including into the shapes above.
    "$(Alpha)",
    "x$(Alpha)y",
    " $(Alpha) ",
    "$(Undefined)",
    "$(Alpha.Length)",
    "$([System.IO.Path]::Combine('a','$(Alpha)'))",
    // Percent escapes: MSBuild unescapes; raw text would be a wrong commit.
    "%20",
    "a%20b",
    "%2f",
    "100%",
    "a%zb",
    // Quote characters (inert in a body, but they flow into later expressions).
    "a'b",
    "a`b",
    "a\"b",
    // Non-ASCII, incl. a non-BMP scalar (UTF-16-unit semantics downstream).
    "caf\u{e9}",
    "o\u{17f}x",
    "\u{1d11e}",
];

fn xml_escape_body(body: &str) -> String {
    // The bodies are *already* XML source fragments (they contain comments,
    // CDATA and entities on purpose), so they are emitted verbatim. Only the
    // generator is trusted to keep them well-formed; a malformed one would make
    // both sides fail to parse, which the harness reports rather than hides.
    body.to_string()
}

/// Build a project document from `(name, body)` writes, in order.
fn project_xml(writes: &[(&str, &str)]) -> String {
    let mut xml = String::from("<Project>\n  <PropertyGroup>\n");
    for (name, body) in writes {
        xml.push_str(&format!("    <{name}>{}</{name}>\n", xml_escape_body(body)));
    }
    xml.push_str("  </PropertyGroup>\n</Project>\n");
    xml
}

/// The differential contract for one document. Returns the number of properties
/// we committed (for the anti-vacuity floor).
fn check_property_table_certain_implies_exact(oracle: &mut Oracle, xml: &str) -> usize {
    check_at_path(oracle, xml, None)
}

/// As above, but the document is evaluated *at a real path* by both sides, so
/// MSBuild's reserved path derivatives (`MSBuildProjectDirectory`, …) agree with
/// the ones our walker seeds — the only way to diff anything they feed.
fn check_property_table_at_path(oracle: &mut Oracle, xml: &str, dir: &Path) -> usize {
    check_at_path(oracle, xml, Some(dir))
}

fn check_at_path(oracle: &mut Oracle, xml: &str, dir: Option<&Path>) -> usize {
    let owned;
    let project_path: &Path = match dir {
        Some(dir) => {
            owned = dir.join("Demo.fsproj");
            &owned
        }
        None => Path::new("/repo/proj/Demo.fsproj"),
    };
    let Ok(parsed) = parse_fsproj(
        xml,
        project_path,
        &HashMap::new(),
        &common::oracle_environment(),
    ) else {
        // Malformed XML: both sides reject, nothing to compare.
        return 0;
    };
    let names: Vec<String> = NAMES.iter().map(|n| (*n).to_string()).collect();

    // The walker's contract, not a stricter one — and it is **per property**.
    // A name whose write leaned on an undefined reference, or on a body we
    // declined to model, is still stored (MSBuild's unset-is-empty rule means
    // we have *a* value), but the walker marks its provenance untrusted and
    // consumers degrade on it (`ParsedProject::property_provenance_untrusted`).
    // Those names make no claim.
    //
    // Everything *else* in the same document still does. Skipping the whole
    // document whenever any diagnostic fired — which is what this used to do —
    // hid exactly the interesting case: a degraded `Alpha` next to a clean but
    // wrongly-evaluated `Beta` was never compared at all.
    let ours: Vec<(String, String)> = names
        .iter()
        .filter(|n| !parsed.property_provenance_untrusted(n))
        .filter_map(|n| parsed.properties.get(n).map(|v| (n.clone(), v.clone())))
        .collect();

    let Some(theirs) = oracle.project(xml, &names, dir.map(|_| project_path)) else {
        assert!(
            ours.is_empty(),
            "certain-implies-exact violated: MSBuild rejects this project, but we \
             committed {ours:?}\n--- xml ---\n{xml}"
        );
        return 0;
    };

    for (name, our_value) in &ours {
        let their_value = theirs
            .get(name)
            .expect("oracle answers for every requested name");
        assert_eq!(
            our_value, their_value,
            "certain-implies-exact violated for ${{{name}}}: we evaluate it to \
             {our_value:?}, MSBuild to {their_value:?}\n--- xml ---\n{xml}"
        );
    }
    ours.len()
}

/// Every body in the alphabet, on its own — the corner list. A failure here
/// names the exact shape.
#[test]
fn every_body_shape_is_exact_or_declined() {
    let mut oracle = Oracle::spawn();
    let mut committed = 0usize;
    for body in BODIES {
        let xml = project_xml(&[("Alpha", body)]);
        committed += check_property_table_certain_implies_exact(&mut oracle, &xml);
    }
    eprintln!("body corners: {committed} of {} committed", BODIES.len());
    // Anti-vacuity: a harness that declined everything would pass while testing
    // nothing on the committed side.
    assert!(
        committed >= 15,
        "too few committed body shapes ({committed}) — the walker may have \
         started declining everything, which passes vacuously"
    );
}

/// Bodies that *read* earlier properties, so an XML-layer mistake in the first
/// write propagates into a second one's expansion — the way it does in a real
/// chain (`<P> </P>` then `'$(P)' == ''`).
///
/// This cross product is what caught the composed-escape wrong-commit: with
/// `Alpha=100%` (a body that is *individually* fine — a bare `%` is literal in
/// MSBuild) the reader `$(Alpha)$(Alpha)` yields `100%100%`, in which MSBuild
/// reads `%10` as an escape and unescapes it to U+0010. Neither ingredient is
/// suspicious alone, and the guard in `substitute` was originally written
/// against the *input* rather than the composed result, so it missed exactly
/// this. Keep both ingredients in their pools.
#[test]
fn bodies_reading_earlier_writes_are_exact_or_declined() {
    let mut oracle = Oracle::spawn();
    let readers = [
        "$(Alpha)",
        "[$(Alpha)]",
        "$(Alpha.Length)",
        // Splices `Alpha` twice, so a value ending in a bare `%` can compose an
        // escape across the boundary. See the doc comment.
        "$(Alpha)$(Alpha)",
    ];
    let mut committed = 0usize;
    for body in BODIES {
        for reader in readers {
            let xml = project_xml(&[("Alpha", body), ("Beta", reader)]);
            committed += check_property_table_certain_implies_exact(&mut oracle, &xml);
        }
    }
    eprintln!("reader pairs: {committed} committed");
    assert!(committed >= 30, "too few committed ({committed})");
}

/// Bodies that read the **reserved path derivatives**, with the project living
/// at adversarial paths — including a directory whose name literally contains
/// `%20`.
///
/// This is the dimension the in-memory checks structurally cannot see: they hand
/// our parser a fake path while MSBuild evaluates a pathless document, so
/// anything `$(MSBuildProjectDirectory)` feeds is incomparable. It matters
/// because a `%XX` in a path-derived value is *not* an escape — MSBuild stores
/// reserved values pre-escaped, so a project in `…/a%20b/` really does resolve
/// `$(MSBuildProjectDirectory)/Foo.fs` to `…/a%20b/Foo.fs`. A guard that scans
/// only the composed result would drop that item; a reviewer caught exactly that
/// on this branch, and this test is how the machine catches it next time.
#[test]
fn reserved_path_properties_are_exact_or_declined() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();
    let dirs = [
        tmp.path().join("plain"),
        // A literal percent-two-hex in the directory name.
        tmp.path().join("a%20b"),
        // A trailing bare `%`, so the *next* characters in a body can compose an
        // escape across the boundary — that one really is an escape.
        tmp.path().join("pct%"),
        tmp.path().join("dot.ted"),
        // The other **eight** reserved characters MSBuild escapes when it seeds
        // a reserved path (`EscapingUtilities.cs:310` — `% * ? @ $ ( ) ; '`).
        // Tracking only `%` is what made a `;` in the project's own directory a
        // live wrong commit: the seed is escaped, so the `;` cannot split an
        // item list, the parens cannot open an expression, and the `*`/`?`
        // cannot glob. A generator over document *text* can never find this —
        // the input lives in the project's path, not in its XML.
        tmp.path().join("semi;colon"),
        tmp.path().join("paren(s)"),
        tmp.path().join("star*glob"),
        tmp.path().join("quest?ion"),
        tmp.path().join("at@sign"),
        tmp.path().join("dollar$sign"),
        tmp.path().join("quo'te"),
    ];
    let readers = [
        "$(MSBuildProjectDirectory)/Foo.fs",
        "$(MSBuildProjectDirectory)20b",
        "$(MSBuildProjectName)",
        "$(MSBuildProjectDirectory)$(MSBuildProjectExtension)",
        "$(MSBuildProjectDirectory)/$(Alpha)",
    ];
    let mut committed = 0usize;
    for dir in &dirs {
        std::fs::create_dir_all(dir).unwrap();
        for reader in readers {
            for body in ["x", "100%", " ", "%20"] {
                let xml = project_xml(&[("Alpha", body), ("Beta", reader)]);
                committed += check_property_table_at_path(&mut oracle, &xml, dir);
            }
        }
    }
    eprintln!("reserved-path readers: {committed} committed");
    assert!(committed >= 10, "too few committed ({committed})");
}

/// **Anti-degradation.** Certain-implies-exact is a one-sided contract: it
/// catches a wrong *commit*, and a diagnostic silently withdraws the claim — so
/// a spurious *decline* passes every check above vacuously. That is a real
/// failure mode (it is what a reviewer caught on this branch: a `%XX` in the
/// project's own directory name is *not* an escape, but a result-only escape
/// scan degraded the item anyway, quietly losing a Compile item MSBuild
/// resolves fine).
///
/// So: shapes that must **commit** are asserted to commit, with MSBuild's value.
/// Shapes that must **degrade** are asserted to degrade. Both directions, or the
/// harness only measures half of what it claims to.
#[test]
fn reserved_path_shapes_commit_or_degrade_as_pinned() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();

    // (directory name, body of `Beta`, must-commit?, MSBuild's value — with
    // `{dir}` standing for the project directory).
    //
    // **Every row's expected value is asserted against MSBuild**, degrade rows
    // included. That is deliberate: my first attempt at this table pinned the
    // `pct%` row the wrong way round, from the model I had just built rather
    // than from the oracle, and it passed. An expectation I reasoned out is
    // exactly as untrustworthy as the code I reasoned out.
    let cases: &[(&str, &str, bool, &str)] = &[
        (
            "plain",
            "$(MSBuildProjectDirectory)/Foo.fs",
            true,
            "{dir}/Foo.fs",
        ),
        ("plain", "$(MSBuildProjectName)", true, "Demo"),
        // The percent sequence comes from the project's own directory name,
        // where MSBuild treats it as literal text (it stores reserved values
        // pre-escaped). We must commit, not degrade.
        (
            "a%20b",
            "$(MSBuildProjectDirectory)/Foo.fs",
            true,
            "{dir}/Foo.fs",
        ),
        ("a%20b", "$(MSBuildProjectDirectory)", true, "{dir}"),
        // Provenance follows the **percent**, not the whole sequence: a trusted
        // `%` cannot introduce an escape even when the XML supplies the two hex
        // digits after it, so this is the literal `…/pct%20b`.
        ("pct%", "$(MSBuildProjectDirectory)20b", true, "{dir}20b"),
        // An escape authored in the XML is unescaped at the point of use, and
        // we now model that rather than degrading: MSBuild yields `a b`, and so
        // do we — in a plain directory and in one whose own name carries a `%`
        // (whose escape is a *different* percent, and stays inert).
        ("plain", "a%20b", true, "a b"),
        ("a%20b", "a%20b", true, "a b"),
        // A bare `%` authored in the XML can still compose an escape with
        // XML-authored hex digits: `100%` + `20b` → `%20` → a space. Composition
        // happens inside the escaped domain, so the leaf decodes what MSBuild
        // decodes.
        ("plain", "$(Pct)20b", true, "100 b"),
        // Trust must survive being laundered through an ordinary property write
        // (`Base` holds the reserved directory): still literal, still committed.
        ("a%20b", "$(Base)/Foo.fs", true, "{dir}/Foo.fs"),
        ("pct%", "$(Base)/Foo.fs", true, "{dir}/Foo.fs"),
        // A property-function *result* is escaped by MSBuild on the way out, so
        // its percent cannot introduce an escape — the exact opposite of the
        // plain `$(Pct)20b` splice two rows above, on the very same value.
        ("plain", "$(Pct.ToString())20b", true, "100%20b"),
        ("plain", "$(Pct.TrimStart('z'))20b", true, "100%20b"),
        // `TrimEnd` is not in our dispatch table, so this declines for an
        // unrelated reason — pinned so the row is not mistaken for an escape
        // failure if someone adds the member later.
        ("plain", "$(Pct.TrimEnd('z'))20b", false, "100%20b"),
        // A string indexer's `Char` is the one expression result MSBuild does
        // *not* escape, so its `%` still composes with the XML's hex digits and
        // is unescaped. The domain models that as the single raw entrance
        // (`Escaped::push_unescaped_raw`), so the value is right rather than
        // declined — and treating every result as escaped would commit `%20b`.
        ("plain", "$(Pct[3])20b", true, " b"),
        ("plain", "$(Pct[3])", true, "%"),
        // A reserved character in the project's own directory survives the seed's
        // escape and the leaf's unescape unchanged. Note what this does *not*
        // prove: `-getProperty` unescapes at the read, so the old raw-text seed
        // agreed here too. The escape only becomes observable where the value is
        // *scanned* — an item spec splitting on `;` — which is why the guard for
        // that lives in `fsproj_msbuild_diff.rs`
        // (`a_reserved_character_in_the_project_directory_does_not_split_items`).
        // Kept anyway: it pins the seed → value round trip these rows depend on.
        ("semi;colon", "$(MSBuildProjectDirectory)", true, "{dir}"),
        ("paren(s)", "$(MSBuildProjectDirectory)", true, "{dir}"),
        ("star*glob", "$(MSBuildProjectDirectory)", true, "{dir}"),
    ];

    for (dir_name, body, must_commit, expected) in cases {
        let dir = tmp.path().join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        let project_path = dir.join("Demo.fsproj");
        // Preludes: `Pct` is XML-authored text ending in a bare `%` (so it *can*
        // compose an escape with XML hex digits), and `Base` launders a trusted
        // reserved value through an ordinary property write — whose provenance
        // must survive, or `$(Base)/Foo.fs` under `…/a%20b/` would wrongly
        // degrade (codex found precisely that).
        let xml = project_xml(&[
            ("Pct", "100%"),
            ("Base", "$(MSBuildProjectDirectory)"),
            ("Beta", body),
        ]);
        let parsed = parse_fsproj(
            &xml,
            &project_path,
            &HashMap::new(),
            &common::oracle_environment(),
        )
        .expect("well-formed");
        let names = vec!["Beta".to_string()];
        let theirs = oracle
            .project(&xml, &names, Some(&project_path))
            .expect("MSBuild evaluates these documents");
        let theirs = theirs.get("Beta").expect("oracle answers for Beta");

        // Every row's expectation is checked against MSBuild — including the
        // degrade rows, whose whole justification is that MSBuild's value is
        // *not* the raw text we would otherwise have committed.
        let want = expected.replace("{dir}", &dir.to_string_lossy());
        assert_eq!(
            theirs, &want,
            "in {dir_name}/, body {body:?}: the pinned MSBuild value is wrong"
        );

        let committed = parsed.diagnostics.is_empty() && parsed.properties.contains_key("Beta");
        assert_eq!(
            committed, *must_commit,
            "in {dir_name}/, body {body:?}: expected must_commit={must_commit}, got \
             committed={committed} (diagnostics: {:?})",
            parsed.diagnostics
        );
        if *must_commit {
            assert_eq!(
                parsed.properties.get("Beta").map(String::as_str),
                Some(theirs.as_str()),
                "in {dir_name}/, body {body:?}"
            );
        }
    }
}

/// Duplicate imports. MSBuild registers every performed import in a
/// per-evaluation seen-set (`Evaluator._importsSeen`,
/// `StringComparer.OrdinalIgnoreCase` over the lexically-normalised path —
/// **no symlink resolution**) *before* walking the imported file, and skips
/// any later import that resolves to a seen path with warning MSB4011
/// (MSB4210 when the target is the entry project). The evaluation succeeds,
/// so the walker must both skip the duplicate and stay non-partial:
/// re-running an imported body silently doubles an accumulator property
/// (`$(Order)a` → `aa` vs MSBuild's `a`) in a result that claims exactness.
///
/// Every cluster's expected value is read from the oracle evaluating the same
/// on-disk file cluster, never hand-pinned; each cluster gets its own
/// directory so the wildcard case cannot see its neighbours' props files.
#[test]
fn duplicate_imports_are_skipped_exactly() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();

    const ACCUMULATE_A: &str =
        r#"<Project><PropertyGroup><Order>$(Order)a</Order></PropertyGroup></Project>"#;
    let entry = |imports: &str| {
        format!(
            "<Project>\n{imports}\n  <PropertyGroup>\n    <R>[$(Order)]</R>\n  </PropertyGroup>\n</Project>"
        )
    };

    struct Cluster {
        name: &'static str,
        entry: String,
        files: Vec<(&'static str, &'static str)>,
    }
    let mut cases = vec![
        Cluster {
            name: "dup-segment",
            entry: entry(r#"  <Import Project="a.props;a.props" />"#),
            files: vec![("a.props", ACCUMULATE_A)],
        },
        Cluster {
            name: "repeated-elements",
            entry: entry("  <Import Project=\"a.props\" />\n  <Import Project=\"a.props\" />"),
            files: vec![("a.props", ACCUMULATE_A)],
        },
        Cluster {
            name: "wildcard-overlap",
            entry: entry(r#"  <Import Project="a.props;*.props" />"#),
            files: vec![("a.props", ACCUMULATE_A)],
        },
        Cluster {
            name: "respelt",
            entry: entry(r#"  <Import Project="a.props;sub/../a.props;A.PROPS" />"#),
            files: vec![("a.props", ACCUMULATE_A), ("sub/.keep", "")],
        },
        Cluster {
            name: "cycle",
            entry: entry(r#"  <Import Project="a.props" />"#),
            files: vec![
                (
                    "a.props",
                    "<Project>\n  <PropertyGroup><Order>$(Order)a</Order></PropertyGroup>\n  <Import Project=\"b.props\" />\n</Project>",
                ),
                (
                    "b.props",
                    "<Project>\n  <PropertyGroup><Order>$(Order)b</Order></PropertyGroup>\n  <Import Project=\"a.props\" />\n</Project>",
                ),
            ],
        },
        Cluster {
            name: "unicode-distinct",
            // Dotted capital İ vs plain i: .NET's OrdinalIgnoreCase keeps
            // the pair distinct (its ordinal casing table carves out the
            // Turkish-I family), so MSBuild imports both — and so do we,
            // with certainty, because Unicode's fold also keeps İ
            // distinct. Pins the fuzzy tier against over-widening.
            entry: entry("  <Import Project=\"\u{130}.props;i.props\" />"),
            files: vec![
                ("\u{130}.props", ACCUMULATE_A),
                (
                    "i.props",
                    r#"<Project><PropertyGroup><Order>$(Order)b</Order></PropertyGroup></Project>"#,
                ),
            ],
        },
        Cluster {
            name: "self-import",
            entry: "<Project>\n  <PropertyGroup><Order>$(Order)x</Order></PropertyGroup>\n  <Import Project=\"Demo.fsproj\" />\n  <PropertyGroup>\n    <R>[$(Order)]</R>\n  </PropertyGroup>\n</Project>".to_string(),
            files: vec![],
        },
    ];
    // A symlink alias is a *distinct* import — MSBuild's key never resolves
    // symlinks, so the body genuinely runs twice. Guards the dedup key's
    // domain: keying on the canonicalised path would skip this one and
    // diverge in the opposite direction.
    #[cfg(unix)]
    cases.push(Cluster {
        name: "symlink-alias",
        entry: entry(r#"  <Import Project="a.props;link.props" />"#),
        files: vec![("a.props", ACCUMULATE_A)],
    });

    for Cluster {
        name: cluster,
        entry: xml,
        files,
    } in &cases
    {
        let dir = tmp.path().join(cluster);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
        }
        #[cfg(unix)]
        if *cluster == "symlink-alias" {
            std::os::unix::fs::symlink(dir.join("a.props"), dir.join("link.props")).unwrap();
        }
        let project_path = dir.join("Demo.fsproj");
        // On disk for our walker too: the self-import cluster follows an
        // import *of the entry file itself*, which must be readable.
        std::fs::write(&project_path, xml).unwrap();

        let parsed = parse_fsproj_with_imports(
            xml,
            &project_path,
            &HashMap::new(),
            &common::oracle_environment(),
            None,
            None,
        )
        .expect("well-formed");
        let names = vec!["R".to_string()];
        let theirs = oracle
            .project(xml, &names, Some(&project_path))
            .expect("MSBuild evaluates every duplicate-import cluster (the skip is a warning)");
        let theirs = theirs.get("R").expect("oracle answers for R");

        // Must-commit, not just exact-if-committed: a walker that degraded
        // these shapes would pass a one-sided check vacuously, and MSBuild
        // treats every one of them as an ordinary clean evaluation.
        assert!(
            !parsed.is_partial,
            "{cluster}: duplicate imports are a clean skip in MSBuild, not a \
             degrade (diagnostics: {:?})",
            parsed.diagnostics
        );
        assert_eq!(
            parsed.properties.get("R").map(String::as_str),
            Some(theirs.as_str()),
            "{cluster}: certain-implies-exact violated (diagnostics: {:?})",
            parsed.diagnostics
        );
    }
}

/// Fixed-seed sweep over multi-write documents: several properties, adversarial
/// bodies, later ones reading earlier ones.
#[test]
fn fixed_seed_document_sweep() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0x5eed_1a7e_d0c5);

    const CASES: usize = 1500;
    let mut committed = 0usize;
    for _ in 0..CASES {
        let count = 1 + rng.below(4);
        let writes: Vec<(&str, &str)> = (0..count)
            .map(|i| (NAMES[i % NAMES.len()], *rng.pick(BODIES)))
            .collect();
        let xml = project_xml(&writes);
        committed += check_property_table_certain_implies_exact(&mut oracle, &xml);
    }
    eprintln!("document sweep: {committed} committed over {CASES} documents");
    assert!(
        committed >= 500,
        "too few committed ({committed}) over {CASES} documents"
    );
}
