//! Hand-written recursive-descent parser over the [`FilteredToken`] stream.
//!
//! Per `docs/parser-plan.md`, the parser builds a rowan green tree via
//! [`GreenNodeBuilder`] and exposes a typed AST in [`crate::syntax`]. Phase 2
//! grows the Phase 1 scaffold (empty file + lone int literal) one literal
//! form at a time; see the table in `docs/parser-plan.md` for the running
//! list of supported `SynConst` variants.
//!
//! This module owns the public entry points ([`parse`], [`parse_with_symbols`]),
//! the [`Parse`] result type, and the `Parser` state struct. The productions
//! themselves are a single `impl Parser` spread across thematic submodules
//! (each declares its own `impl<'src> Parser<'src>` block over the same struct):
//! `cursor` (token-stream/emission primitives), `decls` (module/`open`/`let`
//! structure), `pat` (patterns), `strings` (interpolated strings), `types`
//! (types), and the expression grammar — `expr` (entry/Pratt core +
//! predicates) plus its facets `expr_control` (`if`/`match`/`fun`/`function`),
//! `expr_op` (prefix/infix operators), `expr_app` (application), and
//! `expr_atom` (atomic forms). Methods are `pub(super)` so
//! the productions can call across submodules while staying private to the
//! crate. Numeric-literal and token classification helpers live in `numeric`
//! and `classify`.
//!
//! Full-fidelity strategy (plan D7): LexFilter drops trivia and inserts
//! zero-or-more `Virtual` tokens — but the green tree must satisfy
//! `text(tree) == source` for LSP range fidelity. We therefore walk the
//! *raw* lexer stream in lockstep with the filtered stream:
//!
//! * Trivia raw tokens (whitespace, newlines, comments) are emitted at their
//!   source byte range with their natural kind.
//! * Non-trivia raw tokens are aligned with the corresponding filtered Raw
//!   token (same span) and emitted with the kind chosen by the production
//!   that consumed the filtered token. Raw tokens swallowed by LexFilter
//!   (not surfaced as filtered tokens at all) fall through to ERROR — they
//!   stay in the tree, just unparsed.
//! * Virtual filtered tokens get the *same source span* as the real token
//!   they were inserted at (see `Filter::insert_token`), so we must NOT
//!   read source bytes from that span — instead we emit a zero-width token
//!   so the lossless invariant holds.

use std::collections::HashSet;
use std::ops::Range;

use rowan::{GreenNodeBuilder, Language};

use crate::directives::{PreprocError, TriviaToken, lex_with_symbols_full_trivia};
use crate::language_version::LanguageVersion;
use crate::lexer::{LexError, Token};
use crate::lexfilter::{
    FilteredToken, OffsideDiagnostic, OffsideSeverity, Virtual, filter_collect,
};
use crate::syntax::{FSharpLang, SyntaxKind, SyntaxNode, kind_interval};

/// FCS's `parsAugmentationsIllegalOnDelegateType` (`FSComp.txt:545`) — a
/// delegate type definition (`type T = delegate of …`) admits no augmentation,
/// whether written as a trailing `with` block or as bare trailing members.
/// Shared by the type-defn ([`decls_type`]) and signature ([`decls_sig`])
/// productions.
const DELEGATE_AUGMENTATION_ERROR: &str =
    "Augmentations are not permitted on delegate type moduleDefns";

/// Phase 10.14: every `SynTypeDefnSigRepr` body form is now modelled — the
/// `SynTypeDefnSimpleRepr` reprs (abbreviation `type T = <ty>`, slice 1;
/// opaque/bodyless `type T`, slice 2a; record / union / enum, slice 2b), an
/// object-model body of `member`/`abstract`/`static member` / `val`-field /
/// `inherit` / `interface` signatures (slices 3a/3b), an explicit-kind body
/// (`class`/`struct`/`interface … end`, slice 3c), a bodyless `with`-augmentation
/// `type T with member …` (slice 4, outer-slot member sigs), a trailing
/// `with`/bare-member sig on a structural repr (`type R = {…} with member …`,
/// slice 6, also outer-slot), a `delegate of …` body (slice 7,
/// [`SyntaxKind::DELEGATE_REPR`]), and attributed member sigs (slice 8). This
/// diagnostic now fires only for a residual *member-sig kind* the member-block
/// loop does not model (an attributed `inherit`/`interface`, or a malformed body),
/// which is skipped so a nested spec is not leaked as a top-level decl. Shared by
/// the signature ([`decls_sig`]) and recovery ([`decls_recover`]) productions.
const TYPE_SIG_UNSUPPORTED_BODY_ERROR: &str = "this signature type-definition body form is not yet supported (later \
     phase-10.14 slice)";

