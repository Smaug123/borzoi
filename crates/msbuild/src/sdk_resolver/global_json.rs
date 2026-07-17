//! `global.json` discovery and parsing.
//!
//! .NET's SDK resolver searches upward from the working directory for
//! the nearest `global.json` and uses its `sdk` block to constrain
//! which installed SDK is selected. The same file's `msbuild-sdks`
//! map separately pins specific Project SDK package versions for
//! `<Import Sdk="…"/>` resolution. This module covers all of:
//!
//! - [`find_global_json`] walks upward from a starting directory.
//! - [`parse_global_json`] parses the (JSONC-flavoured) text into a
//!   schema-shaped [`GlobalJson`] document carrying both the `sdk`
//!   block (as [`GlobalJsonSettings`]) and the `msbuild-sdks` map.
//! - [`GlobalJsonSettings::into_spec`] applies the per-host defaults and
//!   produces a [`VersionSpec`] the SDK resolver can apply.
//!
//! `global.json` is officially JSONC: standard JSON plus `//` line
//! comments and `/* … */` block comments. Real-world files routinely
//! carry comments. The parser here strips comments first, then walks
//! a minimal AST big enough to extract the keys we care about:
//! `sdk.version`, `sdk.rollForward`, `sdk.allowPrerelease`, and the
//! `msbuild-sdks` object of `Name → Version` pins. Other fields
//! (`tools`, `projects`, …) are tolerated and ignored.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::SdkVersion;
use super::version_spec::{RollForward, VersionSpec};

/// Parsed view of a `global.json` document as a whole. `sdk` carries
/// the `sdk` block (or `None` if absent / set to `null`).
/// `msbuild_sdks` carries the top-level `msbuild-sdks` map of
/// `Name → Version` pins — see [`parse_global_json`] for the schema.
/// An empty map represents either an absent or empty `msbuild-sdks`
/// key; the parser does not distinguish those (`msbuild-sdks: null` is
/// also folded into "empty").
///
/// Ordering: `msbuild_sdks` is a `BTreeMap` so iteration is
/// deterministic by SDK name. The SDK resolver only ever does point
/// lookups, but the deterministic order makes round-trip serialisation
/// and test failure output stable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalJson {
    pub sdk: Option<GlobalJsonSettings>,
    pub msbuild_sdks: BTreeMap<String, SdkVersion>,
    /// Whether the *workload* reader's view of this document engages
    /// workload-set selection. MSBuild's workload manifest provider
    /// re-reads `global.json` through its own parser
    /// (`SdkDirectoryWorkloadManifestProvider.GlobalJsonReader`, which
    /// matches keys case-insensitively — unlike the host's `sdk` block
    /// reader mirrored by the rest of this module): an
    /// `sdk.workloadVersion` pin selects a workload set or fails the
    /// evaluation when that set is not installed. True also for shapes
    /// that reader rejects outright (a non-object `sdk` value, or a
    /// non-string `workloadVersion`/`workloads-update-mode`), since a
    /// real evaluation errors there. Consumers treat `true` as "outside
    /// the workload locator resolution envelope" and degrade.
    pub pins_workload_set: bool,
}

/// One entry of the `sdk.paths` array from a `global.json`.
///
/// `paths` was added in .NET 10 to point the host at extra SDK install
/// roots (typically a repo-local build under `artifacts/`). The .NET
/// host treats the literal token `"$host$"` as a request to also
/// consult the regular host install at that ordinal position; any
/// other entry is a path string (relative to the `global.json`
/// directory, or absolute).
///
/// The parser keeps the [`Relative`] payload as the raw JSON string.
/// Joining against the `global.json` directory is the consumer's job
/// — this crate doesn't know where the document lives on disk, and
/// resolving here would lose information the LSP layer needs (e.g.
/// which entry was `$host$`-substituted, for diagnostics).
///
/// [`Relative`]: SdkPathEntry::Relative
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdkPathEntry {
    /// The literal `"$host$"` token, exact case. Resolves at the
    /// consumer's discretion to the host SDK install location.
    Host,
    /// Any other non-empty string. Treated opaquely at the parser
    /// layer; the consumer joins it against the `global.json`
    /// file's directory to produce a `PathBuf`.
    Relative(String),
}

