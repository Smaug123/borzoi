//! The syntax tree: one flat preorder table per parse.
//!
//! A [`Tree`] holds every node and token of a file in one contiguous array in
//! preorder, so a subtree is the id range `[id, sub_end)`: `descendants` is a
//! linear scan and a first child is `id + 1`. A handle ([`SyntaxNode`],
//! [`SyntaxToken`]) is `(Arc<Tree>, id)`, so navigating allocates nothing,
//! and the tree is `Send + Sync`.
//!
//! The handles keep the API (and the semantics, edge cases included) of the
//! subset of rowan 0.17's red tree the workspace uses, so the typed facade in
//! the parent module reads them unchanged: `first_token` stops at an empty
//! first child, `token_at_offset` splits at a boundary between two tokens,
//! `covering_element` picks among children by rowan's binary search, and
//! `Debug` prints rowan's format. `tests/all/flat_tree_reference.rs` holds
//! every method to rowan's on the same green tree. Two deliberate differences:
//! handle equality is the element itself (rowan's is green pointer and offset,
//! under which two zero-width tokens of one kind at one offset are equal), and
//! subtree equality is [`SyntaxNode::structurally_eq`] rather than comparing
//! green nodes.
//!
//! The parser still builds a rowan green tree, and [`SyntaxNode::new_root`]
//! flattens it (`docs/flat-syntax-tree-plan.md`, Stage 2 removes that step).

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use super::SyntaxKind;

pub use rowan::{Direction, TextRange, TextSize, TokenAtOffset, WalkEvent};

/// The shared owner a handle holds. `Arc`, so a tree can cross threads; it
/// costs 1-3% over `Rc` on resolve and infer
/// (`docs/flat-syntax-tree-plan.md`, "The spike").
type Shared<T> = Arc<T>;

const NONE: u32 = u32::MAX;

// A parse can move to, and be read from, other threads.
const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    send_sync::<SyntaxNode>();
    send_sync::<SyntaxToken>();
    send_sync::<SyntaxText>();
};

#[derive(Clone, Copy, Debug)]
struct Entry {
    kind: SyntaxKind,
    is_token: bool,
    parent: u32,
    next_sib: u32,
    prev_sib: u32,
    /// Exclusive end of this element's subtree in preorder ids.
    sub_end: u32,
    start: u32,
    end: u32,
    /// Index among the parent's children, tokens included.
    index: u32,
}

/// The whole parse of one file.
struct Tree {
    /// Every element, in preorder.
    entries: Vec<Entry>,
    /// The concatenated token text: the source, by losslessness.
    text: String,
    /// The green tree flattened into `entries`, kept for
    /// [`SyntaxNode::reference_green`] only.
    green: rowan::GreenNode,
}

