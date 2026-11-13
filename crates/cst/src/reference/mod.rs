//! Test-only reference implementations of the token pipeline's stages, frozen
//! before the stages of `docs/token-pipeline-soa-plan.md` that replace them
//! (D7), and compiled only under the `token-reference` feature.
//!
//! [`lexfilter`] is a verbatim copy of `crate::lexfilter` and [`sign_fold`] of
//! the parser's sign-folding pass, as they stood before the filter changed,
//! fed by the full-trivia driver iterator. Under the feature, every parse
//! asserts that its filtered stream, offside diagnostics and
//! language-version flag equal [`filtered_stream`]'s, so each later stage is
//! held to the code it replaced on every input any test parses.
//!
//! Nothing here may change except to delete it.

// The copies keep the items the live code needed, whether or not the
// reference uses them.
#![allow(dead_code)]

use std::collections::HashSet;

use crate::directives::{PreprocError, TriviaToken, lex_with_symbols_full_trivia};
use crate::language_version::LanguageVersion;

pub mod lexfilter;
pub mod sign_fold;

/// The filtered stream the productions read for `source`, by the reference
/// pipeline: the frozen filter over the active tokens of the full-trivia
/// driver's stream, then the frozen sign fold.
pub fn filtered_stream<'src>(
    source: &'src str,
    symbols: &HashSet<String>,
    lang: LanguageVersion,
) -> lexfilter::FilterRun<'src> {
    let raw: Vec<_> = lex_with_symbols_full_trivia(source, symbols)
        .filter(|(res, _)| matches!(res, Ok(_) | Err(PreprocError::Lex(_))))
        .collect();
    let active = raw.iter().filter_map(|(res, span)| match res {
        Ok(TriviaToken::Lexed(t)) => Some((Ok(t.clone()), span.clone())),
        Err(PreprocError::Lex(e)) => Some((Err(*e), span.clone())),
        _ => None,
    });
    let mut run = lexfilter::filter_collect(source, lang, active);
    run.tokens = sign_fold::fold_adjacent_signs(source, &raw, run.tokens);
    run
}
