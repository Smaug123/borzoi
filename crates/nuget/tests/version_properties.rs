//! Property tests for `NuGetVersion`, pure Rust side (no oracle): round
//! trips, total-order laws, and the deliberate SemVer deviations (metadata-
//! and case-insensitive equality). The *authority* on NuGet fidelity is the
//! oracle diff in `version_diff.rs`; these pin the algebra.
//!
//! The algebra is load-bearing beyond the Rust `Ord` contract. The resolver
//! settles each package at the greatest lower bound of its surviving edges
//! with `Iterator::max`, and `VersionRange` membership is a pair of bound
//! comparisons; under a comparator that is merely *mostly* transitive,
//! `max` returns whichever element the iteration order happened to favour
//! and a range admits or rejects a version according to how its bounds were
//! spelled. Both are wrong answers delivered quietly. The
//! shapes that make a mixed numeric/string rule non-transitive in general —
//! a label that parses as `Int32` compares numerically, one that overflows
//! degrades to a string — are exercised deliberately, from
//! [`common::COMPARATOR_POOL`]; see [`laws_hold_over_every_triple_of_the_pool`]
//! for the exhaustive check and
//! [`the_pooled_generator_reaches_the_adversarial_corners`] for the evidence
//! that the random side reaches them too.

mod common;

use borzoi_nuget::NuGetVersion;
use common::COMPARATOR_POOL;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
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

    /// The full law set over triples from the *wide* generator: it reaches
    /// spellings the small pool never emits, at the cost of relating its
    /// draws only rarely (which is what the pooled test below is for).
    #[test]
    fn order_laws(pa in parts(), pb in parts(), pc in parts()) {
        check_laws(&cases([pa.render(), pb.render(), pc.render()]));
    }

    /// A stable version outranks every prerelease of the same numbers.
    #[test]
    fn stable_beats_prerelease(p in parts(), extra in proptest::collection::vec(label(), 1..=3)) {
        let stable = Parts { labels: vec![], metadata: None, ..p.clone() };
        let pre = Parts { labels: extra, metadata: None, ..p };
        prop_assert!(parse(&stable.render()) > parse(&pre.render()));
    }

    /// The full law set over a set of versions drawn from the small shared
    /// pool, so the triples genuinely relate. Every set is checked at every
    /// pair and every triple, ordered *and* unordered.
    #[test]
    fn order_laws_over_a_pooled_set(spellings in pooled_set(3..=5)) {
        check_laws(&cases(spellings));
    }
}

// ============================================================================
// The total-order laws
// ============================================================================

/// One version under test: how it was spelled, what it parsed to, and the
/// reparse of its normalised string (so normalisation's effect on ordering
/// is checked by the same pass).
struct Case {
    spelling: String,
    version: NuGetVersion,
    renormalised: NuGetVersion,
}

fn cases(spellings: impl IntoIterator<Item = String>) -> Vec<Case> {
    spellings
        .into_iter()
        .map(|spelling| {
            let version = parse(&spelling);
            let renormalised = parse(&version.to_normalized_string());
            Case {
                spelling,
                version,
                renormalised,
            }
        })
        .collect()
}

/// How often [`check_laws`] met each configuration that makes a law say
/// something. A law whose antecedent is never satisfied is a law that was
/// never tested, so these are asserted rather than trusted: exactly for the
/// fixed pool, against floors for the sampled sets.
#[derive(Default, Debug, Clone, Copy)]
struct Coverage {
    /// Pairs `(a, b)` with `a < b` — antisymmetry's non-trivial side.
    ordered_pairs: u64,
    /// Pairs that compare equal despite being spelled differently.
    equal_distinct_spellings: u64,
    /// Pairs that compare equal but are *not* `eq_strict`, i.e. the pairs on
    /// which NuGet's own `Compare` and `Equals` disagree.
    equal_but_not_strictly: u64,
    /// Triples satisfying `<`-transitivity's antecedent, `a < b && b < c`.
    lt_chains: u64,
    /// Triples satisfying `==`-transitivity's antecedent with at least two
    /// distinct spellings involved (`a == b && b == c` on three copies of one
    /// string proves nothing).
    eq_chains: u64,
}