impl Tree {
    fn from_green(green: rowan::GreenNode) -> Tree {
        struct Frame<'a> {
            children: rowan::Children<'a>,
            id: u32,
            prev: u32,
            idx: u32,
        }
        let mut entries = Vec::new();
        let mut text = String::new();
        entries.push(Entry {
            kind: kind_of(green.kind()),
            is_token: false,
            parent: NONE,
            next_sib: NONE,
            prev_sib: NONE,
            sub_end: NONE,
            start: 0,
            end: NONE,
            index: 0,
        });
        // Iterative: the tree may be as tall as `MAX_TREE_HEIGHT`.
        let mut stack = vec![Frame {
            children: green.children(),
            id: 0,
            prev: NONE,
            idx: 0,
        }];
        while let Some(frame) = stack.last_mut() {
            match frame.children.next() {
                Some(child) => {
                    let cid = entries.len() as u32;
                    let parent = frame.id;
                    let prev = frame.prev;
                    let index = frame.idx;
                    frame.prev = cid;
                    frame.idx += 1;
                    if prev != NONE {
                        entries[prev as usize].next_sib = cid;
                    }
                    let start = text.len() as u32;
                    match child {
                        rowan::NodeOrToken::Token(t) => {
                            text.push_str(t.text());
                            entries.push(Entry {
                                kind: kind_of(t.kind()),
                                is_token: true,
                                parent,
                                next_sib: NONE,
                                prev_sib: prev,
                                sub_end: cid + 1,
                                start,
                                end: text.len() as u32,
                                index,
                            });
                        }
                        rowan::NodeOrToken::Node(n) => {
                            entries.push(Entry {
                                kind: kind_of(n.kind()),
                                is_token: false,
                                parent,
                                next_sib: NONE,
                                prev_sib: prev,
                                sub_end: NONE,
                                start,
                                end: NONE,
                                index,
                            });
                            stack.push(Frame {
                                children: n.children(),
                                id: cid,
                                prev: NONE,
                                idx: 0,
                            });
                        }
                    }
                }
                None => {
                    let id = frame.id as usize;
                    stack.pop();
                    entries[id].sub_end = entries.len() as u32;
                    entries[id].end = text.len() as u32;
                }
            }
        }
        Tree {
            entries,
            text,
            green,
        }
    }

    #[inline]
    fn e(&self, id: u32) -> &Entry {
        &self.entries[id as usize]
    }

    #[inline]
    fn range(&self, id: u32) -> TextRange {
        let e = self.e(id);
        TextRange::new(TextSize::from(e.start), TextSize::from(e.end))
    }

    #[inline]
    fn first_child_or_token(&self, id: u32) -> u32 {
        if id + 1 < self.e(id).sub_end {
            id + 1
        } else {
            NONE
        }
    }

    fn last_child_or_token(&self, id: u32) -> u32 {
        let mut c = self.first_child_or_token(id);
        if c == NONE {
            return NONE;
        }
        while self.e(c).next_sib != NONE {
            c = self.e(c).next_sib;
        }
        c
    }

    fn next_node_from(&self, mut c: u32) -> u32 {
        while c != NONE && self.e(c).is_token {
            c = self.e(c).next_sib;
        }
        c
    }

    fn prev_node_from(&self, mut c: u32) -> u32 {
        while c != NONE && self.e(c).is_token {
            c = self.e(c).prev_sib;
        }
        c
    }

    /// rowan's `first_token`: follows first children, stopping (with `None`)
    /// at an empty node.
    fn first_token(&self, mut id: u32) -> u32 {
        loop {
            if self.e(id).is_token {
                return id;
            }
            id = self.first_child_or_token(id);
            if id == NONE {
                return NONE;
            }
        }
    }

    fn last_token(&self, mut id: u32) -> u32 {
        loop {
            if self.e(id).is_token {
                return id;
            }
            id = self.last_child_or_token(id);
            if id == NONE {
                return NONE;
            }
        }
    }
}

fn kind_of(raw: rowan::SyntaxKind) -> SyntaxKind {
    SyntaxKind::from_raw(raw.0).expect("rowan SyntaxKind out of range")
}

/// A subtree's shape, kinds and text: see [`SyntaxNode::structure`].
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Structure {
    /// Per element in preorder: kind, whether a token, subtree size, text
    /// length.
    shape: Vec<(SyntaxKind, bool, u32, u32)>,
    text: String,
}

// ---- handles ---------------------------------------------------------------

/// A node: a handle on one element of its file's tree. Cloning shares the
/// tree.
#[derive(Clone)]
pub struct SyntaxNode {
    tree: Shared<Tree>,
    id: u32,
}

/// A token: a handle on one leaf of its file's tree. Its text is a slice of the
/// file's.
#[derive(Clone)]
pub struct SyntaxToken {
    tree: Shared<Tree>,
    id: u32,
}

/// A node or a token: what child and preorder walks that include tokens yield.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum NodeOrToken<N, T> {
    Node(N),
    Token(T),
}

/// A node or token of the tree.
pub type SyntaxElement = NodeOrToken<SyntaxNode, SyntaxToken>;

