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
//! The table's value type is `Option<Binding>` — `None` for an unbound
//! variable, `Some(t)` for one bound to a term `t` (which may itself contain
//! variables, and is shared, so reading it is a pointer copy). `ena` merges
//! two values via [`ena::unify::UnifyValue`]; we make [`Binding`] an
//! [`EqUnifyValue`] so a `Some`/`Some` merge succeeds only on syntactic
//! equality. That branch is in fact never reached: every call here resolves
//! both operands to a *root* before unioning, so the table only ever merges an
//! unbound (`None`) root with something — structural unification of two bound
//! terms is our job, done by [`InferTable::unify`], not `ena`'s. This is the standard
//! way `rustc` (whence `ena` comes) layers an HM unifier over the union-find.
//!
//! [`type-checker-plan`]: ../../../docs/type-checker-plan.md

use std::rc::Rc;

use ena::unify::{EqUnifyValue, InPlaceUnificationTable, UnifyKey};

use crate::ty::{MAX_TYPE_DEPTH, MAX_TYPE_NODES, Ty, TySize, TyVid, TypeBudgetExceeded};

impl UnifyKey for TyVid {
    type Value = Option<Binding>;

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

/// The term a variable is bound to: never itself a [`Ty::Var`] (a variable
/// meeting a variable is a union, not a binding), and shared, so that reading
/// a binding is a pointer copy rather than a copy of the term.
///
/// `pub` only because it is [`TyVid`]'s `UnifyKey::Value`; the module is
/// private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding(Rc<Ty>);

// A `Some`/`Some` merge succeeds only when the two bound terms are syntactically
// equal. Our algorithm resolves to roots before unioning, so this is never hit
// with two `Some`s in practice — but the bound is what lets `Option<Binding>`
// be a `UnifyValue`.
impl EqUnifyValue for Binding {}

/// Why a [`InferTable::unify`] failed. Best-effort inference treats a
/// mismatch or an occurs failure as "leave the variables unsolved" (D5: say
/// nothing), so neither surfaces a diagnostic in this phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnifyError {
    /// Two incompatible concrete types: different named paths, different array
    /// ranks, or a named type against an array.
    Mismatch,
    /// The occurs check failed — binding the variable would build an infinite
    /// type (`'a = 'a[]`).
    Occurs,
    /// The unification walked past the type budget, which the table now
    /// records ([`InferTable::over_budget`]): whether the terms unify is not
    /// known.
    OverBudget(TypeBudgetExceeded),
}

impl From<TypeBudgetExceeded> for UnifyError {
    fn from(e: TypeBudgetExceeded) -> UnifyError {
        UnifyError::OverBudget(e)
    }
}

/// The bounds every walk of an [`InferTable`] is held to: by default the
/// crate's [`MAX_TYPE_NODES`] and [`MAX_TYPE_DEPTH`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TypeBudget {
    /// The most nodes a resolution may build. A unification may visit twice
    /// as many, which is what one whose unified type is within this bound
    /// needs (see [`InferTable::unify`]).
    pub(crate) nodes: usize,
    /// The deepest any walk may reach, the root at depth 1.
    pub(crate) depth: usize,
}

impl Default for TypeBudget {
    fn default() -> Self {
        TypeBudget {
            nodes: MAX_TYPE_NODES,
            depth: MAX_TYPE_DEPTH,
        }
    }
}

/// The largest walks an inference table made, by kind: what the corpus sweep
/// measures the type budget ([`MAX_TYPE_NODES`], [`MAX_TYPE_DEPTH`]) against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TypeWalks {
    /// The largest type a resolution built, and the deepest.
    pub resolve: TySize,
    /// The most nodes one unification visited (its occurs checks included),
    /// and the deepest it reached.
    pub unify: TySize,
}

/// One walk's count against its budget.
struct Walk {
    nodes: usize,
    depth: usize,
    max_nodes: usize,
    max_depth: usize,
}

impl Walk {
    fn new(max_nodes: usize, max_depth: usize) -> Walk {
        Walk {
            nodes: 0,
            depth: 0,
            max_nodes,
            max_depth,
        }
    }

