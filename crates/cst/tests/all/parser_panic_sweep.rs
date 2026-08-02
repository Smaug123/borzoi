//! Panic-freedom over adversarial input: no source, at any language version,
//! may make [`parse_with_options`] panic or drop bytes.
//!
//! The parser is hand-written recursive descent, and its productions guard
//! their recursions with per-token starter predicates. Several productions also
//! assert that the token they were handed is one they have an arm for — a
//! useful assertion exactly while it holds, and a dead parse the moment a
//! predicate and the production it gates disagree. The corpus gates cannot see
//! that class: they walk the F# compiler's own sources, which are well-formed
//! by construction, so every starter predicate is consulted on exactly the
//! input it was written for.
//!
//! This sweep supplies the other half — token soup that is not F#. It is not
//! trying to be realistic; it is trying to reach the disagreements, so its
//! alphabet is the punctuation and keywords the guards branch on, concatenated
//! without regard for whether the result means anything.
//!
//! Two properties per input, both universal:
//!
//! * **No panic.** Two callers contain their own parses — the LSP
//!   (`borzoi::cst_panic_safe`) and
//!   [`borzoi_cst::parser::diagnostics_are_version_invariant`], which parses at
//!   versions the caller never asked for — so the cost of one is a file with no
//!   diagnostics rather than a dead server. That is not a reason to tolerate
//!   one: a contained panic is still an answer withheld, and containment is
//!   defence in depth for a guard that turns out to be wrong, not a licence to
//!   leave it wrong. Note `borzoi_sema::SyntaxRecovery::of_guessed_version`
//!   calls straight in, with no wrapper of its own.
//! * **Lossless round-trip** (`root.text() == src`). The same recovery arms
//!   that must not panic must also not swallow the offending token, or the CST
//!   stops being a faithful record of the buffer. `parser_corpus` asserts this
//!   over real F#; the failure mode it cannot reach is a *recovery* arm, which
//!   only malformed input enters.
//!
//! **The version axis is load-bearing.** The guards are version-gated in places
//! (the lex filter's strict-indentation rules differ across F# 8), so a
//! generator that only exercises the default version tests one column of the
//! matrix. Every case here runs at all ten versions, and as both an
//! implementation and a signature file.
//!
//! The deterministic half — exhaustive pairs and triples — runs in the ordinary
//! suite, because a defect it can reach is reachable by a user today. The
//! fresh-seed half is `#[ignore]`d and mirrors `borzoi-nuget`'s soak: a fixed
//! seed that has passed once passes forever, so the long random walk takes a
//! *new* seed each run and prints it (`BORZOI_PARSER_PANIC_SEED=<seed>` to
//! reproduce).
//!
//! The parser keeps its other `unreachable!`s rather than converting them all
//! to recovery arms, and this sweep is why that is a defensible position: an
//! assertion that genuinely holds is worth more than a silent recovery, because
//! it turns a parser bug into a loud one. What makes it defensible is that
//! something *searches* — an assertion nothing tries to falsify is a claim, not
//! a proof. So the rule is: keep the assertion, and if this sweep reaches it,
//! that is the evidence it was never an invariant.

use std::collections::BTreeMap;
use std::collections::HashSet;

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};
use borzoi_oracle_harness::panic_silence::take_silenced_panic;

use crate::common::catch_unwind_silent;

/// Every (file kind, language version) the parser can be asked for.
///
/// The version axis is load-bearing because the guards are version-gated in
/// places. The file-kind axis is load-bearing because a `.fsi` is a different
/// grammar with its own productions and its own starter predicates, and the LSP
/// parses signature files on the same request paths as implementation ones — so
/// a sweep over `Impl` alone would leave half the parser unswept.
fn every_parse_setting() -> impl Iterator<Item = (FileKind, LanguageVersion)> {
    [FileKind::Impl, FileKind::Sig]
        .into_iter()
        .flat_map(|kind| {
            LanguageVersion::NUMBERED
                .into_iter()
                .chain([LanguageVersion::Preview])
                .map(move |lang| (kind, lang))
        })
}

/// The pieces exhaustive enumeration uses. Deliberately small enough that
/// *triples* are affordable, and chosen for the shapes the hand-written guards
/// branch on: bracket openers and closers (whose LexFilter-swallowed forms are
/// what strand a token past its owning production), the dotted and range heads,
/// the prefix operators, and the block-structure keywords whose recovery arms
/// re-enter the expression cycle.
///
/// `"try)..\n"` is a member of this product — a keyword, a closer the lex
/// filter swallows, and a range head with nothing to bind to — and that is the
/// shape this alphabet is sized to reach.
const CORE: &[&str] = &[
    "(", ")", "[", "]", "{", "}", "|", ".", "..", "*", "-", "&", "?", "_", "1", "x", "\n", " ",
    "match", "with", "let", "fun", "try", "if", "->", "=",
];

