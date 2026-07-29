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
use rowan::{TextRange, TextSize};

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

/// The main fixture plus the retained-manifest-auto-open one — the env in
/// which `Cases.Union.Target` (enclosing namespace) contends with
/// `Cases.Retained.Auto.Target` (manifest surface).
fn case_pattern_autoopen_env() -> AssemblyEnv {
    let main = std::fs::read(ensure_fixture_built()).expect("read main fixture dll");
    let auto = std::fs::read(crate::common::ensure_case_pattern_autoopen_fixture_built())
        .expect("read auto-open fixture dll");
    let main = Ecma335Assembly::parse(&main).expect("parse main fixture dll");
    let auto = Ecma335Assembly::parse(&auto).expect("parse auto-open fixture dll");
    AssemblyEnv::from_views(&[main, auto]).expect("build AssemblyEnv")
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

/// [`at`]'s last-occurrence twin, for a source that also **declares** the name
/// it probes: `find` would hand back the declaration's range instead of the
/// use's.
fn at_last(hay: &str, needle: &str) -> TextRange {
    let start = hay
        .rfind(needle)
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
fn member_access_through_an_alias_with_a_companion_module_binds_the_module() {
    // `type WidgetC = Widget` with a `[<ModuleSuffix>] module WidgetC` that also
    // defines `Make` (codex round 6): FCS routes `WidgetC.Make` to the *companion
    // module's* `Make`, not the target `Widget`'s static (verified against
    // fcs-dump: `WidgetC.Make` resolves to `WidgetCModule.Make`).
    //
    // That is the module-over-target precedence, and the rooting walk now models
    // it directly: one `(namespace, name)` names a candidate *set*, tried
    // module-first, and the module owns the path here — so the alias's own
    // reading is never reached and there is nothing to resolve *through*.
    let env = fixture_env();
    let src = "module M\nopen Lib\nlet _ = WidgetC.Make()\n";
    let rf = resolve(src, &env);
    let resolution = rf.resolution_at(at(src, "WidgetC.Make"));
    let Some(Resolution::Member { parent, idx }) = resolution else {
        panic!("`WidgetC.Make` must bind the companion module's `Make`; got {resolution:?}");
    };
    assert!(
        env.is_module(parent),
        "`WidgetC.Make` must bind the companion MODULE's member, not the alias target's; \
         got {} (module={})",
        env.entity_full_name(parent),
        env.is_module(parent),
    );
    assert_eq!(env.member_display_name(parent, idx), "Make");
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
        definition_range: None,
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
    // Both DLLs export a public top-level `N.Container`; only the first nests
    // `type Alias = Widget` (a marker with a Local target).
    //
    // Same-named modules from different assemblies genuinely **merge** — they
    // do not shadow. fsi-verified 2026-07-25 with two probe libraries each
    // exporting `namespace N; module Container`: `N.Container.onlyInA`,
    // `N.Container.onlyInB` and the *earlier* reference's nested
    // `N.Container.AliasA` all resolve. So FCS binds `Alias` here, and this is
    // a **known coverage gap**, not a modelling claim: a genuine cross-DLL
    // collision commits nothing, because committing on a sole supplier needs
    // our "supplies the path" to agree with FCS's exactly, and it does not
    // (see `resolve_assembly::one_contestant_supplying_the_tail_defers_rather_than_binds`).
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
fn a_nested_alias_supplied_by_both_colliding_roots_defers() {
    // The companion control: when BOTH merged containers nest an `Alias`, FCS
    // binds the latest *accessible* reference's, which we do not model — so
    // this one must still defer rather than chase the first-indexed subtree.
    use borzoi_assembly::EntityKind;
    let widget = synth_entity("A", &["N"], "Widget", EntityKind::Class);
    let alias = || {
        let mut m = synth_marker("A", &[], "Alias", &["N", "Widget"]);
        m.namespace = Vec::new();
        m
    };
    let mut container_a = synth_entity("A", &["N"], "Container", EntityKind::Module);
    container_a.nested_types = vec![alias()];
    let mut container_b = synth_entity("B", &["N"], "Container", EntityKind::Module);
    container_b.nested_types = vec![alias()];
    let env = two_dll_env(vec![widget, container_a], vec![container_b]);
    let src = "module M\nlet f (x : N.Container.Alias) = x\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Alias")),
        None,
        "two suppliers at a merged root are undecidable for us — defer"
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

/// The head of a `Type.Case` PATTERN whose type is a referenced-assembly union
/// resolves to that union's `Entity`, and — for a field-carrying case, which
/// compiles to a nested IL type — the whole `Type.Case` span resolves to the
/// case's nested `Entity`. This is the pattern-position sibling of how the
/// value path resolves `SynAccess.Internal` as an expression: before this, the
/// qualified case-pattern path consulted only in-file / cross-project tables
/// and recorded *nothing* for an assembly union, surfacing as
/// "No definition available".
#[test]
fn qualified_case_pattern_resolves_into_an_assembly_union() {
    let env = fixture_env();
    let src = "namespace Consumer\n\
               open Demo.CasePat\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | Shape.Circle r -> r\n\
               \x20       | Shape.Dot -> 0\n";
    let rf = resolve(src, &env);

    let shape = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape union in env");
    let circle = env
        .nested(shape, "Circle", 0)
        .expect("Shape.Circle compiles to a nested case type");

    // Head `Shape` (first occurrence, in `Shape.Circle`) → the union entity.
    assert_eq!(
        rf.resolution_at(at(src, "Shape")),
        Some(Resolution::Entity(shape)),
        "the qualified case-pattern head must resolve to the assembly union"
    );
    // Whole `Shape.Circle` span → the field-carrying case's nested type.
    assert_eq!(
        rf.resolution_at(at(src, "Shape.Circle")),
        Some(Resolution::Entity(circle)),
        "a field-carrying case tail resolves to its nested IL type"
    );
}

/// A *nullary* case has no nested IL type (it is a singleton), so the case tail
/// defers — but the head still resolves to the union, exactly as an opened
/// assembly case is a known case *reference* with an opaque target.
#[test]
fn qualified_nullary_case_pattern_resolves_head_defers_tail() {
    let env = fixture_env();
    let src = "namespace Consumer\n\
               open Demo.CasePat\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | Shape.Dot -> 0\n\
               \x20       | _ -> 1\n";
    let rf = resolve(src, &env);

    let shape = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape union in env");
    let whole = at(src, "Shape.Dot");
    let head = TextRange::new(whole.start(), whole.start() + TextSize::from(5u32));

    assert_eq!(
        rf.resolution_at(head),
        Some(Resolution::Entity(shape)),
        "the nullary case-pattern head still resolves to the assembly union"
    );
    assert_eq!(
        rf.resolution_at(whole),
        Some(Resolution::Deferred(DeferredReason::QualifiedAccess)),
        "a nullary case has no nested type, so the case tail defers"
    );
}

/// The motivating shape (`SynType.Var` under `open Fantomas.FCS.Syntax` +
/// `open WoofWare.Whippet.Fantomas`, which also declares a `SynType` module): a
/// later-opened MODULE shares the union's source name, shadowing it in the
/// value/module namespace. A pattern head is a type/constructor lookup, so the
/// resolver must see PAST the module and root the union.
#[test]
fn qualified_case_pattern_sees_past_a_shadowing_module() {
    let env = fixture_env();
    // `Demo.CasePat.Later` (the module) is opened LAST, so it wins the
    // value/module namespace — but the pattern must still find the union.
    let src = "namespace Consumer\n\
               open Demo.CasePat\n\
               open Demo.CasePat.Later\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | ShadowedUnion.Shaded n -> n\n\
               \x20       | _ -> 0\n";
    let rf = resolve(src, &env);

    let union = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "ShadowedUnion", 0)
        .expect("Demo.CasePat.ShadowedUnion union in env");
    let shaded = env
        .nested(union, "Shaded", 0)
        .expect("ShadowedUnion.Shaded nested case type");

    assert_eq!(
        rf.resolution_at(at(src, "ShadowedUnion")),
        Some(Resolution::Entity(union)),
        "the case-pattern head must root the union, not the shadowing module"
    );
    assert_eq!(
        rf.resolution_at(at(src, "ShadowedUnion.Shaded")),
        Some(Resolution::Entity(shaded)),
        "the case tail resolves to the union case, not through the module"
    );
}

/// A qualified case pattern writes no generic arguments, so a *generic* union
/// (`GenericShape<'T>`) must resolve arity-agnostically — an arity-0 lookup
/// would exclude every generic DU (`Result`, `Choice`, …).
#[test]
fn qualified_generic_union_case_pattern_resolves() {
    let env = fixture_env();
    let src = "namespace Consumer\n\
               open Demo.CasePat\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | GenericShape.GenericCircle v -> v\n\
               \x20       | _ -> 0\n";
    let rf = resolve(src, &env);
    let generic = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "GenericShape", 1)
        .expect("Demo.CasePat.GenericShape<'T> union in env");
    assert_eq!(
        rf.resolution_at(at(src, "GenericShape")),
        Some(Resolution::Entity(generic)),
        "a generic union head must resolve despite writing no type arguments"
    );
}

