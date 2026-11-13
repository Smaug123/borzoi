//! Pure MSBuild-style glob matching for `.fsproj` item includes.
//!
//! This is the policy half of the glob *seam* (`GlobResolver`): the
//! evaluator never globs, it hands each globbing item element to a
//! caller-supplied resolver. This module is the *pure* core of the
//! resolver this crate ships ([`crate::glob_resolver`]) — pattern parsing
//! and matching against relative paths, plus a deterministic selection
//! over a candidate set. The filesystem enumeration that produces those
//! candidates lives in `glob_resolver`.
//!
//! ## Semantics modelled
//!
//! - `*` matches zero or more characters *within a single path segment*
//!   (never crosses `/`).
//! - `?` matches exactly one character within a segment.
//! - `**`, as a *whole* segment, matches zero or more whole path
//!   segments (recursive). `**` embedded in a larger segment (e.g.
//!   `a**b`) parses as two `*` wildcards, but MSBuild's file matcher
//!   refuses such a filespec ([`is_legal_filespec`]), so the resolver
//!   declines before matching one.
//! - A filename part of `*.*` matches every file where MSBuild's matcher
//!   reads it so ([`Pattern::with_all_files_rule`]).
//! - Matching is **case-sensitive** and paths are compared with `/`
//!   separators (backslashes normalise to `/`, runs of `/` collapse, and
//!   lone `.` current-directory segments are dropped). `..` is left for
//!   the filesystem layer to resolve against the base directory.
//!   MSBuild's own case behaviour depends on the host, so [`select`]
//!   **declines** ([`GlobDecline::CaseAmbiguous`]) whenever folding ASCII
//!   case would change what a fragment selects: the answer is then the
//!   same under either rule, or there is no answer.
//!
//! [`select`] is an information-preserving primitive: it orders but does
//! not deduplicate. Across Include fragments it keeps document order; and
//! *within* one fragment's expansion it sorts exactly as MSBuild does —
//! `EngineFileUtilities.GetFileList` sorts each wildcard fragment's matches
//! with `StringComparer.OrdinalIgnoreCase` before returning them, so the
//! order is a function of the names, not of the filesystem. Where that sort
//! cannot be reproduced exactly it declines: two matches equal under it
//! (only possible on a case-sensitive filesystem) leave the order to .NET's
//! unstable `Array.Sort`, and a non-ASCII name would need .NET's per-UTF-16
//! simple case mapping, which this module does not model.
//!
//! ## Testing
//!
//! The matcher is property-tested against an independent naive reference
//! oracle (`matches_naive`). That proves the two agree, catching
//! implementation bugs — but *not* MSBuild-faithfulness (both could share
//! a wrong assumption). MSBuild-faithfulness is a separate concern pinned
//! by the 9b-2 `dotnet msbuild` oracle.

use crate::properties::escaping::{self, Escaped};

/// Why a fragment's expansion cannot be reproduced exactly, so the resolver
/// declines rather than commit a list MSBuild may not produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobDecline {
    /// Two matches of one fragment compare equal under `OrdinalIgnoreCase`
    /// (`A.fs` and `a.fs` on a case-sensitive filesystem). MSBuild orders
    /// them with an unstable sort, so no order is *the* order.
    OrderTie { first: String, second: String },
    /// A fragment matched several files and one name is not ASCII. Its
    /// position under `OrdinalIgnoreCase` depends on .NET's per-UTF-16 simple
    /// case mapping, which is not modelled.
    NonAsciiOrder { path: String },
    /// Folding ASCII case changes whether `path` is selected — by the include
    /// or by an exclude. MSBuild's matching is case-insensitive on some hosts
    /// and not on others, so the selection is not knowable here.
    CaseAmbiguous { path: String },
    /// A recursive walk reached [`crate::glob_resolver`]'s depth bound with
    /// directories still below it, so matches may be missing.
    DepthLimit { dir: String },
    /// The walk met a symbolic link back to a directory already on its path.
    /// What MSBuild does with a cycle is not modelled.
    SymlinkCycle { path: String },
    /// MSBuild's file matcher refuses the decoded filespec (see
    /// `FileMatcher.IsLegalFileSpec`): `a**b.fs`, a `..` after a wildcard, a `...`.
    /// It then falls back to a different reading per caller, which is not
    /// modelled.
    IllegalFilespec { pattern: String },
    /// A glob include meets excludes, and the include or an exclude has a
    /// `.` segment. MSBuild's file matcher relates them by spelling, so
    /// `Include="*.fs" Exclude="./a.fs"` excludes nothing, nor does
    /// `Include="./*.fs" Exclude="a.fs"`; the matcher here drops a `.`
    /// segment, so it declines instead. (A `..` segment it keeps, and compares
    /// by spelling as MSBuild does.)
    CurDirSegmentExclude { pattern: String },
    /// A spec, or a name a glob would match, is outside the shapes the
    /// resolver models ([`Unmodelled`] says which). The resolver commits only
    /// inside an allow-list the MSBuild differentials verify, so a shape
    /// nobody has checked declines here instead of matching the plain way.
    OutsideModelledShape { spec: String, shape: Unmodelled },
}

