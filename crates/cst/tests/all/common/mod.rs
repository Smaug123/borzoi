//! Shared helpers for the CST crate's integration tests.
//!
//! The crate has one test binary (`tests/all/main.rs`); this is a `mod`-declared
//! submodule of it, so every case group (`tests/all/corpus.rs`,
//! `tests/all/lexer_diff.rs`, `tests/all/lexfilter_diff/`, …) reaches it as
//! `crate::common`. (A `tests/foo.rs` *outside* `all/` would be compiled as a
//! second test binary, which is the trap `all_case_groups_are_declared` guards.)
//!
//! The *process plumbing* for driving `fcs-dump` lives in the shared
//! `borzoi-oracle-harness` dev-crate ([`BoundedCommand`], [`BatchChild`]),
//! over the process-global spawn lock in `borzoi-spawn`: this module only
//! knows the subcommands and their JSON. The lock has to be the *one* lock in
//! the process — a per-harness copy excludes nothing against the other spawns a
//! test binary makes — and the bounded-wait logic is worth writing once rather
//! than once per oracle.

#![allow(dead_code)] // each importer uses a different subset.

pub mod normalised_ast;
mod range_audit;

#[allow(unused_imports)]
pub use range_audit::{assert_ast_ranges_match, assert_sig_ast_ranges_match, ast_ranges_match};

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::lexer::{Token, lex};
use borzoi_cst::lexfilter::{FilteredToken, Virtual, filter};
use borzoi_cst::parser::{
    FileKind, ParseOptions, parse, parse_sig, parse_with_options, parse_with_symbols,
};
use borzoi_oracle_harness::{BatchChild, BoundedCommand};

/// The sweeps here silence their *expected* panics; the implementation is shared
/// with sema's and the LSP's sweeps, because a hook swap races once a crate's
/// cases share one test binary. See that module for the whole story.
pub use borzoi_oracle_harness::panic_silence::catch_unwind_silent;
use serde::Deserialize;
use tempfile::NamedTempFile;

// ============================================================================
// Workspace pathing
// ============================================================================

/// The workspace root, two `..` jumps above the CST crate's `CARGO_MANIFEST_DIR`.
/// `tools/fcs-dump` lives there.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root parent")
        .to_path_buf()
}

// ============================================================================
// fcs-dump invocation
// ============================================================================

/// Run `fcs-dump <subcommand> <source>` and return its stdout as a UTF-8 string.
///
/// Honours `BORZOI_FCS_DUMP` (path to a pre-built self-contained binary)
/// when set; otherwise builds `tools/fcs-dump` via [`ensure_fcs_dump_built`]
/// (sentinel-gated, so the build runs at most once per source change across the
/// whole `cargo test`) and execs the resulting apphost on every call.
///
/// Building then exec-ing the binary directly — rather than `dotnet run` per
/// call — avoids a race: `dotnet run` does an MSBuild incremental-build check on
/// every invocation, mutating `obj/Release/net10.0/*.cache`, so N parallel test
/// threads racing those shared files produce non-deterministic build failures.
/// The hot path here has no shared mutable build state.
///
/// Most callers now go through the batched [`fcs_ast_batch`] &co. instead; this
/// one-shot form remains for [`invoke_fcs_dump_with_defines`] (batch mode can't
/// carry `#if` symbols).
pub fn invoke_fcs_dump(subcommand: &str, source: &Path) -> String {
    invoke_fcs_dump_with_defines(subcommand, source, &[])
}

/// Like [`invoke_fcs_dump`], but passes `defines` as trailing
/// conditional-compilation symbols (`fcs-dump ast <file> SYM…`), so `#if SYM`
/// selects the active branch the caller intends. Only the `ast` subcommand
/// honours them.
/// Build the base `fcs-dump <subcommand>` command, without source arguments,
/// so callers that need to drive stdin/stdout themselves (e.g. the
/// `ast-batch` corpus sweep) can configure the pipes. Honours the same
/// `BORZOI_FCS_DUMP` / build-once strategy as [`invoke_fcs_dump`].
pub fn fcs_dump_command(subcommand: &str) -> Command {
    let mut c = Command::new(fcs_dump_bin());
    c.arg(subcommand);
    c
}

/// The `fcs-dump` executable: the prebuilt one named by `BORZOI_FCS_DUMP`,
/// else the apphost [`ensure_fcs_dump_built`] produces.
fn fcs_dump_bin() -> OsString {
    std::env::var_os("BORZOI_FCS_DUMP")
        .unwrap_or_else(|| ensure_fcs_dump_built().as_os_str().to_os_string())
}

pub fn invoke_fcs_dump_with_defines(subcommand: &str, source: &Path, defines: &[&str]) -> String {
    let mut cmd = fcs_dump_command(subcommand);
    cmd.arg(source);
    for d in defines {
        cmd.arg(d);
    }

    // Bounded, like the batch children: this is the path `#if`-carrying tests take
    // (batch mode can't pass defines), and it is where a wedged fcs-dump was
    // observed hanging `parser_diff_ifdef` indefinitely.
    let out = BoundedCommand::new(cmd).run_ok(format_args!("fcs-dump {subcommand}"));
    String::from_utf8(out.stdout).expect("fcs-dump stdout is UTF-8")
}

// ============================================================================
// Batched fcs-dump invocation
// ============================================================================
//
// One-shot `invoke_fcs_dump` spawns a fresh `dotnet fcs-dump` process per call,
// paying the ~150-300 ms .NET + FCS startup *every* time — and the per-case
// `parser_diff_*` / `lexfilter_diff` / `lexer_diff` tests make ~1800 such calls
// across the crate. The `*-batch` subcommands (already used by the corpus
// sweep, `parser_corpus_diff.rs`) instead read source paths from stdin and emit
// one JSON line per path, so the startup is paid once per *pool slot* rather
// than once per case. `BatchChild` drives such a child in lock-step: write a
// path, read exactly the one response line it produces — hence [`BatchPool`],
// which holds several so that libtest's threads don't queue behind one.

/// A pool of interchangeable `fcs-dump <subcommand>` children.
///
/// A single child *cannot* serve concurrent callers: requests and responses are
/// matched positionally, so overlapping round-trips would cross their answers.
/// One child therefore serialises every FCS round-trip in the process — which,
/// now that the whole crate's cases live in one test binary (`tests/all/`),
/// means serialising the entire suite behind one .NET process. But the children
/// are stateless with respect to *each other*: `n` of them serve `n` of libtest's
/// threads concurrently, each round-trip holding its own child exclusively.
///
/// Slots spawn lazily and [`request`](Self::request) prefers the lowest idle
/// slot, so the pool converges on roughly as many children as there is real
/// concurrency, rather than eagerly paying `n` .NET startups.
struct BatchPool {
    subcommand: &'static str,
    slots: Vec<Mutex<Option<BatchChild>>>,
}

/// Cap on `fcs-dump` children resident in this process *across all pools*.
///
/// Each child is a resident FCS/.NET process holding hundreds of MB, and a
/// child, once spawned, lives until the test binary exits. The budget is
/// process-wide rather than per-pool because the pools do not take turns: the
/// parser diffs drive `ast`, the lex-filter diffs drive `tokens-filtered` and
/// the lexer diffs `tokens-raw`, and libtest interleaves all three — so a
/// per-pool cap of *n* would leave `FCS_POOLS * n` children resident.
///
/// Kept deliberately modest, and overridable with `BORZOI_FCS_CHILDREN`.
/// The gain is steeply diminishing — the children are CPU-hungry, so a handful
/// already saturates the cores — while the cost is not: a dev box often has
/// sibling worktrees running their own suites, and a pool sized to *this*
/// process's core count is antisocial about a machine it doesn't own.
///
/// The budget is divided evenly among the [`FCS_POOLS`] pools, so it is clamped
/// *up* to that many: the pools serve different `fcs-dump` subcommands and are
/// not interchangeable, so each needs at least one child to make progress at all,
/// and a budget of 1 is simply not satisfiable. Clamping is the honest response —
/// the alternative, rounding each pool's share up to 1, would quietly hold
/// `FCS_POOLS` children while claiming to honour a budget of one.
fn requested_fcs_children() -> usize {
    std::env::var("BORZOI_FCS_CHILDREN")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(6)
}

