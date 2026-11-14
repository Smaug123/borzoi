//! The walk over a file's module-level `let` declarations, and each
//! binding's head: its parameters and its function type.

use super::*;

impl<'a> Gen<'a> {
    /// Fold `file` into per-binding constraint batches, solving each before the
    /// next (Algorithm W's sequential order — the generation half of D8, now at
    /// binding granularity). The walk visits `LET_DECL`s in **document order**,
    /// which is F#'s sequential checking order, so a later binding's use sees the
    /// scheme an earlier function binding published.
    ///
    /// `descendants()` reaches only ordinary `LET_DECL`s: an expression-level
    /// `let … in` is a distinct `LET_OR_USE_EXPR`, never cast by [`LetDecl`], so no
    /// binding nests inside another *walked* binding's RHS — the sequential-solve
    /// invariant. A `debug_assert` pins it (a `LET_DECL` descendant of a binding's
    /// RHS would break the ordering assumption).
    ///
    /// A `let` FCS drops unchecked, directly in a namespace
    /// ([`crate::resolve::dropped_in_namespace`]), is not walked: FCS types
    /// nothing in it.
    pub(super) fn walk(&mut self, file: &ImplFile) {
        let dropped = crate::resolve::dropped_in_namespace(file);
        for node in file.syntax().descendants() {
            if let Some(let_decl) = LetDecl::cast(node)
                && !dropped.contains(&let_decl.syntax().text_range())
            {
                self.let_binding(&let_decl);
            }
        }
    }

    /// A `let`/`let rec` declaration. Only ordinary `LET_DECL`s reach here, not
    /// expression-level `let … in` / `use` / the computation-expression binders
    /// (`LET_OR_USE_EXPR`), whose RHS *does* get an expected type from the
    /// builder's `Bind`. Each binding is solved and finalised on its own (Stage
    /// 3.2c-2c): for a value binding, type the RHS and link it to the binder's
    /// variable so a `let z = y` chain propagates; for a function binding, walk the
    /// body, reunify parameter slots on a complete binding, then **generalise** or
    /// emit the function type.
    fn let_binding(&mut self, let_decl: &LetDecl) {
        // A declaration is typed only if nothing up to its end recovered.
        // Recovery can spill past the declaration it broke out of — `let f
        // (s: string)) p =` leaves its indented body's `let`s standing as
        // apparently intact top-level declarations, which FCS never checks —
        // so no declaration after a recovery can be shown to be one FCS
        // typed. A broken declaration's own binders are skipped the same way.
        // Past the type budget nothing more is typed: the file publishes
        // nothing ([`Self::finish`]), so walking on would only spend more.
        if self.solver.table.over_budget().is_none()
            && self
                .facts
                .resolved
                .recovery()
                .clean_through(let_decl.syntax())
        {
            self.type_let_decl(let_decl);
        }
        for binding in let_decl.bindings() {
            self.own_binders(&binding);
        }
    }

    /// Give every name a module-level binding binds its variable now, in its
    /// own declaration, whether or not the declaration was typed.
    ///
    /// A binder's variable is otherwise created lazily, at its first use —
    /// and a use in a later binding would then allocate it *after* that
    /// binding's mark, where it passes for the later binding's own variable:
    /// the guards that stop a binding grounding an earlier binder's open
    /// variable ([`Self::arg_check_binds_only_current_vars`],
    /// [`Self::mentions_inherited_open`]) key on allocation age, and would let
    /// `mono u` retype a `let rec u` as `mono`'s domain. Parameters are not
    /// binders of the declaration and are left to their function.
    fn own_binders(&mut self, binding: &Binding) {
        let inline = binding.is_inline();
        let header_rejects = self.facts.in_rejecting_header(binding.syntax());
        for def in self.binding_defs(binding) {
            self.def_var(def);
            self.binders.module_binder(
                def,
                ModuleBinder {
                    inline,
                    header_rejects,
                },
            );
        }
    }

