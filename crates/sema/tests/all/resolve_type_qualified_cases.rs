//! FCS-free tests for **type-qualified case** resolution — `Color.Red` where
//! `Color` is a union (`[<RequireQualifiedAccess>]` or not) or an enum type and
//! `Red` is one of its cases. The head `Color` resolves to the type, the whole
//! `Color.Red` span to the case.
//!
//! Same-file, this mirrors the enum path (`resolve_enums.rs`) but now also covers
//! union cases (a non-RQA union case is *also* reachable bare/`Mod.Case`; the
//! type-qualified form is an additional path). Cross-file (`Lib.Color.Red`,
//! `open Lib; Color.Red`) the case is resolved through a project type-qualified-
//! case index, the same way `resolve_qualified_values.rs` resolves a cross-file
//! `Mod.value`.
//!
//! Pinned against FCS (`fcs-dump uses-project`): every form below resolves the
//! whole dotted span to the declaring file's case. A value named like the type
//! that is *later* in source makes `Color.Red` an FS0039 error (member access on
//! the value), so we defer there.

use crate::common::ensure_assembly_fixture_built;
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, DefKind, ProjectItems, Resolution, ResolvedFile, resolve_file, resolve_project,
};
use rowan::{TextRange, TextSize};

fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parsed.errors
    );
    resolve_file(
        &ImplFile::cast(parsed.root).expect("impl file"),
        &ProjectItems::default(),
        &AssemblyEnv::default(),
    )
}

fn impl_file(src: &str) -> ImplFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parsed.errors
    );
    ImplFile::cast(parsed.root).expect("impl file")
}

fn nth(src: &str, needle: &str, n: usize) -> TextRange {
    let mut from = 0;
    for _ in 0..n {
        let i = src[from..].find(needle).expect("occurrence") + from;
        from = i + needle.len();
    }
    let i = src[from..].find(needle).expect("occurrence") + from;
    TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + needle.len()).unwrap().into(),
    )
}

/// Assert that same-file `Color.Red` resolves: head `Color` → the type def, whole
/// `Color.Red` span → the case def of `case_kind`.
fn assert_same_file_qualified_case(src: &str, case_kind: DefKind) {
    let rf = resolve(src);
    // Head `Color` (its second occurrence — the use) → the type def (first `Color`).
    let head = rf
        .resolution_at(nth(src, "Color", 1))
        .expect("head Color resolves");
    let head_def = rf.resolved_def(head).expect("head names a def");
    assert_eq!(head_def.range, nth(src, "Color", 0), "head → the type def");
    assert_eq!(head_def.kind, DefKind::Type, "head is the type");
    // Whole `Color.Red` → the case def (first `Red`).
    let whole = rf
        .resolution_at(nth(src, "Color.Red", 0))
        .expect("Color.Red resolves");
    let case_def = rf.resolved_def(whole).expect("whole names a def");
    assert_eq!(case_def.range, nth(src, "Red", 0), "whole → the case def");
    assert_eq!(case_def.kind, case_kind, "whole is the case");
}

/// Assert that the whole `whole` dotted span in `src1` resolves to file0's case at
/// the first `Red`.
fn assert_cross_file_case(src0: &str, src1: &str, whole: &str) {
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, whole, 0))
        .unwrap_or_else(|| panic!("no resolution at {whole:?}"));
    let (file_idx, def) = proj
        .item_def(res)
        .unwrap_or_else(|| panic!("{whole:?} does not resolve to a cross-file item: {res:?}"));
    assert_eq!(file_idx, 0, "{whole:?} resolves into file0");
    assert_eq!(def.range, nth(src0, "Red", 0), "{whole:?} → file0's case");
}

#[test]
fn an_inaccessible_companion_value_does_not_mis_bind_a_cross_file_export() {
    // The inaccessible-type gate's companion branch must not commit a cross-file
    // target when the companion value is itself inaccessible. file0 exports the
    // literal path `A.Foo.Red`; file1 has a farther public same-file `A.Foo.Red`
    // and a nearer `Nest.A` whose `type private Foo` is inaccessible AND whose
    // companion `module Foo` has a `let private Red` (also inaccessible from the
    // sibling `Nest.B`). fcs-dump: `A.Foo.Red` in `Nest.B` binds the farther
    // same-file `Client.A.Foo.Red`, NOT file0. Returning a terminal `Miss` on the
    // inaccessible companion ends the same-file walk and lets the exact-path
    // cross-file fallback commit file0 — a wrong target (codex).
    let src0 = "module A\n\nmodule Foo =\n    let Red = 111\n";
    let src1 = "module Client\n\nmodule A =\n    type Foo =\n        | Red\n        | Blue\n\nmodule Nest =\n    module A =\n        type private Foo =\n            | Hidden\n        module Foo =\n            let private Red = 0\n\n    module B =\n        let y = A.Foo.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj.file(1).resolution_at(nth(src1, "A.Foo.Red", 0));
    // Must not commit file0's `A.Foo.Red`: either the farther same-file case in
    // file1, or an honest defer — never the earlier-file export.
    if let Some(res) = res
        && let Some((file_idx, _)) = proj.item_def(res)
    {
        assert_ne!(
            file_idx, 0,
            "must not mis-bind file0's A.Foo.Red past an inaccessible companion value"
        );
    }
}

#[test]
fn a_headerless_companion_value_is_not_stepped_over_to_a_root_case() {
    // The inaccessible-companion transparency must NOT over-fire in a headerless
    // file, where a nested module's bindings carry no `qualified` path. Here a root
    // `module A` has `type Foo = | Red | Blue` and a nearer `Nest.A.Foo` has an
    // ACCESSIBLE `let Red = 99`. fcs-dump: `A.Foo.Red` in `Nest.B` binds the nearer
    // let (`Nest.A.Foo.Red`). The companion value's accessibility is unprovable here
    // (no `qualified`), so the resolver must NOT treat it as transparent and step the
    // walk over it onto the root union case — it must bind the nearer let or defer,
    // never the root `| Red`.
    let src = "module A =\n    type Foo =\n        | Red\n        | Blue\n\nmodule Nest =\n    module A =\n        module Foo =\n            let Red = 99\n\n    module B =\n        let y = A.Foo.Red\n";
    let rf = resolve(src);
    if let Some(res) = rf.resolution_at(nth(src, "A.Foo.Red", 0))
        && let Some(def) = rf.resolved_def(res)
    {
        assert_ne!(
            def.range,
            nth(src, "Red", 0),
            "must not step over the nearer companion value onto the root union case"
        );
    }
}

#[test]
fn type_qualified_case_through_a_module_alias_resolves() {
    // `module P = Lib.Pal; P.Color.Red` → `Lib.Pal.Color.Red` (the alias is followed
    // for the type-qualifier head, like a qualified value).
    assert_cross_file_case(
        "namespace Lib\nmodule Pal =\n    type Color = Red | Blue\n",
        "module Client\nmodule P = Lib.Pal\nlet x = P.Color.Red\n",
        "P.Color.Red",
    );
}

#[test]
fn type_qualified_case_alias_shadows_a_colliding_root() {
    // Soundness: with both an alias `module P = Lib.Pal` and a colliding root
    // `module P` (each with `Color.Red`), `P.Color.Red` resolves through the alias
    // (FCS: `Lib.Pal.Color.Red`), NOT the root `P`. The alias is definitive for the
    // head, so the root must not bind.
    let src0 = "namespace Lib\nmodule Pal =\n    type Color = Red | Blue\n";
    let src1 = "module P\ntype Color = Red | Blue\n";
    let src2 = "module Client\nmodule P = Lib.Pal\nlet x = P.Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let res = proj
        .file(2)
        .resolution_at(nth(src2, "P.Color.Red", 0))
        .expect("P.Color.Red resolves");
    let (file_idx, _) = proj.item_def(res).expect("cross-file item");
    assert_eq!(
        file_idx, 0,
        "resolves through the alias to file0 (Lib.Pal), not root P (file1)"
    );
}

#[test]
fn type_qualified_case_resolves_when_a_case_shares_the_type_name_same_file() {
    // `type Color = Color | Red; Color.Red` — the case `Color` (a non-RQA union
    // case, in the value frame) shares the type name, but a case constructor is not
    // a dottable value, so it does NOT shadow the qualifier: FCS resolves `Color.Red`
    // to the type's `Red` case. (Regression: my value-collision rule used to treat
    // the case `Color` as a shadowing value and defer.)
    let src = "module M\ntype Color = Color | Red\nlet c = Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Color.Red", 0))
        .expect("Color.Red resolves");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0), "→ the Red case def");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn type_qualified_case_resolves_when_a_case_shares_the_type_name_cross_file() {
    // The same shape cross-file: the case `Color` exported at `Lib.Color` must not
    // block the type-qualified `Lib.Color.Red`.
    assert_cross_file_case(
        "namespace Lib\ntype Color = Color | Red\n",
        "module Client\nopen Lib\nlet x = Color.Red\n",
        "Color.Red",
    );
    assert_cross_file_case(
        "namespace Lib\ntype Color = Color | Red\n",
        "module Client\nlet x = Lib.Color.Red\n",
        "Lib.Color.Red",
    );
}

// ---- same-file ----

#[test]
fn same_file_union_type_qualified_case_resolves() {
    let src = "type Color = Red | Blue\nlet c = Color.Red\n";
    assert_same_file_qualified_case(src, DefKind::UnionCase);
}

#[test]
fn same_file_require_qualified_union_type_qualified_case_resolves() {
    let src = "[<RequireQualifiedAccess>]\ntype Color = Red | Blue\nlet c = Color.Red\n";
    assert_same_file_qualified_case(src, DefKind::UnionCase);
}

#[test]
fn same_file_enum_type_qualified_case_still_resolves() {
    // Control: the pre-existing enum path is unchanged.
    let src = "type Color = Red = 0 | Blue = 1\nlet c = Color.Red\n";
    assert_same_file_qualified_case(src, DefKind::EnumCase);
}

#[test]
fn type_qualified_case_defers_when_a_later_value_shadows_the_type() {
    // FCS FS0039: a value `Color` later than the type makes `Color.Red` member
    // access on the value, so we defer (never the case).
    let src = "type Color = Red | Blue\nlet Color = 0\nlet c = Color.Red\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("value-shadowed `Color.Red` should defer, got {other:?}"),
    }
}

#[test]
fn module_qualified_case_value_head_does_not_suppress_a_bare_reachable_earlier_module() {
    // A same-file *value* head does NOT block a bare-reachable earlier `Pal.Color.Red`.
    // With a root `module Pal` (file0, so `Pal.Color.Red` is reachable unqualified) and
    // a same-file `exception Pal`, FCS resolves the dotted head to file0's root module
    // `Pal` (the module namespace wins for a dotted path) — the whole `Pal.Color.Red`
    // binds file0's case, NOT member access on the same-file exception. So `Miss`
    // (fall through to the cross-file branches) is exactly right here.
    //
    // (This pins the analysis of a codex report claiming the value head should make
    // this member access: FCS-verified, it does not when the earlier module is
    // bare-reachable — the head resolves to that module. When the earlier module is
    // *not* bare-reachable, FCS does read the same-file value as member access, but then
    // `Miss` merely defers — no wrong target either way.)
    let src0 = "module Pal\ntype Color = Red | Blue\n";
    let src1 = "module Client\nexception Pal of int\nlet x = Pal.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Pal.Color.Red", 0))
        .expect("Pal.Color.Red resolves to the bare-reachable earlier module");
    let (file_idx, def) = proj.item_def(res).expect("cross-file item");
    assert_eq!(file_idx, 0, "→ file0's root module Pal, not member access");
    assert_eq!(def.range, nth(src0, "Red", 0), "→ file0's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn union_type_qualified_case_defers_when_an_earlier_value_shares_the_name() {
    // A *union* case loses the qualifier to **any** in-scope value, even one
    // declared *before* the type: FCS reads `let Color = 0; type Color = Red | Blue;
    // Color.Red` as member access on the value (FS0039), NOT the case. (Enums differ
    // — an earlier value lets the case win, pinned in `resolve_enums.rs`.)
    let src = "let Color = 0\ntype Color = Red | Blue\nlet c = Color.Red\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("value-shadowed union `Color.Red` should defer, got {other:?}"),
    }
}

