//! FCS-free tests for in-project **module-value opens** (`open M`, substep 3):
//! a plain `open` of a project module brings that module's direct `let` values
//! into unqualified scope, as source-ordered *opened* entries that participate
//! in the one latest-wins frame alongside locals and `open type` statics.
//!
//! The behaviours pinned here were probed against FCS (the oracle):
//! - a bare use after `open Shared` resolves to `Shared`'s value;
//! - opens and locals interleave by source order, latest wins (rows a/b);
//! - a *dotted* head through the open (`Sub.bar`, naming a submodule we do not
//!   model) stays conservative — it defers, never mis-resolves;
//! - the same holds across Compile-order files (`resolve_project`);
//! - opened entries do **not** leak into a sibling block (opens are per-block).
//!
//! Identifiers avoid appearing as substrings of F# keywords so the
//! `nth`-occurrence needle lands on the intended token.

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
        "snippet has parse errors: {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
}

fn impl_file(src: &str) -> ImplFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors: {src:?}: {:?}",
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

/// Assert the use of `needle` at occurrence `use_idx` resolves to a binder at
/// occurrence `def_idx` (same file).
fn assert_use(src: &str, needle: &str, use_idx: usize, def_idx: usize) {
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, needle, use_idx))
        .unwrap_or_else(|| panic!("no resolution at {needle:?} use ({use_idx}) in {src:?}"));
    let def = rf
        .resolved_def(res)
        .unwrap_or_else(|| panic!("{needle:?} use ({use_idx}) names no in-file def in {src:?}"));
    assert_eq!(
        def.range,
        nth(src, needle, def_idx),
        "{needle:?} use ({use_idx}) points at the wrong def in {src:?}"
    );
}

/// Assert the use of `needle` at occurrence `use_idx` does **not** resolve to an
/// in-file binder — it is `Deferred` (or unrecorded), the honest "say nothing".
fn assert_deferred(src: &str, needle: &str, use_idx: usize) {
    let rf = resolve(src);
    match rf.resolution_at(nth(src, needle, use_idx)) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("{needle:?} use ({use_idx}) should defer in {src:?}, got {other:?}"),
    }
}

#[test]
fn open_project_module_brings_bare_value_into_scope() {
    // `module N` opens the sibling `module Shared`, so the bare `foo` resolves to
    // `Shared`'s `let foo` (occurrence 0), not deferred.
    assert_use(
        "namespace Demo\nmodule Shared =\n    let foo = 42\nmodule N =\n    open Shared\n    let y = foo\n",
        "foo",
        1,
        0,
    );
}

#[test]
fn later_open_module_shadows_earlier_local() {
    // Row a: a local `let foo = 99` (occ 1) then `open Shared` (whose `foo` is
    // occ 0) — the *later* open wins, so the use (occ 2) is `Shared.foo`.
    assert_use(
        "namespace Demo\nmodule Shared =\n    let foo = 42\nmodule N =\n    let foo = 99\n    open Shared\n    let y = foo\n",
        "foo",
        2,
        0,
    );
}

#[test]
fn later_local_shadows_earlier_open_module() {
    // Row b: `open Shared` (whose `foo` is occ 0) then a local `let foo = 99`
    // (occ 1) — the *later* local wins, so the use (occ 2) is the local.
    assert_use(
        "namespace Demo\nmodule Shared =\n    let foo = 42\nmodule N =\n    open Shared\n    let foo = 99\n    let y = foo\n",
        "foo",
        2,
        1,
    );
}

#[test]
fn dotted_head_through_open_module_defers() {
    // `open Shared` brings `Shared`'s direct *values* into scope, but `Sub` is a
    // *submodule* we do not model. A dotted head `Sub.bar` must stay conservative
    // — defer, never mis-resolve to an assembly/cross-file path.
    let src = "namespace Demo\nmodule Shared =\n    module Sub =\n        let bar = 7\nmodule N =\n    open Shared\n    let y = Sub.bar\n";
    assert_deferred(src, "Sub", 0);
    assert_deferred(src, "bar", 1);
}

