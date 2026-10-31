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

pub mod companion_corpus;
pub mod corpus_manifest;
pub mod fold_matrix;
pub mod fsharp_member_corpus;
pub mod generator;
pub mod measure_corpus;
pub mod measure_fn_corpus;
pub mod member_hiding_corpus;
pub mod module_value_corpus;
pub mod overload_corpus;
pub mod scope_gen;
pub mod tier_corpus;

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
/// Honours `BORZOI_FCS_DUMP` (path to a pre-built framework-dependent binary)
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

/// Like [`invoke_fcs_dump`], but a check the resident handler reports as failed
/// comes back as `Err(message)` instead of a panic — for a corpus sweep, where
/// one file FCS cannot check is a count, not a failure of the sweep.
pub fn try_invoke_fcs_dump(subcommand: &str, source: &Path) -> Result<String, String> {
    try_invoke_fcs_dump_with_refs(subcommand, source, &[])
}

/// [`try_invoke_fcs_dump`], with `refs` resolvable to the snippet as
/// [`invoke_fcs_dump_with_refs`] makes them.
pub fn try_invoke_fcs_dump_with_refs(
    subcommand: &str,
    source: &Path,
    refs: &[&Path],
) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(rename = "BatchError")]
        batch_error: Option<String>,
    }
    let request = serde_json::json!({
        "kind": subcommand,
        "path": source.display().to_string(),
        "refs": refs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
    })
    .to_string();
    let line = fcs_file_batch_pool().request(&request);
    match serde_json::from_str::<Probe>(&line) {
        Ok(Probe {
            batch_error: Some(msg),
        }) => Err(msg),
        _ => Ok(line),
    }
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
/// honouring `BORZOI_FCS_DUMP` (a prebuilt framework-dependent binary) exactly as
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
    /// Whether FCS's check reported any error-severity diagnostic. Required, not
    /// defaulted: a missing field must not read as "checked cleanly".
    #[serde(rename = "HasCheckErrors")]
    pub has_check_errors: bool,
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
///
/// Ordered so `(tag, bucket)` pairs can live in a `BTreeSet` — the census tag
/// enumeration compares the whole pairing, not just the names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

/// Every sub-tag [`classify`] pairs with [`Bucket::B1`].
///
/// The resolution generator publishes a `gap_b1_by_tag` histogram, and a
/// histogram in `statistics` is a *metric namespace*: the dashboard offers one
/// plottable metric per key, so every key has to be there every run, zeros
/// included (see `docs/continuous-measurements.md`). Counting only the tags
/// that *occurred* would retire a metric the moment a construct stopped
/// appearing in the corpus — which is the good news of a gap being closed
/// reported as a measurement going missing.
///
/// Emitting zeros needs the whole list up front, and a hand-copied list beside
/// the taxonomy is exactly the kind of thing that drifts silently.
/// [`b1_tags_are_exactly_the_tags_classify_pairs_with_b1`] proves it is neither
/// short nor long by enumerating `classify` over every combination of the flags
/// it reads.
pub const B1_TAGS: [&str; 13] = [
    "active-pattern-case",
    "active-pattern-fn",
    "constructor",
    "entity:module",
    "entity:namespace",
    "entity:type",
    "parameter",
    "static-member",
    "static-member:overloaded(group)",
    "type-parameter",
    "union-case",
    "value:local-or-param",
    "value:module-or-import",
];

/// Every sub-tag [`classify`] can produce, in every bucket — the superset of
/// [`B1_TAGS`], paired with the bucket each tag belongs to.
///
/// The uses census publishes a `by_tag` histogram over the *whole* taxonomy
/// rather than the B1 slice, and it needs the same treatment for the same
/// reason: a key that appears only when it is non-zero is not a metric. The
/// bucket travels with the tag because the census also publishes per-bucket
/// totals, and deriving one from the other beats keeping two lists in step.
///
/// `"definition-occurrence"` is deliberately absent: `classify` returns it with
/// bucket `None`, and a defining occurrence is not a name to resolve, so it sits
/// outside the census denominator rather than in a bucket.
/// [`census_tags_are_exactly_what_classify_can_produce`] pins the list.
pub const CENSUS_TAGS: [(&str, Bucket); 19] = [
    ("active-pattern-case", Bucket::B1),
    ("active-pattern-fn", Bucket::B1),
    ("constructor", Bucket::B1),
    ("constructor:overloaded", Bucket::B3),
    ("entity:module", Bucket::B1),
    ("entity:namespace", Bucket::B1),
    ("entity:type", Bucket::B1),
    ("instance-member:overloaded", Bucket::B3),
    ("instance-member:simple", Bucket::B2),
    ("member:extension", Bucket::B3),
    ("other", Bucket::Other),
    ("parameter", Bucket::B1),
    ("record/class-field", Bucket::B2),
    ("static-member", Bucket::B1),
    ("static-member:overloaded(group)", Bucket::B1),
    ("type-parameter", Bucket::B1),
    ("union-case", Bucket::B1),
    ("value:local-or-param", Bucket::B1),
    ("value:module-or-import", Bucket::B1),
];

/// The symbol classes [`classify`] distinguishes, plus one it does not place.
const PROBE_CLASSES: [&str; 8] = [
    "Entity",
    "GenericParameter",
    "UnionCase",
    "ActivePatternCase",
    "Field",
    "Parameter",
    "Mfv",
    "AClassTheTaxonomyDoesNotPlace",
];

/// The flags [`classify`] reads.
const PROBE_FLAGS: [&str; 10] = [
    "IsFromDefinition",
    "IsNamespace",
    "IsModule",
    "IsMember",
    "IsInstance",
    "IsExtension",
    "IsConstructor",
    "IsModuleValueOrMember",
    "IsActivePattern",
    "IsOverloaded",
];

/// The size of the shape space [`classify_probe`] indexes: the symbol class
/// crossed with all 1,024 settings of the ten flags [`classify`] reads, 8,192
/// in total.
pub const CLASSIFY_PROBES: usize = PROBE_CLASSES.len() << PROBE_FLAGS.len();

