//! A function binding's finalisation: its rigid type variables, and its
//! generalisation to a scheme.

use super::*;

impl<'a> Gen<'a> {
    /// Whether every variable the current binding's head declares
    /// ([`Scope::rigid_typars`]) is, as solved, an unbound variable distinct
    /// from the others: FCS rejects the binding otherwise.
    fn rigid_typars_hold(&mut self, scope: &Scope) -> bool {
        let mut roots = Vec::new();
        for v in scope.rigid_typars.clone() {
            let resolved = self.solver.table.resolve(self.solver.tys.var(v));
            match self.solver.tys.kind(resolved) {
                TyKind::Var(r) if !roots.contains(r) => roots.push(*r),
                _ => return false,
            }
        }
        true
    }

    /// Whether every variable still open in the resolved type `ty` is one the
    /// current binding's head declares ([`Scope::rigid_typars`]).
    fn only_rigid_typars_open(&mut self, scope: &Scope, ty: Ty) -> bool {
        let mut open = Vec::new();
        free_vars(self.solver.tys, ty, &mut open);
        let rigid = scope.rigid_typars.clone();
        open.into_iter()
            .all(|v| rigid.iter().any(|&r| self.solver.table.unioned(r, v)))
    }

    /// Withhold the function `f_def` where the solved binding breaks a declared
    /// variable's rigidity ([`Incomplete::RigidTypeVariable`]): as solved, or
    /// with every relation it holds applied as an equality
    /// ([`Self::rigid_under_every_relation`]). A variable declared twice
    /// (`<'a, 'a>`, which FCS rejects) is one variable listed twice, so it is
    /// never distinct from itself and fails here too. Called after
    /// [`Self::solve`].
    ///
    /// FCS may then reject the declaration whole — it reports no binder of it,
    /// parameters included — so the whole binding is withheld: the function,
    /// its parameters, and everything its walk emitted. So is an
    /// **incomplete** binding with declared variables, whether or not their
    /// annotations are read (`<'a when 'a : equality>` is not): a relation the
    /// walk does not model (`p + "t"`, statically resolved) may be the one that
    /// fixes a declared variable, and nothing then rules the rejection out.
    pub(super) fn withhold_if_not_rigid(&mut self, scope: &mut Scope, f_def: Option<DefId>) {
        if !scope.declares_typars
            || (scope.complete && !scope.rigid_rejected && self.rigid_typars_hold(scope))
        {
            return;
        }
        // An incomplete binding has its cause recorded already.
        if scope.complete {
            scope.mark_incomplete(Incomplete::RigidTypeVariable);
        }
        self.out.discard_since(scope.emissions);
        self.binders.withhold(f_def);
        self.binders.withhold(scope.binding_params.iter().copied());
    }

    /// Finalise a function binding after its constraints are solved (Stage
    /// 3.2c-2c). Resolve the function's type:
    ///
    /// - **Ground** ⇒ nothing to do; [`Self::finish`] reads it back and emits it
    ///   (the monomorphic path from 3.2c-2b, e.g. `bool -> int`). Ground emission
    ///   needs no completeness/poison check (the subset argument in the module
    ///   docs), so we do not gate it.
    /// - **Open** ⇒ try to [generalise](Self::generalise) it into a scheme, iff the
    ///   binding is walk-complete and every open variable is created-this-binding
    ///   ([`Scope::mark`]) and unpoisoned. A stored scheme is emitted by `finish`; a use
    ///   [instantiates](Solver::instantiate) it. If generalisation is not sound
    ///   (incomplete, an environment/poisoned var), the binder simply stays open →
    ///   silence (D5).
    pub(super) fn finalise_function(&mut self, scope: &Scope, f_def: DefId) {
        let fv = self.def_var(f_def);
        let fn_ty = self.solver.table.resolve(self.solver.tys.var(fv));
        if self.solver.tys.is_ground(fn_ty) {
            // The monomorphic case: `finish` reads `fv` back and emits it.
            return;
        }
        if !scope.complete {
            // An incomplete binding must not generalise (its openness may be an
            // artefact of an unmodelled constraint FCS uses to ground it).
            return;
        }
        // A head that declares its variables generalises those alone: FCS
        // defaults every other variable left open to `obj`
        // (`let f<'a> (x: 'a) y` is `'a -> obj -> …`), which is not modelled.
        if !scope.rigid_typars.is_empty() && !self.only_rigid_typars_open(scope, fn_ty) {
            return;
        }
        if let Some(scheme) = self.generalise(scope, fn_ty) {
            self.binders.generalised(f_def, scheme);
        }
    }

