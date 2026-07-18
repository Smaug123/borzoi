//! FCS-free tests for resolution into a referenced assembly — both directly
//! fully-qualified paths and paths shortened by an `open` (Stage E).
//!
//! These build an [`AssemblyEnv`] from the sema-owned fixture assembly and
//! check the resolver directly: a `Namespace.Type` prefix resolves to an
//! `Entity`, a trailing static member to a `Member`, a non-existent tail stays
//! `Deferred` (never a wrong `Member`); an `open` lets an unqualified
//! `Type.Member` resolve, a local binding shadows an opened name, and opens
//! that leave a name ambiguous defer. `resolve_assembly_diff.rs` checks these
//! shapes against FCS.

use crate::common::ensure_assembly_fixture_built;
use borzoi_assembly::{
    Augmentation, Ecma335Assembly, EcmaView, Entity, EntityKind, Member, ModuleValue, SkippedMember,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AbbreviationVisibility, AssemblyEnv, ProjectItems, Resolution, ResolvedFile, SemanticClass,
    resolve_file, resolve_project,
};
use rowan::TextRange;

fn fixture_env() -> AssemblyEnv {
    let bytes = std::fs::read(ensure_assembly_fixture_built()).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv")
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

fn span(start: usize, len: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + len).unwrap().into(),
    )
}

/// Range of `needle`'s only occurrence in `hay`.
fn at(hay: &str, needle: &str) -> TextRange {
    let s = hay
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {hay:?}"));
    span(s, needle.len())
}

fn member_name(m: &Member) -> &str {
    match m {
        Member::Method(x) => &x.name,
        Member::Field(x) => &x.name,
        Member::Property(x) => &x.name,
        Member::Event(x) => &x.name,
    }
}

/// Regression: a computation-expression binder's LHS is a *deconstruction
/// pattern*, not a function-binding head. A constructor-shaped head like
/// `let! Ctor x = m` must leave `Ctor` a reference (only `x` binds), so a body
/// use of `Ctor` does **not** resolve to a bogus pattern-introduced local.
/// (`BinderRole::Pattern`, not `Let`.)
#[test]
fn ce_binder_constructor_head_is_not_a_local() {
    let env = AssemblyEnv::default();
    let src = "let f m =\n    async {\n        let! Ctor x = m\n        return Ctor\n    }\n";
    let rf = resolve(src, &env);

    // The body use of `Ctor` (the one after `return `) must not be a local.
    let body_ctor = {
        let s = src.find("return Ctor").expect("body use") + "return ".len();
        span(s, "Ctor".len())
    };
    assert!(
        matches!(
            rf.resolution_at(body_ctor),
            None | Some(Resolution::Deferred(_))
        ),
        "constructor head `Ctor` must not introduce a local; body use resolved to {:?}",
        rf.resolution_at(body_ctor),
    );
}

#[test]
fn qualified_static_member_resolves_to_an_assembly_member() {
    let env = fixture_env();
    let src = "module M\nlet x = Demo.Calc.Zero()\n";
    let rf = resolve(src, &env);

    // The `Calc` segment resolves to the `Demo.Calc` type entity.
    let calc = env
        .lookup_type(&["Demo".to_string()], "Calc", 0)
        .expect("Demo.Calc in env");
    assert_eq!(
        rf.resolution_at(at(src, "Calc")),
        Some(Resolution::Entity(calc)),
        "the `Calc` qualifier resolves to the Demo.Calc entity"
    );

    // The whole path `Demo.Calc.Zero` resolves to the `Zero` member of `Calc`.
    match rf.resolution_at(at(src, "Demo.Calc.Zero")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, calc, "member's parent is Demo.Calc");
            assert_eq!(member_name(env.member_at(parent, idx)), "Zero");
        }
        other => panic!("expected Member for Demo.Calc.Zero, got {other:?}"),
    }
}

#[test]
fn qualified_static_property_resolves_to_an_assembly_member() {
    let env = fixture_env();
    let src = "module M\nlet x = Demo.Calc.Answer\n";
    let rf = resolve(src, &env);

    match rf.resolution_at(at(src, "Demo.Calc.Answer")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(member_name(env.member_at(parent, idx)), "Answer");
        }
        other => panic!("expected Member for Demo.Calc.Answer, got {other:?}"),
    }
}

#[test]
fn type_qualified_instance_member_is_not_resolved() {
    // `Demo.Thing.Go` names an *instance* method via a type-qualified path.
    // F# resolves only static members that way (an instance member needs a
    // value receiver), so we must NOT record a Member for it.
    let env = fixture_env();
    let src = "module M\nlet x = Demo.Thing.Go\n";
    let rf = resolve(src, &env);
    assert!(
        !rf.resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "an instance member must not resolve via a type-qualified path"
    );
}

#[test]
fn inaccessible_assembly_symbols_do_not_resolve() {
    // Cross-assembly, only *public* types/members are accessible. The fixture's
    // `Demo.Hidden` is an internal type and `Demo.Calc.Hush` an internal static
    // member — neither must resolve (FCS would not), even though both exist in
    // the index.
    let env = fixture_env();

    let via_internal_type = resolve("module M\nlet x = Demo.Hidden.Secret()\n", &env);
    assert!(
        !via_internal_type
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "a path through an internal type must not resolve into the assembly"
    );

    let internal_member = resolve("module M\nlet x = Demo.Calc.Hush()\n", &env);
    assert!(
        !internal_member
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "an internal static member must not resolve to a Member"
    );
}

#[test]
fn nonexistent_tail_stays_deferred_never_a_wrong_member() {
    // `Demo.Calc` is a real type but `Nope` names nothing on it. The type
    // qualifier still resolves; the tail must NOT resolve to a fabricated
    // member (correctness over availability).
    let env = fixture_env();
    let src = "module M\nlet x = Demo.Calc.Nope\n";
    let rf = resolve(src, &env);

    // The rooting type still resolves.
    let calc = env.lookup_type(&["Demo".to_string()], "Calc", 0).unwrap();
    assert_eq!(
        rf.resolution_at(at(src, "Calc")),
        Some(Resolution::Entity(calc))
    );

    // Nothing in the file resolves to a Member (no fabricated `Nope`).
    assert!(
        !rf.resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "a non-existent member must not resolve to any Member"
    );

    // The unresolved tail is explicitly Deferred, not silently absent (D5: a
    // modeled-but-unresolved name use is recorded, not dropped).
    assert!(
        matches!(
            rf.resolution_at(at(src, "Nope")),
            Some(Resolution::Deferred(_))
        ),
        "the unresolved tail segment must be Deferred"
    );
}

#[test]
fn same_file_module_qualified_path_does_not_hit_a_colliding_assembly_type() {
    // The file is `module Demo` and defines `Calc`; the fixture assembly ALSO
    // has a `Demo.Calc` type. `Demo.Calc` here is the in-file value (FCS
    // resolves it so), so we must NOT record the assembly Entity — a same-file
    // module-qualified path is not an assembly path.
    let env = fixture_env();
    let src = "module Demo\nlet Calc = 1\nlet x = Demo.Calc\n";
    let rf = resolve(src, &env);

    assert!(
        !rf.resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "a path rooted at the current module must not resolve into a colliding assembly type"
    );
}

#[test]
fn nested_module_shadows_a_colliding_assembly_member() {
    // The fixture has `Demo.Calc.Answer`. With `open Demo`, a bare `Calc.Answer`
    // routes to that assembly member — UNLESS the file declares a nested
    // `module Calc`, which F# resolves first. Sema does not model the nested
    // module's members yet (parser 8.4 lands the syntax; resolution is a later
    // slice), but it must still defer `Calc.Answer` rather than fall through to
    // the colliding assembly member (the `assembly_path_records` soundness
    // tripwire).
    let env = fixture_env();

    // Control: with no nested module, `open Demo` routes `Calc.Answer` to the
    // assembly member — establishing the collision the nested module shadows.
    let control = resolve("module Outer\nopen Demo\nlet y = Calc.Answer\n", &env);
    assert!(
        control
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "control: `open Demo` should route `Calc.Answer` to the assembly member"
    );

    // With a project nested `module Calc`, the same reference must NOT hit the
    // assembly — it defers (sound under-resolution, never a wrong member).
    let shadowed = resolve(
        "module Outer\nopen Demo\nmodule Calc =\n    let z = 1\nlet y = Calc.Answer\n",
        &env,
    );
    assert!(
        !shadowed
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "a nested module must shadow a colliding assembly member, not fall through"
    );
}

#[test]
fn module_abbrev_alias_shadows_a_colliding_assembly_member() {
    // Like `nested_module_shadows_a_colliding_assembly_member`, but the project
    // name `Calc` is introduced by a module *abbreviation* (`module Calc = …`,
    // parser 8.5) rather than a nested module. The alias is unmodelled, but a
    // reference rooted at it must still defer rather than fall through to the
    // colliding `Demo.Calc.Answer` assembly member.
    let env = fixture_env();
    let shadowed = resolve(
        "module Outer\nopen Demo\nmodule Calc = Other.Thing\nlet y = Calc.Answer\n",
        &env,
    );
    assert!(
        !shadowed
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "a module-abbreviation alias must shadow a colliding assembly member"
    );
}

#[test]
fn type_definition_name_shadows_a_colliding_assembly_member() {
    // Like `nested_module_shadows_a_colliding_assembly_member`, but the project
    // name `Calc` is introduced by a *type definition* (`type Calc = …`, parser
    // phase 9) rather than a nested module. Sema does not model the type's
    // members yet, but a reference rooted at the type name must still defer
    // rather than fall through to the colliding `Demo.Calc.Answer` assembly
    // member (the `assembly_path_records` soundness tripwire the phase-9 `Types`
    // resolver arm guards).
    let env = fixture_env();
    let shadowed = resolve(
        "module Outer\nopen Demo\ntype Calc = int\nlet y = Calc.Answer\n",
        &env,
    );
    assert!(
        !shadowed
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "a project type name must shadow a colliding assembly member, not fall through"
    );
}

#[test]
fn nested_module_type_prefix_does_not_fall_through_to_a_colliding_assembly_type() {
    // SOUNDNESS (type position): a *nested* `module Sub` whose name collides with
    // the assembly namespace `Demo.Sub`. A qualified type `Sub.Calc` descends into
    // the nested module — and `Demo.Sub.Calc` is an assembly type. F# binds the
    // nested module's own `type Calc` (`M.Sub.Calc`) when it has one, and falls
    // through to `Demo.Sub.Calc` only when it does not (FCS-verified both ways).
    // We model neither the nested module's types nor that fall-through, so we
    // cannot tell the cases apart — we must DEFER, never bind the assembly
    // `Demo.Sub.Calc` (which would be a wrong target when the module has `Calc`).
    // A nested module shadows on a *proper prefix* (unlike a top-level module,
    // which merges with the assembly namespace), so the type path must veto the
    // opens tier here.
    let env = fixture_env();
    let demo_sub_calc = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Calc", 0)
        .unwrap();
    for src in [
        // The nested module declares `type Calc` — FCS binds the project type.
        "module M\nopen Demo\nmodule Sub =\n    type Calc = int\nlet f (x : Sub.Calc) = x\n",
        // …and the empty-module case (FCS falls through to the assembly): we still
        // defer, since we cannot distinguish it from the shadowing case.
        "module M\nopen Demo\nmodule Sub =\n    let placeholder = 1\nlet f (x : Sub.Calc) = x\n",
    ] {
        let rf = resolve(src, &env);
        assert!(
            !rf.resolutions()
                .values()
                .any(|r| *r == Resolution::Entity(demo_sub_calc)),
            "nested `module Sub` type prefix must not bind the assembly `Demo.Sub.Calc` in {src:?}",
        );
    }
}

#[test]
fn anonymous_file_type_name_does_not_shadow_a_later_files_assembly_path() {
    // Regression (codex review of phase 9.1): a type defined in an *anonymous*
    // (header-less) module is visible cross-file only as `<Filename>.Calc`, not
    // bare `Calc`. Recording a bare `Calc` cross-file shadow would wrongly make
    // a *later* file's `open Demo; Calc.Answer` defer instead of resolving the
    // assembly member `Demo.Calc.Answer`. So an anonymous file must contribute
    // *no* cross-file shadow — the same fix applies to anonymous-file nested
    // modules and module abbreviations (the shared `record_project_name_shadow`
    // gate). The same-file shadow is unaffected (it stays local to file 1).
    let env = fixture_env();
    let file1 = impl_file("type Calc = int\n");
    let src2 = "module Other\nopen Demo\nlet x = Calc.Answer\n";
    let file2 = impl_file(src2);
    let proj = resolve_project(&[file1, file2], &env);
    assert!(
        proj.file(1)
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "an anonymous-file type must not shadow a later file's `Demo.Calc.Answer` \
         assembly resolution"
    );
}

#[test]
fn cross_file_project_item_prefix_is_not_overridden_by_assembly() {
    // An earlier file exports `Demo.Calc` (a value); a later file references
    // `Demo.Calc.Answer`. F# resolves `Demo.Calc` to the project value and
    // `.Answer` as member access on it — so even though the fixture assembly
    // has `Demo.Calc.Answer`, we must NOT record an assembly Member.
    let env = fixture_env();
    let file1 = impl_file("module Demo\nlet Calc = 1\n");
    let file2 = impl_file("module Other\nlet x = Demo.Calc.Answer\n");
    let proj = resolve_project(&[file1, file2], &env);

    assert!(
        !proj
            .file(1)
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "a path whose prefix is a cross-file project item must not resolve into the assembly"
    );
}

#[test]
fn cross_file_module_prefix_falls_through_to_assembly() {
    // An earlier file is `module Demo.Calc` (a project *module*) that does NOT
    // export `Answer`. The fixture assembly has a `Demo.Calc` type with
    // `Answer`. F# *merges* the project module header with the assembly
    // namespace and, because the module does not provide `Answer`, falls
    // through to the assembly type — so `Demo.Calc.Answer` resolves into the
    // assembly. (FCS-verified; see `resolve_project_assembly_diff.rs`.)
    let env = fixture_env();
    let src2 = "module Other\nlet x = Demo.Calc.Answer\n";
    let file1 = impl_file("module Demo.Calc\nlet foo = 1\n");
    let file2 = impl_file(src2);
    let proj = resolve_project(&[file1, file2], &env);
    let rf = proj.file(1);

    let calc = env.lookup_type(&["Demo".to_string()], "Calc", 0).unwrap();
    assert_eq!(
        rf.resolution_at(at(src2, "Calc")),
        Some(Resolution::Entity(calc)),
        "the `Calc` segment resolves to the assembly Demo.Calc entity"
    );
    match rf.resolution_at(at(src2, "Demo.Calc.Answer")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, calc, "member's parent is the assembly Demo.Calc");
            assert_eq!(member_name(env.member_at(parent, idx)), "Answer");
        }
        other => panic!("expected assembly Member for Demo.Calc.Answer, got {other:?}"),
    }
}

#[test]
fn cross_file_nested_module_prefix_does_not_hit_a_colliding_assembly_member() {
    // Companion to `cross_file_module_prefix_falls_through_to_assembly`, but the
    // earlier file declares `Calc` as a *nested* `module Calc = …` inside
    // `module Demo`. Unlike a top-level `module Demo.Calc` (whose values are in
    // the project index, so a non-provided `Answer` soundly falls through to the
    // merged assembly namespace), a nested module's members are *not* modeled
    // yet — so sema cannot tell a member it provides from one it does not and
    // must defer `Demo.Calc.Answer` rather than resolve it to the colliding
    // assembly member. Pins that the nested-module shadow carries *across files*.
    let env = fixture_env();
    let src2 = "module Other\nlet x = Demo.Calc.Answer\n";
    let file1 = impl_file("module Demo\nmodule Calc =\n    let foo = 1\n");
    let file2 = impl_file(src2);
    let proj = resolve_project(&[file1, file2], &env);
    let rf = proj.file(1);

    assert!(
        !rf.resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "a path rooted at an earlier file's nested module must not fall through \
         to a colliding assembly member"
    );
}

#[test]
fn cross_file_nested_module_under_namespace_qualifies_the_shadow() {
    // Like the previous test, but the earlier file's nested `module Calc` lives
    // under a `namespace Demo` (the common real-world shape). The namespace
    // contributes no value prefix (`module_path` is `None` for a namespace), but
    // the nested module is still qualified `Demo.Calc` — so the cross-file shadow
    // must carry the namespace prefix, or `Demo.Calc.Answer` would slip through.
    let env = fixture_env();
    let src2 = "module Other\nlet x = Demo.Calc.Answer\n";
    let file1 = impl_file("namespace Demo\nmodule Calc =\n    let foo = 1\n");
    let file2 = impl_file(src2);
    let proj = resolve_project(&[file1, file2], &env);
    let rf = proj.file(1);

    assert!(
        !rf.resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "a path rooted at an earlier file's namespace-qualified nested module \
         must not fall through to a colliding assembly member"
    );
}

#[test]
fn exact_project_module_path_is_not_overridden_by_assembly() {
    // An earlier file is `module Demo.Calc`; a later use names `Demo.Calc`
    // exactly (the module path itself, not a value under it — so the cross-file
    // Item branch, which keys on exact exported *values*, does not catch it).
    // The fixture assembly also has a public `Demo.Calc` type — but the project
    // module shadows it, so we must NOT record the assembly Entity.
    let env = fixture_env();
    let file1 = impl_file("module Demo.Calc\nlet foo = 1\n");
    let file2 = impl_file("module Other\nlet x = Demo.Calc\n");
    let proj = resolve_project(&[file1, file2], &env);

    assert!(
        !proj
            .file(1)
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "an exact project module path must not resolve into the assembly"
    );
}

#[test]
fn value_less_project_module_does_not_shadow_assembly() {
    // file1 is `module Demo.Calc` with no exported values (only an `open`), so
    // it provides no `Answer`. The module header merges with the assembly
    // namespace and falls through: `Demo.Calc.Answer` resolves into the
    // assembly, the value-less project module notwithstanding. (FCS-verified.)
    let env = fixture_env();
    let src2 = "module Other\nlet x = Demo.Calc.Answer\n";
    let file1 = impl_file("module Demo.Calc\nopen System\n");
    let file2 = impl_file(src2);
    let proj = resolve_project(&[file1, file2], &env);

    match proj.file(1).resolution_at(at(src2, "Demo.Calc.Answer")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(member_name(env.member_at(parent, idx)), "Answer");
        }
        other => panic!("expected assembly Member for Demo.Calc.Answer, got {other:?}"),
    }
}

#[test]
fn shared_root_namespace_does_not_block_assembly_resolution() {
    // An earlier file is `module Demo.Other`, so `Demo` is a *namespace* the
    // project shares with the assembly — and namespaces merge. A later
    // reference to the assembly's `Demo.Calc.Zero` must still resolve; the
    // project's `Demo` namespace prefix must not shadow it (only a project
    // module/value does).
    let env = fixture_env();
    let file1 = impl_file("module Demo.Other\nlet foo = 1\n");
    let file2 = impl_file("module M\nlet x = Demo.Calc.Zero()\n");
    let proj = resolve_project(&[file1, file2], &env);

    assert!(
        proj.file(1)
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "an assembly reference under a shared root namespace must still resolve"
    );
}

#[test]
fn unknown_namespace_path_does_not_resolve_into_the_assembly() {
    // A path whose head is no namespace/type in the env falls through — no
    // Entity/Member, and certainly never Unresolved.
    let env = fixture_env();
    let src = "module M\nlet x = Nope.Missing.Gone\n";
    let rf = resolve(src, &env);

    for r in rf.resolutions().values() {
        assert!(
            !matches!(r, Resolution::Entity(_) | Resolution::Member { .. }),
            "unknown path must not resolve into the assembly, got {r:?}"
        );
        assert!(
            !matches!(r, Resolution::Unresolved),
            "must never be Unresolved"
        );
    }
}

// ── Stage E: resolution through `open` ──────────────────────────────────────

/// Range of `needle`'s *last* occurrence in `hay`.
fn at_last(hay: &str, needle: &str) -> TextRange {
    let s = hay
        .rfind(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {hay:?}"));
    span(s, needle.len())
}

#[test]
fn open_namespace_resolves_unqualified_type_and_member() {
    // `open Demo` brings the type `Calc` into scope, so `Calc.Zero` resolves to
    // `Demo.Calc.Zero` even though `Demo` is not written at the use site.
    let env = fixture_env();
    let src = "open Demo\nlet x = Calc.Zero()\n";
    let rf = resolve(src, &env);

    let calc = env.lookup_type(&["Demo".to_string()], "Calc", 0).unwrap();
    assert_eq!(
        rf.resolution_at(at(src, "Calc")),
        Some(Resolution::Entity(calc)),
        "the unqualified `Calc` resolves to Demo.Calc via the open"
    );
    match rf.resolution_at(at(src, "Calc.Zero")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, calc);
            assert_eq!(member_name(env.member_at(parent, idx)), "Zero");
        }
        other => panic!("expected Member for Calc.Zero via open, got {other:?}"),
    }
}

#[test]
fn open_type_resolves_unqualified_static_members() {
    // `open type Demo.Calc` brings the type's static members into unqualified
    // scope, so bare `Zero` (a static method) and `Answer` (a static property)
    // resolve to `Demo.Calc.Zero` / `Demo.Calc.Answer`.
    let env = fixture_env();
    let src = "open type Demo.Calc\nlet x = Zero()\nlet y = Answer\n";
    let rf = resolve(src, &env);
    let calc = env.lookup_type(&["Demo".to_string()], "Calc", 0).unwrap();
    for name in ["Zero", "Answer"] {
        match rf.resolution_at(at(src, name)) {
            Some(Resolution::Member { parent, idx }) => {
                assert_eq!(parent, calc, "{name} parent");
                assert_eq!(member_name(env.member_at(parent, idx)), name);
            }
            other => panic!("expected Member for opened static {name}, got {other:?}"),
        }
    }
}

#[test]
fn overloaded_opened_static_member_defers() {
    // `Demo.Calc.Twice` is overloaded (two public statics), so it is not uniquely
    // selectable — a bare `Twice` under `open type Demo.Calc` must defer rather
    // than pick one overload (we don't model overload resolution). This also
    // guards the "present but ambiguous" case being mistaken for "absent".
    let env = fixture_env();
    let src = "open type Demo.Calc\nlet x = Twice 1\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Twice")),
            None | Some(Resolution::Deferred(_))
        ),
        "an overloaded opened static must defer, not pick an overload"
    );
}

