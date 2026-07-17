//! Diagnostic: sweep a directory of `.dll`s and, for each, run
//! `enumerate_type_defs_with_skips` — reporting the kept-type count alongside
//! the members and whole-types the projector had to drop. It is the hands-on
//! counterpart to the per-member "bound uncertainty" tests: point it at a real
//! shared-runtime or ref-pack directory to watch a modern BCL project nearly in
//! full, with the residual coverage gaps (function pointers, `modreq`,
//! `allows ref struct`, …) tallied rather than sinking whole assemblies.
//!
//! Not a test — a standalone harness. Usage:
//! `cargo run -p borzoi-assembly --example enum_sweep -- <dir> [<dir> ...]`

use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity};
use std::collections::BTreeMap;

/// Collect each dropped member's reason, recursing into nested types.
fn collect_reasons(entities: &[Entity], out: &mut Vec<String>) {
    for e in entities {
        for s in &e.skipped_members {
            out.push(s.reason.clone());
        }
        collect_reasons(&e.nested_types, out);
    }
}

/// Count kept types, recursing into nested types.
fn count_types(entities: &[Entity]) -> usize {
    entities
        .iter()
        .map(|e| 1 + count_types(&e.nested_types))
        .sum()
}

/// Count dropped members across the kept type tree.
fn count_member_drops(entities: &[Entity]) -> usize {
    entities
        .iter()
        .map(|e| e.skipped_members.len() + count_member_drops(&e.nested_types))
        .sum()
}

fn main() {
    let dirs: Vec<String> = std::env::args().skip(1).collect();
    let mut ok = 0usize;
    let mut parse_err = 0usize;
    let mut enum_err = 0usize;
    let mut total_types = 0usize;
    let mut total_member_drops = 0usize;
    let mut total_type_drops = 0usize;
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();

    for dir in &dirs {
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read_dir {dir}: {e}"))
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "dll").unwrap_or(false))
            .collect();
        paths.sort();
        for p in paths {
            let bytes = std::fs::read(&p).unwrap();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            match Ecma335Assembly::parse(&bytes) {
                Err(e) => {
                    parse_err += 1;
                    println!("PARSE-ERR  {name}: {e}");
                }
                Ok(view) => match view.enumerate_type_defs_with_skips() {
                    // A whole-image / structural failure (bad F# pickle, a
                    // nesting cycle) is still fatal — everything else is now a
                    // localized drop.
                    Err(e) => {
                        enum_err += 1;
                        println!("ENUM-ERR   {name}: {e}");
                    }
                    Ok((entities, skips)) => {
                        ok += 1;
                        let n = count_types(&entities);
                        let md = count_member_drops(&entities);
                        let td = skips.dropped_types.len();
                        total_types += n;
                        total_member_drops += md;
                        total_type_drops += td;
                        let mut rs = Vec::new();
                        collect_reasons(&entities, &mut rs);
                        for r in rs {
                            *reasons.entry(r).or_default() += 1;
                        }
                        for t in &skips.dropped_types {
                            *reasons.entry(format!("[type] {}", t.reason)).or_default() += 1;
                        }
                        println!("OK         {name}: {n} types (dropped {md} members, {td} types)");
                    }
                },
            }
        }
    }
    println!("\n=== drop reasons ===");
    let mut by_count: Vec<_> = reasons.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1));
    for (reason, count) in by_count {
        println!("{count:6}  {reason}");
    }
    println!("\n=== summary ===");
    println!(
        "ok={ok} parse_err={parse_err} enum_err={enum_err} \
         total_types={total_types} member_drops={total_member_drops} \
         type_drops={total_type_drops}"
    );
}
