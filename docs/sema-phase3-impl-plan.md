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
- **Preceding files' values** — inference folded over Compile order
  (`infer_project`): a use of an earlier file's top-level value is typed from
  what that file published for it; details below.
- **Earlier files' types** — a single-segment type name binds a type an
  earlier Compile-order file declares in the enclosing namespace
  (`Resolution::EarlierFileType`), and its annotation types as that type's
  `Ty::Project`; details below.
- **Annotation type variables** — a `'a` in a module-level function's head
  annotations is one inference variable per name (FCS's implicitly scoped
  type variable), and a declared one (`let f<'a> …`) is that and rigid;
  details below.
- **Referenced union cases and `match`** — a referenced assembly's union case
  types off the static maker fsc compiles it to (joined to the pickle's case
  by declaration position), as a value and as a pattern; `match` checks its
  clauses in FCS's order over case, constant, tuple and name patterns; and
  FSharp.Core's renamed generics (`Result`, `Choice`) read in annotations;
  details below.
- **The project's own union cases** — a case of a union the project declares,
  in the same file or an earlier one, as a value and in a `match` pattern,
  bare or qualified by its type; its scheme read off the declaration's field
  annotations in the union's own type-parameter scope; details below.
- **Project-type annotations** — `Ty::Project` names a type the project
  declares by FCS's path for it (namespace, enclosing modules' compiled names,
  its own) and arity, unique per assembly by FS0249; an in-file type's
  annotation bridges to it where that path is provably the written one; a
  terminal abbreviation with a structural target (`int array`) resolves; and
  FSharp.Core's own applied abbreviations (`int option`, `int array`) read
  through to the type FCS names; details below.
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
corpus sample (2026-11-06, after annotation type variables: 1 013 of 2 647
counted bindings complete, 38.3 %; 1 011 before, and 38.9 % of the 2 602
walked bindings before the census counted the bindings its own annotation
stops, below):

