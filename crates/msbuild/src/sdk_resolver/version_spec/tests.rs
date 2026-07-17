use std::cell::Cell;

use proptest::prelude::*;
use proptest::test_runner::{Config as PtConfig, TestRunner};

use super::super::SdkVersion;
use super::{RollForward, VersionSpec, select_sdk_version};

/// `SdkVersion::parse(s).unwrap()` with a less noisy spelling at call
/// sites — the tests are about *what* version, not whether it parses.
fn v(s: &str) -> SdkVersion {
    SdkVersion::parse(s).unwrap_or_else(|| panic!("failed to parse {s:?}"))
}

/// `select_sdk_version` but for ergonomic call sites — takes printed
/// version names and a `VersionSpec`, returns the printed selected
/// name (or `None`).
fn select(candidates: &[&str], spec: VersionSpec) -> Option<String> {
    let parsed: Vec<SdkVersion> = candidates.iter().map(|s| v(s)).collect();
    select_sdk_version(&parsed, Some(&spec)).map(|c| {
        // Recover a printed form. Tests below assert against the
        // original string, so we just need a unique reverse of `v`.
        // The candidate list is the source of truth — find the index.
        let idx = parsed.iter().position(|p| p == c).unwrap();
        candidates[idx].to_owned()
    })
}

/// Test helper: build a spec with the caller's host default set to
/// `false`. Most existing tests assert "stable pin excludes
/// prereleases" semantics, which is what VS-host callers would
/// observe; tests that need CLI/`true` behaviour use `spec_allow_pre`.
fn spec(version: &str, rf: RollForward) -> VersionSpec {
    VersionSpec::with_version(v(version), rf, false)
}

fn spec_allow_pre(version: &str, rf: RollForward, allow: bool) -> VersionSpec {
    VersionSpec::with_version(v(version), rf, allow)
}

// ---------------- Constructor: caller-supplied default ----------------

#[test]
fn with_version_passes_through_caller_default_for_stable_pin() {
    let s = VersionSpec::with_version(v("9.0.100"), RollForward::Patch, false);
    assert!(!s.allow_prerelease());
    let s = VersionSpec::with_version(v("9.0.100"), RollForward::Patch, true);
    assert!(s.allow_prerelease());
}

#[test]
fn with_version_prerelease_pin_unconditionally_forces_allow_prerelease() {
    // Upstream's `from_nearest_global_file` re-asserts
    // `allow_prerelease=true` after constructing the resolver if the
    // requested version is a prerelease, regardless of what the
    // caller (or `global.json`) said.
    let s = VersionSpec::with_version(v("9.0.100-preview.1"), RollForward::Patch, false);
    assert!(s.allow_prerelease());
    let s = VersionSpec::with_version(v("9.0.100-preview.1"), RollForward::Patch, true);
    assert!(s.allow_prerelease());
}

// ---------------- spec == None reproduces v1a ----------------

#[test]
fn no_spec_picks_max() {
    let candidates = [v("8.0.401"), v("9.0.100"), v("10.0.100")];
    let picked = select_sdk_version(&candidates, None);
    assert_eq!(picked, Some(&v("10.0.100")));
}

#[test]
fn no_spec_includes_prereleases_in_pool() {
    // v1a behaviour: highest-wins even with prereleases. Stable still
    // beats prerelease at same numeric, so this just checks we don't
    // accidentally filter prereleases out at the no-spec gate.
    let candidates = [v("9.0.100-preview.1"), v("9.0.100-rc.2")];
    let picked = select_sdk_version(&candidates, None);
    assert_eq!(picked, Some(&v("9.0.100-rc.2")));
}

#[test]
fn empty_candidates_is_none() {
    assert!(select_sdk_version(&[], None).is_none());
    assert!(select_sdk_version(&[], Some(&spec("9.0.100", RollForward::Patch))).is_none());
}

// ---------------- Disable ----------------

