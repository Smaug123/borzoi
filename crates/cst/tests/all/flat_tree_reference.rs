//! The flat syntax table (`borzoi_cst::syntax::SyntaxNode`) against rowan's
//! red tree, as a reference implementation of the same API.
//!
//! Both are built from the one green tree a parse produces, so they describe
//! the same elements. Every element is checked under every public method, each
//! result mapped to the preorder position of the element(s) it names: handle
//! equality is identity, so positions are what can be compared across the two
//! implementations. `token_at_offset` is checked at every offset, and
//! `covering_element` at every empty range and every short run of tokens.

use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};
use borzoi_cst::syntax::{
    Direction, FSharpLang, NodeOrToken, SyntaxElement, SyntaxNode, TextRange, TextSize,
    TokenAtOffset, WalkEvent,
};

type RNode = rowan::SyntaxNode<FSharpLang>;
type RElement = rowan::SyntaxElement<FSharpLang>;

/// Both sides' elements in preorder. A result is compared by its child-index
/// path from the root, not by handle equality: rowan's `Eq` is (green pointer,
/// offset), under which two zero-width tokens of one kind at one offset (which
/// share a deduplicated green token) are the *same* element, where the flat
/// table's identity is the element.
struct Pair {
    flat: Vec<SyntaxElement>,
    red: Vec<RElement>,
}

type Path = Vec<usize>;

impl Pair {
    fn new(root: &SyntaxNode) -> Pair {
        let red_root = RNode::new_root(root.reference_green().expect("a root").to_owned());
        let flat: Vec<SyntaxElement> = root.descendants_with_tokens().collect();
        let red: Vec<RElement> = red_root.descendants_with_tokens().collect();
        assert_eq!(flat.len(), red.len(), "element counts differ");
        Pair { flat, red }
    }

    fn f(&self, e: impl Into<SyntaxElement>) -> Path {
        let mut e: SyntaxElement = e.into();
        let mut path = Vec::new();
        while let Some(parent) = e.parent() {
            path.push(e.index());
            e = NodeOrToken::Node(parent);
        }
        path.reverse();
        path
    }

    fn r(&self, e: impl Into<RElement>) -> Path {
        let mut e: RElement = e.into();
        let mut path = Vec::new();
        while let Some(parent) = e.parent() {
            path.push(e.index());
            e = rowan::NodeOrToken::Node(parent);
        }
        path.reverse();
        path
    }

    fn fo(&self, e: Option<impl Into<SyntaxElement>>) -> Option<Path> {
        e.map(|e| self.f(e))
    }

    fn ro(&self, e: Option<impl Into<RElement>>) -> Option<Path> {
        e.map(|e| self.r(e))
    }

    fn fs(&self, it: impl Iterator<Item = impl Into<SyntaxElement>>) -> Vec<Path> {
        it.map(|e| self.f(e)).collect()
    }

    fn rs(&self, it: impl Iterator<Item = impl Into<RElement>>) -> Vec<Path> {
        it.map(|e| self.r(e)).collect()
    }

    fn fwalk(&self, ev: WalkEvent<impl Into<SyntaxElement>>) -> (bool, Path) {
        match ev {
            WalkEvent::Enter(e) => (true, self.f(e)),
            WalkEvent::Leave(e) => (false, self.f(e)),
        }
    }

    fn rwalk(&self, ev: rowan::WalkEvent<impl Into<RElement>>) -> (bool, Path) {
        match ev {
            rowan::WalkEvent::Enter(e) => (true, self.r(e)),
            rowan::WalkEvent::Leave(e) => (false, self.r(e)),
        }
    }

    fn ftao(&self, t: TokenAtOffset<borzoi_cst::syntax::SyntaxToken>) -> Vec<Path> {
        t.map(|t| self.f(t)).collect()
    }

    fn rtao(&self, t: rowan::TokenAtOffset<rowan::SyntaxToken<FSharpLang>>) -> Vec<Path> {
        t.map(|t| self.r(t)).collect()
    }
}

/// How much of each element to check. `Full` adds every method whose cost
/// grows with the element's subtree or sibling list (so quadratic over a file):
/// right for small generated inputs, unaffordable over the corpus, where the
/// root's whole-tree walks cover the same relations once.
#[derive(Clone, Copy, PartialEq)]
enum Depth {
    Full,
    Local,
}

