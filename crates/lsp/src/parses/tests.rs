//! Memo ≡ recompute (`docs/incremental-lsp-plan.md`, D7): over generated
//! sequences of lookups, every parse [`ParseMemo::parse`] hands back equals a
//! fresh [`SourceParse::new`] of the same inputs, and the memo parses exactly
//! when a model that remembers each file's last text under each set of inputs
//! says it holds no parse of this one.
//!
//! The sequences revisit texts, so a text is often looked up again after the
//! memo replaced its parse with another's (the ABA case, where a stale hit
//! would hide), and they spell one file two ways and look up byte-equal texts
//! in fresh allocations.

use std::cell::RefCell;
use std::collections::BTreeSet;

use borzoi_cst::syntax::Structure;
use proptest::prelude::*;

use super::*;
use crate::diagnostics::{FileDiagnostics, syntactic_diagnostics};

/// The files a lookup names: one spelt in two cases (one key on a
/// case-folding platform, two elsewhere), one spelt plainly and through a `.`
/// and a `..`, and none.
const PATHS: [Option<&str>; 5] = [
    Some("/w/A.fs"),
    Some("/w/a.fs"),
    Some("/w/./sub/../B.fs"),
    Some("/w/B.fs"),
    None,
];

/// Texts whose parses differ by the symbols (`#if X`), the language version
/// (`#elif`, the offside rule), the grammar (a signature), and the
/// preprocessor's report (a lex error, an unclosed `#if`, a `#line`); and one
/// edit of the first.
const TEXTS: &[&str] = &[
    "module A\n\nlet a = 1\n",
    "module A\n\nlet a = 1\n// typed\n",
    "let x = \"unterminated\n#line 7 \"g.fs\"\nlet y = (\n",
    "#if X\nlet a = 1\n#else\nlet a = `\n#endif\n#if\n",
    "#if X\n#elif Y\nlet q = 1\n#endif\n",
    "type T =\n  abstract M : int -> int\nval f : int\n",
    "let f x =\n  match x with\n  | _ ->\n  ()\n",
];

fn symbol_set(i: usize) -> HashSet<String> {
    match i {
        0 => HashSet::new(),
        _ => HashSet::from(["X".to_string()]),
    }
}

const LANGS: [LanguageVersion; 3] = [
    LanguageVersion::MIN,
    LanguageVersion::DEFAULT,
    LanguageVersion::MAX,
];

#[derive(Clone, Debug)]
struct Lookup {
    path: usize,
    text: usize,
    /// Whether the text is a fresh allocation rather than the case's shared
    /// one for that text.
    fresh: bool,
    signature: bool,
    symbols: usize,
    lang: usize,
}

fn lookup() -> impl Strategy<Value = Lookup> {
    (
        0..PATHS.len(),
        0..TEXTS.len(),
        any::<bool>(),
        prop::bool::weighted(0.2),
        0..2usize,
        0..LANGS.len(),
    )
        .prop_map(|(path, text, fresh, signature, symbols, lang)| Lookup {
            path,
            text,
            fresh,
            signature,
            symbols,
            lang,
        })
}

/// A parse as its readers see it: the tree by shape, kinds and text, and
/// everything else by value, with the diagnostics read from it.
type Observed = (
    String,
    FileKind,
    BTreeSet<String>,
    LanguageVersion,
    Option<(
        Structure,
        Vec<borzoi_cst::parser::ParseError>,
        Vec<borzoi_cst::parser::ParseError>,
        LanguageVersion,
        bool,
        Preprocessed,
    )>,
    Preprocessed,
    Vec<FileDiagnostics>,
);

fn observed(parse: &SourceParse) -> Observed {
    (
        parse.text().to_string(),
        parse.file_kind(),
        parse.symbols().iter().cloned().collect(),
        parse.lang(),
        parse.parse().map(|p| {
            (
                p.root.structure(),
                p.errors.clone(),
                p.warnings.clone(),
                p.lang,
                p.shape_depends_on_language_version,
                p.preprocessor.clone(),
            )
        }),
        parse.preprocessed().into_owned(),
        syntactic_diagnostics(parse),
    )
}

/// What the generated lookups reached, summed over cases.
#[derive(Debug, Default)]
struct Exercised {
    /// Hits on the allocation the memo holds.
    pointer_hits: u64,
    /// Hits on a byte-equal text in another allocation.
    byte_hits: u64,
    /// Misses that replaced a parse under the same inputs.
    replaced: u64,
    /// Misses on a text the memo once held under the same inputs, since
    /// replaced: the ABA case.
    aba: u64,
    /// Lookups naming no file.
    unremembered: u64,
    /// Lookups under a second set of inputs for a file.
    second_inputs: u64,
}

type Inputs = (bool, usize, usize);