/// A project type / abbreviation of the same simple name shadows any assembly
/// union in the constructor namespace — FCS chases the project abbreviation to
/// its target's cases, which this branch does not model, so it must DECLINE
/// (record nothing) rather than root the assembly union (a wrong target).
#[test]
fn qualified_case_pattern_declines_when_a_project_type_shadows() {
    let env = fixture_env();
    let src = "namespace Consumer\n\
               open Demo.CasePat\n\
               module M =\n\
               \x20   type Shape = int\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | Shape.Circle r -> r\n\
               \x20       | _ -> 0\n";
    let rf = resolve(src, &env);
    let whole = at(src, "Shape.Circle");
    let head = TextRange::new(whole.start(), whole.start() + TextSize::from(5u32));
    assert_eq!(
        rf.resolution_at(head),
        None,
        "a project type of the same name must suppress the assembly union reading"
    );
    assert_eq!(rf.resolution_at(whole), None);
}

/// An assembly **abbreviation** aliasing a union (`Lib.UAlias = Lib.U`, and `U`
/// owns `UCase`) binds the head, but this branch does not chase the alias to its
/// target's cases — so it DECLINES rather than skip the abbreviation and commit
/// an unrelated lower-tier reading.
#[test]
fn qualified_case_pattern_declines_through_an_assembly_abbreviation() {
    let env = fixture_env();
    let src = "namespace Consumer\n\
               open Lib\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | UAlias.UCase -> 0\n\
               \x20       | _ -> 1\n";
    let rf = resolve(src, &env);
    let whole = at(src, "UAlias.UCase");
    let head = TextRange::new(whole.start(), whole.start() + TextSize::from(6u32));
    assert_eq!(
        rf.resolution_at(head),
        None,
        "an unchased assembly abbreviation head must decline, not mis-root"
    );
    assert_eq!(rf.resolution_at(whole), None);
}

/// A cross-DLL name collision at the rooting tier: FCS merges same-FQN roots by
/// reference order, which sema does not model — so a case pattern whose union
/// FQN is exported by more than one loaded DLL DECLINES.
#[test]
fn qualified_case_pattern_declines_on_cross_dll_collision() {
    let env = fixture_env_doubled();
    let src = "namespace Consumer\n\
               open Demo.CasePat\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | Shape.Circle r -> r\n\
               \x20       | _ -> 0\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Shape")),
        None,
        "a union FQN exported by two DLLs must decline (unmodelled merge order)"
    );
}

/// The same collision, between two loaded DLLs that share a manifest
/// **identity** (same name, version and public key) but export different things
/// at the name: the main fixture's `Cases.Union.Target` union against the
/// duplicate-identity fixture's `Cases.Union.Target` *module*. A module is
/// transparent to a constructor-namespace head lookup, so nothing here trips the
/// "two unions own the case" check — only per-DLL **provenance** sees the
/// contest. Keyed on the identity instead, the pair reads as one DLL and the
/// union is committed regardless of reference order, where FCS may bind the
/// module (codex review).
#[test]
fn qualified_case_pattern_declines_on_same_identity_dll_collision() {
    let main = std::fs::read(ensure_fixture_built()).expect("read main fixture dll");
    let dup = std::fs::read(crate::common::ensure_case_pattern_dup_fixture_built())
        .expect("read duplicate-identity fixture dll");
    let main = Ecma335Assembly::parse(&main).expect("parse main fixture dll");
    let dup = Ecma335Assembly::parse(&dup).expect("parse duplicate-identity fixture dll");
    let env = AssemblyEnv::from_views(&[main, dup]).expect("build AssemblyEnv");

    let src = "namespace Consumer\n\
               open Cases.Union\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | Target.Carrier r -> r\n\
               \x20       | _ -> 0\n";
    let rf = resolve(src, &env);
    let whole = at(src, "Target.Carrier");
    let head = TextRange::new(whole.start(), whole.start() + TextSize::from(6u32));
    assert_eq!(
        rf.resolution_at(head),
        None,
        "a union contested by a same-identity DLL's module must decline"
    );
    assert_eq!(rf.resolution_at(whole), None);
}

