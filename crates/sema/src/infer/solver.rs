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
    /// genuine `Eq(arg, dom)` (via [`Solver::unify_checked`]). An `ArgCheck` that never wakes,
    /// fails the gate, or whose `Eq` itself fails (ill-typed code) or breaks a
    /// constraint is
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
    /// The **per-variable constraints**: each instantiation variable whose
    /// type parameter carries `equality` or `comparison`, with which
    /// ([`Self::constrain`]), in the order they were made. FCS holds such a
    /// constraint on the type variable and checks it whenever a unification
    /// solves the variable; [`Self::unify_checked`] is that check, for the
    /// unifications that go through it (an argument check's discharge). The
    /// verdict reads the variable's resolution and the env
    /// ([`satisfies`]), nothing else, so it follows the variable through
    /// unions and through structure: `'T` unioned with `'a`, or bound to
    /// `'a * int`, is checked again when `'a` is bound.
    ///
    /// Never cleared, like [`Self::scheme_inst_vars`]: an entry is a fact
    /// about its variable. A check reads a window of them
    /// ([`ConstraintMark`]).
    constrained: Vec<(TyVid, EqualityComparison)>,
}

/// A position in [`Solver`]'s per-variable constraints
/// ([`Solver::constraint_mark`]): a check given one reads the constraints
/// recorded since. A binding's census answers for its own constraints, so
/// each binding checks from the mark taken at its start
/// ([`Scope::constraints`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ConstraintMark(usize);

/// Why [`Solver::unify_checked`] bound nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The two types do not unify.
    Mismatch,
    /// They unify, but the unification takes a constrained variable of the
    /// window to a strictly worse verdict that the constraint must not
    /// reach: from holding or open to provably failing
    /// ([`ConstraintGap::Violated`]) or not known either way
    /// ([`ConstraintGap::Undecided`]), or from the second to the first. It
    /// names the worst verdict reached. FCS meets that check inside the
    /// unification (`UnifyOverallType`) and undoes it.
    Constraint(ConstraintGap),
}

impl<'a> Solver<'a> {
    pub(super) fn new(tys: &'a TyTable) -> Self {
        Solver {
            tys,
            table: InferTable::new(tys),
            constraints: Vec::new(),
            settled_args: Vec::new(),
            scheme_inst_vars: HashSet::default(),
            constrained: Vec::new(),
        }
    }

    /// Record that the variable `v` must satisfy `req`, checked by every
    /// [`Self::unify_checked`] whose window holds it.
    pub(super) fn constrain(&mut self, v: TyVid, req: EqualityComparison) {
        self.constrained.push((v, req));
    }

    /// The mark from which a check reads the constraints recorded after this
    /// point.
    pub(super) fn constraint_mark(&self) -> ConstraintMark {
        ConstraintMark(self.constrained.len())
    }

    /// The worst verdict ([`satisfies`]) over the constraints recorded since
    /// `since`, in the table as it stands.
    pub(super) fn constraint_verdict(
        &mut self,
        env: &AssemblyEnv,
        since: ConstraintMark,
    ) -> Verdict {
        let constrained = &self.constrained[since.0..];
        constraint_verdict_of(&mut self.table, env, self.tys, constrained)
    }

    /// [`Self::constraint_verdict`] in the table with every one of
    /// `relations` unified too, in order, a relation that fails to unify
    /// skipped; the table is left as it was.
    pub(super) fn probed_constraint_verdict(
        &mut self,
        env: &AssemblyEnv,
        since: ConstraintMark,
        relations: &[(TyVid, TyVid)],
    ) -> Verdict {
        let constrained = &self.constrained[since.0..];
        let tys = self.tys;
        let mut verdict = Verdict::Holds;
        let _ = self.table.probe(|t| -> Result<(), ()> {
            for (a, b) in relations {
                let _ = t.unify(tys.var(*a), tys.var(*b));
            }
            verdict = constraint_verdict_of(t, env, tys, constrained);
            Err(())
        });
        verdict
    }