impl<N, T> NodeOrToken<N, T> {
    pub fn into_node(self) -> Option<N> {
        match self {
            NodeOrToken::Node(n) => Some(n),
            NodeOrToken::Token(_) => None,
        }
    }
    pub fn into_token(self) -> Option<T> {
        match self {
            NodeOrToken::Node(_) => None,
            NodeOrToken::Token(t) => Some(t),
        }
    }
    pub fn as_node(&self) -> Option<&N> {
        match self {
            NodeOrToken::Node(n) => Some(n),
            NodeOrToken::Token(_) => None,
        }
    }
    pub fn as_token(&self) -> Option<&T> {
        match self {
            NodeOrToken::Node(_) => None,
            NodeOrToken::Token(t) => Some(t),
        }
    }
}

impl<N: fmt::Display, T: fmt::Display> fmt::Display for NodeOrToken<N, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeOrToken::Node(n) => n.fmt(f),
            NodeOrToken::Token(t) => t.fmt(f),
        }
    }
}

fn element(tree: &Shared<Tree>, id: u32) -> SyntaxElement {
    if tree.e(id).is_token {
        NodeOrToken::Token(SyntaxToken {
            tree: tree.clone(),
            id,
        })
    } else {
        NodeOrToken::Node(SyntaxNode {
            tree: tree.clone(),
            id,
        })
    }
}

fn opt_node(tree: &Shared<Tree>, id: u32) -> Option<SyntaxNode> {
    (id != NONE).then(|| SyntaxNode {
        tree: tree.clone(),
        id,
    })
}

fn opt_token(tree: &Shared<Tree>, id: u32) -> Option<SyntaxToken> {
    (id != NONE).then(|| SyntaxToken {
        tree: tree.clone(),
        id,
    })
}

fn opt_element(tree: &Shared<Tree>, id: u32) -> Option<SyntaxElement> {
    (id != NONE).then(|| element(tree, id))
}

macro_rules! identity {
    ($t:ty) => {
        impl PartialEq for $t {
            #[inline]
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id && Shared::ptr_eq(&self.tree, &other.tree)
            }
        }
        impl Eq for $t {}
        impl Hash for $t {
            fn hash<H: Hasher>(&self, state: &mut H) {
                (Shared::as_ptr(&self.tree) as usize).hash(state);
                self.id.hash(state);
            }
        }
    };
}
identity!(SyntaxNode);
identity!(SyntaxToken);

