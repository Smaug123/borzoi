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
//   GenCls     — generic class + static:    a bare generic *class* name IS a
//                                           constructor expression, so the module
//                                           occupies (modules-first).
//   GenRec     — generic record + static:   a bare record name is NOT a
//                                           constructor expression, so FCS falls
//                                           through to the type's static.
//   GenUni     — generic union + static:    likewise not a constructor
//                                           expression — falls through to the
//                                           type's static.

namespace QP.ModHalf

module Collide =
    /// A union whose case names collide with `Object` member names and with the
    /// type half's statics: `Collide.Equals` / `Collide.CaseOnly` resolve
    /// through `TryFindTypeWithUnionCase` inside the module.
    type U =
        | Equals
        | CaseOnly

    /// A generic **class** — `Collide.GenCls ()` is a constructor expression, so
    /// the module owns the qualifier (`nested (.., 0)` misses it on arity, but
    /// FCS's `ResolveObjectConstructorPrim` admits it).
    type GenCls<'a>() =
        member _.Kind = "cls"

    /// A generic **record** — its bare name is *not* a constructor expression, so
    /// FCS falls through the module to the type half's static `GenRec`.
    type GenRec<'a> = { G: 'a }

    /// A generic **union** — likewise not a bare constructor expression.
    type GenUni<'a> = OnlyCase of 'a

    let fromModule () = 1

    let Shared () = 2

namespace QP.TypeHalf

type Collide() =
    static member TypeOnly() = 3
    static member Shared() = 4
    static member GenCls() = 5
    static member GenRec() = 6
    static member GenUni() = 7
