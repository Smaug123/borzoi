//! The solve of a binding's constraints, at its end ([`Gen::solve`]) and
//! mid-walk ([`Gen::settle`]): the worklist, and the checks a solve is held to.

use super::*;

/// The open variables of [`Scope::opaque`]'s entries, as roots of their classes
/// ([`Gen::opaque_roots`]).
#[derive(Debug, Default)]
struct OpaqueRoots {
    /// Of the entries that hold everywhere: a binder the binding refers to,
    /// a member access's result.
    everywhere: HashSet<TyVid>,
    /// Of each method argument's entry, with the argument's span: a relation
    /// inside it may bind them.
    outside: Vec<(TextRange, HashSet<TyVid>)>,
}

/// A pending member access inside [`Gen::solve`] — the suspended form of
/// [`Constraint::HasMember`], threaded through the solver's fixpoint.
#[derive(Debug, Clone)]
pub(super) struct SuspendedMember {
    recv: TyVid,
    pub(super) name: String,
    pub(super) result: TyVid,
    pub(super) use_range: TextRange,
    pub(super) kind: MemberAccessKind,
    args_by: TextSize,
    /// Whether the member waited on its receiver, which FCS must know at the
    /// access itself; otherwise it waited only on its arguments.
    waited_on_receiver: bool,
}

impl SuspendedMember {
    /// Whether the member woke only through a relation FCS applies after it:
    /// FCS reads the receiver at the access, and a method's arguments once
    /// they are checked, so a discharge ending later than that
    /// (`latest_discharge`) is one FCS had not applied
    /// ([`WorklistOutcome::late_member`]).
    fn late(&self, latest_discharge: Option<TextSize>) -> bool {
        let by = if self.waited_on_receiver {
            self.use_range.start()
        } else {
            self.args_by
        };
        latest_discharge.is_some_and(|end| end > by)
    }
}

/// Which argument checks a run of the worklist may fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fire {
    /// None: the walk is not finished ([`Gen::settle`]), or the binding's
    /// checks could not be verified ([`Gen::solve`]).
    None,
    /// Every check whose gate holds.
    All,
}

/// What one run of the worklist did ([`Gen::run_worklist`]).
#[derive(Debug, Default)]
struct WorklistOutcome {
    /// The member accesses still pending.
    members: Vec<SuspendedMember>,
    /// The argument checks still pending.
    args: Vec<ArgCheck>,
    /// How many argument checks discharged.
    discharged: usize,
    /// How many argument checks fired and failed to unify.
    failed: usize,
    /// Whether a member access was resolved after an argument check that ends
    /// after it in the source had discharged — so possibly through that check,
    /// which FCS had not yet applied at the access.
    late_member: bool,
}

