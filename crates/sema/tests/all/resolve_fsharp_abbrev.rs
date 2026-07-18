//! FCS-free regression tests for F# assembly type abbreviations in type-position
//! lookup. Plain abbreviations are present in F# signature data but not as ECMA
//! TypeDefs; the projection surfaces each public one as a name-only
//! `EntityKind::Abbreviation` marker entity, and the resolver shadow-defers a
//! lookup that lands on a marker (never resolving through it). When the pickle
//! cannot be decoded at all, a coarse per-namespace fallback defers every bare
//! name under the assembly's namespaces instead.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_oracle_harness::BoundedCommand;
use borzoi_sema::{
    AssemblyEnv, DeferredReason, ProjectItems, Resolution, ResolvedFile, resolve_file,
};
use rowan::TextRange;

/// Budget for one fixture `dotnet build`. A cold build restores packages and runs
/// the F# compiler, which is legitimately minutes, so the bound sits far above the
/// harness's per-request default: it is there to stop a build that has *stalled* —
/// blocked on a NuGet lock held by a concurrent run in a sibling worktree, say —
/// from hanging the suite forever, not to police a slow one.
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

/// `dotnet build -c Release` a fixture project under [`BUILD_TIMEOUT`], failing
/// loudly (with the build's own output) if it errors or never finishes.
fn dotnet_build(project: &Path, what: &str) {
    let mut cmd = Command::new("dotnet");
    cmd.args(["build", "-c", "Release", "--nologo"])
        .arg(project);
    BoundedCommand::new(cmd).timeout(BUILD_TIMEOUT).run_ok(what);
}

fn ensure_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fsharp_abbrev_env");
            dotnet_build(&project, "dotnet build F# abbreviation fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaFSharpAbbrevFixture.dll")
        })
        .as_path()
}

fn fixture_env() -> AssemblyEnv {
    let bytes = std::fs::read(ensure_fixture_built()).expect("read F# abbreviation fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse F# abbreviation fixture dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv")
}

/// The main fixture referenced **twice** — two loaded DLLs exporting the same
/// FQNs, so every top-level name collides across DLLs (including alias *targets*,
/// which then also defer via target-uniqueness). A coarse multi-DLL behavioural
/// pin; [`collision_env`] is the precise cross-DLL-rooting-collision test whose
/// *target* stays unique.
fn fixture_env_doubled() -> AssemblyEnv {
    let bytes = std::fs::read(ensure_fixture_built()).expect("read F# abbreviation fixture dll");
    let v1 = Ecma335Assembly::parse(&bytes).expect("parse F# abbreviation fixture dll");
    let v2 = Ecma335Assembly::parse(&bytes).expect("parse F# abbreviation fixture dll");
    AssemblyEnv::from_views(&[v1, v2]).expect("build AssemblyEnv")
}

fn ensure_collision_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/fsharp_abbrev_collision_env");
            dotnet_build(&project, "dotnet build F# abbreviation collision fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaFSharpAbbrevCollisionFixture.dll")
        })
        .as_path()
}

/// The main fixture (which exports `Lib.WidgetAlias` as an abbreviation whose
/// target `Lib.Widget` it *alone* declares) referenced FIRST, plus a second DLL
/// exporting `Lib.WidgetAlias` as a real class — so `Lib.WidgetAlias` collides
/// across DLLs while the alias's target stays unique. This isolates the
/// rooting-FQN-collision guard: without it, resolve-through would chase the
/// main fixture's unique target and commit `Widget.Make` (codex P1).
fn collision_env() -> AssemblyEnv {
    let main = std::fs::read(ensure_fixture_built()).expect("read main fixture dll");
    let collision = std::fs::read(ensure_collision_fixture_built()).expect("read collision dll");
    let main = Ecma335Assembly::parse(&main).expect("parse main fixture dll");
    let collision = Ecma335Assembly::parse(&collision).expect("parse collision fixture dll");
    AssemblyEnv::from_views(&[main, collision]).expect("build AssemblyEnv")
}

/// A *separate* fixture for the ROOT (`namespace global`) tier: its
/// signature-data flag applies to the empty namespace, which — unlike every
/// other namespace check here — is not name-scoped in `fsharp_abbrev_env`'s
/// assembly, so sharing one assembly would make every bare name in every
/// other test here defer via the root tier too.
fn ensure_root_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/fsharp_abbrev_root_env");
            dotnet_build(&project, "dotnet build F# root-abbreviation fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaFSharpAbbrevRootFixture.dll")
        })
        .as_path()
}

fn root_fixture_env() -> AssemblyEnv {
    let bytes =
        std::fs::read(ensure_root_fixture_built()).expect("read F# root-abbreviation fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse F# root-abbreviation fixture dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv")
}

fn resolve(src: &str, env: &AssemblyEnv) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), env)
}

fn at(hay: &str, needle: &str) -> TextRange {
    let start = hay
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {hay:?}"));
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + needle.len()).unwrap().into(),
    )
}

fn assert_shadowable(src: &str) {
    let env = fixture_env();
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "int64")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "Lib.int64 may be a metadata-invisible F# abbreviation"
    );
}

#[test]
fn opened_fsharp_assembly_namespace_marks_annotation_shadowable() {
    assert_shadowable("module M\nopen Lib\nlet x : int64 = \"\"\n");
}

#[test]
fn enclosing_fsharp_assembly_namespace_marks_annotation_shadowable() {
    assert_shadowable("namespace Lib\nmodule M =\n    let x : int64 = \"\"\n");
}

