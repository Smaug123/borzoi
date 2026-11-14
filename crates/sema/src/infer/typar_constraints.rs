//! Whether a type satisfies an `equality` or `comparison` constraint.

use super::*;

/// One of the two constraints [`satisfies`] decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Needs {
    Equality,
    Comparison,
}

/// What is known of a type against `equality` or `comparison`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    /// It satisfies the constraint.
    Holds,
    /// It provably does not: F# reports FS0001.
    Fails,
    /// It is on neither list.
    Undecided,
    /// It still holds a type variable, and nothing else decides it.
    Open,
}

/// Whether `ty` satisfies `needs`, by a **closed** list read off FCS's
/// `SolveTypeSupportsEquality` / `SolveTypeSupportsComparison`
/// (ConstraintSolver.fs):
///
/// - a function type fails both (`SpecialNotEquatableHeadType`; and
///   `FSharpFunc` implements no `IComparable`);
/// - a tuple or an array satisfies either exactly when its elements do
///   (`SpecialComparableHeadType`); so do `nativeint` and `unativeint`, which
///   have no elements;
/// - the sealed primitives and `string` satisfy both: none carries
///   `[<NoEquality>]` or `[<NoComparison>]`, and each implements
///   `IComparable`; `unit` satisfies equality;
/// - `obj` satisfies equality (`Object.Equals`) and fails comparison;
/// - a non-generic type of an assembly that is not F#-authored satisfies
///   equality unless it carries `[<NoEquality>]`: FCS reads it through IL, so it
///   is no F# structural type that could lack generated equality.
///
/// Everything else is [`Verdict::Undecided`]: an F# record, union or struct
/// (its equality is derived from its fields and attributes), a generic type
/// (its parameters may be `EqualityConditionalOn`), and comparison on any
/// other named type (it turns on the interface hierarchy). A type parameter
/// left in the type is [`Verdict::Open`] when nothing else decides.
pub(super) fn satisfies(env: &AssemblyEnv, tys: &TyTable, ty: Ty, needs: Needs) -> Verdict {
    match tys.kind(ty) {
        TyKind::Var(_) => Verdict::Open,
        // A rigid type parameter, which satisfies a constraint only if it
        // declares it. None reaches a binding's table today (an annotation
        // naming a type parameter declines), so this is the reading that stays
        // sound if one does.
        TyKind::Param(_) => Verdict::Undecided,
        TyKind::Fun { .. } => Verdict::Fails,
        // An F# type's equality and comparison are derived from its fields and
        // attributes, which are not read.
        TyKind::Project { .. } => Verdict::Undecided,
        TyKind::Tuple(elems) => elems.iter().fold(Verdict::Holds, |acc, &e| {
            worse(acc, satisfies(env, tys, e, needs))
        }),
        TyKind::Array { elem, .. } => satisfies(env, tys, *elem, needs),
        TyKind::Named { path, args } if args.is_empty() => {
            if is_sealed_primitive(&path.segments_in(env.names())) {
                if needs == Needs::Comparison && tys.is_named(env.names(), ty, &UNIT_PATH) {
                    return Verdict::Undecided;
                }
                return Verdict::Holds;
            }
            if tys.is_named(env.names(), ty, &OBJECT_PATH) {
                return match needs {
                    Needs::Equality => Verdict::Holds,
                    Needs::Comparison => Verdict::Fails,
                };
            }
            if needs == Needs::Comparison {
                return Verdict::Undecided;
            }
            let path = path.spelled(env.names());
            let Some((name, namespace)) = path.split_last() else {
                return Verdict::Undecided;
            };
            match env.lookup_type_if_certain(namespace, name, 0) {
                Some(handle) => {
                    let entity = env.entity(handle);
                    if entity.member_measures == MemberMeasures::NotFSharp
                        && entity.generic_parameters.is_empty()
                        && !entity.is_no_equality
                    {
                        Verdict::Holds
                    } else {
                        Verdict::Undecided
                    }
                }
                None => Verdict::Undecided,
            }
        }
        TyKind::Named { .. } => Verdict::Undecided,
    }
}

/// The worst verdict ([`satisfies`]) over `constrained`, each variable with
/// the constraints it carries, as `table` resolves them.
pub(super) fn constraint_verdict_of(
    table: &mut InferTable,
    env: &AssemblyEnv,
    tys: &TyTable,
    constrained: &[(TyVid, EqualityComparison)],
) -> Verdict {
    let mut verdict = Verdict::Holds;
    for &(v, req) in constrained {
        let ty = table.resolve(tys.var(v));
        for (needed, wanted) in [
            (Needs::Equality, req.equality),
            (Needs::Comparison, req.comparison),
        ] {
            if wanted {
                verdict = worse(verdict, satisfies(env, tys, ty, needed));
            }
        }
    }
    verdict
}

/// The verdict of a type made of parts with verdicts `a` and `b`: any part
/// failing fails it, then any undecided, then any open.
pub(super) fn worse(a: Verdict, b: Verdict) -> Verdict {
    let rank = |v| match v {
        Verdict::Fails => 3,
        Verdict::Undecided => 2,
        Verdict::Open => 1,
        Verdict::Holds => 0,
    };
    if rank(b) > rank(a) { b } else { a }
}
