# An incremental LSP: file state, derived values and the edit loop

> **Status (2026-11-13).** Plan written; Stage 0 (the edit-loop count harness),
> Stage 1 (the inference cutoff), Stage 2a (counted source reads), Stage 2b
> (the `Vfs` owns source text) and Stage 3a (the MSBuild evaluation records
> its read set) implemented. Tracks
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
  names and entry types in the host's order, a canonical path, or a
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
| `didOpen E` (cold, one evaluation) | 320.9 M | 332.8 M | 102,897 | 105,900 |
| `semanticTokens E` (cold, one evaluation) | 3,078.8 M | 3,095.3 M | 3,516,099 | 3,519,117 |
| `semanticTokens L` (cold) | 598.7 M | 598.5 M | 377,500 | 377,500 |
| `hover L` (cold) | 123.7 M | 123.7 M | 147,139 | 147,139 |
| keystroke total | 37.12 M | 37.12 M | 28,403 | 28,403 |

Every keystroke row is unchanged, allocations exactly. Recording costs each
evaluation about 12 M instructions and 3,000 allocations, 3.7% of the
evaluation: one path per call (756 calls) and the listings' names, kept
until the evaluation ends and de-duplicated into the read set. The cold rows
that read files are kernel-heavy; both binaries' runs agreed to within 2%.

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

If built: a worker runs semantic requests against a cloned `Vfs` and the memo
tables behind a lock. The dispatch thread applies edits and answers syntactic
requests. A superseded request is cancelled at its next memo boundary.

**Oracle**: `incremental_state_machine` with the worker enabled, a loom-style
or exhaustive interleaving test of the memo table's insert-versus-read race,
the Stage 8 interleaving property, and the `blocked-edit` measurement under
budget.

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
