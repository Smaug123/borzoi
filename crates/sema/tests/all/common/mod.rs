//! Shared helpers for the sema crate's integration tests.
//!
//! The process plumbing for driving `fcs-dump` is the shared [`BoundedCommand`]
//! (over the process-global spawn lock in `borzoi-spawn`); this module knows
//! the subcommands, the `uses` projection (`NormalisedUse` / `parse_fcs_uses`) the
//! sema name-resolution oracle needs, and nothing about pipes. Every child here
//! runs under a deadline and has both its output pipes drained concurrently — the
//! hand-rolled `invoke_fcs_dump_project` did neither, and wrote its stdin
//! synchronously against undrained output pipes, which deadlocks on a large enough
//! Compile order.

#![allow(dead_code)] // each importer uses a different subset.

pub mod fold_matrix;
pub mod generator;
pub mod overload_corpus;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use borzoi_oracle_harness::{BatchChild, BoundedCommand, default_timeout};
use serde::Deserialize;

// ============================================================================
// `dotnet build` serialisation
// ============================================================================

/// Serialises every `dotnet build` invocation in this test binary.
///
/// The per-fixture `OnceLock`s below de-dupe builds *within* a single
/// `ensure_*_built` function, but two different `ensure_*_built` functions
/// hit by concurrent test threads can still spawn overlapping `dotnet build`
/// processes. Even though sema's two fixtures (`tools/fcs-dump` and
/// `tests/fixtures/assembly_env`) don't share `ProjectReference` edges,
/// MSBuild and the .NET host both write to shared NuGet / global cache state
/// during a build, and concurrent writes there have triggered transient
/// failures elsewhere in this repo (see `crates/assembly/tests/all/common/mod.rs`).
///
/// **This lock is in-process only.** Two `cargo test` invocations against
/// the same workspace can still race on `obj/` and `bin/` for the same
/// fixture, because cargo's package-cache lock is released before tests
/// run. The `BORZOI_ASSEMBLY_FIXTURE_DLL` / `BORZOI_FCS_DUMP`
/// env-var bypasses below close that gap — CI and any caller running
/// parallel cargo invocations should pre-build and set them.
static BUILD_LOCK: Mutex<()> = Mutex::new(());

// ============================================================================
// Workspace pathing
// ============================================================================

/// The workspace root, two `..` jumps above the sema crate's `CARGO_MANIFEST_DIR`.
/// `tools/fcs-dump` lives there.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root parent")
        .to_path_buf()
}

pub fn project_dir() -> PathBuf {
    workspace_root().join("tools").join("fcs-dump")
}

// ============================================================================
// fcs-dump invocation
// ============================================================================

/// Build the base `fcs-dump <subcommand>` command, without arguments.
///
/// Honours `BORZOI_FCS_DUMP` (path to a pre-built self-contained binary)
/// when set; otherwise builds `tools/fcs-dump` **once** per test binary and
/// execs the resulting assembly on every call. The build-once strategy avoids
/// the `dotnet run` incremental-build race when N test threads invoke it
/// concurrently.
fn fcs_dump_command(subcommand: &str) -> Command {
    if let Some(bin) = std::env::var_os("BORZOI_FCS_DUMP") {
        let mut c = Command::new(bin);
        c.arg(subcommand);
        c
    } else {
        let bin = ensure_fcs_dump_built();
        let mut c = Command::new("dotnet");
        c.arg(bin).arg(subcommand);
        c
    }
}

/// Run the single-file `<subcommand>` projection over `source` and return the
/// oracle's JSON as a UTF-8 string.
///
/// Routes through the resident [`file-batch`](fcs_file_batch_pool) pool, so the
/// ~1.6 s .NET + FCS cold-start is paid once per pool slot rather than once per
/// call. The returned JSON is the same payload the one-shot `fcs-dump
/// <subcommand> <file>` emits (compact rather than indented, which the
/// `serde_json` consumers do not observe), so every `parse_fcs_*` caller is
/// unchanged.
pub fn invoke_fcs_dump(subcommand: &str, source: &Path) -> String {
    invoke_fcs_dump_with_refs(subcommand, source, &[])
}

/// Like [`invoke_fcs_dump`], but makes `refs` (extra `.dll`s) resolvable to the
/// snippet — so a fixture assembly's types can be referenced without an
/// offset-shifting `#r` line in the source. The refs ride in the batch request,
/// not `BORZOI_FCS_EXTRA_REFS`: one resident child serves callers whose fixture
/// sets differ, and an env var fixed at spawn could not.
pub fn invoke_fcs_dump_with_refs(subcommand: &str, source: &Path, refs: &[&Path]) -> String {
    let request = serde_json::json!({
        "kind": subcommand,
        "path": source.display().to_string(),
        "refs": refs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
    })
    .to_string();
    let line = fcs_file_batch_pool().request(&request);
    reject_batch_error(&line, subcommand);
    line
}

/// The number of resident `fcs-dump` pools in this test binary; the child budget
/// (`BORZOI_FCS_CHILDREN`, default 6) is split evenly between them, mirroring
/// `cst`'s harness. Two: the single-file `file-batch` pool
/// ([`fcs_file_batch_pool`]) and the multi-file `uses-project` pool
/// ([`fcs_project_batch_pool`]).
const FCS_POOLS: usize = 2;

/// Process-wide cap on resident `fcs-dump` children, split evenly across the
/// [`FCS_POOLS`] pools. Each child is a warm FCS/.NET process holding hundreds of
/// MB and lives until the test binary exits, so the default is deliberately
/// modest (and, like `cst`, a machine with sibling worktrees running their own
/// suites should not be crowded out); override with `BORZOI_FCS_CHILDREN`.
fn fcs_children_budget() -> usize {
    std::env::var("BORZOI_FCS_CHILDREN")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(6)
        .max(FCS_POOLS)
}

/// A pool of interchangeable resident `fcs-dump` children driving one
/// subcommand. A single [`BatchChild`] cannot serve concurrent callers (requests
/// and responses match positionally), so one child serialises every round-trip
/// in the process; a pool of `n` lets `n` of libtest's threads make progress at
/// once. Slots spawn lazily and the lowest idle one is preferred, so the pool
/// converges on roughly the real concurrency rather than eagerly paying `n` .NET
/// startups. Mirrors `crates/cst/tests/all/common/mod.rs`.
struct BatchPool {
    subcommand: &'static str,
    /// Per-request deadline for this pool's children. Snippet pools take the
    /// harness default; the whole-project pool keeps [`PROJECT_TIMEOUT`] — a
    /// large compile order (or a loaded machine) legitimately checks for longer
    /// than the per-snippet default, and a bound tight enough to kill a healthy
    /// project would be worse than no bound at all.
    request_timeout: Duration,
    slots: Vec<Mutex<Option<BatchChild>>>,
}

impl BatchPool {
    fn new(subcommand: &'static str, request_timeout: Duration) -> Self {
        let share = fcs_children_budget() / FCS_POOLS;
        let n = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            subcommand,
            request_timeout,
            slots: (0..n.min(share).max(1)).map(|_| Mutex::new(None)).collect(),
        }
    }

    /// Ask an idle child about `request`, holding it for the whole round-trip.
    /// Falls back to blocking on a round-robin slot when every child is busy, so
    /// a saturated pool queues rather than spawning without bound.
    fn request(&self, request: &str) -> String {
        for slot in &self.slots {
            if let Ok(mut guard) = slot.try_lock() {
                return self.round_trip(&mut guard, request);
            }
        }
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let i = NEXT.fetch_add(1, Ordering::Relaxed) % self.slots.len();
        let mut guard = self.slots[i]
            .lock()
            .expect("fcs-dump batch mutex poisoned (a previous request failed)");
        self.round_trip(&mut guard, request)
    }

    fn round_trip(&self, slot: &mut Option<BatchChild>, request: &str) -> String {
        slot.get_or_insert_with(|| spawn_fcs_batch_child(self.subcommand, self.request_timeout))
            .request(request)
    }
}

/// Spawn one resident `fcs-dump <subcommand>` child under `timeout` per request,
/// honouring `BORZOI_FCS_DUMP` (a prebuilt self-contained binary) exactly as
/// [`fcs_dump_command`] does, and otherwise `dotnet <fcs-dump.dll>`.
/// [`BatchChild::with_factory`] (not `spawn`) so the per-request deadline is the
/// pool's, not the harness's fixed `default_timeout`; `2` attempts matches
/// `spawn`'s default retry-on-a-fresh-child.
fn spawn_fcs_batch_child(subcommand: &'static str, timeout: Duration) -> BatchChild {
    let make: Box<dyn FnMut() -> Command + Send> =
        if let Some(bin) = std::env::var_os("BORZOI_FCS_DUMP") {
            Box::new(move || {
                let mut c = Command::new(&bin);
                c.arg(subcommand);
                c
            })
        } else {
            let dll = ensure_fcs_dump_built()
                .to_str()
                .expect("fcs-dump.dll path is UTF-8")
                .to_owned();
            Box::new(move || {
                let mut c = Command::new("dotnet");
                c.arg(&dll).arg(subcommand);
                c
            })
        };
    BatchChild::with_factory(make, format!("fcs-dump {subcommand}"), timeout, 2)
}