    /// Every binder a module-level binding's head binds — the names of a
    /// value pattern, or a function's name — but not its parameters.
    fn binding_defs(&self, binding: &Binding) -> Vec<DefId> {
        let Some(pat) = binding.pat() else {
            return Vec::new();
        };
        let names: Vec<NamedPat> = match &pat {
            Pat::LongIdent(head)
                if head.args().next().is_some() || head.name_pat_pairs().is_some() =>
            {
                Vec::new()
            }
            _ => pat.syntax().descendants_of::<NamedPat>().collect(),
        };
        let mut defs: Vec<DefId> = names
            .iter()
            .filter_map(|n| n.ident())
            .filter_map(|tok| self.facts.def_at(tok.text_range()))
            .collect();
        if let Pat::LongIdent(head) = &pat
            && let Some(name) = head.head().and_then(|li| li.idents().last())
            && let Some(def) = self.facts.def_at(name.text_range())
        {
            defs.push(def);
        }
        defs
    }

    /// Type one module-level `let` declaration ([`Self::let_binding`]).
    fn type_let_decl(&mut self, let_decl: &LetDecl) {
        // A recursive group (`let rec … and …`) is solved as a unit: a sibling
        // binding's constraints can flow back to a binder, so a literal RHS is
        // *not* isolated and its type may be retargeted. Defer the whole group
        // (D5); its binders get no typed variable, so uses of them stay Deferred.
        if let_decl.is_rec() {
            return;
        }
        // A binding that may carry `[<EntryPoint>]` is not typed at all
        // ([`Self::may_be_entry_point`]); its binders stay open, and a later
        // use of one is an environment reference that never generalises.
        if self.facts.may_be_entry_point(let_decl.syntax()) {
            return;
        }
        for binding in let_decl.bindings() {
            if self.facts.may_be_entry_point(binding.syntax()) {
                continue;
            }
            // `let mutable f x = …` is FS0831: FCS types the function
            // monomorphically at `obj`, which nothing here models.
            if binding.is_mutable()
                && matches!(binding.pat(), Some(Pat::LongIdent(head)) if head.args().next().is_some())
            {
                continue;
            }
            // A return-type annotation (`let x : T = …` / `let f x : T = …`)
            // supplies an expected type that checks — and may retarget — the RHS
            // or body, a coercion-possible position: the RHS is never walked
            // here. But the annotation itself types the **binder** (Stage R2-a):
            // the binder↔annotation relation is a positionally-fixed truth even
            // on ill-typed code (`let x : int64 = "s"` still has
            // `x : System.Int64` in FCS — the RHS is the error site, the binder
            // is not), so a simple-named value binder whose annotation provably
            // denotes a primitive alias is bound to it. Function bindings with
            // return annotations stay skipped (their body↔annotation relation is
            // subsumption — Stage R2-c), as do mutable binders (v1 scope).
            // From here the binding is walked for its type, whether or not it
            // finds one.
            if binding.return_type().is_some() || binding.expr().is_some() {
                let walked = self.binding_defs(&binding);
                self.binders.walked(walked);
            }
            // The binding's scope is entered before any of its variables is
            // allocated, and closed once it is solved.
            let mut scope = self.enter_scope();
            let census = if let Some(ann) = binding.return_type() {
                self.annotated_value_binding(&mut scope, &binding, &ann)
            } else if let Some(rhs) = binding.expr() {
                self.unannotated_binding(&mut scope, &binding, &rhs)
            } else {
                Census::Discard
            };
            self.close_scope(scope, census);
        }
    }

