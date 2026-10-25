//! Property tests for `VersionRange`, pure Rust side (no oracle). Fidelity
//! to NuGet is the oracle diff's job (`range_diff.rs`); these pin internal
//! coherence: `satisfies` agrees with the parsed bounds, ranges are convex,
//! and the normalised string round-trips.

use borzoi_nuget::{NuGetVersion, VersionRange};
use proptest::prelude::*;
use std::cmp::Ordering;

/// A valid version string: 1–4 numeric parts, optional labels. (No
/// metadata: range bound comparisons ignore it anyway, and keeping bound
/// strings metadata-free lets the round-trip property compare normalised
/// forms directly.)
fn version_string() -> impl Strategy<Value = String> {
    let component =
        prop_oneof![4 => 0u32..20, 1 => Just(i32::MAX as u32), 2 => 0u32..=i32::MAX as u32];
    let label = proptest::string::string_regex("[0-9A-Za-z-]{1,6}")
        .expect("valid regex")
        .prop_filter("no leading-zero numeric labels", |l| {
            !(l.len() > 1 && l.as_bytes()[0] == b'0' && l.bytes().all(|b| b.is_ascii_digit()))
        });
    (
        proptest::collection::vec(component, 1..=4),
        proptest::collection::vec(label, 0..3),
    )
        .prop_map(|(nums, labels)| {
            let mut s = nums
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(".");
            if !labels.is_empty() {
                s.push('-');
                s.push_str(&labels.join("."));
            }
            s
        })
}

fn v(s: &str) -> NuGetVersion {
    NuGetVersion::parse(s).unwrap_or_else(|e| panic!("{s:?} should parse: {e}"))
}

/// What `satisfies` must mean, computed straight from the accessors.
fn bounds_formula(range: &VersionRange, version: &NuGetVersion) -> bool {
    let lower_ok = match range.min_version() {
        None => true,
        Some(min) => match version.cmp(min) {
            Ordering::Less => false,
            Ordering::Equal => range.is_min_inclusive(),
            Ordering::Greater => true,
        },
    };
    let upper_ok = match range.max_version() {
        None => true,
        Some(max) => match version.cmp(max) {
            Ordering::Greater => false,
            Ordering::Equal => range.is_max_inclusive(),
            Ordering::Less => true,
        },
    };
    lower_ok && upper_ok
}

proptest! {
    /// A bracketed range built from two ordered versions parses, and
    /// `satisfies` matches the bounds formula for arbitrary probes —
    /// including the exact endpoints.
    #[test]
    fn satisfies_matches_bounds(
        a in version_string(),
        b in version_string(),
        probe in version_string(),
        min_incl in any::<bool>(),
        max_incl in any::<bool>(),
    ) {
        let (va, vb) = (v(&a), v(&b));
        // "[x, x)" / "(x, x]" are rejected by design (mixed inclusivity on
        // equal bounds), and comparer-equal versions can arise from
        // distinct strings ("0" vs "0.0") — skip that corner.
        prop_assume!(va != vb || min_incl == max_incl);
        let (lo, hi) = if va <= vb { (&a, &b) } else { (&b, &a) };
        let (lo_v, hi_v) = if va <= vb { (&va, &vb) } else { (&vb, &va) };
        let rendered = format!(
            "{}{lo}, {hi}{}",
            if min_incl { '[' } else { '(' },
            if max_incl { ']' } else { ')' },
        );
        let range = VersionRange::parse(&rendered)
            .unwrap_or_else(|e| panic!("{rendered:?} should parse: {e}"));
        for candidate in [&v(&probe), lo_v, hi_v] {
            prop_assert_eq!(
                range.satisfies(candidate),
                bounds_formula(&range, candidate),
                "range {} probe {}",
                rendered,
                candidate
            );
        }
    }

    /// Bare version = inclusive minimum, nothing else.
    #[test]
    fn bare_version_is_minimum(a in version_string(), probe in version_string()) {
        let range = VersionRange::parse(&a).unwrap_or_else(|e| panic!("{a:?}: {e}"));
        let (va, vp) = (v(&a), v(&probe));
        prop_assert_eq!(range.satisfies(&vp), vp >= va);
    }

    /// Ranges are convex: x <= y <= z with x and z inside puts y inside.
    #[test]
    fn ranges_are_convex(
        a in version_string(),
        b in version_string(),
        p1 in version_string(),
        p2 in version_string(),
        p3 in version_string(),
        min_incl in any::<bool>(),
        max_incl in any::<bool>(),
    ) {
        let (va, vb) = (v(&a), v(&b));
        // "[x, x)" / "(x, x]" are rejected by design (mixed inclusivity on
        // equal bounds), and comparer-equal versions can arise from
        // distinct strings ("0" vs "0.0") — skip that corner.
        prop_assume!(va != vb || min_incl == max_incl);
        let (lo, hi) = if va <= vb { (a.clone(), b) } else { (b, a) };
        let rendered = format!(
            "{}{lo}, {hi}{}",
            if min_incl { '[' } else { '(' },
            if max_incl { ']' } else { ')' },
        );
        let range = VersionRange::parse(&rendered)
            .unwrap_or_else(|e| panic!("{rendered:?} should parse: {e}"));
        let mut probes = [v(&p1), v(&p2), v(&p3)];
        probes.sort();
        let [x, y, z] = probes;
        if range.satisfies(&x) && range.satisfies(&z) {
            prop_assert!(range.satisfies(&y), "range {rendered} not convex at {y}");
        }
    }

    /// parse ∘ normalise is the identity: same structure, same normalised
    /// string.
    #[test]
    fn normalised_round_trip(
        a in version_string(),
        b in version_string(),
        min_incl in any::<bool>(),
        max_incl in any::<bool>(),
    ) {
        let (va, vb) = (v(&a), v(&b));
        // "[x, x)" / "(x, x]" are rejected by design (mixed inclusivity on
        // equal bounds), and comparer-equal versions can arise from
        // distinct strings ("0" vs "0.0") — skip that corner.
        prop_assume!(va != vb || min_incl == max_incl);
        let (lo, hi) = if va <= vb { (a.clone(), b) } else { (b, a) };
        let rendered = format!(
            "{}{lo}, {hi}{}",
            if min_incl { '[' } else { '(' },
            if max_incl { ']' } else { ')' },
        );
        let range = VersionRange::parse(&rendered)
            .unwrap_or_else(|e| panic!("{rendered:?} should parse: {e}"));
        let n = range.to_normalized_string();
        let reparsed = VersionRange::parse(&n)
            .unwrap_or_else(|e| panic!("normalised {n:?} should reparse: {e}"));
        prop_assert_eq!(&reparsed, &range);
        prop_assert_eq!(reparsed.to_normalized_string(), n);
    }
}
