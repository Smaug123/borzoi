//! Deterministic per-phase counts for one `.fsproj` evaluation as the server
//! runs it: retired instructions, heap allocations and bytes allocated, each
//! the minimum over `EVAL_COUNTS_REPS` runs (default 5).
//!
//! The phases:
//!
//! - **lsp.evaluate**: what the server pays to evaluate the project again
//!   after its cached evaluation is dropped: SDK discovery, every MSBuild
//!   pass, and building the checks that confirm the result later
//!   (`borzoi::workspace::Workspace::project`).
//! - **msbuild.evaluate**: one `parse_fsproj_with_imports` call with the
//!   server's resolvers, the server's first pass alone.
//! - The parts of that call that do not depend on how the walk is written,
//!   each asked again over the call's own read set, one query per distinct
//!   read: **fs.text** (reading the files it read whole), **xml.parse**
//!   (parsing each as XML), **digest** (each text's `TextDigest`),
//!   **fs.other** (its other queries: `stat`s, listings and
//!   canonicalisations). Its `read_counts` say how many times it asked each
//!   kind, so a file read twice in one evaluation shows as more opens than
//!   distinct texts.
//!
//! The rest of `msbuild.evaluate` is the walk itself: expanding properties,
//! deciding conditions, matching globs and recording reads.
//!
//! Instructions come from the kernel's per-process counter
//! (`proc_pid_rusage`, macOS only; elsewhere the column reads `n/a`) and
//! include kernel mode, so the file-system phases carry the kernel's cost of
//! each call. Allocations come from a counting wrapper around the system
//! allocator. Wall time is not reported.
//!
//! ```sh
//! nix develop -c cargo run --release -p borzoi --example eval_counts
//! ```
//!
//! * `EVAL_COUNTS_PROJECT`: the `.fsproj` to evaluate. Default:
//!   `WoofWare.Incremental` from the whole-project gate's corpus cache.
//! * `EVAL_COUNTS_REPS` (default 5).
//! * `EVAL_COUNTS_LOOP`: run only `lsp.evaluate`, this many times, for a
//!   sampling profiler to attach to.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use borzoi::sdk_discovery::{SdkDiscovery, SdkDiscoveryEnv};
use borzoi::workspace::Workspace;
use borzoi_msbuild::observed::{self as msbuild_observed, Observation, Observer, TextDigest};
use borzoi_msbuild::{GlobResolver, ParsedProject, SdkResolver, parse_fsproj_with_imports};

/// The system allocator, counting calls and requested bytes.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

// SAFETY: every method forwards to `System` with its arguments unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Instructions this process has retired so far, where the platform says.
#[cfg(target_os = "macos")]
fn instructions_retired() -> Option<u64> {
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
        fn getpid() -> i32;
    }
    /// `RUSAGE_INFO_V4`, whose struct is a 16-byte UUID followed by `u64`s.
    const RUSAGE_INFO_V4: i32 = 4;
    /// `ri_instructions`'s index among those `u64`s.
    const RI_INSTRUCTIONS: usize = 29;
    let mut buffer = [0u64; 64];
    // SAFETY: `buffer` is larger than `rusage_info_v4` (37 words).
    let rc = unsafe { proc_pid_rusage(getpid(), RUSAGE_INFO_V4, buffer.as_mut_ptr()) };
    (rc == 0).then(|| buffer[2 + RI_INSTRUCTIONS])
}

#[cfg(not(target_os = "macos"))]
fn instructions_retired() -> Option<u64> {
    None
}

/// What one run of a phase cost.
#[derive(Clone, Copy)]
struct Cost {
    instructions: Option<u64>,
    allocations: u64,
    allocated_bytes: u64,
}

impl Cost {
    fn min(self, other: Cost) -> Cost {
        Cost {
            instructions: match (self.instructions, other.instructions) {
                (Some(a), Some(b)) => Some(a.min(b)),
                _ => None,
            },
            allocations: self.allocations.min(other.allocations),
            allocated_bytes: self.allocated_bytes.min(other.allocated_bytes),
        }
    }
}

/// Run `f` `reps` times, print the cheapest run of each count, and return the
/// last result, dropped outside the measured region.
fn phase<T>(label: &str, reps: usize, mut f: impl FnMut() -> T) -> T {
    let mut best: Option<Cost> = None;
    let mut last = None;
    for _ in 0..reps {
        drop(last.take());
        let (a0, b0, i0) = (
            ALLOCATIONS.load(Relaxed),
            ALLOCATED_BYTES.load(Relaxed),
            instructions_retired(),
        );
        let result = std::hint::black_box(f());
        let i1 = instructions_retired();
        let cost = Cost {
            instructions: i0.zip(i1).map(|(i0, i1)| i1 - i0),
            allocations: ALLOCATIONS.load(Relaxed) - a0,
            allocated_bytes: ALLOCATED_BYTES.load(Relaxed) - b0,
        };
        best = Some(best.map_or(cost, |b| b.min(cost)));
        last = Some(result);
    }
    let best = best.expect("at least one rep");
    let instructions = best
        .instructions
        .map_or_else(|| "n/a".to_string(), |i| format!("{:.2} M", i as f64 / 1e6));
    println!(
        "{label:<18} {instructions:>12} {:>10} allocs {:>12} bytes",
        best.allocations, best.allocated_bytes
    );
    last.expect("at least one rep")
}