    /// A binding with no return annotation, whose right-hand side is `rhs`
    /// ([`Self::type_let_decl`]), walked and solved in `scope`.
    fn unannotated_binding(&mut self, scope: &mut Scope, binding: &Binding, rhs: &Expr) -> Census {
        // The RHS of a real `LET_DECL` never itself contains a `LET_DECL`
        // (expression-level lets are `LET_OR_USE_EXPR`), so scopes do not
        // nest.
        debug_assert!(
            rhs.syntax()
                .descendants_where(|k| k == SyntaxKind::LET_DECL)
                .next()
                .is_none(),
            "a LET_DECL nested inside a walked binding's RHS breaks sequential solve"
        );
        match binding.pat() {
            // A value binding `let x = <rhs>`. The RHS is a coercion-free
            // position (no expected type), so infer it in synth mode and link
            // the binder's variable to it so a use picks up the type (the
            // binder's `DefId` comes from its own name occurrence — binders
            // self-resolve). A *typed* pattern (`let (x: T) = …`) is a
            // `Pat::Typed`, not `Pat::Named`, so it is excluded here. A value
            // binding does **not** generalise (the value restriction — F#
            // rejects `let g = id` / `let x = []` with FS0030), so it just
            // links and solves.
            Some(Pat::Named(named)) => {
                let rhs_var = self.infer_expr(scope, rhs, None);
                if let Some(tok) = named.ident()
                    && let Some(def) = self.facts.def_at(tok.text_range())
                {
                    // Create the binder's variable in *its own* binding — even
                    // when the RHS did not type — so it carries an index below
                    // any later binding's mark. A later function that references
                    // this value then sees an inherited-open (environment)
                    // variable and defers via the mark check, rather than
                    // wrongly quantifying a value whose type FCS determines
                    // through a constraint we did not model (`let a = f 0 …
                    // let g x = (x, a)`).
                    let dv = self.def_var(def);
                    if let Some(rhs_var) = rhs_var {
                        self.solver
                            .eq(self.solver.tys.var(dv), self.solver.tys.var(rhs_var));
                    }
                }
                self.solve(scope);
                Census::Record
            }
            // A function binding `let f x … = <body>` — a `LongIdentPat` head
            // with arguments. The parameter slot variables are collected
            // *first* (each simple-named parameter gets a private slot recorded
            // in `param_slots`, so the body's condition typing can ground that
            // slot); then the body is walked in synth mode for its own
            // emissions (e.g. a body that is an `if` emits its result), and its
            // return variable curries with the parameter slots into the
            // function's own `TyKind::Fun` type ([`Self::function_type`]). The
            // arguments span both `SynArgPats` shapes, matching `binders`'s
            // function-head test: the curried list (`args()`) and the
            // named-field group `Case (field = pat; …)` (`name_pat_pairs()`,
            // whose own `args()` is empty). Parameters bind lazily via name
            // resolution at their uses.
            Some(Pat::LongIdent(head))
                if head.args().next().is_some() || head.name_pat_pairs().is_some() =>
            {
                scope.body = Some(rhs.syntax().clone());
                let head_typars = self.open_head_typars(scope, &head, None, Some(rhs));
                let arg_vars =
                    self.param_vars(scope, &head, AnnotationCx::head(head_typars.as_ref()));
                // FCS binds a parameter directly only if it is simple
                // ([`is_simple_param`]); any other it elaborates through a
                // `match` whose target is the body, and the typed tree then
                // keys the body's nodes elsewhere (`(a, ())` moves the
                // body's root to the `()`). The body's type is still the
                // function's result, exactly, so it is walked as usual, but
                // nothing it records is kept.
                let body_mark = self.out.mark();
                let ret = self.infer_expr(scope, rhs, None);
                if !head.args().all(|arg| is_simple_param(&arg)) {
                    self.out.discard_since(body_mark);
                }
                let f_def = self.function_type(scope, &head, &arg_vars, ret);
                // On a complete binding, undo the 2b slot/binder decoupling so
                // `let id x = x` generalises to `'a -> 'a`, not `'a -> 'b`.
                if scope.complete {
                    self.slot_binder_reunify(scope);
                }
                self.solve(scope);
                self.withhold_if_not_rigid(scope, f_def);
                if let Some(f_def) = f_def {
                    self.finalise_function(scope, f_def);
                }
                Census::Record
            }
            // A parenthesised head: a **trivial typed pattern**
            // `let (x: T) = …` (`Paren > Typed > Named`) rides the Stage
            // R2-a annotation gate — same positionally-fixed
            // binder↔annotation truth as the return-annotation form, with
            // the same R2-e coverage check-walk over the RHS. Any other
            // parenthesised shape keeps today's catch-all behaviour.
            Some(Pat::Paren(paren)) if trivial_typed_head(&paren).is_some() => {
                match trivial_typed_head(&paren) {
                    Some((named, ty)) => {
                        self.bind_annotated_named(scope, &named, &ty, Some(rhs.clone()))
                    }
                    None => Census::Discard,
                }
            }
            // A pattern value binding `let (a, b) = <rhs>`, typed like a
            // plain value binding: the pattern's binders are linked to the
            // RHS through the pattern's structure ([`Self::pattern_rhs`]),
            // and nothing generalises. A shape [`Self::pattern_ty`] rejects
            // is not walked at all, and a later use of a binder it leaves
            // open is an environment reference, which is poisoned and so
            // never generalises.
            Some(pat @ (Pat::Paren(_) | Pat::Tuple(_))) => {
                let census = match self.binding_pattern_ty(scope, binding, &pat, BinderSite::Module)
                {
                    Ok(pat_ty) => {
                        self.pattern_rhs(scope, &pat, pat_ty, rhs, BinderSite::Module);
                        Census::Record
                    }
                    // Not walked, but counted: an annotation in the
                    // pattern is the binding's own, so the census keeps
                    // it under that reason.
                    Err(Incomplete::PatternAnnotation) => {
                        scope.mark_incomplete(Incomplete::BindingAnnotation);
                        Census::Record
                    }
                    Err(_) => Census::Discard,
                };
                self.solve(scope);
                census
            }
            // Not walked: no census entry.
            _ => {
                self.solve(scope);
                Census::Discard
            }
        }
    }

