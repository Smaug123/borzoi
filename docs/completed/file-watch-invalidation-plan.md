# File-watch invalidation plan (`workspace/didChangeWatchedFiles`)

> **Status: implemented.** The notification handler, dynamic watcher
> registration, referenced-assembly invalidation, and capability-gated pull
> diagnostic refresh are in place. `State::apply_watched_changes` /
> `classify_change` / `Workspace::invalidate_projects` /
> `SemanticState::invalidate_all` / `State::watched_files_registration` are all
> in place with their oracles. The remaining items under "Risks / out of scope"
> are deliberately deferred optimizations or separate enhancements, not
> unfinished stages. Kept as the design record.

> Closes the "stale until restart" gap flagged across the codebase
> (`semantic.rs`, `workspace.rs`, `workspace_symbol.rs`,
> `fsproj-consumption-plan.md`, `fsproj-project-graph-plan.md`): when a
> project-structure file changes **on disk**, the LSP keeps serving the
> first-evaluated project. Captures decisions before implementation so future
> work can resume from a cold pickup.

## Why, and why now

`Workspace.projects` (the `.fsproj` evaluation cache) is populated on first
lookup and **never invalidated** — it lives for the whole server lifetime. So a
`.fsproj` / `Directory.Build.props|targets` / `global.json` edited on disk is
not seen until restart: stale `DefineConstants` (diagnostics evaluated against
the wrong `#if` branch), stale Compile set, stale ownership, stale semantic /
assembly env.

This bites the agent workflow directly. Agents edit files **on disk**, not
through the editor's text-sync. After the pull-diagnostics work, an agent that
edits a `.fsproj` and then asks `workspace/diagnostic` gets results computed
against the stale project. File-watch invalidation is what makes "edit project
on disk → ask for diagnostics" correct.

Parser- and sema-coverage-independent: this is cache lifecycle, not analysis.

## Background: what exists

- `Workspace.projects: HashMap<PathBuf, Option<EvaluatedProject>>` — the
  `.fsproj` eval cache. **No invalidation method exists.**
- `SemanticState` caches `project_parses`, `resolved_projects`, and
  `assembly_envs`.
  - `invalidate_project(path)` drops `project_parses` + `resolved_projects` —
    **not** `assembly_envs` (its doc: a `.fsproj` / `project.assets.json` edit
    "needs the `didChangeWatchedFiles` follow-up to wire up").
  - `invalidate_file(path)` drops the parses/resolved of every cached project
    that lists `path` — the targeted path used on `.fs` text-sync today.
- `State::invalidate_owning_project(uri)` (on `didOpen`/`didChange`/`didClose`)
  calls `semantic.invalidate_file` for source URIs only.
- `publish_diagnostics(conn, state, uri)` recomputes + publishes one buffer's
  diagnostics through the publish planner.
- The dispatch loop ignores inbound `Message::Response`, and the server sends
  **no outgoing requests** today ("No outgoing requests yet").
- lsp-types has `DidChangeWatchedFiles` (`DidChangeWatchedFilesParams { changes:
  Vec<FileEvent> }`), `RegisterCapability` / `RegistrationParams` /
  `FileSystemWatcher`, and `ClientCapabilities.workspace.did_change_watched_files
  .dynamic_registration`.

## Settled decisions

### W1 — Classify the change, don't guess
A pure `classify_change(uri, typ) -> ChangeClass` discriminated union over the
path **and** the `FileChangeType`:
- **`Structural`** — `*.fsproj`, `Directory.Build.props`, `Directory.Build.targets`,
  `global.json`, `*.props`, `*.targets`, `project.assets.json`; **and** a
  `*.fs`/`*.fsi`/`*.fsx` that was *created* or *deleted* — a source
  create/delete moves a project's glob-expanded `<Compile>` set
  (`Include="*.fs"`), which the cached evaluation would otherwise miss.
- **`Source`** — a *content change* to an existing `*.fs`/`*.fsi`/`*.fsx` (the
  Compile set is unchanged; only the semantic parses that read it are stale).
- **`Ignored`** — everything else.

Pure (path text + change type), property-testable, no IO. Filename matches are
case-insensitive (mirrors `path_extension`).

