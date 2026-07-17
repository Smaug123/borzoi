//! Tests for the `workspace/symbol` handler.
//!
//! Drive `handle` directly with a populated `State` (the JSON-RPC round-trip
//! lives in `lsp_integration.rs`). These pin the algorithm: the search set is
//! the owning projects of the open source buffers; symbols are the same
//! top-level exports `textDocument/documentSymbol` surfaces, filtered by a
//! case-insensitive substring `query`.

use std::fs;
use std::path::Path;

use borzoi::handlers::workspace_symbol;
use borzoi::server::State;
use lsp_types::{
    PartialResultParams, SymbolInformation, SymbolKind, Url, WorkDoneProgressParams,
    WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use tempfile::TempDir;

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn params(query: &str) -> WorkspaceSymbolParams {
    WorkspaceSymbolParams {
        query: query.to_string(),
        partial_result_params: PartialResultParams::default(),
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

/// Run the handler and unwrap the `Flat` response shape it always returns.
fn run(state: &mut State, query: &str) -> Vec<SymbolInformation> {
    match workspace_symbol::handle(state, params(query)) {
        Some(WorkspaceSymbolResponse::Flat(symbols)) => symbols,
        Some(WorkspaceSymbolResponse::Nested(_)) => panic!("expected the Flat response shape"),
        None => panic!("handler must always return Some, never an error envelope"),
    }
}

fn names(symbols: &[SymbolInformation]) -> Vec<&str> {
    let mut out: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    out.sort_unstable();
    out
}

/// A two-file project with both files open in the editor.
fn two_file_project() -> (TempDir, State, Url, Url) {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    let a = tmp.path().join("A.fs");
    let b = tmp.path().join("B.fs");
    write(
        &proj,
        r#"<Project>
          <ItemGroup>
            <Compile Include="A.fs" />
            <Compile Include="B.fs" />
          </ItemGroup>
        </Project>"#,
    );
    let a_src = "module Shared\nlet foo = 1\n";
    let b_src = "module Other\nlet bar = 2\n";
    write(&a, a_src);
    write(&b, b_src);

    let a_uri = Url::from_file_path(&a).unwrap();
    let b_uri = Url::from_file_path(&b).unwrap();
    let mut state = State::default();
    state.docs.insert(a_uri.clone(), a_src.to_string());
    state.docs.insert(b_uri.clone(), b_src.to_string());
    (tmp, state, a_uri, b_uri)
}

#[test]
fn finds_top_level_symbols_across_the_project() {
    let (_tmp, mut state, a_uri, b_uri) = two_file_project();
    // An empty query matches everything.
    let symbols = run(&mut state, "");
    assert_eq!(names(&symbols), vec!["bar", "foo"], "{symbols:#?}");

    // Each symbol is reported against its own file.
    let foo = symbols.iter().find(|s| s.name == "foo").unwrap();
    let bar = symbols.iter().find(|s| s.name == "bar").unwrap();
    assert_eq!(foo.location.uri, a_uri);
    assert_eq!(bar.location.uri, b_uri);
    // The `foo` binder sits at line 1, columns 4..7.
    assert_eq!(foo.location.range.start.line, 1);
    assert_eq!(foo.location.range.start.character, 4);
}

#[test]
fn query_filters_case_insensitively() {
    let (_tmp, mut state, _a, _b) = two_file_project();
    // `FO` matches `foo` regardless of case; `bar` is excluded.
    let symbols = run(&mut state, "FO");
    assert_eq!(names(&symbols), vec!["foo"], "{symbols:#?}");
}

#[test]
fn searches_the_whole_project_when_only_one_file_is_open() {
    // The headline value: opening *one* file makes the entire project's
    // top-level symbols searchable — `B.fs`'s `bar` is found even though only
    // `A.fs` is open, because both are in the project's Compile list.
    let (_tmp, mut state, a_uri, b_uri) = two_file_project();
    // Close B's buffer (only A.fs open).
    state.docs.remove(&b_uri);

    let symbols = run(&mut state, "bar");
    assert_eq!(names(&symbols), vec!["bar"], "{symbols:#?}");
    // `bar` is reported against B.fs's on-disk URI even though B isn't open.
    assert_eq!(symbols[0].location.uri, b_uri);
    // Sanity: A.fs is the open buffer driving project discovery.
    assert!(state.docs.contains_key(&a_uri));
}

#[test]
fn distinguishes_function_and_value_kinds() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    let a = tmp.path().join("A.fs");
    write(
        &proj,
        r#"<Project><ItemGroup><Compile Include="A.fs" /></ItemGroup></Project>"#,
    );
    let src = "let value = 1\nlet func x = x\n";
    write(&a, src);

    let a_uri = Url::from_file_path(&a).unwrap();
    let mut state = State::default();
    state.docs.insert(a_uri.clone(), src.to_string());

    let symbols = run(&mut state, "");
    let value = symbols.iter().find(|s| s.name == "value").unwrap();
    let func = symbols.iter().find(|s| s.name == "func").unwrap();
    assert_eq!(value.kind, SymbolKind::VARIABLE);
    assert_eq!(func.kind, SymbolKind::FUNCTION);
}

#[test]
fn parameters_are_not_workspace_symbols() {
    // `let func x = x` binds a parameter `x`, but only top-level exports are
    // surfaced, so an empty query returns `func` and never the parameter.
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    let a = tmp.path().join("A.fs");
    write(
        &proj,
        r#"<Project><ItemGroup><Compile Include="A.fs" /></ItemGroup></Project>"#,
    );
    let src = "let func x = x\n";
    write(&a, src);
    let a_uri = Url::from_file_path(&a).unwrap();
    let mut state = State::default();
    state.docs.insert(a_uri, src.to_string());

    let all = run(&mut state, "");
    assert_eq!(names(&all), vec!["func"], "{all:#?}");
}

#[test]
fn orphan_buffer_falls_back_to_single_file_extraction() {
    // A `.fs` file under no `.fsproj` still has its own top-level symbols
    // searchable, via the single-file fallback (mirrors the references
    // handler's degradation discipline).
    let tmp = TempDir::new().unwrap();
    let loose = tmp.path().join("Loose.fs");
    let src = "let orphanSym = 1\n";
    write(&loose, src);
    let uri = Url::from_file_path(&loose).unwrap();

    let mut state = State::default();
    state.docs.insert(uri.clone(), src.to_string());

    let symbols = run(&mut state, "orphan");
    assert_eq!(names(&symbols), vec!["orphanSym"], "{symbols:#?}");
    assert_eq!(symbols[0].location.uri, uri);
}

/// Two open files of the *same* project, reached via path-equal but
/// byte-distinct directory spellings (here a casing flip on a
/// case-insensitive filesystem), must fold the project once — not twice.
/// `owning_project` returns the project path spelled however the opening file
/// was, so a raw `PathBuf` dedup would let both spellings through and emit
/// every symbol twice. Gated to case-insensitive platforms because on a
/// case-sensitive one the two spellings are genuinely different paths.
#[cfg(any(windows, target_os = "macos"))]
#[test]
fn same_project_via_different_path_spellings_is_not_duplicated() {
    let tmp = TempDir::new().unwrap();
    let proj_dir = tmp.path().join("proj");
    let proj = proj_dir.join("P.fsproj");
    let a = proj_dir.join("A.fs");
    let b = proj_dir.join("B.fs");
    write(
        &proj,
        r#"<Project>
          <ItemGroup>
            <Compile Include="A.fs" />
            <Compile Include="B.fs" />
          </ItemGroup>
        </Project>"#,
    );
    write(&a, "let aSym = 1\n");
    write(&b, "let bSym = 2\n");

    // Open A under the real (lowercase) dir and B under an upper-cased dir that
    // resolves to the same directory on a case-insensitive filesystem.
    let a_uri = Url::from_file_path(&a).unwrap();
    let b_uri = Url::from_file_path(tmp.path().join("PROJ").join("B.fs")).unwrap();
    let mut state = State::default();
    state.docs.insert(a_uri, "let aSym = 1\n".to_string());
    state.docs.insert(b_uri, "let bSym = 2\n".to_string());

    let symbols = run(&mut state, "");
    assert_eq!(
        names(&symbols),
        vec!["aSym", "bSym"],
        "each symbol must appear exactly once: {symbols:#?}"
    );
}

#[test]
fn shared_file_compiled_by_two_open_projects_is_listed_once() {
    // The link case: one `Shared.fs` is `<Compile>`d by both `A` and `B`. With
    // a file from each project open, both projects are folded — but the shared
    // file's symbols must appear once, not once per including project.
    let tmp = TempDir::new().unwrap();
    let shared = tmp.path().join("Shared.fs");
    let a_proj = tmp.path().join("A").join("A.fsproj");
    let b_proj = tmp.path().join("B").join("B.fsproj");
    let a_own = tmp.path().join("A").join("AOwn.fs");
    let b_own = tmp.path().join("B").join("BOwn.fs");
    write(
        &a_proj,
        r#"<Project>
          <ItemGroup>
            <Compile Include="AOwn.fs" />
            <Compile Include="../Shared.fs" />
          </ItemGroup>
        </Project>"#,
    );
    write(
        &b_proj,
        r#"<Project>
          <ItemGroup>
            <Compile Include="BOwn.fs" />
            <Compile Include="../Shared.fs" />
          </ItemGroup>
        </Project>"#,
    );
    write(&shared, "let sharedSym = 1\n");
    write(&a_own, "let aOwnSym = 1\n");
    write(&b_own, "let bOwnSym = 1\n");

    // Open one file from each project so both projects enter the search set.
    let a_uri = Url::from_file_path(&a_own).unwrap();
    let b_uri = Url::from_file_path(&b_own).unwrap();
    let mut state = State::default();
    state.docs.insert(a_uri, "let aOwnSym = 1\n".to_string());
    state.docs.insert(b_uri, "let bOwnSym = 1\n".to_string());

    let symbols = run(&mut state, "");
    assert_eq!(
        names(&symbols),
        vec!["aOwnSym", "bOwnSym", "sharedSym"],
        "the shared file's symbol must appear exactly once: {symbols:#?}"
    );
}

#[test]
fn shared_file_exposes_each_projects_active_if_branch() {
    // A linked file with an `#if`, compiled by two projects with *different*
    // `DefineConstants`, exposes a *different* symbol to each. Dedup must be by
    // symbol identity, not by path: collapsing the file after the first project
    // would drop the other project's branch symbol. Both must appear.
    let tmp = TempDir::new().unwrap();
    let shared = tmp.path().join("Shared.fs");
    let a_proj = tmp.path().join("A").join("A.fsproj");
    let b_proj = tmp.path().join("B").join("B.fsproj");
    let a_own = tmp.path().join("A").join("AOwn.fs");
    let b_own = tmp.path().join("B").join("BOwn.fs");
    // A defines FOO (so Shared.fs exposes `fooOnly`); B does not (so it exposes
    // `elseOnly`).
    write(
        &a_proj,
        r#"<Project>
          <PropertyGroup><DefineConstants>FOO</DefineConstants></PropertyGroup>
          <ItemGroup>
            <Compile Include="AOwn.fs" />
            <Compile Include="../Shared.fs" />
          </ItemGroup>
        </Project>"#,
    );
    write(
        &b_proj,
        r#"<Project>
          <ItemGroup>
            <Compile Include="BOwn.fs" />
            <Compile Include="../Shared.fs" />
          </ItemGroup>
        </Project>"#,
    );
    write(
        &shared,
        "#if FOO\nlet fooOnly = 1\n#else\nlet elseOnly = 1\n#endif\n",
    );
    write(&a_own, "let aOwnSym = 1\n");
    write(&b_own, "let bOwnSym = 1\n");

    let a_uri = Url::from_file_path(&a_own).unwrap();
    let b_uri = Url::from_file_path(&b_own).unwrap();
    let mut state = State::default();
    state.docs.insert(a_uri, "let aOwnSym = 1\n".to_string());
    state.docs.insert(b_uri, "let bOwnSym = 1\n".to_string());

    let symbols = run(&mut state, "");
    assert_eq!(
        names(&symbols),
        vec!["aOwnSym", "bOwnSym", "elseOnly", "fooOnly"],
        "each project's active #if branch symbol must survive: {symbols:#?}"
    );
}

#[test]
fn query_matches_non_ascii_identifiers_case_insensitively() {
    // F# identifiers can be Unicode; the advertised case-insensitive match must
    // fold non-ASCII letters too (`CAFÉ` should find `café`).
    let tmp = TempDir::new().unwrap();
    let loose = tmp.path().join("Loose.fs");
    let src = "let café = 1\n";
    write(&loose, src);
    let uri = Url::from_file_path(&loose).unwrap();

    let mut state = State::default();
    state.docs.insert(uri, src.to_string());

    let symbols = run(&mut state, "CAFÉ");
    assert_eq!(names(&symbols), vec!["café"], "{symbols:#?}");
}

#[test]
fn open_linked_file_is_not_reparsed_under_wrong_defines() {
    // A shared file lives outside any project tree and is linked into a project
    // that defines FOO. When the shared file is *open*, the project fold already
    // emits its symbols under the correct (FOO) defines. The single-file
    // fallback must not *also* reparse it under its own (orphan → default)
    // symbol set, which would activate the `#else` branch and surface `notFoo`
    // — a symbol no searched project compiles.
    let tmp = TempDir::new().unwrap();
    let shared = tmp.path().join("shared").join("Lib.fs");
    let app_proj = tmp.path().join("app").join("App.fsproj");
    let app_own = tmp.path().join("app").join("App.fs");
    write(
        &app_proj,
        r#"<Project>
          <PropertyGroup><DefineConstants>FOO</DefineConstants></PropertyGroup>
          <ItemGroup>
            <Compile Include="App.fs" />
            <Compile Include="../shared/Lib.fs" />
          </ItemGroup>
        </Project>"#,
    );
    write(&app_own, "let appSym = 1\n");
    let lib_src = "#if FOO\nlet onlyFoo = 1\n#else\nlet notFoo = 1\n#endif\n";
    write(&shared, lib_src);

    // Open App.fs (folds the App project) and the shared Lib.fs itself.
    let app_uri = Url::from_file_path(&app_own).unwrap();
    let lib_uri = Url::from_file_path(&shared).unwrap();
    let mut state = State::default();
    state.docs.insert(app_uri, "let appSym = 1\n".to_string());
    state.docs.insert(lib_uri, lib_src.to_string());

    let symbols = run(&mut state, "");
    assert_eq!(
        names(&symbols),
        vec!["appSym", "onlyFoo"],
        "the fallback must not surface the wrong-branch `notFoo`: {symbols:#?}"
    );
}

#[test]
fn no_open_documents_returns_an_empty_list() {
    // Nothing open → nothing to search. Always `Some(empty)`, never `None`, so
    // a stale client symbol list clears rather than sticking.
    let mut state = State::default();
    let symbols = run(&mut state, "anything");
    assert!(symbols.is_empty(), "{symbols:#?}");
}

#[test]
fn buffer_overlay_beats_disk_text() {
    // The open buffer's edits are searched, not the stale on-disk text:
    // disk says `diskSym`, the buffer says `bufferSym`.
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    let a = tmp.path().join("A.fs");
    write(
        &proj,
        r#"<Project><ItemGroup><Compile Include="A.fs" /></ItemGroup></Project>"#,
    );
    write(&a, "let diskSym = 1\n");
    let a_uri = Url::from_file_path(&a).unwrap();

    let mut state = State::default();
    state
        .docs
        .insert(a_uri.clone(), "let bufferSym = 1\n".to_string());

    let symbols = run(&mut state, "");
    assert_eq!(names(&symbols), vec!["bufferSym"], "{symbols:#?}");
}

/// The CST parser runs panic-safely; the handler must survive arbitrary
/// buffers and queries rather than unwind through the request loop.
mod panic_safe {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn handler_never_panics(src in "(?s).{0,80}", query in ".{0,8}") {
            let mut state = State::default();
            let uri = Url::parse("inmemory:///Sample.fs").unwrap();
            state.docs.insert(uri, src);
            let _ = workspace_symbol::handle(&mut state, params(&query));
        }
    }
}
