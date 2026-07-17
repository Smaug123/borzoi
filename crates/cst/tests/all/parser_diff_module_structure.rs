//! Differential test (`parser::parse` vs FCS): module structure — `open`
//! declarations, named-module & namespace headers, and attributes. Split out
//! of the former monolithic `parser_diff.rs`.

use crate::common::{
    assert_asts_match, assert_asts_match_allow_errors, assert_asts_match_fcs_accepts_ours_rejects,
    assert_asts_match_fcs_rejects_ours_accepts, assert_asts_match_with_diagnostic,
};

// ---- Phase 8.1: `open` declarations ------------------------------------

/// Phase 8.1 — `open Foo` with a single-segment path. Projects to
/// `SynModuleDecl.Open(SynOpenDeclTarget.ModuleOrNamespace(SynLongIdent
/// [System], _), _)` (`pars.fsy:1396`). `open` opens no LexFilter context,
/// so it arrives as a plain `Token::Open` in the filtered stream like any
/// other statement.
#[test]
fn diff_ast_open_single() {
    assert_asts_match("open System\nlet x = 1\n");
}

/// Phase 8.1 — dotted `open A.B.C`. The target's `SynLongIdent` carries the
/// full path; the parser builds a bare `LONG_IDENT` child of `OPEN_DECL`.
#[test]
fn diff_ast_open_dotted() {
    assert_asts_match("open System.Collections.Generic\nlet x = 1\n");
}

/// Phase 8.1 — `open` as the sole declaration in the file (no trailing
/// decl). Confirms the decl loop closes cleanly with an Open at the end.
#[test]
fn diff_ast_open_only() {
    assert_asts_match("open System\n");
}

/// Phase 8.1 — two consecutive `open`s. Each is its own `SynModuleDecl.Open`
/// separated by the LexFilter `OBLOCKSEP`.
#[test]
fn diff_ast_open_multiple() {
    assert_asts_match("open System\nopen System.Text\nlet x = 1\n");
}

/// Phase 8.1 — `open global.System`. `global` is a valid leading path
/// segment (FCS's `path` production). FCS stores it as an ident with
/// `idText = "`global`"` and `IdentTrivia.OriginalNotation "global"`; the
/// normaliser prefers the original notation, so both sides project
/// `["global", "System"]`.
#[test]
fn diff_ast_open_global() {
    assert_asts_match("open global.System\nlet x = 1\n");
}

/// Phase 8.1 — bare `open global`. The single-segment `global` head on its
/// own, with no trailing decl.
#[test]
fn diff_ast_open_global_bare() {
    assert_asts_match("open global\n");
}

/// Phase 8.1 — `open type System.Math`. The `type` keyword is *swallowed*
/// by LexFilter (it pushes a transient `CtxtTypeDefns`), so the parser
/// recovers it from the raw stream and then parses the trailing type via
/// `parse_type` — `SynOpenDeclTarget.Type(SynType)` (`pars.fsy:1402`).
#[test]
fn diff_ast_open_type() {
    assert_asts_match("open type System.Math\nlet x = 1\n");
}

/// Phase 8.1 — `open type` with a `global`-rooted target
/// (`open type global.System.Math`). FCS admits `global` as the type-path root
/// (its head segment's `idText` is the single-backtick-quoted `` `global` ``),
/// so the target is `Type(LongIdent(["global"; "System"; "Math"]))`.
#[test]
fn diff_ast_open_type_global_qualified() {
    assert_asts_match("open type global.System.Math\nlet x = 1\n");
}

/// Phase 8.1 — `open type` with a postfix-application target (`int list`).
/// FCS's `openDecl` accepts `appTypeWithoutNull`, which includes postfix app,
/// so the target is `App(LongIdent list, [int])`.
#[test]
fn diff_ast_open_type_app() {
    assert_asts_match("open type System.Collections.Generic.List<int>\nlet x = 1\n");
}

/// Phase 8.1 — `open type int[]`. The array suffix is part of
/// `appTypeWithoutNull`, so it stays in the open target (`Array`).
#[test]
fn diff_ast_open_type_array() {
    assert_asts_match("open type int[]\nlet x = 1\n");
}

/// Phase 8.1 — `open type A -> B`. The arrow is *not* part of
/// `appTypeWithoutNull`: FCS opens just `A` (`Open(Type(LongIdent ["A"]))`)
/// and the trailing `-> B` is a parse error. We mirror both — the open
/// target stops at `A` and the dangling `->` is recovered as an error — so
/// the projected decls match (`allow_errors` since FCS diagnoses too).
#[test]
fn diff_ast_open_type_arrow_terminates() {
    assert_asts_match_allow_errors("open type A -> B\n");
}

/// Phase 8.1 — `open type int * string`. The tuple `*` is likewise outside
/// `appTypeWithoutNull`: the target is just `int`, `* string` errors.
#[test]
fn diff_ast_open_type_star_terminates() {
    assert_asts_match_allow_errors("open type int * string\n");
}

/// Phase 8.1 — `open` with its path on an indented continuation line. FCS
/// accepts this (LexFilter emits no `OBLOCKSEP` before the indented
/// `System`, so it continues the same statement), yielding
/// `ModuleOrNamespace [System]`. Guards the layout gate in `parse_open_decl`
/// against over-rejecting a legitimately-continued path.
#[test]
fn diff_ast_open_continuation_line() {
    assert_asts_match("open\n    System\nlet x = 1\n");
}

// ---- Phase 8.2: named-module & namespace headers -----------------------

/// Phase 8.2 — whole-file `module Foo` (no `=`). FCS produces a single
/// `SynModuleOrNamespace([Foo], isRecursive=false, NamedModule, decls=[Let])`
/// (`pars.fsy:536 moduleIntro`; verified against `fcs-dump ast`). The
/// swallowed raw `module` is recovered as `MODULE_TOK`, the name lands in a
/// bare `LONG_IDENT`, and the body `let` flows in as the module's decls.
#[test]
fn diff_ast_module_named() {
    assert_asts_match("module Foo\nlet x = 1\n");
}

/// Phase 8.2 — `module Foo` with no body: a `NamedModule` with an empty
/// decls list. Pins that the header parser composes with an empty body.
#[test]
fn diff_ast_module_named_empty_body() {
    assert_asts_match("module Foo\n");
}

/// Phase 8.2 — dotted `module Foo.Bar.Baz`. Every segment lands in the
/// header `longId`.
#[test]
fn diff_ast_module_named_dotted() {
    assert_asts_match("module Foo.Bar.Baz\nlet x = 1\n");
}

/// Phase 8.2 — `module rec Foo` sets `SynModuleOrNamespace.isRecursive`. The
/// `rec` keyword passes through raw and is claimed as `REC_TOK`.
#[test]
fn diff_ast_module_named_rec() {
    assert_asts_match("module rec Foo\nlet x = 1\n");
}

/// Phase 8.2 — `module internal Foo`. The access modifier is consumed as
/// `ACCESS_TOK` (kept out of ERROR) and projected as the module's
/// accessibility (`SynModuleOrNamespace` field 6), so the diff now verifies the
/// `internal` lands on the header rather than being silently dropped.
#[test]
fn diff_ast_module_named_internal() {
    assert_asts_match("module internal Foo\nlet x = 1\n");
}

/// Phase 8.2 — `module private Foo`. Companion to the `internal` case for
/// the other common access modifier.
#[test]
fn diff_ast_module_named_private() {
    assert_asts_match("module private Foo\nlet x = 1\n");
}

/// Phase 8.2 — `namespace Foo` is a `DeclaredNamespace`. Unlike `module`,
/// `namespace` is not swallowed by LexFilter — it reaches the parser as a
/// real `Token::Namespace` and is claimed as `NAMESPACE_TOK`. A `let` in a
/// namespace is accepted at parse time (FCS rejects it only semantically).
#[test]
fn diff_ast_namespace_declared() {
    assert_asts_match("namespace Foo\nlet x = 1\n");
}

/// Phase 8.2 — dotted `namespace Foo.Bar`.
#[test]
fn diff_ast_namespace_dotted() {
    assert_asts_match("namespace Foo.Bar\nlet x = 1\n");
}

/// Phase 8.2 — `namespace global` is a `GlobalNamespace` with an empty
/// `longId` (FCS's post-parse pass strips the sole `global` segment,
/// `ParseAndCheckInputs.fs:154-164`). The bare `global` is emitted as
/// `GLOBAL_TOK` rather than a path.
#[test]
fn diff_ast_namespace_global() {
    assert_asts_match("namespace global\nlet x = 1\n");
}

/// Phase 8.2 — `namespace rec A.B` sets `isRecursive` and carries the dotted
/// path.
#[test]
fn diff_ast_namespace_rec_dotted() {
    assert_asts_match("namespace rec A.B\nlet x = 1\n");
}

/// Phase 8.2 — a namespace whose body is an `open` (rather than a `let`),
/// exercising the header + the phase-8.1 decl together.
#[test]
fn diff_ast_namespace_with_open_body() {
    assert_asts_match("namespace Foo.Bar\nopen System\n");
}

// ---- Phase 8.3: multiple namespaces per file ---------------------------

/// Phase 8.3 — two `namespace` blocks, each with a body decl, produce **two**
/// `SynModuleOrNamespace` entries in `ParsedImplFileInput.contents` (verified
/// against `fcs-dump ast`). The file-level loop starts a fresh
/// `MODULE_OR_NAMESPACE` at each `namespace`; LexFilter already segments them
/// with an `OBLOCKSEP`.
#[test]
fn diff_ast_two_namespaces_with_decls() {
    assert_asts_match("namespace A\nlet x = 1\nnamespace B\nlet y = 2\n");
}

/// Phase 8.3 — three namespaces, to confirm the file loop iterates beyond two.
#[test]
fn diff_ast_three_namespaces() {
    assert_asts_match("namespace A\nlet x = 1\nnamespace B\nlet y = 2\nnamespace C\nlet z = 3\n");
}

/// Phase 8.3 — two bare namespaces with empty bodies. Each is still its own
/// `DeclaredNamespace` (with an empty decls list).
#[test]
fn diff_ast_two_bare_namespaces() {
    assert_asts_match("namespace A\nnamespace B\n");
}

/// Phase 8.3 — dotted namespace paths across the segment boundary.
#[test]
fn diff_ast_two_dotted_namespaces() {
    assert_asts_match("namespace A.B\nlet x = 1\nnamespace C.D\nlet y = 2\n");
}

// ---- Decls before the first `namespace` (FCS FS0222) --------------------
//
// "Only '#' compiler directives may occur prior to the first 'namespace'
// declaration." A non-empty anonymous prefix before a `namespace` is an FCS
// error; FCS drops the leading decls and keeps only the namespace(s). Our parser
// wraps the prefix in an `ERROR` node (not projected as a module) and flags it,
// so both sides project the same namespace-only shape. `#`-directives are trivia
// and stay legal before a namespace.

/// An `open` before the first `namespace` — dropped; one `DeclaredNamespace`.
#[test]
fn diff_ast_open_before_namespace_is_error() {
    assert_asts_match_allow_errors("open System\nnamespace N\n");
}

/// A `let` before the first `namespace` — likewise dropped.
#[test]
fn diff_ast_let_before_namespace_is_error() {
    assert_asts_match_allow_errors("let x = 1\nnamespace N\n");
}

/// A prefix before *two* namespaces — both namespaces survive, the prefix is
/// dropped (the file loop keeps iterating past the dropped prefix).
#[test]
fn diff_ast_prefix_before_two_namespaces_is_error() {
    assert_asts_match_allow_errors("open System\nnamespace A\nnamespace B\n");
}

/// A warning directive before a `namespace` is *legal* parsed-file trivia — one
/// `DeclaredNamespace`, no error. Guards that the FS0222 check doesn't over-fire
/// on directive prefixes.
#[test]
fn diff_ast_directive_before_namespace_is_ok() {
    assert_asts_match("#nowarn \"57\"\nnamespace N\n");
}

/// Ordinary `#` compiler directives before the first `namespace` are legal and
/// omitted from the projected module list, not an FS0222 anonymous prefix.
#[test]
fn diff_ast_hash_directive_before_namespace_is_ok() {
    assert_asts_match("#I \"/tmp\"\nnamespace N\n");
}

/// An *attribute-only* prefix before a `namespace` (`[<AutoOpen>]⏎namespace N`)
/// is also illegal — the deferred-attribute arm marks content as begun, so the
/// prefix is dropped (one `DeclaredNamespace`), not left as an anon module.
#[test]
fn diff_ast_attr_prefix_before_namespace_is_error() {
    assert_asts_match_allow_errors("[<AutoOpen>]\nnamespace N\n");
}

// ---- File-form mixing: whole-file `module` header + `namespace` ---------
//
// A whole-file `module M` header (no `=`) cannot coexist with a `namespace` — a
// file is either module-headed or namespaced. FCS bails the whole file to a
// single empty `AnonModule`, dropping the module header, its body, AND the
// namespace. Our parser wraps all parsed segments in one outer (empty)
// `AnonModule` + flags it, matching that module-and-namespace-free projection.

/// A whole-file `module M` header followed by a `namespace` → one empty
/// `AnonModule` (both dropped).
#[test]
fn diff_ast_module_header_then_namespace_is_error() {
    assert_asts_match_allow_errors("module M\nnamespace N\n");
}

/// The module's own body is dropped too — still one empty `AnonModule`.
#[test]
fn diff_ast_module_with_body_then_namespace_is_error() {
    assert_asts_match_allow_errors("module M\nlet x = 1\nnamespace N\n");
}

/// A dotted whole-file `module M.Sub` header before a namespace — same recovery.
#[test]
fn diff_ast_dotted_module_header_then_namespace_is_error() {
    assert_asts_match_allow_errors("module M.Sub\nnamespace P\n");
}

/// An *attributed* whole-file module header before a namespace
/// (`[<AutoOpen>]⏎module M⏎namespace N`) is still file-form mixing — the leading
/// `[<` doesn't hide the head from the detector — so it bails to one empty
/// `AnonModule` too.
#[test]
fn diff_ast_attr_module_header_then_namespace_is_error() {
    assert_asts_match_allow_errors("[<AutoOpen>]\nmodule M\nnamespace N\n");
}

/// Phase 8.3 × 8.4 — a namespace whose body is a *nested module*, then a
/// second namespace. Exercises the nested-module detection (and its swallowed-
/// `module` keyword preservation) inside a namespace that is not the last
/// file segment.
#[test]
fn diff_ast_namespace_with_nested_module_then_namespace() {
    assert_asts_match("namespace A\nmodule M =\n    let x = 1\nnamespace B\nlet y = 2\n");
}

/// Phase 8.3 — a namespace whose body is an `open`, followed by a second
/// namespace.
#[test]
fn diff_ast_two_namespaces_open_body() {
    assert_asts_match("namespace A\nopen System\nnamespace B\nopen System.IO\n");
}

// ---- Phase 8.4: nested modules (`module X = <block>`) ------------------

/// Phase 8.4 — the ubiquitous `module Foo =\n  <indented body>` shape. FCS
/// wraps the whole file in the implicit `AnonModule`, whose single decl is a
/// `SynModuleDecl.NestedModule(SynComponentInfo([Foo], …), isRecursive=false,
/// decls=[Let], …)` (`pars.fsy:1305` → `namedModuleDefnBlock`; verified
/// against `fcs-dump ast`). The swallowed raw `module` is recovered as
/// `MODULE_TOK`, the name lands in a `LONG_IDENT`, and the offside body parses
/// via the shared `parse_module_decls` loop.
#[test]
fn diff_ast_nested_module_let() {
    assert_asts_match("module Foo =\n    let x = 1\n");
}

/// Phase 8.4 — a multi-decl body. Each inner `let` ends in the
/// `OBLOCKEND·ODECLEND` binding-terminator pair; only the trailing lone
/// `OBLOCKEND` closes the body. Pins that the body loop walks several decls
/// before terminating.
#[test]
fn diff_ast_nested_module_multi_decl() {
    assert_asts_match("module Foo =\n    let x = 1\n    let y = 2\n");
}

/// Phase 8.4 — a bare-expression body (`SynModuleDecl.Expr`). An expr decl
/// leaves *no* trailing `OBLOCKEND`, so the body's lone `OBLOCKEND` is the
/// first the loop sees — exercises the other side of the binding-vs-body
/// terminator distinction.
#[test]
fn diff_ast_nested_module_expr_body() {
    assert_asts_match("module Foo =\n    printfn \"hi\"\n");
}

/// Phase 8.4 — an `open` body decl inside a nested module (phase-8.1 decl
/// nested under the phase-8.4 header).
#[test]
fn diff_ast_nested_module_open_body() {
    assert_asts_match("module Foo =\n    open System\n    let x = 1\n");
}

/// Phase 8.4 — an empty body (`module Foo =` with nothing indented under it):
/// the `OBLOCKBEGIN` is immediately followed by the body-closing `OBLOCKEND`,
/// so the loop terminates with zero decls. FCS still emits a `NestedModule`
/// with an empty decls list, but reports the offside FS0058 (the body block's
/// anchor — the EOF — is offside of the `module …=` head); since the §A offside
/// emission landed we report the matching FS0058 at that same span while
/// recovering the identical empty-body tree.
#[test]
fn diff_ast_nested_module_empty_body() {
    assert_asts_match_with_diagnostic("module Foo =\n", 58);
}

/// Phase 8.4 — `module rec Inner =` sets `SynModuleDecl.NestedModule`'s
/// `isRecursive`. The `rec` keyword passes through raw and is claimed as
/// `REC_TOK`.
#[test]
fn diff_ast_nested_module_rec() {
    assert_asts_match("module rec Inner =\n    let x = 1\n");
}

/// Phase 8.4 — a nested module inside a whole-file `NamedModule` header
/// (phase 8.2 header + phase 8.4 nested decl).
#[test]
fn diff_ast_nested_module_in_named() {
    assert_asts_match("module Top\nmodule Foo =\n    let x = 1\n");
}

/// Phase 8.4 — a nested module inside a `namespace` (the common real-world
/// shape: a namespace whose sole decl is a module).
#[test]
fn diff_ast_nested_module_in_namespace() {
    assert_asts_match("namespace N\nmodule Foo =\n    let x = 1\n");
}

/// Phase 8.4 — a doubly-nested `module A =\n module B =\n  let x = 1`. The
/// closing run collapses to three adjacent `OBLOCKEND`s (the inner `let`'s,
/// `B`'s body, `A`'s body); recursion + the adjacency terminator unwind them
/// to the correct nesting.
#[test]
fn diff_ast_nested_module_doubly_nested() {
    assert_asts_match("module A =\n    module B =\n        let x = 1\n");
}

/// Phase 8.4 — two sibling nested modules at the file top level (each its own
/// `NestedModule` in the `AnonModule`'s decls). Pins that the loop resumes
/// after a nested module's body-closing `OBLOCKEND` to parse the next sibling.
#[test]
fn diff_ast_nested_module_two_siblings() {
    assert_asts_match("module A =\n    let x = 1\nmodule B =\n    let y = 2\n");
}

/// Phase 8.4 — a nested module followed by a sibling top-level `let`. The
/// body terminates at its `OBLOCKEND`, and the col-0 `let` parses as a
/// sibling `AnonModule` decl.
#[test]
fn diff_ast_nested_module_then_sibling_let() {
    assert_asts_match("module Foo =\n    let x = 1\nlet z = 2\n");
}

/// Phase 8.4 — `module Foo =` immediately followed by a col-0 `let`: the
/// module body is *empty* (the un-indented `let` is not part of it) and the
/// `let` is a sibling. Pins the layout-driven empty-body case. FCS reports the
/// offside FS0058 (the col-0 `let` is offside of the `module …=` head's body
/// block); since the §A offside emission landed we report the matching FS0058
/// while recovering the identical empty-body-plus-sibling tree.
#[test]
fn diff_ast_nested_module_empty_then_col0_let() {
    assert_asts_match_with_diagnostic("module Foo =\nlet z = 1\n", 58);
}

/// Phase 8.4 — a `fun`-lambda inside a nested module. A *simple* arg
/// (`fun x -> x`) claims no `_argN`, so this does not yet exercise FCS's
/// per-`moduleDefn` `SynArgNameGenerator.Reset()` (the non-simple-arg
/// body-lowering it would interact with is a deferred phase-10 divergence);
/// it is a forward guard for when that lands, and a smoke test that a lambda
/// body parses inside the nested loop today.
#[test]
fn diff_ast_nested_module_fun_body() {
    assert_asts_match("module Foo =\n    let f = fun x -> x\n");
}

// ---- Phase 8.5: module abbreviations (`module X = LongId`) --------------

/// Phase 8.5 — `module Foo = Bar.Baz` is a `SynModuleDecl.ModuleAbbrev`
/// (`ident="Foo"`, `longId=["Bar";"Baz"]`), decided by FCS's
/// `namedModuleDefnBlock` because the body is a single bare long-ident
/// (`pars.fsy:1427`; verified against `fcs-dump ast`). Distinct from a nested
/// module — the `module X =` path forks on body shape.
#[test]
fn diff_ast_module_abbrev_dotted() {
    assert_asts_match("module Foo = Bar.Baz\n");
}

/// Phase 8.5 — a single-segment RHS (`module M = N`) is still an abbreviation
/// (`longId=["N"]`), distinct from a nested module with one expr decl.
#[test]
fn diff_ast_module_abbrev_single_segment() {
    assert_asts_match("module M = N\n");
}

/// Phase 8.5 — an abbreviation interleaved with another decl. The `module A =
/// B.C` abbreviation and the following `let` are two sibling decls of the
/// implicit `AnonModule`.
#[test]
fn diff_ast_module_abbrev_then_let() {
    assert_asts_match("module A = B.C\nlet x = 1\n");
}

/// Phase 8.5 — a `global`-rooted RHS. FCS's `path` accepts `global` as the
/// head segment; the `SynLongIdent` stores it via `IdentTrivia.OriginalNotation`,
/// which the normaliser unwraps to `"global"` on both sides.
#[test]
fn diff_ast_module_abbrev_global_rhs() {
    assert_asts_match("module M = global.System\n");
}

/// Phase 8.5 — a dotted LHS (`module X.Y = Z`) is **not** a valid abbreviation:
/// FCS emits "A module abbreviation must be a simple name, not a path" and
/// **no** decl. We mirror that — the decl is an `ERROR` node (not cast as a
/// `ModuleDecl`), so both sides project an empty `AnonModule`.
#[test]
fn diff_ast_module_abbrev_dotted_lhs_is_error() {
    assert_asts_match_allow_errors("module X.Y = Z\n");
}

/// Phase 8.4 — a dotted *nested* module head (`module A.B =`⏎`  let x = 1`) is
/// likewise rejected: FCS requires a simple nested-module name and drops the
/// decl to an empty `AnonModule`. Distinct from the abbreviation case above (the
/// body here is a real block, not a bare long-ident).
#[test]
fn diff_ast_dotted_nested_module_is_error() {
    assert_asts_match_allow_errors("module A.B =\n  let x = 1\n");
}

/// Phase 8.5 — `rec` is rejected on an abbreviation ("Invalid use of 'rec'
/// keyword"); FCS emits no decl, and neither do we.
#[test]
fn diff_ast_module_abbrev_rec_is_error() {
    assert_asts_match_allow_errors("module rec Foo = Bar\n");
}

/// Phase 8.5 — an accessibility modifier is rejected on an abbreviation; FCS
/// emits no decl, and neither do we.
#[test]
fn diff_ast_module_abbrev_access_is_error() {
    assert_asts_match_allow_errors("module internal Foo = Bar\n");
}

/// Phase 10.5 — a bare attribute carried on a same-line `let` binding. FCS
/// folds the attribute into the binding (`SynBinding.attributes`), with no
/// standalone `SynModuleDecl.Attributes`. The bare attribute's `ArgExpr` is
/// FCS's synthetic `mkSynUnit`, i.e. `Const(Unit)`, and its `Target` is `None`.
#[test]
fn diff_ast_attribute_let_binding() {
    assert_asts_match("[<Foo>] let x = 1\n");
}

/// Phase 10.5 — the canonical `[<EntryPoint>]` form, attribute on its own line
/// before a function-form binding. The newline path routes the `let` through
/// LexFilter's `BlockSep` + `OLET` relabel, which must still attach to the
/// binding rather than producing a standalone attributes decl.
#[test]
fn diff_ast_attribute_entrypoint_newline() {
    assert_asts_match("[<EntryPoint>]\nlet main argv = 0\n");
}

/// Phase 10.5 — a dotted attribute path on a binding. The `TypeName`
/// `SynLongIdent` carries all three segments.
#[test]
fn diff_ast_attribute_dotted_on_let() {
    assert_asts_match("[<Foo.Bar.Baz>] let x = 1\n");
}

/// Phase 10.5 — two adjacent attribute lists on one binding group into a
/// two-element `SynAttributes` on the binding (FCS's `attributeList attributes`
/// recursion), distinct from one `[<A; B>]` list (a later slice).
#[test]
fn diff_ast_attribute_two_lists_on_let() {
    assert_asts_match("[<A>] [<B>] let x = 1\n");
}

/// Phase 10.5a — two attributes in *one* `[<A; B>]` list (FCS's
/// `attributeListElements: attribute | attributeListElements seps attribute`).
/// Both attributes land in a single `SynAttributeList`, distinct from the
/// `[<A>] [<B>]` two-list form above (which yields two lists). Verified against
/// `fcs-dump ast`: one `SynAttributeList`, two attributes, no parse errors.
#[test]
fn diff_ast_attribute_two_attrs_one_list() {
    assert_asts_match("[<A; B>] let x = 1\n");
}

/// Phase 10.5a — three `;`-separated attributes in one list, pinning that the
/// `attributeListElements` recursion accumulates an arbitrary number of attrs.
#[test]
fn diff_ast_attribute_three_attrs_one_list() {
    assert_asts_match("[<A; B; C>] let x = 1\n");
}

/// Phase 10.5a — a *trailing* separator before `>]` (`[<A; B;>]`). FCS's
/// `attributeList: LBRACK_LESS attributeListElements opt_seps GREATER_RBRACK`
/// admits the optional trailing `seps`; verified to parse cleanly with two
/// attributes in one list.
#[test]
fn diff_ast_attribute_trailing_separator() {
    assert_asts_match("[<A; B;>] let x = 1\n");
}

/// Phase 10.5a — a `;`-separated list with the second attribute *indented* on
/// the next line. An indented continuation emits no offside `Virtual::BlockSep`,
/// so the `;` is the sole separator and the newline is pure trivia; FCS folds
/// both into one `SynAttributeList`.
#[test]
fn diff_ast_attribute_multiline_indented() {
    assert_asts_match("[<A;\n  B>]\nlet x = 1\n");
}

/// Phase 10.5a — a list whose second attribute *aligns* at column 0 on the next
/// line (`[<A;\nB>]`). Here the lexfilter emits an offside `Virtual::BlockSep`
/// after the `;`, so the separator group is `; OBLOCKSEP`; FCS still accepts it
/// as one `SynAttributeList` with two attributes (verified `ParseHadErrors:
/// false`). Pins that the separator loop consumes the trailing `BlockSep`.
#[test]
fn diff_ast_attribute_multiline_aligned_semicolon() {
    assert_asts_match("[<A;\nB>]\nlet x = 1\n");
}

/// Phase 10.5a — an aligned multiline list with *no* `;`: the offside
/// `Virtual::BlockSep` alone separates the attributes (`[<A\nB>]`, FCS's
/// `seps: OBLOCKSEP`). FCS accepts it as one list of two attributes (verified).
/// Pins that a lone `BlockSep` is consumed as a separator group.
#[test]
fn diff_ast_attribute_multiline_aligned_no_semicolon() {
    assert_asts_match("[<A\nB>]\nlet x = 1\n");
}

/// Phase 10.5a — the `OBLOCKSEP ;` separator pair: the continuation aligns at
/// column 0 and *leads* with the `;` (`[<A\n; B>]`), so the lexfilter emits the
/// `BlockSep` before the `;`. FCS accepts this `seps` form too (verified).
#[test]
fn diff_ast_attribute_multiline_aligned_leading_semicolon() {
    assert_asts_match("[<A\n; B>]\nlet x = 1\n");
}

/// Phase 10.5 — a `global`-qualified attribute path. FCS's `path` production
/// accepts `global` only as the head segment; the `SynLongIdent` stores it via
/// `IdentTrivia.OriginalNotation`, which the normaliser prefers so both sides
/// project `"global"`. Mirrors the expr long-ident parser's `global` handling.
#[test]
fn diff_ast_attribute_global_qualified_path() {
    assert_asts_match("[<global.System.Obsolete>] let x = 1\n");
}

/// Phase 10.5b — an attribute argument expression. FCS's `attribute: path
/// opt_HIGH_PRECEDENCE_APP opt_atomicExprAfterType` carries the optional
/// `atomicExprAfterType` as `SynAttribute.ArgExpr`. `[<Foo(1, 2)>]` →
/// `ArgExpr = Paren(Tuple [Const 1; Const 2])` (verified against `fcs-dump`),
/// replacing the bare attribute's synthetic `mkSynUnit`.
#[test]
fn diff_ast_attribute_arg_paren_tuple() {
    assert_asts_match("[<Foo(1, 2)>] let x = 1\n");
}

/// Phase 10.5b — single-value parenthesised argument (`[<Foo(1)>]` →
/// `ArgExpr = Paren(Const 1)`).
#[test]
fn diff_ast_attribute_arg_paren_single() {
    assert_asts_match("[<Foo(1)>] let x = 1\n");
}

