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
//!
//! # Keys, and merging
//!
//! An entry names its item with a **key**: the text before its first
//! [`KEY_SEPARATOR`], or the whole entry if it has none ([`key_of`]). Keys are
//! distinct within a manifest. A keyed entry is `key<TAB>value`, so an item
//! whose verdict moves changes one line and keeps its key; an entry with no
//! separator is its own key, an item that is either present or absent.
//!
//! That makes a manifest a finite map from key to entry, and two independent
//! changes to one merge as maps ([`merge3`]): a key one side changed takes that
//! side's entry, and a key both sides changed differently is a conflict. Git
//! is pointed at this through `.gitattributes` and `tools/manifest-merge.sh`
//! for each manifest whose every entry is keyed, so rebasing past an unrelated
//! change to one needs no regeneration. (An unkeyed entry is its own key, so a
//! line both sides rewrote would merge to both rewrites; such a manifest stays
//! on Git's text merge.) The merge is only as good as the manifest's lines are
//! independent: a line
//! summing over items (`matches=…`) changes with every item it sums and so
//! conflicts whenever two changes touch any of them. A manifest that should
//! merge keeps such counts to its *inputs* (what the oracle reported), which
//! move only when the corpus or the oracle does, and lists its own verdicts
//! item by item.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Set to a non-empty value other than `0` to rewrite every manifest a test run
/// checks, instead of comparing against it.
pub const UPDATE_ENV: &str = "BORZOI_UPDATE_MANIFESTS";

/// How many diff lines [`check`] prints before eliding the rest.
const DIFF_LIMIT: usize = 200;

/// Ends an entry's key ([`key_of`]). Also separates the values of a
/// [`Manifest::from_keyed`] entry.
pub const KEY_SEPARATOR: char = '\t';

/// The key `entry` names its item by: the text before its first
/// [`KEY_SEPARATOR`], or all of it.
pub fn key_of(entry: &str) -> &str {
    entry
        .split_once(KEY_SEPARATOR)
        .map_or(entry, |(key, _)| key)
}

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
    /// Two entries with one key ([`key_of`]): the manifest would no longer say
    /// which entry is the item's.
    DuplicateKey(String),
    /// A [`KEY_SEPARATOR`] inside a [`Manifest::from_keyed`] key or value,
    /// which would move where the key ends or where one value does.
    KeySeparator(String),
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
            EntryError::DuplicateKey(k) => write!(f, "two manifest entries have the key {k:?}"),
            EntryError::KeySeparator(e) => {
                write!(f, "keyed manifest key or value contains a tab: {e:?}")
            }
        }
    }
}

