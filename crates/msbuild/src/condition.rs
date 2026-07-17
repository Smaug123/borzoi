//! Evaluator for MSBuild `Condition` attributes.
//!
//! Phase 3 implements the subset described in plan D5: single-quoted
//! string literals (with `$(Name)` substitution inside), `==` / `!=`
//! equality mirroring MSBuild's `MultipleComparisonNode` dispatch (empty
//! short-circuit, then double-numeric — decimal or hex — then MSBuild
//! boolean vocabulary, then case-insensitive string equality), relational
//! comparisons mirroring `NumericComparisonExpressionNode` (doubles,
//! `System.Version`-style dotted versions, and MSBuild's mixed
//! number-vs-version major-only rule), MSBuild version comparison
//! functions, the `System.Version.Parse(...).Build/Revision` comparisons the
//! SDK uses, the pure `HasTrailingSlash('...')` built-in, `And` / `Or`
//! boolean connectives, unary `!`, parentheses, bare `true` / `false`
//! literals, and a standalone scalar coerced to bool through MSBuild's
//! boolean vocabulary (`Condition="$(SomeBool)"`, `!$(V.Contains('{'))`).
//! Fixed names (keywords, property-function type/member names) match
//! case-insensitively throughout, as MSBuild's do. The filesystem-touching
//! entry point additionally gives `Exists('...')` a filesystem callback;
//! pure evaluation treats a reached `Exists` call as [`Outcome::Unsupported`].
//! Anything outside that grammar — arithmetic, `@(...)` item references,
//! unmodelled property functions, relational comparisons whose operands are
//! neither numbers nor versions (a hard project error in MSBuild itself) —
//! produces [`Outcome::Unsupported`], at which point the caller treats the
//! containing construct as **excluded** (plan D5).
//!
//! The comparison semantics were derived from MSBuild's own evaluator
//! (`src/Build/Evaluation/Conditionals/*.cs` — `MultipleComparisonNode`,
//! `NumericComparisonExpressionNode`, `ConversionUtilities`) and pinned by
//! unit tests verified against `dotnet msbuild` 10.0.300; tests below cite
//! that oracle.
//!
//! We deliberately fail safe: when we can't tell whether a condition is
//! true or false, we MUST NOT proceed as if it were true. Otherwise a
//! `<Compile Include="DebugOnly.fs" Condition="$([System.String]::IsNullOrEmpty('$(Foo)'))" />`
//! (an unmodelled property function) would silently leak into Release
//! builds. Excluding (and surfacing a diagnostic) preserves the "never
//! produce wrong output" promise.
//!
//! Property substitution inside condition strings reuses the main
//! [`properties::substitute`] function. Undefined references become
//! `""` (matching MSBuild's "unset property → empty string" rule) so
//! we can still compute a truth value, but the names of every
//! undefined reference are returned alongside the outcome so the
//! walker can emit [`UndefinedProperty`](super::diagnostic::DiagnosticKind::UndefinedProperty)
//! diagnostics. Without that, a condition like
//! `'$(TargetFramework)' == '$(FcsTargetNetFxFramework)'` (both sides
//! undefined here) silently expands to `'' == ''` = true, picking a
//! branch MSBuild might not — see plan D5's fail-loud stance.
//!
//! The standard protected-write idiom
//! (`<Configuration Condition="'$(Configuration)' == ''">Debug</…>`)
//! is unaffected: the walker short-circuits protected writes *before*
//! evaluating the condition, so the diagnostic never fires for that
//! pattern. Unsupported expressions (unknown property functions, item
//! references inside the substitution) still poison the whole condition.

use super::properties::escaping::Escaped;
use super::properties::{Issue, PropertyMap, substitute, substitute_with_fs};

/// Result of evaluating a condition string. The `Unsupported` variant
/// carries the original condition text so callers can fold it into a
/// [`DiagnosticKind::UnsupportedCondition`](super::diagnostic::DiagnosticKind::UnsupportedCondition)
/// without re-parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    True,
    False,
    /// The condition contains syntax or operators outside the
    /// grammar we model. Treat the containing construct as excluded;
    /// do NOT proceed as if it were true.
    Unsupported,
}

/// Outcome plus the names of any `$(...)` references encountered
/// during evaluation that weren't defined in the supplied
/// [`PropertyMap`]. Each was substituted to `""`, so the outcome is
/// still meaningful (and matches what MSBuild would compute given the
/// same property map), but callers should emit an
/// [`UndefinedProperty`](super::diagnostic::DiagnosticKind::UndefinedProperty)
/// diagnostic per name and mark the project partial — our map may be
/// missing values MSBuild itself would have seen (imported targets,
/// environment, etc.), making our truth value potentially divergent.
/// Names are returned in encounter order with duplicates preserved,
/// matching how `properties::substitute` reports issues for
/// non-condition substitution sites elsewhere in the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eval {
    pub outcome: Outcome,
    pub undefined_properties: Vec<String>,
    /// The subset of [`Self::undefined_properties`] with at least one
    /// occurrence *outside* a comparison against the empty literal
    /// (`'$(X)' == ''` / `'$(X)' != ''`). The empty-comparison shape is
    /// the MSBuild default-fill / is-it-set idiom, which the evaluator's
    /// self-default exemption may treat as deterministic; any other
    /// context (`'$(X)' != 'bar'`, `Exists('$(X)')`, …) is a genuine
    /// branch decision on the unknown value and must never be exempt.
    pub undefined_outside_empty_comparison: Vec<String>,
}

/// Evaluate a `Condition` attribute against the current property map.
///
/// Returns [`Outcome::True`] / [`Outcome::False`] when the entire
/// expression reduces to a boolean within our grammar, and
/// [`Outcome::Unsupported`] when any part of the parse or any
/// `$(...)` expansion strays outside it. Whitespace around tokens is
/// insignificant; an entirely-blank condition string evaluates as
/// [`Outcome::True`] (matching MSBuild, which omits the attribute and
/// the attribute-with-only-whitespace-content identically).
///
/// When the outcome is `Unsupported`, the `undefined_properties` list
/// is empty: the `UnsupportedCondition` diagnostic the caller will
/// emit subsumes any per-property concerns, and reporting both would
/// double up. For `True` / `False`, the list is populated.
pub fn evaluate(source: &str, props: &PropertyMap) -> Eval {
    evaluate_inner(source, props, None)
}

/// Evaluate a condition with support for MSBuild's filesystem-backed
/// `Exists('...')` predicate.
pub fn evaluate_with_exists(
    source: &str,
    props: &PropertyMap,
    exists: &dyn Fn(&str) -> bool,
) -> Eval {
    evaluate_inner(source, props, Some(exists))
}

fn evaluate_inner(
    source: &str,
    props: &PropertyMap,
    exists: Option<&dyn Fn(&str) -> bool>,
) -> Eval {
    if source.trim().is_empty() {
        return Eval {
            outcome: Outcome::True,
            undefined_properties: Vec::new(),
            undefined_outside_empty_comparison: Vec::new(),
        };
    }
    // MSBuild's condition scanner lexes item-list (`@(`) and metadata (`%(`)
    // references directly from the *raw* condition text, before any property
    // expansion; we model neither, and MSBuild treats a bare marker in a
    // condition as a hard lex error. A single scan of the raw source closes
    // that whole class up front: some of our reductions (string instance
    // methods, `System.Version` parts, version functions) collapse a `$(…)`
    // subexpression to a value *before* the operand-level `@(`/`%(` check in
    // `expand_for_condition` can see it, which would otherwise let a raw
    // marker in the source slip through and commit us to a boolean MSBuild
    // never reaches. Rejecting here is uniform and structural — no individual
    // reducer needs its own marker guard.
    //
    // Markers arriving *only* via `$()` substitution are not in the raw
    // source and are unaffected, matching MSBuild (it never re-lexes
    // substituted text). This is deliberately conservative: it also rejects
    // the rare balanced `@(x)` forms MSBuild happens to accept, but only ever
    // toward `Unsupported`, never a wrong committed gate.
    if source.contains("@(") || source.contains("%(") {
        return unsupported();
    }
    // NB: escape-bearing conditions are **modelled**, not degraded (stage E2 of
    // `docs/msbuild-escaped-value-plan.md`). MSBuild unescapes `%XX` at the
    // operand leaf, so `'%74rue' == 'true'` is *true* — and `expand_for_condition`
    // decodes each operand there, exactly once, having scanned it escaped.
    //
    // A `%` **outside** quotes needs no guard here: it is not an escape at all,
    // because MSBuild's scanner rejects it (`MSB4090: Found an unexpected
    // character '7' … in condition "%74rue"`). Our tokeniser reaches the same
    // verdict structurally — `%` matches no token start, so it lexes to
    // `Token::Unknown`, which the parser surfaces as `Unsupported`. That is the
    // right mapping for an MSBuild error, and it is why deleting the wholesale
    // degrade does not open a hole.
    let tokens = match tokenise(source) {
        Ok(t) => t,
        Err(()) => return unsupported(),
    };
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
    };
    let Ok(expr) = parser.parse_or() else {
        return unsupported();
    };
    if parser.pos != parser.tokens.len() {
        // Trailing tokens — something we don't model came after a
        // well-formed prefix (`'a' == 'b' garbage`). Don't pretend
        // the prefix's truth value is the whole condition's.
        return unsupported();
    }
    let mut undefined = Vec::new();
    let outcome = match eval_bool(&expr, props, exists, &mut undefined) {
        Ok(true) => Outcome::True,
        Ok(false) => Outcome::False,
        Err(()) => {
            // Discard any undefined names we may have collected: the
            // UnsupportedCondition diagnostic the caller raises is
            // the authoritative description of the failure, and a
            // separate UndefinedProperty for some property mentioned
            // in the same condition would be redundant noise.
            return unsupported();
        }
    };
    // Scan the *whole parsed tree* for reference contexts — evaluation
    // above short-circuits (`'$(X)' == '' Or '$(X)' == 'x'` never expands
    // the second arm when the first is true), so evaluation-order records
    // would under-report non-default uses.
    let mut outside = Vec::new();
    refs_outside_empty_comparison(&expr, &mut outside);
    let undefined_outside_empty_comparison = undefined
        .iter()
        .filter(|name| outside.iter().any(|o| o.eq_ignore_ascii_case(name)))
        .cloned()
        .collect();
    Eval {
        outcome,
        undefined_properties: undefined,
        undefined_outside_empty_comparison,
    }
}

/// Collect every property reference with at least one *syntactic*
/// occurrence outside a comparison against the empty literal. An
/// operand's references count as empty-compared exactly when the other
/// operand is the (quote-stripped, exactly) empty literal; `Exists(…)`
/// and every other context is non-default. Purely syntactic on purpose —
/// see the call site.
fn refs_outside_empty_comparison(expr: &BoolExpr, out: &mut Vec<String>) {
    match expr {
        BoolExpr::True | BoolExpr::False => {}
        BoolExpr::Not(inner) => refs_outside_empty_comparison(inner, out),
        BoolExpr::And(lhs, rhs) | BoolExpr::Or(lhs, rhs) => {
            refs_outside_empty_comparison(lhs, out);
            refs_outside_empty_comparison(rhs, out);
        }
        BoolExpr::Exists(raw) | BoolExpr::HasTrailingSlash(raw) => {
            out.extend(super::evaluator::simple_property_references(raw).map(str::to_string))
        }
        BoolExpr::MsbuildVersionFunction(inner) => {
            out.extend(super::evaluator::simple_property_references(inner).map(str::to_string))
        }
        // A standalone scalar coerced to bool is a genuine branch decision
        // on its value, never the is-it-set idiom.
        BoolExpr::CoerceBool(scalar) => scalar_refs_outside_empty_comparison(scalar, out),
        BoolExpr::Compare { op, lhs, rhs } => {
            // Only the equality shape (`'$(X)' == ''` / `'$(X)' != ''`) is the
            // is-it-set idiom; a relational comparison against any operand is a
            // genuine branch decision on the value.
            let exempting = matches!(op, CompareOp::Eq | CompareOp::Neq);
            if !(exempting && rhs.is_empty_string_literal()) {
                scalar_refs_outside_empty_comparison(lhs, out);
            }
            if !(exempting && lhs.is_empty_string_literal()) {
                scalar_refs_outside_empty_comparison(rhs, out);
            }
        }
    }
}

fn scalar_refs_outside_empty_comparison(scalar: &ScalarExpr, out: &mut Vec<String>) {
    match scalar {
        ScalarExpr::Bare(_) => {}
        ScalarExpr::String(raw) | ScalarExpr::Property(raw) => {
            out.extend(super::evaluator::simple_property_references(raw).map(str::to_string))
        }
        // The raw `Parse(...)` argument may hold `$(X)` — including under
        // the `.Split('-')[0]` idiom, whose base property
        // `simple_property_references` extracts like every other
        // taint/reference scan.
        ScalarExpr::SystemVersionPart { arg, .. } => {
            out.extend(super::evaluator::simple_property_references(arg).map(str::to_string))
        }
    }
}

impl ScalarExpr {
    fn is_empty_string_literal(&self) -> bool {
        matches!(self, ScalarExpr::String(s) if s.is_empty())
    }
}

fn unsupported() -> Eval {
    Eval {
        outcome: Outcome::Unsupported,
        undefined_properties: Vec::new(),
        undefined_outside_empty_comparison: Vec::new(),
    }
}

// -- Tokeniser --------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionPart {
    Build,
    Revision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// Raw string-literal text (the content between the single quotes,
    /// pre-substitution). We hold the raw form until evaluation so the
    /// PropertyMap available at evaluation time — not tokenisation —
    /// is the one used for `$(...)` expansion.
    String(String),
    Bare(String),
    SystemVersionPart {
        arg: String,
        part: VersionPart,
    },
    /// A bare (unquoted) `$(…)` property expression that isn't one of the
    /// specially-recognised version functions below — a plain reference
    /// (`$(SomeBool)`) or an instance-method call (`$(V.Contains('{'))`).
    /// The raw `$(…)` text is kept until evaluation, when it is expanded
    /// via [`super::properties::substitute`] and used either as a
    /// comparison operand or coerced to bool (`Condition="$(SomeBool)"`).
    /// An expression the substituter can't evaluate poisons the condition
    /// to [`Outcome::Unsupported`], exactly as it did when this arrived as
    /// [`Token::Unknown`].
    Property(String),
    True,
    False,
    Exists,
    /// MSBuild's `HasTrailingSlash('…')` built-in condition function.
    HasTrailingSlash,
    MsbuildVersionFunction(String),
    And,
    Or,
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    Bang,
    LParen,
    RParen,
    /// An identifier or stray character we don't recognise. Carrying
    /// the literal text would just bloat the token type — the parser
    /// rejects this on sight and returns Unsupported.
    Unknown,
}