#[test]
fn overloaded_static_via_open_is_not_overridden_by_a_lower_tier() {
    // Regression: a higher-priority reading that reaches an *overloaded* static
    // member (the type owns the member name, but it is an overload set we cannot
    // uniquely select) must NOT be discarded in favour of a lower-priority tier
    // that resolves the same path to a *unique* member of a *different* type.
    //
    // `Demo.Calc.Twice` is overloaded. Clone `Demo.Calc` into the ROOT namespace
    // keeping only one `Twice`, so root `Calc.Twice` is a unique static. With
    // `open Demo` in scope, F# resolves `Calc` to `Demo.Calc` (opens outrank the
    // root) and leaves the overloaded `Twice` to overload resolution — so `Calc`
    // must bind to `Demo.Calc` and `Twice` must defer, never bind the root member.
    let mut entities: Vec<Entity> = {
        let bytes = std::fs::read(ensure_assembly_fixture_built()).expect("read fixture dll");
        Ecma335Assembly::parse(&bytes)
            .expect("parse fixture dll")
            .enumerate_type_defs()
            .expect("enumerate fixture types")
    };

    let demo_calc = entities
        .iter()
        .find(|e| e.namespace == ["Demo"] && e.name == "Calc")
        .expect("Demo.Calc in fixture")
        .clone();
    let twice = |m: &Member| member_name(m) == "Twice";
    assert_eq!(
        demo_calc.members.iter().filter(|m| twice(m)).count(),
        2,
        "Demo.Calc.Twice must be overloaded in the fixture"
    );

    // The real fixture also ships a global-namespace `Calc` (the open-partial vs
    // complete-root sweep case); drop it so the synthetic root `Calc` below is
    // the only one, keeping this test's "unique `Twice` at the root tier" shape.
    entities.retain(|e| !(e.namespace.is_empty() && e.name == "Calc"));

    let mut root_calc = demo_calc.clone();
    root_calc.namespace = vec![]; // global namespace → the "root / as-written" tier
    let mut kept = false;
    root_calc.members.retain(|m| {
        if twice(m) {
            let keep = !kept;
            kept = true;
            keep
        } else {
            true
        }
    });
    entities.push(root_calc);
    let env = AssemblyEnv::from_entities(entities);

    let demo_calc_h = env
        .lookup_type(&["Demo".to_string()], "Calc", 0)
        .expect("Demo.Calc");
    let root_calc_h = env.lookup_type(&[], "Calc", 0).expect("root Calc");
    assert_ne!(demo_calc_h, root_calc_h);
    assert!(env.static_member(demo_calc_h, "Twice").is_none()); // overloaded
    assert!(env.static_member(root_calc_h, "Twice").is_some()); // unique

    let src = "module Mod\nopen Demo\nlet x = Calc.Twice\n";
    let rf = resolve_file(&impl_file(src), &ProjectItems::default(), &env);

    assert_eq!(
        rf.resolution_at(at(src, "Calc")),
        Some(Resolution::Entity(demo_calc_h)),
        "`Calc` must resolve to Demo.Calc (opens outrank root), not the root Calc"
    );
    assert!(
        matches!(
            rf.resolution_at(at(src, "Calc.Twice")),
            None | Some(Resolution::Deferred(_))
        ),
        "the overloaded `Twice` must defer, not bind a lower-tier root member"
    );
}

#[test]
fn open_type_internal_static_member_does_not_resolve() {
    // `Hush` is an *internal* static of `Demo.Calc` — inaccessible cross-assembly,
    // so `open type` does not bring it into scope (`static_member` is public-only).
    let env = fixture_env();
    let src = "open type Demo.Calc\nlet x = Hush()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Hush")),
            None | Some(Resolution::Deferred(_))
        ),
        "an internal static member must not resolve via open type"
    );
}

#[test]
fn local_binding_shadows_an_opened_type_static_member() {
    // A local `Zero` shadows the opened-type static `Demo.Calc.Zero` — lexical
    // scope is searched before opened types.
    let env = fixture_env();
    let src = "open type Demo.Calc\nlet Zero = 1\nlet y = Zero\n";
    let rf = resolve(src, &env);
    let res = rf
        .resolution_at(at_last(src, "Zero"))
        .expect("a resolution");
    assert!(
        matches!(res, Resolution::Item(_)),
        "a local binding must shadow the opened static, got {res:?}"
    );
}

#[test]
fn open_type_does_not_leak_across_top_level_blocks() {
    // An `open` is scoped to its top-level block (FCS-verified): an `open type` in
    // one `namespace`/`module` block does not carry to the next. Block 1's `Zero`
    // resolves via the open; block 2's `Zero` (open out of scope) defers.
    let env = fixture_env();
    let src = "namespace N\nopen type Demo.Calc\nmodule A =\n    let x = Zero()\nnamespace M\nmodule B =\n    let y = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "block-1 `Zero` should resolve via the open, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
    assert!(
        matches!(
            rf.resolution_at(at_last(src, "Zero")),
            None | Some(Resolution::Deferred(_))
        ),
        "an open type must not leak into the next top-level block"
    );
}

#[test]
fn plain_class_open_does_not_suppress_open_type_statics() {
    // A plain `open` of a *class* (`Demo.Thing`) imports no unqualified values, so
    // it must not make bare-name resolution opaque: a sibling `open type Demo.Calc`
    // still resolves `Zero` to `Demo.Calc.Zero`.
    let env = fixture_env();
    let src = "open Demo.Thing\nopen type Demo.Calc\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "a plain class open must not suppress open-type statics, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

#[test]
fn open_type_target_is_shortened_by_an_earlier_open() {
    // The `open type` target is resolved through the active name environment:
    // `open Demo` shortens `open type Calc` to `Demo.Calc` (FCS-verified), so a
    // bare `Zero` resolves to `Demo.Calc.Zero` — not opaque just because the
    // literal `Calc` is not a top-level type.
    let env = fixture_env();
    let src = "open Demo\nopen type Calc\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    let calc = env.lookup_type(&["Demo".to_string()], "Calc", 0).unwrap();
    match rf.resolution_at(at(src, "Zero")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, calc, "the shortened open type roots at Demo.Calc");
            assert_eq!(member_name(env.member_at(parent, idx)), "Zero");
        }
        other => panic!("expected Demo.Calc.Zero via a shortened open type, got {other:?}"),
    }
}

#[test]
fn latest_open_shortens_the_open_type_target() {
    // With both `open Demo` and `open Demo.Sub` in scope, the `open type Calc`
    // target shortens to two types (`Demo.Calc`, `Demo.Sub.Calc`); F# breaks it by
    // **latest-open precedence** — the later `open Demo.Sub` wins, so `open type
    // Calc` is `Demo.Sub.Calc` and bare `Zero` its `Zero` member (FCS-verified).
    // (Was `ambiguously_shortened_open_type_defers`.)
    let env = fixture_env();
    let src = "open Demo\nopen Demo.Sub\nopen type Calc\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    let demo_sub_calc = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Calc", 0)
        .unwrap();
    match rf.resolution_at(at(src, "Zero")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(
                parent, demo_sub_calc,
                "bare `Zero` is the latest open's Demo.Sub.Calc.Zero"
            );
            assert_eq!(member_name(env.member_at(parent, idx)), "Zero");
        }
        other => panic!("expected the `Zero` member of `Demo.Sub.Calc`, got {other:?}"),
    }
}

#[test]
fn open_type_of_a_namespace_local_project_type_defers() {
    // `open type Demo.Calc` where `Demo.Calc` is a *same-file* namespace-qualified
    // project type (`namespace Demo; type Calc`) referenced from a sibling
    // `module M`: the project type shadows the referenced assembly's `Demo.Calc`,
    // and we don't model project-type statics, so a bare `Zero` defers rather than
    // resolving to the assembly member.
    let env = fixture_env();
    let src = "namespace Demo\ntype Calc = int\nmodule M =\n    open type Demo.Calc\n    let x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        !rf.resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "a same-file namespace-local project type must shadow the assembly open type"
    );
}

#[test]
fn plain_open_of_a_project_module_suppresses_opened_type_statics() {
    // An earlier file's `module B` exports a value `Zero`. A later file opens both
    // the assembly type `Demo.Calc` (statics) and the project module `B`. F#
    // brings B's `Zero` into unqualified scope, and the *later* `open B` wins, so
    // bare `Zero` is the project value `B.Zero` — which we do not model. We must
    // NOT resolve it to the assembly static `Demo.Calc.Zero`; deferring is sound.
    let env = fixture_env();
    let file1 = impl_file("module B\nlet Zero = 0\n");
    let file2 = impl_file("module M\nopen type Demo.Calc\nopen B\nlet x = Zero\n");
    let proj = resolve_project(&[file1, file2], &env);
    assert!(
        !proj
            .file(1)
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "a plain open of a project module must suppress opened-type statics (B.Zero shadows)"
    );
}

#[test]
fn plain_open_of_a_path_under_a_nested_module_is_opaque() {
    // A same-file nested module under another (`module Proj` ▸ `module Inner` with
    // `let Zero`). `open Proj.Inner` opens the *inner* project module — bringing
    // its value `Zero` into unqualified scope (which we do not model). With a
    // sibling `open type Demo.Calc`, bare `Zero` must defer (the project value
    // shadows), not resolve to the assembly static `Demo.Calc.Zero`. The opacity
    // check must match a path *under* a nested module, not only the exact name.
    let env = fixture_env();
    let src = "module Proj =\n    module Inner =\n        let Zero = 0\nopen type Demo.Calc\nopen Proj.Inner\nlet x = Zero\n";
    let rf = resolve(src, &env);
    assert!(
        !rf.resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "opening a path under a nested module must suppress opened-type statics"
    );
}

#[test]
fn plain_open_of_a_module_colliding_with_an_assembly_class_is_opaque() {
    // An earlier file's `module Demo.Calc` exports `Zero` — and collides with the
    // assembly's `Demo.Calc` *class*. A later file `open type Demo.Sub.Calc; open
    // Demo.Calc; let x = Zero`: F# opens the project module `Demo.Calc` (its value
    // `Zero` enters unqualified scope, latest open wins), so bare `Zero` is the
    // project value — NOT the assembly static `Demo.Sub.Calc.Zero`. The
    // project-module open must be opaque even though the path is also an assembly
    // class (the assembly-type classification must not pre-empt it).
    let env = fixture_env();
    let file1 = impl_file("module Demo.Calc\nlet Zero = 0\n");
    let file2 = impl_file("module M\nopen type Demo.Sub.Calc\nopen Demo.Calc\nlet x = Zero\n");
    let proj = resolve_project(&[file1, file2], &env);
    assert!(
        !proj
            .file(1)
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "a project module colliding with an assembly class must be an opaque open"
    );
}

#[test]
fn open_type_target_resolves_against_the_enclosing_namespace() {
    // `open type Calc` inside `namespace Demo`, with no shadowing open, binds to
    // the enclosing namespace's `Demo.Calc` (FCS-verified), so a bare `Zero`
    // resolves to `Demo.Calc.Zero`. (An explicit `open` would take precedence over
    // the enclosing namespace — see the differential — so the container is only a
    // fallback.)
    let env = fixture_env();
    let src = "namespace Demo\nmodule M =\n    open type Calc\n    let x = Zero()\n";
    let rf = resolve(src, &env);
    let calc = env.lookup_type(&["Demo".to_string()], "Calc", 0).unwrap();
    match rf.resolution_at(at(src, "Zero")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(
                parent, calc,
                "the open type binds to the enclosing Demo.Calc"
            );
            assert_eq!(member_name(env.member_at(parent, idx)), "Zero");
        }
        other => panic!("expected Demo.Calc.Zero via the enclosing namespace, got {other:?}"),
    }
}

#[test]
fn project_value_does_not_shadow_an_open_type_target() {
    // An earlier file's `module Demo` has a *value* `Calc` (`let Calc = 1`). A
    // later file `open type Demo.Calc` resolves a TYPE: the value is in a
    // different namespace and must not shadow it, so the assembly type `Demo.Calc`
    // opens and bare `Zero` resolves to `Demo.Calc.Zero` (FCS: value/type
    // namespaces are distinct). The value-aware project-shadow check is for
    // *expression* paths; the open-type target uses the type-namespace one.
    let env = fixture_env();
    let file1 = impl_file("module Demo\nlet Calc = 1\n");
    let src2 = "module M\nopen type Demo.Calc\nlet x = Zero()\n";
    let file2 = impl_file(src2);
    let proj = resolve_project(&[file1, file2], &env);
    let calc = env.lookup_type(&["Demo".to_string()], "Calc", 0).unwrap();
    match proj.file(1).resolution_at(at(src2, "Zero")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(
                parent, calc,
                "a project value must not shadow the open type"
            );
            assert_eq!(member_name(env.member_at(parent, idx)), "Zero");
        }
        other => panic!("expected Demo.Calc.Zero (value must not shadow the type), got {other:?}"),
    }
}

#[test]
fn plain_open_of_a_class_does_not_import_statics() {
    // A *plain* `open Demo.Calc` (a class, no `type`) does NOT import the class's
    // static members in F# — only `open type` does. So bare `Zero` must not
    // resolve to `Demo.Calc.Zero` (FCS leaves it undefined).
    let env = fixture_env();
    let src = "open Demo.Calc\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Zero")),
            None | Some(Resolution::Deferred(_))
        ),
        "a plain `open <class>` must not import the class's statics"
    );
}

#[test]
fn open_type_of_a_project_shadowed_path_does_not_use_the_assembly() {
    // `open type Demo.Calc` where `Demo.Calc` is *also* an in-project type (here
    // `module Demo` + `type Calc`) must not model the referenced-assembly
    // `Demo.Calc` (the project type shadows it, and we don't model project-type
    // statics) — bare `Zero` defers rather than resolving to the assembly.
    let env = fixture_env();
    let src = "module Demo\ntype Calc = int\nopen type Demo.Calc\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        !rf.resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "a project-shadowed open type must not resolve a bare name to the assembly"
    );
}

#[test]
fn an_unmodelled_open_type_defers_a_modelled_static_member() {
    // A modelled `open type Demo.Calc` plus an *unmodelled* `open type Local` (an
    // in-project type whose statics we cannot enumerate) — the unmodelled open
    // could also provide `Zero`, so a bare `Zero` defers rather than picking the
    // modelled `Demo.Calc.Zero`.
    let env = fixture_env();
    let src = "type Local = int\nopen type Local\nopen type Demo.Calc\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        !rf.resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Member { .. })),
        "an unmodelled open type in scope must defer bare-name resolution"
    );
}

#[test]
fn two_open_types_resolve_a_bare_static_to_the_later_open() {
    // Both `Demo.Calc` and `Demo.Sub.Calc` have a static `Zero`. Opening both,
    // the *later* open wins: bare `Zero` is `Demo.Sub.Calc.Zero` (FCS-verified —
    // opens share one source-ordered, latest-wins frame, so this is no longer an
    // ambiguity we defer). Pinned against FCS in `resolve_assembly_diff.rs`.
    let env = fixture_env();
    let src = "open type Demo.Calc\nopen type Demo.Sub.Calc\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    let sub_calc = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Calc", 0)
        .expect("Demo.Sub.Calc in env");
    match rf.resolution_at(at(src, "Zero")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, sub_calc, "the later open type wins");
            assert_eq!(member_name(env.member_at(parent, idx)), "Zero");
        }
        other => panic!("expected Demo.Sub.Calc.Zero via the later open, got {other:?}"),
    }
}

#[test]
fn an_earlier_local_is_shadowed_by_a_later_open_type_static() {
    // A top-level `let Answer = 9` then `open type Demo.Calc`: the *later* open
    // shadows the earlier local, so bare `Answer` is `Demo.Calc.Answer` (FCS-
    // verified). Locals and opened statics share one source-ordered frame. The
    // reverse order keeps the local — see
    // `local_binding_shadows_an_opened_type_static_member`. Pinned against FCS in
    // `resolve_assembly_diff.rs`.
    let env = fixture_env();
    let src = "let Answer = 9\nopen type Demo.Calc\nlet y = Answer\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at_last(src, "Answer")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(
                member_name(env.member_at(parent, idx)),
                "Answer",
                "the later open type shadows the earlier local"
            );
        }
        other => {
            panic!("expected Demo.Calc.Answer (later open shadows earlier local), got {other:?}")
        }
    }
}

#[test]
fn local_binding_shadows_an_opened_name() {
    // Even with `open Demo` in scope, a local `Calc` wins — lexical scope is
    // searched before imports, matching F#.
    let env = fixture_env();
    let src = "open Demo\nlet Calc = 1\nlet y = Calc\n";
    let rf = resolve(src, &env);

    // The use on the last line resolves to the local Item, not Demo.Calc.
    let res = rf
        .resolution_at(at_last(src, "Calc"))
        .expect("a resolution");
    assert!(
        matches!(res, Resolution::Item(_)),
        "a local binding must shadow the opened name, got {res:?}"
    );
}

#[test]
fn latest_open_wins_when_two_opens_declare_the_same_name() {
    // Both `Demo` and `Demo.Sub` declare a `Calc`. F# is **latest-open-wins** (not
    // ambiguity): the later `open Demo.Sub` shadows the earlier `open Demo`, so
    // `Calc.Zero` is `Demo.Sub.Calc.Zero` (FCS-verified). (Was `ambiguous_opens_defer`.)
    let env = fixture_env();
    let src = "open Demo\nopen Demo.Sub\nlet x = Calc.Zero()\n";
    let rf = resolve(src, &env);
    let demo_sub_calc = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Calc", 0)
        .unwrap();
    assert_eq!(
        rf.resolution_at(at(src, "Calc")),
        Some(Resolution::Entity(demo_sub_calc)),
        "the later `open Demo.Sub` shadows the earlier `open Demo`"
    );
    match rf.resolution_at(at(src, "Calc.Zero")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, demo_sub_calc, "member parent is Demo.Sub.Calc");
            assert_eq!(member_name(env.member_at(parent, idx)), "Zero");
        }
        other => panic!("expected the `Zero` member of `Demo.Sub.Calc`, got {other:?}"),
    }
}

#[test]
fn latest_open_root_reading_shadows_an_earlier_open() {
    // `Widget` is in root `Sub` and root `Zap`, but NOT in the relative `Demo.Sub`.
    // In `namespace Demo`, `open Zap; open Sub; (x: Widget)` is `Sub.Widget`: the
    // *later* `open Sub`'s **root** reading (its relative `Demo.Sub` has no
    // `Widget`) shadows the earlier `open Zap` — a relative open's root reading is
    // ordered at *its* open's source position, not globally last (FCS; codex R3
    // [P2-1]).
    let env = fixture_env();
    let src =
        "namespace Demo\n\nmodule M =\n    open Zap\n    open Sub\n    let f (x : Widget) = x\n";
    let rf = resolve(src, &env);
    let sub_widget = env.lookup_type(&["Sub".to_string()], "Widget", 0).unwrap();
    assert_eq!(
        rf.resolution_at(at(src, "Widget")),
        Some(Resolution::Entity(sub_widget)),
        "the later `open Sub`'s root reading `Sub.Widget` shadows the earlier `open Zap`",
    );
}

#[test]
fn a_later_open_chains_through_both_readings_of_an_earlier_open() {
    // A later `open Extra` chains through **both** readings of the earlier
    // `open Sub` (its relative `Demo.Sub` and merged root `Sub`), naming both
    // `Demo.Sub.Extra` and `Sub.Extra` — relative first (FCS; codex R3 [P2-2] and
    // R4). So under `namespace Demo; open Sub; open Extra`:
    //   * a relative-only `RelThing` → `Demo.Sub.Extra.RelThing`,
    //   * a root-only `ExtraThing`   → `Sub.Extra.ExtraThing` (falls to the root),
    //   * a colliding `Shared`       → `Demo.Sub.Extra.Shared` (relative wins).
    // The last is the R4 regression: the relative chained reading must out-rank the
    // root one, not the reverse.
    let env = fixture_env();
    let prelude = "namespace Demo\n\nmodule M =\n    open Sub\n    open Extra\n    let f (x : ";
    let demo_sub_extra = &["Demo".to_string(), "Sub".to_string(), "Extra".to_string()];
    let sub_extra = &["Sub".to_string(), "Extra".to_string()];

    let src = format!("{prelude}RelThing) = x\n");
    let rf = resolve(&src, &env);
    assert_eq!(
        rf.resolution_at(at(&src, "RelThing")),
        Some(Resolution::Entity(
            env.lookup_type(demo_sub_extra, "RelThing", 0).unwrap()
        )),
        "relative-only name chains through `Demo.Sub.Extra`",
    );

    let src = format!("{prelude}ExtraThing) = x\n");
    let rf = resolve(&src, &env);
    assert_eq!(
        rf.resolution_at(at(&src, "ExtraThing")),
        Some(Resolution::Entity(
            env.lookup_type(sub_extra, "ExtraThing", 0).unwrap()
        )),
        "root-only name falls to the root chained reading `Sub.Extra`",
    );

    let src = format!("{prelude}Shared) = x\n");
    let rf = resolve(&src, &env);
    assert_eq!(
        rf.resolution_at(at(&src, "Shared")),
        Some(Resolution::Entity(
            env.lookup_type(demo_sub_extra, "Shared", 0).unwrap()
        )),
        "a name in BOTH resolves the *relative* `Demo.Sub.Extra.Shared` (R4: relative out-ranks root)",
    );
}

#[test]
fn nested_module_exact_name_as_type_resolves_the_assembly() {
    // A module is not a type: a nested `module Calc`'s **own name** used as a type
    // (`(x: Calc)`) resolves the opened assembly type `Demo.Calc`, not a defer —
    // only a *proper descent* through the nested module (`Calc.Inner`) vetoes
    // (FCS; codex R3 [P2-3], the strict-prefix fix to
    // `type_path_descends_into_nested_module`).
    let env = fixture_env();
    let src = "module M\nopen Demo\nmodule Calc =\n    let placeholder = 1\nlet f (x : Calc) = x\n";
    let rf = resolve(src, &env);
    let demo_calc = env.lookup_type(&["Demo".to_string()], "Calc", 0).unwrap();
    let calc_at = {
        let s = src.rfind("Calc").expect("annotation use of Calc");
        span(s, "Calc".len())
    };
    assert_eq!(
        rf.resolution_at(calc_at),
        Some(Resolution::Entity(demo_calc)),
        "a nested module's own name as a type resolves the assembly `Demo.Calc`",
    );
}

