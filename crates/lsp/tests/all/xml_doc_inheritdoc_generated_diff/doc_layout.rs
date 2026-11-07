//! The documentation file's layout, around generated hierarchies.
//!
//! Roslyn's `XmlDocumentationProvider` reads a doc file node by node: it
//! indexes every `<member>` it reads whole (last one wins), never indexes one
//! nested inside another, and steps over a `<member>` directly after one it
//! read whole. The index reads the `<members>` of one `<doc>`. Where the two
//! readings could differ the expansion must decline; where they agree it
//! must expand as Roslyn does. This compiles a generated hierarchy, then
//! rewrites the compiler's `.xml` through a generated layout — the text
//! between entries, duplicated entries (identical or not), nested,
//! namespaced, empty and nameless `<member>`s, entries outside `<members>`,
//! and further `<doc>` sections in a `<span>` — and compares every entry.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;

use super::{
    DEEP_CASES, DEFAULT_CASES, Reach, assert_every_dimension_expands, assert_sound, cases,
    check_each, reach_by, sample,
};
use crate::common::inheritdoc_diff::{Census, Compared, Verdict};
use crate::xml_doc_inheritdoc_diff::{compare, fixture, fixture_without_core_library};

/// The text between two consecutive entries of `<members>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gap {
    /// The compiler's own: a newline and indentation.
    Newline,
    /// Nothing: the next entry starts where the last one ends.
    None,
    Comment,
    /// Non-whitespace text.
    Text,
}

impl Gap {
    fn text(self) -> &'static str {
        match self {
            Gap::Newline => "\n        ",
            Gap::None => "",
            Gap::Comment => "<!-- between -->",
            Gap::Text => "between",
        }
    }

    fn dim(self) -> Dim {
        match self {
            Gap::Newline => Dim::GapNewline,
            Gap::None => Dim::GapNone,
            Gap::Comment => Dim::GapComment,
            Gap::Text => Dim::GapText,
        }
    }
}

/// One rewrite of `<members>`. Entries are named by index, modulo the number
/// of entries that bear an `<inheritdoc>` or are written out in full — the
/// ones an expansion reads.
#[derive(Debug, Clone, Copy)]
enum Edit {
    /// A copy of entry `of` inserted before entry `at`, its text the same or
    /// rewritten.
    Duplicate {
        of: usize,
        at: usize,
        rewritten: bool,
    },
    /// A copy of entry `inner` inside entry `outer`, at its end.
    Nest {
        inner: usize,
        outer: usize,
        rewritten: bool,
    },
    /// A copy of entry `of`, in a namespace, before entry `at`.
    Namespaced { of: usize, at: usize },
    /// An empty `<member name="…"/>` of a key no symbol has, before entry `at`.
    Empty { at: usize },
    /// A `<member>` without a name, before entry `at`.
    Nameless { at: usize },
    /// A copy of entry `of` under `<doc>` but after `<members>`.
    Outside { of: usize, rewritten: bool },
}

/// A further `<doc>` section: copies of entries (rewritten or not), or
/// entries of keys no symbol has.
#[derive(Debug, Clone)]
struct Section {
    entries: Vec<SectionEntry>,
}

#[derive(Debug, Clone, Copy)]
enum SectionEntry {
    Copy { of: usize, rewritten: bool },
    Fresh,
}

#[derive(Debug, Clone)]
struct Layout {
    /// The gap before each of these entries; the compiler's newline before
    /// every other.
    gaps: Vec<(usize, Gap)>,
    edits: Vec<Edit>,
    /// Whether the file is wrapped in a `<span>` (implied by any section).
    span: bool,
    before: Vec<Section>,
    after: Vec<Section>,
}

/// A dimension of the layout, attached to the entries it touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Dim {
    /// The entry follows a gap of this kind.
    GapNewline,
    /// The entry directly follows another, so Roslyn steps over it.
    GapNone,
    GapComment,
    GapText,
    /// The entry has an identical copy in `<members>`.
    DuplicateSame,
    /// The entry has a differing copy in `<members>`.
    DuplicateRewritten,
    /// The entry holds a nested `<member>`.
    Nested,
    /// The entry has a copy nested in another, which Roslyn never indexes.
    NestedCopy,
    /// The file holds a namespaced `member`.
    Namespaced,
    /// The entry follows an empty `<member/>`.
    AfterEmpty,
    /// The entry follows a `<member>` without a name.
    AfterNameless,
    /// The entry has a copy outside `<members>`.
    Outside,
    /// The file is wrapped in a `<span>`.
    Spanned,
    /// The entry has a copy in another `<doc>` section.
    SectionCopy,
    /// Another `<doc>` section, of fresh keys only, follows the compiler's.
    SectionAfter,
    /// Another `<doc>` section precedes the compiler's.
    SectionBefore,
}

