//! Differential tests: `NuGetVersion` vs the real `NuGet.Versioning` via
//! `tools/nuget-oracle`. The oracle is the authority on every "what does
//! NuGet actually do here" corner (whitespace, leading zeros, unicode
//! digits, Int32 overflow, …) — when this test fails, the Rust side is
//! wrong by definition.
//!
//! Inputs are deterministic (fixed-seed SplitMix64, plus a hand-written
//! corner list), so a failure reproduces exactly and the oracle batch stays
//! stable run-to-run; the *random-input* exploration lives in the proptest
//! file, which doesn't need cross-process reproducibility.

mod common;

use borzoi_nuget::NuGetVersion;
use common::{Oracle, SplitMix64, gen_version_string};

/// Hand-picked corners: every historically-fiddly shape gets a guaranteed
/// seat regardless of what the generator happens to produce.
const CORNERS: &[&str] = &[
    "",
    "1",
    "1.2",
    "1.2.3",
    "1.2.3.4",
    "1.2.3.0",
    "0.0.0",
    "01.002.0003",
    "2147483647",
    "2147483647.2147483647.2147483647.2147483647",
    "2147483648",
    "1.2147483648.0",
    "4294967296.0.0",
    "9999999999999999999999.0.0",
    "1.0.0-beta",
    "1.0.0-beta.11",
    "1.0.0-beta.011",
    "1.0.0-BETA",
    "1.0.0-0",
    "1.0.0-00",
    "1.0.0-0a",
    "1.0.0-a-b-c",
    "1.0.0-a.b.c.d.e",
    "1.0.0-2147483648",
    "1.0.0--1",
    "1.0.0--0",
    "1.0.0--2147483648",
    "1.0.0--2147483649",
    "1.0.0-01",
    "1.0.0-1",
    "1.0.0+meta",
    "1.0.0+meta.2",
    "1.0.0+meta..x",
    "1.0.0-rc.1+build.5",
    "1.0.0+build-with-dash",
    "1.0.0+build+again",
    "1.0.0-",
    "1.0.0+",
    "1.0.0-+",
    "-1.0.0",
    "+1.0.0",
    "1..0",
    ".1.0",
    "1.0.",
    "1.0.0.0.0",
    " 1.0.0",
    "1.0.0 ",
    "1. 0.0",
    "1 .0.0",
    "1.0.0\t",
    "\u{00a0}1.0.0",
    "1.0.0-be ta",
    "1.0.0-βeta",
    "١.٠.٠",
    "1,0",
    "v1.0.0",
    "1.0.0-be_ta",
    "1.-0.0",
    "1.+0.0",
    "banana",
    ".",
    "..",
    "-",
    "+",
];

fn oracle_str(v: &serde_json::Value, field: &str) -> String {
    v.get(field)
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("oracle response missing string field {field}: {v}"))
        .to_owned()
}

fn oracle_u64(v: &serde_json::Value, field: &str) -> u64 {
    v.get(field)
        .and_then(|x| x.as_u64())
        .unwrap_or_else(|| panic!("oracle response missing numeric field {field}: {v}"))
}

fn oracle_bool(v: &serde_json::Value, field: &str) -> bool {
    v.get(field)
        .and_then(|x| x.as_bool())
        .unwrap_or_else(|| panic!("oracle response missing bool field {field}: {v}"))
}

