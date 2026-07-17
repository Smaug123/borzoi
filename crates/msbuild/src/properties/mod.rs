//! `$(Name)` property substitution.
//!
//! Phase 2 evaluates `<PropertyGroup>` children in document order, with a
//! single forward pass. Each property's value has `$(...)` references
//! expanded against the map built so far; the result is then assigned. A
//! reference to a name that hasn't been defined yet emits
//! [`Issue::Undefined`] and is substituted as empty (matching MSBuild's
//! behaviour, which the [`is_partial`](super::ParsedProject::is_partial)
//! flag lets callers detect). The evaluator supports a tiny allowlist of
//! property functions needed by SDK target-framework inference; anything else
//! more elaborate than a bare identifier inside `$(...)` — property functions,
//! item-vector transforms, registry refs — emits [`Issue::Unsupported`] and is
//! left literal in the output.
//!
//! Property names that map to *reserved* MSBuild properties (seeded by
//! [`well_known`]) or to caller-supplied *global* properties cannot be
//! overridden by the project file; project-side writes to those names are
//! silently ignored, matching MSBuild semantics.
//!
//! The evaluator does not iterate to a fixed point. Forward references —
//! `$(Foo)` appearing before `<Foo>...</Foo>` — are diagnosed; real
//! MSBuild does a separate pre-pass that catches some of these, but phase
//! 2 deliberately stays single-pass for predictability.

use std::collections::HashMap;
use std::path::Path;

pub(crate) mod escaping;
mod expr;
// Stage P0 of `docs/msbuild-unix-path-fixup-plan.md`: the eligibility half of
// MSBuild's unix path fixup, wired to nothing yet (P1/P2 bracket the worlds
// through conditions and the property table). The differential in
// `tests/path_fixup_diff.rs` reaches it via `test_support`.
#[allow(dead_code)]
pub(crate) mod path_fixup;

use escaping::Escaped;

pub(crate) use expr::is_referenceable_name;

const MAX_TARGET_FRAMEWORK_VERSION_PARTS: usize = 4;

/// A single issue encountered while substituting an attribute or property
/// value. Callers attach a span to convert these into
/// [`Diagnostic`](super::Diagnostic)s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    /// `$(Name)` where `Name` is not in the property map. Substituted as
    /// the empty string in the output.
    Undefined { name: String },
    /// `$(...)` containing anything other than a bare identifier — for
    /// example `$([System.IO.Path]::Combine(...))`. Left literal in the
    /// output.
    Unsupported { expression: String },
}

/// Property bag keyed case-insensitively, matching MSBuild's
/// `StringComparison.OrdinalIgnoreCase` rule for property names. We
/// preserve the canonical (first-written) casing for output so callers
/// see names exactly as they appeared in the project source.
#[derive(Debug, Clone, Default)]
struct Entry {
    /// The canonical key casing (from the first insertion).
    canonical: String,
    /// The value, in MSBuild's **escaped domain** — the form MSBuild itself
    /// stores. It leaves the domain exactly once, at a point of use; see
    /// [`escaping::Escaped`].
    value: Escaped,
}

#[derive(Debug, Default, Clone)]
pub struct PropertyMap {
    inner: HashMap<String, Entry>,
}

impl PropertyMap {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// Insert or overwrite with **project XML** text (a property body, an
    /// attribute, or a caller-supplied global): already escaped-domain text,
    /// stored verbatim. The canonical case is taken from the first insertion;
    /// later writes update the value but leave the recorded casing alone
    /// (mirrors MSBuild, which echoes back the original declaration's casing
    /// regardless of how reassignments spelt it).
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.insert_escaped(key, Escaped::from_xml(value.into()));
    }

    /// Insert text the evaluator **computed from the world**: a filesystem
    /// path, a toolset/SDK seed, the `MSBuildThisFile*` pair. Escaped on the
    /// way in, exactly as MSBuild escapes such values when it seeds them
    /// (`Evaluator.cs:1186–1189`, `Toolset.cs:802`) — which is what makes a
    /// `%`, `;` or `(` in a project's own path inert rather than an escape, a
    /// list separator, or an expression delimiter.
    pub fn insert_computed(&mut self, key: impl Into<String>, value: impl AsRef<str>) {
        self.insert_escaped(key, Escaped::from_computed(value.as_ref()));
    }

    /// Insert an already-escaped value — the result of a substitution, which is
    /// composed *in* the domain and never left it.
    pub fn insert_escaped(&mut self, key: impl Into<String>, value: Escaped) {
        let key = key.into();
        let lower = key.to_ascii_lowercase();
        match self.inner.entry(lower) {
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                slot.get_mut().value = value;
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(Entry {
                    canonical: key,
                    value,
                });
            }
        }
    }

    /// The stored value, still escaped. The caller must decide how to leave the
    /// domain — `Escaped::unescape` at a point of use, `Escaped::as_escaped` for
    /// a splice or a scan MSBuild also performs on escaped text. (The domain type
    /// is crate-internal by design: no consumer outside the evaluator should be
    /// able to pick the wrong one.)
    pub fn get(&self, key: &str) -> Option<&Escaped> {
        let lower = key.to_ascii_lowercase();
        self.inner.get(&lower).map(|e| &e.value)
    }

    /// The stored value at its point of use: unescaped exactly once. This is
    /// what MSBuild's own `ProjectProperty.EvaluatedValue` returns
    /// (`ProjectProperty.cs:89`), and the form every consumer outside this crate
    /// sees.
    pub fn get_unescaped(&self, key: &str) -> Option<String> {
        self.get(key).map(Escaped::unescape)
    }

    /// Remove the binding for `key` (case-insensitive), if any. Used to
    /// invalidate a property after a tainted overwrite — leaving the
    /// prior value behind would let later substitutions resolve to the
    /// stale value rather than emit Undefined.
    pub fn remove(&mut self, key: &str) {
        let lower = key.to_ascii_lowercase();
        self.inner.remove(&lower);
    }

    /// Canonical keys (the casing the value was first inserted with).
    pub fn canonical_keys(&self) -> impl Iterator<Item = &str> {
        self.inner.values().map(|e| e.canonical.as_str())
    }
}

/// Reserved MSBuild properties derivable purely from the project file's
/// path. We seed these so that common substitutions
/// (`$(MSBuildProjectDirectory)/...`, `$(MSBuildProjectName)`) work out
/// of the box without the caller having to supply them.
///
/// MSBuild's authoritative list is larger (`MSBuildBinPath`,
/// `MSBuildExtensionsPath`, the `MSBuildThisFile*` family for the
/// currently-importing file, etc.). We only seed the path-derivable
/// subset; SDK-dependent paths are left out because we don't model
/// SDKs in phase 2.
pub fn well_known(project_path: &Path) -> PropertyMap {
    let mut map = PropertyMap::new();
    let full_path = project_path.to_string_lossy().into_owned();
    let dir = project_path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = project_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = project_path
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = project_path
        .extension()
        .map(|n| format!(".{}", n.to_string_lossy()))
        .unwrap_or_default();
    // MSBuild docs: `MSBuildProjectDirectory` has no trailing separator;
    // `MSBuildThisFileDirectory` does. Replicate that distinction.
    let dir_with_sep = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    map.insert_computed("MSBuildProjectFullPath", full_path);
    map.insert_computed("MSBuildProjectDirectory", dir);
    map.insert_computed("MSBuildThisFileDirectory", dir_with_sep);
    map.insert_computed("MSBuildProjectName", stem);
    // MSBuild seeds both names with the project's filename:
    // `MSBuildThisFile` (relative to whichever file is currently
    // importing — for us, always the project) and `MSBuildProjectFile`
    // (the project-scoped alias). Real projects reach for either, and
    // omitting the alias makes `$(MSBuildProjectFile)` resolve to empty,
    // which truncates Include paths without a useful diagnostic.
    map.insert_computed("MSBuildProjectFile", file.clone());
    map.insert_computed("MSBuildThisFile", file);
    map.insert_computed("MSBuildProjectExtension", extension);
    map
}

/// Expand `$(Name)` occurrences in `input` — **project XML text**, hence
/// already escaped-domain text — against `props`. The result stays in the
/// domain: composition happens *inside* it, exactly as in MSBuild, so the
/// caller decides where to leave it (see [`escaping::Escaped`]).
///
/// Undefined references are replaced with the empty string (mirroring MSBuild);
/// unsupported expressions are passed through literally so the caller can detect
/// residual `$(` after substitution.
///
/// Composing in the domain is what makes the awkward corners fall out rather
/// than needing rules of their own. A `%` in the project's own path is inert
/// because the seed was escaped to `%25`; an escape composed across a splice
/// boundary (`$(Alpha)$(Alpha)` with `<Alpha>100%</Alpha>` → `100%100%`, in
/// which MSBuild reads `%10`) is decoded by whichever leaf unescapes, because
/// the composition is what it decodes.
pub fn substitute(input: &str, props: &PropertyMap) -> (Escaped, Vec<Issue>) {
    substitute_impl(input, props, false)
}