impl SyntaxNode {
    /// The root of the tree flattened from `green`.
    pub fn new_root(green: rowan::GreenNode) -> SyntaxNode {
        SyntaxNode {
            tree: Shared::new(Tree::from_green(green)),
            id: 0,
        }
    }

    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        self.tree.e(self.id).kind
    }

    #[inline]
    pub fn text_range(&self) -> TextRange {
        self.tree.range(self.id)
    }

    pub fn index(&self) -> usize {
        self.tree.e(self.id).index as usize
    }

    pub fn text(&self) -> SyntaxText {
        SyntaxText {
            tree: self.tree.clone(),
            range: self.text_range(),
        }
    }

    /// The green tree this table was flattened from, for the root only: the
    /// rowan reference differential (`flat_tree_reference`) rebuilds rowan's
    /// red tree from it. Not an API: compare subtrees with
    /// [`SyntaxNode::structurally_eq`] or [`SyntaxNode::structure`].
    #[doc(hidden)]
    pub fn reference_green(&self) -> Option<&rowan::GreenNodeData> {
        (self.id == 0).then(|| &*self.tree.green)
    }

    /// Whether the two subtrees have the same shape, kinds and token text,
    /// wherever they sit: the equivalence rowan's green-node equality
    /// computed. Handle equality (`==`) is identity instead.
    pub fn structurally_eq(&self, other: &SyntaxNode) -> bool {
        let (a, b) = (&*self.tree, &*other.tree);
        let (sa, sb) = (self.id, other.id);
        let len = a.e(sa).sub_end - sa;
        if b.e(sb).sub_end - sb != len {
            return false;
        }
        (0..len).all(|i| {
            let (ea, eb) = (a.e(sa + i), b.e(sb + i));
            ea.kind == eb.kind
                && ea.is_token == eb.is_token
                && ea.sub_end - (sa + i) == eb.sub_end - (sb + i)
                && (!ea.is_token
                    || a.text[ea.start as usize..ea.end as usize]
                        == b.text[eb.start as usize..eb.end as usize])
        })
    }

    /// An owned snapshot of the subtree under [`SyntaxNode::structurally_eq`]'s
    /// equivalence: two snapshots are equal exactly when their subtrees are
    /// structurally equal.
    pub fn structure(&self) -> Structure {
        let t = &*self.tree;
        let end = t.e(self.id).sub_end;
        let shape = (self.id..end)
            .map(|i| {
                let e = t.e(i);
                (e.kind, e.is_token, e.sub_end - i, e.end - e.start)
            })
            .collect();
        Structure {
            shape,
            text: self.to_string(),
        }
    }

    #[inline]
    pub fn parent(&self) -> Option<SyntaxNode> {
        opt_node(&self.tree, self.tree.e(self.id).parent)
    }

    pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
        std::iter::successors(Some(self.clone()), SyntaxNode::parent)
    }

    pub fn tree_top(&self) -> SyntaxNode {
        SyntaxNode {
            tree: self.tree.clone(),
            id: 0,
        }
    }

    #[inline]
    pub fn children(&self) -> SyntaxNodeChildren {
        SyntaxNodeChildren {
            tree: self.tree.clone(),
            next: self.tree.first_child_or_token(self.id),
        }
    }

    #[inline]
    pub fn children_with_tokens(&self) -> SyntaxElementChildren {
        SyntaxElementChildren {
            tree: self.tree.clone(),
            next: self.tree.first_child_or_token(self.id),
        }
    }

    pub fn first_child(&self) -> Option<SyntaxNode> {
        let c = self.tree.first_child_or_token(self.id);
        opt_node(&self.tree, self.tree.next_node_from(c))
    }

    pub fn last_child(&self) -> Option<SyntaxNode> {
        let c = self.tree.last_child_or_token(self.id);
        opt_node(&self.tree, self.tree.prev_node_from(c))
    }

    pub fn first_child_or_token(&self) -> Option<SyntaxElement> {
        opt_element(&self.tree, self.tree.first_child_or_token(self.id))
    }

    pub fn last_child_or_token(&self) -> Option<SyntaxElement> {
        opt_element(&self.tree, self.tree.last_child_or_token(self.id))
    }

    pub fn next_sibling(&self) -> Option<SyntaxNode> {
        let c = self.tree.e(self.id).next_sib;
        opt_node(&self.tree, self.tree.next_node_from(c))
    }

    pub fn prev_sibling(&self) -> Option<SyntaxNode> {
        let c = self.tree.e(self.id).prev_sib;
        opt_node(&self.tree, self.tree.prev_node_from(c))
    }

    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
        opt_element(&self.tree, self.tree.e(self.id).next_sib)
    }

    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
        opt_element(&self.tree, self.tree.e(self.id).prev_sib)
    }

    pub fn first_token(&self) -> Option<SyntaxToken> {
        opt_token(&self.tree, self.tree.first_token(self.id))
    }

    pub fn last_token(&self) -> Option<SyntaxToken> {
        opt_token(&self.tree, self.tree.last_token(self.id))
    }

    pub fn siblings(&self, direction: Direction) -> impl Iterator<Item = SyntaxNode> + use<> {
        std::iter::successors(Some(self.clone()), move |n| match direction {
            Direction::Next => n.next_sibling(),
            Direction::Prev => n.prev_sibling(),
        })
    }

    pub fn siblings_with_tokens(
        &self,
        direction: Direction,
    ) -> impl Iterator<Item = SyntaxElement> + use<> {
        let me: SyntaxElement = NodeOrToken::Node(self.clone());
        std::iter::successors(Some(me), move |el| match direction {
            Direction::Next => el.next_sibling_or_token(),
            Direction::Prev => el.prev_sibling_or_token(),
        })
    }

    #[inline]
    pub fn descendants(&self) -> Descendants {
        Descendants {
            tree: self.tree.clone(),
            next: self.id,
            end: self.tree.e(self.id).sub_end,
        }
    }

    #[inline]
    pub fn descendants_with_tokens(&self) -> DescendantsWithTokens {
        DescendantsWithTokens {
            tree: self.tree.clone(),
            next: self.id,
            end: self.tree.e(self.id).sub_end,
        }
    }

    pub fn preorder(&self) -> Preorder {
        Preorder {
            inner: PreorderRaw::new(self.tree.clone(), self.id, false),
        }
    }

    pub fn preorder_with_tokens(&self) -> PreorderWithTokens {
        PreorderWithTokens {
            inner: PreorderRaw::new(self.tree.clone(), self.id, true),
        }
    }

    pub fn token_at_offset(&self, offset: TextSize) -> TokenAtOffset<SyntaxToken> {
        let range = self.text_range();
        assert!(
            range.start() <= offset && offset <= range.end(),
            "Bad offset: range {range:?} offset {offset:?}"
        );
        if range.is_empty() {
            return TokenAtOffset::None;
        }
        let t = &self.tree;
        let touching = |c: u32| {
            let r = t.range(c);
            !r.is_empty() && r.start() <= offset && offset <= r.end()
        };
        let first_touching = |id: u32| -> u32 {
            let mut c = t.first_child_or_token(id);
            while c != NONE && !touching(c) {
                c = t.e(c).next_sib;
            }
            c
        };
        // Descend from an element touching `offset` to its single token.
        let descend = |mut id: u32| -> u32 {
            while !t.e(id).is_token {
                id = first_touching(id);
                assert!(id != NONE);
            }
            id
        };
        let left = first_touching(self.id);
        assert!(left != NONE);
        let mut right = t.e(left).next_sib;
        while right != NONE && !touching(right) {
            right = t.e(right).next_sib;
        }
        let tok = |id| SyntaxToken {
            tree: t.clone(),
            id,
        };
        if right != NONE {
            TokenAtOffset::Between(tok(descend(left)), tok(descend(right)))
        } else {
            // rowan recurses into `left` here, which re-splits.
            match t.e(left).is_token {
                true => TokenAtOffset::Single(tok(left)),
                false => SyntaxNode {
                    tree: t.clone(),
                    id: left,
                }
                .token_at_offset(offset),
            }
        }
    }

    pub fn covering_element(&self, range: TextRange) -> SyntaxElement {
        let mut res: SyntaxElement = NodeOrToken::Node(self.clone());
        loop {
            assert!(
                res.text_range().contains_range(range),
                "Bad range: node range {:?}, range {:?}",
                res.text_range(),
                range,
            );
            res = match &res {
                NodeOrToken::Token(_) => return res,
                NodeOrToken::Node(node) => match node.child_or_token_at_range(range) {
                    Some(it) => it,
                    None => return res,
                },
            };
        }
    }

    /// rowan's `child_at_range`, binary search included, so empty children
    /// pick the same way.
    pub fn child_or_token_at_range(&self, range: TextRange) -> Option<SyntaxElement> {
        let t = &self.tree;
        let mut ids = Vec::new();
        let mut c = t.first_child_or_token(self.id);
        while c != NONE {
            ids.push(c);
            c = t.e(c).next_sib;
        }
        let idx = ids
            .binary_search_by(|&c| TextRange::ordering(t.range(c), range))
            .unwrap_or_else(|it| it.saturating_sub(1));
        let c = *ids.get(idx)?;
        t.range(c).contains_range(range).then(|| element(t, c))
    }
}

