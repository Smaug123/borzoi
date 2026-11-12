//! Deterministic counts for `resolve_file` and `infer_file` over a corpus, each
//! file analysed on its own against the BCL: retired instructions, heap
//! allocations, reallocations and bytes allocated, all per source byte, beside
//! the wall time. `docs/sema-interning-plan.md` states each of its stages'
//! gains in these units.
//!
//! Instruction and allocation counts, unlike wall time, barely move with
//! machine load, so they are what a claimed gain is measured in; the wall time
//! is printed for orientation only. Instructions come from the kernel's
//! per-process counter (`proc_pid_rusage`, macOS only; elsewhere the column
//! reads `n/a`). Allocations come from a counting wrapper around the system
//! allocator. Each phase reports the minimum of each count over
//! `SEMA_COUNTS_REPS` runs (default 3).
//!
//! After the phases it prints a census of what the plan's representations
//! would hold, outside any measured region: identifier tokens and their
//! distinct spellings, the definitions and whether each one's `name` is its
//! defining token's text, and the published inferred types with their named
//! nodes and distinct paths.
//!
//! ```sh
//! nix develop -c sh -c 'SEMA_COUNTS_FSHARP_CORE=$PWD/tools/fcs-dump/bin/Release/net10.0/FSharp.Core.dll \
//!   cargo run --release -p borzoi-sema --example sema_counts'
//! ```
//!
//! * `BORZOI_CORPUS` (set by `nix develop`): the corpus root.
//! * `SEMA_COUNTS_SUBDIR` (default `src`): the subtree of it to measure; `.`
//!   measures the whole corpus.
//! * `SEMA_COUNTS_FSHARP_CORE`: an `FSharp.Core.dll` (building
//!   `tools/fcs-dump` puts one beside it). `System.Runtime.dll` and
//!   `netstandard.dll` come from the reference pack under `DOTNET_ROOT`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse_with_symbols;
use borzoi_cst::syntax::{AstNode, ImplFile, SyntaxKind};
use borzoi_sema::{
    AssemblyEnv, PrecedingTypes, ProjectItems, SyntaxRecovery, Ty, infer_file, resolve_file,
};

/// The system allocator, counting calls and requested bytes.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static REALLOCATIONS: AtomicU64 = AtomicU64::new(0);
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
        // Counted apart from allocations: a growing `Vec` reallocates, and the
        // plan's stages that presize or share storage move this column alone.
        REALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size.saturating_sub(layout.size()) as u64, Relaxed);
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
    wall: Duration,
    instructions: Option<u64>,
    allocations: u64,
    reallocations: u64,
    allocated_bytes: u64,
}

impl Cost {
    fn min(self, other: Cost) -> Cost {
        Cost {
            wall: self.wall.min(other.wall),
            instructions: match (self.instructions, other.instructions) {
                (Some(a), Some(b)) => Some(a.min(b)),
                _ => None,
            },
            allocations: self.allocations.min(other.allocations),
            reallocations: self.reallocations.min(other.reallocations),
            allocated_bytes: self.allocated_bytes.min(other.allocated_bytes),
        }
    }
}

/// Run `f` `reps` times; print the cheapest run of each count per source byte;
/// return the last result, so the work is not optimised away and later phases
/// can consume it. The result is dropped outside the measured region.
fn phase<T>(label: &str, reps: usize, bytes: usize, mut f: impl FnMut() -> T) -> T {
    let mut best: Option<Cost> = None;
    let mut last = None;
    for _ in 0..reps {
        drop(last.take());
        let (a0, r0, b0, i0) = (
            ALLOCATIONS.load(Relaxed),
            REALLOCATIONS.load(Relaxed),
            ALLOCATED_BYTES.load(Relaxed),
            instructions_retired(),
        );
        let start = Instant::now();
        let result = std::hint::black_box(f());
        let wall = start.elapsed();
        let i1 = instructions_retired();
        let cost = Cost {
            wall,
            instructions: i0.zip(i1).map(|(i0, i1)| i1 - i0),
            allocations: ALLOCATIONS.load(Relaxed) - a0,
            reallocations: REALLOCATIONS.load(Relaxed) - r0,
            allocated_bytes: ALLOCATED_BYTES.load(Relaxed) - b0,
        };
        best = Some(best.map_or(cost, |b| b.min(cost)));
        last = Some(result);
    }
    let cost = best.expect("reps > 0");
    let per_byte = |n: u64| n as f64 / bytes as f64;
    let instructions = cost
        .instructions
        .map_or_else(|| "n/a".to_string(), |i| format!("{:.1}", per_byte(i)));
    println!(
        "{label:<12} {instructions:>9} instr/B {:>7.3} allocs/B {:>7.3} reallocs/B {:>7.1} alloc-B/B {:>7.1} ns/B (wall)",
        per_byte(cost.allocations),
        per_byte(cost.reallocations),
        per_byte(cost.allocated_bytes),
        cost.wall.as_secs_f64() * 1e9 / bytes as f64,
    );
    last.expect("reps > 0")
}