#[test]
fn unknown_open_does_not_resolve() {
    // Opening a namespace that nothing declares must not make a path resolve
    // (and never `Unresolved`). `Thing.Go` exists under `Demo` — if the bogus
    // open leaked a reading, `Thing` would bind — but has no root reading (the
    // global namespace has no `Thing`; `Calc` would partially resolve there).
    let env = fixture_env();
    let src = "open Nope\nlet x = Thing.Go()\n";
    let rf = resolve(src, &env);

    for r in rf.resolutions().values() {
        assert!(
            !matches!(r, Resolution::Entity(_) | Resolution::Member { .. }),
            "a bogus open must not resolve into the assembly, got {r:?}"
        );
        assert!(!matches!(r, Resolution::Unresolved), "never Unresolved");
    }
}

#[test]
fn open_of_a_project_module_defers_a_name_an_assembly_open_could_resolve() {
    // file1 is `module B` exporting `Calc`. file2 has `open Demo` (the assembly
    // namespace, with `Demo.Calc.Zero`) *and* `open B` (a project module that
    // brings `B.Calc` into scope). `Calc.Zero` could resolve via the project
    // open `B` — which we don't model — so even though `open Demo` yields an
    // assembly candidate, we must defer, not record `Demo.Calc.Zero`.
    let env = fixture_env();
    let file1 = impl_file("module B\nlet Calc = 1\n");
    let file2 = impl_file("open Demo\nopen B\nlet x = Calc.Zero\n");
    let proj = resolve_project(&[file1, file2], &env);

    assert!(
        !proj
            .file(1)
            .resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "an active project-module open must defer a name an assembly open could resolve"
    );
}

#[test]
fn anonymous_file_nested_module_does_not_shadow_a_cross_file_assembly_path() {
    // file1 is *anonymous* (opens with a top-level `let`), and nests
    // `module Demo = module Calc = …`. Those nested names are cross-file
    // reachable only via the implicit *filename* module (`<File1>.Demo.Calc`),
    // NOT as bare `Demo.Calc`. So a later file's `Demo.Calc.Answer` must resolve
    // to the *assembly* `Demo.Calc.Answer` (the fixture type), not be deferred by
    // a leaked bare-`Demo.Calc` project shadow.
    let env = fixture_env();
    let file1 =
        impl_file("let top = 1\nmodule Demo =\n    module Calc =\n        let placeholder = 1\n");
    let file2 = impl_file("module Other\nlet x = Demo.Calc.Answer\n");
    let proj = resolve_project(&[file1, file2], &env);
    let rf = proj.file(1);

    let src2 = "module Other\nlet x = Demo.Calc.Answer\n";
    match rf.resolution_at(at(src2, "Demo.Calc.Answer")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(member_name(env.member_at(parent, idx)), "Answer");
        }
        other => panic!(
            "expected the assembly Member for Demo.Calc.Answer (the anonymous \
             file's nested `Demo.Calc` must not bare-shadow it), got {other:?}"
        ),
    }
}

#[test]
fn open_resolves_through_a_head_named_like_a_member_less_project_module() {
    // file1 is `module Calc` (a project module) that does NOT export `Answer`.
    // file2 has `open Demo`, then `Calc.Answer`. The written head `Calc` could
    // be the project module, but it lacks `Answer`, so F# falls through to the
    // assembly type `Demo.Calc` brought into scope by `open Demo`: `Calc.Answer`
    // resolves to `Demo.Calc.Answer`. (FCS-verified.)
    let env = fixture_env();
    let src2 = "open Demo\nlet x = Calc.Answer\n";
    let file1 = impl_file("module Calc\nlet placeholder = 1\n");
    let file2 = impl_file(src2);
    let proj = resolve_project(&[file1, file2], &env);
    let rf = proj.file(1);

    let calc = env.lookup_type(&["Demo".to_string()], "Calc", 0).unwrap();
    assert_eq!(
        rf.resolution_at(at(src2, "Calc")),
        Some(Resolution::Entity(calc)),
        "the unqualified `Calc` resolves to Demo.Calc via the open"
    );
    match rf.resolution_at(at(src2, "Calc.Answer")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, calc);
            assert_eq!(member_name(env.member_at(parent, idx)), "Answer");
        }
        other => panic!("expected Member for Calc.Answer via open, got {other:?}"),
    }
}

#[test]
fn an_unmodelled_module_open_defers_a_namespace_opened_name() {
    // `open Demo` (a namespace, with `Demo.Calc.Zero`) plus `open Demo.Thing`
    // (a *type* — F# `open` of a module/type brings its members into scope,
    // which we don't model). The opened type could provide `Calc`, so even
    // though `open Demo` yields an assembly candidate, we must defer.
    let env = fixture_env();
    let src = "open Demo\nopen Demo.Thing\nlet x = Calc.Zero\n";
    let rf = resolve(src, &env);

    assert!(
        !rf.resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "an unmodelled module/type open in scope must defer a namespace-opened name"
    );
}

#[test]
fn global_qualified_open_is_normalized() {
    // `open global.Demo` is F#'s root-qualified form of `open Demo`. With a later
    // `open Demo.Sub` also in scope (both declaring `Calc`), latest-open-wins picks
    // `Demo.Sub.Calc` — so `Calc.Zero` is `Demo.Sub.Calc.Zero`. This pins that the
    // `global.` prefix is normalised to `Demo` (so the earlier open is a real
    // `open Demo`, shadowed by the later `open Demo.Sub`), not left unresolved.
    let env = fixture_env();
    let src = "open global.Demo\nopen Demo.Sub\nlet x = Calc.Zero()\n";
    let rf = resolve(src, &env);
    let demo_sub_calc = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Calc", 0)
        .unwrap();
    assert_eq!(
        rf.resolution_at(at(src, "Calc")),
        Some(Resolution::Entity(demo_sub_calc)),
        "`open global.Demo` normalises to `open Demo`; the later `open Demo.Sub` wins",
    );
}

#[test]
fn nested_type_open_is_unmodelled_and_defers() {
    // `open Demo.Thing.Inner` opens a *nested* type/module (`Inner` is nested in
    // `Demo.Thing`). Its members come into scope unmodelled, so with `open Demo`
    // also active, `Calc.Zero` must defer — the nested open could supply `Calc`.
    let env = fixture_env();
    let src = "open Demo\nopen Demo.Thing.Inner\nlet x = Calc.Zero\n";
    let rf = resolve(src, &env);

    assert!(
        !rf.resolutions()
            .values()
            .any(|r| matches!(r, Resolution::Entity(_) | Resolution::Member { .. })),
        "an open of a nested assembly type/module must defer like a top-level one"
    );
}

#[test]
fn inaccessible_type_open_does_not_suppress_other_opens() {
    // `open Demo.Hidden` targets an *internal* type — F# cannot open it
    // cross-assembly, so it brings no members into scope and must not suppress
    // the valid `open Demo`. `Calc.Zero` therefore still resolves to
    // `Demo.Calc.Zero` despite the inaccessible open in scope. (Contrast
    // `nested_type_open_is_unmodelled_and_defers`, where the opened type is
    // *public* and so does suppress.) FCS-verified in `resolve_assembly_diff`.
    let env = fixture_env();
    let src = "open Demo\nopen Demo.Hidden\nlet x = Calc.Zero()\n";
    let rf = resolve(src, &env);

    let calc = env.lookup_type(&["Demo".to_string()], "Calc", 0).unwrap();
    assert_eq!(
        rf.resolution_at(at(src, "Calc")),
        Some(Resolution::Entity(calc)),
        "the inaccessible `open Demo.Hidden` must not suppress `open Demo`"
    );
    match rf.resolution_at(at(src, "Calc.Zero")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, calc);
            assert_eq!(member_name(env.member_at(parent, idx)), "Zero");
        }
        other => panic!("expected Member for Calc.Zero, got {other:?}"),
    }
}

#[test]
fn qualified_project_value_defers_under_an_open_type() {
    // An `open type T` whose nested types we do not model is in scope, so a
    // relative qualified value `Mod.v` (head name-shortened to `Demo.Mod`) is
    // deferred — the opened type could have a nested type `Mod` that shadows the
    // project module. FCS resolves `Demo.Mod.v` here (it knows `Calc` has no
    // nested `Mod`); we cannot enumerate `Calc`'s nested types, so we
    // conservatively defer (sound — a coverage gap, never a wrong go-to-definition;
    // the same policy `unmodelled_open_active` enforces on the assembly path).
    let env = fixture_env();
    let src = "namespace Demo\nmodule Mod =\n    let v = 1\nmodule N =\n    open type Calc\n    let x = Mod.v\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "Mod.v")) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("`Mod.v` under an active `open type` should defer, got {other:?}"),
    }
}

#[test]
fn qualified_project_value_resolves_without_an_open_type() {
    // Control: the same sibling reference `Mod.v` *without* an `open type` in scope
    // resolves to the project module value (`Demo.Mod.v`) — the conservative
    // deferral above is gated on the unmodelled type open, not on assembly
    // references in general.
    let env = fixture_env();
    let src = "namespace Demo\nmodule Mod =\n    let v = 1\nmodule N =\n    let x = Mod.v\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Mod.v")),
            Some(Resolution::Item(_))
        ),
        "`Mod.v` should resolve to the project module value without an open type"
    );
}

#[test]
fn opened_static_head_is_member_access_not_a_module_path() {
    // `open type Demo.Calc` brings the static `Answer` into scope; `Answer.x` is
    // member access on that static value, not a module/assembly path. The head
    // `Answer` must resolve to the opened static (`Member`), not be re-resolved as
    // a qualified module/assembly path (FCS: `Answer` → `Demo.Calc.Answer`). Guards
    // that a non-case value head still blocks the dotted-path resolution.
    let env = fixture_env();
    let src = "open type Demo.Calc\nlet x = Answer.id\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Answer")),
            Some(Resolution::Member { .. })
        ),
        "the opened static head `Answer` must resolve to its Member (member access), got {:?}",
        rf.resolution_at(at(src, "Answer")),
    );
}

#[test]
fn open_of_a_project_namespace_still_opens_the_assembly_namespace() {
    // codex review (open-arm consolidation): `open Demo` from `namespace Outer`
    // resolves the relative project namespace `Outer.Demo` *and* the
    // referenced-assembly namespace `Demo` — F# opens both. The assembly
    // interpretation of the as-written path must survive the project-namespace
    // match, so a later `Calc.Zero()` still resolves the assembly member
    // `Demo.Calc.Zero`.
    let env = fixture_env();
    let f0 = impl_file("namespace Outer.Demo\ntype T = X | Y\n");
    let src1 = "namespace Outer\nmodule C =\n    open Demo\n    let z = Calc.Zero()\n";
    let proj = resolve_project(&[f0, impl_file(src1)], &env);
    let rf = proj.file(1);

    let i = src1.find("Calc.Zero").expect("use");
    match rf.resolution_at(span(i, "Calc.Zero".len())) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(member_name(env.member_at(parent, idx)), "Zero");
        }
        other => panic!("expected assembly Member for `Calc.Zero`, got {other:?}"),
    }
}

// ============================================================================
// Type position: referenced-assembly types named in a *type* (annotation,
// parameter type, …) rather than an expression. Stage E resolved type-qualified
// *value/member* paths; these pin the *type-reference* counterpart — the
// `Resolution::Entity` a type name records — arity-aware and shortened by a
// namespace `open`. The assembly-only envelope: a fully-qualified path or a
// plain `open <namespace>` (the complete type scope there is the AssemblyEnv).
// `resolve_assembly_diff.rs` checks these against FCS, including a strengthened
// *completeness* property.
// ============================================================================

#[test]
fn fq_type_in_annotation_resolves_to_its_entity() {
    let env = fixture_env();
    let src = "module M\nlet f (x : Demo.Thing) = x\n";
    let rf = resolve(src, &env);
    let thing = env
        .lookup_type(&["Demo".to_string()], "Thing", 0)
        .expect("Demo.Thing in env");
    assert_eq!(
        rf.resolution_at(at(src, "Thing")),
        Some(Resolution::Entity(thing)),
        "the `Thing` type segment resolves to the Demo.Thing entity"
    );
}

#[test]
fn opened_namespace_type_in_annotation_resolves_to_its_entity() {
    let env = fixture_env();
    let src = "open Demo\nlet f (x : Thing) = x\n";
    let rf = resolve(src, &env);
    let thing = env
        .lookup_type(&["Demo".to_string()], "Thing", 0)
        .expect("Demo.Thing in env");
    assert_eq!(
        rf.resolution_at(at(src, "Thing")),
        Some(Resolution::Entity(thing)),
        "`open Demo` lets the bare type `Thing` resolve to Demo.Thing"
    );
}

#[test]
fn generic_type_in_annotation_is_arity_aware() {
    // `Demo.Pair`, ``Pair`1``, ``Pair`2`` are distinct CLR type defs; the generic
    // arity written in the annotation selects which one — a bare `Pair` is the
    // non-generic, `Pair<int>` the arity-1, `Pair<int,string>` the arity-2.
    let env = fixture_env();
    let pair0 = env.lookup_type(&["Demo".to_string()], "Pair", 0).unwrap();
    let pair1 = env.lookup_type(&["Demo".to_string()], "Pair", 1).unwrap();
    let pair2 = env.lookup_type(&["Demo".to_string()], "Pair", 2).unwrap();

    let s1 = "open Demo\nlet f (x : Pair<int>) = x\n";
    assert_eq!(
        resolve(s1, &env).resolution_at(at(s1, "Pair")),
        Some(Resolution::Entity(pair1)),
        "`Pair<int>` is the arity-1 Pair"
    );

    let s2 = "let f (x : Demo.Pair<int, string>) = x\n";
    assert_eq!(
        resolve(s2, &env).resolution_at(at(s2, "Pair")),
        Some(Resolution::Entity(pair2)),
        "`Demo.Pair<int, string>` is the arity-2 Pair"
    );

    let s0 = "open Demo\nlet f (x : Pair) = x\n";
    assert_eq!(
        resolve(s0, &env).resolution_at(at(s0, "Pair")),
        Some(Resolution::Entity(pair0)),
        "bare `Pair` is the non-generic Pair"
    );
}

#[test]
fn wrong_arity_type_does_not_resolve() {
    // No ``Pair`3`` exists; an arity-3 reference must not resolve to a Pair of a
    // different arity (correctness over availability — never a wrong entity).
    let env = fixture_env();
    let src = "open Demo\nlet f (x : Pair<int, string, bool>) = x\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Pair")),
            None | Some(Resolution::Deferred(_))
        ),
        "an arity with no matching type def must not resolve, got {:?}",
        rf.resolution_at(at(src, "Pair")),
    );
}

#[test]
fn nested_type_in_annotation_resolves_via_the_nested_walk() {
    let env = fixture_env();
    let src = "let f (x : Demo.Thing.Inner) = x\n";
    let rf = resolve(src, &env);
    let thing = env.lookup_type(&["Demo".to_string()], "Thing", 0).unwrap();
    let inner = env.nested(thing, "Inner", 0).expect("Thing.Inner");
    assert_eq!(
        rf.resolution_at(at(src, "Inner")),
        Some(Resolution::Entity(inner)),
        "`Demo.Thing.Inner` resolves the nested `Inner` to its entity"
    );
    assert_eq!(
        rf.resolution_at(at(src, "Thing")),
        Some(Resolution::Entity(thing)),
        "the `Thing` encloser segment resolves to its entity too"
    );
}

#[test]
fn latest_open_wins_for_a_bare_annotation_type() {
    // `Calc` is both `Demo.Calc` and `Demo.Sub.Calc`; with both namespaces opened,
    // F# is **latest-open-wins** — the later `open Demo.Sub` shadows `open Demo`,
    // so `(x : Calc)` is `Demo.Sub.Calc` (FCS-verified). (Was
    // `ambiguous_opened_type_in_annotation_defers`.)
    let env = fixture_env();
    let src = "open Demo\nopen Demo.Sub\nlet f (x : Calc) = x\n";
    let rf = resolve(src, &env);
    let demo_sub_calc = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Calc", 0)
        .unwrap();
    assert_eq!(
        rf.resolution_at(at(src, "Calc")),
        Some(Resolution::Entity(demo_sub_calc)),
        "the later `open Demo.Sub` wins the bare annotation type",
    );
}

#[test]
fn in_file_type_shadows_a_colliding_assembly_type_in_annotation() {
    // A single-segment type name that is an in-file `type` def resolves there
    // (a `Local`), never to a same-named assembly entity — the local definition
    // is the more-specific, in-scope one.
    let env = fixture_env();
    let src = "type Thing = int\nlet f (x : Thing) = x\n";
    let rf = resolve(src, &env);
    // The *use* of `Thing` in the annotation is its second occurrence.
    let use_range = {
        let s = src.rfind("Thing").expect("annotation use of Thing");
        span(s, "Thing".len())
    };
    assert!(
        matches!(rf.resolution_at(use_range), Some(Resolution::Local(_))),
        "an in-file `type Thing` shadows the assembly Demo.Thing in type position, got {:?}",
        rf.resolution_at(use_range),
    );
}

#[test]
fn bare_type_resolves_through_the_enclosing_namespace() {
    // Inside `namespace Demo`, a bare `Thing` resolves against the enclosing
    // namespace (`Demo.Thing`) — tier 2 of the type-path precedence (no open, no
    // root `Thing`). Matches FCS.
    let env = fixture_env();
    let src = "namespace Demo\n\nmodule M =\n    let f (x : Thing) = x\n";
    let rf = resolve(src, &env);
    let thing = env
        .lookup_type(&["Demo".to_string()], "Thing", 0)
        .expect("Demo.Thing in env");
    assert_eq!(
        rf.resolution_at(at(src, "Thing")),
        Some(Resolution::Entity(thing)),
        "a bare `Thing` under `namespace Demo` resolves to Demo.Thing"
    );
}

#[test]
fn qualified_type_prefers_the_enclosing_namespace_over_the_root() {
    // `namespace Demo; x : Sub.Thing` — `Sub` resolves relative to the enclosing
    // `Demo` first (→ `Demo.Sub.Thing`), never the root `Sub.Thing`, even though
    // both exist. Tier 2 (enclosing namespace) out-ranks tier 3 (root). Matches FCS.
    let env = fixture_env();
    let src = "namespace Demo\n\nmodule M =\n    let f (x : Sub.Thing) = x\n";
    let rf = resolve(src, &env);
    let demo_sub_thing = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Thing", 0)
        .expect("Demo.Sub.Thing in env");
    let root_sub_thing = env
        .lookup_type(&["Sub".to_string()], "Thing", 0)
        .expect("root Sub.Thing in env");
    assert_eq!(
        rf.resolution_at(at(src, "Thing")),
        Some(Resolution::Entity(demo_sub_thing)),
        "`Sub.Thing` under `namespace Demo` is the relative `Demo.Sub.Thing`"
    );
    assert_ne!(
        rf.resolution_at(at(src, "Thing")),
        Some(Resolution::Entity(root_sub_thing)),
        "must not bind the root `Sub.Thing`"
    );
}

#[test]
fn project_namespace_and_relative_assembly_namespace_both_open() {
    // Stage 3 (namespace merge): an earlier file declares a *project*
    // `namespace Sub`, and the referenced assembly has a relative `Demo.Sub`. In
    // `namespace Demo`, `open Sub` opens **both** — the project namespace *and*
    // the canonicalised assembly `Demo.Sub` (F#; dropping the old
    // `!is_project_namespace_path` gate). So a later `Deep`, which lives only in
    // the assembly `Demo.Sub`, resolves to `Demo.Sub.Deep` — not the wrong root
    // `Sub.*`, and no longer a sound under-resolution. (Was
    // `…_suppressing_…_under_resolves_soundly`; the #595 KNOWN BOUNDARY is closed.)
    let env = fixture_env();
    let file1 = impl_file("namespace Sub\n\ntype Marker = int\n");
    let src2 = "namespace Demo\n\nmodule M =\n    open Sub\n    let f (x : Deep) = x\n";
    let file2 = impl_file(src2);
    let proj = resolve_project(&[file1, file2], &env);
    let rf = proj.file(1);
    let use_range = {
        let s = src2.rfind("Deep").expect("annotation use of Deep");
        span(s, "Deep".len())
    };
    let demo_sub_deep = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Deep", 0)
        .expect("fixture has Demo.Sub.Deep");
    assert_eq!(
        rf.resolution_at(use_range),
        Some(Resolution::Entity(demo_sub_deep)),
        "`open Sub` in `namespace Demo` resolves the assembly-only `Deep` to Demo.Sub.Deep",
    );
}

#[test]
fn merged_root_open_does_not_leak_across_top_level_blocks() {
    // SOUNDNESS (block scope): `open Sub` in `namespace Demo` opens the relative
    // `Demo.Sub` *and*, at lower priority, the root `Sub` (one `OpenGroup` in
    // `Resolver::imports`). That open is scoped to its top-level block — a
    // *sibling* block with no such open must NOT see it, so `RootOnly` (reachable
    // only through the root `Sub`) must defer in the second block, never resolve
    // to the leaked `Sub.RootOnly`. Guards the per-block reset of `imports` to
    // `implicit_open_groups()` in `resolve_file`.
    let env = fixture_env();
    let src = "namespace Demo\n\nmodule A =\n    open Sub\n    let a = 1\n\nnamespace Other\n\nmodule B =\n    let f (x : RootOnly) = x\n";
    let rf = resolve(src, &env);
    let root_only = env
        .lookup_type(&["Sub".to_string()], "RootOnly", 0)
        .unwrap();
    let use_range = {
        let s = src.rfind("RootOnly").expect("annotation use of RootOnly");
        span(s, "RootOnly".len())
    };
    assert_ne!(
        rf.resolution_at(use_range),
        Some(Resolution::Entity(root_only)),
        "a merged-root `open Sub` must not leak `Sub.RootOnly` into a sibling block",
    );
}

#[test]
fn nested_namespace_direct_expression_ref_resolves_through_the_enclosing_namespace() {
    // COMPLETENESS (expression/value path, stage 2): `namespace Demo;
    // Sub.Calc.Zero()` — F# resolves `Sub` through the enclosing `Demo` (tier 2),
    // so `Calc` is `Demo.Sub.Calc` and the whole path the `Zero` member of it,
    // never the root `Sub.Calc`. The shared tier walker now gives the value/member
    // path this enclosing-namespace tier (it previously deferred). Matches FCS.
    let env = fixture_env();
    let src = "namespace Demo\n\nmodule M =\n    let z = Sub.Calc.Zero()\n";
    let rf = resolve(src, &env);
    let demo_sub_calc = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Calc", 0)
        .unwrap();
    let root_calc = env.lookup_type(&["Sub".to_string()], "Calc", 0).unwrap();
    assert_eq!(
        rf.resolution_at(at(src, "Calc")),
        Some(Resolution::Entity(demo_sub_calc)),
        "the `Calc` qualifier resolves to `Demo.Sub.Calc`, not the root"
    );
    assert_ne!(
        rf.resolution_at(at(src, "Calc")),
        Some(Resolution::Entity(root_calc)),
        "must not bind the root `Sub.Calc`"
    );
    let whole = {
        let s = src.find("Sub.Calc.Zero").unwrap();
        span(s, "Sub.Calc.Zero".len())
    };
    match rf.resolution_at(whole) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, demo_sub_calc, "member parent is Demo.Sub.Calc");
            assert_eq!(member_name(env.member_at(parent, idx)), "Zero");
        }
        other => panic!("expected the `Zero` member of `Demo.Sub.Calc`, got {other:?}"),
    }
}