#[test]
fn real_type_in_signature_data_namespace_still_resolves() {
    // Regression pin (codex review P2 on `docs/completed/r2-annotation-typing-plan.md`):
    // `Lib` carries F# signature data (because of the `int64` abbreviation), but
    // it also declares a perfectly ordinary ECMA TypeDef, `Marker`. The V3 defer
    // must only kick in once the normal tiered lookup has failed to find a real
    // match — checking it *before* that lookup made every single-segment type
    // name under `open Lib` defer, including `Marker`, which used to resolve
    // (losing go-to-definition for ordinary types from any opened F# library).
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet x : Marker = Unchecked.defaultof<_>\n";
    let rf = resolve(src, &env);
    let marker = env
        .lookup_type(&["Lib".into()], "Marker", 0)
        .expect("fixture must declare Lib.Marker");
    assert_eq!(
        rf.resolution_at(at(src, "Marker")),
        Some(Resolution::Entity(marker)),
        "Marker is a real TypeDef and must resolve, not defer"
    );
}

#[test]
fn resolve_through_a_same_assembly_abbreviation_binds_the_member_tail() {
    // `type WidgetAlias = Widget` aliases a same-assembly type (so `Widget` is
    // loaded in the env, unlike the `string`/`int` aliases). The `Make` static
    // must resolve THROUGH the alias to a member on `Widget` — where the plain
    // marker defer would have left the whole path unresolved. `WidgetAlias` itself
    // binds to the marker (FCS points the alias name at the abbreviation).
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet _ = WidgetAlias.Make()\n";
    let rf = resolve(src, &env);

    let marker = env
        .lookup_type(&["Lib".into()], "WidgetAlias", 0)
        .expect("fixture must declare Lib.WidgetAlias");
    assert_eq!(
        rf.resolution_at(at(src, "WidgetAlias")),
        Some(Resolution::Entity(marker)),
        "the alias segment binds to the abbreviation marker",
    );
    assert!(
        matches!(
            rf.resolution_at(at(src, "WidgetAlias.Make")),
            Some(Resolution::Member { .. })
        ),
        "`Make` must resolve through the alias to a member on `Widget`; got {:?}",
        rf.resolution_at(at(src, "WidgetAlias.Make")),
    );
}

#[test]
fn bare_alias_use_defers_rather_than_naming_a_target() {
    // A *bare* alias use with no member tail — `Lib.WidgetAlias`, the alias as the
    // terminal segment — defers. Resolve-through chases the target to walk a
    // member *tail* (`Lib.WidgetAlias.Make`, the sibling test); a bare use FCS
    // resolves by the target's value/constructor surface, which we do not model. A
    // constructible class points at the terminal type, but `type UAlias = U` where
    // `U` is a union without a constructor errors FS1133 with *no* symbol use — we
    // cannot tell those apart here, so we defer (own-and-defer) rather than commit
    // either the marker or a possibly-erroneous target (codex review). Both a class
    // alias and a union alias must therefore defer, never resolve.
    let env = fixture_env();
    for src in [
        "module M\nlet _ = Lib.WidgetAlias()\n",
        "module M\nlet _ = Lib.UAlias\n",
    ] {
        let rf = resolve(src, &env);
        let alias = if src.contains("WidgetAlias") {
            "WidgetAlias"
        } else {
            "UAlias"
        };
        assert_eq!(
            rf.resolution_at(at(src, alias)),
            Some(Resolution::Deferred(DeferredReason::QualifiedAccess)),
            "a bare alias use must defer, not name a target; got {:?} for {alias}",
            rf.resolution_at(at(src, alias)),
        );
    }
}

#[test]
fn cross_dll_collision_at_an_alias_fqn_defers_resolve_through() {
    // P1 #1 — the alias's own FQN merges across DLLs, target still unique. The
    // main fixture exports `Lib.WidgetAlias` as an abbreviation (→ its unique
    // `Lib.Widget`); a second DLL exports `Lib.WidgetAlias` as a real class. FCS
    // applies reference-order precedence sema does not model, so resolve-through
    // would chase the main fixture's unique target and commit `Widget.Make` —
    // whereas single-DLL the SAME access resolves (the sibling test), so the
    // rooting-collision guard, not a general failure, is what defers here.
    let env = collision_env();
    let src = "module M\nopen Lib\nlet _ = WidgetAlias.Make()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "WidgetAlias.Make")),
            Some(Resolution::Member { .. })
        ),
        "a resolve-through at a cross-DLL-colliding alias FQN must defer; got {:?}",
        rf.resolution_at(at(src, "WidgetAlias.Make")),
    );
}

#[test]
fn arity_overloaded_alias_still_resolves_through() {
    // `type AliasO = Widget` beside a generic `type AliasO<'T>` in ONE DLL: the
    // cross-DLL-collision guard counts distinct DLLs at arity 0, so the nullary
    // alias is unique and `AliasO.Make` resolves through to `Widget.Make` — an
    // arity-agnostic same-name count would wrongly over-defer it (codex round 9).
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet _ = AliasO.Make()\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "AliasO.Make")),
            Some(Resolution::Member { .. })
        ),
        "a nullary alias beside a generic same-named type must still resolve through; got {:?}",
        rf.resolution_at(at(src, "AliasO.Make")),
    );
}

