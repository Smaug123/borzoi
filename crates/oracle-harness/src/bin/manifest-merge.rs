//! The Git merge driver for exact manifests: `manifest-merge BASE OURS THEIRS
//! [PATH]`, Git's `%O %A %B %P`. Merges the three as keyed line sets
//! ([`borzoi_oracle_harness::manifest::merge_files`]) and writes the result
//! over `OURS`, as Git expects.
//!
//! Exits 0 on a clean merge. When both sides changed one key differently it
//! writes the merge with those keys between conflict markers and exits 1, and
//! when an input is not a keyed manifest — one from before the manifest was
//! keyed, say — it leaves `OURS` alone and exits 2; Git reports either as a
//! conflict. Run through `tools/manifest-merge.sh`, which
//! `.gitattributes` and AGENTS.md describe.

use std::process::ExitCode;

use borzoi_oracle_harness::manifest::{MergeFailure, merge_files};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [base, ours, theirs, rest @ ..] = args.as_slice() else {
        eprintln!("usage: manifest-merge BASE OURS THEIRS [PATH]");
        return ExitCode::from(2);
    };
    let path = rest.first().map_or(ours.as_str(), String::as_str);
    let read = |p: &str| {
        std::fs::read_to_string(p).unwrap_or_else(|e| panic!("manifest-merge: read {p}: {e}"))
    };
    match merge_files(&read(base), &read(ours), &read(theirs)) {
        Ok(merged) => {
            write(ours, &merged);
            ExitCode::SUCCESS
        }
        Err(MergeFailure::Conflicts {
            header,
            keys,
            marked,
        }) => {
            write(ours, &marked);
            eprintln!(
                "manifest-merge: {path}: both sides changed {} key(s) differently{}; \
                 regenerate the manifest, or resolve the marked entries:",
                keys.len(),
                if header { ", and the header" } else { "" }
            );
            for c in &keys {
                eprintln!("  {}", c.key);
            }
            ExitCode::from(1)
        }
        Err(MergeFailure::Unkeyed { side, entry }) => {
            eprintln!(
                "manifest-merge: {path}: the {side:?} side has an unkeyed entry {entry:?} — \
                 a branch from before the manifest was keyed? Left unmerged: regenerate it"
            );
            ExitCode::from(2)
        }
        Err(MergeFailure::Malformed { side, line, error }) => {
            eprintln!(
                "manifest-merge: {path}: the {side:?} side is not a manifest (line {line}: \
                 {error}); left unmerged"
            );
            ExitCode::from(2)
        }
    }
}

fn write(path: &str, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|e| panic!("manifest-merge: write {path}: {e}"));
}