#[test]
fn unmodelled_open_does_not_bind_root_when_the_enclosing_reading_differs() {
    // SOUNDNESS: an `open type Demo.Calc` makes `unmodelled_open_active`, so the
    // relative readings are unsafe. But the root reading is the absolute winner
    // only if no nearer namespace reading would win — and `namespace Demo;
    // Sub.Calc.Zero()` has an enclosing `Demo.Sub.Calc` differing from the root
    // `Sub.Calc`. We must defer, not bind the wrong root `Sub.Calc`.
    let env = fixture_env();
    let src =
        "namespace Demo\n\nmodule M =\n    open type Demo.Calc\n    let z = Sub.Calc.Zero()\n";
    let rf = resolve(src, &env);
    let root_calc = env.lookup_type(&["Sub".to_string()], "Calc", 0).unwrap();
    // The `Calc` qualifier in `Sub.Calc` is the last `Calc` (the first is in the
    // `open type Demo.Calc` clause).
    let calc_seg = {
        let s = src.rfind("Calc").unwrap();
        span(s, "Calc".len())
    };
    assert_ne!(
        rf.resolution_at(calc_seg),
        Some(Resolution::Entity(root_calc)),
        "must not bind the root `Sub.Calc` under an unmodelled open with a differing enclosing reading"
    );
    // The whole `Sub.Calc.Zero` member must not be the root member either.
    let whole = {
        let s = src.rfind("Sub.Calc.Zero").unwrap();
        span(s, "Sub.Calc.Zero".len())
    };
    assert!(
        !matches!(
            rf.resolution_at(whole),
            Some(Resolution::Member { parent, .. }) if parent == root_calc
        ),
        "must not bind the root `Sub.Calc.Zero` member, got {:?}",
        rf.resolution_at(whole),
    );
}

#[test]
fn unmodelled_open_does_not_bind_root_when_an_explicit_open_reading_differs() {
    // SOUNDNESS (sibling of `unmodelled_open_does_not_bind_root_when_the_enclosing_
    // reading_differs`): the differing higher-precedence reading can come from an
    // explicit `open`, not only the enclosing namespace. Here there is *no*
    // enclosing namespace, but `open Demo` (a modelled namespace open, higher
    // precedence than the root) reads `Sub.Calc.Zero` as `Demo.Sub.Calc.Zero`,
    // differing from the root `Sub.Calc.Zero`. The `open type Demo.Calc` makes
    // relative readings unsafe, so we must defer — never bind the wrong root.
    let env = fixture_env();
    let src = "module M\nopen Demo\nopen type Demo.Calc\nlet z = Sub.Calc.Zero()\n";
    let rf = resolve(src, &env);
    let root_calc = env.lookup_type(&["Sub".to_string()], "Calc", 0).unwrap();
    // The `Calc` in `Sub.Calc` is the last `Calc` (the first is in `open type`).
    let calc_seg = {
        let s = src.rfind("Calc").unwrap();
        span(s, "Calc".len())
    };
    assert_ne!(
        rf.resolution_at(calc_seg),
        Some(Resolution::Entity(root_calc)),
        "must not bind the root `Sub.Calc` when an explicit open reads the path differently"
    );
    let whole = {
        let s = src.rfind("Sub.Calc.Zero").unwrap();
        span(s, "Sub.Calc.Zero".len())
    };
    assert!(
        !matches!(
            rf.resolution_at(whole),
            Some(Resolution::Member { parent, .. }) if parent == root_calc
        ),
        "must not bind the root `Sub.Calc.Zero` member, got {:?}",
        rf.resolution_at(whole),
    );
}

#[test]
fn relative_type_does_not_reach_an_ancestor_namespace() {
    // Inside `namespace Demo.Sub`, a qualified `Sub.Calc` resolves `Sub` against
    // the *current* namespace (`Demo.Sub.Sub`, absent) then the root (`Sub.Calc`)
    // — never the *ancestor* `Demo` (which would give `Demo.Sub.Calc`). FS0039: a
    // relative path does not see ancestor namespaces. Matches FCS.
    let env = fixture_env();
    let src = "namespace Demo.Sub\n\nmodule M =\n    let f (x : Sub.Calc) = x\n";
    let rf = resolve(src, &env);
    let root_sub_calc = env
        .lookup_type(&["Sub".to_string()], "Calc", 0)
        .expect("root Sub.Calc in env");
    let demo_sub_calc = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Calc", 0)
        .expect("Demo.Sub.Calc in env");
    let use_range = {
        let s = src.rfind("Calc").expect("annotation use of Calc");
        span(s, "Calc".len())
    };
    assert_eq!(
        rf.resolution_at(use_range),
        Some(Resolution::Entity(root_sub_calc)),
        "`Sub.Calc` in `namespace Demo.Sub` is the root `Sub.Calc`, not the ancestor"
    );
    assert_ne!(
        rf.resolution_at(use_range),
        Some(Resolution::Entity(demo_sub_calc)),
        "must not reach the ancestor `Demo.Sub.Calc`"
    );
}

#[test]
fn relative_open_does_not_reach_an_ancestor_namespace() {
    // The open counterpart: inside `namespace Demo.Sub`, `open Sub` resolves to
    // the current `Demo.Sub.Sub` (absent) then root `Sub` — never the ancestor
    // `Demo.Sub`. So a later `Calc` is the root `Sub.Calc`.
    let env = fixture_env();
    let src = "namespace Demo.Sub\n\nmodule M =\n    open Sub\n    let f (x : Calc) = x\n";
    let rf = resolve(src, &env);
    let root_sub_calc = env
        .lookup_type(&["Sub".to_string()], "Calc", 0)
        .expect("root Sub.Calc in env");
    let demo_sub_calc = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Calc", 0)
        .unwrap();
    let use_range = {
        let s = src.rfind("Calc").expect("annotation use of Calc");
        span(s, "Calc".len())
    };
    assert_eq!(
        rf.resolution_at(use_range),
        Some(Resolution::Entity(root_sub_calc)),
        "`open Sub` in `namespace Demo.Sub` is the root `Sub`"
    );
    assert_ne!(
        rf.resolution_at(use_range),
        Some(Resolution::Entity(demo_sub_calc)),
        "must not canonicalise through the ancestor `Demo`"
    );
}

#[test]
fn opaque_project_module_open_defers_assembly_type_resolution() {
    // An opened project module whose contents we cannot enumerate could supply a
    // *type* `Thing` that shadows the assembly `Demo.Thing`. While any opaque open
    // is active — `unmodelled_open_active`, `opaque_dotted_open`, *or*
    // `opaque_value_open` (the last not implied by the others on the
    // `open_imports_project_values` fallback) — the type path must defer rather
    // than resolve `Thing` through the earlier `open Demo` (D5: FCS could bind
    // `M.Thing`, which we don't model — say nothing, not wrong). This pins the
    // soundness contract (defer, never a wrong assembly entity) for an opaque
    // project-module open in scope.
    let env = fixture_env();
    let src = "module M =\n    let v = 1\n\nopen Demo\nopen M\nlet f (x : Thing) = x\n";
    let rf = resolve(src, &env);
    let demo_thing = env.lookup_type(&["Demo".to_string()], "Thing", 0).unwrap();
    let use_range = {
        let s = src.rfind("Thing").expect("annotation use of Thing");
        span(s, "Thing".len())
    };
    assert_ne!(
        rf.resolution_at(use_range),
        Some(Resolution::Entity(demo_thing)),
        "an opaque project-module open must suppress the assembly `Demo.Thing`"
    );
    assert!(
        matches!(
            rf.resolution_at(use_range),
            None | Some(Resolution::Deferred(_))
        ),
        "the type defers while an opaque open is active, got {:?}",
        rf.resolution_at(use_range),
    );
}

#[test]
fn relative_open_skips_an_inaccessible_namespace_for_the_public_root() {
    // `Demo.Hush` exists in metadata but holds only an *internal* type — empty
    // cross-assembly. So in `namespace Demo`, `open Hush` must NOT canonicalise to
    // the inaccessible `Demo.Hush`; it falls back to the *public* root `Hush`, and
    // a later `Visible` resolves to `Hush.Visible`. Matches FCS.
    let env = fixture_env();
    let src = "namespace Demo\n\nmodule M =\n    open Hush\n    let f (x : Visible) = x\n";
    let rf = resolve(src, &env);
    let root_hush_visible = env
        .lookup_type(&["Hush".to_string()], "Visible", 0)
        .expect("root Hush.Visible in env");
    assert_eq!(
        rf.resolution_at(at(src, "Visible")),
        Some(Resolution::Entity(root_hush_visible)),
        "`open Hush` skips the internal-only `Demo.Hush` for the public root `Hush`"
    );
}

#[test]
fn relative_open_under_a_module_is_rooted_not_canonicalised() {
    // A *top-level module* `Demo` introduces no namespace, so `open Sub` is the
    // root `Sub` (a module name is not an enclosing namespace). A later `Thing` is
    // the root `Sub.Thing`, never `Demo.Sub.Thing`. (`namespace_depth` is 0 here,
    // so `open_namespace_readings` yields only the as-written root reading.)
    let env = fixture_env();
    let src = "module Demo\nopen Sub\nlet f (x : Thing) = x\n";
    let rf = resolve(src, &env);
    let root_sub_thing = env
        .lookup_type(&["Sub".to_string()], "Thing", 0)
        .expect("root Sub.Thing in env");
    assert_eq!(
        rf.resolution_at(at(src, "Thing")),
        Some(Resolution::Entity(root_sub_thing)),
        "`open Sub` in `module Demo` is the root `Sub`, so `Thing` is root `Sub.Thing`"
    );
}

#[test]
fn chained_open_shortens_through_a_prior_open() {
    // `open Demo; open Sub` — the second open is resolved through the first, so it
    // names `Demo.Sub` (not the root `Sub`). `Deep` lives only in `Demo.Sub`
    // (no `Demo.Deep`, no root `Sub.Deep`), so it resolves unambiguously to
    // `Demo.Sub.Deep` via the chain. Matches FCS.
    let env = fixture_env();
    let src = "open Demo\nopen Sub\nlet f (x : Deep) = x\n";
    let rf = resolve(src, &env);
    let demo_sub_deep = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Deep", 0)
        .expect("Demo.Sub.Deep in env");
    assert_eq!(
        rf.resolution_at(at(src, "Deep")),
        Some(Resolution::Entity(demo_sub_deep)),
        "`open Demo; open Sub` resolves `Deep` to `Demo.Sub.Deep` (chained open)"
    );
}

#[test]
fn generic_assembly_augmentation_head_does_not_mis_resolve_to_a_wrong_arity() {
    // `type Demo.Pair<'a> with …` augments the arity-1 ``Pair`1``. The head's
    // generic arity is not on its `long_id`, so the augmentation head must NOT
    // key an assembly lookup at arity 0 and resolve the *non-generic* `Demo.Pair`
    // — that would be a wrong go-to-definition target. It resolves in-file only
    // (here: nothing), never to the wrong-arity entity (D5).
    let env = fixture_env();
    let src = "type Demo.Pair<'a> with\n    member _.M (z: int) = z\n";
    let p = parse(src);
    // Only assert the resolution contract when the augmentation parses cleanly;
    // the point is the *head* not mis-resolving, not the augmentation syntax.
    if p.errors.is_empty() {
        let rf = resolve(src, &env);
        let pair0 = env.lookup_type(&["Demo".to_string()], "Pair", 0).unwrap();
        assert_ne!(
            rf.resolution_at(at(src, "Pair")),
            Some(Resolution::Entity(pair0)),
            "augmentation head must not resolve to the wrong-arity Pair`0"
        );
    }
}

#[test]
fn relative_namespace_open_canonicalises_to_the_enclosing_namespace() {
    // Inside `namespace Demo`, `open Sub` is the *relative* namespace `Demo.Sub`,
    // not the root `Sub` (both exist in the fixture). FCS resolves a later `Calc`
    // annotation to `Demo.Sub.Calc`; the canonicalised open lets us record exactly
    // that — never the root `Sub.Calc` (a wrong target) and never `Demo.Calc`.
    let env = fixture_env();
    let src = "namespace Demo\n\nmodule M =\n    open Sub\n    let f (x : Calc) = x\n";
    let rf = resolve(src, &env);
    let demo_sub_calc = env
        .lookup_type(&["Demo".to_string(), "Sub".to_string()], "Calc", 0)
        .expect("Demo.Sub.Calc in env");
    let root_sub_calc = env
        .lookup_type(&["Sub".to_string()], "Calc", 0)
        .expect("root Sub.Calc in env");
    let demo_calc = env.lookup_type(&["Demo".to_string()], "Calc", 0).unwrap();
    let use_range = {
        let s = src.rfind("Calc").expect("annotation use of Calc");
        span(s, "Calc".len())
    };
    assert_eq!(
        rf.resolution_at(use_range),
        Some(Resolution::Entity(demo_sub_calc)),
        "a relative `open Sub` resolves `Calc` to `Demo.Sub.Calc`"
    );
    assert_ne!(
        rf.resolution_at(use_range),
        Some(Resolution::Entity(root_sub_calc)),
        "must not bind the root `Sub.Calc`"
    );
    assert_ne!(
        rf.resolution_at(use_range),
        Some(Resolution::Entity(demo_calc)),
        "must not bind the enclosing `Demo.Calc`"
    );
}

#[test]
fn rooted_open_is_absolute_not_canonicalised() {
    // `open global.Sub` is fully rooted: it must name the *root* `Sub`, never the
    // relative `Demo.Sub`, even inside `namespace Demo`. So a later `Calc` is the
    // root `Sub.Calc`.
    let env = fixture_env();
    let src = "namespace Demo\n\nmodule M =\n    open global.Sub\n    let f (x : Calc) = x\n";
    let rf = resolve(src, &env);
    let root_sub_calc = env
        .lookup_type(&["Sub".to_string()], "Calc", 0)
        .expect("root Sub.Calc in env");
    let use_range = {
        let s = src.rfind("Calc").expect("annotation use of Calc");
        span(s, "Calc".len())
    };
    assert_eq!(
        rf.resolution_at(use_range),
        Some(Resolution::Entity(root_sub_calc)),
        "a `global.`-rooted open is absolute → root `Sub.Calc`"
    );
}

#[test]
fn explicit_open_in_anonymous_nested_module_resolves_the_assembly_type() {
    // A headerless file's `module M = …` (an anonymous-root nested module) with an
    // explicit `open Demo`: the bare type `Thing` resolves to `Demo.Thing` via the
    // open in `imports`. The current module's own name (`M`) must not be folded in
    // as an enclosing namespace — doing so produced a spurious project-shadowed
    // `M.Thing` candidate that suppressed the valid open (a completeness loss).
    let env = fixture_env();
    let src = "module M =\n    open Demo\n    let f (x : Thing) = x\n";
    let rf = resolve(src, &env);
    let thing = env
        .lookup_type(&["Demo".to_string()], "Thing", 0)
        .expect("Demo.Thing in env");
    assert_eq!(
        rf.resolution_at(at(src, "Thing")),
        Some(Resolution::Entity(thing)),
        "`open Demo` in a nested module resolves the bare `Thing` to Demo.Thing"
    );
}

// ---- Assembly types evict the head-slot value (docs/head-slot-assembly-eviction-plan.md) ----
//
// FCS's `eUnqualifiedItems` slot is one latest-wins list across the value and
// TYPE namespaces, and a type brought in by `open` enters it at the open's
// position (the M20 model). Assembly types are no exception: an `open System`
// after a `let Math = …` puts the class `System.Math` in the slot and EVICTS
// the local value, so `Math.PI` is `System.Math.PI`, not member access on the
// anon-record (probes A1/A3/Aenum/Ageneric, all dotnet-build + fcs-dump
// pinned). Sema cannot resolve the evicted head (its assembly members are
// barred for an evicted head, the M20t/M20u rule), so it must DEFER rather
// than mis-record the local value. A non-constructible kind (interface, F#
// union/record/module) keeps the value (A2/Amodule); an open BEFORE the value
// loses (A5).

/// All types in the sema C# fixture assembly.
fn fixture_entities() -> Vec<Entity> {
    let bytes = std::fs::read(ensure_assembly_fixture_built()).expect("read fixture dll");
    Ecma335Assembly::parse(&bytes)
        .expect("parse fixture dll")
        .enumerate_type_defs()
        .expect("enumerate fixture types")
}

/// An env whose only type is a public `Ns.Color` of the given kind / value-
/// type-ness / generic arity — cloned from a fixture class so its metadata
/// fields stay valid, then retargeted. `Ns` is an assembly-only namespace, so
/// `open Ns` classifies as a namespace reading (`has_namespace`) exactly like
/// `open System`.
fn env_shadow_color(kind: EntityKind, is_struct: bool, arity: usize) -> AssemblyEnv {
    let ents = fixture_entities();
    let mut e = ents
        .iter()
        .find(|e| {
            e.namespace == ["Demo"]
                && e.kind == EntityKind::Class
                && e.generic_parameters.len() == arity
        })
        .unwrap_or_else(|| panic!("no arity-{arity} Demo class in the fixture"))
        .clone();
    e.namespace = vec!["Ns".to_string()];
    e.name = "Color".to_string();
    e.kind = kind;
    e.is_struct = is_struct;
    e.members = vec![];
    e.nested_types = vec![];
    // A real F# union carries its case names in the pickle; a `None` here would
    // read as name-unknown residue (a hidden case could be anything), raising the
    // fold's generation barrier. These eviction fixtures test the *type-slot*
    // channel, not the case fold, so give the union knowable cases that cannot
    // collide with the `Color` value under test.
    if kind == EntityKind::Union {
        e.union_case_names = Some(vec!["Hue".to_string()]);
    }
    AssemblyEnv::from_entities(vec![e])
}

/// `let Color = {| Red = 3 |}` then `open Ns` (after the value) then a
/// `Color.Red` use. The head use and whole span are the sole `Color.Red`.
const ASM_EVICT_SRC: &str = "module M\nlet Color = {| Red = 3 |}\nopen Ns\nlet u = Color.Red\n";

fn asm_head_range() -> TextRange {
    span(ASM_EVICT_SRC.find("Color.Red").expect("use"), 5)
}

fn asm_binder_range() -> TextRange {
    span(ASM_EVICT_SRC.find("let Color").expect("binder") + 4, 5)
}

/// Assert the head defers — an evicted head sema cannot resolve, never the
/// local value.
fn assert_head_evicted(env: &AssemblyEnv) {
    let rf = resolve(ASM_EVICT_SRC, env);
    match rf.resolution_at(asm_head_range()) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("expected the evicted head to defer, got {other:?}"),
    }
    // The whole `Color.Red` span likewise never records the local value.
    let whole = span(
        ASM_EVICT_SRC.find("Color.Red").expect("use"),
        "Color.Red".len(),
    );
    match rf.resolution_at(whole) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("expected `Color.Red` to defer for an evicted head, got {other:?}"),
    }
}

/// Assert the head keeps — it resolves to the local `let Color` value (member
/// access on the anon-record, unmodeled, so the tail defers).
fn assert_head_kept(env: &AssemblyEnv) {
    let rf = resolve(ASM_EVICT_SRC, env);
    let res = rf
        .resolution_at(asm_head_range())
        .expect("the head resolves to the local value");
    let def = rf.resolved_def(res).expect("local value def");
    assert_eq!(
        def.range,
        asm_binder_range(),
        "the head must be the local value, not the opened assembly type"
    );
}

#[test]
fn opened_assembly_class_evicts_the_value() {
    // A1: a plain class evicts (isClassTy).
    assert_head_evicted(&env_shadow_color(EntityKind::Class, false, 0));
}

#[test]
fn opened_assembly_struct_evicts_the_value() {
    // A3: a struct evicts (isStructTy) — keyed off the reliable IL `is_struct`
    // value-type signal, not the spoofable source attribute (unlike project
    // M20m, round 7).
    assert_head_evicted(&env_shadow_color(EntityKind::Struct, true, 0));
}

#[test]
fn opened_assembly_enum_evicts_the_value() {
    // Aenum: an enum is a value type — evicts.
    assert_head_evicted(&env_shadow_color(EntityKind::Enum, true, 0));
}

#[test]
fn opened_assembly_generic_only_class_evicts_the_value() {
    // Ageneric: a generic-only class `Color<'T>` (no arity-0 form) still evicts
    // a bare head — FCS's no-arity type lookup matches it — so the slot
    // consultation must scan all arities, not fast-path arity 0.
    assert_head_evicted(&env_shadow_color(EntityKind::Class, false, 1));
}

#[test]
fn opened_assembly_struct_record_evicts_the_value() {
    // A `[<Struct>]` record projects as `EntityKind::Record` with `is_struct =
    // true` — the IL value-type signal wins, so it evicts (unlike a plain
    // reference record).
    assert_head_evicted(&env_shadow_color(EntityKind::Record, true, 0));
}

#[test]
fn opened_assembly_interface_keeps_the_value() {
    // A2: an interface is not construction-capable — the value keeps the slot.
    assert_head_kept(&env_shadow_color(EntityKind::Interface, false, 0));
}

#[test]
fn opened_assembly_union_keeps_the_value() {
    // An F# union is `isClassTy`-false (the same predicate as project M20k) —
    // keeps. (A read-as-Class misprojection would over-evict, a safe
    // availability loss, never a wrong target.)
    assert_head_kept(&env_shadow_color(EntityKind::Union, false, 0));
}

#[test]
fn opened_assembly_record_keeps_the_value() {
    // A plain (reference) F# record — `isClassTy`-false, keeps (project M20l).
    assert_head_kept(&env_shadow_color(EntityKind::Record, false, 0));
}

#[test]
fn opened_assembly_module_keeps_the_value() {
    // Amodule: an F# module is not a constructible type — keeps.
    assert_head_kept(&env_shadow_color(EntityKind::Module, false, 0));
}

