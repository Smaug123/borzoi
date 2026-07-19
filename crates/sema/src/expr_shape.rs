//! Shared recognition of expression shapes whose surface identifiers are not
//! ordinary value references.

use borzoi_cst::syntax::{AstNode, Expr};

/// Whether an argument element is a named argument `name = value`.
///
/// F# parses it as `App[InfixApp[name, "="], value]`: an outer non-infix
/// application whose function is the infix `=` operator applied to the label.
/// A positional infix expression (`a + b`) is the infix application itself,
/// while a nested equality inside a record or lambda is not the element's own
/// top-level operator.
pub(crate) fn is_named_arg(element: &Expr) -> bool {
    let Expr::App(outer) = element else {
        return false;
    };
    if outer.is_infix() {
        return false;
    }
    let Some(Expr::App(op_app)) = outer.func() else {
        return false;
    };
    op_app.is_infix()
        && op_app
            .func()
            .is_some_and(|op| op.syntax().text().to_string().trim() == "=")
}
