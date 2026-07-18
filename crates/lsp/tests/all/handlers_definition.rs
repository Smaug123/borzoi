//! Tests for the `textDocument/definition` handler.
//!
//! Drives `handle` directly with a populated `State` — the JSON-RPC harness
//! that exercises the wire format lands with Stage 8. These tests pin the
//! handler's algorithm: cursor → `Resolution` → `Location`, with the
//! single-file fallback for orphan / partial-project buffers.

use std::fs;
use std::path::Path;

use crate::common::runtime_project_state;
use borzoi::handlers::definition;
use borzoi::handlers::definition::DefinitionOutcome;
use borzoi::server::State;
use lsp_types::{
    GotoDefinitionParams, GotoDefinitionResponse, Location, PartialResultParams, Position, Range,
    TextDocumentIdentifier, TextDocumentPositionParams, Url, WorkDoneProgressParams,
};
use tempfile::TempDir;

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn params(uri: &Url, line: u32, character: u32) -> GotoDefinitionParams {
    GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

fn run(state: &mut State, uri: &Url, line: u32, character: u32) -> Option<Location> {
    // These tests never hit a referenced-assembly SourceLink source, so the
    // handler always resolves synchronously (`Ready`); a `Deferred` here is a bug.
    match definition::handle(state, params(uri, line, character)) {
        DefinitionOutcome::Ready(Some(GotoDefinitionResponse::Scalar(loc))) => Some(loc),
        DefinitionOutcome::Ready(Some(
            GotoDefinitionResponse::Array(_) | GotoDefinitionResponse::Link(_),
        )) => {
            panic!("unexpected non-scalar response shape for v1")
        }
        DefinitionOutcome::Ready(None) => None,
        DefinitionOutcome::Deferred(_) => panic!("unexpected deferred fetch in a local-only test"),
    }
}

/// Locate the byte offset of `needle`'s first occurrence in `text`, returning
/// `(line, character)` for the *middle* of `needle` so the cursor lands inside
/// the identifier.
fn cursor_inside(text: &str, needle: &str) -> (u32, u32) {
    let byte = text
        .find(needle)
        .unwrap_or_else(|| panic!("needle {needle:?} not in source"))
        + needle.len() / 2;
    // Convert byte offset to (line, UTF-16 col). Our F# tests are ASCII so
    // bytes == UTF-16 units; line counting handles `\n`/`\r\n` the same way
    // `position_to_offset` does.
    let prefix = &text[..byte];
    let line = prefix.matches('\n').count() as u32;
    let column = prefix
        .rsplit_once('\n')
        .map(|(_, last)| last.len())
        .unwrap_or(prefix.len()) as u32;
    (line, column)
}

/// Construct an in-memory state with an open buffer (no project on disk).
/// Returns the URI of the opened buffer.
fn orphan_state(text: &str) -> (State, Url) {
    let mut state = State::default();
    let uri = Url::parse("inmemory:///Sample.fs").unwrap();
    state.docs.insert(uri.clone(), text.to_string());
    (state, uri)
}

/// Go-to-definition folds only the Compile **prefix** up to the cursor's file:
/// a cross-file jump from file 1 of a three-file project resolves the target in
/// file 0 (inside the [0, 1] prefix) and never folds file 2. Pins the
/// resolution-slice wiring (`resolved_prefix_for` with the file's Compile index,
/// not `usize::MAX`) — a regression to the whole-project fold would cache all
/// three files.
#[test]
fn project_definition_folds_only_the_prefix_up_to_the_cursor_file() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    let a = tmp.path().join("A.fs");
    let b = tmp.path().join("B.fs");
    let c = tmp.path().join("C.fs");
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
    write(&a, "module A\nlet foo = 1\n");
    let b_src = "module B\nlet bar = A.foo\n";
    write(&b, b_src);
    write(&c, "module C\nlet baz = B.bar\n");

    let a_uri = Url::from_file_path(&a).unwrap();
    let b_uri = Url::from_file_path(&b).unwrap();
    let mut state = State::default();
    state.docs.insert(b_uri.clone(), b_src.to_string());

    // Cursor on the `foo` use in B (file 1) → its definition in A (file 0).
    let (l, ch) = cursor_inside(b_src, "foo");
    let loc = run(&mut state, &b_uri, l, ch).expect("cross-file definition");
    assert_eq!(loc.uri, a_uri, "definition lands in A");

    // Only the prefix [A, B] was folded; C (file 2) never was.
    assert_eq!(
        state.semantic.cached_resolved_len(&proj),
        Some(2),
        "definition from file 1 folds the [0, 1] prefix, not file 2"
    );
}

