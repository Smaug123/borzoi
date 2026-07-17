//! `NuGetVersion`: NuGet's version model, matching `NuGet.Versioning`'s
//! `NuGetVersion` / `VersionComparer.Default` semantics (differentially
//! tested against them in `tests/version_diff.rs`).
//!
//! NuGet is *not* strict SemVer 2.0.0. The deviations this type reproduces:
//!
//! - **A 4th numeric part** (`1.2.3.4`, "legacy" System.Version style). The
//!   normalised string keeps the revision only when it's non-zero.
//! - **Partial versions**: `1` and `1.2` parse, padding missing parts with
//!   zeros.
//! - **Whitespace tolerance**: the whole string is trimmed, and — a
//!   `System.Version`/`int.TryParse` leak — whitespace *inside* a numeric
//!   component is accepted (`1. 2.3` parses as `1.2.3`).
//! - **Release labels compare case-insensitively** (`1.0-BETA` == `1.0-beta`;
//!   strict SemVer is case-sensitive). Labels that parse as `Int32` compare
//!   numerically — including *negative* ones like `-1`, since `-` is a legal
//!   label character and NuGet just calls `int.TryParse`. All-digit labels
//!   with a leading zero are rejected at parse (the strict SemVer rule), but
//!   `-01` is not all-digit, parses fine, and compares as the number −1. A
//!   numeric-looking label that overflows `Int32` silently degrades to
//!   alphanumeric comparison.
//! - **Ordering and equality ignore build metadata** (`1.0+a` == `1.0+b`).
//!
//! ## `Ord`/`Eq` vs NuGet's `Equals`
//!
//! NuGet's comparer is internally *inconsistent*: `Compare("1.0--0",
//! "1.0-0")` is `0` (both labels are the integer zero) but `Equals` on the
//! same pair is `false` (labels compare as case-insensitive strings).
//! Rust's `Ord` contract requires `a == b ⟺ cmp == Equal`, so this type
//! cannot copy that inconsistency into the std traits. The split:
//!
//! - [`Ord`]/[`PartialEq`]/[`Hash`] follow **`Compare`** (the lawful total
//!   order): `1.0--0 == 1.0-0`.
//! - [`NuGetVersion::eq_strict`] follows **`Equals`** (NuGet's *identity*,
//!   what it uses for dictionary keys and dedup): `1.0--0 ≠ 1.0-0`, while
//!   `1.0-BETA` still equals `1.0-beta`. `eq_strict` implies `==`.
//!
//! The differential test pins each side to its own oracle field.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

/// A parsed NuGet package version. See the module docs for how this deviates
/// from strict SemVer, and for why `==` (comparer equality) is deliberately
/// coarser than [`eq_strict`](NuGetVersion::eq_strict) (NuGet's identity).
#[derive(Debug, Clone)]
pub struct NuGetVersion {
    major: u32,
    minor: u32,
    patch: u32,
    revision: u32,
    /// Original-case release labels (`beta.11` → `["beta", "11"]`); empty for
    /// a stable version.
    release_labels: Vec<String>,
    /// Build metadata after `+`, original case, `None` if absent. Excluded
    /// from all comparisons.
    metadata: Option<String>,
}

/// Why a version string failed to parse.
///
/// The *set of accepted strings* is differentially pinned to
/// `NuGetVersion.TryParse`; the error taxonomy is our own (NuGet only says
/// true/false).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionParseError {
    /// The input was empty (or only whitespace, which NuGet trims away).
    Empty,
    /// A dotted numeric component was empty, non-numeric, or exceeded
    /// `i32::MAX` (NuGet stores components as `Int32`).
    BadNumericPart,
    /// More than four dotted numeric components.
    WrongPartCount,
    /// A release label (between `-` and `+`) was empty or contained a
    /// character outside `[0-9A-Za-z-]`.
    BadReleaseLabel,
    /// Build metadata (after `+`) was empty, had an empty dot-part, or
    /// contained a character outside `[0-9A-Za-z-]`.
    BadMetadata,
}

impl fmt::Display for VersionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            VersionParseError::Empty => "empty version string",
            VersionParseError::BadNumericPart => {
                "numeric version component is empty, non-numeric, or exceeds Int32"
            }
            VersionParseError::WrongPartCount => "more than four numeric version components",
            VersionParseError::BadReleaseLabel => "empty or invalid release label",
            VersionParseError::BadMetadata => "empty or invalid build metadata",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for VersionParseError {}

/// Is `b` legal inside a release label or metadata part? NuGet's
/// `IsLetterOrDigitOrDash` — explicitly ASCII-range, so `βeta` is invalid.
fn is_part_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

