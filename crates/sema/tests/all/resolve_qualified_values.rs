//! FCS-free tests for **qualified value references** through a project module —
//! `Mod.value`, `A.B.value`, `Alias.value` — resolved by name-shortening the
//! module *prefix* (relative to the enclosing namespace / opens, following module
//! abbreviations) and looking the value up under it, same-file or in an earlier
//! Compile-order file.
//!
//! FCS reports the value use spanning the **whole** dotted path (`Target.foo` →
//! `Demo.Target.foo`), so the resolution is recorded at that whole range; the
//! module qualifier segments are left `Deferred` (no module-as-def model).

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, ProjectItems, Resolution, ResolvedFile, resolve_file, resolve_project,
};
use rowan::TextRange;

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

/// The byte range of the `n`th (0-based) occurrence of `needle` in `src`.
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

/// Assert the (single) qualified use written `whole` in `src` resolves to the
/// in-file binder at the `def_idx`-th occurrence of `def_needle`.
fn assert_qualified(src: &str, whole: &str, def_needle: &str, def_idx: usize) {
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, whole, 0))
        .unwrap_or_else(|| panic!("no resolution at {whole:?} in {src:?}"));
    let def = rf
        .resolved_def(res)
        .unwrap_or_else(|| panic!("{whole:?} names no in-file def in {src:?}"));
    assert_eq!(
        def.range,
        nth(src, def_needle, def_idx),
        "{whole:?} points at the wrong def in {src:?}"
    );
}

#[test]
fn qualified_value_in_sibling_module_resolves() {
    // `Target.foo` (relative to `namespace Demo`) → `Demo.Target.foo`.
    let src =
        "namespace Demo\nmodule Target =\n    let foo = 1\nmodule N =\n    let x = Target.foo\n";
    assert_qualified(src, "Target.foo", "foo", 0);
}

#[test]
fn qualified_value_through_a_nested_module_chain_resolves() {
    // `A.B.foo` where `A.B` is a nested module → `Demo.A.B.foo`.
    let src = "namespace Demo\nmodule A =\n    module B =\n        let foo = 1\nmodule N =\n    let x = A.B.foo\n";
    assert_qualified(src, "A.B.foo", "foo", 0);
}

#[test]
fn qualified_value_through_an_alias_resolves() {
    // `Alias.foo` where `module Alias = Target` → `Demo.Target.foo` (the prefix is
    // name-shortened through the alias).
    let src = "namespace Demo\nmodule Target =\n    let foo = 1\nmodule N =\n    module Alias = Target\n    let x = Alias.foo\n";
    assert_qualified(src, "Alias.foo", "foo", 0);
}

#[test]
fn qualified_value_shortened_by_an_open_resolves() {
    // `open Demo; Target.foo` → `Demo.Target.foo` (the prefix `Target` is
    // shortened by the open).
    let src = "namespace Demo\nmodule Target =\n    let foo = 1\nnamespace Other\nmodule N =\n    open Demo\n    let x = Target.foo\n";
    assert_qualified(src, "Target.foo", "foo", 0);
}

#[test]
fn qualified_self_module_name_defers() {
    // FCS: a module's own simple name is not in scope within itself, so `module M`
    // referencing `M.x` from its own body is FS0039 ("module 'M' is not
    // defined"). We must defer, never resolve `M.x` to the binding being defined
    // (which `prepare_binding` eagerly pushed into `self.items`).
    let src = "namespace Demo\nmodule M =\n    let x = M.x\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "M.x", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("self-module `M.x` should defer, got {other:?}"),
    }
}

#[test]
fn qualified_enclosing_module_name_defers() {
    // FCS: an enclosing module's simple name is not in scope within a nested
    // module either — `Inner` referencing `Outer.v` is FS0039. (Its value `v` is
    // reachable unqualified, but not via the ancestor's name.) Defer.
    let src = "namespace Demo\nmodule Outer =\n    let v = 1\n    module Inner =\n        let y = Outer.v\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Outer.v", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("ancestor `Outer.v` should defer, got {other:?}"),
    }
}

#[test]
fn qualified_self_name_to_descendant_defers() {
    // FCS: using a module's *own name* as the head qualifier to reach a descendant
    // is FS0039 — `Outer.Inner.y` from within `Outer` is rejected (the head
    // `Outer` is out of scope), even though `Inner.y` (head `Inner`) resolves. The
    // self/ancestor check is on the *head* segment, not the full resolved prefix.
    let src = "namespace Demo\nmodule Outer =\n    module Inner =\n        let y = 1\n    let z = Outer.Inner.y\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Outer.Inner.y", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("self-name-headed `Outer.Inner.y` should defer, got {other:?}"),
    }
}

