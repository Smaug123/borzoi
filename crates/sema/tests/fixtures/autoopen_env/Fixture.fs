// Test data for crates/sema's auto-open-module resolution
// (tests/all/resolve_autoopen.rs). It stands in for FSharp.Core.
//
// A *real* F# library, deliberately: the F# compiler emits the
// CompilationMapping / AutoOpen / CompilationSourceName / CompilationRepresentation
// attributes the projector reads, AND the assembly carries a genuine F# signature
// pickle. So when F# member projection moves from ECMA-335 heuristics to being
// pickle-driven, this fixture — and the tests over it — keep working unchanged
// (a C# stand-in could carry the attributes but never a pickle).
//
// Read through Ecma335Assembly::enumerate_type_defs.

namespace Microsoft.FSharp.Core

// An `[<AutoOpen>]` module in the implicitly-opened `Microsoft.FSharp.Core`
// namespace — FSharp.Core's `ExtraTopLevelOperators`, home of `printfn`.
// Opening the namespace (which the resolver does implicitly) also opens this
// module, so `printfnLike` resolves with no `open`. `printfnLike` is *generic*
// (like the real `printfn`/`PrintFormatLine<'T>`) and `[<CompiledName>]`-renamed,
// exercising both the generic-module-member keep and the source-name split.
[<AutoOpen>]
module CoreOps =
    [<CompiledName("PrintFormatLikeLine")>]
    let printfnLike (x: 'T) : 'T = x

    // A plainly-named auto-open member (source name == IL name).
    let plainCore () = 1

// An `[<AutoOpen>]` module of *extension members* — FSharp.Core's `LazyExtensions`
// shape, the one the ⚠ soundness bug fired on. Its augmentations of `System.String`
// compile to public statics of the module class, so the auto-open fold used to push
// them as bare names; FCS pushes neither (`AddValRefsToItems` filters
// `not vref.IsMember`), and neither is reachable module-qualified either — only
// `s.ExtInstance()` reaches one. Both fsi-verified FS0039.
[<AutoOpen>]
module CoreExts =
    type System.String with

        // Instance augmentation: excluded from bare scope (and indexed in
        // `Entity::extension_member_names`).
        member this.ExtInstance() = this + this

        // Static augmentation: excluded too, and *not* covered by the per-method
        // *surface* extension flag (FCS's `IsInstanceMember` gate keeps it off) —
        // it needs `MethodLike::is_fsharp_extension_member`.
        static member ExtStatic(s: string) = s

        // The augmentation half of the `NameClash` collision below.
        member this.NameClash() = this.Length

    // A plain `let` in the very same auto-open module: the filter is
    // extension-keyed, not module-keyed, so this one must still resolve bare.
    let plainBesideExts () = 12

    // A plain `let` sharing its name with an augmentation *in the same module* —
    // F# permits it (the augmentation compiles to a mangled `String.NameClash`,
    // the `let` to a plain `NameClash`). FCS resolves both bare `NameClash` and
    // `CoreExts.NameClash` to the `let` (fsi-verified), so the extension filter
    // must be keyed per *member*, not per name: hiding the name would hide the
    // value with it. (codex review, PR #916)
    let NameClash (x: int) = x + 1

// An `[<Extension>]`-attributed module: fsc marks BOTH the module class and the
// `let` with the CLR `[Extension]` attribute, yet the `let` is a *value*, not a
// member — and FCS adds a module's contents through its vals
// (`AddModuleOrNamespaceContentsToNameEnv`), where the C#-style extension
// predicate never runs. So bare `Tripled` resolves (fsi-verified) even though it
// carries the very attribute that hides `Select` after an `open type`. The
// C#-style filter must therefore not apply to module entities. (codex review)
[<AutoOpen>]
[<System.Runtime.CompilerServices.Extension>]
module CoreExtAttrLets =
    [<System.Runtime.CompilerServices.Extension>]
    let Tripled (x: int) = x * 3

// A module WITHOUT [<AutoOpen>] in the same namespace: opening the namespace
// does NOT bring its members into unqualified scope. Negative control.
module CoreClosed =
    let closedValue () = 2

// An *internal* [<AutoOpen>] module: not accessible cross-assembly, so even its
// members must NOT resolve bare. Negative control for the public-accessibility
// filter on auto-open modules.
[<AutoOpen>]
module internal CoreInternal =
    let internalValue () = 4

namespace Demo.Auto

// An `[<AutoOpen>]` module in a NON-implicit namespace: its members resolve
// unqualified only after an explicit `open Demo.Auto`.
[<AutoOpen>]
module Extra =
    let extraValue () = 3

    // R2-0 regression: nested types of an auto-open module are imported into
    // type scope by F#, but sema only enumerates the module's statics. Opening
    // Demo.Auto must therefore mark bare type annotations as shadowable.
    type int64 = Shadow

    // An *uppercase* auto-open member, so a project file extending this same
    // namespace can declare a colliding union case `Tag`. After `open Demo.Auto`,
    // FCS gives the project case priority over this assembly auto-open value
    // (assembly members are the lowest-priority interpretation of an `open`):
    // `tests/all/resolve_autoopen.rs::project_namespace_case_outranks_assembly_auto_open`.
    let Tag = 99

    // codex round 6: probe-confirmed against real fsc — an accessible nested
    // type of an in-scope auto-open module shadows a *same-tier* direct type
    // of the same name. `open Demo.Auto; (x : SameTierName)` binds this one,
    // not `Demo.Auto.SameTierName` below.
    type SameTierName = { AutoField: int }

    // FCS auto-opens RECURSIVELY (NameResolution.fs's
    // AddModuleOrNamespaceRefsToNameEnv: "Recursive because of 'AutoOpen'"):
    // `open Demo.Auto` opens Extra AND, transitively, ChainedInner — its
    // values resolve bare, and its nested type shadows the same-tier direct
    // `Chained` below exactly as Extra's own nested types do.
    [<AutoOpen>]
    module ChainedInner =
        let chainedValue () = 5
        type Chained = { InnerField: int }

    // DFS-order pin (codex on the transitive-auto-open change): FCS opens
    // auto-open modules depth-first — DeepFirst, then its Deepest, THEN the
    // later sibling DeepSecond — and later-added contents win, so a bare
    // `orderMarker` binds DeepSecond's. A breadth-first traversal would open
    // Deepest last and bind the wrong member.
    [<AutoOpen>]
    module DeepFirst =
        [<AutoOpen>]
        module Deepest =
            let orderMarker () = 1

    [<AutoOpen>]
    module DeepSecond =
        let orderMarker () = 2

    // Negative control: a nested module WITHOUT [<AutoOpen>] is not opened by
    // `open Demo.Auto`, even though its parent is auto-open.
    module ChainedClosed =
        let chainedClosedValue () = 6

    // Negative control: an *internal* nested auto-open module is not
    // accessible cross-assembly, so it must not contribute either.
    [<AutoOpen>]
    module internal ChainedInternal =
        let chainedInternalValue () = 7

// A direct type colliding with `Extra.SameTierName` above, at the exact same
// `Demo.Auto` tier — the same-tier collision codex round 6 flagged.
type SameTierName = { DirectField: int }

// A direct type colliding with the transitively-auto-opened
// `Extra.ChainedInner.Chained` above — the nested chain's same-tier shadow.
type Chained = { DirectField: int }

namespace Demo.Low

// A real, ordinary type also named `int64` — the priority-ordering
// counterpart to `Demo.Auto.Extra.int64` above. Neither namespace shadows the
// other structurally; which wins a same-name lookup depends only on which
// `open` is later (R2-0 codex round 3: the shadow check must participate in
// the *same* priority walk as the real lookup, not run wholly before or
// after it).
type int64 = { RealField: int }

namespace Demo

// A non-generic type sharing a name with a module — F# forces the module's
// `ModuleSuffix` (compiled `TaggedModule`, source name `Tagged`). The type keeps
// the bare name `Tagged`; the module is reachable only by its source name (which
// the type occupies here), never by its compiled name `TaggedModule`.
type Tagged = { TaggedField: int }

[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
module Tagged =
    let wrap (v: int) = v

// A module-suffix module with NO clashing type: its source name `Solo` is free,
// so it is reachable by `Solo` but never by its compiled name `SoloModule`.
[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
module Solo =
    let wrap (v: int) = v

// The same companion collision, but *nested* inside a module: `Outer` holds a
// nested type `Tagged` and a nested suffixed module `Tagged` (compiled
// `TaggedModule`). Nested lookup must prefer the exact-name type.
module Outer =
    type Tagged = { OuterTaggedField: int }

    [<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
    module Tagged =
        let wrap (v: int) = v

// An `[<AutoOpen>]` module in the RELATIVE reading of `open Sub` from
// `namespace Demo` (which resolves to `Demo.Sub`). `sharedMarker` collides with
// the root `Sub.RootAuto.sharedMarker` below; latest-open-wins keeps the relative
// reading higher, so from `namespace Demo; open Sub` a bare `sharedMarker` is
// `Demo.Sub.RelAuto.sharedMarker` (FCS) — the auto-open modules of both readings
// must be applied lowest-priority-first so the relative one wins the collision.
namespace Demo.Sub

[<AutoOpen>]
module RelAuto =
    let sharedMarker = 1
    let relOnlyMarker = 10

// An `[<AutoOpen>]` module in the ROOT reading of `open Sub` (root `Sub`, distinct
// from the relative `Demo.Sub`). Its `rootOnlyMarker` is reachable only through
// the root reading; its `sharedMarker` is shadowed by the relative one.
namespace Sub

[<AutoOpen>]
module RootAuto =
    let sharedMarker = 2
    let rootOnlyMarker = 20

// ===== Assembly-level `[<assembly: AutoOpen("…")>]` (plan A3/S3) =====
//
// FSharp.Core's manifest carries assembly-level AutoOpen attributes naming
// the namespaces (and a few modules) the compiler implicitly opens in every
// file — there is no hardcoded list in FCS. These stand in for that shape.

// A namespace named by the manifest attribute below: implicitly opened in
// every file referencing this assembly, so its own [<AutoOpen>] module
// contributes bare names with no `open` in source (the `Microsoft.FSharp.Core`
// mechanism, but for a path the resolver cannot have hardcoded).
namespace SemaAutoOpen.FromManifest

[<AutoOpen>]
module ManifestOps =
    let manifestValue () = 8

// Negative control: a plain (non-auto-open) module in the manifest-opened
// namespace still requires qualification.
module ManifestClosed =
    let manifestClosedValue () = 9

// A MODULE named directly by an assembly-level AutoOpen (FSharp.Core does
// this for `LanguagePrimitives.IntrinsicOperators` and the
// `TaskBuilderExtensions` priorities). Sema deliberately does NOT open these
// yet: their real-world surface is operators (A4/S4) and extension members,
// and extension-member statics must never become bare-resolvable.
namespace SemaAutoOpen

module DirectOps =
    let directValue () = 11

[<assembly: AutoOpen("SemaAutoOpen.FromManifest")>]
[<assembly: AutoOpen("SemaAutoOpen.DirectOps")>]
// A path that exists nowhere — FCS warns and skips it; it must not sink or
// skew resolution.
[<assembly: AutoOpen("SemaAutoOpen.NoSuchPath")>]
do ()

// ===== The extension-visibility matrix (tests/all/extension_visibility_matrix.rs) =====
//
// Every *declaration shape* an extension member can take, so the matrix test can
// cross them with every *access channel* (bare after `open`, bare after auto-open,
// module-qualified, bare after `open type`, type-qualified) and diff each cell
// against FCS. The bugs this repo shipped in PR #916 were all single cells of that
// grid — "C#-style extension, bare after `open type`", "static augmentation,
// module-qualified", "`[<Extension>]` module `let`, bare" — so the grid, not the
// examples, is the unit of coverage.

namespace Demo.ExtMatrix

// Augmentations of a BCL type (optional type extensions: they cross the assembly
// boundary via `String.<Member>` name mangling), beside plain `let`s.
module Aug =
    type System.String with

        member this.InstAug() = this.Length

        static member StatAug(s: string) = s

        // `[<CompiledName>]`-renamed: the IL name loses the mangling, so the
        // projector's dot-name fallback cannot see it — only the pickle can.
        [<CompiledName("RenamedAugCompiled")>]
        member this.RenamedAug() = this.Length

        // Generic-method augmentation: the shape the retired per-method overlay
        // could not flag at all.
        member this.GenericAug(x: 'a) = this.Length

        // Collides with the plain `let` below.
        member this.Clash() = this.Length

    let plainLet (x: int) = x + 1

    // Same name as the augmentation above: FCS resolves both bare `Clash` and
    // `Aug.Clash` to THIS one.
    let Clash (x: int) = x + 2

// The same shapes behind an `[<AutoOpen>]`, for the auto-open channel.
[<AutoOpen>]
module AugAuto =
    type System.String with

        member this.AutoInstAug() = this.Length

        static member AutoStatAug(s: string) = s

    let autoPlainLet (x: int) = x + 3

// An `[<Extension>]` module: fsc marks the module class AND each `let` with the
// CLR attribute, yet the `let`s are values — FCS admits them to bare scope.
[<System.Runtime.CompilerServices.Extension>]
module ExtAttrLets =
    [<System.Runtime.CompilerServices.Extension>]
    let ExtAttrLet (x: int) = x * 2

// A C#-style extension type declared in F#: `open type` must NOT make `CsStyle`
// bare-resolvable, but must make the plain `PlainStatic` so.
[<System.Runtime.CompilerServices.Extension>]
type ExtType =

    [<System.Runtime.CompilerServices.Extension>]
    static member CsStyle(x: int) = x * 3

    static member PlainStatic(x: int) = x * 4

    // A *curried* C#-style extension static. FCS's predicate matches only methods
    // with exactly ONE argument group, so this one stays bare-resolvable after
    // `open type` (fsi-verified) — the shape review round 2 caught us hiding.
    [<System.Runtime.CompilerServices.Extension>]
    static member CurriedExt (x: int) (y: int) = x + y

// `[<Extension>]` on a GENERIC type. FCS's `IsTyconRefUsedForCSharpStyleExtensionMembers`
// requires the container to be non-generic (`isNil (tcref.Typars m)`), so this is not
// a C#-style extension container at all and its attributed static stays in unqualified
// scope: `open type GenericExtType<int>` then bare `GenExt` compiles (fsi-verified).
// Review round 3 caught us hiding it (or, on an F# assembly, deferring it).
[<System.Runtime.CompilerServices.Extension>]
type GenericExtType<'a> =

    [<System.Runtime.CompilerServices.Extension>]
    static member GenExt(x: int) = x * 5

// ===== The TIERED channel (extension_visibility_matrix.rs) =====
//
// Every shape above is probed in ONE tier, and that is a blind spot: with a single
// `open`, "we own this path and defer" and "this name is genuinely absent here"
// are indistinguishable — both resolve to nothing, so the matrix passes either way.
// BOTH review findings of PR #916's round 3 and round 4 lived in exactly that gap:
//
//   - round 3: an *undecidable* member reported itself absent, and a lower `open`
//     re-rooted the path — a WRONG TARGET;
//   - round 4: a *hidden augmentation* wrongly owned the path, and the lower `open`
//     that FCS resolves was swallowed — a LOST RESOLUTION.
//
// A second tier makes ownership observable. Each shape below is declared in
// `Demo.TierHigh` with a plain, ordinary `let`/static of the SAME name in
// `Demo.TierLow`. With `open Demo.TierLow` then `open Demo.TierHigh`, FCS's answer
// *names the owner*: it resolves the TierLow member exactly when the TierHigh shape
// is invisible to that channel, and the TierHigh one when it is not. Both failure
// modes above become a visible name mismatch, not a shared silence.

namespace Demo.TierLow

// The lower tier: every member is an ordinary value/static — always visible through
// every channel. So whenever FCS names one of these, it is saying "the higher tier
// does not own this path".
module M =
    let InstAug (s: string) = s.Length + 1000
    let StatAug (s: string) = s.Length + 1001
    let RenamedAug (s: string) = s.Length + 1002
    let TierPlain (x: int) = x + 1003

type TierType =

    static member CsStyle(x: int) = x + 1004

    static member PlainStatic(x: int) = x + 1005

// The lower tier for the **C# assembly**'s `Demo.Exts` (a real Roslyn extension
// method). This pair is the *exact* counterpart of the `TierType` one above: a
// Roslyn extension method always has exactly one argument group, so we can decide
// it is hidden — whereas an F#-declared one leaves `arg_group_count` unknowable and
// only defers. Same property, no uncertainty; it is what actually pins the
// bare-channel fall-through.
type TierCs =

    static member Doubled(x: int) = x + 1006

    static member Origin() = 1007

namespace Demo.TierHigh

// The higher tier: the same names, but as the shapes whose visibility is in
// question. An augmentation is unreachable qualified (FS0039), so `M.InstAug` must
// fall through to `Demo.TierLow.M.InstAug` — while the ordinary `TierPlain` beside
// them must NOT fall through (latest-open-wins), which is the converse guard.
module M =
    type System.String with

        member this.InstAug() = this.Length

        static member StatAug(s: string) = s.Length

        [<CompiledName("TierRenamedCompiled")>]
        member this.RenamedAug() = this.Length

    // Positive control: an ordinary `let` in the higher tier DOES own the path.
    let TierPlain (x: int) = x + 2

// A C#-style extension static is the mirror image of an augmentation: *hidden* from
// the bare channel but *reachable* qualified. So `TierType.CsStyle` must resolve
// HERE (no fall-through), while bare `CsStyle` after `open type` on both tiers must
// fall through to `Demo.TierLow.TierType.CsStyle`. One shape, opposite answers in
// the two channels — a filter keyed on the wrong thing gets one of them wrong.
[<System.Runtime.CompilerServices.Extension>]
type TierType =

    [<System.Runtime.CompilerServices.Extension>]
    static member CsStyle(x: int) = x * 6

    static member PlainStatic(x: int) = x * 7

// ===== `open <assembly module>` (docs/assembly-module-open-plan.md, Slice A) =====
//
// Opening a module of a *referenced assembly* brings its values into scope — the
// channel sema modelled for neither `open type` nor the auto-open fold. The oracle
// answers (§3 of the plan, all fsi-verified against a real referenced assembly) are
// what these shapes pin.

namespace Demo.ModuleOpen

// The plain case: an ordinary module, explicitly opened.
module Plain =
    let plainOpened (x: int) = x + 1

    // Present ONLY in this assembly: a project module of the same FQN must not suppress
    // it — FCS merges the two halves (review round 6).
    let assemblyOnlyValue (x: int) = x + 21

    // A *submodule*: reachable as a dotted head through the opened module
    // (`open Demo.ModuleOpen.Plain` then `Sub.subOpened ()`) — Q10.
    module Sub =
        let subOpened () = 20

// The cross-assembly MERGE (review round 5): the sibling `fsharp_abbrev_env` fixture
// declares this very module FQN too. FCS merges them — each assembly's unique values
// resolve, and a colliding name binds the later-referenced one. Sema does not model
// reference order, so a collision defers rather than bind the wrong assembly's value.
module Shared =
    let onlyInAutoOpenFixture () = 70
    let collidingShared () = "autoopen"

// A *childless* module whose path is ALSO a namespace in the sibling C# fixture
// (`Demo.ModuleOpen.Merged`). FCS opens and merges both halves (Q9). Childless on
// purpose: a module with nested members still blankets dotted heads until Slice B, and
// that conservatism would mask the merge this pins.
module Merged =
    let fromModuleHalf (x: int) = x + 40

// A second module with a COLLIDING value name: latest-open-wins (Q8).
module Later =
    let plainOpened (x: int) = x + 100

// `[<RequireQualifiedAccess>]`: FCS makes the `open` itself an error (FS0892) and
// imports nothing, so its values must NOT resolve bare (Q5).
[<RequireQualifiedAccess>]
module Rqa =
    let rqaOpened () = 30

// A `[<Literal>]` in an opened module: FCS brings it into bare scope (fsi-verified),
// and it used to be projected as NO MEMBER AT ALL — an invisible bare name, which is
// what proved a blacklist of "things we cannot enumerate" unsound. It is now projected
// as its static literal field, so it resolves like any other value.
module WithLiteral =
    [<Literal>]
    let TheAnswer = 42

    let alongside (x: int) = x + 9

// A `[<Struct; RequireQualifiedAccess>]` union. RQA keeps its *cases* out of bare
// scope (Q6) — but a struct union is construction-capable, so its TYPE NAME still
// occupies FCS's unqualified value slot and evicts an earlier opened value. The
// whitelist must not wave it through on RQA alone (review round 3).
module WithStructRqaUnion =
    [<Struct; RequireQualifiedAccess>]
    type Flag =
        | On
        | Off

    let besideFlag (x: int) = x + 11

// A module carrying a *pattern surface* we cannot yet enumerate (Slice C): a union
// whose cases FCS brings into bare scope (Q1). Until Slice C, opening this module
// must stay conservative — a case use must never resolve to some earlier open's
// same-named value.
module WithCases =
    type Colour =
        | Crimson
        | Viridian

    let caseless (x: int) = x + 7

// A module whose nested *constructible type* takes the bare name `Tag` — FCS puts a
// class's name in the unqualified value slot as a constructor, where it EVICTS an
// earlier opened value of the same name. Until Slice B models that slot, opening this
// module must shadow earlier opens conservatively (review of Slice A).
module WithNestedClass =
    type Tag(x: int) =
        member _.X = x

    let alsoHere (x: int) = x + 8

// A module whose nested child is an `[<AutoOpen>]` **type** — not a module (review
// round 14). `CanAutoOpenTyconRef` (NameResolution.fs) auto-opens *any* non-generic,
// F#-declared type carrying `[<AutoOpen>]`, adding its static content to the
// environment. So `open WithAutoOpenType` imports `Tag` (= 42) from the record's
// statics — a name our projection does not enumerate.
//
// fsi-verified twice: (a) the static IS imported, and (b) it lands BELOW the module's
// own vals (a `let Tag` beside it would win) but ABOVE an *earlier* open's value —
// which is exactly `ModuleOpenSurface::HiddenBelowVals`: raise the generation barrier,
// but keep this module's own vals as safe targets.
module WithAutoOpenType =
    [<AutoOpen>]
    type AutoStatics =
        { AutoField: int }
        static member Tag = 42

    let alsoHereToo (x: int) = x + 9


// The *namespace* encoding of the same FQN the abbrev fixture nests (`NestEnc.Inner`):
// a top-level module `Inner` in namespace `NestEnc`. The two encodings must merge.
namespace NestEnc

module Inner =
    let fromNamespaceEncoding () = 80

    [<Literal>]
    let DecimalConst = 1.5M


// The MODULE halves of the namespace-fold matrix's cross-kind FQNs. Each is a
// `module Demo.NsFold.<Shape>` whose namespace twin (carrying the child shape under
// test) lives in the abbrev fixture, so `open Demo.NsFold.<Shape>` is cross-kind.
// Values only (no nested types), so the dotted-head blanket stays off; the
// `mh`-prefixed value is unique to the module half, others deliberately collide.
namespace Demo.NsFold

module Exn =
    let mhExn () = 200
    // Collides with the namespace half's exception `NsExn` — a value-vs-case contest
    // in the value space; a pattern still names the exception.
    let NsExn (x: int) = x

module Union =
    let mhUnion () = 201
    // Collides with the namespace union's case `UCaseA`.
    let UCaseA () = 0

module RqaUnion =
    let mhRqa () = 202

module StructUnion =
    let mhStruct () = 203

module ClassType =
    let mhClass () = 204
    // Collides with the namespace half's `type NsClass` — value-vs-type (codex P1-A).
    let NsClass () = 0

module AutoType =
    let mhAutoType () = 205

module AutoModule =
    let mhAutoModule () = 206
    // Collides with the namespace auto-open module's `nsAutoVal`.
    let nsAutoVal () = 0

module ExnLit =
    let mhExnLit () = 208

module TierClash =
    let mhTier () = 209

module EvictA =
    let mhEvictA () = 210

module EvictB =
    let mhEvictB () = 211

module Abbrev =
    let mhAbbrev () = 207

// The ASSEMBLY module halves of the project-half matrix
// (`project_half_matrix.rs`): each `Demo.PjFold.<Shape>` FQN is also declared
// as a namespace by a PROJECT file in the test, so `open Demo.PjFold.<Shape>`
// is cross-kind with the project namespace half — the `is_project_namespace_path`
// arm of the `cross_kind` demote.
namespace Demo.PjFold

module Exn =
    let mhPjExn () = 400

module Union =
    let mhPjUnion () = 401

module AutoMod =
    let mhPjAuto () = 402
    // Collides with the project auto-open module's `pjAutoVal`.
    let pjAutoVal () = 0

module ClassShape =
    let mhPjClass () = 403