/// The case-pattern twin of `resolve_autoopen`'s dropped-type tests. The
/// retained-surface branch walks the tiers above the manifest surface, so the
/// same two incompleteness arms must stop it committing: a **dropped type in
/// the winning prefix** (namespace-scoped), and an **unknowable extension
/// surface** (global — no prefix is safe at all). Both deferral-only.
#[test]
fn a_retained_surface_walk_declines_on_projection_uncertainty() {
    let src = "namespace Cases.Union\n\
               module M =\n\
               \x20   let f (x: Cases.Union.Target) =\n\
               \x20       match x with\n\
               \x20       | Target.Carrier r -> r\n\
               \x20       | _ -> 0\n";
    let whole = at(src, "Target.Carrier");
    let head = TextRange::new(whole.start(), whole.start() + TextSize::from(6u32));

    // Control: with the surface fully modelled the enclosing namespace commits.
    // Pinned to the entity, since a `Deferred` is a resolution too and would
    // make the three declines below vacuous.
    let env = case_pattern_autoopen_env();
    let target = env
        .lookup_type(&["Cases".into(), "Union".into()], "Target", 0)
        .expect("Cases.Union.Target in the fixture env");
    assert_eq!(
        resolve(src, &env).resolution_at(head),
        Some(Resolution::Entity(target)),
        "control: the enclosing namespace outranks the manifest surface"
    );

    // A dropped type in the winning prefix (`Cases.Union`) — the surviving
    // `Target` may have a same-named dropped sibling FCS binds instead.
    let mut env = case_pattern_autoopen_env();
    env.mark_namespace_dropped_type(vec!["Cases".into(), "Retained".into()]);
    env.mark_namespace_dropped_type(vec!["Cases".into(), "Union".into()]);
    assert_eq!(resolve(src, &env).resolution_at(head), None);

    // The global arm: nowhere is safe, so decline outright.
    let mut env = case_pattern_autoopen_env();
    env.mark_extension_surface_unknowable();
    assert_eq!(resolve(src, &env).resolution_at(head), None);
}

/// A union imported through an `open` of an assembly **module** (not a
/// namespace) is a documented completeness gap: `assembly_prefixes_by_priority`
/// carries only namespace readings, and the module open sets
/// `opaque_value_open`, so `record_qualified_case_pattern` returns before the
/// assembly branch runs. The point of this test is that the gap is a **sound
/// decline** (records nothing), never a wrong target — resolving assembly-module
/// opens here is a follow-up.
#[test]
fn module_opened_union_case_pattern_declines_soundly() {
    let env = fixture_env();
    let src = "namespace Consumer\n\
               open Demo.MOpen.UnionMod\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | MUnion.MCaseA -> 0\n\
               \x20       | _ -> 1\n";
    let rf = resolve(src, &env);
    let whole = at(src, "MUnion.MCaseA");
    let head = TextRange::new(whole.start(), whole.start() + TextSize::from(6u32));
    assert_eq!(rf.resolution_at(head), None);
    assert_eq!(rf.resolution_at(whole), None);
}

/// The **enclosing-namespace** tier of the shared walk: no `open` at all, the
/// file's own `namespace Demo.CasePat` supplies the prefix.
#[test]
fn qualified_case_pattern_resolves_at_the_enclosing_namespace() {
    let env = fixture_env();
    let src = "namespace Demo.CasePat\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | Shape.Circle r -> r\n\
               \x20       | _ -> 0\n";
    let rf = resolve(src, &env);
    let shape = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape union in env");
    let circle = env
        .nested(shape, "Circle", 0)
        .expect("Shape.Circle compiles to a nested case type");
    assert_eq!(
        rf.resolution_at(at(src, "Shape")),
        Some(Resolution::Entity(shape)),
        "the enclosing namespace is a reading tier of its own"
    );
    assert_eq!(
        rf.resolution_at(at(src, "Shape.Circle")),
        Some(Resolution::Entity(circle))
    );
}

/// An `[<AutoOpen>]` module in the opened namespace declaring a type of the
/// head's name out-ranks the namespace's own direct members (FCS-probed), and
/// its nested content is not in the direct bucket — so the tier's
/// `ShadowVeto::Preemptive` vetoes even the real same-tier union. Decline.
#[test]
fn qualified_case_pattern_declines_under_an_auto_open_type_shadow() {
    let env = fixture_env();
    let src = "namespace Consumer\n\
               open Demo.CasePatAuto\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | Hidden.HiddenA n -> n\n\
               \x20       | _ -> 0\n";
    let rf = resolve(src, &env);
    let whole = at(src, "Hidden.HiddenA");
    let head = TextRange::new(whole.start(), whole.start() + TextSize::from(6u32));
    assert_eq!(
        rf.resolution_at(head),
        None,
        "`Demo.CasePatAuto.Auto.Hidden` out-ranks the direct `Hidden` — decline"
    );
    assert_eq!(rf.resolution_at(whole), None);
}

/// The cross-tier interleave. A project union `Consumer.Shape` in an earlier
/// file is reachable at the *enclosing-namespace* tier, and an assembly union
/// `Demo.CasePat.Shape` owning the same case name is reachable through a later
/// `open` — which out-ranks it, so FCS binds the assembly case. Sema cannot
/// commit the assembly reading (a project type of the head's simple name is in
/// scope, and FCS would chase a project abbreviation of that name to its
/// target's cases), so the site DECLINES. Before the interleave the project
/// reading was committed unconditionally: a wrong target, not a missed one.
#[test]
fn assembly_open_outranks_a_project_case_at_the_enclosing_namespace() {
    let env = fixture_env();
    let earlier = "namespace Consumer\n\
                   type Shape =\n\
                   \x20   | Circle of int\n\
                   \x20   | Square\n";
    let later = "namespace Consumer\n\
                 open Demo.CasePat\n\
                 module M =\n\
                 \x20   let f x =\n\
                 \x20       match x with\n\
                 \x20       | Shape.Circle r -> r\n\
                 \x20       | _ -> 0\n";
    let files: Vec<_> = [earlier, later]
        .iter()
        .map(|src| {
            let parsed = parse(src);
            assert!(parsed.errors.is_empty(), "parse errors in {src:?}");
            ImplFile::cast(parsed.root).expect("impl file")
        })
        .collect();
    let contended = borzoi_sema::resolve_project(&files, &env);
    let whole = at(later, "Shape.Circle");
    let head = TextRange::new(whole.start(), whole.start() + TextSize::from(5u32));
    assert_eq!(
        contended.file(1).resolution_at(head),
        None,
        "the assembly union's `open` out-ranks the project union's namespace"
    );
    assert_eq!(contended.file(1).resolution_at(whole), None);

    // Controls: the decline is caused by the *contending assembly union*, not by
    // some unrelated effect of having an `open` at all. With no open, and with
    // an open of a namespace that supplies no `Shape`, the project case binds.
    for control in [
        later.replace("open Demo.CasePat\n", ""),
        later.replace("open Demo.CasePat\n", "open Demo.CasePat.Later\n"),
    ] {
        let files: Vec<_> = [earlier, control.as_str()]
            .iter()
            .map(|src| {
                let parsed = parse(src);
                assert!(parsed.errors.is_empty(), "parse errors in {src:?}");
                ImplFile::cast(parsed.root).expect("impl file")
            })
            .collect();
        let uncontended = borzoi_sema::resolve_project(&files, &env);
        let whole = at(&control, "Shape.Circle");
        assert!(
            matches!(
                uncontended.file(1).resolution_at(whole),
                Some(Resolution::Item(_))
            ),
            "uncontended, the project case binds ({control:?}): {:?}",
            uncontended.file(1).resolution_at(whole)
        );
    }
}