/// The budget actually in force: `requested`, raised to one child per pool.
pub fn effective_fcs_children(requested: usize) -> usize {
    requested.max(FCS_POOLS)
}

/// How many children one pool may hold, given a `requested` process-wide budget.
///
/// Pure and public so `oracle_pool.rs` can search the whole input range for a
/// setting that overruns the budget — integer division makes that easy to get
/// subtly wrong, and the failure (more resident .NET processes than asked for)
/// is invisible until a runner is OOM-killed.
pub fn fcs_slots_per_pool(requested: usize) -> usize {
    effective_fcs_children(requested) / FCS_POOLS
}

/// The number of [`BatchPool`]s in this process; the child budget is split evenly
/// between them. Asserted in [`BatchPool::new`], so adding a pool without updating
/// this fails loudly rather than quietly overrunning the budget.
const FCS_POOLS: usize = 3;

impl BatchPool {
    /// Size the pool to libtest's own default thread count, capped at this pool's
    /// share of the process-wide [`max_fcs_children`] budget.
    fn new(subcommand: &'static str) -> Self {
        static LIVE_POOLS: AtomicUsize = AtomicUsize::new(0);
        let nth = LIVE_POOLS.fetch_add(1, Ordering::Relaxed) + 1;
        assert!(
            nth <= FCS_POOLS,
            "{nth} fcs-dump pools but FCS_POOLS is {FCS_POOLS}: raise it (and the \
             child budget), or the budget silently overruns"
        );

        let share = fcs_slots_per_pool(requested_fcs_children());
        let n = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            subcommand,
            slots: (0..n.min(share)).map(|_| Mutex::new(None)).collect(),
        }
    }

    /// Ask an idle child about `source`, holding it for the whole round-trip.
    /// Falls back to *blocking* on a round-robin slot when every child is busy,
    /// so a saturated pool queues rather than spawning without bound.
    fn request(&self, source: &Path) -> String {
        for slot in &self.slots {
            if let Ok(mut guard) = slot.try_lock() {
                return self.round_trip(&mut guard, source);
            }
        }
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let i = NEXT.fetch_add(1, Ordering::Relaxed) % self.slots.len();
        let mut guard = self.slots[i]
            .lock()
            .expect("fcs-dump batch mutex poisoned (a previous request failed)");
        self.round_trip(&mut guard, source)
    }

    /// Spawn this slot's child on first use — bounded and self-healing; see the
    /// harness crate for the wedge/crash recovery it provides — then drive one
    /// lock-step request through it. (The batch payload carries an extra `Path`
    /// field over the one-shot dump; the JSON consumers read by field name and
    /// ignore it.)
    fn round_trip(&self, slot: &mut Option<BatchChild>, source: &Path) -> String {
        slot.get_or_insert_with(|| BatchChild::spawn(fcs_dump_bin(), &[self.subcommand]))
            .request(&source.display().to_string())
    }
}

/// Run `source` through the shared `ast-batch` pool and return its JSONL line.
/// Drop-in for one-shot `invoke_fcs_dump("ast", source)`, but amortising the
/// process startup across the whole test binary. Batch mode keeps
/// `ConditionalDefines` empty, so callers that need `#if` symbols must stay on
/// [`invoke_fcs_dump_with_defines`].
pub fn fcs_ast_batch(source: &Path) -> String {
    static P: OnceLock<BatchPool> = OnceLock::new();
    P.get_or_init(|| BatchPool::new("ast-batch"))
        .request(source)
}

/// Run `source` through the shared `tokens-filtered-batch` pool. Drop-in for
/// one-shot `invoke_fcs_dump("tokens-filtered", source)`.
pub fn fcs_tokens_filtered_batch(source: &Path) -> String {
    static P: OnceLock<BatchPool> = OnceLock::new();
    P.get_or_init(|| BatchPool::new("tokens-filtered-batch"))
        .request(source)
}

/// Run `source` through the shared `tokens-raw-batch` pool. Drop-in for
/// one-shot `invoke_fcs_dump("tokens-raw", source)`.
pub fn fcs_tokens_raw_batch(source: &Path) -> String {
    static P: OnceLock<BatchPool> = OnceLock::new();
    P.get_or_init(|| BatchPool::new("tokens-raw-batch"))
        .request(source)
}

/// Build `tools/fcs-dump` (when no prebuilt `BORZOI_FCS_DUMP` is set) and
/// return the path to the produced **apphost** — the directly-executable native
/// launcher sitting next to `fcs-dump.dll` — so callers exec it without the
/// `dotnet` front-end.
///
/// A per-process `OnceLock` alone would re-run `dotnet build` once per test
/// binary (~2 s apiece). This crate is now a single binary (`tests/all/`), but
/// the *other* crates' harnesses build the same oracle, and a plain `cargo test`
/// runs their binaries back to back — so the gate still earns its keep. To avoid
/// the rebuild while staying correct, it is a **single** marker file
/// (`bin/.fcs-dump-built`) whose *contents* are the source hash
/// ([`fcs_dump_source_fingerprint`]) of the apphost currently on disk. The
/// build runs only when that recorded hash doesn't match the current sources
/// (or the apphost is missing); otherwise the binary skips straight to exec.
///
/// The marker records the *built output's* hash rather than merely "some build
/// with this hash once happened" — because the apphost is one mutable file, so
/// a per-hash marker would be unsound: after building hash A then hash B (which
/// overwrites the apphost), reverting/branch-switching back to A would find a
/// stale `built-A` marker and skip the build, running the B binary as the
/// oracle for A's sources. One content-bearing marker makes a revert mismatch
/// (recorded B ≠ wanted A) and correctly rebuild. The oracle is thus always
/// fresh, unlike pinning a binary path in the devshell.
///
/// Cargo runs test binaries serially, so the first-builds / rest-skip handoff
/// needs no inter-process lock; the marker write is atomic (temp + rename) so a
/// partially-written hash can never read as a match.
fn ensure_fcs_dump_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project = project_dir();
            let bin = project.join("bin");
            let apphost = bin.join("Release").join("net10.0").join("fcs-dump");
            let marker = bin.join(".fcs-dump-built");
            let want = format!("{:016x}", fcs_dump_source_fingerprint(&project));

            let fresh = apphost.exists()
                && std::fs::read_to_string(&marker)
                    .map(|recorded| recorded.trim() == want)
                    .unwrap_or(false);
            if !fresh {
                let mut cmd = Command::new("dotnet");
                cmd.args(["build", "-c", "Release", "--nologo"])
                    .arg(&project);
                // Generous, but bounded: a cold build (restore + compile against
                // FCS) is legitimately minutes, while a *stalled* one — say,
                // blocked on a NuGet lock held by a concurrent run in a sibling
                // worktree — would otherwise hang the suite forever.
                BoundedCommand::new(cmd)
                    .timeout(Duration::from_secs(1800))
                    .run_ok("dotnet build fcs-dump");
                assert!(
                    apphost.exists(),
                    "dotnet build fcs-dump produced no apphost at {apphost:?}"
                );
                // Write the marker only after a successful build, so it always
                // names the hash of the apphost actually on disk.
                write_marker_atomically(&marker, &want);
            }
            apphost
        })
        .as_path()
}

/// Hash the inputs whose change should force an `fcs-dump` rebuild: the tool's
/// own sources (`*.fs` / `*.fsproj`) and the flake lock (which pins the SDK and
/// thus the FCS package set). A plain non-cryptographic content hash — just a
/// freshness key for the build marker — so editing the oracle reliably
/// invalidates a prior build. Missing files hash as empty, keeping the
/// fingerprint defined when run outside a Nix checkout.
fn fcs_dump_source_fingerprint(project: &Path) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut sources: Vec<PathBuf> = std::fs::read_dir(project)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("fs" | "fsproj")
            )
        })
        .collect();
    sources.sort();
    sources.push(workspace_root().join("flake.lock"));

    let mut h = DefaultHasher::new();
    for p in &sources {
        // File name first so a rename can't alias two contents to one hash.
        p.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .hash(&mut h);
        std::fs::read(p).unwrap_or_default().hash(&mut h);
    }
    h.finish()
}