/// Shape `index` (below [`CLASSIFY_PROBES`]) of everything [`classify`] can be
/// handed.
///
/// Enumerating the input space is how both tag lists below are established —
/// searching it exhaustively is cheaper than arguing about which arms of the
/// match are reachable, and it cannot go stale when an arm is added. Indexing
/// it lets a property test draw from the same space.
pub fn classify_probe(index: usize) -> CensusUse {
    assert!(index < CLASSIFY_PROBES, "probe {index} is out of range");
    let class = PROBE_CLASSES[index >> PROBE_FLAGS.len()];
    let bits = index & ((1 << PROBE_FLAGS.len()) - 1);
    let mut fields = serde_json::Map::new();
    fields.insert(
        "Range".into(),
        serde_json::json!({
            "File": "probe.fs",
            "Start": { "Line": 1, "Col": 0 },
            "End": { "Line": 1, "Col": 1 }
        }),
    );
    fields.insert("DeclRange".into(), serde_json::Value::Null);
    fields.insert("Class".into(), serde_json::json!(class));
    for (flag_index, flag) in PROBE_FLAGS.iter().enumerate() {
        fields.insert(
            (*flag).into(),
            serde_json::json!((bits >> flag_index) & 1 == 1),
        );
    }
    // Read by other consumers of the census, never by `classify`.
    for flag in ["IsProperty", "IsValue", "IsFunction"] {
        fields.insert(flag.into(), serde_json::json!(false));
    }
    serde_json::from_value(serde_json::Value::Object(fields))
        .expect("the probe carries every field the census use needs")
}

/// Every shape [`classify`] can be handed — [`classify_probe`] over its whole
/// index range.
#[cfg(test)]
fn classify_probe_space() -> Vec<CensusUse> {
    (0..CLASSIFY_PROBES).map(classify_probe).collect()
}

/// [`B1_TAGS`] is the whole of what [`classify`] can produce for
/// [`Bucket::B1`], established by enumeration rather than by reading the match.
#[test]
fn b1_tags_are_exactly_the_tags_classify_pairs_with_b1() {
    let mut produced = std::collections::BTreeSet::new();
    for probe in classify_probe_space() {
        if let (Some(Bucket::B1), tag) = classify(&probe) {
            produced.insert(tag);
        }
    }

    assert_eq!(
        produced,
        B1_TAGS
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
}

/// [`CENSUS_TAGS`] is the whole of what [`classify`] can produce, bucket
/// included — the same enumeration as
/// [`b1_tags_are_exactly_the_tags_classify_pairs_with_b1`], lifted off the B1
/// filter so the uses census can seed its histogram with every key.
///
/// Checking the *pairing* and not just the names matters: the census derives
/// its per-bucket totals from this table, so a tag filed under the wrong bucket
/// would move counts between B1/B2/B3 — the census's headline split — while
/// every key stayed present and every total stayed the same.
#[test]
fn census_tags_are_exactly_what_classify_can_produce() {
    let mut produced: std::collections::BTreeSet<(&str, Bucket)> =
        std::collections::BTreeSet::new();
    for probe in classify_probe_space() {
        if let (Some(bucket), tag) = classify(&probe) {
            produced.insert((tag, bucket));
        }
    }

    assert_eq!(
        produced,
        CENSUS_TAGS
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );

    // The B1 slice of the whole must be exactly the B1 list, so the two
    // constants cannot drift into disagreeing about a shared tag.
    let b1_of_census: std::collections::BTreeSet<&str> = CENSUS_TAGS
        .into_iter()
        .filter(|(_, bucket)| *bucket == Bucket::B1)
        .map(|(tag, _)| tag)
        .collect();
    assert_eq!(
        b1_of_census,
        B1_TAGS
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
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
    /// Where FCS declares the use's symbol, relative to the checked file
    /// (file-equality test, as [`parse_fcs_uses`]).
    pub decl: CensusDecl,
    /// What machinery a resolver needs (see [`classify`]); `None` for a defining
    /// occurrence.
    pub bucket: Option<Bucket>,
}

/// Where FCS declares the symbol a census use resolves to, relative to the
/// checked file.
///
/// Three cases rather than an `Option`, because "declared in another file" and
/// "FCS gave no declaration" are different claims: the first is evidence that
/// no binder of this file is the answer, the second is no evidence at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CensusDecl {
    /// Declared in this file, at this half-open byte range.
    InFile(usize, usize),
    /// Declared in a *different* file — a referenced assembly or FSharp.Core.
    /// FCS checked this file alone, so the target is not a binder of this file.
    OtherFile,
    /// FCS reported no declaration location for the symbol.
    Absent,
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
            // The use's own range is always in the checked file; an in-file
            // declaration shares that file (see `parse_fcs_uses`).
            let decl = match &u.decl_range {
                None => CensusDecl::Absent,
                Some(d) if d.file == u.range.file => CensusDecl::InFile(
                    idx.offset(d.start.line, d.start.col),
                    idx.offset(d.end.line, d.end.col),
                ),
                Some(_) => CensusDecl::OtherFile,
            };
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

/// Build the **duplicate-identity** fixture (`tests/fixtures/case_pattern_dup_env`)
/// once per test binary and return its `.dll` path. Its `<AssemblyName>` is
/// deliberately the *same* as [`ensure_abbrev_fixture_built`]'s, so loading both
/// gives an [`AssemblyEnv`](borzoi_sema::AssemblyEnv) two distinct DLLs sharing
/// one manifest identity — the shape a collision check keyed on the identity
/// rather than on per-DLL provenance silently merges. Behind the shared
/// [`BUILD_LOCK`] like the other fixtures.
pub fn ensure_case_pattern_dup_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/case_pattern_dup_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(
                &project,
                "dotnet build case-pattern duplicate-identity fixture",
            );
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaFSharpAbbrevFixture.dll")
        })
        .as_path()
}

