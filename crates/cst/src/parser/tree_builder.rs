//! [`TreeBuilder`]: the syntax table's [`Builder`] with the height of the
//! tree it is building kept alongside.
//!
//! The parser builds many nested nodes from a loop rather than a recursive
//! call — an application chain `f a a a`, a left-associative operator chain, a
//! postfix type application `int list list` — by wrapping what it has built
//! so far under a fresh node ([`TreeBuilder::start_node_at`]). Such a chain is
//! as deep as it is long, and every consumer that walks the tree recursively
//! (sema's resolution and inference) needs stack in proportion to its height.
//! Counting at the builder bounds every such shape at once, whichever
//! production built it: the parser reads [`TreeBuilder::height_if_closed_now`]
//! and stops short of [`super::MAX_TREE_HEIGHT`].

use crate::syntax::{Builder, Checkpoint, SyntaxKind, SyntaxNode};

/// One finished element awaiting a parent.
#[derive(Clone, Copy)]
struct Pending {
    /// Nodes on its longest root-to-leaf path: 0 for a token.
    height: u32,
    /// The tallest of this element and the earlier pending elements of the
    /// same parent, so a wrap can recover the tallest child its parent keeps.
    prefix_max: u32,
}

/// One open node.
#[derive(Clone, Copy)]
struct Open {
    /// Index of its first pending child.
    first_child: usize,
    /// Its tallest pending child (0 if none).
    max_child: u32,
    /// The height the tree would have if it and every node above it were
    /// closed now: the largest `depth + max_child` over it and its ancestors,
    /// the root at depth 1.
    reach: u32,
}

/// A [`Builder`] that also tracks the height of what it builds.
///
/// `pending` mirrors the builder's elements awaiting a parent element for
/// element, and `open` its open nodes, so every operation updates both exactly
/// as the builder updates its own. [`Self::height_if_closed_now`] is then
/// exact, and costs nothing to read.
pub(super) struct TreeBuilder {
    inner: Builder,
    pending: Vec<Pending>,
    open: Vec<Open>,
    /// The tallest pending element outside every open node (a finished root).
    top_max: u32,
}

impl TreeBuilder {
    pub(super) fn new() -> Self {
        Self {
            inner: Builder::new(),
            pending: Vec::new(),
            open: Vec::new(),
            top_max: 0,
        }
    }

    pub(super) fn token(&mut self, kind: SyntaxKind, text: &str) {
        self.inner.token(kind, text);
        self.push(0);
    }

    pub(super) fn start_node(&mut self, kind: SyntaxKind) {
        self.inner.start_node(kind);
        self.open_from(self.pending.len());
    }

    pub(super) fn checkpoint(&self) -> Checkpoint {
        self.inner.checkpoint()
    }

    /// Open a node whose children start at `checkpoint`, wrapping the elements
    /// pushed since. The builder asserts the checkpoint is still valid; that
    /// check runs first, so nothing here sees an index it rejected.
    pub(super) fn start_node_at(&mut self, checkpoint: Checkpoint, kind: SyntaxKind) {
        self.inner.start_node_at(checkpoint, kind);
        let cp = checkpoint.pending_len();
        // The parent keeps only the children before the checkpoint.
        let kept = if cp > self.first_child() {
            self.pending[cp - 1].prefix_max
        } else {
            0
        };
        self.set_innermost_max(kept);
        self.open_from(cp);
        // The wrapped elements' running maxima restart under the new node.
        let mut running = 0;
        for element in &mut self.pending[cp..] {
            running = running.max(element.height);
            element.prefix_max = running;
        }
        self.set_innermost_max(running);
    }

    pub(super) fn finish_node(&mut self) {
        self.inner.finish_node();
        let node = self
            .open
            .pop()
            .expect("the builder's finish_node succeeded, so a node was open");
        self.pending.truncate(node.first_child);
        self.push(1 + node.max_child);
    }

    /// The height the tree would have if every open node were closed now with
    /// nothing more added. Exact, not a bound: nodes not yet opened — such as
    /// one a production will open around children it has already finished —
    /// are by definition not in it.
    pub(super) fn height_if_closed_now(&self) -> usize {
        self.open.last().map_or(self.top_max, |node| node.reach) as usize
    }

    /// The finished tree's root.
    pub(super) fn finish(self) -> SyntaxNode {
        self.inner.finish()
    }

    /// Finish the tree and drop it, however tall it is: for a tree
    /// [`super::parse_inner`] throws away, which may be far taller than any
    /// tree a parse returns.
    pub(super) fn discard(self) {
        self.inner.discard();
    }

    /// Index of the innermost open node's first pending child (0 outside every
    /// node).
    fn first_child(&self) -> usize {
        self.open.last().map_or(0, |node| node.first_child)
    }

    fn push(&mut self, height: u32) {
        let same_parent_before = self.pending.len() > self.first_child();
        let prefix_max = match self.pending.last() {
            Some(previous) if same_parent_before => previous.prefix_max.max(height),
            _ => height,
        };
        self.pending.push(Pending { height, prefix_max });
        self.set_innermost_max(prefix_max);
    }