#[test]
fn cross_dll_merged_parent_defers_nested_resolve_through() {
    // P1 #2 — a nested alias below a parent module whose FQN merges across DLLs.
    // The main fixture referenced twice merges `Lib.Nested`, so `children(parent)`
    // sees only one contributor; the rooting-collision guard (the parent FQN
    // collides) defers rather than commit one contributor's `Widget.Make`.
    let env = fixture_env_doubled();
    let src = "module M\nlet _ = Lib.Nested.NestedAlias.Make()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "Lib.Nested.NestedAlias.Make")),
            Some(Resolution::Member { .. })
        ),
        "a resolve-through below a cross-DLL-merged parent must defer; got {:?}",
        rf.resolution_at(at(src, "Lib.Nested.NestedAlias.Make")),
    );
}

#[test]
fn member_access_through_an_alias_with_a_companion_module_defers() {
    // `type WidgetC = Widget` with a `[<ModuleSuffix>] module WidgetC` that also
    // defines `Make` (codex round 6): FCS routes `WidgetC.Make` to the *companion
    // module's* `Make`, not the target `Widget`'s static — a module-over-target
    // member precedence we do not model. The resolve-through must DEFER, never
    // commit `Widget.Make` (verified against fcs-dump: `WidgetC.Make` resolves to
    // `WidgetCModule.Make`).
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet _ = WidgetC.Make()\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "WidgetC.Make")),
            Some(Resolution::Member { .. })
        ),
        "a member access through an alias with a companion module must defer, not \
         commit the target's member; got {:?}",
        rf.resolution_at(at(src, "WidgetC.Make")),
    );
}

#[test]
fn nested_terminal_alias_defers_but_a_qualifier_through_it_resolves() {
    // The nested-descent counterpart of the bare/qualifier split (codex round 5):
    // `Lib.Nested.NestedAlias` (a nested alias as the terminal segment, no tail) is
    // a bare use and must DEFER exactly like a top-level bare alias, while a
    // qualifier through it — `Lib.Nested.NestedAlias.Make` — still resolves the
    // `Make` static on the chased `Widget` target.
    let env = fixture_env();

    let bare = "module M\nlet _ = Lib.Nested.NestedAlias\n";
    let rf = resolve(bare, &env);
    assert_eq!(
        rf.resolution_at(at(bare, "NestedAlias")),
        Some(Resolution::Deferred(DeferredReason::QualifiedAccess)),
        "a terminal nested alias (bare) must defer, not name a target; got {:?}",
        rf.resolution_at(at(bare, "NestedAlias")),
    );

    let qual = "module M\nlet _ = Lib.Nested.NestedAlias.Make()\n";
    let rf = resolve(qual, &env);
    assert!(
        matches!(
            rf.resolution_at(at(qual, "Lib.Nested.NestedAlias.Make")),
            Some(Resolution::Member { .. })
        ),
        "a qualifier through a nested alias still resolves the member; got {:?}",
        rf.resolution_at(at(qual, "Lib.Nested.NestedAlias.Make")),
    );
}

#[test]
fn resolve_through_an_alias_owns_the_path_over_a_lower_reading() {
    // `open Lib.Lower` brings a `UAlias` class with a real static `UCase`; `open
    // Lib` (later, so it wins the `UAlias` binding) brings `UAlias = U`, a union
    // alias. `UAlias.UCase` must resolve THROUGH the later alias — the union case
    // lives in `union_case_names`, not the `members` surface the tail walk
    // searches — and OWN the path, never ceding to `Lower.UAlias.UCase`. Absence
    // from the target's member surface is not proof of absence (codex round 4:
    // resolve-through must not let a lower reading win on a non-member surface).
    let env = fixture_env();
    let src = "module M\nopen Lib.Lower\nopen Lib\nlet _ = UAlias.UCase\n";
    let rf = resolve(src, &env);
    assert!(
        !matches!(
            rf.resolution_at(at(src, "UAlias.UCase")),
            Some(Resolution::Member { .. })
        ),
        "the aliased tail must own/defer, not cede to the lower reading's static \
         member; got {:?}",
        rf.resolution_at(at(src, "UAlias.UCase")),
    );
}

#[test]
fn root_namespace_with_signature_data_marks_annotation_shadowable_with_no_open() {
    // Regression pin (codex review P2, round 4, on
    // `docs/completed/r2-annotation-typing-plan.md`): the fixture declares `namespace
    // global; type uint64 = string` — a genuine F# abbreviation with an empty
    // namespace path. FCS lets a bare, unopened name bind to a global-namespace
    // abbreviation, so the ROOT tier (the empty prefix `resolve_type_path` also
    // walks with no `open` in scope) needs the same shadow check as every
    // opened/enclosing reading — a guard that skipped the empty prefix would
    // wrongly resolve `uint64` as the primitive alias.
    let env = root_fixture_env();
    let src = "module M\nlet x : uint64 = \"\"\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "uint64")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "global.uint64 may be a metadata-invisible F# abbreviation, with no open needed"
    );
}

#[test]
fn root_namespace_real_type_still_resolves_with_no_open() {
    // The round-2/round-3 counterpart at the ROOT tier: `GlobalMarker` is a
    // real TypeDef at `namespace global`, so it must resolve — not defer —
    // even though the same (empty) namespace carries signature data.
    let env = root_fixture_env();
    let src = "module M\nlet x : GlobalMarker = Unchecked.defaultof<_>\n";
    let rf = resolve(src, &env);
    let marker = env
        .lookup_type(&[], "GlobalMarker", 0)
        .expect("fixture must declare the global-namespace GlobalMarker");
    assert_eq!(
        rf.resolution_at(at(src, "GlobalMarker")),
        Some(Resolution::Entity(marker)),
        "GlobalMarker is a real TypeDef and must resolve, not defer"
    );
}

