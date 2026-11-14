//! Best-effort type inference over the parsed CST (Phase 3).
//!
//! **Literal typing in soundness-safe positions.** A literal's
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
//! isolation (`USER_NUM_LIT` like `1I`/`1G`, source-location identifiers).
//!
//! Pure (no IO): a binding-RHS literal's type is syntactically determined.
//! Differentially tested against FCS's typed tree
//! (`crates/sema/tests/all/infer_literals_diff.rs`), which exercises both the sound
//! positions (must agree) and the unsound ones (must stay silent).
//!
//! **Unification as the walk goes, suspension where it cannot.** Per the
//! plan's [D8], a fold over each binding unifies every equality it generates
//! in the [`InferTable`] substrate at the point it generates it ([`Gen::eq`]),
//! and suspends the relations it cannot decide there — a member access and an
//! argument check ([`Constraint`]) — for a solve that decides them mid-walk
//! ([`Gen::settle`]) or at the binding's end ([`Gen::solve`]). Each
//! expression's type is read back by resolving its variable — emitted only
//! when it is fully [`ground`](TyTable::is_ground) (D5: silence otherwise).
//!
//! **Value-reference propagation.** Generation consults name resolution
//! ([`crate::resolve_file`]) to connect a value *use* to its binder:
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
//! the binder's type there would violate D5.
//!
//! **Parentheses and tuples.** The coercion-free RHS typer
//! recurses through transparent parentheses (`let y = (x)`) and types a
//! reference tuple `(a, b, …)` as [`TyKind::Tuple`], recursing into each element
//! (still coercion-free — an unannotated tuple imposes no expected type on its
//! elements). A tuple-bound value hovers as `int * string`. Struct tuples and an
//! element we cannot type are deferred (the latter leaves the tuple un-ground
//! while its typeable siblings still emit).
//!
//! **The bidirectional spine and control flow.** RHS typing
//! is a recursive [`Gen::infer_expr`] threading an `expected` type
//! (bidirectional checking): `None` is *synth* mode — a coercion-free position
//! where the synthesized type *is* the elaborated one, so the node is emitted —
//! and `Some(_)` is *check* mode — a coercion-possible position where the node is
//! **not** emitted, since the elaborated type may be a coercion inference
//! doesn't model (no subtype relation). Check mode only suppresses emission
//! and propagates the mode to children; it deliberately adds no cross-constraint,
//! which without a subtype relation could only force equality and back-flow a
//! coerced type onto a binder. This is the permanent HM shape: function
//! application checks arguments against parameter types the same
//! way, and subtype-aware checking would *complete* the check-mode rule (relate
//! by subtyping, emit the coerced type) without replacing it. For
//! `if`/`then`/`else`, in synth mode the result is the then-branch's
//! synthesized type (and the whole `if` defers if that branch can't synthesize —
//! its type must not be taken from the coercible else), the else-branch is checked
//! against it; an `if` with no *final* `else` (a plain `if c then a` or an `elif`
//! chain that never terminates in `else`) results in `unit`, not the then-branch
//! type, so it too defers. In check mode both branches are checked. The result
//! flows to the binder and hover (`let r = if … then 1 else 2` ⇒ `int`).
//!
//! **Function and lambda bodies.** Generation walks the
//! **body** of a function binding (`let f x … = body`), a lambda
//! (`fun x … -> body`), and a `while` loop, so their sub-expressions get typed —
//! e.g. a function body that is an `if` emits its result type
//! (`let f c = if c then 1 else 2` ⇒ the `if` and the `1` type as `int`). Each
//! body carries the right bidirectional *mode*: a function-binding body and a
//! synth-position lambda body are coercion-free (synth); a lambda reached in
//! *check* mode passes it on (an expected function type could retarget the body);
//! a `while` body is checked against `unit`.
//!
//! **`TyKind::Fun`, monomorphic function types, and condition typing.**
//! A `let`-function binder carries a [`TyKind::Fun`] type built by currying its
//! parameter variables over the body's return variable
//! ([`Gen::function_type`]) — emitted (on [`InferredFile::def_type`]) only when
//! every operand is [`ground`](TyTable::is_ground), i.e. **monomorphic** functions
//! (`let f c = if c then 1 else 2` ⇒ `f : bool -> int`). A polymorphic function
//! (an un-grounded parameter or return) leaves an open variable and so is
//! published only if it generalises (below).
//!
//! **Condition typing.** A parameter is grounded by a real constraint, and one
//! that inference supplies is the `if`/`while` **condition** ([`Gen::constrain_bool`]): a condition admits *no*
//! subsumption — F# requires it to be exactly `bool` — so unifying it with
//! `bool` is a genuine equality, not the coercion-possible check of an `if`
//! branch. `let f c = if c then …` thus makes `c : bool` *inside* the function
//! type.
//!
//! The subtlety is that condition typing is one *modelled* constraint among
//! possibly-**unmodelled** others: a parameter can also carry a use-site
//! ascription (`(c : int)`) or an annotation inference does not read, and on
//! ill-typed mid-edit code F# resolves the conflict differently (keeping `int`,
//! reporting the condition error). So the condition-derived `bool` must not reach
//! any place we publish a parameter's *own* type. The mechanism:
//! [`Gen::param_var`] gives each named parameter — alone, or an element of a
//! tupled parameter ([`Gen::pattern_ty`]) — a **private slot**
//! variable that is the parameter's type *inside* the function's `TyKind::Fun`, and
//! condition typing grounds **that slot** ([`BinderEnv::param_slot`]), *not* the
//! parameter's binder [`def_var`](Gen::def_var). The binder's variable — which
//! its expression uses and `def_type` read off — is therefore never grounded by a
//! condition, so `bool` flows *only* into the function signature and never into a
//! standalone parameter read-off (D5). A parameter pattern outside the modelled
//! shapes (a constructor, a literal, an annotation outside the modelled set) gets no
//! slot, so it never grounds and the function defers.
//!
//! Publishing only the function type is provably sound: a ground `TyKind::Fun` means
//! every parameter slot is ground, and any unmodelled constraint on a parameter
//! would have blocked the function's groundness (so it would have deferred) —
//! hence a ground function matches FCS, while still surfacing the parameter types
//! *within* the signature (`f : bool -> int`).
//!
//! **`let`-generalisation, instantiation, and typar rendering.**
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
//! modelled** — which needs this machinery:
//!
//! - **Per-binding walk-completeness** ([`Scope::complete`]). During one binding's
//!   generation, any sub-expression or pattern we do not fully model marks the
//!   binding *incomplete* (every `None`-return arm of [`Gen::infer_construct`], a
//!   deferred literal, an unresolved name, a lambda, a struct/degenerate tuple, an
//!   `if` with no final `else`, an unmodelled parameter pattern, and a condition shape
//!   [`Gen::constrain_bool`] does not fully model — a compound `x && y` drops FCS
//!   constraints on `x`/`y`). Incomplete ⇒ **no generalisation** (ground emission
//!   is unaffected — the subset argument needs no completeness).
//! - **Poisoned variables** ([`Scope::poison`]). Check mode deliberately drops the
//!   relation between a checked expression and its expected type (there is no
//!   subtype relation); that dropped relation could ground *either* endpoint in FCS
//!   (`let f x = if true then x else 1` unifies `x` with `int` through the
//!   else-relation we drop). So [`Gen::infer_expr`] poisons the expected and
//!   returned variables of every check-mode call; at generalisation the set is
//!   closed one step (resolve each, poison every variable in the resolved term). A
//!   poisoned variable is never generalised (but *poisoned-and-ground* is fine —
//!   the subset argument again).
//!
//! On a **complete** function binding the private-slot decoupling is undone:
//! [`Gen::slot_binder_reunify`] emits `Eq(slot, binder_var)` per simple parameter
//! (F# has one variable for the parameter — the decoupling only exists to fence
//! off *unmodelled* constraints, and every unmodelled shape already sets
//! incomplete). A parameter's binder var can then be ground (or a `Param`), but
//! [`Gen::finish`] never publishes a parameter from it: it publishes the
//! parameter's annotation, or its ground slot in a published function type, and
//! nothing for a generic one. Parameter uses in
//! synth positions may emit (`let f x = if x then (1, x) else (2, x)` types the
//! tuple's `x` as `bool`), sound on a complete binding.
//!
//! Generalisation is **sequential per binding** (Algorithm W): [`Gen::let_binding`]
//! drains and solves each binding's constraints before the next, so a later use
//! sees the earlier binding's [scheme](BinderEnv::scheme). A **vid mark**
//! ([`InferTable::mark`]) taken at binding start makes the environment-freeness
//! check trivial (an open var inherited from an earlier binder has a smaller index,
//! so it defers this function rather than being quantified). At a function
//! binding's finalisation: resolve its type; if ground, emit it; else
//! generalise iff complete **and** every open var is created-this-binding and
//! unpoisoned — replacing those vars by [`TyKind::Param`]s numbered by first
//! appearance (DFS, argument-before-return) — else defer. A use of a scheme'd
//! binder ([`Gen::infer_construct`]'s [`Expr::Ident`] arm) instantiates with a
//! fresh var per distinct `Param`.
//!
//! **Function application.** `f x` is a
//! modelled construct ([`Gen::infer_app`]): the **function position** is
//! synthesized ([`Gen::infer_callee`]) but records **no** node — FCS emits a typed
//! node only at the whole application, never at the bare function-position use
//! (probed against the `types` oracle) — then fresh domain `d` and result `r`
//! variables are created and `Eq(tf, Fun(d, r))` is unified. That equality is
//! **genuinely true**: applying an in-file value forces a function shape (an
//! in-file binder cannot be a method group). The `App` node emits `r` in synth
//! positions (D5-safe: `r` is fixed by the `Eq` against the function's *own* type,
//! independent of how the argument coerces). Curried `f x y` falls out of nested
//! `App`s, and so does an **infix** application (see *Infix operators* below); a
//! **bracket indexer** (`arr[i]`, which the parser stores under `APP_EXPR` but F#
//! lowers to a member lookup, not an application), `TypeApp`, and applied
//! `DotGet` stay unmodelled ⇒ incomplete.
//!
//! **The suspended `HasMember` worklist (fields and properties).** A
//! member access `recv.Name` where `Name` is a **field or non-indexer instance
//! property** on a **non-generic [`TyKind::Named`]** receiver resolved through the
//! [`AssemblyEnv`] types: `let s = "hi"` then `let n = s.Length` ⇒ `n : int`,
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
//! - **The worklist.** `Constraint` holds a suspended
//!   [`Constraint::HasMember`] `{ recv, name, result }`. [`Gen::solve`]
//!   fixpoints: wake each parked member whose `recv`
//!   resolves to a concrete head ([`Gen::wake_member`]), unify `result` with
//!   the member's type, repeat until none fires (each fires
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
//!
//! **The member-resolution side-table.** On a successful `HasMember`
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
//! **The application wake rule (suspended arg↔param).** The
//! worklist's second client: `let g y = id y` ⇒
//! `g : 'a -> 'a`, `let n = id 42` ⇒ `n : int`, `let h y = fb y` (in-file
//! `fb : bool -> int`) ⇒ `h : bool -> int`, and chained `let c x = id (id x)` ⇒
//! `'a -> 'a`.
//!
//! Each modelled application generates a **suspended**
//! [`Constraint::ArgCheck`] `{ arg, dom, r }` for the argument↔parameter
//! subsumption (the argument walk does not take the [`Gen::infer_expr`] poison
//! wrapper — [`Gen::infer_arg`] opts the application path out; every *other* check
//! site takes it).
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
//!   [`Solver::scheme_inst_vars`] populated by [`Solver::instantiate`], membership
//!   checked against union-find roots), or a **function type**
//!   ([`Solver::function_domain`]), which F# relates to an argument by equality
//!   alone. `obj`, arbitrary named types and arrays are excluded — F# admits a
//!   coercion there, so equality would be wrong.
//!
//! **Loosening at each use.** FCS gives a use of a value a type the
//! definition does not have (`MakeApplicableExprWithFlex`): every parameter of
//! a type that is not sealed becomes a fresh variable that is a subtype of it,
//! so `let g y = fo y` with `fo : obj -> int` keeps `g : 'a -> int`, and
//! `let g = fo` is `'a -> int` too. [`Gen::value_use`] models that wherever
//! the value's type is known at the use, so an argument check against a
//! function type relates the argument's type FCS has.
//!
//! **Verification.** FCS checks in source order; the worklist does not. On a
//! body FCS rejects, a check can fire that FCS never applied — `(p |> fs, fi p,
//! p)` fixes `p` to `string` in FCS, which then rejects `fi p`. So
//! [`Gen::solve`] fires the checks under a snapshot and keeps them only when
//! [`Gen::verified`]: every relation the binding holds is consistent as an
//! equality, so FCS, which tries an equality first, applies all of them, in
//! whatever order. Otherwise no check fires at all.
//!
//! **Deferred poisoning** (poison is never removed, only not added). At binding
//! finalisation (in [`Gen::solve`], before the poison
//! closure / generalisation) every `ArgCheck` that did **not** successfully
//! discharge — never woken, gate failed, or the `Eq` itself failed on ill-typed
//! code (the atomic rollback leaves no trace) — poisons its `arg`, `dom`, **and**
//! the result `r`. A successful discharge poisons nothing (the relation is now
//! modelled); a ground result still emits (poison bites only open vars). So a
//! failed-shape `n 3` poisons its dangling `r`, and a polymorphic result that
//! never wakes defers.
//!
//! **The soundness hazard this design closes.** *Unconditional* discharge would
//! be a D5 violation on ill-typed mid-edit code: `let h (y: string) = (y, f y)`
//! with `f : int -> int` would ground `y := int` through the wake (the annotation
//! is unmodelled), and `y`'s synth-mode tuple use would emit `int` where FCS keeps
//! `string`. The completeness gate blocks it: the annotated parameter `(y:
//! string)` marks the binding incomplete, so the `ArgCheck` never discharges and
//! `y` stays open (silent). Argument-node *emission* stays off (a check-mode arg
//! records no node even when its relation discharges).
//!
//! **Method-call typing.** A method call
//! `recv.Method(args)` types as the method's **return** type:
//! `let h = s.GetHashCode()` ⇒ `int`, `let t = s.GetType()` ⇒ `System.Type`.
//!
//! - **Parse shape.** A method call is an **`App` whose callee is a member access**
//!   (`APP_EXPR[ LONG_IDENT_EXPR[s.M] / DOT_GET_EXPR["hi".M], arg ]`), and FCS emits
//!   its `call:instance` node at the *whole* application, typed as the return type,
//!   with the receiver its own node. [`Gen::infer_app`] routes a member-access
//!   callee to [`Gen::infer_method_call`].
//! - **The worklist, by *kind*.** [`Constraint::HasMember`] carries a
//!   [`MemberAccessKind`] (`Data` | `Method { arg_count }`); the wake dispatches the
//!   lookup on it — [`AssemblyEnv::instance_data_member`] vs
//!   [`AssemblyEnv::instance_method`] (a **single non-overloaded, non-generic**
//!   public instance method) — and the rest of the wake (bridge the return type,
//!   record the member, unify the result) is shared. Overloaded (`s.Substring(1)` —
//!   FCS `call:instance-overloaded`), generic, static, extension, and constructor
//!   methods defer.
//! - **Well-formedness gate.** The return type is the call's type only when the
//!   argument list is well-formed: the wake gates on the call's positional
//!   `arg_count` ([`Gen::method_arg_vids`]) being `Some(param_count)` exactly. FCS does
//!   *not* type an ill-formed call as the method return — it falls back to `obj`
//!   (`call:function`) — so each of these defers fully (no type, no recorded
//!   resolution): a wrong **arity** (`s.ToLowerInvariant(1)`, `s.Insert()`), a
//!   **parenthesized unit** (`s.ToLowerInvariant(())` is one unit argument, not
//!   zero), a **named argument** (`s.Insert(foo = 0, …)` — [`is_named_arg`], names
//!   unvalidated here), and an extra-parenthesized tuple (`s.Insert((0, "z"))`, which
//!   FCS elaborates as a method-value `application`, so only the one call-parenthesis
//!   layer is peeled and it counts as a single argument). Conservative on
//!   `[<Optional>]` / `params` / named-argument methods, never wrong.
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
//!   as `Data` does, so the hover / go-to-def path serves a called method name
//!   too.
//!
//! **Expression-level `let … in` and sequences.** A function body with a
//! local `let` or a statement is walked
//! ([`Gen::infer_let_in`], [`Gen::infer_sequential`]); the bang, `use` and
//! `rec` forms stay unmodelled.
//!
//! - **A local is typed only when its RHS is ground on its own.** F# generalises
//!   an eligible local, so a variable shared by every use is wrong for it (see
//!   [`Gen::local_binding`]). After the RHS is walked, the constraints so far are
//!   [settled](Gen::settle) — equalities and member wakes, the two kinds that do
//!   not depend on the rest of the binding — and a binder still open marks the
//!   binding incomplete. Argument checks, the only channel through which a use
//!   could ground the local, then never fire.
//! - **A statement is a check position whose unit relation is dropped.** FCS
//!   unifies a statement's type with `unit`; a statement type that is not ground
//!   after settling therefore marks the binding incomplete too.
//! - **Emission follows FCS's nodes.** A sequence records one node at its full
//!   range, typed as its last statement; a statement's root records nothing (FCS
//!   puts a synthetic `unit` node there when its type is not `unit`); a `let … in`
//!   records nothing (FCS keys that node at the binder's identifier, typed as the
//!   body). A local binder publishes its type through `def_type`, checked by the
//!   `binder-types` oracle, which walks declaration bodies for local `let`s.
//!   That publication is an emission like a node's, and FCS keeps neither
//!   inside the operand of a construct it **rejects**. Three rules follow, each
//!   applying to nodes and locals alike: a method call keeps nothing inside
//!   itself and a check-mode subtree records nothing
//!   ([`Emissions::discard_since`], [`Gen::infer_expr_inner`]) — a local's
//!   RHS is a synth position that can sit inside a check position, which is
//!   where FCS rejects an application; and a member access's receiver is
//!   **guarded** on the access resolving ([`emissions::Guard`]), since FCS drops the
//!   receiver of `(1).Length` or `s.Length.Foo` too.
//! - **A condition grounds a parameter only at its first occurrence.** FCS
//!   unifies in source order, so a statement (`x; if x …`) or any earlier use
//!   fixes the parameter first; our solver is unordered, so a condition grounds
//!   its slot only when nothing could have come before it
//!   ([`Gen::is_first_occurrence`]). A parameter whose annotation names an
//!   implicitly scoped type variable (below) is reached through every other
//!   mention of the variable too, so a condition on it never grounds.
//! - **A type variable in a function's head is one variable per name.** FCS
//!   creates `'a` at its first mention in a module-level declaration and
//!   reads every later mention as the same, ordinary inference variable, which
//!   the body may fix (FS0064) and what is left generalises. So the head's
//!   annotations — parameters and return — read each name as one fresh
//!   variable ([`Gen::open_head_typars`]); a parameter so annotated is an
//!   unannotated one whose binder and slot are tied to that structure, and a
//!   return annotation's check discharges as the equality FCS tries first.
//!   A mention anywhere else (a lambda's annotation, a local's, a type
//!   application) is the same variable, so a body mentioning one of the
//!   head's names declines the head's, as does a head with `<'a>`, `^a` or
//!   `'_a`.
//!
//! **Infix operators.** `a OP b` parses as `App(App(OP, a), b)` with the inner
//! application marked infix, and FCS checks it exactly as the application
//! `OP a b`: the operator's value, then the left operand, then the right. So it
//! is typed as one ([`Gen::infer_infix`]), the outer application an ordinary
//! one and the inner one a callee — no node inside, as for any curried call.
//! The operator's value is whatever the resolver says its token names
//! ([`Gen::operator_value`]): an in-file definition is an applied value like
//! any other, and an assembly's operator is a use of an assembly value (below).
//! Only a signature with no constraint on any type parameter
//! qualifies — `+` and `=` decline — since the instantiated variables are
//! scheme-instantiation variables, against which an argument check discharges
//! as an equality. Everything else declines with an [`Incomplete::Operator`]
//! reason. A result fixed only through a function-typed domain (`|>`'s
//! `'T1 -> 'U`) is fixed by the check against it, where the binding's checks
//! are verified.
//!
//! **Assembly values.** A use the resolver resolved to a referenced F#
//! assembly's module function or value — a bare name at its token, a
//! qualified `List.map` over the whole path — is typed as FCS types a use of a
//! value (`TcVal`): the member's IL signature read as its F# type
//! ([`crate::function_ty::module_function_scheme`]), instantiated and loosened
//! as a generalised in-file binder's is ([`Gen::nonlocal_value_use`]). A
//! module-qualified callee is a value application, not a method call. What
//! the reading declines records [`Incomplete::AssemblyValue`].
//!
//! **Preceding files' values.** A use the resolver resolved to an earlier
//! Compile-order file's top-level value ([`Resolution::Item`] of an item this
//! file does not own) is typed from what that file's inference published for
//! the binder ([`PrecedingTypes`], threaded by [`crate::infer_project()`]): its
//! ground type or generalised scheme, instantiated and loosened exactly as an
//! in-file scheme's use is ([`Gen::nonlocal_value_use`], and
//! [`Gen::receiver_var`] for a member access's receiver). FCS fixed that type
//! when it checked the earlier file, so nothing in this file can change it.
//! A value the earlier file published no type for records
//! [`Incomplete::PrecedingValue`] with that file's reason; the verdict each
//! module-level binder publishes is decided in [`Gen::finish`].
//!
//! [D8]: ../../../docs/type-checker-plan.md