/// A **dropped TypeDef** in the reading the walk commits at makes that reading's
/// type set incomplete, and the missing name is unknowable: the drop record
/// keeps only the namespace, so the dropped type may *be* a same-named type FCS
/// binds instead — another DLL's same-FQN half at a reference order we do not
/// model, or a same-name/other-arity sibling in this one. Committing the
/// survivor is then a wrong target, not a missed one, so a dropped type in a
/// visited reading must veto the whole walk (D5).
///
/// This is the type-position counterpart of the *value* path's long-established
/// treatment of the same marker (`a_lone_module_under_a_dropped_type_names_no_
/// definite_target` and neighbours in `resolve_assembly.rs`), which the type
/// path did not share.
#[test]
fn a_dropped_type_in_the_enclosing_namespace_defers_a_type_annotation() {
    let src = "namespace Demo.CasePat\n\
               module M =\n\
               \x20   let f (x: Shape) = x\n";

    // Control: with a complete projection the enclosing-namespace tier commits.
    let clean = fixture_env();
    let shape = clean
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");
    assert_eq!(
        resolve(src, &clean).resolution_at(at(src, "Shape")),
        Some(Resolution::Entity(shape)),
        "control: a clean `Demo.CasePat` commits the annotation"
    );

    let mut dropped = fixture_env();
    dropped.mark_namespace_dropped_type(vec!["Demo".into(), "CasePat".into()]);
    assert!(
        !matches!(
            resolve(src, &dropped).resolution_at(at(src, "Shape")),
            Some(Resolution::Entity(_))
        ),
        "a dropped TypeDef in `Demo.CasePat` may itself be the `Shape` FCS binds; \
         the surviving one is not a safe target — got {:?}",
        resolve(src, &dropped).resolution_at(at(src, "Shape"))
    );
}

/// The same hazard at the **opened** tier, which a *different* guard owns: the
/// open-side fold goes opaque on an `open` whose path carries a dropped split
/// (`names_uncovered_dropped_path` and the fold's `path_dropped` residue,
/// `resolve/decls.rs`), raising `unmodelled_open_active`, which
/// `decide_type_path` defers on before the walk starts. So this pins the
/// *boundary* of `dropped_type_could_root_this_path`'s job — the opened tier is
/// somebody else's — and fails if that older guard regresses.
#[test]
fn a_dropped_type_in_an_opened_namespace_defers_a_type_annotation() {
    let src = "namespace Consumer\n\
               open Demo.CasePat\n\
               module M =\n\
               \x20   let f (x: Shape) = x\n";

    let clean = fixture_env();
    let shape = clean
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");
    assert_eq!(
        resolve(src, &clean).resolution_at(at(src, "Shape")),
        Some(Resolution::Entity(shape)),
        "control: a clean opened namespace commits the annotation"
    );

    let mut dropped = fixture_env();
    dropped.mark_namespace_dropped_type(vec!["Demo".into(), "CasePat".into()]);
    assert!(
        !matches!(
            resolve(src, &dropped).resolution_at(at(src, "Shape")),
            Some(Resolution::Entity(_))
        ),
        "an opened namespace with a dropped TypeDef names no definite type — got {:?}",
        resolve(src, &dropped).resolution_at(at(src, "Shape"))
    );
}

/// The qualified-union-case-pattern walk shares the type path's per-tier verdict,
/// so it inherits the same hole: the head `Shape` is looked up in a reading whose
/// dropped TypeDef may be another `Shape` owning a `Circle` case of its own.
#[test]
fn a_dropped_type_in_the_enclosing_namespace_defers_a_qualified_case_pattern() {
    let src = "namespace Demo.CasePat\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | Shape.Circle r -> r\n\
               \x20       | _ -> 0\n";

    // Control: `qualified_case_pattern_resolves_at_the_enclosing_namespace` pins
    // the clean commit; repeat it here so this test cannot go vacuous on its own.
    let clean = fixture_env();
    assert!(
        matches!(
            resolve(src, &clean).resolution_at(at(src, "Shape")),
            Some(Resolution::Entity(_))
        ),
        "control: a clean `Demo.CasePat` commits the case pattern"
    );

    let mut dropped = fixture_env();
    dropped.mark_namespace_dropped_type(vec!["Demo".into(), "CasePat".into()]);
    let rf = resolve(src, &dropped);
    assert_eq!(
        rf.resolution_at(at(src, "Shape")),
        None,
        "a dropped TypeDef in `Demo.CasePat` may own the `Circle` case FCS binds"
    );
    assert_eq!(rf.resolution_at(at(src, "Shape.Circle")), None);
}

/// Why the gate is keyed on the **path**, not the reading: a qualified
/// annotation is looked up in `prefix ++ names[..n-1]`, which is not the reading
/// prefix. Here the root tier's prefix is `[]` while the type is found in
/// `Demo.CasePat`, so `namespace_has_dropped_type(prefix)` would answer about
/// `[]` and commit the survivor. Hence
/// `any_split_of_a_module_path_has_a_dropped_type` over `prefix ++ names`.
#[test]
fn a_dropped_type_at_a_qualified_paths_split_defers_the_annotation() {
    let src = "namespace Consumer\n\
               module M =\n\
               \x20   let f (x: Demo.CasePat.Shape) = x\n";

    let clean = fixture_env();
    let shape = clean
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");
    assert_eq!(
        resolve(src, &clean).resolution_at(at(src, "Shape")),
        Some(Resolution::Entity(shape)),
        "control: the fully-qualified path commits at the root reading"
    );

    // The drop is in the namespace the *leaf* is looked up in — which no tier
    // prefix of this walk equals.
    let mut dropped = fixture_env();
    dropped.mark_namespace_dropped_type(vec!["Demo".into(), "CasePat".into()]);
    assert!(
        !matches!(
            resolve(src, &dropped).resolution_at(at(src, "Shape")),
            Some(Resolution::Entity(_))
        ),
        "the dropped TypeDef sits at a split of `Demo.CasePat.Shape`, not at the \
         root reading the walk visits — got {:?}",
        resolve(src, &dropped).resolution_at(at(src, "Shape"))
    );

    // And at an intermediate split: a drop in `Demo` may be a module `CasePat`
    // whose own `Shape` FCS merges in at the same path.
    let mut mid = fixture_env();
    mid.mark_namespace_dropped_type(vec!["Demo".into()]);
    assert!(
        !matches!(
            resolve(src, &mid).resolution_at(at(src, "Shape")),
            Some(Resolution::Entity(_))
        ),
        "a drop at the `Demo` split may be a same-FQN `Demo.CasePat` half — got {:?}",
        resolve(src, &mid).resolution_at(at(src, "Shape"))
    );
}