impl<'a> Gen<'a> {
    /// Discharge the constraint set into the table by unification (the solve
    /// half of D8), now a **worklist** (Stage 3.3a) with two suspended clients
    /// (`HasMember`, and the Stage-3.3c `ArgCheck`). Best-effort (D5): any
    /// constraint that fails to unify is *skipped* — discharged through
    /// [`InferTable::unify_atomic`], so a failed constraint rolls back whole and
    /// leaves no partial binding to leak into read-off. Its variables stay
    /// unresolved, so read-off stays silent rather than guessing.
    ///
    /// Three constraint kinds:
    /// - **`Eq`** is discharged eagerly.
    /// - **`HasMember`** is *suspended*: it fires only once its receiver resolves
    ///   to a concrete [`TyKind::Named`] head, at which point an unambiguous public
    ///   instance data member is looked up ([`Self::wake_member`]) and its type is
    ///   unified with the result (a new `Eq`). A concrete non-`Named` head
    ///   (`Array` / `Tuple` / `Fun` / `Param`) drops the constraint (defer — those
    ///   intrinsic members are out of scope). An unresolved receiver keeps it
    ///   pending.
    /// - **`ArgCheck`** (Stage 3.3c) is *suspended*: it fires once the enclosing
    ///   binding is walk-complete **and** its domain `dom` resolves to a
    ///   [no-subsumption](Self::no_subsumption_domain) type or a
    ///   [function type](Solver::function_domain), discharging `Eq(arg, dom)`. An
    ///   `ArgCheck` that never fires, or whose `Eq` fails, is **undischarged**
    ///   and — after the fixpoint — poisons `arg`, `dom`, and the application
    ///   result `r`, so an unwoken arg relation never generalises.
    ///
    /// The loop fixpoints: discharge every pending `Eq`, then scan the suspended
    /// lists — a member/arg wake pushes a new `Eq` (discharged next iteration) and
    /// may ground a *later* suspension's watched variable (a chained member
    /// `s.A.B`, or an `ArgCheck` whose `dom` a prior wake grounds). Each suspension
    /// fires at most once, so termination is by count. Members that never wake are
    /// dropped; `ArgCheck`s that never fire are poisoned (see above).
    ///
    /// **The solve is speculative.** FCS checks a body in source order: the
    /// first relation to fix a variable wins and a later conflicting one is
    /// reported and recovered; a member is looked up on what is known of its
    /// receiver at the access. The fixpoint keeps no order, so on a body FCS
    /// rejects it can fire a check FCS never applied — `(p |> fs, fi p, p)` would
    /// make `p` an `int` through `fi`, where FCS has fixed it to `string` and
    /// rejects `fi p`. So the argument checks are fired under a snapshot, and
    /// kept only if the result is [verified](Self::verified): every relation the
    /// binding holds, modelled or dropped, is consistent as an equality, so FCS —
    /// which tries an equality first — rejects none of them, and the order in
    /// which they are applied cannot matter. Otherwise everything the checks did
    /// is undone and the binding is solved with none of them firing: each stays
    /// undischarged and poisoned, which is silence.
    pub(super) fn solve(&mut self, scope: &mut Scope) {
        let constraints = self.solver.constraints.clone();
        let settled_args = self.solver.settled_args.clone();
        let poison = scope.poison.len();
        let eq_failures = scope.eq_failures;
        let snapshot = self.solver.table.snapshot();
        self.out.begin_trial();
        let outcome = self.run_worklist(scope, Fire::All);
        let log = self.out.end_trial();
        let speculative = self.probed_constraint_verdict(scope, &outcome);
        let outcome = if outcome.discharged == 0 || self.verified(scope, &outcome, speculative) {
            self.solver.table.commit(snapshot);
            outcome
        } else {
            self.solver.table.rollback_to(snapshot);
            self.out.undo(log);
            scope.poison.truncate(poison);
            scope.eq_failures = eq_failures;
            self.solver.constraints = constraints;
            self.solver.settled_args = settled_args;
            self.run_worklist(scope, Fire::None)
        };
        // Deferred poison (Stage 3.3c): every arg check still pending never fired
        // (its domain never became ready, the binding is incomplete, or its
        // checks were not verified), so its `arg`/`dom`/`r` are poisoned — an
        // undischarged arg relation must not let any of them generalise. (A
        // fired-and-failed check was already poisoned in the worklist; a
        // fired-and-succeeded one poisons nothing.) Members that never woke are
        // dropped.
        if !scope.rigid_typars.is_empty() {
            scope.rigid_rejected = !self.rigid_under_every_relation(scope, &outcome);
        }
        // The loosening invariant holds of the solution, however it was
        // reached: [`Self::verified`] checks it of a speculative one, and a
        // binding where no check discharged is not speculated at all, yet an
        // annotation can still give a use its structure
        // (`let f (g: obj -> 'a) = g`).
        if !self.loosening_holds(scope) {
            scope.poison.extend(scope.undecided_uses.iter().copied());
            scope.poison.extend(scope.bounded_flex.iter().copied());
            if scope.complete {
                scope.mark_incomplete(Incomplete::LoosenedUse);
            }
        }
        for ac in outcome.args {
            self.poison_arg_check(scope, ac);
        }
        self.check_constraints(scope, speculative);
    }