#[test]
fn enum_type_qualified_case_wins_over_an_earlier_value() {
    // Control (the union case above must NOT regress this): an *enum* case wins the
    // qualifier over an earlier same-named value (FCS resolves `Color.Red` to the
    // case). Pins the union-vs-enum asymmetry in the value-collision rule.
    let src = "let Color = 0\ntype Color = Red = 0 | Blue = 1\nlet c = Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Color.Red", 0))
        .expect("enum Color.Red resolves over an earlier value");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0));
    assert_eq!(def.kind, DefKind::EnumCase);
}

#[test]
fn same_file_type_qualified_case_defers_under_an_opaque_module_open() {
    // FCS: an opened module's submodule out-ranks a same-file type for the
    // qualifier — `open M (module Color with Red); type Color = Red | Blue;
    // Color.Red` resolves through `M.Color.Red`, NOT the same-file union case. While
    // any opaque project-module open is in scope we cannot prove the open lacks such
    // a `Color`, so we defer (sound; never the wrong case). Both positions.
    let expr = "module Top\nmodule M =\n    module Color =\n        let Red = 1\nopen M\ntype Color = Red | Blue\nlet x = Color.Red\n";
    let rf = resolve(expr);
    match rf.resolution_at(nth(expr, "Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => {
            panic!("`Color.Red` under an opaque module open should defer (expr), got {other:?}")
        }
    }
    let pat = "module Top\nmodule M =\n    module Color =\n        let Red = 1\nopen M\ntype Color = Red | Blue\nlet f c = match c with Color.Red -> 1 | _ -> 0\n";
    let rf = resolve(pat);
    match rf.resolution_at(nth(pat, "Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => {
            panic!("`Color.Red` under an opaque module open should defer (pat), got {other:?}")
        }
    }
}

#[test]
fn type_qualified_case_defers_under_an_open_type() {
    // An `open type T` sets `unmodelled_open_active` (T's unmodelled nested types
    // could supply the head and out-rank the project type for the qualifier), so a
    // `Type.Case` reference defers while one is in scope — the same conservatism the
    // qualified-value branch has. (A *modelled* assembly `open type` isolates that
    // flag; here a project `open type` exercises the defer behaviour without an
    // assembly fixture.)
    let src = "module M\ntype Foo = { x : int }\nopen type Foo\ntype Color = Red | Blue\nlet c = Color.Red\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("`Color.Red` under an open type should defer, got {other:?}"),
    }
}

#[test]
fn same_file_union_type_qualified_case_in_pattern_resolves() {
    // `match c with Color.Red` — pattern position resolves identically to the
    // expression form (FCS): the whole `Color.Red` → the case.
    let src = "type Color = Red | Blue\nlet f c = match c with Color.Red -> 1 | _ -> 0\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Color.Red", 0))
        .expect("Color.Red pattern resolves");
    let case_def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(case_def.range, nth(src, "Red", 0));
    assert_eq!(case_def.kind, DefKind::UnionCase);
}

// ---- same-file module-qualified (Gap A, clean case via the per-container name view) ----

/// Assert a same-file *module-qualified* `Pal.Color.Red` resolves: the whole span →
/// the case def (`case_kind`), and the type segment `Color` (its use) → the type def.
/// (FCS: whole → `<root>.Pal.Color.Red`; middle `Color` → the type; the module
/// segment is a nav gap we defer.)
fn assert_same_file_module_qualified_case(src: &str, case_kind: DefKind) {
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("Pal.Color.Red resolves");
    let case_def = rf.resolved_def(whole).expect("whole names a def");
    assert_eq!(case_def.range, nth(src, "Red", 0), "whole → the case def");
    assert_eq!(case_def.kind, case_kind, "whole is the case");
    let ty = rf
        .resolution_at(nth(src, "Color", 1))
        .expect("Color type segment resolves");
    let ty_def = rf.resolved_def(ty).expect("type segment names a def");
    assert_eq!(ty_def.range, nth(src, "Color", 0), "Color → the type def");
    assert_eq!(ty_def.kind, DefKind::Type, "the segment is the type");
}

#[test]
fn same_file_module_qualified_union_case_resolves() {
    // Gap A clean case: `Pal.Color.Red` where `Pal.Color` is a same-file union type.
    let src = "module Top\nmodule Pal =\n    type Color = Red | Blue\nlet x = Pal.Color.Red\n";
    assert_same_file_module_qualified_case(src, DefKind::UnionCase);
}

#[test]
fn same_file_module_qualified_require_qualified_case_resolves() {
    let src = "module Top\nmodule Pal =\n    [<RequireQualifiedAccess>]\n    type Color = Red | Blue\nlet x = Pal.Color.Red\n";
    assert_same_file_module_qualified_case(src, DefKind::UnionCase);
}

#[test]
fn same_file_module_qualified_enum_case_resolves() {
    let src =
        "module Top\nmodule Pal =\n    type Color = Red = 0 | Blue = 1\nlet x = Pal.Color.Red\n";
    assert_same_file_module_qualified_case(src, DefKind::EnumCase);
}

#[test]
fn same_file_module_qualified_case_in_pattern_resolves() {
    // Pattern position resolves identically (FCS).
    let src = "module Top\nmodule Pal =\n    type Color = Red | Blue\nlet f c = match c with Pal.Color.Red -> 1 | _ -> 0\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("Pal.Color.Red pattern resolves");
    let case_def = rf.resolved_def(whole).expect("whole names a def");
    assert_eq!(case_def.range, nth(src, "Red", 0));
    assert_eq!(case_def.kind, DefKind::UnionCase);
}

#[test]
fn same_file_module_qualified_case_resolves_to_a_sibling_module() {
    // A *sibling* nested module's type qualifies (the self/ancestor guard rejects only
    // the current container / an ancestor as the head).
    let src = "namespace Lib\nmodule Pal =\n    type Color = Red | Blue\nmodule Use =\n    let x = Pal.Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("sibling Pal.Color.Red resolves");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0));
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn anonymous_root_module_qualified_case_resolves() {
    // A header-less file's own nested module resolves: the per-container name view
    // includes anonymous-root nested modules (unlike the cross-file module indices).
    let src = "module Pal =\n    type Color = Red | Blue\nlet x = Pal.Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("anonymous-root Pal.Color.Red resolves");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0));
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_prefers_enclosing_over_an_open() {
    // FCS: the enclosing container's nested `Pal` beats an `open`-supplied `A.Pal`.
    // The head resolves through the lexical container chain only (no opens tier), so
    // `Pal.Color.Red` in `Client.Use` → `Client.Pal`'s case, never `A.Pal`'s.
    let src = "namespace A\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Pal =\n    type Color = Red | Blue\nmodule Use =\n    let x = Pal.Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("Pal.Color.Red resolves");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "Red", 1),
        "→ enclosing Client.Pal's Red, not A.Pal's"
    );
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_forward_reference_defers() {
    // F# is order-sensitive: a bare head referencing a module defined *later* in the
    // same container is FS0039 (FCS reports nothing). `container_decls` is populated
    // during the walk, so the later `module Pal` is not yet visible at the reference —
    // we must not emit its case.
    let src = "module Top\nlet x = Pal.Color.Red\nmodule Pal =\n    type Color = Red | Blue\n";
    let rf = resolve(src);
    assert!(
        rf.resolution_at(nth(src, "Pal.Color.Red", 0)).is_none(),
        "forward reference to a later module must defer (FS0039)"
    );
}

#[test]
fn module_qualified_case_resolves_a_headerless_root_sibling_from_a_nested_module() {
    // Soundness (codex): in a *headerless* file the implicit anonymous root lexically
    // contains sibling modules, so from a nested `module Outer` a sibling root
    // `module Pal`'s case IS in scope. With an earlier file also exporting
    // `Pal.Color.Red`, FCS binds the *same-file* sibling — so the root tier (`k == 0`)
    // must be searched whenever the file is headerless (`namespace_depth == 0`), even
    // when `container_path` is non-empty.
    let src0 = "namespace Lib\nmodule Pal =\n    type Color = Red | Blue\n";
    let src1 =
        "module Pal =\n    type Color = Red | Blue\nmodule Outer =\n    let x = Pal.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Pal.Color.Red", 0))
        .expect("Pal.Color.Red resolves");
    // A same-file sibling resolves to file1's own interned case (a `Local`), so check
    // via file1's resolver — not `item_def`, which is for *cross-file* items. The
    // earlier file (file0) would only ever surface as such a cross-file item.
    assert!(
        proj.item_def(res).is_none(),
        "did not fall through to a cross-file item (file0): {res:?}"
    );
    let def = proj
        .file(1)
        .resolved_def(res)
        .expect("resolves to a same-file def");
    assert_eq!(def.range, nth(src1, "Red", 0), "→ file1's own Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_root_head_does_not_outrank_an_open() {
    // FCS: with a root `module Pal` (namespace global), an `A.Pal`, and
    // `namespace Client; open A; Pal.Color.Red`, the later open outranks the
    // root and `A.Pal` owns the residual — FCS binds `A.Pal.Color.Red`. With
    // opens as full candidates, sema emits exactly that, never the root's case.
    let src = "namespace global\nmodule Pal =\n    type Color = Red | Blue\nnamespace A\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("resolves through the open");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "Red", 1),
        "→ A.Pal's Red case, not the root's"
    );
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_ignores_a_local_active_pattern_in_the_module() {
    // A *local* `let (|Color|_|)` inside a function in `module Pal` is not a member of
    // `Pal` — `Pal.Color` cannot reach it — so it must not register as contention. A
    // `match … with Pal.Color.Red` still resolves the union case (FCS), not defers.
    let src = "module Top\nmodule Pal =\n    let f x =\n        let (|Color|_|) y = if y then Some () else None\n        match x with\n        | Color -> 1\n        | _ -> 0\n    type Color = Red | Blue\nlet g c = match c with Pal.Color.Red -> 1 | _ -> 0\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("Pal.Color.Red resolves past the local active pattern");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(
        def.kind,
        DefKind::UnionCase,
        "→ the union case, not deferred"
    );
}

#[test]
fn module_qualified_case_defers_through_an_ancestor_namespace() {
    // FS0039: a bare head is not searched in an *ancestor* namespace. From
    // `namespace N.Sub`, `Pal.Color.Red` does NOT resolve to `N.Pal` (the ancestor
    // namespace `N`'s module) — FCS reports nothing. The head walk must honour
    // `namespace_depth` and skip ancestor-namespace prefixes.
    let src = "namespace N\nmodule Pal =\n    type Color = Red | Blue\nnamespace N.Sub\nmodule Use =\n    let x = Pal.Color.Red\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Pal.Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("ancestor-namespace `Pal.Color.Red` should defer, got {other:?}"),
    }
}

