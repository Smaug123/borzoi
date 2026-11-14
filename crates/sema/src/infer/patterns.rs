//! Patterns: their shapes, their types, and the binders they bind.

use super::*;

/// Where a pattern's named binders are bound, which decides the variable each
/// one's type is read from ([`Gen::register_binder`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BinderSite {
    /// A function parameter.
    Param,
    /// An expression-level `let`.
    Local,
    /// A module-level `let`.
    Module,
}

/// A pattern as inference reads it ([`Gen::pattern_shape`]): the shapes it
/// types, each name already resolved to its binder.
#[derive(Debug, Clone)]
pub(super) enum PatShape {
    /// A name.
    Name(DefId),
    /// A name with an annotation in the modelled set, and its type.
    Annotated(DefId, Ty),
    /// `_`.
    Wildcard,
    /// `()`.
    Unit,
    /// A (reference) tuple of two or more elements.
    Tuple(Vec<PatShape>),
    /// A constant (`1`, `"s"`, `'c'`, `true`), of its literal's type, and
    /// the value it spells as reachability compares them ([`literal_ctor`]).
    Const(Ty, Ctor<UnionId>),
    /// A referenced assembly's union case (`None`, `Some x`, `Ok _`): which
    /// case, its scheme, and the pattern over its fields — `None` for a
    /// nullary case, and for `Case _`, which matches every field at once.
    Case(CaseKey, UnionCaseScheme, Option<Box<PatShape>>),
}

/// Where a pattern is read ([`Gen::pattern_shape_in`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PatSite {
    /// A `let` or parameter pattern.
    Binding,
    /// A `match` clause's pattern.
    Match,
}

impl PatShape {
    /// Add every name the pattern binds to `out`.
    pub(super) fn names(&self, out: &mut HashSet<DefId>) {
        match self {
            PatShape::Name(def) | PatShape::Annotated(def, _) => {
                out.insert(*def);
            }
            PatShape::Tuple(elems) => elems.iter().for_each(|e| e.names(out)),
            PatShape::Case(_, _, Some(arg)) => arg.names(out),
            PatShape::Wildcard
            | PatShape::Unit
            | PatShape::Const(..)
            | PatShape::Case(_, _, None) => {}
        }
    }

    /// The pattern as reachability sees it ([`crate::match_reach`]).
    fn clause_pat(&self) -> ClausePat<UnionId> {
        match self {
            PatShape::Name(_) | PatShape::Annotated(..) | PatShape::Wildcard => ClausePat::Any,
            PatShape::Unit => ClausePat::Ctor(Ctor::Unit, Vec::new()),
            PatShape::Tuple(elems) => ClausePat::Ctor(
                Ctor::Tuple(elems.len()),
                elems.iter().map(PatShape::clause_pat).collect(),
            ),
            PatShape::Const(_, ctor) => ClausePat::Ctor(ctor.clone(), Vec::new()),
            PatShape::Case(key, scheme, arg) => {
                let fields = match (arg.as_deref(), scheme.fields.len()) {
                    (None, _) => Vec::new(),
                    (Some(PatShape::Wildcard), n) => vec![ClausePat::Any; n],
                    (Some(one), 1) => vec![one.clause_pat()],
                    (Some(PatShape::Tuple(elems)), _) => {
                        elems.iter().map(PatShape::clause_pat).collect()
                    }
                    (Some(other), n) => {
                        unreachable!("a case of {n} fields over {other:?} ([`Gen::case_pattern`])")
                    }
                };
                ClausePat::Ctor(
                    Ctor::Case {
                        union: key.union,
                        index: key.index,
                        siblings: key.siblings,
                    },
                    fields,
                )
            }
        }
    }
}

impl<'a> Gen<'a> {
    /// The type of pattern `pat`, generated structurally, with each named
    /// binder in it registered for `site`; `Err` names the first shape not
    /// modelled. The pattern is read whole ([`Self::pattern_shape`]) before
    /// anything is generated, so a rejected pattern registers no binder and
    /// adds no constraint: a name annotated beside a `()` is not left grounded.
    pub(super) fn pattern_ty(
        &mut self,
        scope: &mut Scope,
        pat: &Pat,
        site: BinderSite,
        cx: AnnotationCx<'_>,
    ) -> Result<Ty, Incomplete> {
        let shape = self.pattern_shape(pat, cx)?;
        Ok(self.gen_pattern(scope, &shape, site))
    }

