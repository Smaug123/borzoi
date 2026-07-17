//! Pure MSBuild-style glob matching for `.fsproj` item includes.
//!
//! This is the policy half of the `borzoi-msbuild` glob *seam*
//! (`GlobResolver`): the `msbuild` core deliberately stays
//! filesystem-free and dependency-light, so the LSP shell owns glob
//! semantics and ordering. This module is the *pure* core of that —
//! pattern parsing and matching against relative paths, plus a
//! deterministic selection over a candidate set. The filesystem
//! enumeration that produces those candidates, and the wiring into the
//! parser, live separately (phase 9b-2).
//!
//! ## Semantics modelled
//!
//! - `*` matches zero or more characters *within a single path segment*
//!   (never crosses `/`).
//! - `?` matches exactly one character within a segment.
//! - `**`, as a *whole* segment, matches zero or more whole path
//!   segments (recursive). `**` embedded in a larger segment (e.g.
//!   `a**b`) is treated as ordinary `*` wildcards, not recursion.
//! - Matching is **case-sensitive** and paths are compared with `/`
//!   separators (backslashes normalise to `/`, runs of `/` collapse, and
//!   lone `.` current-directory segments are dropped). `..` is left for
//!   the filesystem layer to resolve against the base directory (9b-2).
//!   Case-sensitivity is the dominant Linux-CI / agent behaviour; it
//!   diverges from MSBuild on case-insensitive filesystems. This and the
//!   embedded-`**` rule are pinned against real `dotnet msbuild` by the
//!   oracle diff test in phase 9b-2.
//!
//! [`select`] is an information-preserving primitive: it orders but does
//! not deduplicate. Across Include fragments it keeps document order; and
//! *within* one fragment's expansion it sorts **lexicographically** by the
//! `/`-normalised relative path — our deterministic, platform-independent
//! stand-in for MSBuild's filesystem-dependent enumeration order. Whether
//! the final Compile list folds duplicates from overlapping fragments is a
//! faithfulness decision deferred to the 9b-2 resolver and its oracle.
//!
//! ## Testing
//!
//! The matcher is property-tested against an independent naive reference
//! oracle (`matches_naive`). That proves the two agree, catching
//! implementation bugs — but *not* MSBuild-faithfulness (both could share
//! a wrong assumption). MSBuild-faithfulness is a separate concern pinned
//! by the 9b-2 `dotnet msbuild` oracle.

/// One token inside a single-segment pattern piece.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    /// A literal character.
    Lit(char),
    /// `*` — zero or more non-`/` characters.
    Star,
    /// `?` — exactly one character.
    Question,
}

/// One segment of a compiled pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    /// Matches exactly one path segment via this token sequence.
    Match(Vec<Tok>),
    /// `**` — matches zero or more whole path segments.
    DoubleStar,
}

/// A compiled MSBuild include/exclude pattern, relative to a base dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    segs: Vec<Seg>,
}

/// Normalise a path/pattern fragment: backslashes to `/`, collapse runs
/// of `/`, drop empty and lone-`.` (current-directory) segments. Returns
/// the surviving segments. `..` is *not* resolved here — that needs the
/// base directory and is the filesystem layer's job (9b-2); candidates and
/// patterns reaching this matcher are expected base-relative without `..`.
pub fn split_segments(raw: &str) -> Vec<&str> {
    raw.split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .collect()
}

impl Pattern {
    /// Parse one already-expanded include/exclude fragment. A segment
    /// equal exactly to `**` becomes [`Seg::DoubleStar`]; otherwise each
    /// character becomes a [`Tok`] (`*`/`?`/literal).
    pub fn parse(fragment: &str) -> Pattern {
        let segs = split_segments(fragment)
            .into_iter()
            .map(|s| {
                if s == "**" {
                    Seg::DoubleStar
                } else {
                    Seg::Match(
                        s.chars()
                            .map(|c| match c {
                                '*' => Tok::Star,
                                '?' => Tok::Question,
                                other => Tok::Lit(other),
                            })
                            .collect(),
                    )
                }
            })
            .collect();
        Pattern { segs }
    }