impl std::fmt::Display for GlobDecline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GlobDecline::OrderTie { first, second } => write!(
                f,
                "`{first}` and `{second}` differ only in case, so MSBuild's order between them is unspecified"
            ),
            GlobDecline::NonAsciiOrder { path } => write!(
                f,
                "`{path}` has a non-ASCII name, whose position in MSBuild's case-insensitive order is not modelled"
            ),
            GlobDecline::CaseAmbiguous { path } => write!(
                f,
                "whether `{path}` is selected depends on whether matching ignores case"
            ),
            GlobDecline::DepthLimit { dir } => {
                write!(
                    f,
                    "the recursive walk stopped at its depth bound in `{dir}`"
                )
            }
            GlobDecline::SymlinkCycle { path } => {
                write!(f, "the symbolic link `{path}` closes a directory cycle")
            }
            GlobDecline::IllegalFilespec { pattern } => write!(
                f,
                "MSBuild's file matcher refuses the pattern `{pattern}`, and its fallback is not modelled"
            ),
            GlobDecline::CurDirSegmentExclude { pattern } => write!(
                f,
                "`{pattern}` has a `.` segment, which MSBuild's file matcher compares by spelling against excludes"
            ),
            GlobDecline::OutsideModelledShape { spec, shape } => write!(
                f,
                "`{spec}` {shape}, which the glob resolver does not model"
            ),
        }
    }
}

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

/// One `;`-separated fragment of an item `Include` or `Exclude`, still in
/// MSBuild's **escaped** domain — the unit [`crate::GlobRequest`] carries
/// across the resolver seam.
///
/// MSBuild splits a spec on the `;`s of its escaped text, classifies each
/// fragment as a glob or a literal on that escaped text, and only then
/// decodes it. A fragment is therefore the product of the split: it never
/// holds a live `;`, so nothing downstream has a reason to split it again,
/// and its classification ([`Self::is_wildcard`]) is asked of the escaped
/// text this type keeps. Decoding is the last step, at [`Self::unescape`].
/// An escaped `%3b` decodes to a `;` that is part of a filename, and an
/// escaped `%2a` to a star that is part of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecFragment(Escaped);

impl SpecFragment {
    /// Split an escaped item spec into its fragments as MSBuild does: on
    /// every `;` of the escaped text, each fragment trimmed, empty ones
    /// dropped. `a%3bb.fs;z.fs` is two fragments, the first naming `a;b.fs`.
    pub fn split_escaped(spec: &str) -> Vec<SpecFragment> {
        Escaped::from_xml(spec)
            .split_list()
            .map(SpecFragment)
            .collect()
    }

    /// One fragment the evaluator has already split off an escaped spec
    /// (trimmed, non-empty, no live `;`).
    pub(crate) fn from_split(fragment: &str) -> SpecFragment {
        debug_assert!(!fragment.contains(';') && fragment.trim() == fragment);
        SpecFragment(Escaped::from_xml(fragment))
    }

    /// The fragment's escaped spelling, for display.
    pub fn as_escaped(&self) -> &str {
        self.0.as_escaped()
    }

    /// The fragment at its point of use: decoded, exactly once.
    pub fn unescape(&self) -> String {
        self.0.unescape()
    }

