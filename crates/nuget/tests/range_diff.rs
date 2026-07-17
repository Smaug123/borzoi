//! Differential tests: `VersionRange` vs the real `NuGet.Versioning` via
//! `tools/nuget-oracle` — parse acceptance + every parsed field, then
//! `Satisfies` over (range, version) pairs including exact-endpoint probes.
//! Same fixed-seed determinism rationale as `version_diff.rs`.

mod common;

use borzoi_nuget::{NuGetVersion, VersionRange};
use common::{Oracle, SplitMix64, gen_range_string, gen_version_string};

/// Hand-picked corners: bracket structure, degenerate intervals, floats in
/// every position (legal and not), whitespace, metadata in bounds.
const CORNERS: &[&str] = &[
    "",
    "1.0.0",
    "1.0",
    "1",
    "1.0.0-beta",
    "1.0.0+meta",
    "[1.0]",
    "[1.0.0]",
    "(1.0)",
    "[1.0)",
    "(1.0]",
    "( 1.0 )",
    "[1.0,2.0]",
    "(1.0,2.0)",
    "[1.0, 2.0)",
    "(1.0, 2.0]",
    "[ 1.0 , 2.0 ]",
    "[1.0 ,2.0]",
    "[1.0,\t2.0]",
    " [1.0, 2.0] ",
    "[,2.0]",
    "(,2.0)",
    "(,2.0]",
    "[1.0,]",
    "[1.0,)",
    "(1.0,)",
    "(,)",
    "[,]",
    "[,)",
    "(,]",
    "[]",
    "()",
    "[ ]",
    "[2.0,1.0]",
    "(1.0,1.0)",
    "[1.0,1.0)",
    "(1.0,1.0]",
    "[1.0,1.0]",
    "[1.0,2.0,3.0]",
    "[1.0-beta,2.0-rc]",
    "[1.0+m,2.0+n]",
    "*",
    "*-*",
    " * ",
    "1.*",
    "1.2.*",
    "1.2.3.*",
    "1.2.3.4.*",
    "01.*",
    "1.0.0-*",
    "1.0.0-beta*",
    "1.0.0-beta.*",
    "1.0.0-beta.rc*",
    "1.0.0-BETA*",
    "1.0.0-*beta",
    "1.0.0-be*ta",
    "1.*-*",
    "1.*-beta*",
    "1.2.*-*",
    "1.2.3.*-rc*",
    "*-beta*",
    "*.*",
    "*.1",
    "1.*.2",
    "1.**",
    "*1",
    "1.0.*+meta",
    "[1.0.*]",
    "[1.0.*,)",
    "[1.0.*, 2.0)",
    "(1.0.*, 2.0)",
    "[*, 2.0)",
    "[*-*, )",
    "[1.0, 2.0.*]",
    "[1.0, *]",
    "[1.0.0-*, 2.0]",
    "( )",
    "[ 1.0 ]",
    "1*",
    "0*",
    "00*",
    "[1*]",
    "[0*]",
    "[0*, 2.0)",
    "[ 1*, 2.0)",
    "[1*, 2.0]",
    "[1*, )",
    "[12*,]",
    "(1*,10)",
    "(1*, 20.0]",
    "[ 1*, )",
    "[\t*, 2.0)",
    "1*-beta*",
    "[1*-beta*, )",
    "[0*-beta*, )",
    "1.9*",
    "1.2.9*",
    "01.0*",
    "10+*",
    "1.+2.*",
    "1. 2.*",
    " *-* ",
    "( *-*, )",
    "[*]",
    "[ * ]",
    "[*-*]",
    "1..*",
    "1.-0*",
    "1.0.0+m-*",
    "1.0.0-beta.-*",
    "[ 1.0.*, 2.0)",
    "[1.0.* , 2.0)",
    "[ * , 2.0)",
    "[1.5.*, 1.0]",
    "[1.0.*, 1.0.0]",
    "[1.0.*, 1.0.0)",
    "[2.0,1.0)",
    "1.2.3.4-*",
    "1.2.3.0-*",
    "1.0,2.0",
    "1.0 2.0",
    "[banana]",
    "banana",
    "-*",
    "[-*]",
    "[",
    "]",
    ",",
    "[,",
];

fn oracle_str(v: &serde_json::Value, field: &str) -> String {
    v.get(field)
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("oracle response missing string field {field}: {v}"))
        .to_owned()
}

fn oracle_bool(v: &serde_json::Value, field: &str) -> bool {
    v.get(field)
        .and_then(|x| x.as_bool())
        .unwrap_or_else(|| panic!("oracle response missing bool field {field}: {v}"))
}