impl Coverage {
    fn add(&mut self, other: Coverage) {
        self.ordered_pairs += other.ordered_pairs;
        self.equal_distinct_spellings += other.equal_distinct_spellings;
        self.equal_but_not_strictly += other.equal_but_not_strictly;
        self.lt_chains += other.lt_chains;
        self.eq_chains += other.eq_chains;
    }
}

/// Every total-order law, checked at every pair and every triple of `cases`
/// (including the diagonal). Panics naming the offending tuple; returns what
/// the check actually managed to exercise.
///
/// The laws, and why each is worth stating for this type:
///
/// - **Reflexivity**, **trichotomy** (exactly one of `<`, `==`, `>`) and
///   **antisymmetry**. Trichotomy is not free: it is where `PartialOrd`,
///   `PartialEq` and `Ord` are forced to agree with each other.
/// - **Transitivity of `<`** — the headline. A comparator that compares two
///   numeric labels numerically but a numeric against a string one
///   *by string* would fail here; NuGet instead ranks every numeric label
///   below every alphanumeric one, uniformly, which is what saves it.
/// - **Transitivity of `==`**, i.e. equality is an equivalence. Non-obvious
///   because equality is the conjunction of two different coarsenings:
///   case-insensitive for string labels, `int.TryParse` value for numeric
///   ones, so `1.0--0 == 1.0-0` and `1.0-A == 1.0-a`.
/// - **`Ord`/`Eq` consistency** and **`Hash`/`Eq` consistency**, the two
///   contracts `BTreeMap` and `HashMap` respectively rely on.
/// - **Normalisation preserves the order**: reparsing both sides'
///   `to_normalized_string()` gives the same verdict. Normalisation drops
///   metadata and a zero revision, so this says the dropped parts really are
///   invisible to the comparator.
/// - **`eq_strict` refines `==`** (NuGet's `Equals` implies its `Compare`
///   agreeing), and is itself an equivalence — it is what the caller keys
///   "the same version of the same package" on.
fn check_laws(cases: &[Case]) -> Coverage {
    let mut cov = Coverage::default();

    for a in cases {
        // Identical operands are the whole point here: reflexivity is a law,
        // so `clippy::eq_op`'s usual reading — that one side is a typo for
        // something else — does not apply.
        #[allow(clippy::eq_op)]
        {
            assert_eq!(
                a.version.cmp(&a.version),
                Ordering::Equal,
                "cmp is not reflexive at {:?}",
                a.spelling
            );
            assert!(
                a.version == a.version,
                "== is not reflexive at {:?}",
                a.spelling
            );
            assert!(
                a.version.eq_strict(&a.version),
                "eq_strict is not reflexive at {:?}",
                a.spelling
            );
        }
    }

    for a in cases {
        for b in cases {
            let (x, y) = (&a.version, &b.version);
            let (sa, sb) = (&a.spelling, &b.spelling);

            let trichotomy = u8::from(x < y) + u8::from(x == y) + u8::from(x > y);
            assert_eq!(
                trichotomy,
                1,
                "trichotomy fails for {sa:?} vs {sb:?}: <={} =={} >={}",
                x < y,
                x == y,
                x > y
            );
            assert_eq!(
                x.cmp(y),
                y.cmp(x).reverse(),
                "antisymmetry fails for {sa:?} vs {sb:?}"
            );
            assert_eq!(
                x.cmp(y) == Ordering::Equal,
                x == y,
                "Ord/Eq disagree for {sa:?} vs {sb:?}"
            );
            if x == y {
                assert_eq!(
                    hash_of(x),
                    hash_of(y),
                    "Hash/Eq disagree for {sa:?} vs {sb:?}"
                );
            }
            assert_eq!(
                a.renormalised.cmp(&b.renormalised),
                x.cmp(y),
                "normalisation moved the order of {sa:?} vs {sb:?} \
                 ({:?} vs {:?})",
                x.to_normalized_string(),
                y.to_normalized_string()
            );
            if x.eq_strict(y) {
                assert!(x == y, "eq_strict does not imply == for {sa:?} vs {sb:?}");
            }
            assert_eq!(
                x.eq_strict(y),
                y.eq_strict(x),
                "eq_strict is not symmetric for {sa:?} vs {sb:?}"
            );

            if x < y {
                cov.ordered_pairs += 1;
            }
            if x == y && sa != sb {
                cov.equal_distinct_spellings += 1;
                if !x.eq_strict(y) {
                    cov.equal_but_not_strictly += 1;
                }
            }
        }
    }

    for a in cases {
        for b in cases {
            for c in cases {
                let (x, y, z) = (&a.version, &b.version, &c.version);
                if x < y && y < z {
                    cov.lt_chains += 1;
                    assert!(
                        x < z,
                        "transitivity of < fails: {:?} < {:?} < {:?} but not {:?} < {:?}",
                        a.spelling,
                        b.spelling,
                        c.spelling,
                        a.spelling,
                        c.spelling
                    );
                }
                if x == y && y == z {
                    if a.spelling != b.spelling || b.spelling != c.spelling {
                        cov.eq_chains += 1;
                    }
                    assert!(
                        x == z,
                        "transitivity of == fails: {:?} == {:?} == {:?} but not {:?} == {:?}",
                        a.spelling,
                        b.spelling,
                        c.spelling,
                        a.spelling,
                        c.spelling
                    );
                }
                if x.eq_strict(y) && y.eq_strict(z) {
                    assert!(
                        x.eq_strict(z),
                        "transitivity of eq_strict fails: {:?}, {:?}, {:?}",
                        a.spelling,
                        b.spelling,
                        c.spelling
                    );
                }
            }
        }
    }

    cov
}

