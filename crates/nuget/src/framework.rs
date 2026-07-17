//! `NuGetFramework`: NuGet's target-framework model — TFM parsing (short
//! folder names and long `FrameworkName` forms), the compatibility
//! relation, and nearest-candidate selection. Differentially pinned to
//! `NuGet.Frameworks` in `tests/framework_diff.rs`.
//!
//! Slice 3 of `docs/nuget-restore-plan.md`. Unlike versions and ranges the
//! interesting risk here is *table completeness* (identifier aliases, the
//! netstandard support matrix, precedence in `GetNearest`), not grammar
//! wildness — the diff sweeps the zoo cross-product precisely because the
//! input population is nearly enumerable.

use std::fmt;

/// Canonical identifiers (NuGet's `FrameworkConstants.FrameworkIdentifiers`).
mod id {
    pub const NET_FRAMEWORK: &str = ".NETFramework";
    pub const NET_CORE_APP: &str = ".NETCoreApp";
    pub const NET_STANDARD: &str = ".NETStandard";
    pub const PORTABLE: &str = ".NETPortable";
    pub const DOTNET: &str = ".NETPlatform";
    pub const UAP: &str = "UAP";
    pub const WINDOWS: &str = "Windows";
    pub const WINRT: &str = "WinRT";
    pub const WINDOWS_PHONE: &str = "WindowsPhone";
    pub const WINDOWS_PHONE_APP: &str = "WindowsPhoneApp";
    pub const SILVERLIGHT: &str = "Silverlight";
    pub const MONO_ANDROID: &str = "MonoAndroid";
    pub const MONO_TOUCH: &str = "MonoTouch";
    pub const MONO_MAC: &str = "MonoMac";
    pub const XAMARIN_IOS: &str = "Xamarin.iOS";
    pub const XAMARIN_MAC: &str = "Xamarin.Mac";
    pub const XAMARIN_TVOS: &str = "Xamarin.TVOS";
    pub const XAMARIN_WATCHOS: &str = "Xamarin.WatchOS";
    pub const DNX: &str = "DNX";
    pub const DNXCORE: &str = "DNXCore";
    pub const ASPNET: &str = "ASP.NET";
    pub const ASPNETCORE: &str = "ASP.NETCore";
    pub const NATIVE: &str = "native";
    pub const NETCORE: &str = ".NETCore";
    pub const NETMF: &str = ".NETMicroFramework";
    pub const NETNANO: &str = ".NETnanoFramework";
    pub const TIZEN: &str = "Tizen";
    pub const ANY: &str = "Any";
    pub const AGNOSTIC: &str = "Agnostic";
    pub const UNSUPPORTED: &str = "Unsupported";
}

/// Short-prefix → canonical identifier, longest-prefix-first where aliases
/// nest (`netstandard` before `net`, `aspnetcore` before `aspnet`, …).
/// `net` itself is special-cased in [`parse_short`].
const SHORT_ALIASES: &[(&str, &str)] = &[
    ("netframework", id::NET_FRAMEWORK),
    ("netstandard", id::NET_STANDARD),
    ("netcoreapp", id::NET_CORE_APP),
    ("netcore", id::NETCORE),
    ("netnano", id::NETNANO),
    ("netmf", id::NETMF),
    ("uap", id::UAP),
    ("winrt", id::WINRT),
    ("win", id::WINDOWS),
    ("wpa", id::WINDOWS_PHONE_APP),
    ("wp", id::WINDOWS_PHONE),
    ("sl", id::SILVERLIGHT),
    ("monoandroid", id::MONO_ANDROID),
    ("monotouch", id::MONO_TOUCH),
    ("monomac", id::MONO_MAC),
    ("xamarinios", id::XAMARIN_IOS),
    ("xamarin.ios", id::XAMARIN_IOS),
    ("xamarinmac", id::XAMARIN_MAC),
    ("xamarin.mac", id::XAMARIN_MAC),
    ("xamarintvos", id::XAMARIN_TVOS),
    ("xamarin.tvos", id::XAMARIN_TVOS),
    ("xamarinwatchos", id::XAMARIN_WATCHOS),
    ("xamarin.watchos", id::XAMARIN_WATCHOS),
    ("dnxcore", id::DNXCORE),
    ("dnx", id::DNX),
    ("aspnetcore", id::ASPNETCORE),
    ("aspnet", id::ASPNET),
    ("dotnet", id::DOTNET),
    ("native", id::NATIVE),
    ("tizen", id::TIZEN),
    ("any", id::ANY),
    ("agnostic", id::AGNOSTIC),
    ("unsupported", id::UNSUPPORTED),
];

/// The complete PCL profile table, enumerated mechanically from the
/// oracle (`Profile{1..400}` swept through `GetShortFolderName`) — 44
/// real profiles. Members are the short names NuGet renders in
/// `portable-…` folder names, in NuGet's own print order.
const PCL_PROFILES: &[(u32, &[&str])] = &[
    (2, &["net40", "sl4", "win8", "wp7"]),
    (3, &["net40", "sl4"]),
    (4, &["net45", "sl4", "win8", "wp7"]),
    (5, &["net40", "win8"]),
    (6, &["net403", "win8"]),
    (7, &["net45", "win8"]),
    (14, &["net40", "sl5"]),
    (18, &["net403", "sl4"]),
    (19, &["net403", "sl5"]),
    (23, &["net45", "sl4"]),
    (24, &["net45", "sl5"]),
    (31, &["win81", "wp81"]),
    (32, &["win81", "wpa81"]),
    (36, &["net40", "sl4", "win8", "wp8"]),
    (37, &["net40", "sl5", "win8"]),
    (41, &["net403", "sl4", "win8"]),
    (42, &["net403", "sl5", "win8"]),
    (44, &["net451", "win81"]),
    (46, &["net45", "sl4", "win8"]),
    (47, &["net45", "sl5", "win8"]),
    (49, &["net45", "wp8"]),
    (78, &["net45", "win8", "wp8"]),
    (84, &["wp81", "wpa81"]),
    (88, &["net40", "sl4", "win8", "wp75"]),
    (92, &["net40", "win8", "wpa81"]),
    (95, &["net403", "sl4", "win8", "wp7"]),
    (96, &["net403", "sl4", "win8", "wp75"]),
    (102, &["net403", "win8", "wpa81"]),
    (104, &["net45", "sl4", "win8", "wp75"]),
    (111, &["net45", "win8", "wpa81"]),
    (136, &["net40", "sl5", "win8", "wp8"]),
    (143, &["net403", "sl4", "win8", "wp8"]),
    (147, &["net403", "sl5", "win8", "wp8"]),
    (151, &["net451", "win81", "wpa81"]),
    (154, &["net45", "sl4", "win8", "wp8"]),
    (157, &["win81", "wp81", "wpa81"]),
    (158, &["net45", "sl5", "win8", "wp8"]),
    (225, &["net40", "sl5", "win8", "wpa81"]),
    (240, &["net403", "sl5", "win8", "wpa81"]),
    (255, &["net45", "sl5", "win8", "wpa81"]),
    (259, &["net45", "win8", "wp8", "wpa81"]),
    (328, &["net40", "sl5", "win8", "wp8", "wpa81"]),
    (336, &["net403", "sl5", "win8", "wp8", "wpa81"]),
    (344, &["net45", "sl5", "win8", "wp8", "wpa81"]),
];

/// A parsed target framework.
///
/// Equality is NuGet's full-framework equality: identifier, version,
/// platform, and profile, all case-insensitively.
#[derive(Debug, Clone)]
pub struct NuGetFramework {
    framework: String,
    version: [u32; 4],
    platform: Option<(String, [u32; 4])>,
    profile: Option<String>,
}

/// Why a framework string failed to parse. NuGet almost never throws —
/// empty and whitespace inputs are the `Unsupported` *framework*, not
/// errors — but a handful of shapes do (a portable member with a profile,
/// a long form with no identifier or a malformed key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameworkParseError {
    /// A shape NuGet's own parser throws on.
    Invalid,
}

impl fmt::Display for FrameworkParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameworkParseError::Invalid => f.write_str("unparseable framework string"),
        }
    }
}

impl std::error::Error for FrameworkParseError {}

fn unsupported() -> NuGetFramework {
    NuGetFramework {
        framework: id::UNSUPPORTED.to_owned(),
        version: [0; 4],
        platform: None,
        profile: None,
    }
}

/// The long form's Version value: 1–4 dotted components, each tolerating
/// surrounding whitespace. NuGet accepts a single component and zero-fills
/// the rest (`.NETFramework,Version=v4` → 4.0.0.0) — unlike bare
/// `System.Version.Parse`, which requires two; oracle-pinned.
fn parse_long_version(text: &str) -> Option<[u32; 4]> {
    let parts: Vec<&str> = text.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut out = [0u32; 4];
    for (slot, part) in out.iter_mut().zip(&parts) {
        let t = part.trim_matches(char::is_whitespace);
        // int.TryParse leniency: an optional leading '+' per component
        // ("Version=v+4.5" parses) — oracle-pinned.
        let t = t.strip_prefix('+').unwrap_or(t);
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) || t.len() > 10 {
            return None;
        }
        let v: u64 = t.parse().ok()?;
        if v > i32::MAX as u64 {
            return None;
        }
        *slot = v as u32;
    }
    Some(out)
}

/// Parse a dotted version with up to four numeric components.
fn parse_dotted_version(text: &str) -> Option<[u32; 4]> {
    let mut out = [0u32; 4];
    let parts: Vec<&str> = text.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    for (slot, part) in out.iter_mut().zip(&parts) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) || part.len() > 10 {
            return None;
        }
        let v: u64 = part.parse().ok()?;
        if v > i32::MAX as u64 {
            return None;
        }
        *slot = v as u32;
    }
    Some(out)
}

/// Short-form version text → four components. Undotted digit strings of
/// length ≤ 4 expand digit-per-component (`45` → 4.5, `481` → 4.8.1);
/// dotted strings parse as written.
fn parse_short_version(text: &str) -> Option<[u32; 4]> {
    if text.is_empty() {
        return Some([0; 4]);
    }
    if text.contains('.') {
        return parse_dotted_version(text);
    }
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Digit-per-component, and only the first FOUR digits count — the rest
    // are silently discarded ("2147483648" is the version 2.1.4.7).
    let mut out = [0u32; 4];
    for (slot, b) in out.iter_mut().zip(text.bytes().take(4)) {
        *slot = (b - b'0') as u32;
    }
    Some(out)
}

