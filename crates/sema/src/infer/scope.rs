//! [`Scope`]: one module-level binding's state, from [`Gen::enter_scope`] to
//! [`Gen::close_scope`].

use super::*;

/// The state of one module-level binding's walk and solve: FCS's
/// generalisation scope. [`Gen::enter_scope`] builds one at the binding's
/// start, every walk and solve step of that binding is handed it, and
/// [`Gen::close_scope`] consumes it at the binding's end, recording its census
/// entry or not. Nothing outside a binding holds one, so no step can read
/// another binding's state.
///
/// Not `Clone`: a binding has exactly one.
pub(super) struct Scope {
    /// The vid mark taken at the binding's start — the boundary between this
    /// binding's own variables and *environment* variables inherited from
    /// earlier bindings. Generalisation refuses to quantify an inherited-open
    /// variable ([`Gen::generalise`]), and the [`Constraint::ArgCheck`] wake
    /// applies the same discipline: a discharge may bind only open variables
    /// created in this binding ([`Gen::arg_check_binds_only_current_vars`]),
    /// never retro-ground an earlier binder's still-open variable (whose type
    /// FCS fixed at *its* binding, e.g. `let g = id` staying `'a -> 'a` under
    /// a later `g 1`).
    pub(super) mark: u32,
    /// [`Emissions::mark`] at the binding's start.
    pub(super) emissions: EmissionMark,
    /// Walk-completeness (Stage 3.2c-2c): cleared by any unmodelled
    /// sub-expression or pattern. A binding may generalise only if it stays
    /// complete — the proof that every constraint FCS has on its open variables is
    /// one we modelled. Ground emission is *not* gated on this (the subset
    /// argument in the module docs).
    pub(super) complete: bool,
    /// Why the binding is incomplete, in the order the walk found out: its
    /// census entry ([`InferredFile::incompleteness`]).
    pub(super) reasons: Vec<Incomplete>,
    /// The body of the function binding being walked, for
    /// [`Gen::is_first_occurrence`]. `None` outside a function binding.
    pub(super) body: Option<SyntaxNode>,
    /// The poison set (Stage 3.2c-2c): the raw variables of every check-mode
    /// [`Gen::infer_expr`] call (the expected and the returned var), whose
    /// cross-relation this stage drops. At generalisation its one-step closure
    /// (resolve each, take every var in the resolved term) gives the variables
    /// that may *not* be quantified — FCS's dropped relation could have
    /// grounded them.
    pub(super) poison: Vec<TyVid>,
    /// The simple named parameters of the binding as `(DefId, slot)` pairs, in
    /// head order (Stage 3.2c-2c); drives the slot=binder reunification on a
    /// complete function binding.
    pub(super) params: Vec<(DefId, TyVid)>,
    /// The binding's parameters, as registered: what
    /// [`Gen::withhold_if_not_rigid`] withholds with the function.
    pub(super) binding_params: Vec<DefId>,
    /// The names the binding's `match` patterns bind. FCS never generalises a
    /// pattern variable, so each is this binding's own monomorphic variable,
    /// used as a parameter is ([`Gen::value_use`]): not another binding's
    /// opaque one.
    pub(super) match_locals: HashSet<DefId>,
    /// [`Solver::constraint_mark`] at the binding's start: the binding's
    /// per-variable constraints are those recorded since
    /// ([`Gen::constrained_scheme_use`]), and its checks read those alone —
    /// an argument check's discharge ([`Solver::unify_checked`]) and the
    /// check once the binding is solved ([`Gen::check_constraints`]).
    pub(super) constraints: ConstraintMark,
    /// The binding's **implicitly scoped type variables**: each name a `'a` in
    /// its head's annotations spells, to the one inference variable every
    /// mention of it denotes ([`Gen::open_head_typars`]). Empty where the head
    /// declares its variables explicitly or mentions a `^a`. The head's
    /// annotations read their own copy ([`AnnotationCx::Head`]); this one is
    /// the binding's solve's ([`Gen::is_head_typar_root`]).
    pub(super) head_typars: HashMap<String, TyVid>,
    /// The binding's parameters whose annotation mentions one of
    /// [`Scope::head_typars`].
    pub(super) typar_params: HashSet<DefId>,
    /// Of [`Scope::head_typars`], the variables the head declares
    /// (`let f<'a> …`), in declaration order: rigid, so each must end the
    /// binding a distinct unbound variable ([`Gen::rigid_typars_hold`]).
    pub(super) rigid_typars: Vec<TyVid>,
    /// Whether the binding's head declares type variables (`let f<'a> …`),
    /// whether or not [`Gen::open_head_typars`] reads them.
    pub(super) declares_typars: bool,
    /// Whether the binding, with every relation it holds applied as the
    /// equality FCS tries first, breaks a declared variable's rigidity or
    /// fails outright ([`Gen::solve`]): FCS then rejects the declaration.
    pub(super) rigid_rejected: bool,
    /// Every check-mode relation the walk dropped, as the checked expression's
    /// variable and its expected one ([`Gen::infer_expr`]). FCS applies each
    /// one, as an equality whenever that is consistent with what it knows —
    /// which is what [`Gen::verified`] tests them for.
    pub(super) dropped: Vec<(TyVid, TyVid, TextRange)>,
    /// How many equalities failed to unify. A failed one is a relation FCS
    /// rejects, in an order the solver does not keep.
    pub(super) eq_failures: usize,
    /// The variables a loosened value use stands in for a parameter type other
    /// than `obj` ([`Gen::loosened`]). In FCS each is a subtype of that type, a
    /// constraint inference does not model.
    pub(super) bounded_flex: Vec<TyVid>,
    /// The variables of the value uses whose loosening could not be decided at
    /// the use, because the value's type was not yet known there
    /// ([`Gen::value_use`]).
    pub(super) undecided_uses: Vec<TyVid>,
    /// The variables FCS constrains through a relation inference does not
    /// model, so that what FCS makes of them is unknown here
    /// ([`Gen::arg_check_binds_only_current_vars`], [`Gen::verified`]): each
    /// binder this binding refers to that is not its own — an earlier binder,
    /// whose type FCS fixed at its own binding — and each argument of a method
    /// call, which FCS relates to a parameter type it reads off the method it
    /// resolves; and each member access's result, until its wake binds it to
    /// the member's type.
    ///
    /// A method argument's entry carries the argument's span: a relation
    /// inside the argument is one FCS checks before it relates the argument to
    /// the parameter, so it may bind the argument's variables.
    pub(super) opaque: Vec<(TyVid, Option<TextRange>)>,
}