/// [`substitute`], with the filesystem-probing property functions enabled
/// (`GetDirectoryNameOfFileAbove` walks ancestor directories with
/// `is_file`). Only the with-imports evaluation path may use this; the
/// pure `parse_fsproj` surface promises no filesystem access, so there
/// the same expression stays a visible [`Issue::Unsupported`].
pub fn substitute_with_fs(input: &str, props: &PropertyMap) -> (Escaped, Vec<Issue>) {
    substitute_impl(input, props, true)
}

fn substitute_impl(
    input: &str,
    props: &PropertyMap,
    fs_probes_allowed: bool,
) -> (Escaped, Vec<Issue>) {
    // The buffer is in the escaped domain throughout: the XML text between
    // references is already escaped text, and every spliced value is escaped
    // (a stored property is, by construction; an expression result is escaped
    // by the evaluator — bar the one `Char` hole it models). Nothing here needs
    // to know *where* a `%` came from, which is the whole point.
    let mut out = Escaped::default();
    let mut issues = Vec::new();
    let mut rest = input;
    while let Some(idx) = rest.find("$(") {
        out.push_xml(&rest[..idx]);
        let after = &rest[idx + 2..];
        // Fast path: simple `$(Identifier)` — match the identifier and a
        // single closing paren without doing any nested-paren counting.
        // This is the only case we substitute without parsing. MSBuild
        // property names allow letters, digits, `_`, and `-`, but **not**
        // `.`: inside `$(…)` a dot is always member access, never part of a
        // name (a dotted name is illegal at every MSBuild source —
        // MSB5016/MSB4177). So a reference containing `.` (`$(Foo.Bar)`,
        // `$(Foo.Method())`) drops to the slow path and is parsed as an
        // expression by [`expr`]; a bare `$(Name)` short-circuits here.
        let id_len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            .count();
        if id_len > 0 && after.as_bytes().get(id_len) == Some(&b')') {
            let name = &after[..id_len];
            rest = &after[id_len + 1..];
            if let Some(value) = props.get(name) {
                out.push(value);
            } else {
                issues.push(Issue::Undefined {
                    name: name.to_string(),
                });
            }
            continue;
        }
        // Slow path: anything else — property functions, member chains,
        // nested expressions. Find the matching close paren (nesting-aware)
        // so the diagnostic captures the whole expression accurately, then
        // hand the interior to the expression parser/evaluator.
        match expr::find_close(after) {
            Some(close) => {
                let inner = &after[..close];
                rest = &after[close + 1..];
                if let Some(evaluated) = expr::evaluate(inner, props, fs_probes_allowed) {
                    issues.extend(evaluated.issues);
                    // The evaluator hands back an `Escaped`: a function result
                    // is escaped (so its `%` cannot compose an escape with what
                    // follows), while a string indexer's `Char` went in raw —
                    // MSBuild's own hole, modelled in `expr::evaluate`.
                    out.push(&evaluated.value);
                } else {
                    issues.push(Issue::Unsupported {
                        expression: format!("$({inner})"),
                    });
                    out.push_xml("$(");
                    out.push_xml(inner);
                    out.push_xml(")");
                }
            }
            None => {
                // *Our scanner* found no matching close. MSBuild passes a
                // genuinely unbalanced `$(` through literally (`a$(b`
                // expands to itself, oracle-pinned), but its scanner's
                // quote handling differs from ours on malformed nestings —
                // the generative sweep found inputs we deem unbalanced
                // that MSBuild scans as balanced and then *errors* on. We
                // can't tell those apart, so keep the characters (the
                // caller still sees the rough shape) but withdraw the
                // claim with an issue.
                issues.push(Issue::Unsupported {
                    expression: rest[idx..].to_string(),
                });
                out.push_xml(&rest[idx..]);
                return (out, issues);
            }
        }
    }
    out.push_xml(rest);
    (out, issues)
}

// The subset of `[MSBuild]::` / `[System.IO.Path]::` static functions and
// `System.String` instance methods we evaluate lives in the `expr` dispatch
// tables; the leaf computations (TFM inference, path handling) stay here and
// are called from there. Keep the dispatch narrow: unsupported expressions
// must stay visible rather than looking successfully reduced.
//
// The path functions below (`GetDirectoryNameOfFileAbove`, `NormalizePath`,
// `Combine`) are the ones NuGet.props uses to discover
// `Directory.Packages.props`. A wrong answer there silently redirects a whole
// *import*, so an argument we couldn't pin down exactly (an undefined
// property, a relative path whose base would be the MSBuild process working
// directory, a Windows drive path on a unix host) refuses to evaluate — the
// expression stays visibly unsupported instead of resolving to a confidently
// wrong path. `GetDirectoryNameOfFileAbove` probes the filesystem (read-only),
// the one impurity in this module, and is gated on the caller's fs capability.

/// Evaluate one argument of a path function to an *exact* string: a
/// single-quoted literal or a bare expression fragment (the SDK writes
/// both — `Microsoft.Common.props` passes `$(MSBuildProjectDirectory)`
/// unquoted where `NuGet.props` quotes it). `None` when the argument
/// leaned on an undefined property or an unsupported nested expression —
/// a path assembled from a guess must stay visibly unsupported.
fn eval_exact_path_arg(
    arg: &str,
    props: &PropertyMap,
    reject_escaped_backslash: bool,
) -> Option<String> {
    let (value, issues) = match string_literal_arg(arg) {
        Some(quoted) => substitute(quoted, props),
        None => {
            let trimmed = arg.trim();
            if trimmed.contains(STRING_DELIMS) {
                return None;
            }
            substitute(trimmed, props)
        }
    };
    // A path function's argument is a **point of use**: MSBuild unescapes
    // before the function runs (`Combine` of `a%2fb` and `b` is `a/b/b`,
    // oracle-pinned 2026-07-11), so the argument leaves the domain here. This
    // used to decline instead, because the raw text was all we had.
    if !issues.is_empty() {
        return None;
    }
    let unescaped = value.unescape();
    // A decoded NUL makes `Path.GetFullPath` throw (failing the whole
    // evaluation), so any path function declines it — E3 newly admits it. A
    // decoded backslash is *not* declined here: `NormalizePath` normalises it
    // correctly (via `GetFullPath`), and only `Combine` diverges, so that guard
    // is Combine-specific (see `expr::eval_static`).
    if unescaped.contains('\0') {
        return None;
    }
    // `Combine` only: an *escaped* backslash (`%5c`) is invisible to MSBuild's
    // unix path fixup (which scans escaped text), so `Combine` leaves it a
    // literal `\` in the result (`Combine('a%5cb','c')` is `a\b/c`, oracle-pinned
    // 2026-07-13) — whereas `combine_path` converts every `\`→`/`. A *live*
    // backslash *is* fixed up, so those still commit. `NormalizePath` /
    // `GetDirectoryNameOfFileAbove` normalise separators via `GetFullPath`, which
    // converts a backslash regardless of how it was spelled
    // (`NormalizePath('/a%5cb', …)` is `/a/b/…`, oracle-pinned), so they pass
    // `false` and keep committing. (Unix only — on Windows the percent-escape
    // guard below already covers it.)
    if reject_escaped_backslash && !cfg!(windows) && contains_escaped_backslash(value.as_escaped())
    {
        return None;
    }
    // On Windows, `combine_path`/`normalize_path` join with `/` where .NET's
    // `Path` uses `\`, so a call that E3 newly admits — one whose argument
    // carried a `%XX` escape — would commit the wrong separator
    // (`Combine('a%20b','c')` is `a b\c` on Windows, not our `a b/c`). Those
    // declined pre-E3 at the entry guard; keep them declining until the helper is
    // host-correct.
    if cfg!(windows) && contains_percent_escape(value.as_escaped()) {
        return None;
    }
    Some(unescaped)
}

/// A `%5c`/`%5C` escape — an *escaped* backslash. It survives MSBuild's unix
/// path fixup (which scans the escaped text and finds no `\`) and decodes to a
/// literal `\`, where a converter that rewrites every `\`→`/` would diverge.
/// See [`eval_exact_path_arg`].
fn contains_escaped_backslash(escaped: &str) -> bool {
    escaped
        .as_bytes()
        .windows(3)
        .any(|w| w[0] == b'%' && w[1] == b'5' && (w[2] == b'c' || w[2] == b'C'))
}

/// A `%` followed by two ASCII hex digits — MSBuild's `%XX` escape.
fn contains_percent_escape(s: &str) -> bool {
    s.as_bytes()
        .windows(3)
        .any(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit())
}

fn eval_exact_path_args(
    args: &str,
    props: &PropertyMap,
    reject_escaped_backslash: bool,
) -> Option<Vec<String>> {
    let args = split_args(args)?;
    if args.is_empty() {
        return None;
    }
    args.into_iter()
        .map(|arg| eval_exact_path_arg(arg, props, reject_escaped_backslash))
        .collect()
}

