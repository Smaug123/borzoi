//! `namespace`/`module`/`open` declaration headers.

use crate::common::assert_filtered_streams_match;

/// Whole-file `namespace Foo` declaration. Exercises CtxtNamespaceHead's
/// single-IDENT path (LexFilter.fs:1726) and the head→body+SeqBlock
/// transition (LexFilter.fs:1742-1745) when the trigger is EOF — handled
/// by `tokenForcesHeadContextClosure` cascading through the head before
/// the body push.
#[test]
fn diff_filtered_namespace_bare() {
    assert_filtered_streams_match("namespace Foo\n");
}

/// Dotted-path namespace. Exercises both arms of CtxtNamespaceHead's
/// state machine: IDENT after the NAMESPACE keyword (prev=Keyword→Ident),
/// DOT after an IDENT (prev=Ident→Keyword), and IDENT after a DOT.
#[test]
fn diff_filtered_namespace_dotted() {
    assert_filtered_streams_match("namespace Foo.Bar\n");
}

/// `namespace global`. FCS L1728 permits GLOBAL as both a valid
/// continuation token (alongside REC / IDENT) and a valid prev-state.
#[test]
fn diff_filtered_namespace_global() {
    assert_filtered_streams_match("namespace global\n");
}

/// Namespace followed by a module declaration. The first non-head token
/// (MODULE at col 0) transitions out of CtxtNamespaceHead, pushing
/// CtxtNamespaceBody + an inner SeqBlock(AddBlockEnd) anchored at
/// MODULE's column.
#[test]
fn diff_filtered_namespace_then_module() {
    assert_filtered_streams_match("namespace Foo.Bar\n\nmodule Some =\n    let x = 1\n");
}

/// Two namespaces in one file with a module in the first. FCS's
/// SeqBlock rule (LexFilter.fs:1831) special-cases the next `NAMESPACE`
/// arriving at the inner block's column under `CtxtNamespaceBody`: it
/// closes the SeqBlock with a -1 grace so the NamespaceBody offside-pop
/// runs and the outer file-level `SeqBlock` emits `OBLOCKSEP` between
/// the two `namespace` declarations.
#[test]
fn diff_filtered_two_namespaces_with_decls() {
    assert_filtered_streams_match(
        "namespace A\nmodule M =\n    let x = 1\nnamespace B\nmodule N =\n    let y = 2\n",
    );
}

/// Top-level `module Foo = body` shape. MODULE is swallowed (FCS uses
/// `pool.Return` + `hwTokenFetch`, no emit); CtxtModuleHead drives the
/// IDENT scan; EQUALS pops the head, pushes CtxtModuleBody(false) and
/// the body SeqBlock(AddBlockEnd) (LexFilter.fs:1771-1776).
#[test]
fn diff_filtered_module_eq_body() {
    assert_filtered_streams_match("module Foo =\n    let x = 1\n");
}

/// Whole-file `module Foo\n…` shape. The first non-head token at col 0
/// hits the catch-all (LexFilter.fs:1777) which inspects `rest`; finding
/// `[CtxtSeqBlock]` it pushes CtxtModuleBody(wholeFile=true) + SeqBlock
/// (LexFilter.fs:1789-1791).
#[test]
fn diff_filtered_module_whole_file() {
    assert_filtered_streams_match("module Foo\nlet x = 1\n");
}

/// `module rec Foo = …`. Tests the (MODULE | REC | DOT) → (REC | IDENT)
/// arm: REC arrives at prev=Module, advances to prev=RecOrDot; IDENT then
/// arrives at prev=RecOrDot and advances to prev=Ident.
#[test]
fn diff_filtered_module_rec() {
    assert_filtered_streams_match("module rec Foo =\n    let x = 1\n");
}

/// Access modifier between MODULE and the path. FCS L1761 passes
/// PUBLIC/PRIVATE/INTERNAL through unchanged while keeping prev=Module
/// so the subsequent IDENT still matches the MODULE-prev accept set.
#[test]
fn diff_filtered_module_internal() {
    assert_filtered_streams_match("module internal Foo.Bar\n\nlet x = 1\n");
}

/// Module attributes block: `module [<RequireQualifiedAccess>] Foo`.
/// LBRACK_LESS flips CtxtModuleHead.attrs on; tokens inside pass through
/// at col > head; GREATER_RBRACK flips attrs back off.
#[test]
fn diff_filtered_module_attributes() {
    assert_filtered_streams_match("module [<RequireQualifiedAccess>] Foo =\n    let x = 1\n");
}

/// Module attribute with `=` inside `[<...>]` (named argument). The attrs
/// passthrough must take precedence over the EQUALS/COLON transition —
/// otherwise the `=` in `Name = "x"` is mistaken for the module-body
/// delimiter and the rest of the head mis-tokenises. Uses a space between
/// identifier and `(` so the `HighPrecedenceParenthesisApp` adjacency rule
/// (LexFilter.fs:2655) doesn't fire, keeping the test focused on the
/// attrs-internal `RPAREN` swallow (FCS's outer wrapper unconditionally
/// converts RPAREN to *_COMING_SOON faux tokens that are filtered out).
#[test]
fn diff_filtered_module_attribute_with_colon_arg() {
    assert_filtered_streams_match("module [<Foo (x : int)>] Bar =\n    let x = 1\n");
}

/// Module attribute whose call argument lives in an adjacent `(` —
/// `Foo(Name = "x")`. The IDENT `Foo` is consumed by the ModuleHead
/// attrs arm, so the HPA dispatch must run BEFORE the context arms;
/// otherwise FCS emits `HighPrecedenceParenthesisApp` between `Foo`
/// and `(` but the port doesn't.
#[test]
fn diff_filtered_module_attribute_with_paren_app_arg() {
    assert_filtered_streams_match("module [<Foo(Name = \"x\")>] M =\n    let x = 1\n");
}

/// Dotted module path with whole-file body. Exercises the IDENT/DOT
/// alternation in CtxtModuleHead's accept patterns.
#[test]
fn diff_filtered_module_dotted_path() {
    assert_filtered_streams_match("module Foo.Bar.Baz =\n    let x = 1\n");
}

/// Nested module inside a namespace + outer module body. Stack at the
/// inner `module`'s MODULE arm: `[SeqBlock, NamespaceBody, SeqBlock,
/// ModuleBody, SeqBlock]`. The `_ :: _` check makes `nested=true`, so
/// `end_token_for_a_ctxt` would emit OBLOCKSEP if the head is
/// force-closed.
#[test]
fn diff_filtered_module_nested_in_namespace() {
    assert_filtered_streams_match(
        "namespace Foo\n\nmodule Outer =\n    let x = 1\n\n    module Inner =\n        let y = 2\n",
    );
}

/// `open` declaration. OPEN opens no LexFilter context, so it flows through
/// the SeqBlock like any ordinary statement: `Open IDENT . IDENT`, then the
/// next line's `OBLOCKSEP`/`OffsideLet`. Foundation check for parser phase
/// 8.1.
#[test]
fn diff_filtered_open_simple() {
    assert_filtered_streams_match("open System.Text\nlet x = 1\n");
}

/// `open type System.Math`. TYPE is swallowed (it pushes a transient
/// `CtxtTypeDefns` exactly as a bare `type` definition does); without a
/// following `=` the next line's offside-pop closes it. Confirms the
/// swallowed-`type` after `open` leaves a sane stream for the parser.
#[test]
fn diff_filtered_open_type() {
    assert_filtered_streams_match("open type System.Math\nlet x = 1\n");
}