    /// An annotated **value** binding `let x : T = …` (Stage R2-a) or an
    /// annotated **function** binding `let f x : T = …` (Stage R2-c). The value
    /// form's binder is bound to the annotation's type iff the shape is the
    /// modelled one — a non-mutable, simple-named head whose annotation passes
    /// [`FileFacts::annotation_ty`] — and its RHS is check-walked for coverage
    /// (Stage R2-e; see [`Self::bind_annotated_named`]). The function form
    /// dispatches to [`Self::annotated_function_binding`]. Anything else stays
    /// silent, exactly like the pre-R2-a blanket skip (D5), with no census
    /// entry.
    fn annotated_value_binding(
        &mut self,
        scope: &mut Scope,
        binding: &Binding,
        ann: &Type,
    ) -> Census {
        // Mutable binders are deliberately out of scope in v1 (the
        // set-site↔annotation interaction is unaudited; see the plan's
        // out-of-scope list).
        if binding.is_mutable() {
            return Census::Discard;
        }
        match binding.pat() {
            Some(Pat::Named(named)) => {
                self.bind_annotated_named(scope, &named, ann, binding.expr())
            }
            // A function head with a return annotation (Stage R2-c).
            Some(Pat::LongIdent(head))
                if head.args().next().is_some() || head.name_pat_pairs().is_some() =>
            {
                self.annotated_function_binding(scope, &head, binding.expr(), ann)
            }
            // Any other pattern shape: skipped as before.
            _ => Census::Discard,
        }
    }

    /// An annotated **function** binding `let f x : T = body` (Stage R2-c).
    /// The return annotation is a positionally-fixed truth on the function's
    /// *return slot* — even on an ill-typed body, FCS's `f` carries the
    /// annotated return (`let f x : int = "s"` is `'a -> int`) — but the
    /// body↔annotation relation is **subsumption** (`let f x : obj = "hi"` is
    /// legal), so the body is walked in *check* mode against the annotation
    /// and the dropped top-level relation is suspended as a
    /// [`Constraint::ArgCheck`], exactly the application argument's rule: it
    /// discharges as a genuine equality only on a walk-complete binding whose
    /// annotation resolves to a **no-subsumption** type (sealed primitives and
    /// tuples of such — R6's `let h x : int = x` grounds `x` through it), and
    /// an undischarged check poisons its endpoints so nothing bogus
    /// generalises. The function type curries the parameter slots over the
    /// annotation variable, so a ground parameter side emits the annotated
    /// return even when the body relation stays dropped
    /// (`let f (b: bool) : obj = b` ⇒ `bool -> obj`).
    ///
    /// An annotation outside the gate keeps the pre-R2-c whole-binding skip:
    /// nothing is walked and nothing is constrained.
    fn annotated_function_binding(
        &mut self,
        scope: &mut Scope,
        head: &LongIdentPat,
        rhs: Option<Expr>,
        ann: &Type,
    ) -> Census {
        let head_typars = self.open_head_typars(scope, head, Some(ann), rhs.as_ref());
        let cx = AnnotationCx::head(head_typars.as_ref());
        let Some(t) = self.facts.annotation_ty(self.solver.tys, ann, cx) else {
            // Not walked, but counted ([`Incomplete::BindingAnnotation`]).
            scope.mark_incomplete(Incomplete::BindingAnnotation);
            return Census::Record;
        };
        scope.body = rhs.as_ref().map(|rhs| rhs.syntax().clone());

        let arg_vars = self.param_vars(scope, head, cx);
        let r = self.solver.table.fresh();
        self.solver.eq(self.solver.tys.var(r), t);
        if let Some(rhs) = rhs {
            debug_assert!(
                rhs.syntax()
                    .descendants_where(|k| k == SyntaxKind::LET_DECL)
                    .next()
                    .is_none(),
                "a LET_DECL nested inside a walked binding's body breaks sequential solve"
            );
            // The body walk: check mode against the annotation, the top-level
            // relation suspended rather than eagerly poisoned (the
            // [`Self::infer_arg`] opt-out, shared with application arguments).
            if let Some(bv) = self.infer_arg(scope, &rhs, r) {
                self.solver.constraints.push(Constraint::ArgCheck(ArgCheck {
                    arg: bv,
                    dom: r,
                    r,
                    span: self.facts.node_span(rhs.syntax()),
                }));
            }
        } else {
            scope.mark_incomplete(Incomplete::Recovery);
        }
        let f_def = self.function_type(scope, head, &arg_vars, Some(r));
        if scope.complete {
            self.slot_binder_reunify(scope);
        }
        self.solve(scope);
        self.withhold_if_not_rigid(scope, f_def);
        if let Some(f_def) = f_def {
            self.finalise_function(scope, f_def);
        }
        Census::Record
    }

