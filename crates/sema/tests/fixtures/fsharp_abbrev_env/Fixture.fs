namespace Lib

type Marker = { Value: int }

// A same-assembly type with a static member, and an abbreviation aliasing it.
// Unlike the `string`/`int` aliases below (whose BCL/FSharp.Core targets are not
// loaded in the single-DLL fixture env), `Widget` lives in THIS assembly, so it
// is loaded — which lets the resolver chase `WidgetAlias` to it and resolve the
// `Make` static tail THROUGH the alias (Stage 4 resolve-through).
type Widget() =
    static member Make () = 1

type WidgetAlias = Widget

// Stage 4 resolve-through ownership (codex round 4): `UAlias = U` aliases a union.
// A qualified `UAlias.UCase` must bind THROUGH the alias to the union *case* —
// which lives in `union_case_names`, not the `members` surface the tail walk
// searches — and must OWN the path, so an earlier `open` of a same-named,
// top-level `UAlias` (namespace `Lib.Lower`, below, with a real static `UCase`)
// cannot win. Absence of `UCase` from the target's member surface must not cede.
type U =
    | UCase

type UAlias = U

// A nested alias of a loaded same-assembly type (codex round 5): the nested
// descent branch must treat a *terminal* nested alias (`Lib.Nested.NestedAlias`,
// no member tail) as a bare use and DEFER it, exactly like the top-level rooting
// branch, while a *qualifier* through it (`Lib.Nested.NestedAlias.Make`) still
// resolves the `Make` static on the chased `Widget` target.
module Nested =
    type NestedAlias = Widget

// An abbreviation with BOTH a loaded target AND a ModuleSuffix companion module
// (codex round 6): FCS routes `WidgetC.Make` to the companion module's `Make`,
// NOT the target `Widget`'s static `Make` — a module-over-target member
// precedence we do not model, so a member access through such an alias must
// DEFER rather than commit the target's member.
type WidgetC = Widget

[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
module WidgetC =
    let Make () = 2

// A nullary abbreviation ALONGSIDE a generic type of the same source name
// (codex round 9): `lookup_type(.., 0)` selects the nullary alias, and FCS
// resolves `AliasO.Make` through `Widget`. The cross-DLL-collision guard must
// count distinct DLLs at arity 0, not all same-named entities, or it would
// wrongly defer this legal same-DLL overload.
type AliasO = Widget

type AliasO<'T> = { OValue: 'T }


// ==== Chase-able abbreviation targets (abbreviation-target projection plan,
// Stage 4). Each pins one shape of `AssemblyEnv::resolve_abbreviation_tycon`:
// a same-assembly nominal target, a two-hop chain, a generic head at arity 1,
// and a BCL target that resolves only when `System.Runtime` is in the env.
type MarkerAlias = Marker
type MarkerAliasAlias = MarkerAlias

type GenRec<'T> = { Payload: 'T }
type GenAlias<'T> = GenRec<'T>

type Str = System.String
type Env = System.Environment

type int64 = string

// The review-confirmed same-tier collision: `Collide` exists BOTH as a direct
// ECMA TypeDef in `Lib` and as a metadata-invisible abbreviation inside an
// in-scope `[<AutoOpen>]` module. Probed against real fsc: `open Lib;
// (x : Collide)` binds `Lib.Auto.Collide` (= string), NOT the direct record —
// the auto-open module's contents outrank the namespace's own direct members
// even at the same tier, and the abbreviation emits no TypeDef for the
// precise veto to see unless the pickle overlay synthesises a marker for it.
type Collide = { Direct: int }

[<AutoOpen>]
module Auto =
    type Collide = string

// A private abbreviation is not nameable from outside the assembly, so it
// must NOT produce a shadow marker (`open Lib; (x : Hidden)` cannot bind it).
type private Hidden = string

// An abbreviation with a suffixed MODULE companion (codex review round 2):
// the marker synthesised for the abbreviation takes the source-name index
// slot, hiding the module — but a consumer's plain `open Lib.Companion`
// opens the module's values in FCS, so the resolver must treat the open as
// opaque rather than "a class open that brings nothing".
type Companion = string