/// `CORE` plus the wider surface the random walk draws on: the remaining
/// bracket families, the string and quotation openers, the type-relation and
/// pipeline operators, and the declaration keywords. Too large to enumerate
/// exhaustively past pairs, which is what the random half is for.
const WIDE: &[&str] = &[
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    "[|",
    "|]",
    "{|",
    "|}",
    "|",
    "||",
    ".",
    "..",
    "..^",
    ".[",
    ",",
    ";",
    ":",
    "::",
    "->",
    "<-",
    "=",
    "*",
    "+",
    "-",
    "%",
    "?",
    "??",
    "&",
    "&&",
    "@",
    "^",
    "!",
    "~",
    "'",
    "\"",
    "\"\"\"",
    "$\"",
    "#",
    "<@",
    "@>",
    "<",
    ">",
    "<<",
    ">>",
    "|>",
    ":>",
    ":?",
    ":?>",
    "[<",
    ">]",
    "(*",
    "*)",
    "//",
    "`",
    "1",
    "0x1",
    "1.0",
    "1L",
    "'a'",
    "\"s\"",
    "_",
    "x",
    "X.Y",
    "let",
    "in",
    "match",
    "with",
    "when",
    "fun",
    "function",
    "if",
    "then",
    "else",
    "do",
    "type",
    "module",
    "namespace",
    "open",
    "member",
    "static",
    "new",
    "inherit",
    "for",
    "to",
    "while",
    "try",
    "finally",
    "rec",
    "and",
    "not",
    "null",
    "true",
    "false",
    "begin",
    "end",
    "struct",
    "class",
    "interface",
    "abstract",
    "override",
    "val",
    "mutable",
    "global",
    "base",
    "yield",
    "return",
    "use",
    "lazy",
    "assert",
    "downcast",
    "upcast",
    "of",
    "as",
    "sig",
    "delegate",
    "\n",
    " ",
    "  ",
    "\t",
];

/// One failing input, keyed by *cause* rather than by spelling: a sweep that
/// reported every source reaching one broken guard would print thousands of
/// lines describing a single defect.
#[derive(Debug)]
struct Failure {
    /// `file:line` of the assertion that fired, or the round-trip marker.
    cause: String,
    /// The first source that produced it, and the setting it produced it under.
    witness: String,
}

/// What a run of [`probe`] observed.
#[derive(Default)]
struct Census {
    /// (source, file kind, version) triples parsed.
    checked: usize,
    /// Parses that reported at least one error. This sweep asserts an
    /// *absence*, and an absence over inputs the productions accept without
    /// ever entering a recovery arm is free — so a run in which nothing errored
    /// has stopped testing what it claims to.
    with_errors: usize,
    /// Parses that entered the constant-literal dispatch's recovery arm — the
    /// specific arm the known regression reached. Tracked separately because
    /// only sources long enough to strand a token there can reach it.
    reached_const_recovery: usize,
    /// First witness of each distinct failure cause.
    failures: BTreeMap<String, Failure>,
}

/// The message `Parser::parse_const_payload`'s recovery arm reports. Spelled
/// out here so a rename fails this sweep's non-vacuity check rather than
/// silently switching it off.
const CONST_RECOVERY_MESSAGE: &str = "expected a constant literal";

/// Parse `src` under every setting, folding what happened into `census`.
fn probe(src: &str, census: &mut Census) {
    for (file_kind, lang) in every_parse_setting() {
        census.checked += 1;
        let symbols = HashSet::new();
        let parsed = catch_unwind_silent(|| {
            parse_with_options(
                src,
                ParseOptions {
                    file_kind,
                    symbols: &symbols,
                    lang,
                },
            )
        });
        let cause = match parsed {
            Err(_) => {
                let silenced = take_silenced_panic();
                let (location, message) = silenced
                    .map(|p| (p.location, p.message))
                    .unwrap_or_else(|| ("<unknown>".to_string(), String::new()));
                // The message is truncated because several assertions
                // interpolate the offending token, which would split one cause
                // into a bucket per token.
                let message: String = message.chars().take(60).collect();
                format!("panic at {location}: {message}")
            }
            Ok(parse) => {
                if !parse.errors.is_empty() {
                    census.with_errors += 1;
                }
                if parse
                    .errors
                    .iter()
                    .any(|e| e.message == CONST_RECOVERY_MESSAGE)
                {
                    census.reached_const_recovery += 1;
                }
                if parse.root.text() == src {
                    continue;
                }
                "lossless round-trip violated (root.text() != source)".to_string()
            }
        };
        census
            .failures
            .entry(cause.clone())
            .or_insert_with(|| Failure {
                cause,
                witness: format!("{src:?} as {file_kind:?} at {lang}"),
            });
    }
}