// The one dropped-TypeDef property with no case here, and why. A per-tier
// verdict stops being consulted once `resolve_assembly_path_over` holds a
// partial fallback, while a complete reading at a lower tier out-ranks a
// partial at a higher one — so a dropped type below a held fallback is exactly
// the reading FCS prefers. `dropped_type_could_root_this_path` scans every
// reading before the walk runs, so `fallback` cannot suppress it and the
// property holds by construction.
//
// No test asserts it because none would discriminate: every witness
// constructible against this fixture — a drop under the `open`ed path, at the
// root reading, or at the file's own enclosing namespace — is already deferred
// by the open-side opacity fold or the enclosing-namespace guard, so the
// assertion passes whether or not the gate is consulted. Such a test reads as
// coverage it is not. The property may be unreachable through today's
// resolver; it is part of the walk's contract either way.

/// A **project `[<AutoOpen>]` module** in a reading the type walk visits can
/// supply a type of the name being resolved, and FCS binds *it* over the same
/// namespace's direct members — the project-side twin of the assembly-side rule
/// [`AssemblyEnv::auto_open_modules_in_namespace_shadow_type_named`] already
/// encodes. Sema does not enumerate such a module's types, so *which* of them it
/// holds is unmodelled; committing the assembly type over one is a wrong target,
/// not a missed one.
///
/// fsc-verified (two projects, `net10.0`): `Lib` declares
/// `namespace N` + `type Foo = { FromLib : int }`; the referencing project
/// declares `namespace N` + `[<AutoOpen>] module Auto = type Foo = { FromProjectAutoOpen : string }`.
/// `let f () : Foo = { FromProjectAutoOpen = "x" }` compiles;
/// `{ FromLib = 1 }` fails with *"No assignment given for field
/// 'FromProjectAutoOpen' of type 'N.Auto.Foo'"* — FCS named the auto-open type.
/// Dropping the `[<AutoOpen>]` module makes `{ FromLib = 1 }` compile, so the
/// probe discriminates.
///
/// Both halves of that probe are asserted here. Which *channel* declines the
/// first is not pinned by a single-file case — the same-file auto-open name set
/// would reach it too — and is the subject of
/// [`a_cross_file_project_auto_open_module_defers_only_a_name_the_project_declares`].
#[test]
fn a_project_auto_open_module_defers_a_same_namespace_assembly_type() {
    let env = fixture_env();
    let shadowed = "namespace Demo.CasePat\n\
                    [<AutoOpen>]\n\
                    module Auto =\n\
                    \x20   type Shape = { FromProjectAutoOpen : string }\n\
                    module M =\n\
                    \x20   let f (y: Shape) = y\n";
    assert!(
        !matches!(
            resolve(shadowed, &env).resolution_at(at_last(shadowed, "Shape")),
            Some(Resolution::Entity(_))
        ),
        "the project `[<AutoOpen>] module Auto` declares its own `Shape`, which \
         FCS binds over the assembly's — got {:?}",
        resolve(shadowed, &env).resolution_at(at_last(shadowed, "Shape"))
    );

    let shape = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");

    // Control: the same module holding no type of the name commits. The veto is
    // keyed on a name the project declares somewhere, not on the module's mere
    // presence — fsc-verified by the second half of the probe above.
    let unrelated = "namespace Demo.CasePat\n\
                     [<AutoOpen>]\n\
                     module Auto =\n\
                     \x20   let v = 1\n\
                     module M =\n\
                     \x20   let f (y: Shape) = y\n";
    assert_eq!(
        resolve(unrelated, &env).resolution_at(at(unrelated, "Shape")),
        Some(Resolution::Entity(shape)),
        "control: an auto-open module that declares no `Shape` hides none"
    );

    // Control: the identical file without the auto-open module commits, so the
    // decline is caused by the module and not by the enclosing-namespace tier.
    let clean = "namespace Demo.CasePat\n\
                 module M =\n\
                 \x20   let f (y: Shape) = y\n";
    assert_eq!(
        resolve(clean, &env).resolution_at(at(clean, "Shape")),
        Some(Resolution::Entity(shape)),
        "control: with no project auto-open module the annotation commits"
    );
}

/// The channel in isolation: a project `[<AutoOpen>]` module in a **preceding
/// Compile-order file**, where no same-file signal can reach the use and the
/// only thing that can decline is the namespace-keyed
/// `project_shadow_at`.
///
/// That veto is keyed on two facts about *different* files — the module's
/// namespace, and whether any project file declares a type of the name — and
/// its soundness is that the second is complete: a type inside the module is a
/// project type declaration like any other, so the whole-file pre-scan sees it
/// whether or not the walk indexes it. The two cases below are that claim's
/// only two outcomes.
///
/// It is the channel's **only** guard, measured rather than assumed: stubbing
/// `project_shadow_at`'s type index to `false` leaves the
/// single-file cases above green — the same-file auto-open name set reaches
/// those uses too — and leaves `tier_order_diff` green, whose probes are all
/// one file. Only this case fails.
#[test]
fn a_cross_file_project_auto_open_module_defers_only_a_name_the_project_declares() {
    let env = fixture_env();
    let shape = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");
    let user = "namespace Demo.CasePat\n\
                module M =\n\
                \x20   let f (y: Shape) = y\n";

    let resolve_pair = |first: &str| {
        let files: Vec<_> = [first, user]
            .iter()
            .map(|src| {
                let parsed = parse(src);
                assert!(parsed.errors.is_empty(), "parse errors in {src:?}");
                ImplFile::cast(parsed.root).expect("impl file")
            })
            .collect();
        let project = borzoi_sema::resolve_project(&files, &env);
        project.files()[1].resolution_at(at(user, "Shape"))
    };

    // The earlier file's auto-open module declares the name: unreachable from
    // here through anything sema models, and FCS binds it.
    let declaring = "namespace Demo.CasePat\n\
                     [<AutoOpen>]\n\
                     module Auto =\n\
                     \x20   type Shape = { FromProjectAutoOpen : string }\n";
    assert!(
        !matches!(resolve_pair(declaring), Some(Resolution::Entity(_))),
        "an earlier file's auto-open module declares `Shape`; committing the assembly \
         type would be a wrong target — got {:?}",
        resolve_pair(declaring)
    );

    // It declares something else: no project file declares `Shape` at all, so
    // nothing can be hiding in there and the assembly type commits.
    let not_declaring = "namespace Demo.CasePat\n\
                         [<AutoOpen>]\n\
                         module Auto =\n\
                         \x20   type Other = { FromProjectAutoOpen : string }\n";
    assert_eq!(
        resolve_pair(not_declaring),
        Some(Resolution::Entity(shape)),
        "no project file declares `Shape`, so the auto-open module cannot hold one"
    );
}