/// `$([MSBuild]::GetDirectoryNameOfFileAbove(start, file))`: walk up from
/// `start` (inclusive) looking for `file`; return the directory containing
/// it, or `""` when the search exhausts the tree (MSBuild returns empty,
/// not an error). `None` — surfaced as [`Issue::Unsupported`] by the
/// caller — for the shapes we can't answer faithfully: a relative or
/// Windows-drive `start` (the real base would be the MSBuild process
/// state we don't receive), or a `file` with `..`/absolute components
/// (MSBuild combines and probes those; refusing is conservative).
fn get_directory_name_of_file_above(start: &str, file: &str) -> Option<String> {
    let start = start.replace('\\', "/");
    let file = file.replace('\\', "/");
    if rejects_windows_drive_path(&start) || rejects_windows_drive_path(&file) {
        return None;
    }
    let start_dir = std::path::Path::new(&start);
    // On a Windows host `C:/repo` is absolute by std's own rules; on a
    // unix host only `/…` is (drive paths were rejected above).
    if !start_dir.is_absolute() {
        return None;
    }
    let file_path = std::path::Path::new(&file);
    if file.is_empty()
        || !file_path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    let mut dir = start_dir.to_path_buf();
    loop {
        if dir.join(file_path).is_file() {
            return Some(dir.to_string_lossy().into_owned());
        }
        if !dir.pop() {
            return Some(String::new());
        }
    }
}

/// `$([MSBuild]::NormalizePath(parts…))`: .NET
/// `Path.GetFullPath(Path.Combine(parts))`. Combine ignores empty parts
/// and restarts at a later absolute part; GetFullPath resolves `.`/`..`
/// lexically (excess `..` stops at the root) and preserves a trailing
/// separator. `None` — [`Issue::Unsupported`] — when the combined path is
/// relative (GetFullPath would resolve it against the MSBuild process
/// working directory) or names a Windows drive on a unix host.
fn normalize_path(parts: &[String]) -> Option<String> {
    let combined = combine_path(parts)?;
    if !is_rooted(&combined) {
        return None;
    }
    // A Windows-host drive root (`C:/…`) and a Windows-host UNC root
    // (`//server/share/…`, the normalised `\\server\share`) survive
    // normalisation as unpoppable prefixes, exactly like `/` on unix —
    // .NET's GetFullPath cannot `..` above a share root. On unix hosts
    // .NET does NOT treat `//server/share` as special (verified against
    // `dotnet msbuild`: `NormalizePath('//server/share', '../../z')` is
    // `/z`), so there the double slash falls through to the ordinary
    // `/` root below and collapses.
    let (root, rest) = if let Some(unc) = combined.strip_prefix("//").filter(|_| cfg!(windows)) {
        let mut segments = unc.splitn(3, '/');
        let (Some(server), Some(share)) = (segments.next(), segments.next()) else {
            // `//server` with no share names no filesystem object a
            // path function can reason about — stay unsupported.
            return None;
        };
        if server.is_empty() || share.is_empty() {
            return None;
        }
        (
            format!("//{server}/{share}/"),
            segments.next().unwrap_or(""),
        )
    } else {
        match combined.split_once('/') {
            Some((drive, rest)) if !drive.is_empty() => (format!("{drive}/"), rest),
            _ => ("/".to_string(), &combined[1..]),
        }
    };
    let trailing_separator = combined.ends_with('/') && combined.len() > root.len();
    let mut resolved: Vec<&str> = Vec::new();
    for segment in rest.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    let mut out = String::with_capacity(combined.len());
    out.push_str(&root);
    out.push_str(&resolved.join("/"));
    if trailing_separator && out.len() > root.len() {
        out.push('/');
    }
    Some(out)
}

/// `$([System.IO.Path]::Combine(parts…))`: .NET `Path.Combine` — join with
/// a separator, skip empty parts, restart at a later absolute part, and
/// perform **no** `.`/`..` normalisation. Unlike [`normalize_path`] the
/// result may legitimately be relative (Combine does not resolve).
/// `None` for Windows drive paths, which this unix-host evaluator must
/// not guess at.
fn combine_path(parts: &[String]) -> Option<String> {
    let mut combined = String::new();
    for part in parts {
        let part = part.replace('\\', "/");
        if rejects_windows_drive_path(&part) {
            return None;
        }
        if part.is_empty() {
            continue;
        }
        if is_rooted(&part) || combined.is_empty() {
            combined = part;
        } else {
            if !combined.ends_with('/') {
                combined.push('/');
            }
            combined.push_str(&part);
        }
    }
    Some(combined)
}

/// `C:`-style prefix (any drive letter), after backslash normalisation.
fn has_windows_drive_prefix(part: &str) -> bool {
    let bytes = part.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Whether a drive-qualified path must refuse to evaluate. On a unix
/// host a `C:\…` path cannot be probed or normalised faithfully, so the
/// expression stays visibly unsupported; a Windows host owns drive
/// semantics natively (`$(MSBuildProjectDirectory)` IS drive-qualified
/// there) and must not reject them.
fn rejects_windows_drive_path(part: &str) -> bool {
    !cfg!(windows) && has_windows_drive_prefix(part)
}

/// Rooted ≡ "an absolute base that later `Path.Combine` arguments reset
/// to", host-aware: `/…` everywhere, plus `C:/…` on a Windows host.
fn is_rooted(part: &str) -> bool {
    part.starts_with('/')
        || (cfg!(windows)
            && has_windows_drive_prefix(part)
            && part.as_bytes().get(2) == Some(&b'/'))
}

/// MSBuild's three interchangeable function string-literal delimiters. A
/// string closes only at its *own* delimiter; the other two are ordinary
/// text inside it.
const STRING_DELIMS: [char; 3] = ['\'', '`', '"'];

fn split_args(args: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut in_string: Option<char> = None;
    for (i, c) in args.char_indices() {
        if let Some(delim) = in_string {
            if c == delim {
                in_string = None;
            }
            continue;
        }
        match c {
            _ if STRING_DELIMS.contains(&c) => in_string = Some(c),
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                parts.push(args[start..i].trim());
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    if in_string.is_some() || depth != 0 {
        return None;
    }
    parts.push(args[start..].trim());
    Some(parts)
}

fn string_literal_arg(arg: &str) -> Option<&str> {
    let arg = arg.trim();
    for delim in STRING_DELIMS {
        if let Some(inner) = arg.strip_prefix(delim).and_then(|s| s.strip_suffix(delim)) {
            if inner.contains(delim) {
                return None;
            }
            return Some(inner);
        }
    }
    None
}

fn parse_target_framework_version_part_count(arg: &str) -> Option<usize> {
    let count = arg.trim().parse().ok()?;
    if (0..=MAX_TARGET_FRAMEWORK_VERSION_PARTS).contains(&count) {
        Some(count)
    } else {
        None
    }
}

fn infer_target_framework_identifier(tfm: &str) -> Option<&'static str> {
    let short = tfm.split('-').next().unwrap_or("").to_ascii_lowercase();
    if let Some(suffix) = parse_prefixed_version(&short, "netstandard") {
        return parse_prefixed_framework_version(suffix).map(|_| ".NETStandard");
    }
    if let Some(suffix) = parse_prefixed_version(&short, "netcoreapp") {
        return parse_prefixed_framework_version(suffix).map(|_| ".NETCoreApp");
    }
    let suffix = short.strip_prefix("net")?;
    if !suffix.is_empty() && suffix.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
        if parse_dotted_netframework_version(suffix).is_some() {
            Some(".NETFramework")
        } else if suffix.contains('.') {
            parse_dotted_version(suffix).map(|_| ".NETCoreApp")
        } else if parse_compact_netcoreapp_version(suffix).is_some() {
            Some(".NETCoreApp")
        } else {
            Some(".NETFramework")
        }
    } else {
        None
    }
}

fn infer_target_framework_version(tfm: &str, min_parts: usize) -> Option<String> {
    let short = tfm.split('-').next().unwrap_or("").to_ascii_lowercase();
    let version = if let Some(suffix) = parse_prefixed_version(&short, "netstandard") {
        parse_prefixed_framework_version(suffix)?
    } else if let Some(suffix) = parse_prefixed_version(&short, "netcoreapp") {
        parse_prefixed_framework_version(suffix)?
    } else {
        let suffix = short.strip_prefix("net")?;
        if suffix.contains('.') {
            parse_dotted_version(suffix)?
        } else {
            parse_netframework_version(suffix)?
        }
    };
    Some(normalize_version_parts(version, min_parts))
}

/// Compare two operands of an `[MSBuild]::Version*` intrinsic
/// (`VersionEquals`, `VersionGreaterThanOrEquals`, …). Shared by the condition
/// evaluator ([`crate::condition`]) and the property-expression evaluator
/// ([`expr`]) so the two paths cannot drift. `Err(())` (⇒ the caller declines,
/// matching the real build's MSB4184 error) for either operand malformed; see
/// [`parse_msbuild_version`].
pub(crate) fn compare_msbuild_versions(lhs: &str, rhs: &str) -> Result<std::cmp::Ordering, ()> {
    Ok(parse_msbuild_version(lhs)?.cmp(&parse_msbuild_version(rhs)?))
}

/// Parse a version operand as the `[MSBuild]::Version*` intrinsics do
/// (ground-truthed against `dotnet msbuild` 10.0.301): trim **Unicode**
/// whitespace (NBSP-padded operands parse — distinct from the ASCII-only
/// [`AreFeaturesEnabled`](expr) path), strip a single leading `v`/`V`, drop a
/// prerelease/metadata suffix at the first `-`/`+` (`10.0.100-preview.1` →
/// `10.0.100`, `3+meta` → `3` — MSBuild compares numerically and is *not*
/// SemVer-aware, so `1.0.0` neither exceeds nor trails `1.0.0-preview`), then
/// require **1–4** dot-separated non-empty all-ASCII-digit components (each
/// ≤ `i32::MAX`), padded to four with zeros so **missing components compare as
/// 0** (`1.0` == `1.0.0`). `Err(())` for empty, non-numeric, or >4-component
/// input, each an MSB4184 error in the real build.
pub(crate) fn parse_msbuild_version(value: &str) -> Result<[u32; 4], ()> {
    let value = value.trim();
    let value = value
        .strip_prefix('v')
        .or_else(|| value.strip_prefix('V'))
        .unwrap_or(value);
    let value = value.find(['-', '+']).map_or(value, |idx| &value[..idx]);
    if value.is_empty() {
        return Err(());
    }
    let parts: Vec<&str> = value.split('.').collect();
    if !(1..=4).contains(&parts.len()) {
        return Err(());
    }
    let mut version = [0; 4];
    for (idx, part) in parts.into_iter().enumerate() {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(());
        }
        let part = part.parse::<u32>().map_err(|_| ())?;
        if part > i32::MAX as u32 {
            return Err(());
        }
        version[idx] = part;
    }
    Ok(version)
}

