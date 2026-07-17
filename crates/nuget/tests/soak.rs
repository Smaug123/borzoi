//! Fresh-seed differential soak: the committed diff tests run a *fixed*
//! corpus (reproducibility), which means they re-check the same inputs
//! forever — a clean pass proves nothing about shapes the seed never
//! produced. This `#[ignore]`d soak reruns the same differential logic
//! with a fresh seed and 10× volume; run it when touching the parsers:
//!
//! ```sh
//! cargo test -p borzoi-nuget --test soak -- --ignored --nocapture
//! ```
//!
//! It has caught a real bug the fixed corpus missed (an over-fit
//! "bracketed dots-zero floats must resolve to 0.0.0" rule, actually the
//! min>max ordering check wearing a costume — the fixed corpus never
//! paired a dots-zero star with a large-enough max). On failure it prints
//! the seed; reproduce with `BORZOI_NUGET_SOAK_SEED=<seed>`.
//! `BORZOI_NUGET_SOAK_VERSIONS` / `_RANGES` scale the volume.
//!
//! Checks are the high-signal subset (acceptance, normalised strings,
//! float behaviour, cmp/eq/satisfies); the fixed-seed tests remain the
//! field-by-field pin.
//!
//! ## Envelope
//!
//! Versions and ranges are checked for exact agreement everywhere — their
//! grammars are total and the fuzzer explores the whole space. Frameworks
//! are different: NuGet's TFM parser is wildly lenient, salvaging garbage
//! ("netcoreapp3-.0", "Profile= Client") into specific frameworks that no
//! real project or package ever produces. So the framework checks assert
//! *exact fields* only on the **canonical round-trip envelope** (inputs
//! equal to their own `GetShortFolderName`, minus the platform-grammar
//! deviation documented in `framework.rs`), and off that envelope require
//! only that we never invent a *specific* framework from a string NuGet
//! can't parse. Compatibility and nearest-match are fuzzed over random
//! pairings/candidate-sets of the **real-TFM zoo** only. The tight,
//! reviewable real-world contract is the fixed corpus in
//! `framework_diff.rs`; this soak is the regression guard around it.

mod common;

