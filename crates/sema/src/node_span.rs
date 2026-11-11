//! The span FCS reports for a syntax node: its range with the leading and
//! trailing **trivia** (whitespace, comments) trimmed.
//!
//! A node's raw `text_range()` can include trivia the parser attached *inside*
//! the node — e.g. the space after `->` before a lambda body's `if` becomes a
//! leading child of the `IF_THEN_ELSE_EXPR` — which would be off-by-one against
//! FCS's keyword-anchored range. So the span runs from the node's first
//! non-trivia token to its last.
//!
//! Inference asks for the span of nodes at every depth of a tall tree, and
//! finding a node's first or last token is a walk down one edge of it: down
//! the right spine of an `elif` chain, the left spine of a left-associative
//! operator chain. Asked afresh at every node, that is quadratic in the tree's
//! height. [`NodeSpans`] remembers, for each node one of its walks passed
//! through, which token that edge of the node ends at, so each node is walked
//! into at most once per edge however many spans are asked of it.

use std::collections::HashMap;

use borzoi_cst::syntax::{SyntaxElement, SyntaxNode, SyntaxToken};
use rowan::{NodeOrToken, TextRange};

/// The spans of the nodes of one tree, remembered as they are found.
#[derive(Default)]
pub(crate) struct NodeSpans {
    /// Each node's first non-trivia token's range, or `None` if it has none.
    first: HashMap<SyntaxNode, Option<TextRange>>,
    /// Each node's last non-trivia token's range, or `None` if it has none.
    last: HashMap<SyntaxNode, Option<TextRange>>,
}

/// Which edge of a node a walk looks for its token at.
#[derive(Clone, Copy)]
enum Edge {
    First,
    Last,
}

impl Edge {
    fn child(self, node: &SyntaxNode) -> Option<SyntaxElement> {
        match self {
            Edge::First => node.first_child_or_token(),
            Edge::Last => node.last_child_or_token(),
        }
    }

    fn sibling(self, el: &SyntaxElement) -> Option<SyntaxElement> {
        match self {
            Edge::First => el.next_sibling_or_token(),
            Edge::Last => el.prev_sibling_or_token(),
        }
    }
}

impl NodeSpans {
    /// `node`'s range from its first non-trivia token to its last; its raw
    /// range if it has no non-trivia token (never for a well-formed
    /// construct). Equal to `node_span_by_scan`, which reads every token.
    pub(crate) fn span(&mut self, node: &SyntaxNode) -> TextRange {
        match (
            self.edge_token(node, Edge::First),
            self.edge_token(node, Edge::Last),
        ) {
            (Some(first), Some(last)) => TextRange::new(first.start(), last.end()),
            (None, None) => node.text_range(),
            (first, last) => unreachable!(
                "a node with a non-trivia token has one at each end: {first:?} / {last:?}"
            ),
        }
    }

    /// The range of `node`'s non-trivia token nearest `edge`.
    ///
    /// Walks the node's elements from that edge inward, depth first, skipping
    /// trivia, and stops at the first non-trivia token. Every node the walk
    /// enters is *pending* until the walk leaves it: the token it stops at is
    /// each pending node's own nearest token too (all that precedes it in each
    /// was trivia or empty), and a node the walk leaves without one has none.
    /// Both are remembered, and a remembered node is answered without being
    /// entered, so no node is entered twice for one edge. The pending nodes
    /// sit on the heap, so the walk's stack does not grow with the tree.
    fn edge_token(&mut self, node: &SyntaxNode, edge: Edge) -> Option<TextRange> {
        let memo = match edge {
            Edge::First => &mut self.first,
            Edge::Last => &mut self.last,
        };
        if let Some(&known) = memo.get(node) {
            return known;
        }
        let mut pending: Vec<SyntaxNode> = vec![node.clone()];
        let mut at = edge.child(node);
        let found = loop {
            match at {
                None => {
                    // The innermost pending node is exhausted: it has no
                    // non-trivia token, so look on past it in its parent.
                    let done = pending.pop().expect("the walk has a node open");
                    let next = edge.sibling(&NodeOrToken::Node(done.clone()));
                    memo.insert(done, None);
                    if pending.is_empty() {
                        return None;
                    }
                    at = next;
                }
                Some(NodeOrToken::Token(t)) if !is_trivia(&t) => break t.text_range(),
                Some(el @ NodeOrToken::Token(_)) => at = edge.sibling(&el),
                Some(NodeOrToken::Node(n)) => match memo.get(&n) {
                    Some(&Some(known)) => break known,
                    Some(None) => at = edge.sibling(&NodeOrToken::Node(n)),
                    None => {
                        at = edge.child(&n);
                        pending.push(n);
                    }
                },
            }
        };
        for open in pending {
            memo.insert(open, Some(found));
        }
        Some(found)
    }
}