    /// Whether MSBuild expands this fragment as a glob
    /// (`EngineFileUtilities.FilespecHasWildcards`): a live `*`/`?` and no
    /// escaped one. When true, every `*`/`?` in [`Self::unescape`] is a
    /// wildcard; when false, the decoded text is a literal path, stars and all
    /// (`a%2a*.fs` is the one item `a**.fs`).
    pub fn is_wildcard(&self) -> bool {
        self.0.is_filespec_wildcard()
    }

    /// Whether this `Exclude` fragment takes part in excluding from a
    /// **glob** include — MSBuild's `EngineFileUtilities.IsValidExclude`,
    /// which drops an exclude carrying both a live and an escaped wildcard
    /// before the file matcher sees any. The ones it keeps are decoded and
    /// then parsed as patterns, so an escaped `%2a` in one *is* a wildcard
    /// there: `Include="*.fs" Exclude="a%2ab.fs"` excludes `axb.fs`, while
    /// against the literal include `axb.fs` the same exclude matches only a
    /// file named `a*b.fs` (both pinned against `dotnet msbuild` 10.0.301).
    pub fn excludes_from_globs(&self) -> bool {
        !(escaping::has_live_wildcard(self.0.as_escaped())
            && escaping::contains_escaped_wildcards(self.0.as_escaped()))
    }
}

/// Which allow-list rule a spec or name breaks
/// ([`GlobDecline::OutsideModelledShape`]). Each names MSBuild behaviour the
/// resolver's plain matching would get wrong, found by review or by the
/// generative differential: none of it is modelled, so all of it declines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unmodelled {
    /// A character outside printable ASCII. MSBuild's `?` matches one UTF-16
    /// code unit, not one character, and its `OrdinalIgnoreCase` folds
    /// non-ASCII letters onto ASCII ones (`ı` onto `I`).
    NonAscii,
    /// A trailing path separator: MSBuild keeps the empty filename part, so
    /// the spec names a directory, not a file.
    TrailingSeparator,
    /// A segment ending in `.` or a space (other than `.` and `..`), which
    /// MSBuild's Windows-derived filename rules treat specially (`*.fs.`).
    TrailingDotOrSpace,
    /// One of `"<>|:`, which Windows forbids in a filename and MSBuild's
    /// matchers treat specially (as wildcards, or as a drive separator).
    WindowsReservedCharacter,
    /// A glob include with a `..` segment meets an exclude with a wildcard
    /// directory part. MSBuild applies that part only to the directories it
    /// walks, not to the include's own `..` prefix, so the SDK's `**/.*/**`
    /// leaves `../shared/*.fs` alone; matching the plain way, `.*` would take
    /// the `..`.
    ParentSegmentUnderWildcardExclude,
    /// A glob exclude with a wildcard directory part, anchored differently
    /// from the glob include it filters (one absolute, the other relative).
    MixedAnchoring,
    /// A glob exclude names a directory after a `**` in its wildcard part, and
    /// the glob include it filters starts its walk below the exclude's base,
    /// inside a directory that name could match. MSBuild applies that part
    /// only to directories its walk enters, so `**/Deep/**` leaves
    /// `sub/Deep/*.fs` alone; matched by full path, it takes the file (#615).
    GlobstarExcludeAboveIncludeRoot,
}

impl std::fmt::Display for Unmodelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Unmodelled::NonAscii => "has a character outside printable ASCII",
            Unmodelled::TrailingSeparator => "ends in a path separator",
            Unmodelled::TrailingDotOrSpace => "has a segment ending in `.` or a space",
            Unmodelled::WindowsReservedCharacter => "has one of `\"<>|:`",
            Unmodelled::ParentSegmentUnderWildcardExclude => {
                "has a `..` segment and meets an exclude with a wildcard directory part"
            }
            Unmodelled::MixedAnchoring => {
                "has a wildcard directory part and is anchored unlike the include it filters"
            }
            Unmodelled::GlobstarExcludeAboveIncludeRoot => {
                "names a directory after `**` that the include's walk root lies inside"
            }
        })
    }
}