/// The same tier, the other name-blind channel: an assembly whose abbreviations
/// are [`AbbreviationVisibility::Unknowable`] may declare a metadata-invisible
/// `Shape` abbreviation into the namespace, which FCS merges with the visible
/// union and may bind instead. A *visible* match at the tier is no evidence
/// against it, so the tier cannot be trusted even when it resolves.
#[test]
fn an_unknowable_abbreviation_namespace_defers_a_type_it_does_resolve() {
    use borzoi_assembly::EcmaView;
    use borzoi_sema::AbbreviationVisibility;
    let bytes = std::fs::read(ensure_fixture_built()).expect("read F# abbreviation fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse F# abbreviation fixture dll");
    let entities = view.enumerate_type_defs().expect("enumerate fixture types");
    let unknowable = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        PathBuf::from("SemaFSharpAbbrevFixture.dll"),
        entities,
        AbbreviationVisibility::Unknowable,
        Vec::new(),
    )]);
    let src = "namespace Demo.CasePat\n\
               module M =\n\
               \x20   let f (y: Shape) = y\n";
    assert!(
        !matches!(
            resolve(src, &unknowable).resolution_at(at(src, "Shape")),
            Some(Resolution::Entity(_))
        ),
        "an unknowable-abbreviation namespace may hold an invisible `Shape` \
         abbreviation FCS binds instead — got {:?}",
        resolve(src, &unknowable).resolution_at(at(src, "Shape"))
    );

    // Control: the same source against the *decodable* env commits, so the
    // decline is the unknowable pickle and not the shape of the file.
    let clean = fixture_env();
    let shape = clean
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");
    assert_eq!(
        resolve(src, &clean).resolution_at(at(src, "Shape")),
        Some(Resolution::Entity(shape)),
        "control: a decodable pickle commits the same annotation"
    );
}

/// The qualified-union-case-pattern head shares the type path's per-tier verdict,
/// so it inherits both channels: an invisible same-named union at the tier owns
/// the case FCS binds.
#[test]
fn an_unknowable_abbreviation_namespace_defers_a_qualified_case_pattern() {
    use borzoi_assembly::EcmaView;
    use borzoi_sema::AbbreviationVisibility;
    let bytes = std::fs::read(ensure_fixture_built()).expect("read F# abbreviation fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse F# abbreviation fixture dll");
    let entities = view.enumerate_type_defs().expect("enumerate fixture types");
    let unknowable = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        PathBuf::from("SemaFSharpAbbrevFixture.dll"),
        entities,
        AbbreviationVisibility::Unknowable,
        Vec::new(),
    )]);
    let src = "namespace Demo.CasePat\n\
               module M =\n\
               \x20   let f x =\n\
               \x20       match x with\n\
               \x20       | Shape.Circle r -> r\n\
               \x20       | _ -> 0\n";
    assert_eq!(
        resolve(src, &unknowable).resolution_at(at(src, "Shape")),
        None,
        "the case head's tier is untrustworthy for the same reason the annotation's is"
    );

    // Control: `qualified_case_pattern_resolves_at_the_enclosing_namespace` pins
    // the clean commit; repeated so this cannot go vacuous on its own.
    let clean = fixture_env();
    assert!(
        matches!(
            resolve(src, &clean).resolution_at(at(src, "Shape")),
            Some(Resolution::Entity(_))
        ),
        "control: a decodable pickle commits the case pattern"
    );
}

/// The veto is accessibility-filtered: a `module private` auto-open shadows only
/// where it is *visible*. Its same-file path record carries the privacy flag, and
/// ignoring it would defer a name FCS resolves — an availability regression the
/// stronger verdict would otherwise buy.
///
/// fsc-verified against the same two-project setup: with
/// `namespace N` + `[<AutoOpen>] module private Auto = type Foo = { FromProjectAutoOpen : string }`
/// followed by `namespace Other` + `open N`, `let f () : Foo = { FromLib = 1 }`
/// **compiles** — the private module is invisible there, so the referenced
/// assembly's `N.Foo` binds. Move the same use inside `namespace N` and it fails
/// with "type 'N.Auto.Foo'", so the module does shadow within its own scope.
#[test]
fn a_private_project_auto_open_module_shadows_only_within_its_scope() {
    let env = fixture_env();
    let shape = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");

    // Out of scope: a different namespace group in the same file. The private
    // module cannot supply `Shape` there, so the assembly type must commit —
    // and it declares one, so only the visibility filter can be what lets this
    // through.
    let outside = "namespace Demo.CasePat\n\
                   [<AutoOpen>]\n\
                   module private Auto =\n\
                   \x20   type Shape = { FromProjectAutoOpen : string }\n\
                   namespace Other\n\
                   open Demo.CasePat\n\
                   module M =\n\
                   \x20   let f (y: Shape) = y\n";
    assert_eq!(
        resolve(outside, &env).resolution_at(at_last(outside, "Shape")),
        Some(Resolution::Entity(shape)),
        "a private auto-open module is invisible from another namespace group — \
         vetoing there would defer a name FCS resolves"
    );

    // In scope: the same private module, used from inside its own namespace.
    let inside = "namespace Demo.CasePat\n\
                  [<AutoOpen>]\n\
                  module private Auto =\n\
                  \x20   type Shape = { FromProjectAutoOpen : string }\n\
                  module M =\n\
                  \x20   let f (y: Shape) = y\n";
    assert!(
        !matches!(
            resolve(inside, &env).resolution_at(at_last(inside, "Shape")),
            Some(Resolution::Entity(_))
        ),
        "within its own namespace the private module is in scope and declares \
         `Shape` — got {:?}",
        resolve(inside, &env).resolution_at(at_last(inside, "Shape"))
    );
}

