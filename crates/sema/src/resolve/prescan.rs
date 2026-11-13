//! What a pass over the whole file decides before the walk starts
//! ([`Prescan`]). The walk borrows it and cannot write it, so a fact computed
//! here holds at every position of the file, whatever the walk has reached.

use borzoi_cst::syntax::{ExceptionDefnDecl, SyntaxNode, TypeDefn};

use super::{exception_named_like_core_attribute, type_defn_renames_core_attribute};

/// The file's prescan facts; see the [module docs](self).
#[derive(Debug)]
pub(super) struct Prescan {
    /// Whether this file declares a type spelled like `[<AutoOpen>]` or
    /// `[<RequireQualifiedAccess>]`, or an abbreviation of one, or an
    /// exception so named — which makes every written attribute name
    /// untrustworthy for those two
    /// ([`Resolver::attribute_spellings_trusted`](super::state::Resolver::attribute_spellings_trusted)).
    /// Whole-file, so a `[<AutoOpen>]` written before the alias is declared
    /// is as untrustworthy as one written after it.
    pub(super) declares_core_attribute_alias: bool,
}

impl Prescan {
    /// The prescan of the file rooted at `root`.
    pub(super) fn of(root: &SyntaxNode) -> Prescan {
        Prescan {
            declares_core_attribute_alias: declares_core_attribute_alias(root),
        }
    }
}

/// Whether the file rooted at `root`, an implementation or a signature,
/// declares a type or an exception that may alias `[<AutoOpen>]` or
/// `[<RequireQualifiedAccess>]` ([`Prescan::declares_core_attribute_alias`]).
pub(super) fn declares_core_attribute_alias(root: &SyntaxNode) -> bool {
    root.descendants_of::<TypeDefn>()
        .any(|defn| type_defn_renames_core_attribute(&defn))
        || root
            .descendants_of::<ExceptionDefnDecl>()
            .any(|exn| exception_named_like_core_attribute(&exn))
}