/// One dotted numeric component, with `System.Version` +
/// `int.TryParse(NumberStyles.Integer)` semantics: optional leading/trailing
/// whitespace, optional leading `+` sign (a leading `-` can never reach
/// here: any `-` in the input ends the version section first), ASCII digits
/// only, `<= i32::MAX` after ignoring leading zeros.
fn parse_component(component: &str) -> Result<u32, VersionParseError> {
    // Oracle-verified: the AllowLeading/TrailingWhite trimming is full
    // Char.IsWhiteSpace (U+00A0 included), not the narrow ASCII set.
    let t = component.trim_matches(char::is_whitespace);
    let t = t.strip_prefix('+').unwrap_or(t);
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return Err(VersionParseError::BadNumericPart);
    }
    let significant = t.trim_start_matches('0');
    if significant.len() > 10 {
        return Err(VersionParseError::BadNumericPart);
    }
    let value: u64 = if significant.is_empty() {
        0
    } else {
        significant
            .parse()
            .map_err(|_| VersionParseError::BadNumericPart)?
    };
    if value > i32::MAX as u64 {
        return Err(VersionParseError::BadNumericPart);
    }
    Ok(value as u32)
}

impl NuGetVersion {
    /// Parse a version string with `NuGetVersion.TryParse` semantics.
    pub fn parse(input: &str) -> Result<NuGetVersion, VersionParseError> {
        // NuGet trims the whole string up front (Char.IsWhiteSpace set,
        // which Rust's char::is_whitespace matches).
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(VersionParseError::Empty);
        }

        // Section split, mirroring ParseSections: the first '-' or '+' ends
        // the numeric section; if '-' came first, release labels run until
        // the first '+'; everything after that '+' is metadata verbatim (a
        // second '+' lands *inside* the metadata and fails validation).
        let mut dash = None;
        let mut plus = None;
        for (i, c) in trimmed.char_indices() {
            match c {
                '-' if dash.is_none() && plus.is_none() => dash = Some(i),
                '+' if plus.is_none() => plus = Some(i),
                _ => {}
            }
        }
        let version_part = &trimmed[..dash.or(plus).unwrap_or(trimmed.len())];
        let release_part = dash.map(|d| &trimmed[d + 1..plus.unwrap_or(trimmed.len())]);
        let metadata_part = plus.map(|p| &trimmed[p + 1..]);

        if version_part.is_empty() {
            return Err(VersionParseError::BadNumericPart);
        }

        // System.Version needs >= 2 components, so NuGet appends ".0" to a
        // bare "1" before handing over.
        let padded;
        let effective = if version_part.contains('.') {
            version_part
        } else {
            padded = format!("{version_part}.0");
            &padded
        };
        let components: Vec<&str> = effective.split('.').collect();
        if components.len() > 4 {
            return Err(VersionParseError::WrongPartCount);
        }
        let mut nums = [0u32; 4];
        for (slot, component) in nums.iter_mut().zip(&components) {
            *slot = parse_component(component)?;
        }