/// Build the **retained manifest auto-open** fixture
/// (`tests/fixtures/case_pattern_autoopen_env`) once per test binary and return
/// its `.dll` path. Its `[<AutoOpen>] module Cases.Retained.Auto` declares
/// `Target`, the head `fsharp_abbrev_env`'s `Cases.Union` also declares, so
/// loading both puts a bare-scope reading no namespace-prefix walk can see above
/// one it can. Behind the shared [`BUILD_LOCK`] like the other fixtures.
pub fn ensure_case_pattern_autoopen_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/case_pattern_autoopen_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(&project, "dotnet build case-pattern auto-open fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaCasePatternAutoOpenFixture.dll")
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

/// Build the **unmodelled-contributor** fixture
/// (`tests/fixtures/contributor_env`, `SemaContributorFixture.dll`) once per
/// test binary and return its `.dll` path: referenced statics reaching bare
/// scope through a base class, an `[<AutoOpen>]` abbreviation, or a member the
/// projection drops (`tests/all/unmodelled_contributor_grid.rs`). Behind the
/// shared [`BUILD_LOCK`] like the other fixtures.
pub fn ensure_contributor_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contributor_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(&project, "dotnet build contributor fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaContributorFixture.dll")
        })
        .as_path()
}

/// Build the **computation-expression extension** fixture
/// (`tests/fixtures/ce_extension_env`, `SemaCeExtensionFixture.dll`) once per
/// test binary and return its `.dll` path: extension members adding custom
/// operations to FSharp.Core's `AsyncBuilder`. Behind the shared
/// [`BUILD_LOCK`] like the other fixtures.
pub fn ensure_ce_extension_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ce_extension_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(
                &project,
                "dotnet build computation-expression extension fixture",
            );
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaCeExtensionFixture.dll")
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

/// Build the **member-hiding** corpus (`tests/fixtures/member_hiding`) once per
/// test binary and return its `.dll` path, writing `csharp` as its `Generated.cs`
/// first.
///
/// One assembly, because the question it poses is about hiding *within* one
/// reference: which level of a base chain a member access reaches when several
/// declare the name. Contested names *across* assemblies are
/// [`ensure_tier_corpus_built`]'s. The write holds [`BUILD_LOCK`] and is
/// deterministic, like [`ensure_overload_corpus_built`]'s.
pub fn ensure_member_hiding_corpus_built(csharp: &str) -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/member_hiding");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            std::fs::write(project.join("Generated.cs"), csharp).expect("write Generated.cs");
            dotnet_build(&project, "dotnet build member-hiding fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("MemberHiding.dll")
        })
        .as_path()
}

/// Build the **F#-authored member** corpus (`tests/fixtures/fsharp_member_env`)
/// once per test binary and return its `.dll` path.
///
/// An F# fixture rather than the C# one next door: the metadata an F# compiler
/// emits is what every `<ProjectReference>` in a real solution hands us, and it
/// carries shapes C# never produces — record fields as properties, a case-test
/// property beside the case it tests, and a `[<CompiledName>]` whose compiled
/// name is not the name any F# source writes.
///
/// `Generated.fs` is not checked in: it is emitted from
/// [`fsharp_member_corpus`]'s table here, under the shared [`BUILD_LOCK`] like
/// every other fixture.
pub fn ensure_fsharp_member_corpus_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fsharp_member_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            std::fs::write(
                project.join("Generated.fs"),
                fsharp_member_corpus::fixture_source(),
            )
            .expect("write fsharp-member Generated.fs");
            dotnet_build(&project, "dotnet build fsharp-member fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join(format!("{}.dll", fsharp_member_corpus::FIXTURE_ASM))
        })
        .as_path()
}

/// Build the **unit-of-measure** fixture (`tests/fixtures/measure_env`,
/// `SemaMeasureFixture.dll`) once per test binary and return its `.dll` path.
///
/// `Generated.fs` is emitted from [`measure_corpus`]'s tables before the build,
/// so the fixture and the probe's site index cannot drift apart.
pub fn ensure_measure_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/measure_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            std::fs::write(
                project.join("Generated.fs"),
                measure_corpus::fixture_source(),
            )
            .expect("write measure Generated.fs");
            dotnet_build(&project, "dotnet build measure fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join(format!("{}.dll", measure_corpus::FIXTURE_ASM))
        })
        .as_path()
}

/// Build the **module-function measure** fixture (`tests/fixtures/measure_fn_env`,
/// `SemaMeasureFnFixture.dll`, with its project reference
/// `SemaMeasureFnBase.dll`) once per test binary and return both `.dll` paths,
/// the fixture first.
///
/// `Generated.fs` is emitted from [`measure_fn_corpus`]'s tables before the
/// build, so the fixture and the probe's row index cannot drift apart.
pub fn ensure_measure_fn_fixture_built() -> (&'static Path, &'static Path) {
    static BUILT: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    let (fixture, base) = BUILT.get_or_init(|| {
        let project =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/measure_fn_env");
        let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
        std::fs::write(
            project.join("Generated.fs"),
            measure_fn_corpus::fixture_source(),
        )
        .expect("write measure-fn Generated.fs");
        dotnet_build(&project, "dotnet build measure-fn fixture");
        let out = project.join("bin").join("Release").join("net10.0");
        (
            out.join(format!("{}.dll", measure_fn_corpus::FIXTURE_ASM)),
            out.join(format!("{}.dll", measure_fn_corpus::BASE_ASM)),
        )
    });
    (fixture.as_path(), base.as_path())
}

/// Build the **module-value** fixture (`tests/fixtures/module_value_env`,
/// generated from [`module_value_corpus`]) once per test binary and return its
/// `.dll` path.
pub fn ensure_module_value_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/module_value_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            std::fs::write(
                project.join("Generated.fs"),
                module_value_corpus::fixture_source(),
            )
            .expect("write module-value Generated.fs");
            dotnet_build(&project, "dotnet build module-value fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join(format!("{}.dll", module_value_corpus::FIXTURE_ASM))
        })
        .as_path()
}

