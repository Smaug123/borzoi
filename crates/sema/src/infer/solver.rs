//! [`Solver`]: the unification table and the constraints over its variables.

use super::*;

/// A **suspended** type constraint: one that generation does not decide where
/// it meets it, so the solver ([`Gen::settle`], [`Gen::solve`]) decides it
/// later. An equality is never one: it is unified where it is generated
/// ([`Gen::eq`]).
#[derive(Debug, Clone)]
pub(super) enum Constraint {
    /// The receiver `recv` has an instance member named `name` — of [`kind`] a data
    /// member (field / non-indexer property, Stage 3.3a) *or* a method (a
    /// single-candidate method **call**, Stage 3.3d) — whose type is `result`.
    /// **Suspended**: it discharges only once `recv` resolves to a concrete
    /// [`TyKind::Named`] head, at which point the member is looked up against the
    /// [`AssemblyEnv`] (dispatched on `kind`: `instance_data_member` vs
    /// `instance_method`) and its type — a data member's type, or a method's
    /// **return** type — bridged to a [`Ty`] and unified with `result`. A
    /// failed / ambiguous / unmodelled lookup silently drops the constraint (D5:
    /// `result` stays open, so the access defers). Each fires at most once.
    ///
    /// `use_range` is the member-name token's source range (Stage 3.3b): on a
    /// successful wake the resolved member's identity is recorded there in
    /// [`InferredFile::member_resolutions`], so the LSP can surface hover /
    /// go-to-definition on the member name. It is inert until a wake succeeds.
    ///
    /// [`kind`]: MemberAccessKind
    HasMember {
        recv: TyVid,
        name: String,
        result: TyVid,
        use_range: TextRange,
        kind: MemberAccessKind,
        /// Where FCS has checked everything the lookup reads besides the
        /// receiver: a method call's arguments, which it checks before it
        /// resolves the overload; for a data member, the access itself.
        args_by: TextSize,
    },
    /// A **suspended application argument↔parameter check** (Stage 3.3c): at a
    /// modelled application `f x`, the argument variable `arg` and the function's
    /// domain variable `dom` are related by F#'s subsumption, which this stage
    /// drops. Rather than eagerly poison both (the 3.2c-3 rule), the relation is
    /// suspended: the [`Gen::solve`] fixpoint wakes it iff the enclosing binding is
    /// walk-complete **and** `dom` resolves to a **no-subsumption** type
    /// ([`Gen::no_subsumption_domain`]) — a sealed BCL primitive, a tuple of such,
    /// or an unbound scheme-instantiation root — at which point it discharges as a
    /// genuine `Eq(arg, dom)` (via `unify_atomic`). An `ArgCheck` that never wakes,
    /// fails the gate, or whose `Eq` itself fails (ill-typed code) is
    /// **undischarged**: [`Gen::solve`] then poisons `arg`, `dom`, and the
    /// application result `r` (deferred poison), so nothing bogus generalises. `r`
    /// is carried only to drive that deferred poison; the discharge reads only
    /// `arg`/`dom`.
    ArgCheck(ArgCheck),
}

/// A pending application argument↔parameter check inside [`Gen::solve`] (Stage
/// 3.3c): the argument variable `arg`, the function domain `dom`, and the
/// application result `r` (carried only for the deferred poison). The suspended
/// form of [`Constraint::ArgCheck`], threaded through the solver's fixpoint.
#[derive(Debug, Clone, Copy)]
pub(super) struct ArgCheck {
    pub(super) arg: TyVid,
    pub(super) dom: TyVid,
    pub(super) r: TyVid,
    /// The argument's source span. FCS, which checks in source order, applies
    /// the relation while it checks the argument, so by its end at the latest.
    pub(super) span: TextRange,
}

