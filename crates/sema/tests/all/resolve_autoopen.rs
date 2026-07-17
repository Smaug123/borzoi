//! FCS-free tests for `[<AutoOpen>]`-module resolution — the printfn milestone.
//!
//! Opening a namespace (implicitly, as F# does for `Microsoft.FSharp.Core` &
//! friends, or explicitly via `open`) also opens any `[<AutoOpen>]` *module* it
//! declares, bringing the module's static members into unqualified scope. This
//! is how `printfn` (a static of FSharp.Core's auto-open `ExtraTopLevelOperators`)
//! resolves bare. The fixture is a real F# library standing in for FSharp.Core
//! (see `tests/fixtures/autoopen_env/Fixture.fs`): `CoreOps` is an auto-open module
//! in the implicitly-opened `Microsoft.FSharp.Core`; `Demo.Auto.Extra` is one in
//! a namespace you must `open` first; `CoreClosed` is a non-auto-open negative
//! control.
//!
//! The member resolved through `printfnLike` exercises the F# *source name*
//! recovery end to end: the IL method is `PrintFormatLikeLine`, matched by its
//! `CompilationSourceName("printfnLike")`.

use std::path::{Path, PathBuf};

use borzoi_assembly::{Ecma335Assembly, Member};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, DeferredReason, OpenFoldSpace, OpenFoldTarget, ProjectItems, Resolution,
    ResolvedFile, resolve_file, resolve_project,
};
use rowan::TextRange;

/// Build the auto-open fixture once per test binary and return the `.dll` path.
///
/// Delegates to [`crate::common::ensure_autoopen_fixture_built`], which builds it
/// behind the binary-wide `BUILD_LOCK` so the three groups sharing this fixture
/// cannot race its `obj/`/`bin/`.
fn ensure_fixture_built() -> &'static Path {
    crate::common::ensure_autoopen_fixture_built()
}

fn fixture_env() -> AssemblyEnv {
    let bytes = std::fs::read(ensure_fixture_built()).expect("read autoopen fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse autoopen fixture dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv")
}

/// An env over **both** sema fixtures — the F# auto-open fixture and the C# one. A
/// path can be a module in one assembly and a namespace in another only *across*
/// assemblies (FS0247 forbids the same-assembly clash), so the module/namespace merge
/// (plan Q9) needs two.
fn two_assembly_env() -> AssemblyEnv {
    let fs_bytes = std::fs::read(ensure_fixture_built()).expect("read autoopen fixture dll");
    let cs_bytes = std::fs::read(crate::common::ensure_assembly_fixture_built())
        .expect("read C# assembly fixture dll");
    let fs_view = Ecma335Assembly::parse(&fs_bytes).expect("parse autoopen fixture dll");
    let cs_view = Ecma335Assembly::parse(&cs_bytes).expect("parse C# fixture dll");
    AssemblyEnv::from_views(&[fs_view, cs_view]).expect("build two-assembly AssemblyEnv")
}

/// Build the **abbrev** fixture once per test binary, like
/// [`ensure_fixture_built`] — delegated to [`crate::common`] so it shares the
/// binary-wide `BUILD_LOCK` (an uncached `dotnet build` racing another
/// fixture's build fails writing `…deps.json`; review round 8 reproduced it).
fn ensure_abbrev_fixture_built() -> &'static Path {
    crate::common::ensure_abbrev_fixture_built()
}

/// An env over **both F# fixtures**, which declare the same module FQN
/// (`Demo.ModuleOpen.Shared`) and the two metadata encodings of `NestEnc.Inner` — the
/// cross-assembly merge shapes (review rounds 5 and 7).
fn two_fsharp_assembly_env() -> AssemblyEnv {
    let a = std::fs::read(ensure_fixture_built()).expect("read autoopen fixture dll");
    let b = std::fs::read(ensure_abbrev_fixture_built()).expect("read abbrev fixture dll");
    let va = Ecma335Assembly::parse(&a).expect("parse autoopen fixture");
    let vb = Ecma335Assembly::parse(&b).expect("parse abbrev fixture");
    AssemblyEnv::from_views(&[va, vb]).expect("build two-F#-assembly AssemblyEnv")
}

fn impl_file(src: &str) -> ImplFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        p.errors
    );
    ImplFile::cast(p.root).expect("impl file")
}

fn resolve(src: &str, env: &AssemblyEnv) -> ResolvedFile {
    resolve_file(&impl_file(src), &ProjectItems::default(), env)
}

/// Range of `needle`'s only occurrence in `hay`.
fn at(hay: &str, needle: &str) -> TextRange {
    let s = hay
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {hay:?}"));
    let end = s + needle.len();
    TextRange::new(
        u32::try_from(s).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

fn il_name(m: &Member) -> &str {
    match m {
        Member::Method(x) => &x.name,
        Member::Field(x) => &x.name,
        Member::Property(x) => &x.name,
        Member::Event(x) => &x.name,
    }
}

fn core(env: &AssemblyEnv, name: &str) -> borzoi_sema::EntityHandle {
    env.lookup_type(
        &["Microsoft".into(), "FSharp".into(), "Core".into()],
        name,
        0,
    )
    .unwrap_or_else(|| panic!("fixture must declare Microsoft.FSharp.Core.{name}"))
}

#[test]
fn implicit_auto_open_module_resolves_renamed_member_bare() {
    // `printfnLike` is a static of the auto-open `CoreOps` module in the
    // implicitly-opened `Microsoft.FSharp.Core` namespace — so it resolves with
    // no `open` statement. Its IL method is `PrintFormatLikeLine`, reached by the
    // F# source name from `CompilationSourceName`. This is the `printfn` shape.
    let env = fixture_env();
    let src = "let test () = printfnLike 1\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "printfnLike")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, core(&env, "CoreOps"), "parent module");
            assert_eq!(il_name(env.member_at(parent, idx)), "PrintFormatLikeLine");
        }
        other => panic!("expected Member for bare `printfnLike`, got {other:?}"),
    }
}

#[test]
fn implicit_auto_open_module_resolves_plain_member_bare() {
    // A plainly-named auto-open member (source name == IL name) also resolves
    // bare — the source-name fallback path.
    let env = fixture_env();
    let src = "let test () = plainCore 1\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "plainCore")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, core(&env, "CoreOps"), "parent module");
            assert_eq!(il_name(env.member_at(parent, idx)), "plainCore");
        }
        other => panic!("expected Member for bare `plainCore`, got {other:?}"),
    }
}

#[test]
fn non_auto_open_module_member_does_not_resolve_bare() {
    // `closedValue` lives in `CoreClosed`, a *non*-auto-open module in the same
    // namespace. Opening the namespace does not open the module, so the bare name
    // must not resolve to it (it stays deferred — never a wrong member).
    let env = fixture_env();
    let src = "let test () = closedValue 1\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "closedValue")),
            None | Some(Resolution::Deferred(_))
        ),
        "a non-auto-open module's member must not resolve bare; got {:?}",
        rf.resolution_at(at(src, "closedValue")),
    );
}

#[test]
fn explicit_open_brings_in_auto_open_module() {
    // `Demo.Auto.Extra` is an auto-open module in a non-implicit namespace, so
    // `extraValue` resolves bare only after `open Demo.Auto`.
    let env = fixture_env();
    let src = "open Demo.Auto\nlet test () = extraValue 1\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "extraValue")) {
        Some(Resolution::Member { parent, idx }) => {
            let extra = env
                .lookup_type(&["Demo".into(), "Auto".into()], "Extra", 0)
                .expect("fixture must declare Demo.Auto.Extra");
            assert_eq!(parent, extra, "parent module");
            assert_eq!(il_name(env.member_at(parent, idx)), "extraValue");
        }
        other => panic!("expected Member for `extraValue` after open, got {other:?}"),
    }
}

#[test]
fn auto_open_module_nested_type_marks_bare_annotation_shadowable() {
    let env = fixture_env();
    let src = "module M\nopen Demo.Auto\nlet f (x : int64) = x\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "int64")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "a nested type in Demo.Auto.Extra could shadow the primitive alias"
    );
}

#[test]
fn auto_open_module_nested_type_outranks_a_same_tier_direct_type() {
    // Regression pin (codex review P2, round 6, on
    // `docs/completed/r2-annotation-typing-plan.md`), probe-confirmed against real fsc:
    // a small two-project repro (`namespace Ns; type Foo = ...; [<AutoOpen>]
    // module Auto = type Foo = ...`, then a consumer `open Ns; let x : Foo =
    // { ... }`) fails to compile with FS1129/FS0764 against `Ns.Auto.Foo`'s
    // fields — proving the auto-open module's nested type wins even at the
    // *same* tier as a direct namespace type, not just at a higher-priority
    // tier. `Demo.Auto` declares `SameTierName` directly AND through the
    // auto-open `Extra` module; a check that only ran once the tier's own
    // direct lookup came up `NoMatch` would resolve the direct type here
    // instead of deferring.
    let env = fixture_env();
    let src = "module M\nopen Demo.Auto\nlet f (x : SameTierName) = x\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "SameTierName")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "the auto-open Extra.SameTierName must shadow the direct Demo.Auto.SameTierName"
    );
}