/// Build the module-value fixture's **twin** (`tests/fixtures/module_value_twin_env`,
/// `SemaModuleValueTwin.dll`), which declares a type of the same name as one
/// the fixture declares, once per test binary and return its `.dll` path.
pub fn ensure_module_value_twin_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/module_value_twin_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(&project, "dotnet build module-value twin");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaModuleValueTwin.dll")
        })
        .as_path()
}

/// The references [`module_value_fixture_env`] adds to [`full_bcl_env`], for
/// the oracle.
pub fn module_value_fixture_refs() -> Vec<&'static Path> {
    vec![
        ensure_module_value_fixture_built(),
        ensure_module_value_twin_built(),
    ]
}

/// [`full_bcl_env`] plus the module-value fixture
/// ([`ensure_module_value_fixture_built`]) and its twin
/// ([`ensure_module_value_twin_built`]), the reference set its differential
/// hands the oracle too.
pub fn module_value_fixture_env() -> &'static borzoi_sema::AssemblyEnv {
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
        // `ResizeArray<'T>`'s target, `System.Collections.Generic.List<'T>`,
        // lives in System.Collections.
        let collections = std::fs::read(
            sysrt_path
                .parent()
                .expect("ref dir")
                .join("System.Collections.dll"),
        )
        .expect("read System.Collections.dll");
        let fixture = std::fs::read(ensure_module_value_fixture_built())
            .expect("read the module-value fixture");
        let twin =
            std::fs::read(ensure_module_value_twin_built()).expect("read the module-value twin");
        let views = vec![
            Ecma335Assembly::parse(&core).expect("parse FSharp.Core.dll"),
            Ecma335Assembly::parse(&sysrt).expect("parse System.Runtime.dll"),
            Ecma335Assembly::parse(&netstd).expect("parse netstandard.dll"),
            Ecma335Assembly::parse(&collections).expect("parse System.Collections.dll"),
            Ecma335Assembly::parse(&fixture).expect("parse SemaModuleValueFixture.dll"),
            Ecma335Assembly::parse(&twin).expect("parse SemaModuleValueTwin.dll"),
        ];
        borzoi_sema::AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
    })
}

/// Build the **constraint** fixture (`tests/fixtures/constrained_env`,
/// `SemaConstrainedFixture.dll`) once per test binary and return its `.dll`
/// path.
///
/// It exists because the annotation sweep is otherwise structurally blind to
/// F#-only typar constraints: they have no IL encoding, and every constrained
/// head reachable from the shipped FSharp.Core is *also* excluded from a bridge
/// by an unrelated guard, so a constraint-blind bridge passes the sweep green
/// while publishing a type FCS disagrees with. Its constraint types are
/// declared so no other guard can stand in — none renamed, none an
/// abbreviation, each paired with an unconstrained twin.
///
/// It carries the other dimensions that need an F#-authored reference rather
/// than a library head, on the same terms: `[<Obsolete(_, true)>]` and
/// `[<CompilerMessage(_, _, IsError = true)>]` heads, each with a
/// warning-level twin so a decline is attributable to the `IsError` flag, and
/// the same two attributes on **abbreviations** — the one carrier with no ECMA
/// row at all, whose attributes exist only in the F# pickle.
pub fn ensure_constrained_fixture_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/constrained_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            dotnet_build(&project, "dotnet build constrained fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaConstrainedFixture.dll")
        })
        .as_path()
}

/// Build the **companion-head** corpus (`tests/fixtures/companion_env`) once per
/// test binary and return its `.dll` path.
///
/// One assembly, because the question is what a *single* `(namespace, name)`
/// holds — a type, its companion module, or both — rather than how two
/// assemblies contest a name (that is [`ensure_tier_corpus_built`]'s). Its
/// `Generated.fs` is not checked in: it is emitted from
/// [`companion_corpus`]'s dimension enums here, under the shared [`BUILD_LOCK`]
/// like every other fixture.
pub fn ensure_companion_corpus_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/companion_env");
            let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
            std::fs::write(
                project.join("Generated.fs"),
                companion_corpus::fixture_source(&companion_corpus::corpus()),
            )
            .expect("write companion Generated.fs");
            dotnet_build(&project, "dotnet build companion fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join(format!("{}.dll", companion_corpus::FIXTURE_ASM))
        })
        .as_path()
}

/// Build the **tier-order** corpus (`tests/fixtures/tier_env` and
/// `tests/fixtures/tier_decoy_env`) once per test binary and return both
/// `.dll` paths, contributor first.
///
/// Two assemblies because the sweep varies which is referenced first: an
/// assembly's root-namespace contents and its `[<assembly: AutoOpen>]` targets
/// both enter the name environment when that assembly is imported, so the
/// manifest-surface/root contest is a question about reference order and
/// cannot be posed within one DLL. Neither `Generated.fs` is checked in — both
/// are emitted from `tier_corpus::Tier` here, under the shared [`BUILD_LOCK`]
/// like every other fixture, so the F# universe cannot drift from the Rust
/// matrix that probes it.
pub fn ensure_tier_corpus_built() -> (&'static Path, &'static Path) {
    static BUILT: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    let (contributor, decoy) = BUILT.get_or_init(|| {
        let plants = tier_corpus::corpus();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let contributor = root.join("tests/fixtures/tier_env");
        let decoy = root.join("tests/fixtures/tier_decoy_env");
        let _guard = BUILD_LOCK.lock().expect("BUILD_LOCK poisoned");
        std::fs::write(
            contributor.join("Generated.fs"),
            tier_corpus::contributor_source(&plants),
        )
        .expect("write tier contributor Generated.fs");
        std::fs::write(
            decoy.join("Generated.fs"),
            tier_corpus::decoy_source(&plants),
        )
        .expect("write tier decoy Generated.fs");
        dotnet_build(&contributor, "dotnet build tier contributor fixture");
        dotnet_build(&decoy, "dotnet build tier decoy fixture");
        let dll = |project: &Path, name: &str| {
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join(format!("{name}.dll"))
        };
        (
            dll(&contributor, tier_corpus::CONTRIBUTOR_ASM),
            dll(&decoy, tier_corpus::DECOY_ASM),
        )
    });
    (contributor.as_path(), decoy.as_path())
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
/// `BORZOI_FCS_DUMP` prebuilt-binary override is *not* honoured
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
/// [`full_bcl_env`] plus the constraint fixture
/// ([`ensure_constrained_fixture_built`]) — the reference set the annotation
/// sweep needs, and exactly what it hands the oracle, so neither side can see a
/// head the other cannot.
pub fn constrained_fixture_env() -> &'static borzoi_sema::AssemblyEnv {
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
        let fixture =
            std::fs::read(ensure_constrained_fixture_built()).expect("read the constraint fixture");
        let views = vec![
            Ecma335Assembly::parse(&core).expect("parse FSharp.Core.dll"),
            Ecma335Assembly::parse(&sysrt).expect("parse System.Runtime.dll"),
            Ecma335Assembly::parse(&netstd).expect("parse netstandard.dll"),
            Ecma335Assembly::parse(&fixture).expect("parse SemaConstrainedFixture.dll"),
        ];
        borzoi_sema::AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
    })
}

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

