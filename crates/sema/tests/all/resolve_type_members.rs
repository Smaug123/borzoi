//! FCS-free tests for **project-type static member** resolution — `Color.Red`
//! where `Color` is a same-file type and `Red` is one of its *static members*
//! (property, `member val`, method), plus the module-qualified form
//! `Pal.Color.Red`. The head/type segment resolves to the type, the whole
//! dotted span to the member — the same recording shape as type-qualified
//! cases.
//!
//! Pinned against FCS by the M-series probes (`docs/project-type-member-plan.md`,
//! all `dotnet build`-verified 2026-07-02):
//! - M1: both `Color2.Red` (2-seg) and `Pal.Color.Red` (3-seg) resolve; FCS's
//!   `DeclRange` is the member's *name* range.
//! - M2a: the member beats a companion `module Color = let Red`.
//! - M2c/M2d: the qualifier is latest-wins across the type and value
//!   namespaces (a *later* value takes it — member access on the value; a
//!   later type takes it back) — the same rule as enum cases.
//! - M4a/M4b: a same-file augmentation's members exist only *from the
//!   augmentation's position*.
//! - M9: an instance member owns the name (FCS commits and errors FS0806) —
//!   never emit, never fall through.
//! - M6: inherited statics resolve through the derived name to the *base*'s
//!   member — unmodeled (base chasing), so a type with `inherit` defers all
//!   member emission.
//!
//! Slice 1 is same-file emit only: contested or unprobed shapes (overloads,
//! access-restricted members, operator names, `inherit`, unindexable
//! augmentations) defer — sound, never a wrong target.

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

fn nth(src: &str, needle: &str, n: usize) -> TextRange {
    let mut from = 0;
    for _ in 0..n {
        let i = src[from..].find(needle).expect("occurrence") + from;
        from = i + needle.len();
    }
    let i = src[from..].find(needle).expect("occurrence") + from;
    TextRange::new(
        rowan::TextSize::from(i as u32),
        rowan::TextSize::from((i + needle.len()) as u32),
    )
}

/// Assert the resolution at `use_range` is a def whose *name range* is
/// `def_range` (the member/type name token), via the file's own def arena.
fn assert_def_at(rf: &ResolvedFile, src: &str, use_range: TextRange, def_range: TextRange) {
    let res = rf
        .resolution_at(use_range)
        .unwrap_or_else(|| panic!("no resolution at {use_range:?} in {src:?}"));
    let def = rf
        .resolved_def(res)
        .unwrap_or_else(|| panic!("{res:?} at {use_range:?} has no in-file def in {src:?}"));
    assert_eq!(
        def.range, def_range,
        "wrong def for {use_range:?} in {src:?}: got {:?} ({})",
        def.range, def.name
    );
}

/// Assert an honest defer (unrecorded or `Deferred`) at `use_range`.
fn assert_defers_at(rf: &ResolvedFile, src: &str, use_range: TextRange) {
    match rf.resolution_at(use_range) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("expected defer at {use_range:?} in {src:?}, got {other:?}"),
    }
}

// ---- D1 same-file emit (M1) ----

#[test]
fn type_qualified_static_member_resolves() {
    // M1 (2-seg): `Color2.Red` — whole span → the member, head → the type.
    let src = "module Demo\ntype Color2() =\n    static member Red = 2\nlet y = Color2.Red\n";
    let rf = resolve(src);
    assert_def_at(&rf, src, nth(src, "Color2.Red", 0), nth(src, "Red", 0));
    assert_def_at(&rf, src, nth(src, "Color2", 1), nth(src, "Color2", 0));
}

#[test]
fn module_qualified_static_member_resolves() {
    // M1 (3-seg): `Pal.Color.Red` — whole span → the member, type segment →
    // the type, module head deferred (no module-as-def).
    let src = "module Demo\nmodule Pal =\n    type Color() =\n        static member Red = 1\nlet x = Pal.Color.Red\n";
    let rf = resolve(src);
    assert_def_at(&rf, src, nth(src, "Pal.Color.Red", 0), nth(src, "Red", 0));
    assert_def_at(&rf, src, nth(src, "Color", 1), nth(src, "Color", 0));
}

#[test]
fn static_member_val_and_method_resolve() {
    // `member val` (auto-property) and a single (un-overloaded) static method
    // are emit-eligible statics like a plain property.
    let src = "module Demo\ntype Color() =\n    static member val Red = 1 with get, set\n    static member Make (x: int) = x\nlet a = Color.Red\nlet b = Color.Make 3\n";
    let rf = resolve(src);
    assert_def_at(&rf, src, nth(src, "Color.Red", 0), nth(src, "Red", 0));
    assert_def_at(&rf, src, nth(src, "Color.Make", 0), nth(src, "Make", 0));
}

