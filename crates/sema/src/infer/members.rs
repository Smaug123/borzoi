//! Member accesses and method calls: what the walk generates for them, and
//! the wakes that resolve them.

use super::*;

/// Which kind of member a suspended [`Constraint::HasMember`] resolves to — a
/// **data** member (field / non-indexer property, Stage 3.3a) or a **method** (a
/// single-candidate instance method call, Stage 3.3d). It selects only the
/// assembly lookup the wake runs; everything else the wake does (bridge the type,
/// record the member's identity at `use_range`, unify the result) is **shared**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MemberAccessKind {
    /// A field or non-indexer property (`recv.Length`), typed as the member's type
    /// ([`AssemblyEnv::instance_data_member`]).
    Data,
    /// A method **call** — an instance `recv.M(args)`, or (stage OV-7,
    /// `is_static`) a type-qualified static `Type.M(args)` — typed as the chosen
    /// overload's **return** type by the OV-6 engine ([`Gen::wake_member`] runs
    /// the group's [`AssemblyEnv::instance_method_group`] /
    /// [`AssemblyEnv::static_method_group`] +
    /// [`AssemblyEnv::resolve_overload`]). `args` carries the per-argument
    /// inference variable of each positional argument ([`Gen::method_arg_vids`]) —
    /// its length is the argument count the arity gate reads, and each variable is
    /// resolved to a ground [`Ty`] the applicability matcher consumes. `None` when
    /// the argument shape is not a plain positional list (a named argument, a
    /// recovery hole), which the wake defers — FCS does not type an ill-formed
    /// call as the method return (`s.Insert(foo = 0, …)` ⇒ `obj`, not `string`).
    Method {
        args: Option<Vec<TyVid>>,
        /// Whether this is a **static** call through a type-qualified path
        /// ([`Gen::static_callee`]) — the wake then consults the static method
        /// group. An instance call through a value receiver is `false`.
        is_static: bool,
    },
}

impl<'a> Gen<'a> {
    /// Push a suspended member constraint: `recv` has an instance data member
    /// named by the token `member` whose type is `result`; the token's range is
    /// the constraint's `use_range` (Stage 3.3a; `use_range` added in 3.3b).
    /// Both type endpoints are **poisoned** unconditionally at the point of
    /// generation ([`Self::gen_member_access`]), so this only enqueues the
    /// relation; the solver wakes it when `recv`'s head is concrete.
    fn has_member(
        &mut self,
        scope: &mut Scope,
        recv: TyVid,
        member: &SyntaxToken,
        result: TyVid,
        kind: MemberAccessKind,
        args_by: TextSize,
    ) {
        self.solver.constraints.push(Constraint::HasMember {
            recv,
            name: ident_text(member),
            result,
            use_range: member.text_range(),
            kind,
            args_by,
        });
        // Poison both endpoints: an unwoken (open) member relation must not let its
        // receiver or result generalise. A ground endpoint is unaffected (poison
        // bites only open vars). Shared by the data-member chain
        // ([`Self::gen_member_access`]) and the method call ([`Self::infer_method_call`]).
        scope.poison.push(recv);
        scope.poison.push(result);
        // Until the wake binds it to the member's type, the result is one FCS
        // knows and inference does not; once bound it has no variable left.
        scope.opaque.push((result, None));
    }

    /// Generate a **data-member** access chain over a receiver variable `recv`, one
    /// [`Constraint::HasMember`] (`kind = Data`) per trailing segment (Stage 3.3a).
    /// The `segments` are the member idents after the receiver (`["Length"]` for
    /// `s.Length`; `["A", "B"]` for a multi-dot `s.A.B`). Returns the variable
    /// standing for the **final** member's type — the type of the whole access — or
    /// `None` if there are no segments (a bare receiver, not a member access).
    ///
    /// Each segment chains: segment *i*'s constraint watches the previous result
    /// (the receiver for the first), so a multi-dot chain wakes segment by segment
    /// as each result grounds. Every receiver and result variable is poisoned by
    /// [`Self::has_member`] — an unwoken member relation must never let either
    /// generalise (the same rule as application's `r`-poison).
    fn gen_member_access(
        &mut self,
        scope: &mut Scope,
        recv: TyVid,
        segments: &[SyntaxToken],
    ) -> Option<TyVid> {
        let mut cur = recv;
        let mut last = None;
        for seg in segments {
            let result = self.solver.table.fresh();
            self.has_member(
                scope,
                cur,
                seg,
                result,
                MemberAccessKind::Data,
                seg.text_range().start(),
            );
            cur = result;
            last = Some(result);
        }
        last
    }