#[test]
fn same_file_module_qualified_case_defers_through_the_containers_own_name() {
    // FS0039: a module's own name is not in scope as the head within it, so
    // `Outer.Color.Red` inside `module Outer` does not resolve (self/ancestor guard).
    let src = "module Outer\ntype Color = Red | Blue\nlet x = Outer.Color.Red\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Outer.Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("self-named `Outer.Color.Red` should defer, got {other:?}"),
    }
}

#[test]
fn module_qualified_case_defers_when_a_value_shares_the_name() {
    // A `let Color` next to `type Color` in `Pal` is contention ({Type, Value}); FCS
    // reads `Pal.Color` as the value (member access), so we defer. Both orders; and the
    // anonymous-root form, whose value has no qualified path (caught by the name view).
    for src in [
        "module Top\nmodule Pal =\n    let Color = 0\n    type Color = Red | Blue\nlet x = Pal.Color.Red\n",
        "module Top\nmodule Pal =\n    type Color = Red | Blue\n    let Color = 0\nlet x = Pal.Color.Red\n",
        "module Pal =\n    let Color = 0\n    type Color = Red | Blue\nlet x = Pal.Color.Red\n",
    ] {
        let rf = resolve(src);
        match rf.resolution_at(nth(src, "Pal.Color.Red", 0)) {
            None | Some(Resolution::Deferred(_)) => {}
            other => {
                panic!("value-sharing `Pal.Color.Red` should defer, got {other:?} for {src:?}")
            }
        }
    }
}

#[test]
fn module_qualified_case_defers_when_an_exception_shares_the_name() {
    // An `exception Color` is a dottable value at the segment; with a `type Color`
    // too it is contention. (This source is in fact FS0037-illegal — an exception
    // is a tycon, so it collides with `type Color` — so there is no FCS pin either
    // way; on illegal input deferring is trivially sound.)
    let src = "module Top\nmodule Pal =\n    exception Color of int\n    type Color = Red | Blue\nlet x = Pal.Color.Red\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Pal.Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("exception-sharing `Pal.Color.Red` should defer, got {other:?}"),
    }
}

#[test]
fn module_qualified_case_defers_when_another_types_case_shares_the_name() {
    // A union case `Color` of a *different* type, plus a `type Color`, is contention
    // ({Type, UnionCase}). FCS-verified: `Pal.Color.Red` → `Top.Pal.Other.Color` (the
    // case constructor of `Other`, member access), NOT the type's case. Defer.
    let src = "module Top\nmodule Pal =\n    type Other = Color | X\n    type Color = Red | Blue\nlet z = Pal.Color.Red\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Pal.Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("case-name-sharing `Pal.Color.Red` should defer, got {other:?}"),
    }
}

#[test]
fn module_qualified_case_defers_past_an_active_pattern_in_pattern_position() {
    // Position-specific (codex): in *pattern* position an active pattern participates
    // in the constructor namespace, so `match c with Pal.Color.Red` — where `Pal` has
    // both `(|Color|_|)` and `type Color` — does NOT resolve to the union case (FCS
    // reports nothing). Unlike the expression form, the pattern path treats an
    // active-pattern segment as contention and defers.
    let src = "module Top\nmodule Pal =\n    let (|Color|_|) x = if x then Some () else None\n    type Color = Red | Blue\nlet f c = match c with Pal.Color.Red -> 1 | _ -> 0\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Pal.Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("active-pattern `Pal.Color.Red` pattern should defer, got {other:?}"),
    }
}

#[test]
fn module_qualified_case_resolves_past_an_active_pattern_sharing_the_name() {
    // An active pattern `(|Color|_|)` alongside `type Color` is not a *dottable value*
    // (it is not a value in expression position), so it does not contend for the
    // segment: FCS resolves `Pal.Color.Red` to the type's case (`Top.Pal.Color.Red`),
    // and we emit it.
    let src = "module Top\nmodule Pal =\n    let (|Color|_|) x = if x then Some () else None\n    type Color = Red | Blue\nlet z = Pal.Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("Pal.Color.Red resolves past the active pattern");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0), "→ the type's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_never_resolves_to_a_case_shadowed_by_a_submodule() {
    // The head `Pal` resolves to the inner `Outer.Pal`, whose `Color` is a *submodule*
    // ({Module}) — so the Gap A branch must NOT emit the outer `Top.Pal.Color`'s case.
    // FCS reads `Pal.Color.Red` as the submodule's value `Outer.Pal.Color.Red`, which
    // the qualified-value branch resolves; either way the soundness property is that we
    // never point at `Top.Pal.Color`'s `Red` case (the first `Red`).
    let src = "module Top\nmodule Pal =\n    type Color = Red | Blue\nmodule Outer =\n    module Pal =\n        module Color =\n            let Red = 0\n    let z = Pal.Color.Red\n";
    let rf = resolve(src);
    if let Some(res) = rf.resolution_at(nth(src, "Pal.Color.Red", 0))
        && let Some(def) = rf.resolved_def(res)
    {
        assert_ne!(
            def.range,
            nth(src, "Red", 0),
            "must not point at the shadowed Top.Pal.Color case"
        );
    }
}

#[test]
fn module_qualified_case_resolves_outward_when_the_innermost_module_lacks_the_type() {
    // FCS (probes R1bexpr/R1bpat): a lexically inner `module Pal` without `Color`
    // does NOT end the search — F# tries each same-named module candidate outward
    // and binds the root same-file `Pal`'s case. The head walk must continue past
    // a candidate whose residual misses, not stop-and-miss (r14).
    let src = "module Pal =\n    type Color = Red | Blue\nmodule Outer =\n    module Pal =\n        type Other = X | Y\n    let z = Pal.Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("resolves outward to the root Pal's case");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0), "→ the root Pal's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_never_emits_an_outer_module_shadowed_by_an_inner_alias() {
    // An inner `module P = Target` alias shadows an enclosing real `module P`. FCS
    // resolves `P.Color.Red` through the alias to `Top.Target.Color.Red`, NOT the
    // outer P's case. We don't resolve same-file aliases (a later stage), so this
    // defers — but it must never emit the *outer* module P's case (a wrong target).
    let src = "module Top\nmodule Target =\n    type Color = Red | Blue\nmodule P =\n    type Color = Red | Blue\nmodule Use =\n    module P = Target\n    let x = P.Color.Red\n";
    let rf = resolve(src);
    if let Some(res) = rf.resolution_at(nth(src, "P.Color.Red", 0))
        && let Some(def) = rf.resolved_def(res)
    {
        assert_ne!(
            def.range,
            nth(src, "Red", 1),
            "must not emit the outer module P's case (shadowed by the inner alias)"
        );
    }
}

// ---- cross-file ----

#[test]
fn cross_file_union_type_qualified_case_resolves_fully_qualified() {
    assert_cross_file_case(
        "namespace Lib\ntype Color = Red | Blue\n",
        "module Client\nlet x = Lib.Color.Red\n",
        "Lib.Color.Red",
    );
}

#[test]
fn cross_file_union_type_qualified_case_resolves_through_open() {
    assert_cross_file_case(
        "namespace Lib\ntype Color = Red | Blue\n",
        "module Client\nopen Lib\nlet x = Color.Red\n",
        "Color.Red",
    );
}

#[test]
fn cross_file_require_qualified_union_type_qualified_case_resolves_through_open() {
    assert_cross_file_case(
        "namespace Lib\n[<RequireQualifiedAccess>]\ntype Color = Red | Blue\n",
        "module Client\nopen Lib\nlet x = Color.Red\n",
        "Color.Red",
    );
}

#[test]
fn cross_file_enum_type_qualified_case_resolves_through_open() {
    assert_cross_file_case(
        "namespace Lib\ntype Color = Red = 0 | Blue = 1\n",
        "module Client\nopen Lib\nlet x = Color.Red\n",
        "Color.Red",
    );
}

#[test]
fn cross_file_type_qualified_case_defers_through_a_module_open() {
    // `open Lib.Pal` opens a *module*, which could carry unmodelled submodules — and
    // a later module open *can* shadow the type qualifier: FCS resolves
    // `open A (type Color); open B (module Color); Color.Red` to `B.Color.Red` (the
    // later submodule member), not A's case. So while any project-module open is in
    // scope we conservatively defer a dotted head (matching
    // `dotted_head_through_open_module_defers`). Sound — never the wrong case; an
    // availability gap. The fully-qualified form below resolves.
    let proj = resolve_project(
        &[
            impl_file("namespace Lib\nmodule Pal =\n    type Color = Red | Blue\n"),
            impl_file("module Client\nopen Lib.Pal\nlet x = Color.Red\n"),
        ],
        &AssemblyEnv::default(),
    );
    let src1 = "module Client\nopen Lib.Pal\nlet x = Color.Red\n";
    match proj.file(1).resolution_at(nth(src1, "Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("`Color.Red` under a module open should defer, got {other:?}"),
    }
}

#[test]
fn cross_file_type_qualified_case_in_a_nested_module_resolves_fully_qualified() {
    assert_cross_file_case(
        "namespace Lib\nmodule Pal =\n    type Color = Red | Blue\n",
        "module Client\nlet x = Lib.Pal.Color.Red\n",
        "Lib.Pal.Color.Red",
    );
}

#[test]
fn cross_file_type_qualified_case_relative_under_the_same_namespace() {
    assert_cross_file_case(
        "namespace Lib\ntype Color = Red | Blue\n",
        "namespace Lib\nmodule N =\n    let x = Color.Red\n",
        "Color.Red",
    );
}

// ---- cross-file, pattern position (the primary use of RQA/enum cases) ----

#[test]
fn cross_file_union_type_qualified_case_in_pattern_resolves_through_open() {
    assert_cross_file_case(
        "namespace Lib\ntype Color = Red | Blue\n",
        "module Client\nopen Lib\nlet f x = match x with Color.Red -> 1 | _ -> 0\n",
        "Color.Red",
    );
}

#[test]
fn cross_file_require_qualified_union_type_qualified_case_in_pattern_resolves() {
    assert_cross_file_case(
        "namespace Lib\n[<RequireQualifiedAccess>]\ntype Color = Red | Blue\n",
        "module Client\nopen Lib\nlet f x = match x with Color.Red -> 1 | _ -> 0\n",
        "Color.Red",
    );
}

#[test]
fn cross_file_type_qualified_case_in_pattern_resolves_fully_qualified() {
    assert_cross_file_case(
        "namespace Lib\ntype Color = Red | Blue\n",
        "module Client\nlet f x = match x with Lib.Color.Red -> 1 | _ -> 0\n",
        "Lib.Color.Red",
    );
}

// ---- review fixes ----

#[test]
fn qualified_union_case_keeps_one_identity_with_bare_use() {
    // `Color.Red` must resolve to the *same* resolution as the bare `Red` (and the
    // declaration) for a non-RQA union in a named module — one symbol for
    // find-references / rename, not a `Local`/`Item` split (the case is an `Item`).
    let src = "module M\ntype Color = Red | Blue\nlet a = Red\nlet b = Color.Red\n";
    let rf = resolve(src);
    let bare = rf
        .resolution_at(nth(src, "Red", 1))
        .expect("bare Red resolves");
    let qualified = rf
        .resolution_at(nth(src, "Color.Red", 0))
        .expect("Color.Red resolves");
    assert!(
        matches!(bare, Resolution::Item(_)),
        "bare Red is an Item, got {bare:?}"
    );
    assert_eq!(
        bare, qualified,
        "Color.Red must be the same resolution as bare Red"
    );
}

#[test]
fn same_file_module_qualified_case_wins_over_an_earlier_files_value_path() {
    // The same-file relative head shadows an earlier file's *root* module of the same
    // name: file0 exports a value at the exact written path `Pal.Color.Red`, but
    // file1's `module Pal = type Color = …` makes `Pal.Color.Red` resolve to the
    // **same-file** case (FCS: `Client.Pal.Color.Red`). So the same-file branch must
    // run before the cross-file qualified-value / exact-export branches.
    let src0 = "module Pal.Color\nlet Red = 1\n";
    let src1 = "module Client\nmodule Pal =\n    type Color = Red | Blue\nlet x = Pal.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Pal.Color.Red", 0))
        .expect("Pal.Color.Red resolves");
    let (file_idx, def) = proj.item_def(res).expect("resolves to an item");
    assert_eq!(file_idx, 1, "same-file (file1) case, not file0's value");
    assert_eq!(def.range, nth(src1, "Red", 0), "→ file1's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_resolves_when_head_module_is_contended_with_a_case() {
    // A union case `Pal` (of another type) alongside `module Pal`: FCS resolves
    // `Pal.Color.Red` through the *module* (the module wins over a co-named case for a
    // dotted head), to the same-file case — never an earlier file's value. Only a
    // dottable value / alias blocks a module head, not a case constructor.
    let src0 = "module Pal.Color\nlet Red = 1\n";
    let src1 = "module Client\ntype T = Pal | X\nmodule Pal =\n    type Color = Red | Blue\nlet x = Pal.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Pal.Color.Red", 0))
        .expect("Pal.Color.Red resolves");
    let (file_idx, def) = proj.item_def(res).expect("resolves to an item");
    assert_eq!(file_idx, 1, "same-file (file1) case, not file0's value");
    assert_eq!(def.range, nth(src1, "Red", 0), "→ file1's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_resolves_when_head_module_is_contended_with_a_type() {
    // A same-file `module Pal` plus a `type Pal` in the same container: FCS resolves
    // `Pal.Color.Red` through the *module* (the module wins for a dotted head) to the
    // same-file case, never an earlier file's `Pal.Color.Red`. A same-file module
    // claims the head even when contended with a type.
    let src0 = "module Pal.Color\nlet Red = 1\n";
    let src1 = "module Client\nmodule Pal =\n    type Color = Red | Blue\ntype Pal = int\nlet x = Pal.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Pal.Color.Red", 0))
        .expect("Pal.Color.Red resolves");
    let (file_idx, def) = proj.item_def(res).expect("resolves to an item");
    assert_eq!(file_idx, 1, "same-file (file1) case, not file0's value");
    assert_eq!(def.range, nth(src1, "Red", 0), "→ file1's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_contended_segment_does_not_navigate_to_an_earlier_file() {
    // The segment `Color` is both a submodule and a type inside the same-file `Pal`
    // (contended). Whatever it resolves to is same-file rooted — it must never
    // navigate to an earlier file's `Pal.Color.Red`. (FCS picks the type's case
    // here — the Emit path, pinned by `module_qualified_case_emit_beats_a_companion_module_value`.)
    let src0 = "module Pal.Color\nlet Red = 1\n";
    let src1 = "module Client\nmodule Pal =\n    module Color =\n        let Other = 1\n    type Color = Red | Blue\nlet x = Pal.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    if let Some(res) = proj.file(1).resolution_at(nth(src1, "Pal.Color.Red", 0))
        && let Some((file_idx, _)) = proj.item_def(res)
    {
        assert_ne!(file_idx, 0, "contended segment must not navigate to file0");
    }
}