// ---- orphan-file fallback ----

#[test]
fn local_let_self_reference_resolves_to_its_binder() {
    let src = "let x = 1\nlet y = x\n";
    let (mut state, uri) = orphan_state(src);

    let (l, c) = cursor_inside(src, "x"); // first `x` (the binder itself)
    let loc = run(&mut state, &uri, l, c).expect("a definition for `x`");
    // The use *is* the definition for the binder's own range — a `Local`
    // resolution at its self-range, pointing back to itself.
    assert_eq!(loc.uri, uri);
    assert_eq!(
        loc.range.start,
        Position {
            line: 0,
            character: 4
        }
    );
}

/// An attribute name navigates to its in-file attribute type: the resolver's
/// attribute-type resolutions (EX-3 §2(d), a separate map from the ordinary
/// one) are chained into `smallest_resolution_with_range`, so
/// go-to-definition on `[<MyAttr>]` reaches `type MyAttrAttribute` through
/// FCS's suffix-first candidate rule.
#[test]
fn attribute_name_resolves_to_its_in_file_type() {
    let src = "type MyAttrAttribute() =\n    inherit System.Attribute()\n\n[<MyAttr>]\nlet x = 1\n";
    let (mut state, uri) = orphan_state(src);

    let (l, c) = cursor_inside(src, "[<MyAttr>]");
    let loc = run(&mut state, &uri, l, c).expect("a definition for the attribute name");
    assert_eq!(loc.uri, uri);
    // The binder is the `MyAttrAttribute` ident on line 0.
    assert_eq!(
        loc.range.start,
        Position {
            line: 0,
            character: 5
        }
    );
}

#[test]
fn local_reference_resolves_to_the_let_binder() {
    let src = "let x = 1\nlet y = x\n";
    let (mut state, uri) = orphan_state(src);

    // Cursor on the *use* `x` on line 1.
    let use_byte = src.rfind('x').unwrap();
    let line = src[..use_byte].matches('\n').count() as u32;
    let col = use_byte - src[..use_byte].rfind('\n').unwrap_or(0) - 1;
    let loc = run(&mut state, &uri, line, col as u32).expect("a definition for the use of `x`");
    assert_eq!(loc.uri, uri);
    assert_eq!(
        loc.range,
        Range {
            start: Position {
                line: 0,
                character: 4
            },
            end: Position {
                line: 0,
                character: 5
            },
        }
    );
}

#[test]
fn parameter_reference_resolves_to_its_parameter_binder() {
    let src = "let f x = x\n";
    let (mut state, uri) = orphan_state(src);

    // Cursor on the use `x` in the body.
    let use_byte = src.rfind('x').unwrap();
    let line = 0u32;
    let col = use_byte as u32;
    let loc = run(&mut state, &uri, line, col).expect("definition for parameter use");
    assert_eq!(loc.uri, uri);
    // Parameter binder is at column 6 (the `x` after `f `).
    assert_eq!(loc.range.start.character, 6);
}