/// Whether `item` ends in the ` x<digits>` suffix [`Manifest::from_counted`]
/// writes.
fn has_count_suffix(item: &str) -> bool {
    item.rsplit_once(" x")
        .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// `item`, or `item x<n>` when it occurs `n > 1` times.
fn counted(item: String, n: usize) -> String {
    if n == 1 { item } else { format!("{item} x{n}") }
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
    /// meant to be a key, and two identical ones mean the key is too coarse. So
    /// are two entries with one [`key_of`].
    pub fn from_entries(entries: impl IntoIterator<Item = String>) -> Result<Self, EntryError> {
        let mut entries: Vec<String> = entries.into_iter().collect();
        for e in &entries {
            validate(e)?;
        }
        entries.sort();
        if let Some(w) = entries.windows(2).find(|w| w[0] == w[1]) {
            return Err(EntryError::Duplicate(w[0].clone()));
        }
        let mut keys = BTreeSet::new();
        if let Some(e) = entries.iter().find(|e| !keys.insert(key_of(e))) {
            return Err(EntryError::DuplicateKey(key_of(e).to_string()));
        }
        Ok(Manifest { entries })
    }

    /// A manifest of `(key, value)` items: one entry per key,
    /// `key<TAB>value<TAB>value…`, its values sorted and counted as
    /// [`Self::from_counted`] counts items. Several values under one key are
    /// for an item the oracle reports more than once — FCS records an
    /// attribute's name and its constructor at one range — so the entry is the
    /// multiset of their verdicts.
    pub fn from_keyed(
        items: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, EntryError> {
        let mut by_key: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        for (key, value) in items {
            for part in [&key, &value] {
                validate(part)?;
                if part.contains(KEY_SEPARATOR) {
                    return Err(EntryError::KeySeparator(part.clone()));
                }
            }
            if has_count_suffix(&value) {
                return Err(EntryError::ReservedCountSuffix(value));
            }
            *by_key.entry(key).or_default().entry(value).or_default() += 1;
        }
        Manifest::from_entries(by_key.into_iter().map(|(key, values)| {
            let mut entry = key;
            for (value, n) in values {
                entry.push(KEY_SEPARATOR);
                entry.push_str(&counted(value, n));
            }
            entry
        }))
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
            if has_count_suffix(&item) {
                return Err(EntryError::ReservedCountSuffix(item));
            }
            *counts.entry(item).or_default() += 1;
        }
        Manifest::from_entries(counts.into_iter().map(|(item, n)| counted(item, n)))
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

/// Whether [`UPDATE_ENV`] asks for manifests to be rewritten rather than compared.
pub fn update_requested() -> bool {
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
    if let Err(message) = compare(path, actual, regenerate) {
        panic!("{message}");
    }
}

/// [`check`] for a caller that reports a failure itself — a binary that exits
/// non-zero with the message — rather than by panicking.
pub fn compare(path: &Path, actual: &Manifest, regenerate: &str) -> Result<(), String> {
    compare_with(path, actual, regenerate, update_requested())
}

/// [`compare`] with the rewrite decision passed in rather than read from
/// [`UPDATE_ENV`], for a caller whose own tests must not depend on it.
pub fn compare_with(
    path: &Path,
    actual: &Manifest,
    regenerate: &str,
    update: bool,
) -> Result<(), String> {
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
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        std::fs::write(path, actual.render(&header))
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        eprintln!(
            "manifest {}: wrote {} entries ({UPDATE_ENV} is set)",
            path.display(),
            actual.entries().len()
        );
        return Ok(());
    }
    let Some(text) = existing else {
        return Err(format!(
            "no manifest at {}. Generate it with:\n  {regenerate}",
            path.display()
        ));
    };
    let expected = Manifest::parse(&text).map_err(|(line, e)| {
        format!(
            "manifest {} is malformed at line {line}: {e}. Regenerate it with:\n  {regenerate}",
            path.display()
        )
    })?;
    let d = expected.diff(actual);
    if d.is_empty() {
        return Ok(());
    }
    Err(format!(
        "the run does not match the checked-in manifest {}.\n{}\n\
         Every movement is signal: a lost entry may be a regression, and a gained \
         one an improvement to acknowledge. If every moved line is intended, \
         regenerate the manifest and commit the diff:\n  {regenerate}",
        path.display(),
        d.render(DIFF_LIMIT),
    ))
}

/// A key both sides of a [`merge3`] changed from the base, to different
/// entries. `None` is the key's absence: an item one side dropped and the
/// other moved conflicts too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeConflict {
    pub key: String,
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
}

/// The three-way merge of one key's entries: a side that left the base alone
/// takes the other side's. `Err(())` when both changed it, differently.
fn merge_one<T: PartialEq>(base: T, ours: T, theirs: T) -> Result<T, ()> {
    if ours == theirs || theirs == base {
        Ok(ours)
    } else if ours == base {
        Ok(theirs)
    } else {
        Err(())
    }
}

/// The three-way merge of two manifests changed independently from `base`,
/// key by key ([`key_of`]): each key takes the entry of whichever side changed
/// it, adding and dropping included. Fails, naming each, on the keys both sides
/// changed to different entries; nothing else can conflict.
///
/// Symmetric in `ours` and `theirs`, and `merge3(b, m, m) == Ok(m)`.
pub fn merge3(
    base: &Manifest,
    ours: &Manifest,
    theirs: &Manifest,
) -> Result<Manifest, Vec<MergeConflict>> {
    let by_key = |m: &Manifest| -> BTreeMap<String, String> {
        m.entries
            .iter()
            .map(|e| (key_of(e).to_string(), e.clone()))
            .collect()
    };
    let (base, ours, theirs) = (by_key(base), by_key(ours), by_key(theirs));
    let keys: BTreeSet<&String> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .collect();
    let (mut merged, mut conflicts) = (Vec::new(), Vec::new());
    for key in keys {
        let (b, o, t) = (base.get(key), ours.get(key), theirs.get(key));
        match merge_one(b, o, t) {
            Ok(entry) => merged.extend(entry.cloned()),
            Err(()) => conflicts.push(MergeConflict {
                key: key.clone(),
                base: b.cloned(),
                ours: o.cloned(),
                theirs: t.cloned(),
            }),
        }
    }
    if conflicts.is_empty() {
        Ok(Manifest::from_entries(merged)
            .expect("one entry per key, each taken from a valid manifest"))
    } else {
        Err(conflicts)
    }
}

/// Which input of a [`merge_files`] a problem is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeSide {
    Base,
    Ours,
    Theirs,
}