    /// Read a pattern into the shapes inference types, or name the first one it
    /// does not. Generates nothing.
    ///
    /// A struct tuple is not modelled ([`TyKind::Tuple`] has no struct flag), nor
    /// is an annotation around anything but a name, or outside the modelled set
    /// ([`FileFacts::annotation_ty`]).
    fn pattern_shape(&self, pat: &Pat, cx: AnnotationCx<'_>) -> Result<PatShape, Incomplete> {
        self.pattern_shape_in(pat, PatSite::Binding, cx)
    }

    /// [`Self::pattern_shape`] at `site`: a `match` clause's pattern reads
    /// union cases and constants, and declines an annotated name; a `let` or
    /// parameter pattern reads neither (an incomplete pattern there is rare,
    /// and its relation to the RHS is [`Self::pattern_rhs`]'s, which does not
    /// guard what a case's structure could bind).
    pub(super) fn pattern_shape_in(
        &self,
        pat: &Pat,
        site: PatSite,
        cx: AnnotationCx<'_>,
    ) -> Result<PatShape, Incomplete> {
        // Recovery drops what it cannot parse, so a recovered pattern's
        // surviving children look well-formed: `(a,b,)` reads as a pair, where
        // FCS keeps a third, recovery element. And it can spill past the
        // declaration it broke out of, leaving a later `let` apparently intact
        // where FCS never checks it. So a pattern is read only when nothing up
        // to it recovered ([`crate::recovery::SyntaxRecovery::clean_through`]).
        let recovery = self.facts.resolved.recovery();
        if !recovery.declaration_is_intact(pat.syntax()) || !recovery.clean_through(pat.syntax()) {
            return Err(Incomplete::Recovery);
        }
        let named_def = |named: &NamedPat| {
            named
                .ident()
                .and_then(|tok| self.facts.def_at(tok.text_range()))
                .ok_or(Incomplete::Recovery)
        };
        match pat {
            // A name the resolver reads as a referenced assembly's union case
            // (`None`) is that case, applied to nothing.
            Pat::Named(named)
                if site == PatSite::Match
                    && named.ident().is_some_and(|tok| {
                        self.facts.def_at(tok.text_range()).is_none()
                            || self.facts.own_union_case_at(tok.text_range())
                    }) =>
            {
                let tok = named.ident().ok_or(Incomplete::Recovery)?;
                self.case_pattern(tok.text_range(), None, cx)
            }
            Pat::Named(named) => Ok(PatShape::Name(named_def(named)?)),
            Pat::LongIdent(li) if site == PatSite::Match => {
                // `Case` or `Case p`: at most one argument, no named fields,
                // no type arguments, no active pattern, and a head of one
                // name the resolver reads as a case.
                let mut args = li.args();
                let arg = match (args.next(), args.next()) {
                    (arg, None) => arg,
                    _ => return Err(Incomplete::PatternShape(pat.syntax().kind())),
                };
                if li.name_pat_pairs().is_some() {
                    return Err(Incomplete::PatternShape(pat.syntax().kind()));
                }
                let head = li
                    .head()
                    .filter(|_| li.typar_decls().is_none() && li.active_pat_name().is_none())
                    .ok_or(Incomplete::PatternShape(pat.syntax().kind()))?;
                // A qualified case (`U.A`) is resolved over the whole head.
                let idents: Vec<SyntaxToken> = head.idents().collect();
                let (Some(first), Some(last)) = (idents.first(), idents.last()) else {
                    return Err(Incomplete::Recovery);
                };
                let at = TextRange::new(first.text_range().start(), last.text_range().end());
                self.case_pattern(at, arg.as_ref(), cx)
            }
            Pat::Const(c) if site == PatSite::Match && !c.syntax().text().is_empty() => {
                let kind = c.literal().map(|l| l.kind());
                match kind {
                    Some(
                        SyntaxKind::INT32_LIT
                        | SyntaxKind::STRING_LIT
                        | SyntaxKind::CHAR_LIT
                        | SyntaxKind::BOOL_LIT,
                    ) => {
                        let literal = c.literal().expect("a literal kind");
                        Ok(PatShape::Const(
                            literal_kind_ty(
                                self.facts.env.names(),
                                self.solver.tys,
                                literal.kind(),
                            )
                            .expect("a typed literal"),
                            literal_ctor(literal.kind(), literal.text()),
                        ))
                    }
                    _ => Err(Incomplete::PatternShape(pat.syntax().kind())),
                }
            }
            // FCS relates a `match` pattern's annotation to the scrutinee by
            // equality and rejects a mismatch, keeping no binder; the
            // annotation would already have typed the name.
            Pat::Typed(_) if site == PatSite::Match => Err(Incomplete::CoercedPattern),
            Pat::Typed(typed) => {
                let Some(t) = typed
                    .ty()
                    .and_then(|ty| self.facts.annotation_ty(self.solver.tys, &ty, cx))
                else {
                    return Err(Incomplete::PatternAnnotation);
                };
                let Some(Pat::Named(named)) = typed.pat() else {
                    return Err(Incomplete::PatternAnnotation);
                };
                Ok(PatShape::Annotated(named_def(&named)?, t))
            }
            Pat::Wildcard(_) => Ok(PatShape::Wildcard),
            Pat::Tuple(tuple) if tuple.is_struct() => Err(Incomplete::StructTuple),
            Pat::Tuple(tuple) => {
                let elems = tuple
                    .elements()
                    .map(|el| self.pattern_shape_in(&el, site, cx))
                    .collect::<Result<Vec<_>, _>>()?;
                if elems.len() < 2 {
                    return Err(Incomplete::Recovery);
                }
                Ok(PatShape::Tuple(elems))
            }
            Pat::Paren(p) => match p.inner() {
                Some(inner) => self.pattern_shape_in(&inner, site, cx),
                None => Err(Incomplete::Recovery),
            },
            // `()` is a constant pattern whose node is zero-width: the
            // parentheses sit outside it.
            Pat::Const(c) if c.syntax().text().is_empty() => Ok(PatShape::Unit),
            other => Err(Incomplete::PatternShape(other.syntax().kind())),
        }
    }