#[test]
fn open_project_module_resolves_cross_file_value() {
    // `open Shared` in a *later* file brings the earlier file's `module Shared`
    // value `foo` into unqualified scope.
    let src1 = "module Shared\nlet foo = 1\n";
    let src2 = "module Other\nopen Shared\nlet y = foo\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());

    let i = src2.rfind("foo").expect("`foo` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    let res = proj
        .file(1)
        .resolution_at(use_range)
        .expect("a resolution at bare `foo`");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("bare `foo` resolves to a cross-file item");
    assert_eq!(file_idx, 0, "declared in file1");
    let di = src1.find("foo").expect("`foo` def");
    assert_eq!(
        def.range,
        TextRange::new(
            u32::try_from(di).unwrap().into(),
            u32::try_from(di + 3).unwrap().into(),
        ),
        "points at file1's `foo` binder"
    );
}

#[test]
fn dotted_head_through_open_module_does_not_mis_route_cross_file() {
    // Soundness: file0 exports `Sub.bar`. file1 opens a project module `Shared`
    // that has its *own* submodule `Sub`, then writes `Sub.bar`. FCS resolves
    // that to `Demo.Shared.Sub.bar` (in file1). We must NOT route it to file0's
    // colliding `Sub.bar` — `opaque_dotted_open` keeps the dotted head conservative
    // (we defer; never a wrong cross-file Item).
    let src1 = "module Sub\nlet bar = 99\n";
    let src2 = "namespace Demo\nmodule Shared =\n    module Sub =\n        let bar = 7\nmodule N =\n    open Shared\n    let y = Sub.bar\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());
    let rf = proj.file(1);

    // Nothing in `Sub.bar` may resolve to a cross-file (file0) item.
    for (range, res) in rf.resolutions() {
        if let Some((idx, _)) = proj.item_def(*res) {
            assert_ne!(
                idx, 0,
                "use at {range:?} mis-routed to file0 (cross-file collision)"
            );
        }
    }
}

#[test]
fn opening_a_module_alias_resolves_to_the_target() {
    // FCS: `open Other` (has `foo`) then `open Alias` (= `Target`, which also has
    // `foo`) — the alias resolves to its target and the *later* open wins, so the
    // bare `foo` is `Demo.Target.foo`. We model the alias, so `foo` resolves to
    // `Target`'s binder (occurrence 0), never the earlier `open Other`'s
    // (occurrence 1).
    let src = "namespace Demo\nmodule Target =\n    let foo = 5\nmodule Other =\n    let foo = 9\nmodule N =\n    module Alias = Target\n    open Other\n    open Alias\n    let y = foo\n";
    // `foo`: def in Target (0), def in Other (1), use (2).
    assert_use(src, "foo", 2, 0);
}

#[test]
fn cross_file_alias_open_stays_conservative() {
    // Soundness (codex review): an alias declared in an *earlier* file is not
    // followed (cross-file alias resolution is a follow-up), but it must stay
    // conservative — a later file's `open Demo.Alias` must NOT leave an earlier
    // `open Demo.Other`'s value resolving. file0 has `Other.foo` and `Target.foo`
    // and `Alias = Target`; file1 opens Other then Alias and uses `foo`. F# opens
    // the alias target (`Demo.Target.foo`); we cannot resolve that cross-file, but
    // `foo` must defer, never resolve to `Demo.Other.foo` (file0's `foo = 9`).
    let src0 = "namespace Demo\nmodule Other =\n    let foo = 9\nmodule Target =\n    let foo = 5\nmodule Alias = Target\n";
    let src1 =
        "namespace App\nmodule N =\n    open Demo.Other\n    open Demo.Alias\n    let y = foo\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("foo").expect("`foo` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    // If we resolve it at all, it must not be file0's `Other.foo` (the `foo = 9`).
    if let Some(res) = proj.file(1).resolution_at(use_range)
        && let Some((file_idx, def)) = proj.item_def(res)
    {
        let other_foo = src0.find("foo").expect("Other.foo def");
        let is_other_foo =
            file_idx == 0 && def.range.start() == u32::try_from(other_foo).unwrap().into();
        assert!(
            !is_other_foo,
            "cross-file `open Demo.Alias` leaked the earlier `open Demo.Other`'s `foo`"
        );
    }
}

#[test]
fn inner_type_does_not_shadow_an_outer_module_alias_for_open() {
    // FCS-pinned: a nearer *type* does NOT shadow an outer *module abbreviation*
    // for `open` — module abbreviations live in the module/namespace namespace,
    // types in the type namespace, and `open` resolves the former. With `module
    // Alias = Target` in `N` and `type Alias = int; open Alias` in `N.Child`, FCS
    // resolves bare `foo` to `Demo.Target.foo` (no error) — the `open` follows the
    // module alias, ignoring the same-named type. (Only *module-like* declarations
    // — nested modules / abbreviations — shadow an alias; see
    // `inner_module_shadows_an_outer_alias_of_the_same_name`.)
    let src = "namespace Demo\nmodule Target =\n    let foo = 5\nmodule N =\n    module Alias = Target\n    module Child =\n        type Alias = int\n        open Alias\n        let y = foo\n";
    // `foo`: def in Target (0), use in Child (1) — resolves to Target's `foo`.
    assert_use(src, "foo", 1, 0);
}

#[test]
fn inner_module_shadows_an_outer_alias_of_the_same_name() {
    // FCS: a nearer module-like declaration shadows an outer alias of the same
    // name. `module N` aliases `Alias = Target`, but its child `Child` declares
    // its own real `module Alias` (with `foo = 99`); `open Alias` in `Child` opens
    // *that* one, so `foo` is `Demo.N.Child.Alias.foo` (occurrence 1), not the
    // outer alias's `Target.foo` (occurrence 0).
    let src = "namespace Demo\nmodule Target =\n    let foo = 5\nmodule N =\n    module Alias = Target\n    module Child =\n        module Alias =\n            let foo = 99\n        open Alias\n        let y = foo\n";
    // `foo`: def in Target (0), def in inner Alias (1), use (2).
    assert_use(src, "foo", 2, 1);
}

#[test]
fn open_alias_prefixed_submodule_resolves_to_the_target_submodule() {
    // FCS: `module Alias = Target` then `open Alias.Sub` resolves `Alias` (the
    // bare head) through the alias, so it opens `Target.Sub` — bare `bar` is
    // `Demo.Target.Sub.bar`. (An alias is a bare-head lexical name, so an alias
    // *prefix* of an open path is followed.)
    let src = "namespace Demo\nmodule Target =\n    module Sub =\n        let bar = 1\nmodule N =\n    module Alias = Target\n    module M =\n        open Alias.Sub\n        let y = bar\n";
    // `bar`: def in Target.Sub (0), use in M (1).
    assert_use(src, "bar", 1, 0);
}

#[test]
fn qualified_alias_through_its_container_does_not_resolve() {
    // FCS: an alias is not a *member* of its enclosing module, so `open N.Alias`
    // (the alias reached via a qualified path, not as a bare head) is FS0039 even
    // from a child module of `N`. We must defer, never follow the alias — bare
    // `foo` stays unresolved.
    let src = "namespace Demo\nmodule Target =\n    let foo = 1\nmodule N =\n    module Alias = Target\n    module M =\n        open N.Alias\n        let y = foo\n";
    // `foo`: def in Target (0), use in M (1) — must defer.
    assert_deferred(src, "foo", 1);
}

#[test]
fn alias_is_not_visible_outside_its_lexical_scope() {
    // Soundness (codex review): a module abbreviation is *lexically scoped* to its
    // enclosing module — it is not a member accessible by qualified path. `module
    // N` declares `module Alias = Target`; a sibling `module M`'s `open N.Alias`
    // is FS0039 in F# ("namespace 'Alias' is not defined"). We must NOT follow the
    // alias from outside `N`: bare `foo` in `M` must defer, never resolve to
    // `Target.foo`.
    let src = "namespace Demo\nmodule Target =\n    let foo = 5\nmodule N =\n    module Alias = Target\nmodule M =\n    open N.Alias\n    let y = foo\n";
    // `foo`: def in Target (0), use in M (1) — must defer.
    assert_deferred(src, "foo", 1);
}

#[test]
fn open_alias_brings_the_target_modules_value_into_scope() {
    // `module Alias = Target` then `open Alias` brings `Target`'s value `foo` into
    // unqualified scope (the alias is resolved to its target — FCS:
    // `Demo.Target.foo`).
    let src = "namespace Demo\nmodule Target =\n    let foo = 5\nmodule N =\n    module Alias = Target\n    open Alias\n    let y = foo\n";
    // `foo`: def (0), use (1).
    assert_use(src, "foo", 1, 0);
}

#[test]
fn open_through_an_alias_chain_resolves_to_the_final_target() {
    // FCS: alias chains resolve transitively — `module A1 = Target; module A2 =
    // A1; open A2` brings `Target`'s `foo` into scope (`Demo.Target.foo`).
    let src = "namespace Demo\nmodule Target =\n    let foo = 5\nmodule N =\n    module A1 = Target\n    module A2 = A1\n    open A2\n    let y = foo\n";
    // `foo`: def (0), use (1).
    assert_use(src, "foo", 1, 0);
}

#[test]
fn chained_open_of_a_submodule_through_an_alias_resolves() {
    // The declined-then-fixed limitation (codex review 5). `open Other` (has a
    // submodule `Sub`), then `module Alias = Target`, `open Alias`, `open Sub` —
    // FCS chains `open Sub` through the alias's target to `Target.Sub`, the later
    // open winning, so bare `bar` is `Demo.Target.Sub.bar`. Modelling the alias,
    // `bar` resolves to `Target.Sub`'s binder (occurrence 0), never `Other.Sub`'s
    // (occurrence 1).
    let src = "namespace Demo\nmodule Target =\n    module Sub =\n        let bar = 1\nmodule Other =\n    module Sub =\n        let bar = 2\nmodule N =\n    open Other\n    module Alias = Target\n    open Alias\n    open Sub\n    let y = bar\n";
    // `bar`: def in Target.Sub (0), def in Other.Sub (1), use (2).
    assert_use(src, "bar", 2, 0);
}

#[test]
fn ambiguous_module_open_across_two_opens_does_not_mis_resolve() {
    // Codex P2 / FCS: `open A; open B; open Shared` where both `A.Shared` and
    // `B.Shared` exist — FCS picks `Demo.B.Shared.foo` (latest open wins). `A` and
    // `B` here are submodule-only (no direct values), so opening them is opaque;
    // either way the bare `foo` must NOT resolve to `A.Shared.foo` (the earlier
    // open). Conservative deferral is sound.
    let src = "namespace Demo\nmodule A =\n    module Shared =\n        let foo = 1\nmodule B =\n    module Shared =\n        let foo = 2\nmodule N =\n    open A\n    open B\n    open Shared\n    let y = foo\n";
    // `foo`: def in A.Shared (0), def in B.Shared (1), use (2). Must not be the
    // A.Shared one (occurrence 0).
    let rf = resolve(src);
    let res = rf.resolution_at(nth(src, "foo", 2));
    let to_a = matches!(res, Some(r)
        if rf.resolved_def(r).is_some_and(|d| d.range == nth(src, "foo", 0)));
    assert!(
        !to_a,
        "bare `foo` must not resolve to the earlier `open A`'s `Shared.foo`, got {res:?}"
    );
}

#[test]
fn chained_open_of_a_submodule_resolves_through_the_earlier_open() {
    // Codex P2 / FCS: `open Shared; open Sub` resolves `open Sub` as
    // `open Shared.Sub` (chained), and the later open wins — so the bare `bar`
    // (present in both `Shared` and `Shared.Sub`) resolves to `Demo.Shared.Sub.bar`,
    // the submodule's. We model chaining, so `bar` resolves to `Sub`'s binder
    // (occurrence 1), never `Shared`'s (occurrence 0).
    let src = "namespace Demo\nmodule Shared =\n    let bar = 1\n    module Sub =\n        let bar = 7\nmodule N =\n    open Shared\n    open Sub\n    let y = bar\n";
    // `bar`: def in Shared (0), def in Shared.Sub (1), use (2).
    assert_use(src, "bar", 2, 1);
}

#[test]
fn open_module_with_a_union_case_resolves_it_over_an_earlier_open() {
    // FCS: `open Other` (has a value `Red`) then `open M` (whose union type has a
    // case `Red`, plus a value `foo`). FCS resolves bare `Red` to `M`'s case (the
    // later open wins). Now that non-qualified union cases are exported, `open M`
    // enumerates the case `Red`, so it resolves to `M`'s case binder (occurrence 1)
    // — not the earlier `open Other`'s value (occurrence 0). (Previously we could
    // only suppress the earlier open and defer; exporting the case lets us resolve
    // it precisely.) `M`'s own value `foo` resolves too.
    let src = "namespace Demo\nmodule Other =\n    let Red = 1\nmodule M =\n    type T = Red | Blue\n    let foo = 2\nmodule N =\n    open Other\n    open M\n    let a = Red\n    let b = foo\n";
    // `Red`: value def in Other (0), case def in M (1), use (2).
    assert_use(src, "Red", 2, 1);
    // `M`'s own value `foo` resolves (occurrence 1 is the use).
    assert_use(src, "foo", 1, 0);
}

#[test]
fn open_fully_modelled_modules_keep_earlier_values_available() {
    // The flip side of the barrier: when a later `open M` has *no* unmodelled
    // members (only `let`s), it cannot shadow unexpectedly, so an earlier open's
    // value stays resolvable. `open A (let aa); open B (let bb); use aa` resolves.
    let src = "namespace Demo\nmodule A =\n    let aa = 1\nmodule B =\n    let bb = 2\nmodule N =\n    open A\n    open B\n    let y = aa\n";
    // `aa`: def (0), use (1).
    assert_use(src, "aa", 1, 0);
}

#[test]
fn later_namespace_open_wins_over_earlier_module_open() {
    // Codex P2 / FCS: source order is preserved *across* open kinds. `open
    // A.Container` (a project module with submodule `Shared`) then `open B` (a
    // namespace with module `Shared`) then `open Shared` — the later `open B`
    // wins, so `foo` is `B.Shared.foo`, NOT `A.Container.Shared.foo`. The
    // shortening prefixes (module opens and namespace opens) form one
    // source-ordered list, latest first.
    let src_a = "namespace A\nmodule Container =\n    module Shared =\n        let foo = 1\n";
    let src_b = "namespace B\nmodule Shared =\n    let foo = 2\n";
    let src_c = "namespace C\nmodule N =\n    open A.Container\n    open B\n    open Shared\n    let y = foo\n";
    let proj = resolve_project(
        &[impl_file(src_a), impl_file(src_b), impl_file(src_c)],
        &AssemblyEnv::default(),
    );

    let i = src_c.rfind("foo").expect("`foo` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 3).unwrap().into(),
    );
    match proj.file(2).resolution_at(use_range) {
        Some(res) if matches!(res, Resolution::Item(_)) => {
            let (file_idx, _) = proj.item_def(res).expect("item def");
            assert_eq!(
                file_idx, 1,
                "`foo` must resolve to B.Shared (file1, the later open), not A.Container.Shared"
            );
        }
        // Deferring is acceptable (sound) — but resolving to file0 is wrong.
        other => assert!(
            !matches!(other, Some(Resolution::Item(_))),
            "unexpected resolution {other:?}"
        ),
    }
}

#[test]
fn global_qualified_open_resolves_to_the_root_module() {
    // Codex P2 / FCS: `open global.Root` is fully rooted — it must open the *root*
    // `Root` module, not an enclosing `N.Root`. file0 is the root `module Root`;
    // file1's `namespace N` also has a `module Root`, but `Inner`'s
    // `open global.Root` brings file0's `v` into scope (FCS: decl in file0).
    let src1 = "module Root\nlet v = 1\n";
    let src2 = "namespace N\nmodule Root =\n    let v = 2\nmodule Inner =\n    open global.Root\n    let y = v\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());

    let i = src2.rfind("v").expect("`v` use"); // `let y = v`
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    let res = proj
        .file(1)
        .resolution_at(use_range)
        .expect("a resolution at bare `v`");
    let (file_idx, _) = proj
        .item_def(res)
        .expect("`v` resolves to a cross-file item (the root module)");
    assert_eq!(
        file_idx, 0,
        "`open global.Root` must resolve `v` to the root module in file0, not N.Root in file1"
    );
}

#[test]
fn open_does_not_leak_into_a_sibling_block() {
    // `open Demo.Shared` lives in `module A` of the first `namespace Other`
    // block, so `foo` there resolves (occ 0). The *second* `namespace Other`
    // block's `module B` has no open, so its bare `foo` must NOT resolve — opens
    // are scoped to one block (FCS reports FS0039 there).
    let src = "namespace Demo\nmodule Shared =\n    let foo = 1\nnamespace Other\nmodule A =\n    open Demo.Shared\n    let a = foo\nnamespace Other\nmodule B =\n    let b = foo\n";
    // `foo`: def (occ 0), use in A (occ 1), use in B (occ 2).
    assert_use(src, "foo", 1, 0);
    assert_deferred(src, "foo", 2);
}

/// Codex review of §7's machinery slice (`docs/assembly-module-open-plan.md`): a
/// project namespace's own case can be shadowed by a *later-folded*
/// `[<AutoOpen>]` submodule's unenumerable content — an active pattern's cases
/// are never cross-file exported at all
/// (`Resolver::module_has_hidden_values`'s doc), so `namespace Probe`'s own
/// `exception Red` folding *before* `Sub`'s `(|Red|_|)` does not mean the
/// exception wins in pattern position. fcs-dump confirms FCS binds the active
/// pattern (`Probe.Sub.(|Red|_|).Red`); sema cannot name an active pattern at
/// all, so it must at least DEFER the namespace's own exception rather than
/// wrongly commit it. This needs the generation barrier raised **between** the
/// namespace's own push and the hidden submodule's recursive push — bumping
/// once, upfront, before the namespace's own case is even pushed, would stamp
/// that case with the bumped generation too and it would never go stale.
#[test]
fn an_auto_open_submodule_active_pattern_wins_over_an_earlier_same_named_exception() {
    // FCS-verified (`uses-project`, diagnostics-clean): `open Probe` folds the
    // namespace's `exception Red` and *then* its `[<AutoOpen>] module Sub`'s
    // active pattern `(|Red|_|)`, which — folded later — WINS in pattern position.
    // FCS binds the pattern `Red` to `Probe.Sub.(|Red|_|).Red`, not the exception.
    //
    // Stage 3a: the AP case is now enumerable cross-file (the narrowed AP
    // hidden-value trigger), so the AutoOpen-fragment fold brings it into pattern
    // scope and we resolve to it — where we used to defer (the AP was unenumerable
    // and staled the exception). The resolution now agrees with FCS exactly.
    let src1 = "namespace Probe\n\nexception Red of int\n\n[<AutoOpen>]\nmodule Sub =\n    let (|Red|_|) (x: int) = if x = 0 then Some () else None\n";
    let src2 = "open Probe\nlet f x =\n    match x with\n    | Red -> 1\n    | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());

    let i = src2.rfind("Red").expect("`Red` pattern use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "Red".len()).unwrap().into(),
    );
    let res = proj
        .file(1)
        .resolution_at(use_range)
        .expect("`Red` pattern resolves");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("`Red` resolves to the AutoOpen submodule's active pattern");
    assert_eq!(file_idx, 0, "the recognizer is declared in file0");
    let ap = src1.find("|Red|_|").expect("recognizer name span");
    assert_eq!(
        def.range,
        TextRange::new(
            u32::try_from(ap).unwrap().into(),
            u32::try_from(ap + "|Red|_|".len()).unwrap().into(),
        ),
        "points at `Sub.(|Red|_|)`, not the earlier `exception Red`"
    );
}

/// Codex review round 2 of §7's machinery slice: `project_namespace_contestant_names`
/// flattened every descendant's constructible type names into ONE list, losing which
/// source folds later — so it could feed the assembly-facing collision check, but
/// nothing made a LATER auto-open submodule's own type evict an EARLIER submodule's
/// value, both purely project-side. fcs-dump confirms FCS binds the LATER-folded
/// `type Clash()` (module `B`), not the EARLIER `let Clash` (module `A`), after
/// `open Probe`. Fixed by pushing a `Deferred` override for each source's own
/// constructible type names *before* that source's own value push, per recursion step
/// (`Self::open_project_namespace_values`) — so it reaches an earlier source's
/// already-pushed value but never out-races its own container's value (F#'s tycon
/// tier folds before that SAME container's vals).
#[test]
fn a_later_auto_open_submodules_type_evicts_an_earlier_ones_value() {
    let src1 = "namespace Probe\n\n[<AutoOpen>]\nmodule A =\n    let Clash () = 1\n\n[<AutoOpen>]\nmodule B =\n    type Clash() =\n        member _.X = 2\n";
    let src2 = "open Probe\nlet y = Clash ()\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());

    let i = src2.find("Clash").expect("`Clash` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "Clash".len()).unwrap().into(),
    );
    match proj.file(1).resolution_at(use_range) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "module `B`'s later-folded `type Clash` must evict module `A`'s earlier \
             `let Clash` value (FCS binds the type after `open Probe`); sema does not \
             model project type constructors, so the bare use must at least defer, \
             never wrongly commit `A`'s value — got {other:?}"
        ),
    }
}