    /// Unify `a` and `b` as FCS unifies a variable that carries a
    /// constraint: atomically ([`InferTable::unify_atomic`]), and checking,
    /// once the unification has bound what it binds, every constraint
    /// recorded since `since` against its variable's new resolution. One the
    /// unification breaks ([`Refusal::Constraint`]) undoes it whole, as
    /// FCS's `UnifyOverallType` undoes the equality it was solving.
    ///
    /// A constrained variable is checked through whatever its resolution
    /// mentions, not only where the unification binds the variable itself.
    /// The unification binds or unions only open variables of `a`'s and
    /// `b`'s resolutions, so a constraint whose resolution mentions none of
    /// them keeps its verdict and is not read again. A constraint the table
    /// already broke before this unification is not this unification's
    /// refusal.
    pub(super) fn unify_checked(
        &mut self,
        env: &AssemblyEnv,
        a: Ty,
        b: Ty,
        since: ConstraintMark,
    ) -> Result<(), Refusal> {
        let affected = self.affected_constraints(a, b, since);
        let before: Vec<Verdict> = affected
            .iter()
            .map(|&c| constraint_verdict_of(&mut self.table, env, self.tys, &[c]))
            .collect();
        let snapshot = self.table.snapshot();
        if self.table.unify(a, b).is_err() {
            self.table.rollback_to(snapshot);
            return Err(Refusal::Mismatch);
        }
        let mut broken = Verdict::Holds;
        for (&c, &was) in affected.iter().zip(&before) {
            let now = constraint_verdict_of(&mut self.table, env, self.tys, &[c]);
            if worse(was, now) != was {
                broken = worse(broken, now);
            }
        }
        match violation(broken) {
            Some(gap) => {
                self.table.rollback_to(snapshot);
                Err(Refusal::Constraint(gap))
            }
            None => {
                self.table.commit(snapshot);
                Ok(())
            }
        }
    }