        let mut release_labels = Vec::new();
        if let Some(release) = release_part {
            for label in release.split('.') {
                if label.is_empty() || !label.bytes().all(is_part_byte) {
                    return Err(VersionParseError::BadReleaseLabel);
                }
                // Oracle-verified: strict SemVer's no-leading-zeros rule *is*
                // enforced for numeric labels ("01", "beta.011" are rejected)
                // — but only all-digit labels count as numeric here, so
                // "-01" is legal (and later *compares* as the number -1).
                if label.len() > 1
                    && label.as_bytes()[0] == b'0'
                    && label.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(VersionParseError::BadReleaseLabel);
                }
                release_labels.push(label.to_owned());
            }
        }

        let metadata = match metadata_part {
            None => None,
            Some(m) => {
                if m.is_empty()
                    || m.split('.')
                        .any(|p| p.is_empty() || !p.bytes().all(is_part_byte))
                {
                    return Err(VersionParseError::BadMetadata);
                }
                Some(m.to_owned())
            }
        };

        Ok(NuGetVersion {
            major: nums[0],
            minor: nums[1],
            patch: nums[2],
            revision: nums[3],
            release_labels,
            metadata,
        })
    }

    /// Major component.
    pub fn major(&self) -> u32 {
        self.major
    }

    /// Minor component (0 when absent from the input).
    pub fn minor(&self) -> u32 {
        self.minor
    }

    /// Patch component (0 when absent from the input).
    pub fn patch(&self) -> u32 {
        self.patch
    }

    /// The 4th, System.Version-style component (0 when absent). A non-zero
    /// revision is what NuGet calls a *legacy* version.
    pub fn revision(&self) -> u32 {
        self.revision
    }

    /// Release labels in original case; empty for a stable version.
    pub fn release_labels(&self) -> &[String] {
        &self.release_labels
    }

    /// Build metadata (after `+`), original case. Ignored by every
    /// comparison, matching `VersionComparer.Default`.
    pub fn metadata(&self) -> Option<&str> {
        self.metadata.as_deref()
    }

    /// True when the version has release labels.
    pub fn is_prerelease(&self) -> bool {
        !self.release_labels.is_empty()
    }

    /// NuGet's *identity* — `VersionComparer.Default.Equals`: numeric parts
    /// equal and release labels pairwise equal as case-insensitive strings
    /// (metadata still ignored). Coarser than clone-equality (`1.0-BETA` ==
    /// `1.0-beta`) but finer than `==` (`1.0--0` ≠ `1.0-0`, though they
    /// `cmp` as equal). This is what NuGet keys dictionaries by; use it
    /// wherever "the same version of the same package" is the question.
    pub fn eq_strict(&self, other: &NuGetVersion) -> bool {
        (self.major, self.minor, self.patch, self.revision)
            == (other.major, other.minor, other.patch, other.revision)
            && self.release_labels.len() == other.release_labels.len()
            && self
                .release_labels
                .iter()
                .zip(&other.release_labels)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
    }

    /// `ToNormalizedString()`: `major.minor.patch`, plus `.revision` only
    /// when non-zero, plus `-labels` in original case. No metadata.
    pub fn to_normalized_string(&self) -> String {
        let mut s = format!("{}.{}.{}", self.major, self.minor, self.patch);
        if self.revision != 0 {
            s.push('.');
            s.push_str(&self.revision.to_string());
        }
        if !self.release_labels.is_empty() {
            s.push('-');
            s.push_str(&self.release_labels.join("."));
        }
        s
    }

    /// `ToFullString()`: the normalised string plus `+metadata` when present.
    pub fn to_full_string(&self) -> String {
        let mut s = self.to_normalized_string();
        if let Some(m) = &self.metadata {
            s.push('+');
            s.push_str(m);
        }
        s
    }
}

impl fmt::Display for NuGetVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_normalized_string())
    }
}

impl FromStr for NuGetVersion {
    type Err = VersionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        NuGetVersion::parse(s)
    }
}

/// A release label as `VersionComparer.CompareRelease` sees it: `int.TryParse`
/// first (so `-1`, `011` are numeric; anything overflowing `Int32` is not),
/// otherwise an ordinal-case-insensitive string.
enum LabelKey<'a> {
    Numeric(i32),
    Alpha(&'a str),
}

fn label_key(label: &str) -> LabelKey<'_> {
    match label_numeric(label) {
        Some(n) => LabelKey::Numeric(n),
        None => LabelKey::Alpha(label),
    }
}

/// `int.TryParse` on a charset-valid label: optional leading `-` (legal in
/// labels!), ASCII digits, `Int32` range. `+` and whitespace can't occur —
/// the parser's charset validation already excluded them.
fn label_numeric(label: &str) -> Option<i32> {
    let (negative, digits) = match label.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, label),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let significant = digits.trim_start_matches('0');
    if significant.len() > 10 {
        return None;
    }
    let magnitude: i64 = if significant.is_empty() {
        0
    } else {
        significant.parse().ok()?
    };
    let value = if negative { -magnitude } else { magnitude };
    i32::try_from(value).ok()
}

fn compare_labels(a: &str, b: &str) -> Ordering {
    match (label_key(a), label_key(b)) {
        (LabelKey::Numeric(x), LabelKey::Numeric(y)) => x.cmp(&y),
        // Numeric labels sort below alphanumeric ones.
        (LabelKey::Numeric(_), LabelKey::Alpha(_)) => Ordering::Less,
        (LabelKey::Alpha(_), LabelKey::Numeric(_)) => Ordering::Greater,
        (LabelKey::Alpha(x), LabelKey::Alpha(y)) => {
            // OrdinalIgnoreCase; labels are validated ASCII so this is exact.
            x.bytes()
                .map(|b| b.to_ascii_lowercase())
                .cmp(y.bytes().map(|b| b.to_ascii_lowercase()))
        }
    }
}

impl Ord for NuGetVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let nums = (self.major, self.minor, self.patch, self.revision).cmp(&(
            other.major,
            other.minor,
            other.patch,
            other.revision,
        ));
        if nums != Ordering::Equal {
            return nums;
        }
        match (self.is_prerelease(), other.is_prerelease()) {
            (false, false) => Ordering::Equal,
            // A stable version outranks any prerelease of the same numbers.
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (true, true) => {
                for (a, b) in self.release_labels.iter().zip(&other.release_labels) {
                    let ord = compare_labels(a, b);
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                // Shared prefix equal: the longer label list is greater
                // (1.0-a < 1.0-a.b).
                self.release_labels.len().cmp(&other.release_labels.len())
            }
        }
    }
}