/// Phase 10.5b — a *spaced* argument (`[<Foo (1)>]`). FCS's
/// `opt_HIGH_PRECEDENCE_APP` makes the leading-paren adjacency optional, so the
/// spaced form has the same `Paren(Const 1)` `ArgExpr` as the unspaced one
/// (verified). We do not model the high-precedence marker; the optional atomic
/// arg is parsed directly.
#[test]
fn diff_ast_attribute_arg_paren_spaced() {
    assert_asts_match("[<Foo (1)>] let x = 1\n");
}

/// Phase 10.5b — a string-literal argument (`[<Foo "hi">]` →
/// `ArgExpr = Const(String "hi")`), the `atomicExprAfterType` constant form
/// (not a parenExpr).
#[test]
fn diff_ast_attribute_arg_string() {
    assert_asts_match("[<Foo \"hi\">] let x = 1\n");
}

/// Phase 10.5b — a bool-keyword argument (`[<Foo true>]` →
/// `ArgExpr = Const(Bool true)`), pinning the `TRUE` `atomicExprAfterType` arm.
#[test]
fn diff_ast_attribute_arg_bool() {
    assert_asts_match("[<Foo true>] let x = 1\n");
}

/// A *source-identifier* argument (`[<Foo __LINE__>]` →
/// `ArgExpr = Const(SourceIdentifier "__LINE__")`). The keyword-string reaches
/// `atomicExprAfterType` via `constant` → `sourceIdentifier`, so it is a valid
/// attribute argument — unlike a bare ident (`[<Foo Bar>]`, an FCS parse error).
/// The green-tree shape is also pinned by `attribute_arg_source_identifier` in
/// the unit tests.
#[test]
fn diff_ast_attribute_source_identifier_arg() {
    assert_asts_match("[<Foo __LINE__>] let x = 1\n");
}

/// Phase 10.5b — an argument composes with the 10.5a `;`-separated list:
/// `[<A(1); B>]` is one `SynAttributeList` whose first attribute has
/// `ArgExpr = Paren(Const 1)` and second is the bare `B`.
#[test]
fn diff_ast_attribute_arg_then_separated() {
    assert_asts_match("[<A(1); B>] let x = 1\n");
}

/// Phase 10.5b — the no-argument form is unchanged: a bare `[<Literal>]` still
/// projects FCS's synthetic `mkSynUnit` (`ArgExpr = Const Unit`). Guards that
/// adding the optional arg didn't perturb the bare path.
#[test]
fn diff_ast_attribute_no_arg_unit() {
    assert_asts_match("[<Literal>] let x = 1\n");
}

/// Phase 10.5c — an attribute *target* via the generic `ident COLON` arm
/// (`[<assembly: Foo>]`). FCS's `attributeTarget` populates `SynAttribute.Target`
/// with the `assembly` ident; the path is `Foo` (verified `Target = "assembly"`).
#[test]
fn diff_ast_attribute_target_assembly() {
    assert_asts_match("[<assembly: Foo>] let x = 1\n");
}

/// Phase 10.5c — a generic non-keyword target ident (`[<field: Foo>]` →
/// `Target = "field"`), exercising the same `ident COLON` arm with a different
/// well-known target word.
#[test]
fn diff_ast_attribute_target_field() {
    assert_asts_match("[<field: Foo>] let x = 1\n");
}

/// Phase 10.5c — the `type:` keyword target (`typeKeyword COLON` →
/// `Target = "type"`). `type` lexes as `Token::Type`, not an ident, so the
/// target detection accepts it; FCS's canonical idText is the keyword text.
#[test]
fn diff_ast_attribute_target_type() {
    assert_asts_match("[<type: Foo>] let x = 1\n");
}

/// Phase 10.5c — the `return:` target (FCS's `YIELD COLON → "return"`). `return`
/// lexes as `Token::Return` here; the target text is the keyword text "return".
#[test]
fn diff_ast_attribute_target_return() {
    assert_asts_match("[<return: Foo>] let x = 1\n");
}

/// Phase 10.5c — a target composes with the 10.5b argument:
/// `[<assembly: Foo(1)>]` is `Target = "assembly"`, path `Foo`, and
/// `ArgExpr = Paren(Const 1)`.
#[test]
fn diff_ast_attribute_target_with_arg() {
    assert_asts_match("[<assembly: Foo(1)>] let x = 1\n");
}

/// Phase 10.5c — a targeted attribute whose path is *indented on the next line*
/// (`[<assembly:\n    Foo>]`). FCS parses this via its `attributeTarget
/// OBLOCKBEGIN path oblockend` arm (verified `ParseHadErrors: false`); our
/// lexfilter emits *no* `BlockBegin` for the indented continuation inside
/// `[< … >]` — the newline is plain trivia — so the existing target + path path
/// handles it with the trivia simply attaching to the path's `LONG_IDENT`. (A
/// path aligned at column 0, `[<assembly:\nFoo>]`, *is* an FCS error and is not
/// claimed here.)
#[test]
fn diff_ast_attribute_target_offside_path() {
    assert_asts_match("[<assembly:\n    Foo>] let x = 1\n");
}

/// Phase 10.5c — the swallowed-`type` target with an indented path
/// (`[<type:\n    Foo>]`), exercising the `type:` raw-recovery together with the
/// offside-path layout. FCS accepts it (verified).
#[test]
fn diff_ast_attribute_target_type_offside_path() {
    assert_asts_match("[<type:\n    Foo>] let x = 1\n");
}

// ---- Phase 9.1: type abbreviations -------------------------------------
//
// `type T = <typ>` → `SynModuleDecl.Types [SynTypeDefn(SynComponentInfo
// (… longId=[T] …), SynTypeDefnRepr.Simple(SynTypeDefnSimpleRepr.TypeAbbrev
// (Ok, rhsType, _), _), [], None, _, { LeadingKeyword = Type; … })]`
// (`pars.fsy:2455`). The `type` keyword is *swallowed* by LexFilter (it pushes
// a transient `CtxtTypeDefns`, like `module`), so the parser recovers it from
// the raw stream as `TYPE_TOK`; the abbreviation RHS reuses the phase-7 full
// `parse_type` (FCS's `typ`, so `->`/`*`/postfix-app all extend the abbrev).

/// Phase 9.1 — the minimal abbreviation: a single atomic RHS.
#[test]
fn diff_ast_type_abbrev_int() {
    assert_asts_match("type T = int\n");
}

/// Phase 9.1 — a function-type RHS. The abbrev RHS is FCS's full `typ`, so the
/// top-level `->` extends the abbreviation: `TypeAbbrev(Fun(int, int))`.
#[test]
fn diff_ast_type_abbrev_fun() {
    assert_asts_match("type T = int -> int\n");
}

/// Phase 9.1 — a tuple-type RHS: `TypeAbbrev(Tuple[int, string])`.
#[test]
fn diff_ast_type_abbrev_tuple() {
    assert_asts_match("type T = int * string\n");
}

/// Phase 9.1 — a postfix-application RHS: `TypeAbbrev(App(list, [int]))`.
#[test]
fn diff_ast_type_abbrev_app() {
    assert_asts_match("type T = int list\n");
}

/// Phase 9.1 — a dotted RHS path: `TypeAbbrev(LongIdent [System; Int32])`.
#[test]
fn diff_ast_type_abbrev_dotted() {
    assert_asts_match("type T = System.Int32\n");
}

/// Phase 9.1 — an abbreviation followed by an unrelated `let`. The
/// `isTypeContinuator` offside-pop fires at the `let` (aligned with `type`),
/// so the type closes and the binding is a sibling decl.
#[test]
fn diff_ast_type_abbrev_then_let() {
    assert_asts_match("type T = int\nlet x = 1\n");
}

/// Phase 9.1 — two consecutive `type` declarations are **two** separate
/// `SynModuleDecl.Types` nodes (only `and` aggregates into one node, which is
/// phase 9.2). Pins the per-`type` carrier boundary.
#[test]
fn diff_ast_type_abbrev_two_decls() {
    assert_asts_match("type T = int\ntype U = string\n");
}

// ---- Bodyless types (`SynTypeDefnSimpleRepr.None`) ---------------------
//
// A type definition with no `=` and no body — `type Foo`, `[<Measure>] type m`.
// FCS's `tyconDefn` reduces to its bare `typeNameInfo` alternative (or, when a
// primary constructor precedes the absent `=`, the `recover` alternative),
// building a `SynTypeDefnRepr.Simple(SynTypeDefnSimpleRepr.None)` with **no
// parse error**. The canonical use is units of measure; whether a bodyless type
// is actually legal (it requires the `Measure` attribute, or being an exception)
// is a type-checker concern, so the parser accepts every bodyless `type`.

/// The minimal bodyless type — `type Foo`. Both sides project a `None` repr,
/// empty members, no implicit ctor. Before this the parser flagged "expected
/// `=` in type definition".
#[test]
fn diff_ast_type_bodyless() {
    assert_asts_match("type Foo\n");
}

/// The motivating case — a unit-of-measure definition `[<Measure>] type m`. The
/// `Measure` attribute rides `SynComponentInfo.attributes` (phase 10.7a); the
/// repr is `None`.
#[test]
fn diff_ast_type_bodyless_measure() {
    assert_asts_match("[<Measure>] type m\n");
}

/// A bodyless type with postfix type parameters — `type Foo<'a>`. The typars
/// parse exactly as on a body-bearing definition; only the repr is `None`.
#[test]
fn diff_ast_type_bodyless_generic() {
    assert_asts_match("type Foo<'a>\n");
}

/// A bodyless type with a primary constructor but no `=` — `type C(x)`. FCS's
/// `recover` alternative routes the constructor to the *outer* members slot with
/// `implicitConstructor = None` (distinct from the `= <object-model>` path,
/// which duplicates it across both slots); the normaliser mirrors that, and
/// neither side reports an error.
#[test]
fn diff_ast_type_bodyless_ctor() {
    assert_asts_match("type C(x)\n");
}

/// An `and`-chain of bodyless types — `type a\nand b`. FCS keeps both in one
/// `SynModuleDecl.Types`, so the bodyless path must still feed the
/// `and`-continuation gate.
#[test]
fn diff_ast_type_bodyless_and_chain() {
    assert_asts_match("type a\nand b\n");
}

/// An `and`-chain of measure definitions — `[<Measure>] type m\nand n`. The
/// attribute is on the first definition; the chain continues across the absent
/// bodies.
#[test]
fn diff_ast_type_bodyless_measure_and_chain() {
    assert_asts_match("[<Measure>] type m\nand n\n");
}

/// A bodyless type followed by an unrelated `let` — `type Foo\nlet x = 1`. The
/// bodyless definition closes (no open body block), so the column-0 `let` is a
/// sibling decl, not swallowed into the type.
#[test]
fn diff_ast_type_bodyless_then_let() {
    assert_asts_match("type Foo\nlet x = 1\n");
}

/// Two consecutive bodyless `type` declarations are **two** `Types` nodes — the
/// bodyless path does not greedily aggregate a following fresh `type`.
#[test]
fn diff_ast_type_bodyless_two_decls() {
    assert_asts_match("type a\ntype b\n");
}

// ---- Type-header accessibility (`type internal Foo`) -------------------
//
// FCS's `tyconNameAndTyparDecls: opt_access path` (`pars.fsy:2543`) accepts a
// `private`/`internal`/`public` modifier *before* the type name; it lands in
// `SynComponentInfo.accessibility` (field 6), which the normaliser now projects
// on both sides ([`NormalisedTypeDefn::access`]). We consume it as an
// `ACCESS_TOK` child of the `TYPE_DEFN`, between the leading `TYPE_TOK`/`AND_TOK`
// and the name's `LONG_IDENT`; the projector reads that before-name token, so a
// misplaced modifier now shows up as a diff. This is a distinct slot from the
// *constructor* accessibility (`type C private (x)`, after the name) and the
// after-name `type C private = …` slot, which FCS discards (and so do we).

/// The minimal case — an `internal` abbreviation. Before this the access
/// keyword sat where the type name was expected and produced "expected
/// identifier after `type`".
#[test]
fn diff_ast_type_internal_abbrev() {
    assert_asts_match("type internal Foo = int\n");
}

/// As above for `private`.
#[test]
fn diff_ast_type_private_abbrev() {
    assert_asts_match("type private Baz = int\n");
}

/// As above for `public`, over a (bar-led) union repr — the modifier rides the
/// header regardless of which repr follows.
#[test]
fn diff_ast_type_public_union() {
    assert_asts_match("type public U = A | B\n");
}

/// Accessibility over a record repr — `type internal R = { X : int }`. Confirms
/// the modifier consumption is independent of the simple-repr dispatch.
#[test]
fn diff_ast_type_internal_record() {
    assert_asts_match("type internal R = { X : int }\n");
}

/// Accessibility composes with postfix type parameters — `type internal
/// Foo<'a> = 'a`. The modifier is consumed before the name, leaving the
/// adjacent `<'a>` typar parse untouched.
#[test]
fn diff_ast_type_internal_generic() {
    assert_asts_match("type internal Foo<'a> = 'a\n");
}

/// Accessibility on an `and`-chained continuation — `type T = int and internal
/// U = string`. Each `AND tyconDefn` re-enters the header, so the modifier is
/// admitted on continuations exactly as on the first definition.
#[test]
fn diff_ast_type_and_chain_internal_continuation() {
    assert_asts_match("type T = int\nand internal U = string\n");
}

// ---- After-name constructor accessibility, no parens (`type C private = …`)
//
// FCS's `tyconDefn` has a second `opt_access` *after* the name, before the
// (optional) primary-constructor args (`typeNameInfo opt_attributes opt_access
// opt_simplePatterns … EQUALS …`, `pars.fsy:1647`). With ctor args it is the
// constructor's accessibility (`type C private (x)`, phase 9.8a); with **no**
// args (`type C private = …`) FCS parses the modifier and then **discards** it
// entirely — there is no `ImplicitCtor`, and `SynComponentInfo.accessibility`
// stays `None`, so the AST is identical to `type C = …`. We mirror that:
// consume the modifier as a bare `ACCESS_TOK` sibling of the `TYPE_DEFN`
// (elided), leaving the repr shape untouched. This is a distinct slot from the
// type's own *before*-name access (`type internal C`), which DOES survive in
// `SynComponentInfo.accessibility`.

/// The minimal case — abbreviation repr. Before this the modifier sat where the
/// repr's `=` was expected and the type definition failed to parse.
#[test]
fn diff_ast_type_after_name_private_abbrev() {
    assert_asts_match("type C private = int\n");
}

/// As above for `internal`.
#[test]
fn diff_ast_type_after_name_internal_abbrev() {
    assert_asts_match("type C internal = int\n");
}

/// After-name access over a record repr — `type R private = { X : int }`.
#[test]
fn diff_ast_type_after_name_private_record() {
    assert_asts_match("type R private = { X : int }\n");
}

/// After-name access over a union repr — `type U private = A | B`.
#[test]
fn diff_ast_type_after_name_private_union() {
    assert_asts_match("type U private = A | B\n");
}

/// After-name access on an `and`-chained continuation — `type T = int and U
/// private = string`. The continuation re-enters the same header / ctor path,
/// so the discarded modifier is handled there too.
#[test]
fn diff_ast_type_after_name_private_and_chain() {
    assert_asts_match("type T = int\nand U private = string\n");
}

// ---- Phase 9.2: `and`-chained type definitions -------------------------
//
// `type T = … and U = …` is **one** `SynModuleDecl.Types` node holding several
// `SynTypeDefn` (leading keywords `Type` then `And`, `SyntaxTrivia.fsi:256`);
// only a fresh `type` keyword starts a new node. `and` is a real filtered
// token (LexFilter keeps `CtxtTypeDefns` open across an aligned `and` via
// `isTypeContinuator`), claimed as `AND_TOK`.

/// Phase 9.2 — the minimal chain: one `Types` node, two abbreviation defns.
#[test]
fn diff_ast_type_and_chain_two() {
    assert_asts_match("type T = int\nand U = string\n");
}

/// Phase 9.2 — a three-way chain stays one node with three defns.
#[test]
fn diff_ast_type_and_chain_three() {
    assert_asts_match("type T = int\nand U = string\nand V = bool\n");
}

// NB: a *single-line* chain (`type T = int and U = string`) is **invalid** F# —
// FCS rejects it ("Unexpected keyword 'and' in member definition") because the
// inline `and` stays inside the first body's still-open offside block. The
// parser mirrors the rejection (it does not splice a bogus chain); see the
// `inline_type_and_chain_is_rejected` unit test in `parser::tests::structure`.

/// Phase 9.2 — an `and`-chain followed by a *fresh* `type`: the chain is one
/// node (`[T; U]`), the fresh `type` starts a second node (`[V]`). Pins that
/// `and` aggregates but `type` separates.
#[test]
fn diff_ast_type_and_chain_then_fresh_type() {
    assert_asts_match("type T = int\nand U = string\ntype V = bool\n");
}

/// Phase 9.2 — an offside chain (each body indented under its own `=`). Here
/// every body opens its own SeqBlock, so the per-defn `OBLOCKEND` discipline is
/// exercised across the `and` boundary.
#[test]
fn diff_ast_type_and_chain_offside() {
    assert_asts_match("type T =\n    int\nand U =\n    string\n");
}

// ---- Phase 9.3: type parameters ----------------------------------------
//
// `SynComponentInfo.typeParams: SynTyparDecls option`. Postfix `type T<'a>`
// (`PostfixList`, via the phase-7.6 `HighPrecedenceTyApp` + `< … >` machinery)
// and single-prefix `type 'a T` (`SinglePrefix`); each typar is a `SynTyparDecl`
// wrapping a `SynTypar(ident, staticReq, _)`. The `preferPostfix` flag and the
// `SynTyparDecls` *variant* are elided — the flat typar list (name + head-type)
// is what's projected. (Parenthesised-prefix `('a, 'b) T` is deferred — its
// `)` is LexFilter-swallowed; see `prefix_list_typars_are_a_clean_error`.)

/// Phase 9.3 — a single postfix type parameter: `T<'a>`.
#[test]
fn diff_ast_type_param_postfix_single() {
    assert_asts_match("type T<'a> = 'a list\n");
}

/// Phase 9.3 — two postfix type parameters, comma-separated.
#[test]
fn diff_ast_type_param_postfix_two() {
    assert_asts_match("type T<'a, 'b> = 'a * 'b\n");
}

/// Phase 9.3 — a single-prefix type parameter: `'a T` (typar before the name).
#[test]
fn diff_ast_type_param_prefix_single() {
    assert_asts_match("type 'a T = 'a list\n");
}

/// Phase 9.3 — *spaced* postfix `T <'a>` (a space before `<`). LexFilter emits
/// no `HighPrecedenceTyApp` virtual here (only a bare `Less`); FCS still parses
/// the generic definition (with a "non-adjacent type parameters" warning, not
/// an error), so the `opt_HIGH_PRECEDENCE_TYAPP` path must accept it.
#[test]
fn diff_ast_type_param_postfix_spaced() {
    assert_asts_match("type T <'a> = 'a list\n");
}

/// Phase 9.3 — a head-type (statically-resolved) type parameter `^a`
/// (`TyparStaticReq.HeadType`), read from the `^` sigil.
#[test]
fn diff_ast_type_param_head_typar() {
    assert_asts_match("type T<^a> = int\n");
}

/// Phase 9.3 — generic definitions in an `and`-chain. Each `SynTypeDefn`
/// carries its own `typeParams`; offside so the chain is valid (9.2).
#[test]
fn diff_ast_type_param_and_chain() {
    assert_asts_match("type T<'a> = 'a list\nand U<'b> = 'b list\n");
}

/// Phase 9.3 — a multiline postfix list: a later typar is offside on the next
/// line, so LexFilter inserts a `BlockSep` after the comma. FCS accepts the
/// two-parameter list, so the comma loop must skip the layout separator.
#[test]
fn diff_ast_type_param_postfix_multiline() {
    assert_asts_match("type T<'a,\n       'b> = 'a * 'b\n");
}

/// Attributes on a type-parameter declaration: `type T<[<Measure>] 'a>`. FCS's
/// `SynTyparDecl(attributes, …)` carries a leading `attributes` run; the typar
/// list opener is now an attribute `[<` rather than the sigil.
#[test]
fn diff_ast_typar_decl_attribute() {
    assert_asts_match("type T<[<Measure>] 'a> = int\n");
}

/// Attributes on a *head-type* typar declaration: `type T<[<Measure>] ^a>` —
/// the attribute precedes the `^` sigil.
#[test]
fn diff_ast_typar_decl_attribute_head() {
    assert_asts_match("type T<[<Measure>] ^a> = int\n");
}

/// Two attributed typars, the second carrying a `;`-separated multi-attribute
/// list — the `FSharp.Core` `Map` header shape.
#[test]
fn diff_ast_typar_decl_two_attributed() {
    assert_asts_match(
        "type Map<[<EqualityConditionalOn>] 'Key, \
         [<EqualityConditionalOn; ComparisonConditionalOn>] 'Value> = int\n",
    );
}

/// An attributed typar that also carries an inside-`<>` `when` constraint —
/// the attribute run precedes the typar, the constraint follows the list.
#[test]
fn diff_ast_typar_decl_attribute_with_constraint() {
    assert_asts_match("type T<[<Measure>] 'a when 'a : comparison> = 'a list\n");
}

/// An attribute set offside from its typar (`[<Measure>]⏎ 'a`): LexFilter emits
/// a `BlockSep` between the attribute list and the sigil, which the pre-sigil
/// drain in `parse_typar_decl` must skip. FCS accepts this layout.
#[test]
fn diff_ast_typar_decl_attribute_offside() {
    assert_asts_match("type T<[<Measure>]\n       'a> = int\n");
}

// ---- Phase 9.3b: type-parameter constraints (`when 'a : …`) -------------
//
// The `when` clause (`opt_typeConstraints`, `pars.fsy:2615`) is an
// `and`-separated `SynTypeConstraint` list. It attaches in two grammar
// positions: *inside* the angle brackets (`postfixTyparDecls`,
// `pars.fsy:2578`) — where it lands in `SynTyparDecls.PostfixList`'s
// constraints — and *after* the typar decls at component-info level
// (`pars.fsy:1605`) — where it lands in `SynComponentInfo.constraints`. The
// normaliser merges both. Covered here: the constraints that need no fresh
// sub-parser. Deferred: `default 'a : t` (library-only error), `'a : enum<…>`
// / `'a : delegate<…>` (constraint type-args), `'a : (member …)` (needs member
// signatures, phase 10.12), and the self-constrained bare type.

/// Phase 9.3b — `'a : comparison` (`WhereTyparIsComparable`).
#[test]
fn diff_ast_typar_constraint_comparison() {
    assert_asts_match("type T<'a when 'a : comparison> = 'a list\n");
}

/// Phase 9.3b — `'a : equality` (`WhereTyparIsEquatable`).
#[test]
fn diff_ast_typar_constraint_equality() {
    assert_asts_match("type T<'a when 'a : equality> = 'a list\n");
}

/// Phase 9.3b — `'a : unmanaged` (`WhereTyparIsUnmanaged`).
#[test]
fn diff_ast_typar_constraint_unmanaged() {
    assert_asts_match("type T<'a when 'a : unmanaged> = 'a list\n");
}

/// Phase 9.3b — `'a : struct` (`WhereTyparIsValueType`), a keyword-token
/// constraint (not an ident).
#[test]
fn diff_ast_typar_constraint_struct() {
    assert_asts_match("type T<'a when 'a : struct> = 'a list\n");
}

/// Phase 9.3b — `'a : not struct` (`WhereTyparIsReferenceType`): the `not` is a
/// plain `IDENT` preceding the `struct` keyword.
#[test]
fn diff_ast_typar_constraint_not_struct() {
    assert_asts_match("type T<'a when 'a : not struct> = 'a list\n");
}

/// Phase 9.3b — `'a : null` (`WhereTyparSupportsNull`).
#[test]
fn diff_ast_typar_constraint_null() {
    assert_asts_match("type T<'a when 'a : null> = 'a list\n");
}

/// Phase 9.3b — `'a : not null` (`WhereTyparNotSupportsNull`).
#[test]
fn diff_ast_typar_constraint_not_null() {
    assert_asts_match("type T<'a when 'a : not null> = 'a list\n");
}

/// Phase 9.3b — `'a :> System.IDisposable` (`WhereTyparSubtypeOfType`): the
/// `:>` carries a constraint type (reuses `parse_type`).
#[test]
fn diff_ast_typar_constraint_subtype() {
    assert_asts_match("type T<'a when 'a :> System.IDisposable> = 'a list\n");
}

/// Phase 9.3b — an `and`-separated chain of two constraints.
#[test]
fn diff_ast_typar_constraint_and_chain() {
    assert_asts_match("type T<'a when 'a : comparison and 'a : equality> = 'a list\n");
}

/// Phase 9.3b — the *after-decls* position: `type T<'a> when 'a : comparison`
/// puts the clause in `SynComponentInfo.constraints` rather than the
/// `PostfixList`. The normaliser must read both sources.
#[test]
fn diff_ast_typar_constraint_after_decls() {
    assert_asts_match("type T<'a> when 'a : comparison = 'a list\n");
}

/// Phase 9.3b — a constraint keyword written as a *backticked* identifier
/// (`` ``comparison`` ``). FCS de-quotes it to `Ident.idText = "comparison"`, so
/// it is the same `WhereTyparIsComparable` as the bare form; the parser must
/// classify on the stripped text, not the raw lexeme.
#[test]
fn diff_ast_typar_constraint_backticked_ident() {
    assert_asts_match("type T<'a when 'a : ``comparison``> = 'a list\n");
}

/// Phase 9.3b — a backticked `` ``not`` `` before `null` is still the
/// `WhereTyparNotSupportsNull` constraint (the `not` is an `IDENT` either way).
#[test]
fn diff_ast_typar_constraint_backticked_not() {
    assert_asts_match("type T<'a when 'a : ``not`` null> = 'a list\n");
}

// ---- Top-level `;;` separator ------------------------------------------
//
// `;;` is a top-level decl separator (`topSeparator: SEMICOLON_SEMICOLON`,
// `pars.fsy:6967`), accepted between/after module definitions and producing no
// `SynModuleDecl` of its own. FCS emits no diagnostic for a post-decl `;;`; the
// parser lands it as an inert `SEMISEMI_TOK` the normaliser ignores. (A
// *leading* `;;` is a separate, FCS-rejected case — see the
// `leading_semisemi_still_errors` unit test in `parser::tests::bindings`.)

/// Two `let` decls separated by `;;` — two `SynModuleDecl.Let`, no error.
#[test]
fn diff_ast_semisemi_between_lets() {
    assert_asts_match("let x = a;;\nlet y = b\n");
}

/// `;;` on the same line as both decls (`let x = a;; let y = b`).
#[test]
fn diff_ast_semisemi_inline_between_lets() {
    assert_asts_match("let x = a;; let y = b\n");
}

/// A trailing `;;` after the only decl.
#[test]
fn diff_ast_semisemi_trailing() {
    assert_asts_match("let x = 1;;\n");
}

/// A run of separators (`;;;;`) between two decls.
#[test]
fn diff_ast_semisemi_run() {
    assert_asts_match("let x = 1;;;;\nlet y = 2\n");
}

/// `;;` after an `open` decl — no `BlockEnd` precedes the `;;`, so this pins
/// that the separator clears the loop's pending-separator state.
#[test]
fn diff_ast_semisemi_after_open() {
    assert_asts_match("open System;; let y = b\n");
}

/// `;;` after a `type` decl (`SynModuleDecl.Types` then `Let`).
#[test]
fn diff_ast_semisemi_after_type() {
    assert_asts_match("type T = int;; let y = b\n");
}

/// `;;` between two top-level expression decls (`SynModuleDecl.Expr`).
#[test]
fn diff_ast_semisemi_between_exprs() {
    assert_asts_match("printfn \"a\";; printfn \"b\"\n");
}

/// `;;` inside a nested module body (the decl loop runs in `BodyScope::Nested`
/// too) — the separator must not disturb the body-closing `OBLOCKEND` handling.
#[test]
fn diff_ast_semisemi_in_nested_module() {
    assert_asts_match("module M =\n    let x = 1;;\n    let y = 2\n");
}

/// A trailing `;;` as the last token of a nested module body.
#[test]
fn diff_ast_semisemi_trailing_in_nested_module() {
    assert_asts_match("module M =\n    let x = 1;;\n");
}

// A *leading* `;;` (before any decl) is rejected by both parsers, but the two
// *recoveries* diverge: FCS discards the whole module body (`decls: []`),
// whereas we keep the recovered trailing decl (more useful for an LSP). That
// is a deliberate error-recovery difference on malformed input, not an AST the
// two are meant to agree on, so there is no `assert_asts_match*` here; the
// `leading_semisemi_still_errors` unit test (`parser::tests::bindings`) pins
// our behaviour — an error plus the recovered `let`.