#[test]
fn range_parse_and_satisfies_agree_with_oracle() {
    let mut inputs: Vec<String> = CORNERS.iter().map(|s| s.to_string()).collect();
    let mut rng = SplitMix64(0x5eed_0002);
    for _ in 0..5000 {
        inputs.push(gen_range_string(&mut rng));
    }

    let mut oracle = Oracle::spawn();
    let mut mismatches: Vec<String> = Vec::new();
    let mut parsed: Vec<(String, VersionRange)> = Vec::new();

    for input in &inputs {
        let ours = VersionRange::parse(input);
        let resp = oracle.request(&serde_json::json!({
            "op": "parseRange",
            "input": input,
        }));
        let oracle_ok = oracle_bool(&resp, "ok");

        match (&ours, oracle_ok) {
            (Ok(range), true) => {
                let checks: &[(&str, String)] = &[
                    ("normalized", range.to_normalized_string()),
                    (
                        "minVersion",
                        range
                            .min_version()
                            .map(|v| v.to_full_string())
                            .unwrap_or_default(),
                    ),
                    (
                        "maxVersion",
                        range
                            .max_version()
                            .map(|v| v.to_full_string())
                            .unwrap_or_default(),
                    ),
                    (
                        "floatBehavior",
                        range
                            .float_behavior()
                            .map(|b| b.to_string())
                            .unwrap_or_else(|| "None".to_owned()),
                    ),
                ];
                for (field, mine) in checks {
                    let theirs = oracle_str(&resp, field);
                    if *mine != theirs {
                        mismatches.push(format!(
                            "{input:?}: {field} ours={mine:?} oracle={theirs:?}"
                        ));
                    }
                }
                let flags: &[(&str, bool)] = &[
                    ("hasLowerBound", range.has_lower_bound()),
                    ("isMinInclusive", range.is_min_inclusive()),
                    ("hasUpperBound", range.has_upper_bound()),
                    ("isMaxInclusive", range.is_max_inclusive()),
                    ("isFloating", range.is_floating()),
                ];
                for (field, mine) in flags {
                    let theirs = oracle_bool(&resp, field);
                    if *mine != theirs {
                        mismatches.push(format!("{input:?}: {field} ours={mine} oracle={theirs}"));
                    }
                }
                parsed.push((input.clone(), range.clone()));
            }
            (Err(_), false) => {} // agree: unparseable
            (Ok(range), false) => {
                mismatches.push(format!(
                    "{input:?}: we parsed {} but NuGet rejects",
                    range.to_normalized_string()
                ));
            }
            (Err(e), true) => {
                mismatches.push(format!(
                    "{input:?}: NuGet parses (normalized {:?}) but we reject: {e}",
                    oracle_str(&resp, "normalized")
                ));
            }
        }
    }

    // Satisfies phase: each mutually-parsed range probed with (a) generated
    // versions and (b) its own bounds — the exact inclusivity edges.
    let n = parsed.len();
    assert!(n > 800, "generator degenerated: only {n} parseable ranges");
    let mut version_rng = SplitMix64(0x5eed_0003);
    let mut checked = 0usize;
    for (range_str, range) in &parsed {
        let mut probes: Vec<String> = Vec::new();
        for _ in 0..2 {
            probes.push(gen_version_string(&mut version_rng));
        }
        if let Some(min) = range.min_version() {
            probes.push(min.to_full_string());
            // One tick either side of the boundary in prerelease space.
            probes.push(format!("{}-0", min.to_normalized_string()));
        }
        if let Some(max) = range.max_version() {
            probes.push(max.to_full_string());
            probes.push(format!("{}-0", max.to_normalized_string()));
        }
        for probe in probes {
            let Ok(version) = NuGetVersion::parse(&probe) else {
                continue; // e.g. "-0" appended to a prerelease bound
            };
            let resp = oracle.request(&serde_json::json!({
                "op": "rangeSatisfies",
                "range": range_str,
                "version": probe,
            }));
            if !oracle_bool(&resp, "ok") {
                continue; // oracle rejected the probe version (parity checked in version_diff)
            }
            checked += 1;
            let theirs = oracle_bool(&resp, "satisfies");
            let mine = range.satisfies(&version);
            if mine != theirs {
                mismatches.push(format!(
                    "satisfies({range_str:?}, {probe:?}): ours={mine} oracle={theirs}"
                ));
            }
        }
    }
    assert!(checked > 2000, "satisfies phase degenerated: {checked}");

    if !mismatches.is_empty() {
        let shown = mismatches.iter().take(25).cloned().collect::<Vec<_>>();
        panic!(
            "{} divergence(s) from NuGet.Versioning ranges; first {}:\n{}",
            mismatches.len(),
            shown.len(),
            shown.join("\n")
        );
    }
}
