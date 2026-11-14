//! Hand-written recursive-descent parser over the [`FilteredToken`] stream.
//!
//! Per `docs/parser-plan.md`, the parser writes the syntax table through
//! [`crate::syntax`]'s builder (wrapped to track the tree's height) and
//! exposes a typed AST in [`crate::syntax`]. Phase 2 grows the Phase 1 scaffold (empty file + lone int literal) one literal
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
//! zero-or-more `Virtual` tokens — but the tree must satisfy
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

use crate::directives::{PreprocError, Preprocessed, TriviaToken};
use crate::language_version::{FcsEffect, LanguageFeature, LanguageVersion};
use crate::lexer::{LexError, Token, TokenKind};
use crate::lexfilter::{
    ContextPush, FilteredToken, OffsideDiagnostic, OffsideSeverity, Virtual, filter_collect,
};
use crate::syntax::{SyntaxKind, SyntaxNode};
use crate::tokens::{FilteredKind, FilteredTokens, RawKind, RawTokens, lex_full_trivia_at};
use dual_keyword::{DualKeyword, KeywordForm};
use expr::decl_expr_keyword;
use filtered_cursor::FilteredCursor;

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
    /// The reused [`Parser::parse_do_expr`] self-consumes the `do`'s offside block
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
mod dual_keyword;
mod escapes;
mod expr;
mod expr_app;
mod expr_atom;
mod expr_control;
mod expr_op;
mod feature_gates;
mod filtered_cursor;
mod measure;
mod numeric;
mod pat;
mod sign_fold;
mod strings;
mod tree_builder;
mod types;
mod warn_directives;

use crate::syntax::Checkpoint;
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
use tree_builder::TreeBuilder;

/// Outcome of a parse. Holds the tree plus diagnostics partitioned by
/// severity; the tree is always present (errors become [`SyntaxKind::ERROR`]
/// tokens) so consumers don't need to plumb a [`Result`] through.
///
/// `errors` and `warnings` mirror FCS's two parse-time severities (an `errors`
/// entry corresponds to FCS's `ParseHadErrors`; `warnings` are diagnostics FCS
/// emits via `warning(...)` that leave `hadErrors` false). They are kept as
/// separate vectors rather than a severity field on [`ParseDiagnostic`] so the
/// `errors.is_empty()` error-parity checks in the differential test harness
/// read FCS's `ParseHadErrors` directly.
///
/// Each diagnostic carries a [`DiagnosticClass`]. The
/// [`Legality`](DiagnosticClass::Legality) ones are the language-version gates
/// that never move the tree: each is a feature error FCS reports at
/// [`Parse::lang`] over the tree it builds at every version. The rest, the
/// [`Recovery`](DiagnosticClass::Recovery) diagnostics, are held to FCS's
/// parse at that version like any other diagnostic, and
/// [`Parse::recovery_errors`] reads them alone.
///
/// Not yet honoured: `#nowarn` / `#warnon` suppression. FCS drops a warning
/// whose number is disabled at its position; `warnings` here are unfiltered, so
/// a consumer would surface a warning the user silenced. The directive layer
/// already parses these directives (`directives`' `Directive::NoWarn` /
/// `WarnOn`); wiring scoped suppression through to `warnings` is a follow-up.
#[derive(Debug, Clone)]
pub struct Parse {
    pub root: SyntaxNode,
    pub errors: Vec<ParseDiagnostic>,
    pub warnings: Vec<ParseDiagnostic>,
    /// The language version this tree was parsed against. The convenience
    /// entry points ([`parse`], [`parse_with_symbols`], …) default to
    /// [`LanguageVersion::Preview`] — every implemented feature on, i.e. the
    /// parser's historical "no version gating" behaviour — so existing callers
    /// are unaffected. [`parse_with_options`] threads a chosen version through.
    /// It drives the language-version gates (`docs/ast-versioning-plan.md`,
    /// the feature table), most of them diagnostics only, and the lexer's
    /// reading of a `$$"""` opener and the lex-filter's strict-indentation
    /// push decision and `relaxWhitespace2` pops — which **can** change the
    /// tree shape across the F# 8 and F# 6 boundaries;
    /// [`Parse::shape_depends_on_language_version`] reports whether it
    /// actually did for this source.
    pub lang: LanguageVersion,
    /// Whether this parse's tree shape depends on [`Parse::lang`]: the source
    /// holds a `$$"""` string, which the lexer reads differently below F# 8,
    /// or the lex-filter reached a decision point whose outcome differs across
    /// language versions (an offside version-gated context push — aborted at
    /// F# 8+, kept with a warning below — or a `relaxWhitespace2` pop that
    /// matters, F# 6).
    /// `false` **proves** the tree is identical under every
    /// [`LanguageVersion`] (see
    /// `crate::lexfilter::FilterRun::shape_depends_on_language_version` for
    /// the argument and for `true`'s over-approximation caveat — verify
    /// against parses at [`LanguageVersion::SHAPE_REPRESENTATIVES`] before
    /// paying a real cost for `true`), so a consumer that cannot pin the
    /// project's real language version may still trust an unflagged parse;
    /// diagnostics can still differ by version (FS0058 severity, `#elif`
    /// legality) — only the *shape* guarantee is claimed.
    pub shape_depends_on_language_version: bool,
    /// What the preprocessor reported during the lex this parse made: every
    /// error in stream order, and the active-branch `#line` directives.
    /// The same as [`crate::directives::preprocess`] of the source under the
    /// parse's symbols, so a consumer that reports lexer errors beside the
    /// parse's (the LSP's diagnostics) need not lex again. A function of the
    /// source and the symbols alone, never of [`Parse::lang`].
    pub preprocessor: Preprocessed,
}