impl SyntaxToken {
    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        self.tree.e(self.id).kind
    }

    #[inline]
    pub fn text_range(&self) -> TextRange {
        self.tree.range(self.id)
    }

    pub fn index(&self) -> usize {
        self.tree.e(self.id).index as usize
    }

    #[inline]
    pub fn text(&self) -> &str {
        let e = self.tree.e(self.id);
        &self.tree.text[e.start as usize..e.end as usize]
    }

    #[inline]
    pub fn parent(&self) -> Option<SyntaxNode> {
        opt_node(&self.tree, self.tree.e(self.id).parent)
    }

    pub fn parent_ancestors(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
        std::iter::successors(self.parent(), SyntaxNode::parent)
    }

    pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
        self.parent_ancestors()
    }

    pub fn tree_top(&self) -> SyntaxNode {
        SyntaxNode {
            tree: self.tree.clone(),
            id: 0,
        }
    }

    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
        opt_element(&self.tree, self.tree.e(self.id).next_sib)
    }

    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
        opt_element(&self.tree, self.tree.e(self.id).prev_sib)
    }

    pub fn siblings_with_tokens(
        &self,
        direction: Direction,
    ) -> impl Iterator<Item = SyntaxElement> + use<> {
        let me: SyntaxElement = NodeOrToken::Token(self.clone());
        std::iter::successors(Some(me), move |el| match direction {
            Direction::Next => el.next_sibling_or_token(),
            Direction::Prev => el.prev_sibling_or_token(),
        })
    }

    pub fn next_token(&self) -> Option<SyntaxToken> {
        match self.next_sibling_or_token() {
            Some(element) => element.first_token(),
            None => self
                .parent_ancestors()
                .find_map(|it| it.next_sibling_or_token())
                .and_then(|element| element.first_token()),
        }
    }

    pub fn prev_token(&self) -> Option<SyntaxToken> {
        match self.prev_sibling_or_token() {
            Some(element) => element.last_token(),
            None => self
                .parent_ancestors()
                .find_map(|it| it.prev_sibling_or_token())
                .and_then(|element| element.last_token()),
        }
    }
}

