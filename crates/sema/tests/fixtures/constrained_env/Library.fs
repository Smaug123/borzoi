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