    /// Build a pattern whose leading `literal` segments match **verbatim**
    /// — any `*`/`?`/`**` in them are ordinary characters, not wildcards —
    /// followed by the glob segments parsed from `fragment`.
    ///
    /// This anchors a glob at a *known-literal* base directory. The resolver
    /// passes the project directory (which may legitimately contain `*`/`?`
    /// on a case-sensitive Unix filesystem) as `literal`, so those bytes stay
    /// literal instead of silently turning the base path into a wildcard that
    /// would match sibling directories. `literal` segments are taken as
    /// already-split path components; each is normalised again so a caller
    /// may pass either pre-split segments or `/`-joined ones.
    pub fn with_literal_prefix(literal: &[&str], fragment: &str) -> Pattern {
        let mut segs: Vec<Seg> = literal
            .iter()
            .flat_map(|s| split_segments(s))
            .map(|s| Seg::Match(s.chars().map(Tok::Lit).collect()))
            .collect();
        segs.extend(Pattern::parse(fragment).segs);
        Pattern { segs }
    }

    /// Whether this pattern contains any wildcard (`*`, `?`, or `**`).
    /// A pure-literal fragment takes the no-filesystem passthrough in the
    /// resolver (9b-2).
    pub fn is_glob(&self) -> bool {
        self.segs.iter().any(|seg| match seg {
            Seg::DoubleStar => true,
            Seg::Match(toks) => toks.iter().any(|t| matches!(t, Tok::Star | Tok::Question)),
        })
    }

    /// Match against a relative candidate path (normalised internally).
    pub fn matches(&self, candidate: &str) -> bool {
        let path = split_segments(candidate);
        path_match(&self.segs, &path)
    }
}

/// The fixed (wildcard-free) leading directory of a glob fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobRoot {
    /// The leading wildcard-free segments, normalised (separators unified,
    /// empty/`.` dropped, `..` preserved). Empty when the first segment is
    /// already a wildcard (e.g. `*.fs`, `**/x.fs`). The resolver roots its
    /// filesystem walk here; absoluteness (a leading `/`) is recovered from
    /// the raw fragment, not from these segments.
    pub prefix: Vec<String>,
    /// The segment-depth of the wildcard tail that follows the prefix, or
    /// `None` when that tail contains `**` (unbounded). The resolver walks
    /// from [`GlobRoot::prefix`] to this depth.
    pub tail_depth: Option<usize>,
}

/// Split a fragment into its fixed leading directory and the wildcard tail
/// that follows. `../shared/*.fs` splits into prefix `["..", "shared"]` and
/// a depth-1 tail, so the resolver can enumerate the sibling directory
/// rather than the project directory.
pub fn split_glob_root(fragment: &str) -> GlobRoot {
    let segs = split_segments(fragment);
    let is_wild = |s: &str| s.contains('*') || s.contains('?');
    let prefix_len = segs.iter().take_while(|s| !is_wild(s)).count();
    let prefix = segs[..prefix_len].iter().map(|s| s.to_string()).collect();
    let tail = &segs[prefix_len..];
    let tail_depth = if tail.contains(&"**") {
        None
    } else {
        Some(tail.len())
    };
    GlobRoot { prefix, tail_depth }
}

/// Iterative two-pointer wildcard match of one segment's tokens against a
/// string's characters: `*` spans 0+ chars, `?` exactly one. Distinct in
/// structure from the recursive test oracle so their agreement is
/// meaningful.
fn seg_match(toks: &[Tok], s: &[char]) -> bool {
    let (mut i, mut j) = (0usize, 0usize);
    // (token index just after the last `*`, candidate index it was at)
    let mut star: Option<(usize, usize)> = None;
    while j < s.len() {
        match toks.get(i) {
            Some(Tok::Question) => {
                i += 1;
                j += 1;
            }
            Some(Tok::Lit(c)) if *c == s[j] => {
                i += 1;
                j += 1;
            }
            Some(Tok::Star) => {
                star = Some((i, j));
                i += 1;
            }
            _ => match star {
                Some((si, sj)) => {
                    i = si + 1;
                    j = sj + 1;
                    star = Some((si, sj + 1));
                }
                None => return false,
            },
        }
    }
    while matches!(toks.get(i), Some(Tok::Star)) {
        i += 1;
    }
    i == toks.len()
}