    /// Type a **single-candidate instance method call** `recv.Method(args)` (Stage
    /// 3.3d), returning the call's type variable — the method's **return** type. The
    /// whole-call node is emitted by [`Self::infer_app`]'s callers at the `APP_EXPR`
    /// range (where FCS emits its `call:instance` node), read back only when
    /// [`ground`](TyTable::is_ground) — and a *rejected* call's `result` stays open (its
    /// wake never discharges), so the whole-call node naturally drops. In a check
    /// position the whole-call node is not emitted at all.
    ///
    /// **A method call emits *no node inside itself*** — not its receiver, not its
    /// argument's sub-expressions. FCS lowers a rejected / ill-formed call to a
    /// single node and emits nothing inside it, and its receiver `result` can even be
    /// grounded by a *surrounding* constraint (`f (s.ToLowerInvariant(1))` grounds the
    /// bad call to `f`'s domain), so gating the receiver on its own groundness is
    /// unsound. Rather than track per-call acceptance, this **discards** every
    /// emission `method_callee` and the argument walk produce (a snapshot + truncate):
    /// the receiver's *type* still flows to the wake (and its hover comes from name
    /// resolution), only its expression-*node* is dropped. Sound by under-emission —
    /// we never publish a node FCS lacks.
    ///
    /// A **static** call `Type.Method(args)` (stage OV-7) — a callee whose
    /// second-to-last segment the resolver resolved to a referenced-assembly
    /// *type* entity ([`Self::static_callee`]) — takes the same path with
    /// `is_static` set: the receiver variable is unified with the entity's
    /// [`TyKind::Named`] immediately (a static receiver is known at generation, so
    /// the wake fires on the first fixpoint pass), and no receiver node is
    /// emitted (FCS emits no node anywhere on the qualified path — probed
    /// 2026-07-10: one `call:static*` node at the whole application, argument
    /// consts inside, nothing at the path or method name).
    ///
    /// `None` (deferred, incomplete) when the callee is neither a modelled member
    /// access nor an entity-rooted static path — e.g. a qualified call whose root
    /// is a *module* (module-function application has F# value semantics, not
    /// .NET method-call semantics — out of scope), a project-defined type, or an
    /// unresolved head (leaving the resolver's static-member resolution
    /// untouched).
    pub(super) fn infer_method_call(
        &mut self,
        scope: &mut Scope,
        callee: &Expr,
        arg: &Expr,
    ) -> Option<TyVid> {
        // Snapshot the emissions: everything `method_callee` and the argument
        // walk record inside the call — nodes and local binders alike — is
        // discarded below (see the doc).
        let emit_mark = self.out.mark();
        if let Some((path, method_tok)) = self.static_callee(callee) {
            let arg_vids = self.method_arg_vids(scope, arg);
            // The static receiver's type is the rooting entity itself, ground at
            // generation time.
            let recv = self.solver.table.fresh();
            self.solver.eq(
                self.solver.tys.var(recv),
                self.solver.tys.named_path(self.facts.env.names(), &path),
            );
            let result = self.solver.table.fresh();
            self.has_member(
                scope,
                recv,
                &method_tok,
                result,
                MemberAccessKind::Method {
                    args: arg_vids,
                    is_static: true,
                },
                self.facts.node_span(arg.syntax()).end(),
            );
            // Discard any argument sub-expression nodes (the shared method-call
            // rule: a method call emits nothing inside itself).
            self.out.discard_since(emit_mark);
            return Some(result);
        }
        let recorded = scope.reasons_recorded();
        let Some((recv, method_tok)) = self.method_callee(scope, callee) else {
            self.out.discard_since(emit_mark);
            scope.mark_consequence(recorded, Incomplete::MethodCallee(callee.syntax().kind()));
            return None;
        };
        // Walk each positional argument in **check mode**, collecting the
        // per-argument inference variable the OV-6 overload engine reads at the
        // wake (its length is the argument count the arity gate uses). The
        // check-mode walk (each element against a fresh expected) both suppresses
        // the element's node emission and poisons its generalisable parameter uses
        // — exactly as the pre-OV-6 whole-argument check walk did (the tuple arm
        // already walks its elements this way), so `let f x = s.CompareTo x` stays
        // `obj -> int`, not `'a -> int`. `None` means the argument shape is not a
        // plain positional list (a named argument, a recovery hole); the wake
        // defers such a call. A direct unit `()` is `Some([])` (no walk, no
        // gratuitous incompleteness — the common `s.M()` shape).
        let arg_vids = self.method_arg_vids(scope, arg);
        // Suspend the method lookup; `result` is the whole call's (return) type.
        // `has_member` poisons `recv` and `result` — an unwoken method relation must
        // not let either generalise (a ground discharge is unaffected).
        let result = self.solver.table.fresh();
        self.has_member(
            scope,
            recv,
            &method_tok,
            result,
            MemberAccessKind::Method {
                args: arg_vids,
                is_static: false,
            },
            self.facts.node_span(arg.syntax()).end(),
        );
        // Discard every node emitted inside the call (receiver + argument
        // sub-expressions): a method call emits nothing inside itself (see the doc).
        self.out.discard_since(emit_mark);
        Some(result)
    }

    /// Recognise a **static-call callee** `Type.Method` (stage OV-7): a
    /// `LONG_IDENT_EXPR` whose **second-to-last** segment carries the resolver's
    /// [`Resolution::Entity`] — the rooting referenced-assembly type — with the
    /// final segment as the method name. Returns the entity's canonical
    /// [`TyKind::Named`] path and the method token, or `None` when the callee is not
    /// that shape:
    ///
    /// - the rooting segment resolved to anything else — an in-file value/type, a
    ///   static *member* result (`System.Console.Out.WriteLine` — the segment
    ///   before the method carries no `Entity` record), or nothing;
    /// - the entity is a **module** — a module function is an F# *value*
    ///   (curried, possibly generic, applied with value semantics), not a .NET
    ///   method call, so the overload engine's commit rules do not transfer;
    /// - the entity does not **round-trip** through
    ///   [`AssemblyEnv::lookup_type`]`(namespace, name, 0)` — a nested or generic
    ///   type, whose wake-side lookup would miss or land elsewhere. The check
    ///   makes the generation-side handle and the wake-side lookup provably
    ///   agree, rather than assuming the path encoding.
    ///
    /// A **parenthesised** callee (`(System.String.Compare)(…)`) is deliberately
    /// *not* peeled (unlike [`Self::method_callee`]): FCS elaborates it as a
    /// first-class **method value**, whose semantics differ from a call —
    /// probed 2026-07-11 (review round 4): the overloaded
    /// `(String.Compare)("a", "b")` is `obj` (the method-group conversion
    /// fails) where the call form commits `Int32`, so peeling into the call
    /// engine would publish a type at a node FCS types `obj`.
    ///
    /// An active-pattern segment declines as in [`Self::method_callee`].
    fn static_callee(&self, callee: &Expr) -> Option<(Vec<String>, SyntaxToken)> {
        let Expr::LongIdent(li) = callee else {
            return None;
        };
        let long_ident = li.long_ident()?;
        if long_ident.active_pat_names().next().is_some() {
            return None;
        }
        let idents: Vec<SyntaxToken> = long_ident.idents().collect();
        let (method, prefix) = idents.split_last()?;
        let root = prefix.last()?;
        let Resolution::Entity(handle) = self.facts.resolved.resolution_at(root.text_range())?
        else {
            return None;
        };
        if self.facts.env.is_module(handle) {
            return None;
        }
        let entity = self.facts.env.entity(handle);
        if self
            .facts
            .env
            .lookup_type(&entity.namespace, &entity.name, 0)
            != Some(handle)
        {
            return None;
        }
        let mut path = entity.namespace.clone();
        path.push(entity.name.clone());
        Some((path, method.clone()))
    }