#[test]
fn higher_priority_shadow_risk_outranks_a_lower_priority_real_match() {
    // Regression pin (codex review P2, round 3, on
    // `docs/completed/r2-annotation-typing-plan.md`): `Demo.Low` declares a real type
    // `int64`; `Demo.Auto`'s auto-open `Extra` module has an (unmodelled)
    // nested `int64` too. `open Demo.Auto` is the LATER — higher-priority —
    // open, so F# would bind (or here, defer to) it, never falling through to
    // the earlier `Demo.Low` reading's real type. A shadow check that only
    // ran after the *whole* tiered walk failed would instead resolve
    // `Demo.Low.int64` here, since a per-tier NoMatch on the (unopenable)
    // `Demo.Auto` namespace itself would fall through — the wrong-resolution
    // case the review flagged.
    let env = fixture_env();
    let src = "module M\nopen Demo.Low\nopen Demo.Auto\nlet f (x : int64) = x\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "int64")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "the later, higher-priority open's shadow risk must win over the earlier real type"
    );
}

#[test]
fn higher_priority_real_match_outranks_a_lower_priority_shadow_risk() {
    // The flip side: swap the open order so `Demo.Low`'s real `int64` is now
    // the LATER — higher-priority — open. Its real match must win over
    // `Demo.Auto`'s lower-priority shadow risk, exercising the round-2 fix
    // (a real match must not needlessly defer) in a multi-open setting.
    let env = fixture_env();
    let src = "module M\nopen Demo.Auto\nopen Demo.Low\nlet f (x : int64) = x\n";
    let rf = resolve(src, &env);
    let real = env
        .lookup_type(&["Demo".into(), "Low".into()], "int64", 0)
        .expect("fixture must declare Demo.Low.int64");
    assert_eq!(
        rf.resolution_at(at(src, "int64")),
        Some(Resolution::Entity(real)),
        "the later, higher-priority open's real type must win over the earlier shadow risk"
    );
}

#[test]
fn project_namespace_case_outranks_assembly_auto_open() {
    // codex review: `open Demo.Auto` opens BOTH the project namespace `Demo.Auto`
    // (its union case `Tag`, declared in file 0) AND the referenced assembly's
    // auto-open `Extra` module (its `let Tag`). An assembly member is the
    // lowest-priority interpretation of an `open`, so the project case wins
    // (FCS: bare `Tag` is `Demo.Auto.Color.Tag`). The namespace pass opens the
    // assembly auto-opens *before* the project cases, so the latter out-rank via
    // latest-wins. Before the fix the order was reversed and bare `Tag` resolved
    // to the assembly `Extra.Tag` member.
    let env = fixture_env();
    let src0 = "namespace Demo.Auto\ntype Color = Tag | Other\n";
    let src1 = "module Client\nopen Demo.Auto\nlet x = Tag\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &env);

    let (file_idx, def) = proj
        .file(1)
        .resolution_at(at(src1, "Tag"))
        .and_then(|r| proj.item_def(r))
        .expect("bare `Tag` resolves to the project case, not the assembly member");
    assert_eq!(
        file_idx, 0,
        "the project union case in file 0, not the assembly `Extra.Tag`"
    );
    assert_eq!(def.range, at(src0, "Tag"));
}

#[test]
fn relative_reading_assembly_auto_open_outranks_root_project_case() {
    // The cross-READING sibling of `project_namespace_case_outranks_assembly_auto_open`:
    // there the project case and the assembly auto-open share ONE reading
    // (`Demo.Auto`) and the project side wins within it. Here they sit in
    // DIFFERENT readings of `open Auto` from `namespace Demo` — the assembly
    // auto-open `Demo.Auto.Extra.Tag` in the RELATIVE reading, the project case
    // `Auto.Color.Tag` (file 0) in the ROOT one — and the relative reading wins
    // *whichever side each lives on* (fsc-probed: bare `Tag` typechecks as
    // `int`, the assembly value; annotating it `: Color` is FS0001). A reading's
    // project cases must be applied at that reading's priority, not blanket-last.
    let env = fixture_env();
    let extra = env
        .lookup_type(&["Demo".into(), "Auto".into()], "Extra", 0)
        .expect("fixture must declare Demo.Auto.Extra");
    let src0 = "namespace Auto\n\ntype Color = Tag | Other\n";
    let src1 = "namespace Demo\n\nmodule M =\n    open Auto\n    let x = Tag\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &env);

    match proj.file(1).resolution_at(at(src1, "Tag")) {
        Some(Resolution::Member { parent, .. }) => assert_eq!(
            parent, extra,
            "bare `Tag` is the relative reading's `Demo.Auto.Extra.Tag`",
        ),
        other => panic!(
            "bare `Tag` under `open Auto` from `namespace Demo` is the RELATIVE \
             reading's assembly `Demo.Auto.Extra.Tag`, not the root project case; \
             got {other:?}"
        ),
    }
}

#[test]
fn opened_assembly_value_shadows_a_union_type_qualifier() {
    // codex review: after `open Demo.Auto` brings the auto-open assembly value `Tag`
    // (`Extra.Tag`), a same-named union *type* `Tag` does NOT win the qualifier —
    // FCS reads `Tag.One` as member access on the opened value. The opened value has
    // no in-file `Def` range, so the union value-collision rule must key on the
    // *classification* (a definite non-case value), not on `value_def_range`. Defer.
    let env = fixture_env();
    let src = "open Demo.Auto\ntype Tag = One | Two\nlet x = Tag.One\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "Tag.One")) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("`Tag.One` shadowed by the opened value should defer, got {other:?}"),
    }
}

#[test]
fn nested_module_suffix_does_not_shadow_a_same_named_nested_type() {
    // The companion collision, nested: `Demo.Outer` holds a nested type `Tagged`
    // and a nested suffixed module `TaggedModule` (source name `Tagged`). `nested`
    // must prefer the exact-IL-name type over the source-name alias, just as the
    // top-level index does — never order-dependently.
    let env = fixture_env();
    let outer = env
        .lookup_type(&["Demo".into()], "Outer", 0)
        .expect("fixture must declare Demo.Outer");
    let ty = env
        .nested(outer, "Tagged", 0)
        .expect("nested `Tagged` must resolve");
    assert!(
        !env.is_module(ty),
        "nested `Tagged` must be the type, not the companion module"
    );
    // The nested module's compiled name is never an F# source name.
    assert!(
        env.nested(outer, "TaggedModule", 0).is_none(),
        "the nested compiled name `TaggedModule` must not be searchable"
    );
}

#[test]
fn internal_auto_open_module_member_does_not_resolve_bare() {
    // `internalValue` is a public static of `CoreInternal`, an *internal*
    // auto-open module. F# in another assembly cannot reach an internal module,
    // so opening the namespace must not bring its members into scope — the
    // public-accessibility filter on auto-open modules. (`opened_static_member`
    // checks member access but not the parent's, so the filter is load-bearing.)
    let env = fixture_env();
    let src = "let test () = internalValue 1\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "internalValue")),
            None | Some(Resolution::Deferred(_))
        ),
        "an internal auto-open module's member must not resolve bare; got {:?}",
        rf.resolution_at(at(src, "internalValue")),
    );
}

#[test]
fn module_suffix_does_not_shadow_a_same_named_type() {
    // `Demo.Tagged` (a non-generic type) and `Demo.TaggedModule` (a module whose
    // F# source name is also `Tagged`) collide on the arity-0 index key. The type
    // must keep the bare name; the module must still be reachable by its IL name.
    // Pins that source-name indexing is additive, never displacing a real type.
    let env = fixture_env();
    let ty = env
        .lookup_type(&["Demo".into()], "Tagged", 0)
        .expect("the type `Demo.Tagged` must keep the bare source name");
    assert!(
        !env.is_module(ty),
        "`Demo.Tagged` must resolve to the type, not the module"
    );
    // The companion module's compiled name `TaggedModule` is never an F# source
    // name, so it is not a lookup key.
    assert!(
        env.lookup_type(&["Demo".into()], "TaggedModule", 0)
            .is_none(),
        "the compiled name `TaggedModule` must not be searchable"
    );
}