/// The resident single-file oracle pool (`fcs-dump file-batch`): `uses`,
/// `binder-types`, `types`, `attrs`, `overloads`, each carried as a
/// `{ kind, path, refs }` JSON request. Snippets, so the harness default
/// per-request deadline is ample.
fn fcs_file_batch_pool() -> &'static BatchPool {
    static P: OnceLock<BatchPool> = OnceLock::new();
    P.get_or_init(|| BatchPool::new("file-batch", default_timeout()))
}

/// Turn a resident batch handler's `{ "BatchError": <msg> }` failure sentinel
/// back into the loud panic the one-shot's `failwith` would have raised — a
/// single bad snippet/project fails its own test with the FCS message, without
/// wedging the resident child that a thrown exception would have killed. A normal
/// projection payload has no `BatchError` key, so this is a no-op for it.
fn reject_batch_error(line: &str, subcommand: &str) {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(rename = "BatchError")]
        batch_error: Option<String>,
    }
    if let Ok(Probe {
        batch_error: Some(msg),
    }) = serde_json::from_str::<Probe>(line)
    {
        panic!("fcs-dump resident batch ({subcommand}) failed: {msg}");
    }
}

/// Run `fcs-dump uses-project`, feeding `paths` (Compile order) on stdin one per
/// line, and return its stdout. The project-aware oracle for cross-file
/// resolution: each file is checked in the context of the files before it.
pub fn invoke_fcs_dump_project(paths: &[&Path]) -> String {
    invoke_fcs_dump_project_with_refs(paths, &[])
}

/// Like [`invoke_fcs_dump_project`], but makes `refs` (extra `.dll`s) resolvable
/// to every project file — the oracle for cross-file resolution *into a
/// referenced assembly* (the combination the single-file `uses` + refs and the
/// ref-less `uses-project` paths each cover only half of).
///
/// Routes through the resident [`uses-project`](fcs_project_batch_pool) pool: the
/// whole Compile order rides in one JSON request (`{ paths, refs, defines,
/// langversion }`, Compile order preserved), and the reply is the same
/// `{ Files: [...] }` the one-shot emits — so [`parse_fcs_uses_project`] is
/// unchanged. A resident child warms one `FSharpChecker` across projects, so only
/// the first project it serves pays the FCS assembly-load cost.
///
/// The one-shot `uses-project` read the caller's `#if` symbols and `<LangVersion>`
/// pin from the child's *environment* (`BORZOI_FCS_DEFINES` /
/// `BORZOI_FCS_LANGVERSION`); a resident child's env is fixed at spawn, so these
/// travel in each *request* instead, keeping the differential faithful for a
/// caller that sets them (the corpus-diff / LSP consumers do — this sema harness
/// does not, but the helper must not silently drop them).
pub fn invoke_fcs_dump_project_with_refs(paths: &[&Path], refs: &[&Path]) -> String {
    let mut request = serde_json::json!({
        "paths": paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "refs": refs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
    });
    if let Ok(defines) = std::env::var("BORZOI_FCS_DEFINES") {
        let list: Vec<String> = defines
            .split([';', '\n'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        request["defines"] = serde_json::json!(list);
    }
    if let Ok(lang) = std::env::var("BORZOI_FCS_LANGVERSION") {
        let lang = lang.trim();
        if !lang.is_empty() {
            request["langversion"] = serde_json::json!(lang);
        }
    }
    let line = fcs_project_batch_pool().request(&request.to_string());
    reject_batch_error(&line, "uses-project");
    line
}

/// The resident project oracle pool (`fcs-dump uses-project-batch`): each request
/// is one Compile-ordered `{ paths, refs, defines, langversion }` project. Keeps
/// [`PROJECT_TIMEOUT`] per request — one request type-checks a whole Compile
/// order, the same scale the one-shot `uses-project` budgeted for.
fn fcs_project_batch_pool() -> &'static BatchPool {
    static P: OnceLock<BatchPool> = OnceLock::new();
    P.get_or_init(|| BatchPool::new("uses-project-batch", PROJECT_TIMEOUT))
}

/// Run `fcs-dump uses-census-batch` over `paths` (any order; **each file is
/// type-checked in isolation**) and return its JSONL stdout — one
/// `{ Path, Ok, Error, HasCheckErrors, Uses }` object per line. The tolerant
/// census oracle for the Phase-3 scoping measurement
/// (`tests/all/uses_census.rs`); this harness ignores `HasCheckErrors` because
/// partial results are the population that measurement intentionally counts.
pub fn invoke_fcs_dump_census(paths: &[PathBuf]) -> String {
    census_driver("uses-census-batch", paths, &[])
}

/// Like [`invoke_fcs_dump_census`], but makes `refs` (extra `.dll`s) resolvable
/// to every snippet via `BORZOI_FCS_EXTRA_REFS` — so FCS reports the symbol
/// kinds of a *referenced-assembly* member (`Demo.Calc.Zero`). The oracle for
/// the cross-assembly classification differential.
pub fn invoke_fcs_dump_census_with_refs(paths: &[PathBuf], refs: &[&Path]) -> String {
    census_driver("uses-census-batch", paths, refs)
}

/// Run `fcs-dump uses-census-project` over `paths` (**Compile order**, checked as
/// one project so cross-file names resolve) and return the same JSONL shape. The
/// unbiased counterpart used by the isolation-bias probe
/// (`tests/all/uses_census_project.rs`): the same files, this way vs. the batch way,
/// expose the member accesses the isolated batch loses to unresolved siblings.
pub fn invoke_fcs_dump_census_project(paths: &[PathBuf]) -> String {
    census_driver("uses-census-project", paths, &[])
}

/// Run `fcs-dump types-census-batch` over `paths` (any order; **each file is
/// type-checked in isolation** with `keepAssemblyContents`) and return its JSONL
/// stdout — one `{ Path, Ok, Error, Exprs }` object per line. The tolerant
/// oracle for the Phase-3 *type* scoping measurement (`tests/all/types_census.rs`),
/// the type-side sibling of [`invoke_fcs_dump_census`].
pub fn invoke_fcs_dump_types_census(paths: &[PathBuf]) -> String {
    census_driver("types-census-batch", paths, &[])
}

/// Shared driver for the two census subcommands. The paths go to the child's
/// stdin on a dedicated thread while both its output pipes are drained on theirs,
/// so a large input (whose stdin exceeds the pipe buffer) cannot deadlock against
/// the child's interleaved per-file output.
///
/// A whole-corpus census is a legitimately long run — thousands of files
/// type-checked by one child — so it gets a budget to match, rather than the
/// per-request default sized for a single snippet.
fn census_driver(subcommand: &'static str, paths: &[PathBuf], refs: &[&Path]) -> String {
    let mut cmd = fcs_dump_command(subcommand);
    if !refs.is_empty() {
        let joined = refs
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(";");
        cmd.env("BORZOI_FCS_EXTRA_REFS", joined);
    }
    let out = BoundedCommand::new(cmd)
        .stdin_lines(paths.iter().map(|p| p.display().to_string()))
        .timeout(PROJECT_TIMEOUT)
        .run_ok(format_args!("fcs-dump {subcommand}"));
    String::from_utf8(out.stdout).expect("fcs-dump stdout is UTF-8")
}

/// Budget for one whole-project fcs-dump run — a census sweep, or a `uses-project`
/// type-check of an entire Compile order. Generous: it bounds "this will never
/// finish", it is not a performance target. A bound tight enough to kill a healthy
/// large project would be worse than no bound at all, since the caller could not
/// then tell a killed run from a genuinely broken one.
const PROJECT_TIMEOUT: Duration = Duration::from_secs(3600);

// ============================================================================
// Census classification — the Phase-3 scoping taxonomy (single source of truth)
// ============================================================================

/// One file's census result (`{ Path, Ok, HasCheckErrors, Uses }` JSON line).
/// `HasCheckErrors` is intentionally ignored here; this view is shared by
/// `tests/all/uses_census.rs` (corpus sweep) and `tests/all/uses_census_project.rs`
/// (isolation-bias probe).
#[derive(Deserialize)]
pub struct FileCensus {
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Ok")]
    pub ok: bool,
    #[serde(rename = "Uses", default)]
    pub uses: Vec<CensusUse>,
}

/// The FCS facts about one resolved symbol use that determine its bucket, plus
/// the finer member/value distinctions (`IsProperty` / `IsValue` / `IsFunction`)
/// the classification differential ([`crate::classify_diff`]) compares our
/// [`SemanticClass`](borzoi_sema::SemanticClass) commitments against. Only
/// `SymbolName` is still dropped by serde (kept in the JSON for ad-hoc
/// drill-down).
#[derive(Deserialize)]
pub struct CensusUse {
    /// The use's byte range and (when in-file) declaration range. The census
    /// proper ignores these — it buckets by machinery, not location — but the
    /// resolution corpus-diff ([`census_resolve_uses`]) needs them to verify
    /// *which binder* our resolver points at. Private: the public currency is
    /// the byte-offset [`ResolveDiffUse`] the normaliser produces.
    #[serde(rename = "Range")]
    range: FcsRange,
    #[serde(rename = "DeclRange")]
    decl_range: Option<FcsRange>,
    #[serde(rename = "IsFromDefinition")]
    pub is_from_definition: bool,
    #[serde(rename = "Class")]
    pub class: String,
    #[serde(rename = "IsMember")]
    pub is_member: bool,
    #[serde(rename = "IsInstance")]
    pub is_instance: bool,
    #[serde(rename = "IsExtension")]
    pub is_extension: bool,
    #[serde(rename = "IsConstructor")]
    pub is_constructor: bool,
    #[serde(rename = "IsModuleValueOrMember")]
    pub is_module_value_or_member: bool,
    /// `m.IsProperty` — a getter/setter member. Only the classification
    /// differential reads it (the bucket census does not need it).
    #[serde(rename = "IsProperty")]
    pub is_property: bool,
    /// `m.IsValue` — a value (not a function/method). FCS surfaces function and
    /// lambda *parameters* as local values, so this is how a parameter use is
    /// told from a member.
    #[serde(rename = "IsValue")]
    pub is_value: bool,
    /// `m.CurriedParameterGroups.Count > 0` — the symbol is a curried function.
    /// A property of the symbol, not the occurrence, so every use of a function
    /// binding carries it.
    #[serde(rename = "IsFunction")]
    pub is_function: bool,
    #[serde(rename = "IsActivePattern")]
    pub is_active_pattern: bool,
    #[serde(rename = "IsOverloaded")]
    pub is_overloaded: bool,
    #[serde(rename = "IsNamespace")]
    pub is_namespace: bool,
    #[serde(rename = "IsModule")]
    pub is_module: bool,
}

impl CensusUse {
    /// The use occurrence's half-open byte range, resolved against `idx` (a
    /// [`LineIndex`] over the exact source FCS checked). The census proper
    /// buckets by machinery, not location, so the raw range is private; this is
    /// the classification differential's accessor for it.
    pub fn use_range_bytes(&self, idx: &LineIndex) -> (usize, usize) {
        (
            idx.offset(self.range.start.line, self.range.start.col),
            idx.offset(self.range.end.line, self.range.end.col),
        )
    }

    /// The declaration's half-open byte range **when it lies in the checked
    /// file**, else `None` (a referenced-assembly / FSharp.Core declaration).
    /// The in-file test mirrors [`census_resolve_uses`] and [`parse_fcs_uses`]:
    /// the use is always in the checked file, so an in-file declaration shares
    /// its `File`.
    pub fn decl_range_bytes(&self, idx: &LineIndex) -> Option<(usize, usize)> {
        self.decl_range.as_ref().and_then(|d| {
            (d.file == self.range.file).then(|| {
                (
                    idx.offset(d.start.line, d.start.col),
                    idx.offset(d.end.line, d.end.col),
                )
            })
        })
    }
}

/// What machinery a resolver needs for a use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    /// Lexical: scope / import / path / assembly-index. No type inference.
    B1,
    /// Shallow inference: a receiver type for a single-candidate member/field.
    B2,
    /// Hard pile: overload resolution or extension-member search. (Active-pattern
    /// and union *cases* are B1 — they resolve by a name index, not inference.)
    B3,
    /// Unclassified (a symbol category the taxonomy does not place).
    Other,
}