pub use borzoi_oracle_harness::fcs_position::LineIndex;

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
    /// What kind of symbol FCS bound — `"module"` / `"type"` / `"namespace"` for
    /// an entity, else `"member"`, `"unioncase"`, … . A type and its **companion
    /// module** share one `(namespace, name)` and render the same
    /// [`full_name`](Self::full_name), so this is the only field that witnesses
    /// which of them a use bound.
    pub kind: Option<String>,
    /// The entity's declared generic arity — the second half of that witness,
    /// since two types at one name differ only by it. `None` for a non-entity.
    pub generic_arity: Option<usize>,
    /// The entity a member/field/case is declared in, named **structurally**:
    /// one `(compiled name, generic parameter count)` per segment, outermost
    /// first. [`full_name`](Self::full_name) prints that entity through
    /// `NicePrint`, so it arrives decorated with type arguments and its identity
    /// cannot be read back off the string — and a compiled name may itself
    /// contain a dot, so a dotted form could not be split either. corpus-diff
    /// certifies against this instead.
    pub declaring_path: Option<Vec<(String, usize)>>,
    /// Whether the use is a constructor, which names its own type rather than a
    /// member of it.
    pub is_constructor: bool,
    /// For a value or member, the name FCS's name environment keys it by
    /// (`FSharpMemberOrFunctionOrValue.LogicalName`): an operator's compiled
    /// name, `op_Addition`, where [`name`](Self::name) is the rendering `(+)`.
    pub logical_name: Option<String>,
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
    #[serde(rename = "LogicalName", default)]
    logical_name: Option<String>,
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
    #[serde(rename = "SymbolKind", default)]
    kind: Option<String>,
    #[serde(rename = "GenericArity", default)]
    generic_arity: Option<usize>,
    #[serde(rename = "DeclaringPath", default)]
    declaring_path: Option<Vec<RawDeclaringSegment>>,
    #[serde(rename = "DeclaringNamespace", default)]
    declaring_namespace: Option<String>,
    #[serde(rename = "IsConstructor", default)]
    is_constructor: bool,
}

#[derive(Deserialize)]
struct RawDeclaringSegment {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Arity")]
    arity: usize,
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
                kind: u.kind,
                generic_arity: u.generic_arity,
                declaring_path: u.declaring_path.map(|path| {
                    path.into_iter()
                        .map(|segment| (segment.name, segment.arity))
                        .collect()
                }),
                is_constructor: u.is_constructor,
                logical_name: u.logical_name,
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
    /// FCS's symbol kind (`unioncase`, `member`, `type`, …), or `None`.
    pub kind: Option<String>,
    /// The declaring entity's path, outermost first, as `(compiled name,
    /// generic arity)` — the structural identity a rendered full name cannot
    /// carry (`FSharpOption`1` for `Option<_>.Some`). `None` when FCS reported
    /// no declaring entity.
    pub declaring_path: Option<Vec<(String, usize)>>,
    /// The namespace the declaring path's outermost entity sits in, `None` for
    /// the root namespace or when FCS reported no declaring entity.
    pub declaring_namespace: Option<String>,
    /// Whether the use is a constructor call (FCS names the constructed type).
    pub is_constructor: bool,
}

/// All symbol uses FCS reported for one project file.
#[derive(Debug, Clone)]
pub struct FileUses {
    /// The project file these uses belong to (matched by file name to a
    /// supplied source).
    pub path: PathBuf,
    pub uses: Vec<NormalisedProjectUse>,
    /// The message of every `Severity=Error` diagnostic FCS reported for the
    /// file — empty for a file that checked cleanly.
    pub errors: Vec<String>,
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
    #[serde(rename = "Diagnostics", default)]
    diagnostics: Vec<RawProjectDiagnostic>,
}

#[derive(Deserialize)]
struct RawProjectDiagnostic {
    #[serde(rename = "Severity")]
    severity: String,
    #[serde(rename = "Message")]
    message: String,
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
                        kind: u.kind,
                        declaring_path: u.declaring_path.map(|path| {
                            path.into_iter().map(|seg| (seg.name, seg.arity)).collect()
                        }),
                        declaring_namespace: u.declaring_namespace,
                        is_constructor: u.is_constructor,
                    }
                })
                .collect();
            FileUses {
                path: path.to_path_buf(),
                uses,
                errors: f
                    .diagnostics
                    .into_iter()
                    .filter(|d| d.severity == "Error")
                    .map(|d| d.message)
                    .collect(),
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
    /// Every `Severity=Error` diagnostic of the check. FCS's typed tree omits an
    /// expression it could not check, so a consumer asserting "every type we
    /// produced, FCS confirms" can only read a *missing* node as a disagreement
    /// on a line that checked cleanly.
    ///
    /// `Option`, not a defaulted `Vec`: absent and empty must stay
    /// distinguishable. A `BORZOI_FCS_DUMP` override pointing at a binary that
    /// predates the field would otherwise report *no errors* for every check, and
    /// a consumer whose soundness argument rests on "the line checked cleanly"
    /// would silently lose it. [`parse_fcs_types_with_errors`] refuses absence;
    /// [`parse_fcs_types`], which makes no such claim, ignores it.
    #[serde(rename = "Errors")]
    errors: Option<Vec<RawError>>,
    /// The declarations whose typed tree fcs-dump could not convert, and so
    /// reported nothing for. `Option` for the reason [`Self::errors`] is: a
    /// binary predating the field must not read as "every declaration
    /// converted".
    #[serde(rename = "Unconverted")]
    unconverted: Option<Vec<RawUnconverted>>,
}

