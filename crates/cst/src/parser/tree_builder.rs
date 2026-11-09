//! [`TreeBuilder`]: rowan's [`GreenNodeBuilder`] with the height of the tree
//! it is building kept alongside.
//!
//! The parser builds many nested nodes from a loop rather than a recursive
//! call — an application chain `f a a a`, a left-associative operator chain, a
//! postfix type application `int list list` — by wrapping what it has built
//! so far under a fresh node ([`TreeBuilder::start_node_at`]). Such a chain is
//! as deep as it is long, and rowan frees a green tree recursively, so a long
//! enough one overflows the stack when the tree is dropped (and so does any
//! consumer that walks the tree recursively). Counting at the builder bounds
//! every such shape at once, whichever production built it: the parser reads
//! [`TreeBuilder::height_if_closed_now`] and stops past
//! [`super::MAX_TREE_HEIGHT`].

use rowan::{GreenNode, GreenNodeBuilder};

/// A position in the pending children, from [`TreeBuilder::checkpoint`]: the
/// point [`TreeBuilder::start_node_at`] wraps from.
#[derive(Clone, Copy, Debug)]
pub(super) struct Checkpoint {
    rowan: rowan::Checkpoint,
    /// The pending-child index `rowan` names (rowan's own is opaque).
    index: usize,
}

/// A [`GreenNodeBuilder`] that also tracks the height of what it builds.
///
/// Heights count nodes on a root-to-leaf path: a token is 0, a node is one more
/// than its tallest child. `heights` mirrors rowan's pending-children stack
/// element for element, and `open` mirrors its open-node stack (each entry the
/// index of that node's first pending child), so every operation updates both
/// exactly as rowan updates its own.
pub(super) struct TreeBuilder {
    inner: GreenNodeBuilder<'static>,
    heights: Vec<u32>,
    open: Vec<usize>,
    /// The tallest pending element. Every finished node is a pending element or
    /// sits inside one (and is shorter than it), so this is also the tallest
    /// node finished so far.
    tallest: u32,
}

impl TreeBuilder {
    pub(super) fn new() -> Self {
        Self {
            inner: GreenNodeBuilder::new(),
            heights: Vec::new(),
            open: Vec::new(),
            tallest: 0,
        }
    }

    pub(super) fn token(&mut self, kind: rowan::SyntaxKind, text: &str) {
        self.inner.token(kind, text);
        self.heights.push(0);
    }

    pub(super) fn start_node(&mut self, kind: rowan::SyntaxKind) {
        self.inner.start_node(kind);
        self.open.push(self.heights.len());
    }

