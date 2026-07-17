//! Panic-safe wrappers over the CST parser.
//!
//! The parser is hand-written recursive descent; on a (rare) malformed input
//! one of its internal invariant guards may fire. The diagnostic-publish path
//! ([`crate::diagnostics`]) has wrapped the parser call in [`catch_unwind`]
//! since it was first wired up; every other LSP-side caller (request handlers,
//! the per-project fold) needs the same guard or a parser panic on a stray
//! buffer will unwind through the request loop and terminate the server.
//!
//! This module centralises the policy so the wrapper is one named place rather
//! than a copy/paste at every call site.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, Parse, ParseOptions, parse_with_options};

/// [`borzoi_cst::parser::parse_with_options`] for an implementation file,
/// wrapped in [`catch_unwind`]. Returns `None` if the parser panicked so callers
/// can degrade to a no-result answer — never an LSP error envelope, never a
/// server crash. `lang` drives the language-version feature gate (e.g. `#elif`);
/// resolve it with [`crate::workspace::Workspace::lang_version_for`].
pub fn parse_with_symbols(
    text: &str,
    symbols: &HashSet<String>,
    lang: LanguageVersion,
) -> Option<Parse> {
    let opts = ParseOptions {
        file_kind: FileKind::Impl,
        symbols,
        lang,
    };
    match catch_unwind(AssertUnwindSafe(|| parse_with_options(text, opts))) {
        Ok(parsed) => Some(parsed),
        Err(_) => {
            crate::log_warn!("parser panicked; returning no result for this buffer");
            None
        }
    }
}