impl SyntaxElement {
    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        match self {
            NodeOrToken::Node(n) => n.kind(),
            NodeOrToken::Token(t) => t.kind(),
        }
    }
    #[inline]
    pub fn text_range(&self) -> TextRange {
        match self {
            NodeOrToken::Node(n) => n.text_range(),
            NodeOrToken::Token(t) => t.text_range(),
        }
    }
    pub fn index(&self) -> usize {
        match self {
            NodeOrToken::Node(n) => n.index(),
            NodeOrToken::Token(t) => t.index(),
        }
    }
    pub fn parent(&self) -> Option<SyntaxNode> {
        match self {
            NodeOrToken::Node(n) => n.parent(),
            NodeOrToken::Token(t) => t.parent(),
        }
    }
    pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
        let first = match self {
            NodeOrToken::Node(n) => Some(n.clone()),
            NodeOrToken::Token(t) => t.parent(),
        };
        std::iter::successors(first, SyntaxNode::parent)
    }
    pub fn first_token(&self) -> Option<SyntaxToken> {
        match self {
            NodeOrToken::Node(n) => n.first_token(),
            NodeOrToken::Token(t) => Some(t.clone()),
        }
    }
    pub fn last_token(&self) -> Option<SyntaxToken> {
        match self {
            NodeOrToken::Node(n) => n.last_token(),
            NodeOrToken::Token(t) => Some(t.clone()),
        }
    }
    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
        match self {
            NodeOrToken::Node(n) => n.next_sibling_or_token(),
            NodeOrToken::Token(t) => t.next_sibling_or_token(),
        }
    }
    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
        match self {
            NodeOrToken::Node(n) => n.prev_sibling_or_token(),
            NodeOrToken::Token(t) => t.prev_sibling_or_token(),
        }
    }
}

