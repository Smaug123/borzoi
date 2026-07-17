//! Pure version-selection logic for `Microsoft.NET.Sdk` resolution.
//!
//! Phase 7b-v1a always picked the highest installed SDK. Real-world
//! projects use `global.json` to pin a specific version with a
//! [`RollForward`] policy describing what to accept when the exact
//! pin isn't installed — and the MSBuild `Sdk="Name/Version"` form
//! amounts to an exact pin. This module models both with a single
//! [`VersionSpec`] value and exposes [`select_sdk_version`], a pure
//! function from `(candidates, spec)` to the chosen version.
//!
//! Filesystem discovery (`global.json` upward walk, JSONC parsing)
//! lives in [`super::global_json`]: the shell constructs a
//! `VersionSpec` from those settings and hands it to the resolver.
//! This file deliberately knows nothing about either.
//!
//! ## Feature bands
//!
//! .NET SDK versions encode a "feature band" in the third numeric
//! component: in `9.0.100`, the band is `9.0.1xx` (third component
//! integer-divided by 100). Patch versions within a band share that
//! band but differ in the last two digits — `9.0.100`, `9.0.101`,
//! `9.0.199` are all in `9.0.1xx`. `9.0.200` is a *different* band.
//! Roll-forward policies operate on these bands; see [`SdkVersion`]
//! for the accessors.
//!
//! ## Selection model
//!
//! Each [`RollForward`] variant determines two things:
//!
//! 1. The **admission predicate** — which candidates the spec is
//!    willing to consider at all.
//! 2. The **selection rule** within the admitted set.
//!
//! `LatestPatch`/`LatestFeature`/`LatestMinor`/`LatestMajor` use a
//! flat "highest in admitted set" rule. `Patch`/`Feature`/`Minor`/
//! `Major` are *cascading*: they prefer the exact pin if installed,
//! else they pick the lowest band/minor/major *above* the pin that has
//! any candidates and take the highest within. This matches the
//! .NET SDK's behaviour described in the `global.json` docs ("rolls
//! forward to the next higher feature band" etc.).

use super::SdkVersion;

/// Roll-forward policy taken verbatim from .NET SDK's `global.json`
/// schema. Names match the JSON values (case-insensitive on the wire,
/// canonicalised here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollForward {
    /// Exact match only. The pin must be installed; anything else
    /// fails.
    Disable,
    /// Same `(major, minor, feature_band)` as the pin, patch `>=` pin.
    /// Prefers the exact pin if installed; else highest patch in band.
    /// This is .NET's default when a `version` is specified.
    Patch,
    /// Same `(major, minor)`, version `>=` pin. Cascades by feature
    /// band — lowest band `>=` pin's that has candidates wins, then
    /// highest within. Exact pin preferred if installed.
    Feature,
    /// Same `major`, version `>=` pin. Cascades by minor.
    Minor,
    /// Version `>=` pin (across majors). Cascades by major.
    Major,
    /// Same `(major, minor, feature_band)` as the pin; highest in
    /// band wins (ignoring where the pin sits within it — no exact
    /// preference).
    LatestPatch,
    /// Same `(major, minor)`; highest wins.
    LatestFeature,
    /// Same `major`; highest wins.
    LatestMinor,
    /// Highest installed wins overall. This is the spec implied by
    /// v1a's "no `global.json` discovered" branch, and .NET's default
    /// when no `version` is specified.
    LatestMajor,
}

/// A version constraint to apply when selecting an installed SDK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSpec {
    version: Option<SdkVersion>,
    roll_forward: RollForward,
    allow_prerelease: bool,
}

impl VersionSpec {
    /// Pin a specific version. `roll_forward` describes how to relax
    /// the pin.
    ///
    /// `allow_prerelease` is the *host-supplied default* — .NET's CLI
    /// host passes `true`, the VS host passes `false`. It can also be
    /// overridden by `sdk.allowPrerelease` in `global.json`; the
    /// caller has already collapsed those into a single boolean by
    /// the time it reaches this constructor.
    ///
    /// Independently, a prerelease pin unconditionally forces
    /// `allow_prerelease = true` (without this, the pin itself
    /// wouldn't satisfy, which would be a contradiction).
    /// dotnet-runtime's `from_nearest_global_file` applies the same
    /// override at the end of its construction.
    pub fn with_version(
        version: SdkVersion,
        roll_forward: RollForward,
        allow_prerelease: bool,
    ) -> Self {
        let allow_prerelease = allow_prerelease || version.is_prerelease();
        Self {
            version: Some(version),
            roll_forward,
            allow_prerelease,
        }
    }

    /// "No pin, take the latest." Reproduces v1a's behaviour. The only
    /// roll-forward policy that's well-defined without a pin is
    /// `LatestMajor`, so it's hard-coded.
    pub fn any_version(allow_prerelease: bool) -> Self {
        Self {
            version: None,
            roll_forward: RollForward::LatestMajor,
            allow_prerelease,
        }
    }

    pub fn version(&self) -> Option<&SdkVersion> {
        self.version.as_ref()
    }

    pub fn roll_forward(&self) -> RollForward {
        self.roll_forward
    }

    pub fn allow_prerelease(&self) -> bool {
        self.allow_prerelease
    }
}