    /// Open a node whose first pending child is at `first_child`, with no
    /// children counted yet.
    fn open_from(&mut self, first_child: usize) {
        let depth = self.open.len() as u32 + 1;
        let above = self.height_if_closed_now() as u32;
        self.open.push(Open {
            first_child,
            max_child: 0,
            reach: above.max(depth),
        });
    }

    /// Set the innermost open node's tallest child (or the top level's) and
    /// recompute its `reach`.
    fn set_innermost_max(&mut self, max_child: u32) {
        let depth = self.open.len() as u32;
        let above = match self.open.len() {
            0 => {
                self.top_max = max_child;
                return;
            }
            1 => self.top_max,
            n => self.open[n - 2].reach,
        };
        let node = self.open.last_mut().expect("depth ≥ 1");
        node.max_child = max_child;
        node.reach = above.max(depth + max_child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::WalkEvent;

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

    /// Replay `ops` (skipping any that would be ill-formed) under a root node,
    /// returning the builder.
    fn replay(ops: &[Op]) -> TreeBuilder {
        let mut b = TreeBuilder::new();
        b.start_node(SyntaxKind::IMPL_FILE);
        // Live checkpoints, each with the open depth it was taken at: rowan
        // requires a wrap's checkpoint to lie within the innermost open node.
        let mut checkpoints: Vec<(Checkpoint, usize)> = Vec::new();
        for &op in ops {
            match op {
                Op::Token => b.token(SyntaxKind::IDENT_TOK, "a"),
                Op::Start => b.start_node(SyntaxKind::PAREN_EXPR),
                Op::Checkpoint => checkpoints.push((b.checkpoint(), b.open.len())),
                Op::StartAt(n) => {
                    checkpoints.retain(|(c, depth)| {
                        *depth == b.open.len() && c.pending_len() <= b.pending.len()
                    });
                    if !checkpoints.is_empty() {
                        let (cp, _) = checkpoints[n % checkpoints.len()];
                        b.start_node_at(cp, SyntaxKind::APP_EXPR);
                    }
                }
                Op::Finish => {
                    if b.open.len() > 1 {
                        b.finish_node();
                    }
                }
            }
            checkpoints
                .retain(|(c, depth)| *depth <= b.open.len() && c.pending_len() <= b.pending.len());
        }
        b
    }

    /// After every prefix of `ops`, the tracked height equals the height of
    /// the tree that closing every open node would produce.
    fn check(ops: &[Op]) {
        for end in 0..=ops.len() {
            let mut b = replay(&ops[..end]);
            let predicted = b.height_if_closed_now();
            while !b.open.is_empty() {
                b.finish_node();
            }
            assert_eq!(b.height_if_closed_now(), predicted, "closing changed it");
            let actual = height(&b.finish());
            assert_eq!(predicted, actual, "after {:?}", &ops[..end]);
        }
    }

    fn ops() -> impl proptest::strategy::Strategy<Value = Vec<Op>> {
        use proptest::prelude::*;
        let op = prop_oneof![
            Just(Op::Token),
            Just(Op::Start),
            any::<usize>().prop_map(Op::StartAt),
            Just(Op::Finish),
            Just(Op::Checkpoint),
        ];
        proptest::collection::vec(op, 0..120)
    }

    #[test]
    fn tracked_height_is_the_height_of_the_tree_closed_now() {
        use proptest::prelude::*;
        proptest!(|(ops in ops())| check(&ops));
    }

    /// For any well-formed operation sequence (empty nodes, and wraps from
    /// every live checkpoint, included), the table the builder writes is
    /// exactly the one flattening rowan's green tree for the same operations
    /// gives: `Builder::finish` asserts that under `rowan-reference`, which
    /// this crate's tests turn on.
    #[test]
    fn the_built_table_is_rowans_flattening() {
        use proptest::prelude::*;
        proptest!(|(ops in ops())| {
            let mut b = replay(&ops);
            while !b.open.is_empty() {
                b.finish_node();
            }
            b.finish();
        });
    }

    #[test]
    fn a_left_nested_wrap_chain_grows_one_level_per_wrap() {
        let mut b = TreeBuilder::new();
        b.start_node(SyntaxKind::IMPL_FILE);
        let cp = b.checkpoint();
        b.token(SyntaxKind::IDENT_TOK, "f");
        for _ in 0..10 {
            b.start_node_at(cp, SyntaxKind::APP_EXPR);
            b.token(SyntaxKind::IDENT_TOK, "a");
            b.finish_node();
        }
        assert_eq!(b.height_if_closed_now(), 11);
        b.finish_node();
        assert_eq!(height(&b.finish()), 11);
    }

    /// A tall finished sibling does not make a later shallow one count as
    /// tall: only the open chain and its own pending children do.
    #[test]
    fn a_tall_sibling_does_not_add_to_the_open_depth() {
        let mut b = TreeBuilder::new();
        b.start_node(SyntaxKind::IMPL_FILE);
        for _ in 0..50 {
            b.start_node(SyntaxKind::PAREN_EXPR);
        }
        b.token(SyntaxKind::IDENT_TOK, "a");
        for _ in 0..50 {
            b.finish_node();
        }
        assert_eq!(b.height_if_closed_now(), 51);
        for _ in 0..20 {
            b.start_node(SyntaxKind::PAREN_EXPR);
        }
        assert_eq!(b.height_if_closed_now(), 51);
    }
}
