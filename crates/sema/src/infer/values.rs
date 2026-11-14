//! Value uses: of in-file binders, earlier files' values, and assembly values
//! and cases, each instantiated and loosened as FCS types a use.

use super::*;

impl<'a> Gen<'a> {
    /// Whether a use of the in-file binder `def` is left untyped, marking the
    /// binding incomplete for why: F# may reject the use for the binder's
    /// attributes ([`FileFacts::use_may_be_rejected`]), or the binder is a function
    /// withheld for a rigid variable its body fixed ([`BinderEnv::is_withheld`]).
    pub(super) fn declines_use(&mut self, scope: &mut Scope, def: DefId) -> bool {
        if self.facts.use_may_be_rejected(def) {
            scope.mark_incomplete(Incomplete::RejectedUse);
            true
        } else if self.binders.is_withheld(def) {
            scope.mark_incomplete(Incomplete::RigidTypeVariable);
            true
        } else {
            false
        }
    }

    /// The type variable of a use of the referenced assembly's value the
    /// resolver says the use at `at` names — a function or value of an F#
    /// module, by its IL signature read as its F# type
    /// ([`module_function_scheme`]) — or why it is not typed. `at` is where the
    /// resolver keys the use: a bare name's token, or a qualified path's whole
    /// range ([`Self::module_value_path`]).
    ///
    /// FCS types such a use exactly as it types a use of a generalised in-file
    /// binder (`TcVal`): each type parameter becomes a fresh inference
    /// variable, and the value is loosened at the use
    /// (`MakeApplicableExprWithFlex`). So it is [`Self::scheme_use`] over the
    /// reading. Whatever the reading cannot vouch for — a constrained type
    /// parameter, a unit of measure, an argument-group shape, a type the bridge
    /// does not carry, an attribute that rejects the use — declines with its
    /// reason. The resolver's map holds no assembly reading under an
    /// incomplete projection, so that declines as [`Incomplete::NotInFileValue`].
    ///
    /// A use the resolver resolved to a **preceding file's** top-level value
    /// ([`Resolution::Item`] of an earlier file) is the same kind of use: FCS
    /// types it from the value's type, fixed when that file was checked, so it
    /// instantiates and loosens the type that file published
    /// ([`PrecedingTypes`]) as an in-file binder's would be. A value that
    /// file published no type for declines with the reason it recorded
    /// ([`Incomplete::PrecedingValue`]).
    pub(super) fn nonlocal_value_use(
        &mut self,
        scope: &mut Scope,
        at: TextRange,
    ) -> Result<TyVid, Incomplete> {
        let (parent, idx) = match self.facts.resolved.resolution_at(at) {
            Some(Resolution::Member { parent, idx }) => (parent, idx),
            Some(Resolution::Item(id))
                if self.facts.own_union_cases.contains(&id)
                    || self.facts.preceding.case(id).is_some() =>
            {
                return self.case_use(at);
            }
            Some(Resolution::Local(_)) if self.facts.own_union_case_at(at) => {
                return self.case_use(at);
            }
            Some(Resolution::Item(id)) if !self.facts.resolved.owns_item(id) => {
                let ty = *self
                    .facts
                    .preceding
                    .get(id)
                    .map_err(Incomplete::PrecedingValue)?;
                return Ok(self.scheme_use(scope, self.facts.env.types(), ty));
            }
            Some(Resolution::UnionCase { .. }) => return self.case_use(at),
            _ => return Err(Incomplete::NotInFileValue),
        };
        let scheme = match self.facts.env.member_at(parent, idx) {
            Member::Method(m) => module_function_scheme(self.facts.env, parent, m),
            _ => Err(FunctionTyGap::NotAFunction),
        }
        .map_err(Incomplete::AssemblyValue)?;
        Ok(self.constrained_scheme_use(scope, &scheme))
    }

    /// The type variable of a use of the case the resolver reads at `at`
    /// ([`FileFacts::case_at`]), as a value: the union for a nullary case (`None`), else the
    /// function from its field, or the tuple of its fields, to the union
    /// (`Some`, `UPair`), each of the union's type parameters instantiated
    /// fresh. Unlike a value's use, a case's is not loosened
    /// (`MakeApplicableExprWithFlex` is `TcVal`'s): unapplied, FCS
    /// eta-expands it at exactly its field types (`UObj` is `obj -> UObj`),
    /// and applied, it checks each argument against an unsealed field by
    /// coercion (`TcExprsWithFlexes`), which an application's argument check
    /// reproduces — as `infer_union_case_diff`'s unsealed and many-field rows
    /// hold it to.
    fn case_use(&mut self, at: TextRange) -> Result<TyVid, Incomplete> {
        let (_, scheme) = self.facts.case_at(at)?;
        let env_tys = self.facts.env.types();
        let ty = self.solver.instantiate(env_tys, scheme.value_ty(env_tys));
        let uv = self.solver.table.fresh();
        self.solver.eq(self.solver.tys.var(uv), ty);
        Ok(uv)
    }

