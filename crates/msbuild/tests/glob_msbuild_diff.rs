//! Differential: the shipped glob resolver ([`glob_resolver::resolve`], wired
//! into [`parse_fsproj_with_imports`] exactly as the LSP wires it) against the
//! real MSBuild evaluator (the oracle's `items` op), comparing the **ordered**
//! `Compile` list.
//!
//! ## Why order, and why it is checkable
//!
//! F# compiles files in `Compile` order, and the LSP folds name resolution over
//! that order, so the order *is* the product: a resolver that selected the right
//! files in the wrong order would resolve names against the wrong prefix of the
//! project. MSBuild's order within one wildcard fragment is not the
//! filesystem's: `EngineFileUtilities.GetFileList` sorts each fragment's matches
//! with `StringComparer.OrdinalIgnoreCase` ("for determinism") before returning
//! them, and fragments concatenate in document order. So the order is a
//! function of the names alone, and this harness asserts it exactly.
//!
//! ## The asserted property
//!
//! Certain implies exact: whenever our parse commits (the Compile capture is not
//! marked uncertain), our ordered `Compile` paths equal MSBuild's. A decline
//! makes no claim. Hand-picked corners must commit, and so must every generated
//! case whose patterns cannot meet a case variant of a generated name: MSBuild's
//! case behaviour is host-dependent, so only those may decline.
//!
//! The generated file names mix case, digits, `_` and `-` — exactly the
//! characters where an ordinal sort and an `OrdinalIgnoreCase` sort disagree
//! (`_` sits between the upper- and lower-case letters; `-` and `.` and `/`
//! below both). Inputs are a fixed-seed sweep, so a failure reproduces exactly.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use borzoi_msbuild::{GlobResolver, ItemKind, glob_resolver, parse_fsproj_with_imports};
use common::{Oracle, SplitMix64};
use tempfile::TempDir;

/// Escape `s` for an XML attribute value.
fn xml_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn project_xml(include: &str, exclude: Option<&str>) -> String {
    let exclude = exclude
        .map(|e| format!(" Exclude=\"{}\"", xml_attr(e)))
        .unwrap_or_default();
    format!(
        "<Project>\n  <ItemGroup>\n    <Compile Include=\"{}\"{exclude} />\n  </ItemGroup>\n</Project>\n",
        xml_attr(include)
    )
}

/// Lexical normalisation, so `dir/./a.fs` and `dir/a.fs` compare equal without
/// touching the filesystem (a literal include need not exist).
fn lexical(path: &Path) -> String {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.to_string_lossy().replace('\\', "/")
}

/// Our ordered `Compile` list, or the reason we declined to commit one.
fn ours(project: &Path, xml: &str) -> Result<Vec<String>, String> {
    let glob: &GlobResolver<'_> = &glob_resolver::resolve;
    let parsed = parse_fsproj_with_imports(
        xml,
        project,
        &HashMap::new(),
        &common::oracle_environment(),
        None,
        Some(glob),
    )
    .expect("well-formed XML parses");
    if parsed.items_uncertain {
        let reason = parsed
            .compile_item_uncertainties
            .first()
            .map(|c| format!("{:?}", c.kind))
            .unwrap_or_else(|| "items_uncertain with no recorded cause".to_string());
        return Err(reason);
    }
    Ok(parsed
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Compile)
        .map(|item| lexical(&item.include))
        .collect())
}

fn theirs(oracle: &mut Oracle, project: &Path, xml: &str) -> Vec<String> {
    oracle
        .items(xml, project, "Compile", &[])
        .expect("MSBuild evaluates these documents")
        .iter()
        .map(|p| lexical(Path::new(p)))
        .collect()
}

/// Lay `files` down under `dir` (with parent directories).
fn lay_down(dir: &Path, files: &[&str]) {
    for rel in files {
        let full = dir.join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, b"module M\n").unwrap();
    }
}

/// One hand-picked case: evaluate `include`/`exclude` from `project_dir` both
/// ways and require an exact, committed match.
fn assert_matches_msbuild(
    oracle: &mut Oracle,
    project_dir: &Path,
    include: &str,
    exclude: Option<&str>,
) {
    let xml = project_xml(include, exclude);
    let project = project_dir.join("Test.fsproj");
    std::fs::write(&project, &xml).unwrap();
    let theirs = theirs(oracle, &project, &xml);
    let ours = ours(&project, &xml).unwrap_or_else(|reason| {
        panic!("declined a case it must commit: include={include:?} exclude={exclude:?}: {reason}")
    });
    assert_eq!(
        ours, theirs,
        "ordered Compile list diverges from MSBuild for include={include:?} exclude={exclude:?}"
    );
}