/// `[MSBuild]::GetTargetPlatformIdentifier(tfm)` — the platform moniker of a
/// TFM (`net8.0-windows` → `windows`), empty for a platform-free TFM.
///
/// Pinned (oracle 10.0.301) only for the **platform-free** case: a recognised
/// base TFM with no `-` component → `""`. A platform-bearing TFM
/// (`net8.0-windows`) and an unrecognised base (`garbage`, which MSBuild maps
/// to `""`) both decline — the platform-moniker/version parse envelope is not
/// yet pinned, and the plain SDK chain (net10.0 / net8.0 / netstandard2.x)
/// never targets a platform. This is exact-or-decline: we never commit a wrong
/// moniker.
pub(crate) fn infer_target_platform_identifier(tfm: &str) -> Option<String> {
    if tfm.contains('-') {
        return None;
    }
    infer_target_framework_identifier(tfm).map(|_| String::new())
}

/// `[MSBuild]::GetTargetPlatformVersion(tfm, minParts)` — the platform version
/// of a TFM, `0.0` for a platform-free TFM. Same platform-free-only envelope as
/// [`infer_target_platform_identifier`]. The min-part count floors at 1
/// (`minParts == 0` → `"0"`, oracle-pinned) and trailing zeros are trimmed —
/// the same [`normalize_version_parts`] formatting `GetTargetFrameworkVersion`
/// uses (whose framework version, being non-zero, never exposes the all-zeros
/// floor this one does).
pub(crate) fn infer_target_platform_version(tfm: &str, min_parts: usize) -> Option<String> {
    if tfm.contains('-') {
        return None;
    }
    infer_target_framework_identifier(tfm)?;
    Some(normalize_version_parts(
        vec!["0".to_string(), "0".to_string()],
        min_parts.max(1),
    ))
}

fn parse_dotted_netframework_version(suffix: &str) -> Option<Vec<String>> {
    let version = parse_dotted_version(suffix)?;
    if matches!(
        version.first().map(String::as_str),
        Some("1" | "2" | "3" | "4")
    ) {
        Some(version)
    } else {
        None
    }
}

fn parse_compact_netcoreapp_version(suffix: &str) -> Option<Vec<String>> {
    let version = parse_netframework_version(suffix)?;
    if matches!(
        version.first().map(String::as_str),
        Some("5" | "6" | "7" | "8" | "9")
    ) {
        Some(version)
    } else {
        None
    }
}

fn parse_prefixed_framework_version(suffix: &str) -> Option<Vec<String>> {
    if suffix.contains('.') {
        parse_dotted_version(suffix)
    } else {
        parse_compact_version(suffix)
    }
}

fn parse_prefixed_version<'a>(tfm: &'a str, prefix: &str) -> Option<&'a str> {
    let suffix = tfm.strip_prefix(prefix)?;
    if suffix.is_empty() {
        None
    } else {
        Some(suffix)
    }
}

fn parse_dotted_version(suffix: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = suffix.split('.').map(str::to_string).collect();
    if parts.is_empty()
        || parts.len() > MAX_TARGET_FRAMEWORK_VERSION_PARTS
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some(parts)
}

fn parse_netframework_version(suffix: &str) -> Option<Vec<String>> {
    parse_compact_version(suffix)
}

fn parse_compact_version(suffix: &str) -> Option<Vec<String>> {
    if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut parts: Vec<String> = suffix.chars().map(|c| c.to_string()).collect();
    if parts.len() > MAX_TARGET_FRAMEWORK_VERSION_PARTS {
        return None;
    }
    if parts.len() == 1 {
        parts.push("0".to_string());
    }
    Some(parts)
}