/// One entry of a per-declaration dump's `Unconverted` list (fcs-dump's
/// `UnconvertedDeclaration`).
#[derive(Deserialize)]
struct RawUnconverted {
    #[serde(rename = "Range")]
    range: FcsRange,
    #[serde(rename = "InFile")]
    in_file: bool,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Message")]
    message: String,
}

/// A declaration whose typed tree FCS could not convert to its public
/// expression view, so a per-declaration dump (`types`, `binder-types`)
/// reports **no** node or binder inside it — not even the ones FCS did type.
///
/// The conversion re-solves each call's witnesses, and a call whose type
/// arguments violate a constraint (`g = g` with `g : int -> int`) throws there;
/// so does a pattern match FCS recovered from. An absent node inside such a
/// declaration is therefore no evidence of anything: a consumer must treat the
/// whole declaration as ungraded, never as agreement and never as a
/// disagreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FcsUnconverted {
    /// FCS's half-open byte range for the declaration: the binder's name through
    /// its body's range. Neither end is the declaration's: what precedes the
    /// name (`this` in `member this.M`) lies outside it, and a body that opens
    /// with a local `let` reports only that first binding. A consumer widens it
    /// to the source declaration that contains it.
    At {
        start: usize,
        end: usize,
        name: String,
        message: String,
    },
    /// fcs-dump could not place the declaration in this file, so nothing in the
    /// file can be shown to lie outside it.
    Unlocated { name: String, message: String },
}

/// A per-declaration dump read in full: the type map, every error FCS
/// reported, and the declarations it could not convert. The currency for a
/// consumer that must read a *missing* node as evidence — see
/// [`FcsUnconverted`] for why the errors alone do not license that.
#[derive(Debug, Clone)]
pub struct FcsTypedDump {
    pub types: FcsTypeMap,
    pub errors: Vec<FcsCheckError>,
    pub unconverted: Vec<FcsUnconverted>,
    /// For a `binder-types` dump, the constraint kinds on each binder's type
    /// parameters, by binder range (see [`GradedDumps::hides_constraint`]);
    /// empty for a `types` dump.
    pub typar_constraints: HashMap<(usize, usize), Vec<String>>,
}

fn normalise_unconverted(raw: Vec<RawUnconverted>, idx: &LineIndex) -> Vec<FcsUnconverted> {
    raw.into_iter()
        .map(|u| {
            if u.in_file {
                FcsUnconverted::At {
                    start: idx.offset(u.range.start.line, u.range.start.col),
                    end: idx.offset(u.range.end.line, u.range.end.col),
                    name: u.name,
                    message: u.message,
                }
            } else {
                FcsUnconverted::Unlocated {
                    name: u.name,
                    message: u.message,
                }
            }
        })
        .collect()
}

/// The source region an unconverted FCS declaration spanning `[start, end)`
/// stands for: everything that could belong to it.
///
/// FCS's range is a binding's name through its body's range, and both fall
/// short of the declaration: the name misses what sits before it (`this` in
/// `member this.M`, a pattern `let`'s other binders), and a body that opens
/// with a local `let` reports only that first binding. So the region widens to
/// the outermost `let` declaration around it; failing that, the member
/// definition; failing that, the type definition (a class's `let`s and `do`s
/// convert as its constructor); failing that, the module-level expression.
/// Widening only loses grading; it can never let a commit in the declaration be
/// graded.
pub fn unconverted_region(
    file: Option<&borzoi_cst::syntax::ImplFile>,
    start: usize,
    end: usize,
) -> (usize, usize) {
    use borzoi_cst::syntax::{AstNode, SyntaxKind};
    let Some(file) = file else {
        return (start, end);
    };
    let range = rowan::TextRange::new((start as u32).into(), (end as u32).into());
    let ancestors: Vec<_> = match file.syntax().covering_element(range) {
        rowan::NodeOrToken::Node(n) => n.ancestors().collect(),
        rowan::NodeOrToken::Token(t) => t.parent_ancestors().collect(),
    };
    let region = ancestors
        .iter()
        .rfind(|n| n.kind() == SyntaxKind::LET_DECL)
        .or_else(|| {
            ancestors
                .iter()
                .find(|n| n.kind() == SyntaxKind::MEMBER_DEFN)
        })
        .or_else(|| ancestors.iter().find(|n| n.kind() == SyntaxKind::TYPE_DEFN))
        .or_else(|| ancestors.iter().find(|n| n.kind() == SyntaxKind::EXPR_DECL));
    match region {
        Some(n) => {
            let r = n.text_range();
            (
                (u32::from(r.start()) as usize).min(start),
                (u32::from(r.end()) as usize).max(end),
            )
        }
        None => (start, end),
    }
}

/// The source regions of `unconverted`'s declarations in `file`
/// ([`unconverted_region`]): where a grader must record a commit as ungraded.
/// `None` when one of them could not be placed in the file, so that no commit
/// in it can be shown to lie outside an unconverted declaration.
pub fn ungraded_regions(
    file: Option<&borzoi_cst::syntax::ImplFile>,
    unconverted: &[FcsUnconverted],
) -> Option<Vec<(usize, usize)>> {
    unconverted
        .iter()
        .map(|u| match u {
            FcsUnconverted::At { start, end, .. } => Some(unconverted_region(file, *start, *end)),
            FcsUnconverted::Unlocated { .. } => None,
        })
        .collect()
}

