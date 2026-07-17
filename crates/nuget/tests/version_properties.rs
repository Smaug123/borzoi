//! Property tests for `NuGetVersion`, pure Rust side (no oracle): round
//! trips, total-order laws, and the deliberate SemVer deviations (metadata-
//! and case-insensitive equality). The *authority* on NuGet fidelity is the
//! oracle diff in `version_diff.rs`; these pin the algebra.

use borzoi_nuget::NuGetVersion;
use proptest::prelude::*;
use std::cmp::Ordering;
use std::hash::{DefaultHasher, Hash, Hasher};

/// The structured description of a valid version string: 1–4 numeric parts,
/// optional release labels, optional metadata.
#[derive(Debug, Clone)]
struct Parts {
    nums: Vec<u32>,
    labels: Vec<String>,
    metadata: Option<String>,
}

impl Parts {
    fn render(&self) -> String {
        let mut s = self
            .nums
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".");
        if !self.labels.is_empty() {
            s.push('-');
            s.push_str(&self.labels.join("."));
        }
        if let Some(m) = &self.metadata {
            s.push('+');
            s.push_str(m);
        }
        s
    }
}

fn component() -> impl Strategy<Value = u32> {
    prop_oneof![
        4 => 0u32..20,
        1 => Just(i32::MAX as u32),
        2 => 0u32..=i32::MAX as u32,
    ]
}

fn label() -> impl Strategy<Value = String> {
    // Includes purely-numeric and '-'-bearing labels — but not all-digit
    // labels with a leading zero, which NuGet rejects at parse (the strict
    // SemVer rule; the oracle diff pins that).
    proptest::string::string_regex("[0-9A-Za-z-]{1,8}")
        .expect("valid regex")
        .prop_filter("no leading-zero numeric labels", |l| {
            !(l.len() > 1 && l.as_bytes()[0] == b'0' && l.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn parts() -> impl Strategy<Value = Parts> {
    (
        proptest::collection::vec(component(), 1..=4),
        proptest::collection::vec(label(), 0..4),
        proptest::option::of(proptest::collection::vec(label(), 1..=3)),
    )
        .prop_map(|(nums, labels, metadata)| Parts {
            nums,
            labels,
            metadata: metadata.map(|m| m.join(".")),
        })
}

fn parse(s: &str) -> NuGetVersion {
    NuGetVersion::parse(s).unwrap_or_else(|e| panic!("{s:?} should parse: {e}"))
}

fn hash_of(v: &NuGetVersion) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

proptest! {
    /// Every structurally-valid string parses.
    #[test]
    fn constructed_strings_parse(p in parts()) {
        parse(&p.render());
    }

    /// parse ∘ normalise is the identity on parsed values, and normalisation
    /// is idempotent.
    #[test]
    fn normalised_round_trip(p in parts()) {
        let v = parse(&p.render());
        let n = v.to_normalized_string();
        let reparsed = parse(&n);
        prop_assert_eq!(&reparsed, &v);
        prop_assert_eq!(reparsed.to_normalized_string(), n);
    }

    /// Equality, ordering, and hashing all ignore build metadata.
    #[test]
    fn metadata_is_invisible(p in parts(), meta in proptest::collection::vec(label(), 1..=3)) {
        let bare = Parts { metadata: None, ..p.clone() };
        let with = Parts { metadata: Some(meta.join(".")), ..p };
        let (a, b) = (parse(&bare.render()), parse(&with.render()));
        prop_assert_eq!(&a, &b);
        prop_assert_eq!(a.cmp(&b), Ordering::Equal);
        prop_assert_eq!(hash_of(&a), hash_of(&b));
    }

    /// Equality, ordering, and hashing ignore release-label ASCII case.
    #[test]
    fn label_case_is_invisible(p in parts()) {
        let upper = Parts {
            labels: p.labels.iter().map(|l| l.to_ascii_uppercase()).collect(),
            ..p.clone()
        };
        let lower = Parts {
            labels: p.labels.iter().map(|l| l.to_ascii_lowercase()).collect(),
            ..p
        };
        let (a, b) = (parse(&upper.render()), parse(&lower.render()));
        prop_assert_eq!(&a, &b);
        prop_assert_eq!(a.cmp(&b), Ordering::Equal);
        prop_assert_eq!(hash_of(&a), hash_of(&b));
    }

    /// Total-order laws: reflexivity, antisymmetry, transitivity, and
    /// Eq-consistency, over arbitrary triples.
    #[test]
    fn order_laws(pa in parts(), pb in parts(), pc in parts()) {
        let a = parse(&pa.render());
        let b = parse(&pb.render());
        let c = parse(&pc.render());
        prop_assert_eq!(a.cmp(&a), Ordering::Equal);
        prop_assert_eq!(a.cmp(&b), b.cmp(&a).reverse());
        if a <= b && b <= c {
            prop_assert!(a <= c);
        }
        prop_assert_eq!(a == b, a.cmp(&b) == Ordering::Equal);
    }

    /// A stable version outranks every prerelease of the same numbers.
    #[test]
    fn stable_beats_prerelease(p in parts(), extra in proptest::collection::vec(label(), 1..=3)) {
        let stable = Parts { labels: vec![], metadata: None, ..p.clone() };
        let pre = Parts { labels: extra, metadata: None, ..p };
        prop_assert!(parse(&stable.render()) > parse(&pre.render()));
    }
}
