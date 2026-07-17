//! `VersionRange`: NuGet's version-range model, matching
//! `NuGet.Versioning`'s `VersionRange` (differentially tested against it in
//! `tests/range_diff.rs`).
//!
//! Shapes accepted, mirroring `VersionRange.TryParse`:
//!
//! - **Bare version** — `1.0.0` means `[1.0.0, )`: an inclusive minimum with
//!   no upper bound (NuGet's "minimum version" semantics, *not* an exact
//!   pin).
//! - **Interval notation** — `[1.0, 2.0)`, `(, 2.0]`, `[1.0]` (exact pin;
//!   the single-version form requires both brackets inclusive), with either
//!   bound omissible. Bounds are compared with `VersionComparer.Default`
//!   (so `satisfies` inherits every quirk documented in [`crate::version`]).
//! - **Floating** — `*`, `1.*`, `1.2.*`, `1.2.3.*`, release floats
//!   (`1.0.0-*`, `1.0.0-beta*`, `1.0.0-beta.*`), and combined forms
//!   (`*-*`, `1.*-beta*`). A float contributes a *resolved base minimum*
//!   (e.g. `1.2.*` → min `1.2.0`) and [`VersionRange::satisfies`] uses only
//!   that bound, mirroring `VersionRangeBase.Satisfies`; picking the best
//!   float *match* among candidates is the resolver's job (deferred — the
//!   restore plan declines floats at resolution time anyway).
//!
//! `satisfies` is pure bounds arithmetic: a prerelease inside a
//! stable-bounded range *does* satisfy it (`[1.0, 2.0]` contains
//! `1.5.0-beta`). NuGet's "no prereleases unless asked" behaviour lives in
//! its resolver/`FindBestMatch`, not in `Satisfies` — the oracle diff pins
//! this.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use crate::version::{NuGetVersion, VersionParseError};

/// A parsed NuGet version range.
///
/// Structural equality (bounds compare with `NuGetVersion`'s
/// comparer-equality, plus inclusiveness flags and the float spec); this is
/// *not* NuGet's `VersionRangeComparer` — nothing needs that yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRange {
    /// Resolved lower bound; for a floating range this is the float's base
    /// minimum (`1.2.*` → `1.2.0`).
    min: Option<NuGetVersion>,
    max: Option<NuGetVersion>,
    min_inclusive: bool,
    max_inclusive: bool,
    float: Option<FloatSpec>,
}

/// The floating part of a floating range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloatSpec {
    behavior: FloatBehavior,
    /// The float pattern in *normalised* form (`01.*-BETA*` → `1.*-BETA*`:
    /// numeric prefix normalised, release prefix verbatim). The normalised
    /// range string prints this rather than the resolved minimum — pinned
    /// by the oracle diff.
    pattern: String,
}

/// Which positions float — the mirror of `NuGetVersionFloatBehavior`.
/// `Display` produces the exact .NET enum names the oracle reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatBehavior {
    /// `1.0.0-*` / `1.0.0-beta*`: only the release labels float.
    Prerelease,
    /// `1.2.3.*`
    Revision,
    /// `1.2.*`
    Patch,
    /// `1.*`
    Minor,
    /// `*`
    Major,
    /// `*-*`: absolutely anything, prereleases included.
    AbsoluteLatest,
    /// `1.2.3.*-…*`
    PrereleaseRevision,
    /// `1.2.*-…*`
    PrereleasePatch,
    /// `1.*-…*`
    PrereleaseMinor,
    /// `*-…*` with a release prefix (bare `*-*` is `AbsoluteLatest`).
    PrereleaseMajor,
}

impl fmt::Display for FloatBehavior {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            FloatBehavior::Prerelease => "Prerelease",
            FloatBehavior::Revision => "Revision",
            FloatBehavior::Patch => "Patch",
            FloatBehavior::Minor => "Minor",
            FloatBehavior::Major => "Major",
            FloatBehavior::AbsoluteLatest => "AbsoluteLatest",
            FloatBehavior::PrereleaseRevision => "PrereleaseRevision",
            FloatBehavior::PrereleasePatch => "PrereleasePatch",
            FloatBehavior::PrereleaseMinor => "PrereleaseMinor",
            FloatBehavior::PrereleaseMajor => "PrereleaseMajor",
        };
        f.write_str(name)
    }
}