    /// Bind the named binder `named` to its annotation `ann`'s type, when
    /// [`FileFacts::annotation_ty`] can produce one — the shared tail of the
    /// return-annotation and trivial-typed-pattern forms. Enters the normal
    /// per-binding flow far enough to bind the binder's variable with a genuine
    /// `Eq` and finalise. The binder's ground type then flows to
    /// `def_type`/hover and to its uses through the existing machinery (a
    /// ground type is unaffected by the environment-reference poison).
    ///
    /// The RHS is walked in **check mode** against the binder's variable for
    /// *coverage*, not typing (Stage R2-e): check mode suppresses every RHS
    /// node emission — the annotation may retarget the RHS root (the 3.2b-1
    /// lesson: `let o : obj = s` elaborates the use `s` as `obj`), so no RHS
    /// node may be read off — but member accesses inside the RHS still
    /// generate and wake (`let n : int = s.Length` records `Length`'s identity
    /// for hover), and the centralised check-mode poison covers the dropped
    /// RHS↔annotation subsumption automatically. The binder's type is ground
    /// before the walk, so nothing the walk does can retract it. An annotation
    /// outside the gate keeps the whole-binding skip (no walk), as before.
    fn bind_annotated_named(
        &mut self,
        scope: &mut Scope,
        named: &NamedPat,
        ann: &Type,
        rhs: Option<Expr>,
    ) -> Census {
        let Some(t) = self
            .facts
            .annotation_ty(self.solver.tys, ann, AnnotationCx::Plain)
        else {
            // Not walked, but counted ([`Incomplete::BindingAnnotation`]).
            scope.mark_incomplete(Incomplete::BindingAnnotation);
            self.solve(scope);
            return Census::Record;
        };
        let census = if let Some(tok) = named.ident()
            && let Some(def) = self.facts.def_at(tok.text_range())
        {
            let dv = self.def_var(def);
            self.solver.eq(self.solver.tys.var(dv), t);
            if let Some(rhs) = rhs {
                debug_assert!(
                    rhs.syntax()
                        .descendants_where(|k| k == SyntaxKind::LET_DECL)
                        .next()
                        .is_none(),
                    "a LET_DECL nested inside a walked binding's RHS breaks sequential solve"
                );
                let _ = self.infer_expr(scope, &rhs, Some(dv));
            }
            Census::Record
        } else {
            Census::Discard
        };
        self.solve(scope);
        census
    }