#[test]
fn an_assembly_open_before_the_value_keeps_it() {
    // A5: the open is BEFORE the value, so the class enters the slot earlier
    // and the later value re-takes it — member access on the local value.
    let env = env_shadow_color(EntityKind::Class, false, 0);
    let src = "module M\nopen Ns\nlet Color = {| Red = 3 |}\nlet u = Color.Red\n";
    let rf = resolve(src, &env);
    let head = span(src.find("Color.Red").expect("use"), 5);
    let res = rf
        .resolution_at(head)
        .expect("the head resolves to the value");
    let def = rf.resolved_def(res).expect("local value def");
    assert_eq!(
        def.range,
        span(src.find("let Color").expect("binder") + 4, 5)
    );
}

#[test]
fn a_module_alias_open_does_not_consult_assembly_types() {
    // Codex review of Stage 1: `open Alias` where `Alias = Zap` (a project
    // module) is a MODULE-only open — `open_interpretations` marks its
    // namespaces unreachable, so it lands in `module_open_prefixes` but NOT
    // `explicit_open_prefixes`. FCS keeps the local `Widget` value (probed
    // with the fixture referenced: head → `Client.Widget`), so the assembly
    // `Zap.Widget` class must NOT be consulted for eviction here — only a
    // genuine namespace open brings assembly types into the slot. Before the
    // fix, the scan over `module_open_prefixes` over-evicted and deferred the
    // resolvable local head.
    let env = fixture_env();
    let l0 = "module Zap\nlet placeholder = 1\n";
    let l1 = "module Client\nmodule Alias = Zap\nlet Widget = {| Foo = 3 |}\nopen Alias\nlet x = Widget.Foo\n";
    let proj = resolve_project(&[impl_file(l0), impl_file(l1)], &env);
    let rf = proj.file(1);
    let head = span(l1.find("Widget.Foo").expect("use"), 6);
    let res = rf
        .resolution_at(head)
        .expect("the head resolves to the local value, not the aliased assembly type");
    let def = rf.resolved_def(res).expect("local value def");
    assert_eq!(
        def.range,
        span(l1.find("let Widget").expect("binder") + 4, 6),
        "a module-alias open must not evict via assembly namespace types"
    );
}

#[test]
fn a_direct_module_open_merged_with_an_assembly_namespace_evicts() {
    // Codex round 2: with a project `module Demo` AND the referenced assembly's
    // namespace `Demo`, a DIRECT `open Demo` merges them — the assembly
    // `Demo.Calc` class occupies the slot and evicts the local `Calc` (probed
    // with the fixture referenced: head `Calc` → `Demo.Calc`, `Calc.Answer` →
    // `Demo.Calc.Answer`). `project_readings_only` filters that assembly-
    // namespace reading out of `explicit_open_prefixes`, so the eviction check
    // must consult it via the reading set, not the prefix-list category. Unlike
    // a module *alias* (which produces no reading), this must still evict — so
    // the head defers (the M20t/M20u assembly bar), never records the value.
    let env = fixture_env();
    let l0 = "module Demo\nlet placeholder = 1\n";
    let l1 = "module Client\nlet Calc = {| Answer = 3 |}\nopen Demo\nlet x = Calc.Answer\n";
    let proj = resolve_project(&[impl_file(l0), impl_file(l1)], &env);
    let rf = proj.file(1);
    let head = span(l1.find("Calc.Answer").expect("use"), 4);
    match rf.resolution_at(head) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("expected the merged-assembly-evicted head to defer, got {other:?}"),
    }
}

#[test]
fn colliding_assembly_types_all_count_for_eviction() {
    // Codex round 3: two referenced assemblies can expose the same
    // `(namespace, name, arity)`. `by_type` is first-wins, so if the
    // first-indexed `Ns.Color` is a non-constructible interface but a later
    // one is a public class, the eviction check must still see the class and
    // evict — scanning only `by_type` would under-evict and mis-record the
    // local value. Build the collision directly: an interface `Ns.Color` then
    // a class `Ns.Color`, both arity 0.
    let ents = fixture_entities();
    let base = ents
        .iter()
        .find(|e| {
            e.namespace == ["Demo"]
                && e.kind == EntityKind::Class
                && e.generic_parameters.is_empty()
        })
        .expect("an arity-0 Demo class")
        .clone();
    let mut iface = base.clone();
    iface.namespace = vec!["Ns".to_string()];
    iface.name = "Color".to_string();
    iface.kind = EntityKind::Interface;
    iface.is_struct = false;
    iface.members = vec![];
    iface.nested_types = vec![];
    let mut klass = iface.clone();
    klass.kind = EntityKind::Class;
    // Interface first (wins `by_type`), class second (the constructible one).
    let env = AssemblyEnv::from_entities(vec![iface, klass]);
    assert_head_evicted(&env);
}

// ===== Extension members never enter unqualified scope (autoopen plan ⚠) =====

#[test]
fn open_type_does_not_bring_csharp_extension_methods_into_bare_scope() {
    // `Demo.Exts` holds a C#-style extension method (`Doubled`) beside a plain
    // static (`Origin`). FCS admits extension members to no unqualified scope —
    // `ChooseMethInfosForNameEnv` filters `IsMethInfoPlainCSharpStyleExtensionMember`
    // — so after `open type Demo.Exts` a bare `Doubled` is FS0039 (fsi-verified),
    // while the plain `Origin` resolves. Before the fix we resolved both.
    let env = fixture_env();

    let src = "open type Demo.Exts\nlet x = Doubled 1\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Doubled")),
            Some(Resolution::Member { .. })
        ),
        "an `open type` must not make a C#-style extension method bare-resolvable, got {:?}",
        rf.resolution_at(at(src, "Doubled"))
    );

    let src = "open type Demo.Exts\nlet x = Origin()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Origin")),
            Some(Resolution::Member { .. })
        ),
        "the extension filter is member-keyed: a plain static of the same opened type still resolves"
    );
}

#[test]
fn csharp_extension_method_still_resolves_qualified() {
    // The other half of the C#-style rule: an extension method *is* an ordinary
    // static under a qualified path (fsi: `System.Linq.Enumerable.Select(xs, f)`
    // compiles), so only the bare-name channel drops it.
    let env = fixture_env();
    let src = "let x = Demo.Exts.Doubled 1\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "Demo.Exts.Doubled")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(member_name(env.member_at(parent, idx)), "Doubled");
        }
        other => panic!("a qualified C#-style extension method must resolve, got {other:?}"),
    }
}

/// Review round 3, the tier fall-through: an **undecidable** qualified member must
/// keep the path *owned*, not report itself absent.
///
/// `High.M.Mangled` is `Augmentation::Possible` (a pickle-less image's dotted
/// `[<CompiledName>]`, indistinguishable from an augmentation's mangling), while
/// `Low.M.Mangled` is an ordinary static. With `open Low` then `open High`, the
/// latest open — `High` — owns the path `M.Mangled`. If the uncertain member
/// reported itself *absent*, the walk would fall through to the lower-priority
/// `Low` reading and resolve to `Low.M.Mangled`: a **wrong target**, where the
/// honest answer is a deferral.
#[test]
fn an_undecidable_qualified_member_does_not_fall_through_to_a_lower_open() {
    let template = {
        let entities = fixture_entities();
        entities
            .iter()
            .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
            .cloned()
            .expect("Demo.Calc")
    };
    let static_named = |name: &str, augmentation| {
        let mut m = template
            .members
            .iter()
            .find_map(|m| match m {
                Member::Method(mm) if mm.name == "Zero" => Some(mm.clone()),
                _ => None,
            })
            .expect("Zero template");
        m.name = format!("String.{name}");
        m.source_name = Some(name.to_string());
        m.augmentation = augmentation;
        Member::Method(m)
    };
    let module = |namespace: &str, member: Member| {
        let mut e = template.clone();
        e.namespace = vec![namespace.to_string()];
        e.name = "M".to_string();
        e.kind = EntityKind::Module;
        e.members = vec![member];
        e.nested_types = vec![];
        e
    };

    let env = AssemblyEnv::from_entities(vec![
        module("High", static_named("Mangled", Augmentation::Possible)),
        module("Low", static_named("Mangled", Augmentation::No)),
    ]);

    let src = "open Low\nopen High\nlet x = M.Mangled 1\n";
    let rf = resolve(src, &env);
    let low = env
        .lookup_type(&["Low".to_string()], "M", 0)
        .expect("Low.M in env");

    let res = rf.resolution_at(at(src, "M.Mangled"));
    if let Some(Resolution::Member { parent, .. }) = res {
        assert_ne!(
            parent, low,
            "the undecidable High.M.Mangled must not fall through to Low.M.Mangled — \
             that is a wrong target, not a deferral"
        );
        panic!("an undecidable qualified member must not resolve to a target at all, got {res:?}");
    }
}

/// The mirror image of the test above, and the case the rebase onto the OV-7
/// ownership fallback (#914) re-opened: a **certainly hidden** augmentation must
/// *not* own the path — it must fall through to the lower `open`.
///
/// `High.M.X` is an `Augmentation::Certain` (an F#-native augmentation, which a
/// qualified path cannot reach at all — fsi: `CoreExts.ExtStatic "x"` is FS0039),
/// while `Low.M.X` is an ordinary static. [`AssemblyEnv::static_lookup`] rightly
/// calls the `High` reading `Absent`, but the OV-7 base-chain fallback then sees
/// the very same hidden member — it is a public method of the name on the module
/// class — and would re-claim the path as a deferral. That contradicts FCS, which
/// resolves `Low.M.X` (fsi-verified 2026-07-11: `open Low; open High; M.X 1` is
/// `101`, i.e. `Low.M.X`, when `High.M`'s only `X` is an augmentation).
///
/// So the two predicates must agree on what a hidden augmentation is: invisible.
/// `Uncertain` (a *possible* augmentation) still owns — that is the test above.
#[test]
fn a_hidden_augmentation_does_not_own_a_qualified_path() {
    let template = {
        let entities = fixture_entities();
        entities
            .iter()
            .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
            .cloned()
            .expect("Demo.Calc")
    };
    let static_named = |name: &str, augmentation| {
        let mut m = template
            .members
            .iter()
            .find_map(|m| match m {
                Member::Method(mm) if mm.name == "Zero" => Some(mm.clone()),
                _ => None,
            })
            .expect("Zero template");
        m.name = format!("String.{name}");
        m.source_name = Some(name.to_string());
        m.augmentation = augmentation;
        Member::Method(m)
    };
    let module = |namespace: &str, member: Member| {
        let mut e = template.clone();
        e.namespace = vec![namespace.to_string()];
        e.name = "M".to_string();
        e.kind = EntityKind::Module;
        e.members = vec![member];
        e.nested_types = vec![];
        e
    };

    let env = AssemblyEnv::from_entities(vec![
        module("High", static_named("X", Augmentation::Certain)),
        module("Low", static_named("X", Augmentation::No)),
    ]);

    let src = "open Low\nopen High\nlet x = M.X 1\n";
    let rf = resolve(src, &env);
    let low = env
        .lookup_type(&["Low".to_string()], "M", 0)
        .expect("Low.M in env");

    match rf.resolution_at(at(src, "M.X")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(
                parent, low,
                "the hidden augmentation in High.M must not own the path: FCS resolves \
                 Low.M.X here"
            );
            // `member_name` here is the test's *IL*-name helper, and `static_named`
            // gives every member the mangled IL name `String.X` over the source name
            // `X` — so this pins the member we landed on, not its source spelling.
            assert_eq!(member_name(env.member_at(parent, idx)), "String.X");
        }
        other => panic!(
            "a hidden augmentation must not swallow the path — Low.M.X must resolve, got {other:?}"
        ),
    }
}

/// Review round 9, P1: a **merged** open — one module FQN exposed by several assemblies —
/// can name definite targets only when every half's bare-name surface is provably
/// complete. FCS adds each half's contents in *reference order*, so an unmodelled union
/// case `Hit` in the later-referenced half binds over a visible `let Hit` in the earlier
/// one. We cannot see the case, so the visible member is not a safe target: defer.
///
/// (Within a *single* module the same collision is decidable and needs no deferral: FCS
/// adds a module's tycons before its vals, so a `let Hit` beside a nested union's case
/// `Hit` binds the **let** — fsi-verified. That is why the gate keys on the merge.)
#[test]
fn a_merged_open_with_an_incomplete_half_defers_its_visible_names() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    let mut visible = template.clone();
    visible.namespace = vec!["Dup".to_string()];
    visible.name = "M".to_string();
    visible.kind = EntityKind::Module;
    visible.nested_types = vec![];

    // The same FQN from another assembly, whose surface is NOT provably complete (an
    // undecodable member — which could be anything, including a name `visible` exports).
    let mut incomplete = visible.clone();
    incomplete.members = vec![];
    incomplete.skipped_members = vec![borzoi_assembly::SkippedMember {
        name: "undecodable".to_string(),
        reason: "projection could not read it".to_string(),
    }];

    let env = AssemblyEnv::from_entities(vec![visible, incomplete]);
    let src = "open Dup.M\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "a merged open with an unprovable half must not name a definite target, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// Review round 9, P2: an `open` that names nothing under an **incomplete** prefix must
/// not fall through to a lower tier.
///
/// `open Parent` where projection dropped a type in `Parent`'s namespace may have brought
/// in a nested module `Parent.Sub` we cannot see — and FCS binds it at a *higher* priority
/// than any root `Sub`. So a following `open Sub` must go opaque rather than resolve the
/// root module's values, which would be a wrong target.
#[test]
fn an_open_under_an_incomplete_prefix_does_not_fall_through_to_a_root_module() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    let mut parent = template.clone();
    parent.namespace = vec![];
    parent.name = "Parent".to_string();
    parent.kind = EntityKind::Module;
    parent.members = vec![];
    parent.nested_types = vec![];

    // A ROOT module `Sub`, whose `Zero` a fall-through would wrongly resolve.
    let mut root_sub = template.clone();
    root_sub.namespace = vec![];
    root_sub.name = "Sub".to_string();
    root_sub.kind = EntityKind::Module;
    root_sub.nested_types = vec![];

    let mut env = AssemblyEnv::from_entities(vec![parent, root_sub]);
    // Projection dropped a type in `Parent`'s (root) namespace — so `Parent.Sub` may exist
    // in FCS's world and be invisible in ours.
    env.mark_namespace_dropped_type(vec![]);

    let src = "open Parent\nopen Sub\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "`open Sub` under an incomplete `Parent` must not bind the root `Sub`'s member — \
         FCS may bind a dropped `Parent.Sub`; defer instead. Got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// Review round 10, P1: a **dropped type** in the namespace means a lone *visible* module
/// handle does not prove FCS sees only one module — the dropped TypeDef may itself be
/// another assembly's same-FQN module, which FCS merges and orders by reference. So the
/// visible module's own members are not safe targets either.
///
/// (Contrast `a_lone_module_with_hidden_tycon_names_still_names_its_values` below: a
/// hidden union case is *tycon-tier* and loses to our vals, so that module keeps definite
/// targets. The distinction is what keeps the conservatism from costing availability.)
#[test]
fn a_lone_module_under_a_dropped_type_names_no_definite_target() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");
    let mut m = template.clone();
    m.namespace = vec!["Drop".to_string()];
    m.name = "M".to_string();
    m.kind = EntityKind::Module;
    m.nested_types = vec![];

    let mut env = AssemblyEnv::from_entities(vec![m]);
    env.mark_namespace_dropped_type(vec!["Drop".to_string()]);

    let src = "open Drop.M\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "a dropped type in the namespace may BE another assembly's same-FQN module: the \
         visible module's members are not safe targets, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// The availability half of the same rule: a module whose only hidden names are
/// **tycon-tier** (a nested union's cases) still names its own values. FCS adds a module's
/// tycons *before* its vals, so a `let Hit` beside a case `Hit` binds the `let`
/// (fsi-verified) — the very member we enumerate. Blanket-deferring here would have cost
/// most real library modules their values.
#[test]
fn a_lone_module_with_hidden_tycon_names_still_names_its_values() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");
    let mut m = template.clone();
    m.namespace = vec!["Tycon".to_string()];
    m.name = "M".to_string();
    m.kind = EntityKind::Module;

    // A nested (non-RQA) union: its cases are bare names we cannot enumerate.
    let mut union = template.clone();
    union.namespace = vec![];
    union.name = "Colour".to_string();
    union.kind = EntityKind::Union;
    union.members = vec![];
    union.nested_types = vec![];
    m.nested_types = vec![union];

    let env = AssemblyEnv::from_entities(vec![m]);
    let src = "open Tycon.M\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "hidden tycon-tier names lose to the module's own vals: `Zero` must still \
         resolve, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// Review round 10, P2: a `global.`-rooted open cannot be shortened through any prefix, so
/// an incomplete prefix cannot hide a higher-priority reading of it — the veto must not
/// fire, or opening one incomplete module would make every later rooted open opaque.
#[test]
fn a_rooted_open_is_not_vetoed_by_an_incomplete_prefix() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");
    // `Parent` lives in a namespace with a dropped type, so it may hide a nested module.
    let mut parent = template.clone();
    parent.namespace = vec!["Inc".to_string()];
    parent.name = "Parent".to_string();
    parent.kind = EntityKind::Module;
    parent.members = vec![];
    parent.nested_types = vec![];

    // `Sub` is a ROOT module in a clean namespace — nothing about it is unknowable.
    let mut root_sub = template.clone();
    root_sub.namespace = vec![];
    root_sub.name = "Sub".to_string();
    root_sub.kind = EntityKind::Module;
    root_sub.nested_types = vec![];

    let mut env = AssemblyEnv::from_entities(vec![parent, root_sub]);
    env.mark_namespace_dropped_type(vec!["Inc".to_string()]);

    let src = "open Inc.Parent\nopen global.Sub\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "`open global.Sub` is rooted: no prefix can shorten it, so the incomplete \
         `Parent` must not veto it. Got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// Review round 11: `incomplete_open_prefixes` is open state, and open state is
/// **block-scoped**. An incomplete module opened in one top-level block was still vetoing
/// opens in a *sibling* block, where the original open is long out of scope — suppressing
/// members the sibling legitimately imports.
#[test]
fn an_incomplete_prefix_does_not_leak_into_a_sibling_top_level_block() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    let mut parent = template.clone();
    parent.namespace = vec!["Inc".to_string()];
    parent.name = "Parent".to_string();
    parent.kind = EntityKind::Module;
    parent.members = vec![];
    parent.nested_types = vec![];

    let mut sub = template.clone();
    sub.namespace = vec![];
    sub.name = "Sub".to_string();
    sub.kind = EntityKind::Module;
    sub.nested_types = vec![];

    let mut env = AssemblyEnv::from_entities(vec![parent, sub]);
    env.mark_namespace_dropped_type(vec!["Inc".to_string()]);

    // Two top-level BLOCKS: the first opens the incomplete module, the second is
    // unrelated and opens `Sub`.
    let src = "namespace First\n\nopen Inc.Parent\n\nnamespace Second\n\nopen Sub\n\nmodule M =\n    let x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "the incomplete prefix belongs to the FIRST block: it must not veto the second \
         block's `open Sub`. Got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// Review round 13, deliberately left standing (§5a of
/// `docs/assembly-module-open-plan.md`). `incomplete_open_prefixes` is consulted as a
/// **non-empty-vector** test, so *any* incomplete prefix in scope vetoes a later `open
/// Sub` — even when a newer, provably-complete prefix would outrank it.
///
/// Here `open Inc.Parent` is incomplete (a dropped type in `Inc` could hide a nested
/// module), but the later `open Clean` establishes a prefix that definitely supplies
/// `Clean.Sub`. Being later, it outranks `Inc.Parent`, so FCS binds the visible
/// `Clean.Sub.Zero` and no hidden `Inc.Parent.Sub` can take it. We veto anyway and defer.
///
/// The veto should apply only when an incomplete prefix outranks *every* resolved
/// interpretation — which means modelling rank between shortening prefixes, the same
/// "model the contest exactly" reasoning that produced rounds 5–12. If it is worth doing
/// it is worth its own slice and its own oracle work, so it is not being bolted onto the
/// round-13 diff. Remove the `#[ignore]` and watch this fail as step one.
#[test]
#[ignore = "§5a of docs/assembly-module-open-plan.md: the incomplete-prefix veto ignores precedence"]
fn a_newer_definite_prefix_outranks_an_incomplete_one() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // The INCOMPLETE prefix: `Inc.Parent`, in a namespace carrying a dropped type.
    let mut parent = template.clone();
    parent.namespace = vec!["Inc".to_string()];
    parent.name = "Parent".to_string();
    parent.kind = EntityKind::Module;
    parent.members = vec![];
    parent.nested_types = vec![];

    // The DEFINITE, later prefix: `Clean`, holding a nested `Sub` with the member.
    let mut sub = template.clone();
    sub.namespace = vec!["Clean".to_string()];
    sub.name = "Sub".to_string();
    sub.kind = EntityKind::Module;
    sub.nested_types = vec![];

    let mut env = AssemblyEnv::from_entities(vec![parent, sub]);
    env.mark_namespace_dropped_type(vec!["Inc".to_string()]);

    let src = "module M\nopen Inc.Parent\nopen Clean\nopen Sub\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "`open Clean` is later than the incomplete `open Inc.Parent`, so `Sub` names the \
         visible `Clean.Sub` and nothing hidden under `Inc.Parent` can outrank it — the \
         veto must not fire. Got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// Review round 16, P1: the lookup and the safety check must span the **same space**.
///
/// `opened_assembly_modules` merges *every* split of a module FQN (round 7): one assembly
/// may expose `A.B.C` as a top-level `C` in namespace `A.B`, another as root module `A` →
/// nested `B` → nested `C`. But the uncertainty check asked `namespace_has_dropped_type`
/// about only the **visible** encoding's own namespace. Here the visible encoding is the
/// nested one, whose owning namespace is the root `[]` — while a *different* assembly
/// dropped a top-level type in `A.B`. That dropped type could itself be another same-FQN
/// module (round 10's insight), which FCS merges and orders by reference.
///
/// So the module was certified `Complete` and `open A.B.C` named a definite `Member`,
/// decided against a half we cannot see. The check now walks every split of the path.
#[test]
fn a_dropped_type_at_another_split_of_the_module_path_defers() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // The NESTED encoding: root module `A` → `B` → `C`, so `C`'s owning namespace is `[]`.
    let mut c = template.clone();
    c.namespace = vec![];
    c.name = "C".to_string();
    c.kind = EntityKind::Module;
    c.nested_types = vec![];

    let mut b = c.clone();
    b.name = "B".to_string();
    b.members = vec![];
    b.nested_types = vec![c];

    let mut a = b.clone();
    a.name = "A".to_string();
    a.members = vec![];
    a.nested_types = vec![b];

    let mut env = AssemblyEnv::from_entities(vec![a]);
    // Another assembly dropped a top-level type in `A.B` — the OTHER split of the same
    // FQN. It could be a same-named module whose members FCS merges here.
    env.mark_namespace_dropped_type(vec!["A".to_string(), "B".to_string()]);

    let src = "open A.B.C\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "a dropped type in `A.B` is a possible same-FQN module half at another split of \
         `A.B.C`; the open must not name a definite target, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// Review round 17, P1: the mirror image of round 15's hole. On a cross-kind path we defer
