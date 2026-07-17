//! Exhaustive framework differential against `NuGet.Frameworks`, enumerating
//! a large cross-product of canonical TFMs rather than a hand-picked zoo.
//! This exists because the hand-curated `framework_diff.rs` corpus, however
//! broad, only tests what someone thought to list — and code review kept
//! surfacing legacy-TFM gaps it missed (portable-versioned names, `.NETCore`,
//! WinRT/UAP nuances, CompactFramework). This test front-loads that
//! discovery: it sweeps the whole canonical space in one pass.
//!
//! It encodes the resolver's **correctness envelope** precisely:
//!
//! - **Parse** must be *exact* on every canonical input (an input equal to
//!   its own `GetShortFolderName`, minus the documented platform-grammar
//!   deviation). Zero tolerance, all frameworks.
//! - **Compatibility** must be *exact in both directions* — never
//!   over-resolve (a false positive selects an incompatible asset:
//!   corruption) and never under-resolve (skips a valid asset) — whenever
//!   the **project** is a *live* framework: `.NETFramework`, `.NETCoreApp`
//!   (including its net5.0+ platform TFMs), or `.NETStandard`. These are
//!   the only frameworks an F# project targets, and they resolve exactly.
//! - When the project is a *dead* framework (Windows Phone, Silverlight,
//!   WinRT, UAP, `.NETPlatform`/dotnet, DNX/ASP.NET, `.NETCore`, old
//!   Xamarin…) compatibility may diverge either way — those frameworks are
//!   never resolved against, and their byzantine legacy compat graph is
//!   deliberately left unmodelled rather than reverse-engineered. A
//!   broadened proactive sweep confirmed the split: every over- or
//!   under-resolution falls on a dead *project*; no live project diverges.

mod common;

use borzoi_nuget::NuGetFramework;
use common::Oracle;

