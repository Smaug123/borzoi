//! Expression and pattern name resolution.

use borzoi_cst::syntax::{AstNode, Expr, InterpStringPart, MatchClause, Pat, SyntaxToken};

use crate::binders::{BinderRole, binders};
use crate::def::{Def, DefKind};

use super::id_text;
use super::model::{DeferredReason, Resolution};
use super::state::{Frame, Resolver, ScopeEntry};

impl<'a> Resolver<'a> {
    pub(super) fn resolve_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Const(_) => {}
            // A unit-of-measure annotated literal (`1.0<m>`). The underlying
            // constant binds nothing, and the measure names (`m`, `kg`, …) live
            // in F#'s separate measure namespace, which this resolver does not
            // model — so, like a plain `Const`, it references no value names.
            Expr::MeasureLit(_) => {}
            // `null` (FCS's `SynExpr.Null`) binds and references no names.
            Expr::Null(_) => {}
            Expr::Ident(e) => {
                if let Some(tok) = e.ident() {
                    self.resolve_name_use(&tok);
                }
            }
            // `'T` (FCS's `SynExpr.Typar`) — a type parameter used as the head of
            // a statically-resolved `'T.Member` call. It names a *type* parameter,
            // resolved against the open typar frames (a member/type/`let` header's
            // `<'T>`), exactly like a `Type::Var` use. (The `.Member` is resolved
            // against `'T`'s constraint type, which needs type inference we don't
            // do here; the enclosing `DotGet` already leaves the member path
            // alone.) Unrecorded — a sound deferral — when no header declares it.
            Expr::Typar(e) => {
                if let Some(tok) = e.ident()
                    && let Some(id) = self.lookup_typar(id_text(tok.text()))
                    && let Some((range, _)) = super::types::typar_occurrence(e.syntax())
                {
                    self.record(range, Resolution::Local(id));
                }
            }
            Expr::LongIdent(e) => {
                // A path carrying an active-pattern-name `opName` segment
                // (`(|Foo|_|)`, the folded `(|Foo|_|).Bar`, the qualified
                // `Foo.(|Bar|_|)`) is deferred. The recognizer is not keyed in
                // value scope (`define_active_pattern` exposes only the cases),
                // and `idents()` cannot see the name node — so feeding the
                // *remaining* tokens to `resolve_long_ident` would mis-read them
                // as the whole path, resolving the `Foo` qualifier / the `.Bar`
                // member as a value. So skip: a coverage gap (active-pattern
                // value references are not modelled yet), never wrong.
                if let Some(li) = e.long_ident()
                    && li.active_pat_names().next().is_none()
                {
                    let segments: Vec<SyntaxToken> = li.idents().collect();
                    self.resolve_long_ident(&segments);
                }
            }
            Expr::Paren(e) => {
                if let Some(inner) = e.inner() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::Tuple(e) => {
                for el in e.elements() {
                    self.resolve_expr(&el);
                }
            }
            Expr::App(e) => {
                if let Some(f) = e.func() {
                    self.resolve_expr(&f);
                }
                if let Some(a) = e.arg() {
                    self.resolve_expr(&a);
                }
            }
            Expr::DotGet(e) => {
                // `expr.Member` — resolve the LHS value expression. The member
                // path is resolved against the LHS's *type* (not a value
                // binder), and we don't model types here, so the member names
                // are left alone rather than mis-resolved to locals (same
                // treatment as record field labels).
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::Dynamic(e) => {
                // `a?b` — the dynamic lookup. The LHS is an ordinary value
                // expression, resolved in the enclosing scope. The argument is the
                // dynamic *member name*: for the bare-ident form (`a?b`) it is a
                // member resolved against the LHS's type at runtime, *not* a value
                // binder, so it is left alone (same treatment as a `DotGet` member
                // path) — resolving it would wrongly bind to a same-named local.
                // Only the parenthesised form (`a?(e)`) carries a real value
                // sub-expression, so resolve the argument only when it is a paren.
                if let Some(lhs) = e.lhs() {
                    self.resolve_expr(&lhs);
                }
                if let Some(arg @ Expr::Paren(_)) = e.arg() {
                    self.resolve_expr(&arg);
                }
            }
            Expr::DotLambda(e) => {
                // `_.Member` — the accessor-function shorthand `(fun x -> x.Member)`.
                // The member spine (`M`, `.N`, …) is accessed off the
                // *synthesised* parameter, resolved against its type (which sema
                // does not model), so it carries no in-file value reference and
                // must not be mis-resolved to a same-named local. The value
                // sub-expressions in member *arguments* / *indices* (`arg` in
                // `_.M(arg)`) do resolve, in the enclosing scope — the anonymous
                // parameter is unreferenceable, so no scope frame is introduced.
                if let Some(body) = e.expr() {
                    self.resolve_dot_lambda_body(&body);
                }
            }
            Expr::DotIndexedGet(e) => {
                // `obj.[index]` — both the indexed object and the index
                // expression resolve in the enclosing scope.
                if let Some(object) = e.object() {
                    self.resolve_expr(&object);
                }
                if let Some(index) = e.index() {
                    self.resolve_expr(&index);
                }
            }
            Expr::IndexRange(e) => {
                // `lower..upper` — each present bound is an ordinary value
                // expression resolved in the enclosing scope; an absent bound
                // (open range) contributes nothing.
                if let Some(lower) = e.lower() {
                    self.resolve_expr(&lower);
                }
                if let Some(upper) = e.upper() {
                    self.resolve_expr(&upper);
                }
            }
            Expr::IndexFromEnd(e) => {
                // `arr.[^expr]` — the from-end bound is an ordinary value
                // expression resolved in the enclosing scope.
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::AddressOf(e) => {
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::New(e) => {
                // `new T(args)` — resolve the target type `T` (a type use) and
                // the constructor argument expression in the enclosing scope.
                if let Some(ty) = e.target_type() {
                    self.resolve_type(&ty);
                }
                if let Some(arg) = e.arg() {
                    self.resolve_expr(&arg);
                }
            }
            Expr::ObjExpr(e) => {
                // `{ new T(args) with member … interface I with member … }` —
                // resolve the object type `T` and the constructor argument in the
                // enclosing scope (same treatment as `New`). The `with member …`
                // and `interface I with member …` bodies introduce their own
                // `this`/parameter binders; resolving them in the enclosing scope
                // *without* that scoping could mis-resolve a parameter to an outer
                // binder of the same name (unsound), so — exactly like
                // type-definition member bodies (`ModuleDecl::Types`) — they (and
                // the implemented interface type names) are left for the dedicated
                // member-resolution slice. Under-resolution here is sound
                // (availability only, never a wrong resolution).
                if let Some(ty) = e.obj_type() {
                    self.resolve_type(&ty);
                }
                if let Some(arg) = e.arg() {
                    self.resolve_expr(&arg);
                }
            }
            Expr::InferredUpcast(e) => {
                // `upcast e` — the inferred coercion has no syntactic target
                // type; resolve only the coerced operand (like `AddressOf`).
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::InferredDowncast(e) => {
                // `downcast e` — as `InferredUpcast`.
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::Lazy(e) => {
                // `lazy e` — the delayed computation binds no names of its own;
                // the operand resolves in the enclosing scope (like `AddressOf`).
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::Assert(e) => {
                // `assert e` — as `lazy`; the asserted operand resolves in the
                // enclosing scope.
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::Fixed(e) => {
                // `fixed e` — as `lazy`/`assert`; the pinned operand resolves in
                // the enclosing scope and binds no names of its own.
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::TypeApp(e) => {
                // `f<T>` — resolve the type-applied head expression (the value
                // reference `f`) and each type argument (a type use).
                if let Some(head) = e.expr() {
                    self.resolve_expr(&head);
                }
                for ty in e.type_args() {
                    self.resolve_type(&ty);
                }
            }
            Expr::Assign(e) => {
                // `target <- value`. The target is a *use* of an existing
                // mutable binder (a value/field), not a binding site, so it
                // resolves like any other expression; the value is the RHS.
                if let Some(target) = e.target() {
                    self.resolve_expr(&target);
                }
                if let Some(value) = e.value() {
                    self.resolve_expr(&value);
                }
            }
            Expr::Typed(e) => {
                // `(value : T)` — resolve the wrapped value expression and the
                // annotation `T` (a type use).
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
                if let Some(ty) = e.ty() {
                    self.resolve_type(&ty);
                }
            }
            Expr::TypeTest(e) => {
                // `e :? T` — resolve the tested expression and the target type.
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
                if let Some(ty) = e.ty() {
                    self.resolve_type(&ty);
                }
            }
            Expr::Upcast(e) => {
                // `e :> T` — resolve the cast expression and the target type.
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
                if let Some(ty) = e.ty() {
                    self.resolve_type(&ty);
                }
            }
            Expr::Cons(e) => {
                // `a :: b` — the cons operator. Both operands are ordinary value
                // expressions resolved in the enclosing scope; the `::` operator
                // itself names no in-file binder.
                if let Some(lhs) = e.lhs() {
                    self.resolve_expr(&lhs);
                }
                if let Some(rhs) = e.rhs() {
                    self.resolve_expr(&rhs);
                }
            }
            Expr::JoinIn(e) => {
                // `lhs in rhs` — the query computation-expression join operator
                // (`join x in xs on (a = b)`). Both operands are ordinary value
                // expressions resolved in the enclosing scope; the join-specific
                // binding/`on` semantics live in the CE builder's translation,
                // which sema does not model, so we under-resolve soundly here
                // (the same treatment as `Cons`).
                if let Some(lhs) = e.lhs() {
                    self.resolve_expr(&lhs);
                }
                if let Some(rhs) = e.rhs() {
                    self.resolve_expr(&rhs);
                }
            }
            Expr::Downcast(e) => {
                // `e :?> T` — resolve the cast expression and the target type.
                if let Some(inner) = e.expr() {
                    self.resolve_expr(&inner);
                }
                if let Some(ty) = e.ty() {
                    self.resolve_type(&ty);
                }
            }
            Expr::IfThenElse(e) => {
                for sub in [e.condition(), e.then_branch(), e.else_branch()]
                    .into_iter()
                    .flatten()
                {
                    self.resolve_expr(&sub);
                }
            }
            Expr::Sequential(e) => {
                for stmt in e.statements() {
                    self.resolve_expr(&stmt);
                }
            }
            Expr::InterpString(e) => {
                for part in e.parts() {
                    // The `: ident` format qualifier names a .NET format
                    // specifier, not a binding, so only the fill expression is
                    // resolved.
                    if let InterpStringPart::Fill { expr, .. } = part {
                        self.resolve_expr(&expr);
                    }
                }
            }
            Expr::Fun(e) => {
                let entries = self.pattern_locals(e.args(), BinderRole::Param);
                self.scopes.push(Frame { entries });
                if let Some(body) = e.body() {
                    self.resolve_expr(&body);
                }
                self.scopes.pop();
            }
            Expr::Quote(e) => {
                // A quotation `<@ … @>` captures the enclosing scope; resolve
                // the uses inside it against that scope.
                if let Some(inner) = e.inner() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::InlineIl(e) => {
                // Inline IL `(# "instr" type (T) arg₀ … : retTy #)`. The
                // instruction string is opaque (no names), but the value
                // arguments and the type operands (`type (T)` arg + `: retTy`
                // return) reference enclosing bindings/types, so resolve them in
                // the enclosing scope. The expression binds no names of its own.
                for arg in e.args() {
                    self.resolve_expr(&arg);
                }
                for ty in e.types() {
                    self.resolve_type(&ty);
                }
            }
            Expr::LibraryOnlyFieldGet(e) => {
                // A library-only cons-cell field read `obj.( :: ).<int>`
                // (FSharp.Core). The object references an enclosing binding, so
                // resolve it; the cons name and field number bind/reference no
                // value names.
                if let Some(obj) = e.object() {
                    self.resolve_expr(&obj);
                }
            }
            Expr::StaticOptimization(e) => {
                // A static-optimization binding RHS — `mainExpr when 'T : ty =
                // branch …` (FSharp.Core). The fallthrough main expression and
                // each clause's branch reference enclosing bindings, so resolve
                // them in the enclosing scope. The condition types (`ty` in
                // `'T : ty`) live in F#'s type namespace and reference no value
                // names; the expression binds no names of its own.
                if let Some(main) = e.main_expr() {
                    self.resolve_expr(&main);
                }
                for clause in e.clauses() {
                    for cond in clause.conditions() {
                        if let Some(ty) = cond.ty() {
                            self.resolve_type(&ty);
                        }
                    }
                    if let Some(branch) = clause.branch() {
                        self.resolve_expr(&branch);
                    }
                }
            }
            Expr::TraitCall(e) => {
                // An SRTP trait call `( ^a : (static member M : sig) arg )`. The
                // argument expression references enclosing bindings (`arg`), so
                // resolve it in the enclosing scope. The member signature's own
                // types (`sig`) are deferred, mirroring the SRTP member
                // *constraint*, which likewise does not resolve its member-sig
                // internals; the call binds no names of its own.
                //
                // Resolve *every* support type, not just the first. The support is
                // FCS's `typarAlts` — `typar (or appType)*` — so while the head is
                // always a typar (a `Type::Var`, a no-op here), a later alternative
                // is a full type and can name something real: `((^a or Witness) : …)`
                // must take `Witness` to its definition.
                for support in e.support_types() {
                    self.resolve_type(&support);
                }
                if let Some(arg) = e.arg() {
                    self.resolve_expr(&arg);
                }
            }
            Expr::Computation(e) => {
                // A computation-expression body (`seq { … }`); the builder name
                // is resolved by the enclosing `App`. Walk the body for the
                // uses we can resolve. The `let!`/`use!`/`and!` binders inside
                // are now modelled as `Expr::LetOrUse` (their bound names
                // resolve to locals); other CE-specific constructs still fall
                // through to `Deferred`, never to a wrong resolution.
                if let Some(inner) = e.inner() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::Record(e) => {
                // `{ src with F = e; … }` — resolve the copy source (if any) and
                // each field's value in the enclosing scope. The field *names*
                // are record labels resolved against the record's type (not
                // value references); we don't model types, so they are left
                // alone rather than mis-resolved to locals.
                if let Some(src) = e.copy_source() {
                    self.resolve_expr(&src);
                }
                for field in e.fields() {
                    if let Some(value) = field.value() {
                        self.resolve_expr(&value);
                    }
                }
            }
            Expr::AnonRecd(e) => {
                // `{| src with F = e; … |}` — identical scoping to `Record`:
                // resolve the copy source (if any) and each field's value in
                // the enclosing scope; the anonymous-record field labels have
                // no in-file binder and are left alone.
                if let Some(src) = e.copy_source() {
                    self.resolve_expr(&src);
                }
                for field in e.fields() {
                    if let Some(value) = field.value() {
                        self.resolve_expr(&value);
                    }
                }
            }
            Expr::ArrayOrList(e) => {
                // A list `[ … ]` / array `[| … |]` expression. The element body
                // (`None` for an empty `[]` / `[||]`) is a single expr — a
                // `Sequential` for several `;`/offside-separated elements — so
                // resolving it walks every element in the enclosing scope.
                if let Some(inner) = e.inner() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::Yield(e) => {
                // `yield` / `return` / `yield!` / `return!` wrap one expression.
                if let Some(inner) = e.inner() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::DoBang(e) => {
                if let Some(inner) = e.inner() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::Do(e) => {
                // `do e` binds no names; resolve the bound expression in the
                // enclosing scope.
                if let Some(inner) = e.inner() {
                    self.resolve_expr(&inner);
                }
            }
            Expr::LetOrUse(e) if e.is_bang() => {
                // `let!`/`use!`(+`and!`) computation-expression binders. These
                // are non-recursive: each binding RHS is resolved in the
                // enclosing scope (and an applicative `and!` group's RHSs do
                // not see each other's binders), then all the binding patterns
                // are bound as locals for the body.
                //
                // The LHS is a *deconstruction pattern* (`let! Some x = m`), not
                // a function-binding head, so it uses `BinderRole::Pattern`: a
                // constructor head like `Some` stays a reference and only the
                // nested names (`x`) bind — `BinderRole::Let` would mis-read the
                // head as a bound function value.
                let mut entries = Vec::new();
                for b in e.bindings() {
                    if let Some(rhs) = b.expr() {
                        self.resolve_expr(&rhs);
                    }
                    // A typed bang binder (`let! x : MyType = m`,
                    // `AllowTypedLetUseAndBang`) carries a `BINDING_RETURN_INFO`
                    // like a plain `let`; resolve the type names in it so the
                    // annotation participates in name resolution too.
                    if let Some(ret) = b.return_type() {
                        self.resolve_type(&ret);
                    }
                    entries.extend(self.pattern_locals(b.pat().into_iter(), BinderRole::Pattern));
                }
                self.scopes.push(Frame { entries });
                if let Some(body) = e.body() {
                    self.resolve_expr(&body);
                }
                self.scopes.pop();
            }
            Expr::LetOrUse(e) => {
                // Plain expression-level (block) `let`/`use` — a
                // function-binding head, not a deconstruction pattern, with the
                // module-level `let`'s scoping rules (just *local*). See
                // [`Self::resolve_local_let`].
                self.resolve_local_let(e);
            }
            Expr::MatchLambda(e) => {
                // `function | pat -> result | …`: like `Match` but with no
                // scrutinee — each clause's pattern binders scope its guard +
                // result.
                for clause in e.clauses() {
                    self.resolve_match_clause(&clause);
                }
            }
            Expr::Match(e) => {
                // The scrutinee is resolved in the enclosing scope; the
                // clause's pattern binders scope both its `when` guard and its
                // result (the guard precedes the result in source order).
                if let Some(scrutinee) = e.scrutinee() {
                    self.resolve_expr(&scrutinee);
                }
                for clause in e.clauses() {
                    self.resolve_match_clause(&clause);
                }
            }
            Expr::MatchBang(e) => {
                // `match! e with …` — the computation-expression match binder.
                // Identical scoping to `Match`: the scrutinee resolves in the
                // enclosing scope, then each clause's pattern binders scope its
                // guard + result.
                if let Some(scrutinee) = e.scrutinee() {
                    self.resolve_expr(&scrutinee);
                }
                for clause in e.clauses() {
                    self.resolve_match_clause(&clause);
                }
            }
            Expr::While(e) => {
                // `while cond do body` — both the condition and the body
                // resolve in the enclosing scope; a `while` loop binds no names.
                if let Some(cond) = e.cond() {
                    self.resolve_expr(&cond);
                }
                if let Some(body) = e.body() {
                    self.resolve_expr(&body);
                }
            }
            Expr::WhileBang(e) => {
                // `while! cond do body` — same scoping as `while`: condition and
                // body resolve in the enclosing scope, binds no names.
                if let Some(cond) = e.cond() {
                    self.resolve_expr(&cond);
                }
                if let Some(body) = e.body() {
                    self.resolve_expr(&body);
                }
            }
            Expr::ForEach(e) => {
                // `for pat in enumExpr do body` — the collection resolves in the
                // enclosing scope (it cannot see the loop variable), then the
                // binder pattern's names scope the body. Same frame discipline
                // as a `match` clause.
                if let Some(enum_expr) = e.enum_expr() {
                    self.resolve_expr(&enum_expr);
                }
                let entries = self.pattern_locals(e.pat().into_iter(), BinderRole::Pattern);
                self.scopes.push(Frame { entries });
                if let Some(body) = e.body() {
                    self.resolve_expr(&body);
                }
                self.scopes.pop();
            }
            Expr::For(e) => {
                // `for ident = from to/downto to do body` — both bounds resolve
                // in the enclosing scope; the loop variable `ident` (a single
                // pattern-local binder, never a deconstruction) then scopes the
                // body. Same frame discipline as the `ForEach` / match-clause
                // path, but the binder is a bare ident token, not a pattern.
                if let Some(from) = e.from_expr() {
                    self.resolve_expr(&from);
                }
                if let Some(to) = e.to_expr() {
                    self.resolve_expr(&to);
                }
                let mut entries = Vec::new();
                if let Some(ident) = e.ident() {
                    let def = Def::from_token(&ident, DefKind::PatternLocal);
                    let name = id_text(&def.name).to_string();
                    let range = def.range;
                    let id = self.intern(def);
                    let res = Resolution::Local(id);
                    self.record(range, res);
                    entries.push(ScopeEntry::binding(name, res, self.open_generation));
                }
                self.scopes.push(Frame { entries });
                if let Some(body) = e.body() {
                    self.resolve_expr(&body);
                }
                self.scopes.pop();
            }
            Expr::Try(e) => {
                // `try body with <clauses>` / `try body finally cleanup` — the
                // protected body resolves in the enclosing scope (the `try` binds
                // no names of its own). In the `with` form each handler clause's
                // pattern binders then scope its `when` guard + result, exactly
                // as a `match` clause (`resolve_match_clause`); in the `finally`
                // form the cleanup expression resolves in the enclosing scope
                // (it binds no names either). One arm covers both: the clause
                // loop is empty for `try/finally` and `finally_expr()` is `None`
                // for `try/with`.
                if let Some(body) = e.try_expr() {
                    self.resolve_expr(&body);
                }
                for clause in e.with_clauses() {
                    self.resolve_match_clause(&clause);
                }
                if let Some(finally) = e.finally_expr() {
                    self.resolve_expr(&finally);
                }
            }
        }
    }

    /// Resolve the value positions of an accessor-function shorthand body
    /// (`SynExpr.DotLambda`'s `expr`), skipping the member spine rooted at the
    /// implicit parameter. `_.M(arg)` desugars to `fun x -> x.M(arg)`: the spine
    /// (`M`, a trailing `.N`, a generic `<T>`) is member access against `x`'s
    /// type, so it resolves to no in-file value and must not capture a same-named
    /// local; member *arguments* and *indices* are ordinary values resolved in
    /// the enclosing scope (the anonymous `x` is unreferenceable, so they never
    /// see it).
    ///
    /// `resolve_expr` already skips member names on a [`Expr::DotGet`] LHS — the
    /// only position it would over-resolve is the *leftmost* spine head (a bare
    /// `Ident`/`LongIdent` that here names a member, not a value), so this walk
    /// descends the spine and defers to `resolve_expr` for everything off it.
    pub(super) fn resolve_dot_lambda_body(&mut self, body: &Expr) {
        match body {
            // Spine root — the first member(s): `_.Foo` / `_.Foo.Bar`. These
            // name members off the implicit parameter, so resolve nothing.
            Expr::Ident(_) | Expr::LongIdent(_) => {}
            // `_.M(arg)` / `_.M arg`: the function is further down the spine; the
            // argument is an ordinary value.
            Expr::App(e) => {
                if let Some(f) = e.func() {
                    self.resolve_dot_lambda_body(&f);
                }
                if let Some(a) = e.arg() {
                    self.resolve_expr(&a);
                }
            }
            // `_.Foo(x).Bar`: the receiver is the spine; the `.Bar` members are
            // already skipped by `resolve_expr`'s `DotGet` treatment, mirrored
            // here by only descending into the LHS.
            Expr::DotGet(e) => {
                if let Some(inner) = e.expr() {
                    self.resolve_dot_lambda_body(&inner);
                }
            }
            // `_.Items.[i]`: the indexed object is the spine; the index is a
            // value resolved in the enclosing scope.
            Expr::DotIndexedGet(e) => {
                if let Some(object) = e.object() {
                    self.resolve_dot_lambda_body(&object);
                }
                if let Some(index) = e.index() {
                    self.resolve_expr(&index);
                }
            }
            // `_.M<T>(x)`: a generic member — the type-applied head is the spine,
            // the type arguments are types resolved in the enclosing scope.
            Expr::TypeApp(e) => {
                if let Some(f) = e.expr() {
                    self.resolve_dot_lambda_body(&f);
                }
                for ty in e.type_args() {
                    self.resolve_type(&ty);
                }
            }
            // Defensive: a well-formed dot-lambda body is always one of the
            // member-spine shapes above (its head is a member name). Anything
            // else is left alone — conservatively skipping over-resolution of a
            // member rather than risk capturing a local.
            _ => {}
        }
    }

    /// Resolve one `match`/`match!`/`function` clause: push a frame for the
    /// clause pattern's binders, then resolve the optional `when` guard and the
    /// result inside it. The guard and result both see the clause binders; the
    /// guard precedes the result in source order. Shared by [`Expr::Match`],
    /// [`Expr::MatchBang`], and [`Expr::MatchLambda`].
    pub(super) fn resolve_match_clause(&mut self, clause: &MatchClause) {
        let entries = self.pattern_locals(clause.pat().into_iter(), BinderRole::Pattern);
        self.scopes.push(Frame { entries });
        if let Some(guard) = clause.guard() {
            self.resolve_expr(&guard);
        }
        if let Some(result) = clause.result() {
            self.resolve_expr(&result);
        }
        self.scopes.pop();
    }

    /// Intern the binders of each pattern in `pats` (under `role`), recording
    /// each binder's self-resolution, and return them as scope entries for a
    /// fresh frame. Used for lambda parameters and `match`-clause patterns —
    /// both bind locals, never exported items.
    pub(super) fn pattern_locals(
        &mut self,
        pats: impl Iterator<Item = Pat>,
        role: BinderRole,
    ) -> Vec<ScopeEntry> {
        // Lambda parameters (`BinderRole::Param`) are a binding-head position:
        // curried params scope left to right, but this walk resolves each param's
        // patterns before interning any binder, so an active-pattern argument in a
        // later param (`fun d (DivBy d) -> …`) must not commit its expression
        // against the enclosing scope (the earlier `d` is not yet in scope). Decline
        // such argument resolutions here (the binder exclusion still runs); a
        // `match`-clause pattern (`BinderRole::Pattern`) keeps full resolution
        // against the enclosing scope, which is FCS's rule there. See
        // [`Self::decline_binding_head_param_exprs`].
        let saved = std::mem::replace(
            &mut self.decline_binding_head_param_exprs,
            role == BinderRole::Param,
        );
        let mut entries = Vec::new();
        for pat in pats {
            // Annotations in the pattern (`fun (x : T) -> …`, `match … with
            // (x : T) -> …`, `:? T`) are type uses, resolved alongside the
            // binders the pattern introduces.
            self.resolve_pat_types(&pat);
            for def in binders(&pat, role) {
                // An active-pattern *parameter* argument (`divisor` in `match n
                // with DivBy divisor -> …`): the shape-keyed split
                // ([`Self::split_active_pattern_args`], run by `resolve_pat_types`
                // just above) has already resolved it as an expression and excluded
                // its fabricated binder range. Skip it — before the `provisional`
                // branch, so a would-be provisional case-reference head is dropped
                // too — leaving no recorded self-resolution and no scope entry.
                if self.excluded_param_ranges.contains(&def.range) {
                    continue;
                }
                // Provisional maybe-var head (`None` in `match … with None -> …`,
                // `fun None -> …`): resolve a known union-case reference, else
                // decline (drop). See the module-level "Provisional pattern
                // heads" note.
                if def.provisional {
                    if let Some(res) = self.case_reference(&def.name) {
                        self.record(def.range, res);
                    }
                    continue;
                }
                let name = id_text(&def.name).to_string();
                let range = def.range;
                let id = self.intern(def);
                let res = Resolution::Local(id);
                self.record(range, res);
                entries.push(ScopeEntry::binding(name, res, self.open_generation));
            }
        }
        self.decline_binding_head_param_exprs = saved;
        entries
    }

    pub(super) fn resolve_name_use(&mut self, tok: &SyntaxToken) {
        // The `base` keyword reaches here as the receiver of a direct
        // `base.[i]` indexer (parsed as a bare `Ident("base")` head). Like the
        // `base`-headed long-ident path, it is the reserved base-class receiver,
        // not a value binder, so it must never resolve to an in-file name — not
        // even a back-ticked `` ``base`` `` binder (whose token text carries the
        // backticks, so the unquoted-`base` test distinguishes them). Defer it.
        if tok.text() == "base" {
            return;
        }
        // A bare use of a name that is one of the *currently-being-resolved*
        // active-pattern recognizer's own cases (see [`Resolver::ap_body_case_names`]):
        // inside the recognizer's own body this is ambiguous between constructing
        // the result case (FCS `ActivePatternCase`) and a fresh uppercase pattern
        // rebinding (`match n with A -> A`, FCS a fresh local), which a
        // resolution-only pass cannot tell apart. Decline (say nothing) rather than
        // commit an outer same-named value (the AP-body-shadow bug) or the case —
        // sound either way. Only *bare* single-ident uses reach here; a qualified
        // head (`A.X`) goes through [`Self::resolve_long_ident`], which ignores this.
        if self.ap_body_case_names.contains(id_text(tok.text())) {
            return;
        }
        // Every name an `open` brings into scope is an *opened* entry in the value
        // frame alongside locals, parameters, and union/exception cases (see
        // [`Self::top_level`] and [`Self::open_type_statics`]). So the ordinary
        // [`lookup`](Self::lookup) — innermost frame first, latest entry within a
        // frame — resolves opened statics, opened module values, locals, and cases
        // uniformly by source order, the latest in scope winning. An unbound name
        // (or one shadowed away by an opaque open) falls back to `Deferred`.
        let res = self
            .lookup(id_text(tok.text()))
            .unwrap_or(Resolution::Deferred(DeferredReason::UnboundName));
        self.record(tok.text_range(), res);
    }
}