#[test]
fn ancestor_namespace_of_signature_data_is_not_marked_shadowable() {
    // Regression pin (codex review P2 on `docs/completed/r2-annotation-typing-plan.md`):
    // the fixture assembly declares a real TypeDef at `Other.Deep` but nothing
    // directly in `Other`. F# `open N` imports only `N`'s direct members, so an
    // abbreviation that could only live in `Other.Deep`'s signature data is never
    // in scope from `open Other` — marking `Other` shadowable on `Other.Deep`'s
    // evidence (the old ancestor-prefix-expansion bug) would wrongly defer this.
    let env = fixture_env();
    let src = "module M\nopen Other\nlet x : int64 = 1L\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "int64")),
        None,
        "Other has no direct signature data, so int64 is not shadowed by it"
    );
}

#[test]
fn bare_names_with_no_abbreviation_do_not_defer() {
    // The name-keyed refinement over the original coarse per-namespace flag:
    // `Lib` genuinely exports abbreviations (`int64`, `Collide`), but none
    // named `uint64` — the pickled signature data says so exactly. A coarse
    // "Lib carries signature data" signal deferred EVERY bare annotation under
    // `open Lib`; the abbreviation markers synthesised from the pickle defer
    // only the names that actually collide, so `uint64` keeps its "no shadow
    // possible" reading (the signal the R2 alias gate needs to ever fire for
    // projects that reference any F# library).
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet x : uint64 = 1UL\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "uint64")),
        None,
        "Lib's signature data has no `uint64` abbreviation, so nothing shadows it"
    );
}

#[test]
fn auto_open_abbreviation_shadows_a_same_tier_direct_type() {
    // Review-confirmed (reproduced end-to-end against real fsc): `Lib`
    // declares `Collide` twice — a direct record TypeDef, and an abbreviation
    // inside the `[<AutoOpen>] module Auto`. fsc binds `Lib.Auto.Collide`
    // (= string): an auto-open module's contents outrank the same namespace's
    // own direct members even at the same tier. The abbreviation emits no
    // TypeDef, so the precise auto-open veto can only see it through a
    // pickle-synthesised marker child of `Auto`; without one, the tier's own
    // lookup resolves the direct record — a wrong target, not a sound defer.
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet f (x : Collide) = x\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Collide")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "the auto-open `Auto.Collide` abbreviation must shadow the direct `Lib.Collide`"
    );
}

#[test]
fn private_abbreviation_does_not_shadow() {
    // `Lib.Hidden` is `type private Hidden = string`: not nameable from
    // another assembly, so `open Lib; (x : Hidden)` cannot bind it and the
    // annotation must keep its no-shadow reading. Pins the marker synthesis'
    // accessibility filter (a pickled entity with a non-empty `TAccess` path
    // list is not public).
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet x : Hidden = Unchecked.defaultof<_>\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Hidden")),
        None,
        "a private abbreviation is invisible cross-assembly and must not shadow"
    );
}

#[test]
fn unknowable_abbreviations_fall_back_to_coarse_namespace_defers() {
    // The fallback channel: when an assembly's signature pickle cannot be
    // decoded (or it embeds foreign CCU pickles), its abbreviations are
    // unknowable — no markers exist — so the resolver must defer EVERY bare
    // name under the namespaces the assembly declares into, name-blind, as
    // the pre-marker coarse signal did. `uint64` names no abbreviation in the
    // fixture, so this deferring proves the coarse channel (contrast
    // `bare_names_with_no_abbreviation_do_not_defer`, which pins that the
    // same lookup does NOT defer when the pickle decoded).
    use borzoi_assembly::EcmaView;
    use borzoi_sema::AbbreviationVisibility;
    let bytes = std::fs::read(ensure_fixture_built()).expect("read F# abbreviation fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse F# abbreviation fixture dll");
    let entities = view.enumerate_type_defs().expect("enumerate fixture types");
    let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        PathBuf::from("SemaFSharpAbbrevFixture.dll"),
        entities,
        AbbreviationVisibility::Unknowable,
        Vec::new(),
    )]);
    let src = "module M\nopen Lib\nlet x : uint64 = 1UL\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "uint64")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "an unknowable assembly's namespaces defer every bare annotation under them"
    );
}

#[test]
fn open_type_of_an_abbreviation_marker_goes_opaque() {
    // codex review (marker PR): `open type Lib.int64` (where `Lib.int64` is a
    // metadata-invisible abbreviation of `string`) opens the TARGET's statics
    // in FCS. We cannot enumerate them from a name-only marker, so the open
    // must go opaque — suppressing earlier opens' same-named values — rather
    // than pushing an empty statics set that would let `Opened.openedValue`
    // keep winning where FCS might bind a target static of the same name.
    let env = fixture_env();
    let src = "module M\nmodule Opened =\n    let openedValue = 1\nopen Opened\nopen type Lib.int64\nlet y = openedValue\n";
    let rf = resolve(src, &env);
    let use_start = src.rfind("openedValue").expect("use site");
    let range = TextRange::new(
        u32::try_from(use_start).unwrap().into(),
        u32::try_from(use_start + "openedValue".len())
            .unwrap()
            .into(),
    );
    assert_eq!(
        rf.resolution_at(range),
        Some(Resolution::Deferred(DeferredReason::UnboundName)),
        "the opened value must defer past an opaque `open type` of a marker \
         (without the opaque routing it wrongly resolves the opened Item)"
    );
}