use std::cell::RefCell;

use borzoi_assembly::{
    EntityKind, EqualityComparison, FSharpConstraints, Member, MemberMeasures, Primitive, TypeRef,
};
use borzoi_cst::syntax::{
    AppExpr, AstNode, AttributeList, Binding, ConstExpr, DotGetExpr, Expr, IfThenElseExpr,
    ImplFile, LetDecl, LetOrUseExpr, LongIdentExpr, LongIdentPat, MatchExpr, MemberDefn,
    ModuleOrNamespace, ModuleOrNamespaceKind, NamedPat, NestedModuleDecl, ParenPat, Pat,
    RecordRepr, SequentialExpr, SyntaxKind, SyntaxNode, SyntaxToken, TupleSegment, Type, TypeDefn,
    TypeDefnRepr, UnionRepr, VarType,
};
use borzoi_cst::syntax::{TextRange, TextSize};

use crate::assembly_env::{AssemblyEnv, EntityHandle};
use crate::def::{DefId, DefKind};
use crate::function_ty::{
    Arith, Builtin, FunctionTyGap, ModuleFunctionScheme, UnionCaseScheme, builtin_operator,
    module_function_scheme, union_case_scheme,
};
use crate::hash::{HashMap, HashSet};
use crate::infer_project::{ModuleValue, PrecedingGap, PrecedingTypes};
use crate::match_reach::{ClausePat, ConstKind, Ctor, provably_reachable};
use crate::member_ty::type_ref_to_ty;
use crate::names::NameTable;
use crate::node_span::NodeSpans;
use crate::overload::arity_window;
use crate::qnof::QualifiedNameOfFile;
use crate::resolve::{CaseKind, ItemId, Resolution, ResolvedFile};
use crate::ty::{MAX_TYPE_DEPTH, Ty, TyKind, TyPath, TyTable, TyVid, TypeBudgetExceeded};
use crate::unify::{InferTable, TypeWalks};