#[test]
fn unclashed_module_suffix_resolves_by_source_name_only() {
    // `Demo.SoloModule` has no clashing type, so its source name `Solo` is free:
    // it resolves by `Solo` but never by its compiled name `SoloModule`.
    let env = fixture_env();
    let module = env
        .lookup_type(&["Demo".into()], "Solo", 0)
        .expect("a suffixed module must be reachable by its F# source name");
    assert!(env.is_module(module), "`Solo` must be the module");
    assert!(
        env.lookup_type(&["Demo".into()], "SoloModule", 0).is_none(),
        "the compiled name `SoloModule` must not be searchable"
    );
}

#[test]
fn auto_open_module_member_does_not_resolve_without_its_open() {
    // Negative direction of the explicit-open test: with no `open Demo.Auto`,
    // `extraValue` must not resolve (its namespace is not implicitly opened).
    let env = fixture_env();
    let src = "let test () = extraValue 1\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "extraValue")),
            None | Some(Resolution::Deferred(_))
        ),
        "without `open Demo.Auto`, `extraValue` must not resolve; got {:?}",
        rf.resolution_at(at(src, "extraValue")),
    );
}

#[test]
fn colliding_auto_open_across_open_readings_prefers_relative() {
    // `open Sub` from `namespace Demo` has two assembly readings — the relative
    // `Demo.Sub` and the merged root `Sub` — each declaring an `[<AutoOpen>]`
    // module with a `sharedMarker`. Latest-open-wins keeps the RELATIVE reading
    // higher, so (FCS-verified against this fixture):
    //   * the colliding `sharedMarker`   → `Demo.Sub.RelAuto.sharedMarker`,
    //   * the relative-only `relOnlyMarker` → `Demo.Sub.RelAuto.relOnlyMarker`,
    //   * the root-only `rootOnlyMarker`  → `Sub.RootAuto.rootOnlyMarker`.
    // codex R5 regression: an open's readings feed `open_auto_open_modules_in` too,
    // and opened statics are latest-insertion-wins, so the readings must be applied
    // lowest-priority-first (like the shortening prefixes) or the root member wrongly
    // shadows the relative one on a collision.
    let env = fixture_env();
    let rel_auto = env
        .lookup_type(&["Demo".into(), "Sub".into()], "RelAuto", 0)
        .expect("fixture must declare Demo.Sub.RelAuto");
    let root_auto = env
        .lookup_type(&["Sub".into()], "RootAuto", 0)
        .expect("fixture must declare Sub.RootAuto");

    let member_parent = |member: &str| -> borzoi_sema::EntityHandle {
        let src = format!("namespace Demo\n\nmodule M =\n    open Sub\n    let x = {member}\n");
        let rf = resolve(&src, &env);
        match rf.resolution_at(at(&src, member)) {
            Some(Resolution::Member { parent, .. }) => parent,
            other => panic!("expected a Member for bare `{member}`, got {other:?}"),
        }
    };

    assert_eq!(
        member_parent("sharedMarker"),
        rel_auto,
        "the colliding `sharedMarker` resolves the RELATIVE `Demo.Sub.RelAuto`, not the root",
    );
    assert_eq!(
        member_parent("relOnlyMarker"),
        rel_auto,
        "a relative-only member resolves `Demo.Sub.RelAuto`",
    );
    assert_eq!(
        member_parent("rootOnlyMarker"),
        root_auto,
        "a root-only member falls to the root reading `Sub.RootAuto`",
    );
}
#[test]
fn nested_auto_open_module_value_resolves_transitively() {
    // FCS auto-opens recursively (NameResolution.fs's
    // AddModuleOrNamespaceRefsToNameEnv is documented "Recursive because of
    // 'AutoOpen'"), so `open Demo.Auto` opens `Extra` AND its nested
    // `[<AutoOpen>] module ChainedInner` — `chainedValue` resolves bare.
    let env = fixture_env();
    let src = "open Demo.Auto\nlet test () = chainedValue ()\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "chainedValue")) {
        Some(Resolution::Member { parent, idx }) => {
            let extra = env
                .lookup_type(&["Demo".into(), "Auto".into()], "Extra", 0)
                .expect("fixture must declare Demo.Auto.Extra");
            let inner = env
                .nested(extra, "ChainedInner", 0)
                .expect("fixture must nest ChainedInner in Extra");
            assert_eq!(parent, inner, "parent module is the nested auto-open");
            assert_eq!(il_name(env.member_at(parent, idx)), "chainedValue");
        }
        other => panic!("expected Member for transitively-opened `chainedValue`, got {other:?}"),
    }
}

#[test]
fn nested_auto_open_module_type_outranks_a_same_tier_direct_type() {
    // The round-6 same-tier rule, one auto-open level down: `Demo.Auto`
    // declares `Chained` directly AND inside the transitively-auto-opened
    // `Extra.ChainedInner`. The chained nested type wins in FCS, so the
    // precise veto must see through the chain and defer.
    let env = fixture_env();
    let src = "module M\nopen Demo.Auto\nlet f (x : Chained) = x\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Chained")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "ChainedInner.Chained must shadow the direct Demo.Auto.Chained"
    );
}

#[test]
fn nested_module_without_auto_open_stays_closed() {
    // `ChainedClosed` nests in the auto-open `Extra` but is NOT itself
    // [<AutoOpen>]: `open Demo.Auto` must not import its members.
    let env = fixture_env();
    let src = "open Demo.Auto\nlet test () = chainedClosedValue ()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "chainedClosedValue")),
            None | Some(Resolution::Deferred(_))
        ),
        "a non-auto-open nested module's member must not resolve bare; got {:?}",
        rf.resolution_at(at(src, "chainedClosedValue")),
    );
}

#[test]
fn internal_nested_auto_open_module_does_not_contribute() {
    // `ChainedInternal` is [<AutoOpen>] but internal: inaccessible
    // cross-assembly, so it must not contribute members either.
    let env = fixture_env();
    let src = "open Demo.Auto\nlet test () = chainedInternalValue ()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "chainedInternalValue")),
            None | Some(Resolution::Deferred(_))
        ),
        "an internal nested auto-open module's member must not resolve bare; got {:?}",
        rf.resolution_at(at(src, "chainedInternalValue")),
    );
}

#[test]
fn auto_open_closure_applies_in_depth_first_order() {
    // codex on the transitive-auto-open change: FCS recurses into each nested
    // auto-open module before its next sibling (depth-first pre-order), and
    // later-added contents win. `Extra` nests `DeepFirst` (whose `Deepest`
    // holds an `orderMarker`) before the sibling `DeepSecond` (also holding
    // one): FCS's order is Deepest, THEN DeepSecond, so DeepSecond's marker
    // wins. A breadth-first closure pushes Deepest last and binds the wrong
    // member.
    let env = fixture_env();
    let src = "open Demo.Auto\nlet test () = orderMarker ()\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "orderMarker")) {
        Some(Resolution::Member { parent, idx }) => {
            let extra = env
                .lookup_type(&["Demo".into(), "Auto".into()], "Extra", 0)
                .expect("fixture must declare Demo.Auto.Extra");
            let second = env
                .nested(extra, "DeepSecond", 0)
                .expect("fixture must nest DeepSecond in Extra");
            assert_eq!(
                parent, second,
                "the later sibling must win over the earlier sibling's descendant"
            );
            assert_eq!(il_name(env.member_at(parent, idx)), "orderMarker");
        }
        other => panic!("expected Member for `orderMarker`, got {other:?}"),
    }
}

#[test]
fn rec_forward_value_path_does_not_bind_the_assembly_member() {
    // The value-position sibling of the rec-module type-path fix: inside
    // `module rec M`, `Sub.RelAuto.sharedMarker` names the forward-declared
    // project modules in FCS, while the assembly also has
    // `Demo.Sub.RelAuto.sharedMarker` in scope via `open Demo`. The value
    // path already defers here (records nothing at any segment and no
    // whole-span member) — this pins that soundness so a future value-path
    // change cannot start binding the assembly member on rec-forward heads.
    let env = fixture_env();
    let src = "module rec M\nopen Demo\nlet y = Sub.RelAuto.sharedMarker\nmodule Sub =\n    module RelAuto =\n        let sharedMarker = 5\n";
    let rf = resolve(src, &env);
    let whole = TextRange::new(at(src, "Sub").start(), at(src, "sharedMarker").end());
    assert_eq!(rf.resolution_at(at(src, "Sub")), None);
    assert_eq!(rf.resolution_at(at(src, "RelAuto")), None);
    assert_eq!(
        rf.resolution_at(whole),
        None,
        "no whole-span assembly member may be recorded for a rec-forward path"
    );
}

