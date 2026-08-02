//! Differential tests: `NuGetFramework` vs the real `NuGet.Frameworks` via
//! `tools/nuget-oracle` — both parse entry points (`Parse` and
//! `ParseFolder`) field-by-field, `IsCompatible` over the full
//! zoo × zoo cross-product, and `GetNearest` over random candidate sets.
//!
//! Unlike versions/ranges, the framework input space is close to
//! *enumerable*: real-world TFMs are a finite zoo, so the corner list aims
//! for population coverage and the cross-product does the rest. The
//! generator only adds mutations (case, separators, digits) to catch
//! parse-boundary behaviour.

mod common;

use borzoi_nuget::NuGetFramework;
use common::{FRAMEWORK_ZOO as CORNERS, Oracle, SplitMix64, gen_framework_string};

/// The deliberate platform-grammar deviation (see `framework.rs`): true
/// when the oracle's parsed platform contains a non-letter, i.e. a shape
/// our letters-only grammar refuses — acceptance differences on such
/// inputs are out of envelope.
fn oracle_platform_out_of_envelope(resp: &serde_json::Value) -> bool {
    let platform = resp["platform"].as_str().unwrap_or("");
    !platform.is_empty() && !platform.bytes().all(|b| b.is_ascii_alphabetic())
}

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
fn framework_parse_agrees_with_oracle() {
    let mut inputs: Vec<String> = CORNERS.iter().map(|s| s.to_string()).collect();
    let mut rng = SplitMix64(0x5eed_0004);
    for _ in 0..3000 {
        inputs.push(gen_framework_string(&mut rng));
    }

    let mut oracle = Oracle::spawn();
    let mut mismatches: Vec<String> = Vec::new();

    for (op, ours_parse) in [
        (
            "parseFramework",
            NuGetFramework::parse as fn(&str) -> Result<NuGetFramework, _>,
        ),
        ("parseFolder", NuGetFramework::parse_folder),
    ] {
        for input in &inputs {
            let ours = ours_parse(input);
            let resp = oracle.request(&serde_json::json!({"op": op, "input": input}));
            let oracle_ok = oracle_bool(&resp, "ok");
            match (&ours, oracle_ok) {
                (Ok(f), true) => {
                    // Same envelope rule as the reject arm: if we came
                    // back Unsupported where the oracle parsed a
                    // *non-canonical* spelling, skip — pinned only for
                    // canonical inputs.
                    if f.is_unsupported()
                        && !resp["isUnsupported"].as_bool().unwrap_or(false)
                        && (!oracle_str(&resp, "shortFolderName").eq_ignore_ascii_case(input)
                            || oracle_platform_out_of_envelope(&resp))
                    {
                        continue;
                    }
                    let fields: &[(&str, String)] = &[
                        ("shortFolderName", f.short_folder_name().unwrap_or_default()),
                        ("framework", f.framework().to_owned()),
                        ("version", f.version_string()),
                        ("platform", f.platform().unwrap_or_default().to_owned()),
                        ("platformVersion", f.platform_version_string()),
                        ("profile", f.profile().unwrap_or_default().to_owned()),
                    ];
                    for (field, mine) in fields {
                        let theirs = oracle_str(&resp, field);
                        if *mine != theirs {
                            mismatches.push(format!(
                                "{op} {input:?}: {field} ours={mine:?} oracle={theirs:?}"
                            ));
                        }
                    }
                    let flags: &[(&str, bool)] = &[
                        ("isSpecificFramework", f.is_specific_framework()),
                        ("isUnsupported", f.is_unsupported()),
                        ("isAny", f.is_any()),
                        ("isPCL", f.is_pcl()),
                        ("hasPlatform", f.platform().is_some()),
                        ("hasProfile", f.profile().is_some()),
                    ];
                    for (field, mine) in flags {
                        let theirs = oracle_bool(&resp, field);
                        if *mine != theirs {
                            mismatches.push(format!(
                                "{op} {input:?}: {field} ours={mine} oracle={theirs}"
                            ));
                        }
                    }
                }
                (Err(_), false) => {}
                (Ok(f), false) => {
                    mismatches.push(format!(
                        "{op} {input:?}: we parsed {:?} but NuGet throws",
                        f.short_folder_name()
                    ));
                }
                (Err(e), true) => {
                    // Hard divergence only when the input is a canonical
                    // spelling (it equals its own oracle short name):
                    // fsproj TargetFrameworks and nupkg folder names are
                    // generated *from* GetShortFolderName, so canonical
                    // spellings are the resolver's envelope. NuGet
                    // salvaging some non-canonical garbage we refuse
                    // ("net9999999999-99.0") is out of scope — we treat
                    // those as Unsupported, and slice 7 must decline any
                    // package whose folder TFM we can't parse.
                    let short = oracle_str(&resp, "shortFolderName");
                    if short.eq_ignore_ascii_case(input) {
                        mismatches.push(format!(
                            "{op} {input:?}: NuGet parses canonically but we reject: {e}"
                        ));
                    }
                }
            }
        }
    }

    if !mismatches.is_empty() {
        let shown = mismatches.iter().take(30).cloned().collect::<Vec<_>>();
        panic!(
            "{} divergence(s) from NuGet.Frameworks parse; first {}:\n{}",
            mismatches.len(),
            shown.len(),
            shown.join("\n")
        );
    }
}

