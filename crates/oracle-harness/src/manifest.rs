//! Exact per-item manifests for deterministic corpus gates.
//!
//! A corpus sweep grades thousands of items (uses, commits, files) against an
//! oracle. Summarising that as a count and asserting a one-sided bound
//! (`matches >= FLOOR`, `alt_binders <= CEILING`) leaves the gap between the
//! bound and the measured value as room for a regression to hide in, and a
//! movement *within* the count (one item gained, another lost) is invisible
//! however tight the bound.
//!
//! When the corpus is content-addressed and both sides are deterministic, no
//! such slack is needed: the sweep's outcome for every item is a fixed fact, so
//! it can be checked in and compared **exactly**. A [`Manifest`] is that fact —
//! one compact line per item, sorted — and [`check`] compares a run against the
//! checked-in copy. Any movement, in either direction, fails with a readable
//! line diff; an intended movement is acknowledged by regenerating the file
//! (set [`UPDATE_ENV`]) and committing the diff, where a reviewer sees exactly
//! which items moved.
//!
//! A manifest records state, so it cannot by itself say a state is *wrong*.
//! Hard soundness gates (a divergence count that must be zero) stay separate
//! assertions, which run whether or not the manifest is being regenerated, so
//! regeneration cannot bless a wrong answer.

use std::collections::BTreeMap;
use std::path::Path;

/// Set to a non-empty value other than `0` to rewrite every manifest a test run
/// checks, instead of comparing against it.
pub const UPDATE_ENV: &str = "BORZOI_UPDATE_MANIFESTS";

/// How many diff lines [`check`] prints before eliding the rest.
const DIFF_LIMIT: usize = 200;

/// Why a string cannot be a manifest entry. Each would make the rendered file
/// fail to parse back to the same manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryError {
    /// The empty string (blank lines are skipped on parse).
    Empty,
    /// A line break inside the entry.
    LineBreak(String),
    /// A leading `#` (comment lines are skipped on parse).
    CommentMarker(String),
    /// Leading or trailing whitespace, which an editor may silently strip.
    SurroundingWhitespace(String),
    /// The same entry twice. Entries are keys: a caller whose items can repeat
    /// uses [`Manifest::from_counted`].
    Duplicate(String),
    /// An item passed to [`Manifest::from_counted`] that already ends in the
    /// ` x<digits>` count suffix, which would make `a x2` indistinguishable from
    /// `a` twice.
    ReservedCountSuffix(String),
}

impl std::fmt::Display for EntryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EntryError::Empty => write!(f, "empty manifest entry"),
            EntryError::LineBreak(e) => write!(f, "manifest entry contains a line break: {e:?}"),
            EntryError::CommentMarker(e) => write!(f, "manifest entry starts with `#`: {e:?}"),
            EntryError::SurroundingWhitespace(e) => {
                write!(f, "manifest entry has surrounding whitespace: {e:?}")
            }
            EntryError::Duplicate(e) => write!(f, "duplicate manifest entry: {e:?}"),
            EntryError::ReservedCountSuffix(e) => {
                write!(f, "counted item ends in the reserved ` x<n>` suffix: {e:?}")
            }
        }
    }
}

fn validate(entry: &str) -> Result<(), EntryError> {
    if entry.is_empty() {
        Err(EntryError::Empty)
    } else if entry.contains(['\n', '\r']) {
        Err(EntryError::LineBreak(entry.to_string()))
    } else if entry.starts_with('#') {
        Err(EntryError::CommentMarker(entry.to_string()))
    } else if entry.trim() != entry {
        Err(EntryError::SurroundingWhitespace(entry.to_string()))
    } else {
        Ok(())
    }
}

/// A set of manifest entries: each a valid single line (see [`EntryError`]),
/// held sorted and distinct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    entries: Vec<String>,
}

impl Manifest {
    /// A manifest of exactly these entries. Duplicates are an error: an entry is
    /// meant to be a key, and two identical ones mean the key is too coarse.
    pub fn from_entries(entries: impl IntoIterator<Item = String>) -> Result<Self, EntryError> {
        let mut entries: Vec<String> = entries.into_iter().collect();
        for e in &entries {
            validate(e)?;
        }
        entries.sort();
        if let Some(w) = entries.windows(2).find(|w| w[0] == w[1]) {
            return Err(EntryError::Duplicate(w[0].clone()));
        }
        Ok(Manifest { entries })
    }