#[test]
fn plain_open_of_a_marker_with_a_module_companion_binds_the_module_value() {
    // codex review round 2 (marker PR): `Lib.Companion` is BOTH an
    // abbreviation (`type Companion = string` — a marker, which wins the
    // source-name index slot) and a suffixed module companion
    // (`module Companion`, compiled `CompanionModule`). A plain
    // `open Lib.Companion` opens the MODULE's values in FCS, so its
    // `fromCompanion` shadows the earlier `open Other`'s same-named value.
    //
    // The companion module is enumerable, so we bind its `fromCompanion`
    // exactly as FCS does — the precise, latest-open-wins target. (Previously
    // the enumerable check compared the type-preferring `opened_assembly_type`
    // handle — the abbreviation marker — against `opened_assembly_module`; they
    // differ at a collision, so the open was wrongly deemed opaque and this
    // deferred. The guard now asks whether a module interpretation *exists*, the
    // §5a fix; see `docs/assembly-module-open-plan.md`.) The load-bearing
    // property is unchanged: the marker-backed open does NOT leak `Other`'s
    // value — it binds the companion module's own.
    let env = fixture_env();
    let src = "module M\nmodule Other =\n    let fromCompanion = 99\nopen Other\nopen Lib.Companion\nlet y = fromCompanion\n";
    let rf = resolve(src, &env);
    let use_start = src.rfind("fromCompanion").expect("use site");
    let range = TextRange::new(
        u32::try_from(use_start).unwrap().into(),
        u32::try_from(use_start + "fromCompanion".len())
            .unwrap()
            .into(),
    );
    match rf.resolution_at(range) {
        Some(Resolution::Member { parent, .. }) => assert_eq!(
            env.entity(parent).name,
            "CompanionModule",
            "the marker-backed open must bind the companion module's own \
             `fromCompanion`, not leak `Other`'s"
        ),
        other => panic!("expected the companion module's `fromCompanion` Member, got {other:?}"),
    }
}

/// Review round 13 (§5a of `docs/assembly-module-open-plan.md`), now **delivered**.
/// The sibling test above pins the shadowing half: `open Lib.Companion` binds the
/// companion module's `fromCompanion` over an earlier open's. This pins the bare
/// half — FCS **resolves** `fromCompanion` to the companion module's own value.
///
/// `Lib.Companion` is both an abbreviation (which wins the type-index slot) and a suffixed
/// companion module. `opened_assembly_type` returns the type-index winner while
/// `opened_assembly_module` returns the module, so the guard's old `h == handle` identity
/// test failed, the abbreviation branch raised `opaque_value_open`, and the name deferred —
/// even though the fold can enumerate that module perfectly well. The guard now asks whether
/// the path *has* a module interpretation (`opened_assembly_module(&path).is_some()`), the
/// exact condition `open_interpretations` uses to emit the `AssemblyModule` tier.
#[test]
fn an_opened_companion_module_behind_a_type_collision_still_resolves() {
    let env = fixture_env();
    let src = "module M\nopen Lib.Companion\nlet y = fromCompanion\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "fromCompanion")),
            Some(Resolution::Member { .. })
        ),
        "FCS opens the MODULE half of `Lib.Companion` and binds its `fromCompanion`; the \
         abbreviation winning the type-index slot must not hide it — got {:?}",
        rf.resolution_at(at(src, "fromCompanion"))
    );
}

#[test]
fn module_companion_does_not_suppress_the_abbreviation_marker() {
    // codex round 4: the suffixed module companion (`module Companion`,
    // compiled `CompanionModule`, source name `Companion`) must not count as
    // "an ECMA row already occupies the abbreviation's slot" — a module never
    // occupies the TYPE-position name. Without the marker, the type index
    // hands `Companion` to the module and a bare annotation binds a module
    // entity where FCS binds the abbreviation (= string).
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet x : Companion = \"\"\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Companion")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "the abbreviation marker must shadow the type position, not the module companion"
    );
}

#[test]
fn renamed_abbreviation_marker_outranks_its_module_companion() {
    // codex round 5: `[<CompiledName("RenamedAbbrev")>] type Renamed = string`
    // gives the marker a source_name, which routes it through the same
    // source-named index pass as the suffixed `module Renamed` companion. The
    // type must still win the bare name (F#'s type-over-module slot rule):
    // the annotation defers on the abbreviation marker rather than binding
    // the module entity.
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet x : Renamed = \"\"\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Renamed")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "the renamed abbreviation's marker must win the bare name over the module companion"
    );
}

#[test]
fn nested_renamed_abbreviation_marker_outranks_its_module_companion() {
    // codex round 6: the round-5 rule, one level down. `Lib.Holder` nests a
    // renamed abbreviation (`NestedRenamed`, compiled `NestedRenamedAbbrev` —
    // so its marker carries a source_name) and a suffixed module companion.
    // `AssemblyEnv::nested`'s source-name tier must prefer the TYPE (the
    // marker, which shadow-defers the whole path — a multi-segment path
    // records nothing) over the module in any child storage order; matching
    // the module instead records a module entity in type position where FCS
    // binds the abbreviation.
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet x : Holder.NestedRenamed = \"\"\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "NestedRenamed")),
        None,
        "the nested marker must shadow-defer the path (recording nothing at a \
         multi-segment tail), never bind the module companion in type position"
    );
}