fn default_project() -> PathBuf {
    let cache = std::env::var_os("BORZOI_PROJECT_CORPUS_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set"))
                .join(".cache/borzoi/project-corpus")
        });
    cache.join(
        "v2/project-corpus/Smaug123/WoofWare.Incremental/WoofWare.Incremental/WoofWare.Incremental.fsproj",
    )
}

/// The server's first MSBuild pass over `project`, with the server's
/// resolvers and default build properties.
fn evaluate_once(
    project: &Path,
    source: &str,
    env: &SdkDiscoveryEnv,
    extras: &HashMap<String, String>,
) -> ParsedProject {
    let disc = SdkDiscovery::for_project(project, env, &Observer::new())
        .expect("SDK discovery succeeds under nix develop");
    let resolver: &SdkResolver<'_> = &|name, observer| disc.resolve(name, observer);
    let glob_resolver: &GlobResolver<'_> = &borzoi_msbuild::glob_resolver::resolve;
    parse_fsproj_with_imports(
        source,
        project,
        extras,
        &env.build_environment,
        Some(resolver),
        Some(glob_resolver),
    )
    .expect("the project evaluates")
}

fn main() {
    let project = std::env::var_os("EVAL_COUNTS_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(default_project);
    let reps: usize = std::env::var("EVAL_COUNTS_REPS")
        .ok()
        .map(|r| r.parse().expect("EVAL_COUNTS_REPS is a number"))
        .unwrap_or(5);
    let env = SdkDiscoveryEnv::from_process_env();
    let mut workspace = Workspace::with_env(env.clone());
    let extras = workspace.build_properties();
    println!("project {}", project.display());

    assert!(
        workspace.project(&project).is_some(),
        "the project evaluates"
    );
    if let Some(loops) = std::env::var("EVAL_COUNTS_LOOP")
        .ok()
        .map(|n| n.parse::<usize>().expect("EVAL_COUNTS_LOOP is a number"))
    {
        phase("lsp.evaluate", loops, || {
            workspace.drop_every_evaluation();
            workspace.project(&project).is_some()
        });
        return;
    }
    phase("lsp.evaluate", reps, || {
        workspace.drop_every_evaluation();
        workspace.project(&project).is_some()
    });

    let source = msbuild_observed::host_fs::read_to_string(&project).expect("the project reads");
    let parsed = phase("msbuild.evaluate", reps, || {
        evaluate_once(&project, &source, &env, &extras)
    });
    let counts = parsed.read_counts;
    let reads = parsed.read_set.reads();
    let texts: Vec<&Path> = reads
        .iter()
        .filter(|read| matches!(read.observation, Observation::Text(Ok(_))))
        .map(|read| read.path.as_path())
        .collect();
    let others: Vec<_> = reads
        .iter()
        .filter(|read| !matches!(read.observation, Observation::Text(_)))
        .collect();
    let bytes: usize = texts
        .iter()
        .map(|path| {
            msbuild_observed::host_fs::read_to_string(path)
                .map(|t| t.len())
                .unwrap_or(0)
        })
        .sum();
    println!(
        "read_counts: {} opens, {} stats, {} listings, {} canonicalisations; \
         {} distinct reads, {} distinct texts ({} bytes), {} other distinct queries",
        counts.opens,
        counts.stats,
        counts.listings,
        counts.canonicalizations,
        reads.len(),
        texts.len(),
        bytes,
        others.len()
    );

    for (label, matches) in [
        (
            "canonical",
            (|o: &Observation| matches!(o, Observation::Canonical(_))) as fn(&Observation) -> bool,
        ),
        ("kind", |o| matches!(o, Observation::Kind(_))),
        ("link_kind", |o| matches!(o, Observation::LinkKind(_))),
        ("listing", |o| matches!(o, Observation::Listing(_))),
        ("stamp", |o| matches!(o, Observation::Stamp(_))),
    ] {
        let of_kind: Vec<_> = others
            .iter()
            .filter(|read| matches(&read.observation))
            .collect();
        phase(&format!("fs.{label}({})", of_kind.len()), reps, || {
            of_kind
                .iter()
                .map(|read| msbuild_observed::answer_now(&read.path, &read.observation))
                .collect::<Vec<_>>()
        });
    }
    let sources: Vec<String> = phase("fs.text", reps, || {
        texts
            .iter()
            .map(|path| msbuild_observed::host_fs::read_to_string(path).expect("still readable"))
            .collect()
    });
    phase("xml.parse", reps, || {
        sources
            .iter()
            .map(|text| roxmltree::Document::parse(text).map(|d| d.descendants().count()))
            .filter_map(Result::ok)
            .sum::<usize>()
    });
    phase("digest", reps, || {
        sources
            .iter()
            .map(|t| TextDigest::of(t))
            .collect::<Vec<_>>()
    });
    phase("fs.other", reps, || {
        others
            .iter()
            .map(|read| msbuild_observed::answer_now(&read.path, &read.observation))
            .collect::<Vec<_>>()
    });
}