#[test]
fn member_beats_a_companion_module_value() {
    // M2a: FCS resolves `Pal.Color.Red` to the type's member even with a
    // companion `module Color = let Red` — the member wins the segment.
    let src = "module Demo\nmodule Pal =\n    type Color() =\n        static member Red = 1\n    module Color =\n        let Red = 2\nlet x = Pal.Color.Red\n";
    let rf = resolve(src);
    assert_def_at(&rf, src, nth(src, "Pal.Color.Red", 0), nth(src, "Red", 0));
}

// ---- The qualifier is latest-wins across type/value namespaces (M2c/M2d) ----

#[test]
fn a_later_value_takes_the_qualifier_from_the_member() {
    // M2c: `let Color = …` *after* the type — FCS reads `Color.Red` as member
    // access on the value (the anonymous-record field), so the member must not
    // emit; the head resolves to the value.
    let src = "module Demo\ntype Color() =\n    static member Red = 1\nlet Color = {| Red = 3 |}\nlet x = Color.Red\n";
    let rf = resolve(src);
    assert_defers_at(&rf, src, nth(src, "Color.Red", 0));
    // The head is the value (member access), not the type.
    let head = nth(src, "Color", 2);
    let res = rf.resolution_at(head).expect("head resolves to the value");
    let def = rf.resolved_def(res).expect("value def");
    assert_eq!(def.range, nth(src, "Color", 1), "head must be the value");
}

#[test]
fn a_later_type_takes_the_qualifier_back_from_the_value() {
    // M2d: the type is *later* than the value — the member emits (latest-wins
    // across the type and value namespaces, the enum-case rule).
    let src = "module Demo\nlet Color = {| Red = 3 |}\ntype Color() =\n    static member Red = 1\nlet x = Color.Red\n";
    let rf = resolve(src);
    assert_def_at(&rf, src, nth(src, "Color.Red", 0), nth(src, "Red", 1));
    assert_def_at(&rf, src, nth(src, "Color", 2), nth(src, "Color", 1));
}

// ---- Augmentations are position-sensitive (M4a/M4b) ----

#[test]
fn augmentation_member_resolves_after_its_declaration() {
    // M4b: a same-file `type Color with static member Red` — the member
    // resolves after the augmentation.
    let src = "module Demo\ntype Color() =\n    static member Blue = 0\ntype Color with\n    static member Red = 1\nlet x = Color.Red\n";
    let rf = resolve(src);
    assert_def_at(&rf, src, nth(src, "Color.Red", 0), nth(src, "Red", 0));
}

#[test]
fn augmentation_member_defers_before_its_declaration() {
    // M4a: before the augmentation the member does not exist (FS0039) — the
    // use must not resolve to the later augmentation's member.
    let src = "module Demo\ntype Color() =\n    static member Blue = 0\nlet x = Color.Red\ntype Color with\n    static member Red = 1\n";
    let rf = resolve(src);
    assert_defers_at(&rf, src, nth(src, "Color.Red", 0));
}

// ---- Owned-but-not-emittable shapes defer (never fall through) ----

#[test]
fn instance_member_at_the_segment_defers() {
    // M9: FCS commits to the instance member (FS0806, not a backtrack) — the
    // name is owned; sema must neither emit nor resolve past it.
    let src = "module Demo\ntype Color() =\n    member _.Red = 1\nlet x = Color.Red\n";
    let rf = resolve(src);
    assert_defers_at(&rf, src, nth(src, "Color.Red", 0));
}

#[test]
fn overloaded_static_member_defers() {
    // Two `Make` overloads: FCS picks by argument type (inference), so sema
    // cannot name a single def — defer.
    let src = "module Demo\ntype Color() =\n    static member Make (x: int) = x\n    static member Make (x: string) = 0\nlet a = Color.Make 3\n";
    let rf = resolve(src);
    assert_defers_at(&rf, src, nth(src, "Color.Make", 0));
}

#[test]
fn access_restricted_static_member_defers() {
    // `static member private` — access rules are unmodeled; owned, no emit.
    let src = "module Demo\ntype Color() =\n    static member private Red = 1\nlet x = Color.Red\n";
    let rf = resolve(src);
    assert_defers_at(&rf, src, nth(src, "Color.Red", 0));
}

#[test]
fn a_type_with_inherit_defers_member_emission() {
    // M6: inherited statics resolve through the derived name to the *base*'s
    // member — sema does not chase bases, so a type with `inherit` defers all
    // member emission (even of its own members: shadowing is unprobed).
    let src = "module Demo\ntype Base() =\n    static member Red = 1\ntype Color() =\n    inherit Base()\n    static member Blue = 2\nlet x = Color.Blue\nlet y = Color.Red\n";
    let rf = resolve(src);
    assert_defers_at(&rf, src, nth(src, "Color.Blue", 0));
    assert_defers_at(&rf, src, nth(src, "Color.Red", 0));
}

#[test]
fn pattern_position_static_member_never_emits() {
    // A static member is not a pattern (only cases/literals/actives match) —
    // pattern position must not emit the member.
    let src = "module Demo\ntype Color() =\n    static member Red = 1\nlet f c = match c with Color.Red -> 0 | _ -> 1\n";
    let rf = resolve(src);
    assert_defers_at(&rf, src, nth(src, "Color.Red", 0));
}

