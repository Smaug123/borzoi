//! Predicates over syntax that more than one part of the walk reads.

use super::*;

/// Whether FCS binds function parameter `p` directly, without elaborating it
/// through a `match` — `SimplePatsOfPat` in `SyntaxTreeOps.fs`: `()`, a
/// (parenthesised) reference tuple of simple elements, or one simple element.
pub(super) fn is_simple_param(p: &Pat) -> bool {
    match p {
        Pat::Tuple(t) if !t.is_struct() => t.elements().all(|el| is_simple_param_element(&el)),
        Pat::Paren(paren) => match paren.inner() {
            Some(Pat::Tuple(t)) if !t.is_struct() => {
                t.elements().all(|el| is_simple_param_element(&el))
            }
            Some(Pat::Const(c)) if c.syntax().text().is_empty() => true,
            _ => is_simple_param_element(p),
        },
        Pat::Const(c) if c.syntax().text().is_empty() => true,
        _ => is_simple_param_element(p),
    }
}

/// One element of a simple parameter — `SimplePatOfPat` without a match: a
/// name, `?name`, `_`, or one of those under parentheses, an annotation or
/// attributes.
fn is_simple_param_element(p: &Pat) -> bool {
    match p {
        Pat::Named(_) | Pat::OptionalVal(_) | Pat::Wildcard(_) => true,
        Pat::Typed(t) => t.pat().is_some_and(|inner| is_simple_param_element(&inner)),
        Pat::Attrib(a) => a.pat().is_some_and(|inner| is_simple_param_element(&inner)),
        Pat::Paren(paren) => paren
            .inner()
            .is_some_and(|inner| is_simple_param_element(&inner)),
        _ => false,
    }
}

/// Whether the named patterns anywhere in `pats` bind some name twice.
pub(super) fn binds_a_name_twice(pats: &[Pat]) -> bool {
    let mut seen = HashSet::default();
    pats.iter()
        .flat_map(|p| p.syntax().descendants_of::<NamedPat>())
        .filter_map(|n| n.ident())
        .any(|tok| !seen.insert(ident_text(&tok)))
}

pub(super) fn ident_text(tok: &SyntaxToken) -> String {
    let text = tok.text();
    text.strip_prefix("``")
        .and_then(|t| t.strip_suffix("``"))
        .unwrap_or(text)
        .to_string()
}

/// Whether an expression is a **unit** literal `()` — a `CONST_EXPR` whose first
/// token is [`SyntaxKind::LPAREN_TOK`], the multi-token `SynConst.Unit` shape
/// (its [`ConstExpr::literal`](borzoi_cst::syntax::ConstExpr::literal) returns
/// the `(` token, not `None`). Its node spans both parentheses, as FCS's does. A
/// unit method-call argument `s.M()` has no parameters to poison, so
/// [`Gen::infer_method_call`] takes it as zero arguments without walking it.
pub(super) fn is_unit_literal(e: &Expr) -> bool {
    matches!(e, Expr::Const(c) if c.literal().map(|t| t.kind()) == Some(SyntaxKind::LPAREN_TOK))
}

/// Peel transparent parentheses, returning the innermost non-[`Expr::Paren`]
/// expression — or `None` if a `Paren` wraps a recovery hole (no inner
/// expression).
pub(super) fn unparenthesize(e: Expr) -> Option<Expr> {
    let mut cur = e;
    while let Expr::Paren(p) = cur {
        cur = p.inner()?;
    }
    Some(cur)
}

/// Whether `file` has no `module` or `namespace` header: its one fragment is
/// the implicit module FCS names after the file.
pub(super) fn is_headerless(file: &ImplFile) -> bool {
    matches!(
        &file.modules().collect::<Vec<_>>()[..],
        [only] if only.kind() == ModuleOrNamespaceKind::Anon
    )
}