/// Enumerate a broad cross-product of TFM strings across every framework
/// family, at a spread of versions/platforms/profiles/portable forms.
fn gen_tfms() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let fams: &[(&str, &[&str])] = &[
        (
            "net",
            &[
                "11", "20", "35", "40", "403", "45", "451", "452", "46", "461", "462", "47", "471",
                "472", "48", "481",
            ],
        ),
        ("net", &["5.0", "6.0", "7.0", "8.0", "9.0", "10.0"]),
        (
            "netstandard",
            &[
                "1.0", "1.1", "1.2", "1.3", "1.4", "1.5", "1.6", "2.0", "2.1",
            ],
        ),
        (
            "netcoreapp",
            &["1.0", "1.1", "2.0", "2.1", "2.2", "3.0", "3.1", "5.0"],
        ),
        ("netcore", &["", "45", "451", "50"]),
        // Identifier-name short aliases NuGet accepts (and some it doesn't —
        // netmicroframework/netplatform must stay Unsupported).
        ("netframework", &["45", "40", "472"]),
        ("silverlight", &["4", "5"]),
        ("windowsphone", &["", "8", "81"]),
        ("windows", &["", "8", "81"]),
        ("netmicroframework", &["", "1.0"]),
        ("netplatform", &["", "5.4"]),
        ("uap", &["", "10", "10.0", "10.0.14393", "10.0.16299"]),
        ("win", &["", "8", "81", "10"]),
        ("winrt", &["", "45"]),
        ("wp", &["", "7", "71", "8", "81"]),
        ("wpa", &["", "81"]),
        ("sl", &["3", "4", "5"]),
        ("monoandroid", &["", "10", "90", "12.0"]),
        ("monotouch", &["", "10"]),
        ("monomac", &["", "20"]),
        ("xamarinios", &["", "10"]),
        ("xamarinmac", &["", "20"]),
        ("xamarintvos", &[""]),
        ("xamarinwatchos", &[""]),
        ("tizen", &["", "40", "60"]),
        ("dnx", &["", "45", "451", "452"]),
        ("dnxcore", &["", "50"]),
        ("aspnet", &["", "50"]),
        ("aspnetcore", &["", "50"]),
        ("native", &[""]),
        ("netmf", &["", "1.0"]),
        ("netnano", &["1.0"]),
        (
            "dotnet",
            &["", "5.1", "5.2", "5.3", "5.4", "5.5", "5.6", "6.0"],
        ),
    ];
    for (fam, vers) in fams {
        for v in *vers {
            out.push(format!("{fam}{v}"));
        }
    }
    for base in ["net5.0", "net6.0", "net7.0", "net8.0"] {
        for plat in [
            "windows",
            "android",
            "ios",
            "macos",
            "maccatalyst",
            "tvos",
            "tizen",
            "browser",
        ] {
            out.push(format!("{base}-{plat}"));
            out.push(format!("{base}-{plat}10.0"));
            out.push(format!("{base}-{plat}10.0.19041"));
        }
    }
    for base in ["net40", "net45", "netstandard2.0", "netcoreapp3.1"] {
        // "wp"/"wp71" profiles canonicalise to WindowsPhone; "full" drops.
        for prof in ["client", "full", "cf", "wp", "wp71"] {
            out.push(format!("{base}-{prof}"));
        }
    }
    // Dead-framework version spreads (proven to only ever under-resolve, and
    // only as *projects* — never a live project — so they exercise the
    // envelope's dead side and confirm no live project over-resolves them as
    // candidates). Includes a verbatim (non-matching) portable member list.
    for f in [
        "wp",
        "wpa",
        "sl",
        "silverlight",
        "winrt",
        "dotnet",
        "windowsphone",
    ] {
        for v in ["", "5", "8", "81", "5.4", "10"] {
            out.push(format!("{f}{v}"));
        }
    }
    out.push("portable85-wp8+win81".to_owned());
    // Every real PCL profile (the full PCL_PROFILES table), so the compat
    // cross-product exercises each one against Xamarin/android projects —
    // the class a sparse profile list let slip past review.
    let profiles = [
        2u32, 3, 4, 5, 6, 7, 14, 18, 19, 23, 24, 31, 32, 36, 37, 41, 42, 44, 46, 47, 49, 78, 84,
        88, 92, 95, 96, 102, 104, 111, 136, 143, 147, 151, 154, 157, 158, 225, 240, 255, 259, 328,
        336, 344, 459,
    ];
    for p in profiles {
        out.push(format!("portable-profile{p}"));
        out.push(format!("portable-Profile{p}"));
        // Include dotted portable versions (a component > 9 renders
        // dotted: "portable4.10-…", "portable10.0-…").
        for ver in ["", "45", "73", "4.10", "10.0"] {
            out.push(format!("portable{ver}-profile{p}"));
        }
    }
    for members in [
        "net45+win8",
        "net45+win8+wp8",
        "net45+win8+wp8+wpa81",
        "net40+sl5+win8+wp8",
        "net45+sl5+win8",
        "win8+net45",
        "net451+win81",
    ] {
        out.push(format!("portable-{members}"));
        for ver in ["45", "46", "85"] {
            out.push(format!("portable{ver}-{members}"));
        }
    }
    // `netportable` is an accepted alias for the `portable` prefix.
    out.push("netportable-profile7".to_owned());
    out.push("netportable45-profile7".to_owned());
    out.push("netportable-net45+win8".to_owned());
    for (id, vs) in [
        ("NETFramework", &["v4.5", "v4"][..]),
        ("NETPortable", &["v4.5"][..]),
        ("NETMicroFramework", &["v1.0"][..]),
        ("NETPlatform", &["v5.4"][..]),
    ] {
        for v in vs {
            out.push(format!("{id},Version={v}"));
        }
    }
    for (id, vs) in [
        (".NETFramework", &["v4", "v4.5", "v4.7.2"][..]),
        (".NETCoreApp", &["v8", "v8.0", "v3.1"][..]),
        (".NETStandard", &["v2.0", "v2.1"][..]),
        (".NETPortable", &["v0.0", "v4.5", "v7.3", "v8.5"][..]),
        (".NETPlatform", &["v5.4"][..]),
        ("UAP", &["v10.0"][..]),
        ("Silverlight", &["v5.0"][..]),
        ("WindowsPhone", &["v8.0"][..]),
    ] {
        for v in vs {
            out.push(format!("{id},Version={v}"));
            out.push(format!("{id},Version={v},Profile=Profile259"));
            out.push(format!("{id},Version={v},Profile=Client"));
            out.push(format!("{id},Version={v},Profile=Full"));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The frameworks F# projects actually target — the ones whose
/// compatibility must be *exact*. Everything else is a dead framework
/// whose compat graph is left unmodelled (never resolved against).
fn is_live_framework(f: &NuGetFramework) -> bool {
    matches!(
        f.framework(),
        ".NETFramework" | ".NETCoreApp" | ".NETStandard"
    )
}

fn s(resp: &serde_json::Value, field: &str) -> String {
    resp[field].as_str().unwrap_or("").to_owned()
}

#[test]
fn parse_and_compat_are_within_envelope() {
    let tfms = gen_tfms();
    let mut oracle = Oracle::spawn();
    let mut parse_fail: Vec<String> = Vec::new();
    // Frameworks both sides accept as canonical specifics — the compat pool.
    let mut canonical: Vec<(String, NuGetFramework)> = Vec::new();

    for op in ["parseFramework", "parseFolder"] {
        for t in &tfms {
            let ours = if op == "parseFramework" {
                NuGetFramework::parse(t)
            } else {
                NuGetFramework::parse_folder(t)
            };
            let resp = oracle.request(&serde_json::json!({"op": op, "input": t}));
            let ok = resp["ok"].as_bool().expect("ok");
            let oshort = s(&resp, "shortFolderName");
            let plat = s(&resp, "platform");
            // Canonical envelope: input equals its own short name, and the
            // platform (if any) is within our letters-only grammar.
            let canonical_input = oshort.eq_ignore_ascii_case(t)
                && (plat.is_empty() || plat.bytes().all(|b| b.is_ascii_alphabetic()));

            match (&ours, ok) {
                (Ok(f), true) if canonical_input => {
                    if f.short_folder_name().unwrap_or_default() != oshort
                        || f.framework() != s(&resp, "framework")
                        || f.version_string() != s(&resp, "version")
                        || f.profile().unwrap_or("") != s(&resp, "profile")
                    {
                        parse_fail.push(format!(
                            "[{op}] {t:?}: ours(short={:?} fw={} v={} prof={:?}) oracle(short={:?} fw={} v={} prof={:?})",
                            f.short_folder_name(), f.framework(), f.version_string(), f.profile().unwrap_or(""),
                            oshort, s(&resp, "framework"), s(&resp, "version"), s(&resp, "profile"),
                        ));
                    }
                    if op == "parseFramework" && f.is_specific_framework() {
                        canonical.push((t.clone(), f.clone()));
                    }
                }
                (Err(_), true) if canonical_input => {
                    parse_fail.push(format!("[{op}] {t:?}: we reject, oracle short={oshort:?}"));
                }
                (Ok(f), false) if f.is_specific_framework() => {
                    parse_fail.push(format!(
                        "[{op}] {t:?}: we invent specific {}, oracle rejects",
                        f.framework()
                    ));
                }
                _ => {}
            }
        }
    }

    assert!(
        parse_fail.is_empty(),
        "{} parse divergence(s) from NuGet.Frameworks; first {}:\n{}",
        parse_fail.len(),
        parse_fail.len().min(30),
        parse_fail
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        canonical.len() > 200,
        "canonical set too small: {}",
        canonical.len()
    );

    // Compat cross-product: when the project is a live framework, require
    // exact agreement in both directions. Dead-framework projects (never
    // resolved against) may diverge.
    let mut divergences: Vec<String> = Vec::new();
    for (ps, pf) in &canonical {
        if !is_live_framework(pf) {
            continue;
        }
        for (cs, cf) in &canonical {
            let resp = oracle.request(&serde_json::json!({
                "op": "isCompatible", "project": ps, "candidate": cs,
            }));
            if !resp["ok"].as_bool().expect("ok") {
                continue;
            }
            let theirs = resp["compatible"].as_bool().expect("compatible");
            let mine = NuGetFramework::is_compatible(pf, cf);
            if mine != theirs {
                let dir = if mine { "OVER" } else { "UNDER" };
                divergences.push(format!("{dir} {ps:?} <- {cs:?} (oracle={theirs})"));
            }
        }
    }

    assert!(
        divergences.is_empty(),
        "{} compat divergence(s) on a LIVE project (must be exact — \
         over-resolving selects an incompatible asset, under-resolving \
         skips a valid one); first {}:\n{}",
        divergences.len(),
        divergences.len().min(30),
        divergences
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