#[test]
fn a_union_case_still_beats_the_member_machinery() {
    // Guard: a union type's *case* keeps resolving through the case path (the
    // member index must not shadow or duplicate it). M2b pins that a case and
    // a member can never share a name, so the two indexes are disjoint.
    let src = "module Demo\ntype Color =\n    | Red\n    | Blue\n    static member Green = 1\nlet x = Color.Red\nlet y = Color.Green\n";
    let rf = resolve(src);
    assert_def_at(&rf, src, nth(src, "Color.Red", 0), nth(src, "Red", 0));
    assert_def_at(&rf, src, nth(src, "Color.Green", 0), nth(src, "Green", 0));
}

// ---- A cross-file module at the head outranks the same-file type ----

#[test]
fn a_cross_file_type_shadow_does_not_contest_the_qualifier() {
    // Probe M19 (dotnet-build-verified, fcs-dump-pinned; codex round 5, P1): a
    // `namespace global` type in an earlier file puts a TYPE at a root
    // completion of the head — but a cross-file type never outranks the
    // lexical type (the CF12/CF13 principle): FCS binds the same-file case.
    // The contest guard must consult only REAL modules — its old conflated-
    // shadow clause vetoed the same-file emit here and the fall-through then
    // bound file0's same-written-path case: a wrong target.
    let src0 = "namespace global\ntype Color =\n    | Red = 1\n    | Blue = 2\n";
    let enum_src =
        "module Client\ntype Color =\n    | Red = 0\n    | Blue = 1\nlet x = Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(enum_src)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(1);
    let res = rf
        .resolution_at(nth(enum_src, "Color.Red", 0))
        .expect("the same-file enum case resolves");
    let def = rf
        .resolved_def(res)
        .expect("a same-file def, never file0's case");
    assert_eq!(def.range, nth(enum_src, "Red", 0), "→ this file's Red case");

    let member_src =
        "module Client\ntype Color() =\n    static member Red = 1\nlet x = Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(member_src)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(1);
    let res = rf
        .resolution_at(nth(member_src, "Color.Red", 0))
        .expect("the same-file member resolves");
    let def = rf
        .resolved_def(res)
        .expect("a same-file def, never file0's case");
    assert_eq!(def.range, nth(member_src, "Red", 0), "→ the static member");
}

#[test]
fn a_residual_less_cross_file_namespace_does_not_contest_the_qualifier() {
    // Probe M18 (dotnet-build-verified, fcs-dump-pinned; codex round 4): a
    // cross-file `namespace Color` shares the head name but a namespace can
    // never own a 2-segment *expression* residual (values cannot live directly
    // under namespaces), so FCS backtracks past it and binds the same-file
    // enum case / member. Namespaces must not veto the qualifier emit.
    let src0 = "namespace Color\nmodule Placeholder =\n    let y = 1\n";
    let enum_src = "namespace Client\ntype Color =\n    | Red = 0\n    | Blue = 1\nmodule Use =\n    let x = Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(enum_src)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(1);
    let res = rf
        .resolution_at(nth(enum_src, "Color.Red", 0))
        .expect("the same-file enum case resolves");
    let def = rf.resolved_def(res).expect("same-file case def");
    assert_eq!(def.range, nth(enum_src, "Red", 0), "→ the enum's Red case");

    let member_src = "namespace Client\ntype Color() =\n    static member Red = 1\nmodule Use =\n    let x = Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(member_src)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(1);
    let res = rf
        .resolution_at(nth(member_src, "Color.Red", 0))
        .expect("the same-file member resolves");
    let def = rf.resolved_def(res).expect("same-file member def");
    assert_eq!(def.range, nth(member_src, "Red", 0), "→ the static member");
}

#[test]
fn a_cross_file_module_at_the_head_takes_the_qualifier_from_the_member() {
    // Probe M13 (dotnet-build-verified, fcs-dump-pinned): with an earlier
    // file's `module Color / let Red = 99`, FCS resolves `Color.Red` to the
    // MODULE's value — the module namespace owns the dotted head over the
    // same-file type (the r13 rule, cross-file), regardless of position. The
    // member emit must stand down and let the qualified-value path resolve it.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\ntype Color() =\n    static member Red = 1\nlet x = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Color.Red", 0))
        .expect("the cross-file module's value resolves");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("a cross-file item, never the same-file member");
    assert_eq!(file_idx, 0, "→ file0's export");
    assert_eq!(def.range, nth(src0, "Red", 0), "→ the module's Red value");
}

#[test]
fn a_cross_file_module_at_the_head_takes_the_qualifier_from_the_enum_case() {
    // Probe M14: the identical shape with a same-file ENUM — FCS still binds
    // the cross-file module's value, so the (pre-existing) enum-case qualifier
    // emit stands down the same way. (The old "the enum type shadows a
    // same-named cross-file export" pin was about *value* exports; a module-
    // namespace owner at the head wins.)
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\ntype Color =\n    | Red = 0\n    | Blue = 1\nlet x = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Color.Red", 0))
        .expect("the cross-file module's value resolves");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("a cross-file item, never the same-file enum case");
    assert_eq!(file_idx, 0, "→ file0's export");
    assert_eq!(def.range, nth(src0, "Red", 0), "→ the module's Red value");
}