#[test]
fn type_use_resolves_to_its_in_file_type_definition() {
    // `type B = A` — the cursor on the `A` *use* jumps to `type A`'s binder.
    // End-to-end through the single-file fallback: cursor → byte → sema
    // `Resolution::Local` (a `DefKind::Type` binder) → `Location`.
    let src = "type A = int\ntype B = A\n";
    let (mut state, uri) = orphan_state(src);

    // The `A` use is the *last* `A` (the def is the first); `rfind` finds it.
    let use_byte = src.rfind('A').unwrap();
    let line = src[..use_byte].matches('\n').count() as u32;
    let col = (use_byte - src[..use_byte].rfind('\n').unwrap_or(0) - 1) as u32;
    let loc = run(&mut state, &uri, line, col).expect("a definition for the type use `A`");

    assert_eq!(loc.uri, uri);
    // `type A` puts the binder `A` at line 0, columns 5..6.
    assert_eq!(
        loc.range,
        Range {
            start: Position {
                line: 0,
                character: 5
            },
            end: Position {
                line: 0,
                character: 6
            },
        }
    );
}

#[test]
fn nested_module_sibling_reference_jumps_to_definition() {
    // The real-world shape (cf. `FSharp.Core/string.fs`): a `module` nested under
    // a `namespace`, where the whole body lives in the nested module. Cursor on
    // the `answer` use jumps to the sibling `let answer` — the resolver now
    // descends into the nested-module body.
    let src = "namespace Demo\nmodule Calc =\n    let answer = 1\n    let doubled = answer\n";
    let (mut state, uri) = orphan_state(src);

    let use_byte = src.rfind("answer").unwrap();
    let line = src[..use_byte].matches('\n').count() as u32;
    let col = (use_byte - src[..use_byte].rfind('\n').unwrap_or(0) - 1) as u32;
    let loc = run(&mut state, &uri, line, col).expect("a definition for the nested `answer` use");

    assert_eq!(loc.uri, uri);
    // `let answer` is on line 2; `    let ` is 8 chars, so `answer` is cols 8..14.
    assert_eq!(
        loc.range,
        Range {
            start: Position {
                line: 2,
                character: 8
            },
            end: Position {
                line: 2,
                character: 14
            },
        }
    );
}

#[test]
fn parameter_type_annotation_jumps_to_type_definition() {
    // The headline case: cursor on the `A` in `let f (x : A) = x` jumps to
    // `type A`. End-to-end through the single-file fallback.
    let src = "type A = int\nlet f (x : A) = x\n";
    let (mut state, uri) = orphan_state(src);

    let use_byte = src.rfind('A').unwrap();
    let line = src[..use_byte].matches('\n').count() as u32;
    let col = (use_byte - src[..use_byte].rfind('\n').unwrap_or(0) - 1) as u32;
    let loc = run(&mut state, &uri, line, col).expect("a definition for the annotation `A`");

    assert_eq!(loc.uri, uri);
    assert_eq!(
        loc.range,
        Range {
            start: Position {
                line: 0,
                character: 5
            },
            end: Position {
                line: 0,
                character: 6
            },
        }
    );
}

#[test]
fn unresolved_name_returns_none() {
    // A name that isn't bound anywhere we model: `Deferred(UnboundName)`,
    // which v1 maps to no result.
    let src = "let y = nowhere\n";
    let (mut state, uri) = orphan_state(src);
    let (l, c) = cursor_inside(src, "nowhere");
    assert!(run(&mut state, &uri, l, c).is_none());
}

#[test]
fn cursor_off_any_identifier_returns_none() {
    // Cursor on a whitespace byte. No recorded resolution contains it.
    let src = "let x = 1\n";
    let (mut state, uri) = orphan_state(src);
    // Column 3 is the space after `let`.
    assert!(run(&mut state, &uri, 0, 3).is_none());
}

#[test]
fn missing_buffer_returns_none() {
    // The handler defends against a stray request for a URI we have no
    // buffer for — `Ok(None)`, not an error envelope.
    let mut state = State::default();
    let uri = Url::parse("inmemory:///Missing.fs").unwrap();
    assert!(run(&mut state, &uri, 0, 0).is_none());
}

// ---- project-level resolution ----