/// Write `contents` to `marker` atomically: write a pid-tagged sibling temp
/// file and rename it into place, so a reader never observes a half-written
/// hash. Best-effort — any failure just means the next test binary rebuilds
/// (still correct, only slower).
fn write_marker_atomically(marker: &Path, contents: &str) {
    let Some(dir) = marker.parent() else {
        return;
    };
    let tmp = dir.join(format!(".fcs-dump-built.tmp-{}", std::process::id()));
    if std::fs::write(&tmp, contents).is_ok() && std::fs::rename(&tmp, marker).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

pub fn project_dir() -> PathBuf {
    workspace_root().join("tools").join("fcs-dump")
}

/// Root of the F# corpus the differential tests walk, taken from the
/// `BORZOI_CORPUS` environment variable.
///
/// `nix develop` sets this to the pinned `fsharp-src` flake input — a
/// content-addressed checkout of the F# compiler in the Nix store (see
/// `flake.nix`). There is no on-disk fallback; run the corpus tests under
/// `nix develop`, or point `BORZOI_CORPUS` at a local F# checkout.
///
/// Panics if the variable is unset or does not resolve to a directory.
pub fn corpus_root() -> PathBuf {
    let root = match std::env::var_os("BORZOI_CORPUS") {
        Some(p) => PathBuf::from(p),
        None => panic!(
            "BORZOI_CORPUS is not set. Run the corpus tests under \
             `nix develop` (which points it at the pinned `fsharp-src` flake \
             input), or set it to a local F# compiler checkout."
        ),
    };
    assert!(
        root.is_dir(),
        "F# corpus root {root:?} (from BORZOI_CORPUS) is not a directory."
    );
    root
}

/// Recursively collect F# source files under `root`.
///
/// The corpus can contain symlinks, so the walk follows symlinked directories
/// but records canonical directory targets to avoid cycles. Symlinked files are
/// included by the link path, matching the path the parser and oracle will read.
pub fn collect_fsharp_corpus_files(root: &Path) -> Result<Vec<PathBuf>, CorpusWalkError> {
    let mut out = Vec::new();
    let mut seen_dirs = HashSet::new();
    collect_fsharp_corpus_files_into(root, &mut out, &mut seen_dirs)?;
    out.sort();
    Ok(out)
}

#[derive(Debug)]
pub enum CorpusWalkError {
    CanonicalizeDir {
        path: PathBuf,
        source: std::io::Error,
    },
    ReadDir {
        path: PathBuf,
        source: std::io::Error,
    },
    ReadEntry {
        path: PathBuf,
        source: std::io::Error,
    },
    FileType {
        path: PathBuf,
        source: std::io::Error,
    },
    Metadata {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for CorpusWalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CanonicalizeDir { path, source } => {
                write!(f, "canonicalize directory {}: {source}", path.display())
            }
            Self::ReadDir { path, source } => {
                write!(f, "read directory {}: {source}", path.display())
            }
            Self::ReadEntry { path, source } => {
                write!(f, "read directory entry in {}: {source}", path.display())
            }
            Self::FileType { path, source } => {
                write!(f, "read file type for {}: {source}", path.display())
            }
            Self::Metadata { path, source } => {
                write!(
                    f,
                    "read symlink target metadata for {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for CorpusWalkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CanonicalizeDir { source, .. }
            | Self::ReadDir { source, .. }
            | Self::ReadEntry { source, .. }
            | Self::FileType { source, .. }
            | Self::Metadata { source, .. } => Some(source),
        }
    }
}

fn collect_fsharp_corpus_files_into(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    seen_dirs: &mut HashSet<PathBuf>,
) -> Result<(), CorpusWalkError> {
    let canonical = dir
        .canonicalize()
        .map_err(|source| CorpusWalkError::CanonicalizeDir {
            path: dir.to_path_buf(),
            source,
        })?;
    if !seen_dirs.insert(canonical) {
        return Ok(());
    }

    let entries = std::fs::read_dir(dir).map_err(|source| CorpusWalkError::ReadDir {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CorpusWalkError::ReadEntry {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| CorpusWalkError::FileType {
                path: path.clone(),
                source,
            })?;

        if file_type.is_dir() {
            if is_skipped_corpus_dir(&path) {
                continue;
            }
            collect_fsharp_corpus_files_into(&path, out, seen_dirs)?;
            continue;
        }

        if file_type.is_symlink() {
            let metadata =
                std::fs::metadata(&path).map_err(|source| CorpusWalkError::Metadata {
                    path: path.clone(),
                    source,
                })?;
            if metadata.is_dir() {
                if is_skipped_corpus_dir(&path) {
                    continue;
                }
                collect_fsharp_corpus_files_into(&path, out, seen_dirs)?;
            } else if metadata.is_file() && is_fsharp_source_path(&path) {
                out.push(path);
            }
            continue;
        }

        if file_type.is_file() && is_fsharp_source_path(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn is_skipped_corpus_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|s| s.to_str()),
        Some(".git" | "target" | "artifacts" | "bin" | "obj")
    )
}

fn is_fsharp_source_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "fs" | "fsi" | "fsx"))
}

pub fn read_corpus_source(path: &Path) -> Result<String, CorpusSourceReadError> {
    let bytes = std::fs::read(path).map_err(|source| CorpusSourceReadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    String::from_utf8(bytes).map_err(|source| CorpusSourceReadError::NonUtf8 {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug)]
pub enum CorpusSourceReadError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    NonUtf8 {
        path: PathBuf,
        source: std::string::FromUtf8Error,
    },
}

impl CorpusSourceReadError {
    pub fn is_non_utf8(&self) -> bool {
        matches!(self, Self::NonUtf8 { .. })
    }
}

impl std::fmt::Display for CorpusSourceReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "read corpus source {}: {source}", path.display())
            }
            Self::NonUtf8 { path, source } => {
                write!(
                    f,
                    "decode corpus source {} as UTF-8: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for CorpusSourceReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::NonUtf8 { source, .. } => Some(source),
        }
    }
}

// ============================================================================
// Normalised token type — the diff currency
// ============================================================================

/// Each side of the differential diff reduces to a list of these. `kind` is
/// an `FSharpTokenKind` variant name as it appears in FCS's enum (e.g.
/// `"Int32"`, `"OffsideLet"`); `start..end` is a half-open byte range into
/// the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedToken {
    pub kind: String,
    pub start: usize,
    pub end: usize,
}

// ============================================================================
// FCS dump JSON parsing
// ============================================================================

#[derive(Deserialize)]
struct FcsDump {
    #[serde(rename = "Tokens")]
    tokens: Vec<FcsToken>,
}

#[derive(Deserialize)]
struct FcsToken {
    #[serde(rename = "Kind")]
    kind: String,
    #[serde(rename = "Range")]
    range: FcsRange,
}