#[test]
fn an_opened_namespaces_module_at_the_head_takes_the_qualifier_from_the_member() {
    // Probe M15 (dotnet-build-verified, fcs-dump-pinned): `open A` supplies
    // `A.Color` (a module of the opened cross-file namespace) as the head, and
    // FCS binds `Color.Red` to the MODULE's value even though the same-file
    // type is declared *later* than the open — module-namespace priority for
    // the qualifier is position-independent (codex round-2 finding). The
    // member emit stands down on open-supplied completions too.
    let src0 = "namespace A\nmodule Color =\n    let Red = 99\n";
    let src1 =
        "module Client\nopen A\ntype Color() =\n    static member Red = 1\nlet x = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    match proj.file(1).resolution_at(nth(src1, "Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        Some(res) => {
            let (file_idx, def) = proj
                .item_def(res)
                .expect("if resolved, a cross-file item, never the same-file member");
            assert_eq!(file_idx, 0, "→ file0's export");
            assert_eq!(def.range, nth(src0, "Red", 0), "→ the opened module's Red");
        }
    }
}

#[test]
fn an_opened_namespaces_module_at_the_head_takes_the_qualifier_from_the_enum_case() {
    // Probe M16: the identical shape with a same-file enum — the pre-existing
    // enum-case qualifier emit stands down the same way.
    let src0 = "namespace A\nmodule Color =\n    let Red = 99\n";
    let src1 =
        "module Client\nopen A\ntype Color =\n    | Red = 0\n    | Blue = 1\nlet x = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    match proj.file(1).resolution_at(nth(src1, "Color.Red", 0)) {
        None | Some(Resolution::Deferred(_)) => {}
        Some(res) => {
            let (file_idx, def) = proj
                .item_def(res)
                .expect("if resolved, a cross-file item, never the same-file enum case");
            assert_eq!(file_idx, 0, "→ file0's export");
            assert_eq!(def.range, nth(src0, "Red", 0), "→ the opened module's Red");
        }
    }
}

#[test]
fn the_enclosing_modules_own_name_does_not_contest_the_qualifier() {
    // Probe M17 (dotnet-build-verified, fcs-dump-pinned; codex round 3):
    // within `module Color`, the module's own name is not in scope as a head
    // (the FS0039 own-name rule the candidate walk already applies), so it
    // cannot contest the qualifier — FCS resolves `Color.Red` to the
    // same-file enum case / static member, head → the TYPE.
    let enum_src = "module Color\ntype Color =\n    | Red = 0\n    | Blue = 1\nlet x = Color.Red\n";
    let rf = resolve(enum_src);
    assert_def_at(
        &rf,
        enum_src,
        nth(enum_src, "Color.Red", 0),
        nth(enum_src, "Red", 0),
    );
    assert_def_at(
        &rf,
        enum_src,
        nth(enum_src, "Color", 2),
        nth(enum_src, "Color", 1),
    );

    let member_src = "module Color\ntype Color() =\n    static member Red = 1\nlet x = Color.Red\n";
    let rf = resolve(member_src);
    assert_def_at(
        &rf,
        member_src,
        nth(member_src, "Color.Red", 0),
        nth(member_src, "Red", 0),
    );
}

#[test]
fn split_get_set_accessor_declarations_defer() {
    // A property written as SEPARATE accessor declarations shares one name
    // across two member defns; slice 1's repeated-name rule treats that as an
    // overload and withholds the emit. FCS resolves a read to the getter, so
    // this is a deliberate availability gap (sound — an honest defer, never a
    // wrong target); distinguishing split accessors from genuine overloads
    // needs kind-tracking in the member index (codex round 6, documented in
    // `docs/project-type-member-plan.md` §4).
    let src = "module Demo\ntype Color() =\n    static member Red with get () = 1\n    static member Red with set (_v: int) = ()\nlet x = Color.Red\n";
    let rf = resolve(src);
    assert_defers_at(&rf, src, nth(src, "Color.Red", 0));
}

#[test]
fn accessor_level_access_modifier_defers() {
    // Codex round-2 finding: `static member Red with private get() = 1` hides
    // the `private` inside the GET/SET accessor node (`dotnet fsi`: reading it
    // from outside is FS0491). Access rules are unmodeled, so any accessor
    // modifier keeps the property out of the emit set, like member-level ones.
    let src = "module Demo\ntype Color() =\n    static member Red with private get () = 1\nlet x = Color.Red\n";
    let rf = resolve(src);
    assert_defers_at(&rf, src, nth(src, "Color.Red", 0));
}

// ---- Project wiring sanity: same-file emit under resolve_project ----

