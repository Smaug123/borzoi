//! Property tests for `NuGetFramework`, pure Rust side (no oracle). NuGet
//! fidelity is `framework_diff.rs`'s job; these pin oracle-independent
//! algebra — the invariants that must hold whatever the tables say, so a
//! regression is caught even with the oracle unavailable. The domain is the
//! real-TFM zoo (`FRAMEWORK_ZOO`), sampled by proptest.

mod common;

use borzoi_nuget::NuGetFramework;
use common::FRAMEWORK_ZOO;
use proptest::prelude::*;

/// A parseable, *specific* framework drawn from the zoo (the domain over
/// which compatibility and nearest-match are meaningful).
fn specific_framework() -> impl Strategy<Value = NuGetFramework> {
    proptest::sample::select(FRAMEWORK_ZOO).prop_filter_map("parseable specific framework", |s| {
        NuGetFramework::parse(s)
            .ok()
            .filter(NuGetFramework::is_specific_framework)
    })
}

/// A specific framework parsed from a *short* TFM (no comma). The long
/// `FrameworkName` form lets NuGet salvage bogus identifiers ("net8,0" →
/// framework "net8") that don't round-trip and aren't real frameworks;
/// excluding commas keeps the round-trip domain to genuine short forms,
/// which every long form has an equivalent of.
fn short_form_framework() -> impl Strategy<Value = (String, NuGetFramework)> {
    proptest::sample::select(FRAMEWORK_ZOO).prop_filter_map(
        "parseable specific short-form framework",
        |s| {
            if s.contains(',') {
                return None;
            }
            NuGetFramework::parse(s)
                .ok()
                .filter(NuGetFramework::is_specific_framework)
                .map(|f| (s.to_string(), f))
        },
    )
}

proptest! {
    /// Compatibility is reflexive: every specific framework can consume its
    /// own assets.
    #[test]
    fn compat_is_reflexive(f in specific_framework()) {
        prop_assert!(NuGetFramework::is_compatible(&f, &f));
    }

    /// A short-form framework round-trips through its own
    /// `GetShortFolderName`: parsing the rendered short name yields an equal
    /// framework.
    #[test]
    fn short_name_round_trips((_s, f) in short_form_framework()) {
        if let Some(short) = f.short_folder_name() {
            let reparsed = NuGetFramework::parse(&short)
                .unwrap_or_else(|e| panic!("short name {short:?} should parse: {e}"));
            prop_assert_eq!(reparsed, f);
        }
    }

    /// Single-candidate nearest is exactly compatibility: `get_nearest(p,
    /// [c])` picks `c` iff `p` accepts `c`.
    #[test]
    fn nearest_single_candidate(p in specific_framework(), c in specific_framework()) {
        let got = NuGetFramework::get_nearest(&p, std::slice::from_ref(&c));
        prop_assert_eq!(got.is_some(), NuGetFramework::is_compatible(&p, &c));
    }

    /// A framework is its own nearest match.
    #[test]
    fn self_is_nearest(f in specific_framework()) {
        prop_assert_eq!(
            NuGetFramework::get_nearest(&f, std::slice::from_ref(&f)),
            Some(0)
        );
    }

    /// The correctness invariant behind the whole resolver use: whatever
    /// `get_nearest` returns is always a *compatible* candidate, and it
    /// returns `Some` exactly when some candidate is compatible. (The
    /// *choice* among compatibles is what `framework_diff`/`soak` pin; this
    /// guards the never-pick-an-incompatible-asset property.)
    #[test]
    fn nearest_picks_a_compatible_candidate(
        p in specific_framework(),
        cs in proptest::collection::vec(specific_framework(), 0..6),
    ) {
        let any_compatible = cs.iter().any(|c| NuGetFramework::is_compatible(&p, c));
        match NuGetFramework::get_nearest(&p, &cs) {
            Some(i) => prop_assert!(NuGetFramework::is_compatible(&p, &cs[i])),
            None => prop_assert!(!any_compatible),
        }
    }
}