/// Parsed view of the `sdk` block of a `global.json` file. All four
/// fields are optional in the JSON; absent fields stay `None` here and
/// the per-host default fills them in at [`Self::into_spec`].
///
/// We deliberately do *not* fold the host default into this struct.
/// The parser is policy-free; the shell that picked the host
/// (CLI vs VS) provides the default at conversion time.
///
/// `paths` is the .NET 10 multi-root extension — see [`SdkPathEntry`].
/// `None` means the field was absent or `null` (use the host install
/// only). `Some(vec)` carries the explicit list, including the empty
/// list as an opt-out (no usable SDK roots).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GlobalJsonSettings {
    pub version: Option<SdkVersion>,
    pub roll_forward: Option<RollForward>,
    pub allow_prerelease: Option<bool>,
    pub paths: Option<Vec<SdkPathEntry>>,
}

impl GlobalJsonSettings {
    /// Apply per-host defaults and produce a [`VersionSpec`].
    ///
    /// `host_default_allow_prerelease` is the host policy fallback (the
    /// CLI host passes `true`, the VS host passes `false`). The JSON's
    /// own `allowPrerelease` overrides it; a prerelease pin further
    /// overrides both inside [`VersionSpec::with_version`].
    ///
    /// When `version` is `Some` and `rollForward` was absent in the
    /// JSON, .NET defaults to `Patch` — exact pin preferred, rolling
    /// forward to a higher patch in the same feature band only when
    /// the exact pin isn't installed. See
    /// <https://learn.microsoft.com/dotnet/core/tools/global-json>.
    /// Note: this is *not* `LatestPatch`, which would happily prefer
    /// `9.0.105` over an installed `9.0.100` pin.
    ///
    /// When `version` is `None`, `rollForward` is meaningless to .NET
    /// (it picks the latest installed); we honour that by returning a
    /// pinless [`VersionSpec::any_version`] regardless of any
    /// `rollForward` value the in-memory struct happens to carry.
    /// Note that [`parse_global_json`] rejects JSON shapes where this
    /// arises (rollForward without version) — the
    /// `roll_forward = Some(_), version = None` shape is only
    /// reachable by hand-constructing the struct, in which case we
    /// remain forgiving rather than panic.
    pub fn into_spec(self, host_default_allow_prerelease: bool) -> VersionSpec {
        let allow_prerelease = self
            .allow_prerelease
            .unwrap_or(host_default_allow_prerelease);
        match self.version {
            Some(v) => VersionSpec::with_version(
                v,
                self.roll_forward.unwrap_or(RollForward::Patch),
                allow_prerelease,
            ),
            None => VersionSpec::any_version(allow_prerelease),
        }
    }
}

/// Reasons [`parse_global_json`] can refuse to produce settings. The
/// caller is expected to surface this to the user (an
/// `SdkVersionNotSatisfied`-adjacent diagnostic would be misleading —
/// the failure is in the *config*, not in version selection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalJsonError {
    /// JSON syntax error: unterminated string, missing comma, etc.
    /// `position` is a byte offset into the original text (before
    /// comment stripping) only as a best-effort hint; exact position
    /// tracking through the comment stripper is not worth the
    /// complexity for the LSP's purposes.
    Syntax { message: String, position: usize },
    /// `sdk.version` was present but didn't parse as a strict
    /// three-component `major.minor.featureBand[-prerelease]` value.
    /// .NET's `global.json` consumer only accepts that exact shape —
    /// `8.0`, `8.0.0.0`, `8.0.99`, etc. are all rejected even though
    /// `SdkVersion::parse` (which is looser, since it doubles as the
    /// directory-name reader) would accept them.
    InvalidVersion(String),
    /// `sdk.rollForward` was present but didn't name a policy. .NET's
    /// nine documented values are matched case-insensitively
    /// (`latestPatch`, `latestMajor`, …); anything else lands here.
    InvalidRollForward(String),
    /// `sdk.rollForward` was specified without `sdk.version`. The eight
    /// policies that constrain selection around a requested version
    /// (`disable`, `patch`, `feature`, `minor`, `major`, `latestPatch`,
    /// `latestFeature`, `latestMinor`) all require a version to anchor
    /// the comparison. .NET rejects this combination; the only
    /// rollForward policy that is meaningful version-less is
    /// `latestMajor` (it means "pick the freshest installed", which is
    /// the no-version default anyway).
    RollForwardRequiresVersion(RollForward),
    /// A typed field had the wrong JSON type — e.g. `sdk.version` was a
    /// number instead of a string, `sdk.allowPrerelease` was a string
    /// instead of a bool, or `sdk` itself wasn't an object.
    InvalidType {
        field: &'static str,
        expected: &'static str,
    },
    /// An entry of the `msbuild-sdks` map was the wrong JSON type —
    /// values must be strings naming an SDK version. The `name` is the
    /// map key, carried so the diagnostic can point at the offending
    /// entry rather than "somewhere in msbuild-sdks".
    InvalidMsBuildSdksEntryType {
        name: String,
        expected: &'static str,
    },
    /// An entry of the `msbuild-sdks` map carried a version string
    /// that `SdkVersion::parse` refused. Project SDK package versions
    /// follow NuGet SemVer (e.g. `3.7.134`, `11.0.0-beta.25569.5`) and
    /// must be parseable for the resolver to use the entry as a pin.
    InvalidMsBuildSdkVersion { name: String, value: String },
}