// ===== Assembly-level `[<assembly: AutoOpen("…")>]` (plan A3/S3) =====
//
// The manifest names `SemaAutoOpen.FromManifest` (a namespace) and
// `SemaAutoOpen.DirectOps` (a module); the resolver's implicit opens are
// driven by that list (plus the hardcoded FSharp.Core fallback), so the
// namespace's auto-open module contributes bare names with no `open` —
// a path the hardcoded list cannot have known.

#[test]
fn manifest_auto_open_namespace_resolves_bare() {
    let env = fixture_env();
    let src = "let test () = manifestValue ()\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "manifestValue")) {
        Some(Resolution::Member { parent, idx }) => {
            let expected = env
                .lookup_type(
                    &["SemaAutoOpen".into(), "FromManifest".into()],
                    "ManifestOps",
                    0,
                )
                .expect("fixture must declare SemaAutoOpen.FromManifest.ManifestOps");
            assert_eq!(parent, expected, "manifestValue resolves into ManifestOps");
            assert_eq!(il_name(env.member_at(parent, idx)), "manifestValue");
        }
        other => panic!("expected Member for bare `manifestValue`, got {other:?}"),
    }
}

#[test]
fn manifest_opened_namespace_plain_module_still_requires_qualification() {
    // The manifest opens the NAMESPACE; a plain (non-auto-open) module inside
    // it contributes nothing bare, exactly like `CoreClosed` under the
    // implicit `Microsoft.FSharp.Core`.
    let env = fixture_env();
    let src = "let test () = manifestClosedValue ()\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "manifestClosedValue")),
        Some(Resolution::Deferred(DeferredReason::UnboundName)),
        "a plain module's member must not resolve bare via the manifest open"
    );
}

#[test]
fn manifest_auto_open_module_path_is_skipped_conservatively() {
    // `SemaAutoOpen.DirectOps` is a MODULE named by an assembly-level
    // AutoOpen. FCS opens it (its values resolve bare); sema deliberately
    // skips module-shaped entries for now — their real-world surface
    // (IntrinsicOperators, TaskBuilderExtensions.*) is operators (A4/S4)
    // and extension members, and extension-member statics must never become
    // bare-resolvable. The sound half of FCS's behaviour: the bare name
    // stays Deferred (never a wrong Member, never Unresolved).
    let env = fixture_env();
    let src = "let test () = directValue ()\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "directValue")),
        Some(Resolution::Deferred(DeferredReason::UnboundName)),
        "module-shaped manifest AutoOpen entries are conservatively skipped"
    );
}

#[test]
fn contested_manifest_namespace_defers_instead_of_wrongly_resolving() {
    // The fsi-verified shape behind the contested-namespace drop (codex P2,
    // round 3): FCS opens the CONTRIBUTING assembly's namespace entity only,
    // so when a sibling assembly also declares the namespace our path-based
    // (assembly-blind) open cannot be applied faithfully — it is dropped
    // entirely, and the bare name defers (the sound half of FCS's behaviour;
    // FCS would resolve `manifestValue` into the contributor).
    use borzoi_assembly::EcmaView;
    let bytes = std::fs::read(ensure_fixture_built()).expect("read autoopen fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse autoopen fixture dll");
    let entities = view.enumerate_type_defs().expect("enumerate fixture");
    let mut sibling = entities
        .iter()
        .find(|e| e.name == "ManifestOps")
        .expect("fixture declares ManifestOps")
        .clone();
    sibling.assembly.name = "Sibling".to_string();
    sibling.name = "SiblingMarker".to_string();
    sibling.is_auto_open = false;
    sibling.members = Vec::new();
    sibling.nested_types = Vec::new();
    let env = borzoi_sema::AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
        (
            ensure_fixture_built().to_path_buf(),
            entities,
            borzoi_sema::AbbreviationVisibility::Modelled,
            vec!["SemaAutoOpen.FromManifest".to_string()],
        ),
        (
            PathBuf::from("sibling.dll"),
            vec![sibling],
            borzoi_sema::AbbreviationVisibility::Modelled,
            vec![],
        ),
    ]);
    let src = "let test () = manifestValue ()\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "manifestValue")),
        Some(Resolution::Deferred(DeferredReason::UnboundName)),
        "a contested manifest namespace must defer its bare names, never wrongly resolve"
    );
}

// ===== Extension members never enter unqualified scope (autoopen plan ⚠) =====

#[test]
fn auto_open_module_extension_members_are_not_bare_resolvable() {
    // `CoreExts` is an `[<AutoOpen>]` module of `System.String` augmentations in the
    // implicitly-opened `Microsoft.FSharp.Core` — FSharp.Core's `LazyExtensions`
    // shape. Its members compile to public statics of the module class, so the
    // auto-open fold used to push both as bare names; FCS pushes neither (an
    // extension member is a member, and `AddValRefsToItems` filters
    // `not vref.IsMember`), both fsi-verified FS0039. The instance one is caught by
    // the per-method flag; the *static* one only by the pickle's static-extension
    // index — which is why the model carries both.
    let env = fixture_env();

    let nowhere = "let test s = zzzNoSuchName s\n";
    let unbound = resolve(nowhere, &env).resolution_at(at(nowhere, "zzzNoSuchName"));

    for name in ["ExtInstance", "ExtStatic"] {
        let src = format!("let test s = {name} s\n");
        let rf = resolve(&src, &env);
        assert_eq!(
            rf.resolution_at(at(&src, name)),
            unbound,
            "bare `{name}` is an extension member of the auto-open CoreExts: it must \
             resolve exactly as an unbound name does"
        );
    }

    // Extension-keyed, not module-keyed: the plain `let` next door still resolves.
    let src = "let test () = plainBesideExts ()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "plainBesideExts")),
            Some(Resolution::Member { .. })
        ),
        "a plain `let` in the same auto-open module must still resolve bare, got {:?}",
        rf.resolution_at(at(src, "plainBesideExts"))
    );
}

#[test]
fn module_qualified_extension_members_do_not_resolve() {
    // Nor are they reachable qualified: FCS resolves a module-qualified path
    // against the module's *values*, and an extension member is not one (fsi:
    // `M.ExtStatic "x"` ⇒ FS0039). Only `s.ExtInstance()` reaches it.
    let env = fixture_env();
    for name in ["ExtInstance", "ExtStatic"] {
        let src = format!("let test s = Microsoft.FSharp.Core.CoreExts.{name} s\n");
        let path = format!("Microsoft.FSharp.Core.CoreExts.{name}");
        let rf = resolve(&src, &env);
        assert!(
            !matches!(
                rf.resolution_at(at(&src, &path)),
                Some(Resolution::Member { .. })
            ),
            "a module-qualified F#-native extension member (`{name}`) must not resolve"
        );
    }
}

#[test]
fn a_let_sharing_its_name_with_an_augmentation_still_resolves() {
    // codex review (PR #916): F# permits a module to declare BOTH `let NameClash`
    // and an augmentation `member _.NameClash()`. FCS resolves bare `NameClash` and
    // `CoreExts.NameClash` to the *`let`* (fsi-verified) — the augmentation is
    // simply invisible to those channels. A name-keyed extension filter would hide
    // the value along with the augmentation, so the filter is per *member*.
    let env = fixture_env();

    let src = "let test () = NameClash 1\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "NameClash")),
            Some(Resolution::Member { .. })
        ),
        "bare `NameClash` must resolve to the plain `let`, not be hidden by the \
         same-named augmentation, got {:?}",
        rf.resolution_at(at(src, "NameClash"))
    );

    let src = "let test () = Microsoft.FSharp.Core.CoreExts.NameClash 1\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Microsoft.FSharp.Core.CoreExts.NameClash")),
            Some(Resolution::Member { .. })
        ),
        "the module-qualified `NameClash` must resolve to the plain `let` too"
    );
}

#[test]
fn an_extension_attributed_module_let_still_resolves_bare() {
    // codex review (PR #916): fsc marks both the module class and the `let` of an
    // `[<Extension>]` module with the CLR attribute — but FCS adds a module's
    // contents through its *vals*, where the C#-style extension predicate never
    // runs, so bare `Tripled` resolves (fsi-verified). Keying the bare filter on the
    // method's `[Extension]` attribute alone would wrongly hide it; the C#-style
    // predicate is scoped to non-module entities (and demands the enclosing-type
    // marker, as FCS's `isEnclExtTy` does).
    let env = fixture_env();
    let src = "let test () = Tripled 3\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Tripled")),
            Some(Resolution::Member { .. })
        ),
        "an `[<Extension>]`-attributed module `let` is a value, not a member: it \
         must still resolve bare, got {:?}",
        rf.resolution_at(at(src, "Tripled"))
    );
}

