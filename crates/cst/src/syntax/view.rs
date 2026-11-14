//! Reading a parse through a per-version facade
//! (`docs/ast-versioning-plan.md` D4): the sealed [`Facade`] trait and
//! [`Parse::view`](crate::parser::Parse::view)'s error.

use crate::language_version::LanguageVersion;
use crate::syntax::AstNode;

/// Keeps [`Facade`] implemented only by the generated facades.
pub trait Sealed {}

/// A per-version facade's root, `vN::ParsedInput`: what
/// [`Parse::view`](crate::parser::Parse::view) returns. Implemented by each
/// generated facade (`syntax::v4_6`, …, `syntax::preview`) and nothing else.
pub trait Facade: AstNode + Sealed {
    /// The language versions whose parses this facade reads: its own, and its
    /// aliases' (a version whose surface equals an earlier one's shares that
    /// facade's types).
    const VERSIONS: &'static [LanguageVersion];
}

/// [`Parse::view`](crate::parser::Parse::view) asked a facade to read a parse
/// made at a language version it does not serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewError {
    /// The parse's language version.
    pub parsed: LanguageVersion,
    /// The versions the facade serves.
    pub facade: &'static [LanguageVersion],
}

impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let facade: Vec<String> = self.facade.iter().map(|v| v.to_string()).collect();
        write!(
            f,
            "a parse at F# {} read through the facade for F# {}",
            self.parsed,
            facade.join(" / ")
        )
    }
}

impl std::error::Error for ViewError {}
