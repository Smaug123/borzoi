# OV-6.1 — curried-member detection (undo the multi-parameter defer)

> **Status: landed** (PR #899). Follow-up to OV-6 removing the "possibly curried"
> over-approximation: restores commits for multi-parameter C# method calls
> (`s.Insert(0, "z")`, `s.Substring(1, 2)`, tupled overloads) and closes the
> curried-*loser* soundness hole (a base 1-param `M` winning beside a derived
> curried `M x y` losing would make FCS report FS0816). Part of
> [`overload-resolution-plan.md`](../overload-resolution-plan.md) (§5).

## What landed

Currying is F#-only: a method compiled by C#/VB is always a single argument
group, and the projector already knows per-assembly whether the host is F# (the
`FSharpInterfaceDataVersionAttribute` marker). So curry-ness is decidable at
**assembly granularity** — a non-F# assembly's methods are provably single-group;
an F# assembly's methods are unknown from the flattened MethodDef alone (a curried
`M a b` and a tupled `M(a, b)` are indistinguishable).

- **Model** — `MethodLike` gains `arg_group_count: Option<usize>`
  (`crates/assembly/src/model.rs`): `Some(1)` = provably single-group,
  `Some(n≥2)` = curried, `None` = unknown.
- **Projector** — the IL projector stamps every method `Some(1)` (the C#/VB fact).
  `blank_arg_group_counts` in `crates/assembly/src/ecma335_assembly.rs` resets
  every method to `None` when the assembly carries the F# interface-data marker
  (keying on the marker, not a decodable pickle, so a marker-only F# assembly with
  external/stripped `.sigdata` still blanks).
- **Engine** — the curried-member gate in `crates/sema/src/infer.rs`
  (`Gen::wake_member`): if any candidate in the group is *possibly curried*
  (`parameters.len() >= 2 && arg_group_count != Some(1)`), the whole call defers.
  C# multi-parameter groups commit again; curried F# winners and losers defer via
  a direct check (sound even in a FSharp.Core-free env).

Tests: `fsharp_core_methods_have_unknown_arg_group_count` and the marker probe in
`crates/assembly/tests/all/projector_{fsharp_core,markers}.rs`; the parametrised
`arg_group_count` cases in `crates/sema/tests/all/infer_{member_access,static_call}_diff.rs`.

## Deferred (out of OV-6.1 scope)

- **Per-val F# curry precision** — matching each `PickledValReprInfo.arg_repr.len()`
  to the IL method to let a *tupled* F# multi-parameter member commit. No
  observable benefit while the FSharp.Core assembly-level auto-open deferral stands
  (it defers every method call in any env containing an F# assembly). Subsequently
  progressed via the F# pickle merge work (`crates/assembly/src/fsharp_pickle_merge.rs`);
  not part of this slice.
- **Direct marker-only-F# test** — the shipped F# DLLs embed their signature
  resources, so no fixture exercises the marker-present/pickle-absent cell; blanking
  is sound by construction there. Needs a stripped-resource F# DLL (or a C# assembly
  stamped with the marker) if that build plumbing lands.