/// Bucket one use, returning `None` for a defining occurrence (excluded from the
/// resolution denominator — a definition is not a name to resolve) plus a
/// human-readable sub-tag for the histogram. This is the taxonomy; keep it the
/// single copy so the two census tests cannot drift apart.
pub fn classify(u: &CensusUse) -> (Option<Bucket>, &'static str) {
    if u.is_from_definition {
        return (None, "definition-occurrence");
    }
    let tag = |b: Bucket, s: &'static str| (Some(b), s);
    match u.class.as_str() {
        "Entity" if u.is_namespace => tag(Bucket::B1, "entity:namespace"),
        "Entity" if u.is_module => tag(Bucket::B1, "entity:module"),
        "Entity" => tag(Bucket::B1, "entity:type"),
        "GenericParameter" => tag(Bucket::B1, "type-parameter"),
        "UnionCase" => tag(Bucket::B1, "union-case"),
        // An active-pattern *case* in a pattern (`match x with Even -> …`)
        // resolves by name to its defining `(|Even|Odd|)` function via an
        // active-pattern index — name resolution, not inference: the scrutinee
        // type is needed to *check* the match, not to *resolve the case name*
        // (go-to-def / find-refs work without it). So B1, alongside union cases —
        // not an inference hard pile. (The `(|…|)` *function* is a plain value
        // binding and lands in B1 via the `!is_member` arm below.)
        "ActivePatternCase" => tag(Bucket::B1, "active-pattern-case"),
        "Field" => tag(Bucket::B2, "record/class-field"),
        "Parameter" => tag(Bucket::B1, "parameter"),
        "Mfv" => {
            if !u.is_member {
                if u.is_module_value_or_member {
                    tag(Bucket::B1, "value:module-or-import")
                } else {
                    tag(Bucket::B1, "value:local-or-param")
                }
            } else if u.is_extension {
                tag(Bucket::B3, "member:extension")
            } else if u.is_active_pattern {
                // A member active pattern is exotic, but its name still resolves
                // lexically (see the ActivePatternCase arm) — not inference.
                tag(Bucket::B1, "active-pattern-fn")
            } else if u.is_constructor {
                if u.is_overloaded {
                    tag(Bucket::B3, "constructor:overloaded")
                } else {
                    tag(Bucket::B1, "constructor")
                }
            } else if !u.is_instance {
                // Static member: reached via a *type* path, which name resolution
                // already handles (Phase 2's `System.Console.WriteLine`). The
                // method group resolves without inference; picking an overload
                // would still need it, hence the sub-tag, but the name is B1.
                if u.is_overloaded {
                    tag(Bucket::B1, "static-member:overloaded(group)")
                } else {
                    tag(Bucket::B1, "static-member")
                }
            } else if u.is_overloaded {
                tag(Bucket::B3, "instance-member:overloaded")
            } else {
                tag(Bucket::B2, "instance-member:simple")
            }
        }
        _ => (Some(Bucket::Other), "other"),
    }
}

// ============================================================================
// Resolution corpus-diff projection — the name-resolution sweep currency
// ============================================================================

/// One census use, normalised for the resolution corpus-diff
/// (`crates/sema/tests/all/resolve_corpus_diff.rs`): the use range as byte offsets,
/// the declaration range *when it lies in this file*, the [`Bucket`] it
/// classifies into, and FCS's defining-occurrence flag. The location-bearing
/// sibling of the bucket-only census view: same per-use facts as
/// [`NormalisedUse`], plus the classification the sweep needs to restrict to the
/// lexical (B1) slice a pure name-resolver can reproduce.
#[derive(Debug, Clone)]
pub struct ResolveDiffUse {
    /// Half-open byte range of the reference into the checked source.
    pub start: usize,
    pub end: usize,
    /// FCS's defining-occurrence flag (a definition is not a name to resolve).
    pub is_from_definition: bool,
    /// The declaration range as byte offsets *when the declaration lies in this
    /// file* (file-equality test, as [`parse_fcs_uses`]). `None` for
    /// referenced-assembly / FSharp.Core declarations — out of the sweep's
    /// in-file slice.
    pub decl: Option<(usize, usize)>,
    /// What machinery a resolver needs (see [`classify`]); `None` for a defining
    /// occurrence. The sweep keeps only [`Bucket::B1`].
    pub bucket: Option<Bucket>,
}

/// Normalise one census file's uses against its source text. `source` must be
/// the exact text of `file` (offsets index into it). Reuses the census
/// [`classify`] taxonomy and the [`parse_fcs_uses`] in-file-declaration test, so
/// the resolution sweep and the bucket census cannot drift apart.
pub fn census_resolve_uses(file: &FileCensus, source: &str) -> Vec<ResolveDiffUse> {
    let idx = LineIndex::new(source);
    file.uses
        .iter()
        .map(|u| {
            let (bucket, _tag) = classify(u);
            let decl = u.decl_range.as_ref().and_then(|d| {
                // The use's own range is always in the checked file; an in-file
                // declaration shares that file (see `parse_fcs_uses`).
                (d.file == u.range.file).then(|| {
                    (
                        idx.offset(d.start.line, d.start.col),
                        idx.offset(d.end.line, d.end.col),
                    )
                })
            });
            ResolveDiffUse {
                start: idx.offset(u.range.start.line, u.range.start.col),
                end: idx.offset(u.range.end.line, u.range.end.col),
                is_from_definition: u.is_from_definition,
                decl,
                bucket,
            }
        })
        .collect()
}