#[test]
fn disable_requires_exact_pin() {
    assert_eq!(
        select(&["9.0.100"], spec("9.0.100", RollForward::Disable)),
        Some("9.0.100".into())
    );
    assert_eq!(
        select(&["9.0.105"], spec("9.0.100", RollForward::Disable)),
        None
    );
    // Other versions installed don't help — only exact.
    assert_eq!(
        select(
            &["9.0.105", "9.0.200", "10.0.100"],
            spec("9.0.100", RollForward::Disable)
        ),
        None
    );
}

// ---------------- Patch (default when version specified) ----------------

#[test]
fn patch_picks_exact_pin_when_installed() {
    assert_eq!(
        select(&["9.0.100", "9.0.105"], spec("9.0.100", RollForward::Patch)),
        Some("9.0.100".into())
    );
}

#[test]
fn patch_rolls_forward_within_band() {
    assert_eq!(
        select(&["9.0.105"], spec("9.0.100", RollForward::Patch)),
        Some("9.0.105".into())
    );
    // Multiple patches in band, no exact pin → highest in band.
    assert_eq!(
        select(&["9.0.105", "9.0.199"], spec("9.0.100", RollForward::Patch)),
        Some("9.0.199".into())
    );
}

#[test]
fn patch_will_not_roll_backward() {
    // Pin 9.0.150, only 9.0.100 installed (same band, but patch < pin).
    assert_eq!(
        select(&["9.0.100"], spec("9.0.150", RollForward::Patch)),
        None
    );
}

#[test]
fn patch_will_not_cross_feature_band() {
    // Pin 9.0.100, only 9.0.200 installed (different band).
    assert_eq!(
        select(&["9.0.200"], spec("9.0.100", RollForward::Patch)),
        None
    );
}

// ---------------- LatestPatch ----------------

#[test]
fn latest_patch_ignores_exact_preference() {
    assert_eq!(
        select(
            &["9.0.100", "9.0.105"],
            spec("9.0.100", RollForward::LatestPatch)
        ),
        Some("9.0.105".into())
    );
}

#[test]
fn latest_patch_will_not_roll_backward_within_band() {
    // Pin 9.0.150, installed 9.0.100. Upstream's `matches_policy`
    // applies `current >= requested_version` regardless of the
    // roll-forward variant, so even LatestPatch refuses to pick a
    // lower patch in the same band when a pin is present.
    assert_eq!(
        select(&["9.0.100"], spec("9.0.150", RollForward::LatestPatch)),
        None
    );
}

#[test]
fn latest_patch_will_not_cross_feature_band() {
    assert_eq!(
        select(&["9.0.200"], spec("9.0.100", RollForward::LatestPatch)),
        None
    );
}

// ---------------- Feature ----------------

#[test]
fn feature_prefers_same_band_then_cascades() {
    // Pin 9.0.100 (band 1). Same band available + higher bands.
    // Cascading: prefer same band (lowest band ≥ pin's), highest patch.
    assert_eq!(
        select(
            &["9.0.105", "9.0.200", "9.0.300"],
            spec("9.0.100", RollForward::Feature)
        ),
        Some("9.0.105".into())
    );
}

#[test]
fn feature_cascades_when_pin_band_absent() {
    // Pin's band (1xx) not installed; next higher = 2xx.
    assert_eq!(
        select(
            &["9.0.200", "9.0.300"],
            spec("9.0.100", RollForward::Feature)
        ),
        Some("9.0.200".into())
    );
}

#[test]
fn feature_will_not_cascade_to_higher_minor() {
    assert_eq!(
        select(&["9.1.100"], spec("9.0.100", RollForward::Feature)),
        None
    );
}

#[test]
fn feature_pin_wins_when_no_higher_patch_in_band() {
    // Pin 9.0.100 sits alone in band 1; band 2 also has a candidate.
    // Cascade picks the lowest band (1), max within → 9.0.100. This
    // matches the pin not because of any "exact match" preference,
    // but because nothing else is in band 1.
    assert_eq!(
        select(
            &["9.0.100", "9.0.200"],
            spec("9.0.100", RollForward::Feature)
        ),
        Some("9.0.100".into())
    );
}

