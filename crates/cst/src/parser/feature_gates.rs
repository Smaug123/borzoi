//! The language-version gates FCS applies while parsing: each construct a
//! [`LanguageFeature`] introduced draws a diagnostic when parsed at a version
//! below that feature's.
//!
//! Most are checks FCS makes with `CheckLanguageFeatureAndRecover` before
//! parsing the construct anyway, so its tree is the same at every version and
//! only the diagnostics move. That is how every gate is modelled here: a walk
//! over the finished tree, which never alters it. Four gate a feature whose
//! tree FCS does change below its version, and here keep the newer tree: the
//! dotless `float32` literal, which FCS's lexer replaces with `0.0f`; the
//! nullness annotation, whose `|` FCS's lex-filter does not relabel; and the
//! `_.` pattern and the `for _ =` loop, where FCS raises a parse error and
//! recovers to a different tree. Which is which is the feature table's
//! `fcs_effect`, and each diagnostic's
//! [`DiagnosticClass`](super::DiagnosticClass) is read off it
//! ([`ParseDiagnostic::gate`]): `Legality` for the first kind, `Recovery` for
//! the second.
//!
//! Each gate reports at the range FCS's check is given, which is not always the
//! construct's: a `while!` reports its keyword, a `module [<A>] M` the token
//! after the attributes (FCS reports at the empty `opt_rec`, whose range is the
//! next token's). `parser_diff_feature_gates` holds every span, severity and
//! message to FCS at every version.
//!
//! Two of the features are checked by FCS's type checker, not its parser:
//! interpolated strings ([`LanguageFeature::StringInterpolation`]) and `^`
//! index arguments ([`LanguageFeature::FromEndSlicing`]). Their gates report
//! here what the checker reports, so that a node a version's facade leaves out
//! lies under a diagnostic (`docs/ast-versioning-plan.md`, D9), and
//! `parser_diff_feature_gates` holds them to FCS's check diagnostics.
//!
//! The features gated here are the feature table's live `Kind` rows, through
//! [`kind_feature`], and its live `Positional` rows whose detector is a function
//! below (`crates/cst/language-features.tsv`; the lex-filter's `Positional`
//! rows are gated there).

use std::ops::Range;

use crate::language_version::{LanguageFeature, LanguageVersion};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, kind_feature};

use super::ParseDiagnostic;

/// FCS's FS1153 (`lexInvalidFloat`), the lexer's error for a dotless `float32`
/// literal below [`LanguageFeature::DotlessFloat32Literal`].
const INVALID_FLOAT: &str = "Invalid floating point number";

/// FCS's FS0010 (`parsUnexpectedSymbolDot`), raised at the `.` of a pattern
/// long identifier rooted at `_` below
/// [`LanguageFeature::SingleUnderscorePattern`], whatever the pattern's context.
const UNEXPECTED_SYMBOL_DOT: &str =
    "Unexpected symbol '.' in member definition. Expected 'with', '=' or other token.";

/// FCS's FS0525 (`parsIntegerForLoopRequiresSimpleIdentifier`), raised at the
/// `_` of `for _ = …` below [`LanguageFeature::WildCardInForLoop`].
const FOR_LOOP_REQUIRES_SIMPLE_IDENTIFIER: &str =
    "An integer for loop must use a simple identifier";

/// FCS's FS3303 (`fromEndSlicingRequiresVFive`): the type checker's error for a
/// `^` index argument below [`LanguageFeature::FromEndSlicing`], which it
/// reports in place of FS3350.
const FROM_END_SLICING: &str =
    "The 'from the end slicing' feature requires language version 'preview'.";

/// FCS's FS3534 (`tcTraitInvocationShouldUseTick`): its checker's error for a
/// `^e` that is not an index argument, at every version.
const STATIC_CONSTRAINT_INVOCATION: &str = "Invocation of a static constraint should use \"'T.Ident\" \
     and not \"^T.Ident\", even for statically resolved type parameters.";

