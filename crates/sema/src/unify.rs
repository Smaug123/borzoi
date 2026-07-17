//! The unification substrate for Phase 3 inference (the
//! [`type-checker-plan`] D8 decision): an [`ena`] union-find table plus
//! textbook structural (Robinson) unification written on top of it.
//!
//! [`InferTable`] hands out fresh inference variables ([`TyVid`]), unifies two
//! [`Ty`] terms (mutating the table so they become equal, with an occurs check
//! that forbids infinite types), and resolves a term against the solved
//! substitution. Generation (in [`crate::infer`]) produces an inert
//! `Vec<Constraint>`; the solver replays each equality through
//! [`InferTable::unify_atomic`] (so a rejected constraint leaves no trace).
//!
//! The table's value type is `Option<Ty>` — `None` for an unbound variable,
//! `Some(t)` for one bound to a term `t` (which may itself contain variables).
//! `ena` merges two values via [`ena::unify::UnifyValue`]; we make `Ty` an
//! [`EqUnifyValue`] so a `Some`/`Some` merge succeeds only on syntactic
//! equality. That branch is in fact never reached: every call here resolves
//! both operands to a *root* before unioning, so the table only ever merges an
//! unbound (`None`) root with something — structural unification of two bound
//! terms is our job, done by [`Self::unify`], not `ena`'s. This is the standard
//! way `rustc` (whence `ena` comes) layers an HM unifier over the union-find.
//!
//! [`type-checker-plan`]: ../../../docs/type-checker-plan.md

use ena::unify::{EqUnifyValue, InPlaceUnificationTable, UnifyKey};

use crate::ty::{Ty, TyVid};

impl UnifyKey for TyVid {
    type Value = Option<Ty>;

    fn index(&self) -> u32 {
        self.0
    }

    fn from_index(u: u32) -> Self {
        TyVid(u)
    }

    fn tag() -> &'static str {
        "TyVid"
    }
}

// A `Some`/`Some` merge succeeds only when the two bound terms are syntactically
// equal. Our algorithm resolves to roots before unioning, so this is never hit
// with two `Some`s in practice — but the bound is what lets `Option<Ty>` be a
// `UnifyValue`.
impl EqUnifyValue for Ty {}

/// Why a [`InferTable::unify`] failed. Best-effort inference treats either as
/// "leave the variables unsolved" (D5: say nothing), so neither surfaces a
/// diagnostic in this phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnifyError {
    /// Two incompatible concrete types: different named paths, different array
    /// ranks, or a named type against an array.
    Mismatch,
    /// The occurs check failed — binding the variable would build an infinite
    /// type (`'a = 'a[]`).
    Occurs,
}

/// The union-find table over inference variables, plus the structural unifier.
#[derive(Debug, Default)]
pub(crate) struct InferTable {
    table: InPlaceUnificationTable<TyVid>,
}

impl InferTable {
    pub(crate) fn new() -> Self {
        InferTable::default()
    }

    /// A fresh, unbound inference variable.
    pub(crate) fn fresh(&mut self) -> TyVid {
        self.table.new_key(None)
    }

    /// The number of inference variables allocated so far — the **vid mark** the
    /// generaliser takes at a binding's start (Stage 3.2c-2c). Since
    /// [`Self::fresh`] hands out indices monotonically (`fresh`'s index equals
    /// the length before it), a variable was created *during* the current binding
    /// iff its [`index`](TyVid::index) is `>= mark`. That makes the
    /// environment-freeness check trivial: an open variable inherited from an
    /// earlier binder's deferred type has a smaller index, so it is never mistaken
    /// for a freshly-introduced (generalisable) one.
    pub(crate) fn mark(&self) -> u32 {
        self.table.len() as u32
    }

    /// Whether any variable with index `< mark` shares `v`'s equivalence class —
    /// the **environment-freeness** check the generaliser uses (Stage 3.2c-2c). An
    /// open variable in a function's type is quantifiable only if no *older*
    /// variable (from an earlier binder's still-open type, allocated before this
    /// binding's `mark`) is unioned with it; such an inherited variable must defer
    /// the function rather than be captured into its scheme. Since `ena` may pick
    /// either member as a class root, this checks the class directly rather than
    /// relying on the root's index. O(`mark`) per call — fine at per-file scale.
    pub(crate) fn any_older_unioned(&mut self, v: TyVid, mark: u32) -> bool {
        (0..mark).any(|k| self.table.unioned(TyVid(k), v))
    }