#[test]
fn cross_file_qualified_reference_jumps_to_other_file() {
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
    let b_src = "module Other\nlet bar = Shared.foo\n";
    write(&b, b_src);

    let b_uri = Url::from_file_path(&b).unwrap();
    let a_uri = Url::from_file_path(&a).unwrap();

    let mut state = State::default();
    state.docs.insert(b_uri.clone(), b_src.to_string());
    // A.fs is on disk but not in the editor; the fold reads it from disk.

    let (l, c) = cursor_inside(b_src, "foo"); // the use's `foo`
    let loc = run(&mut state, &b_uri, l, c).expect("cross-file definition");
    assert_eq!(loc.uri, a_uri);
    // foo binder is at `let foo = 1` on line 1 of A — column 4..7.
    assert_eq!(
        loc.range,
        Range {
            start: Position {
                line: 1,
                character: 4,
            },
            end: Position {
                line: 1,
                character: 7,
            },
        }
    );
}

#[test]
fn assembly_path_returns_none_pinned_for_metadata_uri_followup() {
    // A fully-qualified assembly path (`System.Console.WriteLine`) resolves
    // to a referenced-assembly entity. v1 has no `metadata://` URI for that;
    // the response is `None`. Pin this so a future stage that adds metadata
    // resolution is a deliberate flip.
    //
    // We don't have a real assembly env in this test (no `dotnet_root`,
    // no `project.assets.json`), so the resolution is actually `Deferred`
    // — which *also* maps to `None`, matching the same client-observable
    // behaviour. The test pins the no-Location outcome, which is the
    // contract we care about regardless of which Deferred-vs-Entity path
    // sema took.
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    let f = tmp.path().join("Lib.fs");
    write(
        &proj,
        r#"<Project>
          <ItemGroup><Compile Include="Lib.fs" /></ItemGroup>
        </Project>"#,
    );
    let src = "let _ = System.Console.WriteLine \"hi\"\n";
    write(&f, src);

    let uri = Url::from_file_path(&f).unwrap();
    let mut state = State::default();
    state.docs.insert(uri.clone(), src.to_string());

    let (l, c) = cursor_inside(src, "WriteLine");
    assert!(run(&mut state, &uri, l, c).is_none());
}

#[test]
fn orphan_file_under_partial_project_falls_back_to_single_file() {
    // The project can't be fully evaluated (unresolved `<Import>`), so
    // `resolved_project_for` returns None. The single-file fallback must
    // still answer a same-file local reference — anything less would
    // surprise a user whose project temporarily can't restore.
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
    let src = "let x = 1\nlet y = x\n";
    write(&f, src);

    let uri = Url::from_file_path(&f).unwrap();
    let mut state = State::default();
    state.docs.insert(uri.clone(), src.to_string());

    // Use on line 1.
    let loc = run(&mut state, &uri, 1, 8).expect("fallback definition");
    assert_eq!(loc.uri, uri);
    assert_eq!(loc.range.start.character, 4);
}

#[test]
fn long_ident_segment_prefers_whole_path_over_inner_deferred() {
    // `Shared.foo` records the whole-path range as `Item` and the inner
    // `Shared` segment as `Deferred(QualifiedAccess)`. A cursor on the
    // `S` of `Shared` must follow the non-Deferred whole-path resolution,
    // not silently bind to the inner `Deferred`. Without the
    // prefer-non-Deferred rule in `smallest_resolution_at` this test fails
    // with `None`.
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
    let b_src = "module Other\nlet bar = Shared.foo\n";
    write(&b, b_src);

    let b_uri = Url::from_file_path(&b).unwrap();
    let mut state = State::default();
    state.docs.insert(b_uri.clone(), b_src.to_string());

    // Cursor on the `S` of `Shared` (start of the LongIdent).
    let segment_byte = b_src.find("Shared").unwrap();
    let line = b_src[..segment_byte].matches('\n').count() as u32;
    let col = b_src[..segment_byte]
        .rsplit_once('\n')
        .map(|(_, last)| last.len())
        .unwrap_or(segment_byte) as u32;
    let loc =
        run(&mut state, &b_uri, line, col).expect("a definition for the LongIdent first segment");
    // The whole-path Item resolves to file 0's `foo` binder regardless of
    // which segment the cursor sat on.
    assert_eq!(loc.uri, Url::from_file_path(&a).unwrap());
    assert_eq!(loc.range.start.line, 1);
}