    /// Count a visit to a node at `depth`, or report the bound it passes.
    fn visit(&mut self, depth: usize) -> Result<(), TypeBudgetExceeded> {
        if depth > self.max_depth {
            return Err(TypeBudgetExceeded::Depth);
        }
        self.nodes += 1;
        if self.nodes > self.max_nodes {
            return Err(TypeBudgetExceeded::Nodes);
        }
        self.depth = self.depth.max(depth);
        Ok(())
    }

    fn size(&self) -> TySize {
        TySize {
            nodes: self.nodes,
            depth: self.depth,
        }
    }
}

/// What a term is at its top, once a variable is looked through: the term
/// itself, the term a variable is bound to, or a variable's unbound root.
enum Head<'t> {
    Term(&'t Ty),
    Bound(Rc<Ty>),
    Root(TyVid),
}

/// An open snapshot of an [`InferTable`] ([`InferTable::snapshot`]). Not
/// `Clone`: it is closed exactly once, by a commit or a rollback.
pub(crate) struct TableSnapshot(ena::unify::Snapshot<ena::unify::InPlace<TyVid>>);

/// The union-find table over inference variables, plus the structural unifier.
///
/// # The type budget
///
/// The table shares structure: a variable bound to `'a * 'a` with `'a` bound
/// to `'b * 'b` holds a type twice the size of what it stores, so resolving a
/// type, checking an occurrence or unifying two can walk exponentially more
/// than the table holds. Every walk therefore counts the nodes it visits and
/// the depth it reaches against a [`TypeBudget`], and stops at the first node
/// past it, having built at most the budget's worth.
///
/// The guarantee: a walk that passes the budget records it
/// ([`Self::over_budget`]), and from then on the table answers nothing it
/// would have to walk for — [`Self::resolve`] returns its argument
/// unresolved, and [`Self::unify`] fails with [`UnifyError::OverBudget`] — so
/// what it says after that point is not an answer. The record survives a
/// [`Self::rollback_to`]: it is a fact about the program, not about the
/// speculation in progress. A caller that reads the table's answers into a
/// result must therefore discard the result once the record is set;
/// inference discards the whole file's ([`crate::infer_file`]).
#[derive(Debug, Default)]
pub(crate) struct InferTable {
    table: InPlaceUnificationTable<TyVid>,
    budget: TypeBudget,
    exceeded: Option<TypeBudgetExceeded>,
    walks: TypeWalks,
}

impl InferTable {
    pub(crate) fn new() -> Self {
        InferTable::default()
    }

    /// A table whose walks are held to `budget` rather than the crate's.
    #[cfg(test)]
    pub(crate) fn with_budget(budget: TypeBudget) -> Self {
        InferTable {
            budget,
            ..InferTable::default()
        }
    }

    /// Which bound a walk passed, once one has: from then on the table's
    /// answers are not answers (see the type budget, above).
    pub(crate) fn over_budget(&self) -> Option<TypeBudgetExceeded> {
        self.exceeded
    }

    /// Record that a type the caller would build passes the budget, without
    /// building it.
    pub(crate) fn exceed(&mut self, why: TypeBudgetExceeded) {
        self.exceeded.get_or_insert(why);
    }