    /// The **per-argument inference variables** a method call `recv.M(args)`
    /// supplies (Stage 3.3d / OV-6), or `None` when the argument shape is one this
    /// stage cannot confidently read as a plain positional list — in which case
    /// the call defers (D5). Mirrors the arity decomposition the pre-OV-6
    /// `method_arg_count` used, but now walks each element ([`Self::walk_arg_element`])
    /// to capture its variable; the returned vector's length *is* the argument
    /// count:
    ///
    /// - a **direct** unit `()` (`M()`) is `Some([])` — no element, no walk;
    /// - the list `M(a, b)` parses as `Paren(Tuple(a, b))`; peeling that **one**
    ///   call-parenthesis layer gives the reference tuple, one variable per element;
    /// - a **parenthesized** unit `M(())` is one explicit unit argument;
    /// - any other single expression is one argument (`M(a)`, `M(a + b)`, `M(f x)`,
    ///   and — deliberately — `M((a, b))`, whose extra parentheses make the tuple a
    ///   single value FCS elaborates as a method-value application, so counting its
    ///   inner tuple would wrongly accept it);
    /// - a **named argument** (`M(name = value)`) or a recovery hole is `None`.
    ///
    /// Only the *one* call-argument parenthesis layer is peeled, so the double-paren
    /// `M((a, b))` stays a single argument and defers for a multi-parameter method.
    ///
    /// **Every** element is walked in check mode for its poison side effect even
    /// when the shape is ultimately non-positional (a named argument): a parameter
    /// used inside a named argument (`s.Insert(startIndex = x, …)`) must still be
    /// poisoned, exactly as the pre-OV-6 whole-argument check walk did — otherwise
    /// it could wrongly generalise when the result is grounded by an annotation. A
    /// non-positional shape additionally marks the binding walk-incomplete before
    /// returning `None`.
    fn method_arg_vids(&mut self, scope: &mut Scope, arg: &Expr) -> Option<Vec<TyVid>> {
        // Peel exactly the call's own argument parentheses: `M()` is a bare unit
        // const, `M(e)` wraps its argument in one `Paren`, `M e` is a bare argument.
        let inner = match arg {
            _ if is_unit_literal(arg) => return Some(Vec::new()), // `M()`
            // `M(e)` — the call parens; a recovery hole (no inner) is incomplete.
            Expr::Paren(p) => match p.inner() {
                Some(inner) => inner,
                None => {
                    scope.mark_incomplete(Incomplete::Recovery);
                    return None;
                }
            },
            other => other.clone(), // `M e`
        };
        // A unit *inside* the call parens (`M(())`) is one explicit unit argument.
        if is_unit_literal(&inner) {
            return Some(vec![self.walk_arg_element(scope, &inner)]);
        }
        match &inner {
            Expr::Tuple(t) if !t.is_struct() => {
                // Walk every element (for its poison / incompleteness side effect),
                // recording whether the shape is a clean positional list.
                let mut vids = Vec::new();
                let mut positional = true;
                for el in t.elements() {
                    if is_named_arg(&el) {
                        positional = false;
                    }
                    vids.push(self.walk_arg_element(scope, &el));
                }
                // A **well-formed** positional tuple has exactly one more element
                // than commas. A trailing / doubled comma is a parser *recovery* on
                // a malformed argument list (`s.Insert(0, "z",)`), which FCS types
                // as `obj` (no method) — so reject it rather than counting elements.
                let commas = t
                    .syntax()
                    .children_with_tokens()
                    .filter(|c| c.kind() == SyntaxKind::COMMA_TOK)
                    .count();
                if !positional || vids.len() != commas + 1 {
                    scope.mark_incomplete(Incomplete::MethodArgShape);
                    return None;
                }
                Some(vids)
            }
            // A single argument — walk it regardless (poison), then classify: a named
            // argument is non-positional (defer + incomplete); anything else is one
            // positional value.
            _ => {
                let v = self.walk_arg_element(scope, &inner);
                if is_named_arg(&inner) {
                    scope.mark_incomplete(Incomplete::MethodArgShape);
                    None
                } else {
                    Some(vec![v])
                }
            }
        }
    }

