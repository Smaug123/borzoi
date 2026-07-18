---
name: resolve-divergence-sweep
description: How to regenerate the categorised name-resolution divergence report (`resolve_divergence.rs` in borzoi-sema) — the resolution analogue of the parser's `fcs_divergence.rs`. It sweeps the F# corpus, resolves each file in isolation, and writes per-bucket worklists (matches / divergences / gaps) under `resolve-divergence/`. Use whenever you want the full triage lists behind the `resolve_corpus_diff` gate — "which in-file names that FCS resolves do we still defer or get wrong, and what are they?" — rather than the gate's sample output.
---

# Name-resolution divergence sweep (worklist generator vs FCS)

`crates/sema/tests/all/resolve_divergence.rs`
(`regenerate_resolution_divergence_report`) is the **resolution analogue of
`cst`'s `fcs_divergence.rs`**. Where the gate `resolve_corpus_diff.rs` only
*asserts* a floor of matches / ceiling of divergences and prints a sample, this
generator sweeps the same corpus sample, classifies every FCS-resolved symbol
*use whose declaration is in the same file*, and writes the **full categorised
worklists** — so you can triage rather than guess.

It is a **measurement, not a gate**: it asserts only that the sweep was
non-vacuous (so a broken oracle or empty corpus fails loudly). The gate is
`resolve_corpus_diff.rs`; this is the report *behind* its numbers.

## Scope — per-file isolation, in-file declarations only

Like `resolve_corpus_diff`, each `.fs` file is resolved **in isolation with an
empty `AssemblyEnv`**, so it can only adjudicate **in-file** declarations. Uses
FCS resolves into a referenced assembly (FSharp.Core / BCL / NuGet) have no
in-file declaration to compare against, so they are tallied under `out-of-file`
and otherwise skipped. Imported-assembly *target identity* is checked by the
**whole-project** differential instead — see [[resolve-real-project-diff]] and
the `corpus-diff` crate, which drive the real assembly closure and FCS's
`uses-project` oracle.

## Running

`#[ignore]`d (it type-checks a corpus sample and writes files). Run under
`nix develop`, which sets `BORZOI_CORPUS` to the pinned F# checkout:

```sh
BORZOI_RESOLVE_DIVERGENCE_OUT=target/resolve-divergence \
  nix develop -c cargo test -p borzoi-sema --test all \
  resolve_divergence:: -- --ignored --nocapture
```

Note the `--test all <group>::` filter form (one test binary per crate — see
AGENTS.md); `--test resolve_divergence` does **not** resolve. If `BORZOI_CORPUS`
is unset the test skips with guidance instead of failing.

### Knobs

- `BORZOI_RESOLVE_DIVERGENCE_OUT` — output directory. Defaults to
  `resolve-divergence/` at the **workspace root** (gitignored). The default is
  the workspace root, not the cwd, because cargo runs the test from
  `crates/sema/`; a bare relative default would litter that crate with untracked
  report files. Point it at `target/…` (as above) to keep it out of the tree
  entirely.
- `BORZOI_RESOLVE_DIFF_STRIDE` (default `13`) / `BORZOI_RESOLVE_DIFF_LIMIT`
  (default unbounded) — the corpus sample. **These are shared verbatim with the
  `resolve_corpus_diff` gate**, so the two see the same files: this report's
  `gap_b1` count is exactly the gate's `tally.gaps`, the denominator of its
  B1-coverage ratchet. To sweep every file, set `BORZOI_RESOLVE_DIFF_STRIDE=1`.

## The report — what each file means

Written under the output dir. Every worklist line is
`<bucket>/<tag>\t<path>:<start>..<end>\t<text>[\t<ours>]`, where `<bucket>` is
the *machinery* the use needs (see below) and `<tag>` is the finer `classify`
sub-tag (`value:module-or-import`, `union-case`, …).

- **`summary.txt`** — start here. Per-bucket counts, the **`gap_b1` sub-tag
  histogram** (the actionable digest — which constructs dominate the missing
  binds), and the coverage ratios.
- **`gap_b1.txt` — the primary worklist.** FCS resolves the use with *no
  inference* (bucket B1) and its declaration is in this file, yet we return
  `Deferred`/nothing. These are pure-lexical names we ought to bind and don't —
  the analogue of `fcs_divergence`'s `we_reject_fcs_accepts.txt`. Sorted by
  sub-tag then path so the dominant missing constructs group together.
- **`divergence.txt`** — the unambiguous soundness faults (D5): FCS found an
  in-file binder, but we gave a *differently-named* binder, an assembly
  `Entity`/`Member`, or `Unresolved`. The gate ceilings the B1 slice of this to
  zero, so real entries here should be B1-free; sorted bucket-first so any B1
  fault sorts to the top.
- **`alt_binder.txt`** — a *same-named* in-file binder at a different range
  (OR-pattern canonicalisation / isolation-bias recovery). Reported, **not a
  fault**.
- **`gap_b2.txt` / `gap_b3.txt` / `gap_other.txt`** — declined uses that need
  shallow inference (a receiver type / B2), the hard pile (overload / extension
  search / B3), or fall outside the taxonomy. **Expected** until inference
  lands; listed for measurement, not as a worklist.
- **infra worklists** — `our_parser_errors.txt`, `our_panics.txt`,
  `fcs_not_ok.txt`, `unreadable.txt`: one path per line, the files we could not
  compare.

Matches are the headline success and are only **counted** (in `summary.txt`),
never listed.

### The bucket taxonomy

- **B1** — pure lexical: FCS resolves it with no type inference at all. The only
  slice the gate adjudicates; `gap_b1` is where the work is.
- **B2** — needs shallow inference (a receiver type, e.g. `x.Length`).
- **B3** — the hard pile: overload resolution, extension-method search.
- **other** — outside the taxonomy.

## Related

- [[resolve-real-project-diff]] — the *whole-project* differential (one restored
  project, real assembly closure, gated to zero). This sweep is the
  corpus-wide, per-file-isolation *worklist* counterpart.
- `parser-divergence-sweep` — the same idea one layer down: `fcs_divergence.rs`
  categorises parser (not resolution) divergences vs FCS over the same corpus.