/// Why [`merge_files`] produced no merged manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeFailure {
    /// An input is not a rendered manifest, so it has no keys to merge by.
    Malformed {
        side: MergeSide,
        line: usize,
        error: EntryError,
    },
    /// An input has an entry with no key ([`key_of`] is all of it), so the
    /// merge could not tell a rewrite of it from a new item: a branch from
    /// before the manifest was keyed, say, whose rewrite of a line would come
    /// out beside the keyed one rather than conflict with it.
    Unkeyed { side: MergeSide, entry: String },
    /// Both sides changed the same keys (or the header) differently.
    /// `marked` is the merge with each such key's three entries between Git's
    /// conflict markers, in its sorted place, for a person to resolve.
    Conflicts {
        header: bool,
        keys: Vec<MergeConflict>,
        marked: String,
    },
}

/// A rendered manifest's comment lines, in order, and its entries, each of
/// which must be keyed.
fn split_file(text: &str, side: MergeSide) -> Result<(Vec<&str>, Manifest), MergeFailure> {
    let header = text.lines().filter(|l| l.starts_with('#')).collect();
    let manifest = Manifest::parse(text).map_err(|(line, error)| MergeFailure::Malformed {
        side,
        line,
        error,
    })?;
    if let Some(e) = manifest.entries.iter().find(|e| key_of(e) == e.as_str()) {
        return Err(MergeFailure::Unkeyed {
            side,
            entry: e.clone(),
        });
    }
    Ok((header, manifest))
}

/// [`merge3`] over three rendered manifest files — what the Git merge driver
/// `tools/manifest-merge.sh` runs. Every entry of every input must be keyed
/// ([`MergeFailure::Unkeyed`]). The comment header merges as one value: a
/// side that left it alone takes the other side's. The result is rendered as
/// [`Manifest::render`] renders: the header, then the entries sorted.
pub fn merge_files(base: &str, ours: &str, theirs: &str) -> Result<String, MergeFailure> {
    let (base_header, base) = split_file(base, MergeSide::Base)?;
    let (ours_header, ours) = split_file(ours, MergeSide::Ours)?;
    let (theirs_header, theirs) = split_file(theirs, MergeSide::Theirs)?;
    let header = merge_one(&base_header, &ours_header, &theirs_header);
    let entries = merge3(&base, &ours, &theirs);
    let mut out = String::new();
    let line = |out: &mut String, l: &str| {
        out.push_str(l);
        out.push('\n');
    };
    let marked = |out: &mut String, [b, o, t]: [&[&str]; 3]| {
        for (marker, lines) in [("<<<<<<< ours", o), ("||||||| base", b), ("=======", t)] {
            line(out, marker);
            for l in lines {
                line(out, l);
            }
        }
        line(out, ">>>>>>> theirs");
    };
    match header {
        Ok(h) => h.iter().for_each(|l| line(&mut out, l)),
        Err(()) => marked(&mut out, [&base_header, &ours_header, &theirs_header]),
    }
    match (header, entries) {
        (Ok(_), Ok(merged)) => {
            merged.entries.iter().for_each(|e| line(&mut out, e));
            Ok(out)
        }
        (header, entries) => {
            let keys = entries.as_ref().err().cloned().unwrap_or_default();
            // The clean keys' entries and the conflicted keys' blocks, each at
            // its key's sorted place.
            let conflicted: BTreeSet<&str> = keys.iter().map(|c| c.key.as_str()).collect();
            let clean = merge3(
                &base,
                &without(&ours, &conflicted),
                &without(&theirs, &conflicted),
            )
            .expect("no key left that both sides changed");
            let mut blocks: Vec<(&str, Option<&MergeConflict>)> = clean
                .entries
                .iter()
                .filter(|e| !conflicted.contains(key_of(e)))
                .map(|e| (e.as_str(), None))
                .chain(keys.iter().map(|c| (c.key.as_str(), Some(c))))
                .collect();
            blocks.sort_by(|a, b| a.0.cmp(b.0));
            for (text, conflict) in blocks {
                match conflict {
                    None => line(&mut out, text),
                    Some(c) => {
                        let [b, o, t] = [&c.base, &c.ours, &c.theirs]
                            .map(|e| e.iter().map(String::as_str).collect::<Vec<_>>());
                        marked(&mut out, [&b, &o, &t]);
                    }
                }
            }
            Err(MergeFailure::Conflicts {
                header: header.is_err(),
                keys,
                marked: out,
            })
        }
    }
}