// ============================================================================
// Type census classification — the Phase-3 *type* scoping taxonomy
// ============================================================================

/// One file's type-census result (`{ Path, Ok, Exprs }` JSON line) from
/// `fcs-dump types-census-batch`. Parallels [`FileCensus`] but the population is
/// FCS's elaborated *expression* nodes (each carrying an inferred type), not
/// symbol uses.
#[derive(Deserialize)]
pub struct FileTypeCensus {
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Ok")]
    pub ok: bool,
    #[serde(rename = "Exprs", default)]
    pub exprs: Vec<CensusExpr>,
}

/// One typed expression node: its machinery `kind` (set by fcs-dump's
/// `classifyExpr`) and FCS's rendered inferred type. The `Range` field in the
/// JSON is ignored here (the census buckets by kind, not location).
#[derive(Deserialize)]
pub struct CensusExpr {
    #[serde(rename = "Kind")]
    pub kind: String,
    #[serde(rename = "Type")]
    pub ty: String,
}

/// What machinery a resolver needs to assign an *expression* its type. The
/// type-side analogue of [`Bucket`]; it diverges from the name-resolution
/// taxonomy in two principled places, because *typing an expression* is a
/// different question from *resolving a name*:
///
/// * An **overloaded static call** is name-`B1` (the method *group* resolves by
///   a type path) but type-`H`: its *return type* is unknown until overload
///   resolution picks a candidate.
/// * An **overloaded constructor** is name-`B3` but type-`S`: `new T(…)` has
///   type `T` regardless of which `.ctor` is chosen, so the overload is
///   irrelevant to the *expression's* type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeBucket {
    /// Literal: the constant's primitive type. Reproducible by Phase 3.1 alone.
    Lit,
    /// Lexical / HM spine: leaves (value refs), function application, lambdas,
    /// `let`/`if`/tuples/records/coercions, static calls, constructors — typed
    /// by the unification spine with **no type-directed member lookup**.
    Spine,
    /// Member lookup (shallow inference): a single-candidate instance member or
    /// instance field, whose type needs the *receiver* type first.
    Member,
    /// Hard pile: overloaded instance/static call, extension member, or SRTP
    /// trait call — the type needs overload resolution / constraint solving.
    Hard,
    /// A kind the taxonomy does not place (kept visible, never silently merged).
    Other,
}

/// Bucket one expression node by its fcs-dump `kind` tag. Single source of truth
/// for the type census; mirrors [`classify`] for the uses census.
pub fn classify_expr(kind: &str) -> TypeBucket {
    match kind {
        "const" => TypeBucket::Lit,

        // Receiver-type-dependent, single candidate → shallow inference. Anon-
        // record field reads (`r.X`) need the receiver's structural type to know
        // the field's type, exactly like a nominal instance field.
        "call:instance" | "field-get:instance" | "il-field-get:instance" | "anon-record-get" => {
            TypeBucket::Member
        }

        // Need overload resolution / extension search / SRTP to know the type.
        "call:instance-overloaded" | "call:static-overloaded" | "call:extension" | "trait-call" => {
            TypeBucket::Hard
        }

        // Everything else is typed by the lexical / HM spine: value references
        // (local and module), genuine function/static calls, constructors
        // (`new T(…)` is `T` even when overloaded), lambdas, control flow,
        // tuples / records / unions / arrays, coercions, pattern-match
        // elaboration, etc.
        "value"
        | "value:module"
        | "call:function"
        | "call:static"
        | "application"
        | "lambda"
        | "type-lambda"
        | "if"
        | "let"
        | "let-rec"
        | "sequential"
        | "new-tuple"
        | "new-record"
        | "new-anon-record"
        | "new-union-case"
        | "new-array"
        | "new-object"
        | "new-object-overloaded"
        | "new-delegate"
        | "coerce"
        | "type-test"
        | "tuple-get"
        | "union-case-get"
        | "field-get:static"
        | "decision-tree"
        | "decision-tree-success"
        | "try-with"
        | "try-finally"
        | "while"
        | "for"
        | "quote"
        | "object-expr"
        | "this-value"
        | "base-value"
        | "default-value"
        | "value-set"
        | "address-of"
        | "address-set"
        | "field-set"
        | "union-case-tag"
        | "union-case-test"
        | "union-case-set"
        | "witness-arg"
        | "il-field-get:static"
        | "il-field-set"
        | "il-asm" => TypeBucket::Spine,

        _ => TypeBucket::Other,
    }
}

/// Parse the type-census JSONL (one [`FileTypeCensus`] per non-blank line).
pub fn parse_type_census_jsonl(json: &str) -> Vec<FileTypeCensus> {
    json.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("type census line is valid JSON"))
        .collect()
}

/// Read a `usize`-valued environment variable, falling back to `default` when
/// unset or unparseable. Used to tune census sample size / project-prefix.
pub fn env_usize_or(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Parse the census JSONL (one `FileCensus` per non-blank line).
pub fn parse_census_jsonl(json: &str) -> Vec<FileCensus> {
    json.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("census line is valid JSON"))
        .collect()
}

/// Bucket counts (`[B1, B2, B3, Other]`) plus the sub-tag histogram for a set of
/// uses. Definitions are excluded from the bucket counts (see [`classify`]).
#[derive(Default)]
pub struct Tally {
    pub buckets: [u64; 4],
    pub subtags: std::collections::BTreeMap<&'static str, u64>,
}

impl Tally {
    pub fn add<'a>(&mut self, uses: impl Iterator<Item = &'a CensusUse>) {
        for u in uses {
            let (b, tag) = classify(u);
            *self.subtags.entry(tag).or_default() += 1;
            match b {
                Some(Bucket::B1) => self.buckets[0] += 1,
                Some(Bucket::B2) => self.buckets[1] += 1,
                Some(Bucket::B3) => self.buckets[2] += 1,
                Some(Bucket::Other) => self.buckets[3] += 1,
                None => {}
            }
        }
    }

    /// Non-definition uses (the resolution denominator).
    pub fn nondef(&self) -> u64 {
        self.buckets.iter().sum()
    }

    /// Fraction of non-definition uses that need any inference (B2 + B3).
    pub fn needs_inference_pct(&self) -> f64 {
        let n = self.nondef();
        if n == 0 {
            0.0
        } else {
            100.0 * (self.buckets[1] + self.buckets[2]) as f64 / n as f64
        }
    }
}

/// Write `source` to a uniquely-named temp `.fs` file (parallel-safe) and return
/// the path. `label` distinguishes co-existing files of one test; the pid +
/// monotonic counter keep names distinct across processes and calls. Left on
/// disk for the caller to clean up after the `fcs-dump` child has read it.
pub fn temp_fs_file(label: &str, source: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("borzoi_sema_{label}_{}_{n}.fs", std::process::id()));
    std::fs::write(&path, source).expect("write temp .fs");
    path
}

/// Like [`temp_fs_file`], but materialises a whole **file tree** under a fresh
/// unique root, with caller-controlled relative paths (file name, extension,
/// and directory included). The signature fixtures need this: FCS's
/// `QualifiedNameOfFile` derivation reads the real file *stem* (the
/// filename-derived case) and *directory* (the deduplication key), so the
/// per-call random names `temp_fs_file` generates would change the semantics
/// under test. Returns the root (remove it with `remove_dir_all` when done)
/// and the absolute path + source of each file, in input order.
pub fn temp_fs_tree(label: &str, files: &[(&str, &str)]) -> (PathBuf, Vec<(PathBuf, String)>) {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("borzoi_sema_{label}_{}_{n}", std::process::id()));
    let written = files
        .iter()
        .map(|(rel, src)| {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create temp tree dir");
            }
            std::fs::write(&path, src).expect("write temp tree file");
            (path, (*src).to_string())
        })
        .collect();
    (root, written)
}

/// Build the sema assembly fixture (`tests/fixtures/assembly_env`,
/// `SemaAssemblyEnvFixture.dll`) once per test binary and return its `.dll`
/// path. Shared by the `AssemblyEnv` index tests and the assembly-resolution
/// differential, which both treat it as a referenced assembly.
///
/// Honours `BORZOI_ASSEMBLY_FIXTURE_DLL` (an absolute path to a
/// pre-built `SemaAssemblyEnvFixture.dll`) when set; CI and any
/// concurrent-cargo caller should set it to skip the in-test `dotnet build`
/// entirely. The build path holds [`BUILD_LOCK`] so it can't overlap an
/// `ensure_fcs_dump_built` call on another thread.
pub fn ensure_assembly_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            if let Some(prebuilt) = std::env::var_os("BORZOI_ASSEMBLY_FIXTURE_DLL") {
                return PathBuf::from(prebuilt);
            }
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assembly_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(&project, "dotnet build assembly fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaAssemblyEnvFixture.dll")
        })
        .as_path()
}