    /// [`Self::scheme_use`] over a module function's reading: each type
    /// parameter carrying `equality` or `comparison` instantiates to a variable
    /// recorded with its constraint ([`Scope::constrained`]) and poisoned, so it
    /// is checked once the binding is solved and never generalised while open.
    fn constrained_scheme_use(
        &mut self,
        scope: &mut Scope,
        scheme: &ModuleFunctionScheme,
    ) -> TyVid {
        let mut fresh: HashMap<u32, TyVid> = HashMap::default();
        let body = self
            .solver
            .instantiate_rec(self.facts.env.types(), scheme.ty, &mut fresh);
        for (i, req) in scheme.requirements.iter().enumerate() {
            let Some(req) = req else { continue };
            // A type parameter the type does not mention has no variable to
            // check; FCS solves it by default, which nothing reads.
            if let Some(&v) = u32::try_from(i).ok().and_then(|i| fresh.get(&i)) {
                scope.constrained.push((v, *req));
                scope.poison.push(v);
            }
        }
        let ty = self.loosened(scope, body).unwrap_or(body);
        let uv = self.solver.table.fresh();
        self.solver.eq(self.solver.tys.var(uv), ty);
        uv
    }

    /// The type variable of a use of a value whose type is `scheme`, a type of
    /// `from`: the scheme instantiated afresh and loosened as FCS loosens a
    /// value at a use ([`Self::loosened`]).
    fn scheme_use(&mut self, scope: &mut Scope, from: &TyTable, scheme: Ty) -> TyVid {
        let body = self.solver.instantiate(from, scheme);
        let ty = self.loosened(scope, body).unwrap_or(body);
        let uv = self.solver.table.fresh();
        self.solver.eq(self.solver.tys.var(uv), ty);
        uv
    }

    /// The range of a qualified path `M.f` that names a referenced F# module's
    /// member, or a preceding file's top-level value: the shape of a
    /// module-qualified value (`List.map`,
    /// `Microsoft.FSharp.Core.Operators.id`, `Shared.helper`). The resolver
    /// records such a value over the whole path, as FCS reports it, so that is
    /// the range read. `None` for a path whose head is an in-file binder (a
    /// member access on a value), for one with an active-pattern segment, and
    /// for every other record — a path that only begins with a module value
    /// (`M.v.Length`) among them.
    pub(super) fn module_value_path(&self, li: &LongIdentExpr) -> Option<TextRange> {
        let long_ident = li.long_ident()?;
        if long_ident.active_pat_names().next().is_some() {
            return None;
        }
        let idents: Vec<SyntaxToken> = long_ident.idents().collect();
        let (head, rest) = idents.split_first()?;
        let last = rest.last()?;
        let whole = TextRange::new(head.text_range().start(), last.text_range().end());
        // A qualified case of a project union (`U.A`, `Lib.A`, `Lib.U.B`),
        // whose head may be the union's own type.
        if self.facts.own_union_case_at(whole)
            || matches!(
                self.facts.resolved.resolution_at(whole),
                Some(Resolution::Item(id)) if self.facts.preceding.case(id).is_some()
            )
        {
            return Some(whole);
        }
        if self.facts.def_at(head.text_range()).is_some() {
            return None;
        }
        match self.facts.resolved.resolution_at(whole)? {
            Resolution::Member { parent, .. } if self.facts.env.is_module(parent) => Some(whole),
            Resolution::Item(id) if !self.facts.resolved.owns_item(id) => Some(whole),
            _ => None,
        }
    }

    /// [`Self::nonlocal_value_use`], recording why the binding is incomplete
    /// when it declines.
    pub(super) fn nonlocal_value_or_incomplete(
        &mut self,
        scope: &mut Scope,
        at: TextRange,
    ) -> Option<TyVid> {
        match self.nonlocal_value_use(scope, at) {
            Ok(v) => Some(v),
            Err(why) => {
                scope.mark_incomplete(why);
                None
            }
        }
    }