/// `m` without the entries whose keys are in `keys`.
fn without(m: &Manifest, keys: &BTreeSet<&str>) -> Manifest {
    Manifest::from_entries(
        m.entries
            .iter()
            .filter(|e| !keys.contains(key_of(e)))
            .cloned(),
    )
    .expect("a subset of a valid manifest")
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

    /// A keyed manifest over a small key and value alphabet, so independently
    /// generated ones share keys: each key absent, bare (`a`), or with a value
    /// (`a<TAB>x`).
    fn keyed() -> impl Strategy<Value = BTreeMap<String, Option<String>>> {
        prop::collection::btree_map("[a-d]{1,2}", prop::option::of("[xyz]"), 0..8)
    }

    /// `map` with every bare key given a value: the form the merge driver
    /// accepts.
    fn valued(map: BTreeMap<String, Option<String>>) -> BTreeMap<String, Option<String>> {
        map.into_iter()
            .map(|(k, v)| (k, Some(v.unwrap_or_else(|| "w".to_string()))))
            .collect()
    }

    fn manifest_of(map: &BTreeMap<String, Option<String>>) -> Manifest {
        Manifest::from_entries(map.iter().map(|(k, v)| match v {
            Some(v) => format!("{k}{KEY_SEPARATOR}{v}"),
            None => k.clone(),
        }))
        .expect("one entry per key")
    }

    /// `base` with each key in `edits` set to its edit: `None` drops it,
    /// `Some(v)` writes `v` (itself absent-or-valued).
    type Edits = BTreeMap<String, Option<Option<String>>>;

    fn edits() -> impl Strategy<Value = Edits> {
        prop::collection::btree_map(
            "[a-d]{1,2}",
            prop::option::of(prop::option::of("[xyz]")),
            0..5,
        )
    }

    fn apply(
        base: &BTreeMap<String, Option<String>>,
        edits: &Edits,
    ) -> BTreeMap<String, Option<String>> {
        let mut out = base.clone();
        for (k, edit) in edits {
            match edit {
                Some(v) => out.insert(k.clone(), v.clone()),
                None => out.remove(k),
            };
        }
        out
    }

    /// The naive oracle: per key, the three sides' entries, and the conflicts.
    fn reference_merge(b: &Manifest, o: &Manifest, t: &Manifest) -> Result<Manifest, Vec<String>> {
        let map = |m: &Manifest| -> BTreeMap<String, String> {
            m.entries()
                .iter()
                .map(|e| (key_of(e).to_string(), e.clone()))
                .collect()
        };
        let (b, o, t) = (map(b), map(o), map(t));
        let keys: BTreeSet<&String> = b.keys().chain(o.keys()).chain(t.keys()).collect();
        let (mut out, mut conflicts) = (Vec::new(), Vec::new());
        for k in keys {
            let (bk, ok, tk) = (b.get(k), o.get(k), t.get(k));
            if ok != bk && tk != bk && ok != tk {
                conflicts.push(k.clone());
            } else if ok != bk {
                out.extend(ok.cloned());
            } else {
                out.extend(tk.cloned());
            }
        }
        if conflicts.is_empty() {
            Ok(Manifest::from_entries(out).expect("one per key"))
        } else {
            Err(conflicts)
        }
    }

    fn conflict_keys(r: Result<Manifest, Vec<MergeConflict>>) -> Result<Manifest, Vec<String>> {
        r.map_err(|cs| cs.into_iter().map(|c| c.key).collect())
    }

    proptest! {
        #[test]
        fn merge_agrees_with_the_per_key_reference(
            b in keyed(), eo in edits(), et in edits()
        ) {
            let (base, ours, theirs) =
                (manifest_of(&b), manifest_of(&apply(&b, &eo)), manifest_of(&apply(&b, &et)));
            prop_assert_eq!(
                conflict_keys(merge3(&base, &ours, &theirs)),
                reference_merge(&base, &ours, &theirs)
            );
        }

        #[test]
        fn merge_is_symmetric(b in keyed(), o in keyed(), t in keyed()) {
            let (b, o, t) = (manifest_of(&b), manifest_of(&o), manifest_of(&t));
            prop_assert_eq!(merge3(&b, &o, &t), merge3(&b, &t, &o).map_err(|cs| {
                cs.into_iter()
                    .map(|c| MergeConflict { ours: c.theirs, theirs: c.ours, ..c })
                    .collect::<Vec<_>>()
            }));
        }

        #[test]
        fn a_side_that_changed_nothing_takes_the_other(b in keyed(), m in keyed()) {
            let (b, m) = (manifest_of(&b), manifest_of(&m));
            prop_assert_eq!(merge3(&b, &m, &m), Ok(m.clone()));
            prop_assert_eq!(merge3(&b, &b, &m), Ok(m.clone()));
            prop_assert_eq!(merge3(&b, &m, &b), Ok(m.clone()));
        }

        /// Merging is idempotent: folding the same side in again changes nothing.
        #[test]
        fn merging_a_side_in_twice_changes_nothing(b in keyed(), eo in edits(), et in edits()) {
            let (base, ours, theirs) =
                (manifest_of(&b), manifest_of(&apply(&b, &eo)), manifest_of(&apply(&b, &et)));
            if let Ok(merged) = merge3(&base, &ours, &theirs) {
                prop_assert_eq!(merge3(&base, &merged, &theirs), Ok(merged.clone()));
                prop_assert_eq!(merge3(&base, &ours, &merged), Ok(merged));
            }
        }

        /// Edits to disjoint keys always merge, to both edits applied.
        #[test]
        fn disjoint_edits_merge_to_both(b in keyed(), eo in edits(), et in edits()) {
            let et: Edits = et.into_iter().filter(|(k, _)| !eo.contains_key(k)).collect();
            let both: Edits = eo.iter().chain(&et).map(|(k, v)| (k.clone(), v.clone())).collect();
            prop_assert_eq!(
                merge3(&manifest_of(&b), &manifest_of(&apply(&b, &eo)), &manifest_of(&apply(&b, &et))),
                Ok(manifest_of(&apply(&b, &both)))
            );
        }

        /// One key moved to two different entries is always a conflict, and
        /// the only one, whatever else either side did elsewhere.
        #[test]
        fn one_key_changed_two_ways_always_conflicts(
            b in keyed(),
            eo in edits(),
            et in edits(),
            key in "[a-d]{1,2}",
            [v1, v2] in prop::array::uniform2(prop::option::of(prop::option::of("[xyz]"))),
        ) {
            let current = b.get(&key).cloned();
            prop_assume!(v1 != v2 && v1 != current && v2 != current);
            let (mut eo, mut et) = (eo, et);
            et.retain(|k, _| !eo.contains_key(k));
            let set = |edits: &mut Edits, v: Option<Option<String>>| match v {
                Some(v) => edits.insert(key.clone(), Some(v)),
                None => edits.insert(key.clone(), None),
            };
            set(&mut eo, v1);
            set(&mut et, v2);
            let r = merge3(&manifest_of(&b), &manifest_of(&apply(&b, &eo)), &manifest_of(&apply(&b, &et)));
            prop_assert_eq!(conflict_keys(r), Err(vec![key.clone()]));
        }

        /// The file form merges as the manifests do, its header included, and
        /// a conflicted merge marks every conflicted key's entries.
        #[test]
        fn merge_files_renders_merge3(
            b in keyed(), eo in edits(), et in edits(),
            headers in prop::array::uniform3(prop::sample::select(vec!["h1", "h2"])),
        ) {
            let (base, ours, theirs) = (
                manifest_of(&valued(b.clone())),
                manifest_of(&valued(apply(&b, &eo))),
                manifest_of(&valued(apply(&b, &et))),
            );
            let [hb, ho, ht] = headers;
            let merged = merge_files(&base.render(hb), &ours.render(ho), &theirs.render(ht));
            let header = merge_one(hb, ho, ht);
            match (merge3(&base, &ours, &theirs), header) {
                (Ok(m), Ok(h)) => prop_assert_eq!(merged, Ok(m.render(h))),
                (entries, header) => {
                    let Err(MergeFailure::Conflicts { header: h, keys, marked }) = merged else {
                        return Err(TestCaseError::fail(format!("expected conflicts, got {merged:?}")));
                    };
                    prop_assert_eq!(h, header.is_err());
                    prop_assert_eq!(keys.clone(), entries.err().unwrap_or_default());
                    for c in &keys {
                        for e in [&c.base, &c.ours, &c.theirs].into_iter().flatten() {
                            prop_assert!(marked.lines().any(|l| l == e), "{e:?} not in {marked}");
                        }
                    }
                    prop_assert_eq!(marked.matches("<<<<<<< ours").count(), keys.len() + usize::from(h));
                }
            }
        }

        /// A keyed manifest has one entry per key, and its values under each
        /// key are exactly the multiset it was given.
        #[test]
        fn from_keyed_keeps_each_keys_value_multiset(
            items in prop::collection::vec(("[a-c]", "[xy]{1,2}"), 0..12)
        ) {
            let m = Manifest::from_keyed(items.iter().map(|(k, v)| (k.to_string(), v.to_string())))
                .expect("valid items");
            let mut want: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
            for (k, v) in &items {
                *want.entry(k.clone()).or_default().entry(v.clone()).or_default() += 1;
            }
            let mut got: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
            for e in m.entries() {
                let mut fields = e.split(KEY_SEPARATOR);
                let key = fields.next().expect("a key").to_string();
                prop_assert_eq!(key_of(e), key.as_str());
                for f in fields {
                    let (v, n) = match f.rsplit_once(" x") {
                        Some((v, n)) if n.parse::<usize>().is_ok() => (v, n.parse().expect("checked")),
                        _ => (f, 1),
                    };
                    *got.entry(key.clone()).or_default().entry(v.to_string()).or_default() += n;
                }
            }
            prop_assert_eq!(got, want);
        }
    }

    #[test]
    fn keys_are_distinct() {
        assert_eq!(
            Manifest::from_entries(["a\tx".to_string(), "a\ty".to_string()]),
            Err(EntryError::DuplicateKey("a".to_string()))
        );
        assert_eq!(
            Manifest::from_entries(["a".to_string(), "a\ty".to_string()]),
            Err(EntryError::DuplicateKey("a".to_string()))
        );
        assert!(Manifest::from_entries(["a b\tx".to_string(), "a\ty".to_string()]).is_ok());
        assert!(matches!(
            Manifest::from_keyed([("a\tb".to_string(), "x".to_string())]),
            Err(EntryError::KeySeparator(_))
        ));
        assert!(matches!(
            Manifest::from_keyed([("a".to_string(), "x\ty".to_string())]),
            Err(EntryError::KeySeparator(_))
        ));
        assert!(matches!(
            Manifest::from_keyed([("a".to_string(), "x x2".to_string())]),
            Err(EntryError::ReservedCountSuffix(_))
        ));
    }

    proptest! {
        /// The driver merges only keyed manifests: an unkeyed entry is its own
        /// key, so a line one side rewrote in the old format and the other
        /// rekeyed would merge to both. Any side with one fails, naming it —
        /// a branch from before a manifest was keyed included.
        #[test]
        fn an_unkeyed_entry_on_any_side_fails_the_merge(
            b in keyed(), o in keyed(), t in keyed(), side in 0..3usize, bare in "[a-d]{1,2}"
        ) {
            let mut files = [b, o, t].map(|m| manifest_of(&valued(m)).render("h"));
            files[side].push_str(&format!("zz{bare}\n"));
            let want = [MergeSide::Base, MergeSide::Ours, MergeSide::Theirs][side];
            match merge_files(&files[0], &files[1], &files[2]) {
                Err(MergeFailure::Unkeyed { side, entry }) => {
                    prop_assert_eq!(side, want);
                    prop_assert_eq!(entry, format!("zz{bare}"));
                }
                other => prop_assert!(false, "expected Unkeyed, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_file_that_is_not_a_manifest_names_its_side() {
        assert!(matches!(
            merge_files("a\tx\n", "a\tx\n", "a\tx\na\ty\n"),
            Err(MergeFailure::Malformed {
                side: MergeSide::Theirs,
                ..
            })
        ));
    }

    /// Every checked-in manifest parses — keys distinct included — so the
    /// merge driver `.gitattributes` names for them can read each one.
    #[test]
    fn every_checked_in_manifest_parses() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut dirs: Vec<std::path::PathBuf> = vec![root.join("crates/corpus-diff/manifests")];
        for krate in std::fs::read_dir(root.join("crates")).expect("crates/") {
            dirs.push(krate.expect("entry").path().join("tests/manifests"));
        }
        let mut seen = 0;
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    let text = std::fs::read_to_string(&path).expect("utf-8 manifest");
                    if let Err((line, e)) = Manifest::parse(&text) {
                        panic!("{}:{line}: {e}", path.display());
                    }
                    seen += 1;
                }
            }
        }
        assert!(
            seen > 20,
            "found only {seen} manifests; did the layout move?"
        );
    }

    /// The merge driver only knows an item by its key, so every manifest
    /// `.gitattributes` gives it must key every entry: an unkeyed line both
    /// sides rewrote would merge to both new lines rather than conflict.
    #[test]
    fn every_merge_driver_manifest_is_keyed() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let attributes =
            std::fs::read_to_string(root.join(".gitattributes")).expect(".gitattributes");
        let mut named = 0;
        for line in attributes.lines() {
            let Some(path) = line.strip_suffix(" merge=borzoi-manifest") else {
                continue;
            };
            let text = std::fs::read_to_string(root.join(path))
                .unwrap_or_else(|e| panic!("{path}, named in .gitattributes: {e}"));
            let manifest =
                Manifest::parse(&text).unwrap_or_else(|(l, e)| panic!("{path}:{l}: {e}"));
            if let Some(e) = manifest.entries().iter().find(|e| key_of(e) == e.as_str()) {
                panic!("{path} has an unkeyed entry: {e:?}");
            }
            named += 1;
        }
        assert!(named >= 2, "the driver names only {named} manifests");
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
        assert_eq!(
            compare_with(&path, &m(&["a 1", "b 2"]), "regen", false),
            Ok(())
        );
        let msg = compare_with(&path, &m(&["a 1", "b 3"]), "regen-cmd", false)
            .expect_err("a moved entry must fail");
        assert!(msg.contains("- b 2\n+ b 3\n"), "{msg}");
        assert!(msg.contains("regen-cmd"), "{msg}");
        let missing = dir.path().join("absent.txt");
        let msg = compare_with(&missing, &m(&["a 1"]), "regen-cmd", false)
            .expect_err("a missing manifest must fail");
        assert!(msg.contains("regen-cmd"), "{msg}");

        assert_eq!(compare_with(&path, &m(&["c"]), "regen", true), Ok(()));
        assert_eq!(
            Manifest::parse(&std::fs::read_to_string(&path).unwrap()),
            Ok(m(&["c"]))
        );
    }
}