fn main() {
    let corpus = PathBuf::from(std::env::var_os("BORZOI_CORPUS").expect("BORZOI_CORPUS unset"));
    let subdir = std::env::var("SEMA_COUNTS_SUBDIR").unwrap_or_else(|_| "src".into());
    let reps: usize = std::env::var("SEMA_COUNTS_REPS")
        .ok()
        .map(|s| s.parse().expect("SEMA_COUNTS_REPS is a number"))
        .unwrap_or(3);
    assert!(reps > 0, "SEMA_COUNTS_REPS must be positive");

    let files = fsharp_files(&corpus.join(subdir));
    let bytes: usize = files.iter().map(String::len).sum();
    let env = bcl_env();
    let symbols = HashSet::new();
    let impls: Vec<(usize, ImplFile, SyntaxRecovery)> = files
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let parse = parse_with_symbols(s, &symbols);
            (parse.root.kind() == SyntaxKind::IMPL_FILE).then(|| {
                let recovery = SyntaxRecovery::of(&parse);
                (
                    i,
                    ImplFile::cast(parse.root).expect("an IMPL_FILE"),
                    recovery,
                )
            })
        })
        .collect();
    println!(
        "{} .fs files ({} implementation files), {bytes} bytes, min of {reps}",
        files.len(),
        impls.len(),
    );

    let resolved = phase("resolve_file", reps, bytes, || {
        impls
            .iter()
            .map(|(_, file, rec)| resolve_file(file, &ProjectItems::default(), &env, rec))
            .collect::<Vec<_>>()
    });
    let inferred = phase("infer_file", reps, bytes, || {
        impls
            .iter()
            .zip(&resolved)
            .map(|((_, file, _), r)| infer_file(file, r, &env, &PrecedingTypes::default()))
            .collect::<Vec<_>>()
    });

    // The census: what interned names, paths and types would hold.
    let mut ident_tokens = 0usize;
    let mut spellings: HashSet<&str> = HashSet::new();
    // The tree is lossless, so a file's source is its tree's text, and a
    // range into one is a range into the other.
    let texts: Vec<&str> = impls.iter().map(|(i, _, _)| files[*i].as_str()).collect();
    for ((_, file, _), text) in impls.iter().zip(&texts) {
        for token in file
            .syntax()
            .tokens_where(|kind| kind == SyntaxKind::IDENT_TOK)
        {
            ident_tokens += 1;
            let range = token.text_range();
            spellings.insert(&text[usize::from(range.start())..usize::from(range.end())]);
        }
    }
    println!(
        "identifiers: {ident_tokens} tokens, {} distinct spellings",
        spellings.len()
    );

    let mut defs = 0usize;
    let mut def_names: HashSet<&str> = HashSet::new();
    let mut name_differs: BTreeMap<String, usize> = BTreeMap::new();
    for (r, text) in resolved.iter().zip(&texts) {
        for def in r.defs() {
            defs += 1;
            def_names.insert(&def.name);
            let spelled = text.get(usize::from(def.range.start())..usize::from(def.range.end()));
            if spelled != Some(def.name.as_str()) {
                *name_differs.entry(format!("{:?}", def.kind)).or_default() += 1;
            }
        }
    }
    let differs: usize = name_differs.values().sum();
    println!(
        "defs: {defs}, {} distinct names; name differs from its range's text for {differs}{}",
        def_names.len(),
        if differs == 0 {
            String::new()
        } else {
            format!(" ({name_differs:?})")
        }
    );

    let mut published = 0usize;
    let mut nodes = 0usize;
    let mut named_nodes = 0usize;
    let mut paths: HashSet<Vec<String>> = HashSet::new();
    for file in &inferred {
        for ty in file.types().values().chain(file.def_types().values()) {
            published += 1;
            nodes += ty.size().nodes;
            let mut stack = vec![ty];
            while let Some(ty) = stack.pop() {
                match ty {
                    Ty::Named { path, args } | Ty::Project { path, args } => {
                        named_nodes += 1;
                        paths.insert(path.clone());
                        stack.extend(args);
                    }
                    Ty::Array { elem, .. } => stack.push(elem),
                    Ty::Tuple(elems) => stack.extend(elems),
                    Ty::Fun { arg, ret } => {
                        stack.push(arg);
                        stack.push(ret);
                    }
                    Ty::Param(_) | Ty::Var(_) => {}
                }
            }
        }
    }
    println!(
        "published types: {published}, {nodes} nodes, {named_nodes} named nodes over {} distinct paths",
        paths.len()
    );
}

fn fsharp_files(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "fs") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}

/// `FSharp.Core` plus `System.Runtime` and the `netstandard` facade, the set
/// the sema tests' `full_bcl_env` uses.
fn bcl_env() -> AssemblyEnv {
    let core_path = std::env::var_os("SEMA_COUNTS_FSHARP_CORE")
        .expect("SEMA_COUNTS_FSHARP_CORE unset: point it at an FSharp.Core.dll");
    let packs = PathBuf::from(std::env::var_os("DOTNET_ROOT").expect("DOTNET_ROOT unset"))
        .join("packs")
        .join("Microsoft.NETCore.App.Ref");
    let ref_dir = std::fs::read_dir(&packs)
        .unwrap_or_else(|e| panic!("{}: {e}", packs.display()))
        .map(|e| e.expect("pack entry").path().join("ref").join("net10.0"))
        .find(|p| p.join("System.Runtime.dll").exists())
        .expect("no ref/net10.0/System.Runtime.dll in the reference pack");
    let read = |p: &Path| std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let images = [
        read(Path::new(&core_path)),
        read(&ref_dir.join("System.Runtime.dll")),
        read(&ref_dir.join("netstandard.dll")),
    ];
    let views: Vec<_> = images
        .iter()
        .map(|bytes| Ecma335Assembly::parse(bytes).expect("parse a BCL assembly"))
        .collect();
    AssemblyEnv::from_views(&views).expect("build the AssemblyEnv")
}
