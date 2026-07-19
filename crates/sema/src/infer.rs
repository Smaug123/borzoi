//! Best-effort type inference over the parsed CST (Phase 3).
//!
//! **Stage 3.1 — literal typing in soundness-safe positions.** A literal's
//! *displayed* F# type is not fixed by its lexical kind: F# retargets literals
//! by the **expected type** that reaches them. `let x: int64 = 42` makes `42` an
//! `int64`; `printfn "%d"` makes `"%d"` a `PrintfFormat`; `1.0<kg>` makes the
//! `1.0` measured; an `op_Implicit` target makes `"ab"B` a `ReadOnlySpan<byte>`.
//! Typing a literal by kind alone would therefore be *wrong* in those contexts —
//! a D5 soundness violation, the one thing this layer must never do.
//!
//! The sound observation is about **position, not kind**: a literal that is the
//! *immediate right-hand side of an unannotated, simple-name `let` binding* has
//! **no expected type reaching it** — the binding's type is whatever the literal
//! is. Nothing downstream can retarget it either: F# binds the literal's own
//! type at the `let`, so a later conflicting use is a *type error*, not a
//! retype (verified against FCS). In that position every literal keeps its
//! natural type, so typing it is sound — including the common unsuffixed
//! `let n = 42` / `let s = "hi"` that a kind-based rule would have to defer.
//!
//! Everything else is left absent — the D5 "Deferred, say nothing" contract:
//! literals in argument / annotated / measure / collection positions (where an
//! expected type may retarget them) and the few whose type is not fixed even in
//! isolation (`USER_NUM_LIT` like `1I`/`1G`, source-location identifiers). They
//! become typeable once expected-type propagation lands (Phase 3.2), which
//! generalises exactly this "what expected type reaches here" question.
//!
//! Pure (no IO): a binding-RHS literal's type is syntactically determined.
//! Differentially tested against FCS's typed tree
//! (`crates/sema/tests/all/infer_literals_diff.rs`), which exercises both the sound
//! positions (must agree) and the unsound ones (must stay silent).
//!
//! **Stage 3.2a — the generate→solve pipeline.** Inference is structured as
//! the plan's [D8] generate→solve: a pure fold produces an inert
//! `Vec<Constraint>` over inference variables, the [`InferTable`] substrate
//! solves them by unification, and each expression's type is read back by
//! resolving its variable — emitted only when it is fully [`ground`](Ty::is_ground)
//! (D5: silence otherwise).
//!
//! **Stage 3.2b-1 — value-reference propagation.** Generation now consults name
//! resolution ([`crate::resolve_file`]) to connect a value *use* to its binder:
//! each in-file binder gets an inference variable keyed by its `DefId`, and a
//! sound value binding `let y = <rhs>` unifies that variable with its RHS. The
//! RHS may itself be a value use (`let y = x`), so a binder's type propagates
//! down a `let z = y` chain.
//!
//! A use's type is emitted **only in that bare-`let`-RHS position** — the same
//! coercion-free position [`literal_ty`] relies on. Elsewhere (an argument, an
//! annotated binding, an `if`/`match` arm) F# may insert a *subsumption
//! coercion* — `s : string` flowing into an `obj` slot has expression type
//! `obj`, not `string` — so the use's type is **not** its binder's, and emitting
//! the binder's type there would violate D5. Those positions defer until
//! expected-type propagation lands. Function bindings (generic, needing
//! instantiation) and binders we never typed also stay Deferred.
//!
//! **Stage 3.2b-2 — parentheses and tuples.** The coercion-free RHS typer
//! recurses through transparent parentheses (`let y = (x)`) and types a
//! reference tuple `(a, b, …)` as [`Ty::Tuple`], recursing into each element
//! (still coercion-free — an unannotated tuple imposes no expected type on its
//! elements). A tuple-bound value hovers as `int * string`. Struct tuples and an
//! element we cannot type are deferred (the latter leaves the tuple un-ground
//! while its typeable siblings still emit).
//!
//! **Stage 3.2c-1 — the bidirectional spine + control flow.** RHS typing
//! becomes a recursive [`Gen::infer_expr`] threading an `expected` type
//! (bidirectional checking): `None` is *synth* mode — a coercion-free position
//! where the synthesized type *is* the elaborated one, so the node is emitted —
//! and `Some(_)` is *check* mode — a coercion-possible position where the node is
//! **not** emitted, since the elaborated type may be a coercion this stage
//! doesn't model (no subtype relation yet). Check mode only suppresses emission
//! and propagates the mode to children; it deliberately adds no cross-constraint,
//! which without a subtype relation could only force equality and back-flow a
//! coerced type onto a binder. This is the permanent HM shape: function
//! application (a later slice) checks arguments against parameter types the same
//! way, and subtype-aware checking later *completes* the check-mode rule (relate
//! by subtyping, emit the coerced type) without replacing it. The first construct
//! it unlocks is `if`/`then`/`else`: in synth mode the result is the then-branch's
//! synthesized type (and the whole `if` defers if that branch can't synthesize —
//! its type must not be taken from the coercible else), the else-branch is checked
//! against it; an `if` with no *final* `else` (a plain `if c then a` or an `elif`
//! chain that never terminates in `else`) results in `unit`, not the then-branch
//! type, so it too defers. In check mode both branches are checked. The result
//! flows to the binder and hover (`let r = if … then 1 else 2` ⇒ `int`).
//!
//! **Stage 3.2c-2a — function/lambda body traversal.** Generation now walks the
//! **body** of a function binding (`let f x … = body`), a lambda
//! (`fun x … -> body`), and a `while` loop, so their sub-expressions get typed —
//! e.g. a function body that is an `if` emits its result type
//! (`let f c = if c then 1 else 2` ⇒ the `if` and the `1` type as `int`). Each
//! body carries the right bidirectional *mode*: a function-binding body and a
//! synth-position lambda body are coercion-free (synth); a lambda reached in
//! *check* mode passes it on (an expected function type could retarget the body);
//! a `while` body is checked against `unit`. The function value itself stays
//! Deferred: its `Ty::Fun` type (and let-generalisation) are a later slice — this
//! one lays the body-traversal groundwork they build on.
//!
//! **Stage 3.2c-2b — `Ty::Fun` + monomorphic function types + condition typing.**
//! A `let`-function binder now carries a [`Ty::Fun`] type built by currying its
//! parameter variables over the body's return variable
//! ([`Gen::function_type`]) — emitted (on [`InferredFile::def_type`]) only when
//! every operand is [`ground`](Ty::is_ground), i.e. **monomorphic** functions
//! (`let f c = if c then 1 else 2` ⇒ `f : bool -> int`). A polymorphic function
//! (an un-grounded parameter or return) leaves an open variable and so silently
//! defers until let-generalisation (3.2c-2c).
//!
//! This is where **condition typing** returns, done soundly. A parameter is
//! grounded by a real constraint, and the one this stage supplies is the
//! `if`/`while` **condition** ([`Gen::constrain_bool`]): a condition admits *no*
//! subsumption — F# requires it to be exactly `bool` — so unifying it with
//! `bool` is a genuine equality, not the coercion-possible check of an `if`
//! branch. `let f c = if c then …` thus makes `c : bool` *inside* the function
//! type.
//!
//! The subtlety is that condition typing is one *modelled* constraint among
//! possibly-**unmodelled** others: a parameter can also carry a use-site
//! ascription (`(c : int)`) or an annotation this stage does not read, and on
//! ill-typed mid-edit code F# resolves the conflict differently (keeping `int`,
//! reporting the condition error). So the condition-derived `bool` must not reach
//! any place we publish a parameter's *own* type. The mechanism:
//! [`Gen::param_var`] gives each simple named parameter a **private slot**
//! variable that is the parameter's type *inside* the function's `Ty::Fun`, and
//! condition typing grounds **that slot** ([`Gen::param_slots`]), *not* the
//! parameter's binder [`def_var`](Gen::def_var). The binder's variable — which
//! its expression uses and `def_type` read off — is therefore never grounded by a
//! condition, so `bool` flows *only* into the function signature and never into a
//! standalone parameter read-off (D5). Any non-simple parameter (annotated,
//! tupled, wildcard, unit) gets no slot, so it never grounds and the function
//! defers.
//!
//! Publishing only the function type is provably sound: a ground `Ty::Fun` means
//! every parameter slot is ground, and any unmodelled constraint on a parameter
//! would have blocked the function's groundness (so it would have deferred) —
//! hence a ground function matches FCS, while still surfacing the parameter types
//! *within* the signature (`f : bool -> int`).
//!
//! **Stage 3.2c-2c — `let`-generalisation, instantiation, and typar rendering.**
//! A function binding whose type is still *open* after solving may be
//! **generalised** — `let f x = x` ⇒ `f : 'a -> 'a` — with each use instantiating
//! the scheme afresh. This is the genuinely hard HM part, and its soundness is
//! **asymmetric** with everything above: every constraint generation emits is a
//! *true equality in FCS's derivation too*, so our constraint set is a **subset**
//! of FCS's. Grounding under a subset stays sound (a superset of consistent
//! equalities cannot change a *determined* value). But *openness* under our subset
//! is **not** a subset property: a variable open for us may be ground for FCS
//! through a constraint we do not model (`let f x = x + 1` has `x` open only
//! because `+` is unmodelled — FCS grounds it to `int`). Emitting `'a -> 'b` there
//! would be a wrong published type (a D5 violation). So a variable may be
//! quantified only when we can prove **every constraint FCS has on it is one we
//! modelled** — which needs new machinery:
//!
//! - **Per-binding walk-completeness** ([`Gen::complete`]). During one binding's
//!   generation, any sub-expression or pattern we do not fully model marks the
//!   binding *incomplete* (every `None`-return arm of [`Gen::infer_expr_inner`], a
//!   deferred literal, an unresolved name, a lambda, a struct/degenerate tuple, an
//!   `if` with no final `else`, a non-simple parameter, and a condition shape
//!   [`Gen::constrain_bool`] does not fully model — a compound `x && y` drops FCS
//!   constraints on `x`/`y`). Incomplete ⇒ **no generalisation** (ground emission
//!   is unaffected — the subset argument needs no completeness).
//! - **Poisoned variables** ([`Gen::poison`]). Check mode deliberately drops the
//!   relation between a checked expression and its expected type (no subtype
//!   relation yet); that dropped relation could ground *either* endpoint in FCS
//!   (`let f x = if true then x else 1` unifies `x` with `int` through the
//!   else-relation we drop). So [`Gen::infer_expr`] poisons the expected and
//!   returned variables of every check-mode call; at generalisation the set is
//!   closed one step (resolve each, poison every variable in the resolved term). A
//!   poisoned variable is never generalised (but *poisoned-and-ground* is fine —
//!   the subset argument again).
//!
//! On a **complete** function binding the 2b private-slot decoupling is undone:
//! [`Gen::slot_binder_reunify`] emits `Eq(slot, binder_var)` per simple parameter
//! (F# has one variable for the parameter — the decoupling only exists to fence
//! off *unmodelled* constraints, and every unmodelled shape already sets
//! incomplete). A parameter's binder var can then be ground (or a `Param`), so the
//! "params are never published standalone" invariant becomes **explicit**:
//! [`Gen::finish`] skips [`Gen::param_defs`] in `def_types`. Parameter uses in
//! synth positions may now emit (`let f x = if x then (1, x) else (2, x)` types the
//! tuple's `x` as `bool`), sound on a complete binding.
//!
//! Generalisation is **sequential per binding** (Algorithm W): [`Gen::let_binding`]
//! drains and solves each binding's constraints before the next, so a later use
//! sees the earlier binding's [scheme](Gen::def_schemes). A **vid mark**
//! ([`InferTable::mark`]) taken at binding start makes the environment-freeness
//! check trivial (an open var inherited from an earlier binder has a smaller index,
//! so it defers this function rather than being quantified). At a function
//! binding's finalisation: resolve its type; if ground, emit as before; else
//! generalise iff complete **and** every open var is created-this-binding and
//! unpoisoned — replacing those vars by [`Ty::Param`]s numbered by first
//! appearance (DFS, argument-before-return) — else defer. A use of a scheme'd
//! binder ([`Gen::infer_expr_inner`]'s [`Expr::Ident`] arm) instantiates with a
//! fresh var per distinct `Param`.
//!
//! **Stage 3.2c-3 — function application (v1: no worklist).** `f x` becomes a
//! modelled construct ([`Gen::infer_app`]): the **function position** is
//! synthesized ([`Gen::infer_callee`]) but records **no** node — FCS emits a typed
//! node only at the whole application, never at the bare function-position use
//! (probed against the `types` oracle) — then fresh domain `d` and result `r`
//! variables are created and `Eq(tf, Fun(d, r))` is pushed. That equality is
//! **genuinely true**: applying an in-file value forces a function shape (an
//! in-file binder cannot be a method group). The argument is walked in **check
//! mode** against `d`, so [`Gen::infer_expr`]'s poison wrapper poisons `d` and the
//! argument's variable — F# relates them by *subsumption*, which this stage drops,
//! and that poison is the soundness story. The `App` node emits `r` in synth
//! positions (D5-safe: `r` is fixed by the `Eq` against the function's *own* type,
//! independent of how the argument coerces). Curried `f x y` falls out of nested
//! `App`s; **infix** application, a **bracket indexer** (`arr[i]`, which the parser
//! stores under `APP_EXPR` but F# lowers to a member lookup, not an application),
//! `TypeApp`, and applied `DotGet` stay unmodelled ⇒ incomplete. The application
//! result `r` is also **poisoned** unconditionally: a `ground` `r` is unaffected
//! (poison bites only open vars, so every ground payoff still emits), but a
//! still-*open* `r` — a polymorphic result, or a result whose shape `Eq` could not
//! unify (a value applied as `n 3`, whose failed constraint rolls back leaving `r`
//! unrelated) — never generalises, so nothing bogus is published.
//!
//! Payoffs and gaps, both by construction: a ground `f : bool -> int` applied to
//! any argument grounds `r` (a ground var is unaffected by poison), so
//! `let n = f true` ⇒ `n : int` and a partial `let g = add true` ⇒
//! `g : bool -> int`. A **polymorphic** call defers: `let n = id 42` has
//! `id`'s scheme instantiate to `d = r`, both poisoned by the argument check, so
//! `r` stays open → silence (FCS grounds `n : int`; we say nothing rather than risk
//! a wrong type). Likewise `let g y = id y` cannot generalise (`d`/`r`/`y`
//! poisoned). Closing the polymorphic gap is Stage 3.3's suspended coercion-free
//! wake rule.
//!
//! **Stage 3.3a — the suspended `HasMember` worklist (fields/properties).** A
//! member access `recv.Name` where `Name` is a **field or non-indexer instance
//! property** on a **non-generic [`Ty::Named`]** receiver resolved through the
//! [`AssemblyEnv`] now types: `let s = "hi"` then `let n = s.Length` ⇒ `n : int`,
//! `let n = "hi".Length` ⇒ `int`, and chains / multi-dot access.
//!
//! - **Two parse shapes** (probed against `fcs-dump types` and the CST first):
//!   `s.Length` (a value receiver) is a **`LONG_IDENT_EXPR`** whose head token
//!   resolves to an in-file value binder and whose trailing segments are the
//!   members ([`Gen::infer_long_ident_member`]); `"hi".Length` / `(expr).Length`
//!   are a **`DotGet`** = a receiver *expression* + a member path
//!   ([`Gen::infer_dot_get`]). FCS emits a node at the *whole* access (the member
//!   result) **and** at the receiver (its own value), never at the bare `.Name` —
//!   so generation emits the receiver at its own range (synth: a member access
//!   never coerces its receiver) and the whole access at the node range.
//! - **The worklist.** `Constraint` grew a suspended
//!   [`Constraint::HasMember`] `{ recv, name, result }`. [`Gen::solve`] discharges
//!   the eager `Eq`s, then fixpoints: wake each parked member whose `recv`
//!   resolves to a concrete head ([`Gen::wake_member`]), discharge the
//!   `Eq(result, member_ty)` a wake produces, repeat until none fires (each fires
//!   at most once; a wake can ground a later chained member's receiver). A
//!   `Named` head is looked up; an `Array`/`Tuple`/`Fun`/`Param` head is dropped
//!   (arrays' `.Length` is intrinsic, out of scope); an unresolved receiver stays
//!   parked and is dropped with the batch.
//! - **Lookup + bridge.** [`AssemblyEnv::lookup_type`] (the receiver type, arity 0)
//!   then `AssemblyEnv::instance_data_member_ty` (a *single unambiguous* public
//!   instance field / non-indexer property, resolved across the receiver's base
//!   chain — a property + method group of the same name, an indexer, a method, an
//!   event, or a static all defer), bridged to a [`Ty`] by [`crate::member_ty`] (primitives,
//!   non-generic non-nested named types, plain vectors; everything else, incl.
//!   generic member types, defers). Any miss is silence (D5), never an error.
//! - **Poison.** As a *modelled* construct `HasMember` does **not** clear
//!   walk-completeness; instead [`Gen::gen_member_access`] poisons `recv` **and**
//!   `result` unconditionally (application's `r`-poison pattern) — an unwoken
//!   member relation must never let either var generalise (`let f x = (x.Foo, x)`
//!   defers, not a bogus `'a -> 'b * 'a`), while a ground emission is unaffected.
//! - [`infer_file`] gained an [`AssemblyEnv`] parameter (pure — values in). The
//!   application wake rule is Stage 3.3c.
//!
//! **Stage 3.3b — member-resolution side-table.** On a successful `HasMember`
//! wake ([`Gen::wake_member`]) the resolved member's identity is recorded at the
//! member-name use range in [`InferredFile::member_resolutions`], in the
//! resolver's own [`Resolution::Member`] `{ parent, idx }` shape (so every LSP
//! rendering / navigation path is uniform). The `use_range` is threaded onto
//! [`Constraint::HasMember`] at generation ([`Gen::gen_member_access`], one per
//! member segment) and is inert until a wake succeeds — recorded only from a
//! single-candidate discharge, never a guess (D5). The LSP layers this over the
//! resolver's `Deferred(QualifiedAccess)` at a member-name range for hover /
//! go-to-definition; dot-completion is served from the receiver's inferred type.
//!
//! **Stage 3.3c — the application wake rule (suspended arg↔param).** The
//! worklist's second client, closing 3.2c-3's polymorphic gap: `let g y = id y` ⇒
//! `g : 'a -> 'a`, `let n = id 42` ⇒ `n : int`, `let h y = fb y` (in-file
//! `fb : bool -> int`) ⇒ `h : bool -> int`, and chained `let c x = id (id x)` ⇒
//! `'a -> 'a`.
//!
//! 3.2c-3 dropped the argument↔parameter subsumption by *eagerly poisoning* the
//! domain `d` and the argument's variable, so a polymorphic application could
//! never generalise. 3.3c replaces that eager poison with a **suspended**
//! [`Constraint::ArgCheck`] `{ arg, dom, r }` generated at each modelled
//! application (the argument walk no longer takes the [`Gen::infer_expr`] poison
//! wrapper — [`Gen::infer_arg`] opts the application path out; every *other* check
//! site is unchanged), and the eager `d`/`r` poison is gone.
//!
//! **The wake rule** runs in the same per-binding fixpoint as `HasMember`
//! ([`Gen::solve`]). An `ArgCheck` discharges as a genuine `Eq(arg, dom)` (via
//! `unify_atomic`) iff **both**:
//! - the **enclosing binding is walk-complete** — the soundness gate. On a
//!   complete binding every constraint FCS has on the argument's variables among
//!   the shapes we model is present (the same subset argument that justifies the
//!   slot=binder reunification), so grounding the relation cannot disagree with
//!   FCS. Anything unmodelled — an annotation, an ascription, a compound
//!   condition — sets the binding *incomplete* and so protects the relation; and
//! - `dom` resolves to a **no-subsumption** type ([`Gen::no_subsumption_domain`]):
//!   a sealed BCL primitive (the set [`literal_ty`] produces — via
//!   [`is_sealed_primitive`]), a tuple of such (recursively), or an unbound root
//!   that is a **scheme-instantiation variable of ours** (a provenance set
//!   [`Gen::scheme_inst_vars`] populated by [`Gen::instantiate`], membership
//!   checked against union-find roots). `obj`, arbitrary named types, arrays, and
//!   [`Ty::Fun`] are excluded — F# admits a coercion there, so equality would be
//!   wrong (e.g. `let g y = fo y` with `fo : obj -> int` keeps `g : 'a -> int` in
//!   FCS, so we defer rather than emit `obj -> int`).
//!
//! **Deferred poisoning** replaces the "un-poison" idea (never remove poison, just
//! don't add it). At binding finalisation (in [`Gen::solve`], before the poison
//! closure / generalisation) every `ArgCheck` that did **not** successfully
//! discharge — never woken, gate failed, or the `Eq` itself failed on ill-typed
//! code (the atomic rollback leaves no trace) — poisons its `arg`, `dom`, **and**
//! the result `r`. A successful discharge poisons nothing (the relation is now
//! modelled); a ground result still emits (poison bites only open vars). This
//! keeps the 3.2c-3 guards intact: a failed-shape `n 3` still poisons its dangling
//! `r`, and a polymorphic result that never wakes still defers.
//!
//! **The soundness hazard this design closes.** *Unconditional* discharge would
//! be a D5 violation on ill-typed mid-edit code: `let h (y: string) = (y, f y)`
//! with `f : int -> int` would ground `y := int` through the wake (the annotation
//! is unmodelled), and `y`'s synth-mode tuple use would emit `int` where FCS keeps
//! `string`. The completeness gate blocks it: the annotated parameter `(y:
//! string)` marks the binding incomplete, so the `ArgCheck` never discharges and
//! `y` stays open (silent). Argument-node *emission* stays off (a check-mode arg
//! records no node even when its relation discharges); conditional read-off of
//! check-mode nodes is a separate follow-up.
//!
//! **Stage 3.3d — single-candidate method-call typing.** A method call
//! `recv.Method(args)` — the ~73 % of the census Member bucket 3.3a (fields /
//! properties, ~26 %) left deferred — types as the method's **return** type:
//! `let h = s.GetHashCode()` ⇒ `int`, `let t = s.GetType()` ⇒ `System.Type`.
//!
//! - **Parse shape.** A method call is an **`App` whose callee is a member access**
//!   (`APP_EXPR[ LONG_IDENT_EXPR[s.M] / DOT_GET_EXPR["hi".M], arg ]`), and FCS emits
//!   its `call:instance` node at the *whole* application, typed as the return type,
//!   with the receiver its own node. [`Gen::infer_callee`] never modelled such a
//!   callee (it always deferred), so [`Gen::infer_app`] routes a member-access
//!   callee to [`Gen::infer_method_call`] — pure extra coverage, no emission change.
//! - **The worklist, extended by *kind*.** [`Constraint::HasMember`] grew a
//!   [`MemberAccessKind`] (`Data` | `Method { arg_count }`); the wake dispatches the
//!   lookup on it — [`AssemblyEnv::instance_data_member`] vs
//!   [`AssemblyEnv::instance_method`] (a **single non-overloaded, non-generic**
//!   public instance method) — and the rest of the wake (bridge the return type,
//!   record the member, unify the result) is shared. Overloaded (`s.Substring(1)` —
//!   FCS `call:instance-overloaded`), generic, static, extension, and constructor
//!   methods defer.
//! - **Well-formedness gate.** The return type is the call's type only when the
//!   argument list is well-formed: the wake gates on the call's positional
//!   `arg_count` ([`method_arg_count`]) being `Some(param_count)` exactly. FCS does
//!   *not* type an ill-formed call as the method return — it falls back to `obj`
//!   (`call:function`) — so each of these defers fully (no type, no recorded
//!   resolution): a wrong **arity** (`s.ToLowerInvariant(1)`, `s.Insert()`), a
//!   **parenthesized unit** (`s.ToLowerInvariant(())` is one unit argument, not
//!   zero), a **named argument** (`s.Insert(foo = 0, …)` — [`is_named_arg`], names
//!   unvalidated here), and an extra-parenthesized tuple (`s.Insert((0, "z"))`, which
//!   FCS elaborates as a method-value `application`, so only the one call-parenthesis
//!   layer is peeled and it counts as a single argument). Conservative on
//!   `[<Optional>]` / `params` / named-argument methods (a later slice), never wrong.
//! - **Void returns defer.** A `void`-returning method's call type is F# `unit`
//!   (unmodelled), so [`Gen::wake_member`] records the member's identity but skips
//!   the type unify when the return `TypeRef` is [`Primitive::Void`] — bridging it
//!   would emit the wrong `System.Void`.
//! - **A method call emits no node inside itself** (`gen_member_access`'s receiver,
//!   the argument's sub-expressions). FCS lowers a rejected / ill-formed call to a
//!   single node and emits nothing inside it, and a receiver's `result` can even be
//!   grounded by a *surrounding* constraint (`f (s.ToLowerInvariant(1))` grounds the
//!   bad call to `f`'s domain), so gating a receiver on its *own* groundness is
//!   unsound. [`Gen::infer_method_call`] therefore **discards** every emission its
//!   receiver / argument walk produces — the receiver's *type* still flows to the
//!   wake (its hover comes from name resolution); only its expression-*node* is
//!   dropped. The whole-call node (emitted by the caller) is read back only when
//!   ground, and a rejected call's `result` stays open in synth position (and a
//!   check-position call emits no whole-call node), so it drops naturally.
//! - **The argument** is walked in check mode (poisoning any generalisable parameter
//!   use — a method arg↔param subsumption we drop and never discharge, since F# does
//!   not generalise a method argument); a **unit** `()` argument is skipped (no
//!   parameters, and modelled — so it does not mark the binding incomplete).
//! - **Chaining is free.** A method result feeding a further access
//!   (`s.ToLowerInvariant().Length`) resolves through the same fixpoint — the inner
//!   call grounds the receiver, then the outer member wakes.
//! - **LSP.** The `Method` wake records the method in `member_resolutions` exactly
//!   as `Data` does, so 3.3b's hover / go-to-def path serves a called method name
//!   with no LSP change.
//!
//! [D8]: ../../../docs/type-checker-plan.md

