//! The pinned SDK's own `.props`/`.targets` chain, as an input corpus.
//!
//! Shared by the census (which asks whether every committed value is exact) and
//! the decline attribution (which asks why the rest were not committed). They
//! must draw from *identical* populations or neither number can be compared to
//! the other, so the extraction lives here rather than in either of them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A property table standing in for a mid-evaluation SDK chain. Both sides see
/// exactly these values, so the comparison stays apples-to-apples; the point of
/// seeding is that a *defined* receiver lets far more expressions reduce, which
/// is where wrong-commits can happen at all (an undefined reference is already
/// an `Issue`, hence a decline).
///
/// **Reserved names are deliberately absent** (`MSBuildProjectDirectory`,
/// `MSBuildThisFileDirectory`, …): MSBuild refuses to have them injected
/// ("property is reserved, and cannot be modified"), so the oracle cannot be
/// put in a state where both sides agree on their value. Our side sees them
/// undefined and declines — sound, just less covered. How much less is
/// measured, not assumed: `sdk_chain_decline_attribution.rs` seeds every
/// reserved name and counts what that turns on — +12 expressions and +24
/// conditions, itself an over-statement since a real walk already supplies
/// several of the names this table lacks.
pub fn seeded_props() -> Vec<(String, String)> {
    [
        ("TargetFramework", "net10.0"),
        ("TargetFrameworks", "net10.0;net9.0"),
        ("TargetFrameworkIdentifier", ".NETCoreApp"),
        ("TargetFrameworkVersion", "v10.0"),
        ("Configuration", "Debug"),
        ("Platform", "AnyCPU"),
        ("BaseIntermediateOutputPath", "obj/"),
        ("MSBuildProjectExtensionsPath", "/repo/proj/obj/"),
        ("OutputPath", "bin/Debug/net10.0/"),
        ("NetCoreRoot", "/usr/share/dotnet/"),
        ("BundledNETCoreAppPackageVersion", "10.0.3"),
        ("RuntimeIdentifier", "osx-arm64"),
        ("LangVersion", "latest"),
        ("AssemblyName", "Demo"),
        ("Version", "1.2.3"),
        ("VersionPrefix", "1.2.3"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// Locate the pinned SDK's import chain: `$DOTNET_ROOT/sdk/<version>/`.
/// The devshell pins exactly one version, which is the whole point — the
/// census is against *the* SDK the rest of the crate claims exactness for.
pub fn sdk_dir() -> PathBuf {
    let root = std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .expect("DOTNET_ROOT is not set; run under nix develop");
    let sdk = root.join("sdk");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&sdk)
        .unwrap_or_else(|e| panic!("read {}: {e}", sdk.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    versions.sort();
    versions
        .pop()
        .unwrap_or_else(|| panic!("no SDK under {}", sdk.display()))
}

fn walk_msbuild_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        // `file_type` (not `metadata`) so a symlinked directory isn't
        // followed into a cycle.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk_msbuild_files(&path, out);
        } else if kind.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "props" | "targets") {
                out.push(path);
            }
        }
    }
}

/// The extent of a `$(…)` starting at `text[start]` (which must be `$`).
/// Quote-aware over all three MSBuild string delimiters and nesting-aware over
/// inner `$(…)`, mirroring the evaluator's own scanner — an expression the
/// scanner can't close is not extracted (there is nothing to evaluate).
fn dollar_extent(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 2;
    let mut delim: Option<u8> = None;
    let mut depth = 1usize;
    while i < bytes.len() {
        let b = bytes[i];
        match delim {
            Some(d) => {
                if b == d {
                    delim = None;
                }
            }
            None => match b {
                b'\'' | b'`' | b'"' => delim = Some(b),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Every top-level `$(…)` in `text` that *calls something* — a property
/// function (`::`) or an instance member (`.Foo(`). A bare `$(Name)` reference
/// has no evaluator surface worth censusing (it is a map lookup).
pub fn extract_call_expressions(text: &str, out: &mut BTreeSet<String>) {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'$'
            && bytes[i + 1] == b'('
            && let Some(close) = dollar_extent(bytes, i)
        {
            let whole = &text[i..=close];
            let inner = &text[i + 2..close];
            // Item-language operands (`@(…)`, `%(…)`) are a different,
            // item-typed language `substitute` passes through untouched;
            // out of scope for the property differential (plan D1).
            let interesting = inner.contains("::") || inner.contains('.');
            if interesting && !whole.contains("@(") && !whole.contains("%(") {
                out.insert(whole.to_string());
            }
            i = close + 1;
            continue;
        }
        i += 1;
    }
}

/// Every `.props`/`.targets` file under `dir`, recursively.
pub fn msbuild_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_msbuild_files(dir, &mut out);
    assert!(
        out.len() > 100,
        "expected the SDK's props/targets chain under {}, found {}",
        dir.display(),
        out.len()
    );
    out
}

/// Every `Condition` attribute in `text`, excluding the item-language operands
/// (`@(…)`, `%(…)`) that are a separate language (plan D1).
///
/// Parsed rather than scraped: the XML layer unescapes `&gt;`/`&amp;` before
/// MSBuild ever sees the condition text, so a raw scan would census a string
/// the evaluator is never handed.
pub fn extract_conditions(text: &str, out: &mut BTreeSet<String>) {
    let Ok(doc) = roxmltree::Document::parse(text) else {
        return;
    };
    for node in doc.descendants() {
        if let Some(cond) = node.attribute("Condition")
            && !cond.contains("@(")
            && !cond.contains("%(")
        {
            out.insert(cond.to_string());
        }
    }
}