    /// Whether `a` and `b` share an equivalence class (are unioned). Used by the
    /// Stage-3.3c arg-wake bookkeeping: a fired `ArgCheck` counts as discharged
    /// only if its `arg`/`dom` actually became unioned (the `Eq` held rather than
    /// rolled back), and the scheme-instantiation provenance check membership is
    /// root-aware via this.
    pub(crate) fn unioned(&mut self, a: TyVid, b: TyVid) -> bool {
        self.table.unioned(a, b)
    }

    /// Resolve `ty` one level: follow a variable through its binding (and any
    /// chain of variable-to-variable unions) to either a concrete term head
    /// (`Named` / `Array`) or a canonical *unbound* representative variable.
    fn shallow_resolve(&mut self, ty: &Ty) -> Ty {
        match ty {
            Ty::Var(v) => match self.table.probe_value(*v) {
                Some(bound) => self.shallow_resolve(&bound),
                None => Ty::Var(self.table.find(*v)),
            },
            other => other.clone(),
        }
    }

    /// Fully resolve `ty` against the current substitution, recursing into
    /// compound types. An unbound variable stays a [`Ty::Var`] (the "unknown"
    /// that the read-off in [`crate::infer`] turns into silence).
    pub(crate) fn resolve(&mut self, ty: &Ty) -> Ty {
        match self.shallow_resolve(ty) {
            Ty::Array { elem, rank } => Ty::Array {
                elem: Box::new(self.resolve(&elem)),
                rank,
            },
            Ty::Tuple(elems) => Ty::Tuple(elems.iter().map(|e| self.resolve(e)).collect()),
            Ty::Fun { arg, ret } => Ty::Fun {
                arg: Box::new(self.resolve(&arg)),
                ret: Box::new(self.resolve(&ret)),
            },
            // `Named` / `Param` (ground heads) or an unbound `Var` — nothing more
            // to do.
            resolved => resolved,
        }
    }

    /// Whether `v`'s equivalence class occurs anywhere within `ty` (after
    /// resolution). The occurs check that keeps [`Self::unify`] from binding a
    /// variable to a term containing itself.
    fn occurs(&mut self, v: TyVid, ty: &Ty) -> bool {
        match self.shallow_resolve(ty) {
            Ty::Var(v2) => self.table.unioned(v, v2),
            // A `Param` is a rigid constant, so — like a `Named` — it cannot
            // contain a variable's equivalence class.
            Ty::Named(_) | Ty::Param(_) => false,
            Ty::Array { elem, .. } => self.occurs(v, &elem),
            Ty::Tuple(elems) => elems.iter().any(|e| self.occurs(v, e)),
            Ty::Fun { arg, ret } => self.occurs(v, &arg) || self.occurs(v, &ret),
        }
    }

    /// Unify `a` and `b` atomically: on success the table reflects the merge; on
    /// failure it is rolled back to exactly its prior state. The plain
    /// [`Self::unify`] may leave *partial* bindings when a compound mismatches
    /// after an earlier sub-term unified (e.g. `'a * string` vs `int * bool`
    /// binds `'a := int` before the second element fails) — and since
    /// [`crate::infer`]'s solver discharges constraints best-effort, *ignoring*
    /// failures, an un-rolled-back partial binding would leak into read-off. So
    /// the solver unifies through this wrapper: a rejected constraint leaves no
    /// trace.
    pub(crate) fn unify_atomic(&mut self, a: &Ty, b: &Ty) -> Result<(), UnifyError> {
        self.probe(|t| t.unify(a, b))
    }

