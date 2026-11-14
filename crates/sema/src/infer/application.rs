//! Applications: a callee, its arguments, and infix operators.

use super::*;

impl<'a> Gen<'a> {
    /// Type a **modelled** (non-infix) function application `f x`, returning its
    /// **result** variable `r` (Stage 3.2c-3). It generates the application's
    /// constraints but records **no** node — the caller decides emission, since
    /// whether the node is emitted depends on position: a top-level application in a
    /// synth position emits `r`, but an application in the **function position** of
    /// an outer application does not (FCS collapses a curried spine into one typed
    /// node at the whole application — confirmed by probing the `types` oracle).
    ///
    /// The function position is synthesized by [`Self::infer_callee`] (which also
    /// records no node — FCS emits no typed node at a bare function-position use).
    /// Fresh domain `d` and result `r` variables are created and `Eq(tf, Fun(d, r))`
    /// is pushed — a **genuine** equality: applying an in-file value forces a
    /// function shape (an in-file binder cannot be a method group). The argument is
    /// walked in **check mode** against `d` via [`Self::infer_arg`], which opts the
    /// application path *out* of the poison wrapper's automatic poison of `d`/arg —
    /// F# relates them by *subsumption*, and 3.3c defers that relation as a
    /// suspended [`Constraint::ArgCheck`] rather than poisoning it eagerly. `r` is
    /// D5-safe to emit: it is fixed by the `Eq` against the function's *own* type,
    /// independent of how the argument coerces.
    ///
    /// Application does **not** mark the binding incomplete: the construct is
    /// modelled, and the dropped argument↔parameter relation is exactly what the
    /// `ArgCheck` (and the deferred poison on undischarged ones) accounts for. (An
    /// argument that itself fails to walk clears completeness via the existing
    /// per-arm rule, which is correct — and, having no `arg` endpoint, falls back to
    /// the 3.2c-3 eager `d`/`r` poison.)
    ///
    /// The polymorphic gap 3.2c-3 left open is closed by the wake (Stage 3.3c):
    /// `let g y = id y` generalises to `'a -> 'a` because the `ArgCheck` discharges
    /// (`id`'s domain is a scheme-instantiation var of ours, a no-subsumption
    /// domain), grounding `y ↔ d ↔ r`. A ground `f : bool -> int` applied to any
    /// argument still grounds `r` (a ground var is unaffected by poison). See
    /// [`Self::solve`] for the wake rule and its completeness gate.
    pub(super) fn infer_app(&mut self, scope: &mut Scope, app: &AppExpr) -> Option<TyVid> {
        let Some(func) = app.func() else {
            scope.mark_incomplete(Incomplete::Recovery);
            return None;
        };
        let Some(arg) = app.arg() else {
            scope.mark_incomplete(Incomplete::Recovery);
            return None;
        };
        // A **method call** `recv.Method(args)` is an application whose callee is a
        // member access (Stage 3.3d). `infer_callee` never modelled such a callee
        // (it always deferred), so routing it to method typing only adds coverage —
        // no value-application emission changes.
        // A module-qualified function (`List.map f`, `(List.map) f`) is a
        // value, applied with value semantics, not a method call.
        let module_value = matches!(
            unparenthesize(func.clone()),
            Some(Expr::LongIdent(li)) if self.module_value_path(&li).is_some()
        );
        // Asked first: a prefix operator (`-a`) is a bare `LONG_IDENT_EXPR`
        // callee, which is otherwise read as a member access.
        if let Some(r) = self.builtin_application(scope, &func, &arg) {
            return r;
        }
        if !module_value && is_member_access_callee(&func) {
            return self.infer_method_call(scope, &func, &arg);
        }
        // Synthesize the function position (no node emitted).
        let tf = self.infer_callee(scope, &func)?;
        Some(self.apply(scope, tf, &arg))
    }