// ---- Top-level single `;` separator ------------------------------------
//
// A single `;` is also a top-level decl separator (`topSeparator: SEMICOLON`,
// `pars.fsy:6967`) — the same role as `;;` and the offside `OBLOCKSEP`. FCS
// emits no diagnostic for a post-decl `;`; like `;;` it produces no
// `SynModuleDecl` and lands as an inert `SEMI_TOK` the normaliser ignores.
//
// The clean cases are the *non-offside-block* decls (`open`, a bare expression),
// a `let` whose `;` is trailing or newline-separated, and an *inline* `;` before
// a fresh `let` binding (`open X; let y = 1` → `Open` + `Let`). The post-`;`
// `let` arrives as a *raw* `Token::Let` (a single `;`, unlike `;;`, does not
// short-circuit the offside rule — FCS's `isSemiSemi`, lexfilter
// `LexFilter.fs:1806` — so it is not rewritten to the offside `Virtual::Let`);
// `parse_let_decl` accepts the raw keyword, so the module decl loop routes it to
// a `LET_DECL` just like the offside form.
//
// Several cases stay out of scope:
//   * a `let` *before* the inline `;` (`let x = a; let y = b`, `let a = 1; open …`)
//     — the binding's `typedSequentialExpr` RHS swallows the `;` (it never
//     reaches the loop as a top separator), so the trailing content lands in
//     *expression* position. FCS treats `let x = a; let y = b` as one
//     `let`-in-sequential decl, and rejects `let a = 1; open …` (`open` is not an
//     expression). Only a *non-`let`* decl before the `;` (`open`/`type`/an
//     expression) leaves the `;` as a real top separator;
//   * `open X; let y = 1 in y` — the `let … in …` is a `SynModuleDecl.Expr`, but
//     our parser does not yet model a module-level `let`-in expression (it errors
//     on the offside `let x = 1 in x` too); routing it to `parse_let_decl` is no
//     worse — both sides error — so no *clean* case regresses;
//   * `open X; use y = r` — FCS accepts it but emits a warning ("`use` bindings
//     are not permitted in modules"), so it is not error-free on the FCS side;
//   * `type T = int; <inline decl>` — the `;` stays *inside* the type's offside
//     block (only `;;` closes it), so FCS rejects it. The single-`;` separator is
//     gated on the offside-block `depth == 0`, so we likewise error rather than
//     swallowing the in-body `;` (pinned by `parser::tests::bindings::
//     semi_inside_type_block_errors`); `type T = int;; open System` *does* close
//     the block and stays clean (`diff_ast_semisemi_*`).

/// Two `let` decls separated by a single `;` (newline after the `;`, so the
/// second `let` is offside `Virtual::Let`).
#[test]
fn diff_ast_semi_between_lets() {
    assert_asts_match("let x = a;\nlet y = b\n");
}

/// A trailing single `;` after the only decl.
#[test]
fn diff_ast_semi_trailing() {
    assert_asts_match("let x = 1;\n");
}

/// `;` after an `open` decl, before an expression decl (`open` leaves no
/// `BlockEnd`, so this pins the separator clearing the loop's pending-separator
/// state).
#[test]
fn diff_ast_semi_after_open() {
    assert_asts_match("open System; printfn \"x\"\n");
}

/// `open X; open Y` — two `SynModuleDecl.Open` separated by a single `;`.
#[test]
fn diff_ast_semi_between_opens() {
    assert_asts_match("open System; open System.IO\n");
}

/// A run of three `open` decls separated by single `;`s.
#[test]
fn diff_ast_semi_run_of_opens() {
    assert_asts_match("open A; open B; open C\n");
}

/// Two top-level expression decls separated by `;` (`a; b` → two
/// `SynModuleDecl.Expr`, not one `Sequential`).
#[test]
fn diff_ast_semi_between_exprs() {
    assert_asts_match("printfn \"a\"; printfn \"b\"\n");
}

/// A single `;` inside a nested module body.
#[test]
fn diff_ast_semi_in_nested_module() {
    assert_asts_match("module M =\n    let x = 1;\n    let y = 2\n");
}

/// `open X; let y = 1` — an *inline* `;` before a fresh `let` binding. The
/// post-`;` `let` is a raw `Token::Let` (not the offside `Virtual::Let`); the
/// loop routes it to a `LET_DECL`, giving `Open` + `Let` as FCS does.
#[test]
fn diff_ast_semi_then_let_decl() {
    assert_asts_match("open System.IO; let y = 1\n");
}

/// `printfn "a"; let y = 1` — an inline `;` after an expression decl, before a
/// fresh `let` binding (`Expr` + `Let`).
#[test]
fn diff_ast_semi_expr_then_let_decl() {
    assert_asts_match("printfn \"a\"; let y = 1\n");
}

/// A `;` separator inside a whole-file `module` header's body. The body's
/// opening `OBLOCKBEGIN` is consumed *inside* the decl loop (not by a caller),
/// so the single-`;` depth baseline must skip it for the body's top level to
/// read as depth 0 — otherwise the `;` is wrongly rejected.
#[test]
fn diff_ast_semi_in_module_header_body() {
    assert_asts_match("module M\nopen A; open B\n");
}

/// The same inside a `namespace` body.
#[test]
fn diff_ast_semi_in_namespace_body() {
    assert_asts_match("namespace N\nopen A; open B\n");
}

/// A module abbreviation followed by `;;` and a sibling `open`
/// (`module M = N;; open System` → `ModuleAbbrev` + `Open`). The `;;` closes the
/// abbreviation body's block (unlike a single `;`), so the `open` is a sibling
/// at the outer level — matching FCS. (The single-`;` form errors loudly; see
/// `parser::tests::bindings::module_abbrev_trailing_semi_errors_loudly`.)
#[test]
fn diff_ast_module_abbrev_semisemi_then_open() {
    assert_asts_match("module M = N;; open System\n");
}

/// A `;` separator after a *type abbreviation* whose block has already closed on
/// the preceding newline (`type T = int`⏎`    ; let x = 1` → `Types` + `Let`).
/// The `;` is at depth 0 (unlike an inline `type T = int; …`, where it stays
/// inside the type body), so it is a clean top separator and the following `let`
/// is a module binding — matching FCS.
#[test]
fn diff_ast_semi_after_closed_type_then_let() {
    assert_asts_match("type T = int\n    ; let x = 1\n");
}

// ---- Phase 9.4: record types -------------------------------------------
//
// `type T = { F : T1; mutable G : T2 }` → `SynTypeDefnSimpleRepr.Record`
// (`SyntaxTree.fsi:1382`), a `SynField` list. The `{` is a real `LBrace`; the
// `}` is LexFilter-swallowed (recovered like the CE `}` in 10.2). Fields are
// `[mutable] ident : <typ>` separated by `;`/`OBLOCKSEP` (the anon-record-type
// `seps` machinery, phase 7.9). Field accessibility / `isStatic` / attributes
// are elided; the field type reuses the full `parse_type`.

/// Phase 9.4 — the minimal record: a single field.
#[test]
fn diff_ast_record_single_field() {
    assert_asts_match("type T = { X : int }\n");
}

/// Phase 9.4 — multiple `;`-separated fields, including a `mutable` field
/// (`SynField.isMutable`).
#[test]
fn diff_ast_record_mutable_field() {
    assert_asts_match("type T = { X : int; mutable Y : string }\n");
}

/// Phase 9.4 — a field whose type is itself a postfix application
/// (`int list`), confirming the field type is the full `parse_type`.
#[test]
fn diff_ast_record_app_field_type() {
    assert_asts_match("type T = { Xs : int list }\n");
}

/// Phase 9.4 — offside record: fields on their own lines, separated by the
/// `OBLOCKSEP` virtual rather than `;`.
#[test]
fn diff_ast_record_offside_fields() {
    assert_asts_match("type T =\n    { X : int\n      Y : string }\n");
}

/// Phase 9.4 — a generic record (`type T<'a> = { Value : 'a }`), exercising the
/// 9.3 type parameters together with a record repr whose field references the
/// typar.
#[test]
fn diff_ast_record_generic() {
    assert_asts_match("type T<'a> = { Value : 'a }\n");
}

/// Phase 9.4 — a record with a repr-level access modifier
/// (`type T = private { … }`, FCS's `opt_access braceFieldDeclList`). The
/// access token sits before the `{`; it is consumed as an `ACCESS_TOK` child of
/// the `RECORD_REPR` and projected as the repr's accessibility
/// (`SynTypeDefnSimpleRepr.Record` field 0), so the diff verifies it.
#[test]
fn diff_ast_record_private() {
    assert_asts_match("type T = private { X : int }\n");
}

/// Phase 9.4 — a *field-level* access modifier (`{ private X : int }`) is
/// illegal; FCS reports the error but discards it (`SynField.accessibility`
/// stays `None`). Our tree keeps the recovery `ACCESS_TOK`, so the field
/// projection must *not* surface it — otherwise this allow-errors diff would
/// falsely diverge. Regression guard for that discard.
#[test]
fn diff_ast_record_field_private_recovers() {
    assert_asts_match_allow_errors("type R = { private X : int }\n");
}

/// Phase 9.4 — the closing `}` on its own line. LexFilter emits an
/// `OBLOCKSEP` before the swallowed `}`; the separator run must not drain past
/// the raw `}` (which `bump_swallowed_closer` then recovers).
#[test]
fn diff_ast_record_close_on_own_line() {
    assert_asts_match("type T =\n    { X : int\n    }\n");
}

/// Phase 9.4 — `}`-on-own-line record type *in an `and` chain*. LexFilter emits
/// an `OBLOCKSEP` before the swallowed `}` and then the type body's `BlockEnd`
/// that admits the `and U = …` continuation. The close-line `OBLOCKSEP` must be
/// consumed zero-width (not drained, not left in the filtered stream) so the
/// `BlockEnd` is still observed and the chain parses — a regression guard for
/// the separator-group rework.
#[test]
fn diff_ast_record_close_on_own_line_and_chain() {
    assert_asts_match("type T =\n    { X : int\n    }\nand U = { Y : int }\n");
}

// ---- Phase 9.5: discriminated unions -----------------------------------
//
// `type T = A | B of int * x:string` → `SynTypeDefnSimpleRepr.Union`
// (`SyntaxTree.fsi:1376`), a `SynUnionCase` list. Cases are `Bar`-separated
// (optional leading `|`); each is `Name [of T1 * x:T2 * …]` where the `of`
// fields are `*`-separated `SynField`s (anonymous or `name : T`), parsed at the
// tuple-segment level (`parse_app_type_can_be_nullable`, so `*` separates
// *fields*, not a tuple). Disambiguated from an abbreviation by a leading `|`
// or a case name followed by `of`/`|` — but **not** `Ident | null`, which stays
// a `WithNull` abbreviation (see `diff_ast_type_def_withnull_not_union`).

/// Phase 9.5 — two nullary cases.
#[test]
fn diff_ast_union_two_nullary() {
    assert_asts_match("type T = A | B\n");
}

/// Phase 9.5 — three cases (the `Bar` loop runs more than once).
#[test]
fn diff_ast_union_three_cases() {
    assert_asts_match("type T = A | B | C\n");
}

/// Phase 9.5 — offside leading-bar cases on their own lines.
#[test]
fn diff_ast_union_offside_leading_bars() {
    assert_asts_match("type T =\n    | A\n    | B\n");
}

/// Phase 9.5 — a single case with one field (`A of int`); no bar needed.
#[test]
fn diff_ast_union_single_case_of() {
    assert_asts_match("type T = A of int\n");
}

/// Phase 9.5 — anonymous `*`-separated fields (`A of int * string` → two
/// fields, not a tuple).
#[test]
fn diff_ast_union_anon_fields() {
    assert_asts_match("type T = A of int * string\n");
}

/// Phase 9.5 — named fields (`B of x:int * y:string` → `SynField.idOpt` set).
#[test]
fn diff_ast_union_named_fields() {
    assert_asts_match("type T = B of x:int * y:string\n");
}

/// Phase 9.5 — a mix of a nullary case and a case with a field.
#[test]
fn diff_ast_union_mixed() {
    assert_asts_match("type T = A | B of int\n");
}

/// Phase 9.5 — a generic union whose case field references the type parameter.
#[test]
fn diff_ast_union_generic() {
    assert_asts_match("type T<'a> = Nil | Cons of 'a\n");
}

/// Phase 9.5 regression — `type T = int | null` is a `WithNull` **abbreviation**
/// (7.11), not a union: the union detection must exclude `Ident | null`.
#[test]
fn diff_ast_type_def_withnull_not_union() {
    assert_asts_match("type T = int | null\n");
}

/// Phase 9.5 — a *parenthesised* nullable union-case field. FCS's union-case
/// field is `appTypeNullableInParens`: `T | null` is only nullable when
/// parenthesised, so `A of (string | null)` is a single field whose type is
/// `WithNull(string)`. (The bare `A of string | null` is an FCS error — see the
/// `union_unparenthesized_nullable_field_errors` unit test.)
#[test]
fn diff_ast_union_paren_nullable_field() {
    assert_asts_match("type T = A of (string | null)\n");
}

// ---- Phase 9.6: enums --------------------------------------------------
//
// `type T = A = 0 | B = 1` → `SynTypeDefnSimpleRepr.Enum` (`SyntaxTree.fsi:1379`),
// a `SynEnumCase` list. Enum cases (`Name = <value>`) share the union grammar
// and `Bar` separators; the Union-vs-Enum repr is decided post-hoc — any case
// with a `= value` makes it an `Enum` (FCS errors on a mixed group via
// `parsAllEnumFieldsRequireValues`). The value is an `atomicExpr` (a `SynExpr`,
// reusing the expression projector).

/// Phase 9.6 — the canonical enum.
#[test]
fn diff_ast_enum_basic() {
    assert_asts_match("type T = A = 0 | B = 1\n");
}

/// Phase 9.6 — leading-bar, offside cases.
#[test]
fn diff_ast_enum_offside_leading_bars() {
    assert_asts_match("type E =\n    | A = 0\n    | B = 1\n");
}

/// Phase 9.6 — a dotted long-identifier value (`atomicExpr`'s `atomicExpr DOT
/// atomicExprQualification`). `parse_ident_expr` walks the dot chain, so this
/// matches FCS's `LongIdent` value exactly (no negative-literal-folding gap).
#[test]
fn diff_ast_enum_dotted_ident_value() {
    assert_asts_match("type E = A = System.Int32.MaxValue\n");
}

// NB: a *high-precedence paren application* value (`f(1)`) is **not**
// diff-tested here. `atomicExpr` is self-recursive on
// `HIGH_PRECEDENCE_PAREN_APP` (`pars.fsy:5247`); FCS accepts `f(1)` (marking
// the `App` `ExprAtomicFlag.Atomic`) and so does the parser, producing the
// matching `App` shape — but the CST normaliser hardcodes `is_atomic: false`
// for every `App` (atomic-flag read-back from the `HIGH_PRECEDENCE_PAREN_APP`
// marker is a pre-existing deferral), so the projected value disagrees only on
// that flag. The parser behaviour is pinned by the
// `enum_high_precedence_paren_app_value_parses` unit test.

/// Phase 9.6 — a *negative* enum value (`B = -1`). The adjacent sign-fold pass
/// (`sign_fold`) merges `-1` into one signed literal token, exactly as FCS folds
/// at the token layer, so both sides project a negative `Const` and the diff
/// lines up.
#[test]
fn diff_ast_enum_negative_value() {
    assert_asts_match("type T = A = 0 | B = -1\n");
}

/// Phase 9.6 sad path — a mixed group (`A = 0 | B`, an enum case then a
/// value-less case) is an FCS error (`parsAllEnumFieldsRequireValues`); FCS
/// still emits an `Enum` repr with only the valued case. The parser mirrors the
/// shape (the value-less case projects to no enum case), so the `allow_errors`
/// diff lines up.
#[test]
fn diff_ast_enum_mixed_is_error() {
    assert_asts_match_allow_errors("type T = A = 0 | B\n");
}

/// An enum case value that is a high-precedence application `f(1)` projects to
/// `App(Atomic, …)` in FCS (flag `0`). `parse_enum_case_value` has its own HPA
/// loop (separate from `parse_app_expr`); it stamps the same atomic-application
/// marker, so the value's `is_atomic` diffs against the oracle. Guards against
/// the marker being modelled in only one of the two `APP_EXPR` build sites.
#[test]
fn diff_ast_enum_atomic_app_value() {
    assert_asts_match("type E = A = f(1)\n");
}

// ---- Delegates ---------------------------------------------------------
//
// `type T = delegate of int -> int` → `pars.fsy:1779`'s `DELEGATE OF topType`,
// which FCS lowers to `SynTypeDefnRepr.ObjectModel(SynTypeDefnKind.Delegate(ty,
// arity), [AbstractSlot "Invoke"], _)`. Both the `arity` (`SynValInfo`) and the
// synthetic `Invoke` slot are derived from the same signature `ty`, so we keep
// the surface `DELEGATE_REPR > [DELEGATE_TOK, OF_TOK, <type>]` shape and the
// normaliser compares only that signature type
// (`NormalisedTypeRepr::Delegate`). The `<type>` is parsed by the shared
// `parse_type`, so every type form the abbreviation arm supports composes here.

/// The canonical delegate — a single function arrow.
#[test]
fn diff_ast_delegate_basic() {
    assert_asts_match("type Blah = delegate of int -> int\n");
}

/// Tupled arguments (`int * int -> int`): the signature `ty` is
/// `Fun(Tuple[int; int], int)` (the arity's tupling is reflected in the type).
#[test]
fn diff_ast_delegate_tupled_args() {
    assert_asts_match("type Blah = delegate of int * int -> int\n");
}

/// Curried arguments (`int -> int -> int`): right-nested `Fun(int, Fun(int,
/// int))`, exactly as the abbreviation arrow nests.
#[test]
fn diff_ast_delegate_curried_args() {
    assert_asts_match("type Blah = delegate of int -> int -> int\n");
}

/// A `unit -> unit` delegate — the no-arg / no-result shape.
#[test]
fn diff_ast_delegate_unit() {
    assert_asts_match("type Blah = delegate of unit -> unit\n");
}

/// A parenthesised function argument (`(int -> int) -> int`) — the inner arrow
/// is a `Paren(Fun …)` argument, distinct from the curried form above.
#[test]
fn diff_ast_delegate_paren_arg() {
    assert_asts_match("type Blah = delegate of (int -> int) -> int\n");
}

/// A generic delegate over its own type parameters (`type T<'a> = delegate of
/// 'a -> 'a`): composes the 9.3 typar header with the delegate body.
#[test]
fn diff_ast_delegate_generic() {
    assert_asts_match("type Func<'a> = delegate of 'a -> 'a\n");
}

/// A generic type-application in the signature (`delegate of int -> int list`).
#[test]
fn diff_ast_delegate_app_type_result() {
    assert_asts_match("type Blah = delegate of int -> int list\n");
}

/// Offside layout — the `delegate of …` body on its own indented line.
#[test]
fn diff_ast_delegate_offside() {
    assert_asts_match("type Blah =\n    delegate of int -> int\n");
}

/// A delegate as one link of an `and`-chained type group — pins that the
/// delegate body offsides and closes its block so the `and` continuation
/// splices correctly.
#[test]
fn diff_ast_delegate_and_chain() {
    assert_asts_match("type A = delegate of int -> int\nand B = int\n");
}

// ---- Phase 9.7: object-model scaffolding + instance `member` methods ----

/// Phase 9.7 — the canonical instance member. `type T =\n  member this.M = 1`
/// projects to `SynModuleDecl.Types [SynTypeDefn]`, repr
/// `ObjectModel(Unspecified, members=[Member], _)`. The member is a `SynBinding`
/// whose `headPat` is `SynPat.LongIdent([this; M], …)`, `valData.memberFlags`
/// is `Some(IsInstance=true, MemberKind=Member)`, and trivia
/// `LeadingKeyword = Member` (the member flags are elided; the leading keyword
/// and the repr's member slot carry the distinction from a `let`).
#[test]
fn diff_ast_member_single() {
    assert_asts_match("type T =\n  member this.M = 1\n");
}

/// Phase 9.7 — two instance members. Drives the member-block continuation
/// terminator (`OBLOCKEND·ODECLEND·OBLOCKSEP` between members, a lone
/// `OBLOCKEND` for the last member's RHS, then the body-closing `OBLOCKEND`).
#[test]
fn diff_ast_member_two() {
    assert_asts_match("type T =\n  member this.M = 1\n  member this.N = 2\n");
}

/// Phase 9.7 — a member with curried arguments. `member this.Add a b = a + b`
/// has `headPat = SynPat.LongIdent([this; Add], args=Pats[Named a; Named b])`;
/// the args sweep mirrors the function-form `let` head.
#[test]
fn diff_ast_member_with_args() {
    assert_asts_match("type T =\n  member this.Add a b = a + b\n");
}

/// A member with an optional argument — `member this.M(?x) = x`, the canonical
/// site for `SynPat.OptionalVal`. The member head is `SynPat.LongIdent([this;
/// M], args=Pats[Paren(OptionalVal "x")])`, exercising the optional-value
/// pattern through the member-argument parsing path (distinct from the
/// function-form `let` head). Here the optional argument is also *semantically*
/// valid, but parsing is identical to the `let` sites.
#[test]
fn diff_ast_member_optional_arg() {
    assert_asts_match("type T =\n  member this.M(?x) = x\n");
}

/// A *generic* member — explicit value-typar decls on the head
/// (`member this.M<'a>(x: 'a) = x`). FCS's `memberCore` carries
/// `opt_explicitValTyparDecls` between the name and the args, stored on
/// `SynPat.LongIdent.typarDecls` (the same `<'a>` a `let f<'a>` head carries).
#[test]
fn diff_ast_member_generic() {
    assert_asts_match("type T() =\n  member this.M<'a>(x: 'a) = x\n");
}

/// A generic `static member` with two typars (`static member F<'a, 'b>(x) = x`).
#[test]
fn diff_ast_static_member_generic_two_typars() {
    assert_asts_match("type T() =\n  static member F<'a, 'b>(x) = x\n");
}

/// A generic member with *no* curried args — `member this.TypeFunc<'a> =
/// typeof<'a>.Name`. The typars force the `SynPat.LongIdent` form even with zero
/// args (mirroring `let h<'a> = …`).
#[test]
fn diff_ast_member_generic_no_args() {
    assert_asts_match("type T() =\n  member this.TypeFunc<'a> = typeof<'a>.Name\n");
}

/// A generic member whose typar carries a `when` constraint —
/// `static member Key<'a when 'a :> System.IComparable>() = ()`. The constraint
/// clause rides inside the typar-decl list.
#[test]
fn diff_ast_member_generic_constrained_typar() {
    assert_asts_match("type T() =\n  static member Key<'a when 'a :> System.IComparable>() = ()\n");
}

/// Return-type annotation on a member head — `member this.M : int = 1`.
/// FCS's `memberCore` uses the same `opt_topReturnTypeWithTypeConstraints`
/// production as a `let`, so this projects identically to the let case:
/// `headPat = LongIdent([this; M])` stays bare, and the RHS is wrapped in
/// `SynExpr.Typed(Const 1, int)` (the `BINDING_RETURN_INFO` node reconstructs
/// the wrapper via the shared `normalise_binding`).
#[test]
fn diff_ast_member_return_type() {
    assert_asts_match("type T =\n  member this.M : int = 1\n");
}

/// Return type on a member with curried args — `member this.Add a b : int =
/// a + b`. The curried-arg sweep stops at the `:` (not an atomic-pat start),
/// so the type attaches as the member's return info, not to the last param.
#[test]
fn diff_ast_member_return_type_with_args() {
    assert_asts_match("type T =\n  member this.Add a b : int = a + b\n");
}

/// Return type on a `static member` head — `static member M : int = 1`.
/// Pins that the static flag and return-info compose.
#[test]
fn diff_ast_static_member_return_type() {
    assert_asts_match("type T =\n  static member M : int = 1\n");
}

/// A non-trivial member return type — `member this.M : int -> int = id`.
/// Confirms the full `parse_type` surface reaches the member return-info site.
#[test]
fn diff_ast_member_return_type_function_type() {
    assert_asts_match("type T =\n  member this.M : int -> int = id\n");
}

/// A *named* parameter in a member return-type annotation — `member _.M : x: int
/// -> int = …`. The return-info production is `opt_topReturnTypeWithType‑
/// Constraints` (`pars.fsy:6039`), a `topType` context, so the labelled argument
/// is a `SignatureParameter` exactly as in a `.fsi` member sig.
#[test]
fn diff_ast_member_return_type_named_param() {
    assert_asts_match("type T() =\n  member _.M : x: int -> int = fun y -> y\n");
}

/// Phase 9.7 — a member whose body is an offside multi-statement sequence
/// (`SynExpr.Sequential`). The member's RHS reuses the shared seq-block
/// expression parser, so the body's inter-statement `OBLOCKSEP` is consumed by
/// the RHS parse, and only the member's RHS-close `OBLOCKEND` reaches the member
/// loop. (An expression-position `let … in` body needs the not-yet-implemented
/// expression-level block-`let`, a later slice, so it is not exercised here.)
#[test]
fn diff_ast_member_seq_body() {
    assert_asts_match("type T =\n  member this.M =\n    ignore 1\n    2\n");
}

/// Phase 9.7 — a member-bearing type definition followed by a sibling
/// top-level `let`. Confirms the body-closing `OBLOCKEND` is handed back to the
/// enclosing `parse_module_decls` loop so the `let` parses as its own decl.
#[test]
fn diff_ast_member_then_let() {
    assert_asts_match("type T =\n  member this.M = 1\nlet y = 2\n");
}

/// Phase 9.7 — a member whose body is a paren application
/// (`member this.M = id (1)`). Confirms the member RHS admits an arbitrary
/// expression, not just an atom.
#[test]
fn diff_ast_member_app_body() {
    assert_asts_match("type T =\n  member this.M = id 1\n");
}

/// Phase 9.7 — the wildcard self-identifier `member _.M = 1`. FCS stores the
/// `_` as the first segment of the head `SynPat.LongIdent` (`idText = "_"`), so
/// the head is `LongIdent([_; M])`. The member-head path parser accepts the
/// leading `_` (`allow_underscore_head`).
#[test]
fn diff_ast_member_wildcard_self() {
    assert_asts_match("type T =\n  member _.M = 1\n");
}

/// Phase 9.7 — wildcard self-id with curried args (`member _.Add a b = a + b`).
#[test]
fn diff_ast_member_wildcard_with_args() {
    assert_asts_match("type T =\n  member _.Add a b = a + b\n");
}

/// An `inline` member — FCS's `memberCore` is `opt_inline bindingPattern …`
/// (`pars.fsy:1901`), so `member inline this.M = 1` sets `SynBinding.isInline`
/// while the head pattern stays `LongIdent([this; M])`. The `inline` token sits
/// inside the member `BINDING` (where `Binding::is_inline` reads it), so the
/// normalised binding's `is_inline` flag matches FCS.
#[test]
fn diff_ast_member_inline() {
    assert_asts_match("type T =\n  member inline this.M = 1\n");
}

/// An `inline` member with a wildcard self-id and a curried unit arg — the
/// reported `member inline _.Delay () = ()` (a `TaskBuilder`-style builder
/// method). The `inline` precedes the wildcard head; `Delay ()` sweeps the unit
/// arg pattern as usual.
#[test]
fn diff_ast_member_inline_wildcard_with_unit_arg() {
    assert_asts_match("type T =\n  member inline _.Delay () = ()\n");
}

/// `static member inline` — the `inline` modifier composes with the `static`
/// flag (`staticMemberOrMemberOrOverride memberCore`), still landing inside the
/// binding ahead of the head pattern.
#[test]
fn diff_ast_static_member_inline() {
    assert_asts_match("type T =\n  static member inline M x = x\n");
}

/// `inline` on a member with a return-type annotation — the modifier, the head,
/// and the `: int` return info all compose (`memberCore`'s
/// `opt_inline bindingPattern opt_topReturnTypeWithTypeConstraints`).
#[test]
fn diff_ast_member_inline_return_type() {
    assert_asts_match("type T =\n  member inline this.M : int = 1\n");
}

/// Phase 9.7 — two members on a single line
/// (`type T = member this.M = 1 member this.N = 2`). FCS accepts this; the first
/// member's RHS-close `OBLOCKEND` is trailed *directly* by the next `member`
/// (no `ODECLEND`, since the decl never went offside), so the member-block loop
/// must treat a trailing raw `member` as a continuation too.
#[test]
fn diff_ast_member_two_same_line() {
    assert_asts_match("type T = member this.M = 1 member this.N = 2\n");
}

/// Phase 9.7 — same-line members separated by `;`
/// (`member this.M = 1; member this.N = 2`). The `;` is consumed inside the
/// first member's RHS seq-block, leaving the same `OBLOCKEND·member` shape as
/// the space-separated form.
#[test]
fn diff_ast_member_two_semicolon() {
    assert_asts_match("type T =\n  member this.M = 1; member this.N = 2\n");
}

/// An *adjacent* parenthesised member argument (`member this.M(x) = x`). The
/// shared HPA-aware curried-args sweep skips the `HighPrecedenceParenApp`
/// virtual before the `(`, so the arg parses to `Pats[Paren(Named "x")]` — the
/// FCS shape (matching `member this.M (x)`).
#[test]
fn diff_ast_member_paren_arg() {
    assert_asts_match("type T =\n  member this.M(x) = x\n");
}

/// An adjacent *unit* member argument (`member this.M() = 1`) →
/// `Pats[Paren(Const Unit)]`.
#[test]
fn diff_ast_member_unit_arg() {
    assert_asts_match("type T =\n  member this.M() = 1\n");
}

/// Phase 9.7 — a member head with no self qualifier (`member M = 1`). FCS's
/// *parser* accepts this without error (verified via `fcs-dump`:
/// `ParseHadErrors = false`), producing a single-segment `SynPat.LongIdent([M])`
/// member — the "instance members need a self-identifier" rule is a later
/// *type-check* diagnostic, not a parse error. We mirror FCS's `ParsedInput`
/// (design D2), so the head is a 1-segment `LongIdent` here too; rejecting it at
/// parse time would diverge. This regression guard pins the FCS-faithful shape.
#[test]
fn diff_ast_member_no_self_qualifier() {
    assert_asts_match("type T =\n  member M = 1\n");
}

// ---- Phase 9.8a: implicit primary constructor --------------------------