/// LSP clients key documents by URI *string*, so a Location whose URI was
/// reconstructed from the project's compile-item path can be a *different*
/// URI than the one the client opened. Pin: when a same-file `Local`
/// resolution fires, the response URI must equal the request URI verbatim,
/// even if the project lists the file under a different lexical spelling.
#[cfg(any(windows, target_os = "macos"))]
#[test]
fn local_definition_preserves_request_uri_casing() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    let listed = tmp.path().join("Lib.fs");
    write(
        &proj,
        r#"<Project>
          <ItemGroup><Compile Include="Lib.fs" /></ItemGroup>
        </Project>"#,
    );
    let src = "let x = 1\nlet y = x\n";
    write(&listed, src);

    // Open the file under a different casing than the project lists it.
    let opened = tmp.path().join("lib.fs");
    let request_uri = Url::from_file_path(&opened).unwrap();
    let mut state = State::default();
    state.docs.insert(request_uri.clone(), src.to_string());

    let loc = run(&mut state, &request_uri, 1, 8).expect("definition for `x` use");
    // The response URI must match the client's open document, not the
    // project's `Lib.fs` spelling.
    assert_eq!(
        loc.uri, request_uri,
        "expected request URI to be preserved (the client keys by URI string)"
    );
}

/// Cross-file `Item` location: when the target file is *open* in the editor
/// under a different casing than the project lists, the Location URI must
/// prefer the open-buffer URI over a freshly-constructed one — same client-
/// keyed-by-URI-string reasoning as the same-file case.
#[cfg(any(windows, target_os = "macos"))]
#[test]
fn cross_file_definition_prefers_open_buffer_uri() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    let a_listed = tmp.path().join("A.fs");
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
    write(&a_listed, "module Shared\nlet foo = 1\n");
    let b_src = "module Other\nlet bar = Shared.foo\n";
    write(&b, b_src);

    // The user has *both* files open: B with its normal casing, A under a
    // different casing than the project lists.
    let a_opened = tmp.path().join("a.fs");
    let a_uri = Url::from_file_path(&a_opened).unwrap();
    let b_uri = Url::from_file_path(&b).unwrap();
    let mut state = State::default();
    state
        .docs
        .insert(a_uri.clone(), "module Shared\nlet foo = 1\n".to_string());
    state.docs.insert(b_uri.clone(), b_src.to_string());

    let (l, c) = cursor_inside(b_src, "foo");
    let loc = run(&mut state, &b_uri, l, c).expect("cross-file definition");
    assert_eq!(
        loc.uri, a_uri,
        "expected the open buffer's URI, not the project's casing"
    );
}