mod annotations;
mod application;
mod binder_env;
mod bindings;
mod builtins;
mod emissions;
mod expr;
mod extension_scope;
mod facts;
mod finish;
mod generalise;
mod literals;
mod members;
mod patterns;
mod project_types;
mod scope;
mod solve;
mod solver;
mod subsumption;
mod syntax;
#[cfg(test)]
mod tests;
mod typar_constraints;
mod values;

// Every submodule reads its siblings' items, and this module's, through
// `use super::*`: the names below are what each makes visible to the rest.
use annotations::*;
use binder_env::BinderEnv;
use emissions::{EmissionMark, Emissions};
use extension_scope::*;
use facts::*;
use finish::*;
use literals::*;
use members::*;
use patterns::*;
use project_types::*;
use scope::*;
use solve::*;
use solver::*;
use subsumption::*;
use syntax::*;
use typar_constraints::*;

pub(crate) use annotations::{constrained_parameter, rejected_by_attributes};
pub(crate) use builtins::is_conversion_target;
#[cfg(feature = "test-support")]
pub(crate) use project_types::rekey_records;
pub(crate) use project_types::{ProjectCase, ProjectRecord, ProjectRecords};

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
#[derive(Debug, Default, PartialEq, Eq)]
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
    /// Each counted binding's incompleteness reasons, in walk order; empty for
    /// a complete binding. See [`Self::incompleteness`].
    incompleteness: Vec<Vec<Incomplete>>,
    /// Each `(expr).Member` access's **receiver** type, keyed by the whole
    /// `DOT_GET_EXPR`'s range — recorded whether or not the access resolves.
    /// See [`Self::dot_get_receiver_type`].
    receiver_types: HashMap<TextRange, Ty>,
    /// What this file publishes for each binder a module-level `let` binds:
    /// its certain type, or why it has none — read for the binders the file
    /// exports by [`FileContribution::of`], like every field below.
    ///
    /// [`FileContribution::of`]: crate::infer_project::FileContribution::of
    module_values: HashMap<DefId, ModuleValue>,
    /// The types this file declares that an annotation may name as a
    /// [`TyKind::Project`], with their path and arity, so a later file's
    /// annotation naming one ([`Resolution::EarlierFileType`]) bridges exactly
    /// as this file's would.
    project_types: HashMap<DefId, (TyPath, usize)>,
    /// The cases of this file's unions inference types, by the case's item,
    /// so a later file's use of one types exactly as this file's would.
    project_cases: HashMap<ItemId, ProjectCase>,
    /// How many constructor-like declarations of each name the file makes,
    /// for later files' case screen.
    constructor_names: HashMap<String, usize>,
    /// The records this file declares whose fields inference reads.
    project_records: ProjectRecords,
    /// See [`Self::deepest_expression_walk`].
    deepest_expr_depth: usize,
    /// See [`Self::type_budget_exceeded`].
    type_budget_exceeded: Option<TypeBudgetExceeded>,
    /// See [`Self::type_walks`].
    type_walks: TypeWalks,
}