#[test]
fn rec_module_multi_segment_forward_path_defers() {
    // Review finding #3 (probe-confirmed): inside `module rec`, a
    // multi-segment annotation can name a nested module declared LATER —
    // `Deep.Marker` binds the forward `M.Deep.Marker` in FCS. The
    // source-ordered walk has not seen `module Deep` yet, so the
    // descends-into-nested-module veto misses and the tiered walk bound the
    // assembly `Other.Deep.Marker` instead — a wrong target. The rec
    // pre-scan of the block's module names must defer the path (recording
    // nothing — a multi-segment tail is never a primitive-alias head).
    let env = fixture_env();
    let src = "module rec M\nopen Other\nlet f (x : Deep.Marker) = x\nmodule Deep =\n    type Marker = A of int\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Marker")),
        None,
        "a rec-forward module path must not bind the same-path assembly type"
    );
}

#[test]
fn non_rec_later_module_does_not_veto_the_assembly_path() {
    // The non-rec control: without `rec`, the later `module Deep` is NOT in
    // scope at the annotation, so FCS genuinely binds the assembly
    // `Other.Deep.Marker` — the pre-scan must key on `rec` and leave this
    // resolving.
    let env = fixture_env();
    let src = "module M\nopen Other\nlet f (x : Deep.Marker) = x\nmodule Deep =\n    type Marker = A of int\n";
    let rf = resolve(src, &env);
    let marker = env
        .lookup_type(&["Other".into(), "Deep".into()], "Marker", 0)
        .expect("fixture must declare Other.Deep.Marker");
    assert_eq!(
        rf.resolution_at(at(src, "Marker")),
        Some(Resolution::Entity(marker)),
        "without rec the assembly path is the true binding"
    );
}

// ==== Stage 4 of `docs/abbreviation-target-projection-plan.md`: resolving
// *through* a marker via its decoded `abbreviation_target`. The marker itself
// is what the name binds (FCS reports the abbreviation entity at the use), so
// a chase-able marker records `Resolution::Entity(marker)`; the chase's
// *terminal* only steers what a path may do PAST the abbreviation (nested
// types, static members). A target we cannot chase — undeclared assembly,
// structural shape, `None` — keeps the pre-chase shadow-defer exactly.

fn env_with_bcl() -> AssemblyEnv {
    let fixture = std::fs::read(ensure_fixture_built()).expect("read F# abbreviation fixture dll");
    let bcl =
        std::fs::read(crate::common::ensure_system_runtime_dll()).expect("read System.Runtime.dll");
    let views = vec![
        Ecma335Assembly::parse(&fixture).expect("parse F# abbreviation fixture dll"),
        Ecma335Assembly::parse(&bcl).expect("parse System.Runtime.dll"),
    ];
    AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
}

#[test]
fn same_assembly_abbreviation_resolves_to_its_marker() {
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet f (x : MarkerAlias) = x\n";
    let rf = resolve(src, &env);
    let marker = env
        .lookup_type(&["Lib".into()], "MarkerAlias", 0)
        .expect("fixture must surface the MarkerAlias marker");
    assert!(
        env.is_abbreviation(marker),
        "MarkerAlias must be a pickle-synthesised marker"
    );
    assert_eq!(
        rf.resolution_at(at(src, "MarkerAlias")),
        Some(Resolution::Entity(marker)),
        "a marker whose target chases to a same-assembly TypeDef must resolve, not defer"
    );
}

#[test]
fn abbreviation_chain_resolves_through_two_markers() {
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet f (x : MarkerAliasAlias) = x\n";
    let rf = resolve(src, &env);
    let marker = env
        .lookup_type(&["Lib".into()], "MarkerAliasAlias", 0)
        .expect("fixture must surface the MarkerAliasAlias marker");
    assert_eq!(
        rf.resolution_at(at(src, "MarkerAliasAlias")),
        Some(Resolution::Entity(marker)),
        "a marker → marker → TypeDef chain must chase to the terminal and resolve"
    );
}

#[test]
fn generic_abbreviation_resolves_at_its_arity() {
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet f (x : GenAlias<int>) = x\n";
    let rf = resolve(src, &env);
    let marker = env
        .lookup_type(&["Lib".into()], "GenAlias", 1)
        .expect("fixture must surface the arity-1 GenAlias marker");
    assert_eq!(
        rf.resolution_at(at(src, "GenAlias")),
        Some(Resolution::Entity(marker)),
        "a generic marker (the `option` shape) must resolve at its own arity"
    );
}

#[test]
fn qualified_abbreviation_path_resolves_at_the_tail() {
    let env = fixture_env();
    let src = "module M\nlet f (x : Lib.MarkerAlias) = x\n";
    let rf = resolve(src, &env);
    let marker = env
        .lookup_type(&["Lib".into()], "MarkerAlias", 0)
        .expect("fixture must surface the MarkerAlias marker");
    assert_eq!(
        rf.resolution_at(at(src, "MarkerAlias")),
        Some(Resolution::Entity(marker)),
        "a fully-qualified path ending at a chase-able marker must resolve its tail"
    );
}

#[test]
fn bcl_target_without_the_target_assembly_still_defers() {
    // `Str = System.String`, but the fixture-only env has no assembly named
    // `System.Runtime`: the chase must decline and the marker keep its
    // shadow-defer (D5 — a chase that cannot finish never resolves).
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet x : Str = Unchecked.defaultof<_>\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Str")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "an unloadable target assembly must keep the marker deferring"
    );
}

#[test]
fn bcl_target_resolves_with_the_target_assembly_loaded() {
    let env = env_with_bcl();
    let src = "module M\nopen Lib\nlet x : Str = Unchecked.defaultof<_>\n";
    let rf = resolve(src, &env);
    let marker = env
        .lookup_type(&["Lib".into()], "Str", 0)
        .expect("fixture must surface the Str marker");
    assert_eq!(
        rf.resolution_at(at(src, "Str")),
        Some(Resolution::Entity(marker)),
        "a cross-assembly BCL target must chase once System.Runtime is loaded"
    );
}

