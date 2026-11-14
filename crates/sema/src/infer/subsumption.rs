//! Which types admit no coercion, so that an equality is FCS's own relation,
//! and which types F# loosens a use against.

use super::*;

impl<'a> Gen<'a> {
    /// Whether `dom` (an application's domain variable) resolves to a
    /// **no-subsumption** type, against which a suspended [`Constraint::ArgCheck`]
    /// may discharge as a genuine `Eq(arg, dom)` (Stage 3.3c). A no-subsumption
    /// type is one F# admits **no coercion into**, so relating the argument to it
    /// by equality is exactly what FCS does:
    ///
    /// - a **sealed BCL primitive** — the numeric primitives plus `Boolean`,
    ///   `String`, `Char`, and `Decimal` (the same set [`literal_ty`] produces);
    /// - a **tuple** whose elements are all no-subsumption (recursively);
    /// - a sealed generic application, and a **project union or record**
    ///   ([`FileFacts::sealed_project_types`]), which F# seals;
    /// - an **unbound root that is a scheme-instantiation variable of ours**
    ///   ([`Solver::scheme_inst_vars`], checked against union-find roots) — a
    ///   quantified typar of one of our own schemes admits no coercion.
    ///
    /// Deliberately **excluded** (conservative — silence over a wrong type, D5):
    /// `System.Object` (subsumption target for every type), named types not
    /// shown sealed, any other project type (a class may have subtypes), and
    /// arrays. Each leaves the `ArgCheck` undischarged, so the application
    /// defers rather than grounding to a possibly-wrong type. A function type
    /// is the other domain a check discharges against
    /// ([`Solver::function_domain`]).
    pub(super) fn no_subsumption_domain(&mut self, scope: &Scope, dom: TyVid) -> bool {
        let resolved = self.solver.table.resolve(self.solver.tys.var(dom));
        self.no_subsumption_ty(scope, resolved)
    }

    /// The recursive core of [`Self::no_subsumption_domain`], over a **resolved**
    /// type: a sealed primitive, a tuple of such, or an unbound scheme-instantiation
    /// root. Everything else is `false`.
    fn no_subsumption_ty(&mut self, scope: &Scope, ty: Ty) -> bool {
        match self.solver.tys.kind(ty) {
            // A sealed BCL primitive; `obj` and other nullary named types are
            // not.
            TyKind::Named { path, args } if args.is_empty() => {
                is_sealed_primitive(&path.segments_in(self.facts.env.names()))
            }
            // A sealed generic application (`'a list`, `int option`, a generic
            // record or struct): a sealed type has no subtype to coerce into
            // it, and its arguments are invariant, so the equality FCS tries
            // first is its relation. Where it fails, FCS may still convert
            // (an `op_Implicit` into the type), and the failed equality
            // rolls the binding back ([`Self::verified`]).
            TyKind::Named { .. } => is_sealed(self.facts.env, self.solver.tys, ty),
            TyKind::Tuple(elems) => elems.iter().all(|&e| self.no_subsumption_ty(scope, e)),
            // An implicitly scoped type variable is an ordinary inference
            // variable while unbound, so the equality FCS tries first holds.
            TyKind::Var(v) => {
                self.solver.is_scheme_inst_root(*v) || self.is_head_typar_root(scope, *v)
            }
            // A project union or record is sealed, and relates as a sealed
            // generic application does; any other project type may be a class
            // with subtypes.
            TyKind::Project { path, args } => self
                .facts
                .sealed_project_types
                .get(path)
                .is_some_and(|arities| arities.contains(&args.len())),
            // Arrays, functions, and quantified `Param`s (never bound into the
            // table) are all excluded.
            TyKind::Array { .. } | TyKind::Fun { .. } | TyKind::Param(_) => false,
        }
    }

    /// Whether the (resolved, unbound) variable `v` is — or is unioned with —
    /// one of the current binding's implicitly scoped type variables
    /// ([`Scope::head_typars`]).
    fn is_head_typar_root(&mut self, scope: &Scope, v: TyVid) -> bool {
        let vars: Vec<TyVid> = scope.head_typars.values().copied().collect();
        vars.into_iter().any(|tv| self.solver.table.unioned(tv, v))
    }
}

pub(super) fn is_sealed_primitive(path: &[&str]) -> bool {
    matches!(
        path,
        ["System", "Int32"]
            | ["System", "SByte"]
            | ["System", "Byte"]
            | ["System", "Int16"]
            | ["System", "UInt16"]
            | ["System", "UInt32"]
            | ["System", "Int64"]
            | ["System", "UInt64"]
            | ["System", "IntPtr"]
            | ["System", "UIntPtr"]
            | ["System", "Double"]
            | ["System", "Single"]
            | ["System", "Decimal"]
            | ["System", "String"]
            | ["System", "Char"]
            | ["System", "Boolean"]
            | ["Microsoft", "FSharp", "Core", "Unit"]
    )
}