fn check_element(p: &Pair, i: usize, depth: Depth) {
    let (f, r) = (&p.flat[i], &p.red[i]);
    let ctx = || format!("element {i} ({:?})", r);
    assert_eq!(f.kind(), r.kind(), "{}: kind", ctx());
    assert_eq!(f.text_range(), r.text_range(), "{}: range", ctx());
    assert_eq!(f.index(), r.index(), "{}: index", ctx());
    assert_eq!(p.fo(f.parent()), p.ro(r.parent()), "{}: parent", ctx());
    if depth == Depth::Full {
        assert_eq!(f.to_string(), r.to_string(), "{}: Display", ctx());
        assert_eq!(
            p.fs(f.ancestors()),
            p.rs(r.ancestors()),
            "{}: ancestors",
            ctx()
        );
    }
    assert_eq!(
        p.fo(f.first_token()),
        p.ro(red_first_token(r)),
        "{}: first_token",
        ctx()
    );
    assert_eq!(
        p.fo(f.last_token()),
        p.ro(red_last_token(r)),
        "{}: last_token",
        ctx()
    );
    assert_eq!(
        p.fo(f.next_sibling_or_token()),
        p.ro(r.next_sibling_or_token()),
        "{}: next_sibling_or_token",
        ctx()
    );
    assert_eq!(
        p.fo(f.prev_sibling_or_token()),
        p.ro(r.prev_sibling_or_token()),
        "{}: prev_sibling_or_token",
        ctx()
    );
    match (f, r) {
        (NodeOrToken::Node(f), rowan::NodeOrToken::Node(r)) => check_node(p, f, r, &ctx(), depth),
        (NodeOrToken::Token(f), rowan::NodeOrToken::Token(r)) => {
            let ctx = ctx();
            assert_eq!(f.text(), r.text(), "{ctx}: text");
            assert_eq!(format!("{f:?}"), format!("{r:?}"), "{ctx}: Debug");
            assert_eq!(
                p.fo(f.next_token()),
                p.ro(r.next_token()),
                "{ctx}: next_token"
            );
            assert_eq!(
                p.fo(f.prev_token()),
                p.ro(r.prev_token()),
                "{ctx}: prev_token"
            );
            if depth == Depth::Local {
                return;
            }
            assert_eq!(
                p.fs(f.parent_ancestors()),
                p.rs(r.parent_ancestors()),
                "{ctx}: parent_ancestors"
            );
            for (fd, rd) in [
                (Direction::Next, rowan::Direction::Next),
                (Direction::Prev, rowan::Direction::Prev),
            ] {
                assert_eq!(
                    p.fs(f.siblings_with_tokens(fd)),
                    p.rs(r.siblings_with_tokens(rd)),
                    "{ctx}: siblings_with_tokens"
                );
            }
        }
        _ => panic!("{}: node/token disagreement", ctx()),
    }
}

fn red_first_token(r: &RElement) -> Option<rowan::SyntaxToken<FSharpLang>> {
    match r {
        rowan::NodeOrToken::Node(n) => n.first_token(),
        rowan::NodeOrToken::Token(t) => Some(t.clone()),
    }
}

fn red_last_token(r: &RElement) -> Option<rowan::SyntaxToken<FSharpLang>> {
    match r {
        rowan::NodeOrToken::Node(n) => n.last_token(),
        rowan::NodeOrToken::Token(t) => Some(t.clone()),
    }
}

fn check_node(p: &Pair, f: &SyntaxNode, r: &RNode, ctx: &str, depth: Depth) {
    assert_eq!(format!("{f:?}"), format!("{r:?}"), "{ctx}: Debug");
    assert_eq!(p.fs(f.children()), p.rs(r.children()), "{ctx}: children");
    assert_eq!(
        p.fs(f.children_with_tokens()),
        p.rs(r.children_with_tokens()),
        "{ctx}: children_with_tokens"
    );
    assert_eq!(
        p.fo(f.first_child()),
        p.ro(r.first_child()),
        "{ctx}: first_child"
    );
    assert_eq!(
        p.fo(f.last_child()),
        p.ro(r.last_child()),
        "{ctx}: last_child"
    );
    assert_eq!(
        p.fo(f.first_child_or_token()),
        p.ro(r.first_child_or_token()),
        "{ctx}: first_child_or_token"
    );
    assert_eq!(
        p.fo(f.last_child_or_token()),
        p.ro(r.last_child_or_token()),
        "{ctx}: last_child_or_token"
    );
    assert_eq!(
        p.fo(f.next_sibling()),
        p.ro(r.next_sibling()),
        "{ctx}: next_sibling"
    );
    assert_eq!(
        p.fo(f.prev_sibling()),
        p.ro(r.prev_sibling()),
        "{ctx}: prev_sibling"
    );
    if depth == Depth::Local {
        return;
    }
    assert_eq!(f.text().to_string(), r.text().to_string(), "{ctx}: text");
    for (fd, rd) in [
        (Direction::Next, rowan::Direction::Next),
        (Direction::Prev, rowan::Direction::Prev),
    ] {
        assert_eq!(
            p.fs(f.siblings(fd)),
            p.rs(r.siblings(rd)),
            "{ctx}: siblings"
        );
        assert_eq!(
            p.fs(f.siblings_with_tokens(fd)),
            p.rs(r.siblings_with_tokens(rd)),
            "{ctx}: siblings_with_tokens"
        );
    }
    assert_eq!(
        p.fs(f.descendants()),
        p.rs(r.descendants()),
        "{ctx}: descendants"
    );
    assert_eq!(
        p.fs(f.descendants_with_tokens()),
        p.rs(r.descendants_with_tokens()),
        "{ctx}: descendants_with_tokens"
    );
    let fpre: Vec<_> = f.preorder().map(|e| p.fwalk(e)).collect();
    let rpre: Vec<_> = r.preorder().map(|e| p.rwalk(e)).collect();
    assert_eq!(fpre, rpre, "{ctx}: preorder");
    let fpre: Vec<_> = f.preorder_with_tokens().map(|e| p.fwalk(e)).collect();
    let rpre: Vec<_> = r.preorder_with_tokens().map(|e| p.rwalk(e)).collect();
    assert_eq!(fpre, rpre, "{ctx}: preorder_with_tokens");
}