#[test]
fn probe_explicit_module_open() {
    let env = fixture_env();
    for (src, name) in [
        (
            "open Microsoft.FSharp.Core.CoreClosed\nlet t () = closedValue ()\n",
            "closedValue",
        ),
        (
            "open Demo.ExtMatrix.Aug\nlet t () = plainLet 1\n",
            "plainLet",
        ),
    ] {
        eprintln!(
            "PROBE {name}: {:?}",
            resolve(src, &env).resolution_at(at(src, name))
        );
    }
}

/// Review round 3: `[<Extension>]` on a **generic** type is not a C#-style extension
/// container at all — FCS's `IsTyconRefUsedForCSharpStyleExtensionMembers` requires
/// `isNil (tcref.Typars m)` — so its attributed static stays in unqualified scope
/// (fsi: `open type GenericExtType<int>` then bare `GenExt 2` compiles). The
/// predicate must therefore check the container's genericity, not just its attribute.
#[test]
fn a_generic_extension_container_does_not_hide_its_statics() {
    let env = fixture_env();
    let handle = env
        .lookup_type(
            &["Demo".to_string(), "ExtMatrix".to_string()],
            "GenericExtType",
            1,
        )
        .expect("Demo.ExtMatrix.GenericExtType<'a> in the fixture");
    let entries = env.open_static_entries(handle);
    let entry = entries
        .iter()
        .find(|(name, _)| *name == "GenExt")
        .expect("a generic container's [<Extension>] static stays in bare scope");
    assert!(
        entry.1.is_some(),
        "…and names its target: the container is generic, so FCS's C#-style filter \
         does not apply and there is nothing undecidable about it"
    );
}

// ===== `open <assembly module>` — Slice A (docs/assembly-module-open-plan.md) =====

#[test]
fn opening_an_assembly_module_brings_its_values_into_bare_scope() {
    // The gap the extension-visibility matrix found: sema modelled the auto-open fold
    // and `open type`, but a plain `open <module of a referenced assembly>` resolved
    // nothing. FCS brings the module's vals into unqualified scope (fsi-verified).
    let env = fixture_env();
    let src = "open Demo.ModuleOpen.Plain\nlet t () = plainOpened 1\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "plainOpened")) {
        Some(Resolution::Member { parent, .. }) => {
            assert_eq!(
                env.entity(parent).name,
                "Plain",
                "the bare name must resolve into the opened module"
            );
        }
        other => panic!("expected the opened module's value, got {other:?}"),
    }
}

#[test]
fn an_assembly_module_open_does_not_blank_out_other_opens() {
    // The blast radius (plan §1): falling through to no interpretation set
    // `opaque_value_open`, whose contract makes `lookup` skip EVERY opened entry — so
    // one `open MyLib.Helpers` used to defer bare names from *unrelated* opens too.
    let env = fixture_env();
    let src = "open Demo.Auto\nopen Demo.ModuleOpen.Plain\nlet t () = extraValue ()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "extraValue")),
            Some(Resolution::Member { .. })
        ),
        "an assembly-module open must not make an unrelated open's bare name defer, got {:?}",
        rf.resolution_at(at(src, "extraValue"))
    );
}

#[test]
fn a_later_assembly_module_open_shadows_an_earlier_one() {
    // Q8: latest-open-wins between two module opens sharing a value name (both orders
    // fsi-verified).
    let env = fixture_env();
    for (src, winner) in [
        (
            "open Demo.ModuleOpen.Later\nopen Demo.ModuleOpen.Plain\nlet t () = plainOpened 1\n",
            "Plain",
        ),
        (
            "open Demo.ModuleOpen.Plain\nopen Demo.ModuleOpen.Later\nlet t () = plainOpened 1\n",
            "Later",
        ),
    ] {
        let rf = resolve(src, &env);
        match rf.resolution_at(at(src, "plainOpened")) {
            Some(Resolution::Member { parent, .. }) => assert_eq!(
                env.entity(parent).name,
                winner,
                "the LAST open must win, in {src:?}"
            ),
            other => panic!("expected a member for {src:?}, got {other:?}"),
        }
    }
}

#[test]
fn a_require_qualified_access_module_open_still_imports_its_values() {
    // Q5, **corrected** (review round 4). The first probe read a lone FS0892 as "the
    // open imports nothing" — but that error is about the `open` itself, and the bare
    // use after it produced *no* FS0039: FCS reports the error and still enters the
    // module's contents into the name environment (re-probed).
    //
    // So the values resolve. Dropping the module from the walk instead would be a wrong
    // target, not a deferral: with `open Prefix` in scope, `open M` where `Prefix.M` is
    // RQA and a root `M` exists would bind the root's values where FCS binds
    // `Prefix.M`'s. (Reporting FS0892 is a Phase-4 concern.)
    let env = fixture_env();
    let src = "open Demo.ModuleOpen.Rqa\nlet t () = rqaOpened ()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "rqaOpened")),
            Some(Resolution::Member { .. })
        ),
        "FCS imports an RQA module's contents (with an FS0892 on the open), got {:?}",
        rf.resolution_at(at(src, "rqaOpened"))
    );
}

#[test]
fn a_submodule_of_an_opened_assembly_module_never_names_a_wrong_target() {
    // Slice A's contract for Q10 (`open M` then `Sub.f ()`, which FCS resolves): the
    // tiered assembly walk roots a path at a *namespace* prefix, and an opened module
    // is not one — so the dotted head stays conservative (`unmodelled_open_active`)
    // and defers. A deferral, never a wrong target. Slice B lifts it; the ignored
    // test below is the target behaviour and flips green when it lands.
    let env = fixture_env();
    let src = "open Demo.ModuleOpen.Plain\nlet t () = Sub.subOpened ()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Sub.subOpened")),
            Some(Resolution::Member { .. })
        ),
        "until Slice B a submodule dotted head must defer, not name a target"
    );
}

#[test]
#[ignore = "Slice B of docs/assembly-module-open-plan.md: submodule dotted heads through an opened assembly module"]
fn a_submodule_of_an_opened_assembly_module_is_a_dotted_head() {
    // Q10, fsi-verified: `open ProbeNs.Helpers` then `NotAuto.subVal ()` compiles.
    let env = fixture_env();
    let src = "open Demo.ModuleOpen.Plain\nlet t () = Sub.subOpened ()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Sub.subOpened")),
            Some(Resolution::Member { .. })
        ),
        "a submodule of an opened module must be a dotted head, got {:?}",
        rf.resolution_at(at(src, "Sub.subOpened"))
    );
}

#[test]
fn an_enumerated_case_surface_no_longer_blanks_an_earlier_open() {
    // Slice C, the fold. `Demo.ModuleOpen.WithCases` declares a union (`Colour =
    // Crimson | Viridian`) whose cases FCS brings into bare scope (Q1). The fold
    // enumerates them from the pickle (`Entity::union_case_names`), so the open
    // carries NO name-unknown residue: an earlier open's `Tag` — a name this module
    // provably does not supply — keeps resolving, exactly as FCS binds it. (Until
    // Slice B/C this deferred: the cases were invisible, so the whole open had to
    // shadow conservatively.)
    let env = fixture_env();
    let src = "open Demo.Auto\nopen Demo.ModuleOpen.WithCases\nlet t () = Tag\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Tag")),
            Some(Resolution::Member { .. })
        ),
        "`WithCases`'s complete surface (cases enumerated) supplies no `Tag`, so the \
         earlier open's value resolves, got {:?}",
        rf.resolution_at(at(src, "Tag"))
    );

    // The cases themselves are in scope as (opaque) entries: bare `Crimson` is a
    // case reference with no committed target — Deferred, present, shadowing.
    let src = "open Demo.ModuleOpen.WithCases\nlet t () = Crimson\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Crimson")),
        Some(Resolution::Deferred(DeferredReason::UnboundName)),
        "a folded union case is in scope, opaque"
    );

    // …while its own enumerable value still resolves.
    let src = "open Demo.ModuleOpen.WithCases\nlet t () = caseless 1\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "caseless")),
            Some(Resolution::Member { .. })
        ),
        "the module's plain value must still resolve"
    );
}

