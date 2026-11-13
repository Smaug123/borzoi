//! The parse memo: the one place the server parses a source at the settings
//! it is read under (`docs/incremental-lsp-plan.md`, D2 and Stage 5).
//!
//! A parse is a pure function of the text, the file kind (implementation or
//! signature grammar), the `#if` symbols and the language version
//! ([`SourceParse::new`]). [`ParseMemo::parse`] keys each file's parses on
//! exactly those inputs and hands back the remembered one when they all
//! match, the text compared by pointer and then by bytes. So the syntactic
//! diagnostics, the project fold and the single-file handlers share one parse
//! of a text, and a hit is never stale: nothing is invalidated, an entry is
//! simply not what a lookup with other inputs finds.
//!
//! What the parse carries beside the tree is what its readers need from the
//! lex it made: the preprocessor's errors and `#line` directives
//! ([`borzoi_cst::parser::Parse::preprocessor`]), which the diagnostics
//! report. So a source is lexed once, by its parse.
//!
//! The memo counts what it does per generation ([`ParseCounts`]): the parses
//! it ran and the ones it handed back again.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use borzoi_cst::directives::{Preprocessed, preprocess};
use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, Parse};

use crate::paths::{lexically_normalize, path_dedup_key};

/// One parse of one text under one set of inputs, as the memo hands it out.
#[derive(Debug)]
pub struct SourceParse {
    text: Arc<str>,
    file_kind: FileKind,
    symbols: HashSet<String>,
    lang: LanguageVersion,
    /// The parse, or `None` if the parser panicked
    /// ([`crate::cst_panic_safe`]).
    parse: Option<Parse>,
}

impl SourceParse {
    /// Parse `text` from scratch under `file_kind`, `symbols` and `lang`,
    /// panic-safely. What [`ParseMemo::parse`] remembers and must equal.
    pub fn new(
        text: Arc<str>,
        file_kind: FileKind,
        symbols: &HashSet<String>,
        lang: LanguageVersion,
    ) -> SourceParse {
        let parse = crate::cst_panic_safe::parse_kind_with_symbols(&text, symbols, lang, file_kind);
        SourceParse {
            text,
            file_kind,
            symbols: symbols.clone(),
            lang,
            parse,
        }
    }

    /// The text that was parsed.
    pub fn text(&self) -> &Arc<str> {
        &self.text
    }

    /// The grammar it was parsed under.
    pub fn file_kind(&self) -> FileKind {
        self.file_kind
    }

    /// The `#if` symbols it was parsed under.
    pub fn symbols(&self) -> &HashSet<String> {
        &self.symbols
    }

    /// The language version it was parsed under.
    pub fn lang(&self) -> LanguageVersion {
        self.lang
    }

    /// The parse, or `None` if the parser panicked on this text.
    pub fn parse(&self) -> Option<&Parse> {
        self.parse.as_ref()
    }

    /// What the preprocessor reported for the text under the symbols: the
    /// parse's own record of its lex, or, if the parser panicked, a lex made
    /// now. Parser panics are not expected (`parser_panic_sweep`), so that
    /// lex is not remembered.
    pub fn preprocessed(&self) -> Cow<'_, Preprocessed> {
        match &self.parse {
            Some(parse) => Cow::Borrowed(&parse.preprocessor),
            None => Cow::Owned(preprocess(&self.text, &self.symbols)),
        }
    }

    /// Whether this is the parse of `text` under the other inputs: the text
    /// by pointer, else by bytes.
    fn answers(
        &self,
        text: &Arc<str>,
        file_kind: FileKind,
        symbols: &HashSet<String>,
        lang: LanguageVersion,
    ) -> bool {
        self.has_inputs(file_kind, symbols, lang)
            && (Arc::ptr_eq(&self.text, text) || *self.text == **text)
    }

    /// Whether this was parsed under these inputs besides the text.
    fn has_inputs(
        &self,
        file_kind: FileKind,
        symbols: &HashSet<String>,
        lang: LanguageVersion,
    ) -> bool {
        self.file_kind == file_kind && self.lang == lang && self.symbols == *symbols
    }
}

/// What the memo did in one generation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParseCounts {
    /// Sources parsed: lookups the memo did not hold.
    pub parsed: u64,
    /// Lookups answered with a parse the memo held.
    pub reused: u64,
}

/// Every file's latest parse under each set of inputs it was read under.
#[derive(Debug, Default)]
pub struct ParseMemo {
    /// Per file, by [`path_dedup_key`] of its lexically normalised path, the
    /// latest parse under each set of inputs: normally one, and one more for
    /// each other project setting a file linked into two projects is read
    /// under. A parse of a new text replaces the one under the same inputs,
    /// so editing a file does not grow its list.
    files: HashMap<String, Vec<Arc<SourceParse>>>,
    counts: ParseCounts,
}

impl ParseMemo {
    /// The parse of `text` under `file_kind`, `symbols` and `lang`: the one
    /// remembered for `path` under exactly those inputs, else a fresh one,
    /// which is remembered in place of the one under the same inputs. With no
    /// `path` (a buffer that names no file) the parse is not remembered.
    pub fn parse(
        &mut self,
        path: Option<&Path>,
        text: &Arc<str>,
        file_kind: FileKind,
        symbols: &HashSet<String>,
        lang: LanguageVersion,
    ) -> Arc<SourceParse> {
        let Some(path) = path else {
            self.counts.parsed += 1;
            return Arc::new(SourceParse::new(Arc::clone(text), file_kind, symbols, lang));
        };
        let parses = self
            .files
            .entry(path_dedup_key(&lexically_normalize(path)))
            .or_default();
        if let Some(hit) = parses
            .iter()
            .find(|parse| parse.answers(text, file_kind, symbols, lang))
        {
            self.counts.reused += 1;
            return Arc::clone(hit);
        }
        self.counts.parsed += 1;
        let fresh = Arc::new(SourceParse::new(Arc::clone(text), file_kind, symbols, lang));
        match parses
            .iter_mut()
            .find(|parse| parse.has_inputs(file_kind, symbols, lang))
        {
            Some(slot) => *slot = Arc::clone(&fresh),
            None => parses.push(Arc::clone(&fresh)),
        }
        fresh
    }

    /// Forget every parse. Correctness never needs it (a lookup compares
    /// every input); it bounds memory on a structural change.
    pub fn clear(&mut self) {
        self.files.clear();
    }

    /// Start counting a new generation.
    pub fn begin_generation(&mut self) {
        self.counts = ParseCounts::default();
    }

    /// What the memo did since the generation began, and start afresh.
    pub fn end_generation(&mut self) -> ParseCounts {
        std::mem::take(&mut self.counts)
    }
}

#[cfg(test)]
mod tests;