    /// The largest walks the table has made.
    pub(crate) fn walks(&self) -> TypeWalks {
        self.walks
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

    /// The root of `v`'s equivalence class: two variables are
    /// [`unioned`](Self::unioned) exactly when their roots are equal, until
    /// the table next changes.
    pub(crate) fn root(&mut self, v: TyVid) -> TyVid {
        self.table.find(v)
    }

    /// Look through `ty` one level: a variable to the term it is bound to, or
    /// to its canonical unbound root; any other term to itself.
    fn head<'t>(&mut self, ty: &'t Ty) -> Head<'t> {
        match ty {
            Ty::Var(v) => match self.table.probe_value(*v) {
                Some(Binding(bound)) => {
                    debug_assert!(
                        !matches!(*bound, Ty::Var(_)),
                        "a variable is bound to a term, never to a variable"
                    );
                    Head::Bound(bound)
                }
                None => Head::Root(self.table.find(*v)),
            },
            other => Head::Term(other),
        }
    }

    /// Fully resolve `ty` against the current substitution, recursing into
    /// compound types. An unbound variable stays a [`Ty::Var`] (the "unknown"
    /// that the read-off in [`crate::infer`] turns into silence).
    ///
    /// A type past the budget is not built: the walk stops at the first node
    /// over it, the table records it ([`Self::over_budget`]), and `ty` comes
    /// back unresolved — as it does from every call once that is recorded.
    pub(crate) fn resolve(&mut self, ty: &Ty) -> Ty {
        if self.exceeded.is_none() {
            let mut walk = Walk::new(self.budget.nodes, self.budget.depth);
            let resolved = self.resolve_in(ty, 1, &mut walk);
            self.walks.resolve = self.walks.resolve.max_each(walk.size());
            match resolved {
                Ok(resolved) => return resolved,
                Err(e) => self.exceed(e),
            }
        }
        ty.clone()
    }

    /// [`Self::resolve`]'s walk, of `ty` at `depth`.
    fn resolve_in(
        &mut self,
        ty: &Ty,
        depth: usize,
        walk: &mut Walk,
    ) -> Result<Ty, TypeBudgetExceeded> {
        walk.visit(depth)?;
        let head = self.head(ty);
        let term = match &head {
            Head::Root(v) => return Ok(Ty::Var(*v)),
            Head::Term(t) => *t,
            Head::Bound(t) => &**t,
        };
        let below = depth + 1;
        Ok(match term {
            Ty::Array { elem, rank } => Ty::Array {
                elem: Box::new(self.resolve_in(elem, below, walk)?),
                rank: *rank,
            },
            Ty::Tuple(elems) => Ty::Tuple(
                elems
                    .iter()
                    .map(|e| self.resolve_in(e, below, walk))
                    .collect::<Result<_, _>>()?,
            ),
            Ty::Fun { arg, ret } => Ty::Fun {
                arg: Box::new(self.resolve_in(arg, below, walk)?),
                ret: Box::new(self.resolve_in(ret, below, walk)?),
            },
            // An application's ARGUMENTS are types and can hold solved
            // variables; only its head is a leaf.
            Ty::Named { path, args } => Ty::Named {
                path: path.clone(),
                args: args
                    .iter()
                    .map(|a| self.resolve_in(a, below, walk))
                    .collect::<Result<_, _>>()?,
            },
            Ty::Project { path, args } => Ty::Project {
                path: path.clone(),
                args: args
                    .iter()
                    .map(|a| self.resolve_in(a, below, walk))
                    .collect::<Result<_, _>>()?,
            },
            Ty::Param(i) => Ty::Param(*i),
            Ty::Var(_) => unreachable!("a variable's head is its binding or its root"),
        })
    }

    /// Whether `v`'s equivalence class occurs anywhere within `ty` (after
    /// resolution), `ty` sitting at `depth`. The occurs check that keeps
    /// [`InferTable::unify`] from binding a variable to a term containing
    /// itself; it counts against the unification's walk.
    fn occurs_in(
        &mut self,
        v: TyVid,
        ty: &Ty,
        depth: usize,
        walk: &mut Walk,
    ) -> Result<bool, TypeBudgetExceeded> {
        walk.visit(depth)?;
        let head = self.head(ty);
        let term = match &head {
            Head::Root(v2) => return Ok(self.table.unioned(v, *v2)),
            Head::Term(t) => *t,
            Head::Bound(t) => &**t,
        };
        let below = depth + 1;
        match term {
            // A `Param` is a rigid constant, so it cannot contain a variable's
            // equivalence class. A `Named`'s own path cannot either, but its
            // generic ARGUMENTS are types and can.
            Ty::Param(_) => Ok(false),
            Ty::Named { args, .. } | Ty::Project { args, .. } | Ty::Tuple(args) => {
                for a in args {
                    if self.occurs_in(v, a, below, walk)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Ty::Array { elem, .. } => self.occurs_in(v, elem, below, walk),
            Ty::Fun { arg, ret } => {
                Ok(self.occurs_in(v, arg, below, walk)? || self.occurs_in(v, ret, below, walk)?)
            }
            Ty::Var(_) => unreachable!("a variable's head is its binding or its root"),
        }
    }

    /// Unify `a` and `b` atomically: on success the table reflects the merge; on
    /// failure it is rolled back to exactly its prior state. The plain
    /// [`InferTable::unify`] may leave *partial* bindings when a compound mismatches
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
    /// is **observationally equal to applying `f`'s mutations directly**. The
    /// one exception is the type budget's record, which a rollback keeps.
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

    /// Open a snapshot of the table, to be closed by exactly one of
    /// [`Self::commit`] (keep everything since) or [`Self::rollback_to`]
    /// (restore the table to exactly this point). Snapshots nest LIFO, as
    /// [`Self::probe`]'s do: one opened later must be closed first.
    pub(crate) fn snapshot(&mut self) -> TableSnapshot {
        TableSnapshot(self.table.snapshot())
    }

    /// Keep every change since `snapshot` was opened.
    pub(crate) fn commit(&mut self, snapshot: TableSnapshot) {
        self.table.commit(snapshot.0);
    }

    /// Restore the table to exactly its state when `snapshot` was opened,
    /// discarding every variable and binding made since. The type budget's
    /// record is kept.
    pub(crate) fn rollback_to(&mut self, snapshot: TableSnapshot) {
        self.table.rollback_to(snapshot.0);
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
    /// The walk visits each position of the unified type at most once in
    /// step and once more in an occurs check — the positions where a variable
    /// is bound do not nest — so a unification whose unified type is within
    /// the budget's nodes visits at most twice that many, which is its bound.
    /// One that would visit more fails with [`UnifyError::OverBudget`], as
    /// every call does once the table has recorded that.
    ///
    /// **May leave partial bindings on failure** (a compound that mismatches
    /// after an earlier sub-term bound); a caller that discards the result needs
    /// [`Self::unify_atomic`] instead.
    pub(crate) fn unify(&mut self, a: &Ty, b: &Ty) -> Result<(), UnifyError> {
        if let Some(e) = self.exceeded {
            return Err(UnifyError::OverBudget(e));
        }
        let mut walk = Walk::new(self.budget.nodes.saturating_mul(2), self.budget.depth);
        let unified = self.unify_in(a, b, 1, &mut walk);
        self.walks.unify = self.walks.unify.max_each(walk.size());
        if let Err(UnifyError::OverBudget(e)) = unified {
            self.exceed(e);
        }
        unified
    }

    /// [`Self::unify`]'s walk, of `a` and `b` at `depth`.
    fn unify_in(
        &mut self,
        a: &Ty,
        b: &Ty,
        depth: usize,
        walk: &mut Walk,
    ) -> Result<(), UnifyError> {
        walk.visit(depth)?;
        let a = self.head(a);
        let b = self.head(b);
        let (ta, tb) = match (&a, &b) {
            (Head::Root(va), Head::Root(vb)) => {
                // Both unbound roots (values `None`), so the merge is
                // infallible — see the module docs.
                self.table
                    .unify_var_var(*va, *vb)
                    .expect("merging two unbound variables is infallible");
                return Ok(());
            }
            (Head::Root(v), _) => return self.bind(*v, &b, depth, walk),
            (_, Head::Root(v)) => return self.bind(*v, &a, depth, walk),
            (Head::Term(ta), Head::Term(tb)) => (*ta, *tb),
            (Head::Term(ta), Head::Bound(tb)) => (*ta, &**tb),
            (Head::Bound(ta), Head::Term(tb)) => (&**ta, *tb),
            (Head::Bound(ta), Head::Bound(tb)) => (&**ta, &**tb),
        };
        let below = depth + 1;
        match (ta, tb) {
            // Two applications are equal when their heads are and their
            // arguments unify pairwise. A differing arity is a mismatch, not a
            // partial match: `Dictionary<int>` is not a `Dictionary<int, string>`.
            (Ty::Named { path: pa, args: aa }, Ty::Named { path: pb, args: ab })
            | (Ty::Project { path: pa, args: aa }, Ty::Project { path: pb, args: ab }) => {
                if pa != pb || aa.len() != ab.len() {
                    return Err(UnifyError::Mismatch);
                }
                for (x, y) in aa.iter().zip(ab.iter()) {
                    self.unify_in(x, y, below, walk)?;
                }
                Ok(())
            }
            (Ty::Array { elem: ea, rank: ra }, Ty::Array { elem: eb, rank: rb }) => {
                if ra != rb {
                    return Err(UnifyError::Mismatch);
                }
                self.unify_in(ea, eb, below, walk)
            }
            (Ty::Tuple(ea), Ty::Tuple(eb)) => {
                // Tuples unify element-wise; differing arity is a mismatch (an
                // n-tuple is not an m-tuple).
                if ea.len() != eb.len() {
                    return Err(UnifyError::Mismatch);
                }
                for (a, b) in ea.iter().zip(eb.iter()) {
                    self.unify_in(a, b, below, walk)?;
                }
                Ok(())
            }
            (Ty::Fun { arg: aa, ret: ra }, Ty::Fun { arg: ab, ret: rb }) => {
                // Function types unify component-wise (domain with domain, range
                // with range) — the standard congruence rule.
                self.unify_in(aa, ab, below, walk)?;
                self.unify_in(ra, rb, below, walk)
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

    /// Bind the unbound root `v` to the term `head` names, which sits at
    /// `depth` of the unification: occurs-check it, then bind.
    fn bind(
        &mut self,
        v: TyVid,
        head: &Head<'_>,
        depth: usize,
        walk: &mut Walk,
    ) -> Result<(), UnifyError> {
        let binding = match head {
            Head::Root(_) => unreachable!("a variable is bound to a term"),
            Head::Term(term) => {
                if self.occurs_in(v, term, depth, walk)? {
                    return Err(UnifyError::Occurs);
                }
                Binding(Rc::new((*term).clone()))
            }
            Head::Bound(term) => {
                if self.occurs_in(v, term, depth, walk)? {
                    return Err(UnifyError::Occurs);
                }
                Binding(Rc::clone(term))
            }
        };
        // `v` is an unbound root (value `None`), so binding it cannot
        // conflict — see the module docs.
        self.table
            .unify_var_value(v, Some(binding))
            .expect("binding an unbound variable is infallible");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Property tests for the substrate against a textbook Robinson MGU
    //! reference (gospel principle 4 / the `property-based-testing` skill): the
    //! reference *is* the spec, and the `ena`-backed table must agree with it on
    //! arbitrary constraint sequences.

    use proptest::prelude::*;

    use super::{InferTable, TypeBudget, UnifyError};
    use crate::hash::HashMap;
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
                Ty::Named { path, args } => Ty::Named {
                    path,
                    args: args.iter().map(|a| self.resolve(a)).collect(),
                },
                Ty::Project { path, args } => Ty::Project {
                    path,
                    args: args.iter().map(|a| self.resolve(a)).collect(),
                },
                resolved => resolved,
            }
        }

        fn occurs(&self, v: u32, ty: &Ty) -> bool {
            match self.walk(ty) {
                Ty::Var(v2) => v2.index() == v,
                Ty::Param(_) => false,
                Ty::Named { args, .. } => args.iter().any(|a| self.occurs(v, a)),
                Ty::Project { args, .. } => args.iter().any(|a| self.occurs(v, a)),
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
                (Ty::Named { path: pa, args: aa }, Ty::Named { path: pb, args: ab }) => {
                    if pa != pb || aa.len() != ab.len() {
                        return Err(UnifyError::Mismatch);
                    }
                    for (x, y) in aa.iter().zip(ab.iter()) {
                        self.unify(x, y)?;
                    }
                    Ok(())
                }
                (Ty::Project { path: pa, args: aa }, Ty::Project { path: pb, args: ab }) => {
                    if pa != pb || aa.len() != ab.len() {
                        return Err(UnifyError::Mismatch);
                    }
                    for (x, y) in aa.iter().zip(ab.iter()) {
                        self.unify(x, y)?;
                    }
                    Ok(())
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
        let mut rename: HashMap<u32, u32> = HashMap::default();
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
            Ty::Named { path, args } => Ty::Named {
                path: path.clone(),
                args: args.iter().map(|a| canon_one(a, rename, next)).collect(),
            },
            Ty::Project { path, args } => Ty::Project {
                path: path.clone(),
                args: args.iter().map(|a| canon_one(a, rename, next)).collect(),
            },
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
            // Project types, one at the same path as the assembly type `A.B`:
            // the two are different types and never unify.
            prop::sample::select(vec!["A.B", "P.Q"]).prop_map(|p: &str| Ty::Project {
                path: p.split('.').map(str::to_owned).collect(),
                args: Vec::new(),
            }),
        ];
        // Nested arrays / tuples / functions / generic applications around the
        // leaves; bounded depth keeps shrinking quick. Exercises the structural
        // `Array` / `Tuple` / `Fun` / applied-`Named` unify arms.
        //
        // The applied `Named` arm is what makes a variable reachable *inside* a
        // named type. Without it every `Ty::Named` the generator produced was a
        // leaf, so any operation that treated `Named` as a leaf — the occurs
        // check, either resolver — agreed with the reference vacuously.
        leaf.prop_recursive(3, 24, 3, |inner| {
            prop_oneof![
                (inner.clone(), 1u32..3).prop_map(|(elem, rank)| Ty::Array {
                    elem: Box::new(elem),
                    rank,
                }),
                prop::collection::vec(inner.clone(), 2..4).prop_map(Ty::Tuple),
                (inner.clone(), inner.clone()).prop_map(|(arg, ret)| Ty::Fun {
                    arg: Box::new(arg),
                    ret: Box::new(ret),
                }),
                (
                    prop::sample::select(vec!["G.One", "G.Two"]),
                    prop::collection::vec(inner.clone(), 1..3),
                )
                    .prop_map(|(head, args)| Ty::Named {
                        path: head.split('.').map(str::to_owned).collect(),
                        args,
                    }),
                // An applied project type, at a path an applied `Named` also
                // takes, so a variable is reachable inside one.
                (
                    prop::sample::select(vec!["G.One", "P.R"]),
                    prop::collection::vec(inner, 1..3),
                )
                    .prop_map(|(head, args)| Ty::Project {
                        path: head.split('.').map(str::to_owned).collect(),
                        args,
                    }),
            ]
        })
    }

    /// A variable bound inside a named type's ARGUMENTS must be substituted by
    /// `resolve`. The direct statement of what the generated property covers
    /// only by chance — `N<'a> = N<int>` binds `'a`, so resolving `N<'a>` must
    /// give `N<int>` and not the stale `N<'a>`.
    #[test]
    fn resolve_substitutes_inside_a_named_types_arguments() {
        let mut table = InferTable::new();
        let v = table.fresh();
        let applied = |arg: Ty| Ty::Named {
            path: vec!["G".to_string(), "One".to_string()],
            args: vec![arg],
        };
        table
            .unify(&applied(Ty::Var(v)), &applied(Ty::named("System.Int32")))
            .expect("same head and arity unify");
        assert_eq!(
            table.resolve(&applied(Ty::Var(v))),
            applied(Ty::named("System.Int32")),
            "the argument variable was solved, so it must not survive resolution"
        );
        assert!(
            table.resolve(&applied(Ty::Var(v))).is_ground(),
            "a fully-solved application is ground"
        );
    }

    /// Every variable still standing in a resolved type, in traversal order.
    fn free_vars(ty: &Ty, out: &mut Vec<TyVid>) {
        match ty {
            Ty::Var(v) => out.push(*v),
            Ty::Named { args, .. } | Ty::Project { args, .. } => {
                args.iter().for_each(|a| free_vars(a, out))
            }
            Ty::Array { elem, .. } => free_vars(elem, out),
            Ty::Tuple(elems) => elems.iter().for_each(|e| free_vars(e, out)),
            Ty::Fun { arg, ret } => {
                free_vars(arg, out);
                free_vars(ret, out);
            }
            Ty::Param(_) => {}
        }
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

    /// [`pooled_table`], held to `budget`.
    fn budgeted_pool(budget: TypeBudget) -> InferTable {
        let mut table = InferTable::with_budget(budget);
        for _ in 0..POOL {
            table.fresh();
        }
        table
    }

    proptest! {
        /// **Resolution is complete**: no variable surviving a resolved type is
        /// one the table has already solved. Stated absolutely rather than
        /// against the reference MGU on purpose — the two resolvers are
        /// independent implementations of the same recursion, so a shape either
        /// one treats as a leaf is a shape they agree about vacuously, and
        /// `matches_reference_mgu` cannot see it. (That is exactly what happened
        /// when `Ty::Named` gained arguments: both resolvers stopped at the head
        /// and the differential stayed green.)
        #[test]
        fn resolution_leaves_no_solved_variable(constraints in constraint_list()) {
            let mut table = pooled_table();
            for (a, b) in &constraints {
                if table.unify(a, b).is_err() {
                    break;
                }
                for t in [a, b] {
                    let resolved = table.resolve(t);
                    let mut vars = Vec::new();
                    free_vars(&resolved, &mut vars);
                    for v in vars {
                        let again = table.resolve(&Ty::Var(v));
                        prop_assert_eq!(
                            &again, &Ty::Var(v),
                            "resolved {:?} still holds {:?}, which the table solves to {:?}",
                            resolved, v, again
                        );
                    }
                }
            }
        }

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

        /// **The budget is exact for resolution.** Under a small budget, a
        /// resolved type the reference MGU says is within it comes back equal
        /// to the reference's, and one past it in either dimension is refused:
        /// the table records the bound and hands back its argument unresolved.
        /// Constraints are replayed until one passes the budget.
        #[test]
        fn resolution_is_exact_within_the_budget_and_refused_past_it(
            constraints in constraint_list(),
            nodes in 1usize..14,
            depth in 1usize..6,
        ) {
            let budget = TypeBudget { nodes, depth };
            let mut table = budgeted_pool(budget);
            let mut reference = RefSubst::default();
            for (a, b) in &constraints {
                let ours = table.unify(a, b);
                if matches!(ours, Err(UnifyError::OverBudget(_))) {
                    break;
                }
                let theirs = reference.unify(a, b);
                prop_assert_eq!(ours.is_err(), theirs.is_err());
                if ours.is_err() {
                    break;
                }
            }
            for k in 0..POOL {
                if table.over_budget().is_some() {
                    break;
                }
                let v = Ty::Var(TyVid(k));
                let want = reference.resolve(&v);
                let size = want.size();
                let got = table.resolve(&v);
                if size.nodes <= nodes && size.depth <= depth {
                    prop_assert_eq!(table.over_budget(), None);
                    prop_assert_eq!(canonicalize(&[got]), canonicalize(&[want]));
                } else {
                    prop_assert!(table.over_budget().is_some(), "{:?} is past {:?}", size, budget);
                    prop_assert_eq!(got, v);
                }
            }
        }

        /// **A unification within the budget is never refused.** Whenever the
        /// reference unifies two terms and the unified type is within the
        /// budget, the table unifies them too: a unification visits each
        /// position of the unified type at most twice, and its node bound is
        /// twice the budget's.
        #[test]
        fn unification_within_the_budget_is_never_refused(
            constraints in constraint_list(),
            nodes in 1usize..14,
            depth in 1usize..6,
        ) {
            let budget = TypeBudget { nodes, depth };
            let mut table = budgeted_pool(budget);
            let mut reference = RefSubst::default();
            for (a, b) in &constraints {
                if table.over_budget().is_some() {
                    break;
                }
                let theirs = reference.unify(a, b);
                let ours = table.unify(a, b);
                let size = reference.resolve(a).size();
                if theirs.is_ok() && size.nodes <= nodes && size.depth <= depth {
                    prop_assert!(
                        ours.is_ok(),
                        "{:?} = {:?} unifies within {:?} (unified size {:?}) but was {:?}",
                        a, b, budget, size, ours
                    );
                }
                if ours.is_err() || theirs.is_err() {
                    break;
                }
            }
        }

        /// Once a walk passes the budget, the table answers nothing it would
        /// walk for, and undoing a speculation does not forget it.
        #[test]
        fn the_budget_record_is_sticky(constraints in constraint_list()) {
            let mut table = budgeted_pool(TypeBudget { nodes: 1, depth: 1 });
            let _ = table.probe(|t| {
                for (a, b) in &constraints {
                    let _ = t.unify(a, b);
                }
                Err::<(), ()>(())
            });
            if table.over_budget().is_some() {
                for (a, b) in &constraints {
                    prop_assert!(matches!(table.unify(a, b), Err(UnifyError::OverBudget(_))));
                    prop_assert_eq!(&table.resolve(a), a);
                }
            }
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