impl InferredFile {
    /// This file's inference with every type's paths translated from `from`'s
    /// table to `to`'s, so it compares with an inference made against `to`.
    #[cfg(feature = "test-support")]
    pub(crate) fn rekey_names(&self, from: &AssemblyEnv, to: &AssemblyEnv) -> InferredFile {
        let InferredFile {
            types,
            def_types,
            member_resolutions,
            incompleteness,
            receiver_types,
            module_values,
            project_types,
            project_cases,
            constructor_names,
            project_records,
            deepest_expr_depth,
            type_budget_exceeded,
            type_walks,
        } = self;
        let tys = |map: &HashMap<TextRange, Ty>| {
            map.iter()
                .map(|(range, ty)| (*range, ty.rekey(from, to)))
                .collect()
        };
        InferredFile {
            types: tys(types),
            def_types: def_types
                .iter()
                .map(|(def, ty)| (*def, ty.rekey(from, to)))
                .collect(),
            member_resolutions: member_resolutions.clone(),
            incompleteness: incompleteness.clone(),
            receiver_types: tys(receiver_types),
            module_values: module_values
                .iter()
                .map(|(def, value)| (*def, value.rekey(from, to)))
                .collect(),
            project_types: project_types
                .iter()
                .map(|(def, (path, arity))| (*def, (path.rekey(from.names(), to.names()), *arity)))
                .collect(),
            project_cases: project_cases
                .iter()
                .map(|(id, case)| (*id, case.rekey(from, to)))
                .collect(),
            constructor_names: constructor_names.clone(),
            project_records: rekey_records(project_records, from, to),
            deepest_expr_depth: *deepest_expr_depth,
            type_budget_exceeded: *type_budget_exceeded,
            type_walks: *type_walks,
        }
    }

