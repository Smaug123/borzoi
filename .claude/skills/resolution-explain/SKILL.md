---
name: resolution-explain
description: How to run the resolution-explain trace — the "why did this name defer?" observability tool. Point `explain_token_at_position` (an env-driven ignored test in `borzoi-corpus-diff`) at one token in a restored F# project and it dumps the token's resolution plus every `open` in the file with how that open perturbs later resolution (its `OpenOpacity`). Use when a name resolves to `Deferred`/"No definition available" and you want the mechanical replacement for hand-tracing which `open` poisoned it — the `open TypeEquality` breaking a bare `List.replicate`, generalised. It reports candidate facts (perturbing opens + caveats), never a per-token culprit verdict.
---

# Resolution-explain trace: why did this name defer?

When a name in an F# file resolves to `Deferred(..)` (surfacing as "No
definition available" on hover / go-to-def), the cause is often an earlier
`open` whose target sema cannot fully model — the motivating case was an
`open TypeEquality` poisoning a bare `List.replicate` in the same block. This
tool replaces hand-tracing that: it dumps the token's resolution and **every
`open` in the file with how it perturbs later resolution**, so you correlate the
perturbing opens against the token yourself.

The tool is deliberately a **fact reporter, not a verdict engine**. It lists
which opens perturb resolution (candidates, with their line ranges) and spells
out the caveats; it does *not* claim which open gated *this* token. See
"What it will and won't tell you" below for why.

## Running (the ad-hoc CLI)

`explain_token_at_position` in
`crates/corpus-diff/tests/project_resolution.rs` is an `#[ignore]`d,
env-driven test — the same shape as the corpus-diff runners. It loads the
project through the exact runtime chain the LSP uses (Compile order + assets
assembly closure + the `resolve_project` fold), resolves the token, and dumps
the explanation to **stderr**.

```sh
BORZOI_EXPLAIN_PROJECT=/abs/or/rel/path/to/Foo/Foo.fsproj \
BORZOI_EXPLAIN_FILE=Bar.fs \
BORZOI_EXPLAIN_LINE=42 \
BORZOI_EXPLAIN_COL=17 \
  nix develop -c cargo test -p borzoi-corpus-diff \
  --test project_resolution explain_token_at_position -- --ignored --nocapture
```

Env vars:

- `BORZOI_EXPLAIN_PROJECT` — the `.fsproj`. May be relative; it is
  canonicalized to an absolute path before loading (borzoi's MSBuild evaluator
  rejects a non-rooted `.fsproj`).
- `BORZOI_EXPLAIN_FILE` — which `.fs`. Matched first as a path **suffix**
  (whole trailing components, so `Bar.fs` does *not* match `MyBar.fs`), then as
  a substring; the match must be **unique** or it panics with the candidates. A
  bare filename usually suffices.
- `BORZOI_EXPLAIN_LINE` / `BORZOI_EXPLAIN_COL` — **1-based** (editor parity;
  the LSP is 0-based internally, converted for you).