    /// The constraints recorded since `since` whose variable's resolution
    /// mentions an open variable of `a`'s or `b`'s: the only ones unifying
    /// `a` with `b` can move. A verdict ([`satisfies`]) is read only of
    /// those it returns, and telling which they are costs constant time per
    /// constraint whose variable is unbound or bound to a ground type, a
    /// resolution of its type per other constraint.
    fn affected_constraints(
        &mut self,
        a: Ty,
        b: Ty,
        since: ConstraintMark,
    ) -> Vec<(TyVid, EqualityComparison)> {
        if self.constrained.len() == since.0 {
            return Vec::new();
        }
        let tys = self.tys;
        let mut reach = HashSet::default();
        for side in [a, b] {
            let resolved = self.table.resolve(side);
            collect_var_roots(tys, resolved, &mut reach);
        }
        let reach: HashSet<TyVid> = reach.into_iter().map(|v| self.table.root(v)).collect();
        if reach.is_empty() {
            return Vec::new();
        }
        let mut affected = Vec::new();
        for &(v, req) in &self.constrained[since.0..] {
            let hit = match self.table.var_head(v) {
                Head::Root(root) => reach.contains(&root),
                Head::Term(bound) if tys.is_ground(bound) => false,
                Head::Term(bound) => {
                    let resolved = self.table.resolve(bound);
                    let mut vars = HashSet::default();
                    collect_var_roots(tys, resolved, &mut vars);
                    vars.into_iter()
                        .any(|w| reach.contains(&self.table.root(w)))
                }
            };
            if hit {
                affected.push((v, req));
            }
        }
        affected
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

#[cfg(test)]
mod tests {
    //! [`Solver::unify_checked`] against a reference that never undoes
    //! anything: a fresh table per question, replaying the unifications
    //! accepted so far, with every constraint's verdict read afresh.

    use std::cell::RefCell;

    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestRunner};

    use super::*;

    /// How many inference variables a case relates.
    const POOL: usize = 4;

    /// A type over the pool's variables, built into a session table by
    /// [`Shape::build`]. The leaves cover each verdict [`satisfies`] gives:
    /// a primitive (holds), `obj` (fails comparison), `unit` (undecided
    /// under comparison), a nominal type the env does not hold, an applied
    /// generic and a project type (undecided), a function (fails).
    #[derive(Debug, Clone)]
    enum Shape {
        Var(usize),
        Named(&'static str),
        Tuple(Vec<Shape>),
        Array(Box<Shape>),
        Fun(Box<Shape>, Box<Shape>),
        Generic(Vec<Shape>),
        Project(Vec<Shape>),
    }

    impl Shape {
        fn build(&self, env: &AssemblyEnv, tys: &TyTable, pool: &[TyVid]) -> Ty {
            let all = |shapes: &[Shape]| -> Vec<Ty> {
                shapes.iter().map(|s| s.build(env, tys, pool)).collect()
            };
            match self {
                Shape::Var(k) => tys.var(pool[*k]),
                Shape::Named(path) => tys.named(env.names(), path),
                Shape::Tuple(elems) => tys.tuple(all(elems)),
                Shape::Array(elem) => tys.array(elem.build(env, tys, pool), 1),
                Shape::Fun(arg, ret) => {
                    tys.fun(arg.build(env, tys, pool), ret.build(env, tys, pool))
                }
                Shape::Generic(args) => tys.app(TyPath::dotted(env.names(), "G.One"), all(args)),
                Shape::Project(args) => tys.project(TyPath::dotted(env.names(), "P.R"), all(args)),
            }
        }
    }

    /// A shape whose leaves are variables with weight `var_weight` out of
    /// ten, so that a run fuzzes over how much of each type is open.
    fn shape(var_weight: u32) -> impl Strategy<Value = Shape> {
        let leaf = prop_oneof![
            var_weight => (0..POOL).prop_map(Shape::Var),
            (10 - var_weight) => prop::sample::select(vec![
                "System.Int32",
                "System.String",
                "System.Object",
                "Microsoft.FSharp.Core.Unit",
                "N.Opaque",
            ])
            .prop_map(Shape::Named),
        ];
        leaf.prop_recursive(3, 12, 3, |inner| {
            prop_oneof![
                4 => prop::collection::vec(inner.clone(), 2..4).prop_map(Shape::Tuple),
                2 => inner.clone().prop_map(|e| Shape::Array(Box::new(e))),
                3 => (inner.clone(), inner.clone())
                    .prop_map(|(a, r)| Shape::Fun(Box::new(a), Box::new(r))),
                1 => prop::collection::vec(inner.clone(), 1..3).prop_map(Shape::Generic),
                1 => prop::collection::vec(inner, 1..3).prop_map(Shape::Project),
            ]
        })
    }

    /// One unification of a case: checked ([`Solver::unify_checked`]), or
    /// plain ([`InferTable::unify_atomic`]), as an equality the walk
    /// generates is today, which can leave a constraint broken.
    #[derive(Debug, Clone)]
    struct Op {
        pair: (Shape, Shape),
        checked: bool,
    }

    /// One case: the constraints, how many of them are recorded before the
    /// window's mark, and the unifications, in order.
    #[derive(Debug, Clone)]
    struct Case {
        constraints: Vec<(usize, EqualityComparison)>,
        before_mark: usize,
        ops: Vec<Op>,
    }

    fn requirement() -> impl Strategy<Value = EqualityComparison> {
        prop::sample::select(vec![(true, false), (false, true), (true, true)]).prop_map(
            |(equality, comparison)| EqualityComparison {
                equality,
                comparison,
            },
        )
    }

    /// A unification, a lone variable on one side more often than not: two
    /// arbitrary shapes rarely unify.
    fn op(var_weight: u32) -> impl Strategy<Value = Op> {
        let pair = prop_oneof![
            3 => ((0..POOL).prop_map(Shape::Var), shape(var_weight)),
            1 => (shape(var_weight), shape(var_weight)),
        ];
        (pair, prop::bool::weighted(0.8)).prop_map(|(pair, checked)| Op { pair, checked })
    }

    fn case() -> impl Strategy<Value = Case> {
        (1u32..10).prop_flat_map(|var_weight| {
            (
                prop::collection::vec((0..POOL, requirement()), 1..5),
                0usize..3,
                prop::collection::vec(op(var_weight), 1..14),
            )
                .prop_map(|(constraints, before_mark, ops)| Case {
                    before_mark: before_mark.min(constraints.len()),
                    constraints,
                    ops,
                })
        })
    }

    /// What a run saw, so it can be held to have explored each outcome.
    #[derive(Debug, Default)]
    struct Seen {
        ok: usize,
        mismatch: usize,
        violated: usize,
        undecided: usize,
        /// Refusals of a unification neither of whose sides is a variable
        /// the window constrains: the constraint was reached through a union
        /// or a structure.
        refused_indirectly: usize,
        /// Acceptances while some window constraint was already broken.
        ok_despite_broken: usize,
        /// Plain unifications, which the reference replays as well.
        plain: usize,
        /// Refusals for a constraint only a constraint outside the window
        /// would have been broken by had it been in it.
        outside_ignored: usize,
    }

    /// A fresh table with the pool's variables and `ops` unified, each of
    /// which must unify.
    fn replay<'t>(env: &AssemblyEnv, tys: &'t TyTable, ops: &[&(Shape, Shape)]) -> InferTable<'t> {
        let mut table = InferTable::new(tys);
        let pool: Vec<TyVid> = (0..POOL).map(|_| table.fresh()).collect();
        for (a, b) in ops {
            let unified = table.unify(a.build(env, tys, &pool), b.build(env, tys, &pool));
            assert!(unified.is_ok(), "an accepted unification replays");
        }
        table
    }