/// Why a range string failed to parse. As with versions, the *accepted set*
/// is differentially pinned to `VersionRange.TryParse`; the taxonomy is
/// ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeParseError {
    /// The input was empty (or only whitespace).
    Empty,
    /// Structurally not a range: unbalanced brackets, too many commas, both
    /// bounds absent, or a single-version interval without `[..]`.
    Malformed,
    /// A bound failed to parse as a version.
    BadVersion(VersionParseError),
    /// A `*` pattern that isn't one of the supported float shapes.
    BadFloat,
}

impl fmt::Display for RangeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RangeParseError::Empty => f.write_str("empty range string"),
            RangeParseError::Malformed => f.write_str("malformed range syntax"),
            RangeParseError::BadVersion(e) => write!(f, "bad version bound: {e}"),
            RangeParseError::BadFloat => f.write_str("unsupported floating-version pattern"),
        }
    }
}

impl std::error::Error for RangeParseError {}

/// Parse one lower-bound string: the float grammar when the *last
/// character* is `*`, otherwise a plain version (in which case any interior
/// `*` fails version-charset validation — that's how `1.0.* ` with trailing
/// whitespace gets rejected while ` 1.0.*` parses; there is no trimming
/// rule, just star-must-be-last).
///
/// Identical for bare ranges and bracketed min slots. (An earlier revision
/// special-cased brackets, misreading the oracle: "[1*, 2.0]" rejects
/// because the substituted minimum 10.0.0 exceeds the max — the ordering
/// check, not a float rule. "[1*, )" parses fine, as [10.0.0, ). Only the
/// single-element interval is genuinely different: it never floats.)
fn parse_min_bound(text: &str) -> Result<(NuGetVersion, Option<FloatSpec>), RangeParseError> {
    if text.ends_with('*') {
        parse_float(text)
    } else {
        let v = NuGetVersion::parse(text).map_err(RangeParseError::BadVersion)?;
        Ok((v, None))
    }
}

/// `FloatRange.TryParse`, behaviourally (every rule below is
/// oracle-pinned; several are surprising):
///
/// - The literal strings `*` and `*-*` are special-cased (Major /
///   AbsoluteLatest). ` *-*` with a leading space is *not* AbsoluteLatest —
///   it takes the general path and comes out PrereleaseMajor.
/// - Numeric side: the trailing `*` is **substituted with `0`** to form the
///   base minimum, and the behaviour comes from the **dot count**. So
///   `1.9*` is a Minor float with base `1.90.0` (not `1.9.0`), and `1*`
///   (zero dots) is **not a float at all** — it's the plain version `10`.
/// - Release side: the trailing `*` is **stripped**; an empty prefix
///   becomes `0`, a prefix ending in `.` gets `0` appended (`-*` → `-0`,
///   `beta.*` → `beta.0`, `beta*` → `beta`).
/// - The composed base string goes through the *lenient* version parser,
///   which is where floats inherit whitespace tolerance (`0\t.91*` is a
///   Minor float, base `0.910.0`).
fn parse_float(text: &str) -> Result<(NuGetVersion, Option<FloatSpec>), RangeParseError> {
    fn spec(behavior: FloatBehavior, pattern: String) -> Option<FloatSpec> {
        Some(FloatSpec { behavior, pattern })
    }

    if text == "*" {
        let min = NuGetVersion::parse("0.0.0").expect("static");
        return Ok((min, spec(FloatBehavior::Major, "*".to_owned())));
    }
    if text == "*-*" {
        let min = NuGetVersion::parse("0.0.0-0").expect("static");
        return Ok((min, spec(FloatBehavior::AbsoluteLatest, "*-*".to_owned())));
    }

    // '+' anywhere rejects — even "1.+2.*", where the lenient version parse
    // of the base would have accepted '+' as a component sign. (Metadata
    // has no meaning in a float pattern.)
    if text.contains('+') {
        return Err(RangeParseError::BadFloat);
    }

    // Split off the release at the first '-', mirroring version sectioning.
    let (version_part, release_part) = match text.find('-') {
        Some(i) => (&text[..i], Some(&text[i + 1..])),
        None => (text, None),
    };

    match release_part {
        None => {
            // Pure numeric float candidate: `text` ends with '*'.
            let base = format!("{}0", &text[..text.len() - 1]);
            let min = NuGetVersion::parse(&base).map_err(RangeParseError::BadVersion)?;
            let behavior = match text.matches('.').count() {
                // "1*" is the version "10" (star substituted), not a float
                // — in bare and bracketed positions alike.
                0 => return Ok((min, None)),
                1 => FloatBehavior::Minor,
                2 => FloatBehavior::Patch,
                3 => FloatBehavior::Revision,
                _ => return Err(RangeParseError::BadFloat),
            };
            let pattern = float_pattern(behavior, &min, None);
            Ok((min, spec(behavior, pattern)))
        }
        Some(release) => {
            // `text` ends with '*', so `release` does too (a '*' confined
            // to the version part with a fixed release, like "1.*-beta",
            // means `text` doesn't end with '*' and never reaches here).
            let prefix = &release[..release.len() - 1];
            let release_min = if prefix.is_empty() {
                "0".to_owned()
            } else if prefix.ends_with('.') {
                format!("{prefix}0")
            } else {
                prefix.to_owned()
            };
            let (base_version, behavior) = if version_part.contains('*') {
                if !version_part.ends_with('*') {
                    return Err(RangeParseError::BadFloat);
                }
                let base = format!("{}0", &version_part[..version_part.len() - 1]);
                let behavior = match version_part.matches('.').count() {
                    0 => FloatBehavior::PrereleaseMajor,
                    1 => FloatBehavior::PrereleaseMinor,
                    2 => FloatBehavior::PrereleasePatch,
                    3 => FloatBehavior::PrereleaseRevision,
                    _ => return Err(RangeParseError::BadFloat),
                };
                (base, behavior)
            } else {
                (version_part.to_owned(), FloatBehavior::Prerelease)
            };
            // Any leftover '*' (in the prefix or mid-version) lands in this
            // compose and fails version-charset validation naturally.
            let min = NuGetVersion::parse(&format!("{base_version}-{release_min}"))
                .map_err(RangeParseError::BadVersion)?;
            let pattern = float_pattern(behavior, &min, Some(prefix));
            Ok((min, spec(behavior, pattern)))
        }
    }
}