[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
module Companion =
    let fromCompanion = 1

// A RENAMED abbreviation with a suffixed module companion (codex round 5):
// the marker carries a source_name (`Renamed`, compiled `RenamedAbbrev`), so
// it lands in the same source-named index pass as the module — the index
// must still give the TYPE the bare name.
[<CompiledName("RenamedAbbrev")>]
type Renamed = string

[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
module Renamed =
    let fromRenamed = 1

// The same renamed-abbreviation + module-companion pair, nested inside a
// module (codex round 6): the nested lookup must apply the same
// type-over-module rule as the top-level index, in any child order.
module Holder =
    [<CompiledName("NestedRenamedAbbrev")>]
    type NestedRenamed = string

    [<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
    module NestedRenamed =
        let fromNested = 1

// The competing lower-priority reading for the resolve-through ownership test: a
// *top-level* `UAlias` (so `lookup_type` finds it) with a real static `UCase`. An
// earlier `open Lib.Lower` puts it in scope below the later `open Lib`'s union
// alias; resolving `UAlias.UCase` must NOT cede to this one.
namespace Lib.Lower

type UAlias() =
    static member UCase = 0

namespace Other.Deep

type Marker = { Value: int }

// ===== The cross-assembly module MERGE (assembly-module-open plan, review round 5) ====
//
// The *same* module FQN as `Demo.ModuleOpen.Shared` in the sibling `autoopen_env`
// fixture. FCS merges two referenced assemblies' same-named modules: `open` imports the
// unique values of each, and a colliding name binds the LATER-REFERENCED assembly's
// (fsi-verified with two probe libraries). We do not model reference order as a
// resolution input, so a collision must DEFER — never bind the wrong assembly's value.
namespace Demo.ModuleOpen

module Shared =
    let onlyInAbbrevFixture () = 71
    let collidingShared () = "abbrev"


// The NAMESPACE half of `Demo.ModuleOpen.Merged` — the sibling autoopen fixture declares a
// MODULE at that path. FCS folds the two halves in *reference order*, so this auto-open
// module's `fromModuleHalf` wins when this assembly is referenced later. Sema does not
// model reference order, so a name both halves supply must DEFER (review round 11).
namespace Demo.ModuleOpen.Merged

[<AutoOpen>]
module NamespaceHalf =
    let fromModuleHalf (x: int) = x + 90
    let onlyInNamespaceHalf () = 91

// A UNION declared directly in the namespace half. Opening a namespace imports its
// unions' **cases** into bare scope, and we enumerate no cases at all — so this case is
// a name the merge imports that sema cannot see. `Tag` deliberately collides with the
// autoopen fixture's `Demo.Auto.Tag` *value*: after `open Demo.Auto` then
// `open Demo.ModuleOpen.Merged`, FCS binds this CASE (the later open wins), so an
// implementation that leaves the earlier value current returns a wrong target.
//
// This is what forces the cross-kind open to raise the generation barrier and not merely
// decline the module half's own names (review round 15).
type Verdict =
    | Tag
    | Acquitted

// ==== The namespace-fold matrix (`namespace_fold_matrix.rs`) ====
// Each child shape lives in its OWN cross-kind namespace `Demo.NsFold.<Shape>` so the
// cells never cross-contaminate: a residue-bearing shape (an auto-open type, a
// case-nameless union) poisons its whole namespace group, which must not leak into a
// sibling shape's cell. The autoopen fixture declares a `module Demo.NsFold.<Shape>`
// at each of these FQNs (the module half), making every one a cross-kind open.

namespace Demo.NsFold.Exn
// An exception constructor — value + pattern scope, a definite `Entity` target.
// `NsExn` collides with the module half's `let NsExn`; `NsExnSolo` is unique.
exception NsExn of int
exception NsExnSolo of int

namespace Demo.NsFold.Union
// A plain union: its cases import bare into value + pattern scope.
type UnionShape =
    | UCaseA
    | UCaseB

namespace Demo.NsFold.RqaUnion
// A `[<RequireQualifiedAccess>]` union: its cases are NOT imported bare (Q6).
[<RequireQualifiedAccess>]
type RqaShape =
    | RqaA
    | RqaB

namespace Demo.NsFold.StructUnion
// A `[<Struct>]` union: its cases import bare (non-RQA); its type name takes the slot.
[<Struct>]
type StructShape =
    | StructOn
    | StructOff

namespace Demo.NsFold.ClassType
// A plain class — a constructor-slot type. `NsClass` collides with the module half's
// `let NsClass` value (a value-vs-type contest — codex P1-A); `NsClassSolo` is unique.
// The statics feed the dotted-head cells: `NsClassSolo.SoloStat` is the qualified
// channel through an uncontested type head; `NsClass.Stat` puts the SAME contest
// behind a dotted head.
type NsClass() =
    member _.Ping() = 1
    static member Stat = 3

type NsClassSolo() =
    member _.Pong() = 2
    static member SoloStat = 4

namespace Demo.NsFold.AutoType
// A non-generic `[<AutoOpen>]` TYPE: FCS adds its statics at the tycon tier (round 14);
// we cannot enumerate them — residue that poisons the whole group.
[<AutoOpen>]
type NsAutoType() =
    static member AutoStatic() = 5

namespace Demo.NsFold.AutoModule
// An `[<AutoOpen>]` submodule of the namespace, folded recursively.
[<AutoOpen>]
module NsAutoModule =
    // `nsAutoVal` collides with the module half's `let nsAutoVal`; `nsAutoSolo` is unique.
    let nsAutoVal () = 6
    let nsAutoSolo () = 7
    [<Literal>]
    let NsLiteral = 4242
    let (|NsEven|NsOdd|) n = if n % 2 = 0 then NsEven else NsOdd

namespace Demo.NsFold.ExnLit
// §8 cell 8b: an exception and a same-named `[<Literal>]` in ONE namespace surface.
// FCS folds the exception at the tycon tier and the `[<AutoOpen>]` module's contents
// after it, so the literal wins the bare name in BOTH positions — in a pattern it is
// a constant pattern that beats the exception. Sema cannot model that re-ordering
// reliably (a `decimal` literal carries no CLI `Literal` flag — Q17), so the
// namespace half folds the exception opaque and the pattern defers.
exception NsExnLit of int

[<AutoOpen>]
module NsExnLitAuto =
    [<Literal>]
    let NsExnLit = 4243

namespace Demo.NsFold.TierClash
// A second-TIER clash in ONE namespace surface: the tycon tier folds the type
// `NsTier`, the `[<AutoOpen>]` module's vals fold a same-named VALUE after it, so
// the value wins the bare slot AND captures the head of a dotted `NsTier.TierStat`.
type NsTier() =
    static member TierStat = 9

[<AutoOpen>]
module NsTierAuto =
    let NsTier = 10

namespace Demo.NsFold.EvictA
// With EvictB: the cross-OPEN dotted-head contest. Both namespaces carry a class
// `NsDup`; `FromA`/`FromB` are unique to one type, `DupStat` is on both, so the
// cells pin F#'s whole-path-first, latest-open-wins qualified precedence.
type NsDup() =
    static member FromA = 11
    static member DupStat = 12

namespace Demo.NsFold.EvictB
// EvictA's twin — see there.
type NsDup() =
    static member FromB = 13
    static member DupStat = 14

namespace Demo.NsFold.Abbrev
// A type abbreviation (erased from IL — pickle-only).
type NsAbbrev = int

namespace Demo.PjMix.NsOnly
// The project-half matrix's REVERSE flavor: an assembly NAMESPACE half (no
// assembly module half anywhere) whose module half is a PROJECT file's
// `module Demo.PjMix.NsOnly`.
type PjNsClass() =
    static member PjNsStat = 12

exception PjNsExn of int

// ---------------------------------------------------------------------------
// The MODULE-OPEN matrix's shapes (`module_open_matrix.rs`): each child shape
// lives in its own module under `Demo.MOpen`, and — unlike `Demo.NsFold` —
// nothing else declares these FQNs, so `open Demo.MOpen.<Shape>` is a PURE
// assembly-module open (the fold's original seam, plan §7's module-open half).
namespace Demo.MOpen

module Vals =
    let mVal () = 300
    [<Literal>]
    let MLit = 301

module ExnMod =
    // A MODULE-level exception commits its entity (§8 demotes only the
    // namespace half), in value and pattern scope both.
    let mExnVal () = 302
    exception MExn of int

module UnionMod =
    type MUnion =
        | MCaseA
        | MCaseB

module RqaMod =
    [<RequireQualifiedAccess>]
    type MRqa =
        | MRqaA
        | MRqaB

module StructMod =
    [<Struct>]
    type MStruct =
        | MOn
        | MOff

module ActPat =
    let (|MEven|MOdd|) n = if n % 2 = 0 then MEven else MOdd

module AutoSub =
    let mAutoOuter () = 303
    [<AutoOpen>]
    module Inner =
        let mAutoInner () = 304

module AutoTypeMod =
    // The `[<AutoOpen>]` TYPE's statics are unenumerable, but SAME-surface:
    // the module's vals fold AFTER the tycon tier, so `mPoisoned` still
    // commits soundly — unlike the namespace matrix's AutoType shape, whose
    // module-half value sat on a DIFFERENT surface (reference-order) and had
    // to demote. FCS-verified by the matrix.
    let mPoisoned () = 305
    [<AutoOpen>]
    type MAutoType() =
        static member MAutoStatic() = 306

module ClassMod =
    // A type nested in the module: its bare name is the constructor slot; its
    // static behind a dotted head is Slice B's nested-type channel.
    let mClassVal () = 307
    type MClass() =
        static member MStat = 308

module SubMod =
    // A plain (non-auto-open) submodule: `open SubMod` does NOT import its
    // contents; `Sub.subVal` is Slice B's submodule dotted head.
    module Sub =
        let subVal () = 309

module ExnLitMod =
    // §8 cell 8b's MODULE-half flavor (codex review): the exception folds at
    // the tycon tier, the `[<AutoOpen>]` literal after it, so the literal wins
    // the bare name in BOTH positions — in a pattern it is a constant pattern
    // that beats the exception. `case_reference` must not dig past the
    // shadowing value entry to the exception (a wrong target).
    exception MExnLit of int

    [<AutoOpen>]
    module MExnLitAuto =
        [<Literal>]
        let MExnLit = 4244

module ExnShadowMod =
    // The same shadow with a PLAIN (non-literal) value: the fix's other face.
    exception MExnShadow of int

    [<AutoOpen>]
    module MExnShadowAuto =
        let MExnShadow = 99

module DupA =
    let dupVal () = 310
    let onlyA () = 311

module DupB =
    // Collides with DupA's `dupVal`: two module opens contest by position —
    // the latest open wins, no reference-order uncertainty.
    let dupVal () = 312