// ============================================================================
// The small shared pool, and the evidence that it reaches the corners
// ============================================================================

/// Numeric sections, drawn from a handful of spellings so that two draws
/// collide often. Four of these denote the same version.
const POOL_NUMS: &[&str] = &["1", "1.0", "1.0.0", "1.0.0.0", "1.0.0.1", "1.0.1", "2.0.0"];

/// Release-label / metadata segments: numeric (including negative and
/// leading-zero-behind-a-dash spellings), `Int32`-overflowing at both ends,
/// alphanumeric, and case pairs.
const POOL_LABELS: &[&str] = &[
    "0",
    "-0",
    "-00",
    "1",
    "-1",
    "-01",
    "2",
    "10",
    "2147483647",
    "-2147483648",
    "2147483648",
    "-2147483649",
    "99999999999999999999",
    "a",
    "A",
    "b",
    "-",
    "a-b",
    "a1",
    "1a",
    "alpha",
    "ALPHA",
];

/// A version string built entirely from the pools, so any two draws are
/// likely to share a numeric section and to differ only in their labels —
/// the regime in which transitivity has something to say.
fn pooled_version() -> impl Strategy<Value = String> {
    (
        prop::sample::select(POOL_NUMS),
        prop::collection::vec(prop::sample::select(POOL_LABELS), 0..3),
        prop::option::of(prop::sample::select(POOL_LABELS)),
    )
        .prop_map(|(nums, labels, metadata)| {
            let mut s = nums.to_owned();
            if !labels.is_empty() {
                s.push('-');
                s.push_str(&labels.join("."));
            }
            if let Some(m) = metadata {
                s.push('+');
                s.push_str(m);
            }
            s
        })
}

fn pooled_set(size: std::ops::RangeInclusive<usize>) -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(pooled_version(), size)
}

/// Every triple from [`COMPARATOR_POOL`], exhaustively. Random sampling is
/// the wrong tool for a rule whose defects live in a handful of specific
/// label pairs; the pool is 46 entries, so all 97,336 triples cost
/// milliseconds and there is no seed to get unlucky with.
#[test]
fn laws_hold_over_every_triple_of_the_pool() {
    let cases = cases(COMPARATOR_POOL.iter().map(|s| (*s).to_owned()));
    let cov = check_laws(&cases);
    println!("pool coverage: {cov:?}");

    // Exact, not floors: the pool is fixed and the comparator deterministic,
    // so any movement here is a real change in the comparator or the pool and
    // should be looked at, in either direction. Editing the pool means
    // re-deriving these — that is the point.
    assert_eq!(
        (
            cov.ordered_pairs,
            cov.equal_distinct_spellings,
            cov.equal_but_not_strictly,
            cov.lt_chains,
            cov.eq_chains
        ),
        (1_008, 54, 8, 14_064, 378),
        "pool coverage moved: {cov:?}"
    );
}