impl From<SyntaxNode> for SyntaxElement {
    fn from(n: SyntaxNode) -> SyntaxElement {
        NodeOrToken::Node(n)
    }
}

impl From<SyntaxToken> for SyntaxElement {
    fn from(t: SyntaxToken) -> SyntaxElement {
        NodeOrToken::Token(t)
    }
}

// ---- formatting (rowan's exact output) -------------------------------------

impl fmt::Debug for SyntaxNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            let mut level = 0;
            for event in self.preorder_with_tokens() {
                match event {
                    WalkEvent::Enter(element) => {
                        for _ in 0..level {
                            write!(f, "  ")?;
                        }
                        match element {
                            NodeOrToken::Node(node) => writeln!(f, "{node:?}")?,
                            NodeOrToken::Token(token) => writeln!(f, "{token:?}")?,
                        }
                        level += 1;
                    }
                    WalkEvent::Leave(_) => level -= 1,
                }
            }
            assert_eq!(level, 0);
            Ok(())
        } else {
            write!(f, "{:?}@{:?}", self.kind(), self.text_range())
        }
    }
}

impl fmt::Display for SyntaxNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let e = self.tree.e(self.id);
        f.write_str(&self.tree.text[e.start as usize..e.end as usize])
    }
}

impl fmt::Debug for SyntaxToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}@{:?}", self.kind(), self.text_range())?;
        let text = self.text();
        if text.len() < 25 {
            return write!(f, " {text:?}");
        }
        for idx in 21..25 {
            if text.is_char_boundary(idx) {
                let text = format!("{} ...", &text[..idx]);
                return write!(f, " {text:?}");
            }
        }
        unreachable!()
    }
}

impl fmt::Display for SyntaxToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

// ---- iterators --------------------------------------------------------------

pub struct SyntaxNodeChildren {
    tree: Shared<Tree>,
    next: u32,
}

impl Iterator for SyntaxNodeChildren {
    type Item = SyntaxNode;
    #[inline]
    fn next(&mut self) -> Option<SyntaxNode> {
        let c = self.tree.next_node_from(self.next);
        if c == NONE {
            self.next = NONE;
            return None;
        }
        self.next = self.tree.e(c).next_sib;
        Some(SyntaxNode {
            tree: self.tree.clone(),
            id: c,
        })
    }
}

pub struct SyntaxElementChildren {
    tree: Shared<Tree>,
    next: u32,
}

impl Iterator for SyntaxElementChildren {
    type Item = SyntaxElement;
    #[inline]
    fn next(&mut self) -> Option<SyntaxElement> {
        let c = self.next;
        if c == NONE {
            return None;
        }
        self.next = self.tree.e(c).next_sib;
        Some(element(&self.tree, c))
    }
}

pub struct Descendants {
    tree: Shared<Tree>,
    next: u32,
    end: u32,
}

impl Iterator for Descendants {
    type Item = SyntaxNode;
    #[inline]
    fn next(&mut self) -> Option<SyntaxNode> {
        while self.next < self.end {
            let id = self.next;
            self.next += 1;
            if !self.tree.e(id).is_token {
                return Some(SyntaxNode {
                    tree: self.tree.clone(),
                    id,
                });
            }
        }
        None
    }
}

pub struct DescendantsWithTokens {
    tree: Shared<Tree>,
    next: u32,
    end: u32,
}

impl Iterator for DescendantsWithTokens {
    type Item = SyntaxElement;
    #[inline]
    fn next(&mut self) -> Option<SyntaxElement> {
        if self.next < self.end {
            let id = self.next;
            self.next += 1;
            Some(element(&self.tree, id))
        } else {
            None
        }
    }
}

struct PreorderRaw {
    tree: Shared<Tree>,
    next: u32,
    end: u32,
    open: Vec<u32>,
    with_tokens: bool,
}

impl PreorderRaw {
    fn new(tree: Shared<Tree>, root: u32, with_tokens: bool) -> Self {
        let end = tree.e(root).sub_end;
        PreorderRaw {
            tree,
            next: root,
            end,
            open: Vec::new(),
            with_tokens,
        }
    }