/// One item in an object-model type body (phases 9.7–9.9b). Distinguishes the
/// terminator shape: a `Member`/`ClassLet` has an offside `= <expr>` RHS block
/// (it leaves a RHS-close `OBLOCKEND`), whereas a `ValField` has no RHS block
/// (it leaves only an `OBLOCKSEP` before the next item).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ObjectModelItem {
    /// `[static] member …` (phases 9.7/9.9a) — a `MEMBER_DEFN`.
    Member,
    /// A class-local `let`/`let rec` (phase 9.8b) — a `MEMBER_LET_BINDINGS`.
    ClassLet,
    /// A `static let`/`static let rec` (phase 9.8c) — the same
    /// `MEMBER_LET_BINDINGS` as a [`ClassLet`](Self::ClassLet) with a leading
    /// `STATIC_TOK` (FCS's `STATIC classDefnBindings`, `pars.fsy:2009`). Same
    /// offside `= <expr>` RHS-block terminator shape as a `ClassLet`.
    StaticClassLet,
    /// A class-body `do <expr>` binding (phase 9.8d) — a `MEMBER_DO`. FCS's
    /// `do`-binding `classDefnBindings` arm (`SynMemberDefn.LetBindings([Do …])`).
    /// The reused [`Self::parse_do_expr`] self-consumes the `do`'s offside block
    /// and trailing `ODECLEND`, so it takes the no-RHS-block terminator.
    Do,
    /// A class-body `static do <expr>` binding (phase 9.8d) — the same `MEMBER_DO`
    /// as a [`Do`](Self::Do) with a leading `STATIC_TOK` (FCS's `StaticDo`).
    StaticDo,
    /// `[static] val …` (phase 9.9b) — a `VAL_FIELD` (no `= <expr>` RHS).
    ValField,
    /// `[static] member val …` (phase 9.9c) — an `AUTO_PROPERTY`.
    AutoProperty,
    /// `new(args) = …` (phase 9.10b) — an explicit constructor, a `MEMBER_DEFN`
    /// whose head is the `new` keyword.
    NewCtor,
    /// `abstract [member] M : T` (phase 9.10c) — an abstract slot (`ABSTRACT_SLOT`,
    /// no `= <expr>` RHS).
    AbstractSlot,
    /// `inherit Base[(args)] [as base]` (phase 9.11a) — a base-class clause
    /// (`INHERIT_MEMBER`, no `= <expr>` RHS).
    Inherit,
    /// `interface I [with member …]` (phase 9.11b) — an interface implementation
    /// (`INTERFACE_IMPL`). The `with member …` block self-drains its own close
    /// virtuals (via the shared with-augment loop), so like a `val` field it
    /// takes the no-RHS-block terminator.
    Interface,
}

mod classify;
mod cursor;
mod decls;
mod decls_binding;
mod decls_member;
mod decls_recover;
mod decls_repr;
mod decls_sig;
mod decls_type;
mod escapes;
mod expr;
mod expr_app;
mod expr_atom;
mod expr_control;
mod expr_op;
mod measure;
mod numeric;
mod pat;
mod sign_fold;
mod strings;
mod types;

use classify::{
    byte_interp_lit_kind, classify_op_text, ident_text_leads_uppercase, ident_token_text,
    int32_exponent_is_one, int32_exponent_is_zero, is_paren_operator_name, is_prefix_op_text,
    raw_after_lparen_starts_expr, raw_is_trivia, raw_significant, raw_starts_anon_recd_type,
    raw_starts_atomic_expr, raw_starts_atomic_pat, raw_starts_atomic_type,
    raw_starts_attribute_arg, raw_starts_const_payload, raw_starts_minus_expr,
    raw_starts_pat_element, raw_starts_postfix_app_head, raw_trivia_kind,
    token_is_folded_signed_literal, token_is_int32_exponent, trivia_kind,
};
use escapes::{
    ByteCharVerdict, MAX_BMP_CODE_UNIT, MAX_UNICODE_SCALAR, byte_string_wide_unit_count,
    classify_byte_char, long_unicode_escapes,
};
use numeric::{
    DecimalIntError, IntSuffixedError, classify_suffixed_int, float32_body_parses,
    separators_well_placed, validate_decimal_int, validate_xieee32, validate_xieee64,
    validate_xint_int32,
};
use pat::PatCtx;