fn run(lookups: &[Lookup], exercised: &mut Exercised) -> Result<(), TestCaseError> {
    let shared: Vec<Arc<str>> = TEXTS.iter().map(|t| Arc::from(*t)).collect();
    let mut memo = ParseMemo::default();
    // The model: per file key and inputs, the last text parsed, and every
    // text ever parsed.
    let mut last: HashMap<(String, Inputs), usize> = HashMap::new();
    let mut ever: HashMap<(String, Inputs), BTreeSet<usize>> = HashMap::new();
    for l in lookups {
        let text = if l.fresh {
            Arc::from(TEXTS[l.text])
        } else {
            Arc::clone(&shared[l.text])
        };
        let file_kind = if l.signature {
            FileKind::Sig
        } else {
            FileKind::Impl
        };
        let symbols = symbol_set(l.symbols);
        let lang = LANGS[l.lang];
        let path = PATHS[l.path].map(Path::new);
        let inputs = (l.signature, l.symbols, l.lang);
        let key = path.map(|p| path_dedup_key(&lexically_normalize(p)));
        let held = key
            .as_ref()
            .and_then(|k| last.get(&(k.clone(), inputs)))
            .copied();
        let expect_hit = held == Some(l.text);

        let before = memo.counts;
        let got = memo.parse(path, &text, file_kind, &symbols, lang);
        let after = memo.counts;
        let want = SourceParse::new(Arc::clone(&text), file_kind, &symbols, lang);
        prop_assert_eq!(observed(&got), observed(&want), "{:?}", l);
        prop_assert_eq!(
            (after.reused - before.reused, after.parsed - before.parsed),
            if expect_hit { (1, 0) } else { (0, 1) },
            "{:?}: the model {} a parse of this text",
            l,
            if expect_hit { "holds" } else { "holds no" }
        );

        match &key {
            None => exercised.unremembered += 1,
            Some(k) => {
                let seen = ever.entry((k.clone(), inputs)).or_default();
                if expect_hit {
                    if Arc::ptr_eq(got.text(), &text) {
                        exercised.pointer_hits += 1;
                    } else {
                        exercised.byte_hits += 1;
                    }
                } else {
                    if held.is_some() {
                        exercised.replaced += 1;
                    }
                    if seen.contains(&l.text) {
                        exercised.aba += 1;
                    }
                }
                if seen.is_empty() && ever.keys().any(|(k2, i2)| k2 == k && *i2 != inputs) {
                    exercised.second_inputs += 1;
                }
                ever.entry((k.clone(), inputs)).or_default().insert(l.text);
                last.insert((k.clone(), inputs), l.text);
            }
        }
        // The memo holds one parse per file and inputs, no more.
        for parses in memo.files.values() {
            for (i, a) in parses.iter().enumerate() {
                for b in &parses[i + 1..] {
                    prop_assert!(
                        !a.has_inputs(b.file_kind, &b.symbols, b.lang),
                        "two parses under the same inputs"
                    );
                }
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn the_memo_hands_back_what_a_fresh_parse_computes(
        lookups in prop::collection::vec(lookup(), 1..40),
    ) {
        run(&lookups, &mut Exercised::default())?;
    }
}

/// The property above must reach the cases it exists for: over a fixed
/// seed's worth of generated sequences, hits on the held allocation and on a
/// byte-equal copy, a parse replaced under the same inputs, a text looked up
/// again after its parse was replaced, a lookup naming no file, and a file
/// read under a second set of inputs.
#[test]
fn the_generator_reaches_every_kind_of_lookup() {
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: 64,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[11; 32]),
    );
    let exercised = RefCell::new(Exercised::default());
    runner
        .run(&prop::collection::vec(lookup(), 1..40), |lookups| {
            run(&lookups, &mut exercised.borrow_mut())
        })
        .unwrap();
    let e = exercised.into_inner();
    eprintln!("{e:?}");
    assert!(
        e.pointer_hits > 0
            && e.byte_hits > 0
            && e.replaced > 0
            && e.aba > 0
            && e.unremembered > 0
            && e.second_inputs > 0,
        "{e:?}"
    );
}

/// The counts are per generation: beginning one forgets the last one's, and
/// a lookup between two generations counts in neither.
#[test]
fn the_counts_are_per_generation() {
    let mut memo = ParseMemo::default();
    let text: Arc<str> = Arc::from("let a = 1\n");
    let path = Some(Path::new("/w/A.fs"));
    let symbols = HashSet::new();
    let lang = LanguageVersion::DEFAULT;
    memo.begin_generation();
    memo.parse(path, &text, FileKind::Impl, &symbols, lang);
    memo.parse(path, &text, FileKind::Impl, &symbols, lang);
    assert_eq!(
        memo.end_generation(),
        ParseCounts {
            parsed: 1,
            reused: 1
        }
    );
    memo.parse(path, &text, FileKind::Impl, &symbols, lang);
    memo.begin_generation();
    memo.parse(path, &text, FileKind::Impl, &symbols, lang);
    assert_eq!(
        memo.end_generation(),
        ParseCounts {
            parsed: 0,
            reused: 1
        }
    );
}