#[test]
fn feature_does_not_prefer_exact_pin_over_higher_patch_in_band() {
    // Pin 9.0.501, candidates 9.0.501 and 9.0.503 (both band 5).
    // Upstream's `exact_match_preferred()` is true only for `disable`
    // and `patch`; for `feature` the within-band higher patch wins.
    assert_eq!(
        select(
            &["9.0.501", "9.0.503"],
            spec("9.0.501", RollForward::Feature)
        ),
        Some("9.0.503".into())
    );
}

// ---------------- LatestFeature ----------------

#[test]
fn latest_feature_picks_highest_in_minor() {
    assert_eq!(
        select(
            &["9.0.100", "9.0.200", "9.0.300"],
            spec("9.0.100", RollForward::LatestFeature)
        ),
        Some("9.0.300".into())
    );
}

#[test]
fn latest_feature_will_not_cross_minor() {
    assert_eq!(
        select(&["9.1.100"], spec("9.0.100", RollForward::LatestFeature)),
        None
    );
}

// ---------------- Minor / LatestMinor ----------------

#[test]
fn minor_cascades_by_minor_then_highest_within() {
    // Pin 9.0.100. Same minor (0) and higher minor (1) both installed.
    // Prefer same minor (lowest ≥ pin's), highest within.
    assert_eq!(
        select(&["9.0.200", "9.1.100"], spec("9.0.100", RollForward::Minor)),
        Some("9.0.200".into())
    );
}

#[test]
fn minor_cascades_to_higher_minor_when_pin_minor_absent() {
    assert_eq!(
        select(&["9.1.100", "9.2.100"], spec("9.0.100", RollForward::Minor)),
        Some("9.1.100".into())
    );
}

#[test]
fn minor_cascade_descends_into_lowest_feature_band_in_target_minor() {
    // Pin 9.0.500 (band 5). Only minor 1 has candidates, with bands
    // 1 and 2 both ≥ pin. The Minor cascade must prefer the *lower*
    // band within the chosen minor, not just take the max overall.
    // (Codex review #3 example.)
    assert_eq!(
        select(&["9.1.100", "9.1.205"], spec("9.0.500", RollForward::Minor)),
        Some("9.1.100".into())
    );
}

#[test]
fn minor_does_not_prefer_exact_pin_over_higher_patch_in_band() {
    // Same as the Feature test: `exact_match_preferred()` is false
    // for Minor too, so the within-band max wins.
    assert_eq!(
        select(&["9.0.501", "9.0.503"], spec("9.0.501", RollForward::Minor)),
        Some("9.0.503".into())
    );
}

#[test]
fn minor_will_not_cross_major() {
    assert_eq!(
        select(&["10.0.100"], spec("9.0.100", RollForward::Minor)),
        None
    );
}

#[test]
fn latest_minor_picks_highest_in_major() {
    assert_eq!(
        select(
            &["9.0.100", "9.5.200"],
            spec("9.0.100", RollForward::LatestMinor)
        ),
        Some("9.5.200".into())
    );
}

// ---------------- Major / LatestMajor ----------------

#[test]
fn major_cascades_by_major() {
    assert_eq!(
        select(
            &["9.0.100", "10.0.100", "11.0.100"],
            spec("9.0.100", RollForward::Major)
        ),
        Some("9.0.100".into())
    );
    assert_eq!(
        select(
            &["10.0.100", "11.0.100"],
            spec("9.0.100", RollForward::Major)
        ),
        Some("10.0.100".into())
    );
}

#[test]
fn major_will_not_roll_backward() {
    assert_eq!(
        select(&["8.0.401"], spec("9.0.100", RollForward::Major)),
        None
    );
}

#[test]
fn major_cascade_descends_into_lowest_minor_and_band() {
    // Pin 9.0.500. Major-admitted set includes 10.0.500 (band 5),
    // 10.1.100 (lower minor's lowest band), and 11.0.100. The full
    // cascade key (major, minor, feature_band) ranks them:
    // (10,0,5), (10,1,1), (11,0,1). Min = (10,0,5) → 10.0.500 wins.
    assert_eq!(
        select(
            &["10.0.500", "10.1.100", "11.0.100"],
            spec("9.0.500", RollForward::Major)
        ),
        Some("10.0.500".into())
    );
}