    /// On a **complete** function binding, emit `Eq(slot, binder_var)` for each
    /// simple named parameter (Stage 3.2c-2c). The 2b private-slot decoupling
    /// fences a condition-derived `bool` off from a parameter read-off when an
    /// *unmodelled* constraint might contradict it — but every unmodelled shape
    /// already sets the binding incomplete, and on a complete binding the two
    /// variables genuinely *are* one (F# has a single variable per parameter), so
    /// reunifying them lets the parameter generalise together with the function
    /// (`let id x = x` ⇒ `'a -> 'a`). Incomplete bindings skip this entirely,
    /// preserving the exact soundness boundary the #701 review rounds settled.
    fn slot_binder_reunify(&mut self, scope: &Scope) {
        for &(def, slot) in &scope.params {
            let dv = self.def_var(def);
            self.solver
                .eq(self.solver.tys.var(slot), self.solver.tys.var(dv));
        }
    }

    /// Emit a **monomorphic** function type on a `let`-function binder. Curries
    /// the pre-collected parameter variables `arg_vars` over the body's return
    /// variable `ret` into a [`TyKind::Fun`] and unifies it with the function's own
    /// inference variable; at read-off it is emitted only if fully
    /// [`ground`](TyTable::is_ground), so a polymorphic function (any parameter or the
    /// return still an open variable) silently defers until let-generalisation
    /// (3.2c-2c).
    ///
    /// `arg_vars` come from [`Self::param_var`], collected *before* the body walk
    /// so a named parameter's private slot is recorded in time for the body's
    /// condition typing to ground it. Sound by construction: a named parameter
    /// (alone or inside a tuple) contributes a private slot variable (grounded,
    /// if at all, by its annotation or by condition typing, and read off *only*
    /// through this function type); an unmodelled parameter shape contributes a
    /// fresh unbound variable, leaving the function type non-ground so it defers.
    ///
    /// Returns the function's `DefId` when it built and constrained the type, so
    /// [`Self::let_binding`] can finalise it ([`Self::finalise_function`]);
    /// `None` on an early-out (a shape this stage does not model), in which case
    /// the binding defers.
    fn function_type(
        &mut self,
        scope: &Scope,
        head: &LongIdentPat,
        arg_vars: &[TyVid],
        ret: Option<TyVid>,
    ) -> Option<DefId> {
        let ret = ret?;
        // A named-field-group head (`let f (a = x) = …`) needs a record type we do
        // not model; defer it (`args()` is empty there, so `arg_vars` is also
        // empty and the guard below fires, but be explicit).
        if head.name_pat_pairs().is_some() {
            return None;
        }
        if arg_vars.is_empty() {
            return None;
        }
        let name = head.head().and_then(|li| li.idents().last())?;
        let f_def = self.facts.def_at(name.text_range())?;
        // A function of n parameters is a chain n + 1 deep, which the tree
        // does not bound: its parameters are siblings. Past the type budget's
        // depth the chain is not built.
        if arg_vars.len() >= MAX_TYPE_DEPTH {
            self.solver.table.exceed(TypeBudgetExceeded::Depth);
            return None;
        }
        // Curry right: `Fun(a0, Fun(a1, … Fun(an, ret)))`, matching FCS.
        let mut fun_ty = self.solver.tys.var(ret);
        for &arg in arg_vars.iter().rev() {
            fun_ty = self.solver.tys.fun(self.solver.tys.var(arg), fun_ty);
        }
        let fv = self.def_var(f_def);
        self.solver.eq(self.solver.tys.var(fv), fun_ty);
        // This binding's parameters are this head's: the scope is the
        // binding's own, and a local function is not walked.
        for &(param, _) in &scope.params {
            self.binders.own_param(param, f_def);
        }
        Some(f_def)
    }