/// A tripwire against a tempting "optimisation": the veto must still fire for a
/// use **inside the auto-open module's own body**. It looks excludable — the
/// module is already recorded in `auto_open_module_paths` before its body is
/// walked, and `[<AutoOpen>]` is *about* what happens outside the declaration —
/// but its types are in scope inside it lexically, whether or not `AutoOpen` is
/// involved, so the shadow risk is exactly the sibling module's.
///
/// fsc-verified: `namespace N` + `[<AutoOpen>] module Auto` containing both
/// `type Foo = { FromProjectAutoOpen : string }` and `let usesLib () : Foo = {
/// FromLib = 1 }` fails with *"No assignment given for field
/// 'FromProjectAutoOpen' of type 'N.Auto.Foo'"* — the module's own `Foo` beats
/// the referenced assembly's `N.Foo` inside the body. Excluding the module here
/// would therefore commit a **wrong target**, not recover a missed one.
///
/// When the module declares no such type the annotation commits, which
/// fsc agrees with (`{ FromLib = 1 }` compiles then) — the second case below.
#[test]
fn a_project_auto_open_module_vetoes_its_own_body_too() {
    let env = fixture_env();
    let src = "namespace Demo.CasePat\n\
               [<AutoOpen>]\n\
               module Auto =\n\
               \x20   type Shape = { FromProjectAutoOpen : string }\n\
               \x20   let f (y: Shape) = y\n";
    assert!(
        !matches!(
            resolve(src, &env).resolution_at(at_last(src, "Shape")),
            Some(Resolution::Entity(_))
        ),
        "`Auto`'s own types are in scope in its body, so its `Shape` shadows the \
         assembly union exactly as it would for a sibling — got {:?}",
        resolve(src, &env).resolution_at(at_last(src, "Shape"))
    );

    let unrelated = "namespace Demo.CasePat\n\
                     [<AutoOpen>]\n\
                     module Auto =\n\
                     \x20   let f (y: Shape) = y\n";
    let shape = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");
    assert_eq!(
        resolve(unrelated, &env).resolution_at(at(unrelated, "Shape")),
        Some(Resolution::Entity(shape)),
        "control: `Auto` declares no `Shape`, so its own body sees the assembly's"
    );
}

/// The evidence that the project channel is keyed by the path's **form** and
/// not merely by its head's name.
///
/// A [`ShadowVeto::Preemptive`] verdict ends the whole walk, root tier and all,
/// so a channel that answered "some project declaration has this name" would
/// stop *fully-qualified* annotations resolving in every file whose namespace
/// holds a project `[<AutoOpen>]` module — the commonest shape in F#
/// (`System.Text.Json.X`). The auto-open module here declares a **type** of the
/// head's name, which fsc-verifiably cannot own the tail (FCS falls through to
/// the referenced namespace), so the dotted head asks the module index and this
/// path commits. `a_project_auto_open_module_defers_a_dotted_head_it_declares_a_module_for`
/// is the same file with a module in that slot, and it declines.
#[test]
fn a_fully_qualified_path_still_commits_beside_a_project_auto_open_module() {
    let env = fixture_env();
    let src = "namespace Demo.CasePat\n\
               [<AutoOpen>]\n\
               module Auto =\n\
               \x20   type Demo = { FromProjectAutoOpen : string }\n\
               module M =\n\
               \x20   let f (y: Demo.CasePat.Shape) = y\n";
    let shape = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");
    assert_eq!(
        resolve(src, &env).resolution_at(at(src, "Shape")),
        Some(Resolution::Entity(shape)),
        "the fully-qualified reading must survive the project auto-open module"
    );
}

/// The dotted twin of the bare project channel: what owns a *dotted* head is a
/// **module**, so the head is asked of the project's module names rather than
/// its type names.
///
/// fsc-verified, all three shapes, with the assembly's `Demo.CasePat.Shape`
/// present as a referenced project:
///
/// - `[<AutoOpen>] module Auto = module Demo = module CasePat = type Shape`
///   compiles, and a record literal built at the annotated type typechecks
///   against the *project*'s field — so FCS reads the whole path through the
///   project module and the assembly type is a wrong target;
/// - the same module holding nothing the path needs (`module Demo = let
///   unrelated = 1`) still compiles: FCS falls through to the referenced
///   namespace. Sema cannot tell the two apart — it does not enumerate an
///   auto-open module's contents — so it defers for both, and this case is the
///   veto's cost;
/// - `type Demo = { … }` in that position also falls through, which is why the
///   bare channel's type index is *not* consulted for a dotted head.
#[test]
fn a_project_auto_open_module_defers_a_dotted_head_it_declares_a_module_for() {
    let env = fixture_env();
    let shadowed = "namespace Demo.CasePat\n\
                    [<AutoOpen>]\n\
                    module Auto =\n\
                    \x20   module Demo =\n\
                    \x20       module CasePat =\n\
                    \x20           type Shape = { FromProjectAutoOpen : string }\n\
                    module M =\n\
                    \x20   let f (y: Demo.CasePat.Shape) = y\n";
    assert!(
        !matches!(
            resolve(shadowed, &env).resolution_at(at_last(shadowed, "Shape")),
            Some(Resolution::Entity(_))
        ),
        "the project `[<AutoOpen>] module Auto` declares a `Demo` the whole path \
         reads through; committing the assembly type is a wrong target — got {:?}",
        resolve(shadowed, &env).resolution_at(at_last(shadowed, "Shape"))
    );

    let shape = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");

    // Control: an auto-open module declaring some *other* module leaves the head
    // unclaimed, so the fully-qualified reading commits. The veto is keyed on
    // the head's name, not on the module's presence.
    let unrelated = "namespace Demo.CasePat\n\
                     [<AutoOpen>]\n\
                     module Auto =\n\
                     \x20   module Other =\n\
                     \x20       let v = 1\n\
                     module M =\n\
                     \x20   let f (y: Demo.CasePat.Shape) = y\n";
    assert_eq!(
        resolve(unrelated, &env).resolution_at(at(unrelated, "Shape")),
        Some(Resolution::Entity(shape)),
        "control: no project module named `Demo`, so nothing can own this head"
    );
}

/// A **module abbreviation** in the auto-open module is not a shadow risk, and
/// the module index must not treat it as one.
///
/// fsc-verified: `[<AutoOpen>] module Auto = module Lst =
/// Microsoft.FSharp.Collections.List` leaves a *sibling* module's `Lst.length`
/// failing with FS0039. An abbreviation binds a name inside its own container
/// and is published nowhere — so it cannot be the unmodelled declaration this
/// channel guards against.
///
/// Inside that container it *is* in scope, but there it is not unmodelled
/// either: `Resolver::module_aliases` resolves same-file abbreviations
/// directly. Everything this index carries is a declaration nothing else
/// models, which is what makes a name in it evidence of a real hazard.
#[test]
fn a_module_abbreviation_in_an_auto_open_module_is_not_a_dotted_head_shadow() {
    let env = fixture_env();
    let shape = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");
    let src = "namespace Demo.CasePat\n\
               [<AutoOpen>]\n\
               module Auto =\n\
               \x20   module Demo = Microsoft.FSharp.Collections.List\n\
               module M =\n\
               \x20   let f (y: Demo.CasePat.Shape) = y\n";
    assert_eq!(
        resolve(src, &env).resolution_at(at(src, "Shape")),
        Some(Resolution::Entity(shape)),
        "an abbreviation publishes nothing, so it hides no `Demo` from this path"
    );
}

