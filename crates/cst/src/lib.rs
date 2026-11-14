// Docs across the crate (each `SyntaxKind` names the union accessor that reads
// it) link into the union typed facade, which is public only under
// `unstable-union`. Without the feature those links point at crate-private
// items; with it they are checked like any other, and any other link to a
// private item stays private under both, so the all-features doc gate still
// catches it.
#![cfg_attr(
    not(feature = "unstable-union"),
    allow(rustdoc::private_intra_doc_links)
)]

pub mod directives;
pub mod language_version;
pub mod lexer;
pub mod lexfilter;
pub mod parser;
pub mod syntax;
pub mod tokens;