    /// A manifest of these items as a multiset: an item that occurs `n > 1`
    /// times becomes the single entry `"<item> x<n>"`. For oracles that report
    /// the same fact more than once (FCS records some symbol uses twice at one
    /// range), where the multiplicity is itself part of the deterministic output.
    /// An item may not itself end in ` x<digits>`, so the encoding is injective.
    pub fn from_counted(items: impl IntoIterator<Item = String>) -> Result<Self, EntryError> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for item in items {
            validate(&item)?;
            if item
                .rsplit_once(" x")
                .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            {
                return Err(EntryError::ReservedCountSuffix(item));
            }
            *counts.entry(item).or_default() += 1;
        }
        Manifest::from_entries(counts.into_iter().map(|(item, n)| {
            if n == 1 { item } else { format!("{item} x{n}") }
        }))
    }

    /// Read a rendered manifest back. Blank lines and `#` comment lines are
    /// skipped; every other line is an entry.
    pub fn parse(text: &str) -> Result<Self, (usize, EntryError)> {
        let mut entries = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            validate(line).map_err(|e| (i + 1, e))?;
            entries.push(line.to_string());
        }
        Manifest::from_entries(entries).map_err(|e| (0, e))
    }

    /// The file form: `header` as `#` comment lines, then one entry per line.
    pub fn render(&self, header: &str) -> String {
        let mut out = String::new();
        for line in header.lines() {
            if line.is_empty() {
                out.push_str("#\n");
            } else {
                out.push_str("# ");
                out.push_str(line);
                out.push('\n');
            }
        }
        for e in &self.entries {
            out.push_str(e);
            out.push('\n');
        }
        out
    }

    /// The entries, sorted and distinct.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// What changed from `self` (the expected manifest) to `actual`.
    pub fn diff(&self, actual: &Manifest) -> ManifestDiff {
        let (mut removed, mut added) = (Vec::new(), Vec::new());
        let (mut a, mut b) = (
            self.entries.iter().peekable(),
            actual.entries.iter().peekable(),
        );
        loop {
            match (a.peek(), b.peek()) {
                (Some(x), Some(y)) if x == y => {
                    a.next();
                    b.next();
                }
                (Some(x), Some(y)) if x < y => removed.push(a.next().expect("peeked").clone()),
                (Some(_), Some(_)) | (None, Some(_)) => {
                    added.push(b.next().expect("peeked").clone())
                }
                (Some(_), None) => removed.push(a.next().expect("peeked").clone()),
                (None, None) => break,
            }
        }
        ManifestDiff { removed, added }
    }
}

/// The entries one manifest has and the other lacks. Both lists are sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestDiff {
    /// In the expected manifest, absent from the run.
    pub removed: Vec<String>,
    /// In the run, absent from the expected manifest.
    pub added: Vec<String>,
}

impl ManifestDiff {
    /// Whether the two manifests were identical.
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty()
    }

    /// A unified-style listing (`- ` removed, `+ ` added), merged in sort order
    /// so an item that changed bucket shows its old and new lines together when
    /// the entry format puts the item's key first. At most `limit` lines; the
    /// rest are counted.
    pub fn render(&self, limit: usize) -> String {
        let mut lines: Vec<(&str, char)> = self
            .removed
            .iter()
            .map(|e| (e.as_str(), '-'))
            .chain(self.added.iter().map(|e| (e.as_str(), '+')))
            .collect();
        lines.sort();
        let mut out = format!(
            "{} entries removed, {} added:\n",
            self.removed.len(),
            self.added.len()
        );
        for (e, sign) in lines.iter().take(limit) {
            out.push_str(&format!("{sign} {e}\n"));
        }
        if lines.len() > limit {
            out.push_str(&format!("… and {} more\n", lines.len() - limit));
        }
        out
    }
}

/// The suffix Nix appends on case-insensitive filesystems (macOS) to the second
/// of two store names that differ only in case: the pinned F# corpus holds both
/// `CompilerOptions/Fsc` and `CompilerOptions/fsc`, and on macOS the latter is
/// unpacked as `fsc~nix~case~hack~1`. Linux keeps the original name.
const NIX_CASE_HACK: &str = "~nix~case~hack~";

