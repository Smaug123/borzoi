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
//! `--nocapture`: that list is the modelling worklist, ordered by how often the
//! real SDK reaches each shape. It answers "what would I have to implement?" —
//! for "what is actually blocking this?", which is a different question with a
//! different answer, see `sdk_chain_decline_attribution.rs`.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use borzoi_msbuild::test_support::{Outcome, PropertyMap, evaluate};
use common::ExpandVerdict;
use common::sdk_chain::{
    extract_call_expressions, extract_conditions, msbuild_files, sdk_dir, seeded_props,
};
use common::{Oracle, check_expand_certain_implies_exact};

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
/// every one; the declined shapes are the modelling worklist.
#[test]
fn sdk_chain_property_expressions_are_never_wrongly_committed() {
    let sdk = sdk_dir();
    let files = msbuild_files(&sdk);

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
    report(
        "declined expression shapes (the modelling worklist)",
        &declined,
    );

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
    // capability regression even though it stays *sound*. The buckets printed
    // above say which functions to model, and that is where the coverage is:
    // 193 of the 330 declines still decline with every name they reference
    // defined, so no property table reaches them
    // (`sdk_chain_decline_attribution.rs`).
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
    let files = msbuild_files(&sdk);

    let mut conditions: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        if let Ok(text) = std::fs::read_to_string(file) {
            extract_conditions(&text, &mut conditions);
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
    // majority is operand-blocked, but on *ordinary* SDK-computed names
    // (`_TargetFrameworkVersionWithoutV`, `OutputType`, `Language`, …), which a
    // context-free census cannot have: 2 355 of the 2 619 withdrawals involve no
    // reserved name at all (`sdk_chain_decline_attribution.rs`).
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
///   is `Core`, and so on) while this census's table does not:
///   `'$(MSBuildRuntimeType)' == 'Core'` computes `False` here where MSBuild
///   says `True`. That divergence is real but *channelled*, not silent — and it
///   is largely an artefact of censusing context-free. A real walk that resolves
///   an SDK does seed `MSBuildRuntimeType`, and agrees with MSBuild on it; the
///   names a real walk still leaves empty are enumerated in
///   `docs/msbuild-reserved-seeding-plan.md`.
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