#[test]
fn module_qualified_contention_does_not_navigate_to_an_earlier_file() {
    // Soundness (codex [P1]): when the head `Pal` binds to a same-file nested module
    // but the segment is contended (here `let Color` + `type Color`), the reference is
    // same-file-rooted — FCS reads `Pal.Color` as the same-file value (member access),
    // NOT a navigation to file0's `Pal.Color.Red`. So the same-file branch must stop
    // resolution on contention, not fall through to the cross-file value branch.
    let src0 = "module Pal.Color\nlet Red = 1\n";
    let src1 = "module Client\nmodule Pal =\n    let Color = 0\n    type Color = Red | Blue\nlet x = Pal.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    match proj.file(1).resolution_at(nth(src1, "Pal.Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "contended same-file `Pal.Color.Red` must not navigate cross-file, got {other:?}"
        ),
    }
}

#[test]
fn anonymous_root_module_qualified_case_wins_over_an_earlier_files_value() {
    // The same, anonymous-root: file1's nested-module case (a `Resolution::Local`, no
    // cross-file handle) must shadow file0's same-path value — never file0's `Item`.
    let src0 = "module Pal.Color\nlet Red = 1\n";
    let src1 = "module Pal =\n    type Color = Red | Blue\nlet x = Pal.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Pal.Color.Red", 0))
        .expect("anonymous-root same-file case should resolve");
    assert!(
        proj.item_def(res).is_none(),
        "must not resolve to a cross-file item (file0's value), got {res:?}"
    );
    let def = proj
        .file(1)
        .resolved_def(res)
        .expect("file-local def in file1");
    assert_eq!(def.range, nth(src1, "Red", 0), "→ file1's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_own_module_head_resolves_outward_to_an_earlier_file() {
    // Inside `module Top.Pal`, the head `Pal` is the module's own name — not in scope
    // (FS0039) — so `Pal.Color.Red` resolves *outward* to an earlier file's root
    // `module Pal` (FCS: file0's `Pal.Color.Red`). The same-file branch must `Miss`
    // (not stop) on a self/ancestor head, so the cross-file path runs.
    assert_cross_file_case(
        "module Pal\ntype Color = Red | Blue\n",
        "module Top\nmodule Pal =\n    let x = Pal.Color.Red\n",
        "Pal.Color.Red",
    );
}

#[test]
fn module_qualified_case_absent_segment_resolves_outward_to_an_earlier_file() {
    // A same-file `module Pal` that does **not** declare `Color`: F# searches outward
    // and resolves `Pal.Color.Red` to an earlier file's root `module Pal` (FCS:
    // file0's `Pal.Color.Red`). An absent segment must `Miss` (fall through to the
    // cross-file path), not `DeferStop`.
    assert_cross_file_case(
        "module Pal\ntype Color = Red | Blue\n",
        "module Client\nmodule Pal =\n    type Other = X | Y\nlet x = Pal.Color.Red\n",
        "Pal.Color.Red",
    );
}

#[test]
fn cross_file_module_qualified_case_resolves_despite_a_local_type_of_the_head_name() {
    // A local `type Pal` does NOT shadow a cross-file module `Pal` for
    // `Pal.Color.Red` (a type name is not a value/member-access head) — FCS resolves
    // through the cross-file module's case `Lib.Pal.Color.Red`. The same-file
    // classifier must `Miss` (not stop) for a non-module head, so `cross_file_type_case`
    // still runs.
    assert_cross_file_case(
        "namespace Lib\nmodule Pal =\n    type Color = Red | Blue\n",
        "module Client\nopen Lib\ntype Pal = int\nlet x = Pal.Color.Red\n",
        "Pal.Color.Red",
    );
}

#[test]
fn cross_file_module_qualified_case_resolves_despite_a_non_rec_binder_of_the_head_name() {
    // `let Pal = Pal.Color.Red` (non-`rec` binder named `Pal`): the binder is not in
    // scope in its own RHS, so `Pal.Color.Red` resolves cross-file to `Lib.Pal`'s case
    // (FCS). The eagerly-interned binder must not make the name view treat `Pal` as a
    // same-file value and stop resolution (the name view marks values at scope-entry
    // time, after the RHS).
    assert_cross_file_case(
        "namespace Lib\nmodule Pal =\n    type Color = Red | Blue\n",
        "module Client\nopen Lib\nlet Pal = Pal.Color.Red\n",
        "Pal.Color.Red",
    );
}

#[test]
fn cross_file_type_qualified_case_resolves_for_a_non_rec_self_reference() {
    // Gap B (`docs/type-qualified-case-prefix-plan.md`): a non-`rec` binding whose
    // name equals the type may reference an earlier file's case in its own RHS — the
    // binder is **not** in scope in its own RHS, so the eagerly-interned same-file
    // `let Color` must not shadow the qualifier. FCS-verified: `Lib.Container.Color.Red`
    // → file0's case (the binding's own value does not count, via `pending_items`).
    assert_cross_file_case(
        "namespace Lib\nmodule Container =\n    type Color = Red | Blue\n",
        "namespace Lib\nmodule Container =\n    let Color = Lib.Container.Color.Red\n",
        "Lib.Container.Color.Red",
    );
}

#[test]
fn cross_file_type_qualified_case_defers_when_a_value_shadows_the_type() {
    // A later same-module `let Color` makes `Color` a value; F# then reads
    // `Lib.Container.Color.Red` as member access on that value (FCS: `Color` →
    // `Lib.Container.Color`, the value), not the case. We must defer — never return
    // the stale case from the type-qualified index.
    let proj = resolve_project(
        &[
            impl_file("namespace Lib\nmodule Container =\n    type Color = Red | Blue\n"),
            impl_file("namespace Lib\nmodule Container =\n    let Color = 0\n"),
            impl_file("module Client\nlet x = Lib.Container.Color.Red\n"),
        ],
        &AssemblyEnv::default(),
    );
    let src2 = "module Client\nlet x = Lib.Container.Color.Red\n";
    match proj
        .file(2)
        .resolution_at(nth(src2, "Lib.Container.Color.Red", 0))
    {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("value-prefixed `Lib.Container.Color.Red` should defer, got {other:?}"),
    }
}

#[test]
fn cross_file_type_qualified_case_defers_when_any_value_shares_the_path() {
    // This fixture is FS0248-ILLEGAL (a same-named `module Container` in two
    // files) — the only way a value and a type reach the same qualified path from
    // *different* declarations. On illegal input deferring is trivially sound;
    // the legal (same-block) shapes are pinned by
    // `cross_file_type_qualified_case_defers_when_a_same_block_value_shares_the_path`.
    // (An earlier comment here claimed FCS resolves this shape to the case — a pin
    // taken before probes were dotnet-build-checked; `uses-project` tolerates type
    // errors, and no legal cross-block shape exists.)
    let src0 = "namespace Lib\nmodule Container =\n    let Color = 0\n";
    let src1 = "namespace Lib\nmodule Container =\n    type Color = Red | Blue\n";
    let src2 = "module Client\nlet x = Lib.Container.Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    match proj
        .file(2)
        .resolution_at(nth(src2, "Lib.Container.Color.Red", 0))
    {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("value-sharing `Lib.Container.Color.Red` should defer, got {other:?}"),
    }
}