/// The dimensions some sampled entry must expand under.
const EXPANDING: [Dim; 9] = [
    Dim::GapNewline,
    Dim::GapComment,
    Dim::GapText,
    Dim::DuplicateSame,
    Dim::Nested,
    Dim::AfterEmpty,
    Dim::AfterNameless,
    Dim::Spanned,
    Dim::SectionAfter,
];

/// The dimensions under which Roslyn may read a key otherwise than the index
/// does, so the expansion declines: some sampled entry must reach the
/// comparison under each, and decline (as `ReadDifferently`, or over an
/// ambiguous entry).
const DECLINING: [Dim; 7] = [
    Dim::GapNone,
    Dim::DuplicateRewritten,
    Dim::NestedCopy,
    Dim::Namespaced,
    Dim::Outside,
    Dim::SectionCopy,
    Dim::SectionBefore,
];

fn gap() -> impl Strategy<Value = Gap> {
    prop_oneof![Just(Gap::None), Just(Gap::Comment), Just(Gap::Text)]
}

fn edit() -> impl Strategy<Value = Edit> {
    let i = || 0..64usize;
    prop_oneof![
        3 => (i(), i(), any::<bool>()).prop_map(|(of, at, rewritten)| Edit::Duplicate { of, at, rewritten }),
        2 => (i(), i(), any::<bool>()).prop_map(|(inner, outer, rewritten)| Edit::Nest { inner, outer, rewritten }),
        2 => i().prop_map(|at| Edit::Empty { at }),
        2 => i().prop_map(|at| Edit::Nameless { at }),
        1 => (i(), any::<bool>()).prop_map(|(of, rewritten)| Edit::Outside { of, rewritten }),
    ]
}

fn section() -> impl Strategy<Value = Section> {
    prop::collection::vec(
        prop_oneof![
            1 => (0..64usize, any::<bool>()).prop_map(|(of, rewritten)| SectionEntry::Copy { of, rewritten }),
            1 => Just(SectionEntry::Fresh),
        ],
        1..3,
    )
    .prop_map(|entries| Section { entries })
}

fn layout() -> impl Strategy<Value = Layout> {
    (
        prop::collection::vec((0..64usize, gap()), 0..7),
        prop::collection::vec(edit(), 1..6),
        // One namespaced `member` anywhere makes every key uncertain, so it
        // is rare.
        prop::option::weighted(0.15, (0..64usize, 0..64usize)),
        any::<bool>(),
        prop::collection::vec(section(), 0..2),
        prop::collection::vec(section(), 0..2),
    )
        .prop_map(
            |(gaps, mut edits, namespaced, span, before, after)| Layout {
                gaps,
                edits: {
                    edits.extend(namespaced.map(|(of, at)| Edit::Namespaced { of, at }));
                    edits
                },
                // Sections before the compiler's are rarer: they leave nothing
                // the index reads as Roslyn does.
                span,
                before: if span { before } else { Vec::new() },
                after,
            },
        )
}

/// One `<member>` of the compiled file: its key and text.
#[derive(Debug, Clone)]
struct Entry {
    key: String,
    text: String,
}

/// The compiler's entries, in order.
fn entries_of(xml: &str) -> Vec<Entry> {
    let doc = roxmltree::Document::parse(xml).expect("the compiler's doc file parses");
    let members = doc
        .descendants()
        .find(|n| n.has_tag_name("members"))
        .expect("the compiler writes <members>");
    members
        .children()
        .filter(|n| n.has_tag_name("member"))
        .map(|m| Entry {
            key: m.attribute("name").expect("a named entry").to_string(),
            text: xml[m.range()].to_string(),
        })
        .collect()
}

/// `text` with its own content told apart from the original's: the first
/// summary prefixed, or a remark added.
fn rewrite(text: &str, n: usize) -> String {
    if let Some(at) = text.find("<summary>") {
        let at = at + "<summary>".len();
        return format!("{}Rewritten {n}: {}", &text[..at], &text[at..]);
    }
    let open = text.find('>').expect("an element") + 1;
    format!(
        "{}<remarks>Rewritten {n}.</remarks>{}",
        &text[..open],
        &text[open..]
    )
}

/// `text`, an entry, with `inner` inserted before its end tag.
fn nest(text: &str, inner: &str) -> String {
    let end = text.rfind("</member>").expect("an entry with content");
    format!("{}{inner}{}", &text[..end], &text[end..])
}

