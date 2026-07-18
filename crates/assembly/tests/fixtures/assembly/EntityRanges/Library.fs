// Fixture for the entity `definition_range` overlay — the host CCU's signature
// pickle records each entity's `entity_range` (FCS's `p_entity_spec_data`
// pickles a single `p_range x.entity_range`), which the projector merge carries
// onto `Entity::definition_range` so go-to-definition can navigate a method-less
// or sequence-point-less entity.
//
// This fixture is DELIBERATELY NOT differential (its own `.fs`, not MiniLibFs's
// `Library.fs`): it defines an F#-generic entity and arity twins, and
// `fcs-dump entities` fails on F#-defined generic entities (the IL-typar surface
// is only reachable for IL-imported types — see MiniLibFs `Library.fs`), so
// these would sink `diff_assembly_minilib_fs`. The tests parse it and assert the
// projected ranges directly.
//
// Conventions (pinned against this source by the pickled_ranges oracle):
// **1-based lines, 0-based columns**, spanning exactly the source binder
// identifier.

namespace EntityRangesNs

// A module whose only member is a value — the motivating shape. Its getter
// carries no PDB sequence point, so the pickled `entity_range` is the module's
// only navigable source location.
module ValueOnly =
    let answer = 42

// A plain type (a record), not a module: pins that the range spans the `type`
// binder identifier just as it does for a module.
type ChoiceR = { Field: int }

// A method-less entity: a standalone unit-of-measure. No methods at all, so the
// token sweep finds nothing and only the range can navigate it.
[<Measure>]
type mr

// Module-suffix source name. `[<CompilationRepresentation(ModuleSuffix)>]`
// appends "Module" to the IL class name (`SuffixedRModule`); the range must
// span the *source* binder `SuffixedR`, not the IL name — pins that a renamed /
// suffixed module navigates to the right identifier.
[<CompilationRepresentation(CompilationRepresentationFlags.ModuleSuffix)>]
module SuffixedR =
    let make (v: int) = v + 1

// An exception plus an exception-*abbreviation* alias. The alias has no ECMA
// TypeDef and no method tokens, yet is a reachable `Resolution::Entity`
// (`EntityKind::Exception`), so its synthesised marker's `definition_range` is
// its only conceivable source location.
exception MyErrorR of string

exception MyErrorAliasR = MyErrorR

// Arity twins: two distinct source types whose *IL* names both strip to the
// arity-suffix-free name `ArityTwin`. F# forbids two same-named types in one
// scope, so the collision is manufactured with `[<CompiledName>]`: the
// non-generic keeps its name, the generic is compiled as `ArityTwin`1`. Both
// the collected pickle targets and the ECMA metadata rows then collapse onto
// the single name `ArityTwin`, which the range overlay must decline (D5) —
// neither twin carries a range.
type ArityTwin = { AtField: int }

[<CompiledName("ArityTwin`1")>]
type ArityTwinG<'T> = { AtGeneric: 'T }