/// Rebuild the normalised float pattern from the behaviour and the *parsed*
/// base minimum — which is exactly why "01.0*-BETA*" prints "1.*-BETA*" and
/// "1.9*" prints "1.*" (the 90 lives only in the resolved minimum).
fn float_pattern(
    behavior: FloatBehavior,
    min: &NuGetVersion,
    release_prefix: Option<&str>,
) -> String {
    let numeric = match behavior {
        FloatBehavior::Major | FloatBehavior::PrereleaseMajor => "*".to_owned(),
        FloatBehavior::AbsoluteLatest => return "*-*".to_owned(),
        FloatBehavior::Minor | FloatBehavior::PrereleaseMinor => format!("{}.*", min.major()),
        FloatBehavior::Patch | FloatBehavior::PrereleasePatch => {
            format!("{}.{}.*", min.major(), min.minor())
        }
        FloatBehavior::Revision | FloatBehavior::PrereleaseRevision => {
            format!("{}.{}.{}.*", min.major(), min.minor(), min.patch())
        }
        FloatBehavior::Prerelease => {
            let mut s = format!("{}.{}.{}", min.major(), min.minor(), min.patch());
            if min.revision() != 0 {
                s.push('.');
                s.push_str(&min.revision().to_string());
            }
            s
        }
    };
    match release_prefix {
        None => numeric,
        Some(prefix) => format!("{numeric}-{prefix}*"),
    }
}