#[test]
fn qualified_sibling_to_descendant_resolves() {
    // Control: the same `Outer.Inner.y` from a *sibling* `module N` resolves
    // (`Demo.Outer.Inner.y`) — the head `Outer` is a sibling, in scope.
    let src = "namespace Demo\nmodule Outer =\n    module Inner =\n        let y = 1\nmodule N =\n    let z = Outer.Inner.y\n";
    assert_qualified(src, "Outer.Inner.y", "y", 0);
}

#[test]
fn qualified_descendant_module_value_resolves() {
    // The flip side: a module *can* reference its own nested (descendant) module
    // by name — `Outer` referencing `Inner.y` resolves to `Demo.Outer.Inner.y`.
    // (Sibling and descendant prefixes are in scope; only self/ancestor are not.)
    let src = "namespace Demo\nmodule Outer =\n    module Inner =\n        let y = 1\n    let z = Inner.y\n";
    assert_qualified(src, "Inner.y", "y", 0);
}

#[test]
fn qualified_value_defers_while_a_project_module_is_open() {
    // Conservative (codex review round 6): while a plain `open <project module>`
    // is in scope (`open A` sets `opaque_dotted_open`), a qualified value whose
    // head is name-shortened relative to the enclosing scope is deferred — the
    // open could supply the head from content we do not model (a submodule, or a
    // type from a namespace the module merges with), so resolving it risks the
    // wrong target. FCS would resolve `Demo.A.B.foo` here, but we cannot prove the
    // absence of a shadowing collision, so we defer (sound; a coverage gap, never a
    // wrong go-to-definition).
    let src = "namespace Demo\nmodule A =\n    module B =\n        let foo = 1\nmodule N =\n    open A\n    let x = B.foo\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "B.foo", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("`B.foo` under an opaque project-module open should defer, got {other:?}"),
    }
}

#[test]
fn same_file_namespace_qualified_value_defers() {
    // FCS-pinned: a *same-file* fully-namespace-qualified reference is FS0039 —
    // `Demo.Target.foo` from a sibling `module N` under `namespace Demo` is
    // rejected (the enclosing namespace's own name is not in scope within the same
    // file; only an *earlier file* establishes `Demo` as referenceable, see
    // `cross_file_namespace_qualified_value_resolves`). We defer, matching FCS.
    let src = "namespace Demo\nmodule Target =\n    let foo = 1\nmodule N =\n    let x = Demo.Target.foo\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Demo.Target.foo", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("same-file `Demo.Target.foo` should defer, got {other:?}"),
    }
}

#[test]
fn qualified_value_defers_with_a_colliding_opened_union_case() {
    // `open M` (M has `type T = Sub | …`) brings the union case `Sub`, and a
    // sibling `module Sub` exports `foo`. FCS resolves `Sub.foo` to the *module*
    // value `Demo.Sub.foo` (the module path wins over the opened case). We defer:
    // `open M` is a project-module open (`opaque_dotted_open`), so under the
    // conservative policy the relative qualified-value resolution stands down — an
    // open could supply the head from content we cannot model. Sound (never the
    // wrong target / the case); a coverage gap.
    let src = "namespace Demo\nmodule Sub =\n    let foo = 1\nmodule M =\n    type T = Sub | Other\nmodule N =\n    open M\n    let x = Sub.foo\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "Sub.foo", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => {
            panic!("`Sub.foo` under an opaque project-module open should defer, got {other:?}")
        }
    }
}

#[test]
fn cross_file_namespace_qualified_value_resolves() {
    // Regression (codex review): an earlier file exports `Demo.Target.foo`; a
    // later file *under `namespace Demo`* references it by the full path
    // `Demo.Target.foo`. FCS's project oracle resolves this — the enclosing
    // namespace's name is referenceable cross-file (an earlier file established
    // it), unlike the same-file relative self/ancestor cases. The exact cross-file
    // qualified-export lookup must still fire (the head-guard, which suppresses
    // same-file self-names, must not reject it).
    let src1 = "namespace Demo\nmodule Target =\n    let foo = 1\n";
    let src2 = "namespace Demo\nmodule N =\n    let x = Demo.Target.foo\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());
    let i = src2.find("Demo.Target.foo").expect("use");
    let whole = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "Demo.Target.foo".len()).unwrap().into(),
    );
    let res = proj
        .file(1)
        .resolution_at(whole)
        .expect("resolution at Demo.Target.foo");
    let (file_idx, def) = proj.item_def(res).expect("cross-file item");
    assert_eq!(file_idx, 0, "resolves into the earlier file");
    let fi = src1.find("foo").expect("def");
    assert_eq!(
        def.range,
        TextRange::new(
            u32::try_from(fi).unwrap().into(),
            u32::try_from(fi + 3).unwrap().into()
        )
    );
}