    /// Apply a synthesized function `tf` to `arg`: the half of
    /// [`Self::infer_app`] after the callee, shared with the infix operator
    /// application ([`Self::infer_infix`]). Returns the result variable.
    fn apply(&mut self, scope: &mut Scope, tf: TyVid, arg: &Expr) -> TyVid {
        let d = self.solver.table.fresh();
        let r = self.solver.table.fresh();
        // Genuinely true: applying `func` forces it to a function shape.
        self.eq(
            scope,
            self.solver.tys.var(tf),
            self.solver
                .tys
                .fun(self.solver.tys.var(d), self.solver.tys.var(r)),
        );
        // The argument is a check position against the domain `d`, so it is walked
        // in check mode (its node is not emitted) — but **without** the poison
        // wrapper's automatic poison of `d`/arg ([`Self::infer_arg`]). Stage 3.3c
        // replaces that eager poison with a *suspended* `ArgCheck`: the dropped
        // subsumption relation is discharged as a genuine `Eq(arg, d)` when it is
        // provably coercion-free (a walk-complete binding and a no-subsumption
        // domain), and only poisoned if it stays undischarged. The argument's var,
        // if we could synthesize one, is the `ArgCheck`'s `arg` endpoint.
        let arg_var = self.infer_arg(scope, arg, d);
        match arg_var {
            Some(arg_var) => {
                // Suspend the arg↔param relation. `solve` wakes it (a complete
                // binding + a no-subsumption `d`) or, failing that, poisons
                // `arg`/`d`/`r` — so an unwoken relation never generalises anything.
                self.solver.constraints.push(Constraint::ArgCheck(ArgCheck {
                    arg: arg_var,
                    dom: d,
                    r,
                    span: self.facts.node_span(arg.syntax()),
                }));
            }
            None => {
                // The argument did not synthesize a variable (an unmodelled
                // shape — the arg's own recursion already marked incomplete). There
                // is no `arg` endpoint to relate, so fall back to the 3.2c-3 eager
                // poison of `d`/`r`: an unwoken relation must not let either
                // generalise. A *ground* `r` (a ground function's result) is
                // unaffected, so ground payoffs still emit.
                scope.poison.push(d);
                scope.poison.push(r);
            }
        }
        r
    }

    /// The partial application `OP lhs` inside an infix application
    /// `lhs OP rhs` — the parser's `App(App(OP, lhs), rhs)`, whose inner
    /// application `inner` is marked infix — returning its result variable,
    /// which the outer application applies to `rhs`. FCS checks the two
    /// exactly as it checks `OP lhs rhs`: an application of the operator's
    /// value, the left operand first. So this is [`Self::apply`] over the
    /// operator's value ([`Self::operator_value`]), and the outer application
    /// is an ordinary one; neither records a node inside the application, as
    /// for any curried call.
    fn infer_infix(&mut self, scope: &mut Scope, inner: &AppExpr) -> Option<TyVid> {
        let (Some(op), Some(lhs)) = (inner.func(), inner.arg()) else {
            scope.mark_incomplete(Incomplete::Recovery);
            return None;
        };
        let tf = self.operator_value(scope, &op)?;
        Some(self.apply(scope, tf, &lhs))
    }