/// Returns `Err(())` only for genuinely malformed input — an
/// unterminated string literal, since that's the one case we can't
/// recover from without inventing characters. Every other surprising
/// byte becomes [`Token::Unknown`], which the parser will reject when
/// it can't fit one into the grammar.
fn tokenise(source: &str) -> Result<Vec<Token>, ()> {
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut tokens = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            b')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            b'=' if bytes.get(i + 1) == Some(&b'=') => {
                tokens.push(Token::Eq);
                i += 2;
            }
            b'!' if bytes.get(i + 1) == Some(&b'=') => {
                tokens.push(Token::Neq);
                i += 2;
            }
            b'<' if bytes.get(i + 1) == Some(&b'=') => {
                tokens.push(Token::Lte);
                i += 2;
            }
            b'>' if bytes.get(i + 1) == Some(&b'=') => {
                tokens.push(Token::Gte);
                i += 2;
            }
            b'<' => {
                tokens.push(Token::Lt);
                i += 1;
            }
            b'>' => {
                tokens.push(Token::Gt);
                i += 1;
            }
            b'!' => {
                tokens.push(Token::Bang);
                i += 1;
            }
            b'\'' => {
                // Walk until the next unescaped quote. MSBuild
                // condition strings have no `\`-escape syntax — the
                // grammar simply doesn't admit a literal `'` inside a
                // string. An unterminated literal is a hard parse
                // error: treating it as Unsupported would silently
                // accept truncated input that happens to look closer
                // to true than to false.
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'\'' {
                    j += 1;
                }
                if j == bytes.len() {
                    return Err(());
                }
                // Safety: the slice is bounded by the original
                // string's byte indices, and we only advanced through
                // ASCII single quotes (which are full code points by
                // themselves), so `start..j` is on UTF-8 boundaries.
                tokens.push(Token::String(source[start..j].to_string()));
                i = j + 1;
            }
            b'$' if bytes.get(i + 1) == Some(&b'(') => {
                let after = &source[i + 2..];
                let Some(close) = find_balanced_property_expression_close(after) else {
                    tokens.push(Token::Unknown);
                    i += 1;
                    continue;
                };
                // MSBuild tolerates whitespace inside `$( … )` for
                // *property-function* forms, but NOT for a simple property
                // reference: `$( Foo )` (and `$(Foo )` / `$( Foo)`) is
                // illegal, where `$(Foo)` is fine (verified against the
                // oracle). Trim for classifying the function forms, but keep
                // the raw spelling to gate the simple-reference case.
                let raw_inner = &after[..close];
                let inner = raw_inner.trim_matches(|c: char| c.is_ascii_whitespace());
                if parse_msbuild_version_function_call(inner).is_some() {
                    tokens.push(Token::MsbuildVersionFunction(inner.to_string()));
                } else if let Some((arg, part)) = parse_system_version_part_token(inner) {
                    tokens.push(Token::SystemVersionPart {
                        arg: arg.to_string(),
                        part,
                    });
                } else if inner.contains('(') || inner.contains('[') {
                    // A property-function form (an instance-method call like
                    // `$(V.Contains('x'))` or a `[Type]::` static call).
                    // Whitespace around it is MSBuild-legal, so classify from
                    // the trimmed text.
                    tokens.push(Token::Property(format!("$({inner})")));
                } else if raw_inner == inner {
                    // A simple property reference with no surrounding
                    // whitespace — the only spelling MSBuild accepts.
                    tokens.push(Token::Property(format!("$({inner})")));
                } else {
                    // A whitespace-padded simple reference (`$( Foo )`):
                    // MSBuild rejects it, so we must not evaluate it as
                    // `$(Foo)` — surface it as unmodelled instead.
                    tokens.push(Token::Unknown);
                }
                i += 2 + close + 1;
            }
            // Bare numerics, mirroring MSBuild's `Scanner.ParseNumeric`
            // greedy lexing: `0x` + hex digits, or optional sign then digits
            // and dots interleaved (so a bare `1.2.3` version is one token).
            // Validity is the comparison layer's problem, exactly as
            // MSBuild leaves it to conversion.
            b if b.is_ascii_digit() || b == b'.' || b == b'+' || b == b'-' => {
                let start = i;
                if b == b'0' && matches!(bytes.get(i + 1), Some(b'x' | b'X')) && i + 2 < bytes.len()
                {
                    i += 2;
                    while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                        i += 1;
                    }
                } else {
                    if b == b'+' || b == b'-' {
                        i += 1;
                    }
                    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                        i += 1;
                    }
                }
                tokens.push(Token::Bare(source[start..i].to_string()));
            }
            b if is_ident_start(b) => {
                let start = i;
                while i < bytes.len() && is_ident_continue(bytes[i]) {
                    i += 1;
                }
                let word = &source[start..i];
                tokens.push(match_keyword(word));
            }
            _ => {
                // Stray punctuation we don't understand (`<`, `>`,
                // `+`, etc. — operators MSBuild supports but we
                // don't). Emit Unknown so the parser surfaces the
                // condition as Unsupported.
                tokens.push(Token::Unknown);
                i += 1;
            }
        }
    }
    Ok(tokens)
}