impl std::fmt::Display for GlobalJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GlobalJsonError::Syntax { message, position } => {
                write!(f, "{message} (byte position {position})")
            }
            GlobalJsonError::InvalidVersion(v) => {
                write!(f, "sdk.version is not a valid version: {v:?}")
            }
            GlobalJsonError::InvalidRollForward(v) => {
                write!(f, "sdk.rollForward is not a known policy: {v:?}")
            }
            GlobalJsonError::RollForwardRequiresVersion(rf) => {
                write!(f, "sdk.rollForward {rf:?} requires sdk.version to be set")
            }
            GlobalJsonError::InvalidType { field, expected } => {
                write!(f, "{field} has the wrong type; expected {expected}")
            }
            GlobalJsonError::InvalidMsBuildSdksEntryType { name, expected } => {
                write!(
                    f,
                    "msbuild-sdks[{name:?}] has the wrong type; expected {expected}"
                )
            }
            GlobalJsonError::InvalidMsBuildSdkVersion { name, value } => {
                write!(
                    f,
                    "msbuild-sdks[{name:?}] is not a valid version: {value:?}"
                )
            }
        }
    }
}

impl std::error::Error for GlobalJsonError {}

/// Parse a `global.json` text into a [`GlobalJson`] document.
///
/// Both top-level keys we care about are optional: a file with neither
/// `sdk` nor `msbuild-sdks` parses to [`GlobalJson::default`] (no
/// error). A file with only `msbuild-sdks` populates that field and
/// leaves `sdk` as `None`; a file with only `sdk` does the inverse.
/// An empty `sdk` block (`"sdk": {}`) yields
/// `sdk = Some(GlobalJsonSettings::default())` — the key was present,
/// even if its body had nothing for us. `sdk: null` is folded into
/// "absent" to match .NET's tolerance for serialiser-emitted nulls.
///
/// Strips C-style line/block comments before parsing, matching the
/// JSONC behaviour of .NET's `System.Text.Json` consumer. UTF-8 BOM
/// at the start of input is tolerated.
pub fn parse_global_json(text: &str) -> Result<GlobalJson, GlobalJsonError> {
    let stripped = strip_jsonc_comments(text)?;
    let mut parser = Parser::new(&stripped);
    parser.skip_ws()?;
    let value = parser.parse_value()?;
    parser.skip_ws()?;
    if parser.pos < parser.src.len() {
        return Err(GlobalJsonError::Syntax {
            message: format!(
                "unexpected trailing input after JSON value: byte {:#x}",
                parser.src[parser.pos]
            ),
            position: parser.pos,
        });
    }
    let JsonValue::Object(top) = value else {
        return Err(GlobalJsonError::InvalidType {
            field: "global.json",
            expected: "object",
        });
    };
    let sdk = parse_sdk_block(&top)?;
    let msbuild_sdks = parse_msbuild_sdks(&top)?;
    let pins_workload_set = detect_workload_set_pin(&top);
    Ok(GlobalJson {
        sdk,
        msbuild_sdks,
        pins_workload_set,
    })
}