#[test]
fn cross_file_type_qualified_case_defers_when_a_same_file_value_shadows_an_earlier_case() {
    // A value at `Lib.Container.Color` declared in the *same file* as the reference
    // shadows an *earlier* file's `Lib.Container.Color.Red` case (it is later in
    // Compile order). FCS reads `Container.Color.Red` as member access on the value;
    // we defer. (Round-8 regression: the value-shadow check only consulted earlier
    // files, missing same-file values.)
    let src0 = "namespace Lib\nmodule Container =\n    type Color = Red | Blue\n";
    let src1 = "namespace Lib\nmodule Container =\n    let Color = 0\nmodule Client =\n    let x = Container.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    match proj
        .file(1)
        .resolution_at(nth(src1, "Container.Color.Red", 0))
    {
        None | Some(Resolution::Deferred(_)) => {}
        other => {
            panic!("same-file value-shadowed `Container.Color.Red` should defer, got {other:?}")
        }
    }
}

/// Assert the whole `whole` span in `src1` resolves to file0's case named `case`.
fn assert_cross_file_named_case(src0: &str, src1: &str, whole: &str, case: &str) {
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, whole, 0))
        .unwrap_or_else(|| panic!("no resolution at {whole:?}"));
    let (file_idx, def) = proj
        .item_def(res)
        .unwrap_or_else(|| panic!("{whole:?} is not a cross-file item: {res:?}"));
    assert_eq!(file_idx, 0);
    assert_eq!(def.range, nth(src0, case, 0), "{whole:?} → file0's {case}");
}

#[test]
fn same_file_applied_qualified_case_pattern_resolves() {
    // An *applied* qualified case pattern `Shape.Circle r` (payload case) resolves
    // the head `Shape.Circle` to the case — the nullary-only gate used to drop it.
    let src = "module M\n[<RequireQualifiedAccess>]\ntype Shape = Circle of int | Square\nlet f s = match s with Shape.Circle r -> r | _ -> 0\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Shape.Circle", 0))
        .expect("Shape.Circle pattern resolves");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Circle", 0));
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn cross_file_applied_qualified_case_pattern_resolves_through_open() {
    assert_cross_file_named_case(
        "namespace Lib\n[<RequireQualifiedAccess>]\ntype Shape = Circle of int | Square\n",
        "module Client\nopen Lib\nlet f s = match s with Shape.Circle r -> r | _ -> 0\n",
        "Shape.Circle",
        "Circle",
    );
}

#[test]
fn cross_file_applied_qualified_case_expression_resolves_through_open() {
    // The construction form `Shape.Circle 5` (expression) resolves cross-file too.
    assert_cross_file_named_case(
        "namespace Lib\n[<RequireQualifiedAccess>]\ntype Shape = Circle of int | Square\n",
        "module Client\nopen Lib\nlet s = Shape.Circle 5\n",
        "Shape.Circle",
        "Circle",
    );
}

// ---- Gap A head walk: which declarations own / hide a dotted module head ----
//
// FCS-pinned (uses-project over each two-file pair; every source below verified
// FS-error-free with `dotnet build` — `uses-project` silently tolerates type
// errors, and an erroring probe pins nothing): a dotted head `Pal` in
// `Pal.Color.Red` is owned by the *module namespace* (nested modules and module
// abbreviations). A type, union-case constructor, active pattern, or exception
// constructor named `Pal` — co-declared with the module or declared in a nearer
// container — never hides the module, in either expression or pattern position.
// The one position split: a let-bound *value* `Pal` commits member access in
// expression position (FCS binds the value and `.Color` fails on it), but is
// invisible to a dotted head in pattern position (values are not pattern
// constructors). `exception Pal` + `module Pal` in one container is FS0037
// (duplicate definition), so an exception can never legally contend with a
// module head; from a different container it does not hide it.

/// Assert that file1's `Pal.Color.Red` (the first occurrence) resolves to
/// file1's **own** case at `nth(src1, "Red", red_idx)` — never into file0.
fn assert_resolves_to_own_case(src0: &str, src1: &str, red_idx: usize) {
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Pal.Color.Red", 0))
        .unwrap_or_else(|| panic!("Pal.Color.Red should resolve same-file for {src1:?}"));
    let (file_idx, def) = proj.item_def(res).expect("resolves to an item");
    assert_eq!(file_idx, 1, "same-file (file1) case, not file0's: {src1:?}");
    assert_eq!(def.range, nth(src1, "Red", red_idx), "→ file1's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_pattern_resolves_past_a_value_sharing_the_head_name() {
    // FCS (probe Cpat): in *pattern* position a co-declared `let Pal = 0` does NOT
    // shadow `module Pal` for the head — the pattern resolves to the same-file
    // module's case. (Expression position differs: FCS binds the value and the
    // path is member access — probe Cexpr.)
    let src = "module Client\nlet Pal = 0\nmodule Pal =\n    type Color = Red | Blue\nlet f c = match c with Pal.Color.Red -> 1 | _ -> 0\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("pattern Pal.Color.Red resolves past the value head");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0), "→ the module's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_pattern_value_head_does_not_navigate_to_an_earlier_file() {
    // The cross-file bait variant of the above: file0 exports the exact written
    // path, but FCS still resolves file1's own case. Falling through to the
    // cross-file index here was a wrong go-to-definition target.
    assert_resolves_to_own_case(
        "module Pal\ntype Color = Red | Blue\n",
        "module Client\nlet Pal = 0\nmodule Pal =\n    type Color = Red | Blue\nlet f c = match c with Pal.Color.Red -> 1 | _ -> 0\n",
        0,
    );
}

#[test]
fn module_qualified_case_pattern_head_walk_skips_a_value_only_container() {
    // FCS (probe Epat): a `let Pal = 0` in the use's own container does not stop
    // the pattern head from binding the *outer* same-file `module Pal` — the walk
    // must skip value-only containers in pattern position, not stop-and-miss
    // (which navigated to file0's export instead).
    assert_resolves_to_own_case(
        "module Pal\ntype Color = Red | Blue\n",
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Inner =\n    let Pal = 0\n    let f c = match c with Pal.Color.Red -> 1 | _ -> 0\n",
        0,
    );
}

#[test]
fn module_qualified_case_head_walk_skips_an_exception_only_container() {
    // FCS (probes E2pat / E2expr): an `exception Pal` in a nearer container does
    // not hide an outer same-file `module Pal` — in either position. (Co-declared
    // exception + module is FS0037, so "exception contends with the module head"
    // cannot arise in legal code; and probes Gpat/Gexpr show an exception-only
    // head does not commit a dotted path even when no module is in scope.)
    for src1 in [
        // pattern position
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Inner =\n    exception Pal\n    let f c = match c with Pal.Color.Red -> 1 | _ -> 0\n",
        // expression position
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Inner =\n    exception Pal\n    let x = Pal.Color.Red\n",
    ] {
        assert_resolves_to_own_case("module Pal\ntype Color = Red | Blue\n", src1, 0);
    }
}

#[test]
fn module_qualified_case_head_walk_skips_constructor_and_type_containers() {
    // FCS (probes Hpat/Hexpr, Kpat/Kexpr, Ppat/Pexpr): a union-case constructor,
    // a type, or an active pattern named `Pal` in a nearer container never hides
    // an outer same-file `module Pal` for a dotted head — in either position.
    for src1 in [
        // union-case constructor, pattern / expression
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Inner =\n    type T = Pal | Q\n    let f c = match c with Pal.Color.Red -> 1 | _ -> 0\n",
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Inner =\n    type T = Pal | Q\n    let x = Pal.Color.Red\n",
        // type, pattern / expression
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Inner =\n    type Pal = int\n    let f c = match c with Pal.Color.Red -> 1 | _ -> 0\n",
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Inner =\n    type Pal = int\n    let x = Pal.Color.Red\n",
        // active pattern, pattern / expression
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Inner =\n    let (|Pal|_|) (x: int) = if x = 0 then Some () else None\n    let f c = match c with Pal.Color.Red -> 1 | _ -> 0\n",
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Inner =\n    let (|Pal|_|) (x: int) = if x = 0 then Some () else None\n    let x = Pal.Color.Red\n",
    ] {
        assert_resolves_to_own_case("module Pal\ntype Color = Red | Blue\n", src1, 0);
    }
}

// ---- Gap A segment: a same-file type without the modeled case ----

#[test]
fn module_qualified_member_segment_does_not_navigate_to_an_earlier_file() {
    // FCS (probe Aexpr): `Pal.Color` is a same-file *object* type whose `Red` is a
    // static member — FCS resolves the member, same-file. The member index
    // (`resolve_type_members.rs`, probes M1/M2a) now emits exactly that — and
    // must never fall through to file0's same-written-path case.
    let src0 = "module Pal\ntype Color = Red | Blue\n";
    let src1 = "module Client\nmodule Pal =\n    type Color() =\n        static member Red = 42\nlet x = Pal.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Pal.Color.Red", 0))
        .expect("the same-file static member resolves");
    let def = proj
        .file(1)
        .resolved_def(res)
        .expect("a same-file def, never file0's case");
    assert_eq!(def.range, nth(src1, "Red", 0), "→ the type's static member");
}

#[test]
fn module_qualified_case_type_segment_without_case_defers_in_expression() {
    // FCS (probe Bexpr) resolves this *outward* to file0's case — the same-file
    // union `Color` lacks `Red` and carries no members. But sema cannot tell a
    // member-less type from one with an unmodeled static `Red` (probe Aexpr, where
    // outward navigation is a wrong target), so once a same-file type owns the
    // segment in expression position it defers — a deliberate, sound availability
    // sacrifice (documented in docs/type-qualified-case-prefix-plan.md).
    let src0 = "module Pal\ntype Color = Red | Blue\n";
    let src1 =
        "module Client\nmodule Pal =\n    type Color = Green | Purple\nlet x = Pal.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    match proj.file(1).resolution_at(nth(src1, "Pal.Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => {
            panic!("type-without-case segment must defer in expression position, got {other:?}")
        }
    }
}

#[test]
fn module_qualified_case_type_segment_without_case_searches_outward_in_pattern() {
    // FCS (probes Apat / Bpat): in *pattern* position a same-file type without the
    // case does NOT commit — a static member is not a pattern — so FCS searches
    // outward and binds file0's case. The classifier must keep falling through to
    // the cross-file index here (this pins the position split of the rule above).
    for src1 in [
        "module Client\nmodule Pal =\n    type Color = Green | Purple\nlet f c = match c with Pal.Color.Red -> 1 | _ -> 0\n",
        "module Client\nmodule Pal =\n    type Color() =\n        static member Red = 42\nlet f c = match c with Pal.Color.Red -> 1 | _ -> 0\n",
    ] {
        assert_cross_file_case(
            "module Pal\ntype Color = Red | Blue\n",
            src1,
            "Pal.Color.Red",
        );
    }
}

#[test]
fn module_qualified_case_pattern_defers_when_a_submodule_owns_the_case_name() {
    // FCS (probe Qpat): the same-file submodule `Pal.Color` declares a bare
    // union case `Red` (of its type `H`), and FCS resolves the pattern to that
    // same-file case — never file0's export at the same written path. We do not
    // resolve two-level module heads yet, so defer — but never navigate cross-file.
    let src0 = "module Pal\ntype Color = Red | Blue\n";
    let src1 = "module Client\nmodule Pal =\n    module Color =\n        type H = Red | Q\nlet f c = match c with Pal.Color.Red -> 1 | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    match proj.file(1).resolution_at(nth(src1, "Pal.Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("submodule-owned pattern segment must defer, got {other:?}"),
    }
}