/// Render a version as dotted text with at least `min_parts` components,
/// dropping trailing zeros beyond that.
fn dotted(version: [u32; 4], min_parts: usize) -> String {
    let mut last = 4;
    while last > min_parts && version[last - 1] == 0 {
        last -= 1;
    }
    version[..last]
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// Render a .NETFramework-style digit-concat version (`4.5.1` → `451`),
/// dropping trailing zeros but keeping at least two digits. A component
/// above 9 can't concat unambiguously — NuGet falls back to dotted form
/// (`4.50` renders "net4.50", not "net450").
fn digit_concat(version: [u32; 4]) -> String {
    let mut last = 4;
    while last > 2 && version[last - 1] == 0 {
        last -= 1;
    }
    if version[..last].iter().any(|&part| part > 9) {
        return dotted(version, 2);
    }
    let mut s = String::new();
    for part in &version[..last] {
        s.push_str(&part.to_string());
    }
    s
}

fn is_zero(version: [u32; 4]) -> bool {
    version == [0; 4]
}

impl NuGetFramework {
    /// `NuGetFramework.Parse`: accepts short TFMs (`net8.0`) and long
    /// `FrameworkName` forms (`.NETFramework,Version=v4.7.2`).
    pub fn parse(input: &str) -> Result<NuGetFramework, FrameworkParseError> {
        if input.contains(',') {
            parse_long(input)
        } else {
            parse_short(input)
        }
    }

    /// `NuGetFramework.ParseFolder`: the package-folder-name entry point
    /// (`lib/net45/`); short forms only.
    pub fn parse_folder(input: &str) -> Result<NuGetFramework, FrameworkParseError> {
        if input.contains(',') {
            // A comma is never valid in a folder name; NuGet yields
            // Unsupported rather than attempting the long form.
            return Ok(unsupported());
        }
        parse_short(input)
    }

    /// `GetShortFolderName()`, or `None` where NuGet's own implementation
    /// throws (some unsupported/agnostic shapes).
    pub fn short_folder_name(&self) -> Option<String> {
        let fw = self.framework.as_str();
        let v = self.version;
        let base = if fw.eq_ignore_ascii_case(id::NET_FRAMEWORK) {
            if is_zero(v) {
                "net".to_owned()
            } else {
                format!("net{}", digit_concat(v))
            }
        } else if fw.eq_ignore_ascii_case(id::NET_CORE_APP) {
            if v[0] >= 5 {
                let mut s = format!("net{}", dotted(v, 2));
                if let Some((platform, pv)) = &self.platform {
                    s.push('-');
                    s.push_str(&platform.to_ascii_lowercase());
                    if !is_zero(*pv) {
                        s.push_str(&dotted(*pv, 2));
                    }
                }
                s
            } else if is_zero(v) {
                "netcoreapp".to_owned()
            } else {
                format!("netcoreapp{}", dotted(v, 2))
            }
        } else if fw.eq_ignore_ascii_case(id::NET_STANDARD) {
            if is_zero(v) {
                "netstandard".to_owned()
            } else {
                format!("netstandard{}", dotted(v, 2))
            }
        } else if fw.eq_ignore_ascii_case(id::PORTABLE) {
            let tail = if is_zero(v) {
                String::new()
            } else {
                digit_concat(v)
            };
            // A profile-less portable renders just the version prefix, but
            // only for non-PCL versions (>= 5.0): ".NETPortable,Version=v7.3"
            // → "portable73", while a PCL-range profile-less portable
            // (v0/v4.x) has no short name (GetShortFolderName throws).
            // Oracle-pinned.
            let Some(profile) = self.profile.as_deref().filter(|p| !p.is_empty()) else {
                return if v[0] >= 5 {
                    Some(format!("portable{tail}"))
                } else {
                    None
                };
            };
            let number: Option<u32> = profile
                .to_ascii_lowercase()
                .strip_prefix("profile")
                .and_then(|n| n.parse().ok());
            // Profile rendering by portable version (oracle-pinned):
            // versions 0 and 4.x expand a known profile to its member
            // list and THROW for unknown numbers ("portable-profile2591"
            // has no short name); any other version renders the profile
            // lowercased verbatim, known or not
            // (".NETPortable,Version=v8.5,Profile=Profile259" is
            // "portable85-profile259", v7.3+Profile459 is
            // "portable73-profile459").
            match number {
                Some(n) if self.is_pcl() => match PCL_PROFILES.iter().find(|(p, _)| *p == n) {
                    Some((_, members)) => {
                        format!("portable{tail}-{}", members.join("+"))
                    }
                    None => return None,
                },
                Some(_) => format!("portable{tail}-{}", profile.to_ascii_lowercase()),
                // Verbatim-list profile: re-render each member's short
                // name, unparseable ones as "unsupported". PCL-range
                // portables (v < 5) sort and dedup the members; non-PCL
                // ones (v >= 5, like "portable85-wp8+win81") print in the
                // stored order. Oracle-pinned.
                None => {
                    let mut members: Vec<String> = profile
                        .split('+')
                        .map(|part| match parse_short(part) {
                            Ok(f) if !f.is_unsupported() => f
                                .short_folder_name()
                                .unwrap_or_else(|| "unsupported".to_owned()),
                            _ => "unsupported".to_owned(),
                        })
                        .collect();
                    if self.is_pcl() {
                        members.sort();
                        members.dedup();
                    }
                    format!("portable{tail}-{}", members.join("+"))
                }
            }
        } else if fw.eq_ignore_ascii_case(id::DOTNET) {
            // v0 always prints bare "dotnet" (profile or not:
            // "dotnet-6.0"); v5.0 prints bare only without a profile
            // ("dotnet5-.4" → "dotnet50-.4"); other versions digit-concat.
            // Oracle-pinned.
            if is_zero(v) || (v == [5, 0, 0, 0] && self.profile.is_none()) {
                "dotnet".to_owned()
            } else {
                format!("dotnet{}", digit_concat(v))
            }
        } else if fw.eq_ignore_ascii_case(id::UAP) {
            format!("uap{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::WINDOWS) {
            format!("win{}", tail1(v))
        } else if fw.eq_ignore_ascii_case(id::WINRT) {
            format!("winrt{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::WINDOWS_PHONE) {
            format!("wp{}", tail1(v))
        } else if fw.eq_ignore_ascii_case(id::WINDOWS_PHONE_APP) {
            // Two-digit minimum, unlike wp/win ("wpa8" prints "wpa80").
            format!("wpa{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::SILVERLIGHT) {
            format!("sl{}", tail1(v))
        } else if fw.eq_ignore_ascii_case(id::MONO_ANDROID) {
            format!("monoandroid{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::MONO_TOUCH) {
            format!("monotouch{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::MONO_MAC) {
            format!("monomac{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::XAMARIN_IOS) {
            format!("xamarinios{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::XAMARIN_MAC) {
            format!("xamarinmac{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::XAMARIN_TVOS) {
            format!("xamarintvos{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::XAMARIN_WATCHOS) {
            format!("xamarinwatchos{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::DNX) {
            format!("dnx{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::DNXCORE) {
            format!("dnxcore{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::ASPNET) {
            format!("aspnet{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::ASPNETCORE) {
            format!("aspnetcore{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::NATIVE) {
            format!("native{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::NETCORE) {
            format!("netcore{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::NETMF) {
            // Digit-concat, two digits minimum ("netmf1" prints "netmf10").
            format!("netmf{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::NETNANO) {
            format!(
                "netnano{}",
                if is_zero(v) {
                    String::new()
                } else {
                    dotted(v, 2)
                }
            )
        } else if fw.eq_ignore_ascii_case(id::TIZEN) {
            format!("tizen{}", tail2(v))
        } else if fw.eq_ignore_ascii_case(id::ANY) {
            "any".to_owned()
        } else if fw.eq_ignore_ascii_case(id::AGNOSTIC) {
            "agnostic".to_owned()
        } else if fw.eq_ignore_ascii_case(id::UNSUPPORTED) {
            "unsupported".to_owned()
        } else {
            // Unknown identifiers render sanitised: lowercase, all
            // non-alphanumerics stripped, digit-concat version tail
            // (".NE TStandard" v2.0 → "netstandard20") — oracle-pinned.
            let sanitized: String = fw
                .to_ascii_lowercase()
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect();
            if sanitized.is_empty() {
                return None;
            }
            format!(
                "{sanitized}{}",
                if is_zero(v) {
                    String::new()
                } else {
                    digit_concat(v)
                }
            )
        };
        let mut s = base;
        // The profile is rendered inline for net5.0+ .NETCoreApp (where the
        // suffix slot is the *platform*, appended in that branch), so it is
        // excluded here — but a *sub-net5* .NETCoreApp with a profile
        // ("netcoreapp-3.0" → profile "3.0") still needs the generic append.
        let net_core_platformed = fw.eq_ignore_ascii_case(id::NET_CORE_APP) && v[0] >= 5;
        if !fw.eq_ignore_ascii_case(id::PORTABLE)
            && !net_core_platformed
            && let Some(profile) = &self.profile
        {
            s.push('-');
            s.push_str(&profile_short(profile));
        }
        Some(s)
    }

    /// Canonical identifier (`.NETCoreApp`, …).
    pub fn framework(&self) -> &str {
        &self.framework
    }

    /// The four-part version, `System.Version.ToString()` style.
    pub fn version_string(&self) -> String {
        let [a, b, c, d] = self.version;
        format!("{a}.{b}.{c}.{d}")
    }

    /// Platform name for net5.0+ platformed TFMs.
    pub fn platform(&self) -> Option<&str> {
        self.platform.as_ref().map(|(name, _)| name.as_str())
    }

    /// Platform version (`0.0.0.0` when no platform).
    pub fn platform_version_string(&self) -> String {
        let [a, b, c, d] = self.platform.as_ref().map(|(_, v)| *v).unwrap_or([0; 4]);
        format!("{a}.{b}.{c}.{d}")
    }

    /// Profile, if any.
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// A real framework (not Any/Agnostic/Unsupported).
    pub fn is_specific_framework(&self) -> bool {
        !self.is_any()
            && !self.is_unsupported()
            && !self.framework.eq_ignore_ascii_case(id::AGNOSTIC)
    }

    /// The special `Unsupported` framework.
    pub fn is_unsupported(&self) -> bool {
        self.framework.eq_ignore_ascii_case(id::UNSUPPORTED)
    }

    /// The special `Any` framework.
    pub fn is_any(&self) -> bool {
        self.framework.eq_ignore_ascii_case(id::ANY)
    }

    /// A portable class library: `.NETPortable` with major version below 5
    /// (v7.3 portable long-forms exist and are *not* PCLs — oracle-pinned).
    pub fn is_pcl(&self) -> bool {
        self.framework.eq_ignore_ascii_case(id::PORTABLE) && self.version[0] < 5
    }

    /// `DefaultCompatibilityProvider.IsCompatible(project, candidate)`: can
    /// a project targeting `project` consume assets built for `candidate`?
    pub fn is_compatible(project: &NuGetFramework, candidate: &NuGetFramework) -> bool {
        compat::is_compatible(project, candidate)
    }

    /// `FrameworkReducer.GetNearest`: the index of the best candidate for
    /// `project`, or `None` when nothing is compatible.
    pub fn get_nearest(project: &NuGetFramework, candidates: &[NuGetFramework]) -> Option<usize> {
        nearest::get_nearest(project, candidates)
    }

    /// The exact `FrameworkReducer.GetNearest` mirror, for the callers whose
    /// answer must equal NuGet's rather than merely be compatible: nuspec
    /// dependency- and reference-group selection, and the content model's
    /// group tie-break ([`crate::assets`]).
    pub(crate) fn get_nearest_reducer(
        project: &NuGetFramework,
        candidates: &[NuGetFramework],
    ) -> Option<usize> {
        dependency_group_nearest::get_nearest(project, candidates)
    }

    /// The project frameworks this crate resolves against.
    ///
    /// These are the only frameworks an F# project targets, and — not
    /// coincidentally — the only ones on which the compatibility relation is
    /// differentially *exact* in both directions (see the `nearest` module's
    /// precision envelope). Outside them the resolver and asset selection
    /// decline rather than risk an incompatible pick.
    pub fn is_resolver_project_framework(&self) -> bool {
        matches!(
            self.framework(),
            id::NET_FRAMEWORK | id::NET_CORE_APP | id::NET_STANDARD
        )
    }

    /// `FrameworkConstants.CommonFrameworks.DotNet`: `.NETPlatform` at the
    /// *empty* version.
    ///
    /// Deliberately not reachable through [`Self::parse_folder`], because it is
    /// not the same thing: the short name `dotnet` **parses** to `.NETPlatform
    /// 5.0`. The content model's replacement table
    /// ([`crate::assets`], for the `any` folder) uses the constant rather than
    /// the parser, so a package holding both `lib/any/` and `lib/dotnet5.0/`
    /// has two distinct asset groups.
    pub(crate) fn dot_net_platform_empty_version() -> NuGetFramework {
        NuGetFramework {
            framework: id::DOTNET.to_owned(),
            version: [0; 4],
            platform: None,
            profile: None,
        }
    }

    /// A framework NuGet's content model could not parse: the raw folder name
    /// as the identifier, at version 0.0.0.0.
    ///
    /// This is the last arm of `ManagedCodeConventions`'
    /// `TargetFrameworkName_ParserCore` — a `lib/`-child folder whose name is
    /// neither a folder TFM nor a `FrameworkName` becomes a framework named
    /// after itself, which is then compatible with nothing.
    pub(crate) fn unknown_identifier(name: &str) -> NuGetFramework {
        NuGetFramework {
            framework: name.to_owned(),
            version: [0; 4],
            platform: None,
            profile: None,
        }
    }
}

/// win8 / win81 style tail: nothing at 0.0, else digit-concat trimmed to
/// one digit minimum.
fn tail1(version: [u32; 4]) -> String {
    if is_zero(version) {
        return String::new();
    }
    let mut last = 4;
    while last > 1 && version[last - 1] == 0 {
        last -= 1;
    }
    if version[..last].iter().any(|&part| part > 9) {
        return dotted(version, 2);
    }
    let mut s = String::new();
    for part in &version[..last] {
        s.push_str(&part.to_string());
    }
    s
}

/// monoandroid90 / dnx451 style tail: nothing at 0.0, else digit-concat
/// with two digits minimum.
fn tail2(version: [u32; 4]) -> String {
    if is_zero(version) {
        String::new()
    } else {
        digit_concat(version)
    }
}

/// Short-form parse. Never fails: unrecognised shapes become Unsupported,
/// matching `NuGetFramework.Parse`.
fn parse_short(input: &str) -> Result<NuGetFramework, FrameworkParseError> {
    // Whitespace anywhere in a short TFM — including the empty string —
    // yields Unsupported ("netStandard2.0- ", " net8.0",
    // "portable-Pro file7"; only the long comma form tolerates spaces).
    if input.is_empty() || input.contains(char::is_whitespace) {
        return Ok(unsupported());
    }
    let lower = input.to_ascii_lowercase(); // byte-length-preserving

    // Portable: "portable[<version>]-<list>", where the version is the
    // digit-concat form ("portable45-…" is v4.5, "portable73-…" is v7.3,
    // "portable-…" is v0.0). The list is sliced from the *original* input —
    // profiles are stored verbatim, case included ("portable-Profile7"
    // keeps "Profile7").
    // `netportable` is an accepted alias for the `portable` prefix.
    if let Some(rest) = lower
        .strip_prefix("portable")
        .or_else(|| lower.strip_prefix("netportable"))
    {
        // The version runs to the '-' and may be dotted, not just
        // digit-concat: a component above 9 forces dotted rendering, so
        // ".NETPortable,Version=v4.10" is folder "portable4.10-…".
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(rest.len());
        let (ver_text, tail) = rest.split_at(digits_end);
        if let Some(list) = tail.strip_prefix('-') {
            let version = parse_short_version(ver_text).unwrap_or([0; 4]);
            let list_start = input.len() - list.len();
            return parse_portable(version, &input[list_start..]);
        }
        return Ok(unsupported());
    }

    // Identifier = leading letters/dots; version+suffix = the rest.
    // Leading dots are stripped before alias lookup (".net5.0" parses as
    // net5.0, ".netstandard2.0" as netstandard2.0).
    let split = lower
        .find(|c: char| !c.is_ascii_alphabetic() && c != '.')
        .unwrap_or(lower.len());
    let (alias, rest) = lower.split_at(split);
    // ".net5.0" parses like "net5.0", but ".sl5"/".win8"/".uap" do NOT —
    // there is no general dot-strip, only the ".net" spelling plus the
    // canonical-identifier matches (".netstandard2.0" hits ".NETStandard"
    // case-insensitively).
    let alias = if alias == ".net" { "net" } else { alias };

    // The version runs to the '-' (if any); after it, platform or profile.
    let (version_text, suffix) = match rest.find('-') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };

    // Bare digits imply "net" ("45" and "4.5" are net45) — but only below
    // the .NETCoreApp split and above zero: "8.0" and "0" are Unsupported.
    // With an explicit "net" the split is purely the expanded major
    // version: >= 5 is .NETCoreApp even from digit expansion ("net931" is
    // .NETCoreApp 9.3.1), below is .NETFramework.
    // Implied net covers only the historic MSBuild default set
    // {2.0, 3.5, 4.0, 4.5} in either spelling ("45", "4.5"); "17", "46",
    // "451", "8.0" are all Unsupported — oracle-pinned.
    // …and only in those exact spellings: "450" expands to the same
    // version as "45" yet is Unsupported.
    let implied_net = alias.is_empty()
        && matches!(
            version_text,
            "20" | "35" | "40" | "45" | "2.0" | "3.5" | "4.0" | "4.5"
        );
    if alias.is_empty() && !implied_net {
        return Ok(unsupported());
    }
    let (framework, version) = if alias == "net" || implied_net {
        let Some(version) = parse_short_version(version_text) else {
            return Ok(unsupported());
        };
        if version[0] >= 5 {
            (id::NET_CORE_APP, version)
        } else {
            (id::NET_FRAMEWORK, version)
        }
    } else {
        let canonical = SHORT_ALIASES
            .iter()
            .find(|(a, _)| *a == alias)
            .map(|(_, c)| *c)
            .or_else(|| {
                // The canonical identifier spelling is itself accepted in
                // short form (".netstandard2.0", "silverlight5") —
                // case-insensitive. Note this matches only the *dotless*
                // canonicals plus the explicit aliases above; the dotted
                // ones NuGet canonicalises in short form (.NETFramework →
                // "netframework") are the SHORT_ALIASES entries, and the
                // ones it does *not* (.NETMicroFramework, .NETPlatform)
                // correctly stay unmatched here → Unsupported.
                CANONICAL_IDENTIFIERS
                    .iter()
                    .find(|c| c.eq_ignore_ascii_case(alias))
                    .copied()
            });
        let Some(canonical) = canonical else {
            return Ok(unsupported());
        };
        // The special frameworks reject any version digits *or* suffix:
        // "agnostic1", "any-foo", "agnostic-bar" are all Unsupported
        // (never Any/Agnostic with a profile) — oracle-pinned.
        if matches!(canonical, id::ANY | id::AGNOSTIC | id::UNSUPPORTED)
            && (!version_text.is_empty() || suffix.is_some())
        {
            return Ok(unsupported());
        }
        let Some(mut version) = parse_short_version(version_text) else {
            return Ok(unsupported());
        };
        if version_text.is_empty() && suffix.is_none() {
            // Bare "dotnet" is .NETPlatform 5.0, but a suffix suppresses
            // the default ("dotnet-6.0" is v0.0, profile "6.0").
            version = default_version(canonical);
        }
        (canonical, version)
    };

    let mut result = NuGetFramework {
        framework: framework.to_owned(),
        version,
        platform: None,
        profile: None,
    };

    if let Some(suffix) = suffix {
        if framework == id::NET_CORE_APP && version[0] >= 5 {
            // Platform grammar — a DELIBERATE deviation from NuGet.
            // NuGet's own parser salvages irrational shapes ("+windows-"
            // is a platform but "windows-" is not; "windows-0" parses but
            // "windows-1" does not — backtracking artifacts). We accept
            // exactly the real-world shape: letters, then an optional
            // digit-led dotted version ("windows", "windows10.0.19041",
            // "ios15.0"). Anything else is Unsupported here, and the
            // resolver must decline any package whose folder TFM parses
            // Unsupported (docs/nuget-restore-plan.md). The differential
            // tests carve this out by checking the oracle's platform
            // field against the same letters-only grammar.
            let boundary = suffix
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(suffix.len());
            let (name, pv_text) = suffix.split_at(boundary);
            if name.is_empty() {
                return Ok(unsupported());
            }
            let pv = if pv_text.is_empty() {
                [0; 4]
            } else if pv_text.starts_with(|c: char| c.is_ascii_digit())
                && let Some(pv) = parse_dotted_version(pv_text)
            {
                pv
            } else {
                return Ok(unsupported());
            };
            // Original case preserved ("net8.0-Windows" reports
            // Platform="Windows"); lowering happens at render time.
            let original_name = &input[input.len() - suffix.len()..][..name.len()];
            result.platform = Some((original_name.to_owned(), pv));
        } else {
            // Pre-net5 profile suffix — original case preserved
            // ("NET1.0-WINDOWS" reports Profile="WINDOWS"). Charset is
            // letters/digits/dots/dashes; anything else ("8_0") makes
            // the whole TFM Unsupported — oracle-pinned.
            // '+' is fine ("netstandard-2.0+x" has profile "2.0+x");
            // '_' is not ("net-8_0" is Unsupported) — oracle-pinned.
            if suffix.is_empty()
                || !suffix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+')
            {
                return Ok(unsupported());
            }
            let original = &input[input.len() - suffix.len()..];
            // A "-full" suffix is dropped entirely (net45-full ≡ net45,
            // netstandard2.0-full ≡ netstandard2.0); other profiles are
            // canonicalised. Oracle-pinned.
            result.profile = canonical_profile(original);
        }
    }
    Ok(result)
}

/// Profile spellings NuGet canonicalises (matched case-insensitively).
/// `full` → `None` (dropped); `client`/`cf` → their canonical spellings;
/// everything else is kept verbatim.
fn canonical_profile(profile: &str) -> Option<String> {
    let lower = profile.to_ascii_lowercase();
    match lower.as_str() {
        "full" => return None,
        "client" => return Some("Client".to_owned()),
        "cf" => return Some("CompactFramework".to_owned()),
        _ => {}
    }
    // A `wp`-prefixed profile expands to `WindowsPhone`, keeping any version
    // suffix ("wp" → "WindowsPhone", "wp71" → "WindowsPhone71") — rendered
    // back to `wp…` by `profile_short`. Oracle-pinned.
    if let Some(rest) = lower.strip_prefix("wp") {
        return Some(format!("WindowsPhone{rest}"));
    }
    Some(profile.to_owned())
}

/// The short-folder-name spelling of a profile — the inverse of the
/// canonicalisation above: `CompactFramework` → `cf`, `WindowsPhone…` →
/// `wp…`, everything else lowercases (`Client` → `client`, `Profile7` →
/// `profile7`).
fn profile_short(profile: &str) -> String {
    if profile.eq_ignore_ascii_case("CompactFramework") {
        "cf".to_owned()
    } else if let Some(rest) = profile
        .to_ascii_lowercase()
        .strip_prefix("windowsphone")
        .map(str::to_owned)
    {
        format!("wp{rest}")
    } else {
        profile.to_ascii_lowercase()
    }
}

/// Zero-version PCL members inherit their family's canonical version for
/// profile lookup (win → win8, wp → wp7, wpa → wpa81).
fn member_default_version(framework: &str) -> [u32; 4] {
    match framework {
        id::WINDOWS => [8, 0, 0, 0],
        id::WINDOWS_PHONE => [7, 0, 0, 0],
        id::WINDOWS_PHONE_APP => [8, 1, 0, 0],
        _ => [0; 4],
    }
}

fn default_version(framework: &str) -> [u32; 4] {
    match framework {
        // "dotnet" bare means .NETPlatform 5.0; everything else is 0.0.
        id::DOTNET => [5, 0, 0, 0],
        _ => [0; 4],
    }
}

/// `portable-net45+win8` → .NETPortable v0.0. A member list matching a
/// known profile stores `ProfileN`; `portable-profileN` names one
/// directly (kept verbatim, case included); any other list — even with
/// unparseable members — stores the *verbatim list* as the profile.
fn parse_portable(version: [u32; 4], list: &str) -> Result<NuGetFramework, FrameworkParseError> {
    if list.is_empty() {
        return Ok(unsupported());
    }
    let pcl = |profile: String| NuGetFramework {
        framework: id::PORTABLE.to_owned(),
        version,
        platform: None,
        profile: Some(profile),
    };
    let lower = list.to_ascii_lowercase();
    if lower
        .strip_prefix("profile")
        .is_some_and(|n| n.parse::<u32>().is_ok())
    {
        return Ok(pcl(list.to_owned()));
    }
    // Empty members are dropped ("portable-win8+net45+" is Profile7).
    let mut members: Vec<String> = Vec::new();
    for part in list.split('+').filter(|p| !p.is_empty()) {
        let mut f = parse_short(part)?;
        // A member carrying a profile makes the whole parse THROW
        // ("portable-net-40+sl5+…") — oracle-pinned.
        if f.profile.is_some() {
            return Err(FrameworkParseError::Invalid);
        }
        // Members normalise zero versions to the family default before
        // profile lookup: "win0" ≡ "win" ≡ win8, so
        // "portable-net45+win0" is Profile7 — oracle-pinned.
        if is_zero(f.version) {
            f.version = member_default_version(&f.framework);
        }
        members.push(if f.is_unsupported() {
            "unsupported".to_owned()
        } else {
            f.short_folder_name()
                .unwrap_or_else(|| "unsupported".to_owned())
        });
    }
    members.sort();
    members.dedup();
    let matched = PCL_PROFILES.iter().find(|(_, table)| {
        table.len() == members.len()
            && table
                .iter()
                .zip(&members)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
    });
    Ok(match matched {
        Some((number, _)) => pcl(format!("Profile{number}")),
        None => pcl(list.to_owned()),
    })
}

/// Long `FrameworkName` form: `Identifier,Version=vX.Y[,Profile=…]`.
fn parse_long(input: &str) -> Result<NuGetFramework, FrameworkParseError> {
    let mut framework: Option<String> = None;
    let mut version = [0u32; 4];
    let mut profile: Option<String> = None;
    let platform: Option<(String, [u32; 4])> = None;

    for raw in input.split(',') {
        let part = raw.trim();
        // Empty parts are skipped entirely; the first non-empty part is
        // the identifier (",45" parses with identifier "45").
        if part.is_empty() {
            continue;
        }
        if framework.is_none() {
            framework = Some(canonical_identifier(part));
            continue;
        }
        // After the identifier: known keys (matched case-insensitively)
        // apply; unknown keys and stray non-key parts are silently
        // IGNORED (".NETFramework,Version9=v4.5" is net 0.0;
        // ".NETCoreApp,Version=v8.0,Platform=wind,ows" drops both the
        // Platform value and the "ows" remnant). The one thing that
        // throws is a recognised Version key with an unparseable value —
        // and only a *lowercase* 'v' prefix is stripped, so "Version=V4.0"
        // throws while "version=v 4.5" parses (component whitespace is
        // Version-parse leniency). All oracle-pinned.
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        // The key is NOT trimmed: "Profile =Client" (trailing space) is
        // an unknown key and is ignored — oracle-pinned.
        match key.to_ascii_lowercase().as_str() {
            "version" => {
                // The 'v' strips only as the value's FIRST character —
                // "Version= v5.4" (space before the v) throws, while
                // "Version=v 4.5" parses (component whitespace is
                // System.Version leniency). Oracle-pinned.
                let v = value.strip_prefix('v').unwrap_or(value);
                version = parse_long_version(v).ok_or(FrameworkParseError::Invalid)?;
            }
            "profile" => {
                let v = value.trim();
                // A portable Profile value containing '-' throws
                // (".NETPortable,…,Profile=Pro-file259") — oracle-pinned.
                if framework.as_deref() == Some(id::PORTABLE) && v.contains('-') {
                    return Err(FrameworkParseError::Invalid);
                }
                if !v.is_empty() {
                    profile = Some(v.to_owned());
                }
            }
            // Accepted and discarded — the long form never yields a
            // platformed framework.
            "platform" => {}
            _ => {}
        }
    }

    Ok(NuGetFramework {
        framework: framework.ok_or(FrameworkParseError::Invalid)?,
        version,
        platform,
        profile,
    })
}

const CANONICAL_IDENTIFIERS: &[&str] = &[
    id::NET_FRAMEWORK,
    id::NET_CORE_APP,
    id::NET_STANDARD,
    id::PORTABLE,
    id::DOTNET,
    id::UAP,
    id::WINDOWS,
    id::WINRT,
    id::WINDOWS_PHONE,
    id::WINDOWS_PHONE_APP,
    id::SILVERLIGHT,
    id::MONO_ANDROID,
    id::MONO_TOUCH,
    id::MONO_MAC,
    id::XAMARIN_IOS,
    id::XAMARIN_MAC,
    id::XAMARIN_TVOS,
    id::XAMARIN_WATCHOS,
    id::DNX,
    id::DNXCORE,
    id::ASPNET,
    id::ASPNETCORE,
    id::NATIVE,
    id::NETCORE,
    id::NETMF,
    id::NETNANO,
    id::TIZEN,
    id::ANY,
    id::AGNOSTIC,
    id::UNSUPPORTED,
];

/// Map a long-form identifier onto its canonical spelling when known —
/// including via the short-alias table ("net,45" is .NETFramework,
/// "sl,3" is Silverlight); otherwise keep it verbatim (NuGet preserves
/// unknown identifiers).
fn canonical_identifier(identifier: &str) -> String {
    if let Some(c) = CANONICAL_IDENTIFIERS
        .iter()
        .find(|c| c.eq_ignore_ascii_case(identifier))
    {
        return (*c).to_owned();
    }
    let lower = identifier.to_ascii_lowercase();
    if lower == "net" || lower == ".net" {
        return id::NET_FRAMEWORK.to_owned();
    }
    if lower == "portable" || lower == "netportable" {
        return id::PORTABLE.to_owned();
    }
    // The dotless-canonical short aliases (netframework → .NETFramework,
    // netstandard → .NETStandard, silverlight → Silverlight, …) also
    // canonicalise the long form "NETFramework,Version=v4.5". The dotted
    // canonicals NuGet does *not* alias (.NETMicroFramework, .NETPlatform)
    // are absent from SHORT_ALIASES, so "NETPlatform,Version=v5.4" correctly
    // stays verbatim — oracle-pinned.
    if let Some((_, c)) = SHORT_ALIASES.iter().find(|(a, _)| *a == lower) {
        return (*c).to_owned();
    }
    identifier.to_owned()
}

impl PartialEq for NuGetFramework {
    fn eq(&self, other: &Self) -> bool {
        self.framework.eq_ignore_ascii_case(&other.framework)
            && self.version == other.version
            && match (&self.platform, &other.platform) {
                (None, None) => true,
                (Some((an, av)), Some((bn, bv))) => an.eq_ignore_ascii_case(bn) && av == bv,
                _ => false,
            }
            && match (&self.profile, &other.profile) {
                (None, None) => true,
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                _ => false,
            }
    }
}

impl Eq for NuGetFramework {}

/// `DefaultCompatibilityProvider` behaviourally: equality, same-identifier
/// version dominance, the one-way cross-framework table, and the special
/// Any/Agnostic rules. Every entry oracle-pinned by the zoo cross-product.
mod compat {
    use super::{NuGetFramework, id};

    /// One-way mappings: (project identifier, min ≤ version < max-excl)
    /// accepts (candidate identifier, max candidate version).
    struct Mapping {
        project: &'static str,
        project_min: [u32; 4],
        /// Exclusive project ceiling — the dotnet-generation rows stop at
        /// net5.0 (netcoreapp3.1 ← dotnet5.6, but net5.0+ ← dotnet is
        /// false). `None` = unbounded.
        project_max_excl: Option<[u32; 4]>,
        candidate: &'static str,
        candidate_max: [u32; 4],
    }

    const NS: &str = id::NET_STANDARD;

    /// project-family ← netstandard support matrix plus the other one-way
    /// compat edges. Seeded from the documented tables; the diff corrects.
    const MAPPINGS: &[Mapping] = &[
        // .NETCoreApp ← .NETStandard
        m(id::NET_CORE_APP, [1, 0, 0, 0], NS, [1, 6, 0, 0]),
        m(id::NET_CORE_APP, [2, 0, 0, 0], NS, [2, 0, 0, 0]),
        m(id::NET_CORE_APP, [3, 0, 0, 0], NS, [2, 1, 0, 0]),
        // .NETFramework ← .NETStandard
        m(id::NET_FRAMEWORK, [4, 5, 0, 0], NS, [1, 1, 0, 0]),
        m(id::NET_FRAMEWORK, [4, 5, 1, 0], NS, [1, 2, 0, 0]),
        m(id::NET_FRAMEWORK, [4, 6, 0, 0], NS, [1, 3, 0, 0]),
        m(id::NET_FRAMEWORK, [4, 6, 1, 0], NS, [2, 0, 0, 0]),
        // UAP ← .NETStandard
        m(id::UAP, [10, 0, 0, 0], NS, [1, 4, 0, 0]),
        m(id::UAP, [10, 0, 15064, 0], NS, [2, 0, 0, 0]),
        // Xamarin / Mono ← .NETStandard
        m(id::MONO_ANDROID, [0, 0, 0, 0], NS, [2, 1, 0, 0]),
        m(id::MONO_TOUCH, [0, 0, 0, 0], NS, [2, 1, 0, 0]),
        m(id::MONO_MAC, [0, 0, 0, 0], NS, [2, 1, 0, 0]),
        m(id::XAMARIN_IOS, [0, 0, 0, 0], NS, [2, 1, 0, 0]),
        m(id::XAMARIN_MAC, [0, 0, 0, 0], NS, [2, 1, 0, 0]),
        m(id::XAMARIN_TVOS, [0, 0, 0, 0], NS, [2, 1, 0, 0]),
        m(id::XAMARIN_WATCHOS, [0, 0, 0, 0], NS, [2, 1, 0, 0]),
        // Tizen ← .NETStandard
        // Bare/pre-4.0 Tizen reaches netstandard 1.6; 4.0 raises it to 2.0.
        m(id::TIZEN, [0, 0, 0, 0], NS, [1, 6, 0, 0]),
        m(id::TIZEN, [4, 0, 0, 0], NS, [2, 0, 0, 0]),
        m(id::TIZEN, [6, 0, 0, 0], NS, [2, 1, 0, 0]),
        // Windows-family ← .NETStandard
        m(id::WINDOWS, [8, 0, 0, 0], NS, [1, 1, 0, 0]),
        m(id::WINDOWS, [8, 1, 0, 0], NS, [1, 2, 0, 0]),
        m(id::WINDOWS_PHONE, [8, 0, 0, 0], NS, [1, 0, 0, 0]),
        m(id::WINDOWS_PHONE_APP, [8, 1, 0, 0], NS, [1, 2, 0, 0]),
        // DNXCore ← .NETStandard
        m(id::DNXCORE, [5, 0, 0, 0], NS, [1, 5, 0, 0]),
        // dotnet generations (.NETPlatform): .NETFramework 4.5+ and the
        // Xamarin/Mono family accept them; .NETCoreApp does NOT (all
        // versions — oracle-pinned).
        m(id::NET_FRAMEWORK, [4, 5, 0, 0], id::DOTNET, [5, 2, 0, 0]),
        m(id::NET_FRAMEWORK, [4, 5, 1, 0], id::DOTNET, [5, 3, 0, 0]),
        m(id::NET_FRAMEWORK, [4, 6, 0, 0], id::DOTNET, [5, 4, 0, 0]),
        m(id::NET_FRAMEWORK, [4, 6, 1, 0], id::DOTNET, [5, 5, 0, 0]),
        m(id::NET_FRAMEWORK, [4, 6, 2, 0], id::DOTNET, [5, 6, 0, 0]),
        // Xamarin/Mono accept dotnet-generation candidates (but NOT bare
        // .NETFramework ones — oracle-pinned; their PCL acceptance is the
        // optional-framework rule in is_compatible).
        m(id::MONO_ANDROID, [0, 0, 0, 0], id::DOTNET, [5, 6, 0, 0]),
        m(id::MONO_TOUCH, [0, 0, 0, 0], id::DOTNET, [5, 6, 0, 0]),
        m(id::MONO_MAC, [0, 0, 0, 0], id::DOTNET, [5, 6, 0, 0]),
        m(id::XAMARIN_IOS, [0, 0, 0, 0], id::DOTNET, [5, 6, 0, 0]),
        m(id::XAMARIN_MAC, [0, 0, 0, 0], id::DOTNET, [5, 6, 0, 0]),
        m(id::XAMARIN_TVOS, [0, 0, 0, 0], id::DOTNET, [5, 6, 0, 0]),
        m(id::XAMARIN_WATCHOS, [0, 0, 0, 0], id::DOTNET, [5, 6, 0, 0]),
        // UAP accepts the Windows-family and dotnet generations.
        m(id::UAP, [10, 0, 0, 0], id::WINDOWS, [8, 1, 0, 0]),
        m(id::UAP, [10, 0, 0, 0], id::WINDOWS_PHONE_APP, [8, 1, 0, 0]),
        m(id::UAP, [10, 0, 0, 0], id::WINRT, [4, 5, 0, 0]),
        m(id::UAP, [10, 0, 0, 0], id::NETCORE, [5, 0, 0, 0]),
        m(id::UAP, [10, 0, 0, 0], id::DOTNET, [5, 5, 0, 0]),
        // Windows-family netstandard/dotnet reach. Any-version Windows
        // gets win8's ceilings (the Windows-floor rule's cross-family
        // counterpart); 8.1 raises them.
        m(id::WINDOWS, [0, 0, 0, 0], NS, [1, 1, 0, 0]),
        m(id::WINDOWS, [0, 0, 0, 0], id::DOTNET, [5, 2, 0, 0]),
        m(id::WINDOWS, [8, 1, 0, 0], id::DOTNET, [5, 3, 0, 0]),
        m(id::WINDOWS_PHONE, [8, 0, 0, 0], id::DOTNET, [5, 1, 0, 0]),
        m(id::WINDOWS_PHONE, [8, 1, 0, 0], id::DOTNET, [5, 3, 0, 0]),
        m(
            id::WINDOWS_PHONE_APP,
            [8, 1, 0, 0],
            id::DOTNET,
            [5, 3, 0, 0],
        ),
        // Synthetic .NETCore 5.0+ package-based TFMs (`netcore50`,
        // `netcore60`, ...) keep their own identifier but still inherit the
        // legacy project.json fallback reach. These are not the Windows
        // Store `netcore45`/`netcore451` aliases handled by normalization.
        m(id::NETCORE, [5, 0, 0, 0], NS, [1, 4, 0, 0]),
        m(id::NETCORE, [5, 0, 0, 0], id::DOTNET, [5, 5, 0, 0]),
        m(id::NETCORE, [5, 0, 0, 0], id::WINDOWS, [8, 1, 0, 0]),
        // DNXCore (and ASP.NETCore via translation).
        m(id::DNXCORE, [5, 0, 0, 0], id::DOTNET, [5, 6, 0, 0]),
    ];

    const fn m(
        project: &'static str,
        project_min: [u32; 4],
        candidate: &'static str,
        candidate_max: [u32; 4],
    ) -> Mapping {
        Mapping {
            project,
            project_min,
            project_max_excl: None,
            candidate,
            candidate_max,
        }
    }

    fn profiles_match(project: &NuGetFramework, candidate: &NuGetFramework) -> bool {
        // .NETFramework's Client/Full/absent profiles are mutually
        // equivalent (net4.5 accepts a Profile=Client candidate).
        let equivalent = |f: &NuGetFramework| {
            f.framework.eq_ignore_ascii_case(id::NET_FRAMEWORK)
                && f.profile.as_deref().is_none_or(|p| {
                    p.eq_ignore_ascii_case("Client") || p.eq_ignore_ascii_case("Full")
                })
        };
        if equivalent(project) && equivalent(candidate) {
            return true;
        }
        match (&project.profile, &candidate.profile) {
            (None, None) => true,
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }

    /// A PCL candidate's member frameworks, from the profile table
    /// (ProfileN) or a verbatim member list.
    fn pcl_members(candidate: &NuGetFramework) -> Vec<NuGetFramework> {
        let Some(profile) = candidate.profile.as_deref() else {
            return Vec::new();
        };
        let number: Option<u32> = profile
            .to_ascii_lowercase()
            .strip_prefix("profile")
            .and_then(|n| n.parse().ok());
        let shorts: Vec<String> = match number {
            Some(n) => match super::PCL_PROFILES.iter().find(|(p, _)| *p == n) {
                Some((_, members)) => members.iter().map(|m| (*m).to_owned()).collect(),
                None => return Vec::new(),
            },
            None => profile.split('+').map(str::to_owned).collect(),
        };
        shorts
            .iter()
            .filter_map(|m| NuGetFramework::parse_folder(m).ok())
            .filter(|f| f.is_specific_framework())
            .collect()
    }

    /// The profile *number* of a PCL candidate — directly from a `ProfileN`
    /// profile, or by resolving a verbatim member list to its known profile.
    fn pcl_profile_number(candidate: &NuGetFramework) -> Option<u32> {
        let profile = candidate.profile.as_deref()?;
        if let Some(n) = profile
            .to_ascii_lowercase()
            .strip_prefix("profile")
            .and_then(|n| n.parse().ok())
        {
            return Some(n);
        }
        let mut members: Vec<String> = profile
            .split('+')
            .map(|part| match super::parse_short(part) {
                Ok(f) if !f.is_unsupported() => f.short_folder_name().unwrap_or_default(),
                _ => "unsupported".to_owned(),
            })
            .collect();
        members.sort();
        members.dedup();
        super::PCL_PROFILES
            .iter()
            .find(|(_, table)| {
                table.len() == members.len()
                    && table
                        .iter()
                        .zip(&members)
                        .all(|(a, b)| a.eq_ignore_ascii_case(b))
            })
            .map(|(n, _)| *n)
    }

    /// The PCL profiles the Xamarin family and net6.0+ android accept
    /// (NuGet's Xamarin-supported portable set — a Xamarin project takes
    /// *these* PCL assets, not every PCL). Oracle-enumerated.
    const XAMARIN_PCL_PROFILES: &[u32] = &[
        5, 6, 7, 14, 19, 24, 37, 42, 44, 47, 49, 78, 92, 102, 111, 136, 147, 151, 158, 225, 255,
        259, 328, 336, 344,
    ];

    /// Compat folds several dead identifiers onto their live equivalents so
    /// the mapping table stays small (all oracle-pinned):
    ///
    /// - **`.NETCore`** (the Windows-8 Store framework, later renamed
    ///   `Windows`/`win`) ≡ Windows: `netcore45` ≡ win8.0, `netcore451` ≡
    ///   win8.1, bare `netcore` ≡ win8.0.
    /// - **WinRT** ≡ Windows 8.0 — but *asymmetrically*. A WinRT **project**
    ///   has a full Windows-8 project's reach regardless of its (synthetic)
    ///   version, so it always folds to Windows 8.0. A WinRT **candidate**
    ///   folds only up to WinRT 4.5 (its true Windows-8 equivalent);
    ///   `winrt46`/`winrt8` assets are *newer* than the Windows-8 they'd
    ///   claim, so they stay WinRT and fall through to incompatible.
    /// - **UAP below 10.0** floors to UAP 10.0 (`uap`, `uap10`≡v1.0,
    ///   `uap80`≡v8.0 all behave as uap10.0).
    /// - Bare Windows/WindowsPhone/WindowsPhoneApp take their canonical
    ///   default version.
    fn normalize(f: &NuGetFramework, role: Role) -> NuGetFramework {
        let mut out = f.clone();
        if out.framework.eq_ignore_ascii_case(id::NETCORE) && out.version <= [4, 5, 1, 0] {
            // .NETCore only ever shipped 4.5 / 4.5.1 (≡ win8.0 / win8.1);
            // higher (synthetic) versions like netcore50 stay .NETCore and
            // use explicit project.json compatibility mappings.
            out.framework = id::WINDOWS.to_owned();
            out.version = if out.version == [4, 5, 1, 0] {
                [8, 1, 0, 0]
            } else {
                [8, 0, 0, 0]
            };
        } else if out.framework.eq_ignore_ascii_case(id::WINRT)
            && (role == Role::Project || out.version <= [4, 5, 0, 0])
        {
            out.framework = id::WINDOWS.to_owned();
            out.version = [8, 0, 0, 0];
        } else if out.framework.eq_ignore_ascii_case(id::UAP) && out.version < [10, 0, 0, 0] {
            out.version = [10, 0, 0, 0];
        } else if out.version == [0; 4] {
            let default: Option<[u32; 4]> = if out.framework.eq_ignore_ascii_case(id::WINDOWS) {
                Some([8, 0, 0, 0])
            } else if out.framework.eq_ignore_ascii_case(id::WINDOWS_PHONE) {
                Some([7, 0, 0, 0])
            } else if out.framework.eq_ignore_ascii_case(id::WINDOWS_PHONE_APP) {
                Some([8, 1, 0, 0])
            } else {
                None
            };
            if let Some(v) = default {
                out.version = v;
            }
        }
        out
    }

    /// Which side of `IsCompatible(project, candidate)` a framework sits on
    /// — WinRT normalization differs by role.
    #[derive(PartialEq, Clone, Copy)]
    enum Role {
        Project,
        Candidate,
    }

    /// Per-profile netstandard support ceiling (NuGet's
    /// `PortableFrameworkMappings.CompatibilityMappings`).
    const PCL_NETSTANDARD: &[(u32, [u32; 4])] = &[
        (7, [1, 1, 0, 0]),
        (31, [1, 0, 0, 0]),
        (32, [1, 2, 0, 0]),
        (44, [1, 2, 0, 0]),
        (49, [1, 0, 0, 0]),
        (78, [1, 0, 0, 0]),
        (84, [1, 0, 0, 0]),
        (111, [1, 1, 0, 0]),
        (151, [1, 2, 0, 0]),
        (157, [1, 0, 0, 0]),
        (259, [1, 0, 0, 0]),
    ];

    /// The DNX/ASP.NET legacy family behaves as a .NETFramework
    /// equivalent for cross-identifier purposes (dnx451 accepts exactly
    /// what net451 does, aspnet50 exactly what net45 does), and
    /// ASP.NETCore as DNXCore 5.0 — all oracle-pinned.
    fn translate(f: &NuGetFramework) -> NuGetFramework {
        let mut out = f.clone();
        if out.framework.eq_ignore_ascii_case(id::DNX) {
            out.framework = id::NET_FRAMEWORK.to_owned();
        } else if out.framework.eq_ignore_ascii_case(id::ASPNET) {
            out.framework = id::NET_FRAMEWORK.to_owned();
            out.version = [4, 5, 0, 0];
        } else if out.framework.eq_ignore_ascii_case(id::ASPNETCORE) {
            out.framework = id::DNXCORE.to_owned();
            out.version = [5, 0, 0, 0];
        }
        out
    }

    pub fn is_compatible(project: &NuGetFramework, candidate: &NuGetFramework) -> bool {
        // Special frameworks: Any either side is compatible; equal
        // frameworks are compatible (including Agnostic == Agnostic and
        // Unsupported == Unsupported — two unparseable TFMs count as
        // compatible, oracle-pinned); an Agnostic *candidate* otherwise
        // fits any specific project, while an Agnostic project accepts
        // nothing further; Unsupported matches nothing else.
        if candidate.is_any() || project.is_any() {
            return true;
        }
        if project == candidate {
            return true;
        }
        if candidate
            .framework
            .eq_ignore_ascii_case(super::id::AGNOSTIC)
        {
            return project.is_specific_framework();
        }
        if project.is_unsupported() || candidate.is_unsupported() {
            return false;
        }
        // WinRT accepts WinRT by version (winrt8 accepts winrt46) — checked
        // before normalization, which folds a WinRT *project* to Windows
        // (losing the identity) but keeps a WinRT *candidate* above 4.5.
        if project.framework.eq_ignore_ascii_case(id::WINRT)
            && candidate.framework.eq_ignore_ascii_case(id::WINRT)
        {
            // Bare `winrt` ≡ WinRT 4.5 (its Windows-8 equivalent), so it
            // still accepts winrt45.
            let pv = if project.version == [0; 4] {
                [4, 5, 0, 0]
            } else {
                project.version
            };
            return candidate.version <= pv;
        }
        let project = &normalize(project, Role::Project);
        let candidate = &normalize(candidate, Role::Candidate);
        if project == candidate {
            return true;
        }

        // Same-family legacy pairs resolve before translation (dnx452
        // accepts dnx451 by the ordinary same-identifier version rule);
        // the DNX family also accepts its ASP.NET contemporaries
        // directly (dnx451 accepts aspnet50, dnxcore50 accepts
        // aspnetcore50 — versions not compared, oracle-pinned).
        if project.framework.eq_ignore_ascii_case(&candidate.framework)
            && matches!(
                project.framework.to_ascii_lowercase().as_str(),
                "dnx" | "dnxcore" | "asp.net" | "asp.netcore"
            )
        {
            return candidate.version <= project.version && profiles_match(project, candidate);
        }
        if project.framework.eq_ignore_ascii_case(id::DNX)
            && candidate.framework.eq_ignore_ascii_case(id::ASPNET)
        {
            return true;
        }
        if project.framework.eq_ignore_ascii_case(id::DNXCORE)
            && candidate.framework.eq_ignore_ascii_case(id::ASPNETCORE)
        {
            return true;
        }

        // Platform fallback: an android-platformed project accepts
        // MonoAndroid candidates of any version, tizen accepts Tizen — and
        // via the PCL bolt-on below, android inherits MonoAndroid's blanket
        // PCL acceptance. Each fallback only applies from the .NET version
        // that *introduced* the platform: android arrived in net6.0
        // (net5.0-android does NOT accept MonoAndroid), tizen in net5.0.
        // Other platforms (ios, macos, maccatalyst, tvos) get no fallback.
        // All oracle-pinned.
        let platform_fallback: Option<&str> = project.platform.as_ref().and_then(|(name, _)| {
            if !project.framework.eq_ignore_ascii_case(id::NET_CORE_APP) {
                None
            } else if name.eq_ignore_ascii_case("android") && project.version >= [6, 0, 0, 0] {
                Some(id::MONO_ANDROID)
            } else if name.eq_ignore_ascii_case("tizen") && project.version >= [6, 0, 0, 0] {
                Some(id::TIZEN)
            } else {
                None
            }
        });
        if let Some(fallback) = platform_fallback
            && candidate.framework.eq_ignore_ascii_case(fallback)
        {
            return true;
        }

        // Translation is one-way: a DNX/ASP.NET *project* reaches like
        // its netfx equivalent, but as a *candidate* it stays itself
        // (net45 does not accept aspnet50 assets).
        let project = &translate(project);
        if project == candidate {
            return true;
        }

        // A PCL candidate — checked before the same-identifier rule
        // because portable *versions* are ignored entirely (v0.0 accepts
        // v4.5 of the same profile): compatible when the project is
        // compatible with any member, or the project sits in the
        // Xamarin bolt-on family (MonoMac excluded — oracle-pinned).
        if candidate.is_pcl() {
            let members = pcl_members(candidate);
            if members.is_empty() {
                return false;
            }
            if project.is_pcl() {
                let pm = pcl_members(project);
                return !pm.is_empty()
                    && pm
                        .iter()
                        .all(|p| members.iter().any(|c| is_compatible(p, c)));
            }
            // The Xamarin family (and net6.0+ android via the fallback)
            // accepts a *fixed set* of PCL profiles — NuGet's
            // Xamarin-supported portable table — not every PCL. A PCL whose
            // profile is outside the set (e.g. Profile3 = net40+sl4) is
            // rejected even though its members look reachable.
            const XAMARIN_FAMILY: &[&str] = &[
                id::MONO_ANDROID,
                id::MONO_TOUCH,
                id::XAMARIN_IOS,
                id::XAMARIN_MAC,
                id::XAMARIN_TVOS,
                id::XAMARIN_WATCHOS,
            ];
            if XAMARIN_FAMILY
                .iter()
                .any(|x| project.framework.eq_ignore_ascii_case(x))
                || platform_fallback == Some(id::MONO_ANDROID)
            {
                return pcl_profile_number(candidate)
                    .is_some_and(|n| XAMARIN_PCL_PROFILES.contains(&n));
            }
            return members.iter().any(|member| is_compatible(project, member));
        }

        // Same identifier: candidate version must not exceed the project's,
        // profiles must match, with platform rules for net5.0+.
        if project.framework.eq_ignore_ascii_case(&candidate.framework) {
            // Windows floor: every Windows project accepts win8-or-older
            // candidates regardless of its own parsed version — "win10"
            // parses as Windows *1.0* (digit expansion) yet accepts win8
            // and inherits win8's cross-family reach (oracle-pinned).
            if project.framework.eq_ignore_ascii_case(id::WINDOWS)
                && candidate.version <= [8, 0, 0, 0]
                && profiles_match(project, candidate)
            {
                return true;
            }
            if candidate.version > project.version || !profiles_match(project, candidate) {
                return false;
            }
            return match (&project.platform, &candidate.platform) {
                (_, None) => true,
                (None, Some(_)) => false,
                (Some((pn, pv)), Some((cn, cv))) => pn.eq_ignore_ascii_case(cn) && cv <= pv,
            };
        }

        // Platformed or profiled candidates never match cross-identifier.
        if candidate.platform.is_some() || candidate.profile.is_some() {
            return false;
        }

        // A PCL *project*: same-profile PCL candidates match regardless
        // of portable version, and each profile supports netstandard up
        // to a fixed ceiling (Profile259 → 1.0; silverlight-bearing
        // profiles → none) — oracle-pinned.
        if project.is_pcl() {
            if candidate.is_pcl() {
                // Member subset: every project member must be satisfied
                // by some candidate member (portable version ignored) —
                // Profile7 accepts Profile259, net45+sl5+win8 accepts
                // net40+sl5+win8+wp8.
                let (pm, cm) = (pcl_members(project), pcl_members(candidate));
                return !pm.is_empty()
                    && !cm.is_empty()
                    && pm.iter().all(|p| cm.iter().any(|c| is_compatible(p, c)));
            }
            if candidate.framework.eq_ignore_ascii_case(id::NET_STANDARD) {
                let ceiling = project
                    .profile
                    .as_deref()
                    .and_then(|p| {
                        p.to_ascii_lowercase()
                            .strip_prefix("profile")
                            .and_then(|n| n.parse::<u32>().ok())
                    })
                    .and_then(|n| PCL_NETSTANDARD.iter().find(|(p, _)| *p == n))
                    .map(|(_, v)| *v);
                return match ceiling {
                    Some(max) => candidate.version <= max,
                    None => false,
                };
            }
            return false;
        }

        // A profiled project (other than .NETFramework's Client/Full, which
        // are ≡ profileless) gets no cross-identifier reach: a
        // CompactFramework project does not accept netstandard/dotnet
        // assets — oracle-pinned.
        let profileless = project.profile.as_deref().is_none_or(|p| {
            project.framework.eq_ignore_ascii_case(id::NET_FRAMEWORK)
                && (p.eq_ignore_ascii_case("Client") || p.eq_ignore_ascii_case("Full"))
        });
        if !profileless {
            return false;
        }

        MAPPINGS.iter().any(|mapping| {
            project.framework.eq_ignore_ascii_case(mapping.project)
                && project.version >= mapping.project_min
                && mapping
                    .project_max_excl
                    .is_none_or(|max| project.version < max)
                && candidate.framework.eq_ignore_ascii_case(mapping.candidate)
                && candidate.version <= mapping.candidate_max
        })
    }
}

/// `FrameworkReducer.GetNearest` behaviourally: keep the compatible
/// candidates, reduce upwards under the compatibility relation itself
/// (drop any candidate that another candidate *accepts* — so
/// `portable-profile259` beats `netstandard0.0` because the PCL accepts
/// it, and `net45` beats `dotnet5.1` for a DNX project), then tie-break
/// the surviving maximal elements: same identifier as the project (its
/// platform-fallback family included) first, highest version,
/// platform-presence match, first occurrence. Non-specific *projects*
/// return None.
///
/// ## Precision envelope
///
/// The **compatibility** relation (which candidates are even eligible) is
/// exact — that is the correctness-critical part, since it decides whether
/// an asset may be consumed at all. The **tie-break** among mutually
/// compatible candidates is exact on realistic candidate sets (versions of
/// one framework family, the shape a real package's `lib/` folders take —
/// pinned by `framework_diff.rs`), but *approximate* on synthetic
/// heterogeneous mixes where NuGet's `FrameworkReducer` consults a larger
/// cross-family precedence table than the coarse banding below (e.g. for a
/// `uap` project it ranks `winrt` above `netstandard`). Every such
/// disagreement is a choice between two candidates NuGet also deems
/// compatible, so the worst case is selecting a less-specific-but-still-
/// compatible asset — an optimality gap, never an incompatible pick. The
/// `soak` test asserts the correctness invariant (we pick iff a compatible
/// candidate exists, and homogeneous sets resolve exactly); slice 8's
/// end-to-end differential over real packages is where exact folder
/// selection meets reality.
mod nearest {
    use super::NuGetFramework;

    pub fn get_nearest(project: &NuGetFramework, candidates: &[NuGetFramework]) -> Option<usize> {
        if !project.is_specific_framework() {
            return None;
        }
        let compatible: Vec<usize> = (0..candidates.len())
            .filter(|&i| NuGetFramework::is_compatible(project, &candidates[i]))
            .collect();
        if compatible.is_empty() {
            return None;
        }

        // Specific candidates dominate Any/Agnostic/Unsupported ones; the
        // latter only win when nothing specific is compatible.
        let (specific, fallback): (Vec<usize>, Vec<usize>) = compatible
            .iter()
            .partition(|&&i| candidates[i].is_specific_framework());
        let pool = if specific.is_empty() {
            fallback
        } else {
            specific
        };

        // Reduce upwards: drop c when some other candidate accepts c.
        let maximal: Vec<usize> = pool
            .iter()
            .copied()
            .filter(|&i| {
                !pool.iter().any(|&j| {
                    j != i
                        && !(candidates[j] == candidates[i])
                        && NuGetFramework::is_compatible(&candidates[j], &candidates[i])
                })
            })
            .collect();
        let mut group = if maximal.is_empty() { pool } else { maximal };

        // Tie-break the maximal set: same identifier as the project,
        // then NuGet's framework precedence (netstandard outranks the
        // dotnet generations, PCLs come last — FrameworkReducer's
        // precedence tables), then highest version, platform-presence
        // match, first occurrence.
        // "Same family" includes the legacy translation: a DNX/ASP.NET
        // project's own family is .NETFramework (dnx451 picks NET45 over
        // netstandard1.2), ASP.NETCore's is DNXCore.
        let family: &str = if project.framework().eq_ignore_ascii_case(super::id::DNX)
            || project.framework().eq_ignore_ascii_case(super::id::ASPNET)
        {
            super::id::NET_FRAMEWORK
        } else if project
            .framework()
            .eq_ignore_ascii_case(super::id::ASPNETCORE)
        {
            super::id::DNXCORE
        } else {
            project.framework()
        };
        // A platformed net5.0+ project's platform-fallback assets
        // (MonoAndroid/Tizen for android/tizen) rank *below* the modern
        // same-identifier .NET assets: NuGet prefers a net6.0 folder over a
        // legacy monoandroid one. They land in the generic cross-family
        // band (2), so a same-identifier net asset of any version wins over
        // them — the correctness-favouring choice (modern .NET over legacy
        // Xamarin). The exact NuGet threshold (the fallback outranks
        // *net5.0* but not net6.0) is cross-family tie-break precedence,
        // documented-approximate on `get_nearest` and tolerated by the
        // differential harnesses; both picks are always compatible.
        let band = |c: &NuGetFramework| -> u32 {
            if c.framework().eq_ignore_ascii_case(project.framework())
                || c.framework().eq_ignore_ascii_case(family)
            {
                0
            } else if c.framework().eq_ignore_ascii_case(super::id::NET_STANDARD) {
                1
            } else if c.is_pcl() {
                4
            } else if c.framework().eq_ignore_ascii_case(super::id::DOTNET) {
                3
            } else {
                2
            }
        };
        let profile_number = |c: &NuGetFramework| -> u32 {
            c.profile()
                .and_then(|p| {
                    p.to_ascii_lowercase()
                        .strip_prefix("profile")
                        .and_then(|n| n.parse().ok())
                })
                .unwrap_or(u32::MAX)
        };
        group.sort_by(|&a, &b| {
            let (ca, cb) = (&candidates[a], &candidates[b]);
            band(ca)
                .cmp(&band(cb))
                .then_with(|| {
                    // PCL-vs-PCL ties go to the lower profile number
                    // (oracle-pinned on Profile47 vs Profile111).
                    if ca.is_pcl() && cb.is_pcl() {
                        profile_number(ca).cmp(&profile_number(cb))
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then_with(|| cb.version.cmp(&ca.version))
                .then_with(|| {
                    let pa = ca.platform.is_some() == project.platform.is_some();
                    let pb = cb.platform.is_some() == project.platform.is_some();
                    pb.cmp(&pa)
                })
                .then(a.cmp(&b))
        });
        group.first().copied()
    }
}

/// Exact `FrameworkReducer.GetNearest` path for nuspec dependency groups.
///
/// Asset folder selection can tolerate the public `get_nearest` tie-break
/// envelope because every disagreement is still a compatible asset. Dependency
/// groups are different: the selected group changes the restore graph, so this
/// mirrors NuGet's reducer steps rather than the coarse cross-family banding.
mod dependency_group_nearest {
    use std::cmp::Ordering;

    use super::{NuGetFramework, PCL_PROFILES, id};

    pub fn get_nearest(project: &NuGetFramework, candidates: &[NuGetFramework]) -> Option<usize> {
        let mut possible: Vec<usize> = (0..candidates.len()).collect();
        if possible.iter().any(|&i| !candidates[i].is_unsupported()) {
            possible.retain(|&i| !candidates[i].is_unsupported());
        }

        if let Some(index) = possible
            .iter()
            .copied()
            .find(|&i| full_eq(project, &candidates[i]))
        {
            return Some(index);
        }

        let compatible = possible
            .into_iter()
            .filter(|&i| NuGetFramework::is_compatible(project, &candidates[i]))
            .collect::<Vec<_>>();
        if compatible.is_empty() {
            return None;
        }

        let mut reduced = reduce_upwards(candidates, compatible);
        let is_net6_era = is_net5_era(project) && project.version[0] >= 6;

        if reduced.len() > 1 && reduced.iter().any(|&i| same_name(&candidates[i], project)) {
            reduced.retain(|&i| {
                let candidate = &candidates[i];
                if is_net6_era
                    && project.platform.is_some()
                    && (same_id(candidate.framework(), id::MONO_ANDROID)
                        || same_id(candidate.framework(), id::TIZEN))
                {
                    true
                } else {
                    same_name(candidate, project)
                }
            });
        }

        if reduced.len() > 1 {
            let any_pcl = reduced.iter().any(|&i| candidates[i].is_pcl());
            let any_non_pcl = reduced.iter().any(|&i| !candidates[i].is_pcl());
            if any_pcl && any_non_pcl {
                reduced.retain(|&i| !candidates[i].is_pcl());
            } else if any_pcl {
                reduced = if project.is_pcl() {
                    nearest_pcl_to_pcl(project, candidates, reduced)
                } else {
                    nearest_non_pcl_to_pcl(project, candidates, reduced)
                };
                if reduced.len() > 1
                    && let Some(best) = best_pcl(candidates, &reduced)
                {
                    reduced = vec![best];
                }
            }
        }

        if reduced.len() > 1
            && !is_package_based(project)
            && reduced.iter().any(|&i| is_package_based(&candidates[i]))
            && reduced.iter().any(|&i| !is_package_based(&candidates[i]))
        {
            reduced.retain(|&i| !is_package_based(&candidates[i]));
        }

        if reduced.len() > 1 && !reduced.iter().any(|&i| candidates[i].is_pcl()) {
            if let Some(project_profile) = project.profile.as_deref() {
                let same_profile = reduced
                    .iter()
                    .copied()
                    .filter(|&i| {
                        let candidate = &candidates[i];
                        same_name(candidate, project)
                            && candidate.profile.as_deref().is_some_and(|profile| {
                                profile.eq_ignore_ascii_case(project_profile)
                            })
                    })
                    .collect::<Vec<_>>();
                if !same_profile.is_empty() {
                    reduced = same_profile;
                }
            }

            if reduced.len() > 1
                && reduced.iter().any(|&i| candidates[i].profile.is_some())
                && reduced.iter().any(|&i| candidates[i].profile.is_none())
            {
                reduced.retain(|&i| candidates[i].profile.is_none());
            }
        }

        if reduced.len() > 1 && project.platform.is_some() {
            let has_modern_same_name = reduced
                .iter()
                .any(|&i| same_name(&candidates[i], project) && candidates[i].version[0] >= 6);
            if !is_net6_era || has_modern_same_name {
                let highest_same_name_version = reduced
                    .iter()
                    .copied()
                    .filter(|&i| same_name(&candidates[i], project))
                    .map(|i| candidates[i].version)
                    .max();
                if let Some(version) = highest_same_name_version {
                    reduced.retain(|&i| {
                        same_name(&candidates[i], project) && candidates[i].version == version
                    });
                }
            } else if is_net6_era
                && reduced.iter().any(|&i| {
                    same_id(candidates[i].framework(), id::MONO_ANDROID)
                        || same_id(candidates[i].framework(), id::TIZEN)
                })
            {
                let pick_tizen = reduced
                    .iter()
                    .any(|&i| same_id(candidates[i].framework(), id::TIZEN));
                reduced.retain(|&i| {
                    if pick_tizen {
                        same_id(candidates[i].framework(), id::TIZEN)
                    } else {
                        same_id(candidates[i].framework(), id::MONO_ANDROID)
                    }
                });
            }
        }

        match reduced.len() {
            0 => None,
            1 => reduced.first().copied(),
            _ => {
                reduced.sort_by(|&a, &b| {
                    final_compare(&candidates[a], &candidates[b]).then(a.cmp(&b))
                });
                reduced.first().copied()
            }
        }
    }

    fn reduce_upwards(candidates: &[NuGetFramework], mut input: Vec<usize>) -> Vec<usize> {
        if input.iter().any(|&i| !candidates[i].is_any()) {
            input.retain(|&i| !candidates[i].is_any());
        }

        let input = distinct_full(candidates, input);
        let mut results = Vec::new();
        for &i in &input {
            let mut duplicate = false;
            for &j in &input {
                if i == j {
                    continue;
                }
                if NuGetFramework::is_compatible(&candidates[j], &candidates[i]) {
                    let reverse_compatible =
                        NuGetFramework::is_compatible(&candidates[i], &candidates[j]);
                    duplicate = !reverse_compatible;
                    if reverse_compatible && same_name(&candidates[i], &candidates[j]) {
                        duplicate =
                            is_zero(candidates[i].version) && !is_zero(candidates[j].version);
                    }
                }
                if duplicate {
                    break;
                }
            }
            if !duplicate {
                results.push(i);
            }
        }

        results
            .sort_by(|&a, &b| reduce_core_compare(&candidates[a], &candidates[b]).then(a.cmp(&b)));
        results
    }

    fn distinct_full(candidates: &[NuGetFramework], input: Vec<usize>) -> Vec<usize> {
        let mut output = Vec::new();
        for index in input {
            if !output
                .iter()
                .any(|&existing| full_eq(&candidates[existing], &candidates[index]))
            {
                output.push(index);
            }
        }
        output
    }

    fn nearest_non_pcl_to_pcl(
        project: &NuGetFramework,
        candidates: &[NuGetFramework],
        reduced: Vec<usize>,
    ) -> Vec<usize> {
        let mut members = Vec::new();
        for &index in &reduced {
            for member in pcl_members(&candidates[index]) {
                members.push(member);
            }
        }
        let Some(nearest_member) =
            get_nearest(project, &members).map(|index| members[index].clone())
        else {
            return reduced;
        };
        reduced
            .into_iter()
            .filter(|&index| {
                pcl_members(&candidates[index])
                    .iter()
                    .any(|member| full_eq(member, &nearest_member))
            })
            .collect()
    }

    fn nearest_pcl_to_pcl(
        project: &NuGetFramework,
        candidates: &[NuGetFramework],
        reduced: Vec<usize>,
    ) -> Vec<usize> {
        let project_members = reduce_equivalent(pcl_members(project));
        let mut all_members = Vec::new();
        for &index in &reduced {
            for member in pcl_members(&candidates[index]) {
                if !all_members
                    .iter()
                    .any(|existing| full_eq(existing, &member))
                {
                    all_members.push(member);
                }
            }
        }

        let mut scores: Vec<(usize, u32)> = reduced.iter().copied().map(|i| (i, 0)).collect();
        for project_member in project_members {
            let Some(nearest_member) =
                get_nearest(&project_member, &all_members).map(|index| all_members[index].clone())
            else {
                continue;
            };
            for (index, score) in &mut scores {
                if pcl_members(&candidates[*index])
                    .iter()
                    .any(|member| full_eq(member, &nearest_member))
                {
                    *score += 1;
                }
            }
        }

        let Some(best_score) = scores.iter().map(|(_, score)| *score).max() else {
            return reduced;
        };
        scores
            .into_iter()
            .filter_map(|(index, score)| (score == best_score).then_some(index))
            .collect()
    }

    fn reduce_equivalent(frameworks: Vec<NuGetFramework>) -> Vec<NuGetFramework> {
        let mut indices = (0..frameworks.len()).collect::<Vec<_>>();
        indices.sort_by(|&a, &b| {
            equivalent_compare(&frameworks[a], &frameworks[b])
                .then_with(|| nuget_framework_sort_compare(&frameworks[b], &frameworks[a]))
                .then(a.cmp(&b))
        });

        let mut output = Vec::new();
        for index in indices {
            let candidate = &frameworks[index];
            if !output.iter().any(|existing| {
                full_eq(existing, candidate) || equivalent_frameworks(existing, candidate)
            }) {
                output.push(candidate.clone());
            }
        }
        output
    }

    fn best_pcl(candidates: &[NuGetFramework], reduced: &[usize]) -> Option<usize> {
        let mut current = None;
        for &index in reduced {
            if current.is_none_or(|current_index| {
                is_better_pcl(&candidates[current_index], &candidates[index])
            }) {
                current = Some(index);
            }
        }
        current
    }

    fn is_better_pcl(current: &NuGetFramework, considering: &NuGetFramework) -> bool {
        let current_members = pcl_members(current);
        let considering_members = pcl_members(considering);

        match considering_members.len().cmp(&current_members.len()) {
            Ordering::Less => return true,
            Ordering::Greater => return false,
            Ordering::Equal => {}
        }

        let mut current_highest = 0u32;
        let mut considering_highest = 0u32;
        for member in &considering_members {
            if let Some(current_member) = current_members
                .iter()
                .find(|candidate| same_id(candidate.framework(), member.framework()))
            {
                match member.version.cmp(&current_member.version) {
                    Ordering::Less => current_highest += 1,
                    Ordering::Greater => considering_highest += 1,
                    Ordering::Equal => {}
                }
            }
        }
        match considering_highest.cmp(&current_highest) {
            Ordering::Greater => return true,
            Ordering::Less => return false,
            Ordering::Equal => {}
        }

        let current_net = current_members
            .iter()
            .find(|member| same_id(member.framework(), id::NET_FRAMEWORK));
        let considering_net = considering_members
            .iter()
            .find(|member| same_id(member.framework(), id::NET_FRAMEWORK));
        if let (Some(current_net), Some(considering_net)) = (current_net, considering_net) {
            match considering_net.version.cmp(&current_net.version) {
                Ordering::Greater => return true,
                Ordering::Less => return false,
                Ordering::Equal => {}
            }
        }

        let current_name = current.short_folder_name().unwrap_or_default();
        let considering_name = considering.short_folder_name().unwrap_or_default();
        ci_cmp(&considering_name, &current_name) == Ordering::Less
    }

    fn pcl_members(framework: &NuGetFramework) -> Vec<NuGetFramework> {
        let Some(profile) = framework.profile.as_deref() else {
            return Vec::new();
        };
        let number = profile
            .to_ascii_lowercase()
            .strip_prefix("profile")
            .and_then(|n| n.parse::<u32>().ok());
        let members = match number {
            Some(number) => match PCL_PROFILES.iter().find(|(profile, _)| *profile == number) {
                Some((_, members)) => members.to_vec(),
                None => return Vec::new(),
            },
            None => profile.split('+').collect::<Vec<_>>(),
        };
        members
            .iter()
            .filter_map(|member| NuGetFramework::parse_folder(member).ok())
            .filter(NuGetFramework::is_specific_framework)
            .collect()
    }

    fn final_compare(a: &NuGetFramework, b: &NuGetFramework) -> Ordering {
        framework_precedence_compare(a, b)
            .then_with(|| nuget_framework_sort_compare(b, a))
            .then_with(|| framework_hash(a).cmp(&framework_hash(b)))
    }

    fn reduce_core_compare(a: &NuGetFramework, b: &NuGetFramework) -> Ordering {
        ci_cmp(a.framework(), b.framework()).then_with(|| {
            let a_name = framework_display_for_sort(a);
            let b_name = framework_display_for_sort(b);
            ci_cmp(&a_name, &b_name)
        })
    }

    fn framework_precedence_compare(a: &NuGetFramework, b: &NuGetFramework) -> Ordering {
        let a_package_based = is_package_based(a) && !is_netcore50_and_up(a);
        let b_package_based = is_package_based(b) && !is_netcore50_and_up(b);
        match a_package_based.cmp(&b_package_based) {
            Ordering::Equal => {}
            ordering => return ordering,
        }

        let precedence = if a_package_based {
            package_based_precedence
        } else {
            non_package_based_precedence
        };
        precedence(a.framework()).cmp(&precedence(b.framework()))
    }

    fn equivalent_compare(a: &NuGetFramework, b: &NuGetFramework) -> Ordering {
        equivalent_precedence(a.framework()).cmp(&equivalent_precedence(b.framework()))
    }

    fn non_package_based_precedence(framework: &str) -> u32 {
        if same_id(framework, id::NET_FRAMEWORK) {
            0
        } else if same_id(framework, id::NETCORE) {
            1
        } else if same_id(framework, id::WINDOWS) {
            2
        } else if same_id(framework, id::WINDOWS_PHONE_APP) {
            3
        } else {
            u32::MAX
        }
    }

    fn package_based_precedence(framework: &str) -> u32 {
        if same_id(framework, id::NET_CORE_APP) {
            0
        } else if same_id(framework, id::NET_STANDARD) {
            2
        } else if same_id(framework, id::DOTNET) {
            3
        } else {
            u32::MAX
        }
    }

    fn equivalent_precedence(framework: &str) -> u32 {
        if same_id(framework, id::WINDOWS) {
            0
        } else if same_id(framework, id::NETCORE) {
            1
        } else if same_id(framework, id::WINRT) {
            2
        } else if same_id(framework, id::WINDOWS_PHONE) {
            3
        } else if same_id(framework, id::SILVERLIGHT) {
            4
        } else if same_id(framework, id::DNXCORE) {
            5
        } else if same_id(framework, id::ASPNETCORE) {
            6
        } else if same_id(framework, id::DNX) {
            7
        } else if same_id(framework, id::ASPNET) {
            8
        } else {
            u32::MAX
        }
    }

    fn nuget_framework_sort_compare(a: &NuGetFramework, b: &NuGetFramework) -> Ordering {
        if a.is_any() && !b.is_any() {
            return Ordering::Less;
        }
        if !a.is_any() && b.is_any() {
            return Ordering::Greater;
        }
        if a.is_unsupported() && !b.is_unsupported() {
            return Ordering::Greater;
        }
        if !a.is_unsupported() && b.is_unsupported() {
            return Ordering::Less;
        }
        ci_cmp(a.framework(), b.framework())
            .then_with(|| a.version.cmp(&b.version))
            .then_with(|| opt_ci_cmp(a.profile.as_deref(), b.profile.as_deref()))
            .then_with(|| {
                opt_ci_cmp(
                    a.platform.as_ref().map(|(name, _)| name.as_str()),
                    b.platform.as_ref().map(|(name, _)| name.as_str()),
                )
            })
            .then_with(|| platform_version(a).cmp(&platform_version(b)))
    }

    fn framework_hash(framework: &NuGetFramework) -> u64 {
        // NuGet uses NuGetFramework.GetHashCode only as the last deterministic
        // tie-break. This FNV-1a hash is not byte-for-byte .NET's combiner, but
        // it is reached only after the reducer cannot otherwise distinguish two
        // frameworks.
        let mut hash = 0xcbf29ce484222325u64;
        for part in [
            framework.framework.as_str(),
            &framework.version_string(),
            framework.profile.as_deref().unwrap_or(""),
            framework
                .platform
                .as_ref()
                .map(|(name, _)| name.as_str())
                .unwrap_or(""),
            &framework.platform_version_string(),
        ] {
            for byte in part.to_ascii_uppercase().bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x100000001b3);
            }
            hash ^= 0xff;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn framework_display_for_sort(framework: &NuGetFramework) -> String {
        framework.short_folder_name().unwrap_or_else(|| {
            format!(
                "{},{},{}",
                framework.framework(),
                framework.version_string(),
                framework.profile().unwrap_or("")
            )
        })
    }

    fn is_package_based(framework: &NuGetFramework) -> bool {
        same_id(framework.framework(), id::DNXCORE)
            || same_id(framework.framework(), id::DOTNET)
            || same_id(framework.framework(), id::NET_STANDARD)
            || same_id(framework.framework(), id::NET_CORE_APP)
            || same_id(framework.framework(), id::UAP)
            || same_id(framework.framework(), id::TIZEN)
            || is_netcore50_and_up(framework)
    }

    fn is_netcore50_and_up(framework: &NuGetFramework) -> bool {
        same_id(framework.framework(), id::NETCORE) && framework.version[0] >= 5
    }

    fn is_net5_era(framework: &NuGetFramework) -> bool {
        same_id(framework.framework(), id::NET_CORE_APP) && framework.version[0] >= 5
    }

    fn equivalent_frameworks(a: &NuGetFramework, b: &NuGetFramework) -> bool {
        full_eq(a, b)
            || equivalent_pairs(a)
                .iter()
                .any(|equivalent| full_eq(equivalent, b))
    }

    fn equivalent_pairs(framework: &NuGetFramework) -> Vec<NuGetFramework> {
        let mut out = Vec::new();
        let mut add = |left: &str, right: &str| {
            let left =
                NuGetFramework::parse_folder(left).expect("equivalent framework literal parses");
            let right =
                NuGetFramework::parse_folder(right).expect("equivalent framework literal parses");
            if full_eq(framework, &left) {
                out.push(right.clone());
            }
            if full_eq(framework, &right) {
                out.push(left);
            }
        };
        add("uap", "uap10.0");
        add("win", "win8");
        add("win8", "netcore45");
        add("netcore45", "winrt45");
        add("netcore", "netcore45");
        add("winrt", "winrt45");
        add("win81", "netcore451");
        add("wp", "wp7");
        add("wp7", "sl3-wp");
        add("wp71", "sl4-wp71");
        add("wp8", "sl8-wp");
        add("wp81", "sl81-wp");
        add("wpa", "wpa81");
        add("tizen", "tizen3");
        add("dnx", "dnx45");
        add("dnxcore", "dnxcore50");
        add("dotnet", "dotnet50");
        add("aspnet", "aspnet50");
        add("aspnetcore", "aspnetcore50");
        add("dnx45", "aspnet50");
        add("dnxcore50", "aspnetcore50");
        out
    }

    fn full_eq(a: &NuGetFramework, b: &NuGetFramework) -> bool {
        !a.is_unsupported() && !b.is_unsupported() && a == b
    }

    fn same_name(a: &NuGetFramework, b: &NuGetFramework) -> bool {
        same_id(a.framework(), b.framework())
    }

    fn same_id(a: &str, b: &str) -> bool {
        a.eq_ignore_ascii_case(b)
    }

    fn is_zero(version: [u32; 4]) -> bool {
        version == [0; 4]
    }

    fn platform_version(framework: &NuGetFramework) -> [u32; 4] {
        framework
            .platform
            .as_ref()
            .map(|(_, version)| *version)
            .unwrap_or([0; 4])
    }

    fn ci_cmp(a: &str, b: &str) -> Ordering {
        a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase())
    }

    fn opt_ci_cmp(a: Option<&str>, b: Option<&str>) -> Ordering {
        match (a, b) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(a), Some(b)) => ci_cmp(a, b),
        }
    }
}