impl VersionRange {
    /// Parse with `VersionRange.TryParse` semantics (floating allowed).
    pub fn parse(input: &str) -> Result<VersionRange, RangeParseError> {
        let t = input.trim();
        if t.is_empty() {
            return Err(RangeParseError::Empty);
        }

        let starts_bracket = t.starts_with('[') || t.starts_with('(');
        let ends_bracket = t.ends_with(']') || t.ends_with(')');
        if !starts_bracket {
            if ends_bracket {
                return Err(RangeParseError::Malformed);
            }
            // Bare form: an inclusive minimum, floating or fixed.
            let (min, float) = parse_min_bound(t)?;
            return Ok(VersionRange {
                min: Some(min),
                max: None,
                min_inclusive: true,
                max_inclusive: false,
                float,
            });
        }
        if !ends_bracket {
            return Err(RangeParseError::Malformed);
        }

        let min_inclusive = t.starts_with('[');
        let max_inclusive = t.ends_with(']');
        let inner = &t[1..t.len() - 1]; // brackets are ASCII, so byte-safe
        let parts: Vec<&str> = inner.split(',').collect();

        // All parts being the *empty string* rejects ("[]", "[,]", "(,)");
        // whitespace-only parts instead mean "this bound is absent" ("[ ]"
        // and "[, ]" are the unbounded range "(, )") — oracle-pinned.
        if parts.iter().all(|p| p.is_empty()) {
            return Err(RangeParseError::Malformed);
        }

        let (min, float, max) = match parts.len() {
            1 => {
                // Single-element interval: requires [..] exactly, even in
                // the degenerate whitespace form ("( )" is rejected while
                // "[ ]" parses as unbounded).
                if !min_inclusive || !max_inclusive {
                    return Err(RangeParseError::Malformed);
                }
                if parts[0].trim().is_empty() {
                    (None, None, None)
                } else {
                    // The single-element form never enters the float
                    // grammar: any '*' fails version-charset validation, so
                    // "[*]", "[1.0.*]", "[1*]", and even "[0*]" all reject
                    // (unlike the two-part min slot) — oracle-pinned.
                    let v = NuGetVersion::parse(parts[0]).map_err(RangeParseError::BadVersion)?;
                    (Some(v.clone()), None, Some(v))
                }
            }
            2 => {
                // The bound goes in *raw* — no trimming. Whitespace
                // tolerance comes solely from the lenient version parser,
                // which is what makes "[ 1.0.*, 2)" parse while
                // "[1.0.* , 2)" fails (trailing space means the star is no
                // longer the last character, so it's version-parsed and the
                // '*' is an invalid character), and why "( *-*, )" is
                // PrereleaseMajor (the leading space defeats the literal
                // "*-*" AbsoluteLatest match).
                let min = if parts[0].trim().is_empty() {
                    None
                } else {
                    Some(parse_min_bound(parts[0])?)
                };
                let max = if parts[1].trim().is_empty() {
                    None
                } else {
                    // '*' is not a valid version character, so a float in
                    // the max slot fails version parse naturally.
                    Some(
                        NuGetVersion::parse(parts[1].trim())
                            .map_err(RangeParseError::BadVersion)?,
                    )
                };
                let (min, float) = match min {
                    Some((v, f)) => (Some(v), f),
                    None => (None, None),
                };
                (min, float, max)
            }
            _ => return Err(RangeParseError::Malformed),
        };

        // Bound ordering: min above max rejects; equal bounds reject only
        // on *mixed* inclusivity ("[1.0,1.0)" invalid, "(1.0,1.0)" is a
        // legal empty range) — oracle-pinned.
        if let (Some(min_v), Some(max_v)) = (&min, &max) {
            match min_v.cmp(max_v) {
                Ordering::Greater => return Err(RangeParseError::Malformed),
                Ordering::Equal if min_inclusive != max_inclusive => {
                    return Err(RangeParseError::Malformed);
                }
                _ => {}
            }
        }

        Ok(VersionRange {
            // An absent bound coerces its inclusivity flag off ("[,2.0]"
            // reports IsMinInclusive=false and prints "(, 2.0.0]").
            min_inclusive: min.is_some() && min_inclusive,
            max_inclusive: max.is_some() && max_inclusive,
            min,
            max,
            float,
        })
    }

    /// Resolved lower bound (the float's base minimum for floating ranges).
    pub fn min_version(&self) -> Option<&NuGetVersion> {
        self.min.as_ref()
    }

    /// Upper bound, if any.
    pub fn max_version(&self) -> Option<&NuGetVersion> {
        self.max.as_ref()
    }

    /// Whether the lower bound, when present, is included.
    pub fn is_min_inclusive(&self) -> bool {
        self.min_inclusive
    }

    /// Whether the upper bound, when present, is included.
    pub fn is_max_inclusive(&self) -> bool {
        self.max_inclusive
    }

    /// True when a lower bound exists.
    pub fn has_lower_bound(&self) -> bool {
        self.min.is_some()
    }

    /// True when an upper bound exists.
    pub fn has_upper_bound(&self) -> bool {
        self.max.is_some()
    }

    /// True for floating ranges (`1.*`, …). The offline resolver declines
    /// these — resolving a float faithfully needs feed state we can't see.
    pub fn is_floating(&self) -> bool {
        self.float.is_some()
    }

    /// The float behaviour, for floating ranges.
    pub fn float_behavior(&self) -> Option<FloatBehavior> {
        self.float.as_ref().map(|f| f.behavior)
    }