/// every name the **module** half *enumerates* — but a module half we cannot fully project
/// (skipped members, an undecodable pickle) has names we never enumerate at all, so no
/// deferred entry is pushed for them. The **namespace** half's values, meanwhile, are still
/// pushed as definite `Member`s. A hidden module name colliding with one of those left the
/// namespace's value as the answer, while FCS — folding the halves in reference order —
/// may bind the module's hidden item. A wrong target.
///
/// The invariant is now the single sentence that covers rounds 15 and 17 together: **a
/// merge names a definite target only when EVERY half is fully enumerable.** So an
/// incomplete module half makes the namespace half defer too.
#[test]
fn an_incomplete_module_half_makes_the_namespace_half_defer() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // The MODULE half at `P.M`. Its VISIBLE members are empty and one member is
    // undecodable — so the only name it could supply is one we cannot see. That is the
    // whole point: were `Zero` among its visible members, its own (deferred) entry would
    // shadow the namespace half's and mask the bug.
    let mut module_half = template.clone();
    module_half.namespace = vec!["P".to_string()];
    module_half.name = "M".to_string();
    module_half.kind = EntityKind::Module;
    module_half.nested_types = vec![];
    module_half.members = vec![];
    module_half.skipped_members = vec![SkippedMember {
        name: "Undecodable".to_string(),
        reason: "test: a member we could not project — it could be a `Zero`".to_string(),
    }];

    // The NAMESPACE half at the same FQN: an `[<AutoOpen>]` module in namespace `P.M`,
    // supplying a value. Today this resolves definitely — even though the module half's
    // hidden member could be a same-named item FCS would bind instead.
    let mut ns_half = template.clone();
    ns_half.namespace = vec!["P".to_string(), "M".to_string()];
    ns_half.name = "NsAuto".to_string();
    ns_half.kind = EntityKind::Module;
    ns_half.is_auto_open = true;
    ns_half.nested_types = vec![];
    ns_half.skipped_members = vec![];

    let env = AssemblyEnv::from_entities(vec![module_half, ns_half]);

    let src = "open P.M\nlet x = Zero()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "the module half is incompletely projected, so its hidden members may contest \
         this namespace-half value; FCS orders the halves by reference — defer, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// Review round 18, P1 (finding 1): one `open M` produces several interpretations at
/// different **priority tiers** — a relative `N.M` and a root `M`. FCS resolves the open
/// to a *single* module and folds only that one; we cannot tell which it picks, so we
/// apply all tiers as an over-approximation. A single pre-loop barrier tagged every tier
/// with the same generation, so a hidden name in the higher-priority tier (which we do
/// not enumerate, hence push nothing for) left the lower tier's *enumerated* value as a
/// definite target. FCS binds the higher tier's hidden case.
///
/// The deep cut (round 18): grant a definite target only when the open has **exactly one**
/// interpretation. Multiple tiers ⇒ ambiguous ⇒ every name defers. No per-tier barrier
/// gymnastics — the ambiguity itself is the disqualifier.
#[test]
fn a_multi_tier_assembly_module_open_defers() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // The ROOT tier: module `M` in the global namespace, with an enumerated `Zero`.
    let mut root = template.clone();
    root.namespace = vec![];
    root.name = "M".to_string();
    root.kind = EntityKind::Module;
    root.nested_types = vec![];

    // The RELATIVE tier: `N.M`, whose only `Zero` is a HIDDEN union case (not enumerated).
    // FCS, resolving `open M` inside `namespace N`, reaches this one first.
    let mut zero_case = template.clone();
    zero_case.namespace = vec![];
    zero_case.name = "U".to_string();
    zero_case.kind = EntityKind::Union;
    zero_case.members = vec![];
    zero_case.nested_types = vec![];
    let mut relative = template.clone();
    relative.namespace = vec!["N".to_string()];
    relative.name = "M".to_string();
    relative.kind = EntityKind::Module;
    relative.members = vec![]; // enumerates no `Zero` of its own
    relative.nested_types = vec![zero_case];

    let env = AssemblyEnv::from_entities(vec![root, relative]);
    let src = "namespace N\n\nmodule Test =\n    open M\n    let x = Zero\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "`open M` reaches both the relative `N.M` (hidden case `Zero`) and the root `M` \
         (enumerated `Zero`); which FCS binds is a contest we do not model — defer, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// Review round 18, P1 (finding 2): the cross-kind merge check only recognised *assembly*
/// namespaces (`has_namespace`), not **project** ones. When an earlier project file
/// declares a namespace at the same FQN as a referenced module, `open X` opens both — the
/// project namespace's cases and the assembly module's statics. FCS binds the project
/// case; sema returned the assembly `Member`, because the module half was granted a
/// definite target (the project namespace never entered the `is_merge` test).
///
/// §7's "machinery" slice (`docs/assembly-module-open-plan.md`) later folded the project
/// namespace half properly (`open_project_namespace_values`, applied strictly after the
/// assembly module fold): the project case now wins the collision by **position** (Q14 —
/// the project's own fragment always folds last), a definite `Resolution::Item`, not a
/// deferral. (Before that slice, the blanket `is_project_namespace_path` cross-kind demote
/// made every colliding name defer instead — sound, but unavailable; this test now pins the
/// stronger, available result.)
///
/// The fixture is deliberately a **bare, bar-less** single case (`type T = Zero`, not
/// `type T = | Zero`) — codex review of this slice caught that a bar-less RHS is FCS's
/// (and this parser's, `peek_is_union_or_enum_repr_start`) type-**abbreviation** shape, not
/// a union case, so it exported nothing and this test's assembly-vs-project collision was
/// never actually exercised; a leading `|` is what forces the union-case parse this test
/// needs.
#[test]
fn an_assembly_module_colocated_with_a_project_namespace_wins_the_collision() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // The ASSEMBLY MODULE `Foo`, with an enumerated `Zero`.
    let mut module = template.clone();
    module.namespace = vec![];
    module.name = "Foo".to_string();
    module.kind = EntityKind::Module;
    module.nested_types = vec![];
    let env = AssemblyEnv::from_entities(vec![module]);

    // An earlier PROJECT file declares `namespace Foo` with a union case `Zero` (the
    // leading `|` forces the union parse over the bare-ident abbreviation shape). FCS
    // folds both halves; the project case is what it binds.
    let file0 = impl_file("namespace Foo\n\ntype T =\n    | Zero\n");
    let file1 = impl_file("open Foo\nlet x = Zero\n");
    let proj = resolve_project(&[file0, file1], &env);

    let src1 = "open Foo\nlet x = Zero\n";
    let i = src1.rfind("Zero").expect("use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "Zero".len()).unwrap().into(),
    );
    assert!(
        matches!(
            proj.file(1).resolution_at(use_range),
            Some(Resolution::Item(_))
        ),
        "a project namespace `Foo` co-locates with the assembly module `Foo`; FCS binds \
         the project case, and it must resolve definitely (a project `Item`), not the \
         assembly member and not a deferral — got {:?}",
        proj.file(1).resolution_at(use_range)
    );
}

/// Codex review round 5 of §7's machinery slice: `[<AutoOpen>]` is real F# on a plain
/// TYPE, not just a module — fcs-dump-verified: `namespace X` / `[<AutoOpen>] type T =
/// static member Clash = 5` makes `open X; Clash` bind `X.T.Clash`, exactly like an
/// explicit `open type`. Sema has no project-side `open_type_statics` equivalent (it
/// does not model project type members at all), so `Clash` is invisible to every
/// enumeration this fold does — `open_module_values`, `direct_project_type_contestants`
/// (which only tracks the type's OWN name, `T`, as a value-slot contestant, not `T`'s
/// members), `direct_value_names_at`. With a colocated assembly module ALSO exporting a
/// value `Clash`, the assembly's value stayed wrongly definite: nothing marked the
/// project namespace as having unenumerable content. Fixed by marking the type's own
/// container hidden (`Resolver::note_hidden_value_module`, the same signal an active
/// pattern already gives) whenever a type carries `[<AutoOpen>]`, and feeding that into
/// `full_residue` in `decls.rs` so a colliding assembly value defers instead of
/// committing.
#[test]
fn an_auto_open_project_type_contests_a_colocated_assembly_modules_value() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // The ASSEMBLY MODULE `Bar`, with an enumerated `Clash`.
    let mut module = template.clone();
    module.namespace = vec![];
    module.name = "Bar".to_string();
    module.kind = EntityKind::Module;
    module.nested_types = vec![];
    let env = AssemblyEnv::from_entities(vec![module]);

    // An earlier PROJECT file declares `namespace Bar` with an `[<AutoOpen>]` type
    // whose static member shares the assembly module's enumerated name (`Zero`, per
    // the shared fixture template above).
    let file0 = impl_file("namespace Bar\n\n[<AutoOpen>]\ntype T =\n    static member Zero = 1\n");
    let file1 = impl_file("open Bar\nlet x = Zero\n");
    let proj = resolve_project(&[file0, file1], &env);

    let src1 = "open Bar\nlet x = Zero\n";
    let i = src1.rfind("Zero").expect("use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "Zero".len()).unwrap().into(),
    );
    assert!(
        !matches!(
            proj.file(1).resolution_at(use_range),
            Some(Resolution::Member { .. })
        ),
        "an `[<AutoOpen>]` project type's static member is unenumerable, so it must mark \
         its namespace as hidden — a colliding assembly value must defer, never wrongly \
         commit — got {:?}",
        proj.file(1).resolution_at(use_range)
    );
}

/// Codex review of §7's machinery slice (the follow-up to the test above): a project
/// namespace's own **constructible type** (a class/struct/enum — [`SlotClass::Evicts`] /
/// `Unknown`, never a plain union/record) takes FCS's unqualified constructor slot
/// exactly like an assembly namespace's constructible types already do
/// (`AssemblyEnv::open_namespace_fold_surfaces`'s `contestant_names`), so it can EVICT a
/// same-named *value* from a colocated assembly module. `open_project_namespace_values`
/// itself pushes no entry for a project type (sema does not model project type
/// constructors), so without `project_namespace_contestant_names` feeding this name into
/// the fold's `collisions()` check, the assembly's value stayed wrongly definite once the
/// `cross_kind` blanket demote was deleted. This test pins the sound (if unavailable)
/// outcome: the bare use must defer, never commit the assembly member.
#[test]
fn a_project_namespace_type_contests_a_colocated_assembly_modules_value() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // The ASSEMBLY MODULE `Bar`, with an enumerated `Zero`.
    let mut module = template.clone();
    module.namespace = vec![];
    module.name = "Bar".to_string();
    module.kind = EntityKind::Module;
    module.nested_types = vec![];
    let env = AssemblyEnv::from_entities(vec![module]);

    // An earlier PROJECT file declares `namespace Bar` with a CONSTRUCTIBLE type also
    // named `Zero` (an implicit primary constructor — `SlotClass::Evicts`). FCS's
    // unqualified constructor slot then has two contestants for `Zero ()`; sema models
    // neither project type constructors nor the fold-order tie-break, so it must defer
    // rather than commit the assembly's `Zero` as if the project type did not exist.
    let file0 = impl_file("namespace Bar\n\ntype Zero() =\n    member _.X = 1\n");
    let file1 = impl_file("open Bar\nlet x = Zero\n");
    let proj = resolve_project(&[file0, file1], &env);

    let src1 = "open Bar\nlet x = Zero\n";
    let i = src1.rfind("Zero").expect("use");
    let use_range = TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + "Zero".len()).unwrap().into(),
    );
    assert!(
        !matches!(
            proj.file(1).resolution_at(use_range),
            Some(Resolution::Member { .. })
        ),
        "a project namespace's own constructible type contests the assembly module's \
         same-named value for FCS's unqualified constructor slot; the bare use must defer, \
         never wrongly commit the assembly's value — got {:?}",
        proj.file(1).resolution_at(use_range)
    );
}

/// Review round 19 — the deep cut did NOT fully close the seam. It gated the assembly
/// module arm's certainty, but a *project* module interpretation at a LOWER priority
/// tier still resolves definitely — and a single pre-loop barrier gives it the same
/// generation as the HIGHER-priority assembly module that hides a colliding name, so
/// nothing shadows it. The fold's per-interpretation barrier (bump when a
/// residue-bearing interpretation is applied, staling everything folded before it —
/// exactly FCS's fold order) closes it by construction.
#[test]
fn a_higher_hidden_assembly_module_must_shadow_a_lower_project_module() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // Relative `N.M`: an assembly module whose only `Zero` is a HIDDEN union case
    // (`union_case_names` empty on a Union = unknowable — name-unknown residue).
    let mut zero_case = template.clone();
    zero_case.namespace = vec![];
    zero_case.name = "U".to_string();
    zero_case.kind = EntityKind::Union;
    zero_case.members = vec![];
    zero_case.nested_types = vec![];
    let mut relative = template.clone();
    relative.namespace = vec!["N".to_string()];
    relative.name = "M".to_string();
    relative.kind = EntityKind::Module;
    relative.members = vec![];
    relative.nested_types = vec![zero_case];
    let env = AssemblyEnv::from_entities(vec![relative]);

    // Root PROJECT `module M` exporting `Zero`. `open M` inside `namespace N` reaches the
    // relative assembly `N.M` (higher priority, hidden case `Zero`) and the root project
    // `M` (lower, enumerated `Zero`). FCS binds the relative case.
    let file0 = impl_file("module M\n\nlet Zero () = 1\n");
    let file1 = impl_file("namespace N\n\nmodule Test =\n    open M\n    let x = Zero\n");
    let proj = resolve_project(&[file0, file1], &env);

    let src1 = "namespace N\n\nmodule Test =\n    open M\n    let x = Zero\n";
    let i = src1.rfind("Zero").expect("use");
    let use_range = span(i, "Zero".len());
    assert!(
        !matches!(
            proj.file(1).resolution_at(use_range),
            Some(Resolution::Item(_))
        ),
        "the higher-priority assembly `N.M` hides a case `Zero`; the lower project \
         `M.Zero` must not be a definite target — got {:?}",
        proj.file(1).resolution_at(use_range)
    );
}

/// The fold's availability payoff: a module whose nested union's case names ARE
/// enumerable (`union_case_names` from the pickle) carries no name-unknown residue, so
/// opening it must NOT blank out an earlier open's unrelated value — the fold pushes
/// the case names as (opaque) entries instead of raising the blanket barrier.
#[test]
fn an_enumerable_union_does_not_stale_an_earlier_opens_values() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // `Vals.A`: a plain module with vals (the Calc members: `Zero`, `Answer`, …).
    let mut vals = template.clone();
    vals.namespace = vec!["Vals".to_string()];
    vals.name = "A".to_string();
    vals.kind = EntityKind::Module;
    vals.nested_types = vec![];

    // `Tycon.M`: a module whose only content is a union with KNOWN case names.
    let mut union = template.clone();
    union.namespace = vec![];
    union.name = "Colour".to_string();
    union.kind = EntityKind::Union;
    union.members = vec![];
    union.nested_types = vec![];
    union.union_case_names = Some(vec!["Red".to_string(), "Green".to_string()]);
    let mut m = template.clone();
    m.namespace = vec!["Tycon".to_string()];
    m.name = "M".to_string();
    m.kind = EntityKind::Module;
    m.members = vec![];
    m.nested_types = vec![union];

    let env = AssemblyEnv::from_entities(vec![vals, m]);
    let src = "open Vals.A\nopen Tycon.M\nlet x = Zero ()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "`Tycon.M`'s union cases are enumerable (`Red`/`Green`), so the earlier open's \
         `Zero` must survive and resolve — got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// The mirror image: an enumerable union case name COLLIDING with an earlier open's
/// value must shadow it (FCS folds the later open's cases over the earlier open's
/// vals). The case itself may be opaque (`Deferred`) — but the stale value must never
/// be the answer.
#[test]
fn an_enumerable_union_case_shadows_an_earlier_opens_value() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    let mut union = template.clone();
    union.namespace = vec![];
    union.name = "Colour".to_string();
    union.kind = EntityKind::Union;
    union.members = vec![];
    union.nested_types = vec![];
    union.union_case_names = Some(vec!["Red".to_string()]);
    let mut m = template.clone();
    m.namespace = vec!["Tycon".to_string()];
    m.name = "M".to_string();
    m.kind = EntityKind::Module;
    m.members = vec![];
    m.nested_types = vec![union];
    let env = AssemblyEnv::from_entities(vec![m]);

    // The earlier PROJECT module supplies a value `Red`; the later assembly open's
    // union case `Red` outranks it.
    let file0 = impl_file("module P\n\nlet Red = 1\n");
    let file1 = impl_file("module Test\nopen P\nopen Tycon.M\nlet x = Red\n");
    let proj = resolve_project(&[file0, file1], &env);

    let src1 = "module Test\nopen P\nopen Tycon.M\nlet x = Red\n";
    let i = src1.rfind("Red").expect("use");
    let use_range = span(i, "Red".len());
    assert!(
        !matches!(
            proj.file(1).resolution_at(use_range),
            Some(Resolution::Item(_))
        ),
        "the later open's union case `Red` shadows the earlier open's value `Red` \
         (FCS binds the case) — got {:?}",
        proj.file(1).resolution_at(use_range)
    );
}

/// An `[<RequireQualifiedAccess>]` union's cases are NOT folded (FCS:
/// `isILOrRequiredQualifiedAccess` suppresses them from unqualified/pattern scope), so
/// they shadow nothing: the earlier open's value keeps resolving.
#[test]
fn an_rqa_unions_cases_do_not_shadow_an_earlier_opens_value() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    let mut union = template.clone();
    union.namespace = vec![];
    union.name = "Colour".to_string();
    union.kind = EntityKind::Union;
    union.members = vec![];
    union.nested_types = vec![];
    union.is_require_qualified_access = true;
    union.union_case_names = Some(vec!["Red".to_string()]);
    let mut m = template.clone();
    m.namespace = vec!["Tycon".to_string()];
    m.name = "M".to_string();
    m.kind = EntityKind::Module;
    m.members = vec![];
    m.nested_types = vec![union];
    let env = AssemblyEnv::from_entities(vec![m]);

    let file0 = impl_file("module P\n\nlet Red = 1\n");
    let file1 = impl_file("module Test\nopen P\nopen Tycon.M\nlet x = Red\n");
    let proj = resolve_project(&[file0, file1], &env);

    let src1 = "module Test\nopen P\nopen Tycon.M\nlet x = Red\n";
    let i = src1.rfind("Red").expect("use");
    let use_range = span(i, "Red".len());
    assert!(
        matches!(
            proj.file(1).resolution_at(use_range),
            Some(Resolution::Item(_))
        ),
        "an RQA union's cases are not imported by `open`, so the earlier open's value \
         `Red` still resolves — got {:?}",
        proj.file(1).resolution_at(use_range)
    );
}

/// An F# exception nested in an opened module is a bare constructor name (FCS folds
/// exceptions first, into value AND pattern scope): it must shadow an earlier open's
/// same-named value.
#[test]
fn an_exception_constructor_shadows_an_earlier_opens_value() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    let mut exn = template.clone();
    exn.namespace = vec![];
    exn.name = "Boom".to_string();
    exn.kind = EntityKind::Exception;
    exn.members = vec![];
    exn.nested_types = vec![];
    let mut m = template.clone();
    m.namespace = vec!["Tycon".to_string()];
    m.name = "M".to_string();
    m.kind = EntityKind::Module;
    m.members = vec![];
    m.nested_types = vec![exn];
    let env = AssemblyEnv::from_entities(vec![m]);

    let file0 = impl_file("module P\n\nlet Boom = 1\n");
    let file1 = impl_file("module Test\nopen P\nopen Tycon.M\nlet x = Boom\n");
    let proj = resolve_project(&[file0, file1], &env);

    let src1 = "module Test\nopen P\nopen Tycon.M\nlet x = Boom\n";
    let i = src1.rfind("Boom").expect("use");
    let use_range = span(i, "Boom".len());
    assert!(
        !matches!(
            proj.file(1).resolution_at(use_range),
            Some(Resolution::Item(_))
        ),
        "the opened module's exception constructor `Boom` shadows the earlier open's \
         value (FCS binds the exception) — got {:?}",
        proj.file(1).resolution_at(use_range)
    );
}

/// Codex round 20, P2: FCS's `CanAutoOpenTyconRef` opens an `[<AutoOpen>]` type only
/// when its type-parameter list is EMPTY — a generic auto-open type contributes
/// nothing, so it is not residue and must not stale an earlier open's values.
#[test]
fn a_generic_auto_open_type_is_not_residue() {
    let ents = fixture_entities();
    let template = ents
        .iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");
    let generic = ents
        .iter()
        .find(|e| {
            e.namespace == vec!["Demo".to_string()]
                && e.kind == EntityKind::Class
                && e.generic_parameters.len() == 1
        })
        .expect("an arity-1 Demo class in the fixture");

    let mut vals = template.clone();
    vals.namespace = vec!["Vals".to_string()];
    vals.name = "A".to_string();
    vals.kind = EntityKind::Module;
    vals.nested_types = vec![];

    let mut auto = generic.clone();
    auto.namespace = vec![];
    auto.name = "Helpers".to_string();
    auto.is_auto_open = true;
    auto.nested_types = vec![];
    let mut m = template.clone();
    m.namespace = vec!["Tycon".to_string()];
    m.name = "M".to_string();
    m.kind = EntityKind::Module;
    m.members = vec![];
    m.nested_types = vec![auto];

    let env = AssemblyEnv::from_entities(vec![vals, m]);
    let src = "open Vals.A\nopen Tycon.M\nlet x = Zero ()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "a GENERIC auto-open type is never opened by FCS, so it hides nothing: the \
         earlier open's `Zero` must survive — got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// Codex round 20, P2: an opened module's exception constructor is a DEFINITE
/// expression target (FCS resolves `Boom` to the constructor), so the tycon tier's
/// opaque type-name entry must not mask the exception's own `Entity` entry.
#[test]
fn an_opened_exception_constructor_is_a_definite_target() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    let mut exn = template.clone();
    exn.namespace = vec![];
    exn.name = "Boom".to_string();
    exn.kind = EntityKind::Exception;
    exn.members = vec![];
    exn.nested_types = vec![];
    let mut m = template.clone();
    m.namespace = vec!["Tycon".to_string()];
    m.name = "M".to_string();
    m.kind = EntityKind::Module;
    m.members = vec![];
    m.nested_types = vec![exn];
    let env = AssemblyEnv::from_entities(vec![m]);

    let m_handle = env
        .lookup_type(&["Tycon".to_string()], "M", 0)
        .expect("Tycon.M");
    let boom = env.nested(m_handle, "Boom", 0).expect("Tycon.M.Boom");

    let src = "open Tycon.M\nlet x = Boom\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Boom")),
        Some(Resolution::Entity(boom)),
        "the folded exception constructor names its entity"
    );
}

