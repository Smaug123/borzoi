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

`resolved_project_and_env_for` (the full fold, used by `references`) delegates
with `up_to_index = usize::MAX`, so `want_len = parses.len()` and behaviour is
unchanged. The cache is shared and grows to the deepest requester; a full request
after a sliced one just extends the prefix (incrementally, reusing what's there).

**Every single-file handler** is now on the prefix method: **semantic-tokens**
(the hot path the latency report named), **hover**, **definition**, and
**completion**. Each computes `idx` (position in `parses.paths`) *before*
resolving and passes it as `up_to_index` — the token handler and hover through
`resolved_prefix_and_env_for` (they render `Entity`/`Member` handles, which are
only meaningful against the fold's env), definition and completion through the
env-less `resolved_prefix_for`. The cross-file lookups stay in-bounds by F#
order-sensitivity: any `Item`/binder file *k* references — a `token_classifier`
`item_def`, a go-to-definition `item_def`, a completion receiver's type, a hover
target — is declared in `0..=k`, inside the prefix.

`references` stays on the full method (it scans every file for *uses* of the
symbol, and a use can be in a *later* file — the one direction order-sensitivity
does not bound).

## Correctness guards

- **Sema (property):** `resolve_project(&files[..=k]).file(k) == resolve_project(&files).file(k)`
  over generated projects, for every *k* — the invariant the slice rests on.
  Extends `resolve_incremental_diff.rs`.
- **LSP:** a sliced token request for file *k* yields the same tokens as the full
  fold; the cached project's length is exactly `k+1` (not the project size),
  proving the suffix wasn't folded; a later deeper request extends the prefix; an
  edit still reuses the unchanged prefix (stage-2 reuse count).
- **LSP (hover/definition/completion):** the existing per-handler result tests are
  unchanged (the slice must not move any answer), and one new test per handler
  drives a cross-file request from an *early* file of a multi-file project and
  asserts `cached_resolved_len == idx+1` — the suffix handler was never folded,
  while the cross-file target (an earlier file, in-prefix) still resolves.

## Cross-buffer refresh (`workspace/semanticTokens/refresh`)

Built on this slice: after an edit, an already-open *later* buffer may resolve
differently, but the client only re-requests tokens for the buffer it edited. The
server sends `workspace/semanticTokens/refresh` so the client re-requests every
visible buffer — and each folds only up to its own Compile index (this slice), so
the refresh is cheap.

- **Signal — conservative, invalidation-driven.** `wants_refresh` is set by
  *every* invalidator (a text-sync edit, or a watched structural /
  referenced-assembly change) — **at the invalidation, not at a later fold**, so
  an invalidation with no following fold (a `didClose` restoring disk text, a
  watched-file change, a project that now evaluates partially) still refreshes.
  It is *not* keyed on "the fold reused a previous result": extending a cached
  prefix to a deeper file (a hover after the token request) folds incrementally
  but changed nothing (no invalidation → no refresh). Deciding *precisely* whether
  a later buffer's tokens moved would mean re-deriving the whole downstream
  projection a token classifier follows — an export's name, its `SemanticClass`
  (a later `A.f` recolours function→variable with the path/id unchanged), its
  accessibility, the auto-open/namespace surface — against *the state the client
  last saw* (distinct from the deliberately-stale deeper base the fold retains for
  reuse). A too-narrow signal silently leaves tokens stale, so we refresh on any
  invalidation and let the client diff the returned tokens, re-rendering only what
  changed — an unaffected buffer just pays a re-tokenise (the resolution is reused).
- **No loop.** The refresh's own re-requests are ordinary requests, not
  invalidations, so they set nothing.
- **One refresh in flight (coalescing).** The dispatch loop drains `wants_refresh`
  after every request *and* notification — but the drain sends nothing while a
  previous refresh is still outstanding (`pending_refresh_id`), leaving the flag
  set. The next refresh goes out only when the client *replies* to the first
  (`Message::Response` clears the slot and re-drains). So a typing burst — a
  `didChange` per keystroke, each draining after the notification — collapses to
  one refresh at a time instead of one per keystroke, each of which would fan a
  workspace-wide re-tokenise across every visible buffer. The client-side debounce
  rate-limits further: one refresh per typing pause.
- **Plumbing.** Sent with a fresh JSON-RPC id per send (ids must distinguish
  concurrently-outstanding requests) — retained in `pending_refresh_id` to match
  the reply — iff the client advertised
  `workspace.semanticTokens.refreshSupport`.

## Done in follow-ups

- Slicing hover / definition / completion — each now computes `idx` first and
  folds only the prefix (`resolved_prefix_and_env_for` / `resolved_prefix_for`).
  `references` remains the only single-cursor handler on the full fold, and
  necessarily so (it scans *later* files for uses).