/// The rewritten file, and the dimensions of each key it touches.
fn render(entries: &[Entry], l: &Layout) -> (String, BTreeMap<String, BTreeSet<Dim>>) {
    let n = entries.len();
    // The entries an expansion reads: indices name these, modulo their
    // number.
    let read: Vec<usize> = (0..n)
        .filter(|&i| {
            let t = &entries[i].text;
            t.contains("<inheritdoc") || t.contains("Summary of")
        })
        .collect();
    let read = if read.is_empty() {
        (0..n).collect()
    } else {
        read
    };
    let pick = |i: usize| read[i % read.len()];
    let mut touched: BTreeMap<String, BTreeSet<Dim>> = BTreeMap::new();
    let mut touch = |key: &str, d: Dim| {
        touched.entry(key.to_string()).or_default().insert(d);
    };
    let mut rewrites = 0usize;
    let fresh_text = |rewrites: &mut usize| {
        *rewrites += 1;
        format!(
            "<member name=\"T:Gen.Fresh{r}\"><summary>Fresh {r}.</summary></member>",
            r = *rewrites
        )
    };
    let copy = |e: &Entry, rewritten: bool, rewrites: &mut usize| {
        if rewritten {
            *rewrites += 1;
            rewrite(&e.text, *rewrites)
        } else {
            e.text.clone()
        }
    };
    // The texts of `<members>` in order, each with the key the next gap
    // attributes to it; insertions are keyed by the entry they precede.
    let mut texts: Vec<String> = entries.iter().map(|e| e.text.clone()).collect();
    let mut inserted: Vec<Vec<(String, Option<Dim>)>> = vec![Vec::new(); n];
    let mut outside: Vec<String> = Vec::new();
    let any_namespaced = l.edits.iter().any(|e| matches!(e, Edit::Namespaced { .. }));
    for edit in &l.edits {
        match *edit {
            Edit::Duplicate { of, at, rewritten } => {
                let e = &entries[pick(of)];
                inserted[pick(at)].push((copy(e, rewritten, &mut rewrites), None));
                touch(
                    &e.key,
                    if rewritten {
                        Dim::DuplicateRewritten
                    } else {
                        Dim::DuplicateSame
                    },
                );
            }
            Edit::Nest {
                inner,
                outer,
                rewritten,
            } => {
                let (e, o) = (&entries[pick(inner)], pick(outer));
                let inner_text = copy(e, rewritten, &mut rewrites);
                texts[o] = nest(&texts[o], &inner_text);
                touch(&e.key, Dim::NestedCopy);
                touch(&entries[o].key, Dim::Nested);
            }
            Edit::Namespaced { of, at } => {
                let e = &entries[pick(of)];
                let text = e.text.replacen("<member ", "<member xmlns=\"urn:x\" ", 1);
                inserted[pick(at)].push((text, None));
            }
            Edit::Empty { at } => {
                rewrites += 1;
                inserted[pick(at)].push((
                    format!("<member name=\"T:Gen.Empty{rewrites}\"/>"),
                    Some(Dim::AfterEmpty),
                ));
            }
            Edit::Nameless { at } => {
                inserted[pick(at)].push((
                    "<member><summary>Nameless.</summary></member>".to_string(),
                    Some(Dim::AfterNameless),
                ));
            }
            Edit::Outside { of, rewritten } => {
                let e = &entries[pick(of)];
                outside.push(copy(e, rewritten, &mut rewrites));
                touch(&e.key, Dim::Outside);
            }
        }
    }
    // Lay `<members>` out: each entry after what was inserted before it, a
    // gap (cycled) between any two items. An item's dimension (an empty or
    // nameless `<member>`'s) and the gap before an entry go to that entry.
    let mut members = String::new();
    let special: BTreeMap<usize, Gap> = l.gaps.iter().map(|&(i, g)| (pick(i), g)).collect();
    let mut pending: Vec<Dim> = Vec::new();
    let mut first = true;
    let mut put = |members: &mut String, text: &str, g: Gap, pending: &mut Vec<Dim>| {
        if !first {
            members.push_str(g.text());
            pending.push(g.dim());
        }
        first = false;
        members.push_str(text);
    };
    for (i, e) in entries.iter().enumerate() {
        let g = special.get(&i).copied().unwrap_or(Gap::Newline);
        for (text, dim) in &inserted[i] {
            put(&mut members, text, Gap::Newline, &mut pending);
            pending.extend(*dim);
        }
        put(&mut members, &texts[i], g, &mut pending);
        for d in pending.drain(..) {
            touch(&e.key, d);
        }
    }
    let section_text = |s: &Section, rewrites: &mut usize, touch: &mut dyn FnMut(&str, Dim)| {
        let mut out = String::from("<doc><assembly><name>Fx</name></assembly><members>");
        for entry in &s.entries {
            match *entry {
                SectionEntry::Copy { of, rewritten } => {
                    let e = &entries[pick(of)];
                    out.push_str(&copy(e, rewritten, rewrites));
                    touch(&e.key, Dim::SectionCopy);
                }
                SectionEntry::Fresh => out.push_str(&fresh_text(rewrites)),
            }
        }
        out.push_str("</members></doc>");
        out
    };
    let span = l.span || !l.before.is_empty() || !l.after.is_empty();
    let mut out = String::from("<?xml version=\"1.0\"?>\n");
    if span {
        out.push_str("<span>");
    }
    for s in &l.before {
        out.push_str(&section_text(s, &mut rewrites, &mut touch));
    }
    out.push_str(
        "<doc>\n    <assembly>\n        <name>Fx</name>\n    </assembly>\n    <members>\n        ",
    );
    out.push_str(&members);
    out.push_str("\n    </members>");
    for o in &outside {
        out.push_str(o);
    }
    out.push_str("\n</doc>");
    for s in &l.after {
        out.push_str(&section_text(s, &mut rewrites, &mut touch));
    }
    if span {
        out.push_str("</span>");
    }
    // File-wide dimensions.
    let mut file_wide: Vec<Dim> = Vec::new();
    if span {
        file_wide.push(Dim::Spanned);
    }
    if any_namespaced {
        file_wide.push(Dim::Namespaced);
    }
    if !l.before.is_empty() {
        file_wide.push(Dim::SectionBefore);
    }
    let fresh_only = |s: &Section| s.entries.iter().all(|e| matches!(e, SectionEntry::Fresh));
    if !l.after.is_empty() && l.after.iter().all(fresh_only) {
        file_wide.push(Dim::SectionAfter);
    }
    for e in entries {
        for d in &file_wide {
            touch(&e.key, *d);
        }
    }
    (out, touched)
}