impl Parse {
    /// The [`DiagnosticClass::Recovery`] entries of [`Parse::errors`]: every
    /// error but the legality gates, which leave the tree as FCS builds it.
    pub fn recovery_errors(&self) -> impl Iterator<Item = &ParseDiagnostic> {
        self.errors.iter().filter(|e| e.is_recovery())
    }

    /// The [`DiagnosticClass::Recovery`] entries of [`Parse::warnings`].
    pub fn recovery_warnings(&self) -> impl Iterator<Item = &ParseDiagnostic> {
        self.warnings.iter().filter(|e| e.is_recovery())
    }

    /// The tree read through the per-version facade `V`
    /// (`docs/ast-versioning-plan.md` D4): `V`'s root over [`Parse::root`],
    /// e.g. `parse.view::<syntax::v8::ParsedInput>()`.
    ///
    /// It is an error, not a panic or a silent projection, to read a parse
    /// through a facade that does not serve [`Parse::lang`] (`lang ∉
    /// V::VERSIONS`): the facade's exhaustiveness is a claim about its own
    /// version's syntax, and a tree parsed at another version can hold a
    /// construct the facade has no variant for without any diagnostic saying
    /// so. Parse at a version the facade serves instead.
    ///
    /// Under a facade that serves `lang`, a node outside its surface reads as
    /// an absent child (its cast is `None`), and an entry of [`Parse::errors`]
    /// whose [`gate`](field@ParseDiagnostic::gate) is the feature that
    /// introduced the node's kind lies over it. So a parse with no gate
    /// diagnostic reads totally.
    pub fn view<V: crate::syntax::Facade>(&self) -> Result<V, crate::syntax::ViewError> {
        if !V::VERSIONS.contains(&self.lang) {
            return Err(crate::syntax::ViewError {
                parsed: self.lang,
                facade: V::VERSIONS,
            });
        }
        Ok(V::cast(self.root.clone())
            .expect("a parse's root is an IMPL_FILE or SIG_FILE, which every facade casts"))
    }
}

/// The message of FCS's FS0597, reported on an application argument that
/// ends in an adjacent application (`g f(x)`). It is a parse error that keeps
/// the tree.
pub const SUCCESSIVE_ARGS_MESSAGE: &str = "Successive arguments should be separated by spaces or tupled, and arguments involving function or method applications should be parenthesized";

/// FCS's `atomicExpr` flag (the `bool` each `atomicExpr` production pairs
/// with its tree in `pars.fsy`): whether the expression ends in an adjacent
/// application. `atomicExpr HIGH_PRECEDENCE_PAREN_APP atomicExpr` sets it;
/// `atomicExpr DOT …`, `atomicExpr QMARK …` and `PREFIX_OP atomicExpr` pass
/// the receiver's on; every other production clears it. Its only reader is
/// `argExpr`, which reports [`SUCCESSIVE_ARGS_MESSAGE`] on an argument that
/// has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
enum EndsInAdjacentApp {
    Yes,
    No,
}

/// A problem the productions, the lexical scans, the directive driver or the
/// lex-filter found. `span` is a byte range into the input source. None of
/// them is a language-version gate, so each reaches [`Parse`] as a
/// [`DiagnosticClass::Recovery`] [`ParseDiagnostic`] ([`ParseError::recovery`]);
/// only the gates build any other ([`ParseDiagnostic::gate`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseError {
    pub(crate) message: String,
    pub(crate) span: Range<usize>,
}

impl ParseError {
    /// This problem as a [`DiagnosticClass::Recovery`] diagnostic.
    fn recovery(self) -> ParseDiagnostic {
        ParseDiagnostic {
            message: self.message,
            span: self.span,
            class: DiagnosticClass::Recovery,
            gate: None,
        }
    }
}

/// A diagnostic a parse reports: an entry of [`Parse::errors`] or
/// [`Parse::warnings`]. `span` is a byte range into the input source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub message: String,
    pub span: Range<usize>,
    /// What the diagnostic says about the tree it sits on.
    pub class: DiagnosticClass,
    /// The feature, when this is a language-version gate: the parse's
    /// version is below `feature`'s, and the diagnostic sits over a construct
    /// that feature introduced (or, for a gate FCS reports elsewhere, the
    /// range its check is given). Set by the gate constructor whatever
    /// the class, so a [`DiagnosticClass::Recovery`] gate (one whose feature
    /// FCS handles by reshaping) names its feature too. `None` for every other
    /// diagnostic, including the offside diagnostics a version moves, which
    /// sit over no construct of a feature.
    ///
    /// `class` is [`DiagnosticClass::of_gate`] of it, so a `Legality { feature
    /// }` diagnostic always has `gate == Some(feature)`.
    pub gate: Option<LanguageFeature>,
}