/// Build the **auto-open** fixture (`tests/fixtures/autoopen_env`,
/// `SemaAutoOpenFixture.dll`) once per test binary and return its `.dll` path.
///
/// Three case groups reference it (`assembly_env`, `extension_visibility_matrix`,
/// `resolve_autoopen`), so it must live here behind the shared [`BUILD_LOCK`]: a
/// per-group `OnceLock` de-dupes within one group, but folded into a single
/// binary the groups' tests run concurrently, and independent builders would race
/// on the fixture's `obj/`/`bin/`. Routing all three through this one builder is
/// what makes the build happen once and under the lock.
pub fn ensure_autoopen_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/autoopen_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(&project, "dotnet build autoopen fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaAutoOpenFixture.dll")
        })
        .as_path()
}

/// Build the **F# abbrev** fixture (`tests/fixtures/fsharp_abbrev_env`,
/// `SemaFSharpAbbrevFixture.dll`) once per test binary and return its `.dll`
/// path. Shared by the cross-assembly merge tests (module-open plan, review
/// rounds 5/7/15), so it must live here behind the shared [`BUILD_LOCK`]: an
/// uncached `dotnet build` racing another fixture's build fails writing
/// `…deps.json` while the other process holds it (round 8 reproduced it).
pub fn ensure_abbrev_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fsharp_abbrev_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(&project, "dotnet build abbrev fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaFSharpAbbrevFixture.dll")
        })
        .as_path()
}

/// Build the **module-vs-type qualifier** fixture
/// (`tests/fixtures/qualifier_env`, `SemaQualifierFixture.dll`) once per test
/// binary and return its `.dll` path. The deliberate `Collide` module/type
/// bare-name collision for the qualifier-precedence differential
/// (`tests/all/resolve_qualifier_precedence_diff.rs`); behind the shared
/// [`BUILD_LOCK`] like the other fixtures.
pub fn ensure_qualifier_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qualifier_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(&project, "dotnet build qualifier fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaQualifierFixture.dll")
        })
        .as_path()
}

/// Build the **active-pattern** fixture (`tests/fixtures/active_pattern_env`,
/// `SemaActivePatternFixture.dll`) once per test binary and return its `.dll`
/// path. A referenced F# library exposing active-pattern recognizers of every
/// shape, so the assembly-side use-site split (export-decl plan Stage 3b) can be
/// diffed against FCS. Behind the shared [`BUILD_LOCK`] like the other fixtures.
pub fn ensure_active_pattern_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/active_pattern_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(&project, "dotnet build active-pattern fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaActivePatternFixture.dll")
        })
        .as_path()
}

/// Build the **OV-9 overload-corpus** fixture
/// (`tests/fixtures/overload_corpus`, `OverloadCorpus.dll`) once per test binary
/// and return its `.dll` path. `csharp` is the generated universe
/// ([`overload_corpus::corpus`]`().csharp`), written to `Generated.cs` inside the
/// fixture project immediately before the build — the C# source is *not* checked
/// in, so the assembly and the Rust generator cannot drift apart.
///
/// Honours `BORZOI_OVERLOAD_CORPUS_DLL` (a path to a pre-built assembly) so
/// CI / concurrent-cargo callers can skip the in-test `dotnet build`; the build
/// path holds [`BUILD_LOCK`], like the other fixtures.
///
/// The write is unconditional but *deterministic*: two concurrent cargo
/// invocations write byte-identical content, so they cannot corrupt each other's
/// source (they can still race on `obj/`, exactly as the other fixtures can —
/// see [`BUILD_LOCK`]).
pub fn ensure_overload_corpus_built(csharp: &str) -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            if let Some(prebuilt) = std::env::var_os("BORZOI_OVERLOAD_CORPUS_DLL") {
                return PathBuf::from(prebuilt);
            }
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/overload_corpus");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            std::fs::write(project.join("Generated.cs"), csharp).expect("write Generated.cs");
            dotnet_build(&project, "dotnet build overload corpus fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("OverloadCorpus.dll")
        })
        .as_path()
}

/// Build `tools/fcs-dump` once (thread-safe) and return the path to the
/// produced `.dll`. Only the first caller pays the `dotnet build` cost.
/// Honours `BORZOI_FCS_DUMP` via [`fcs_dump_command`] — that path
/// never reaches this function.
fn ensure_fcs_dump_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project = project_dir();
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(&project, "dotnet build fcs-dump");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("fcs-dump.dll")
        })
        .as_path()
}

/// Run a `dotnet build` of `project` under a deadline, failing loudly (with the
/// build's own output) if it errors or never finishes. `what` names it in the
/// panic.
///
/// A cold build — restoring packages, compiling — is legitimately minutes, so the
/// budget sits far above the harness's per-request default; it is there to stop a
/// build that has *stalled* (blocked on a NuGet lock held by a concurrent run in a
/// sibling worktree, say) from hanging the suite forever, not to police a slow one.
fn dotnet_build(project: &Path, what: &str) {
    let mut cmd = Command::new("dotnet");
    cmd.args(["build", "-c", "Release", "--nologo"])
        .arg(project);
    BoundedCommand::new(cmd).timeout(BUILD_TIMEOUT).run_ok(what);
}

/// Budget for one `dotnet build`. See [`dotnet_build`].
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

/// Path to a real `FSharp.Core.dll` that is *always present* in the checkout.
///
/// Building `tools/fcs-dump` copies the FSharp.Compiler.Service dependency's
/// `FSharp.Core.dll` into the same output directory as `fcs-dump.dll`, so we
/// reuse the build-once helper and return the sibling. This lets a sema test
/// resolve against the genuine shipped FSharp.Core in every lane (the
/// `BORZOI_FCS_DUMP` self-contained-binary override is *not* honoured
/// here — the sibling `.dll` only exists in a `dotnet build` output dir, so
/// this path always builds `fcs-dump`). Mirrors the assembly crate's helper.
pub fn ensure_fsharp_core_dll() -> PathBuf {
    ensure_fcs_dump_built()
        .parent()
        .expect("fcs-dump.dll has a parent dir")
        .join("FSharp.Core.dll")
}

/// A real BCL `System.Runtime.dll` **reference assembly** — the one carrying the
/// public API surface of `System.String` (its `Length` property, `Chars`
/// indexer, `Empty` static field, …). The member-access differential
/// (`tests/all/infer_member_access_diff.rs`) builds an [`AssemblyEnv`] from it so our
/// side has the same `System.String` members FCS references when it type-checks
/// the same script against the SDK's real BCL — the two then agree on
/// `s.Length : System.Int32`.
///
/// Located inside the SDK's `Microsoft.NETCore.App.Ref` pack rooted at
/// `$DOTNET_ROOT`. Honour `BORZOI_SYSTEM_RUNTIME_DLL` for CI (an explicit
/// path skips the pack search). Panics with a clear message if neither is
/// available, so a missing devShell fails loudly rather than silently skipping
/// the differential.
///
/// **Pack selection**: a `DOTNET_ROOT` may hold several major ref packs (e.g.
/// `10.0.8` *and* `9.0.10`), and a naive version sort mis-picks — `"9.0.10"`
/// sorts *after* `"10.0.0"` lexicographically. Instead, pick any pack that
/// actually contains `ref/net10.0/System.Runtime.dll` (they all expose the same
/// net10 API surface), which sidesteps version-string ordering entirely.
pub fn ensure_system_runtime_dll() -> PathBuf {
    if let Some(explicit) = std::env::var_os("BORZOI_SYSTEM_RUNTIME_DLL") {
        return PathBuf::from(explicit);
    }
    let dotnet_root = std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .expect("DOTNET_ROOT unset (run under `nix develop`, or set BORZOI_SYSTEM_RUNTIME_DLL)");
    let packs = dotnet_root.join("packs").join("Microsoft.NETCore.App.Ref");
    std::fs::read_dir(&packs)
        .unwrap_or_else(|e| panic!("read ref packs dir {}: {e}", packs.display()))
        .filter_map(|e| e.ok())
        .map(|e| {
            e.path()
                .join("ref")
                .join("net10.0")
                .join("System.Runtime.dll")
        })
        .find(|p| p.exists())
        .unwrap_or_else(|| {
            panic!(
                "no ref/net10.0/System.Runtime.dll under any pack in {}",
                packs.display()
            )
        })
}