#[test]
fn compatibility_cross_product_agrees_with_oracle() {
    let mut oracle = Oracle::spawn();

    // The mutually-parseable zoo (specific frameworks only enter compat in
    // practice, but diff every parseable pair — NuGet answers for all).
    let mut zoo: Vec<(String, NuGetFramework)> = Vec::new();
    for input in CORNERS {
        if let Ok(f) = NuGetFramework::parse(input) {
            let resp = oracle.request(&serde_json::json!({"op": "parseFramework", "input": input}));
            if oracle_bool(&resp, "ok") {
                zoo.push((input.to_string(), f));
            }
        }
    }
    assert!(zoo.len() > 80, "zoo degenerated: {}", zoo.len());

    let mut mismatches: Vec<String> = Vec::new();
    for (ps, pf) in &zoo {
        for (cs, cf) in &zoo {
            let resp = oracle.request(&serde_json::json!({
                "op": "isCompatible", "project": ps, "candidate": cs,
            }));
            if !oracle_bool(&resp, "ok") {
                continue;
            }
            let theirs = oracle_bool(&resp, "compatible");
            let mine = NuGetFramework::is_compatible(pf, cf);
            if mine != theirs {
                mismatches.push(format!(
                    "isCompatible({ps:?}, {cs:?}): ours={mine} oracle={theirs}"
                ));
            }
        }
    }

    if !mismatches.is_empty() {
        let shown = mismatches.iter().take(30).cloned().collect::<Vec<_>>();
        panic!(
            "{} compat divergence(s); first {}:\n{}",
            mismatches.len(),
            shown.len(),
            shown.join("\n")
        );
    }
}