fn is_trivia(t: &SyntaxToken) -> bool {
    t.kind().is_trivia()
}

/// [`NodeSpans::span`] by reading every token under `node`: the reference it
/// is held to.
#[cfg(test)]
fn node_span_by_scan(node: &SyntaxNode) -> TextRange {
    let mut toks = node
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !is_trivia(t));
    match toks.next() {
        Some(first) => {
            let end = toks
                .last()
                .unwrap_or_else(|| first.clone())
                .text_range()
                .end();
            TextRange::new(first.text_range().start(), end)
        }
        None => node.text_range(),
    }
}

#[cfg(test)]
mod tests {
    use borzoi_cst::parser::{parse, parse_sig};
    use borzoi_oracle_harness::repetition_shapes::{SHAPES, nested_in_modules_at};
    use proptest::prelude::*;

    use super::*;

    /// Every node of `root`, in document order.
    fn nodes(root: &SyntaxNode) -> Vec<SyntaxNode> {
        root.descendants().collect()
    }

    /// Asks one [`NodeSpans`] for the span of every node of `root`, in the
    /// order `order` gives, and holds each answer to the scan's.
    fn assert_spans_match(root: &SyntaxNode, order: &[usize]) -> Result<(), TestCaseError> {
        let nodes = nodes(root);
        let mut spans = NodeSpans::default();
        for &i in order {
            let node = &nodes[i % nodes.len()];
            prop_assert_eq!(
                spans.span(node),
                node_span_by_scan(node),
                "{:?} at {:?} in {:?}",
                node.kind(),
                node.text_range(),
                root.text().to_string()
            );
        }
        Ok(())
    }

    /// The orders a tree's spans are asked in: outermost first, innermost
    /// first, and each node asked twice in a row — a memo filled by one order
    /// answers differently from one filled by another if it is wrong.
    fn fixed_orders(n: usize) -> [Vec<usize>; 3] {
        [
            (0..n).collect(),
            (0..n).rev().collect(),
            (0..n).flat_map(|i| [i, i]).collect(),
        ]
    }

    #[test]
    fn spans_match_the_scan_on_every_repetition_shape() {
        for shape in SHAPES {
            for src in [
                shape.source(1),
                shape.source(7),
                nested_in_modules_at(&shape.source(3), 5),
            ] {
                let root = match shape.kind {
                    borzoi_oracle_harness::repetition_shapes::Kind::Impl => parse(&src).root,
                    borzoi_oracle_harness::repetition_shapes::Kind::Sig => parse_sig(&src).root,
                };
                for order in fixed_orders(nodes(&root).len()) {
                    assert_spans_match(&root, &order)
                        .unwrap_or_else(|e| panic!("{}: {e}", shape.name));
                }
            }
        }
    }

    /// Fragments that stack up into deep, broken and trivia-laden trees: every
    /// bracket and operator form the edges walk down, the trivia they skip
    /// (comments, blank lines, indentation) at every position, and enough
    /// unbalanced input that recovery leaves empty and error nodes behind.
    const FRAGMENTS: &[&str] = &[
        "let x = ",
        "let f a b = ",
        "a",
        "b",
        "f",
        "M.N.x",
        "1",
        "\"s\"",
        " ",
        "\n",
        "\n    ",
        "\n  ",
        "// c\n",
        "(* c *)",
        "(",
        ")",
        "[",
        "]",
        "[|",
        "|]",
        "{",
        "}",
        "<",
        ">",
        "+",
        "-",
        "*",
        "|>",
        "<|",
        ">>",
        "::",
        "@",
        "=",
        ":=",
        "&&",
        "||",
        ",",
        ";",
        ".",
        ".[",
        "?",
        "if a then ",
        " elif b then ",
        " else ",
        "match x with ",
        "| ",
        " -> ",
        "fun x -> ",
        "begin ",
        " end",
        "do ",
        "x.M(",
        "f<int>",
        "#if A\n",
        "#else\n",
        "#endif\n",
        "type T = ",
        "member _.M = ",
        "module M =\n",
        "open System\n",
        "in ",
        "yield ",
        "seq { ",
        "async { ",
    ];

    fn source() -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(FRAGMENTS), 0..60).prop_map(|fs| fs.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn spans_match_the_scan_on_generated_trees(
            src in source(),
            sig in any::<bool>(),
            order in prop::collection::vec(any::<usize>(), 0..200),
        ) {
            let root = if sig { parse_sig(&src).root } else { parse(&src).root };
            for fixed in fixed_orders(nodes(&root).len()) {
                assert_spans_match(&root, &fixed)?;
            }
            assert_spans_match(&root, &order)?;
        }
    }
}