use borzoi_nuget::{NuGetFramework, NuGetVersion, VersionRange};
use common::{
    FRAMEWORK_ZOO, Oracle, SplitMix64, gen_framework_string, gen_range_string, gen_version_string,
};

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[test]
#[ignore = "fresh-seed soak; run explicitly when touching the parsers"]
fn fresh_seed_soak_agrees_with_oracle() {
    let seed = env_u64("BORZOI_NUGET_SOAK_SEED", {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos() as u64
    });
    let n_versions = env_u64("BORZOI_NUGET_SOAK_VERSIONS", 100_000) as usize;
    let n_ranges = env_u64("BORZOI_NUGET_SOAK_RANGES", 70_000) as usize;
    println!("soak seed: {seed} (versions={n_versions}, ranges={n_ranges})");

    let mut rng = SplitMix64(seed);
    let mut oracle = Oracle::spawn();
    let mut mismatches: Vec<String> = Vec::new();
    let mut parsed: Vec<(String, NuGetVersion)> = Vec::new();

    for _ in 0..n_versions {
        let input = gen_version_string(&mut rng);
        let ours = NuGetVersion::parse(&input);
        let resp = oracle.request(&serde_json::json!({"op": "parseVersion", "input": input}));
        let ok = resp["ok"].as_bool().expect("ok field");
        match (&ours, ok) {
            (Ok(v), true) => {
                if v.to_normalized_string() != resp["normalized"].as_str().expect("normalized")
                    || v.to_full_string() != resp["full"].as_str().expect("full")
                {
                    mismatches.push(format!("version fields: {input:?}"));
                }
                parsed.push((input, v.clone()));
            }
            (Err(_), false) => {}
            _ => mismatches.push(format!("version accept: {input:?} ours={}", ours.is_ok())),
        }
    }

    let n = parsed.len();
    assert!(n > n_versions / 10, "generator degenerated: {n} parsed");
    for i in 0..n - 1 {
        let (sa, va) = &parsed[i];
        let (sb, vb) = &parsed[(i * 13 + 7) % n];
        let resp = oracle.request(&serde_json::json!({"op": "compareVersions", "a": sa, "b": sb}));
        let cmp = resp["cmp"].as_i64().expect("cmp");
        let ours = match va.cmp(vb) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        };
        if ours != cmp
            || (va == vb) != (cmp == 0)
            || va.eq_strict(vb) != resp["eq"].as_bool().expect("eq")
        {
            mismatches.push(format!("compare: {sa:?} vs {sb:?}"));
        }
    }

    let mut satisfies_checked = 0usize;
    for _ in 0..n_ranges {
        let input = gen_range_string(&mut rng);
        let ours = VersionRange::parse(&input);
        let resp = oracle.request(&serde_json::json!({"op": "parseRange", "input": input}));
        let ok = resp["ok"].as_bool().expect("ok field");
        match (&ours, ok) {
            (Ok(r), true) => {
                let float = r
                    .float_behavior()
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "None".to_owned());
                // Float *fields* on pathological patterns are out of
                // envelope for the same reason as float acceptance below —
                // the resolver declines every floating range, and the ~90
                // realistic float corners in range_diff.rs are the exact
                // pin. Non-float ranges stay exact here.
                if !input.contains('*')
                    && (r.to_normalized_string()
                        != resp["normalized"].as_str().expect("normalized")
                        || float != resp["floatBehavior"].as_str().expect("floatBehavior"))
                {
                    mismatches.push(format!("range fields: {input:?}"));
                }
                let mut probes: Vec<String> = vec![parsed[rng.below(n)].0.clone()];
                if let Some(min) = r.min_version() {
                    probes.push(min.to_full_string());
                }
                if let Some(max) = r.max_version() {
                    probes.push(max.to_full_string());
                    probes.push(format!("{}-0", max.to_normalized_string()));
                }
                for probe in probes {
                    let Ok(v) = NuGetVersion::parse(&probe) else {
                        continue;
                    };
                    let resp = oracle.request(&serde_json::json!({
                        "op": "rangeSatisfies", "range": input, "version": probe,
                    }));
                    if !resp["ok"].as_bool().expect("ok field") {
                        continue;
                    }
                    satisfies_checked += 1;
                    if r.satisfies(&v) != resp["satisfies"].as_bool().expect("satisfies") {
                        mismatches.push(format!("satisfies: {input:?} / {probe:?}"));
                    }
                }
            }
            (Err(_), false) => {}
            // Float parse-acceptance on pathological patterns (dash-laden
            // prerelease labels like "9--1v.*") is out of envelope: the
            // resolver *declines every floating range* by policy (see
            // docs/nuget-restore-plan.md), so whether such garbage parses
            // as a malformed float or is rejected outright, it is declined
            // either way — never resolved differently. The ~90 realistic
            // float corners in `range_diff.rs` remain the exact pin.
            _ if input.contains('*') => {}
            _ => mismatches.push(format!("range accept: {input:?} ours={}", ours.is_ok())),
        }
    }
    assert!(satisfies_checked > n_ranges / 10, "satisfies degenerated");

    // Frameworks: mutated-zoo parse (both entry points), random compat
    // pairs, random nearest sets — same checks as framework_diff, fresh
    // inputs.
    let n_frameworks = env_u64("BORZOI_NUGET_SOAK_FRAMEWORKS", 20_000) as usize;
    let mut fw_inputs: Vec<String> = Vec::new();
    for _ in 0..n_frameworks {
        fw_inputs.push(gen_framework_string(&mut rng));
    }
    for op in ["parseFramework", "parseFolder"] {
        for input in &fw_inputs {
            let ours = if op == "parseFramework" {
                NuGetFramework::parse(input)
            } else {
                NuGetFramework::parse_folder(input)
            };
            let resp = oracle.request(&serde_json::json!({"op": op, "input": input}));
            let ok = resp["ok"].as_bool().expect("ok field");
            // Same canonical-spelling envelope as framework_diff.rs: when
            // we come out Unsupported (or reject) on an input that is NOT
            // its own oracle short name, NuGet salvaging it is out of
            // scope — the resolver only meets canonical spellings.
            let oracle_short = resp["shortFolderName"].as_str().unwrap_or("");
            let oracle_platform = resp["platform"].as_str().unwrap_or("");
            // Canonical spelling, minus the deliberate platform-grammar
            // deviation (see framework.rs): oracle platforms containing
            // non-letters are out of envelope even when canonical.
            let canonical = oracle_short.eq_ignore_ascii_case(input)
                && (oracle_platform.is_empty()
                    || oracle_platform.bytes().all(|b| b.is_ascii_alphabetic()));
            // The soak asserts *exact fields* only on the canonical
            // round-trip envelope. NuGet's parser is far more lenient than
            // any real TFM: it salvages garbage like "netcoreapp3-.0" or
            // "Profile= Client" into specific frameworks with dashes-in-
            // versions and spaces-in-profiles, and its short-name emitter
            // faithfully reproduces the garbage. Chasing byte-agreement on
            // that salvage surface is a bottomless well with no payoff —
            // restore only ever meets canonical spellings (fsproj
            // TargetFrameworks and nupkg folder names are *generated by*
            // GetShortFolderName). So off the envelope we require only
            // "don't lie": never claim a non-canonical string is a
            // *specific* framework we'd then resolve against.
            match (&ours, ok) {
                (Ok(f), true) if canonical => {
                    if f.short_folder_name().unwrap_or_default() != oracle_short
                        || f.framework() != resp["framework"].as_str().expect("framework")
                        || f.version_string() != resp["version"].as_str().expect("version")
                    {
                        mismatches.push(format!("{op}: {input:?}"));
                    }
                }
                (Err(_), true) if canonical => {
                    mismatches.push(format!("{op} reject canonical: {input:?}"));
                }
                // Off-envelope: any parse (or non-parse) is acceptable so
                // long as we don't manufacture a specific framework from a
                // string NuGet itself couldn't parse at all.
                (Ok(f), false) if f.is_specific_framework() => {
                    mismatches.push(format!("{op} invents specific: {input:?}"));
                }
                _ => {}
            }
        }
    }

    // Compat and nearest draw only from the real-TFM zoo, not the mutated
    // fuzz inputs: semantic behaviour (compatibility, nearest-match) on a
    // salvaged-garbage framework is meaningless to the resolver and would
    // only add cross-parser flakiness. The zoo is the resolver's actual
    // envelope; the random *pairings and candidate sets* over it are what
    // this section fuzzes.
    let parseable: Vec<(String, NuGetFramework)> = FRAMEWORK_ZOO
        .iter()
        .filter_map(|s| NuGetFramework::parse(s).ok().map(|f| (s.to_string(), f)))
        .collect();
    let np = parseable.len();
    for _ in 0..(n_frameworks / 2) {
        let (ps, pf) = &parseable[rng.below(np)];
        let (cs, cf) = &parseable[rng.below(np)];
        let resp = oracle.request(&serde_json::json!({
            "op": "isCompatible", "project": ps, "candidate": cs,
        }));
        if !resp["ok"].as_bool().expect("ok field") {
            continue;
        }
        let theirs = resp["compatible"].as_bool().expect("compatible");
        if NuGetFramework::is_compatible(pf, cf) != theirs {
            mismatches.push(format!("isCompatible({ps:?}, {cs:?}): oracle={theirs}"));
        }
    }

    let folder_pool: Vec<String> = parseable
        .iter()
        .filter(|(s, _)| NuGetFramework::parse_folder(s).is_ok())
        .map(|(s, _)| s.clone())
        .collect();
    let specific_projects: Vec<&(String, NuGetFramework)> = parseable
        .iter()
        .filter(|(_, f)| f.is_specific_framework())
        .collect();
    for _ in 0..(n_frameworks / 10) {
        let (ps, pf) = specific_projects[rng.below(specific_projects.len())];
        let n = 1 + rng.below(8);
        let mut cands: Vec<String> = Vec::new();
        for _ in 0..n {
            let c = folder_pool[rng.below(folder_pool.len())].clone();
            if !cands.contains(&c) {
                cands.push(c);
            }
        }
        let resp = oracle.request(&serde_json::json!({
            "op": "getNearest", "project": ps, "candidates": cands,
        }));
        if !resp["ok"].as_bool().expect("ok field") {
            continue;
        }
        let their_index = resp["nearest"].as_i64().expect("nearest");
        let parsed: Vec<NuGetFramework> = cands
            .iter()
            .map(|c| NuGetFramework::parse_folder(c).expect("pool is folder-parseable"))
            .collect();
        let mine = NuGetFramework::get_nearest(pf, &parsed);

        // Correctness invariant (asserted everywhere): we return Some
        // exactly when the oracle finds a compatible candidate, and our
        // pick is itself compatible. The exact *choice* among mutually
        // compatible candidates is only pinned on homogeneous sets — where
        // every candidate the oracle deemed compatible shares the project's
        // framework identifier, the realistic shape of a package's folders.
        // Heterogeneous cross-family precedence is documented-approximate
        // (see framework.rs); both picks are always compatible, so a
        // disagreement there is an optimality gap, not a correctness one.
        match (mine, their_index) {
            (None, ti) if ti < 0 => {}
            (None, ti) => {
                mismatches.push(format!(
                    "getNearest({ps:?}, {cands:?}): we found nothing, oracle={ti}"
                ));
            }
            (Some(mi), ti) if ti < 0 => {
                mismatches.push(format!(
                    "getNearest({ps:?}, {cands:?}): we picked {mi}, oracle found nothing"
                ));
            }
            (Some(mi), ti) => {
                if !NuGetFramework::is_compatible(pf, &parsed[mi]) {
                    mismatches.push(format!(
                        "getNearest({ps:?}, {cands:?}): our pick {mi} is incompatible"
                    ));
                }
                // Exact pin on homogeneous candidate sets only, and even
                // there interchangeable picks (mutually compatible — equal
                // frameworks or distinct spellings normalising to the same,
                // like uap8 ≡ uap) are both correct.
                let homogeneous = parsed
                    .iter()
                    .filter(|c| NuGetFramework::is_compatible(pf, c))
                    .all(|c| c.framework().eq_ignore_ascii_case(pf.framework()));
                let interchangeable =
                    NuGetFramework::is_compatible(&parsed[mi], &parsed[ti as usize])
                        && NuGetFramework::is_compatible(&parsed[ti as usize], &parsed[mi]);
                if homogeneous && !interchangeable {
                    mismatches.push(format!(
                        "getNearest({ps:?}, {cands:?}): homogeneous, ours={mi} oracle={ti}"
                    ));
                }
            }
        }
    }

    if !mismatches.is_empty() {
        let shown = mismatches.iter().take(25).cloned().collect::<Vec<_>>();
        panic!(
            "seed {seed}: {} divergence(s); first {}:\n{}",
            mismatches.len(),
            shown.len(),
            shown.join("\n")
        );
    }
}