    /// The type variable of an infix operator's value, as [`Self::infer_callee`]
    /// gives an applied name's: the resolver decides which definition the
    /// token names, so a shadowing one — in this file, in an opened module,
    /// spelled as the compiled name (`op_PipeRight`) — is the operator, and
    /// FSharp.Core's is reached only when nothing shadows it.
    ///
    /// - **An in-file binder** is an applied value like any other
    ///   ([`Self::value_use`]).
    /// - **An assembly's operator** — a member of one of its F# modules — is a
    ///   use of an assembly value ([`Self::nonlocal_value_use`]). Only a
    ///   signature with no constraint on any type parameter qualifies:
    ///   instantiating a constrained one as a plain variable would claim a
    ///   constraint is modelled that is not, which is what walk-completeness
    ///   rests on.
    /// - **A preceding file's operator** is a use of the type that file
    ///   published for it ([`Self::nonlocal_value_use`]).
    ///
    /// Anything else — a deferred lookup, a signature that does not carry
    /// over, a preceding file's operator it published no type for — declines,
    /// marking the binding incomplete with the reason.
    fn operator_value(&mut self, scope: &mut Scope, op: &Expr) -> Option<TyVid> {
        let tok = match op {
            Expr::LongIdent(li) => li.long_ident().and_then(|li| {
                let mut idents = li.idents();
                match (idents.next(), idents.next(), li.active_pat_names().next()) {
                    (Some(tok), None, None) => Some(tok),
                    _ => None,
                }
            }),
            _ => None,
        };
        let Some(tok) = tok else {
            scope.mark_incomplete(Incomplete::CalleeShape(op.syntax().kind()));
            return None;
        };
        if let Some(def) = self.facts.def_at(tok.text_range()) {
            if self.declines_use(scope, def) {
                return None;
            }
            return Some(self.value_use(scope, def));
        }
        match self.nonlocal_value_use(scope, tok.text_range()) {
            Ok(v) => Some(v),
            Err(why) => {
                let why = match why {
                    Incomplete::AssemblyValue(FunctionTyGap::Constrained) => {
                        Incomplete::Operator(OperatorGap::Constrained)
                    }
                    Incomplete::AssemblyValue(_) => Incomplete::Operator(OperatorGap::Signature),
                    // A preceding file's operator, untyped there: its own reason.
                    preceding @ Incomplete::PrecedingValue(_) => preceding,
                    _ => Incomplete::Operator(OperatorGap::Unresolved),
                };
                scope.mark_incomplete(why);
                None
            }
        }
    }

    /// Walk a modelled application's **argument** in check mode against the
    /// domain `d`, returning its synthesized variable, but **suppressing** the
    /// [`Self::infer_expr`] poison wrapper's poison of the top-level arg↔`d`
    /// relation (Stage 3.3c). At an application site that relation is not poisoned
    /// eagerly; it becomes a suspended [`Constraint::ArgCheck`] the solver may
    /// later discharge. Every *other* check site keeps the wrapper's poison — only
    /// this application path opts out.
    ///
    /// Two shapes get the opt-out, since they preserve the "this whole thing is the
    /// argument" position transparently:
    /// - **Parentheses** — peeled and recursed as an argument (a member access
    ///   never coerces its receiver; a paren never coerces its content), so
    ///   `id (id x)`'s inner application relates its result to the outer domain via
    ///   an `ArgCheck`, not the wrapper's poison.
    /// - **A nested application** in argument position — `infer_construct`'s
    ///   `App` arm ([`Self::infer_app`]) already suspends its *own* arg and returns
    ///   its result without emitting a node.
    ///
    /// Any other argument shape recurses through [`Self::infer_expr_inner`] once,
    /// so its *sub*-positions (e.g. a tuple's elements, an `if`'s branches) keep
    /// the wrapper's poison — the subsumption dropped *there* is genuinely eager;
    /// only the top-level arg↔`d` relation is deferred.
    pub(super) fn infer_arg(&mut self, scope: &mut Scope, arg: &Expr, d: TyVid) -> Option<TyVid> {
        match arg {
            Expr::Paren(p) => match p.inner() {
                Some(inner) => self.infer_arg(scope, &inner, d),
                None => {
                    scope.mark_incomplete(Incomplete::Recovery);
                    None
                }
            },
            _ => self.infer_expr_inner(scope, arg, Some(d)),
        }
    }

    /// Synthesize the **function position** of an application, returning its type
    /// variable **without recording an expression node** (Stage 3.2c-3). FCS emits
    /// no typed node at a bare function-position use (`f` in `f x`, `add` in
    /// `add true false`) — only at the whole application — so emitting one would key
    /// a type at a range where FCS has no node and fail the differential. The callee
    /// spine is:
    ///
    /// - **A value/function reference** ([`Expr::Ident`]): resolve it; a
    ///   generalised binder instantiates its scheme afresh, a plain in-file binder
    ///   contributes its variable (poisoned if it is an *environment* reference, as
    ///   in [`Self::infer_construct`]'s ident arm). No node is emitted.
    /// - **Parentheses** ([`Expr::Paren`]): transparent — peel and recurse.
    /// - **A nested application** (curried `f x y` = `App(App(f, x), y)`): the inner
    ///   `App(f, x)` is itself a callee, so recurse through [`Self::infer_app`]-style
    ///   logic with its own node emission suppressed (an inner curried application
    ///   carries no FCS node either).
    ///
    /// Any other callee shape (an applied `DotGet`, a lambda, a literal) is
    /// unmodelled ⇒ incomplete, and the whole application defers.
    fn infer_callee(&mut self, scope: &mut Scope, e: &Expr) -> Option<TyVid> {
        self.out.enter_expression();
        let result = self.infer_callee_spine(scope, e);
        self.out.leave_expression();
        result
    }

