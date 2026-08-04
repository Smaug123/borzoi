//! Differential test: the two **filesystem-probing** path intrinsics,
//! `$([MSBuild]::GetPathOfFileAbove(...))` and
//! `$([MSBuild]::GetDirectoryNameOfFileAbove(...))`, swept over real directory
//! trees against the real evaluator.
//!
//! ## Why this needs its own harness
//!
//! Every other property differential here is structurally blind to these two
//! functions. `property_expr_diff` and `condition_diff` drive the oracle's
//! `eval`/`expand` ops, which build an *in-memory* stub with no path — so
//! `MSBuildThisFileDirectory` is not anchored to a directory anyone controls,
//! and there is nothing on disk to probe. `fsproj_property_table_diff` does hand
//! MSBuild a document, but through `parse_fsproj` — the **pure** surface, whose
//! documented contract is "no filesystem access", so our side declines both
//! functions unconditionally and every row would agree vacuously.
//!
//! What is left is this shape: materialise a real multi-level tree, evaluate it
//! through `parse_fsproj_with_imports` (the fs-enabled surface the LSP actually
//! uses), and ask MSBuild for the same document at the same path. Both sides
//! then walk the same filesystem, so host semantics — symlinks, case folding —
//! are shared by construction rather than modelled.
//!
//! ## The axes
//!
//! Three, crossed: **where the marker file is** (a subset of four nested
//! directory levels — 16 placements, including none and all), **how the
//! starting directory is spelled** (including the `..`-bearing and
//! nonexistent-hop forms, an empty one, and a relative one), and **which
//! intrinsic** is called (with the argument order mirrored between them, and
//! `GetPathOfFileAbove`'s one-argument overload as its own form).
//!
//! ## What it found
//!
//! Written with the `GetPathOfFileAbove` implementation, and it covers the two
//! wrong commits that motivated it — both of which every existing harness
//! passed:
//!
//! - `GetDirectoryNameOfFileAbove` walked the **raw** starting directory, where
//!   MSBuild `Path.GetFullPath`s it first. With a `..` in the start — i.e. the
//!   `$(MSBuildThisFileDirectory)../` idiom the SDK and users both write — the
//!   `..` survived as a literal component the walk could pop back down through,
//!   reaching a directory MSBuild never visits and committing it where MSBuild
//!   returns empty.
//! - the import-attribute special case this replaced joined an **empty**
//!   starting directory onto the current file's directory and committed the
//!   result, where MSBuild raises MSB4184.
//!
//! Both are ordinary certain-implies-exact violations once the axis is swept;
//! neither needed a new contract, only a harness that puts a real tree under the
//! evaluation.
//!
//! ## The contract
//!
//! The crate's usual one. We commit `R` with trusted provenance ⟹ MSBuild
//! evaluates the same document at the same path to the byte-identical value; we
//! decline ⟹ no claim; MSBuild rejects the document ⟹ we committed nothing.

mod common;

use std::collections::HashMap;
use std::path::Path;

use borzoi_msbuild::parse_fsproj_with_imports;
use common::Oracle;
use tempfile::TempDir;

/// The nested levels, outermost first. The project file lives in the innermost;
/// a marker may be placed at any subset of them.
const LEVELS: &[&str] = &["", "a", "a/b", "a/b/c"];

/// The file the intrinsics search for. Deliberately *not* `Directory.Build.props`:
/// that name has its own implicit-import machinery, which would make a failure
/// here ambiguous between the probe and the splice.
const MARKER: &str = "marker.props";

/// How the starting directory is written, as MSBuild source. The project sits in
/// `a/b/c`, so `$(MSBuildThisFileDirectory)` is that directory with a trailing
/// separator.
const STARTS: &[(&str, &str)] = &[
    ("this-file-dir", "$(MSBuildThisFileDirectory)"),
    // The idiom that exposed the un-normalised walk: MSBuild collapses the `..`
    // lexically before searching, so `a/b/c/../` searches `a/b` and upward and
    // can never reach `a/b/c`.
    ("parent-slash", "$(MSBuildThisFileDirectory)../"),
    ("parent-bare", "$(MSBuildThisFileDirectory).."),
    ("grandparent", "$(MSBuildThisFileDirectory)../../"),
    // A hop through a directory that does not exist: the walk is lexical, so
    // this is the same search as `parent-slash`.
    (
        "nonexistent-hop",
        "$(MSBuildThisFileDirectory)../nosuch/../",
    ),
    ("nonexistent-leaf", "$(MSBuildThisFileDirectory)nosuch/"),
    // MSB4184 on `GetPathOfFileAbove`: "The value cannot be an empty string."
    ("empty", ""),
    // Resolved against the MSBuild *process* working directory, which we do not
    // receive — so this must decline rather than guess a base.
    ("relative", ".."),
];

/// Which intrinsic to call, and how. `Path`/`Dir` take the starting directory
/// from [`STARTS`]; `PathDefaulted` exercises the one-argument overload, whose
/// start is implicitly the containing file's directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Path,
    Dir,
    PathDefaulted,
    /// `GetPathOfFileAbove` with a `file` that is not a bare filename: MSB4184,
    /// "can only be a file name and cannot include a directory".
    PathDirectoryComponent,
}