    /// Whether the current binding's declared variables ([`Scope::rigid_typars`])
    /// stay distinct and unbound with every relation it holds — the pending
    /// argument checks of `outcome` and the dropped check-mode relations —
    /// applied as an equality, and none of them failing. FCS tries the
    /// equality first, so a relation that would fix a declared variable is an
    /// error there (FS0670), and one that fails is an error too: in either
    /// case the declaration may be rejected whole.
    fn rigid_under_every_relation(&mut self, scope: &Scope, outcome: &WorklistOutcome) -> bool {
        if scope.eq_failures > 0 {
            return false;
        }
        let relations: Vec<(TyVid, TyVid)> = outcome
            .args
            .iter()
            .map(|ac| (ac.arg, ac.dom))
            .chain(scope.dropped.iter().map(|&(a, b, _)| (a, b)))
            .collect();
        let rigid = scope.rigid_typars.clone();
        let mut holds = false;
        let tys = self.solver.tys;
        let _ = self.solver.table.probe(|t| -> Result<(), ()> {
            let unified = relations
                .iter()
                .all(|(a, b)| t.unify(tys.var(*a), tys.var(*b)).is_ok());
            let mut roots = Vec::new();
            holds = unified
                && rigid
                    .iter()
                    .all(|&v| match *tys.kind(t.resolve(tys.var(v))) {
                        TyKind::Var(r) if !roots.contains(&r) => {
                            roots.push(r);
                            true
                        }
                        _ => false,
                    });
            Err(())
        });
        holds
    }

    /// The worst verdict ([`satisfies`]) over the current binding's
    /// constrained instantiations ([`Scope::constrained`]) in the table as it
    /// stands.
    fn constraint_verdict(&mut self, scope: &Scope) -> Verdict {
        constraint_verdict_of(
            &mut self.solver.table,
            self.facts.env,
            self.solver.tys,
            &scope.constrained,
        )
    }

    /// [`Self::constraint_verdict`] once every relation the solve left
    /// unapplied — the argument checks still pending and the check-mode
    /// relations the walk dropped ([`Scope::dropped`]) — is applied too, as
    /// [`Self::verified`]'s probe applies them: a constrained instantiation
    /// that only such a relation fixes is met there and nowhere else. The table
    /// is left as it was.
    fn probed_constraint_verdict(&mut self, scope: &Scope, outcome: &WorklistOutcome) -> Verdict {
        let relations: Vec<(TyVid, TyVid)> = outcome
            .args
            .iter()
            .map(|ac| (ac.arg, ac.dom))
            .chain(scope.dropped.iter().map(|&(a, b, _)| (a, b)))
            .collect();
        let env = self.facts.env;
        let constrained = &scope.constrained;
        let mut verdict = Verdict::Holds;
        let tys = self.solver.tys;
        let _ = self.solver.table.probe(|t| -> Result<(), ()> {
            for (a, b) in &relations {
                // A relation that does not unify fails the speculation in
                // `verified` on its own; the rest still decide the verdict.
                let _ = t.unify(tys.var(*a), tys.var(*b));
            }
            verdict = constraint_verdict_of(t, env, tys, constrained);
            Err(())
        });
        verdict
    }

    /// Record in the census what the binding just solved did to its
    /// constrained instantiations ([`Scope::constrained`]): the worse of
    /// `speculative`, their verdict under the speculative solve, and their
    /// verdict now.
    ///
    /// The check is over a whole substitution, so the order FCS meets the uses
    /// in cannot matter to it. A violation, or a type not known either way,
    /// marks the binding incomplete — it is not the program FCS accepts, or not
    /// one known to be — and [`Self::verified`] has already refused the
    /// speculative solve that produced it. A variable left open needs nothing
    /// here: it was poisoned when it was made, so it does not generalise.
    fn check_constraints(&mut self, scope: &mut Scope, speculative: Verdict) {
        match worse(speculative, self.constraint_verdict(scope)) {
            Verdict::Holds | Verdict::Open => {}
            Verdict::Fails => {
                scope.mark_incomplete(Incomplete::Constraint(ConstraintGap::Violated))
            }
            Verdict::Undecided => {
                scope.mark_incomplete(Incomplete::Constraint(ConstraintGap::Undecided))
            }
        }
    }