/// Codex review of §7's machinery slice: an explicit `open` of a project namespace
/// folds the CURRENT (opening) file's own `[<AutoOpen>]` submodule *after* an
/// earlier file's same-named one — Q14-style, the opening file's own fragment
/// folds last. fcs-dump-verified (isolated from a same-namespace enclosing
/// block, which uses a different, non-`open`-driven auto-open channel with its
/// own precedence): `Probe.Alpha.clash`, not `Probe.Zebra.clash`.
#[test]
fn current_files_own_auto_open_submodule_wins_over_an_earlier_files() {
    let src0 = "namespace Probe\n\n[<AutoOpen>]\nmodule Zebra =\n    let clash () = 1\n";
    let src1 = "namespace Probe\n\n[<AutoOpen>]\nmodule Alpha =\n    let clash () = 2\n\nnamespace Other\n\nmodule Client =\n    open Probe\n    let y = clash\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let i = src1.rfind("clash").expect("`clash` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "clash".len()).unwrap().into(),
    );
    let res = proj
        .file(1)
        .resolution_at(use_range)
        .expect("a resolution at bare `clash`");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("bare `clash` resolves to a cross-file item");
    assert_eq!(
        file_idx, 1,
        "the CURRENT file's own auto-open submodule (Alpha) must win over an earlier \
         file's (Zebra) — the opening file's own fragment folds last"
    );
    let di = src1
        .find("clash")
        .expect("`clash` def (Alpha's, textually first in file1)");
    assert_eq!(
        def.range,
        TextRange::new(
            u32::try_from(di).unwrap().into(),
            u32::try_from(di + "clash".len()).unwrap().into(),
        ),
        "points at Alpha's `clash` binder, not Zebra's"
    );
}