/// The dotted channel in isolation, across files — the module-index twin of
/// [`a_cross_file_project_auto_open_module_defers_only_a_name_the_project_declares`],
/// and for the same reason: `tier_order_diff`'s probes are all one file, so the
/// same-file case above would still pass with the cross-file fold stubbed out.
/// Only this one fails.
#[test]
fn a_cross_file_project_auto_open_module_defers_a_dotted_head_it_declares_a_module_for() {
    let env = fixture_env();
    let shape = env
        .lookup_type(&["Demo".into(), "CasePat".into()], "Shape", 0)
        .expect("Demo.CasePat.Shape in env");
    let user = "namespace Demo.CasePat\n\
                module M =\n\
                \x20   let f (y: Demo.CasePat.Shape) = y\n";

    let resolve_pair = |first: &str| {
        let files: Vec<_> = [first, user]
            .iter()
            .map(|src| {
                let parsed = parse(src);
                assert!(parsed.errors.is_empty(), "parse errors in {src:?}");
                ImplFile::cast(parsed.root).expect("impl file")
            })
            .collect();
        let project = borzoi_sema::resolve_project(&files, &env);
        project.files()[1].resolution_at(at(user, "Shape"))
    };

    let declaring = "namespace Demo.CasePat\n\
                     [<AutoOpen>]\n\
                     module Auto =\n\
                     \x20   module Demo =\n\
                     \x20       module CasePat =\n\
                     \x20           type Shape = { FromProjectAutoOpen : string }\n";
    assert!(
        !matches!(resolve_pair(declaring), Some(Resolution::Entity(_))),
        "an earlier file's auto-open module declares the `Demo` this path reads \
         through — got {:?}",
        resolve_pair(declaring)
    );

    let not_declaring = "namespace Demo.CasePat\n\
                         [<AutoOpen>]\n\
                         module Auto =\n\
                         \x20   module Other =\n\
                         \x20       let v = 1\n";
    assert_eq!(
        resolve_pair(not_declaring),
        Some(Resolution::Entity(shape)),
        "no project file declares a module `Demo`, so the head is unclaimed"
    );
}

// ===== The decline census: what *occupies* the name, not merely that it is =====
//
// Every shape below stops the assembly walk for a different reason — an alias
// whose target cannot be chased, an alias that owns its tail, a project entity
// holding the path, a case-pattern head bound by a non-union, a rooting two
// DLLs contest. The resolver is right to give them one verdict (the walk stops
// here), but a census that also gave them one *cause* could not price a change
// to any of them: the aggregate moves and nothing says which model owned the
// move. So each names its own occupant, and this table is the two-sided ratchet
// on that — a decline that changes which guard produced it fails here.

/// Every `(cause, tier)` label pair `src` records against `env`, deduplicated:
/// the census's whole view of a snippet, keyed by nothing positional so a case
/// asserts on the guards that fired rather than on the range they were filed
/// under.
fn decline_causes(src: &str, env: &AssemblyEnv) -> Vec<String> {
    let rf = resolve(src, env);
    let mut seen: Vec<String> = rf
        .decline_sites()
        .map(|(_, site)| format!("{}@{}", site.cause.label(), site.tier.label()))
        .collect();
    seen.sort();
    seen.dedup();
    seen
}

/// The synthetic env of `absent_child_past_a_chased_alias_defers_instead_of_ceding`:
/// a chased alias in the later (higher-priority) open whose target lacks the
/// written child, against a lower open's same-named type that does declare it.
fn alias_owned_tail_env() -> AssemblyEnv {
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
    two_dll_env(vec![widget, alias], vec![other_alias])
}

#[test]
fn each_occupied_name_decline_names_the_thing_that_occupies_it() {
    // `Lib.Str = System.String` with no `System.Runtime` in the env: the alias
    // binds the name and its target cannot be chased, in either walk.
    let unchaseable_value = "module M\nlet _ = Lib.Str.Format()\n";
    let unchaseable_type = "module M\nlet f (v : Lib.Str) = v\n";
    // A same-file `namespace Lib` declaring `Widget` exports that *type* path,
    // so a sibling module's qualified annotation is project-shadowed.
    let project_type_path =
        "namespace Lib\n\ntype Widget = int\n\nmodule M =\n    let f (v : Lib.Widget) = v\n";
    // A project `module Lib` holds the head of a value path.
    let project_module = "module M\nmodule Lib =\n    let x = 1\nlet _ = Lib.Widget.Make()\n";
    // `Lib.UAlias` aliases the union that owns `UCase`; the case-pattern head
    // walk binds the alias and does not chase it.
    let case_head = "namespace Consumer\nopen Lib\nmodule M =\n    let f x =\n        match x with\n        | UAlias.UCase -> 0\n        | _ -> 1\n";
    // The same union's FQN exported by two loaded DLLs.
    let case_contested = "namespace Consumer\nopen Demo.CasePat\nmodule M =\n    let f x =\n        match x with\n        | Shape.Circle r -> r\n        | _ -> 0\n";
    // A chased alias owns the path, so its absent child defers rather than
    // ceding to the lower open's same-named type.
    let alias_tail = "module M\nopen OtherNs\nopen AliasNs\nlet f (x : Alias.Inner) = x\n";

    let env = fixture_env();
    let doubled = fixture_env_doubled();
    let synthetic = alias_owned_tail_env();
    let cases: [(&str, &str, &AssemblyEnv, &str); 7] = [
        (
            "an unchaseable alias target in the value walk",
            unchaseable_value,
            &env,
            "alias_target_unchaseable@root",
        ),
        (
            "an unchaseable alias target in the type walk",
            unchaseable_type,
            &env,
            "alias_target_unchaseable@root",
        ),
        (
            "a project type exported at the annotated type path",
            project_type_path,
            &env,
            "project_type_path_shadow@root",
        ),
        (
            "a project module holding a value path's head",
            project_module,
            &env,
            "project_path_shadow@root",
        ),
        (
            "an assembly abbreviation on a case-pattern head",
            case_head,
            &env,
            "case_pattern_head_occupied@explicit_open",
        ),
        (
            "a case-pattern head two DLLs contest",
            case_contested,
            &doubled,
            "contested_rooting@explicit_open",
        ),
        (
            "an absent child past a chased alias",
            alias_tail,
            &synthetic,
            "alias_owned_tail@explicit_open",
        ),
    ];

    for (what, src, case_env, expected) in cases {
        assert!(
            decline_causes(src, case_env).contains(&expected.to_string()),
            "{what}: expected the census to record {expected:?}, got {:?}",
            decline_causes(src, case_env),
        );
    }

    // The property the table exists for, stated separately from the labels: no
    // two of these shapes may share a cause. A split that later re-merged two
    // of them would keep every `contains` above passing only by making this
    // fail.
    let mut causes: Vec<&str> = cases
        .iter()
        .map(|(_, _, _, expected)| expected.split('@').next().expect("a cause label"))
        .collect();
    causes.sort_unstable();
    let distinct = {
        let mut c = causes.clone();
        c.dedup();
        c
    };
    assert_eq!(
        distinct.len(),
        // The two alias-target rows are the one deliberate sharing: chasing an
        // alias is the same question in either walk, and the fix is the same
        // model.
        cases.len() - 1,
        "these shapes must not collapse into one cause: {causes:?}"
    );
}