use std::collections::{HashMap, HashSet};

use borzoi_assembly::{EntityKind, Primitive, TypeRef};
use borzoi_cst::syntax::{
    AppExpr, AstNode, Binding, ConstExpr, DotGetExpr, Expr, IfThenElseExpr, ImplFile, LetDecl,
    LongIdentExpr, LongIdentPat, NamedPat, ParenPat, Pat, SyntaxKind, SyntaxNode, SyntaxToken,
    TupleSegment, Type,
};
use rowan::TextRange;

use crate::assembly_env::{AssemblyEnv, EntityHandle};
use crate::def::DefId;
use crate::member_ty::type_ref_to_ty;
use crate::overload::arity_window;
use crate::resolve::{Resolution, ResolvedFile};
use crate::ty::{Ty, TyVid};
use crate::unify::InferTable;

/// Per-file inference result. Two views, both populated only when a type can be
/// assigned *soundly* (absence means "Deferred", D5 — the consumer shows
/// nothing):
///
/// - [`types`](Self::types): each *expression*'s source range → its inferred
///   type (a literal, or a value use in a coercion-free position). The
///   expression-node view — the type *at this spot* — which a hover on a bare
///   literal uses.
/// - [`def_type`](Self::def_type): each *binder*'s [`DefId`] → its inferred
///   type. The symbol view — the type *of this value* — which a hover on a
///   resolved name uses, and which (unlike an expression-node type) is the same
///   at every occurrence of the binder, so it is unaffected by subsumption
///   coercions at any individual use site.
#[derive(Debug, Default)]
pub struct InferredFile {
    types: HashMap<TextRange, Ty>,
    def_types: HashMap<DefId, Ty>,
    /// Each resolved **member access** keyed by the member-name use range → the
    /// member's identity, in the resolver's own [`Resolution::Member`] shape
    /// (Stage 3.3b). Populated only on a successful, single-candidate `HasMember`
    /// wake ([`Gen::wake_member`]) — never from a guess — so the LSP can layer it
    /// over the resolver's `Deferred(QualifiedAccess)` at a member-name range and
    /// serve hover / go-to-definition identically to a resolver-resolved member
    /// (`System.Console.WriteLine`). Absent means "no sound answer" (D5).
    member_resolutions: HashMap<TextRange, Resolution>,
}

impl InferredFile {
    /// The inferred type of the expression at `range`, if one was assigned.
    pub fn type_at(&self, range: TextRange) -> Option<&Ty> {
        self.types.get(&range)
    }

    /// All `(range, type)` pairs this file inferred.
    pub fn types(&self) -> &HashMap<TextRange, Ty> {
        &self.types
    }

    /// The inferred type of the binder `def`, if one was assigned — the type of
    /// the value itself, valid at every occurrence (definition and uses alike).
    pub fn def_type(&self, def: DefId) -> Option<&Ty> {
        self.def_types.get(&def)
    }

    /// All `(binder, type)` pairs this file inferred — the symbol view, each
    /// keyed by its [`DefId`] (whose defining range is
    /// [`ResolvedFile::def`](crate::ResolvedFile::def)`(id).range`). Used by the
    /// binder-type differential.
    pub fn def_types(&self) -> &HashMap<DefId, Ty> {
        &self.def_types
    }

    /// The member a member-access resolved to at the member-name `range`, if a
    /// `HasMember` wake produced one (Stage 3.3b). Always a
    /// [`Resolution::Member`] — the same shape the resolver records for a static
    /// path — so the LSP can hand it to the identical hover / go-to-definition
    /// path. `None` when nothing was resolved at that range (deferred / ambiguous
    /// / open receiver): D5 silence.
    pub fn member_resolution_at(&self, range: TextRange) -> Option<Resolution> {
        self.member_resolutions.get(&range).copied()
    }

    /// All `(member-name range, member resolution)` pairs inference resolved — the
    /// side-table the LSP layers over the resolver's `Deferred(QualifiedAccess)`
    /// records (Stage 3.3b).
    pub fn member_resolutions(&self) -> &HashMap<TextRange, Resolution> {
        &self.member_resolutions
    }

    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

/// A type constraint produced by generation and discharged by the solver.
/// `Eq` (type equality) is the eager equality; `HasMember` is the **suspended**
/// member constraint added in Stage 3.3a, which the [`Gen::solve`] loop wakes
/// when its receiver's head becomes concrete (turning the straight-line loop
/// into a worklist). Neither carries an origin span yet — those arrive with
/// Phase 4 diagnostics, the first consumer that needs them.
#[derive(Debug)]
enum Constraint {
    /// The two types must be equal.
    Eq(Ty, Ty),
    /// The receiver `recv` has an instance member named `name` — of [`kind`] a data
    /// member (field / non-indexer property, Stage 3.3a) *or* a method (a
    /// single-candidate method **call**, Stage 3.3d) — whose type is `result`.
    /// **Suspended**: it discharges only once `recv` resolves to a concrete
    /// [`Ty::Named`] head, at which point the member is looked up against the
    /// [`AssemblyEnv`] (dispatched on `kind`: `instance_data_member` vs
    /// `instance_method`) and its type — a data member's type, or a method's
    /// **return** type — bridged to a [`Ty`], pushing `Eq(result, member_ty)`. A
    /// failed / ambiguous / unmodelled lookup silently drops the constraint (D5:
    /// `result` stays open, so the access defers). Each fires at most once.
    ///
    /// `use_range` is the member-name token's source range (Stage 3.3b): on a
    /// successful wake the resolved member's identity is recorded there in
    /// [`InferredFile::member_resolutions`], so the LSP can surface hover /
    /// go-to-definition on the member name. It is inert until a wake succeeds.
    ///
    /// [`kind`]: MemberAccessKind
    HasMember {
        recv: TyVid,
        name: String,
        result: TyVid,
        use_range: TextRange,
        kind: MemberAccessKind,
    },
    /// A **suspended application argument↔parameter check** (Stage 3.3c): at a
    /// modelled application `f x`, the argument variable `arg` and the function's
    /// domain variable `dom` are related by F#'s subsumption, which this stage
    /// drops. Rather than eagerly poison both (the 3.2c-3 rule), the relation is
    /// suspended: the [`Gen::solve`] fixpoint wakes it iff the enclosing binding is
    /// walk-complete **and** `dom` resolves to a **no-subsumption** type
    /// ([`Gen::no_subsumption_domain`]) — a sealed BCL primitive, a tuple of such,
    /// or an unbound scheme-instantiation root — at which point it discharges as a
    /// genuine `Eq(arg, dom)` (via `unify_atomic`). An `ArgCheck` that never wakes,
    /// fails the gate, or whose `Eq` itself fails (ill-typed code) is
    /// **undischarged**: [`Gen::solve`] then poisons `arg`, `dom`, and the
    /// application result `r` (deferred poison), so nothing bogus generalises. `r`
    /// is carried only to drive that deferred poison; the discharge reads only
    /// `arg`/`dom`.
    ArgCheck { arg: TyVid, dom: TyVid, r: TyVid },
}

/// A pending application argument↔parameter check inside [`Gen::solve`] (Stage
/// 3.3c): the argument variable `arg`, the function domain `dom`, and the
/// application result `r` (carried only for the deferred poison). The suspended
/// form of [`Constraint::ArgCheck`], threaded through the solver's fixpoint.
#[derive(Debug, Clone, Copy)]
struct ArgCheck {
    arg: TyVid,
    dom: TyVid,
    r: TyVid,
}

/// Which kind of member a suspended [`Constraint::HasMember`] resolves to — a
/// **data** member (field / non-indexer property, Stage 3.3a) or a **method** (a
/// single-candidate instance method call, Stage 3.3d). It selects only the
/// assembly lookup the wake runs; everything else the wake does (bridge the type,
/// record the member's identity at `use_range`, unify the result) is **shared**.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MemberAccessKind {
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

/// A pending member access inside [`Gen::solve`] — the suspended form of
/// [`Constraint::HasMember`], threaded through the solver's fixpoint.
#[derive(Debug, Clone)]
struct SuspendedMember {
    recv: TyVid,
    name: String,
    result: TyVid,
    use_range: TextRange,
    kind: MemberAccessKind,
}

/// Infer best-effort types for `file`, given its name resolution `resolved` and
/// the project's `env` (the flattened referenced-assembly view). Types each
/// literal in a soundness-safe position (the immediate RHS of an unannotated,
/// simple-name `let`), each *use* of an in-file binder whose type that propagates
/// to, and (Stage 3.3a) a member access `recv.Name` whose receiver is a
/// non-generic assembly type with a single public instance field / non-indexer
/// property named `Name`; leaves all other expressions absent (Deferred). Pure:
/// a function of the parsed file, its resolution, and the assembly env (values
/// in — the env is read, never mutated).
///
/// Structured as generate→solve (the plan's D8): a pure `walk` fold builds an
/// inert constraint set over inference variables, `solve` discharges it — the
/// eager `Eq`s first, then a worklist waking each suspended `HasMember` once its
/// receiver's head is concrete — and each recorded expression's type is read back
/// from its variable, emitted only when fully [`ground`](Ty::is_ground).
pub fn infer_file(file: &ImplFile, resolved: &ResolvedFile, env: &AssemblyEnv) -> InferredFile {
    let ext_scope = ExtensionScope::of(resolved, env);
    let mut cx = Gen::new(resolved, env, ext_scope);
    cx.walk(file);
    cx.solve();
    cx.finish()
}

/// The OV-6 overload engine's **extension-absence gate** (see
/// `docs/overload-resolution-plan.md` §4.1(4) and
/// `docs/extension-scope-enumeration-plan.md`). An overloaded call `recv.M(args)`
/// may commit an *intrinsic* winner only when no in-scope F#-style or C#-style
/// extension member **named `M`** could join FCS's method group (the P15 landmine:
/// an applicable extension can beat an applicable intrinsic on any betterness rule
/// before rule 10).
///
/// The gate keeps the OV-6 *source set* — which is complete by construction after
/// eight review rounds — but refines each source from a boolean ("any extension
/// source in scope ⇒ defer") to a **name test** ("an extension *named `M`* in
/// scope ⇒ defer"), because an in-scope extension competes only within its own
/// name's group (probed — the enumeration plan §1). The sources:
///
/// - **Assembly side — name-keyed (EX-1/EX-2).** The env's auto-open surfaces, the
///   file's in-scope namespace chain (the always-in-scope root `[]` plus each
///   declared namespace and its ancestors), and the file's explicit
///   `open <namespace>` targets ([`Self::in_scope_namespaces`], which folds both).
///   [`AssemblyEnv::extension_named_in_scope`] answers, keyed by the call shape
///   (`is_static` — a value receiver's group takes only *instance* extensions, a
///   type-qualified call's only *static* ones). An unenumerable surface there
///   (projection failure, a dropped type, a contested auto-open) still defers for
///   every name.
/// - **`open`s the resolver could not name-key (EX-2 ⇒ [`Self::opens_unknowable`]).**
///   A project module/namespace open (EX-3), an assembly-module or `open type`, or
///   an opaque / vetoed / dropped-path open — the resolver marks these and the gate
///   defers *every* call in the file, exactly as the pre-EX-2 presence gate did for
///   *any* `open`.
/// - **Project side — name-keyed where walkable ([`Self::augmentation_instance_names`] /
///   [`Self::augmentation_static_names`], EX-3 §2(a)/(b)), presence-based for the
///   rest ([`Self::project_source_present`]).** A `type … with` augmentation
///   defers exactly its member names (own-file and preceding Compile-order files
///   alike); an augmentation member the walk cannot name, or an attribute the
///   resolver cannot prove is no C#-style `[<Extension>]` (EX-3 §2(d)), still
///   defers wholesale — own-file or threaded from a preceding file
///   ([`ResolvedFile::preceding_declares_extension_source`]). An `[<AutoOpen>]`
///   module is no trigger of its own (AO-1): its extension-capable contents are
///   exactly those two signals, both collected file-globally.
///
/// A source whose names cannot be computed **stays a wholesale defer**, so every
/// refinement strictly *shrinks* the deferred set and can never license a commit a
/// full name enumeration would not. The [`Default`] scope (no source, no opened
/// namespace, nothing unknowable) is used by in-crate unit tests that drive [`Gen`]
/// without a file.
#[derive(Default)]
struct ExtensionScope {
    /// A **project** extension source we could not name-key is in scope: an
    /// augmentation member with an un-walkable name (own-file or preceding —
    /// EX-3 §2(a)/(b) thread the walkable ones as the sets below), or an
    /// attribute the resolver could not prove is no C#-style `[<Extension>]`
    /// (EX-3 §2(d)). Presence-based and name-blind: every method-call commit
    /// defers when set, whatever the name.
    project_source_present: bool,
    /// Some explicit `open` in the file brings an extension surface the resolver
    /// could not name-key (EX-2): a project module/namespace, an assembly module /
    /// `open type`, or an opaque / vetoed / dropped-path open. Presence-based like
    /// [`Self::project_source_present`] — every commit defers when set — and set
    /// straight from [`ResolvedFile::open_extension_unknowable`].
    opens_unknowable: bool,
    /// The file's **in-scope namespace chain plus its explicit `open <namespace>`
    /// targets**: the always-in-scope root `[]`, each declared namespace and its
    /// ancestors, and every assembly namespace an `open` brought in (EX-2,
    /// [`ResolvedFile::open_extension_namespaces`]). A referenced-assembly extension
    /// declared in any of these is in scope — an enclosing namespace needs no
    /// `open`, an `open`ed one is opened — so the assembly-side gate is asked the
    /// same query about all of them per call.
    in_scope_namespaces: Vec<Vec<String>>,
    /// The same-file augmentation member names, keyed by call shape (EX-3
    /// §2(a)): a `type … with` member joins **its own name's** call group, so
    /// only calls of these names defer. Wholesale unknowability (an
    /// un-walkable member name) folds into
    /// [`Self::project_source_present`] instead. Both sets empty in
    /// [`Default`], matching the no-source unit-test scope.
    augmentation_instance_names: std::collections::HashSet<String>,
    augmentation_static_names: std::collections::HashSet<String>,
}

impl ExtensionScope {
    fn of(resolved: &ResolvedFile, env: &AssemblyEnv) -> ExtensionScope {
        // The root must be included explicitly — `namespace_paths` omits it, and it
        // is always in scope (so `module M` sees a global `[<Extension>]`). The
        // file's explicit `open <namespace>` targets (EX-2) join it: opening a
        // namespace makes its extensions in-scope exactly as an enclosing namespace
        // does, so the same per-namespace query serves both.
        let in_scope_namespaces = std::iter::once(Vec::new())
            .chain(resolved.namespace_paths().iter().cloned())
            .chain(resolved.open_extension_namespaces().iter().cloned())
            .collect();
        let project_source_present =
            // A preceding Compile-order file's extension source the fold could
            // not name-key (its walkable augmentation names thread as the
            // `preceding_*` sets below instead).
            resolved.preceding_declares_extension_source()
            // This file's own declarations: an augmentation member whose NAME
            // could not be walked (EX-3 §2(a) — the name-keyed sets below
            // carry the walkable ones), or an attribute that **may declare an
            // extension** — EX-3 §2(d) stage 5: the old "any attribute"
            // trigger, refined through the resolver's per-attribute type
            // resolutions (differentially validated against FCS). An
            // attribute resolving to a concrete non-`ExtensionAttribute` type
            // provably marks nothing; one resolving TO the marker, or that
            // the resolver declined to pin, keeps the presence defer. An
            // `[<AutoOpen>]` module as such is NOT a trigger (AO-1): both its
            // extension-capable content kinds — augmentations and
            // `[<Extension>]` attributes — are collected file-globally by the
            // two walks above, nested modules included, so the old presence
            // defer was pure over-approximation. Explicit `open`s are not here
            // either — EX-2 routes each open's target to `in_scope_namespaces`
            // (name-keyed) or `opens_unknowable` (defer).
            || resolved.augmentation_names_unknowable()
            || resolved.attributes_may_declare_extension(env);
        // Own-file and preceding-file augmentation names key the same
        // per-name check (EX-3 §2(a)/(b)): a preceding same-namespace
        // augmentation is in scope with no `open`, exactly like an own-file
        // one, so the union is the file's project-side extension name set.
        let augmentation_instance_names = resolved
            .augmentation_instance_names()
            .union(resolved.preceding_augmentation_instance_names())
            .cloned()
            .collect();
        let augmentation_static_names = resolved
            .augmentation_static_names()
            .union(resolved.preceding_augmentation_static_names())
            .cloned()
            .collect();
        ExtensionScope {
            project_source_present,
            opens_unknowable: resolved.open_extension_unknowable(),
            in_scope_namespaces,
            augmentation_instance_names,
            augmentation_static_names,
        }
    }

    /// Whether an extension member **named `name`** is provably absent from the
    /// file's in-scope extension surface — the precondition for committing an
    /// intrinsic overload of that name (`is_static` selects the call shape, since
    /// FCS takes only *instance* extensions into a value receiver's group and only
    /// *static* ones into a type-qualified call's).
    ///
    /// **Name-keyed on the assembly side (EX-1), for explicit `open <namespace>`
    /// targets (EX-2), and for project augmentations (EX-3 §2(a)/(b));
    /// presence-based for what those walks could not name-key.** An in-scope
    /// extension competes only within its *own* name's group (probed —
    /// `docs/extension-scope-enumeration-plan.md` §1), so a surface that provably
    /// lacks `name` is no reason to defer; that conflation is what made every
    /// FSharp.Core-referencing project defer every overloaded call. The remaining
    /// name-blind triggers ([`Self::project_source_present`],
    /// [`Self::opens_unknowable`]) still defer wholesale — sound.
    fn absent(&self, env: &AssemblyEnv, name: &str, is_static: bool) -> bool {
        !self.project_source_present
            && !self.opens_unknowable
            // EX-3 §2(a): a same-file augmentation member joins its own
            // name's group only — the name-keyed project-side sibling of the
            // assembly query below.
            && !if is_static {
                self.augmentation_static_names.contains(name)
            } else {
                self.augmentation_instance_names.contains(name)
            }
            && !env.extension_named_in_scope(&self.in_scope_namespaces, name, is_static)
    }
}

/// Mutable state threaded through generation: the unification table, the inert
/// constraint set, the per-expression variables to read back, and the cache of
/// per-binder ([`DefId`]) variables that lets a value *use* unify with its
/// binder. Grouped into one value so the per-node rules read locally.
struct Gen<'a> {
    resolved: &'a ResolvedFile,
    /// The flattened referenced-assembly view a member access resolves *into*
    /// (Stage 3.3a): the receiver's [`Ty::Named`] head is looked up here for its
    /// instance data member. Read-only (pure — values in).
    env: &'a AssemblyEnv,
    table: InferTable,
    constraints: Vec<Constraint>,
    /// `(range, var)` per expression whose type to emit; `range` is the read-off
    /// key (and the same range the resolver keyed the occurrence under).
    exprs: Vec<(TextRange, TyVid)>,
    /// The inference variable standing for each referenced in-file binder.
    def_vars: HashMap<DefId, TyVid>,
    /// The **private function-type slot** variable for each simple named
    /// parameter of a `let`-function head ([`Self::param_var`]): the parameter's
    /// type *inside* the function's [`Ty::Fun`], and the only variable
    /// [`Self::constrain_bool`] grounds when that parameter is used as a
    /// condition. It is deliberately **separate** from the parameter binder's
    /// [`def_var`](Self::def_var) — which its expression uses and `def_type` read
    /// off — so the condition-derived `bool` flows *only* into the function type,
    /// never into a standalone parameter read-off where an unmodelled constraint
    /// (a use-site ascription, an annotation) could make it wrong on ill-typed
    /// input (D5). Keyed by the parameter's `DefId`, so a condition referencing
    /// that parameter finds its slot. On a *complete* binding the slot is
    /// reunified with the binder var ([`Self::slot_binder_reunify`]).
    param_slots: HashMap<DefId, TyVid>,
    /// Every parameter `DefId` collected during [`Self::param_var`]. A parameter's
    /// type is never published standalone (D5): [`Self::finish`] skips these in
    /// `def_types`. Before 3.2c-2c this was *emergent* (a parameter's binder var
    /// was never ground); the slot=binder reunification on complete bindings can
    /// now ground it, so the exclusion is made **explicit**.
    param_defs: HashSet<DefId>,
    /// Generalised binders' schemes, keyed by `DefId` (Stage 3.2c-2c). The scheme
    /// body has open variables replaced by [`Ty::Param`]s (numbered by first
    /// appearance); each use [instantiates](Self::instantiate) it with fresh
    /// variables. Computed at a function binding's finalisation, consulted by a
    /// later binding's use — so it is filled sequentially, in document order.
    def_schemes: HashMap<DefId, Ty>,
    /// **Per-binding** walk-completeness (Stage 3.2c-2c). Reset to `true` at each
    /// binding's start ([`Self::let_binding`]); cleared by any unmodelled
    /// sub-expression or pattern. A binding may generalise only if it stays
    /// complete — the proof that every constraint FCS has on its open variables is
    /// one we modelled. Ground emission is *not* gated on this (the subset
    /// argument in the module docs).
    complete: bool,
    /// **Per-binding** poison set (Stage 3.2c-2c): the raw variables of every
    /// check-mode [`Self::infer_expr`] call (the expected and the returned var),
    /// whose cross-relation this stage drops. Reset per binding. At generalisation
    /// its one-step closure (resolve each, take every var in the resolved term)
    /// gives the variables that may *not* be quantified — FCS's dropped relation
    /// could have grounded them.
    poison: Vec<TyVid>,
    /// The simple named parameters of the **current** binding as `(DefId, slot)`
    /// pairs, in head order (Stage 3.2c-2c). Reset per binding; drives the
    /// slot=binder reunification on a complete function binding.
    cur_params: Vec<(DefId, TyVid)>,
    /// Each successfully-woken member access → its resolved identity, keyed by the
    /// member-name use range (Stage 3.3b). Filled by [`Self::wake_member`] on a
    /// single-candidate discharge; read out in [`Self::finish`] into
    /// [`InferredFile::member_resolutions`].
    member_resolutions: HashMap<TextRange, Resolution>,
    /// The vid mark taken at the **current** binding's start — the boundary
    /// between this binding's own variables and *environment* variables
    /// inherited from earlier bindings. Generalisation already consults a mark
    /// (threaded by value) to refuse quantifying an inherited-open variable;
    /// this field lets the [`Constraint::ArgCheck`] wake apply the same
    /// discipline: a discharge may bind only open variables created in the
    /// current binding ([`Gen::arg_check_binds_only_current_vars`]), never
    /// retro-ground an earlier binder's still-open variable (whose type FCS
    /// fixed at *its* binding, e.g. `let g = id` staying `'a -> 'a` under a
    /// later `g 1`).
    cur_mark: u32,
    /// The **scheme-instantiation provenance set** (Stage 3.3c): every fresh
    /// inference variable [`Self::instantiate`] created from a [`Ty::Param`]. An
    /// application's suspended [`Constraint::ArgCheck`] discharges only when its
    /// domain resolves to a **no-subsumption** type — a sealed BCL primitive, a
    /// tuple of such, or *an unbound root in this set*. Membership is why
    /// `let g y = id y` may safely ground `y ↔ id`'s domain: that domain is an
    /// instantiation of one of *our own* schemes, whose quantified typar admits no
    /// coercion (our generaliser only quantifies unconstrained vars). Never
    /// cleared: an entry is a permanent fact about a variable's origin. Membership
    /// is checked against union-find **roots** ([`Self::is_scheme_inst_root`]), so
    /// a var unioned with an instantiation var still counts.
    scheme_inst_vars: HashSet<TyVid>,
    /// The file's in-scope instance-extension-member surface (OV-6): the
    /// extension-absence gate the method-call wake consults before committing an
    /// overload. Computed once in [`infer_file`] from the file and the env.
    ext_scope: ExtensionScope,
}

