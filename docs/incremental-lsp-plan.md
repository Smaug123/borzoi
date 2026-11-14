# An incremental LSP: file state, derived values and the edit loop

> **Status (2026-11-14).** Plan written; Stage 0 (the edit-loop count harness),
> Stage 1 (the inference cutoff), Stage 2a (counted source reads), Stage 2b
> (the `Vfs` owns source text), Stage 3a (the MSBuild evaluation records
> its read set), Stage 3b (evaluations are validated, not trusted),
> Stage 4a (ownership listings are observed and counted), Stage 4b
> (ownership is memoised), Stage 5 (one parse per text), Stages 6a and 6b
> (env read sets, targeted assembly invalidation), Stage 7 (the
> hand-wired invalidation deleted), Stage 8 (coalescing in the dispatch
> loop), with the follow-up cutoffs on the evaluation's consumed outputs, and
> Stage 9 (a running request stops for a notification) implemented; Stage
> 9's gate is still not met (see its measurement). Tracks
> [#544](https://github.com/WoofWare/borzoi/issues/544).
> The counts below were taken at 369039e9 (`origin/main`), with the harness at
> 3e426d12. Its file-system columns came from a one-off instrument (below) that
> the committed harness no longer carries.

## Problem

The issue, from Fable's diagnosis, says the server has:

- one 32 MiB dispatch thread, no debounce, synchronous diagnostics on every
  keystroke, and cancellation only for the workspace sweep;
- about 16 ad hoc cache maps with hand-wired invalidation and `Arc::ptr_eq`
  witnesses, and a documented history of eight invalidation misses.

It names five things that are concretely wrong:

1. every keystroke re-reads every Compile item from disk and string-compares it;
2. the `.fsproj` evaluation cache has no staleness check, and relies on the
   client to send watched-file notifications;
3. a save in project A evicts project B's assembly env;
4. file ownership does a `read_dir` walk up the ancestors on every request;
5. inference has no export-equality cutoff.

Its fix is a VFS that owns every file's text and revision; derived values keyed
by content hashes of their inputs, with early cutoff by value equality; and
requests run against an immutable snapshot, so that the dispatch thread only
applies edits.

This section checks each claim against the code and measures what a keystroke
costs. Most of the claims hold. Two are wrong as stated, and the issue misses
several costs that the measurement shows.

### Measurements

Stage 0's harness (`crates/lsp/examples/edit_counts.rs`) runs the real dispatch
loop (`server::run_with_fetcher`) on its own thread, over an in-memory
connection. It opens two documents in a restored corpus project, an early
Compile item `E` and the last one `L`, and makes one cold pass of requests.
Then it types into a trailing line comment of `E`, one character per
keystroke. After each keystroke it sends what a pull client (VS Code) sends:

- `textDocument/diagnostic` for both documents, echoing each `resultId`;
- `semanticTokens/full` for both documents (every edit makes the server ask
  for a `workspace/semanticTokens/refresh`);
- a hover in `L`.

A comment edit changes no file's exports, so a server that is truly
incremental should answer it most cheaply of all.

Every row counts every thread except the client's: the server thread, and the
workers it starts to read DLLs in parallel when it builds an env.

- **instructions**: process instructions less the client thread's own;
- **allocations**: from a counting allocator;
- **file-system calls**: `open`, `fstat` (std's whole-file reads ask for the
  size of the file they opened), the path `stat` family, `opendir` and
  `realpath`. These are a **one-off measurement**, made at 3e426d12 by
  redirecting the executable's own libc import slots to counting wrappers.
  That only works on macOS arm64. CI is Linux-only, so a committed version
  could never be checked there, and the committed harness drops it. Later
  stages count file-system calls inside the modules that make them (D3),
  where a test can assert them on any platform.

**Kernel instructions.** `ri_instructions` counts kernel-mode instructions
too. So a row that does file I/O carries a kernel component that depends on
the machine's state, not on the server: `semanticTokens E` (80 reads per
keystroke) and the cold rows. That component can add up to about 50%, and it
is bimodal.

On the measuring machine the vnode table is full (`kern.num_vnodes` =
`kern.maxvnodes`), so whether the 80 files' vnodes are still cached depends
on other processes' file activity. The same commit measures `semanticTokens E`
at about 24.4 M with the vnodes cached and about 36.2 M once they have been
recycled. Background file churn reproduces the high level at 369039e9 itself,
with user time and the syscall count (420) identical in both levels. A bisect
of every commit from 369039e9 to d3a4a131 measures about 24.5 M and a 164 M
keystroke total throughout, so the baseline is 164 M. Rows that open no files
do not move.

So the run-to-run agreement holds only for rows without file I/O. Across
runs, their minima agree to within 0.2%. A row with I/O should be compared at
the same kernel level: the minimum of several runs, on an otherwise quiet
machine.

A standalone open/`fstat`/read/close microbenchmark over the project's files
costs about 167 k kernel instructions per file with warm vnodes, and about
150 k more when recycled. Extrapolated, and not measured inside the LSP,
roughly 13 M of `semanticTokens E`'s 24.4 M fast level is the kernel's share
of `build_parses` re-reading the 80 files. So Stage 2b, which stops those
reads, likely saves more than its user-space accounting suggests.

**Run it under `nix develop`.** The env the server builds depends on the
environment it finds (the `dotnet` on the path, the SDK and packs it
resolves). Outside the devshell, the same binary measured a 136 M keystroke
total, because it built a different env.

Wall time is not reported, because the machine is heavily loaded.