impl ParseDiagnostic {
    /// The diagnostic a gate on `feature` reports at `span`, classed from the
    /// feature table by [`DiagnosticClass::of_gate`].
    pub(crate) fn gate(feature: LanguageFeature, message: String, span: Range<usize>) -> Self {
        ParseDiagnostic {
            message,
            span,
            class: DiagnosticClass::of_gate(feature),
            gate: Some(feature),
        }
    }

    /// Whether this is a [`DiagnosticClass::Recovery`] diagnostic.
    pub fn is_recovery(&self) -> bool {
        self.class == DiagnosticClass::Recovery
    }
}

/// What a parse diagnostic says about the tree it sits on
/// (`docs/ast-versioning-plan.md`, D9).
///
/// The split lets a consumer tell "this is the tree FCS builds, but the pinned
/// language version forbids a construct in it" from "this tree is a
/// recovery". [`Parse::errors`] less its `Legality` entries is what FCS's
/// parse reports at the same version, and no `Legality` diagnostic moves the
/// tree, so a reading that asks only whether the tree can be trusted reads the
/// [`Recovery`](Self::Recovery) diagnostics alone ([`Parse::recovery_errors`]),
/// and a version guess on the wrong side of a `Legality` gate cannot change
/// its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticClass {
    /// `feature` is newer than the parse's language version, and FCS builds
    /// the same tree at every version: below the feature's version it
    /// reports the feature error over that tree
    /// (`CheckLanguageFeatureAndRecover`), or it checks the feature only after
    /// parsing. The tree is the one a newer pin gives; only this diagnostic
    /// moves.
    Legality { feature: LanguageFeature },
    /// Everything else: a syntax error the parse recovered from, an offside
    /// diagnostic, and a gate on a feature FCS handles by reshaping its tree or
    /// by moving another diagnostic, so that its tree or its other diagnostics
    /// depend on the version.
    Recovery,
}

impl DiagnosticClass {
    /// The class of the diagnostic a gate on `feature` reports, read off the
    /// feature table's `fcs_effect` column: [`Legality`](Self::Legality)
    /// when every effect is [`FcsEffect::Diagnose`] (vacuously so for a feature
    /// only the type checker consults), and [`Recovery`](Self::Recovery) when
    /// any is [`FcsEffect::Reshape`] or [`FcsEffect::Severity`].
    pub fn of_gate(feature: LanguageFeature) -> Self {
        if feature
            .fcs_effects()
            .iter()
            .all(|&effect| effect == FcsEffect::Diagnose)
        {
            DiagnosticClass::Legality { feature }
        } else {
            DiagnosticClass::Recovery
        }
    }
}

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

/// Whether `source`'s tree and its [`DiagnosticClass::Recovery`] diagnostics
/// are the same at every [`LanguageVersion`]: whether a parse at a guessed
/// version reads, legality gates aside, as a parse at the real one would.
///
/// The [`Legality`](DiagnosticClass::Legality) diagnostics are left out
/// because they are the one version-dependent output that cannot change what a
/// tree means: FCS builds the same tree on either side of such a gate. A
/// consumer that trusts a tree on the strength of its diagnostics reads only
/// the `Recovery` ones (`borzoi_sema::SyntaxRecovery`), so a guess on the
/// wrong side of a legality gate changes nothing it reads, and a file using a
/// newer construct faithfully gated at an older version stays invariant.
///
/// Deliberately a *function of the source* rather than a field on [`Parse`]:
/// answering it costs extra parses, and a field would charge every parse in
/// the workspace for a verdict almost none of them read.
///
/// Decided by **re-parsing at both ends of the ladder and comparing the
/// output**, deliberately rather than by asking each version-gated producer
/// whether it fired. Two review rounds each found a producer such an
/// enumeration had missed — a gate whose diagnostic is *suppressed* at the
/// parsed version emits nothing for the run to notice, and a depth-limited
/// parse collapses its errors to one whose span still moves — and an
/// enumeration that has to be complete to be sound is the wrong shape for a
/// claim consumers read as proof. Comparing outputs cannot miss a producer,
/// because it never names one.
///
/// Two points suffice for the diagnostics because every version gate is a
/// monotone threshold: a feature becomes available at some version and stays
/// available. That is an argument rather than a mechanism, so it is checked by
/// a property over *all* versions
/// (`shape_sensitivity::unset_diagnostics_flag_proves_version_invariance`).
/// The tree needs no such argument: an unflagged
/// [`Parse::shape_depends_on_language_version`] proves it the same at every
/// version, and a flagged one is compared at the two ends and at every
/// [`LanguageVersion::SHAPE_REPRESENTATIVES`] band between them, whose
/// diagnostics are compared too.
///
/// Costs two extra parses (three for a flagged source), so a caller that does
/// not need the verdict should not ask: it is worth paying only where a
/// commitment rests on the diagnostics being the real build's, and the version
/// is a guess.
///
/// Every parse runs under [`catch_unwind`](std::panic::catch_unwind), and a
/// panic reads as version-*dependent*. The parser is not expected to panic —
/// `parser_panic_sweep` enumerates adversarial token soup at every version and
/// asserts it does not — but this function parses at versions the caller
/// never asked for, so a *future* invariant guard that fires only at one of
/// them would cost the caller a verdict it never requested. Containing it here
/// rather than at the call site is what makes the containment hold for every
/// caller: the LSP wraps its own parses (`borzoi::cst_panic_safe`), but
/// `borzoi_sema::SyntaxRecovery::of_guessed_version` calls straight in. A panic
/// is also exactly the reading this returns `false` for on its merits — it
/// proves nothing about the other versions, so the caller must retain no
/// diagnostics.
pub fn diagnostics_are_version_invariant(
    source: &str,
    symbols: &HashSet<String>,
    file_kind: FileKind,
) -> bool {
    let at = |lang| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            parse_inner(source, symbols, file_kind, lang)
        }))
        .ok()
    };
    let (Some(lo), Some(hi)) = (at(LanguageVersion::MIN), at(LanguageVersion::MAX)) else {
        return false;
    };
    if !same_recovery_diagnostics(&lo, &hi) {
        return false;
    }
    if !lo.shape_depends_on_language_version && !hi.shape_depends_on_language_version {
        return true;
    }
    let ends = [lo.lang, hi.lang].map(LanguageVersion::shape_representative);
    lo.root.structurally_eq(&hi.root)
        && LanguageVersion::SHAPE_REPRESENTATIVES
            .into_iter()
            .filter(|lang| !ends.contains(lang))
            .all(|lang| {
                at(lang).is_some_and(|mid| {
                    same_recovery_diagnostics(&mid, &hi) && mid.root.structurally_eq(&hi.root)
                })
            })
}