    /// Walk one method-argument element in **check mode**, returning the inference
    /// variable carrying its type. An element that cannot be typed (a unit, or an
    /// unmodelled sub-expression) leaves an **unbound** fresh var — so the argument
    /// *count* stays exact while its type reads as Deferred at the wake — and marks
    /// the binding walk-incomplete. The check-mode walk (a fresh expected)
    /// suppresses the element's node emission and poisons its generalisable
    /// parameter uses; the overload wake later resolves the variable to a ground
    /// [`Ty`] for [`AssemblyEnv::may_apply`] / [`AssemblyEnv::must_apply`].
    fn walk_arg_element(&mut self, scope: &mut Scope, el: &Expr) -> TyVid {
        let expected = self.solver.table.fresh();
        let v = match self.infer_expr(scope, el, Some(expected)) {
            Some(v) => v,
            None => {
                scope.mark_incomplete_propagated();
                self.solver.table.fresh()
            }
        };
        // The relation to the parameter is not the stand-in's but unknown, so
        // it is opaque rather than dropped.
        scope.dropped.retain(|&(_, e, _)| e != expected);
        scope
            .opaque
            .push((v, Some(self.facts.node_span(el.syntax()))));
        v
    }

    /// Resolve a **method-call callee** (a member access in function position) into
    /// its `(receiver-var-for-the-method, method-name-token)` (Stage 3.3d), or
    /// `None` when the callee is not a modelled member access. Emits the receiver's
    /// own node and chains any **data-member prefix** before the final method
    /// segment (`s.Field.M(…)` = a `Data` `HasMember` on `Field`, then a `Method`
    /// one on `M`), reusing [`Self::gen_member_access`]:
    ///
    /// - **`LONG_IDENT_EXPR`** (`s.M`, `s.Field.M`): the head resolves to an in-file
    ///   value binder ([`Self::receiver_var`], emitting its value node); the trailing
    ///   idents split into a data-member prefix and the final method. A head that is
    ///   not an in-file value binder (a static path `System.Console.WriteLine`)
    ///   returns `None` **before** any emission, so the call defers cleanly.
    /// - **`DOT_GET_EXPR`** (`"hi".M`, `(expr).M`): the inner receiver *expression*
    ///   is synthesized (its own node emitted), then the member path splits the same
    ///   way. This also handles a method result feeding a further call
    ///   (`s.ToLower().M()`) — the inner `APP_EXPR` receiver grounds through the
    ///   fixpoint.
    /// - **`PAREN_EXPR`**: transparent — peel and recurse.
    ///
    /// An active-pattern name segment (`Foo.(|Bar|_|)`) is not a plain member token,
    /// so a path carrying one declines.
    fn method_callee(&mut self, scope: &mut Scope, callee: &Expr) -> Option<(TyVid, SyntaxToken)> {
        match callee {
            Expr::LongIdent(li) => {
                let long_ident = li.long_ident()?;
                if long_ident.active_pat_names().next().is_some() {
                    return None;
                }
                let idents: Vec<SyntaxToken> = long_ident.idents().collect();
                let (head, rest) = idents.split_first()?;
                // Need a head *and* at least one member segment (the method).
                let (method, prefix) = rest.split_last()?;
                let head_var = self.receiver_var(scope, head)?;
                // Emit the receiver's own value node (coercion-free — synth).
                self.emit(head.text_range(), head_var, None);
                let recv = self.chain_prefix(scope, head_var, prefix)?;
                Some((recv, method.clone()))
            }
            Expr::DotGet(dg) => {
                let recv_expr = dg.expr()?;
                let long_ident = dg.long_ident()?;
                if long_ident.active_pat_names().next().is_some() {
                    return None;
                }
                let segments: Vec<SyntaxToken> = long_ident.idents().collect();
                let (method, prefix) = segments.split_last()?;
                // The receiver is a coercion-free (synth) sub-position — a member
                // access never coerces its receiver — so synthesize it (emitting its
                // own node).
                let head_var = self.infer_expr(scope, &recv_expr, None)?;
                let recv = self.chain_prefix(scope, head_var, prefix)?;
                Some((recv, method.clone()))
            }
            Expr::Paren(p) => self.method_callee(scope, &p.inner()?),
            _ => None,
        }
    }

    /// Chain a **data-member prefix** onto a receiver variable, returning the
    /// receiver for the final method segment (Stage 3.3d). An empty prefix (the
    /// common `s.M(…)` shape) passes the receiver through unchanged; a non-empty one
    /// (`s.Field.M(…)`) generates the data-member chain via
    /// [`Self::gen_member_access`].
    fn chain_prefix(
        &mut self,
        scope: &mut Scope,
        recv: TyVid,
        prefix: &[SyntaxToken],
    ) -> Option<TyVid> {
        if prefix.is_empty() {
            Some(recv)
        } else {
            self.gen_member_access(scope, recv, prefix)
        }
    }

