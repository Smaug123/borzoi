//! FCS-free tests for **cross-file union / exception case** resolution: a
//! non-`[<RequireQualifiedAccess>]` case declared in an earlier Compile-order
//! file is reachable from a later file, both opened (`open Lib; Red`) and via the
//! value-namespace shortcut path (`Lib.Red`, module + case, skipping the type).
//!
//! A union case's value-namespace path is `Module.Case` (the type segment is
//! elided), exactly one segment beyond the module — the same shape the
//! module-value-open and qualified-value machinery already handle, so exporting
//! the case as a project item makes both forms resolve. The *type-qualified* form
//! `Lib.Color.Red` is a separate slice (it needs the type qualifier resolved
//! cross-file) and stays deferred here.
//!
//! Cross-file cases carry a **kind** (`ExportedItem.is_case` →
//! `ProjectItems.case_item_ids`), so an opened cross-file case is recognized in
//! *pattern* position too (`match c with Red`), and a case/module name collision
//! stays sound (`open Lib; Red.foo` with both a case and a `module Red` defers the
//! head rather than mis-pointing at the case).
//!
//! Pattern position resolves for a **clean** opened module; the constructor-
//! namespace collision cases defer (sound boundaries, a follow-up): a case shadowed
//! at its path by a same-named `let` is not recovered cross-file, and a case from a
//! *hidden* module (one also bringing an unenumerable active pattern / alias) is not
//! trusted (the hidden constructor could shadow it — FCS picks the active pattern).

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, Resolution, resolve_project};
use rowan::TextRange;

fn impl_file(src: &str) -> ImplFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        p.errors
    );
    ImplFile::cast(p.root).expect("impl file")
}

fn range_of(hay: &str, needle: &str) -> TextRange {
    let s = hay
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {hay:?}"));
    TextRange::new(
        u32::try_from(s).unwrap().into(),
        u32::try_from(s + needle.len()).unwrap().into(),
    )
}

#[test]
fn opened_cross_file_union_case_resolves() {
    // `open Lib` in a later file brings the earlier file's DU case `Red` into
    // unqualified scope (FCS: `Lib.Color.Red`, declared in file0).
    let src0 = "module Lib\ntype Color = Red | Green\n";
    let src1 = "module Client\nopen Lib\nlet x = Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let res = proj
        .file(1)
        .resolution_at(use_range)
        .expect("a resolution at bare `Red`");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("bare `Red` resolves to a cross-file item");
    assert_eq!(file_idx, 0, "declared in file0");
    assert_eq!(
        def.range,
        range_of(src0, "Red"),
        "points at file0's `Red` case"
    );
}