/// Whether two parses report the same [`DiagnosticClass::Recovery`] errors and
/// warnings.
fn same_recovery_diagnostics(a: &Parse, b: &Parse) -> bool {
    a.recovery_errors().eq(b.recovery_errors()) && a.recovery_warnings().eq(b.recovery_warnings())
}

/// Every context the lex filter pushes over the token stream a parse of
/// `source` under `symbols` and `lang` consumes (see
/// [`crate::lexfilter::context_pushes`]).
pub fn context_pushes(
    source: &str,
    symbols: &HashSet<String>,
    lang: LanguageVersion,
) -> Vec<ContextPush> {
    let lexed = lex_full_trivia_at(source, symbols, lang);
    crate::lexfilter::context_pushes(&lexed.tokens, lang)
}

fn parse_inner(
    source: &str,
    symbols: &HashSet<String>,
    file_kind: FileKind,
    lang: LanguageVersion,
) -> Parse {
    parse_inner_stopping_at(source, symbols, file_kind, lang, TREE_HEIGHT_STOP)
}

/// [`parse_inner`] with the tree height at which the parse first tries
/// stopping given explicitly. A low stop makes the depth limit latch early, at
/// whichever safe point comes first, which is how the parser's own tests reach
/// every production's handling of a latch.
fn parse_inner_stopping_at(
    source: &str,
    symbols: &HashSet<String>,
    file_kind: FileKind,
    lang: LanguageVersion,
    initial_stop: usize,
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
    let lexed = lex_full_trivia_at(source, symbols, lang);
    let directive_errors: Vec<ParseError> = lexed
        .preproc_errors
        .iter()
        .map(|(e, span)| ParseError {
            message: e.diagnostic_message(),
            span: e.reporting_span(span.clone()),
        })
        .collect();
    // Every `#elif` directive FCS feature-checks (all branches/depths,
    // separator required); `langversion_diagnostics` turns them into errors
    // when `lang` predates 11.0.
    let elif_directives: &[Range<usize>] = &lexed.elif_directives;
    let preprocessor = lexed.preprocessed;
    let raw_tokens = lexed.tokens;
    // The active-branch real tokens are the substream LexFilter and the
    // productions consume. Directive / inactive-code trivia are not active
    // code, so they are dropped here; the parser still emits them from
    // `raw_tokens` for losslessness.
    let filter_run = filter_collect(&raw_tokens, lang);
    let mut filtered_tokens = filter_run.tokens;
    let offside_diagnostics: Vec<OffsideDiagnostic> = filter_run.diagnostics;
    let extended_openers = extended_interpolation_openers(source, &raw_tokens);
    let shape_depends_on_language_version =
        filter_run.shape_depends_on_language_version || !extended_openers.is_empty();
    let extended_interpolation_errors = extended_interpolation_diagnostics(&extended_openers, lang);
    // Fold an adjacent `+`/`-` into the following numeric literal, mirroring
    // FCS's token-layer sign fold (`LexFilter.fs:2694`). Done before the
    // parser runs so a signed literal is a single token in both expression
    // and pattern position. See `sign_fold`.
    let folded = sign_fold::fold_adjacent_signs(&raw_tokens, &mut filtered_tokens);
    // A lex-filter gate reports at FCS's lexbuf lexeme, which for a token the
    // filter handed over is that token's range; FCS folds a sign into a
    // literal before handing it over, so a literal the fold merged reports
    // with its sign.
    let filter_gates: Vec<ParseDiagnostic> = filter_run
        .gates
        .into_iter()
        .map(|(feature, span)| {
            let span = folded
                .binary_search_by_key(&span.end, |merged| merged.end)
                .ok()
                .map(|i| folded[i].clone())
                .filter(|merged| merged.start < span.start)
                .unwrap_or(span);
            ParseDiagnostic::gate(feature, feature.not_available_message(lang), span)
        })
        .collect();

    // FS1161 tabs, FS0046 reserved-identifier warnings and `:`/`$` inside an
    // operator lexeme: lexer-level diagnostics, scanned off the raw table
    // before the parser consumes it. The tab and operator errors are merged
    // into `errors` below (and sorted into source order alongside the
    // directive and production errors); the reserved-identifier warnings
    // leave `ParseHadErrors` false, so they never affect the parse-error
    // parity the differential harness checks.
    let [tab_errors, reserved_warnings, operator_char_errors] =
        lexical_diagnostics(source, &raw_tokens);

    // Language-version legality gate. The version never threads into the
    // preprocessor — the driver records the gated `#elif` spans, and we turn
    // them into diagnostics here — so the tree shape is identical regardless of
    // `lang`. Mirrors FCS, which gates `#elif` with `CheckLanguageFeatureAndRecover`:
    // it reports the feature error and recovers, leaving the parse unchanged.
    let langversion_errors = langversion_diagnostics(elif_directives, lang);

    let mut parser = Parser::new(source, raw_tokens, filtered_tokens);
    parser.tree_height_stop = initial_stop;
    // The parse stops once the tree would reach `tree_height_stop`, but that
    // measure can only count the ancestors already open, and only at a safe
    // point: a production that wraps finished children under a node it opens
    // afterwards (`start_node_at`, as a nested module does around its body)
    // adds levels the parse never saw coming, and a loop that grows the tree
    // without reaching a safe point is not stopped at all. So the finished
    // height is checked here, and an over-tall parse is redone once, stopping
    // lower by the excess. If that is still too tall, the file is given up
    // whole: a root holding the source as one `ERROR` token, with the depth
    // diagnostic. At most two parses, then; on real input never more than one.
    for round in 0.. {
        match file_kind {
            FileKind::Impl => parser.parse_impl_file(),
            FileKind::Sig => parser.parse_sig_file(),
        }
        let height = parser.builder.height_if_closed_now();
        if height <= MAX_TREE_HEIGHT {
            break;
        }
        if round == 0 {
            let stop = parser
                .tree_height_stop
                .saturating_sub(height - MAX_TREE_HEIGHT);
            parser = parser.restart(stop);
        } else {
            parser = parser.restart(0);
            parser.give_up_whole(file_kind);
            break;
        }
    }
    // If the depth guard fired (the recursion bound or the tree-height bound),
    // the parse stopped at the breach and drained the rest of the input to EOF;
    // the unwinding productions emitted a cascade of "expected closer" errors
    // against that EOF. Collapse the whole diagnostic set to the single depth
    // error — the file is pathologically nested and already an error; one
    // characterised diagnostic is the useful signal. (The tree stays lossless
    // regardless.)
    if parser.depth_limit_hit {
        let span = parser.depth_limit_span.clone().unwrap_or(0..source.len());
        return Parse {
            root: parser.builder.finish(),
            errors: vec![
                ParseError {
                    message: "nesting is too deep; parsing stopped here".to_string(),
                    span,
                }
                .recovery(),
            ],
            warnings: Vec::new(),
            lang,
            shape_depends_on_language_version,
            preprocessor,
        };
    }
    // Build the root now: the feature gates below walk it for constructs a
    // language feature introduced after `lang`.
    let root = parser.builder.finish();
    let gates = feature_gates::feature_gate_diagnostics(&root, lang);
    // Merge the directive errors (collected in source order above), the FS1161
    // tab errors (scanned off the whitespace trivia), the `#elif` trivia gate,
    // the feature gates, the lex-filter's and the lexer's gates and the
    // `#nowarn` diagnostics with the productions' errors (emitted in parse
    // order), then re-sort by span start so the final list reads in source
    // order regardless of which producer found each problem. The gates class
    // their diagnostics; every other producer finds recovery diagnostics.
    let mut errors: Vec<ParseDiagnostic> = parser
        .errors
        .into_iter()
        .chain(directive_errors)
        .map(ParseError::recovery)
        .collect();
    errors.extend(tab_errors);
    errors.extend(operator_char_errors);
    errors.extend(langversion_errors);
    errors.extend(gates);
    errors.extend(filter_gates);
    errors.extend(extended_interpolation_errors);
    let (warn_errors, warn_warnings) =
        warn_directives::warn_directive_diagnostics(&root, source, lang);
    errors.extend(warn_errors);
    // Warnings come from the productions and the lex-filter's offside
    // diagnostics; directive/tab/langversion problems are all errors.
    let mut warnings: Vec<ParseDiagnostic> = parser
        .warnings
        .into_iter()
        .map(ParseError::recovery)
        .collect();
    warnings.extend(reserved_warnings);
    warnings.extend(warn_warnings);
    // The lex-filter's offside / indentation diagnostics (FS0058), split by the
    // severity it resolved from `lang`.
    for diag in offside_diagnostics {
        let err = ParseError {
            message: diag.message,
            span: diag.span,
        }
        .recovery();
        match diag.severity {
            OffsideSeverity::Error => errors.push(err),
            OffsideSeverity::Warning => warnings.push(err),
        }
    }
    let span_order = |a: &ParseDiagnostic, b: &ParseDiagnostic| {
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
        preprocessor,
    }
}