    /// Each of `constraints`' verdict, read afresh from `table`.
    fn verdicts(
        table: &mut InferTable,
        env: &AssemblyEnv,
        tys: &TyTable,
        constraints: &[(TyVid, EqualityComparison)],
    ) -> Vec<Verdict> {
        constraints
            .iter()
            .map(|&c| constraint_verdict_of(table, env, tys, &[c]))
            .collect()
    }

    /// Every pool variable's resolution.
    fn resolutions(table: &mut InferTable, tys: &TyTable, pool: &[TyVid]) -> Vec<Ty> {
        pool.iter().map(|&v| table.resolve(tys.var(v))).collect()
    }

    fn is_bad(v: Verdict) -> bool {
        violation(v).is_some()
    }

    /// The worst verdict `now` reaches that is both strictly worse than
    /// `was` and a violation.
    fn broken(was: &[Verdict], now: &[Verdict]) -> Option<ConstraintGap> {
        let worst = was
            .iter()
            .zip(now)
            .filter(|&(&was, &now)| worse(was, now) != was && is_bad(now))
            .fold(Verdict::Holds, |acc, (_, &now)| worse(acc, now));
        violation(worst)
    }

    /// The properties, per unification:
    ///
    /// - it is refused as a mismatch exactly when it does not unify;
    /// - otherwise it is refused for a constraint exactly when unifying it
    ///   takes some window constraint to a strictly worse verdict that is a
    ///   violation, the refusal naming the worst such;
    /// - the solver's table always equals a fresh one with only the accepted
    ///   unifications applied, so a refusal leaves no trace;
    /// - a constraint outside the window decides nothing;
    /// - a violation stays one as the table is refined (the bad verdicts are
    ///   upward closed), which is what lets the solve treat a refused check
    ///   and a violation left in the table alike.
    fn check(case: &Case, seen: &mut Seen) -> Result<(), TestCaseError> {
        let env = AssemblyEnv::default();
        let tys = TyTable::session();
        let mut solver = Solver::new(&tys);
        let pool: Vec<TyVid> = (0..POOL).map(|_| solver.table.fresh()).collect();
        let (outside, inside) = case.constraints.split_at(case.before_mark);
        for &(k, req) in outside {
            solver.constrain(pool[k], req);
        }
        let mark = solver.constraint_mark();
        for &(k, req) in inside {
            solver.constrain(pool[k], req);
        }
        let window: Vec<(TyVid, EqualityComparison)> =
            inside.iter().map(|&(k, req)| (pool[k], req)).collect();
        let all: Vec<(TyVid, EqualityComparison)> = case
            .constraints
            .iter()
            .map(|&(k, req)| (pool[k], req))
            .collect();

        let mut accepted: Vec<&(Shape, Shape)> = Vec::new();
        for op in &case.ops {
            let (a, b) = &op.pair;
            let (ta, tb) = (a.build(&env, &tys, &pool), b.build(&env, &tys, &pool));
            if !op.checked {
                if solver.table.unify_atomic(ta, tb).is_ok() {
                    accepted.push(&op.pair);
                }
                seen.plain += 1;
                continue;
            }
            let mut before_table = replay(&env, &tys, &accepted);
            let before = verdicts(&mut before_table, &env, &tys, &window);
            let before_all = verdicts(&mut before_table, &env, &tys, &all);
            let mut after_table = replay(&env, &tys, &accepted);
            let expected = if after_table.unify(ta, tb).is_err() {
                Err(Refusal::Mismatch)
            } else {
                let after = verdicts(&mut after_table, &env, &tys, &window);
                let after_all = verdicts(&mut after_table, &env, &tys, &all);
                for (&was, &now) in before_all.iter().zip(&after_all) {
                    prop_assert!(!is_bad(was) || is_bad(now), "{:?} healed to {:?}", was, now);
                }
                match broken(&before, &after) {
                    Some(gap) => Err(Refusal::Constraint(gap)),
                    None => {
                        if broken(&before_all, &after_all).is_some() {
                            seen.outside_ignored += 1;
                        }
                        Ok(())
                    }
                }
            };
            let got = solver.unify_checked(&env, ta, tb, mark);
            prop_assert_eq!(got, expected, "unifying {:?} with {:?}", a, b);
            match got {
                Ok(()) => {
                    seen.ok += 1;
                    if before.iter().any(|&v| is_bad(v)) {
                        seen.ok_despite_broken += 1;
                    }
                    accepted.push(&op.pair);
                }
                Err(Refusal::Mismatch) => seen.mismatch += 1,
                Err(Refusal::Constraint(gap)) => {
                    match gap {
                        ConstraintGap::Violated => seen.violated += 1,
                        ConstraintGap::Undecided => seen.undecided += 1,
                    }
                    let direct = |t: Ty| match tys.kind(t) {
                        TyKind::Var(v) => window.iter().any(|&(w, _)| w == *v),
                        _ => false,
                    };
                    if !direct(ta) && !direct(tb) {
                        seen.refused_indirectly += 1;
                    }
                }
            }
            let mut reference = replay(&env, &tys, &accepted);
            prop_assert_eq!(
                resolutions(&mut solver.table, &tys, &pool),
                resolutions(&mut reference, &tys, &pool),
                "the solver's table is not the accepted unifications'"
            );
        }
        Ok(())
    }