    /// Run `f` against a **scoped snapshot** of the table: keep its effects iff it
    /// returns `Ok`, otherwise roll the table back to *exactly* its prior state.
    /// This is the scoped speculation primitive (OV-4 of the overload plan): a
    /// candidate can be tested under a fresh trace and discarded on mismatch with
    /// no residue, and — since `ena` snapshots nest LIFO — `probe` calls may be
    /// nested (an outer `probe` around inner ones). [`Self::unify_atomic`] is the
    /// degenerate one-constraint case.
    ///
    /// The two laws (property-tested): a `probe` returning `Err` is
    /// **observationally the identity** on the table, and a `probe` returning `Ok`
    /// is **observationally equal to applying `f`'s mutations directly**.
    pub(crate) fn probe<T, E>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let snapshot = self.table.snapshot();
        match f(self) {
            Ok(v) => {
                self.table.commit(snapshot);
                Ok(v)
            }
            Err(e) => {
                self.table.rollback_to(snapshot);
                Err(e)
            }
        }
    }

    /// Unify `a` and `b`, mutating the table so they become equal, or report
    /// why they cannot. Structural (Robinson) unification on top of the
    /// union-find: resolve both to their roots, then
    /// - two unbound variables → union them;
    /// - a variable and a concrete term → occurs-check, then bind;
    /// - two concrete terms → recurse structurally (`Named` paths must match,
    ///   `Array` ranks must match and elements unify);
    /// - otherwise a [`UnifyError::Mismatch`].
    ///
    /// **May leave partial bindings on failure** (a compound that mismatches
    /// after an earlier sub-term bound); a caller that discards the result needs
    /// [`Self::unify_atomic`] instead.
    pub(crate) fn unify(&mut self, a: &Ty, b: &Ty) -> Result<(), UnifyError> {
        let a = self.shallow_resolve(a);
        let b = self.shallow_resolve(b);
        match (a, b) {
            (Ty::Var(va), Ty::Var(vb)) => {
                // Both unbound roots (values `None`), so the merge is
                // infallible — see the module docs.
                self.table
                    .unify_var_var(va, vb)
                    .expect("merging two unbound variables is infallible");
                Ok(())
            }
            (Ty::Var(v), term) | (term, Ty::Var(v)) => {
                if self.occurs(v, &term) {
                    return Err(UnifyError::Occurs);
                }
                // `v` is an unbound root (value `None`), so binding it cannot
                // conflict — see the module docs.
                self.table
                    .unify_var_value(v, Some(term))
                    .expect("binding an unbound variable is infallible");
                Ok(())
            }
            (Ty::Named(pa), Ty::Named(pb)) => {
                if pa == pb {
                    Ok(())
                } else {
                    Err(UnifyError::Mismatch)
                }
            }
            (Ty::Array { elem: ea, rank: ra }, Ty::Array { elem: eb, rank: rb }) => {
                if ra != rb {
                    return Err(UnifyError::Mismatch);
                }
                self.unify(&ea, &eb)
            }
            (Ty::Tuple(ea), Ty::Tuple(eb)) => {
                // Tuples unify element-wise; differing arity is a mismatch (an
                // n-tuple is not an m-tuple).
                if ea.len() != eb.len() {
                    return Err(UnifyError::Mismatch);
                }
                for (a, b) in ea.iter().zip(eb.iter()) {
                    self.unify(a, b)?;
                }
                Ok(())
            }
            (Ty::Fun { arg: aa, ret: ra }, Ty::Fun { arg: ab, ret: rb }) => {
                // Function types unify component-wise (domain with domain, range
                // with range) — the standard congruence rule.
                self.unify(&aa, &ab)?;
                self.unify(&ra, &rb)
            }
            // Two quantified parameters unify iff they are the same parameter — a
            // `Param` is a rigid constant, equal only to itself. `Param` against
            // any *other* concrete head (`Named`, `Array`, `Tuple`, `Fun`) is a
            // mismatch (the catch-all below). These arms are unreachable in
            // practice — schemes are instantiated to fresh `Var`s before their
            // bodies touch the table — but kept total (and property-tested as
            // rigid constants against the reference MGU).
            (Ty::Param(i), Ty::Param(j)) => {
                if i == j {
                    Ok(())
                } else {
                    Err(UnifyError::Mismatch)
                }
            }
            _ => Err(UnifyError::Mismatch),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Property tests for the substrate against a textbook Robinson MGU
    //! reference (gospel principle 4 / the `property-based-testing` skill): the
    //! reference *is* the spec, and the `ena`-backed table must agree with it on
    //! arbitrary constraint sequences.

    use std::collections::HashMap;

    use proptest::prelude::*;

    use super::{InferTable, UnifyError};
    use crate::ty::{Ty, TyVid};

    /// Number of inference variables both solvers share (generated terms draw
    /// `Ty::Var` indices from `0..POOL`).
    const POOL: u32 = 5;

    // ----- Reference: textbook Robinson MGU over a substitution map -----

    /// A naive most-general-unifier: a substitution `var-index -> term`, grown
    /// by binding a variable to a term whenever they unify. Obviously correct,
    /// so it is the oracle the `ena`-backed table is diffed against.
    #[derive(Default)]
    struct RefSubst {
        map: HashMap<u32, Ty>,
    }

    impl RefSubst {
        /// Follow a chain of variable bindings to its end (a concrete head or an
        /// unbound variable).
        fn walk(&self, ty: &Ty) -> Ty {
            match ty {
                Ty::Var(v) => match self.map.get(&v.index()) {
                    Some(bound) => self.walk(&bound.clone()),
                    None => ty.clone(),
                },
                other => other.clone(),
            }
        }

        fn resolve(&self, ty: &Ty) -> Ty {
            match self.walk(ty) {
                Ty::Array { elem, rank } => Ty::Array {
                    elem: Box::new(self.resolve(&elem)),
                    rank,
                },
                Ty::Tuple(elems) => Ty::Tuple(elems.iter().map(|e| self.resolve(e)).collect()),
                Ty::Fun { arg, ret } => Ty::Fun {
                    arg: Box::new(self.resolve(&arg)),
                    ret: Box::new(self.resolve(&ret)),
                },
                resolved => resolved,
            }
        }

        fn occurs(&self, v: u32, ty: &Ty) -> bool {
            match self.walk(ty) {
                Ty::Var(v2) => v2.index() == v,
                Ty::Named(_) | Ty::Param(_) => false,
                Ty::Array { elem, .. } => self.occurs(v, &elem),
                Ty::Tuple(elems) => elems.iter().any(|e| self.occurs(v, e)),
                Ty::Fun { arg, ret } => self.occurs(v, &arg) || self.occurs(v, &ret),
            }
        }

        fn unify(&mut self, a: &Ty, b: &Ty) -> Result<(), UnifyError> {
            let a = self.walk(a);
            let b = self.walk(b);
            match (a, b) {
                (Ty::Var(x), Ty::Var(y)) if x == y => Ok(()),
                (Ty::Var(x), term) | (term, Ty::Var(x)) => {
                    if self.occurs(x.index(), &term) {
                        return Err(UnifyError::Occurs);
                    }
                    self.map.insert(x.index(), term);
                    Ok(())
                }
                (Ty::Named(pa), Ty::Named(pb)) => {
                    if pa == pb {
                        Ok(())
                    } else {
                        Err(UnifyError::Mismatch)
                    }
                }
                (Ty::Array { elem: ea, rank: ra }, Ty::Array { elem: eb, rank: rb }) => {
                    if ra == rb {
                        self.unify(&ea, &eb)
                    } else {
                        Err(UnifyError::Mismatch)
                    }
                }
                (Ty::Tuple(ea), Ty::Tuple(eb)) => {
                    if ea.len() != eb.len() {
                        return Err(UnifyError::Mismatch);
                    }
                    for (a, b) in ea.iter().zip(eb.iter()) {
                        self.unify(a, b)?;
                    }
                    Ok(())
                }
                (Ty::Fun { arg: aa, ret: ra }, Ty::Fun { arg: ab, ret: rb }) => {
                    self.unify(&aa, &ab)?;
                    self.unify(&ra, &rb)
                }
                // Quantified parameters are rigid constants: equal only to the
                // same index, mismatched against anything else.
                (Ty::Param(i), Ty::Param(j)) => {
                    if i == j {
                        Ok(())
                    } else {
                        Err(UnifyError::Mismatch)
                    }
                }
                _ => Err(UnifyError::Mismatch),
            }
        }
    }

    /// Canonicalise resolved terms by renaming variables to `0, 1, 2, …` in
    /// order of first appearance across the whole list. Two solvers' free
    /// choice of equivalence-class representative then washes out, so equal
    /// canonical forms mean equal substitutions (same ground parts, same
    /// partition of the unbound variables).
    fn canonicalize(types: &[Ty]) -> Vec<Ty> {
        let mut rename: HashMap<u32, u32> = HashMap::new();
        let mut next = 0u32;
        types
            .iter()
            .map(|t| canon_one(t, &mut rename, &mut next))
            .collect()
    }

    fn canon_one(t: &Ty, rename: &mut HashMap<u32, u32>, next: &mut u32) -> Ty {
        match t {
            Ty::Var(v) => {
                let id = *rename.entry(v.index()).or_insert_with(|| {
                    let i = *next;
                    *next += 1;
                    i
                });
                Ty::Var(TyVid(id))
            }
            // A `Param` is a rigid constant, not a variable — it is not renamed
            // by first appearance (only free `Var`s are), so it passes through
            // like a `Named`.
            Ty::Named(p) => Ty::Named(p.clone()),
            Ty::Param(i) => Ty::Param(*i),
            Ty::Array { elem, rank } => Ty::Array {
                elem: Box::new(canon_one(elem, rename, next)),
                rank: *rank,
            },
            Ty::Tuple(elems) => {
                Ty::Tuple(elems.iter().map(|e| canon_one(e, rename, next)).collect())
            }
            Ty::Fun { arg, ret } => Ty::Fun {
                arg: Box::new(canon_one(arg, rename, next)),
                ret: Box::new(canon_one(ret, rename, next)),
            },
        }
    }

    // ----- Generators -----

    fn ty_strategy() -> impl Strategy<Value = Ty> {
        let leaf = prop_oneof![
            (0u32..POOL).prop_map(|v| Ty::Var(TyVid(v))),
            // Quantified parameters over a small index pool, so `Param`/`Param`
            // unify (equal-index success, distinct-index mismatch) and
            // `Param`/other (mismatch) arms get exercised as rigid constants.
            (0u32..3).prop_map(Ty::Param),
            prop::sample::select(vec![
                "System.Int32",
                "System.String",
                "System.Boolean",
                "A.B",
            ])
            .prop_map(Ty::named),
        ];
        // Nested arrays / tuples / functions around the leaves; bounded depth
        // keeps shrinking quick. Exercises the structural `Array` / `Tuple` /
        // `Fun` unify arms.
        leaf.prop_recursive(3, 24, 3, |inner| {
            prop_oneof![
                (inner.clone(), 1u32..3).prop_map(|(elem, rank)| Ty::Array {
                    elem: Box::new(elem),
                    rank,
                }),
                prop::collection::vec(inner.clone(), 2..4).prop_map(Ty::Tuple),
                (inner.clone(), inner).prop_map(|(arg, ret)| Ty::Fun {
                    arg: Box::new(arg),
                    ret: Box::new(ret),
                }),
            ]
        })
    }

    fn constraint_list() -> impl Strategy<Value = Vec<(Ty, Ty)>> {
        prop::collection::vec((ty_strategy(), ty_strategy()), 0..12)
    }

    /// A table with `POOL` fresh variables pre-allocated, so a generated
    /// `Ty::Var(TyVid(k))` (`k < POOL`) names a real table key.
    fn pooled_table() -> InferTable {
        let mut table = InferTable::new();
        for _ in 0..POOL {
            table.fresh();
        }
        table
    }

    proptest! {
        /// The headline property: for an arbitrary equality-constraint
        /// sequence, the `ena`-backed table and the reference MGU agree on which
        /// constraint (if any) is first unsatisfiable and on the resolved
        /// substitution (modulo representative renaming). The per-iteration
        /// agreement check pins the first-failure *index* — a divergence there
        /// fails the test on the spot.
        #[test]
        fn matches_reference_mgu(constraints in constraint_list()) {
            let mut table = pooled_table();
            let mut reference = RefSubst::default();

            // Replay the same sequence into both, stopping the moment a
            // constraint is rejected. With this term grammar (vars / named /
            // single-child arrays) a failing `unify` never partially mutates,
            // so the surviving substitution is exactly the successful prefix's.
            for (i, (a, b)) in constraints.iter().enumerate() {
                let table_err = table.unify(a, b).is_err();
                let ref_err = reference.unify(a, b).is_err();
                prop_assert_eq!(
                    table_err, ref_err,
                    "table/reference disagree on constraint {} ({:?} = {:?})", i, a, b
                );
                if table_err {
                    break;
                }
            }

            let pool: Vec<Ty> = (0..POOL).map(|k| Ty::Var(TyVid(k))).collect();
            let table_resolved: Vec<Ty> = pool.iter().map(|t| table.resolve(t)).collect();
            let ref_resolved: Vec<Ty> = pool.iter().map(|t| reference.resolve(t)).collect();
            prop_assert_eq!(canonicalize(&table_resolved), canonicalize(&ref_resolved));
        }

        /// Scoped-speculation law 1 (OV-4): a `probe` that returns `Err` is
        /// **observationally the identity** — the pool resolves identically
        /// before and after, whatever the speculative body did.
        #[test]
        fn probe_rollback_is_identity(pre in constraint_list(), extra in constraint_list()) {
            let mut table = pooled_table();
            for (a, b) in &pre { let _ = table.unify_atomic(a, b); }
            let pool: Vec<Ty> = (0..POOL).map(|k| Ty::Var(TyVid(k))).collect();
            let before: Vec<Ty> = pool.iter().map(|t| table.resolve(t)).collect();

            let out: Result<(), ()> = table.probe(|t| {
                for (a, b) in &extra { let _ = t.unify(a, b); }
                Err(())
            });
            prop_assert!(out.is_err());

            let after: Vec<Ty> = pool.iter().map(|t| table.resolve(t)).collect();
            prop_assert_eq!(canonicalize(&before), canonicalize(&after));
        }

        /// Scoped-speculation law 2 (OV-4): a `probe` that returns `Ok` is
        /// **observationally equal to applying its body directly** — a committing
        /// probe of the same mutation sequence lands the table in the same state.
        #[test]
        fn probe_commit_equals_direct(pre in constraint_list(), extra in constraint_list()) {
            let mut spec = pooled_table();
            let mut direct = pooled_table();
            for (a, b) in &pre {
                let _ = spec.unify_atomic(a, b);
                let _ = direct.unify_atomic(a, b);
            }
            let _: Result<(), ()> = spec.probe(|t| {
                for (a, b) in &extra { let _ = t.unify(a, b); }
                Ok(())
            });
            for (a, b) in &extra { let _ = direct.unify(a, b); }

            let pool: Vec<Ty> = (0..POOL).map(|k| Ty::Var(TyVid(k))).collect();
            let spec_r: Vec<Ty> = pool.iter().map(|t| spec.resolve(t)).collect();
            let direct_r: Vec<Ty> = pool.iter().map(|t| direct.resolve(t)).collect();
            prop_assert_eq!(canonicalize(&spec_r), canonicalize(&direct_r));
        }

        /// Unifying any term with itself succeeds (reflexivity).
        #[test]
        fn unify_is_reflexive(t in ty_strategy()) {
            let mut table = pooled_table();
            prop_assert!(table.unify(&t, &t).is_ok());
        }

        /// `unify(a, b)` succeeds iff `unify(b, a)` does (symmetry), on
        /// independent fresh tables.
        #[test]
        fn unify_is_symmetric(a in ty_strategy(), b in ty_strategy()) {
            let mut forward = pooled_table();
            let mut backward = pooled_table();
            prop_assert_eq!(forward.unify(&a, &b).is_ok(), backward.unify(&b, &a).is_ok());
        }

        /// `resolve` is idempotent, and the identity on an already-ground type.
        #[test]
        fn resolve_is_idempotent(constraints in constraint_list()) {
            let mut table = pooled_table();
            for (a, b) in &constraints {
                let _ = table.unify(a, b);
            }
            for k in 0..POOL {
                let once = table.resolve(&Ty::Var(TyVid(k)));
                let twice = table.resolve(&once);
                prop_assert_eq!(&once, &twice);
            }
        }
    }

    /// The occurs check rejects an infinite type in both solvers.
    #[test]
    fn occurs_check_rejects_infinite_type() {
        let mut table = InferTable::new();
        let v = table.fresh();
        let infinite = Ty::Array {
            elem: Box::new(Ty::Var(v)),
            rank: 1,
        };
        assert_eq!(table.unify(&Ty::Var(v), &infinite), Err(UnifyError::Occurs));

        let mut reference = RefSubst::default();
        assert_eq!(
            reference.unify(&Ty::Var(TyVid(v.index())), &infinite),
            Err(UnifyError::Occurs)
        );
    }

    /// `probe` nests LIFO: an inner `probe` that rolls back leaves the outer
    /// `probe`'s own bindings intact, and the outer commit lands them.
    #[test]
    fn probe_nests_lifo() {
        let int = Ty::named("System.Int32");
        let string = Ty::named("System.String");
        let mut table = InferTable::new();
        let a = table.fresh();
        let b = table.fresh();
        let c = table.fresh();

        let out: Result<(), ()> = table.probe(|t| {
            t.unify(&Ty::Var(a), &int).unwrap(); // outer binding
            // Inner speculation binds `b`, then rejects → rolled back.
            let inner: Result<(), ()> = t.probe(|t2| {
                t2.unify(&Ty::Var(b), &string).unwrap();
                Err(())
            });
            assert!(inner.is_err());
            t.unify(&Ty::Var(c), &string).unwrap(); // outer binding after inner
            Ok(())
        });

        assert!(out.is_ok());
        assert_eq!(table.resolve(&Ty::Var(a)), int, "outer binding committed");
        assert!(
            matches!(table.resolve(&Ty::Var(b)), Ty::Var(_)),
            "inner speculative binding rolled back"
        );
        assert_eq!(
            table.resolve(&Ty::Var(c)),
            string,
            "outer binding after the inner rollback committed"
        );
    }

    /// A chain `'a = 'b`, `'b = int` resolves `'a` to `int` — transitivity
    /// through the union-find.
    #[test]
    fn transitive_chain_resolves_to_ground() {
        let mut table = InferTable::new();
        let a = table.fresh();
        let b = table.fresh();
        table.unify(&Ty::Var(a), &Ty::Var(b)).unwrap();
        table
            .unify(&Ty::Var(b), &Ty::named("System.Int32"))
            .unwrap();
        assert_eq!(table.resolve(&Ty::Var(a)), Ty::named("System.Int32"));
    }

    /// A function type unifies component-wise: `('a -> int)` against
    /// `(bool -> 'b)` solves `'a := bool`, `'b := int`, resolving both to the
    /// ground `bool -> int`. And the occurs check reaches through a function
    /// (`'a = 'a -> int` is infinite).
    #[test]
    fn function_types_unify_component_wise() {
        let fun = |a: Ty, b: Ty| Ty::Fun {
            arg: Box::new(a),
            ret: Box::new(b),
        };

        let mut table = InferTable::new();
        let a = table.fresh();
        let b = table.fresh();
        table
            .unify(
                &fun(Ty::Var(a), Ty::named("System.Int32")),
                &fun(Ty::named("System.Boolean"), Ty::Var(b)),
            )
            .unwrap();
        let want = fun(Ty::named("System.Boolean"), Ty::named("System.Int32"));
        assert_eq!(table.resolve(&Ty::Var(a)), Ty::named("System.Boolean"));
        assert_eq!(table.resolve(&Ty::Var(b)), Ty::named("System.Int32"));
        assert_eq!(
            table.resolve(&fun(Ty::Var(a), Ty::named("System.Int32"))),
            want
        );

        let mut occ = InferTable::new();
        let v = occ.fresh();
        let infinite = fun(Ty::Var(v), Ty::named("System.Int32"));
        assert_eq!(occ.unify(&Ty::Var(v), &infinite), Err(UnifyError::Occurs));
    }

    /// `unify_atomic` rolls back the partial binding a compound mismatch leaves:
    /// unifying `('a * string)` with `(int * bool)` binds `'a := int` for the
    /// first element, then fails on the second — and `'a` must be left unbound.
    /// The non-atomic `unify` deliberately does *not* roll back (it is the
    /// structural primitive); the contrast is what the solver relies on.
    #[test]
    fn unify_atomic_rolls_back_partial_bindings() {
        let lhs = |v| Ty::Tuple(vec![Ty::Var(v), Ty::named("System.String")]);
        let rhs = Ty::Tuple(vec![Ty::named("System.Int32"), Ty::named("System.Boolean")]);

        let mut atomic = InferTable::new();
        let a = atomic.fresh();
        assert_eq!(
            atomic.unify_atomic(&lhs(a), &rhs),
            Err(UnifyError::Mismatch)
        );
        assert_eq!(atomic.resolve(&Ty::Var(a)), Ty::Var(a), "rolled back");

        let mut plain = InferTable::new();
        let b = plain.fresh();
        assert_eq!(plain.unify(&lhs(b), &rhs), Err(UnifyError::Mismatch));
        assert_eq!(
            plain.resolve(&Ty::Var(b)),
            Ty::named("System.Int32"),
            "non-atomic unify leaves the partial binding"
        );
    }
}