    fn next(&mut self) -> Option<WalkEvent<u32>> {
        loop {
            if let Some(&top) = self.open.last()
                && self.next >= self.tree.e(top).sub_end
            {
                self.open.pop();
                return Some(WalkEvent::Leave(top));
            }
            if self.next >= self.end {
                return None;
            }
            let id = self.next;
            self.next += 1;
            if self.tree.e(id).is_token && !self.with_tokens {
                continue;
            }
            self.open.push(id);
            return Some(WalkEvent::Enter(id));
        }
    }

    fn skip_subtree(&mut self) {
        // Called after an `Enter(id)`: jump past `id`'s subtree; its `Leave`
        // comes next.
        if let Some(&top) = self.open.last() {
            self.next = self.tree.e(top).sub_end;
        }
    }
}

pub struct Preorder {
    inner: PreorderRaw,
}

impl Preorder {
    pub fn skip_subtree(&mut self) {
        self.inner.skip_subtree();
    }
}

impl Iterator for Preorder {
    type Item = WalkEvent<SyntaxNode>;
    fn next(&mut self) -> Option<Self::Item> {
        let t = &self.inner.tree.clone();
        self.inner.next().map(|ev| {
            ev.map(|id| SyntaxNode {
                tree: t.clone(),
                id,
            })
        })
    }
}

pub struct PreorderWithTokens {
    inner: PreorderRaw,
}

impl PreorderWithTokens {
    pub fn skip_subtree(&mut self) {
        self.inner.skip_subtree();
    }
}

impl Iterator for PreorderWithTokens {
    type Item = WalkEvent<SyntaxElement>;
    fn next(&mut self) -> Option<Self::Item> {
        let t = &self.inner.tree.clone();
        self.inner.next().map(|ev| ev.map(|id| element(t, id)))
    }
}

// ---- text -------------------------------------------------------------------

/// A node's text: a slice of its file's.
#[derive(Clone)]
pub struct SyntaxText {
    tree: Shared<Tree>,
    range: TextRange,
}

impl SyntaxText {
    fn as_str(&self) -> &str {
        &self.tree.text[self.range]
    }
    pub fn len(&self) -> TextSize {
        self.range.len()
    }
    pub fn is_empty(&self) -> bool {
        self.range.is_empty()
    }
    pub fn contains_char(&self, c: char) -> bool {
        self.as_str().contains(c)
    }
    pub fn find_char(&self, c: char) -> Option<TextSize> {
        self.as_str().find(c).map(|i| TextSize::from(i as u32))
    }
    pub fn char_at(&self, offset: TextSize) -> Option<char> {
        self.as_str().get(usize::from(offset)..)?.chars().next()
    }
    pub fn slice(&self, range: TextRange) -> SyntaxText {
        let start = self.range.start() + range.start();
        let r = TextRange::at(start, range.len());
        assert!(self.range.contains_range(r));
        SyntaxText {
            tree: self.tree.clone(),
            range: r,
        }
    }
    pub fn for_each_chunk<F: FnMut(&str)>(&self, mut f: F) {
        f(self.as_str())
    }
    pub fn try_for_each_chunk<F: FnMut(&str) -> Result<(), E>, E>(
        &self,
        mut f: F,
    ) -> Result<(), E> {
        f(self.as_str())
    }
}

impl fmt::Debug for SyntaxText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for SyntaxText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<SyntaxText> for String {
    fn from(t: SyntaxText) -> String {
        t.as_str().to_owned()
    }
}

impl PartialEq<str> for SyntaxText {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for SyntaxText {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<SyntaxText> for str {
    fn eq(&self, other: &SyntaxText) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<SyntaxText> for &str {
    fn eq(&self, other: &SyntaxText) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq for SyntaxText {
    fn eq(&self, other: &SyntaxText) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for SyntaxText {}