    /// How deeply the walk nested expressions at its deepest: the most
    /// expression levels it had open at once. The walk recurses once per
    /// level, so this is what its stack grows with; the stack-bound test
    /// (`tests/all/walk_stack_bound.rs`) reads it to know the walk reached
    /// the depth it is measuring, since a walk that declines at the top of a
    /// tall expression survives any stack.
    pub fn deepest_expression_walk(&self) -> usize {
        self.deepest_expr_depth
    }

    /// Which bound of the type budget ([`MAX_TYPE_NODES`],
    /// [`MAX_TYPE_DEPTH`]) the file's inference reached, if it reached one.
    ///
    /// A file that reaches the budget publishes no inferred type at all: no
    /// expression type, binder type, member resolution or receiver type, and
    /// every module-level value declines with
    /// [`PrecedingGap::TypeBudget`] for the files after it. Inference stops
    /// walking at the first type past the budget, having built at most the
    /// budget's worth of it, and what it worked out before that point is
    /// discarded with the rest: a type it never finished may feed any of it.
    /// What the file declares — its types, union cases and records, which
    /// inference reads off declarations rather than infers — is published as
    /// usual.
    ///
    /// [`MAX_TYPE_NODES`]: crate::MAX_TYPE_NODES
    /// [`MAX_TYPE_DEPTH`]: crate::MAX_TYPE_DEPTH
    pub fn type_budget_exceeded(&self) -> Option<TypeBudgetExceeded> {
        self.type_budget_exceeded
    }