/// The allow-list: whether a **decoded** spec, or a name a glob would match,
/// is in the shape the resolver models, and if not, which rule it breaks.
/// Printable ASCII; no trailing separator, and no segment ending in `.` or a
/// space (`.` and `..` themselves are segments, not endings); none of
/// `"<>|:`. Runs of separators collapse, as MSBuild's do (`bin\/**`, the
/// SDK's own spelling of an output exclude, is `bin/**`). A `*` or `?` is allowed: in a glob it is a wildcard, and in a
/// literal name (decoded from `%2a`) MSBuild compares it as written
/// ([`compares_unnormalised`]).
pub fn unmodelled_shape(spec: &str) -> Option<Unmodelled> {
    if !spec.chars().all(|c| (' '..='~').contains(&c)) {
        return Some(Unmodelled::NonAscii);
    }
    if spec.contains(['"', '<', '>', '|', ':']) {
        return Some(Unmodelled::WindowsReservedCharacter);
    }
    if spec.ends_with(['/', '\\']) {
        return Some(Unmodelled::TrailingSeparator);
    }
    for seg in spec.split(['/', '\\']) {
        if seg != "." && seg != ".." && seg.ends_with(['.', ' ']) {
            return Some(Unmodelled::TrailingDotOrSpace);
        }
    }
    None
}

/// Whether a decoded glob has a wildcard directory part (`**/a.fs`,
/// `bin/**`): what [`Unmodelled::MixedAnchoring`] asks of an exclude.
pub fn has_wildcard_directory_part(spec: &str) -> bool {
    !split_filespec(spec).1.is_empty()
}

/// Characters that make MSBuild reject a filespec outright
/// (`FileUtilities.InvalidPathChars`): `|`, NUL and the C0 controls.
fn is_invalid_path_char(c: char) -> bool {
    c == '|' || c < ' '
}

/// MSBuild's `FileMatcher.SplitFileSpec`: the fixed directory (up to the
/// last separator before the first wildcard), the wildcard directory part
/// (from there to the last separator), and the filename part. A filename
/// part of `**` moves into the directory part and leaves `*.*` behind, as
/// MSBuild's does.
pub(crate) fn split_filespec(spec: &str) -> (&str, String, String) {
    let is_sep = |c: char| c == '/' || c == '\\';
    let (dirs, filename) = match spec.rfind(is_sep) {
        Some(last_sep) => (&spec[..=last_sep], &spec[last_sep + 1..]),
        None => ("", spec),
    };
    let (fixed, mut wildcard_dir) = match dirs.find(['*', '?']) {
        None => (dirs, String::new()),
        Some(first_wild) => {
            let cut = dirs[..first_wild].rfind(is_sep).map_or(0, |i| i + 1);
            (&dirs[..cut], dirs[cut..].to_string())
        }
    };
    let mut filename = filename.to_string();
    if filename == "**" {
        wildcard_dir.push_str("**/");
        filename = "*.*".to_string();
    }
    (fixed, wildcard_dir, filename)
}

/// `FileMatcher.HasMisplacedRecursiveOperator`: a `**` that is not a whole
/// path segment.
fn has_misplaced_recursive_operator(part: &str) -> bool {
    let b = part.as_bytes();
    let is_slash = |c: u8| c == b'/' || c == b'\\';
    (0..b.len().saturating_sub(1)).any(|i| {
        b[i] == b'*'
            && b[i + 1] == b'*'
            && !((i == 0 || is_slash(b[i - 1])) && i + 2 < b.len() && is_slash(b[i + 2]))
    })
}

/// Whether MSBuild's file matcher accepts a **decoded** glob filespec at all:
/// `FileMatcher.RawFileSpecIsValid` and `IsLegalFileSpec`. It refuses a
/// filespec holding an invalid path character, a `...`, or a `:` anywhere but
/// the second character; and one whose wildcard directory part holds `..`, or
/// whose `**` is not a whole path segment (`a**b.fs`). What MSBuild then does
/// differs by caller — an illegal include is kept as a literal item, an
/// illegal exclude falls back to a literal comparison — so the resolver
/// declines rather than reproduce each fallback.
pub fn is_legal_filespec(spec: &str) -> bool {
    if spec.chars().any(is_invalid_path_char) || spec.contains("...") {
        return false;
    }
    if spec.rfind(':').is_some_and(|colon| colon != 1) {
        return false;
    }
    let (_, wildcard_dir, filename) = split_filespec(spec);
    !wildcard_dir.contains("..")
        && !has_misplaced_recursive_operator(&wildcard_dir)
        && !has_misplaced_recursive_operator(&filename)
}

