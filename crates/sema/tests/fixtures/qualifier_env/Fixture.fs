// Test data for crates/sema's module-vs-type qualifier precedence differential
// (tests/all/resolve_qualifier_precedence_diff.rs).
//
// The two namespaces below each contribute one `Collide` candidate — a MODULE
// half and a TYPE half — so a snippet's `open QP.ModHalf` / `open QP.TypeHalf`
// pair puts a same-named module and type in scope, and `Collide.<member>`
// exercises FCS's qualifier rule (`ResolveExprLongIdentPrim`,
// NameResolution.fs): the module search runs first across ALL module
// candidates; a module reading whose in-module member lookup fails razes
// `UndefinedName` and does NOT own the path, so the type search may re-root
// it. Member names are chosen so every arm of that rule has a cell:
//
//   fromModule — module-only val:           both searches agree on the module.
//   TypeOnly   — type-only static:          the module reading must fall through.
//   Shared     — val AND static:            modules-first (the module wins even
//                                           when the type half's open is later).
//   Equals     — union case, `Object` name: in-module union-case search finds it
//                                           (never `Object.Equals` — a module
//                                           qualifier has no base chain).
//   CaseOnly   — union case only:           occupied in the module, absent on
//                                           the type.
//
// The `Gen*` children are a **generic type of each kind**, each with a matching
// TYPE-half static, exercising the child-type occupancy rule
// (`child_type_keeps_module_qualifier`) exhaustively. `nested (.., 0)` misses a
// generic child on arity, so `static_lookup` decides who owns the qualifier —
// and FCS keeps the module for every kind EXCEPT a record/union, whose bare
// name is not an expression (probed, both open orders):
//
//   GenCls        — generic class (public ctor)        -> module
//   GenClsPriv    — generic class (private ctor only)  -> module
//   GenStructCtor — generic struct (explicit ctor)     -> module
//   GenStructDef  — generic struct (implicit default)  -> module
//   GenIface      — generic interface (no ctor at all) -> module
//   GenDel        — generic delegate                   -> module
//   GenAbbr       — generic abbreviation               -> module (chases target)
//   GenRec        — generic record                     -> TYPE (falls through)
//   GenUni        — generic union                      -> TYPE (falls through)

namespace QP.ModHalf

module Collide =
    /// A union whose case names collide with `Object` member names and with the
    /// type half's statics: `Collide.Equals` / `Collide.CaseOnly` resolve
    /// through `TryFindTypeWithUnionCase` inside the module.
    type U =
        | Equals
        | CaseOnly

    // The generic-type child of each kind. FCS keeps the module qualifier for
    // all but the record and union (probed) — the kind-based occupancy rule.
    type GenCls<'a>() =
        member _.Kind = "cls"

    type GenClsPriv<'a> private (x: 'a) =
        member _.X = x

    [<Struct>]
    type GenStructCtor<'a>(x: 'a) =
        member _.X = x

    [<Struct>]
    type GenStructDef<'a> =
        struct
            val mutable X: 'a
        end

    type GenIface<'a> =
        abstract M: unit -> 'a

    type GenDel<'a> = delegate of 'a -> unit

    type GenAbbr<'a> = ResizeArray<'a>

    /// A generic **record** — its bare name is *not* an expression, so FCS falls
    /// through the module to the type half's static `GenRec`.
    type GenRec<'a> = { G: 'a }

    /// A generic **union** — likewise; falls through to the type half's static.
    type GenUni<'a> = OnlyCase of 'a

    let fromModule () = 1

    let Shared () = 2

namespace QP.TypeHalf

type Collide() =
    static member TypeOnly() = 3
    static member Shared() = 4
    static member GenCls() = 5
    static member GenClsPriv() = 6
    static member GenStructCtor() = 7
    static member GenStructDef() = 8
    static member GenIface() = 9
    static member GenDel() = 10
    static member GenAbbr() = 11
    static member GenRec() = 12
    static member GenUni() = 13