/// Phase 9.8a — the empty primary constructor `type T() = member _.X = 1`.
/// FCS gives repr `ObjectModel(Unspecified, members=[ImplicitCtor; Member])`
/// *and* `SynTypeDefn.implicitConstructor = Some(ImplicitCtor)` (the ctor in
/// both slots), with the empty `()` a bare `SynPat.Const(Unit)` (no `Paren`).
#[test]
fn diff_ast_implicit_ctor_empty() {
    assert_asts_match("type T() =\n  member _.X = 1\n");
}

/// Phase 9.8a — a typed constructor argument `type T(x: int) = …`. The args are
/// a `SynPat.Paren(SynPat.Typed(SynPat.Named "x", int))` (FCS 43.x unifies ctor
/// args into `SynPat`), reusing the pattern projector.
#[test]
fn diff_ast_implicit_ctor_typed_arg() {
    assert_asts_match("type T(x: int) =\n  member _.X = x\n");
}

/// Phase 9.8a — a single untyped argument `type T(x) = …` →
/// `Paren(Named "x")`.
#[test]
fn diff_ast_implicit_ctor_single_arg() {
    assert_asts_match("type T(x) =\n  member _.X = x\n");
}

/// Phase 9.8a — multiple constructor arguments `type T(x, y) = …` →
/// `Paren(Tuple[Named "x", Named "y"])`.
#[test]
fn diff_ast_implicit_ctor_multi_arg() {
    assert_asts_match("type T(x, y) =\n  member _.X = x\n");
}

/// Phase 9.8a — the `as self` self-identifier `type T(x) as self = …` →
/// `ImplicitCtor.selfIdentifier = Some "self"`.
#[test]
fn diff_ast_implicit_ctor_as_self() {
    assert_asts_match("type T(x) as self =\n  member _.X = x\n");
}

/// Phase 9.8a — a *spaced* constructor `type T () = …` (no
/// `HighPrecedenceParenApp` virtual, FCS accepts it) still parses the ctor.
#[test]
fn diff_ast_implicit_ctor_spaced() {
    assert_asts_match("type T () =\n  member _.X = 1\n");
}

/// Phase 9.8a — a constructor on a generic type `type T<'a>(x: 'a) = …`.
#[test]
fn diff_ast_implicit_ctor_generic() {
    assert_asts_match("type T<'a>(x: 'a) =\n  member _.X = x\n");
}

/// Phase 9.8a — the constructor follows an *after-decls* `when` constraint
/// (`type T<'a> when 'a : equality (x: 'a) = …`). FCS's `typeNameInfo` ends with
/// the `when` clause, so the ctor is parsed *after* the constraint (the reverse
/// order `(x) when …` is an FCS parse error); the parser mirrors this.
#[test]
fn diff_ast_implicit_ctor_after_when_constraint() {
    assert_asts_match("type T<'a> when 'a : equality (x: 'a) =\n  member _.X = x\n");
}

/// Phase 9.8a — an accessibility modifier before the constructor
/// (`type C private (x: int) = …`, FCS's `opt_access` before
/// `opt_simplePatterns`). The modifier is consumed as `ACCESS_TOK` and elided,
/// so the projected shape matches FCS.
#[test]
fn diff_ast_implicit_ctor_access() {
    assert_asts_match("type C private (x: int) =\n  member _.X = x\n");
}

/// Phase 9.8a — a constructor on a *non-class* repr (`type R(x: int) = { X:
/// int }`) is an FCS error ("Only class types may take value arguments"), yet
/// FCS still fills the `implicitConstructor` slot and the `Record` repr. The
/// parser mirrors both the diagnostic and the shape, so the `allow_errors` diff
/// lines up.
#[test]
fn diff_ast_implicit_ctor_on_record_is_error() {
    assert_asts_match_allow_errors("type R(x: int) = { X: int }\n");
}

/// Phase 9.8a — likewise a constructor on a type abbreviation
/// (`type A(x: int) = int`).
#[test]
fn diff_ast_implicit_ctor_on_abbrev_is_error() {
    assert_asts_match_allow_errors("type A(x: int) = int\n");
}

// ---- Phase 10.7j (primary-ctor attributes): ImplicitCtor.attributes ----
//
// A leading `[<…>]` between the type name and the constructor parens
// (`type T [<A>] ()`) attaches to `SynMemberDefn.ImplicitCtor.attributes` (FCS
// field 1, in `typeNameInfo opt_attributes opt_access opt_simplePatterns`). FCS
// stores the ctor in *both* `SynTypeDefn.implicitConstructor` and the object-model
// members, so the attribute appears on both. Shapes ground-truthed with `fcs-dump`.

/// Phase 10.7j — the canonical empty attributed ctor `type T [<A>] ()`.
#[test]
fn diff_ast_ctor_attr_empty() {
    assert_asts_match("type T [<A>] () = class end\n");
}

/// Phase 10.7j — an attributed ctor with a typed argument.
#[test]
fn diff_ast_ctor_attr_arg() {
    assert_asts_match("type T [<A>] (x: int) = class end\n");
}

/// Phase 10.7j — a dotted attribute path on the ctor.
#[test]
fn diff_ast_ctor_attr_dotted_path() {
    assert_asts_match("type T [<System.Obsolete>] () = class end\n");
}

/// Phase 10.7j — accessibility after the attribute (`[<A>] private ()`, FCS's
/// `opt_attributes opt_access`).
#[test]
fn diff_ast_ctor_attr_then_access() {
    assert_asts_match("type T [<A>] private () = class end\n");
}

/// Phase 10.7j — an attributed ctor on a generic type (`type T<'a> [<A>] ()`).
#[test]
fn diff_ast_ctor_attr_generic() {
    assert_asts_match("type T<'a> [<A>] () = class end\n");
}

/// Phase 10.7j — a multi-attribute list (`[<A; B>]`) on the ctor.
#[test]
fn diff_ast_ctor_attr_multi() {
    assert_asts_match("type T [<A; B>] () = class end\n");
}

/// Phase 10.7j — a ctor attribute with an argument (composing 10.5b).
#[test]
fn diff_ast_ctor_attr_with_arg() {
    assert_asts_match("type T [<CompiledName(\"X\")>] () = class end\n");
}

/// Phase 10.7j — an attributed ctor with an `as self` self-identifier.
#[test]
fn diff_ast_ctor_attr_as_self() {
    assert_asts_match("type T [<A>] (x: int) as self = class end\n");
}

/// Phase 10.7j — the offside `type T [<A>]⏎  ()` layout (the ctor parens on the
/// next line, with an `OBLOCKSEP` between the attribute and the `(`).
#[test]
fn diff_ast_ctor_attr_offside() {
    assert_asts_match("type T [<A>]\n  () = class end\n");
}

/// Phase 10.7j — an attributed ctor whose body is a member block (the common
/// real form: the ctor + its attribute, then `member …`).
#[test]
fn diff_ast_ctor_attr_with_members() {
    assert_asts_match("type T [<A>] (x: int) =\n  member this.M = x\n");
}

// ---- Phase 9.8b: class-local `let`/`let rec` ---------------------------

/// Phase 9.8b — a class-local `let` (`type T() =`⏎`  let y = 1`⏎`  member …`).
/// FCS gives repr `ObjectModel(Unspecified, members=[ImplicitCtor; LetBindings;
/// Member])`; the `LetBindings` carries one `Normal` binding (leading keyword
/// `Let`).
#[test]
fn diff_ast_class_local_let() {
    assert_asts_match("type T() =\n  let y = 1\n  member _.Y = y\n");
}

/// Phase 9.8b — a class-local `let rec` → `LetBindings(isRecursive = true)`, the
/// binding's leading keyword `LetRec`.
#[test]
fn diff_ast_class_local_let_rec() {
    assert_asts_match("type T() =\n  let rec f x = f x\n  member _.M = 1\n");
}

/// Phase 9.8b — two separate class-local `let`s before a member → two
/// `LetBindings` members then the `Member`.
#[test]
fn diff_ast_class_local_multiple_lets() {
    assert_asts_match("type T() =\n  let a = 1\n  let b = 2\n  member _.S = a + b\n");
}

/// Phase 9.8b — a class-local `let` as the *last* item (no trailing member).
#[test]
fn diff_ast_class_local_let_only() {
    assert_asts_match("type T() =\n  let a = 1\n  let b = 2\n");
}

/// Phase 9.8b — a class-local `let … and …` group (FCS's `localBindings` —
/// `LetBindings` with two bindings, leading `LetRec` then `And`).
#[test]
fn diff_ast_class_local_let_and_chain() {
    assert_asts_match("type T() =\n  let rec a = 1\n  and b = 2\n  member _.S = a + b\n");
}

// ---- Phase 9.8c: static class-local `let`/`let rec` --------------------

/// Phase 9.8c — a `static let` (FCS's `STATIC classDefnBindings`,
/// `pars.fsy:2009`). FCS gives the *same* `SynMemberDefn.LetBindings` as a
/// class-local `let`, but `mkClassMemberLocalBindings` rewrites the head
/// binding's leading keyword `Let` → `StaticLet` (the `isStatic` field is
/// elided by the oracle, so the static distinction rides on the leading
/// keyword).
#[test]
fn diff_ast_static_class_local_let() {
    assert_asts_match("type T() =\n  static let x = 1\n  member _.X = x\n");
}

/// Phase 9.8c — a `static let rec` → head leading keyword `StaticLetRec`.
#[test]
fn diff_ast_static_class_local_let_rec() {
    assert_asts_match("type T() =\n  static let rec f x = f x\n  member _.M = 1\n");
}

/// Phase 9.8c — a `static let rec … and …` group: head `StaticLetRec`, the
/// `and`-chained continuation stays `And` (FCS rewrites only the first binding).
#[test]
fn diff_ast_static_class_local_let_and_chain() {
    assert_asts_match("type T() =\n  static let rec a = 1\n  and b = 2\n  member _.S = a + b\n");
}

/// Phase 9.8c — a `static let` as the *last* item (no trailing member), driving
/// the body-close terminator off a static binding.
#[test]
fn diff_ast_static_class_local_let_only() {
    assert_asts_match("type T() =\n  static let a = 1\n  static let b = 2\n");
}

/// Phase 9.8c — a `static let` mixed with a plain class-local `let` and a member
/// (drives the member-block continuation across a static binding).
#[test]
fn diff_ast_static_and_instance_class_local_let() {
    assert_asts_match("type T() =\n  static let a = 1\n  let b = 2\n  member _.S = a + b\n");
}

// ---- Phase 9.9a: static members ----------------------------------------

/// Phase 9.9a — a static member (`type T =`⏎`  static member M = 1`). FCS gives
/// `SynMemberDefn.Member` with `memberFlags.IsInstance = false`, a
/// single-segment head `LongIdent(["M"])` (no self-id), and leading keyword
/// `StaticMember` (the member-flags are elided, so the leading keyword carries
/// the static distinction).
#[test]
fn diff_ast_static_member() {
    assert_asts_match("type T =\n  static member M = 1\n");
}

/// Phase 9.9a — a static member with curried arguments.
#[test]
fn diff_ast_static_member_with_args() {
    assert_asts_match("type T =\n  static member Add a b = a + b\n");
}

/// Phase 9.9a — a static and an instance member in the same body (drives the
/// member-block continuation across a `static member`).
#[test]
fn diff_ast_static_and_instance_member() {
    assert_asts_match("type T =\n  static member M = 1\n  member this.N = 2\n");
}

/// Phase 9.9a — two static members.
#[test]
fn diff_ast_two_static_members() {
    assert_asts_match("type T =\n  static member M = 1\n  static member N = 2\n");
}

// ---- Phase 9.10a: override / default members ---------------------------
//
// `override this.M = …` / `default this.M = …` are *not* a new AST node: each
// is `SynMemberDefn.Member(SynBinding …)` — the same `MEMBER_DEFN` as a plain
// `member` (9.7) — differing only in the binding's `SynLeadingKeyword`
// (`Override` / `Default`) and an elided `SynMemberFlags.IsOverrideOrExplicitImpl`.

/// Phase 9.10a — a property-style override (`override this.M = 1`). FCS gives
/// `SynMemberDefn.Member` with head `LongIdent(["this","M"], Pats[])` and leading
/// keyword `Override`; the member-flags are elided.
#[test]
fn diff_ast_override_member() {
    assert_asts_match("type T =\n  override this.M = 1\n");
}

/// Phase 9.10a — an override with a unit argument (`override this.M() = 1`),
/// head `LongIdent(["this","M"], Pats[Paren(Unit)])`.
#[test]
fn diff_ast_override_member_unit_arg() {
    assert_asts_match("type T =\n  override this.M() = 1\n");
}

/// Phase 9.10a — an override with curried arguments.
#[test]
fn diff_ast_override_member_with_args() {
    assert_asts_match("type T =\n  override this.Add a b = a + b\n");
}

/// Phase 9.10a — `default this.M = 1`, identical shape with leading keyword
/// `Default`.
#[test]
fn diff_ast_default_member() {
    assert_asts_match("type T =\n  default this.M = 1\n");
}

/// Phase 9.10a — an `override` alongside a plain `member` (drives the
/// member-block continuation across an `override`).
#[test]
fn diff_ast_override_and_member() {
    assert_asts_match("type T =\n  member this.M = 1\n  override this.N = 2\n");
}

/// Phase 9.10a — an override on a generic type (composes with the type header).
#[test]
fn diff_ast_override_member_generic() {
    assert_asts_match("type T<'a> =\n  override this.M = 1\n");
}

/// Phase 9.10a — `override val P = … with get, set` is the *auto-property*
/// production (`memberFlags autoPropsDefnDecl`, `pars.fsy:2099`), not a member
/// method: FCS gives `SynMemberDefn.AutoProperty` (leading keyword `OverrideVal`,
/// elided by the normaliser). The `override`/`default` `val` lookahead must route
/// to the auto-property parser, exactly like `member val`.
#[test]
fn diff_ast_override_val_auto_property() {
    assert_asts_match("type T() =\n  override val P = 0 with get, set\n");
}

/// Phase 9.10a — `default val P = …` likewise → `AutoProperty` (`DefaultVal`).
#[test]
fn diff_ast_default_val_auto_property() {
    assert_asts_match("type T() =\n  default val P = 0 with get, set\n");
}

/// Phase 9.10a — `override val P = 0` with no get/set clause (`propKind=Member`).
#[test]
fn diff_ast_override_val_no_getset() {
    assert_asts_match("type T() =\n  override val P = 0\n");
}

// ---- Phase 9.10b: explicit `new(...)` constructors -----------------------
//
// `new(a) = …` is the `classDefnMember` NEW arm (`pars.fsy:2106`):
// `SynMemberDefn.Member(SynBinding …)` whose head is `LongIdent(["new"],
// args=Pats[atomicPattern])`, leading keyword `New`, `MemberKind=Constructor`.
// Reuses the `MEMBER_DEFN` node; the head is the `new` keyword, the args the
// general atomic-pattern parser (so `()` is `Paren(Const Unit)`). `as self` goes
// to the elided `valData.thisIdOpt`.

/// Phase 9.10b — the canonical explicit constructor (`new(a) = { x = a }`).
#[test]
fn diff_ast_new_ctor() {
    assert_asts_match("type T =\n  val x : int\n  new(a) = { x = a }\n");
}

/// Phase 9.10b — a no-arg constructor (`new() = …`); the `()` is `Paren(Const
/// Unit)` (unlike the implicit ctor's bare `Const Unit`).
#[test]
fn diff_ast_new_ctor_unit() {
    assert_asts_match("type T =\n  val x : int\n  new() = { x = 0 }\n");
}

/// Phase 9.10b — a multi-arg constructor (`new(a, b) = …`), args
/// `Paren(Tuple[a, b])`.
#[test]
fn diff_ast_new_ctor_tuple_args() {
    assert_asts_match("type T =\n  val x : int\n  new(a, b) = { x = a }\n");
}

/// Phase 9.10b — `new(a) as self = …`; the `as self` is elided (FCS stores it in
/// `valData.thisIdOpt`).
#[test]
fn diff_ast_new_ctor_as_self() {
    assert_asts_match("type T =\n  val x : int\n  new(a) as self = { x = a }\n");
}

/// Phase 9.10b — an explicit constructor alongside a regular member.
#[test]
fn diff_ast_new_ctor_and_member() {
    assert_asts_match("type T =\n  val x : int\n  new(a) = { x = a }\n  member this.X = this.x\n");
}

/// Phase 9.10b — an *unparenthesised* single-argument constructor (`new a =
/// …`). FCS's `new atomicPattern`: the bare `a` is an `atomicPattern`, so the
/// arg is `SynPat.Named "a"` (not the paren form). Unlike the parenthesised
/// forms, LexFilter opens *no* offside RHS block after the `=` here, so the
/// member terminator must not assume a RHS-close `OBLOCKEND`.
#[test]
fn diff_ast_new_ctor_unparenthesised_arg() {
    assert_asts_match("type T =\n  val x : int\n  new a = { x = a }\n");
}

/// Phase 9.10b — the unparenthesised constructor *followed by a dedented
/// top-level declaration*. This is the shape that appears in FCS's own
/// benchmark sources (`Misc.fs`, `FSharpCoreFunctions.fs`): the block the
/// no-RHS-block ctor leaves must not swallow the following `type`.
#[test]
fn diff_ast_new_ctor_unparenthesised_arg_then_type() {
    assert_asts_match(
        "type T =\n  val x : int\n  new a = { x = a }\n\ntype U() =\n  member _.X = 1\n",
    );
}

/// Phase 9.10b — the unparenthesised constructor with an *offside* RHS on the
/// next line, followed by a dedented declaration. Here LexFilter *does* open a
/// RHS block (the body is offside), so the terminator must consume its
/// RHS-close `OBLOCKEND` — the mirror of the same-line case, exercising the
/// dynamic "did the RHS open a block" flag in both directions.
#[test]
fn diff_ast_new_ctor_unparenthesised_arg_offside_rhs_then_type() {
    assert_asts_match(
        "type T =\n  val x : int\n  new a =\n    { x = a }\n\ntype U() =\n  member _.X = 1\n",
    );
}

/// Phase 9.10b — an access-modified constructor (`private new(a) = …`). FCS's
/// `opt_access` before `NEW` (`pars.fsy:2106`); the access lands in the head
/// pattern's accessibility and is elided.
#[test]
fn diff_ast_new_ctor_private() {
    assert_asts_match("type T =\n  val x : int\n  private new(a) = { x = a }\n");
}

/// Phase 9.10b — `internal new(...)` (the other access modifier on a ctor).
#[test]
fn diff_ast_new_ctor_internal() {
    assert_asts_match("type T =\n  val x : int\n  internal new() = { x = 0 }\n");
}

// ---- Phase 9.10c: abstract slots -----------------------------------------
//
// `abstract M : int -> int` is the only genuinely new member node:
// `SynMemberDefn.AbstractSlot(slotSig: SynValSig, flags, …)` — a named value
// *signature* with no `= expr` body (`abstractMemberFlags`, `pars.fsy:1973`).
// We model it as `ABSTRACT_SLOT > [ABSTRACT_TOK, MEMBER_TOK?, VAL_SIG]`; the
// `VAL_SIG` (ident `:` type) is shared with phase 10.12. The leading keyword
// `Abstract` / `AbstractMember` is pinned; arity / flags / typars are elided.
// No `= expr` RHS → the no-RHS-block terminator (like `val` fields).

/// Phase 9.10c — the canonical abstract method (`abstract M : int -> int`),
/// leading keyword `Abstract`, `synType = Fun(int, int)`.
#[test]
fn diff_ast_abstract_method() {
    assert_asts_match("type T =\n  abstract M : int -> int\n");
}

/// Phase 9.10c — `abstract member M : …` (the `member`-keyword variant), leading
/// keyword `AbstractMember`; otherwise identical.
#[test]
fn diff_ast_abstract_member_method() {
    assert_asts_match("type T =\n  abstract member M : int -> int\n");
}

/// Phase 9.10c — a property-shaped abstract slot (`abstract P : int`, no arrow).
#[test]
fn diff_ast_abstract_property() {
    assert_asts_match("type T =\n  abstract P : int\n");
}

/// Phase 9.10c — a curried abstract method signature.
#[test]
fn diff_ast_abstract_curried() {
    assert_asts_match("type T =\n  abstract M : int -> int -> int\n");
}

/// Phase 9.10c — two abstract slots (drives the member-block continuation across
/// a no-RHS abstract slot).
#[test]
fn diff_ast_two_abstract_slots() {
    assert_asts_match("type T =\n  abstract M : int\n  abstract N : int\n");
}

/// Phase 9.10c — an abstract slot followed by a concrete member.
#[test]
fn diff_ast_abstract_then_member() {
    assert_asts_match("type T =\n  abstract M : int\n  member this.N = 1\n");
}

/// Phase 9.10c — an `inline` abstract slot (`abstract inline X : …`, FCS's
/// `opt_inline` before the name; `SynValSig.isInline = true`, elided).
#[test]
fn diff_ast_abstract_inline() {
    assert_asts_match("type T =\n  abstract inline X : int -> int\n");
}

/// Phase 9.10c — `abstract member inline X : …` (the `member` + `inline` combo).
#[test]
fn diff_ast_abstract_member_inline() {
    assert_asts_match("type T =\n  abstract member inline X : int -> int\n");
}

// ---- static abstract slots (F# 7 IWSAM) ------------------------------------
//
// A `static abstract [member] M : …` slot is FCS's `abstractMemberFlags` with a
// leading `static` (`SynMemberFlags { IsInstance = false }`) — the interface
// static-abstract member of a static-member-constrained interface. The leading
// keyword projects to `StaticAbstract` / `StaticAbstractMember`. Only the
// `static abstract` order is legal (`abstract static` is an FCS error on both
// sides). Parses in any type at the syntax level (the interface restriction is
// a later semantic check).

/// `static abstract M : int -> int` — the bare (no `member`) static-abstract
/// slot, leading keyword `StaticAbstract`.
#[test]
fn diff_ast_static_abstract_method() {
    assert_asts_match("type T =\n  static abstract M : int -> int\n");
}

/// `static abstract member M : …` — the `member`-keyword variant, leading
/// keyword `StaticAbstractMember`.
#[test]
fn diff_ast_static_abstract_member_method() {
    assert_asts_match("type T =\n  static abstract member M : int -> int\n");
}

/// A property-shaped static-abstract slot (`static abstract P : int`, no arrow).
#[test]
fn diff_ast_static_abstract_property() {
    assert_asts_match("type T =\n  static abstract P : int\n");
}

/// A static-abstract *property with an accessor clause* (`… with get`).
#[test]
fn diff_ast_static_abstract_property_with_get() {
    assert_asts_match("type T =\n  static abstract P : int with get\n");
}

/// `static abstract member inline` — the full modifier stack, mirroring the
/// instance `abstract member inline` test above.
#[test]
fn diff_ast_static_abstract_member_inline() {
    assert_asts_match("type T =\n  static abstract member inline X : int -> int\n");
}

/// Phase 9.10c — a *generic* abstract member (`abstract M<'U> : 'U -> 'U`), FCS's
/// `opt_explicitValTyparDecls` after the name (`SynValSig.explicitTypeParams`,
/// elided). The typar decls reuse the type-header postfix `< … >` parser.
#[test]
fn diff_ast_abstract_generic() {
    assert_asts_match("type T =\n  abstract M<'U> : 'U -> 'U\n");
}

/// Phase 9.10c — an access modifier on an abstract slot (`abstract private M : …`)
/// is *illegal* (abstract slots inherit the type's visibility), but FCS recovers
/// by building the `AbstractSlot` anyway and reporting a diagnostic. We mirror
/// that: consume the access token (elided) + record an error, so the slot — and
/// any following members — still parse. Hence `_allow_errors`.
#[test]
fn diff_ast_abstract_private_recovers() {
    assert_asts_match_allow_errors("type T =\n  abstract private M : int\n");
}

/// Phase 9.10c — `abstract member private M : …` likewise recovers as an
/// `AbstractSlot` (leading keyword `AbstractMember`), the access elided.
#[test]
fn diff_ast_abstract_member_private_recovers() {
    assert_asts_match_allow_errors("type T =\n  abstract member private M : int\n");
}

/// A property-shaped abstract slot with an explicit `with get, set` accessor
/// clause (FCS's `classMemberSpfnGetSet` → `PropertyGetSet`, `pars.fsy:2060`).
/// The `with`/`get`/`set` keywords drive only the slot's `SynMemberKind` flags
/// and trivia, both elided by the normaliser, so the projection is the plain
/// `AbstractSlot { name = "Bar", ty = int }` regardless of the accessor clause.
#[test]
fn diff_ast_abstract_property_get_set() {
    assert_asts_match("type T =\n  abstract Bar : int with get, set\n");
}

/// `abstract … with get` (FCS's `PropertyGet`) — a single accessor.
#[test]
fn diff_ast_abstract_property_get_only() {
    assert_asts_match("type T =\n  abstract Bar : int with get\n");
}

/// `abstract … with set` (FCS's `PropertySet`) — a single accessor.
#[test]
fn diff_ast_abstract_property_set_only() {
    assert_asts_match("type T =\n  abstract Bar : int with set\n");
}

/// `abstract … with set, get` — the accessors in the reversed order (still
/// `PropertyGetSet`; FCS canonicalises the order, elided here).
#[test]
fn diff_ast_abstract_property_set_then_get() {
    assert_asts_match("type T =\n  abstract Bar : int with set, get\n");
}

/// `abstract member … with get, set` — the `member`-keyword leading-keyword
/// variant combined with the accessor clause.
#[test]
fn diff_ast_abstract_member_property_get_set() {
    assert_asts_match("type T =\n  abstract member Bar : int with get, set\n");
}

/// An abstract get/set slot followed by another member — the accessor clause
/// must terminate cleanly (consume its `OEND`) so the member block continues.
#[test]
fn diff_ast_abstract_get_set_then_member() {
    assert_asts_match("type T =\n  abstract Bar : int with get, set\n  member this.N = 1\n");
}

/// Two abstract slots, each ending in its own get/set clause on a separate line
/// — the first clause's `OEND`/`OBLOCKSEP` must be drained so the second slot
/// anchors at the member-block column rather than being swallowed.
#[test]
fn diff_ast_two_abstract_get_set_slots() {
    assert_asts_match("type T =\n  abstract P : int with get\n  abstract Q : int with set\n");
}

/// A get/set abstract slot followed by a *plain* (method-shaped) abstract slot —
/// the accessor clause must not bleed into the following slot.
#[test]
fn diff_ast_abstract_get_set_then_plain_slot() {
    assert_asts_match("type T =\n  abstract P : int with get, set\n  abstract Q : int\n");
}

// ---- Operator- / active-pattern-named abstract slots ----------------------
//
// FCS's abstract-slot name is an `opName` (`pars.fsy:2060` → `nameop`), so an
// abstract slot may be named by an operator (`abstract (+) : …`) or an
// active-pattern (`abstract (|Foo|_|) : …`), reducing to a `SynValSig` whose
// name segment is the mangled `op_*` / folded active-pattern name — exactly as
// a *concrete* member sig or a `let` binding head. Previously the classifier
// claimed only an `Ident` name after `abstract`, and `parse_abstract_slot_at`
// parsed only `Ident`/`QuotedIdent`, so these were clean errors while FCS
// accepted them. The slot now reuses the binding-head operator / active-pattern
// machinery for its name.
// ---------------------------------------------------------------------------

/// A binary operator as an abstract method slot (`abstract (+) : int -> int ->
/// int`). FCS's `op_Addition` name with `OriginalNotationWithParen "+"`; the
/// differential normaliser unwraps it to the source spelling `+`.
#[test]
fn diff_ast_abstract_operator_binary() {
    assert_asts_match("type T =\n  abstract (+) : int -> int -> int\n");
}

/// A comparison operator as an abstract slot (`abstract (<=) : …`).
#[test]
fn diff_ast_abstract_operator_compare() {
    assert_asts_match("type T =\n  abstract (<=) : int -> int -> bool\n");
}

/// The `member`-keyword leading variant with an operator name
/// (`abstract member (+) : …`).
#[test]
fn diff_ast_abstract_member_operator() {
    assert_asts_match("type T =\n  abstract member (+) : int -> int -> int\n");
}

/// A property-shaped operator abstract slot with an accessor clause
/// (`abstract (+) : int with get, set`). Confirms the accessor clause parses
/// after an operator name exactly as after an ident name.
#[test]
fn diff_ast_abstract_operator_property_get_set() {
    assert_asts_match("type T =\n  abstract (+) : int with get, set\n");
}

/// A static-abstract (IWSAM) operator slot (`static abstract (+) : …`) — the
/// F# 7 static-abstract interface member with an operator name.
#[test]
fn diff_ast_static_abstract_operator() {
    assert_asts_match("type T =\n  static abstract (+) : int -> int -> int\n");
}

/// An active-pattern-named abstract slot (`abstract (|Foo|_|) : int`). FCS's
/// `opName` abstract-slot name covers the active-pattern productions; the slot
/// name reduces to the folded `"|Foo|_|"` idText, matching the concrete
/// active-pattern member heads.
#[test]
fn diff_ast_abstract_active_pattern() {
    assert_asts_match("type T =\n  abstract (|Foo|_|) : int\n");
}

/// The `neg18` shape — a *funky* operator-named abstract property slot
/// (`abstract (.[]) : 'T with get, set`, `op_DotLBrackRBrack`). Exercises the
/// operator-named abstract slot together with the clean funky-operator-name
/// admission (`.[]`).
#[test]
fn diff_ast_abstract_funky_dot_bracket_property() {
    assert_asts_match("type T =\n  abstract (.[]) : int with get, set\n");
}