/// The errors the gates report for the tree `root` parsed at `lang`.
pub(super) fn feature_gate_diagnostics(
    root: &SyntaxNode,
    lang: LanguageVersion,
) -> Vec<ParseDiagnostic> {
    let mut out = Vec::new();
    // A parse at preview (an orphan buffer) has every feature gated here, so
    // skip the walk. `POSITIONAL_GATES` is every live `Kind` or `Positional`
    // row of the feature table, a superset of what this walk gates; a
    // numbered version lacks `FromEndSlicing`, so its parse always walks.
    if LanguageFeature::POSITIONAL_GATES
        .iter()
        .all(|&f| lang.supports(f))
    {
        return out;
    }
    let mut gate = |feature: LanguageFeature, span: Range<usize>| {
        if !lang.supports(feature) {
            out.push(ParseDiagnostic::gate(
                feature,
                gate_message(feature, lang),
                span,
            ));
        }
    };
    // `^e` below preview where it is not an index argument.
    let mut static_constraint_invocations = Vec::new();
    for node in root.descendants() {
        if node.kind() == SyntaxKind::INDEX_FROM_END_EXPR && !is_index_argument(&node, lang) {
            static_constraint_invocations.extend(significant_range(&node));
        } else if let Some(feature) = kind_feature(node.kind())
            && let Some(span) = kind_gate_span(&node)
        {
            gate(feature, span);
        }
        match node.kind() {
            SyntaxKind::LONG_IDENT_PAT => {
                if let Some(span) = underscore_dot_pattern_span(&node) {
                    gate(LanguageFeature::SingleUnderscorePattern, span);
                }
            }
            SyntaxKind::FOR_EXPR => {
                if let Some(span) = wildcard_for_loop_span(&node) {
                    gate(LanguageFeature::WildCardInForLoop, span);
                }
            }
            SyntaxKind::TYPAR_DECL => {
                if let Some(span) = typar_decl_intersection_span(&node) {
                    gate(LanguageFeature::ConstraintIntersectionOnFlexibleTypes, span);
                }
            }
            SyntaxKind::MODULE_OR_NAMESPACE | SyntaxKind::NESTED_MODULE_DECL => {
                if let Some(span) = module_attributes_span(&node) {
                    gate(LanguageFeature::AttributesToRightOfModuleKeyword, span);
                }
            }
            SyntaxKind::BINDING => {
                if let Some(span) = typed_bang_binding_span(&node) {
                    gate(LanguageFeature::AllowTypedLetUseAndBang, span);
                }
            }
            SyntaxKind::YIELD_OR_RETURN_EXPR | SyntaxKind::YIELD_OR_RETURN_FROM_EXPR => {
                if let Some(span) = typed_yield_or_return_span(&node) {
                    gate(LanguageFeature::AllowTypedLetUseAndBang, span);
                }
            }
            _ => {}
        }
        // From 7.0 on, FCS warns on every `static abstract` instead (FS3535,
        // `tcUsingInterfacesWithStaticAbstractMethods`). That warning is not
        // reported: code declaring such interfaces routinely silences it with
        // `#nowarn "3535"`, which we do not consume, so reporting it would
        // flag files the build does not.
        if let Some(span) = static_abstract_span(&node) {
            gate(LanguageFeature::InterfacesWithAbstractStaticMembers, span);
        }
    }
    // FCS's checker rejects such a `^e` at every version (FS3534) and consults
    // the feature only for an index argument; below preview the error is
    // reported here, as the feature's gate, so that the node, which the
    // facades below preview leave out, lies under a diagnostic naming it.
    let from_end = LanguageFeature::FromEndSlicing;
    if !lang.supports(from_end) {
        out.extend(static_constraint_invocations.into_iter().map(|span| {
            ParseDiagnostic::gate(from_end, STATIC_CONSTRAINT_INVOCATION.to_string(), span)
        }));
    }
    if !lang.supports(LanguageFeature::DotlessFloat32Literal) {
        for token in root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            if let Some(span) = dotless_float32_span(&token) {
                out.push(ParseDiagnostic::gate(
                    LanguageFeature::DotlessFloat32Literal,
                    INVALID_FLOAT.to_string(),
                    span,
                ));
            }
        }
    }
    out
}