#[test]
fn latest_major_picks_highest_at_or_above_pin() {
    assert_eq!(
        select(
            &["8.0.401", "9.0.100", "10.0.100"],
            spec("9.0.100", RollForward::LatestMajor)
        ),
        Some("10.0.100".into())
    );
}

#[test]
fn latest_major_will_not_roll_backward_when_pinned() {
    // Upstream's `matches_policy` applies `current >= pin` even for
    // `latestMajor` once a pin is in play, so a lone older SDK is
    // not a satisfying candidate. (Codex review #1 example.)
    assert_eq!(
        select(&["8.0.401"], spec("9.0.100", RollForward::LatestMajor)),
        None
    );
}

#[test]
fn any_version_constructor_is_latest_major() {
    let any_pre = VersionSpec::any_version(true);
    assert!(matches!(any_pre.roll_forward(), RollForward::LatestMajor));
    assert!(any_pre.version().is_none());
    assert!(any_pre.allow_prerelease());
}

// ---------------- Prerelease gating ----------------

#[test]
fn stable_pin_excludes_prereleases_by_default() {
    assert_eq!(
        select(
            &["9.0.100", "9.0.105-preview.1"],
            spec("9.0.100", RollForward::LatestPatch)
        ),
        Some("9.0.100".into())
    );
}

#[test]
fn prerelease_pin_allows_prereleases() {
    // Pin is preview.1. allow_prerelease defaults to true. Pick the
    // exact pin even though stable 9.0.100 also exists in band.
    assert_eq!(
        select(
            &["9.0.100", "9.0.100-preview.1", "9.0.100-rc.2"],
            spec("9.0.100-preview.1", RollForward::Patch)
        ),
        Some("9.0.100-preview.1".into())
    );
}

#[test]
fn allow_prerelease_override_admits_prereleases_under_stable_pin() {
    assert_eq!(
        select(
            &["9.0.105-preview.1"],
            spec_allow_pre("9.0.100", RollForward::Patch, true)
        ),
        Some("9.0.105-preview.1".into())
    );
}

#[test]
fn prerelease_pin_forces_admission_of_pin_itself() {
    // Upstream coerces `allow_prerelease=true` whenever the requested
    // version is a prerelease, so an explicit `false` from the caller
    // doesn't prevent the pin from satisfying.
    assert_eq!(
        select(
            &["9.0.100-preview.1"],
            spec_allow_pre("9.0.100-preview.1", RollForward::Patch, false)
        ),
        Some("9.0.100-preview.1".into())
    );
}

#[test]
fn latest_major_admits_prerelease_when_no_stable_satisfies_pin() {
    // Codex review #1 motivating example: a CLI project with a stable
    // pin `8.0.400` and `rollForward=latestMajor` would build under
    // `dotnet` even if the only roll-forward target is a preview SDK,
    // because the CLI host passes `allow_prerelease=true`.
    assert_eq!(
        select(
            &["9.0.100-preview.7"],
            spec_allow_pre("8.0.400", RollForward::LatestMajor, true)
        ),
        Some("9.0.100-preview.7".into())
    );
}

// ---------------- Property tests ----------------

fn numeric_strategy() -> impl Strategy<Value = Vec<u64>> {
    // Realistic .NET-SDK shapes: major in 6..=12, minor in 0..=3,
    // patch in 0..=599 (covers feature bands 0..=5 and patch_in_band
    // 0..=99). Keeps printed forms short and human-readable.
    (6u64..=12u64, 0u64..=3u64, 0u64..=599u64).prop_map(|(a, b, c)| vec![a, b, c])
}

fn prerelease_strategy() -> impl Strategy<Value = Option<String>> {
    // See the parent module's identical strategy: avoid leading-zero
    // numeric identifiers so the inputs parse back to themselves.
    prop_oneof![
        3 => Just(None),
        1 => "preview\\.(0|[1-9][0-9]?)".prop_map(Some),
        1 => "rc\\.(0|[1-9][0-9]?)".prop_map(Some),
    ]
}