    /// The largest type walks the file's inference made: a measurement, for
    /// how far real code sits below the type budget.
    pub fn type_walks(&self) -> TypeWalks {
        self.type_walks
    }

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

    /// The synthesized type of a `(expr).Member` access's receiver expression,
    /// keyed by the whole `DOT_GET_EXPR`'s range, whether or not the access
    /// resolves.
    ///
    /// This is dot-completion's question — what is the receiver, while the
    /// member name is still being typed (`"hi".Le`) — and it is deliberately not
    /// an expression node: FCS keeps no node inside the receiver of an access
    /// it rejects, so [`Self::type_at`] withholds the receiver until the access
    /// resolves. A receiver is never coerced, so its synthesized type is its type
    /// either way.
    pub fn dot_get_receiver_type(&self, dot_get: TextRange) -> Option<&Ty> {
        self.receiver_types.get(&dot_get)
    }

    /// Why each counted binding was incomplete, one entry per binding in walk
    /// order (empty for a complete binding; a reason repeats if it was met
    /// more than once). A measurement, not a result: it is what the corpus
    /// sweep reads to say which unmodelled construct is holding real code back.
    ///
    /// Counted means walked, or declined for its own annotation
    /// ([`Incomplete::BindingAnnotation`]), which is the one reason the walk
    /// declines a whole binding for that a later stage of inference is meant
    /// to remove — so the bindings it stops stay in the denominator. Other
    /// whole-binding declines are not counted: a `let rec` group, a binding
    /// that may be `[<EntryPoint>]`, a `let mutable` function, a pattern shape
    /// the walk does not read.
    pub fn incompleteness(&self) -> &[Vec<Incomplete>] {
        &self.incompleteness
    }

    /// What this file publishes for the module-level binder `def`, or `None`
    /// when `def` is not one the walk met as a module-level binder.
    pub(crate) fn module_value(&self, def: DefId) -> Option<&ModuleValue> {
        self.module_values.get(&def)
    }

    /// Every type this file declares that an annotation bridges, with its
    /// path and arity. See [`Self::project_types`](InferredFile).
    pub(crate) fn project_types(&self) -> &HashMap<DefId, (TyPath, usize)> {
        &self.project_types
    }

    /// Every case of this file's unions that inference types, by the case's
    /// item. See [`Self::project_cases`](InferredFile).
    pub(crate) fn project_cases(&self) -> &HashMap<ItemId, ProjectCase> {
        &self.project_cases
    }

    /// How many constructor-like declarations of each name the file makes.
    /// See [`Self::constructor_names`](InferredFile).
    pub(crate) fn constructor_names(&self) -> &HashMap<String, usize> {
        &self.constructor_names
    }

