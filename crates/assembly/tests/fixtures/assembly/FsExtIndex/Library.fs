// F# fixture for the OV-0.5 F#-native extension-member **name index**
// (`Entity::extension_member_names`).
//
// It augments `System.String` (a BCL type, so no sibling project reference is
// needed) with the exact instance-extension shapes the per-method
// `is_extension_method` overlay UNDER-flags — a generic-method extension and an
// optional-parameter extension (overload-resolution-plan §6.1(b)) — alongside a
// plain instance extension, a *static* extension, and a non-extension `let`.
//
// The name index reads the pickled `IsExtensionMember ∧ IsInstance` bit per val
// (recording the val's *logical* name), before any IL-method matching, so it
// must name all THREE instance extensions and NEITHER of the other two:
//
//   - `Twice`      — plain instance extension        → indexed
//   - `GenericExt` — generic-method instance ext     → indexed (overlay misses)
//   - `OptExt`     — optional-parameter instance ext → indexed (overlay misses)
//   - `StaticExt`  — static extension                → excluded (not instance)
//   - `notExtension` — plain module `let`            → excluded (not extension)
//
// The augmentations live inside a module (making them OPTIONAL type extensions,
// which cross the assembly boundary via `String.<Member>` name mangling) rather
// than at namespace level (INTRINSIC extensions, which are file-local and never
// reach metadata).

namespace FsExtIndex

module Extensions =

    type System.String with

        // Plain instance extension — the per-method overlay flags this one too.
        member this.Twice() = this + this

        // Generic-method instance extension. The overlay SKIPS generic vals
        // (they have no IL method it can key by name+arity), so
        // `is_extension_method` reads false — a false negative the name index
        // must not share.
        member this.GenericExt<'a>(y: 'a) = this.Length + 1

        // Optional-parameter instance extension. The overlay keys on IL arity,
        // which the `FSharpOption<int>`-wrapped optional shifts, so
        // `is_extension_method` reads false — another false negative.
        member this.OptExt(?n: int) = this.Length + defaultArg n 0

        // Static extension — `IsExtensionMember = true` but NOT an instance
        // member, so excluded from the instance index (matches FCS's
        // `IsInstanceMember` gate on the surface flag).
        static member StaticExt() = "static"

    // A plain module `let`, not an extension member — excluded from the index.
    let notExtension (x: int) = x + 1

// Module-open fold (Slice B): the pattern surface an `open` imports. Kept in a
// separate module so the exact-set assertions on `Extensions` stay untouched.
//
// - Active patterns compile to module methods whose IL name is the literal
//   banana form (`|Even|Odd|`, `|Positive|_|`); the projector must keep the
//   name verbatim — sema derives the pattern *tags* (`Even`, `Odd`,
//   `Positive`) by splitting it.
// - The union's case names are pickle-only (`PickledUnionCase.ident.name`);
//   `apply_union_case_names` lifts them onto `Entity::union_case_names`.
// - A union with a `static member` pickles as `UnionWithStaticFields`, the
//   second case-bearing representation — its cases must be lifted too.
module PatternSurface =

    let (|Even|Odd|) (n: int) = if n % 2 = 0 then Even else Odd

    let (|Positive|_|) (n: int) = if n > 0 then Some Positive else None

    type Verdict =
        | Accepted
        | Rejected of reason: string

    type Tallied =
        | Zero
        | Some' of int

        static member Origin = Tallied.Zero

    // A PRIVATE representation: the cases are inaccessible outside the
    // declaring scope, so a cross-assembly `open` imports none of them (codex
    // round 21). The projection must record "knowably zero accessible cases"
    // (`Some []`), never mistake it for the unknowable `None`.
    type Concealed =
        private
        | Hidden of int

    let reveal (c: Concealed) =
        match c with
        | Hidden n -> n

    // An exception ABBREVIATION: `PatternAlias` has no ECMA TypeDef of its own
    // (fsc emits nothing — the alias lives only in the pickle), yet FCS folds
    // it into an `open`'s value AND pattern scope as a constructor. The merge
    // must synthesize a marker child so the fold can see the name (codex
    // round 22).
    exception PatternProblem of int
    exception PatternAlias = PatternProblem

    // An `[<AutoOpen>]` type ABBREVIATION: opening the module imports the
    // TARGET's static content. The marker must carry the attribute so the
    // fold treats it as an auto-open type (residue), not a plain abbreviation
    // (codex round 22).
    [<AutoOpen>]
    type TalliedAlias = Tallied

    // ARITY-OVERLOADED unions: two legal `Ambig` types whose CLR names differ
    // only by the arity suffix (`Ambig` / `Ambig\`1`), which `strip_arity`
    // collapses. The case overlay must key the final segment by (name, arity)
    // or one union receives the other's cases (codex round 24).
    type Ambig = AmbigA

    [<RequireQualifiedAccess>]
    type Ambig<'T> = AmbigB of 'T