fn canonical_tempdir() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    (tmp, root)
}

const TREE: &[&str] = &[
    "a.fs",
    "b.fs",
    "m.fs",
    "z.fs",
    "sub/c.fs",
    "sub/deep/d.fs",
    "sub/e.fsi",
];

/// Names where ordinal and `OrdinalIgnoreCase` order disagree.
const MIXED_CASE_TREE: &[&str] = &[
    "Zeta.fs",
    "alpha.fs",
    "Beta.fs",
    "_under.fs",
    "0digit.fs",
    "ab-c.fs",
    "ab.fs",
    "ab/x.fs",
    "Sub/q.fs",
    "sub2/r.fs",
];

#[test]
fn hand_picked_corners() {
    let mut oracle = Oracle::spawn();
    let simple: &[(&str, Option<&str>)] = &[
        ("**/*.fs", None),
        ("*.fs", None),
        ("sub/*.fs", None),
        ("**/*.fsi", None),
        // MSBuild keeps duplicates across overlapping fragments.
        ("a.fs;*.fs", None),
        ("*.fs;?.fs", None),
        ("**/*.fs", Some("sub/**/*.fs")),
        ("*.fs", Some("b.fs")),
        ("?.fs", None),
    ];
    for (include, exclude) in simple {
        let (_tmp, root) = canonical_tempdir();
        lay_down(&root, TREE);
        assert_matches_msbuild(&mut oracle, &root, include, *exclude);
    }

    // A relative recursive Include with an *absolute* Exclude — the form
    // MSBuild produces from `$(MSBuildProjectDirectory)/sub/c.fs`.
    {
        let (_tmp, root) = canonical_tempdir();
        lay_down(&root, TREE);
        let abs = root.join("sub/c.fs").to_string_lossy().into_owned();
        assert_matches_msbuild(&mut oracle, &root, "**/*.fs", Some(&abs));
    }

    // A glob rooted above the project directory enumerates the sibling and
    // not the project-local decoy. An *absolute* Exclude does not cross-match
    // a `..` include (MSBuild matches it in the include's relative frame); a
    // relative one in that frame does.
    {
        let (_tmp, root) = canonical_tempdir();
        lay_down(&root, &["shared/a.fs", "shared/b.fs", "proj/local.fs"]);
        let proj = root.join("proj");
        let abs = root.join("shared/a.fs").to_string_lossy().into_owned();
        assert_matches_msbuild(&mut oracle, &proj, "../shared/*.fs", None);
        assert_matches_msbuild(&mut oracle, &proj, "../shared/*.fs", Some(&abs));
        assert_matches_msbuild(&mut oracle, &proj, "../shared/*.fs", Some("../shared/a.fs"));
    }

    // A `*` in the project directory's *name* is literal, not a wildcard
    // matching the sibling `axb/proj`. (Unix only: Windows rejects the name.)
    #[cfg(unix)]
    {
        let (_tmp, root) = canonical_tempdir();
        lay_down(&root, &["a*b/proj/real.fs", "axb/proj/decoy.fs"]);
        assert_matches_msbuild(&mut oracle, &root.join("a*b/proj"), "*.fs", None);
    }

    // A non-ASCII name *above* the glob root is shared by every match, so it
    // cannot decide their order: the case must commit.
    {
        let (_tmp, root) = canonical_tempdir();
        lay_down(
            &root,
            &["José/app/b.fs", "José/app/a.fs", "José/app/sub/c.fs"],
        );
        let proj = root.join("José/app");
        assert_matches_msbuild(&mut oracle, &proj, "*.fs", None);
        assert_matches_msbuild(&mut oracle, &proj, "**/*.fs", None);
    }

    // The order MSBuild imposes is `OrdinalIgnoreCase` over the matched
    // relative paths, not ordinal and not the filesystem's.
    for (include, exclude) in [("**/*.fs", None), ("*.fs", None), ("*/*.fs;*.fs", None)] {
        let (_tmp, root) = canonical_tempdir();
        lay_down(&root, MIXED_CASE_TREE);
        assert_matches_msbuild(&mut oracle, &root, include, exclude);
    }
}

/// Name stems for generated files: case, digits and the punctuation that
/// separates the two orders.
const STEMS: &[&str] = &[
    "a", "A", "b", "B", "z", "Z", "ab", "Ab", "aB", "ab-c", "ab_c", "_x", "0", "9z", "m1", "m10",
    "m2", "Lib", "lib2",
];
const DIRS: &[&str] = &["", "", "", "sub/", "Sub2/", "ab/", "_d/", "sub/deep/", "Z/"];
const EXTS: &[&str] = &[".fs", ".fs", ".fs", ".fsi"];