    /// The inference variable for a function parameter's slot *inside* the
    /// function type. A simple named (or parenthesised named) parameter gets a
    /// **fresh private slot** variable, recorded in [`BinderEnv::param_slot`] under
    /// its `DefId` so condition typing can ground it — but deliberately **not**
    /// its binder's [`def_var`](Self::def_var), so the condition-derived `bool`
    /// never leaks into a standalone parameter read-off (its expression uses or
    /// `def_type`).
    ///
    /// An **annotated** parameter `(x: T)` whose annotation passes the R2-a
    /// gate ([`FileFacts::annotation_ty`]) is a modelled shape too (Stage R2-b): a
    /// parameter annotation is *exact* in F# — subsumption applies at call
    /// sites, never at the binder, and the annotation wins on the binder even
    /// on ill-typed code (`let f (c: int) = if c then …` keeps `c : int` in
    /// FCS, the condition being the error site) — so it grounds **both** the
    /// slot and the binder variable with genuine `Eq`s. Grounding the binder
    /// eagerly (unlike the bare-named decoupling) is what keeps a conflicting
    /// later constraint sound: the annotation's `Eq` lands first in generation
    /// order, so a contradicting condition / `ArgCheck` wake fails and rolls
    /// back instead of retyping the parameter. The binding stays *complete* —
    /// that is the point of the stage — because the annotation is now a
    /// modelled constraint, not a dropped one.
    ///
    /// A tuple, wildcard or parenthesised parameter is typed structurally by
    /// [`Self::pattern_ty`]: a tuple's slot is the tuple of its elements'
    /// slots, each named element registered exactly as a simple parameter is,
    /// and a wildcard is a fresh variable that nothing grounds, so it
    /// generalises like an unused parameter.
    ///
    /// Any other pattern shape (a non-table annotation, a constructor, a
    /// struct tuple, or a recovery hole) gets a fresh, unregistered variable —
    /// never ground, so it defers the whole function type — and marks the
    /// binding **incomplete** (Stage 3.2c-2c): its type comes from a shape
    /// this stage does not model, so a function with such a parameter must not
    /// generalise.
    fn param_var(&mut self, scope: &mut Scope, pat: &Pat, cx: AnnotationCx<'_>) -> TyVid {
        match self
            .pattern_ty(scope, pat, BinderSite::Param, cx)
            .map(|t| (t, self.solver.tys.kind(t)))
        {
            Ok((_, TyKind::Var(slot))) => *slot,
            Ok((t, _)) => {
                let slot = self.solver.table.fresh();
                self.solver.eq(self.solver.tys.var(slot), t);
                slot
            }
            Err(why) => {
                scope.mark_incomplete(why);
                self.solver.table.fresh()
            }
        }
    }

    /// Open the implicitly scoped type variables of a module-level function
    /// binding's head — its parameters' annotations and its return annotation
    /// `ret` — returning them for the head's annotations to read
    /// ([`AnnotationCx::Head`]), and keeping them as [`Scope::head_typars`] for
    /// the binding's solve. `None` where nothing opens.
    ///
    /// FCS creates a type variable no `<…>` declares at its first mention
    /// (`TcTypeOrMeasureParameter`, rigidity `WarnIfNotRigid`), and every
    /// later mention of the name in the module-level declaration is the same
    /// variable: the unscoped environment starts empty per declaration. It is
    /// an ordinary inference variable — the body may fix it (warning FS0064),
    /// and what is left generalises — so one fresh variable per name is FCS's
    /// own reading. Only the head's mentions are read, each name as FCS reads
    /// it (`'``a``` is `'a`).
    ///
    /// A variable the head **declares** (`let f<'a> …`) is one too, but
    /// rigid: FCS rejects a body that fixes it or merges it with another
    /// declared one (FS0670), so it is recorded ([`Scope::rigid_typars`]) and
    /// checked once the binding is solved.
    ///
    /// Nothing opens, so every `'a` declines as before, where the head
    /// declares a variable with a constraint (`<'a when …>`), an attribute
    /// (`[<Measure>]`), a `^` or a leading `_`; mentions a
    /// `^a` (statically resolved, whose name `'a` would share) or a `'_a`; or
    /// binds named fields (`Case (f = p)`) rather than parameters; and where
    /// the body `body` mentions one of the head's names (a lambda's or a
    /// local's annotation, a type application), which is the same variable and
    /// a relation on it the walk does not read. A declaration binding more
    /// than one function is a `let rec`, which is not walked.
    fn open_head_typars(
        &mut self,
        scope: &mut Scope,
        head: &LongIdentPat,
        ret: Option<&Type>,
        body: Option<&Expr>,
    ) -> Option<HashMap<String, TyVid>> {
        scope.declares_typars = head.typar_decls().is_some();
        if head.name_pat_pairs().is_some() {
            return None;
        }
        let mut declared: Vec<String> = Vec::new();
        if let Some(decls) = head.typar_decls() {
            if decls.constraint_clause().is_some() {
                return None;
            }
            for decl in decls.typars() {
                if decl.is_head_type()
                    || decl.attributes().next().is_some()
                    || decl.intersection_constraints().next().is_some()
                {
                    return None;
                }
                let name = ident_text(&decl.ident()?);
                if name.starts_with('_') {
                    return None;
                }
                declared.push(name);
            }
        }
        let mentions: Vec<VarType> = head
            .args()
            .map(|arg| arg.syntax().clone())
            .chain(ret.map(|t| t.syntax().clone()))
            .flat_map(|n| n.descendants_of::<VarType>().collect::<Vec<_>>())
            .collect();
        if mentions.iter().any(VarType::is_head_type) {
            return None;
        }
        let mut names = declared.clone();
        for mention in &mentions {
            let name = ident_text(&mention.ident()?);
            if name.starts_with('_') {
                return None;
            }
            names.push(name);
        }
        let in_body = body.is_some_and(|b| {
            b.syntax()
                .descendants_of::<VarType>()
                .filter_map(|v| v.ident())
                .any(|tok| names.contains(&ident_text(&tok)))
        });
        if in_body {
            return None;
        }
        let mut typars = HashMap::default();
        for name in names {
            typars
                .entry(name)
                .or_insert_with(|| self.solver.table.fresh());
        }
        scope.rigid_typars = declared.iter().map(|n| typars[n]).collect();
        scope.head_typars = typars.clone();
        Some(typars)
    }