impl PartialOrd for NuGetVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for NuGetVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for NuGetVersion {}

impl Hash for NuGetVersion {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.major, self.minor, self.patch, self.revision).hash(state);
        state.write_usize(self.release_labels.len());
        for label in &self.release_labels {
            // Hash the *comparison key*, keeping Hash consistent with the
            // lawful Eq: "01" and "1" are ==, so they must hash alike.
            match label_key(label) {
                LabelKey::Numeric(n) => {
                    state.write_u8(0);
                    state.write_i32(n);
                }
                LabelKey::Alpha(a) => {
                    state.write_u8(1);
                    for b in a.bytes() {
                        state.write_u8(b.to_ascii_lowercase());
                    }
                    state.write_u8(0xff); // terminator, not a valid label byte
                }
            }
        }
        // Metadata deliberately excluded: 1.0+a == 1.0+b.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> NuGetVersion {
        NuGetVersion::parse(s).unwrap_or_else(|e| panic!("{s:?} should parse: {e}"))
    }

    #[test]
    fn partial_versions_pad_with_zeros() {
        assert_eq!(v("1").to_normalized_string(), "1.0.0");
        assert_eq!(v("1.2").to_normalized_string(), "1.2.0");
        assert_eq!(v("1.2.3").to_normalized_string(), "1.2.3");
    }

    #[test]
    fn revision_kept_only_when_nonzero() {
        assert_eq!(v("1.2.3.4").to_normalized_string(), "1.2.3.4");
        assert_eq!(v("1.2.3.0").to_normalized_string(), "1.2.3");
    }

    #[test]
    fn leading_zeros_normalise_away() {
        assert_eq!(v("01.002.0003").to_normalized_string(), "1.2.3");
    }

    #[test]
    fn whole_string_and_component_whitespace_tolerated() {
        assert_eq!(v(" 1.0.0\t").to_normalized_string(), "1.0.0");
        // The System.Version leak: whitespace inside a component.
        assert_eq!(v("1. 2.3").to_normalized_string(), "1.2.3");
    }

    #[test]
    fn ordering_basics() {
        assert!(v("1.0.0") < v("1.0.1"));
        assert!(v("1.0.0.1") > v("1.0.0"));
        assert!(v("1.0.0-beta") < v("1.0.0"));
        assert!(v("1.0.0-alpha") < v("1.0.0-beta"));
        assert!(v("1.0.0-beta.2") < v("1.0.0-beta.11"), "numeric labels");
        assert!(v("1.0.0-a") < v("1.0.0-a.b"), "longer label list wins");
        assert!(v("1.0.0-1") < v("1.0.0-a"), "numeric below alphanumeric");
    }

    #[test]
    fn negative_numeric_labels_are_numeric() {
        // int.TryParse("-1") succeeds, so "-1" is a numeric label.
        assert!(v("1.0.0--1") < v("1.0.0-0"));
        assert!(v("1.0.0--1") < v("1.0.0-a"));
    }

    #[test]
    fn equality_ignores_case_and_metadata() {
        assert_eq!(v("1.0.0-BETA"), v("1.0.0-beta"));
        assert_eq!(v("1.0.0+left"), v("1.0.0+right"));
        assert_eq!(v("1.0.0+meta"), v("1.0.0"));
    }

    #[test]
    fn strict_equality_is_nugets_identity() {
        // == follows Compare; eq_strict follows Equals. They disagree
        // exactly where NuGet's comparer is inconsistent with itself:
        // "-0" and "0" are both the integer zero to Compare but different
        // strings to Equals. ("01" vs "1" would be another such pair, but
        // leading-zero numeric labels don't parse in the first place.)
        assert_eq!(v("1.0.0--0"), v("1.0.0-0"));
        assert!(!v("1.0.0--0").eq_strict(&v("1.0.0-0")));
        assert!(v("1.0.0-BETA").eq_strict(&v("1.0.0-beta")));
        assert!(v("1.0.0+left").eq_strict(&v("1.0.0+right")));
    }

    #[test]
    fn full_string_carries_metadata() {
        assert_eq!(
            v("1.0.0-rc.1+build.5").to_full_string(),
            "1.0.0-rc.1+build.5"
        );
        assert_eq!(v("1.0.0-rc.1+build.5").to_normalized_string(), "1.0.0-rc.1");
    }

    #[test]
    fn obvious_rejections() {
        for bad in [
            "",
            "1.0.0-",
            "1.0.0+",
            "1..0",
            "1.0.0.0.0",
            "-1.0.0",
            "banana",
        ] {
            assert!(
                NuGetVersion::parse(bad).is_err(),
                "{bad:?} should not parse"
            );
        }
    }
}
