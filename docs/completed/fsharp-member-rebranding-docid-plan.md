# F# member-rebranding doc-ID fidelity: plan

Status: **complete** for the module-value `P:` slice. The landed model uses the
existing `MethodLike::module_value` marker to choose the XML-doc `P:` prefix for
F# module values; the originally proposed `il_doc_kind` / `DocMemberKind` enum
was never kept as live model state.

## Problem

The documentation-comment ID a member generates is keyed off its `Member`
variant (`doc_id::member_doc_id`): `Member::Method` → `M:`, `Field` → `F:`,
`Property` → `P:`, `Event` → `E:`. For an **F#-projected** assembly the variant
is the FCS *source-level* kind, not the IL kind. Before this slice,
`project_fsharp_members`
(`crates/assembly/src/ecma335_assembly.rs`) rebrands a **module value** — an IL
*property* (`get_x` + property `x`, e.g. `Operators.NaN`, `TaskBuilderModule.task`)
— to `Member::Method` named after the property. The F# compiler's own
`FSharp.Core.xml`, however, keys that member from its **compiled** shape as `P:`.
That made us emit `M:` and miss hover XML lookup by doc ID. The landed fix keeps
the source-level method view but records that it came from a module value, so
`doc_id::member_doc_id` emits `P:` for that case.

## Empirical findings (FSharp.Core.dll + FSharp.Core.xml, .NET SDK 10.0.203)

Our reader **enumerates FSharp.Core.dll cleanly** (254 entities) — there is *no*
fail-loud blocker (unlike the BCL ref assemblies), so a `FSharp.Core.xml`
differential is viable.

In the original SDK 10.0.203 baseline, of 2424 `<member>` keys, our generator
reproduced 1753 (72.3%). The 671 misses break down by prefix:

| prefix | missing | cause |
|---|---|---|
| `P:` | 196 | **this slice** (mostly) — see below |
| `M:` | 318 | separate: generic module methods / F# array-bound encoding (`[0:]`) — **deferred** |
| `T:` | 157 | separate: type keys we don't emit (naming / nested / synthesized) — **deferred** |
| `F:` | 0 | — |

The 196 missing `P:` keys, classified by the declaring entity's kind:

| declaring kind | count | meaning |
|---|---|---|
| Module | **154** | module value (IL property) rebranded to `Member::Method` — **the target of this slice** |
| Class | 19 | class properties FCS surfaces that we drop, or naming — *mostly deferred* |
| Interface | 1 | as above |
| (declaring type not matched) | 22 | overlaps the `T:` gap (type naming) — deferred |

Two findings sharpen the scope:

- **The dominant, well-targeted case is the module-value rebrand (154 / 196).**
  Fixing it closes ~79% of the `P:` gap. Examples:
  `P:Microsoft.FSharp.Core.ExtraTopLevelOperators.query`,
  `P:Microsoft.FSharp.Control.TaskBuilderModule.task`,
  `P:Microsoft.FSharp.Core.LanguagePrimitives.GenericComparer`.
- **The documented "record/exception field-backed property → `Field`" case is
  *not* an actual bug.** `F:` misses = 0: the F# compiler's `FSharp.Core.xml`
  also keys those members as `F:`, so our rebrand already matches. No work is
  needed there (the doc_id module-doc limitation note overstates it).

## Why a model change

Once `project_fsharp_members` rebrands the module value to `Member::Method`, the
IL-property identity is gone, and `member_doc_id` needs a surviving signal to
emit `P:`. The rest of the key is already correct — a rebranded zero-arg module
value yields `…Module.task` with no parens, so only the **prefix** differs
(`M:` → `P:`). The signal now used is `MethodLike::module_value`, which is set at
the same projection site that rebrands the module property into a source-level
method.

## Target model

The landed model is deliberately narrower than the first sketch:

- `MethodLike::module_value` records the F# module value identity at projection
  time.
- `doc_id::member_doc_id` chooses `P:` when `m.module_value.is_some()`, otherwise
  it uses the member variant's natural prefix.
