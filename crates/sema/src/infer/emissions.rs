//! [`Emissions`], in a module of its own so that its fields are reached only
//! through its methods.

use super::Incomplete;
use crate::def::DefId;
use crate::hash::{HashMap, HashSet};
use crate::resolve::Resolution;
use crate::ty::TyVid;
use borzoi_cst::syntax::{SyntaxToken, TextRange};

/// An index into [`Emissions::guards`].
type GuardId = usize;

/// A construct FCS may **reject**, recorded so that what we emit inside its
/// operand can wait on its acceptance. FCS keeps no node and no binder inside
/// the operand of a construct it rejects — a member access on a receiver
/// without that member keeps only the whole access, typed `'a` — so an emission
/// inside one is published only if this guard, and every guard enclosing it,
/// was confirmed ([`Emissions::guard_holds`]).
#[derive(Debug)]
pub(super) struct Guard {
    /// The guard the construct itself sat under.
    parent: Option<GuardId>,
    /// Whether the construct was accepted: set by the event that proves it.
    confirmed: bool,
}

/// What a speculative solve ([`super::Gen::solve`]) recorded here, so that
/// it can be undone ([`Emissions::undo`]). The table's half of the undo
/// is the [`super::Solver`]'s snapshot, which the solve holds: the
/// speculation spans the solver and the emissions.
#[derive(Debug, Default)]
pub(super) struct TrialLog {
    /// Each member resolution recorded, with the one it replaced.
    resolutions: Vec<(TextRange, Option<Resolution>)>,
    /// Each guard confirmed.
    guards: Vec<GuardId>,
}

/// The length of both emission lists, the nodes and the local binders, for
/// a barrier to [discard back to](Emissions::discard_since).
#[derive(Debug, Clone, Copy)]
pub(super) struct EmissionMark {
    exprs: usize,
    locals: usize,
}

/// What the walk emits, for [`super::Gen::finish`] to read: FCS's
/// `tcSink`. The walk only writes it — it appends, discards back to a
/// mark, and opens and confirms guards — and nothing it decides reads it:
/// no method here answers a question about what was emitted.
/// [`Self::into_emitted`] gives `finish` what stands.
///
/// The one write that is not an append is the speculative solve's undo
/// ([`Self::undo`]), which restores the member resolutions and guards a
/// solve that was not kept recorded.
pub(super) struct Emissions {
    /// `(range, var)` per expression whose type to emit; `range` is the read-off
    /// key (and the same range the resolver keyed the occurrence under).
    exprs: Vec<(TextRange, TyVid, Option<GuardId>)>,
    /// Every expression-level `let` binder [`super::Gen::local_binding`] bound (CE-1).
    /// Unlike a declaration's binder, a local sits *inside* an expression, so
    /// its publication is an emission like a node's: [`super::Gen::finish`] publishes
    /// one only if it is still in [`Self::local_emits`].
    local_defs: HashSet<DefId>,
    /// The local binders to publish, in walk order — the binder-side twin of
    /// [`Self::exprs`], and discarded with it by an emission barrier
    /// ([`Self::discard_since`]).
    local_emits: Vec<(DefId, Option<GuardId>)>,
    /// Each `(expr).Member` receiver's variable, keyed by the `DOT_GET_EXPR`'s
    /// range — unguarded and never discarded, for
    /// [`InferredFile::dot_get_receiver_type`](super::InferredFile::dot_get_receiver_type).
    dot_get_receivers: Vec<(TextRange, TyVid)>,
    /// Each begun binding's incompleteness reasons, for
    /// [`InferredFile::incompleteness`](super::InferredFile::incompleteness).
    incompleteness: Vec<Vec<Incomplete>>,
    /// Ranges whose emission [`super::Gen::finish`] drops: a `match` scrutinee's own
    /// node, which FCS may cover with a temporary's `let` typed as the whole
    /// match. Filtered at read-off rather than cut from [`Self::exprs`], whose
    /// positions emission marks record.
    ungraded_ranges: HashSet<TextRange>,
    /// Every [`Guard`] the walk opened, indexed by [`GuardId`].
    guards: Vec<Guard>,
    /// The guards enclosing the current walk position, innermost last; an
    /// emission is tagged with the top one.
    guard_stack: Vec<GuardId>,
    /// The guard each member-name token's wake confirms: segment *i* of an
    /// access confirms the *i*-th guard around its receiver
    /// ([`Self::open_receiver_guards`]). Keyed by the token's range, which is
    /// the member constraint's `use_range`.
    guard_on_member: HashMap<TextRange, GuardId>,
    /// Each successfully-woken member access → its resolved identity, keyed by the
    /// member-name use range (Stage 3.3b). Filled by [`super::Gen::wake_member`] on a
    /// single-candidate discharge; read out in [`super::Gen::finish`] into
    /// [`InferredFile::member_resolutions`](super::InferredFile::member_resolutions).
    member_resolutions: HashMap<TextRange, Resolution>,
    /// The undo log of the speculative solve in progress ([`super::Gen::solve`]).
    trial: Option<TrialLog>,
    /// How many expression levels the walk has open on its recursion now
    /// ([`super::Gen::infer_expr_inner`], [`super::Gen::infer_callee`]), and the most there
    /// have been ([`InferredFile::deepest_expression_walk`](super::InferredFile::deepest_expression_walk)).
    expr_depth: usize,
    deepest_expr_depth: usize,
}

