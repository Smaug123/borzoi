//! Snapshot tests for `src/fsproj` against real `.fsproj` files vendored
//! from the F# compiler tree (see `tests/fixtures/fsproj-corpus/SOURCES.md`).
//!
//! Each test renders a deterministic textual summary of what the parser
//! produced — items in compile order, their `<Link>` metadata, project-
//! defined property names, and diagnostics — and compares it to a checked-in
//! `.snap` file. To refresh the snapshots after intentional parser changes
//! or after re-vendoring upstream sources, run:
//!
//! ```text
//! UPDATE_FSPROJ_SNAPSHOTS=1 cargo test --test fsproj_corpus
//! ```
//!
//! In addition to the snapshot match, each case asserts two invariants the
//! parser must hold on real code:
//!
//! 1. `ParsedProject::items` contains only Compile-flavoured items; project
//!    references stay in `ParsedProject::project_references`.
//! 2. No `Include` path appears twice within a single resolved config. The
//!    `FSharp.Core` Proto/Release pair is the motivating case: naive
//!    `[CompileBefore…, Compile…]` concatenation that ignored the Proto-vs-
//!    Release condition gates would duplicate `prim-types-prelude.fs`.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use borzoi_msbuild::{ItemKind, ParsedProject, ResolvedItem, parse_fsproj};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fsproj-corpus")
}

#[test]
fn assembly_check() {
    run_snapshot(
        "AssemblyCheck/AssemblyCheck.fsproj",
        &[],
        "AssemblyCheck.snap",
    );
}

#[test]
fn fslex() {
    run_snapshot("fslex/fslex.fsproj", &[], "fslex.snap");
}

#[test]
fn fsharp_core_proto() {
    run_snapshot(
        "FSharp.Core/FSharp.Core.fsproj",
        &[("Configuration", "Proto")],
        "FSharp.Core.Proto.snap",
    );
}

#[test]
fn fsharp_core_release() {
    run_snapshot(
        "FSharp.Core/FSharp.Core.fsproj",
        &[("Configuration", "Release")],
        "FSharp.Core.Release.snap",
    );
}

#[test]
fn fsharp_compiler_service_release() {
    run_snapshot(
        "FSharp.Compiler.Service/FSharp.Compiler.Service.fsproj",
        &[("Configuration", "Release")],
        "FSharp.Compiler.Service.Release.snap",
    );
}