/// Outcome of a parse. Holds the tree plus diagnostics partitioned by
/// severity; the tree is always present (errors become [`SyntaxKind::ERROR`]
/// tokens) so consumers don't need to plumb a [`Result`] through.
///
/// `errors` and `warnings` mirror FCS's two parse-time severities (an `errors`
/// entry corresponds to FCS's `ParseHadErrors`; `warnings` are diagnostics FCS
/// emits via `warning(...)` that leave `hadErrors` false). They are kept as
/// separate vectors rather than a `severity` field on [`ParseError`] so the
/// ~200 existing error-construction sites — and the `errors.is_empty()`
/// error-parity checks in the differential test harness — stay unchanged.
///
/// Not yet honoured: `#nowarn` / `#warnon` suppression. FCS drops a warning
/// whose number is disabled at its position; `warnings` here are unfiltered, so
/// a consumer would surface a warning the user silenced. The directive layer
/// already parses these directives (`directives`' `Directive::NoWarn` /
/// `WarnOn`); wiring scoped suppression through to `warnings` is a follow-up.
#[derive(Debug, Clone)]
pub struct Parse {
    pub root: SyntaxNode,
    pub errors: Vec<ParseError>,
    pub warnings: Vec<ParseError>,
    /// The language version this tree was parsed against. The convenience
    /// entry points ([`parse`], [`parse_with_symbols`], …) default to
    /// [`LanguageVersion::Preview`] — every implemented feature on, i.e. the
    /// parser's historical "no version gating" behaviour — so existing callers
    /// are unaffected. [`parse_with_options`] threads a chosen version through.
    /// It drives the `#elif` legality gate (diagnostics only — see
    /// `docs/ast-versioning-plan.md`) and, since the offside-diagnostics
    /// emission stages, the lex-filter's strict-indentation push decision —
    /// which **can** change the tree shape across the F# 8 boundary;
    /// [`Parse::shape_depends_on_language_version`] reports whether it
    /// actually did for this source.
    pub lang: LanguageVersion,
    /// Whether this parse's tree shape depends on [`Parse::lang`]: the
    /// lex-filter reached a strict-indentation decision point
    /// (an offside version-gated context push — aborted at F# 8+, kept with
    /// a warning below) whose outcome differs across language versions.
    /// `false` **proves** the tree is identical under every
    /// [`LanguageVersion`] (see
    /// `crate::lexfilter::FilterRun::shape_depends_on_language_version` for
    /// the argument and for `true`'s over-approximation caveat — verify
    /// against a parse from the other side of the F# 8 boundary before
    /// paying a real cost for `true`), so a consumer that cannot pin the
    /// project's real language version may still trust an unflagged parse;
    /// diagnostics can still differ by version (FS0058 severity, `#elif`
    /// legality) — only the *shape* guarantee is claimed.
    pub shape_depends_on_language_version: bool,
}

/// A parse-time problem. `span` is a byte range into the input source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub span: Range<usize>,
}

/// The raw stream the parser walks for losslessness: the byte-complete
/// full-trivia preprocessor stream, covering *every* source byte as either a
/// real [`Token`] ([`TriviaToken::Lexed`]) or a directive / inactive-code
/// trivia marker.
type RawTok<'src> = (Result<TriviaToken<'src>, PreprocError>, Range<usize>);
type FilteredTok<'src> = (Result<FilteredToken<'src>, LexError>, Range<usize>);

/// Parse an F# source string with no preprocessor symbols defined — every
/// `#if <ident>` is false, so the active branch is the `#else` / post-`#endif`
/// code. Always returns a tree — see [`Parse`]. For project-aware parsing
/// (real `<DefineConstants>`), use [`parse_with_symbols`].
pub fn parse(source: &str) -> Parse {
    parse_with_symbols(source, &HashSet::new())
}

/// Parse an F# *signature* file (`.fsi`) — the `SIG_FILE` root holding
/// `SynModuleOrNamespaceSig`s (phase 10.11). Same lexer/lex-filter pipeline as
/// [`parse`]; only the top-level production differs (type-only specifications
/// instead of definitions). No preprocessor symbols defined — see [`parse`].
pub fn parse_sig(source: &str) -> Parse {
    parse_sig_with_symbols(source, &HashSet::new())
}

/// Parse an F# source string against a preprocessor symbol set. The tree
/// reflects only the **active** `#if` branches; directive lines and dead
/// (`#if`-eliminated) regions are kept as trivia tokens, so the tree stays
/// lossless (`text(tree) == source`) but its structural nodes are those of
/// the selected compilation.
pub fn parse_with_symbols(source: &str, symbols: &HashSet<String>) -> Parse {
    parse_inner(source, symbols, FileKind::Impl, LanguageVersion::Preview)
}

/// Signature-file counterpart of [`parse_with_symbols`] — emits the `SIG_FILE`
/// root (phase 10.11).
pub fn parse_sig_with_symbols(source: &str, symbols: &HashSet<String>) -> Parse {
    parse_inner(source, symbols, FileKind::Sig, LanguageVersion::Preview)
}

/// Selects the implementation- vs signature-file top-level production for a
/// parse. The lexer/lex-filter pipeline is identical; only the final entry
/// point (`parse_impl_file` vs `parse_sig_file`) differs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileKind {
    /// Implementation file (`.fs`) — `parse_impl_file`.
    Impl,
    /// Signature file (`.fsi`) — `parse_sig_file`.
    Sig,
}

/// Everything a parse depends on besides the source text: the file kind, the
/// preprocessor symbol set, and the language version to gate against. The
/// single general entry point [`parse_with_options`] takes one of these; the
/// other `parse*` functions are convenience wrappers that fill it in (file kind
/// from which they are, no symbols, [`LanguageVersion::Preview`]).
///
/// Consolidating the axes here keeps the entry-point matrix from growing
/// combinatorially as parameters accrue (see `docs/ast-versioning-plan.md` D3).
#[derive(Clone, Copy)]
pub struct ParseOptions<'a> {
    /// Implementation- vs signature-file top-level production.
    pub file_kind: FileKind,
    /// Defined `#if` symbols (e.g. evaluated `<DefineConstants>`). Empty means
    /// every `#if <ident>` is false.
    pub symbols: &'a HashSet<String>,
    /// The language version to parse against. Drives the legality gate (today,
    /// only `#elif`); recorded on [`Parse::lang`].
    pub lang: LanguageVersion,
}