/// The env a real F# project effectively resolves under, shared once per test
/// binary: the real `FSharp.Core.dll` plus `System.Runtime.dll` and the
/// `netstandard` facade beside it. The facade matters: FSharp.Core's
/// signature pickle names its BCL abbreviation targets through the
/// `netstandard` CCU, so the primitive-alias chase (`int` → `int32` → a
/// `netstandard` type forwarder → `System.Int32`) needs all three loaded.
pub fn full_bcl_env() -> &'static borzoi_sema::AssemblyEnv {
    use std::sync::OnceLock;
    static ENV: OnceLock<borzoi_sema::AssemblyEnv> = OnceLock::new();
    ENV.get_or_init(|| {
        use borzoi_assembly::Ecma335Assembly;
        let core = std::fs::read(ensure_fsharp_core_dll()).expect("read FSharp.Core.dll");
        let sysrt_path = ensure_system_runtime_dll();
        let netstd_path = sysrt_path
            .parent()
            .expect("ref dir")
            .join("netstandard.dll");
        let sysrt = std::fs::read(&sysrt_path).expect("read System.Runtime.dll");
        let netstd = std::fs::read(&netstd_path).expect("read netstandard.dll");
        let views = vec![
            Ecma335Assembly::parse(&core).expect("parse FSharp.Core.dll"),
            Ecma335Assembly::parse(&sysrt).expect("parse System.Runtime.dll"),
            Ecma335Assembly::parse(&netstd).expect("parse netstandard.dll"),
        ];
        borzoi_sema::AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
    })
}

// ============================================================================
// Line/column → byte offset
// ============================================================================

/// Lookup byte offset for an FCS `(line, col)` position. FCS uses 1-based
/// lines and 0-based columns, and columns count **UTF-16 code units** — so
/// `col` cannot just be added to the line's byte start. `offset` walks the
/// line a char at a time, accumulating UTF-16 units until it reaches `col`,
/// and returns the byte position.
pub struct LineIndex<'a> {
    source: &'a str,
    /// Byte offset of the start of each line (1-indexed: `starts[1]` is line 1).
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        // `starts[0]` is unused so we can index by 1-based line number.
        let mut starts = vec![0, 0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { source, starts }
    }

    pub fn offset(&self, line: u32, col: u32) -> usize {
        let line = line as usize;
        let col = col as usize;
        // FCS sometimes reports end positions one past the last line
        // (line = lastLine+1, col = 0). Clamp those to the source length.
        if line >= self.starts.len() {
            return self.source.len();
        }
        let base = self.starts[line];
        let line_end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source.len());

        let mut units = 0usize;
        let mut byte_pos = base;
        for ch in self.source[base..line_end].chars() {
            if units >= col {
                break;
            }
            let next_units = units + ch.len_utf16();
            // `col` falls inside a surrogate pair — defensive clamp to the
            // char boundary just before. FCS only emits col positions at char
            // boundaries, so this only fires on malformed input.
            if next_units > col {
                break;
            }
            units = next_units;
            byte_pos += ch.len_utf8();
        }
        byte_pos.min(self.source.len())
    }
}

// ============================================================================
// `uses` dump projection — the name-resolution oracle currency
// ============================================================================

/// A single symbol use reported by FCS, normalised to byte offsets into the
/// checked source. The diff currency for the Stage C name-resolution oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedUse {
    /// `FSharpSymbol.DisplayName` of the symbol referenced at this use.
    pub name: String,
    /// Half-open byte range of the reference into the checked source.
    pub start: usize,
    pub end: usize,
    /// Whether FCS marks this use as the symbol's defining occurrence.
    pub is_from_definition: bool,
    /// The symbol's declaration range as byte offsets, *when the declaration
    /// lies in the checked file*. FCS reports a declaration range for symbols
    /// declared anywhere (including referenced assemblies / FSharp.Core); we
    /// only convert it — meaningfully against this source — when its `File`
    /// equals the use's own `File`. Otherwise `None`.
    pub decl: Option<(usize, usize)>,
    /// The declaring assembly's simple name (`FSharpSymbol.Assembly.SimpleName`),
    /// or `None` when FCS cannot produce it. The currency for matching a
    /// referenced-assembly resolution, whose declaration range is unreliable.
    pub assembly: Option<String>,
    /// The symbol's full name (`FSharpSymbol.FullName`, e.g. `Demo.Calc.Zero`),
    /// or `None` when FCS cannot produce it.
    pub full_name: Option<String>,
}

#[derive(Deserialize)]
struct UsesDump {
    #[serde(rename = "Uses")]
    uses: Vec<RawUse>,
}

#[derive(Deserialize)]
struct RawUse {
    #[serde(rename = "SymbolName")]
    symbol_name: String,
    #[serde(rename = "Range")]
    range: FcsRange,
    #[serde(rename = "IsFromDefinition")]
    is_from_definition: bool,
    #[serde(rename = "DeclRange")]
    decl_range: Option<FcsRange>,
    // `Assembly` / `FullName` are emitted by the current fcs-dump but absent in
    // older JSON; default to `None` so both shapes deserialise.
    #[serde(rename = "Assembly", default)]
    assembly: Option<String>,
    #[serde(rename = "FullName", default)]
    full_name: Option<String>,
}

#[derive(Deserialize)]
struct FcsRange {
    #[serde(rename = "File")]
    file: String,
    #[serde(rename = "Start")]
    start: FcsPos,
    #[serde(rename = "End")]
    end: FcsPos,
}

#[derive(Deserialize)]
struct FcsPos {
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Col")]
    col: u32,
}

/// Parse the `uses` subcommand's JSON into byte-offset-normalised uses.
/// `source` must be the exact text of the checked file (offsets index into it).
pub fn parse_fcs_uses(json: &str, source: &str) -> Vec<NormalisedUse> {
    let dump: UsesDump = serde_json::from_str(json).expect("fcs-dump uses JSON shape");
    let idx = LineIndex::new(source);
    dump.uses
        .into_iter()
        .map(|u| {
            let decl = u.decl_range.and_then(|d| {
                // The use's own range is always in the checked file; an in-file
                // declaration shares that file, so file equality is the
                // in-file test (no need to thread the path in separately).
                (d.file == u.range.file).then(|| {
                    (
                        idx.offset(d.start.line, d.start.col),
                        idx.offset(d.end.line, d.end.col),
                    )
                })
            });
            NormalisedUse {
                name: u.symbol_name,
                start: idx.offset(u.range.start.line, u.range.start.col),
                end: idx.offset(u.range.end.line, u.range.end.col),
                is_from_definition: u.is_from_definition,
                decl,
                assembly: u.assembly,
                full_name: u.full_name,
            }
        })
        .collect()
}

// ============================================================================
// `attrs` dump projection — the attribute-resolution oracle currency
// ============================================================================

/// One attribute-type resolution reported by FCS (an
/// `ItemOccurrence.UseInAttribute` entity use), normalised to byte offsets.
/// The diff currency for the EX-3 §2(d) attribute-resolution differential
/// (`tests/all/attr_resolution_diff.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedAttr {
    /// `FSharpEntity.DisplayName` of the resolved attribute type (already
    /// suffixed: `[<Literal>]` reports `LiteralAttribute`).
    pub name: String,
    /// Half-open byte range of the *written* attribute name (the full dotted
    /// path as written, not the synthesized suffix candidate).
    pub start: usize,
    pub end: usize,
    /// The resolved entity's declaration range as byte offsets, *when the
    /// declaration lies in the checked file* — the matching currency for a
    /// project-declared attribute type, which has no referenced-assembly
    /// identity. `None` for a referenced-assembly or out-of-file declaration,
    /// exactly as [`NormalisedUse::decl`].
    pub decl: Option<(usize, usize)>,
    /// The resolved entity's declaring assembly simple name / full name —
    /// the same referenced-assembly matching currency as
    /// [`NormalisedUse::assembly`] / [`NormalisedUse::full_name`].
    pub assembly: Option<String>,
    pub full_name: Option<String>,
    /// The *terminal* entity after chasing an abbreviation chain: for
    /// `type MyExt = ExtensionAttribute`, `full_name` names the abbreviation
    /// and `target_full_name` names `ExtensionAttribute`. Equal to the
    /// resolved entity when it is not an abbreviation; `None` when the chase
    /// found no terminal (an opaque or over-long chain) — unknowable, never
    /// "not of interest".
    pub target_assembly: Option<String>,
    pub target_full_name: Option<String>,
    /// `true` when FCS sank **distinct** entities at this range (an attribute
    /// on a type parameter records both the built-in special attribute and a
    /// same-named local): the record marks an attribute with *no claim* about
    /// its target — a differential can neither confirm nor refute a
    /// commitment there.
    pub ambiguous: bool,
}

/// A check error reported alongside the `attrs` dump. An attribute FCS cannot
/// resolve sinks *no* record, so the errors are what distinguish "no
/// attributes" from "attributes it could not resolve".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FcsCheckError {
    pub line: u32,
    pub code: u32,
    pub message: String,
}

/// The `attrs` subcommand's payload, normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrsOracle {
    /// Source order (the op sorts by range start).
    pub attrs: Vec<NormalisedAttr>,
    /// Every `Severity=Error` diagnostic of the check.
    pub errors: Vec<FcsCheckError>,
}

#[derive(Deserialize)]
struct AttrsDump {
    #[serde(rename = "Attrs")]
    attrs: Vec<RawAttr>,
    #[serde(rename = "Errors")]
    errors: Vec<RawAttrError>,
}