/// Transcription of the *effect* of
/// `SdkDirectoryWorkloadManifestProvider.GlobalJsonReader.ParseGlobalJson`
/// (dotnet/sdk, fetched 2026-07-10): does the workload manifest
/// provider's own read of this document influence workload manifest
/// selection?
///
/// That reader matches property names ordinal-case-insensitively (so
/// `"SDK"`/`"WorkloadVersion"` count, unlike the host's case-sensitive
/// `sdk` block), and walks *every* top-level `sdk` occurrence:
///
/// - a non-object `sdk` value (including `null`) throws
///   `JsonFormatException` inside the provider — the real evaluation
///   fails, so the document "pins" in the sense that we cannot resolve
///   as if it weren't there;
/// - `workloadVersion` with a string value selects that workload set
///   (or fails the evaluation when it isn't installed) — pin;
/// - `workloadVersion` with a non-string value throws — pin;
/// - `workloads-update-mode` with a non-string value throws — pin;
/// - `workloads-update-mode` with a string value only toggles the
///   preference between workload sets and loose manifests, which can
///   change the outcome only when a `workloadsets` directory exists on
///   disk — a layout the workload locator resolution already degrades
///   on — so it is *not* a pin here.
fn detect_workload_set_pin(top: &[(String, JsonValue)]) -> bool {
    top.iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("sdk"))
        .any(|(_, value)| match value {
            JsonValue::Object(fields) => fields.iter().any(|(key, value)| {
                key.eq_ignore_ascii_case("workloadVersion")
                    || (key.eq_ignore_ascii_case("workloads-update-mode")
                        && !matches!(value, JsonValue::String(_)))
            }),
            _ => true,
        })
}

/// Pull the `sdk` block out of the top-level object. `None` covers
/// both "key absent" and `"sdk": null` (matching .NET's tolerance for
/// serialiser-emitted nulls); `Some(GlobalJsonSettings::default())`
/// covers an explicit empty `"sdk": {}` block. Errors propagate up.
fn parse_sdk_block(
    top: &[(String, JsonValue)],
) -> Result<Option<GlobalJsonSettings>, GlobalJsonError> {
    let sdk = match find_field(top, "sdk") {
        None | Some(JsonValue::Null) => return Ok(None),
        Some(JsonValue::Object(items)) => items,
        Some(_) => {
            return Err(GlobalJsonError::InvalidType {
                field: "sdk",
                expected: "object",
            });
        }
    };

    // .NET's `global.json` consumer treats an explicit JSON `null` for
    // any of the three SDK fields the same as the field being absent.
    // Serializers that emit nullable optional fields shouldn't poison
    // the rest of the file, so we mirror that.
    let mut out = GlobalJsonSettings::default();
    match find_field(sdk, "version") {
        None | Some(JsonValue::Null) => {}
        Some(JsonValue::String(s)) => {
            // .NET's `global.json` consumer accepts only the strict
            // three-component `major.minor.featureBand[-prerelease]`
            // shape — not the looser `SdkVersion::parse` directory-name
            // form that tolerates 1+ components. Without this check,
            // `8.0.100.0` would normalise to `8.0.100` and silently
            // resolve a different SDK from what the user wrote.
            let numeric_part = s.split_once('-').map(|(head, _)| head).unwrap_or(s);
            if numeric_part.split('.').count() != 3 {
                return Err(GlobalJsonError::InvalidVersion(s.clone()));
            }
            let parsed =
                SdkVersion::parse(s).ok_or_else(|| GlobalJsonError::InvalidVersion(s.clone()))?;
            // .NET SDK feature bands start at `x.y.100`. Versions like
            // `8.0`, `8.0.0`, or `8.0.99` are syntactically parseable but
            // can't refer to any real installed SDK, so the .NET host
            // rejects them at `global.json` parse time and we mirror that
            // — surfacing the error here is much friendlier than a later
            // `SdkVersionNotSatisfied` against an unreachable spec.
            if parsed.feature_band() == 0 {
                return Err(GlobalJsonError::InvalidVersion(s.clone()));
            }
            out.version = Some(parsed);
        }
        Some(_) => {
            return Err(GlobalJsonError::InvalidType {
                field: "sdk.version",
                expected: "string",
            });
        }
    }
    match find_field(sdk, "rollForward") {
        None | Some(JsonValue::Null) => {}
        Some(JsonValue::String(s)) => {
            out.roll_forward = Some(
                parse_roll_forward(s)
                    .ok_or_else(|| GlobalJsonError::InvalidRollForward(s.clone()))?,
            );
        }
        Some(_) => {
            return Err(GlobalJsonError::InvalidType {
                field: "sdk.rollForward",
                expected: "string",
            });
        }
    }
    match find_field(sdk, "allowPrerelease") {
        None | Some(JsonValue::Null) => {}
        Some(JsonValue::Bool(b)) => {
            out.allow_prerelease = Some(*b);
        }
        Some(_) => {
            return Err(GlobalJsonError::InvalidType {
                field: "sdk.allowPrerelease",
                expected: "boolean",
            });
        }
    }
    out.paths = parse_sdk_paths(sdk)?;
    // Cross-field validation: every roll-forward policy other than
    // `latestMajor` is meaningless without a `version` to anchor the
    // comparison. .NET treats this shape as invalid `global.json`;
    // catch it here rather than silently swallowing the policy at
    // `into_spec` time.
    if let (None, Some(rf)) = (&out.version, out.roll_forward)
        && rf != RollForward::LatestMajor
    {
        return Err(GlobalJsonError::RollForwardRequiresVersion(rf));
    }
    Ok(Some(out))
}

