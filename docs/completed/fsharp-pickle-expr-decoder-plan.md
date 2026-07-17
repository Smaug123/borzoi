# Plan: complete the F# pickle `u_expr` decoder (FSharp.Core attribute args)

> **Status: LANDED.** The real, shipped `FSharp.Core.dll` now unpickles
> end-to-end — `crates/assembly/tests/fsharp_pickle_fsharp_core.rs::unpickles_real_fsharp_core_end_to_end`
> is green and no longer `#[ignore]`d (verified 2026-06-28: the test is an
> always-on `#[test]` with no `#[ignore]` attribute). Implemented in
> `crates/assembly/src/fsharp_pickle/expr.rs` (`read_expr` arms 3/4/5/7/8/11/12/13/14,
> `read_op` covering the operators FSharp.Core reaches incl. `TOp.ILAsm` →
> `read_il_instr`) and `constraints.rs` (`read_trait_sln` — traits in
> expression position carry a real solution). What was *not* needed for
> FSharp.Core stays loud-on-unknown (D6.5): `Expr.Match`/`Expr.Obj`
> (tags 9/10), the payload-bearing `u_op` operators (4/5/17/18/22/25/26/31/32),
> the payload `u_ILInstr` opcodes, and `u_trait_sln` arms 4/5
> (`u_rfref`/`u_anonInfo`). Extend those if a future referenced assembly trips
> one — the §4 iterative loop and the `MalformedPickleLazyFrame` backstop still
> apply.
>
> The original scoping write-up follows for context.
>
> ---
>
> **(Original status: scoped, not started.)** This was the *second* blocker to
> unpickling the real `FSharp.Core.dll`. The first — an `OsgnDoubleLink` on a
> re-declared typar inside `IResumableStateMachine` — was fixed first (the
> idempotent-relink change in `crates/assembly/src/fsharp_pickle/osgn.rs`).
> With that in, the phase-1 walk ran far further and stopped here.

## 1. The blocker

`crates/assembly/tests/fsharp_pickle_fsharp_core.rs::unpickles_real_fsharp_core_end_to_end`
(currently `#[ignore]`d) fails with:

```
UnsupportedPickleExpr { context: "u_attrib_expr orig", tag: 4 }
```

decoding the `ReflectedDefinitionAttribute` entity. Tag 4 is `Expr.Lambda`.
The attribute argument is `App(Lambda(…), …)` — FSharp.Core's
`[<AttributeUsage(AttributeTargets.Method ||| AttributeTargets.Property)>]`
pickles the inline `(|||)` operator application as an applied lambda rather
than a folded constant.

## 2. Why it can't be skipped

FCS's pickler emits attribute arguments as the **full** expression tree:

```
p_attrib       (TypedTreePickle.fs:2875) = p_tup6 tcref kind (p_list p_attrib_expr) …
p_attrib_expr  (:2878)                    = p_tup2 p_expr p_expr   // orig, evaluated
```

`p_attrib_expr` only normalises a literal `Expr.Val` to `Expr.Const`; every
other shape is pickled verbatim via the general `p_expr`. The exprs are
written **inline with no byte-length framing**, so the unpickler cannot skip
past an attribute argument without fully decoding its expression tree. Since
`u_attribs` is read inline as part of every `u_entity_spec_data` /
`u_ValData` / typar / slotparam body, an unmodelled attribute expr aborts the
whole CCU decode.

Today `crates/assembly/src/fsharp_pickle/expr.rs` models only the
attribute-arg *subset* that real fixtures had tripped (`Const`, `Val`, `App`,
`Op{Array,Coerce}`) and hard-errors on the rest (D6.5, loud-on-unknown).
FSharp.Core needs much more of `u_expr`.

## 3. Scope — what `u_expr` needs

The decode is **alignment-only**: nothing downstream consumes attribute-
argument *values* (the measure overlay walks tycon kinds; auto-open reads
ECMA-335 attribute *presence*, not pickled values). So every new arm only has
to consume the exact bytes FCS wrote; the decoded value can be dropped or kept
minimal. But the byte consumption must be exact, so each sub-decoder must
mirror FCS faithfully.

`u_expr` (`TypedTreePickle.fs:3795-3894`) — 15 arms; **have** 0/1/2(partial)/6,
**need** the rest:

| tag | arm | extra sub-decoders needed |
| --- | --- | --- |
| 3 | `Sequential(e,e,int,range)` | — |
| 4 | `Lambda(optVal,optVal,Vals,e,range,ty)` | `u_Val` (have `read_val_data`), `u_Vals` (`u_list u_Val`) |
| 5 | `TyLambda(tyar_specs,e,range,ty)` | `u_tyar_specs` (have) |
| 7 | `LetRec(binds,e,range)` | `u_binds`=`u_list u_bind`, `u_bind` (`:3501`) |
| 8 | `Let(bind,e,range)` | `u_bind` |
| 9 | `Match(range,dtree,targets,range,ty)` | `u_dtree` (`:3469`), `u_target` (`:3497`) |
| 10 | `Obj(ty,optVal,e,methods,intfs,range)` | `u_method` (`:3938`), `u_intf` (`:3946`), `u_methods`, `u_intfs` |
| 11 | `StaticOptimization(constraints,e,e,range)` | `u_static_optimization_constraint` |
| 12 | `TyChoose(tyar_specs,e,range)` | — |
| 13 | `Quote(e,range,ty)` | — |
| 14 | `WitnessArg(trait,range)` | `u_trait` (have) |

`u_op` (`:3630`) — **~36 `TOp` arms**, currently only tags 15/19 handled inside
`expr.rs`'s tag-2 arm. Several carry payloads (`u_ILMethodRef`, `u_tcref`,
`u_ucref`, `u_rfref`, `u_bytes`, `u_array u_uint16`, sub-`u_op` data). This is
the single largest piece. Factor it into its own `u_op` decoder rather than
nesting in `read_expr`'s tag-2 arm.

Heaviest new sub-decoders: `u_dtree` (decision trees — `TestForBitsSet`,
switch tables), `u_target`, `u_method`/`u_intf` (object-expression members,
each carrying typars + a slotsig + body), `u_bind` (a `u_Val` + body expr).

## 4. Approach

- New module `expr.rs` grows (or split `u_op` into `op.rs`). Keep the existing
  typed `PickledExpr` subset for the shapes a consumer might one day inspect;
  for the new alignment-only arms either extend `PickledExpr` with the needed
  variants or add a single `PickledExpr::Other`-style catch that still records
  the tag, so the value stays inspectable for debugging without exploding the
  model. Decide per-arm; lean minimal.
- **Iterative gap discovery, FSharp.Core as oracle.** Un-`#[ignore]` the
  `unpickles_real_fsharp_core_end_to_end` test and run it; each failure names
  the next missing tag (the `context` + tag in `UnsupportedPickleExpr` /
  `UnsupportedPickleTag`). Implement, re-run. Expect several rounds — the expr
  tree pulls in `u_op`, which pulls in `u_dtree`/`u_method`, each of which may
  surface its own first-seen tag. Keep the loud-on-unknown envelope so every
  new shape pinpoints itself.
- **Guardrail:** the `u_lazy` length check on `u_modul_typ`
  (`MalformedPickleLazyFrame`) already catches any drift the new decoders
  introduce inside a module body — a mis-sized arm fails loudly at the frame
  boundary instead of corrupting downstream.
- When the end-to-end test goes green, also run the corpus sweep
  (`BORZOI_CORPUS`) over a wider set of F# assemblies to flush remaining
  shapes, then un-`#[ignore]` and keep the test always-on.

## 5. Tests first (per the FCS-oracle workflow)

1. Re-enable `unpickles_real_fsharp_core_end_to_end`; it is the integration
   oracle — green only when every reachable shape decodes.
2. Unit-pin each new arm against hand-built wire bytes in `expr.rs`'s test
   module (mirror the existing `Const`/`App` byte tests), so a regression in
   one arm is localised rather than only surfacing as a far-downstream drift.
3. Keep the byte-exactness honest: where FCS drops a field (`u_dummy_range`,
   `vrefFlags`), decode-and-discard but assert EOF in the unit test so an
   off-by-one is caught.

## 6. Why this is its own slice

It is essentially completing the TypedTree expression-tree unpickler — a large
surface (15 expr arms + ~36 `u_op` arms + decision-tree/object-expression
sub-decoders) that is orthogonal to, and much bigger than, the one-line-
semantics relink fix it sits behind. Landing it unblocks the real-FSharp.Core
decode, which is itself the prerequisite for the pickle-driven member-
projection migration in
[`fsharp-core-autoopen-resolution-plan.md`](../fsharp-core-autoopen-resolution-plan.md)
§9.2.