With `BORZOI_EXPLAIN_PROJECT` unset the test prints usage and returns green (it
is a CLI wearing a test's clothes, not a gate).

## Prerequisites

Same as the whole-project differential — see [[resolve-real-project-diff]]:

- The project must be **`dotnet restore`d**; the loader reads
  `<project_dir>/obj/project.assets.json` (only that standard location).
- Run under `nix develop`. The first run builds `fcs-dump` (several minutes),
  though this tool itself never queries FCS — the loader path just shares the
  harness. A signature file (`.fsi`) in the Compile set makes the LSP refuse
  the whole project, so the load panics; pick a signature-free project.

## Reading the dump

`TokenExplanation::render()` produces something like:

```
Bar.fs @ 42:17 (byte 981)
token "List.replicate" @ 981..995
  resolution: Deferred(QualifiedAccess)
  opens (source order):
    open System @ 120..129 — (no modeled per-open effect)
    open type TypeEquality @ 130..152 — PERTURBS [opaque_value, unmodelled, staled_earlier]
  note: token is Deferred. 1 open(s) trigger a modeled per-open perturbation
  [TypeEquality @ 130..152]; if the token is a dotted HEAD (e.g. `List` in
  `List.replicate`) lexically after one in the SAME block/enclosing module,
  deleting that open may let it resolve. This per-open view does NOT attribute
  per-token deferrals — correlate manually: a member/qualified TAIL
  (`value.Member`) defers pending inference regardless of any open; an attribute
  (`[<Attr>]`) whose in-file type precedes ANY later open defers to that open;
  and an open's scope is its block, not an offset prefix (the resolver resets
  open-state at block boundaries).
```

- **token / resolution** — the smallest resolution recorded at that byte, and a
  human rendering (a project def site, an assembly full name, or a
  `Deferred(..)` / not-found note). `(no resolution recorded here)` means
  nothing resolved at that byte — check the line/col.
- **opens (source order)** — every `open` in the file, each tagged either
  `PERTURBS [flags…]` or `(no modeled per-open effect)`. The tool **never**
  labels an open `clean`: an all-false open can still take part in a per-token
  deferral it cannot see.
- **note** — printed for any `Deferred` token (even with zero opens). It names
  the perturbing candidates and states the per-token deferral causes the
  per-open view cannot attribute.

### The opacity flags (`OpenOpacity`)

An open is flagged `PERTURBS` if any of these fired. The first three are
walk-state booleans, attributed by **false→true transition within a block**
(monotone — so of two same-effect opens only the *first* records the flag, and
unblocking a name can need more than one deletion):

- `opaque_value` — bare-name lookup skips opened entries (the open could shadow
  a modelled name with an unenumerable value).
- `opaque_dotted` — dotted-path *heads* defer (the open's submodules / nested
  types are unmodelled).
- `unmodelled` — *qualified* paths defer (an `open type`, or a plain `open` of
  an assembly module / class whose nested types we cannot enumerate).
- `staled_earlier` — the open raised the generation barrier, staling earlier
  opened names and local bindings. Bumps on **every** such open (not monotone),
  so a barrier-only open is still flagged — the reason a second `open type`
  reads as perturbing even when it flips no boolean.
- `imported_deferred` — the open imported a name that is *itself* `Deferred`
  (e.g. a cross-assembly duplicate); a use of that name defers with this open as
  its source, though it set no flag and raised no barrier.
- `added_reading` — the open added a namespace **reading**/shortening prefix, a
  new qualified-path precedence entry. Usually *resolves* names, but re-orders
  precedence so a later head can root here over a lower open's. Fires for nearly
  every meaningful namespace/module open — so `PERTURBS` including only
  `added_reading` marks a broad **candidate**, not a culprit.

## What it will and won't tell you

**Will:** the token's resolution, and each open's *per-open* perturbation facts
+ ranges, deterministically from source.

**Won't:** which open gated this token. The trace is **per-open**; several
deferral causes are **per-token** — they depend on the use site, not on any one
open, so a per-open trace cannot attribute them:

- a member/qualified **TAIL** (`value.Member`) defers pending inference
  regardless of any open;
- an **attribute** (`[<Attr>]`) whose in-file type precedes *any* later open
  defers to that open (every open advances the open frontier);
- an open's lexical scope is a **block**, not an offset prefix — the resolver
  resets open-state at top-level block / sibling boundaries, so an earlier open
  by offset may be out of scope entirely.

So use the ranges: a perturbing open is a suspect only if the token is a dotted
**head** lexically after it in the **same block / enclosing module**. Deleting
that `open` and re-running is the confirmation step the tool deliberately leaves
to you.

## Programmatic use

The CLI is a thin wrapper over `borzoi_corpus_diff::explain_token(&loaded,
file_idx, byte) -> TokenExplanation` (a pure query over an already-loaded
project — no refetch, no effects). `TokenExplanation` exposes `resolution`,
`opens: Vec<ExplainedOpen>`, `perturbing_opens()` (the candidate filter), and
`render()`. The underlying per-file datum is
`borzoi_sema::ResolvedFile::resolution_trace() -> &ResolutionTrace` (a
`Vec<OpenTrace>`, each carrying an `OpenOpacity`), always computed and
deterministic — it does not perturb the `incremental ≡ batch` fold differential.

## Related

- [[resolve-real-project-diff]] — the whole-project name-resolution *gate* over
  one restored project (the loader chain this tool reuses). Use it to find
  *which* tokens diverge/defer; use resolution-explain to see *why* one did.
- [[resolve-divergence-sweep]] — the corpus-wide, per-file worklist of what
  resolution still gets wrong or defers.