#[test]
fn module_qualified_case_emit_beats_a_companion_module_value() {
    // FCS (probe Jexpr): when the same-file type has the case AND a companion
    // `module Color` declares a value `Red`, the *type's case* wins.
    let src = "module Client\nmodule Pal =\n    type Color = Red | Blue\n    module Color =\n        let Red = 0\nlet x = Pal.Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("Pal.Color.Red resolves to the type's case");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0), "→ the type's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_expression_segment_owned_by_a_type_defers_even_with_a_companion_value() {
    // FCS consults the *type* before the companion module's contents in expression
    // position: with `type Color() = static member Red` AND `module Color = let Red`,
    // FCS binds the type's static member (probe M2expr) — resolving the companion's
    // value would be a wrong target. The member index now emits exactly that
    // (`resolve_type_members.rs`, probe M2a). In the member-LESS shape (probe
    // Iexpr, where FCS does resolve the companion's value) sema still cannot
    // prove member absence (augmentations can add one later in the file), so it
    // keeps deferring: a documented availability sacrifice.
    let member_src = "module Client\nmodule Pal =\n    type Color() =\n        static member Red = 42\n    module Color =\n        let Red = 0\nlet x = Pal.Color.Red\n";
    let rf = resolve(member_src);
    let res = rf
        .resolution_at(nth(member_src, "Pal.Color.Red", 0))
        .expect("the type's static member resolves");
    let def = rf.resolved_def(res).expect("member def");
    assert_eq!(
        def.range,
        nth(member_src, "Red", 0),
        "→ the type's static member, never the companion's value"
    );

    let memberless_src = "module Client\nmodule Pal =\n    type Color = Green | Purple\n    module Color =\n        let Red = 0\nlet x = Pal.Color.Red\n";
    let rf = resolve(memberless_src);
    match rf.resolution_at(nth(memberless_src, "Pal.Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("member-less type-owned segment must still defer, got {other:?}"),
    }
}

// ---- Gap A head walk: multi-candidate outward search (codex r14, FCS-probed) ----

#[test]
fn module_qualified_case_head_walk_continues_past_a_module_lacking_the_residual() {
    // FCS (probes R1bexpr / R1bpat): when the nearer same-file `Outer.Pal` lacks
    // `Color`, F# tries the next same-named candidate outward and binds the outer
    // same-file `Client.Pal`'s case — never file0's export at the written path.
    for src1 in [
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Outer =\n    module Pal =\n        type Other = X | Y\n    let z = Pal.Color.Red\n",
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Outer =\n    module Pal =\n        type Other = X | Y\n    let f c = match c with Pal.Color.Red -> 1 | _ -> 0\n",
    ] {
        assert_resolves_to_own_case("module Pal\ntype Color = Red | Blue\n", src1, 0);
    }
}

#[test]
fn module_qualified_case_head_walk_continues_past_the_containers_own_name() {
    // FCS (probe R3bexpr): inside `module Top.Pal`, the self candidate is FS0039 —
    // and F# then tries the next candidate outward, binding the same-file
    // `Client.Pal`'s case (not file0's export). The self/ancestor guard must skip
    // to the next candidate, not abandon the same-file search.
    assert_resolves_to_own_case(
        "module Pal\ntype Color = Red | Blue\n",
        "module Client\nmodule Pal =\n    type Color = Red | Blue\nmodule Top =\n    module Pal =\n        let x = Pal.Color.Red\n",
        0,
    );
}

#[test]
fn namespace_global_root_module_does_not_outrank_an_open() {
    // FCS (probe NGexpr): `namespace global`'s root `module Pal` ranks below the
    // later `open A` positionally, and `A.Pal` is a same-file module owning the
    // residual — FCS binds `A.Pal.Color.Red` (never the root `Pal`'s case). With
    // opens as full candidates, sema now emits exactly that.
    let src = "namespace global\nmodule Pal =\n    type Color = Red | Blue\nnamespace A\nmodule Pal =\n    type Color = Red | Blue\nnamespace global\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("resolves through the open");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "Red", 1),
        "→ A.Pal's Red case, not the root's"
    );
    assert_eq!(def.kind, DefKind::UnionCase);
}

// ---- Gap A head walk: opens interleave with lexical candidates (codex r16) ----
//
// FCS-probed (OP1/OP2/OP3, all dotnet-build-clean): the head environment is ONE
// source-position-ordered latest-wins list over lexical module declarations AND
// `open` declarations. An open declared *after* a lexical `module Pal` outranks
// it (even from an enclosing namespace block); one declared *before* it loses
// (`module_qualified_case_prefers_enclosing_over_an_open`). Residual
// backtracking applies across both kinds.

#[test]
fn module_qualified_case_resolves_through_a_later_open_to_a_same_file_module() {
    // FCS (probes OP1/OP2/OP3): in each shape the `open A` outranks the same-file
    // lexical candidate (or is reached after its residual misses, OP2), and `A.Pal`
    // is a same-file module owning the residual — FCS binds `A.Pal.Color.Red`.
    // An open-supplied same-file module is a full candidate in the positional
    // latest-wins environment, classified with the same complete-information
    // machinery as a lexical candidate (previously these deferred).
    for src in [
        // OP1: candidate in an earlier `namespace Client` block, open in a later one.
        "namespace A\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n",
        // OP2: open then a residual-missing sibling module; backtracks to the open.
        "namespace A\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Pal =\n    type Other = X | Y\nmodule Use =\n    let z = Pal.Color.Red\n",
        // OP3 (codex r16): open inside a nested module, outer lexical candidate.
        "namespace A\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nmodule Outer =\n    open A\n    module Pal =\n        type Other = X | Y\n    let z = Pal.Color.Red\n",
    ] {
        let rf = resolve(src);
        let whole = rf
            .resolution_at(nth(src, "Pal.Color.Red", 0))
            .unwrap_or_else(|| panic!("open-supplied A.Pal should resolve for {src:?}"));
        let def = rf.resolved_def(whole).expect("names a def");
        assert_eq!(
            def.range,
            nth(src, "Red", 0),
            "→ A.Pal's Red case in {src:?}"
        );
        assert_eq!(def.kind, DefKind::UnionCase);
    }
}

#[test]
fn module_qualified_case_resolves_through_a_later_open_cross_file() {
    // The cross-file variant of OP1: `open A` (an earlier file's namespace) is
    // later in source than the same-file lexical `Client.Pal`, so FCS binds
    // file0's `A.Pal.Color.Red`. The classifier must Miss (contested by the
    // open) so the open-aware cross-file branch resolves it.
    let src0 = "namespace A\nmodule Pal =\n    type Color = Red | Blue\n";
    let src1 = "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Pal.Color.Red", 0))
        .expect("resolves through the open");
    let (file_idx, def) = proj.item_def(res).expect("resolves to a cross-file item");
    assert_eq!(file_idx, 0, "→ file0's A.Pal.Color.Red (the open outranks)");
    assert_eq!(def.range, nth(src0, "Red", 0));
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_resolves_past_a_later_open_lacking_the_residual() {
    // FCS (probe BK1): a later `open A` whose `A.Pal` exists but lacks `Color`
    // does NOT commit — FCS backtracks past it to the lexical `Client.Pal` and
    // binds its case. The open contest must be residual-aware: a same-file open
    // target is a complete declared-name view, so "nothing named `Color` there"
    // is provable and the candidate stands (codex r17).
    let src = "namespace A\nmodule Pal =\n    type Other = X | Y\nnamespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("resolves past the residual-less open to the lexical candidate");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0), "→ Client.Pal's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_resolves_past_an_assembly_open_lacking_the_residual() {
    // codex r17: an `open Demo` (a referenced-assembly namespace) that could
    // supply the head `Sub` (the assembly has `Demo.Sub`) must not suppress the
    // lexical `module Sub` when the assembly target has nothing named `Color` —
    // the assembly env is complete, so absence is provable and the same-file
    // case resolves (the residual-backtracking rule of probe BK1, applied to an
    // assembly target).
    let bytes = std::fs::read(ensure_assembly_fixture_built()).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");
    let src = "module Client\nmodule Sub =\n    type Color = Red | Blue\nopen Demo\nlet x = Sub.Color.Red\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let rf = resolve_file(
        &ImplFile::cast(parsed.root).expect("impl file"),
        &ProjectItems::default(),
        &env,
    );
    let whole = rf
        .resolution_at(nth(src, "Sub.Color.Red", 0))
        .expect("resolves past the assembly open to the lexical module");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "Red", 0),
        "→ the lexical Sub's Red case"
    );
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn namespace_global_root_module_resolves_when_no_open_contests() {
    // FCS (probes G1 / G3): a same-file `namespace global` root module IS the
    // binding when no later-positioned open could supply the head — from a real
    // namespace with no open at all (G1), and even in its own block when the
    // `open` precedes the module declaration (G3: positional latest-wins, the
    // same rule as everywhere else — the root is not a special always-below-opens
    // tier, r18). The root tier must be searched for real-root files too; the
    // positional open contest supplies the "later open outranks it" half
    // (pinned by `namespace_global_root_module_does_not_outrank_an_open`).
    // (Cross-file bait is impossible here: a root `module Pal` in two files is
    // FS0248, so emitting the same-file root is always FCS-faithful.)
    for (src, red_idx, pal_idx) in [
        // G1: use inside a real namespace, no open in scope.
        (
            "namespace global\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nmodule Use =\n    let x = Pal.Color.Red\n",
            0usize,
            0usize,
        ),
        // G3: the open precedes the root module declaration — the module wins.
        (
            "namespace A\nmodule Pal =\n    type Color = Red | Blue\nnamespace global\nopen A\nmodule Pal =\n    type Color = Red | Blue\nmodule Use =\n    let x = Pal.Color.Red\n",
            1usize,
            1usize,
        ),
    ] {
        let rf = resolve(src);
        let whole = rf
            .resolution_at(nth(src, "Pal.Color.Red", 0))
            .unwrap_or_else(|| panic!("namespace-global root Pal should resolve for {src:?}"));
        let def = rf.resolved_def(whole).expect("names a def");
        assert_eq!(
            def.range,
            nth(src, "Red", red_idx),
            "→ the same-file root Pal's Red case (Pal occurrence {pal_idx}) in {src:?}"
        );
        assert_eq!(def.kind, DefKind::UnionCase);
    }
}

#[test]
fn module_qualified_case_resolves_through_the_latest_of_two_opens() {
    // FCS (probe OO1): `open A; open B`, both with a same-file `Pal` owning the
    // residual — the LATEST open wins (`B.Pal.Color.Red`).
    let src = "namespace A\nmodule Pal =\n    type Color = Red | Blue\nnamespace B\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nopen B\nmodule Use =\n    let x = Pal.Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("resolves through the latest open");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 1), "→ B.Pal's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_pattern_resolves_through_an_open_to_a_same_file_module() {
    // FCS (probe OSpat): pattern position, `open A` supplying the same-file
    // `A.Pal` — the pattern binds its case.
    let src = "namespace A\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let f c = match c with Pal.Color.Red -> 1 | _ -> 0\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("pattern resolves through the open");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0), "→ A.Pal's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_value_head_masks_a_later_open_in_expression() {
    // FCS (probe VP1expr): an in-scope `let Pal` commits member access in
    // expression position even against a LATER `open A` whose `A.Pal` owns the
    // residual (FCS binds the value; `.Color` fails on it) — value heads are not
    // positional against opens. The case must never resolve here.
    let src = "namespace A\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nmodule Use =\n    let Pal = 0\n    open A\n    let x = Pal.Color.Red\n";
    let rf = resolve(src);
    if let Some(res) = rf.resolution_at(nth(src, "Pal.Color.Red", 0))
        && let Some(def) = rf.resolved_def(res)
    {
        assert_ne!(
            def.range,
            nth(src, "Red", 0),
            "the value head masks the later open; must not emit A.Pal's case"
        );
    }
}