/// Parse with explicit [`ParseOptions`] — the general entry point that the
/// other `parse*` functions wrap. Use this to parse against a specific
/// [`LanguageVersion`] (e.g. an `.fsproj` `<LangVersion>` pin).
pub fn parse_with_options(source: &str, opts: ParseOptions<'_>) -> Parse {
    parse_inner(source, opts.symbols, opts.file_kind, opts.lang)
}

fn parse_inner(
    source: &str,
    symbols: &HashSet<String>,
    file_kind: FileKind,
    lang: LanguageVersion,
) -> Parse {
    // Non-`Lex` preprocessor errors — malformed directive lines (`#if *FOO*`),
    // orphan `#else`/`#elif`/`#endif`, bad chains, and `#if` unclosed at EOF —
    // are dropped from the raw stream but re-attached to `Parse::errors` below.
    // Dropping them from the raw stream is load-bearing: their bytes are already
    // covered by the directive's trivia token (so keeping them would double-count
    // in the lossless drain), and a raw `Err` acts as a phantom stopper in the
    // productions' raw lookahead (which treats any `Err` as a hard stop).
    // Surfacing them in the error list mirrors FCS, which treats a malformed
    // directive as a compile error. The LSP's parser-diagnostics producer dedups
    // these against its lexer producer by `PreprocError::reporting_span`, so the
    // user still sees one squiggle, not two. Active lex failures
    // (`PreprocError::Lex`) are kept in the raw stream — they are genuine breaks
    // that surface as ERROR nodes.
    let mut directive_errors: Vec<ParseError> = Vec::new();
    let mut full_trivia = lex_with_symbols_full_trivia(source, symbols);
    let raw_tokens: Vec<RawTok<'_>> = full_trivia
        .by_ref()
        .filter(|(res, span)| match res {
            Ok(_) | Err(PreprocError::Lex(_)) => true,
            Err(e) => {
                directive_errors.push(ParseError {
                    message: e.diagnostic_message(),
                    span: e.reporting_span(span.clone()),
                });
                false
            }
        })
        .collect();
    // The driver, now drained, has recorded every `#elif` directive FCS
    // feature-checks (all branches/depths, separator required). Snapshot the
    // spans before `full_trivia` is dropped; `langversion_diagnostics` turns
    // them into errors when `lang` predates 11.0.
    let elif_directives: Vec<Range<usize>> = full_trivia.elif_directives().to_vec();
    // The active-branch real tokens are the substream LexFilter and the
    // productions consume. Directive / inactive-code trivia are not active
    // code, so they are dropped here; the parser still emits them from
    // `raw_tokens` for losslessness.
    let filter_run = {
        let active = raw_tokens.iter().filter_map(|(res, span)| match res {
            Ok(TriviaToken::Lexed(t)) => Some((Ok(t.clone()), span.clone())),
            Err(PreprocError::Lex(e)) => Some((Err(e.clone()), span.clone())),
            _ => None,
        });
        filter_collect(source, lang, active)
    };
    let filtered_tokens: Vec<FilteredTok<'_>> = filter_run.tokens;
    let offside_diagnostics: Vec<OffsideDiagnostic> = filter_run.diagnostics;
    let shape_depends_on_language_version = filter_run.shape_depends_on_language_version;
    // Fold an adjacent `+`/`-` into the following numeric literal, mirroring
    // FCS's token-layer sign fold (`LexFilter.fs:2694`). Done before the
    // parser runs so a signed literal is a single token in both expression
    // and pattern position. See `sign_fold`.
    let filtered_tokens = sign_fold::fold_adjacent_signs(source, &raw_tokens, filtered_tokens);

    // FS1161 tabs: a lexer-level diagnostic, scanned off the active-code
    // whitespace trivia before `raw_tokens` is consumed by the parser. Merged
    // into `errors` below (and sorted into source order alongside the directive
    // and production errors).
    let tab_errors = tab_diagnostics(source, &raw_tokens);

    // FS0046 reserved-identifier warnings: scanned off the same active-code
    // tokens. A *warning* (leaves `ParseHadErrors` false), so it never affects
    // the parse-error parity the differential harness checks.
    let reserved_warnings = reserved_ident_diagnostics(&raw_tokens);

    // Language-version legality gate. The version never threads into the
    // preprocessor — the driver records the gated `#elif` spans, and we turn
    // them into diagnostics here — so the tree shape is identical regardless of
    // `lang`. Mirrors FCS, which gates `#elif` with `CheckLanguageFeatureAndRecover`:
    // it reports the feature error and recovers, leaving the parse unchanged.
    let langversion_errors = langversion_diagnostics(&elif_directives, lang);

    let mut parser = Parser::new(source, raw_tokens, filtered_tokens);
    match file_kind {
        FileKind::Impl => parser.parse_impl_file(),
        FileKind::Sig => parser.parse_sig_file(),
    }
    // If the recursion-depth guard fired, the parse stopped at the breach and
    // drained the rest of the input to EOF; the unwinding productions emitted a
    // cascade of "expected closer" errors against that EOF. Collapse the whole
    // diagnostic set to the single depth error — the file is pathologically
    // nested and already an error; one characterised diagnostic is the useful
    // signal. (The tree stays lossless regardless.)
    if parser.depth_limit_hit {
        let span = parser.depth_limit_span.clone().unwrap_or(0..source.len());
        return Parse {
            root: SyntaxNode::new_root(parser.builder.finish()),
            errors: vec![ParseError {
                message: "expression, type, or pattern nesting is too deep; parsing stopped here"
                    .to_string(),
                span,
            }],
            warnings: Vec::new(),
            lang,
            shape_depends_on_language_version,
        };
    }
    // Build the root now: the node-surface gate below walks it for typed-node
    // features that are out of surface at `lang` (today: nullness).
    let root = SyntaxNode::new_root(parser.builder.finish());
    // Merge the directive errors (collected in source order above), the FS1161
    // tab errors (scanned off the whitespace trivia), the `#elif` trivia gate,
    // and the typed-node surface gate with the productions' errors (emitted in
    // parse order), then re-sort by span start so the final list reads in source
    // order regardless of which producer found each problem.
    let mut errors = parser.errors;
    errors.extend(directive_errors);
    errors.extend(tab_errors);
    errors.extend(langversion_errors);
    errors.extend(node_surface_diagnostics(&root, lang));
    // Warnings come from the productions and (once the emission stages land) the
    // lex-filter's offside diagnostics; directive/tab/langversion problems are
    // all errors.
    let mut warnings = parser.warnings;
    warnings.extend(reserved_warnings);
    // The lex-filter's offside / indentation diagnostics (FS0058), split by the
    // severity it resolved from `lang`. Empty until the emission stages of
    // `docs/offside-diagnostics-plan.md`.
    for diag in offside_diagnostics {
        let err = ParseError {
            message: diag.message,
            span: diag.span,
        };
        match diag.severity {
            OffsideSeverity::Error => errors.push(err),
            OffsideSeverity::Warning => warnings.push(err),
        }
    }
    let span_order = |a: &ParseError, b: &ParseError| {
        a.span
            .start
            .cmp(&b.span.start)
            .then(a.span.end.cmp(&b.span.end))
    };
    errors.sort_by(&span_order);
    warnings.sort_by(&span_order);
    Parse {
        root,
        errors,
        warnings,
        lang,
        shape_depends_on_language_version,
    }
}

