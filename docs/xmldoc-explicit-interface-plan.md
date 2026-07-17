# Explicit-interface doc-ID fidelity: structural modelling plan

> **Status (2026-07-15).** Stage 1 — the structured explicit-interface model
> read from the ECMA-335 `MethodImpl` table — is **landed and hardened**.
> Stage 3 (generator emits `@` for type-parameter interface args) is
> **rejected** (it would diverge from fresh Roslyn). **Stages 2 and 4 are
> deferred until the hover XML-doc lookup layer exists** — which it still does
> not: `doc_id` only *generates* keys, and no hover consumer reads XML doc
> summaries (see `crates/lsp/src/handlers/hover.rs`; the hover slice is tracked
> in [`hover-signature-plan.md`](hover-signature-plan.md)). Detail below the
> landed list is only on the deferred Stages 2 and 4.

## Landed

- **Base doc-ID generator** (PR #586, [`crates/assembly/src/doc_id.rs`](../crates/assembly/src/doc_id.rs)).
  Escapes explicit-interface member names by string transform (`.`→`#`,
  `<>`→`{}`) and stays **fresh-Roslyn-faithful**, keeping `,` between concrete
  interface type-arguments. Pinned by `doc_id_diff` (and
  `doc_id_fsharp_core_diff`).
- **Stage 1 — structured model** (PR #619, then hardened over several review
  rounds). `MethodLike` / `Property` / `Event` in `model.rs` carry:
  - `implements: Vec<InterfaceMemberImpl>` — a **proven-only** channel; each
    entry's `member` is an `ImplementedMember { Method, Property, Event,
    Unresolved }` DU, carrying the declaration's kind as `MethodSemantics`
    proved it, so the future doc-ID / lookup consumer can pick `M:` / `P:` /
    `E:` without re-guessing (cross-kind and external-`Unresolved` cases stay
    distinguishable);
  - `unclassified_impls: Vec<UnclassifiedMethodImpl>` — the raw parent
    `TypeRef` (with assembly identity) plus the declaration's raw name, for
    `Reference`-scoped declarations that are neither in the in-module interface
    closure nor a provable ancestor. This is the one residual (an
    in-image-undecidable case — external interface inheritance vs. a covariant
    override redirected to the original declarer); a future multi-assembly
    layer (sema's `AssemblyEnv` holds every referenced assembly) can finish the
    classification by resolving the parent to an `Entity`.

  Populated from the `MethodImpl` table by the projector in
  `ecma335_assembly.rs`; membership is checked against the implementing type's
  in-module **transitive** interface closure (with per-frame generic
  substitution bounded by `MAX_SUBSTITUTED_NODES` in `reader/members.rs`). The
  reader is fail-soft: a row is skipped or left `Unresolved`, never
  misclassified. **doc-ID output is unchanged** — the new data is not yet
  consumed by the generator. Pinned by `methodimpl_classification` (fabricated
  IL shapes + FSharp.Core sweep, including the external / generic-inherited /
  reabstraction / F-bounded / overloaded-accessor edge fixtures),
  `bcl_ref_pack_sweep` (the ref-pack-wide convention differential), and
  `explicit_interface` (mangled-name ⇔ classification agreement).

---

## Still to do — Stages 2 and 4 (deferred to the hover XML-doc lookup layer)

Both test / serve the *lookup* layer rather than the generator, so both wait on
the hover XML-doc lookup slice, which does not yet exist. Stage 1's structured
`implements` / `interface` info is the input available to Stage 4 when a precise
(type-parameter vs concrete) decision is wanted instead of a dual-lookup.

### The problem the lookup layer must solve

For an explicit interface implementation on a generic type, the doc-comment ID's
**member-name portion** encodes the interface's generic arguments in a different
dialect from a normal signature, and the *shipped* BCL reference-pack `.xml`
(which the hover lookup will read) disagrees with freshly-compiled Roslyn
output. One real key from the net10.0 ref pack shows both dialects at once:

```
M:…Dictionary`2.System#Collections#Generic#ICollection{System#Collections#Generic#KeyValuePair{TKey@TValue}}#Add(System.Collections.Generic.KeyValuePair{`0,`1})
```

- **Member-name portion** (the explicit-interface prefix, before `#Add`):
  `.`→`#`, `<`/`>`→`{`/`}`, type parameters rendered **by name** (`TKey`,
  `TValue`), and the generic-argument separator is **`@`**.
- **Parameter-list portion** (inside `(...)`): the ordinary doc-ID dialect — `.`
  kept, `,` separator, type parameters as **indices** (`` `0,`1 ``). This is
  today's `type_enc`, already correct.

Empirical findings from a sweep of the net10.0 ref pack:

1. **Shipped vs fresh disagree.** Shipped ref-pack XML uses `@` for
   type-parameter args in the member-name portion; current Roslyn (our
   `doc_id_diff` fixtures compile fresh) uses `,`. A freshly-compiled
   `Wrapper<A,B> : ILookup<A,B>` keys as `…ILookup{A,B}#Get`. You cannot make a
   single generated string match both oracles for this shape.
2. **Concrete args are unobserved in shipped XML.** The core BCL ref packs
   contain *no* multi-argument member-name portions with concrete
   (`System#`-qualified) arguments — every multi-arg case is between type
   parameters (`TKey@TValue`). So `@` is evidenced **only** for type parameters;
   fresh Roslyn gives `,` for the concrete case.