#[test]
fn parse_and_compare_agree_with_oracle() {
    let mut inputs: Vec<String> = CORNERS.iter().map(|s| s.to_string()).collect();
    let mut rng = SplitMix64(0x5eed_0001);
    for _ in 0..6000 {
        inputs.push(gen_version_string(&mut rng));
    }

    let mut oracle = Oracle::spawn();
    let mut mismatches: Vec<String> = Vec::new();
    // Inputs both sides parsed, for the comparison phase.
    let mut parsed: Vec<(String, NuGetVersion)> = Vec::new();

    for input in &inputs {
        let ours = NuGetVersion::parse(input);
        let resp = oracle.request(&serde_json::json!({
            "op": "parseVersion",
            "input": input,
        }));
        let oracle_ok = oracle_bool(&resp, "ok");

        match (&ours, oracle_ok) {
            (Ok(v), true) => {
                let fields: &[(&str, String)] = &[
                    ("normalized", v.to_normalized_string()),
                    ("full", v.to_full_string()),
                    ("metadata", v.metadata().unwrap_or("").to_owned()),
                ];
                for (field, mine) in fields {
                    let theirs = oracle_str(&resp, field);
                    if *mine != theirs {
                        mismatches.push(format!(
                            "{input:?}: {field} ours={mine:?} oracle={theirs:?}"
                        ));
                    }
                }
                let nums: &[(&str, u64)] = &[
                    ("major", v.major() as u64),
                    ("minor", v.minor() as u64),
                    ("patch", v.patch() as u64),
                    ("revision", v.revision() as u64),
                ];
                for (field, mine) in nums {
                    let theirs = oracle_u64(&resp, field);
                    if *mine != theirs {
                        mismatches.push(format!("{input:?}: {field} ours={mine} oracle={theirs}"));
                    }
                }
                let their_labels: Vec<String> = resp
                    .get("releaseLabels")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|s| s.as_str().unwrap_or_default().to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                if v.release_labels() != their_labels.as_slice() {
                    mismatches.push(format!(
                        "{input:?}: releaseLabels ours={:?} oracle={their_labels:?}",
                        v.release_labels()
                    ));
                }
                if v.metadata().is_some() != oracle_bool(&resp, "hasMetadata") {
                    mismatches.push(format!(
                        "{input:?}: hasMetadata ours={} oracle={}",
                        v.metadata().is_some(),
                        oracle_bool(&resp, "hasMetadata")
                    ));
                }
                if v.is_prerelease() != oracle_bool(&resp, "isPrerelease") {
                    mismatches.push(format!(
                        "{input:?}: isPrerelease ours={} oracle={}",
                        v.is_prerelease(),
                        oracle_bool(&resp, "isPrerelease")
                    ));
                }
                parsed.push((input.clone(), v.clone()));
            }
            (Err(_), false) => {} // agree: unparseable
            (Ok(v), false) => {
                mismatches.push(format!(
                    "{input:?}: we parsed {} but NuGet rejects",
                    v.to_full_string()
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

    // Comparison phase over pairs of mutually-parseable inputs: adjacent
    // pairs plus a deterministic stride so distant shapes meet too.
    let n = parsed.len();
    assert!(n > 1000, "generator degenerated: only {n} parseable inputs");
    let mut pairs: Vec<(usize, usize)> = (0..n - 1).map(|i| (i, i + 1)).collect();
    pairs.extend((0..n).map(|i| (i, (i * 7 + 13) % n)));

    for (i, j) in pairs {
        let (sa, va) = &parsed[i];
        let (sb, vb) = &parsed[j];
        let resp = oracle.request(&serde_json::json!({
            "op": "compareVersions",
            "a": sa,
            "b": sb,
        }));
        assert!(
            oracle_bool(&resp, "ok"),
            "oracle failed to reparse {sa:?} / {sb:?}"
        );
        let their_cmp = resp.get("cmp").and_then(|x| x.as_i64()).expect("cmp field");
        let our_cmp = match va.cmp(vb) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        };
        if our_cmp != their_cmp {
            mismatches.push(format!(
                "cmp({sa:?}, {sb:?}): ours={our_cmp} oracle={their_cmp}"
            ));
        }
        // Our lawful `==` mirrors Compare (Rust's Ord contract), so it's
        // diffed against the oracle's *cmp*; NuGet's Equals — inconsistent
        // with its own Compare on pairs like 1.0-01 / 1.0-1 — is mirrored
        // by eq_strict and diffed against the oracle's *eq*.
        if (va == vb) != (their_cmp == 0) {
            mismatches.push(format!(
                "==({sa:?}, {sb:?}): ours={} oracle cmp={their_cmp}",
                va == vb
            ));
        }
        let their_eq = oracle_bool(&resp, "eq");
        if va.eq_strict(vb) != their_eq {
            mismatches.push(format!(
                "eq_strict({sa:?}, {sb:?}): ours={} oracle={their_eq}",
                va.eq_strict(vb)
            ));
        }
    }

    if !mismatches.is_empty() {
        let shown = mismatches.iter().take(25).cloned().collect::<Vec<_>>();
        panic!(
            "{} divergence(s) from NuGet.Versioning; first {}:\n{}",
            mismatches.len(),
            shown.len(),
            shown.join("\n")
        );
    }
}
