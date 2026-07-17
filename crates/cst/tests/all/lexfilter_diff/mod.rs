//! Differential test: our `lexfilter::filter` vs FCS's post-`UseLexFilter`
//! token stream.
//!
//! Mirrors `tests/all/lexer_diff.rs` but drives `tools/fcs-dump tokens-filtered`
//! to get the *parser-facing* stream (with `OffsideLet`, `OffsideBlockBegin`,
//! etc. inserted).
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.
//!
//! This is the lex-filter case group; the cases live in the submodules
//! below, grouped by F# construct. The shared `assert_filtered_streams_match`
//! harness lives in `tests/all/common/mod.rs`.

mod assignment;
mod closers;
mod comments_strings;
mod computation_expr;
mod functions_exceptions;
mod ident_adjacency;
mod interface_head;
mod lambdas;
mod lazy_assert;
mod let_if;
mod loops;
mod match_try;
mod modules;
mod quotations;
mod ranges;
mod record_update;
mod semisemi;
mod seqblock_continuation;
mod typars;
mod type_definitions;