#[derive(Deserialize)]
struct RawAttr {
    #[serde(rename = "SymbolName")]
    symbol_name: String,
    #[serde(rename = "Range")]
    range: FcsRange,
    #[serde(rename = "DeclRange")]
    decl_range: Option<FcsRange>,
    #[serde(rename = "Assembly")]
    assembly: Option<String>,
    #[serde(rename = "FullName")]
    full_name: Option<String>,
    #[serde(rename = "TargetAssembly")]
    target_assembly: Option<String>,
    #[serde(rename = "TargetFullName")]
    target_full_name: Option<String>,
    #[serde(rename = "Ambiguous", default)]
    ambiguous: bool,
}

#[derive(Deserialize)]
struct RawAttrError {
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Code")]
    code: u32,
    #[serde(rename = "Message")]
    message: String,
}

/// Normalise raw `attrs` records against the exact checked source text.
fn normalise_attrs(attrs: Vec<RawAttr>, errors: Vec<RawAttrError>, source: &str) -> AttrsOracle {
    let idx = LineIndex::new(source);
    AttrsOracle {
        attrs: attrs
            .into_iter()
            .map(|a| {
                let decl = a.decl_range.and_then(|d| {
                    // In-file test by file equality, as `parse_fcs_uses` does.
                    (d.file == a.range.file).then(|| {
                        (
                            idx.offset(d.start.line, d.start.col),
                            idx.offset(d.end.line, d.end.col),
                        )
                    })
                });
                NormalisedAttr {
                    name: a.symbol_name,
                    start: idx.offset(a.range.start.line, a.range.start.col),
                    end: idx.offset(a.range.end.line, a.range.end.col),
                    decl,
                    assembly: a.assembly,
                    full_name: a.full_name,
                    target_assembly: a.target_assembly,
                    target_full_name: a.target_full_name,
                    ambiguous: a.ambiguous,
                }
            })
            .collect(),
        errors: errors
            .into_iter()
            .map(|e| FcsCheckError {
                line: e.line,
                code: e.code,
                message: e.message,
            })
            .collect(),
    }
}

/// Parse the `attrs` subcommand's JSON into byte-offset-normalised attribute
/// resolutions. `source` must be the exact text of the checked file.
pub fn parse_fcs_attrs(json: &str, source: &str) -> AttrsOracle {
    let dump: AttrsDump = serde_json::from_str(json).expect("fcs-dump attrs JSON shape");
    normalise_attrs(dump.attrs, dump.errors, source)
}

/// Run `fcs-dump attrs-batch` over `paths` (each type-checked **in
/// isolation**, the SDK reference set harvested once) and return its JSONL
/// stdout — one `{ Path, Ok, Error, Attrs, Errors }` object per line. The
/// resident oracle for the generative and corpus attribute differentials.
pub fn invoke_fcs_dump_attrs_batch(paths: &[PathBuf]) -> String {
    census_driver("attrs-batch", paths, &[])
}

/// One `attrs-batch` line, normalised against its file's source text.
pub struct AttrsBatchEntry {
    pub path: String,
    /// `false` when the oracle could not check the file at all (reference
    /// resolution failed, type-check aborted, an exception) — `error` says
    /// why, and `oracle` is empty. A file with mere check *errors* is `ok`.
    pub ok: bool,
    pub error: String,
    pub oracle: AttrsOracle,
}

#[derive(Deserialize)]
struct RawAttrsBatchLine {
    #[serde(rename = "Path")]
    path: String,
    #[serde(rename = "Ok")]
    ok: bool,
    #[serde(rename = "Error", default)]
    error: String,
    #[serde(rename = "Attrs", default)]
    attrs: Vec<RawAttr>,
    #[serde(rename = "Errors", default)]
    errors: Vec<RawAttrError>,
}

/// Parse `attrs-batch` JSONL. `source_for` maps each line's `Path` back to
/// the exact source text that was checked (offsets normalise against it).
pub fn parse_fcs_attrs_batch(
    jsonl: &str,
    mut source_for: impl FnMut(&str) -> String,
) -> Vec<AttrsBatchEntry> {
    jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let raw: RawAttrsBatchLine =
                serde_json::from_str(line).expect("fcs-dump attrs-batch JSONL shape");
            let source = source_for(&raw.path);
            AttrsBatchEntry {
                oracle: normalise_attrs(raw.attrs, raw.errors, &source),
                path: raw.path,
                ok: raw.ok,
                error: raw.error,
            }
        })
        .collect()
}

// ============================================================================
// `uses-project` dump projection — the cross-file resolution oracle currency
// ============================================================================

/// Where a use's symbol is declared, normalised to a project file and a byte
/// range into *that* file. The Stage A cross-file oracle's distinguishing
/// feature over [`NormalisedUse`]: `file` may differ from the use's own file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclSite {
    /// The declaring project file (the file the harness was given whose name
    /// matches FCS's reported declaration file).
    pub file: PathBuf,
    /// Half-open byte range of the declaration into `file`.
    pub start: usize,
    pub end: usize,
}

/// A single symbol use reported by FCS for one project file, normalised to byte
/// offsets. Like [`NormalisedUse`] but the declaration carries *which* project
/// file it lives in (see [`DeclSite`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedProjectUse {
    pub name: String,
    /// Half-open byte range of the reference into this file.
    pub start: usize,
    pub end: usize,
    pub is_from_definition: bool,
    /// The declaration site, when it lies in one of the project files supplied
    /// to the harness. `None` for declarations in referenced assemblies /
    /// FSharp.Core (out of this slice's scope), exactly as the single-file
    /// projection drops them.
    pub decl: Option<DeclSite>,
    /// The declaring assembly's simple name, for matching a resolution *into a
    /// referenced assembly* (whose declaration range is unreliable) — the
    /// project analogue of [`NormalisedUse::assembly`]. `None` when FCS cannot
    /// produce it.
    pub assembly: Option<String>,
    /// The symbol's full name (`Demo.Calc.Zero`), or `None`.
    pub full_name: Option<String>,
}

/// All symbol uses FCS reported for one project file.
#[derive(Debug, Clone)]
pub struct FileUses {
    /// The project file these uses belong to (matched by file name to a
    /// supplied source).
    pub path: PathBuf,
    pub uses: Vec<NormalisedProjectUse>,
}

#[derive(Deserialize)]
struct ProjectUsesDump {
    #[serde(rename = "Files")]
    files: Vec<RawFileUses>,
}

#[derive(Deserialize)]
struct RawFileUses {
    #[serde(rename = "Path")]
    path: String,
    #[serde(rename = "Uses")]
    uses: Vec<RawUse>,
}

/// Parse the `uses-project` subcommand's JSON into per-file, byte-offset-
/// normalised uses with cross-file declaration sites.
///
/// `sources` is `(path, text)` for every file handed to the harness; offsets
/// are computed against the matching file's text. Files are matched to FCS's
/// reported paths by **file name** — robust against path normalisation (FCS
/// reports `Path.GetFullPath`, which does not resolve symlinks) and not
/// requiring the files to still exist on disk at parse time. Callers must give
/// the files distinct names (the [`temp_fs_file`] helper does).
pub fn parse_fcs_uses_project(json: &str, sources: &[(PathBuf, String)]) -> Vec<FileUses> {
    let dump: ProjectUsesDump =
        serde_json::from_str(json).expect("fcs-dump uses-project JSON shape");

    let by_name: HashMap<&OsStr, (&Path, &str)> = sources
        .iter()
        .map(|(p, src)| {
            (
                p.file_name().expect("source path has a file name"),
                (p.as_path(), src.as_str()),
            )
        })
        .collect();
    let lookup = |fcs_path: &str| -> Option<(&Path, &str)> {
        by_name.get(Path::new(fcs_path).file_name()?).copied()
    };

    dump.files
        .into_iter()
        .map(|f| {
            let (path, src) = lookup(&f.path)
                .unwrap_or_else(|| panic!("fcs reported uses for unknown file {:?}", f.path));
            let idx = LineIndex::new(src);
            let uses = f
                .uses
                .into_iter()
                .map(|u| {
                    let decl = u.decl_range.and_then(|d| {
                        lookup(&d.file).map(|(dpath, dsrc)| {
                            let didx = LineIndex::new(dsrc);
                            DeclSite {
                                file: dpath.to_path_buf(),
                                start: didx.offset(d.start.line, d.start.col),
                                end: didx.offset(d.end.line, d.end.col),
                            }
                        })
                    });
                    NormalisedProjectUse {
                        name: u.symbol_name,
                        start: idx.offset(u.range.start.line, u.range.start.col),
                        end: idx.offset(u.range.end.line, u.range.end.col),
                        is_from_definition: u.is_from_definition,
                        decl,
                        assembly: u.assembly,
                        full_name: u.full_name,
                    }
                })
                .collect();
            FileUses {
                path: path.to_path_buf(),
                uses,
            }
        })
        .collect()
}

// ============================================================================
// `types` dump projection — the inference oracle currency
// ============================================================================

#[derive(Deserialize)]
struct TypesDump {
    #[serde(rename = "Exprs")]
    exprs: Vec<RawTypedExpr>,
}

