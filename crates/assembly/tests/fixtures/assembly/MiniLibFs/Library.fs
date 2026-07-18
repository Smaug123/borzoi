// F# fixture for the assembly-reader phase-4b differential test.
//
// Each top-level definition exercises one branch of the
// `CompilationMappingAttribute` kind decoder layered on top of the
// ECMA-335 type flags:
//
// - `module Hello` → SourceConstructFlags.Module (7)
// - `type Choice` → SourceConstructFlags.SumType (1) → Union
// - `type Point` → SourceConstructFlags.RecordType (2) → Record
// - `exception MyError` → SourceConstructFlags.Exception (5)
//
// Each F#-kind compiles to a class plus a tail of compiler-generated
// accessors, override methods, and (for DUs) synthetic nested types.
// FCS hides those through `IsCompilerGenerated`; phase 4b teaches the
// Rust-side projector to agree.

namespace MiniLibFs

module Hello =
    let answer = 42
    let inc x = x + 1
    // `[<CompiledName>]` overrides the IL name. fcs-dump's
    // `projectMember` uses `CompiledName` for ALL methods (not just
    // F#-native extensions) so this round-trips against the IL.
    [<CompiledName("RenamedAtIl")>]
    let renamed x = x + 2
    // Mutable module-level `let`: compiles to a static property with
    // BOTH a getter and a setter (the backing field lives in a
    // separate `<StartupCode>` class). FCS surfaces this as a method
    // with the getter's signature, so the Rust side projects the
    // getter and ignores the setter.
    let mutable counter = 0
    // Module-level literal: compiles to a static literal field on the
    // module class (no property/method). FCS surfaces it through
    // `MembersFunctionsAndValues`, but both projectors filter literals
    // out for now; the diff agrees on the truncated member set.
    [<Literal>]
    let MaxValue = 100
    // Generic module-level `let`: projected on BOTH sides since the
    // pickle-driven member-list cutover — the Rust side reads the IL
    // generic-parameter rows, fcs-dump renders the typars from the FCS
    // public surface (name-only; method typars are invariant in IL).
    // This binding pins that the two renderings agree.
    let identity<'a> (x: 'a) : 'a = x
    // A generic `let` whose typar carries an IL-VISIBLE constraint: the
    // flexible `#seq<int>` parameter compiles to a coercion-constraint row
    // (the `array2D` shape that used to abort `fcs-dump entities` on real
    // FSharp.Core). fcs-dump's FCS-surface typar rendering is name-only, so
    // BOTH sides elide it from the differential (`isProjectableMethod` /
    // `is_unmirrorable_generic_module_method`); the owned model keeps it,
    // constraint and all (`constrained_generic_let_is_kept_in_the_owned_model`).
    let constrainedFlatten (xs: #seq<int>) = Seq.length xs
    // Nullary functions declared with explicit `()`. FCS surfaces these
    // with `CurriedParameterGroups = [|[|unit|]|]` (a single synthetic
    // `unit` parameter), while the compiled IL has zero parameters —
    // F# never emits `unit` as a real CLR parameter. The fcs-dump-side
    // strip in `renderMethodSignature` collapses the synthetic param to
    // match the IL view. `pingUnit` also pins the existing return-unit
    // collapse (`Microsoft.FSharp.Core.Unit` → `System.Void`) in the
    // same shape, but the parameter strip is what makes this fixture
    // load-bearing for the diff.
    let ping () = 1
    let pingUnit () : unit = ()
    // Negative case for the synthetic-unit strip: a *user-named* unit
    // parameter (`u: unit`) is NOT erased by F# — it compiles to a real
    // `Microsoft.FSharp.Core.Unit` IL parameter and FCS surfaces it
    // faithfully. The fcs-dump strip discriminates on
    // `FSharpParameter.Name.IsNone` (synthetic `()`) vs `Some "u"`
    // (user-named), so this binding pins the negative direction of the
    // strip: both sides must keep the Unit parameter for the diff to
    // agree.
    let pingNamed (u: unit) = 1
    // No `let _ (_: unit) = ...` fixture: F# wildcard unit params
    // compile to a real `Microsoft.FSharp.Core.Unit` IL parameter
    // (named `_arg1`), but FCS surfaces them with `Name = None` — same
    // as the synthetic `()` shape. The two are indistinguishable at the
    // FCS-symbol level, so the fcs-dump strip cannot safely handle both
    // without help from the Rust side; see the deferred-case TODO
    // in `renderMethodSignature`'s doc comment.
    do ()

type Choice =
    | Yes
    | No

type Point = { X: int; Y: int }

// A record whose canonical fields are `mutable`: FCS surfaces these
// through `FSharpField.IsMutable = true`, which translates to a field
// *without* `init_only`. The compiler emits a property setter (not
// just a writable IL field) for each mutable field, so the Rust
// side must read setter presence — not assume `init_only` — to agree.
type MutPoint = { mutable MX: int; mutable MY: int }

exception MyError of string

// A `[<Struct>]` record — same Record-kind shape as `Point` above but
// emitted as a CLR value type. The struct-ness is hidden by the F#
// kind (both sides project `EntityKind::Record`); the `is_struct`
// orthogonal flag recovers it from `extends System.ValueType` on the
// Rust side and `e.IsValueType` on the FCS side. Phase 4f covers
// the struct-record arm only — `[<Struct>] type U = Foo of int | Bar`
// (struct DUs) and `[<Struct>] type C(...) = ...` (primary-ctor
// struct classes) are follow-up slices because they expose distinct
// member projection gaps.
[<Struct>]
type SPoint = { SX: int; SY: int }

// Phase 4e: F# can also annotate types with `[<System.Obsolete>]`.
// The fixtures here stick to F# kinds the existing diff oracle already
// handles cleanly (record, union) — class fixtures (`type Foo() = ...`)
// are exercised elsewhere (see `NullableHost` below) and only became
// diff-clean after phase 4k closed the FCS-ctor-return vs IL-void gap.
// Two shapes pinned: bare and with message.
// (The on-wire CA blob is identical to the C# fixtures' shape, but
// running both halves the chance a one-sided regression slips through.)
[<System.Obsolete>]
type ObsoleteRecordFs = { Tag: string }

[<System.Obsolete("use Choice2 instead")>]
type ObsoleteUnionFs =
    | A
    | B

// Phase 4g: F# can also annotate types with
// `[<System.Diagnostics.CodeAnalysis.Experimental>]`. Same kind/shape
// constraints as the Obsolete F# fixtures (stick to record and union;
// the class-constructor gap that originally motivated this restriction
// is closed by phase 4k, but record/union still cover the attribute
// path more cheaply). Two shapes pinned: bare (id only) and with
// `UrlFormat`.
[<System.Diagnostics.CodeAnalysis.Experimental("DIAG_FS_001")>]
type ExperimentalRecordFs = { Note: string }

[<System.Diagnostics.CodeAnalysis.Experimental("DIAG_FS_002", UrlFormat = "https://example.com/{0}")>]
type ExperimentalUnionFs =
    | X
    | Y

// Phase 4i: `[<AutoOpen>]` on a module. The attribute means callers don't
// need an explicit `open MiniLibFs.Auto` — referencing the parent
// namespace is enough. The flag has no runtime effect; it's a marker that
// FCS reads to drive name resolution. Both projectors lift it onto
// `Entity::is_auto_open` and the diff harness renders it as an
// `auto_open` prefix on the entity kind (`auto_open Module`). The fixture
// uses the parameterless ctor — the `AutoOpenAttribute(path)` overload
// targets the assembly itself (`[<assembly: AutoOpen("…")>]`), never a
// TypeDef row, so we don't need to discriminate between the two.
[<AutoOpen>]
module AutoOpenModule =
    let constant = 7

// Phase 4i.2: `[<RequireQualifiedAccess>]` on a module. Callers must
// write `RqaModule.foo` — `open RqaModule` is rejected at use sites by
// the F# compiler. Pure marker, no payload. Both projectors lift it
// onto `Entity::is_require_qualified_access` and the diff harness
// renders it as the `require_qualified_access` prefix on the kind
// (`require_qualified_access Module`).
[<RequireQualifiedAccess>]
module RqaModule =
    let answer = 42

// `[<RequireQualifiedAccess>]` on a discriminated union — callers must
// write `RqaUnion.A`, never just `A`. Same flag and prefix as the
// module case; pinning both targets verifies the decoder is keyed on
// the attribute and not on the entity kind.
[<RequireQualifiedAccess>]
type RqaUnion =
    | A
    | B

// Phase 4j: the derived-impl policy cluster — `NoEquality`,
// `NoComparison`, `StructuralEquality`, `StructuralComparison`. All four
// live in `Microsoft.FSharp.Core` and are pure markers (parameterless);
// both projectors lift them onto matching `Entity::is_*` flags. The diff
// harness renders each present flag as its own snake-case prefix token
// (`no_equality`, `no_comparison`, `structural_equality`,
// `structural_comparison`), so a single fixture can pin multiple tokens
// at once.
//
// Three fixtures exercise the cluster:
//
// - `[<NoEquality; NoComparison>]` is the canonical "disable everything"
//   shape. F# requires `NoComparison` to be paired with `NoEquality`
//   (you can't have comparison without equality), so the lone-`NoComparison`
//   case isn't a legal compile target.
// - `[<StructuralEquality; StructuralComparison>]` is the explicit-opt-in
//   shape; for records this is technically redundant with the default but
//   F# accepts it and emits the attributes verbatim, so the diff pins them.
// - `[<CustomEquality; NoComparison>]` is deliberately omitted: `CustomEquality`
//   lives in the same cluster on the FCS side but is NOT decoded by either
//   projector (the four `[Self::is_*]` flags cover the auto-derived-impl
//   policy; `Custom*` signals a user-supplied implementation, a different
//   axis). Both sides ignore it, so adding such a fixture would diff clean
//   but exercise nothing — leave it for a follow-up if Custom* gains a flag.
[<NoEquality; NoComparison>]
type NoEqNoCmp = { NeData: int }

[<StructuralEquality; StructuralComparison>]
type ExplicitStructural = { EsValue: int }

// Phase 4k: `[<AllowNullLiteral>]` opts a reference type out of F#'s
// default null-prohibition (callers may then bind `null` to a value of
// this type). The only valid targets are reference-type classes and
// interfaces (F# rejects it on records, DUs, and value types). Both
// projectors lift it onto `Entity::is_allow_null_literal` and the diff
// harness renders it as the `allow_null_literal` prefix on the kind.
//
// Three shapes pin the bool-ctor-arg decode on both sides:
//
//   - `[<AllowNullLiteral>]`        — parameterless ctor (== `(true)`)
//   - `[<AllowNullLiteral(true)>]`  — explicit `(true)` overload
//   - `[<AllowNullLiteral(false)>]` — explicit `(false)` overload, the
//     deliberate *disable* shape that opts a derived class out of an
//     inherited `(true)` (see `tests/fsharp/typecheck/sigs/pos16.fs`
//     and `tests/fsharp/optimize/analyses/effects.fs` in the F#
//     compiler tree). Both projectors clear the flag for this case so
//     the diff agrees and `(false)` does *not* surface the
//     `allow_null_literal` token.
//
// Fixtures use classes with no members — just the auto-generated
// default constructor. Interfaces would be more natural targets but
// expose unrelated interface-projection gaps (no base_type on the FCS
// side, missing `abstract` flag on members) that are a separate slice.
// The class form exercises the attribute through the entity kind alone;
// the long-standing `() -> Foo` (FCS surface) vs `() -> System.Void`
// (IL truth) constructor-signature gap is closed in this slice with a
// one-branch normalisation in `fcs-dump`'s `renderMethodSignature`
// (mirroring the existing unit-as-void return-type collapse). Both
// halves now report the IL truth, so the diff agrees on these fixtures
// without filtering out the synthetic `.ctor`.
[<AllowNullLiteral>]
type NullableHost() = class end

[<AllowNullLiteral(true)>]
type NullableHostTrue() = class end

[<AllowNullLiteral(false)>]
type NullableHostFalse() = class end

// F# optional parameter. `?count` compiles to an instance method whose
// parameter is typed `FSharpOption<int>` and carries
// `[Microsoft.FSharp.Core.OptionalArgumentAttribute]`. The inner `int` is a
// value type and so must stay nullability-oblivious on both projectors.
type OptionalArgHost() =
    member _.WithOptional(?count: int) = defaultArg count 0

// Phase 6c1: `[<Measure>] type T` — a unit of measure. fsc emits an
// ECMA TypeDef row with
// `[CompilationMappingAttribute(SourceConstructFlags.Measure)]` and
// `extends System.Object` (no struct/interface signal). The F#
// signature pickle records measure-ness on `Entity.typar_kind`
// (`TyparKind::Measure`) — *not* on the repr (a real measure type
// emits a regular object-model repr; `PickledTyconRepr::Measureable`
// is the abbreviation form `[<Measure>] type T = m * kg`, which fsc
// inlines without an ECMA row). The projector merge upgrades the
// entity's kind from `Class` to `Measure`. fcs-dump emits
// `"IsMeasure": true`.
//
// (Phase 6c1 deliberately excluded `type X = Y` plain abbreviations:
// fsc inlines those at the call site without emitting any ECMA TypeDef
// row, so there's nothing in the ECMA tree to enrich. Synthesising an
// entity from the pickle alone was the silent-fallback antipattern that
// D5 rejects — until a real consumer arrived. The abbreviation-marker
// slice is that consumer: plain abbreviations now appear below
// (`IntId`/`S`) as name-only markers synthesised from the pickle.)
[<Measure>]
type m

// A second measure to confirm the diff doesn't accidentally key on the
// name.
[<Measure>]
type kg

// An attribute whose argument is `typeof<int>` — i.e. a *non-constant*
// `u_expr` in attribute-argument position. fsc pickles this into the
// signature stream (attributes on public surface are part of the
// signature data), and the un-pickler reaches it eagerly while decoding
// every entity's attribute list. Before this slice, the `u_expr`
// decoder handled only `Expr.Const` (tag 0) and hard-errored on the
// `typeof` form, which failed the *whole* CCU decode; enumeration then
// recorded skipped F# overlays and left `m`/`kg` projected as `Class`
// instead of `Measure`. This fixture pins that the overlay survives a
// `typeof` attribute argument elsewhere in the assembly: the
// `diff_assembly_minilib_fs` differential test only stays green if the
// measure kinds are still recovered with this attribute present.
// No member exposes `ty`: an exposed property would surface a member-
// projection gap (FCS hides it; the IL reader doesn't) unrelated to this
// slice. The unused primary-ctor argument still gives the `.ctor` a
// `System.Type` parameter, which is all the `typeof<int>` application
// below needs.
type CarriesTypeAttribute(ty: System.Type) =
    inherit System.Attribute()

[<CarriesType(typeof<int>)>]
type TypeofTagged = { TtField: int }

// Phase 4l (`where T : unmanaged` typar constraint) is exercised entirely
// from the C# side (`Blittable<T>` / `BlittableHost.MakeDefault<T>` in
// `MiniLib/Counter.cs`). An F# fixture (`type FsBlittable<'T when 'T :
// unmanaged>`) would emit an equivalent IL shape, but fcs-dump can't
// project F#-defined generic entities: the IL-typar surface
// (`ILGenericParameterDef`) is only reachable for IL-imported types, and
// the FCS public `FSharpGenericParameter` surface doesn't expose enough
// to mirror it. Closing that gap is a separate slice; until then,
// pinning the C# emission alone is sufficient — both compilers use the
// same canonical encoding (`value_type` bit + `IsUnmanagedAttribute` +
// `System.ValueType modreq(UnmanagedType)`), so the decoder we're
// proving here lights up on F# binaries too in production use.

// Module-suffix source name. `[<CompilationRepresentation(ModuleSuffix)>]`
// forces the compiler to append "Module" to the IL class name
// (`SuffixedModule`), exactly as it does automatically when a module shares
// its name with a type (FSharp.Core's `List` type + `List` module). F#
// recovers the source name `Suffixed` by stripping the suffix. fcs-dump
// reports the `DisplayName` (`Suffixed`); the Rust projector records
// `Entity::source_name = Some("Suffixed")` and the normaliser renders the
// FQN from it, so the differential agrees. The `[<CompiledName>]`-renamed
// `create` inside doubles as a module-member source-name pin
// (`Make` ⇒ `create`).
[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
module Suffixed =
    [<CompiledName("Make")>]
    let create (v: int) = v + 1

// An `inline` function with a statically-resolved type parameter (`^a`) emits a
// `$W` witness-passing duplicate (`addThem$W`) alongside the real `addThem`.
// Both are generic module members carrying the same source name, so the
// projector must drop the `$W` twin (FCS never surfaces it) or the name would be
// ambiguous. `addThem` itself is a generic module `let`, projected on both
// sides — its SRTP member constraint is erased from IL, and the fcs-dump
// rendering emits the same unconstrained typar, so the diff also pins the
// erased-constraint agreement.
// Type ABBREVIATIONS (`type X = Y`). fsc inlines these at every use site and
// emits NO ECMA TypeDef row — they live only in the F# signature pickle's
// `type_abbrev` field. The Rust projector synthesises a name-only
// `EntityKind::Abbreviation` marker for each from the pickle
// (`apply_abbreviation_markers`), and fcs-dump projects the identical name-only
// entity through its minimal `IsFSharpAbbreviation` branch, so both normalise to
// the same entity — which makes `diff_assembly_minilib_fs` a free oracle that the
// marker shape matches FCS's abbreviation surface. The *target* the two carry
// (`IntId` → `Microsoft.FSharp.Core.int`; `S` → `System.String`) is elided by the
// whole-tree normaliser and compared by the dedicated abbreviation-target
// differential instead. See `docs/abbreviation-target-projection-plan.md`.
//
//   - `IntId` is a referenced-assembly (FSharp.Core) primitive alias — the
//     immediate, unchased logical target is `Microsoft.FSharp.Core.int`.
//   - `S` targets a BCL type directly, so FCS renders the target `System.String`
//     (already an `AccessPath`+`LogicalName` FQN, not chased through an alias).
//   - `ObjId` is a second referenced-assembly alias (`Microsoft.FSharp.Core.obj`).
//   - `PointAlias` targets a *same-assembly* type (`Point`, above). fsc pickles
//     even that as a non-local ref back into `MiniLibFs` itself, which the decoder
//     normalises to `MiniLibFs.Point` with `ccu = None` (rendered path-only —
//     exactly as FCS does).
//   - `SelfVar<'T> = 'T` targets the abbreviation's own type parameter, decoded
//     to `Var(0)` and rendered `!T0`.
//   - `MyList<'T> = 'T list` is a *generic instantiation* — a structural shape the
//     nullary decoder slice declines (`None`), so the two-sided differential
//     asserts nothing about it (while FCS still renders it). Its marker's arity is
//     load-bearing: it must not collide with the nullary aliases above.
type IntId = int

type S = System.String

type ObjId = obj

type PointAlias = Point

type SelfVar<'T> = 'T

type MyList<'T> = 'T list

module Witness =
    let inline addThem (x: ^a) (y: ^a) : ^a = x + y
    // A real member whose compiled name merely *embeds* `$W` (it is not a
    // witness duplicate, which appends `$W`). It must survive the witness
    // filter — pins that the filter matches the `$W` *suffix*, not a substring.
    [<CompiledName("Keep$Wrapper")>]
    let keepWrapper () = 0

    // A real member whose compiled name *ends* in `$W` but has no non-witness
    // twin (there is no `Lone`). A `$W` is a witness duplicate only when it
    // shadows a real sibling, so this lone member must survive too.
    [<CompiledName("Lone$W")>]
    let loneW () = 0