### W2 — Broad-but-correct invalidation (first cut)
A **`Structural`** change clears the **whole** `Workspace.projects` cache and the
**whole** `SemanticState`. `Directory.Build.*` / `global.json` affect an entire
directory subtree, and the `ProjectReference` dependents of an edited `.fsproj`
are not cheaply enumerable, so a full clear is the obviously-correct choice;
re-evaluation is lazy and watched-file changes are infrequent (a human/agent
editing a project), so the cost is negligible. Targeted (per-project / per-subtree)
invalidation is a **measured optimization**, not part of this slice (gospel:
correctness over availability; leverage compute — measure first).

A **`Source`** change keeps today's targeted `semantic.invalidate_file(path)`.

### W3 — Republish open buffers after a structural change
An open `.fs` buffer's diagnostics depend on its owning project's
`DefineConstants` (via `symbols_for`), so after a `Structural` change re-run
`publish_diagnostics` for **every open document** (the changed `.fsproj`
included, if open). `publish_diagnostics` sends only for push-mode clients;
pull-mode clients advertising `workspace.diagnostic.refreshSupport` receive a
`workspace/diagnostic/refresh` request; other pull clients see the fresh caches
on their next natural document/workspace pull. A `Source`-only change cannot
alter an open buffer's lexer/parser diagnostics (cross-file sema isn't in the
diagnostic path yet) → invalidate semantic, **no** republish.

### W4 — Assembly env must drop too
A `Structural` change clears `SemanticState.assembly_envs` (a `.fsproj` /
`project.assets.json` change can change the reference set), which
`invalidate_project` / `invalidate_file` deliberately don't. Hence a new
`SemanticState::invalidate_all()` clearing all three maps.

### W5 — We don't watch; the client does
No filesystem watching of our own (no `notify` crate, no polling, no FS thread):
dependency rejection, no framework brain. We are a pure classifier plus a thin
reactive shell that responds to the client's notification.

### W6 — Make it fire via dynamic registration
To get clients to actually send the notification, register watchers with
`client/registerCapability` (`workspace/didChangeWatchedFiles`, globs for the
structural + source patterns), **only** when the client advertised
`workspace.didChangeWatchedFiles.dynamicRegistration`. Fire-and-forget — the
dispatch loop already ignores the client's `Message::Response`. Clients without
dynamic registration rely on their own static watching; the handler works either
way. (Tests don't advertise the capability, so the integration harness sees no
outgoing request.)

## Functional-core / imperative-shell

```text
classify_change(uri, typ) -> ChangeClass       (NEW, pure: Structural | Source | Ignored)

handle_notification(DidChangeWatchedFiles):    (shell)
  structural = false
  for change in changes:
    match classify_change(change.uri, change.typ):
      Structural => structural = true
      Source(path) => semantic.invalidate_file(path)
      Ignored => {}
  if structural:
    workspace.invalidate_projects()             (NEW: projects.clear())
    semantic.invalidate_all()                   (NEW: clears all three maps)
    for uri in open docs: publish_diagnostics(uri) # sends only in push mode

main/run (Stage 2): if client supports dynamic registration
                    → send client/registerCapability with the watcher globs
```

## Staged implementation

> Each stage on its own branch, stacked as needed.

### Stage 1 — Notification handler + invalidation + republish ✅ DONE

**Implements:** W1, W2, W3, W4, W5.

**Work:**
- `Workspace::invalidate_projects(&mut self)` (`self.projects.clear()`).
- `SemanticState::invalidate_all(&mut self)` (clear `project_parses` +
  `resolved_projects` + `assembly_envs`).
- A pure `classify_change(uri) -> ChangeClass` (in `server.rs` or a small
  module).
- `DidChangeWatchedFiles::METHOD` arm in `handle_notification`: classify all
  changes, invalidate, and republish open buffers for push-mode clients on a
  structural change.

**Correctness oracle:**
- *Classify properties:* each extension / special filename maps to the right
  class, case-insensitively; unrelated files are `Ignored`.
- *Stale-cache oracle (the headline):* evaluate a `.fsproj` with
  `DefineConstants=A` (so `Workspace::symbols_for` sees `A`), rewrite it on disk
  to `B`, deliver a `DidChangeWatchedFiles` change for it, then assert
  `symbols_for` now sees `B`. The inverse of the existing
  `invalidate_owning_project_skips_fsproj_uris` cache-proof test.
- *Republish:* after a structural change, every open buffer gets a fresh
  `publish_diagnostics` in push mode and no publish in pull mode; a source-only
  change invalidates semantic without republishing (assert via a spy / by
  observing a changed result).

### Stage 2 — Dynamic watcher registration ✅ DONE