/// Codex review of §7's machinery slice: [`ProjectItems::auto_open_module_paths`]
/// used to be a `HashSet`, so when two DIFFERENT preceding files each declare a
/// same-named-clashing `[<AutoOpen>]` submodule of one namespace, which one won
/// a later explicit `open`'s fold was the set's nondeterministic iteration
/// order, not Compile order. Fixed to a `Vec` (order-preserving —
/// `ProjectItems::extend_with`'s per-file loop already runs in Compile order).
/// fcs-dump-verified: the LATER-compiled file's submodule wins, symmetric in
/// either declaration order — this pins one direction deterministically.
#[test]
fn later_compile_order_auto_open_submodule_wins_a_collision_across_two_preceding_files() {
    let src0 = "namespace Probe\n\n[<AutoOpen>]\nmodule Mod1 =\n    let clash () = 1\n";
    let src1 = "namespace Probe\n\n[<AutoOpen>]\nmodule Mod2 =\n    let clash () = 2\n";
    let src2 = "namespace Other\n\nmodule Client =\n    open Probe\n    let y = clash\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );

    let i = src2.rfind("clash").expect("`clash` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "clash".len()).unwrap().into(),
    );
    let res = proj
        .file(2)
        .resolution_at(use_range)
        .expect("a resolution at bare `clash`");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("bare `clash` resolves to a cross-file item");
    assert_eq!(
        file_idx, 1,
        "the LATER-compiled preceding file's auto-open submodule (Mod2, file1) must win \
         the collision, not the earlier one (Mod1, file0) and not nondeterministically"
    );
    let di = src1.find("clash").expect("`clash` def in file1");
    assert_eq!(
        def.range,
        TextRange::new(
            u32::try_from(di).unwrap().into(),
            u32::try_from(di + "clash".len()).unwrap().into(),
        ),
        "points at file1's `clash` binder"
    );
}