/// `skip_subtree` on a deterministic subset of `Enter` events.
fn check_skipping_preorder(p: &Pair, f: &SyntaxNode, r: &RNode) {
    let skip = |path: &Path| path.iter().sum::<usize>() % 3 == 1;
    let mut fev = Vec::new();
    let mut fit = f.preorder_with_tokens();
    while let Some(e) = fit.next() {
        let e = p.fwalk(e);
        fev.push(e.clone());
        if e.0 && skip(&e.1) {
            fit.skip_subtree();
        }
    }
    let mut rev = Vec::new();
    let mut rit = r.preorder_with_tokens();
    while let Some(e) = rit.next() {
        let e = p.rwalk(e);
        rev.push(e.clone());
        if e.0 && skip(&e.1) {
            rit.skip_subtree();
        }
    }
    assert_eq!(fev, rev, "preorder_with_tokens + skip_subtree");
    let mut fev = Vec::new();
    let mut fit = f.preorder();
    while let Some(e) = fit.next() {
        let e = p.fwalk(e);
        fev.push(e.clone());
        if e.0 && skip(&e.1) {
            fit.skip_subtree();
        }
    }
    let mut rev = Vec::new();
    let mut rit = r.preorder();
    while let Some(e) = rit.next() {
        let e = p.rwalk(e);
        rev.push(e.clone());
        if e.0 && skip(&e.1) {
            rit.skip_subtree();
        }
    }
    assert_eq!(fev, rev, "preorder + skip_subtree");
}

/// How many tokens' offsets a [`Depth::Local`] check samples per file.
const LOCAL_OFFSET_SAMPLES: usize = 2000;

fn check_offsets(p: &Pair, f: &SyntaxNode, r: &RNode, depth: Depth) {
    let tokens: Vec<TextRange> = r
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .map(|t| t.text_range())
        .collect();
    // `Full`: every offset, and a covering range from every token. `Local`:
    // at most `LOCAL_OFFSET_SAMPLES` evenly spaced tokens' boundaries and one
    // offset inside each, and covering ranges from those tokens only. Each
    // query scans a sibling list linearly on both sides, so all of them over
    // a corpus file is quadratic (a 3.6 MB file of one flat module took six
    // minutes at every 16th token); the exhaustive generated inputs check
    // every offset.
    let stride = match depth {
        Depth::Full => 1,
        Depth::Local => tokens.len().div_ceil(LOCAL_OFFSET_SAMPLES).max(1),
    };
    let offsets: Vec<TextSize> = match depth {
        Depth::Full => (0..=u32::from(f.text_range().end()))
            .map(TextSize::from)
            .collect(),
        Depth::Local => tokens
            .iter()
            .step_by(stride)
            .flat_map(|t| [t.start(), t.start() + TextSize::from(1), t.end()])
            .filter(|&o| o <= f.text_range().end())
            .collect(),
    };
    for off in offsets {
        assert_eq!(
            p.ftao(f.token_at_offset(off)),
            p.rtao(r.token_at_offset(off)),
            "token_at_offset {off:?}"
        );
        let empty = TextRange::empty(off);
        assert_eq!(
            p.f(f.covering_element(empty)),
            p.r(r.covering_element(empty)),
            "covering_element {empty:?}"
        );
        assert_eq!(
            p.fo(f.child_or_token_at_range(empty)),
            p.ro(r.child_or_token_at_range(empty)),
            "child_or_token_at_range {empty:?}"
        );
    }
    for i in (0..tokens.len()).step_by(stride) {
        for j in i..(i + 4).min(tokens.len()) {
            let range = TextRange::new(tokens[i].start(), tokens[j].end());
            assert_eq!(
                p.f(f.covering_element(range)),
                p.r(r.covering_element(range)),
                "covering_element {range:?}"
            );
        }
    }
}