fn version_strategy() -> impl Strategy<Value = SdkVersion> {
    (numeric_strategy(), prerelease_strategy()).prop_map(|(numeric, pre)| {
        let head = numeric
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".");
        let s = match pre {
            Some(suffix) => format!("{head}-{suffix}"),
            None => head,
        };
        v(&s)
    })
}

fn roll_forward_strategy() -> impl Strategy<Value = RollForward> {
    prop_oneof![
        Just(RollForward::Disable),
        Just(RollForward::Patch),
        Just(RollForward::Feature),
        Just(RollForward::Minor),
        Just(RollForward::Major),
        Just(RollForward::LatestPatch),
        Just(RollForward::LatestFeature),
        Just(RollForward::LatestMinor),
        Just(RollForward::LatestMajor),
    ]
}

#[test]
fn proptest_selected_version_satisfies_admission() {
    // For any spec + candidate pool, if `select_sdk_version` returns
    // a version, that version must individually satisfy the spec —
    // i.e. it must round-trip through `admits`. This is the core
    // "no spurious selections" guarantee.
    let mut runner = TestRunner::new(PtConfig {
        cases: 512,
        ..PtConfig::default()
    });
    let strategy = (
        proptest::collection::vec(version_strategy(), 0usize..=8),
        version_strategy(),
        roll_forward_strategy(),
        any::<bool>(),
    );
    // Distribution sanity: both "selected something" and "selected
    // nothing" outcomes must fire often enough that we know the
    // generator is reaching both regimes. With 512 cases and a
    // reasonable spread of rollForward variants (some loose, some
    // strict), seeing fewer than 50 of either is a generator bug.
    let some_count = Cell::new(0usize);
    let none_count = Cell::new(0usize);
    runner
        .run(&strategy, |(candidates, pin, rf, allow_pre)| {
            let spec = VersionSpec::with_version(pin.clone(), rf, allow_pre);
            if let Some(picked) = select_sdk_version(&candidates, Some(&spec)) {
                some_count.set(some_count.get() + 1);
                let single = [picked.clone()];
                prop_assert!(
                    select_sdk_version(&single, Some(&spec)).is_some(),
                    "selected version {picked:?} not admitted by spec \
                         (pin={pin:?}, rf={rf:?}, allow_pre={allow_pre:?})"
                );
            } else {
                none_count.set(none_count.get() + 1);
            }
            Ok(())
        })
        .unwrap();
    let s = some_count.get();
    let n = none_count.get();
    assert!(
        s >= 50 && n >= 50,
        "distribution skew: selected={s} / unselected={n} in 512 cases"
    );
}