/// Pull the `sdk.paths` array out of the `sdk` block.
///
/// Returns `None` for absent / `null` (matches .NET's "use the host
/// install" default). Returns `Some(vec)` when the field is present
/// as an array — including the empty array, which is .NET's
/// documented opt-out shape (the host treats it as "no usable SDK
/// roots"). Non-array values are rejected via the existing
/// [`GlobalJsonError::InvalidType`] shape.
///
/// String entries are classified per-entry: the exact-case token
/// `"$host$"` becomes [`SdkPathEntry::Host`]; any other string —
/// *including the empty string* — becomes [`SdkPathEntry::Relative`].
/// The .NET host treats `$host$` case-sensitively (no `$Host$`
/// alias), so we match.
///
/// Leniency on individual entries mirrors the .NET host
/// (`fxr/sdk_info.cpp`): non-string entries are *silently skipped*
/// rather than failing the whole file, because the host emits a
/// trace warning and continues. Rejecting the file outright here
/// would lose the user's `sdk.version` and `allowPrerelease` pins
/// for workspaces the host would still resolve, which is worse than
/// dropping a malformed entry. Empty-string entries are kept as
/// `Relative("")`: the host pushes them as-is, and joining against
/// the `global.json` directory then yields that directory — odd,
/// but the host's behaviour, so we match it bit-for-bit.
fn parse_sdk_paths(
    sdk: &[(String, JsonValue)],
) -> Result<Option<Vec<SdkPathEntry>>, GlobalJsonError> {
    let entries = match find_field(sdk, "paths") {
        None | Some(JsonValue::Null) => return Ok(None),
        Some(JsonValue::Array(items)) => items,
        Some(_) => {
            return Err(GlobalJsonError::InvalidType {
                field: "sdk.paths",
                expected: "array",
            });
        }
    };
    let mut out = Vec::with_capacity(entries.len());
    for value in entries.iter() {
        let JsonValue::String(s) = value else {
            // Match the .NET host: skip non-string entries rather
            // than rejecting the whole file.
            continue;
        };
        out.push(if s == "$host$" {
            SdkPathEntry::Host
        } else {
            SdkPathEntry::Relative(s.clone())
        });
    }
    Ok(Some(out))
}

/// Pull the `msbuild-sdks` map out of the top-level object. Returns
/// an empty map when the key is absent or `null` (matching .NET's
/// tolerance for serialiser-emitted nulls). Non-object values are
/// rejected; non-string entry values are rejected; unparseable
/// version strings are rejected. Duplicate keys keep the first
/// occurrence (consistent with [`find_field`]).
///
/// Strictness on entry values is deliberate. The resolver consumes
/// these as exact-version pins (`RollForward::Disable`), so a
/// malformed entry would silently fail to match anything restored —
/// hard to debug. Surfacing the parse error here surfaces a typo at
/// the same point as `sdk.version` mistakes.
fn parse_msbuild_sdks(
    top: &[(String, JsonValue)],
) -> Result<BTreeMap<String, SdkVersion>, GlobalJsonError> {
    let entries = match find_field(top, "msbuild-sdks") {
        None | Some(JsonValue::Null) => return Ok(BTreeMap::new()),
        Some(JsonValue::Object(items)) => items,
        Some(_) => {
            return Err(GlobalJsonError::InvalidType {
                field: "msbuild-sdks",
                expected: "object",
            });
        }
    };
    let mut out: BTreeMap<String, SdkVersion> = BTreeMap::new();
    for (name, value) in entries {
        // Duplicate keys: keep the first occurrence so behaviour
        // matches a single point-lookup via `find_field`. JSON
        // duplicate-key semantics are technically undefined; .NET's
        // `System.Text.Json` keeps the last, but the difference only
        // shows up on malformed input. Document the choice and move on.
        if out.contains_key(name) {
            continue;
        }
        let version_str = match value {
            JsonValue::String(s) => s,
            JsonValue::Null => continue,
            _ => {
                return Err(GlobalJsonError::InvalidMsBuildSdksEntryType {
                    name: name.clone(),
                    expected: "string",
                });
            }
        };
        let parsed = SdkVersion::parse(version_str).ok_or_else(|| {
            GlobalJsonError::InvalidMsBuildSdkVersion {
                name: name.clone(),
                value: version_str.clone(),
            }
        })?;
        out.insert(name.clone(), parsed);
    }
    Ok(out)
}