/// Codex round 21: a union with a PRIVATE representation (`type U = private | Hidden`)
/// knowably contributes no case to a cross-assembly `open` — FCS resolves an earlier
/// same-named binding, so the knowably-empty case list (`Some([])`) must be neither
/// residue nor a shadow.
#[test]
fn a_private_representation_union_shadows_nothing() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    let mut union = template.clone();
    union.namespace = vec![];
    union.name = "Concealed".to_string();
    union.kind = EntityKind::Union;
    union.members = vec![];
    union.nested_types = vec![];
    union.union_case_names = Some(vec![]);
    let mut m = template.clone();
    m.namespace = vec!["Tycon".to_string()];
    m.name = "M".to_string();
    m.kind = EntityKind::Module;
    m.members = vec![];
    m.nested_types = vec![union];
    let env = AssemblyEnv::from_entities(vec![m]);

    // The earlier open's `Hidden` value keeps resolving: the private case is
    // inaccessible cross-assembly, so FCS binds the earlier name.
    let file0 = impl_file("module P\n\nlet Hidden = 1\n");
    let file1 = impl_file("module Test\nopen P\nopen Tycon.M\nlet x = Hidden\n");
    let proj = resolve_project(&[file0, file1], &env);

    let src1 = "module Test\nopen P\nopen Tycon.M\nlet x = Hidden\n";
    let i = src1.rfind("Hidden").expect("use");
    let use_range = span(i, "Hidden".len());
    assert!(
        matches!(
            proj.file(1).resolution_at(use_range),
            Some(Resolution::Item(_))
        ),
        "a private-representation union contributes no case: the earlier open's \
         `Hidden` resolves — got {:?}",
        proj.file(1).resolution_at(use_range)
    );
}

/// Codex round 22, P1: the generation barrier must shadow **lexical bindings** too,
/// not just earlier opens' entries. FCS's name environment is latest-wins across
/// bindings AND opens, so `let Hit = 1` followed by `open M` — where `M` hides a name
/// we cannot list — may bind M's hidden `Hit`; returning the `let` is a wrong target.
/// Deferring is safe in both directions (if FCS binds the let, we lose availability
/// only).
#[test]
fn a_residue_open_shadows_an_earlier_lexical_binding() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // A union the pickle did not describe: name-unknown residue.
    let mut union = template.clone();
    union.namespace = vec![];
    union.name = "U".to_string();
    union.kind = EntityKind::Union;
    union.members = vec![];
    union.nested_types = vec![];
    union.union_case_names = None;
    let mut m = template.clone();
    m.namespace = vec!["Tycon".to_string()];
    m.name = "M".to_string();
    m.kind = EntityKind::Module;
    m.members = vec![];
    m.nested_types = vec![union];
    let env = AssemblyEnv::from_entities(vec![m]);

    let src = "module Test\nlet Hit = 1\nopen Tycon.M\nlet x = Hit\n";
    let rf = resolve(src, &env);
    let i = src.rfind("Hit").expect("use");
    assert!(
        !matches!(
            rf.resolution_at(span(i, "Hit".len())),
            Some(Resolution::Item(_))
        ),
        "the residue-bearing open may hide a `Hit` that shadows the earlier `let` — \
         the binding must not stay a definite target, got {:?}",
        rf.resolution_at(span(i, "Hit".len()))
    );

    // Control 1: a binding AFTER the open beats it (FCS latest-wins), so it resolves.
    let src = "module Test\nopen Tycon.M\nlet Hit = 1\nlet x = Hit\n";
    let rf = resolve(src, &env);
    let i = src.rfind("Hit").expect("use");
    assert!(
        matches!(
            rf.resolution_at(span(i, "Hit".len())),
            Some(Resolution::Item(_))
        ),
        "a binding after the open outranks anything it hides, got {:?}",
        rf.resolution_at(span(i, "Hit".len()))
    );

    // Control 2: a use BETWEEN the binding and the open still resolves — the hidden
    // names enter at the open's position.
    let src = "module Test\nlet Hit = 1\nlet y = Hit\nopen Tycon.M\n";
    let rf = resolve(src, &env);
    let i = src.rfind("Hit").expect("use");
    assert!(
        matches!(
            rf.resolution_at(span(i, "Hit".len())),
            Some(Resolution::Item(_))
        ),
        "a use before the open is untouched by it, got {:?}",
        rf.resolution_at(span(i, "Hit".len()))
    );
}

/// Codex round 23, P1: a dropped TypeDef can BE the module an `open` names. When
/// projection dropped the only entity at `Drop.M`, the open resolves to NO
/// interpretation at all — but FCS opens the real module, whose exports may shadow
/// any earlier name. A silent no-op open is a wrong-target factory; it must go
/// conservative instead.
#[test]
fn an_open_of_a_wholly_dropped_module_is_not_a_no_op() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");
    let mut vals = template.clone();
    vals.namespace = vec!["Vals".to_string()];
    vals.name = "A".to_string();
    vals.kind = EntityKind::Module;
    vals.nested_types = vec![];
    let mut env = AssemblyEnv::from_entities(vec![vals]);
    // The only TypeDef at `Drop.M` was dropped: it is recorded under its
    // namespace, and nothing visible remains at the path.
    env.mark_namespace_dropped_type(vec!["Drop".to_string()]);

    let src = "open Vals.A\nopen Drop.M\nlet x = Zero ()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "`Drop.M` may be the dropped module and may export `Zero`; the earlier open's \
         member must not survive the open — got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );

    // Control: with nothing dropped, the same shape keeps the earlier member —
    // `open Drop.M` then genuinely names nothing anywhere.
    let mut vals2 = template.clone();
    vals2.namespace = vec!["Vals".to_string()];
    vals2.name = "A".to_string();
    vals2.kind = EntityKind::Module;
    vals2.nested_types = vec![];
    let env = AssemblyEnv::from_entities(vec![vals2]);
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "with no dropped types the unresolvable open hides nothing we must guard, \
         got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

// ===== Slice B/C follow-up: the assembly NAMESPACE half joins the fold =====
//
// Opening a namespace adds its **direct tycon tier** to scope, exactly as opening
// a module does (`AddModuleOrNamespaceContentsToNameEnv`): a namespace-level
// exception constructor becomes bare-resolvable, a namespace-level union's cases
// enter value/pattern scope, a namespace-level type name occupies the constructor
// slot. Until this slice sema folded only a namespace's `[<AutoOpen>]` *modules*,
// so its own tycon tier was invisible — and, because that tier was unmodelled, a
// **cross-kind** open (an FQN that is both an assembly module and a namespace) had
// to blanket-demote the whole module half (`cross_kind`). Folding the namespace
// tycon tier lets the contest be per-name instead, deleting the `has_namespace`
// arm of that demote.

/// A namespace-level **exception** in a referenced assembly folds under
/// `open <namespace>` (FCS: exceptions enter value + pattern scope) — but
/// **opaque**: in scope, shadowing by position, naming nothing (§8 of
/// `docs/assembly-module-open-plan.md`, option A). FCS can re-order the bare
/// name after the fold (a later open's constructible type evicts it from the
/// unqualified constructor slot — cell 8a; a same-surface `[<Literal>]` beats
/// it as a constant pattern — cell 8b), and sema's bare-name lookup models
/// neither, so a definite `Entity` here risks a wrong target. Previously sema
/// folded only a namespace's auto-open modules, so a bare `Boom` after
/// `open Ns` resolved to *nothing*; the opaque fold is the sound middle:
/// `Deferred`, never unbound and never a wrong target. Committing the entity
/// again is §8's option B (model the slot eviction).
#[test]
fn open_of_an_assembly_namespace_folds_a_direct_exception() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    let mut exn = template.clone();
    exn.namespace = vec!["Ns".to_string()];
    exn.name = "Boom".to_string();
    exn.kind = EntityKind::Exception;
    exn.members = vec![];
    exn.nested_types = vec![];
    let env = AssemblyEnv::from_entities(vec![exn]);

    let src = "open Ns\nlet x = Boom 3\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "Boom")) {
        Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "a namespace-level assembly exception must fold opaque — in scope and \
             deferring, not unbound and not a committed entity — got {other:?}"
        ),
    }
}

/// The core availability win of deleting the cross-kind blanket demote: on an FQN
/// that is **both** an assembly module and an assembly namespace, a name unique to
/// the *module* half (a module val) resolves to its definite target. FCS merges
/// the two interpretations (Q9), so both surfaces contribute; a name in only one
/// half is uncontested. The namespace half's unique exception is *in scope* too,
/// but opaque (§8 option A — a definite exception is evictable by orderings the
/// bare-name lookup does not model), so it defers rather than naming its entity.
#[test]
fn a_cross_kind_open_resolves_names_unique_to_each_half() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // The MODULE half `Foo` (root namespace), carrying `Demo.Calc`'s static `Zero`.
    let mut module = template.clone();
    module.namespace = vec![];
    module.name = "Foo".to_string();
    module.kind = EntityKind::Module;
    module.nested_types = vec![];

    // The NAMESPACE half at the same FQN: an exception `Boom` declared directly in
    // namespace `Foo`.
    let mut exn = template.clone();
    exn.namespace = vec!["Foo".to_string()];
    exn.name = "Boom".to_string();
    exn.kind = EntityKind::Exception;
    exn.members = vec![];
    exn.nested_types = vec![];

    let env = AssemblyEnv::from_entities(vec![module, exn]);
    let src = "open Foo\nlet x = Zero ()\nlet y = Boom 3\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "Zero")) {
        Some(Resolution::Member { .. }) => {}
        other => panic!("the module half's unique static `Zero` must resolve, got {other:?}"),
    }
    match rf.resolution_at(at(src, "Boom")) {
        Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "the namespace half's unique exception `Boom` must be in scope but opaque \
             (deferring, not unbound, not a committed entity), got {other:?}"
        ),
    }
}

/// The soundness half: a name supplied by **both** halves of a cross-kind merge is
/// a reference-order contest FCS orders and sema does not — so it defers (in scope,
/// naming nothing), never a wrong target.
#[test]
fn a_cross_kind_open_defers_a_name_supplied_by_both_halves() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // Module half `Foo` with static `Zero`.
    let mut module = template.clone();
    module.namespace = vec![];
    module.name = "Foo".to_string();
    module.kind = EntityKind::Module;
    module.nested_types = vec![];

    // Namespace half `Foo` with an exception *also* named `Zero`.
    let mut exn = template.clone();
    exn.namespace = vec!["Foo".to_string()];
    exn.name = "Zero".to_string();
    exn.kind = EntityKind::Exception;
    exn.members = vec![];
    exn.nested_types = vec![];

    let env = AssemblyEnv::from_entities(vec![module, exn]);
    let src = "open Foo\nlet x = Zero ()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. }) | Some(Resolution::Entity(_))
        ),
        "`Zero` is supplied by both the module and namespace halves; FCS orders \
         them by reference and we do not — defer, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// A namespace-level union with a **case-nameless** representation (the pickle did
/// not describe its cases) is name-unknown residue: its hidden case could outrank
/// an earlier open's value, so the cross-kind open must raise the generation
/// barrier and not hand back the stale earlier member.
#[test]
fn a_cross_kind_namespace_union_without_case_names_shadows_an_earlier_open() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // An earlier open supplying a value `Zero` (a module `Early` with the static).
    let mut early = template.clone();
    early.namespace = vec![];
    early.name = "Early".to_string();
    early.kind = EntityKind::Module;
    early.nested_types = vec![];

    // The cross-kind `Foo`: a module half (no `Zero` of its own) and a namespace
    // half whose union's case names are unknowable.
    let mut module = template.clone();
    module.namespace = vec![];
    module.name = "Foo".to_string();
    module.kind = EntityKind::Module;
    module.members = vec![];
    module.nested_types = vec![];
    let mut union = template.clone();
    union.namespace = vec!["Foo".to_string()];
    union.name = "U".to_string();
    union.kind = EntityKind::Union;
    union.union_case_names = None; // unknowable — a hidden case could be `Zero`
    union.members = vec![];
    union.nested_types = vec![];

    let env = AssemblyEnv::from_entities(vec![early, module, union]);
    let src = "open Early\nopen Foo\nlet x = Zero ()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "the namespace half's case-nameless union may hide a `Zero`; the earlier \
         open's member must be shadowed, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// codex P1: a namespace half's **constructible type name** takes FCS's unqualified
/// constructor slot, so it contests a same-named *value* the module half supplies. It
/// must be a fold contestant, or a bare `Zero` commits the module value where FCS may
/// bind the type (reference order). The head-slot eviction machinery covers a namespace
/// type vs a *local*, not vs an opened value, so the fold must carry the contestant.
#[test]
fn a_cross_kind_namespace_constructible_type_contests_a_module_value() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // Module half `Foo` with static `Zero`.
    let mut module = template.clone();
    module.namespace = vec![];
    module.name = "Foo".to_string();
    module.kind = EntityKind::Module;
    module.nested_types = vec![];

    // Namespace half `Foo` with a **class** `Zero` — constructible, so its type name
    // takes the constructor slot and contests the module value.
    let mut ty = template.clone();
    ty.namespace = vec!["Foo".to_string()];
    ty.name = "Zero".to_string();
    ty.kind = EntityKind::Class;
    ty.is_struct = false;
    ty.members = vec![];
    ty.nested_types = vec![];

    let env = AssemblyEnv::from_entities(vec![module.clone(), ty.clone()]);
    let src = "open Foo\nlet x = Zero ()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "the namespace half's constructible type `Zero` contests the module value; FCS \
         orders the merge by reference and we do not — defer, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );

    // Control: a non-constructible type (interface) takes no constructor slot, so it
    // does not contest — the module value resolves.
    let mut iface = ty.clone();
    iface.kind = EntityKind::Interface;
    let env = AssemblyEnv::from_entities(vec![module, iface]);
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "a non-constructible namespace type takes no slot; the module value must still \
         resolve, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// codex P1: two referenced assemblies each declaring `namespace Ns; exception Boom` is a
/// cross-assembly merge FCS orders by reference — which we do not model — so `open Ns;
/// Boom` must defer. The namespace fold therefore produces **one surface per contributing
/// assembly** (like `opened_assembly_modules`), so the duplicate collides *across*
/// surfaces; a single lumped surface would treat it as declaration-ordered and commit the
/// last.
#[test]
fn a_cross_assembly_namespace_duplicate_defers() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    let mut boom_a = template.clone();
    boom_a.namespace = vec!["Ns".to_string()];
    boom_a.name = "Boom".to_string();
    boom_a.kind = EntityKind::Exception;
    boom_a.members = vec![];
    boom_a.nested_types = vec![];

    // The same FQN from a DIFFERENT assembly (distinct identity).
    let mut boom_b = boom_a.clone();
    boom_b.assembly.name = "OtherAsm".to_string();

    let env = AssemblyEnv::from_entities(vec![boom_a, boom_b]);
    let src = "open Ns\nlet x = Boom 3\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Boom")),
            Some(Resolution::Entity(_))
        ),
        "two assemblies supply `Ns.Boom`; FCS's winner is reference-order-dependent — \
         defer, got {:?}",
        rf.resolution_at(at(src, "Boom"))
    );
}

/// codex review round 2, P2: within ONE assembly FCS folds a namespace's `[<AutoOpen>]`
/// module **after** its tycon tier, so an auto-open value `Zero` wins over a same-named
/// direct type `Zero`. The type is only a *value-slot contestant*, and a contestant
/// demotes a value from another surface, never one in its own — so `open Ns; Zero ()`
/// still resolves the auto-open value.
#[test]
fn a_namespace_auto_open_value_beats_a_sibling_type_of_the_same_name() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // A constructible type `Zero` directly in namespace `Ns`.
    let mut ty = template.clone();
    ty.namespace = vec!["Ns".to_string()];
    ty.name = "Zero".to_string();
    ty.kind = EntityKind::Class;
    ty.is_struct = false;
    ty.members = vec![];
    ty.nested_types = vec![];

    // An `[<AutoOpen>]` module in `Ns` exporting a value `Zero` (Calc's static).
    let mut auto = template.clone();
    auto.namespace = vec!["Ns".to_string()];
    auto.name = "M".to_string();
    auto.kind = EntityKind::Module;
    auto.is_auto_open = true;
    auto.nested_types = vec![];

    let env = AssemblyEnv::from_entities(vec![ty, auto]);
    let src = "open Ns\nlet x = Zero ()\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "Zero")) {
        Some(Resolution::Member { .. }) => {}
        other => panic!(
            "the auto-open module's `Zero` folds after the sibling type, so it wins — \
             got {other:?}"
        ),
    }
}

/// codex review round 2, P2: a namespace's top-level `[<AutoOpen>]` module is a
/// directly-opened root, so FCS folds its own tycons before its own vals — an
/// `[<AutoOpen>]` *type* nested in it is residue *below* the module's vals, which stay
/// definite. Folding it with `top = false` (as a recursed descendant) would promote that
/// to full residue and demote the vals.
#[test]
fn a_namespace_top_level_auto_open_modules_vals_survive_an_auto_open_type() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // A non-generic `[<AutoOpen>]` type — its statics we cannot enumerate (residue).
    let mut aux = template.clone();
    aux.name = "Aux".to_string();
    aux.kind = EntityKind::Class;
    aux.is_auto_open = true;
    aux.generic_parameters = vec![];
    aux.members = vec![];
    aux.nested_types = vec![];

    // The namespace's top-level `[<AutoOpen>]` module: the auto-open type plus the
    // ordinary static `Zero`.
    let mut auto = template.clone();
    auto.namespace = vec!["Ns".to_string()];
    auto.name = "M".to_string();
    auto.kind = EntityKind::Module;
    auto.is_auto_open = true;
    auto.nested_types = vec![aux];

    let env = AssemblyEnv::from_entities(vec![auto]);
    let src = "open Ns\nlet x = Zero ()\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "Zero")) {
        Some(Resolution::Member { .. }) => {}
        other => panic!(
            "the module's own val folds after its tycon tier, so the auto-open type's \
             residue must not demote it — got {other:?}"
        ),
    }
}

/// codex review round 2, P1: a contributing assembly whose F# signature is unknowable can
/// hide erased type abbreviations from a namespace entirely (they live only in the
/// pickle). On a cross-kind `open Foo`, an unseen constructible abbreviation could contest
/// the module half's value, so the namespace fold appends a residue-only surface and the
/// module half must defer.
#[test]
fn a_cross_kind_open_with_an_unknowable_namespace_half_defers_the_module_half() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // The MODULE half `Foo` (a modelled assembly), carrying the static `Zero`.
    let mut module = template.clone();
    module.namespace = vec![];
    module.name = "Foo".to_string();
    module.kind = EntityKind::Module;
    module.nested_types = vec![];

    // The NAMESPACE half at the same FQN, in an assembly whose abbreviations are
    // unknowable — a hidden `Zero` abbreviation could contest the module value.
    let mut ns_marker = template.clone();
    ns_marker.namespace = vec!["Foo".to_string()];
    ns_marker.name = "Marker".to_string();
    ns_marker.kind = EntityKind::Class;
    ns_marker.members = vec![];
    ns_marker.nested_types = vec![];

    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
        (
            std::path::PathBuf::from("Modelled.dll"),
            vec![module],
            borzoi_sema::AbbreviationVisibility::Modelled,
            Vec::new(),
        ),
        (
            std::path::PathBuf::from("Unknowable.dll"),
            vec![ns_marker],
            borzoi_sema::AbbreviationVisibility::Unknowable,
            Vec::new(),
        ),
    ]);
    let src = "open Foo\nlet x = Zero ()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "the namespace half's assembly hides abbreviations; an unseen `Zero` could \
         contest the module value — defer, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// codex review round 6, P1: a pickle-only abbreviation is erased from IL, so it can sit
/// directly in an ANCESTOR of a namespace the unknowable assembly visibly declares into.
/// When the assembly's only surviving type is in `Foo.Sub`, `has_namespace(["Foo"])` is
/// still true (a prefix), but an exact-match residue check misses the hidden `Foo`
/// abbreviation — so `open Foo` (a module in another assembly) could commit a same-named
/// value FCS may bind to that abbreviation. The residue check must span ancestors too.
#[test]
fn a_cross_kind_open_with_an_unknowable_parent_namespace_defers_the_module_half() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // The MODULE half `Foo` (modelled), carrying the static `Zero`.
    let mut module = template.clone();
    module.namespace = vec![];
    module.name = "Foo".to_string();
    module.kind = EntityKind::Module;
    module.nested_types = vec![];

    // An unknowable assembly whose only visible type is in `Foo.Sub` — its erased
    // abbreviations could include a `Zero` directly in `Foo`.
    let mut deep = template.clone();
    deep.namespace = vec!["Foo".to_string(), "Sub".to_string()];
    deep.name = "Marker".to_string();
    deep.kind = EntityKind::Class;
    deep.members = vec![];
    deep.nested_types = vec![];

    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
        (
            std::path::PathBuf::from("Modelled.dll"),
            vec![module],
            borzoi_sema::AbbreviationVisibility::Modelled,
            Vec::new(),
        ),
        (
            std::path::PathBuf::from("Unknowable.dll"),
            vec![deep],
            borzoi_sema::AbbreviationVisibility::Unknowable,
            Vec::new(),
        ),
    ]);
    let src = "open Foo\nlet x = Zero ()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "the unknowable assembly's visible type is in `Foo.Sub` but a hidden `Zero` \
         abbreviation could sit directly in `Foo`; the module value must defer, got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// codex review round 7, P1: a PURE namespace open with residue (a non-generic
