//! The depth limit may latch at any safe point without breaking a production.
//!
//! Latching drains the rest of the input, so a production that is not ready
//! to see end of input next — one that bumped a token it looked ahead at and
//! then assumes the rest of what it saw — panics or drops bytes. The tree
//! bound latches far more often than the recursion bound ever did, and in the
//! middle of chains rather than only at extreme depth, so every safe point is
//! reachable. This drives the latch to each of them: the token soup of
//! `tests/all/parser_panic_sweep.rs`'s exhaustive triples (its `CORE`
//! alphabet), parsed with every low stop height, so the latch fires at the
//! first safe point past each height.

use super::*;

/// `parser_panic_sweep`'s `CORE` alphabet: the punctuation and keywords the
/// productions' guards branch on.
const CORE: &[&str] = &[
    "(", ")", "[", "]", "{", "}", "|", ".", "..", "*", "-", "&", "?", "_", "1", "x", "\n", " ",
    "match", "with", "let", "fun", "try", "if", "->", "=",
];

/// Parse `src` stopping at every height in `stops`; `Err` describes the first
/// panic or lost byte.
fn probe(src: &str, stops: std::ops::RangeInclusive<usize>) -> Result<(), String> {
    let symbols = HashSet::new();
    for stop in stops {
        for kind in [FileKind::Impl, FileKind::Sig] {
            let parsed = std::panic::catch_unwind(|| {
                parse_inner_stopping_at(src, &symbols, kind, LanguageVersion::Preview, stop)
            })
            .map_err(|_| format!("{src:?} as {kind:?} panicked stopping at height {stop}"))?;
            if parsed.root.text() != src {
                return Err(format!(
                    "{src:?} as {kind:?} lost bytes stopping at height {stop}"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn every_core_triple_survives_a_latch_at_every_low_height() {
    let mut failures = Vec::new();
    for a in CORE {
        for b in CORE {
            for c in CORE {
                let src = format!("{a}{b}{c}\n");
                if let Err(failure) = probe(&src, 0..=8) {
                    failures.push(failure);
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures, first: {:?}",
        failures.len(),
        failures.iter().take(10).collect::<Vec<_>>()
    );
}

/// Well-formed input reaches safe points the soup does not: chains, bodies,
/// members, signatures.
#[test]
fn well_formed_snippets_survive_a_latch_at_every_height() {
    for src in [
        "let x = f a b c\nlet y = x + 1 * 2 |> g\n",
        "let f x =\n  match x with\n  | Some (a, b) :: rest when a > 0 -> a.M().N.[0]\n  | _ -> 0\n",
        "type T() =\n  member _.M(x: int list) = x\n  static member N = 1.0<m * s>\n",
        "module M =\n  type R = { A: int; B: string }\n  let r = { A = 1; B = \"s\" }\n",
        "let x = seq { for i in 1 .. 10 do yield i }\nlet y = async { let! a = b in return a }\n",
        "let g = fun (a: 'T) -> _.Length\nlet h<'a when 'a : equality> (x: 'a) = x\n",
        "namespace N\nmodule M =\n  let x = if a then b elif c then d else e\n",
    ] {
        let symbols = HashSet::new();
        let full = parse_inner(src, &symbols, FileKind::Impl, LanguageVersion::Preview);
        assert!(full.errors.is_empty(), "{src:?}: {:?}", full.errors);
        // No tree is taller than it has nodes, so this reaches every height.
        let nodes = full.root.descendants().count();
        probe(src, 0..=nodes).unwrap_or_else(|failure| panic!("{failure}"));
    }
}

fn height(root: &SyntaxNode) -> usize {
    let (mut depth, mut height) = (0usize, 0usize);
    for event in root.preorder() {
        match event {
            crate::syntax::WalkEvent::Enter(_) => {
                depth += 1;
                height = height.max(depth);
            }
            crate::syntax::WalkEvent::Leave(_) => depth -= 1,
        }
    }
    height
}

/// An or-pattern whose operands are all missing: no operand reaches its own
/// safe point, so only the pattern-tail loop's check stops the run. It must
/// stop near the bound in the first parse, not fall back to a lower stop or to
/// giving the file up (each fallback costs a whole reparse).
#[test]
fn a_chain_of_missing_pattern_operands_stops_at_the_bound() {
    let src = format!("let f (a{}) = 0\n", " |".repeat(1018));
    let symbols = HashSet::new();
    let parsed = parse_inner(&src, &symbols, FileKind::Impl, LanguageVersion::Preview);
    let h = height(&parsed.root);
    assert!(h <= MAX_TREE_HEIGHT, "height {h}");
    assert!(
        h > TREE_HEIGHT_STOP / 2,
        "height {h}: the parse gave up far below the bound"
    );
    assert_eq!(parsed.root.text(), src.as_str());
}

/// The last resort keeps the whole source, losslessly, in a two-level tree.
#[test]
fn giving_a_file_up_whole_is_lossless_and_flat() {
    let src = "let x = f a b c\n";
    let symbols = HashSet::new();
    let raw: Vec<RawTok<'_>> = lex_with_symbols_full_trivia(src, &symbols).collect();
    let filtered = filter_collect(src, LanguageVersion::Preview, active_tokens(&raw)).tokens;
    let mut parser = Parser::new(src, raw, filtered);
    parser.give_up_whole(FileKind::Impl);
    let root = SyntaxNode::new_root(parser.builder.finish());
    assert_eq!(root.text(), src);
    assert_eq!(height(&root), 2);
    assert!(parser.depth_limit_hit);
}