#[derive(Deserialize)]
struct RawTypedExpr {
    #[serde(rename = "Range")]
    range: FcsRange,
    /// Canonical (abbreviation-resolved BCL-FQN) rendering — the field the
    /// inference differential compares against [`borzoi_sema::Ty::render`].
    #[serde(rename = "TypeCanon")]
    type_canon: String,
}

/// Parse the `types` subcommand's JSON into a map from an expression's half-open
/// byte range `(start, end)` to FCS's canonical inferred type at that span.
/// `source` must be the exact text of the checked file (offsets index into it).
/// fcs-dump de-duplicates nodes by range, so each range appears at most once.
pub fn parse_fcs_types(
    json: &str,
    source: &str,
) -> std::collections::HashMap<(usize, usize), String> {
    let dump: TypesDump = serde_json::from_str(json).expect("fcs-dump types JSON shape");
    let idx = LineIndex::new(source);
    dump.exprs
        .into_iter()
        .map(|e| {
            let start = idx.offset(e.range.start.line, e.range.start.col);
            let end = idx.offset(e.range.end.line, e.range.end.col);
            ((start, end), e.type_canon)
        })
        .collect()
}

#[derive(Deserialize)]
struct BinderTypesDump {
    #[serde(rename = "Binders")]
    binders: Vec<RawBinder>,
}

#[derive(Deserialize)]
struct RawBinder {
    #[serde(rename = "Range")]
    range: FcsRange,
    /// Canonical (abbreviation-resolved BCL-FQN) rendering of the binder's type,
    /// the field the binder-type differential compares against
    /// [`borzoi_sema::Ty::render`].
    #[serde(rename = "TypeCanon")]
    type_canon: String,
}

/// Parse the `binder-types` subcommand's JSON into a map from a binder's
/// *declaration* half-open byte range `(start, end)` to FCS's canonical inferred
/// type there. `source` must be the exact text of the checked file (offsets index
/// into it). The range is a binder's `DeclarationLocation`, matching the `sema`
/// resolver's [`borzoi_sema::Def`]`::range`, so our `def_type` map keys
/// (a `DefId` → its `Def::range`) line up with this map's keys.
pub fn parse_fcs_binder_types(
    json: &str,
    source: &str,
) -> std::collections::HashMap<(usize, usize), String> {
    let dump: BinderTypesDump =
        serde_json::from_str(json).expect("fcs-dump binder-types JSON shape");
    let idx = LineIndex::new(source);
    dump.binders
        .into_iter()
        .map(|b| {
            let start = idx.offset(b.range.start.line, b.range.start.col);
            let end = idx.offset(b.range.end.line, b.range.end.col);
            ((start, end), b.type_canon)
        })
        .collect()
}

// ============================================================================
// `overloads` dump projection — the overload-resolution oracle (Stage OV-1)
// ============================================================================

#[derive(Deserialize)]
struct OverloadsDump {
    #[serde(rename = "Calls")]
    calls: Vec<RawCall>,
    #[serde(rename = "Errors", default)]
    errors: Vec<RawError>,
}

#[derive(Deserialize)]
struct RawError {
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Code")]
    code: u32,
    #[serde(rename = "Message")]
    message: String,
}

/// One error FCS reported while checking the file: its 1-based start line, the
/// `FS####` code, and the message.
///
/// The overload differential needs these because **an elaborated call node does
/// not mean FCS resolved the call**: FCS's single-`IsCandidate` shortcut
/// (`docs/overload-resolution-plan.md` §2.2) commits the lone arity-surviving
/// candidate with *no applicability test*, so `M("x")` against a sole `M(int)`
/// still names `M(int)` in the typed tree while raising an argument-type error.
/// Any claim about FCS's *applicability judgment* must therefore be restricted
/// to the sites FCS did not error on.
#[derive(Debug, Clone)]
pub struct FcsError {
    pub line: u32,
    pub code: u32,
    pub message: String,
}

#[derive(Deserialize)]
struct RawCall {
    #[serde(rename = "Range")]
    range: FcsRange,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Kind")]
    kind: String,
    #[serde(rename = "DeclaringType")]
    declaring_type: String,
    #[serde(rename = "Params")]
    params: Vec<Vec<String>>,
    #[serde(rename = "Return")]
    ret: String,
    #[serde(rename = "XmlDocSig")]
    xml_doc_sig: String,
}

/// One call node FCS elaborated, from the `overloads` oracle: the overload FCS
/// **chose** at a `Call`/`NewObject` site. `xml_doc_sig` identifies the chosen
/// member in one string; `params`/`ret` are its canonical-rendered signature
/// (the same currency as the `types`/`binder-types` oracles). `start`/`end` are
/// half-open byte offsets into the checked source.
///
/// Per `docs/overload-resolution-plan.md` §3.1: compare by *signature*
/// (`xml_doc_sig` + `params`), never gate on `kind` (`isOverloadedMember`
/// undercounts), and tolerate a *missing* call (out-arg/tuple-return folding can
/// erase the node — probe P12).
///
/// **A record is an *elaboration*, not a *resolution*** (OV-9). FCS's
/// single-`IsCandidate` shortcut (§2.2) commits the lone *arity*-surviving
/// candidate with **no applicability test** — so a sole `M(int)` called `M("x")`
/// still reports `M(int)` here, while FCS raises an argument-type error; and the
/// shortcut is arity-based *post-normalisation*, so it fires inside a
/// multi-candidate group too (`M(int)`/`M(int,int)` called `M(3, "x")` reports
/// `M(int,int)`). Any claim about FCS's *applicability judgment* must therefore
/// be restricted to sites FCS did not error on — see
/// [`parse_fcs_overloads_with_errors`] and [`FcsError`].
///
/// **`params`/`ret` are best-effort for a *generic* overload:** a typar inside a
/// generic *instantiation* (`'T list`) has no enclosing scope in the oracle's
/// canonicaliser, so it falls back to FCS display text (non-canonical, sensitive
/// to the source typar name). For a generic overload, key on [`Self::xml_doc_sig`]
/// — it *is* canonical and stable. Canonical generic-instantiation rendering is
/// the deferred "Ty generic args" work (plan §7); the engine defers generic
/// winners in v1 (§5), so nothing v1 needs relies on the generic `params` form.
#[derive(Debug, Clone)]
pub struct FcsCall {
    pub name: String,
    pub kind: String,
    pub declaring_type: String,
    pub params: Vec<Vec<String>>,
    pub ret: String,
    pub xml_doc_sig: String,
    pub start: usize,
    pub end: usize,
}

impl FcsCall {
    /// The single argument group flattened (a .NET method has one curried
    /// group). Convenience for the common single-group probe assertions.
    pub fn flat_params(&self) -> Vec<String> {
        self.params.iter().flatten().cloned().collect()
    }
}

/// Parse the `overloads` subcommand's JSON into the **invocation** nodes FCS
/// elaborated, in document order. `source` must be the exact text of the checked
/// file (offsets index into it).
///
/// **Range-keyed contract (like [`parse_fcs_types`]).** This is the set of
/// invocation nodes keyed by range, *not* a curated list of user-written call
/// sites. A consumer (the OV-6/OV-9 engine differential) selects the record at
/// the **range of the source call it is resolving** — synthesized invocations
/// the elaborated tree also contains (an implicit base constructor on a
/// type-name range, an inserted widening/`op_Implicit` conversion) sit at ranges
/// no consumer queries. The OV-1 probe tests select by call *name*, which is
/// equivalent for their single-call shapes. Non-invocation reads (plain
/// property/event accessors, module-value refs) are already excluded by the
/// oracle; genuine invocations — including compiler-synthesized ones — are kept.
pub fn parse_fcs_overloads(json: &str, source: &str) -> Vec<FcsCall> {
    parse_fcs_overloads_with_errors(json, source).0
}

/// [`parse_fcs_overloads`] plus the errors FCS reported (see [`FcsError`] for why
/// a consumer reasoning about FCS's *applicability judgment* — as opposed to its
/// elaborated tree — needs them).
pub fn parse_fcs_overloads_with_errors(json: &str, source: &str) -> (Vec<FcsCall>, Vec<FcsError>) {
    let dump: OverloadsDump = serde_json::from_str(json).expect("fcs-dump overloads JSON shape");
    let idx = LineIndex::new(source);
    let calls = dump
        .calls
        .into_iter()
        .map(|c| FcsCall {
            name: c.name,
            kind: c.kind,
            declaring_type: c.declaring_type,
            params: c.params,
            ret: c.ret,
            xml_doc_sig: c.xml_doc_sig,
            start: idx.offset(c.range.start.line, c.range.start.col),
            end: idx.offset(c.range.end.line, c.range.end.col),
        })
        .collect();
    let errors = dump
        .errors
        .into_iter()
        .map(|e| FcsError {
            line: e.line,
            code: e.code,
            message: e.message,
        })
        .collect();
    (calls, errors)
}