/// The inference variables and what relates them: the unification table and
/// its snapshots, the suspended constraints, and the provenance of the
/// variables an instantiation made. Of what one file's walk accumulates, the
/// only state a later answer is computed from besides [`BinderEnv`].
///
/// It knows types and variables only: no binder, node or emission. What a
/// speculative solve ([`Gen::solve`]) must undo outside the table is
/// [`Emissions`]' to log.
pub(super) struct Solver<'a> {
    /// The session's type table ([`TyTable::session`]): every term the walk
    /// builds is an id into it. What the file publishes is copied into the
    /// env's ([`FileFacts::publish`]), and a type the env holds — an earlier file's
    /// published type, a scheme — is read through an instantiation
    /// ([`Self::instantiate`]).
    pub(super) tys: &'a TyTable,
    /// The unification table. Every equality the walk generates is already
    /// in it ([`Gen::eq`]).
    pub(super) table: InferTable<'a>,
    /// The suspended constraints still pending, except the argument checks a
    /// settle parked ([`Self::settled_args`]).
    pub(super) constraints: Vec<Constraint>,
    /// The argument checks a mid-walk [`Gen::settle`] parked, in the order
    /// it met them: they stand ahead of every argument check in
    /// [`Self::constraints`], and only [`Gen::solve`] fires one. Kept apart
    /// so a settle does not read them again: a binding of `n` statements
    /// settles `n` times.
    pub(super) settled_args: Vec<ArgCheck>,
    /// The **scheme-instantiation provenance set** (Stage 3.3c): every fresh
    /// inference variable [`Self::instantiate`] created from a [`TyKind::Param`]. An
    /// application's suspended [`Constraint::ArgCheck`] discharges only when its
    /// domain resolves to a **no-subsumption** type — a sealed BCL primitive, a
    /// tuple of such, or *an unbound root in this set*. Membership is why
    /// `let g y = id y` may safely ground `y ↔ id`'s domain: that domain is an
    /// instantiation of one of *our own* schemes, whose quantified typar admits no
    /// coercion (our generaliser only quantifies unconstrained vars). Never
    /// cleared: an entry is a permanent fact about a variable's origin. Membership
    /// is checked against union-find **roots** ([`Self::is_scheme_inst_root`]), so
    /// a var unioned with an instantiation var still counts.
    scheme_inst_vars: HashSet<TyVid>,
}

impl<'a> Solver<'a> {
    pub(super) fn new(tys: &'a TyTable) -> Self {
        Solver {
            tys,
            table: InferTable::new(tys),
            constraints: Vec::new(),
            settled_args: Vec::new(),
            scheme_inst_vars: HashSet::default(),
        }
    }

    /// Instantiate a scheme body with a **fresh** inference variable per distinct
    /// [`TyKind::Param`], memoised within this one instantiation so the same index maps
    /// to the same variable (Stage 3.2c-2c). The fresh variables are `>= ` the
    /// current binding's mark, so a use inside a later function generalises in turn
    /// (`let h x = (id, x)` ⇒ `'a -> ('b -> 'b) * 'a`).
    ///
    /// `scheme` is a type of `from`: the env's table for a scheme the env
    /// holds (an earlier file's published type, an assembly function's or
    /// case's, a project case's or record's), or the session's for one of the
    /// file's own generalised binders. The instance is the session's.
    pub(super) fn instantiate(&mut self, from: &TyTable, scheme: Ty) -> Ty {
        let mut fresh: HashMap<u32, TyVid> = HashMap::default();
        self.instantiate_rec(from, scheme, &mut fresh)
    }

    pub(super) fn instantiate_rec(
        &mut self,
        from: &TyTable,
        ty: Ty,
        fresh: &mut HashMap<u32, TyVid>,
    ) -> Ty {
        let tys = self.tys;
        let mut all = |cx: &mut Self, args: &[Ty]| -> Vec<Ty> {
            args.iter()
                .map(|&a| cx.instantiate_rec(from, a, fresh))
                .collect()
        };
        match from.kind(ty) {
            TyKind::Param(i) => {
                let v = *fresh.entry(*i).or_insert_with(|| self.table.fresh());
                // Record the provenance: this fresh var is an instantiation of a
                // quantified typar of one of our schemes (Stage 3.3c). Such a typar
                // admits no coercion, so it is a **no-subsumption** domain against
                // which a suspended `ArgCheck` may safely discharge.
                self.scheme_inst_vars.insert(v);
                tys.var(v)
            }
            TyKind::Var(v) => tys.var(*v),
            TyKind::Named { path, args } => {
                let args = all(self, args);
                tys.app(path.clone(), args)
            }
            TyKind::Project { path, args } => {
                let args = all(self, args);
                tys.project(path.clone(), args)
            }
            TyKind::Array { elem, rank } => {
                let elem = self.instantiate_rec(from, *elem, fresh);
                tys.array(elem, *rank)
            }
            TyKind::Tuple(elems) => {
                let elems = all(self, elems);
                tys.tuple(elems)
            }
            TyKind::Fun { arg, ret } => {
                let arg = self.instantiate_rec(from, *arg, fresh);
                let ret = self.instantiate_rec(from, *ret, fresh);
                tys.fun(arg, ret)
            }
        }
    }