    /// Whether a speculative solve's argument checks may be kept
    /// ([`Self::solve`]): whether the binding's relations, taken as equalities,
    /// are consistent — so FCS, which tries an equality first at each and
    /// falls back to a coercion or an error only when it fails, applies every
    /// one of them as an equality, in whatever order, and agrees with what the
    /// discharges grounded. That needs:
    ///
    /// - no equality to have failed, and no fired check;
    /// - the relations the solve did not apply — the checks still pending and
    ///   the check-mode relations the walk dropped ([`Scope::dropped`]) — to
    ///   unify with the result, all together;
    /// - no member to have been resolved through a check FCS applies only
    ///   after it ([`WorklistOutcome::late_member`]): FCS rejects a member
    ///   access on a receiver it does not know yet (FS0072), consistent or not;
    /// - every constrained instantiation ([`Scope::constrained`]) to satisfy
    ///   its constraint, or be open, with the relations above applied
    ///   (`constraints`, [`Self::probed_constraint_verdict`]): a branch that
    ///   joins an open parameter to a function can be what fixes one. A
    ///   violation changes no type in itself, but
    ///   FCS meets it inside an argument check (`UnifyOverallType`), undoes the
    ///   equality it was solving, and reports the error with the rest of that
    ///   unification never applied: `a |> hash` over a function-holding `a`
    ///   leaves the pipe's result `'a`. A type not known to satisfy it may be
    ///   such a violation;
    ///
    /// and, in the result with every relation applied, each value use
    /// loosened as FCS loosens it:
    ///
    /// - no bounded loosened variable ([`Scope::bounded_flex`]) fixed to a
    ///   type, a subtype constraint nothing checks;
    /// - no undecided use ([`Scope::undecided_uses`]) of a value taking a
    ///   parameter of a type that is not sealed: whether FCS loosened it
    ///   depends on whether it knew that type at the use.
    ///
    /// The joint unification is a probe: the table is left as it was.
    fn verified(&mut self, scope: &Scope, outcome: &WorklistOutcome, constraints: Verdict) -> bool {
        if scope.eq_failures > 0 || outcome.failed > 0 || outcome.late_member {
            return false;
        }
        if matches!(constraints, Verdict::Fails | Verdict::Undecided) {
            return false;
        }
        let env = self.facts.env;
        let relations: Vec<(TyVid, TyVid, TextRange)> = outcome
            .args
            .iter()
            .map(|ac| (ac.arg, ac.dom, ac.span))
            .chain(scope.dropped.iter().copied())
            .collect();
        // A relation touching what FCS knows and inference does not proves
        // nothing by being consistent here.
        let mut opaque: Option<OpaqueRoots> = None;
        for &(a, b, span) in &relations {
            let roots = self.solver.relation_roots(a, b);
            if roots.is_empty() {
                continue;
            }
            let opaque = match &opaque {
                Some(opaque) => opaque,
                None => opaque.insert(self.opaque_roots(scope)),
            };
            if self.roots_touch_opaque(scope, roots, span, opaque) {
                return false;
            }
        }
        let relations: Vec<(TyVid, TyVid)> =
            relations.into_iter().map(|(a, b, _)| (a, b)).collect();
        let bounded = &scope.bounded_flex;
        let undecided = &scope.undecided_uses;
        let mut consistent = false;
        let tys = self.solver.tys;
        let _ = self.solver.table.probe(|t| -> Result<(), ()> {
            let unified = relations
                .iter()
                .all(|(a, b)| t.unify(tys.var(*a), tys.var(*b)).is_ok());
            consistent = unified && loosened_as_fcs(env, tys, t, bounded, undecided);
            Err(())
        });
        consistent
    }

    /// The loosening half of [`Self::verified`], of the solution `solve`
    /// kept: no bounded loosened variable is fixed and no undecided use takes
    /// a parameter of a type that is not sealed. Of the solution *kept*, not
    /// of every relation applied: a relation `solve` did not apply is
    /// poisoned rather than published, so it cannot make a published use the
    /// wrong one. The probe leaves the table as it was.
    fn loosening_holds(&mut self, scope: &Scope) -> bool {
        let env = self.facts.env;
        let bounded = &scope.bounded_flex;
        let undecided = &scope.undecided_uses;
        let mut holds = false;
        let tys = self.solver.tys;
        let _ = self.solver.table.probe(|t| -> Result<(), ()> {
            holds = loosened_as_fcs(env, tys, t, bounded, undecided);
            Err(())
        });
        holds
    }

    /// Settle the constraints generated **so far** in the current binding,
    /// mid-walk (CE-1): discharge the eager `Eq`s and wake members to a fixpoint,
    /// then put every suspension still pending back on the constraint list,
    /// and every [`ArgCheck`], none of which fires here, on
    /// [`Solver::settled_args`], for the binding's final [`Self::solve`]. Used to
    /// ask whether a local binder or a statement is ground *at that point in
    /// the walk*.
    ///
    /// Only the two constraint kinds that are true regardless of the rest of the
    /// binding are discharged. An `Eq` is a genuine equality, and a member wake
    /// reads nothing but its receiver's type — so discharging either early is
    /// what the final solve would do anyway. An argument check is gated on the
    /// **whole** binding's walk-completeness, which is not known until the walk
    /// ends, so it waits for [`Self::solve`].
    pub(super) fn settle(&mut self, scope: &mut Scope) {
        let outcome = self.run_worklist(scope, Fire::None);
        for m in outcome.members {
            self.solver.constraints.push(Constraint::HasMember {
                recv: m.recv,
                name: m.name,
                result: m.result,
                use_range: m.use_range,
                kind: m.kind,
                args_by: m.args_by,
            });
        }
        debug_assert!(
            self.solver.settled_args.is_empty(),
            "the worklist took them"
        );
        self.solver.settled_args = outcome.args;
    }