    /// Generalise a resolved, **open** function type into a scheme, or return
    /// `None` to defer (Stage 3.2c-2c). Every open variable in `fn_ty` must be
    /// **quantifiable**: created during this binding (no variable with index
    /// `< scope.mark` is unioned with it — an environment variable inherited from an
    /// earlier binder, whose openness we do not control, must defer this function)
    /// and **not poisoned** (a dropped check-mode relation could have grounded it
    /// in FCS). If all pass, each distinct open root is replaced by a [`TyKind::Param`]
    /// numbered by **first appearance** in a depth-first, argument-before-return
    /// walk — matching FCS's canonical numbering — and the substituted body is the
    /// scheme. Parameters are never bound *into* the table, so a `debug_assert`
    /// pins that the resolved `fn_ty` carries none before we introduce them.
    pub(super) fn generalise(&mut self, scope: &Scope, fn_ty: Ty) -> Option<Ty> {
        debug_assert!(
            !contains_param(self.solver.tys, fn_ty),
            "a resolved fn_ty must not already contain a Ty::Param before generalisation"
        );
        // The poison closure: every variable reachable (after resolution) from a
        // poisoned variable. `resolve` is deep, so one pass reaches a fixpoint.
        let mut poison_roots: HashSet<TyVid> = HashSet::default();
        for v in &scope.poison {
            let resolved = self.solver.table.resolve(self.solver.tys.var(*v));
            collect_var_roots(self.solver.tys, resolved, &mut poison_roots);
        }

        // The open roots of `fn_ty`, in first-appearance (DFS) order.
        let mut order: Vec<TyVid> = Vec::new();
        collect_open_order(self.solver.tys, fn_ty, &mut order);

        // Every open root must be quantifiable, else defer the whole function.
        for &v in &order {
            if poison_roots.contains(&v) || self.solver.table.any_older_unioned(v, scope.mark) {
                return None;
            }
        }

        // Assign `Param(i)` by first appearance, then substitute.
        let mut assign: HashMap<TyVid, u32> = HashMap::default();
        for &v in &order {
            let next = assign.len() as u32;
            assign.entry(v).or_insert(next);
        }
        Some(subst_params(self.solver.tys, fn_ty, &assign))
    }
}

pub(super) fn contains_param(tys: &TyTable, ty: Ty) -> bool {
    match tys.kind(ty) {
        TyKind::Param(_) => true,
        TyKind::Var(_) => false,
        TyKind::Named { args, .. } | TyKind::Project { args, .. } | TyKind::Tuple(args) => {
            args.iter().any(|&a| contains_param(tys, a))
        }
        TyKind::Array { elem, .. } => contains_param(tys, *elem),
        TyKind::Fun { arg, ret } => contains_param(tys, *arg) || contains_param(tys, *ret),
    }
}

/// Collect the open (root) variables of a **resolved** `fn_ty` in **first
/// appearance** order under a depth-first, argument-before-return walk — the
/// order [`Gen::generalise`] numbers [`TyKind::Param`]s by, matching FCS's canonical
/// typar numbering. Duplicates are dropped (a repeated variable keeps its first
/// position), so `'a -> 'a` numbers its single variable `Param(0)` once.
fn collect_open_order(tys: &TyTable, ty: Ty, order: &mut Vec<TyVid>) {
    match tys.kind(ty) {
        TyKind::Var(v) => {
            if !order.contains(v) {
                order.push(*v);
            }
        }
        TyKind::Param(_) => {}
        TyKind::Named { args, .. } | TyKind::Project { args, .. } | TyKind::Tuple(args) => {
            args.iter().for_each(|&a| collect_open_order(tys, a, order))
        }
        TyKind::Array { elem, .. } => collect_open_order(tys, *elem, order),
        TyKind::Fun { arg, ret } => {
            collect_open_order(tys, *arg, order);
            collect_open_order(tys, *ret, order);
        }
    }
}

/// Substitute each open variable in a **resolved** term by its assigned
/// [`TyKind::Param`] index (the generalisation substitution in [`Gen::generalise`]).
/// A variable absent from `assign` cannot occur (every open root of `fn_ty` was
/// assigned), so this is total over the term.
fn subst_params(tys: &TyTable, ty: Ty, assign: &HashMap<TyVid, u32>) -> Ty {
    let all =
        |args: &[Ty]| -> Vec<Ty> { args.iter().map(|&a| subst_params(tys, a, assign)).collect() };
    match tys.kind(ty) {
        TyKind::Var(v) => match assign.get(v) {
            Some(&i) => tys.param(i),
            None => ty,
        },
        TyKind::Param(_) => ty,
        TyKind::Named { path, args } => tys.app(path.clone(), all(args)),
        TyKind::Project { path, args } => tys.project(path.clone(), all(args)),
        TyKind::Array { elem, rank } => tys.array(subst_params(tys, *elem, assign), *rank),
        TyKind::Tuple(elems) => tys.tuple(all(elems)),
        TyKind::Fun { arg, ret } => tys.fun(
            subst_params(tys, *arg, assign),
            subst_params(tys, *ret, assign),
        ),
    }
}

/// Whether `path` is a **sealed BCL primitive** — the no-subsumption named types
/// against which a Stage-3.3c [`Constraint::ArgCheck`] may discharge
/// ([`Gen::no_subsumption_domain`]). This is exactly the set of scalar primitives
/// [`literal_ty`] produces (the numeric primitives plus `Boolean`, `String`,
/// `Char`, `Decimal`) — every one sealed, so F# admits no coercion *into* it, and
/// relating an argument to it by equality is what FCS does. `System.Object`,
/// `byte[]` (a literal produces an *array*, not a named type here), and any other
/// named type are deliberately absent (subsumption is possible or unproven).
///
/// Kept in lock-step with [`literal_ty`] by a shared list would over-engineer a
/// 16-entry match; instead a unit test asserts the two agree.
/// Every variable still standing in the resolved type `ty`.
fn free_vars(tys: &TyTable, ty: Ty, out: &mut Vec<TyVid>) {
    match tys.kind(ty) {
        TyKind::Var(v) => out.push(*v),
        TyKind::Named { args, .. } | TyKind::Project { args, .. } | TyKind::Tuple(args) => {
            args.iter().for_each(|&a| free_vars(tys, a, out));
        }
        TyKind::Array { elem, .. } => free_vars(tys, *elem, out),
        TyKind::Fun { arg, ret } => {
            free_vars(tys, *arg, out);
            free_vars(tys, *ret, out);
        }
        TyKind::Param(_) => {}
    }
}