#[derive(Deserialize)]
struct FcsRange {
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

/// Diagnostics slice of the `fcs-dump ast` payload: the `Diagnostics` array,
/// each entry carrying an `ErrorNumber` and a `Range`. Used by
/// [`assert_asts_match_with_diagnostic`] to pin a recoverable FCS diagnostic
/// (e.g. FS1161 "TABs are not allowed") by error number and byte span.
#[derive(Deserialize)]
struct FcsAstDiagnostics {
    #[serde(rename = "Diagnostics")]
    diagnostics: Vec<FcsAstDiagnostic>,
}

#[derive(Deserialize)]
struct FcsAstParseStatus {
    #[serde(rename = "ParseHadErrors")]
    parse_had_errors: bool,
}

#[derive(Deserialize)]
struct FcsAstDiagnostic {
    #[serde(rename = "ErrorNumber")]
    error_number: i64,
    #[serde(rename = "Message")]
    message: String,
    #[serde(rename = "Range")]
    range: FcsRange,
    /// `"Error"` / `"Warning"` — `FSharpDiagnosticSeverity.ToString()` from
    /// `fcs-dump`. Load-bearing for the offside oracle: the same FS0058 is an
    /// error at F# 8+ and a warning below, so the version-pinned diff must pin
    /// severity, not just span + message.
    #[serde(rename = "Severity")]
    severity: String,
}

pub fn fcs_parse_had_errors(json: &str) -> bool {
    let dump: FcsAstParseStatus =
        serde_json::from_str(json).expect("fcs-dump ast parse status shape");
    dump.parse_had_errors
}

fn assert_fcs_parse_clean(json: &str, source: &str) {
    assert!(
        !fcs_parse_had_errors(json),
        "expected FCS to parse cleanly for {source:?}, but ParseHadErrors was true",
    );
}

fn assert_fcs_parse_rejected(json: &str, source: &str) {
    assert!(
        fcs_parse_had_errors(json),
        "expected FCS to report parse errors for {source:?}, but ParseHadErrors was false",
    );
}

/// Byte spans of every `fcs-dump ast` diagnostic whose `ErrorNumber` equals
/// `error_number`, in the order FCS reported them. FCS positions are 1-based
/// line / 0-based UTF-16 column, so each end is mapped through [`LineIndex`].
pub fn fcs_diagnostic_spans(
    json: &str,
    source: &str,
    error_number: i64,
) -> Vec<std::ops::Range<usize>> {
    let dump: FcsAstDiagnostics =
        serde_json::from_str(json).expect("fcs-dump ast diagnostics shape");
    let line_index = LineIndex::new(source);
    dump.diagnostics
        .into_iter()
        .filter(|d| d.error_number == error_number)
        .map(|d| {
            line_index.offset(d.range.start.line, d.range.start.col)
                ..line_index.offset(d.range.end.line, d.range.end.col)
        })
        .collect()
}

/// Like [`fcs_diagnostic_spans`], but paired with each diagnostic's full
/// message text — used to pin FS0058 message parity (the limiting-context
/// position FCS embeds) in addition to the span.
pub fn fcs_diagnostics(
    json: &str,
    source: &str,
    error_number: i64,
) -> Vec<(std::ops::Range<usize>, String)> {
    fcs_diagnostics_with_severity(json, source, error_number)
        .into_iter()
        .map(|(span, msg, _severity)| (span, msg))
        .collect()
}

/// Like [`fcs_diagnostics`], but also carries each diagnostic's severity string
/// (`"Error"` / `"Warning"`) — for oracles that must pin the error-vs-warning
/// split, e.g. an FS0058 that flips severity with the language version.
pub fn fcs_diagnostics_with_severity(
    json: &str,
    source: &str,
    error_number: i64,
) -> Vec<(std::ops::Range<usize>, String, String)> {
    let dump: FcsAstDiagnostics =
        serde_json::from_str(json).expect("fcs-dump ast diagnostics shape");
    let line_index = LineIndex::new(source);
    dump.diagnostics
        .into_iter()
        .filter(|d| d.error_number == error_number)
        .map(|d| {
            (
                line_index.offset(d.range.start.line, d.range.start.col)
                    ..line_index.offset(d.range.end.line, d.range.end.col),
                d.message,
                d.severity,
            )
        })
        .collect()
}

pub fn parse_fcs_dump(json: &str, source: &str) -> Vec<NormalisedToken> {
    let dump: FcsDump = serde_json::from_str(json).expect("fcs-dump JSON shape");
    let line_index = LineIndex::new(source);
    dump.tokens
        .into_iter()
        .map(|t| NormalisedToken {
            kind: t.kind,
            start: line_index.offset(t.range.start.line, t.range.start.col),
            end: line_index.offset(t.range.end.line, t.range.end.col),
        })
        .collect()
}

/// Lookup byte offset for an FCS `(line, col)` position. FCS uses 1-based
/// lines and 0-based columns, and columns count **UTF-16 code units** — so
/// `col` cannot just be added to the line's byte start: every BMP non-ASCII
/// char (`—` is 3 UTF-8 bytes / 1 UTF-16 unit) and every supplementary char
/// (`💩` is 4 UTF-8 bytes / 2 UTF-16 units via surrogate pair) shifts the
/// two scales apart. FCS also strips a file-start UTF-8 BOM for line-1 column
/// positions, so line 1, column 0 is the byte after that BOM. `offset` walks
/// the line a char at a time, accumulating UTF-16 units until it reaches `col`,
/// and returns the byte position.
pub struct LineIndex<'a> {
    source: &'a str,
    /// Byte offset of the start of each line (1-indexed: `starts[1]` is line 1).
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        // `starts[0]` is unused so we can index by 1-based line number.
        let line_one_start = source
            .strip_prefix('\u{feff}')
            .map_or(0, |_| '\u{feff}'.len_utf8());
        let mut starts = vec![0, line_one_start];
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
        // FCS sometimes reports end positions that point one past the last
        // line (line = lastLine+1, col = 0). Clamp those to the source length.
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
            // char boundary just before. FCS itself only emits col positions
            // at char boundaries, so this only fires on malformed input.
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
// Rust Token → FCS FSharpTokenKind name mapping (used by both raw &
// filtered diff harnesses — the filtered stream still contains the same
// raw tokens, plus virtual ones)
// ============================================================================

pub fn is_trivia(tok: &Token<'_>) -> bool {
    matches!(
        tok,
        Token::Whitespace | Token::Newline | Token::LineComment | Token::BlockComment
    )
}

pub fn rust_kind_name(tok: &Token<'_>) -> String {
    use Token::*;
    let s = match tok {
        // ---- keywords -------------------------------------------------------
        And => "And",
        As => "As",
        Assert => "Assert",
        Base => "Base",
        Begin => "Begin",
        Class => "Class",
        Do => "Do",
        Done => "Done",
        DownTo => "DownTo",
        Else => "Else",
        End => "End",
        Exception => "Exception",
        False => "False",
        Finally => "Finally",
        For => "For",
        Fun => "Fun",
        Function => "Function",
        If => "If",
        In => "In",
        Inherit => "Inherit",
        Lazy => "Lazy",
        Let => "Let",
        Match => "Match",
        Mod => "InfixMod",
        Module => "Module",
        Mutable => "Mutable",
        New => "New",
        Of => "Of",
        Open => "Open",
        Or => "Or",
        Private => "Private",
        Rec => "Rec",
        Sig => "Sig",
        Struct => "Struct",
        Then => "Then",
        To => "To",
        True => "True",
        Try => "Try",
        Type => "Type",
        Val => "Val",
        When => "When",
        While => "While",
        With => "With",
        Underscore => "Underscore",
        Abstract => "Abstract",
        Const => "Const",
        Default => "Default",
        Delegate => "Delegate",
        Downcast => "Downcast",
        Elif => "Elif",
        Extern => "Extern",
        Fixed => "Fixed",
        Global => "Global",
        Inline => "Inline",
        Interface => "Interface",
        Internal => "Internal",
        Member => "Member",
        Namespace => "Namespace",
        Null => "Null",
        Override => "Override",
        Public => "Public",
        // `return` lexes to `YIELD(false)` (see LexHelpers.fs:352), sharing the
        // Yield kind with `yield`. Likewise `use` shares Let with `let`.
        Return => "Yield",
        Static => "Static",
        Upcast => "Upcast",
        Use => "Let",
        Void => "Void",
        Yield => "Yield",
        DoBang => "DoBang",
        YieldBang => "YieldBang",
        ReturnBang => "YieldBang",
        MatchBang => "MatchBang",
        AndBang => "Binder",
        LetBang => "Binder",
        UseBang => "Binder",
        WhileBang => "WhileBang",

        // ---- identifiers ----------------------------------------------------
        Ident(_) | QuotedIdent(_) => "Identifier",
        KeywordString(_) => "KeywordString",

        // ---- numeric literals ----------------------------------------------
        Int(_) => "Int32",
        XInt(_) => "Int32",
        XIntSuffixed(s) | IntSuffixed(s) => return int_suffix_kind(s).into(),
        BigNum(_) => "BigNumber",
        Decimal(_) => "Decimal",
        Float32(_) | XIEEE32(_) => "Ieee32",
        Float64(_) | XIEEE64(_) => "Ieee64",
        IntDotDot(_) => "Int32DotDot",

        // ---- char & string -------------------------------------------------
        // Byte char literals (`'A'B`) lex.fsl-side become UINT8 (see
        // lex.fsl:526-585), which maps to `FSharpTokenKind.UInt8`
        // (ServiceLexing.fs:1555). Our Rust regex aggregates both forms
        // into `Char`; dispatch on the suffix.
        Char(s) => {
            if s.ends_with("'B") {
                "UInt8"
            } else {
                "Char"
            }
        }
        TripleString | VerbatimString | String => "String",
        // FCS surfaces all four `INTERP_STRING_*` tokens as
        // `FSharpTokenKind.String` (ServiceLexing.fs:1573-1577): the
        // public token API doesn't distinguish bare strings, interp
        // openers, interp parts, or interp ends.
        InterpString(_) => "String",

        // ---- punctuation ---------------------------------------------------
        LParen => "LeftParenthesis",
        RParen => "RightParenthesis",
        LBrack => "LeftBracket",
        RBrack => "RightBracket",
        LBrackBar => "LeftBracketBar",
        BarRBrack => "BarRightBracket",
        LBrackLess => "LeftBracketLess",
        GreaterRBrack => "GreaterRightBracket",
        LBrace => "LeftBrace",
        RBrace => "RightBrace",
        LBraceBar => "LeftBraceBar",
        BarRBrace => "BarRightBrace",
        Comma => "Comma",
        SemiSemi => "SemicolonSemicolon",
        Semi => "Semicolon",
        DotDotHat => "DotDotHat",
        DotDot => "DotDot",
        Dot => "Dot",
        ColonColon => "ColonColon",
        ColonQMarkGreater => "ColonQuestionMarkGreater",
        ColonQMark => "ColonQuestionMark",
        ColonGreater => "ColonGreater",
        ColonEquals => "ColonEquals",
        Colon => "Colon",
        RArrow => "RightArrow",
        LArrow => "LeftArrow",
        Equals => "Equals",
        AmpAmp => "AmpersandAmpersand",
        Amp => "Ampersand",
        BarBar => "BarBar",
        Bar => "Bar",
        QMarkQMark => "QuestionMarkQuestionMark",
        QMark => "QuestionMark",
        Hash => "Hash",
        Dollar => "Dollar",
        Tilde => "PrefixOperator",
        // Standalone `'` (e.g. in type parameters `'T`) maps to FCS's
        // `QUOTE`, which `ServiceLexing.fs:1472` collapses into
        // `FSharpTokenKind.RightQuote` (the *same* kind it uses for the
        // closing `@>` of a quotation).
        Quote => "RightQuote",
        LQuote | LQuoteRaw => "LeftQuote",
        RQuote | RQuoteRaw => "RightQuote",
        // Single `<`/`>` (regardless of typar-vs-comparison bool). FCS's
        // public `FSharpTokenKind` flattens both `LESS true`/`LESS false`
        // and `GREATER true`/`GREATER false` to `Less`/`Greater` (the bool
        // is only on the internal LexFilter token type), so this mapping is
        // bool-agnostic.
        Less(_) => "Less",
        Greater(_) => "Greater",
        // Compound tokens: FCS emits them as-is in the raw (pre-LexFilter)
        // stream with dedicated kinds (ServiceLexing.fs:1536-1537); the
        // LexFilter splits them before the parser sees them. Map to the FCS
        // raw kind so the lexer_diff tests agree.
        RQuoteDot | RQuoteRawDot => "RightQuoteDot",
        RQuoteBarRBrace | RQuoteRawBarRBrace => "RQuoteBarRightBrace",
        LParenStarRParen => "LeftParenthesisStarRightParenthesis",
        FunkyOpName(_) => "FunkyOperatorName",

        Op(s) => return op_kind_name(s).into(),

        // ---- trivia --------------------------------------------------------
        // Caller filters these before reaching us; surface the bug if not.
        Whitespace | Newline | LineComment | BlockComment => {
            panic!("trivia token {tok:?} reached rust_kind_name; caller forgot to filter")
        }
    };
    s.into()
}

/// Pick the FCS `FSharpTokenKind` name for an `IntSuffixed`/`XIntSuffixed`
/// token from its trailing suffix (`y`, `uy`, `s`, `us`, `l`, `u`, `ul`, `n`,
/// `un`, `L`, `UL`, `uL`). Mirrors lex.fsl's `integer_size_suffix`.
///
/// `L` collapses to `UInt64` (not `Int64`) to match an apparent upstream bug
/// in ServiceLexing.fs:1556 — `INT64 _ -> FSharpTokenKind.UInt64`. Both signed
/// and unsigned 64-bit literals report the same kind through the public API;
/// the `FSharpTokenKind.Int64` case is effectively unreachable. We mirror the
/// bug so the differential test passes on real FCS output.
fn int_suffix_kind(text: &str) -> &'static str {
    // Longest suffix first to disambiguate `uL` vs `L`.
    const SUFFIXES: &[(&str, &str)] = &[
        ("UL", "UInt64"),
        ("uL", "UInt64"),
        ("ul", "UInt32"),
        ("un", "UNativeInt"),
        ("uy", "UInt8"),
        ("us", "UInt16"),
        ("L", "UInt64"), // FCS bug: INT64 maps to UInt64, see doc above.
        ("n", "NativeInt"),
        ("u", "UInt32"),
        ("l", "Int32"),
        ("s", "Int16"),
        ("y", "Int8"),
    ];
    for (suf, kind) in SUFFIXES {
        if text.ends_with(suf) {
            return kind;
        }
    }
    panic!("int suffix not recognised: {text:?}");
}