    /// The worklist behind [`Self::solve`] and [`Self::settle`]: discharge every
    /// pending constraint to a fixpoint and report what it did, with the
    /// suspensions that never fired. `fire` gates the [`ArgCheck`] wake —
    /// [`Self::settle`] runs without it, since the check's completeness gate is
    /// not yet decided.
    fn run_worklist(&mut self, scope: &mut Scope, fire: Fire) -> WorklistOutcome {
        // Split the batch: discharge every eager `Eq` now, park suspensions.
        let mut suspended: Vec<SuspendedMember> = Vec::new();
        // No argument check fires under `Fire::None`, so the ones a settle
        // parked wait outside the fixpoint, ahead of those this run parks.
        let (held_args, mut suspended_args) = match fire {
            Fire::None => (std::mem::take(&mut self.solver.settled_args), Vec::new()),
            Fire::All => (Vec::new(), std::mem::take(&mut self.solver.settled_args)),
        };
        self.discharge_eqs(scope, &mut suspended, &mut suspended_args);
        let mut outcome = WorklistOutcome::default();
        // The latest end of a discharged check's argument, which a member
        // resolved afterwards may have learned its receiver from.
        let mut latest_discharge: Option<TextSize> = None;

        // Fixpoint: scan the parked members and arg-checks; wake each whose watched
        // variable is now concrete/ready, discharging any `Eq` its wake produces.
        // Repeat while anything fires. Each suspension fires at most once (a woken
        // one is not re-parked), so the loop runs at most `suspended.len() +
        // suspended_args.len()` times.
        loop {
            let mut still_pending: Vec<SuspendedMember> = Vec::new();
            let mut woke = false;
            for m in std::mem::take(&mut suspended) {
                let head = self.solver.table.resolve(self.solver.tys.var(m.recv));
                match self.solver.tys.kind(head) {
                    // Unresolved receiver — keep it parked for a later wake.
                    TyKind::Var(_) => still_pending.push(SuspendedMember {
                        waited_on_receiver: true,
                        ..m
                    }),
                    // A concrete named receiver — attempt the member lookup (a data
                    // member or a method, per `m.kind`), which may push an
                    // `Eq(result, member_ty)` and record the resolved member's
                    // identity at `use_range` (Stage 3.3b). A `false` return is a
                    // *retry* request (an overload whose argument is not yet ground):
                    // re-park without counting as progress, so a later wake that
                    // grounds the argument re-triggers this one. Termination holds —
                    // a retry sets no `woke`, and the number of progress passes is
                    // bounded by the (finite) suspension count.
                    // `wake_member` looks the head up at arity 0, so a GENERIC
                    // receiver must not reach it: with both `N.C` and `N.C<'T>`
                    // in the env it would resolve the nullary one and commit a
                    // member of a type the receiver is not. Making the lookup
                    // arity-aware is what lets these wake; until then they stay
                    // parked, which is the silence they had before `Ty` carried
                    // arguments at all.
                    TyKind::Named { args, .. } if !args.is_empty() => {
                        still_pending.push(m);
                    }
                    TyKind::Named { path, .. } => {
                        let path = path.spelled(self.facts.env.names());
                        if self.wake_member(scope, &path, &m.name, m.result, m.use_range, &m.kind) {
                            woke = true;
                            if m.late(latest_discharge) {
                                outcome.late_member = true;
                            }
                        } else {
                            still_pending.push(m);
                        }
                    }
                    // A field of a project record, read off its declaration.
                    TyKind::Project { .. } if self.wake_record_field(head, &m) => {
                        woke = true;
                        if m.late(latest_discharge) {
                            outcome.late_member = true;
                        }
                    }
                    // Any other concrete head (array / tuple / function / a
                    // quantified param) has no assembly entity to look a member up
                    // on in this stage — drop the constraint (defer, D5). Arrays'
                    // `.Length` is an intrinsic, out of scope.
                    TyKind::Array { .. }
                    | TyKind::Tuple(_)
                    | TyKind::Fun { .. }
                    | TyKind::Param(_) => {
                        woke = true;
                    }
                    // Any other member of a project type is not modelled: drop
                    // the constraint, leaving the access's (poisoned) result
                    // open, and say so in the census, which would otherwise
                    // count the binding complete with an untyped access in it.
                    TyKind::Project { .. } => {
                        woke = true;
                        scope.reasons.push(Incomplete::ProjectTypeMember);
                    }
                }
            }
            suspended = still_pending;

            // Wake arg-checks whose gate is now met. The completeness gate is a
            // constant across the batch (the walk is finished when `solve` runs), so
            // if the binding is incomplete none ever fires — all fall through to the
            // deferred poison in `solve`. A mid-walk `settle` fires none at all. The
            // environment guard
            // ([`Self::arg_check_binds_only_current_vars`]) keeps the discharge
            // from retro-grounding an *earlier* binding's still-open variable.
            let mut still_pending_args: Vec<ArgCheck> = Vec::new();
            for ac in std::mem::take(&mut suspended_args) {
                if fire == Fire::All
                    && scope.complete
                    && (self.no_subsumption_domain(scope, ac.dom)
                        || self.solver.function_domain(ac))
                    && self.arg_check_binds_only_current_vars(scope, ac)
                {
                    // Fire: discharge the arg↔param relation as a genuine equality,
                    // **immediately** (not via the deferred `Eq` queue) so we learn
                    // the outcome now. On success the check discharged (poison
                    // nothing — the relation is modelled). On failure (ill-typed
                    // code) the atomic rollback leaves no trace, and the check is
                    // *undischarged*, so its `arg`/`dom`/`r` are poisoned. Either
                    // way the check has fired and is not re-parked.
                    if self
                        .solver
                        .table
                        .unify_atomic(self.solver.tys.var(ac.arg), self.solver.tys.var(ac.dom))
                        .is_err()
                    {
                        self.poison_arg_check(scope, ac);
                        outcome.failed += 1;
                    } else {
                        outcome.discharged += 1;
                        latest_discharge = latest_discharge.max(Some(ac.span.end()));
                    }
                    woke = true;
                } else {
                    still_pending_args.push(ac);
                }
            }
            suspended_args = still_pending_args;

            // Discharge the `Eq`s a wake produced (grounding the next chain link).
            self.discharge_eqs(scope, &mut suspended, &mut suspended_args);
            // Nothing fired this pass ⇒ no watched variable can become more ready ⇒
            // fixpoint.
            if !woke {
                break;
            }
        }
        outcome.members = suspended;
        outcome.args = held_args;
        outcome.args.extend(suspended_args);
        outcome
    }