/// Codex review round 4 of §7's machinery slice: an `[<AutoOpen>]` submodule's values
/// are already visible to the REST of its own enclosing namespace's scope from the
/// submodule's own declaration site — fcs-dump-verified, no `open` needed at all:
/// `namespace N` / `[<AutoOpen>] module A = let x = 1` / `module Client = let y = x`
/// resolves `y` to `A.x`. An explicit `open N`, written INSIDE that same namespace
/// (a redundant self-open), must therefore be a no-op for this recursive fold — it
/// must NOT re-push `A`'s values at the *explicit open's* (later) position, which
/// would wrongly override a local binding declared between the namespace's start and
/// the open (fcs-dump: `let x = 2; open N; let y = x` keeps `Client.x`, not `A.x`).
#[test]
fn a_self_open_of_the_enclosing_namespace_does_not_replay_its_auto_open_at_a_later_position() {
    let src = "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let x = 1\n\nmodule Client =\n    let x = 2\n    open N\n    let y = x\n";
    let rf = resolve(src);

    let i = src.rfind("x").expect("`x` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    let res = rf
        .resolution_at(use_range)
        .expect("a resolution at bare `x`");
    let def = rf
        .resolved_def(res)
        .unwrap_or_else(|| panic!("`x` use names no in-file def, got {res:?}"));
    let local_def_i = src.rfind("let x = 2").expect("Client's `x` def") + "let ".len();
    assert_eq!(
        def.range,
        TextRange::new(
            u32::try_from(local_def_i).unwrap().into(),
            u32::try_from(local_def_i + 1).unwrap().into(),
        ),
        "`open N` inside N must not re-fold A's auto-open value over Client's own local \
         `x` — got a resolution to {:?} instead of Client's local binder",
        def.range
    );
}

/// Codex review round 4 of §7's machinery slice, confirmed via the review's own
/// fcs-dump run: `private` restricts a module to its own enclosing container and that
/// container's descendants — an UNRELATED `namespace Other` in the SAME FILE, opening
/// `N`, does not see `N`'s `[<AutoOpen>] module private A`'s contents (FCS reports the
/// name unbound), even though `project_auto_open_submodules_in`'s same-file branch
/// previously included every same-file submodule regardless of the stored privacy
/// flag. Contrast `resolve_types.rs`'s `private_project_auto_open_module_still_shadows_within_its_own_file`,
/// where the opener IS inside `A`'s own enclosing namespace and the shadow (a
/// different, type-position consumer) correctly still applies.
#[test]
fn a_same_file_private_auto_open_submodule_does_not_shadow_an_unrelated_namespace() {
    let src = "namespace N\n\n[<AutoOpen>]\nmodule private A =\n    let hidden = 1\n\nnamespace Other\n\nmodule Client =\n    open N\n    let y = hidden\n";
    assert_deferred(src, "hidden", 1);
}

/// Codex review round 4 of §7's machinery slice: a project *namespace*, unlike a
/// module, can span multiple Compile-order files, each contributing its own
/// fragment — and `open_module_values(namespace, ..)` aggregates the namespace's
/// direct cases across every file as ONE group, always pushed ahead of every
/// auto-open submodule. fcs-dump confirms that's backward when a name straddles
/// both tiers across files: file0's `[<AutoOpen>] module A = let Clash = 1` (a
/// value) loses to file1's later `exception Clash` (a direct case) after `open
/// Probe`, but the aggregate-then-loop order pushes `A`'s value last, wrongly
/// winning. This crate has no per-name file-provenance to interleave the two
/// files' fragments correctly, so the sound, available choice is to defer a
/// name straddling both tiers rather than guess a winner.
/// Cross-tier straddle **S2**: an auto-open submodule value in file0
/// (`[<AutoOpen>] module A = let Clash = 1`) and the namespace's own direct
/// `exception Clash` in file1. FCS binds the *latest file*'s contribution — the
/// file1 direct exception (`Probe.Clash`, oracle-pinned). The natural push order
/// (direct tier first, submodules after) gets this backward, so the straddle fold
/// re-pushes the direct-tier winner last, ordered by per-name file provenance.
/// (On `main` this deferred, for lack of provenance.)
#[test]
fn a_cross_file_later_direct_case_wins_over_an_earlier_auto_open_value() {
    let src0 = "namespace Probe\n\n[<AutoOpen>]\nmodule A =\n    let Clash = 1\n";
    let src1 = "namespace Probe\n\nexception Clash of int\n";
    let src2 = "namespace Other\n\nmodule Client =\n    open Probe\n    let y = Clash\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let i = src2.rfind("Clash").expect("`Clash` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "Clash".len()).unwrap().into(),
    );
    let res = proj
        .file(2)
        .resolution_at(use_range)
        .expect("a resolution at bare `Clash`");
    let (file_idx, _) = proj
        .item_def(res)
        .expect("the later direct exception resolves cross-file");
    assert_eq!(file_idx, 1, "resolves to file1's direct `exception Clash`");
}

/// The pattern-position counterpart of the S2 straddle: an auto-open submodule
/// `exception Clash` in file0 and the namespace's own direct `exception Clash` in
/// file1. In a `match` pattern (the constructor namespace), FCS still binds the
/// latest file's contribution — file1's direct exception (oracle-pinned). The
/// direct-tier winner is re-pushed as an ordinary entry (a case serves both
/// namespaces), so `case_reference` finds it.
#[test]
fn a_cross_file_later_direct_case_wins_in_pattern_position() {
    let src0 = "namespace Probe\n\n[<AutoOpen>]\nmodule A =\n    exception Clash of int\n";
    let src1 = "namespace Probe\n\nexception Clash of int\n";
    let src2 = "namespace Other\n\nmodule Client =\n    open Probe\n    let f x =\n        match x with\n        | Clash _ -> 1\n        | _ -> 0\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let i = src2.rfind("Clash").expect("`Clash` pattern use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "Clash".len()).unwrap().into(),
    );
    let res = proj
        .file(2)
        .resolution_at(use_range)
        .expect("a resolution at pattern `Clash`");
    let (file_idx, _) = proj
        .item_def(res)
        .expect("the later direct exception resolves cross-file in pattern position");
    assert_eq!(
        file_idx, 1,
        "pattern binds file1's direct `exception Clash`"
    );
}

/// Cross-tier straddle **S1** (the dual of S2): the namespace's own direct
/// `exception Clash` in file0, an auto-open submodule value in file1
/// (`[<AutoOpen>] module A = let Clash = 1`). FCS binds the later file's
/// submodule value `Probe.A.Clash`. Now that `submodule_contributions_at` is
/// per-fragment exact (Stage 5), the fold knows the submodule value genuinely
/// folds at file1 — later than the direct case at file0 — so the natural push
/// order (the submodule after the direct tier) commits it, no defer. (On `main`
/// this deferred, for lack of auto-open surface fold-position provenance.)
#[test]
fn a_cross_file_later_auto_open_value_wins_over_a_direct_case() {
    let src0 = "namespace Probe\n\nexception Clash of int\n";
    let src1 = "namespace Probe\n\n[<AutoOpen>]\nmodule A =\n    let Clash = 1\n";
    let src2 = "namespace Other\n\nmodule Client =\n    open Probe\n    let y = Clash\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let i = src2.rfind("Clash").expect("`Clash` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "Clash".len()).unwrap().into(),
    );
    let res = proj
        .file(2)
        .resolution_at(use_range)
        .expect("a resolution at bare `Clash`");
    let (file_idx, _) = proj
        .item_def(res)
        .expect("the later auto-open submodule value resolves cross-file");
    assert_eq!(
        file_idx, 1,
        "resolves to file1's later `[<AutoOpen>] module A`'s `let Clash`, not the \
         earlier direct `exception Clash`"
    );
}