The project is **WoofWare.Incremental** (pin `f6b8fdf7`, from the whole-project
gate's corpus cache): 82 Compile items, 314 KB. `E` is `[10] Stack.fs` and `L`
is `[81] Skeleton/Skeleton.fs`. The table gives the minimum over five
keystrokes. The maximum is within 1% on every row except `didChange`.

| message, per keystroke | instructions | allocs | open | fstat | stat | opendir | realpath |
|---|---:|---:|---:|---:|---:|---:|---:|
| `didChange E` | 0.18 M | 98 | 0 | 0 | 0 | 0 | 0 |
| `diagnostic E` (changed: re-parsed) | 4.85 M | 1,523 | 0 | 0 | 0 | 4 | 0 |
| `diagnostic L` (`unchanged`) | 3.35 M | 1,522 | 0 | 0 | 0 | 4 | 0 |
| `semanticTokens E` | 24.5 M | 8,907 | **80** | 80 | 0 | 1 | 1 |
| `semanticTokens L` | 14.3 M | 18,737 | 0 | 0 | 0 | 2 | 1 |
| `hover L` | **116.9 M** | 140,624 | 0 | 0 | 0 | 2 | 0 |
| **keystroke total** | **164.0 M** | 171,411 | **80** | 80 | **0** | **13** | 2 |
| a bare barrier (the floor each row includes) | 0.03 M | 9 | 0 | 0 | 0 | 0 | 0 |

The cold pass, once per session, for comparison:

| message, cold | instructions | allocs | open | fstat | stat | opendir | realpath |
|---|---:|---:|---:|---:|---:|---:|---:|
| `didOpen E` (evaluates the project) | 325 M | 103,110 | 117 | 117 | 371 | 67 | 252 |
| `semanticTokens E` (builds the env, parses the project) | 3,124 M | 3,565,884 | 356 | 356 | 381 | 70 | 257 |
| `semanticTokens L` (folds the rest) | 709 M | 572,732 | 0 | 0 | 0 | 2 | 1 |
| `hover L` (infers the project) | 121 M | 146,314 | 0 | 0 | 0 | 2 | 0 |

Two other runs bound the attribution:

- **With `E` moved next to `L`** (`EDIT_COUNTS_EARLY=79`), `hover L` costs
  9.7 M. So about **107 M of the 117 M** is re-inferring files 10–78, which
  this edit cannot have changed. That is item 5's cost: about two thirds of
  every keystroke.
- **WoofWare.PawPrint.Domain** (pin `4a076c83`, 34 items, 398 KB) costs
  246 M per keystroke. `hover L` is 165 M of that, `semanticTokens E` reads
  32 files, and there are 9 `opendir`s.

The hover target is the first `let` binding in `L`. The rows describe the
project fold only if the requests actually take it. A handler that falls
back to single-file analysis still answers, with lexical tokens or a local
hover, so the harness observes the path instead of the answer:

- a `tracing` subscriber counts the `semantic.project_lookup` spans the
  server thread opens, which the handlers reach only once the file's owning
  project is found and its parses are accepted;
- every token and hover request must open one for the measured project, and
  none may open one for any other;
- the cold pass must build an env, and no env may be built from zero DLLs
  (the `count` of the `build_env_from_dll_paths` span). An unrestored
  project's empty env is otherwise silent;
- any `window/showMessage` fails the run, since the server's only toast
  reports a declined fold or declined reference edges.

Every other callsite is registered as uninteresting, so the subscriber does
not move the counts. Three negative controls fail as they should: a Compile
item missing from disk, an `L` linked from outside the project's directory
tree (ownership misses it, so the requests never reach the fold), and an
unrestored SDK project.

A push client (`EDIT_COUNTS_CLIENT=push`) sends no diagnostic requests. Its
`didChange` costs 3.0 M and 2 `opendir`s instead, because it lexes, parses and
publishes synchronously.

### The claims, checked

**One 32 MiB dispatch thread: holds.** `SERVER_STACK_BYTES` is
`borzoi_sema::WALK_STACK_BYTES` (`crates/lsp/src/server.rs:574`,
`crates/sema/src/lib.rs:112`), which is `32 << 20`. `main` spawns the loop on
that one thread.

**No debounce: holds.** `pending_refresh_id` bounds server→client refreshes to
one in flight. Its own comment says that this is "a concurrency bound, **not**
a debounce" (`server.rs:213`). Every `didChange` is handled when it is read.

**Synchronous diagnostics per keystroke: holds for push clients.** `didChange`
calls `publish_diagnostics` inline (`server.rs:1210`, `server.rs:1264`): a lex
and parse of the buffer, 3.0 M instructions. A pull client gets the same work
on its `textDocument/diagnostic` request instead. Both are syntactic only, with
no semantic diagnostics yet. So the expensive part of a keystroke is not
diagnostics. It is the semantic requests that follow it (the table above).

**Cancellation only for the workspace sweep: holds.** `$/cancelRequest` is
honoured only for the in-flight `workspace/diagnostic` sweep (`server.rs:722`).
Every other request is answered before the next message is read, so a cancel
can only arrive too late. A cold `semanticTokens` costs 3.1 G instructions,
and nothing can interrupt it. Neither can anything interrupt the edits queued
behind it.

**About 16 cache maps: roughly right.** The inventory below counts 17 server
caches that hold derived data, plus 4 on disk and the memo tables inside each
`AssemblyEnv`. Six of them have a witness that the code checks: the parse
variants, the XML-doc stamps, the two env-identity caches, the inference fold
and the incremental fold base. The rest rely on an invalidation call reaching
them.

**Eight invalidation misses: the source is wrong, but the conclusion holds.**
The "eight" is from `refresh_project_deferrals`'s doc comment
(`server.rs:1322`): "Review rounds found eight bugs of exactly that shape — a
decline reached through a path nobody had added a notify call to". Those were
misses of deferral *reporting*, not of cache invalidation. Their record is
`docs/fsproj-deferral-message-plan.md`, and the fix was the same move this
plan makes: derive the state, rather than notify on events. The
*cache-invalidation* misses are a separate list, and a longer one ("The
invalidation misses" below). Most of them predate the repository import
(8d36d518), so git has only the doc comments and the tests that pin them.

**Item 1 (every keystroke re-reads every Compile item): holds, on the first
semantic request after the edit.** `didChange` calls `invalidate_file`
(`crates/lsp/src/semantic.rs:659`), which drops `project_parses` for every
project listing the file. The next fold runs `build_parses` (`semantic.rs:1433`),
and that calls `read_text` (`semantic.rs:3297`) for each Compile item.
`read_text` scans every open buffer, comparing normalised paths, then falls
back to `disk_text::read`. Each text is then compared with the parse-cache
variant's text (`semantic.rs:1554`). The measured cost is 80 `open`s per
keystroke on an 82-item project with two buffers open, each followed by the
`fstat` std's read makes. No path is `stat`ed, so the re-read is the only
staleness check these files get.

**Item 2 (the `.fsproj` evaluation cache has no staleness check): holds.**
`Workspace::projects` (`crates/lsp/src/workspace.rs:106`) is filled once per
`ProjectId` and cleared only by `invalidate_projects` (`workspace.rs:325`). That
runs only for a structural `workspace/didChangeWatchedFiles` event. The
evaluation reads the project, its `Directory.Build.*` and its imports
(117 opens, 371 stats, 67 `opendir`s and 252 `realpath`s, measured), and
remembers none of them. A client that does not watch files can be served a
stale `DefineConstants` or Compile list indefinitely, and so can a change the
watcher globs miss. The state-machine test (below) is careful not to compare
while events are pending, because the server "*cannot* know the disk moved".

**Item 3 (a save in A evicts B's env): wrong as stated, right in substance.**
Saving an F# *source* evicts no env: a watched `.fs` content change is
`ChangeClass::Source`, which only calls `invalidate_file`
(`server.rs:392`, `server.rs:506`). But saving a `.fsproj`, `.props`,
`.targets`, `global.json`, `nuget.config` or assets file anywhere calls
`invalidate_projects` and `invalidate_all`. Those drop every project's
evaluation, env, folds, PDB images and XML-doc caches (`semantic.rs:574`). Any
`.dll`, `.cs` or `.csproj` change, which every build makes, calls
`invalidate_assembly_state` (`semantic.rs:601`). That drops every project's
env and folds, whether or not that project references the DLL. A cold env plus
fold is the 3.1 G + 0.7 G above. `CachedAssemblyEnv` already holds the list of
DLLs it was built from, so this targeting is cheap to fix.

**Item 4 (an ownership walk per request): holds, and it happens several times
per request.** `Workspace::owning_project` (`workspace.rs:750`) calls
`all_fsproj_in` (`workspace.rs:994`), a `read_dir`, for each ancestor until a
project claims the file. Nothing is memoised. `symbols_for` and
`lang_version_for` each start their own walk, and `diagnose_file` asks for
both twice: once for the `resultId`, and again inside `grouped_for_uri_linked`
(`crates/lsp/src/handlers/diagnostic.rs:103`, `:109`). So one pull of a file
in a subdirectory costs four walks of two directories each, eight `opendir`s
when the report is re-computed. A keystroke costs 13 `opendir`s.

**Item 5 (no export-equality cutoff in inference): holds, and it is the largest
cost.** `InferredProject::shared_prefix`
(`crates/sema/src/infer_project.rs:280`) reuses only the *leading* files whose
`ResolvedFile` is the same allocation. `infer_project_reusing`
(`infer_project.rs:335`) re-infers everything from the first changed file to
the horizon. Resolution does cut off: `same_export_contribution`
(`crates/sema/src/resolve/model.rs:3921`) lets the incremental fold keep the
suffix's very `Arc`s after an export-preserving edit. But inference stops at
the edited file, so it re-infers the whole suffix it would have kept.

### What the issue missed

- **The edited file is parsed twice per keystroke.** The diagnostic path
  (`diagnostics::grouped_diagnostics`) and the fold (`build_parses`) each parse
  the buffer, with nothing shared. `diagnostic E` alone costs 4.85 M.
- **An `unchanged` pull still costs 3.35 M instructions and 4 `opendir`s.**
  The `resultId` is keyed on the symbols and language version, which take two
  ownership walks to find.
- **The buffer lookup is quadratic.** `read_text` scans every open buffer, and
  normalises its URI, once for each Compile item.
- **`project_parses` is rebuilt whole after any edit.** The qualified-name
  pairing (`semantic.rs:1715`) and the `ProjectParses` vectors are rebuilt
  even when every tree is a cache hit.
- **The per-message refresh is cheap, as designed.** `refresh_project_deferrals`
  runs after every message (`server.rs:689`, `:738`), but a bare barrier
  costs 27 k instructions. Nothing here needs to change.

### The caches

Key, invalidation trigger and staleness witness of every cache. A "witness" is
a check made when the value is read, as opposed to an invalidation call that
must reach the cache first.

| # | cache | key | invalidated by | witness |
|---|---|---|---|---|
| 1 | `State::docs` (`server.rs:147`) | `Uri` | text sync | none needed (it is the buffer), but no revision is kept |
| 2 | `State::doc_projects` (`server.rs:199`) | `Uri` | open/close; recomputed on a structural event | none |
| 3 | `Workspace::projects` (`workspace.rs:106`) | `ProjectId` | structural event (clears all) | none; `projects_generation` only tells the sweep that a clear happened |
| 4 | `SemanticState::project_parses` (`semantic.rs:257`) | `ProjectId` | text sync of any listed file; structural | none (rebuilt by re-reading every item) |
| 5 | `observed_fold_refusals` (`semantic.rs:273`) | `ProjectId` | every `invalidate_file` clears all; written only by `fold` | none (written only by the fold, which keeps it consistent) |
| 6 | `file_parses` (`semantic.rs:294`) | path → variants | structural (memory only) | full input equality: text, symbols, language version, provenance trust |
| 7 | `assembly_envs` (`semantic.rs:333`) | `(ProjectId, dotnet root, ServedTfm)` | structural; any `.dll`/`.cs`/`.csproj`; C#-references option; name-table cap | none; a transient sidecar failure is never cached |
| 8 | `resolved_projects` (`semantic.rs:347`) | `ProjectId` | with `project_parses`; assembly-state | none on the fast path (`semantic.rs:1052`) |
| 9 | `inferred_projects` (`semantic.rs:353`) | `ProjectId` | assembly-state; structural | `Arc::ptr_eq` on the env and on each leading resolved file |
| 10 | `prev_resolved` (`semantic.rs:369`) | `ProjectId` | assembly-state; structural | `Arc::ptr_eq` on the env (`semantic.rs:1130`); tree identity (`same_tree`); export equality |
| 11 | `pdb_images` (`semantic.rs:411`) | DLL path | assembly-state; structural | none |
| 12 | `DocFileCache` (`xml_doc/lookup.rs:85`) | `.xml` path | memory only | `(size, mtime)` `stat` on every use |
| 13 | `KeyCensusCache` (`xml_doc/lookup.rs:145`) | DLL path | memory only | env identity (`Weak::ptr_eq`) |
| 14 | `DocIdIndexCache` (`xml_doc/lookup.rs:177`) | env | memory only | env identity |
| 15 | `SidecarManager` latch (`semantic.rs:438`) | — | never (process lifetime) | the sidecar's own content addressing |
| 16 | `PublishState` (`publish.rs:49`) | target `Uri` | each publish | none needed (it records what was sent) |
| 17 | `warned_uncertain_projects` (`server.rs:187`) | project identity | recomputed after every message | derived from state, by design |
| — | `AssemblyCache`, on disk (`assembly_cache.rs`) | canonical DLL path | never | binary tag + DLL `(size, mtime)` |
| — | C# sidecar metadata, on disk | input content | never | content-addressed |
| — | SourceLink sources, on disk (`goto_source.rs`) | URL | never | immutable by URL |
| — | the pull `resultId`, held by the client (`pull.rs:62`) | 128-bit hash of (text, symbols, version) | — | the hash itself: already the design this plan proposes |
| — | memo tables inside an `AssemblyEnv` (`sema/src/assembly_env.rs:1155–1187`, `names.rs:153`, the namespace fold surfaces) | entity / name | the env's lifetime | none needed: an env's inputs are immutable |

### The invalidation misses

Every row is a cache that served a value which its inputs no longer justified.
Each was found in review or by a test, and each was fixed by widening a key or
adding one more invalidation call. A row with no PR number predates the import
(8d36d518), so its evidence is the doc comment and the test that pin it.

| miss | the fix, as it stands | pinned by |
|---|---|---|
| An env keyed on the project alone served a stale empty env after the dotnet root became known | the root in the key (`semantic.rs:333` doc) | `assembly_env_cache_keys_on_dotnet_root` |
| An env keyed without the served-TFM verdict confused `NoneDeclared` with `Untrusted` | `ServedTfm` in the key (fsproj 3.3d, round 19) | `assembly_env_cache_keys_on_target_framework` |
| A trusted project's accepted parse satisfied an untrusted one, skipping the version gate | `lang_version_untrusted` in the parse variant's key (`CachedParse`) | `untrusted_lang_version_refuses_the_fold_only_for_straddling_files` |
| An open `.fsproj` filled the project memo from disk on a path that text sync never invalidates, pinning a stale Compile list for the session (#277) | `.fsproj` buffers kept out of the deferral scope | `fsproj_sync_does_not_pin_the_project_cache` |
| An edit to a linked file invalidated one of the projects that compile it | `invalidate_file` walks every cached project | `invalidate_owning_project_clears_every_project_containing_the_file` |
| A semantic-tokens refresh owed at the fold was lost when no fold followed (#125) | the flag set at invalidation | `invalidation_without_a_fold_still_owes_a_refresh`, `edit_after_structural_invalidation_still_refreshes` |
| A transient sidecar failure's degraded env was cached (#213) | not cached, retried | `a_transient_scan_failure_makes_the_env_non_cacheable` |
| Envs built under one C#-references setting were served under the other (#440) | the setter drops them | `changing_the_csharp_references_option_drops_cached_envs` |
| A created or deleted source left a glob-expanded Compile set stale | create/delete classified as structural | `created_source_refreshes_globbed_compile_set` |
| The workspace sweep combined facts from two generations of project evaluations (#385) | `projects_generation` restarts the sweep | the structural-change property in `workspace_diagnostic_sweep.rs` |
| A rebuilt `.xml` beside an unchanged DLL kept its old docs (#307) | `(size, mtime)` stamp on every use | `a_failure_is_cached_until_the_file_changes` |
| Two spellings of one `.fsproj` shared an evaluation, which MSBuild treats as two projects (#399) | `ProjectId` is the evaluated spelling | `a_symlinked_spelling_is_its_own_project` |
| A refused project, which has no `project_parses` entry, kept its refusal after the edit that fixed it | `invalidate_file` clears every refusal | no test of the buffer-edit path; `a_recovered_fold_reports_again_when_it_breaks_the_same_way` covers the disk path |

#313 then planted ten more defects of this kind (M1–M10: the parse key
ignoring `#if` symbols; a `.fsproj`, `.dll` or source watch event ignored;
`didClose` not invalidating; reuse past an export change; a `resultId`
omitting the symbols; a refresh or republish not owed). It added
`incremental_state_machine.rs`, which catches each one. That test is the
oracle this plan builds on (D7).

The pattern is the one `docs/fsproj-deferral-message-plan.md` found for
reporting. Each cache is correct only if every path that changes one of its
inputs calls its invalidator, and nothing checks that. The fix is also the
same: make the cached value a function of inputs that the cache can see, and
check those inputs when the value is read.

## Design

### D1. One owner of file state: the `Vfs`

A `Vfs` value holds every file the server has read, keyed by the file's
lexically normalised spelling. Lookup folds case where the platform does
(`paths_equal`), and a project keeps its own `ProjectId` spelling rule. Each
entry is one of:

- `Buffer { text: Arc<str>, version }`: the client's text, which always wins;
- `Disk { text: Arc<str>, stamp }`: read from disk;
- `Absent { stamp }`: a probe that found nothing, which is a fact like any other.

A **stamp** is `(dev, ino, size, mtime_ns, ctime_ns)`. A disk entry is
validated once per **generation**. A generation is the unit of work between
two dispatched messages: the first read in a generation `stat`s the file,
re-reads it only if the stamp moved, and later reads in that generation use
the result. `ctime` cannot be set from user space, so a rewrite that keeps the
size and resets the mtime still moves the stamp.

**Reading is stamp-then-read.** A read opens the file, takes the stamp from
`fstat` on that descriptor, and then reads. A write that lands after the stamp
moves the stamp, so the next generation re-reads. Taking the stamp after the
read would be wrong: it could record the new stamp beside the old text, and
nothing would ever re-read it.

**Racy timestamps.** A filesystem's timestamps tick at its granularity. On
Linux that is the kernel's coarse clock (a few milliseconds) even where the
stored field has nanoseconds; on HFS+ it is a second, and on FAT two. So two
same-size writes within one tick can leave the stamp unchanged. The `Vfs`
adopts git's racy-timestamp rule (its `racy-git` design note).
Each stamp records the wall-clock time it was taken. A stamp whose mtime or
ctime is within `G` of that time is **racy**: it is used for this generation
and re-read in the next, until a later observation is more than `G` past it.
`G` is 2 s, the coarsest local timestamp granularity in common use. The
observation time is a value passed to `Vfs::begin_generation(now)`, not read
from a clock inside the `Vfs`, so the rule can be property-tested with
generated clocks.

The guarantee: within a generation, every read of a path returns the same
text. That text is the buffer, or what the disk held at the moment of the
generation's stamp. Once a write is more than `G` old, the first generation
after it sees it. Per platform:

- **Linux** (ext4, xfs, btrfs, tmpfs), **macOS** (APFS, HFS+) and
  **Windows** (NTFS): the guarantee holds as stated. Each has local timestamps
  at granularity `G` or finer, and `ctime` moves on every write.
- **Not covered**: filesystems whose timestamps come from another machine's
  clock (NFS and SMB mounts with clock skew greater than `G`), and those that
  do not maintain `ctime`. A write there can be missed until its stamp moves
  for another reason. A watch event, which is a hint (below), still covers it.

The existing on-disk caches accept a wider hole: they have no racy rule.

A directory listing that ownership consults (D4) is an entry too, stamped by
the directory's own `stat`. Creating, deleting or renaming an entry moves the
directory's mtime, and the racy rule applies as for files. Listings for
`<Compile>` glob expansion are **deferred**. No stage consumes them until the
evaluator's read set does (D3), which records them itself.

A watched-file event becomes a hint: it marks the named paths for an immediate
re-`stat`, and stops being the only way the server learns that the disk moved.
So for every input class it witnesses (D7), the server needs no file-watch
support from its client to be correct. Agent clients, which this server is
scoped for, mostly have none. One class stays unwitnessed:
**`CSharpSources`**. The `.cs` and `.csproj` files behind a C# project
reference are read by the sidecar process, not through the `Vfs`, so a `.cs`
edit with no watch event can serve stale C# metadata (Stage 6b). Closing that
needs the sidecar to report its read set.

**Edits are values.** `enum Edit { Open, Change, Close, Watched }` is applied
by one function, `Vfs::apply(&mut self, Edit)`, which is the only mutator. An
edit bumps the `Vfs`'s revision. The dispatch loop owns the `Vfs` mutably, and
derived computations read it through a shared borrow. D6 explains why no
persistent map or snapshot type is needed yet.

### D2. Derived values are pure functions of named inputs, memoised by value

Each derived kind has one memo table with an explicit key: the identities of
its inputs. There is no dependency tracking, and no "invalidate X when Y"
call. A lookup recomputes the key from the current inputs, and a hit is a value
whose recorded inputs equal them:

| derived value | key (all by value) |
|---|---|
| a parse | text (`Arc<str>`, compared by pointer and then by bytes), symbol set, language version, provenance trust: today's `CachedParse` key |
| a project evaluation | its **read set** (D3), re-validated against the `Vfs` |
| a file's owning project | the stamped ancestor listings, and the evaluations the walk consulted |
| an assembly env | the read set of its reference resolution (D3, Stage 6a) |
| a file's resolution | as today: same tree, same pairing, same env, and an equal threaded prefix |
| a file's inference | its resolution (`Arc`), the env, and a published prefix in which every earlier file's own contribution is equal to before |
| a file's syntactic diagnostics | the parse |

Hashes only *find* a candidate. A hit is confirmed by equality: of the bytes
for text, and of the value for exports and published types. So a hash
collision costs a comparison, never a wrong answer. The pull `resultId` stays
a 128-bit hash, because the client holds it and the server cannot compare
texts it no longer has.

**Early cutoff** is equality of a derived value's *output* with its previous
output, at the points where a sequential fold threads state from file to file.
Resolution already does this (`same_export_contribution`). Inference does it
on each file's contribution to `PrecedingTypes` (Stage 1). A file is reused
while every earlier file's contribution equals its previous one and the
file's own resolution is the same allocation. Today, only the leading run is
reused.

A memo table is bounded by the inputs it can still match. When a newer value
is stored for the same file and settings, the entry it replaces is dropped, as
`file_parses` does now. Correctness never depends on eviction.

### D3. Computations that touch the disk themselves record a read set

Three computations read the file system directly, not through a value they
are passed:

- the MSBuild evaluation, with about 70 `std::fs` call sites in
  `borzoi-msbuild`: imports, `Exists()`, globs, SDK resolution;
- the ownership walk;
- the assembly env's reference resolution: the assets file, the project
  graph, the output-DLL search, the packs. In `crates/lsp/src`, about 380
  lines touch the file system across 23 files (a grep, inline tests included).
  Most of them are this.

The fourth reader, the fold's source reads (`read_text`, `document_text`),
becomes the `Vfs`'s disk side (D1).

Making them pure by passing every file in would mean knowing the read set in
advance, which is impossible. Instead each records what it observed, **as
data**: `ReadSet = Vec<(path, Observation)>`, where `Observation` is
`Text(hash)`, `Listing(names)`, `Absent` or `Stamp(stamp)`. It returns that
alongside its result. Re-validation is a pure function of the read set and the
current `Vfs`. Recording absence is required, not optional: an evaluation that
concluded "no `Directory.Build.props` here" depends on that file staying
absent. "Absent vs unread" is the recurring defect class in this repository.

Each reader keeps its direct code. Its file-system calls move into an
**`observed` module**, which performs each call and appends the observation.
That module is also the only place file-system calls are **counted**: it keeps
per-generation counts by kind (opens, `stat`s, listings), and a test reads
them. So "a comment keystroke performs 0 opens" is an assertion that runs on
Linux CI, not a measurement on one developer's Mac.

`disallowed-methods` entries ban `std::fs` outside the `observed` modules, as
`clippy.toml` already bans direct child-process spawns.

- `borzoi-msbuild` gets a crate-local `clippy.toml` that repeats the
  workspace's spawn bans and adds `std::fs`.
- In `borzoi`, the ban landed in Stage 2a. A call site on a path no stage
  routes yet is a method of `observed::Unobserved`, whose variant names its
  reader and the stage that will route it (3b for the LSP's evaluation reads,
  4a for ownership, 6a for env resolution), or says that none does. The enum
  is the worklist: routing a reader deletes its variant, and the compiler
  lists what is left. Test code reaches the `std` calls through a `host_fs`
  helper rather than a wholesale allow, because `disallowed_methods` is one
  lint: allowing it for `std::fs` also allows the spawns it bans. Stage 3a's
  crate-local `clippy.toml` in `borzoi-msbuild` meets the same choice.

So a new call site that skips the read set and the counts is a lint error,
not a review finding.

**Immutable roots.** The SDK install and the NuGet global packages folder hold
versioned directories whose *contents* are immutable at their path.
`AssemblyCache` already relies on that axiom (`assembly_cache.rs`, "Keying").
So a `Text` or `Stamp` observation of a file inside a versioned directory
there is recorded but not re-`stat`ed on every validation. Re-checking all of
them would cost about 800 file-system calls per evaluation (the cold
`didOpen E` row above).

The axiom covers existing versions only. Installing a package, or adding an
SDK or targeting-pack version, changes which versions exist without touching
any existing file. SDK resolution (`sdk_resolver::collect_from_dotnet_root`)
and pack selection (`project_assets::framework::best_version_with_tfm`) read
exactly that from directory listings. So every `Listing` and every `Absent`
observation is validated, under these roots as elsewhere: one `stat` of each
listed directory, a handful per evaluation.

A structural watch event still drops everything, so a client that watches
files keeps today's coverage of an in-place rewrite of a versioned file.
Everything outside these roots (the project, its imports, `Directory.Build.*`,
`global.json`, `nuget.config`, the assets and lock files) is re-validated in
full.

### D4. Ownership is a memoised derived value

`owning_project(file)` is a function of the ancestor directories' listings and
the evaluations it consulted. Both are D1/D3 inputs, so it is memoised per file
and validated by their stamps. `symbols_for` and `lang_version_for` share one
answer instead of walking twice each. The membership-over-proximity rule and
its `Membership::Unknown` discipline are unchanged. Only how often it runs
changes.

### D5. No framework

Salsa (or a similar framework) would give automatic dependency tracking, early
cutoff, cancellation and parallel snapshots. Weighed against a hand-written
`Vfs` and one memo table per derived kind:

- **Local reasoning.** With salsa, the dependency graph is whatever a tracked
  function happened to read at run time. A call site does not say what a value
  depends on, and the framework, not the caller, decides when to recompute.
  That is the framework inversion the design principles warn against. A memo
  key written out per kind (D2) puts the inputs at the call site.
- **Fit.** About seven derived kinds exist (parse, evaluation, ownership, env,
  resolution, inference, diagnostics). The two hard ones, resolution and
  inference, are sequential folds that already have a hand-written incremental
  algorithm with cutoff, held to batch by a differential. Salsa would model
  them as a chain of per-file queries over the threaded prefix: the same
  algorithm, restated in the framework's terms. `AssemblyEnv` grows an
  append-only name table through interior mutability (the interning plan's
  D2). Salsa requires immutable `Eq` values, so the env would have to be split
  or wrapped.
- **Reach.** `borzoi-sema` is meant to be reusable and depends only on `cst`
  and `assembly`. Salsa's database type would have to run through its public
  API, or be wrapped at the LSP boundary, which leaves the hand-written memo
  table anyway.
- **Supply chain.** The devshell vendors an offline registry. Salsa brings
  roughly a dozen transitive crates (dashmap, parking_lot, crossbeam,
  boxcar, indexmap, …) and a fast-moving API.
- **What salsa would buy that this plan does not** is cancellation and
  parallel queries against snapshots. D6 argues that neither is needed until
  measurement says so. If Stage 9 finds that it is, salsa is worth weighing
  again, against a snapshot of the `Vfs` plus these same tables.

The trigger for revisiting this decision is a derived kind whose inputs
cannot be named at its call site, or more kinds than one module can hold.

### D6. Concurrency only by measurement; coalescing first

The issue proposes requests against an immutable snapshot, so that the dispatch
thread only applies edits. That buys two things: a long request no longer
blocks edits, and a superseded request can be dropped. It costs a second thread
that touches derived state, which means synchronised memo tables or
per-snapshot copies, and it costs local reasoning.

The measurement says where the time goes. A steady keystroke is 164 M
instructions, and Stages 1–5 address about 115 M of that (the expected gains
below). What then remains slow is *cold* work: the first fold (3.1 G), an
evaluation (0.3 G), and an env rebuilt after a build. That work happens once
per session or once per build, not once per keystroke.

So two steps come first, both on one thread:

- **coalescing**: before handling a message, the loop drains the channel. A
  `didChange` superseded by a later one for the same document is applied but
  not diagnosed. A request already named by a `$/cancelRequest` in the drained
  batch is answered `RequestCancelled` without running. A semantic request
  whose document has a newer queued change is answered `ContentModified`.
- **re-measuring**, with Stage 0.

A snapshot worker (Stage 9) is built only if, after those steps, a
representative script still blocks edits behind requests for longer than a
stated budget. Stage 9 writes the budget and the script down now. Coalescing
cannot interrupt a request that is already running, so the case it leaves
open is an edit that arrives during a cold fold. That is the case the issue's
snapshot proposal addressed. If it is built, the snapshot is the `Vfs` (cloned: about a
hundred `Arc`s) plus the memo tables behind a lock. It is not a persistent data
structure.

### D7. The oracle: an incremental answer equals a from-scratch answer

The oracle exists already. `crates/lsp/tests/all/incremental_state_machine.rs`
generates client scripts over a small world: three projects, four
cross-referencing sources with `#if` blocks, a props file, and restored assets
over the real `System.Runtime.dll`. The scripts open, edit, close and save
buffers, write and delete files on disk, and flush watch events. At every
`Check`, every handler's answer from the long-lived session must equal the
answer from *fresh* sessions over the same disk and buffers. That covers
hover, definition and references at every identifier, completion, symbols,
tokens, both diagnostic models, and what a push or pull client would display.
It catches #313's ten planted defects, and its shrunk failures are pinned as
scripts.

It has one blind spot, which matters to this plan: it never compares while
watch events are pending, because today the server cannot know that the disk
moved. This plan makes the server able to know (D1, D3, D4). So each stage
widens one data value in the test, `WITNESSED: &[InputClass]`. `InputClass`
is `SourceContent`, `ProjectFiles`, `DirectoryMembership` or
`CSharpSources`. The last is **unwitnessed by design** (D1, Stage 6b): it is
named so that no claim can include it by accident. A `Check` runs with events
pending whenever every pending event's class is witnessed. A stage that claims
to witness a class has to survive generated scripts that write files of that
class and never flush. Once the three witnessable classes are witnessed, a
script with no `Flush` at all must agree with a fresh session. The world has
no C# project, so `CSharpSources` never arises there.

Four more oracles, because equivalence cannot see missed work:

- **Per-kind batch equivalence.** Resolution has `resolve_incremental_diff.rs`
  (incremental ≡ batch over generated edit sequences) and
  `body_edit_preserves_full_suffix_reuse`, which guards the spurious-invalidation
  direction. Inference gets the same pair when it gets a cutoff (Stage 1).
- **Memo ≡ recompute.** Each memo table has a property test against calling
  its function directly, over generated input sequences that include an input
  changing back to an earlier value. That ABA case is where a stale hit
  hides.
- **Read-set soundness** for D3. Perturbing any file that is *not* in an
  evaluation's read set must not change the evaluation. Perturbing one that
  *is* in it must fail validation. The perturbations are generated over the
  msbuild crate's existing project fixtures, and include creating a file that
  was recorded `Absent`.
- **Counts.** Each stage reports `edit_counts` before and after. A claimed gain
  that does not show up there is a defect in the stage, even if every
  equivalence test passes. File-system counts come from the `observed`
  modules (D3), so an LSP test can assert them on CI ("a comment keystroke
  performs 0 opens") as well as the harness reporting them.

## Implementation plan

> Implement this plan with each stage on its own branch, stacked as necessary
> on previous branches, so that a reviewer can review each branch in isolation.

Every stage lands alone on `main`, with the full gate, the whole-project gate
and `incremental_state_machine` green. Each reports `edit_counts` before and
after on WoofWare.Incremental, with the default script. The expected gains are
derived from the measured rows and from the `EDIT_COUNTS_EARLY=79` attribution
run. Each stage restates its gain as a measurement when it lands.

File-system counts in these stages come from the `observed` modules (D3), not
from the one-off instrument behind the Measurements table. A stage that
routes a reader through its module, with behaviour unchanged, **reproduces**
the one-off count (the "a" stages) before a later stage changes it. That ties
the new counter to the old measurement before anything moves. `edit_counts`
prints the `observed` counts as columns from Stage 2a on, on every platform.

The order puts the largest cheap win first (Stage 1). The concretely wrong
item (Stage 3) follows as soon as the state it needs exists (Stage 2b). The
rest are ordered by size.

### Stage 0: the edit-loop count harness

**Dependencies**: none. **Implements**: the measurement every later stage
reports.

`crates/lsp/examples/edit_counts.rs`, described under "Measurements". It
counts instructions and allocations per message for a scripted session on a
restored project, across every thread except the client's. The run fails if
any semantic request does not reach the measured project's fold, if an env is
built from no DLLs, or on a deferral toast.

**Oracle**: it runs to completion under `nix develop` and reproduces the
instruction and allocation baseline. Allocations are exact. Instructions agree
to within 0.2% on rows without file I/O, and at the same kernel level on rows
with it (see "Kernel instructions").

### Stage 1: an export-equality cutoff for inference

**Dependencies**: none. **Implements**: D2's cutoff for inference (item 5).

`PrecedingTypes::publish` is a deterministic function of
`(idx, resolved, inferred, screened)`. So a file's **contribution**, what
`publish` adds for it, is a value that can be computed on its own and
compared. `publish` is split into computing a `FileContribution` and merging
it, with no change to the result.

`infer_project_reusing` then folds with a running "prefix still equal" flag,
as the resolver's `in_sync` does. While the flag holds, a file is reused when
its `ResolvedFile` is the same allocation as the one `prev` inferred and its
pairing is unchanged. A file that is re-inferred while the flag holds has its
contribution compared with the contribution `prev`'s file makes, computed
from `prev`'s own `(resolved, inferred)` and not stored. The flag stays set
only if they are equal.

Each comparison is one file's contribution. Nothing stores or compares a
whole `PrecedingTypes` per file, which would be O(files × published).
`ModuleValue` and `Ty` already derive `Eq`; the case, record and type entries
gain it where they lack it. `inferred_prefix_for` is unchanged apart from
calling the new reuse.

**Oracle**:
- A new `infer_incremental_diff` in `borzoi-sema`: incremental ≡ batch
  inference, over the edit sequences `resolve_incremental_diff` already
  generates.
- `body_edit_preserves_full_inference_reuse`: after an export-preserving body
  edit to file `k`, exactly one file is re-inferred. This guards the
  missed-reuse direction, which the equivalence test cannot see. An inference
  reuse count sits beside `last_fold_reused_files` so the LSP can assert it
  too.
- `incremental_state_machine` and the inference corpus gate are unchanged.

**Expected gain**: about 107 M of re-inference leaves `hover L`, taking it
from 117 M toward the 9.7 M measured with `E` next to `L`. The keystroke total
goes from 164 M to about 57 M, and allocations fall by about 130 k per
keystroke.

**Implemented** (`crates/sema/src/infer_project.rs`), as planned:

- `FileContribution` borrows from the file's resolution and inference, and
  `PrecedingTypes::merge` clones it into the table; `publish` is gone. Its
  constructor names, cases and records are sorted by key, so that equality
  does not depend on a hash map's iteration order. The case and record types
  already had `Eq`; the sort needed `Ord` on `ItemId`.
- The per-file reuse key (`FileInputs`) is the `ResolvedFile` allocation, the
  Compile item (its tree by identity and its QNOF, compared with the
  resolver's own `same_tree`), whether a signature screens the file, and
  whether it repeats an earlier implementation's QNOF. That last is now
  computed with a running set rather than a scan of the earlier files, since
  every file now asks it.
- `infer_project_reusing_with_reuse` returns the per-file reuse vector, and
  the LSP keeps its count as `SemanticState::last_inference_reused_files`
  (`incremental_inference_reuses_what_the_cutoff_allows`).
- `InferredFile`, `PrecedingTypes` and `InferredProject` gained value
  equality, which the differential compares. The project's ignores the reuse
  key, which is how the result was computed and not what it is.
- Once a file's contribution differs, every later file is inferred afresh,
  including files that read nothing from the changed export, as the
  resolver's `in_sync` does. A finer cutoff would need each file's read set
  of exports.

The oracle is as planned, with these additions:

- The scenario generator moved from `resolve_incremental_diff.rs` to
  `tests/all/common/edit_script.rs`, which both differentials use. Its edits
  never changed a type behind unchanged exports: every literal was an `int`. So
  it gained bodies of other types (a string, a tuple, a generic lambda,
  `None`), uses of an earlier file's union case, field reads of an earlier
  file's record, and a union case gaining or losing a payload. This changes
  the case space `resolve_incremental_diff` runs over, too.
- `generator_exercises_the_cutoff` asserts that the seeded scripts reuse a file
  after an earlier one is inferred afresh, and that some file whose resolution
  the resolver reused has its inference redone with a different result: the
  case a cutoff that took unequal contributions as equal would serve stale.
- `body_edit_preserves_full_inference_reuse` makes three edits that keep a
  file's contribution: a leading comment, every literal bumped, and every
  literal body swapped with a `let` body of the same type, which renumbers
  every definition after it.
- An ignored corpus sample, `incremental_equals_batch_over_a_corpus_sample`,
  runs in CI beside `infer_corpus_diff`. It takes 24 windows of 6 consecutive
  Compile items from the corpus's `.fsproj` files (signatures included) and
  edits every file four ways: a leading comment, a binding appended, its first
  `int` literal made a string, and its first module-level body made
  `failwith "edit"`. Wherever the resolver reused every file but the commented
  one, inference must too: 126 of 126 comment edits did.

Two planted bugs were caught, then removed:

- Taking unequal contributions as equal fails `incremental_equals_batch`,
  `generator_exercises_the_cutoff`, two pinned cases, and the corpus sample.
  The corpus sample caught it only after the `failwith` edit was added; before
  that, no edit of the sample changed an export's type behind a reused
  resolution.
- Reusing past a changed prefix (the flag dropped from the reuse condition)
  fails the same five tests.

**Measured** on WoofWare.Incremental from 5f98f07b, the minimum over 20
keystrokes:

| message, per keystroke | instructions before | after | allocs before | after |
|---|---:|---:|---:|---:|
| `didChange E` | 0.17 M | 0.17 M | 98 | 98 |
| `diagnostic E` | 4.89 M | 4.89 M | 1,526 | 1,526 |
| `diagnostic L` (`unchanged`) | 3.36 M | 3.35 M | 1,522 | 1,522 |
| `semanticTokens E` | 24.6 M | 24.5 M | 8,610 | 8,611 |
| `semanticTokens L` | 14.1 M | 14.1 M | 18,440 | 18,440 |
| `hover L` | **117.2 M** | **6.43 M** | 140,624 | 7,015 |
| **keystroke total** | **164.4 M** | **53.5 M** | 170,820 | 37,212 |

On WoofWare.PawPrint.Domain (minimum over 10 keystrokes), `hover L` falls
from 165.8 M to 9.6 M and the keystroke total from 242.9 M to 86.6 M.

`hover L` falls further than the 9.7 M expected, which was measured with `E`
at index 79 and so re-inferred three files; here only `E` is inferred afresh.
The cold rows move by under 1%: a cold `hover L` is 122.7 M before and
122.3 M after, with 146,314 and 146,618 allocations (the extra 304 are the
reuse keys). One-shot inference is unchanged: `sema_counts` over the corpus's
`src` reads `resolve_file` 1135.7 → 1135.4 and `infer_file` 175.4 → 175.4
instructions per byte, with allocations identical.

### Stage 2a: source reads are observed and counted

**Dependencies**: none (parallel with Stage 1). **Implements**: D3's `observed`
module and counts, for the fold's source reads.

`read_text`, `document_text` and `disk_text::read` move into an `observed`
module of `borzoi`. It counts opens per generation, and generations begin in
the dispatch loop. The crate-wide `disallowed-methods` ban on `std::fs` lands
here, with the allow worklist of D3. Behaviour is unchanged.

**Oracle**:
- Every test and manifest is unchanged.
- A new LSP test on a generated `N`-item project with two buffers open: a
  comment keystroke followed by a semantic-tokens request performs exactly
  `N − 2` opens. This pins today's behaviour.
- `edit_counts` prints 80 opens per keystroke on WoofWare.Incremental,
  reproducing the one-off count.
- `cargo clippy` fails on a new direct `std::fs` call outside the module and
  the worklist.

**Expected gain**: none (the stage is infrastructure).

**Implemented** (`crates/lsp/src/observed.rs`, `crates/lsp/clippy.toml`):

- `observed::Sources` replaces `State::docs`' `HashMap<Uri, String>`. It
  holds the open buffers and performs the source reads: `text_of` (formerly
  `semantic::read_text`, the fold's), `document_text` (formerly
  `handlers::diagnostic::document_text`) and `read_disk` (formerly
  `disk_text::read`). `disk_text` keeps the pure byte-order-mark rule. Every
  disk read counts one open, whether or not it succeeds.
- The dispatch loop begins a generation before each message, and before each
  step of a `workspace/diagnostic` sweep, and ends it afterwards.
  `Sources::end_generation` sends the generation's number and counts to the
  channel a caller attached with `Sources::log_generations`, which is how a
  test reads them from a loop it does not own, and emits them as the
  `observed.generation` `tracing` event, which is how `edit_counts` reads
  them. A test does not read the event: `tracing` caches a callsite's
  interest process-wide, and with one dispatcher registered it asks only the
  default of the thread that first reaches the callsite. A loop running in
  another test of the same binary would cache "never" for the test's
  scoped subscriber too. The counts sit in a `Cell`, because the fold borrows
  the buffers shared beside the rest of the state.
- The crate's `clippy.toml` repeats the workspace's (Clippy reads only the
  nearest one; `clippy_config_repeats_the_workspace_bans` holds it to the
  workspace's lines) and bans the calls that observe the file system: the
  `std::fs` reads, listings, `metadata`, `canonicalize` and opens, and the
  `Path` probes (`exists`, `is_file`, `is_dir`, `metadata`, …). Writes are
  not observations and are not banned.
- The worklist is `observed::Unobserved`, not `#[allow]` attributes: 60 call
  sites in the library, by variant `EnvResolution` (Stage 6a; 28),
  `Evaluation` (3b; 8), `Ownership` (4a; 1), and four readers that **no stage
  routes**: `Restore` (12), `GoToDefinition` (8), `XmlDoc` (2) and
  `WorkspaceSweep` (1). The last four are outside D3's list of readers; each
  variant's doc says why it is left (immutable roots, its own stamp witness,
  a child process's scratch output, the sweep's walk of the roots).
- Test code reaches the banned calls through `host_fs` (the library's
  `observed::host_fs` under `cfg(test)`, and `tests/all/common/host_fs.rs`).
  The plan said inline test modules would allow the lint wholesale; that
  would also have lifted the spawn ban in every test, which the workspace
  relies on, so about 200 test call sites were rewritten mechanically instead.
  No test's assertions changed.
- `edit_counts` prints an `opens` column from the generation events.

The oracle is as planned:

- `observed_counts` (`tests/all/observed_counts.rs`) runs the real loop over
  a generated `N`-item SDK-less project (`N` from 3 to 9, two open items
  chosen at random, push and pull clients). It pins three counts: the cold
  `semanticTokens` request on one open item opens `N − 2` files, a comment
  keystroke followed by that request opens `N − 2`, and the other open item's
  request, answered from the cached fold, opens 0. It also checks that the
  generations are numbered consecutively and that there is at least one per
  message, which is the non-vacuity check that the loop's generations run.
- Written first against a `read_disk` that counted nothing, it failed
  (`0 ≠ N − 2`). Three planted bugs fail it, and were removed: `text_of`
  ignoring the buffers (`N` opens); the loop never beginning a generation
  (the numbering check); and `begin_generation` not resetting the counts
  (cumulative sums).
- A direct `std::fs::read_to_string` planted in `build_parses` fails
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`.

**Measured** on WoofWare.Incremental, 20 keystrokes per run, each row the
minimum over three runs of each binary. "Before" is #633's head
(dd80d7e1).

| message, per keystroke | instructions before | after | allocs before | after | opens after |
|---|---:|---:|---:|---:|---:|
| `didChange E` | 0.14 M | 0.17 M | 98 | 98 | 0 |
| `diagnostic E` | 4.88 M | 4.87 M | 1,526 | 1,526 | 0 |
| `diagnostic L` (`unchanged`) | 3.34 M | 3.34 M | 1,522 | 1,522 | 0 |
| `semanticTokens E` | 24.2 M | 24.0 M | 7,804 | 7,804 | **80** |
| `semanticTokens L` | 14.2 M | 14.3 M | 17,113 | 17,113 | 0 |
| `hover L` | 6.42 M | 6.42 M | 7,015 | 7,015 | 0 |
| **keystroke total** | **53.3 M** | **53.1 M** | 35,078 | 35,078 | **80** |

The 80 opens reproduce the one-off count, and the cold `semanticTokens E`
opens 80 too. Allocations are identical. `semanticTokens E` sometimes takes
one more allocation, of 328 bytes, in both binaries' runs. `didChange E`'s
"after" leaves out one run's 8 k reading, which is below the bare barrier
it includes and so is a counter artefact; its other two runs read 0.17 M
and 0.18 M, against the baseline's 0.14–0.18 M. The cold rows move by under
1.5%, in both directions.

### Stage 2b: the `Vfs` owns file text

**Dependencies**: Stage 2a. **Implements**: D1 for source files.

The `Vfs` replaces `State::docs` and the `observed` source reads. Its disk
side reads stamp-then-read and applies the racy rule. `build_parses` asks the
`Vfs` for each Compile item. An unchanged disk file then costs one `stat` per
generation and returns the same `Arc<str>`, so the parse-cache comparison
becomes a pointer test. `read_text`'s scan of every buffer goes away.

**Cached folds are validated, not only rebuilt.** Once a project's fold is
cached, `parses_for_project` and `resolved_prefix_and_env_for` answer from
`project_parses` and `resolved_projects` without reaching `build_parses`, and
so without reading the `Vfs`. Re-reading in `build_parses` alone would
therefore leave a closed Compile item's disk edit unseen until something
invalidated the project. So `ProjectParses` records the stamp (or buffer
version) of each Compile item it was built from. Every generation that reads
a cached fold validates those, and a mismatch drops the cached parses and the
resolved project exactly as `invalidate_file` does today. Watched source
events keep calling `invalidate_file` as well, and also become re-`stat`
hints. The event path is removed only in Stage 7, once every cache it guards
is validated.

**Oracle**:
- Stage 2a's count test flips: the comment keystroke performs **0 opens**,
  and each request that reads the fold performs at most `N` `stat`s.
- `incremental_state_machine` gains `WITNESSED = [SourceContent]`. Scripts
  that `Write` a source and `Check` without `Flush` must agree with a fresh
  session. At least one pinned script does exactly that and fails on `main`
  (checked once while writing the stage, by running it against the parent
  commit). The world's writes go through the real filesystem, so on Linux CI
  the racy rule is what keeps two same-size writes within a tick from being
  missed.
- A `Vfs` property test with a generated clock. Over generated sequences of
  buffer edits, disk writes (same-size rewrites and rewrites back to earlier
  content included) and `begin_generation(now)` calls, `Vfs::read` equals
  "buffer if open, else the disk's bytes without a BOM" at every generation
  that is more than `G` after the last write. Within `G` it equals either the
  old or the new bytes, and the next generation re-reads. The disk is a model
  whose timestamps tick at a generated granularity up to `G`, so the property
  covers coarse-clock filesystems that the CI machine does not have.

**Expected gain**: per keystroke, 80 opens and 80 `fstat`s become 0. Path
`stat`s go from 0 to at most 80 per request that reads the fold, which is
three requests (`semanticTokens E`, `semanticTokens L`, `hover L`), so about
240 per keystroke. Instructions are not estimated in user space;
the kernel's share of the 80 reads, extrapolated from a microbenchmark, is
roughly 13 M of `semanticTokens E`'s 24.4 M with warm vnodes, and more when
they are recycled (see "Kernel instructions"). The stage measures both levels. The other expected effect is that `semanticTokens E` falls
toward the cost of parsing and resolving `E` alone.

**Implemented** (`crates/lsp/src/vfs.rs`, `crates/lsp/src/observed.rs`,
`crates/lsp/src/semantic.rs`):

- `vfs::Vfs` replaces `observed::Sources` as `State::docs`. It holds the
  buffers as `Arc<str>`, indexed by normalised path, so `text_of` is a hash
  lookup rather than a scan of every buffer. Its disk side is a `vfs::Disk`:
  `observed::HostDisk` in the server (`metadata` for a `stat`; open, `fstat`
  on the descriptor, then read), and a model with a generated clock in the
  tests. Generic over the disk, with `HostDisk` as the default parameter, so
  no server signature names it. The stamp is `(dev, ino, size, mtime, ctime)`
  from `MetadataExt`. On a non-unix host `std` exposes no change time, so the
  stamp is the size and mtime with no ctime, and a stamp without one is
  always racy: such a host re-reads every unbuffered item in every
  fold-reading generation, as Stage 2a did.
- A disk entry is `Present` (stamp, the generation time it was taken, and the
  text, `None` when not UTF-8), `Absent`, or `Failed` (any other error,
  re-read in every generation). Racy stamps (mtime or ctime within `G` = 2 s
  of the generation's time) are re-read in every generation, and a re-read
  whose text is unchanged keeps the old allocation, so pointer identity
  survives the racy window.
- The generation log (`log_generations`, and `end_generation` returning the
  `Generation`) moves from `Sources` to the `Vfs` unchanged, with `stats`
  beside `opens` in its counts and in the `observed.generation` event.
- Outside a generation the `Vfs` remembers nothing and reads every time. That
  keeps the semantic layer's unit tests, which call it directly with no loop,
  on today's semantics.
- The generation's time is `State::clock` (`vfs::Clock`), read in the loop
  and passed to `begin_generation`. `Clock::HostAhead` and `Clock::HostBehind`
  exist for tests whose disk does not change while the server runs: ahead
  makes freshly written files read as old, behind makes every stamp racy.
- **The witness is the text, not the stamp.** `ProjectParses::texts` already
  held each item's text; they are now the `Vfs`'s own allocations, and
  `SemanticState::validate_cached_fold` asks the `Vfs` for every item again
  and compares by pointer, then by bytes. That covers a buffer (its `Arc`
  changes with every edit) and a disk file (the same `Arc` while its stamp
  holds) with one rule, and it keeps the fold when an item is rewritten to
  the text it had, which a recorded stamp or version would drop. The plan
  said to record stamps and buffer versions; this is the deviation.
- Validation runs at the top of `resolved_prefix_and_env_for` (before its
  fast path) and `parses_for_project`, and is memoised per generation on
  the cached entry (`CachedParses::validated_in`). A mismatch drops the
  parses, the resolved fold and the fold refusal, and owes a
  `semanticTokens/refresh`, as `invalidate_file` does. `inferred_prefix_for`
  reads only after `resolved_prefix_and_env_for`. `fold_outcome`, which the
  deferral refresh reads after every message, does not validate: a deferral
  message about a project whose item moved on disk unwatched is corrected by
  the next request that reads the fold, not before.
- A fold that succeeds where the last one was refused owes a
  `semanticTokens/refresh` too: an unwatched restore of a deleted item is
  noticed by the fold itself, and nothing else would tell the client that
  its single-file tokens are stale.
- The parse cache compares texts by pointer first, and a byte-equal hit
  adopts the `Vfs`'s allocation, so the next comparison is a pointer test.
- Watched-file events still call `invalidate_file` (and everything else they
  called), and each event's path is also a `Vfs::hint`, which forgets the
  entry so the next read re-reads it whatever its stamp says.
- A disk text the `Vfs` re-reads is not, by itself, reported to a
  workspace-diagnostic client: an unwatched write that no fold reads owes no
  `workspace/diagnostic/refresh`. The client's next pull sees it, since its
  `resultId` hashes the text.

The oracle:

- `incremental_state_machine` gains `InputClass` (`SourceContent`,
  `ProjectFiles`, `DirectoryMembership`, and `ReferencedAssemblies` for the
  assets file and the DLL, which the plan's list had no class for) and
  `WITNESSED = [SourceContent]`. A source write is `DirectoryMembership` when
  it creates or deletes a source in a directory a project on disk globs, and
  `SourceContent` otherwise. A `Check` runs whenever every pending event is
  witnessed, and `run_case` checks once more before its final flush. An
  unflushed `Check` asks the direct queries first and compares what the
  client displays after the refreshes they owed: until the server has had a
  message since the write, nothing could have told the client.
- `unwatched_source_writes_agree_with_a_fresh_session` generates scripts whose
  disk writes and deletes are all sources and which never flush, and asserts
  that comparisons with writes pending happened (`unflushed_checks`).
- Four pinned no-flush scripts: a rewrite of a closed source, the same at
  the same size (`x` renamed `y`), a delete (the fresh session refuses the
  fold), and that delete followed by the file's restore (found by Codex; the
  generated test does not reach it at its default case count). Run against Stage 2a, the first three fail, as does the existing "external
  write" script now that `run_case` checks before its final flush, and the
  generated test (minimal script: open `A`, rewrite `Shared/S.fs` without
  `x`).
- `vfs::tests` holds the property with a generated clock and a model disk
  ticking at 1 ns, 1 ms, 1 s or 2 s: over writes (same-size and back to
  earlier content, a byte-order-mark spelling, non-UTF-8), deletes, buffer
  edits, clock moves, writes in the middle of a generation and writes landing
  between a read's stamp and its bytes, **every** generation's first read is
  exact. The argument in the module docs shows why the plan's "within `G` it
  may be the old bytes" is not needed: a write after a non-racy stamp always
  carries a later timestamp, and a racy stamp is re-read. Later reads in a
  generation return the same allocation and touch nothing, and an unchanged
  file read more than `G` after its mtime costs exactly one `stat`.
  `the_generator_reaches_the_racy_and_the_stamp_only_cases` is the
  non-vacuity check (190 hidden writes caught, 1,056 stamp-only reads and
  1,424 absent re-checks over its seed).
  `a_same_size_write_within_the_window_of_the_stamp_is_not_missed` walks the
  racy hole step by step on a one-second disk.
- `observed_counts` flips: on a clock ahead of the files, the cold request
  opens `N − 2`, and the keystroke plus request, and the other document's
  request, each open 0 and `stat` exactly `N − 2`. The control runs on a
  clock behind, where every stamp is racy, and each of the three opens
  `N − 2`.

Planted, each caught, then removed:

- Skipping `validate_cached_fold`: the pinned and generated no-flush tests and
  both `observed_counts` properties fail.
- Validating only the items open in buffers: the same four fail.
- Not owing a refresh on a validation mismatch: the pinned and generated
  no-flush tests fail (the displayed tokens of the other open document).
- Ignoring the racy window: the `Vfs` property, the step-by-step racy test,
  the allocation test and the non-vacuity test fail, and so does the
  `observed_counts` control. The state machine does not see it on macOS,
  whose APFS stamps move at nanosecond granularity.
- Reusing a disk entry without comparing stamps, and treating a recorded
  absence as permanent: the `Vfs` property and its non-vacuity test fail.

**Measured** on WoofWare.Incremental, 20 keystrokes per run. "Before" is
#636's head (2e19e69c), the minimum over six runs; "after" is the minimum
over three. The rows that touch the disk (`semanticTokens E` before; the three
fold-reading rows after) are kernel-heavy (see "Kernel instructions"); their
runs agreed to within 1.3% before and 0.4% after, so every run sat at the
vnode-cached level.

| message, per keystroke | instructions before | after | allocs before | after | opens before | after | stats after |
|---|---:|---:|---:|---:|---:|---:|---:|
| `didChange E` | 0.17 M | 0.17 M | 98 | 99 | 0 | 0 | 0 |
| `diagnostic E` | 4.85 M | 4.86 M | 1,526 | 1,525 | 0 | 0 | 0 |
| `diagnostic L` (`unchanged`) | 3.32 M | 3.34 M | 1,522 | 1,521 | 0 | 0 | 0 |
| `semanticTokens E` | **23.9 M** | **8.26 M** | 7,804 | 6,092 | **80** | **0** | 80 |
| `semanticTokens L` | 14.2 M | 16.2 M | 17,113 | 17,635 | 0 | 0 | 80 |
| `hover L` | 6.42 M | 8.37 M | 7,015 | 7,536 | 0 | 0 | 80 |
| **keystroke total** | **53.0 M** | **41.2 M** | 35,078 | 34,408 | **80** | **0** | **240** |

`semanticTokens E`'s opens fall to 0, as expected, and it falls toward the
cost of parsing and resolving `E` alone: 15.7 M less, of which the kernel's
share of the 80 reads was predicted at about 13 M. The path `stat`s are the
predicted 240: one per unbuffered item in each of the three requests that read
the fold. The cost of validating a cached fold is what `semanticTokens L` and
`hover L` gained: about 1.95 M instructions and 521 allocations for 80 items,
roughly 24 k instructions and 6.5 allocations per item (the `stat`, and the
normalised path the buffer lookup builds). The cold rows move by under 2%:
`semanticTokens L` and `hover L` validate the fold the first request built (80
`stat`s each, about 2.3 M), and the cold `semanticTokens E` still opens 80.

### Stage 3a: the MSBuild evaluation records its read set

**Dependencies**: none. **Implements**: D3, in `borzoi-msbuild`.

Every `std::fs` call in `crates/msbuild/src` (non-test) moves into one
`observed` module. That module performs the call, counts it, and appends a
`(path, Observation)` to the evaluation's `ReadSet`. `ParsedProject` carries
the `ReadSet` and the counts. A crate-local `clippy.toml` bans `std::fs`
outside the module. Nothing consumes the read set yet.

**Oracle**:
- Every msbuild differential and corpus manifest is unchanged: the
  evaluation's results move by zero lines.
- The counts reproduce the one-off `didOpen E` row on WoofWare.Incremental
  (117 opens, 371 `stat`s, 67 listings, 252 `realpath`s), up to the reads the
  LSP makes outside the evaluator, which the stage itemises.
- `cargo clippy` fails on a new direct `std::fs` call in the crate.
- The read-set soundness property of D7, over the crate's project fixtures and
  the `with_imports_tests` trees.

**Expected gain**: none (the stage is infrastructure). The read set's size per
corpus project is reported, so Stage 3b's validation cost is known in advance.

**Implemented** (`crates/msbuild/src/observed.rs`, `crates/msbuild/clippy.toml`):

- Every file-system query in `crates/msbuild/src` is one of six functions in
  `borzoi_msbuild::observed`: `read_to_string`, `kind` (a following `stat`,
  behind `is_file`, `is_dir` and `exists`), `link_kind` (`lstat`),
  `read_dir`, and `canonicalize`. Each performs the `std` call, counts it,
  and records a `Read`: the path and an `Observation`, which is the query
  together with everything its asker could learn from the answer (a
  `TextDigest` of length and a 128-bit hash, an `EntryKind`, a listing's
  names and entry types sorted by name, a canonical path, or a
  `Failure` carrying the `io::Error`'s kind and message, so absence is
  `NotFound`). 46 call sites moved: the evaluator (9), `item_spec` (2),
  `properties` (1), `imports` (3), the glob resolver (4), and SDK
  resolution (`sdk_resolver` 8, `global_json` 1, `workloads` 18).
- **The read set is keyed by query, not by path.** The plan's
  `Observation` was `Text(hash)`, `Listing(names)`, `Absent` or
  `Stamp(stamp)`. One path is often asked several things (canonicalised,
  then read; `stat`ed, then listed), and the answer to one does not
  determine another, so each variant is a query with its answer, and a
  failure of any query is a `Failure`. `ReadSet::first_stale` asks each
  query again of the host and compares answers; that is the validator the
  oracle uses, and Stage 3b replaces "the host" with the `Vfs`. A query
  asked twice with one answer is one read; with two answers (the file
  moved during the evaluation) it is two, and the set never validates.
  The reads are sorted, so the read set is a set: workload manifest
  resolution probes in a `HashMap`'s order, and two equal evaluations
  otherwise compared unequal (found by Codex;
  `the_read_set_does_not_depend_on_the_order_of_its_queries`). Reads compare
  by the path's raw spelling, since `Path` equality ignores a trailing
  separator and `p/child/` is a different query from `p/child` (Codex;
  `a_trailing_separator_is_a_different_query`).
- The query functions are public, so a caller's own resolver can query
  through the observer it is handed.
- **One `Observer` per evaluation, shared by reference.** It lives in the
  walker's `State` and records through `Cell`/`RefCell`: the evaluator is
  single-threaded, and the observer is hidden from everything but the one
  evaluation. `parse_fsproj_with_imports` creates it, so the read set
  covers every pass `walk_with_imports` makes (the deferred
  `Directory.Build.props` pass and the `TargetFramework` carve-out
  included), and `ParsedProject` gains `read_set` and `read_counts`.
  `parse_fsproj` touches no file; its walker asserts that its observer saw
  nothing.
- **The caller's resolvers observe too.** A resolver that reads the disk
  must record into the evaluation's read set, so `SdkResolver` became
  `dyn Fn(&str, &Observer)`, and `GlobRequest` carries `observer`. The
  crate's own resolvers (`resolve_sdk`, `locate_dotnet_sdk`,
  `glob_resolver::resolve`) and its other public readers
  (`detect_implicit_imports`, `find_global_json`) take the observer as an
  argument. In the LSP, `SdkDiscovery::resolve` forwards it. Its own call
  of `find_global_json` (SDK discovery runs before the evaluation) and its
  `Microsoft.NET.Sdk` probe for a dotnet root pass
  `Unobserved::Evaluation.msbuild_observer()`, a fresh observer nobody
  reads, so the LSP's worklist still names them.
- The condition evaluator's `Exists` callback and its permission to run
  `GetDirectoryNameOfFileAbove` were two arguments that had to agree; they
  are now one `condition::Probes { exists, observer }`, and the property
  expander's `fs_probes_allowed: bool` is an `Option<&Observer>`.
- `crates/msbuild/clippy.toml` repeats the workspace's spawn bans verbatim
  (`clippy_config_repeats_the_workspace_bans`) and bans the same `std::fs`
  and `Path` probes as `crates/lsp/clippy.toml`. Test code reaches them
  through `observed::host_fs`, which the integration tests reach through
  the `test-support` feature: about 130 test call sites were rewritten,
  as Stage 2a rewrote the LSP's, rather than allowing the lint (which
  would lift the spawn bans too). No test's assertions changed.
- The restore-cone read census (`evaluator::reads`, stage A₁.3a of
  `docs/nuget-dgspec-boundary-plan.md`) is a different kind of read: which
  properties and item types the restore targets read from the evaluation's
  end state, against MSBuild's. It records no file-system query, so it
  neither duplicates the read set nor offers a second record of it. The two
  meet in one place: the census asserts that a probed walk returns the same
  `ParsedProject` as an unprobed one, and that comparison now includes the
  read set.
- The LSP emits each evaluation's counts and read-set size as an
  `observed.evaluation` event (`observed::report_evaluation`), and
  `edit_counts` prints them as `e.open`, `e.stat`, `e.list`, `e.canon` and
  `e.reads`. `fsproj_msbuild_corpus_diff` prints each corpus project's
  read-set size and counts.

The oracle (`crates/msbuild/src/observed/soundness.rs`):

- `soundness::sweep(root, evaluate, extra)` perturbs the tree under `root`
  one change at a time, restoring each, and asserts that the read set is
  **sound**: if no answer in it moved, evaluating again gives an equal
  `ParsedProject` (read set and counts included). It counts failures of
  **precision**: a change that touches no path the read set names (nor an
  ancestor of one), and adds no entry to a directory it listed, should
  leave every answer standing. Deciding what a change touches means knowing
  every spelling the file system accepts for a file (through a symlink, a
  `..` after one, a case-insensitive volume); after three Codex rounds
  found one more such spelling each, the check compares lexically and only
  the generated worlds, which spell each file one way, assert it. An
  imprecise read set costs a needless re-evaluation, never a wrong one. The
  changes: every file rewritten (an MSBuild file with a property more),
  rewritten at the same size, and deleted; a file created in every
  directory under every name of a vocabulary (the implicit-import names,
  `global.json`, a fresh `.fs` and `.props`, and every name in the tree or
  the read set); every recorded absence created; every listed directory
  grown.
- The vocabulary rule is chosen without regard to what the read set
  recorded. The first version created files only where the read set had
  recorded an absence, and a planted unrecorded `Directory.Build.props`
  probe passed it: a perturbation chosen from the read set cannot find what
  the read set left out.
- It runs over the `with_imports_tests` trees: the helpers every one of
  those tests goes through (`parse`, `parse_with_extras`,
  `parse_with_sdk_resolution`, `parse_file_with_sdk_env`) sweep the test's
  own temporary directory before returning, so 300 tests' trees are swept.
- `with_imports_tests::read_set` adds generated worlds that reach every
  query kind at once: SDK resolution through `resolve_sdk` over a fake
  dotnet root (its version listing and `Sdk.props` probes), the
  `Directory.Build.*` walk, `Exists()`, a conditional and a wildcard
  `<Import>`, `GetDirectoryNameOfFileAbove`, a Compile glob, and
  `@(ProjectReference->Exists())`; plus two perturbations of their own (a
  newer SDK installed, and an `extra.props` beside an installed
  `Sdk.props`). `every_perturbation_rule_moves_some_evaluation` is the
  non-vacuity check: over a fixed sample, each rule moves some evaluation,
  and some perturbation exercises the precision check.
  `a_full_world_records_every_kind_of_query` checks that the read set holds
  a query of each kind, an absence among them.
- The sweep changes only paths that lie under its root lexically and
  physically (a read of `../outside/x` is recorded but not perturbed), and
  does not mistake a dangling symlink for an absent path (Codex found both;
  `the_sweep_writes_only_under_its_root`, `a_dangling_symlink_is_swept`).
- A listing is sorted by name, for the reader as for the read set: tmpfs
  lists in creation order, so a file deleted and written back moved, and an
  evaluation reading the host's order could change with no entry changing
  (Codex; `a_listing_is_sorted_by_name`). Every reader sorted or selected
  by version already, so no result moves.
- After every perturbation the sweep asserts that the tree is back exactly
  as it was (each entry's kind, permissions, bytes and symlink target),
  rather than trusting its undo. Codex's last rounds found the undo
  changing permissions and failing on unreadable files one shape at a time;
  the snapshot catches any such infidelity, and the undo now keeps
  permissions and leaves unreadable files alone
  (`the_sweep_keeps_permissions`).
- Workload manifest resolution probes its manifests in key order: a probe
  that degrades stops the loop, so `HashMap` order decided what the probes
  before it recorded (Codex;
  `what_resolution_observes_does_not_depend_on_hash_order`).
- `fsproj_corpus::fixture_read_sets_are_sound_through_the_real_sdk_chain`
  copies three of the crate's project fixtures into temporary directories
  and sweeps them through the real SDK chain and the glob resolver.

Written first, it was run against an observer that records nothing (the
read set before this stage): 218 of the 301 `with_imports_tests` fail, and
all three `read_set` tests. Planted, each caught, then removed:

| plant | failing `with_imports_tests` (of 304) |
|---|---:|
| the observer records nothing | 285 |
| failed queries (absences) not recorded | 285 |
| `detect_implicit_imports` probes through an unread observer | 227 |
| text digest of length only | 96 |
| `Exists()` probes through an unread observer | 8 |
| listings recorded without their entries | 4 |
| `GetDirectoryNameOfFileAbove` probes through an unread observer | 3 |
| the glob resolver lists through an unread observer | 2 (`read_set`) |
| SDK resolution lists the dotnet root through an unread observer | 2 (`read_set`) |

A direct `std::fs::read_to_string` planted in the evaluator fails
`cargo clippy -p borzoi-msbuild --all-targets --all-features -- -D warnings`.

**Measured** on WoofWare.Incremental, 20 keystrokes per run, the minimum
over six runs of #638's head (de9cb28d) and three of this stage's.

The evaluation the cold `didOpen E` runs observes 115 opens, 326 `stat`s,
63 listings and 252 canonicalisations, and its read set holds 534
distinct reads. The one-off row was 117, 371, 67 and 252. The difference
is the LSP's own reads around the evaluation, which a temporary trace of
the `Unobserved` methods and `borzoi_nuget::settings` itemised exactly:

| reader | opens | stats | listings | canon. |
|---|---:|---:|---:|---:|
| the evaluation (`ParsedProject::read_counts`) | 115 | 326 | 63 | 252 |
| the `.fsproj` text (`Unobserved::Evaluation`) | 1 | | | |
| NuGet settings for the global packages folder: three `nuget.config` spellings in each of 11 ancestors, the user config, its `config` directory and the machine-wide one | 1 | 34 | 3 | |
| `find_global_json`'s walk of 11 ancestors, into an unread observer | | 11 | | |
| ownership's listing of the project directory (`Unobserved::Ownership`) | | | 1 | |
| **total**, the one-off row | **117** | **371** | **67** | **252** |

The cold `semanticTokens E` runs the same evaluation again (115, 326, 63,
252): the env's project-graph walk (`resolve_node_uncached`) evaluates the
entry project afresh rather than reading `Workspace::projects`. That is
not new; the one-off `semanticTokens E` row counted it too.

| message | instructions before | after | allocs before | after |
|---|---:|---:|---:|---:|
| `didOpen E` (cold, one evaluation) | 320.9 M | 328.4 M | 102,897 | 104,615 |
| `semanticTokens E` (cold, one evaluation) | 3,078.8 M | 3,093.7 M | 3,516,099 | 3,517,846 |
| `semanticTokens L` (cold) | 598.7 M | 598.4 M | 377,500 | 377,500 |
| `hover L` (cold) | 123.7 M | 123.8 M | 147,139 | 147,139 |
| keystroke total | 37.12 M | 37.18 M | 28,403 | 28,403 |

Every keystroke row is unchanged within run-to-run noise, allocations
exactly, and no keystroke runs an evaluation. Recording costs each
evaluation about 7.5 M instructions and 1,700 allocations, 2.3% of the
evaluation: one path per call (756 calls) and the listings' names, kept
until the evaluation ends, then sorted and de-duplicated into the read set.
The cold rows that read files are kernel-heavy; both binaries' runs agreed
to within 2%.

Read-set sizes over the 72 projects of the MSBuild corpus differential
(`fsproj_msbuild_corpus_diff`, which prints them), which bound Stage 3b's
validation cost:

| per evaluation | min | median | max |
|---|---:|---:|---:|
| distinct reads | 402 | 523 | 606 |
| opens | 98 | 125 | 202 |
| `stat`s | 344 | 394 | 703 |
| listings | 64 | 65 | 128 |
| canonicalisations | 220 | 268 | 454 |

Most of each set is the SDK chain, whose file contents D3 exempts from
re-validation; its listings (about 65) are validated in full. The corpus
differential and `sdk_chain_decline_attribution` are unchanged: the
manifest moved by zero lines and there are no divergences.

### Stage 3b: evaluations are validated, not trusted

**Dependencies**: Stages 2b and 3a. **Implements**: D3 in the LSP (item 2).

`Workspace::projects` entries carry their read set. A lookup in a new
generation validates every entry except the immutable-root contents (D3)
against the `Vfs`, and re-evaluates on any mismatch. The structural watch
event still clears everything (D3's immutable-roots fallback).

**Oracle**:
- `WITNESSED += [ProjectFiles]`. Generated scripts that write `.fsproj` and
  `common.props` and never flush must agree with a fresh session. The pinned
  script is the shape of `watched_fsproj_change_refreshes_stale_defines`, run
  without its event, and fails on `main`.
- A pinned script installs a new version of the world's stub framework pack
  (and, separately, an SDK version that `global.json` then selects) without an
  event. The next `Check` must agree with a fresh session, which proves that
  listings under the immutable roots are still validated.
- The read-set soundness property, now through the LSP.

**Expected gain**: a correctness fix. Its cost is the validation `stat`s:
the user-owned read set plus the listed directories under the immutable
roots, per generation that reads an evaluation. That is expected to be a few
dozen, and the counts report it.

**Implemented** (`crates/lsp/src/workspace.rs`, `crates/lsp/src/read_checks.rs`,
`crates/lsp/src/vfs.rs`, `crates/lsp/src/server.rs`):

- **What an entry depends on.** `evaluate_project` returns the evaluation with
  its `Evaluation::reads`: the `.fsproj` read and SDK discovery's reads, which
  now record into an observer of their own (`SdkDiscovery::for_project` takes
  one, and so does `resolve_dotnet_root`), unioned (`ReadSet::union`) with the
  read set of every pass it ran: pass 1 and the seeded pass 2, whether or
  not pass 2 is served (a pass discarded for a `TreatAsLocalProperty`
  opt-out decided that it was; found by Codex). A failed pass fails on the source text and the caller's arguments
  alone (`ParseError`), so its reads are not needed. `Unobserved::Evaluation`
  is gone: the `.fsproj` buffer diagnostics' probes, which nothing caches,
  became `Unobserved::FsprojDiagnostics`, and the env's fallback probe for a
  dotnet root `Unobserved::EnvResolution`.
- **Validated per generation, lazily.** `Workspace::begin_generation` and
  `end_generation` run beside the `Vfs`'s (`State::begin_generation`), and the
  first lookup of a cached evaluation in a generation confirms its reads
  (`Workspace::validate`); later lookups in the generation trust it. An entry
  any of whose answers moved is dropped and evaluated afresh, and the drop
  ends the `projects_generation`. Outside a generation a cached evaluation is
  served as it is, which keeps unit tests that drive the workspace directly on
  their old semantics. The deferral refresh after every message reads without
  validating (`project_evaluation_as_last_confirmed`), as Stage 2b's
  `fold_outcome` does, so a `didChange` or a refresh reply pays nothing.
- **Downstream caches.** A dropped evaluation moves the
  `projects_generation`; `SemanticState` records the one its caches were
  computed in and drops them all, as a structural watch event does, when it
  moves (`sync_with_evaluations`, at the top of `validate_cached_fold`, which
  first validates the project at hand, and of `cached_assembly_env`). The
  dispatch loop then does what a structural event does for the client
  (`respond_to_moved_evaluations`): republishes every open buffer to a push
  client, owes a pull client a diagnostic refresh and the semantic-token
  refresh, and recomputes the deferral scope. The sweep's existing restart on
  a new `projects_generation` covers it too.
- **Immutable contents** (D3). Reads strictly inside a version directory are
  not confirmed: `<dotnet>/sdk/<v>/`, `<dotnet>/packs/<pack>/<v>/`,
  `<dotnet>/sdk-manifests/<band>/<manifest>/<v>/`, and `<id>/<v>/` under the
  NuGet folder, for every dotnet root SDK discovery consulted or the SDK
  resolved from, and the folder discovery names. A version directory reached
  through a symlink (Nix's combined installs are symlink farms) is matched in
  its canonical spelling too, taken from a canonicalisation of the version
  directory itself in the read set, which is confirmed. (A canonicalisation
  of a path below a version says nothing about the version: a symlink there
  can lead into the user's tree. Found by Codex.) Only a version some read showed to exist (a
  read inside it got an answer) is trusted, and the entry then confirms that
  it still does; a read inside a version no read showed to exist (an
  `Exists` probe of one not installed) is confirmed like any other
  (`Evaluation::checks`, found by Codex). A NuGet package is extracted in
  place, and is complete only once NuGet writes its `.nupkg.metadata`, so a
  package version is trusted only when that file was written before the
  evaluation began, outside the racy window (`nuget_committed_before`;
  Codex: an evaluation racing an extraction would otherwise keep a half
  package). An SDK, pack or workload manifest installed *while* an
  evaluation reads it has no such marker; that race is the axiom's
  boundary, as an in-place rewrite of a version is, and a watch event or a
  restart covers it.
- **Confirming a read costs a `stat`** (`read_checks`). A whole-file read is
  confirmed through a `vfs::DiskCache` (factored out of the `Vfs`, which keeps
  one for sources), by the file's stamp. Any other read, the first time it is
  confirmed, is asked again behind a *witness*: the stamps of the directories
  whose entries decide its answer, taken before the question. While they hold
  (and none is racy), so does the answer, and each directory is `stat`ed once
  per generation however many reads share it. A listing's witness is the
  directory listed; what a path names (following symlinks or not), whether
  it is there, and its canonical spelling are witnessed by every directory
  its resolution looks an entry up in, walked as the kernel walks it: one
  resolver, which takes a link's target component by component and each
  `..` from the directory reached, splitting a path on separators as the
  kernel does: a `.` is looked up too, and with each `.` and `..` the
  directory it is looked up in is witnessed (its search permission decides
  the lookup, and a `chmod` moves its stamp). An `lstat` of a path ending
  in a separator follows a final symlink, so such a spelling has no
  `LinkKind` witness. Only a *fact* (an answer, or an absence;
  `read_checks::a_fact`) has a witness or is trusted inside an installed
  version: a read that failed otherwise (a permission, an I/O error, a
  listing that failed at an entry) can recover with nothing on disk
  changing, and is asked in every generation. (Codex found it twice, for a
  partial listing's witness and for a failure inside a version; the two
  became one predicate.) Codex found two spellings a narrower witness missed
  (`Exists('link/')`, and a link whose target ends with a separator, to
  another link); the narrower witnesses went, and a property on the host's
  file system now generates trees with symlinks of every such spelling.
  Codex's next two findings, a `..` and a trailing `.` whose directory lost
  its search permission, were in the same walk: it split paths with
  `Path::components`, which drops a `.`. It now splits on separators, and
  the host property takes search permissions away and gives them back.
- **The graph walk reads the confirmed entry.** The env's project-graph walk
  evaluated the entry project afresh, a second evaluation on every cold env
  build. It now reads the cached evaluation when this generation has
  confirmed it (`project_graph_impl`), which is the same computation.
- `Generation` gains `EvaluationCounts`: evaluations run, and the
  validation's opens, `stat`s, listings and canonicalisations, which
  `edit_counts` prints as `v.open`, `v.stat`, `v.list` and `v.canon`.

**Deviations.**

- **The plan validated "against the `Vfs`".** The witnesses and the file
  stamps live in a `ReadChecks` the `Workspace` owns, which reuses the `Vfs`'s
  stamping (`DiskCache`, `Stamp`, the racy rule) and begins and ends with it,
  logging into the same `Generation`. Reaching the `State`'s `Vfs` from every
  `Workspace` lookup would have meant threading it through about 200 call
  sites.
- **Immutable contents exempt every read strictly inside a version**, not
  only texts and stamps as D3 wrote: a listing or an absence inside an
  installed version is a fact about that version, which never changes in
  place. The reads D3's argument is about (the listing of the directory
  holding the versions, and a version directory's own existence) do not lie
  strictly inside one, and are confirmed. So are a `sdk-manifests` band and
  manifest directory, which a workload update writes into.
- **D3 confirms listings by the directory's stamp; this stage extends the
  witness** to every query, because asking the evaluation's `realpath`s and
  listings again cost 16 M instructions per generation on
  WoofWare.Incremental.
- **Split off: a `.fsproj` buffer's diagnostics.** They evaluate the buffer,
  not the cached evaluation, and nothing confirms what they read, so an
  unwatched write to an input of an open `.fsproj` buffer leaves the
  diagnostics the client shows for it stale (found by the generated
  property). The state machine names it `InputClass::FsprojBufferInputs`,
  unwitnessed, as `CSharpSources` is; closing it means recording the buffer
  evaluation's reads (and its graph walk's) per open buffer and confirming
  them in every generation.
- **`DirectoryMembership` stays unwitnessed** until Stage 4b. The state
  machine now separates it from `GlobMembership` (a source created or deleted
  where a project's `*.fs` glob reads, which the evaluation's listing
  witnesses): a `.fsproj` created or deleted moves ownership, which every
  request recomputes, but nothing owes the client a refresh of what it shows.
- **A gap left open: NuGet settings.** SDK discovery chooses the global
  packages folder through `borzoi_nuget::settings`, which reads
  `nuget.config` files directly; those reads are not in the read set, so a
  `nuget.config` edit that moves the folder reaches a cached evaluation only
  through a watch event. Closing it needs `borzoi-nuget` to report what it
  read.

The oracle:

- `incremental_state_machine`: `WITNESSED` gains `ProjectFiles` and
  `GlobMembership`. `unwatched_project_writes_agree_with_a_fresh_session`
  generates scripts that write and delete `.fsproj` files, `common.props`
  and sources and never flush, opening only sources, and asserts that
  comparisons with a project write pending happened
  (`unflushed_project_checks`: 96 at 96 cases). Pinned no-flush scripts: the
  two existing define changes (pull and push), now compared before their
  final flush; an SDK version installed beside the one a project resolves; a
  `global.json` rewritten to select another installed SDK; a source created
  where a project globs; an import created that a project recorded absent.
  All six fail on `main` (435ccaec), as does the generated property, and
  pass here. A stub SDK (`ProjectSpec::sdk`, `Cmd::Install`) is new to the
  world for them.
- `workspace::tests::validation`: the read-set soundness sweep through the
  LSP (`the_workspace_read_set_is_sound`): `soundness::sweep` over a world with
  a stub SDK found through `$DOTNET_ROOT` and, separately, through `$PATH`, a
  `Directory.Build.props`, a conditional import and a glob, whose read set is
  the workspace's (preparation reads and every pass); a newer SDK installed
  and a `global.json` pin each move the evaluation, as the non-vacuity
  check. A second sweep holds what a cached evaluation *confirms*, its checks
  after the immutable-content exemption, to the same property, over a
  project tree outside the installs (where the axiom holds): the SDK
  project, and a multi-targeted one whose seeded pass is discarded and which
  probes an absent import (`the_workspace_checks_are_sound_outside_the_installs`). Beside it: the immutable-content table, a moved project file noticed
  in the next generation and trusted outside one, an SDK version trusted
  while its listing is confirmed, a version installed after the evaluation,
  a file only the seeded pass reads, a discarded pass's reads, and the graph
  walk reading only a confirmed entry.
- `read_checks::tests`: a model file system (files, directories, absolute
  symlinks, renames, a directory moved and linked from where it was) whose
  timestamps tick at 1 ns, 1 ms, 1 s or 2 s, and a generated clock. For all
  generated changes, a check that holds is right: its query gets the
  recorded answer as of its generation (a change made during a generation
  may be seen or not within it, never after it). Its non-vacuity check counts
  generations confirmed by witnesses alone and checks that found the disk
  moved; a step-by-step test walks a same-tick change past a racy stamp.
  Beside the model, `a_check_that_holds_is_right_on_the_host` generates
  trees on the host's file system with symlinks whose targets are relative,
  absolute, end with a separator or hold a `..`, and asks every query at
  plain and irregular spellings, pausing after each change for longer than a
  coarse filesystem tick (it runs ahead of the files' clock, so that the
  witnesses are used).
- `observed_counts` gains evaluation counts: no request after the project
  opens evaluates it (the cold env build reads the confirmed evaluation),
  confirming it opens nothing and `stat`s something, and on a racy clock it
  re-reads the project file.

Planted, each caught, then removed:

| plant | caught by |
|---|---|
| no validation (the cache trusted, as on `main`) | six pinned scripts, two `validation` tests, both `observed_counts` properties |
| the semantic caches not dropped on a moved generation | the glob pinned script |
| no response to the client (republish, refreshes) | five pinned scripts |
| everything under a dotnet root immutable | the SDK-install pinned script, two `validation` tests |
| SDK discovery's reads not recorded | the `global.json` pinned script, the LSP soundness sweep |
| the seeded pass's reads dropped | `a_file_only_the_seeded_pass_reads_is_confirmed` |
| the graph walk reading an unconfirmed entry | `the_graph_walk_reads_only_a_confirmed_entry` |
| witnesses trusted while racy | all three `read_checks` tests |
| a `Kind` witness that does not follow a symlink chain | the model property and its non-vacuity check |
| a canonical spelling witnessed by its link chain only | the model property, once the model could move a directory and link it back |
| the version directory itself immutable, not only its contents | the immutable-content table only: the holder's listing still sees an install |
| a discarded seeded pass's reads dropped (Codex) | `a_discarded_pass_reads_are_confirmed`, the checks sweep |
| reads inside a version trusted before any showed it exists (Codex) | `a_version_installed_after_the_evaluation_is_seen` |
| a version's alias inferred from a canonicalisation below it (Codex) | `only_a_version_directory_canonicalised_gives_an_alias` |
| a witness for a trailing-separator spelling (Codex) | `a_spelling_the_stamps_cannot_follow_is_asked_every_time` |
| a `Kind` witnessed by its link chain's parents, not its resolution (Codex) | the spelling test, the model property and its non-vacuity check |
| the directory a `..` leaves not stamped (Codex) | `a_directory_a_dot_is_looked_up_in_is_witnessed` |
| a `.` dropped from the walk (Codex) | `a_directory_a_dot_is_looked_up_in_is_witnessed` |
| a package version trusted without its commit marker (Codex) | `a_package_is_trusted_only_once_committed_before_the_evaluation` |
| a listing that failed at an entry witnessed by its directory (Codex) | `a_listing_with_a_failed_entry_is_asked_every_time` |
| a failed read inside an installed version trusted as immutable (Codex) | `a_failed_read_inside_a_version_is_confirmed` |

Three plants first passed every test: the canonical-spelling witness (the
model could not move a directory), the seeded pass's reads and the graph walk
(nothing evaluated a multi-targeted project or walked before confirming). The
tests named against them were added, and each then failed.

**Measured** on WoofWare.Incremental, 20 keystrokes per run, each row the
minimum over three runs of each binary: #643's head (d4d935f4), `main` at
8353c41f (Stage 3a squash-merged, with sema's Stages 6c and 7 and #649),
and this stage at its final head (7ef02432). Every keystroke row's runs
agreed to within 0.3%; the cold `diagnostic E` is small and noisy.

| message | instructions d4d935f4 | `main` | 3b | allocs `main` | 3b | v.open | v.stat |
|---|---:|---:|---:|---:|---:|---:|---:|
| `didOpen E` (cold, evaluates) | 329.9 M | 341.7 M | 371.3 M | 108,684 | 122,900 | 0 | 0 |
| `didOpen L` (cold, first confirmation) | 1.9 M | 1.8 M | 76.5 M | 798 | 15,571 | 24 | 2,793 |
| `semanticTokens E` (cold) | 3,103.7 M | 3,126.5 M | **2,777.8 M** | 3,521,178 | 3,413,688 | 0 | 159 |
| `semanticTokens L` (cold) | 595.6 M | 592.2 M | 595.8 M | 360,670 | 360,676 | 0 | 159 |
| `hover L` (cold) | 122.6 M | 94.1 M | 96.7 M | 80,962 | 80,968 | 0 | 159 |
| `didChange E` | 0.17 M | 0.17 M | 0.16 M | 99 | 99 | 0 | 0 |
| `diagnostic E` | 4.86 M | 4.90 M | 8.10 M | 1,525 | 1,531 | 0 | 159 |
| `diagnostic L` (`unchanged`) | 3.34 M | 3.36 M | 6.56 M | 1,521 | 1,527 | 0 | 159 |
| `semanticTokens E` | 7.88 M | 7.97 M | 11.15 M | 5,355 | 5,361 | 0 | 159 |
| `semanticTokens L` | 12.44 M | 12.53 M | 15.75 M | 12,183 | 12,189 | 0 | 159 |
| `hover L` | 8.35 M | 7.34 M | 10.52 M | 5,066 | 5,072 | 0 | 159 |
| **keystroke total** | **37.1 M** | **36.3 M** | **52.3 M** | 25,749 | 25,779 | **0** | **795** |

No keystroke runs an evaluation, and every source-read column is unchanged
(80 `stat`s on each of the three fold-reading rows, no opens). Each message
that reads the evaluation confirms it once: 159 `stat`s and nothing else
(no opens, listings or canonicalisations), about 3.2 M instructions and 6
allocations. Five of a keystroke's six messages read it (`didChange` from a
pull client does not), so a keystroke costs **16.0 M instructions (+44%)**
and 30 allocations more than on `main`, and 795 `stat`s. The plan expected
a few dozen `stat`s per message. The read set's part outside installed
versions is larger than that: mostly the SDK chain's workload and pack
enumeration (the directories holding versions, which an install changes)
and the absence probes up eleven ancestors, and each witness reaches every
directory its path's resolution looks an entry up in. The devshell's .NET
is a Nix symlink farm, which doubles some of them.

What the witnesses bought, measured on the way: asking every read outside
the installed versions again cost 16.6 M instructions per message (138
`stat`s, 37 listings and 31 `realpath`s); confirming them by stamps, 3.2 M.
Without the immutable-content exemption it was about 60 M.

The cold rows: the first confirmation (`didOpen L`, here) reads each
confirmed file once and takes every witness (an `lstat` per component of
each path), about 75 M once per evaluation. The cold `semanticTokens E`
falls by 349 M: the env's graph walk reads the confirmed evaluation instead
of evaluating the entry a second time. The evaluation itself (`didOpen E`)
costs 30 M and 14,200 allocations more, recording the preparation reads
and building each entry's checks from the union of its read sets.

If the 16 M matters, the remedy D3 names, confirming once per revision and
once per bounded time window, would take a keystroke to one confirmation;
it is a staleness bound the guarantee would have to state, and is not
adopted here.

### Stage 4a: ownership listings are observed and counted

**Dependencies**: Stage 2a. **Implements**: D3's `observed` module for the
ownership walk.

`all_fsproj_in` and `find_owning_project`'s listings move into an `observed`
module and are counted per generation. Their worklist allows go. Behaviour is
unchanged.

**Oracle**:
- Every ownership test is unchanged.
- A count test pins today's walks: a pull of an unchanged file one directory
  below its project performs four listings of two directories (eight).
- `edit_counts` prints 13 listings per keystroke on WoofWare.Incremental,
  reproducing the one-off count.

**Expected gain**: none (the stage is infrastructure).

**Implemented** (`crates/lsp/src/workspace.rs`, `crates/lsp/src/observed.rs`,
`crates/lsp/src/vfs.rs`):

- `Workspace::owning_project` walks with a `borzoi_msbuild::observed::Observer`
  of its own, and `all_fsproj_in` and `find_owning_project` list each
  directory through it (`observed::read_dir`). That is the module the
  evaluations already observe through, so the walk's reads are the same
  `Read`s, with `Listing` observations, that `read_checks` confirms: Stage
  4b's memo needs nothing new to validate them. The walk's counts go into a
  per-generation `Cell` on the `Workspace`, beside its count of evaluations.
  `Unobserved::Ownership` is gone.
- `Generation` gains `ownership` (a `borzoi_msbuild::observed::ReadCounts`),
  `Workspace::end_generation` returns both the evaluation and the ownership
  counts (`WorkspaceCounts`), and the `observed.generation` event carries
  them as `o_listings` and `o_stats`. `edit_counts` prints them as `o.list`
  and `o.stat`. Nothing `stat`s yet, so `o.stat` is 0 until Stage 4b.
- `find_owning_project` takes the observer to list through. It was public
  for the tests alone, which now hand it one.

The oracle:

- Every ownership test's assertions are unchanged; the calls of
  `find_owning_project` pass an observer.
- `observed_counts` pins today's walks for a pull client. A pull of a file
  one directory below its project that its text answers `unchanged` lists
  **four** times: it walks twice, for the file's symbols and its language
  version (which the result id hashes), and each walk lists the file's
  directory and its project's, where the project lists the file and the
  walk stops. After a keystroke, the `full` report walks twice more to
  compute the diagnostics: **eight**. In general a file `d` directories
  below lists `2(d + 1)` and `4(d + 1)` times. Written first against a walk
  that counted nothing, it failed (`0 ≠ 4`).
- `workspace::tests`: the walk lists the directories from the file's up to
  the listing project's, each once, as `Listing` reads in its observer; a
  file no project lists climbs to the file system's root and then lists
  the directories up to the nearest project again, every listing observed;
  and the counts are per generation.
- `edit_counts` prints **13** listings per keystroke on
  WoofWare.Incremental, reproducing the one-off count. `E` (`Stack.fs`) lies
  in the project's directory and `L` (`Skeleton/Skeleton.fs`) one below it:
  `diagnostic E` is a `full` report (four walks of one directory),
  `diagnostic L` an `unchanged` one (two walks of two), and
  `semanticTokens E`, `semanticTokens L` and `hover L` walk once each (one,
  two and two listings).

**Departure.** The plan's count test said an *unchanged* pull one directory
down performs "four listings of two directories (eight)". Eight is the
`full` pull's count; an unchanged pull walks twice and lists four times.
Both are pinned.

Planted, each caught, then removed:

| plant | caught by |
|---|---|
| the walk's counts dropped | both `observed_counts` ownership tests |
| the counts not reset when a generation begins | `the_ownership_walks_are_counted_per_generation`, both `observed_counts` ownership tests |
| a direct `std::fs::read_dir` in `all_fsproj_in` | `cargo clippy` (`disallowed_methods`) |
| the fallback walk listing through an observer of its own | `the_ownership_walks_fallback_lists_through_the_observer_too` |

**Measured** on WoofWare.Incremental, 20 keystrokes per run, each row the
minimum over three runs of each binary: #660's head (2eb1b108, Stage 3b
before its squash; `main` at 3d1e038a adds #657's change to the MSBuild
evaluator, which no keystroke runs) and this stage. Every
other column is unchanged (80 `stat`s on each fold-reading row, 137
validation `stat`s on each row that reads the evaluation).

| message, per keystroke | instructions 3b | 4a | allocs 3b | 4a | o.list |
|---|---:|---:|---:|---:|---:|
| `didChange E` | 0.14 M | 0.17 M | 99 | 99 | 0 |
| `diagnostic E` | 7.08 M | 7.62 M | 1,531 | 2,227 | 4 |
| `diagnostic L` (`unchanged`) | 5.58 M | 5.89 M | 1,519 | 1,891 | 4 |
| `semanticTokens E` | 9.88 M | 10.01 M | 5,357 | 5,531 | 1 |
| `semanticTokens L` | 13.87 M | 13.96 M | 12,177 | 12,363 | 2 |
| `hover L` | 9.23 M | 9.40 M | 5,060 | 5,246 | 2 |
| **keystroke total** | **45.81 M** | **47.32 M** | 25,743 | 27,357 | **13** |

The 13 listings cost 1.5 M instructions (+3.3%) and 1,614 allocations per
keystroke more than the uncounted ones did. Nine of them list the project's
directory, which holds 82 entries, and four `Skeleton/`, which holds four.
An observed listing records every entry's name and kind and sorts them,
where the walk read only each entry's path. Stage 4b removes the
listings from the keystroke. The `didChange E` row reads no file and moves
within its runs' spread (0.14–0.19 M across both binaries).

### Stage 4b: ownership is memoised

**Dependencies**: Stages 3b and 4a. **Implements**: D4 (item 4).

`owning_project` memoises its answer per file. The key is the stamped
ancestor listings and the evaluations consulted, validated per generation.
`symbols_for` and `lang_version_for` share one lookup, so `diagnose_file`'s
four walks become one memo hit.

**Oracle**:
- Stage 4a's count test flips to 0 listings, with one directory `stat` per
  ancestor the memo depends on.
- A property test: over generated directory trees (projects at random depths,
  ties, partial evaluations, files added and removed), the memoised answer
  equals `owning_project`'s uncached walk, which is kept as a test-only
  reference. The existing ownership tests are unchanged.
- `WITNESSED += [DirectoryMembership]`: a `.fsproj` created beside an open
  file without an event.

**Expected gain**: per keystroke, 13 listings become 0. The `unchanged`
diagnostic pull falls from 3.35 M toward the cost of hashing the text twice
(about 0.2 M on `L`).

**Implemented** (`crates/lsp/src/workspace.rs`, `crates/lsp/src/read_checks.rs`):

- **The memo.** `Workspace::owners` holds, per file spelling, the owner a
  walk inside a generation found, the directories it listed and the
  evaluations whose membership verdicts it consulted, each by an identity:
  `ListingId` for a listing, and `EvaluationId`, which every
  `CachedEvaluation` gets when it is cached. The first ask in a later
  generation confirms the answer: every directory's listing is still the
  one the walk read and still holds (`ReadChecks::holds`, Stage 3b's
  witnesses: the directory's stamp), and every consulted evaluation, once
  `Workspace::validate` has confirmed it for this generation, is still the
  entry with that identity. Later asks in the generation are hits. An
  answer that does not stand is walked again. Validation is exact, with no
  staleness window: Stage 3b's racy rule applies to every witness.
- **One listing per directory.** The listings live in one table
  (`DirListings`), one per directory, confirmed at most once per
  generation and shared by every owner whose walk read it; a walk reads a
  directory's listing from it when it still holds. An owner holding copies
  of its listings would keep, after a sweep over a directory of `N` files,
  `N` copies of a listing of `N` entries (Codex measured 257 MB for 2,000
  files).
- **Born witnesses.** `ReadChecks::list` stamps a directory, then lists it,
  and returns the check with that stamp as its witness when the listing is
  an answer and a fact. So the generation after a walk confirms it by the
  `stat`s alone; a check built from an observer's read set would have
  listed once more, to take its witness.
- **Its own checker.** The listings are made and confirmed through a
  `ReadChecks` of their own, not the one confirming evaluations, so its
  counts are the generation's ownership counts (`o.list`, `o.stat`). The price is that
  a directory both confirm (the project's) is `stat`ed twice per
  generation.
- **An owner that moves.** A walk that finds another owner than the answer
  it replaces ends the `projects_generation`, and the loop does what it
  does for a moved evaluation (`respond_to_moved_evaluations`): it
  republishes the open buffers to a push client, owes a pull client the
  diagnostic and semantic-token refreshes, drops the semantic caches and
  recomputes the deferral scope. The request itself was answered from the
  fresh walk; what this closes is what the client is shown, which Stage 3b
  named as `DirectoryMembership`'s gap.
- **Outside a generation** nothing confirms an answer, so none is
  remembered: every ask walks, as before. A structural watch event
  (`invalidate_projects`) forgets every owner with every evaluation, so a
  watch event still covers a file system whose stamps do not move (D1's
  "not covered").

**Departures.**

- `symbols_for` and `lang_version_for` do not share one lookup. Each asks
  the memo; the first ask in a generation confirms the answer and the rest
  are hash lookups, which the counts below show cost nothing measurable.
- The property test's reference is a fresh `Workspace`'s walk, outside any
  generation, rather than a test-only copy of the walk: the production walk
  is the reference, and the property is the memo against it.
- A `stat` per directory is the confirmation's whole file-system cost, as
  planned. The keystroke falls by more than the listings cost, because a
  walk also asks each candidate project whether it lists the file, which
  scans its Compile items; a hit skips that too.

The oracle:

- `observed_counts` flips: a pull of a file `d` directories below its
  project, `unchanged` or `full`, lists nothing and `stat`s each of the
  `d + 1` directories the walk read, once. The control on a racy clock
  lists and `stat`s each once: no witness is trusted.
- `workspace::tests::ownership_memo`: for all generated trees (projects in
  three nested directories, two names per directory so that they tie,
  broken projects whose membership is unknown, projects listing files
  above, beside and below them, sources created and deleted beside them)
  and the changes made between generations, the answer of a long-lived
  workspace inside a generation equals a fresh workspace's walk. Its
  non-vacuity check counts asks answered by confirming a remembered
  answer, remembered answers that no longer stood, and owners that moved.
  Beside it: a walk lists each directory once and records what it
  consulted; a generation after the walk confirms by one `stat` per
  directory however often it asks; the owners of ten files in one
  directory share its listing (one listing of each directory, one kept);
  outside a generation every ask walks.
- `read_checks`: the model property holds every listing `ReadChecks::list`
  makes to the same rule as every other check, in each later generation,
  with a non-vacuity count of listings confirmed by their born witness and
  of listings found moved. The model gains a change that lands at a
  `stat`, just before its stamp.
- `incremental_state_machine`: `WITNESSED` gains `DirectoryMembership`.
  `unwatched_project_creations_agree_with_a_fresh_session` generates
  scripts that create, rewrite and delete projects (most often `O`, which
  sorts before `P` in `P`'s directory and takes over whichever of its
  sources it lists) and never flush; its non-vacuity check asks for
  comparisons with a creation or deletion pending (36 at its default 16
  cases). `unwatched_project_writes_agree_with_a_fresh_session` now
  compares with those pending too. Four pinned no-flush scripts: a project
  created beside an open file that takes it over (push), the same created
  and deleted again (pull), a project created above an orphan file that the
  fallback hands it to, and a sibling project rewritten in place to list an
  open file (no directory's listing moves; only the consulted evaluation
  does). Against Stage 4a, the first three pinned scripts fail (the client
  is shown diagnostics computed under the old owner), and so does the new
  generated test; the fourth passes there, since every request walks, and
  is for the memo's consulted evaluations.

Planted, each caught, then removed:

| plant | caught by |
|---|---|
| a remembered owner never confirmed again | both `ownership_memo` tests, `an_owner_is_walked_once_and_confirmed_by_stamps`, both `observed_counts` properties, four pinned scripts, the generated creation test |
| no `projects_generation` move when an owner moves | three pinned scripts, the generated creation test |
| consulted evaluations not confirmed | both `ownership_memo` tests, the sibling-rewrite pinned script, the generated creation test |
| owners remembered and served outside a generation | `outside_a_generation_every_ask_walks` (added for it: the plant first passed every test) |
| a shared listing never confirmed again | both `ownership_memo` tests, `an_owner_is_walked_once_and_confirmed_by_stamps`, both `observed_counts` properties, three pinned scripts, the generated creation test |

One plant is not a defect: `ReadChecks::list` asking before it stamps.
A change landing between the listing and the stamp carries a time after
the generation's, so the stamp is racy and the next generation lists
again. No test can tell the two orders apart, and the model's change at a
`stat` confirms it; the stamp comes first anyway, as it does everywhere
else in `read_checks`.

**Measured** on WoofWare.Incremental, 20 keystrokes per run, each row the
minimum over three runs of each binary: Stage 3b (2eb1b108), Stage 4a
(33c211b9) and this stage. Every source-read and validation column is unchanged (80
`stat`s on each fold-reading row, 137 validation `stat`s on each row that
reads the evaluation).

| message, per keystroke | instructions 3b | 4a | 4b | allocs 3b | 4a | 4b | o.list 4a | 4b | o.stat 4b |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `didChange E` | 0.14 M | 0.17 M | 0.18 M | 99 | 99 | 99 | 0 | 0 | 0 |
| `diagnostic E` | 7.08 M | 7.62 M | **3.63 M** | 1,531 | 2,227 | 238 | 4 | 0 | 1 |
| `diagnostic L` (`unchanged`) | 5.58 M | 5.89 M | **2.68 M** | 1,519 | 1,891 | 84 | 4 | 0 | 2 |
| `semanticTokens E` | 9.88 M | 10.01 M | 9.01 M | 5,357 | 5,531 | 5,036 | 1 | 0 | 1 |
| `semanticTokens L` | 13.87 M | 13.96 M | 12.40 M | 12,177 | 12,363 | 11,461 | 2 | 0 | 2 |
| `hover L` | 9.23 M | 9.40 M | 7.83 M | 5,060 | 5,246 | 4,344 | 2 | 0 | 2 |
| **keystroke total** | **45.81 M** | **47.32 M** | **35.81 M** | 25,743 | 27,357 | 21,262 | **13** | **0** | **8** |

The 13 listings become 0, and each message that asks for an owner
confirms it with one `stat` of each directory the walk read: 8 per
keystroke. The keystroke falls by 10.0 M instructions (22%) and 4,481
allocations against Stage 3b, and by 11.5 M against Stage 4a's counted
listings. That is more than the listings cost: each of the 13 walks also
asked the project whether it lists the file, a scan of its 82 Compile
items that normalises each path, and a hit asks nothing. The `unchanged`
pull of `L` falls to 2.68 M, not to the plan's 0.2 M: the plan predates
Stage 3b, whose confirmation of the evaluation (137 `stat`s, about 2.4 M
here) every message that reads the evaluation pays. The cold rows that
walk for the first time cost one `stat` per directory more than Stage
4a's (the stamp a walk takes before it lists), and `didOpen L` lists only
`Skeleton/`, reading the project's directory from the listing `didOpen E`
made; the cold `diagnostic` rows fall by 4 M and 7 M for the same reason
as the keystroke's. `didChange E` reads no file and moves within its
runs' spread (0.13–0.19 M across the three binaries).

### Stage 5: one parse per text

**Dependencies**: Stage 2b. **Implements**: D2 for parses, which diagnostics
and the fold share.

The per-file parse memo becomes the only place a source is parsed. Syntactic
diagnostics read the memoised parse (the parse plus its diagnostics, recorded
together) instead of parsing again.

**Oracle**:
- The diagnostics differential and both diagnostic models in
  `incremental_state_machine` are unchanged.
- Memo ≡ recompute over generated text, symbol and version sequences.

**Expected gain**: `diagnostic E` from 4.85 M to near the floor. One parse of
`E` (under 5 M) leaves the keystroke.

**Implemented** (`crates/lsp/src/parses.rs`, `crates/lsp/src/diagnostics.rs`,
`crates/lsp/src/semantic.rs`, `crates/cst/src/tokens.rs`):

- **The memo.** `parses::ParseMemo`, on `SemanticState`, holds each file's
  latest parse under each set of inputs: the text, the grammar
  (implementation or signature), the `#if` symbols and the language
  version. `ParseMemo::parse` hands back the parse whose inputs all match,
  the text compared by pointer and then by bytes, and otherwise parses
  (`SourceParse::new`, panic-safe) and replaces the parse under the same
  inputs. Files are keyed as the `Vfs` keys its buffers (`path_dedup_key`
  of the lexically normalised path), so a buffer and the Compile item that
  names it share their parses. A buffer that names no file (`untitled:`) is
  parsed and not remembered.
- **Every reader parses through it.** The syntactic diagnostics
  (`server::grouped_for_uri`, for a push and a pull client alike), the
  fold (`build_parses`, on a miss in its per-file cache), and every
  single-file handler (hover, definition, references, document and
  workspace symbols, semantic tokens) ask the memo, the handlers through
  `State::parse_buffer`. So in the keystroke script the pull of `E`'s
  diagnostics parses `E` and the fold the next request builds reads that
  parse.
- **The fold's own cache** (`SemanticState::file_parses`) keeps what the
  fold derives from a parse: the cast tree, the `SyntaxRecovery`, and the
  verdict of the version-boundary gate. Its entries hold the memo's
  `SourceParse` and are keyed by its inputs plus the project's
  `LangVersion` provenance trust, as before, since a hit skips the gate.
- **One lex.** A parse records what the preprocessor reported during the
  lex it made (`Parse::preprocessor`, a `directives::Preprocessed`): every
  error in stream order, lexer errors in active code included, and the
  active-branch `#line` directives. The token table's writer collects them
  as the driver writes its items, and `lex_full_trivia_into` hands back the
  driver's `LineDirectiveStore`. `diagnostics::syntactic_diagnostics` reads
  the preprocessor's diagnostics, the overlap spans the parser's are
  deduplicated against, and the line directives from it, and builds one
  `LineIndex` where it built two. The diagnostics lex nothing of their own.
  If the parser panics (not expected: `parser_panic_sweep`), the report
  comes from a swallow-mode lex made then (`directives::preprocess`), as the
  lexer diagnostics did before.
- **Counted.** `Generation` gains `parses` (`ParseCounts`: sources parsed
  and parses handed back again), the `observed.generation` event carries
  them as `parsed` and `parses_reused`, and `edit_counts` prints them as
  `p.new` and `p.reuse`.

**Departures.**

- **Not every parse goes through the memo.** The verification parses at a
  version other than the file's own are not memoised: the shape gate's
  parses under untrusted `LangVersion` provenance, and
  `SyntaxRecovery::of_guessed_version`'s two endpoint parses inside
  `borzoi-cst`. They run only for an untrusted project's flagged files and
  the single-file hover, their verdict is cached with the fold's entry, and
  no other reader asks for a parse at those versions.
- **D2's parse key named provenance trust.** Trust is not an input of the
  parse; it decides only what the fold derives from it. So the memo keys on
  the parse's inputs, and the fold's cache adds the trust.
- **The syntactic diagnostics are not memoised themselves.** They are
  computed from the memoised parse each time they are asked for. In the
  keystroke script no text is diagnosed twice (an unchanged pull is
  answered from its result id), so a table for them would hold nothing a
  later request reads.
- **The single lex needed a `borzoi-cst` change.** The token-pipeline plan
  called reading the lexer errors from the table "an LSP change". The
  parse's lex is the only one left, and its table is gone once the parse
  returns, so `Parse` carries the report. That costs a parse nothing on
  clean source (the writer records an error only when the driver reports
  one) and moves the line-directive store it already built.
- **The expected gain was a parse of `E` "under 5 M".** `E` (`Stack.fs`)
  costs 0.62 M to parse and 0.24 M to lex three times. In the script the
  diagnostic pull of `E` comes first, so it keeps the parse and loses the
  lexes, and `semanticTokens E` loses the parse.

The oracle:

- **The diagnostics are unchanged.** Every existing diagnostics test keeps
  its assertions, as do both diagnostic models in `incremental_state_machine`.
  `tests/all/common/diagnostics_reference.rs` keeps the producer as it was,
  three lexes beside a parse, and two tests hold the new one to it:
  `syntactic_diagnostics_diff` over every one- and two-piece source and
  random walks over pieces that reach every producer and rule (lexer and
  directive errors, the overlap rule dropping a parser error, the FS3350
  exemption, warnings, cross-file `#line` groups, under two symbols, five
  versions and both grammars; a fixed-seed census asserts each is reached),
  and `parser_corpus_sweep`, which now compares every corpus file's groups
  (5,213 files, all equal).
- **The report is the swallow-mode driver's.** Under `token-reference`,
  every lex asserts that the table's report equals `directives::preprocess`,
  so the parser suites and the cst corpus sweeps hold it on every file.
  `token_table_reference` asserts it explicitly over its pairs, wide pairs
  and random walks, and `a_parse_reports_what_the_preprocessor_does` holds a
  parse to passing it on at both ends of the version ladder and in both
  grammars, reaching every preprocessor error variant and a `#line`.
  `parser_depth_limit::a_breaching_parse_keeps_the_preprocessor_report`
  covers the parse that gives up on depth.
- **Memo ≡ recompute.** `parses::tests` generates sequences of lookups over
  five spellings of three files (one by case, one through `.` and `..`)
  and none, seven texts whose parses differ by symbols, version, grammar and
  preprocessor report, fresh and shared allocations, two symbol sets and
  three versions. Every parse handed back equals a fresh one (tree by
  structure, errors, warnings, report, flags and diagnostics), and the memo
  parses exactly when a model that remembers each file's last text under
  each set of inputs says it holds none. Its non-vacuity check counts hits on
  the held allocation and on a byte-equal copy, replacements, ABA lookups
  (a text again after its parse was replaced), lookups naming no file, and
  second input sets.
- **Counts.** `observed_counts::a_keystroke_parses_the_edited_text_once`:
  after a comment keystroke, a pull client's diagnostic request parses once
  and the edited document's semantic tokens reuse that parse (a push client:
  `didChange` parses, the tokens reuse), and the other document's request
  parses nothing. `single_file_requests_reuse_the_diagnostics_parse`: on a
  buffer no project owns, after the pull parses it, hover, definition,
  references, document symbols, semantic tokens and workspace symbols each
  read the memo's parse and parse nothing.

Planted, each caught, then removed:

| plant | caught by |
|---|---|
| the diagnostics parse outside the memo (the behaviour before this stage) | `a_keystroke_parses_the_edited_text_once` |
| the fold parses outside the memo | `a_keystroke_parses_the_edited_text_once` |
| a single-file handler (document symbols) parses outside the memo | `single_file_requests_reuse_the_diagnostics_parse` |
| the memo ignores the language version | both `parses` properties |
| the memo ignores the symbols | both `parses` properties, six `incremental_state_machine` tests |
| the memo ignores the grammar | both `parses` properties |
| the memo compares texts by pointer only | both `parses` properties |
| the memo never replaces a parse under the same inputs | both `parses` properties |
| the memo keys a file by its spelling, not normalised | both `parses` properties (once the paths included one file spelt plainly and through `.` and `..`) |
| the counts not reset when a generation begins | `the_counts_are_per_generation` (once it looked up between generations) |
| the overlap spans taken from lexer errors alone | all three `syntactic_diagnostics_diff` tests |
| the table's report skipping lexer errors | four `token_table_reference` tests, three `syntactic_diagnostics_diff` tests, seven `diagnostics` unit tests |
| the table's report dropping the `#line` directives | three `token_table_reference` tests, four `parser_ifdef` tests (through the lockstep assertion), three `syntactic_diagnostics_diff` tests, two `diagnostics` unit tests |
| a parse that gives up on depth dropping the report | `a_breaching_parse_keeps_the_preprocessor_report` (added for it: the plant first passed every test) |

Three plants first passed every test: the spelling-keyed memo (the
generated paths spelt the one file through `.` and `..` only), the counts
not reset (a lookup between generations was never made), and the dropped
report on a depth breach (no test parsed a deep file with a preprocessor
error). The tests named against them were extended or added, and each then
failed.

**Measured** on WoofWare.Incremental, 20 keystrokes per run, each row the
minimum over three runs of each binary: Stage 4b (0f573532, re-measured
here at 35.55 M per keystroke against its own report's 35.81 M) and this
stage (52ee07d0). Every source-read, validation and ownership column is
unchanged (80 `stat`s on each fold-reading row, 137 validation `stat`s on
each row that reads the evaluation, 8 ownership `stat`s per keystroke).

| message, per keystroke | instructions before | after | allocs before | after | p.new | p.reuse |
|---|---:|---:|---:|---:|---:|---:|
| `didChange E` | 0.16 M | 0.17 M | 99 | 99 | 0 | 0 |
| `diagnostic E` | 3.63 M | 3.39 M | 238 | 245 | 1 | 0 |
| `diagnostic L` (`unchanged`) | 2.68 M | 2.68 M | 84 | 84 | 0 | 0 |
| `semanticTokens E` | 9.00 M | 8.38 M | 5,018 | 4,928 | 0 | 1 |
| `semanticTokens L` | 12.36 M | 12.38 M | 11,443 | 11,443 | 0 | 0 |
| `hover L` | 7.65 M | 7.67 M | 3,878 | 3,878 | 0 | 0 |
| `keystroke total` | 35.55 M | 34.72 M | 20,760 | 20,677 | 1 | 1 |

| cold message | instructions before | after | allocs before | after | p.new | p.reuse |
|---|---:|---:|---:|---:|---:|---:|
| `didOpen E` | 339.9 M | 339.8 M | 122,459 | 122,425 | 0 | 0 |
| `didOpen L` | 63.0 M | 62.8 M | 13,366 | 13,366 | 0 | 0 |
| `diagnostic E (cold)` | 6.3 M | 5.9 M | 239 | 248 | 1 | 0 |
| `diagnostic L (cold)` | 10.5 M | 8.1 M | 319 | 322 | 1 | 0 |
| `semanticTokens E (cold)` | 2622.9 M | 2620.2 M | 3,413,069 | 3,413,488 | 80 | 2 |
| `semanticTokens L (cold)` | 568.6 M | 567.2 M | 357,816 | 357,816 | 0 | 0 |
| `hover L (cold)` | 91.4 M | 91.4 M | 58,098 | 58,098 | 0 | 0 |

The keystroke falls by 0.83 M instructions (2.3%) and 83 allocations. The
three lexes of `E` the diagnostics made cost 0.24 M, and `diagnostic E`
falls by that; it keeps `E`'s one parse, as the first to ask. The parse
leaves `semanticTokens E` (0.62 M and 90 allocations), whose fold reads
it (`p.reuse` 1). `diagnostic E` gains 7 allocations: the memo's key and
entry. The cold rows: the cold pulls lose their lexes (`diagnostic L` 2.4
M, a larger file), and the cold `semanticTokens E` parses 80 items and
reuses the 2 the cold pulls parsed. It moves within its runs' spread
(2,620–2,656 M); its 419 extra allocations are the memo's entries for the
80 items.

### Stage 6a: env reference resolution records its read set

**Dependencies**: Stage 2a. **Implements**: D3 for the assembly env's
reference resolution.

The env's reference resolution touches the disk from the LSP crate itself:
`project_assets`, the project graph, `locate_fsharp_output_dll` and
`child_dirs`, `entry_ref_tfms`' canonicalisation, and the DLL reads. All of it
moves into its own `observed` module, which counts each call and returns a
`ReadSet` beside the `ReferenceSet`. `CachedAssemblyEnv` carries it. The
module's worklist allows go. Nothing validates against the read set yet, so
behaviour is unchanged. Without this stage, Stage 6b's env key would be
hand-wired invalidation under another name.

**Oracle**:
- `reference_set_msbuild_diff`, `resolve_real_project_diff` and the
  whole-project gate are unchanged.
- A read-set soundness property over generated restored-project fixtures (an
  assets file, framework and package DLLs, sibling projects built and
  unbuilt). Perturbing anything outside the read set leaves
  `reference_dlls_for_project` unchanged. Perturbing anything inside it,
  including creating a recorded-`Absent` output DLL or adding a pack version
  to a recorded listing, fails validation.
- `cargo clippy` fails on a new direct `std::fs` call on the path.

**Expected gain**: none (the stage is infrastructure). The read set's size on
WoofWare.Incremental is reported, which bounds Stage 7's validation cost.

**Implemented** (`crates/lsp/src/semantic.rs`, `crates/lsp/src/project_assets/`,
`crates/lsp/src/workspace.rs`, `crates/msbuild/src/observed.rs`):

- **One observer per resolution.** `cached_assembly_env` makes a
  `borzoi_msbuild::observed::Observer` and threads it through the
  resolution: the producer-TFM recovery (`env_ref_tfms`,
  `resolve_transitive_project_tfms`), the project-graph walk
  (`Workspace::project_graph_with_producer_tfms`), `graph_ref_targets`'
  canonicalisations, the assets files and package probes
  (`resolve_assemblies_*`, `pick_existing`), the framework packs
  (`resolve_framework`, `best_version_with_tfm`), the output-DLL search
  (`locate_fsharp_output_dll`, `child_dirs`), and the DLL reads. It is the
  module the evaluations and the ownership walk already observe through, so
  the env's reads are the same `Read`s, in the currency `read_checks`
  confirms, and Stage 6b needs nothing new to validate them.
- **The graph walk records what it consulted.** Each evaluation a node
  makes contributes its read set (the seeded inner pass and each remaining
  TFM of a multi-targeted project too: they all go through one recording
  closure in `resolve_node_uncached`; Codex found the branches dropped); the
  entry, when this
  generation has confirmed its cached evaluation, contributes the reads that
  confirm it (its read set less the contents of installed versions). The
  `.fsproj` diagnostics' walk (`Workspace::project_graph`) records into an
  observer of `Unobserved::FsprojDiagnostics`, which nothing reads.
- **A DLL is recorded by its stamp.** `Observation` gains `Stamp`
  (`FileStamp`: device, inode, size, modification and change times), and
  the module gains `stamp` and `read_stamped`, which stamps the open file
  before reading it, as the `Vfs` does. A stamp taken within
  `STAMP_RACY_WINDOW` (2 s, the `Vfs`'s `RACY_WINDOW`) of the file's last
  change is `racy` and never holds again (`Read::still_holds`,
  `ReadChecks::holds`): two writes of the same size in one timestamp tick
  leave every field the same (Codex). `enumerate_dll_type_defs_cached`
  stamps each DLL before it asks the assembly cache, so a projection the
  cache answers is recorded exactly as one read from the DLL. The DLLs are
  read on worker threads, each with an observer of its own, which
  `Observer::absorb` joins.
- **What the env carries.** `CachedAssemblyEnv::reads` is an `EnvReads`: the
  read set, and the readers the resolution consulted that do not record
  (`unrecorded`): an on-demand restore, which reads a warm package cache and
  a scratch directory that is gone when it returns, and the C# sidecar,
  which reads the C# sources itself. Either is named when it is consulted,
  whatever it concludes: a restore that declines for a package missing from
  the cache, and a C# build that fails, depend on what they read as much as
  a success does (Codex found both named only on success). `SemanticState::env_reads_for_project` reads it, and
  `reference_resolution_for_project` returns `reference_dlls_for_project`'s
  answer with what resolving it read.
- **The worklist.** `Unobserved::EnvResolution` is gone. What remains of it
  are three readers that no stage of this plan routes as env resolution:
  `EnvKey` (the dotnet-root fallback probe and its canonical spelling,
  computed on every lookup to key the cache, which Stage 6b decides),
  `AssemblyCache` (the cache's entries and the stamps it brackets them
  with, never an input of their own since the env records the DLL's stamp),
  and `CSharpSidecar` (the sidecar's binaries and the `dotnet` that runs
  them).
- **Counted.** Each resolution emits `observed.env_resolution`: its own
  opens, `stat`s, listings and canonicalisations (the evaluations it ran
  report theirs as before), its read set's size and its unrecorded readers.
  `edit_counts` prints them as `a.open`, `a.stat`, `a.list`, `a.canon`,
  `a.reads` and `a.unrec`.
- `borzoi_msbuild::observed::soundness::sweep_with` is the sweep for any
  computation that returns a value and its read set; `sweep` is it applied
  to an evaluation.

**Departures.**

- **No new `observed` module.** The plan gave the env a module of its own.
  The evaluator's already counts every call and records `Read`s that
  `read_checks` confirms, so the env reads through it, and the module grows
  the one observation it lacked (`Stamp`).
- **A DLL's observation is its stamp, not a digest of its bytes.** Hashing
  every referenced DLL would add a read of each to every warm start that the
  assembly cache answers, and the cache keys on the DLL's stamp already.
  D3's observation list names `Stamp(stamp)`; this is that.
- **The read set is not always complete, and says so.** An on-demand
  restore and the C# sidecar answer without recording; `EnvReads::unrecorded`
  names them rather than presenting a partial read set as whole. Stage 6b
  has to decide what such an env is validated by.

The oracle:

- `reference_set_msbuild_diff`, `resolve_real_project_diff` and the
  whole-project gate are unchanged.
- `env_read_set_soundness::the_env_resolution_read_set_is_sound` generates
  restored-project fixtures: packages whose DLL is in the primary package
  folder, only in the fallback, or in neither; one or two framework-pack
  versions that hold the project's TFM or not; up to two sibling F#
  projects built under the project's TFM, another, both or neither,
  recorded in the assets file or not, naming their output or not, and
  multi-targeted with a restore of their own or not (a multi-targeted one
  imports a file under its other TFM alone, and references a built project
  of its own, which an extra perturbation removes through that import). `sweep_with` perturbs
  every file (rewrites, same-size rewrites, deletions, creations under
  every name in every directory, every recorded absence created, every
  listing grown) and asserts that whenever every answer stands, resolving
  again gives the same DLLs. The plan's two named perturbations are extras:
  a newer pack version installed, and each unbuilt sibling's output built;
  each of these must fail validation where the resolution looked for it.
  Its non-vacuity check, over a fixed seed, finds package, framework and
  sibling DLLs, sees deletions and the extras move the resolution, and sees
  recorded absences created fail validation.
- `the_env_records_each_dll_stamp_with_or_without_the_cache`: the env's read
  set holds a stamp of every DLL it folded, whether read or answered by the
  assembly cache (a real `System.Runtime.dll`, cached by a first build), and
  rewriting the DLL fails validation.
- `workspace::tests::validation::the_graph_walk_records_the_confirmed_entry_and_each_evaluated_node`.
- `semantic::tests::a_consulted_unrecording_reader_is_named_whatever_it_concludes`:
  a restore that declines, and an enabled sidecar's C# reference that
  delivers nothing, are each named; a disabled sidecar is not.
- `observed::tests::a_stamped_read_records_a_stamp_a_rewrite_moves` in
  `borzoi-msbuild`: a stamp of a file changed within the window is racy and
  never holds; past it, a same-size rewrite fails validation.
- `read_checks::tests::a_racy_stamp_never_holds`, which also holds the two
  windows equal. The stamp test checks that an env built straight after its
  DLLs were written does not validate.
- `cargo clippy` fails on a direct `std::fs` call on the path: the crate's
  ban, which now has no allow there.

Planted, each caught, then removed:

| plant | caught by |
|---|---|
| `child_dirs`' listing unrecorded | both soundness tests |
| a candidate output DLL's `stat` unrecorded | both soundness tests |
| the framework pack's version listing unrecorded | both soundness tests |
| a fresh node evaluation's reads dropped by the graph walk | both soundness tests |
| the reads of a multi-targeted node's other TFM branches dropped (Codex) | both soundness tests (once a multi-targeted sibling imported a file under one TFM alone) |
| an on-demand restore named only when it succeeds (Codex) | `a_consulted_unrecording_reader_is_named_whatever_it_concludes` |
| the C# sidecar named only when it delivers metadata (Codex) | `a_consulted_unrecording_reader_is_named_whatever_it_concludes` |
| the producer assets files' reads unrecorded | both soundness tests (once siblings could be multi-targeted with a restore of their own) |
| `pick_existing`'s probes unrecorded | both soundness tests (once packages could sit in a fallback folder) |
| the confirmed entry's reads dropped by the graph walk | `the_graph_walk_records_the_confirmed_entry_and_each_evaluated_node` (added for it) |
| the stamp before the assembly cache dropped | the stamp test, its cached case |
| the worker threads' reads not absorbed | the stamp test |
| the env not keeping its read set | the stamp test |
| a direct `std::fs::read_to_string` in `read_raw_assets` | `cargo clippy` (`disallowed_methods`) |
| a racy stamp holding in `Read::still_holds` (Codex) | `a_stamped_read_records_a_stamp_a_rewrite_moves`, the stamp test |
| a racy stamp holding in `ReadChecks::holds` (Codex) | `read_checks::tests::a_racy_stamp_never_holds` |
| a stamp never marked racy | `a_stamped_read_records_a_stamp_a_rewrite_moves`, the stamp test |

Codex also found `read_stamped` preallocating the file's size infallibly,
which aborts the process on a file too large to hold where `std::fs::read`
fails with `OutOfMemory` and the DLL is skipped. It reserves fallibly now,
as `std::fs::read` does. No test holds it: the allocation-limit harness
(`borzoi_oracle_harness::alloc_limit`) is a global allocator a test binary
installs, which `borzoi-msbuild`'s do not.

Four plants first passed every test, and the tests named against them were
extended or added until each failed. Two still pass, and neither is a
defect the generated trees can show: the canonicalisation that seeds a
graph node's TFM, which `graph_ref_targets` records again for the same path
(the read set is a set, so it is unchanged), and the canonicalisation of an
assets file's path, whose answer moves only through a symlink, which the
fixtures never hold. The second is a gap: a soundness fixture with symlinked
project and `obj` directories would see it.

**Measured** on WoofWare.Incremental, 20 keystrokes per run, each row the
minimum over three runs of each binary: Stage 5 merged with `main`
(34655569) and this stage. No keystroke builds an env, and no keystroke
row moves beyond its runs' spread: 34.83 M per keystroke before, 34.67 M
after, and 35.07 M in a second set of runs of this stage's final head taken
while the gate ran on the same machine, every allocation count identical. The
one env build, in the cold `semanticTokens E`, records **385 reads** (the
graph walk's evaluations included) from 159 stamped DLL reads, 167 `stat`s,
3 listings and 4 canonicalisations, with nothing unrecorded: the size Stage
6b's validation starts from, before the installed-version exemption. It
costs 1,336 allocations more (3,413,630 to 3,414,966) and its instructions
move within the runs' spread (2,599–2,655 M before, 2,605–2,634 M after).

### Stage 6b: targeted assembly invalidation

**Dependencies**: Stages 3b and 6a. **Implements**: D2's env key (item 3).

An env is valid while its read set validates against the `Vfs`, with the
immutable-root contents exempt as in D3. A `.dll` event marks paths for
re-`stat`. It no longer clears every env. `pdb_images` is keyed by the DLL's
stamp.

**C# references** keep their event-driven invalidation. With
`enableCSharpProjectReferences`, a `.cs` edit changes neither the F#
project's evaluation nor any DLL in the env's read set. The sidecar
re-derives its content-addressed metadata only when the env is rebuilt
(`metadata_dlls_for_csproj`). So a `.cs`/`.csproj` event still drops every
env that has a C# edge, and only those. `CSharpSources` is therefore an
explicitly **unwitnessed** input class (D1, D7). A C# edit with no watch event
serves stale metadata until the sidecar reports its read set (the `.csproj`
closure's sources and imports), which is follow-up work outside this plan.

**Oracle**:
- `incremental_state_machine` (its `.dll` writes) is unchanged.
- A new LSP test: two unrelated projects, with a rebuilt DLL referenced by only
  one. The other's env survives (by `Arc::ptr_eq`), and the first's is rebuilt.
- With C# references enabled, a `.cs` edit plus its event re-reads the edited
  type's metadata into the env of a project that references the `.csproj`,
  and leaves an F#-only project's env alone (`csharp_ref_assembly_env_e2e`'s
  fixture).

**Expected gain**: a build or project-file save in one project costs zero env
rebuilds in every project that does not reference what changed. Today each
costs one cold env and fold per open project (about 3.8 G instructions on
WoofWare.Incremental with the on-disk cache off).

**Implemented** (`crates/lsp/src/semantic.rs`, `crates/lsp/src/workspace.rs`,
`crates/lsp/src/read_checks.rs`, `crates/lsp/src/server.rs`,
`crates/msbuild/src/observed.rs`):

- **An env is confirmed, not trusted.** Each `assembly_envs` entry
  (`EnvEntry`) carries what to confirm before it is served again
  (`EnvChecks`), and the generation that last confirmed it. The first
  lookup in a generation confirms every check (`Workspace::confirm_env`);
  later lookups in the generation are hits. An entry any of whose answers
  moved is dropped with every fold against its env (`drop_env`: the
  resolved, inferred and incremental-base folds hold handles into it) and
  built afresh, and the drop owes the client a semantic-token refresh.
  Outside a generation an env is served as it is, as a cached evaluation is.
- **The fold's fast path confirms its env too.** `resolved_projects` was
  served without asking for the env again, on the ground that every rebuilt
  env cleared it. It now finds the entry its env belongs to, by identity
  rather than by key (computing the key would cost the SDK probing the fast
  path exists to skip), and confirms it (`env_still_served`); a fold whose
  env has no entry goes with it.
- **What an env confirms** (`Workspace::env_checks`) is its resolution's
  read set less the contents of the installed versions it trusts, by the
  rule a cached evaluation follows, which became one function for both
  (`confirming_reads`). The installed versions are those under the dotnet
  root the env was resolved against, under every package folder its assets
  file names (`ResolvedAssemblies::package_folders`: NuGet extracts a package
  version into `<id>/<version>/` there and never changes it in place,
  whichever settings chose the folder), and under every root and the NuGet
  folder the entry project's cached evaluation names (its `ImmutableRoots`,
  which the cached evaluation now keeps). A package version is trusted only
  if its `.nupkg.metadata` was written outside the racy window before the
  resolution began (`nuget_committed_before`, read at the generation's time
  before the build).
- **Shared stamps.** The env's checks are confirmed through the workspace's
  own `ReadChecks`, the one confirming evaluations, so a directory or file
  both depend on (the entry's `.fsproj` closure, which an env records through
  its graph walk) is `stat`ed once per generation. A checker has an identity
  (`ReadChecks::id`), since a check's witness names its directories by index:
  checks made under another workspace never hold. The generation separates
  the counts (`EnvCounts`: envs built, and the confirmation's opens, `stat`s,
  listings and canonicalisations), which `edit_counts` prints as `a.built`,
  `av.open`, `av.stat`, `av.list` and `av.canon`.
- **A watch event widens the next confirmation, and names paths.** Every
  event batch goes to `SemanticState::hint_assembly_paths`. It drops at once
  each env that read one of its paths as spelt (`EnvChecks::named_by`: a path
  its resolution read, or an entry of a directory it listed), which is the
  only cover for a file system whose stamps do not move (D1's "not
  covered"). Every other env stands, with a mark (`EnvEntry::event_since`)
  that makes its next confirmation inside a generation cover the installed
  contents it trusts too (`EnvChecks::exempt`; `confirm_env`'s
  `after_event`), whatever path the event named. That is D3's fallback ("a
  structural watch event still drops everything") restated for envs: an
  in-place rewrite inside an installed version moves a stamp the widened
  confirmation sees, under any spelling the client reports (another case, a
  symlink, a native separator), at the cost of one full confirmation per
  event rather than a rebuild of every env. A structural event no longer
  drops a fully recorded env: `invalidate_all` keeps every env whose reads
  are all recorded. The one
  exception is a NuGet settings file (`nuget.config` in any casing): SDK
  discovery reads NuGet's settings directly, unrecorded (Stage 3b's open
  gap), and they choose the folder versioned SDKs resolve from, so an event
  naming one drops every env (`drop_every_env`; found by Codex).
- **What the graph walk absorbs for the entry** is its cached evaluation's
  whole read set (`CachedEvaluation::reads`), not the reads that confirm it:
  the installed contents an evaluation exempts (an SDK's imports) are then
  among the env's exempt reads, which a watch event makes it confirm.
- **C# references keep their event-driven invalidation.**
  `invalidate_assembly_state` is now the event-driven rule for the envs with
  an unrecorded reader: it drops every env whose `EnvReads::unrecorded` is
  not empty, and `apply_watched_changes` calls it for every `.dll`, `.cs` and
  `.csproj` event, as before. `invalidate_all` drops them too. A `.cs` edit
  plus its event therefore drops the envs with a C# edge (and those an
  on-demand restore built) and nothing else. `CSharpSources` stays
  unwitnessed.
- **`pdb_images` is keyed by the DLL's stamp** (`FileStamp`, through the
  `GoToDefinition` reader). An entry answers only while the DLL's stamp is
  the recorded one, and a racy stamp is never cached under. The sidecar
  `.pdb` the DLL names is not in the key, so a referenced-assembly watch
  event still clears the map, as before: a symbol file added beside an
  unchanged DLL reaches it that way (found by Codex). `.pdb` files are not
  watched, so one added with no `.dll` event is unseen, as before.
- **Both trust opt-ins drop every env** when they change
  (`set_on_demand_restore_enabled`, as `set_csharp_project_references_enabled`
  did), since neither is in the env's key and a structural event no longer
  clears every env behind them.
- **A racy DLL stamp is confirmed by the DLL's bytes** (see the second
  decision below): `read_stamped` records a digest of the bytes it read
  beside a racy stamp (`FileStamp::digest`), and `Read::still_holds` and
  `ReadChecks::holds` confirm such a stamp by reading the file again and
  comparing digests. A read that finds the bytes under a stamp no longer racy
  keeps that stamp on the check (`Check::confirmed`), which then confirms it
  by a `stat`. `enumerate_dll_type_defs_cached` records its stamp taken before
  the assembly cache is asked only when the cache answers: on a miss the read
  records the stamp (and digest) its bytes rest on, and under a racy stamp
  the cache is not asked at all (an answer resting on a racy stamp could
  never be confirmed again; found by Codex).
- **An unreadable DLL's stamp says so.** `read_stamped` of a DLL that can
  be `stat`ed and not read recorded the open's failure as the stamp's
  answer; asked again as a `stat`, it succeeded, so it never held, and the
  env was rebuilt and the client refreshed in every generation. Asking every
  failed stamp as a whole-file read instead was wrong the other way: a
  `stat` that failed (a directory above denying search) and then succeeded,
  the file still unreadable, held. So the two are told apart in the record:
  a whole-file read that could not open or read the file records the file's
  stamp marked `FileStamp::unreadable` (by a `stat`, when it could not
  open it), or the `stat`'s failure, exactly as `stamp` would. An unreadable
  stamp is asked again as the read (`read_stamped_now`, compared by
  `same_unreadable`, raciness aside: the failing read is what it rests on),
  is never a fact, and so never trusted inside an installed version; a
  failed stamp is asked as a `stat`, as before (both found by Codex, in two
  rounds).
- **A generation number is a workspace's.** An entry confirmed in
  generation `n` is trusted for the rest of `n` only while the workspace
  confirming it owns its checks (`Workspace::owns`): a replaced workspace
  starts its generations again from 1 (found by Codex).

**The first decision: envs with unrecorded readers.** An env built through an
on-demand restore or the C# sidecar (`EnvReads::unrecorded` not empty) is
never trusted on its recorded reads alone. It is confirmed like any other
env, and it is dropped besides by everything that dropped every env before
this stage: a structural watch event or a dropped evaluation
(`invalidate_all`, through `sync_with_evaluations`), and a referenced-assembly
watch event (`.dll`, `.cs`, `.csproj`: `invalidate_assembly_state`). That is
today's event-driven invalidation for exactly those envs, with the read-set
confirmation added on top, so such an env is never served on less evidence
than before. Rebuilding it in every generation was the other option: it
would re-run a restore or a C# design-time build per message.

The guarantee, stated per env:

- **All reads recorded.** A cached env is served in a generation only if
  every read its resolution made, bar the contents of the installed versions
  it trusts, gets its recorded answer as of that generation's stamps (each
  witness and file stamp under the racy rule, a racy DLL stamp by the DLL's
  bytes), and every trusted version still exists. By the read-set soundness
  property it is then the env a fresh resolution would build. Outside it:
  an in-place change to an installed version (D3's axiom), which a watch
  event naming the file covers, and the file systems D1 lists as not covered,
  which a watch event naming the path covers too.
- **An unrecorded reader.** As above for what it recorded; for what the
  restore or the sidecar read, the env is current as of the last watch event
  of a structural or referenced-assembly class, or the last dropped
  evaluation. A `.cs` or `.csproj` edit with no watch event serves stale C#
  metadata (the `CSharpSources` class, unwitnessed by design), and a package
  added to the warm cache with no event reaches an on-demand restore's env
  only through one of those.

**The second decision: the installed-version exemption, and the racy
window.** The exemption is applied to env reads with Stage 3b's conditions
(above). On WoofWare.Incremental every DLL the env reads is a framework pack
or a package, so it exempts all 159 DLL stamps and the reads inside those
versions: the confirmed reads hold none of the DLL stamps. That needed the
package folders the assets file names among the roots: the whole-project
corpus restores into a package folder of its own, which NuGet's settings do
not name, and with only the settings' folder trusted every package DLL was
`stat`ed in every generation (185 `stat`s and 4.3 M instructions per
fold-reading message, measured on the way).

The racy window had a cost no measurement on WoofWare.Incremental shows,
because its env reads no project output: an env reading a DLL written within
2 s of the resolution (a referenced project just built, a package extracted
during the resolution) recorded a racy stamp, which never held again. So
every generation inside the window rebuilt the env, and each rebuild owed the
client a token refresh, whose re-pull rebuilt it again. The state machine
found it as soon as envs were confirmed: every case whose world was written
within 2 s of a request looped on refreshes until the window passed
("re-pulls kept releasing refreshes after 8 rounds"). Each rebuild is a cold
env and the cold fold it forces (on WoofWare.Incremental, with the on-disk
cache off, the 2.9 G of the 6a build round below). That is structural, and the remedy is git's for a racy index
entry: a racy stamp from a whole-file read carries a digest of the bytes read,
and confirms by reading the file again and comparing, a few million
instructions for a 2 MB DLL where a rebuild costs a cold env; once a read
finds the bytes under a settled stamp, the check keeps that stamp and
confirms by a `stat`. A same-size rewrite inside the window changes the
bytes, so it fails exactly as before. A racy stamp no read took (the stamp a
cache hit rests on) still never holds again.

**Departures.**

- **A structural watch event no longer drops a fully recorded env.** The
  plan's headline needs it: a project-file save is structural, and so is a
  build that writes `obj/*.AssemblyInfo.fs` or `*.nuget.g.props`. The event
  drops the envs whose reads name its path, and the rest are confirmed. The
  evaluations, parses and folds still go with every structural event, as
  before (Stage 7's subject), so a save in an unrelated project still costs
  the measured project an evaluation, its parses and a cold fold: only the
  env is kept.
- **D3's fallback, for envs, is a widened confirmation.** The plan kept
  "a structural watch event still drops everything" for evaluations; for
  envs, any event makes the next confirmation cover the installed contents
  (above), which covers the same in-place change without dropping the envs
  it did not touch. A first version matched the event's path against the
  installed versions each env trusted; Codex found three spellings it missed
  in two rounds (case on a folding file system, a symlinked alias, Windows'
  separator), so the matching went and confirmation took its place, which
  no spelling reaches.
- **Racy DLL stamps carry a digest** (the second decision). Stage 6a's rule
  was that a racy stamp never holds again; with envs confirmed, that rule
  rebuilt an env in every generation of the 2 s after a write and looped
  the client on token refreshes. `FileStamp` gains `digest`, and a racy
  stamp a whole-file read took holds while the file's bytes are the ones
  read.
- **The env is confirmed through the workspace's checker**, not one of its
  own as the ownership listings are. The env's read set holds the entry's
  evaluation checks (its graph walk absorbs them), so a separate checker
  would `stat` all of them twice per generation.
- **`Unobserved::EnvKey` stays.** The key (the dotnet root a project with no
  SDK falls back to, and its canonical spelling) is computed afresh on every
  slow-path lookup, so no cached value rests on it, and the env records
  every read it makes under that root. It costs one canonicalisation per
  lookup that misses the fold cache.
- **No new count harness was needed beyond two scripts** for
  `edit_counts`: `EDIT_COUNTS_SCRIPT=unrelated-build` and `unrelated-save`
  deliver the watch events a build or a save of another project of the
  workspace (by default `WoofWare.Incremental.Test`, which references the
  measured project, not the reverse) reports, without writing: neither file
  is one the measured project's evaluation or env reads, so the event is all
  that can move its answers.
- **Open: memory.** `invalidate_all` used to free every env; now an env
  stays until its reads move or a watch event names one. An env under a key
  no lookup asks for again (a project whose TFM or dotnet root moved) is
  never confirmed, so never dropped: the table grows by one env per distinct
  (project, root, TFM) a session meets. Replacing a project's env with the
  newest would bound it, but the cache deliberately holds one env per TFM
  (`assembly_env_cache_keys_on_target_framework`, preparation for a TFM
  override), so that is left to the issue it belongs to.

The oracle:

- `incremental_state_machine`: the world gains a package in a package folder
  of its own, which either assets file may name, whose DLL is a copy of the
  real `System.Console.dll` or not an assembly at all, and sources read
  `System.Console.Out`. `ReferencedAssemblies` splits in two:
  `ReferenceResolution` (the assets files and the package DLL, outside every
  installed version), which `WITNESSED` gains, and
  `InstalledVersionContents` (the framework pack's DLL, rewritten in place
  inside an installed version), unwitnessed by D3's axiom. The existing
  `.dll` writes are unchanged, and still flushed.
  `unwatched_assembly_writes_agree_with_a_fresh_session` generates scripts
  that write and delete the assets files and the package DLL and never flush;
  its non-vacuity check asks for comparisons with such a write pending and
  for hovers that read the package (272 and 4 at its default 16 cases). Four
  pinned no-flush scripts: the package DLL rewritten from junk to an
  assembly, an assets file rewritten to name the package, the package DLL
  created where the env recorded it absent, and the DLL and the assets file
  deleted. Every env in the world records all its reads (no C# project, no
  on-demand restore). On 6a the generated test and all four pinned scripts
  fail.
- `env_validation` (new): two unrelated restored projects over a stub
  framework pack, one referencing an F# project whose built output is a
  copy of `System.Console.dll` and a package. A rebuild of that output, with
  its watch event or without one, rebuilds the referencing env and leaves the
  other the same allocation (`Arc::ptr_eq`), and an unchanged generation
  builds no env and confirms each by `stat`s alone; an event naming files no
  env read drops nothing, an unwatched in-place rewrite inside a trusted
  package version goes unseen (the exemption is live), and any event at all
  then makes the next confirmation see it; a package is trusted only once
  committed; an env made under another workspace is built again. The first
  two fail on 6a.
- `csharp_ref_assembly_env_e2e::a_cs_edit_and_its_event_rebuild_only_the_env_with_a_csharp_edge`
  (the plan's C# oracle, on `csharp_ref_assembly_env_e2e`'s fixture plus an
  F#-only project): a `.cs` edit adding a namespace and its event put the
  namespace in the referencing env and leave the F#-only env the same
  allocation. Fails on 6a.
- `env_read_set_soundness::what_an_env_confirms_is_sound_outside_the_installs`:
  `sweep_with` over the project tree of every generated restored-project
  fixture holds what a cached env confirms, inside a generation, to
  soundness, and a pack version installed beside the recorded ones must
  fail it; `an_env_trusts_installed_contents_and_confirms_the_rest` checks,
  over a fixed seed, that every read left out lies inside the framework pack
  or a package, and that some of each kind are.
- `the_env_records_each_dll_stamp_with_or_without_the_cache` (6a's) now holds
  the digest rule: a freshly built env's racy DLL stamps carry digests, hold
  while the bytes do, and fail on a same-size rewrite inside the window.
  `a_racy_whole_file_read_holds_while_its_bytes_do` (`borzoi-msbuild`) and
  `a_racy_stamp_holds_while_its_bytes_do_on_the_host` (`read_checks`) hold
  `Read::still_holds` and `ReadChecks::holds` to it, including the settled
  stamp that later confirms by a `stat`.
- `env_validation` also holds an in-place change to an import inside an
  installed SDK version, with its event, to reaching the env through the
  entry's evaluation (`a_watched_change_to_an_installed_import_reaches_the_env`),
  and a package version deleted whole, without one, to being seen
  (`a_removed_package_version_is_seen_without_an_event`).
- `a_failed_whole_file_read_holds_while_it_keeps_failing_on_the_host`
  (`read_checks`): an unreadable DLL's stamp holds while the read keeps
  failing, and fails once it succeeds; a `stat` that failed and then
  succeeds, the file still unreadable, fails.
- Unit tests whose subject the stage changed were rewritten to the new rule,
  not loosened: `a_dropped_env_forces_a_cold_refold` (was
  `assembly_state_invalidation_forces_cold_refold`),
  `the_broad_invalidations_drop_only_an_env_with_an_unrecorded_reader` (was
  `invalidate_all_drops_assembly_env`),
  `a_watched_change_drops_only_an_env_that_read_its_path` (was
  `watched_dll_change_drops_the_assembly_env_cache`), and
  `pdb_image_cache_computes_once_per_stamp_and_caches_the_negative`. New:
  `a_trust_opt_in_change_drops_every_env`.

Planted, each caught, then removed:

| plant | caught by |
|---|---|
| no confirmation (a cached env served as it is, as on 6a) | the generated assembly test, the pinned scripts, four `env_validation` tests |
| the fold's fast path not confirming its env | the generated assembly test, the pinned scripts |
| a dropped env owing no refresh | the pinned scripts (what a client displays) |
| watch events not reaching `hint_assembly_paths` | `a_watched_change_drops_only_an_env_that_read_its_path`, `a_watched_rebuild_rebuilds_only_the_env_that_reads_it`, `watched_assembly_refresh_e2e` |
| `named_by` blind to the paths an env read | the same three, `a_dropped_env_forces_a_cold_refold` |
| no event mark (an event not widening the next confirmation) | `an_event_widens_the_next_confirmation_to_installed_contents` |
| the widened confirmation skipping the installed contents | `an_event_widens_the_next_confirmation_to_installed_contents` |
| the graph walk absorbing the entry's checks, not its whole read set | `a_watched_change_to_an_installed_import_reaches_the_env` (added for it) |
| `invalidate_assembly_state` dropping nothing | `the_broad_invalidations_drop_only_an_env_with_an_unrecorded_reader`, the C# e2e |
| `invalidate_all` keeping an env with an unrecorded reader | `the_broad_invalidations_drop_only_an_env_with_an_unrecorded_reader` |
| a NuGet settings event keeping every env (Codex) | `a_watched_change_drops_only_an_env_that_read_its_path` |
| the env ignoring the entry evaluation's roots | three `env_validation`/`env_read_set_soundness` tests |
| the env ignoring the assets file's package folders | `an_env_trusts_a_package_only_once_it_was_committed`, `an_event_widens_the_next_confirmation_to_installed_contents` |
| a package trusted before its commit marker | `an_env_trusts_a_package_only_once_it_was_committed` |
| everything under a dotnet root trusted, not only version contents | both new `env_read_set_soundness` tests |
| a trusted version's existence not confirmed | `a_removed_package_version_is_seen_without_an_event` (added for it) |
| a racy stamp holding on a digest mismatch | `a_racy_stamp_holds_while_its_bytes_do_on_the_host`, the pinned scripts |
| `Read::still_holds` ignoring the digest | `a_racy_whole_file_read_holds_while_its_bytes_do`, the stamp test |
| no digest recorded | both racy-content tests, the stamp test, every generated state-machine test (the refresh loop) |
| a settled stamp trusted without comparing | `a_racy_stamp_holds_while_its_bytes_do_on_the_host` |
| the pre-cache stamp recorded on a miss | the stamp test, every generated state-machine test (the refresh loop) |
| the assembly cache asked under a racy stamp (Codex) | the stamp test (once it rewrote a DLL with its mtime put back) |
| an unreadable read asked as a `stat` (Codex), in either crate | `a_failed_whole_file_read_holds_while_it_keeps_failing_on_the_host` |
| a generation number trusted under another workspace (Codex) | `an_env_made_under_another_workspace_is_built_again` (once its build and the replacement's first generation shared a number) |
| checks confirmed through another workspace's checker | `an_env_made_under_another_workspace_is_built_again` |
| `pdb_images` keyed by path alone | `pdb_image_cache_computes_once_per_stamp_and_caches_the_negative` |
| `pdb_images` kept on an assembly event (Codex) | the same |
| the restore opt-in keeping every env | `a_trust_opt_in_change_drops_every_env` |

Four plants first passed every test, and the tests named against them were
added or extended until each failed: the entry's checks absorbed rather
than its reads, a version's existence not confirmed, the cache asked under a
racy stamp, and a generation number trusted under another workspace. The
last one also found a defect: a confirmation outside a generation recorded
`None` as the generation that confirmed the env, which made the test pass by
accident; it now leaves the last generation's record standing. One plant is
not a defect: `drop_env` not dropping the folds against an env. The fold's
fast path finds no entry for that env, drops the env again and folds anew,
and the inferred and incremental-base folds are reused only against the same
env by identity; `drop_env` clears them to bound memory.


**Measured** on WoofWare.Incremental, each row the minimum over three runs
of each binary: Stage 6a (a055f302, with this stage's `edit_counts` scripts)
and this stage. The keystroke script ran 20 keystrokes per run; the two
unrelated-change scripts 5 rounds per run, each round the change's watch
event and then the keystroke's requests, against `WoofWare.Incremental.Test`
(which references the measured project, not the reverse). The on-disk
assembly cache is off, as in every earlier measurement.

The plan's headline, per round:

| round of | instructions 6a | 6b | allocs 6a | 6b | envs built 6a | 6b |
|---|---:|---:|---:|---:|---:|---:|
| a build of the unrelated project (its two output DLLs' events) | 3,114.8 M | **37.6 M** | 3,819,086 | 7,128 | 1 | **0** |
| a save of the unrelated project's `.fsproj` | 3,647.9 M | **1,238.2 M** | 3,967,669 | 583,168 | 1 | **0** |

| row, per round | build 6a | 6b | save 6a | 6b |
|---|---:|---:|---:|---:|
| `didChangeWatchedFiles` | 154.4 M | 1.0 M | 481.4 M | 328.0 M |
| `diagnostic E` | 2.6 M | 2.6 M | 56.1 M | 55.9 M |
| `diagnostic L` | 2.7 M | 2.7 M | 2.7 M | 2.7 M |
| `semanticTokens E` | 2,307.6 M | 17.0 M | 2,464.0 M | 213.5 M |
| `semanticTokens L` | 555.6 M | 7.0 M | 554.3 M | 550.5 M |
| `hover L` | 88.6 M | 6.7 M | 88.4 M | 87.2 M |

A build of a project the measured one does not reference builds no env and
folds nothing. The first fold-reading request after the event confirms the
env widened to its installed contents (352 `stat`s, about 10 M: the price
of covering an in-place change to an installed version under any
spelling), the rest confirm it with 10 `stat`s, and all serve the cached
fold. 6a dropped every env on the `.dll` event (the
154 M of `didChangeWatchedFiles` is freeing it) and the next request built
it and folded cold. A save builds no env either; what is left (1.23 G) is
everything else a structural event still drops: every evaluation (the
measured project is evaluated again, 329 M), every parse (82 items re-parsed
in `semanticTokens E`) and every fold (cold again in `semanticTokens L` and
`hover L`). That is Stage 7's.

The keystroke:

| message, per keystroke | instructions 6a | 6b | allocs 6a | 6b | av.stat |
|---|---:|---:|---:|---:|---:|
| `didChange E` | 0.14 M | 0.16 M | 99 | 99 | 0 |
| `diagnostic E` | 3.39 M | 3.41 M | 247 | 247 | 0 |
| `diagnostic L` (`unchanged`) | 2.67 M | 2.68 M | 84 | 84 | 0 |
| `semanticTokens E` | 8.40 M | 9.05 M | 4,928 | 4,932 | 10 |
| `semanticTokens L` | 12.47 M | 13.09 M | 11,443 | 11,447 | 10 |
| `hover L` | 7.69 M | 8.38 M | 3,878 | 3,886 | 10 |
| **keystroke total** | **34.90 M** | **36.82 M** | 20,679 | 20,695 | **30** |

Each message that reads the fold confirms the env once: 10 `stat`s and
nothing else (the entry's evaluation checks among its reads were confirmed
by the workspace earlier in the same generation, through the same checker,
so they cost nothing more), about 0.65 M instructions and 4 allocations. A
keystroke costs 1.9 M (5.5%) more. The plan's Stage 7 estimate ("roughly
130 `stat`s per fold-reading request in total") put this part at about 50:
the installed-version exemption and the shared checker make it 10.

The cold rows: the env is built in `semanticTokens E (cold)` as before, now
also turning its reads into checks and exempt reads (67 M and 20,600
allocations more: classifying each read's path against the installed
versions, and 175 `stat`s of the packages' commit markers), and the first
confirmation, in `semanticTokens L (cold)`, asks each check's query once and
takes its witness (2,377 `stat`s, 33 listings, 27 canonicalisations; 52 M),
as an evaluation's first confirmation does (Stage 3b). From then on it is
the 10 `stat`s.

### Stage 7: delete the hand-wired invalidation

**Dependencies**: Stages 2b–6b. **Implements**: the end state of D2.

With every cache keyed on its inputs, `invalidate_file`, `invalidate_project`
and `invalidate_all` delete; what remains of them is the refresh bookkeeping.
`invalidate_assembly_state` shrinks to Stage 6b's C#-edge case.
`project_parses` and `resolved_projects` become lookups through the memo
tables, validated per generation. `apply_watched_changes` reduces to `Vfs`
hints plus the refresh decisions it makes today.

**Oracle**:
- `incremental_state_machine` passes with every witnessed input class
  (`SourceContent`, `ProjectFiles`, `DirectoryMembership`), and a generated
  run with **no** `Flush` command agrees with fresh sessions. The world has no
  C# project, and `CSharpSources` is excluded from the claim by name.
- #313's planted mutations M1–M10 are re-planted where their code still
  exists, and are still caught.

**Expected cost**: this stage *raises* the file-system counts further. Stage
2b already validates the 80 Compile items per fold-reading generation (about
240 `stat`s per keystroke). Here, a cached fold also validates the
evaluation's read set and the env's, with DLL contents under immutable roots
exempt and listings not. On WoofWare.Incremental that is roughly 130 `stat`s
per fold-reading request in total. A keystroke has three such requests, so
the total is roughly **400 `stat`s per keystroke**, against 0 path `stat`s and
80 opens today. The `observed` counters measure it per message in
`edit_counts`, and the read-set sizes reported by Stages 3a and 6a predict it
before the stage is written. If the measured instructions matter, the
remedy to weigh is validating once per revision plus once per bounded time
window: that is a stated staleness bound, which the guarantee section would
then have to carry. It is not adopted by default.

**Implemented** (`crates/lsp/src/semantic.rs`, `crates/lsp/src/workspace.rs`,
`crates/lsp/src/read_checks.rs`, `crates/lsp/src/server.rs`,
`crates/lsp/src/xml_doc/lookup.rs`):

- **The invalidators are gone.** `SemanticState::invalidate_file`,
  `invalidate_project` and `invalidate_all`, and
  `Workspace::invalidate_projects`, delete. What text sync still does is the
  refresh bookkeeping: a source's `didOpen`, `didChange` or `didClose` owes a
  semantic-token refresh (`State::note_source_sync`,
  `SemanticState::owe_token_refresh`), since another open buffer's tokens may
  read what changed and the client re-asks only for the one it touched. A
  `.fsproj` buffer's sync owes nothing.
- **`project_parses` and `resolved_projects` are validated memo lookups.** A
  cached `ProjectParses` records the evaluation it was built from
  (`CachedParses::evaluation`, the `EvaluationId` Stage 4b gave every cached
  evaluation, now public as `Workspace::evaluation_id`). Every read compares
  it with the evaluation the workspace serves after confirming it
  (`validate_cached_fold`), and, once per generation, every Compile item's
  text, as Stage 2b did. A mismatch of either drops the parses and the fold
  folded over them. The resolved fold is served only through that check and
  its env's confirmation (Stage 6b's `env_still_served`), so neither table
  needs to hear about an edit, a write or a moved evaluation. A moved
  `projects_generation` no longer drops every semantic cache
  (`sync_with_evaluations`): what it drops is the envs with an unrecorded
  reader, and nothing else.
- **A change owes one refresh.** Every text sync of a source owes the
  semantic-token refresh when it is synced. A cached fold found stale owes
  no second one when every item that moved, moved to the text an open
  buffer holds now (`owe_refresh_for_unannounced`, run by the rebuild,
  which compares the stale parses' texts with the ones it read, so
  validation still stops at the first item that moved). A fold reads a
  buffer in preference to the disk, so such a move is that buffer's opening
  or one of its edits since the fold was last confirmed, each of which owed
  the refresh. A move to disk text (a buffer closed, a write with or without
  an event) owes one when it is found: possibly a second one for a watched
  write or a close, never a missed one. Without this, every keystroke sent
  the client two workspace-wide refreshes, one at the `didChange` and one
  from the next request's validation (found by Codex;
  `each_change_owes_one_refresh`). Two earlier versions recorded which
  paths had been announced, first as a set a fold consumed and then by the
  generation of the announcement, and Codex found three ways each let an
  unwatched write inherit an older announcement or a second project lose
  one (`a_stale_fold_owes_a_refresh_for_every_change_no_sync_announced`
  holds all three). The buffer the fold read is the announcement itself, so
  there is nothing to record and no lifecycle to get wrong.
- **A fold refusal records the evaluation it was refused under**, and
  `fold_outcome` reports `Unknown` under any other
  (`a_fold_refusal_does_not_outlive_the_project_that_caused_it`). A text
  change does not clear it (see the departures).
- **`apply_watched_changes` is hints plus its refresh decisions.** Every
  event's paths are hints (`Vfs::hint`, `Workspace::hint`,
  `SemanticState::hint_assembly_paths`), and each class owes what it owed
  before: a structural event republishes every open buffer to a push client
  and owes both refreshes, a source event owes both refreshes, and a
  referenced-assembly event drops the envs with an unrecorded reader. The
  structural event also ends the `projects_generation`
  (`Workspace::note_structural_event`), so an in-flight
  `workspace/diagnostic` sweep restarts on it as before (#385), and drops the
  envs with an unrecorded reader, as `invalidate_all` did.
- **What a hint does.** `ReadChecks::hint` forgot whole-file stamps
  (Stage 2b, 3b); it now also distrusts every witness taken before the hint
  that names a hinted path or the directory holding it (a per-checker hint
  count, recorded with each witness and each hinted directory), so the next
  confirmation asks those reads again: a file's text is read and compared, a
  path's kind and canonical spelling and its directory's listing are asked.
  `Workspace::hint` hints the ownership walks' checker as well as the one
  evaluations and envs share. That is the cover the structural clear gave a
  file system whose stamps do not move (D1's "not covered"), for the paths an
  event names, now without dropping what the re-asked reads confirm.
- **An env is dropped by an event only for a DLL whose stamp it read**
  (`EnvChecks::named_by`). Stage 6b dropped an env for any path its
  resolution read or any entry of a directory it listed; every such read is
  now asked again by the hint, and only a stamp, which a `stat` answers as
  before on a file system whose stamps do not move, needs the drop. So a
  save of the project's own `.fsproj` that leaves its bytes as they were no
  longer rebuilds its env.
- **A structural event widens the next confirmation of every evaluation**
  to the installed contents it trusts (`CachedEvaluation::exempt` and
  `event_since`), as Stage 6b's events do for envs. That is D3's fallback
  ("a structural watch event still drops everything") restated: an in-place
  change inside an installed SDK version is seen after an event under any
  spelling, without dropping the evaluations it did not touch.
- **NuGet's settings drop every evaluation.** SDK discovery reads them
  directly (Stage 3b's open gap), so an event naming a `nuget.config` drops
  every evaluation (`Workspace::drop_every_evaluation`) as well as every env,
  which is what the structural clear did for them.
- **The memory-only clears go.** `ParseMemo::clear`, the doc-file, census
  and ID-index caches' `clear`, and `file_parses`' clear on a structural
  event were for memory alone; each table is bounded by the files and
  settings the session meets (and the ID-index cache prunes dead envs).
- **Counted.** `Generation::folds` (`folded` in the event, `f.new` in
  `edit_counts`) counts the Compile-order folds a generation computed.
  `EDIT_COUNTS_SCRIPT=own-save` writes the measured project's `.fsproj` back
  unchanged, in place, and reports it.

**Decision 1: exactness.** Validation stays exact: every cached value is
confirmed against its inputs in every generation that reads it, under the
racy rule, with no staleness window, and the plan's "once per revision plus a
bounded window" remedy is not adopted. The cost is recovered structurally:
an event re-asks only what it names instead of dropping what depends on it;
the parses key on the evaluation's identity, so an evaluation that still
confirms keeps them; the env and the evaluations share one checker (6b); and
the widened confirmation after an event pays the installed contents' stamps
once per event, then their `stat`s from the `DiskCache` the first one
filled.

**Decision 2: the readers no stage routes** (`observed::Unobserved`):

| reader | cached across generations? | covered by | guarantee |
|---|---|---|---|
| `Restore` | not by itself; the env it builds is | event-driven: a structural or referenced-assembly event, or a moved `projects_generation`, drops it (`invalidate_assembly_state`) | as Stage 6b: such an env is current as of the last such event, and its recorded reads are confirmed besides |
| `GoToDefinition` | the PDB images (`pdb_images`); the DLL and SourceLink reads are per request, and the SourceLink cache on disk is immutable by URL | the DLL's stamp (never cached under a racy one), and event-driven for the sidecar `.pdb` it does not record: the same events clear the map | a PDB image answers for the DLL's current bytes; a sidecar `.pdb` added beside an unchanged DLL is seen after the next such event |
| `XmlDoc` | the parsed doc files, censuses and ID indices | its own witness: the `.xml`'s full stamp (device, inode, size, mtime, ctime), and nothing cached under a racy one; the others by env identity | exact: a parse is served only for the file's current bytes, with no event (the `(size, mtime)` witness it had accepted a same-size, same-mtime rewrite) |
| `WorkspaceSweep` | no: each request walks the roots afresh; a sweep in flight spans generations | the sweep restarts when the `projects_generation` moves, which a structural event now moves explicitly | a sweep's report reads one generation of the evaluations, as before |

The other variants are as Stage 6b left them (`FsprojDiagnostics` is
computed per request, `AssemblyCache` answers for a stamp the env records,
and `CSharpSidecar` feeds envs with an unrecorded reader), but for one.
`EnvKey` is gone: Stage 6b kept it on the ground that the env key is
computed afresh on every slow-path lookup, so no cached value rests on it.
The resolved fold does, because its fast path never computes the key, and
once a structural event stops dropping folds, an SDK installed in an
earlier `global.json` `sdk.paths` root, which moves the root a project with
no SDK of its own probes for, left the fold on the env of the old root
(found by Codex). So the probe (`Workspace::dotnet_root_and_reads_for_project`)
and the key's canonicalisation of the root record into the env's read set,
and the env's confirmation notices the move
(`an_env_records_what_choosing_its_dotnet_root_read`).

**Decision 3: what the deletion forced** (the departures below).

**Departures.**

- **A structural watch event drops no evaluation.** D3 kept "a structural
  watch event still drops everything" as the cover for an in-place change to
  an installed version. For evaluations, as Stage 6b did for envs, the cover
  is a widened confirmation instead: the next confirmation after the event
  asks the installed contents too. Its price is once per structural event:
  306 `stat`s on WoofWare.Incremental, and, the first time, a read of each
  installed file the evaluation read, which then sits in the checker's
  `DiskCache`.
- **The structural event still ends the `projects_generation`**, though it
  drops nothing, so that an in-flight `workspace/diagnostic` sweep restarts
  on it (#385's property), as it did when the event dropped every
  evaluation. A moved generation drops only the envs with an unrecorded
  reader.
- **A hint, not a drop, covers a file system whose stamps do not move.** The
  structural clear covered such a file system for every evaluation; a hint
  covers it for the paths the event names, and for each directory holding
  one, under the spelling the event uses or the inode it resolves to now.
  A change on such a file system that no event names is outside the
  guarantee, as D1 says.
- **An env is dropped by an event only for a DLL whose stamp it read**
  (above). Stage 6b's broader rule rebuilt the env of a project whose own
  `.fsproj` was saved unchanged.
- **The parses key on the evaluation's identity, not its value.** An
  evaluation re-run to the same Compile items and symbols (a comment added
  to the `.fsproj`) drops the project's parses; the per-file cache keeps the
  rebuild to a lookup per item. The env, whose read set holds the entry's
  `.fsproj` text, is rebuilt then too (see the open gaps). The follow-up
  cutoffs on the evaluation's consumed outputs (after Stage 8) close both.
- **A fold refusal survives a text change.** `invalidate_file` cleared every
  refusal on every edit, which made `fold_outcome` read `Unknown` until the
  next fold. A refused project caches no parses, so the next request that
  needs its fold folds it again and records what it finds; until then the
  report stands. The messages are the same either way: `Unknown` carried the
  last fold verdict forward (`project_deferral::reconcile`). A refusal under
  an evaluation the workspace no longer serves is `Unknown`, which is what
  `a_fold_refusal_does_not_outlive_the_project_that_caused_it` holds.
- **Tests that drove the server outside a generation** and relied on the
  structural clear (`project_defines_change_busts_the_cache`,
  `watched_fsproj_change_refreshes_stale_defines`,
  `created_source_refreshes_globbed_compile_set`, the sweep's mid-sweep
  property) now run each message in a generation, as the dispatch loop does:
  outside one a cached evaluation is served as it is (Stage 3b). Unit tests
  that called `invalidate_project` to force a refold now make the edit that
  forces it, or, where the subject is the rebuild from the per-file caches,
  call a test-only `forget_fold`.

The oracle:

- `incremental_state_machine`: `unflushed_runs_agree_with_a_fresh_session`
  generates scripts that make every kind of write the server is meant to
  notice (sources, project files, projects created and deleted beside the
  sources, assets files, the package DLL), open only sources and never
  flush; its non-vacuity check asks for comparisons with a write of each
  witnessed kind pending. `CSharpSources` is outside the claim by name (no C#
  project), as are `InstalledVersionContents` and `FsprojBufferInputs` (no
  script writes the pack's DLL or opens a project file). The world's
  `Install` of a file inside an installed SDK version that already exists is
  now `InstalledVersionContents`, and a pinned script rewrites an installed
  SDK's `Sdk.props` in place with its event
  ("a watched in-place rewrite of an installed SDK reaches the projects that
  resolve it"), which the widened confirmation is what passes. The flushed
  generated test and every pinned script are unchanged: an event is now a
  hint, and they hold the refresh decisions to what they were.
- `read_checks::tests`: the model property gains changes on a file system
  whose stamps do not move, each followed by a hint naming the paths it
  changed; a check that holds must still be right. Its non-vacuity check
  counts checks that only the hint can have found moved (`hinted_moved`).
  Against 6b's hint, which forgot only files, the property fails (`/w/a`
  held its stale listing); against a hint that matched spellings alone it
  failed again, through the symlink `/w/c/a`, which is why a hint matches by
  inode too.
- `workspace::tests::a_hint_asks_again_what_was_read_at_the_paths_it_names`:
  after a hint naming an entry of `sub/`, the ownership walk lists `sub/`
  again and nothing else; after one naming the project file, the evaluation
  reads it again.
- `observed_counts::a_save_that_changes_no_input_recomputes_nothing`: over
  generated projects, after another project's `.fsproj` event, a source
  created beside it, the project's own `.fsproj` written back unchanged, and
  a closed Compile item written back unchanged, each with its event, the
  requests that follow evaluate nothing, parse nothing, build no env and
  fold nothing. On 6b it fails (`evaluated: 1, parsed: 3` for the unrelated
  project's event). The control,
  `a_save_that_changes_the_project_recomputes_it`, gives the project a
  define and must evaluate it once and fold.
- `xml_doc::lookup::tests::a_same_size_rewrite_with_its_mtime_put_back_is_seen`.
- `observed_counts::a_stale_fold_owes_a_refresh_for_every_change_no_sync_announced`:
  the three announcement cases Codex found.
- `observed_counts::each_change_owes_one_refresh`: a keystroke owes one
  semantic-token refresh, at the keystroke, and the request that then finds
  the fold stale owes none; a closed item rewritten with no event owes one,
  from the request that finds it.
- #313's mutations M1–M10, re-planted below.

**Measured** on WoofWare.Incremental, each row the minimum over three runs of
each binary: Stage 6b's head with this stage's `own-save` script (fcbbc7ed)
and this stage at f1cc6641 (merged with `main` at 93abbbc5, whose changes
since 6b are outside the LSP). The keystroke script ran 20 keystrokes per run; the three
save scripts 5 rounds per run, each round the save's watch event and then
the keystroke's requests. The on-disk assembly cache is off.

The headline, per round:

| round of | instructions 6b | 7 | allocs 6b | 7 | evaluations 6b | 7 | parses 6b | 7 | envs built 6b | 7 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| a save of an unrelated project's `.fsproj` | 1,238.6 M | **49.2 M** | 582,997 | 7,431 | 1 | **0** | 82 | **0** | 0 | 0 |
| a save of the project's own `.fsproj`, unchanged | 3,779.5 M | **59.1 M** | 4,000,900 | 9,770 | 1 | **0** | 82 | **0** | 1 | **0** |
| a build of an unrelated project | 37.6 M | 38.8 M | 7,128 | 7,378 | 0 | 0 | 0 | 0 | 0 | 0 |

No round folds (`f.new` is 0 on every row after the event). Per message, the
`stat`s (sources, evaluation confirmation, ownership, env confirmation) and
anything opened:

| message, per round | unrelated save 6b | 7 | own save 6b | 7 |
|---|---|---|---|---|
| `didChangeWatchedFiles` | evaluates (115 opens) | 306 v, 4 o | evaluates (115 opens) | 1 v.open, 472 v, 4 v.canon, 1 o.list, 4 o |
| `diagnostic E` | 2,461 v, 32 v.list, 31 v.canon, 1 o | 137 v, 1 o | 2,460 v, 32 v.list, 31 v.canon, 1 o | 1 v.open, 136 v, 1 o |
| `diagnostic L` | 137 v, 2 o | 137 v, 2 o | 1 v.open, 136 v, 2 o | 1 v.open, 136 v, 2 o |
| `semanticTokens E` | 80 s, 137 v, 1 o, 352 av; parses 82 | 80 s, 137 v, 1 o, 352 av | 80 s, 136 v, 1 o, 175 av; builds the env | 80 s, 136 v, 1 o, 543 av |
| `semanticTokens L` | 80 s, 137 v, 2 o, 10 av; folds cold | 80 s, 137 v, 2 o, 10 av | 80 s, 136 v, 2 o, 2,378 av; folds cold | 80 s, 136 v, 2 o, 10 av |
| `hover L` | 80 s, 137 v, 2 o, 10 av; infers cold | 80 s, 137 v, 2 o, 10 av | 80 s, 136 v, 2 o, 10 av; infers cold | 80 s, 136 v, 2 o, 10 av |

A save of an unrelated project now costs the measured project what a build
of one costs, plus the widened confirmation of its evaluation in the event's
own message (306 `stat`s, 12.6 M): D3's cover for an in-place change to an
installed version. A save of the project's own `.fsproj` re-reads that file
(the hint forgets its stamp; the digest is unchanged, so the evaluation and
the env stand), lists its directory once (the hint distrusts the witness),
and, because the write is within the racy window, reads it again in every
message of the round (`v.open` 1). Its env confirmation in `semanticTokens E`
is widened by the event (543 `stat`s).

The keystroke:

| message, per keystroke | instructions 6b | 7 | allocs 6b | 7 |
|---|---:|---:|---:|---:|
| `didChange E` | 0.17 M | **0.02 M** | 99 | 30 |
| `diagnostic E` | 3.42 M | 3.44 M | 247 | 248 |
| `diagnostic L` (`unchanged`) | 2.68 M | 2.70 M | 84 | 84 |
| `semanticTokens E` | 9.10 M | 9.34 M | 4,932 | 5,010 |
| `semanticTokens L` | 13.04 M | 13.27 M | 11,447 | 11,450 |
| `hover L` | 8.41 M | 8.48 M | 3,886 | 3,886 |
| **keystroke total** | **36.84 M** | **37.30 M** | 20,695 | 20,708 |

Every `stat` column is unchanged: 240 source `stat`s, 680 evaluation, 8
ownership and 30 env `stat`s per keystroke, no opens. `didChange E` no longer
walks every cached project's Compile items to invalidate them. The fold the
edit stales is now found by `semanticTokens E`'s validation, which compares
items up to the edited one (index 10) before dropping the fold, and its
rebuild compares the stale texts with the new to decide the refresh: 78
allocations and about 0.2 M; the keystroke is 0.46 M (1.2%) dearer in all,
most of it in rows whose work is unchanged and which moved by as much
between this stage's own runs on earlier heads (37.06 M to 37.30 M). A first version decided it in validation, comparing
every item, which cost 0.88 M and 525 allocations. The cold rows move within
their runs' spread; the env's first confirmation (`semanticTokens L (cold)`)
`stat`s 10 more paths, its root's canonical spelling now among its reads.

Planted, each caught, then removed (each run against `cargo test -p borzoi
--lib --test all`):

| plant | caught by |
|---|---|
| cached parses not checked against the evaluation | the pinned scripts, the generated unflushed, project-write and project-creation tests |
| cached parses checked against the evaluation only, never their texts | 29 tests: every generated and pinned state-machine test, both `observed_counts` keystroke properties, `parse_cache`, a dozen `semantic` unit tests |
| a refusal reported under any evaluation | `a_fold_refusal_does_not_outlive_the_project_that_caused_it`, `a_carried_fold_verdict_is_never_restated` |
| no widened confirmation after a structural event | the installed-SDK pinned script, `a_watched_change_to_an_installed_import_reaches_the_env` |
| the exempt reads recorded empty | the same two |
| a structural event not ending the projects generation | the mid-sweep property, `the_broad_invalidations_drop_only_an_env_with_an_unrecorded_reader`, `invalidation_without_a_fold_still_owes_a_refresh` |
| a moved generation keeping the envs with an unrecorded reader | the same two unit tests, three generated state-machine tests |
| a hint distrusting no witness (6b's) | the `read_checks` model property and its non-vacuity check, `a_hint_asks_again_what_was_read_at_the_paths_it_names` |
| a hint matching spellings only | the model property (a listing through a symlink) |
| a hint not reaching the ownership walks' checker | `a_hint_asks_again_what_was_read_at_the_paths_it_names` (added for it) |
| an env dropped by an event naming any read (6b's) | `a_save_that_changes_no_input_recomputes_nothing`, `a_watched_change_drops_only_an_env_that_read_its_path`, `an_event_names_an_env_only_through_a_stamp_it_read` |
| an env never dropped by an event | `a_watched_rebuild_rebuilds_only_the_env_that_reads_it`, `sibling_rebuild_refreshes_the_assembly_env_after_watched_dll_change`, `an_event_names_an_env_only_through_a_stamp_it_read` |
| a NuGet settings event keeping the evaluations | `a_nuget_settings_event_re_evaluates_every_project` (added for it) |
| a structural event owing no token refresh | `diagnostic_refresh_precedes_semantic_tokens_refresh` |
| the doc-file cache keyed by size and mtime, caching under a racy stamp | `a_same_size_rewrite_with_its_mtime_put_back_is_seen` |
| the doc-file cache caching under a racy stamp | `nothing_is_cached_under_a_racy_stamp` (added for it) |
| a validation owing a refresh for an announced change (Codex) | `each_change_owes_one_refresh` (added for it: `(1, 1, 0, 1)` where `(1, 0, 0, 1)` is right) |
| every change treated as announced | `each_change_owes_one_refresh` (the unwatched write owes none), the pinned scripts |
| announcements as a set a fold consumes (Codex) | `a_stale_fold_owes_a_refresh_for_every_change_no_sync_announced` (added for it) |
| announcements by generation, not by the text announced (Codex) | the same test, once it wrote a closed source two projects compile (added for it) |
| the dotnet-root probe's reads left out of the env (Codex) | `an_env_records_what_choosing_its_dotnet_root_read` (added for it) |

#313's mutations, re-planted where their code still exists:

| mutation | as planted now | caught by |
|---|---|---|
| M1 the parse key ignores `#if` symbols | the memo and the fold's cache both ignore them | every generated and pinned state-machine test, both `parses` properties, `shared_file_exposes_each_projects_active_if_branch` |
| M2 a `.fsproj` watch event ignored | its events dropped before anything sees them | the mid-sweep property, two `lsp_integration` tests, five `server` unit tests |
| M3 reuse past an export change | `same_export_contribution` always true | the pinned scripts, the unflushed test, two `semantic` unit tests |
| M4 `didClose` not invalidating | `didClose` owes no refresh (it drops nothing now) | `every_source_sync_notification_owes_a_refresh` (added for it) |
| M5 a watched source content change not invalidating | its events dropped | `watched_diagnostic_inputs_request_diagnostic_refresh`, `diagnostic_input_changes_owe_diagnostic_refresh`, two `workspace_diagnostic_sweep` tests |
| M6 the `resultId` omits the symbols | as then | the pinned scripts, two generated tests, `result_id_changes_with_symbols`, `project_defines_change_busts_the_cache` |
| M7 text sync owes no refresh | as then | `every_source_sync_notification_owes_a_refresh`, two `server` refresh tests |
| M8 a `.dll` watch event ignored | its events dropped | `a_watched_rebuild_rebuilds_only_the_env_that_reads_it`, `sibling_rebuild_refreshes_the_assembly_env_after_watched_dll_change` |
| M9 a structural change does not republish | as then | `push_client_retains_publish_diagnostics_lifecycle`, three `server` unit tests |
| M10 a structural change owes no diagnostic refresh | as then | `watched_diagnostic_inputs_request_diagnostic_refresh`, three `server` unit tests |

Four plants first passed every test: the hint skipping the ownership
checker, the NuGet event keeping evaluations (the assertion on it was
vacuous: the event is structural and ends the generation anyway), the racy
doc-file cache, and M4. The tests named against them were added, and each
then failed. M4 and M7 show a blind spot of `incremental_state_machine`: a
comparison asks its direct queries before it compares what the client
displays, and a query that reads the fold finds the edited text and owes the
refresh itself, so a text sync that owes none is masked there.

### Stage 8: coalescing in the dispatch loop

**Dependencies**: Stage 2b (document versions live in the `Vfs`).
**Implements**: D6's first step.

The loop drains the channel before handling a message. A `didChange`
superseded in the batch is applied but not diagnosed. A request cancelled in
the batch is answered `RequestCancelled` without running. A semantic request
whose document has a newer queued change is answered `ContentModified`.

This only touches messages that are still queued. It cannot interrupt a
request that is already running, so an edit that arrives during a cold fold
(3.1 G instructions today) still waits for the whole fold. That case is the
one the issue's snapshot proposal addresses, and Stage 9's gate measures it.

**Oracle**:
- A loop-level test over generated interleavings of changes, requests and
  cancels, delivered as one batch. Every request is answered exactly once.
  The final state equals the state after applying the same edits one message
  at a time. No request that is answered with a result is computed against a
  superseded text.
- `incremental_state_machine` is unchanged.

**Expected gain**: a burst of `n` queued keystrokes costs one keystroke's
requests instead of `n`. `edit_counts` grows a burst mode, which sends `n`
changes before draining, to measure it.

**Implemented** (`crates/lsp/src/batch.rs`, `crates/lsp/src/server.rs`,
`crates/lsp/src/vfs.rs`):

- **The loop reads before it handles, and again before each step.** The
  messages it has read and not finished with are one **epoch**: it starts
  with the first message read while nothing was pending, and ends when every
  message read since has been handled. Before each step the loop reads
  whatever else is waiting into the epoch (`Dispatcher::admit`), so a
  message that arrives while the loop works on an earlier one still decides
  the fate of every request not yet run.
- **The decision is a pure function.** `batch::plan` maps the epoch's
  messages, as `Shape`s (kind, URI, document, request id), to the steps to
  execute, and the `Dispatcher` executes the first step whose message it has
  not handled yet, then reads, plans again and repeats. Every message is
  parsed once, into an `Incoming` whose text sync carries its parsed
  parameters, so the plan and the handler cannot disagree about whether, or
  to which document, a sync applies. A handled message keeps its shape in
  the epoch: a text sync already applied still refuses an earlier request it
  superseded when the plan is made again.
- **The plan.** For the messages before the epoch's first `shutdown` or
  `exit`: every notification and every reply to a server request in arrival
  order, a text sync that a later one of the same URI supersedes applied
  without its push diagnostics; then every request in arrival order, one a
  later `$/cancelRequest` names answered `RequestCancelled`, otherwise one
  whose document a later text sync changes answered `ContentModified`,
  neither run; every other request runs. From the barrier on the lifecycle
  rules apply as they did one message at a time: `shutdown` acknowledged,
  other requests refused `InvalidRequest`, notifications dropped, replies
  handled, `exit` ends the session and nothing after it is read.
- **A request's document** is `params.textDocument.uri`, read whatever the
  method (`batch::named_document`), so a request the server does not know
  yet, or one with malformed other parameters, is refused all the same.
  Documents are compared as the `Vfs` reads them (`vfs::DocumentKey`): two
  `file:` URIs path-equal under the platform's case rule are one document.
  Supersession of a sync's diagnostics compares exact URIs, since a push
  client keys what it shows by URI.
- **Each executed step is a generation of its own**, as each message was. A
  refusal reads nothing and opens none.
- **The epoch is bounded** at 4,096 messages (`MAX_PENDING`); beyond it the
  loop reads nothing more until the epoch ends.
- **The server's input is buffered** (`server::buffer_input`, applied in
  `main`). `lsp-server`'s stdio transport hands each message over a
  rendezvous channel and decodes the next only once the last is taken, so
  read directly a burst would sit in stdin while the loop handled its first
  message, and nothing would be waiting to plan with it (found by Codex: 49
  of 50 cancelled requests still ran). A thread takes each message the
  moment it is decoded into a buffer of 1,024, which the loop reads; a full
  buffer holds the reader back.

**Decision 1: a request runs after every notification the loop has read,
not in arrival order.** The plan names three rules; they refuse a request
only when its own document changed. A request on `L` sent before a change to
`E` that its answer reads (a hover on `L` of a binding `E` defines) would
still be computed against `E`'s superseded text if the loop ran it in
arrival order. So the plan applies every notification read first: every
text a running request reads is the newest the client has sent, and its own
document's text is the one its positions count, because a later change to
it would have refused it. The order of answers changes, which the protocol
allows (responses are matched by id).

**Decision 2: `ContentModified`, against the specification's advice.** LSP
3.17 says a server should not send `ContentModified` for a change it detects
among its unprocessed messages, since a result computed on the older state
may still be useful. The requirement here is that no answer is computed
against a superseded text, so the request is refused; `ContentModified` is
the code that says why. VS Code's client turns it into a cancellation for
semantic tokens and a default (empty) answer for the rest, and re-asks after
its own change.

**The guarantees**, for the messages of one epoch:

- every request read before `exit` is answered exactly once: by running it,
  or by one refusal (a `workspace/diagnostic` sweep that runs is answered
  when it completes, superseded or cancelled, as before; one in flight at
  `exit` is abandoned, as before);
- every request answered with a result was run after every notification the
  loop had read when it began, and after no later change to its own
  document that the loop had read;
- the notifications are applied in arrival order, so the state the epoch
  leaves is the state the same messages leave handled one at a time.

Outside them: a message that arrives while a request runs (the request is
not interrupted; Stage 9's subject), a `workspace/diagnostic` sweep in
flight across epochs (its interleaving is as before), and an epoch at its
bound.

The oracle:

- `batch::tests::the_plan_is_its_specification`: generated epochs of opens,
  changes and closes over four documents (two of them one file under two
  URIs, one not a `file:` URI), watched-file events, ignored syncs,
  cancellations, requests, `shutdown`, replies and `exit`, with and without
  `shutdown` acknowledged before; the plan equals the specification stated
  message by message by scanning the rest of the epoch.
- `dispatch_coalescing::a_coalesced_batch_agrees_with_its_messages_one_at_a_time`
  (the plan's loop-level property): generated interleavings over a
  three-source project whose sources read each other, delivered as one batch
  to a coalescing `Dispatcher` and one message per batch to a sequential
  one, with disk writes between batches and push or pull clients. Every
  request is answered exactly once, with the fate the specification states;
  the coalescing session runs exactly the requests it answers by running;
  it computes push diagnostics exactly as often as the sequential session,
  less once per superseded open or change; every answer with a result
  equals the sequential session's answer to the same request after the whole
  batch; and after every batch the published diagnostics and every
  handler's answer at every identifier agree. A quarter of the batches arrive
  in chunks with steps between them (exactly once, and the final state). Its
  non-vacuity check asks for every fate, an answer that a later change to
  another document moved, a spared publish and a chunked batch.
- `a_burst_behind_a_rendezvous_transport_is_planned_together`: behind a
  rendezvous channel, as `lsp-server`'s stdio transport delivers, a client's
  whole burst is taken by the input buffer before the loop starts, and every
  request a later cancellation names is refused.
- Pinned: the real loop reads every queued message before handling one
  (`the_loop_reads_every_waiting_message_before_it_handles_one`) and reads
  again before each (`the_loop_reads_again_before_each_message_it_handles`,
  a message sent at the end of the first generation); a handled change still
  refuses the request it superseded; a change read mid-epoch refuses a
  pending request; a hover reads the newest text of the document it depends
  on; a superseded change publishes nothing.
- `incremental_state_machine` is unchanged, and passes.
- Two `workspace_diagnostic_sweep` tests whose subject is a change landing
  mid-sweep (`a_refresh_owed_mid_sweep_follows_the_reply`,
  `a_refresh_held_for_a_cancelled_sweep_is_released`) queued the change with
  the sweep request before the loop started; the loop now plans them
  together and applies the change before the sweep starts, so the refresh it
  owes is no longer held. They drive a `Dispatcher` by hand instead, handing
  the change over after the sweep's first step (`mid_sweep`).

Planted, each caught, then removed (each run against `cargo test -p borzoi
--lib --test all`):

| plant | caught by |
|---|---|
| requests in arrival order among the notifications (the three rules alone) | `the_plan_is_its_specification`, the generated property (an answer differs from the one after the batch), `a_request_reads_the_newest_text_of_every_other_document` |
| no `ContentModified` | the specification and generated properties, both loop tests, both mid-epoch tests |
| no `RequestCancelled` | the specification and generated properties, `the_loop_reads_every_waiting_message_before_it_handles_one`, `cancel_request_ends_the_sweep` |
| every sync diagnosed | the specification and generated properties (the publish count), `a_superseded_change_publishes_nothing` |
| a sync's diagnostics superseded by a path-equal URI, not the same URI | the specification and generated properties |
| documents compared by URI, not path-equal | the generated property (the second URI of `A`) |
| a handled message's shape forgotten when the plan is made again | `a_handled_change_still_refuses_the_request_it_superseded` |
| a refusal that runs the request anyway | the generated property's count of requests run, once `lsp.request` was opened inside `handle_request` (added for it) |
| the loop reading only when the epoch begins | `the_loop_reads_again_before_each_message_it_handles` (added for it) |
| an epoch of one message (no coalescing) | both loop tests |
| text syncs after `shutdown` applied | `the_plan_is_its_specification` |
| a cancellation not reaching the sweep in flight | `a_refresh_held_for_a_cancelled_sweep_is_released` |
| the two refusal codes swapped | the generated property, `the_loop_reads_every_waiting_message_before_it_handles_one`, `cancel_request_ends_the_sweep` |
| an acknowledged `shutdown` forgotten by the next epoch | the generated property (the sequential session's later epochs) |
| the input read straight from the transport's rendezvous channel (Codex) | `a_burst_behind_a_rendezvous_transport_is_planned_together` (added for it) |

Two plants first passed every test. A refusal that ran the request anyway
called `handle_request` outside the `lsp.request` span the property counted,
which the dispatcher opened around its own call; the span moved into
`handle_request`, so any path that runs a request opens it. The loop reading
only when the epoch began passed because every loop test queued its whole
script before the loop started; `the_loop_reads_again_before_each_message_it_handles`
sends its change from a `tracing` subscriber at the end of the first
generation. Plants of the planner's lifecycle rules (syncs applied after
`shutdown`) are caught by the specification property alone: the generated
property's sequential session runs the same planner one message at a time,
so the two agree on any rule both apply.

**Departures.**

- **Requests run after every notification read**, not in arrival order
  (decision 1). The plan's three rules leave a request on one document
  computed against another's superseded text.
- **The loop reads again before each step**, not once per batch, and plans
  the whole epoch again, handled messages included, whenever a message
  joins it. So a change that arrives while the loop works on an earlier
  message refuses every request not yet run that it supersedes.
- **A superseded `didOpen` is not diagnosed either**, as a superseded
  `didChange` is not; a `didClose` always clears what its document
  published, since that computes nothing.
- **A notification after `shutdown`** is dropped without a generation; it
  had one, in which it did nothing.
- **`ContentModified` despite LSP 3.17's advice** (decision 2).
- **The input is buffered.** The plan assumed the channel the loop drains
  holds what the client has sent; `lsp-server`'s stdio transport holds one
  message at a time, so the server buffers its input itself.
- **Two sweep tests drive a `Dispatcher`** rather than a pre-queued script
  (above), and `Dispatcher` is public so that tests can hand it batches of
  their choosing.

**Measured** on WoofWare.Incremental, each row the minimum over three runs of
each binary: Stage 7 (81e3227c, the head that squash-merged as 38f7f1f8,
with this stage's `edit_counts`) and this stage. The keystroke script ran 20
keystrokes per run; the burst mode (`EDIT_COUNTS_BURST=n`: `n` keystrokes,
each with the requests a pull client sends after it, all queued while the
server is held between two messages) ran 5 bursts of 4 and 3 bursts of 16
per run. The on-disk assembly cache is off. `edit_counts` runs the loop over
an in-memory connection, so every message the client has sent is waiting;
behind stdio the input buffer makes that so up to the time to decode each
message.

| message, per keystroke | instructions 7 | 8 | allocs 7 | 8 |
|---|---:|---:|---:|---:|
| `didChange E` | 0.02 M | 0.01 M | 30 | 52 |
| `diagnostic E` | 3.47 M | 3.54 M | 248 | 265 |
| `diagnostic L` (`unchanged`) | 2.73 M | 2.73 M | 84 | 102 |
| `semanticTokens E` | 9.85 M | 9.77 M | 5,010 | 5,027 |
| `semanticTokens L` | 13.85 M | 13.63 M | 11,450 | 11,468 |
| `hover L` | 8.96 M | 8.90 M | 3,886 | 3,912 |
| **keystroke total** | **39.11 M** | **38.72 M** | 20,708 | 20,826 |

A keystroke arrives alone, so nothing coalesces: its cost moves by 1%, about
what runs of one binary differ by, and each message allocates 17 to 22 more
times (its parsed form, its shape, the plan).

| burst, whole | instructions 7 | 8 | allocs 7 | 8 | folds 7 | 8 | parses 7 | 8 | answered with a result 7 | 8 | `ContentModified` 8 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 4 keystrokes | 153.1 M | **88.8 M** | 82,592 | 29,737 | 8 | **1** | 4 | **1** | 20 | 14 | 6 |
| 16 keystrokes | 609.5 M | **298.1 M** | 330,300 | 76,408 | 32 | **1** | 16 | **1** | 80 | 50 | 30 |

A burst folds and parses once, and every request on `E` but the last
keystroke's is answered `ContentModified`. The plan's expected gain (a burst
costs one keystroke's requests) is not reached: the requests on `L`, whose
text never changes, are not superseded by anything, so each keystroke's
`diagnostic L`, `semanticTokens L` and `hover L` still run, against the one
fold, about 17 M per keystroke after the first ((298.1 − 38.7) / 15). A
client that cancels a request it repeats has those answered
`RequestCancelled`; a server that answered identical requests once per
epoch would not need it (see the open gaps).

**Open gaps.**

- Identical requests on an unchanged document within one epoch each run
  (the burst's requests on `L`); answering them once per epoch, against one
  state, would make a burst cost one keystroke's requests.
- A request does not run while notifications keep arriving: a client that
  never pauses its notifications starves its requests. Each notification is
  cheap (a pull client's `didChange` is about 0.01 M), so this needs a client
  sending faster than that.
- At the epoch's bound (4,096 messages) a request can run while a newer
  change to its document waits unread.
- A text change landing while a `workspace/diagnostic` sweep is in flight
  does not restart it; the report converges through the refresh the change
  owes, as before.
- A refused request that would have found a fold refusal does not report it;
  the next request that folds the project does.
- Behind stdio, what the loop finds waiting is what the input thread has
  decoded when it looks: a message still being decoded misses the plan. No
  test drives the real binary over stdio with a burst, since the outcome
  depends on that timing; the rendezvous test holds the buffer to taking a
  whole burst eagerly.

### Follow-up: cutoffs on the evaluation's consumed outputs

**Dependencies**: Stage 8. **Closes**:
[#712](https://github.com/WoofWare/borzoi/issues/712) and
[#723](https://github.com/WoofWare/borzoi/issues/723).

Stage 7 left two open gaps. The parses keyed on the evaluation's identity, so
an evaluation run again to the same Compile items and symbols (a comment
added to the `.fsproj`) dropped every parse and the fold over them (#712).
And the env's read set held the entry project's `.fsproj` text, which its
graph walk absorbs with the rest of the entry's evaluation, so any edit to
the file, a define included, rebuilt the env and forced a cold fold (#723).
Stage 9's `blocked-edit` measured that rebuild as most of a 3.4 G wait.

Both are D2's early cutoff, applied where the evaluation meets the two
derived values that read it: compare what the consumer read, not which
evaluation it read it from.

**What the parses read** (`semantic::ParseInputs`): the Compile items
(`CompileBefore`, `Compile`, `CompileAfter`) in the evaluation's order, the
`#if` symbols (`COMPILED`, `EDITING` and `$(DefineConstants)`), the language
version, and the cause of an untrusted `LangVersion` provenance (the
version-boundary gate runs exactly when there is one, and a refusal it makes
names it). Whether the evaluation declines the fold at all is the `Err` of
the same function. `build_parses` takes a `ParseInputs` and the `Vfs` and no
longer takes the workspace, so it cannot read anything else of the
evaluation: completeness is the function's signature, not a review finding.
A cached `ProjectParses` records its inputs and the evaluation last found to
give them. `validate_cached_fold` compares nothing while the workspace
serves that evaluation; once per new evaluation it computes the inputs again
and keeps the parses, the fold and the fold outcome when they are equal.

**What the env reads** splits in two (`semantic::EnvInputs`). Resolving the
env's references (the producer-TFM recovery, the graph walk and the
evaluations behind it, the assets files, the packs, the output search)
produces a `ReferenceSet`; building the env reads only two things of it, the
reference DLLs in fold order and whether some declared reference could not
be read (which marks the env incomplete), and then each DLL's bytes. So the
env is a function of `EnvInputs` and its DLLs' stamps, and `build_env` takes
exactly those. Each cached env (`EnvEntry`) carries its inputs and two sets
of checks: `contents` (what the build read: the DLL stamps) and `resolution`
(what resolving read, the entry's whole evaluation read set among it, and
the key's root probe and canonicalisation). The first lookup in a generation
confirms `contents`; a moved DLL drops the env, as before. Then
`resolution`; when it moves, the references are resolved again and the env
is kept, with the new resolution's checks and unrecorded readers, exactly
when the new `EnvInputs` are equal (`keep_if_resolved_alike`). Otherwise the
env is built from the resolution just made, on the fold's fast path as on
the slow one, so a generation resolves an env's references at most once
(`EnvCounts::resolutions`; Codex found the fast path resolving twice).

**The key moves with the evaluation.** The env's key (the dotnet root and the
served TFM) is chosen from the entry's evaluation. The slow path chooses it
afresh on every lookup; the fold's fast path finds the env by identity and
never did, which was sound only because a moved evaluation dropped the
parses and the fold with it. A retarget of an SDK-less project moves no
`#if` symbol, so the parses now survive it, and the fast path chooses the
key again whenever the resolution's reads have moved, before resolving: an
env under another key than the one the project would choose now is not
served (`env_still_served`).

**The guarantee.** A cached fold is served in a generation only if its
parses' Compile texts are the ones it read, the evaluation the workspace
serves gives the `ParseInputs` it was built from, its env's DLL stamps hold,
and either every read its env's resolution made holds or a resolution made
in this generation, under the key the project chooses now, gives the
`EnvInputs` the env was built from. `build_parses` and `build_env` read
nothing else, so the fold is then the one a fresh session folds. Validation
stays exact: no comparison is skipped or deferred, and every read set is
confirmed under Stage 3b's racy rule as before. What it costs is one
resolution of the env's references per generation in which the resolution's
reads moved and the inputs did not.

**Departures from the issues.** #723 proposed keying the env on "the
evaluation's reference-relevant outputs". Naming them ahead of the
resolution would mean listing what the graph walk, the producer-TFM
recovery and `resolve_reference_dlls` read of every evaluation they consult,
a list that grows with each reference rule. The cutoff sits after the
resolution instead, on the two values the build reads, so the resolution
stays one function with one read set, and only its output is compared.

Oracle:

- `observed_counts`: `an_edit_no_consumer_reads_costs_one_evaluation`
  (a comment, whitespace, an unread property, each with its watch event or
  without one) costs one evaluation and one resolution, and parses, builds
  and folds nothing; `a_define_change_keeps_the_env` re-parses and folds,
  builds no env and resolves once. Both fail on Stage 8 (`envs_built: 1,
  folds: 2`).
- `evaluation_cutoff` (new): over generated sequences of edits to a
  restored project and a referenced one (a define, the Compile order, the
  language version, the TFM, a reference to a built, an unbuilt and a C#
  project, the referenced project's output name, a comment, indentation, an
  unread property, the referenced project's comment), each step compares a
  long-lived state with a fresh one. Kept is exact: the env is the same
  allocation exactly when a fresh env would be built from the same DLLs with
  the same completeness under the same TFM, a generation builds one exactly
  when it is not, and the fold is kept, and nothing folds, exactly when the
  env is kept and the evaluation's values the parses read are unchanged.
  Sound: the fold is folded against the env the project's key serves now,
  built from the fresh state's DLLs and completeness, over parses whose trees
  are structurally the fresh state's, and its outcome is reported. Its
  non-vacuity check asks for kept and rebuilt envs, kept folds, parses that
  moved under a kept env, an env that moved under kept parses, and
  completeness that moved with the DLLs unchanged.
  `each_consumed_value_moves_only_what_reads_it` pins each edit's verdict.
  Both fail on Stage 8.
- `incremental_state_machine`: a `Touch` command rewrites a project file
  keeping everything else it says, with a comment or a `<TargetFramework>`
  (only `net10.0` is restored), in the flushed, project-write and unflushed
  generators. A pinned script retargets `P` with no event, through a comment
  and back: the parses stand and the env must move.
- `evaluation_cutoff::a_change_of_language_version_provenance_alone_reaches_the_fold`:
  a `LangVersion` of `7.0` written under a gate the walker cannot pin, and
  then not, around a file whose parse shape depends on the version: the
  fold is refused exactly while the provenance is untrusted, though the
  items, the symbols and the version are the same.
- `semantic::tests::a_kept_env_names_the_unrecorded_readers_of_the_resolution_that_kept_it`:
  an env kept across an on-demand restore that resolves, as the assets file
  did, to no DLL names the restore.

Planted, each caught, then removed (each run against `cargo test -p borzoi
--lib --test all`):

| plant | caught by |
|---|---|
| the env ignores a reference-set change (the DLLs not compared) | both `evaluation_cutoff` properties, three state-machine tests (the pinned scripts, unflushed, unwatched assembly writes), `a_watched_change_to_an_installed_import_reaches_the_env` |
| the env ignores a TFM change: the fast path keeps the key | the generated `evaluation_cutoff` property (the fold's env is not the one its key serves), `an_env_records_what_choosing_its_dotnet_root_read` |
| the env ignores a TFM change: the key holds no TFM | both `evaluation_cutoff` properties, `assembly_env_cache_keys_on_target_framework` |
| the parses ignore a define change | both `evaluation_cutoff` properties, `warm_session_agrees_with_a_fresh_one`, two `observed_counts` properties |
| the parses ignore a Compile-order change | both `evaluation_cutoff` properties |
| the parse cutoff compares the wrong field (the language version alone) | both `evaluation_cutoff` properties, five state-machine tests, two `observed_counts` properties |
| the parse cutoff ignores the language version | both `evaluation_cutoff` properties |
| the parse cutoff ignores `LangVersion` provenance | `a_change_of_language_version_provenance_alone_reaches_the_fold` (added for it) |
| the env cutoff compares the wrong field (the DLLs the env holds, not those resolved) | `each_consumed_value_moves_only_what_reads_it` (its junk-package flavour) |
| the env cutoff ignores incompleteness | both `evaluation_cutoff` properties |
| a moved resolution served without resolving again | both `evaluation_cutoff` properties, four state-machine tests, both new `observed_counts` properties, `a_watched_change_to_an_installed_import_reaches_the_env`, `an_env_records_what_choosing_its_dotnet_root_read` |
| a moved resolution rebuilds the env (no cutoff: Stage 8's cost) | both `evaluation_cutoff` properties, both new `observed_counts` properties |
| a kept env keeps its old resolution checks (a resolution every generation) | both new `observed_counts` properties (`envs_resolved_again`) |
| the parse cutoff does not record the evaluation it agreed with | the generated `evaluation_cutoff` property (the fold outcome reads `Unknown`) |
| a kept env keeps its old unrecorded readers | `a_kept_env_names_the_unrecorded_readers_of_the_resolution_that_kept_it` (added for it) |
| the DLLs an env holds not confirmed | four `env_validation` tests, two state-machine tests |
| the env built with no incompleteness mark | the generated `evaluation_cutoff` property, three `semantic` incompleteness tests |
| the fast path discarding a resolution that found new inputs, so the refold resolves again (Codex) | the generated `evaluation_cutoff` property (added for it: two resolutions in one generation) |

Codex's second round found the same shape once more: an env built that
way but not cached (a transient C# sidecar or restore failure) was built
again by the refold. The fast path now hands the env it built to the refold
(`FoldEnv::Replaced`), cached or not; no test reaches a transient failure.
It also found the generated property's census able to miss a category on
an unlucky seed; two fixed sequences that reach every category now run
first.

Two plants first passed every test: the parse cutoff ignoring `LangVersion`
provenance (no test changed the provenance alone) and a kept env keeping
its old unrecorded readers (no test kept an env across a resolution that
consulted a new one). The tests named against them were added, and each
then failed. The fast path keeping the key is caught by the generated
property and a unit test, not by the state machine's retarget script: the
resolution it runs is under the new TFM, so it keeps the env only when the
new TFM's DLLs are the old ones, which no answer can tell apart; the
property's check that the fold's env is the one the project's key serves
can.

**Measured** on WoofWare.Incremental (and WoofWare.PawPrint.Domain for
`blocked-edit`), each row the minimum over three runs of each binary (five
for `blocked-edit`): Stage 8's head (e1726d32, with this follow-up's
`edit_counts`) and this follow-up. The keystroke script ran 20 keystrokes per
run, the save and edit scripts 5 rounds. `EDIT_COUNTS_SCRIPT=own-comment` is
new: each round adds an XML comment to the measured project's `.fsproj` (or
takes it away again) and reports the write. `edit_counts` prints the
re-resolutions as `a.again`. The on-disk assembly cache is off.

| per round of | instructions 8 | this | allocs 8 | this | envs built 8 | this | folds 8 | this |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| a keystroke | 35.25 M | 35.31 M | 20,397 | 20,479 | 0 | 0 | 2 | 2 |
| a save of the project's own `.fsproj`, unchanged | 53.13 M | 53.10 M | 8,826 | 8,832 | 0 | 0 | 0 | 0 |
| a comment added to the project's `.fsproj` | 3,568.6 M | **495.5 M** | 3,984,851 | 167,702 | 1 | **0** | 2 | **0** |
| a save of an unrelated project's `.fsproj` | 47.22 M | 47.38 M | 7,168 | 7,174 | 0 | 0 | 0 | 0 |

The comment round, per message:

| message | instructions 8 | this | what it does now |
|---|---:|---:|---|
| `didChangeWatchedFiles` | 326.1 M | 327.6 M | evaluates the project (115 files opened) |
| `diagnostic E` | 51.8 M | 52.0 M | the new evaluation's first confirmation (2,239 `stat`s, 32 listings) |
| `diagnostic L` | 2.8 M | 2.8 M | |
| `semanticTokens E` | 2,494.6 M | **50.6 M** | resolves the env's references again (654 reads) and keeps the env and the fold; Stage 8 built the env and folded cold |
| `semanticTokens L` | 602.5 M | **56.1 M** | the new resolution checks' first confirmation (2,158 `stat`s, 33 listings, 28 canonicalisations); Stage 8 folded cold |
| `hover L` | 88.5 M | **6.4 M** | Stage 8 inferred cold |

What a comment edit now costs is one evaluation and the first confirmation
of each new read set: the evaluation's (as Stage 3b's first confirmation)
and the env resolution's, plus the resolution itself (about 35 M). The
first confirmations take a witness per read, which a confirmation born with
its witnesses (as Stage 4b's listings are) would save; that is left open.

Stage 9's gate, `blocked-edit` (a define added or taken away while
`semanticTokens L` re-folds, five runs of five rounds):

| project | the edit waited, Stage 8 | this | envs built 8 | this |
|---|---:|---:|---:|---:|
| WoofWare.Incremental (82 items) | 3,430.4 M | **995.1 M** | 1 | 0 |
| WoofWare.PawPrint.Domain (34 items) | 3,897.4 M | **814.4 M** | 1 | 0 |

No round builds an env: each resolves the references once and keeps it. What
the edit waits for is now the evaluation (about 330 M), its first
confirmation, the resolution and its first confirmation (about 110 M
together), and the fold, which the define makes cold (every tree is new;
the per-file parse cache serves the other define's trees from an earlier
round, so `p.new` stays at 2). Both still exceed Stage 9's 50 M budget, by
20 and 16 times: the gate's verdict stands, and the snapshot worker is what
takes the rest off the edit's path.

The keystroke and the two saves move within their runs' spread. The cold
env build (`semanticTokens E (cold)`) costs 1,670 allocations more (two sets
of checks where there was one), its instructions within the spread.

**Open gaps.**

- An env with an unrecorded reader (an on-demand restore, the C# sidecar)
  is still dropped whenever the `projects_generation` moves, so any
  `.fsproj` edit rebuilds it, as before.
- A new read set's first confirmation takes a witness per read: about 50 M
  for the evaluation's and 55 M for the env resolution's on
  WoofWare.Incremental, two thirds of what a comment edit now costs beyond
  the evaluation.
- A resolution run again evaluates every referenced project afresh (only
  the entry's evaluation is read from the cache), so its cost grows with
  the project graph.
- The env's key is compared by value, not by what it selects: a TFM change
  that selects the same DLLs (a retarget from none to the assets file's sole
  target) still builds the env again under the new key.
- Ownership answers record the identity of the evaluations they consulted,
  so they are walked again after any evaluation is run again (a few `stat`s
  and a scan of the Compile items), and a fold refusal is reported only
  under the evaluation it was refused under.
- Stage 9's gate still fails (above).

### Stage 9 (gated): requests on a snapshot

**Dependencies**: Stage 8 and its measurement. **Implements**: D6's second
step, only if the gate fails.

**The measurement**, fixed now so that the gate is not set after the fact, is
an `edit_counts` mode, `EDIT_COUNTS_SCRIPT=blocked-edit`:

1. Open `E` and `L`, and warm the fold as the default script does.
2. Rewrite the `.fsproj` on disk with one more `<DefineConstants>` symbol, so
   the next fold re-evaluates the project and re-parses every item (no watch
   event; Stage 3b's validation sees it).
3. Send `semanticTokens L`. Without waiting for its answer, send
   `didChange E` (a comment keystroke) and `diagnostic E`.
4. Report the server instructions retired between `semanticTokens L` arriving
   and the answer to `diagnostic E`, beside the instructions of
   `didChange E` + `diagnostic E` measured on their own. The difference is
   what the edit waited for.

**The budget**: the edit waits for at most **50 M** server instructions. That
is about what one steady keystroke is expected to cost after Stages 1–5
(about 50 M), so an edit never waits much longer than the work it causes. The
gate is measured on macOS, where the instruction counter exists, under
`nix develop`, on WoofWare.Incremental and on WoofWare.PawPrint.Domain. The
re-fold in step 3 reads every file, so its count carries the kernel component
described under "Kernel instructions". The wait is taken as the minimum over
five runs, which compares the vnode-cached level.

**The gate**: if either project's wait exceeds the budget after Stage 8, this
stage is built. Otherwise it is not, and the measurement is recorded here.
On today's numbers the wait is a whole re-fold, so the gate fails unless
Stages 3b–7 make the post-save fold far cheaper than a cold one. Writing it
down now means measurement decides, not preference.

**Measured after Stage 8: the gate fails, so this stage is built.**
`EDIT_COUNTS_SCRIPT=blocked-edit` (`crates/lsp/examples/edit_counts.rs`)
does steps 1–4 as fixed above. It holds the server at the MSBuild evaluation
`semanticTokens L` makes, queues `didChange E` and `diagnostic E` then, so
the edit arrives while the request runs, and measures from before
`semanticTokens L` is sent to the settle after `diagnostic E`'s answer. Each
round adds the symbol to the `.fsproj` or takes it away again, so every
round re-evaluates; "alone" is the same edit and pull on their own, measured
after the round once the rewritten file is past the racy window. The
`.fsproj` is a private copy of each corpus project (the script writes it).
Five runs of five rounds each, under `nix develop` on macOS, the minimum of
each row:

| project | `semanticTokens L` .. `diagnostic E` | `didChange E` + `diagnostic E` alone | **the edit waited** | budget |
|---|---:|---:|---:|---:|
| WoofWare.Incremental (82 items) | 3,446.1 M | 3.7 M | **3,437.2 M** | 50 M |
| WoofWare.PawPrint.Domain (34 items) | 3,910.7 M | 8.6 M | **3,901.9 M** | 50 M |

Both exceed the budget by about 70 times. In every round the request
re-evaluates the project (115 files opened), builds an env (`a.built` 1)
and folds once from cold (`f.new` 1). The env is rebuilt because its read
set holds the entry project's `.fsproj` text (Stage 7's open gap: an
evaluation re-run to new defines rebuilds the env too), and that is most of
the wait. Without it the wait would still be a cold fold, which costs at least
0.62 G on WoofWare.Incremental (the cold pass's `semanticTokens L`, which
folds only the items after `E`, over a built env), twelve times the budget. Stage 8
cannot shorten either, since the edit arrives while the request runs.

The follow-up cutoffs on the evaluation's consumed outputs (above) remove
the env rebuild: the wait is now 995.1 M on WoofWare.Incremental and
814.4 M on WoofWare.PawPrint.Domain, still 20 and 16 times the budget.

The sketch, written before the measurement: a worker runs semantic requests
against a cloned `Vfs` and the memo tables behind a lock. The dispatch thread
applies edits and answers syntactic requests. A superseded request is
cancelled at its next memo boundary.

**The design: requests yield to notifications.** The measurement says what
the edit waits for: not work of its own (`didChange E` + `diagnostic E` cost
3.7 M), but the rest of a request that began before it. Stopping that request
at its next memo boundary removes the wait; running it beside the edit is not
needed for that. So this stage keeps one owner of the server's state and adds
the stop:

- **One owner.** The dispatch thread stays the only thread that reads or
  writes `State`: the `Vfs`, the workspace's evaluations and read checks, the
  parse memo, the envs, the folds. No memo table is shared, so no insert can
  race a read, and there is no snapshot to bound, clone or retire.
- **The input thread counts notifications** (`crate::inbox`). The loop reads
  its messages from a thread of its own, which takes each message from the
  transport the moment it is decoded into a channel of 1,024
  (`server::INPUT_BUFFER`, as Stage 8's input buffer did), and for each
  notification increments a shared counter *after* the message is in the
  channel. The channel and the counter are the only state two threads share.
- **A running request watches the counter.** Before each step the loop reads
  the counter and only then reads the waiting messages, so every
  notification counted by then is in the epoch the step is planned with. A
  request runs with a `Watch` holding the value read. At each checkpoint it
  compares the counter with it, and once it has moved, the request returns
  `Superseded`, which every handler and every fold entry point propagates
  (`Result<_, Superseded>`).
- **Checkpoints sit at memo boundaries.** No unit of work whose result is
  cached begins without one just before it: the parses (whose rebuild may
  evaluate the project again), each Compile item's parse, the env, the
  fold, the inference, each project `workspace/symbol` parses. One more
  comes after the handler has returned, before its answer is sent. So an abandoned run keeps every unit it
  completed, and the next run finds them in the caches.
- **An abandoned request is unanswered.** Its message goes back into the
  epoch, and the loop ends the generation and does nothing more for it: the
  response to a moved evaluation, the deferral report and the owed
  refreshes are derived from the state, and the step that applies the
  notification does them. It reads what has arrived and plans again. Stage 8's plan then
  decides the request's fate as before: refused `RequestCancelled` if a later
  cancellation names it, `ContentModified` if a later text sync changes its
  document, otherwise run again, after the new notifications.
- **The one change to Stage 8's plan: a re-run waits its turn.** A request
  abandoned with `n` messages in the epoch runs after every request among
  those `n`, not in its arrival position. The requests that arrived while it
  ran are the client's newer interest (`diagnostic E` after `didChange E`),
  and they are usually cheaper than the re-run.

**The guarantees**, on top of Stage 8's:

- Every request read before `exit` is answered exactly once. Only the loop
  sends a request's answer, after a run that was not superseded or as a
  refusal; an abandoned run sends nothing.
- A request is answered with a result only if no notification was counted
  between the moment the loop read the counter for its run and its last
  checkpoint, which follows the handler. Every notification counted before
  that read was planned before the run began, and is applied before it. So
  no answer carries a result computed against a text when the server had
  already read a newer one before deciding to send it.
- The notifications are still applied in arrival order, one at a time, so
  the state an epoch leaves is the state its messages leave handled one at a
  time (Stage 8).
- **Bounds.** One `State`, no snapshots. The input channel holds 1,024
  messages and the epoch 4,096 (Stage 8); a client further ahead waits in
  the transport. One request runs at a time. Past a notification's arrival,
  a running request does at most the rest of the unit it is in before it
  stops: one evaluation, one env build, one Compile item's parse, one fold,
  one inference, or the handler's own work after its last fold (classifying
  tokens, rendering a hover). Each is measured in `edit_counts`.
- **Progress.** A run is abandoned only when a notification arrives while
  it runs, and it keeps what it completed. A client whose notifications never
  pause starves its requests, as in Stage 8, now also mid-request.

**Why not the sketch.** A worker against a snapshot needs every memo table it
reads to be shared or copied. The workspace's read checks, the `Vfs`'s disk
cache and the env entries record, in `RefCell`s and `Cell`s, the generation
that last confirmed them, and each generation confirms its reads once. Two
generations in flight at once (the worker's and the edit's) would need a
confirmation per snapshot in every table, or a lock held for the whole
request, which puts the edit back behind it. Each would be a shared-state
race for every table, not one. What the sketch buys over this design is that
a request the edit does not supersede keeps running beside it. The cost of
giving that up is bounded: the abandoned run keeps its completed units, and
the re-run pays one unit at most again.

**Oracle**:
- `inbox::tests`: the protocol between the input thread and the loop
  (the push, the count, the loop's read of the counter, its read of the
  channel, the checkpoints and the answer), every interleaving of up to three
  notifications with a request's run, exhaustively: no answer with a result
  misses a notification counted before its last checkpoint, every request is
  answered once, every notification is applied once.
- `incremental_state_machine` with runs abandoned: a harmless notification
  sent at generated checkpoints makes requests yield and run again, and every
  answer still equals a fresh session's.
- The Stage 8 interleaving property, with messages delivered at checkpoints
  inside running requests, not only between steps.
- `the_plan_is_its_specification`, with re-runs.
- A test that a request stops before it begins another unit, wherever the
  notification arrives, and one that the edit's pull is answered before the
  request it interrupted.
- `blocked-edit`, measured on both projects.

**Implemented** (`crates/lsp/src/inbox.rs`, `crates/lsp/src/server.rs`,
`crates/lsp/src/batch.rs`, `crates/lsp/src/semantic.rs`, the handlers):

- **The inbox.** `inbox::forward` is the input thread: it takes each message
  from the transport into a channel of 1,024 and counts each notification
  after the message is in the channel. `server::run_with_fetcher` starts it,
  so `main` no longer buffers its input itself; `server::run_inbox` is the
  loop over an inbox, which tests and `edit_counts` drive through an
  `InboxSender` of their own. The handover is a description
  (`inbox::handover`: push, then count), which `InboxSender::send` performs
  and the interleaving test schedules action by action.
- **The step.** `Dispatcher::step` reads the counter, then what is waiting
  (`Inbox::begin_step`), plans, and runs the next step; a request runs with
  the `Watch` holding the count read.
- **The checkpoints.** `Watch::check` emits `inbox::CHECKPOINT_EVENT`, then
  compares. The fold's entry points have watched variants
  (`SemanticState::parses_for_project_watched`,
  `resolved_prefix_and_env_watched`, `inferred_prefix_watched`) that check
  before returning a cached prefix, before the parses, before each Compile
  item, before the env, before the fold and before the inference, each unit
  cached before the next checkpoint. The handlers that fold (`hover`, `definition`,
  `references`, `completion`, `semanticTokens`, `workspace/symbol`) take a
  watch (`handle_watched`), find the owning project through
  `Workspace::owning_project_watched` (a checkpoint before each evaluation
  the walk may run: each project a remembered answer consulted, each
  candidate it weighs) and check after it; the loop checks once more after
  the handler returns. Every other caller keeps
  the unwatched entry points, which no checkpoint can stop
  (`inbox::never_superseded`). Hover computes its project prefix once
  (`FilePrefix`) and hands it to both of its project paths.
- **A stop.** A handler's `Superseded` reaches `Dispatcher::execute`, which
  puts the request back into the epoch unanswered,
  ends the generation, emits `inbox::STOPPED_EVENT` and returns
  `Progress::Stopped`, leaving the moved evaluations' response, the
  deferral report and the owed refreshes to the next step (each step that
  applies a notification does them), so that they cannot delay the
  notification: re-walking every open buffer's owner after a moved
  evaluation can evaluate other projects (found by the latency sweep's
  three-project scenario). The next step reads what arrived and
  only then records the request as a `batch::Restart` with the size of the
  epoch, so the requests that arrived with the notification run before it.
  A stopped fold records no outcome. When it stops a request, the loop owes
  a semantic-token refresh for every fold found stale and not yet rebuilt
  (`SemanticState::settle_stale_folds`), wherever the stop came, since a
  request stopped by its own cancellation is refused and no rebuild may
  finish. It owes it without deciding, as the rebuild would, whether a
  buffer announced the change: that would read every remaining item before
  the notification is handled. So a stop can owe one refresh more than the
  finished request would, never one fewer. The step that
  applies a cancellation responds to moved evaluations, as every other step
  that applies a notification does.
- **The plan** takes the restarts (`batch::plan`'s `restarts`): a restarted
  request runs after every request the epoch held when it stopped and
  before any read later, and its fate is still decided by what arrived after
  it.

The oracle, as built:

- `inbox::tests::every_interleaving_answers_after_every_counted_notification`:
  the input thread's actions (`inbox::handover`'s, performed by the real
  `InboxSender`) against the loop's (the real `Inbox::begin_step`, each
  notification applied in its own step, a run of three checkpoints), every
  interleaving of up to three notifications with one request, by an explorer
  that replays a prefix of choices: one answer, computed after every
  notification counted before its last checkpoint; every notification
  applied once, in order; some schedules stop the run and some do not. Its
  first version passed both orders it exists for (a count before the push,
  the counter read after the channel): it performed the push and the count
  itself rather than as `send` describes them, and had no point after the
  last, empty, read of the channel. Both are caught now.
- `inbox::tests::a_full_inbox_holds_the_sender_back` and
  `dispatch_coalescing::the_epoch_holds_at_most_its_bound`: the two queues
  are bounded.
- `incremental_state_machine::a_session_whose_requests_stop_agrees_with_a_fresh_one`:
  the cases of `warm_session_agrees_with_a_fresh_one`, with the long-lived
  session's requests stopping at up to sixteen checkpoints drawn from a hash
  of the case (a notification the server ignores, sent from a subscriber
  before the checkpoint reads the count). Every answer and everything the
  client displays equals fresh sessions'. The default run stops 63 runs.
- `dispatch_coalescing::a_coalesced_batch_agrees_with_its_messages_one_at_a_time`,
  Stage 8's property, with a third delivery: a prefix handed over, and the
  rest sent through the inbox at a generated checkpoint of whichever request
  runs then. Exactly once, the final state, and, for every request answered
  after the rest arrived, the whole batch's fate and the sequential
  session's answer after the whole batch. Its non-vacuity check asks for a
  batch in which a run stopped and an answer with a result after the
  arrival.
- `dispatch_coalescing::a_request_stops_for_a_notification_wherever_it_arrives`:
  the cancellation latency in units. A notification sent as any span of the
  request's handler begins (the handler's own, each unit's, every span
  inside them) stops the request once, before it begins another unit
  (`evaluate_project`, `parse_compile_item`, `build_assembly_env`,
  `resolve_project`, `infer_project`) before the stopped step ends, and the
  request then answers as it does unstopped. Swept over every such span of a
  cold hover, cold references, cold semantic tokens, a hover whose fold is
  cached and whose inference is not, `workspace/symbol` over two projects,
  `workspace/symbol` over three whose project files were all rewritten, and
  a hover whose ownership walk weighs three rewritten projects. Its first version sent the
  notification only as a unit began, over the three cold requests; Codex
  found two units it let begin after a notification (the next project's
  first parse in `workspace/symbol`, and cold inference after a cached
  prefix) and, in its third round, a third (the next ownership walk's
  evaluation in `workspace/symbol`), in its fourth a fourth (the next
  candidate of one ownership walk); and the plant of a missing checkpoint
  after the handler passed it once every unit was followed by one. Each was
  a scenario the sweep did not have, so each is now one of its scenarios.
- `dispatch_coalescing::an_edit_that_arrives_mid_request_is_answered_before_it`:
  the gate in small. A change to `A` and a pull of `A` arrive at a hover on
  `C`'s first checkpoint; the hover stops, the pull is answered first, and
  the hover after it, with the type the new `A` gives.
- `dispatch_coalescing::a_stopped_rebuild_still_owes_the_refresh_for_an_unannounced_change`
  (added for a plant that passed everything else).
- `the_plan_is_its_specification`, with up to four restarts per epoch, and
  `a_restarted_request_waits_behind_the_requests_read_while_it_ran`.
- The loop tests that queue a script before the loop starts send it into the
  inbox itself (`inbox::channel`), so all of it is waiting when the loop
  starts, as before. `the_loop_reads_again_before_each_message_it_handles`
  hands its late change to the inbox between two steps of a `Dispatcher`,
  not from a `tracing` subscriber at the end of the first generation, which
  under parallel tests could miss that event (Codex saw it fail once in
  three runs). Every test that installs a `tracing` subscriber does so
  through `common::with_subscriber`, which holds one lock across the scope:
  `tracing` recomputes its process-wide interest and maximum level as
  dispatchers come and go, and two recomputations racing can leave a live
  subscriber seeing nothing (the reason the observed counts are read from a
  log, not from events). Five consecutive runs of the three groups that
  install one pass.

Planted, each caught, then removed (each run against the `inbox` and
`batch` unit tests, `dispatch_coalescing`, `workspace_diagnostic_sweep`,
and the state machine's warm and stopping properties):

| plant | caught by |
|---|---|
| no cancellation (a checkpoint never sees the count move) | the exhaustive and full-inbox tests, the coalescing property, the latency sweep, the gate in small, the stopping state machine |
| a stale answer: no checkpoint after the handler | the latency sweep, once it covered every span (a notification during references' scan) |
| no checkpoint before a project's first parse (Codex) | the latency sweep, once it covered `workspace/symbol` over two projects |
| no checkpoint between a cached prefix and the inference (Codex) | the latency sweep, once it covered a hover over a cached fold |
| no checkpoint between `workspace/symbol`'s ownership walks (Codex) | the latency sweep, once it counted evaluations as units and covered three projects that all evaluate again |
| a stopped run doing its follow-ups (a moved evaluation's response, the deferral report) before its step ends | the same scenario: re-walking every open buffer's owner evaluates the other two |
| a stopped fold's partial parses cached as the project's | the latency sweep, the stopping state machine |
| the stale parses lost on a stop (the rebuild's refresh decision) | `a_stopped_rebuild_still_owes_the_refresh_for_an_unannounced_change` (added for it: it passed everything else) |
| the stale parses kept for a rebuild that a cancellation refuses (Codex, rounds 4 and 6: during the rebuild, then before it) | the same test, once it cancelled the request it stopped, and once it swept the stop over every span of semantic tokens and references |
| no settling of stale folds at a stop | the same sweep |
| settling by reading every remaining item at the stop (Codex, round 7: the notification waits for the project's reads) | the reads were deleted; the refresh is owed without them |
| the cancellation's step not responding to a moved evaluation a stopped request left (Codex) | `a_request_cancelled_mid_evaluation_still_reports_the_moved_evaluation` (added for it) |
| no checkpoint between the candidates an ownership walk weighs (Codex) | the latency sweep, once it covered a directory of three projects that all evaluate again |
| an unbounded inbox | `a_full_inbox_holds_the_sender_back` |
| an unbounded epoch | `the_epoch_holds_at_most_its_bound` |
| a stopped request answered as well as run again | the coalescing property, the latency sweep, the gate in small, the stopping state machine |
| a stopped request refused rather than run again | the same four |
| a restart in its arrival position (no `Restart`) | the gate in small |
| a restart's place taken before reading what arrived | the gate in small |
| no checkpoint after the env | the latency sweep |
| no checkpoint between Compile items | the latency sweep |
| no checkpoint after the fold | nothing, once every unit was preceded by a checkpoint: it guarded no unit (the next is preceded by its own) and no answer (the loop's checks it), so it was deleted, with the one after the inference |
| the count before the push | the exhaustive and full-inbox tests |
| the counter read after the channel | the exhaustive test |

The last two passed the first version of the exhaustive test (above).

**Measured** on the build box, under `nix develop`, each row the minimum
over the runs of each binary: Stage 8's head (e1726d32, the content of
1e162da1) with this stage's `edit_counts`, and this stage. Instructions are
counted on every server thread: the process's retired instructions
(`proc_pid_rusage`'s `ri_instructions`, which keeps the counts of threads
that have exited, the env build's DLL readers among them) less the client
thread's own (`thread_selfcounts`). The client hands its messages to the
inbox itself, so no input thread runs; in the server that thread moves one
message per channel operation. The keystroke script ran 20 keystrokes per
run, the bursts 5 of 4 and 3 of 16, three runs each; `blocked-edit` five
rounds per run, five runs. The on-disk assembly cache is off.

| message, per keystroke (WoofWare.Incremental) | instructions 8 | 9 | allocs 8 | 9 |
|---|---:|---:|---:|---:|
| `didChange E` | 0.11 M | 0.13 M | 64 | 72 |
| `diagnostic E` | 3.57 M | 3.57 M | 271 | 284 |
| `diagnostic L` (`unchanged`) | 2.77 M | 2.75 M | 108 | 121 |
| `semanticTokens E` | 9.74 M | 9.78 M | 5,033 | 5,044 |
| `semanticTokens L` | 13.57 M | 13.59 M | 11,474 | 11,485 |
| `hover L` | 8.89 M | 8.92 M | 3,918 | 3,945 |
| **keystroke total** | **38.69 M** | **38.81 M** | 20,868 | 20,951 |

A keystroke stops nothing (the client waits for each answer before it
sends more), so it pays the checkpoints alone: each step reads the counter
and allocates the batch it read, and each request is cloned before it runs,
in case it stops. 0.3% in all.

| burst, whole | instructions 8 | 9 | allocs 8 | 9 | answered with a result | `ContentModified` |
|---|---:|---:|---:|---:|---:|---:|
| 4 keystrokes | 89.6 M | 89.4 M | 29,800 | 29,957 | 14 | 6 |
| 16 keystrokes | 300.1 M | 302.4 M | 76,615 | 77,104 | 50 | 30 |

A burst is queued before the loop reads it, so it too stops nothing, and is
planned as in Stage 8 (one fold, one parse).

`blocked-edit`, from before `semanticTokens L` is sent to the arrival of
`diagnostic E`'s answer:

| project | window 8 | 9 | of which before the edit 8 | 9 | after the edit, less alone, 8 | 9 | alone 8 | 9 | **the edit waited** 8 | **9** | budget |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| WoofWare.Incremental | 3,478.0 M | 384.5 M | 297.6 M | 300.0 M | 3,125.8 M | 79.7 M | 3.9 M | 4.0 M | 3,436.2 M | **379.8 M** | 50 M |
| WoofWare.PawPrint.Domain | 3,950.3 M | 394.3 M | 300.9 M | 299.7 M | 3,630.5 M | 43.6 M | 8.8 M | 8.9 M | 3,941.5 M | **356.4 M** | 50 M |

In every round the request stopped once, and `diagnostic E` was answered
before it. **The gate as the plan fixed it still fails, by about 7 times.**
What the window now consists of, on WoofWare.Incremental (attributed by
`EDIT_COUNTS_GENERATIONS` and, once, by probes at the evaluation's steps):

- **300 M before the edit was sent**: the request's work up to the
  evaluation the script holds it at, nearly all of it the re-evaluation of
  the rewritten `.fsproj` (115 files opened). The edit had not been sent
  then, so it cannot have waited for it, but the plan's window starts when
  `semanticTokens L` arrives; nothing this stage does reaches it.
- **31 M, the rest of the unit the request was in**: after the evaluation
  returns, building its checks (`Evaluation::checks_and_exempt`) costs 31 M
  before it is cached and the next checkpoint is reached. This is the
  cancellation latency the gate meets.
- **45–54 M, the first confirmation of the new evaluation**, which
  `diagnostic E` pays: a fresh evaluation's checks have no witnesses yet, so
  the first generation after it asks each read again (2,262 `stat`s, 32
  listings, 31 canonicalisations) where later ones `stat` 135 paths. Stage 8
  paid it inside the request.
- **4 M, the edit and the pull themselves**, as alone.

So the part after the edit is under the budget on WoofWare.PawPrint.Domain
(43.6 M) and over it on WoofWare.Incremental (79.7 M), and the window is
over it on both because of work done before the edit was sent.

The tables were taken at d2b7f874 (main merged). `blocked-edit` was taken
again at 282c5e50, after Codex's rounds added checkpoints (before the
inference, before a cached prefix is returned, before each project's
parses, before each candidate of an ownership walk) and moved a stopped
run's follow-ups to the next step, on a box running the full gate at the
same time: window 381.1 M and 392.2 M, the edit waited 377.1 M and 383.0 M,
of which before the edit 294.2 M and 298.7 M. The part after the edit is
the window less two readings taken at different moments of different
rounds, so its minimum moves between runs (43.6 M and 83.9 M on
WoofWare.PawPrint.Domain); the window and the wait are the gate's figures.
The final head differs from 282c5e50 only in what a stop settles, which
in this script is nothing (the evaluation moved, so no fold is kept
stale).

**With the cutoffs** (the follow-up above, merged at db3128a9), measured
on the box the same way: the keystroke script and the bursts three runs
each, `blocked-edit` five, each row the minimum. Stage 9 alone and the
cutoffs alone (7547cb2a, with this stage's harness) beside them:

| per keystroke (WoofWare.Incremental) | Stage 8 | Stage 9 | combined |
|---|---:|---:|---:|
| `didChange E` | 0.11 M | 0.13 M | 0.01 M |
| `diagnostic E` | 3.57 M | 3.57 M | 3.57 M |
| `diagnostic L` | 2.77 M | 2.75 M | 2.77 M |
| `semanticTokens E` | 9.74 M | 9.78 M | 9.77 M |
| `semanticTokens L` | 13.57 M | 13.59 M | 13.58 M |
| `hover L` | 8.89 M | 8.92 M | 8.93 M |
| **total** | **38.69 M** | **38.81 M** | **38.78 M** |

| burst, whole | Stage 8 | Stage 9 | combined |
|---|---:|---:|---:|
| 4 keystrokes | 89.6 M | 89.4 M | 89.1 M |
| 16 keystrokes | 300.1 M | 302.4 M | 299.4 M |

| `blocked-edit`: the edit waited | Stage 8 | cutoffs alone | Stage 9 alone | **combined** | budget |
|---|---:|---:|---:|---:|---:|
| WoofWare.Incremental | 3,436.2 M | 990.7 M | 379.8 M | **377.5 M** | 50 M |
| WoofWare.PawPrint.Domain | 3,941.5 M | 825.9 M | 356.4 M | **379.3 M** | 50 M |

The combined window is 381.3 M and 388.0 M, of which 294.5 M and 295.0 M
come before the edit is sent; the request stopped once in every round, and
`diagnostic E` was answered first. Together the two cut the wait to about
a ninth of Stage 8's and well below the cutoffs alone, but not below Stage 9
alone: what the cutoffs remove (the env rebuilt after the defines moved) is
work a stopped request no longer does before the edit is handled. What is
left is what Stage 9 alone left: the re-evaluation before the edit is sent
(about 295 M), the rest of the evaluation unit the edit arrives in (31 M,
`checks_and_exempt`), and the new evaluation's first confirmation, which
`diagnostic E` pays (2,397 and 2,448 `stat`s). The gate fails by about 7
times.

**Departures.**

- **No worker and no snapshot.** The plan sketched a worker running
  semantic requests against a cloned `Vfs` and the memo tables behind a
  lock. This stage keeps the one owner and stops the request instead (the
  design's "Why not the sketch"). The oracle's "interleaving test of the
  memo table's insert-versus-read race" is accordingly an exhaustive test
  of what is shared: the inbox's channel and counter.
- **The measurement's end.** The plan's window ends at the answer to
  `diagnostic E`; Stage 8's harness read the counters after both answers and
  a settle, which on Stage 8 is the same moment, since `diagnostic E` was
  answered last. It now reads them as `diagnostic E`'s answer arrives, as the
  plan defines, and also reports the split at the edit.
- **`edit_counts` hands its messages to the inbox**, so it runs no input
  thread, and a message is in the inbox, and a notification counted, when
  its send returns; the baseline's harness writes to Stage 8's connection,
  which its loop reads directly.
- **Every notification stops a running request**, not only a text sync: the
  input thread cannot know what the request reads, and a cancellation of
  the running request should stop it too. A notification that changes
  nothing the request read costs the rest of a unit.
- **A re-run waits behind the requests that arrived while it ran**, a change
  to Stage 8's plan.
- **The loop's input thread moved into `run_with_fetcher`** from `main`
  (`inbox::forward`, which replaces `server::buffer_input`).

**Open gaps.**

- The gate fails: the window is 384.5 M and 394.3 M against 50 M. 300 M of
  it is the re-evaluation, done before the edit was sent.
- A fresh evaluation's first confirmation asks every read again (2,262
  `stat`s, 45–54 M on WoofWare.Incremental), because its checks are taken
  with no witnesses; the request that would have paid it stopped, so the
  edit's pull pays it.
- Building a fresh evaluation's checks (`Evaluation::checks_and_exempt`)
  costs 31 M after the evaluation itself, within the unit, so before the
  next checkpoint.
- An edit that arrives during an env build waits for the rest of it (up to
  2.7 G on WoofWare.Incremental, cold), and one that arrives during a cold
  fold or inference for the rest of that (0.6 G and 0.1 G): each is one unit.
- An edit waits in full for a notification ahead of it (a `didOpen` that
  evaluates a project, about 0.3 G); only requests stop.
- A client whose notifications never pause starves its requests, now also
  mid-request: each stops at its next checkpoint.
- A notification that cannot change what a request read (a cancellation of
  another request, `didSave`) stops it all the same.
- A `workspace/diagnostic` sweep in flight is not stopped (as in Stage 8).
- A stop that finds a fold stale owes a semantic-token refresh even when an
  open buffer's sync already announced the change: one refresh more than
  the finished request would owe.

### Follow-up: a new read set starts with the witnesses it shares

**Dependencies**: Stage 9. **Closes**:
[#733](https://github.com/WoofWare/borzoi/issues/733) and
[#742](https://github.com/WoofWare/borzoi/issues/742).

After a re-evaluation or a re-resolution, the new read set's checks were
made without witnesses, so its first confirmation walked every read's path
again (an `lstat` per component) and asked it again: 2,239 `stat`s, 32
listings and 31 canonicalisations for the evaluation on
WoofWare.Incremental, 2,158 `stat`s for the env's resolution, about 50 M
instructions each. And building the evaluation's checks
(`Evaluation::checks_and_exempt`) cost 31 M, the cancellation latency
Stage 9's `blocked-edit` meets.

**A witness belongs to a read.** A witness says that while its
directories keep their stamps, the query gets the answer it got when the
witness was taken, and it is kept only if that answer was the recorded one.
Nothing in that names the evaluation or env that recorded the read. So a
witness carries over to another read exactly when the two reads are equal:
the same path, spelt the same (`Read`'s equality is on the raw spelling),
and the same recorded answer, under the same checker. It carries its
stamps, the time of the generation that took them and the watch events it
had seen, so the racy rule and the hints judge it as they judged it for the
read it was taken for. Its stamps are compared with the ones the
directories have in the generation that confirms it, as a fresh witness's
are; a moved stamp sends the read back to be asked, as for any check.

`ReadChecks::check` makes that structural: a checker hands out one `Check`
per distinct read, shared (an `Rc`) by every holder while any holds it, from
an index of weak handles keyed by a hash of the read and confirmed by
equality, swept once released entries outnumber held ones. A witness names
the checker that took it, so a check confirmed through another checker asks
again. The workspace holds a dropped evaluation's checks until the project
is evaluated again (`Workspace::retired`, at most one entry per project), so
the new evaluation's equal reads take them over; the env's re-resolution
runs while its old checks are held, and its reads of the project's
evaluation are the evaluation's own, so it shares both.

**Building checks cheaply.** Profiled with markers on the build box,
`checks_and_exempt` spent its 31 M mostly placing reads among the installed
versions: each read's version was found twice, each time spelling the path
again and comparing it with every root and alias component by component,
and the versions shown were kept in a set ordered by components. It now
finds each read's version once, places a path spelt as its components by
its bytes (with the version directory a leading part of it, so nothing is
allocated), keys the versions by spelling, asks for a package's commit
marker once per version, and partitions by index rather than by hashing
every read. A reference implementation of the old placement and the old
partition is kept in the tests, and the new ones are held to it.

Building a fresh evaluation's checks, measured between markers either
side of `checks_and_exempt` in `blocked-edit` on WoofWare.Incremental
(minimum over two runs of two rounds): **30.3 M on `main`, 5.7 M here**.

**The guarantee.** Unchanged, and stated per check: a check holds in a
generation only if its read's query gets the recorded answer as of that
generation, whether the witness it holds was taken for it or for an equal
read. Stage 3b's racy rule, Stage 6b's digest rule for racy stamps (the
settled stamp a racy whole-file read confirms by is shared the same way),
the immutable-install exemption and the directory-witness rules are
untouched; only who holds a witness changed.

Oracle:

- `read_checks::tests::a_shared_witness_accepts_exactly_when_a_fresh_confirmation_would`:
  over the model file system, generated changes (stealthy ones with their
  hints, interleaved ones, clocks of every granularity) and records of the
  read sets again (inside a generation or between, with the old checks held
  or released first), by three holders, one confirmed through a second
  checker: in every generation no change lands inside, each check accepts
  exactly when the query, asked of the disk now, gets the answer recorded;
  in one a change lands inside, only when the answer was right as it began
  or is now. Every record asserts that the check it is handed is a check of
  its own read. Its generator check asks for shared checks, listings and
  canonicalisations confirmed by a shared witness in their first generation,
  and witnesses that found the disk moved.
- `workspace::tests::validation::a_project_evaluated_again_starts_with_the_dropped_ones_witnesses`
  and `semantic::tests::an_env_resolved_again_starts_with_the_witnesses_it_shares`:
  after an edit that moves nothing else (a comment, written in place), the
  new evaluation's and the new resolution's first confirmations cost exactly
  what a steady confirmation does. Both fail on Stage 9's head. Their
  absence probes reach the temporary directory's ancestors, which
  concurrent tests change, so each runs until a run sees nothing else move.
- `a_version_is_found_as_the_reference_finds_it` and
  `reads_are_partitioned_as_the_reference_partitions_them`: the placement
  and the partition against their first versions, over `.` and `..`,
  doubled and trailing separators, a root not spelt as its components,
  aliases, failed and absent reads and uncommitted packages.

Planted, each caught, then removed (each run against the `read_checks`,
`workspace` and `semantic` unit tests):

| plant | caught by |
|---|---|
| a check found by hashing the path alone and matching it, so a read is handed another read's check | the model property (its record's own-read assertion) and its generator check, both count tests, `outside_a_generation_every_ask_walks`, `the_generator_reaches_witnessed_and_moved_checks` |
| a witness confirmed through a checker that did not take it (no checker guard) | the model property (holder 2) and its generator check |
| the index sweeping entries still held | both count tests, the generator check |
| a dropped evaluation's checks not held (no retirement) | the evaluation count test |
| the held checks released before the new ones are made | the evaluation count test |
| the env's checks made outside the checker's index | the env count test |
| a root matched as a byte prefix without its separator | both reference properties, once their generators had near-miss spellings (`/dd`, `/nn`); before, only an unrelated test, by chance |
| a `.` name taken as spelt as components | the placement reference property |
| a package version trusted without its commit marker | `a_package_is_trusted_only_once_committed_before_the_evaluation` |
| a version shown by a read that failed | `a_version_installed_after_the_evaluation_is_seen` |
| a failed read inside a version exempt | `a_failed_read_inside_a_version_is_confirmed` |
| an alias found by bytes under a root not spelt as its components (Codex) | both reference properties, once the references found their own aliases and a root could end in a separator |

Codex found that an install root ending in a separator lost every alias of
its versions (so their contents were confirmed in every generation rather
than trusted): the alias search placed a canonicalisation by its bytes
without asking whether the roots were spelt as their components. The
reference properties had passed it because the placement reference took
its aliases from the code under test, and no generated root of the alias
was spelt otherwise; now the references find their own aliases, and the
roots vary.

The first version of the path-only plant changed only the match, inside
buckets keyed by the whole read, and so changed nothing; it was replanted to
hash the path alone, and then caught. Two plants pass every test, and are
not defects: refreshing a shared witness's time to the generation that
shares it, or its hint count to the checker's. Both stand on the record
itself, which observed the same answer at that moment: a change after it
either lands in a later tick than the witness's stamps (which the refreshed
racy test then demands) or is hinted after the refreshed count. Neither is
adopted, since neither is needed for the carry-over the issues ask for.

**Measured** on the build box under `nix develop`, each row the minimum
over the runs of each binary: `main` at e853ad8b (Stage 9 squash-merged)
and this follow-up. The keystroke script ran 20 keystrokes per run, three
runs; `own-comment` and `unrelated-save` five rounds per run, three runs
(on a private copy of the project); `blocked-edit` five rounds per run,
five runs, on both projects.

`own-comment` on WoofWare.Incremental, per round:

| message | instructions `main` | this | `stat`s `main` | this | what it does now |
|---|---:|---:|---:|---:|---|
| `didChangeWatchedFiles` | 336.3 M | **305.9 M** | 60 | 60 | evaluates the project and builds its checks |
| `diagnostic E` | 52.7 M | **4.2 M** | 2,239 (32 listings, 31 canon.) | 197 (4 canon.) | the new evaluation's first confirmation |
| `diagnostic L` | 3.0 M | 2.8 M | 135 | 135 | |
| `semanticTokens E` | 53.3 M | **24.5 M** | 135 + 218 | 135 + 172 | resolves the env's references again and keeps it |
| `semanticTokens L` | 58.1 M | **7.2 M** | 135 + 2,158 (33 listings, 28 canon.) | 135 + 24 (1 canon.) | the new resolution's first confirmation |
| `hover L` | 6.9 M | 6.7 M | 135 + 10 | 135 + 10 | |
| **round** | **516.4 M** | **351.5 M** | 2,839 + 2,386 | 797 + 206 | |

(`stat`s are the evaluation's confirmation, then the env's.) What the
first confirmations still ask is what the event made them ask: the watch
event names the project file, which hints its directory, so every witness
through it is asked again, as a hint requires.

| per round of | instructions `main` | this | allocs `main` | this |
|---|---:|---:|---:|---:|
| a keystroke | 36.45 M | 35.69 M | 20,598 | 20,598 |
| a save of an unrelated project's `.fsproj` | 47.89 M | 47.55 M | 7,296 | 7,282 |

Every keystroke and save row's `stat`s are unchanged (136 per message that
reads the evaluation, 10 for the env). The first `unrelated-save` runs of
this follow-up were taken while another job's `nix develop` copied a dirty
tree into the store: `/nix/store`'s stamp moved under every witness through
it, and every message asked those reads again (682 M a round). The runs in
the table were taken on a quiet box, interleaved with `main`'s.

`blocked-edit` (Stage 9's gate):

| project | window `main` | this | the edit waited `main` | **this** | `v.stat` in the window, `main` | this | budget |
|---|---:|---:|---:|---:|---:|---:|---:|
| WoofWare.Incremental | 399.1 M | 329.0 M | 369.1 M | **285.7 M** | 2,397 | 293 | 50 M |
| WoofWare.PawPrint.Domain | 403.3 M | 327.2 M | 366.3 M | **275.3 M** | 2,448 | 308 | 50 M |

Of the window, about 305 M comes before the edit is sent (the
re-evaluation), as before. What the edit meets after it is the rest of the
evaluation's unit (the checks) and the first confirmation (now about
3.5 M). The gate still fails: what is left is the re-evaluation
itself, which the edit cannot have waited for and which the plan's window
counts all the same.

Some rounds still pay a full first confirmation (the maximum of the edit's
wait stays near 380 M): `/nix/store` is one of the directories every
witness of the SDK's reads names, and a witness whose stamp of it is
within the racy window of the generation that took it, or that the store
has moved since, asks again. On the box that happens whenever any job adds
a store path. It is the directory-witness rule working as it must, and not
something carrying a witness over can save.

**Open gaps.**

- An env built afresh (a moved DLL, a new key, references that resolve to
  other inputs) starts with only the witnesses its reads share with checks
  still held: the evaluation's, never the dropped env's.
- A watch event naming the project file hints its directory, so the reads
  through it are asked again in full however little moved.
- On a Nix install, any store write moves `/nix/store`, under every
  witness of a read through it.
- Stage 9's gate fails, on the re-evaluation before the edit.

## Non-goals

- **Semantic diagnostics.** The server publishes syntactic diagnostics only.
  This plan makes the edit loop cheap enough that adding them later does not
  multiply today's cost. It does not add them.
- **Incremental parsing.** Re-parsing `E` after a keystroke costs under 5 M.
  Stage 5 removes the second parse, and the first is not worth a
  tree-diffing parser.
- **Incremental text sync.** The server advertises `FULL` sync. A full text
  per keystroke costs the client's copy and one `Arc<str>`, which is small
  beside everything above.
- **Persisting derived values across restarts**, beyond the existing assembly
  cache.