    /// Poison an **undischarged** arg check's `arg`, `dom`, and result `r` (Stage
    /// 3.3c): never fired, or fired but its `Eq(arg, dom)` failed. A *ground*
    /// endpoint (e.g. a ground application result) is unaffected — poison bites only
    /// open vars — so ground payoffs still emit.
    fn poison_arg_check(&mut self, scope: &mut Scope, ac: ArgCheck) {
        scope.poison.push(ac.arg);
        scope.poison.push(ac.dom);
        scope.poison.push(ac.r);
    }

    /// The [`Constraint::ArgCheck`] wake's **environment guard**: whether every
    /// *open* variable the `Eq(arg, dom)` discharge could bind was created in
    /// the **current** binding (no union-find class member older than
    /// [`Scope::mark`]). An earlier binder's still-open variable — `g` in
    /// `let g = id` under a later `g 1`, or an open `a` from an unmodelled
    /// `let a = h 0` used as `fb a` — has its type fixed by FCS at *its own*
    /// binding; grounding it through a later use's wake would retro-type it
    /// (`g : int -> int` where FCS keeps `'a -> 'a`) and publish a wrong
    /// `def_type`. The same discipline generalisation already applies via
    /// `any_older_unioned`, now on the wake path. A blocked check stays parked
    /// and falls through to the deferred poison — silence, never wrongness
    /// (D5). Ground endpoints have no open roots, so grounded-argument wakes
    /// (`let n = fb b`) and same-binding instantiations (`let n = id 42`,
    /// `let g y = id y`) fire exactly as before.
    ///
    /// An earlier binder's open variable can also be given a *structure* of
    /// this binding's variables, by an equality of this binding's: applying
    /// it, `oldf x`, makes it `d -> r`. Those variables are then the earlier
    /// binder's, though created here, and grounding `r` retypes it as surely —
    /// `oldf x |> inc` would make `oldf` return `int` where FCS has it return
    /// whatever it does. So no variable the resolution of an environment use
    /// in this binding mentions may be bound either — nor one in a method
    /// call's argument, which FCS fixes against the method's parameter
    /// ([`Scope::opaque`]).
    fn arg_check_binds_only_current_vars(&mut self, scope: &Scope, ac: ArgCheck) -> bool {
        !self.touches_opaque(scope, ac.arg, ac.dom, ac.span)
    }