/// The [`Ty`] of a literal whose type is fixed once it is known to sit in a
/// no-expected-type position (the caller guarantees that). Nearly total over
/// literal kinds — the position, not the kind, is what makes it sound — except
/// the few whose type is not fixed even in isolation.
/// F#'s `unit`, as FCS names it.
pub(super) const UNIT: &str = "Microsoft.FSharp.Core.Unit";

/// [`UNIT`]'s segments.
pub(super) const UNIT_PATH: [&str; 4] = ["Microsoft", "FSharp", "Core", "Unit"];

/// `System.Object`, the bound of a loosening every type meets.
pub(super) const OBJECT_PATH: [&str; 2] = ["System", "Object"];

/// Whether `ty` is a function taking a parameter — of any curried group, or
/// any element of a tupled one — of a known type that is not sealed: one FCS
/// loosens at a use of a value of this type, if it knows the type there
/// ([`Gen::value_use`]). A parameter whose type is still a variable is not.
/// Whether, in the table `t`, every bounded loosened variable is still
/// unbound and no undecided use takes a parameter of a type that is not
/// sealed ([`Gen::verified`]).
pub(super) fn loosened_as_fcs(
    env: &AssemblyEnv,
    tys: &TyTable,
    t: &mut InferTable,
    bounded: &[TyVid],
    undecided: &[TyVid],
) -> bool {
    bounded
        .iter()
        .all(|&v| matches!(tys.kind(t.resolve(tys.var(v))), TyKind::Var(_)))
        && undecided
            .iter()
            .all(|&v| !takes_unsealed_parameter(env, tys, t.resolve(tys.var(v))))
}

fn takes_unsealed_parameter(env: &AssemblyEnv, tys: &TyTable, ty: Ty) -> bool {
    let mut ty = ty;
    while let TyKind::Fun { arg, ret } = *tys.kind(ty) {
        let unsealed = |t: Ty| !matches!(tys.kind(t), TyKind::Var(_)) && !is_sealed(env, tys, t);
        let hit = match tys.kind(arg) {
            TyKind::Tuple(elems) => elems.iter().any(|&e| unsealed(e)),
            _ => unsealed(arg),
        };
        if hit {
            return true;
        }
        ty = ret;
    }
    false
}

/// Whether FCS treats `ty` as **sealed** (`isSealedTy`), so that a use of a
/// value taking it does not loosen it ([`Gen::loosened`]). `false` whenever it
/// cannot be shown, which loosens more than FCS does and so only declines.
///
/// A type variable is not a reference type, so it is sealed, as are function,
/// tuple and array types, `unit`, value types, and F# unions, records and
/// exceptions. An IL class is sealed by its flag (an F# class carries the flag
/// exactly when it is `[<Sealed>]`); an interface never is.
pub(super) fn is_sealed(env: &AssemblyEnv, tys: &TyTable, ty: Ty) -> bool {
    match tys.kind(ty) {
        TyKind::Var(_)
        | TyKind::Param(_)
        | TyKind::Fun { .. }
        | TyKind::Tuple(_)
        | TyKind::Array { .. } => true,
        // Whether a project type is sealed turns on its kind and attributes,
        // which are not read.
        TyKind::Project { .. } => false,
        TyKind::Named { path, args } => {
            if (args.is_empty() && is_sealed_primitive(&path.segments_in(env.names())))
                || tys.is_named(env.names(), ty, &UNIT_PATH)
            {
                return true;
            }
            let path = path.spelled(env.names());
            let Some((name, namespace)) = path.split_last() else {
                return false;
            };
            // A generic application names its type by its compiled path, which
            // the source-name index does not hold for a renamed one
            // (`FSharpList`).
            let handle = if args.is_empty() {
                env.lookup_type_if_certain(namespace, name, 0)
            } else {
                env.generic_type_at_compiled_path(namespace, name, args.len())
            };
            let Some(handle) = handle else {
                return false;
            };
            let entity = env.entity(handle);
            match entity.kind {
                EntityKind::Struct
                | EntityKind::Enum
                | EntityKind::Union
                | EntityKind::Record
                | EntityKind::Exception
                | EntityKind::Delegate => true,
                EntityKind::Class => entity.is_sealed,
                EntityKind::Interface
                | EntityKind::Module
                | EntityKind::Abbreviation
                | EntityKind::Measure => false,
            }
        }
    }
}