#[test]
fn module_shortcut_cross_file_union_case_resolves() {
    // The value-namespace shortcut `Lib.Red` (module + case, skipping the type
    // `Color`) resolves to the case (FCS: `Lib.Color.Red`).
    let src0 = "module Lib\ntype Color = Red | Green\n";
    let src1 = "module Client\nlet x = Lib.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let res = proj
        .file(1)
        .resolution_at(range_of(src1, "Lib.Red"))
        .expect("a resolution at `Lib.Red`");
    let (file_idx, def) = proj.item_def(res).expect("`Lib.Red` resolves cross-file");
    assert_eq!(file_idx, 0);
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn opened_cross_file_exception_constructor_resolves() {
    // An `exception` constructor is a value-namespace case too: `open Lib` brings
    // `MyErr` into scope.
    let src0 = "module Lib\nexception MyErr of string\n";
    let src1 = "module Client\nopen Lib\nlet e = MyErr \"x\"\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let res = proj
        .file(1)
        .resolution_at(range_of(src1, "MyErr"))
        .expect("a resolution at `MyErr`");
    let (file_idx, def) = proj.item_def(res).expect("`MyErr` resolves cross-file");
    assert_eq!(file_idx, 0);
    assert_eq!(def.range, range_of(src0, "MyErr"));
}

#[test]
fn a_later_value_shadows_a_same_named_case_when_opened() {
    // FCS: a module with a union case `Red` and a *later* `let Red = 0` — the
    // later value shadows the case in expression position, so `open M; Red` (and
    // `M.Red`) resolve to the value (`Demo.M.Red`, the `let`), not the earlier
    // case. Both are exported at the same path `[Demo, M, Red]`; the latest-wins
    // lookup must pick the later `let`.
    let src0 = "namespace Demo\nmodule M =\n    type T = Red | Blue\n    let Red = 0\n";
    let src1 = "namespace Demo\nmodule N =\n    open M\n    let x = Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let res = proj
        .file(1)
        .resolution_at(use_range)
        .expect("a resolution at bare `Red`");
    let (file_idx, def) = proj.item_def(res).expect("resolves cross-file");
    assert_eq!(file_idx, 0);
    // The later `let Red = 0` (the second `Red` occurrence in src0), not the case.
    let value_red = {
        let first = src0.find("Red").expect("case Red");
        src0[first + 3..].find("Red").expect("value Red") + first + 3
    };
    assert_eq!(
        def.range,
        TextRange::new(
            u32::try_from(value_red).unwrap().into(),
            u32::try_from(value_red + 3).unwrap().into(),
        ),
        "the later `let Red` shadows the case in expression position"
    );
}

#[test]
fn namespace_level_union_case_resolves_via_the_qualified_shortcut() {
    // A union case declared directly under a `namespace` (no enclosing module) is
    // exported under the container path (`Lib`, since a namespace carries no
    // value-export `module_path`), so the *qualified* shortcut `Lib.Red` resolves
    // it cross-file (FCS: `Lib.Color.Red`).
    let src0 = "namespace Lib\ntype Color = Red | Green\n";
    let src1 = "module Client\nlet x = Lib.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let res = proj
        .file(1)
        .resolution_at(range_of(src1, "Lib.Red"))
        .expect("a resolution at `Lib.Red`");
    let (file_idx, def) = proj.item_def(res).expect("resolves cross-file");
    assert_eq!(file_idx, 0);
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn namespace_open_of_a_direct_case_resolves() {
    // The bare `open <namespace>; Red` form for a case declared directly under a
    // namespace resolves via the project-namespace index (FCS: `Lib.Color.Red`).
    let src0 = "namespace Lib\ntype Color = Red | Green\n";
    let src1 = "module Client\nopen Lib\nlet x = Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(1)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("bare namespace-open `Red` resolves cross-file");
    assert_eq!(file_idx, 0);
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn relative_namespace_open_of_a_direct_case_resolves() {
    // The namespace path is resolved *relative* to the enclosing namespace: a case
    // in `namespace Outer.Inner`, opened from inside `namespace Outer` as
    // `open Inner`, resolves (FCS: `Outer.Inner.Color.Red`). `open Inner` must not
    // look under the raw root `Inner`.
    let src0 = "namespace Outer.Inner\ntype Color = Red | Green\n";
    let src1 = "namespace Outer\nmodule Client =\n    open Inner\n    let x = Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(1)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("relative namespace-open `Red` resolves cross-file");
    assert_eq!(file_idx, 0);
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn chained_relative_namespace_opens_resolve_a_direct_case() {
    // codex review: a *chained* relative open must shorten against the **resolved**
    // namespace, not the raw written prefix. In `namespace Outer`, `open Inner;
    // open Deep` reaches `Outer.Inner.Deep` (FCS: `Outer.Inner.Deep.Color.Red`) —
    // `open Deep` shortens against the `Outer.Inner` that `open Inner` resolved to.
    let src0 = "namespace Outer.Inner.Deep\ntype Color = Red | Green\n";
    let src1 = "namespace Outer\nmodule C =\n    open Inner\n    open Deep\n    let x = Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(1)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("chained relative namespace-open `Red` resolves cross-file");
    assert_eq!(file_idx, 0);
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn namespace_open_of_own_segment_binds_the_root_not_self() {
    // codex review: `open Inner` from inside `namespace Outer.Inner` is **not** a
    // self-open of `Outer.Inner` — F# does not treat a namespace's own segment as a
    // relative child. With a root `namespace Inner` (case `Red`), `open Inner` binds
    // the *root* `Inner` (FCS: `Inner.RootT.Red`, file 0), so the self/ancestor
    // candidate must be skipped and the root tier reached.
    let src0 = "namespace Inner\ntype RootT = Red | Blue\n";
    let src1 = "namespace Outer.Inner\nmodule C =\n    open Inner\n    let x = Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(1)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("`open Inner` binds the root namespace's case");
    assert_eq!(
        file_idx, 0,
        "the root `namespace Inner`, not self `Outer.Inner`"
    );
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn explicit_ancestor_open_lets_a_later_open_bind_the_current_namespace() {
    // codex review: the self/ancestor skip is for the *implicit* enclosing tier
    // only. An *explicit* `open Outer; open Inner` inside `namespace Outer.Inner`
    // intentionally binds the second open to `Outer.Inner` (FCS:
    // `Outer.Inner.LocalT.Red`, file 1), even with a colliding root `namespace
    // Inner` (file 0).
    let src0 = "namespace Inner\ntype RootT = Red | Blue\n";
    let src1 = "namespace Outer.Inner\ntype LocalT = Red | Green\n";
    let src2 =
        "namespace Outer.Inner\nmodule C =\n    open Outer\n    open Inner\n    let x = Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );

    let i = src2.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(2)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("explicit `open Inner` binds the current namespace's case");
    assert_eq!(
        file_idx, 1,
        "the explicitly opened `Outer.Inner` (file 1), not root"
    );
    assert_eq!(def.range, range_of(src1, "Red"));
}

#[test]
fn open_opens_both_a_module_and_a_same_named_relative_namespace() {
    // codex review: `open Inner` from `namespace Outer` names *both* a root
    // `module Inner` (with `let Red`) and the relative `namespace Outer.Inner`
    // (with case `Red`). FCS opens both, and the namespace case wins bare `Red` —
    // in *expression* and *pattern* position (`Outer.Inner.Color.Red`, file 1).
    let src0 = "module Inner\nlet Red = 0\n";
    let src1 = "namespace Outer.Inner\ntype Color = Red | Green\n";
    let src2 = "namespace Outer\nmodule C =\n    open Inner\n    let x = Red\n    let f y = match y with Red -> 1 | _ -> 2\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(2);
    let case_red = range_of(src1, "Red"); // `Outer.Inner` `type Color = Red`

    for needle in ["= Red", "with Red"] {
        let off = src2.find(needle).expect("use") + needle.len() - 3;
        let use_range = TextRange::new(
            u32::try_from(off).unwrap().into(),
            u32::try_from(off + 3).unwrap().into(),
        );
        let (file_idx, def) = rf
            .resolution_at(use_range)
            .and_then(|r| proj.item_def(r))
            .unwrap_or_else(|| panic!("`Red` at {needle:?} resolves"));
        assert_eq!(
            file_idx, 1,
            "the namespace case (file 1), not module Inner's value"
        );
        assert_eq!(def.range, case_red);
    }
}

#[test]
fn relative_namespace_open_keeps_the_as_written_root_open() {
    // codex review: a relative namespace open keeps the as-written root open too.
    // `namespace Outer.Inner` makes `open Inner` (from `namespace Outer`) relative
    // (`Outer.Inner`), but a root `module Inner.Deep` is still reachable: `open
    // Inner; open Deep; foo` resolves `foo` from `Inner.Deep` (FCS, file 0), since
    // `Outer.Inner.Deep` does not exist and the root `Inner` prefix is retained.
    let src0 = "module Inner.Deep\nlet foo = 0\n";
    let src1 = "namespace Outer.Inner\ntype T = X | Y\n";
    let src2 = "namespace Outer\nmodule C =\n    open Inner\n    open Deep\n    let z = foo\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );

    let i = src2.rfind("foo").expect("`foo` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(2)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("`foo` resolves through the retained root `Inner`");
    assert_eq!(file_idx, 0, "the root `module Inner.Deep` (file 0)");
    assert_eq!(def.range, range_of(src0, "foo"));
}

#[test]
fn open_opens_both_a_root_and_a_relative_namespace() {
    // codex review: `open Inner` from `namespace Outer` opens *both* the root
    // `namespace Inner` and the relative `namespace Outer.Inner` (FCS opens all
    // namespaces the path names). Their distinct cases both resolve: `RootCase` →
    // `Inner.RootT.RootCase` (file 0), `LocalCase` → `Outer.Inner.LocalT.LocalCase`
    // (file 1).
    let src0 = "namespace Inner\ntype RootT = RootCase | RB\n";
    let src1 = "namespace Outer.Inner\ntype LocalT = LocalCase | LB\n";
    let src2 = "namespace Outer\nmodule C =\n    open Inner\n    let a = RootCase\n    let b = LocalCase\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(2);

    for (needle, want_file, want_src) in [("RootCase", 0usize, src0), ("LocalCase", 1usize, src1)] {
        let i = src2.rfind(needle).expect("use");
        let use_range = TextRange::new(
            u32::try_from(i).unwrap().into(),
            u32::try_from(i + needle.len()).unwrap().into(),
        );
        let (file_idx, def) = rf
            .resolution_at(use_range)
            .and_then(|r| proj.item_def(r))
            .unwrap_or_else(|| panic!("`{needle}` resolves"));
        assert_eq!(file_idx, want_file, "`{needle}` from file {want_file}");
        assert_eq!(def.range, range_of(want_src, needle));
    }
}

#[test]
fn module_colliding_chained_namespace_open_resolves() {
    // codex review: even when each `open` *also* names a module, the resolved
    // namespace must be recorded as a shortening prefix so the chain reaches the
    // namespace. `module Inner` (nested `module Deep` with `let Red`) collides with
    // `namespace Outer.Inner.Deep` (case `Red`); from `namespace Outer`,
    // `open Inner; open Deep; Red` binds the namespace case
    // `Outer.Inner.Deep.Color.Red` (FCS, file 1), not the module value.
    let src0 = "module Inner\nmodule Deep =\n    let Red = 0\n";
    let src1 = "namespace Outer.Inner.Deep\ntype Color = Red | Green\n";
    let src2 = "namespace Outer\nmodule C =\n    open Inner\n    open Deep\n    let y = Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );

    let i = src2.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(2)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("module-colliding chained namespace open resolves");
    assert_eq!(
        file_idx, 1,
        "the namespace case `Outer.Inner.Deep` (file 1)"
    );
    assert_eq!(def.range, range_of(src1, "Red"));
}

#[test]
fn relative_namespace_open_ignores_module_segments_of_the_container() {
    // codex review: implicit relative namespace resolution probes only enclosing
    // *namespace* prefixes, not nested-module segments. Inside `module Outer.Client`
    // (a dotted top-level module), `open Inner` must NOT reach `Outer.Client.Inner`
    // (FCS leaves it undefined) — `Outer.Client` is a module, not a namespace
    // container. So the case `Red` does not resolve from `Outer.Client.Inner`.
    let src0 = "namespace Outer.Client.Inner\ntype DeepT = Red | Blue\n";
    let src1 = "module Outer.Client\nopen Inner\nlet x = Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    let i = src1.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    // Must not resolve to `Outer.Client.Inner`'s case (file 0).
    let to_deep = rf
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .is_some_and(|(idx, _)| idx == 0);
    assert!(
        !to_deep,
        "`open Inner` inside `module Outer.Client` must not reach `Outer.Client.Inner`"
    );
}

#[test]
fn relative_namespace_open_beats_a_root_collision_from_a_module() {
    // codex review: the self/ancestor skip uses the enclosing *namespace* prefix,
    // not the full container. From inside `namespace Outer; module M`, `open Client`
    // resolves the relative `Outer.Client` over a root `namespace Client` (FCS:
    // `Outer.Client.LocalT.Red`) — the relative match must not be dropped as if it
    // were the container's own segment.
    let src0 = "namespace Outer.Client\ntype LocalT = Red | Green\n";
    let src1 = "namespace Client\ntype RootT = Red | Blue\n";
    let src2 = "namespace Outer\nmodule M =\n    open Client\n    let x = Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );

    let i = src2.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(2)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("relative `Outer.Client` resolves");
    assert_eq!(
        file_idx, 0,
        "the relative `Outer.Client` (file 0), not root `Client`"
    );
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn namespace_opened_union_case_resolves_in_pattern_position() {
    // FCS sweep: a namespace-opened union case resolves in PATTERN position (the
    // `case_reference` path), not just expression position. `open Lib` brings
    // `Color.Red` into the constructor namespace, so `match x with Red` is
    // `Lib.Color.Red` (file 0).
    let src0 = "namespace Lib\ntype Color = Red | Blue\n";
    let src1 = "module Client\nopen Lib\nlet f x = match x with Red -> 1 | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let (file_idx, def) = proj
        .file(1)
        .resolution_at(range_of(src1, "Red"))
        .and_then(|r| proj.item_def(r))
        .expect("the pattern case `Red` resolves cross-file");
    assert_eq!(file_idx, 0, "`Lib.Color.Red` (file 0)");
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn namespace_opened_case_outranks_a_local_value_in_pattern_position() {
    // FCS sweep: in pattern position a union case is resolved through F#'s
    // constructor namespace, which a same-named *value* does not enter. So even
    // with a later local `let Red = 99`, `match x with Red` is the namespace-opened
    // case `Lib.Color.Red` (file 0), not the local value (FCS).
    let src0 = "namespace Lib\ntype Color = Red | Blue\n";
    let src1 = "module Client\nopen Lib\nlet Red = 99\nlet f x = match x with Red -> 1 | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    // The pattern `Red` is the last occurrence (after the local `let Red`).
    let i = src1.rfind("Red").expect("pattern `Red`");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(1)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("the pattern case resolves to the namespace case, not the local value");
    assert_eq!(
        file_idx, 0,
        "`Lib.Color.Red` (file 0), not the local `let Red`"
    );
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn namespace_opened_exception_constructor_resolves_in_both_positions() {
    // FCS sweep: an `exception` constructor declared directly under a namespace
    // resolves through a namespace `open` in BOTH expression and pattern position
    // (FCS: `Lib.MyErr`).
    let src0 = "namespace Lib\nexception MyErr of int\n";
    let src_expr = "module Client\nopen Lib\nlet x = MyErr 3\n";
    let src_pat = "module Client\nopen Lib\nlet f x = match x with MyErr n -> n | _ -> 0\n";

    for src1 in [src_expr, src_pat] {
        let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
        let (file_idx, def) = proj
            .file(1)
            .resolution_at(range_of(src1, "MyErr"))
            .and_then(|r| proj.item_def(r))
            .unwrap_or_else(|| panic!("`MyErr` resolves in {src1:?}"));
        assert_eq!(file_idx, 0, "`Lib.MyErr` (file 0) in {src1:?}");
        assert_eq!(def.range, range_of(src0, "MyErr"));
    }
}

#[test]
fn chained_namespace_opens_at_depth_resolve_through_the_open_prefix() {
    // FCS sweep: chained opens compose through the explicit-open prefix even in a
    // deep namespace. In `namespace A.B`, `open C` opens `A.B.C`, then `open D`
    // shortens to `A.B.C.D` via that prefix, so `deep` is `A.B.C.D.deep` (file 0).
    let src0 = "namespace A.B\nmodule C =\n    module D =\n        let deep = 1\n";
    let src1 = "namespace A.B\nmodule User =\n    open C\n    open D\n    let x = deep\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let (file_idx, def) = proj
        .file(1)
        .resolution_at(range_of(src1, "deep"))
        .and_then(|r| proj.item_def(r))
        .expect("`deep` resolves through the chained opens");
    assert_eq!(file_idx, 0, "`A.B.C.D.deep` (file 0)");
    assert_eq!(def.range, range_of(src0, "deep"));
}

#[test]
fn relative_namespace_open_ignores_ancestor_sibling_namespaces() {
    // codex review: implicit relative `open` searches only the current namespace's
    // immediate child, never ancestor-sibling prefixes. From `namespace A.B.C`,
    // `open D` must NOT reach the sibling `namespace A.B.D` (FCS: FS0039 — `D` and
    // `CaseD` undefined); only the child `A.B.C.D` and the root `D` are searched.
    let src0 = "namespace A.B.D\ntype T = CaseD | Other\n";
    let src1 = "namespace A.B.C\nmodule M =\n    open D\n    let x = CaseD\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("CaseD").expect("`CaseD` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 5).unwrap().into(),
    );
    let to_sibling = proj
        .file(1)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .is_some_and(|(idx, _)| idx == 0);
    assert!(
        !to_sibling,
        "`open D` in `namespace A.B.C` must not reach sibling `namespace A.B.D`"
    );
}

#[test]
fn relative_namespace_open_reaches_the_immediate_child_of_a_deep_namespace() {
    // codex review: the immediate child of the *current* namespace is reachable
    // even when the namespace is deep. From `namespace A.B.C`, `open D` reaches the
    // child `A.B.C.D` (FCS: resolves, with an FS0893 partial-path warning), so its
    // case resolves — the lower bound of the relative search is `namespace_depth`,
    // not below it.
    let src0 = "namespace A.B.C.D\ntype T = CaseChild | Other\n";
    let src1 = "namespace A.B.C\nmodule M =\n    open D\n    let x = CaseChild\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("CaseChild").expect("`CaseChild` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "CaseChild".len()).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(1)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("the immediate child namespace resolves");
    assert_eq!(file_idx, 0, "the child namespace `A.B.C.D` (file 0)");
    assert_eq!(def.range, range_of(src0, "CaseChild"));
}

#[test]
fn relative_module_open_outranks_a_root_namespace_collision() {
    // codex review: precedence is the open path's *relativeness*, not the
    // module-vs-namespace category. From `namespace Outer; module C`, `open Inner`
    // resolves the relative module `Outer.Inner` (its `let Red`) over a root
    // `namespace Inner` declaring case `Red` — FCS: bare `Red` is
    // `Outer.Inner.Red` (file 0), even though the namespace pass used to be applied
    // after the module and so let the root namespace's case shadow it.
    let src0 = "module Outer.Inner\nlet Red = 1\n";
    let src1 = "namespace Inner\ntype Color = Red | Blue\n";
    let src2 = "namespace Outer\nmodule C =\n    open Inner\n    let x = Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );

    let i = src2.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(2)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("bare `Red` resolves to the relative module value");
    assert_eq!(
        file_idx, 0,
        "the relative module `Outer.Inner` (file 0), not root `namespace Inner`"
    );
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn relative_module_open_outranks_a_root_module_keeping_both_open() {
    // codex review: an `open` opens *every* matching project module, not just the
    // most relative one, ordered by proximity. With a root `module Inner` and a
    // relative `module Outer.Inner` both exporting `Both`, `open Inner` from
    // `namespace Outer; module C` opens both — FCS resolves `RootOnly` to the root
    // module, `LocalOnly` to the relative module, and the colliding `Both` to the
    // relative module (`Outer.Inner.Both`, the higher-priority one).
    let src0 = "module Inner\nlet RootOnly = 0\nlet Both = 1\n";
    let src1 = "module Outer.Inner\nlet LocalOnly = 2\nlet Both = 3\n";
    let src2 = "namespace Outer\nmodule C =\n    open Inner\n    let a = RootOnly\n    let b = LocalOnly\n    let c = Both\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );

    let resolve_use = |needle: &str| {
        proj.file(2)
            .resolution_at(range_of(src2, needle))
            .and_then(|r| proj.item_def(r))
            .unwrap_or_else(|| panic!("{needle} resolves"))
    };

    let (root_only_file, root_only_def) = resolve_use("RootOnly");
    assert_eq!(root_only_file, 0, "`RootOnly` is the root module (file 0)");
    assert_eq!(root_only_def.range, range_of(src0, "RootOnly"));

    let (local_only_file, local_only_def) = resolve_use("LocalOnly");
    assert_eq!(
        local_only_file, 1,
        "`LocalOnly` is the relative module (file 1)"
    );
    assert_eq!(local_only_def.range, range_of(src1, "LocalOnly"));

    let (both_file, both_def) = resolve_use("Both");
    assert_eq!(
        both_file, 1,
        "the colliding `Both` resolves to the relative module (file 1), the higher priority"
    );
    assert_eq!(both_def.range, range_of(src1, "Both"));
}

#[test]
fn a_namespace_open_keeps_an_anonymous_root_module_opaque() {
    // codex review: `open X` resolves the project namespace `X`, but an
    // anonymous-root local `module X` (whose values we cannot enumerate) shares the
    // path. FCS opens both, so the local module's `Foo` shadows the earlier
    // `open A`'s `Foo` (FCS: bare `Foo` is the local `X.Foo`). We cannot enumerate
    // the local module, so we must stay opaque and defer — never resolve `Foo` to
    // `A.Foo`. The opaque fallback is gated on whether an *enumerable module* was
    // opened, not on whether *any* project entity (here, the namespace) resolved.
    let src0 = "namespace X\ntype T = Dummy\n";
    let src1 = "module A\nlet Foo = 1\n";
    let src2 = "open A\nmodule X =\n    let Foo = 2\nopen X\nlet y = Foo\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );

    let i = src2.rfind("Foo").expect("`Foo` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let res = proj.file(2).resolution_at(use_range);
    // Whatever we do, we must NOT resolve `Foo` to the earlier `open A`'s `A.Foo`
    // (file 1): the local `module X` shadows it, so the conservative answer defers.
    if let Some(r) = res
        && let Some((file_idx, _)) = proj.item_def(r)
    {
        assert_ne!(
            file_idx, 1,
            "`Foo` must not resolve to `A.Foo`; the anonymous-root `module X` shadows it (stay opaque)"
        );
    }
}

#[test]
fn dotted_module_implies_namespaces_for_chained_open() {
    // codex review: a *dotted top-level module* `module Outer.Inner.Helpers` makes
    // `Outer` and `Outer.Inner` namespaces. From `namespace Outer`, `open Inner;
    // open Helpers` chains through them to the module `Outer.Inner.Helpers`, so its
    // case `Red` resolves (FCS: `Outer.Inner.Helpers.Color.Red`).
    let src0 = "module Outer.Inner.Helpers\ntype Color = Red | Green\n";
    let src1 = "namespace Outer\nmodule C =\n    open Inner\n    open Helpers\n    let x = Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(1)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("chained open through a dotted module's namespaces resolves");
    assert_eq!(file_idx, 0);
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn chained_relative_namespace_open_outranks_a_root_collision() {
    // codex review: with both `Outer.Inner.Deep` (file 0) and a root `Inner.Deep`
    // (file 1) declaring a case `Red`, `open Inner; open Deep` from `namespace
    // Outer` binds the **relative** `Outer.Inner.Deep.Color.Red` (FCS), not the
    // root `Inner.Deep`'s case — the resolved-prefix chain must win.
    let src0 = "namespace Outer.Inner.Deep\ntype Color = Red | Green\n";
    let src1 = "namespace Inner.Deep\ntype Other = Red | Blue\n";
    let src2 = "namespace Outer\nmodule C =\n    open Inner\n    open Deep\n    let x = Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );

    let i = src2.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = proj
        .file(2)
        .resolution_at(use_range)
        .and_then(|r| proj.item_def(r))
        .expect("chained relative open resolves");
    assert_eq!(
        file_idx, 0,
        "the relative `Outer.Inner.Deep`, not root `Inner.Deep`"
    );
    assert_eq!(def.range, range_of(src0, "Red"));
}

#[test]
fn exported_case_has_one_identity_for_declaration_and_cross_file_use() {
    // Find-references / rename relies on a single `Resolution` identity. A
    // non-qualified union case is exported as an `Item`, and its *declaration*
    // occurrence carries the same `Item` — so the declaration and a later file's
    // opened use share one resolution (the handler collects by `Resolution`
    // equality).
    let src0 = "module Lib\ntype Color = Red | Green\n";
    let src1 = "module Client\nopen Lib\nlet x = Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let decl = proj
        .file(0)
        .resolution_at(range_of(src0, "Red"))
        .expect("the case declaration resolves to itself");
    let i = src1.rfind("Red").expect("`Red` use");
    let use_res = proj
        .file(1)
        .resolution_at(TextRange::new(
            u32::try_from(i).unwrap().into(),
            u32::try_from(i + 3).unwrap().into(),
        ))
        .expect("the cross-file use resolves");
    assert!(
        matches!(decl, Resolution::Item(_)),
        "decl is an Item: {decl:?}"
    );
    assert_eq!(
        decl, use_res,
        "the declaration and the cross-file use must share one Resolution identity"
    );
}

#[test]
fn pattern_case_does_not_return_a_stale_earlier_open() {
    // Soundness (codex review): `open Other` (same-file case `Red`) then `open Lib`
    // (earlier-file case `Red`); `match x with Red`. FCS resolves the *latest* open
    // (`Lib.LC.Red`). We cannot resolve a cross-file case in pattern position
    // (follow-up), but we must NOT return the stale earlier `Other.Red` — defer
    // instead (sound; never a wrong go-to-definition).
    let src0 = "module Lib\ntype LC = Red | Blue\n";
    let src1 = "namespace App\nmodule Other =\n    type OC = Red | Green\nmodule N =\n    open Other\n    open Lib\n    let f x = match x with Red -> 1 | _ -> 2\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    // The pattern `Red` must not resolve to `Other.Red` (occurrence 0 in src1, the
    // case decl). It may defer or bind a fresh variable, but never the stale case.
    let red_decl = range_of(src1, "Red"); // `Other`'s `type OC = Red`
    let i = src1.rfind("Red").expect("pattern `Red`");
    let pat = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let to_other = rf
        .resolution_at(pat)
        .and_then(|r| rf.resolved_def(r))
        .is_some_and(|d| d.range == red_decl);
    assert!(
        !to_other,
        "pattern `Red` must not resolve to the stale earlier `open Other`'s case"
    );
}

#[test]
fn opened_case_does_not_mask_a_same_named_dotted_module() {
    // FCS: `open Lib; Red.foo` where `Lib` exposes both a case `Red` (`type C =
    // Red | Blue`) and a `module Red` resolves to the *module* member
    // `Lib.Red.foo` — the opened case does not mask the dotted module path (a
    // nullary case has no `.member`). The `foo` use must resolve cross-file to the
    // module value, and `Red` must NOT be recorded as the case.
    let src0 = "namespace Lib\ntype C = Red | Blue\nmodule Red =\n    let foo = 1\n";
    let src1 = "module Client\nopen Lib\nlet x = Red.foo\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    // `Red.foo` (whole) resolves to the module value `Lib.Red.foo`.
    let res = rf
        .resolution_at(range_of(src1, "Red.foo"))
        .expect("a resolution at `Red.foo`");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("resolves cross-file to the module value");
    assert_eq!(file_idx, 0);
    assert_eq!(
        def.range,
        range_of(src0, "foo"),
        "points at `module Red`'s `foo`"
    );

    // The head `Red` must not be recorded as the case constructor (that would be a
    // wrong go-to-def — FCS treats it as the module qualifier).
    let case_red = range_of(src0, "Red"); // `type C = Red`
    let i = src1.find("Red.foo").expect("head");
    let head = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let head_is_case = rf
        .resolution_at(head)
        .and_then(|r| rf.resolved_def(r))
        .is_some_and(|d| d.range == case_red);
    assert!(
        !head_is_case,
        "the dotted head `Red` must not resolve to the case"
    );
}

#[test]
fn opened_case_dotted_head_defers_under_an_opaque_module_open() {
    // Soundness (codex review): `open M` where `M` has both a case `Red` and a
    // `module Red` sets `opaque_dotted_open`, so the module-path resolution is
    // skipped. FCS resolves `Red.foo` through the module (`Demo.M.Red.foo`); we
    // cannot under the opaque open, but we must NOT record the head `Red` as the
    // opened case (a wrong go-to-def) — it defers. (Single-file here so the case
    // is a same-file `Item`, classifiable as a case.)
    let src = "namespace Demo\nmodule M =\n    type T = Red | Blue\n    module Red =\n        let foo = 1\nmodule N =\n    open M\n    let x = Red.foo\n";
    let proj = resolve_project(&[impl_file(src)], &AssemblyEnv::default());
    let rf = proj.file(0);

    // The head `Red` (in `Red.foo`) must not resolve to the case (occurrence 0).
    let case_red = range_of(src, "Red"); // `type T = Red`
    let i = src.rfind("Red.foo").expect("dotted head");
    let head = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let head_is_case = rf
        .resolution_at(head)
        .and_then(|r| rf.resolved_def(r))
        .is_some_and(|d| d.range == case_red);
    assert!(
        !head_is_case,
        "the dotted head `Red` under an opaque open must not resolve to the case"
    );
}

#[test]
fn pattern_position_cross_file_case_resolves() {
    // A cross-file opened case used as a *pattern* head resolves to the case (FCS:
    // `match c with Red | Green` → `Lib.Color.Red` / `Lib.Color.Green`, declared in
    // file0). Cross-file case-kind lets `case_reference` recognize the opened
    // `Item` in pattern position.
    let src0 = "module Lib\ntype Color = Red | Green\n";
    let src1 = "module Client\nopen Lib\nlet f c = match c with Red -> 1 | Green -> 2\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    for case in ["Red", "Green"] {
        // The pattern head resolves to file0's case binder.
        let pat = {
            let i = src1.find(&format!("{case} ->")).expect("pattern head");
            TextRange::new(
                u32::try_from(i).unwrap().into(),
                u32::try_from(i + case.len()).unwrap().into(),
            )
        };
        let res = proj
            .file(1)
            .resolution_at(pat)
            .unwrap_or_else(|| panic!("no resolution at pattern `{case}`"));
        let (file_idx, def) = proj
            .item_def(res)
            .unwrap_or_else(|| panic!("pattern `{case}` resolves to a cross-file item"));
        assert_eq!(file_idx, 0, "`{case}` declared in file0");
        assert_eq!(
            def.range,
            range_of(src0, case),
            "points at file0's `{case}` case"
        );
    }
}

#[test]
fn pattern_position_cross_file_case_with_field_resolves() {
    // A constructor pattern *with a field* (`Circle r`) resolves the case head
    // cross-file (`Lib.Shape.Circle`); the field `r` binds a fresh local.
    let src0 = "module Lib\ntype Shape = Circle of int | Square\n";
    let src1 = "module Client\nopen Lib\nlet area s = match s with Circle r -> r | Square -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let pat = {
        let i = src1.find("Circle r").expect("ctor pattern");
        TextRange::new(
            u32::try_from(i).unwrap().into(),
            u32::try_from(i + "Circle".len()).unwrap().into(),
        )
    };
    let res = proj
        .file(1)
        .resolution_at(pat)
        .expect("a resolution at `Circle`");
    let (file_idx, def) = proj.item_def(res).expect("`Circle` resolves cross-file");
    assert_eq!(file_idx, 0);
    assert_eq!(def.range, range_of(src0, "Circle"));
}

#[test]
fn require_qualified_case_is_not_a_pattern_constructor_cross_file() {
    // A `[<RequireQualifiedAccess>]` case is not brought into scope bare even after
    // `open`, so a pattern head `Red` is a fresh variable binding (a local), not
    // file0's case — must not route cross-file.
    let src0 = "module Lib\n[<RequireQualifiedAccess>]\ntype Color = Red | Green\n";
    let src1 = "module Client\nopen Lib\nlet f c = match c with Red -> 1 | _ -> 2\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let routes_to_file0 = proj
        .file(1)
        .resolutions()
        .values()
        .any(|r| proj.item_def(*r).is_some_and(|(idx, _)| idx == 0));
    assert!(
        !routes_to_file0,
        "a require-qualified case must not be a cross-file pattern constructor"
    );
}

#[test]
fn value_shadowed_case_resolves_expression_to_value_and_pattern_to_case() {
    // A module exporting a case `Red` and a *later* same-named `let Red = 0`: the
    // value shadows the case in *expression* position (FCS / us: the value), but
    // **not** in *pattern* position (FCS: `match x with Red` → the case, since
    // values do not shadow constructors there). The constructor index keeps the
    // case separate from the value index, so both resolve to their namespace.
    let src0 = "module Lib\ntype T = Red | Blue\nlet Red = 0\n";
    let src1 = "module Client\nopen Lib\nlet v = Red\nlet f x = match x with Red -> 1 | _ -> 2\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    let case_red = range_of(src0, "Red"); // `type T = Red`
    let value_red = {
        let first = src0.find("Red").expect("case Red");
        src0[first + 3..].find("Red").expect("value Red") + first + 3
    };
    let value_red = TextRange::new(
        u32::try_from(value_red).unwrap().into(),
        u32::try_from(value_red + 3).unwrap().into(),
    );

    // Expression `let v = Red` → the value (the later `let`).
    let expr = {
        let i = src1.find("= Red").expect("expr") + 2;
        TextRange::new(
            u32::try_from(i).unwrap().into(),
            u32::try_from(i + 3).unwrap().into(),
        )
    };
    let expr_def = rf
        .resolution_at(expr)
        .and_then(|r| proj.item_def(r))
        .expect("expression `Red` resolves cross-file");
    assert_eq!(expr_def.1.range, value_red, "expression `Red` is the value");

    // Pattern `match x with Red` → the case.
    let pat = {
        let i = src1.find("Red ->").expect("pattern");
        TextRange::new(
            u32::try_from(i).unwrap().into(),
            u32::try_from(i + 3).unwrap().into(),
        )
    };
    let pat_def = rf
        .resolution_at(pat)
        .and_then(|r| proj.item_def(r))
        .expect("pattern `Red` resolves cross-file");
    assert_eq!(pat_def.1.range, case_red, "pattern `Red` is the case");
}

#[test]
fn opened_case_defers_in_pattern_under_an_opaque_open() {
    // Soundness (codex review): after `open Lib` brings the case `Red` into scope, a
    // later *opaque* open (here `open type Foo` whose project target's statics we
    // cannot enumerate — `opaque_value_open`) could bring an unenumerable
    // constructor `Red` that shadows it. `case_reference` must defer the opened case
    // in pattern position while an opaque open is active, mirroring `lookup`'s
    // expression-side conservatism — rather than record `Lib.Red` (a wrong
    // go-to-def). Conservative under-resolution; sound.
    let src0 = "module Lib\ntype Color = Red | Green\n";
    let src1 = "module N\nopen Lib\ntype Foo = int\nopen type Foo\nlet f x = match x with Red -> 1 | _ -> 2\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    let i = src1.find("Red ->").expect("pattern");
    let pat = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    // Must NOT resolve to file0's case while an opaque open is in scope.
    let to_case = rf
        .resolution_at(pat)
        .and_then(|r| proj.item_def(r))
        .is_some_and(|(idx, _)| idx == 0);
    assert!(
        !to_case,
        "an opened case must defer in pattern position while an opaque open is active"
    );
}

#[test]
fn value_shadowed_later_open_case_wins_over_an_earlier_open_in_pattern() {
    // Soundness (codex review): `open A` (case `Red`) then `open M` where M exports
    // a case `Red` *shadowed* by a later `let Red = 0`. In pattern position M's
    // constructor (the latest open) shadows A's, so the pattern resolves to M's
    // case `Cm.Red` (file 1) — not A's `Ca.Red` and not deferred.
    let src_a = "module A\ntype Ca = Red | Other\n";
    let src_m = "module M\ntype Cm = Red | Blue\nlet Red = 0\n";
    let src_n = "module N\nopen A\nopen M\nlet f x = match x with Red -> 1 | _ -> 2\n";
    let proj = resolve_project(
        &[impl_file(src_a), impl_file(src_m), impl_file(src_n)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(2);

    let i = src_n.find("Red ->").expect("pattern");
    let pat = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = rf
        .resolution_at(pat)
        .and_then(|r| proj.item_def(r))
        .expect("pattern `Red` resolves to the latest open's case");
    assert_eq!(file_idx, 1, "the later `open M`'s case (file 1), not A's");
    assert_eq!(def.range, range_of(src_m, "Red"), "M's case `Cm.Red`");
}

#[test]
fn repeatedly_value_shadowed_case_resolves_in_pattern() {
    // The value-shadow chain `type Cm = Red` → `let Red = 0` → `let Red = 1` does
    // not affect the constructor index — the case survives, so the pattern resolves
    // to M's case (the latest open), the same as a single shadow.
    let src_a = "module A\ntype Ca = Red | Other\n";
    let src_m = "module M\ntype Cm = Red | Blue\nlet Red = 0\nlet Red = 1\n";
    let src_n = "module N\nopen A\nopen M\nlet f x = match x with Red -> 1 | _ -> 2\n";
    let proj = resolve_project(
        &[impl_file(src_a), impl_file(src_m), impl_file(src_n)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(2);

    let i = src_n.find("Red ->").expect("pattern");
    let pat = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let (file_idx, def) = rf
        .resolution_at(pat)
        .and_then(|r| proj.item_def(r))
        .expect("pattern `Red` resolves to M's case despite repeated shadowing");
    assert_eq!(file_idx, 1);
    assert_eq!(def.range, range_of(src_m, "Red"), "M's case `Cm.Red`");
}

#[test]
fn hidden_module_case_defers_in_cross_file_pattern() {
    // Soundness (codex review): a module `M` exporting a union case `Red` *and* an
    // active pattern `(|Red|_|)` — FCS resolves `open M; match x with Red` to the
    // active pattern (`M.(|Red|_|).Red`), not the union case. The active pattern is
    // not enumerable cross-file (it makes `M` hidden), so the union case cannot be
    // trusted in pattern position — we defer rather than return the (possibly
    // shadowed) union case.
    let src0 = "module M\ntype T = Red | Blue\nlet (|Red|_|) x = if x then Some () else None\n";
    let src1 = "module N\nopen M\nlet f x = match x with Red -> 1 | _ -> 2\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    let case_red = range_of(src0, "Red"); // `type T = Red`
    let i = src1.find("Red ->").expect("pattern");
    let pat = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let to_union_case = rf
        .resolution_at(pat)
        .and_then(|r| proj.item_def(r))
        .is_some_and(|(idx, d)| idx == 0 && d.range == case_red);
    assert!(
        !to_union_case,
        "a hidden module's union case must not be trusted in pattern position"
    );
}

#[test]
fn hidden_module_value_shadowed_case_defers_in_pattern() {
    // GPT-5.5 High-1 cross-product: M exports a union case `Red`, a same-named
    // `let Red = 0` (value-shadowing it), AND an active pattern `(|Red|_|)`. FCS
    // resolves `open M; match x with Red` to the active pattern, not the union
    // case. The new constructor `pattern_only` entry for the value-shadowed case
    // must be *suppressed* (M is hidden by the active pattern), so the pattern
    // defers — not the union case.
    let src0 = "module M\ntype T = Red | Blue\nlet Red = 0\nlet (|Red|_|) x = if x then Some () else None\n";
    let src1 = "module N\nopen M\nlet f x = match x with Red -> 1 | _ -> 2\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    let case_red = range_of(src0, "Red"); // `type T = Red`
    let i = src1.find("Red ->").expect("pattern");
    let pat = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let to_union_case = rf
        .resolution_at(pat)
        .and_then(|r| proj.item_def(r))
        .is_some_and(|(idx, d)| idx == 0 && d.range == case_red);
    assert!(
        !to_union_case,
        "a hidden module's value-shadowed union case must still be suppressed in pattern position"
    );
}

#[test]
fn cross_file_module_augmentation_splits_value_and_constructor() {
    // GPT-5.5 High-2: a `namespace Ns; module M` augmented across files — file 0
    // declares `type T = Red | Blue`, file 1 a `let Red = 0`. File 1's `module N`
    // opens M. FCS: expression `Red` → file 1's value (`Ns.M.Red`); pattern `Red` →
    // file 0's case (`Ns.M.T.Red`). The constructor projection must merge
    // independently of the value-space dedup — file 1's `let` must not block file
    // 0's case in pattern position.
    let src0 = "namespace Ns\nmodule M =\n    type T = Red | Blue\n";
    let src1 = "namespace Ns\nmodule M =\n    let Red = 0\nmodule N =\n    open M\n    let v = Red\n    let f x = match x with Red -> 1 | _ -> 2\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    // Expression `let v = Red` (in file 1) → file 1's value.
    let expr = {
        let i = src1.find("= Red").expect("expr") + 2;
        TextRange::new(
            u32::try_from(i).unwrap().into(),
            u32::try_from(i + 3).unwrap().into(),
        )
    };
    let (e_file, e_def) = proj
        .file(1)
        .resolution_at(expr)
        .and_then(|r| proj.item_def(r))
        .expect("expression `Red` resolves");
    assert_eq!(e_file, 1, "expression `Red` is file 1's value");
    assert_eq!(e_def.range, range_of(src1, "Red"), "file 1's `let Red`");

    // Pattern `match x with Red` (in file 1) → file 0's case.
    let pat = {
        let i = src1.find("Red ->").expect("pattern");
        TextRange::new(
            u32::try_from(i).unwrap().into(),
            u32::try_from(i + 3).unwrap().into(),
        )
    };
    let (p_file, p_def) = proj
        .file(1)
        .resolution_at(pat)
        .and_then(|r| proj.item_def(r))
        .expect("pattern `Red` resolves");
    assert_eq!(p_file, 0, "pattern `Red` is file 0's case");
    assert_eq!(
        p_def.range,
        range_of(src0, "Red"),
        "file 0's `type T = Red`"
    );
}

#[test]
fn cross_file_case_module_collision_dotted_head_defers() {
    // The round-6 residual, now cross-file: `open Lib; Red.foo` where Lib (earlier
    // file) has both a case `Red` and a `module Red`. FCS resolves `Red.foo`
    // through the module (`Lib.Red.foo`); under the opaque `open Lib` we cannot, so
    // the head `Red` must defer — never record the cross-file case (a wrong
    // go-to-def). Cross-file case-kind lets the fallback recognize the opened case
    // and defer.
    let src0 = "module Lib\ntype T = Red | Blue\nmodule Red =\n    let foo = 1\n";
    let src1 = "module Client\nopen Lib\nlet x = Red.foo\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    let i = src1.find("Red.foo").expect("dotted head");
    let head = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    // `Red` must not resolve to file0's case (occurrence: `type T = Red`).
    let to_case = rf
        .resolution_at(head)
        .and_then(|r| proj.item_def(r))
        .is_some_and(|(idx, def)| idx == 0 && def.range == range_of(src0, "Red"));
    assert!(
        !to_case,
        "the dotted head `Red` must not resolve to the cross-file case"
    );
}

#[test]
fn require_qualified_case_is_not_opened_cross_file() {
    // A `[<RequireQualifiedAccess>]` union's cases are reachable only as
    // `Color.Red`, never bare `Red` even after `open`. So an opened bare `Red`
    // must NOT resolve cross-file (FCS reports FS0039).
    let src0 = "module Lib\n[<RequireQualifiedAccess>]\ntype Color = Red | Green\n";
    let src1 = "module Client\nopen Lib\nlet x = Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("Red").expect("`Red` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    match proj.file(1).resolution_at(use_range) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("a require-qualified case must not open cross-file, got {other:?}"),
    }
}