impl<'a> Gen<'a> {
    fn new(resolved: &'a ResolvedFile, env: &'a AssemblyEnv, ext_scope: ExtensionScope) -> Self {
        Gen {
            resolved,
            env,
            ext_scope,
            table: InferTable::new(),
            constraints: Vec::new(),
            exprs: Vec::new(),
            def_vars: HashMap::new(),
            param_slots: HashMap::new(),
            param_defs: HashSet::new(),
            def_schemes: HashMap::new(),
            complete: true,
            poison: Vec::new(),
            cur_params: Vec::new(),
            member_resolutions: HashMap::new(),
            cur_mark: 0,
            scheme_inst_vars: HashSet::new(),
        }
    }

    /// Mark the current binding **walk-incomplete** — some sub-expression or
    /// pattern is not fully modelled, so the binding must not generalise (Stage
    /// 3.2c-2c). Ground emission is unaffected.
    fn mark_incomplete(&mut self) {
        self.complete = false;
    }

    /// Reset the per-binding state at a binding's start — the vid mark (also
    /// stored as [`Self::cur_mark`] for the ArgCheck wake's environment guard),
    /// walk-completeness, the poison set, and the parameter list — returning
    /// the mark for the paths that thread it to
    /// [`Self::finalise_function`]. Every binding path must call this exactly
    /// once, before allocating any of the binding's variables.
    fn begin_binding(&mut self) -> u32 {
        let mark = self.table.mark();
        self.cur_mark = mark;
        self.complete = true;
        self.poison.clear();
        self.cur_params.clear();
        mark
    }

    /// The inference variable for a binder, created on first reference. A use
    /// and its binding thus share one variable regardless of which the walk
    /// reaches first — unification is order-independent.
    fn def_var(&mut self, def: DefId) -> TyVid {
        if let Some(&v) = self.def_vars.get(&def) {
            return v;
        }
        let v = self.table.fresh();
        self.def_vars.insert(def, v);
        v
    }

    fn eq(&mut self, a: Ty, b: Ty) {
        self.constraints.push(Constraint::Eq(a, b));
    }