/// The message FCS reports a gate on `feature` with at `lang`: FS3350, unless
/// FCS has an error of its own for the feature.
fn gate_message(feature: LanguageFeature, lang: LanguageVersion) -> String {
    match feature {
        LanguageFeature::SingleUnderscorePattern => UNEXPECTED_SYMBOL_DOT.to_string(),
        LanguageFeature::WildCardInForLoop => FOR_LOOP_REQUIRES_SIMPLE_IDENTIFIER.to_string(),
        LanguageFeature::FromEndSlicing => FROM_END_SLICING.to_string(),
        _ => feature.not_available_message(lang),
    }
}

fn token_span(token: &SyntaxToken) -> Range<usize> {
    let range = token.text_range();
    usize::from(range.start())..usize::from(range.end())
}

/// A token that stands for source text: not trivia, and not one of the
/// zero-width `ERROR` placeholders the parser leaves for consumed virtuals.
fn is_significant(token: &SyntaxToken) -> bool {
    !token.kind().is_trivia() && !token.text().is_empty()
}

/// The range of `node`'s source text, from its first significant token to its
/// last — a node's own range can open on trivia the parser attached to it.
fn significant_range(node: &SyntaxNode) -> Option<Range<usize>> {
    let mut tokens = node
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(is_significant);
    let first = tokens.next()?;
    let last = tokens.last().unwrap_or_else(|| first.clone());
    Some(token_span(&first).start..token_span(&last).end)
}

/// `node`'s direct children that stand for source text, in order: child nodes
/// with significant text, and significant tokens.
fn significant_children(node: &SyntaxNode) -> impl Iterator<Item = SyntaxElement> {
    node.children_with_tokens().filter(|el| match el {
        SyntaxElement::Token(t) => is_significant(t),
        SyntaxElement::Node(n) => significant_range(n).is_some(),
    })
}

/// Where FCS reports a [`kind_feature`] node: a `while!` at its keyword, a
/// `_.M` at its `_.`, an intersection type, a nullness annotation, an
/// interpolated string and an index argument `^e` at the whole node.
fn kind_gate_span(node: &SyntaxNode) -> Option<Range<usize>> {
    match node.kind() {
        SyntaxKind::WHILE_BANG_EXPR => node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|t| t.kind() == SyntaxKind::WHILE_BANG_TOK)
            .map(|t| token_span(&t)),
        SyntaxKind::WITH_NULL_TYPE => {
            let range = node.text_range();
            Some(usize::from(range.start())..usize::from(range.end()))
        }
        // `pars.fsy`'s `UNDERSCORE DOT atomicExpr` checks the union of the
        // two tokens' ranges.
        SyntaxKind::DOT_LAMBDA_EXPR => {
            let mut tokens = node
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .filter(is_significant);
            let underscore = tokens.next()?;
            let dot = tokens.find(|t| t.kind() == SyntaxKind::DOT_TOK)?;
            Some(token_span(&underscore).start..token_span(&dot).end)
        }
        _ => significant_range(node),
    }
}