    /// Whether relating `a` to `b`, at `span` in the source, could bind a
    /// variable whose type FCS decides by something inference does not see:
    /// one older than this binding, or one an [`Scope::opaque`] entry's
    /// resolution mentions — unless that entry is a method argument whose
    /// span holds `span`, a relation FCS checks before it relates the argument
    /// to the method's parameter.
    pub(super) fn touches_opaque(
        &mut self,
        scope: &Scope,
        a: TyVid,
        b: TyVid,
        span: TextRange,
    ) -> bool {
        let roots = self.solver.relation_roots(a, b);
        if roots.is_empty() {
            return false;
        }
        let opaque = self.opaque_roots(scope);
        self.roots_touch_opaque(scope, roots, span, &opaque)
    }

    /// The open variables of every [`Scope::opaque`] entry's resolution, as
    /// roots of their classes: valid until the table next changes.
    ///
    /// There is one entry per environment use in the binding, so a batch of
    /// relations reads them once ([`Self::verified`]): reading them for each
    /// relation would cost a binding of `n` statements `n²`. And a relation
    /// with no open variable ([`Self::touches_opaque`]) does not read them.
    fn opaque_roots(&mut self, scope: &Scope) -> OpaqueRoots {
        let mut out = OpaqueRoots::default();
        for (u, within) in scope.opaque.clone() {
            let roots = self.solver.open_roots(u);
            match within {
                None => out.everywhere.extend(roots),
                Some(arg) => out.outside.push((arg, roots)),
            }
        }
        out
    }

    /// [`Self::touches_opaque`] over a relation's `roots`, against the
    /// [`Self::opaque_roots`] read since the table last changed. Every
    /// variable here is the root of its class, so two are unioned exactly
    /// when they are equal.
    fn roots_touch_opaque(
        &mut self,
        scope: &Scope,
        roots: HashSet<TyVid>,
        span: TextRange,
        opaque: &OpaqueRoots,
    ) -> bool {
        roots.into_iter().any(|v| {
            self.solver.table.any_older_unioned(v, scope.mark)
                || opaque.everywhere.contains(&v)
                || opaque
                    .outside
                    .iter()
                    .any(|(arg, roots)| !arg.contains_range(span) && roots.contains(&v))
        })
    }

    /// Discharge every pending `Eq` in `self.solver.constraints` (best-effort, via
    /// [`InferTable::unify_atomic`]), routing each suspended constraint into its
    /// pending list instead. Shared by [`Self::solve`]'s initial split and its
    /// per-wake re-drain (a wake pushes a fresh `Eq`, and a member/arg wake could
    /// in principle push a fresh suspension).
    fn discharge_eqs(
        &mut self,
        scope: &mut Scope,
        suspended: &mut Vec<SuspendedMember>,
        suspended_args: &mut Vec<ArgCheck>,
    ) {
        for c in std::mem::take(&mut self.solver.constraints) {
            match c {
                Constraint::Eq(a, b) => {
                    if self.solver.table.unify_atomic(a, b).is_err() {
                        scope.eq_failures += 1;
                    }
                }
                Constraint::HasMember {
                    recv,
                    name,
                    result,
                    use_range,
                    kind,
                    args_by,
                } => {
                    suspended.push(SuspendedMember {
                        recv,
                        name,
                        result,
                        use_range,
                        kind,
                        args_by,
                        waited_on_receiver: false,
                    });
                }
                Constraint::ArgCheck(ac) => suspended_args.push(ac),
            }
        }
    }
}
