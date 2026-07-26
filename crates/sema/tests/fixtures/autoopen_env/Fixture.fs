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

            // The open-shortening chain is transitive too: `open Demo.Auto`
            // auto-opens Extra → DeepFirst → Deepest, so `open DeepShorten`
            // reaches this two-levels-down plain module.
            module DeepShorten =
                let deepShortenValue () = 23

    [<AutoOpen>]
    module DeepSecond =
        let orderMarker () = 2

    // Negative control: a nested module WITHOUT [<AutoOpen>] is not opened by
    // `open Demo.Auto`, even though its parent is auto-open.
    module ChainedClosed =
        let chainedClosedValue () = 6

        // Negative control for the SHORTENING closure: the recursion runs
        // through `[<AutoOpen>]` modules only, so a plain module nested in a
        // plain module is not reachable by its short name — `open Demo.Auto;
        // open ClosedDeeper` is FS0039.
        module ClosedDeeper =
            let closedDeeperValue () = 24

    // Negative control: an *internal* nested auto-open module is not
    // accessible cross-assembly, so it must not contribute either.
    [<AutoOpen>]
    module internal ChainedInternal =
        let chainedInternalValue () = 7

        // …and neither is its nested module reachable as a shortening prefix.
        module InternalDeeper =
            let internalDeeperValue () = 25

    // The open-shortening channel with an EXPLICIT namespace prefix: after
    // `open Demo.Auto`, `open ExtraShorten` reaches
    // `Demo.Auto.Extra.ExtraShorten`. The value collides with `Extra`'s own,
    // so a missed shortening is a wrong target.
    let extraShortenTarget () = 26
    module ExtraShorten =
        let extraShortenTarget () = 27

    // PRECEDENCE control. `Demo.Auto` declares a direct `ShortenContest`
    // module too (below). FCS adds the opened namespace's own submodules
    // first and *then* recurses into its `[<AutoOpen>]` modules, and the
    // recursion's additions are layered on top — so `open Demo.Auto; open
    // ShortenContest` binds THIS one, not the direct sibling.
    module ShortenContest =
        let contestValue () = 28

// The direct half of the shortening-precedence contest: a `Demo.Auto`
// submodule sharing its name with `Extra.ShortenContest` above.
module ShortenContest =
    let contestValue () = 29

// A direct type colliding with `Extra.SameTierName` above, at the exact same
// `Demo.Auto` tier — the same-tier collision codex round 6 flagged.
type SameTierName = { DirectField: int }

// A direct type colliding with the transitively-auto-opened
// `Extra.ChainedInner.Chained` above — the nested chain's same-tier shadow.
type Chained = { DirectField: int }

namespace Demo.TwoAsm