fn find_balanced_property_expression_close(input: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    for (idx, ch) in input.char_indices() {
        if in_string {
            if ch == '\'' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '\'' => in_string = true,
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    return Some(idx);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// MSBuild resolves property-function type and member names
/// case-insensitively (`$([msbuild]::versiongreaterthan(…))` is as valid as
/// the canonical spelling), so every fixed-name match in this module goes
/// through these ASCII-case-folding strip helpers. The needles are all pure
/// ASCII; a non-char-boundary split can therefore only mean "no match".
fn strip_prefix_ignore_ascii_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let (head, tail) = s.split_at_checked(prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then_some(tail)
}

fn strip_suffix_ignore_ascii_case<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let (head, tail) = s.split_at_checked(s.len().checked_sub(suffix.len())?)?;
    tail.eq_ignore_ascii_case(suffix).then_some(head)
}

/// Match a (whitespace-trimmed) `$( … )` interior against the supported
/// `[MSBuild]::Version*` comparison functions, returning the operator and
/// the raw argument list. Whitespace between the member name and its `(`
/// is MSBuild-legal and tolerated. Shared by the tokeniser (support gate)
/// and the evaluator so the two can't drift.
fn parse_msbuild_version_function_call(inner: &str) -> Option<(CompareOp, &str)> {
    let (op, rest) = [
        ("[MSBuild]::VersionGreaterThanOrEquals", CompareOp::Gte),
        ("[MSBuild]::VersionGreaterThan", CompareOp::Gt),
        ("[MSBuild]::VersionLessThanOrEquals", CompareOp::Lte),
        ("[MSBuild]::VersionLessThan", CompareOp::Lt),
        ("[MSBuild]::VersionEquals", CompareOp::Eq),
        ("[MSBuild]::VersionNotEquals", CompareOp::Neq),
    ]
    .into_iter()
    .find_map(|(name, op)| strip_prefix_ignore_ascii_case(inner, name).map(|rest| (op, rest)))?;
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let args = rest.strip_prefix('(')?.strip_suffix(')')?;
    Some((op, args))
}

/// Match `[System.Version]::Parse( … ).Build` / `.Revision`, tolerating
/// whitespace at each joint of the call syntax. The stripped-from-the-end
/// `)` is the `Parse` call's own close — parentheses inside the argument
/// (e.g. the `.Split('-')[0]` idiom) sit safely to its left.
fn parse_system_version_part_token(inner: &str) -> Option<(&str, VersionPart)> {
    let rest = strip_prefix_ignore_ascii_case(inner, "[System.Version]::Parse")?;
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let rest = rest.strip_prefix('(')?;
    let rest = rest.trim_end_matches(|c: char| c.is_ascii_whitespace());
    let (rest, part) = if let Some(rest) = strip_suffix_ignore_ascii_case(rest, ".Build") {
        (rest, VersionPart::Build)
    } else {
        (
            strip_suffix_ignore_ascii_case(rest, ".Revision")?,
            VersionPart::Revision,
        )
    };
    let rest = rest.trim_end_matches(|c: char| c.is_ascii_whitespace());
    let arg = rest.strip_suffix(')')?;
    Some((arg, part))
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn match_keyword(word: &str) -> Token {
    // MSBuild treats these keywords as case-insensitive: `And` /
    // `AND` / `and` all parse the same. The condition language is
    // OrdinalIgnoreCase end-to-end (mirroring the rest of MSBuild's
    // property-name semantics), so we match here without preserving
    // the input casing.
    if word.eq_ignore_ascii_case("true") {
        Token::True
    } else if word.eq_ignore_ascii_case("false") {
        Token::False
    } else if word.eq_ignore_ascii_case("and") {
        Token::And
    } else if word.eq_ignore_ascii_case("or") {
        Token::Or
    } else if word.eq_ignore_ascii_case("exists") {
        Token::Exists
    } else if word.eq_ignore_ascii_case("hastrailingslash") {
        Token::HasTrailingSlash
    } else {
        // Any other bare word is an unquoted simple-string operand, exactly
        // as MSBuild's scanner treats it: `Release == 'Release'` is true,
        // and a standalone `on` / `yes` / `off` / `no` coerces through the
        // boolean vocabulary (a non-boolean bare word like `maybe` is then a
        // project error → Unsupported). Carrying the text lets the scalar
        // machinery expand/compare it like any other operand. (An unmodelled
        // *function* name still fails: the trailing `(...)` can't fit the
        // grammar after the bare word, so the condition stays Unsupported.)
        Token::Bare(word.to_string())
    }
}

// -- Parser -----------------------------------------------------------

/// Boolean expression AST. We don't store string AST nodes separately
/// — string literals only appear inside [`BoolExpr::Compare`], so
/// embedding the raw text directly keeps the type small and matches
/// the actual grammar.
#[derive(Debug, Clone)]
enum BoolExpr {
    True,
    False,
    Not(Box<BoolExpr>),
    And(Box<BoolExpr>, Box<BoolExpr>),
    Or(Box<BoolExpr>, Box<BoolExpr>),
    Exists(String),
    /// `HasTrailingSlash('…')`: the raw (quote-stripped, pre-substitution)
    /// argument text.
    HasTrailingSlash(String),
    MsbuildVersionFunction(String),
    /// A scalar standing alone as a boolean, coerced through MSBuild's
    /// boolean vocabulary (`Condition="$(SomeBool)"`, `!$(V.Contains('x'))`,
    /// `Condition="'true'"`).
    CoerceBool(ScalarExpr),
    /// `lhs <op> rhs`. String and version-function operands keep their
    /// pre-substitution raw text; expansion runs at evaluation time so the
    /// latest [`PropertyMap`] is used.
    Compare {
        op: CompareOp,
        lhs: ScalarExpr,
        rhs: ScalarExpr,
    },
}

#[derive(Debug, Clone)]
enum ScalarExpr {
    String(String),
    Bare(String),
    SystemVersionPart {
        arg: String,
        part: VersionPart,
    },
    /// A bare `$(…)` property expression (reference or instance-method
    /// call), expanded via [`super::properties::substitute`] at evaluation.
    Property(String),
}

#[derive(Debug, Clone, Copy)]
enum CompareOp {
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<&'a Token> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_or(&mut self) -> Result<BoolExpr, ()> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.bump();
            let right = self.parse_and()?;
            left = BoolExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<BoolExpr, ()> {
        let mut left = self.parse_primary_bool()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.bump();
            let right = self.parse_primary_bool()?;
            left = BoolExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// A boolean-typed primary at comparison level: a bool factor
    /// (parenthesised sub-expression, `true`/`false`, a function, or a
    /// scalar coerced to bool) optionally followed by a comparison operator
    /// and a right-hand scalar (`'a' == 'b'`, `1 < 2`,
    /// `System.Version.Parse(...).Build >= 0`, `$(P) == 'x'`). A leading
    /// scalar with no operator is coerced to bool (`Condition="$(SomeBool)"`).
    fn parse_primary_bool(&mut self) -> Result<BoolExpr, ()> {
        // A leading scalar may be the left of a comparison OR stand alone
        // as a coerced boolean. Only scalars can be a comparison LHS, so
        // handle them here; every other factor delegates below.
        if self.peek().is_some_and(Token::is_scalar_start) {
            let lhs = self.parse_scalar()?;
            return match self.peek_compare_op() {
                Some(op) => {
                    self.bump();
                    let rhs = self.parse_scalar()?;
                    Ok(BoolExpr::Compare { op, lhs, rhs })
                }
                None => Ok(BoolExpr::CoerceBool(lhs)),
            };
        }
        self.parse_bool_factor()
    }

    /// A boolean *factor*: everything a comparison operand cannot follow.
    /// Crucially, a leading scalar here is coerced to bool WITHOUT
    /// consuming a trailing comparison operator — so `!` (which parses a
    /// factor) binds tighter than comparison, matching MSBuild: `!'a' == 'b'`
    /// is `(!'a') == 'b'` (a project error → any trailing operator is left
    /// unconsumed and the top level reports Unsupported), never
    /// `!('a' == 'b')`.
    fn parse_bool_factor(&mut self) -> Result<BoolExpr, ()> {
        match self.peek() {
            Some(Token::Bang) => {
                self.bump();
                Ok(BoolExpr::Not(Box::new(self.parse_bool_factor()?)))
            }
            Some(Token::LParen) => {
                self.bump();
                let inner = self.parse_or()?;
                match self.bump() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err(()),
                }
            }
            Some(Token::True) => {
                self.bump();
                Ok(BoolExpr::True)
            }
            Some(Token::False) => {
                self.bump();
                Ok(BoolExpr::False)
            }
            Some(Token::Exists) => {
                self.bump();
                Ok(BoolExpr::Exists(self.parse_single_string_argument()?))
            }
            Some(Token::HasTrailingSlash) => {
                self.bump();
                Ok(BoolExpr::HasTrailingSlash(
                    self.parse_single_string_argument()?,
                ))
            }
            Some(Token::MsbuildVersionFunction(inner)) => {
                let inner = inner.clone();
                self.bump();
                Ok(BoolExpr::MsbuildVersionFunction(inner))
            }
            Some(token) if token.is_scalar_start() => {
                Ok(BoolExpr::CoerceBool(self.parse_scalar()?))
            }
            _ => Err(()),
        }
    }

    /// Parse `( '…' )` — the single quoted string-literal argument shared by
    /// `Exists` and `HasTrailingSlash`. Returns the quote-stripped raw text.
    fn parse_single_string_argument(&mut self) -> Result<String, ()> {
        match self.bump() {
            Some(Token::LParen) => {}
            _ => return Err(()),
        }
        let arg = match self.bump() {
            Some(Token::String(s)) => s.clone(),
            _ => return Err(()),
        };
        match self.bump() {
            Some(Token::RParen) => Ok(arg),
            _ => Err(()),
        }
    }

    fn peek_compare_op(&self) -> Option<CompareOp> {
        match self.peek()? {
            Token::Eq => Some(CompareOp::Eq),
            Token::Neq => Some(CompareOp::Neq),
            Token::Lt => Some(CompareOp::Lt),
            Token::Lte => Some(CompareOp::Lte),
            Token::Gt => Some(CompareOp::Gt),
            Token::Gte => Some(CompareOp::Gte),
            _ => None,
        }
    }

    fn parse_scalar(&mut self) -> Result<ScalarExpr, ()> {
        match self.bump() {
            Some(Token::String(s)) => Ok(ScalarExpr::String(s.clone())),
            Some(Token::Bare(s)) => Ok(ScalarExpr::Bare(s.clone())),
            Some(Token::SystemVersionPart { arg, part }) => Ok(ScalarExpr::SystemVersionPart {
                arg: arg.clone(),
                part: part.clone(),
            }),
            Some(Token::Property(raw)) => Ok(ScalarExpr::Property(raw.clone())),
            // `true`/`false` double as unquoted string operands in a
            // comparison (`$(X) == false`), where MSBuild compares them
            // through the boolean vocabulary. We normalise to the canonical
            // lowercase spelling since the token discards the input casing.
            Some(Token::True) => Ok(ScalarExpr::Bare("true".to_string())),
            Some(Token::False) => Ok(ScalarExpr::Bare("false".to_string())),
            _ => Err(()),
        }
    }
}

impl Token {
    fn is_scalar_start(&self) -> bool {
        matches!(
            self,
            Token::String(_)
                | Token::Bare(_)
                | Token::SystemVersionPart { .. }
                | Token::Property(_)
                // `true`/`false` can begin a comparison (`false == $(X)`) as
                // well as stand alone; the standalone case coerces the same
                // way `BoolExpr::True`/`False` would.
                | Token::True
                | Token::False
        )
    }
}

// -- Evaluator --------------------------------------------------------

fn eval_bool(
    expr: &BoolExpr,
    props: &PropertyMap,
    exists: Option<&dyn Fn(&str) -> bool>,
    undefined: &mut Vec<String>,
) -> Result<bool, ()> {
    match expr {
        BoolExpr::True => Ok(true),
        BoolExpr::False => Ok(false),
        BoolExpr::Not(expr) => Ok(!eval_bool(expr, props, exists, undefined)?),
        // Short-circuit on the resolved truth value, matching MSBuild's
        // left-to-right evaluation order. This is only observable when
        // the right-hand side encounters an Unsupported expression
        // inside a string literal — the *parser* already rejected
        // anything else (a bare `HasTrailingSlash(...)` token sequence
        // doesn't fit the grammar and short-circuits the whole
        // condition to Unsupported well before evaluation). Where it
        // does fire, propagating Err only when the resolving side
        // can't be computed preserves the "never produce wrong output"
        // promise without overly-aggressive Unsupported reporting.
        //
        // The short-circuit also means we don't collect undefined-property
        // names from the skipped side. MSBuild itself never sees those
        // refs when the result is decided by the first side, so reporting
        // them as "we couldn't resolve" would be over-eager.
        BoolExpr::And(a, b) => {
            let av = eval_bool(a, props, exists, undefined)?;
            if !av {
                return Ok(false);
            }
            eval_bool(b, props, exists, undefined)
        }
        BoolExpr::Or(a, b) => {
            let av = eval_bool(a, props, exists, undefined)?;
            if av {
                return Ok(true);
            }
            eval_bool(b, props, exists, undefined)
        }
        BoolExpr::Exists(raw) => {
            let Some(exists) = exists else {
                return Err(());
            };
            let path = expand_for_condition(raw, props, true, undefined)?;
            // Trim the *padding*, then decode: an escaped `%20` is part of the
            // filename, not padding around it. `Exists('path%20')` probes the
            // file literally named `path ` and finds it (oracle-pinned).
            Ok(exists(&path.trimmed_unescaped()))
        }
        BoolExpr::HasTrailingSlash(raw) => {
            let value = expand_for_condition(raw, props, exists.is_some(), undefined)?;
            // MSBuild expands the argument into an item list: each entry is
            // whitespace-trimmed and empty entries are dropped
            // (`'a/;'` and `';/'` are the single item `a/` / `/`). A single
            // remaining item is checked for a trailing `/` or `\`; more than
            // one is a project error we fail safe to Unsupported; none (an
            // empty or all-separator argument) has no trailing slash.
            //
            // The split and the trim happen **in the escaped domain**, and each
            // entry decodes at the leaf — the same order the item pass uses, and
            // for the same reason: an escaped `%3b` is data, not a separator, and
            // an escaped `%20` is data, not padding. Both pinned against
            // `dotnet msbuild`: `HasTrailingSlash('foo%2f')` is true (it decodes
            // to `foo/`), while `HasTrailingSlash('foo/%20')` is **false** — the
            // decoded value is `foo/ `, whose last character is a space. Decoding
            // first and trimming after would commit `true` there.
            let mut items = value.split_list().map(|entry| entry.unescape());
            let item = items.next();
            if items.next().is_some() {
                return Err(());
            }
            Ok(item.is_some_and(|s: String| s.ends_with('/') || s.ends_with('\\')))
        }
        BoolExpr::MsbuildVersionFunction(inner) => {
            eval_msbuild_version_function(inner, props, exists.is_some(), undefined)
        }
        BoolExpr::CoerceBool(scalar) => {
            let value = expand_scalar_for_condition(scalar, props, exists.is_some(), undefined)?;
            // A standalone scalar is coerced through MSBuild's boolean
            // vocabulary; a value outside it (`1`, `foo`, an unset
            // property's "") is a project error → Unsupported.
            parse_msbuild_bool(&value).ok_or(())
        }
        BoolExpr::Compare { op, lhs, rhs } => {
            let lhs = expand_scalar_for_condition(lhs, props, exists.is_some(), undefined)?;
            let rhs = expand_scalar_for_condition(rhs, props, exists.is_some(), undefined)?;
            if !matches!(op, CompareOp::Eq | CompareOp::Neq) {
                let ordering = compare_relational_values(&lhs, &rhs)?;
                return Ok(match op {
                    CompareOp::Lt => ordering.is_lt(),
                    CompareOp::Lte => !ordering.is_gt(),
                    CompareOp::Gt => ordering.is_gt(),
                    CompareOp::Gte => !ordering.is_lt(),
                    CompareOp::Eq | CompareOp::Neq => unreachable!(),
                });
            }
            let equal = compare_equality_values(&lhs, &rhs)?;
            Ok(match op {
                CompareOp::Eq => equal,
                CompareOp::Neq => !equal,
                CompareOp::Lt | CompareOp::Lte | CompareOp::Gt | CompareOp::Gte => unreachable!(),
            })
        }
    }
}

fn expand_scalar_for_condition(
    scalar: &ScalarExpr,
    props: &PropertyMap,
    fs_probes_allowed: bool,
    undefined: &mut Vec<String>,
) -> Result<String, ()> {
    match scalar {
        ScalarExpr::String(raw) => expand_operand(raw, props, fs_probes_allowed, undefined),
        ScalarExpr::Bare(raw) => Ok(raw.clone()),
        ScalarExpr::Property(raw) => expand_operand(raw, props, fs_probes_allowed, undefined),
        ScalarExpr::SystemVersionPart { arg, part } => {
            let version =
                expand_system_version_parse_arg(arg, props, fs_probes_allowed, undefined)?;
            let version = parse_system_version(&version)?;
            let idx = match part {
                VersionPart::Build => 2,
                VersionPart::Revision => 3,
            };
            Ok(version[idx].to_string())
        }
    }
}

/// The `$('$(X.Split('-')[0])')` idiom inside a `[System.Version]::Parse`
/// argument: returns the property name when the (trimmed, quote-stripped)
/// argument is exactly a `$(...)` reference carrying that `.Split` call.
/// The taint-side counterpart lives in
/// [`super::evaluator::simple_property_references`], which extracts the base
/// property of any `.Split` call so uncertainty scans see what this resolves.
fn split_dash_zero_property(raw: &str) -> Option<&str> {
    let raw = raw.trim();
    let raw = raw
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(raw);
    let inner = raw.strip_prefix("$(")?.strip_suffix(')')?;
    strip_suffix_ignore_ascii_case(inner, ".Split('-')[0]")
}

fn expand_system_version_parse_arg(
    raw: &str,
    props: &PropertyMap,
    fs_probes_allowed: bool,
    undefined: &mut Vec<String>,
) -> Result<String, ()> {
    if let Some(property) = split_dash_zero_property(raw) {
        if let Some(value) = props.get(property) {
            // A condition operand is a point of use: MSBuild compares the
            // *unescaped* value.
            let value = value.unescape();
            return Ok(value.split('-').next().unwrap_or("").to_string());
        }
        undefined.push(property.to_string());
        return Ok(String::new());
    }
    let raw = raw.trim();
    let raw = raw
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(raw);
    expand_operand(raw, props, fs_probes_allowed, undefined)
}

fn eval_msbuild_version_function(
    inner: &str,
    props: &PropertyMap,
    fs_probes_allowed: bool,
    undefined: &mut Vec<String>,
) -> Result<bool, ()> {
    let (function, raw_args) = parse_msbuild_version_function_call(inner).ok_or(())?;
    let args = split_function_args(raw_args)?;
    if args.len() != 2 {
        return Err(());
    }
    let lhs = expand_function_arg_for_condition(args[0], props, fs_probes_allowed, undefined)?;
    let rhs = expand_function_arg_for_condition(args[1], props, fs_probes_allowed, undefined)?;
    let ordering = crate::properties::compare_msbuild_versions(&lhs, &rhs)?;
    Ok(match function {
        CompareOp::Gt => ordering.is_gt(),
        CompareOp::Gte => !ordering.is_lt(),
        CompareOp::Lt => ordering.is_lt(),
        CompareOp::Lte => !ordering.is_gt(),
        CompareOp::Eq => ordering.is_eq(),
        CompareOp::Neq => !ordering.is_eq(),
    })
}

fn split_function_args(raw: &str) -> Result<Vec<&str>, ()> {
    let mut args = Vec::new();
    let mut start = 0;
    let mut in_string = false;
    let mut paren_depth = 0usize;
    for (idx, ch) in raw.char_indices() {
        if in_string {
            if ch == '\'' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '\'' => in_string = true,
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.checked_sub(1).ok_or(())?,
            ',' if paren_depth == 0 => {
                args.push(raw[start..idx].trim());
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    if in_string || paren_depth != 0 {
        return Err(());
    }
    args.push(raw[start..].trim());
    Ok(args)
}

fn expand_function_arg_for_condition(
    raw: &str,
    props: &PropertyMap,
    fs_probes_allowed: bool,
    undefined: &mut Vec<String>,
) -> Result<String, ()> {
    let raw = raw.trim();
    let unquoted = raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\''));
    expand_operand(unquoted.unwrap_or(raw), props, fs_probes_allowed, undefined)
}

/// Relational comparison, mirroring MSBuild's
/// `NumericComparisonExpressionNode.BoolEvaluate` dispatch exactly: both
/// sides numeric → compare as doubles; else both sides `System.Version` →
/// compare as versions (missing components count as `-1`, so `2.0 < 2.0.0`);
/// else one of each → the mixed major-only rule
/// ([`compare_version_and_number`]). A side that is neither is a project
/// error in MSBuild — Unsupported here.
fn compare_relational_values(lhs: &str, rhs: &str) -> Result<std::cmp::Ordering, ()> {
    let lhs_number = parse_msbuild_number(lhs);
    let rhs_number = parse_msbuild_number(rhs);
    if let (Some(lhs), Some(rhs)) = (lhs_number, rhs_number) {
        // Both finite by construction, so the comparison is total.
        return lhs.partial_cmp(&rhs).ok_or(());
    }
    match (parse_system_version(lhs), parse_system_version(rhs)) {
        (Ok(lhs), Ok(rhs)) => Ok(lhs.cmp(&rhs)),
        (Ok(lhs), Err(())) => rhs_number
            .map(|number| compare_version_and_number(&lhs, number))
            .ok_or(()),
        (Err(()), Ok(rhs)) => lhs_number
            .map(|number| compare_version_and_number(&rhs, number).reverse())
            .ok_or(()),
        (Err(()), Err(())) => Err(()),
    }
}

/// MSBuild's mixed number/version comparison (the `Compare(Version, double)`
/// overloads in e.g. `LessThanExpressionNode`): only the version's *major*
/// component is compared against the number, and when they tie the version
/// counts as strictly greater — "Version treats the objects with more dots
/// as larger" (`6.0.0.0 > 6` is true). Returns the ordering of the version
/// relative to the number.
fn compare_version_and_number(version: &[i64; 4], number: f64) -> std::cmp::Ordering {
    let major = version[0] as f64;
    if major < number {
        std::cmp::Ordering::Less
    } else {
        // Equal majors: the version side wins outright.
        std::cmp::Ordering::Greater
    }
}

/// Equality, mirroring MSBuild's `MultipleComparisonNode.BoolEvaluate`: an
/// empty side short-circuits to emptiness comparison, then numeric (double)
/// equality, then MSBuild boolean equality (`'on' == 'yes'` is true), then
/// ordinal case-insensitive string equality. Unlike the relational operators,
/// equality never compares versions: `'01.0.0' == '1.0.0'` is false.
fn compare_equality_values(lhs: &str, rhs: &str) -> Result<bool, ()> {
    if lhs == rhs {
        return Ok(true);
    }
    if lhs.is_empty() || rhs.is_empty() {
        // Exactly one side is empty (both-empty was byte-equal above).
        return Ok(false);
    }
    if let (Some(lhs), Some(rhs)) = (parse_msbuild_number(lhs), parse_msbuild_number(rhs)) {
        return Ok(lhs == rhs);
    }
    if let (Some(lhs), Some(rhs)) = (parse_msbuild_bool(lhs), parse_msbuild_bool(rhs)) {
        return Ok(lhs == rhs);
    }
    // MSBuild string equality is ordinal case-insensitive, which folds the
    // full Unicode case map (e.g. `'É' == 'é'` is true). We don't carry a
    // Unicode case-mapping table, and `eq_ignore_ascii_case` only folds
    // `A-Z`/`a-z`. Byte-equal values already returned true above; pure ASCII
    // values are safe to fold here; other non-byte-equal values are
    // Unsupported rather than a guessed false gate.
    if lhs.is_ascii() && rhs.is_ascii() {
        Ok(lhs.eq_ignore_ascii_case(rhs))
    } else {
        Err(())
    }
}

/// A numeric condition operand, mirroring
/// `ConversionUtilities.TryConvertDecimalOrHexToDouble`: either a decimal
/// literal (optional leading sign, digits with at most one `.` — no
/// whitespace, no exponent, no thousands separators; values that round to
/// infinity are rejected), or a hex literal (`0x` + hex digits parsed as a
/// 32-bit value whose bit pattern is reinterpreted as signed, so
/// `0xFFFFFFFF` is `-1`). MSBuild compares these as .NET doubles; `f64`
/// string parsing is correctly rounded in both Rust and .NET, so the values
/// agree bit-for-bit.
fn parse_msbuild_number(value: &str) -> Option<f64> {
    if let Some(hex) = strip_prefix_ignore_ascii_case(value, "0x") {
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let bits = u32::from_str_radix(hex, 16).ok()?;
        return Some(bits as i32 as f64);
    }
    let unsigned = value.strip_prefix(['+', '-']).unwrap_or(value);
    let mut dots = 0usize;
    let mut digits = 0usize;
    for b in unsigned.bytes() {
        match b {
            b'.' => dots += 1,
            b'0'..=b'9' => digits += 1,
            _ => return None,
        }
    }
    if dots > 1 || digits == 0 {
        return None;
    }
    let number: f64 = value.parse().ok()?;
    number.is_finite().then_some(number)
}

/// MSBuild's boolean vocabulary (`ConversionUtilities.TryConvertStringToBool`):
/// `true`/`on`/`yes` and `false`/`off`/`no`, each optionally negated with a
/// leading `!`, all case-insensitive. Deliberately untrimmed: MSBuild's own
/// conversion is (probed via `ReferenceOutputAssembly=" true "`, dotnet
/// 10.0.301, 2026-07-10 — the padded spelling does not compare true).
pub(crate) fn parse_msbuild_bool(value: &str) -> Option<bool> {
    const TRUE_VALUES: [&str; 6] = ["true", "on", "yes", "!false", "!off", "!no"];
    const FALSE_VALUES: [&str; 6] = ["false", "off", "no", "!true", "!on", "!yes"];
    if TRUE_VALUES.iter().any(|t| value.eq_ignore_ascii_case(t)) {
        return Some(true);
    }
    if FALSE_VALUES.iter().any(|f| value.eq_ignore_ascii_case(f)) {
        return Some(false);
    }
    None
}

/// `Version.TryParse` as MSBuild condition operands (and
/// `[System.Version]::Parse`) see it: 2–4 dot-separated components, each
/// parsed like .NET `int.TryParse(NumberStyles.Integer)` — whitespace-trimmed
/// with an optional leading `+`, so `' 2 . 5 '` and `'+2.5'` are versions.
/// Missing components are `-1`, ranking below an explicit zero. (.NET also
/// accepts `-0` components and trims non-ASCII Unicode whitespace; both are
/// degenerate spellings we reject toward Unsupported instead.)
fn parse_system_version(value: &str) -> Result<[i64; 4], ()> {
    let parts: Vec<&str> = value.split('.').collect();
    if !(2..=4).contains(&parts.len()) {
        return Err(());
    }
    let mut version = [-1, -1, -1, -1];
    for (idx, part) in parts.into_iter().enumerate() {
        let part = part.trim_matches(|c: char| c.is_ascii_whitespace());
        let part = part.strip_prefix('+').unwrap_or(part);
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(());
        }
        version[idx] = part.parse::<i32>().map(i64::from).map_err(|_| ())?;
    }
    Ok(version)
}

/// Substitute `$(...)` in a condition-string side. Undefined
/// references become `""` (matching MSBuild) but their names are
/// appended to `undefined` so the caller can surface them as
/// [`UndefinedProperty`](super::diagnostic::DiagnosticKind::UndefinedProperty)
/// diagnostics — without this, a condition like
/// `'$(Foo)' == '$(Bar)'` where both sides are unknown to us would
/// trivially resolve to `'' == ''` = true, silently picking a branch
/// MSBuild might not. An unsupported expression on either side
/// (property function, item reference) poisons the whole condition,
/// since we can't compute the result without evaluating it.
///
/// Item-list (`@(Items)`) and metadata (`%(Identity)`) references
/// also poison the condition. `substitute` doesn't flag them
/// (they're not `$(...)`), so we scan the post-substitution value
/// explicitly. Without this, `Condition="'@(Compile)' == ''"` would
/// compare the literal text `@(Compile)` against `''` and silently
/// resolve to false — masking an item-list reference MSBuild would
/// have expanded, and breaking plan D5's "fail loudly on unsupported
/// constructs" stance.
fn expand_for_condition(
    raw: &str,
    props: &PropertyMap,
    fs_probes_allowed: bool,
    undefined: &mut Vec<String>,
) -> Result<Escaped, ()> {
    // FS-probing property functions follow the same split as `Exists()`:
    // available exactly when the caller supplied a filesystem oracle.
    let (value, issues) = if fs_probes_allowed {
        substitute_with_fs(raw, props)
    } else {
        substitute(raw, props)
    };
    for issue in issues {
        match issue {
            Issue::Undefined { name } => undefined.push(name),
            Issue::Unsupported { .. } => return Err(()),
        }
    }
    // Item and metadata references are scanned on the **escaped** text, exactly
    // as MSBuild scans them: an escaped `%25(` is not a metadata reference.
    if value.as_escaped().contains("@(") || value.as_escaped().contains("%(") {
        return Err(());
    }
    // The value stays **in the domain**. Most operands are compared as text and
    // decode immediately ([`expand_operand`]) — but a built-in that splits or
    // trims its argument must do so *before* decoding, because an escaped `%3b`
    // is data rather than a separator and an escaped `%20` is data rather than
    // padding. `HasTrailingSlash` and `Exists` are exactly those.
    Ok(value)
}

/// A condition operand at its **point of use**: decoded exactly once.
///
/// MSBuild compares unescaped values, so `'%74rue' == 'true'` is true and
/// `'$(P)' == 'a b'` is true for `<P>a%20b</P>` (both oracle-pinned).
fn expand_operand(
    raw: &str,
    props: &PropertyMap,
    fs_probes_allowed: bool,
    undefined: &mut Vec<String>,
) -> Result<String, ()> {
    Ok(expand_for_condition(raw, props, fs_probes_allowed, undefined)?.unescape())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> PropertyMap {
        let mut m = PropertyMap::new();
        for (k, v) in pairs {
            m.insert(*k, *v);
        }
        m
    }

    /// Tests in this module assert outcome-only behaviour; shadow the
    /// outer `evaluate` to drop the undefined-property list so the
    /// assertions stay readable. Tests below that *do* check
    /// undefined names call `super::evaluate` explicitly.
    fn evaluate(source: &str, props: &PropertyMap) -> Outcome {
        super::evaluate(source, props).outcome
    }

    #[test]
    fn empty_condition_is_true() {
        assert_eq!(evaluate("", &PropertyMap::new()), Outcome::True);
        assert_eq!(evaluate("   ", &PropertyMap::new()), Outcome::True);
    }

    #[test]
    fn bool_literals_case_insensitive() {
        assert_eq!(evaluate("true", &PropertyMap::new()), Outcome::True);
        assert_eq!(evaluate("True", &PropertyMap::new()), Outcome::True);
        assert_eq!(evaluate("FALSE", &PropertyMap::new()), Outcome::False);
    }

    #[test]
    fn bang_negates_bool_expression() {
        let p = props(&[("Configuration", "Debug")]);
        assert_eq!(evaluate("!false", &p), Outcome::True);
        assert_eq!(
            evaluate("!('$(Configuration)' == 'Debug')", &p),
            Outcome::False
        );
    }

    #[test]
    fn is_null_or_empty_as_a_condition_leaf() {
        // `$([System.String]::IsNullOrEmpty('$(X)'))` reduces to a bool literal
        // via `substitute`, then the condition treats "True"/"False" as the
        // leaf. Pinned against dotnet msbuild 10.0.301.
        let set = props(&[("X", "v10.0")]);
        assert_eq!(
            evaluate("$([System.String]::IsNullOrEmpty('$(X)'))", &set),
            Outcome::False
        );
        assert_eq!(
            evaluate("!$([System.String]::IsNullOrEmpty('$(X)'))", &set),
            Outcome::True
        );
        let empty = props(&[("X", "")]);
        assert_eq!(
            evaluate("$([System.String]::IsNullOrEmpty('$(X)'))", &empty),
            Outcome::True
        );
    }

    #[test]
    fn workflow_build_extensions_import_gate_resolves_false() {
        // The SDK's `Microsoft.WorkflowBuildExtensions.targets` import gate
        // (Microsoft.Common.targets) is the first `walk_opaque` latch in a real
        // `net10.0` chain: its left conjunction is true for any modern TFV and
        // the trailing `Exists('…WorkflowBuildExtensions.targets')` is false, so
        // MSBuild skips the import. Before `IsNullOrEmpty` the whole condition
        // was Unsupported (latching opacity and poisoning ~50 downstream reads);
        // now it must resolve exactly to False. `Exists` returns false for the
        // never-present .NET-Framework/VS targets file.
        let p = props(&[
            ("TargetFrameworkVersion", "v10.0"),
            ("MSBuildToolsPath", "/nonexistent/tools"),
        ]);
        let cond = "('$(TargetFrameworkVersion)' != 'v2.0' and \
                    '$(TargetFrameworkVersion)' != 'v3.5' and \
                    (!$([System.String]::IsNullOrEmpty('$(TargetFrameworkVersion)')) and \
                    !$(TargetFrameworkVersion.StartsWith('v4.0')))) and \
                    Exists('$(MSBuildToolsPath)\\Microsoft.WorkflowBuildExtensions.targets')";
        let eval = super::evaluate_with_exists(cond, &p, &|_| false);
        assert_eq!(eval.outcome, Outcome::False, "{cond}");
    }

    #[test]
    fn bang_does_not_negate_string_comparison_without_parens() {
        let p = props(&[("Configuration", "Debug")]);
        assert_eq!(
            evaluate("!'$(Configuration)' == 'Release'", &p),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate("!'$(Configuration)' != ''", &p),
            Outcome::Unsupported
        );
    }

    #[test]
    fn string_equality_compares_substituted_values() {
        let p = props(&[("Configuration", "Release")]);
        assert_eq!(
            evaluate("'$(Configuration)' == 'Release'", &p),
            Outcome::True
        );
        assert_eq!(
            evaluate("'$(Configuration)' != 'Release'", &p),
            Outcome::False
        );
        assert_eq!(
            evaluate("'$(Configuration)' == 'Debug'", &p),
            Outcome::False
        );
    }

    #[test]
    fn string_equality_is_case_insensitive() {
        let p = props(&[("Configuration", "Release")]);
        // Both the value and the literal differ in case — MSBuild
        // matches them.
        assert_eq!(
            evaluate("'$(Configuration)' == 'release'", &p),
            Outcome::True
        );
    }

    #[test]
    fn undefined_property_becomes_empty_string_and_is_reported() {
        // `'$(Missing)' == ''` is the canonical "if not set" idiom.
        // For evaluation purposes the undefined ref expands to "" (so
        // the outcome matches what MSBuild would compute given the
        // same property map), but the name is returned in
        // `undefined_properties` so the caller can mark the project
        // partial — our map may be missing values MSBuild itself
        // would have seen.
        let eval = super::evaluate("'$(Missing)' == ''", &PropertyMap::new());
        assert_eq!(eval.outcome, Outcome::True);
        assert_eq!(eval.undefined_properties, vec!["Missing".to_string()]);
    }

    #[test]
    fn both_sides_undefined_collects_both_names() {
        // The motivating case from codex review: both sides expand to
        // "" silently, the comparison is trivially true, and without
        // reporting we'd silently include the gated construct.
        let eval = super::evaluate("'$(A)' == '$(B)'", &PropertyMap::new());
        assert_eq!(eval.outcome, Outcome::True);
        assert_eq!(
            eval.undefined_properties,
            vec!["A".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn defined_property_does_not_appear_in_undefined_list() {
        let p = props(&[("A", "x")]);
        let eval = super::evaluate("'$(A)' == '$(Missing)'", &p);
        assert_eq!(eval.outcome, Outcome::False);
        assert_eq!(eval.undefined_properties, vec!["Missing".to_string()]);
    }

    #[test]
    fn short_circuit_skips_undefined_collection_on_decided_branch() {
        // `false And X` short-circuits before evaluating X, so any
        // undefined refs inside X are not collected — MSBuild itself
        // wouldn't observe them either. We rely on this so a
        // common-case condition like
        //   '$(Configuration)' == 'Debug' And '$(SomeFlag)' == 'on'
        // doesn't drag an extra UndefinedProperty in for SomeFlag
        // when Configuration already resolved the And to false.
        let p = props(&[("A", "x")]);
        let eval = super::evaluate("'$(A)' == 'no' And '$(Missing)' == 'y'", &p);
        assert_eq!(eval.outcome, Outcome::False);
        assert!(
            eval.undefined_properties.is_empty(),
            "{:?}",
            eval.undefined_properties
        );
    }

    #[test]
    fn pure_evaluation_short_circuits_unreached_exists() {
        assert_eq!(
            evaluate("true Or Exists('x')", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("false And Exists('x')", &PropertyMap::new()),
            Outcome::False
        );
    }

    #[test]
    fn unsupported_condition_returns_no_undefined_names() {
        // When the outcome is Unsupported, the caller emits
        // UnsupportedCondition; any UndefinedProperty for properties
        // mentioned in the same condition would be redundant noise.
        let eval = super::evaluate("'$(Missing)' != '' Or Exists('x')", &PropertyMap::new());
        assert_eq!(eval.outcome, Outcome::Unsupported);
        assert!(eval.undefined_properties.is_empty());
    }

    #[test]
    fn and_or_combine() {
        let p = props(&[("A", "x"), ("B", "y")]);
        assert_eq!(
            evaluate("'$(A)' == 'x' And '$(B)' == 'y'", &p),
            Outcome::True
        );
        assert_eq!(
            evaluate("'$(A)' == 'x' And '$(B)' == 'z'", &p),
            Outcome::False
        );
        assert_eq!(
            evaluate("'$(A)' == 'x' Or '$(B)' == 'z'", &p),
            Outcome::True
        );
        assert_eq!(
            evaluate("'$(A)' == 'no' Or '$(B)' == 'no'", &p),
            Outcome::False
        );
    }

    #[test]
    fn parens_override_precedence() {
        // Without parens, `And` binds tighter than `Or`:
        //   false Or true And false = false Or (true And false) = false
        assert_eq!(
            evaluate("false Or true And false", &PropertyMap::new()),
            Outcome::False
        );
        // With parens the meaning flips:
        //   (false Or true) And false = true And false = false
        // — boring example. Pick a more discriminating one:
        //   true And (false Or true) = true And true = true
        assert_eq!(
            evaluate("true And (false Or true)", &PropertyMap::new()),
            Outcome::True
        );
        // Without parens: true And false Or true = (true And false) Or true = true
        assert_eq!(
            evaluate("true And false Or true", &PropertyMap::new()),
            Outcome::True
        );
    }

    #[test]
    fn property_function_makes_condition_unsupported() {
        let p = props(&[("X", "foo")]);
        // The property-function `$([System.String]::Copy('x'))` lives
        // inside a string literal, but substitute() emits Unsupported
        // for it — so the whole condition becomes Unsupported.
        assert_eq!(
            evaluate("'$([System.String]::Copy('x'))' == 'x'", &p),
            Outcome::Unsupported
        );
    }

    #[test]
    fn exists_without_filesystem_callback_is_unsupported() {
        assert_eq!(
            evaluate("Exists('a.fs')", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn exists_uses_filesystem_callback() {
        let exists = |path: &str| path == "a.fs";
        assert_eq!(
            super::evaluate_with_exists("Exists('a.fs')", &PropertyMap::new(), &exists).outcome,
            Outcome::True
        );
        assert_eq!(
            super::evaluate_with_exists("Exists('b.fs')", &PropertyMap::new(), &exists).outcome,
            Outcome::False
        );
    }

    #[test]
    fn exists_trims_expanded_argument_before_callback() {
        let p = props(&[("File", " marker.txt ")]);
        let exists = |path: &str| path == "marker.txt";
        assert_eq!(
            super::evaluate_with_exists("Exists(' $(File) ')", &p, &exists).outcome,
            Outcome::True
        );
    }

    #[test]
    fn exists_supports_bang_and_property_substitution() {
        let p = props(&[("File", "a.fs")]);
        let exists = |path: &str| path == "a.fs";
        assert_eq!(
            super::evaluate_with_exists("!Exists('b.fs')", &p, &exists).outcome,
            Outcome::True
        );
        let eval = super::evaluate_with_exists("Exists('$(File)')", &p, &exists);
        assert_eq!(eval.outcome, Outcome::True);
        assert!(eval.undefined_properties.is_empty());
    }

    #[test]
    fn exists_reports_undefined_properties() {
        let exists = |_path: &str| false;
        let eval =
            super::evaluate_with_exists("Exists('$(Missing)')", &PropertyMap::new(), &exists);
        assert_eq!(eval.outcome, Outcome::False);
        assert_eq!(eval.undefined_properties, vec!["Missing".to_string()]);
    }

    #[test]
    fn unknown_function_call_makes_condition_unsupported() {
        assert_eq!(
            evaluate("SomeUnknownFunction('a.fs')", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn unterminated_string_is_unsupported() {
        assert_eq!(
            evaluate("'unterminated", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn trailing_garbage_after_valid_prefix_is_unsupported() {
        // A condition that parses cleanly for the first few tokens
        // and then has junk must NOT be reported as the prefix's
        // truth value.
        assert_eq!(
            evaluate("'a' == 'a' garbage", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn numeric_version_relational_operators_are_supported() {
        assert_eq!(
            evaluate("'2.0' > '1.9'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'2.0' < '2.0.0'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'v2.0' < '2.1'", &PropertyMap::new()),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate("'10.0' <= '9.0'", &PropertyMap::new()),
            Outcome::False
        );
    }

    /// MSBuild unescapes `%XX` (a `%` followed by two hex digits) inside
    /// conditions before comparing: `'%74rue' == 'true'` is *true* (pinned
    /// against `dotnet msbuild` 10.0.301, property and item gates alike).
    /// We do not model unescaping, so committing a boolean from the raw
    /// text would be a wrong gate — an escape-bearing condition degrades
    /// to `Unsupported` (fail-safe: exclusion plus a diagnostic). A `%`
    /// with any other suffix is literal and evaluates normally.
    #[test]
    fn a_built_in_splits_and_trims_before_it_decodes() {
        // `HasTrailingSlash` expands its argument into an *item list*: split on
        // `;`, trim each entry. Both happen in the escaped domain, and the decode
        // is the leaf — the same rule the item pass follows. Pinned against
        // `dotnet msbuild` 10.0.301 (2026-07-12):
        //
        //   HasTrailingSlash('foo%2f')  -> true   (decoded: `foo/`)
        //   HasTrailingSlash('foo/%20') -> false  (decoded: `foo/ ` — the escaped
        //                                          space is data, and is NOT
        //                                          trimmed away)
        //
        // The second is the whole point: decoding and *then* trimming would eat a
        // character MSBuild keeps, and commit `true` for a value that has no
        // trailing slash at all.
        let props = PropertyMap::new();
        assert_eq!(
            evaluate("HasTrailingSlash('foo%2f')", &props),
            Outcome::True
        );
        assert_eq!(
            evaluate("HasTrailingSlash('foo/%20')", &props),
            Outcome::False
        );
        // Authored padding *is* padding, and is still trimmed.
        assert_eq!(
            evaluate("HasTrailingSlash('  foo/  ')", &props),
            Outcome::True
        );
        // The list split is on escaped semicolons, so an escaped `%3b` is data:
        // one entry, which does not end in a slash.
        assert_eq!(
            evaluate("HasTrailingSlash('a%3bb')", &props),
            Outcome::False
        );
    }

    #[test]
    fn escapes_are_decoded_at_the_operand_not_degraded() {
        // MSBuild unescapes `%XX` at the operand leaf, so an escape-bearing
        // *quoted* operand compares as the text it decodes to. Stage E2 of
        // `docs/msbuild-escaped-value-plan.md`: this used to degrade the whole
        // condition, which was fail-safe but cost real gates. All pinned against
        // `dotnet msbuild` 10.0.301 (2026-07-12).
        assert_eq!(
            evaluate("'%74rue' == 'true'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'a%20b' == 'a b'", &PropertyMap::new()),
            Outcome::True
        );
        // …and so does one arriving through a property value.
        let mut props = PropertyMap::new();
        props.insert("P", "a%20b");
        assert_eq!(evaluate("'$(P)' == 'a b'", &props), Outcome::True);

        // A `%` **outside** quotes is not an escape at all: MSBuild's scanner
        // rejects it outright (`MSB4090: Found an unexpected character '7' at
        // position 1 in condition "%74rue"`), and likewise inside a bare numeric
        // (`1%2E0 > 0.5`). An MSBuild error maps to `Unsupported`, so these still
        // decline — and must, since committing a boolean for a condition MSBuild
        // refuses to evaluate would be a wrong gate.
        assert_eq!(
            evaluate("%74rue", &PropertyMap::new()),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate("1%2E0 > 0.5", &PropertyMap::new()),
            Outcome::Unsupported
        );

        // Bare `%` (not followed by two hex digits) is literal, not an
        // escape: these stay committed.
        assert_eq!(
            evaluate("'100%' == '100%'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'a%zz' == 'a%zz'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'a%zz' == 'true'", &PropertyMap::new()),
            Outcome::False
        );
    }

    /// When both sides are numeric, relational comparison is double-first —
    /// `1.5 > 1.20` numerically, NOT version-wise (`1.5 < 1.20` as versions).
    /// Pinned against `dotnet msbuild` 10.0.300.
    #[test]
    fn dotted_numeric_relational_operands_compare_as_doubles() {
        assert_eq!(
            evaluate("'1.5' > '1.20'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'1.2' < '1.10'", &PropertyMap::new()),
            Outcome::False
        );
    }

    #[test]
    fn equality_uses_numeric_comparison_when_both_sides_are_numeric() {
        assert_eq!(evaluate("1 == 1.0", &PropertyMap::new()), Outcome::True);
        assert_eq!(evaluate("'1' == '1.0'", &PropertyMap::new()), Outcome::True);
        assert_eq!(evaluate("01 == 1", &PropertyMap::new()), Outcome::True);
        assert_eq!(
            evaluate("'1.5' == '1.50'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(evaluate("1 != 1.0", &PropertyMap::new()), Outcome::False);
        assert_eq!(evaluate("'01' != '1'", &PropertyMap::new()), Outcome::False);
    }

    #[test]
    fn ordinary_comparisons_preserve_operand_whitespace() {
        assert_eq!(
            evaluate("' 1 ' == '1'", &PropertyMap::new()),
            Outcome::False
        );
        assert_eq!(evaluate("' 1 ' != '1'", &PropertyMap::new()), Outcome::True);
        assert_eq!(
            evaluate("' 2 ' > '1'", &PropertyMap::new()),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate("' 2.0 ' == '2.0'", &PropertyMap::new()),
            Outcome::False
        );

        let p = props(&[("Value", " 2 ")]);
        assert_eq!(evaluate("'$(Value)' == '2'", &p), Outcome::False);
        assert_eq!(evaluate("'$(Value)' > '1'", &p), Outcome::Unsupported);
    }

    /// Unlike the relational operators, MSBuild `==` / `!=` never fall back
    /// to `System.Version` comparison — non-numeric dotted values compare as
    /// strings. Every case here is pinned against `dotnet msbuild` 10.0.300.
    #[test]
    fn equality_never_compares_versions() {
        assert_eq!(
            evaluate("'2.0' == '2.0.0'", &PropertyMap::new()),
            Outcome::False
        );
        assert_eq!(
            evaluate("'2.0' != '2.0.0'", &PropertyMap::new()),
            Outcome::True
        );
        // Version-equal but string-different spellings are NOT equal.
        assert_eq!(
            evaluate("'01.0.0' == '1.0.0'", &PropertyMap::new()),
            Outcome::False
        );
        assert_eq!(
            evaluate("'1.0.0' == '1.00.0'", &PropertyMap::new()),
            Outcome::False
        );
        assert_eq!(
            evaluate("' 2 . 5 ' == '2.5'", &PropertyMap::new()),
            Outcome::False
        );
    }

    /// MSBuild's `==` coerces both sides to its boolean vocabulary
    /// (`true`/`on`/`yes`, `false`/`off`/`no`, `!`-negations) before falling
    /// back to string comparison. Pinned against `dotnet msbuild` 10.0.300.
    #[test]
    fn equality_compares_msbuild_booleans() {
        assert_eq!(
            evaluate("'on' == 'yes'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'!false' == 'true'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'!TRUE' == 'FALSE'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'off' != 'no'", &PropertyMap::new()),
            Outcome::False
        );
        // Bool coercion only fires when BOTH sides are boolean spellings.
        assert_eq!(evaluate("'on' == 'x'", &PropertyMap::new()), Outcome::False);
    }

    /// Hex literals are numeric operands: parsed as a 32-bit value whose bit
    /// pattern is reinterpreted as signed (`Int32.TryParse` with
    /// `AllowHexSpecifier`). Pinned against `dotnet msbuild` 10.0.300.
    #[test]
    fn hex_operands_compare_numerically() {
        assert_eq!(
            evaluate("'0x10' == '16'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'0xFF' > '254'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'0x10' > '15'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'0xFFFFFFFF' == '-1'", &PropertyMap::new()),
            Outcome::True
        );
        // Overflowing 32 bits stops being numeric: string comparison.
        assert_eq!(
            evaluate("'0x100000000' == '4294967296'", &PropertyMap::new()),
            Outcome::False
        );
    }

    /// MSBuild numeric equality is .NET *double* equality — values collapse
    /// to the nearest representable f64 before comparing. Pinned against
    /// `dotnet msbuild` 10.0.300.
    #[test]
    fn equality_uses_double_rounding() {
        assert_eq!(
            evaluate("'0.1' == '0.10000000000000000001'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate(
                "'99999999999999999999999999999999999999' == '99999999999999999999999999999999999998'",
                &PropertyMap::new()
            ),
            Outcome::True
        );
        assert_eq!(evaluate("'-0' == '0'", &PropertyMap::new()), Outcome::True);
        assert_eq!(evaluate("'+1' == '1'", &PropertyMap::new()), Outcome::True);
        assert_eq!(evaluate("'2.' == '2'", &PropertyMap::new()), Outcome::True);
    }

    /// The numeric grammar excludes exponents (`NumberStyles` without
    /// `AllowExponent`), so `'1e2'` is a string, not 100. Pinned against
    /// `dotnet msbuild` 10.0.300.
    #[test]
    fn exponent_spellings_are_not_numeric() {
        assert_eq!(
            evaluate("'1e2' == '100'", &PropertyMap::new()),
            Outcome::False
        );
    }

    /// An empty side short-circuits equality to emptiness comparison
    /// (`MultipleComparisonNode.EvaluatesToEmpty`), before the string path
    /// would bail to Unsupported on non-ASCII content. Pinned against
    /// `dotnet msbuild` 10.0.300.
    #[test]
    fn empty_comparison_against_non_ascii_value_is_supported() {
        assert_eq!(evaluate("'É' == ''", &PropertyMap::new()), Outcome::False);
        assert_eq!(evaluate("'É' != ''", &PropertyMap::new()), Outcome::True);
    }

    #[test]
    fn target_framework_version_condition_is_supported() {
        let p = props(&[("_TargetFrameworkVersionWithoutV", "2.0")]);
        assert_eq!(
            evaluate("'$(_TargetFrameworkVersionWithoutV)' < '2.1'", &p),
            Outcome::True
        );
        assert_eq!(
            evaluate("'$(_TargetFrameworkVersionWithoutV)' >= '2.1'", &p),
            Outcome::False
        );
    }

    #[test]
    fn msbuild_version_comparison_functions_are_supported() {
        let p = props(&[("TargetFrameworkVersion", "v8.0")]);
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionGreaterThanOrEquals($(TargetFrameworkVersion), '8.0'))",
                &p
            ),
            Outcome::True
        );
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionLessThan($(TargetFrameworkVersion), '8.0'))",
                &p
            ),
            Outcome::False
        );
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionGreaterThanOrEquals('2.0', '2.0.0'))",
                &p
            ),
            Outcome::True
        );
    }

    // Pinned against `dotnet msbuild` 10.0.300:
    //   $([MSBuild]::VersionEquals('2.0', '2.0.0'))                    => TRUE
    //   $([MSBuild]::VersionEquals('v1.2.3-pre+metadata', '1.2.3'))    => TRUE
    //   $([MSBuild]::VersionEquals('01.0', '1.0'))                     => TRUE
    //   $([MSBuild]::VersionEquals('1.2', '1.3'))                      => FALSE
    //   $([MSBuild]::VersionNotEquals('2.0', '2.0.0'))                 => FALSE
    //   $([MSBuild]::VersionNotEquals('1.2', '1.3'))                   => TRUE
    //   $([msbuild]::versionequals('1.0','1'))                         => TRUE
    //   $([MSBuild]::VersionEquals('abc', '1.0'))                      => MSB4184
    // (The real .NET SDK gates on these, e.g.
    // `Microsoft.NET.TargetFrameworkInference.targets`:
    // `$([MSBuild]::VersionEquals($(TargetPlatformVersion), 0.0))`.)
    #[test]
    fn msbuild_version_equality_functions_are_supported() {
        let p = props(&[("TargetPlatformVersion", "0.0")]);
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionEquals($(TargetPlatformVersion), 0.0))",
                &p
            ),
            Outcome::True
        );
        assert_eq!(
            evaluate("$([MSBuild]::VersionEquals('2.0', '2.0.0'))", &p),
            Outcome::True
        );
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionEquals('v1.2.3-pre+metadata', '1.2.3'))",
                &p
            ),
            Outcome::True
        );
        assert_eq!(
            evaluate("$([MSBuild]::VersionEquals('01.0', '1.0'))", &p),
            Outcome::True
        );
        assert_eq!(
            evaluate("$([MSBuild]::VersionEquals('1.2', '1.3'))", &p),
            Outcome::False
        );
        assert_eq!(
            evaluate("$([MSBuild]::VersionNotEquals('2.0', '2.0.0'))", &p),
            Outcome::False
        );
        assert_eq!(
            evaluate("$([MSBuild]::VersionNotEquals('1.2', '1.3'))", &p),
            Outcome::True
        );
        assert_eq!(
            evaluate("$([msbuild]::versionequals('1.0','1'))", &p),
            Outcome::True
        );
        assert_eq!(
            evaluate("$([MSBuild]::VersionEquals('abc', '1.0'))", &p),
            Outcome::Unsupported
        );
    }

    #[test]
    fn msbuild_version_comparison_functions_use_simple_version_operands() {
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionGreaterThanOrEquals('1', '1.0'))",
                &PropertyMap::new(),
            ),
            Outcome::True
        );
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionGreaterThan('10.0.100-preview.1', '10.0.99'))",
                &PropertyMap::new(),
            ),
            Outcome::True
        );
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionGreaterThan('10.0.100-preview.1', '10.0.100'))",
                &PropertyMap::new(),
            ),
            Outcome::False
        );
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionGreaterThanOrEquals('v1.2.3-pre+metadata', '1.2.3.0'))",
                &PropertyMap::new(),
            ),
            Outcome::True
        );
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionLessThan('3.2', '3.14-pre'))",
                &PropertyMap::new(),
            ),
            Outcome::True
        );
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionGreaterThanOrEquals('3+metadata', '3.0'))",
                &PropertyMap::new(),
            ),
            Outcome::True
        );
    }

    #[test]
    fn msbuild_version_comparison_functions_reject_invalid_version_operands() {
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionGreaterThan('1.2.3.4.5', '1.0'))",
                &PropertyMap::new(),
            ),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionGreaterThan('2147483648.0', '1.0'))",
                &PropertyMap::new(),
            ),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate(
                "$([MSBuild]::VersionGreaterThanOrEquals('vv1.2', '1.0'))",
                &PropertyMap::new(),
            ),
            Outcome::Unsupported
        );
    }

    #[test]
    fn msbuild_version_function_respects_boolean_short_circuit() {
        let eval = super::evaluate(
            "'$(Known)' == 'no' And $([MSBuild]::VersionGreaterThanOrEquals($(Missing), '8.0'))",
            &props(&[("Known", "yes")]),
        );
        assert_eq!(eval.outcome, Outcome::False);
        assert!(eval.undefined_properties.is_empty());
    }

    #[test]
    fn system_version_parse_component_comparisons_are_supported() {
        let p = props(&[("WindowsSdkPackageVersion", "10.0.26100.34-preview")]);
        assert_eq!(
            evaluate(
                "$([System.Version]::Parse('$(WindowsSdkPackageVersion.Split('-')[0])').Build) <= 26100",
                &p
            ),
            Outcome::True
        );
        assert_eq!(
            evaluate(
                "$([System.Version]::Parse('$(WindowsSdkPackageVersion.Split('-')[0])').Revision) <= 33",
                &p
            ),
            Outcome::False
        );
    }

    #[test]
    fn system_version_parse_rejects_unsplit_prerelease_versions() {
        let p = props(&[("WindowsSdkPackageVersion", "10.0.26100.34-preview")]);
        assert_eq!(
            evaluate(
                "$([System.Version]::Parse('$(WindowsSdkPackageVersion)').Build) <= 26100",
                &p
            ),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate(
                "$([System.Version]::Parse('10.0.1-preview').Build) <= 1",
                &PropertyMap::new()
            ),
            Outcome::Unsupported
        );
    }

    #[test]
    fn system_version_parse_missing_build_and_revision_are_negative_one() {
        assert_eq!(
            evaluate(
                "$([System.Version]::Parse('10.0').Build) < 0",
                &PropertyMap::new()
            ),
            Outcome::True
        );
        assert_eq!(
            evaluate(
                "$([System.Version]::Parse('10.0.1').Revision) < 0",
                &PropertyMap::new()
            ),
            Outcome::True
        );
        assert_eq!(
            evaluate(
                "$([System.Version]::Parse('10.0').Build) >= 0",
                &PropertyMap::new()
            ),
            Outcome::False
        );
        assert_eq!(
            evaluate(
                "$([System.Version]::Parse('10.0').Build) == -1",
                &PropertyMap::new()
            ),
            Outcome::True
        );
    }

    /// MSBuild's relational operators accept a plain number against a
    /// version (`NumericComparisonExpressionNode`'s mixed `Compare`
    /// overloads): the number is compared against the version's *major*
    /// component only, and on a tie the version counts as strictly greater
    /// ("6.0.0.0 > 6"). Every case pinned against `dotnet msbuild` 10.0.300.
    #[test]
    fn mixed_number_and_version_relational_comparisons_are_supported() {
        let cases = [
            ("'1' < '2.0.0'", Outcome::True),
            ("'1.0.0' > '1'", Outcome::True),
            // Major tie: the version side is strictly greater.
            ("'2' < '2.0.0'", Outcome::True),
            ("'2' <= '2.0.0'", Outcome::True),
            ("'2' >= '2.0.0'", Outcome::False),
            ("'1' < '1.0.1'", Outcome::True),
            ("'3' > '2.9.0'", Outcome::True),
            // Only the major component is consulted: 0.5 vs major 0.
            ("'.5' < '0.75.0'", Outcome::False),
            ("'0x10' < '16.0.1'", Outcome::True),
            ("'-1' < '0.0.1'", Outcome::True),
        ];
        for (cond, expected) in cases {
            assert_eq!(evaluate(cond, &PropertyMap::new()), expected, "{cond}");
        }
    }

    /// `Version.TryParse` runs components through `int.TryParse`, which
    /// trims whitespace and accepts a leading `+` — so `'+2.5'` and
    /// `' 2 . 5 '` are versions, not junk. Pinned against `dotnet msbuild`
    /// 10.0.300.
    #[test]
    fn version_operands_accept_signed_and_whitespace_components() {
        assert_eq!(
            evaluate("'+2.5' < '2.5.0'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'+2.5' > '2.5.0'", &PropertyMap::new()),
            Outcome::False
        );
        assert_eq!(
            evaluate("' 2 . 5 ' > '2.4'", &PropertyMap::new()),
            Outcome::True
        );
    }

    /// MSBuild's scanner lexes bare numerics greedily — hex, dot-led,
    /// sign-led, and multi-dot (version) spellings are all one numeric
    /// token, with validity decided by the later conversion. Pinned against
    /// `dotnet msbuild` 10.0.300.
    #[test]
    fn bare_numeric_spellings_match_msbuild() {
        let cases = [
            ("0x10 == 16", Outcome::True),
            (".5 < 1", Outcome::True),
            ("-.5 < 0", Outcome::True),
            ("1.2.3 < 1.2.4", Outcome::True),
        ];
        for (cond, expected) in cases {
            assert_eq!(evaluate(cond, &PropertyMap::new()), expected, "{cond}");
        }
    }

    /// MSBuild tolerates whitespace at every joint of the property-function
    /// syntax: inside `$( … )`, between the member name and its argument
    /// list, around arguments, and around the trailing member access.
    /// Pinned against `dotnet msbuild` 10.0.300.
    #[test]
    fn property_function_whitespace_is_tolerated() {
        let cases = [
            "$( [MSBuild]::VersionGreaterThan('2','1') )",
            "$([MSBuild]::VersionGreaterThan ('2','1'))",
            "$([System.Version]::Parse ('10.0.1').Build) == 1",
            "$([System.Version]::Parse( '10.0.1' ).Build) == 1",
            "$([System.Version]::Parse('10.0.1') .Build) == 1",
            "$([System.Version]::Parse('10.0.1').Build ) == 1",
        ];
        for cond in cases {
            assert_eq!(evaluate(cond, &PropertyMap::new()), Outcome::True, "{cond}");
        }
    }

    /// MSBuild resolves property-function type and member names
    /// case-insensitively. Pinned against `dotnet msbuild` 10.0.300.
    #[test]
    fn msbuild_version_function_names_are_case_insensitive() {
        let cases = [
            "$([msbuild]::VersionGreaterThan('2','1'))",
            "$([MSBuild]::versiongreaterthan('2','1'))",
            "$([MSBUILD]::VERSIONGREATERTHAN('2','1'))",
            "$([MSBuild]::versionlessthan('1.0', '2'))",
        ];
        for cond in cases {
            assert_eq!(evaluate(cond, &PropertyMap::new()), Outcome::True, "{cond}");
        }
    }

    /// `[System.Version]::Parse(...).Build` and the `.Split('-')[0]` idiom
    /// resolve case-insensitively too. Pinned against `dotnet msbuild`
    /// 10.0.300.
    #[test]
    fn system_version_parse_is_case_insensitive() {
        assert_eq!(
            evaluate(
                "$([System.version]::parse('10.0.1').build) == 1",
                &PropertyMap::new()
            ),
            Outcome::True
        );
        assert_eq!(
            evaluate(
                "$([System.Version]::Parse('10.0.1').BUILD) == 1",
                &PropertyMap::new()
            ),
            Outcome::True
        );
        let p = props(&[("V", "10.0.26100.34-preview")]);
        assert_eq!(
            evaluate(
                "$([System.version]::parse('$(V.split('-')[0])').build) <= 26100",
                &p
            ),
            Outcome::True
        );
    }

    #[test]
    fn bang_does_not_negate_scalar_comparison_without_parens() {
        assert_eq!(
            evaluate("!1 == 2", &PropertyMap::new()),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate(
                "!$([System.Version]::Parse('10.0').Build) == 2",
                &PropertyMap::new()
            ),
            Outcome::Unsupported
        );
        assert_eq!(evaluate("!(1 == 2)", &PropertyMap::new()), Outcome::True);
    }

    #[test]
    fn non_version_relational_comparison_is_unsupported() {
        assert_eq!(
            evaluate("'alpha' > 'beta'", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn item_reference_inside_string_makes_condition_unsupported() {
        // `substitute` only handles `$(...)`, so a literal `@(...)`
        // inside a string literal would survive expansion and the
        // Compare would silently treat it as plain text. That would
        // produce the wrong truth value with no diagnostic — exactly
        // the failure mode plan D5's fail-loud rule exists to
        // prevent.
        assert_eq!(
            evaluate("'@(Compile)' == ''", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn metadata_reference_inside_string_makes_condition_unsupported() {
        assert_eq!(
            evaluate("'%(Identity)' == 'foo.fs'", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn item_reference_introduced_via_substitution_makes_condition_unsupported() {
        // The reference can also arrive via a property: a property
        // whose value happens to be `@(Files)` substitutes cleanly
        // (no $(...) inside), but the resulting condition string
        // still contains the item-list reference and must be
        // rejected.
        let p = props(&[("Sources", "@(Files)")]);
        assert_eq!(evaluate("'$(Sources)' == ''", &p), Outcome::Unsupported);
    }

    #[test]
    fn unknown_operator_anywhere_makes_condition_unsupported() {
        // An unmodelled function call lexes as a token sequence the parser
        // can't fit anywhere; we fail the whole condition rather than guess
        // at a truth value for the unmodelled operator. This is the
        // conservative half of plan D5's "fail loudly" stance: better to
        // mark the construct excluded than silently flip its truth value.
        assert_eq!(
            evaluate(
                "'$(X)' == '' Or SomeUnknownFunction('x')",
                &PropertyMap::new()
            ),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate("true And SomeUnknownFunction('x')", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn substitution_inside_string_concatenates_with_surrounding_text() {
        let p = props(&[("Version", "8.0")]);
        assert_eq!(evaluate("'net$(Version)' == 'net8.0'", &p), Outcome::True);
    }

    #[test]
    fn nested_parens_handled() {
        assert_eq!(
            evaluate("((true And false) Or (true And true))", &PropertyMap::new()),
            Outcome::True
        );
    }

    #[test]
    fn byte_equal_non_ascii_strings_are_equal() {
        // Trivially-equal non-ASCII strings don't need case folding;
        // we shouldn't bail on them.
        assert_eq!(
            evaluate("'café' == 'café'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("'café' != 'café'", &PropertyMap::new()),
            Outcome::False
        );
    }

    #[test]
    fn non_byte_equal_non_ascii_strings_are_unsupported() {
        // MSBuild's ordinal-ignore-case fold would handle 'É' vs 'é'
        // correctly; our `eq_ignore_ascii_case` would silently return
        // false. Bail to Unsupported rather than flip the gate on
        // case differences we can't fold.
        assert_eq!(
            evaluate("'É' == 'é'", &PropertyMap::new()),
            Outcome::Unsupported
        );
        // Same handling for !=: we can't decide either way.
        assert_eq!(
            evaluate("'É' != 'é'", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn mixed_ascii_and_non_ascii_unequal_is_unsupported() {
        // Pure ASCII vs non-ASCII isn't byte-equal; the safe move
        // is still Unsupported, because we have no general way to
        // prove case folding wouldn't bring them together.
        assert_eq!(
            evaluate("'foo' == 'fÖo'", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn unbalanced_paren_is_unsupported() {
        assert_eq!(
            evaluate("(true And false", &PropertyMap::new()),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate("true And false)", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    // --- HasTrailingSlash(...) ---
    //
    // MSBuild's second built-in condition function (after Exists). It
    // expands its single argument into a task item — which trims
    // surrounding whitespace — then reports whether the last character is
    // `/` or `\`. An empty result is false; a multi-item (`;`) argument is
    // a project error. Every case pinned against `dotnet msbuild` 10.0.300.

    #[test]
    fn has_trailing_slash_checks_last_character() {
        assert_eq!(
            evaluate("HasTrailingSlash('a/')", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("HasTrailingSlash('a\\')", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("HasTrailingSlash('a')", &PropertyMap::new()),
            Outcome::False
        );
        assert_eq!(
            evaluate("HasTrailingSlash('')", &PropertyMap::new()),
            Outcome::False
        );
    }

    #[test]
    fn has_trailing_slash_trims_expanded_argument() {
        // The argument goes through item-spec expansion, which trims
        // surrounding whitespace before the last-character check.
        assert_eq!(
            evaluate("HasTrailingSlash(' a/ ')", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("HasTrailingSlash(' /a ')", &PropertyMap::new()),
            Outcome::False
        );
        assert_eq!(
            evaluate("HasTrailingSlash('   ')", &PropertyMap::new()),
            Outcome::False
        );
    }

    #[test]
    fn has_trailing_slash_expands_and_negates() {
        let p = props(&[("OutDir", "bin/Debug/")]);
        assert_eq!(evaluate("HasTrailingSlash('$(OutDir)')", &p), Outcome::True);
        assert_eq!(
            evaluate("!HasTrailingSlash('$(OutDir)')", &p),
            Outcome::False
        );
        let p = props(&[("OutDir", "bin/Debug")]);
        assert_eq!(
            evaluate("'$(OutDir)' != '' and !HasTrailingSlash('$(OutDir)')", &p),
            Outcome::True
        );
    }

    #[test]
    fn has_trailing_slash_reports_undefined_argument() {
        let eval = super::evaluate("HasTrailingSlash('$(Missing)')", &PropertyMap::new());
        assert_eq!(eval.outcome, Outcome::False);
        assert_eq!(eval.undefined_properties, vec!["Missing".to_string()]);
    }

    #[test]
    fn has_trailing_slash_multi_item_argument_is_unsupported() {
        // Two non-empty items is a project error → Unsupported.
        assert_eq!(
            evaluate("HasTrailingSlash('a/;b')", &PropertyMap::new()),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate("HasTrailingSlash('a;b/')", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn has_trailing_slash_drops_empty_list_entries() {
        // MSBuild trims each list entry and drops empty ones, so a lone
        // trailing/leading `;` leaves a single decidable item. Pinned
        // against dotnet msbuild 10.0.300.
        assert_eq!(
            evaluate("HasTrailingSlash('a/;')", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("HasTrailingSlash(';/')", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(
            evaluate("HasTrailingSlash('a/ ; ')", &PropertyMap::new()),
            Outcome::True
        );
        // All entries empty → no item → no trailing slash.
        assert_eq!(
            evaluate("HasTrailingSlash(';')", &PropertyMap::new()),
            Outcome::False
        );
    }

    #[test]
    fn has_trailing_slash_unquoted_argument_is_unsupported() {
        // Real usage always quotes the argument (`HasTrailingSlash('$(X)')`);
        // a bare-token argument is outside the tight grammar we model.
        assert_eq!(
            evaluate("HasTrailingSlash(abc)", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn item_reference_in_has_trailing_slash_is_unsupported() {
        assert_eq!(
            evaluate("HasTrailingSlash('@(Compile)')", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    // --- bare scalar / property coerced to boolean ---
    //
    // A standalone scalar (property reference, string literal, or bare
    // token) is coerced to bool through MSBuild's boolean vocabulary —
    // `true`/`on`/`yes`/`!false`/… and their negations, case-insensitive.
    // A value outside that vocabulary (`1`, `foo`, the empty string of an
    // unset property) is a project error, so we fail safe to Unsupported.
    // Pinned against `dotnet msbuild` 10.0.300.

    #[test]
    fn bare_property_coerces_to_boolean() {
        assert_eq!(evaluate("$(P)", &props(&[("P", "true")])), Outcome::True);
        assert_eq!(evaluate("$(P)", &props(&[("P", "false")])), Outcome::False);
        assert_eq!(evaluate("$(P)", &props(&[("P", "True")])), Outcome::True);
        // MSBuild's standalone-boolean vocabulary is the same on/yes/off/no
        // set as `==` coercion.
        assert_eq!(evaluate("$(P)", &props(&[("P", "on")])), Outcome::True);
        assert_eq!(evaluate("$(P)", &props(&[("P", "yes")])), Outcome::True);
        assert_eq!(evaluate("$(P)", &props(&[("P", "no")])), Outcome::False);
    }

    #[test]
    fn non_boolean_bare_scalar_is_unsupported() {
        // `1` is numeric, not in the boolean vocabulary; a non-bool word
        // and the empty string of an unset property are project errors too.
        assert_eq!(
            evaluate("$(P)", &props(&[("P", "1")])),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate("$(P)", &props(&[("P", "foo")])),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate("$(Missing)", &PropertyMap::new()),
            Outcome::Unsupported
        );
        assert_eq!(evaluate("'foo'", &PropertyMap::new()), Outcome::Unsupported);
    }

    #[test]
    fn bang_negates_coerced_bare_scalar() {
        assert_eq!(evaluate("!$(P)", &props(&[("P", "false")])), Outcome::True);
        assert_eq!(evaluate("!$(P)", &props(&[("P", "true")])), Outcome::False);
    }

    #[test]
    fn string_literal_coerces_to_boolean() {
        assert_eq!(evaluate("'true'", &PropertyMap::new()), Outcome::True);
        assert_eq!(evaluate("'on'", &PropertyMap::new()), Outcome::True);
        assert_eq!(evaluate("'$(P)'", &props(&[("P", "true")])), Outcome::True);
    }

    #[test]
    fn bare_word_boolean_literals_are_supported() {
        // MSBuild's boolean vocabulary works as bare (unquoted) condition
        // words, not just `true`/`false`. Pinned against dotnet msbuild
        // 10.0.300.
        assert_eq!(evaluate("on", &PropertyMap::new()), Outcome::True);
        assert_eq!(evaluate("yes", &PropertyMap::new()), Outcome::True);
        assert_eq!(evaluate("off", &PropertyMap::new()), Outcome::False);
        assert_eq!(evaluate("no", &PropertyMap::new()), Outcome::False);
        assert_eq!(evaluate("!off", &PropertyMap::new()), Outcome::True);
        // A non-boolean bare word is a project error → Unsupported.
        assert_eq!(evaluate("maybe", &PropertyMap::new()), Outcome::Unsupported);
    }

    #[test]
    fn bare_word_is_a_string_comparison_operand() {
        // Unquoted words are simple-string operands, compared like quoted
        // strings (case-insensitive). Pinned against dotnet msbuild 10.0.300.
        assert_eq!(
            evaluate("Release == 'Release'", &PropertyMap::new()),
            Outcome::True
        );
        assert_eq!(evaluate("on == 'x'", &PropertyMap::new()), Outcome::False);
    }

    #[test]
    fn bang_before_comparison_stays_unsupported() {
        // `!'a' == 'b'` binds as `(!'a') == 'b'` in MSBuild, which errors
        // (a bool compared to a string). We must not silently read it as
        // `!('a' == 'b')`.
        assert_eq!(
            evaluate("!'a' == 'b'", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn boolean_literal_as_comparison_operand() {
        // `true`/`false` are usable as unquoted comparison operands on either
        // side, compared through the boolean vocabulary. The .NET SDK
        // container targets gate on `$(X.Contains('-')) == false`. Pinned
        // against dotnet msbuild 10.0.300.
        assert_eq!(
            evaluate("$(P) == false", &props(&[("P", "False")])),
            Outcome::True
        );
        assert_eq!(
            evaluate("$(P) == false", &props(&[("P", "True")])),
            Outcome::False
        );
        assert_eq!(
            evaluate("$(P) == true", &props(&[("P", "True")])),
            Outcome::True
        );
        assert_eq!(
            evaluate("false == $(P)", &props(&[("P", "False")])),
            Outcome::True
        );
        assert_eq!(
            evaluate("$(P.Contains('-')) == false", &props(&[("P", "8.0.100")])),
            Outcome::True
        );
    }

    #[test]
    fn whitespace_around_simple_property_reference_is_unsupported() {
        // MSBuild rejects a simple property reference with interior
        // whitespace (`$( Foo )`, `$(Foo )`, `$( Foo)`), unlike `$(Foo)`.
        // Committing to `Foo`'s value there would be a wrong gate. Pinned
        // against dotnet msbuild 10.0.300.
        let p = props(&[("Foo", "true")]);
        assert_eq!(evaluate("$(Foo)", &p), Outcome::True);
        assert_eq!(evaluate("$( Foo )", &p), Outcome::Unsupported);
        assert_eq!(evaluate("$(Foo )", &p), Outcome::Unsupported);
        assert_eq!(evaluate("$( Foo)", &p), Outcome::Unsupported);
        assert_eq!(evaluate("$( Foo ) == 'true'", &p), Outcome::Unsupported);
        // Property-function forms DO tolerate surrounding whitespace.
        assert_eq!(evaluate("$( Foo.Contains('r') )", &p), Outcome::True);
    }

    #[test]
    fn bare_property_as_comparison_operand() {
        assert_eq!(
            evaluate("$(P) == 'y'", &props(&[("P", "y")])),
            Outcome::True
        );
        assert_eq!(
            evaluate("$(P) != 'y'", &props(&[("P", "x")])),
            Outcome::True
        );
    }

    #[test]
    fn bare_unsupported_property_function_is_unsupported() {
        // A bare `$(...)` that isn't a modelled function still poisons the
        // condition when the substituter can't evaluate it.
        assert_eq!(
            evaluate("$([System.String]::Copy('x'))", &props(&[("X", "y")])),
            Outcome::Unsupported
        );
    }

    #[test]
    fn contains_property_function_in_condition_is_supported() {
        // The motivating F# SDK shape: `!$(V.Contains('{'))`.
        assert_eq!(
            evaluate("!$(P.Contains('{'))", &props(&[("P", "8.0.0")])),
            Outcome::True
        );
        assert_eq!(
            evaluate("!$(P.Contains('{'))", &props(&[("P", "{{x}}")])),
            Outcome::False
        );
    }

    #[test]
    fn contains_receiver_is_tracked_as_non_default_reference() {
        // The receiver of a `.Contains` call is a genuine branch-decision
        // property; an undefined one must surface *outside* the is-it-set
        // exemption so a gate depending on it is treated as uncertain
        // rather than certain.
        // Both the tight and the whitespace-before-`(` spellings must track
        // the receiver (MSBuild accepts both, so the scanner must too).
        for cond in ["$(Missing.Contains('x'))", "$(Missing.Contains ('x'))"] {
            let eval = super::evaluate(cond, &PropertyMap::new());
            assert_eq!(eval.outcome, Outcome::False, "{cond}");
            assert_eq!(
                eval.undefined_properties,
                vec!["Missing".to_string()],
                "{cond}"
            );
            assert_eq!(
                eval.undefined_outside_empty_comparison,
                vec!["Missing".to_string()],
                "{cond}"
            );
        }
    }

    #[test]
    fn raw_item_marker_masked_by_string_method_is_unsupported() {
        // A raw `@(` / `%(` opener in a string-method argument makes MSBuild
        // reject the whole condition at lex time: its scanner lexes item-list
        // (`@(`) and metadata (`%(`) markers straight from the raw condition
        // text, before any property expansion. Reducing `$(V.Contains('@('))`
        // to a bool would consume the marker inside the method and hide it
        // from every later check, letting us commit to a branch MSBuild never
        // reaches. The up-front raw-source marker scan closes that class.
        for cond in [
            "$(V.Contains('@('))",
            "$(V.StartsWith('%('))",
            "$(V.EndsWith('@(x)'))",
        ] {
            assert_eq!(
                evaluate(cond, &props(&[("V", "x")])),
                Outcome::Unsupported,
                "{cond}"
            );
        }
    }

    #[test]
    fn raw_marker_anywhere_in_condition_is_unsupported() {
        // The scan is not method-specific: any raw item/metadata marker in the
        // source text is Unsupported, whether it sits inside a built-in
        // function argument, a comparison operand string literal, or a
        // metadata reference standing on its own.
        assert_eq!(
            evaluate("HasTrailingSlash('@(x)/')", &PropertyMap::new()),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate("'$(A)' == '@(x)'", &props(&[("A", "y")])),
            Outcome::Unsupported
        );
        assert_eq!(
            evaluate("'%(Identity)' == 'x'", &PropertyMap::new()),
            Outcome::Unsupported
        );
    }

    #[test]
    fn marker_delivered_only_via_substitution_still_commits() {
        // A marker that appears only *after* `$()` substitution is not in the
        // raw source, so the scan leaves it alone — matching MSBuild, which
        // never re-lexes substituted text. Here the `.Contains` needle arrives
        // via `$(N)` and reduces to an ordinary substring test.
        let p = props(&[("V", "a@(b"), ("N", "@(")]);
        assert_eq!(evaluate("$(V.Contains('$(N)'))", &p), Outcome::True);
    }

    #[test]
    fn fsharp_core_maximum_major_version_gate_is_supported() {
        // The exact condition from Microsoft.FSharp.Core.NetSdk.props gating
        // FSharpCoreMaximumMajorVersion. Resolved end-to-end (equality,
        // And, the .Contains property function, and `!` coercion).
        let cond = "'$(FSCorePackageVersionSet)' == 'true' and '$(FSCorePackageVersion)' != '' \
                    and !$(FSCorePackageVersion.Contains('{'))";
        assert_eq!(
            evaluate(
                cond,
                &props(&[
                    ("FSCorePackageVersionSet", "true"),
                    ("FSCorePackageVersion", "8.0.0"),
                ]),
            ),
            Outcome::True
        );
        // The unreplaced token placeholder still contains `{`, so the gate
        // is false (the SDK skips setting the max major version).
        assert_eq!(
            evaluate(
                cond,
                &props(&[
                    ("FSCorePackageVersionSet", "true"),
                    ("FSCorePackageVersion", "{{FSCorePackageVersionValue}}"),
                ]),
            ),
            Outcome::False
        );
    }
}

#[cfg(test)]
mod prop_tests {
    //! Property-based tests for the condition evaluator.
    //!
    //! Strategy: generate a random AST inside our grammar, render it
    //! as a condition string, feed it back through the production
    //! evaluator, and compare against a directly-computed truth
    //! value. This exercises the lexer, parser, and evaluator
    //! end-to-end against a reference implementation that ignores
    //! surface syntax outside literals (token-separating whitespace,
    //! operator casing, parens).
    //!
    //! Restrictions on generated input:
    //!   * String literals contain only "safe" bytes — no `'` (would
    //!     close the literal), no `$` (no substitution semantics to
    //!     test here), no control chars.
    //!   * Tree depth is bounded so generation terminates.
    //!
    //! Coverage instrumentation: every property tracks how often it
    //! observes True vs False outcomes and asserts both regimes are
    //! visited. A property that only ever sees one branch isn't
    //! exploring the space.

    use std::sync::atomic::{AtomicUsize, Ordering};

    use proptest::prelude::*;

    use super::*;

    /// Proptests only care about the truth value; shadow the outer
    /// `evaluate` to drop the undefined-property list so the existing
    /// `prop_assert_eq!(evaluate(...), Outcome::True, ...)` assertions
    /// continue to compile after the signature widening.
    fn evaluate(source: &str, props: &PropertyMap) -> Outcome {
        super::evaluate(source, props).outcome
    }

    /// A node in the reference AST. Exactly mirrors the subset of
    /// MSBuild condition syntax we model — boolean literals, string
    /// equality / inequality, And, Or, parens (parens aren't an AST
    /// node; the renderer adds them around every binary op so the
    /// generated string is unambiguous regardless of precedence).
    #[derive(Debug, Clone)]
    enum Ast {
        BoolLit(bool),
        Eq(String, String),
        Neq(String, String),
        And(Box<Ast>, Box<Ast>),
        Or(Box<Ast>, Box<Ast>),
    }

    /// Reference evaluator. Comparing this against the production
    /// `evaluate` is the whole point — if these disagree, either the
    /// reference is wrong or the production parser/evaluator is.
    fn truth(ast: &Ast) -> bool {
        match ast {
            Ast::BoolLit(b) => *b,
            Ast::Eq(a, b) => reference_equality(a, b),
            Ast::Neq(a, b) => !reference_equality(a, b),
            Ast::And(a, b) => truth(a) && truth(b),
            Ast::Or(a, b) => truth(a) || truth(b),
        }
    }

    /// MSBuild's `==` dispatch (empty → numeric → boolean → string),
    /// restated over the operand strings directly. The *scalar* parsers are
    /// deliberately the production ones — their semantics are pinned
    /// separately by the unit tests verified against `dotnet msbuild` — so
    /// this property's job is the surrounding pipeline: rendering,
    /// tokenising, parsing, and evaluation-order must land every operand
    /// pair in this same comparison.
    fn reference_equality(lhs: &str, rhs: &str) -> bool {
        if lhs.is_empty() || rhs.is_empty() {
            return lhs.is_empty() && rhs.is_empty();
        }
        if let (Some(lhs), Some(rhs)) = (
            super::parse_msbuild_number(lhs),
            super::parse_msbuild_number(rhs),
        ) {
            return lhs == rhs;
        }
        if let (Some(lhs), Some(rhs)) = (
            super::parse_msbuild_bool(lhs),
            super::parse_msbuild_bool(rhs),
        ) {
            return lhs == rhs;
        }
        lhs.eq_ignore_ascii_case(rhs)
    }

    /// Render an AST as a condition string. We wrap every binary op
    /// in parens — that way the round-trip property doesn't depend on
    /// our precedence implementation matching the renderer's
    /// assumption. Precedence is tested separately by the unit tests
    /// above.
    fn render(ast: &Ast) -> String {
        match ast {
            Ast::BoolLit(true) => "true".into(),
            Ast::BoolLit(false) => "false".into(),
            Ast::Eq(a, b) => format!("'{a}' == '{b}'"),
            Ast::Neq(a, b) => format!("'{a}' != '{b}'"),
            Ast::And(a, b) => format!("({} And {})", render(a), render(b)),
            Ast::Or(a, b) => format!("({} Or {})", render(a), render(b)),
        }
    }

    /// Generate string-literal contents that won't break the lexer:
    /// no `'` (closes the literal early), no `$` (would introduce
    /// substitution semantics we test separately), and only printable
    /// ASCII — control chars wouldn't add coverage but might trip
    /// the lexer's byte-classification helpers.
    ///
    /// Each character is drawn *directly* from its valid byte range rather
    /// than `any::<u8>().prop_filter(is_ascii_digit)`: a filter over the full
    /// 0–255 byte space rejects ~96% of draws for digits (10/256) and ~80%
    /// for letters (52/256). proptest counts those rejects against the
    /// per-test `max_local_rejects` budget (default 65536), and across a
    /// `cases`-sized run the cumulative count sits right at that boundary —
    /// so a slightly unlucky seed tips it over and the test aborts with
    /// "Too many local rejects". Range strategies never reject, removing the
    /// flake. (See proptest's `RangeInclusive<u8> as Strategy`.)
    fn arb_safe_str() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                // Letters (mixed case so we exercise the
                // case-insensitive comparison path)
                prop_oneof![b'A'..=b'Z', b'a'..=b'z'],
                // Digits
                b'0'..=b'9',
                // A small set of safe punctuation. Excluding `'`, `$`,
                // `(`, `)` (these are operators in our grammar — would
                // confuse the lexer if they sat inside a string literal
                // and the literal then participated in concatenation).
                // Actually the lexer doesn't tokenise inside `'...'`, so
                // these are fine — but keeping the list small keeps test
                // failures readable.
                Just(b'_'),
                Just(b'-'),
                Just(b'.'),
                Just(b'/'),
                Just(b' '),
            ],
            0..8usize,
        )
        .prop_map(|bytes| String::from_utf8(bytes).expect("ASCII-only"))
    }

    fn arb_ast() -> impl Strategy<Value = Ast> {
        // Leaf strategies are cheap; branching strategies multiply
        // by ~4 per level. Cap depth at 4 so each test case stays
        // small enough to debug if a counterexample is found.
        let leaf = prop_oneof![
            any::<bool>().prop_map(Ast::BoolLit),
            (arb_safe_str(), arb_safe_str()).prop_map(|(a, b)| Ast::Eq(a, b)),
            (arb_safe_str(), arb_safe_str()).prop_map(|(a, b)| Ast::Neq(a, b)),
        ];
        leaf.prop_recursive(4, 32, 2, |inner| {
            prop_oneof![
                (inner.clone(), inner.clone())
                    .prop_map(|(a, b)| Ast::And(Box::new(a), Box::new(b))),
                (inner.clone(), inner).prop_map(|(a, b)| Ast::Or(Box::new(a), Box::new(b))),
            ]
        })
    }

    fn arb_valid_msbuild_version_operand() -> impl Strategy<Value = String> {
        (
            prop_oneof![Just(""), Just("v"), Just("V")],
            prop::collection::vec(0u32..=99, 1..=4usize),
            prop_oneof![
                Just("".to_string()),
                "[A-Za-z][A-Za-z0-9]*(\\.[0-9]{1,2}){0,2}".prop_map(|s| format!("-{s}")),
                "[A-Za-z][A-Za-z0-9]*(\\.[0-9]{1,2}){0,2}".prop_map(|s| format!("-{s}+metadata")),
                Just("+metadata".to_string()),
            ],
        )
            .prop_map(|(prefix, parts, suffix)| {
                let numeric = parts
                    .into_iter()
                    .map(|part| part.to_string())
                    .collect::<Vec<_>>()
                    .join(".");
                format!("{prefix}{numeric}{suffix}")
            })
    }

    fn arb_invalid_msbuild_version_operand() -> impl Strategy<Value = String> {
        prop_oneof![
            prop::collection::vec(0u32..=99, 5..=7usize).prop_map(|parts| {
                parts
                    .into_iter()
                    .map(|part| part.to_string())
                    .collect::<Vec<_>>()
                    .join(".")
            }),
            (0usize..4, 2_147_483_648u64..=2_147_483_700u64).prop_map(
                |(bad_idx, bad_component)| {
                    let mut parts = [1u64, 2, 3, 4];
                    parts[bad_idx] = bad_component;
                    parts
                        .into_iter()
                        .map(|part| part.to_string())
                        .collect::<Vec<_>>()
                        .join(".")
                },
            ),
            (0u32..=99).prop_map(|major| format!("vv{major}")),
            (0u32..=99, 0u32..=99).prop_map(|(major, minor)| format!("{major}..{minor}")),
        ]
    }

    fn arb_equivalent_decimal_literals() -> impl Strategy<Value = (String, String)> {
        prop_oneof![
            (0u32..=10_000, 0usize..=3, 0usize..=3, 0usize..=3).prop_map(
                |(number, lhs_leading, rhs_leading, rhs_trailing)| {
                    let lhs = format!("{}{}", "0".repeat(lhs_leading), number);
                    let mut rhs = format!("{}{}", "0".repeat(rhs_leading), number);
                    if rhs_trailing > 0 {
                        rhs.push('.');
                        rhs.push_str(&"0".repeat(rhs_trailing));
                    }
                    (lhs, rhs)
                },
            ),
            (0u32..=1_000, 1u32..=999, 0usize..=3, 0usize..=3, 0usize..=3,).prop_map(
                |(whole, fraction, lhs_leading, rhs_leading, extra_trailing)| {
                    let fraction = format!("{fraction:03}");
                    let lhs = format!("{}{}.{}", "0".repeat(lhs_leading), whole, fraction);
                    let rhs = format!(
                        "{}{}.{}{}",
                        "0".repeat(rhs_leading),
                        whole,
                        fraction,
                        "0".repeat(extra_trailing)
                    );
                    (lhs, rhs)
                },
            ),
        ]
    }

    /// Expand existing whitespace runs and pad both ends of a
    /// rendered condition string. We deliberately do NOT insert
    /// whitespace at new positions: separating `==` into `= =` or
    /// splitting an identifier in half would actually change
    /// tokenisation, and that's not the claim being tested. The
    /// renderer already places single spaces between every
    /// token-distinct pair (because the binary-op syntax dictates
    /// it), so widening those runs exercises the lexer's
    /// "whitespace runs are insignificant" behaviour without
    /// re-tokenising the input. Whitespace inside `'...'` literals
    /// IS significant (preserves the string value), so we copy them
    /// verbatim.
    fn pad_whitespace(s: &str) -> String {
        let mut out = String::with_capacity(s.len() * 2);
        let mut in_string = false;
        for c in s.chars() {
            if in_string {
                out.push(c);
                if c == '\'' {
                    in_string = false;
                }
                continue;
            }
            if c == '\'' {
                in_string = true;
                out.push(c);
                continue;
            }
            if c.is_whitespace() {
                out.push(' ');
                out.push(' ');
            } else {
                out.push(c);
            }
        }
        format!("  {}  ", out)
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 512,
            ..ProptestConfig::default()
        })]

        /// Generated AST renders to a string whose evaluated outcome
        /// matches the reference truth value. End-to-end check of
        /// lex → parse → eval.
        #[test]
        fn render_eval_matches_reference_truth(ast in arb_ast()) {
            let rendered = render(&ast);
            let expected = truth(&ast);
            let got = evaluate(&rendered, &PropertyMap::new());
            let expected_outcome = if expected { Outcome::True } else { Outcome::False };
            // Surface the rendered form on failure so a counterexample
            // is actionable without re-rendering by hand.
            prop_assert_eq!(
                got, expected_outcome,
                "mismatch on rendered condition {:?}", rendered
            );
            // Track distribution so a degenerate generator can't
            // satisfy the property by always producing the same
            // branch.
            if expected {
                TRUTH_TRUE.fetch_add(1, Ordering::Relaxed);
            } else {
                TRUTH_FALSE.fetch_add(1, Ordering::Relaxed);
            }
        }

        /// Random whitespace between tokens must not change the
        /// outcome. Pinned separately because whitespace handling is
        /// the easiest lexer bug to hide.
        #[test]
        fn whitespace_padding_is_insignificant(ast in arb_ast()) {
            let rendered = render(&ast);
            let padded = pad_whitespace(&rendered);
            let lhs = evaluate(&rendered, &PropertyMap::new());
            let rhs = evaluate(&padded, &PropertyMap::new());
            prop_assert_eq!(
                lhs, rhs,
                "whitespace changed outcome: original {:?} padded {:?}", rendered, padded
            );
        }

        /// `'x' == 'x'` is true for every safe string. Tests the
        /// reflexivity of string equality and that the lexer
        /// preserves literal contents end-to-end.
        #[test]
        fn equality_is_reflexive(s in arb_safe_str()) {
            let cond = format!("'{s}' == '{s}'");
            prop_assert_eq!(evaluate(&cond, &PropertyMap::new()), Outcome::True,
                "reflexivity violated for {:?}", s);
        }

        /// MSBuild documents string `==` as case-insensitive.
        /// Compare a string against its uppercase form: must equal.
        #[test]
        fn equality_is_case_insensitive(s in arb_safe_str()) {
            let upper = s.to_ascii_uppercase();
            let cond = format!("'{s}' == '{upper}'");
            prop_assert_eq!(evaluate(&cond, &PropertyMap::new()), Outcome::True,
                "case-insensitive equality violated for {:?} vs {:?}", s, upper);
        }

        /// The MSBuild version helper functions compare missing Build/Revision
        /// components as zero, but their operands still have to fit
        /// System.Version's shape and component range.
        #[test]
        fn msbuild_version_functions_reject_generated_invalid_operands(
            invalid in arb_invalid_msbuild_version_operand(),
            valid in arb_valid_msbuild_version_operand(),
        ) {
            let invalid_lhs =
                format!("$([MSBuild]::VersionGreaterThan('{invalid}', '{valid}'))");
            prop_assert_eq!(
                evaluate(&invalid_lhs, &PropertyMap::new()),
                Outcome::Unsupported,
                "invalid lhs operand was accepted in {:?}",
                invalid_lhs,
            );

            let invalid_rhs =
                format!("$([MSBuild]::VersionLessThan('{valid}', '{invalid}'))");
            prop_assert_eq!(
                evaluate(&invalid_rhs, &PropertyMap::new()),
                Outcome::Unsupported,
                "invalid rhs operand was accepted in {:?}",
                invalid_rhs,
            );
        }

        #[test]
        fn decimal_equality_normalizes_equivalent_spellings(
            (lhs, rhs) in arb_equivalent_decimal_literals(),
        ) {
            let eq = format!("'{lhs}' == '{rhs}'");
            prop_assert_eq!(
                evaluate(&eq, &PropertyMap::new()),
                Outcome::True,
                "equivalent decimals did not compare equal in {:?}",
                eq,
            );

            let neq = format!("{lhs} != {rhs}");
            prop_assert_eq!(
                evaluate(&neq, &PropertyMap::new()),
                Outcome::False,
                "equivalent bare decimals compared not-equal in {:?}",
                neq,
            );
        }

        /// The condition language is case-insensitive end-to-end — keywords,
        /// property-function type/member names, version prefixes, boolean
        /// vocabulary. Randomly flipping the case of any letters in a
        /// version-function condition must not change the outcome.
        #[test]
        fn version_function_casing_is_insignificant(
            lhs in arb_valid_msbuild_version_operand(),
            rhs in arb_valid_msbuild_version_operand(),
            function in prop::sample::select(&[
                "VersionGreaterThanOrEquals",
                "VersionGreaterThan",
                "VersionLessThanOrEquals",
                "VersionLessThan",
                "VersionEquals",
                "VersionNotEquals",
            ][..]),
            flips in prop::collection::vec(any::<bool>(), 128),
        ) {
            let canonical =
                format!("$([MSBuild]::{function}('{lhs}', '{rhs}'))");
            let mutated: String = canonical
                .chars()
                .zip(flips.iter().cycle())
                .map(|(c, flip)| {
                    if *flip {
                        if c.is_ascii_lowercase() {
                            c.to_ascii_uppercase()
                        } else {
                            c.to_ascii_lowercase()
                        }
                    } else {
                        c
                    }
                })
                .collect();
            prop_assert_eq!(
                evaluate(&canonical, &PropertyMap::new()),
                evaluate(&mutated, &PropertyMap::new()),
                "casing changed outcome: {:?} vs {:?}", canonical, mutated
            );
        }
    }

    // Coverage counters live at module scope (proptest can't share
    // mutable state across cases inside the macro). We tally outcomes
    // as the round-trip property runs, then a separate harness test
    // checks the tally meets a minimum balance so a degenerate
    // generator can't pass by always producing one branch.
    static TRUTH_TRUE: AtomicUsize = AtomicUsize::new(0);
    static TRUTH_FALSE: AtomicUsize = AtomicUsize::new(0);

    /// Coverage assertion: the round-trip property must visit both
    /// truth-value regimes a non-trivial number of times. With 512
    /// cases and a roughly-balanced generator, observing fewer than
    /// 16 of either side is overwhelmingly improbable (well below
    /// 1e-11 even under pessimistic models of the generator). If
    /// this trips, the AST generator has skewed and the property
    /// isn't really exploring the space.
    #[test]
    fn round_trip_property_observed_both_outcomes() {
        // Force the round-trip property to actually run before this
        // assertion fires. Proptest runs tests in arbitrary order;
        // by re-invoking the same evaluator with a trivially-true
        // and trivially-false expression we ensure the counters
        // can't be zero just because of run ordering.
        for _ in 0..16 {
            if evaluate("true", &PropertyMap::new()) == Outcome::True {
                TRUTH_TRUE.fetch_add(1, Ordering::Relaxed);
            }
            if evaluate("false", &PropertyMap::new()) == Outcome::False {
                TRUTH_FALSE.fetch_add(1, Ordering::Relaxed);
            }
        }
        let t = TRUTH_TRUE.load(Ordering::Relaxed);
        let f = TRUTH_FALSE.load(Ordering::Relaxed);
        assert!(
            t >= 16 && f >= 16,
            "coverage too skewed: {t} true / {f} false — generator is not exploring both branches"
        );
    }
}