/// Whether the `^e` node `node` is an index argument at `lang`, the one place
/// FCS's checker consults [`LanguageFeature::FromEndSlicing`]
/// (`CheckExpressions.fs`'s `DecodeIndexArg`): the argument of `e.[…]`, or of
/// `e[…]` written with no space before the `[`, after splitting a tuple of
/// arguments and taking either bound of a range. Anywhere else FCS's checker
/// rejects `^e` at every version (FS3534), which the gate reports instead below
/// preview.
///
/// FCS's checker reads `e[…]` as indexing only from
/// [`LanguageFeature::IndexerNotationWithoutDot`] (6.0), and then only when
/// `e` is not a function; below it, and for a function, it is an application.
/// The version is read here; the type, which a parse cannot know, is not, so a
/// function applied to `[^e]` with no space draws FS3303 here where FCS's
/// checker reports FS3534.
fn is_index_argument(node: &SyntaxNode, lang: LanguageVersion) -> bool {
    let mut arg = node.clone();
    let mut parent = node.parent();
    for wrapper in [SyntaxKind::INDEX_RANGE_EXPR, SyntaxKind::TUPLE_EXPR] {
        if let Some(p) = parent.clone().filter(|p| p.kind() == wrapper) {
            arg = p;
            parent = arg.parent();
        }
    }
    let Some(parent) = parent else {
        return false;
    };
    match parent.kind() {
        // The receiver is the first child, the argument the second.
        SyntaxKind::DOT_INDEXED_GET_EXPR => parent.children().nth(1).as_ref() == Some(&arg),
        // `IndexerNotationWithoutDot` gates nothing of its own (its row is
        // checker-only), so its version is read as this gate's condition
        // rather than through `supports`.
        SyntaxKind::ARRAY_OR_LIST_EXPR
            if lang >= LanguageFeature::IndexerNotationWithoutDot.introduced() =>
        {
            let Some(app) = parent.parent().filter(|a| a.kind() == SyntaxKind::APP_EXPR) else {
                return false;
            };
            let mut children = app.children();
            let (Some(receiver), Some(list)) = (children.next(), children.next()) else {
                return false;
            };
            // A list, `[…]`: an array, `[|…|]`, is applied at every version.
            let opener = parent
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(is_significant);
            list == parent
                && opener.is_some_and(|t| t.kind() == SyntaxKind::LBRACK_TOK)
                && parent.children().count() == 1
                && significant_range(&receiver)
                    .zip(significant_range(&list))
                    .is_some_and(|(r, l)| r.end == l.start)
        }
        _ => false,
    }
}

/// `_.M` in a pattern (`member _.M`, `| _.Case`): FCS's
/// `atomicPatternLongIdent` raises its parse error at the `.` (`pars.fsy`).
fn underscore_dot_pattern_span(node: &SyntaxNode) -> Option<Range<usize>> {
    let long_ident = node
        .children()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT)?;
    let mut tokens = long_ident
        .children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(is_significant);
    let first = tokens.next()?;
    let dot = tokens.next()?;
    (first.kind() == SyntaxKind::IDENT_TOK
        && first.text() == "_"
        && dot.kind() == SyntaxKind::DOT_TOK)
        .then(|| token_span(&dot))
}

/// `for _ = a to b`: FCS's `idOfPat` raises its parse error at the pattern
/// (`ParseHelpers.fs`).
fn wildcard_for_loop_span(node: &SyntaxNode) -> Option<Range<usize>> {
    let mut children = significant_children(node);
    if children.next()?.kind() != SyntaxKind::FOR_TOK {
        return None;
    }
    let SyntaxElement::Token(binder) = children.next()? else {
        return None;
    };
    let equals = children.next()?;
    (binder.text() == "_" && equals.kind() == SyntaxKind::EQUALS_TOK).then(|| token_span(&binder))
}

/// `'a & #I` in a type-parameter declaration: FCS reports from the first `&`
/// to the end of the constraints (`pars.fsy` `typarDecl`).
fn typar_decl_intersection_span(node: &SyntaxNode) -> Option<Range<usize>> {
    let amp = node
        .children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|t| t.kind() == SyntaxKind::AMP_TOK)?;
    let end = significant_range(node)?.end;
    Some(token_span(&amp).start..end)
}

/// `module [<A>] M`, top-level or nested: attributes after the `module`
/// keyword. FCS reports at its empty `opt_rec`, whose range is the token
/// after the attributes and the access modifier — `rec` if present, else the
/// first segment of the name (`pars.fsy` `moduleIntro`).
fn module_attributes_span(node: &SyntaxNode) -> Option<Range<usize>> {
    let mut children = significant_children(node)
        .skip_while(|el| el.kind() != SyntaxKind::MODULE_TOK)
        .skip(1)
        .peekable();
    if children.peek()?.kind() != SyntaxKind::ATTRIBUTE_LIST {
        return None;
    }
    let after = children.find(|el| {
        !matches!(
            el.kind(),
            SyntaxKind::ATTRIBUTE_LIST | SyntaxKind::ACCESS_TOK
        )
    })?;
    let first = match after {
        SyntaxElement::Token(t) => t,
        SyntaxElement::Node(n) => n
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(is_significant)?,
    };
    Some(token_span(&first))
}