/// Map an operator's text to an `FSharpTokenKind` name following lex.fsl's
/// precedence-bucket rules (lines 970-986). `ignored_op_char = . | $ | ?`
/// can prefix the "significant" character.
///
/// Single-character `*`, `-`, `%` and the exact string `%%` are *carve-outs*
/// in lex.fsl: they produce dedicated `STAR` (839), `MINUS` (964),
/// `PERCENT_OP` (960/962) tokens rather than the precedence buckets. The
/// general-operator rules only kick in when these chars have extra op chars
/// attached (`**`, `*=`, `-->`, `%>`, `<=`, `>=` etc.). Single `<`/`>` are
/// also FCS carve-outs (`LESS`/`GREATER`), but on the Rust side they lex as
/// dedicated `Token::Less(bool)`/`Token::Greater(bool)` variants and never
/// reach this function.
///
/// **Total.** The lexer's `Op` regex over-munches `:`- and `.`-led runs into a
/// single token (`::!`, `:^`, `...`) that FCS would split and that has no single
/// kind. Rather than panic — which aborts the corpus sweep and drops the whole
/// file — such inputs return [`UNCLASSIFIED_OP`], a sentinel that can never equal
/// a real `FSharpTokenKind`, so the diff always records them as a divergence.
const UNCLASSIFIED_OP: &str = "<unclassified-op>";

