//! Tests for the `textDocument/references` handler.
//!
//! Drive `handle` directly with a populated `State` — the JSON-RPC harness
//! lands with Stage 8. These tests pin the algorithm: cursor →
//! `Resolution` → every matching range in every file, with
//! `include_declaration` honoured.

use std::fs;
use std::path::Path;

use borzoi::handlers::references;
use borzoi::server::State;
use lsp_types::{
    Location, PartialResultParams, Position, ReferenceContext, ReferenceParams,
    TextDocumentIdentifier, TextDocumentPositionParams, Url, WorkDoneProgressParams,
};
use tempfile::TempDir;

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn params(uri: &Url, line: u32, character: u32, include_declaration: bool) -> ReferenceParams {
    ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration,
        },
    }
}

fn run(
    state: &mut State,
    uri: &Url,
    line: u32,
    character: u32,
    include_declaration: bool,
) -> Vec<Location> {
    references::handle(state, params(uri, line, character, include_declaration))
        .expect("handler returns Some, never an error envelope")
}

fn orphan_state(text: &str) -> (State, Url) {
    let mut state = State::default();
    let uri = Url::parse("inmemory:///Sample.fs").unwrap();
    state.docs.insert(uri.clone(), text.to_string());
    (state, uri)
}

#[test]
fn local_let_collects_self_and_uses_with_declaration() {
    // `let x = 1 in x + x` — `x` resolves three times: the binder's self-
    // range and two uses. With include_declaration=true we report all three.
    let src = "let x = 1\nlet y = x + x\n";
    let (mut state, uri) = orphan_state(src);

    // Cursor on the binder `x` (line 0 col 4).
    let locs = run(&mut state, &uri, 0, 4, true);
    assert_eq!(locs.len(), 3, "{locs:#?}");
    // All in the same URI.
    assert!(locs.iter().all(|l| l.uri == uri));
}

#[test]
fn local_let_omits_declaration_when_requested() {
    // Same source; include_declaration=false → only the two uses.
    let src = "let x = 1\nlet y = x + x\n";
    let (mut state, uri) = orphan_state(src);

    let locs = run(&mut state, &uri, 0, 4, false);
    assert_eq!(locs.len(), 2, "{locs:#?}");
    // Neither remaining location overlaps the binder at (0, 4..5).
    let decl = lsp_types::Range {
        start: Position {
            line: 0,
            character: 4,
        },
        end: Position {
            line: 0,
            character: 5,
        },
    };
    assert!(
        locs.iter().all(|l| l.range != decl),
        "decl range leaked: {locs:#?}"
    );
}

#[test]
fn shadowing_param_does_not_pollute_outer_let() {
    // Outer `let x` shadowed by an inner parameter `x`. Cursor on the inner
    // parameter use should *not* drag references to the outer `x` in. The
    // shadowing produces distinct `DefId`s, so the `Local` resolutions are
    // unequal under PartialEq.
    let src = "let x = 1\nlet f x = x\n";
    let (mut state, uri) = orphan_state(src);

    // Cursor on the parameter use `x` in `let f x = x`: column 10 on line 1.
    let locs = run(&mut state, &uri, 1, 10, true);
    // 2 references: the parameter binder at (1, 6..7) and its use at (1, 10..11).
    assert_eq!(locs.len(), 2, "{locs:#?}");
    for l in &locs {
        assert_eq!(l.range.start.line, 1);
        // None of them should be the outer `let x` at (0, 4..5).
        assert_ne!(l.range.start.character, 4);
    }
}

#[test]
fn unresolved_cursor_returns_empty_list_not_none() {
    // `Deferred(UnboundName)` cursor → empty list (the spec answer that
    // clears a stale reference panel cleanly). Critically not `None`.
    let src = "let _ = nowhere\n";
    let (mut state, uri) = orphan_state(src);
    let locs = run(&mut state, &uri, 0, 9, true);
    assert!(locs.is_empty());
}

#[test]
fn missing_buffer_returns_none() {
    // No buffer for the URI → `None`; the JSON-RPC layer serialises this as
    // null, never an error envelope.
    let mut state = State::default();
    let uri = Url::parse("inmemory:///Missing.fs").unwrap();
    let resp = references::handle(&mut state, params(&uri, 0, 0, true));
    assert!(resp.is_none());
}

#[test]
fn cross_file_item_collects_uses_in_every_file() {
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
    write(&a, "module Shared\nlet foo = 1\n");
    let b_src = "module Other\nlet bar = Shared.foo\nlet baz = Shared.foo\n";
    write(&b, b_src);

    let a_uri = Url::from_file_path(&a).unwrap();
    let b_uri = Url::from_file_path(&b).unwrap();
    let a_src = "module Shared\nlet foo = 1\n";
    let mut state = State::default();
    state.docs.insert(a_uri.clone(), a_src.to_string());
    state.docs.insert(b_uri.clone(), b_src.to_string());

    // Cursor on the `foo` binder in A (line 1, col 4..7). With
    // include_declaration=true we expect 3: the binder + 2 uses in B.
    let locs = run(&mut state, &a_uri, 1, 4, true);
    assert_eq!(locs.len(), 3, "{locs:#?}");
    let on_a = locs.iter().filter(|l| l.uri == a_uri).count();
    let on_b = locs.iter().filter(|l| l.uri == b_uri).count();
    assert_eq!(on_a, 1, "expected 1 ref in A (the binder)");
    assert_eq!(on_b, 2, "expected 2 refs in B");
}

