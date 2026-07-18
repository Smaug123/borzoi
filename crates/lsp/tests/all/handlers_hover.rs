//! Tests for the `textDocument/hover` handler.
//!
//! Pin the body format ("`name` — kind") for each resolution kind. A Deferred /
//! unresolved name now yields an *explanation* of why go-to-definition finds
//! nothing (see [`deferred_resolution_explains_why_definition_is_unavailable`]),
//! not a bare `None`. The referenced-assembly arms (entity / member) are pinned
//! against a real `FSharp.Core.dll` env at the bottom of the file. The JSON-RPC
//! end-to-end test lands with Stage 8.

use std::fs;
use std::path::Path;

use crate::common::{ensure_fsharp_core_dll, ensure_system_runtime_dll, runtime_project_state};
use borzoi::handlers::definition::{entity_definition_document, member_definition_document};
use borzoi::handlers::hover;
use borzoi::handlers::hover::{entity_hover_label, member_hover_label};
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::semantic::SemanticState;
use borzoi::server::State;
use borzoi::workspace::Workspace;
use borzoi_assembly::{Ecma335Assembly, EcmaView};
use borzoi_sema::AssemblyEnv;
use lsp_types::{
    Hover, HoverContents, HoverParams, MarkupKind, Position, TextDocumentIdentifier,
    TextDocumentPositionParams, Url, WorkDoneProgressParams,
};
use tempfile::TempDir;

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn params(uri: &Url, line: u32, character: u32) -> HoverParams {
    HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

fn run(state: &mut State, uri: &Url, line: u32, character: u32) -> Option<Hover> {
    hover::handle(state, params(uri, line, character))
}

fn body(hover: &Hover) -> &str {
    match &hover.contents {
        HoverContents::Markup(m) => {
            assert_eq!(m.kind, MarkupKind::Markdown);
            &m.value
        }
        other => panic!("expected markup contents, got {other:?}"),
    }
}

fn orphan_state(text: &str) -> (State, Url) {
    let mut state = State::default();
    let uri = Url::parse("inmemory:///Sample.fs").unwrap();
    state.docs.insert(uri.clone(), text.to_string());
    (state, uri)
}

#[test]
fn hovers_a_value_let_binding() {
    let src = "let x = 1\n";
    let (mut state, uri) = orphan_state(src);

    // Cursor on the binder `x` (line 0, column 4). Inference types `x` from its
    // literal RHS, so the hover shows the value's type.
    let hover = run(&mut state, &uri, 0, 4).expect("hover for `x`");
    assert_eq!(body(&hover), "`x : int` — value");
    // Range covers the identifier `x` at (0, 4..5).
    let range = hover.range.expect("hover.range pins the symbol");
    assert_eq!(range.start.character, 4);
    assert_eq!(range.end.character, 5);
}

#[test]
fn hovers_a_value_use_with_its_binders_type() {
    // `let y = x` — the *use* `x` resolves by name; its hover shows `x`'s type.
    let src = "let x = 1\nlet y = x\n";
    let (mut state, uri) = orphan_state(src);

    let x_use_col = "let y = ".len() as u32; // the `x` on line 1
    let hover = run(&mut state, &uri, 1, x_use_col).expect("hover for the use `x`");
    assert_eq!(body(&hover), "`x : int` — value");
}

#[test]
fn hovers_a_tuple_value() {
    // `let p = (1, "hi")`: the binder types as a tuple, shown in F# form (3.2b-2).
    let src = "let p = (1, \"hi\")\n";
    let (mut state, uri) = orphan_state(src);
    let hover = run(&mut state, &uri, 0, 4).expect("hover for `p`");
    assert_eq!(body(&hover), "`p : int * string` — value");
}

#[test]
fn hovers_an_if_bound_value() {
    // `let r = if true then 1 else 2`: the if-expression's type (the then-branch)
    // flows to the binder, surfaced on hover (3.2c-1).
    let src = "let r = if true then 1 else 2\n";
    let (mut state, uri) = orphan_state(src);
    let hover = run(&mut state, &uri, 0, 4).expect("hover for `r`");
    assert_eq!(body(&hover), "`r : int` — value");
}

#[test]
fn tuple_expression_hover_is_not_labelled_literal() {
    // Hovering inside the tuple but off any element (the space after the comma)
    // falls to the inferred-type path with the whole-tuple range — no element
    // range covers it. The tuple is a compound expression, so it shows its type
    // *without* the `literal` descriptor (which would misdescribe it).
    let src = "let p = (1, \"hi\")\n";
    let (mut state, uri) = orphan_state(src);
    let space_col = src.find(',').unwrap() as u32 + 1; // the ` ` between elements
    let hover = run(&mut state, &uri, 0, space_col).expect("hover inside the tuple");
    assert_eq!(body(&hover), "`1, \"hi\"` — int * string");
}

#[test]
fn hover_shows_binder_type_not_coerced_use_type() {
    // `let o : obj = s` coerces the *expression* `s` to obj, but the *symbol* `s`
    // is still a string — a symbol hover shows the binder's type. (Inference
    // emits no expression-type for the coerced use, but the binder type is
    // unaffected, so the hover is both correct and useful here.)
    let src = "let s = \"hi\"\nlet o : obj = s\n";
    let (mut state, uri) = orphan_state(src);

    let s_use_col = "let o : obj = ".len() as u32; // the `s` on line 1
    let hover = run(&mut state, &uri, 1, s_use_col).expect("hover for the use `s`");
    assert_eq!(body(&hover), "`s : string` — value");

    // The annotated binder `o` types from its annotation (Stage R2-a): the
    // annotation is an exact equality on the *binder* regardless of `obj`'s
    // subsumption-target role on the RHS.
    let o_col = "let ".len() as u32;
    let o_hover = run(&mut state, &uri, 1, o_col).expect("hover for `o`");
    assert_eq!(body(&o_hover), "`o : obj` — value");
}

#[test]
fn hovers_an_annotated_value_binder() {
    // `let x : int64 = 42`: the binder types from the annotation (Stage R2-a),
    // rendered in the F# display form.
    let src = "let x : int64 = 42\n";
    let (mut state, uri) = orphan_state(src);
    let hover = run(&mut state, &uri, 0, 4).expect("hover for `x`");
    assert_eq!(body(&hover), "`x : int64` — value");
}

#[test]
fn hovers_a_return_annotated_function() {
    // `let h x : int = x`: the return annotation grounds the parameter through
    // the body (Stage R2-c), so the function hovers `int -> int`.
    let src = "let h x : int = x\n";
    let (mut state, uri) = orphan_state(src);
    let hover = run(&mut state, &uri, 0, 4).expect("hover for `h`");
    assert_eq!(body(&hover), "`h : int -> int` — function");
}

#[test]
fn generalised_function_hover_shows_scheme() {
    // `let f x = x` generalises to `'a -> 'a` (Stage 3.2c-2c), so its hover shows
    // the scheme via `Ty::render_fsharp` (`'a`, `'b`, …). (Before generalisation
    // this deferred with the bare `— function` form.)
    let src = "let f x = x\n";
    let (mut state, uri) = orphan_state(src);
    let hover = run(&mut state, &uri, 0, 4).expect("hover for `f`");
    assert_eq!(body(&hover), "`f : 'a -> 'a` — function");
}

#[test]
fn hovers_a_monomorphic_function() {
    // `let f c = if c then 1 else 2`: the condition grounds `c : bool` and the
    // body returns int, so the monomorphic function type `bool -> int` is
    // surfaced on hover (3.2c-2b).
    let src = "let f c = if c then 1 else 2\n";
    let (mut state, uri) = orphan_state(src);

    // Cursor on `f`.
    let hover = run(&mut state, &uri, 0, 4).expect("hover for `f`");
    assert_eq!(body(&hover), "`f : bool -> int` — function");

    // The parameter `c` is grounded to bool only *inside* the function type; it is
    // not published on its own (a standalone parameter type is unsound on
    // ill-typed mid-edit code), so its hover stays the bare form.
    let c_hover = run(&mut state, &uri, 0, 6).expect("hover for `c`");
    assert_eq!(body(&c_hover), "`c` — parameter");
}

#[test]
fn hovers_a_generic_function_scheme() {
    // A two-parameter generic function `let k a b = a` generalises to
    // `'a -> 'b -> 'a` (Stage 3.2c-2c); hover shows the scheme.
    let src = "let k a b = a\n";
    let (mut state, uri) = orphan_state(src);

    // Cursor on `k`.
    let hover = run(&mut state, &uri, 0, 4).expect("hover for `k`");
    assert_eq!(body(&hover), "`k : 'a -> 'b -> 'a` — function");
}

#[test]
fn hovers_a_parameter_reference() {
    let src = "let f x = x\n";
    let (mut state, uri) = orphan_state(src);

    // Cursor on the use `x` in the body (column 10).
    let hover = run(&mut state, &uri, 0, 10).expect("hover for parameter use");
    assert_eq!(body(&hover), "`x` — parameter");
}

#[test]
fn deferred_resolution_explains_why_definition_is_unavailable() {
    // An unbound name is `Deferred(UnboundName)`: there's no binder to describe,
    // so hover now explains *why* go-to-definition finds nothing rather than
    // staying silent. The orphan buffer is single-file, so the degraded note
    // about missing project context is appended.
    let src = "let _ = nowhere\n";
    let (mut state, uri) = orphan_state(src);
    let hover = run(&mut state, &uri, 0, 8).expect("an explanation for the unbound name");
    let body = body(&hover);
    assert!(body.starts_with("**No definition available**"), "{body}");
    assert!(body.contains("AutoOpen"), "{body}");
    assert!(body.contains("without project context"), "{body}");
    // The tooltip is anchored to the identifier `nowhere` (columns 8..15).
    let range = hover.range.expect("hover.range pins the unresolved name");
    assert_eq!((range.start.character, range.end.character), (8, 15));
}

#[test]
fn cursor_off_identifier_returns_none() {
    // Cursor on whitespace — no recorded resolution contains it.
    let src = "let x = 1\n";
    let (mut state, uri) = orphan_state(src);
    assert!(run(&mut state, &uri, 0, 3).is_none());
}

#[test]
fn missing_buffer_returns_none() {
    let mut state = State::default();
    let uri = Url::parse("inmemory:///Missing.fs").unwrap();
    assert!(run(&mut state, &uri, 0, 0).is_none());
}

// ----------------------------------------------------------------------------
// Inferred literal types (Phase 3.1) — the fallback when no name resolves.
// Body format mirrors `format_def`: `` `<literal>` — <fsharp-type> literal ``.
// ----------------------------------------------------------------------------

#[test]
fn hovers_an_int_literal() {
    // `let x = 1`: cursor on the literal `1` (column 8). The binder `x` has a
    // resolution; the literal does not, so this exercises the inferred fallback.
    let (mut state, uri) = orphan_state("let x = 1\n");
    let hover = run(&mut state, &uri, 0, 8).expect("hover for the int literal");
    assert_eq!(body(&hover), "`1` — int literal");
    let range = hover.range.expect("hover.range pins the literal");
    assert_eq!((range.start.character, range.end.character), (8, 9));
}

#[test]
fn hovers_a_string_literal() {
    // `let s = "hi"`: cursor inside the string literal (column 9).
    let (mut state, uri) = orphan_state("let s = \"hi\"\n");
    let hover = run(&mut state, &uri, 0, 9).expect("hover for the string literal");
    assert_eq!(body(&hover), "`\"hi\"` — string literal");
}

#[test]
fn hovers_a_float_literal() {
    let (mut state, uri) = orphan_state("let f = 1.5\n");
    let hover = run(&mut state, &uri, 0, 9).expect("hover for the float literal");
    assert_eq!(body(&hover), "`1.5` — float literal");
}

#[test]
fn hovers_a_byte_string_literal() {
    // Byte strings are `byte[]`, and the F# alias matches the assembly crate's.
    let (mut state, uri) = orphan_state("let b = \"ab\"B\n");
    let hover = run(&mut state, &uri, 0, 9).expect("hover for the byte-string literal");
    assert_eq!(body(&hover), "`\"ab\"B` — byte[] literal");
}

#[test]
fn annotated_literal_hover_returns_none() {
    // `let x : int64 = 1`: the annotation can retarget the literal, so inference
    // defers it (D5) — and there's no resolution on `1`, so hover stays silent.
    let (mut state, uri) = orphan_state("let x : int64 = 1\n");
    let one_col = "let x : int64 = 1\n".find('1').unwrap() as u32;
    assert!(run(&mut state, &uri, 0, one_col).is_none());
}

#[test]
fn cross_file_qualified_reference_hovers_target_binder() {
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

    // Cursor on `foo` (the use) — cross-file `Item` resolution. We get the
    // same hover body whether the cursor is on the use or on the binder,
    // because both refer to the same `Item`.
    let foo_col = b_src.find("foo").unwrap() as u32;
    let hover = run(&mut state, &b_uri, 1, foo_col).expect("cross-file hover");
    assert_eq!(body(&hover), "`foo` — value");
}

/// The project hover folds only the Compile **prefix** up to the cursor's file,
/// not the whole project: a cross-file hover on file 1 of a three-file project
/// resolves against files [0, 1] (the target binder is in file 0) and leaves
/// file 2 unfolded. Pins the resolution-slice wiring
/// (`resolved_prefix_and_env_for` with the file's index, not `usize::MAX`).
#[test]
fn project_hover_folds_only_the_prefix_up_to_the_cursor_file() {
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
    write(&a, "module Shared\nlet foo = 1\n");
    let b_src = "module Other\nlet bar = Shared.foo\n";
    write(&b, b_src);
    write(&c, "module Third\nlet baz = Other.bar\n");

    let b_uri = Url::from_file_path(&b).unwrap();
    let mut state = State::default();
    state.docs.insert(b_uri.clone(), b_src.to_string());

    // Cursor on `foo` in B (file 1) — cross-file `Item` resolving into A (file 0).
    let foo_col = b_src.find("foo").unwrap() as u32;
    let hover = run(&mut state, &b_uri, 1, foo_col).expect("cross-file hover");
    assert_eq!(body(&hover), "`foo` — value");

    // The fold covered only the prefix [A, B]; C (file 2) was never folded.
    assert_eq!(
        state.semantic.cached_resolved_len(&proj),
        Some(2),
        "hover on file 1 folds the [0, 1] prefix, not file 2"
    );
}

#[test]
fn project_file_literal_hover_shows_inferred_type() {
    // Exercises the inferred-type fallback through the *project* hover path.
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    let a = tmp.path().join("A.fs");
    write(
        &proj,
        r#"<Project>
          <ItemGroup>
            <Compile Include="A.fs" />
          </ItemGroup>
        </Project>"#,
    );
    let a_src = "module Sample\nlet answer = 42\n";
    write(&a, a_src);

    let a_uri = Url::from_file_path(&a).unwrap();
    let mut state = State::default();
    state.docs.insert(a_uri.clone(), a_src.to_string());

    // Cursor on the literal `42` (line 1). `answer` resolves; `42` does not, so
    // the project hover falls back to the inferred literal type.
    let col = a_src.lines().nth(1).unwrap().find("42").unwrap() as u32;
    let hover = run(&mut state, &a_uri, 1, col).expect("project literal hover");
    assert_eq!(body(&hover), "`42` — int literal");

    // Cursor on the binder `answer`: the project hover enriches the resolved
    // name with its inferred type.
    let answer_col = a_src.lines().nth(1).unwrap().find("answer").unwrap() as u32;
    let answer_hover = run(&mut state, &a_uri, 1, answer_col).expect("project binder hover");
    assert_eq!(body(&answer_hover), "`answer : int` — value");
}

// ----------------------------------------------------------------------------
// Referenced-assembly entity / member labels, pinned against a real
// `FSharp.Core.dll`. Driving the full `hover::handle` path to a
// `Resolution::Entity`/`Member` needs a restored project (`project.assets.json`
// + an SDK); these exercise the formatting that path delegates to directly,
// against the same `AssemblyEnv` the handler would build.
// ----------------------------------------------------------------------------

fn ns(segments: &[&str]) -> Vec<String> {
    segments.iter().map(|s| s.to_string()).collect()
}

/// A real `AssemblyEnv` over the shipped `FSharp.Core.dll` (built once via the
/// `tools/fcs-dump` helper). Requires the .NET SDK on PATH — the Nix devShell
/// provides it.
fn fsharp_core_env() -> AssemblyEnv {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let entities = Ecma335Assembly::parse(&bytes)
        .expect("parse FSharp.Core")
        .enumerate_type_defs()
        .expect("enumerate FSharp.Core");
    AssemblyEnv::from_entities(entities)
}

/// As [`fsharp_core_env`], but built with [`AssemblyEnv::from_assemblies`] so the
/// env *records the DLL path* — required for `member_definition_document` /
/// `entity_definition_document` to locate and read the PDB.
fn fsharp_core_env_with_path() -> AssemblyEnv {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let entities = Ecma335Assembly::parse(&bytes)
        .expect("parse FSharp.Core")
        .enumerate_type_defs()
        .expect("enumerate FSharp.Core");
    AssemblyEnv::from_assemblies(vec![(dll, entities)])
}

#[test]
fn definition_document_reports_where_member_and_entity_live() {
    // The "say where it is" path behind the hover `Defined in …` line: from a
    // real path-bearing env, a member and its module both resolve to their
    // source document + line via the DLL's PDB.
    let env = fsharp_core_env_with_path();
    let module = env
        .lookup_type(
            &ns(&["Microsoft", "FSharp", "Core"]),
            "ExtraTopLevelOperators",
            0,
        )
        .expect("ExtraTopLevelOperators resolves");
    let printfn = env.member(module, "printfn").expect("printfn member");

    // The PDB-image cache (shared with go-to-definition) lives on `SemanticState`.
    let mut semantic = SemanticState::default();
    let member_doc = member_definition_document(&mut semantic, &env, module, printfn)
        .expect("printfn has a location");
    assert!(
        member_doc.document.ends_with("fslib-extra-pervasives.fs"),
        "got {}",
        member_doc.document
    );
    assert!(member_doc.line >= 1);

    let entity_doc =
        entity_definition_document(&mut semantic, &env, module).expect("the module has a location");
    assert!(
        entity_doc.document.ends_with("fslib-extra-pervasives.fs"),
        "got {}",
        entity_doc.document
    );
}

#[test]
fn entity_hover_label_renders_declaration_namespace_and_provenance() {
    let env = fsharp_core_env();
    let module = env
        .lookup_type(
            &ns(&["Microsoft", "FSharp", "Core"]),
            "ExtraTopLevelOperators",
            0,
        )
        .expect("ExtraTopLevelOperators resolves");
    let body = entity_hover_label(&env, module);
    // `ExtraTopLevelOperators` is `[<AutoOpen>] module` in FSharp.Core: the
    // declaration head carries the attribute + `module` keyword, the namespace
    // is the context line (no kind qualifier — `module` carries the kind).
    let mut lines = body.split("\n\n");
    assert_eq!(
        lines.next(),
        Some("`[<AutoOpen>] module ExtraTopLevelOperators`")
    );
    assert!(
        body.contains("\n\nin Microsoft.FSharp.Core\n\n"),
        "expected the namespace context line, got:\n{body}"
    );
    assert!(
        body.contains("\n\nfrom FSharp.Core v"),
        "expected an FSharp.Core provenance line, got:\n{body}"
    );
}

#[test]
fn struct_union_entity_renders_struct_attr_and_union_kind() {
    // `ValueOption<'T>` is a `[<Struct>]` DU: the model keeps the F# kind `Union`
    // and sets `is_struct`. The declaration head carries `[<Struct>] type`; the
    // `union` kind (which `type` collapses) rides on the context line. The env
    // keys F# types by their *source* name (`ValueOption`, not the compiled
    // `FSharpValueOption`).
    let env = fsharp_core_env();
    let value_option = env
        .lookup_type(&ns(&["Microsoft", "FSharp", "Core"]), "ValueOption", 1)
        .expect("ValueOption<'T> resolves");
    let body = entity_hover_label(&env, value_option);
    let mut lines = body.split("\n\n");
    assert_eq!(lines.next(), Some("`[<Struct>] type ValueOption<'T>`"));
    assert!(
        body.contains("\n\nunion, in Microsoft.FSharp.Core\n\n"),
        "expected the `union` kind + namespace context line, got:\n{body}"
    );
}

#[test]
fn member_hover_label_renders_signature_declaring_type_and_provenance() {
    let env = fsharp_core_env();
    let module = env
        .lookup_type(
            &ns(&["Microsoft", "FSharp", "Core"]),
            "ExtraTopLevelOperators",
            0,
        )
        .expect("ExtraTopLevelOperators resolves");
    // `printfn` is the F# source name (the IL method is `PrintFormatLine`); a
    // module-level function renders as `val`, the signature is the head, and the
    // declaring module is the context line. The full signature is version-
    // sensitive, so assert its shape rather than pin the whole type.
    let printfn = env.member(module, "printfn").expect("printfn member");
    let body = member_hover_label(&env, module, printfn);
    let head = body.split("\n\n").next().expect("head line");
    assert!(
        head.starts_with("`val printfn") && head.ends_with('`'),
        "expected a `val printfn` signature head, got: {head}"
    );
    assert!(
        body.contains("\n\nin Microsoft.FSharp.Core.ExtraTopLevelOperators\n\n"),
        "expected the declaring-module context line, got:\n{body}"
    );
    assert!(
        body.contains("\n\nfrom FSharp.Core v"),
        "expected an FSharp.Core provenance line, got:\n{body}"
    );
}

#[test]
fn rqa_attribute_renders_on_non_module_kinds() {
    // FSharp.Core ships its own `DynamicallyAccessedMemberTypes` enum carrying
    // `[<RequireQualifiedAccess>]`. The attribute is valid well beyond
    // modules/unions, so the declaration head must surface it here — the
    // regression a P2 review flagged when the prefix was gated to `Module`/`Union`.
    let env = fsharp_core_env();
    let enum_handle = env
        .lookup_type(
            &ns(&["System", "Diagnostics", "CodeAnalysis"]),
            "DynamicallyAccessedMemberTypes",
            0,
        )
        .expect("DynamicallyAccessedMemberTypes resolves");
    let body = entity_hover_label(&env, enum_handle);
    let mut lines = body.split("\n\n");
    assert_eq!(
        lines.next(),
        Some("`[<RequireQualifiedAccess>] type DynamicallyAccessedMemberTypes`")
    );
    assert!(
        body.contains("\n\nenum, in System.Diagnostics.CodeAnalysis\n\n"),
        "expected the `enum` kind + namespace context line, got:\n{body}"
    );
}

/// A minimal `project.assets.json` listing the `Microsoft.NETCore.App` framework
/// reference for `net10.0` — enough for `build_assembly_env` to enumerate the
/// stubbed framework pack's DLLs. No package references, but a (dummy)
/// `packageFolders` entry is required — the resolver rejects an assets file with
/// none (`PackageFolderMissing`).
fn minimal_assets_json(package_folder: &Path) -> String {
    serde_json::json!({
        "version": 3,
        "targets": { "net10.0": {} },
        "libraries": {},
        "packageFolders": { package_folder.to_str().unwrap(): {} },
        "project": {
            "frameworks": {
                "net10.0": { "frameworkReferences": { "Microsoft.NETCore.App": {} } }
            }
        }
    })
    .to_string()
}

#[test]
fn member_access_hover_shows_int_via_project_assembly_env() {
    // Stage 3.3a end-to-end through the project hover path: `let n = s.Length`
    // hovers `n : int`, driven by inference resolving `System.String.Length`
    // against the project's `AssemblyEnv`. We stub a self-contained `dotnet_root`
    // whose `Microsoft.NETCore.App.Ref` pack holds a *real* `System.Runtime.dll`
    // (so `System.String` and its `Length` property are present), point the temp
    // project's `obj/project.assets.json` at that framework, and wire the stub as
    // the workspace's `dotnet_root`.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Stubbed dotnet_root with a real System.Runtime.dll in the ref pack.
    let dotnet_root = root.join("dotnet");
    let pack = dotnet_root
        .join("packs")
        .join("Microsoft.NETCore.App.Ref")
        .join("10.0.0")
        .join("ref")
        .join("net10.0");
    fs::create_dir_all(&pack).unwrap();
    let real_runtime = ensure_system_runtime_dll();
    fs::copy(&real_runtime, pack.join("System.Runtime.dll"))
        .unwrap_or_else(|e| panic!("copy System.Runtime.dll: {e}"));

    // A dummy NuGet package folder (no packages, but the assets file must name at
    // least one — the resolver rejects zero package folders).
    let pkgs = root.join("pkgs");
    fs::create_dir_all(&pkgs).unwrap();

    // A temp project that lists the source file and is "restored" (assets present).
    let proj = root.join("P.fsproj");
    let src_path = root.join("Lib.fs");
    let src = "module M\nlet s = \"hi\"\nlet n = s.Length\n";
    write(
        &proj,
        r#"<Project>
          <ItemGroup><Compile Include="Lib.fs" /></ItemGroup>
        </Project>"#,
    );
    write(&src_path, src);
    write(
        &root.join("obj").join("project.assets.json"),
        &minimal_assets_json(&pkgs),
    );

    // Wire the stubbed dotnet_root through SDK discovery so
    // `dotnet_root_for_project` (which falls back to the discovery root) returns it.
    let env = SdkDiscoveryEnv {
        dotnet_root: Some(dotnet_root),
        ..SdkDiscoveryEnv::default()
    };
    let mut state = State::default();
    state.workspace = Workspace::with_env(env);
    let uri = Url::from_file_path(&src_path).unwrap();
    state.docs.insert(uri.clone(), src.to_string());

    // Hover on the binder `n` (line 2, `let n = …`, `n` at column 4).
    let hover = run(&mut state, &uri, 2, 4).expect("hover for `n`");
    assert_eq!(body(&hover), "`n : int` — value");
}

#[test]
fn member_name_hover_shows_the_member_via_inference() {
    // Stage 3.3b: hover on `Length` in `s.Length` shows the *member* rendering an
    // assembly-path member gets — the same `Resolution::Member` path — not just
    // the whole access's `int` type. The resolver records `Length` as
    // `Deferred(QualifiedAccess)`; inference's member-resolution side-table
    // supplies the identity.
    let src = "module M\nlet s = \"hi\"\nlet n = s.Length\n";
    let (mut state, uri) = runtime_project_state(src);
    // `Length` starts at column 10 on line 2 (`let n = s.Length`).
    let hover = run(&mut state, &uri, 2, 12).expect("hover for `Length`");
    let body = body(&hover);
    // The member head: an F# property signature for `System.String.Length`.
    assert!(
        body.starts_with("`member Length:") || body.starts_with("`member Length "),
        "expected a member signature head, got:\n{body}"
    );
    assert!(
        body.contains("System.String"),
        "expected the declaring type `System.String`, got:\n{body}"
    );
}

#[test]
fn member_name_hover_for_literal_receiver() {
    // The `DotGet` shape (`"hi".Length`) surfaces the member too.
    let src = "module M\nlet n = \"hi\".Length\n";
    let (mut state, uri) = runtime_project_state(src);
    // `Length` in `"hi".Length` — after `let n = "hi".`, at column 13 on line 1.
    let hover = run(&mut state, &uri, 1, 14).expect("hover for `Length`");
    let body = body(&hover);
    assert!(
        body.contains("Length") && body.contains("System.String"),
        "expected the String.Length member, got:\n{body}"
    );
}

#[test]
fn method_call_binder_hover_shows_return_type() {
    // Stage 3.3d: `let a = s.ToLowerInvariant()` hovers `a : string`, driven by
    // inference typing the single-candidate instance method call as its return type.
    let src = "module M\nlet s = \"hi\"\nlet a = s.ToLowerInvariant()\n";
    let (mut state, uri) = runtime_project_state(src);
    // The binder `a` on line 2 (`let a = …`), column 4.
    let hover = run(&mut state, &uri, 2, 4).expect("hover for `a`");
    assert_eq!(body(&hover), "`a : string` — value");
}

#[test]
fn method_name_hover_shows_the_method_via_inference() {
    // Stage 3.3d: hover on a *called* method name shows the member rendering — the
    // method is recorded in the same `member_resolutions` side-table 3.3b uses for
    // fields, so the LSP hover path is unchanged.
    let src = "module M\nlet s = \"hi\"\nlet a = s.ToLowerInvariant()\n";
    let (mut state, uri) = runtime_project_state(src);
    // `ToLowerInvariant` starts at column 10 on line 2 (`let a = s.`); cursor inside.
    let hover = run(&mut state, &uri, 2, 12).expect("hover for `ToLowerInvariant`");
    let body = body(&hover);
    assert!(
        body.contains("ToLowerInvariant") && body.contains("System.String"),
        "expected the String.ToLowerInvariant member, got:\n{body}"
    );
}

#[test]
fn static_call_binder_hover_shows_return_type() {
    // Stage OV-7: `let b = System.String.IsNullOrEmpty "x"` hovers `b : bool`,
    // driven by the overload engine typing the static call as its return type.
    let src = "module M\nlet b = System.String.IsNullOrEmpty \"x\"\n";
    let (mut state, uri) = runtime_project_state(src);
    // The binder `b` on line 1 (`let b = …`), column 4.
    let hover = run(&mut state, &uri, 1, 4).expect("hover for `b`");
    assert_eq!(body(&hover), "`b : bool` — value");
}

#[test]
fn static_method_name_hover_shows_the_overload_via_inference() {
    // Stage OV-7: the resolver leaves an *overloaded* static name
    // (`String.Compare`) as `Deferred(QualifiedAccess)`; a committed overload
    // wake records it in `member_resolutions`, so hover lights up through the
    // same member path.
    let src = "module M\nlet c = System.String.Compare(\"a\", \"b\")\n";
    let (mut state, uri) = runtime_project_state(src);
    // `Compare` starts at column 22 on line 1; cursor inside.
    let hover = run(&mut state, &uri, 1, 24).expect("hover for `Compare`");
    let body = body(&hover);
    assert!(
        body.contains("Compare") && body.contains("System.String"),
        "expected the String.Compare member, got:\n{body}"
    );
}