#[test]
fn static_member_tail_through_an_abbreviation_resolves() {
    // The plan's §2 row 1: `S.Format` where `type S = System.String` resolves
    // the member tail on the TARGET. `Empty` is a (non-overloaded) static
    // field, so the tail commits a `Member` whose parent is the terminal.
    let env = env_with_bcl();
    let src = "module M\nopen Lib\nlet y = Str.Empty\n";
    let rf = resolve(src, &env);
    let string_entity = env
        .lookup_type(&["System".into()], "String", 0)
        .expect("System.Runtime must declare System.String");
    let use_start = src.find("Str.Empty").expect("use site");
    let whole = TextRange::new(
        u32::try_from(use_start).unwrap().into(),
        u32::try_from(use_start + "Str.Empty".len()).unwrap().into(),
    );
    match rf.resolution_at(whole) {
        Some(Resolution::Member { parent, .. }) => assert_eq!(
            parent, string_entity,
            "the static tail must resolve on the chased terminal (System.String)"
        ),
        other => panic!("expected a Member on System.String, got {other:?}"),
    }
}

#[test]
fn open_type_through_an_abbreviation_segment_is_modelled() {
    // codex review (this slice): the `open type` path may pass THROUGH an
    // abbreviation at a non-final segment — `open type Lib.Env.SpecialFolder`
    // where `type Env = System.Environment`. FCS chases `Env` and opens the
    // real nested enum's cases; a walk that descends on the marker's (empty)
    // nested types goes opaque instead, wrongly suppressing earlier opens'
    // values. Pin the non-opacity: `openedValue` (shadowed by nothing the
    // enum brings in) must keep resolving.
    let env = env_with_bcl();
    let src = "module M\nmodule Opened =\n    let openedValue = 1\nopen Opened\nopen type Lib.Env.SpecialFolder\nlet y = openedValue\n";
    let rf = resolve(src, &env);
    let use_start = src.rfind("openedValue").expect("use site");
    let range = TextRange::new(
        u32::try_from(use_start).unwrap().into(),
        u32::try_from(use_start + "openedValue".len())
            .unwrap()
            .into(),
    );
    assert!(
        matches!(rf.resolution_at(range), Some(Resolution::Item(_))),
        "an open-type path through a chase-able abbreviation segment must be \
         modelled, not opaque — `openedValue` binds the project module's own \
         item — got {:?}",
        rf.resolution_at(range)
    );
}

#[test]
fn chase_terminal_is_never_a_marker() {
    // The chase's own contract, pinned over every marker the fixture
    // surfaces: whatever `resolve_abbreviation_tycon` returns is a real (non-marker)
    // entity — a chain never stops half-way — and a decline is `None`, never
    // a partial hop.
    let env = env_with_bcl();
    for (ns, name, arity) in [
        ("Lib", "MarkerAlias", 0),
        ("Lib", "MarkerAliasAlias", 0),
        ("Lib", "GenAlias", 1),
        ("Lib", "Str", 0),
        ("Lib", "int64", 0),
        ("Lib", "Companion", 0),
    ] {
        let marker = env
            .lookup_type(&[ns.into()], name, arity)
            .unwrap_or_else(|| panic!("fixture must surface {ns}.{name}"));
        if !env.is_abbreviation(marker) {
            continue;
        }
        if let Some(terminal) = env.resolve_abbreviation_tycon(marker) {
            assert!(
                !env.is_abbreviation(terminal),
                "chase({ns}.{name}) stopped on a marker"
            );
        }
    }
}

#[test]
fn nested_rec_module_forward_path_defers_too() {
    // The nested `module rec Outer = …` entry point (a fresh rec block inside
    // a non-rec file) must pre-scan its own nested-module names exactly like
    // a top-level `module rec` header.
    let env = fixture_env();
    let src = "module M\nopen Other\nmodule rec Outer =\n    let f (x : Deep.Marker) = x\n    module Deep =\n        type Marker = A of int\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Marker")),
        None,
        "a nested rec block's forward module path must not bind the assembly type"
    );
}

// ==== Cross-DLL collision guards on the chase (codex round 3) ====
//
// FCS applies reference-order precedence when two loaded DLLs export the same
// public FQN, and sema does not model reference order — so a chase that starts
// at (or below) a colliding rooting must defer, never resolve out of the
// first-indexed DLL's subtree.

/// A minimal synthetic entity for the hand-built two-DLL envs below.
fn synth_entity(
    assembly: &str,
    ns: &[&str],
    name: &str,
    kind: borzoi_assembly::EntityKind,
) -> borzoi_assembly::Entity {
    use borzoi_assembly::{Access, AssemblyIdentity, Entity, Version};
    Entity {
        assembly: AssemblyIdentity {
            name: assembly.to_string(),
            version: Version {
                major: 1,
                minor: 0,
                build: 0,
                revision: 0,
            },
            public_key_token: None,
        },
        namespace: ns.iter().map(|s| (*s).to_string()).collect(),
        name: name.to_string(),
        kind,
        access: Access::Public,
        is_sealed: false,
        generic_parameters: vec![],
        base_type: None,
        interfaces: vec![],
        members: vec![],
        skipped_members: vec![],
        method_def_tokens: vec![],
        nested_types: vec![],
        is_readonly: false,
        is_byref_like: false,
        is_struct: false,
        is_auto_open: false,
        is_require_qualified_access: false,
        is_no_equality: false,
        is_no_comparison: false,
        is_structural_equality: false,
        is_structural_comparison: false,
        is_allow_null_literal: false,
        obsolete: None,
        experimental: None,
        default_member: None,
        compiler_feature_required: vec![],
        source_name: None,
        extension_member_names: vec![],
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        custom_attrs: vec![],
        abbreviation_target: None,
    }
}