3. Brace-`@` (interface type-arg separator, e.g. `{TKey@TValue}`) must be
   distinguished from byref-parameter `@` (brace depth 0, e.g.
   `(System.Int32@)`), which the generator already emits correctly.

The member-name portion is stored standalone in `MethodLike.name` (the parameter
list lives in `signature`), so a one-line `,`→`@` in `escape_member_name` is
mechanically possible, but finding (2) makes a blind all-commas→`@` unsafe for
the unobserved concrete case. Stage 1's structured model exists precisely so the
lookup layer can distinguish each argument's kind rather than string-transform
blindly.

### Decision (2026-06-28): the generator is not the fix; Stage 3 is rejected

**The generator stays faithful to fresh Roslyn (keeps `,`); the `,`↔`@`
reconciliation moves to the hover-lookup layer.**

- **Stage 3 (generator emits `@`): rejected.** Emitting `@` for type-parameter
  interface args would make the generator disagree with fresh Roslyn, breaking
  `doc_id_diff` (e.g. `Wrapper<A,B>` fresh-compiles to `{A,B}`). Keep the
  generator single-valued and fresh-faithful.
- **Stage 4 (lookup tolerance) is the chosen fix, deferred.** On a lookup miss,
  retry with the alternate member-name-portion separator. The retry must be
  **brace-depth aware**: only commas *inside* `{…}` (interface type-argument
  separators) flip to `@`; the parameter-list commas (brace depth 0) stay.
  Trying both forms is safe — a member has exactly one shipped key, so at most
  one variant matches.
- **Native-int / type aliases — a second shipped-vs-fresh axis (deferred with
  Stage 4).** For an explicit interface implementation instantiated with `nint`
  / `nuint`, the IL `MethodDef` name (hence *fresh* Roslyn's XML key) carries
  the C# **source alias** in the member-name portion (`…IAdder{nint}#Add`), so
  our name-escaping generator matches fresh Roslyn (`doc_id_diff` stays green).
  The **shipped** ref-pack XML is inconsistent for this shape: most keys use the
  canonical `{System#IntPtr}` / `{System#UIntPtr}` (with `@` separators), but
  some emit a literal `&lt;nint&gt;` (a Roslyn doc-ID bug — the source alias
  with the angle brackets left unconverted). So the lookup layer needs, beyond
  the `@`↔`,` retry, an **alias-normalising** candidate rendered from
  `implements`'s `interface` (metadata signature → canonical `System.IntPtr`);
  and it must accept that a residue of buggy `&lt;nint&gt;` keys is simply
  unmatchable. The `NIntAdder` fixture pins the fresh-Roslyn behaviour our
  generator matches.

### Stage 2 — shipped-XML differential harness (testing infrastructure)

**Dependencies:** the hover lookup layer (tests it, not the generator).

Add a test that locates a real shipped BCL assembly + its sibling `.xml` in the
SDK ref pack (reuse the SDK-locating infra behind the existing differential
tests; `#[ignore]` / env-gate if the pack is absent, like the corpus sweeps),
parses with our reader, and asserts `shipped-keys ⊆ our-ids`. Scope the assertion
to currently-passing kinds (or assert the gap explicitly) so the explicit-
interface `@` keys show as the *known gap* the lookup tolerance must close.

Fixture candidates from the net10.0 `Microsoft.NETCore.App.Ref` pack sweep (107
assemblies with sibling `.xml`; the reader parsed 90, failed loud on 17):
**12 assemblies' XML carries the brace-`@` shape; 5 fully parse** and are usable
fixtures — `System.Collections.Concurrent` (9 brace-`@` keys),
`System.Net.Http` (22), `System.Runtime.Numerics` (4),
`System.Text.RegularExpressions` (1), `System.Threading.Tasks.Dataflow` (12). In
all of them 100% of the brace-`@` keys are currently *not* reproduced by the
generator. `System.Collections.Concurrent` (9 keys) is the compact choice.

### Stage 4 — lookup tolerance for separator version drift (the chosen fix)

**Dependencies:** the hover XML-doc lookup layer.

In the lookup layer, on a miss retry with the alternate member-name separator
(brace-depth-aware `@`↔`,`, per the Decision above), plus the alias-normalising
candidate for the `nint` / `nuint` axis. Decouples correctness from which Roslyn
built the ref pack.

**Correctness oracle:** unit test — lookup succeeds against an XML keyed with `,`
and one keyed with `@` for the same generated member; and the Stage 2 shipped-XML
differential reproduces the type-parameter `@` keys once tolerance is in.

### Remaining lookup-spike questions

- **Concrete multi-arg in shipped XML** — search broader packs (ASP.NET,
  `System.Linq.*`, `System.Text.Json`) for any concrete-arg explicit-interface
  key; confirms whether concrete → `,` holds for shipped, validating the
  per-arg-kind rule.
- **Is `@` stable across shipped .NET versions?** If it flips, Stage 4 becomes
  mandatory rather than optional.
- **Explicit-event fixture coverage** — property coverage is resolved by Stage
  1's structured field; an explicit-event fixture remains a useful test gap if
  the lookup slice needs it.
