//! End-to-end smoke test for `src/fsproj` against a tiny in-tree fsproj.
//! The project is `<Project Sdk="Microsoft.NET.Sdk">`-style — phase 7a
//! defers SDK resolution, so we expect exactly one diagnostic (the
//! SDK shorthand) and the explicit Compile item. Larger corpus walks
//! against vendored F# compiler fsproj live in `tests/fsproj_corpus.rs`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use borzoi_msbuild::{DiagnosticKind, ItemKind, parse_fsproj};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `crates/lsp/`; `tools/` sits at the workspace
    // root, two directories up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root parent")
        .to_path_buf()
}

#[test]
fn fcs_dump_fsproj_extracts_single_program_fs() {
    let path = repo_root().join("tools/fcs-dump/fcs-dump.fsproj");
    let source = fs::read_to_string(&path).expect("fcs-dump.fsproj exists");
    let project =
        parse_fsproj(&source, &path, &HashMap::new(), &HashMap::new()).expect("well-formed XML");

    // The fixture is SDK-style (`<Project Sdk="Microsoft.NET.Sdk">`),
    // so phase 7a flags the SDK shorthand as unsupported and reports
    // the project as partial. Everything else about the walk should
    // be clean: one explicit Compile, no other diagnostics.
    let kinds: Vec<&DiagnosticKind> = project.diagnostics.iter().map(|d| &d.kind).collect();
    assert_eq!(
        kinds,
        [&DiagnosticKind::UnsupportedConstruct {
            element: "Project Sdk=\"Microsoft.NET.Sdk\"".to_string()
        }],
        "expected exactly the SDK shorthand diagnostic, got: {:?}",
        project.diagnostics
    );
    assert!(project.is_partial);

    let expected = repo_root().join("tools/fcs-dump/Program.fs");
    let actual: Vec<&Path> = project.items.iter().map(|i| i.include.as_path()).collect();
    assert_eq!(actual, [expected.as_path()]);
    assert_eq!(project.items[0].kind, ItemKind::Compile);
    assert!(project.items[0].link.is_none());
}