/// An `[<AutoOpen>]` submodule fragment declared in file0 (`module A`) is
/// AUGMENTED by a later PLAIN (`module A`, no attribute) fragment in file2 that
/// adds `X`; the namespace's own direct `exception X` is in file1. FCS binds
/// file1's direct `N.X` — the file2 plain augmentation is **not** auto-opened, so
/// `A.X` never enters the fold at all. Now that the fold is per-fragment exact
/// (Stage 5), `A.X`@file2 is dropped (its fragment carries no `[<AutoOpen>]`), so
/// `X` is not even a straddle and the direct exception wins outright by its
/// natural push. (On `main` the member-file-2 contribution deferred the
/// expression, for lack of fragment provenance.)
#[test]
fn a_plain_augmented_member_does_not_shadow_a_direct_case() {
    let src0 = "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let Dummy = 0\n";
    let src1 = "namespace N\n\nexception X of int\n";
    let src2 = "namespace N\n\nmodule A =\n    let X = 2\n";
    let src3 = "namespace Z\n\nmodule O =\n    open N\n    let y = X\n";
    let proj = resolve_project(
        &[
            impl_file(src0),
            impl_file(src1),
            impl_file(src2),
            impl_file(src3),
        ],
        &AssemblyEnv::default(),
    );
    let i = src3.rfind("X").expect("`X` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    let res = proj
        .file(3)
        .resolution_at(use_range)
        .expect("a resolution at bare `X`");
    let (file_idx, _) = proj
        .item_def(res)
        .expect("the direct exception resolves cross-file");
    assert_eq!(
        file_idx, 1,
        "the plain (file2) augmentation is not auto-opened, so `open N; X` binds \
         file1's direct `exception X`, not `N.A.X`"
    );
}

/// An `[<AutoOpen>]` submodule contributes an **`extern`** value `X` in a later
/// file (file2) — a value-namespace name sema does not intern, so it is invisible
/// to the straddle fold's per-name file provenance. With an auto-open `A` in
/// file0 and the namespace's own direct `exception X` in file1, FCS binds file2's
/// `N.A.X` after `open N` (the latest auto-open value). The fold's `value_slot`
/// for `X` would understate to file0 (the extern unseen) and wrongly commit
/// file1's direct exception — so an extern-bearing module is marked hidden,
/// making the straddle DEFER (codex review of the straddle slice). Sound: FCS
/// resolves it, we decline; never a wrong target.
#[test]
fn an_extern_bearing_auto_open_submodule_defers_the_straddle() {
    let src0 = "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let Dummy = 0\n";
    let src1 = "namespace N\n\nexception X of int\n";
    let src2 = "namespace N\n\n[<AutoOpen>]\nmodule A =\n    extern int X()\n";
    let src3 = "namespace Z\n\nmodule O =\n    open N\n    let y = X\n";
    let proj = resolve_project(
        &[
            impl_file(src0),
            impl_file(src1),
            impl_file(src2),
            impl_file(src3),
        ],
        &AssemblyEnv::default(),
    );
    let i = src3.rfind("X").expect("`X` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    match proj.file(3).resolution_at(use_range) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "an unindexed `extern` in a later auto-open fragment must not let the \
             straddle commit the direct case — got {other:?}"
        ),
    }
}

/// **S1 in the constructor namespace**: the namespace's own direct `exception
/// Clash` in file0, an `[<AutoOpen>]` submodule *exception* `Clash` in file1.
/// A submodule case is later than the direct case, so it wins BOTH namespaces
/// (oracle: latest file wins) — the expression reads its value slot, the pattern
/// its constructor slot, both `Probe.A.Clash`@file1 by natural push order.
#[test]
fn a_cross_file_later_auto_open_case_wins_a_straddle_in_both_namespaces() {
    let src0 = "namespace Probe\n\nexception Clash of int\n";
    let src1 = "namespace Probe\n\n[<AutoOpen>]\nmodule A =\n    exception Clash of int\n";
    let src2 = "namespace Other\n\nmodule Client =\n    open Probe\n    let y = Clash\n    let f x =\n        match x with\n        | Clash _ -> 1\n        | _ -> 0\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    for (label, use_idx) in [("expression", 0usize), ("pattern", 1usize)] {
        let res = proj
            .file(2)
            .resolution_at(nth(src2, "Clash", use_idx))
            .unwrap_or_else(|| panic!("a resolution at {label} `Clash`"));
        let (file_idx, _) = proj
            .item_def(res)
            .unwrap_or_else(|| panic!("{label} `Clash` resolves cross-file"));
        assert_eq!(
            file_idx, 1,
            "{label} binds the later `[<AutoOpen>] module A`'s `exception Clash`@file1"
        );
    }
}

/// Oracle row (Stage 5): auto-open `A`@file0 (`let Dummy`), a **plain** `module
/// A` augmentation adding `X`@file1, and **no** direct `X` at the namespace tier.
/// FCS leaves `open N; X` unbound — the plain file1 fragment is not auto-opened,
/// so `N.A.X` is never in scope. On `main` this was a wrong target (`X` resolved
/// to the plain fragment's member); the per-fragment gate now drops it.
#[test]
fn a_plain_augmented_member_with_no_direct_case_is_unbound() {
    let src0 = "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let Dummy = 0\n";
    let src1 = "namespace N\n\nmodule A =\n    let X = 2\n";
    let src2 = "namespace Z\n\nmodule O =\n    open N\n    let y = X\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let i = src2.rfind("X").expect("`X` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    match proj.file(2).resolution_at(use_range) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "a plain (non-`[<AutoOpen>]`) augmentation is not auto-opened — \
             `open N; X` must not resolve to `N.A.X`; got {other:?}"
        ),
    }
}

/// Oracle row (Stage 5): auto-open `A`@file0 (`let Dummy`), a **second
/// `[<AutoOpen>]`** `module A` fragment adding `X`@file1, no direct `X`. That
/// fragment folds at its OWN file, so `open N; X` binds `N.A.X`@file1 — the dual
/// of the plain-augmentation case, distinguished only by the attribute on the
/// file1 fragment.
#[test]
fn an_auto_open_augmented_member_is_brought_into_scope() {
    let src0 = "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let Dummy = 0\n";
    let src1 = "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let X = 2\n";
    let src2 = "namespace Z\n\nmodule O =\n    open N\n    let y = X\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let i = src2.rfind("X").expect("`X` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    let res = proj
        .file(2)
        .resolution_at(use_range)
        .expect("a resolution at bare `X`");
    let (file_idx, _) = proj
        .item_def(res)
        .expect("the auto-open augmentation's member resolves cross-file");
    assert_eq!(
        file_idx, 1,
        "the file1 `[<AutoOpen>]` fragment folds at its own file, so `open N; X` \
         binds `N.A.X`@file1"
    );
}

/// One char at byte offset `off` — the range of a single-character binder (`X`).
fn one_char(off: usize) -> TextRange {
    TextRange::new(
        u32::try_from(off).unwrap().into(),
        u32::try_from(off + 1).unwrap().into(),
    )
}

/// The `(file_idx, def range)` the use at `use_range` in `proj.file(file)`
/// resolves to, or `None` if we defer / do not record a target there.
fn resolved_target(
    proj: &borzoi_sema::ResolvedProject,
    file: usize,
    use_range: TextRange,
) -> Option<(usize, TextRange)> {
    let res = proj.file(file).resolution_at(use_range)?;
    proj.item_def(res).map(|(idx, def)| (idx, def.range))
}

/// The byte offset of the `X` in `let ey = X` (the expression probe) and in
/// `| X ` (the pattern probe) of the shared same-file-straddle user file.
const STRADDLE_USER: &str = "namespace Z\n\nmodule O =\n    open N\n    let ey = X\n    let pf v = match v with | X n -> n | _ -> 0\n";

fn straddle_expr_use() -> TextRange {
    one_char(STRADDLE_USER.find("ey = X").expect("expr use") + "ey = ".len())
}

