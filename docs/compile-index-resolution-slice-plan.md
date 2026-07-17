# Compile-index resolution slice

## Problem

A `textDocument/semanticTokens/full` request for file *k* folds the **whole**
project, but file *k*'s tokens depend only on files `0..=k`: F# is
order-sensitive, so a file references only itself and earlier Compile-order
files. The suffix `k+1..` is pure waste for that request.

Measured on `WoofWare.Myriad.Plugins` (Tempo), editing `ArgParserGenerator.fs`
(Compile index 16 of 22): the `resolve_project` span is ~55–75ms, of which
~34ms is resolving files 17–21 — files the edited file's own tokens can never
depend on. For a file *early* in the Compile order the waste is almost the whole
project.

## The slice

Resolve only up to (and including) the requested file's Compile index. Sound
because `resolve_file(file_k, preceding_k, env)`'s output is a pure function of
`file_k` and `preceding_k`, and `preceding_k` is the fold of `0..k` — independent
of everything after *k*. So:

> `resolve_project(&files[..=k]).file(k) == resolve_project(&files).file(k)`

for every *k*. Truncating the input truncates the output identically; a prefix
fold is a valid prefix of the full fold.

**No sema change.** The incremental fold already accepts a shorter `new_files`
(stage-2's `removed_trailing_file_matches_cold` / `appended_trailing_file_matches_cold`
prove it), and `incremental ≡ batch` holds for any `new_files` — so folding
`&files[..=k]` incrementally against a previous fold yields exactly
`resolve_project(&files[..=k])`. This is an LSP-caching change only.

## LSP: a prefix-aware resolved-project cache

`resolved_projects[key] = (Arc<ResolvedProject>, Arc<AssemblyEnv>)` becomes the
**deepest prefix folded since the last edit** rather than always the whole
project. Add:

```
resolved_prefix_and_env_for(project, up_to_index, ws, docs)
  -> Option<(Arc<ResolvedProject>, Arc<AssemblyEnv>)>
```

which returns a `ResolvedProject` whose `.file(up_to_index)` is valid
(`len() > up_to_index`). Logic:

1. **Fast path — before any `Workspace`/SDK probing** (`dotnet_root_for_project`
   can spawn `dotnet --info` under a long deadline): using the *already-cached*
   `project_parses` to size the request, if the cached prefix already covers it
   (`resolved.len() >= want_len`), return it. A cached entry is always folded
   against the current env (a rebuilt env drops it via
   `invalidate_assembly_state`), so no re-check needed. If the parses aren't
   cached, neither is the fold (built and dropped together), so this falls through.
2. Otherwise (re-)evaluate parses + env, `want_len = min(up_to_index+1, parses.len())`,
   fold `&parses.files[..want_len]` incrementally against `prev_resolved` (the
   stage-2 base, `Arc::ptr_eq` env gate unchanged), and cache.

`resolved_projects[key]` holds the **last served** fold; `prev_resolved[key]` is
the **deepest same-env** fold — a shallow prefix fold does *not* replace a deeper
pre-edit base, or the next full request would re-`resolve_file` the unchanged
suffix. Keeping the deeper base is sound: its entry for the edited file is stale,
but the incremental fold recomputes any file whose tree changed (`same_tree`) and
reuses the rest.

`resolved_project_and_env_for` (the full fold, used by `references` and — for now
— hover/definition/completion) delegates with `up_to_index = usize::MAX`, so
`want_len = parses.len()` and behaviour is unchanged. The cache is shared and
grows to the deepest requester; a full request after a sliced one just extends
the prefix (incrementally, reusing what's there).

Only the **semantic-tokens** handler is switched to the prefix method in this
slice — the hot path, and the one the latency report named. It already computes
`idx` (position in `parses.paths`); compute it *before* resolving and pass it as
`up_to_index`. `token_classifier(idx)`'s cross-file `item_def` is unaffected: any
`Item` file *k* references is declared in `0..k`, inside the prefix.

`references` stays on the full method (it scans every file for uses). Slicing
hover / definition / completion is a mechanical follow-up (compute `idx` first,
call the prefix method) and is intentionally out of scope here.

## Correctness guards

- **Sema (property):** `resolve_project(&files[..=k]).file(k) == resolve_project(&files).file(k)`
  over generated projects, for every *k* — the invariant the slice rests on.
  Extends `resolve_incremental_diff.rs`.
- **LSP:** a sliced token request for file *k* yields the same tokens as the full
  fold; the cached project's length is exactly `k+1` (not the project size),
  proving the suffix wasn't folded; a later deeper request extends the prefix; an
  edit still reuses the unchanged prefix (stage-2 reuse count).

## Out of scope (next slices)

- **`workspace/semanticTokens/refresh`** after an edit that changed cross-file
  state, so an already-open *later* buffer re-requests and updates. This is what
  delivers "see the change arise in the later file" (which does not work today —
  borzoi never sends the refresh); it builds on this slice (each buffer folds up
  to its own index on demand).
- Slicing hover / definition / completion.