fn op_kind_name(text: &str) -> &'static str {
    // Exact-text carve-outs (no leading ignored_op_chars, no trailing op_chars).
    match text {
        "*" => return "Star",
        "-" => return "Minus",
        "%" | "%%" => return "PercentOperator",
        _ => {}
    }

    // Strip leading purely-ignored op chars (`.`, `?`) to find the
    // bucket-defining character. `$` is NOT stripped: it is both an
    // `ignored_op_char` in FCS lex.fsl *and* an INFIX_COMPARE_OP head, so the
    // lexer classifies `$!` etc. as InfixCompareOperator, not PrefixOperator.
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b'.' | b'?') {
        i += 1;
    }
    // If the whole text is ignored_op_chars, the lexer over-munched a `.`-led
    // run (e.g. `...`, which FCS splits into `DotDot`+`Dot`). No single FCS
    // kind applies — sentinel so the diff records a divergence (see doc above).
    if i >= bytes.len() {
        return UNCLASSIFIED_OP;
    }

    let head = bytes[i];
    // `**` followed by op_chars → INFIX_STAR_STAR_OP. Check 2-char prefix.
    if head == b'*' && bytes.get(i + 1) == Some(&b'*') {
        return "InfixStarStarOperator";
    }
    match head {
        b'*' | b'/' | b'%' => "InfixStarDivideModuloOperator",
        b'+' | b'-' => "PlusMinusOperator",
        b'@' | b'^' => "InfixAtHatOperator",
        b'=' | b'<' | b'$' | b'>' => "InfixCompareOperator",
        b'!' if bytes.get(i + 1) == Some(&b'=') => "InfixCompareOperator",
        b'&' => "InfixAmpersandOperator",
        b'|' => "InfixBarOperator",
        b'!' | b'~' => "PrefixOperator",
        // `:`-led runs the lexer over-munched (e.g. `::!`, `:^`); FCS splits
        // these (`ColonColon`/`Colon` + a following op token), so no single
        // kind applies — sentinel so the diff records a divergence.
        _ => UNCLASSIFIED_OP,
    }
}

// ============================================================================
// FilteredToken → FCS FSharpTokenKind name mapping
// ============================================================================

/// Map a `FilteredToken` to the FCS kind name that `tokens-filtered` would
/// emit for it. Raw tokens delegate to [`rust_kind_name`]; virtuals get the
/// `Offside*` name.
pub fn filtered_kind_name(tok: &FilteredToken<'_>) -> String {
    match tok {
        FilteredToken::Raw(t) => rust_kind_name(t),
        FilteredToken::Virtual(v) => virtual_kind_name(v).into(),
    }
}

fn virtual_kind_name(v: &Virtual) -> &'static str {
    match v {
        Virtual::Let => "OffsideLet",
        Virtual::Binder => "OffsideBinder",
        // `OAND_BANG` has no `FSharpTokenKind` arm (→ `None`), so FCS's public
        // lexer drops it and `assert_filtered_streams_match` drops our
        // `Virtual::AndBang` to match. This name is therefore a sentinel that
        // never reaches a comparison — if the drop is ever removed, the diff
        // fails loudly (the name appears nowhere in any FCS dump) rather than
        // silently passing.
        Virtual::AndBang => "OffsideAndBang",
        Virtual::BlockBegin => "OffsideBlockBegin",
        Virtual::BlockEnd => "OffsideBlockEnd",
        Virtual::DeclEnd => "OffsideDeclEnd",
        Virtual::BlockSep => "OffsideBlockSep",
        Virtual::Do => "OffsideDo",
        Virtual::DoBang => "OffsideDoBang",
        Virtual::Then => "OffsideThen",
        Virtual::Else => "OffsideElse",
        Virtual::Fun => "OffsideFun",
        Virtual::Function => "OffsideFunction",
        Virtual::Lazy => "OffsideLazy",
        Virtual::Assert => "OffsideAssert",
        Virtual::End => "OffsideEnd",
        Virtual::With => "OffsideWith",
        // FCS surfaces the rewritten `IN`→`JOIN_IN` token as
        // `FSharpTokenKind.JoinIn` (ServiceLexing.fs:1508), whose `ToString()`
        // is `"JoinIn"` — the name the `tokens-filtered` dump emits.
        Virtual::JoinIn => "JoinIn",
        Virtual::InterfaceMember => "OffsideInterfaceMember",
        Virtual::RightBlockEnd => "OffsideRightBlockEnd",
        Virtual::HighPrecedenceTyApp => "HighPrecedenceTypeApp",
        Virtual::HighPrecedenceParenApp => "HighPrecedenceParenthesisApp",
        Virtual::HighPrecedenceBrackApp => "HighPrecedenceBracketApp",
    }
}

// ============================================================================
// Divergence reporting
// ============================================================================

pub fn report_divergence(source: &str, rust: &[NormalisedToken], fcs: &[NormalisedToken]) -> ! {
    let mut msg = String::new();
    msg.push_str("token streams diverge.\n\n");
    msg.push_str("source:\n");
    for line in source.lines() {
        msg.push_str("  ");
        msg.push_str(line);
        msg.push('\n');
    }
    msg.push('\n');

    let limit = rust.len().max(fcs.len());
    msg.push_str("idx  rust                            fcs\n");
    for i in 0..limit {
        let r = rust.get(i);
        let f = fcs.get(i);
        let same = r == f;
        msg.push_str(&format!(
            "{:>3}{}  {:<30}  {}\n",
            i,
            if same { " " } else { "*" },
            r.map_or("—".into(), fmt_token),
            f.map_or("—".into(), fmt_token),
        ));
    }
    panic!("{msg}");
}

pub fn fmt_token(t: &NormalisedToken) -> String {
    format!("{} [{}..{})", t.kind, t.start, t.end)
}

// ============================================================================
// AST differential assertions
// ============================================================================
//
// Shared by the `parser_diff_*` case groups (split out of the former
// monolithic `tests/parser_diff.rs`). Each writes `source` to a tempfile,
// dumps FCS's `ParsedInput`, normalises both sides, and `assert_eq!`s them.

/// Assert our parser and FCS agree on the normalised AST for `source`, and
/// that our parser reports no errors.
pub fn assert_asts_match(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = fcs_ast_batch(path);
    assert_fcs_parse_clean(&json, source);
    let fcs = normalised_ast::normalise_fcs_dump(&json);

    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "rust parser produced errors for {source:?}: {:?}",
        parse.errors,
    );
    let rust = normalised_ast::normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "AST divergence for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );
}

/// Assert our parser and FCS reach the same **verdict** on `source` (both clean
/// or both erroring) and, when both are clean, the same normalised AST.
///
/// Unlike [`assert_asts_match`], the caller does not have to know in advance
/// whether FCS accepts — FCS decides, and we must merely agree. That is what
/// makes it usable as the oracle for a *generated matrix* of sources spanning a
/// grammar production, where hand-classifying every cell is exactly the manual
/// labour the matrix exists to remove: a cell we wrongly reject and a cell we
/// wrongly accept both fail, and so does a cell we accept with the wrong tree.
///
/// Returns `true` if FCS accepted, so a caller can assert coverage (e.g. "this
/// matrix must contain at least one accepted and one rejected cell", pinning
/// that the matrix has not degenerated into all-reject and stopped testing
/// anything).
pub fn assert_parse_verdicts_match(source: &str) -> bool {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");

    let json = fcs_ast_batch(tmp.path());
    let fcs_rejects = fcs_parse_had_errors(&json);
    let parse = parse(source);
    let we_reject = !parse.errors.is_empty();

    assert_eq!(
        we_reject,
        fcs_rejects,
        "parse-verdict divergence for {source:?}: we {}, FCS {}{}",
        if we_reject { "reject" } else { "accept" },
        if fcs_rejects { "rejects" } else { "accepts" },
        if we_reject {
            format!("\n  our errors: {:?}", parse.errors)
        } else {
            String::new()
        },
    );

    // A shared verdict of "reject" says nothing about the tree (FCS's recovery
    // AST is not modelled), so only compare structure when both accepted.
    if !fcs_rejects {
        let fcs = normalised_ast::normalise_fcs_dump(&json);
        let rust = normalised_ast::normalise_parse(&parse);
        assert_eq!(
            rust, fcs,
            "AST divergence for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
        );
    }
    !fcs_rejects
}

/// Run `source` through a *one-shot* `fcs-dump ast` with
/// `BORZOI_FCS_LANGVERSION` set to `lang_text` (a canonical token such as
/// `"7.0"`), so FCS parses at that `<LangVersion>`. The batched child can't carry
/// a per-request version (it reads the env once at spawn), so this pays a fresh
/// process — fine for the handful of version-sensitive fixtures.
fn fcs_ast_at_langversion(source: &Path, lang_text: &str) -> String {
    let mut cmd = fcs_dump_command("ast");
    cmd.arg(source);
    cmd.env("BORZOI_FCS_LANGVERSION", lang_text);
    let out =
        BoundedCommand::new(cmd).run_ok(format_args!("fcs-dump ast (langversion {lang_text})"));
    String::from_utf8(out.stdout).expect("fcs-dump stdout is UTF-8")
}