/// Whether a closed [`Scope`]'s census entry is recorded
/// ([`Gen::close_scope`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Census {
    /// The binding was walked: its reasons are one entry of
    /// [`InferredFile::incompleteness`], empty if it was complete.
    Record,
    /// The binding turned out not to be walked here, and not for a reason the
    /// census counts, so it has no entry — each counted binding is counted
    /// once.
    Discard,
}

impl Scope {
    /// Mark the binding **walk-incomplete** — some sub-expression or pattern
    /// is not fully modelled, so the binding must not generalise (Stage
    /// 3.2c-2c). Ground emission is unaffected.
    pub(super) fn mark_incomplete(&mut self, why: Incomplete) {
        self.complete = false;
        self.reasons.push(why);
    }

    /// How many reasons the census entry holds — the mark a sub-walk's
    /// [`Self::mark_consequence`] compares against.
    pub(super) fn reasons_recorded(&self) -> usize {
        self.reasons.len()
    }

    /// Mark the binding incomplete for a reason that may be only the
    /// *consequence* of a sub-walk: a local or statement left open, a method
    /// callee that did not synthesize. If the sub-walk begun at `recorded`
    /// already recorded a reason, that is the cause, and `why` is not counted
    /// again — so "the only observed reason" counts what modelling one
    /// construct could unlock, not its downstream effects.
    pub(super) fn mark_consequence(&mut self, recorded: usize, why: Incomplete) {
        if self.reasons_recorded() > recorded {
            self.mark_incomplete_propagated();
        } else {
            self.mark_incomplete(why);
        }
    }

    /// Mark the binding incomplete where a sub-walk that returned `None` has
    /// already recorded why — so the census counts the cause once.
    pub(super) fn mark_incomplete_propagated(&mut self) {
        self.complete = false;
    }

    /// Whether `def` is a **parameter of the binding** — one collected by
    /// [`Gen::param_var`] into [`Self::params`] for this function head. A use
    /// of such a parameter is generalisable (its variable is a genuine local of
    /// this binding); a use of any *other* in-file binder is an environment
    /// reference this stage must not quantify (Stage 3.2c-2c). The list is one
    /// entry per simple parameter, so the linear scan is cheap.
    pub(super) fn is_param(&self, def: DefId) -> bool {
        self.params.iter().any(|(d, _)| *d == def)
    }
}

impl<'a> Gen<'a> {
    /// A fresh [`Scope`] for a binding starting now: its vid mark and emission
    /// mark are taken here, so it must be entered before any of the binding's
    /// variables is allocated or any of its nodes emitted.
    pub(super) fn enter_scope(&self) -> Scope {
        Scope {
            mark: self.solver.table.mark(),
            emissions: self.out.mark(),
            constraints: self.solver.constraint_mark(),
            complete: true,
            reasons: Vec::new(),
            body: None,
            poison: Vec::new(),
            params: Vec::new(),
            binding_params: Vec::new(),
            match_locals: HashSet::default(),
            head_typars: HashMap::default(),
            typar_params: HashSet::default(),
            rigid_typars: Vec::new(),
            declares_typars: false,
            rigid_rejected: false,
            dropped: Vec::new(),
            eq_failures: 0,
            bounded_flex: Vec::new(),
            undecided_uses: Vec::new(),
            opaque: Vec::new(),
        }
    }

    /// End the binding `scope` was entered for, recording its census entry
    /// ([`InferredFile::incompleteness`]) as `census` says. Everything else the
    /// scope held ends with it.
    pub(super) fn close_scope(&mut self, scope: Scope, census: Census) {
        match census {
            Census::Record => self.out.census(scope.reasons),
            Census::Discard => {}
        }
    }
}