    /// Every record this file declares whose fields inference reads. See
    /// [`Self::project_records`](InferredFile).
    pub(crate) fn project_records(&self) -> &ProjectRecords {
        &self.project_records
    }

    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

/// Why a binding was marked walk-incomplete: the census behind
/// [`InferredFile::incompleteness`].
///
/// An incomplete binding fires no argument check and never generalises, so one
/// unmodelled construct anywhere in a function body switches most of inference
/// off for the whole body. The variants are coarse on purpose — one per *kind*
/// of thing inference does not model — so that "the bindings whose only
/// observed reason is X" ranks what modelling X could unlock. It is a
/// heuristic, not a bound in either direction: the walk does not descend into
/// what it does not model, so a reason beneath an unmodelled construct goes
/// unrecorded; and a failure's effect elsewhere (a local aliasing an open
/// local) can surface as a reason of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Incomplete {
    /// An expression form inference does not model at all, by its syntax kind:
    /// a `match`, a record, a bang or `use`/`rec` `let`, …
    Unmodelled(SyntaxKind),
    /// A value reference the resolver resolved to none of an in-file binder,
    /// a referenced assembly's member, or a preceding file's top-level value:
    /// a name it deferred, a case, a type.
    NotInFileValue,
    /// A use of a preceding Compile-order file's top-level value whose type
    /// that file did not publish, and why.
    PrecedingValue(PrecedingGap),
    /// A use of a referenced assembly's member (`List.map`, a bare `id`) whose
    /// F# type is not read off its signature, and why.
    AssemblyValue(FunctionTyGap),
    /// A member access or call rooted at a qualified path rather than a value:
    /// a static member (`String.Join`), or a member of a module value
    /// (`M.v.Length`).
    QualifiedPath,
    /// A literal whose type is not fixed in isolation (`1I`, `__LINE__`).
    DeferredLiteral,
    /// A lambda: its function type is not modelled.
    Lambda,
    /// A `while` loop: its `unit` type is not modelled.
    While,
    /// An `if` without a final `else`, whose result is `unit`.
    ElselessIf,
    /// A struct tuple.
    StructTuple,
    /// An expression-level `let` that is not `let pat = rhs`: a local
    /// function, a return annotation, an attribute that may be `EntryPoint`.
    LocalBindingShape,
    /// A local whose RHS is not ground when it is bound.
    OpenLocal,
    /// A statement whose type is not ground when it is walked.
    OpenStatement,
    /// A condition whose shape is not modelled: `x && y`, a call, a member.
    ConditionShape,
    /// A condition on a name that is not a parameter of the binding.
    ConditionNotParameter,
    /// A condition on a parameter that is not the parameter's first occurrence.
    ConditionNotFirstOccurrence,
    /// A pattern annotation outside the modelled set, or around anything but
    /// a name.
    PatternAnnotation,
    /// A member access on a value of a project type, whose members are not
    /// modelled: the access is left untyped.
    ProjectTypeMember,
    /// A module-level binding whose own annotation — `let x : T = …`,
    /// `let f x : T = …`, `let (x : T) = …`, or one inside a `let` pattern —
    /// is outside the modelled set. The binding is not walked at all: the
    /// annotation fixes the binder's type, and nothing else may be read in its
    /// place. Unlike [`Self::PatternAnnotation`] on a parameter, which leaves
    /// the rest of the body walked, this is the only reason such a binding can
    /// have.
    BindingAnnotation,
    /// A function whose head declares a type variable (`let f<'a> …`) the
    /// body fixes or merges with another: FCS rejects it (a declared variable
    /// is rigid), so its binder is not published and its uses are not typed.
    RigidTypeVariable,
    /// A use of a value whose type, as solved, takes a parameter FCS loosens
    /// at a use (`obj`, an interface, an unsealed class) where the use was not
    /// typed through that loosening: `let f (g: obj -> 'a) = g` is
    /// `(obj -> 'a) -> 'b -> 'a` in FCS. Its variables are poisoned.
    LoosenedUse,
    /// A use of a referenced assembly's union case whose F# type is not read
    /// off its maker, and why (`function_ty::union_case_scheme`).
    AssemblyCase(FunctionTyGap),
    /// A `match` pattern that would get ahead of a relation FCS applied before
    /// it in source order which inference has not (a pending argument check,
    /// a dropped check-mode relation, a suspended member lookup), or that
    /// would refine an open scrutinee after an unmodelled construct.
    PatternAfterCheck,
    /// A `match` pattern that does not fit its scrutinee's type.
    PatternMismatch,
    /// A `match` clause no value may reach, which FCS's decision compiler
    /// keeps no node of (decided by the private `match_reach` module).
    UnreachableClause,
    /// A `let` pattern holding an annotation, whose relation to the RHS is a
    /// coercion.
    CoercedPattern,
    /// A name bound twice in one binding's parameters or pattern.
    DuplicateBinder,
    /// A `let` pattern whose RHS type holds an earlier binding's open
    /// variable, on which the pattern would impose structure.
    InheritedOpen,
    /// Any other pattern, by its syntax kind: a constructor, a record, a
    /// literal, …
    PatternShape(SyntaxKind),
    /// A callee that is not a value or a curried application, by its syntax
    /// kind: an operator that is not a plain name, a type application, a
    /// lambda, …
    CalleeShape(SyntaxKind),
    /// A method call whose callee is not modelled, by the callee's syntax kind
    /// (a receiver that is not an in-file value, …).
    MethodCallee(SyntaxKind),
    /// A member access whose path is not modelled (an active-pattern segment).
    MemberAccessShape,
    /// A method call whose arguments are not a plain positional list.
    MethodArgShape,
    /// A syntax-recovery hole: a missing sub-expression, an empty paren.
    Recovery,
    /// A use of an in-file binder whose attributes may make F# reject it (an
    /// `[<Obsolete>]`, a `[<CompilerMessage>]`), which FCS then types as its
    /// error type rather than by the definition.
    RejectedUse,
    /// An infix application whose operator inference does not type, and why.
    Operator(OperatorGap),
    /// A use whose type parameter carries `equality` or `comparison`, solved
    /// to a type that does not satisfy it or is not known to.
    Constraint(ConstraintGap),
    /// An application of one of FSharp.Core's statically resolved operators
    /// or conversion functions whose solution F# builds in (`a + b`, `-a`,
    /// `int a`), not typed, and why.
    Builtin(BuiltinGap),
}

/// Why an application of a statically resolved operator or conversion was
/// not typed: the census's split of [`Incomplete::Builtin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BuiltinGap {
    /// An operand's type is not known where the operator is applied. FCS
    /// then leaves the constraint pending, and may solve it later by weak
    /// resolution — which defaults an operand to `int` — so what it settles
    /// on is not knowable from here.
    OpenOperand,
    /// The operand types are known and F# provably rejects them (FS0001):
    /// two different types, one of which the operator's rule solves over.
    Violated,
    /// The operand types are known but are not on the closed list the rule
    /// is modelled over: a type whose own operator members, enum underlying
    /// type or unit of measure would decide it.
    Undecided,
}