    /// Type a **value-receiver** member access `s.Length` — a `LONG_IDENT_EXPR`
    /// whose head resolves to an in-file value binder, with trailing member
    /// segments (Stage 3.3a). Returns the whole access's type variable (the final
    /// member's), or `None` (deferred, incomplete) when the shape is one this stage
    /// does not model:
    ///
    /// - a **bare** long-ident (a single segment, no members) — that is a plain
    ///   value/type reference the resolver already handles, not a member access;
    /// - a head that does **not** resolve to an in-file value binder — a
    ///   fully-qualified static path (`System.Console.WriteLine`) or an unresolved
    ///   name, left for the catch-all so the static-member resolution is untouched;
    /// - an active-pattern segment in the path (`Foo.(|Bar|_|)`), which
    ///   [`LongIdent::idents`](borzoi_cst::syntax::LongIdent::idents) does not surface as a plain token.
    ///
    /// The receiver's own value node is emitted at the head token's range (FCS
    /// emits a value node there), coercion-free (synth) — a member access never
    /// coerces its receiver. The whole-access node is emitted at the
    /// `LONG_IDENT_EXPR` range in the access's own mode (`expected`).
    pub(super) fn infer_long_ident_member(
        &mut self,
        scope: &mut Scope,
        li: &LongIdentExpr,
        expected: Option<TyVid>,
    ) -> Option<TyVid> {
        let Some(long_ident) = li.long_ident() else {
            scope.mark_incomplete(Incomplete::Recovery);
            return None;
        };
        // A module-qualified value (`List.length`), whose node FCS keys at
        // the whole path.
        if let Some(whole) = self.module_value_path(li) {
            let uv = self.nonlocal_value_or_incomplete(scope, whole)?;
            if !self.facts.case_of_fields_at(whole) {
                self.emit(whole, uv, expected);
            }
            return Some(uv);
        }
        // An active-pattern name segment cannot be projected as a plain member
        // token, so a path carrying one is unmodelled here.
        if long_ident.active_pat_names().next().is_some() {
            scope.mark_incomplete(Incomplete::MemberAccessShape);
            return None;
        }
        let idents: Vec<SyntaxToken> = long_ident.idents().collect();
        // Need a head *and* at least one member segment.
        let (head, segments) = match idents.split_first() {
            Some((head, segments)) if !segments.is_empty() => (head, segments),
            _ => {
                scope.mark_incomplete(Incomplete::MemberAccessShape);
                return None;
            }
        };
        // The head must resolve to a value binder, in this file or a preceding
        // one; otherwise this is a qualified static path (or unresolved) we do
        // not model here.
        let recorded = scope.reasons_recorded();
        let Some(recv) = self.receiver_var(scope, head) else {
            scope.mark_consequence(recorded, Incomplete::QualifiedPath);
            return None;
        };
        // Emit the receiver's own value node (coercion-free — synth), guarded on
        // the access resolving: FCS keeps no receiver node for a rejected one.
        self.out.open_receiver_guards(segments);
        self.emit(head.text_range(), recv, None);
        self.out.close_guards(segments.len());
        let result = self.gen_member_access(scope, recv, segments)?;
        self.emit(self.facts.node_span(li.syntax()), result, expected);
        Some(result)
    }

    /// Type a `(expr).Member` member access — a `DOT_GET_EXPR` (Stage 3.3a). The
    /// inner receiver *expression* is synthesized (its own node emitted where FCS
    /// has one — e.g. the `"hi"` literal in `"hi".Length`), and the member path
    /// chains from its variable. Returns the whole access's type, or `None` when
    /// the receiver does not synthesize or the member path is empty/unprojectable.
    pub(super) fn infer_dot_get(
        &mut self,
        scope: &mut Scope,
        dg: &DotGetExpr,
        expected: Option<TyVid>,
    ) -> Option<TyVid> {
        let Some(recv_expr) = dg.expr() else {
            scope.mark_incomplete(Incomplete::Recovery);
            return None;
        };
        let Some(long_ident) = dg.long_ident() else {
            scope.mark_incomplete(Incomplete::Recovery);
            return None;
        };
        if long_ident.active_pat_names().next().is_some() {
            scope.mark_incomplete(Incomplete::MemberAccessShape);
            return None;
        }
        let segments: Vec<SyntaxToken> = long_ident.idents().collect();
        if segments.is_empty() {
            scope.mark_incomplete(Incomplete::Recovery);
            return None;
        }
        // The receiver is a coercion-free (synth) sub-position — a member access
        // never coerces its receiver — so synthesize it, emitting its own node,
        // guarded on the access resolving: FCS keeps nothing inside the receiver
        // of a rejected one.
        self.out.open_receiver_guards(&segments);
        let recv = self.infer_expr(scope, &recv_expr, None);
        self.out.close_guards(segments.len());
        let recv = recv?;
        self.out.dot_get_receiver(dg.syntax().text_range(), recv);
        let result = self.gen_member_access(scope, recv, &segments)?;
        self.emit(self.facts.node_span(dg.syntax()), result, expected);
        Some(result)
    }

    /// The inference variable for a member-access **receiver** whose head token
    /// resolves to an in-file value binder — mirroring [`Self::infer_callee`]'s
    /// ident resolution but for a receiver: a generalised binder instantiates its
    /// scheme afresh; a plain in-file binder contributes its variable (poisoned if
    /// it is an *environment* reference, so a still-open receiver blocks
    /// generalisation). A head the resolver resolved to a **preceding file's**
    /// top-level value takes the type that file published, instantiated as a
    /// scheme's is; one it published no type for records why. `None` if the
    /// head does not resolve to such a value either (a qualified static path,
    /// an assembly name, or unresolved) — the caller then defers, leaving any
    /// static-member resolution untouched.
    fn receiver_var(&mut self, scope: &mut Scope, head: &SyntaxToken) -> Option<TyVid> {
        let Some(def) = self.facts.def_at(head.text_range()) else {
            let Some(Resolution::Item(id)) = self.facts.resolved.resolution_at(head.text_range())
            else {
                return None;
            };
            if self.facts.resolved.owns_item(id) {
                return None;
            }
            let ty = match self.facts.preceding.get(id) {
                Ok(&ty) => ty,
                Err(gap) => {
                    scope.mark_incomplete(Incomplete::PrecedingValue(gap));
                    return None;
                }
            };
            let body = self.solver.instantiate(self.facts.env.types(), ty);
            let uv = self.solver.table.fresh();
            self.solver.eq(self.solver.tys.var(uv), body);
            return Some(uv);
        };
        if self.binders.is_withheld(def) {
            scope.mark_incomplete(Incomplete::RigidTypeVariable);
            return None;
        }
        if let Some(scheme) = self.binders.scheme(def) {
            let body = self.solver.instantiate(self.solver.tys, scheme);
            let uv = self.solver.table.fresh();
            self.solver.eq(self.solver.tys.var(uv), body);
            return Some(uv);
        }
        let dv = self.def_var(def);
        if !scope.is_param(def) {
            scope.poison.push(dv);
            scope.opaque.push((dv, None));
        }
        Some(dv)
    }