/// What stands of a file's [`Emissions`] once its walk is over
/// ([`Emissions::into_emitted`]).
pub(super) struct Emitted {
    /// The nodes to read back, in walk order: every node whose guards
    /// held, but for the ungraded ones.
    pub(super) exprs: Vec<(TextRange, TyVid)>,
    /// Every local binder the walk bound.
    pub(super) local_defs: HashSet<DefId>,
    /// The local binders no barrier discarded and whose guards held.
    pub(super) published_locals: HashSet<DefId>,
    pub(super) dot_get_receivers: Vec<(TextRange, TyVid)>,
    pub(super) incompleteness: Vec<Vec<Incomplete>>,
    pub(super) member_resolutions: HashMap<TextRange, Resolution>,
    pub(super) deepest_expr_depth: usize,
}

impl Emissions {
    pub(super) fn new() -> Self {
        Emissions {
            exprs: Vec::new(),
            local_defs: HashSet::default(),
            local_emits: Vec::new(),
            dot_get_receivers: Vec::new(),
            incompleteness: Vec::new(),
            ungraded_ranges: HashSet::default(),
            guards: Vec::new(),
            guard_stack: Vec::new(),
            guard_on_member: HashMap::default(),
            member_resolutions: HashMap::default(),
            trial: None,
            expr_depth: 0,
            deepest_expr_depth: 0,
        }
    }

    /// Record the node at `range`, whose type is `v`'s, for read-off,
    /// under the current guard.
    pub(super) fn node(&mut self, range: TextRange, v: TyVid) {
        self.exprs
            .push((range, v, self.guard_stack.last().copied()));
    }

    /// Record the local binder `def` for publication, under the current
    /// guard.
    pub(super) fn local(&mut self, def: DefId) {
        self.local_defs.insert(def);
        self.local_emits
            .push((def, self.guard_stack.last().copied()));
    }

    /// Record the receiver of the `DOT_GET_EXPR` at `range`, whose type is
    /// `v`'s.
    pub(super) fn dot_get_receiver(&mut self, range: TextRange, v: TyVid) {
        self.dot_get_receivers.push((range, v));
    }

    /// Record a walked binding's census entry: why it is incomplete, empty
    /// if it is complete.
    pub(super) fn census(&mut self, reasons: Vec<Incomplete>) {
        self.incompleteness.push(reasons);
    }

    /// Leave ungraded the last node recorded since `mark`, if any.
    pub(super) fn ungrade_last_since(&mut self, mark: EmissionMark) {
        if let Some(&(range, _, _)) = self.exprs[mark.exprs..].last() {
            self.ungraded_ranges.insert(range);
        }
    }

    /// The current length of both emission lists — the expression nodes and the
    /// local binders — for a barrier to [discard back to](Self::discard_since).
    pub(super) fn mark(&self) -> EmissionMark {
        EmissionMark {
            exprs: self.exprs.len(),
            locals: self.local_emits.len(),
        }
    }

    /// Discard every emission recorded since `mark`: the nodes *and* the local
    /// binders a walk inside a barrier produced. A method call is such a barrier
    /// (FCS keeps nothing inside a call it rejects, so we keep nothing inside any
    /// call); the two lists are cut together so a `let` in a call's argument
    /// cannot publish its binder while the argument's nodes are dropped.
    pub(super) fn discard_since(&mut self, mark: EmissionMark) {
        self.exprs.truncate(mark.exprs);
        self.local_emits.truncate(mark.locals);
    }