fn run_snapshot(rel_fsproj: &str, extras: &[(&str, &str)], snap_name: &str) {
    let root = fixtures_root();
    let fsproj_path = root.join(rel_fsproj);
    let project_dir = fsproj_path
        .parent()
        .expect("fsproj path has a parent")
        .to_path_buf();
    let source = fs::read_to_string(&fsproj_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", fsproj_path.display()));

    let mut props: HashMap<String, String> = HashMap::new();
    for (k, v) in extras {
        props.insert((*k).into(), (*v).into());
    }
    let project = parse_fsproj(&source, &fsproj_path, &props, &HashMap::new())
        .unwrap_or_else(|e| panic!("parse {}: {e}", fsproj_path.display()));

    assert_compile_items_only(&project.items, rel_fsproj);
    assert_no_duplicate_includes(&project.items, &project_dir, rel_fsproj);
    assert_no_duplicate_includes(&project.project_references, &project_dir, rel_fsproj);

    let actual = render_snapshot(&project, &project_dir);
    let snap_path = root.join(snap_name);
    compare_or_bless(&actual, &snap_path);
}

/// `ParsedProject::items` is documented to hold only Compile-flavoured
/// inputs. It is no longer rank-checkable by `ItemKind`: F#'s
/// `CompileOrder` metadata can put a `<Compile CompileOrder="CompileFirst">`
/// before explicit `<CompileBefore>` items while preserving `kind=Compile`
/// as provenance.
fn assert_compile_items_only(items: &[ResolvedItem], fixture: &str) {
    for item in items {
        if item.kind == ItemKind::ProjectReference {
            panic!(
                "{fixture}: ProjectReference appeared in ParsedProject::items — it belongs in project_references"
            );
        }
    }
}

fn assert_no_duplicate_includes(items: &[ResolvedItem], project_dir: &Path, fixture: &str) {
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for item in items {
        let rel = relative_to(&item.include, project_dir);
        if !seen.insert(rel.clone()) {
            panic!(
                "{fixture}: duplicate Include {} within a single resolved config",
                rel.display()
            );
        }
    }
}

fn render_snapshot(project: &ParsedProject, project_dir: &Path) -> String {
    let mut out = String::new();
    writeln!(out, "is_partial: {}", project.is_partial).unwrap();

    let mut prop_names: Vec<&String> = project.properties.keys().collect();
    prop_names.sort();
    writeln!(out, "properties ({}):", prop_names.len()).unwrap();
    for name in prop_names {
        writeln!(out, "  {name}").unwrap();
    }

    writeln!(out, "items ({}):", project.items.len()).unwrap();
    for item in &project.items {
        let rel = relative_to(&item.include, project_dir);
        let kind = match item.kind {
            ItemKind::CompileBefore => "CompileBefore",
            ItemKind::Compile => "Compile      ",
            ItemKind::CompileAfter => "CompileAfter ",
            ItemKind::ProjectReference => {
                unreachable!("ProjectReference must not appear in ParsedProject::items")
            }
        };
        let path = to_forward_slashes(&rel);
        match &item.link {
            Some(link) => writeln!(out, "  {kind} {path} link={link}").unwrap(),
            None => writeln!(out, "  {kind} {path}").unwrap(),
        }
    }

    writeln!(
        out,
        "project_references ({}):",
        project.project_references.len()
    )
    .unwrap();
    for item in &project.project_references {
        let rel = relative_to(&item.include, project_dir);
        let path = to_forward_slashes(&rel);
        writeln!(out, "  {path}").unwrap();
    }

    writeln!(
        out,
        "package_references ({}, uncertain={}):",
        project.package_references.len(),
        project.package_references_uncertain
    )
    .unwrap();
    for pr in &project.package_references {
        let version = pr.version.as_deref().unwrap_or("<none>");
        writeln!(out, "  {:?} {} version={version}", pr.op, pr.id).unwrap();
    }

    writeln!(
        out,
        "framework_references ({}):",
        project.framework_references.len()
    )
    .unwrap();
    for fr in &project.framework_references {
        writeln!(out, "  {}", fr.name).unwrap();
    }

    writeln!(out, "diagnostics ({}):", project.diagnostics.len()).unwrap();
    for diag in &project.diagnostics {
        writeln!(out, "  {:?}", diag.kind).unwrap();
    }

    out
}

/// Strip the project directory prefix from a parser-produced include path.
/// `PathBuf::join` doesn't normalise `..`, so `<dir>/../X` survives intact;
/// `strip_prefix` removes only the literal prefix, leaving `../X` visible
/// — which is what we want in the snapshot.
fn relative_to(path: &Path, project_dir: &Path) -> PathBuf {
    path.strip_prefix(project_dir)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Render with forward slashes so the snapshot is byte-identical on Unix
/// and Windows.
fn to_forward_slashes(path: &Path) -> String {
    let s = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        s.into_owned()
    } else {
        s.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

fn compare_or_bless(actual: &str, snap_path: &Path) {
    if std::env::var_os("UPDATE_FSPROJ_SNAPSHOTS").is_some() {
        fs::write(snap_path, actual)
            .unwrap_or_else(|e| panic!("write {}: {e}", snap_path.display()));
        return;
    }
    let expected = match fs::read_to_string(snap_path) {
        Ok(s) => s,
        Err(_) => panic!(
            "snapshot {} missing — run `UPDATE_FSPROJ_SNAPSHOTS=1 cargo test --test fsproj_corpus` to create it",
            snap_path.display()
        ),
    };
    if actual != expected {
        panic!(
            "snapshot mismatch at {}\n\
             --- expected ---\n{expected}\n\
             --- actual ---\n{actual}\n\
             run `UPDATE_FSPROJ_SNAPSHOTS=1 cargo test --test fsproj_corpus` to bless",
            snap_path.display(),
        );
    }
}