// NOTE: the root-qualified form `global.Demo.Target.foo` is intentionally not
// tested here — `global` is not yet a supported *expression* atom in the parser
// (see the cst crate's `global_headed_app_source_does_not_panic`), so such a
// reference does not parse and never reaches the qualified-value resolver. It can
// be covered once the parser accepts `global`-headed expressions.

#[test]
fn qualified_value_cross_file_still_resolves() {
    // Regression: a cross-file qualified value (`Shared.foo`, `Shared` a root
    // module in an earlier file) resolves to the earlier file's binder.
    let src1 = "module Shared\nlet foo = 1\n";
    let src2 = "module Other\nlet bar = Shared.foo\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());
    let i = src2.find("Shared.foo").expect("use");
    let whole = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "Shared.foo".len()).unwrap().into(),
    );
    let res = proj
        .file(1)
        .resolution_at(whole)
        .expect("resolution at Shared.foo");
    let (file_idx, def) = proj.item_def(res).expect("cross-file item");
    assert_eq!(file_idx, 0);
    let fi = src1.find("foo").expect("def");
    assert_eq!(
        def.range,
        TextRange::new(
            u32::try_from(fi).unwrap().into(),
            u32::try_from(fi + 3).unwrap().into()
        )
    );
}

#[test]
fn qualified_value_in_anonymous_root_file_defers() {
    // An anonymous-root file's nested modules carry no qualified export path, so a
    // same-file `M.foo` cannot be resolved — it defers (sound; FCS resolves it via
    // the unmodelled filename module).
    let src = "module M =\n    let foo = 1\nmodule N =\n    let x = M.foo\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "M.foo", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("anonymous-root `M.foo` should defer, got {other:?}"),
    }
}

/// Assert the whole `whole` span in `src1` resolves to file0's `def_needle` value.
fn assert_cross_file_value(src0: &str, src1: &str, whole: &str, def_needle: &str) {
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, whole, 0))
        .unwrap_or_else(|| panic!("no resolution at {whole:?}"));
    let (file_idx, def) = proj
        .item_def(res)
        .unwrap_or_else(|| panic!("{whole:?} is not a cross-file item: {res:?}"));
    assert_eq!(file_idx, 0, "{whole:?} resolves into file0");
    assert_eq!(
        def.range,
        nth(src0, def_needle, 0),
        "{whole:?} → file0's def"
    );
}

#[test]
fn cross_file_dotted_head_into_namespace_nested_module_resolves_through_open() {
    // Regression: a value in a module nested under a *namespace* (`module Sub` in
    // `namespace Lib`) referenced cross-file via `open Lib; Sub.foo` resolves to it
    // (FCS: `Lib.Sub.foo`). (Once thought a deferred gap; it resolves through the
    // existing qualified-value machinery — `open Lib` is a *namespace* open, which
    // does not set the conservative dotted-head flag a module open would.)
    assert_cross_file_value(
        "namespace Lib\nmodule Sub =\n    let foo = 1\n",
        "module Client\nopen Lib\nlet x = Sub.foo\n",
        "Sub.foo",
        "foo",
    );
}

#[test]
fn cross_file_fully_qualified_namespace_nested_module_value_resolves() {
    // The fully-qualified `Lib.Sub.foo` (no open, plain consumer) resolves too.
    assert_cross_file_value(
        "namespace Lib\nmodule Sub =\n    let foo = 1\n",
        "module Client\nlet x = Lib.Sub.foo\n",
        "Lib.Sub.foo",
        "foo",
    );
}

#[test]
fn cross_file_dotted_module_namespace_nested_value_resolves() {
    // The dotted top-level-module producer form (`module Lib.Sub`, where `Lib` is an
    // implicit namespace) resolves the same way, opened and fully-qualified.
    assert_cross_file_value(
        "module Lib.Sub\nlet foo = 1\n",
        "module Client\nopen Lib\nlet x = Sub.foo\n",
        "Sub.foo",
        "foo",
    );
    assert_cross_file_value(
        "module Lib.Sub\nlet foo = 1\n",
        "module Client\nlet x = Lib.Sub.foo\n",
        "Lib.Sub.foo",
        "foo",
    );
}

/// A same-file `` ``global`` `` value binder must NOT capture the `global`
/// namespace-root *marker* head. Both normalise to the text `global` (the
/// binder's backticks are stripped by `id_text`), but FCS treats a raw `global`
/// as the root marker — never a value use — exactly as it treats `base`. The
/// marker head must therefore resolve to no in-file def (it defers; the rooted
/// tail is resolved from the root elsewhere). Regression for the sema half of
/// the `global` qualified-root feature (parser side: PR #700).
#[test]
fn global_root_marker_not_captured_by_backtick_binding() {
    let src = "let ``global`` = 1\nlet y = global.System.Object\n";
    let rf = resolve(src);
    // The `global` *keyword* head is the 2nd occurrence of "global" (the 1st is
    // the text inside the binder's backticks).
    let head = nth(src, "global", 1);
    let res = rf.resolution_at(head);
    assert!(
        res.is_none_or(|r| rf.resolved_def(r).is_none()),
        "the `global` marker must not resolve to an in-file def, got {res:?}",
    );
}