const INCLUDES: &[&str] = &[
    "**/*.fs", "*.fs", "**/*", "sub/*.fs", "*/*.fs", "**/a*.fs", "**/?.fs", "**/*.fsi", "ab*.fs",
    "**/m*.fs", "a.fs",
];
const EXCLUDES: &[&str] = &["", "", "", "sub/**", "*.fsi", "**/_*", "a.fs", "**/Z/**"];

/// The patterns above whose verdict on some generated name changes when
/// ASCII case is folded: a literal `a` meets the stems `A`, `Ab` and `aB`.
/// MSBuild's case behaviour depends on the host, so the resolver must decline
/// those cases; every other generated case is decidable and must commit.
/// (`sub` never meets a case variant of itself — the only other spelling is
/// `Sub2` — and no stem starts with `M` or a directory is named `z`.)
const CASE_SENSITIVE_PATTERNS: &[&str] = &["**/a*.fs", "ab*.fs", "a.fs"];

#[test]
fn generated_order_matches_msbuild() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0x6c6f_6261_6c5f_6f72);
    const CASES: usize = 250;
    let mut committed = 0usize;
    let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
    let mut failures = Vec::new();

    for case in 0..CASES {
        let (_tmp, tmp_root) = canonical_tempdir();
        // Every other project sits under a non-ASCII directory: text shared by
        // every match must not make an otherwise decidable case decline.
        let root = tmp_root.join(if case % 2 == 0 { "Prój" } else { "proj" });
        std::fs::create_dir_all(&root).unwrap();
        let file_count = 3 + rng.below(8);
        let mut files: Vec<String> = Vec::new();
        for _ in 0..file_count {
            let name = format!("{}{}{}", rng.pick(DIRS), rng.pick(STEMS), rng.pick(EXTS));
            // A case-insensitive filesystem cannot hold two names differing
            // only in case; skip the second so the tree is the same on every
            // host.
            if files.iter().any(|f| f.eq_ignore_ascii_case(&name)) {
                continue;
            }
            files.push(name);
        }
        let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();
        lay_down(&root, &file_refs);
        // Symbolic links to directories, which MSBuild follows: one to a
        // sibling tree outside the project, one across the project's own tree.
        #[cfg(unix)]
        {
            if case % 3 == 0 {
                let ext = tmp_root.join("ext");
                lay_down(&ext, &["e1.fs", "E2.fs", "deep/e3.fsi"]);
                std::os::unix::fs::symlink(&ext, root.join("lnk")).unwrap();
            }
            if case % 5 == 0 && root.join("sub").is_dir() {
                std::fs::create_dir_all(root.join("_d")).unwrap();
                std::os::unix::fs::symlink(root.join("sub"), root.join("_d/to_sub")).unwrap();
            }
        }

        let fragments = 1 + rng.below(3);
        let include: Vec<&str> = (0..fragments).map(|_| *rng.pick(INCLUDES)).collect();
        // A literal include is not globbed, so only an exclude can make its
        // verdict depend on case.
        let case_sensitive = include
            .iter()
            .any(|f| f.contains('*') && CASE_SENSITIVE_PATTERNS.contains(f));
        let include = include.join(";");
        let exclude = *rng.pick(EXCLUDES);
        let case_sensitive = case_sensitive || CASE_SENSITIVE_PATTERNS.contains(&exclude);
        let exclude = (!exclude.is_empty()).then_some(exclude);

        let xml = project_xml(&include, exclude);
        let project = root.join("Test.fsproj");
        std::fs::write(&project, &xml).unwrap();
        let theirs = theirs(&mut oracle, &project, &xml);
        match ours(&project, &xml) {
            Ok(ours) if ours == theirs => committed += 1,
            Ok(ours) => failures.push(format!(
                "case {case}: files={files:?} include={include:?} exclude={exclude:?}\n  \
                 ours:   {ours:?}\n  theirs: {theirs:?}"
            )),
            Err(reason) if case_sensitive => *reasons.entry(reason).or_default() += 1,
            Err(reason) => failures.push(format!(
                "case {case}: files={files:?} include={include:?} exclude={exclude:?}\n  \
                 declined a decidable case: {reason}"
            )),
        }
    }

    eprintln!("glob order diff: {committed}/{CASES} committed; declines: {reasons:#?}");
    assert!(
        failures.is_empty(),
        "certain-implies-exact violated in {} case(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
    // The per-case obligation above (a decidable case must commit) is the
    // real floor; this one keeps the generator honest — a sweep that drew
    // case-sensitive patterns nearly every time would test little.
    assert!(
        committed * 4 >= CASES * 3,
        "only {committed}/{CASES} generated cases were decidable"
    );
}