#[test]
fn module_qualified_case_pattern_value_does_not_mask_a_later_open() {
    // FCS (probe VP1pat): in pattern position the `let Pal` is invisible, so the
    // later open's same-file case binds.
    let src = "namespace A\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nmodule Use =\n    let Pal = 0\n    open A\n    let f c = match c with Pal.Color.Red -> 1 | _ -> 0\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("pattern resolves through the open past the value");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0), "→ A.Pal's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn module_qualified_case_resolves_through_an_open_to_a_same_file_namespace() {
    // FCS (probe NS1): `open A` supplies the head `Pal` as the *namespace*
    // `A.Pal`, whose type `Color` carries the case — FCS binds `A.Pal.Color.Red`.
    // A namespace spans files, so it qualifies for the complete-information
    // treatment only when it exists in no earlier file and no referenced
    // assembly; here it is same-file-only, so the case emits.
    let src = "namespace A.Pal\ntype Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n";
    let rf = resolve(src);
    let whole = rf
        .resolution_at(nth(src, "Pal.Color.Red", 0))
        .expect("resolves through the open to the same-file namespace's case");
    let def = rf.resolved_def(whole).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0), "→ A.Pal's Red case");
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn cross_file_type_qualified_case_defers_when_a_same_block_value_shares_the_path() {
    // Gap C, closed by probe (PE1/PE2/PF1/PF2): a value and a type can share a
    // qualified path only when declared in ONE module block (cross-block shapes
    // are FS0248/FS0247), and there the VALUE commits — union AND enum, either
    // order. Notably the 2-segment rule "an enum case beats an *earlier* value"
    // does NOT carry to the qualified form: FCS binds `Lib.Container.Color` to
    // the value in all four variants (member access; `.Red` then only compiles
    // if the value's type has such a member), so the case must never resolve.
    let src1 = "module Client\nlet x = Lib.Container.Color.Red\n";
    for src0 in [
        "namespace Lib\nmodule Container =\n    let Color = 0\n    type Color = Red | Blue\n",
        "namespace Lib\nmodule Container =\n    type Color = Red | Blue\n    let Color = 0\n",
        "namespace Lib\nmodule Container =\n    let Color = 0\n    type Color = Red = 0 | Blue = 1\n",
        "namespace Lib\nmodule Container =\n    type Color = Red = 0 | Blue = 1\n    let Color = 0\n",
    ] {
        let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
        match proj
            .file(1)
            .resolution_at(nth(src1, "Lib.Container.Color.Red", 0))
        {
            None | Some(Resolution::Deferred(_)) => {}
            other => panic!(
                "same-block value must shadow the qualified case (got {other:?}) for {src0:?}"
            ),
        }
    }
}

#[test]
fn cross_file_type_qualified_case_pattern_defers_when_a_value_shares_the_path() {
    // Gap C, pattern position (probe PEpat): the same-block value blocks the
    // pattern too (FS1127 — a value is not a pattern constructor); FCS binds no
    // case, so emitting file0's case would be a wrong target.
    let src0 =
        "namespace Lib\nmodule Container =\n    let Color = 0\n    type Color = Red | Blue\n";
    let src1 = "module Client\nlet f c = match c with Lib.Container.Color.Red -> 1 | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    match proj
        .file(1)
        .resolution_at(nth(src1, "Lib.Container.Color.Red", 0))
    {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("pattern with a value at the path must defer, got {other:?}"),
    }
}

// ---- Cross-file open targets: the residual decides (the cross-file type index) ----
//
// FCS-pinned (probes CF2expr/CF2pat/CF2b/CF3expr/CF3pat/CF5/CF8/CF8pat/CF9/
// CF10/CF11, each two-file pair `dotnet build`-verified, 2026-07-02): the
// positional latest-wins contest applies unchanged when the later open's target
// is a CROSS-FILE project entity. An `open A` whose cross-file `A.Pal` (a nested
// module, a `module A.Pal` header, or a namespace) owns nothing named `Color` is
// transparent — FCS backtracks past it to the lexical candidate, both positions
// (CF2expr/CF2pat/CF2b/CF5). A cross-file `type Color` WITHOUT the case commits
// nothing in pattern position (FCS backtracks, CF3pat) — and the cross-file
// case index is complete for a non-abbreviation exported type, so absence is
// provable there. An ABBREVIATION `type Color = Hue` instead commits through
// its target (FCS binds `A.Hue.Red`, both positions — CF8/CF8pat), so a type
// whose cases sema cannot enumerate must keep deferring.

/// Assert that file1's first `Pal.Color.Red` resolution is an honest defer
/// (`Deferred` or unrecorded) — the cross-file open target may own the residual
/// in a way sema cannot resolve or rule out.
fn assert_cross_file_open_target_defers(src0: &str, src1: &str) {
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    match proj.file(1).resolution_at(nth(src1, "Pal.Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("Pal.Color.Red should defer for {src0:?} / {src1:?}, got {other:?}"),
    }
}

#[test]
fn module_qualified_case_resolves_past_a_cross_file_open_lacking_the_residual() {
    // FCS (probes CF2expr / CF2b / CF5): the later `open A` supplies a
    // cross-file `A.Pal` — a nested module, a `module A.Pal` header, or a
    // namespace — with nothing named `Color`, so it is transparent and FCS
    // backtracks to the lexical `Client.Pal`. The cross-file value / case /
    // module / namespace / type indexes are complete for real-root files, so
    // "owns no `Color`" is provable and the candidate stands (previously a
    // blanket DeferStop — cross-file case-less types were not indexed).
    for src0 in [
        // CF2expr: a nested module under a namespace.
        "namespace A\nmodule Pal =\n    let unrelated = 1\n",
        // CF2b: a `module A.Pal` header.
        "module A.Pal\nlet unrelated = 1\n",
        // CF5: a namespace (`A.Pal` never a module).
        "namespace A.Pal\ntype Unrelated = U1 | U2\n",
    ] {
        assert_resolves_to_own_case(
            src0,
            "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n",
            0,
        );
    }
}

#[test]
fn module_qualified_case_pattern_resolves_past_a_cross_file_open_lacking_the_residual() {
    // FCS (probe CF2pat): the pattern-position variant backtracks identically.
    assert_resolves_to_own_case(
        "namespace A\nmodule Pal =\n    let unrelated = 1\n",
        "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let f c = match c with Pal.Color.Red -> 0 | _ -> 1\n",
        0,
    );
}

#[test]
fn module_qualified_case_pattern_resolves_past_a_cross_file_type_without_the_case() {
    // FCS (probe CF3pat): the cross-file `A.Pal` HAS a `type Color`, but it
    // lacks a case `Red` — in pattern position a case-less segment commits
    // nothing (a static member is not a pattern), so FCS backtracks to the
    // lexical candidate. The cross-file case index is complete for a
    // non-abbreviation exported type, so "no case `Red`" is provable.
    assert_resolves_to_own_case(
        "namespace A\nmodule Pal =\n    type Color = Green | Indigo\n",
        "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let f c = match c with Pal.Color.Red -> 0 | _ -> 1\n",
        0,
    );
}

#[test]
fn module_qualified_case_expression_defers_on_a_cross_file_type_without_the_case() {
    // Probe CF3expr: FCS backtracks here too (`type Color = Green | Indigo` has
    // no member `Red` either) — but sema cannot prove member absence (an
    // augmentation elsewhere can add one), so expression position keeps the
    // deliberate over-defer of the same-file rule (the Bexpr sacrifice): a type
    // at the segment defers, never falls through.
    assert_cross_file_open_target_defers(
        "namespace A\nmodule Pal =\n    type Color = Green | Indigo\n",
        "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n",
    );
}

#[test]
fn module_qualified_case_defers_on_a_cross_file_abbreviation_at_the_segment() {
    // FCS (probes CF8 / CF8pat): the cross-file `A.Pal.Color` is an
    // ABBREVIATION of a union carrying `Red` — FCS resolves `Pal.Color.Red`
    // through it to `A.Hue.Red`, in BOTH positions, so the open is *not*
    // transparent. Sema does not chase a cross-file abbreviation's target, so
    // its case set is not provably absent — defer (falling through to the
    // lexical candidate would be a wrong target).
    let src0 = "namespace A\ntype Hue = Red | Blue\nmodule Pal =\n    type Color = Hue\n";
    for src1 in [
        "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n",
        "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let f c = match c with Pal.Color.Red -> 0 | _ -> 1\n",
    ] {
        assert_cross_file_open_target_defers(src0, src1);
    }
}

#[test]
fn module_qualified_case_defers_on_a_cross_file_value_at_the_segment() {
    // A cross-file `let Color` at the segment is a dottable value — member
    // access commits (the same-file `is_dottable_value` rule, and Gap C's
    // value-wins probes) — so the open is not transparent: defer.
    assert_cross_file_open_target_defers(
        "namespace A\nmodule Pal =\n    let Color = 0\n",
        "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n",
    );
}

#[test]
fn module_qualified_case_defers_on_a_cross_file_companion_submodule() {
    // FCS (probe CF11): the cross-file `A.Pal` owns `Color` as a SUBMODULE, and
    // FCS resolves `Pal.Color.Red` through the open to the submodule's own
    // `let Red` — so the open is not transparent, and emitting the lexical
    // candidate's case would be a wrong target. Sema does not resolve the
    // submodule's members from the classifier, so it defers (sound; the
    // qualified-value branch never runs — the reference is open-rooted).
    assert_cross_file_open_target_defers(
        "namespace A\nmodule Pal =\n    module Color =\n        let Red = 1\n",
        "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n",
    );
}

#[test]
fn module_qualified_case_defers_on_a_hidden_cross_file_open_target() {
    // The cross-file `A.Pal` declares an active pattern, so its value-space
    // names are not fully enumerable (`modules_with_hidden_values`): a hidden
    // constructor could own `Color`, so "owns no `Color`" is not provable —
    // defer rather than backtrack.
    assert_cross_file_open_target_defers(
        "namespace A\nmodule Pal =\n    let (|Odd|Even|) n = if n % 2 = 1 then Odd else Even\n",
        "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n",
    );
}

#[test]
fn module_qualified_case_defers_through_a_cross_file_module_alias_target() {
    // Probe CF10: a cross-file module ABBREVIATION `module Pal = Real` is
    // file-private to FCS — it binds the lexical `Client.Pal`, not the alias's
    // target. Sema keeps an earlier file's alias path in the conservative
    // shadow indexes (it does not model abbreviation file-privacy), so the
    // open-supplied head defers — a sound availability gap, never a wrong
    // target.
    assert_cross_file_open_target_defers(
        "module A.Outer\nmodule Real =\n    type Color = Red | Blue\nmodule Pal = Real\n",
        "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A.Outer\nmodule Use =\n    let x = Pal.Color.Red\n",
    );
}

#[test]
fn module_qualified_case_defers_on_a_cross_file_extern_at_the_segment() {
    // A cross-file `extern … Color()` at the segment lands ONLY in the
    // conservative name-shadow index — it is not a value export, not a type,
    // not a module — yet it is a value-space name the open target owns, so the
    // open is not transparent (`dotnet build`: FCS binds `A.Pal.Color` as the
    // extern and errors FS0039 on `.Red`, so no legal program reaches the
    // lexical candidate here). Any project-introduced name at the segment the
    // decision tree cannot positively classify must defer, never fall through.
    let src0 = "namespace A\nmodule Pal =\n    extern int Color()\n";
    for src1 in [
        "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n",
        "namespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let f c = match c with Pal.Color.Red -> 0 | _ -> 1\n",
    ] {
        assert_cross_file_open_target_defers(src0, src1);
    }
}

#[test]
fn module_qualified_case_defers_on_same_file_contents_under_a_multi_file_namespace() {
    // Probe CF9: the namespace `A.Pal` spans file0 and THIS file, and this
    // file's block declares a `type Color` without the case — FCS backtracks
    // (no member `Red`), but sema conservatively defers on any same-file
    // declaration of the segment name under a multi-file namespace target
    // (member absence is unprovable in expression position, and the merged
    // multi-file view is not classified further) — sound, never a wrong target.
    assert_cross_file_open_target_defers(
        "namespace A.Pal\ntype Unrelated = U1 | U2\n",
        "namespace A.Pal\ntype Color = Green | Indigo\nnamespace Client\nmodule Pal =\n    type Color = Red | Blue\nnamespace Client\nopen A\nmodule Use =\n    let x = Pal.Color.Red\n",
    );
}

#[test]
fn active_pattern_terminated_path_does_not_misresolve_as_a_qualified_case() {
    // Safety: a pattern path ending in an *active-pattern name*
    // (`Color.Red.(|Foo|_|)`, now parseable — see `pat.rs`) references the active
    // pattern, NOT the union case its *prefix* happens to spell. FCS folds the
    // whole `pathOp` into `["Color"; "Red"; "|Foo|_|"]`, but our head `LONG_IDENT`
    // carries only the ident segments (the name is a sibling `ACTIVE_PAT_NAME`),
    // so the qualified-case machinery — which reads the last ident segment as the
    // case name — would otherwise see `["Color"; "Red"]` and resolve the span to
    // `Color.Red`. Defer instead: unresolved is graceful, a wrong target is not.
    let src = "module Top\ntype Color = Red | Blue\nlet f x =\n    match x with\n    | Color.Red.(|Foo|_|) y -> 1\n    | _ -> 0\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "an active-pattern-terminated path must not resolve as the case `Color.Red`, got {other:?}"
        ),
    }
}