/// One path component as the input spells it, undoing [`NIX_CASE_HACK`].
fn component_key(component: &str) -> &str {
    match component.rsplit_once(NIX_CASE_HACK) {
        Some((name, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => component,
    }
}

/// `path` relative to `root`, its components joined with `/`, so an entry names
/// the same file whichever directory the input is checked out or unpacked in,
/// whichever platform separator the walk produced, and whether or not Nix had
/// to rename it for a case-insensitive filesystem. Panics if `path` is not
/// under `root`: an entry keyed by an absolute path would differ per machine.
pub fn relative_key(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or_else(|_| {
        panic!(
            "{} is not under the manifest root {}",
            path.display(),
            root.display()
        )
    });
    rel.components()
        .map(|c| component_key(&c.as_os_str().to_string_lossy()).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn update_requested() -> bool {
    std::env::var_os(UPDATE_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Compare `actual` against the manifest checked in at `path`, panicking with a
/// line diff on any difference. `regenerate` is the one command that rewrites
/// this manifest; it goes into the file's header and the failure message.
///
/// With [`UPDATE_ENV`] set, writes `actual` to `path` instead (printing what
/// moved) and passes.
#[track_caller]
pub fn check(path: &Path, actual: &Manifest, regenerate: &str) {
    check_with(path, actual, regenerate, update_requested());
}

#[track_caller]
fn check_with(path: &Path, actual: &Manifest, regenerate: &str, update: bool) {
    let header = format!(
        "Generated; do not edit by hand. One line per graded item, sorted.\n\
         Regenerate with:\n  {regenerate}"
    );
    let existing = std::fs::read_to_string(path).ok();
    if update {
        if let Some(Ok(old)) = existing.as_deref().map(Manifest::parse) {
            let d = old.diff(actual);
            if !d.is_empty() {
                eprintln!("manifest {}: {}", path.display(), d.render(DIFF_LIMIT));
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        }
        std::fs::write(path, actual.render(&header))
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        eprintln!(
            "manifest {}: wrote {} entries ({UPDATE_ENV} is set)",
            path.display(),
            actual.entries().len()
        );
        return;
    }
    let Some(text) = existing else {
        panic!(
            "no manifest at {}. Generate it with:\n  {regenerate}",
            path.display()
        );
    };
    let expected = Manifest::parse(&text).unwrap_or_else(|(line, e)| {
        panic!(
            "manifest {} is malformed at line {line}: {e}. Regenerate it with:\n  {regenerate}",
            path.display()
        )
    });
    let d = expected.diff(actual);
    assert!(
        d.is_empty(),
        "the run does not match the checked-in manifest {}.\n{}\n\
         Every movement is signal: a lost entry may be a regression, and a gained \
         one an improvement to acknowledge. If every moved line is intended, \
         regenerate the manifest and commit the diff:\n  {regenerate}",
        path.display(),
        d.render(DIFF_LIMIT),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    /// Entries from a small alphabet, so generated manifests overlap.
    fn entry() -> impl Strategy<Value = String> {
        "[a-c][a-c :x0-9]{0,3}[a-c0-9]"
    }

    fn manifest() -> impl Strategy<Value = Manifest> {
        prop::collection::btree_set(entry(), 0..12)
            .prop_map(|s| Manifest::from_entries(s).expect("valid entries"))
    }

    proptest! {
        #[test]
        fn render_then_parse_is_identity(m in manifest(), header in "[ -~\n]{0,40}") {
            prop_assert_eq!(Manifest::parse(&m.render(&header)), Ok(m));
        }

        #[test]
        fn diff_is_exactly_the_symmetric_difference(a in manifest(), b in manifest()) {
            let d = a.diff(&b);
            let sa: BTreeSet<&String> = a.entries().iter().collect();
            let sb: BTreeSet<&String> = b.entries().iter().collect();
            let removed: Vec<&String> = sa.difference(&sb).copied().collect();
            let added: Vec<&String> = sb.difference(&sa).copied().collect();
            prop_assert_eq!(d.removed.iter().collect::<Vec<_>>(), removed);
            prop_assert_eq!(d.added.iter().collect::<Vec<_>>(), added);
            prop_assert_eq!(d.is_empty(), a == b);
        }

        #[test]
        fn from_counted_keeps_each_items_multiplicity(
            items in prop::collection::vec("[a-c]{1,2}", 0..20)
        ) {
            let m = Manifest::from_counted(items.clone()).expect("valid items");
            let mut counts: BTreeMap<&String, usize> = BTreeMap::new();
            for i in &items {
                *counts.entry(i).or_default() += 1;
            }
            prop_assert_eq!(m.entries().len(), counts.len());
            for (item, n) in counts {
                let want = if n == 1 { item.clone() } else { format!("{item} x{n}") };
                prop_assert!(m.entries().contains(&want), "missing {want:?} in {m:?}");
            }
        }

        /// A counted manifest decodes back to exactly the multiset it was built
        /// from (so distinct multisets never collapse to one manifest): the
        /// count suffix cannot be forged by an item that spells one itself.
        #[test]
        fn from_counted_decodes_to_its_multiset(
            items in prop::collection::vec(
                prop::sample::select(vec!["a", "a x2", "a x3", "a x", "b x1", "b"]),
                0..6,
            ),
        ) {
            let Ok(m) = Manifest::from_counted(items.iter().map(|s| s.to_string())) else {
                return Ok(());
            };
            let mut want: BTreeMap<String, usize> = BTreeMap::new();
            for i in &items {
                *want.entry(i.to_string()).or_default() += 1;
            }
            let mut got: BTreeMap<String, usize> = BTreeMap::new();
            for e in m.entries() {
                let (item, n) = match e.rsplit_once(" x") {
                    Some((item, n)) if n.parse::<usize>().is_ok_and(|n| n > 1) => {
                        (item.to_string(), n.parse::<usize>().expect("checked"))
                    }
                    _ => (e.clone(), 1),
                };
                *got.entry(item).or_default() += n;
            }
            prop_assert_eq!(got, want);
        }

        /// The key is the components as the input spells them, `/`-joined,
        /// whether or not Nix renamed any of them for a case-insensitive
        /// filesystem.
        #[test]
        fn relative_key_is_the_unhacked_components_joined_by_slash(
            parts in prop::collection::vec(("[A-Za-z0-9 ._~-]{1,8}", prop::option::of(1u32..4)), 1..5),
        ) {
            let root = Path::new("/corpus/root");
            let mut path = root.to_path_buf();
            for (name, hack) in &parts {
                prop_assume!(component_key(name) == name.as_str() && name != "." && name != "..");
                path.push(match hack {
                    Some(n) => format!("{name}{NIX_CASE_HACK}{n}"),
                    None => name.clone(),
                });
            }
            let want = parts.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join("/");
            prop_assert_eq!(relative_key(root, &path), want);
        }

        #[test]
        fn rendered_diff_lists_every_moved_entry_below_the_limit(a in manifest(), b in manifest()) {
            let d = a.diff(&b);
            let text = d.render(usize::MAX);
            for e in &d.removed {
                let want = format!("- {e}");
                prop_assert!(text.lines().any(|l| l == want));
            }
            for e in &d.added {
                let want = format!("+ {e}");
                prop_assert!(text.lines().any(|l| l == want));
            }
        }
    }

    #[test]
    fn relative_key_undoes_the_nix_case_hack_and_nothing_else() {
        let root = Path::new("/nix/store/x-source");
        let key = |rel: &str| relative_key(root, &root.join(rel));
        assert_eq!(
            key("tests/CompilerOptions/fsc~nix~case~hack~1/times/times01.fs"),
            "tests/CompilerOptions/fsc/times/times01.fs"
        );
        assert_eq!(key("a/Fsc/b.fs"), "a/Fsc/b.fs");
        assert_eq!(key("a/x~nix~case~hack~/b.fs"), "a/x~nix~case~hack~/b.fs");
        assert_eq!(
            key("a/x~nix~case~hack~1a/b.fs"),
            "a/x~nix~case~hack~1a/b.fs"
        );
    }

    #[test]
    fn invalid_entries_are_rejected() {
        let bad = |e: &str| Manifest::from_entries([e.to_string()]).unwrap_err();
        assert_eq!(bad(""), EntryError::Empty);
        assert!(matches!(bad("a\nb"), EntryError::LineBreak(_)));
        assert!(matches!(bad("# a"), EntryError::CommentMarker(_)));
        assert!(matches!(bad("a "), EntryError::SurroundingWhitespace(_)));
        assert!(matches!(
            Manifest::from_entries(["a".to_string(), "a".to_string()]),
            Err(EntryError::Duplicate(_))
        ));
    }

    #[test]
    fn check_passes_on_a_match_and_names_the_moved_lines_otherwise() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("m.txt");
        let m = |es: &[&str]| Manifest::from_entries(es.iter().map(|s| s.to_string())).unwrap();
        std::fs::write(&path, m(&["a 1", "b 2"]).render("hdr")).unwrap();
        check_with(&path, &m(&["a 1", "b 2"]), "regen", false);
        let err = {
            let _quiet = crate::panic_silence::silence_panics_here();
            std::panic::catch_unwind(|| check_with(&path, &m(&["a 1", "b 3"]), "regen-cmd", false))
                .expect_err("a moved entry must fail")
        };
        let msg = err
            .downcast_ref::<String>()
            .expect("formatted panic message");
        assert!(msg.contains("- b 2\n+ b 3\n"), "{msg}");
        assert!(msg.contains("regen-cmd"), "{msg}");

        check_with(&path, &m(&["c"]), "regen", true);
        assert_eq!(
            Manifest::parse(&std::fs::read_to_string(&path).unwrap()),
            Ok(m(&["c"]))
        );
    }
}