    /// Whether some value reaches each of `m`'s clauses ([`provably_reachable`]),
    /// as far as its walk goes: it declines at the first clause whose pattern
    /// it does not read, so that clause is asked only whether anything at all
    /// gets past the clauses before it.
    pub(super) fn every_clause_provably_reachable(&self, m: &MatchExpr) -> bool {
        // The patterns of the clauses so far that have no guard (a guarded
        // clause may fail, so it covers nothing).
        let mut covering: Vec<ClausePat<UnionId>> = Vec::new();
        for clause in m.clauses() {
            let shape = clause
                .pat()
                .map(|pat| self.pattern_shape_in(&pat, PatSite::Match, AnnotationCx::Plain));
            let Some(Ok(shape)) = shape else {
                return provably_reachable(&covering, &ClausePat::Any);
            };
            let query = shape.clause_pat();
            if !provably_reachable(&covering, &query) {
                return false;
            }
            if clause.guard().is_none() {
                covering.push(query);
            }
        }
        true
    }

    /// Whether relating the pattern type `pt` to the scrutinee `sv` would get
    /// ahead of a relation FCS, which checks in source order, applied before
    /// the pattern and inference has not: every constraint still pending was
    /// walked before the pattern. Each kind is decided here, so a new kind of
    /// suspended constraint must be:
    ///
    /// - a pending equality, argument check or dropped check-mode relation
    ///   ([`Scope::dropped`]) is applied as an equality under a snapshot, rolled
    ///   back after, and the pattern contradicts them if any of them, or
    ///   relating `pt` after them, fails (a check FCS meets by coercion fails
    ///   here where FCS succeeds, which only declines);
    /// - a suspended member lookup whose receiver the pattern could make
    ///   known is one FCS made on a receiver it did not yet know, an error
    ///   (FS0072), so the pattern must not wake it
    ///   (`(o.Length, match o with "a" -> …)`).
    pub(super) fn pattern_preempts_earlier_relation(
        &mut self,
        scope: &Scope,
        sv: TyVid,
        pt: Ty,
    ) -> bool {
        let mut eqs: Vec<(Ty, Ty)> = self
            .solver
            .settled_args
            .iter()
            .map(|ac| (self.solver.tys.var(ac.arg), self.solver.tys.var(ac.dom)))
            .collect();
        let mut receivers: Vec<TyVid> = Vec::new();
        for c in &self.solver.constraints {
            match c {
                Constraint::Eq(a, b) => eqs.push((*a, *b)),
                Constraint::ArgCheck(ac) => {
                    eqs.push((self.solver.tys.var(ac.arg), self.solver.tys.var(ac.dom)))
                }
                Constraint::HasMember { recv, .. } => receivers.push(*recv),
            }
        }
        eqs.extend(
            scope
                .dropped
                .iter()
                .map(|&(a, b, _)| (self.solver.tys.var(a), self.solver.tys.var(b))),
        );
        // A parameter's private slot and its binder variable are one variable
        // in FCS ([`Self::slot_binder_reunify`]), so what a condition fixed on
        // the slot (`if x then (match x with 1 -> …)`) FCS applied before the
        // pattern too. A binder with no variable yet has had no use the
        // scrutinee could read.
        eqs.extend(scope.params.iter().filter_map(|&(def, slot)| {
            self.binders
                .var(def)
                .map(|dv| (self.solver.tys.var(slot), self.solver.tys.var(dv)))
        }));
        let mut scrutinee_roots: HashSet<TyVid> = HashSet::default();
        collect_var_roots(
            self.solver.tys,
            self.solver.table.resolve(self.solver.tys.var(sv)),
            &mut scrutinee_roots,
        );
        for recv in receivers {
            let mut recv_roots: HashSet<TyVid> = HashSet::default();
            collect_var_roots(
                self.solver.tys,
                self.solver.table.resolve(self.solver.tys.var(recv)),
                &mut recv_roots,
            );
            if recv_roots.iter().any(|&r| {
                scrutinee_roots
                    .iter()
                    .any(|&v| self.solver.table.unioned(r, v))
            }) {
                return true;
            }
        }
        if eqs.is_empty() {
            return false;
        }
        // Earlier relations that already conflict among themselves are ones
        // FCS resolved by their order, which this replay does not follow, so
        // what the pattern meets is unknown: that declines too.
        let snapshot = self.solver.table.snapshot();
        let mut contradicts = false;
        for (a, b) in eqs {
            if self.solver.table.unify_atomic(a, b).is_err() {
                contradicts = true;
                break;
            }
        }
        contradicts = contradicts
            || self
                .solver
                .table
                .unify_atomic(self.solver.tys.var(sv), pt)
                .is_err();
        self.solver.table.rollback_to(snapshot);
        contradicts
    }