/// Both per-declaration dumps of one file, read for a grader that takes a
/// missing node as evidence: FCS's expression and binder types, its errors,
/// and the regions of the declarations it could not convert
/// ([`ungraded_regions`]), inside which nothing may be graded either way.
#[derive(Debug, Clone)]
pub struct GradedDumps {
    pub types: FcsTypeMap,
    pub binders: FcsTypeMap,
    pub errors: Vec<FcsCheckError>,
    pub ungraded: Vec<(usize, usize)>,
    /// Each binder's type-parameter constraint kinds; see
    /// [`Self::hides_constraint`].
    pub binder_constraints: HashMap<(usize, usize), Vec<String>>,
}

impl GradedDumps {
    /// Read the `types` and `binder-types` payloads of `source`, parsed as
    /// `file`. `None` when an unconverted declaration could not be placed in
    /// the file, so nothing in it can be graded.
    pub fn read(
        types_json: &str,
        binders_json: &str,
        source: &str,
        file: Option<&borzoi_cst::syntax::ImplFile>,
    ) -> Option<GradedDumps> {
        let types = parse_fcs_types_dump(types_json, source);
        let binders = parse_fcs_binder_types_dump(binders_json, source);
        let mut unconverted = types.unconverted;
        for u in binders.unconverted {
            if !unconverted.contains(&u) {
                unconverted.push(u);
            }
        }
        Some(GradedDumps {
            ungraded: ungraded_regions(file, &unconverted)?,
            types: types.types,
            binders: binders.types,
            errors: types.errors,
            binder_constraints: binders.typar_constraints,
        })
    }

    /// Whether our type `ours` for the binder at `range`, which renders as
    /// FCS's does, says less than FCS's: it mentions a type parameter, and FCS
    /// constrains one. The canonical rendering omits constraints, so
    /// `'a -> 'a -> System.Boolean` agrees as a string with FCS's
    /// `'a -> 'a -> bool when 'a : equality`, and only this tells a hover that
    /// drops the constraint from one that has none to drop.
    pub fn hides_constraint(&self, range: (usize, usize), ours: &str) -> bool {
        ours.contains('\'')
            && self
                .binder_constraints
                .get(&range)
                .is_some_and(|kinds| !kinds.is_empty())
    }

    /// Whether `[start, end)` overlaps a declaration FCS could not convert.
    pub fn is_ungraded(&self, (start, end): (usize, usize)) -> bool {
        self.ungraded.iter().any(|&(u0, u1)| start < u1 && u0 < end)
    }
}