- The speculative `il_doc_kind` / `DocMemberKind` enum was deleted. Its only
  producer was the same module-value site, and the extra `Method` / `Field` /
  `Event` arms had no evidence-backed use.

A future `M:` / `T:` slice that needs a different IL-kind divergence should
derive a fresh signal from that evidence, not resurrect the dead enum.

## Test strategy

The oracle is a **`FSharp.Core.xml` differential**, but it **must be scoped** —
a whole-file `xml ⊆ ours` can't hold while the `M:`/`T:`/dropped-property gaps
remain. Two complementary checks:

1. **Scoped FSharp.Core differential** (the real oracle): locate
   `FSharp.Core.dll` + `FSharp.Core.xml` in the SDK's `FSharp/` dir (reuse the
   SDK-locating infra behind the existing differential tests; `#[ignore]` /
   env-gate when absent, like the corpus sweeps). Enumerate, generate doc IDs,
   and assert **every `P:` key whose declaring type is a module is reproduced**.
   The current test asserts that subset directly; it does not fail on, or report
   current counts for, the unrelated non-module-`P:` / `M:` / `T:` residues.
2. **Unit test** on the prefix logic: a `MethodLike` with
   `module_value = Some(_)` keys `P:`, and an ordinary method keys `M:` — plus a
   small built-F#-fixture case (the repo already builds `MiniLibFs` via `dotnet`)
   with a module value, asserting its `P:` key end-to-end.

Regression guard: the existing C# `doc_id_diff` must stay green (the new field
defaults to `None`, so C# output is unchanged).

---

## Implementation plan

> Implement this plan with each stage on its own branch, stacked as necessary on
> previous branches, so that a reviewer can review each branch in isolation.

### Stage 0 — Spike: pin the exact target set (fold into Stage 1)

**Oracle:** a throwaway (or `#[ignore]`d) enumeration confirming that all 154
module-`P:` misses are getter-rebranded module *values* (not module functions,
literals, or anything needing a different fix), and that the 19 Class / 1
Interface / 22 unmatched `P:` are genuinely out of scope (dropped FCS-surfaced
properties + type-naming). Settles whether `Field`/literal handling is needed
(expected: no).

### Stage 1 — Use `module_value`; emit `P:` for rebranded module values

**Dependencies:** none. **Implements:** target model.

Use the existing `MethodLike::module_value` marker from the module-value rebrand
site; have `member_doc_id` use it for the `P:` prefix. Keep ordinary methods on
the natural `M:` prefix.

**Correctness oracle:**
- Unit: rebranded-module-value `MethodLike` (`module_value = Some(_)`) → `P:…`;
  ordinary method → `M:…`.
- Existing `doc_id` unit tests + C# `doc_id_diff` stay green (field defaults
  `None` ⇒ C# unchanged).

### Stage 2 — Scoped FSharp.Core.xml differential

**Dependencies:** Stage 1. **Implements:** test strategy (1).

Add the SDK-located `FSharp.Core` differential, asserting the module-`P:` subset
is fully reproduced, and reporting the residual known-gap counts.

**Correctness oracle:** the module-`P:` keys are all reproduced; the harness runs
against the real `FSharp.Core.dll`/`.xml`. Residual `M:`/`T:`/non-module-`P:`
gaps remain outside this slice.

---

## Deferred (separate slices, characterized here so they aren't lost)

- **`M:` gap (318).** Generic module methods and F# array-bound encoding — the
  F# compiler writes `[0:]` for some array params where our generator writes
  `[]` (and generic-module-method arity/encoding differences). Needs its own
  characterization + fix.
- **`T:` gap (157).** Type keys in `FSharp.Core.xml` we don't emit (naming of
  nested/compiler-shaped types, or types we don't surface). Overlaps the 22
  unmatched-`P:` declaring types.
- **Dropped FCS-surfaced type properties (Class 19 / Interface 1).** Union /
  record / class properties the F# compiler documents but
  `project_fsharp_members` drops; reproducing their `P:` keys means surfacing
  them (a model/projection decision beyond doc IDs).