/// Language-version legality diagnostics: report each construct not available
/// at `lang`. Never alters the tree (the spans come from a side-channel the
/// driver records), mirroring FCS's `CheckLanguageFeatureAndRecover`, which
/// reports the feature error and then parses the construct anyway.
///
/// Today the only gated construct is `#elif` (FCS's
/// `LanguageFeature.PreprocessorElif`, F# 11.0): `elif_directives` holds the
/// span of every `#elif` FCS feature-checks — separator required, dead arms and
/// nested branches included — so this is a faithful map of FCS's three check
/// sites, not an approximation. As more features become version-gated this
/// grows into the interval table (Stage 3 of `docs/ast-versioning-plan.md`).
fn langversion_diagnostics(
    elif_directives: &[Range<usize>],
    lang: LanguageVersion,
) -> Vec<ParseError> {
    if lang.supports_preprocessor_elif() {
        return Vec::new();
    }
    elif_directives
        .iter()
        .map(|span| ParseError {
            // FCS FS3350 `chkFeatureNotLanguageSupported`, feature name
            // `featurePreprocessorElif`.
            message: format!(
                "Feature '#elif preprocessor directive' is not available in F# {lang}. \
                 Please use language version 11.0 or greater."
            ),
            span: span.clone(),
        })
        .collect()
}

/// Language-version legality diagnostics for *typed-node* features: each node in
/// `root` whose kind is out of surface at `lang` draws an FS3350 feature error.
///
/// Parallel to [`langversion_diagnostics`], which gates *trivia* features
/// (`#elif`) via a span side-channel; a typed-node feature instead *is* a node in
/// the tree, so the gate walks the green tree against the shared
/// [`kind_interval`](crate::syntax::kind_interval) table. This makes "the tree
/// holds a node out of surface at `lang`" the same fact as "the `vN` projection
/// is not total here" (`docs/ast-versioning-plan.md` P2). Like FCS's
/// `CheckLanguageFeatureAndRecover` the report leaves the tree unchanged — and
/// here the tree is *always* the maximal/preview parse (the version is a lens +
/// diagnostic layer, never a reshape — see
/// `docs/completed/ast-versioning-nullness-proof.md` D-proof-1).
///
/// Today the only gated kind is `WITH_NULL_TYPE` (nullness, F# 9.0). The message
/// mirrors FCS's FS3350 template and feature name; the "Please use language
/// version" tail is what the LSP routes past its overlap dedup.
fn node_surface_diagnostics(root: &SyntaxNode, lang: LanguageVersion) -> Vec<ParseError> {
    root.descendants()
        .filter_map(|n| {
            // Out of surface *because not yet introduced* at `lang`: the
            // "use version N or greater" case. (No modelled kind is `removed`
            // today; a removal would carry a different message.)
            let introduced = kind_interval(n.kind()).introduced?;
            if lang >= introduced {
                return None;
            }
            // A gated kind without a feature name would be a wiring bug; skip it
            // (under-report, never a wrong message) — kept consistent by a test.
            let feature = feature_name_for_kind(n.kind())?;
            let range = n.text_range();
            Some(ParseError {
                // FCS FS3350 `chkFeatureNotLanguageSupported`.
                message: format!(
                    "Feature '{feature}' is not available in F# {lang}. \
                     Please use language version {introduced} or greater."
                ),
                span: usize::from(range.start())..usize::from(range.end()),
            })
        })
        .collect()
}