// ---- Phase 9.10c: `when`-constrained abstract slots ----------------------
//
// FCS's `abstractMemberFlags` arm (`pars.fsy:2060`) ends in
// `COLON topTypeWithTypeConstraints`, so an abstract slot's signature type may
// carry a trailing `when` clause — `SynType.WithGlobalConstraints(ty,
// constraints)`. Previously the slot stopped at the `when`; the slot now routes
// its type through `parse_type_with_constraints` (the same `CONSTRAINED_TYPE`
// wrapper a binding/member return type uses, phase 9.3b). Free type variables in
// an abstract signature are auto-generalised to the method's own type parameters,
// so no explicit `<'T>` decl is needed (though one is accepted — see
// `diff_ast_abstract_generic`).

/// A single `comparison` constraint on an abstract method signature.
#[test]
fn diff_ast_abstract_when_single_constraint() {
    assert_asts_match("type T =\n  abstract M : 'T -> 'T when 'T : comparison\n");
}

/// Two type parameters, one constraint each, joined by `and`.
#[test]
fn diff_ast_abstract_when_two_constraints() {
    assert_asts_match("type T =\n  abstract M : 'T -> 'U when 'T : comparison and 'U : equality\n");
}

/// A subtype constraint (`'T :> System.IComparable`).
#[test]
fn diff_ast_abstract_when_subtype() {
    assert_asts_match("type T =\n  abstract M : 'T -> 'T when 'T :> System.IComparable\n");
}

/// An *explicitly* generic abstract member (`abstract M<'U> : …`) with a trailing
/// `when` constraint — the explicit typar decls (elided) and the global
/// constraint coexist.
#[test]
fn diff_ast_abstract_generic_when_constraint() {
    assert_asts_match("type T =\n  abstract M<'U> : 'U -> 'U when 'U : comparison\n");
}

/// A `member`-keyword abstract slot followed by another member — the `when`
/// clause must terminate cleanly so the member block continues.
#[test]
fn diff_ast_abstract_when_then_member() {
    assert_asts_match(
        "type T =\n  abstract M : 'T -> 'T when 'T : comparison\n  member this.N = 1\n",
    );
}

// ---- Named / optional parameters on impl-side abstract slots --------------
//
// The abstract-slot signature type is FCS's `topType` (`abstractMemberFlags …
// COLON topTypeWithTypeConstraints`, `pars.fsy:2060`), the same layer the `.fsi`
// val/member sigs and `delegate of …` use. It admits a labelled argument
// `[?]ident : <appType>` → `SynType.SignatureParameter` at each arrow / tuple
// element. (Implementation-file counterpart of the `.fsi`
// `diff_sig_member_named_params` family in `parser_diff_sig_files.rs`.)

/// A single named parameter on an abstract method (`abstract M : x: int -> int`).
#[test]
fn diff_ast_abstract_named_param() {
    assert_asts_match("type T =\n  abstract M : x: int -> int\n");
}

/// The motivating example — `abstract member M : name: int * n2: string ->
/// bool`: each tuple element is a `SignatureParameter`, on the `member`-keyword
/// variant.
#[test]
fn diff_ast_abstract_member_named_params_tupled() {
    assert_asts_match("type T =\n  abstract member M : name: int * n2: string -> bool\n");
}

/// Curried named parameters on an abstract slot — each arrow argument is a
/// `SignatureParameter`.
#[test]
fn diff_ast_abstract_named_params_curried() {
    assert_asts_match("type T =\n  abstract M : x: int -> y: int -> int\n");
}

/// An optional parameter on an abstract slot (`abstract M : ?x: int -> int`,
/// `isOptional = true`).
#[test]
fn diff_ast_abstract_optional_param() {
    assert_asts_match("type T =\n  abstract M : ?x: int -> int\n");
}

/// A named parameter on an abstract slot mixed with an unnamed (bare-type) one —
/// only the labelled argument is a `SignatureParameter`.
#[test]
fn diff_ast_abstract_named_param_mixed() {
    assert_asts_match("type T =\n  abstract M : int -> y: string -> bool\n");
}

// ---- Phase 9.11a: inherit members ----------------------------------------
//
// `inherit <atomType> [args] [as base]` is a `classDefnMember`
// (`inheritsDefn`, `pars.fsy:2330`). Two AST nodes share the one
// `INHERIT_MEMBER` green node, discriminated by whether constructor args
// follow: `inherit Base()` → `SynMemberDefn.ImplicitInherit(inheritType,
// inheritArgs, inheritAlias, …)` (args present), `inherit Base` →
// `SynMemberDefn.Inherit(baseType, asIdent, …)` (no args). The base type is
// FCS's `atomType` (our `parse_atomic_type`); the args are an
// `atomicExprAfterType` (`()` → `Const Unit`, `(a, b)` → `Paren(Tuple)`).
// No `= <expr>` RHS → the no-RHS-block terminator (like `val` fields). The
// `as base` alias and trivia are elided.

/// Phase 9.11a — the canonical implicit inherit (`inherit Base()`): args `()`
/// → `ImplicitInherit` with `inheritArgs = Const Unit`.
#[test]
fn diff_ast_inherit_unit_args() {
    assert_asts_match("type C() =\n  inherit Base()\n");
}

/// Phase 9.11a — `inherit Base` (no args) → `Inherit(Some Base, None)`.
#[test]
fn diff_ast_inherit_no_args() {
    assert_asts_match("type C() =\n  inherit Base\n");
}

/// Phase 9.11a — a no-primary-ctor type (`type C =`) still admits `inherit`,
/// yielding `Inherit` with no implicit constructor in slot 3.
#[test]
fn diff_ast_inherit_no_ctor() {
    assert_asts_match("type C =\n  inherit Base\n");
}

/// Phase 9.11a — constructor args (`inherit Base(1, 2)`): `inheritArgs =
/// Paren(Tuple[1, 2])`.
#[test]
fn diff_ast_inherit_tuple_args() {
    assert_asts_match("type C() =\n  inherit Base(1, 2)\n");
}

/// Phase 9.11a — a generic base class (`inherit Base<int>()`): the base
/// `inheritType` is `App(Base, [int])` (the HPA `<…>` wrap stays inside the
/// `atomType`).
#[test]
fn diff_ast_inherit_generic_base() {
    assert_asts_match("type C() =\n  inherit Base<int>()\n");
}

/// Phase 9.11a — a dotted base path (`inherit System.Object()`).
#[test]
fn diff_ast_inherit_dotted_base() {
    assert_asts_match("type C() =\n  inherit System.Object()\n");
}

/// Phase 9.11a — the bare `as base` (the `base` *keyword*) takes FCS's `AS BASE`
/// production, which *always* errors (FS0564,
/// `parsInheritDeclarationsCannotHaveAsBindings`), yet FCS still recovers the
/// `ImplicitInherit` AST with the alias normalised to (elided) `base`. We mirror
/// that: record the error, keep the shape. Hence `_allow_errors`.
#[test]
fn diff_ast_inherit_as_base() {
    assert_asts_match_allow_errors("type C() =\n  inherit Base() as base\n");
}

/// Phase 9.11a — a *quoted* `` as ``base`` `` takes FCS's `AS ident` production
/// whose idText is `base`, the **one** error-free `as`-alias form (no FS0564).
/// The alias is elided, so the shape matches `inherit Base()` exactly.
#[test]
fn diff_ast_inherit_as_quoted_base() {
    assert_asts_match("type C() =\n  inherit Base() as ``base``\n");
}

/// Phase 9.11a — a non-`base` alias (`as foo`) is the erroring `AS ident` form
/// (idText ≠ `base`, FS0564); FCS recovers the AST (alias → elided `base`).
#[test]
fn diff_ast_inherit_as_non_base() {
    assert_asts_match_allow_errors("type C() =\n  inherit Base() as foo\n");
}

/// Phase 9.11a — `inherit` followed by a regular member (drives the member-block
/// continuation across the no-RHS inherit item).
#[test]
fn diff_ast_inherit_then_member() {
    assert_asts_match("type C() =\n  inherit Base()\n  member this.M = 1\n");
}

/// Phase 9.11a — an `inherit` nested in a module (exposes the body close
/// virtuals; pins that the no-RHS terminator leaves the enclosing body's
/// separator alone).
#[test]
fn diff_ast_inherit_nested_in_module() {
    assert_asts_match("module M\ntype C() =\n  inherit Base()\nlet y = 1\n");
}

// ---- Phase 9.11b: interface implementations ------------------------------
//
// `interface I` / `interface I with member … = …` is a `classDefnMember`
// (`pars.fsy:2044`): `SynMemberDefn.Interface(interfaceType, withKeyword,
// members option, range)`. The `interface` keyword in member position arrives
// as `Virtual::InterfaceMember` (OINTERFACE_MEMBER), distinct from the paren
// `interface … end` (9.12). The interface type is `appTypeWithoutNull`
// (`parse_app_type`); the optional `with member …` block is byte-identical to
// the 9.13a/9.15b with-augment stream (raw `WITH` + `OBLOCKBEGIN` + members +
// close), so it reuses `parse_with_augmentation_members`, its members nesting
// in the `INTERFACE_IMPL` node (`SynMemberDefn.Interface.members`). No `with`
// → `members = None` (the no-RHS-block terminator, like `inherit`/`val`).

/// Phase 9.11b — a bare `interface I` (no `with`) → `members = None`.
#[test]
fn diff_ast_interface_bare() {
    assert_asts_match("type C() =\n  inherit obj()\n  interface I\n");
}

/// Phase 9.11b — the canonical `interface I with member … = …` → `members =
/// Some [Member]`, the member nested inside the interface.
#[test]
fn diff_ast_interface_with_member() {
    assert_asts_match("type C() =\n  interface I with\n    member this.M = 1\n");
}

/// Phase 9.11b — two members in the interface's `with` block.
#[test]
fn diff_ast_interface_with_two_members() {
    assert_asts_match(
        "type C() =\n  interface I with\n    member this.M = 1\n    member this.N = 2\n",
    );
}

/// Phase 9.11b — a generic interface type (`interface IFoo<int> with …`); the
/// `interfaceType` is `App(IFoo, [int])` (an `appTypeWithoutNull`).
#[test]
fn diff_ast_interface_generic() {
    assert_asts_match("type C() =\n  interface IFoo<int> with\n    member this.M = 1\n");
}

/// Phase 9.11b — a dotted interface path (`interface System.IDisposable with …`).
#[test]
fn diff_ast_interface_dotted() {
    assert_asts_match(
        "type C() =\n  interface System.IDisposable with\n    member this.Dispose() = ()\n",
    );
}

/// Phase 9.11b — an interface alongside `inherit` and a normal member (the
/// repr's member list, slot 1, mixing all three member kinds).
#[test]
fn diff_ast_interface_with_inherit_and_member() {
    assert_asts_match(
        "type C() =\n  inherit obj()\n  interface I with\n    member this.M = 1\n  member this.N = 2\n",
    );
}

/// Phase 9.11b — two interface implementations in one type body (pins the
/// member-block continuation across the first interface's `with`-block close).
#[test]
fn diff_ast_interface_two_in_one_type() {
    assert_asts_match(
        "type C() =\n  interface I with\n    member this.M = 1\n  interface J with\n    member this.N = 2\n",
    );
}

/// Phase 9.11b — an interface in a `with`-augmentation (`type C with interface
/// I with …`): the interface lands in the **outer** `SynTypeDefn.members` slot
/// via the shared augment loop.
#[test]
fn diff_ast_interface_in_augmentation() {
    assert_asts_match("type C with\n  interface I with\n    member this.M = 1\n");
}

/// Phase 9.11b — an interface nested in a module (pins that the with-block close
/// drain leaves the enclosing module body's separator alone).
#[test]
fn diff_ast_interface_nested_in_module() {
    assert_asts_match(
        "module M\ntype C() =\n  interface I with\n    member this.M = 1\nlet y = 1\n",
    );
}

// ---- Phase 9.11b: interface impls as *bare trailing members* ---------------
//
// An `interface I with …` need not sit inside a `with`-augmentation: it is just
// another member-block item, so it may directly follow a union/record/enum repr
// (phase 9.13b's bare-trailing-members form, FCS's `tyconDefnRhs opt_OBLOCKSEP
// classDefnMembers`). The member-position `interface` keyword is LexFilter-
// relabelled to the `OINTERFACE_MEMBER` virtual (like a class-local `let`), so
// the bare-members gate must recognise it on the *filtered* stream — a raw scan
// can't see the relabel. These pin that it lands in the outer
// `SynTypeDefn.members` slot, exactly like the `with`-augmentation form.

/// Phase 9.11b — a bare interface impl directly after a DU repr (no `with`
/// augmentation): `type U = | A | B  interface I with …`. The interface lands
/// in the outer `SynTypeDefn.members` slot.
#[test]
fn diff_ast_interface_on_union_bare() {
    assert_asts_match("type Foo =\n  | A\n  | B\n  interface I with\n    member this.M = 1\n");
}

/// Phase 9.11b — a DU's bare interface impl followed by an ordinary member (the
/// member-block continuation survives the interface's `with`-block close).
#[test]
fn diff_ast_interface_on_union_then_member() {
    assert_asts_match(
        "type Foo =\n  | A\n  | B\n  interface I with\n    member this.M = 1\n  member this.N = 2\n",
    );
}

/// Phase 9.11b — two bare interface impls after a DU repr.
#[test]
fn diff_ast_interface_two_on_union_bare() {
    assert_asts_match(
        "type Foo =\n  | A\n  | B\n  interface I with\n    member this.M = 1\n  interface J with\n    member this.N = 2\n",
    );
}

/// Phase 9.11b — a DU's interface impl in an explicit `with`-augmentation
/// (`type U = | A | B with interface I with …`), the sibling of the bare form
/// above (routes through the shared augment loop instead of the bare gate).
#[test]
fn diff_ast_interface_on_union_in_augmentation() {
    assert_asts_match(
        "type Foo =\n  | A\n  | B\n  with\n  interface I with\n    member this.M = 1\n",
    );
}

/// Phase 9.11b — a bare interface impl directly after a *record* repr: the
/// bare-members gate is shared with the union path, so the same relabelled
/// `interface` virtual must be recognised here too.
#[test]
fn diff_ast_interface_on_record_bare() {
    assert_asts_match("type Foo =\n  { X : int }\n  interface I with\n    member this.M = 1\n");
}

// ---- Phase 9.11b: interface impls closed by an explicit `end` --------------
//
// FCS's `opt_interfaceImplDefn` (`pars.fsy`) lets a `with`-block interface
// implementation be closed by an explicit `end` keyword
// (`interface I with <members> end`). In that offside position LexFilter
// rewrites the `end` to `OEND` (`Virtual::End`) backed by the real `Token::End`,
// so the parser emits its text as an `END_TOK` child of the `INTERFACE_IMPL`.
// The `end` is structurally inert (FCS's `SynMemberDefn.Interface` has no `end`
// slot), so the normalised projection is identical to the offside-closed form.

/// Phase 9.11b — the canonical `interface I with member … end` (explicit `end`
/// closer). Same `SynMemberDefn.Interface` as the offside-closed form.
#[test]
fn diff_ast_interface_with_member_end() {
    assert_asts_match("type C() =\n  interface I with\n    member this.M = 1\n  end\n");
}

/// Phase 9.11b — explicit-`end` interface impl with two members.
#[test]
fn diff_ast_interface_with_two_members_end() {
    assert_asts_match(
        "type C() =\n  interface I with\n    member this.M = 1\n    member this.N = 2\n  end\n",
    );
}

/// Phase 9.11b — a generic-method interface impl closed by `end`, nested inside a
/// `class … end` body (the corpus's `GenericMethodsOnInterface` shape): the inner
/// `end` closes the interface, the outer `end` the class.
#[test]
fn diff_ast_interface_generic_method_end_in_class() {
    assert_asts_match(
        "type T() = class\n  interface ITest with\n    member x.Foo<'t> (v:'t) = [v]\n    member x.Bar<'t, 'u> (y:'t) : 'u option = None\n  end\nend\n",
    );
}

/// Phase 9.11b — an explicit-`end` interface impl followed by a sibling member:
/// the `end` close must leave the outer member-block continuation intact.
#[test]
fn diff_ast_interface_with_member_end_then_member() {
    assert_asts_match(
        "type C() =\n  interface I with\n    member this.M = 1\n  end\n  member this.N = 2\n",
    );
}

/// Phase 9.11b — an *empty* `with`-block closed by `end` (`interface I with end`,
/// single-line, and its multi-line sibling) is a parse error in FCS: the block
/// requires ≥1 member. The explicit-`end` support must not silently accept it —
/// the empty block is left unclosed so the `end` falls to stray-token recovery.
/// Pins that both forms error (and stay lossless), matching FCS's rejection.
#[test]
fn interface_with_empty_block_end_rejects() {
    use borzoi_cst::parser::parse;
    for src in [
        "type C() =\n  interface I with end\n",
        "type C() =\n  interface I with\n  end\n",
    ] {
        let p = parse(src);
        assert!(
            !p.errors.is_empty(),
            "empty `interface I with end` must error (FCS does): {src:?}"
        );
        assert_eq!(p.root.text().to_string(), src, "lossless: {src:?}");
    }
}

// ---- Phase 9.12: explicit class/struct/interface … end kind markers --------
//
// `type T = class … end` / `struct … end` / `interface … end` set the
// `SynTypeDefnRepr.ObjectModel` *kind* to `Class`/`Struct`/`Interface` (vs the
// default `Unspecified`), the member block delimited by an explicit `end`
// instead of offside layout. LexFilter opens a `Paren { Opener::Class/Struct/
// Interface }` and passes the keyword + `end` through as raw tokens; the members
// reuse 9.7–9.11 via `parse_member_block_items`. Encoded as a `CLASS_TOK`/
// `STRUCT_TOK`/`INTERFACE_TOK` + `END_TOK` direct child of the `OBJECT_MODEL_REPR`.

/// Phase 9.12 — `type T = class … end` with a member → `ObjectModel(Class,
/// [Member])`.
#[test]
fn diff_ast_kind_class() {
    assert_asts_match("type T =\n  class\n    member this.M = 1\n  end\n");
}

/// Phase 9.12 — `type T = struct … end` with a `val` field → `ObjectModel(Struct,
/// [ValField])` (reuses the 9.9b `val`).
#[test]
fn diff_ast_kind_struct() {
    assert_asts_match("type T =\n  struct\n    val x : int\n  end\n");
}

/// Phase 9.12 — `type T = interface … end` with an abstract slot →
/// `ObjectModel(Interface, [AbstractSlot])` (reuses the 9.10c abstract slot).
#[test]
fn diff_ast_kind_interface() {
    assert_asts_match("type T =\n  interface\n    abstract M : int\n  end\n");
}

/// Phase 9.12 — an empty `class … end` body → `ObjectModel(Class, [])`.
#[test]
fn diff_ast_kind_class_empty() {
    assert_asts_match("type T =\n  class\n  end\n");
}

/// Phase 9.12 — the keyword on the same line as `=` (`type T = struct … end`),
/// the alternate layout (struct opens its block with no inner `OBLOCKBEGIN`).
#[test]
fn diff_ast_kind_struct_same_line() {
    assert_asts_match("type T = struct\n  val x : int\nend\n");
}

/// Phase 9.12 — two members in a `class … end` (drives the member-block
/// continuation across the explicit-`end`-suppressed close virtuals).
#[test]
fn diff_ast_kind_class_two_members() {
    assert_asts_match("type T =\n  class\n    member this.M = 1\n    member this.N = 2\n  end\n");
}

/// Phase 9.12 — `inherit` + a member inside a `class … end` (composition of 9.11a
/// inside the explicit kind body) → `ObjectModel(Class, [ImplicitInherit, Member])`.
#[test]
fn diff_ast_kind_class_inherit_member() {
    assert_asts_match("type T =\n  class\n    inherit obj()\n    member this.M = 1\n  end\n");
}

/// Phase 9.12 — a kind-marked type nested in a module with a trailing `let` (pins
/// that the post-`end` stray virtuals are drained without stealing the module's
/// separator).
#[test]
fn diff_ast_kind_struct_nested_in_module() {
    assert_asts_match("module M\ntype T =\n  struct\n    val x : int\n  end\nlet y = 1\n");
}

// ---- Phase 9.14: properties with explicit get/set --------------------------
//
// `member this.P with get() = … and set v = …` →
// `SynMemberDefn.GetSetMember(getBinding option, setBinding option, …)`. Each
// accessor is a `SynBinding` whose head duplicates the property path with an
// `extraId` of `get`/`set`. The `with` is OWITH (the `WithAsLet` context, shared
// with 9.9c auto-properties); `get`/`set` are contextual idents; the clause
// closes with OEND. Inline accessor bodies only (offside bodies + indexer
// setters deferred). Detected by a checkpoint dispatch in `parse_member_defn`
// (head, then `Virtual::With` → `GET_SET_MEMBER`).

/// Phase 9.14 — a get-only property (`member this.P with get() = 1`) →
/// `GetSetMember(Some, None)`.
#[test]
fn diff_ast_get_set_get_only() {
    assert_asts_match("type T() =\n  member this.P with get() = 1\n");
}

/// Phase 9.14 — a set-only property (`member this.P with set v = ()`) →
/// `GetSetMember(None, Some)`.
#[test]
fn diff_ast_get_set_set_only() {
    assert_asts_match("type T() =\n  member this.P with set v = ()\n");
}

/// Phase 9.14 — get + set (`member this.P with get() = 1 and set v = ()`).
#[test]
fn diff_ast_get_set_both() {
    assert_asts_match("type T() =\n  member this.P with get() = 1 and set v = ()\n");
}

/// A member-level `inline` on the get/set property form
/// (`member inline this.P with get() = 1 and set v = ()`). FCS records the
/// flag on each accessor binding's `SynBinding.isInline`; the normaliser elides
/// it on both sides, so this pins that the `inline` (consumed ahead of the head
/// in the `GET_SET_MEMBER` branch) does not perturb the property shape.
#[test]
fn diff_ast_get_set_inline() {
    assert_asts_match("type T() =\n  member inline this.P with get() = 1 and set v = ()\n");
}

/// Phase 9.14 — the accessors in the reverse order (`set … and get …`); the
/// `get`/`set` ident drives the slot, not position.
#[test]
fn diff_ast_get_set_reversed() {
    assert_asts_match("type T() =\n  member this.P with set v = () and get() = 1\n");
}

/// Phase 9.14 — an indexer *getter* (`member this.Item with get(i) = 1`); the
/// accessor arg is `(i)` (a `Paren`).
#[test]
fn diff_ast_get_set_indexer_getter() {
    assert_asts_match("type T() =\n  member this.Item with get(i) = 1\n");
}

/// Phase 9.14b — an indexer *setter* `set i v` (index + value): FCS bundles the
/// space-separated params into a single `SynPat.Tuple(i, v)` arg (vs. our parser's
/// two curried pats), reconstructed by the normaliser.
#[test]
fn diff_ast_get_set_indexer_setter() {
    assert_asts_match("type T() =\n  member this.Item with get(i) = 1 and set i v = ()\n");
}

/// Return-type annotation on a get/set *accessor* — `with get() : int = 1`.
/// FCS models each accessor as a `SynBinding`, so the `: int` is that binding's
/// `returnInfo` and (as everywhere) the body is wrapped in `SynExpr.Typed(Const
/// 1, int)`. We emit a `BINDING_RETURN_INFO` inside the `GET_SET_ACCESSOR` and
/// the normaliser wraps the accessor body in `Typed` to match.
#[test]
fn diff_ast_get_set_getter_return_type() {
    assert_asts_match("type T() =\n  member this.P with get() : int = 1\n");
}

/// Return type on an *indexer* getter — `with get(i) : int = i`. Confirms the
/// arg sweep stops at the `:` and the type attaches as the accessor return info.
#[test]
fn diff_ast_get_set_indexer_getter_return_type() {
    assert_asts_match("type T() =\n  member this.Item with get(i) : int = i\n");
}

/// Return type on a *setter* accessor — `with set v : unit = ()`. FCS accepts
/// it (the accessor binding carries the `returnInfo`/`Typed` like a getter).
#[test]
fn diff_ast_get_set_setter_return_type() {
    assert_asts_match("type T() =\n  member this.P with set v : unit = ()\n");
}

/// Per-accessor independence — `get() : int = 1 and set (v: int) = ()`. Only the
/// getter is typed, so FCS wraps only the getter's body in `Typed` (the setter's
/// `returnInfo` is `None`); our per-accessor projection mirrors that.
#[test]
fn diff_ast_get_set_getter_typed_setter_untyped() {
    assert_asts_match("type T() =\n  member this.P with get() : int = 1 and set (v: int) = ()\n");
}

/// Phase 9.14b — a set-only indexer (`set i v` with no getter).
#[test]
fn diff_ast_get_set_indexer_setter_only() {
    assert_asts_match("type T() =\n  member this.Item with set i v = ()\n");
}

/// Phase 9.14b — a *parenthesised tuple* index (`set (i, j) v`): FCS flattens the
/// paren-tuple index and appends the value into one `Tuple(i, j, v)`.
#[test]
fn diff_ast_get_set_indexer_setter_tuple_index() {
    assert_asts_match("type T() =\n  member this.Item with set (i, j) v = ()\n");
}

/// Phase 9.14b — a parenthesised *singleton* index (`set (i) v`): FCS keeps the
/// `Paren` (`Tuple(Paren(i), v)`) — only a paren-*tuple* index is flattened.
#[test]
fn diff_ast_get_set_indexer_setter_paren_singleton() {
    assert_asts_match("type T() =\n  member this.Item with set (i) v = ()\n");
}

/// Phase 9.14b — a plain (non-indexer) setter `set v` stays a single bare arg
/// (not a tuple); guards that the bundling only fires for the 2-param indexer form.
#[test]
fn diff_ast_get_set_plain_setter_unchanged() {
    assert_asts_match("type T() =\n  member this.P with set v = ()\n");
}

/// Regression — a get/set accessor body that is a *record literal* (`get() =
/// { F = 1 }`) must not over-consume the clause's closing `OEND`. The record
/// `}` is LexFilter-swallowed, so the clause's `OEND` parks at the same span;
/// `parse_record_body` used to eat a trailing `Virtual::End` unconditionally
/// (intended only for the copy-update `{ x with … }` form), stealing this
/// enclosing `OEND`. The theft collapsed the layout so a following column-0
/// `let` was absorbed as a class-local binding instead of a module decl.
#[test]
fn diff_ast_get_set_record_body_then_let() {
    assert_asts_match("type T() =\n  member this.P with get() = { F = 1 }\nlet y = 2\n");
}

/// Regression — the same record-literal accessor body, but as the last member
/// of a trailing `with` augmentation followed by a column-0 `let` (the exact
/// shape of the `E_SettersMustHaveUnit01` divergence: the augmentation used to
/// swallow the `let` as a member binding).
#[test]
fn diff_ast_get_set_record_body_in_augmentation_then_let() {
    assert_asts_match(
        "type T =\n  { F: int }\nwith\n  member this.P with get() = { F = 1 }\nlet y = 2\n",
    );
}

/// Phase 9.14c — a *static* get/set property (`static member P with get() = …`).
/// The 9.14 checkpoint dispatch already routes `static member` here; the static
/// flag is elided and the head path is `[P]` (no `this`), matching FCS — so this
/// works with no code change (the 9.14 "static property head" deferral was just
/// untested). Get+set and get-only.
#[test]
fn diff_ast_get_set_static() {
    assert_asts_match("type T() =\n  static member P with get() = 1 and set v = ()\n");
}

/// Phase 9.14c — a static get-only property.
#[test]
fn diff_ast_get_set_static_get_only() {
    assert_asts_match("type T() =\n  static member P with get() = 1\n");
}

/// Phase 9.14 — an ordinary member followed by a get/set member in one body
/// (drives the member-block continuation across the get/set terminator).
#[test]
fn diff_ast_get_set_after_member() {
    assert_asts_match("type T() =\n  member this.M = 1\n  member this.P with get() = 2\n");
}

/// Phase 9.14 — a get/set member nested in a module with a trailing `let` (pins
/// that the OEND-consumed terminator leaves the module's separator alone).
#[test]
fn diff_ast_get_set_nested_in_module() {
    assert_asts_match(
        "module M\ntype T() =\n  member this.P with get() = 1 and set v = ()\nlet z = 2\n",
    );
}

/// Phase 9.14 — an accessor body that opens an RHS block (`get() = if …`): the
/// body block's `OBLOCKEND` is drained at the accessor boundary, not by
/// `parse_let_equals_rhs`'s drain (which would over-reach). A get-only compound
/// body is FCS-valid (a two-accessor `if … and set` on one line is *not* — FCS
/// errors there, so it is out of scope).
#[test]
fn diff_ast_get_set_compound_body() {
    assert_asts_match("type T() =\n  member this.P with get() = if true then 1 else 2\n");
}

/// Phase 9.14 — an application body (`get() = this.x + 1`), another block-opening
/// body shape.
#[test]
fn diff_ast_get_set_app_body() {
    assert_asts_match("type T() =\n  member this.P with get() = this.x + 1\n");
}

/// Phase 9.14 — accessor accessibility (`… and private set v = …`): the `private`
/// is a per-accessor prefix the clause loop must look *past* (else it would exit
/// early and strand the setter). Projected onto that accessor's
/// [`NormalisedAccessor::access`], so the diff verifies it lands on the setter.
#[test]
fn diff_ast_get_set_accessor_access() {
    assert_asts_match("type T() =\n  member this.P with get() = 1 and private set v = ()\n");
}

/// Phase 9.14 — a *member-level* modifier on a get/set property
/// (`member private this.P with get() = 1 and set v = …`). FCS folds it onto
/// *every* present accessor binding's head pattern, so the projector duplicates
/// the member-level access onto each accessor lacking its own; the diff verifies
/// both the getter and setter carry `private`.
#[test]
fn diff_ast_get_set_member_level_access() {
    assert_asts_match("type T() =\n  member private this.P with get() = 1 and set v = ()\n");
}