#[test]
fn a_relative_or_shortened_assembly_module_open_resolves() {
    // Review (Slice A): the first cut only ever saw the path *as written*, so a
    // relative (`namespace Demo; open ModuleOpen.Plain`) or shortened (`open Demo;
    // open ModuleOpen.Plain`) open never reached the module — and a colliding root
    // module could have won instead. Assembly modules now go through the same tiered
    // interpretation walk as project modules and namespace readings.
    let env = fixture_env();
    for src in [
        // shortened through an earlier open
        "open Demo\nopen ModuleOpen.Plain\nlet t () = plainOpened 1\n",
        // relative to the enclosing namespace
        "namespace Demo\n\nmodule Client =\n    open ModuleOpen.Plain\n    let t () = plainOpened 1\n",
    ] {
        let rf = resolve(src, &env);
        match rf.resolution_at(at(src, "plainOpened")) {
            Some(Resolution::Member { parent, .. }) => assert_eq!(
                env.entity(parent).name,
                "Plain",
                "the tiered walk must reach Demo.ModuleOpen.Plain in {src:?}"
            ),
            other => panic!("expected the opened module's value in {src:?}, got {other:?}"),
        }
    }
}

#[test]
fn a_nested_constructible_type_shadows_an_earlier_opens_value() {
    // Review (Slice A), P1: FCS puts a nested class's bare name in the unqualified
    // *value* slot as a constructor, where it evicts an earlier opened value of the
    // same name. `Demo.ModuleOpen.WithNestedClass` declares a class `Tag`; an earlier
    // `open Demo.Auto` supplies a value `Tag`. Until we model the type slot, bare
    // `Tag` must NOT resolve to that stale earlier value.
    let env = fixture_env();
    let src = "open Demo.Auto\nopen Demo.ModuleOpen.WithNestedClass\nlet t () = Tag\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Tag")),
            Some(Resolution::Member { .. })
        ),
        "a nested constructible type must shadow the earlier open's value, got {:?}",
        rf.resolution_at(at(src, "Tag"))
    );

    // …and the module's own modelled value still resolves (the barrier is a
    // generation bump, not a blanket suppression).
    let src = "open Demo.Auto\nopen Demo.ModuleOpen.WithNestedClass\nlet t () = alsoHere 1\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "alsoHere")),
            Some(Resolution::Member { .. })
        ),
        "the opened module's own value must still resolve past the barrier"
    );
}

#[test]
fn a_cross_kind_open_shadows_an_earlier_opens_value_with_a_namespace_only_name() {
    // Review round 15, P2: the cross-kind cut declined to name a target for the names the
    // MODULE half supplies — but that is only half the job. The NAMESPACE half of the
    // same path contributes names we enumerate *not at all* (its unions' cases, its
    // exception constructors), and FCS adds them at this open, where they outrank an
    // **earlier** open's value. Without a generation barrier the earlier value stayed
    // current and was handed back: a wrong target, of exactly the kind the cut exists to
    // prevent.
    //
    // fsi-verified against two probe assemblies: `open Demo.Auto` (a value `Tag = 99`)
    // then `open Demo.ModuleOpen.Merged` (module half in one assembly; namespace half in
    // the other, declaring `type Verdict = Tag | Acquitted`) binds the union **case**
    // `Tag`, not the earlier value. The fixtures mirror that exactly.
    let env = two_fsharp_assembly_env();

    let src = "open Demo.Auto\nopen Demo.ModuleOpen.Merged\nlet t () = Tag\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Tag")),
            Some(Resolution::Member { .. })
        ),
        "the namespace half's union case `Tag` outranks the earlier `open Demo.Auto`'s \
         value; the cross-kind open must raise the barrier rather than hand back the \
         stale member, got {:?}",
        rf.resolution_at(at(src, "Tag"))
    );
}

#[test]
fn an_auto_open_nested_type_shadows_an_earlier_opens_value() {
    // Review round 14, P1: the enumerability whitelist checked `is_auto_open` on the
    // **Module** arm only, so an `[<AutoOpen>]` *record* (or class/interface/RQA union)
    // let its parent pass as `Complete` — no generation barrier — and a bare name that
    // FCS takes from the auto-open type's statics resolved to a *stale earlier open*
    // instead. A wrong target, not merely a missing one.
    //
    // `CanAutoOpenTyconRef` (NameResolution.fs:1355) auto-opens any non-generic,
    // F#-declared type carrying `[<AutoOpen>]` — not just modules — and adds its static
    // content to the environment. Verified against fsi: with `open Other` (supplying
    // `Shared = 100`) then `open Parent` (whose auto-open type supplies `Shared = 2`),
    // F# binds **2**. The hidden name outranks the earlier open.
    //
    // `Demo.ModuleOpen.WithAutoOpenType`'s record supplies a static `Tag`; the earlier
    // `open Demo.Auto` supplies a value `Tag`. Bare `Tag` must NOT bind that stale value.
    let env = fixture_env();
    let src = "open Demo.Auto\nopen Demo.ModuleOpen.WithAutoOpenType\nlet t () = Tag\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Tag")),
            Some(Resolution::Member { .. })
        ),
        "the auto-open type's hidden static must shadow the earlier open's `Tag`, got {:?}",
        rf.resolution_at(at(src, "Tag"))
    );

    // …and the module's own value still resolves: fsi says the auto-open type's statics
    // land BELOW the module's vals, so this is `HiddenBelowVals` — a generation bump,
    // not a blanket suppression.
    let src = "open Demo.Auto\nopen Demo.ModuleOpen.WithAutoOpenType\nlet t () = alsoHereToo 1\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "alsoHereToo")),
            Some(Resolution::Member { .. })
        ),
        "the opened module's own value must still resolve past the barrier, got {:?}",
        rf.resolution_at(at(src, "alsoHereToo"))
    );
}

/// Q9 / review (Slice A → the namespace half joins the fold): a path that is a
/// **module** in one referenced assembly and a **namespace** in another. FCS opens and
/// merges both halves (fsi-verified with two probe libraries; FS0247 makes this
/// inexpressible within one assembly), folding them in **reference order**.
///
/// Reference order is not a resolution input we model, so a name **both** halves supply
/// still defers. But a name **unique** to one half is uncontested and FCS binds it (Q13)
/// — which the original Slice-A cut sacrificed (it blanket-deferred the whole module
/// half, `docs/assembly-module-open-plan.md` §4c "What this costs"). Now that the
/// assembly namespace half is a fold surface, the contest is per-name: the C# namespace
/// half here supplies only the type `FromNamespaceHalf`, so the module half's own
/// `fromModuleHalf` resolves.
#[test]
fn a_cross_kind_open_resolves_the_module_halfs_unique_name_and_the_namespace_half() {
    let env = two_assembly_env();

    // The MODULE half (F# fixture): a value unique to it — the C# namespace half supplies
    // no such name — so it is uncontested and resolves (the availability §4c restores).
    let src = "open Demo.ModuleOpen.Merged\nlet t () = fromModuleHalf 1\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "fromModuleHalf")),
            Some(Resolution::Member { .. })
        ),
        "the module half's `fromModuleHalf` is unique to it (the C# namespace half \
         supplies only `FromNamespaceHalf`); a merge resolves an uncontested name — \
         got {:?}",
        rf.resolution_at(at(src, "fromModuleHalf"))
    );

    // The NAMESPACE half (C# fixture): a type reached through the merged reading. An
    // ordinary namespace open — it must NOT be suppressed by the module half.
    let src = "open Demo.ModuleOpen.Merged\nlet t () = FromNamespaceHalf.NsStatic ()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "FromNamespaceHalf.NsStatic")),
            Some(Resolution::Member { .. })
        ),
        "the namespace half must not be suppressed by the module half, got {:?}",
        rf.resolution_at(at(src, "FromNamespaceHalf.NsStatic"))
    );
}

#[test]
fn a_literal_in_an_opened_assembly_module_resolves() {
    // Review (Slice A, round 2): a `[<Literal>] let` was projected as *no member at
    // all* — not even a skip — so `open M; TheAnswer` could never resolve, and no
    // consumer could even know to be conservative about the name. FCS resolves it
    // (fsi-verified). It is now claimed to its static literal field in the projector,
    // which is what lets the module's bare-name surface be *proved* complete rather
    // than guessed at.
    let env = fixture_env();
    let src = "open Demo.ModuleOpen.WithLiteral\nlet t () = TheAnswer\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "TheAnswer")),
            Some(Resolution::Member { .. })
        ),
        "an opened module's literal must resolve, got {:?}",
        rf.resolution_at(at(src, "TheAnswer"))
    );
}

