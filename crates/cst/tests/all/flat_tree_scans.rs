//! The kind-filtered scans of the syntax table (`descendants_where`,
//! `descendants_of`, `tokens_where`, `first_token_where`, `last_token_where`)
//! against the compositions of the general walks they stand for, on every node
//! of every source in a generated family. They exist to skip building a handle
//! for an element the filter rejects, so the only thing they may change is
//! cost.

use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};
use borzoi_cst::syntax::{
    AstNode, Expr, NodeOrToken, Pat, SyntaxKind, SyntaxNode, SyntaxToken, Type, TypeDefn,
};

type KindFilter = fn(SyntaxKind) -> bool;

/// Kind filters: both trivial ones, the trivia test sema scans with, and one
/// that splits every kind range arbitrarily.
const FILTERS: [(&str, KindFilter); 4] = [
    ("every kind", |_| true),
    ("no kind", |_| false),
    ("trivia", SyntaxKind::is_trivia),
    ("odd kinds", |k| k as u16 % 2 == 1),
];

fn typed<T: AstNode>(node: &SyntaxNode) {
    let fast: Vec<SyntaxNode> = node
        .descendants_of::<T>()
        .map(|t| t.syntax().clone())
        .collect();
    let slow: Vec<SyntaxNode> = node
        .descendants()
        .filter_map(T::cast)
        .map(|t| t.syntax().clone())
        .collect();
    assert_eq!(
        fast,
        slow,
        "descendants_of::<{}>",
        std::any::type_name::<T>()
    );
}

fn check_source(src: &str, file_kind: FileKind) {
    let symbols = Default::default();
    let parse = parse_with_options(
        src,
        ParseOptions {
            file_kind,
            symbols: &symbols,
            lang: borzoi_cst::language_version::LanguageVersion::Preview,
        },
    );
    for node in parse.root.descendants() {
        for (name, keep) in FILTERS {
            let ctx = format!("{src:?}, {node:?}, {name}");
            let nodes: Vec<SyntaxNode> = node.descendants().filter(|n| keep(n.kind())).collect();
            assert_eq!(
                node.descendants_where(keep).collect::<Vec<_>>(),
                nodes,
                "{ctx}: descendants_where"
            );
            let tokens: Vec<SyntaxToken> = node
                .descendants_with_tokens()
                .filter_map(NodeOrToken::into_token)
                .filter(|t| keep(t.kind()))
                .collect();
            assert_eq!(
                node.tokens_where(keep).collect::<Vec<_>>(),
                tokens,
                "{ctx}: tokens_where"
            );
            assert_eq!(
                node.first_token_where(keep).as_ref(),
                tokens.first(),
                "{ctx}: first_token_where"
            );
            assert_eq!(
                node.last_token_where(keep).as_ref(),
                tokens.last(),
                "{ctx}: last_token_where"
            );
        }
        typed::<Expr>(&node);
        typed::<Pat>(&node);
        typed::<Type>(&node);
        typed::<TypeDefn>(&node);
    }
}

/// Every two-piece source over the panic sweep's wide alphabet, as an
/// implementation and as a signature file: error recovery makes the empty and
/// oddly nested nodes where an off-by-one in a range scan would show.
#[test]
fn scans_agree_with_the_walks_they_stand_for() {
    use crate::parser_panic_sweep::WIDE;
    for a in WIDE {
        for b in WIDE {
            let src = format!("{a}{b}\n");
            check_source(&src, FileKind::Impl);
            check_source(&src, FileKind::Sig);
        }
    }
}