// The **cross-assembly** half of the open-shortening contest: the abbrev
// fixture declares its own `[<AutoOpen>]` module in this same namespace, also
// nesting an `AsmPick` whose value collides with this one. Opening the
// namespace folds both modules, so `open AsmPick` names two different modules
// and the collision is settled by reference order — the dimension
// `open_shortening_matrix`'s `cross-assembly / …` cell diffs against FCS.
[<AutoOpen>]
module AsmAutoA =
    module AsmPick =
        let asmPickValue () = 30

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

    // A nested TYPE in the module-shaped target. FCS's open makes it
    // bare-visible at opens-tier priority — fsi-verified: with a same-named
    // type in the GLOBAL namespace (below), bare `DirectShadow` in both type
    // and expression position binds THIS one, not the root one. Sema does not
    // model the module-shaped open, so type-position resolution must defer
    // the name rather than commit the root decoy (a wrong target).
    type DirectShadow() =
        member _.Marker = 1

    // The decoy-free twin: `DirectOnly` names nothing anywhere else, so the
    // sound verdict is a shadowable deferral, never a clean no-match.
    type DirectOnly() =
        member _.Marker = 2

    // A nested MODULE: opening `DirectOps` also makes `DirectSub` a
    // bare-visible dotted HEAD (`DirectSub.DirectSubT`), outranking the
    // same-named global-namespace module below. Its CONTENTS are not
    // bare-visible, though — `DirectSub` is not `[<AutoOpen>]`, so a bare
    // `DirectSubT` is FS0039 (fsi-verified): the shadow surface is the
    // imported one, not the whole tree.
    module DirectSub =
        type DirectSubT() =
            member _.Marker = 3

    // An `[<AutoOpen>]` nested module: FCS opens it transitively with its
    // parent, so ITS nested type is bare-visible too (fsi-verified).
    [<AutoOpen>]
    module DirectAuto =
        type DirectAutoT() =
            member _.Marker = 4

    // A PRIVATE nested type: never importable cross-assembly, so the
    // same-named global-namespace type below stays FCS's binding for a bare
    // `DirectPrivate` (fsi-verified) — the shadow surface must not count it.
    type private DirectPrivate() =
        member _.Marker = 5

    // A nested module whose name matches NO type anywhere in the surface: a
    // module cannot bind TYPE position, so a bare annotation `DirectHeadOnly`
    // falls through to the same-named global-namespace type below
    // (fsi-verified) — the shadow surface must not veto a single-segment
    // type path on a module-only match.
    module DirectHeadOnly =
        let headOnlyInner () = 6

    // A GENERIC nested type: FCS keys bare-annotation lookup on arity, so
    // `x: DirectArity` falls through to the non-generic global-namespace
    // type below while `x: DirectArity<int>` binds THIS one (fsi-verified)
    // — the shadow surface match must compare the written arity.
    type DirectArity<'T>() =
        member _.Marker = 8

    // A generic type whose name is also a ROOT module: resolving the dotted
    // `DirectGenHead.Nested` skips this arity-1 type (a dotted type head is
    // keyed at arity 0) and binds the global module's nested type below
    // (fsi-verified) — a dotted head's surface match must be arity-0-keyed
    // for types, arityless only for modules.
    type DirectGenHead<'T>() =
        member _.Marker = 10

    // A plain `let` VALUE whose name matches a constructible class in the
    // global namespace below. A module's `let` compiles to a static *member*
    // of the module class, not to a child entity, so it is invisible to the
    // nested-entity shadow scan — correctly so for TYPE position, where a
    // value never shadows a type (`x: DirectValueShadow` binds the global
    // class). EXPRESSION position is the opposite: FCS binds this value, so a
    // bare `DirectValueShadow ()` must never commit the class.
    let DirectValueShadow () = 12

    // codex round 11: a value whose IL name differs from its F# logical name.
    // A guard that compares `SkippedMember::name` (the IL name) by equality can
    // never match the source spelling FCS imports, so the surface must report it
    // as *residue* rather than as an absent name.
    [<CompiledName("CompiledOther")>]
    let CompiledNameShadow () = 13

// ===== ModuleSuffix / companion-pair manifest targets =====
//
// FCS derefs a manifest AutoOpen path through the contributing assembly's
// entity table keyed by LOGICAL/COMPILED names
// (TypedTree.fs `AllEntitiesByCompiledAndLogicalMangledNames`) — never the
// demangled source name a *source-level* `open` resolves by. Four
// fsi-verified consequences, one shape each below. The module half of each
// companion pair is declared FIRST so a metadata-order pick lands on the
// module — the order FCS's keyed deref makes irrelevant.