#[test]
fn member_emit_works_under_resolve_project() {
    let src0 = "module Lib\nlet unrelated = 1\n";
    let src1 = "module Demo\ntype Color() =\n    static member Red = 1\nlet x = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    let res = rf
        .resolution_at(nth(src1, "Color.Red", 0))
        .expect("member resolves");
    let def = rf.resolved_def(res).expect("member def");
    assert_eq!(def.range, nth(src1, "Red", 0));
}

// ---- The definite-value head gate: FCS's unqualified slot is latest-wins ----
//
// FCS resolves a compound head `Color.Red` as member access on a value ONLY
// while the value is the LATEST entry in the unqualified-name slot
// (`ResolveExprLongIdentPrim`'s `ValIsInEnv` — total priority); a `type Color`
// declared later EVICTS the value from the slot, and then modules are searched
// FIRST, then type statics. Pinned by probes M20a–M20i (all dotnet-build-
// verified + fcs-dump-pinned 2026-07-03):
// - M20a/M20b: value, then a later type (class / enum) — a cross-file
//   `module Color / let Red` takes the head (the M20 mis-record shape).
// - M20c: value alone — member access on the value, modules never searched.
// - M20d: type first, value later — the value re-takes the slot.
// - M20e: evicted head + a contesting module WITHOUT the residual — FCS
//   backtracks to the type's static member; sema stands down (module contest)
//   so the head must DEFER, never re-bind the value.
// - M20f/M20g: a function-local / parameter binder is always latest; a type
//   declared after the use is not yet in scope — both keep member access.
// - M20h/M20i: an open-supplied project type enters the slot at the OPEN's
//   position — a later open evicts, an earlier one loses to the value.

#[test]
fn a_later_type_evicts_the_value_and_the_module_takes_the_head() {
    // Probe M20a: the later `type Color()` evicts the local value from the
    // slot, so `Color.Red` is NOT member access — module search runs first and
    // binds the cross-file module's value.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nlet Color = {| Red = 3 |}\ntype Color() =\n    static member Blue = 0\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    let res = rf
        .resolution_at(nth(src1, "Color.Red", 0))
        .expect("the cross-file module's value resolves");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("a cross-file item, never the local value");
    assert_eq!(file_idx, 0, "→ file0's export");
    assert_eq!(def.range, nth(src0, "Red", 0), "→ the module's Red value");
    // The head is a module qualifier (FCS: the module decl) — deferred, never
    // the evicted local value.
    assert_defers_at(rf, src1, nth(src1, "Color", 2));
}

#[test]
fn a_later_enum_evicts_the_value_and_the_module_takes_the_head() {
    // Probe M20b: the enum variant of M20a — any type kind evicts.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nlet Color = {| Red = 3 |}\ntype Color =\n    | Blue = 0\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    let res = rf
        .resolution_at(nth(src1, "Color.Red", 0))
        .expect("the cross-file module's value resolves");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("a cross-file item, never the local value or the enum");
    assert_eq!(file_idx, 0, "→ file0's export");
    assert_eq!(def.range, nth(src0, "Red", 0), "→ the module's Red value");
    assert_defers_at(rf, src1, nth(src1, "Color", 2));
}