    /// Wake a suspended data-member access `m` on the project type `head`
    /// when `head` is a record the project declares with a field of that name
    /// that inference reads, accessible at the access: relate the access's
    /// result to the field's type, the record's parameters instantiated at
    /// `head`'s arguments. `false` for anything else (a method call, a
    /// member that is not a field, a record not read).
    ///
    /// FCS looks a field up by the receiver's type when it knows that type
    /// at the access. When it does not, it resolves the label through the
    /// record-label environment and fixes the receiver to that record, so a
    /// receiver inference learns only later is that record too, or FCS
    /// rejects the program.
    pub(super) fn wake_record_field(&mut self, head: Ty, m: &SuspendedMember) -> bool {
        let TyKind::Project { path, args } = self.solver.tys.kind(head) else {
            return false;
        };
        if !matches!(m.kind, MemberAccessKind::Data) {
            return false;
        }
        let record = self
            .facts
            .project_records
            .get(&(path.clone(), args.len()))
            .or_else(|| self.facts.preceding.record(path, args.len()));
        let Some(record) = record.filter(|r| {
            r.scope
                .is_none_or(|scope| scope.contains_range(m.use_range))
        }) else {
            return false;
        };
        let Some(&field) = record.fields.get(&m.name) else {
            return false;
        };
        let declared = record.record;
        let env_tys = self.facts.env.types();
        let mut fresh: HashMap<u32, TyVid> = HashMap::default();
        let receiver = self.solver.instantiate_rec(env_tys, declared, &mut fresh);
        let field = self.solver.instantiate_rec(env_tys, field, &mut fresh);
        self.solver.eq(receiver, head);
        self.solver.eq(self.solver.tys.var(m.result), field);
        true
    }

