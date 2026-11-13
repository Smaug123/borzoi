# Interned names, paths and types in sema

> **Status (2026-11-12).** Plan written; Stages 0 (count harness), 1 (the
> hasher, [#577](https://github.com/WoofWare/borzoi/pull/577)), 2 (memoised
> namespace fold surfaces), 3 (the name table), 4 (the open-prefix × head
> product on ids) and 9 (`OpenScope`) implemented.
> Tracks [#543](https://github.com/WoofWare/borzoi/issues/543). Stacked on the
> flat syntax table (`docs/flat-syntax-tree-plan.md`), whose stack tip is
> where every count below was taken.

## Problem

After the flat syntax table, syntax-tree code is gone from sema's profile, and
what is left is sema's own representation. The issue lists it:

- a name is a `String` (`Def::name`, every scope and index key), and a path is
  a `Vec<String>` (namespaces, modules, export paths, open prefixes);
- a qualified use builds `O(opens × segments)` strings: for each open prefix
  in scope, `value_slot` clones the prefix, pushes the head and hashes the
  result to probe a map keyed by `Vec<String>`;
- `Ty` is an owned tree whose named nodes carry a `Vec<String>` path, cloned
  deeply wherever a type is copied;
- every sema map uses std's default SipHash (459 source lines mention
  `HashMap` or `HashSet`);
- `Resolver` has 107 fields with no grouping by lifetime;
- the walk's opacity flags are loose booleans, saved and restored by hand.

### Measurements

Stage 0's harness (`crates/sema/examples/sema_counts.rs`) on the stack tip
(b6db82f2), over the corpus's `src/` tree (259 files, 11.8 MB), each file
resolved and inferred on its own against the BCL, min of 3. Instruction and
allocation counts are the measure; the machines are loaded, so wall time is
printed for orientation only.

| phase | instructions / byte | allocations / byte | reallocations / byte | bytes allocated / byte |
|---|---|---|---|---|
| `resolve_file` | 2639.8 | 2.268 | 0.384 | 107.5 |
| `infer_file` | 187.2 | 0.128 | 0.008 | 10.0 |

Parse is about 546 instructions per byte (`docs/token-pipeline-soa-plan.md`),
so resolve is 77–78% of the pipeline and infer about 6%. Resolve makes 26.8 M
allocations and 4.5 M reallocations over the tree, 5.9 per syntax element.
Since this stack was cut, main has merged an operator-name fast path (#580,
about −290 instructions per byte on resolve), indexed forwarder lookups
(#584), and linear-memory nested modules (#586). The stages below rebase onto
main before they land.

An xctrace Time Profiler run of the same workload attributes resolve's self
time to malloc/free 23.9%, SipHash 9.8%, `memcmp` 8.4%, `memmove` 4.5% and
hashbrown 2.0%; inclusive shares are SipHash 17.8%, `String::clone` 9.8%, `Vec`
growth 9.0% and hashbrown 7.7%. The hot functions are `value_slot` (15.2%
inclusive), `head_contested_by_project_module` (11.5%, a quadratic being fixed
separately, which this plan does not count on), `open_namespace_fold_surfaces`
(8.4%) and `AssemblyEnv::types_named` (4.8%). Infer's profile is malloc/free
12.3%, hashbrown 9.1%, SipHash 2.6%.

Two spikes, both throwaway, measure the first two stages directly:

| variant | `resolve_file` instr/B | allocs/B | `infer_file` instr/B |
|---|---|---|---|
| stack tip | 2639.8 | 2.268 | 187.2 |
| every sema map on `rustc-hash` 1.1's `FxHasher` | 2255.1 (−14.6%) | 2.268 | 172.4 (−7.9%) |
| `open_namespace_fold_surfaces` memoised per namespace, two query sites reading the memo | 2502.3 (−5.2%) | 2.211 | 186.9 |

The harness also prints a census of what the new representations would hold:

- 686,222 identifier tokens with 40,313 distinct spellings: an interner over
  the whole tree holds tens of thousands of names, under a megabyte.
- 131,429 definitions with 32,447 distinct names. Each `Def::name` is the
  text of its defining token's range except for 1,487 active-pattern
  recognizers and 2 union cases. Definitions' names are 0.5% of resolve's
  allocations.
- 4,547 published inferred types, 5,554 nodes, of which 4,995 are named nodes
  over 229 distinct paths. Inference publishes few types today, so `Ty`'s
  deep copies are not where the cost is.

So the work is in the resolver's lookups. Hashing is about a sixth of it,
building string keys to probe with is most of the allocation, and `Ty` is cold
until inference covers more of the language.

## Design

### D1. Hasher: `foldhash`, randomly seeded, behind a sema-wide alias

`borzoi_sema::hash::{HashMap, HashSet}` are std's maps with
`foldhash::fast::RandomState`, and every sema map uses them. A clippy
`disallowed-types` entry for `std::collections::{HashMap, HashSet}`, scoped to
sema, keeps new code on the alias. clippy reads the nearest `clippy.toml` and
does not merge it with the workspace's, so sema's must repeat the workspace's
spawn bans; a test that the two files' shared entries agree keeps them in
step.

`foldhash` is about as fast as unseeded FxHash on short keys (the Fx spike
above stands in for it; Stage 1 restates the number with `foldhash` itself)
and keeps two properties of SipHash's `RandomState` that Fx drops:

- **The iteration order stays random per map and per process.** Any
  behaviour that depends on a map's iteration order is a bug today, and it
  shows up as flakiness or as an exact manifest that moves between runs. A
  deterministic hasher would make the same bug deterministic, and invisible
  to every gate.
- **Collisions cannot be precomputed.** The keys are untrusted text: the
  workspace's sources and third-party DLLs' metadata. A collision attack costs
  the victim a slow language server, not a wrong answer. borzoi already
  executes the workspace's MSBuild targets whenever on-demand restore is
  enabled, so the threat is small, but unseeded Fx would let one crafted
  package slow every installation. After D3, almost every map is keyed by
  sequential integer ids, which an attacker does not choose, and the
  interner's string map is the only one that hashes untrusted bytes.

Rejected alternatives: `rustc-hash` (unseeded, so both properties above go);
`ahash` (similar speed, more target-specific code); keeping SipHash
everywhere (17.8% of resolve).

`ResolvedFile::resolutions`, `InferredFile::types` and a few other public
accessors return `&HashMap<…>`, so their types change with the hasher. They
return the alias, which `borzoi_sema` re-exports, and the LSP and corpus-diff
call sites that spell std's type change with them.

### D2. One name table per `AssemblyEnv`, append-only

The table lives inside the `AssemblyEnv` (`env.names()`). It is seeded when
the env is built with every name and path the env indexes, and a fold appends
the names its sources introduce.

**Why the env.** The LSP already pairs every sema result with the env it was
computed against: `resolved_projects` holds `(Arc<ResolvedProject>,
Arc<AssemblyEnv>)`, and `PrevFold::env` and `CachedInference::env` are the
reuse witnesses, compared with `Arc::ptr_eq`. They exist because an
`EntityHandle` is an index into the env. A `Name` or `PathId` is an index into
the env in the same way, so it falls under a rule the code already enforces,
and incremental reuse of a `ResolvedFile` across folds is sound exactly when
it is today. A rebuilt env is a fresh table, and the existing `ptr_eq` check
already cold-folds against it.

**Rejected: a process-global table** (rustc's `Symbol`, rust-analyzer's
`intern`). It never frees, so a long-lived server grows without bound, every
test thread in the single test binary contends on it, and ids leak between
unrelated projects and tests. **Rejected: a table per fold or per file.** The
incremental fold reuses `ResolvedFile`s across folds, and `ProjectItems`
carries earlier files' names into later ones, so both would need remapping
at every boundary.

**Concurrency.** The LSP resolves on one request thread, but the test binaries
share a static BCL env across threads. The seed is frozen after the env is
built and can be probed without a lock. Names a fold appends go into an
overlay, inserted under a lock and read by id without one (append-only
chunked storage). If the overlay's lock shows up in the suite's profile, shard
it by hash.

**Bound.** The overlay grows with every distinct identifier and path the
env's folds ever meet, including each prefix of a name as it is typed. The census puts a
whole 11.8 MB tree at about 40,000 names. After each fold, the LSP checks the
overlay against a cap (order of 2²⁰ names) and, when it is over, drops that
project's env. That turns into a cold rebuild, through machinery that already
exists. The guarantee is that table memory stays below the cap plus one
fold's names.

**Ids never order anything.** Ids depend on interning order, and that order
differs between runs (test threads, which files an incremental fold
re-resolves). So `Name`, `PathId` and any later `TyId` implement `Eq` and
`Hash` and never `Ord`, which makes a `BTreeMap<Name, _>` a compile error.
Ordered output sorts by text.

**Mixing two tables is loud.** Each table has a unique tag. In debug builds an
id carries its table's tag, and equality and `resolve` assert that the tags
match. Comparing a name from one project's env with a name from another's then
panics in a test, instead of silently comparing indices. In release builds an
id is 4 bytes. Today no consumer compares results across projects:
`textDocument/references` is project-scoped.

### D3. Typed ids: `Name` and `PathId`

- **`Name`**: one identifier in F#'s *logical* domain, the text `id_text`
  produces. Backticks are stripped, and only a *bare* operator is compiled,
  so `+` becomes `op_Addition` while `` ``+`` `` is the literal identifier
  `+`, a different name. FCS holds that distinction
  (`a_quoted_definition_is_a_different_name`,
  `a_quoted_operator_name_is_never_its_compiled_name`). A prefix
  application's operator takes its prefix name, so a token's position in the
  tree is part of its name. `Name` has two constructors.
  `names.ident(&SyntaxToken)` goes through `id_text` unchanged; because it
  takes the token, nothing can normalise a name twice (`id_text` is not
  idempotent: `` ``+`` ``'s name `+` would itself become `op_Addition`).
  `names.logical(&str)` takes text that is already a name: metadata names, and
  `Def::key`'s output. There is no `From<&str>`. Display names are rendered
  from a `Name`, never stored as one.
- **`PathId`**: a node in a trie of `Name`s, `(parent: PathId, last: Name)`,
  whose root is the empty path. `names.child(parent, name) -> Option<PathId>`
  never inserts; `names.intern_child` does. Every map keyed by `PathId` got its
  keys from the table, so a `None` from `child` proves that no such map holds
  the path, and a lookup chain stops without allocating anything.
  `ContainerPath::id_of` relies on exactly this property today, per file; the
  table makes it env-wide. `ContainerId`, and the trie inside `PathSet`, become
  `PathId`s.
- **One path type, not one per denotation.** F# lets one path name a
  namespace, a module and a type at once. Which of those a path denotes is
  decided by the map it is looked up in, and a phantom-typed
  `PathId<Namespace>` would turn one path into two types.
- **A `PathId` is a spelling, not an entity.** `AssemblyEnv` still finds a
  type by (namespace, name, arity) and ignores the referencing assembly
  (`member-resolution-soundness`, trap 1), and `Ty::Named` and `Ty::Project`
  stay separate variants. No code may read "same `PathId`" as "same type".

### D4. `Ty`: interned paths now, hash-consing only by measurement

`Ty::Named { path: PathId, args: Vec<Ty> }`, and the same for `Ty::Project`.
Cloning a nullary type then allocates nothing (an empty `Vec` never
allocates), and equality on paths compares integers. Rendering takes the table
(`ty.render(env.names())`), as does `corpus-diff`'s `parse_canonical`, which
builds a `Ty` from FCS's text.

Hash-consing (`TyId` into an arena, so a type is `Copy` and equality is O(1))
is gated (Stage 8). Infer costs 187 instructions per byte and the census
counts 5,554 published type nodes, so it would buy nothing today. If it is
built, terms holding `TyVid`s live in an arena per inference session and only
ground published types can be env-scoped. `TyId` equality must never stand in
for overload sameness: structural signature comparison is the trap that
`member-resolution-soundness` (trap 3) says to delete rather than harden.

### D5. The opacity state becomes a value with an explicit lifetime

The resolver's 11 boolean fields and `DottedOpacity`, grouped by how long each
one lives:

| lifetime | fields | today |
|---|---|---|
| open scope: reset at each top-level block, restored when a nested module ends | `unmodelled_open_active`, `opaque_value_open`, `dotted_opacity`, `recursive_module_active`, `in_exporting_module` | reset by hand in `resolve_file`; saved and restored by hand in `nested_module` |
| file-wide, only ever raised | `open_extension_unknowable`, `attribute_shape_unknowable`, `augmentation_names_unknowable`, `declares_core_attribute_alias`, `global_block_opened` | `bool` fields, each documented as monotone |
| per top-level block | `anonymous_root` | set in `resolve_file` |
| a call's dynamic extent | `decline_binding_head_param_exprs` | set and restored around a call |

The open-scope flags share their lifecycle with the rest of the open state:
`imports`, the five prefix lists, `open_generation`, `latest_open_pos`,
`pattern_suppressed_case_ids`, `auto_open_type_shadow_names`,
`rec_module_names`, `access_floor`, and the container and local-path marks.
`nested_module` saves 21 values one by one and restores them one by one, and
`resolve_file` resets 17 fields by hand at each block.

- **`OpenScope`** holds that whole group. A block starts with
  `OpenScope::block_start(imports, shortening, header)`. A nested module runs
  as `let outer = self.scope.clone(); …; self.scope = outer.leave(&mut
  self.scope, exit)`, where `leave` names each field that does not simply
  come back. Only `auto_open_type_shadow_names` is named: an `[<AutoOpen>]`
  module merges what its body grew into its container. `rec_module_names` and
  `access_floor`, which the module's header sets, come back to their values at
  entry like every other field (the conditional restore the walk spelled out
  by hand was always a restore to the entry value), and the debug assertion
  checks it. A field added to `OpenScope` cannot be missed by the restore.
  Today each of the 21 hand-written pairs can be.
  - The container path and the block's local shadow sets
    (`nested_module_locals`, `augmentation_head_locals`) share the lifetime
    but stay outside `OpenScope`: each is restored through its own mark, which
    costs nothing, where a clone would copy every path the file has entered.
  - `rec_module_names` is shared (`Rc`), since it is only ever replaced
    whole and a nested module clones the scope.
  - `block_type_defs` is reset at each block but is not restored after a
    nested module (a nested module's types stay in the block's set), so it is
    a per-block accumulator, not open scope, and stays outside.
- **The opacity part of `OpenScope`** is `OpenOpacityState { value, dotted,
  unmodelled }`, with raise-only methods, since the flags are monotone within a
  scope. Its predicates are named after the questions the read sites ask:
  `lookup.rs` and `types.rs` each spell three-flag conjunctions inline today.
  The `OpenOpacity` trace record that `resolution-explain` prints becomes
  `after.raised_since(before)`, so it cannot drift from the state.
- **An implication that the code contradicts, settled.** `opaque_value_open`'s
  doc said the flag implies dotted opacity. It does not: the
  `open_imports_project_values` fallback and `open_type_union_cases` raise
  the value part alone, and the census (Stage 9) reaches that state on the
  corpus. The read sites that defer under either part ask
  `bare_or_dotted_heads_unknowable`; two read the dotted part alone (the
  type-qualified case past enumerable project modules, and that channel's
  module list). Encoding the implication would add deferrals there, so the
  stage keeps today's behaviour and corrects the doc: the three parts are
  independent.
- **File-wide unknowables** become a set of reasons with `raise` and no
  `clear`. The ones a prescan computes before the walk become an immutable
  value the walk borrows.
- **Windows** become scoped calls (`self.with_window(…, |r| …)`), so neither
  an early return nor `?` can skip the restore.

### D6. `Resolver` splits by lifetime

The 107 fields fall into these groups:

- **inputs**, borrowed: `preceding`, `assemblies`, `item_base`;
- **prescan facts**, immutable once the walk starts;
- **indexes** the walk builds and only grows: `type_defs`, `type_cases`,
  `type_members`, and so on;
- **`OpenScope`** (D5);
- **output**: `defs`, `items`, `resolutions`, `decline_sites`, `diagnostics`,
  `export_decls`, …, moved into the `ResolvedFile` in one piece;
- **windows** (D5).

With the groups as separate fields, a method can borrow the indexes while
writing the output, which replaces clones made only to satisfy the borrow
checker. This is a refactor, and it is worth doing for local reasoning, not
for counts.

### D7. Keeping the differentials meaningful

No stage intends to change behaviour. Every FCS differential and every exact
manifest has zero lines moved after each stage. That covers
`resolve_corpus_diff`, `infer_corpus_diff`, `attr_resolution_sweep`,
`silent_region_sweep`, `auto_open_member_sweep`, `binder_site_grid`'s pinned
statuses and the whole-project gate. The parser sweeps are unaffected. The
one planned exception is D5's implication: it moves lines only toward
deferral, and the stage that encodes it says so.

The manifests check end-to-end answers. They do not check each index the walk
consults. So wherever a representation changes, the new one is held to the
old in lockstep, as the flat table was held to rowan (`rowan-reference`):

- **`interning-reference`**, a cargo feature of `borzoi-sema` that its own
  dev-dependency self-reference turns on (the way `test-support` is turned
  on). Under it, each converted index is kept beside its string-keyed
  predecessor, and every query asserts that the two answers agree. So every
  test and corpus sweep that runs sema holds the interned representation to
  the string one, query by query, not only the ones written for this plan.
  The reference maps go in the last interning stage (Stage 12).
- **Interning-order invariance**, a metamorphic test. Resolve a corpus sample
  twice, the second time after pre-interning its names into the table in a
  shuffled order. The rendered results must be identical. That catches any
  answer that depends on id values, which D2's missing `Ord` makes hard but
  not impossible: `PathId::index()` exists for storage.
- **Table properties.** `resolve(intern(s)) == s`; `ident(tok)` resolves to
  `id_text(tok)`; `child` agrees with `intern_child` and never inserts; a
  name that `child` misses was never interned; interning from N threads at
  once gives one id per string; mixing tags panics in debug builds.
- **The stack bound.** `walk_stack_bound` pins how deep each walk reached
  before a decline. A representation change that fattens a recursive frame
  fails there and names the shape. A change to `Resolver`'s layout moves no
  frame, since the resolver is borrowed, not copied.

## Implementation plan

> Implement this plan with each stage on its own branch, stacked as necessary
> on previous branches, so that a reviewer can review each branch in isolation.

The counts are instructions per byte of `resolve_file` on Stage 0's harness.
The impacts for Stages 1 and 2 are measured by the spikes above. The others are
estimated from the profile's inclusive shares, and each stage restates its
estimate as a measurement when it lands.

### Stage 0: sema count harness

**Dependencies**: none. **Implements**: measurement for every later stage.

`crates/sema/examples/sema_counts.rs`: `resolve_file` and `infer_file` over
`$BORZOI_CORPUS/src` against the BCL, with retired instructions, allocations,
reallocations and bytes allocated per source byte, min of N, and the census
of names, definitions and types. It is an example on the system allocator,
wrapped by a counter, not a gate.

**Oracle**: it runs under `nix develop` and reproduces the measurements table
above.

### Stage 1: the hasher

**Dependencies**: none. **Implements**: D1.
**Implementation**: [#577](https://github.com/WoofWare/borzoi/pull/577).

`foldhash` joins the vendored registry (the `add-cargo-dependency` skill).
`borzoi_sema::hash` defines the aliases, every sema map moves to them, and the
scoped clippy configuration bans std's.

**Oracle**: every gate is unchanged, and the corpus manifests have zero lines
moved. `cargo clippy` fails on a std `HashMap` in sema (checked once by hand
while writing the stage, not by a test).

**Expected**: about −15% on resolve (2640 → ~2255) and −8% on infer, with no
change in allocations, if `foldhash` matches the Fx spike.

### Stage 2: memoised namespace fold surfaces

**Dependencies**: none, so it can land in parallel with Stage 1.
**Implements**: a measured hot spot outside the issue's list.

`open_namespace_fold_surfaces(namespace)` scans every top-level type of every
assembly and rebuilds the surfaces from scratch. Its callers in `lookup.rs` and
`types.rs` call it once per use and open prefix, only to ask whether any
surface supplies one name. The stage:

- reads the namespace's types from the `types_by_namespace` index instead of
  scanning;
- memoises the surfaces per namespace inside the env, cleared by the env's
  `&mut` mutators (`mark_namespace_dropped_type`,
  `mark_extension_surface_unknowable`), following its `OnceLock` caches;
- answers "does a surface supply `head`" from the memo, with no clone.

**Oracle**: a property test holds the memo and the index to the uncached scan,
kept as a test-only reference in the way #584's `TopLevelLookup::Scan` is.
It covers every namespace of generated environments and includes a mutation
after the memo has filled. Every gate is unchanged.

**Expected**: −5% (measured with the memo and two query sites), more once the
remaining callers borrow the memo instead of cloning it.

**Measured**: resolve 2642.5 → 2499.1 instructions per byte (−5.4%),
allocations 2.268 → 2.206 per byte; infer unchanged. `open_namespace_fold_surfaces`
returns the memo's slice and `ValueSurface` borrows it; the two callers that
keep surfaces past the query clone them. The fold reads none of what the
`mark_*` mutators mark, so their reset is defensive: removing it moves no
case of the property test.

### Stage 3: the name table, with the walk's container ids as its first consumer

**Dependencies**: Stage 1 (the table's maps use the alias).
**Implements**: D2, D3; the `interning-reference` feature and the table
properties of D7.

`NameTable`, `Name` and `PathId`, held by `AssemblyEnv` and seeded at
construction. `ContainerPath`'s per-file `ContainerId` becomes the shared
`PathId`, a change with no behavioural effect: a path the walk has not entered
still holds nothing in any map keyed by it. The env-rebuild cap of D2 lands in
the LSP here.

**Oracle**: D7's table properties and the interning-order metamorphic test;
every gate is unchanged; an LSP test drives the overlay over a small cap and
sees a cold fold against a fresh env.

**Expected**: about 0. This stage is infrastructure, with one real consumer.

**Implemented** (`crates/sema/src/names.rs`), with three departures from D2
and D3, each because nothing in this stage would read what it adds:

- **No seed.** The table starts empty with each env and holds only what the
  walks intern. The seed exists so that a `None` from `child` also proves an
  *env* index lacks a path, and the first map that relies on that is Stage 4's
  (`value_slot`'s assembly probe) or 5c's; whichever lands first seeds the
  table with exactly the keys it then trusts the table for. So there is one
  tier, not a frozen seed and an overlay: id → text and id → path node are
  lock-free reads from append-only chunked storage (`std`'s `OnceLock`, no
  `unsafe`), and text → name and `(parent, name)` → child go through one
  `RwLock`. Measured below, the lock costs nothing visible; Stage 4 restates
  that on the hot path.
- **No `names.ident(&SyntaxToken)` yet.** The container walk already holds
  `id_text`'s output, so it interns through `logical`; `ident` lands with
  the first caller that holds a token.
- **The lockstep runs only in debug builds.** `interning-reference` is turned
  on by the crate's dev-dependency self-reference, which also reaches the
  examples, so `sema_counts` would otherwise measure the reference too. The
  tests and corpus sweeps are debug builds and run it. Its first index is the
  per-walk container trie that `PathId` replaced
  (`resolve/container.rs`'s `reference`): it asserts that the table's ids
  biject with the walk-local ones over the containers the walk entered, and
  that a path the walk never entered has no id or one it never entered either.

The interning-order test (`tests/all/interning_order.rs`) resolves generated
two-file projects against an empty env, and a sample of the corpus against
the BCL, then again after pre-interning every path the first run interned,
plus each path reversed as a decoy, in a shuffled order. A planted dependence
on an id's value (`prefix_id` answering the root for one index) fails the
generated half; the corpus half did not catch it, so it is the weaker of the
two. The LSP drops an env once a fold leaves its table above
`NameTableCap` (2²⁰ entries) and folds cold against a rebuilt one
(`an_env_past_its_name_cap_is_rebuilt_and_folded_cold`). The cap counts
paths as well as names: an edit that rearranges names the table already
holds interns new paths and no new name.

**Measured** (on Stage 9, which it is stacked on): resolve 1584.1 → 1586.2
instructions per byte (+0.1%), allocations 1.736 per byte before and after;
infer 173.5 → 173.0.

### Stage 4: the open-prefix × head product on ids

**Dependencies**: Stage 3. **Implements**: D3 on the hottest path, the issue's
`O(opens × segments)` strings.

The open state (`imports`, `explicit_open_prefixes`, `module_open_prefixes`,
`assembly_open_prefixes`, `incomplete_open_prefixes`,
`open_shortening_prefixes`) holds `PathId`s. `ProjectItems`' type paths
(`exported_type_slot`, `exported_type_order`) are keyed by `PathId`.
`type_path_exports` gets a `PathId` index, replacing its `rfind` scan.
`type_slot_history` is keyed by `(PathId, Name)`. `value_slot` and its
siblings then probe `names.child(prefix, head)` and skip an open whose child
was never interned.

**Oracle**: `interning-reference` lockstep on every converted query; every gate
is unchanged; the interning-order test.

**Expected**: most of `value_slot`'s remaining share, an estimated −8 to −12%,
with allocations down 10–20%. `value_slot` is 15.2% inclusive, and it builds
`1 + segments` strings for every open prefix at every use.

**Implemented** for the explicit namespace and project module opens, which
are the prefixes `value_slot` and its siblings (`lexical_type_is_latest`,
`value_fold_order`) probe project maps under:

- `OpenScope::explicit_open_prefixes` and `module_open_prefixes` hold an
  `OpenedPrefix { pos, id, path }`: the prefix's `PathId`, interned when the
  `open` is processed, beside its segments, which the consumers that ask the
  env or the string-keyed project indexes still read.
- `ProjectItems::type_paths` is a `PathMap` keyed by `PathId`, interned in
  `extend_with`, which now takes the env's table.
  `exported_type_slot`/`exported_type_order` become
  `exported_type_by_id(names, id, …)`, returning the entry, and
  `ExportedTypePath::order`. The scans over it (`direct_type_contestants`,
  `binds_along_path`) go by the table too.
- `type_path_export_ids` indexes `type_path_exports` by `PathId`, the latest
  entry per path, replacing the `rfind` scans.
- `type_slot_history` is a `ChildMap` keyed by `(PathId, Name)`.
- `value_slot` looks the head up once (`names.lookup`), probes
  `names.child(open.id, head)` per open, and reads the in-file `type_defs`
  by the open's id rather than by building and hashing its path. A head the
  table never interned skips every project probe.

Each converted map carries its predecessor under `interning-reference` and
asserts on every query: `PathMap` a string-keyed twin, `ChildMap` a twin
keyed by the container's id and the name's text (the predecessor's own
keying; a twin keyed by the container's segments would cost the reference a
path per level, which `walk_cost_scaling` rejects as quadratic in the
nesting), and the export index the `rfind` it replaced. A `PathMap` also
asserts, in every debug build, that it is probed only with its own table's
ids. Planting a wrong parent in `value_slot`'s child probe fails the lockstep
in `interning_order`, `resolve_project_diff` and `resolve_type_members`.

Not converted, and why:

- `imports`, `assembly_open_prefixes`, `incomplete_open_prefixes` and
  `open_shortening_prefixes`: every consumer of theirs asks the env, whose
  indexes are string-keyed until 5c. Their ids would be read by nothing.
  What is left of `value_slot` (3.9% of resolve's samples, from 20.3%) is
  mostly its assembly probe, `public_types_named` per assembly open, which 5c
  takes once the env's types are keyed by `PathId`.
- No seed (Stage 3's deviation) is needed yet: no map this stage converts is
  an env index.

**Measured**: resolve 1585.6 → 1245.8 instructions per byte (−21.4%),
allocations 1.736 → 1.267 per byte (−27%). More than the estimate: on
main at Stage 3, `value_slot` was 20.3% of resolve's samples, not 15.2%
(the stages that landed since made everything around it cheaper), and it
falls to 3.9%. Infer reads 172.8 → 174.9 (+1.2%) with its allocations
unchanged, though its input is not: a throwaway dump of every corpus
file's resolutions, decline sites, definitions, diagnostics and inferred
types, each file alone and each directory as a project, is byte-identical
before and after. `sema_counts` infers right after resolving, on the heap
resolve leaves behind, which this stage changes.

### Stage 5: the remaining path and name keys

**Dependencies**: Stage 4. **Implements**: D3 for the rest of sema's string-keyed maps and sets (42
source lines declare one keyed by `Vec<String>`, 118 by `String`). It is split so that
each part lands alone:

- **5a**: `ProjectItems`.
- **5b**: the resolver's indexes.
- **5c**: `AssemblyEnv`'s indexes: `types_by_namespace` (and with it
  `types_named`), `auto_open_modules`, `public_namespace_paths`,
  `namespaces_with_dropped_types`, the abbreviation caches.

**Oracle**: `interning-reference` lockstep; every gate is unchanged.
`walk_cost_scaling`'s named-root entries in `MEMORY_EXEMPT` (added by #586:
the walk holds each nested module's export by its full path) come out once
those paths are `PathId`s. The test already fails with "exempt, yet its
working memory grew only …" when an exempt shape stops growing, so the stage
cannot forget to remove them.

**Expected**: the rest of the string hashing, string clones (`String::clone`
is 9.8% inclusive) and path allocations: an estimated −10 to −15% across the
three parts, and allocations toward one per source byte.

**Order.** A profile at Stage 4's head put the env's share first: the
dropped-type gate (`dropped_type_could_root_this_path`, 5.2% of resolve's
samples, all of it building `prefix ++ path` for every reading to ask an
empty set), `AssemblyEnv::types_named` (4.4%, hashing a `Vec<String>`
namespace per probe), and `lookup_type`. 5c also needs nothing from 5a or 5b,
while those two share `PathSet`, so 5c lands first.

**5c, implemented** (`assembly_env/type_index.rs`, `names.rs`):

- **The seed.** `types_by_namespace` is a `TypeIndex` keyed by the
  namespace's `PathId` and then by `Name` (F# source name; the
  logical-segment index by IL or source name). Inserting interns the key, so
  the env's table holds every namespace and type name the index does, and a
  query whose namespace or name has no id answers empty without hashing.
  `public_namespace_paths`, `namespaces_with_dropped_types` and
  `dropped_type_namespace_prefixes` are `PathIdSet`s; `auto_open_modules` and
  `auto_open_module_paths` are `PathMap`s. Each interns as it is filled, which
  is the rest of the seed. `interned_count` leaves the seed out (the env
  records the table's size once its indexes are built and again after
  `mark_namespace_dropped_type`), so the LSP's cap bounds what folds add, as
  D2 intends.
- **Keys at the query.** A query takes a `PathKey { id, text }` and a
  `NameKey { id, text }`: the id answers it, and the text is what the caller
  holds independently of the table. The string-taking public API stays and
  builds its keys with a lookup.
- **The resolver passes ids.** Every `open`'s readings (`OpenGroup`) and
  `assembly_open_prefixes` are interned when the `open` is processed; the
  prefix walks (`assembly_prefixes_by_priority` and its siblings) yield a
  `PrefixRef { id, path }`, the enclosing namespace's id coming from
  `ContainerPath`. The tiered walk's per-reading closures take a `PathKey`.
  `assembly_path_records`, `assembly_type_path_core` and
  `opened_assembly_modules` look the source segments up once per walk
  (`Segments`) and extend the reading's id by them (`extend_ids`), so each
  rooting position's namespace is probed by id. `value_slot`'s assembly probe,
  `namespace_could_supply_slot_type`, the auto-open shadow and
  `declares_type_at_any_arity` go by id the same way.
- **The dropped-type gate** asks `any_split_under_has_a_dropped_type(prefix,
  tail)`: the splits are the prefix's ancestors (by `NameTable::split`) and
  its extensions by the tail (by `child`), each probed by id, and an empty set
  answers at once. A string-keyed `PathIdSet` or `PathMap` probe likewise
  skips the lookup when the set is empty.
- **Lockstep.** Under `interning-reference` in a debug build, `TypeIndex`
  keeps the string-keyed index it replaced and asserts on every query (the
  namespace's population, the name bucket, the first-wins slot, the
  logical-segment bucket); `PathIdSet` and `PathMap` keep their string twins;
  every `PathKey` and `NameKey` asserts that its id spells its text, which is
  what catches a key built against the wrong parent at the call site; and the
  dropped-type walk asserts its answer against the path-building original.
- **`with_fresh_names`** (the interning-order test's copy of an env) now
  re-keys the env's indexes onto the fresh table after interning the test's
  shuffled paths, so the seed takes ids after them.

Not converted, and why:

- The abbreviation caches (`abbreviation_chase_cache`, the measure caches)
  are keyed by `EntityHandle` already; there is no string key to replace.
- `unknowable_abbreviation_namespaces` is asked "is any member under this
  path" by a scan, off every hot path (0.0% of resolve's samples).
- Callers that build a full path to ask the env *and* a project index in the
  same breath (`assembly_path_records`'s `project_shadow_cause`, the
  `open type` target walk) still build it: the project side is 5a/5b's.

**Measured** (5c): resolve 1245.1 → 1134.2 instructions per byte (−8.9%),
allocations 1.267 → 1.186 per byte; infer 174.7 → 175.4. A first cut that
converted the indexes but left most callers on the string API measured
1214.4: a string probe now costs a table lookup per segment, more than
hashing a `Vec<String>` once, so the gain is in the callers that hold ids.
The whole-corpus dump of Stage 4 (resolutions, decline sites, definitions,
diagnostics, inferred types; each file alone and each directory as a
project) is byte-identical before and after.

### Stage 6 (optional): published names

**Dependencies**: Stage 5. **Implements**: D3 at the public API.

`Def::name`, export-declaration paths and the paths inside `Resolution`
payloads become `Name` and `PathId`. The LSP (hover, completion, XML-doc
pairing's `unticked`) and corpus-diff render them through `env.names()`.
`Def::name` is the verbatim spelling today, and a `Name` is logical, so the
spelling a consumer shows has to come from somewhere. The census says it is
the text at `Def::range` for every definition except 1,487 active-pattern
recognizers and 2 union cases. The stage starts by explaining those, and
either derives the spelling from the source or keeps it for those kinds.

**Oracle**: the corpus-diff handler differential (hover text at every graded
position) and every gate are unchanged.

**Expected**: about −0.5% of resolve's allocations. The stage buys one identity
domain across sema's API, not speed.

### Stage 7: `Ty` paths on ids

**Dependencies**: Stage 3, and 5c for `completion`'s member lookups by path.
**Implements**: D4's first half.

**Oracle**: `infer_corpus_diff`'s manifest and the hover-type handler
differential are unchanged. A property over generated types (unify's existing
strategies) checks that converting to and from the string form is the
identity, that equality and hashing agree with the string form's, and that
`render` and `render_fsharp` produce identical text.

**Expected**: small on today's inference, which publishes few types: infer is
about 6% of the pipeline.

### Stage 8 (gated): hash-consed `Ty`

**Dependencies**: Stage 7. **Implements**: D4's second half.

**Gate**: build it only once a profile at the then-current inference coverage
puts `Ty`'s clone, drop, equality and hashing above about 10% of infer's
instructions. The oracle, if it is built, is the same as Stage 7's, plus the
interning-order test over types.

### Stage 9: `OpenScope`

**Dependencies**: none. It conflicts textually with Stage 4 (the same fields
change type), so land it before Stage 4 or rebase it after.
**Implements**: D5's open scope and opacity state.

**Oracle**: every gate is unchanged. The `resolution-explain` trace's tests
are unchanged now that the trace record is computed from the state. A debug
assertion checks that every `OpenScope` field outside `leave`'s named list is
equal after a nested module to what it was before. The census of reachable
opacity combinations is recorded in the PR, which settles D5's implication
question.

**Expected**: about 0.

**Measured**: resolve 1583.2 → 1585.1 instructions per byte (+0.1%),
allocations 1.736 → 1.736 per byte; infer unchanged. A nested module clones
the same fields it saved by hand before, plus a reference count.

**Census.** States change only at an `open`, so the state after each open,
and at each read of a predicate, enumerates the reachable ones. Over the whole
corpus (5,202 files, each resolved on its own, so no earlier file's modules are
in scope and the enumerable-project-module part of the dotted state cannot
rise), the reachable states are: nothing raised; the dotted part alone; the
value part alone; and the value and unqualified parts together. The value part
was never raised with the dotted part, nor the unqualified part without the
value part. So the value part does not imply the dotted part on real code,
and D5's implication is false, not merely unencoded.

### Stage 10: file-wide unknowables and windows

**Dependencies**: Stage 9. **Implements**: the rest of D5.

**Oracle**: every gate is unchanged.

### Stage 11: the resolver split

**Dependencies**: Stages 9 and 10, preferably after Stage 5 so the field types
are final. **Implements**: D6.

**Oracle**: the compiler, and every gate unchanged, `walk_stack_bound`
included. Any clone removed because borrows can now be split shows up in
Stage 0's allocation column.

### Stage 12: retire the string reference

**Dependencies**: Stages 5, 6 (if taken) and 7. **Implements**: the end of
D7's lockstep.

The `interning-reference` feature and the string-keyed maps behind it go.

**Oracle**: `cargo tree` shows no feature user, and the full gate is green.

## Non-goals

- `head_contested_by_project_module`'s quadratic, which is being fixed
  separately.
- Reducing how many walks or lookups the resolver does. This plan makes each
  lookup cheaper, whatever the algorithm.
- Interning inside `borzoi-assembly`, which stays self-contained and reusable.
  The env interns what it reads out of an `Entity`.
- Changing what any lookup answers. Every stage is a change of representation
  checked against the code it replaces; D5's implication is the one place a
  stage may add deferrals, and it says so.