/// The FCS feature display name for a version-gated [`SyntaxKind`], used in its
/// FS3350 message — mirrors FCS `FSComp.SR.feature*` (e.g.
/// `featureNullnessChecking` → "nullness checking"). `None` for kinds that are
/// not version-gated (no `introduced` row); every gated kind must have an entry,
/// asserted by a test so the two never drift.
fn feature_name_for_kind(kind: SyntaxKind) -> Option<&'static str> {
    match kind {
        SyntaxKind::WITH_NULL_TYPE => Some("nullness checking"),
        _ => None,
    }
}

/// FS1161 "TABs are not allowed in F# code". FCS's lexer splits whitespace
/// into `truewhite = [' ']` and `offwhite = ['\t']`; a maximal run of tabs
/// consumed by the main token rule (`offwhite+`, `lex.fsl:705`) is a
/// recoverable error — FCS reports it against the tab run's range, then treats
/// the run as ordinary whitespace, so the tree is unchanged. One diagnostic per
/// maximal `\t` run mirrors `offwhite+`'s maximal munch: `let x =\t\t1` is one
/// error spanning both tabs, and a tab flanked by spaces (`truewhite`) flags
/// only the tab.
///
/// We flag tabs in code-position [`Token::Whitespace`] only — the whole token
/// is `truewhite`/`offwhite`, so every tab in it is `offwhite+`. Tabs inside
/// comments, strings, and char literals are part of those tokens (never a
/// `Whitespace` token), and `#if`-eliminated regions arrive as
/// [`TriviaToken::InactiveCode`] (FCS lexes them under `skip`, no diagnostic) —
/// so both are skipped for free.
///
/// **Directive lines are deliberately not diagnosed.** Whether a tab on a
/// `#…` line is `offwhite+` (→ FS1161) or `anywhite` (swallowed) depends on the
/// exact directive rule FCS matched: `#if`/`#else`/`#elif`/`#endif` and
/// `#nowarn`/`#warnon` carry an `anywhite*` prefix (`lex.fsl:1010` &c., so even
/// a leading tab is swallowed); `#line`/bare-`# N` begin at `#` with no prefix
/// (`:757`, so a leading tab *is* flagged); `#light`/`#indent` match
/// `"#light" anywhite* newline` / `(… ) anywhite+ "on"|"off"` (`:999-1006`);
/// and there are invalid-directive recovery rules (`anywhite* "#if" ident_char+`
/// &c.). Reproducing that grammar faithfully would mean re-implementing FCS's
/// directive lexer for inputs — tabs around directives — that essentially never
/// occur in real code. This LSP doesn't model that grammar (`lexfilter/mod.rs`),
/// so we decline to diagnose tabs on any line whose first non-blank byte is `#`
/// ([`whitespace_on_directive_line`]) rather than approximate it case by case.
/// The trade is deliberate: false negatives on (pathological) directive-line
/// tabs, never a false positive. Ordinary code — the case that matters — is
/// unaffected, since a code line's first non-blank byte isn't `#`.
fn tab_diagnostics(source: &str, raw_tokens: &[RawTok<'_>]) -> Vec<ParseError> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    for (res, span) in raw_tokens {
        if !matches!(res, Ok(TriviaToken::Lexed(Token::Whitespace)))
            || whitespace_on_directive_line(bytes, span.start)
        {
            continue;
        }
        let mut i = span.start;
        while i < span.end {
            if bytes[i] != b'\t' {
                i += 1;
                continue;
            }
            let run_start = i;
            while i < span.end && bytes[i] == b'\t' {
                i += 1;
            }
            out.push(ParseError {
                message: "TABs are not allowed in F# code".to_string(),
                span: run_start..i,
            });
        }
    }
    out
}