    /// `VersionRangeBase.Satisfies`: pure bound comparison under
    /// `VersionComparer.Default`. See the module docs for what this
    /// deliberately does *not* do (prerelease filtering, float matching).
    pub fn satisfies(&self, version: &NuGetVersion) -> bool {
        if let Some(min) = &self.min {
            let ord = version.cmp(min);
            if self.min_inclusive {
                if ord == Ordering::Less {
                    return false;
                }
            } else if ord != Ordering::Greater {
                return false;
            }
        }
        if let Some(max) = &self.max {
            let ord = version.cmp(max);
            if self.max_inclusive {
                if ord == Ordering::Greater {
                    return false;
                }
            } else if ord != Ordering::Less {
                return false;
            }
        }
        true
    }

    /// `ToNormalizedString()`: `[1.0.0, 2.0.0)` style. Exact pins print
    /// long-form (`[1.0.0, 1.0.0]`) and floats print their normalised
    /// pattern (`[1.0.*, )`) — both pinned by the oracle diff.
    pub fn to_normalized_string(&self) -> String {
        let min_text = match (&self.float, &self.min) {
            (Some(f), _) => f.pattern.clone(),
            (None, Some(v)) => v.to_normalized_string(),
            (None, None) => String::new(),
        };
        let open = if self.min_inclusive { '[' } else { '(' };
        let close = if self.max_inclusive { ']' } else { ')' };
        let max_text = match &self.max {
            Some(v) => v.to_normalized_string(),
            None => String::new(),
        };
        format!("{open}{min_text}, {max_text}{close}")
    }
}

impl fmt::Display for VersionRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_normalized_string())
    }
}

impl FromStr for VersionRange {
    type Err = RangeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        VersionRange::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> VersionRange {
        VersionRange::parse(s).unwrap_or_else(|e| panic!("{s:?} should parse: {e}"))
    }

    fn v(s: &str) -> NuGetVersion {
        NuGetVersion::parse(s).unwrap()
    }

    #[test]
    fn bare_version_is_inclusive_minimum() {
        let range = r("1.2.3");
        assert!(range.satisfies(&v("1.2.3")));
        assert!(range.satisfies(&v("99.0.0")));
        assert!(!range.satisfies(&v("1.2.2")));
        assert_eq!(range.to_normalized_string(), "[1.2.3, )");
    }

    #[test]
    fn interval_inclusivity() {
        let range = r("[1.0, 2.0)");
        assert!(range.satisfies(&v("1.0.0")));
        assert!(range.satisfies(&v("1.9.9")));
        assert!(!range.satisfies(&v("2.0.0")));
        assert!(!range.satisfies(&v("0.9.9")));
        assert_eq!(range.to_normalized_string(), "[1.0.0, 2.0.0)");
    }

    #[test]
    fn exact_pin() {
        let range = r("[1.0]");
        assert!(range.satisfies(&v("1.0.0")));
        assert!(!range.satisfies(&v("1.0.1")));
        assert_eq!(range.to_normalized_string(), "[1.0.0, 1.0.0]");
    }

    #[test]
    fn open_lower_bound() {
        let range = r("(, 2.0]");
        assert!(range.satisfies(&v("0.0.1")));
        assert!(range.satisfies(&v("2.0.0")));
        assert!(!range.satisfies(&v("2.0.1")));
    }

    #[test]
    fn prerelease_inside_stable_bounds_satisfies() {
        // NuGet's prerelease gating is a resolver concern, not Satisfies.
        let range = r("[1.0, 2.0]");
        assert!(range.satisfies(&v("1.5.0-beta")));
        // But a prerelease of the *excluded max* itself is below the max.
        assert!(r("[1.0, 2.0)").satisfies(&v("2.0.0-beta")));
    }

    #[test]
    fn floats_parse_and_bound_from_base() {
        let range = r("1.2.*");
        assert!(range.is_floating());
        assert_eq!(range.float_behavior(), Some(FloatBehavior::Patch));
        assert_eq!(range.min_version(), Some(&v("1.2.0")));
        assert!(range.satisfies(&v("1.2.5")));
        // Satisfies is bounds-only: the float does not cap the version.
        assert!(range.satisfies(&v("3.0.0")));
        assert!(!range.satisfies(&v("1.1.9")));
    }

    #[test]
    fn obvious_rejections() {
        for bad in [
            "",
            "[1.0",
            "1.0]",
            "[1.0,2.0,3.0]",
            "[,]",
            "(,)",
            "(1.0)",
            "[1.0,2.0.*]",
            "*.1",
            "1.*.2",
            "banana",
        ] {
            assert!(
                VersionRange::parse(bad).is_err(),
                "{bad:?} should not parse"
            );
        }
    }
}