    pub(super) fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            rowan: self.inner.checkpoint(),
            index: self.heights.len(),
        }
    }

    /// Open a node whose children start at `checkpoint`, wrapping the elements
    /// pushed since. rowan asserts the checkpoint is still valid; that check
    /// runs first, so `open` cannot be pushed an index rowan rejected.
    pub(super) fn start_node_at(&mut self, checkpoint: Checkpoint, kind: rowan::SyntaxKind) {
        self.inner.start_node_at(checkpoint.rowan, kind);
        self.open.push(checkpoint.index);
    }

    pub(super) fn finish_node(&mut self) {
        self.inner.finish_node();
        let first_child = self
            .open
            .pop()
            .expect("rowan's finish_node succeeded, so a node was open");
        let height = 1 + self.heights[first_child..]
            .iter()
            .copied()
            .max()
            .unwrap_or(0);
        self.heights.truncate(first_child);
        self.heights.push(height);
        self.tallest = self.tallest.max(height);
    }

    /// The height the tree would have if every open node were closed now with
    /// nothing more added — at most: the tallest pending element plus one level
    /// per open node.
    pub(super) fn height_if_closed_now(&self) -> usize {
        self.tallest as usize + self.open.len()
    }

    pub(super) fn finish(self) -> GreenNode {
        self.inner.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{FSharpLang, SyntaxKind, SyntaxNode};
    use rowan::{Language, WalkEvent};

    fn raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        FSharpLang::kind_to_raw(kind)
    }

    fn height(root: &SyntaxNode) -> usize {
        let (mut depth, mut height) = (0usize, 0usize);
        for event in root.preorder() {
            match event {
                WalkEvent::Enter(_) => {
                    depth += 1;
                    height = height.max(depth);
                }
                WalkEvent::Leave(_) => depth -= 1,
            }
        }
        height
    }

    /// One builder operation, for driving the builder with arbitrary
    /// well-formed sequences.
    #[derive(Clone, Copy, Debug)]
    enum Op {
        Token,
        Start,
        /// Wrap from the `n`-th live checkpoint (modulo how many there are).
        StartAt(usize),
        Finish,
        Checkpoint,
    }

    /// Replay `ops` (skipping any that would be ill-formed) and check, after
    /// every step, that the tracked height bounds the height the tree would
    /// have if closed now — and, once finished, that it is exact for the root.
    fn run(ops: &[Op]) {
        let mut b = TreeBuilder::new();
        b.start_node(raw(SyntaxKind::IMPL_FILE));
        // Live checkpoints, each with the open depth it was taken at: rowan
        // requires a wrap's checkpoint to lie within the innermost open node.
        let mut checkpoints: Vec<(Checkpoint, usize)> = Vec::new();
        for &op in ops {
            match op {
                Op::Token => b.token(raw(SyntaxKind::IDENT_TOK), "a"),
                Op::Start => b.start_node(raw(SyntaxKind::PAREN_EXPR)),
                Op::Checkpoint => checkpoints.push((b.checkpoint(), b.open.len())),
                Op::StartAt(n) => {
                    // A checkpoint stays valid while its node is innermost and
                    // nothing before it has been folded away.
                    checkpoints
                        .retain(|(c, depth)| *depth == b.open.len() && c.index <= b.heights.len());
                    if !checkpoints.is_empty() {
                        let (cp, _) = checkpoints[n % checkpoints.len()];
                        b.start_node_at(cp, raw(SyntaxKind::APP_EXPR));
                    }
                }
                Op::Finish => {
                    if b.open.len() > 1 {
                        b.finish_node();
                    }
                }
            }
            checkpoints.retain(|(c, depth)| *depth <= b.open.len() && c.index <= b.heights.len());
        }
        let bound = b.height_if_closed_now();
        while !b.open.is_empty() {
            b.finish_node();
        }
        let tallest = b.tallest as usize;
        let root = SyntaxNode::new_root(b.finish());
        let actual = height(&root);
        assert_eq!(actual, tallest, "tracked height of the finished root");
        assert!(
            actual <= bound,
            "closed-now bound {bound} under actual {actual}"
        );
    }

    #[test]
    fn tracked_height_matches_the_built_tree() {
        use proptest::prelude::*;
        let op = prop_oneof![
            Just(Op::Token),
            Just(Op::Start),
            any::<usize>().prop_map(Op::StartAt),
            Just(Op::Finish),
            Just(Op::Checkpoint),
        ];
        proptest!(|(ops in proptest::collection::vec(op, 0..200))| run(&ops));
    }

    #[test]
    fn a_left_nested_wrap_chain_grows_one_level_per_wrap() {
        let mut b = TreeBuilder::new();
        b.start_node(raw(SyntaxKind::IMPL_FILE));
        let cp = b.checkpoint();
        b.token(raw(SyntaxKind::IDENT_TOK), "f");
        for _ in 0..10 {
            b.start_node_at(cp, raw(SyntaxKind::APP_EXPR));
            b.token(raw(SyntaxKind::IDENT_TOK), "a");
            b.finish_node();
        }
        assert_eq!(b.height_if_closed_now(), 11);
        b.finish_node();
        assert_eq!(height(&SyntaxNode::new_root(b.finish())), 11);
    }
}