/// Phase 9.14 — an `inline` accessor prefix (`with inline get() = …`). Elided.
#[test]
fn diff_ast_get_set_accessor_inline() {
    assert_asts_match("type T() =\n  member this.P with inline get() = 1\n");
}

/// Phase 9.14 — an attribute on an accessor (`with [<A>] get() = …`); the
/// attribute list is consumed (elided until phase 10.7) so the accessor still
/// parses.
#[test]
fn diff_ast_get_set_accessor_attribute() {
    assert_asts_match("type T() =\n  member this.P with [<A>] get() = 1\n");
}

/// Phase 9.14 — offside (multi-line) accessor bodies (`get() =⏎  e and set v =⏎
/// e`): each body opens an offside block whose close is drained at the accessor
/// boundary.
#[test]
fn diff_ast_get_set_offside_bodies() {
    assert_asts_match(
        "module M\ntype T() =\n  member this.P\n    with get() =\n      1\n    and set v =\n      ()\nlet z = 2\n",
    );
}

// ---- Phase 10.7a: type-header attributes (`SynComponentInfo.attributes`) ----
//
// `[<Struct>] type T = …` attaches the attribute list(s) to the type
// definition's `SynComponentInfo` (field 0). The leading `[<…>]` is detected
// at the module-decl dispatch (where the `let`/`use` carrier already lives) and
// routed to the swallowed-`type` definition; the attrs become leading children
// of the first `TYPE_DEFN`. (Standalone `SynModuleDecl.Attributes`,
// module-header, and union/enum/field carriers are 10.7b/c.)

/// The headline use: a `[<Struct>]` attribute on a record type.
#[test]
fn diff_ast_type_attr_struct_record() {
    assert_asts_match("[<Struct>] type T = { X : int }\n");
}

/// Simplest repr — an abbreviation with a bare attribute.
#[test]
fn diff_ast_type_attr_abbrev() {
    assert_asts_match("[<A>] type T = int\n");
}

/// The attribute on its own line before `type` (the `attributeList`'s trailing
/// `opt_OBLOCKSEP` / the offside `[<A>]⏎type T` layout).
#[test]
fn diff_ast_type_attr_offside() {
    assert_asts_match("[<A>]\ntype T = int\n");
}

/// Two adjacent `[<…>]` lists group into the one `SynComponentInfo.attributes`.
#[test]
fn diff_ast_type_attr_two_lists() {
    assert_asts_match("[<A>] [<B>] type T = int\n");
}

/// An attribute with an argument (`[<Foo(1)>]`) — composes with 10.5b.
#[test]
fn diff_ast_type_attr_arg() {
    assert_asts_match("[<Foo(1)>] type T = int\n");
}

/// In an `and`-chain the header attribute attaches to the **first** definition
/// only: `[<A>] type T = int and U = string` → `T`'s `SynComponentInfo` carries
/// `[A]`, `U`'s carries `[]` (ground-truthed via `fcs-dump`).
#[test]
fn diff_ast_type_attr_and_chain_first_only() {
    assert_asts_match("[<A>] type T = int\nand U = string\n");
}

/// The attribute may sit *after* the `type` keyword (`typeNameInfo` position):
/// `type [<A>] T = int` attaches to the same `SynComponentInfo.attributes`.
#[test]
fn diff_ast_type_attr_after_keyword() {
    assert_asts_match("type [<A>] T = int\n");
}

/// And after an `and` keyword: `type T = int and [<B>] U = string` attaches
/// `[B]` to `U`'s `SynComponentInfo` (and `T`'s stays empty).
#[test]
fn diff_ast_type_attr_after_and_keyword() {
    assert_asts_match("type T = int\nand [<B>] U = string\n");
}

/// Phase 10.7a follow-up — an *offside name* after an after-keyword attribute:
/// `type [<A>]⏎T = int`, with the name on a fresh line aligned at column 0.
/// FCS accepts this (`ParseHadErrors: false`) because the attribute production's
/// trailing `opt_OBLOCKSEP` absorbs the inter-line separator the column-0 name
/// emits; the attribute attaches to `T`'s `SynComponentInfo`. (The column-0
/// layout is *only* legal with the attribute present — the bare `type⏎T = int`
/// is an FCS error.)
#[test]
fn diff_ast_type_attr_after_keyword_offside_name() {
    assert_asts_match("type [<A>]\nT = int\n");
}

/// An `and`-chain *after* a column-0 offside name. The attribute-licensed
/// column-0 layout makes every body blockless, but FCS still parses the whole
/// chain (`T(attrs=1), U(attrs=0)`); the continuation must survive the blockless
/// first body. The `and` is offside on its own line.
#[test]
fn diff_ast_type_attr_after_keyword_offside_and_chain() {
    assert_asts_match("type [<A>]\nT = int\nand U = string\n");
}

/// The same column-0 regime, but the `and` sits *inline* after the blockless
/// body (`type [<A>]⏎T = int and U = string`). FCS accepts this too
/// (`ParseHadErrors: false`) — unlike a block-bearing body, where an inline
/// `and` is rejected — because the body opened no block to still be inside.
#[test]
fn diff_ast_type_attr_after_keyword_inline_and() {
    assert_asts_match("type [<A>]\nT = int and U = string\n");
}

/// A three-element chain in the column-0 regime: the regime, established by the
/// first definition's attribute, carries across every blockless `and`.
#[test]
fn diff_ast_type_attr_after_keyword_three_chain() {
    assert_asts_match("type [<A>]\nT = int\nand U = string\nand V = bool\n");
}

/// A column-0 chain whose `and`-defn carries its *own* after-`and` attribute:
/// `T(attrs=1), U(attrs=1)` — both `SynComponentInfo.attributes` populate.
#[test]
fn diff_ast_type_attr_after_keyword_offside_and_attr() {
    assert_asts_match("type [<A>]\nT = int\nand [<B>] U = string\n");
}

/// A *continuation* attribute establishes the column-0 regime on its own: the
/// first definition is an ordinary block-bearing `type T = int`, but the second
/// `and [<B>]⏎U` is a column-0 offside name (blockless body) — FCS's
/// `typeNameInfo` `opt_OBLOCKSEP` applies to every `AND tyconDefn`, so the chain
/// continues to `V` (`T(0), U(1), V(0)`). Pins that the regime is tracked per
/// definition, not just from the head.
#[test]
fn diff_ast_type_attr_continuation_offside_attr_chain() {
    assert_asts_match("type T = int\nand [<B>]\nU = string\nand V = bool\n");
}

/// A column-0 offside name with an *indented* union body. The name's drained
/// separator yields a blockless type, but the union repr's first `|` is itself
/// indented, so the repr parses normally — FCS accepts (`T(attrs=1)`). Composes
/// the offside-name fix with a non-abbreviation body.
#[test]
fn diff_ast_type_attr_offside_name_union_body() {
    assert_asts_match("type [<A>]\nT =\n    | A\n");
}

/// As above with an indented record body — `type [<A>]⏎T =⏎    { X : int }`.
#[test]
fn diff_ast_type_attr_offside_name_record_body() {
    assert_asts_match("type [<A>]\nT =\n    { X : int }\n");
}

// ---- Phase 10.7 (case/field attributes): SynUnionCase / SynEnumCase / SynField ----
//
// Attributes on type-repr elements (field 0 of each carrier): union cases
// (`type T = | [<A>] X`), enum cases (`type E = | [<A>] A = 0`), record fields
// (`type R = { [<A>] X : int }`). The attribute lists are parsed at the start of
// each element (after the `|` bar / inside the field), as leading children, via
// the shared `parse_attribute_lists`.

/// Phase 10.7 — an attributed union case (`type T = | [<A>] X`).
#[test]
fn diff_ast_union_case_attr() {
    assert_asts_match("type T = | [<A>] X\n");
}

/// Phase 10.7 — two attributed union cases, one carrying `of` fields.
#[test]
fn diff_ast_union_case_attr_two() {
    assert_asts_match("type T = | [<A>] X | [<B>] Y of int\n");
}

/// Phase 10.7 — a *mixed* union: one attributed case, one plain.
#[test]
fn diff_ast_union_case_attr_mixed() {
    assert_asts_match("type T = | [<A>] X | Y\n");
}

/// Phase 10.7 — a union-case attribute with an argument (`[<Foo(1)>]`, composing
/// with 10.5b).
#[test]
fn diff_ast_union_case_attr_arg() {
    assert_asts_match("type T = | [<Foo(1)>] X\n");
}

/// Phase 10.7 — an attributed enum case (`type E = | [<A>] A = 0 | B = 1`).
#[test]
fn diff_ast_enum_case_attr() {
    assert_asts_match("type E = | [<A>] A = 0 | B = 1\n");
}

/// Phase 10.7 — an attributed record field (`type R = { [<A>] X : int }`).
#[test]
fn diff_ast_record_field_attr() {
    assert_asts_match("type R = { [<A>] X : int }\n");
}

/// Phase 10.7 — an attributed `mutable` record field alongside a plain field.
#[test]
fn diff_ast_record_field_attr_mutable_and_plain() {
    assert_asts_match("type R = { [<A>] mutable X : int; Y : string }\n");
}

/// Phase 10.7 — a union-case attribute on its *own line* (`| [<A>]⏎  X`): the
/// attribute list's trailing offside `OBLOCKSEP` is drained before the case name.
#[test]
fn diff_ast_union_case_attr_own_line() {
    assert_asts_match("type T =\n  | [<A>]\n    X\n  | Y\n");
}

/// Phase 10.7 — a record-field attribute on its own line (`{ [<A>]⏎  X : int }`).
#[test]
fn diff_ast_record_field_attr_own_line() {
    assert_asts_match("type R =\n  { [<A>]\n    X : int }\n");
}

// ---- Phase 10.7 (standalone module attributes): SynModuleDecl.Attributes ----
//
// A leading `[<…>]` at module scope that is *not* attached to a carrier
// (`let`/`type`/nested `module`/`exception`) becomes a `SynModuleDecl.Attributes`
// (`[<assembly: …>]`). Emitted from the leading-attr dispatch's `else` arm when
// the following construct is not one of those deferred carriers.

/// Phase 10.7 — a targeted assembly attribute followed by an expression
/// declaration (`[<assembly: Foo>]⏎ ignore 0`) → `Attributes` + `Expr`.
#[test]
fn diff_ast_standalone_attributes() {
    assert_asts_match("[<assembly: Foo>]\nignore 0\n");
}

/// Phase 10.7 — the canonical AssemblyInfo idiom `[<assembly: Foo>]⏎ do ()`:
/// the standalone `Attributes` decl is followed by the top-level `do` as its
/// own `Expr(Do …)` decl. Confirms the standalone-attrs `do` lookahead and the
/// `do`-expression slice compose.
#[test]
fn diff_ast_standalone_attributes_before_do() {
    assert_asts_match("[<assembly: Foo>]\ndo ()\n");
}

/// Phase 10.7 — two consecutive standalone attribute lists fold into a single
/// `SynModuleDecl.Attributes` (one decl, two `SynAttributeList`s).
#[test]
fn diff_ast_standalone_attributes_two_lists() {
    assert_asts_match("[<assembly: A>]\n[<assembly: B>]\nignore 0\n");
}

/// Phase 10.7 — a standalone attribute with an argument (`[<assembly:
/// Foo(1)>]`, composing with 10.5b).
#[test]
fn diff_ast_standalone_attributes_arg() {
    assert_asts_match("[<assembly: Foo(1)>]\nignore 0\n");
}

/// Phase 10.7 — a standalone attribute at EOF (no following declaration). FCS
/// emits the `Attributes` decl (with a parse error, elided by the normaliser);
/// our shape matches and we parse it without error.
#[test]
fn diff_ast_standalone_attributes_eof() {
    assert_asts_match_fcs_rejects_ours_accepts("[<assembly: Foo>]\n");
}

/// Phase 10.7 — the following expression on the *same line*
/// (`[<assembly: A>] ignore 0`): FCS's `opt_attributes declExpr` needs no
/// separator, so the `Attributes` decl must not require one before the `Expr`.
#[test]
fn diff_ast_standalone_attributes_same_line_expr() {
    assert_asts_match("[<assembly: A>] ignore 0\n");
}

/// Phase 10.7 — a standalone attribute at the *end of a nested module body*,
/// followed by a sibling `module B`: the attr belongs to `A` (an `Attributes`
/// decl), not the outer `module B`. The end-of-scope `OBLOCKEND` must be treated
/// like EOF rather than letting the raw lookahead cross into `module B`.
#[test]
fn diff_ast_standalone_attributes_end_of_nested_body() {
    assert_asts_match_fcs_rejects_ours_accepts(
        "module A =\n    [<assembly: Foo>]\nmodule B =\n    let x = 1\n",
    );
}

/// Phase 10.7 — adjacent attribute lists must not fold across a scope close: a
/// `[<A>]` at the end of nested `module A` followed by a `[<B>]` in the enclosing
/// scope yields A's `Attributes([A])` *and* an outer `Attributes([B])` (not one
/// folded run). Guards `parse_attribute_lists` stopping at `OBLOCKEND`.
#[test]
fn diff_ast_standalone_attributes_no_cross_scope_fold() {
    assert_asts_match_fcs_rejects_ours_accepts(
        "module A =\n    [<assembly: A>]\n[<assembly: B>]\nignore 0\n",
    );
}

// ---- Phase 10.7d (nested module-header attributes): SynComponentInfo ----
//
// A leading `[<…>]` on a *nested* `module M = …` head attaches to the module's
// `SynComponentInfo.attributes` (FCS field 0), exactly like a type header
// (10.7a). The canonical form puts the attribute on its own line before
// `module`; the same-line `[<A>] module M =` form is an FCS *parse error* (out
// of scope here). Shapes ground-truthed with `fcs-dump ast`.

/// Phase 10.7d — the canonical `[<AutoOpen>]⏎module Inner =` shape. The whole
/// file is the implicit `AnonModule`; its single decl is a `NestedModule` whose
/// `SynComponentInfo.attributes` carries `[AutoOpen]`.
#[test]
fn diff_ast_nested_module_attr() {
    assert_asts_match("[<AutoOpen>]\nmodule Inner =\n    let x = 1\n");
}

/// Phase 10.7d — `module rec` with a header attribute: the attribute lands in
/// `SynComponentInfo.attributes` and `isRecursive` is still set.
#[test]
fn diff_ast_nested_module_attr_rec() {
    assert_asts_match("[<AutoOpen>]\nmodule rec Inner =\n    let x = 1\n");
}

/// Phase 10.7d — two attribute *lists* (the first multi-attribute) ahead of the
/// nested module: both fold into `SynComponentInfo.attributes` as two
/// `SynAttributeList`s.
#[test]
fn diff_ast_nested_module_attr_two_lists() {
    assert_asts_match(
        "[<AutoOpen; CompiledName(\"X\")>]\n[<Foo(1)>]\nmodule Inner =\n    let x = 1\n",
    );
}

/// Phase 10.7d — a header attribute with an argument (`[<CompiledName(\"X\")>]`,
/// composing with 10.5b argument parsing).
#[test]
fn diff_ast_nested_module_attr_arg() {
    assert_asts_match("[<CompiledName(\"X\")>]\nmodule Inner =\n    let x = 1\n");
}

/// Phase 10.7d — an attributed nested module *inside an outer module body*
/// (phase 8.2 whole-file header + phase 8.4 nested decl + 10.7d attribute): the
/// attribute attaches to `Inner`, not the enclosing `Outer`.
#[test]
fn diff_ast_nested_module_attr_in_outer() {
    assert_asts_match("module Outer\n[<AutoOpen>]\nmodule Inner =\n    let x = 1\n");
}

/// Phase 10.7d — an attributed module *abbreviation* (`[<A>]⏎module M = N`) is
/// rejected: FCS emits error 535 "Ignoring attributes on module abbreviation"
/// and drops the decl. Our parser likewise errors and emits an `ERROR` node (not
/// a `MODULE_ABBREV_DECL`), so both sides project an empty decl list.
#[test]
fn diff_ast_nested_module_attr_abbrev_rejected() {
    assert_asts_match_allow_errors("[<A>]\nmodule M = N\n");
}

// ---- Phase 10.7e (whole-file module-header attributes): SynModuleOrNamespace ----
//
// A leading `[<…>]` on a whole-file `module Foo` header (no `=`) attaches to
// `SynModuleOrNamespace.attribs` (FCS field 5) rather than a nested-module
// `SynComponentInfo`. The whole file is one `NamedModule`. Shapes ground-truthed
// with `fcs-dump ast`. (Attributes on a `namespace` header are an FCS error — they
// stay in the deferred-carrier set.)

/// Phase 10.7e — the canonical `[<AutoOpen>]⏎module Foo` whole-file header. The
/// attribute lands in `SynModuleOrNamespace.attribs`; the body `let` follows.
#[test]
fn diff_ast_wholefile_module_attr() {
    assert_asts_match("[<AutoOpen>]\nmodule Foo\nlet x = 1\n");
}

/// Phase 10.7e — `module rec Foo` whole-file header with an attribute: attribs
/// populated and `isRecursive` set.
#[test]
fn diff_ast_wholefile_module_attr_rec() {
    assert_asts_match("[<AutoOpen>]\nmodule rec Foo\nlet x = 1\n");
}

/// Phase 10.7e — two attribute *lists* (the first multi-attribute) ahead of the
/// whole-file header: both fold into `attribs` as two `SynAttributeList`s.
#[test]
fn diff_ast_wholefile_module_attr_two_lists() {
    assert_asts_match("[<AutoOpen; CompiledName(\"X\")>]\n[<Foo(1)>]\nmodule Bar\nlet x = 1\n");
}

/// Phase 10.7e — a header attribute with an argument (`[<CompiledName(\"X\")>]`,
/// composing with 10.5b argument parsing).
#[test]
fn diff_ast_wholefile_module_attr_arg() {
    assert_asts_match("[<CompiledName(\"X\")>]\nmodule Foo\nlet x = 1\n");
}

/// Phase 10.7e — a dotted whole-file header (`module A.B.C`): the attribute
/// attaches to the (single) `NamedModule`, name unaffected.
#[test]
fn diff_ast_wholefile_module_attr_dotted() {
    assert_asts_match("[<AutoOpen>]\nmodule A.B.C\nlet x = 1\n");
}

/// Phase 10.7e — an attributed whole-file header with *no body* (`[<AutoOpen>]⏎
/// module Foo` at EOF). Unlike the empty *nested* body (an FCS error), this is
/// error-free: a `NamedModule` with populated `attribs` and empty decls.
#[test]
fn diff_ast_wholefile_module_attr_no_body() {
    assert_asts_match("[<AutoOpen>]\nmodule Foo\n");
}

// ---- Phase 10.7k (after-keyword module attributes) ----------------------
//
// A `[<…>]` *between* the (swallowed) `module` keyword and the name
// (`module [<A>] Foo`) — FCS's `moduleKeyword opt_attributes opt_access opt_rec
// path`. It shares the home of any *leading* `[<A>] module …` attribute
// (FCS appends `$1@attribs2`): a whole-file header's `SynModuleOrNamespace.attribs`
// (10.7e) or a nested module's `SynComponentInfo.attributes` (10.7d). Shapes
// ground-truthed with `fcs-dump ast`.

/// Phase 10.7k — the canonical whole-file `module [<RequireQualifiedAccess>] Foo`.
#[test]
fn diff_ast_module_after_kw_attr() {
    assert_asts_match("module [<RequireQualifiedAccess>] Foo\nlet x = 1\n");
}

/// Phase 10.7k — a multi-attribute list (`[<A; B>]`) right of `module`.
#[test]
fn diff_ast_module_after_kw_attr_multi() {
    assert_asts_match("module [<A; B>] Foo\nlet x = 1\n");
}

/// Phase 10.7k — an after-keyword attribute with an argument (composing 10.5b).
#[test]
fn diff_ast_module_after_kw_attr_arg() {
    assert_asts_match("module [<CompiledName(\"X\")>] Foo\nlet y = 2\n");
}

/// Phase 10.7k — the offside `module [<A>]⏎  Foo` layout (the name on the next
/// line). The attribute still attaches to the whole-file header.
#[test]
fn diff_ast_module_after_kw_attr_offside() {
    assert_asts_match("module [<A>]\n  Foo\nlet x = 1\n");
}

/// Phase 10.7k — a nested `module [<A>] M = …` (in an anonymous file module): the
/// after-keyword attribute lands in the nested `SynComponentInfo.attributes`.
#[test]
fn diff_ast_nested_module_after_kw_attr() {
    assert_asts_match("module [<A>] M =\n  let x = 1\n");
}

/// Phase 10.7k — a nested after-keyword attribute under a `namespace`.
#[test]
fn diff_ast_nested_module_after_kw_attr_in_namespace() {
    assert_asts_match("namespace N\nmodule [<RequireQualifiedAccess>] M =\n  let x = 1\n");
}

/// Phase 10.7k — `module [<A>] rec M = …`: the attribute precedes `rec` (FCS's
/// `opt_attributes opt_rec`), and `isRecursive` is set.
#[test]
fn diff_ast_nested_module_after_kw_attr_rec() {
    assert_asts_match("module [<A>] rec M =\n  let x = 1\n");
}

/// Phase 10.7k — an after-keyword attribute on a module *abbreviation*
/// (`module [<A>] M = N`) is rejected: FCS emits "Ignoring attributes on module
/// abbreviation" and drops the decl. Our parser likewise errors and emits an
/// `ERROR` node, so both sides project an empty decl list.
#[test]
fn diff_ast_nested_module_after_kw_attr_abbrev_rejected() {
    assert_asts_match_allow_errors("module [<A>] M = N\n");
}

// ---- Phase 10.7f (member-definition attributes): SynBinding.attributes ----
//
// A leading `[<…>]` on a `member`/`static member`/`override`/`default` method or
// property, an explicit `new(…)` ctor, or a get/set property attaches to the
// member's `SynBinding.attributes` (FCS field 4) — for get/set, FCS *duplicates*
// the attribute onto *both* accessor bindings. The FCS-side normaliser already
// reads `SynBinding.attributes`; this slice fills in the CST side. Shapes
// ground-truthed with `fcs-dump ast`.

/// Phase 10.7f — an instance method with a leading attribute.
#[test]
fn diff_ast_member_attr() {
    assert_asts_match("type T() =\n    [<A>] member this.M() = 1\n");
}

/// Phase 10.7f — an attributed simple property (`member this.P = …`).
#[test]
fn diff_ast_member_attr_property() {
    assert_asts_match("type T() =\n    [<A>] member this.P = 2\n");
}

/// Phase 10.7f — a `static member` with an attribute.
#[test]
fn diff_ast_member_attr_static() {
    assert_asts_match("type T() =\n    [<A>] static member S() = 3\n");
}

/// Phase 10.7f — an `override` member with an attribute.
#[test]
fn diff_ast_member_attr_override() {
    assert_asts_match("type T() =\n    [<A>] override this.ToString() = \"\"\n");
}

/// Phase 10.7f — an attribute on its own line ahead of the member (the offside
/// `[<A>]⏎member …` form, the common multi-attribute layout).
#[test]
fn diff_ast_member_attr_offside() {
    assert_asts_match("type T() =\n    [<A>]\n    member this.M() = 1\n");
}

/// Phase 10.7f — two attribute lists ahead of a member (the first
/// multi-attribute), folding into the binding's `attributes`.
#[test]
fn diff_ast_member_attr_two_lists() {
    assert_asts_match("type T() =\n    [<A; B>]\n    [<C>]\n    member this.M() = 1\n");
}

/// Phase 10.7f — a member attribute with an argument (composing with 10.5b).
#[test]
fn diff_ast_member_attr_arg() {
    assert_asts_match("type T() =\n    [<CompiledName(\"X\")>] member this.M() = 1\n");
}

/// Phase 10.7f — an attribute on an explicit `new(…)` constructor (FCS models the
/// ctor as a `Member(SynBinding)`, so the attribute lands in the binding). Uses
/// the canonical record-expression ctor body (a `T()` body hits the unrelated
/// `HighPrecedenceApp` `is_atomic` divergence — see `diff_ast_new_ctor`).
#[test]
fn diff_ast_member_attr_new_ctor() {
    assert_asts_match("type T =\n  val x : int\n  [<A>] new() = { x = 0 }\n");
}

/// Phase 10.7f — an attribute on a get/set property: FCS duplicates it onto
/// *both* the get and set accessor bindings.
#[test]
fn diff_ast_member_attr_get_set() {
    assert_asts_match("type T() =\n    [<A>] member this.G with get() = 1 and set v = ()\n");
}

/// Phase 10.7f — an attributed *bare trailing* member (9.13b): a member after a
/// record repr with no `with`. The attr-aware classifier routes the
/// `[<A>] member …` into the outer `SynTypeDefn.members` slot.
#[test]
fn diff_ast_member_attr_bare_trailing() {
    assert_asts_match("type R =\n    { x : int }\n    [<A>] member this.M() = 1\n");
}

/// Phase 10.7f — a property-level attribute *and* an accessor-level attribute
/// (`[<P>] member this.X with [<A>] get() = 1`): the get binding's attributes are
/// the property list **then** the accessor's own (FCS order), with the property
/// list duplicated onto a present setter.
#[test]
fn diff_ast_member_attr_property_and_accessor() {
    assert_asts_match("type T() =\n    [<P>] member this.X with [<A>] get() = 1\n");
}

// ---- Phase 10.7g (abstract-slot attributes): SynValSig.attributes ----
//
// A leading `[<…>]` on an abstract slot (`[<A>] abstract [member] M : T`) attaches
// to `SynValSig.attributes` (FCS field 0). Shapes ground-truthed with `fcs-dump`.

/// Phase 10.7g — an attributed `abstract member` slot.
#[test]
fn diff_ast_abstract_slot_attr() {
    assert_asts_match("type T =\n    [<A>] abstract member M : int\n");
}

/// Phase 10.7g — an attributed bare `abstract` (property-style) slot.
#[test]
fn diff_ast_abstract_slot_attr_property() {
    assert_asts_match("type T =\n    [<A>] abstract P : string\n");
}

/// Phase 10.7g — a multi-attribute list (`[<C; D>]`) on a function-typed abstract
/// slot.
#[test]
fn diff_ast_abstract_slot_attr_multi() {
    assert_asts_match("type T =\n    [<C; D>] abstract member Q : int -> int\n");
}

/// Phase 10.7g — the offside `[<A>]⏎abstract member …` layout.
#[test]
fn diff_ast_abstract_slot_attr_offside() {
    assert_asts_match("type T =\n    [<A>]\n    abstract member M : int\n");
}

/// Phase 10.7g — an abstract-slot attribute with an argument (composing 10.5b).
#[test]
fn diff_ast_abstract_slot_attr_arg() {
    assert_asts_match("type T =\n    [<CompiledName(\"X\")>] abstract member M : int\n");
}

// ---- Phase 10.7h (auto-property attributes): SynMemberDefn.AutoProperty.attributes ----
//
// A leading `[<…>]` on an auto-property (`[<A>] member val P = …`) attaches to
// `SynMemberDefn.AutoProperty.attributes` (FCS field 0). Shapes ground-truthed
// with `fcs-dump`.

/// Phase 10.7h — an attributed plain auto-property (`propKind = Member`).
#[test]
fn diff_ast_auto_property_attr() {
    assert_asts_match("type T() =\n    [<A>] member val X = 0\n");
}

/// Phase 10.7h — an attributed `with get, set` auto-property (the get/set clause
/// trails the RHS-close, so the attribute attaches at the node head, before it).
#[test]
fn diff_ast_auto_property_attr_get_set() {
    assert_asts_match("type T() =\n    [<A>] member val X = 0 with get, set\n");
}

/// Phase 10.7h — an attributed `static member val` (`isStatic = true`).
#[test]
fn diff_ast_auto_property_attr_static() {
    assert_asts_match("type T() =\n    [<A>] static member val X = 0\n");
}

/// Phase 10.7h — an attribute on a type-annotated auto-property (`: int`).
#[test]
fn diff_ast_auto_property_attr_typed() {
    assert_asts_match("type T() =\n    [<A>] member val X : int = 0\n");
}

/// Phase 10.7h — a multi-attribute list (`[<C; D>]`) on an auto-property.
#[test]
fn diff_ast_auto_property_attr_multi() {
    assert_asts_match("type T() =\n    [<C; D>] member val X = 0\n");
}

/// Phase 10.7h — the offside `[<A>]⏎member val …` layout.
#[test]
fn diff_ast_auto_property_attr_offside() {
    assert_asts_match("type T() =\n    [<A>]\n    member val X = 0\n");
}

/// Phase 10.7h — two attribute lists ahead of an auto-property.
#[test]
fn diff_ast_auto_property_attr_two_lists() {
    assert_asts_match("type T() =\n    [<A>]\n    [<B>] member val X = 0\n");
}

/// Phase 10.7h — an auto-property attribute with an argument (composing 10.5b).
#[test]
fn diff_ast_auto_property_attr_arg() {
    assert_asts_match("type T() =\n    [<CompiledName(\"X\")>] member val X = 0\n");
}

/// Phase 10.7h — an attributed *bare trailing* auto-property (9.13b): one after a
/// record repr with no `with`. The attr-aware classifier routes the
/// `[<A>] member val …` into the outer `SynTypeDefn.members` slot.
#[test]
fn diff_ast_auto_property_attr_bare_trailing() {
    assert_asts_match("type R =\n    { x : int }\n    [<A>] member val M = 1\n");
}

// ---- Phase 10.7i (`val`-field attributes): SynField.attributes ----
//
// A leading `[<…>]` on a `val` field (`[<DefaultValue>] val mutable x : int`)
// attaches to `SynField.attributes` (FCS field 0, the same home as a record
// field — 10.7b). Shapes ground-truthed with `fcs-dump`.