// The bare spelling `AutoOpen("SemaAutoOpen.CompanionOps")` names only the
// TYPE half (the suffixed module is keyed `CompanionOpsModule`), and an F#
// type's pickled module contents are empty: FCS silently opens NOTHING —
// no FS0970, and `CompanionShadow` stays unresolvable bare (fsi-verified).
// The global decoy below is FCS's binding for the bare name.
[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
module CompanionOps =
    type CompanionShadow() =
        member _.Marker = 20

type CompanionOps() =
    member _.Marker = 21

// The COMPILED spelling `AutoOpen("SemaAutoOpen.MangledOpsModule")` is the
// key the deref table actually holds for the module half: FCS opens the
// module, and its nested type outranks the same-named global decoy in both
// type and expression position (fsi-verified).
[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
module MangledOps =
    type MangledShadow() =
        member _.Marker = 22

type MangledOps() =
    member _.Marker = 23

// A SOLO suffixed module (no companion type): its source spelling matches no
// key at all — FCS warns FS0970 and ignores the attribute (fsi-verified), so
// the global decoy stays the binding for the bare name.
[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
module SoloSuffixOps =
    type SoloSuffixShadow() =
        member _.Marker = 24

// A GENERIC companion type: its logical name is arity-mangled
// (`GenCompanionOps`1`), so the bare spelling finds neither half — FS0970
// warn-and-ignore again (fsi-verified), and the decoy stays the binding.
[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
module GenCompanionOps =
    type GenCompanionShadow() =
        member _.Marker = 25

type GenCompanionOps<'T>() =
    member _.Marker = 26

// A namespace with a type sharing the auto-opened module's nested-type name:
// an explicit `open SemaAutoOpen.ExplicitBeats` is applied AFTER the manifest
// open, so latest-open-wins binds bare `DirectShadow` HERE (fsi-verified) —
// the one reading that outranks the manifest module surface.
namespace SemaAutoOpen.ExplicitBeats

type DirectShadow() =
    member _.ExplicitMarker = 7

// An INTERNAL module named by the manifest: FCS does not import its surface
// cross-assembly (fsi-verified: with the attribute below, a bare
// `InternalShadow` still binds the global-namespace decoy), so the shadow
// veto must ignore the target entirely.
module internal InternalTarget =
    type InternalShadow() =
        member _.Marker = 9

[<assembly: AutoOpen("SemaAutoOpen.FromManifest")>]
[<assembly: AutoOpen("SemaAutoOpen.DirectOps")>]
[<assembly: AutoOpen("SemaAutoOpen.InternalTarget")>]
[<assembly: AutoOpen("SemaAutoOpen.CompanionOps")>]
[<assembly: AutoOpen("SemaAutoOpen.MangledOpsModule")>]
[<assembly: AutoOpen("SemaAutoOpen.SoloSuffixOps")>]
[<assembly: AutoOpen("SemaAutoOpen.GenCompanionOps")>]
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

// A module whose only nested entities are NON-PUBLIC. In the wild these are the
// compiler-generated closure classes that back a module's `let` values — a real
// F# module like `Fantomas.FCS.Text.Range` has a dozen of them and NO public
// nested member. A `private` nested module reproduces that shape deterministically
// (a public value beside it, so the module itself is enumerable and openable).
// `open`ing such a module seeds no dotted head we cannot model — nothing public to
// root at — so the dotted-head blanket (`opaque_dotted_open`) must NOT fire, and a
// later unrelated qualified path must still resolve. Regression:
// `resolve_autoopen.rs::opening_a_module_with_only_non_public_children_does_not_defer_unrelated_dotted_heads`.
module OnlyNonPublicNested =
    module private Hidden =
        let secret = 41

    let plainValue (x: int) = x + Hidden.secret

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

// ===== Global-namespace decoys for the manifest module-shaped AutoOpen =====
//
// Same simple names as `SemaAutoOpen.DirectOps`'s nested type/module. FCS
// binds the auto-opened module's ones (an open — even the manifest-applied
// kind — outranks the root tier; fsi-verified in both type and expression
// position), so a resolver that never searches the module surface would
// wrongly commit these. `GlobalPlain` is the negative control: no auto-open
// module tree names it, so it must keep resolving via the root tier.
namespace global

type DirectShadow() =
    member _.Decoy = 1

// The decoy for `DirectOps`'s plain `let DirectValueShadow`: a *constructible*
// class, so the expression-position constructor fallback finds it attractive.
// FCS binds the module's value instead, making this a wrong target.
type DirectValueShadow() =
    member _.Decoy = 12

// The decoy for the `[<CompiledName>]` value above: FCS imports the module's
// value under its LOGICAL name, so a bare `CompiledNameShadow ()` binds that,
// never this class.
type CompiledNameShadow() =
    member _.Decoy = 13

module DirectSub =
    type DirectSubT() =
        member _.Decoy = 2

type GlobalPlain() =
    member _.Decoy = 3

// ===== Root-namespace VALUE surfaces (no `open` required at all) =====
//
// A global union's cases are bare-visible because the union type itself is,
// with no `open` anywhere. So `GlobalCaseShadow ()` binds THIS case, not the
// same-named class below (fsi-verified) — the constructor fallback must not
// treat "no open imports it" as "no value binds it".
type GlobalUnionHost =
    | GlobalCaseShadow of unit
    | GlobalCasePlain of int

// The constructible-class decoy the case above must outrank.
type GlobalCaseShadow() =
    member _.Decoy = 20

// A `[<RequireQualifiedAccess>]` union: its cases need the type qualifier, so
// bare `RqaCaseName` does NOT bind here and the class decoy below stays
// resolvable. The veto is allowed to be conservative and defer it — but this
// records that FCS does not shadow, so an over-broad veto is visible as an
// availability loss rather than passing unnoticed.
[<RequireQualifiedAccess>]
type GlobalRqaHost =
    | RqaCaseName of unit

type RqaCaseName() =
    member _.Decoy = 21

// The decoy for `DirectOps`'s PRIVATE nested type of the same name: the
// private one is not importable, so FCS binds THIS one bare (fsi-verified) —
// the manifest-module shadow surface must let it commit.
type DirectPrivate() =
    member _.Decoy = 4

// The decoy for `DirectOps`'s nested MODULE `DirectHeadOnly`: a module cannot
// bind type position, so FCS binds THIS type for a bare annotation
// (fsi-verified) — a module-only surface match must not veto a single-segment
// type path.
type DirectHeadOnly() =
    member _.Decoy = 5

// The arity decoy: non-generic, so a bare `DirectArity` annotation binds THIS
// one (FCS keys the lookup on arity; the surface's generic `DirectArity<'T>`
// does not contest it — fsi-verified) while `DirectArity<int>` binds the
// surface's.
type DirectArity() =
    member _.Decoy = 6

// The dotted-head arity decoy: `DirectGenHead.Nested` binds THIS module's
// nested type — the surface's generic `DirectGenHead<'T>` is skipped at the
// head's arity 0 (fsi-verified).
module DirectGenHead =
    type Nested() =
        member _.Decoy = 7

// The decoy for the INTERNAL manifest target's nested type: the internal
// surface is not imported cross-assembly, so FCS binds THIS one bare
// (fsi-verified).
type InternalShadow() =
    member _.Decoy = 8

// The companion-pair decoy: the manifest's bare spelling derefs to the TYPE
// half and opens nothing, so FCS binds THIS one bare (fsi-verified).
type CompanionShadow() =
    member _.Decoy = 9

// The compiled-spelling decoy: `MangledOpsModule` IS a deref-table key, the
// module is opened, and its nested type outranks this one (fsi-verified) —
// committing this decoy would be a wrong target.
type MangledShadow() =
    member _.Decoy = 10

// The solo-suffixed decoy: the manifest's source spelling matches no key
// (FS0970 warn-and-ignore), so FCS binds THIS one bare (fsi-verified).
type SoloSuffixShadow() =
    member _.Decoy = 11

// The generic-companion decoy: the type half's logical name is arity-mangled,
// so the manifest path matches no key (FS0970) and FCS binds THIS one bare
// (fsi-verified).
type GenCompanionShadow() =
    member _.Decoy = 12

// ===== Dotted-head shadowing by an `[<AutoOpen>]` module =====
//
// A namespace holding BOTH a direct `module DottedHead` with a nested type and
// an `[<AutoOpen>]` module whose own nested `DottedHead` has a same-named
// nested type. For a file in this namespace, `DottedHead.Leaf` binds the
// AUTO-OPEN module's nested type, not the namespace's direct one — the
// auto-open surface out-ranks the namespace's direct members exactly as it
// does for a single-segment annotation (fsc-verified with a two-project probe:
// `x.FromAuto` compiles, `x.Direct` is FS0039).
//
// So a shadow check keyed only on single-segment paths misses this: the head of
// a dotted path is a bare name too, and it is where the path roots.
namespace DottedShadow

module DottedHead =
    type Leaf = { Direct : int }

[<AutoOpen>]
module DottedAuto =
    module DottedHead =
        type Leaf = { FromAuto : int }