    /// Whether the (resolved, unbound) variable `v` is — or is unioned with — a
    /// scheme-instantiation variable ([`Self::scheme_inst_vars`]), checked against
    /// union-find **roots** so a var unioned with an instantiation var still
    /// counts (Stage 3.3c). The set is small (one entry per instantiated typar per
    /// use), so the linear scan is cheap at per-binding scale.
    pub(super) fn is_scheme_inst_root(&mut self, v: TyVid) -> bool {
        self.scheme_inst_vars
            .iter()
            .any(|&iv| self.table.unioned(iv, v))
    }

    /// Whether `ac`'s domain resolves to a **function type** it may discharge
    /// against, as FCS relates them: an argument is checked against its
    /// parameter's type by an equality first and a coercion only if that
    /// fails, and a function type admits no coercion but equality (F# function
    /// types are invariant), so `Eq(arg, dom)` is FCS's own relation — given
    /// that the argument's type is the one FCS has, which a loosened use of a
    /// value makes sure of ([`Gen::value_use`]). Where the equality fixes a
    /// variable FCS bounds by a subtype constraint, [`Gen::verified`] undoes
    /// it.
    pub(super) fn function_domain(&mut self, ac: ArgCheck) -> bool {
        matches!(
            self.tys.kind(self.table.resolve(self.tys.var(ac.dom))),
            TyKind::Fun { .. }
        )
    }

    /// The open variables of `a` and `b`, as roots of their classes.
    pub(super) fn relation_roots(&mut self, a: TyVid, b: TyVid) -> HashSet<TyVid> {
        let mut roots = self.open_roots(a);
        roots.extend(self.open_roots(b));
        roots
    }

    /// The variables of `v`'s resolution, each as the root of its class.
    /// A resolution's variables are already roots, except where the table is
    /// past its budget and returns what it was given.
    pub(super) fn open_roots(&mut self, v: TyVid) -> HashSet<TyVid> {
        let ty = self.table.resolve(self.tys.var(v));
        let mut vars = HashSet::default();
        collect_var_roots(self.tys, ty, &mut vars);
        vars.into_iter().map(|v| self.table.root(v)).collect()
    }
}

impl<'a> Gen<'a> {
    /// Relate `a` and `b` by equality, unifying them now, where the walk
    /// generates the relation: what the walk reads of the table afterwards,
    /// mid-binding, already has it applied, as FCS has applied a relation by
    /// the time its checking order moves past it.
    ///
    /// The unification is atomic ([`InferTable::unify_atomic`]): one that
    /// fails binds nothing, and counts in [`Scope::eq_failures`]. Between two
    /// ground types that is what FCS does too. Where either side holds an
    /// open variable FCS's unification is not atomic — it may have bound part
    /// of the variable before it met the conflict — so after a failure the
    /// table may hold less than FCS's. A binding with a failure fires no
    /// argument check ([`Gen::verified`]) and keeps no declared type variable
    /// ([`Gen::rigid_under_every_relation`]), and a match clause whose pattern
    /// failed declines.
    pub(super) fn eq(&mut self, scope: &mut Scope, a: Ty, b: Ty) {
        if self.solver.table.unify_atomic(a, b).is_err() {
            scope.eq_failures += 1;
        }
    }
}

/// Collect the (root) variables of a **resolved** term into `out` — the poison
/// closure step in [`Gen::generalise`]. A resolved term's [`TyKind::Var`] leaves are
/// already unbound representatives (roots), so equality-membership in a
/// `HashSet<TyVid>` matches the open roots of `fn_ty`.
pub(super) fn collect_var_roots(tys: &TyTable, ty: Ty, out: &mut HashSet<TyVid>) {
    match tys.kind(ty) {
        TyKind::Var(v) => {
            out.insert(*v);
        }
        TyKind::Param(_) => {}
        TyKind::Named { args, .. } | TyKind::Project { args, .. } | TyKind::Tuple(args) => {
            args.iter().for_each(|&a| collect_var_roots(tys, a, out))
        }
        TyKind::Array { elem, .. } => collect_var_roots(tys, *elem, out),
        TyKind::Fun { arg, ret } => {
            collect_var_roots(tys, *arg, out);
            collect_var_roots(tys, *ret, out);
        }
    }
}