#[test]
fn single_segment_active_pattern_qualifier_is_not_a_binder_or_a_case() {
    // The sibling hazard to the test above, on the *binder* path rather than the
    // qualified-case one: with a single prefix segment (`A.(|Foo|Bar|)`), the head
    // `LONG_IDENT` holds one ident, so `binders::single_segment` would read it as a
    // nullary maybe-var head — binding `A` provisionally, which resolution then
    // resolves through `case_reference` to the *unrelated* union case `A`. The
    // prefix of an active-pattern path is a qualifier: neither a binder nor a case.
    let src = "module Top\ntype T = A | B\nlet f x =\n    match x with\n    | A.(|Foo|Bar|) -> 1\n    | _ -> 0\n";
    let rf = resolve(src);
    let qualifier = nth(src, "A.(|Foo|Bar|)", 0);
    let a_only = TextRange::new(qualifier.start(), qualifier.start() + TextSize::from(1));
    match rf.resolution_at(a_only) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "the qualifier of an active-pattern path must not resolve to the case `A`, got {other:?}"
        ),
    }
}

#[test]
fn global_rooted_case_pattern_does_not_misresolve_to_escaped_global_module() {
    // Safety: a `global.`-rooted case pattern is the namespace-root marker, NOT a
    // reference to a same-file escaped ``global`` module. Rooted *pattern*
    // resolution is a follow-up, so `record_qualified_case_pattern` defers on a
    // raw `global` head — the whole span stays unresolved (graceful) rather than
    // mis-resolving to the escaped module's `Red` case. Guards the sema safety of
    // the parser's `global.`-rooted pattern support (`pat.rs`).
    let src = "module Top\nmodule ``global`` =\n    type Color = Red | Blue\nlet f x =\n    match x with\n    | global.Color.Red -> 1\n    | _ -> 0\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "global.Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "root-marker `global.Color.Red` must not resolve to the escaped module, got {other:?}"
        ),
    }
}

// ---- Type accessibility: an inaccessible `private` type's case does not
// resolve cross-file through the type-qualified index (fcs-dump-pinned) ----

#[test]
fn a_private_types_case_is_inaccessible_cross_file_from_unrelated_code() {
    // `type private Foo = | Red` in `namespace B`; from an UNRELATED `namespace
    // Other`, `open B; Foo.Red` is FS0039 in FCS — `Foo` is not accessible, so the
    // case is unbound. The cross-file type-qualified case index must gate on the
    // declaring type's accessibility, or it wrongly resolves `Foo.Red` to the
    // private case (a wrong target on `main`).
    let src0 = "namespace B\n\ntype private Foo =\n    | Red\n    | Blue\n";
    let src1 = "namespace Other\n\nmodule M =\n    open B\n    let y = Foo.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let use_range = nth(src1, "Foo.Red", 0);
    match proj.file(1).resolution_at(use_range) {
        None | Some(Resolution::Deferred(_)) => {}
        other => {
            panic!("an inaccessible private-type case must not resolve cross-file, got {other:?}")
        }
    }
}

#[test]
fn a_private_types_case_is_accessible_cross_file_from_a_descendant() {
    // The gate must not over-reach: from a DESCENDANT of the type's container the
    // `private` case IS accessible (fcs-dump: `Foo.Red` under a module of the same
    // `namespace B` resolves).
    let src0 = "namespace B\n\ntype private Foo =\n    | Red\n    | Blue\n";
    let src1 = "namespace B\n\nmodule Deeper =\n    let y = Foo.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Foo.Red", 0))
        .expect("the descendant resolves the accessible private case");
    let (file_idx, _) = proj.item_def(res).expect("a cross-file case item");
    assert_eq!(file_idx, 0, "resolves to B.Foo.Red in file0");
}

#[test]
fn a_same_file_private_types_case_is_inaccessible_from_a_sibling() {
    // `type private Foo = | Red` in `module A`; a SIBLING `module B` references
    // `A.Foo.Red` in the SAME file. FCS reports FS1092 — the private type `Foo` is
    // inaccessible from a sibling. The same-file module-qualified case resolution
    // was accessibility-blind (a wrong target on `main`).
    let src = "module Lib\n\nmodule A =\n    type private Foo =\n        | Red\n        | Blue\n\nmodule B =\n    let y = A.Foo.Red\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "A.Foo.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "a same-file private-type case must be inaccessible from a sibling, got {other:?}"
        ),
    }
}

#[test]
fn a_same_file_public_types_case_still_resolves_from_a_sibling() {
    // The gate must not over-reach: a PUBLIC type's case resolves from a sibling.
    let src = "module Lib\n\nmodule A =\n    type Foo =\n        | Red\n        | Blue\n\nmodule B =\n    let y = A.Foo.Red\n";
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, "A.Foo.Red", 0))
        .expect("a public type's case resolves from a sibling");
    let def = rf.resolved_def(res).expect("in-file def");
    assert_eq!(def.range, nth(src, "Red", 0), "binds the public case");
}

#[test]
fn an_inaccessible_same_file_type_falls_back_to_a_public_cross_file_case() {
    // The inaccessible-same-file gate must be **transparent**, not terminal: an
    // inaccessible same-file `A.Foo` head must not swallow the reference — FCS skips
    // it and binds a farther same-named candidate. Here file0 has a PUBLIC `A.Foo.Red`
    // (`namespace A`), and file1's `module Ns.A` has a `type private Foo` plus a
    // sibling `module Ns.B` referencing `A.Foo.Red`. fcs-dump: that use binds the
    // public cross-file `A.Foo.Red` in file0, skipping the inaccessible same-file one.
    // A terminal `DeferStop` on the inaccessible head would lose this valid fallback.
    let src0 = "namespace A\n\ntype Foo =\n    | Red\n    | Blue\n";
    let src1 = "namespace Ns\n\nmodule A =\n    type private Foo =\n        | Red\n\nmodule B =\n    let y = A.Foo.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "A.Foo.Red", 0))
        .expect("the inaccessible same-file head falls back to the public cross-file case");
    let (file_idx, _) = proj.item_def(res).expect("a cross-file case item");
    assert_eq!(file_idx, 0, "resolves to the public A.Foo.Red in file0");
}

#[test]
fn an_inaccessible_same_file_type_falls_back_to_a_farther_same_file_case() {
    // The inaccessible-same-file gate must continue the candidate WALK, not just fall
    // to cross-file: a NEARER same-file `module Nest.A` has a `type private Foo`
    // (inaccessible from sibling `Nest.B`), but a FARTHER same-file outer `module A`
    // has a public `type Foo = | Red`. fcs-dump: `A.Foo.Red` in `Nest.B` binds the
    // OUTER `Top.A.Foo.Red`, skipping the nearer inaccessible one. A terminal `Miss`
    // on the inaccessible candidate would jump straight to cross-file and lose this
    // farther same-file binding — so the gate returns `None` (transparent) to let the
    // walk reach the outer `A`.
    let src = "module Top\n\nmodule A =\n    type Foo =\n        | Red\n        | Blue\n\nmodule Nest =\n    module A =\n        type private Foo =\n            | Red\n\n    module B =\n        let y = A.Foo.Red\n";
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, "A.Foo.Red", 0))
        .expect("the walk falls back to the outer same-file A.Foo.Red");
    let def = rf.resolved_def(res).expect("in-file def");
    assert_eq!(
        def.range,
        nth(src, "Red", 0),
        "binds the OUTER public Top.A.Foo.Red, not the nearer inaccessible one"
    );
}

#[test]
fn an_inaccessible_same_file_type_still_yields_to_its_companion_module() {
    // The inaccessible-type gate must suppress ONLY the type's case/member, not the
    // whole candidate: a nearer `module Nest.A` has a `type private Foo` (inaccessible
    // from sibling `Nest.B`) AND a same-named companion `module Foo = let Red`, while a
    // farther outer `module A` has a public `type Foo = | Red`. fcs-dump: `A.Foo.Red`
    // in `Nest.B` binds the NEARER companion value (`Top.Nest.A.FooModule.Red`) — the
    // accessible companion module wins over the outer union case. A gate that returned
    // early would skip the companion and misbind to the outer `Top.A.Foo.Red`.
    let src = "module Top\n\nmodule A =\n    type Foo =\n        | Red\n        | Blue\n\nmodule Nest =\n    module A =\n        type private Foo =\n            | Hidden\n        module Foo =\n            let Red = 99\n\n    module B =\n        let y = A.Foo.Red\n";
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, "A.Foo.Red", 0))
        .expect("the inaccessible type yields to its accessible companion module");
    let def = rf.resolved_def(res).expect("in-file def");
    assert_eq!(
        def.range,
        nth(src, "Red", 1),
        "binds the nearer companion value `let Red = 99`, not the outer union case"
    );
}