/// Like [`assert_asts_match`], but parses **both sides at `lang`** (FCS via
/// `BORZOI_FCS_LANGVERSION`, ours via [`parse_with_options`]). For a fixture
/// FCS parses cleanly at `lang` — no *errors*; warnings such as a below-strict
/// FS0058 are allowed — this pins that the modelled tree agrees at that version.
///
/// The load-bearing case is a below-`8.0` offside where FCS **warns but keeps**
/// the pushed context (nesting the following construct) rather than aborting the
/// push (leaving it a sibling): the severity gate and the push decision are the
/// same `strictIndentation` boolean, so the tree — not just the diagnostic —
/// depends on the version. `lang_text` is the canonical token FCS wants (e.g.
/// `"7.0"`); `lang` is the matching [`LanguageVersion`] for our parser.
pub fn assert_asts_match_at_langversion(source: &str, lang: LanguageVersion, lang_text: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = fcs_ast_at_langversion(path, lang_text);
    assert_fcs_parse_clean(&json, source);
    let fcs = normalised_ast::normalise_fcs_dump(&json);

    let symbols = HashSet::new();
    let parse = parse_with_options(
        source,
        ParseOptions {
            file_kind: FileKind::Impl,
            symbols: &symbols,
            lang,
        },
    );
    assert!(
        parse.errors.is_empty(),
        "rust parser produced errors for {source:?} at {lang:?}: {:?}",
        parse.errors,
    );
    let rust = normalised_ast::normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "AST divergence at {lang:?} for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );
}

/// Like [`assert_asts_match`], but for a `source` FCS still emits an AST for
/// while reporting a *recoverable* diagnostic `FS<error_number>` — e.g. FS1161
/// "TABs are not allowed in F# code", which FCS errors on then treats the tab
/// as ordinary whitespace. Asserts:
///
///  * FCS reports at least one `FS<error_number>` diagnostic (the test would be
///    vacuous otherwise — guards against the fixture drifting to a clean parse);
///  * the normalised ASTs agree (the diagnostic is recoverable, so both sides
///    still produce the same tree);
///  * our parser emits an error at the **same byte span** as each such FCS
///    diagnostic. We don't carry FCS error numbers, so equal spans is the
///    strongest cross-check available — it pins that we flag the same bytes,
///    not merely that we flag *something*.
///
/// This is the diagnostic-comparing counterpart the plain AST diff can't be:
/// `assert_asts_match` only checks our error list is *empty*, so a recoverable
/// FCS error that leaves the tree intact (like FS1161) is otherwise invisible
/// to the differential suite.
pub fn assert_asts_match_with_diagnostic(source: &str, error_number: i64) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = fcs_ast_batch(path);
    assert_fcs_parse_rejected(&json, source);
    let fcs_spans = fcs_diagnostic_spans(&json, source, error_number);
    assert!(
        !fcs_spans.is_empty(),
        "expected FCS to report FS{error_number} for {source:?}, but it reported none",
    );

    let fcs = normalised_ast::normalise_fcs_dump(&json);
    let parse = parse(source);
    let rust = normalised_ast::normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "AST divergence for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );

    for span in &fcs_spans {
        assert!(
            parse.errors.iter().any(|e| &e.span == span),
            "our parser is missing a diagnostic at byte span {span:?} \
             (FCS reports FS{error_number} there) for {source:?}; our errors: {:?}",
            parse.errors,
        );
    }
}

/// Pin the §A offside FS0058 diagnostics *exactly* against FCS: the **set**
/// of `(byte span, message)` pairs our lex-filter flags as offside must equal
/// the set FCS reports FS0058 at. Unlike
/// [`assert_asts_match_with_diagnostic`] — which only checks our errors
/// *include* each FCS span — this also fails on an **extra** diagnostic FCS
/// doesn't emit, which is the failure mode of an over-eager `is_correct_indent`
/// call site (e.g. a `replaceCtxtIgnoreIndent` ported without the
/// ignore-indent exemption). Comparing the full message additionally pins the
/// limiting-context position FCS embeds ("… started at position (2:5) …"), so
/// a wrong `undentationLimit` position — not just a wrong column — diverges.
///
/// Compares sets, not multisets: FCS can report the same FS0058 twice at one
/// span (strict-refusal + recovery re-push both warn), and the duplication is
/// an emission-count artefact rather than behaviour worth pinning.
///
/// Deliberately does **not** compare trees — the fixtures this targets are
/// syntax errors whose recovery shapes may legitimately diverge. Only
/// meaningful for fixtures where every FCS FS0058 is the §A flavour; other
/// FS0058 flavours (not yet emitted by us) would fail as "missing".
pub fn assert_offside_spans_match(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let json = fcs_ast_batch(tmp.path());

    let fcs_diags: std::collections::BTreeSet<(usize, usize, String)> =
        fcs_diagnostics(&json, source, 58)
            .into_iter()
            .map(|(s, msg)| (s.start, s.end, msg))
            .collect();
    assert!(
        !fcs_diags.is_empty(),
        "expected FCS to report FS0058 for {source:?}, but it reported none",
    );

    let parse = parse(source);
    let ours: std::collections::BTreeSet<(usize, usize, String)> = parse
        .errors
        .iter()
        .chain(parse.warnings.iter())
        .filter(|e| e.message.contains("offside of context started at position"))
        .map(|e| (e.span.start, e.span.end, e.message.clone()))
        .collect();

    assert_eq!(
        ours, fcs_diags,
        "offside FS0058 diagnostics diverge for {source:?}\n  ours: {ours:?}\n  fcs:  {fcs_diags:?}\n  all our errors: {:?}",
        parse.errors,
    );
}

