namespace ConstrainedFixture

/// Carries an F#-only constraint, which has no IL encoding at all: a consumer
/// reading only `TypeParameter`'s IL-derived fields sees this as identical to
/// [`Free`]. F# rejects `Constrained<int -> int>` and recovers the annotated
/// binder to `System.Object`, so committing the written type there is a wrong
/// answer rather than a commit on an erroring line.
type Constrained<'T when 'T : comparison> = Con of 'T

/// [`Constrained`]'s unconstrained twin: same shape, same arity, same IL. It is
/// what makes a deferral on `Constrained` attributable — a bridge that declines
/// both has not seen the constraint, it has seen a generic head.
type Free<'T> = Fr of 'T

/// Only the *first* parameter is constrained, so a verdict computed per entity
/// rather than per position is visible.
type ConstrainedKey<'K, 'V when 'K : comparison> = CKey of 'K * 'V

/// Two constrained parameters — the case where every position must decline.
type BothConstrained<'A, 'B when 'A : comparison and 'B : comparison> = Both of 'A * 'B

/// Obsolete **as an error**. F# rejects an annotation naming this (FS0101) and
/// recovers the annotated binder to `System.Object`, so committing the written
/// type is a wrong answer rather than a commit on an erroring line — the same
/// grade of defect as [`Constrained`], reached through a different attribute.
[<System.Obsolete("gone", true)>]
type ErrorObsolete<'T> = EObs of 'T

/// [`ErrorObsolete`]'s twin, obsolete as a *warning*. F# types
/// `WarnObsolete<int>` as itself, so this is what makes a decline on the former
/// attributable to the `IsError` flag rather than to the mere presence of an
/// `Obsolete` attribute.
[<System.Obsolete("soon", false)>]
type WarnObsolete<'T> = WObs of 'T

/// Non-generic, so the *nullary* bridge sees it, and so it can stand as a
/// generic **argument** — the position reached by recursion rather than by the
/// head check.
[<System.Obsolete("gone", true)>]
type ErrorObsoleteAtom = EAtom of int

/// F#'s own error-level diagnostic marker. `CheckEntityAttributes` consults it
/// on the **F#-tycon** path only, so no BCL type can exhibit it and only an
/// F#-authored reference — this fixture, or any third-party F# library — can.
[<CompilerMessage("gone by message", 12001, IsError = true)>]
type ErrorMessaged<'T> = EMsg of 'T

/// The twin, carrying the same attribute at warning level. F# accepts a use, so
/// this is what makes a decline on [`ErrorMessaged`] attributable to `IsError`
/// rather than to the attribute's presence.
[<CompilerMessage("just a note", 12002)>]
type WarnMessaged<'T> = WMsg of 'T

/// Non-generic, so it reaches the nullary bridge and can stand as a generic
/// argument.
[<CompilerMessage("gone by message", 12003, IsError = true)>]
type ErrorMessagedAtom = EMsgAtom of int

/// An **abbreviation**, not a definition, so it emits no ECMA TypeDef: the
/// only record of it is the F# pickle, and resolution returns the synthetic
/// name-only marker `fsharp_pickle_merge` builds. That marker is the sole
/// carrier of these attributes, so an entity whose attributes it did not read
/// is one whose use F# may reject while every projected field says it is fine.
[<CompilerMessage("gone by message", 12004, IsError = true)>]
type ErrorMessagedAbbrev = int

/// [`ErrorMessagedAbbrev`]'s twin at warning level, so a decline on the former
/// is attributable to `IsError` and not to being an abbreviation at all.
[<CompilerMessage("just a note", 12005)>]
type WarnMessagedAbbrev = int

/// The obsolete-attribute half of the same shape.
[<System.Obsolete("gone", true)>]
type ErrorObsoleteAbbrev = int

/// Its warning-level twin.
[<System.Obsolete("soon", false)>]
type WarnObsoleteAbbrev = int

/// An abbreviation **chain** onto a warning-level one. Whether F# reports the
/// link's own attribute at a use of `ChainedToWarnObsolete` decides whether an
/// intermediate link's attributes are part of the question at all.
type ChainedToWarnObsolete = WarnObsoleteAbbrev