/// A `global`-rooted qualified value (`global.Lib.foo`) resolves through the
/// root-namespace marker to the same target as the unrooted `Lib.foo` — the
/// leading `global` segment is stripped and the remainder resolved from the
/// root. Confirms the marker is *transparent* to resolution (not merely
/// non-capturing): the positive complement to
/// `global_root_marker_not_captured_by_backtick_binding`.
#[test]
fn global_rooted_qualified_value_resolves_cross_file() {
    assert_cross_file_value(
        "module Lib\nlet foo = 1\n",
        "module Client\nlet x = global.Lib.foo\n",
        "global.Lib.foo",
        "foo",
    );
}

// ---- Value accessibility on the cross-file qualified-value path ----

#[test]
fn a_qualified_private_value_is_inaccessible_cross_file_from_unrelated_code() {
    // `let private secret` in `module Lib.M`; from an UNRELATED module,
    // `Lib.M.secret` is FS1094 in FCS — inaccessible, so unbound. The cross-file
    // qualified-value lookup (`lookup_qualified_path`) was accessibility-blind and
    // resolved the private value — a wrong target on `main`.
    let src0 = "module Lib.M\n\nlet private secret = 1\n";
    let src1 = "module Other\n\nlet x = Lib.M.secret\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    match proj.file(1).resolution_at(nth(src1, "Lib.M.secret", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "an inaccessible qualified private value must not resolve cross-file, got {other:?}"
        ),
    }
}

#[test]
fn a_qualified_public_value_still_resolves_cross_file() {
    // The gate must not over-reach: a PUBLIC qualified value still resolves
    // cross-file (guards against filtering a non-`private` binding).
    let src0 = "module Lib.M\n\nlet answer = 42\n";
    let src1 = "module Other\n\nlet x = Lib.M.answer\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Lib.M.answer", 0))
        .expect("the public qualified value resolves cross-file");
    let (file_idx, _) = proj.item_def(res).expect("a cross-file value item");
    assert_eq!(file_idx, 0, "resolves to Lib.M.answer in file0");
}

#[test]
fn a_public_qualified_value_survives_a_later_private_redeclaration() {
    // Codex-pinned (fcs-dump): a public `let answer` shadowed by a later `let
    // private answer` at the same path is still bound from outside — FCS resolves
    // `Lib.M.answer` to the EARLIER public binding (FS1094 only when no accessible
    // binding remains). The gate must select the latest *accessible* record, not
    // just filter the last one to `None`.
    let src0 = "module Lib.M\n\nlet answer = 1\n\nlet private answer = 2\n";
    let src1 = "module Other\n\nlet x = Lib.M.answer\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Lib.M.answer", 0))
        .expect("the surviving public binding resolves");
    let (file_idx, def) = proj.item_def(res).expect("a cross-file value item");
    assert_eq!(file_idx, 0);
    // The public `answer` is the first occurrence in file0 (line 3), not the
    // later `private` one.
    assert_eq!(
        def.range,
        nth(src0, "answer", 0),
        "binds the earlier public `answer`"
    );
}

#[test]
fn a_same_file_qualified_private_value_is_inaccessible_from_a_sibling() {
    // `let private secret` in `module A`; a SIBLING `module B` references
    // `A.secret` in the SAME file. FCS reports FS1094 — a `private` value is
    // accessible only within its module's subtree, not a sibling. The same-file
    // qualified-value lookup (`self.items`) was accessibility-blind and resolved
    // the private value — a wrong target on `main` (the same-file parallel to the
    // cross-file `lookup_qualified_path` gate).
    let src = "module Lib\n\nmodule A =\n    let private secret = 1\n\nmodule B =\n    let y = A.secret\n";
    let rf = resolve(src);
    match rf.resolution_at(nth(src, "A.secret", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "a same-file qualified private value must not resolve from a sibling, got {other:?}"
        ),
    }
}

#[test]
fn a_same_file_qualified_public_value_still_resolves_from_a_sibling() {
    // The gate must not over-reach: a PUBLIC value is reachable from a sibling via
    // the qualified path (a `private` value never is — from within its own module
    // the qualifier `A` is FS0039 by the own-name rule, from a sibling it is
    // FS1094 — so `A.public` is the over-reach guard).
    let src = "module Lib\n\nmodule A =\n    let answer = 1\n\nmodule B =\n    let y = A.answer\n";
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, "A.answer", 0))
        .expect("a public sibling value resolves");
    let def = rf.resolved_def(res).expect("in-file def");
    assert_eq!(def.range, nth(src, "answer", 0), "binds A's public answer");
}
