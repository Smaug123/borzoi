---
name: fsharp-fcs-differential-gotchas
description: >-
  Non-obvious behaviours of the real F# compiler (FCS/fsc) that make it
  legitimately disagree with a from-scratch reimplementation, so a differential
  oracle or metadata consumer must account for them rather than treating the
  divergence as a bug. Read before: tightening any differential
  name-resolution oracle against FCS; relying on the order of
  entities/types/modules read from assembly metadata; or emulating
  "later declaration wins" / `[<AutoOpen>]` semantics.
---

# FCS/fsc differential gotchas

Two empirically-discovered facts about the *real* F# compiler. Both are
properties of FCS/fsc, not of this repo, so they stay true as the code around
them evolves. The concrete file/symbol pointers below are given as **searches**,
not fixed paths — run the search to find the current location before relying on
one.

## 1. Isolation type-checking changes *which* binder a name resolves to

When you type-check an F# file **in isolation** (FCS `ParseAndCheckFileInProject`
/ a `uses-census`-style batch on one file with no siblings), FCS's
`GetAllUsesOfAllSymbolsInFile` does **more** than drop unresolvable member uses
(the long-documented "isolation bias" lower bound). It can also change **which
in-scope binder a lexical name resolves to**, because error recovery over an
unresolved sibling type alters binding.

Concrete case from the F# compiler corpus:

```fsharp
match p with | SynPat.Paren(p, _) -> traversePat path p
```

- In a **full project**, the inner `p` from `SynPat.Paren(p, _)` shadows the
  parameter `p`, so the body `p` resolves to the inner binder.
- Checked **alone**, `SynPat.Paren` is an unresolved sibling union case, so FCS
  error-recovers and does **not** bind the inner `p`; the body `p` falls back to
  the enclosing parameter.

A purely-lexical resolver still binds the inner `p`. So FCS and a *correct*
resolver legitimately disagree on the declaration range, **with no bug on either
side**.

**Implication for differential resolution oracles.** Exact declaration-range
matching is too strict over a wild corpus. Split the outcomes:

- Gate hard only on **unambiguous faults**: `Unresolved`, resolving to an
  assembly entity where FCS found an in-file binder, or a **wrong-*named***
  binder.
- Treat **same-named binder at a different range** as a separate, loosely-capped
  class. (This also absorbs OR-pattern canonicalisation, where FCS attributes a
  use to the *first* alternative's binder.)
- Keep strict shadowing correctness covered **FCS-free** in the curated scoping
  tests, and exactly in the curated single-file resolution diff.

To find the current oracle and its classification:

```sh
grep -rln "Unresolved\|declaration.range\|same-named" crates/sema/tests --include='*.rs'
```

Look for the corpus/divergence resolution diff (a `resolve_*divergence*` or
`resolve_*project*diff*` test) alongside the curated scoping/resolution tests.

## 2. fsc metadata TypeDef row order is NOT source declaration order

For an F#-authored assembly, the ECMA metadata row order of nested TypeDefs does
**not** match source declaration order: nested *types* appear first in
declaration order, then nested *modules* in **reverse** declaration order.
Top-level rows are suspect too.

**Why it matters.** FCS's later-wins rules — most visibly the recursive
`[<AutoOpen>]` fold, where a later sibling's member shadows an earlier sibling's
descendant's — are declaration-ordered. FCS reads the **pickle** (whose
`module_type.entities` lists preserve source order), never metadata row order.

**How to apply.** Any consumer of `Entity` ordering (auto-open application order,
"later declaration wins" emulation) must go through the declaration-order overlay
rather than assuming `children()` is source order. The overlay reorders roots +
nested children into pickle order along the authoritative path; unpickled
compiler-generated entities keep the tail. Never assume `children()` order is
source order for shapes the overlay can't cover (non-authoritative images; C#
assemblies, where source order is moot anyway).

Note the differential normaliser **sorts** entity lists, so reordering is
diff-safe — a diff test will not catch an ordering regression here; a targeted
order-sensitive test must.

To find the overlay:

```sh
grep -rln "apply_declaration_order" crates/assembly/src --include='*.rs'
```

It lives in the F# pickle-merge module; the ECMA assembly projector calls it.