    /// Open one [`Guard`] per member segment around a member access's receiver,
    /// nested so the innermost — the one the receiver's emissions are tagged
    /// with — holds only if every segment resolves: FCS drops the receiver of
    /// `s.Length.Foo` because `Foo` is rejected, though `Length` is not.
    /// Segment *i*'s wake confirms guard *i* ([`Self::confirm_guard`]). The
    /// caller walks the receiver, then [`Self::close_guards`].
    pub(super) fn open_receiver_guards(&mut self, segments: &[SyntaxToken]) {
        for seg in segments {
            let g = self.guards.len();
            self.guards.push(Guard {
                parent: self.guard_stack.last().copied(),
                confirmed: false,
            });
            self.guard_stack.push(g);
            self.guard_on_member.insert(seg.text_range(), g);
        }
    }

    /// Close the `n` guards [`Self::open_receiver_guards`] opened.
    pub(super) fn close_guards(&mut self, n: usize) {
        let keep = self.guard_stack.len() - n;
        self.guard_stack.truncate(keep);
    }

    /// Confirm the guard the member-name token at `use_range` confirms,
    /// if it has one.
    pub(super) fn confirm_guard(&mut self, use_range: TextRange) {
        if let Some(&g) = self.guard_on_member.get(&use_range)
            && !self.guards[g].confirmed
        {
            self.guards[g].confirmed = true;
            if let Some(log) = &mut self.trial {
                log.guards.push(g);
            }
        }
    }

    /// Whether an emission tagged with `guard` may be published: the guard and
    /// every guard enclosing it were confirmed.
    fn guard_holds(&self, guard: Option<GuardId>) -> bool {
        let mut cur = guard;
        while let Some(g) = cur {
            if !self.guards[g].confirmed {
                return false;
            }
            cur = self.guards[g].parent;
        }
        true
    }

    /// Record that the member-name token at `use_range` resolves to `res`.
    pub(super) fn member_resolution(&mut self, use_range: TextRange, res: Resolution) {
        let previous = self.member_resolutions.insert(use_range, res);
        if let Some(log) = &mut self.trial {
            log.resolutions.push((use_range, previous));
        }
    }

    /// Start logging what is recorded here, for a speculative solve.
    pub(super) fn begin_trial(&mut self) {
        self.trial = Some(TrialLog::default());
    }

    /// Stop logging, and return what was logged since
    /// [`Self::begin_trial`].
    pub(super) fn end_trial(&mut self) -> TrialLog {
        self.trial.take().unwrap_or_default()
    }

    /// Undo what a speculative solve recorded ([`TrialLog`]), latest first.
    pub(super) fn undo(&mut self, log: TrialLog) {
        for (range, previous) in log.resolutions.into_iter().rev() {
            match previous {
                Some(res) => {
                    self.member_resolutions.insert(range, res);
                }
                None => {
                    self.member_resolutions.remove(&range);
                }
            }
        }
        for g in log.guards {
            self.guards[g].confirmed = false;
        }
    }

    /// Count one more expression level open on the walk's recursion
    /// ([`InferredFile::deepest_expression_walk`](super::InferredFile::deepest_expression_walk));
    /// the caller closes it with [`Self::leave_expression`].
    pub(super) fn enter_expression(&mut self) {
        self.expr_depth += 1;
        self.deepest_expr_depth = self.deepest_expr_depth.max(self.expr_depth);
    }

    /// Close the expression level [`Self::enter_expression`] opened.
    pub(super) fn leave_expression(&mut self) {
        self.expr_depth -= 1;
    }

    /// What stands of the walk's emissions: what a guard that never held,
    /// a barrier or an ungraded range dropped is gone.
    pub(super) fn into_emitted(self) -> Emitted {
        let published_locals = self
            .local_emits
            .iter()
            .filter(|(_, guard)| self.guard_holds(*guard))
            .map(|&(def, _)| def)
            .collect();
        let exprs = self
            .exprs
            .iter()
            .filter(|&&(range, _, guard)| {
                self.guard_holds(guard) && !self.ungraded_ranges.contains(&range)
            })
            .map(|&(range, var, _)| (range, var))
            .collect();
        Emitted {
            exprs,
            local_defs: self.local_defs,
            published_locals,
            dot_get_receivers: self.dot_get_receivers,
            incompleteness: self.incompleteness,
            member_resolutions: self.member_resolutions,
            deepest_expr_depth: self.deepest_expr_depth,
        }
    }
}