fn check_source(src: &str, file_kind: FileKind, depth: Depth) {
    let symbols = Default::default();
    let parse = parse_with_options(
        src,
        ParseOptions {
            file_kind,
            symbols: &symbols,
            lang: borzoi_cst::language_version::LanguageVersion::Preview,
        },
    );
    let root = &parse.root;
    assert_eq!(root.to_string(), src, "lossless");
    let p = Pair::new(root);
    for i in 0..p.flat.len() {
        check_element(&p, i, depth);
    }
    let (f, r) = (root, p.red[0].as_node().unwrap());
    if depth == Depth::Local {
        // The whole-tree relations, once, from the root.
        check_node(&p, f, r, "root", Depth::Full);
    }
    let red_root = p.red[0].as_node().unwrap().clone();
    assert_eq!(
        format!("{root:#?}"),
        format!("{red_root:#?}"),
        "alternate Debug"
    );
    check_skipping_preorder(&p, root, &red_root);
    check_offsets(&p, root, &red_root, depth);
}

const HANDWRITTEN: &[&str] = &[
    "",
    " ",
    "\n",
    "module M\n",
    "let x = 1\n",
    "let f x =\n    match x with\n    | Some y -> y\n    | None -> 0\n",
    "type T =\n    | A of int\n    | B\n    member this.M() = ()\n",
    "let x = (\n",
    "let = \n",
    "namespace N\n\ntype C() =\n    let mutable x = 0\n    member _.X with get() = x and set v = x <- v\n",
    "#if FOO\nlet a = 1\n#else\nlet b = 2\n#endif\n",
    "let s = $\"a {b} c\"\n",
    "let ``a b`` = [| 1; 2 |]\n",
    "if a then elif b then c\n",
    "let x = 1 +\n",
];

#[test]
fn handwritten_sources_agree_with_rowan() {
    for src in HANDWRITTEN {
        check_source(src, FileKind::Impl, Depth::Full);
        check_source(src, FileKind::Sig, Depth::Full);
    }
}

/// Every two-piece source over the panic sweep's wide alphabet: error recovery
/// produces the empty and oddly nested nodes rowan's edge cases are about.
#[test]
fn wide_pairs_agree_with_rowan() {
    use crate::parser_panic_sweep::WIDE;
    for a in WIDE {
        check_source(&format!("{a}\n"), FileKind::Impl, Depth::Full);
        for b in WIDE {
            check_source(&format!("{a}{b}\n"), FileKind::Impl, Depth::Full);
        }
    }
}

#[test]
fn core_triples_agree_with_rowan() {
    use crate::parser_panic_sweep::CORE;
    for a in CORE {
        for b in CORE {
            for c in CORE {
                check_source(&format!("{a}{b}{c}\n"), FileKind::Impl, Depth::Full);
            }
        }
    }
}

/// The whole corpus, each element checked [`Depth::Local`]ly.
#[test]
#[ignore]
fn corpus_agrees_with_rowan() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!("BORZOI_CORPUS unset; skipping");
        return;
    };
    let files =
        crate::common::collect_fsharp_corpus_files(std::path::Path::new(&root)).expect("walk");
    let outcomes = borzoi_oracle_harness::par_map::par_map(files.len(), |i| {
        let path = &files[i];
        let src = std::fs::read_to_string(path).ok()?;
        let kind = if path.extension().is_some_and(|e| e == "fsi") {
            FileKind::Sig
        } else {
            FileKind::Impl
        };
        // A disagreement panics with its details, printed by the panic hook.
        let agreed = std::panic::catch_unwind(|| check_source(&src, kind, Depth::Local));
        Some(agreed.map_err(|_| path.display().to_string()))
    });
    let disagreements: Vec<String> = outcomes
        .iter()
        .flatten()
        .filter_map(|o| o.clone().err())
        .collect();
    let n = outcomes.iter().flatten().count();
    assert!(n > 0, "no corpus file read");
    assert!(
        disagreements.is_empty(),
        "{} of {n} corpus files disagree (each panic above says where): {disagreements:#?}",
        disagreements.len()
    );
    eprintln!("{n} corpus files agree");
}