/// Search upward from `start_dir` for a `global.json` file. Returns
/// the on-disk path of the nearest one (closest ancestor wins), or
/// `None` if no ancestor contains the file.
///
/// `start_dir` is treated as the first candidate (matching .NET's
/// behaviour where a `global.json` next to the project applies). The
/// path is not canonicalised; callers that need stable identity should
/// canonicalise the input first.
///
/// Symlinks in the chain are followed by the OS — we don't probe
/// `file_type()` because we never need to distinguish symlinked vs
/// real files for this lookup.
pub fn find_global_json(start_dir: &Path) -> Option<PathBuf> {
    let mut cursor = Some(start_dir);
    while let Some(dir) = cursor {
        let candidate = dir.join("global.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        cursor = dir.parent();
    }
    None
}

/// Map the documented `rollForward` values to [`RollForward`].
/// `global.json` schema treats these case-insensitively in the .NET
/// host, so we lowercase before matching. Anything else returns
/// `None` so the caller can surface
/// [`GlobalJsonError::InvalidRollForward`].
fn parse_roll_forward(value: &str) -> Option<RollForward> {
    match value.to_ascii_lowercase().as_str() {
        "disable" => Some(RollForward::Disable),
        "patch" => Some(RollForward::Patch),
        "feature" => Some(RollForward::Feature),
        "minor" => Some(RollForward::Minor),
        "major" => Some(RollForward::Major),
        "latestpatch" => Some(RollForward::LatestPatch),
        "latestfeature" => Some(RollForward::LatestFeature),
        "latestminor" => Some(RollForward::LatestMinor),
        "latestmajor" => Some(RollForward::LatestMajor),
        _ => None,
    }
}

// ===================================================================
// Minimal JSONC parser
// ===================================================================
//
// `global.json` is officially JSONC (JSON + // and /* */ comments) and
// we only need to find a string, bool, or sub-object under
// well-known keys. A purpose-built parser keeps the msbuild crate's
// runtime dependencies at just `roxmltree` — per the crate's
// "self-contained" charter — and lets us tailor error variants to the
// schema (`InvalidVersion`, `InvalidRollForward`) without a generic
// `serde` layer between user input and the schema.

/// Tag-only AST: arrays and numbers carry no payload because the
/// schema never reaches into them. If we ever need to read numbers
/// out of `global.json` we'd thread the value through here, but today
/// every typed field is a string, bool, or nested object.
#[derive(Debug, Clone, PartialEq)]
enum JsonValue {
    String(String),
    Bool(bool),
    Object(Vec<(String, JsonValue)>),
    Array(Vec<JsonValue>),
    Number,
    Null,
}

fn find_field<'a>(obj: &'a [(String, JsonValue)], key: &str) -> Option<&'a JsonValue> {
    obj.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

struct Parser<'s> {
    src: &'s [u8],
    pos: usize,
}

impl<'s> Parser<'s> {
    fn new(src: &'s str) -> Self {
        // Strip a UTF-8 BOM if present so it doesn't trip the first
        // `parse_value`. `global.json` files written by Windows
        // tooling sometimes carry one.
        let bytes = src.as_bytes();
        let pos = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            3
        } else {
            0
        };
        Self { src: bytes, pos }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn syntax<T>(&self, message: impl Into<String>) -> Result<T, GlobalJsonError> {
        Err(GlobalJsonError::Syntax {
            message: message.into(),
            position: self.pos,
        })
    }

