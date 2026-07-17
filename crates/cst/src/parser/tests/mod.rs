//! Parser tests, extracted verbatim from the former inline `mod tests`
//! in `parser/mod.rs` and split by grammar area. The shared tree-rendering
//! helpers live here so every thematic submodule can reach them via
//! `use super::*`.

mod bindings;
mod control_flow;
mod expressions;
mod interp;
mod literals;
mod patterns;
mod reserved_idents;
mod structure;
mod tabs;
mod types;

use super::*;

/// Render the tree as the rust-analyzer-style indented S-expression so
/// shape assertions are easy to eyeball.
fn debug_tree(node: &SyntaxNode) -> String {
    let mut s = String::new();
    fmt_tree(node, 0, &mut s);
    s
}

fn fmt_tree(node: &SyntaxNode, indent: usize, out: &mut String) {
    out.push_str(&"  ".repeat(indent));
    out.push_str(&format!("{:?}@{:?}\n", node.kind(), node.text_range()));
    for child in node.children_with_tokens() {
        match child {
            rowan::NodeOrToken::Node(n) => fmt_tree(&n, indent + 1, out),
            rowan::NodeOrToken::Token(t) => {
                out.push_str(&"  ".repeat(indent + 1));
                out.push_str(&format!(
                    "{:?}@{:?} {:?}\n",
                    t.kind(),
                    t.text_range(),
                    t.text()
                ));
            }
        }
    }
}

/// Is any descendant node (including the root) of the given
/// [`SyntaxKind`]? Useful for "X must not appear anywhere in the tree"
/// guard tests (e.g. "no INFIX_APP_EXPR for an adjacent-prefix op").
fn tree_contains_kind(root: &SyntaxNode, kind: SyntaxKind) -> bool {
    root.descendants().any(|n| n.kind() == kind)
}

/// Round-trip property: concatenating all token texts in the green tree
/// reproduces the source exactly. This is the lossless invariant the
/// trivia pipeline is supposed to maintain.
fn assert_lossless(source: &str, parse: &Parse) {
    let mut reconstructed = String::new();
    for el in parse.root.descendants_with_tokens() {
        if let rowan::NodeOrToken::Token(t) = el {
            reconstructed.push_str(t.text());
        }
    }
    assert_eq!(reconstructed, source, "green tree text != source");
}

/// Compact shape of the (single) `INTERP_STRING_EXPR` in `parse`:
/// each `Fragment` part contributes its source text, each `Fill` part
/// contributes the literal marker `<fill>`. Lets the multi-fill tests
/// assert the alternating fragment/fill sequence without pinning exact
/// byte spans.
fn interp_part_shapes(parse: &Parse) -> Vec<String> {
    use crate::syntax::{AstNode, InterpStringExpr, InterpStringPart};
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::INTERP_STRING_EXPR)
        .expect("an INTERP_STRING_EXPR node");
    InterpStringExpr::cast(node)
        .expect("cast INTERP_STRING_EXPR")
        .parts()
        .into_iter()
        .map(|p| match p {
            InterpStringPart::Fragment(t) => t.text().to_string(),
            InterpStringPart::Fill { .. } => "<fill>".to_string(),
        })
        .collect()
}

/// The `: ident` format qualifier of each `Fill` part of the (single)
/// `INTERP_STRING_EXPR` in `parse`, in source order — `Some(text)` when the
/// fill carries a qualifier (`{x:N2}`), `None` otherwise. Fragments are
/// skipped. Lets the qualifier-association tests pin which fill owns which
/// qualifier without pinning fragment spans.
fn interp_fill_qualifiers(parse: &Parse) -> Vec<Option<String>> {
    use crate::syntax::{AstNode, InterpStringExpr, InterpStringPart};
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::INTERP_STRING_EXPR)
        .expect("an INTERP_STRING_EXPR node");
    InterpStringExpr::cast(node)
        .expect("cast INTERP_STRING_EXPR")
        .parts()
        .into_iter()
        .filter_map(|p| match p {
            InterpStringPart::Fragment(_) => None,
            InterpStringPart::Fill { qualifier, .. } => {
                Some(qualifier.map(|t| t.text().to_string()))
            }
        })
        .collect()
}

/// Count `INTERP_STRING_EXPR` nodes anywhere in the tree. Nested interp
/// produces one node per interp string, so this pins that the inner
/// string was recovered as its own (nested) interp node rather than
/// being dropped or folded into the outer fragment.
fn interp_node_count(parse: &Parse) -> usize {
    parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::INTERP_STRING_EXPR)
        .count()
}

/// Extract the head pattern of the first binding in the first
/// (anonymous) module of `root`. Shared by the `as`-pattern unit tests.
fn first_binding_head_pat(root: &SyntaxNode) -> crate::syntax::Pat {
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let");
    };
    let binding = let_decl.bindings().next().expect("binding");
    binding.pat().expect("binding pat")
}

/// Count the direct token children of `node` with the given `kind`.
/// Used by the list/array-pattern unit tests to assert delimiter /
/// separator presence without pinning the full green shape.
fn count_tok(node: &SyntaxNode, kind: SyntaxKind) -> usize {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == kind)
        .count()
}

/// Count tokens of the given `kind` *anywhere* in the tree (not just direct
/// children). Used by the top-level `;;`-separator tests to assert how many
/// `SEMISEMI_TOK`s the decl loop emitted.
fn token_count(root: &SyntaxNode, kind: SyntaxKind) -> usize {
    root.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == kind)
        .count()
}

/// Assert no `DOT_TOK` / `IDENT_TOK` in the tree is a zero-width
/// (virtual-backed) emission. `bump_into` emits virtuals with an
/// empty text slice, so a layout virtual mis-consumed as a path
/// token shows up as an empty-text DOT/IDENT.
fn assert_no_empty_path_tokens(parse: &Parse) {
    for tok in parse
        .root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
    {
        if matches!(tok.kind(), SyntaxKind::DOT_TOK | SyntaxKind::IDENT_TOK) {
            assert!(
                !tok.text().is_empty(),
                "zero-width {:?} — a layout virtual was mis-consumed; tree:\n{}",
                tok.kind(),
                debug_tree(&parse.root),
            );
        }
    }
}

/// Assert no ERROR token in the tree carries source text. The
/// swallowed-`)` paren recovery legitimately emits *zero-width*
/// ERROR markers (even on an error-free parse), but a *non-empty*
/// ERROR means a real token (e.g. a drained `)`) was swallowed into
/// the error node — the corruption these trailing-dot tests guard
/// against.
fn assert_no_nonempty_error_tokens(parse: &Parse) {
    for tok in parse
        .root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
    {
        if tok.kind() == SyntaxKind::ERROR {
            assert!(
                tok.text().is_empty(),
                "non-empty ERROR token {:?} — a real token was drained; tree:\n{}",
                tok.text(),
                debug_tree(&parse.root),
            );
        }
    }
}

/// Every version-gated kind (one whose `kind_interval` carries an `introduced`)
/// must have an FS3350 feature name, so [`node_surface_diagnostics`] never has to
/// silently skip a gated node for want of a message. Iterates the whole
/// `SyntaxKind` range, so a future gated kind added without a name fails here.
#[test]
fn every_gated_kind_has_a_feature_name() {
    for raw in 0..(SyntaxKind::__LAST as u16) {
        let kind = SyntaxKind::from_raw(raw).expect("raw below __LAST is a kind");
        if kind_interval(kind).introduced.is_some() {
            assert!(
                feature_name_for_kind(kind).is_some(),
                "gated kind {kind:?} has no FS3350 feature name",
            );
        }
    }
}