/// Iterative two-pointer match at segment granularity, with
/// [`Seg::DoubleStar`] spanning 0+ whole path segments.
fn path_match(segs: &[Seg], path: &[&str]) -> bool {
    let (mut i, mut j) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while j < path.len() {
        let advanced = match segs.get(i) {
            Some(Seg::DoubleStar) => {
                star = Some((i, j));
                i += 1;
                true
            }
            Some(Seg::Match(toks)) => {
                let sc: Vec<char> = path[j].chars().collect();
                if seg_match(toks, &sc) {
                    i += 1;
                    j += 1;
                    true
                } else {
                    false
                }
            }
            None => false,
        };
        if !advanced {
            match star {
                Some((si, sj)) => {
                    i = si + 1;
                    j = sj + 1;
                    star = Some((si, sj + 1));
                }
                None => return false,
            }
        }
    }
    while matches!(segs.get(i), Some(Seg::DoubleStar)) {
        i += 1;
    }
    i == segs.len()
}

/// Select the candidates matched by `includes` and not by any `exclude`.
///
/// This is an *information-preserving* primitive: it matches and orders,
/// but does **not** deduplicate. The `includes` are processed in document
/// (fragment) order — each fragment's matches appear before the next
/// fragment's, even if a later fragment sorts lower lexicographically.
/// *Within* one fragment's expansion the matches are sorted
/// lexicographically (our deterministic, platform-independent stand-in for
/// MSBuild's filesystem enumeration order).
///
/// Overlapping fragments therefore yield duplicates (a literal also caught
/// by a later glob appears twice). MSBuild item evaluation keeps such
/// duplicates by default; whether and how the final Compile list should
/// fold them is a faithfulness decision left to the 9b-2 resolver and its
/// `dotnet msbuild` oracle — `select` keeps everything so that policy can
/// be applied (or not) downstream. The 9b-2 filesystem enumerator is
/// expected to list each file once, so within a single fragment no
/// duplicates arise.
///
/// Output paths are `/`-normalised (backslashes → `/`, runs of `/`
/// collapsed, lone `.` dropped) so the order is platform-independent even
/// when candidates arrive with OS-native separators.
pub fn select(candidates: &[&str], includes: &[Pattern], excludes: &[Pattern]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for inc in includes {
        let mut matched: Vec<String> = candidates
            .iter()
            .filter(|c| inc.matches(c) && !excludes.iter().any(|p| p.matches(c)))
            .map(|c| split_segments(c).join("/"))
            .collect();
        matched.sort();
        out.append(&mut matched);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;

    // ----- Reference oracle (deliberately naive) -----

    /// Naive within-segment match: `*` = 0+ chars, `?` = exactly 1,
    /// else literal. Operates on char slices; backtracks on `*`.
    fn seg_match_naive(pat: &[char], s: &[char]) -> bool {
        match pat.split_first() {
            None => s.is_empty(),
            Some((&'*', rest)) => (0..=s.len()).any(|i| seg_match_naive(rest, &s[i..])),
            Some((&'?', rest)) => !s.is_empty() && seg_match_naive(rest, &s[1..]),
            Some((&c, rest)) => s.first() == Some(&c) && seg_match_naive(rest, &s[1..]),
        }
    }

    /// Naive whole-path match. A pattern segment equal exactly to `"**"`
    /// matches 0+ path segments; any other segment matches exactly one.
    fn path_match_naive(pat: &[&str], path: &[&str]) -> bool {
        match pat.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => (0..=path.len()).any(|i| path_match_naive(rest, &path[i..])),
            Some((&p, rest)) => {
                if path.is_empty() {
                    return false;
                }
                let pc: Vec<char> = p.chars().collect();
                let sc: Vec<char> = path[0].chars().collect();
                seg_match_naive(&pc, &sc) && path_match_naive(rest, &path[1..])
            }
        }
    }

    /// Independent ground-truth matcher over raw strings.
    fn matches_naive(pattern: &str, path: &str) -> bool {
        let p = split_segments(pattern);
        let s = split_segments(path);
        path_match_naive(&p, &s)
    }

    /// Independent reference for [`select`]: walk includes in document
    /// order, append each fragment's surviving matches sorted, no dedup.
    /// Uses the naive recursive matcher, so agreement with `select` (which
    /// drives the iterative compiled matcher) is meaningful.
    fn select_naive(cands: &[String], incl: &[String], excl: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for inc in incl {
            let mut matched: Vec<String> = cands
                .iter()
                .filter(|c| matches_naive(inc, c) && !excl.iter().any(|p| matches_naive(p, c)))
                .map(|c| split_segments(c).join("/"))
                .collect();
            matched.sort();
            out.append(&mut matched);
        }
        out
    }

    // ----- Generators (generate-correct, fuzz the bias) -----

    fn seg_chars() -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(vec!['a', 'b', 'c']), 1..=3)
            .prop_map(|cs| cs.into_iter().collect())
    }

    /// A relative path: 1–4 segments over {a,b,c}.
    fn path_segs() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(seg_chars(), 1..=4)
    }

    /// Transform a literal segment into a pattern segment that still
    /// matches it: each char becomes `*` (25%), `?` (25%), or itself.
    fn derive_seg(seg: String) -> impl Strategy<Value = String> {
        let n = seg.chars().count();
        prop::collection::vec(0u32..100, n).prop_map(move |codes| {
            seg.chars()
                .zip(codes)
                .map(|(c, code)| match code {
                    0..=24 => '*',
                    25..=49 => '?',
                    _ => c,
                })
                .collect::<String>()
        })
    }

    /// A pattern derived from `path` so it matches by construction: each
    /// segment is replaced by `**` (20%) or a char-transformed version.
    fn derived_pattern(path: Vec<String>) -> impl Strategy<Value = String> {
        let per_seg: Vec<_> = path
            .into_iter()
            .map(|s| {
                (0u32..100, derive_seg(s)).prop_map(|(r, transformed)| {
                    if r < 20 {
                        "**".to_string()
                    } else {
                        transformed
                    }
                })
            })
            .collect();
        per_seg.prop_map(|segs| segs.join("/"))
    }

    fn indep_tok() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("a"), Just("b"), Just("c"), Just("*"), Just("?"),]
    }

    fn indep_seg() -> impl Strategy<Value = String> {
        prop_oneof![
            2 => Just("**".to_string()),
            8 => prop::collection::vec(indep_tok(), 1..=3).prop_map(|t| t.concat()),
        ]
    }

    /// An independent random pattern: 1–4 segments. Usually does not match
    /// a given path, but sometimes coincidentally does.
    fn independent_pattern() -> impl Strategy<Value = String> {
        prop::collection::vec(indep_seg(), 1..=4).prop_map(|s| s.join("/"))
    }

    /// A (pattern, path) pair. Fuzzes a per-sample bias toward deriving
    /// the pattern from the path (guaranteed match) vs. an independent
    /// pattern (usually non-match), so both regimes are explored.
    fn case() -> impl Strategy<Value = (String, String)> {
        (path_segs(), 0u32..=100, 0u32..=100).prop_flat_map(|(segs, bias, roll)| {
            let path_str = segs.join("/");
            let derive = roll <= bias;
            if derive {
                derived_pattern(segs)
                    .prop_map(move |pat| (pat, path_str.clone()))
                    .boxed()
            } else {
                independent_pattern()
                    .prop_map(move |pat| (pat, path_str.clone()))
                    .boxed()
            }
        })
    }

    // ----- Distribution sanity (assert the generator explores) -----

    #[test]
    fn case_distribution_is_non_trivial() {
        // The matcher property below is only meaningful if the generator
        // produces matches AND non-matches, and exercises `*`/`?`/`**`
        // and multi-segment paths. We sample 512 cases and assert each
        // bucket is hit with a wide margin.
        //
        // P(match) ≈ 0.5 (derive branch, ~always matches) + small
        // (independent branch). Over 512 the per-bucket counts sit many
        // (>10) standard deviations above the thresholds below, so the
        // false-positive rate is far under 1e-11.
        let mut runner = TestRunner::default();
        let strat = case();
        let n = 512;
        let (mut matches, mut nonmatches) = (0, 0);
        let (mut has_star, mut has_q, mut has_dstar, mut multiseg) = (0, 0, 0, 0);
        for _ in 0..n {
            let (pat, path) = strat.new_tree(&mut runner).unwrap().current();
            if matches_naive(&pat, &path) {
                matches += 1;
            } else {
                nonmatches += 1;
            }
            if pat.split('/').any(|s| s != "**" && s.contains('*')) {
                has_star += 1;
            }
            if pat.contains('?') {
                has_q += 1;
            }
            if pat.split('/').any(|s| s == "**") {
                has_dstar += 1;
            }
            if path.contains('/') {
                multiseg += 1;
            }
        }
        assert!(matches >= 60, "too few matching cases: {matches}/{n}");
        assert!(
            nonmatches >= 60,
            "too few non-matching cases: {nonmatches}/{n}"
        );
        assert!(has_star >= 30, "too few `*` patterns: {has_star}/{n}");
        assert!(has_q >= 30, "too few `?` patterns: {has_q}/{n}");
        assert!(has_dstar >= 30, "too few `**` patterns: {has_dstar}/{n}");
        assert!(
            multiseg >= 30,
            "too few multi-segment paths: {multiseg}/{n}"
        );
    }

    // ----- select() generator + distribution -----

    fn select_case() -> impl Strategy<Value = (Vec<String>, Vec<String>, Vec<String>)> {
        prop::collection::vec(path_segs(), 1..=6).prop_flat_map(|cands| {
            let cand_strings: Vec<String> = cands.iter().map(|s| s.join("/")).collect();
            let incl_pool = cand_strings.clone();
            let excl_pool = cand_strings.clone();
            let incl = prop::collection::vec(
                prop_oneof![
                    2 => Just("**".to_string()),
                    2 => independent_pattern(),
                    3 => prop::sample::select(incl_pool),
                ],
                1..=3,
            );
            let excl = prop::collection::vec(
                prop_oneof![
                    3 => prop::sample::select(excl_pool),
                    1 => independent_pattern(),
                ],
                0..=3,
            );
            (Just(cand_strings), incl, excl)
        })
    }

    #[test]
    fn select_distribution_is_non_trivial() {
        // Ensure select() cases produce non-empty output often and that
        // excludes actually remove something often — otherwise the
        // equality property below would be vacuous.
        let mut runner = TestRunner::default();
        let strat = select_case();
        let n = 512;
        let (mut nonempty, mut removed) = (0, 0);
        for _ in 0..n {
            let (cands, incl, excl) = strat.new_tree(&mut runner).unwrap().current();
            let with = select_naive(&cands, &incl, &excl);
            let without = select_naive(&cands, &incl, &[]);
            if !with.is_empty() {
                nonempty += 1;
            }
            if without.len() > with.len() {
                removed += 1;
            }
        }
        assert!(
            nonempty >= 40,
            "too few non-empty selections: {nonempty}/{n}"
        );
        assert!(removed >= 40, "too few exclude-removals: {removed}/{n}");
    }

    // ----- Properties -----

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

        /// The compiled matcher agrees with the naive reference oracle.
        #[test]
        fn compiled_matcher_agrees_with_naive((pat, path) in case()) {
            let compiled = Pattern::parse(&pat);
            prop_assert_eq!(
                compiled.matches(&path),
                matches_naive(&pat, &path),
                "pattern {:?} vs path {:?}", pat, path
            );
        }

        /// `select` equals the naive filter→sort→dedup reference.
        #[test]
        fn select_matches_reference((cands, incl, excl) in select_case()) {
            let compiled_incl: Vec<Pattern> = incl.iter().map(|p| Pattern::parse(p)).collect();
            let compiled_excl: Vec<Pattern> = excl.iter().map(|p| Pattern::parse(p)).collect();
            let cand_refs: Vec<&str> = cands.iter().map(String::as_str).collect();
            let got = select(&cand_refs, &compiled_incl, &compiled_excl);
            let want = select_naive(&cands, &incl, &excl);
            prop_assert_eq!(got, want);
        }
    }

    // ----- Example unit tests for easy-to-miss semantics -----

    #[test]
    fn star_is_single_segment() {
        let p = Pattern::parse("*.fs");
        assert!(p.matches("a.fs"));
        assert!(!p.matches("dir/a.fs"), "`*` must not cross `/`");
    }

    #[test]
    fn double_star_is_recursive_and_matches_zero_segments() {
        let p = Pattern::parse("**/*.fs");
        assert!(p.matches("a.fs"), "`**/` must allow zero leading dirs");
        assert!(p.matches("x/a.fs"));
        assert!(p.matches("x/y/a.fs"));
        assert!(!p.matches("x/a.txt"));
    }

    #[test]
    fn trailing_double_star_matches_everything_below() {
        let p = Pattern::parse("src/**");
        assert!(p.matches("src/a.fs"));
        assert!(p.matches("src/x/y/a.fs"));
        assert!(
            p.matches("src"),
            "`**` matches zero segments, so the dir itself matches"
        );
        assert!(!p.matches("other/a.fs"));
    }

    #[test]
    fn question_matches_exactly_one_char() {
        let p = Pattern::parse("a?.fs");
        assert!(p.matches("ab.fs"));
        assert!(!p.matches("a.fs"));
        assert!(!p.matches("abc.fs"));
    }

    #[test]
    fn literal_is_not_glob_and_matches_only_itself() {
        let p = Pattern::parse("src/Main.fs");
        assert!(!p.is_glob());
        assert!(p.matches("src/Main.fs"));
        assert!(!p.matches("src/Other.fs"));
    }

    #[test]
    fn wildcards_report_as_glob() {
        assert!(Pattern::parse("*.fs").is_glob());
        assert!(Pattern::parse("a?.fs").is_glob());
        assert!(Pattern::parse("**/x.fs").is_glob());
    }

    #[test]
    fn literal_prefix_is_not_a_wildcard() {
        // A literal prefix segment containing `*`/`?` matches only itself,
        // while the trailing fragment still globs. This is what keeps a base
        // directory whose name contains glob metacharacters from turning into
        // a pattern that matches sibling directories.
        let pat = Pattern::with_literal_prefix(&["a*b", "pr?j"], "*.fs");
        assert!(pat.is_glob());
        assert!(pat.matches("a*b/pr?j/x.fs"));
        assert!(!pat.matches("axb/pr?j/x.fs"));
        assert!(!pat.matches("a*b/proj/x.fs"));
        assert!(!pat.matches("a*b/pr?j/x.fsi"));
        // A wholly-literal fragment after a literal prefix is not a glob.
        assert!(!Pattern::with_literal_prefix(&["a*b"], "main.fs").is_glob());
    }

    #[test]
    fn backslashes_normalise_to_forward_slash() {
        let p = Pattern::parse("src\\Main.fs");
        assert!(p.matches("src/Main.fs"));
        assert!(p.matches("src\\Main.fs"));
    }

    #[test]
    fn current_dir_segments_are_collapsed() {
        // MSBuild lets a glob spell the project dir as `./` or `.\`, and
        // `.` may appear mid-path. A lone `.` segment is cosmetic and must
        // not affect matching, on either the pattern or the candidate side.
        assert!(Pattern::parse("./*.fs").matches("Main.fs"));
        assert!(Pattern::parse(".\\src/*.fs").matches("src/a.fs"));
        assert!(Pattern::parse("src/./a.fs").matches("src/a.fs"));
        assert!(Pattern::parse("src/a.fs").matches("./src/a.fs"));
        // A `.` inside a segment (a file extension) is preserved.
        assert!(Pattern::parse("a.fs").matches("a.fs"));
        assert!(!Pattern::parse("a.fs").matches("axfs"));
    }

    #[test]
    fn select_preserves_include_fragment_order() {
        // MSBuild adds items in Include-fragment document order; within a
        // single fragment's expansion we sort deterministically. Earlier
        // fragments precede later ones even when the later fragment sorts
        // lower lexicographically.
        let cands = ["a.fs", "b.fs", "z.fs"];
        let incl = [Pattern::parse("z.fs"), Pattern::parse("a.fs")];
        assert_eq!(
            select(&cands, &incl, &[]),
            vec!["z.fs".to_string(), "a.fs".to_string()],
            "fragment order must beat lexicographic order"
        );
    }

    #[test]
    fn select_keeps_overlapping_duplicates() {
        // `select` is an information-preserving primitive: it does NOT
        // deduplicate across fragments. MSBuild item evaluation keeps
        // duplicates from overlapping Include fragments by default, and
        // whether/how to fold them is a faithfulness decision deferred to
        // the 9b-2 resolver + `dotnet msbuild` oracle. So a literal that a
        // later glob also matches appears twice, in document order.
        let cands = ["a.fs", "b.fs"];
        let incl = [Pattern::parse("a.fs"), Pattern::parse("*.fs")];
        assert_eq!(
            select(&cands, &incl, &[]),
            vec!["a.fs".to_string(), "a.fs".to_string(), "b.fs".to_string()],
        );
    }

    #[test]
    fn split_glob_root_separates_fixed_prefix_from_wildcard_tail() {
        let cases = [
            ("*.fs", vec![], Some(1)),
            ("**/*.fs", vec![], None),
            ("sub/*.fs", vec!["sub"], Some(1)),
            ("../shared/*.fs", vec!["..", "shared"], Some(1)),
            ("a/b/**/*.fs", vec!["a", "b"], None),
            ("./src/*.fs", vec!["src"], Some(1)),
            ("a*/x.fs", vec![], Some(2)),
            ("/opt/lib/*.fs", vec!["opt", "lib"], Some(1)),
            ("dir/sub/?.fs", vec!["dir", "sub"], Some(1)),
        ];
        for (frag, prefix, depth) in cases {
            let root = split_glob_root(frag);
            assert_eq!(
                root.prefix,
                prefix.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                "prefix for {frag:?}"
            );
            assert_eq!(root.tail_depth, depth, "tail_depth for {frag:?}");
        }
    }

    #[test]
    fn split_glob_root_prefix_is_wildcard_free_and_total() {
        // The prefix carries no wildcard, and prefix ++ tail reconstructs
        // every surviving segment of the fragment.
        for frag in ["*.fs", "a/b/*.fs", "../x/**/*.fs", "p/q/r*/s.fs"] {
            let root = split_glob_root(frag);
            for seg in &root.prefix {
                assert!(!seg.contains('*') && !seg.contains('?'), "{frag:?}");
            }
            let total = split_segments(frag).len();
            // tail_depth counts tail segments only when bounded; for the
            // unbounded (`**`) case we just check the prefix is a strict
            // prefix of all segments.
            if let Some(d) = root.tail_depth {
                assert_eq!(root.prefix.len() + d, total, "{frag:?}");
            } else {
                assert!(root.prefix.len() < total, "{frag:?}");
            }
        }
    }

    #[test]
    fn select_normalises_separators_in_output() {
        // The FS enumerator (9b-2) may hand us OS-native separators on
        // Windows. `select` must emit `/`-normalised paths so the
        // lexicographic ordering is platform-independent: `a\b.fs` must
        // normalise to `a/b.fs` and sort before `c0.fs` (`/` < `c`), not
        // after it as the raw `\` (0x5C) byte would.
        let incl = [Pattern::parse("**/*.fs")];
        let got = select(&["c0.fs", "a\\b.fs"], &incl, &[]);
        assert_eq!(got, vec!["a/b.fs".to_string(), "c0.fs".to_string()]);
    }
}