/// Whether a **literal** spelling is one MSBuild compares without normalising
/// it (`FileUtilities.PathIsInvalid`, consulted by
/// `NormalizePathForComparisonNoThrow`): one holding an invalid path
/// character, or whose filename part holds a character Windows forbids in a
/// filename (`"<>|:*?`, NUL, the C0 controls). Such a spelling is compared as
/// written, so `./a*b.fs`, `a*b.fs` and its absolute spelling are three
/// different items to an `Exclude`, while `./ab.fs` and `ab.fs` are one.
pub fn compares_unnormalised(spelling: &str) -> bool {
    if spelling.chars().any(is_invalid_path_char) {
        return true;
    }
    let filename = spelling.rsplit(['/', '\\']).next().unwrap_or(spelling);
    filename
        .chars()
        .any(|c| matches!(c, '"' | '<' | '>' | '|' | ':' | '*' | '?') || c < ' ')
}

/// Whether a decoded glob filespec has the shape MSBuild's
/// `FileSpecMatcherTester` matches by **filename alone**: an empty fixed
/// directory and a wildcard directory part of exactly `**`
/// (`**/*.fs`). That branch uses the plain filename glob, without the
/// all-files reading of `*.*` ([`Pattern::with_all_files_rule`]).
pub fn is_recursive_filename_pattern(spec: &str) -> bool {
    let (fixed, wildcard_dir, _) = split_filespec(spec);
    fixed.is_empty() && wildcard_dir.trim_end_matches(['/', '\\']) == "**"
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
        let mut segs = split_segments(fragment)
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
            .collect::<Vec<_>>();
        // MSBuild rewrites a final `**` to `**/*.*` (`FileMatcher.SplitFileSpec`):
        // the pattern names the files *below* a directory, so `bin/**` does
        // not match a file called `bin`, and `a*/**` needs `a*` to be one.
        if segs.last() == Some(&Seg::DoubleStar) {
            segs.push(Seg::Match(vec![Tok::Star]));
        }
        Pattern { segs }
    }

    /// This pattern with MSBuild's all-files reading of its filename part
    /// (`FileMatcher.IsAllFilesWildcard`): a final segment of `*.*` matches
    /// every file, extensionless ones included, exactly as `*` does. The
    /// file matcher applies it to every glob include and to the excludes it
    /// is handed; `FileSpecMatcherTester` applies it except in its
    /// filename-only branch ([`is_recursive_filename_pattern`]). `a*.*` is
    /// not read this way.
    pub fn with_all_files_rule(mut self) -> Pattern {
        if let Some(Seg::Match(toks)) = self.segs.last_mut()
            && toks.as_slice() == [Tok::Star, Tok::Lit('.'), Tok::Star]
        {
            *toks = vec![Tok::Star];
        }
        self
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

    /// Whether this pattern contains any wildcard (`*`, `?`, or `**`). The
    /// resolver does not ask: whether a fragment globs is MSBuild's
    /// classification of its escaped text ([`SpecFragment::is_wildcard`]),
    /// which the parsed pattern no longer carries.
    #[cfg(test)]
    fn is_glob(&self) -> bool {
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

    /// [`Self::matches`] with ASCII case folded on both sides.
    pub fn matches_ignoring_ascii_case(&self, candidate: &str) -> bool {
        let folded = Pattern {
            segs: self
                .segs
                .iter()
                .map(|seg| match seg {
                    Seg::DoubleStar => Seg::DoubleStar,
                    Seg::Match(toks) => Seg::Match(
                        toks.iter()
                            .map(|tok| match tok {
                                Tok::Lit(c) => Tok::Lit(c.to_ascii_lowercase()),
                                other => other.clone(),
                            })
                            .collect(),
                    ),
                })
                .collect(),
        };
        folded.matches(&candidate.to_ascii_lowercase())
    }
}

/// Whether `candidate` survives `include` and `excludes` — and the same
/// verdict with ASCII case folded. A caller that cannot rule out
/// case-insensitive matching must decline when the two differ.
fn selected_both_ways(include: &Pattern, excludes: &[Pattern], candidate: &str) -> (bool, bool) {
    let exact = include.matches(candidate) && !excludes.iter().any(|e| e.matches(candidate));
    let folded = include.matches_ignoring_ascii_case(candidate)
        && !excludes
            .iter()
            .any(|e| e.matches_ignoring_ascii_case(candidate));
    (exact, folded)
}

/// The length in bytes of the longest prefix all of `strings` share.
fn common_prefix_len(strings: &[String]) -> usize {
    let Some((first, rest)) = strings.split_first() else {
        return 0;
    };
    rest.iter().fold(first.len(), |len, s| {
        first.as_bytes()[..len]
            .iter()
            .zip(s.as_bytes())
            .take_while(|(a, b)| a == b)
            .count()
    })
}

/// `StringComparer.OrdinalIgnoreCase` restricted to ASCII: compare UTF-16 code
/// units after upper-casing. Upper, not lower, is load-bearing: `_` (0x5F)
/// sorts *after* `Z` (0x5A) but before `z` (0x7A).
fn ordinal_ignore_case(a: &str, b: &str) -> std::cmp::Ordering {
    a.bytes()
        .map(|c| c.to_ascii_uppercase())
        .cmp(b.bytes().map(|c| c.to_ascii_uppercase()))
}

/// Sort one fragment's matches the way MSBuild does, or decline where that
/// order is not reproducible (see [`GlobDecline::OrderTie`] and
/// [`GlobDecline::NonAsciiOrder`]). A single match has no order to get wrong.
///
/// The text every match shares — the glob root and the project directory
/// above it — cannot decide their order, so only what follows it must be
/// ASCII: a checkout under `/home/José/` still commits.
fn sort_like_msbuild(matched: &mut [String]) -> Result<(), GlobDecline> {
    if matched.len() < 2 {
        return Ok(());
    }
    let shared = common_prefix_len(matched);
    // A shared prefix ending inside a multi-byte character leaves the rest of
    // that character in every suffix, which then fails the ASCII check.
    if let Some(path) = matched.iter().find(|m| !m.as_bytes()[shared..].is_ascii()) {
        return Err(GlobDecline::NonAsciiOrder { path: path.clone() });
    }
    matched.sort_by(|a, b| ordinal_ignore_case(a, b));
    // Identical strings are interchangeable, so only *distinct* names that
    // compare equal leave the order open.
    if let Some(pair) = matched
        .windows(2)
        .find(|pair| pair[0] != pair[1] && ordinal_ignore_case(&pair[0], &pair[1]).is_eq())
    {
        return Err(GlobDecline::OrderTie {
            first: pair[0].clone(),
            second: pair[1].clone(),
        });
    }
    Ok(())
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
/// fragment's, whatever their names. *Within* one fragment's expansion the
/// matches are in MSBuild's `OrdinalIgnoreCase` order (see the module docs).
/// Overlapping fragments therefore yield duplicates (a literal also caught
/// by a later glob appears twice), as MSBuild keeps them.
///
/// Declines — rather than guess — when folding ASCII case would change a
/// fragment's selection, or when its MSBuild order is not reproducible.
///
/// Output paths are `/`-normalised (backslashes → `/`, runs of `/`
/// collapsed, lone `.` dropped).
pub fn select(
    candidates: &[&str],
    includes: &[Pattern],
    excludes: &[Pattern],
) -> Result<Vec<String>, GlobDecline> {
    let mut out: Vec<String> = Vec::new();
    for inc in includes {
        let mut matched: Vec<String> = Vec::new();
        for candidate in candidates {
            let (exact, folded) = selected_both_ways(inc, excludes, candidate);
            if exact != folded {
                return Err(GlobDecline::CaseAmbiguous {
                    path: candidate.to_string(),
                });
            }
            if exact {
                matched.push(split_segments(candidate).join("/"));
            }
        }
        sort_like_msbuild(&mut matched)?;
        out.append(&mut matched);
    }
    Ok(out)
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
        let mut p = split_segments(pattern);
        // MSBuild's final-`**` rewrite (`**/*.*`), as `Pattern::parse` has it.
        if p.last() == Some(&"**") {
            p.push("*");
        }
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
            // MSBuild's `OrdinalIgnoreCase`; the generated names are lower-case
            // ASCII, so this is total and decidable.
            matched.sort_by_key(|m| m.to_ascii_uppercase());
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
            let got = select(&cand_refs, &compiled_incl, &compiled_excl).unwrap();
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
            !p.matches("src"),
            "a final `**` is `**/*.*`: it names files below `src/`, not `src`"
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
            select(&cands, &incl, &[]).unwrap(),
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
            select(&cands, &incl, &[]).unwrap(),
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
        let got = select(&["c0.fs", "a\\b.fs"], &incl, &[]).unwrap();
        assert_eq!(got, vec!["a/b.fs".to_string(), "c0.fs".to_string()]);
    }

    #[test]
    fn select_orders_like_ordinal_ignore_case() {
        // Probed against `dotnet msbuild` (10.0.301): upper-case folding puts
        // `_` after `Z`; `-` < `.` < `/` keeps `ab-c.fs`, `ab.fs`, `ab/x.fs`
        // in that order. The generated differential pins it in general.
        let cands = [
            "_u.fs", "Zeta.fs", "alpha.fs", "ab/x.fs", "ab.fs", "ab-c.fs", "Beta.fs",
        ];
        let got = select(&cands, &[Pattern::parse("**/*.fs")], &[]).unwrap();
        assert_eq!(
            got,
            [
                "ab-c.fs", "ab.fs", "ab/x.fs", "alpha.fs", "Beta.fs", "Zeta.fs", "_u.fs"
            ]
        );
    }

    #[test]
    fn a_non_ascii_prefix_shared_by_every_match_does_not_decline() {
        let incl = [Pattern::parse("/home/José/app/*.fs")];
        let got = select(&["/home/José/app/b.fs", "/home/José/app/a.fs"], &incl, &[]);
        assert_eq!(got.unwrap(), ["home/José/app/a.fs", "home/José/app/b.fs"]);
        // Where the matches differ in a non-ASCII position, it still declines.
        let incl = [Pattern::parse("/home/*/a.fs")];
        let got = select(&["/home/José/a.fs", "/home/Josa/a.fs"], &incl, &[]);
        assert!(
            matches!(got, Err(GlobDecline::NonAsciiOrder { .. })),
            "{got:?}"
        );
    }

    #[test]
    fn select_declines_a_case_only_tie() {
        // Possible only on a case-sensitive filesystem: MSBuild's unstable sort
        // leaves the pair's order unspecified.
        let got = select(&["a.fs", "A.fs"], &[Pattern::parse("*.fs")], &[]);
        assert!(matches!(got, Err(GlobDecline::OrderTie { .. })), "{got:?}");
    }

    #[test]
    fn select_declines_non_ascii_order_but_not_a_lone_match() {
        let incl = [Pattern::parse("*.fs")];
        let got = select(&["é.fs", "a.fs"], &incl, &[]);
        assert!(
            matches!(got, Err(GlobDecline::NonAsciiOrder { .. })),
            "{got:?}"
        );
        // One match has no order to get wrong.
        assert_eq!(select(&["é.fs"], &incl, &[]).unwrap(), ["é.fs"]);
    }

    #[test]
    fn select_declines_when_case_decides_selection() {
        // Include side: `a*.fs` meets `Ab.fs`.
        let got = select(&["Ab.fs"], &[Pattern::parse("a*.fs")], &[]);
        assert!(
            matches!(got, Err(GlobDecline::CaseAmbiguous { .. })),
            "{got:?}"
        );
        // Exclude side: `bin/**` meets `Bin/x.fs`.
        let got = select(
            &["Bin/x.fs"],
            &[Pattern::parse("**/*.fs")],
            &[Pattern::parse("bin/**")],
        );
        assert!(
            matches!(got, Err(GlobDecline::CaseAmbiguous { .. })),
            "{got:?}"
        );
    }

    /// `FileMatcher.RawFileSpecIsValid` and `IsLegalFileSpec`, case by case
    /// from MSBuild's source.
    #[test]
    fn legality_follows_msbuilds_file_matcher() {
        for legal in [
            "*.fs",
            "**/*.fs",
            "src/**/x/*.fs",
            "**",
            "../shared/*.fs",
            "/abs/**/*.fs",
            "C:/x/*.fs",
            "a.fs",
        ] {
            assert!(is_legal_filespec(legal), "{legal}");
        }
        for illegal in [
            // `**` must be a whole segment.
            "a**b.fs",
            "**a/*.fs",
            "src/x**/*.fs",
            "*.fs**",
            // `..` may not follow a wildcard.
            "*/../x.fs",
            "**/../*.fs",
            // `...` anywhere, a `:` anywhere but second, an invalid char.
            "a.../*.fs",
            "ab:c*.fs",
            "a|b*.fs",
            "a\u{1}*.fs",
        ] {
            assert!(!is_legal_filespec(illegal), "{illegal}");
        }
    }

    /// `FileUtilities.PathIsInvalid`: only the filename part is checked for
    /// the characters Windows forbids in a filename.
    #[test]
    fn unnormalised_comparison_follows_path_is_invalid() {
        for raw in ["a*b.fs", "./a?b.fs", "/abs/a:b.fs", "x<y", "a|b/c.fs"] {
            assert!(compares_unnormalised(raw), "{raw}");
        }
        for normalised in ["ab.fs", "./ab.fs", "sub/../ab.fs", "a*b/c.fs", "a b.fs"] {
            assert!(!compares_unnormalised(normalised), "{normalised}");
        }
    }

    /// `IsAllFilesWildcard`: a filename part of `*.*` is every file; `a*.*`
    /// and a `*.*` directory segment are not.
    #[test]
    fn star_dot_star_filename_is_every_file() {
        let all = Pattern::parse("*.*").with_all_files_rule();
        assert!(all.matches("ab") && all.matches("ab.fs"));
        assert!(!Pattern::parse("*.*").matches("ab"));
        let recursive = Pattern::parse("**/*.*").with_all_files_rule();
        assert!(recursive.matches("sub/noext"));
        let prefixed = Pattern::parse("a*.*").with_all_files_rule();
        assert!(!prefixed.matches("ab"));
        let dir = Pattern::parse("*.*/x").with_all_files_rule();
        assert!(!dir.matches("ab/x"));
        assert!(is_recursive_filename_pattern("**/*.*"));
        assert!(!is_recursive_filename_pattern("sub/**/*.*"));
        assert!(!is_recursive_filename_pattern("*.*"));
    }

    /// The allow-list admits the shapes the differentials verify and names
    /// the rule anything else breaks.
    #[test]
    fn the_allow_list_names_what_it_refuses() {
        for modelled in [
            "a.fs",
            "./a.fs",
            "../shared/*.fs",
            "/abs/**/*.fs",
            "a*b.fs",
            "a b.fs",
            "a;b.fs",
            "bin\\/**",
            "pct%.fs",
        ] {
            assert_eq!(unmodelled_shape(modelled), None, "{modelled}");
        }
        for (spec, shape) in [
            ("\u{dc}.fs", Unmodelled::NonAscii),
            ("\u{1f98b}?.fs", Unmodelled::NonAscii),
            ("a\u{1}.fs", Unmodelled::NonAscii),
            ("*.fs/", Unmodelled::TrailingSeparator),
            ("sub\\", Unmodelled::TrailingSeparator),
            ("*.fs.", Unmodelled::TrailingDotOrSpace),
            ("sub./a.fs", Unmodelled::TrailingDotOrSpace),
            ("a.fs ", Unmodelled::TrailingDotOrSpace),
            ("a:b.fs", Unmodelled::WindowsReservedCharacter),
            ("a<b.fs", Unmodelled::WindowsReservedCharacter),
            ("a\"b.fs", Unmodelled::WindowsReservedCharacter),
        ] {
            assert_eq!(unmodelled_shape(spec), Some(shape), "{spec}");
        }
    }

    /// A final `**` names the files below a directory
    /// (`FileMatcher.SplitFileSpec`).
    #[test]
    fn final_double_star_names_files_below() {
        let bin = Pattern::parse("bin/**");
        assert!(bin.matches("bin/x.fs") && bin.matches("bin/a/b.fs"));
        assert!(
            !bin.matches("bin"),
            "a file called `bin` is not below `bin/`"
        );
        assert!(Pattern::parse("**").matches("a.fs"));
        assert!(has_wildcard_directory_part("**/a.fs") && has_wildcard_directory_part("bin/**"));
        assert!(!has_wildcard_directory_part("sub/*.fs"));
    }
}