/// Phase 10.7i — the canonical `[<DefaultValue>] val mutable …` field.
#[test]
fn diff_ast_val_field_attr_default_value() {
    assert_asts_match("type T =\n  [<DefaultValue>] val mutable x : int\n");
}

/// Phase 10.7i — an attribute on a plain (non-mutable) `val` field.
#[test]
fn diff_ast_val_field_attr_plain() {
    assert_asts_match("type T =\n  [<A>] val x : int\n");
}

/// Phase 10.7i — an attribute on a `static val mutable` field.
#[test]
fn diff_ast_val_field_attr_static() {
    assert_asts_match("type T =\n  [<A>] static val mutable y : string\n");
}

/// Phase 10.7i — a multi-attribute list (`[<A; B>]`) on a `val` field.
#[test]
fn diff_ast_val_field_attr_multi() {
    assert_asts_match("type T =\n  [<A; B>] val mutable x : int\n");
}

/// Phase 10.7i — the offside `[<DefaultValue>]⏎val mutable …` layout.
#[test]
fn diff_ast_val_field_attr_offside() {
    assert_asts_match("type T =\n  [<DefaultValue>]\n  val mutable x : int\n");
}

/// Phase 10.7i — two attribute lists ahead of a `val` field.
#[test]
fn diff_ast_val_field_attr_two_lists() {
    assert_asts_match("type T =\n  [<A>]\n  [<B>] val x : int\n");
}

/// Phase 10.7i — a `val`-field attribute with an argument (composing 10.5b).
#[test]
fn diff_ast_val_field_attr_arg() {
    assert_asts_match("type T =\n  [<CompiledName(\"X\")>] val x : int\n");
}

/// Phase 10.7i — an attributed `val` field followed by a plain one: only the
/// first carries attributes (the second's `SynField.attributes` is empty).
#[test]
fn diff_ast_val_field_attr_then_plain() {
    assert_asts_match("type T =\n  [<A>] val x : int\n  val y : string\n");
}

// ---- Phase 10.7l (class-local-`let` attributes): SynBinding.attributes ----
//
// A leading `[<…>]` on a class-local `let`/`use` binding
// (`[<VolatileField>] let mutable x = 0`) attaches to the *first* binding's
// `SynBinding.attributes` — FCS's `opt_attributes opt_access classDefnBindings`
// (`pars.fsy:2004`), the same `localBindings` carrier as the module-level `let`
// (phase 10.5). The `static let` form (`pars.fsy:2009`) shares the carrier.
// Shapes ground-truthed with `fcs-dump`. Several same-line forms below are FCS
// recovery parses (`ParseHadErrors = true`) even though the projected
// `SynBinding` shape is useful to pin; the offside forms remain clean.

/// Phase 10.7l — the canonical same-line `[<VolatileField>] let mutable …`
/// (the `[<…>]` and `let` on one line: the `let` arrives as a *raw* `Token::Let`
/// once `parse_attribute_lists` has consumed the run).
#[test]
fn diff_ast_class_local_let_attr_volatile() {
    assert_asts_match_fcs_rejects_ours_accepts(
        "type Foo private (i : int) =\n  [<VolatileField>] let mutable foo = i\n  member _.Foo = foo\n",
    );
}

/// Phase 10.7l — an attribute on a plain (immutable) class-local `let`.
#[test]
fn diff_ast_class_local_let_attr_plain() {
    assert_asts_match_fcs_rejects_ours_accepts("type T() =\n  [<A>] let x = 1\n  member _.X = x\n");
}

/// Phase 10.7l — the offside `[<A>]⏎let …` layout (the attribute on its own
/// line, the `let` arriving as a `Virtual::Let` after a `BlockSep`).
#[test]
fn diff_ast_class_local_let_attr_offside() {
    assert_asts_match("type T() =\n  [<A>]\n  let mutable x = 0\n  member _.X = x\n");
}

/// Phase 10.7l — a multi-attribute list (`[<A; B>]`) on a class-local `let`.
#[test]
fn diff_ast_class_local_let_attr_multi() {
    assert_asts_match_fcs_rejects_ours_accepts(
        "type T() =\n  [<A; B>] let x = 1\n  member _.X = x\n",
    );
}

/// Phase 10.7l — two attribute lists ahead of a class-local `let`.
#[test]
fn diff_ast_class_local_let_attr_two_lists() {
    assert_asts_match_fcs_rejects_ours_accepts(
        "type T() =\n  [<A>]\n  [<B>] let x = 1\n  member _.X = x\n",
    );
}

/// Phase 10.7l — a class-local-`let` attribute with an argument (composing 10.5b).
#[test]
fn diff_ast_class_local_let_attr_arg() {
    assert_asts_match_fcs_rejects_ours_accepts(
        "type T() =\n  [<CompiledName(\"X\")>] let x = 1\n  member _.X = x\n",
    );
}

/// Phase 10.7l — an attribute on a class-local `let rec`.
#[test]
fn diff_ast_class_local_let_attr_rec() {
    assert_asts_match_fcs_rejects_ours_accepts(
        "type T() =\n  [<A>] let rec f x = f x\n  member _.M = 1\n",
    );
}

/// Phase 10.7l — a class-local `let … and …` group: FCS homes the attribute on
/// the *first* binding only (the `and`-chained continuation's `attributes` is
/// empty), mirroring the module-level projection.
#[test]
fn diff_ast_class_local_let_attr_and_chain() {
    assert_asts_match_fcs_rejects_ours_accepts(
        "type T() =\n  [<A>] let rec a = 1\n  and b = 2\n  member _.S = a + b\n",
    );
}

/// Phase 10.7l — an attributed `static let` (FCS's `STATIC classDefnBindings`):
/// the attribute precedes the `static` keyword and homes on the head binding,
/// whose leading keyword stays `StaticLet`.
#[test]
fn diff_ast_class_local_static_let_attr() {
    assert_asts_match("type T() =\n  [<A>] static let x = 1\n  member _.X = x\n");
}

/// Phase 10.7l — an attributed class-local `let` as the *last* item (the
/// bare-trailing form, no following member), driving the body-close terminator.
#[test]
fn diff_ast_class_local_let_attr_only() {
    assert_asts_match("type T() =\n  [<A>] let x = 1\n");
}

/// Phase 10.7l — an attributed class-local `let` followed by a plain one: only
/// the first carries attributes (the second's binding `attributes` is empty).
#[test]
fn diff_ast_class_local_let_attr_then_plain() {
    assert_asts_match_fcs_rejects_ours_accepts(
        "type T() =\n  [<A>] let x = 1\n  let y = 2\n  member _.S = x + y\n",
    );
}

// ---- Phase 9.15a: exception definitions --------------------------------
//
// `SynModuleDecl.Exception(SynExceptionDefn(SynExceptionDefnRepr(attrs,
// caseName: SynUnionCase, longId: LongIdent option, xmlDoc, access, range),
// withKeyword, members, range), range)` (`SyntaxTree.fsi:1771`). The repr's
// `caseName` reuses a `SynUnionCase` (name + `of`-fields, phase 9.5); `longId`
// carries the abbreviation target. Augmentation members (`with member …`) are
// phase 9.15b. Shapes ground-truthed with `dotnet tools/fcs-dump ast`.

/// Phase 9.15a — the bare form `exception E`. `caseName` is a nullary
/// `SynUnionCase`; `longId` is `None`; `members` is empty.
#[test]
fn diff_ast_exception_bare() {
    assert_asts_match("exception E\n");
}

/// Phase 9.15a — a single anonymous payload field (`exception E of int` →
/// `caseName` carries one `SynField`). Reuses the 9.5 `of`-field parser.
#[test]
fn diff_ast_exception_of_int() {
    assert_asts_match("exception E of int\n");
}

/// Phase 9.15a — a `*`-tupled payload mixing a named (`x:int`) and an anonymous
/// (`string`) field, exercising `SynField.idOpt` and the `*` separator.
#[test]
fn diff_ast_exception_of_named_tuple() {
    assert_asts_match("exception E of x:int * string\n");
}

/// Phase 9.15a — the abbreviation form `exception E = SomeExn`, where the `=`
/// introduces the `longId` target path (not an enum value). The case is nullary.
#[test]
fn diff_ast_exception_abbrev() {
    assert_asts_match("exception E = SomeExn\n");
}

/// Phase 9.15a — a dotted abbreviation target (`exception E = System.Exception`):
/// `longId` is the full path.
#[test]
fn diff_ast_exception_abbrev_dotted() {
    assert_asts_match("exception E = System.Exception\n");
}

/// Phase 9.15a — an accessibility modifier is **permitted** on an exception
/// (`exception internal E`, FCS's `exconCore` `opt_access`), unlike a union
/// case. Accessibility is elided, so the normalised shape equals the bare form;
/// both sides parse without error.
#[test]
fn diff_ast_exception_access() {
    assert_asts_match("exception internal E\n");
}

/// Phase 9.15a — an exception followed by a sibling `let`: two decls of the
/// anonymous module. Pins that the (ODECLEND-less) exception close threads into
/// the decl loop's separator handling.
#[test]
fn diff_ast_exception_then_let() {
    assert_asts_match("exception E\nlet x = 1\n");
}

/// Phase 9.15a — an exception nested in a module body, with a following `let`.
/// The exception's offside pop must close before the `let` at the same column.
#[test]
fn diff_ast_exception_in_module() {
    assert_asts_match("module M =\n  exception Foo\n  let x = 1\n");
}

/// Phase 9.15a sad path — `exception E of` with no field type. FCS accepts the
/// parser result as a zero-field case (`exconIntro: ident OF recover`), while
/// our parser still emits a recoverable error. The normalised shape matches the
/// bare case.
#[test]
fn diff_ast_exception_of_no_field_is_error() {
    assert_asts_match_fcs_accepts_ours_rejects("exception E of\n");
}

// ---- Phase 10.7m: exception attributes (`SynExceptionDefnRepr.attributes`) ----
//
// `[<A>] exception E` attaches the attribute list(s) to the exception's
// `SynExceptionDefnRepr` (field 0). FCS's `pars.fsy:1347` concatenates the
// *leading* module-level `opt_attributes` ($1) with the *after-keyword*
// `EXCEPTION opt_attributes …` lists (`cas`) into that one field — `$1 @ cas`,
// in source order. The reused `caseName` (`SynUnionCase`) always carries none.
// The leading `[<…>]` is detected at the module-decl dispatch (where the
// `let`/`type` carriers already live) and routed to the (un-swallowed)
// `exception` definition; the attrs become leading children of the
// `EXCEPTION_DEFN`. Shapes ground-truthed with `dotnet tools/fcs-dump ast`.

/// The headline use: a single bare attribute on a bare exception.
#[test]
fn diff_ast_exception_attr_bare() {
    assert_asts_match("[<A>] exception E\n");
}

/// The attribute on its own line before `exception` (the offside `[<A>]⏎exception`
/// layout — an inter-line `BlockSep` between `>]` and the keyword). This is the
/// shape the `[<NoEquality;NoComparison>]` corpus exceptions use.
#[test]
fn diff_ast_exception_attr_offside() {
    assert_asts_match("[<A>]\nexception E\n");
}

/// Two adjacent `[<…>]` lists group into the one `SynExceptionDefnRepr.attributes`
/// (the `[<NoEquality;NoComparison>]` corpus shape uses a `;`-joined single list;
/// this pins the two-list grouping too).
#[test]
fn diff_ast_exception_attr_two_lists() {
    assert_asts_match("[<A>] [<B>] exception E\n");
}

/// A `;`-separated multi-attribute list (`[<NoEquality;NoComparison>]`) — the
/// exact corpus form — on an `of`-fields exception.
#[test]
fn diff_ast_exception_attr_multi_of_fields() {
    assert_asts_match("[<NoEquality;NoComparison>] exception E of string\n");
}

/// The exact corpus shape from `EmittedIL/Nullness/ExceptionType.fs`: a
/// `[<NoEquality;NoComparison>]` list on an `of`-fields exception with a named
/// `(string|null)` nullness field. The whole file was previously rejected solely
/// for the deferred exception-attribute carrier; this pins the end-to-end match.
#[test]
fn diff_ast_exception_attr_corpus_nullness_field() {
    assert_asts_match(
        "[<NoEquality;NoComparison>] exception NullableMessage of Message:(string|null)\n",
    );
}

/// An attribute with an argument (`[<Foo(1)>]`) — composes with 10.5b.
#[test]
fn diff_ast_exception_attr_arg() {
    assert_asts_match("[<Foo(1)>] exception E\n");
}

/// The attribute may sit *after* the `exception` keyword (FCS's `EXCEPTION
/// opt_attributes opt_access exconIntro`): `exception [<A>] E` attaches to the
/// same `SynExceptionDefnRepr.attributes` slot.
#[test]
fn diff_ast_exception_attr_after_keyword() {
    assert_asts_match("exception [<A>] E\n");
}

/// An *offside name* after an after-keyword attribute: `exception [<A>]⏎E`, with
/// the name on a fresh column-0 line. FCS accepts this (`ParseHadErrors: false`)
/// because the attribute list's trailing `opt_OBLOCKSEP` absorbs the inter-line
/// separator the column-0 name emits — paralleling `type [<A>]⏎T = int`. (The
/// column-0 layout is only legal with the attribute present; bare `exception⏎E`
/// is an FCS error.)
#[test]
fn diff_ast_exception_attr_after_keyword_offside_name() {
    assert_asts_match("exception [<A>]\nE\n");
}

/// The same offside form with an accessibility modifier on the fresh line:
/// `exception [<A>]⏎internal E`. The `opt_OBLOCKSEP` drain must precede the
/// `opt_access`/name parse, not just the name.
#[test]
fn diff_ast_exception_attr_after_keyword_offside_access() {
    assert_asts_match("exception [<A>]\ninternal E\n");
}

/// Both positions at once: `[<A>] exception [<B>] E`. FCS concatenates leading
/// then after-keyword (`$1 @ cas`), so the repr's attributes are `[A; B]`.
#[test]
fn diff_ast_exception_attr_both_positions() {
    assert_asts_match("[<A>] exception [<B>] E\n");
}

/// An attribute on the abbreviation form (`[<A>] exception E = SomeExn`): the
/// attrs ride the repr; the `= path` target is unaffected.
#[test]
fn diff_ast_exception_attr_abbrev() {
    assert_asts_match("[<A>] exception E = SomeExn\n");
}

/// An attributed exception nested in a module body, with a following `let`: the
/// attribute routing must not disturb the (ODECLEND-less) exception close that
/// threads into the sibling `let`.
#[test]
fn diff_ast_exception_attr_in_module() {
    assert_asts_match("module M =\n  [<A>] exception Foo\n  let x = 1\n");
}

/// An attributed exception followed by an attributed `let`: the carrier dispatch
/// must route the first `[<…>]` to the exception and the second to the `let`,
/// without the exception's attrs leaking onto the binding.
#[test]
fn diff_ast_exception_attr_then_attr_let() {
    assert_asts_match("[<A>] exception E\n[<B>] let x = 1\n");
}

// ---- Phase 9.9b: `val` fields ------------------------------------------

/// Phase 9.9b — a `val` field (`type T =`⏎`  val x : int`). FCS gives repr
/// `ObjectModel(Unspecified, members=[ValField])`; the `ValField` wraps a
/// `SynField` (`idOpt = Some "x"`, `fieldType = int`, `isMutable = false`).
#[test]
fn diff_ast_val_field() {
    assert_asts_match("type T =\n  val x : int\n");
}

/// Phase 9.9b — a mutable `val` field (`val mutable x : int`).
#[test]
fn diff_ast_val_field_mutable() {
    assert_asts_match("type T =\n  val mutable x : int\n");
}

/// Phase 9.9b — a `static val` field (`SynField.isStatic = true`).
#[test]
fn diff_ast_static_val_field() {
    assert_asts_match("type T =\n  static val mutable y : string\n");
}

/// Phase 9.9b — a `val` field with a non-atomic field type (`int list`).
#[test]
fn diff_ast_val_field_app_type() {
    assert_asts_match("type T =\n  val items : int list\n");
}

/// Phase 9.9b — two `val` fields (a `val` field has no RHS block, so the
/// inter-item separator is a bare `OBLOCKSEP`).
#[test]
fn diff_ast_two_val_fields() {
    assert_asts_match("type T =\n  val a : int\n  val b : string\n");
}

/// Phase 9.9b — a `val` field followed by a member (drives the val→member
/// transition: `OBLOCKSEP` then `member`).
#[test]
fn diff_ast_val_field_then_member() {
    assert_asts_match("type T =\n  val x : int\n  member this.M = x\n");
}

/// Phase 9.9b — a `val` field with an accessibility modifier
/// (`val mutable internal x : int` — FCS's `VAL opt_mutable opt_access ident`,
/// access *after* `mutable`). The access is consumed and elided.
#[test]
fn diff_ast_val_field_access() {
    assert_asts_match("type C =\n  val mutable internal x : int\n");
}

/// Phase 9.9b — `;`-separated `val` fields (`val x : int; val y : string`).
/// FCS's object-model members allow `opt_seps` (`;` as well as the offside
/// `OBLOCKSEP`); a `val` field has no RHS seq-block to absorb the `;`, so the
/// member-block terminator consumes it (as a real `SEMI_TOK`).
#[test]
fn diff_ast_val_fields_semicolon_separated() {
    assert_asts_match("type T =\n  val x : int; val y : string\n");
}

/// `opt_seps` is a *single* group, so a *repeated* separator is a parse error.
/// FCS reports "Unexpected symbol ';'" but still recovers both `val` fields, so
/// the projected shape matches under `allow_errors`.
#[test]
fn diff_ast_val_fields_repeated_separator() {
    assert_asts_match_allow_errors("type T =\n  val x : int; ; val y : int\n");
}

// ---- Phase 9.13a: type augmentation (`type T with member …`) ------------
//
// `type T with member …` — FCS's `tyconDefnAugmentation`. The `with` stands in
// for the `=`; the repr is `SynTypeDefnRepr.ObjectModel(Augmentation, [])` and
// the members land in the *outer* `SynTypeDefn.members` slot (not the repr).
// Shapes ground-truthed with `dotnet tools/fcs-dump ast`. Trailing members on a
// simple repr (`type R = {…} with member …`) are phase 9.13b.

/// Phase 9.13a — a single-member augmentation. Repr `ObjectModel(Augmentation,
/// [])`; the member is in the outer slot.
#[test]
fn diff_ast_type_augment_member() {
    assert_asts_match("type T with\n  member this.M = 1\n");
}

/// Phase 9.13a — a two-member augmentation, exercising the inter-member offside
/// continuation in the outer slot.
#[test]
fn diff_ast_type_augment_two_members() {
    assert_asts_match("type T with\n  member this.M = 1\n  member this.N = 2\n");
}

/// Phase 9.13a — a `static member` in an augmentation (reuses the 9.9a static
/// member, routed to the outer slot).
#[test]
fn diff_ast_type_augment_static_member() {
    assert_asts_match("type T with\n  static member M = 1\n");
}

/// Phase 9.13a — a generic type's augmentation (`type T<'a> with member …`): the
/// typars precede the `with`.
#[test]
fn diff_ast_type_augment_generic() {
    assert_asts_match("type T<'a> with\n  member this.M = 1\n");
}

/// Phase 9.13a — an augmentation nested in a module body, with a following
/// `let`. The augment's offside close must not swallow the sibling `let`.
#[test]
fn diff_ast_type_augment_in_module() {
    assert_asts_match("module M =\n  type T with\n    member this.M = 1\n  let y = 2\n");
}

/// Phase 9.13a — an augmentation followed by another type definition: two
/// separate `Types` groups (the augment is *not* an `and`-chain continuation).
#[test]
fn diff_ast_type_augment_then_type() {
    assert_asts_match("type T with\n  member this.M = 1\ntype U = int\n");
}

/// Phase 9.13a — an `and`-chain of two augmentations (`type T with … and U with
/// …`): FCS keeps both in **one** `Types` group. The first augment's `declEnd`
/// must be drained so the `and` continuation is reached.
#[test]
fn diff_ast_type_augment_and_chain() {
    assert_asts_match("type T with\n  member this.M = 1\nand U with\n  member this.N = 2\n");
}

/// Phase 9.13a — a class-local `let` binding in an augmentation body. FCS accepts
/// it as a `SynMemberDefn.LetBindings` in the outer slot; our shared member-block
/// helper routes it the same way. Pins that the `let` augment form diff-matches.
#[test]
fn diff_ast_type_augment_let_binding() {
    assert_asts_match("type T with\n  let x = 1\n");
}

// ---- Phase 9.13a: type augmentation closed by an explicit `end` ------------
//
// FCS's `tyconDefnAugmentation` allows the `with`-block to be closed by an
// explicit `end` keyword (`type T with <members> end`), the same offside-block
// closer the 9.11b interface impls and 9.12 class/struct bodies use. In that
// position LexFilter rewrites the `end` to `OEND` (`Virtual::End`) backed by the
// real `Token::End`, so the parser emits its text as an `END_TOK` child of the
// `TYPE_DEFN` (structurally inert — the augmentation projects identically to the
// offside-closed form). Unlike an interface impl, an *empty* augmentation block
// (`type T with end`) is FCS-valid (`ParseHadErrors: false`).

/// Phase 9.13a — the canonical `type T with member … end` (multi-line). Same
/// `ObjectModel(Augmentation, [Member])` as the offside-closed form.
#[test]
fn diff_ast_type_augment_member_end() {
    assert_asts_match("type T with\n  member this.M = 1\n  end\n");
}

/// Phase 9.13a — the single-line `type T with member … end` form.
#[test]
fn diff_ast_type_augment_member_end_single_line() {
    assert_asts_match("type T with member this.M = 1 end\n");
}

/// Phase 9.13a — an explicit-`end` augmentation whose only member is a `val`
/// field (the corpus `neg04.fs` shape: `type R with val x : string end`).
#[test]
fn diff_ast_type_augment_val_end() {
    assert_asts_match("type T with\n  val x : int\n  end\n");
}

/// Phase 9.13a — an `override` member closed by `end` (the corpus
/// `W_OverrideImplementationInAugmentation01a.fs` shape).
#[test]
fn diff_ast_type_augment_override_end() {
    assert_asts_match("type T2b with\n  override x.M = 0\n  end\n");
}

/// Phase 9.13a — a two-member augmentation closed by `end`, exercising the
/// inter-member offside continuation before the explicit `end`.
#[test]
fn diff_ast_type_augment_two_members_end() {
    assert_asts_match("type T with\n  member this.M = 1\n  member this.N = 2\n  end\n");
}

/// Phase 9.13a — an *empty* explicit-`end` augmentation (`type T with end`). FCS
/// accepts it (unlike an empty `interface I with end`): repr
/// `ObjectModel(Augmentation, [])`.
#[test]
fn diff_ast_type_augment_empty_end() {
    assert_asts_match("type T with end\n");
}

/// Phase 9.13a — an empty augmentation whose `end` is on its own *indented* line
/// (`type T with⏎  end`) is also FCS-valid (the `end` opens the empty block where
/// it sits, so `OBLOCKBEGIN` and `OEND` coincide).
#[test]
fn diff_ast_type_augment_empty_end_indented() {
    assert_asts_match("type T with\n  end\n");
}

/// Phase 9.13a — an empty augmentation whose `end` is *offside* (`type T with⏎end`,
/// the `end` at or left of the `type` column) is an FCS parse error (an FS offside
/// diagnostic), unlike the same-line/indented empty forms. We must reject it too:
/// the offside `end` opens the block back at the `with` (so `OBLOCKBEGIN` ≠ `OEND`)
/// and is left to stray-token recovery rather than silently absorbed. Pins that
/// both the type and exception offside forms error and stay lossless.
#[test]
fn type_exception_augment_empty_offside_end_rejects() {
    use borzoi_cst::parser::parse;
    for src in [
        "type T with\nend\n",
        "exception E with\nend\n",
        "module M =\n  type T with\n  end\n", // `end` at the `type` column: still offside
    ] {
        let p = parse(src);
        assert!(
            !p.errors.is_empty(),
            "offside empty `with … end` must error (FCS does): {src:?}"
        );
        assert_eq!(p.root.text().to_string(), src, "lossless: {src:?}");
    }
}

/// Phase 9.13a — an explicit-`end` augmentation followed by a sibling type
/// definition: the `end` close must leave the outer continuation intact so the
/// following `type U` is a separate `Types` group.
#[test]
fn diff_ast_type_augment_end_then_type() {
    assert_asts_match("type T with\n  member this.M = 1\n  end\ntype U = int\n");
}

/// Phase 9.13a — an `and`-chain of two explicit-`end` augmentations (`type T with
/// … end and U with … end`): FCS keeps both in one `Types` group. The first
/// augment's close (the `end` then its `declEnd`) must be drained so the `and`
/// continuation is reached.
#[test]
fn diff_ast_type_augment_and_chain_end() {
    assert_asts_match(
        "type T with\n  member this.M = 1\n  end\nand U with\n  member this.N = 2\n  end\n",
    );
}

// ---- Phase 9.13b: members trailing a repr (`= <repr> [with] member …`) ----
//
// FCS's `tyconDefnRhsBlock` (`pars.fsy:1731`): after the repr, members can
// trail behind a `with` (`opt_classDefn` — valid after *every* repr, including
// an abbreviation and a pure object model, and whatever the `with`'s
// indentation) or *bare* (the #light `classDefnMembers` form — valid only when
// the member arrives inside the still-open body block, i.e. the repr and the
// members are offside-aligned under `type X =`; a single-line repr with a
// member on the next line is FCS's "Unexpected keyword 'member'" error). The
// members land in the **outer** `SynTypeDefn.members` slot; the repr and its
// kind stay unchanged (`Simple(Record …)`, not `Augmentation`). Shapes
// ground-truthed with `dotnet tools/fcs-dump ast`.

/// Phase 9.13b — the inline `with` after a record repr. Repr stays
/// `Simple(Record …)`; the member is in the outer slot.
#[test]
fn diff_ast_record_with_member() {
    assert_asts_match("type R = { X: int } with member this.M = this.X\n");
}

/// Phase 9.13b — the inline `with` after a union repr.
#[test]
fn diff_ast_union_with_member() {
    assert_asts_match("type U = A | B with member this.M = 1\n");
}

/// Phase 9.13b — the inline `with` after an enum repr (the enum-case `= value`
/// loop must stop at the `with`).
#[test]
fn diff_ast_enum_with_member() {
    assert_asts_match("type E = A = 1 with member this.M = 1\n");
}

/// Phase 9.13b — the inline `with` after a *type abbreviation*. FCS accepts
/// `type T = int with member …` at parse time (rejecting augmented
/// abbreviations is a type-checker concern): repr `Simple(TypeAbbrev int)`,
/// member in the outer slot.
#[test]
fn diff_ast_abbrev_with_member() {
    assert_asts_match("type T = int with member this.M = 1\n");
}

/// Phase 9.13b — the offside form: repr and `with` on their own lines, aligned
/// under the body block.
#[test]
fn diff_ast_record_offside_with_member() {
    assert_asts_match("type R =\n    { X: int }\n    with member this.M = this.X\n");
}

/// Phase 9.13b — the *undented* `with` (column 0, left of the repr). LexFilter
/// grants `with` undentation grace, so the stream is identical to the aligned
/// form; FCS parses it via the post-`oblockend` `opt_classDefn` slot.
#[test]
fn diff_ast_record_undented_with_member() {
    assert_asts_match("type R =\n    { X: int }\nwith member this.M = this.X\n");
}

/// Phase 9.13b — a trailing-`with` block with two members (one static),
/// exercising the inter-member `OBLOCKSEP` continuation in the outer slot.
#[test]
fn diff_ast_record_with_two_members() {
    assert_asts_match(
        "type R = { X: int } with\n    member this.M = this.X\n    static member S = 2\n",
    );
}

/// Phase 9.13b — the fully block-formed `with`: keyword on its own line,
/// members indented beneath it.
#[test]
fn diff_ast_record_with_block_members() {
    assert_asts_match(
        "type R =\n    { X: int }\n    with\n        member this.M = this.X\n        static member S = 2\n",
    );
}

/// Phase 9.13b — nested in a module with a following `let`: the augment's
/// close-virtual drain (`OBLOCKEND`+`ODECLEND`) must leave the module body's
/// `OBLOCKSEP` for the enclosing loop, exactly as in the 9.13a augmentation.
#[test]
fn diff_ast_record_with_member_in_module() {
    assert_asts_match(
        "module M =\n    type R = { X: int } with member this.M = this.X\n    let y = 2\n",
    );
}

/// Phase 9.13b — a trailing-`with` definition continued by `and`: one `Types`
/// group of two definitions. The augment's `declEnd` drain leaves the cursor on
/// the `and` with the member block closed, so the chain gate fires.
#[test]
fn diff_ast_record_with_member_and_chain() {
    assert_asts_match(
        "type R =\n    { X: int }\n    with member this.M = this.X\nand S =\n    { Y: int }\n",
    );
}

/// Phase 9.13b — `with` after a *no-bar* single-`of`-case union (`X of int`).
/// The bare-member form is an FCS error here, but the `with` form is valid.
#[test]
fn diff_ast_union_nobar_of_with_member() {
    assert_asts_match("type U =\n    X of int\n    with member this.M = 1\n");
}

/// Phase 9.13b — *bare* trailing members on a record (the #light no-`with`
/// form): `OBLOCKSEP` then `member` inside the still-open body block.
#[test]
fn diff_ast_record_bare_members() {
    assert_asts_match("type R =\n    { a: int }\n    member r.A = r.a\n");
}