    #[test]
    fn unify_checked_refuses_exactly_what_breaks_a_constraint() {
        let seen = RefCell::new(Seen::default());
        let mut runner = TestRunner::new(Config {
            cases: 4096,
            failure_persistence: None,
            ..Config::default()
        });
        let outcome = runner.run(&case(), |case| check(&case, &mut seen.borrow_mut()));
        if let Err(e) = outcome {
            panic!("{e}");
        }
        let seen = seen.into_inner();
        // Each outcome is common in a run of this size (thousands of
        // unifications): a floor this low failing means the generator no
        // longer reaches it, not bad luck.
        for (what, n) in [
            ("accepted", seen.ok),
            ("applied plain", seen.plain),
            ("mismatched", seen.mismatch),
            ("refused as violated", seen.violated),
            ("refused as undecided", seen.undecided),
            (
                "refused through a union or structure",
                seen.refused_indirectly,
            ),
            (
                "accepted beside a constraint already broken",
                seen.ok_despite_broken,
            ),
            (
                "accepted although it breaks a constraint outside the window",
                seen.outside_ignored,
            ),
        ] {
            assert!(n >= 30, "only {n} unifications {what}: {seen:?}");
        }
    }

    /// The shape the per-variable constraints exist for: a constrained
    /// variable bound to a structure, then a variable inside it bound; and
    /// the same through a union.
    #[test]
    fn a_constraint_is_checked_through_structure_and_unions() {
        let env = AssemblyEnv::default();
        let tys = TyTable::session();
        let mut solver = Solver::new(&tys);
        let (t, a, b) = (
            solver.table.fresh(),
            solver.table.fresh(),
            solver.table.fresh(),
        );
        let mark = solver.constraint_mark();
        solver.constrain(
            t,
            EqualityComparison {
                equality: true,
                comparison: false,
            },
        );
        let int = tys.named(env.names(), "System.Int32");
        let fun = tys.fun(int, int);
        let pair = tys.tuple(vec![tys.var(a), int]);
        assert_eq!(solver.unify_checked(&env, tys.var(t), pair, mark), Ok(()));
        assert_eq!(
            solver.unify_checked(&env, tys.var(a), fun, mark),
            Err(Refusal::Constraint(ConstraintGap::Violated))
        );
        assert_eq!(solver.table.resolve(tys.var(a)), tys.var(a), "undone");
        assert_eq!(
            solver.unify_checked(&env, tys.var(a), tys.var(b), mark),
            Ok(())
        );
        assert_eq!(
            solver.unify_checked(&env, tys.var(b), fun, mark),
            Err(Refusal::Constraint(ConstraintGap::Violated))
        );
        // Outside the window, the same unification is no refusal.
        let later = solver.constraint_mark();
        assert_eq!(solver.unify_checked(&env, tys.var(b), fun, later), Ok(()));
    }
}
