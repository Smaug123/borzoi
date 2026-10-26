# Sema Phase 3 implementation plan — best-effort inference

> **Status:** Phases 3.1–3.3 and 3.x-inh are complete; overload resolution has
> landed through OV-7 and OV-9, with OV-8 (betterness, optional) the only
> outstanding overload slice. This is the implementation breakdown for Phase 3
> of [`type-checker-plan.md`](type-checker-plan.md) (best-effort type inference:
> hover + dot-completion), mirroring
> [`completed/sema-phase1-impl-plan.md`](completed/sema-phase1-impl-plan.md).
> Everything below the "Landed stages" list has detail *only* on what remains.

## Substrate (reference)

Per [D8](type-checker-plan.md#d8-inference-substrate-ena-worklist-not-smt),
inference is **generate → solve**: generation is a pure fold over the resolved
AST producing an inert `Vec<Constraint>`; the solver discharges equality
constraints by union-find (`ena`), with type-directed member lookup (`expr.Foo`)
and function application deferred to *suspended* constraints woken when a
receiver / callee type becomes concrete. `infer_file` only ever emits **ground**
types (`Ty::is_ground`), so an inference `Ty::Var` never reaches a consumer — the
D5 "say nothing when unsure" contract is enforced at read-off. Each new stage
lands on its own branch, stacked as necessary, so a reviewer can review each in
isolation.

## Landed stages (one line each)

- **3.1** (PR #633) — `Ty` + sound literal typing.
- **3.2a** (PR #643) — unification substrate: `ena` + `Ty::Var` + the
  generate→solve plumbing, with literal typing re-routed through it (output
  byte-identical to 3.1).
- **3.2b-1** (PR #646; hover surfacing #650) — value-reference propagation: a
  binder's type flows down an unannotated simple-name `let` chain, typed only in
  the coercion-free RHS position.
- **3.2b-2** (PR #663) — paren transparency + reference tuples (new `Ty::Tuple`).
- **R1** (PR #668) — resolver records `Deferred(ShadowableType)` at uncertain
  type-position defers, so "no record" is a reliable no-shadow-possible signal.
- **3.2b-3 / R2** (#851–#864, 2026-07-08) — annotated-binder typing gated on R1's
  no-shadow signal, per
  [`r2-annotation-typing-plan.md`](completed/r2-annotation-typing-plan.md) (stages
  R2-0–R2-e).
- **3.2c-1** (PR #678) — the bidirectional recursive typer
  (`infer_expr(e, expected)`, synth/check modes) + `if`/`then`/`else`.
- **3.2c-2a** (PR #685) — function/lambda/`while` body traversal carrying the
  correct bidirectional mode (the function *value* still defers).
- **3.2c-2b** (PR #701) — `Ty::Fun` + monomorphic function-type emission + sound
  condition typing via private per-parameter slots; validated by a new
  `fcs-dump binder-types` oracle.
- **3.2c-2c** — `let`-generalisation + instantiation + canonical typar rendering
  (`let f x = x` ⇒ `'a -> 'a`), built on per-binding walk-completeness, the
  check-mode poison set, per-binding Algorithm-W solve, and the new
  `Ty::Param` variant.
- **3.2c-3** — function application v1 (`Expr::App`, no worklist): a ground callee
  grounds the result; polymorphic / ill-typed calls defer (poisoned).
- **3.3a** — the suspended-constraint worklist + `HasMember` typing for fields /
  non-indexer readable properties on a non-generic `Ty::Named` receiver via
  `AssemblyEnv`.
- **3.3b** — LSP member-resolution enrichment (`InferredFile::member_resolutions`
  in the resolver's `Resolution::Member` shape) feeding hover / go-to-def, plus a
  new dot-completion handler.
- **3.3c** — the application wake rule (suspended `ArgCheck`, coercion-free
  domains), closing 3.2c-3's polymorphic gap with completeness-gated,
  deferred-poison discharge.
- **3.3d** — single-candidate method-call typing (`recv.Method(args)` ⇒ the
  method's return type) behind a well-formedness / arity gate; overloaded,
  generic, static, and void methods defer.
- **3.x-inh** — member inheritance (base-class walk) for the data-member and
  method wakes, single-candidate across the whole chain, honouring name-hiding
  and assembly-name identity.
- **CE-1** — expression-level `let … in` and `e1; e2` in inference: a local
  types when its RHS is ground on its own, and anything open marks the binding
  incomplete; per [`ce-desugaring-plan.md`](ce-desugaring-plan.md).
- **Overloads OV-0–OV-7, OV-9** (#872–#899 and follow-ups) — the research probes,
  the `overloads` oracle, assembly flags, cross-assembly dedup, the speculation
  API, the applicability matcher, the instance-call engine with its
  extension-absence gate, OV-6.1 curry detection, OV-7 static calls, and OV-9's
  generator + corpus differential; per
  [`overload-resolution-plan.md`](overload-resolution-plan.md).

---

## Still to do

### Overload resolution — OV-8 (betterness, optional)

The one outstanding overload slice; fully specced in
[`overload-resolution-plan.md`](overload-resolution-plan.md) (§2.5, §6 stage
OV-8). It models FCS's betterness pass to commit calls where several candidates
are applicable but one is strictly better. It is **optional and data-gated**:
OV-9's corpus differential measured how many real calls betterness would recover
(the plan records the current commit rate and the ceiling OV-8 could add), and
that measurement is the input to the go/no-go decision. Read the overload plan's
status block and §6 OV-8 entry before starting.

Read [`overload-resolution-plan.md`](overload-resolution-plan.md) before touching
this pile: it carries the FCS algorithm with citations, the empirical landmine
catalogue (type-directed widening, `op_Implicit`, params/optional arity,
extension-vs-intrinsic betterness), and the two-sided `must_apply`/`may_apply`
sound commit rule.

### Measured blockers (the completeness census)

An incomplete binding fires no argument check and never generalises, so on real
code most of what inference could say is withheld by the *completeness* gate
rather than by any one typing rule. `infer_corpus_diff` prints, from
`InferredFile::incompleteness`, which construct is the **only observed** reason
a binding is incomplete — a heuristic ranking of what modelling it could unlock,
not a bound (the walk does not descend into what it does not model, so a reason
beneath an unmodelled construct goes unseen; and a failure's effect elsewhere,
such as a local aliasing an open local, can surface as a reason of its own). On
the stride-13
corpus sample (2026-10-26, after unconstrained operators; 2 602 walked
bindings, 38.2 % complete — 37.0 % before them, when infix operator application
was the only observed reason of 477 and present in 606):

| blocker | only observed reason of | present in |
|---|---:|---:|
| value not bound in this file (assembly / earlier file) | 487 | 666 |
| method call on a receiver that is not an in-file value | 172 | 308 |
| `use`, `let rec` or `let!` inside an expression | 64 | 90 |
| pattern annotation outside the modelled set | 51 | 331 |
| `match` | 47 | 149 |
| constrained FSharp.Core operator (`+`, `=`, …) | 33 | 58 |
| `new` | 32 | 37 |
| list or array expression | 29 | 78 |

A binding whose attribute the resolver cannot prove is not `[<EntryPoint>]` is
not walked at all (FCS fixes such a binding's type to `string[] -> int` before
checking its body), and neither is any declaration after a syntax error in its
file (recovery can spill into declarations that look intact).

Two corollaries shaped the order of work. Infix operators were the first lever
by a wide margin; with the unconstrained ones modelled, the next is a value
from an assembly or an earlier file — mostly the operands operators used to
hide (`xs |> List.map f`), which the walk now reaches. And a rule that only
fires on complete bindings — the guarded argument read-off, prototyped on
branch `infer-arg-readoff` — measured **zero** gain on the corpus while
bindings are this incomplete, so it waits until the blockers above are lower.
Re-run the sweep and update this table when one lands.

**Infix operators, prototyped and shelved** (branch `infer-operators`). Typing
FSharp.Core's arithmetic and comparisons over *ground* operands measured only
+25 agreements (most real operands are unannotated parameters, so they are
open when the operator is walked; the next lever there is FCS's weak
resolution). The harder problem was proving a use *is* FSharp.Core's operator:
F# makes a value bare-accessible through many channels, and three review rounds
each found another — an aliased `[<AutoOpen>]`, a `module rec`'s later
definition, an assembly-level auto-open, a signature-screened earlier file, an
`[<AutoOpen>]` type abbreviation exposing another type's static properties.
Neither a scoped lookup nor an enumeration of sources closed that set. The
sound route is operator resolution in the resolver itself — `+` looked up
under its compiled name `op_Addition` with the vetted machinery ordinary names
use; inference then trusts that resolution.

**The resolver half has landed.** Every name the resolver derives from a token
is FCS's `Ident.idText` (`id_text`), which for an operator spelling is its
compiled name (`borzoi_assembly::op_name`, a port of `CompileOpName` and its
inverse, which hover uses to show `op_Addition` as `(+)`). So
`+`, `( + )` and `op_Addition` are one name, a quoted `` ``+`` `` is a
different one, and with nothing in-file `+` resolves to FSharp.Core's
`Operators.(+)` through the same lookup as `id`. A *prefix* application is
named after its position (`op_name::prefix_op_name`, applied by `id_text` to
the operator heading a prefix `APP_EXPR`): `-x`, `+x`, `-.x`, `%x` use `~-`,
`~+`, `~-.`, `~%`, never the binary operator their token spells, while a `!`
or `~` operator keeps its spelling (`!r` uses `!`). `operator_name_diff`
grades the space against FCS (every operator × definition form × use form,
every prefix operator beside a decoy binary definition, plus an
opened-then-reopened `Operators`).

**The inference half: unconstrained operators.** `a OP b` is the application
`OP a b`, checked by FCS exactly as one, so inference types it as one
(`Gen::infer_infix`): the operator's value comes from whatever the resolver
says its token names. An in-file definition is an applied value like any other
(`let (+.) (a: int) (b: int) = …` makes `x +. 1` an `int`); FSharp.Core's own
operator — by its module's assembly name and public-key token — has its IL
signature read as its F# type (`function_ty::module_function_scheme`) and
instantiated, each type parameter a fresh scheme-instantiation variable. That
reading is sound only with three facts the IL cannot carry, all now read off
each module function's pickled val (`rebuild_module_member_list`): its curried
argument groups (one IL parameter per group, else it declines — `||>`'s tupled
group does), the F#-only constraints on its type parameters (any constraint
declines: `+` is statically resolved, `=` needs `equality`, so instantiating
either as a plain variable would claim a constraint is modelled that is not),
and whether its type mentions a unit of measure (IL erases them), directly or
behind a named type — `type Dist = float<m>` pickles as the bare name, so every
named head the type applies is cleared through the abbreviation chase, in
whichever assembly declares it (`module_function_measures_diff`) — plus the
val's own `[<CompilerMessage>]` and `[<RequiresExplicitTypeArguments>]`, which
reject a use and live in the pickle alone (`MethodLike::val_facts`). The
bridge carries only primitives, the function's type parameters, F# function
types and `unit`, so `@` and `:=` (over `'T list` and `'T ref`, F# types under
a compiled name) and `^` (obsolete as an error, which FCS types as `obj`)
decline too. Every FSharp.Core `Operators` function the reading admits (14,
`id` and `box` among them), bound as a value, has exactly the type FCS gives
it. `infer_operators_diff` grades a generated grid — every operator ×
operand kind (literal, annotated or open parameter, lambda, nested application,
ill-typed) × position (synth, under an annotation, as an argument, as a callee)
— plus the shadowing routes, against both FCS oracles. Measured: +19
agreements (all `&&` and `||` where the file opens `IntrinsicOperators`), 0
divergences, 37.0 % → 38.2 % complete bindings.

**`&&` and `||` resolve to nothing** unless `IntrinsicOperators` is opened
explicitly: it is a module-shaped `[<AutoOpen>]` path, which the resolver does
not fold (`AssemblyEnv::record_assembly_auto_opens`), so the token is
`Deferred(UnboundName)` and the census records `Operator(Unresolved)`. Folding
it is resolver work.

**Argument checks against a function type.** `|>`, `<|`, `>>` and `<<` fix
their results only through an argument whose domain is a function type
(`|>`'s `'T1 -> 'U`). FCS checks an argument against its parameter's type by
an equality first and a coercion only if that fails (`UnifyOverallType`), and
a function type admits no coercion but equality, so such a check discharges
as `Eq(arg, dom)` (`Gen::function_domain`). Two things make that FCS's own
relation, and both turned out to be holes in the checks that already fired,
against sealed primitives and scheme-instantiation variables:

- **The argument must have the type FCS gives it.** FCS loosens a value at
  every use (`MakeApplicableExprWithFlex`): each parameter of a type that is
  not sealed (`isSealedTy` — an interface, an unsealed class, `obj`) becomes a
  fresh variable that is a subtype of it. So `y |> fo` with `fo : obj -> int`
  leaves `y` generic, and `let g = fo` is `'a -> int` — which inference used to
  publish as `obj -> int`. `Gen::value_use` now models the loosening wherever
  the value's type is known at the use (a scheme, an earlier binder's ground
  type, a settled local, a parameter's annotation); the fresh variables are
  poisoned, and one bounded by anything but `obj` is a subtype constraint
  nothing checks, so fixing it to a type undoes the binding's checks. Where
  the type is not known at the use (an unannotated parameter), the use is
  *undecided*, and must end up taking no unsealed parameter.
- **The order FCS checks in must not matter.** FCS checks a body in source
  order, keeps the first relation to fix a variable, and reports and recovers
  a later conflicting one; it resolves a member on what it knows of the
  receiver at the access (FS0072). The worklist keeps no order. A generated
  grid of every ordered pair of uses of one parameter
  (`infer_operators_diff::every_ordered_pair_of_uses_of_a_parameter_agrees_with_fcs`)
  found 34 wrong commits on the existing rule: a dropped branch relation
  before a direct application (`((if true then 1 else p), fs p, p)` made `p` a
  `string`), a member access before the check that fixes its receiver
  (`(p.Length, fs p)`), and an unfired pipe before a direct application
  (`(p |> fs, fi p, p)` made `p` an `int`). Combined as the arguments of one
  application instead of a tuple's elements, the same pairs found the same
  classes: `(if true then p else 1) +. p` with `(+.) : int -> bool -> int`
  published `bool -> int` where FCS fixes `p` to `int` at the branch.

So `Gen::solve` is speculative: it fires every check whose gate holds under a
snapshot, and keeps the result only when `Gen::verified` — no equality and no
fired check failed; the relations it did not apply (pending checks and the
check-mode relations the walk dropped) unify with it, all together; no member
was resolved through a check that ends after it in the source; none of them
touches a variable FCS constrains through a relation inference does not hold
(`Gen::opaque`: an earlier binder's, a method call's argument outside it, a
member access's result until its wake binds it); and the loosening holds.
Then every relation is consistent as an equality, so
FCS applies each one as an equality, in whatever order, and agrees. Otherwise
the snapshot is rolled back and no check fires: each is undischarged and
poisoned, which is silence. No check fires that would bind such a variable
either (`Gen::arg_check_binds_only_current_vars`): an application gives an
earlier binder's open variable a structure (`oldf x` makes it `d -> r`) whose
grounding retypes the earlier binder, and `(System.String.IsNullOrEmpty(p),
p |> inc, p)` or `arr.Length |> incs` would fix `p` or the member's result to
what the pipe says, where FCS has fixed it from the method or the member.

`infer_operators_diff` grades it: a grid of every function kind (sealed,
generic, loosened to `obj` or to a bound, a lambda, partially applied,
composed, ill-typed) × operand × shape (`|>`, `<|`, a chain, `>>`, `<<`, a
composition as a value) × position, with a per-row obligation that the 185
well-typed rows over a function inference must type commit their
application; the ordered-pair grid, every pair of eight uses of a parameter
over four functions, combined four ways (a tuple, the two arguments of a
generic function, of an operator, and of a function whose parameters fix
them too: 4 096 functions); and a generator of programs mixing every way an
argument is related to a parameter, mostly rejected by FCS (800 files,
24 636 commits, 12 054 FCS errors, no disagreement). Each part of the
verification was checked by deleting it, and some test fails without it,
except two kept as defence in depth: the failed-equality condition (no
generated or hand-made program reaches a failed equality that matters) and
the check on variables an application gave an earlier binder (every such
structure carries a pending check on its domain, which the
earlier-binder condition already rejects).

Measured on the stride-13 corpus sample, nothing moves: 890 + 862 agreements,
0 divergences, 973 / 2 602 complete bindings, the manifest unchanged. Real
pipes run into assembly functions (`xs |> List.map f`), which are
`NotInFileValue`, so the bindings they sit in are incomplete and fire no check;
and the verification costs no agreement the old rule had on that sample.

### Remaining 3.x hard piles (census-priority order)

Each stays `Deferred` (D5 silence) until the corpus demands it
([D9](type-checker-plan.md#d9-scoping-evidence-the-bucket-census)); none has a
detailed sub-plan yet, so writing one is the first step of each:

- **Interface walk** — member resolution through implemented interfaces (the
  base-class walk landed in 3.x-inh covers base *classes* only). Also a
  prerequisite for the *complete* method group overload resolution needs: arity
  is not a sound overload proxy, so the real method group must include base-class
  **and** interface members plus argument-type matching (this is why 3.x-inh was
  sequenced ahead of the sound overload slice). **IW-0–IW-2 landed** (interface
  data members + methods + Object for an interface-typed receiver, via the new
  `interface_member_chain`; class/struct receivers unchanged — FCS does not walk
  their interfaces); **IW-3 (re-declaration hiding, qualified-path precision,
  inherited dot-completion) is the optional remainder.** Sub-plan (scope, the
  interface-DAG soundness rule, and the IW-0–IW-3 staging):
  [`interface-walk-plan.md`](interface-walk-plan.md).
- **Extension members** — currently out of scope for all of 3.3.
- **SRTP** (statically-resolved type parameters).
- **Computation-expression desugaring** — pulled forward (issue #30); sub-plan
  (the FCS member-directed translation, the probe catalogue, the CE-0–CE-8
  staging, and the typing-substrate prerequisites it is gated on — `Ty`
  generic args, F# argument-group recovery, attribute projection):
  [`ce-desugaring-plan.md`](ce-desugaring-plan.md).
- **Units of measure.**
- **Dot-completion of inherited members** — completion is still exact-entity
  only and does not offer members reached via the 3.x-inh base-class walk.