#[derive(Debug, Clone)]
struct Case {
    program: super::Program,
    layout: Layout,
}

fn strategy() -> impl Strategy<Value = Case> {
    (super::program(), layout()).prop_map(|(program, layout)| Case { program, layout })
}

/// One case's comparisons, each with the layout dimensions of the entries it
/// read: its own and the candidates Roslyn names.
fn run(c: &Case) -> Vec<(Compared, BTreeSet<Dim>)> {
    let (source, _) = super::render(&c.program);
    let fx = if c.program.core {
        fixture(&source)
    } else {
        fixture_without_core_library(&source)
    }
    .unwrap_or_else(|e| panic!("the generated program does not compile: {e:#?}\n{source}"));
    let path = fx.dll.with_extension("xml");
    let compiled = std::fs::read_to_string(&path).unwrap();
    let (xml, touched) = render(&entries_of(&compiled), &c.layout);
    std::fs::write(&path, &xml).unwrap();
    let compared = compare(&fx);
    assert_sound(&compared, &format!("{source}\n--- doc file ---\n{xml}"));
    compared
        .into_iter()
        .map(|c| {
            let dims = std::iter::once(&c.key)
                .chain(&c.roslyn_candidates)
                .filter_map(|k| touched.get(k))
                .flatten()
                .copied()
                .collect();
            (c, dims)
        })
        .collect()
}

#[test]
fn generated_doc_layouts_expand_exactly_as_roslyn() {
    check_each(strategy(), cases(DEFAULT_CASES), |c| {
        run(&c);
    });
}

#[test]
#[ignore = "deep run: CI's <inheritdoc> sweep step"]
fn deep_generated_doc_layouts_expand_exactly_as_roslyn() {
    check_each(strategy(), cases(DEEP_CASES), |c| {
        run(&c);
    });
}

/// Every layout dimension under which Roslyn reads as the index does reaches
/// an expanded entry; every one under which it may not reaches a decline.
#[test]
fn the_doc_layout_generator_reaches_every_dimension() {
    let mut reach: BTreeMap<Dim, Reach> = BTreeMap::new();
    let mut declined: BTreeMap<Dim, usize> = BTreeMap::new();
    let mut census = Census::default();
    for case in sample(&strategy(), 64) {
        let compared = run(&case);
        census.add(&compared.iter().map(|(c, _)| c.clone()).collect::<Vec<_>>());
        for (c, dims) in compared {
            reach_by(&mut reach, dims.iter().copied(), &c.verdict);
            if matches!(&c.verdict, Verdict::Declined { .. }) {
                for d in &dims {
                    *declined.entry(*d).or_default() += 1;
                }
            }
        }
    }
    census.print("doc layouts (liveness sample)");
    eprintln!("  declined, per dimension: {declined:?}");
    assert_every_dimension_expands("doc layouts", &EXPANDING, &reach);
    let unreached: Vec<Dim> = DECLINING
        .into_iter()
        .filter(|d| declined.get(d).is_none_or(|n| *n == 0))
        .collect();
    assert!(
        unreached.is_empty(),
        "doc layouts: no sampled entry declined under {unreached:?}"
    );
}