#[test]
fn a_value_with_no_later_type_keeps_the_head() {
    // Probe M20c: the value is the slot's latest entry — member access wins
    // with total priority; the cross-file module is never searched.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nlet Color = {| Red = 3 |}\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_def_at(rf, src1, nth(src1, "Color", 1), nth(src1, "Color", 0));
    // The tail is member access on the value — unmodeled, honestly deferred.
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn a_value_later_than_the_type_retakes_the_head() {
    // Probe M20d: latest-wins in the other direction — a value AFTER the type
    // re-takes the slot (the M2c rule), so member access wins again.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\ntype Color() =\n    static member Blue = 0\nlet Color = {| Red = 3 |}\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_def_at(rf, src1, nth(src1, "Color", 2), nth(src1, "Color", 1));
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn an_evicted_head_with_a_residual_less_module_defers() {
    // Probe M20e: the type evicts the value AND has the member, but the
    // contesting cross-file module lacks `Red` — FCS backtracks to the TYPE's
    // static member. The member emit stands down on the module contest
    // (residual-blind, the M19 boundary), so sema's floor is an honest defer:
    // the head must never re-bind the evicted local value.
    let src0 = "module Color\nlet Blue = 99\n";
    let src1 = "module Client\nlet Color = {| Red = 3 |}\ntype Color() =\n    static member Red = 7\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
    assert_defers_at(rf, src1, nth(src1, "Color", 2));
}

#[test]
fn a_function_local_value_is_always_latest() {
    // Probe M20f: a function-local binder (and a parameter) enters the
    // environment after every module-level type — member access on the local.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\ntype Color() =\n    static member Blue = 0\nlet f () =\n    let Color = {| Red = 3 |}\n    Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_def_at(rf, src1, nth(src1, "Color", 2), nth(src1, "Color", 1));
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn a_type_after_the_use_does_not_evict() {
    // Probe M20g: the type is declared after the use, so it is not yet in
    // scope there — the local value holds the slot at the use.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nlet f () =\n    let Color = {| Red = 3 |}\n    Color.Red\ntype Color() =\n    static member Blue = 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_def_at(rf, src1, nth(src1, "Color", 1), nth(src1, "Color", 0));
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn an_open_supplied_type_evicts_at_the_opens_position() {
    // Probe M20h: `open LibNs` (declared AFTER the value) brings the project
    // type `LibNs.Color` into the slot at the open's position — the value is
    // evicted and the cross-file module takes the head.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "namespace LibNs\ntype Color() =\n    static member Blue = 0\n";
    let src2 = "module Client\nlet Color = {| Red = 3 |}\nopen LibNs\nlet user = Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(2);
    let res = rf
        .resolution_at(nth(src2, "Color.Red", 0))
        .expect("the cross-file module's value resolves");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("a cross-file item, never the local value");
    assert_eq!(file_idx, 0, "→ file0's export");
    assert_eq!(def.range, nth(src0, "Red", 0), "→ the module's Red value");
    assert_defers_at(rf, src2, nth(src2, "Color", 1));
}

#[test]
fn an_open_before_the_value_does_not_evict() {
    // Probe M20i: the same open declared BEFORE the value — the value is the
    // slot's latest entry and keeps member access (the open's type loses).
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "namespace LibNs\ntype Color() =\n    static member Blue = 0\n";
    let src2 = "module Client\nopen LibNs\nlet Color = {| Red = 3 |}\nlet user = Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(2);
    assert_def_at(rf, src2, nth(src2, "Color", 1), nth(src2, "Color", 0));
    assert_defers_at(rf, src2, nth(src2, "Color.Red", 0));
}

#[test]
fn an_evicted_opened_value_defers_behind_the_opaque_module_open() {
    // The opened value sits at its `open`'s position (M20p/M20q), so the
    // later written type EVICTS it — FCS binds the root module's `Color.Red`.
    // Sema cannot deliver that resolution (the project-module `open M` sets
    // `opaque_dotted_open`, barring the qualified block — the documented
    // pre-existing conservatism), so the head defers; it must never record
    // the evicted opened value.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module M\nlet Color = 1\n";
    let src2 =
        "module Client\nopen M\ntype Color() =\n    static member Blue = 0\nlet user = Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(2);
    assert_defers_at(rf, src2, nth(src2, "Color", 1));
    assert_defers_at(rf, src2, nth(src2, "Color.Red", 0));
}

#[test]
fn an_evicted_head_with_nothing_to_resolve_defers() {
    // Single file, no module anywhere: the eviction still bars member access
    // (FCS would search modules, then the type's members — `Blue`-only, so the
    // code is illegal), and with nothing to resolve the head honestly defers
    // instead of re-binding the evicted value.
    let src = "module Client\nlet Color = {| Red = 3 |}\ntype Color() =\n    static member Blue = 0\nlet user = Color.Red\n";
    let rf = resolve(src);
    assert_defers_at(&rf, src, nth(src, "Color", 2));
    assert_defers_at(&rf, src, nth(src, "Color.Red", 0));
}

#[test]
fn an_augmentation_after_the_value_does_not_evict() {
    // Probe M20j: a `type Color with …` AUGMENTATION after the value does NOT
    // re-enter the type in FCS's slot — the value (later than the type's
    // *definition*) keeps member access. The eviction comparison uses
    // definition positions only.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\ntype Color() =\n    static member Blue = 0\nlet Color = {| Red = 3 |}\ntype Color with\n    static member Green = 1\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_def_at(rf, src1, nth(src1, "Color", 3), nth(src1, "Color", 1));
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn a_later_union_does_not_evict_the_value() {
    // Probe M20k (codex round 1): a UNION never enters FCS's unqualified slot
    // under the type name (`AddPartsOfTyconRefToNameEnv` adds only types with
    // "potential use as a constructor" — class/struct/delegate; enums are
    // structs), so the value keeps member access even with the union later.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nlet Color = {| Red = 3 |}\ntype Color =\n    | Blue\n    | Green\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_def_at(rf, src1, nth(src1, "Color", 2), nth(src1, "Color", 0));
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn a_later_record_does_not_evict_the_value() {
    // Probe M20l: records likewise never enter the slot — member access holds.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nlet Color = {| Red = 3 |}\ntype Color = { Blue: int }\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_def_at(rf, src1, nth(src1, "Color", 2), nth(src1, "Color", 0));
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn a_later_struct_union_is_an_unordered_contest_and_defers() {
    // Probe M20m: a genuine `[<Struct>]` union IS a struct type and evicts in
    // FCS (the module binds `Color.Red`) — but the marker is matched
    // textually and a CUSTOM attribute named `Struct` would spoof it while
    // FCS keeps the type ordinary (codex round 7), so sema treats a
    // `Struct`-marked union/record as statically undecidable: defer, never a
    // wrong target in either direction.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nlet Color = {| Red = 3 |}\n[<Struct>]\ntype Color =\n    | Blue\n    | Green\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
    assert_defers_at(rf, src1, nth(src1, "Color", 2));
}

#[test]
fn a_later_abbreviation_is_an_unordered_contest_and_defers() {
    // Probe M20n: an abbreviation's slot entry chases its TARGET (`type Color
    // = int` is a struct → FCS evicts and binds the module's Red; a union
    // target would keep). Sema does not chase for the slot, so a later
    // abbreviation is statically undecidable — defer, never pick a side.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nlet Color = {| Red = 3 |}\ntype Color = int\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_defers_at(rf, src1, nth(src1, "Color", 2));
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn a_later_interface_does_not_evict_the_value() {
    // Probe M20o: an interface has no construction, so it never enters the
    // slot — member access on the value holds.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nlet Color = {| Red = 3 |}\ntype Color =\n    interface\n        abstract Member: int\n    end\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_def_at(rf, src1, nth(src1, "Color", 2), nth(src1, "Color", 0));
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn a_redefined_open_supplied_type_uses_the_latest_slot_class() {
    // Codex round 2 (P3): duplicate same-path types (FS0037-illegal, but live
    // mid-edit) push two `type_path_exports` entries; the slot lookup must
    // take the LATEST — matching `define_type` / `ProjectItems` last-wins —
    // so the stale earlier class's `Evicts` cannot re-route member access on
    // the value through the qualified block.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "namespace LibNs\n\ntype Color() =\n    static member Blue = 0\n\ntype Color =\n    | Blue2\n    | Green\n\nnamespace Client\n\nmodule Use =\n    let Color = {| Red = 3 |}\n    open LibNs\n    let user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    // The LATEST `LibNs.Color` is a plain union (Keeps) — the value holds the
    // slot and the head is member access, never the cross-file module.
    assert_def_at(rf, src1, nth(src1, "Color", 3), nth(src1, "Color", 2));
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn an_opened_value_after_the_type_retakes_the_slot() {
    // Probe M20p (codex round 3): a value brought into scope by an `open`
    // enters FCS's slot at the OPEN's position, not its definition's — an
    // `open M` after the type re-takes the slot for `M.Color`, so `Color.Red`
    // is member access on the opened value, never the cross-file module.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nmodule M =\n    let Color = {| Red = 3 |}\ntype Color() =\n    static member Blue = 0\nopen M\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_def_at(rf, src1, nth(src1, "Color", 2), nth(src1, "Color", 0));
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn an_opened_value_before_the_type_is_evicted() {
    // Probe M20q: the same open BEFORE the type — the type evicts the opened
    // value at the open's position, and FCS binds the cross-file module's
    // Red. Sema cannot deliver that resolution (the project-module `open M`
    // sets `opaque_dotted_open`, barring the qualified block — the documented
    // pre-existing conservatism), so both the head and the whole span defer;
    // the head must never record the evicted opened value.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nmodule M =\n    let Color = {| Red = 3 |}\nopen M\ntype Color() =\n    static member Blue = 0\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
    assert_defers_at(rf, src1, nth(src1, "Color", 2));
}

#[test]
fn a_private_type_behind_an_open_does_not_evict() {
    // Probe M20r (codex round 4): FCS does not import a `type private Color`
    // at an `open` from outside its declaration group, so the opened
    // namespace's private type never enters the slot — the local value keeps
    // member access.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "namespace LibNs\n\ntype private Color() =\n    static member Blue = 0\n";
    let src2 = "module Client\nlet Color = {| Red = 3 |}\nopen LibNs\nlet user = Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(2);
    assert_def_at(rf, src2, nth(src2, "Color", 1), nth(src2, "Color", 0));
    assert_defers_at(rf, src2, nth(src2, "Color.Red", 0));
}

#[test]
fn a_private_type_in_its_own_container_still_evicts() {
    // Probe M20s: within its own container a private type is fully visible
    // and enters the slot normally — the module takes the head.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nlet Color = {| Red = 3 |}\ntype private Color() =\n    static member Blue = 0\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    let res = rf
        .resolution_at(nth(src1, "Color.Red", 0))
        .expect("the cross-file module's value resolves");
    let (file_idx, def) = proj
        .item_def(res)
        .expect("a cross-file item, never the local value");
    assert_eq!(file_idx, 0, "→ file0's export");
    assert_eq!(def.range, nth(src0, "Red", 0), "→ the module's Red value");
    assert_defers_at(rf, src1, nth(src1, "Color", 2));
}

#[test]
fn an_evicted_head_never_takes_the_type_side_fallbacks() {
    // Probes M20t/M20u (codex round 5): for an EVICTED head, FCS's compound
    // search runs moduleSearch first — so a module binding is provable — but
    // the type-side fallbacks resolve through the tycon table, where the
    // evicting type itself is a NEARER candidate FCS tries first: when the
    // opened `LibNs.Color` owns `Red`, FCS binds ITS member (M20t), and only
    // when it lacks `Red` does FCS backtrack to the earlier file's root case
    // (M20u, codex-probed). Sema cannot prove which way it goes (the opened
    // type's members are not modeled through opens), so an evicted head must
    // defer rather than fall through to the cross-file type-case index.
    let src0 = "namespace global\n\ntype Color =\n    | Red\n    | Blue\n";
    // M20t: the opened evictor OWNS the residual — FCS binds its member.
    let src1 = "namespace LibNs\n\ntype Color() =\n    static member Red = 1\n\nnamespace global\n\nmodule Client =\n    let Color = {| Red = 3 |}\n    open LibNs\n    let user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
    assert_defers_at(rf, src1, nth(src1, "Color", 2));

    // M20u: the opened evictor LACKS the residual — FCS backtracks to the
    // root case; sema still defers (it cannot distinguish the two).
    let src2 = "namespace LibNs\n\ntype Color() =\n    static member Blue2 = 1\n\nnamespace global\n\nmodule Client =\n    let Color = {| Red = 3 |}\n    open LibNs\n    let user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src2)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_defers_at(rf, src2, nth(src2, "Color.Red", 0));
    assert_defers_at(rf, src2, nth(src2, "Color", 2));
}

#[test]
fn a_same_file_redefinition_shadows_the_preceding_slot_class() {
    // Codex round 6 (P3, the cross-file analogue of round 2): when the
    // current file redefines an opened type path an earlier file also
    // exports (FS0248-illegal, but live mid-edit), the SAME-FILE export
    // decides the slot — matching `ProjectItems::extend_with`'s later-file-
    // wins — so a stale earlier-file `Evicts` must not evict past the
    // current file's `Keeps` union.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "namespace LibNs\n\ntype Color() =\n    static member Blue = 0\n";
    let src2 = "namespace LibNs\n\ntype Color =\n    | Blue2\n    | Green\n\nnamespace global\n\nmodule Client =\n    let Color = {| Red = 3 |}\n    open LibNs\n    let user = Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(2);
    assert_def_at(rf, src2, nth(src2, "Color", 2), nth(src2, "Color", 1));
    assert_defers_at(rf, src2, nth(src2, "Color.Red", 0));
}

#[test]
fn a_module_open_supplied_class_evicts_the_value() {
    // Probe M20v (codex round 8): a project MODULE open imports types too —
    // the opened `M.Color` class evicts the earlier value at the open's
    // position, and FCS then binds the cross-file `module Color`'s value
    // (module search still precedes the opened type's member). Sema cannot
    // deliver that resolution (the module open's opaque flag bars the
    // qualified block), so both defer; the head must never record the
    // evicted local value.
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nmodule M =\n    type Color() =\n        static member Red = 1\nlet Color = {| Red = 3 |}\nopen M\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
    assert_defers_at(rf, src1, nth(src1, "Color", 2));
}

#[test]
fn a_module_open_supplied_union_keeps_the_value() {
    // Probe M20w: a UNION in the opened module never enters the slot — the
    // value keeps member access (the Keeps classification applies to
    // module-open-supplied types too).
    let src0 = "module Color\nlet Red = 99\n";
    let src1 = "module Client\nmodule M =\n    type Color =\n        | Blue\n        | Green\nlet Color = {| Red = 3 |}\nopen M\nlet user = Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);
    assert_def_at(rf, src1, nth(src1, "Color", 2), nth(src1, "Color", 1));
    assert_defers_at(rf, src1, nth(src1, "Color.Red", 0));
}

#[test]
fn a_same_open_value_and_type_tie_defers() {
    // Probe M20x (codex round 9): ONE `open Lib` inside `namespace N`
    // resolves to both the root `module Lib` (supplying `let Color`, the
    // lower reading) and the relative `N.Lib` namespace (supplying
    // `type Color`, the higher reading) — FCS binds the type's member
    // (`N.Lib.Color.Red`), the opened value loses the intra-open tie. Sema
    // orders both at the open's position and does not model reading
    // priority for the slot, so an equal-position contest defers — the head
    // must never record the opened value.
    let src0 = "module Lib\nlet Color = {| Red = 3 |}\n";
    let src1 = "namespace N.Lib\n\ntype Color() =\n    static member Red = 1\n";
    let src2 = "namespace N\n\nmodule Client =\n    open Lib\n    let user = Color.Red\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let rf = proj.file(2);
    assert_defers_at(rf, src2, nth(src2, "Color.Red", 0));
    assert_defers_at(rf, src2, nth(src2, "Color", 0));
}

#[test]
fn a_same_file_private_types_member_is_inaccessible_from_a_sibling() {
    // `type private Foo() = static member Red` in `module A`; a SIBLING `module B`
    // references `A.Foo.Red` same-file. FCS reports FS1092 (the private type is
    // inaccessible from a sibling). The same-file module-qualified member emit was
    // accessibility-blind (a wrong target on `main`).
    let src = "module Lib\n\nmodule A =\n    type private Foo() =\n        static member Red = 1\n\nmodule B =\n    let y = A.Foo.Red\n";
    let rf = resolve(src);
    assert_defers_at(&rf, src, nth(src, "A.Foo.Red", 0));
}