/// [`assert_offside_spans_match`] at a pinned language version on **both** sides
/// (FCS via `BORZOI_FCS_LANGVERSION`, ours via [`parse_with_options`]). The
/// FS0058 §A offside set (span + message) must agree at `lang`.
///
/// Unlike [`assert_asts_match_at_langversion`] this does *not* require a clean
/// parse or matching trees — it targets *incomplete* inputs (e.g. `match x
/// with\n`) where both sides also emit unrelated recovery errors (FS0010/FS3107)
/// and the recovery tree may legitimately diverge. It pins only the §A FS0058s,
/// so it is the below-8.0 **warning** counterpart of `assert_offside_spans_match`
/// (below 8.0 the offside FS0058 is a warning, not an error): our warnings are
/// searched alongside our errors, and the version-pinned FCS side reports the
/// same FS0058 as a warning.
pub fn assert_offside_spans_match_at_langversion(
    source: &str,
    lang: LanguageVersion,
    lang_text: &str,
) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let json = fcs_ast_at_langversion(tmp.path(), lang_text);

    // (start, end, severity, message) — severity is load-bearing here: the whole
    // point of the version-pinned oracle is that FS0058 is a *warning* below
    // F# 8, so a bug that reported it as an error must fail this.
    let fcs_diags: std::collections::BTreeSet<(usize, usize, String, String)> =
        fcs_diagnostics_with_severity(&json, source, 58)
            .into_iter()
            .map(|(s, msg, sev)| (s.start, s.end, sev, msg))
            .collect();
    assert!(
        !fcs_diags.is_empty(),
        "expected FCS to report FS0058 for {source:?} at {lang:?}, but it reported none",
    );

    let symbols = HashSet::new();
    let parse = parse_with_options(
        source,
        ParseOptions {
            file_kind: FileKind::Impl,
            symbols: &symbols,
            lang,
        },
    );
    // Tag each of our offside FS0058s with the severity it landed at — an error
    // is in `parse.errors`, a warning in `parse.warnings` — so mislabelling
    // (error vs warning) diverges from FCS's serialized `Severity`.
    let tag = |severity: &'static str| {
        let iter = match severity {
            "Error" => parse.errors.iter(),
            _ => parse.warnings.iter(),
        };
        iter.filter(|e| e.message.contains("offside of context started at position"))
            .map(|e| {
                (
                    e.span.start,
                    e.span.end,
                    severity.to_string(),
                    e.message.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let ours: std::collections::BTreeSet<(usize, usize, String, String)> =
        tag("Error").into_iter().chain(tag("Warning")).collect();

    assert_eq!(
        ours, fcs_diags,
        "offside FS0058 diagnostics diverge at {lang:?} for {source:?}\n  ours: {ours:?}\n  fcs:  {fcs_diags:?}\n  all our diags: errors={:?} warnings={:?}",
        parse.errors, parse.warnings,
    );
}

/// Signature-file (`.fsi`) counterpart of [`assert_asts_match`] (phase 10.11).
/// Writes the source to a **`.fsi`** tempfile so `fcs-dump` auto-selects
/// signature parsing (it keys off the extension), and runs our [`parse_sig`]
/// entry. Asserts the normalised `SigFile` ASTs agree and our parser reports no
/// errors.
pub fn assert_sig_asts_match(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fsi").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = fcs_ast_batch(path);
    assert_fcs_parse_clean(&json, source);
    let fcs = normalised_ast::normalise_fcs_dump(&json);

    let parse = parse_sig(source);
    assert!(
        parse.errors.is_empty(),
        "rust parser produced errors for sig {source:?}: {:?}",
        parse.errors,
    );
    let rust = normalised_ast::normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "sig AST divergence for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );
}

/// Signature-file counterpart of [`assert_asts_match_allow_errors`] — for `.fsi`
/// inputs FCS treats as a parse error yet still emits an AST for (e.g. the
/// FS0222 decls-before-namespace recovery). Both sides must error and project
/// the same `SigFile` shape.
pub fn assert_sig_asts_match_allow_errors(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fsi").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = fcs_ast_batch(path);
    assert_fcs_parse_rejected(&json, source);
    let fcs = normalised_ast::normalise_fcs_dump(&json);

    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "expected rust parser to emit at least one error for sig {source:?} (FCS does too); got empty",
    );
    let rust = normalised_ast::normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "sig AST divergence for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );
}

/// Signature-file variant for inputs where FCS reports parse errors but our
/// parser is currently clean. This keeps the acceptance gap explicit at the
/// call site while still pinning the recovery AST shape FCS emits.
pub fn assert_sig_asts_match_fcs_rejects_ours_accepts(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fsi").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = fcs_ast_batch(path);
    assert_fcs_parse_rejected(&json, source);
    let fcs = normalised_ast::normalise_fcs_dump(&json);

    let parse = parse_sig(source);
    assert!(
        parse.errors.is_empty(),
        "expected rust parser to remain clean for known FCS-rejected sig recovery \
         fixture {source:?}; got {:?}",
        parse.errors,
    );
    let rust = normalised_ast::normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "sig AST divergence for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );
}

/// Like [`assert_asts_match`], but defines `defines` for conditional
/// compilation on both sides: FCS via `fcs-dump ast <file> SYM…`, ours via
/// `parse_with_symbols`. Asserts the active branch the symbols select parses
/// identically.
pub fn assert_asts_match_with_defines(source: &str, defines: &[&str]) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = invoke_fcs_dump_with_defines("ast", path, defines);
    assert_fcs_parse_clean(&json, source);
    let fcs = normalised_ast::normalise_fcs_dump(&json);

    let symbols: HashSet<String> = defines.iter().map(|s| s.to_string()).collect();
    let parse = parse_with_symbols(source, &symbols);
    assert!(
        parse.errors.is_empty(),
        "rust parser produced errors for {source:?} (defines {defines:?}): {:?}",
        parse.errors,
    );
    let rust = normalised_ast::normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "AST divergence for source {source:?} (defines {defines:?})\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );
}

/// Variant of [`assert_asts_match`] for inputs that FCS treats as a parse
/// error *yet still emits an AST for* — e.g. `let f = 1 and g = 2`
/// (FS0576 at the `let`). The harness still asserts AST equivalence
/// against FCS but doesn't require our parser to have an empty error list,
/// since matching FCS's diagnostic behaviour is part of being correct.
/// Diagnostic *messages* are not compared (we don't try to reproduce
/// FCS's exact wording); both sides must simply have a non-empty error
/// list, so this stays a meaningful check rather than a no-op.
pub fn assert_asts_match_allow_errors(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = fcs_ast_batch(path);
    assert_fcs_parse_rejected(&json, source);
    let fcs = normalised_ast::normalise_fcs_dump(&json);

    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "expected rust parser to emit at least one error for {source:?} (FCS does too); got empty",
    );
    let rust = normalised_ast::normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "AST divergence for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );
}

/// Variant of [`assert_asts_match`] for inputs where FCS reports parse errors
/// but our parser is currently clean. This keeps the acceptance gap explicit at
/// the call site while still pinning the recovery AST shape FCS emits.
pub fn assert_asts_match_fcs_rejects_ours_accepts(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = fcs_ast_batch(path);
    assert_fcs_parse_rejected(&json, source);
    let fcs = normalised_ast::normalise_fcs_dump(&json);

    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "expected rust parser to remain clean for known FCS-rejected recovery \
         fixture {source:?}; got {:?}",
        parse.errors,
    );
    let rust = normalised_ast::normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "AST divergence for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );
}

/// Variant of [`assert_asts_match`] for inputs FCS accepts but our parser
/// currently rejects. This keeps that rejection gap explicit while still
/// checking that the recovered AST shape matches FCS.
pub fn assert_asts_match_fcs_accepts_ours_rejects(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = fcs_ast_batch(path);
    assert_fcs_parse_clean(&json, source);
    let fcs = normalised_ast::normalise_fcs_dump(&json);

    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "expected rust parser to emit at least one error for known FCS-accepted \
         recovery fixture {source:?}; got empty",
    );
    let rust = normalised_ast::normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "AST divergence for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );
}

// ============================================================================
// Filtered token-stream differential assertion
// ============================================================================
//
// Used by the `tests/lexfilter_diff/` binary's submodules (split out of the
// former monolithic `tests/all/lexfilter_diff/`). Writes `source` to a tempfile,
// drives `fcs-dump tokens-filtered`, runs our `lexfilter::filter`, normalises
// both sides, and diffs them.

/// Assert our `lexfilter::filter` output matches FCS's post-`UseLexFilter`
/// token stream for `source`.
pub fn assert_filtered_streams_match(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create temp .fs file");
    tmp.write_all(source.as_bytes()).expect("write source");

    let fcs_json = fcs_tokens_filtered_batch(tmp.path());
    let fcs_tokens = parse_fcs_dump(&fcs_json, source);

    // `filter` consumes trivia internally (for offside line/column tracking)
    // and never emits Raw whitespace/comments — so no trivia filter is needed
    // on the output. We do drop `Virtual::BlockEnd` because FCS's outer
    // LexFilter wrapper (LexFilter.fs:2837) swallows OBLOCKEND, replacing it
    // with OBLOCKEND_*_COMING_SOON/IS_HERE tokens that all map to
    // `FSharpTokenKind.None` and get filtered. The public-facing stream the
    // harness sees from FCS therefore has no OBLOCKEND. We drop
    // `Virtual::AndBang` for the same reason: `OAND_BANG` has no
    // `FSharpTokenKind` arm (→ `None`), so FCS's public lexer never surfaces
    // the `and!` keyword. The real stream the parser consumes still carries
    // both virtuals; `and_bang_emits_virtual` pins `Virtual::AndBang` directly.
    let rust_tokens: Vec<NormalisedToken> = filter(source, lex(source))
        .filter(|(tok, _)| {
            !matches!(
                tok,
                Ok(FilteredToken::Virtual(Virtual::BlockEnd | Virtual::AndBang))
            )
        })
        .map(|(tok, span)| {
            let tok = tok.unwrap_or_else(|e| panic!("rust lex error {e:?} in {source:?}"));
            NormalisedToken {
                kind: filtered_kind_name(&tok),
                start: span.start,
                end: span.end,
            }
        })
        .collect();

    if rust_tokens != fcs_tokens {
        report_divergence(source, &rust_tokens, &fcs_tokens);
    }
}
