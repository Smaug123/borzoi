//! Conditional-compilation directive support.
//!
//! Mirrors FCS's `LexerIfdefExpression` / `pppars.fsy` / `pplex.fsl`. Currently
//! only Stage 1 (expression AST + parser) is present; later stages add the
//! line recogniser, the stateful preprocessor driver, and `#nowarn` / `#line`
//! handling. See `docs/ifdef-plan.md`.

pub mod driver;
pub mod expr;
pub mod line;
pub mod line_store;

pub use driver::{
    Driver, FullTriviaDriver, PreprocError, TriviaToken, lex_with_symbols,
    lex_with_symbols_full_trivia,
};
pub use expr::{Expr, ParseError, ParseErrorKind, parse_if_expr};
pub use line::{
    Directive, DirectiveError, DirectiveErrorKind, DirectiveKind, Recognised, WarningNumber,
    recognise_directive,
};
pub use line_store::{LineDirective, LineDirectiveStore, Remapped};