#[test]
fn a_struct_rqa_union_still_shadows_an_earlier_opens_value() {
    // Review round 3: `[<RequireQualifiedAccess>]` keeps a union's *cases* out of bare
    // scope (Q6), but a **struct** union is construction-capable, so its type name still
    // takes the unqualified value slot and evicts an earlier opened value of that name.
    // `Demo.ModuleOpen.WithStructRqaUnion` declares a struct RQA union `Flag`; if an
    // earlier open supplied a value `Flag`, waving the module through as "fully
    // enumerable" would resolve that stale value where FCS binds the type.
    let env = fixture_env();
    let h = env
        .lookup_type(
            &["Demo".to_string(), "ModuleOpen".to_string()],
            "WithStructRqaUnion",
            0,
        )
        .expect("WithStructRqaUnion in the fixture");
    let surface = env.open_fold_surface(h);
    assert!(
        surface.entries.iter().any(|e| e.name == "Flag"
            && e.target == OpenFoldTarget::Opaque
            && e.space == OpenFoldSpace::Value),
        "the struct RQA union's TYPE NAME occupies the unqualified value slot: the fold \
         surface must carry an opaque `Flag` value entry, got {:?}",
        surface.entries
    );
    assert!(
        !surface.entries.iter().any(|e| e.is_case),
        "an RQA union's CASES are not imported (Q6)"
    );

    // Its own plain value still resolves past the barrier.
    let src = "open Demo.ModuleOpen.WithStructRqaUnion\nlet t () = besideFlag 1\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "besideFlag")),
            Some(Resolution::Member { .. })
        ),
        "the module's own value must still resolve"
    );
}

#[test]
fn an_assembly_module_does_not_unshadow_an_anonymous_root_project_module() {
    // Review round 4, P1: a headerless file's `module RootOpened` is a project module
    // whose values sema cannot enumerate (`is_project_module_path` cannot see an
    // anonymous-root one), and the fixture assembly exports a module of the same path.
    // FCS opens both and binds the LOCAL `RootOpened.rootShared`. Counting the assembly
    // module as "a module was opened" suppressed the project-opaque fallback, so bare
    // `rootShared` resolved into the referenced assembly — a wrong target. The fallback
    // is gated on a *project* module only.
    let env = fixture_env();
    let src0 = "module Earlier\nlet rootShared = 1\n";
    let src1 = "open Earlier\nmodule RootOpened =\n    let rootShared = 2\nopen RootOpened\nlet y = rootShared\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &env);

    let i = src1.rfind("rootShared").expect("the use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "rootShared".len()).unwrap().into(),
    );
    let res = proj.file(1).resolution_at(use_range);
    assert!(
        !matches!(res, Some(Resolution::Member { .. })),
        "bare `rootShared` must not resolve into the referenced assembly: FCS binds the \
         local anonymous-root `RootOpened.rootShared`, which we cannot enumerate — defer"
    );
    if let Some(r) = res
        && let Some((file_idx, _)) = proj.item_def(r)
    {
        assert_ne!(
            file_idx, 0,
            "nor to the earlier open's `Earlier.rootShared`: the local module shadows it"
        );
    }
}

/// Review round 5: two referenced assemblies exposing the **same module FQN**. FCS
/// merges them — `open Demo.ModuleOpen.Shared` imports the unique values of *both*, and
/// a colliding name binds the later-referenced assembly's (fsi-verified with two probe
/// libraries). Reading only the first-wins type index opened one module, losing the
/// other's values and binding a collision to whichever assembly happened to be indexed
/// first: a wrong target.
///
/// Sema does not model reference order as a resolution input, so the collision **defers**
/// (in scope, no target) while both assemblies' unique values resolve.
#[test]
fn a_merged_module_resolves_unique_names_and_defers_collisions() {
    // Round 12 cut this to "defer every name" — modelling exactly which names the
    // halves contest produced a wrong target in each of rounds 5, 7, 9 and 11,
    // because the contested set was a blacklist over names the model did not
    // represent. The fold (Slice C) lifts the cut soundly: both halves' surfaces are
    // complete-or-residue, so a name unique to one half binds it REGARDLESS of
    // reference order (fsi-verified — FCS imports the unique values of both), and
    // only a name supplied by both is order-dependent and defers.
    let env = two_fsharp_assembly_env();

    for name in ["onlyInAutoOpenFixture", "onlyInAbbrevFixture"] {
        let src = format!("open Demo.ModuleOpen.Shared\nlet t () = {name} ()\n");
        let rf = resolve(&src, &env);
        assert!(
            matches!(
                rf.resolution_at(at(&src, name)),
                Some(Resolution::Member { .. })
            ),
            "`{name}` is unique to one assembly's half of the merged module and \
             resolves regardless of reference order, got {:?}",
            rf.resolution_at(at(&src, name))
        );
    }

    let src = "open Demo.ModuleOpen.Shared\nlet t () = collidingShared ()\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "collidingShared")),
        Some(Resolution::Deferred(DeferredReason::UnboundName)),
        "a name supplied by BOTH assemblies binds whichever is referenced later — an \
         order sema does not model — so it defers, never the wrong assembly"
    );
}

#[test]
fn opening_a_suffixed_companion_module_imports_the_module_not_the_type() {
    // Review round 6, P1: `Demo.Outer` holds a nested type `Tagged` AND a nested
    // companion module `Tagged` (compiled `TaggedModule`). `AssemblyEnv::nested`
    // deliberately prefers the *type* — right for a type-position lookup, wrong for an
    // `open`: FCS accepts `open Demo.Outer.Tagged` and imports the MODULE's `wrap`
    // (fsi-verified against this very fixture). Selecting the type dropped the module
    // interpretation, so after an earlier `open Demo.Solo` a bare `wrap` resolved to
    // that stale earlier member.
    let env = fixture_env();
    let src = "open Demo.Solo\nopen Demo.Outer.Tagged\nlet t () = wrap 5\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "wrap")) {
        Some(Resolution::Member { parent, .. }) => assert_eq!(
            env.entity(parent).name,
            "TaggedModule",
            "bare `wrap` must bind the later-opened companion module, not the earlier \
             `Demo.Solo`'s"
        ),
        other => panic!("expected the companion module's `wrap`, got {other:?}"),
    }
}

#[test]
fn opening_a_companion_module_does_not_suppress_implicit_namespace_readings() {
    // Regression. `open Demo.Tagged` opens the enumerable module `Tagged`
    // (compiled `TaggedModule`), which shares a namespace with the plainly-named
    // *type* `Tagged`. The open-classifier's `enumerable` check compared the
    // type-preferring `opened_assembly_type` handle (the type) against
    // `opened_assembly_module` (the module); they differ, so the enumerable
    // module was misclassified as an *unmodelled type* open and set
    // `unmodelled_open_active` — which suppresses every later *relative* assembly
    // reading, including the implicit open of `Microsoft.FSharp.Core`. So a
    // qualified path whose head is a module of that implicitly-opened namespace
    // (`CoreClosed.closedValue` — the fixture's stand-in for `Seq.toList` through
    // the implicit `Microsoft.FSharp.Collections`) wrongly deferred.
    //
    // A plain F# `open` opens the module, never the bare type (that needs
    // `open type`), so the type companion must not make the open unmodelled.
    // `Demo.Solo` (a suffixed module with NO type companion) is the control that
    // always resolved; `Demo.Tagged` must now behave identically.
    let env = fixture_env();
    let core_closed = env
        .lookup_type(
            &["Microsoft".into(), "FSharp".into(), "Core".into()],
            "CoreClosed",
            0,
        )
        .expect("fixture declares Microsoft.FSharp.Core.CoreClosed");

    for open_line in ["open Demo.Tagged\n", "open Demo.Solo\n"] {
        let src = format!("{open_line}let test () = CoreClosed.closedValue ()\n");
        let rf = resolve(&src, &env);
        assert_eq!(
            rf.resolution_at(at(&src, "CoreClosed")),
            Some(Resolution::Entity(core_closed)),
            "`open`ing an enumerable companion module must not suppress the \
             implicit `Microsoft.FSharp.Core` reading of `CoreClosed` (`{}`)",
            open_line.trim(),
        );
    }

    // The companion module's own value still imports — the open is not weakened.
    let src = "open Demo.Tagged\nlet t () = wrap 5\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "wrap")) {
        Some(Resolution::Member { parent, .. }) => assert_eq!(
            env.entity(parent).name,
            "TaggedModule",
            "bare `wrap` must bind the opened companion module",
        ),
        other => panic!("expected the companion module's `wrap`, got {other:?}"),
    }
}