/// Why a constrained instantiation left its binding incomplete: the
/// census's split of [`Incomplete::Constraint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConstraintGap {
    /// The type provably does not satisfy the constraint (a function type
    /// compared, `obj` ordered): F# rejects the use with FS0001.
    Violated,
    /// The type is not on the closed list of those known to satisfy it, nor
    /// provably outside it: an in-file type, an F# generic, a type whose
    /// interfaces would decide comparison.
    Undecided,
}

/// Why an infix operator was not typed: the census's split of
/// [`Incomplete::Operator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OperatorGap {
    /// The resolver named no value for the operator: a deferred lookup.
    Unresolved,
    /// A referenced assembly's operator with a constraint on a type parameter
    /// — an F#-only one (`=` needs `equality`, `+` is statically resolved) or
    /// an IL one — or whose constraints were not read.
    Constrained,
    /// A referenced assembly's operator whose signature does not carry over as
    /// its F# type: a type the bridge does not carry (`@` over `'T list`), a
    /// unit of measure, or an obsolete-as-error marker (`^`).
    Signature,
}

/// What [`Gen::finish`] needs to know of a module-level binder's binding,
/// beyond its type, to decide what it publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ModuleBinder {
    /// The binding is `inline`.
    inline: bool,
    /// Its top-level module's header carries an attribute that may reject a
    /// use from outside ([`FileFacts::in_rejecting_header`]).
    header_rejects: bool,
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
/// Structured per the plan's D8: the `walk` unifies each equality where it
/// generates it and suspends what it cannot decide there, `solve` decides the
/// suspensions — a worklist waking each suspended `HasMember` once its
/// receiver's head is concrete — and each recorded expression's type is read back
/// from its variable, emitted only when fully ground (no inference variable
/// left in it).
///
/// `preceding` is what the files before this one in Compile order published
/// for their top-level values ([`crate::infer_project()`] folds it): a use the
/// resolver resolves to one of them instantiates its published type. A single
/// file inferred on its own passes [`PrecedingTypes::default`], and every such
/// use declines.
///
/// Its cost is bounded whatever the file: every walk of a type counts against
/// the type budget ([`crate::MAX_TYPE_NODES`], [`crate::MAX_TYPE_DEPTH`]) and
/// stops at it, and a file whose inference reaches it publishes no inferred
/// type ([`InferredFile::type_budget_exceeded`]), never a truncated one.
///
/// The file's name is unknown here, so the types a headerless file declares,
/// which FCS names after the file ([`infer_named_file`]), are not bridged.
pub fn infer_file(
    file: &ImplFile,
    resolved: &ResolvedFile,
    env: &AssemblyEnv,
    preceding: &PrecedingTypes,
) -> InferredFile {
    infer_file_in(file, resolved, env, preceding, None)
}

/// [`infer_file`] for a file whose [`QualifiedNameOfFile`] is `name`. A
/// file with no `module` or `namespace` header puts its contents in an
/// implicit module FCS names after the file (`Program.fs` is `Program`), so
/// the types it declares are named under that module. Only a name whose
/// every dot-separated segment is ASCII letters, digits and `_` is read; a
/// headerless file of any other name bridges none of its types. For any other
/// file the name changes nothing.
pub fn infer_named_file(
    file: &ImplFile,
    resolved: &ResolvedFile,
    env: &AssemblyEnv,
    preceding: &PrecedingTypes,
    name: &QualifiedNameOfFile,
) -> InferredFile {
    let anonymous_module = if is_headerless(file) {
        name.anonymous_module_segments()
    } else {
        None
    };
    infer_file_in(file, resolved, env, preceding, anonymous_module)
}

/// [`infer_file`], with the path of the implicit module the file's contents
/// live under when it is headerless and that path is known.
fn infer_file_in(
    file: &ImplFile,
    resolved: &ResolvedFile,
    env: &AssemblyEnv,
    preceding: &PrecedingTypes,
    anonymous_module: Option<Vec<String>>,
) -> InferredFile {
    let tys = TyTable::session();
    #[cfg(feature = "test-support")]
    {
        let (types, vars) = env.session_seed();
        let mut seeded = HashMap::default();
        for &ty in types {
            tys.import(env.types(), ty, &mut seeded);
        }
        for &v in vars {
            tys.var(TyVid(v));
        }
    }
    let facts = FileFacts::build(file, resolved, env, &tys, preceding, anonymous_module);
    let mut cx = Gen::new(&facts, &tys);
    cx.walk(file);
    let walked = cx.finish();
    facts.into_inferred(walked)
}

/// One file's walk: its immutable facts, and the three parts of what the walk
/// accumulates, each apart so that what reads or writes which is visible at
/// every use. A binding's own state is its [`Scope`], handed to each step of
/// that binding's walk rather than held here.
struct Gen<'a> {
    /// The file's immutable facts, shared with everything that reads them.
    facts: &'a FileFacts<'a>,
    /// The unification table and the constraints over its variables.
    solver: Solver<'a>,
    /// What the walk knows of each binder it met.
    binders: BinderEnv,
    /// What the walk emits for [`Gen::finish`] to read.
    out: Emissions,
}

impl<'a> Gen<'a> {
    fn new(facts: &'a FileFacts<'a>, tys: &'a TyTable) -> Self {
        Gen {
            facts,
            solver: Solver::new(tys),
            binders: BinderEnv::new(),
            out: Emissions::new(),
        }
    }

    /// The inference variable for a binder, created on first reference. A use
    /// and its binding thus share one variable regardless of which the walk
    /// reaches first — unification is order-independent.
    fn def_var(&mut self, def: DefId) -> TyVid {
        if let Some(v) = self.binders.var(def) {
            return v;
        }
        let v = self.solver.table.fresh();
        self.binders.bind_var(def, v);
        v
    }
}