#[test]
fn proptest_disable_returns_some_iff_exact_in_candidates() {
    let mut runner = TestRunner::new(PtConfig {
        cases: 256,
        ..PtConfig::default()
    });
    let strategy = (
        proptest::collection::vec(version_strategy(), 0usize..=6),
        version_strategy(),
    );
    runner
        .run(&strategy, |(candidates, pin)| {
            // For Disable, we always set allow_prerelease=true so the
            // prerelease filter can't intercept. The semantic of
            // Disable is purely "exact match".
            let spec = VersionSpec::with_version(pin.clone(), RollForward::Disable, true);
            let picked = select_sdk_version(&candidates, Some(&spec));
            let exact_present = candidates.iter().any(|c| c == &pin);
            prop_assert_eq!(picked.is_some(), exact_present);
            if let Some(picked) = picked {
                prop_assert_eq!(picked, &pin);
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn proptest_latest_major_picks_max_among_candidates_at_or_above_pin() {
    // LatestMajor with a pin and allow_prerelease=true admits any
    // candidate >= pin (upstream applies `current >= requested` even
    // under the `latest*` policies) and picks the maximum among them.
    // Distribution sanity: both regimes (something selected vs. None)
    // must fire enough times to confirm the generator explores both.
    let mut runner = TestRunner::new(PtConfig {
        cases: 256,
        ..PtConfig::default()
    });
    let strategy = (
        proptest::collection::vec(version_strategy(), 1usize..=8),
        version_strategy(),
    );
    let some_count = Cell::new(0usize);
    let none_count = Cell::new(0usize);
    runner
        .run(&strategy, |(candidates, pin)| {
            let pin_for_filter = pin.clone();
            let spec = VersionSpec::with_version(pin, RollForward::LatestMajor, true);
            let picked = select_sdk_version(&candidates, Some(&spec));
            let expected = candidates.iter().filter(|c| *c >= &pin_for_filter).max();
            if expected.is_some() {
                some_count.set(some_count.get() + 1);
            } else {
                none_count.set(none_count.get() + 1);
            }
            prop_assert_eq!(picked, expected);
            Ok(())
        })
        .unwrap();
    let s = some_count.get();
    let n = none_count.get();
    assert!(
        s >= 25 && n >= 25,
        "distribution skew: selected={s} / unselected={n} in 256 cases"
    );
}

fn check_monotone_ladder(
    candidate: SdkVersion,
    pin: SdkVersion,
    ladder: &[RollForward],
) -> Result<(), TestCaseError> {
    let candidates = [candidate.clone()];
    let admissions: Vec<bool> = ladder
        .iter()
        .map(|rf| {
            // Always allow prereleases so the test isolates the
            // structural admission, not the prerelease filter.
            let spec = VersionSpec::with_version(pin.clone(), *rf, true);
            select_sdk_version(&candidates, Some(&spec)).is_some()
        })
        .collect();
    for (i, w) in admissions.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        prop_assert!(
            !a || b,
            "monotonicity broken: {:?} admitted={a}, {:?} admitted={b}, candidate={:?}, pin={:?}",
            ladder[i],
            ladder[i + 1],
            candidate,
            pin
        );
    }
    Ok(())
}

#[test]
fn proptest_admission_monotone_across_latest_variants() {
    // LatestPatch ⊆ LatestFeature ⊆ LatestMinor ⊆ LatestMajor.
    // Anything admitted by a stricter `Latest*` variant must also be
    // admitted by the looser one.
    let mut runner = TestRunner::new(PtConfig {
        cases: 512,
        ..PtConfig::default()
    });
    let strategy = (version_strategy(), version_strategy());
    let ladder = [
        RollForward::LatestPatch,
        RollForward::LatestFeature,
        RollForward::LatestMinor,
        RollForward::LatestMajor,
    ];
    runner
        .run(&strategy, |(candidate, pin)| {
            check_monotone_ladder(candidate, pin, &ladder)
        })
        .unwrap();
}

#[test]
fn proptest_admission_monotone_across_roll_variants() {
    // Patch ⊆ Feature ⊆ Minor ⊆ Major.
    let mut runner = TestRunner::new(PtConfig {
        cases: 512,
        ..PtConfig::default()
    });
    let strategy = (version_strategy(), version_strategy());
    let ladder = [
        RollForward::Patch,
        RollForward::Feature,
        RollForward::Minor,
        RollForward::Major,
    ];
    runner
        .run(&strategy, |(candidate, pin)| {
            check_monotone_ladder(candidate, pin, &ladder)
        })
        .unwrap();
}

#[test]
fn proptest_no_prerelease_emitted_without_opt_in() {
    // Stable pin with allow_prerelease=false (the default) never
    // selects a prerelease candidate, no matter what roll-forward
    // variant is in play.
    let mut runner = TestRunner::new(PtConfig {
        cases: 512,
        ..PtConfig::default()
    });
    // Generate a stable pin specifically (no prerelease suffix).
    let pin_strategy = numeric_strategy().prop_map(|numeric| {
        let head = numeric
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".");
        v(&head)
    });
    let strategy = (
        proptest::collection::vec(version_strategy(), 0usize..=8),
        pin_strategy,
        roll_forward_strategy(),
    );
    runner
        .run(&strategy, |(candidates, pin, rf)| {
            // Caller-supplied `false`; pin is stable, so force-true
            // doesn't kick in, and no prerelease may ever be selected.
            let spec = VersionSpec::with_version(pin, rf, false);
            if let Some(picked) = select_sdk_version(&candidates, Some(&spec)) {
                prop_assert!(
                    !picked.is_prerelease(),
                    "prerelease leaked under allow_prerelease=false stable pin: {picked:?}"
                );
            }
            Ok(())
        })
        .unwrap();
}