/// Stage 5 codex review (Finding 2): a single source file in two projects'
/// compile lists. A `didChange` on that file must invalidate the parses for
/// **both** cached projects, not just the one `owning_project` would pick.
#[test]
fn invalidate_owning_project_clears_every_project_containing_the_file() {
    use borzoi::semantic::SemanticState;
    use std::collections::HashMap;

    let tmp = TempDir::new().unwrap();
    // Two projects, both listing the same shared file as a link.
    let proj_a = tmp.path().join("A.fsproj");
    let proj_b = tmp.path().join("B.fsproj");
    let shared = tmp.path().join("Shared.fs");
    write(
        &proj_a,
        r#"<Project>
          <ItemGroup><Compile Include="Shared.fs" /></ItemGroup>
        </Project>"#,
    );
    write(
        &proj_b,
        r#"<Project>
          <ItemGroup><Compile Include="Shared.fs" /></ItemGroup>
        </Project>"#,
    );
    write(&shared, "let v = 1\n");

    // Populate the semantic cache for both projects independently.
    let mut state = State::default();
    {
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut state;
        let _ = semantic
            .parses_for_project(&proj_a, workspace, docs)
            .expect("A parses");
    }
    {
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut state;
        let _ = semantic
            .parses_for_project(&proj_b, workspace, docs)
            .expect("B parses");
    }
    // Sanity: both projects are cached. Verify by counting cache hits — a
    // second lookup against an empty docs map must not re-build.
    let probe = |sema: &mut SemanticState, p: &Path, ws: &mut _, docs: &HashMap<_, _>| {
        sema.parses_for_project(p, ws, docs)
            .map(|p| p.len())
            .unwrap_or(0)
    };
    {
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut state;
        assert_eq!(probe(semantic, &proj_a, workspace, docs), 1);
        assert_eq!(probe(semantic, &proj_b, workspace, docs), 1);
    }

    // A `.fs` text-sync against the shared file: per codex finding 2, the
    // invalidation walks the cache and drops every project listing the file.
    let shared_uri = Url::from_file_path(&shared).unwrap();
    state.invalidate_owning_project(&shared_uri);

    // Surgical probe: simulate a new buffer overlay and ensure both
    // projects' parses reflect it on the next lookup (they would not if the
    // invalidation only cleared one). Re-borrow `semantic` between the two
    // calls so each `parses_for_project` returns and drops its borrow before
    // the next one starts.
    state
        .docs
        .insert(shared_uri.clone(), "let after = 2\n".to_string());
    {
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut state;
        let parses_a = semantic
            .parses_for_project(&proj_a, workspace, docs)
            .expect("A parses after invalidation")
            .clone();
        assert!(parses_a.texts[0].contains("after"), "A stale");
    }
    {
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut state;
        let parses_b = semantic
            .parses_for_project(&proj_b, workspace, docs)
            .expect("B parses after invalidation")
            .clone();
        assert!(parses_b.texts[0].contains("after"), "B stale");
    }
}

// ---- Stage 3.3b: member-resolution go-to-definition ----

#[test]
fn goto_def_on_member_name_routes_through_the_member_path() {
    // Stage 3.3b: go-to-definition on `Length` in `s.Length` behaves identically
    // to a resolver-resolved `Resolution::Member` — inference resolves the member
    // (a `System.String` property) and the handler routes it through
    // `assembly_member_location`. `System.String.Length` is a *property* (not a
    // `MethodDef` with a PDB sequence point), so — exactly like a resolver-
    // resolved property member — the outcome is `None` (no source to navigate to),
    // never a panic and never a wrong target.
    let src = "module M\nlet s = \"hi\"\nlet n = s.Length\n";
    let (mut state, uri) = runtime_project_state(src);
    // `Length` at line 2, columns 10..16 — cursor inside it.
    let loc = run(&mut state, &uri, 2, 12);
    assert_eq!(
        loc, None,
        "a property member has no PDB navigation, like a resolver-resolved property"
    );
}

#[test]
fn goto_def_on_member_receiver_still_navigates() {
    // The member-resolution fallback must not disturb the ordinary path: go-to-def
    // on the *receiver* `s` still lands on its `let s` binder.
    let src = "module M\nlet s = \"hi\"\nlet n = s.Length\n";
    let (mut state, uri) = runtime_project_state(src);
    // The receiver `s` in `s.Length` at line 2, column 8.
    let loc = run(&mut state, &uri, 2, 8).expect("definition for the receiver `s`");
    assert_eq!(loc.uri, uri);
    // Points at the `let s` binder on line 1 (`s` at column 4).
    assert_eq!(
        loc.range.start,
        Position {
            line: 1,
            character: 4
        }
    );
}

// ---- Stage 3.3d: method-call member-resolution go-to-definition ----