    /// The slot variables of a function head's parameters ([`Self::param_var`]).
    ///
    /// A name bound twice among them is not typed at all: FCS rejects every
    /// such head but one — a nested tuple, `((a, b), a)`, whose body sees the
    /// inner `a` — and which binder a use means there is FCS's elaboration
    /// order, not the resolver's scoping. Each parameter then gets a fresh,
    /// unregistered variable, so no condition can ground a slot through a use
    /// resolved to the wrong binder, and the binding is incomplete.
    fn param_vars(
        &mut self,
        scope: &mut Scope,
        head: &LongIdentPat,
        cx: AnnotationCx<'_>,
    ) -> Vec<TyVid> {
        let args: Vec<Pat> = head.args().collect();
        if binds_a_name_twice(&args) {
            scope.mark_incomplete(Incomplete::DuplicateBinder);
            return args.iter().map(|_| self.solver.table.fresh()).collect();
        }
        args.iter()
            .map(|arg| self.param_var(scope, arg, cx))
            .collect()
    }

    /// [`Self::pattern_ty`] for the whole pattern of `binding`, declining
    /// shapes FCS rejects as a binding: a name bound twice (as
    /// [`Self::param_vars`] does), and `inline` or `mutable` on anything but a
    /// bare name, for which FCS creates no binders at all.
    pub(super) fn binding_pattern_ty(
        &mut self,
        scope: &mut Scope,
        binding: &Binding,
        pat: &Pat,
        site: BinderSite,
    ) -> Result<Ty, Incomplete> {
        if !matches!(pat, Pat::Named(_)) && (binding.is_inline() || binding.is_mutable()) {
            return Err(Incomplete::PatternShape(pat.syntax().kind()));
        }
        if binds_a_name_twice(std::slice::from_ref(pat)) {
            return Err(Incomplete::DuplicateBinder);
        }
        self.pattern_ty(scope, pat, site, AnnotationCx::Plain)
    }
}

/// The `(x : T)` head of a **trivial typed pattern** — a [`ParenPat`] wrapping
/// a `TypedPat` wrapping a [`NamedPat`], the `let (x: T) = …` form — as
/// `(named, ty)`. `None` for every other parenthesised shape (tuple heads,
/// wildcards, nested parens, a recovery hole), which stay with
/// [`Gen::let_binding`]'s catch-all (Stage R2-a).
fn trivial_typed_head(paren: &ParenPat) -> Option<(NamedPat, Type)> {
    let Pat::Typed(typed) = paren.inner()? else {
        return None;
    };
    let ty = typed.ty()?;
    let Pat::Named(named) = typed.pat()? else {
        return None;
    };
    Some((named, ty))
}