/// A label as `VersionComparer` keys it, computed independently of the
/// implementation under test: `Some(n)` when `int.TryParse` would succeed,
/// `None` when the comparison degrades to a case-insensitive string.
fn label_as_int(label: &str) -> Option<i32> {
    let (negative, digits) = match label.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, label),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // `i128` so a 20-digit label is *evaluated* rather than rejected for
    // overflowing the accumulator; anything past `i128` overflows `Int32`
    // too, so falling through to `None` is the same answer.
    let magnitude: i128 = digits.parse().ok()?;
    i32::try_from(if negative { -magnitude } else { magnitude }).ok()
}

/// Does this label look numeric but overflow `Int32`, so that NuGet compares
/// it as a string? This is the shape the whole exercise is hunting.
fn is_overflowing_numeric(label: &str) -> bool {
    let digits = label.strip_prefix('-').unwrap_or(label);
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && label_as_int(label).is_none()
}

/// The non-vacuity gate on [`pooled_version`]: sample it and assert that the
/// corners actually turn up, and that the law checks over the sampled sets
/// satisfy their antecedents often. A generator that stopped reaching these
/// would leave `order_laws_over_a_pooled_set` green and meaningless.
///
/// Deterministic — a fixed-seed `TestRng`, so this cannot flake; the floors
/// sit at roughly half the observed values, which is headroom for a proptest
/// version bump reshuffling the RNG, not a tolerance for randomness.
#[test]
fn the_pooled_generator_reaches_the_adversarial_corners() {
    const SETS: usize = 5_000;
    const SET_SIZE: usize = 4;

    let mut runner = TestRunner::new_with_rng(
        Config::default(),
        TestRng::deterministic_rng(RngAlgorithm::ChaCha),
    );
    let strategy = pooled_set(SET_SIZE..=SET_SIZE);

    let mut cov = Coverage::default();
    let (mut versions, mut with_overflow_label, mut with_negative_label) = (0u64, 0u64, 0u64);
    let (mut multi_segment, mut legacy_revision, mut with_metadata) = (0u64, 0u64, 0u64);

    for _ in 0..SETS {
        let spellings = strategy
            .new_tree(&mut runner)
            .expect("pooled generator produces a value")
            .current();
        let cases = cases(spellings);
        for case in &cases {
            versions += 1;
            let labels = case.version.release_labels();
            if labels.iter().any(|l| is_overflowing_numeric(l)) {
                with_overflow_label += 1;
            }
            if labels
                .iter()
                .any(|l| label_as_int(l).is_some_and(|n| n < 0))
            {
                with_negative_label += 1;
            }
            if labels.len() > 1 {
                multi_segment += 1;
            }
            if case.version.revision() != 0 {
                legacy_revision += 1;
            }
            if case.version.metadata().is_some() {
                with_metadata += 1;
            }
        }
        cov.add(check_laws(&cases));
    }

    println!(
        "sampled {versions} versions from {SETS} sets: \
         overflow-label {with_overflow_label}, negative-label {with_negative_label}, \
         multi-segment {multi_segment}, legacy-revision {legacy_revision}, \
         metadata {with_metadata}; law coverage {cov:?}"
    );

    // Observed at the pinned proptest version, in the comment beside each
    // floor. The rarest corner by far is a pair that compares equal without
    // being `eq_strict` (it needs two spellings of one integer facing each
    // other at the same label position), which is why it gets a floor of its
    // own rather than riding on the equal-pair count.
    let floors: &[(&str, u64, u64)] = &[
        ("versions", versions, (SETS * SET_SIZE) as u64),
        ("overflow-label", with_overflow_label, 1_200), // 2,528
        ("negative-label", with_negative_label, 1_300), // 2,658
        ("multi-segment", multi_segment, 3_300),        // 6,749
        ("legacy-revision", legacy_revision, 1_400),    // 2,826
        ("metadata", with_metadata, 5_000),             // 10,041
        ("ordered pairs", cov.ordered_pairs, 14_000),   // 28,635
        (
            "equal distinct spellings",
            cov.equal_distinct_spellings,
            1_200,
        ), // 2,454
        ("equal but not strictly", cov.equal_but_not_strictly, 20), // 42
        ("< chains", cov.lt_chains, 8_000),             // 17,536
        ("== chains", cov.eq_chains, 4_000),            // 8,142
    ];
    for (name, observed, floor) in floors {
        assert!(
            observed >= floor,
            "the pooled generator no longer reaches {name}: {observed} < {floor}"
        );
    }
}