    /// The pattern of the case the resolver reads at `at` ([`FileFacts::case_at`]),
    /// over `arg`: `None` for a case written alone. A nullary case takes no
    /// argument; a case of one field takes a pattern of it; a case of several
    /// takes a tuple pattern of exactly that many; and any case of fields
    /// takes `_` for all of them. Anything else — a case of fields written
    /// alone, a mismatched arity, a case whose type is not read — declines.
    fn case_pattern(
        &self,
        at: TextRange,
        arg: Option<&Pat>,
        cx: AnnotationCx<'_>,
    ) -> Result<PatShape, Incomplete> {
        let (key, scheme) = self.facts.case_at(at)?;
        let unparen = |p: &Pat| -> Option<Pat> {
            let mut p = p.clone();
            while let Pat::Paren(inner) = &p {
                p = inner.inner()?;
            }
            Some(p)
        };
        let arg = match (arg, scheme.fields.len()) {
            (None, 0) => None,
            (None, _) | (Some(_), 0) => {
                return Err(Incomplete::PatternShape(SyntaxKind::LONG_IDENT_PAT));
            }
            (Some(arg), n) => {
                let inner = unparen(arg).ok_or(Incomplete::Recovery)?;
                match (&inner, n) {
                    (Pat::Wildcard(_), _) => Some(Box::new(PatShape::Wildcard)),
                    (_, 1) => Some(Box::new(self.pattern_shape_in(arg, PatSite::Match, cx)?)),
                    (Pat::Tuple(t), n) if !t.is_struct() && t.elements().count() == n => Some(
                        Box::new(self.pattern_shape_in(&inner, PatSite::Match, cx)?),
                    ),
                    _ => return Err(Incomplete::PatternShape(SyntaxKind::LONG_IDENT_PAT)),
                }
            }
        };
        Ok(PatShape::Case(key, scheme, arg))
    }