    fn skip_ws(&mut self) -> Result<(), GlobalJsonError> {
        while let Some(b) = self.peek() {
            if b.is_ascii_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
        Ok(())
    }

    fn expect(&mut self, b: u8) -> Result<(), GlobalJsonError> {
        match self.peek() {
            Some(c) if c == b => {
                self.pos += 1;
                Ok(())
            }
            Some(c) => self.syntax(format!("expected {:?} but saw {:?}", b as char, c as char)),
            None => self.syntax(format!("expected {:?} but reached end of input", b as char)),
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, GlobalJsonError> {
        self.skip_ws()?;
        match self.peek() {
            Some(b'{') => self.parse_object().map(JsonValue::Object),
            Some(b'[') => self.parse_array().map(JsonValue::Array),
            Some(b'"') => self.parse_string().map(JsonValue::String),
            Some(b't') | Some(b'f') => self.parse_bool().map(JsonValue::Bool),
            Some(b'n') => self.parse_null().map(|_| JsonValue::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => {
                self.parse_number()?;
                Ok(JsonValue::Number)
            }
            Some(c) => self.syntax(format!(
                "unexpected character {:?} in JSON value",
                c as char
            )),
            None => self.syntax("unexpected end of input while reading a JSON value"),
        }
    }

    fn parse_object(&mut self) -> Result<Vec<(String, JsonValue)>, GlobalJsonError> {
        self.expect(b'{')?;
        let mut out = Vec::new();
        self.skip_ws()?;
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(out);
        }
        loop {
            self.skip_ws()?;
            let key = self.parse_string()?;
            self.skip_ws()?;
            self.expect(b':')?;
            let value = self.parse_value()?;
            out.push((key, value));
            self.skip_ws()?;
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(c) => {
                    return self.syntax(format!(
                        "expected ',' or '}}' in object, saw {:?}",
                        c as char
                    ));
                }
                None => return self.syntax("unterminated object"),
            }
        }
    }

    fn parse_array(&mut self) -> Result<Vec<JsonValue>, GlobalJsonError> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        self.skip_ws()?;
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(out);
        }
        loop {
            let value = self.parse_value()?;
            out.push(value);
            self.skip_ws()?;
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(c) => {
                    return self
                        .syntax(format!("expected ',' or ']' in array, saw {:?}", c as char));
                }
                None => return self.syntax("unterminated array"),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, GlobalJsonError> {
        self.expect(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            match self.bump() {
                None => return self.syntax("unterminated string literal"),
                Some(b'"') => {
                    return String::from_utf8(out).map_err(|_| GlobalJsonError::Syntax {
                        message: "string literal is not valid UTF-8".to_string(),
                        position: self.pos,
                    });
                }
                Some(b'\\') => match self.bump() {
                    None => return self.syntax("escape at end of input"),
                    Some(b'"') => out.push(b'"'),
                    Some(b'\\') => out.push(b'\\'),
                    Some(b'/') => out.push(b'/'),
                    Some(b'b') => out.push(0x08),
                    Some(b'f') => out.push(0x0C),
                    Some(b'n') => out.push(b'\n'),
                    Some(b'r') => out.push(b'\r'),
                    Some(b't') => out.push(b'\t'),
                    Some(b'u') => {
                        let cp = self.read_unicode_escape()?;
                        // Surrogate handling: a high surrogate must be
                        // followed by a `\uXXXX` low surrogate. Anything
                        // else is malformed input.
                        let scalar = if (0xD800..=0xDBFF).contains(&cp) {
                            self.expect(b'\\')?;
                            self.expect(b'u')?;
                            let low = self.read_unicode_escape()?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return self.syntax("expected low surrogate after high surrogate");
                            }
                            0x10000 + (((cp - 0xD800) << 10) | (low - 0xDC00))
                        } else if (0xDC00..=0xDFFF).contains(&cp) {
                            return self.syntax("unexpected low surrogate without preceding high");
                        } else {
                            cp
                        };
                        let ch = char::from_u32(scalar).ok_or_else(|| GlobalJsonError::Syntax {
                            message: format!("invalid Unicode scalar value {scalar:#x}"),
                            position: self.pos,
                        })?;
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    Some(c) => return self.syntax(format!("unknown escape \\{:?}", c as char)),
                },
                Some(b) if b < 0x20 => {
                    // Unescaped control bytes are illegal in JSON strings.
                    return self.syntax(format!("unescaped control byte {b:#x} in string"));
                }
                Some(b) => out.push(b),
            }
        }
    }

    fn read_unicode_escape(&mut self) -> Result<u32, GlobalJsonError> {
        let mut acc: u32 = 0;
        for _ in 0..4 {
            let b = self.bump().ok_or_else(|| GlobalJsonError::Syntax {
                message: "unterminated \\u escape".to_string(),
                position: self.pos,
            })?;
            let digit = match b {
                b'0'..=b'9' => (b - b'0') as u32,
                b'a'..=b'f' => 10 + (b - b'a') as u32,
                b'A'..=b'F' => 10 + (b - b'A') as u32,
                _ => {
                    return self.syntax(format!("non-hex digit {:?} in \\u escape", b as char));
                }
            };
            acc = (acc << 4) | digit;
        }
        Ok(acc)
    }

    fn parse_bool(&mut self) -> Result<bool, GlobalJsonError> {
        if self.src[self.pos..].starts_with(b"true") {
            self.pos += 4;
            Ok(true)
        } else if self.src[self.pos..].starts_with(b"false") {
            self.pos += 5;
            Ok(false)
        } else {
            self.syntax("expected `true` or `false`")
        }
    }

    fn parse_null(&mut self) -> Result<(), GlobalJsonError> {
        if self.src[self.pos..].starts_with(b"null") {
            self.pos += 4;
            Ok(())
        } else {
            self.syntax("expected `null`")
        }
    }

    /// Validate (but don't decode) a JSON number. The schema doesn't
    /// reach into numbers; we only need to skip past one.
    fn parse_number(&mut self) -> Result<(), GlobalJsonError> {
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        // Integer part: 0 alone, or [1-9][0-9]*.
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
            }
            Some(c) if c.is_ascii_digit() => {
                while let Some(c) = self.peek() {
                    if !c.is_ascii_digit() {
                        break;
                    }
                    self.pos += 1;
                }
            }
            _ => return self.syntax("expected digit in number"),
        }
        // Optional fraction.
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let start = self.pos;
            while let Some(c) = self.peek() {
                if !c.is_ascii_digit() {
                    break;
                }
                self.pos += 1;
            }
            if self.pos == start {
                return self.syntax("expected digit after '.' in number");
            }
        }
        // Optional exponent.
        if let Some(b'e' | b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            let start = self.pos;
            while let Some(c) = self.peek() {
                if !c.is_ascii_digit() {
                    break;
                }
                self.pos += 1;
            }
            if self.pos == start {
                return self.syntax("expected digit in number exponent");
            }
        }
        Ok(())
    }
}