/// Language-version legality diagnostics: report each construct not available
/// at `lang`. Never alters the tree (the spans come from a side-channel the
/// driver records), mirroring FCS's `CheckLanguageFeatureAndRecover`, which
/// reports the feature error and then parses the construct anyway.
///
/// The gated construct is `#elif` ([`LanguageFeature::PreprocessorElif`], F#
/// 11.0): `elif_directives` holds the span of every `#elif` FCS feature-checks —
/// separator required, dead arms and nested branches included — so this is a
/// faithful map of FCS's three check sites, not an approximation. The
/// tree-walk gates are `feature_gates`.
fn langversion_diagnostics(
    elif_directives: &[Range<usize>],
    lang: LanguageVersion,
) -> Vec<ParseDiagnostic> {
    let feature = LanguageFeature::PreprocessorElif;
    if lang.supports(feature) {
        return Vec::new();
    }
    elif_directives
        .iter()
        .map(|span| {
            ParseDiagnostic::gate(feature, feature.not_available_message(lang), span.clone())
        })
        .collect()
}

/// The span of every `$$"""` opener among `raw_tokens` (two `$` or more), in
/// source order, whichever way the lexer read it
/// ([`LanguageFeature::ExtendedStringInterpolation`]).
fn extended_interpolation_openers(source: &str, raw_tokens: &RawTokens<'_>) -> Vec<Range<usize>> {
    (0..raw_tokens.len())
        .filter(|&i| {
            matches!(
                raw_tokens.kind(i),
                RawKind::Lexed(
                    TokenKind::InterpExtendedBegin
                        | TokenKind::InterpExtendedBeginEnd
                        | TokenKind::InterpTripleBegin
                        | TokenKind::InterpTripleBeginEnd
                ) | RawKind::UnterminatedString
            )
        })
        .map(|i| raw_tokens.span(i))
        .filter(|span| {
            let text = &source.as_bytes()[span.clone()];
            let dollars = text.iter().take_while(|&&b| b == b'$').count();
            dollars >= 2 && text[dollars..].starts_with(b"\"\"\"")
        })
        .collect()
}