    /// Generate a read pattern's type, registering each name for `site`.
    ///
    /// A name is typed by its site's variable: a parameter's private slot
    /// (see [`Self::param_var`]), or, for a `let`, its binder variable. An
    /// annotated name is grounded to its annotation — the binder↔annotation
    /// relation holds even on ill-typed code, so the annotation wins at the
    /// binder. For a parameter that grounds both the slot and the binder,
    /// eagerly: the annotation's `Eq` then lands before any constraint from
    /// the body, so a contradicting use fails and rolls back instead of
    /// retyping the parameter.
    pub(super) fn gen_pattern(
        &mut self,
        scope: &mut Scope,
        shape: &PatShape,
        site: BinderSite,
    ) -> Ty {
        match shape {
            PatShape::Name(def) => {
                let v = self.register_binder(scope, *def, site);
                self.solver.tys.var(v)
            }
            PatShape::Annotated(def, t) => {
                let t = *t;
                let v = self.register_binder(scope, *def, site);
                self.solver.eq(self.solver.tys.var(v), t);
                if site == BinderSite::Param {
                    let dv = self.def_var(*def);
                    self.solver.eq(self.solver.tys.var(dv), t);
                    // An annotation mentioning an implicitly scoped type
                    // variable ([`Self::open_head_typars`]) fixes the
                    // parameter's structure but not its type, which the body
                    // may still fix: it is not a type known at a use.
                    if self.solver.tys.is_ground(t) {
                        self.binders.annotate_param(*def, t);
                    } else {
                        scope.typar_params.insert(*def);
                    }
                } else {
                    debug_assert!(
                        self.solver.tys.is_ground(t),
                        "a type variable outside a head"
                    );
                }
                t
            }
            PatShape::Wildcard => {
                let v = self.solver.table.fresh();
                self.solver.tys.var(v)
            }
            PatShape::Unit => self.solver.tys.named(self.facts.env.names(), UNIT),
            PatShape::Tuple(elems) => {
                let elems: Vec<Ty> = elems
                    .iter()
                    .map(|el| self.gen_pattern(scope, el, site))
                    .collect();
                self.solver.tys.tuple(elems)
            }
            PatShape::Const(t, _) => *t,
            PatShape::Case(_, scheme, arg) => {
                let env_tys = self.facts.env.types();
                let instance = self.solver.instantiate(env_tys, scheme.value_ty(env_tys));
                match *self.solver.tys.kind(instance) {
                    TyKind::Fun { arg: fields, ret } => {
                        if let Some(arg) = arg
                            && !matches!(**arg, PatShape::Wildcard)
                        {
                            let at = self.gen_pattern(scope, arg, site);
                            self.solver.eq(at, fields);
                        }
                        ret
                    }
                    _ => instance,
                }
            }
        }
    }

    /// The variable a named binder's type is read from at `site`, registering
    /// the binder as that site requires. A parameter gets a fresh private slot,
    /// recorded for condition typing and the slot=binder reunification, and is
    /// published from that slot ([`Self::finish`]). A local is published only if no emission
    /// barrier discards it ([`Emissions::local`]).
    fn register_binder(&mut self, scope: &mut Scope, def: DefId, site: BinderSite) -> TyVid {
        match site {
            BinderSite::Param => {
                let slot = self.solver.table.fresh();
                self.binders.register_param(def, slot);
                scope.params.push((def, slot));
                scope.binding_params.push(def);
                slot
            }
            BinderSite::Local => {
                self.out.local(def);
                self.def_var(def)
            }
            BinderSite::Module => self.def_var(def),
        }
    }
}