#[test]
fn opening_a_module_with_only_non_public_children_does_not_defer_unrelated_dotted_heads() {
    // Regression: `Seq.toList` deferring after `open Fantomas.FCS.Text.Range`.
    // Opening an assembly module raises `opaque_dotted_open` (the Slice B gap — a
    // dotted head *through* the opened module, `open M; Sub.f`, is not modelled),
    // which blanket-defers EVERY later dotted head. The trigger was
    // `!children(h).is_empty()`, which counted the **non-public** compiler-generated
    // closure classes that back a module's `let` values. `Range` is nothing but
    // those, so opening it wrongly killed a bare `Seq.toList` two lines down.
    //
    // `Demo.ModuleOpen.OnlyNonPublicNested` reproduces the shape: only a `private`
    // nested module (the deterministic stand-in for those closures) and no public
    // nested member. It cannot seed a dotted head we don't model, so a later,
    // wholly unrelated qualified path (`CoreClosed.closedValue` through the implicit
    // `Microsoft.FSharp.Core` — the fixture's `Seq.toList` analog) must still
    // resolve. The blanket now keys on an *accessible* (public) child, mirroring the
    // R2 primitive-alias shadow's accessible-child fix.
    let env = fixture_env();
    let core_closed = env
        .lookup_type(
            &["Microsoft".into(), "FSharp".into(), "Core".into()],
            "CoreClosed",
            0,
        )
        .expect("fixture declares Microsoft.FSharp.Core.CoreClosed");
    let src = "open Demo.ModuleOpen.OnlyNonPublicNested\nlet test () = CoreClosed.closedValue ()\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "CoreClosed")),
        Some(Resolution::Entity(core_closed)),
        "opening a module whose only children are non-public must not suppress an \
         unrelated dotted head"
    );

    // The opened module's own value still imports — the open is not weakened, and a
    // module with a genuine *public* nested submodule (`Demo.ModuleOpen.Plain.Sub`)
    // still defers dotted heads, pinned by
    // `a_submodule_of_an_opened_assembly_module_never_names_a_wrong_target`.
    let src2 = "open Demo.ModuleOpen.OnlyNonPublicNested\nlet test () = plainValue 1\n";
    let rf2 = resolve(src2, &env);
    assert!(
        matches!(
            rf2.resolution_at(at(src2, "plainValue")),
            Some(Resolution::Member { .. })
        ),
        "the opened module's own value must still resolve, got {:?}",
        rf2.resolution_at(at(src2, "plainValue")),
    );
}

#[test]
fn a_project_module_and_an_assembly_module_at_one_path_merge() {
    // Review round 6, then the round-18 deep cut. FCS merges a project module with a
    // referenced module of the same FQN — `open Demo.ModuleOpen.Plain` imports the
    // project's values *and* the assembly's, a collision binding the PROJECT's. That is a
    // **merge** (two interpretations of one open: `Module` + `AssemblyModule`), and the
    // deep cut declines a definite target for the assembly half of any merge — the same
    // rule that retired the cross-assembly and cross-kind wrong targets.
    //
    // The FOLD (Slice C) models exactly that: the group applies the assembly half
    // first and the project half after, so latest-wins gives the project a collision
    // (FCS folds the project's own fragment last — Q14), while a name unique to the
    // assembly half keeps its definite target. (Between the round-18 deep cut and the
    // fold, the assembly half deferred wholesale.) `module_has_hidden_values` still
    // guards the project half: a project module with values we cannot enumerate bumps
    // the generation after the assembly entries are pushed, staling them.
    let env = fixture_env();
    let src0 = "namespace Demo.ModuleOpen\n\nmodule Plain =\n    let localOnly = 1\n    let plainOpened = 2\n";
    let src1 = "open Demo.ModuleOpen.Plain\nlet a = localOnly\nlet b = plainOpened\nlet c = assemblyOnlyValue\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &env);

    let range = |needle: &str| {
        let i = src1.rfind(needle).expect("use");
        TextRange::new(
            u32::try_from(i).unwrap().into(),
            u32::try_from(i + needle.len()).unwrap().into(),
        )
    };

    // The project half resolves (it is an in-project item).
    assert!(
        proj.file(1).resolution_at(range("localOnly")).is_some(),
        "the project module's own value must resolve"
    );
    // A name unique to the ASSEMBLY half resolves definitely: the project half is
    // fully enumerated, so nothing contests it (fsi-verified — FCS imports both
    // halves).
    assert!(
        matches!(
            proj.file(1).resolution_at(range("assemblyOnlyValue")),
            Some(Resolution::Member { .. })
        ),
        "a name unique to the assembly half of a project/assembly merge resolves, \
         got {:?}",
        proj.file(1).resolution_at(range("assemblyOnlyValue"))
    );

    // The COLLIDING name still binds the project's (an in-project item, not gated by the
    // assembly-side cut).
    let colliding = proj.file(1).resolution_at(range("plainOpened"));
    assert!(
        colliding.is_some_and(|r| proj.item_def(r).is_some()),
        "a name in both halves binds the PROJECT module's, got {colliding:?}"
    );
}

#[test]
fn both_metadata_encodings_of_one_module_fqn_are_found_and_defer_together() {
    // Review round 7: the same F# module FQN can be *encoded* differently across DLLs —
    // a top-level type in a namespace (`namespace NestEnc; module Inner`) in one, and a
    // root module with a nested module (`module NestEnc = module Inner`) in the other.
    // The walk returned at the first split that yielded roots, so one encoding's module
    // vanished entirely: a colliding value would then have looked *unique* and bound the
    // wrong assembly. Finding both halves is what makes the merge VISIBLE.
    //
    // The fold (Slice C) folds BOTH encodings' surfaces: a name unique to one
    // resolves into ITS assembly (which is what proves both encodings were found —
    // each name exists in exactly one), and a name in both would demote per-name
    // (reference order unmodelled). The round-7 hazard is unchanged in shape: were
    // one encoding invisible, a collision would look unique and bind the wrong
    // assembly — completeness of the walk is what the fold's soundness rests on.
    let env = two_fsharp_assembly_env();
    for name in ["fromNamespaceEncoding", "fromNestedEncoding"] {
        let src = format!("open NestEnc.Inner\nlet t () = {name} ()\n");
        let rf = resolve(&src, &env);
        assert!(
            matches!(
                rf.resolution_at(at(&src, name)),
                Some(Resolution::Member { .. })
            ),
            "`{name}` is unique to one metadata encoding of the merged module and \
             resolves into its assembly, got {:?}",
            rf.resolution_at(at(&src, name))
        );
    }
}

#[test]
fn a_decimal_literal_in_an_opened_module_resolves() {
    // Review round 7: `[<Literal>] let D = 1.5M` is NOT a CLI literal — the CLI has no
    // decimal constant form, so fsc emits a static *init-only* field carrying
    // `[DecimalConstantAttribute]`. The projector's literal filter dropped it, so the
    // name was invisible where FCS resolves it (fsi-verified) — the same hazard as the
    // plain literal, in its one exceptional representation.
    let env = fixture_env();
    let src = "open NestEnc.Inner\nlet t () = DecimalConst\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "DecimalConst")),
            Some(Resolution::Member { .. })
        ),
        "an opened module's decimal literal must resolve, got {:?}",
        rf.resolution_at(at(src, "DecimalConst"))
    );
}

#[test]
fn a_name_supplied_by_both_the_module_and_namespace_halves_of_a_path_defers() {
    // Rounds 11 and 12: `Demo.ModuleOpen.Merged` is a MODULE in the autoopen fixture and
    // a NAMESPACE (with an `[<AutoOpen>]` module) in the abbrev fixture. FCS folds the
    // two in *reference order*, so the namespace half's `fromModuleHalf` wins when its
    // assembly is referenced later, while we apply the module half last unconditionally.
    //
    // Round 11 tried to model exactly which names the halves contest — it enumerated the
    // namespace's auto-open module *values*, and round 12 found the set was still missing
    // its type names, union cases and exception constructors. Slice A now declines the
    // module half of a cross-kind path wholesale, which subsumes this case; the F#-side
    // fixture is kept as a second witness alongside the C#-side one in
    // `the_module_half_of_a_cross_kind_path_defers_but_the_namespace_half_resolves`.
    let env = two_fsharp_assembly_env();

    let src = "open Demo.ModuleOpen.Merged\nlet t () = fromModuleHalf 1\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "fromModuleHalf")),
            Some(Resolution::Member { .. })
        ),
        "both halves of the path supply `fromModuleHalf`; FCS orders them by reference, \
         which we do not model — defer, got {:?}",
        rf.resolution_at(at(src, "fromModuleHalf"))
    );

    // A name only ONE half supplies still resolves.
    let src = "open Demo.ModuleOpen.Merged\nlet t () = onlyInNamespaceHalf ()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "onlyInNamespaceHalf")),
            Some(Resolution::Member { .. })
        ),
        "the namespace half's unique name must still resolve, got {:?}",
        rf.resolution_at(at(src, "onlyInNamespaceHalf"))
    );
}