fn synth_marker(
    assembly: &str,
    ns: &[&str],
    name: &str,
    target_path: &[&str],
) -> borzoi_assembly::Entity {
    use borzoi_assembly::{AbbreviationTarget, EntityKind};
    let mut e = synth_entity(assembly, ns, name, EntityKind::Abbreviation);
    e.abbreviation_target = Some(AbbreviationTarget::Named {
        ccu: None,
        path: target_path.iter().map(|s| (*s).to_string()).collect(),
        args: Vec::new(),
    });
    e
}

fn two_dll_env(a: Vec<borzoi_assembly::Entity>, b: Vec<borzoi_assembly::Entity>) -> AssemblyEnv {
    use borzoi_sema::AbbreviationVisibility;
    AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
        (
            PathBuf::from("A.dll"),
            a,
            AbbreviationVisibility::Modelled,
            Vec::new(),
        ),
        (
            PathBuf::from("B.dll"),
            b,
            AbbreviationVisibility::Modelled,
            Vec::new(),
        ),
    ])
}

#[test]
fn nested_alias_below_a_cross_dll_colliding_root_defers_in_type_position() {
    // Both DLLs export a public top-level `N.Container`; the first nests
    // `type Alias = Widget` (a marker with a Local target). FCS merges the
    // containers by reference order, so `children` of the first-indexed root
    // may miss the other DLL's contribution — the type-position walk must
    // defer the nested-alias chase (recording nothing at a multi-segment
    // tail), exactly as the value-path walk already does.
    use borzoi_assembly::EntityKind;
    let widget = synth_entity("A", &["N"], "Widget", EntityKind::Class);
    let mut container_a = synth_entity("A", &["N"], "Container", EntityKind::Module);
    container_a.nested_types = vec![{
        let mut m = synth_marker("A", &[], "Alias", &["N", "Widget"]);
        m.namespace = Vec::new();
        m
    }];
    let container_b = synth_entity("B", &["N"], "Container", EntityKind::Module);
    let env = two_dll_env(vec![widget, container_a], vec![container_b]);
    let src = "module M\nlet f (x : N.Container.Alias) = x\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Alias")),
        None,
        "a nested alias below a cross-DLL-colliding root must defer, not chase \
         out of the first-indexed subtree"
    );
}

#[test]
fn open_type_of_a_cross_dll_colliding_alias_goes_opaque() {
    // Both DLLs export a public top-level `Lib.S` — a chase-able alias in
    // one, a real class in the other. FCS binds by reference order, so
    // `open type Lib.S` must go opaque (suppressing earlier opens' values)
    // rather than open the first-indexed DLL's pick.
    use borzoi_assembly::EntityKind;
    let widget = synth_entity("A", &["Lib"], "Widget", EntityKind::Class);
    let alias = synth_marker("A", &["Lib"], "S", &["Lib", "Widget"]);
    let s_class = synth_entity("B", &["Lib"], "S", EntityKind::Class);
    let env = two_dll_env(vec![widget, alias], vec![s_class]);
    let src = "module M\nmodule Opened =\n    let openedValue = 1\nopen Opened\nopen type Lib.S\nlet y = openedValue\n";
    let rf = resolve(src, &env);
    let use_start = src.rfind("openedValue").expect("use site");
    let range = TextRange::new(
        u32::try_from(use_start).unwrap().into(),
        u32::try_from(use_start + "openedValue".len())
            .unwrap()
            .into(),
    );
    assert_eq!(
        rf.resolution_at(range),
        Some(Resolution::Deferred(DeferredReason::UnboundName)),
        "an `open type` of a cross-DLL-colliding alias must go opaque, not \
         open the first-indexed target's statics"
    );
}

#[test]
fn absent_child_past_a_chased_alias_defers_instead_of_ceding() {
    // codex round 5: once a type path roots through a *resolved* alias, FCS
    // owns the reading — `AliasNs.Alias.Inner` where the alias's target has
    // no `Inner` (genuinely, or because the projection dropped it) must NOT
    // cede ownership and let a lower-priority open's same-named
    // `Alias.Inner` bind. Mirrors the value-path `via_alias` rule main's
    // Stage 4a established.
    use borzoi_assembly::EntityKind;
    let widget = synth_entity("A", &["AliasNs"], "Widget", EntityKind::Class);
    let alias = synth_marker("A", &["AliasNs"], "Alias", &["AliasNs", "Widget"]);
    let inner = {
        let mut e = synth_entity("B", &[], "Inner", EntityKind::Class);
        e.namespace = Vec::new();
        e
    };
    let mut other_alias = synth_entity("B", &["OtherNs"], "Alias", EntityKind::Class);
    other_alias.nested_types = vec![inner];
    let env = two_dll_env(vec![widget, alias], vec![other_alias]);
    // `open AliasNs` is the LATER (higher-priority) open: its alias reading
    // owns the path even though its target lacks `Inner`.
    let src = "module M\nopen OtherNs\nopen AliasNs\nlet f (x : Alias.Inner) = x\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Inner")),
        None,
        "an absent child past a chased alias must defer the path, not fall \
         through to the lower open's same-named type"
    );
}
