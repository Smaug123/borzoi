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
//   Gen        — generic nested type:       occupies the module-qualified name
//                                           even though an arity-0 lookup
//                                           misses it.

namespace QP.ModHalf

module Collide =
    /// A union whose case names collide with `Object` member names and with the
    /// type half's statics: `Collide.Equals` / `Collide.CaseOnly` resolve
    /// through `TryFindTypeWithUnionCase` inside the module.
    type U =
        | Equals
        | CaseOnly

    /// A generic nested type: FCS's in-module type lookup is arity-indefinite
    /// for a non-final segment, so the name is occupied at any arity.
    type Gen<'a> = { G: 'a }

    let fromModule () = 1

    let Shared () = 2

namespace QP.TypeHalf

type Collide() =
    static member TypeOnly() = 3
    static member Shared() = 4