/// `let! x: T` / `use! x: T` / `and! x: T`: FCS reports the annotation's type
/// (`pars.fsy` `ceBindingCore`). A plain `let` binding with an annotation is
/// not gated.
fn typed_bang_binding_span(binding: &SyntaxNode) -> Option<Range<usize>> {
    let parent = binding.parent()?;
    if parent.kind() != SyntaxKind::LET_OR_USE_EXPR {
        return None;
    }
    let keyword = std::iter::successors(binding.prev_sibling_or_token(), |el| {
        el.prev_sibling_or_token()
    })
    .filter_map(SyntaxElement::into_token)
    .find(is_significant)?;
    let is_bang = match keyword.kind() {
        SyntaxKind::AND_BANG_TOK => true,
        SyntaxKind::BINDER_TOK => keyword.text().ends_with('!'),
        _ => false,
    };
    if !is_bang {
        return None;
    }
    let return_info = binding
        .children()
        .find(|n| n.kind() == SyntaxKind::BINDING_RETURN_INFO)?;
    match return_info.children().find_map(|ty| significant_range(&ty)) {
        Some(span) => Some(span),
        // `let! x : = …`: FCS recovers an empty type at the end of the `:`,
        // and reports there.
        None => {
            let colon = return_info
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(|t| t.kind() == SyntaxKind::COLON_TOK)?;
            let end = token_span(&colon).end;
            Some(end..end)
        }
    }
}

/// `return e: T` / `yield e: T` and their `!` forms: an unparenthesised
/// annotation on the expression. FCS reports the annotated expression
/// (`pars.fsy`'s `YIELD declExpr COLON typ` and siblings). The implicit yield
/// of a `for … -> e: T` comprehension shares the node but not the production
/// (`arrowThenExprR`), and is not gated.
fn typed_yield_or_return_span(node: &SyntaxNode) -> Option<Range<usize>> {
    let keyword = node
        .children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(is_significant)?;
    if !matches!(
        keyword.kind(),
        SyntaxKind::YIELD_TOK | SyntaxKind::YIELD_BANG_TOK
    ) {
        return None;
    }
    let expr = node.children().next()?;
    if expr.kind() != SyntaxKind::TYPED_EXPR {
        return None;
    }
    significant_range(&expr)
}

/// `static abstract`: FCS reports the two keywords (`pars.fsy`
/// `abstractMemberFlags`).
fn static_abstract_span(node: &SyntaxNode) -> Option<Range<usize>> {
    let mut tokens = node
        .children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(is_significant);
    let first = tokens.next()?;
    if first.kind() != SyntaxKind::STATIC_TOK {
        return None;
    }
    let second = tokens.next()?;
    if second.kind() != SyntaxKind::ABSTRACT_TOK {
        return None;
    }
    Some(token_span(&first).start..token_span(&second).end)
}

/// A `float32` literal written as an integer and a suffix (`1f`, `1_0F`): FCS's
/// `ieee32_dotless_no_exponent`, whose `integer` is `digit ((digit | '_')*
/// digit)?`, so `1_f` is not one (it is an invalid numeric literal at every
/// version). A sign folded into the literal is not part of what FCS's lexer
/// reports.
fn dotless_float32_span(token: &SyntaxToken) -> Option<Range<usize>> {
    if token.kind() != SyntaxKind::IEEE32_LIT {
        return None;
    }
    let text = token.text();
    let unsigned = text.trim_start_matches(['+', '-']);
    let digits = unsigned.strip_suffix(['f', 'F'])?.as_bytes();
    let is_integer = digits.first().is_some_and(u8::is_ascii_digit)
        && digits.last().is_some_and(u8::is_ascii_digit)
        && digits.iter().all(|b| b.is_ascii_digit() || *b == b'_');
    if !is_integer {
        return None;
    }
    let span = token_span(token);
    Some(span.start + (text.len() - unsigned.len())..span.end)
}