/// Fail with the whole census rather than the first witness: a sweep that
/// stopped at case one would need as many runs as there are defects, and the
/// count is the number a reader needs to judge whether a change made things
/// better or worse.
fn assert_no_failures(what: &str, census: &Census) {
    assert!(census.checked > 0, "{what}: nothing was parsed");
    assert!(
        census.with_errors > 0,
        "{what}: {} parses, none of which reported an error — the alphabet no \
         longer reaches a recovery arm, so the absence below is free",
        census.checked
    );
    if census.failures.is_empty() {
        eprintln!(
            "{what}: {} (source, kind, version) parses, {} erroring, {} reaching \
             constant recovery, no failures",
            census.checked, census.with_errors, census.reached_const_recovery
        );
        return;
    }
    let mut report = format!(
        "{what}: {} distinct failure cause(s) across {} (source, kind, version) parses:\n",
        census.failures.len(),
        census.checked
    );
    for f in census.failures.values() {
        report.push_str(&format!("  {}\n    first seen: {}\n", f.cause, f.witness));
    }
    panic!("{report}");
}

/// Every one- and two-piece source over [`WIDE`], under every setting.
///
/// Pairs are cheap enough to enumerate over the full alphabet, and they are
/// where a production that consumes nothing before recursing shows up.
#[test]
fn every_wide_pair_parses_without_panicking() {
    let mut census = Census::default();
    for a in WIDE {
        probe(&format!("{a}\n"), &mut census);
        for b in WIDE {
            probe(&format!("{a}{b}\n"), &mut census);
        }
    }
    assert_no_failures("wide pairs", &census);
}

/// Every three-piece source over [`CORE`], under every setting.
///
/// Three is the shortest length that strands a token in constant position: a
/// keyword to open a production, a closer for the lex filter to swallow, and a
/// token the re-entered production has no arm for. Pairs miss it entirely,
/// which is why this length is enumerated rather than sampled.
#[test]
fn every_core_triple_parses_without_panicking() {
    let mut census = Census::default();
    for a in CORE {
        for b in CORE {
            for c in CORE {
                probe(&format!("{a}{b}{c}\n"), &mut census);
            }
        }
    }
    assert_no_failures("core triples", &census);
    // This length is enumerated rather than sampled precisely because it is the
    // shortest that strands a token in constant position; if it stops doing so,
    // the enumeration is no longer buying what it costs.
    assert!(
        census.reached_const_recovery > 0,
        "core triples no longer reach the constant-literal recovery arm"
    );
}

/// The shapes that have actually broken the parser, pinned by spelling.
///
/// The sweeps above subsume these, but a sweep is a search and this is a
/// record: if the alphabet is ever narrowed, or the enumeration bounded to keep
/// it affordable, these keep costing what they cost.
#[test]
fn the_known_panic_witnesses_parse() {
    let mut census = Census::default();
    for src in [
        // Found by `shape_sensitivity`'s `adversarial_soup`: the lex filter
        // swallows the `)`, which switches off the range production's claim on
        // the `..`, and the expression cycle re-enters the atom dispatch with a
        // token no constant-literal arm has.
        "match)..\n",
        "try)..\n",
        // The same defect reached through a longer buffer; kept because it is
        // the spelling the version axis was first observed on.
        ">(match|.\n>\nmatch}..",
    ] {
        probe(src, &mut census);
    }
    assert_no_failures("known witnesses", &census);
    // Every witness here is a source that reached the constant-literal arm
    // through the atom dispatch. Pinning the spelling is worthless if the arm
    // it pins is no longer the one it enters.
    assert!(
        census.reached_const_recovery > 0,
        "the known witnesses no longer reach the constant-literal recovery arm"
    );
}

/// A fresh-seed random walk over [`WIDE`] at lengths the exhaustive halves
/// cannot reach.
///
/// A fixed seed that has passed once passes forever, so this takes a new one
/// each run and prints it. Reproduce a failure with
/// `BORZOI_PARSER_PANIC_SEED=<seed>`.
#[test]
#[ignore = "fresh-seed sweep; run explicitly when touching the parser"]
fn fresh_seed_random_sources_parse_without_panicking() {
    let seed = std::env::var("BORZOI_PARSER_PANIC_SEED")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(|| {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after the epoch")
                .as_nanos() as u64
        });
    let cases: usize = std::env::var("BORZOI_PARSER_PANIC_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30_000);
    eprintln!("parser panic sweep seed: {seed} (cases={cases})");

    let mut rng = SplitMix64(seed);
    let mut census = Census::default();
    for _ in 0..cases {
        let len = 3 + (rng.next() % 10) as usize;
        let mut src = String::new();
        for _ in 0..len {
            src.push_str(WIDE[(rng.next() % WIDE.len() as u64) as usize]);
        }
        src.push('\n');
        probe(&src, &mut census);
    }
    assert_no_failures(&format!("fresh seed {seed}"), &census);
}

/// SplitMix64 — the same generator `borzoi-nuget`'s soak uses. Reproducible
/// from the seed alone, which is the whole contract with the printed line.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}