/// FCS's FS1251 (`lexExtendedStringInterpolationNotSupported`) on each
/// `$$"""` opener below [`LanguageFeature::ExtendedStringInterpolation`],
/// where its lexer reads the opener as a `$"""` one (`lex.fsl`): at the
/// lexeme, which runs to the string's first fill or its end, the token the
/// lexer then produced.
fn extended_interpolation_diagnostics(
    openers: &[Range<usize>],
    lang: LanguageVersion,
) -> Vec<ParseDiagnostic> {
    let feature = LanguageFeature::ExtendedStringInterpolation;
    if lang.supports(feature) {
        return Vec::new();
    }
    openers
        .iter()
        .map(|span| {
            ParseDiagnostic::gate(
                feature,
                "Extended string interpolation is not supported in this version of F#.".to_string(),
                span.clone(),
            )
        })
        .collect()
}

/// The lexer-level diagnostics a parse reports beside its tree, scanned off
/// the raw table: the FS1161 tab errors (`tab_diagnostics`), the FS0046
/// reserved-identifier warnings (`reserved_ident_diagnostics`) and the
/// operator-character errors (`operator_char_diagnostics`), in that order. All
/// are [`DiagnosticClass::Recovery`] diagnostics.
pub fn lexical_diagnostics(source: &str, raw_tokens: &RawTokens<'_>) -> [Vec<ParseDiagnostic>; 3] {
    [
        tab_diagnostics(source, raw_tokens),
        reserved_ident_diagnostics(raw_tokens),
        operator_char_diagnostics(raw_tokens),
    ]
    .map(|found| found.into_iter().map(ParseError::recovery).collect())
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
fn tab_diagnostics(source: &str, raw_tokens: &RawTokens<'_>) -> Vec<ParseError> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    // Most sources hold no tab at all, and one pass over the bytes says so for
    // a fraction of what testing every whitespace token costs.
    if !bytes.contains(&b'\t') {
        return out;
    }
    for i in 0..raw_tokens.len() {
        let span = raw_tokens.span(i);
        // The directive-line test scans back to the start of the line, so it
        // runs only for whitespace that holds a tab; run for every whitespace
        // token, it would make a long line quadratic.
        if raw_tokens.kind(i) != RawKind::Lexed(TokenKind::Whitespace)
            || !bytes[span.clone()].contains(&b'\t')
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
/// FCS's `checkExprOp` (`lex.fsl:110`): an operator lexeme containing `:` or
/// `$` is an error ("'…' is not permitted as a character in operator names"),
/// reported once per offending character over the whole lexeme. The
/// fixed-spelling tokens that legitimately carry those characters (`::`, `:=`,
/// `:>`, `:?>`, a bare `$`) never lex as [`Token::Op`], so every `Op` is subject
/// to the check — except that the `>`-headed rule (`checkExprGreaterColonOp`,
/// `lex.fsl:983`) checks only `$`. That rule applies when the first character
/// past the leading `ignored_op_char`s is `>`; a `$` among those leading
/// characters makes the lexeme an error regardless of which rule claims it.
fn operator_char_diagnostics(raw_tokens: &RawTokens<'_>) -> Vec<ParseError> {
    let mut out = Vec::new();
    for i in 0..raw_tokens.len() {
        if raw_tokens.kind(i) != RawKind::Lexed(TokenKind::Op) {
            continue;
        }
        let (op, span) = (raw_tokens.text(i), raw_tokens.span(i));
        let greater_headed = op.trim_start_matches(['.', '?']).starts_with('>');
        for c in [':', '$'] {
            if op.contains(c) && !(c == ':' && greater_headed) {
                out.push(ParseError {
                    message: format!(
                        "'{c}' is not permitted as a character in operator names and is \
                         reserved for future use"
                    ),
                    span: span.clone(),
                });
            }
        }
    }
    out
}

fn reserved_ident_diagnostics(raw_tokens: &RawTokens<'_>) -> Vec<ParseError> {
    let mut out = Vec::new();
    for i in 0..raw_tokens.len() {
        // The kind test comes first: it is all most tokens need.
        if raw_tokens.kind(i) != RawKind::Lexed(TokenKind::Ident) {
            continue;
        }
        let s = raw_tokens.text(i);
        if !crate::lexer::is_reserved_ident(s) {
            continue;
        }
        let span = raw_tokens.span(i);
        let message = format!("The identifier '{s}' is reserved for future use by F#");
        match glued_ident_suffix(raw_tokens, i, span.end) {
            // `break#` — folded into a non-reserved lexeme, no FS0046.
            Some(GluedSuffix::Hash) => {}
            // `break!` — FS0046 over the full `break!` lexeme.
            Some(GluedSuffix::Bang(bang_end)) => out.push(ParseError {
                message,
                span: span.start..bang_end,
            }),
            // Bare `break` — FS0046 over the word itself.
            None => out.push(ParseError { message, span }),
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
    raw_tokens: &RawTokens<'_>,
    i: usize,
    ident_end: usize,
) -> Option<GluedSuffix> {
    let next = i + 1;
    if next >= raw_tokens.len() || raw_tokens.span(next).start != ident_end {
        return None;
    }
    match raw_tokens.kind(next) {
        RawKind::Lexed(TokenKind::Hash) => Some(GluedSuffix::Hash),
        RawKind::Lexed(TokenKind::Op) if raw_tokens.text(next).starts_with('!') => {
            Some(GluedSuffix::Bang(ident_end + 1))
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

/// The kinds of bracket [`Parser::raw_bracket_left_open`] balances.
const BRACKET_KINDS: usize = 8;

/// How a binding's RHS block ended, read off the token stream: which
/// `OBLOCKEND` the binding owns, and whether FCS's parser met an error
/// inside the block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RhsClose {
    /// No offside block opened after the `=` (or no `=`): the binding owns
    /// no `OBLOCKEND`.
    NoBlock,
    /// The gatherer stopped at the block's own `BlockEnd`, every block opened
    /// inside it closed, nothing left over: the `BlockEnd` at the cursor is
    /// the binding's.
    Clean,
    /// Tokens the gatherer could not take were left in the block: an error
    /// FCS's parser met inside the RHS. The drain counts no blocks, so the
    /// `BlockEnd` it stopped at may be an inner one.
    Leftovers,
    /// The block opened but the cursor is not at its close (an inner block
    /// is still open, or the file ended).
    Unowned,
}

struct Parser<'src> {
    source: &'src str,
    /// Raw lexer stream — includes trivia. Advanced in lockstep with
    /// `filtered` so trivia and filter-swallowed tokens reach the
    /// tree.
    raw_tokens: RawTokens<'src>,
    raw_pos: usize,
    /// Trivia-free token stream as seen by the productions, with the cursor
    /// that walks it. The cursor is monotonic — we only index, never pop.
    filtered: FilteredCursor<'src>,
    /// `block_depth_prefix[i]`: `OBLOCKBEGIN`s minus `OBLOCKEND`s among the
    /// filtered tokens `0..i` ([`Self::block_balance`]).
    block_depth_prefix: Vec<i32>,
    /// `raw_bracket_prefix[i][k]`: openers minus closers of bracket kind `k`
    /// among the raw tokens `0..i` ([`Self::raw_bracket_left_open`], which
    /// builds it on first use).
    raw_bracket_prefix: Option<Vec<[i32; BRACKET_KINDS]>>,
    /// High-water mark of source bytes already emitted into the tree.
    /// Used to clamp filtered-token text starts so FCS-faithful *overlapping*
    /// splits — `RQuoteBarRBrace` becomes `RQUOTE=[s,e-2)` + `BAR_RBRACE=
    /// [s+1,e)` (LexFilter.fs:2757 `UseShiftedLocation(..., 1, 0)`) — don't
    /// double-count the shared byte and break `text(tree) == source`.
    raw_consumed_end: usize,
    builder: TreeBuilder,
    errors: Vec<ParseError>,
    /// How the RHS block of the binding [`Self::parse_binding_with_modifiers`]
    /// finished last ended; [`RhsClose::NoBlock`] for a failed head. A binding
    /// nested in another's RHS finishes first, so after a `let` group this is
    /// its last binding's.
    last_binding_close: RhsClose,
    /// How the RHS [`Self::parse_let_equals_rhs`] parsed last ended.
    rhs_close: RhsClose,
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
    /// Latched once [`MAX_PARSE_DEPTH`] is breached or the tree reaches
    /// [`TREE_HEIGHT_STOP`]. While set, every guarded
    /// entry is a no-op: the breach drained the remaining input to EOF (so the
    /// productions' token loops terminate), and the unwinding parse must not do
    /// further work or re-trigger.
    depth_limit_hit: bool,
    /// Span of the token where the depth limit was breached, for the single
    /// collapsed diagnostic [`parse_inner`] emits. `None` until a breach.
    depth_limit_span: Option<Range<usize>>,
    /// The tree height at which the parse stops: [`TREE_HEIGHT_STOP`], or lower
    /// on a parse [`parse_inner`] redoes because its tree came out taller than
    /// [`MAX_TREE_HEIGHT`].
    tree_height_stop: usize,
}

/// Maximum nesting depth of the recursive-descent productions (expression /
/// type / pattern), counted at the recursion chokepoints. Past this the parser
/// stops descending and records one recovery error.
///
/// Calibration: the worst construct (nested `if`/`then`/`else`, the most stack
/// per level) overflows an 8 MiB stack at roughly 2000–4000 levels in a debug
/// build; the LSP runs the parser on a 32 MiB thread
/// (`borzoi::server::SERVER_STACK_BYTES`), and the tests on 8 MiB. 512 keeps
/// the deepest reachable parse to well under a quarter of 8 MiB (more in
/// release, where frames are smaller), while sitting far above any real source
/// — anything past ~128 levels already exceeds `serde_json`'s default depth, so
/// such files are absent from the FCS differential corpus entirely and this
/// bound introduces no comparison regression.
const MAX_PARSE_DEPTH: u32 = 512;

/// The tallest tree any parse returns: no root-to-leaf path holds more than
/// this many nodes.
///
/// The recursion bound (`MAX_PARSE_DEPTH`) does not bound the
/// tree: application chains (`f a a a`), left-associative operator chains,
/// postfix type applications (`int list list`) and the like are built in a
/// loop, one wrapping node per repeat, so they are as deep as they are long.
/// Any consumer walking the tree recursively needs stack in proportion to its
/// height, and a stack overflow aborts the process, which `catch_unwind`
/// cannot catch. The parser therefore also stops, with the depth-limit
/// diagnostic, once the tree it is building nears this height. (The tree
/// itself is a flat table, so freeing it never recurses.)
///
/// Calibration, in a debug build: `borzoi-sema`'s walks need about 8 MiB for
/// a tree of this height (an operator chain under four hundred nested modules). The
/// LSP answers requests on a thread of `borzoi_sema::WALK_STACK_BYTES`
/// (32 MiB), and sema's `walk_stack_bound` test holds the walks to half of
/// that over every repetition form of the grammar at this bound. The
/// tallest tree in the pinned F# corpus that parses cleanly is 259 nodes deep;
/// the tallest of all is 666, in a file the recursion guard already stops.
pub const MAX_TREE_HEIGHT: usize = 1024;

/// The tree height at which the parser first tries stopping (see
/// [`MAX_TREE_HEIGHT`]). It is measured as
/// [`TreeBuilder::height_if_closed_now`], which counts the open nodes the
/// unwinding productions go on to close; the margin covers the few nodes they
/// add on the way out. Nodes a production opens around children it has already
/// finished are not countable in advance, so [`parse_inner`] checks the
/// finished height and redoes a parse that came out too tall with a lower
/// stop. `tests/all/parser_tree_height.rs` checks the guarantee over the
/// grammar's repetition forms, alone and nested in modules.
const TREE_HEIGHT_STOP: usize = MAX_TREE_HEIGHT - 64;

#[cfg(test)]
mod tests;