fn normalize_version_parts(mut parts: Vec<String>, min_parts: usize) -> String {
    while parts.len() > min_parts && parts.last().is_some_and(|part| part == "0") {
        parts.pop();
    }
    while parts.len() < min_parts {
        parts.push("0".to_string());
    }
    parts.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> PropertyMap {
        let mut m = PropertyMap::new();
        for (k, v) in pairs {
            m.insert(*k, *v);
        }
        m
    }

    /// Expand and take the result to its **point of use** — the value MSBuild's
    /// `GetPropertyValue` returns. Shadows [`super::substitute`], which now
    /// (correctly) stops in the escaped domain; these tests assert what a
    /// consumer finally sees, so they unescape exactly once, like every real
    /// leaf does.
    fn substitute(input: &str, props: &PropertyMap) -> (String, Vec<Issue>) {
        let (value, issues) = super::substitute(input, props);
        (value.unescape(), issues)
    }

    /// The same, for the fs-probing variant.
    #[allow(dead_code)]
    fn substitute_with_fs(input: &str, props: &PropertyMap) -> (String, Vec<Issue>) {
        let (value, issues) = super::substitute_with_fs(input, props);
        (value.unescape(), issues)
    }

    #[test]
    fn passthrough_when_no_dollar_paren() {
        let (out, issues) = substitute("plain text", &PropertyMap::new());
        assert_eq!(out, "plain text");
        assert!(issues.is_empty());
    }

    #[test]
    fn simple_identifier_substitutes() {
        let (out, issues) = substitute("$(Foo)-tail", &map(&[("Foo", "value")]));
        assert_eq!(out, "value-tail");
        assert!(issues.is_empty());
    }

    #[test]
    fn defined_to_empty_substitutes_empty_with_no_issue() {
        let (out, issues) = substitute("[$(Empty)]", &map(&[("Empty", "")]));
        assert_eq!(out, "[]");
        assert!(issues.is_empty());
    }

    #[test]
    fn undefined_substitutes_empty_and_reports() {
        let (out, issues) = substitute("[$(Missing)]", &PropertyMap::new());
        assert_eq!(out, "[]");
        assert_eq!(
            issues,
            vec![Issue::Undefined {
                name: "Missing".to_string()
            }]
        );
    }

    #[test]
    fn multiple_references_all_substitute() {
        let (out, issues) = substitute("$(A)/$(B)", &map(&[("A", "x"), ("B", "y")]));
        assert_eq!(out, "x/y");
        assert!(issues.is_empty());
    }

    #[test]
    fn property_function_captures_full_balanced_expression() {
        // The unsupported-expression diagnostic must capture the whole
        // `$([...](...))` including the closing paren of the outer call,
        // not stop at the first inner `)`.
        let expr = "$([System.IO.Path]::GetFullPath('a', 'b'))";
        let (out, issues) = substitute(expr, &PropertyMap::new());
        assert_eq!(out, expr, "unsupported expressions pass through literally");
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    #[test]
    fn msbuild_get_target_framework_identifier_is_supported() {
        let (out, issues) = substitute(
            "$([MSBuild]::GetTargetFrameworkIdentifier('$(TargetFramework)'))",
            &map(&[("TargetFramework", "net10.0")]),
        );
        assert_eq!(out, ".NETCoreApp");
        assert!(issues.is_empty());
    }

    #[test]
    fn msbuild_get_target_framework_identifier_rejects_non_literal_argument() {
        let expr = "$([MSBuild]::GetTargetFrameworkIdentifier('net8.0' + 'foo'))";
        let (out, issues) = substitute(expr, &PropertyMap::new());
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    #[test]
    fn msbuild_get_target_framework_version_is_supported() {
        let (out, issues) = substitute(
            "$([MSBuild]::GetTargetFrameworkVersion('$(TargetFramework)', 2))",
            &map(&[("TargetFramework", "net472")]),
        );
        assert_eq!(out, "4.7.2");
        assert!(issues.is_empty());
    }

    #[test]
    fn msbuild_get_target_framework_version_defaults_to_two_parts() {
        let (out, issues) = substitute(
            "$([MSBuild]::GetTargetFrameworkVersion('net8.0'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "8.0");
        assert!(issues.is_empty());
    }

    #[test]
    fn msbuild_get_target_framework_version_accepts_zero_part_count() {
        let (out, issues) = substitute(
            "$([MSBuild]::GetTargetFrameworkVersion('net8.0', 0))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "8");
        assert!(issues.is_empty());
    }

    #[test]
    fn msbuild_get_target_framework_version_rejects_excessive_part_count() {
        let expr = "$([MSBuild]::GetTargetFrameworkVersion('net8.0', 1000000000))";
        let (out, issues) = substitute(expr, &PropertyMap::new());
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    #[test]
    fn prefixed_target_framework_identifier_requires_valid_version() {
        for tfm in ["netstandard2.O", "netcoreapp3.O"] {
            assert_eq!(infer_target_framework_identifier(tfm), None, "{tfm}");
        }
    }

    #[test]
    fn dotted_net_target_framework_identifier_requires_valid_version() {
        for tfm in ["net5.x", "net5."] {
            assert_eq!(infer_target_framework_identifier(tfm), None, "{tfm}");
        }
    }

    #[test]
    fn dotted_target_framework_versions_reject_too_many_parts() {
        for tfm in [
            "net8.0.0.0.0",
            "netstandard2.0.0.0.0",
            "netcoreapp3.1.0.0.0",
        ] {
            assert_eq!(infer_target_framework_identifier(tfm), None, "{tfm}");
            assert_eq!(infer_target_framework_version(tfm, 2), None, "{tfm}");
        }
    }

    #[test]
    fn trim_start_property_function_is_supported() {
        let (out, issues) = substitute(
            "$(_Prefix)$(TargetFrameworkVersion.TrimStart('vV'))",
            &map(&[("_Prefix", "tfm="), ("TargetFrameworkVersion", "v10.0")]),
        );
        assert_eq!(out, "tfm=10.0");
        assert!(issues.is_empty());
    }

    #[test]
    fn trim_start_property_function_rejects_chained_call() {
        let expr = "$(TargetFrameworkVersion.TrimStart('vV').Replace('.', '_'))";
        let (out, issues) = substitute(expr, &map(&[("TargetFrameworkVersion", "v8.0")]));
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    // --- string instance methods `.Contains` / `.StartsWith` / `.EndsWith` ---
    //
    // These are .NET `string` instance methods returning a `bool`, which
    // MSBuild renders through `bool.ToString()` — capital-`T` "True" /
    // capital-`F` "False". They are ordinal and case-sensitive. Every
    // expected value below is pinned against `dotnet msbuild` 10.0.300.
    //   $(P.Contains('{'))   P=1.2.3    => False
    //   $(P.Contains('.'))   P=1.2.3    => True
    //   $(P.Contains('A'))   P=abc      => False   (case-sensitive)
    //   $(P.StartsWith('1.')) P=1.2.3   => True
    //   $(P.EndsWith('.3'))  P=1.2.3    => True
    //   $(Missing.Contains('x'))        => False   (unset property => "")
    //   $(P.Contains(''))    P=abc      => True    (empty substring)

    #[test]
    fn string_contains_is_supported() {
        let p = map(&[("FSCorePackageVersion", "8.0.0")]);
        let (out, issues) = substitute("$(FSCorePackageVersion.Contains('{'))", &p);
        assert_eq!(out, "False");
        assert!(issues.is_empty());
        let (out, issues) = substitute("$(FSCorePackageVersion.Contains('.'))", &p);
        assert_eq!(out, "True");
        assert!(issues.is_empty());
    }

    #[test]
    fn string_contains_is_case_sensitive_ordinal() {
        let (out, issues) = substitute("$(P.Contains('A'))", &map(&[("P", "abc")]));
        assert_eq!(out, "False");
        assert!(issues.is_empty());
    }

    #[test]
    fn string_starts_with_and_ends_with_are_supported() {
        let p = map(&[("P", "1.2.3")]);
        let (out, issues) = substitute("$(P.StartsWith('1.'))", &p);
        assert_eq!(out, "True");
        assert!(issues.is_empty());
        let (out, issues) = substitute("$(P.StartsWith('9'))", &p);
        assert_eq!(out, "False");
        assert!(issues.is_empty());
        let (out, issues) = substitute("$(P.EndsWith('.3'))", &p);
        assert_eq!(out, "True");
        assert!(issues.is_empty());
    }

    #[test]
    fn string_method_on_undefined_property_is_empty_and_reports() {
        let (out, issues) = substitute("$(Missing.Contains('x'))", &PropertyMap::new());
        assert_eq!(out, "False");
        assert_eq!(
            issues,
            vec![Issue::Undefined {
                name: "Missing".to_string()
            }]
        );
    }

    #[test]
    fn string_contains_empty_substring_is_true() {
        let (out, issues) = substitute("$(P.Contains(''))", &map(&[("P", "abc")]));
        assert_eq!(out, "True");
        assert!(issues.is_empty());
    }

    #[test]
    fn string_method_argument_is_substituted() {
        // The single string-literal argument may itself contain `$(...)`;
        // MSBuild expands it before the comparison.
        let (out, issues) = substitute(
            "$(P.Contains('$(N)'))",
            &map(&[("P", "1.2.3"), ("N", ".2.")]),
        );
        assert_eq!(out, "True");
        assert!(issues.is_empty());
    }

    #[test]
    fn string_method_name_is_case_insensitive_and_tolerates_whitespace() {
        // MSBuild resolves property-function member names case-insensitively
        // and tolerates whitespace before the argument list (pinned against
        // dotnet msbuild 10.0.300).
        let p = map(&[("P", "1.2.3")]);
        for expr in [
            "$(P.contains('.'))",
            "$(P.CONTAINS('.'))",
            "$(P.Contains ('.'))",
            "$(P.startswith('1'))",
            "$(P.EndsWith ('.3'))",
        ] {
            let (out, issues) = substitute(expr, &p);
            assert_eq!(out, "True", "{expr}");
            assert!(issues.is_empty(), "{expr}: {issues:?}");
        }
    }

    #[test]
    fn contains_is_ordinal_for_non_ascii() {
        // `String.Contains(String)` is ordinal, so a non-ASCII needle is
        // matched literally — Rust's `contains` agrees exactly, and we
        // commit. Pinned against dotnet msbuild 10.0.300:
        //   'café'.Contains('é') => True    'café'.Contains('É') => False
        let p = map(&[("P", "café")]);
        let (out, issues) = substitute("$(P.Contains('é'))", &p);
        assert_eq!(out, "True");
        assert!(issues.is_empty());
        let (out, issues) = substitute("$(P.Contains('É'))", &p);
        assert_eq!(out, "False");
        assert!(issues.is_empty());
    }

    #[test]
    fn starts_with_and_ends_with_bail_on_non_ascii() {
        // `String.StartsWith`/`EndsWith(String)` are culture-sensitive: MSBuild
        // evaluates `'abc'.StartsWith('\u{200b}abc')` (leading zero-width space)
        // to True because the ignorable character is collapsed, where an
        // ordinal prefix check is False. We cannot reproduce that collation, so
        // a non-ASCII operand stays Unsupported rather than risk a wrong gate.
        let zwsp_expr = "$(P.StartsWith('\u{200b}abc'))";
        let (out, issues) = substitute(zwsp_expr, &map(&[("P", "abc")]));
        assert_eq!(out, zwsp_expr, "non-ASCII StartsWith must stay literal");
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: zwsp_expr.to_string()
            }]
        );
        // A non-ASCII *receiver* is equally unreproducible.
        let expr = "$(P.EndsWith('c'))";
        let (out, issues) = substitute(expr, &map(&[("P", "\u{200b}abc")]));
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    #[test]
    fn empty_needle_starts_ends_with_commit_even_for_non_ascii_receiver() {
        // An empty needle is always a prefix/suffix regardless of collation,
        // so the non-ASCII bail must not fire. Pinned against dotnet msbuild
        // 10.0.300: `'café'.StartsWith('')` / `.EndsWith('')` => True.
        let p = map(&[("P", "café")]);
        let (out, issues) = substitute("$(P.StartsWith(''))", &p);
        assert_eq!(out, "True");
        assert!(issues.is_empty());
        let (out, issues) = substitute("$(P.EndsWith(''))", &p);
        assert_eq!(out, "True");
        assert!(issues.is_empty());
    }

    // --- static `[System.String]::IsNullOrEmpty(s)` ---
    //
    // A `bool`-returning static intrinsic, rendered "True"/"False". Every
    // expected value pinned against `dotnet msbuild` 10.0.301:
    //   [System.String]::IsNullOrEmpty('$(Empty)')  Empty=""   => True
    //   [System.String]::IsNullOrEmpty('$(Word)')   Word=x     => False
    //   [System.String]::IsNullOrEmpty('$(Undef)')             => True
    //   [System.String]::IsNullOrEmpty('$(Esc)')    Esc=%20    => False  (decodes to " ", length 1)
    //   [System.String]::IsNullOrEmpty('%20')                  => False
    // IsNullOrEmpty is *domain-insensitive*: a value is empty in the escaped
    // domain iff it is empty decoded (unescaping never maps a non-empty string
    // to empty), so the escaped-vs-decoded distinction that plagues
    // IsNullOrWhiteSpace does not arise here.

    #[test]
    fn string_is_null_or_empty_on_literal() {
        for (arg, expected) in [("''", "True"), ("'x'", "False"), ("' '", "False")] {
            let expr = format!("$([System.String]::IsNullOrEmpty({arg}))");
            let (out, issues) = substitute(&expr, &PropertyMap::new());
            assert_eq!(out, expected, "{expr}");
            assert!(issues.is_empty(), "{expr}: {issues:?}");
        }
    }

    #[test]
    fn string_is_null_or_empty_on_property() {
        let p = map(&[("Empty", ""), ("Word", "x")]);
        let (out, issues) = substitute("$([System.String]::IsNullOrEmpty('$(Empty)'))", &p);
        assert_eq!(out, "True");
        assert!(issues.is_empty());
        let (out, issues) = substitute("$([System.String]::IsNullOrEmpty('$(Word)'))", &p);
        assert_eq!(out, "False");
        assert!(issues.is_empty());
        // The unquoted single-expression argument spelling MSBuild also binds.
        let (out, issues) = substitute("$([System.String]::IsNullOrEmpty($(Word)))", &p);
        assert_eq!(out, "False");
        assert!(issues.is_empty());
    }

    #[test]
    fn string_is_null_or_empty_of_escaped_space_is_false() {
        // `%20` decodes to a single space — non-empty — so IsNullOrEmpty is
        // False (dotnet msbuild 10.0.301: `EscLen=1`, `IsNullOrEmpty=False`).
        let (out, issues) = substitute(
            "$([System.String]::IsNullOrEmpty('%20'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "False");
        assert!(issues.is_empty());
        let p = map(&[("Esc", "%20")]);
        let (out, issues) = substitute("$([System.String]::IsNullOrEmpty('$(Esc)'))", &p);
        assert_eq!(out, "False");
        assert!(issues.is_empty());
    }

    #[test]
    fn string_is_null_or_empty_of_undefined_is_true_and_reports() {
        // MSBuild treats an undefined property as empty, so IsNullOrEmpty is
        // True. The undefined read is still reported as an issue (whether the
        // condition layer exempts it as an is-it-set probe is a separate
        // concern decided there).
        let (out, issues) = substitute(
            "$([System.String]::IsNullOrEmpty('$(Undef)'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "True");
        assert_eq!(
            issues,
            vec![Issue::Undefined {
                name: "Undef".to_string()
            }]
        );
    }

    #[test]
    fn string_method_treats_markers_as_ordinary_substring() {
        // The reducer is a pure substring computation: item-list (`@(`) /
        // metadata (`%(`) markers — whether written raw in the argument or
        // delivered via `$()` substitution — are ordinary characters here,
        // matching MSBuild's string-method machinery. Condition-level marker
        // rejection is the condition scanner's job (see
        // `condition::evaluate_inner`'s raw-source scan), not this reducer's.
        // Raw marker in the receiver value and the needle literal:
        let (out, issues) = substitute("$(P.Contains('@('))", &map(&[("P", "a@(b")]));
        assert_eq!(out, "True");
        assert!(issues.is_empty());
        // Marker delivered only via substitution (pinned against dotnet
        // msbuild 10.0.300: as a condition this evaluates, doesn't reject).
        let (out, issues) =
            substitute("$(P.Contains('$(Q)'))", &map(&[("P", "a@(b"), ("Q", "@(")]));
        assert_eq!(out, "True");
        assert!(issues.is_empty());
    }

    #[test]
    fn string_method_ignores_method_name_inside_argument_literal() {
        // A needle that itself contains a `.Method(` substring must not be
        // mistaken for the call being parsed: `StartsWith('.Contains(')`
        // evaluates StartsWith, not a bogus Contains with an invalid receiver.
        let (out, issues) = substitute("$(P.StartsWith('.Contains('))", &map(&[("P", "x")]));
        assert_eq!(out, "False");
        assert!(issues.is_empty());
        let (out, issues) = substitute(
            "$(P.Contains('.EndsWith('))",
            &map(&[("P", "a.EndsWith(b")]),
        );
        assert_eq!(out, "True");
        assert!(issues.is_empty());
    }

    #[test]
    fn string_method_rejects_chained_call() {
        let expr = "$(P.Contains('a').ToString())";
        let (out, issues) = substitute(expr, &map(&[("P", "abc")]));
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    #[test]
    fn compact_net5_to_net9_target_framework_identifiers_are_coreapp() {
        for tfm in ["net5", "net50", "net9"] {
            assert_eq!(
                infer_target_framework_identifier(tfm),
                Some(".NETCoreApp"),
                "{tfm}"
            );
        }
    }

    #[test]
    fn dotted_net_framework_aliases_keep_netframework_identifier() {
        for tfm in ["net3.5", "net4.8"] {
            assert_eq!(
                infer_target_framework_identifier(tfm),
                Some(".NETFramework"),
                "{tfm}"
            );
        }
    }

    #[test]
    fn compact_prefixed_target_framework_versions_decode_component_digits() {
        for (tfm, expected) in [("netstandard20", "2.0"), ("netcoreapp30", "3.0")] {
            assert_eq!(
                infer_target_framework_version(tfm, 2),
                Some(expected.to_string())
            );
        }
    }

    #[test]
    fn compact_four_field_target_framework_versions_decode_component_digits() {
        assert_eq!(
            infer_target_framework_identifier("net5000"),
            Some(".NETCoreApp")
        );
        assert_eq!(
            infer_target_framework_version("net5000", 2),
            Some("5.0".to_string())
        );
        assert_eq!(
            infer_target_framework_identifier("netstandard1234"),
            Some(".NETStandard")
        );
        assert_eq!(
            infer_target_framework_version("netstandard1234", 4),
            Some("1.2.3.4".to_string())
        );
    }

    #[test]
    fn target_framework_versions_trim_redundant_zero_components_to_minimum() {
        for (tfm, expected) in [
            ("net460", "4.6"),
            ("net5.0.0", "5.0"),
            ("netcoreapp3.0.0", "3.0"),
        ] {
            assert_eq!(
                infer_target_framework_version(tfm, 2),
                Some(expected.to_string()),
                "{tfm}"
            );
        }
    }

    #[test]
    fn sdk_target_framework_inference_chain_is_supported() {
        let mut props = map(&[("TargetFramework", "netstandard2.0")]);

        let (id, issues) = substitute(
            "$([MSBuild]::GetTargetFrameworkIdentifier('$(TargetFramework)'))",
            &props,
        );
        assert_eq!(id, ".NETStandard");
        assert!(issues.is_empty());
        props.insert("TargetFrameworkIdentifier", id);

        let (version, issues) = substitute(
            "v$([MSBuild]::GetTargetFrameworkVersion('$(TargetFramework)', 2))",
            &props,
        );
        assert_eq!(version, "v2.0");
        assert!(issues.is_empty());
        props.insert("TargetFrameworkVersion", version);

        let (without_v, issues) = substitute("$(TargetFrameworkVersion.TrimStart('vV'))", &props);
        assert_eq!(without_v, "2.0");
        assert!(issues.is_empty());
    }

    #[test]
    fn an_escape_decodes_at_the_point_of_use() {
        // MSBuild unescapes `%XX` in an evaluated value, and now so do we: the
        // value is stored escaped and decoded once, at the leaf. This used to
        // withdraw the claim (raw text with no issue would have been a *wrong*
        // value); modelling it is strictly better, and costs no coverage.
        let (out, issues) = substitute("a%20b", &PropertyMap::new());
        assert_eq!(out, "a b");
        assert!(issues.is_empty(), "{issues:?}");

        // An escape composed *across a splice boundary* decodes too, because
        // composition happens inside the domain and the leaf decodes what it
        // finds. With `<Alpha>100%</Alpha>`, `$(Alpha)$(Alpha)` is `100%100%`,
        // in which MSBuild reads `%10` — the walker differential found this,
        // and `dotnet msbuild` gives `100\u00100%` (pinned 2026-07-12).
        let props = map(&[("Alpha", "100%")]);
        let (out, issues) = substitute("$(Alpha)$(Alpha)", &props);
        assert_eq!(out, "100\u{10}0%");
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn escape_from_a_trusted_path_property_is_literal() {
        // A `%XX` in a *path-derived* reserved value is not an escape: MSBuild
        // stores reserved values pre-escaped, so its single unescape pass hands
        // them back unchanged. Probed against `dotnet msbuild` with a project
        // really living in `…/a%20b/`: `$(MSBuildProjectDirectory)` keeps the
        // literal `%20`, and `<Compile Include="$(MSBuildProjectDirectory)/Foo.fs"/>`
        // resolves to `…/a%20b/Foo.fs`. Degrading there would drop a good item.
        let props = well_known(Path::new("/repo/a%20b/Demo.fsproj"));
        let (out, issues) = substitute("$(MSBuildProjectDirectory)/Foo.fs", &props);
        assert_eq!(out, "/repo/a%20b/Foo.fs");
        assert!(
            issues.is_empty(),
            "a path-derived percent sequence is literal: {issues:?}"
        );

        // Provenance follows the **percent**, not the whole sequence: a trusted
        // `%` cannot introduce an escape even when the XML supplies the two hex
        // digits after it. In `…/pct%/`, `$(MSBuildProjectDirectory)20b` is the
        // literal `…/pct%20b` (probed against `dotnet msbuild`). My first
        // attempt pinned this the other way round; codex caught it.
        let props = well_known(Path::new("/repo/pct%/Demo.fsproj"));
        let (out, issues) = substitute("$(MSBuildProjectDirectory)20b", &props);
        assert_eq!(out, "/repo/pct%20b");
        assert!(
            issues.is_empty(),
            "a trusted percent cannot introduce an escape: {issues:?}"
        );

        // Trust survives being laundered through an ordinary property write:
        // MSBuild keeps values escaped internally, so the provenance rides along
        // (`<Base>$(MSBuildProjectDirectory)</Base>` then `$(Base)/Foo.fs`).
        let mut props = well_known(Path::new("/repo/a%20b/Demo.fsproj"));
        let (base, issues) = super::substitute("$(MSBuildProjectDirectory)", &props);
        assert!(issues.is_empty());
        props.insert_escaped("Base", base);
        let (out, issues) = substitute("$(Base)/Foo.fs", &props);
        assert_eq!(out, "/repo/a%20b/Foo.fs");
        assert!(issues.is_empty(), "trust must survive a write: {issues:?}");

        // A write of *XML text* is a different thing entirely: there the `%20`
        // is an escape, and MSBuild's evaluated value is the space it decodes
        // to. We used to withdraw the claim here (raw text with no issue would
        // have been a wrong value); now the domain models it, so the value is
        // simply correct.
        let mut props = well_known(Path::new("/repo/proj/Demo.fsproj"));
        props.insert("Authored", "a%20b");
        let (out, issues) = substitute("$(Authored)", &props);
        assert_eq!(out, "a b", "an XML-authored escape decodes at the leaf");
        assert!(issues.is_empty(), "and no longer degrades: {issues:?}");
    }

    #[test]
    fn expression_result_escapedness_decides_percent_inertness() {
        // MSBuild escapes the *string* a property function returns, so its
        // percents are inert; it hands a string indexer's `Char` back **raw**,
        // so that percent can still compose an escape with what follows. All
        // pinned against `dotnet msbuild` 10.0.301 with `Pct=100%`:
        //
        //   $(Pct)20b                 -> "100 b"    plain splice: composes
        //   $(Pct.ToString())20b      -> "100%20b"  function result: inert
        //   $(Pct.Substring(3))20b    -> "%20b"     function result: inert
        //   $(Pct.Split('z')[0])20b   -> "100%20b"  function result: inert
        //   $(Pct[3])20b              -> " b"       Char: RAW, composes
        //
        // A blanket "expression results are inert" rule commits `%20b` for the
        // indexer — a wrong value, not a safe decline. Hence `Evaluated.escaped`.
        let mut props = PropertyMap::new();
        props.insert("Pct", "100%");

        // (`Substring` is not in our dispatch table, so it declines for an
        // unrelated reason; the escape rule is exercised on the members we do
        // model.)
        for inner in ["$(Pct.ToString())20b", "$(Pct.Split('z')[0])20b"] {
            let (out, issues) = substitute(inner, &props);
            assert!(
                issues.is_empty(),
                "{inner}: a function result's percent is inert: {issues:?}"
            );
            assert_eq!(out, "100%20b", "{inner}");
        }

        // The indexer's `Char` is raw, so it *does* compose an escape — and the
        // leaf decodes it: `%` + `20b` is a space then `b` (oracle-pinned).
        let (out, issues) = substitute("$(Pct[3])20b", &props);
        assert_eq!(out, " b", "an indexer's Char percent composes an escape");
        assert!(issues.is_empty(), "{issues:?}");

        // …as does a plain splice: `100%` + `20b` decodes to `100 b`
        // (`dotnet msbuild`, 2026-07-12).
        let (out, issues) = substitute("$(Pct)20b", &props);
        assert_eq!(out, "100 b");
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn an_escape_inside_an_unsupported_expression_reports_once() {
        // The slow path already raised `Unsupported` (and copied the expression
        // into the output), which withdraws the claim. A second issue for the
        // same span would double the user-visible diagnostic.
        let (_, issues) = substitute("$([Nope]::Thing('%20'))", &PropertyMap::new());
        assert_eq!(
            issues
                .iter()
                .filter(|i| matches!(i, Issue::Unsupported { .. }))
                .count(),
            1,
            "{issues:?}"
        );
    }

    #[test]
    fn unbalanced_dollar_paren_passes_through_with_an_issue() {
        // The characters are preserved, but the claim is withdrawn: MSBuild
        // treats a genuinely unbalanced `$(` as literal text, yet its
        // scanner can find a close (and then error) on quote nestings ours
        // gives up on, so a no-issue passthrough would be a wrong commit.
        let (out, issues) = substitute("a$(unclosed and more", &PropertyMap::new());
        assert_eq!(out, "a$(unclosed and more");
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: "$(unclosed and more".to_string()
            }]
        );
    }

    #[test]
    fn well_known_includes_path_derivatives() {
        let m = well_known(std::path::Path::new("/repo/proj/Demo.fsproj"));
        let get = |k: &str| m.get_unescaped(k);
        assert_eq!(get("MSBuildProjectName").as_deref(), Some("Demo"));
        assert_eq!(get("MSBuildProjectExtension").as_deref(), Some(".fsproj"));
        assert_eq!(
            get("MSBuildProjectFullPath").as_deref(),
            Some("/repo/proj/Demo.fsproj")
        );
        assert_eq!(
            get("MSBuildProjectDirectory").as_deref(),
            Some("/repo/proj")
        );
        // MSBuildThisFileDirectory keeps the trailing separator —
        // matches the documented MSBuild distinction.
        assert_eq!(
            get("MSBuildThisFileDirectory").as_deref(),
            Some("/repo/proj/")
        );
        assert_eq!(get("MSBuildThisFile").as_deref(), Some("Demo.fsproj"));
        // MSBuildProjectFile is the project-scoped alias for the same
        // value — both names must resolve to the project filename.
        assert_eq!(get("MSBuildProjectFile").as_deref(), Some("Demo.fsproj"));
    }

    /// A computed seed enters the domain **escaped**, which is what makes a
    /// reserved character in the project's own path inert. The nine-character
    /// set is MSBuild's (`EscapingUtilities.cs:310`), and the `;` case is the
    /// one that used to be a live wrong commit: a project in a directory named
    /// `a;b` yields *one* Compile item with a literal semicolon, not two
    /// (oracle-pinned 2026-07-12).
    #[test]
    fn a_reserved_character_in_the_project_path_is_inert() {
        let m = well_known(std::path::Path::new("/repo/a;b (x)/Demo.fsproj"));
        let dir = m.get("MSBuildProjectDirectory").expect("seeded");
        assert_eq!(dir.as_escaped(), "/repo/a%3bb %28x%29");
        assert_eq!(dir.unescape(), "/repo/a;b (x)");
        // The `;` cannot split an item list, and the parens cannot open an
        // expression, because neither survives into the escaped text as itself.
        assert_eq!(dir.as_escaped().split(';').count(), 1);
        assert!(!dir.as_escaped().contains('('));
    }

    // --- $([MSBuild]::GetDirectoryNameOfFileAbove(...)) ---
    //
    // These search the filesystem, so each test builds a real tempdir tree.
    // The expected shapes come from MSBuild's documented behaviour (and the
    // `sdk_style_cpm_probe_properties_match_msbuild` differential fixture
    // pins them against a real `dotnet msbuild`).

    fn dir_above_expr(start: &str, file: &str) -> String {
        format!("$([MSBuild]::GetDirectoryNameOfFileAbove('{start}', '{file}'))")
    }

    #[test]
    fn get_directory_name_of_file_above_finds_in_start_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let start = tmp.path().join("a/b");
        std::fs::create_dir_all(&start).unwrap();
        std::fs::write(start.join("marker.props"), "x").unwrap();
        let (out, issues) = substitute_with_fs(
            &dir_above_expr(&start.to_string_lossy(), "marker.props"),
            &PropertyMap::new(),
        );
        assert_eq!(out, start.to_string_lossy());
        assert!(issues.is_empty());
    }

    #[test]
    fn get_directory_name_of_file_above_walks_up() {
        let tmp = tempfile::TempDir::new().unwrap();
        let start = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&start).unwrap();
        std::fs::write(tmp.path().join("a").join("marker.props"), "x").unwrap();
        let (out, issues) = substitute_with_fs(
            &dir_above_expr(&start.to_string_lossy(), "marker.props"),
            &PropertyMap::new(),
        );
        assert_eq!(out, tmp.path().join("a").to_string_lossy());
        assert!(issues.is_empty());
    }

    #[test]
    fn get_directory_name_of_file_above_not_found_is_empty_without_issue() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (out, issues) = substitute_with_fs(
            &dir_above_expr(&tmp.path().to_string_lossy(), "no-such-file.props"),
            &PropertyMap::new(),
        );
        assert_eq!(out, "", "MSBuild returns empty when nothing is found");
        assert!(issues.is_empty());
    }

    #[test]
    fn get_directory_name_of_file_above_expands_property_args() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Directory.Packages.props"), "x").unwrap();
        let props = map(&[
            ("MSBuildProjectDirectory", &*tmp.path().to_string_lossy()),
            ("_File", "Directory.Packages.props"),
        ]);
        let (out, issues) = substitute_with_fs(
            "$([MSBuild]::GetDirectoryNameOfFileAbove('$(MSBuildProjectDirectory)', '$(_File)'))",
            &props,
        );
        assert_eq!(out, tmp.path().to_string_lossy());
        assert!(issues.is_empty());
    }

    #[test]
    fn get_directory_name_of_file_above_is_unsupported_without_fs() {
        // The plain `substitute` is the pure-parse surface (`parse_fsproj`
        // documents "no filesystem access"), so the probing function must
        // not evaluate there even when the file exists.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("marker.props"), "x").unwrap();
        let expr = dir_above_expr(&tmp.path().to_string_lossy(), "marker.props");
        let (out, issues) = substitute(&expr, &PropertyMap::new());
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.clone()
            }]
        );
    }

    #[test]
    fn get_directory_name_of_file_above_undefined_arg_stays_unsupported() {
        // Searching from a directory we guessed (an undefined property
        // substitutes to "") could return a confidently wrong answer;
        // the whole expression must stay visibly unsupported instead.
        let expr = "$([MSBuild]::GetDirectoryNameOfFileAbove('$(Missing)', 'f.props'))";
        let (out, issues) = substitute_with_fs(expr, &PropertyMap::new());
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    #[test]
    fn get_directory_name_of_file_above_relative_start_stays_unsupported() {
        // MSBuild would resolve a relative start against the process
        // working directory, which this evaluator does not receive.
        let expr = "$([MSBuild]::GetDirectoryNameOfFileAbove('relative/dir', 'f.props'))";
        let (out, issues) = substitute_with_fs(expr, &PropertyMap::new());
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    // --- $([MSBuild]::NormalizePath(...)) ---

    #[test]
    fn normalize_path_joins_two_absolute_and_relative() {
        let (out, issues) = substitute(
            "$([MSBuild]::NormalizePath('/a/b', 'file.props'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "/a/b/file.props");
        assert!(issues.is_empty());
    }

    #[test]
    fn normalize_path_resolves_dot_and_dotdot() {
        let (out, issues) = substitute(
            "$([MSBuild]::NormalizePath('/a/b', '..', './c', 'f.txt'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "/a/c/f.txt");
        assert!(issues.is_empty());
    }

    #[test]
    fn normalize_path_excess_dotdot_stops_at_root() {
        // .NET Path.GetFullPath drops `..` segments that would climb
        // past the root.
        let (out, issues) = substitute(
            "$([MSBuild]::NormalizePath('/a', '../../../b'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "/b");
        assert!(issues.is_empty());
    }

    #[test]
    fn normalize_path_later_absolute_arg_resets() {
        // Path.Combine semantics: an absolute later argument discards
        // everything before it.
        let (out, issues) = substitute(
            "$([MSBuild]::NormalizePath('/a/b', '/x/y', 'f.txt'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "/x/y/f.txt");
        assert!(issues.is_empty());
    }

    #[test]
    fn normalize_path_normalises_backslashes() {
        let (out, issues) = substitute(
            "$([MSBuild]::NormalizePath('/a\\b', 'c\\f.txt'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "/a/b/c/f.txt");
        assert!(issues.is_empty());
    }

    #[test]
    fn normalize_path_expands_property_args() {
        let props = map(&[("Base", "/central"), ("File", "Directory.Packages.props")]);
        let (out, issues) = substitute("$([MSBuild]::NormalizePath('$(Base)', '$(File)'))", &props);
        assert_eq!(out, "/central/Directory.Packages.props");
        assert!(issues.is_empty());
    }

    #[test]
    fn normalize_path_relative_result_stays_unsupported() {
        // A relative result would be resolved against the MSBuild
        // process working directory, which this evaluator does not
        // receive — stay visibly unsupported.
        let expr = "$([MSBuild]::NormalizePath('rel/a', 'b.txt'))";
        let (out, issues) = substitute(expr, &PropertyMap::new());
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    #[test]
    fn normalize_path_undefined_arg_stays_unsupported() {
        let expr = "$([MSBuild]::NormalizePath('$(Missing)', 'b.txt'))";
        let (out, issues) = substitute(expr, &PropertyMap::new());
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    #[test]
    fn get_directory_name_of_file_above_accepts_unquoted_property_arg() {
        // Microsoft.Common.props passes the first argument unquoted:
        // $([MSBuild]::GetDirectoryNameOfFileAbove($(MSBuildProjectDirectory), '$(_DirectoryBuildPropsFile)'))
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Directory.Build.props"), "x").unwrap();
        let props = map(&[
            ("MSBuildProjectDirectory", &*tmp.path().to_string_lossy()),
            ("_File", "Directory.Build.props"),
        ]);
        let (out, issues) = substitute_with_fs(
            "$([MSBuild]::GetDirectoryNameOfFileAbove($(MSBuildProjectDirectory), '$(_File)'))",
            &props,
        );
        assert_eq!(out, tmp.path().to_string_lossy());
        assert!(issues.is_empty());
    }

    #[test]
    fn combine_joins_and_does_not_normalise() {
        // Path.Combine joins but never resolves `..`.
        let (out, issues) = substitute(
            "$([System.IO.Path]::Combine('/a/b', '../c.txt'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "/a/b/../c.txt");
        assert!(issues.is_empty());
    }

    #[test]
    fn combine_later_absolute_arg_resets() {
        let (out, issues) = substitute(
            "$([System.IO.Path]::Combine('/a', '/x/y', 'f.txt'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "/x/y/f.txt");
        assert!(issues.is_empty());
    }

    #[test]
    fn combine_relative_result_is_allowed() {
        // Combine does not resolve, so a relative result is exact.
        let (out, issues) = substitute(
            "$([System.IO.Path]::Combine('rel', 'f.txt'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "rel/f.txt");
        assert!(issues.is_empty());
    }

    #[test]
    fn combine_expands_property_args() {
        let props = map(&[("Base", "/central"), ("File", "Directory.Build.props")]);
        let (out, issues) =
            substitute("$([System.IO.Path]::Combine('$(Base)', '$(File)'))", &props);
        assert_eq!(out, "/central/Directory.Build.props");
        assert!(issues.is_empty());
    }

    #[test]
    fn combine_undefined_arg_stays_unsupported() {
        let expr = "$([System.IO.Path]::Combine('$(Missing)', 'f.txt'))";
        let (out, issues) = substitute(expr, &PropertyMap::new());
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    #[test]
    #[cfg(windows)]
    fn normalize_path_preserves_unc_roots_on_windows_hosts() {
        // `\\server\share` (normalised `//server/share`) is a root on
        // Windows: `..` cannot climb above the share, and the double
        // slash must survive normalisation.
        let (out, issues) = substitute(
            "$([MSBuild]::NormalizePath('//server/share/repo', '..', 'f.txt'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "//server/share/f.txt");
        assert!(issues.is_empty());
        let (out, issues) = substitute(
            "$([MSBuild]::NormalizePath('//server/share', '../../..', 'z'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "//server/share/z");
        assert!(issues.is_empty());
    }

    #[test]
    #[cfg(not(windows))]
    fn normalize_path_double_slash_collapses_on_unix_hosts() {
        // Unix .NET has no UNC notion: GetFullPath collapses the double
        // slash and `..` climbs freely (verified against `dotnet
        // msbuild` on this host).
        let (out, issues) = substitute(
            "$([MSBuild]::NormalizePath('//server/share', '../../z'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "/z");
        assert!(issues.is_empty());
    }

    #[test]
    #[cfg(not(windows))]
    fn normalize_path_windows_drive_stays_unsupported_on_unix_hosts() {
        // A Windows drive path can't be resolved faithfully on a
        // non-Windows host; stay visibly unsupported rather than
        // producing a confidently wrong unix-shaped path.
        let expr = "$([MSBuild]::NormalizePath('C:\\a', 'b.txt'))";
        let (out, issues) = substitute(expr, &PropertyMap::new());
        assert_eq!(out, expr);
        assert_eq!(
            issues,
            vec![Issue::Unsupported {
                expression: expr.to_string()
            }]
        );
    }

    #[test]
    #[cfg(windows)]
    fn normalize_path_windows_drive_resolves_on_windows_hosts() {
        // A Windows host owns drive semantics — `$(MSBuildProjectDirectory)`
        // is drive-qualified there, so refusing would break the whole
        // `Directory.Packages.props` discovery chain.
        let (out, issues) = substitute(
            "$([MSBuild]::NormalizePath('C:\\a\\b', '..', 'f.txt'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "C:/a/f.txt");
        assert!(issues.is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn combine_windows_drive_later_arg_resets_on_windows_hosts() {
        let (out, issues) = substitute(
            "$([System.IO.Path]::Combine('C:\\a', 'D:\\x', 'f.txt'))",
            &PropertyMap::new(),
        );
        assert_eq!(out, "D:/x/f.txt");
        assert!(issues.is_empty());
    }
}