/// Strip C-style line (`// …`) and block (`/* … */`) comments from
/// `input`. Comments inside string literals are preserved. The output
/// is the same length-or-shorter and remains valid UTF-8 (we only
/// elide ASCII byte ranges).
///
/// An unterminated `/*` is a syntax error — without this, a trailing
/// `/* …` after an otherwise complete JSON value would be silently
/// swallowed and the broken file would be accepted as valid, hiding
/// an obvious config typo. .NET's JSONC consumer rejects this too.
fn strip_jsonc_comments(input: &str) -> Result<String, GlobalJsonError> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            // Copy string literal verbatim. Track backslash escapes so
            // a `\"` doesn't end the string early.
            out.push(b'"');
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                out.push(c);
                i += 1;
                if c == b'\\' {
                    if i < bytes.len() {
                        out.push(bytes[i]);
                        i += 1;
                    }
                } else if c == b'"' {
                    break;
                }
            }
        } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            // Line comment: skip to (but keep) the newline. Preserving
            // line breaks keeps the position-hint useful.
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            // Block comment: skip to `*/` and consume it. Substitute a
            // single space so `tr/*x*/ue` doesn't collapse into `true`
            // and a malformed file silently parse as valid — the
            // comment must still act as a token delimiter.
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            } else {
                return Err(GlobalJsonError::Syntax {
                    message: "unterminated block comment".to_string(),
                    position: start,
                });
            }
            out.push(b' ');
        } else {
            out.push(b);
            i += 1;
        }
    }
    // Safe: we only ever copy whole input bytes or omit ASCII ranges.
    Ok(String::from_utf8(out).expect("strip preserves UTF-8 validity"))
}

#[cfg(test)]
mod tests;