/// Phase 9.13b — bare trailing members nested in a module with a following
/// `let` (pins the member-block close against the module body's separator).
#[test]
fn diff_ast_record_bare_members_in_module() {
    assert_asts_match(
        "module M =\n    type R =\n        { X: int }\n        member this.M = this.X\n    let y = 2\n",
    );
}

/// Phase 9.13b — bare trailing members on a bar-led union.
#[test]
fn diff_ast_union_bare_members() {
    assert_asts_match("type U =\n    | A\n    | B\n    member this.M = 1\n");
}

/// Phase 9.13b — bare trailing members on a union whose *first* case has no
/// leading bar (`A` then `| B`): still admitted (the repr carries a bar).
#[test]
fn diff_ast_union_nobar_first_bare_members() {
    assert_asts_match("type U =\n    A\n    | B\n    member this.M = 1\n");
}

/// Phase 9.13b — bare trailing members where union cases carry `of`-fields
/// (first or last): the case-field loop must stop at the member's `OBLOCKSEP`.
#[test]
fn diff_ast_union_of_cases_bare_members() {
    assert_asts_match("type U =\n    | A\n    | B of int\n    member this.M = 1\n");
    assert_asts_match("type U =\n    | A of string\n    | B\n    member this.M = 1\n");
}

/// Phase 9.13b — bare trailing members on an enum (bar-led and no-bar single
/// case: an enum admits them regardless of bars).
#[test]
fn diff_ast_enum_bare_members() {
    assert_asts_match("type E =\n    | A = 1\n    | B = 2\n    member this.M = 3\n");
    assert_asts_match("type E =\n    A = 1\n    member this.M = 3\n");
}

/// Phase 9.13b — a `static member` as the first bare trailing member.
#[test]
fn diff_ast_record_bare_static_member() {
    assert_asts_match("type R =\n    { X: int }\n    static member S = 1\n");
}

/// Phase 9.13b — a class-local `let` as a bare trailing item: FCS puts a
/// `SynMemberDefn.LetBindings` in the outer slot (the filtered stream carries
/// it as the offside `OLET` virtual, not a raw `let`).
#[test]
fn diff_ast_record_bare_let() {
    assert_asts_match("type R =\n    { X: int }\n    let helper = 1\n");
}

/// Phase 9.13b — bare trailing members continued by `and`: the member block's
/// close still leaves the body's `OBLOCKEND` for the repr-close consume, so the
/// chain gate fires.
#[test]
fn diff_ast_record_bare_members_and_chain() {
    assert_asts_match("type R =\n    { X: int }\n    member this.M = 1\nand S =\n    { Y: int }\n");
}

/// Phase 9.13b — a trailing `with` after a pure *object model* (`type C() =
/// member … with member …`): the repr keeps its own members (kind
/// `Unspecified`, ctor duplicated), the with-block's member goes to the outer
/// slot. Same hook as the simple reprs — the body block closes before the
/// `with` in every carrier.
#[test]
fn diff_ast_object_model_trailing_with() {
    assert_asts_match("type C() =\n    member this.A = 1\n    with member this.B = 2\n");
}

// ---- Phase 9.9c: auto-properties (`member val`) ------------------------
//
// `SynMemberDefn.AutoProperty(attributes, isStatic, ident, typeOpt, propKind,
// memberFlags, memberFlagsForSet, xmlDoc, accessibility, synExpr, range,
// trivia)` (`SyntaxTree.fsi:1718`). The normaliser keeps `isStatic`, `ident`,
// `typeOpt`, `propKind` (Member/PropertyGet/PropertyGetSet, driven by `with
// get[, set]`), and the `synExpr` initialiser. Shapes ground-truthed with
// `dotnet tools/fcs-dump ast`.

/// Phase 9.9c — the bare form `member val X = 0` (`propKind = Member`, no `with`).
#[test]
fn diff_ast_auto_property() {
    assert_asts_match("type T() =\n  member val X = 0\n");
}

/// Phase 9.9c — `with get` (`propKind = PropertyGet`).
#[test]
fn diff_ast_auto_property_get() {
    assert_asts_match("type T() =\n  member val X = 0 with get\n");
}

/// Phase 9.9c — `with get, set` (`propKind = PropertyGetSet`); the get/set
/// clause trails the RHS-close `OBLOCKEND`.
#[test]
fn diff_ast_auto_property_get_set() {
    assert_asts_match("type T() =\n  member val X = 0 with get, set\n");
}

/// Phase 9.9c — a `static member val` (`isStatic = true`).
#[test]
fn diff_ast_auto_property_static() {
    assert_asts_match("type T() =\n  static member val X = 0\n");
}

/// Phase 9.9c — a type annotation (`member val X : int = 0`, `typeOpt = Some`).
#[test]
fn diff_ast_auto_property_typed() {
    assert_asts_match("type T() =\n  member val X : int = 0\n");
}

/// Phase 9.9c — an accessibility modifier after `val` (`member val private X`,
/// FCS's `autoPropsDefnDecl` `opt_access`); elided, so the shape equals the bare
/// form.
#[test]
fn diff_ast_auto_property_access() {
    assert_asts_match("type T() =\n  member val private X = 0 with get, set\n");
}

/// Phase 9.9c — a non-atomic initialiser (`member val P = (1, 2)`, a tuple),
/// driving the RHS through a real (non-`Const`) expression.
#[test]
fn diff_ast_auto_property_tuple_init() {
    assert_asts_match("type T() =\n  member val P = (1, 2)\n");
}

/// Phase 9.9c — two auto-properties (the get/set clause's `OEND·ODECLEND`
/// threads into the inter-item separator handling).
#[test]
fn diff_ast_two_auto_properties() {
    assert_asts_match("type T() =\n  member val X = 0 with get, set\n  member val Y = 1\n");
}

/// Phase 9.9c — an auto-property followed by a member method (the auto-property →
/// member transition across the offside separator).
#[test]
fn diff_ast_auto_property_then_member() {
    assert_asts_match("type T() =\n  member val X = 0\n  member this.M = x\n");
}

/// Phase 9.9c — accessor-specific visibility (`with get, private set`, FCS's
/// `opt_access` before each accessor; elided). A following member pins that the
/// access tokens are consumed — otherwise the `OEND` is left unconsumed and the
/// loop truncates the body.
#[test]
fn diff_ast_auto_property_accessor_visibility() {
    assert_asts_match(
        "type T() =\n  member val X = 0 with get, private set\n  member this.M = 1\n",
    );
}

/// Phase 9.9c — visibility on a get-only accessor (`with private get`). FCS
/// homes this on the *getter* slot of the property's `SynValSigAccess`, leaving
/// the *overall* access (`SynValSigAccess` field 0) `None` — so the projected
/// `AutoProperty::access` must stay `None` here, distinct from the
/// `member val private X` overall-access case below.
#[test]
fn diff_ast_auto_property_private_get() {
    assert_asts_match("type T() =\n  member val X = 0 with private get\n");
}

/// The auto-property's *overall* access modifier `member val private X = 0`
/// (`SynValSigAccess` field 0). This is the before-name slot; the diff now
/// verifies the `private` is projected onto [`NormalisedMember::AutoProperty`]
/// rather than dropped, distinguishing it from the getter-slot case above.
#[test]
fn diff_ast_auto_property_overall_private() {
    assert_asts_match("type T() =\n  member val private X = 0\n");
}

/// Phase 9.9c — the set-only clause `with set`. Grammar-accepted
/// (`ParseHadErrors: false`); FCS records `propKind = PropertySet`, distinct
/// from `PropertyGetSet`. (The checker rejects it later, but parsing succeeds.)
#[test]
fn diff_ast_auto_property_set_only() {
    assert_asts_match("type T() =\n  member val X = 0 with set\n");
}

/// Phase 9.9c — a quoted property name (`` member val ``display name`` = 0 ``):
/// the normalised name is backtick-stripped to match FCS's `Ident.idText`.
#[test]
fn diff_ast_auto_property_quoted_name() {
    assert_asts_match("type T() =\n  member val ``display name`` = 0 with get\n");
}

/// Phase 9.9c — backticked accessor names (`` with ``get``, ``set`` ``). FCS's
/// `nameop` dequotes them, so the clause still yields `propKind =
/// PropertyGetSet`; the following member pins that they (and the `OEND`) are
/// consumed.
#[test]
fn diff_ast_auto_property_quoted_accessors() {
    assert_asts_match(
        "type T() =\n  member val X = 0 with ``get``, ``set``\n  member this.M = 1\n",
    );
}

/// Phase 9.9c — a `val` field, an auto-property, and a member in one body:
/// exercises all three object-model item kinds and their distinct terminators.
#[test]
fn diff_ast_val_field_auto_property_member() {
    assert_asts_match(
        "type T() =\n  val x : int\n  member val Y = 0 with get, set\n  member this.M = x\n",
    );
}

// ---- Phase 9.15b: exception augmentation (`exception E with member …`) ----
//
// `exception E with member …` — FCS's `exconDefn = exconCore opt_classDefn`,
// where `opt_classDefn = WITH classDefnBlock declEnd`. The augmentation members
// land in the **outer** `SynExceptionDefn.members` slot (`withKeyword=Some`),
// not in any repr. The filtered stream after the case name is byte-for-byte
// identical to the 9.13a type augmentation's (only the leading keyword differs —
// `exception` passes through where `type` is swallowed), so the member-block
// loop and the close-virtual drain are shared with 9.13a. Shapes ground-truthed
// with `dotnet tools/fcs-dump ast`.

/// Phase 9.15b — a single-member augmentation on a bare exception. FCS's
/// `SynExceptionDefn` carries `withKeyword=Some(_)` and one
/// `SynMemberDefn.Member` in the outer `members` slot.
#[test]
fn diff_ast_exception_augment_member() {
    assert_asts_match("exception E with member this.M = 1\n");
}

/// Phase 9.15b — a two-member augmentation, exercising the inter-member offside
/// continuation (`OBLOCKSEP`) in the outer slot.
#[test]
fn diff_ast_exception_augment_two_members() {
    assert_asts_match("exception E with\n  member this.M = 1\n  member this.N = 2\n");
}

/// Phase 9.15b — a `static member` in an augmentation (reuses the 9.9a static
/// member, routed to the outer slot).
#[test]
fn diff_ast_exception_augment_static_member() {
    assert_asts_match("exception E with\n  static member M = 1\n");
}

/// Phase 9.15b — augmentation on an `of`-fields exception (`exception E of int
/// with member …`): the `of`-field case data (phase 9.15a) precedes the `with`,
/// and the members still land in the outer slot.
#[test]
fn diff_ast_exception_augment_of_fields() {
    assert_asts_match("exception E of int with\n  member this.M = 1\n");
}

/// Phase 9.15b — a class-local `let` binding in an augmentation body. FCS accepts
/// it as a `SynMemberDefn.LetBindings` in the outer slot (the same shared
/// member-block helper as the 9.13a type-augment `let` form). A `let` leaves a
/// second trailing `OffsideDeclEnd`, so this also pins that the close drain only
/// claims one decl-end and leaves the rest to the enclosing loop.
#[test]
fn diff_ast_exception_augment_let_binding() {
    assert_asts_match("exception E with\n  let x = 1\n");
}

/// Phase 9.15b — an augmentation nested in a module body, with a following
/// `let`. Pins the single-pair virtual drain: the augment closes its own member
/// block + decl-end but must **not** steal the surrounding module body's
/// `OBLOCKSEP` before the sibling `let`.
#[test]
fn diff_ast_exception_augment_in_module() {
    assert_asts_match("module M =\n  exception E with member this.M = 1\n  let y = 2\n");
}

/// Phase 9.15b — an augmentation followed by a sibling `let` at the same column.
/// Pins that the (now decl-end-bearing) augmented-exception close threads into
/// the module decl loop's separator handling, unlike the bare 9.15a form.
#[test]
fn diff_ast_exception_augment_then_let() {
    assert_asts_match("exception E with member this.M = 1\nlet y = 2\n");
}

/// Phase 9.15b — the grammar is `exconCore exconRepr opt_classDefn`, so an
/// abbreviation (`exconRepr = EQUALS path`) and an augmentation (`opt_classDefn =
/// WITH …`) can both appear: `exception E = SomeExn with member …` is FCS-valid.
/// The `with` follows the abbreviation path (`parse_long_ident_path` stops at the
/// `with` keyword), and the members still land in the outer slot.
#[test]
fn diff_ast_exception_abbrev_then_augment() {
    assert_asts_match("exception E = SomeExn with member this.M = 1\n");
}

// ---- Phase 9.15b: exception augmentation closed by an explicit `end` --------
//
// The exception augmentation (`opt_classDefn = WITH classDefnBlock declEnd`)
// shares its post-`with` filtered stream with the 9.13a type augmentation, so it
// admits the same explicit-`end` closer (`exception E with <members> end`). The
// `end` lands as an inert `END_TOK` child of the `EXCEPTION_DEFN`; an empty block
// (`exception E with end`) is FCS-valid.

/// Phase 9.15b — the canonical `exception E with member … end` (multi-line).
#[test]
fn diff_ast_exception_augment_member_end() {
    assert_asts_match("exception E with\n  member this.M = 1\n  end\n");
}

/// Phase 9.15b — an `of`-fields exception with an `override` member closed by
/// `end` (the corpus `ExceptionDefinitions/AddMethsProps01.fs` shape).
#[test]
fn diff_ast_exception_augment_of_fields_override_end() {
    assert_asts_match("exception E of int with\n  override this.Message = \"x\"\n  end\n");
}

/// Phase 9.15b — an *empty* explicit-`end` exception augmentation. FCS accepts it.
#[test]
fn diff_ast_exception_augment_empty_end() {
    assert_asts_match("exception E with end\n");
}

/// Phase 9.15b — an explicit-`end` exception augmentation followed by a sibling
/// `let`: the `end` close must leave the enclosing module continuation intact.
#[test]
fn diff_ast_exception_augment_end_then_let() {
    assert_asts_match("exception E with\n  member this.M = 1\n  end\nlet y = 2\n");
}

// ---------------------------------------------------------------------------
// Operator-named members — `static member (+) (a, b) = …` (FCS's `opName` in a
// member head, a `SynPat.LongIdent` whose name segment is the mangled operator
// `op_*` with the source spelling in `OriginalNotation` trivia). No-self-id form
// (the dotted `member x.(+)` form is a later slice).
// ---------------------------------------------------------------------------

/// A binary operator as a static member (`static member (+) (a, b) = a`).
#[test]
fn diff_ast_member_operator_binary() {
    assert_asts_match("type T() =\n  static member (+) (a, b) = a\n");
}

/// A comparison operator (`static member (<=) (a, b) = true`).
#[test]
fn diff_ast_member_operator_compare() {
    assert_asts_match("type T() =\n  static member (<=) (a, b) = true\n");
}

/// The dynamic-set operator (`static member (?<-) (a, b, c) = ()` — the
/// `QuestionOperatorAsMember01` shape).
#[test]
fn diff_ast_member_operator_dynamic_set() {
    assert_asts_match("type T() =\n  static member (?<-) (a, b, c) = ()\n");
}

/// A spaced operator name (`static member ( * ) (a, b) = a`).
#[test]
fn diff_ast_member_operator_spaced_star() {
    assert_asts_match("type T() =\n  static member ( * ) (a, b) = a\n");
}

/// An instance operator member without a self-id (`member (+) (a, b) = a`).
#[test]
fn diff_ast_member_operator_instance() {
    assert_asts_match("type T() =\n  member (+) (a, b) = a\n");
}

/// The index-get funky operator name as a static member
/// (`static member (.()) (v, i) = v`). FCS's `operatorName: FUNKY_OPERATOR_NAME`
/// admits `.()` cleanly (`op_ArrayLookup`); the member head shares the binding-head
/// operator machinery, so the funky-op name reduces to the same `SynPat.LongIdent`
/// as `(+)`.
#[test]
fn diff_ast_member_operator_funky_index_get() {
    assert_asts_match("type T() =\n  static member (.()) (v, i) = v\n");
}

/// The index-set funky operator as an instance member
/// (`member (.()<-) (i, v) = ()` → `op_ArrayAssign`).
#[test]
fn diff_ast_member_operator_funky_index_set() {
    assert_asts_match("type T() =\n  member (.()<-) (i, v) = ()\n");
}

// NOTE: an operator-named *abstract* member sig — `abstract (.[]) : int with
// get, set` (the neg18 shape) — is a *separate* pre-existing gap: `abstract (+)
// : …` with any operator name (funky or not) is not yet parsed, since the
// abstract-slot name path does not reuse the binding-head operator machinery.
// That is deliberately out of scope for the funky-operator-name slice.

// ---------------------------------------------------------------------------
// Active-pattern-named members — `static member (|Foo|Bar|) (x, y) = …`. FCS's
// `opName` member name covers the active-pattern productions, so the member
// head routes through the same `mkSynPatMaybeVar` classifier as a `let`
// binding head: a nullary occurrence collapses to `SynPat.Named("|Foo|Bar|")`,
// a function-form (curried args) one stays `SynPat.LongIdent`. (FCS's *parser*
// accepts these even though the type-checker later rejects active patterns as
// members — these are the `E_ActivePatternMember0{1,3}` corpus fixtures.) The
// dotted self-id form (`member x.(|Foo|_|)`) is a later slice, like the
// operator members above.
// ---------------------------------------------------------------------------

/// A multi-case active-pattern static member with a tupled argument
/// (`static member (|Foo|Bar|) (x, y) = x`) — the `E_ActivePatternMember01`
/// shape; function-form, so `SynPat.LongIdent`.
#[test]
fn diff_ast_member_active_pattern_args() {
    assert_asts_match("type T() =\n  static member (|Foo|Bar|) (x, y) = x\n");
}

/// A nullary single-case active-pattern static member (`static member (|A|) = 7`)
/// — the `E_ActivePatternMember03` shape; var-like, so `SynPat.Named`.
#[test]
fn diff_ast_member_active_pattern_nullary() {
    assert_asts_match("type T() =\n  static member (|A|) = 7\n");
}

/// A nullary multi-case active-pattern static member (`static member (|A|B|) = 0`).
#[test]
fn diff_ast_member_active_pattern_nullary_multi() {
    assert_asts_match("type T() =\n  static member (|A|B|) = 0\n");
}

/// A single-argument active-pattern static member (`static member (|A|B|) x = x`)
/// — function-form, so `SynPat.LongIdent`.
#[test]
fn diff_ast_member_active_pattern_single_arg() {
    assert_asts_match("type T() =\n  static member (|A|B|) x = x\n");
}

/// A partial active-pattern instance member without a self-id
/// (`member (|Foo|_|) y = y`).
#[test]
fn diff_ast_member_active_pattern_partial_instance() {
    assert_asts_match("type T() =\n  member (|Foo|_|) y = y\n");
}

// ---------------------------------------------------------------------------
// Dotted self-id operator / active-pattern member names — `member x.(+) …`,
// `member x.(|Foo|Bar|) …`. FCS folds the self-id and the `opName` into one
// `SynLongIdent` (`["x"; "op_Addition"]` / `["x"; "|Foo|Bar|"]`); we mirror that
// with a `LONG_IDENT` carrying the self-id and (operator form) the op tokens, or
// (active-pattern form) a sibling `ACTIVE_PAT_NAME` whose folded segment the
// normaliser appends after the self-id.
// ---------------------------------------------------------------------------

/// An operator member with a wildcard self-id (`member _.(+) a b = a + b`).
#[test]
fn diff_ast_member_dotted_operator() {
    assert_asts_match("type T() =\n  member _.(+) a b = a + b\n");
}

/// An operator member with a named self-id (`member this.(+) a b = a + b`).
#[test]
fn diff_ast_member_dotted_operator_named_self() {
    assert_asts_match("type T() =\n  member this.(+) a b = a + b\n");
}

/// A multi-case active-pattern member with a self-id (`member x.(|Foo|Bar|) y =
/// y`) — the `E_ActivePatternMember02` shape. FCS parses it (the type-checker
/// later rejects it), so this is a clean parse on both sides.
#[test]
fn diff_ast_member_dotted_active_pattern() {
    assert_asts_match("type T() =\n  member x.(|Foo|Bar|) y = y\n");
}

/// A partial active-pattern member with a wildcard self-id and name-position
/// accessibility (`member private _.(|Foo|_|) y = y`).
#[test]
fn diff_ast_member_dotted_active_pattern_private() {
    assert_asts_match("type T() =\n  member private _.(|Foo|_|) y = y\n");
}

/// A *multi-segment* dotted head before an operator name (`member A.B.(+) a b =
/// …`) — FCS folds it into one `SynLongIdent(["A"; "B"; "op_Addition"])`, so the
/// detection must walk past the intermediate `.B` segment to the final `.(+)`.
#[test]
fn diff_ast_member_dotted_operator_multi_segment() {
    assert_asts_match("type T() =\n  member A.B.(+) a b = a + b\n");
}

/// A multi-segment dotted head before an active-pattern name
/// (`member A.B.(|Foo|_|) y = y`) — `["A"; "B"; "|Foo|_|"]`.
#[test]
fn diff_ast_member_dotted_active_pattern_multi_segment() {
    assert_asts_match("type T() =\n  member A.B.(|Foo|_|) y = y\n");
}

/// A dotted *operator* name on an explicit get/set property
/// (`member x.(+) with get() = 1`) — the get/set name projection reads the whole
/// head, so the `( op )` segment inside the `LONG_IDENT` is kept (`["x"; "+"]`).
#[test]
fn diff_ast_member_dotted_operator_get_set() {
    assert_asts_match("type T() =\n  member x.(+) with get() = 1\n");
}

/// A dotted *active-pattern* name on an explicit get/set property
/// (`member x.(|Foo|_|) with get() = None`) — the sibling `ACTIVE_PAT_NAME`
/// segment must survive into the property name (`["x"; "|Foo|_|"]`).
#[test]
fn diff_ast_member_dotted_active_pattern_get_set() {
    assert_asts_match("type T() =\n  member x.(|Foo|_|) with get() = None\n");
}

// ---------------------------------------------------------------------------
// Name-position accessibility on a member definition — `member private M`,
// `static member internal Make`, `member private this.M`. FCS's member grammar
// (`classDefnMember`) is `[static] member [inline] opt_access nameop …`: the
// modifier sits *after* the member keywords (and after `inline`) and *before*
// the name. It lands in the head pattern's accessibility (`SynPat.LongIdent` /
// `SynPat.Named`'s `accessibility`), which the normaliser elides on both sides;
// we consume it as an `ACCESS_TOK` (kept out of ERROR), mirroring the
// signature-side `parse_member_sig` and every other accessibility site.
// ---------------------------------------------------------------------------

/// A `static member private` named-value member — the motivating corpus shape
/// (`static member private Create() = …`).
#[test]
fn diff_ast_member_static_private_named() {
    assert_asts_match("type T() =\n  static member private Create() = 1\n");
}

/// A `static member internal` named-value member.
#[test]
fn diff_ast_member_static_internal_named() {
    assert_asts_match("type T() =\n  static member internal Make() = 1\n");
}

/// An instance member with a self-id and access (`member private this.M() = …`).
#[test]
fn diff_ast_member_private_instance() {
    assert_asts_match("type T() =\n  member private this.M() = 1\n");
}

/// A property member with access (`member internal x.P = …`).
#[test]
fn diff_ast_member_internal_property() {
    assert_asts_match("type T() =\n  member internal x.P = 1\n");
}

/// A bare named-value member with access and no arguments (`member private M = …`).
#[test]
fn diff_ast_member_private_bare_value() {
    assert_asts_match("type T() =\n  member private M = 1\n");
}

/// Access composes with `inline` — FCS's order is `member inline private`
/// (`inline` *before* the modifier; `member private inline` is an FCS error).
#[test]
fn diff_ast_member_inline_private() {
    assert_asts_match("type T() =\n  member inline private this.M() = 1\n");
}

/// Access on an operator member (`static member private (+) (a, b) = a`).
#[test]
fn diff_ast_member_private_operator() {
    assert_asts_match("type T() =\n  static member private (+) (a, b) = a\n");
}

/// Access on a get/set property member (`member private this.P with get() = …`).
#[test]
fn diff_ast_member_private_get_set() {
    assert_asts_match("type T() =\n  member private this.P with get() = 1\n");
}

/// Access after `override` (`override private this.M() = …`) — the modifier
/// sits after the `override` leading keyword just as it does after `member`.
#[test]
fn diff_ast_member_override_private() {
    assert_asts_match("type T() =\n  abstract M : unit -> int\n  override private this.M() = 1\n");
}

// ---- Union-case `FullType` signature form ------------------------------
//
// `unionCaseName COLON topType` → `SynUnionCaseKind.FullType(ty, valInfo)`
// (`pars.fsy:2778`), the FSharp.Core `Option`/`Choice` representation:
// `| None : 'T option`, `| Some : Value:'T -> 'T option`. The case carries a
// *type signature* (a `topType`, so argument labels like `Value:` are allowed)
// in place of the `of`-field list.

/// A single nullary `FullType` case — `| None : 'T option`.
#[test]
fn diff_ast_union_fulltype_nullary() {
    assert_asts_match("type Opt<'T> =\n    | None : 'T option\n");
}

/// A `FullType` case with a labelled function signature —
/// `| Some : Value:'T -> 'T option`.
#[test]
fn diff_ast_union_fulltype_labelled_fn() {
    assert_asts_match("type Opt<'T> =\n    | Some : Value:'T -> 'T option\n");
}

/// Both forms together — the `Option`-shaped definition.
#[test]
fn diff_ast_union_fulltype_option_shape() {
    assert_asts_match(
        "type Opt<'T> =\n    | None : 'T option\n    | Some : Value:'T -> 'T option\n",
    );
}

/// A `FullType` case with a plain (unlabelled) function signature.
#[test]
fn diff_ast_union_fulltype_unlabelled_fn() {
    assert_asts_match("type T =\n    | A : int -> T\n");
}

/// A `FullType` case with no leading bar — `type T = A : int -> T`. FCS's
/// `firstUnionCaseDeclOfMany` admits a bar-less first case, so this is a
/// single-case union, not an abbreviation.
#[test]
fn diff_ast_union_fulltype_no_bar() {
    assert_asts_match("type T = A : int -> T\n");
}

// ---- Operator-named union cases (`[]` / `::`) --------------------------
//
// `unionCaseName` also admits the list constructors as operator names
// (`pars.fsy:2810`): `LPAREN LBRACK RBRACK rparen` → `op_Nil` (`[]`) and
// `LPAREN COLON_COLON rparen` → `op_ColonColon` (`::`). FSharp.Core's `list`
// type defines its cases this way, combined with the `FullType` signature form.
// LexFilter swallows the closing `)` (a paren closer), recovered like every
// swallowed `)`.

/// The `[]` (`op_Nil`) operator case, `FullType` form.
#[test]
fn diff_ast_union_op_nil_fulltype() {
    assert_asts_match("type L<'T> =\n    | ([]) : 'T list\n");
}

/// The `::` (`op_ColonColon`) operator case, `FullType` form with a labelled
/// tuple-and-arrow signature.
#[test]
fn diff_ast_union_op_cons_fulltype() {
    assert_asts_match("type L<'T> =\n    | ( :: ) : Head:'T * Tail:'T list -> 'T list\n");
}

/// The `list`-shaped definition — both operator cases together.
#[test]
fn diff_ast_union_op_nil_and_cons() {
    assert_asts_match(
        "type L<'T> =\n    | ([]) : 'T list\n    | ( :: ) : Head:'T * Tail:'T list -> 'T list\n",
    );
}

/// An operator case with an ordinary `of` field list (`op_ColonColon` carrying
/// fields) — the operator name is orthogonal to the case representation.
#[test]
fn diff_ast_union_op_cons_of_fields() {
    assert_asts_match("type L<'T> =\n    | ( :: ) of 'T * 'T list\n");
}

/// A bar-less operator first case — does FCS treat `type T = ([]) : int` as a
/// single-case union, or (given `([])` could be an array-type abbreviation) as
/// something else? Pin whatever FCS does.
#[test]
fn diff_ast_union_op_nil_no_bar() {
    assert_asts_match("type L<'T> = ([]) : 'T list\n");
}

/// An operator-named *enum* case — FCS accepts `type E = | ([]) = 0` (the bar-led
/// `unionCaseName EQUALS atomicExpr` production, `pars.fsy:2785`), recording the
/// compiled `op_Nil` `idText`. The operator-name mapping must apply to enum cases
/// too, not just union cases.
#[test]
fn diff_ast_enum_op_nil_case() {
    assert_asts_match("type E =\n    | ([]) = 0\n    | ( :: ) = 1\n");
}

// ---- Leading UTF-8 BOM offside ------------------------------------------
//
// FCS strips a file-start UTF-8 BOM (`U+FEFF`) so it does not shift the offside
// column of line 1. We keep it as leading trivia for losslessness but must
// start line-1 columns *after* it — otherwise line 1 is offside-shifted right by
// the BOM width and later top-level tokens are wrongly read as continuations of
// the first statement (e.g. `System.Console.WriteLine(rv)` newline `exit rv`
// collapsing into one application). Found via the corpus divergence sweep (530
// corpus files start with a BOM).

/// A BOM followed by three top-level statements: the two after line 1 must stay
/// separate `do` declarations, not fold into the first via application.
#[test]
fn diff_leading_bom_keeps_top_level_statements_separate() {
    assert_asts_match("\u{FEFF}let a = 1\nf (a)\ng a\n");
}

/// A BOM before an indented block body — the first real token is column 0, so a
/// normally-indented member/let body is unaffected.
#[test]
fn diff_leading_bom_then_module() {
    assert_asts_match("\u{FEFF}module M\nlet a = 1\nlet b = 2\n");
}