#[test]
fn get_nearest_agrees_with_oracle() {
    let mut oracle = Oracle::spawn();

    // Folder-parseable zoo for candidates; Parse-able *specific* zoo for
    // projects. Non-specific projects (Any / Agnostic / Unsupported) are
    // deliberately outside the sweep: the restore resolver always asks
    // with a concrete project TFM and declines otherwise, and our
    // get_nearest pins that contract by returning None — whereas NuGet's
    // reducer has its own (byzantine, irrelevant-to-us) answers there.
    let projects: Vec<String> = CORNERS
        .iter()
        .filter(|s| NuGetFramework::parse(s).is_ok_and(|f| f.is_specific_framework()))
        .map(|s| s.to_string())
        .collect();
    let candidates_pool: Vec<String> = CORNERS
        .iter()
        .filter(|s| NuGetFramework::parse_folder(s).is_ok())
        .map(|s| s.to_string())
        .collect();

    let mut rng = SplitMix64(0x5eed_0005);
    let mut mismatches: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut cross_family = 0usize;

    for _ in 0..2500 {
        let project = &projects[rng.below(projects.len())];
        let n = 1 + rng.below(8);
        let mut cands: Vec<String> = Vec::new();
        for _ in 0..n {
            let c = candidates_pool[rng.below(candidates_pool.len())].clone();
            if !cands.contains(&c) {
                cands.push(c);
            }
        }

        let resp = oracle.request(&serde_json::json!({
            "op": "getNearest", "project": project, "candidates": cands,
        }));
        if !oracle_bool(&resp, "ok") {
            continue;
        }
        checked += 1;
        let their_index = resp
            .get("nearest")
            .and_then(|x| x.as_i64())
            .expect("nearest field");

        let pf = NuGetFramework::parse(project).expect("filtered");
        let parsed: Vec<NuGetFramework> = cands
            .iter()
            .map(|c| NuGetFramework::parse_folder(c).expect("filtered"))
            .collect();
        let mine = NuGetFramework::get_nearest(&pf, &parsed);

        // Two claims, both required for every candidate set: the
        // *correctness* invariant (pick iff a compatible candidate exists,
        // and the pick is itself compatible), and the exact *choice* among
        // the compatible ones. The second is not weaker than the first — two
        // compatible assets are two different DLLs, and which one is selected
        // decides what surface a consumer reads.
        // (Compare by framework *value*, not index: distinct strings can
        // parse equal — "net45" vs ".NETFramework,Version=v4.5".)
        match (mine, their_index) {
            (None, ti) if ti < 0 => {}
            (Some(mi), ti) if ti >= 0 => {
                if !NuGetFramework::is_compatible(&pf, &parsed[mi]) {
                    mismatches.push(format!(
                        "getNearest({project:?}, {cands:?}): our pick {:?} is incompatible",
                        cands[mi]
                    ));
                }
                let compatible: Vec<&NuGetFramework> = parsed
                    .iter()
                    .filter(|c| NuGetFramework::is_compatible(&pf, c))
                    .collect();
                if compatible.len() > 1
                    && !compatible
                        .iter()
                        .all(|c| c.framework().eq_ignore_ascii_case(pf.framework()))
                {
                    cross_family += 1;
                }
                // Interchangeable picks (mutually compatible — equal
                // frameworks, or distinct spellings/versions that normalise
                // to the same, like uap8 ≡ uap) are both correct.
                let interchangeable =
                    NuGetFramework::is_compatible(&parsed[mi], &parsed[ti as usize])
                        && NuGetFramework::is_compatible(&parsed[ti as usize], &parsed[mi]);
                if !interchangeable {
                    mismatches.push(format!(
                        "getNearest({project:?}, {cands:?}): ours={:?} oracle={:?}",
                        cands[mi], cands[ti as usize]
                    ));
                }
            }
            (mine, ti) => {
                mismatches.push(format!(
                    "getNearest({project:?}, {cands:?}): ours={:?} oracle index={ti}",
                    mine.map(|i| &cands[i])
                ));
            }
        }
    }
    assert!(checked > 1500, "nearest phase degenerated: {checked}");
    // The choice is pinned exactly for every candidate set, which says nothing
    // unless sets needing a cross-family choice occur. The seed is fixed, so
    // this count is deterministic.
    assert!(
        cross_family > 100,
        "nearest phase no longer exercises cross-family choices: {cross_family}"
    );

    if !mismatches.is_empty() {
        let shown = mismatches.iter().take(30).cloned().collect::<Vec<_>>();
        panic!(
            "{} nearest divergence(s); first {}:\n{}",
            mismatches.len(),
            shown.len(),
            shown.join("\n")
        );
    }
}

/// A PCL project choosing between two PCL assets, neither of which subsumes
/// the other — the shape where a tie-break invented from profile *numbers*
/// diverges from NuGet.
///
/// Profile numbers are catalogue identifiers, not a ranking, so "lower wins"
/// agrees with NuGet only by coincidence. NuGet scores each candidate by how
/// many of the project's own framework members find their nearest match inside
/// it, which is a different order entirely: here `Profile7` (net45 + win8)
/// prefers `Profile259`, whose members include net45, over the numerically
/// lower `Profile136`, whose highest .NET member is net40.
///
/// The oracle is asked in-test rather than the index being written down, so
/// the case cannot silently pin our own answer. It reproduces
/// `BORZOI_NUGET_SOAK_SEED=1785528757427249586`.
#[test]
fn get_nearest_matches_the_oracle_between_two_incomparable_pcls() {
    let mut oracle = Oracle::spawn();
    let project = "portable-win8+net45";
    let cands = [
        "portable-net40+sl5+win8+wp8",
        "dnx452",
        "net5.0-windows10.0.19041.0",
        "net45+win8",
        "net8.0",
        "portable-profile259",
        ".NETCoreApp,Version=v3.1",
    ];

    let resp = oracle.request(&serde_json::json!({
        "op": "getNearest", "project": project, "candidates": cands,
    }));
    assert!(oracle_bool(&resp, "ok"), "oracle understood the case");
    let theirs = resp
        .get("nearest")
        .and_then(|x| x.as_i64())
        .expect("nearest field");
    assert!(theirs >= 0, "the oracle finds a compatible candidate");

    let pf = NuGetFramework::parse(project).expect("project parses");
    let parsed: Vec<NuGetFramework> = cands
        .iter()
        .map(|c| NuGetFramework::parse_folder(c).expect("candidate parses"))
        .collect();
    let mine = NuGetFramework::get_nearest(&pf, &parsed).expect("we find one too");

    // Both candidates are compatible with the project, so a value comparison
    // is what discriminates: an incompatible pick would be caught by the sweep
    // above, and this case is about *which* compatible asset is nearest.
    assert_eq!(
        parsed[mine], parsed[theirs as usize],
        "ours={:?} oracle={:?}",
        cands[mine], cands[theirs as usize]
    );
}