    /// Push a suspended member constraint: `recv` has an instance data member
    /// `name` whose type is `result`, whose member-name token spans `use_range`
    /// (Stage 3.3a; `use_range` added in 3.3b). Both type endpoints are
    /// **poisoned** unconditionally at the point of generation
    /// ([`Self::gen_member_access`]), so this only enqueues the relation; the
    /// solver wakes it when `recv`'s head is concrete.
    fn has_member(
        &mut self,
        recv: TyVid,
        name: String,
        result: TyVid,
        use_range: TextRange,
        kind: MemberAccessKind,
    ) {
        self.constraints.push(Constraint::HasMember {
            recv,
            name,
            result,
            use_range,
            kind,
        });
        // Poison both endpoints: an unwoken (open) member relation must not let its
        // receiver or result generalise. A ground endpoint is unaffected (poison
        // bites only open vars). Shared by the data-member chain
        // ([`Self::gen_member_access`]) and the method call ([`Self::infer_method_call`]).
        self.poison.push(recv);
        self.poison.push(result);
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
    fn gen_member_access(&mut self, recv: TyVid, segments: &[SyntaxToken]) -> Option<TyVid> {
        let mut cur = recv;
        let mut last = None;
        for seg in segments {
            let result = self.table.fresh();
            self.has_member(
                cur,
                ident_text(seg),
                result,
                seg.text_range(),
                MemberAccessKind::Data,
            );
            cur = result;
            last = Some(result);
        }
        last
    }

    /// Whether `def` is a **parameter of the current binding** — one collected by
    /// [`Self::param_var`] into [`Self::cur_params`] for this function head. A use
    /// of such a parameter is generalisable (its variable is a genuine local of
    /// this binding); a use of any *other* in-file binder is an environment
    /// reference this stage must not quantify (Stage 3.2c-2c). The list is one
    /// entry per simple parameter, so the linear scan is cheap.
    fn is_current_param(&self, def: DefId) -> bool {
        self.cur_params.iter().any(|(d, _)| *d == def)
    }

    /// The in-file binder a source range resolves to, if any (a cross-file item,
    /// assembly entity/member, deferred, or unresolved use returns `None`, so it
    /// stays Deferred).
    fn def_at(&self, range: TextRange) -> Option<DefId> {
        self.resolved
            .resolution_at(range)
            .and_then(|res| self.resolved.resolved_def_id(res))
    }

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
    fn walk(&mut self, file: &ImplFile) {
        for node in file.syntax().descendants() {
            if let Some(let_decl) = LetDecl::cast(node) {
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
        // A recursive group (`let rec … and …`) is solved as a unit: a sibling
        // binding's constraints can flow back to a binder, so a literal RHS is
        // *not* isolated and its type may be retargeted. Defer the whole group
        // (D5); its binders get no typed variable, so uses of them stay Deferred.
        if let_decl.is_rec() {
            return;
        }
        for binding in let_decl.bindings() {
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
            if let Some(ann) = binding.return_type() {
                self.annotated_value_binding(&binding, &ann);
                continue;
            }
            let Some(rhs) = binding.expr() else {
                continue;
            };
            // Per-binding state (Algorithm W). Take the vid mark *before* any of
            // this binding's variables are allocated, and reset completeness /
            // poison / the parameter list. The RHS of a real `LET_DECL` never
            // itself contains a `LET_DECL` (expression-level lets are
            // `LET_OR_USE_EXPR`), so these batches do not nest.
            debug_assert!(
                rhs.syntax()
                    .descendants()
                    .all(|n| n.kind() != SyntaxKind::LET_DECL),
                "a LET_DECL nested inside a walked binding's RHS breaks sequential solve"
            );
            let mark = self.begin_binding();

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
                    let rhs_var = self.infer_expr(&rhs, None);
                    if let Some(tok) = named.ident()
                        && let Some(def) = self.def_at(tok.text_range())
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
                            self.eq(Ty::Var(dv), Ty::Var(rhs_var));
                        }
                    }
                    self.solve();
                }
                // A function binding `let f x … = <body>` — a `LongIdentPat` head
                // with arguments. The parameter slot variables are collected
                // *first* (each simple-named parameter gets a private slot recorded
                // in `param_slots`, so the body's condition typing can ground that
                // slot); then the body is walked in synth mode for its own
                // emissions (e.g. a body that is an `if` emits its result), and its
                // return variable curries with the parameter slots into the
                // function's own `Ty::Fun` type ([`Self::function_type`]). The
                // arguments span both `SynArgPats` shapes, matching `binders`'s
                // function-head test: the curried list (`args()`) and the
                // named-field group `Case (field = pat; …)` (`name_pat_pairs()`,
                // whose own `args()` is empty). Parameters bind lazily via name
                // resolution at their uses.
                Some(Pat::LongIdent(head))
                    if head.args().next().is_some() || head.name_pat_pairs().is_some() =>
                {
                    let arg_vars: Vec<TyVid> =
                        head.args().map(|arg| self.param_var(&arg)).collect();
                    let ret = self.infer_expr(&rhs, None);
                    let f_def = self.function_type(&head, &arg_vars, ret);
                    // On a complete binding, undo the 2b slot/binder decoupling so
                    // `let id x = x` generalises to `'a -> 'a`, not `'a -> 'b`.
                    if self.complete {
                        self.slot_binder_reunify();
                    }
                    self.solve();
                    if let Some(f_def) = f_def {
                        self.finalise_function(f_def, mark);
                    }
                }
                // A parenthesised head: a **trivial typed pattern**
                // `let (x: T) = …` (`Paren > Typed > Named`) rides the Stage
                // R2-a annotation gate — same positionally-fixed
                // binder↔annotation truth as the return-annotation form, with
                // the same R2-e coverage check-walk over the RHS. Any other
                // parenthesised shape keeps today's catch-all behaviour.
                Some(Pat::Paren(paren)) => {
                    if let Some((named, ty)) = trivial_typed_head(&paren) {
                        self.bind_annotated_named(&named, &ty, Some(rhs));
                    } else {
                        self.solve();
                    }
                }
                _ => {
                    self.solve();
                    continue;
                }
            }
        }
    }

    /// An annotated **value** binding `let x : T = …` (Stage R2-a) or an
    /// annotated **function** binding `let f x : T = …` (Stage R2-c). The value
    /// form's binder is bound to the annotation's type iff the shape is the
    /// modelled one — a non-mutable, simple-named head whose annotation passes
    /// [`Self::annotation_ty`] — and its RHS is check-walked for coverage
    /// (Stage R2-e; see [`Self::bind_annotated_named`]). The function form
    /// dispatches to [`Self::annotated_function_binding`]. Anything else stays
    /// silent, exactly like the pre-R2-a blanket skip (D5).
    fn annotated_value_binding(&mut self, binding: &Binding, ann: &Type) {
        // Mutable binders are deliberately out of scope in v1 (the
        // set-site↔annotation interaction is unaudited; see the plan's
        // out-of-scope list).
        if binding.is_mutable() {
            return;
        }
        match binding.pat() {
            Some(Pat::Named(named)) => self.bind_annotated_named(&named, ann, binding.expr()),
            // A function head with a return annotation (Stage R2-c).
            Some(Pat::LongIdent(head))
                if head.args().next().is_some() || head.name_pat_pairs().is_some() =>
            {
                self.annotated_function_binding(&head, binding.expr(), ann);
            }
            // Any other pattern shape: skipped as before.
            _ => {}
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
    fn annotated_function_binding(&mut self, head: &LongIdentPat, rhs: Option<Expr>, ann: &Type) {
        let Some(t) = self.annotation_ty(ann) else {
            return;
        };
        let mark = self.begin_binding();

        let arg_vars: Vec<TyVid> = head.args().map(|arg| self.param_var(&arg)).collect();
        let r = self.table.fresh();
        self.eq(Ty::Var(r), t);
        if let Some(rhs) = rhs {
            debug_assert!(
                rhs.syntax()
                    .descendants()
                    .all(|n| n.kind() != SyntaxKind::LET_DECL),
                "a LET_DECL nested inside a walked binding's body breaks sequential solve"
            );
            // The body walk: check mode against the annotation, the top-level
            // relation suspended rather than eagerly poisoned (the
            // [`Self::infer_arg`] opt-out, shared with application arguments).
            if let Some(bv) = self.infer_arg(&rhs, r) {
                self.constraints
                    .push(Constraint::ArgCheck { arg: bv, dom: r, r });
            }
        } else {
            self.mark_incomplete();
        }
        let f_def = self.function_type(head, &arg_vars, Some(r));
        if self.complete {
            self.slot_binder_reunify();
        }
        self.solve();
        if let Some(f_def) = f_def {
            self.finalise_function(f_def, mark);
        }
    }

    /// Bind the named binder `named` to its annotation `ann`'s type, when
    /// [`Self::annotation_ty`] can produce one — the shared tail of the
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
    fn bind_annotated_named(&mut self, named: &NamedPat, ann: &Type, rhs: Option<Expr>) {
        self.begin_binding();
        if let Some(t) = self.annotation_ty(ann)
            && let Some(tok) = named.ident()
            && let Some(def) = self.def_at(tok.text_range())
        {
            let dv = self.def_var(def);
            self.eq(Ty::Var(dv), t);
            if let Some(rhs) = rhs {
                debug_assert!(
                    rhs.syntax()
                        .descendants()
                        .all(|n| n.kind() != SyntaxKind::LET_DECL),
                    "a LET_DECL nested inside a walked binding's RHS breaks sequential solve"
                );
                let _ = self.infer_expr(&rhs, Some(dv));
            }
        }
        self.solve();
    }

    /// The [`Ty`] an annotation provably denotes, or `None` to defer (Stage
    /// R2-a). Accepts a **bare single-segment `Type::LongIdent`** whose head
    /// the resolver resolved to a concrete entity, peels transparent parens,
    /// and **structurally recurses** function, (non-struct, measure-free)
    /// tuple, and array shapes, whose renderings agree with FCS's canonical
    /// forms byte-for-byte (plan probe R11). Everything else defers:
    /// unresolved/deferred heads, generic/measure applications (`int64
    /// option`, `float<m>`), type variables, and every other `Type` shape.
    ///
    /// A concrete [`Resolution::Entity`] bridges via
    /// [`Self::entity_annotation_ty`] (Stage R2-d) — covering `String` under
    /// `open System` at a single-segment head, qualified `System.Int64` at a
    /// multi-segment head's tail, and the F# primitive aliases: `int` records
    /// its FSharp.Core abbreviation **marker**, which the bridge chases
    /// through the pickled target chain to `System.Int32`. There is no
    /// hard-coded alias table — the semantics come from FSharp.Core's own
    /// signature data, and the real-FSharp.Core sweep in
    /// `resolve_fsharp_core.rs` pins that the chase reproduces every
    /// primitive alias exactly. (With no assembly env at all — FSharp.Core
    /// unreferenced — a primitive annotation records nothing and defers,
    /// which is honest: the name genuinely has no binding.)
    ///
    /// Project-defined types (`Local`/`Item`) still defer: their canonical
    /// rendering against the oracle's `<Project>.M.T` form is unprobed (the
    /// plan's R2-d "probe first" rule). Widening any of these qualifiers needs
    /// its own soundness argument in `docs/completed/r2-annotation-typing-plan.md`
    /// first.
    fn annotation_ty(&self, ty: &Type) -> Option<Ty> {
        match ty {
            Type::Paren(p) => self.annotation_ty(&p.inner()?),
            Type::LongIdent(li) => {
                let li = li.long_ident()?;
                // An active-pattern segment cannot be projected as an ident
                // token; treating the remaining tokens as the path would
                // mis-read it (cannot occur in a well-formed type, but recovery
                // trees can hold anything).
                if li.active_pat_names().next().is_some() {
                    return None;
                }
                let idents: Vec<SyntaxToken> = li.idents().collect();
                // Only a concrete `Entity` recorded at the head's **final**
                // segment — where the resolver roots both a bare and a
                // qualified type — bridges (R2-d); anything else defers.
                match idents.as_slice() {
                    [.., last] => match self.resolved.resolution_at(last.text_range()) {
                        Some(Resolution::Entity(handle)) => self.entity_annotation_ty(handle),
                        _ => None,
                    },
                    [] => None,
                }
            }
            Type::Fun(f) => Some(Ty::Fun {
                arg: Box::new(self.annotation_ty(&f.arg()?)?),
                ret: Box::new(self.annotation_ty(&f.ret()?)?),
            }),
            Type::Tuple(t) => {
                // A struct tuple is a different runtime type; a `/` segment is
                // a unit-of-measure tuple form — both defer.
                if t.is_struct() {
                    return None;
                }
                let mut elems = Vec::new();
                for seg in t.segments() {
                    match seg {
                        TupleSegment::Type(e) => elems.push(self.annotation_ty(&e)?),
                        TupleSegment::Star(_) => {}
                        TupleSegment::Slash(_) => return None,
                    }
                }
                // `Ty::Tuple` is always arity ≥ 2; a degenerate segment list
                // (recovery) defers.
                if elems.len() < 2 {
                    return None;
                }
                Some(Ty::Tuple(elems))
            }
            Type::Array(a) => Some(Ty::Array {
                elem: Box::new(self.annotation_ty(&a.element_type()?)?),
                rank: u32::try_from(a.rank()).ok()?,
            }),
            _ => None,
        }
    }

    /// Bridge a concrete annotation-head [`Resolution::Entity`] to a
    /// [`Ty::Named`] (Stage R2-d), under the same conventions as the
    /// `member_ty.rs` `TypeRef` bridge: **non-generic** and **non-nested**
    /// only, with the same defer set.
    ///
    /// An **abbreviation marker** bridges through its chased terminal
    /// ([`AssemblyEnv::resolve_abbreviation_target`]): `let x : int = …` records the
    /// `int` marker, whose chain (`int` → `int32` → `System.Int32`) lands on
    /// the entity this then bridges exactly like a directly-named one. The
    /// binder-types oracle strips abbreviation layers (fcs-dump's
    /// `renderTypeInScope` renders `AbbreviatedType` through), so the
    /// terminal's FQN is precisely FCS's rendering currency for an annotation
    /// written through an alias. An unchaseable marker defers as before.
    ///
    /// Additionally deferred here:
    ///
    /// - an F# **module** (not a type an annotation can denote — a module
    ///   record at a type head is a resolver artefact to stay silent on);
    /// - a **measure** (`Ty` has no measure story — plan probe R5);
    /// - a **source-renamed** entity (its FCS rendering uses the F# source
    ///   name, which the flat `namespace + IL name` path would misrender);
    /// - the **`unit` terminal** (`Microsoft.FSharp.Core.Unit`): `Ty` has no
    ///   unit story and 3.3d's void rule assumes its absence, so the one
    ///   terminal a chase can reach that `Ty` cannot carry stays deferred
    ///   (revisitable when `Ty` gains a unit story — probe R9 showed the
    ///   rendering itself would be correct).
    ///
    /// The nested/renamed check is one comparison: the canonical dotted path
    /// must equal [`AssemblyEnv::entity_full_name`], which walks enclosing
    /// entities for nested types and prefers the source name.
    fn entity_annotation_ty(&self, handle: EntityHandle) -> Option<Ty> {
        let entity = self.env.entity(handle);
        if !entity.generic_parameters.is_empty() {
            return None;
        }
        let (handle, entity) = if entity.kind == EntityKind::Abbreviation {
            let terminal = self.env.resolve_abbreviation_target(handle)?;
            (terminal, self.env.entity(terminal))
        } else {
            (handle, entity)
        };
        if !entity.generic_parameters.is_empty() {
            return None;
        }
        if matches!(
            entity.kind,
            EntityKind::Module | EntityKind::Abbreviation | EntityKind::Measure
        ) {
            return None;
        }
        let mut path: Vec<String> = entity.namespace.clone();
        path.push(entity.name.clone());
        if path == ["Microsoft", "FSharp", "Core", "Unit"] {
            return None;
        }
        if self.env.entity_full_name(handle) != path.join(".") {
            return None;
        }
        Some(Ty::Named(path))
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
    fn slot_binder_reunify(&mut self) {
        // Iterate a snapshot so the borrow of `cur_params` does not conflict with
        // `def_var` / `eq`; the list is small (one entry per simple parameter).
        for (def, slot) in std::mem::take(&mut self.cur_params) {
            let dv = self.def_var(def);
            self.eq(Ty::Var(slot), Ty::Var(dv));
        }
    }

    /// Synthesize the type variable of expression `e`, generating its
    /// constraints, in **bidirectional** style. `expected` is the type the
    /// surrounding context demands and acts here as a *mode flag*:
    ///
    /// - `None` — *synth mode*, a **coercion-free** position (the bare `let` RHS,
    ///   an unannotated tuple element, an `if` *then*-branch). No expected type
    ///   reaches the expression, so its elaborated type *is* its synthesized
    ///   type, and the node is **recorded** for emission.
    /// - `Some(exp)` — *check mode*, a coercion-**possible** position (an `if`
    ///   *else*-branch against the result; a check-mode tuple's elements; later,
    ///   a function argument against the parameter). The node is **not** recorded
    ///   — its elaborated type may be a coercion of the synthesized type, which
    ///   this stage cannot model (no subtype relation yet), so emitting the
    ///   synthesized type could be wrong (D5). Crucially the variable is **not**
    ///   unified with `exp`: with no subtype relation a cross-constraint could
    ///   only force *equality*, which would either wrongly reject a legal
    ///   coercion or, on an unbound `exp`, back-flow a coerced type onto a binder.
    ///   So check mode today *only* suppresses emission and propagates the mode to
    ///   children; the threaded `exp` is inert payload, reserved for the future.
    ///
    /// The recursive traversal, the per-construct constraints, and the `expected`
    /// threading are the permanent HM spine. When subtype-aware checking lands,
    /// check mode is *completed* (not replaced): `exp` becomes a real expected
    /// type, the variable is related to it by subtyping, and the coerced `exp` is
    /// emitted when the synthesized type is a subtype.
    ///
    /// This wrapper adds the **poison** step (Stage 3.2c-2c): a check-mode call
    /// (`expected.is_some()`) drops the relation between the checked expression and
    /// its expected type, which FCS keeps — so poison both endpoints (the expected
    /// and returned variables). Centralising it here means every present and future
    /// check site inherits it, so a variable FCS might ground through a dropped
    /// relation is never wrongly generalised.
    fn infer_expr(&mut self, e: &Expr, expected: Option<TyVid>) -> Option<TyVid> {
        let result = self.infer_expr_inner(e, expected);
        if let Some(exp) = expected {
            self.poison.push(exp);
            if let Some(r) = result {
                self.poison.push(r);
            }
        }
        result
    }

    /// The per-construct generation rules (the body of [`Self::infer_expr`],
    /// without its poison wrapper). Every arm that returns `None` because a
    /// sub-expression is unmodelled, and every unwalked child, marks the binding
    /// walk-incomplete ([`Self::mark_incomplete`]) — the seed of Stage 3.2c-2c's
    /// generalisation gate. (Ground emission is unaffected: it does not read the
    /// completeness flag.)
    fn infer_expr_inner(&mut self, e: &Expr, expected: Option<TyVid>) -> Option<TyVid> {
        match e {
            // A bare literal. A measure literal (`1.0<kg>`) is an
            // `Expr::MeasureLit`, not `Expr::Const`, so it is excluded
            // structurally — its inner numeric token is *not* the measured type.
            Expr::Const(c) => {
                let Some(ty) = literal_ty(c) else {
                    // A deferred literal (`USER_NUM_LIT`, a source-location
                    // identifier) is unmodelled — its type is not fixed even in
                    // isolation — so the binding must not generalise.
                    self.mark_incomplete();
                    return None;
                };
                let v = self.table.fresh();
                self.eq(Ty::Var(v), ty);
                // The literal *token*'s range, not the `ConstExpr` node's: in a
                // tuple element (`(1, "hi")`) the node range swallows the leading
                // ` ` trivia after the comma, but FCS's node spans only the
                // literal.
                if let Some(lit) = c.literal() {
                    self.emit(lit.text_range(), v, expected);
                }
                Some(v)
            }
            // A value use: its type is the referenced binder's. In synth mode the
            // use occurrence is emitted at its own range; a use resolving to a
            // non-in-file target, or to a binder we never typed, leaves its
            // variable unbound and so stays Deferred at read-off. A use resolving
            // to a **generalised** binder ([`Self::def_schemes`]) instantiates the
            // scheme with fresh variables per distinct [`Ty::Param`] (Stage
            // 3.2c-2c) — checked *before* the plain `def_var` path.
            Expr::Ident(ident) => {
                let Some(tok) = ident.ident() else {
                    self.mark_incomplete();
                    return None;
                };
                let Some(def) = self.def_at(tok.text_range()) else {
                    // A use of a name we do not resolve to an in-file binder
                    // (cross-file, assembly, deferred, unresolved) is unmodelled.
                    self.mark_incomplete();
                    return None;
                };
                if let Some(scheme) = self.def_schemes.get(&def).cloned() {
                    let body = self.instantiate(&scheme);
                    let uv = self.table.fresh();
                    self.eq(Ty::Var(uv), body);
                    self.emit(tok.text_range(), uv, expected);
                    return Some(uv);
                }
                let dv = self.def_var(def);
                // A reference to an **environment** binder — one *not* introduced
                // by the current binding (not one of its parameters) — must not be
                // generalised (Stage 3.2c-2c). Such a binder carries an FCS-known
                // type from *its own* binding: an earlier value's type (which we
                // may not have modelled, e.g. an annotated `let a : int = 1`, a
                // `let rec` or tuple-pattern binding that inference skipped) or a
                // later forward reference. Poison its variable so a still-open
                // environment reference blocks generalisation (a *ground* one — a
                // typed earlier value — is unaffected, since poison only bites open
                // vars). Without this, a skipped earlier binder allocates its
                // variable lazily at the use site (index ≥ the current mark), and
                // the mark check alone would wrongly treat it as quantifiable
                // (`let a : int = 1 \n let h x = (x, a)` ⇒ a bogus `'a -> 'a * 'b`).
                if !self.is_current_param(def) {
                    self.poison.push(dv);
                }
                self.emit(tok.text_range(), dv, expected);
                Some(dv)
            }
            // Parentheses are transparent: the type, position (so `expected`
            // passes straight through), and emission are the inner expression's;
            // FCS's elaborated tree has no node at the parens. (3.2b-2) A `Paren`
            // wrapping a recovery hole (no inner expression) is unmodelled.
            Expr::Paren(p) => {
                let Some(inner) = p.inner() else {
                    self.mark_incomplete();
                    return None;
                };
                self.infer_expr(&inner, expected)
            }
            // A reference tuple `(a, b, …)`: `Ty::Tuple` of the element types. The
            // elements share the tuple's mode — coercion-free (emitted) only when
            // the tuple itself is synth; in check mode the elements are
            // coercion-possible (an expected tuple type could retarget them, e.g.
            // `(3, 4)` against `int64 * int64`), so they are checked, not emitted.
            // An element we can't type becomes a fresh unbound var, so the *tuple*
            // is omitted (not ground) while typeable synth-mode elements emit.
            // (3.2b-2, mode propagation 3.2c-1)
            Expr::Tuple(t) => {
                // Struct tuples (`struct (a, b)`) have a distinct runtime type
                // and canonical rendering; defer them (unmodelled).
                if t.is_struct() {
                    self.mark_incomplete();
                    return None;
                }
                let elems: Vec<Ty> = t
                    .elements()
                    .map(|el| {
                        // Propagate the tuple's mode: synth elements when the tuple
                        // is synth, otherwise a fresh per-element expected (check —
                        // suppresses the element's emission).
                        let elem_expected = expected.map(|_| self.table.fresh());
                        match self.infer_expr(&el, elem_expected) {
                            Some(v) => Ty::Var(v),
                            None => {
                                // An element we cannot type leaves the tuple
                                // un-ground; the element's own recursion already set
                                // incomplete, but this arm may also be reached for a
                                // recovery hole — be explicit.
                                self.mark_incomplete();
                                Ty::Var(self.table.fresh())
                            }
                        }
                    })
                    .collect();
                // A well-formed tuple has ≥ 2 elements; anything less is a
                // degenerate parse, not a tuple type.
                if elems.len() < 2 {
                    self.mark_incomplete();
                    return None;
                }
                let tv = self.table.fresh();
                self.eq(Ty::Var(tv), Ty::Tuple(elems));
                self.emit(node_span(t.syntax()), tv, expected);
                Some(tv)
            }
            // `if c then a else b`: F# uses the *then*-branch's type as the result
            // and coerces `else` to it. The then-branch carries the if's mode (the
            // result's source); the else-branch is checked against the result (a
            // coercion-possible position — not emitted). The condition is left to
            // name resolution — typing it as `bool` needs modelled function types
            // to stay sound (see the module docs), a later slice.
            //
            // In **synth** mode the result is the then-branch's synthesized type;
            // if the then-branch can't synthesize, the whole `if` defers (its type
            // is unknown — we must *not* take it from the coercible else, which FCS
            // may retarget). An `if` **without a final `else`** is *not*
            // synthesized: `if c then a` (and an `elif` chain with no trailing
            // `else`, e.g. `if a then 1 elif b then 2`) desugars with an implicit
            // `else ()`, so its result is `unit` and the then-branch sits in a unit
            // *check* position — emitting its synthesized type would disagree with
            // FCS, so we defer the whole `if` ([`if_chain_has_final_else`]). In
            // **check** mode both branches are checked against the expected type and
            // nothing is emitted. (3.2c-1)
            Expr::IfThenElse(if_expr) => {
                // The condition is `bool` in either mode (F# admits no coercion
                // there), so constrain it regardless of the `if`'s own mode — this
                // grounds a parameter used directly as a condition (3.2c-2b).
                self.constrain_bool(if_expr.condition());
                match expected {
                    None => {
                        // An `if` with no final `else` desugars to `else ()`
                        // (result `unit`), which this stage does not model — defer,
                        // and mark incomplete (its then-branch's true type is a
                        // dropped unit-check relation).
                        if !if_chain_has_final_else(if_expr) {
                            self.mark_incomplete();
                            return None;
                        }
                        let Some(then_branch) = if_expr.then_branch() else {
                            self.mark_incomplete();
                            return None;
                        };
                        // A then-branch that cannot synthesize leaves the `if`'s
                        // result unknown; its recursion already set incomplete.
                        let then_var = self.infer_expr(&then_branch, None)?;
                        if let Some(else_branch) = if_expr.else_branch() {
                            self.infer_expr(&else_branch, Some(then_var));
                        }
                        self.emit(node_span(if_expr.syntax()), then_var, None);
                        Some(then_var)
                    }
                    Some(exp) => {
                        if let Some(then_branch) = if_expr.then_branch() {
                            self.infer_expr(&then_branch, Some(exp));
                        }
                        if let Some(else_branch) = if_expr.else_branch() {
                            self.infer_expr(&else_branch, Some(exp));
                        }
                        Some(exp)
                    }
                }
            }
            // A lambda `fun x … -> body`. Walk the body so its sub-expressions get
            // typed, carrying the lambda's **mode**: coercion-free (synth) only when
            // the lambda itself is (a bare `let g = fun … -> …`); reached in *check*
            // mode (e.g. an `if`-branch checked against another function), an
            // expected function type could retarget the body, so we check it —
            // emission suppressed. The lambda's own function type is not modelled
            // yet (`Ty::Fun` is a later slice), so it defers. A param use in the
            // body binds lazily via name resolution. (3.2c-2a)
            Expr::Fun(fun) => {
                if let Some(body) = fun.body() {
                    let body_expected = expected.map(|_| self.table.fresh());
                    self.infer_expr(&body, body_expected);
                }
                // The lambda's own function type is not modelled yet (`Ty::Fun` on
                // a `fun` value is a later slice), so it defers — and marks the
                // binding incomplete, since a body containing a lambda has an FCS
                // type we cannot reproduce.
                self.mark_incomplete();
                None
            }
            // A `while c do body`: the condition is constrained to `bool`
            // (3.2c-2b), and the body is **checked against `unit`** — a check (not
            // synth) position — so it is walked in check mode (emission suppressed:
            // a non-unit body is a coercion/error FCS reports as `unit`, not the
            // body's synthesized type). The loop's own `unit` type is left to a
            // later slice. (3.2c-2a / 3.2c-2b)
            Expr::While(while_expr) => {
                self.constrain_bool(while_expr.cond());
                if let Some(body) = while_expr.body() {
                    let unit_check = self.table.fresh();
                    self.infer_expr(&body, Some(unit_check));
                }
                // A `while` expression's own `unit` type is not modelled yet, so it
                // defers — and marks the binding incomplete.
                self.mark_incomplete();
                None
            }
            // Function application `f x` (Stage 3.2c-3). Both `APP_EXPR` and
            // `INFIX_APP_EXPR` cast to `Expr::App`; only a genuine (non-infix,
            // non-bracket-indexer) application is modelled. An **infix** application
            // (`x + 1`) and a **bracket indexer** (`arr[i]`, which F# lowers to a
            // `GetSlice`/`Item` member lookup, *not* a function application, but the
            // parser still stores under `APP_EXPR`) both stay unmodelled ⇒
            // incomplete, as do `TypeApp`, applied `DotGet`, and everything else (the
            // catch-all below / other arms). Curried `f x y` falls out of the nested
            // `App`s.
            Expr::App(app) if !app.is_infix() && !app.is_bracket_indexer() => {
                let r = self.infer_app(app)?;
                self.emit(node_span(app.syntax()), r, expected);
                Some(r)
            }
            // A dotted long-ident `s.Length` (Stage 3.3a). The parser stores a
            // value-receiver member access as a `LONG_IDENT_EXPR` (its head resolves
            // to the receiver value; the trailing segments are the members) — *not*
            // a `DotGet`, which is the `(expr).Member` shape below. This arm only
            // fires when the **head** segment resolves to an in-file value binder: a
            // fully-qualified static path (`System.Console.WriteLine`, whose head is
            // a namespace/type, not a value) is left unmodelled here (the catch-all
            // defers it, so the static-member `Member` resolution is untouched — no
            // regression). The receiver's own value node is emitted at the head
            // range (FCS emits a `value:module` node there), then the members chain.
            Expr::LongIdent(li) => self.infer_long_ident_member(li, expected),
            // A `(expr).Member` member access (Stage 3.3a). Both `"hi".Length` and
            // `(f x).Foo` parse as `DOT_GET_EXPR`: an inner receiver *expression*
            // plus a `LongIdent` member path. The receiver is synthesized (its own
            // node emitted where FCS has one — e.g. the `"hi"` literal), and the
            // member path chains from its variable.
            Expr::DotGet(dg) => self.infer_dot_get(dg, expected),
            // Any other expression shape is unmodelled: defer and mark incomplete
            // (the FCS type on this subtree is one we did not reproduce, so an
            // enclosing binding must not generalise). This catches an **infix** or
            // **bracket-indexer** `App` too, which the guarded arm above deliberately
            // does not match.
            _ => {
                self.mark_incomplete();
                None
            }
        }
    }

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
    fn infer_app(&mut self, app: &AppExpr) -> Option<TyVid> {
        let Some(func) = app.func() else {
            self.mark_incomplete();
            return None;
        };
        let Some(arg) = app.arg() else {
            self.mark_incomplete();
            return None;
        };
        // A **method call** `recv.Method(args)` is an application whose callee is a
        // member access (Stage 3.3d). `infer_callee` never modelled such a callee
        // (it always deferred), so routing it to method typing only adds coverage —
        // no value-application emission changes.
        if is_member_access_callee(&func) {
            return self.infer_method_call(&func, &arg);
        }
        // Synthesize the function position (no node emitted).
        let tf = self.infer_callee(&func)?;
        let d = self.table.fresh();
        let r = self.table.fresh();
        // Genuinely true: applying `func` forces it to a function shape.
        self.eq(
            Ty::Var(tf),
            Ty::Fun {
                arg: Box::new(Ty::Var(d)),
                ret: Box::new(Ty::Var(r)),
            },
        );
        // The argument is a check position against the domain `d`, so it is walked
        // in check mode (its node is not emitted) — but **without** the poison
        // wrapper's automatic poison of `d`/arg ([`Self::infer_arg`]). Stage 3.3c
        // replaces that eager poison with a *suspended* `ArgCheck`: the dropped
        // subsumption relation is discharged as a genuine `Eq(arg, d)` when it is
        // provably coercion-free (a walk-complete binding and a no-subsumption
        // domain), and only poisoned if it stays undischarged. The argument's var,
        // if we could synthesize one, is the `ArgCheck`'s `arg` endpoint.
        let arg_var = self.infer_arg(&arg, d);
        match arg_var {
            Some(arg_var) => {
                // Suspend the arg↔param relation. `solve` wakes it (a complete
                // binding + a no-subsumption `d`) or, failing that, poisons
                // `arg`/`d`/`r` — so an unwoken relation never generalises anything.
                self.constraints.push(Constraint::ArgCheck {
                    arg: arg_var,
                    dom: d,
                    r,
                });
            }
            None => {
                // The argument did not synthesize a variable (an unmodelled
                // shape — the arg's own recursion already marked incomplete). There
                // is no `arg` endpoint to relate, so fall back to the 3.2c-3 eager
                // poison of `d`/`r`: an unwoken relation must not let either
                // generalise. A *ground* `r` (a ground function's result) is
                // unaffected, so ground payoffs still emit.
                self.poison.push(d);
                self.poison.push(r);
            }
        }
        Some(r)
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
    /// - **A nested application** in argument position — `infer_expr_inner`'s
    ///   `App` arm ([`Self::infer_app`]) already suspends its *own* arg and returns
    ///   its result without emitting a node.
    ///
    /// Any other argument shape recurses through [`Self::infer_expr_inner`] once,
    /// so its *sub*-positions (e.g. a tuple's elements, an `if`'s branches) keep
    /// the wrapper's poison — the subsumption dropped *there* is genuinely eager;
    /// only the top-level arg↔`d` relation is deferred.
    fn infer_arg(&mut self, arg: &Expr, d: TyVid) -> Option<TyVid> {
        match arg {
            Expr::Paren(p) => match p.inner() {
                Some(inner) => self.infer_arg(&inner, d),
                None => {
                    self.mark_incomplete();
                    None
                }
            },
            _ => self.infer_expr_inner(arg, Some(d)),
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
    ///   in [`Self::infer_expr_inner`]'s ident arm). No node is emitted.
    /// - **Parentheses** ([`Expr::Paren`]): transparent — peel and recurse.
    /// - **A nested application** (curried `f x y` = `App(App(f, x), y)`): the inner
    ///   `App(f, x)` is itself a callee, so recurse through [`Self::infer_app`]-style
    ///   logic with its own node emission suppressed (an inner curried application
    ///   carries no FCS node either).
    ///
    /// Any other callee shape (an applied `DotGet`, a lambda, a literal) is
    /// unmodelled ⇒ incomplete, and the whole application defers.
    fn infer_callee(&mut self, e: &Expr) -> Option<TyVid> {
        match e {
            Expr::Paren(p) => {
                let Some(inner) = p.inner() else {
                    self.mark_incomplete();
                    return None;
                };
                self.infer_callee(&inner)
            }
            // A nested (curried) application in function position: type it as an
            // application whose residual `r` is the callee type, recording **no**
            // node ([`Self::infer_app`] never emits — an inner curried application
            // carries no FCS node either). It still pushes its own
            // `Eq(tf, Fun(d, r))` and checks its argument. An infix or
            // bracket-indexer `App` in the callee spine is unmodelled (⇒ the
            // catch-all below marks incomplete and the whole application defers).
            Expr::App(inner) if !inner.is_infix() && !inner.is_bracket_indexer() => {
                self.infer_app(inner)
            }
            // A value/function reference: mirror the ident arm's resolution, but
            // *do not* emit a node.
            Expr::Ident(ident) => {
                let Some(tok) = ident.ident() else {
                    self.mark_incomplete();
                    return None;
                };
                let Some(def) = self.def_at(tok.text_range()) else {
                    self.mark_incomplete();
                    return None;
                };
                if let Some(scheme) = self.def_schemes.get(&def).cloned() {
                    let body = self.instantiate(&scheme);
                    let uv = self.table.fresh();
                    self.eq(Ty::Var(uv), body);
                    return Some(uv);
                }
                let dv = self.def_var(def);
                // An environment reference (not a parameter of this binding) is
                // poisoned exactly as in the ident arm — a still-open one blocks
                // generalisation; a ground one is unaffected.
                if !self.is_current_param(def) {
                    self.poison.push(dv);
                }
                Some(dv)
            }
            _ => {
                self.mark_incomplete();
                None
            }
        }
    }

    /// Type a **single-candidate instance method call** `recv.Method(args)` (Stage
    /// 3.3d), returning the call's type variable — the method's **return** type. The
    /// whole-call node is emitted by [`Self::infer_app`]'s callers at the `APP_EXPR`
    /// range (where FCS emits its `call:instance` node), read back only when
    /// [`ground`](Ty::is_ground) — and a *rejected* call's `result` stays open (its
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
    /// [`Ty::Named`] immediately (a static receiver is known at generation, so
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
    fn infer_method_call(&mut self, callee: &Expr, arg: &Expr) -> Option<TyVid> {
        // Snapshot the emission list: everything `method_callee` and the argument
        // walk record inside the call is discarded below (see the doc).
        let emit_mark = self.exprs.len();
        if let Some((path, method_tok)) = self.static_callee(callee) {
            let arg_vids = self.method_arg_vids(arg);
            // The static receiver's type is the rooting entity itself, ground at
            // generation time.
            let recv = self.table.fresh();
            self.eq(Ty::Var(recv), Ty::Named(path));
            let result = self.table.fresh();
            self.has_member(
                recv,
                ident_text(&method_tok),
                result,
                method_tok.text_range(),
                MemberAccessKind::Method {
                    args: arg_vids,
                    is_static: true,
                },
            );
            // Discard any argument sub-expression nodes (the shared method-call
            // rule: a method call emits nothing inside itself).
            self.exprs.truncate(emit_mark);
            return Some(result);
        }
        let Some((recv, method_tok)) = self.method_callee(callee) else {
            self.exprs.truncate(emit_mark);
            self.mark_incomplete();
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
        let arg_vids = self.method_arg_vids(arg);
        // Suspend the method lookup; `result` is the whole call's (return) type.
        // `has_member` poisons `recv` and `result` — an unwoken method relation must
        // not let either generalise (a ground discharge is unaffected).
        let result = self.table.fresh();
        self.has_member(
            recv,
            ident_text(&method_tok),
            result,
            method_tok.text_range(),
            MemberAccessKind::Method {
                args: arg_vids,
                is_static: false,
            },
        );
        // Discard every node emitted inside the call (receiver + argument
        // sub-expressions): a method call emits nothing inside itself (see the doc).
        self.exprs.truncate(emit_mark);
        Some(result)
    }

    /// Recognise a **static-call callee** `Type.Method` (stage OV-7): a
    /// `LONG_IDENT_EXPR` whose **second-to-last** segment carries the resolver's
    /// [`Resolution::Entity`] — the rooting referenced-assembly type — with the
    /// final segment as the method name. Returns the entity's canonical
    /// [`Ty::Named`] path and the method token, or `None` when the callee is not
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
        let Resolution::Entity(handle) = self.resolved.resolution_at(root.text_range())? else {
            return None;
        };
        if self.env.is_module(handle) {
            return None;
        }
        let entity = self.env.entity(handle);
        if self.env.lookup_type(&entity.namespace, &entity.name, 0) != Some(handle) {
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
    fn method_arg_vids(&mut self, arg: &Expr) -> Option<Vec<TyVid>> {
        // Peel exactly the call's own argument parentheses: `M()` is a bare unit
        // const, `M(e)` wraps its argument in one `Paren`, `M e` is a bare argument.
        let inner = match arg {
            _ if is_unit_arg(arg) => return Some(Vec::new()), // `M()`
            // `M(e)` — the call parens; a recovery hole (no inner) is incomplete.
            Expr::Paren(p) => match p.inner() {
                Some(inner) => inner,
                None => {
                    self.mark_incomplete();
                    return None;
                }
            },
            other => other.clone(), // `M e`
        };
        // A unit *inside* the call parens (`M(())`) is one explicit unit argument.
        if is_unit_arg(&inner) {
            return Some(vec![self.walk_arg_element(&inner)]);
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
                    vids.push(self.walk_arg_element(&el));
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
                    self.mark_incomplete();
                    return None;
                }
                Some(vids)
            }
            // A single argument — walk it regardless (poison), then classify: a named
            // argument is non-positional (defer + incomplete); anything else is one
            // positional value.
            _ => {
                let v = self.walk_arg_element(&inner);
                if is_named_arg(&inner) {
                    self.mark_incomplete();
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
    fn walk_arg_element(&mut self, el: &Expr) -> TyVid {
        let expected = self.table.fresh();
        match self.infer_expr(el, Some(expected)) {
            Some(v) => v,
            None => {
                self.mark_incomplete();
                self.table.fresh()
            }
        }
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
    fn method_callee(&mut self, callee: &Expr) -> Option<(TyVid, SyntaxToken)> {
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
                let head_var = self.receiver_var(head)?;
                // Emit the receiver's own value node (coercion-free — synth).
                self.emit(head.text_range(), head_var, None);
                let recv = self.chain_prefix(head_var, prefix)?;
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
                let head_var = self.infer_expr(&recv_expr, None)?;
                let recv = self.chain_prefix(head_var, prefix)?;
                Some((recv, method.clone()))
            }
            Expr::Paren(p) => self.method_callee(&p.inner()?),
            _ => None,
        }
    }

    /// Chain a **data-member prefix** onto a receiver variable, returning the
    /// receiver for the final method segment (Stage 3.3d). An empty prefix (the
    /// common `s.M(…)` shape) passes the receiver through unchanged; a non-empty one
    /// (`s.Field.M(…)`) generates the data-member chain via
    /// [`Self::gen_member_access`].
    fn chain_prefix(&mut self, recv: TyVid, prefix: &[SyntaxToken]) -> Option<TyVid> {
        if prefix.is_empty() {
            Some(recv)
        } else {
            self.gen_member_access(recv, prefix)
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
    ///   [`LongIdent::idents`] does not surface as a plain token.
    ///
    /// The receiver's own value node is emitted at the head token's range (FCS
    /// emits a value node there), coercion-free (synth) — a member access never
    /// coerces its receiver. The whole-access node is emitted at the
    /// `LONG_IDENT_EXPR` range in the access's own mode (`expected`).
    fn infer_long_ident_member(
        &mut self,
        li: &LongIdentExpr,
        expected: Option<TyVid>,
    ) -> Option<TyVid> {
        let Some(long_ident) = li.long_ident() else {
            self.mark_incomplete();
            return None;
        };
        // An active-pattern name segment cannot be projected as a plain member
        // token, so a path carrying one is unmodelled here.
        if long_ident.active_pat_names().next().is_some() {
            self.mark_incomplete();
            return None;
        }
        let idents: Vec<SyntaxToken> = long_ident.idents().collect();
        // Need a head *and* at least one member segment.
        let (head, segments) = match idents.split_first() {
            Some((head, segments)) if !segments.is_empty() => (head, segments),
            _ => {
                self.mark_incomplete();
                return None;
            }
        };
        // The head must resolve to an in-file value binder; otherwise this is a
        // qualified static path (or unresolved) we do not model here.
        let Some(recv) = self.receiver_var(head) else {
            self.mark_incomplete();
            return None;
        };
        // Emit the receiver's own value node (coercion-free — synth).
        self.emit(head.text_range(), recv, None);
        let result = self.gen_member_access(recv, segments)?;
        self.emit(node_span(li.syntax()), result, expected);
        Some(result)
    }

    /// Type a `(expr).Member` member access — a `DOT_GET_EXPR` (Stage 3.3a). The
    /// inner receiver *expression* is synthesized (its own node emitted where FCS
    /// has one — e.g. the `"hi"` literal in `"hi".Length`), and the member path
    /// chains from its variable. Returns the whole access's type, or `None` when
    /// the receiver does not synthesize or the member path is empty/unprojectable.
    fn infer_dot_get(&mut self, dg: &DotGetExpr, expected: Option<TyVid>) -> Option<TyVid> {
        let Some(recv_expr) = dg.expr() else {
            self.mark_incomplete();
            return None;
        };
        let Some(long_ident) = dg.long_ident() else {
            self.mark_incomplete();
            return None;
        };
        if long_ident.active_pat_names().next().is_some() {
            self.mark_incomplete();
            return None;
        }
        let segments: Vec<SyntaxToken> = long_ident.idents().collect();
        if segments.is_empty() {
            self.mark_incomplete();
            return None;
        }
        // The receiver is a coercion-free (synth) sub-position — a member access
        // never coerces its receiver — so synthesize it, emitting its own node.
        let recv = self.infer_expr(&recv_expr, None)?;
        let result = self.gen_member_access(recv, &segments)?;
        self.emit(node_span(dg.syntax()), result, expected);
        Some(result)
    }

    /// The inference variable for a member-access **receiver** whose head token
    /// resolves to an in-file value binder — mirroring [`Self::infer_callee`]'s
    /// ident resolution but for a receiver: a generalised binder instantiates its
    /// scheme afresh; a plain in-file binder contributes its variable (poisoned if
    /// it is an *environment* reference, so a still-open receiver blocks
    /// generalisation). `None` if the head does not resolve to an in-file binder (a
    /// qualified static path, a cross-file/assembly name, or unresolved) — the
    /// caller then defers, leaving any static-member resolution untouched.
    fn receiver_var(&mut self, head: &SyntaxToken) -> Option<TyVid> {
        let def = self.def_at(head.text_range())?;
        if let Some(scheme) = self.def_schemes.get(&def).cloned() {
            let body = self.instantiate(&scheme);
            let uv = self.table.fresh();
            self.eq(Ty::Var(uv), body);
            return Some(uv);
        }
        let dv = self.def_var(def);
        if !self.is_current_param(def) {
            self.poison.push(dv);
        }
        Some(dv)
    }

    /// Whether `dom` (an application's domain variable) resolves to a
    /// **no-subsumption** type, against which a suspended [`Constraint::ArgCheck`]
    /// may discharge as a genuine `Eq(arg, dom)` (Stage 3.3c). A no-subsumption
    /// type is one F# admits **no coercion into**, so relating the argument to it
    /// by equality is exactly what FCS does:
    ///
    /// - a **sealed BCL primitive** — the numeric primitives plus `Boolean`,
    ///   `String`, `Char`, and `Decimal` (the same set [`literal_ty`] produces);
    /// - a **tuple** whose elements are all no-subsumption (recursively);
    /// - an **unbound root that is a scheme-instantiation variable of ours**
    ///   ([`Self::scheme_inst_vars`], checked against union-find roots) — a
    ///   quantified typar of one of our own schemes admits no coercion.
    ///
    /// Deliberately **excluded** (conservative — silence over a wrong type, D5):
    /// `System.Object` (subsumption target for every type — FCS keeps the argument
    /// generic there), arbitrary named types (we cannot cheaply prove sealedness),
    /// arrays, and [`Ty::Fun`] (F#'s function-coercion rules are less clear). Each
    /// leaves the `ArgCheck` undischarged, so the application defers rather than
    /// grounding to a possibly-wrong type. (Extending to sealed/reference named
    /// types where FCS also grounds is a possible follow-up.)
    fn no_subsumption_domain(&mut self, dom: TyVid) -> bool {
        let resolved = self.table.resolve(&Ty::Var(dom));
        self.no_subsumption_ty(&resolved)
    }

    /// The recursive core of [`Self::no_subsumption_domain`], over a **resolved**
    /// type: a sealed primitive, a tuple of such, or an unbound scheme-instantiation
    /// root. Everything else is `false`.
    fn no_subsumption_ty(&mut self, ty: &Ty) -> bool {
        match ty {
            // Only a sealed BCL primitive; `obj` and arbitrary named types are not.
            Ty::Named(path) => is_sealed_primitive(path),
            Ty::Tuple(elems) => elems.iter().all(|e| self.no_subsumption_ty(e)),
            Ty::Var(v) => self.is_scheme_inst_root(*v),
            // Arrays, functions, and quantified `Param`s (never bound into the
            // table) are all excluded.
            Ty::Array { .. } | Ty::Fun { .. } | Ty::Param(_) => false,
        }
    }

    /// Whether the (resolved, unbound) variable `v` is — or is unioned with — a
    /// scheme-instantiation variable ([`Self::scheme_inst_vars`]), checked against
    /// union-find **roots** so a var unioned with an instantiation var still
    /// counts (Stage 3.3c). The set is small (one entry per instantiated typar per
    /// use), so the linear scan is cheap at per-binding scale.
    fn is_scheme_inst_root(&mut self, v: TyVid) -> bool {
        self.scheme_inst_vars
            .iter()
            .any(|&iv| self.table.unioned(iv, v))
    }

    /// Constrain an `if` / `while` condition to `bool`. A condition admits **no**
    /// subsumption — F# requires it to be exactly `bool`, unlike a coercion-
    /// possible `if` branch or function argument — so unifying with `bool` is a
    /// *genuine* equality (not the emission-suppressing check mode). This is the
    /// mechanism that grounds a **simple named function parameter** used directly
    /// as a condition (`let f c = if c then …` ⇒ `c : bool`), feeding that
    /// binder's function type (3.2c-2b).
    ///
    /// It grounds the parameter's **private function-type slot**
    /// ([`Self::param_slots`]), *not* its binder variable — so the `bool` reaches
    /// only the function's `Ty::Fun`, never a standalone parameter read-off. It
    /// fires only for a binder that owns such a slot (a simple named parameter of
    /// the function head), so a condition referencing any *other* binder (an
    /// annotated / `rec` / value binder whose type this stage defers) is left
    /// untouched — grounding it would risk `bool` where FCS, on ill-typed input,
    /// keeps that binder's own type. The condition itself records no expression
    /// node.
    ///
    /// **Completeness (Stage 3.2c-2c).** A condition shape this method *fully*
    /// models — a paren-peeled ident that resolves to a slot-owning parameter of
    /// this binding, or a `bool` literal — leaves the binding complete. Anything
    /// else (a compound `x && y`, a member `p.HasValue`, an ident that is not such
    /// a parameter) drops FCS constraints on the condition's sub-terms, so it marks
    /// the binding **incomplete** — otherwise a function whose only openness came
    /// from an unmodelled condition would wrongly generalise.
    fn constrain_bool(&mut self, cond: Option<Expr>) {
        let Some(cond) = cond else {
            // A missing condition (recovery) is unmodelled.
            self.mark_incomplete();
            return;
        };
        match cond {
            Expr::Ident(ident) => {
                let slot = ident
                    .ident()
                    .and_then(|tok| self.def_at(tok.text_range()))
                    .and_then(|def| self.param_slots.get(&def).copied());
                match slot {
                    Some(slot) => self.eq(Ty::Var(slot), Ty::named("System.Boolean")),
                    // A condition ident that is not a slot-owning parameter of this
                    // binding (a value / annotated / `rec` binder, a cross-file
                    // name) is unmodelled here — its `bool`-ness is an FCS
                    // constraint we drop.
                    None => self.mark_incomplete(),
                }
            }
            // A literal condition is complete only when it is a `bool` literal
            // (`if true then …`) — already `bool`, no constraint needed. Any other
            // literal in condition position is ill-typed input we do not model.
            Expr::Const(c) => {
                if c.literal().map(|l| l.kind()) != Some(SyntaxKind::BOOL_LIT) {
                    self.mark_incomplete();
                }
            }
            Expr::Paren(p) => self.constrain_bool(p.inner()),
            // Any other condition shape (`x && y`, `p.HasValue`, a call) imposes FCS
            // constraints on its sub-terms that we drop.
            _ => self.mark_incomplete(),
        }
    }

    /// Emit a **monomorphic** function type on a `let`-function binder. Curries
    /// the pre-collected parameter variables `arg_vars` over the body's return
    /// variable `ret` into a [`Ty::Fun`] and unifies it with the function's own
    /// inference variable; at read-off it is emitted only if fully
    /// [`ground`](Ty::is_ground), so a polymorphic function (any parameter or the
    /// return still an open variable) silently defers until let-generalisation
    /// (3.2c-2c).
    ///
    /// `arg_vars` come from [`Self::param_var`], collected *before* the body walk
    /// so a simple named parameter's private slot is recorded in time for the
    /// body's condition typing to ground it. Sound by construction: a simple named
    /// parameter contributes a private slot variable (grounded, if at all, only by
    /// condition typing, and read off *only* through this function type); any other
    /// parameter shape (annotated, tuple, wildcard, unit) contributes a fresh
    /// unbound variable, leaving the function type non-ground so it defers.
    ///
    /// Returns the function's `DefId` when it built and constrained the type, so
    /// [`Self::let_binding`] can finalise it ([`Self::finalise_function`]);
    /// `None` on an early-out (a shape this stage does not model), in which case
    /// the binding defers.
    fn function_type(
        &mut self,
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
        let f_def = self.def_at(name.text_range())?;
        // Curry right: `Fun(a0, Fun(a1, … Fun(an, ret)))`, matching FCS.
        let mut fun_ty = Ty::Var(ret);
        for &arg in arg_vars.iter().rev() {
            fun_ty = Ty::Fun {
                arg: Box::new(Ty::Var(arg)),
                ret: Box::new(fun_ty),
            };
        }
        let fv = self.def_var(f_def);
        self.eq(Ty::Var(fv), fun_ty);
        Some(f_def)
    }

    /// The inference variable for a function parameter's slot *inside* the
    /// function type. A simple named (or parenthesised named) parameter gets a
    /// **fresh private slot** variable, recorded in [`Self::param_slots`] under
    /// its `DefId` so condition typing can ground it — but deliberately **not**
    /// its binder's [`def_var`](Self::def_var), so the condition-derived `bool`
    /// never leaks into a standalone parameter read-off (its expression uses or
    /// `def_type`).
    ///
    /// An **annotated** parameter `(x: T)` whose annotation passes the R2-a
    /// gate ([`Self::annotation_ty`]) is a modelled shape too (Stage R2-b): a
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
    /// Any other pattern shape (a non-table annotation, tuple, wildcard, unit,
    /// constructor, or a recovery hole) gets a fresh, unregistered variable —
    /// never ground, so it defers the whole function type — and marks the
    /// binding **incomplete** (Stage 3.2c-2c): its type comes from a shape
    /// this stage does not model, so a function with such a parameter must not
    /// generalise.
    fn param_var(&mut self, pat: &Pat) -> TyVid {
        match pat {
            Pat::Named(named) => {
                let slot = self.table.fresh();
                if let Some(tok) = named.ident()
                    && let Some(def) = self.def_at(tok.text_range())
                {
                    self.param_slots.insert(def, slot);
                    // Track the parameter for (a) the "never published standalone"
                    // exclusion in `finish` and (b) the slot=binder reunification on
                    // a complete binding.
                    self.param_defs.insert(def);
                    self.cur_params.push((def, slot));
                }
                slot
            }
            // Stage R2-b: `(x: T)` with a table annotation is a normal simple
            // parameter whose slot *and* binder are grounded to the
            // annotation's type (see the method docs for why both, eagerly).
            Pat::Typed(typed) => {
                if let Some(t) = typed.ty().and_then(|ty| self.annotation_ty(&ty))
                    && let Some(Pat::Named(named)) = typed.pat()
                    && let Some(tok) = named.ident()
                    && let Some(def) = self.def_at(tok.text_range())
                {
                    let slot = self.table.fresh();
                    self.param_slots.insert(def, slot);
                    self.param_defs.insert(def);
                    self.cur_params.push((def, slot));
                    let dv = self.def_var(def);
                    self.eq(Ty::Var(slot), t.clone());
                    self.eq(Ty::Var(dv), t);
                    slot
                } else {
                    self.mark_incomplete();
                    self.table.fresh()
                }
            }
            Pat::Paren(p) => match p.inner() {
                Some(inner) => self.param_var(&inner),
                None => {
                    self.mark_incomplete();
                    self.table.fresh()
                }
            },
            _ => {
                self.mark_incomplete();
                self.table.fresh()
            }
        }
    }

    /// Record an expression's type variable `v` at `range` for read-off — but
    /// only in *synth* mode (`expected` None), a coercion-free position where the
    /// synthesized type is the elaborated one. In *check* mode the node is
    /// **deferred**: its elaborated type may be a coercion this stage cannot
    /// model yet (no subtype relation), so we say nothing rather than risk a
    /// wrong type (D5), and we add no cross-constraint (which, on an unbound
    /// binder, could otherwise back-flow a coerced expected onto it). The
    /// threaded `expected` type is what subtype-aware checking will later consume
    /// here — emitting the coerced type when the synthesized one is a subtype.
    fn emit(&mut self, range: TextRange, v: TyVid, expected: Option<TyVid>) {
        if expected.is_none() {
            self.exprs.push((range, v));
        }
    }

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
    ///   to a concrete [`Ty::Named`] head, at which point an unambiguous public
    ///   instance data member is looked up ([`Self::wake_member`]) and its type is
    ///   unified with the result (a new `Eq`). A concrete non-`Named` head
    ///   (`Array` / `Tuple` / `Fun` / `Param`) drops the constraint (defer — those
    ///   intrinsic members are out of scope). An unresolved receiver keeps it
    ///   pending.
    /// - **`ArgCheck`** (Stage 3.3c) is *suspended*: it fires once the enclosing
    ///   binding is walk-complete **and** its domain `dom` resolves to a
    ///   [no-subsumption](Self::no_subsumption_domain) type, discharging
    ///   `Eq(arg, dom)`. An `ArgCheck` that never fires, or whose `Eq` fails, is
    ///   **undischarged** and — after the fixpoint — poisons `arg`, `dom`, and the
    ///   application result `r`, so an unwoken arg relation never generalises.
    ///
    /// The loop fixpoints: discharge every pending `Eq`, then scan the suspended
    /// lists — a member/arg wake pushes a new `Eq` (discharged next iteration) and
    /// may ground a *later* suspension's watched variable (a chained member
    /// `s.A.B`, or an `ArgCheck` whose `dom` a prior wake grounds). Each suspension
    /// fires at most once, so termination is by count. Members that never wake are
    /// dropped; `ArgCheck`s that never fire are poisoned (see above).
    fn solve(&mut self) {
        // Split the batch: discharge every eager `Eq` now, park suspensions.
        let mut suspended: Vec<SuspendedMember> = Vec::new();
        let mut suspended_args: Vec<ArgCheck> = Vec::new();
        self.discharge_eqs(&mut suspended, &mut suspended_args);

        // Fixpoint: scan the parked members and arg-checks; wake each whose watched
        // variable is now concrete/ready, discharging any `Eq` its wake produces.
        // Repeat while anything fires. Each suspension fires at most once (a woken
        // one is not re-parked), so the loop runs at most `suspended.len() +
        // suspended_args.len()` times.
        loop {
            let mut still_pending: Vec<SuspendedMember> = Vec::new();
            let mut woke = false;
            for m in std::mem::take(&mut suspended) {
                match self.table.resolve(&Ty::Var(m.recv)) {
                    // Unresolved receiver — keep it parked for a later wake.
                    Ty::Var(_) => still_pending.push(m),
                    // A concrete named receiver — attempt the member lookup (a data
                    // member or a method, per `m.kind`), which may push an
                    // `Eq(result, member_ty)` and record the resolved member's
                    // identity at `use_range` (Stage 3.3b). A `false` return is a
                    // *retry* request (an overload whose argument is not yet ground):
                    // re-park without counting as progress, so a later wake that
                    // grounds the argument re-triggers this one. Termination holds —
                    // a retry sets no `woke`, and the number of progress passes is
                    // bounded by the (finite) suspension count.
                    Ty::Named(path) => {
                        if self.wake_member(&path, &m.name, m.result, m.use_range, &m.kind) {
                            woke = true;
                        } else {
                            still_pending.push(m);
                        }
                    }
                    // Any other concrete head (array / tuple / function / a
                    // quantified param) has no assembly entity to look a member up
                    // on in this stage — drop the constraint (defer, D5). Arrays'
                    // `.Length` is an intrinsic, out of scope.
                    Ty::Array { .. } | Ty::Tuple(_) | Ty::Fun { .. } | Ty::Param(_) => {
                        woke = true;
                    }
                }
            }
            suspended = still_pending;

            // Wake arg-checks whose gate is now met. The completeness gate is a
            // constant across the batch (the walk is finished when `solve` runs), so
            // if the binding is incomplete none ever fires — all fall through to the
            // deferred poison below. The environment guard
            // ([`Self::arg_check_binds_only_current_vars`]) keeps the discharge
            // from retro-grounding an *earlier* binding's still-open variable.
            let mut still_pending_args: Vec<ArgCheck> = Vec::new();
            for ac in std::mem::take(&mut suspended_args) {
                if self.complete
                    && self.no_subsumption_domain(ac.dom)
                    && self.arg_check_binds_only_current_vars(ac)
                {
                    // Fire: discharge the arg↔param relation as a genuine equality,
                    // **immediately** (not via the deferred `Eq` queue) so we learn
                    // the outcome now. On success the check discharged (poison
                    // nothing — the relation is modelled). On failure (ill-typed
                    // code) the atomic rollback leaves no trace, and the check is
                    // *undischarged*, so its `arg`/`dom`/`r` are poisoned. Either
                    // way the check has fired and is not re-parked.
                    if self
                        .table
                        .unify_atomic(&Ty::Var(ac.arg), &Ty::Var(ac.dom))
                        .is_err()
                    {
                        self.poison_arg_check(ac);
                    }
                    woke = true;
                } else {
                    still_pending_args.push(ac);
                }
            }
            suspended_args = still_pending_args;

            // Discharge the `Eq`s a wake produced (grounding the next chain link).
            self.discharge_eqs(&mut suspended, &mut suspended_args);
            // Nothing fired this pass ⇒ no watched variable can become more ready ⇒
            // fixpoint.
            if !woke {
                break;
            }
        }

        // Deferred poison (Stage 3.3c): every arg check still pending never fired
        // (its domain never became no-subsumption, or the binding is incomplete), so
        // its `arg`/`dom`/`r` are poisoned — an undischarged arg relation must not
        // let any of them generalise. (A fired-and-failed check was already poisoned
        // above; a fired-and-succeeded one poisons nothing.)
        for ac in suspended_args {
            self.poison_arg_check(ac);
        }
    }

    /// Poison an **undischarged** arg check's `arg`, `dom`, and result `r` (Stage
    /// 3.3c): never fired, or fired but its `Eq(arg, dom)` failed. A *ground*
    /// endpoint (e.g. a ground application result) is unaffected — poison bites only
    /// open vars — so ground payoffs still emit.
    fn poison_arg_check(&mut self, ac: ArgCheck) {
        self.poison.push(ac.arg);
        self.poison.push(ac.dom);
        self.poison.push(ac.r);
    }

    /// The [`Constraint::ArgCheck`] wake's **environment guard**: whether every
    /// *open* variable the `Eq(arg, dom)` discharge could bind was created in
    /// the **current** binding (no union-find class member older than
    /// [`Self::cur_mark`]). An earlier binder's still-open variable — `g` in
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
    fn arg_check_binds_only_current_vars(&mut self, ac: ArgCheck) -> bool {
        let mut roots: HashSet<TyVid> = HashSet::new();
        let arg = self.table.resolve(&Ty::Var(ac.arg));
        collect_var_roots(&arg, &mut roots);
        let dom = self.table.resolve(&Ty::Var(ac.dom));
        collect_var_roots(&dom, &mut roots);
        roots
            .into_iter()
            .all(|v| !self.table.any_older_unioned(v, self.cur_mark))
    }

    /// Discharge every pending `Eq` in `self.constraints` (best-effort, via
    /// [`InferTable::unify_atomic`]), routing each suspended constraint into its
    /// pending list instead. Shared by [`Self::solve`]'s initial split and its
    /// per-wake re-drain (a wake pushes a fresh `Eq`, and a member/arg wake could
    /// in principle push a fresh suspension).
    fn discharge_eqs(
        &mut self,
        suspended: &mut Vec<SuspendedMember>,
        suspended_args: &mut Vec<ArgCheck>,
    ) {
        for c in std::mem::take(&mut self.constraints) {
            match c {
                Constraint::Eq(a, b) => {
                    let _ = self.table.unify_atomic(&a, &b);
                }
                Constraint::HasMember {
                    recv,
                    name,
                    result,
                    use_range,
                    kind,
                } => {
                    suspended.push(SuspendedMember {
                        recv,
                        name,
                        result,
                        use_range,
                        kind,
                    });
                }
                Constraint::ArgCheck { arg, dom, r } => {
                    suspended_args.push(ArgCheck { arg, dom, r });
                }
            }
        }
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
    fn wake_member(
        &mut self,
        path: &[String],
        name: &str,
        result: TyVid,
        use_range: TextRange,
        kind: &MemberAccessKind,
    ) -> bool {
        let Some((type_name, namespace)) = path.split_last() else {
            return true;
        };
        // The receiver is a non-generic `Ty::Named`, so arity 0. A missing entity is
        // silence. The member lookup below walks this entity's base chain (Stage
        // 3.x-inh), so an inherited member is found and returned under its declaring
        // base's handle.
        let Some(handle) = self.env.lookup_type(namespace, type_name, 0) else {
            return true;
        };
        let looked_up = match kind {
            MemberAccessKind::Data => self.env.instance_data_member(handle, name),
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
                    .map(|v| self.table.resolve(&Ty::Var(*v)))
                    .collect();
                // **Extension-absence gate** (§4.1(4), the P15 landmine): an in-scope
                // extension member of the name would join FCS's method group and
                // could win, so commit *any* intrinsic overload only when no such
                // extension can exist ([`ExtensionScope`]). Unproven ⇒ defer. The
                // gate covers statics too (OV-7): an F# augmentation `type T with
                // static member …` in an opened / auto-opened module joins a
                // type-qualified static group the same way.
                if !self.ext_scope.absent(self.env, name, *is_static) {
                    return true;
                }
                // The provably-complete intrinsic method group (§4.1(1)(2)(3)(5)),
                // instance or static per the call shape (OV-7); an incomplete group
                // (unresolvable base, Object-cap, skipped or kind-clashing member)
                // declines and the call defers.
                let group = if *is_static {
                    self.env.static_method_group(handle, name)
                } else {
                    self.env.instance_method_group(handle, name)
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
                        if !arg_tys.iter().all(Ty::is_ground) {
                            return false;
                        }
                        self.env.resolve_overload(&group, &arg_tys)
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
        // `member_ty_ref` from `self.env` would otherwise conflict with the insert
        // / unify below); `type_ref_to_ty` yields an owned `Ty` or `None`, and the
        // void check reads the `TypeRef` while it is still borrowed.
        let member_ty = type_ref_to_ty(member_ty_ref);
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
        self.member_resolutions.insert(
            use_range,
            Resolution::Member {
                parent: decl_handle,
                idx,
            },
        );
        if is_void {
            return true;
        }
        let Some(member_ty) = member_ty else {
            return true;
        };
        let _ = self.table.unify_atomic(&Ty::Var(result), &member_ty);
        true
    }

    /// Finalise a function binding after its constraints are solved (Stage
    /// 3.2c-2c). Resolve the function's type:
    ///
    /// - **Ground** ⇒ nothing to do; [`Self::finish`] reads it back and emits it
    ///   (the monomorphic path from 3.2c-2b, e.g. `bool -> int`). Ground emission
    ///   needs no completeness/poison check (the subset argument in the module
    ///   docs), so we do not gate it.
    /// - **Open** ⇒ try to [generalise](Self::generalise) it into a scheme, iff the
    ///   binding is walk-complete and every open variable is created-this-binding
    ///   (the `mark`) and unpoisoned. A stored scheme is emitted by `finish`; a use
    ///   [instantiates](Self::instantiate) it. If generalisation is not sound
    ///   (incomplete, an environment/poisoned var), the binder simply stays open →
    ///   silence (D5).
    fn finalise_function(&mut self, f_def: DefId, mark: u32) {
        let fv = self.def_var(f_def);
        let fn_ty = self.table.resolve(&Ty::Var(fv));
        if fn_ty.is_ground() {
            // The monomorphic case: `finish` reads `fv` back and emits it.
            return;
        }
        if !self.complete {
            // An incomplete binding must not generalise (its openness may be an
            // artefact of an unmodelled constraint FCS uses to ground it).
            return;
        }
        if let Some(scheme) = self.generalise(&fn_ty, mark) {
            self.def_schemes.insert(f_def, scheme);
        }
    }

    /// Generalise a resolved, **open** function type into a scheme, or return
    /// `None` to defer (Stage 3.2c-2c). Every open variable in `fn_ty` must be
    /// **quantifiable**: created during this binding (no variable with index
    /// `< mark` is unioned with it — an environment variable inherited from an
    /// earlier binder, whose openness we do not control, must defer this function)
    /// and **not poisoned** (a dropped check-mode relation could have grounded it
    /// in FCS). If all pass, each distinct open root is replaced by a [`Ty::Param`]
    /// numbered by **first appearance** in a depth-first, argument-before-return
    /// walk — matching FCS's canonical numbering — and the substituted body is the
    /// scheme. Parameters are never bound *into* the table, so a `debug_assert`
    /// pins that the resolved `fn_ty` carries none before we introduce them.
    fn generalise(&mut self, fn_ty: &Ty, mark: u32) -> Option<Ty> {
        debug_assert!(
            !contains_param(fn_ty),
            "a resolved fn_ty must not already contain a Ty::Param before generalisation"
        );
        // The poison closure: every variable reachable (after resolution) from a
        // poisoned variable. `resolve` is deep, so one pass reaches a fixpoint.
        let poison = std::mem::take(&mut self.poison);
        let mut poison_roots: HashSet<TyVid> = HashSet::new();
        for v in &poison {
            let resolved = self.table.resolve(&Ty::Var(*v));
            collect_var_roots(&resolved, &mut poison_roots);
        }
        // Restore the poison list (it is per-binding and cleared at the next
        // binding's start anyway, but keep `finalise_function`'s contract clean).
        self.poison = poison;

        // The open roots of `fn_ty`, in first-appearance (DFS) order.
        let mut order: Vec<TyVid> = Vec::new();
        collect_open_order(fn_ty, &mut order);

        // Every open root must be quantifiable, else defer the whole function.
        for &v in &order {
            if poison_roots.contains(&v) || self.table.any_older_unioned(v, mark) {
                return None;
            }
        }

        // Assign `Param(i)` by first appearance, then substitute.
        let mut assign: HashMap<TyVid, u32> = HashMap::new();
        for &v in &order {
            let next = assign.len() as u32;
            assign.entry(v).or_insert(next);
        }
        Some(subst_params(fn_ty, &assign))
    }

    /// Instantiate a scheme body with a **fresh** inference variable per distinct
    /// [`Ty::Param`], memoised within this one instantiation so the same index maps
    /// to the same variable (Stage 3.2c-2c). The fresh variables are `>= ` the
    /// current binding's mark, so a use inside a later function generalises in turn
    /// (`let h x = (id, x)` ⇒ `'a -> ('b -> 'b) * 'a`).
    fn instantiate(&mut self, scheme: &Ty) -> Ty {
        let mut fresh: HashMap<u32, TyVid> = HashMap::new();
        self.instantiate_rec(scheme, &mut fresh)
    }

    fn instantiate_rec(&mut self, ty: &Ty, fresh: &mut HashMap<u32, TyVid>) -> Ty {
        match ty {
            Ty::Param(i) => {
                let v = *fresh.entry(*i).or_insert_with(|| self.table.fresh());
                // Record the provenance: this fresh var is an instantiation of a
                // quantified typar of one of our schemes (Stage 3.3c). Such a typar
                // admits no coercion, so it is a **no-subsumption** domain against
                // which a suspended `ArgCheck` may safely discharge.
                self.scheme_inst_vars.insert(v);
                Ty::Var(v)
            }
            Ty::Named(_) | Ty::Var(_) => ty.clone(),
            Ty::Array { elem, rank } => Ty::Array {
                elem: Box::new(self.instantiate_rec(elem, fresh)),
                rank: *rank,
            },
            Ty::Tuple(elems) => Ty::Tuple(
                elems
                    .iter()
                    .map(|e| self.instantiate_rec(e, fresh))
                    .collect(),
            ),
            Ty::Fun { arg, ret } => Ty::Fun {
                arg: Box::new(self.instantiate_rec(arg, fresh)),
                ret: Box::new(self.instantiate_rec(ret, fresh)),
            },
        }
    }

    /// Read each recorded expression's and binder's type back from its (now
    /// solved) variable, emitting only fully-[`ground`](Ty::is_ground) types — an
    /// unsolved variable becomes silence (D5), never a wrong or meaningless
    /// answer.
    fn finish(mut self) -> InferredFile {
        let exprs = std::mem::take(&mut self.exprs);
        let def_vars = std::mem::take(&mut self.def_vars);
        let mut types = HashMap::new();
        for (range, var) in exprs {
            let ty = self.table.resolve(&Ty::Var(var));
            if ty.is_ground() {
                types.insert(range, ty);
            }
        }
        let mut def_types = HashMap::new();
        for (def, var) in def_vars {
            // A **parameter** is never published standalone (D5). Before 3.2c-2c
            // this held emergently (a parameter's binder var was never ground);
            // the slot=binder reunification on a complete binding can now ground it
            // (or make it a `Param`), so the exclusion is made **explicit** — the
            // parameter's type lives solely *inside* the function's `Ty::Fun`.
            if self.param_defs.contains(&def) {
                continue;
            }
            // A **generalised** binder publishes its scheme (its table variable
            // stays open, since `Param`s are never bound into the table). The
            // scheme was computed at the binding's finalisation, in document order.
            if let Some(scheme) = self.def_schemes.get(&def) {
                def_types.insert(def, scheme.clone());
                continue;
            }
            // Otherwise a value / function binder is emitted only when ground
            // (3.2b-1 values via their RHS, 3.2c-2b monomorphic functions via
            // `Ty::Fun`); an open variable stays silent (D5).
            let ty = self.table.resolve(&Ty::Var(var));
            if ty.is_ground() {
                def_types.insert(def, ty);
            }
        }
        let member_resolutions = std::mem::take(&mut self.member_resolutions);
        InferredFile {
            types,
            def_types,
            member_resolutions,
        }
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

/// Whether `ty` contains a [`Ty::Param`] anywhere. A resolved `fn_ty` must not
/// before generalisation introduces them (the `debug_assert` in
/// [`Gen::generalise`]).
fn contains_param(ty: &Ty) -> bool {
    match ty {
        Ty::Param(_) => true,
        Ty::Named(_) | Ty::Var(_) => false,
        Ty::Array { elem, .. } => contains_param(elem),
        Ty::Tuple(elems) => elems.iter().any(contains_param),
        Ty::Fun { arg, ret } => contains_param(arg) || contains_param(ret),
    }
}

/// Collect the (root) variables of a **resolved** term into `out` — the poison
/// closure step in [`Gen::generalise`]. A resolved term's [`Ty::Var`] leaves are
/// already unbound representatives (roots), so equality-membership in a
/// `HashSet<TyVid>` matches the open roots of `fn_ty`.
fn collect_var_roots(ty: &Ty, out: &mut HashSet<TyVid>) {
    match ty {
        Ty::Var(v) => {
            out.insert(*v);
        }
        Ty::Named(_) | Ty::Param(_) => {}
        Ty::Array { elem, .. } => collect_var_roots(elem, out),
        Ty::Tuple(elems) => elems.iter().for_each(|e| collect_var_roots(e, out)),
        Ty::Fun { arg, ret } => {
            collect_var_roots(arg, out);
            collect_var_roots(ret, out);
        }
    }
}

/// Collect the open (root) variables of a **resolved** `fn_ty` in **first
/// appearance** order under a depth-first, argument-before-return walk — the
/// order [`Gen::generalise`] numbers [`Ty::Param`]s by, matching FCS's canonical
/// typar numbering. Duplicates are dropped (a repeated variable keeps its first
/// position), so `'a -> 'a` numbers its single variable `Param(0)` once.
fn collect_open_order(ty: &Ty, order: &mut Vec<TyVid>) {
    match ty {
        Ty::Var(v) => {
            if !order.contains(v) {
                order.push(*v);
            }
        }
        Ty::Named(_) | Ty::Param(_) => {}
        Ty::Array { elem, .. } => collect_open_order(elem, order),
        Ty::Tuple(elems) => elems.iter().for_each(|e| collect_open_order(e, order)),
        Ty::Fun { arg, ret } => {
            collect_open_order(arg, order);
            collect_open_order(ret, order);
        }
    }
}

/// Substitute each open variable in a **resolved** term by its assigned
/// [`Ty::Param`] index (the generalisation substitution in [`Gen::generalise`]).
/// A variable absent from `assign` cannot occur (every open root of `fn_ty` was
/// assigned), so this is total over the term.
fn subst_params(ty: &Ty, assign: &HashMap<TyVid, u32>) -> Ty {
    match ty {
        Ty::Var(v) => match assign.get(v) {
            Some(&i) => Ty::Param(i),
            None => Ty::Var(*v),
        },
        Ty::Named(_) | Ty::Param(_) => ty.clone(),
        Ty::Array { elem, rank } => Ty::Array {
            elem: Box::new(subst_params(elem, assign)),
            rank: *rank,
        },
        Ty::Tuple(elems) => Ty::Tuple(elems.iter().map(|e| subst_params(e, assign)).collect()),
        Ty::Fun { arg, ret } => Ty::Fun {
            arg: Box::new(subst_params(arg, assign)),
            ret: Box::new(subst_params(ret, assign)),
        },
    }
}

/// Whether this `if`/`elif` chain ends in a genuine `else` value branch, so its
/// result type *is* the then-branch's (and may be synthesized in synth mode).
///
/// `elif`/`else if` desugar to a nested [`IfThenElseExpr`] sitting in the outer's
/// else slot, so `else_branch()` alone is misleading: `if a then 1 elif b then 2`
/// has an immediate else-branch (the nested *else-less* `if b then 2`) yet no
/// *final* `else`. Such a chain gets an implicit trailing `else ()`, so its
/// result is `unit`, not the then-branch's type — emitting the latter would
/// disagree with FCS (D5). Only a chain terminating in a non-`if` else branch may
/// be synthesized. Parentheses are transparent: a parenthesized else-less if
/// (`else (if b then 2)`) is likewise `unit`, so we peel them before recursing.
fn if_chain_has_final_else(if_expr: &IfThenElseExpr) -> bool {
    match if_expr.else_branch().and_then(unparenthesize) {
        // No else at all, an `else`-keyword recovery hole, or a parenthesized
        // recovery hole — none is a final value else, so the chain is `unit`.
        None => false,
        // A trailing `elif`/`else if`: recurse — it is final only if *it* is.
        Some(Expr::IfThenElse(nested)) => if_chain_has_final_else(&nested),
        // Any other else expression is a genuine value branch.
        Some(_) => true,
    }
}

/// The source span FCS reports for `node`: its range with leading and trailing
/// **trivia** (whitespace, comments) trimmed. A node's raw `text_range()` can
/// include trivia the parser attached *inside* the node — e.g. the space after
/// `->` before a lambda body's `if` becomes a leading child of the
/// `IF_THEN_ELSE_EXPR` — which would be off-by-one against FCS's keyword-anchored
/// range and so fail the differential. Falls back to the raw range if the node
/// somehow has no non-trivia token (never for a well-formed construct).
fn node_span(node: &SyntaxNode) -> TextRange {
    let mut toks = node
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia());
    match toks.next() {
        Some(first) => {
            let end = toks
                .last()
                .unwrap_or_else(|| first.clone())
                .text_range()
                .end();
            TextRange::new(first.text_range().start(), end)
        }
        None => node.text_range(),
    }
}

/// The member name an identifier token denotes for assembly lookup — its text
/// with surrounding backticks stripped (`` `Foo Bar` `` → `Foo Bar`), matching
/// FCS's `Ident.idText`. Assembly member names carry no backticks, so a
/// backticked source segment must be de-quoted before it is compared against
/// them. A plain identifier passes through unchanged.
fn ident_text(tok: &SyntaxToken) -> String {
    let text = tok.text();
    text.strip_prefix("``")
        .and_then(|t| t.strip_suffix("``"))
        .unwrap_or(text)
        .to_string()
}

/// Whether an application's callee is a **member access** — the shape a method
/// call `recv.Method(args)` has in function position (Stage 3.3d). A
/// `LONG_IDENT_EXPR` (`s.M`) or `DOT_GET_EXPR` (`"hi".M`) callee routes to method
/// typing; parentheses are transparent. Any other callee (a bare value `f`, a
/// nested application) is an ordinary value application. Pure classification (no
/// side effects), so [`Gen::infer_app`] can commit to the method path only when it
/// applies; [`Gen::method_callee`] then does the resolution and may still defer.
fn is_member_access_callee(e: &Expr) -> bool {
    match e {
        Expr::LongIdent(_) | Expr::DotGet(_) => true,
        Expr::Paren(p) => p
            .inner()
            .is_some_and(|inner| is_member_access_callee(&inner)),
        _ => false,
    }
}

/// Whether an expression is a **unit** literal `()` — a `CONST_EXPR` whose first
/// token is [`SyntaxKind::LPAREN_TOK`], the multi-token `SynConst.Unit` shape the
/// parser produces for a unit method-call argument `s.M()` (its
/// [`ConstExpr::literal`](borzoi_cst::syntax::ConstExpr::literal) returns the
/// `(` token, not `None`). A unit argument has no parameters to poison and is fully
/// modelled, so [`Gen::infer_method_call`] skips walking it (avoiding a spurious
/// walk-incomplete from `literal_ty`'s `None` on `()`).
fn is_unit_arg(e: &Expr) -> bool {
    matches!(e, Expr::Const(c) if c.literal().map(|t| t.kind()) == Some(SyntaxKind::LPAREN_TOK))
}

/// Whether an argument element is a **named argument** `name = value` (Stage 3.3d).
/// F# parses it as `App[ InfixApp[name, "="], value ]` — an outer (non-infix)
/// application whose function is the infix `=` operator applied to the name. A
/// *positional* infix argument (`a + b`, itself an infix `App`) is **not** a named
/// argument (the outer `is_infix()` guard), and neither is a nested `=` inside a
/// record / lambda (which is not the element's own top-level operator). The wake
/// cannot validate the name against the method's parameters, so any named argument
/// defers the whole call (conservative — even correct names defer, but never wrong).
fn is_named_arg(el: &Expr) -> bool {
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

/// Peel transparent parentheses, returning the innermost non-[`Expr::Paren`]
/// expression — or `None` if a `Paren` wraps a recovery hole (no inner
/// expression).
fn unparenthesize(e: Expr) -> Option<Expr> {
    let mut cur = e;
    while let Expr::Paren(p) = cur {
        cur = p.inner()?;
    }
    Some(cur)
}

/// Whether `path` is a **sealed BCL primitive** — the no-subsumption named types
/// against which a Stage-3.3c [`Constraint::ArgCheck`] may discharge
/// ([`Gen::no_subsumption_domain`]). This is exactly the set of scalar primitives
/// [`literal_ty`] produces (the numeric primitives plus `Boolean`, `String`,
/// `Char`, `Decimal`) — every one sealed, so F# admits no coercion *into* it, and
/// relating an argument to it by equality is what FCS does. `System.Object`,
/// `byte[]` (a literal produces an *array*, not a named type here), and any other
/// named type are deliberately absent (subsumption is possible or unproven).
///
/// Kept in lock-step with [`literal_ty`] by a shared list would over-engineer a
/// 16-entry match; instead a unit test asserts the two agree.
fn is_sealed_primitive(path: &[String]) -> bool {
    matches!(
        path.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["System", "Int32"]
            | ["System", "SByte"]
            | ["System", "Byte"]
            | ["System", "Int16"]
            | ["System", "UInt16"]
            | ["System", "UInt32"]
            | ["System", "Int64"]
            | ["System", "UInt64"]
            | ["System", "IntPtr"]
            | ["System", "UIntPtr"]
            | ["System", "Double"]
            | ["System", "Single"]
            | ["System", "Decimal"]
            | ["System", "String"]
            | ["System", "Char"]
            | ["System", "Boolean"]
    )
}

/// The [`Ty`] of a literal whose type is fixed once it is known to sit in a
/// no-expected-type position (the caller guarantees that). Nearly total over
/// literal kinds — the position, not the kind, is what makes it sound — except
/// the few whose type is not fixed even in isolation.
fn literal_ty(c: &ConstExpr) -> Option<Ty> {
    let kind = c.literal()?.kind();
    let prim = |path: &str| Some(Ty::named(path));
    match kind {
        // Integers (default and every explicit suffix).
        SyntaxKind::INT32_LIT => prim("System.Int32"),
        SyntaxKind::SBYTE_LIT => prim("System.SByte"),
        SyntaxKind::BYTE_LIT => prim("System.Byte"),
        SyntaxKind::INT16_LIT => prim("System.Int16"),
        SyntaxKind::UINT16_LIT => prim("System.UInt16"),
        SyntaxKind::UINT32_LIT => prim("System.UInt32"),
        SyntaxKind::INT64_LIT => prim("System.Int64"),
        SyntaxKind::UINT64_LIT => prim("System.UInt64"),
        SyntaxKind::INTPTR_LIT => prim("System.IntPtr"),
        SyntaxKind::UINTPTR_LIT => prim("System.UIntPtr"),
        // Floating point and decimal.
        SyntaxKind::IEEE64_LIT => prim("System.Double"),
        SyntaxKind::IEEE32_LIT => prim("System.Single"),
        SyntaxKind::DECIMAL_LIT => prim("System.Decimal"),
        // Text and characters.
        SyntaxKind::STRING_LIT
        | SyntaxKind::VERBATIM_STRING_LIT
        | SyntaxKind::TRIPLE_STRING_LIT => prim("System.String"),
        SyntaxKind::CHAR_LIT => prim("System.Char"),
        SyntaxKind::BOOL_LIT => prim("System.Boolean"),
        // Byte strings are `byte[]` (the `op_Implicit` to `ReadOnlySpan<byte>`
        // fires at a *use* site, not here — the bound value stays `byte[]`).
        SyntaxKind::BYTE_STRING_LIT
        | SyntaxKind::VERBATIM_BYTE_STRING_LIT
        | SyntaxKind::TRIPLE_BYTE_STRING_LIT => Some(Ty::Array {
            elem: Box::new(Ty::named("System.Byte")),
            rank: 1,
        }),
        // Deferred even in a no-expected-type position: a user-defined numeric
        // literal (`USER_NUM_LIT`, e.g. `1I`/`1G`) resolves through whichever
        // `NumericLiteral` module is in scope, and a source-location identifier
        // (`__LINE__` is int, `__SOURCE_FILE__` is string) has no single type.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! Behaviour tests for 3.2c-2b function-type emission and condition typing,
    //! over our *own* output (the FCS differential lives in
    //! `tests/all/infer_binder_types_diff.rs`). Each resolves and infers a snippet and
    //! reads binder types back by name.

    use std::collections::HashMap;

    use borzoi_cst::parser::parse;
    use borzoi_cst::syntax::{AstNode, ImplFile};

    use super::{ExtensionScope, Gen};
    use crate::resolve::ResolvedFile;
    use crate::ty::Ty;
    use crate::{AssemblyEnv, ProjectItems, resolve_file};

    #[test]
    fn extension_scope_absent_decision_logic() {
        // EX-1: the gate is **name-keyed on the assembly side** and presence-based on
        // the project side. With an empty env (no referenced assembly can contribute
        // anything) and no project source, every name is absent.
        let env = AssemblyEnv::default();
        let extension_free = ExtensionScope::default();
        assert!(
            extension_free.absent(&env, "Substring", false),
            "an extension-free scope over an empty env makes every name absent"
        );
        assert!(
            extension_free.absent(&env, "Compare", true),
            "…for a static call too"
        );

        // A **project** source (an augmentation, an attribute, an auto-open) is
        // still name-blind — the resolver does not yet export what it declares
        // (EX-3) — so it defers wholesale, whatever the name.
        let project_source = ExtensionScope {
            project_source_present: true,
            ..ExtensionScope::default()
        };
        assert!(
            !project_source.absent(&env, "Substring", false),
            "a project extension source defers every name (presence-based, EX-3)"
        );
        assert!(
            !project_source.absent(&env, "NoSourceDeclaresThis", false),
            "…including a name no source declares — that is the remaining coverage cost"
        );

        // An `open` the resolver could not name-key (EX-2: a project open, an
        // assembly-module / `open type`, an opaque path) defers wholesale too —
        // exactly as the pre-EX-2 presence gate did for *any* `open`.
        let unknowable_open = ExtensionScope {
            opens_unknowable: true,
            ..ExtensionScope::default()
        };
        assert!(
            !unknowable_open.absent(&env, "Substring", false),
            "an un-name-keyable open defers every name (presence-based, EX-2)"
        );
        assert!(
            !unknowable_open.absent(&env, "NoSourceDeclaresThis", false),
            "…including a name no source declares"
        );
    }

    /// A synthetic FSharp.Core-shaped [`AssemblyEnv`]: an abbreviation
    /// **marker** for each primitive alias whose `Local` pickled target
    /// chases to a BCL-shaped `System.*` entity in the same synthetic
    /// assembly — the exact runtime mechanism (markers + the target chase),
    /// hermetically, with no DLL on disk. The identity name `FSharp.Core`
    /// plus the `Microsoft.FSharp.Core` auto-open make bare `int`/`bool`
    /// annotation heads resolve the way a real project's do.
    fn primitive_env() -> AssemblyEnv {
        use borzoi_assembly::{AbbreviationTarget, EntityKind};
        let pairs: &[(&str, &str)] = &[
            ("bool", "Boolean"),
            ("char", "Char"),
            ("sbyte", "SByte"),
            ("byte", "Byte"),
            ("int16", "Int16"),
            ("uint16", "UInt16"),
            ("int", "Int32"),
            ("int32", "Int32"),
            ("uint", "UInt32"),
            ("int64", "Int64"),
            ("uint64", "UInt64"),
            ("float32", "Single"),
            ("float", "Double"),
            ("nativeint", "IntPtr"),
            ("unativeint", "UIntPtr"),
            ("obj", "Object"),
            ("string", "String"),
            ("decimal", "Decimal"),
        ];
        let mut roots: Vec<borzoi_assembly::Entity> = Vec::new();
        let mut bcl_done = std::collections::HashSet::new();
        for (alias, target) in pairs {
            let mut marker = synthetic_entity(
                &["Microsoft", "FSharp", "Core"],
                alias,
                EntityKind::Abbreviation,
            );
            marker.abbreviation_target = Some(AbbreviationTarget::Named {
                ccu: None,
                path: vec!["System".to_string(), (*target).to_string()],
                args: Vec::new(),
            });
            roots.push(marker);
            if bcl_done.insert(*target) {
                let kind = if *target == "Object" || *target == "String" {
                    EntityKind::Class
                } else {
                    EntityKind::Struct
                };
                roots.push(synthetic_entity(&["System"], target, kind));
            }
        }
        AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
            std::path::PathBuf::from("FSharp.Core.dll"),
            roots,
            crate::AbbreviationVisibility::Modelled,
            vec!["Microsoft.FSharp.Core".to_string()],
        )])
    }

    fn synthetic_entity(
        ns: &[&str],
        name: &str,
        kind: borzoi_assembly::EntityKind,
    ) -> borzoi_assembly::Entity {
        use borzoi_assembly::{Access, AssemblyIdentity, Entity, Version};
        Entity {
            assembly: AssemblyIdentity {
                name: "FSharp.Core".to_string(),
                version: Version {
                    major: 0,
                    minor: 0,
                    build: 0,
                    revision: 0,
                },
                public_key_token: None,
            },
            namespace: ns.iter().map(|s| (*s).to_string()).collect(),
            name: name.to_string(),
            kind,
            access: Access::Public,
            is_sealed: false,
            generic_parameters: vec![],
            base_type: None,
            interfaces: vec![],
            members: vec![],
            skipped_members: vec![],
            method_def_tokens: vec![],
            nested_types: vec![],
            is_readonly: false,
            is_byref_like: false,
            is_struct: false,
            is_auto_open: false,
            is_require_qualified_access: false,
            is_no_equality: false,
            is_no_comparison: false,
            is_structural_equality: false,
            is_structural_comparison: false,
            is_allow_null_literal: false,
            obsolete: None,
            experimental: None,
            default_member: None,
            compiler_feature_required: vec![],
            source_name: None,
            extension_member_names: vec![],
            union_case_names: None,
            static_extension_member_names: Vec::new(),
            is_extension_container: false,
            custom_attrs: vec![],
            abbreviation_target: None,
            definition_range: None,
        }
    }

    /// Infer `src` (single-file), returning each binder's rendered (canonical)
    /// type keyed by name. Snippet names are unique, so a plain map suffices.
    /// The env is [`primitive_env`] so annotated snippets (`let x : int = …`)
    /// type through the marker chase like a real project's would.
    fn def_types(src: &str) -> HashMap<String, String> {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let env = primitive_env();
        let resolved = resolve_file(&file, &ProjectItems::default(), &env);
        let inferred = super::infer_file(&file, &resolved, &env);
        inferred
            .def_types()
            .iter()
            .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
            .collect()
    }

    /// Infer `src`, returning the canonical renders of every *expression*-node
    /// type (the `types()` map), for tests that a condition-derived `bool` never
    /// leaks into an expression read-off.
    fn expr_type_renders(src: &str) -> Vec<String> {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let env = primitive_env();
        let resolved = resolve_file(&file, &ProjectItems::default(), &env);
        let inferred = super::infer_file(&file, &resolved, &env);
        inferred.types().values().map(super::Ty::render).collect()
    }

    #[test]
    fn monomorphic_function_types_via_condition() {
        // `c` is grounded to bool by the condition; the body returns int; so the
        // function is `bool -> int`, published on `f`. The parameter `c` is *not*
        // published on its own — its type lives inside the function type.
        let types = def_types("module M\nlet f c = if c then 1 else 2\n");
        assert_eq!(
            types.get("f").map(String::as_str),
            Some("System.Boolean -> System.Int32")
        );
        assert_eq!(
            types.get("c"),
            None,
            "parameter types are not published standalone"
        );
    }

    #[test]
    fn curried_function_type_right_associates() {
        // Two parameters, both grounded to bool by conditions; nested body → int.
        let types = def_types("module M\nlet f a b = if a then (if b then 1 else 2) else 3\n");
        assert_eq!(
            types.get("f").map(String::as_str),
            Some("System.Boolean -> System.Boolean -> System.Int32")
        );
    }

    #[test]
    fn polymorphic_identity_generalises() {
        // `let f x = x` generalises to `'a -> 'a` (Stage 3.2c-2c): the binding is
        // walk-complete (a bare parameter use), `x`'s variable is created this
        // binding and unpoisoned, so it is quantified. The parameter `x` is still
        // never published standalone. (Flipped from the pre-generalisation
        // `polymorphic_function_defers`, which pinned that `let f x = x` deferred.)
        let types = def_types("module M\nlet f x = x\n");
        assert_eq!(types.get("f").map(String::as_str), Some("'a -> 'a"));
        assert_eq!(types.get("x"), None, "the parameter is never published");
    }

    #[test]
    fn unused_parameter_generalises_to_ground_return() {
        // `let f x = 42` generalises to `'a -> int`: the unused parameter is
        // quantified, the return is the literal's ground `int`. (Flipped from the
        // pre-generalisation `unused_parameter_defers_function`.)
        let types = def_types("module M\nlet f x = 42\n");
        assert_eq!(
            types.get("f").map(String::as_str),
            Some("'a -> System.Int32")
        );
        assert_eq!(types.get("x"), None);
    }

    #[test]
    fn annotated_parameter_types_the_function() {
        // Stage R2-b: a table-annotated parameter `(c: bool)` grounds its slot
        // *and* binder from the annotation (a parameter annotation is exact in
        // F# — subsumption applies at call sites, not the binder), so the
        // function types `bool -> int`. The parameter is still never published
        // standalone. (Flipped from the pre-R2-b
        // `annotated_parameter_defers_function_and_param`.)
        let types = def_types("module M\nlet f (c: bool) = if c then 1 else 2\n");
        assert_eq!(
            types.get("f").map(String::as_str),
            Some("System.Boolean -> System.Int32")
        );
        assert_eq!(
            types.get("c"),
            None,
            "parameters are never published standalone"
        );
    }

    #[test]
    fn non_table_annotated_parameter_still_defers() {
        // A parameter annotation outside the R2-a gate (an unknown name, a
        // generic application) keeps today's mark-incomplete catch-all: the
        // function defers, and the condition must not ground the parameter
        // (FCS would keep the annotation's type and report the condition
        // error).
        for src in [
            "module M\nlet f (c: MyBool) = if c then 1 else 2\n",
            "module M\nlet f (c: bool option) = if c then 1 else 2\n",
        ] {
            let types = def_types(src);
            assert_eq!(types.get("f"), None, "{src:?}");
            assert_eq!(types.get("c"), None, "{src:?}");
        }
    }

    #[test]
    fn ill_typed_condition_keeps_the_parameter_annotation() {
        // `let f (c: int) = if c then 1 else 2` is ill-typed at the condition;
        // FCS keeps `c : int` (the annotation is exact) and `f : int -> int`.
        // The annotation's `Eq(slot, int)` lands first in generation order; the
        // condition's later `Eq(slot, bool)` fails and rolls back whole.
        let src = "module M\nlet f (c: int) = if c then 1 else 2\n";
        let types = def_types(src);
        assert_eq!(
            types.get("f").map(String::as_str),
            Some("System.Int32 -> System.Int32")
        );
        assert!(
            !expr_type_renders(src).contains(&"System.Boolean".to_string()),
            "no condition-derived bool may survive the annotation: {:?}",
            expr_type_renders(src)
        );
    }

    #[test]
    fn annotated_param_beats_a_conflicting_arg_check_wake() {
        // The 3.3c hazard shape, now with the annotation *modelled* (R2-b):
        // `let h (y: string) = (y, fi y)` with in-file `fi : int -> int64`
        // (itself R2-b-annotated). FCS keeps `y : string` (the annotation is
        // exact on the binder; the application is the error site) and
        // `h : string -> string * int64` (the result is fixed by `fi`'s own
        // shape regardless of the bad argument). Ours agrees: the annotation
        // grounds `y` before the wake, so the wake's `Eq(y, int)` fails and
        // rolls back; its poison hits only the already-ground result, so `h`
        // still emits — and crucially no `int` reaches `y`'s synth-mode tuple
        // use (`7L` is the only Int64; nothing emits Int32).
        let src = "module M\nlet fi (n: int) = 7L\nlet h (y: string) = (y, fi y)\n";
        let types = def_types(src);
        assert_eq!(
            types.get("fi").map(String::as_str),
            Some("System.Int32 -> System.Int64")
        );
        assert_eq!(
            types.get("h").map(String::as_str),
            Some("System.String -> System.String * System.Int64")
        );
        let exprs = expr_type_renders(src);
        assert!(
            exprs.contains(&"System.String".to_string()),
            "y's synth-mode tuple use emits the annotation type: {exprs:?}"
        );
        assert!(
            !exprs.contains(&"System.Int32".to_string()),
            "no wake-derived int may reach y's use: {exprs:?}"
        );
    }

    // ========================================================================
    // Stage R2-c — function return-type annotations
    // ========================================================================

    #[test]
    fn return_annotation_grounds_parameter_through_body() {
        // `let h x : int = x`: the sealed `int` annotation discharges the
        // body↔annotation relation as a genuine equality (the ArgCheck wake's
        // own no-subsumption judgment), grounding `x` through its body use.
        let types = def_types("module M\nlet h x : int = x\n");
        assert_eq!(
            types.get("h").map(String::as_str),
            Some("System.Int32 -> System.Int32")
        );
        assert_eq!(types.get("x"), None, "parameters stay unpublished");
    }

    #[test]
    fn subsumption_return_annotation_defers_the_open_parameter() {
        // `let f x : obj = x` is legal via subsumption; the dropped relation
        // must not ground `x` (FCS: `obj -> obj`). The undischarged check
        // poisons `x`, so `f` defers — silence, never `'a -> obj`.
        let types = def_types("module M\nlet f x : obj = x\n");
        assert_eq!(types.get("f"), None);
        // With a ground parameter side the annotated return still emits.
        let ground = def_types("module M\nlet f (b: bool) : obj = b\n");
        assert_eq!(
            ground.get("f").map(String::as_str),
            Some("System.Boolean -> System.Object")
        );
    }

    #[test]
    fn return_annotation_body_emits_no_expression_nodes() {
        // The body is walked in check mode: `let g (x: int) : string = "s"`
        // records no node for the literal (its elaborated type is the
        // annotation's business), while the function still types.
        let src = "module M\nlet g (x: int) : string = \"s\"\n";
        let types = def_types(src);
        assert_eq!(
            types.get("g").map(String::as_str),
            Some("System.Int32 -> System.String")
        );
        assert!(
            expr_type_renders(src).is_empty(),
            "an annotated function body emits no nodes: {:?}",
            expr_type_renders(src)
        );
    }

    #[test]
    fn non_table_return_annotation_keeps_the_skip() {
        // An annotation outside the gate (generic app / unknown name) keeps
        // the pre-R2-c whole-binding skip: nothing emitted anywhere.
        for src in [
            "module M\nlet f x : int option = None\n",
            "module M\nlet f x : MyInt = x\n",
        ] {
            let types = def_types(src);
            assert_eq!(types.get("f"), None, "{src:?}");
            assert!(expr_type_renders(src).is_empty(), "{src:?}");
        }
    }

    #[test]
    fn ill_typed_body_keeps_the_return_annotation() {
        // `let f x : int = "s"` errors at the body; FCS keeps `f : 'a -> int`.
        // The failed discharge rolls back whole and poisons only its own
        // (ground) endpoints, so the unused parameter still quantifies.
        let types = def_types("module M\nlet f x : int = \"s\"\n");
        assert_eq!(
            types.get("f").map(String::as_str),
            Some("'a -> System.Int32")
        );
    }

    #[test]
    fn deferred_binder_used_as_condition_is_not_bool() {
        // Condition typing must not retype a binder through a condition use. On
        // the ill-typed (mid-edit) `let x : int = 1 \n if x then …`, FCS keeps
        // `x : int` and reports the condition mismatch; grounding `x` to `bool`
        // would be a wrong hover (D5). Since Stage R2-a, `x` is typed `int`
        // from its annotation — the condition's failed `Eq` rolls back and the
        // annotation type stands. (Flipped from the pre-R2-a pin that `x`
        // stayed untyped.)
        let types = def_types("module M\nlet x : int = 1\nlet r = if x then 0 else 0\n");
        assert_eq!(
            types.get("x").map(String::as_str),
            Some("System.Int32"),
            "an annotated value used as a condition keeps its annotation type"
        );
        // A `rec` binder is deferred as a group and not condition-typed.
        let rec_types = def_types("module M\nlet rec x = 1\nand r = if x then 0 else 0\n");
        assert_eq!(
            rec_types.get("x"),
            None,
            "a rec binder is not condition-typed"
        );
    }

    // ========================================================================
    // Stage R2-a — annotated value binders (docs/completed/r2-annotation-typing-plan.md)
    // ========================================================================

    #[test]
    fn annotated_value_binder_types_from_annotation() {
        // The annotation types the binder; the RHS is never walked, so the
        // literal node stays absent from the expression map.
        let src = "module M\nlet x : int64 = 42\n";
        let types = def_types(src);
        assert_eq!(types.get("x").map(String::as_str), Some("System.Int64"));
        assert!(
            expr_type_renders(src).is_empty(),
            "the annotated binding's RHS is not walked"
        );
    }

    #[test]
    fn annotated_binder_type_flows_to_uses() {
        // The binder's ground type propagates to a use through the existing
        // def-var machinery: `let y = x` is `Int64` too.
        let types = def_types("module M\nlet x : int64 = 42L\nlet y = x\n");
        assert_eq!(types.get("x").map(String::as_str), Some("System.Int64"));
        assert_eq!(types.get("y").map(String::as_str), Some("System.Int64"));
    }

    #[test]
    fn trivial_typed_pattern_head_types_from_annotation() {
        // `let (x : int64) = 42` — the `Paren > Typed > Named` head rides the
        // same gate; the RHS is likewise not walked.
        let src = "module M\nlet (x : int64) = 42\n";
        let types = def_types(src);
        assert_eq!(types.get("x").map(String::as_str), Some("System.Int64"));
        assert!(expr_type_renders(src).is_empty());
    }

    #[test]
    fn structural_annotations_recurse_through_table_leaves() {
        let types = def_types(
            "module M\nlet a : int * string = (1, \"s\")\nlet f : int -> int = fun x -> x\nlet arr : int[] = [| 1; 2 |]\n",
        );
        assert_eq!(
            types.get("a").map(String::as_str),
            Some("System.Int32 * System.String")
        );
        assert_eq!(
            types.get("f").map(String::as_str),
            Some("System.Int32 -> System.Int32")
        );
        assert_eq!(types.get("arr").map(String::as_str), Some("System.Int32[]"));
    }

    #[test]
    fn annotation_defer_shapes_stay_silent() {
        for (src, why) in [
            // An in-file `type int64` shadows the primitive: the gate sees a
            // concrete `Local` record at the head.
            (
                "module M\ntype int64 = A of int\nlet x : int64 = A 1\n",
                "in-file type shadow",
            ),
            // Generic application (`int64 option`) is a non-bare head.
            ("module M\nlet x : int64 option = None\n", "generic app"),
            // `unit` is deliberately excluded from the v1 table.
            ("module M\nlet x : unit = ()\n", "unit excluded"),
            // A struct tuple is not `Ty::Tuple`.
            (
                "module M\nlet x : struct (int * int) = struct (1, 2)\n",
                "struct tuple",
            ),
            // Mutable binders are out of scope in v1.
            ("module M\nlet mutable x : int64 = 42L\n", "mutable binder"),
            // A non-trivial annotated pattern (tuple head) stays with the
            // pattern catch-all.
            (
                "module M\nlet ((a, b) : int * int) = (1, 2)\n",
                "non-trivial typed pattern",
            ),
            // `module rec`: the R2-0 marker defers the earlier annotation.
            (
                "module rec M\nlet x : int64 = A\ntype int64 = A\n",
                "rec-module forward shadow",
            ),
        ] {
            let types = def_types(src);
            assert_eq!(types.get("x"), None, "{why}: {src:?}");
            assert_eq!(types.get("a"), None, "{why}: {src:?}");
            assert_eq!(types.get("b"), None, "{why}: {src:?}");
        }
        // A multi-segment head whose tail the resolver concretely resolves is
        // NOT a defer shape: `System.Int64` bridges through the R2-d entity
        // record (the real-BCL differential `infer_annotation_entity_diff`
        // pins the same behaviour against System.Runtime).
        let types = def_types("module M\nlet x : System.Int64 = 42L\n");
        assert_eq!(types.get("x").map(String::as_str), Some("System.Int64"));
    }

    #[test]
    fn param_with_unmodelled_ascription_used_as_condition_is_not_bool() {
        // The parameter `c` carries an unmodelled ascription `(c : int)` in the
        // body, is *used as a value* (the first tuple element), *and* is used as a
        // condition. Condition typing grounds only `c`'s private function-type slot
        // (not its binder var), so:
        //   - the standalone parameter type is never published (`def_type`);
        //   - the value use `c` reads its binder var, which stays un-ground, so no
        //     `bool` leaks into the expression `types()` map either;
        //   - the ascription element leaves the tuple un-ground, so the function
        //     defers.
        // FCS keeps `c : int` and reports the condition error, so publishing `bool`
        // anywhere would be wrong (D5).
        let src = "module M\nlet f c = (c, (c : int), if c then 1 else 2)\n";
        let types = def_types(src);
        assert_eq!(types.get("f"), None, "the non-ground function defers");
        assert_eq!(types.get("c"), None, "the parameter is never published");
        assert!(
            !expr_type_renders(src).contains(&"System.Boolean".to_string()),
            "no condition-derived bool may leak into the expression map: {:?}",
            expr_type_renders(src)
        );
    }

    #[test]
    fn plain_value_used_as_condition_keeps_its_rhs_type() {
        // A plain value used as a condition is not condition-groundable either, but
        // loses no coverage: it is already typed by its (bool) RHS. `b : bool`
        // comes from `let b = true`, not from the condition.
        let types = def_types("module M\nlet b = true\nlet r = if b then 0 else 0\n");
        assert_eq!(types.get("b").map(String::as_str), Some("System.Boolean"));
    }

    proptest::proptest! {
        /// For any two same-suffixed integer literals, `let f c = if c then <a>
        /// else <b>` is `bool -> <that primitive>`: the condition grounds the
        /// parameter to `bool` (inside the function type) and the then-branch's
        /// literal is the return, for every suffix/value — the curried function
        /// type is built consistently. The parameter `c` is not published on its
        /// own.
        #[test]
        fn condition_grounded_function_is_bool_to_literal(
            // Bounded to the byte range so every suffix (incl. `uy`) is in range.
            a in 0u32..=255,
            b in 0u32..=255,
            sfx_idx in 0usize..3,
        ) {
            let (sfx, prim) = [("", "System.Int32"), ("L", "System.Int64"), ("uy", "System.Byte")][sfx_idx];
            let src = format!("module M\nlet f c = if c then {a}{sfx} else {b}{sfx}\n");
            let types = def_types(&src);
            let want = format!("System.Boolean -> {prim}");
            proptest::prop_assert_eq!(
                types.get("f").map(String::as_str),
                Some(want.as_str()),
                "src={:?}", src
            );
            proptest::prop_assert_eq!(types.get("c"), None);
        }
    }

    // ===== Stage 3.2c-2c — generalisation behaviour tests =====

    #[test]
    fn constant_function_generalises_over_unused_second_param() {
        // `let k a b = a` ⇒ `'a -> 'b -> 'a`: both parameters quantified, the
        // return the first parameter's variable — reused, so it keeps `'a`.
        let types = def_types("module M\nlet k a b = a\n");
        assert_eq!(types.get("k").map(String::as_str), Some("'a -> 'b -> 'a"));
    }

    #[test]
    fn swap_tuple_function_generalises() {
        // `let f a b = (b, a)` ⇒ `'a -> 'b -> 'b * 'a`: the parameters number by
        // head order, the return tuple by first appearance (`b` then `a`).
        let types = def_types("module M\nlet f a b = (b, a)\n");
        assert_eq!(
            types.get("f").map(String::as_str),
            Some("'a -> 'b -> 'b * 'a")
        );
    }

    #[test]
    fn mixed_ground_and_param_generalises() {
        // `let f c x = ((if c then 1 else 2), x)` ⇒ `bool -> 'a -> int * 'a`: `c`
        // grounds to bool via the condition, the tuple's first element is the
        // ground `int` if-result, `x` is quantified.
        let types = def_types("module M\nlet f c x = ((if c then 1 else 2), x)\n");
        assert_eq!(
            types.get("f").map(String::as_str),
            Some("System.Boolean -> 'a -> System.Int32 * 'a")
        );
    }

    #[test]
    fn slot_binder_reunification_lets_a_param_use_ground() {
        // `let f x = if x then (1, x) else (2, x)` ⇒ `bool -> int * bool`: the
        // condition grounds `x`'s slot to bool, the slot=binder reunification (a
        // *complete* binding) flows that to `x`'s binder var, so the tuple's `x`
        // element is `bool` and the function is fully ground. This is the payoff
        // 2b had to defer.
        let types = def_types("module M\nlet f x = if x then (1, x) else (2, x)\n");
        assert_eq!(
            types.get("f").map(String::as_str),
            Some("System.Boolean -> System.Int32 * System.Boolean")
        );
    }

    #[test]
    fn incomplete_body_blocks_generalisation() {
        // `let f x = x + 1`: the `+` application is unmodelled, so the binding is
        // incomplete and must NOT generalise — even though `x`'s variable is open.
        // FCS grounds `x` to int through `+` (a constraint we drop), so emitting
        // `'a -> 'b` would be wrong (D5). We defer.
        let types = def_types("module M\nlet f x = x + 1\n");
        assert_eq!(
            types.get("f"),
            None,
            "an unmodelled body must block generalisation"
        );
    }

    #[test]
    fn poisoned_param_via_dropped_else_relation_defers() {
        // `let f c x = if c then x else x`: the else-branch `x` is checked against
        // the then-branch `x` (a dropped check-mode relation), poisoning `x`. FCS
        // relates the two through that relation; we drop it, so `x` must not
        // generalise. The binding defers.
        let types = def_types("module M\nlet f c x = if c then x else x\n");
        assert_eq!(
            types.get("f"),
            None,
            "a poisoned parameter must block generalisation"
        );

        // `let f b x = if b then (x, 1) else (x, 2)`: the else-tuple is checked
        // against the then-tuple, poisoning `x` (through the tuple's component
        // relation). FCS relates `x` across the branches; we defer.
        let types2 = def_types("module M\nlet f b x = if b then (x, 1) else (x, 2)\n");
        assert_eq!(
            types2.get("f"),
            None,
            "a component-poisoned parameter must block generalisation"
        );
    }

    #[test]
    fn compound_condition_marks_incomplete_and_defers() {
        // `let f x y = if x && y then 1 else 2`: the condition `x && y` is a
        // compound this stage does not model (it imposes FCS constraints on `x`,
        // `y` we drop), so the binding is incomplete and defers — even though the
        // return is ground.
        let types = def_types("module M\nlet f x y = if x && y then 1 else 2\n");
        assert_eq!(
            types.get("f"),
            None,
            "a compound condition marks the binding incomplete"
        );
    }

    #[test]
    fn skipped_earlier_binder_reference_defers() {
        // A later function referencing an *earlier binder inference skipped* must
        // not quantify it — FCS knows its type through a constraint we did not
        // model, so a bogus scheme would be a D5 violation. Skips: an annotated
        // binder whose annotation is outside the R2-a table subset
        // (`let a : int option = None` — a generic application defers), a
        // `let rec` group, and a tuple-pattern binding. Each leaves the
        // reference open; the function must defer.
        for src in [
            // Annotated value whose annotation R2-a's gate defers (generic app).
            "module M\nlet a : int option = None\nlet h x = (x, a)\n",
            // `let rec` group skipped whole.
            "module M\nlet rec a = 1\nlet h x = (x, a)\n",
            // Tuple-pattern binding (not a `Pat::Named`), skipped via the `_` arm.
            "module M\nlet (a, b) = (1, 2)\nlet h x = (x, a)\n",
        ] {
            let types = def_types(src);
            assert_eq!(
                types.get("h"),
                None,
                "a reference to a skipped earlier binder must defer the function: {src:?}"
            );
        }
        // The flip side (Stage R2-a): a *table*-annotated earlier binder is
        // ground, so the same shape generalises over it — exactly like a
        // literal-typed environment reference.
        let types = def_types("module M\nlet a : int = 1\nlet h x = (x, a)\n");
        assert_eq!(
            types.get("h").map(String::as_str),
            Some("'a -> 'a * System.Int32"),
            "a ground annotated environment reference must not block generalisation"
        );
    }

    #[test]
    fn stale_environment_var_defers_via_the_mark() {
        // `let a = f 0` (an unmodelled application) leaves `a` open (an environment
        // var created in an *earlier* binding). `let g x = (x, a)`: `a`'s variable
        // has index < `g`'s mark, so `g` cannot generalise over it — it defers
        // rather than quantifying an inherited-open variable. (`x` alone would
        // generalise; `a` is what blocks it.)
        let types = def_types("module M\nlet a = f 0\nlet g x = (x, a)\n");
        assert_eq!(
            types.get("g"),
            None,
            "an inherited-open environment var must defer the function"
        );
    }

    #[test]
    fn value_binding_id_alias_stays_silent() {
        // `let g = id` (a value binding of a generalised function) is rejected by
        // F#'s value restriction (FS0030); we stay silent — the instantiation of
        // `id`'s scheme leaves `g` open, so nothing is published. `id` itself
        // generalises to `'a -> 'a`.
        let types = def_types("module M\nlet id x = x\nlet g = id\n");
        assert_eq!(types.get("id").map(String::as_str), Some("'a -> 'a"));
        assert_eq!(types.get("g"), None, "value restriction: g stays silent");
    }

    #[test]
    fn wake_must_not_ground_an_earlier_open_binder() {
        // `let g = id` leaves `g` carrying id's *instantiation* variables,
        // created in g's own (earlier) binding. A later application `g 1` must
        // NOT discharge its ArgCheck against that environment variable: FCS
        // keeps `g : 'a -> 'a` (fcs-dump probe, 2026-07-08 — F# generalises
        // `let g = id`, contra the FS0030 shorthand in the sibling test's
        // comment), so grounding `g` to `int -> int` through the wake and
        // publishing it — as happened before the env guard — is a wrong type,
        // not a missed one. The wake's mark gate blocks it; `g`, the `id` use
        // node, and (in the unannotated shape) `n` all stay silent.
        for src in [
            "module M\nlet id x = x\nlet g = id\nlet n = g 1\n",
            "module M\nlet id x = x\nlet g = id\nlet n : int = g 1\n",
        ] {
            let types = def_types(src);
            assert_eq!(types.get("id").map(String::as_str), Some("'a -> 'a"));
            assert_eq!(types.get("g"), None, "g must stay silent: {src:?}");
            assert!(
                !expr_type_renders(src).contains(&"System.Int32 -> System.Int32".to_string()),
                "no monomorphised type may leak onto the `id` use node: {src:?}"
            );
        }
        // The annotated shape still types `n` from its annotation.
        let annotated = def_types("module M\nlet id x = x\nlet g = id\nlet n : int = g 1\n");
        assert_eq!(annotated.get("n").map(String::as_str), Some("System.Int32"));
    }

    #[test]
    fn wake_must_not_ground_an_earlier_open_argument() {
        // The argument-side twin: `let a = h 0` leaves `a` open (unmodelled
        // application). `let n = fb a` (in-file `fb : bool -> int`) must not
        // ground `a := bool` through the wake — `a`'s type is fixed by its own
        // (unmodelled) binding, and FCS resolves it there, not here.
        let src = "module M\nlet fb b = if b then 1 else 2\nlet a = h 0\nlet n = fb a\n";
        let types = def_types(src);
        assert_eq!(types.get("a"), None, "the earlier open binder stays silent");
    }

    #[test]
    fn nested_instantiation_generalises_in_turn() {
        // `let h x = (id, x)` (with `id : 'a -> 'a` in scope) ⇒
        // `'a -> ('b -> 'b) * 'a`: the use of `id` instantiates its scheme to a
        // *fresh* variable pair (distinct from `h`'s own `'a`), which `h`
        // generalises in turn — the instantiation vars are ≥ `h`'s mark.
        let types = def_types("module M\nlet id x = x\nlet h x = (id, x)\n");
        assert_eq!(
            types.get("h").map(String::as_str),
            Some("'a -> ('b -> 'b) * 'a")
        );
    }

    #[test]
    fn ascription_regression_still_defers_unchanged() {
        // The 2b decoupling regression (an unmodelled ascription used as a
        // condition) must stay deferred — it is an *incomplete* binding shape, so
        // generalisation never fires. This pins that 3.2c-2c did not regress #701.
        let src = "module M\nlet f c = (c, (c : int), if c then 1 else 2)\n";
        let types = def_types(src);
        assert_eq!(types.get("f"), None, "the ascription binding still defers");
        assert_eq!(types.get("c"), None, "the parameter is never published");
        assert!(
            !expr_type_renders(src).contains(&"System.Boolean".to_string()),
            "no condition-derived bool may leak: {:?}",
            expr_type_renders(src)
        );
    }

    #[test]
    fn expression_level_let_in_does_not_nest_as_let_decl() {
        // A binding whose RHS contains an expression-level `let … in` must not
        // trip the sequential-solve `debug_assert` in `let_binding` — that let is a
        // `LET_OR_USE_EXPR`, not a `LET_DECL`, so it does not nest as a walked
        // binding. Inferring it (which runs the assert in test builds) must not
        // panic; the whole-expression body is unmodelled so `g` simply defers.
        let types = def_types("module M\nlet g = let y = 1 in y\n");
        // No panic reached here; `g` is not published (the `let … in` RHS is
        // unmodelled), which is sound (D5).
        assert_eq!(types.get("g"), None);
    }

    // ===== Stage 3.2c-3 — function application behaviour tests =====

    #[test]
    fn ground_application_grounds_the_result() {
        // `let f c = if c then 1 else 2` is `bool -> int`; `let n = f true` applies
        // it, so `n : int`. The result `r` is fixed by `Eq(f, Fun(d, r))` against
        // `f`'s ground type — independent of how the argument coerces (that relation
        // is dropped/poisoned).
        let types = def_types("module M\nlet f c = if c then 1 else 2\nlet n = f true\n");
        assert_eq!(types.get("n").map(String::as_str), Some("System.Int32"));
    }

    #[test]
    fn partial_application_grounds_a_function_result() {
        // `let add a b = …` is `bool -> bool -> int`; `let g = add true` leaves the
        // residual `bool -> int`, ground, so `g : bool -> int`.
        let types = def_types(
            "module M\nlet add a b = if a then (if b then 1 else 2) else 3\nlet g = add true\n",
        );
        assert_eq!(
            types.get("g").map(String::as_str),
            Some("System.Boolean -> System.Int32")
        );
    }

    #[test]
    fn polymorphic_application_wakes_and_grounds_result() {
        // Closed by 3.3c: `let id x = x` (`'a -> 'a`), `let n = id 42` ⇒ `n : int`.
        // `id` instantiates to `'b -> 'b`, so `Eq(id_use, Fun(d, r))` gives
        // `d = r = 'b`. The literal argument `42 : int` is suspended as
        // `ArgCheck { arg: int, dom: d }`; on this walk-complete value binding the
        // wake fires (`d` is a scheme instantiation var of ours — a no-subsumption
        // domain), discharging `Eq(int, d)`, so `d = r = int` and `n : int`,
        // matching FCS. (Flipped from the 3.2c-3 `polymorphic_application_defers`.)
        let types = def_types("module M\nlet id x = x\nlet n = id 42\n");
        assert_eq!(types.get("id").map(String::as_str), Some("'a -> 'a"));
        assert_eq!(
            types.get("n").map(String::as_str),
            Some("System.Int32"),
            "the suspended arg wake grounds the polymorphic application result"
        );
    }

    #[test]
    fn applied_polymorphic_argument_wakes_and_generalises() {
        // `let g y = id y` (with `id : 'a -> 'a`): the argument `y` is suspended as
        // an `ArgCheck { arg: y, dom: d }` against `id`'s domain `d`. On this
        // *walk-complete* binding the wake fires — `d` resolves to a scheme
        // **instantiation variable** of ours (an unbound root in
        // `scheme_inst_vars`, so a no-subsumption domain) — discharging `Eq(y, d)`.
        // `y = d = r = 'a`, so `g` generalises to `'a -> 'a`, matching FCS. This is
        // the headline 3.3c payoff (flipped from the 3.2c-3
        // `applied_polymorphic_argument_defers_via_poison` deferral).
        let types = def_types("module M\nlet id x = x\nlet g y = id y\n");
        assert_eq!(types.get("id").map(String::as_str), Some("'a -> 'a"));
        assert_eq!(
            types.get("g").map(String::as_str),
            Some("'a -> 'a"),
            "the suspended arg↔param wake grounds the parameter, so the function generalises"
        );
    }

    #[test]
    fn ill_typed_application_stays_silent() {
        // `let n = 5` then `n 3`-shape: `n : int` is applied as a function. The
        // `Eq(n, Fun(d, r))` fails to unify (int is not a function shape), so
        // `unify_atomic` rolls it back whole — `n` stays `int`, `r` stays open,
        // nothing wrong is emitted. `n` keeps its RHS type; the application result
        // is silent.
        let types = def_types("module M\nlet n = 5\nlet m = n 3\n");
        assert_eq!(
            types.get("n").map(String::as_str),
            Some("System.Int32"),
            "the value binder keeps its ground RHS type; the failed Eq rolls back"
        );
        assert_eq!(
            types.get("m"),
            None,
            "an ill-typed application result stays silent (the shape Eq fails)"
        );
    }

    #[test]
    fn ground_application_dead_end_lets_unrelated_param_generalise() {
        // A fully-modelled application that dead-ends into ground types does not
        // block generalisation of an unrelated parameter: `let f c = …` (ground
        // `bool -> int`), `let h x = (f true, x)` ⇒ `'a -> int * 'a`. The
        // application `f true` is ground `int` (poison bites only the dropped arg
        // relation — a *ground* var is unaffected), so `x` still quantifies.
        let types = def_types("module M\nlet f c = if c then 1 else 2\nlet h x = (f true, x)\n");
        assert_eq!(
            types.get("h").map(String::as_str),
            Some("'a -> System.Int32 * 'a")
        );
    }

    #[test]
    fn infix_application_stays_unmodelled() {
        // Infix application (`x + 1`) is an `INFIX_APP_EXPR`, deliberately left
        // unmodelled ⇒ incomplete, so `let f x = x + 1` still defers (it does not
        // become an `App` payoff). This pins that 3.2c-3 did not widen scope to
        // infix operators.
        let types = def_types("module M\nlet f x = x + 1\n");
        assert_eq!(
            types.get("f"),
            None,
            "infix application is unmodelled; the function defers"
        );
    }

    #[test]
    fn bracket_indexer_stays_unmodelled() {
        // `f[true]` parses as an `APP_EXPR` (carrying the
        // `HIGH_PRECEDENCE_BRACK_APP_TOK` marker), but F# lowers a bracket indexer
        // to a `GetSlice`/`Item` member lookup, *not* a function application — a
        // construct this stage does not model. So `let n = f[true]` must NOT be
        // typed via the application path (which would wrongly emit `n : int` by
        // treating the indexer as `f` applied to `[true]`). On a ground function
        // whose result is itself a function, the naive model would even *disagree*
        // with FCS (`g[true]` where `g : bool -> 'd -> int` is `obj -> int` in FCS,
        // not our `'? -> int`). We defer.
        let types = def_types("module M\nlet f c = if c then 1 else 2\nlet n = f[true]\n");
        assert_eq!(
            types.get("n"),
            None,
            "a bracket indexer is not a function application; it must defer"
        );
    }

    #[test]
    fn failed_shape_application_result_never_generalises() {
        // A function binding whose body is an application that *cannot* take a
        // function shape must NOT generalise a leaked, unrelated result var. `let n
        // = 5` gives `n : int`; `let h y = n 3` applies `n` (a non-function) — the
        // `Eq(n, Fun(d, r))` fails and rolls back, leaving `r` a fresh open var.
        // Without poisoning `r`, `h`'s type `'y -> r` would generalise to a bogus
        // `'a -> 'b`; FCS reports a type error, so we must stay silent (D5).
        let types = def_types("module M\nlet n = 5\nlet h y = n 3\n");
        assert_eq!(types.get("n").map(String::as_str), Some("System.Int32"));
        assert_eq!(
            types.get("h"),
            None,
            "a failed-shape application result must not leak into a bogus scheme"
        );
    }

    // ===== Stage 3.3c — the application wake rule (suspended arg↔param) =====

    #[test]
    fn chained_application_wakes_through_the_chain() {
        // `let c x = id (id x)` ⇒ `'a -> 'a`: the inner `id x` wakes (`x`'s var =
        // `id`'s domain, a scheme instantiation var), grounding the inner result to
        // that same class; the outer `id (…)` wakes likewise, so the whole chain is
        // one quantifiable class → `'a -> 'a`, matching FCS.
        let types = def_types("module M\nlet id x = x\nlet c x = id (id x)\n");
        assert_eq!(types.get("c").map(String::as_str), Some("'a -> 'a"));
    }

    #[test]
    fn in_file_monomorphic_function_wakes() {
        // `let fb b = if b then 1 else 2` is `bool -> int` (condition-grounded);
        // `let h y = fb y`: the arg `y` is suspended against `fb`'s domain `d`,
        // which the shape `Eq` grounds to the sealed primitive `bool` (a
        // no-subsumption domain). The complete binding's wake discharges
        // `Eq(y, bool)`, so `h : bool -> int`, matching FCS.
        let types = def_types("module M\nlet fb b = if b then 1 else 2\nlet h y = fb y\n");
        assert_eq!(
            types.get("h").map(String::as_str),
            Some("System.Boolean -> System.Int32")
        );
    }

    #[test]
    fn ground_value_argument_wakes() {
        // `let b = true` (`b : bool`), `let fb b2 = if b2 then 1 else 2`
        // (`bool -> int`), `let n = fb b`: the argument is the ground value `b`, and
        // the domain grounds to the sealed `bool`. The wake discharges `Eq(b, bool)`
        // (a no-op — both are already `bool`); `n : int` from the result. FCS agrees.
        let types =
            def_types("module M\nlet b = true\nlet fb b2 = if b2 then 1 else 2\nlet n = fb b\n");
        assert_eq!(types.get("n").map(String::as_str), Some("System.Int32"));
    }

    #[test]
    fn instantiated_scheme_argument_flows_into_a_tuple() {
        // `let k y = (id y, y)` ⇒ `'a -> 'a * 'a`: `id y` wakes (`y = id`'s domain,
        // a scheme instantiation var), so `y`, the inner result, and the outer
        // element all collapse into one quantifiable class; the tuple's second `y`
        // element shares it. FCS: `'a -> 'a * 'a`.
        let types = def_types("module M\nlet id x = x\nlet k y = (id y, y)\n");
        assert_eq!(types.get("k").map(String::as_str), Some("'a -> 'a * 'a"));
    }

    #[test]
    fn annotated_param_hazard_emits_nothing_wrong() {
        // THE SOUNDNESS GUARD. `let h (y: string) = (y, f y)` with `f : int -> int`
        // is ill-typed mid-edit code; FCS keeps `y : string` and `h : string ->
        // string * int` (reporting the argument mismatch). Pre-R2-b the annotated
        // parameter marked the binding incomplete and everything deferred; since
        // R2-b the annotation is *modelled*, and the guard's substance is
        // unchanged: `y` is grounded to `string` by its annotation **before**
        // the wake, so the wake's `Eq(string, int)` fails and rolls back — `y`
        // is never retyped to `int` through the annotation-vs-application
        // conflict, and its synth-mode tuple use reads off `string` (FCS
        // agrees). `h` still emits (`string -> string * int` — the result is
        // fixed by `f`'s own shape), and the params are never published.
        let src = "module M\nlet f (x: int) = x\nlet h (y: string) = (y, f y)\n";
        let types = def_types(src);
        assert_eq!(
            types.get("f").map(String::as_str),
            Some("System.Int32 -> System.Int32")
        );
        assert_eq!(
            types.get("h").map(String::as_str),
            Some("System.String -> System.String * System.Int32")
        );
        assert_eq!(
            types.get("y"),
            None,
            "the annotated param is never published standalone"
        );
        assert_eq!(types.get("x"), None);
        // The precise leak check: the tuple's `y` element must read off the
        // annotation's `string`, not the wake's `int`. (An `Int32` render does
        // legitimately appear elsewhere — `f`'s body use of `x` — so the
        // assertion is range-keyed, not a blanket render scan.)
        let parsed = parse(src);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let env = primitive_env();
        let resolved = resolve_file(&file, &ProjectItems::default(), &env);
        let inferred = super::infer_file(&file, &resolved, &env);
        // Key on the exact one-byte `y` token range: the tuple node itself
        // starts at the same offset (its parens are a separate wrapper).
        let y_use_at = src.rfind("(y,").expect("tuple") + 1;
        let y_use = inferred
            .types()
            .iter()
            .find(|(r, _)| u32::from(r.start()) as usize == y_use_at && r.len() == 1.into())
            .map(|(_, ty)| ty.render());
        assert_eq!(
            y_use.as_deref(),
            Some("System.String"),
            "the tuple's `y` use must read off the annotation type"
        );
    }

    #[test]
    fn ill_typed_literal_against_sealed_domain_fails_discharge_silently() {
        // `let fb b = if b then 1 else 2` (`bool -> int`), `let m = fb 1`: the
        // argument `1 : int` is suspended against the domain, which grounds to the
        // sealed `bool`. The complete binding's wake attempts `Eq(int, bool)`, which
        // **fails** to unify — `unify_atomic` rolls it back, leaving no trace — so
        // the ArgCheck is undischarged and its `arg`/`dom` are poisoned. The result
        // `r` is still grounded to `int` by `Eq(fb, Fun(bool, int))`, so `m : int`
        // (FCS agrees); nothing wrong is published for the argument.
        let types = def_types("module M\nlet fb b = if b then 1 else 2\nlet m = fb 1\n");
        assert_eq!(
            types.get("m").map(String::as_str),
            Some("System.Int32"),
            "the result is grounded by the function shape, independent of the failed arg discharge"
        );
    }

    #[test]
    fn incomplete_binding_blocks_an_otherwise_wakeable_application() {
        // `let g y = (id y, (y : int))`: the `(y : int)` ascription elsewhere in the
        // body marks the binding **incomplete**. So even though `id y` is otherwise
        // wakeable (`y = id`'s scheme-inst domain), the completeness gate forbids the
        // wake, and the binding defers. FCS grounds `g : int -> int * int` through
        // the ascription (a constraint we drop); we stay silent (D5). This pins that
        // the gate protects annotations/ascriptions.
        let types = def_types("module M\nlet id x = x\nlet g y = (id y, (y : int))\n");
        assert_eq!(
            types.get("g"),
            None,
            "an ascription-incomplete binding must not discharge its arg wake"
        );
    }

    #[test]
    fn undischarged_argcheck_poison_blocks_generalisation() {
        // The generalisation-interaction guard: an application whose domain is NOT a
        // no-subsumption type leaves its `ArgCheck` undischarged, and the deferred
        // poison on `arg`/`dom`/`r` must block quantifying them. `let fo (o: obj) =
        // 1` is `obj -> int`; `let g y = fo y`: `obj` admits subsumption (excluded
        // from the no-subsumption set), so the wake never fires. FCS keeps
        // `g : 'a -> int` (it does *not* ground `y := obj`); we defer rather than
        // emit a wrong `obj -> int`. (`fo`'s annotated param also independently
        // defers the callee, so this is belt-and-braces — but it pins the poison
        // path.)
        let types = def_types("module M\nlet fo (o: obj) = 1\nlet g y = fo y\n");
        assert_eq!(
            types.get("g"),
            None,
            "an undischarged arg wake (non-no-subsumption domain) must defer the function"
        );
    }

    proptest::proptest! {
        /// For every literal kind `literal_ty` covers, `let id x = x` +
        /// `let n = id <lit>` grounds `n` to that literal's type: the polymorphic
        /// application wakes (the domain is `id`'s scheme instantiation var), and the
        /// discharged `Eq(arg, dom)` grounds the result. Covers every suffix so the
        /// no-subsumption sealed-primitive set (which mirrors `literal_ty`) is
        /// exercised end-to-end.
        #[test]
        fn identity_application_grounds_result_to_literal_type(idx in 0usize..16) {
            // (literal source, the primitive `literal_ty` assigns it).
            let cases = [
                ("42", "System.Int32"),
                ("42y", "System.SByte"),
                ("42uy", "System.Byte"),
                ("42s", "System.Int16"),
                ("42us", "System.UInt16"),
                ("42u", "System.UInt32"),
                ("42L", "System.Int64"),
                ("42UL", "System.UInt64"),
                ("42n", "System.IntPtr"),
                ("42un", "System.UIntPtr"),
                ("4.2", "System.Double"),
                ("4.2f", "System.Single"),
                ("4.2m", "System.Decimal"),
                ("\"hi\"", "System.String"),
                ("'c'", "System.Char"),
                ("true", "System.Boolean"),
            ];
            let (lit, prim) = cases[idx];
            let src = format!("module M\nlet id x = x\nlet n = id {lit}\n");
            let types = def_types(&src);
            proptest::prop_assert_eq!(
                types.get("n").map(String::as_str),
                Some(prim),
                "src={:?}", src
            );
        }
    }

    #[test]
    fn sealed_primitive_set_matches_literal_ty() {
        // The no-subsumption sealed-primitive set (`is_sealed_primitive`) must be
        // exactly the scalar `Ty::Named`s `literal_ty` produces — the same
        // primitives a literal has, each sealed, so a `<lit>` argument may discharge
        // against a domain grounded to that primitive. This pins the two lists in
        // lock-step (a byte-string literal is a `Ty::Array`, so it is *not* a named
        // sealed primitive — correctly excluded).
        use borzoi_cst::syntax::{ConstExpr, Expr};
        let kinds = [
            "42",
            "42y",
            "42uy",
            "42s",
            "42us",
            "42u",
            "42L",
            "42UL",
            "42n",
            "42un",
            "4.2",
            "4.2f",
            "4.2m",
            "\"s\"",
            "'c'",
            "true",
            "\"bytes\"B",
        ];
        for lit in kinds {
            let src = format!("module M\nlet v = {lit}\n");
            let parsed = parse(&src);
            let file = ImplFile::cast(parsed.root).expect("impl file");
            // Find the `ConstExpr` in the RHS.
            let cst = file
                .syntax()
                .descendants()
                .find_map(Expr::cast)
                .and_then(|e| match e {
                    Expr::Const(c) => Some(c),
                    _ => None,
                })
                .or_else(|| file.syntax().descendants().find_map(ConstExpr::cast))
                .expect("a const literal");
            let ty = super::literal_ty(&cst);
            match ty {
                // Every named literal type must be a sealed primitive.
                Some(super::Ty::Named(ref path)) => assert!(
                    super::is_sealed_primitive(path),
                    "{lit}: literal_ty gave named {path:?} but is_sealed_primitive says no"
                ),
                // A byte string is an array — not a sealed *named* primitive.
                Some(super::Ty::Array { .. }) => {}
                // Deferred literals (none in this list) have no type.
                _ => {}
            }
        }
    }

    // ===== Stage 3.2c-2c — instantiation property tests =====

    /// A tiny pure reference for the expected canonical render of a
    /// permutation-family function `let f p1 … pn = (pσ(1), …, pσ(k))`: the head
    /// is `'a -> … -> 'z` over `n` parameters (numbered by head order), and the
    /// tuple return names each selected parameter by its head index. This computes
    /// the *string* FCS should agree on without an FCS round-trip (a couple of
    /// exemplars in the differential pin the convention).
    fn expected_permutation_render(n: usize, sigma: &[usize]) -> String {
        let head: Vec<String> = (0..n).map(|i| crate::ty::typar_name(i as u32)).collect();
        let ret: Vec<String> = sigma
            .iter()
            .map(|&j| crate::ty::typar_name(j as u32))
            .collect();
        format!("{} -> {}", head.join(" -> "), ret.join(" * "))
    }

    proptest::proptest! {
        /// A permutation-family function generalises to the canonical scheme a pure
        /// reference predicts — parameters number by head order, the return tuple
        /// by first appearance. Exercises arbitrary arities and selections
        /// (including repeats and reordering).
        #[test]
        fn permutation_family_generalises_canonically(
            // 2..=6 parameters; a return tuple of 2..=4 selections into them.
            n in 2usize..=6,
            picks in proptest::collection::vec(0usize..6, 2..=4),
        ) {
            // Clamp the selections into the parameter range.
            let sigma: Vec<usize> = picks.iter().map(|&p| p % n).collect();
            let params: Vec<String> = (0..n).map(|i| format!("p{i}")).collect();
            let ret: Vec<String> = sigma.iter().map(|&j| format!("p{j}")).collect();
            let src = format!(
                "module M\nlet f {} = ({})\n",
                params.join(" "),
                ret.join(", ")
            );
            let types = def_types(&src);
            let want = expected_permutation_render(n, &sigma);
            proptest::prop_assert_eq!(
                types.get("f").map(String::as_str),
                Some(want.as_str()),
                "src={:?}", src
            );
        }
    }

    /// An empty resolved file, so a [`Gen`] can be constructed for unit tests of
    /// the substrate helpers ([`Gen::instantiate`] / [`Gen::generalise`]) that do
    /// not need real name resolution.
    fn empty_resolved() -> ResolvedFile {
        use borzoi_cst::parser::parse;

        let parsed = parse("module M\n");
        let file = ImplFile::cast(parsed.root).expect("impl file");
        resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
    }

    #[test]
    fn instantiation_produces_fresh_distinct_vars() {
        // Instantiating a scheme replaces each distinct `Param` with a fresh var,
        // memoised so the same index maps to one var and distinct indices to
        // distinct vars; no `Param` survives.
        let resolved = empty_resolved();
        let env = AssemblyEnv::default();
        let mut cx = Gen::new(&resolved, &env, super::ExtensionScope::default());
        let scheme = Ty::Fun {
            arg: Box::new(Ty::Param(0)),
            ret: Box::new(Ty::Tuple(vec![Ty::Param(0), Ty::Param(1)])),
        };
        let inst = cx.instantiate(&scheme);
        // Structure preserved, no Param survives.
        assert!(
            !super::contains_param(&inst),
            "no Param may survive: {inst:?}"
        );
        match &inst {
            Ty::Fun { arg, ret } => {
                let arg_v = match **arg {
                    Ty::Var(v) => v,
                    _ => panic!("arg should be a fresh var, got {arg:?}"),
                };
                match &**ret {
                    Ty::Tuple(elems) => {
                        let e0 = match elems[0] {
                            Ty::Var(v) => v,
                            _ => panic!("elem 0 should be a var"),
                        };
                        let e1 = match elems[1] {
                            Ty::Var(v) => v,
                            _ => panic!("elem 1 should be a var"),
                        };
                        // Param(0) appears in arg and elem 0 → same var.
                        assert_eq!(arg_v, e0, "same Param index → same fresh var");
                        // Param(1) is distinct from Param(0).
                        assert_ne!(e0, e1, "distinct Param indices → distinct vars");
                    }
                    _ => panic!("ret should be a tuple"),
                }
            }
            _ => panic!("inst should be a function"),
        }
    }

    #[test]
    fn generalise_after_instantiate_round_trips() {
        // Instantiating a canonical scheme and re-generalising the fresh vars
        // recovers the same scheme (up to canonical numbering). We drive this
        // through the table so the fresh vars are real keys.
        let resolved = empty_resolved();
        let env = AssemblyEnv::default();
        let mut cx = Gen::new(&resolved, &env, super::ExtensionScope::default());
        let mark = cx.table.mark();
        let scheme = Ty::Fun {
            arg: Box::new(Ty::Param(0)),
            ret: Box::new(Ty::Fun {
                arg: Box::new(Ty::Param(1)),
                ret: Box::new(Ty::Tuple(vec![Ty::Param(1), Ty::Param(0)])),
            }),
        };
        let inst = cx.instantiate(&scheme);
        let resolved_inst = cx.table.resolve(&inst);
        let regen = cx
            .generalise(&resolved_inst, mark)
            .expect("fresh vars generalise");
        assert_eq!(regen, scheme, "generalise ∘ instantiate is the identity");
    }
}