fn straddle_pat_use() -> TextRange {
    one_char(STRADDLE_USER.find("| X ").expect("pattern use") + "| ".len())
}

/// Oracle rows (Stage 5, fcs-dump-pinned): a straddle whose two tiers sit in the
/// **same file** — a direct `exception X` and an `[<AutoOpen>] module A` — folds
/// with the auto-open fragment *after* the namespace's direct tier, **regardless
/// of source block order**. So an *expression* `open N; X` binds the submodule
/// value (`N.A.X`), while the *pattern* `X _` binds the direct exception (`N.X`)
/// — the submodule `let X` is not a constructor, so it never enters the pattern
/// namespace. (On `main` the same-file straddle deferred, wrongly believing block
/// order decided; it does not.)
#[test]
fn a_same_file_auto_open_value_wins_the_expression_over_a_direct_case() {
    let src0 = "namespace N\n\nexception X of int\n\n[<AutoOpen>]\nmodule A =\n    let X = 0\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(STRADDLE_USER)],
        &AssemblyEnv::default(),
    );
    let (file_idx, def_range) =
        resolved_target(&proj, 1, straddle_expr_use()).expect("expr `X` resolves cross-file");
    assert_eq!(file_idx, 0, "the auto-open `let X` is in file0");
    assert_eq!(
        def_range,
        one_char(src0.rfind('X').expect("auto-open `let X`")),
        "expr `X` binds the auto-open submodule value `N.A.X`, not the direct exception"
    );
}

/// Companion to the row above: the **pattern** `X _` binds the direct
/// `exception X` (`N.X`), because the auto-open `let X` value never enters the
/// constructor namespace.
#[test]
fn a_same_file_direct_case_wins_the_pattern_over_an_auto_open_value() {
    let src0 = "namespace N\n\nexception X of int\n\n[<AutoOpen>]\nmodule A =\n    let X = 0\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(STRADDLE_USER)],
        &AssemblyEnv::default(),
    );
    let (file_idx, def_range) =
        resolved_target(&proj, 1, straddle_pat_use()).expect("pattern `X` resolves cross-file");
    assert_eq!(file_idx, 0, "the direct `exception X` is in file0");
    assert_eq!(
        def_range,
        one_char(src0.find('X').expect("direct `exception X`")),
        "pattern `X` binds the direct exception `N.X`, not the auto-open value"
    );
}

/// Block-order independence: the SAME file with the auto-open fragment declared
/// **first** and the direct `exception X` **second** resolves identically —
/// expr → auto-open value (`N.A.X`), pattern → direct exception (`N.X`). Locks in
/// that the auto-open folds after the direct tier by *kind*, never by source
/// position (fcs-dump probes G/B).
#[test]
fn a_same_file_straddle_is_block_order_independent() {
    // Auto-open FIRST, direct exception SECOND — the reverse of the two rows above.
    let src0 = "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let X = 0\n\nexception X of int\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(STRADDLE_USER)],
        &AssemblyEnv::default(),
    );
    // `find('X')` is now the auto-open `let X` (declared first); `rfind('X')` the
    // direct exception (declared second).
    let (expr_file, expr_def) =
        resolved_target(&proj, 1, straddle_expr_use()).expect("expr `X` resolves cross-file");
    assert_eq!(expr_file, 0);
    assert_eq!(
        expr_def,
        one_char(src0.find('X').expect("auto-open `let X` (first)")),
        "expr `X` still binds the auto-open value even though it is the earlier block"
    );
    let (pat_file, pat_def) =
        resolved_target(&proj, 1, straddle_pat_use()).expect("pattern `X` resolves cross-file");
    assert_eq!(pat_file, 0);
    assert_eq!(
        pat_def,
        one_char(src0.rfind('X').expect("direct `exception X` (second)")),
        "pattern `X` still binds the direct exception even though it is the later block"
    );
}

/// When the auto-open fragment supplies a **case** (`exception X`), it wins the
/// constructor namespace too — both expr and pattern bind the auto-open exception
/// (`N.A.X`), since the auto-open folds after the direct tier within the file
/// (fcs-dump probes E/F, both block orders). The direct `exception X` loses in
/// both namespaces.
#[test]
fn a_same_file_auto_open_case_wins_both_namespaces() {
    let src0 = "namespace N\n\nexception X of int\n\n[<AutoOpen>]\nmodule A =\n    exception X of string\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(STRADDLE_USER)],
        &AssemblyEnv::default(),
    );
    let auto_open = one_char(src0.rfind('X').expect("auto-open `exception X`"));
    let (expr_file, expr_def) =
        resolved_target(&proj, 1, straddle_expr_use()).expect("expr `X` resolves cross-file");
    assert_eq!((expr_file, expr_def), (0, auto_open), "expr binds `N.A.X`");
    let (pat_file, pat_def) =
        resolved_target(&proj, 1, straddle_pat_use()).expect("pattern `X` resolves cross-file");
    assert_eq!((pat_file, pat_def), (0, auto_open), "pattern binds `N.A.X`");
}

/// A `let private` value is hidden from an `open` outside its module's subtree
/// (fcs-dump: `open M; secret` from a sibling module is unbound — a `private`
/// value is visible only within its container and descendants). On `main` this
/// was a wrong target (the private value resolved cross-file); the accessibility
/// filter now defers it.
#[test]
fn an_open_from_outside_hides_a_modules_private_value() {
    let src0 = "namespace N\nmodule M =\n    let private secret = 1\n";
    let src1 = "namespace N\nmodule Other =\n    open M\n    let y = secret\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let i = src1.rfind("secret").expect("`secret` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "secret".len()).unwrap().into(),
    );
    match proj.file(1).resolution_at(use_range) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("a private value opened from outside its module must defer, got {other:?}"),
    }
}

/// The accessibility filter must not over-reach: a **public** value opened from
/// a sibling module still resolves cross-file (guards against filtering a
/// non-`private` binding).
#[test]
fn an_open_from_outside_still_sees_a_modules_public_value() {
    let src0 = "namespace N\nmodule M =\n    let shared = 1\n";
    let src1 = "namespace N\nmodule Other =\n    open M\n    let y = shared\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let i = src1.rfind("shared").expect("`shared` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "shared".len()).unwrap().into(),
    );
    let res = proj
        .file(1)
        .resolution_at(use_range)
        .expect("a resolution at bare `shared`");
    let (file_idx, _) = proj
        .item_def(res)
        .expect("public `shared` resolves cross-file");
    assert_eq!(file_idx, 0, "resolves to M's public value in file0");
}