    /// Wake a suspended member: look up the member `name` on the receiver type
    /// `path` — dispatched on `kind` to the unambiguous public instance **data
    /// member** ([`AssemblyEnv::instance_data_member`], Stage 3.3a) or the
    /// single-candidate public instance **method** ([`AssemblyEnv::instance_method`],
    /// Stage 3.3d) — unify its bridged type (a data member's type, or a method's
    /// **return** type) with `result`, and record its identity at `use_range` (Stage
    /// 3.3b). Silent on any miss (D5): no such entity, no such member, an ambiguous /
    /// overloaded name, or a member type this stage does not bridge — in each case
    /// `result` stays open, the access defers, and **nothing** is recorded at
    /// `use_range` (never from a guess).
    ///
    /// The member resolution is recorded even when the *type bridge* fails (a generic
    /// member type) or the return is `void` (a `unit` type this phase does not model)
    /// as long as the member is unambiguously identified — hover / go-to-definition
    /// can render a member whose type inference cannot yet express. It is placed
    /// **before** the bridge/void checks accordingly.
    ///
    /// Returns whether the wake **fired** — committed *or* definitively deferred, so
    /// the solve loop drops it. `false` is a **retry** request: an overload group
    /// whose argument types are not yet ground (an argument that is itself a pending
    /// member/application). The loop re-parks it so a later wake that grounds the
    /// argument can re-trigger resolution, rather than dropping the call after one
    /// premature attempt.
    pub(super) fn wake_member(
        &mut self,
        scope: &mut Scope,
        path: &[String],
        name: &str,
        result: TyVid,
        use_range: TextRange,
        kind: &MemberAccessKind,
    ) -> bool {
        let Some((type_name, namespace)) = path.split_last() else {
            return true;
        };
        // The receiver is a non-generic `TyKind::Named`, so arity 0. A missing entity is
        // silence. The member lookup below walks this entity's base chain (Stage
        // 3.x-inh), so an inherited member is found and returned under its declaring
        // base's handle.
        //
        // This is the one place inference itself names an assembly type — a
        // literal's receiver type (`"hi".Length`) is a path this phase writes down,
        // vetted by nothing. So it reads through the drop-marker
        // pairing ([`AssemblyEnv::lookup_type_if_certain`]) rather than the raw
        // index: a dropped same-FQN sibling would leave us committing the
        // survivor's members where FCS binds the dropped type's. The other two
        // doors ([`Self::entity_annotation_ty`], [`Self::static_callee`]) key on an
        // `Entity` the resolver's type-path walk already vetted, and reach this
        // lookup only through the path they reconstruct from it.
        let Some(handle) = self
            .facts
            .env
            .lookup_type_if_certain(namespace, type_name, 0)
        else {
            return true;
        };
        let looked_up = match kind {
            MemberAccessKind::Data => self.facts.env.instance_data_member(handle, name),
            MemberAccessKind::Method { args, is_static } => {
                // A non-positional argument shape (named argument, recovery hole) is
                // not typed by FCS as the method return — defer fully (no type, no
                // member resolution recorded, since an ill-formed call has no
                // resolution FCS would agree on).
                let Some(arg_vids) = args else {
                    return true;
                };
                // Resolve each argument's inference variable to its (possibly still
                // open) ground type — the applicability matcher's input.
                let arg_tys: Vec<Ty> = arg_vids
                    .iter()
                    .map(|v| self.solver.table.resolve(self.solver.tys.var(*v)))
                    .collect();
                // **Extension-absence gate** (§4.1(4), the P15 landmine): an in-scope
                // extension member of the name would join FCS's method group and
                // could win, so commit *any* intrinsic overload only when no such
                // extension can exist ([`ExtensionScope`]). Unproven ⇒ defer. The
                // gate covers statics too (OV-7): an F# augmentation `type T with
                // static member …` in an opened / auto-opened module joins a
                // type-qualified static group the same way.
                if !self
                    .facts
                    .ext_scope
                    .absent(self.facts.env, name, *is_static)
                {
                    return true;
                }
                // The provably-complete intrinsic method group (§4.1(1)(2)(3)(5)),
                // instance or static per the call shape (OV-7); an incomplete group
                // (unresolvable base, Object-cap, skipped or kind-clashing member)
                // declines and the call defers.
                let group = if *is_static {
                    self.facts.env.static_method_group(handle, name)
                } else {
                    self.facts.env.instance_method_group(handle, name)
                };
                let Some(group) = group else {
                    return true;
                };
                // **Curried-member gate** (OV-6.1; plan §5). An F# member with
                // several argument groups (`member x.M a b`) compiles to a
                // *flattened* parameter list indistinguishable from a tupled
                // `member x.M(a, b)` — so committing the tupled return type for a
                // curried member is unsound (FCS types `c.M 1 2` a function, not
                // the return). A member is provably a single argument group only
                // when `arg_group_count == Some(1)` — the C#/VB fact stamped by
                // the projector, blanked to `None` for any F# assembly — or it
                // takes ≤ 1 parameter (which cannot split across groups). If ANY
                // candidate in the group is possibly curried, the *whole* call
                // defers: FCS reports FS0816 (curried overload) and types the
                // call `obj`, so even a single-group winner beside a curried
                // loser must not commit. See `docs/completed/ov-6.1-curry-detection-plan.md`.
                // *Possibly curried* = ≥ 2 parameters and not provably a single
                // group (`arg_group_count != Some(1)`).
                if group.iter().any(|(_, _, m)| {
                    m.signature.parameters.len() >= 2 && m.arg_group_count != Some(1)
                }) {
                    return true;
                }
                match group.as_slice() {
                    // FCS's **single-candidate shortcut** (§2.2): one intrinsic
                    // candidate is committed on arity alone (no per-argument type
                    // check — wrong types surface as later errors, but the call still
                    // elaborates with that member). Decline the generic / constructor
                    // shapes v1 does not type, and — crucially — any **byref/out**
                    // signature (§5): the arity window counts a trailing `out` as
                    // omittable, but FCS folds an *omitted* `out` into a tuple return
                    // (`bool` → `bool * v`) this stage does not model, so committing
                    // the raw return would be unsound. The arity window otherwise
                    // admits the optional/`params` trimming FCS applies.
                    //
                    // A trailing **`[<ParamArray>]`** is excluded from this shortcut:
                    // a single declared params method is *two* FCS candidates
                    // (expanded and direct-array, §2.2), so it is not a single-candidate
                    // shortcut at all — `V(params int[])` called `V("x")` fails both
                    // forms' applicability, which arity alone cannot see. Route it
                    // through the matcher (the `_` arm) like a genuine overload set.
                    [(level, idx, m)]
                        if !m
                            .signature
                            .parameters
                            .last()
                            .is_some_and(|p| p.is_param_array) =>
                    {
                        // Direct unit syntax (`M()` — `arg_tys` is empty, the only
                        // zero-argument shape) is **candidate-dependent** in FCS:
                        // zero arguments when the candidate admits arity 0, else
                        // ONE (possibly ill-typed) unit argument — and the
                        // single-candidate shortcut still elaborates the call as
                        // the member (probed 2026-07-10, GPT-5.6 review:
                        // `String.IsNullOrEmpty()` ⇒ `Boolean`; instance `c.M()`
                        // on a 1-param `M(string)` ⇒ `Int32`). So a unit call also
                        // accepts a window containing 1. The multi-candidate
                        // matcher below keeps the zero reading only — FCS does not
                        // elaborate an ill-typed unit arg through overload
                        // resolution (`"hi".Substring()` ⇒ `obj`, no member call),
                        // and a deferral there is sound regardless.
                        let window = arity_window(m);
                        let arity_fits = window.contains(arg_tys.len())
                            || (arg_tys.is_empty() && window.contains(1));
                        if !m.is_constructor
                            && m.generic_parameters.is_empty()
                            && !m
                                .signature
                                .parameters
                                .iter()
                                .any(|p| p.is_byref || p.is_out)
                            && arity_fits
                        {
                            Some((*level, *idx, *m))
                        } else {
                            None
                        }
                    }
                    // A genuine overload set (≥ 2 distinct members) — or a single
                    // params method (two normalised FCS forms): the OV-6 commit
                    // keystone (§1) needs GROUND argument types. If any argument is
                    // not yet ground — it may be a pending member/application whose
                    // result grounds on a later wake — signal a **retry** (`false`)
                    // so the loop re-parks rather than dropping the call. Once ground,
                    // commit the unique applicable candidate or defer
                    // ([`AssemblyEnv::resolve_overload`]).
                    _ => {
                        if !arg_tys.iter().all(|&t| self.solver.tys.is_ground(t)) {
                            return false;
                        }
                        // The matcher reads each candidate's parameter types,
                        // and an IL parameter type with an erased unit of
                        // measure admits an argument FCS rejects: `O(float<m>)`
                        // beside `O(string)` reads as `O(float64)`, so `O(1.0)`
                        // would pick it where FCS finds no overload. Which
                        // member is chosen is then not known either, so the
                        // whole call defers.
                        if !group.iter().all(|(level, _, _)| {
                            self.facts.env.member_types_keep_their_measures(*level)
                        }) {
                            return true;
                        }
                        self.facts
                            .env
                            .resolve_overload(&group, self.solver.tys, &arg_tys)
                    }
                }
                // The curried-member gate above already deferred the whole call
                // when any candidate was possibly curried, so every survivor here
                // is a provably single-group member — map it to its return type.
                .map(|(level, idx, m)| (level, idx, &m.signature.return_type))
            }
        };
        // `decl_handle` is the entity that *declares* the member — `handle` itself for
        // a member declared on the receiver, or a base type when it is inherited
        // (Stage 3.x-inh). The member resolution is recorded under it so hover /
        // go-to-def point at the declaring type.
        let Some((decl_handle, idx, member_ty_ref)) = looked_up else {
            return true;
        };
        // Bridge the member type before touching `self` mutably (the borrow of
        // `member_ty_ref` from `self.facts.env` would otherwise conflict with the insert
        // / unify below); `type_ref_to_ty` yields an owned `Ty` or `None`, and the
        // void check reads the `TypeRef` while it is still borrowed.
        let member_ty = type_ref_to_ty(self.facts.env.names(), self.solver.tys, member_ty_ref);
        // A `void`-returning method's call type is F# `unit`, which this phase does
        // not model — so its type defers even though its identity is known (a data
        // member is never `void`, so this is a no-op for `kind = Data`). Bridging
        // `void` would yield the wrong `System.Void`, so we must skip the unify.
        let is_void = matches!(member_ty_ref, TypeRef::Primitive(Primitive::Void));
        // A single unambiguous public instance member was identified: record its
        // resolution for the LSP (Stage 3.3b), in the resolver's own shape. Recorded
        // even if the type bridge failed (a generic member type) or the return is
        // void — the member's *identity* is known, so hover / go-to-definition can
        // render it though the type defers.
        self.out.member_resolution(
            use_range,
            Resolution::Member {
                parent: decl_handle,
                idx,
            },
        );
        // The access is one FCS accepts, so what its receiver emitted stands.
        // Whether the receiver has nodes at all is then an assembly reading —
        // the member's existence — so under the seal below it is not confirmed:
        // an incomplete projection publishes only what an empty env would.
        //
        // Nor is it on an F# record, union or exception type: FCS reads such a
        // type's field as a field get, under which the typed tree keeps no
        // node at the receiver (`let f (v: R) = v.Q` has one at `v.Q` only).
        // Its other members keep one, so this under-publishes there.
        let fsharp_field_carrier = matches!(
            self.facts.env.entity(decl_handle).kind,
            EntityKind::Record | EntityKind::Union | EntityKind::Exception
        );
        if !fsharp_field_carrier && !self.facts.env.identities_incomplete() {
            self.out.confirm_guard(use_range);
        }
        if is_void {
            return true;
        }
        let Some(member_ty) = member_ty else {
            return true;
        };
        // IL erases units of measure, so the signature read above is the
        // member's F# type only when its declaring entity's pickle says no
        // measure was erased: a `float<m>` field reads as `float64`, and FCS
        // types the access `float<m>`. The member's identity is known either
        // way, and stays recorded above.
        if !self.facts.env.member_types_keep_their_measures(decl_handle) {
            return true;
        }
        // **The seal, drawn at the door** (see [`Self::finish`]). Under an
        // incomplete projection the unread DLL could declare a colliding
        // receiver type whose member of this name returns something else, so
        // this type is not provably the member's — and once it is unified into
        // `result` nothing downstream can tell it apart from a type owing the
        // assemblies nothing (a literal's). Declining the unify here is what
        // makes the two distinguishable at all: `result` stays open, so
        // `finish` drops it for being non-ground, while `let n = 1` keeps its
        // `int`. The member's *identity* is still recorded above — and sealed
        // with the rest — so the LSP can say why the type is missing.
        if self.facts.env.identities_incomplete() {
            return true;
        }
        if self
            .solver
            .table
            .unify_atomic(self.solver.tys.var(result), member_ty)
            .is_err()
        {
            scope.eq_failures += 1;
        }
        true
    }
}

/// Whether an argument element is a **named argument** `name = value` (Stage 3.3d).
/// F# parses it as `App[ InfixApp[name, "="], value ]` — an outer (non-infix)
/// application whose function is the infix `=` operator applied to the name. A
/// *positional* infix argument (`a + b`, itself an infix `App`) is **not** a named
/// argument (the outer `is_infix()` guard), and neither is a nested `=` inside a
/// record / lambda (which is not the element's own top-level operator). The wake
/// cannot validate the name against the method's parameters, so any named argument
/// defers the whole call (conservative — even correct names defer, but never wrong).
pub(super) fn is_named_arg(el: &Expr) -> bool {
    let Expr::App(outer) = el else {
        return false;
    };
    // A positional infix element (`a + b`) is the infix `App` itself, not an outer
    // application *of* an infix — so exclude it here.
    if outer.is_infix() {
        return false;
    }
    let Some(Expr::App(op_app)) = outer.func() else {
        return false;
    };
    op_app.is_infix()
        && op_app
            .func()
            .is_some_and(|op| op.syntax().text().to_string().trim() == "=")
}