#[test]
fn cross_file_item_omits_declaration() {
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
    write(&a, "module Shared\nlet foo = 1\n");
    let b_src = "module Other\nlet bar = Shared.foo\nlet baz = Shared.foo\n";
    write(&b, b_src);

    let a_uri = Url::from_file_path(&a).unwrap();
    let b_uri = Url::from_file_path(&b).unwrap();
    let a_src = "module Shared\nlet foo = 1\n";
    let mut state = State::default();
    state.docs.insert(a_uri.clone(), a_src.to_string());
    state.docs.insert(b_uri.clone(), b_src.to_string());

    // Cursor on the binder; include_declaration=false → 2 hits, both in B.
    let locs = run(&mut state, &a_uri, 1, 4, false);
    assert_eq!(locs.len(), 2, "{locs:#?}");
    assert!(locs.iter().all(|l| l.uri == b_uri));
}

#[test]
fn local_resolution_short_circuits_to_cursor_file() {
    // A `Local` in file 2 of a 4-file project shouldn't even iterate the
    // other files. We can't directly observe the loop, but we can pin the
    // outward behaviour: only file 2's URI appears in the result, and the
    // count matches the in-file uses.
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    write(
        &proj,
        r#"<Project>
          <ItemGroup>
            <Compile Include="A.fs" />
            <Compile Include="B.fs" />
            <Compile Include="C.fs" />
          </ItemGroup>
        </Project>"#,
    );
    write(&tmp.path().join("A.fs"), "let common = 1\n");
    write(&tmp.path().join("B.fs"), "let common = 2\n"); // shadowed; same name.
    let c_src = "let common = 3\nlet x = common\nlet y = common\n";
    write(&tmp.path().join("C.fs"), c_src);

    let c_uri = Url::from_file_path(tmp.path().join("C.fs")).unwrap();
    let mut state = State::default();
    state.docs.insert(c_uri.clone(), c_src.to_string());

    // Cursor on C's `common` use. The resolution is the *Item* C exports,
    // not a Local — but no other file references it, so the result is just
    // the 3 hits in C (binder + 2 uses), proving the iteration only matches
    // by Resolution identity and ignores same-named items in A/B.
    let locs = run(&mut state, &c_uri, 1, 8, true);
    assert!(locs.iter().all(|l| l.uri == c_uri));
    assert_eq!(locs.len(), 3, "{locs:#?}");
}

#[test]
fn orphan_file_falls_back_to_single_file_references() {
    // Partial project (unresolved Import) → single-file fallback exercised.
    // A local reference still yields multiple matches.
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    let f = tmp.path().join("Lib.fs");
    write(
        &proj,
        r#"<Project>
          <Import Project="Missing.props" />
          <ItemGroup><Compile Include="Lib.fs" /></ItemGroup>
        </Project>"#,
    );
    let src = "let x = 1\nlet y = x\nlet z = x\n";
    write(&f, src);

    let uri = Url::from_file_path(&f).unwrap();
    let mut state = State::default();
    state.docs.insert(uri.clone(), src.to_string());

    let locs = run(&mut state, &uri, 0, 4, true);
    // Binder + 2 uses = 3 references.
    assert_eq!(locs.len(), 3, "{locs:#?}");
}

/// References on an attribute name list the attribute occurrence itself: the
/// attribute-type resolutions live in their own map (EX-3 §2(d)), and both the
/// target lookup and the occurrence collection must chain it — from the
/// attribute use, and from the type's declaration.
#[test]
fn attribute_use_participates_in_references() {
    let src = "type MyAttrAttribute() =\n    inherit System.Attribute()\n\n[<MyAttr>]\nlet x = 1\n";
    let (mut state, uri) = orphan_state(src);

    // Cursor inside the attribute name `MyAttr` (line 3, cols 2..8).
    let locs = run(&mut state, &uri, 3, 4, true);
    // The declaration's self-range plus the attribute occurrence.
    assert_eq!(locs.len(), 2, "{locs:#?}");
    assert!(locs.iter().all(|l| l.uri == uri));
    // Excluding the declaration leaves exactly the attribute occurrence.
    let locs = run(&mut state, &uri, 3, 4, false);
    assert_eq!(locs.len(), 1, "{locs:#?}");
    assert_eq!(
        locs[0].range.start,
        lsp_types::Position {
            line: 3,
            character: 2
        }
    );
}
