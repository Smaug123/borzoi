/// Controlled pairs for the **F#-only typar constraint** projection
/// (`FSharpConstraints`).
///
/// `when 'T : comparison` has no IL encoding whatsoever — it lives only in the
/// F# signature pickle — so each pair's two types emit byte-identical
/// `GenericParam` rows and every IL-derived field reports both as
/// unconstrained. Only the pickle separates them, which is exactly the claim
/// under test.
///
/// This lives in its own assembly rather than beside the other F# fixtures
/// because `fcs-dump entities` refuses an F#-declared *generic* entity (FCS's
/// `IsILTycon` is false for one read out of signature data, and the raw IL row
/// the differential compares against is unreachable through the public
/// surface). The sibling fixtures are each diffed by a named test; this one is
/// not, so a generic F# type can live here without teaching the oracle a shape
/// it has no consumer for.
///
/// Nothing here is renamed — every IL name is its source name. That is
/// deliberate: a consumer must not be able to reach for the source-rename
/// guard to avoid the constraint question.
module MiniLibFsTypars

/// Carries an F#-only constraint the IL cannot express.
type Constrained<'T when 'T : comparison> = Con of 'T

/// [`Constrained`]'s unconstrained twin: same shape, same arity, same IL.
type Free<'T> = Fr of 'T

/// Two parameters, only the first constrained — so a reading that collapses an
/// entity to one answer, rather than one per position, is visible.
type ConstrainedKey<'K, 'V when 'K : comparison> = CKey of 'K * 'V

/// An **abbreviation** may carry constraints of its own, and an abbreviation
/// has no ECMA row at all: it reaches the entity model as a synthesised
/// name-only marker. A consumer that chases an alias to its target must still
/// answer for the alias's own constraints, so the marker carries its own
/// reading.
type ConstrainedAlias<'T when 'T : comparison> = 'T list

/// [`ConstrainedAlias`]'s unconstrained twin.
type FreeAlias<'T> = 'T list

/// A generic **module function**'s typar: the module-member merge reads its
/// constraints off the val's own type.
let constrainedFn<'T when 'T : comparison> (x: 'T) : 'T = x

/// [`constrainedFn`]'s unconstrained twin: same shape, same IL.
let freeFn<'T> (x: 'T) : 'T = x

/// Two typars, only the first constrained — the val's reading is per position.
let keyedFn<'K, 'V when 'K : comparison> (k: 'K) (v: 'V) : 'V = v

/// The constraints a reading records by name: `equality` alone, and both of
/// `equality` and `comparison` written together.
let equatableFn<'T when 'T : equality> (x: 'T) : 'T = x

/// See [`equatableFn`]. F# pickles this one's typar as `comparison` alone,
/// dropping the `equality` it entails.
let bothFn<'T when 'T : equality and 'T : comparison> (x: 'T) : 'T = x

/// A constraint the reading records only as present: `null`, alone and
/// beside `comparison`, which must not make the pair read as `comparison`.
let nullFn<'T when 'T : null> (x: 'T) : 'T = x

/// See [`nullFn`].
let mixedFn<'T when 'T : comparison and 'T : null> (x: 'T) : 'T = x

[<Measure>]
type m

/// A unit of measure IL erases: this compiles to `double metres(double)`, with
/// nothing in the IL to say its F# type is not `float -> float`.
let metres (x: float<m>) : float<m> = x
