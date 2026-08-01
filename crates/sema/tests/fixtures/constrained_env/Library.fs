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