    /// The type variable of a use of the in-file binder `def`, recording no
    /// node — the type FCS gives the use, which is not always the binder's.
    ///
    /// - A **generalised** binder instantiates its scheme afresh.
    /// - Any other contributes its own variable, **poisoned** when it is an
    ///   environment reference (not a parameter of this binding): such a
    ///   binder carries an FCS-known type from its own binding — an earlier
    ///   value we may not have typed (an annotated `let a : int = 1`, a skipped
    ///   `let rec`), or a later forward reference — so a still-open one must
    ///   block generalisation, while a ground one (a typed earlier value) is
    ///   unaffected. Without it, a skipped earlier binder allocates its variable
    ///   lazily at the use (index ≥ the current mark), and the mark check alone
    ///   would treat it as quantifiable (`let a : int = 1 \n let h x = (x, a)`
    ///   ⇒ a bogus `'a -> 'a * 'b`).
    ///
    /// **Loosening.** FCS loosens a value at each use
    /// (`MakeApplicableExprWithFlex`): a parameter of a type that is not sealed
    /// becomes a fresh type variable that is a subtype of it, so `let g = fo`
    /// with `fo : obj -> int` makes `g : 'a -> int`. It decides on the type it
    /// knows at the use, so the use is modelled exactly where that type is
    /// known here too — a scheme, an earlier binder's ground type, a settled
    /// local, a parameter's annotation ([`Self::loosened`]). Where it is not —
    /// an unannotated parameter, an earlier binder left open — the use keeps
    /// the binder's variable and is recorded as **undecided**, which
    /// [`Self::verified`] checks once the binding is solved.
    pub(super) fn value_use(&mut self, scope: &mut Scope, def: DefId) -> TyVid {
        if let Some(scheme) = self.binders.scheme(def) {
            return self.scheme_use(scope, self.solver.tys, scheme);
        }
        let dv = self.def_var(def);
        let match_local = scope.match_locals.contains(&def);
        let current_param = scope.is_param(def) || match_local;
        if !current_param {
            scope.poison.push(dv);
            scope.opaque.push((dv, None));
        }
        let known = match self.binders.param_annotation(def) {
            Some(ann) => Some(ann),
            // A match's pattern variable is typed by its pattern before its
            // clause is walked: a use of one whose type is known then is
            // loosened as FCS loosens it (`Some g -> g` with
            // `g : obj -> int` is `'a -> int` at the use).
            None if match_local => Some(self.solver.table.resolve(self.solver.tys.var(dv)))
                .filter(|&t| self.solver.tys.is_ground(t)),
            None if current_param => None,
            None => Some(self.solver.table.resolve(self.solver.tys.var(dv)))
                .filter(|&t| self.solver.tys.is_ground(t)),
        };
        let Some(known) = known else {
            scope.undecided_uses.push(dv);
            return dv;
        };
        match self.loosened(scope, known) {
            Some(ty) => {
                let uv = self.solver.table.fresh();
                self.solver.eq(self.solver.tys.var(uv), ty);
                uv
            }
            None => dv,
        }
    }

    /// The type FCS gives a use of a value of type `ty`
    /// (`MakeApplicableExprWithFlex`), or `None` when it is `ty` itself: each
    /// parameter of each curried group — each element of a tupled one — whose
    /// type is not sealed ([`is_sealed`]) becomes a fresh variable, a subtype
    /// of that type in FCS. The fresh variable is poisoned: FCS generalises
    /// such a variable by its own rules (a bounded one in argument position is
    /// condensed to its bound), which are not modelled. A bound other than
    /// `obj`, which every type meets, is a constraint inference cannot check,
    /// so its variable is recorded ([`Scope::bounded_flex`]).
    fn loosened(&mut self, scope: &mut Scope, ty: Ty) -> Option<Ty> {
        let tys = self.solver.tys;
        let mut groups = Vec::new();
        let mut ret = ty;
        while let TyKind::Fun { arg, ret: next } = *tys.kind(ret) {
            groups.push(arg);
            ret = next;
        }
        let elements = |group: Ty| match tys.kind(group) {
            TyKind::Tuple(elems) => elems.to_vec(),
            _ => vec![group],
        };
        if groups.iter().all(|&g| {
            elements(g)
                .iter()
                .all(|&e| is_sealed(self.facts.env, tys, e))
        }) {
            return None;
        }
        let mut loosened = Vec::new();
        for group in groups {
            let mut elems: Vec<Ty> = elements(group)
                .into_iter()
                .map(|e| {
                    if is_sealed(self.facts.env, tys, e) {
                        return e;
                    }
                    let v = self.solver.table.fresh();
                    scope.poison.push(v);
                    if !tys.is_named(self.facts.env.names(), e, &OBJECT_PATH) {
                        scope.bounded_flex.push(v);
                    }
                    tys.var(v)
                })
                .collect();
            loosened.push(match tys.kind(group) {
                TyKind::Tuple(_) => tys.tuple(elems),
                _ => elems.pop().expect("one element"),
            });
        }
        Some(
            loosened
                .into_iter()
                .rev()
                .fold(ret, |ret, arg| tys.fun(arg, ret)),
        )
    }
}