impl Form {
    fn expression(self, start: &str) -> String {
        match self {
            // Note the mirrored argument order between the two intrinsics.
            Form::Path => format!("$([MSBuild]::GetPathOfFileAbove('{MARKER}', '{start}'))"),
            Form::Dir => {
                format!("$([MSBuild]::GetDirectoryNameOfFileAbove('{start}', '{MARKER}'))")
            }
            Form::PathDefaulted => format!("$([MSBuild]::GetPathOfFileAbove('{MARKER}'))"),
            Form::PathDirectoryComponent => {
                format!("$([MSBuild]::GetPathOfFileAbove('sub/{MARKER}', '{start}'))")
            }
        }
    }

    /// The start axis is meaningless for the forms that do not take one; running
    /// them once per placement rather than once per start keeps the sweep honest
    /// about how many distinct questions it asks.
    fn takes_a_start(self) -> bool {
        !matches!(self, Form::PathDefaulted)
    }
}

fn project_xml(expression: &str) -> String {
    format!(
        "<Project>\n  <PropertyGroup>\n    <R>{expression}</R>\n  </PropertyGroup>\n</Project>\n"
    )
}

/// Materialise the tree for one marker placement. `placement` is a bitmask over
/// [`LEVELS`]; bit *i* set means the marker exists at that level. Returns the
/// path of the project file, which always lives in the innermost level.
fn build_tree(root: &Path, placement: usize) -> std::path::PathBuf {
    if root.exists() {
        std::fs::remove_dir_all(root).expect("clear case directory");
    }
    for (index, level) in LEVELS.iter().enumerate() {
        let dir = if level.is_empty() {
            root.to_path_buf()
        } else {
            root.join(level)
        };
        std::fs::create_dir_all(&dir).expect("create level");
        if placement & (1 << index) != 0 {
            std::fs::write(dir.join(MARKER), "<Project />").expect("write marker");
        }
    }
    // A `sub/` directory holding the marker, so the directory-component form is
    // rejected for naming a real file rather than merely a missing one.
    let sub = root.join(LEVELS[LEVELS.len() - 1]).join("sub");
    std::fs::create_dir_all(&sub).expect("create sub");
    std::fs::write(sub.join(MARKER), "<Project />").expect("write sub marker");
    root.join(LEVELS[LEVELS.len() - 1]).join("Demo.fsproj")
}

/// Evaluate one case on both sides. `None` on our side means we declined; `None`
/// on MSBuild's means it rejected the document.
fn evaluate(
    oracle: &mut Oracle,
    project_path: &Path,
    xml: &str,
) -> (Option<String>, Option<String>) {
    std::fs::write(project_path, xml).expect("write project");
    let parsed = parse_fsproj_with_imports(
        xml,
        project_path,
        &HashMap::new(),
        &common::oracle_environment(),
        None,
        None,
    )
    .expect("well-formed XML parses");
    let ours = if parsed.property_provenance_untrusted("R") {
        None
    } else {
        parsed.properties.get("R").cloned()
    };
    let theirs = oracle
        .project(xml, &["R".to_string()], Some(project_path), &[])
        .map(|t| t["R"].clone());
    (ours, theirs)
}

/// Certain-implies-exact for both file-above intrinsics, over marker placement ×
/// start spelling × call form.
#[test]
fn file_above_intrinsics_are_exact_or_declined() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("case");

    let mut committed_found = 0usize;
    let mut committed_empty = 0usize;
    let mut declined = 0usize;
    let mut msbuild_rejected = 0usize;
    let mut divergences: Vec<String> = Vec::new();

    for placement in 0..(1usize << LEVELS.len()) {
        let project_path = build_tree(&root, placement);
        for form in [
            Form::Path,
            Form::Dir,
            Form::PathDefaulted,
            Form::PathDirectoryComponent,
        ] {
            let starts: Vec<&(&str, &str)> = if form.takes_a_start() {
                STARTS.iter().collect()
            } else {
                vec![&STARTS[0]]
            };
            for (start_name, start) in starts {
                let expression = form.expression(start);
                let xml = project_xml(&expression);
                let (ours, theirs) = evaluate(&mut oracle, &project_path, &xml);
                let label = format!("placement={placement:04b} {form:?} start={start_name}");
                match (&ours, &theirs) {
                    (Some(ours), Some(theirs)) if ours == theirs => {
                        if ours.is_empty() {
                            committed_empty += 1;
                        } else {
                            committed_found += 1;
                        }
                    }
                    (Some(ours), Some(theirs)) => divergences.push(format!(
                        "{label}: committed {ours:?}, MSBuild says {theirs:?}"
                    )),
                    (Some(ours), None) => divergences.push(format!(
                        "{label}: committed {ours:?}, MSBuild rejects the document"
                    )),
                    (None, Some(_)) => declined += 1,
                    (None, None) => {
                        declined += 1;
                        msbuild_rejected += 1;
                    }
                }
            }
        }
    }

    assert!(
        divergences.is_empty(),
        "{} divergence(s):\n{}",
        divergences.len(),
        divergences.join("\n")
    );

    // A certain-implies-exact assertion is satisfied by a sweep that declines
    // everything. Pin that each outcome is actually reached — in particular that
    // the walk really *finds* files, since a sweep where every probe missed
    // would agree on `""` everywhere and never exercise the search at all.
    assert!(
        committed_found >= 40,
        "the sweep must commit real found paths; only {committed_found} did"
    );
    assert!(
        committed_empty >= 10,
        "the sweep must commit exhausted searches; only {committed_empty} did"
    );
    assert!(
        declined > 0 && msbuild_rejected > 0,
        "the sweep must reach both declines ({declined}) and MSBuild rejections ({msbuild_rejected})"
    );
}
