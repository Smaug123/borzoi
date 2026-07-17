//! Census: every property expression and `Condition` the *real* SDK spells,
//! run through our evaluator against the MSBuild oracle.
//!
//! The generative sweeps in `property_expr_diff.rs` draw from a grammar *we*
//! wrote, so they can only find bugs in shapes we already imagined — three of
//! the five findings in the C.1 review rounds (percent escapes, backtick
//! string literals, invariant-uppercase platform names) were input-language
//! facts our generators could not spell. This test removes the guesswork: the
//! inputs are extracted from the pinned SDK's own `.props`/`.targets`, so the
//! corpus is exactly the surface the evaluator must survive in production.
//!
//! Two assertions, both machine-checked:
//!
//! 1. **Certain-implies-exact** (the soundness gate, same contract as the
//!    other differentials): whenever our evaluator *commits* to an expansion
//!    or a boolean, MSBuild must agree exactly. A decline makes no claim.
//!    This is what catches a wrong-commit on a shape nobody thought to probe.
//! 2. **Coverage ratchets** (the completeness gate): the committed fraction
//!    must not regress. `docs/completed/sdk-chain-exactness-plan.md`'s acceptance
//!    criterion — "the chain evaluates exactly" — becomes a number here
//!    rather than a claim a reviewer has to re-derive by hand.
//!
//! The declined shapes are printed (bucketed by function name) with
//! `--nocapture`: that list *is* the C.2+ worklist, ordered by how often the
//! real SDK reaches each shape.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use borzoi_msbuild::test_support::{Outcome, PropertyMap, evaluate};
use common::ExpandVerdict;
use common::{Oracle, check_expand_certain_implies_exact};

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
/// undefined and declines — sound, just less covered. Seeding them for real is
/// exactly what Stage C.2's trusted seeding is for; when it lands, they move
/// here and the ratchets below go up.
fn seeded_props() -> Vec<(String, String)> {
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
fn sdk_dir() -> PathBuf {
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
fn extract_call_expressions(text: &str, out: &mut BTreeSet<String>) {
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

/// Bucket a declined shape by what it *calls*, so the printed worklist is
/// ordered by the thing that would need modelling, not by call site.
fn decline_bucket(expr: &str) -> String {
    if let Some(rest) = expr.split_once("::") {
        let name: String = rest
            .1
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let ty: String = rest
            .0
            .rsplit('[')
            .next()
            .unwrap_or("")
            .trim_end_matches(']')
            .to_string();
        return format!("[{ty}]::{name}");
    }
    // Instance member: `$(Recv.Member(...))` → `.Member`
    let inner = expr.trim_start_matches("$(").trim_end_matches(')');
    match inner.split_once('.') {
        Some((_, rest)) => {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            format!(".{name}")
        }
        None => "<other>".to_string(),
    }
}

fn report(title: &str, buckets: &BTreeMap<String, usize>) {
    let mut rows: Vec<(&String, &usize)> = buckets.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    eprintln!("--- {title} ---");
    for (bucket, count) in rows {
        eprintln!("  {count:5}  {bucket}");
    }
}

/// Every property-function expression the pinned SDK's import chain spells,
/// evaluated against the real MSBuild evaluator under two property contexts
/// (empty, and a seeded mid-chain table). Certain-implies-exact must hold for
/// every one; the declined shapes are the C.2+ worklist.
#[test]
fn sdk_chain_property_expressions_are_never_wrongly_committed() {
    let sdk = sdk_dir();
    let mut files = Vec::new();
    walk_msbuild_files(&sdk, &mut files);
    assert!(
        files.len() > 100,
        "expected the SDK's props/targets chain under {}, found {} files",
        sdk.display(),
        files.len()
    );

    let mut expressions: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        if let Ok(text) = std::fs::read_to_string(file) {
            extract_call_expressions(&text, &mut expressions);
        }
    }
    assert!(
        expressions.len() > 100,
        "extracted only {} call expressions from {} SDK files — the extractor \
         is probably broken, and a vacuous census would assert nothing",
        expressions.len(),
        files.len()
    );

    let mut oracle = Oracle::spawn();
    let seeded = seeded_props();
    let empty: Vec<(String, String)> = Vec::new();

    let mut exact = 0usize;
    let mut declined: BTreeMap<String, usize> = BTreeMap::new();
    for expr in &expressions {
        // Both contexts must be sound; an expression counts as *covered* if it
        // commits under either (a defined receiver is the realistic case).
        let mut committed = false;
        for props in [&empty, &seeded] {
            match check_expand_certain_implies_exact(&mut oracle, expr, props) {
                ExpandVerdict::Exact => committed = true,
                ExpandVerdict::Partial => {}
            }
        }
        if committed {
            exact += 1;
        } else {
            *declined.entry(decline_bucket(expr)).or_default() += 1;
        }
    }

    let total = expressions.len();
    eprintln!(
        "SDK chain ({}): {total} distinct call expressions, {exact} committed, {} declined",
        sdk.display(),
        total - exact
    );
    report("declined expression shapes (the C.2+ worklist)", &declined);

    // Coverage ratchet, baselined at what C.1 actually reaches (28/396 as of
    // 2026-07-11), raised to 61/396 when the `[MSBuild]::Version*` comparison
    // family landed (2026-07-13), then to 65/396 on unix when the path-fixup
    // keystone let `[System.IO.Path]::Combine` commit backslash-bearing parts
    // (`docs/msbuild-unix-path-fixup-plan.md` P3). The keystone gain is unix-only
    // — the fixup is inert on Windows — so the floor there stays 61. Then
    // `[System.String]::IsNullOrEmpty` landed (Stage C keystone, 2026-07-14),
    // committing one more distinct call expression on *both* platforms (its
    // string logic carries no `cfg!(windows)` divergence), so 61→62 / 65→66.
    // Raise it as stages land; never lower it without saying why — a drop means
    // the evaluator started declining something it used to model, which is a
    // capability regression even though it stays *sound*. Most remaining declines
    // are undefined *reserved* receivers, which Stage C.2's trusted seeding turns
    // on wholesale (e.g. the residual `Version*` declines all have an undefined
    // `$(TargetPlatform*)` arg); the printed buckets say which functions to model.
    let floor = if cfg!(windows) { 62 } else { 66 };
    assert!(
        exact >= floor,
        "SDK-chain expression coverage regressed: only {exact} of {total} \
         committed (floor {floor})"
    );
}

/// Every `Condition` the pinned SDK's import chain spells, against the real
/// evaluator. Same contract: a committed boolean must be MSBuild's boolean.
#[test]
fn sdk_chain_conditions_are_never_wrongly_committed() {
    let sdk = sdk_dir();
    let mut files = Vec::new();
    walk_msbuild_files(&sdk, &mut files);

    let mut conditions: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        // Parse rather than regex the attribute: the XML layer unescapes
        // `&gt;`/`&amp;` before MSBuild ever sees the condition text, so a raw
        // scrape would census a string the evaluator is never handed.
        let Ok(doc) = roxmltree::Document::parse(&text) else {
            continue;
        };
        for node in doc.descendants() {
            if let Some(cond) = node.attribute("Condition") {
                // Item/metadata operands are a separate language (plan D1).
                if !cond.contains("@(") && !cond.contains("%(") {
                    conditions.insert(cond.to_string());
                }
            }
        }
    }
    assert!(
        conditions.len() > 100,
        "extracted only {} conditions from {} SDK files",
        conditions.len(),
        files.len()
    );

    let mut oracle = Oracle::spawn();
    let seeded = seeded_props();
    let empty: Vec<(String, String)> = Vec::new();

    let mut committed = 0usize;
    let mut withdrawn = 0usize;
    for cond in &conditions {
        let mut any = false;
        for props in [&empty, &seeded] {
            if check_condition_claim(&mut oracle, cond, props) {
                any = true;
            }
        }
        if any {
            committed += 1;
        } else {
            withdrawn += 1;
        }
    }

    let total = conditions.len();
    eprintln!(
        "SDK chain ({}): {total} distinct conditions, {committed} committed, \
         {withdrawn} withdrawn (unsupported or undefined-bearing)",
        sdk.display()
    );

    // Same ratchet rationale as the expression census; baselined at 136/2758
    // (2026-07-11), raised to 139 on unix when the path-fixup keystone let
    // `[System.IO.Path]::IsPathRooted` commit non-leading backslash conditions
    // (`docs/msbuild-unix-path-fixup-plan.md` P3). Unix-only gain (the Windows
    // `is_path_rooted` declines), so the floor there stays 130. The withdrawn
    // majority is dominated by undefined reserved receivers — again Stage C.2's
    // seeding, after which this floor jumps.
    let floor = if cfg!(windows) { 130 } else { 139 };
    assert!(
        committed >= floor,
        "SDK-chain condition coverage regressed: only {committed} of {total} \
         committed (floor {floor})"
    );
}

/// The *walker's* condition contract, which is what production actually
/// consumes — and is weaker than `condition_diff.rs`'s, deliberately:
///
/// - `Outcome::Unsupported` makes no claim (fail-safe channel).
/// - A committed boolean that **relied on an undefined reference** also makes
///   no claim: the walker emits an `UndefinedProperty` diagnostic for exactly
///   those names and consumers degrade on it ("MSBuild may have the value, we
///   don't" — `evaluator.rs`). This matters on the SDK chain specifically,
///   because MSBuild *always* defines the reserved names (`MSBuildRuntimeType`
///   is `Core`, and so on) while our table does not: `'$(MSBuildRuntimeType)'
///   == 'Core'` computes `False` on an unseeded table where MSBuild says
///   `True`. That divergence is real but *channelled*, not silent — and
///   closing it for good is precisely Stage C.2's trusted seeding.
/// - Any *other* committed boolean must be MSBuild's boolean, exactly.
///
/// Returns whether we committed a checked claim (for the coverage ratchet).
fn check_condition_claim(oracle: &mut Oracle, cond: &str, props: &[(String, String)]) -> bool {
    let mut map = PropertyMap::new();
    for (k, v) in props {
        map.insert(k.clone(), v.clone());
    }
    let eval = evaluate(cond, &map);
    let ours = match eval.outcome {
        Outcome::Unsupported => return false,
        Outcome::True => true,
        Outcome::False => false,
    };
    if !eval.undefined_properties.is_empty() {
        return false;
    }
    match oracle.eval(cond, props) {
        Some(theirs) => assert_eq!(
            ours, theirs,
            "SDK-chain condition certain-implies-exact violated: we say {ours} for \
             {cond:?} with props {props:?}, but MSBuild says {theirs}"
        ),
        None => panic!(
            "SDK-chain condition certain-implies-exact violated: we commit {ours} for \
             {cond:?} with props {props:?}, but MSBuild rejects it as illegal"
        ),
    }
    true
}
