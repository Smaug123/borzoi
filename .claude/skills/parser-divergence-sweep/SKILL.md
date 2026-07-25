---
name: parser-divergence-sweep
description: How to regenerate the categorised parser-vs-FCS divergence report (`fcs_divergence.rs` in borzoi-cst). It sweeps the F# corpus, parses each `.fs`/`.fsi` with both our parser and FCS, and writes per-bucket worklists (both-reject / we-reject-FCS-accepts / AST-divergence / …) under `fcs-divergence/`. Use whenever you want the full triage lists behind the `parser_corpus_diff` gate — "which files do we reject that FCS accepts, and with what error?" — rather than the gate's sample output.
---

# Parser divergence sweep (worklist generator vs FCS)

`crates/cst/tests/all/fcs_divergence.rs` (`regenerate_fcs_divergence`) is the
parser triage generator. Where the gate `parser_corpus_diff.rs` only *asserts* a
floor of AST matches / ceiling of divergences and prints a sample of paths, this
sweeps the same corpus, classifies every `.fs`/`.fsi` into exactly one bucket,
and writes the **full categorised worklists** — so you can triage rather than
guess.

For each file it compares two signals: whether each side's *parse* succeeded
(FCS `ParseHadErrors == false`; ours `errors.is_empty()`), and — when both are
clean — whether the two **normalised ASTs** are equal. It parses with the
`COMPILED` / `EDITING` `#if` symbols defined, the same implicit set FCS's service
parser uses, so conditional branches agree instead of diverging on symbol-set
mismatch.

It is a **report, not a gate** — the gate is `parser_corpus_diff.rs`; this is
the categorised lists *behind* its numbers.

## Running

`#[ignore]`d (it parses the corpus twice and writes files). Run under
`nix develop`, which sets `BORZOI_CORPUS` to the pinned F# checkout:

```sh
BORZOI_DIVERGENCE_OUT=target/fcs-divergence \
  nix develop -c cargo test -p borzoi-cst --test all \
  fcs_divergence:: -- --ignored --nocapture
```

Note the `--test all <group>::` filter form (one test binary per crate — see
AGENTS.md); `--test fcs_divergence` does **not** resolve. FCS is driven through
the shared request/response `ast-batch` wrapper, so each file has a bounded
timeout and the oracle child is respawned on a wedge/crash. The first run builds
`fcs-dump` (`dotnet build -c Release`), so budget several minutes.

### Knobs

- `BORZOI_DIVERGENCE_OUT` — output directory. Defaults to `fcs-divergence/` at
  the **workspace root** (gitignored). Point it at `target/…` (as above) to keep
  it out of the tree entirely.

This sweep is unstrided — it categorises **every** `.fs`/`.fsi` under
`BORZOI_CORPUS` (`.fsx` is excluded: our parser has no script mode, so an FCS
script parse would not be like-for-like).

## The report — what each file means

Written under the output dir.

- **`summary.json`** — the versioned machine-readable measurement configuration
  and every bucket count, including matches. The main-only continuous-
  measurements workflow wraps this in commit/corpus provenance and records it
  on `stats-data`; scripts should consume this file rather than parsing stderr.
- **`we_reject_fcs_accepts.txt` — the primary worklist.** We error, FCS is
  clean. Each line is `<our first error message>\t<path>`, **sorted by message
  then path**, so the dominant gaps group together — pick the biggest cluster
  and work it.
- **`both_reject.txt`** — both parsers report errors. One path per line.
- **`we_accept_fcs_rejects.txt`** — we are clean, FCS errors (typically negative
  `E_*` test fixtures we don't yet reject). One path per line.
- **`ast_divergence.txt`** — both clean and both normalise, but the normalised
  ASTs differ: a real parser bug or a normaliser asymmetry. One path per line.
- **`uncompared_fcs_unmodeled.txt`** — both clean, but normalising one side
  panicked (a construct the shared normaliser doesn't model yet, or an AST too
  deep for `serde_json`). Can't be compared. One path per line.
- **`our_parser_panics.txt`** — our parser itself panicked. One path per line.
- **`uncompared_other.txt`** — couldn't get that far: `(json parse failure)`,
  `<path>\t(unreadable)`, or `<path>\t(fcs error: …)`.

Files that match (both clean, both normalise, equal ASTs) are the headline
success and are only **counted** (in `summary.json` and the closing stderr
summary), never listed.

## When to reach for this vs. the gate

- Editing the parser or lex-filter and want to see the whole worklist of what
  FCS accepts that we don't → **this sweep** (`we_reject_fcs_accepts.txt`).
- Just checking the ratchet still holds before pushing → the gate
  `parser_corpus_diff::` (see AGENTS.md), not this.

## Related

- `resolve-divergence-sweep` — the same idea one layer up: `resolve_divergence.rs`
  categorises name-**resolution** (not parser) divergences vs FCS over the same
  corpus.
- [[resolve-real-project-diff]] — the whole-project resolution differential.