    /// [`Self::infer_callee`] without the level count.
    fn infer_callee_spine(&mut self, scope: &mut Scope, e: &Expr) -> Option<TyVid> {
        match e {
            Expr::Paren(p) => {
                let Some(inner) = p.inner() else {
                    scope.mark_incomplete(Incomplete::Recovery);
                    return None;
                };
                self.infer_callee(scope, &inner)
            }
            // A nested (curried) application in function position: type it as an
            // application whose residual `r` is the callee type, recording **no**
            // node ([`Self::infer_app`] never emits — an inner curried application
            // carries no FCS node either). It still pushes its own
            // `Eq(tf, Fun(d, r))` and checks its argument. A bracket-indexer
            // `App` in the callee spine is unmodelled (⇒ the catch-all below
            // marks incomplete and the whole application defers).
            Expr::App(inner) if !inner.is_infix() && !inner.is_bracket_indexer() => {
                self.infer_app(scope, inner)
            }
            // The operator applied to its left operand, inside `lhs OP rhs`.
            Expr::App(inner) if inner.is_infix() => self.infer_infix(scope, inner),
            // A value/function reference: mirror the ident arm's resolution, but
            // *do not* emit a node.
            Expr::Ident(ident) => {
                let Some(tok) = ident.ident() else {
                    scope.mark_incomplete(Incomplete::Recovery);
                    return None;
                };
                let def = self
                    .facts
                    .def_at(tok.text_range())
                    .filter(|_| !self.facts.own_union_case_at(tok.text_range()));
                let Some(def) = def else {
                    return self.nonlocal_value_or_incomplete(scope, tok.text_range());
                };
                if self.declines_use(scope, def) {
                    return None;
                }
                Some(self.value_use(scope, def))
            }
            // A module-qualified value (`List.map f`).
            Expr::LongIdent(li) => match self.module_value_path(li) {
                Some(whole) => self.nonlocal_value_or_incomplete(scope, whole),
                None => {
                    scope.mark_incomplete(Incomplete::CalleeShape(e.syntax().kind()));
                    None
                }
            },
            _ => {
                scope.mark_incomplete(Incomplete::CalleeShape(e.syntax().kind()));
                None
            }
        }
    }
}

/// Whether an application's callee is a **member access** — the shape a method
/// call `recv.Method(args)` has in function position (Stage 3.3d). A dotted
/// `LONG_IDENT_EXPR` (`s.M`) or a `DOT_GET_EXPR` (`"hi".M`) callee routes to
/// method typing; parentheses are transparent. Any other callee (a bare value
/// `f`, `(+)` or `(|P|_|)`, a nested application) is an ordinary value
/// application. Pure classification (no
/// side effects), so [`Gen::infer_app`] can commit to the method path only when it
/// applies; [`Gen::method_callee`] then does the resolution and may still defer.
fn is_member_access_callee(e: &Expr) -> bool {
    match e {
        // A single segment — an operator or active-pattern name in
        // parentheses, `(+)` or `(|P|_|)` — names a value, and so does a path
        // through an active-pattern segment: neither is a member access.
        Expr::LongIdent(li) => li
            .long_ident()
            .is_some_and(|l| l.active_pat_names().next().is_none() && l.idents().nth(1).is_some()),
        Expr::DotGet(_) => true,
        Expr::Paren(p) => p
            .inner()
            .is_some_and(|inner| is_member_access_callee(&inner)),
        _ => false,
    }
}