#[test]
fn goto_def_on_method_name_routes_through_the_member_path() {
    // Stage 3.3d: go-to-def on a *called* method name routes through the same
    // `Resolution::Member` / `assembly_member_location` path a field does — the
    // method is recorded in the member-resolution side-table on a successful wake.
    // The BCL ref assembly ships no PDB, so a method (like a property) yields no
    // navigable source — `None`, never a panic and never a wrong target.
    let src = "module M\nlet s = \"hi\"\nlet a = s.ToLowerInvariant()\n";
    let (mut state, uri) = runtime_project_state(src);
    // `ToLowerInvariant` at line 2, cursor inside it (column 12).
    let loc = run(&mut state, &uri, 2, 12);
    assert_eq!(
        loc, None,
        "a ref-assembly method has no PDB navigation, like a resolver-resolved member"
    );
}

#[test]
fn goto_def_on_static_method_name_routes_through_the_member_path() {
    // Stage OV-7: go-to-def on a committed *static* overload's name routes through
    // the same member path. The BCL ref assembly ships no PDB, so it yields `None`
    // — never a panic, never a wrong target.
    let src = "module M\nlet c = System.String.Compare(\"a\", \"b\")\n";
    let (mut state, uri) = runtime_project_state(src);
    // `Compare` at line 1, cursor inside it (column 24).
    let loc = run(&mut state, &uri, 1, 24);
    assert_eq!(
        loc, None,
        "a ref-assembly static method has no PDB navigation"
    );
}

#[test]
fn goto_def_on_method_call_receiver_still_navigates() {
    // The method-call member-resolution fallback must not disturb the receiver path:
    // go-to-def on the receiver `s` still lands on its `let s` binder.
    let src = "module M\nlet s = \"hi\"\nlet a = s.ToLowerInvariant()\n";
    let (mut state, uri) = runtime_project_state(src);
    // The receiver `s` in `s.ToLowerInvariant()` at line 2, column 8.
    let loc = run(&mut state, &uri, 2, 8).expect("definition for the receiver `s`");
    assert_eq!(loc.uri, uri);
    assert_eq!(
        loc.range.start,
        Position {
            line: 1,
            character: 4
        }
    );
}

/// Stage 2 of `docs/fsi-signature-restriction-plan.md`: go-to-definition on a
/// cross-file use of a **signature-exposed** value lands on the ident in the
/// `.fsi` (World A — the signature is the declaration), while a sig-hidden
/// sibling yields no location (its export is dropped; D5 silence).
#[test]
fn goto_def_on_sig_exposed_value_lands_in_the_fsi() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    write(
        &proj,
        r#"<Project>
          <ItemGroup>
            <Compile Include="A.fsi" />
            <Compile Include="A.fs" />
            <Compile Include="B.fs" />
          </ItemGroup>
        </Project>"#,
    );
    write(&tmp.path().join("A.fsi"), "module A\nval publicFoo : int\n");
    write(
        &tmp.path().join("A.fs"),
        "module A\nlet publicFoo = 1\nlet hiddenFoo = 2\n",
    );
    let b_src = "module B\nlet u1 = A.publicFoo\nlet u2 = A.hiddenFoo\n";
    let b = tmp.path().join("B.fs");
    write(&b, b_src);

    let b_uri = Url::from_file_path(&b).unwrap();
    let mut state = State::default();
    state.docs.insert(b_uri.clone(), b_src.to_string());

    let (l, c) = cursor_inside(b_src, "publicFoo");
    let loc = run(&mut state, &b_uri, l, c).expect("definition for the sig-exposed value");
    assert_eq!(
        loc.uri,
        Url::from_file_path(tmp.path().join("A.fsi")).unwrap(),
        "the declaration is the signature (World A)"
    );
    // `val publicFoo : int` — the ident on line 1, columns 4..13.
    assert_eq!(
        loc.range,
        Range {
            start: Position {
                line: 1,
                character: 4
            },
            end: Position {
                line: 1,
                character: 13
            },
        }
    );

    let (l, c) = cursor_inside(b_src, "hiddenFoo");
    assert_eq!(
        run(&mut state, &b_uri, l, c),
        None,
        "a sig-hidden value has no definition target (dropped export)"
    );
}
