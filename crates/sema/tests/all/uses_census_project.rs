//! Isolation-bias probe for the Phase-3 census (`#[ignore]`d).
//!
//! The corpus census ([`uses_census`](../uses_census.rs)) type-checks each file
//! *alone*, so cross-file member accesses on unresolved sibling types drop out
//! and the B2/B3 (needs-inference) share reads low. This probe quantifies that
//! bias: it takes a real, densely interconnected project — the FCS compiler
//! source itself, a prefix of its Compile order — and runs it **both ways** on
//! the *same files*: isolated (`uses-census-batch`) vs. one project
//! (`uses-census-project`, cross-file resolved). The rise in the member fraction
//! is the isolation bias for `src/`-style interconnected code.
//!
//! Generated sources (lexer / parser tables) are absent from the source corpus;
//! they drop equally in *both* passes, so the delta still isolates the
//! cross-file effect and the measured rise is a conservative lower bound.
//!
//! ```text
//! cargo test -p borzoi-sema --test all uses_census_project:: -- --ignored --nocapture
//! ```
//! Needs `BORZOI_CORPUS`. Tune the prefix with
//! `BORZOI_CENSUS_PROJECT_PREFIX` (default 250 files).

use crate::common;

use crate::common::{
    FileCensus, Tally, invoke_fcs_dump_census, invoke_fcs_dump_census_project, parse_census_jsonl,
};
use std::path::{Path, PathBuf};

/// Extract `<Compile Include="...">` paths from an FCS-style `.fsproj`, in
/// document order, resolved against the project directory and filtered to those
/// that exist (dropping generated sources referenced via MSBuild variables or
/// `obj/` paths). A prefix of the Compile order is itself a valid sub-project:
/// F# files may reference only earlier files, so the first `limit` form a
/// self-consistent unit.
fn compile_order(fsproj: &Path, limit: usize) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(fsproj).expect("read fsproj");
    let dir = fsproj.parent().expect("fsproj has a parent dir");
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((_, rest)) = line.split_once("<Compile Include=\"") else {
            continue;
        };
        let Some((raw, _)) = rest.split_once('"') else {
            continue;
        };
        let path = dir.join(raw.replace('\\', "/"));
        if path.is_file() {
            out.push(path);
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

fn tally_ok(files: &[FileCensus]) -> Tally {
    let mut t = Tally::default();
    for f in files.iter().filter(|f| f.ok) {
        t.add(f.uses.iter());
    }
    t
}

/// One bucket line for the side-by-side table.
fn row(label: &str, batch: u64, project: u64, b_den: u64, p_den: u64) {
    let pc = |n, d: u64| {
        if d == 0 {
            0.0
        } else {
            100.0 * n as f64 / d as f64
        }
    };
    println!(
        "  {label:<28} {batch:>8} ({:>4.1}%)   {project:>8} ({:>4.1}%)",
        pc(batch, b_den),
        pc(project, p_den)
    );
}

#[test]
#[ignore = "isolation-bias probe: needs BORZOI_CORPUS + a built fcs-dump"]
fn isolation_bias_probe() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!("BORZOI_CORPUS unset; skipping bias probe.");
        return;
    };
    let fsproj = PathBuf::from(&root).join("src/Compiler/FSharp.Compiler.Service.fsproj");
    if !fsproj.is_file() {
        eprintln!("FCS fsproj not at {fsproj:?}; skipping bias probe.");
        return;
    }
    let prefix = common::env_usize_or("BORZOI_CENSUS_PROJECT_PREFIX", 250);
    let files = compile_order(&fsproj, prefix);
    assert!(
        files.len() > 1,
        "expected a Compile-order prefix, got {files:?}"
    );
    eprintln!(
        "probe: {} FCS Compile-order files — checking isolated, then as one project…",
        files.len()
    );

    let batch = tally_ok(&parse_census_jsonl(&invoke_fcs_dump_census(&files)));
    let project = tally_ok(&parse_census_jsonl(&invoke_fcs_dump_census_project(&files)));

    let (bd, pd) = (batch.nondef(), project.nondef());
    println!(
        "\n=== ISOLATION-BIAS PROBE — FCS source, {} Compile-order files ===",
        files.len()
    );
    println!(
        "  {:<28} {:>15}   {:>15}",
        "", "isolated (batch)", "one project"
    );
    row("non-definition uses", bd, pd, bd, pd);
    row("B1 lexical", batch.buckets[0], project.buckets[0], bd, pd);
    row(
        "B2 shallow inference",
        batch.buckets[1],
        project.buckets[1],
        bd,
        pd,
    );
    row("B3 hard pile", batch.buckets[2], project.buckets[2], bd, pd);
    let (bm, pm) = (
        batch.buckets[1] + batch.buckets[2],
        project.buckets[1] + project.buckets[2],
    );
    row("needs inference (B2+B3)", bm, pm, bd, pd);
    println!(
        "\n  member (B2+B3) uses resolved: {bm} isolated -> {pm} in-project \
         ({:+.0}% — the isolation bias for interconnected code)",
        if bm == 0 {
            0.0
        } else {
            100.0 * (pm as f64 - bm as f64) / bm as f64
        }
    );

    assert!(pd > 0, "project pass observed no uses");
}