/// FS0046 reserved-identifier warnings: a lexer-level diagnostic scanned off the
/// active-code identifier tokens. F#'s ML-compatibility reserved words (`break`,
/// `sealed`, `tailcall`, … — see [`crate::lexer::RESERVED_IDENTS`]) lex as plain
/// identifiers, but FCS's `KeywordOrIdentifierToken` emits a *warning* for each
/// occurrence before handing the parser an `IDENT`. This mirrors that: one
/// warning per reserved word used as an identifier, at its exact span.
///
/// Only real active-code tokens (`TriviaToken::Lexed`) are scanned — inactive
/// (`#if`-eliminated) regions never lex to `Token::Ident`, and backtick-quoted
/// `` ``break`` `` lexes as `Token::QuotedIdent`, so neither draws a warning,
/// matching FCS.
///
/// The warning's span mirrors FCS's `LexemeRange`, which covers the *whole*
/// matched lexeme — and FCS folds a glued `!`/`#` into the identifier lexeme
/// (both `break!` and `break#` lex as one `Identifier` token spanning the
/// suffix). So:
/// * A glued `#` **suppresses** the warning. FCS looks up the full `break#`
///   lexeme (`lex.fsl:367`); it isn't reserved, so only FS1141 fires, no FS0046.
/// * A glued `!` **keeps** the warning but **extends its span over the `!`**.
///   FCS's `ident '!'` rule (`lex.fsl:360`) looks up the *trimmed* `break` (so
///   FS0046 still fires) while retaining the full `break!` `LexemeRange`.
/// * A space breaks the composite: `break #` / `break !` warn over just `break`.
///
/// Our lexer splits both suffixes off as separate `Hash` / `Op("!")` tokens and
/// doesn't yet emit FS1141 for any identifier (the general `ident '!'`/`ident
/// '#'` gap — `foo!` and `foo#` diverge too), so the accept/reject signal for
/// `break!` is not yet faithful; only the FS0046 emission (presence and span) is
/// matched here.
fn reserved_ident_diagnostics(raw_tokens: &[RawTok<'_>]) -> Vec<ParseError> {
    let mut out = Vec::new();
    for (i, (res, span)) in raw_tokens.iter().enumerate() {
        if let Ok(TriviaToken::Lexed(Token::Ident(s))) = res
            && crate::lexer::is_reserved_ident(s)
        {
            match glued_ident_suffix(raw_tokens, i, span.end) {
                // `break#` — folded into a non-reserved lexeme, no FS0046.
                Some(GluedSuffix::Hash) => continue,
                // `break!` — FS0046 over the full `break!` lexeme.
                Some(GluedSuffix::Bang(bang_end)) => out.push(ParseError {
                    message: format!("The identifier '{s}' is reserved for future use by F#"),
                    span: span.start..bang_end,
                }),
                // Bare `break` — FS0046 over the word itself.
                None => out.push(ParseError {
                    message: format!("The identifier '{s}' is reserved for future use by F#"),
                    span: span.clone(),
                }),
            }
        }
    }
    out
}

/// A `!` or `#` FCS would fold into the identifier lexeme: the token at `i + 1`
/// begins exactly at `ident_end` (no intervening trivia) and is a `#` (`Hash`)
/// or an operator whose first char is `!`. See [`reserved_ident_diagnostics`]
/// for how each shapes the FS0046 warning; the `Bang` variant carries the end of
/// the single consumed `!` so the warning span reaches the full `break!` lexeme.
///
/// FCS's `ident '!'` rule (`lex.fsl:360`) consumes exactly *one* trailing `!`,
/// but Logos greedily lexes a run of operator chars into one token — `break!!`
/// and `break!=` yield `Op("!!")` / `Op("!=")`. So any glued operator starting
/// with `!` extends the span by exactly one byte (`!` is ASCII), matching FCS
/// (verified via fcs-dump: `break!!`, `break!=`, `break!+` all warn over cols
/// 8..14, i.e. `break!`).
enum GluedSuffix {
    Hash,
    Bang(usize),
}

fn glued_ident_suffix(
    raw_tokens: &[RawTok<'_>],
    i: usize,
    ident_end: usize,
) -> Option<GluedSuffix> {
    let (res, next_span) = raw_tokens.get(i + 1)?;
    if next_span.start != ident_end {
        return None;
    }
    match res {
        Ok(TriviaToken::Lexed(Token::Hash)) => Some(GluedSuffix::Hash),
        Ok(TriviaToken::Lexed(Token::Op(op))) if op.starts_with('!') => {
            Some(GluedSuffix::Bang(next_span.start + 1))
        }
        _ => None,
    }
}

/// Is the whitespace token at `ws_start` on a `#…` directive line — i.e. is the
/// first non-blank byte of its line a `#`? Such lines (`#if`, `#light`, `#line`,
/// `#nowarn`, the invalid forms, …) are not diagnosed for tabs; see
/// [`tab_diagnostics`] for why. Covers both a directive's leading indent and any
/// whitespace within it, since both share the line's `#` first-non-blank byte.
fn whitespace_on_directive_line(bytes: &[u8], ws_start: usize) -> bool {
    let line_start = bytes[..ws_start]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    let first_non_blank = bytes[line_start..]
        .iter()
        .position(|&b| !matches!(b, b' ' | b'\t'))
        .map(|off| line_start + off);
    first_non_blank.is_some_and(|i| bytes[i] == b'#')
}

/// Style of an enclosing interpolated string, tracked so the nested-interp
/// check ([`Parser::check_interp_nesting`]) can mirror FCS's FS3373/FS3374
/// rules (`dotnet/fsharp/src/Compiler/lex.fsl:600-699`). Verbatim interp
/// (`$@"…"`) isn't implemented in the lexer yet; when it lands it folds into
/// `SingleOrVerbatim`, since FCS treats single and verbatim identically for
/// this check.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InterpStyle {
    SingleOrVerbatim,
    Triple,
}

struct Parser<'src> {
    source: &'src str,
    /// Raw lexer stream — includes trivia. Advanced in lockstep with
    /// `filtered_tokens` so trivia and filter-swallowed tokens reach the
    /// green tree.
    raw_tokens: Vec<RawTok<'src>>,
    raw_pos: usize,
    /// Trivia-free token stream as seen by the productions. `pos` is
    /// monotonic — we only index, never pop.
    filtered_tokens: Vec<FilteredTok<'src>>,
    pos: usize,
    /// High-water mark of source bytes already emitted into the green tree.
    /// Used to clamp filtered-token text starts so FCS-faithful *overlapping*
    /// splits — `RQuoteBarRBrace` becomes `RQUOTE=[s,e-2)` + `BAR_RBRACE=
    /// [s+1,e)` (LexFilter.fs:2757 `UseShiftedLocation(..., 1, 0)`) — don't
    /// double-count the shared byte and break `text(tree) == source`.
    raw_consumed_end: usize,
    builder: GreenNodeBuilder<'static>,
    errors: Vec<ParseError>,
    /// Warning-severity diagnostics (FCS `warning(...)` cases that leave
    /// `hadErrors` false). Kept separate from `errors` — see [`Parse`].
    warnings: Vec<ParseError>,
    /// Stack of enclosing interpolated-string styles, one entry per fill we
    /// are currently parsing inside. Empty at top level. Read by
    /// [`Self::check_interp_nesting`] to emit FS3373/FS3374 when an interp
    /// opener appears inside another interp's fill.
    interp_nest: Vec<InterpStyle>,
    /// Span of the head `new` keyword of the object-expression brace currently
    /// being classified by [`Parser::parse_obj_or_computation_brace`], or `None`
    /// outside one. Set (and saved/restored for nesting) by that handler around
    /// its base-call `parse_expr`; read by [`Parser::parse_new_expr`] to tell
    /// whether *its* `new` is that brace's head — so only the head `new` can be
    /// recognised as the bare `{ new T }` object expression (FCS's `objExpr`
    /// alt `NEW atomType`), never a nested or trailing argless `new` (which FCS
    /// rejects).
    obj_brace_base_new: Option<Range<usize>>,
    /// Set by [`Parser::parse_new_expr`] when the head `new` of the
    /// object-expression brace being classified is the bare no-argument form
    /// (`{ new T }`): no constructor parens, no `with`/interface block, and the
    /// brace closes directly after the type. Read (and saved/restored for
    /// nesting) by [`Parser::parse_obj_or_computation_brace`] to route the brace
    /// to the `OBJ_EXPR` arm. Distinct from the parenthesised `{ new T() }`,
    /// which keeps its argument and stays a computation expression.
    obj_brace_base_no_arg: bool,
    /// Current recursion depth across the mutually-recursive expression / type /
    /// pattern productions, maintained by [`Parser::with_depth`] /
    /// [`Parser::with_depth_bool`] around the recursion chokepoints. Bounds the
    /// hand-written recursive descent so deeply-nested input emits a recovery
    /// error instead of overflowing the stack (a stack overflow *aborts* the
    /// process — it does not unwind, so the LSP's `catch_unwind` parser wrapper
    /// could not recover from it). See [`MAX_PARSE_DEPTH`].
    depth: u32,
    /// Latched once [`MAX_PARSE_DEPTH`] is breached. While set, every guarded
    /// entry is a no-op: the breach drained the remaining input to EOF (so the
    /// productions' token loops terminate), and the unwinding parse must not do
    /// further work or re-trigger.
    depth_limit_hit: bool,
    /// Span of the token where the depth limit was breached, for the single
    /// collapsed diagnostic [`parse_inner`] emits. `None` until a breach.
    depth_limit_span: Option<Range<usize>>,
}

/// Maximum nesting depth of the recursive-descent productions (expression /
/// type / pattern), counted at the recursion chokepoints. Past this the parser
/// stops descending and records one recovery error.
///
/// Calibration: the worst construct (nested `if`/`then`/`else`, the most stack
/// per level) overflows an 8 MiB stack at roughly 2000–4000 levels in a debug
/// build; the LSP runs the parser on its single 8 MiB main thread. 512 keeps
/// the deepest reachable parse to well under a quarter of that budget (more in
/// release, where frames are smaller), while sitting far above any real source
/// — anything past ~128 levels already exceeds `serde_json`'s default depth, so
/// such files are absent from the FCS differential corpus entirely and this
/// bound introduces no comparison regression.
const MAX_PARSE_DEPTH: u32 = 512;

#[cfg(test)]
mod tests;