**Implements:** W6.

**Work:**
- At the start of `run`, when the client advertised
  `workspace.didChangeWatchedFiles.dynamicRegistration`, send
  `client/registerCapability` (`State::watched_files_registration` builds the
  params; `send_request` is the thin shell). Two `{}`-group `FileSystemWatcher`
  globs — `**/*.{fsproj,props,targets,fs,fsi,fsx}` and
  `**/{global.json,project.assets.json}` — with `kind: None` (LSP default
  create | change | delete). Fire-and-forget: the client's response is ignored
  by the loop.

**Correctness oracle:**
- *Unit:* `watched_files_registration()` is `None` with no / unset / `false`
  capability, and `Some` with the expected globs (covering `.fsproj`,
  `global.json`, `.fsx`) when the client advertises dynamic registration.
- *Integration:* a capability-advertising client receives the
  `client/registerCapability` request (method `workspace/didChangeWatchedFiles`)
  as the first message; a default client receives none (a normal request
  round-trips without a stray registration first).

### Stage 3 — Referenced-assembly inputs (added later) ✅ DONE

**Implements:** the fsproj-3.3 residual ("a rebuild of a sibling isn't picked
up until the env is dropped") that Stages 1–2 deliberately left: neither
watcher glob covered binaries, so a sibling project rebuild (`bin/**/*.dll`
rewritten) — or a `.cs`/`.csproj` edit changing what the C# sidecar emits —
never invalidated `assembly_envs`, and the entry served stale referenced
types for the server's lifetime.

**Work:**
- A third `ChangeClass`, **`AssemblyInput`** (`.dll` / `.cs` for any change
  type; `.csproj` for content changes): narrower than `Structural` — project
  evaluation (defines, Compile order) doesn't depend on binaries, so
  `Workspace` caches and `project_parses` survive; no republish (binaries
  can't change an open buffer's lexer/parser diagnostics). A `.csproj`
  **create/delete** is `Structural` instead: an open `.fsproj`'s
  `<ProjectReference>` diagnostics check the target file's existence, so the
  buffer must republish (mirrors the `.fs` create/delete rule).
- `SemanticState::invalidate_assembly_state()`: clears `assembly_envs` +
  `resolved_projects` + `pdb_images`, keeps `project_parses`. The on-disk
  `AssemblyCache` needs nothing — entries are `(size, mtime)`-validated per
  DLL — and the sidecar's cache is content-addressed.
- A third watcher glob, `**/*.{dll,cs,csproj}`.

**Correctness oracle:** classify unit tests (case-insensitivity, all change
types); an Arc-identity cache-drop test plus a project-evaluation-cache
survival test on `apply_watched_changes`; and the end-to-end
`watched_assembly_refresh_e2e` — prime the env over a built F# sibling,
rebuild the sibling with a changed public surface, deliver the DLL event, and
observe the new (and only the new) surface in the refreshed env.

### Follow-up — Pull-diagnostic refresh ✅ DONE

After a `Structural` batch, `State` records diagnostic-refresh debt independently
of its open-buffer republish list. A pull client advertising
`workspace.diagnostic.refreshSupport` receives `workspace/diagnostic/refresh`;
source-content, assembly-input, and ignored changes do not incur the global
request. Diagnostic and semantic-token refreshes share one in-flight slot, with
diagnostics first and the next owed refresh sent after the matching reply.

**Correctness oracle:** capability and change-class truth tables; a no-open-buffer
wire regression; negative wire barriers for unsupported clients and source-only
changes; coalescing while a request is in flight; diagnostic-before-semantic
ordering; and unmatched-response / shutdown behaviour.

## Risks / out of scope

- **Over-invalidation (W2).** A single `.fsproj` edit clears all cached
  projects; they re-evaluate lazily. Acceptable for infrequent watched changes;
  targeted invalidation is a measured follow-up.
- **Client must watch.** A client that neither supports dynamic registration
  (Stage 2) nor watches via its own static configuration never sends the
  notification — but Stage 1's handler is correct and harmless regardless, and
  Stage 2's registration covers dynamic-registration clients.
- **Client must support diagnostic refresh.** A pull client without
  `workspace.diagnostic.refreshSupport` receives no global request and observes
  structural changes on its next natural diagnostic pull.
- **Out of scope:** targeted (per-project / per-subtree) invalidation;
  debouncing bursts of changes; watching arbitrary imported `.props`/`.targets`
  outside the glob set.