| blocker | only observed reason of | present in |
|---|---:|---:|
| value resolved to no in-file binder and no assembly member (an earlier file's, a deferred name) | 416 | 561 |
| callee that is an operator or active-pattern name in parentheses, `(\|P\|_\|) x` (116 in FSharp.Core's `Linq.fs`) | 118 | 119 |
| `use`, `let rec` or `let!` inside an expression | 72 | 95 |
| use of an in-file binder F# may reject for its attributes | 55 | 131 |
| `match` | 55 | 155 |
| pattern annotation outside the modelled set | 45 | 254 |
| list or array expression | 46 | 79 |
| method call whose head the resolver deferred (a project or test-framework name, alone in its file) | 50 | 155 |
| computation expression | 34 | 35 |
| binding whose own annotation is outside the modelled set (not walked) | 24 | 24 |
| `new` | 32 | 37 |
| assembly value with a constraint other than `equality`/`comparison` (its own, or a generic type's it applies) | 10 | 22 |
| statically resolved operator over an operand not known where it is applied | 2 | 18 |
| assembly operator with such a constraint, used as a value | 1 | 1 |

The sample checks each file in isolation against FSharp.Core and the BCL
alone, so most of the first row is a preceding file's value, which no
per-file sample can resolve. Inference now types those uses in a project
(*Preceding files' values*, below), which this sample cannot see by
construction: re-run after that stage, it is unchanged. The whole-project
gate prints the census over real projects twice, folded over Compile order
and each file alone (`project-corpus-diff inference census`), and that is
where the stage is measured.

Until 2026-11-05 the census also left out every binding whose own annotation
(`let x : T`, `let f x : T`, `let (x : T)`) is outside the modelled set — not
walked, so not counted — which hid exactly the bindings that blocker stops.
They are now counted under `BindingAnnotation`: 45 on the sample, and 312 of
797 on the whole-project gate (485 before), where the same 26 are complete.
The same change routes a single-segment callee (`(+)`, `(|P|_|)`) as the value
application it is: the 118 bindings it moves were filed under the method-call
row, which on this sample is otherwise a resolver deferral (a head the file
alone cannot know), and on the whole-project gate the only reason of one
binding. Neither change commits or withdraws a type.

Of the 312 annotations behind those bindings on the whole-project gate, 154
(49 %) name a type declared in the same file, 98 (31 %) a name the resolver
declines as `ShadowableType` — mostly a type an earlier file declares, which
the resolver tracks only by simple name, as a shadow guard — 29 (9 %) a type
variable, and 27 (9 %) an FSharp.Core generic abbreviation applied
(`unit option`, `string list`), which the annotation bridge declines.

The inference half landed first (*Project-type annotations*, below), since the
same-file half needs nothing from the resolver: a project type has a `Ty`, an
in-file type's annotation bridges to it, and FSharp.Core's applied
abbreviations read through to their target. Measured on the whole-project gate
it takes complete bindings from 26 to 30 of 797 and `BindingAnnotation` from
312 to 211, and hovers agreeing with FCS from 158 to 343; `match` becomes the
only reason of 45 bindings (6 before), and `PatternAnnotation`'s 60 and
`BindingAnnotation`'s 211 are now mostly earlier files' types, which the
resolver stage is for. On the stride-13 sample the count of complete bindings
does not move: the annotations it types expose the construct behind them
(`match`, list literals, an earlier file's value).

The resolver stage followed (*Earlier files' types*, below): a bare name an
earlier file's type in the enclosing namespace binds now resolves to it and
types as its `Ty::Project`. On the whole-project gate complete bindings go from
30 to 37 of 797, `BindingAnnotation` from 211 to 160 and `PatternAnnotation`
from 60 to 28 (only reason), and hovers agreeing with FCS from 343 to 622. The
next ranks are `match` (only reason of 74, present in 144), `ProjectTypeMember`
(41 of 72: member access on a project-typed receiver, which the annotations now
expose) and `OBJ_EXPR` (34 of 68).

Measured before the next stage (2026-11-06), by what each failing annotation
names rather than by the census row: of `BindingAnnotation`'s 160, 124 failed
on a type variable (`'a`, `Node<'a>`, `Teq<'a, 'b>`) and only ~30 on a project
type the resolver declines, and of `PatternAnnotation`'s 28, 25 on a type
variable. Bounding each candidate by the bindings it is the only reason of
*and* whose other sub-walks record nothing — a speculative walk beneath each
unmodelled `match` — put type variables first by a wide margin: an opaque
stand-in for each took complete bindings from 37 to 75, where `match` could
complete at most 10 (62 of its 74 hide an unresolved value beneath an arm),
`ProjectTypeMember` at most 41 (37 of them an earlier file's type's members)
and `OBJ_EXPR` at most 34 (28 of those implement `T<_>`). So the stage after
earlier files' types was type variables (*Annotation type variables*, below),
which takes complete bindings from 37 to 63 of 797, `BindingAnnotation` from
160 to 59 and `PatternAnnotation` from 28 to 9 (only reason); hovers agreeing
with FCS stay at 622, 11 parameters gained and 11 withheld — those of
functions that declare their variables (`let setParent<'a> …`) and whose body
is incomplete, which the stage withholds whole for soundness on ill-typed code
(below). The next ranks are `match` (only reason
of 102, present in 160; at most 20 completable alone), `ProjectTypeMember`
(52 of 138, most of the accesses on an earlier file's type), `OBJ_EXPR` (47 of
73) and `RECORD_EXPR` (24 of 59). What `BindingAnnotation` still names is now
mostly project types: a generic application whose argument the bridge declines
(16, `Teq<Result<'ok, 'e>, …>`), `ShadowableType` (9), a value binding's type
variable (9), in-file and earlier generic types (11).

The stage after type variables was ranked by a stub experiment rather than by
the census row (2026-11-06): each candidate replaced by a fresh variable that
records no reason, its contents still walked, and the census re-counted — an
unsound upper bound on what modelling it alone completes. Member access on a
project type bounded at 114 (+51) and object expressions at 110 (+47), but
about nine in ten of both came from one project's crate encoding
(`NodeCrate.Apply<'TEval, 'ret when 'TEval :> NodeEval<'ret>>` applied to
object expressions), and that project is 451 of the gate's 797 bindings;
`match` with union-case values bounded at 82 (+19) on the gate and 1 043 (+31)
on the stride-13 sample, the only candidate gaining on both and across
projects. So the next stage was referenced union cases and `match`
(*Referenced union cases and `match`*, below), which takes complete bindings
from 63 to 67 of 797 and hovers agreeing with FCS from 622 to 653, and the
stride-13 sample from 1 013 to 1 014. Most of the bound it left is the
project's own unions: an in-file or earlier file's case in a pattern is now
the only reason of 51 bindings (`PatternShape(LONG_IDENT_PAT)`), the next
lever, ahead of `ProjectTypeMember` (54), `OBJ_EXPR` (47) and
`BindingAnnotation` (44, from 59 now that `Result<…>` reads).

A binding whose attribute the resolver cannot prove is not `[<EntryPoint>]` is
not walked at all (FCS fixes such a binding's type to `string[] -> int` before
checking its body), and neither is any declaration after a syntax error in its
file (recovery can spill into declarations that look intact).

Two corollaries shaped the order of work. Infix operators were the first lever
by a wide margin; with the unconstrained ones modelled, the next was a value
from an assembly or an earlier file — mostly the operands operators used to
hide (`xs |> List.map f`), which the walk now reaches. The assembly half of
that has landed (below); what is left of the row is earlier files' values and
assembly values the reading declines. And a rule that only
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
(`let (+.) (a: int) (b: int) = …` makes `x +. 1` an `int`); an assembly's
operator — FSharp.Core's, or since assembly module values (below) any F#
assembly's — has its IL
signature read as its F# type (`function_ty::module_function_scheme`) and
instantiated, each type parameter a fresh scheme-instantiation variable. That
reading is sound only with three facts the IL cannot carry, all now read off
each module function's pickled val (`rebuild_module_member_list`): its curried
argument groups (each group's size, which splits the IL parameters, so
`||>`'s tupled first group reads as `'T1 * 'T2`), the F#-only constraints on its type parameters (any constraint
declines: `+` is statically resolved, `=` needs `equality`, so instantiating
either as a plain variable would claim a constraint is modelled that is not),
and whether its type mentions a unit of measure (IL erases them), directly or
behind a named type — `type Dist = float<m>` pickles as the bare name, so every
named head the type applies is cleared through the abbreviation chase, in
whichever assembly declares it (`module_function_measures_diff`) — plus the
val's own `[<CompilerMessage>]` and `[<RequiresExplicitTypeArguments>]`, which
reject a use and live in the pickle alone (`MethodLike::val_facts`). `^`
(obsolete as an error, which FCS types as `obj`) declines too; `@` and `:=`,
over `'T list` and `'T ref`, are typed since the bridge carries generic named
types (below). Every FSharp.Core `Operators` function the reading admits (14,
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

**Assembly module functions and values.** A use the resolver resolved to a
referenced F# assembly's module member (`Resolution::Member` of a module: a
bare `id` or an opened module's name at its token, a qualified `List.map` or
`Microsoft.FSharp.Core.Operators.id` over the whole path, where the resolver
keys a qualified member) is typed as FCS types a use of a value (`TcVal`):
the member's IL signature read as its F# type, each type parameter
instantiated afresh, and the value loosened at the use
(`MakeApplicableExprWithFlex`) exactly as `Gen::value_use` loosens a
generalised in-file binder (`Gen::scheme_use`, shared by both). That holds in
every position — a bare value, applied, partially applied, piped, composed,
passed as an argument, a parenthesised callee, a module-qualified callee (a
value application, not a method call) — and for infix operators of any F#
assembly. `module_function_scheme` splits the IL parameters by the pickled
argument groups (`MethodLike::arg_groups`, `CurriedArgs::split`): a value has
none (`let x = …`, and type functions such as `List.empty`), a group of one
parameter is its type, a group of several their tuple
(`let f (a, b) c = …`), and the sole empty group `unit`. It carries reference tuples, vectors (FSharp.Core's
`[]`1` pickle head hides no measure) and non-generic named types: the type
the reference's own scope resolves to (following forwarders), when it is the
only type at its path and F# names it as the IL does — `Ty` keeps no
assembly, so a type two assemblies declare declines on both sides — and
generic named types (below). It also declines a val another assembly cannot
access, and one whose module, or a module enclosing it, carries an
error-level `[<Obsolete>]` or `[<CompilerMessage>]`, which FCS checks on the
way to the val. A preceding file's value, an inaccessible val (the resolver records
nothing) and a deferred name stay `NotInFileValue`; a member the reading
declines records `AssemblyValue(gap)`. A first-class `reraise` is rejected
only after inference (FS0417), which leaves its type standing, so it is typed.

`infer_assembly_values_diff` grades it against both FCS oracles over a
generated fixture assembly (`module_value_corpus`): 69 signature shapes
(curried, generic, unit group, tupled group, tupled groups beside curried
ones, tuple-typed parameter, function returned, function taken, `obj` taken,
named types, arrays, values, type functions, inline, `[<CompiledName>]`,
warning-level attributes, and the declines — constraints, measures, renamed
types, `[<RequiresExplicitTypeArguments>]`, obsolete and
compiler-message errors, an internal val, a literal) × 14 use forms × 6 positions × 3
spellings, with a per-row obligation on every well-typed synth row whose FCS
type is ground (3 753), a census check that each declined shape records
its reason, and an ill-typed twin per shape; the pipe and use-order grids of
`infer_operators_diff` with their functions relocated into the fixture, with
the in-file grids' obligations unchanged; the argument-check generator
relocated, with every commit the in-file program makes required of the
relocated one; operators and active patterns outside FSharp.Core; and every
FSharp.Core module member the reading admits (279), returned from a function.
Each gate of the reading was checked by deleting it; some test fails without
each, except five guards no input reaches (a leading receiver, which an
augmentation already declines before it; agreement between the val's value
flag, group count and groups, which come from one val; an empty group beside
others, which F# never pickles; a path whose head is an in-file binder, which
the resolver never records as a module member; and a named type renamed by
`[<CompiledName>]`, whose IL name the env's source-name index already misses).
Measured on the stride-13
corpus sample: 890 + 862 → 899 + 870 agreements (17 added, none lost), 0
divergences, 973 → 987 complete bindings; bridging every named type, unchecked,
added nothing over that.

**Generic named types.** The bridge carries a generic named type applied to
arguments it carries in turn — `'T list`, `'T option`, `Result<'T, 'E>`,
`seq<'T>`, `ResizeArray<'T>`, `KeyValuePair<_, _>`, a user's generic record,
union or class, nested and in arrays — as `Ty::Named` with its arguments, the
representation annotations already use. Its path is the compiled one
(`Microsoft.FSharp.Collections.FSharpList`), which is FCS's canonical
rendering, so the rules are those of a non-generic type over that path:

- the reference's own scope resolves it (following forwarders), and it is the
  only type in its namespace, at its arity, that its IL or source name could
  be read back as (`AssemblyEnv::sole_generic_type_referenced`); a namespace
  with a dropped type declines;
- F# names it as the IL does, or it is one of FSharp.Core's renamed generics
  (`FSHARP_CORE_RENAMED`: `list`, `option`, `voption`, `ref`, `Result`,
  `Choice`, `Async`, `Map`, `Set`), checked to be FSharp.Core's own type with
  that source name; `Ty::render_fsharp` shows those by their F# name, since
  `FSharpList` cannot be written in F#;
- no type parameter of it is constrained (`Map`, `Set`, `Nullable`, a user's
  `when 'T : comparison`), since nothing checks an argument against a
  constraint; a type with a measure parameter, which IL erases, declines at
  the val's measure check first, directly or through an abbreviation;
- not byref-like (`ReadOnlySpan<_>`), not `System.Tuple`/`System.ValueTuple`
  (FCS renders those structurally), not nested.

Two rules of the use follow FCS for the new types. `MakeApplicableExprWithFlex`
loosens a parameter whose type is not sealed (`isSealedTy`): an F# union,
record or struct, and a `[<Sealed>]` class, are sealed, an interface
(`seq<'T>`) or an unsealed class (`ResizeArray<'T>`) is not and is loosened
to a bounded variable as before. The sealed test reads a generic type by its
compiled path (`AssemblyEnv::generic_type_at_compiled_path`), since the
source-name index does not hold `FSharpList` under that name. And an
argument checked against a sealed generic domain discharges as an equality
(`Gen::no_subsumption_ty`), as against a sealed primitive: FCS checks a
function argument by an equality first (`UnifyOverallType`), and no subtype
coerces into a sealed type, so `List.map f xs` with `xs` an assembly value
fixes its result. Member access on a generic receiver still parks, and
annotations still decline a renamed or abbreviated generic head (`int list`,
`ResizeArray<int>`), so neither path met these types before and neither
changes.

`infer_assembly_values_diff` adds the dimension to its generated fixture:
F# list (nested, in an array, through a user abbreviation), option (of a list,
a tuple, a function), `Result`, `voption`, `ref`, `Choice`, `Async`,
`seq` taken and returned, `ResizeArray`, a BCL struct and delegate, user
generic records, unions, classes sealed and not, an abbreviation of a
generic instantiation, and the declines (a constrained typar, `Map`,
`Nullable`, measure-parameterised generics directly and through an
abbreviation, a renamed user generic with and without the arity suffix, a
struct tuple, a one-tuple, a nested generic, byref-like spans), each × the
existing use forms, positions and spellings with ill-typed twins: 137 shapes,
6 834 per-row obligations. The twin assembly declares a colliding generic
(both sides decline) and generic types of its own (read through the twin's
scope, and committed). The FSharp.Core sweep now reads 638 module members
(279 before), every one FCS's type, and each of the 450 whose parameters are
all of a shape FCS treats as sealed must commit when returned. Each new gate
was checked by deleting it; some test fails without each, except three kept
as defence in depth (the forwarded target's own name, which the arity-aware
path walk already pins; the FSharp.Core identity of a renamed type, which the
sole-occupant check already enforces against an impostor at its path; and the
top-level full-name comparison, which the rename and nesting checks already
decide). Treating every generic domain, sealed or not, as an equality also
survives: FCS's function arguments are equality-first, so this is the
existing conservatism of `no_subsumption_ty`, not a soundness gate.

Measured on the stride-13 corpus sample: agreements unchanged at 899 + 870
(the manifest does not move), 0 divergences, 987 → 994 complete bindings;
`AssemblyValue(Type)` falls from 11 / 76 to 3 / 5 (only observed / present),
and the reasons that rose in its place are lambdas (present in 52 → 71),
constrained operators (65 → 71), computation expressions (only observed in
24 → 34) and `AssemblyValue(Constrained)` (present in 34 → 36). Every FSharp.Core function over `seq<'T>` (the `Seq` module,
`List.ofSeq`, `Array.concat`, …) still owes nothing: FCS loosens the
interface to a bounded variable and condenses it back on generalisation,
which inference does not model.

A member access on an F# record, union or exception type publishes no
receiver node: FCS reads a record field as a field get, under which its typed
tree keeps no node at the receiver. This was a wrong commit before assembly
values, through an annotation (`let b (v: R) = v.Q` published `v`), which the
member-access grid of BCL receivers could not reach; a grid over F#-authored
receiver kinds (record, union, class, struct record) now does.

Opening any F# assembly module stops the static method-call engine committing
(`System.String.IsNullOrEmpty(h)` commits in a file with no `open` and not in
one that opens a fixture module with no augmentation), the extension-absence
gate's conservatism; the relocated generator compares against an in-file
baseline that opens such a module for that reason. And `let applyTo g x = g x`
does not generalise (an applied unannotated parameter's use is undecided), so a
function passed to an in-file higher-order function owes no commit.

**The oracle grades around what FCS cannot convert.** The `types` and
`binder-types` oracles read FCS's public expression view, which FCS converts
from its typed tree one binding body at a time, re-solving each call's witness
arguments as it goes. A call whose type arguments violate a constraint (`g = g`
with `g : int -> int`, a `[<NoEquality>]` record compared) re-raises the
constraint error there, and a pattern match FCS recovered from throws `"error
recovery"`; either used to sink the whole file. fcs-dump now converts per leaf
declaration and reports each one that throws as `Unconverted` (FCS's range for
it, its name and the message), with nothing from inside it. The sweep widens
each such range to the source declaration around it — FCS's range stops short
of it, at the first local of a `let`-chained body — and records every commit of
ours there as **ungraded**, never as agreement or disagreement, whether or not
FCS reported an error beside it; an unconverted declaration it cannot place
skips the file. Every other parser of the dumps refuses a payload with an
unconverted declaration rather than read its missing nodes as FCS's answer.
Reaching those files exposed one oracle hang: FCS reports a unit-of-measure
abbreviation (`[<Measure>] type sqrm = m^2`) as an abbreviation whose
`AbbreviatedType` is itself, and every abbreviation-chasing renderer in
fcs-dump looped on it; they now stop at a measure.

Measured on the stride-13 corpus sample: fcs-failed 30 → 4 (the rest have no
implementation file or are scripts), compared files 325 → 351, agreements
899 + 870 → 963 + 933 (127 added, all in newly compared files, none lost),
0 divergences, 100 unconverted declarations holding 5 ungraded commits.
Completeness is unchanged: it is tallied before the oracle runs.

**`equality` and `comparison` constraints.** A module function whose type
parameters carry F#'s `equality` or `comparison` constraint and nothing else
— `= <> < > <= >=`, `compare`, `hash`, `max`, `List.contains`, `List.sort`,
`dict`, … — is read like any other (`module_function_scheme`). The pickle
reading records which of the two a type parameter carries
(`FSharpConstraints::Supports`, beside `Free`, `Present` for any other
constraint, and `Unknowable`); F# pickles `equality and comparison` as
`comparison` alone. The two constraints unify nothing — FCS's
`SolveTypeSupportsEquality`/`SolveTypeSupportsComparison` add the constraint
to a variable or check a type and report FS0001 — so the use types as its
signature says, and each constrained instantiation variable is:

- **checked** over the binding's final substitution, against a closed list read
  off those two functions: a function type fails both; tuples and arrays
  recurse; the sealed primitives and `string` satisfy both; `unit`, `obj` and a
  non-generic type of an assembly that is not F#-authored (absent
  `[<NoEquality>]`) satisfy equality; `obj` fails comparison. Anything else — an
  F# record, union or class, a generic type, comparison through an interface
  hierarchy — is undecided. A violation or an undecided type marks the binding
  `Incomplete::Constraint(Violated | Undecided)`;
- **refused speculation**: `Gen::verified` rejects a solve whose constrained
  instantiations fail or are undecided. A violation changes no type in
  itself, but FCS meets it inside an argument check (`UnifyOverallType`),
  undoes the equality it was solving and reports the error with the rest of
  that unification never applied: `a |> hash` over an `int * (int -> int)`
  leaves the pipe's result `'a` in FCS. The grid found this; the design had
  assumed types were the same either way. The verdict is taken with the
  relations the solve left unapplied applied too, as `verified`'s probe
  applies them: a branch joining an open parameter to a function can be the
  only thing that fixes a constrained instantiation;
- **never generalised while open**: it is poisoned when it is made. The
  canonical renderings do not show `when`, so a scheme dropping
  `when 'a : equality` would pass every types oracle while hover showed it
  wrong. The `binder-types` oracle now reports each binder's type-parameter
  constraint kinds (`TyparConstraints`), and the graders (`check_in`, the
  corpus sweep, the local-let differential) count a generic binder type FCS
  constrains as a disagreement.

`infer_equality_constraints_diff` is the grid: twelve uses (FSharp.Core's
operators and functions, prefix and qualified operator spellings, the
fixture's own `eqC`/`cmpC`) × 34 operand sorts (every primitive, `unit`,
`obj`, tuples and arrays with and without a function, BCL classes, an
interface, a BCL struct, F# records and a class from the fixture — one with a
function field and no equality — and in-file records plain, `NoEquality`,
`NoComparison`, `CustomEquality`) × annotated, nested, piped, literal,
half-open and branch-joined operands in synth, argument, tuple and branch positions, plus open
and rigid-`'T` operands. Its table of provable sorts is checked against FCS
(a sort called provable never draws an error); each file FCS accepts over a
provable sort must leave every binding complete and commit every synth row
(1 526 per-row obligations); each FS0001 twin (84) must record
`Incomplete::Constraint` where inference reads the operands. The
assembly-values shape grid admits the fixture's `eqC`, `cmpC` and `bothC`,
and declines `null`-constrained controls. Deleting each gate fails a test —
the ground check, the speculation refusal, the no-generalise poison, the
kind filter (admitting any constraint), `obj` and other named types under
comparison, the F#-authorship test, the function-type verdict — except the
rigid-type-parameter verdict, which no input reaches (an annotation naming a
type parameter declines first).

Measured on the stride-13 corpus sample: agreements 963 + 933 → 978 + 945
(27 added in 12 files, none lost), 0 divergences, complete bindings
994 → 1 010, constrained operators 37 / 71 → 16 / 36 and constrained
assembly values 17 / 36 → 17 / 30 (only observed / present). Two commits in
an unconverted declaration (a `[<NoEquality>]` record compared) are ungraded.

Not done: generalising over a constrained variable with the constraint in the
scheme and `when` in hover (graded by the `tooltips` oracle), and widening the
provable list — F# types' derived equality and comparison, FSharp.Core's
`EqualityConditionalOn`/`ComparisonConditionalOn` generics (`list`,
`option`), comparison through a BCL type's interfaces, `Map` and `Set` in the
bridge. Generalising was measured before it was built, by letting a
constrained variable generalise with no constraint carried at all (unsound,
so an upper bound): on the stride-13 sample it moved **nothing** — no
agreement, no complete binding, and no binder FCS constrains among the ones
committed. Generic functions comparing their own parameters are rare in real
code, and the few in the sample sit in bindings incomplete for another reason.
It waits until those reasons fall.

**Statically resolved operators over known primitive operands.** `+`'s
signature is `^T1 -> ^T2 -> ^T3`: it says nothing of the result until the
member constraint is solved, and over the primitive types F# solves it by a
built-in rule (`SolveMemberConstraint` in ConstraintSolver.fs), not a member
lookup. `function_ty::builtin_operator` recognises the functions that rule
covers — FSharp.Core's own `Operators` (by assembly identity and full name,
so `Operators.Checked` and any other module are not), each name on a closed
list, every type parameter carrying an F#-only constraint and nothing in IL,
and the IL signature the shape the rule assumes: `+ - * / %`,
`&&& ||| ^^^`, `<<< >>>`, prefix `-`, and the conversion functions (`int`,
`float`, `char`, `decimal`, …). `infer_builtin_operators_diff`'s recogniser
test pins the list against FSharp.Core exactly.

FCS applies the rule where it checks the application, from the operand types
it knows there; an operand it does not know yet leaves the constraint pending,
to be solved when it is known or, failing that, by weak resolution, which
defaults it to `int` (`IsBinaryOpOtherArgType`). So inference
(`Gen::builtin_application`) types only three shapes — `a OP b`, `-a`, and a
conversion applied by name — walks the operands as the arguments they are,
settles the binding (`Gen::settle`, which applies only equalities FCS has made
by that point in the source), and reads the operand types there:

- every operand known and on the rule's closed list (`builtin_verdict`): the
  application is typed — the operands' one type for `+ - * / %` and the
  bitwise operators (a number; `+` also `char` and `string`, `-` also `char`;
  bitwise an integer), the left operand's for a shift over an `int`, the
  operand's for `-` over a signed integer, float or `decimal`, the target for
  a conversion from a number, `char` or `string`;
- an operand not known there: `Incomplete::Builtin(OpenOperand)`, untyped —
  except a conversion, whose signature fixes its result (`^T -> int`) and
  which FCS unifies with the application's type before it checks the operand,
  so it is typed whatever the operand is or becomes;
- two different named types the rule takes one of: `Builtin(Violated)` — the
  rule fires and equates them, which fails — except `&&&`'s operands, which
  share one type parameter, so FCS checks the right operand against the
  left's type and inserts F#'s `int` widening (`int64 &&& int` is accepted,
  `int64 + int` is not); the grid found this;
- anything else — another type's own operator members (`DateTime - DateTime`
  is a `TimeSpan`), an enum, a unit of measure, `bool`: `Builtin(Undecided)`.

Any other use — the operator as a value (`(+) a b`, `List.map int`,
`x |> int`) — declines as a constrained signature, as before. One more FCS
behaviour the generators met: in a check position whose expected type is
known, FCS unifies the result's type variable with it *before* it solves the
constraint, so `let (x: obj) = s + s` is rejected where a `string`-returning
function would be coerced. Inference emits nothing in check positions, so it
commits nothing there; the local-let generator keeps the operators out of
coercion positions, where its well-typed family would stop being well-typed.

`infer_builtin_operators_diff` is the grid: 28 uses (each operator infix,
prefix `-`, each conversion by name and qualified, and value spellings) × 25
operand sorts (every primitive, `bool`, `unit`, `obj`, `DateTime`,
`TimeSpan`, `BigInteger`, an enum, a function, a tuple, `float<m>`) ×
annotated, nested, literal, local, member-receiver, open, late-fixed and
unmodelled operands, each with a twin over two different operand types. The
grid's own table of what the rule solves is checked against FCS exactly over
the primitives (FCS accepts a file over them exactly when the table says the
rule solves it, and rejects every pair it calls violated); each solved file
must leave every binding complete but for the deliberately open rows and
commit every synth row (2 682 obligations); every decline over a modelled
spelling must record the reason the table predicts (415 rejected files, 33
accepted off the list). The argument-check generator draws `+`, `<<<`, `-`,
`*` and `int`, and the local-let generator `+` and `int` at `int` and
`string`. Deleting each gate fails a test: the whole rule, the
operand-type check per operator (admitting any type to `*`, any operand to a
conversion, an unsigned one to `-`, a float to `&&&`, any right operand to
`<<<`), the one-type requirement, the bitwise widening exception, the settle
before reading the operands, the conversion's typed result over an open
operand, asking before the member-call route (which took `-a`), the
`Operators` module check (`Operators.Checked` shares the names and shapes),
and the decimal exclusion from `Violated`. The assembly-identity, shape and
constraint-kind checks in the recogniser are not reached by any FSharp.Core
there is; they keep another FSharp.Core's differently shaped operator from
being read as one.

Walking the operands reached member accesses nothing walked before
(`name.Substring (0, name.Length - suffix.Length)`), and the whole-project
gate then called three of them divergent: FCS names `System.String` reached
through `string` (or `System.String` in an annotation) by the `netstandard`
facade FSharp.Core compiles against, and reached through `open System` by
`System.Runtime` — one entity, two names. The gate now accepts the oracle's
assembly when the env's type forwarders send our answer's declaring type's
path there (`AssemblyEnv::assembly_names_type`), and the full name must still
agree; the three are agreements.

Measured on the stride-13 corpus sample: agreements 978 + 945 → 994 + 953
(24 added in 5 files, none lost), 0 divergences, complete bindings
1 010 → 1 011. The prototype before it — the rule with no ground check, all
three type parameters equated, no prefix `-` — measured +16, and admitting
FSharp.Core's generic abbreviations in pattern annotations (`int list`,
`int array`), the next candidate the census suggested, +3.

**Parameter types.** A function parameter's own type is published where it is
known for certain, and never from the parameter's binder variable (which on an
incomplete binding is decoupled from the function type's slot, and can be
grounded by a use the binding's unmodelled constraints contradict):

- its **annotation**, exact in F# — subsumption applies at a call, never at the
  binder — and kept on the binder even where FCS rejects the body;
- otherwise its **slot** in a function type `Gen::finish` publishes, when the
  slot is ground: the published function type's domains are the parameters'
  types, a tuple parameter's elements its elements', so this claims nothing
  the function type has not.

A generic parameter stays silent: FCS's binder rendering renumbers a
parameter's type on its own (`'a` for `y` in `let k x y = x`, whose type is
`'a -> 'b -> 'a`), while hover names it as the function does (`'b`), and one
published `Ty` cannot be both. `infer_parameter_types_diff` pins which
parameters must commit, over annotated, argument-checked, condition-grounded,
tuple, member-receiver and operator-operand parameters and their generic and
open twins; every other grid grades the new commits against `binder-types` as
it grades every binder. Deleting the annotation route or the published-owner
check fails a test.

Measured on the stride-13 corpus sample: agreements 994 + 953 → 994 + 1 068
(115 parameters, none lost), 0 divergences, 3 more commits ungraded in
unconverted declarations; completeness is unchanged, since nothing about a
binding's walk moved.

**Preceding files' values.** Inference folds over Compile order beside the
resolver (`infer_project`): each file publishes, for every top-level value it
exports, the type its own inference committed for the binder — ground, or a
generalised scheme — keyed by the resolver's `ItemId`, and a later file's use
the resolver resolved to that item (`Resolution::Item` of an earlier file) is
typed from it, instantiated and loosened exactly as an in-file scheme's use is
(`Gen::nonlocal_value_use`, as a bare name, a module-qualified path, an infix
operator, or a member access's receiver). Choosing the binder stays with the
resolver. FCS fixes an earlier file's value when it checks that file (and
applies its defaults at the file's end), so nothing in a later file changes
it, and what a file publishes is exactly what its own binder hover claims,
graded where it is defined.

Everything else declines with a `PrecedingGap`, which keeps "did not look"
(`NotInferred`, `NotWalked`) apart from "looked, and nothing is certain"
(`Untyped`, `Signature`, `Inline`, `RejectedUse`, `NotAValue`); an item absent
from the table is `NotInferred`, never a licence. A file with a signature
declines every value: what a later file sees is the signature's type, and a
signature-only attribute (an error-level `[<Obsolete>]` on a `val`) makes a
use `obj` on code FCS accepts; reading the `.fsi`'s own annotation would need
the signature's types resolved in its scope, which nothing does yet. A value
whose attributes, an enclosing module's, or **the file's own top-level module
header's** may reject a use declines (the header case is one no in-file use
meets). `inline` values decline too, by request: no input has shown one typed
wrong, so that gate is defence in depth. The LSP caches the fold per project
and reuses each leading file whose resolved file and env are the very ones it
was inferred from (`infer_project_reusing`).

`infer_cross_file_diff` is the differential, against a new `types-project`
request of `fcs-dump file-batch` (every file of a project checked as one, both
typed-tree oracles per file). Earlier files declare values generic,
monomorphic, loosened, constrained, inline, mutable, incomplete, recursive,
attributed, operators, in a nested, a `rec`, a rejected and a namespaced
module, cases and an exception constructor; later files use each in every
modelled form (value, application, partial application, pipes, composition,
paren callee, qualified path, member access, method call, operator, a
parameter argued through it) × five positions — 1 453 well-typed cells — and
every commit in every file is graded certain-implies-exact. Each value
declared again in one file under a name of the same length is the
**relocation twin**: whatever the in-file walk commits for a twin cell, the
cross-file cell must commit identically (1 656 obligations; 712 cells), and
each declined kind must record its reason. The ill-typed family (718 cells,
1 032 FCS errors) applies each value at a contradictory type and relates one
parameter through two earlier functions of different domains in both orders.
Signature screening, a syntax error before a value, a rejecting header and
shadowing by a later file or a second opened module have their own cases, and
a reusing fold must infer what a cold one does. Deleting the fold, the
rejected-use gate (a wrong `int` for an `[<Obsolete>]` module's value), the
inline gate, the walked/unwalked split, the loosening at a use (a wrong type
for `p |> A.a_fo`), the qualified path or the cross-file receiver fails a
test; four guards survive deletion because another layer covers them: the
screened-implementation arm (the resolver exports only signature-derived
values for a paired file), each of the two not-a-value arms (deleting both
fails), and the three own-file-item checks (the current file is never in the
table it is inferred against).

Measured on the whole-project gate (six projects): 0 divergences; hovers on an
earlier file's value agree with FCS's type 26 times (0 before), and withhold a
type the definition shows 9 times (35 before: an `inline` `trace`, and two
functions with return annotations, whose definition hover comes from the
annotation renderer rather than from inference); 176 such hovers stay withheld
because the earlier binding has no certain type. Completeness does not move
there (26 of 485 walked bindings, folded or each file alone, before the
census counted the bindings an unreadable annotation stops); the per-file
reason `PrecedingValue(NotInferred)`, present in 92 bindings when each file is
inferred alone, is what main recorded as `NotInFileValue`, and the fold
replaces it with the earlier binding's own reason: `Untyped` present in 39,
`Signature` in 31, `Inline` in 23, `NotAValue` (a case constructor) in 8,
`RejectedUse` in 1. The four bindings whose only reason it was stay
incomplete — two case constructors, one untyped value, one signature-screened
value — so on this corpus the next levers are the earlier bindings' own
blockers and the `.fsi`'s annotations, not the threading. The
stride-13 census below checks each file alone, so it cannot see this stage
and is unchanged.

### Project-type annotations

An annotation naming a type the project declares had no `Ty` to bridge to. It
now has `Ty::Project { path, args }`: the path FCS gives the type — the
namespace, each enclosing module's compiled name, the type's own — and its
arguments. A path and an arity name one type project-wide (two at one path and
arity are FS0249 in a single assembly, private ones included), so the same key
will serve a type from an earlier file once the resolver resolves one, and a
signature and its implementation agree on it. It never unifies with a
`Ty::Named` at the same path.

The bridge (`Gen::project_type_paths`) commits only where the written path is
provably FCS's, by its rendering probed directly: a type's `[<CompiledName>]`
renames it; a module takes a `Module` suffix beside a same-named type or under
`CompilationRepresentation(ModuleSuffix)` but ignores `[<CompiledName>]`; a file
without a header names its implicit module after the file. Abbreviations,
augmentations, inline IL and bodyless types decline. Members are not modelled:
an access on a project-typed receiver is left open and counted under
`ProjectTypeMember` (only reason of 4, present in 14, on the whole-project
gate). A project type is read as not sealed, admitting subsumption, and with
equality and comparison undecided — each the reading that only declines.

Two resolver-side pieces landed with it. A type-position name that ends on an
assembly abbreviation whose decoded target is structural (`'T array = 'T[]`, a
tuple, a function) binds the abbreviation, as FCS does, where the walk declined
every unchaseable alias; a path that walks past one still declines. And an
applied abbreviation reads through to its target only when it is FSharp.Core's
own, by assembly name and public-key token, with a one-step target applied to
exactly its parameters in order.

Graded by `infer_project_type_annotation_diff` (18 kinds by 12 positions, six
annotation sites each, an optional extension elsewhere, and an ill-typed
family), `infer_fsharp_core_abbreviation_diff` (ten heads by six arguments by
four contexts, and a parameter-permuting abbreviation from another assembly)
and `resolve_terminal_abbreviation_diff` (five contesting contexts by seven
uses, by `(assembly, full name)`). Still declining: a qualified in-file type
(`M.T`), which the resolver records only at a single segment, and every earlier
file's type, which the next stage resolves where it sits in the enclosing
namespace (*Earlier files' types*).

### Earlier files' types

FCS's name environment is last-write-wins over ordered additions: the
referenced assemblies with their auto-opens (FSharp.Core's `Microsoft.FSharp.*`
among them), then the earlier files (a top-level `[<AutoOpen>]` module opens
there), then the enclosing namespace's fragments with their `[<AutoOpen>]`
modules, then the file's own `open`s and declarations. The resolver's ladder
puts the implicit opens above the enclosing namespace, so it committed
FSharp.Core's `string` where an earlier file declares `Lib.string` and a later
file of `namespace Lib` names it, and likewise past an earlier top-level
`[<AutoOpen>]` module's type: wrong targets, both FCS-probed.

Each file now records every type it declares with its full path, arity, def
and access root, in declaration order (`ResolvedFile::placed_types`), and
counts each declaration it cannot place — a headerless file's, an exception,
a signature's — as unplaced; the fold threads them to later files
(`ProjectItems::earlier_types`), withholding a signatured file's placements. A
later file names such a type by the declaring file's index and the type's
position in that order, as it names an export by its `ItemId`, so an edit that
renumbers the declaring file's defs leaves a reused later file naming the same
type; the incremental fold compares each placed type's path, arity and access
root, and both folds build a signature's slot through one function. For a single-segment name that an earlier file
declares a type of and the use can access (FCS skips one it cannot, probed in
both directions for `private` types and modules), `Resolver::earlier_file_type`
asks the explicit `open`s first: a reading there is a later write, and the walk
commits it as before. Otherwise nothing below the explicit opens commits, and
the earlier file's type does only when it is the project's only declaration of
the name, placed directly in the enclosing namespace at the written arity, the
file declares nothing of the name itself, and the assemblies provably declare
nothing at that namespace. The use resolves to `Resolution::EarlierFileType {
file, def }`; go-to-definition, hover, documentation and find-references treat
it as the type's declaration, and references key a type's in-file `Local` and
later files' uses as one symbol. Inference reads the `Ty::Project` the declaring
file published for the type (`PrecedingTypes::earlier_type`), under the same
checks as an in-file type and never for a signatured file.

Graded by `infer_preceding_type_diff`: 21 kinds by 17 positions (the enclosing
namespace through a `namespace` and a `module N.M` header, an assembly-shared
namespace, `open`s of a namespace and a module, qualified paths, `[<AutoOpen>]`
modules public and private, top-level and nested, nested modules, a headerless
file, an unrelated namespace, a signatured file), and three kinds by those
positions by eight shadows (none, a later project `open`, an assembly `open`,
an in-file type, a same-named module, an `[<AutoOpen>]` redeclaration, an
`[<AutoOpen>]` exception, another arity) by four names (`T`, and FSharp.Core's
or the BCL's `Result`, `string`, `Random`), each program checked by FCS as a
project and graded in the resolver by declaration site and in inference by
type; plus an ill-typed family and an assembly `[<AutoOpen>]` module, reached
by an explicit `open` and sharing the enclosing namespace. Still declining: a
qualified path to a project type (`Lib.T`, `M.T`), an `open` of a project
module, a type in an `[<AutoOpen>]` module, and every name the project declares
twice.

### Annotation type variables

A type variable no `<…>` declares is created by FCS at its first mention in a
module-level declaration and is the same variable at every later one: the
unscoped environment starts empty per declaration
(`TcTypeOrMeasureParameter`, rigidity `WarnIfNotRigid`). It is an ordinary
inference variable — the body may fix it (FS0064, a warning, and the binder
then has the fixed type) and what is left generalises. So the head's
annotations, its parameters' and its return annotation, now read each name as
one fresh variable (`Gen::open_head_typars`); a parameter so annotated is an
unannotated one whose binder and slot are tied to that structure, and a return
annotation's check discharges as the equality FCS tries first (an unbound
variable is a no-subsumption domain, as a scheme instantiation's is).

A variable the head declares (`let f<'a> …`) is created rigid: a body that
fixes it or merges two is an error (FS0670), after which FCS may report no
binder of the declaration at all, parameters with ground annotations
included, and every undeclared variable left open is defaulted to `obj`
(`let f<'a> (x: 'a) y` is `'a -> obj -> …`). So a function whose head declares
variables — read or not: `<'a when 'a : equality>` declines its annotations
but has the same hazard, which `main` published through (`let f<'a when 'a :
equality> (x: 'a) (n: int) = x + 1` gave `n : int` where FCS has no binder) —
commits all or nothing: it publishes only when it is complete, its declared variables stay
distinct and unbound as solved and with every relation it holds applied as an
equality, and no undeclared variable is left to generalise; otherwise its
function, parameters and every emission of its walk are withheld
(`Incomplete::RigidTypeVariable` where nothing else stopped it). On the
whole-project gate that costs 11 parameter hovers `main` served, in
well-typed functions (`setParent<'a> (child : 'a Node) … (parentIndex : int)`)
whose bodies the walk does not finish; serving them again needs a proof that
no unmodelled subtree reaches a declared variable.

A condition on a parameter whose annotation names a variable never grounds
it: the parameter is reached through every other mention of the variable too
(`(y: 'a) (x: 'a)`), so none of its own occurrences is provably the first
constraint. Still declining: a head with a constraint (`'a when …`, `<'a when
…>`), a flexible type (`#seq<'a>`), a wildcard (`_`), `'_a`, a `^a`, a measure
or attributed declaration, named-field heads, a body that mentions one of the
head's names (a local's or lambda's annotation, a type application — the same
variable, related by what the walk does not read), and every value binding
(`let x : 'a list = []`, whose generalisation the value restriction decides).
Hover shows the canonical names `'a`, `'b`, …, not the ones written; the
handler differential grades it by alpha-equivalence.

FCS loosens every use of a function-typed value (`MakeApplicableExprWithFlex`):
a parameter of a type that is not sealed becomes a fresh subtype of it, so
`let f (g: obj -> 'a) = g` is `(obj -> 'a) -> 'b -> 'a`. A use whose type is
not known where it is written is *undecided*, and must end up taking no such
parameter; that invariant was checked only of a speculative solution
(`Gen::verified`), which `solve` skips when no argument check discharged —
harmless while nothing but a check could give such a use its structure, but an
annotated type variable gives it eagerly. It is now checked of the solution
`solve` keeps, on every path (`Gen::loosening_holds`): a use that breaks it
poisons its variables and records `Incomplete::LoosenedUse`. On `main` no
annotation is non-ground (`'a`, `_` and `obj -> _` decline at the bridge), so
the skipped check had nothing to catch there. It costs 5 complete bindings on
the whole-project gate (68 to 63) and no hover: uses of functions taking a
project type, which inference reads as unsealed whatever it is.

`fcs-dump`'s canonical rendering now numbers type parameters by identity
rather than name: FCS names a generalised inference variable `'a`, `'b`, …
without avoiding the names annotations wrote, so a name-keyed table rendered
`let f (x: 'a) y = 1` as `'a -> 'a -> int`, collapsing two variables into one
— a reading under which a wrong answer would agree. Its `types` dump also
carries warnings, so a cell records FS0064.

Graded by `infer_annotation_typar_diff`: 25 heads (variables bare, shared,
tupled, under `list`/`option`/`[]`/a project record/a function, beside an
unannotated or ground parameter, one variable spelled two ways (`'``a``` is
`'a`), and the declining shapes) by 9 return
annotations by 48 bodies (well-typed, fixing a variable, merging two through
`pick`, conditions and fixing uses in both orders across two parameters, local,
lambda and typed-expression mentions, and ill-typed ones), one binding per line
so FCS's errors and FS0064 attribute to cells: every commit certain-implies-exact
against both oracles, a per-cell obligation on the modelled subset, no generic
commit on a declining row; the same grid under `let f<'a, 'b>`; declaration
forms (`inline`, a sibling reusing a name, `^a`) and declining declarations;
value bindings; the oracle's rendering pinned on the colliding cells; and
function-typed parameters over eight domains (`obj`, a project class, an
interface, a tuple holding `obj`, a record, `string`, a type variable, a
function) used returned, applied, piped, passed, wrapped and paired, with a
hole (`obj -> _`) that must decline.
`infer_local_let_diff` adds a random ill-typed family with `(p: 'a) (q: 'a)`,
implicit and declared, which found both rigid-variable defects (a dropped
relation and an unmodelled `+` fixing a declared variable while a ground
parameter published) before review. Each guard's deletion fails a test except
the body-mention decline and the head-only reading, each the other's backstop
(deleting both fails), and the probe applying every relation, a backstop to
the incomplete-binding rule.

### Referenced union cases and `match`

**The case's type comes from its maker.** fsc compiles every union case to a
static method on the union: `NewOk(T)`, FSharp.Core's specially named
`Some(T)` and `Cons(T, FSharpList<T>)`, and for a nullary case a getter
(`get_None()`). Each carries `[CompilationMapping(SourceConstructFlags.UnionCase,
n)]`, where `n` is the case's position among all the union's cases in
declaration order — the order the signature pickle lists them in. So the
projector keeps the makers by `n` (`Entity::union_case_makers`), the pickle
overlay records each accessible case's position and what its pickled fields
say (`UnionCase::declaration_index`, `CaseFields`: how many, whether a unit of
measure appears in them, whether the case is attributed), and
`function_ty::union_case_scheme` joins the two and reads the maker's
parameters through the module-function bridge, extended to a union's own
type parameters. IL erases what the pickle keeps, so a measured field
(directly or behind an abbreviation), an attributed case (an error-level
`[<Obsolete>]` rejects the use) and a constrained union decline. One type
bridge serves module functions and cases alike; converting pickled types
would have been a second.

**A case use is not loosened.** `MakeApplicableExprWithFlex` is `TcVal`'s: a
case applied checks each argument against an unsealed field by coercion
(`TcExprsWithFlexes`), which an application's argument check already models,
and a case unapplied is eta-expanded at exactly its field types
(`UObj : obj -> UObj`). FCS's node at an unapplied case's range is the case
built *inside* that expansion — the union, not the function — so nothing is
emitted there.

**`match` in FCS's order.** The scrutinee, then each clause in source order:
its pattern against the scrutinee's type by equality, its guard as a condition
(`bool`, with the first-occurrence rules an `if` condition has), its result.
The arms agree as an `if`'s branches do. Patterns: name, wildcard, constant
(`int`, `string`, `char`, `bool`), tuple, a referenced case with a pattern
over its field, the tuple of its fields, or `_` for all of them. A match's
pattern variables are never generalised, so a use of one is the binding's own
variable, as a parameter's is — not another binding's opaque one, which kept
`match f x with Some v -> v` open until it was — and is typed by its pattern
before its clause is walked, so a use of one whose type is known then is
loosened as FCS loosens it (`Some g -> g` with `g : obj -> int` is `'a -> int`
at the use). A pattern of structure is related to the scrutinee only where
that cannot bind an earlier binding's open variable or one an opaque local's
resolution mentions, the variables an argument check refuses to bind too, and
a pattern binding a name twice declines (FS0038), as for a `let` pattern.
FCS checks in source order, so an argument check still pending at a pattern
was applied before it: a pattern that contradicts one, once every pending
check is applied as an equality under a rolled-back snapshot, declines
(`(fs x, match x with 1 -> …)` keeps `x : string`), and so does one that
contradicts a check-mode relation dropped before it (an arm checked against
the first); an unmodelled construct leaves no relation to probe, so on an
incomplete binding a pattern of structure relates only a ground scrutinee
(`Some x -> (if x then …) | Some "" -> …` makes `x` `bool` in FCS); and a
suspended member lookup whose receiver the pattern could make known declines
it, since FCS looked the member up before the pattern, on a receiver it did
not yet know (FS0072). The probe decides every kind of pending constraint in
one exhaustive `match`, so a new kind of suspension must be decided there,
and declines too when the replayed relations already conflict (FCS resolved
those by an order the replay does not follow). Together with a pattern that
fails to fit declining its clause (unification rolls back whole, where FCS
types the part that fits first: `(x, 1)` against `int * string` keeps
`x : int`), that partitions every earlier relation a pattern could get ahead
of: applied (a contradiction fails the pattern's equality), pending or
dropped (the probe), or unmodelled (the incomplete-binding rule).
FCS's
dumps cannot grade these — they decline to convert a declaration FCS
error-recovered — so their tests pin FCS's answer directly. Three things FCS's dump cannot
grade decline or go unemitted: a one-clause match (FCS elaborates it to a
`let`, keeping no node at its result), the match's own range, and the
scrutinee's own node (FCS may bind it to a temporary whose `let` sits there,
typed as the whole match).

**FSharp.Core's renamed generics in annotations.** The annotation bridge
declined every source-renamed type; it now reads FSharp.Core's own renamed
generics (`Result` is `FSharpResult`) by the signature bridge's rule, so the two
bridges name a type one way.

Graded by `projector_union_case_makers` (FSharp.Core's Option, ValueOption,
Result, Choice and List pinned by position, and every listed case of
FSharp.Core having one maker of its field count), `modifier_metamorphic` (a
maker is held to the `modopt` rule like a member), `infer_union_case_diff`
(FSharp.Core's cases by ten positions, and a fixture library's unions with an
unsealed field, several fields, a measure direct and behind an abbreviation,
an error-obsolete case and a constrained parameter), `infer_match_diff`
(forty-odd matches, ill-typed ones among them, by five positions), and the
generated families, whose atoms now include options and matches over them
(171, 28 and 88 match payloads committed at ten times the default volume, every
one FCS's). Every guard was deleted in turn and a grid failed; two that
survived (a case of several fields, an unsealed field) were found redundant
and removed, and so were a constraint check the bridge already makes and an
emission filter the identifier arm made unnecessary. fcs-dump's binder oracle
now reads a decision tree's target variables, through which FCS binds a nested
pattern's names (`Some (Some v)`): it reported none of them.

**Unreachable clauses decline.** FCS's decision compiler keeps no node or
binder of a clause no value reaches (FS0026: `_ -> 1 | Some x -> x`, or a case
repeated without a guard), and none in a first clause that matches everything
either (`() -> 1 | _ -> 2` keeps no node at `1`), so a match with a clause not
provably reachable declines before any of it is walked. `match_reach` decides
it: Maranget's usefulness check, under-approximated so that "reachable" is a
proof. A guarded clause covers nothing; a constant counts as equal to another
unless both are spelt canonically (`1` and `0x1` may be one value); a column of
two types proves nothing; and the walk's first unread pattern is asked only
whether anything gets past the clauses before it. A property test holds it to
brute-force enumeration over generated types and clause lists (sound always,
exact when every constant is canonical), and generated clause sets over nine
scrutinee types hold it to FCS.

A condition on a parameter fixes the parameter's private slot, which FCS has
as one variable with the parameter, so the pattern probe replays each
slot-binder pair too: `let f x = if x then (match x with 1 -> x | _ -> x)
else false` declines, where FCS rejects the pattern and keeps `bool -> bool`.

Still declining: `function` (lambdas are unmodelled); `try … with`;
active, record, list, array, `:?`, `null` and OR patterns; an annotated name
in a match pattern (FCS relates it by equality and rejects a mismatch, where
the annotation would already have typed the name); a case or constant in a
`let` or parameter pattern (rare, and that relation's guard does not cover
what a case's structure could bind); a later arm checked
against an open first arm's type, as for an `if`; and a value binding of a
generic case (`let v = None`, which FCS generalises).

### The project's own union cases

**Measured first.** With every use of a project case stubbed as modelled (an
unsound upper bound), the whole-project gate goes from 67 to 81 complete
bindings and stride-13 from 1,014 to 1,016. Of the 14 gate bindings, 9 hinge
on a case pattern qualified by its type (WoofWare.PawPrint's own
`[<RequireQualifiedAccess>]` unions: `EvalStackValue.Int32 …`), 4 on an
earlier file's case as a value, and 1 on an earlier file's case in a pattern.

**The resolver already resolves them.** A bare use of a project case, in its
own file or a later one, resolves to the case's export (`Resolution::Item`),
whose def is the case's declaration. A qualified one (`U.A`, `Lib.U.A`)
resolves the same way over the *whole* head, with nothing at the last
segment. So this stage is inference's alone: read the head's span, and give
each case a scheme.

**A case's scheme is its declaration.** For each union whose path
`project_type_paths` holds (so not a constrained or attributed parameter, nor
an attribute that may reject a use), each case's fields are read through the
annotation bridge in the union's own type-parameter scope (`decl_typars`: a
declared `'a` is the scheme's `Ty::Param`), and the union is the
`Ty::Project` applied to its parameters. The scheme is the
`UnionCaseScheme` an assembly case has, so values, patterns and reachability
treat both alike; a project union's identity for reachability is its first
case's item. A file publishes its cases' schemes by item
(`PrecedingTypes::case`), and a signatured file publishes none. Left out, so
that a use declines:

- a case of a `private` representation (`type U = private A | B`) used
  outside the union's enclosing module or namespace, where FCS rejects a
  use the resolver still reads as the case; such a case is never published
  to a later file (an `internal` representation is the assembly's, so it is
  read everywhere);
- every case of a union the parser recovered from (its surviving cases look
  well-formed: `C : | D` reads as two nullary cases), or declaring a type
  parameter twice (FCS binds a use to the first declaration);
- a case with an attribute (an error-level `[<Obsolete>]` rejects its use);
- a case written in the `Case : type` form, or as an operator;
- a case whose field the annotation bridge declines (a measure, `Map<_, _>`);
- a case whose name anything else the project declares so far could mean:
  an export of its own file or an earlier one (a case of another union, a
  value, an exception, an active pattern's case), or a type there that can
  construct (anything but a union, a record or an enum). FCS resolves such a
  name by orders the resolver's latest-wins history does not follow. An
  `open` adds a module's types, and so their cases, before its values
  (`let X = 5` then `type UV = X | W`: the opened `X` is the value; an
  active pattern `(|C|_|)` before `type U = C of int` makes the opened `C`
  the active pattern). A later file's class takes the case's constructor
  name. A qualified name is resolved against everything of the name its
  module or namespace holds: values, then exceptions, then the *first* type
  declaring the case, in an expression
  (`ResolveExprLongIdentInModuleOrNamespace`), `[<RequireQualifiedAccess>]`
  cases and other files' namespace fragments included (with `type U1 = X |
  Y` in one fragment of `N` and an RQA `type U2 = X | Z` in another, FCS's
  `N.X` is `U2`'s). A name declared once means one thing in every order,
  bare or qualified. The resolver's wrong targets for names declared twice
  are a follow-up of their own.

Graded by `infer_project_union_diff`: about 25 union declarations (plain,
generic and postfix-generic, two parameters, recursive, mutually recursive,
named fields, `[<RequireQualifiedAccess>]`, struct, a field of tuple, unit,
function, `obj`, assembly and project types, an obsolete case and union, a
measured field, a constrained parameter, a case named like its type, a case
name declared twice, a value then a case of one name, members, a private
representation, a nested module, the signature form), each in the using
file, in an earlier file reached by an `open`, by module-qualified paths, and
behind a signature, by value and match contexts, FCS-graded as one project,
with must-commit obligations on every cell this stage models and no commit
on a line FCS rejects (its dumps grade no such declaration); plus an
ill-typed family and namespace fragments declaring one case name twice.
`infer_cross_file_diff` now expects an earlier file's case to be typed, and
holds every cell using one to what its in-file twin commits. The
generated clause sets of `infer_match_diff` include the file's own unions,
bare, type-qualified and `[<RequireQualifiedAccess>]`, so reachability is
exercised over project cases (4,800 functions at 20 times the default
volume, every committed one FCS's).

Still declining: a type-qualified case through an `open` of a project module
(`open Lib` then `U.A`, `Q.QA`), and a case pattern qualified by a module
alone (`| Lib.A ->`, `| Inner.IA ->`), which the resolver leaves unresolved; an
annotation under such an `open`, or through a qualified path to a project
type (as before); a match over an unannotated parameter of a generic union,
which commits no binder, FSharp.Core's included.

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