/// Cross-file module augmentation where a later `let private X` sits over an
/// earlier public `X` at the same path (codex review of the value-accessibility
/// slice). The old latest-wins index collapsed to the private id and lost the
/// public export, forcing a defer; the per-path export-history model keeps both,
/// so the reference from *outside* the module resolves to the **surviving public
/// `X`** — which is exactly what FCS binds (oracle-pinned: `open N.M; X` → the
/// public `N.M.X` in the first file, the private redeclaration being inaccessible
/// from outside). A fallback `open A.F` supplies a same-named public value that
/// the later `open N.M` must shadow with the *real* target, not fall through to.
#[test]
fn a_private_redeclaration_does_not_hide_the_surviving_public_export() {
    let src0 = "namespace N\nmodule M =\n    let X = 20\n";
    let src1 = "namespace N\nmodule M =\n    let private X = 30\n";
    let src_a = "namespace A\nmodule F =\n    let X = 99\n";
    let src2 = "namespace Z\nmodule O =\n    open A.F\n    open N.M\n    let y = X\n";
    let proj = resolve_project(
        &[
            impl_file(src0),
            impl_file(src1),
            impl_file(src_a),
            impl_file(src2),
        ],
        &AssemblyEnv::default(),
    );
    let i = src2.rfind("X").expect("`X` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    let res = proj
        .file(3)
        .resolution_at(use_range)
        .expect("a resolution at bare `X`");
    let (file_idx, _) = proj
        .item_def(res)
        .expect("the surviving public `X` resolves cross-file");
    assert_eq!(
        file_idx, 0,
        "resolves to the public N.M.X in file0, not the inaccessible private redeclaration"
    );
}

/// The dual ordering: an earlier `let private X` then a later public `let X` at
/// the same cross-file module path. From outside, the *latest accessible* export
/// is the later public one (the private being invisible), so `open N.M; X` binds
/// the public `X` in the second file (oracle-pinned). Guards the export-history
/// query against a naive "first public wins" reading — it is latest-accessible,
/// not first-accessible.
#[test]
fn a_public_redeclaration_over_an_earlier_private_wins_from_outside() {
    let src0 = "namespace N\nmodule M =\n    let private X = 30\n";
    let src1 = "namespace N\nmodule M =\n    let X = 20\n";
    let src2 = "namespace Z\nmodule O =\n    open N.M\n    let y = X\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let i = src2.rfind("X").expect("`X` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    let res = proj
        .file(2)
        .resolution_at(use_range)
        .expect("a resolution at bare `X`");
    let (file_idx, _) = proj
        .item_def(res)
        .expect("the later public `X` resolves cross-file");
    assert_eq!(file_idx, 1, "resolves to the later public N.M.X in file1");
}

/// Inherited **type** privacy: a union case of a `private` type is scoped to the
/// type's container (oracle-pinned D3), so it is inaccessible from an outside
/// `open`. Here file 0 declares `type private T = | X` (a real single-case
/// union — note the leading bar; `type private T = X` would be a type
/// *abbreviation* with no case), file 1 augments the module with `let private
/// X`; from an unrelated module both are inaccessible, so FCS reports `X`
/// unbound (FS0037/FS0039, oracle-pinned). The case's export records its
/// access-root as the module (from `type private`), so the collapse recovery
/// skips it — no wrong target.
#[test]
fn an_inherited_private_case_is_not_committed_through_a_collapsed_open() {
    let src0 = "namespace N\nmodule M =\n    type private T = | X\n";
    let src1 = "namespace N\nmodule M =\n    let private X = 20\n";
    let src2 = "namespace Z\nmodule O =\n    open N.M\n    let x = X\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let i = src2.rfind("X").expect("`X` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    match proj.file(2).resolution_at(use_range) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "an inherited-`private` case must not be committed through a collapsed \
             `open` — FCS leaves `X` unbound; got {other:?}"
        ),
    }
}

/// The dual of the tripwire: a **public** union case survives a later `let
/// private X` collapse and resolves through an outside `open`. File 0 declares
/// `type T = | X` (public), file 1 `let private X`; from outside, the private
/// value is inaccessible but the public case is not, so FCS binds `N.M.T.X`
/// (oracle-pinned) — the export-history recovery selects the accessible case.
#[test]
fn a_public_union_case_survives_a_private_value_collapse() {
    let src0 = "namespace N\nmodule M =\n    type T = | X\n";
    let src1 = "namespace N\nmodule M =\n    let private X = 20\n";
    let src2 = "namespace Z\nmodule O =\n    open N.M\n    let x = X\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let i = src2.rfind("X").expect("`X` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    let res = proj
        .file(2)
        .resolution_at(use_range)
        .expect("a resolution at bare `X`");
    let (file_idx, _) = proj
        .item_def(res)
        .expect("the surviving public case resolves cross-file");
    assert_eq!(file_idx, 0, "resolves to the public case N.M.T.X in file0");
}

/// Inherited **module** privacy has the correct access-root: a value in a
/// `module private M` is accessible from a **sibling** module in the same
/// namespace (a `private` module is visible in its enclosing scope), not only
/// from descendants of `M` (oracle-pinned D2). `open M` from `N.Other` resolves
/// `X` to `N.M.X`. Guards against the too-narrow boolean that scoped the value
/// to `M`'s own subtree (which regressed this to a defer / — with a fallback —
/// a wrong target).
#[test]
fn a_module_private_value_resolves_from_a_sibling_in_the_namespace() {
    let src0 = "namespace N\nmodule private M =\n    let X = 1\n";
    let src1 = "namespace N\nmodule Other =\n    open M\n    let y = X\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let i = src1.rfind("X").expect("`X` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    let res = proj
        .file(1)
        .resolution_at(use_range)
        .expect("a resolution at bare `X`");
    let (file_idx, _) = proj
        .item_def(res)
        .expect("the private-module value resolves from a sibling in the namespace");
    assert_eq!(file_idx, 0, "resolves to N.M.X in file0");
}

/// The sibling-access case with a competing fallback `open`: `open A.F` (public
/// `X`) then `open M` (a `module private M` value `X`) from a sibling in `N`.
/// The private module's `X` is accessible here, so the later `open M` shadows
/// the fallback and FCS binds `N.M.X` (oracle-pinned). The too-narrow boolean
/// filtered `X` out of `open M`, letting `A.F.X` win — a wrong target (codex
/// round 3 P1#1). The access-root model keeps `X` visible, so the fallback loses.
#[test]
fn a_module_private_value_shadows_a_fallback_open_from_a_sibling() {
    let src0 = "namespace N\nmodule private M =\n    let X = 1\n";
    let src_a = "namespace A\nmodule F =\n    let X = 99\n";
    let src2 = "namespace N\nmodule Other =\n    open A.F\n    open M\n    let y = X\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src_a), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let i = src2.rfind("X").expect("`X` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    let res = proj
        .file(2)
        .resolution_at(use_range)
        .expect("a resolution at bare `X`");
    let (file_idx, _) = proj.item_def(res).expect("`X` resolves cross-file");
    assert_eq!(
        file_idx, 0,
        "the accessible private-module N.M.X shadows the fallback A.F.X"
    );
}

/// Inherited **module** privacy: a value in a `module private M` is inaccessible
/// from an outside `open`, even without its own `private` modifier — F#
/// accessibility is inherited from the enclosing module. Here file 0 declares
/// `module private M = let X`, file 1 augments `module M` with `let private X`;
/// from an unrelated module BOTH are inaccessible, so FCS reports `X` unbound
/// (FS0039, oracle-pinned). The export history records the file-0 `X` as
/// `is_private` (inherited from `module private M`), so the collapse recovery
/// does not skip the later `private` value and wrongly commit it. On `main` the
/// blunt collapse-defer also declined; the point is the export-history model
/// must not turn that decline into a wrong target (codex round 2 of this PR:
/// it did, until inherited module privacy was tracked).
#[test]
fn a_value_in_a_private_module_is_not_recovered_through_a_collapsed_open() {
    let src0 = "namespace N\nmodule private M =\n    let X = 10\n";
    let src1 = "namespace N\nmodule M =\n    let private X = 20\n";
    let src2 = "namespace Z\nmodule O =\n    open N.M\n    let x = X\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let i = src2.rfind("X").expect("`X` use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + 1).unwrap().into(),
    );
    match proj.file(2).resolution_at(use_range) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "a value inheriting `private` from its module must not be recovered \
             through a collapsed `open` — FCS leaves `X` unbound; got {other:?}"
        ),
    }
}