/// Select the best installed SDK version against a spec.
///
/// Returns `None` if no candidate satisfies the spec.
///
/// `spec == None` reproduces v1a: pick the highest installed
/// (including prereleases — same semantics as a `VersionSpec` built
/// with `any_version(true)`, just stated separately for the
/// no-`global.json` call site).
pub fn select_sdk_version<'a>(
    candidates: &'a [SdkVersion],
    spec: Option<&VersionSpec>,
) -> Option<&'a SdkVersion> {
    let Some(spec) = spec else {
        return candidates.iter().max();
    };

    // Step 1: filter to the admitted set.
    let admitted: Vec<&SdkVersion> = candidates.iter().filter(|c| admits(c, spec)).collect();
    if admitted.is_empty() {
        return None;
    }

    // Step 2: apply the selection rule.
    let Some(pin) = spec.version.as_ref() else {
        // No pin ⇒ only `LatestMajor` makes sense (enforced by
        // constructors), and that means "highest".
        return admitted.into_iter().max();
    };

    match spec.roll_forward {
        RollForward::Disable => {
            // Admission already enforced exact match; just return it.
            admitted.into_iter().find(|c| *c == pin)
        }
        RollForward::LatestPatch
        | RollForward::LatestFeature
        | RollForward::LatestMinor
        | RollForward::LatestMajor => admitted.into_iter().max(),
        RollForward::Patch => {
            // Upstream's `exact_match_preferred()` is true for
            // `disable` and `patch`: probe the pin first, then fall
            // through to the cascade. Within Patch's admitted set
            // every candidate shares a band, so the cascade reduces
            // to "highest in band" — but the exact-pin shortcut can
            // still beat a strictly-higher patch in the same band.
            if let Some(exact) = admitted.iter().copied().find(|c| *c == pin) {
                return Some(exact);
            }
            cascade(&admitted)
        }
        RollForward::Feature | RollForward::Minor | RollForward::Major => cascade(&admitted),
    }
}

/// Does `spec` admit `candidate`? Implements the "admission predicate"
/// half of each [`RollForward`] variant's semantics.
fn admits(candidate: &SdkVersion, spec: &VersionSpec) -> bool {
    // Prerelease gating sits above the per-variant logic. If the pin
    // is itself a prerelease, `VersionSpec::with_version` coerced
    // `allow_prerelease` to `true`, so this branch only excludes
    // prereleases when the pin is stable (or no pin) and the caller
    // didn't opt in.
    if candidate.is_prerelease() && !spec.allow_prerelease {
        return false;
    }
    let Some(pin) = spec.version.as_ref() else {
        // No pin: only `LatestMajor` is meaningful, and it admits
        // anything past the prerelease filter.
        return matches!(spec.roll_forward, RollForward::LatestMajor);
    };
    match spec.roll_forward {
        RollForward::Disable => candidate == pin,
        RollForward::Patch => same_band(candidate, pin) && candidate >= pin,
        RollForward::Feature => {
            candidate.major() == pin.major() && candidate.minor() == pin.minor() && candidate >= pin
        }
        RollForward::Minor => candidate.major() == pin.major() && candidate >= pin,
        RollForward::Major => candidate >= pin,
        // The `Latest*` variants share their structural filter with
        // the non-latest variant of the same level; the difference
        // sits in the *selection rule* (max under filter vs. cascade
        // + exact-pin preference), not the admission predicate. In
        // particular, upstream's `matches_policy` enforces
        // `current >= requested_version` for every variant once a
        // pin is in play — including the `latest*` ones — so a
        // `latestMajor` pin to `9.0.100` still refuses `8.0.401`.
        RollForward::LatestPatch => same_band(candidate, pin) && candidate >= pin,
        RollForward::LatestFeature => {
            candidate.major() == pin.major() && candidate.minor() == pin.minor() && candidate >= pin
        }
        RollForward::LatestMinor => candidate.major() == pin.major() && candidate >= pin,
        RollForward::LatestMajor => candidate >= pin,
    }
}

/// Cascading selection for `Patch`/`Feature`/`Minor`/`Major`.
///
/// Mirrors upstream's pairwise `is_better_match` rule: among two
/// admitted candidates, take the higher when they share a
/// `(major, minor, feature_band)` triple, else take the lower
/// overall. Applied transitively, this picks the candidate whose
/// `(major, minor, feature_band)` triple is minimum and whose
/// version is maximum within that triple.
///
/// `Feature` keeps `(major, minor)` fixed via admission, so the key
/// effectively reduces to `feature_band`. `Patch` keeps the full
/// triple fixed, so the key is constant and the cascade degenerates
/// to "max in admitted". The exact-pin preference for `Patch` is
/// applied by the caller *before* invoking this function — upstream
/// only treats `disable` and `patch` as `exact_match_preferred`.
fn cascade<'a>(admitted: &[&'a SdkVersion]) -> Option<&'a SdkVersion> {
    let min_key = admitted.iter().map(|c| cascade_key(c)).min()?;
    admitted
        .iter()
        .copied()
        .filter(|c| cascade_key(c) == min_key)
        .max()
}

fn cascade_key(v: &SdkVersion) -> (u64, u64, u64) {
    (v.major(), v.minor(), v.feature_band())
}

fn same_band(a: &SdkVersion, b: &SdkVersion) -> bool {
    a.major() == b.major() && a.minor() == b.minor() && a.feature_band() == b.feature_band()
}

#[cfg(test)]
mod tests;