/// `[<AutoOpen>]` type whose statics we cannot enumerate) raises the generation barrier,
/// staling every earlier name — including a local binding. A dotted head through that
/// staled local must then DEFER, not fall through to a same-named referenced-assembly
/// path: the residue could be the head, and we cannot prove otherwise. Without the
/// dotted-head guard the staled local `X` rerouted `X.Zero` to the assembly `Bar.X.Zero`.
#[test]
fn a_pure_namespace_residue_does_not_reroute_a_stale_local_dotted_head() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // An assembly namespace `Bar` with a type `X` carrying the static `Zero`, so
    // `open Bar` makes `X.Zero` a resolvable assembly path.
    let mut bar_x = template.clone();
    bar_x.namespace = vec!["Bar".to_string()];
    bar_x.name = "X".to_string();
    // (keep `Calc`'s members, so `X.Zero` is a real static)
    bar_x.nested_types = vec![];

    // An assembly namespace `Foo` whose only content is a non-generic `[<AutoOpen>]`
    // type — residue we cannot enumerate, which raises the barrier.
    let mut auto_ty = template.clone();
    auto_ty.namespace = vec!["Foo".to_string()];
    auto_ty.name = "AutoTy".to_string();
    auto_ty.kind = EntityKind::Class;
    auto_ty.is_auto_open = true;
    auto_ty.generic_parameters = vec![];
    auto_ty.members = vec![];
    auto_ty.nested_types = vec![];

    let env = AssemblyEnv::from_entities(vec![bar_x, auto_ty]);
    let src = "open Bar\nlet X = {| Zero = 3 |}\nopen Foo\nlet z = X.Zero\n";
    let rf = resolve(src, &env);
    let head = span(src.rfind("X.Zero").expect("use"), 1);
    assert!(
        !matches!(
            rf.resolution_at(head),
            Some(Resolution::Member { .. }) | Some(Resolution::Entity(_))
        ),
        "the staled local `X` must not reroute `X.Zero` to the assembly `Bar.X.Zero`; \
         head resolved to {:?}",
        rf.resolution_at(head)
    );
    let whole = span(src.rfind("X.Zero").expect("use"), "X.Zero".len());
    assert!(
        !matches!(rf.resolution_at(whole), Some(Resolution::Member { .. })),
        "`X.Zero` must not bind the assembly member, got {:?}",
        rf.resolution_at(whole)
    );
}

/// codex review rounds 3+8: a cross-kind `open Foo` whose module half exports a *value*
/// `Zero` and whose namespace half exports an *exception* `Zero` is a reference-order
/// contest. F# keeps the value and pattern namespaces separate, so in principle the
/// exception could still win a *pattern* `Zero` — but a value that is a `[<Literal>]`
/// (or a `decimal` literal, emitted as an init-only field, Q17) is a constant pattern
/// too, and we cannot cheaply tell it from a plain value. So a collided constructor
/// entry conservatively defers in **both** namespaces: correctness (never binding the
/// exception where FCS may bind a colliding literal) over the narrow availability of a
/// pattern-only survivor.
#[test]
fn a_cross_kind_value_exception_collision_defers_in_both_namespaces() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // Module half `Foo` with a value `Zero` (Calc's static).
    let mut module = template.clone();
    module.namespace = vec![];
    module.name = "Foo".to_string();
    module.kind = EntityKind::Module;
    module.nested_types = vec![];

    // Namespace half `Foo` with an exception *also* named `Zero`.
    let mut exn = template.clone();
    exn.namespace = vec!["Foo".to_string()];
    exn.name = "Zero".to_string();
    exn.kind = EntityKind::Exception;
    exn.members = vec![];
    exn.nested_types = vec![];

    let env = AssemblyEnv::from_entities(vec![module, exn]);

    // Both positions defer — the collision is not split by namespace.
    let pat_src = "open Foo\nlet f x = match x with Zero -> 1 | _ -> 0\n";
    let rf = resolve(pat_src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(pat_src, "Zero")),
            Some(Resolution::Entity(_)) | Some(Resolution::Member { .. })
        ),
        "a pattern `Zero` must not bind the exception — a colliding literal could win \
         (Q17); defer, got {:?}",
        rf.resolution_at(at(pat_src, "Zero"))
    );

    let expr_src = "open Foo\nlet y = Zero ()\n";
    let rf = resolve(expr_src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(expr_src, "Zero")),
            Some(Resolution::Member { .. }) | Some(Resolution::Entity(_))
        ),
        "an expression `Zero` is a value-space contest — defer, got {:?}",
        rf.resolution_at(at(expr_src, "Zero"))
    );
}

/// codex review round 4, P1: a namespace half's constructor-slot type enters FCS's
/// unqualified slot and evicts a same-named value from an EARLIER open, even when the
/// cross-kind group supplies no such name itself (so no collision entry is emitted). The
/// generation barrier must still rise, or the stale earlier value is returned where FCS
/// binds the type.
#[test]
fn a_cross_kind_namespace_type_shadows_an_earlier_opens_value() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // An earlier open supplying a value `Zero`.
    let mut early = template.clone();
    early.namespace = vec![];
    early.name = "Early".to_string();
    early.kind = EntityKind::Module;
    early.nested_types = vec![];

    // The cross-kind `Foo`: a module half with NO `Zero`, and a namespace half with a
    // constructible type `Zero` (which evicts the earlier value).
    let mut module = template.clone();
    module.namespace = vec![];
    module.name = "Foo".to_string();
    module.kind = EntityKind::Module;
    module.members = vec![];
    module.nested_types = vec![];
    let mut ty = template.clone();
    ty.namespace = vec!["Foo".to_string()];
    ty.name = "Zero".to_string();
    ty.kind = EntityKind::Class;
    ty.is_struct = false;
    ty.members = vec![];
    ty.nested_types = vec![];

    let env = AssemblyEnv::from_entities(vec![early, module, ty]);
    let src = "open Early\nopen Foo\nlet x = Zero ()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Zero")),
            Some(Resolution::Member { .. })
        ),
        "the namespace type `Zero` evicts the earlier open's value; it must not survive \
         the later open — got {:?}",
        rf.resolution_at(at(src, "Zero"))
    );
}

/// codex review round 9, cell 8a (§8 of `docs/assembly-module-open-plan.md`, closed by
/// option A). A namespace-folded exception must not survive a LATER same-named
/// constructible type opened from another pure namespace: `open Foo` (exception `X`),
/// `open Bar` (constructible `type X`), bare `X ()` — FCS binds `Bar.X` (its ctor slot
/// evicts the exception), an eviction sema's bare-name lookup does not model. The
/// namespace half therefore folds its exceptions **opaque** (in scope, shadowing,
/// naming nothing), so the bare use defers instead of naming the evicted exception.
/// Modelling the eviction and recovering the definite target is §8's option B.
#[test]
fn a_later_namespace_type_evicts_an_earlier_namespace_exception() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // `Foo`: an exception `X`.
    let mut exn = template.clone();
    exn.namespace = vec!["Foo".to_string()];
    exn.name = "X".to_string();
    exn.kind = EntityKind::Exception;
    exn.members = vec![];
    exn.nested_types = vec![];

    // `Bar`: a constructible type `X` (opened LATER — its ctor slot evicts the exn).
    let mut ty = template.clone();
    ty.namespace = vec!["Bar".to_string()];
    ty.name = "X".to_string();
    ty.kind = EntityKind::Class;
    ty.is_struct = false;
    ty.members = vec![];
    ty.nested_types = vec![];

    let env = AssemblyEnv::from_entities(vec![exn, ty]);
    let src = "open Foo\nopen Bar\nlet z = X ()\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "X")) {
        // Option B would flip this to a definite `Bar.X` target; until then the
        // opaque exception entry is the latest `X` in scope and defers.
        Some(Resolution::Deferred(_)) => {}
        other => panic!(
            "the later type `Bar.X` evicts the earlier exception `Foo.X`; bare `X` must \
             defer (and above all must not bind the exception), got {other:?}"
        ),
    }
}

/// codex review round 10 — a risen generation barrier must also veto dotted-head
/// fallback. The cross-kind type barrier (`cross_kind_ns_type`) stales every earlier
/// entry, an unrelated **local** included; a dotted head through that staled local
/// (`X.Zero` after `let X = 5`) must then DEFER — FCS still binds the local, whose
/// slot nothing named `X` contested — and must NOT fall through to the qualified
/// block, which can still see a referenced assembly's `Bar.X.Zero` through the
/// earlier `open Bar` and would record it as the target. The residue arms have had
/// this veto since codex round 7; the round-4 cross-kind-type arm bumped without it.
#[test]
fn a_cross_kind_type_barrier_defers_an_unrelated_locals_dotted_head() {
    let template = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc");

    // `Bar`: an assembly namespace whose class `X` carries a static `Zero` — the
    // wrong target the qualified block would reach for `X.Zero`.
    let mut bar_ty = template.clone();
    bar_ty.namespace = vec!["Bar".to_string()];
    bar_ty.name = "X".to_string();
    bar_ty.kind = EntityKind::Class;
    bar_ty.is_struct = false;
    bar_ty.nested_types = vec![];

    // The cross-kind `Foo`: a childless module half plus a namespace half whose
    // constructible `type Y` — a name unrelated to `X` — raises the barrier.
    let mut foo_module = template.clone();
    foo_module.namespace = vec![];
    foo_module.name = "Foo".to_string();
    foo_module.kind = EntityKind::Module;
    foo_module.members = vec![];
    foo_module.nested_types = vec![];
    let mut foo_ty = template.clone();
    foo_ty.namespace = vec!["Foo".to_string()];
    foo_ty.name = "Y".to_string();
    foo_ty.kind = EntityKind::Class;
    foo_ty.is_struct = false;
    foo_ty.members = vec![];
    foo_ty.nested_types = vec![];

    let env = AssemblyEnv::from_entities(vec![bar_ty, foo_module, foo_ty]);
    let src = "open Bar\nlet X = 5\nopen Foo\nlet t () = X.Zero\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "X.Zero")),
            Some(Resolution::Member { .. }) | Some(Resolution::Entity(_))
        ),
        "the dotted head `X` is a staled local, not the assembly's `Bar.X`; the path \
         must defer, got {:?}",
        rf.resolution_at(at(src, "X.Zero"))
    );
}

/// Cross-assembly classification (semantic tokens): the type a qualified path
/// roots at, and its members, are classified against the [`AssemblyEnv`] by the
/// project-level [`ResolvedProject::token_classifier`]. `Demo.Calc` is a static
/// class; `Zero` a static method; `Answer` a get-only property. This is
/// compositional — cross-assembly *resolution* is checked against FCS in
/// `resolve_assembly_diff`, and the kind mapping is [`AssemblyEnv::entity_class`]
/// / [`AssemblyEnv::member_class`] — so it pins that the classifier wires them
/// together (including the qualified *tails* `Zero` / `Answer`, which have no
/// exact key of their own).
#[test]
fn token_classifier_classifies_referenced_assembly_symbols() {
    let env = fixture_env();
    let src = "module M\nlet z = Demo.Calc.Zero()\nlet a = Demo.Calc.Answer\n";
    let proj = resolve_project(&[impl_file(src)], &env);
    let classify = proj.token_classifier(0, &env);

    // The type the path roots at.
    assert_eq!(
        classify(at(src, "Calc")),
        Some(SemanticClass::Type),
        "`Demo.Calc` roots at a type"
    );
    // Qualified member tails — no exact key of their own; classified via the
    // whole-path occurrence ending at the tail.
    assert_eq!(
        classify(at(src, "Zero")),
        Some(SemanticClass::Method),
        "`Zero` is a static method"
    );
    assert_eq!(
        classify(at(src, "Answer")),
        Some(SemanticClass::Property),
        "`Answer` is a property"
    );
}

/// Cross-assembly member classification reads the **parent** entity's kind: a
/// member of an F# `module` is a `let` (a rebranded value → `Value`, a *generic*
/// value like `typeof<'T>` also → `Value` via `is_module_value_binding`, or a
/// module function → `Function`), not a C#-style `Method`; a module-owned *field*
/// is a `[<Literal>]` value → `Value` (not a `Property`); and a field of an `enum`
/// is a case (`EnumCase`), not a `Property`. Built from synthetic entities (the C#
/// fixture has neither an F# module nor an enum) by cloning a real fixture method
/// and field into module / enum / class parents.
#[test]
fn member_class_reads_parent_kind_for_module_lets_and_enum_cases() {
    let entities = fixture_entities();
    let find = |ns: &str, name: &str| {
        entities
            .iter()
            .find(|e| e.namespace == vec![ns.to_string()] && e.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("{ns}.{name} in fixture"))
    };
    // A real static method and a real field to use as templates.
    let calc = find("Demo", "Calc");
    let method_tpl = calc
        .members
        .iter()
        .find_map(|m| match m {
            Member::Method(mm) => Some(mm.clone()),
            _ => None,
        })
        .expect("a method in Demo.Calc");
    let field_tpl = find("Demo", "Widget")
        .members
        .iter()
        .find_map(|m| match m {
            Member::Field(f) => Some(f.clone()),
            _ => None,
        })
        .expect("a field in Demo.Widget");

    let method = |name: &str, module_value: Option<ModuleValue>, value_binding: bool| {
        let mut m = method_tpl.clone();
        m.name = name.to_string();
        m.source_name = Some(name.to_string());
        m.module_value = module_value;
        m.is_module_value_binding = value_binding;
        Member::Method(m)
    };
    let field = |name: &str| {
        let mut f = field_tpl.clone();
        f.name = name.to_string();
        Member::Field(f)
    };
    let entity = |name: &str, kind: EntityKind, members: Vec<Member>| {
        let mut e = calc.clone();
        e.namespace = vec!["Ns".to_string()];
        e.name = name.to_string();
        e.kind = kind;
        e.members = members;
        e.nested_types = vec![];
        e
    };

    let env = AssemblyEnv::from_entities(vec![
        entity(
            "Mod",
            EntityKind::Module,
            vec![
                method("Fn", None, false),
                method("Val", Some(ModuleValue { is_mutable: false }), true),
                // A *generic* module value (`typeof<'T>`/`sizeof<'T>`): fsc emits it as
                // a generic method (a CLR property cannot be generic), so `module_value`
                // is `None` — but the pickle records zero argument groups, flagged as
                // `is_module_value_binding`. FCS reports `IsValue=true, IsFunction=false`
                // (probed on real FSharp.Core), so it must classify as a value, not a
                // function.
                method("GenVal", None, true),
                // A module's only surfaced field is a `[<Literal>] let` — fsc emits it
                // as a static literal *field*, not an accessor method, and the F# pickle
                // merge claims that field into the module's member list
                // (`rebuild_module_member_list`). FCS still classifies a use of it as a
                // module *value* (`Mfv`/`IsValue`), so it must not fall through to
                // `Property` like a genuine data field on a class.
                field("Lit"),
            ],
        ),
        entity("Colors", EntityKind::Enum, vec![field("Red")]),
        entity("Box", EntityKind::Class, vec![field("Data")]),
        entity("Boom", EntityKind::Exception, vec![]),
    ]);
    let class_of = |ns: &[&str], ty: &str, member: &str| {
        let handle = env
            .lookup_type(&ns.iter().map(|s| s.to_string()).collect::<Vec<_>>(), ty, 0)
            .unwrap_or_else(|| panic!("Ns.{ty} in env"));
        let idx = env
            .member(handle, member)
            .unwrap_or_else(|| panic!("{ty}.{member}"));
        // These synthetic entities are authoritative (`from_entities` sets no
        // non-authoritative flag), so `member_class` always commits here.
        env.member_class(handle, idx)
            .unwrap_or_else(|| panic!("member_class declined {ty}.{member} unexpectedly"))
    };

    // Module: its own entity is a module; its function and value members split.
    let module = env
        .lookup_type(&["Ns".to_string()], "Mod", 0)
        .expect("Ns.Mod");
    assert_eq!(
        env.entity_class(module),
        Some(SemanticClass::Module),
        "an F# module"
    );
    assert_eq!(
        class_of(&["Ns"], "Mod", "Fn"),
        SemanticClass::Function,
        "a module function is a function, not a method"
    );
    assert_eq!(
        class_of(&["Ns"], "Mod", "Val"),
        SemanticClass::Value,
        "a rebranded module value is a value, not a method"
    );
    assert_eq!(
        class_of(&["Ns"], "Mod", "GenVal"),
        SemanticClass::Value,
        "a generic module value (`typeof<'T>`) is a value, not a function, even though \
         `module_value` is `None` (it is method-emitted, flagged `is_module_value_binding`)"
    );
    assert_eq!(
        class_of(&["Ns"], "Mod", "Lit"),
        SemanticClass::Value,
        "a module's `[<Literal>]` surfaces as a Field but is a value, not a property"
    );

    // Enum: the type is a type; its field is a case.
    let colors = env
        .lookup_type(&["Ns".to_string()], "Colors", 0)
        .expect("Ns.Colors");
    assert_eq!(
        env.entity_class(colors),
        Some(SemanticClass::Type),
        "an enum type"
    );
    assert_eq!(
        class_of(&["Ns"], "Colors", "Red"),
        SemanticClass::EnumCase,
        "an enum's field is one of its cases"
    );

    // Control: the same field under a class is data → property.
    assert_eq!(
        class_of(&["Ns"], "Box", "Data"),
        SemanticClass::Property,
        "a class field is data, not a case"
    );

    // An exception entity declines: its `Resolution::Entity` carries no
    // constructor-vs-type occurrence role, so committing either would mis-colour
    // the other.
    let boom = env
        .lookup_type(&["Ns".to_string()], "Boom", 0)
        .expect("Ns.Boom");
    assert_eq!(
        env.entity_class(boom),
        None,
        "an exception entity is declined, not classified as a type"
    );
}

/// A **non-authoritative** F# assembly (`fsc --standalone`, or an absent/undecodable
/// host pickle) keeps its IL-level `CompilationMappingAttribute` module markers, but
/// FCS does not share them: it imports such an assembly through IL, where a module is
/// a plain type and its `let`s are ordinary members (verified end-to-end during
/// development against a real `--standalone` DLL — its module `Host.H` reports
/// `IsModule=false` to FCS, and our projector's
/// `fsharp_signature_non_authoritative` bit is `true` for it). So `entity_class` /
/// `member_class` must **decline** the module classes rather than commit
/// `Module`/`Value`/`Function` a referenced view does not actually present
/// (under-colour, never mis-colour). Pinned deterministically through the runtime
/// projection-knowability constructor, which carries the per-assembly authority bit.
#[test]
fn non_authoritative_assembly_declines_module_classification() {
    let calc = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc in fixture");
    let method_tpl = calc
        .members
        .iter()
        .find_map(|m| match m {
            Member::Method(mm) => Some(mm.clone()),
            _ => None,
        })
        .expect("a method in Demo.Calc");

    // A module `Ns.Mod` with a value member `Val`.
    let mut val = method_tpl;
    val.name = "Val".to_string();
    val.source_name = Some("Val".to_string());
    val.module_value = Some(ModuleValue { is_mutable: false });
    let mut module = calc;
    module.namespace = vec!["Ns".to_string()];
    module.name = "Mod".to_string();
    module.kind = EntityKind::Module;
    module.members = vec![Member::Method(val)];
    module.nested_types = vec![];

    // The runtime constructor tags entities with an `AssemblyId` and records the
    // per-assembly `fsharp_signature_non_authoritative` bit the gate reads.
    let build = |non_authoritative: bool| {
        AssemblyEnv::from_assemblies_with_projection_knowability(vec![(
            std::path::PathBuf::from("Test.dll"),
            vec![module.clone()],
            AbbreviationVisibility::Modelled,
            false,
            non_authoritative,
            Vec::new(),
        )])
    };
    let classes = |env: &AssemblyEnv| {
        let h = env
            .lookup_type(&["Ns".to_string()], "Mod", 0)
            .expect("Ns.Mod");
        let idx = env.member(h, "Val").expect("Mod.Val");
        (env.entity_class(h), env.member_class(h, idx))
    };

    // Authoritative (control): the module and its value classify normally.
    assert_eq!(
        classes(&build(false)),
        (Some(SemanticClass::Module), Some(SemanticClass::Value)),
        "an authoritative module and its value classify"
    );
    // Non-authoritative: both decline.
    assert_eq!(
        classes(&build(true)),
        (None, None),
        "a non-authoritative assembly's module kind and members are declined"
    );
}

/// The same authority split governs **module-qualified member ownership**
/// (`static_lookup` → `qualified_path_occupied`): an *authoritative* module
/// takes the in-module search domain (no base chain, so `Object`'s members do
/// not occupy — the `String.Equals` fix), but a *non-authoritative* one is a
/// plain type to FCS, so it takes the type rule and its base chain occupies
/// `Object`'s member names. Pinned on `Equals`, an `Object` method absent from
/// the module's own contents: authoritative ⇒ `Absent` (a lower reading may own
/// the path), non-authoritative ⇒ `Uncertain` (the base chain occupies it, so
/// the path stays owned). Same builder as
/// [`non_authoritative_assembly_declines_module_classification`].
#[test]
fn non_authoritative_module_uses_the_type_rule_for_qualified_ownership() {
    use borzoi_sema::StaticLookup;

    let calc = fixture_entities()
        .into_iter()
        .find(|e| e.namespace == vec!["Demo".to_string()] && e.name == "Calc")
        .expect("Demo.Calc in fixture");
    let method_tpl = calc
        .members
        .iter()
        .find_map(|m| match m {
            Member::Method(mm) => Some(mm.clone()),
            _ => None,
        })
        .expect("a method in Demo.Calc");

    // A module `Ns.Mod` with a single value member `Val` — and no member or
    // child named `Equals`, so the only way `Equals` is occupied is via the
    // (type-rule) base chain.
    let mut val = method_tpl;
    val.name = "Val".to_string();
    val.source_name = Some("Val".to_string());
    val.module_value = Some(ModuleValue { is_mutable: false });
    let mut module = calc;
    module.namespace = vec!["Ns".to_string()];
    module.name = "Mod".to_string();
    module.kind = EntityKind::Module;
    module.members = vec![Member::Method(val)];
    module.nested_types = vec![];

    let build = |non_authoritative: bool| {
        AssemblyEnv::from_assemblies_with_projection_knowability(vec![(
            std::path::PathBuf::from("Test.dll"),
            vec![module.clone()],
            AbbreviationVisibility::Modelled,
            false,
            non_authoritative,
            Vec::new(),
        )])
    };
    let equals = |env: &AssemblyEnv| {
        let h = env
            .lookup_type(&["Ns".to_string()], "Mod", 0)
            .expect("Ns.Mod");
        env.static_lookup(h, "Equals")
    };

    assert_eq!(
        equals(&build(false)),
        StaticLookup::Absent,
        "an authoritative module ignores `Object.Equals` — a lower reading may own the path"
    );
    assert_eq!(
        equals(&build(true)),
        StaticLookup::Uncertain,
        "a non-authoritative module takes the type rule, whose base chain occupies `Equals`"
    );
}