/// Refuse a dump with unconverted declarations on behalf of a caller that reads
/// it as a complete account of the file. Such a caller would take every node
/// missing from those declarations as FCS's answer; it must use
/// [`parse_fcs_types_dump`] / [`parse_fcs_binder_types_dump`] and account for
/// them instead.
fn refuse_unconverted(op: &str, unconverted: &Option<Vec<RawUnconverted>>) {
    if let Some(u) = unconverted
        && !u.is_empty()
    {
        let names: Vec<String> = u
            .iter()
            .map(|u| format!("`{}` ({})", u.name, u.message.lines().next().unwrap_or("")))
            .collect();
        panic!(
            "fcs-dump {op} could not convert {} declaration(s): {}. This caller reads \
             the dump as complete; use the `*_dump` parser, which reports them.",
            u.len(),
            names.join("; ")
        );
    }
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
pub fn parse_fcs_types(json: &str, source: &str) -> FcsTypeMap {
    let (types, _, unconverted) = parse_types_dump(json, source);
    refuse_unconverted("types", &unconverted);
    types
}

/// [`parse_fcs_types`] plus the errors FCS reported. A consumer that reads a
/// *missing* node as a disagreement needs them: the typed tree omits the whole
/// enclosing expression on a line that failed to check, so the absence is
/// evidence only where the line checked cleanly.
pub fn parse_fcs_types_with_errors(json: &str, source: &str) -> (FcsTypeMap, Vec<FcsCheckError>) {
    let (types, errors, unconverted) = parse_types_dump(json, source);
    refuse_unconverted("types", &unconverted);
    let errors = errors.expect(
        "the `types` payload carries no `Errors` field — the oracle predates it \
         (check BORZOI_FCS_DUMP), and a caller reading a missing node as a \
         disagreement cannot tell a clean check from an unreported one",
    );
    (types, errors)
}

/// The `types` map: an expression's half-open byte range to FCS's canonical type.
pub type FcsTypeMap = std::collections::HashMap<(usize, usize), String>;

/// The `types` dump in full, unconverted declarations included. Refuses a
/// payload missing `Errors` or `Unconverted`: a consumer of this form reads
/// missing nodes as evidence, and an oracle predating either field cannot say
/// where that is sound.
pub fn parse_fcs_types_dump(json: &str, source: &str) -> FcsTypedDump {
    let (types, errors, unconverted) = parse_types_dump(json, source);
    full_dump("types", types, errors, unconverted, source)
}

/// Each binder's type-parameter constraint kinds, by binder range. Refuses a
/// payload whose binders do not carry the field: a reader of it is asking
/// whether a generic type hides a constraint, which an oracle predating the
/// field cannot say.
fn binder_typar_constraints(json: &str, source: &str) -> HashMap<(usize, usize), Vec<String>> {
    let dump: BinderTypesDump =
        serde_json::from_str(json).expect("fcs-dump binder-types JSON shape");
    let idx = LineIndex::new(source);
    dump.binders
        .into_iter()
        .map(|b| {
            let start = idx.offset(b.range.start.line, b.range.start.col);
            let end = idx.offset(b.range.end.line, b.range.end.col);
            let kinds = b.typar_constraints.expect(
                "the `binder-types` payload carries no `TyparConstraints` — the oracle \
                 predates it (check BORZOI_FCS_DUMP)",
            );
            ((start, end), kinds)
        })
        .collect()
}

/// The `binder-types` dump in full; see [`parse_fcs_types_dump`].
pub fn parse_fcs_binder_types_dump(json: &str, source: &str) -> FcsTypedDump {
    let (types, errors, unconverted) = parse_binder_types_dump(json, source);
    FcsTypedDump {
        typar_constraints: binder_typar_constraints(json, source),
        ..full_dump("binder-types", types, errors, unconverted, source)
    }
}

fn full_dump(
    op: &str,
    types: FcsTypeMap,
    errors: Option<Vec<FcsCheckError>>,
    unconverted: Option<Vec<RawUnconverted>>,
    source: &str,
) -> FcsTypedDump {
    let errors = errors.unwrap_or_else(|| {
        panic!("the `{op}` payload carries no `Errors` field (check BORZOI_FCS_DUMP)")
    });
    let unconverted = unconverted.unwrap_or_else(|| {
        panic!(
            "the `{op}` payload carries no `Unconverted` field — the oracle predates \
             it (check BORZOI_FCS_DUMP), and a declaration it could not convert would \
             read as one with no nodes"
        )
    });
    FcsTypedDump {
        types,
        errors,
        unconverted: normalise_unconverted(unconverted, &LineIndex::new(source)),
        typar_constraints: HashMap::new(),
    }
}

/// A dump's three parts, each `None` when the oracle did not report the field.
type RawTypedParts = (
    FcsTypeMap,
    Option<Vec<FcsCheckError>>,
    Option<Vec<RawUnconverted>>,
);

/// The shared body: the type map, the errors, and the unconverted declarations,
/// each *if the oracle reported the field at all*.
fn parse_types_dump(json: &str, source: &str) -> RawTypedParts {
    let dump: TypesDump = serde_json::from_str(json).expect("fcs-dump types JSON shape");
    let idx = LineIndex::new(source);
    let types = dump
        .exprs
        .into_iter()
        .map(|e| {
            let start = idx.offset(e.range.start.line, e.range.start.col);
            let end = idx.offset(e.range.end.line, e.range.end.col);
            ((start, end), e.type_canon)
        })
        .collect();
    let errors = dump.errors.map(|errors| {
        errors
            .into_iter()
            .map(|e| FcsCheckError {
                line: e.line,
                code: e.code,
                message: e.message,
            })
            .collect()
    });
    (types, errors, dump.unconverted)
}

#[derive(Deserialize)]
struct BinderTypesDump {
    #[serde(rename = "Binders")]
    binders: Vec<RawBinder>,
    /// Every `Severity=Error` diagnostic of the check.
    ///
    /// `Option`, not a defaulted `Vec`, for the same reason [`TypesDump`]'s is:
    /// absent and empty must stay distinguishable, or a `BORZOI_FCS_DUMP` binary
    /// predating the field reports *no errors* for every check and a consumer
    /// whose soundness argument rests on "FCS rejected this line" silently loses
    /// it. [`parse_fcs_binder_types_with_errors`] refuses absence;
    /// [`parse_fcs_binder_types`], which makes no such claim, ignores it.
    #[serde(rename = "Errors")]
    errors: Option<Vec<RawError>>,
    /// As [`TypesDump::unconverted`].
    #[serde(rename = "Unconverted")]
    unconverted: Option<Vec<RawUnconverted>>,
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
    /// The kinds of constraint the binder type's type parameters carry
    /// (`equality`, `comparison`, `other`, `unknown`), which `TypeCanon` cannot
    /// show. `None` from an oracle that predates the field.
    #[serde(rename = "TyparConstraints")]
    typar_constraints: Option<Vec<String>>,
}

/// Parse the `binder-types` subcommand's JSON into a map from a binder's
/// *declaration* half-open byte range `(start, end)` to FCS's canonical inferred
/// type there. `source` must be the exact text of the checked file (offsets index
/// into it). The range is a binder's `DeclarationLocation`, matching the `sema`
/// resolver's [`borzoi_sema::Def`]`::range`, so our `def_type` map keys
/// (a `DefId` → its `Def::range`) line up with this map's keys.
pub fn parse_fcs_binder_types(json: &str, source: &str) -> FcsTypeMap {
    let (types, _, unconverted) = parse_binder_types_dump(json, source);
    refuse_unconverted("binder-types", &unconverted);
    types
}

/// [`parse_fcs_binder_types`] plus the errors FCS reported. A consumer that reads
/// a binder's *reported* type as FCS's answer needs them: FCS error-recovers an
/// annotation it rejects to `System.Object` and still emits a binder record, so
/// the record alone cannot distinguish "FCS says this type" from "FCS says this
/// annotation is not writable".
pub fn parse_fcs_binder_types_with_errors(
    json: &str,
    source: &str,
) -> (FcsTypeMap, Vec<FcsCheckError>) {
    let (types, errors, unconverted) = parse_binder_types_dump(json, source);
    refuse_unconverted("binder-types", &unconverted);
    let errors = errors.expect(
        "the `binder-types` payload carries no `Errors` field — the oracle predates it \
         (check BORZOI_FCS_DUMP), and a caller that must not commit on an annotation \
         FCS rejected cannot tell a clean check from an unreported one",
    );
    (types, errors)
}

/// The shared body: the binder-type map, the errors, and the unconverted
/// declarations, each *if the oracle reported the field at all*.
fn parse_binder_types_dump(json: &str, source: &str) -> RawTypedParts {
    let dump: BinderTypesDump =
        serde_json::from_str(json).expect("fcs-dump binder-types JSON shape");
    let idx = LineIndex::new(source);
    let types = dump
        .binders
        .into_iter()
        .map(|b| {
            let start = idx.offset(b.range.start.line, b.range.start.col);
            let end = idx.offset(b.range.end.line, b.range.end.col);
            ((start, end), b.type_canon)
        })
        .collect();
    let errors = dump.errors.map(|errors| {
        errors
            .into_iter()
            .map(|e| FcsCheckError {
                line: e.line,
                code: e.code,
                message: e.message,
            })
            .collect()
    });
    (types, errors, dump.unconverted)
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
