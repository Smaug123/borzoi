//! The `///` documentation FCS attaches to a declaration in F# source — the
//! project-local counterpart of [`super::lookup`], which reads a referenced
//! assembly's `.xml`.
//!
//! # How FCS attaches a doc comment
//!
//! Attachment is decided in two halves, and both are reproduced here exactly
//! or not at all (`XmlDoc.fs`, `LexerStore.fs`, `lex.fsl`, `LexFilter.fs`,
//! `pars.fsy` in the F# compiler):
//!
//! 1. **Lexically** (`XmlDocCollector`). A `///` line comment — exactly three
//!    slashes; `////` is an ordinary comment — is saved as the text after the
//!    slashes, up to the end of the line. The lex filter calls `AddGrabPoint`
//!    after *every* token the lexer returns, so the lines saved since the
//!    previous token become a block keyed by the **start position of the next
//!    token**, whatever that token is. Whitespace, newlines, conditional
//!    directives and `#if`-dead regions are not tokens (the lexer runs with
//!    `skip = true`), so they neither end nor break a block; blank lines do not
//!    either. An ordinary comment (`//`, `////`, `(* … *)`) after some saved
//!    lines arms a *delayed* grab point at itself; if another `///` line then
//!    follows, the earlier lines are given to the comment's position — which no
//!    declaration ever grabs — and the block restarts. So in
//!    `/// a` `// c` `/// b` `let x`, `x` gets ` b` only, while in
//!    `/// a` `// c` `let x` it gets ` a`. A `(*)` inside a block comment is an
//!    immediate grab point (`lex.fsl`'s comment rule), which this module
//!    declines rather than re-lexing comment interiors.
//! 2. **Grammatically** (`grabXmlDoc` in the parser). A declaration takes the
//!    block keyed at *one particular token*: its first attribute list's `[<`
//!    when attributes precede the keyword, otherwise its leading token — `let`
//!    / `use`, `type`, `exception`, `val`, the member's first modifier or
//!    keyword, a union or enum case's `|`, or a bar-less first case's name. An
//!    `and` binding or type takes the block before `and` when there is one
//!    (even an all-blank one: the test is `HasComments`), and otherwise the
//!    block before its first token after `and`. A block keyed at a token no
//!    declaration grabs is dropped (FCS's FS3520 "XML comment is not placed on
//!    a valid language element").
//!
//! The lines are what FCS calls `UnprocessedLines`; [`elaborate`] is its
//! implicit-`<summary>` rule, and [`member_text`] the `<member>` element fsc
//! writes them into, which the shared renderer reads exactly as it reads an
//! entry of a referenced assembly's `.xml`.
//!
//! # What is not modelled
//!
//! Every declaration shape and trivia construct outside the list above is a
//! typed [`SourceDocDecline`], never a guess: a doc shown must be the doc FCS
//! attaches, and a neighbour's doc must never be shown. The differential
//! against FCS (`xml_doc_source_diff`) holds every committed answer to that.
//!
//! A doc is only as right as the resolution that chose its declaration, and
//! the differential found two shapes resolution gets wrong (#323, #324); they
//! decline here too ([`SourceDocDecline::SameNameDeclarations`],
//! [`SourceDocDecline::NamedArgumentCandidate`]) until resolution is fixed.

use std::collections::HashMap;

use borzoi_cst::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use borzoi_sema::{
    Def, DefKind, ProjectFile, Resolution, ResolvedProject, SourceFile, signature_partners,
};
use rowan::{NodeOrToken, TextRange};

use super::depth::{BoundedParseError, parse_bounded};
use super::tree::{DocElement, TooDeep};

/// What FCS attaches to one source declaration, or why we will not say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceDoc {
    /// FCS attaches exactly these `UnprocessedLines` — the text after each
    /// `///`, in order. Empty when FCS attaches nothing; a non-empty block of
    /// blank lines is attached too (and elaborates to nothing).
    Attached(Vec<String>),
    /// We cannot establish what FCS attaches.
    Declined(SourceDocDecline),
}

/// Why [`SourceDoc`] could not establish a declaration's documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceDocDecline {
    /// The definition's range does not land on a declaration we can find in
    /// the tree (a stale or synthetic [`Def`]).
    NoDeclaration,
    /// A value binder inside a `let` binding but outside its head pattern.
    PatternBinder,
    /// A declaration shape whose grab token is not modelled.
    UnmodelledDeclaration(DeclShape),
    /// A `(*)` inside a block comment in the trivia before the grab token —
    /// an immediate grab point in FCS's comment lexer, whose interior rules
    /// (strings and characters inside comments) are not re-lexed here.
    BlockCommentGrabPoint,
    /// A `///` line ended by a lone carriage return, which FCS's comment lexer
    /// drops the line at (`singleLineComment`'s catch-all rule).
    LoneCarriageReturn,
    /// A `#line` directive in the trivia before the grab token: FCS re-keys
    /// every later position by it, which the differential cannot observe.
    LineDirective,
    /// The file did not parse cleanly, so the tree may not be the one FCS
    /// grabs from.
    ParseErrors,
    /// The declaration is in an implementation file a signature constrains,
    /// and its own doc is blank: FCS then shows the signature's doc
    /// (`Val.XmlDoc` falls back to `val_other_xmldoc`, set during signature
    /// conformance), which is not located here.
    SignatureFallback,
    /// The declaration's container declares another type (or the type another
    /// case) of the same name. Same-named types of different arity are legal
    /// and resolution does not yet choose between them by arity (#323), so the
    /// occurrence may name the other one; a duplicate case is an error FCS
    /// resolves to the first.
    SameNameDeclarations,
    /// The occurrence is the left of `=` in a parenthesised application
    /// argument — a named argument when the callee is a method or constructor,
    /// which FCS binds to the parameter, not to the value resolution found
    /// (#324).
    NamedArgumentCandidate,
    /// A use in another file reached a declaration of an implementation file a
    /// signature constrains. FCS binds such a use to the *signature's* symbol,
    /// whose doc is the signature's alone, so the implementation's doc is not
    /// the answer.
    ThroughSignature,
}

/// A declaration shape [`SourceDocDecline::UnmodelledDeclaration`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DeclShape {
    /// A `let`-like binding under a node other than a `let` declaration,
    /// expression or class `let` (`let!`, a computation-expression binder…).
    BindingContainer,
    /// A type definition whose leading token is neither `type`, `and` nor an
    /// attribute list.
    TypeDefinition,
    /// A bar-less first union or enum case carrying attributes, which the
    /// grammar does not accept.
    AttributedBarlessCase,
}

/// The token a declaration grabs its documentation at.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GrabRule {
    /// The block keyed at this token.
    At(SyntaxToken),
    /// An `and` binding or type: the block before `and` if there is one,
    /// otherwise the block before the first token after it.
    AndOrInner {
        and: SyntaxToken,
        inner: SyntaxToken,
    },
}

/// One file's tokens in order, so many declarations' documentation can be read
/// without re-walking the tree.
#[derive(Debug, Clone)]
pub struct SourceDocIndex {
    root: SyntaxNode,
    tokens: Vec<SyntaxToken>,
}

impl SourceDocIndex {
    pub fn new(root: &SyntaxNode) -> Self {
        let tokens = root
            .descendants_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .collect();
        SourceDocIndex {
            root: root.clone(),
            tokens,
        }
    }

    /// The documentation FCS attaches to the declaration `def` names. `def`
    /// must be a binder of the file this index was built over.
    pub fn doc_for(&self, def: &Def) -> SourceDoc {
        match self.grab_rule(def) {
            Ok(None) => SourceDoc::Attached(Vec::new()),
            Ok(Some(GrabRule::At(token))) => match self.block_before(&token) {
                Ok(lines) => SourceDoc::Attached(lines),
                Err(why) => SourceDoc::Declined(why),
            },
            Ok(Some(GrabRule::AndOrInner { and, inner })) => {
                match self.block_before(&and) {
                    // `HasComments`: any saved line, blank or not, wins.
                    Ok(lines) if !lines.is_empty() => SourceDoc::Attached(lines),
                    Ok(_) => match self.block_before(&inner) {
                        Ok(lines) => SourceDoc::Attached(lines),
                        Err(why) => SourceDoc::Declined(why),
                    },
                    Err(why) => SourceDoc::Declined(why),
                }
            }
            Err(why) => SourceDoc::Declined(why),
        }
    }

    /// The `///` lines FCS keys at `grab`: the trivia run between the previous
    /// real token and `grab`, folded through the collector's state machine.
    fn block_before(&self, grab: &SyntaxToken) -> Result<Vec<String>, SourceDocDecline> {
        let at = self.index_of(grab);
        let mut start = at;
        while start > 0 && !is_real(&self.tokens[start - 1]) {
            start -= 1;
        }
        let run = &self.tokens[start..at];
        let mut lines: Vec<String> = Vec::new();
        // A delayed grab point is armed (an ordinary comment followed some
        // saved lines); the next `///` line commits the saved lines elsewhere.
        let mut delayed = false;
        for (i, token) in run.iter().enumerate() {
            match token.kind() {
                SyntaxKind::LINE_COMMENT => {
                    if let Some(line) = doc_line_text(token.text()) {
                        let next = run.get(i + 1).or_else(|| self.tokens.get(at));
                        if next.is_some_and(|n| n.kind() == SyntaxKind::NEWLINE && n.text() == "\r")
                        {
                            return Err(SourceDocDecline::LoneCarriageReturn);
                        }
                        if delayed {
                            lines.clear();
                            delayed = false;
                        }
                        lines.push(line.to_string());
                    } else if !lines.is_empty() {
                        delayed = true;
                    }
                }
                SyntaxKind::BLOCK_COMMENT => {
                    if token.text().contains("(*)") {
                        return Err(SourceDocDecline::BlockCommentGrabPoint);
                    }
                    if !lines.is_empty() {
                        delayed = true;
                    }
                }
                SyntaxKind::HASH_LINE => return Err(SourceDocDecline::LineDirective),
                // Whitespace, newlines, the other directives, dead regions, and
                // the parser's zero-width virtual tokens: not lexer tokens (the
                // lexer consumes them under `skip = true`), so invisible to the
                // collector.
                _ => {}
            }
        }
        Ok(lines)
    }

    fn index_of(&self, token: &SyntaxToken) -> usize {
        let start = token.text_range().start();
        let mut i = self
            .tokens
            .partition_point(|t| t.text_range().start() < start);
        while self.tokens[i] != *token {
            i += 1;
        }
        i
    }

    /// Which token `def`'s declaration grabs at, or `None` for a binder FCS
    /// gives no documentation (parameters, pattern locals, type parameters).
    fn grab_rule(&self, def: &Def) -> Result<Option<GrabRule>, SourceDocDecline> {
        let element = covering(&self.root, def.range).ok_or(SourceDocDecline::NoDeclaration)?;
        match def.kind {
            // An active-pattern case token's own symbol (`FSharpActivePatternCase`
            // at its defining occurrence) carries no doc: FCS reads it from the
            // recognizer's value, which a defining occurrence does not record.
            DefKind::Parameter
            | DefKind::PatternLocal
            | DefKind::TypeParam
            | DefKind::ActivePatternCase => Ok(None),
            DefKind::Value { .. } | DefKind::ActivePattern => binding_rule(&element, def.range),
            DefKind::Type => type_rule(&element, def.range).map(Some),
            DefKind::UnionCase | DefKind::EnumCase => case_rule(&element, def.range).map(Some),
            DefKind::ExceptionCase => exception_rule(&element, def.range).map(Some),
            DefKind::Member => member_rule(&element).map(Some),
        }
    }
}

/// The text after the `///` of an XML doc comment, or `None` for an ordinary
/// line comment (`//`, `////`, a shebang).
pub fn doc_line_text(comment: &str) -> Option<&str> {
    let rest = comment.strip_prefix("///")?;
    (!rest.starts_with('/')).then_some(rest)
}

/// A token the FCS lexer returns to the lex filter — one that takes the
/// pending block as a grab point. Trivia and the parser's zero-width virtual
/// tokens are not.
fn is_real(token: &SyntaxToken) -> bool {
    !token.kind().is_trivia() && !token.text().is_empty()
}

fn first_real_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(is_real)
}

fn covering(root: &SyntaxNode, range: TextRange) -> Option<NodeOrToken<SyntaxNode, SyntaxToken>> {
    if !root.text_range().contains_range(range) {
        return None;
    }
    Some(root.covering_element(range))
}

fn ancestors(
    element: &NodeOrToken<SyntaxNode, SyntaxToken>,
) -> impl Iterator<Item = SyntaxNode> + use<> {
    let start = match element {
        NodeOrToken::Node(n) => Some(n.clone()),
        NodeOrToken::Token(t) => t.parent(),
    };
    std::iter::successors(start, |n| n.parent())
}

/// The previous sibling element of `node` that is neither trivia nor a
/// zero-width virtual token.
fn previous_real_sibling(node: &SyntaxNode) -> Option<NodeOrToken<SyntaxNode, SyntaxToken>> {
    std::iter::successors(node.prev_sibling_or_token(), |e| e.prev_sibling_or_token()).find(|e| {
        match e {
            NodeOrToken::Token(t) => is_real(t),
            NodeOrToken::Node(n) => first_real_token(n).is_some(),
        }
    })
}

/// A `let` / `use` binder (value, function, active-pattern recognizer or case).
fn binding_rule(
    element: &NodeOrToken<SyntaxNode, SyntaxToken>,
    range: TextRange,
) -> Result<Option<GrabRule>, SourceDocDecline> {
    if is_self_identifier(element, range) {
        return Ok(None);
    }
    let binding = ancestors(element)
        .find(|n| matches!(n.kind(), SyntaxKind::BINDING | SyntaxKind::VAL_DECL))
        .ok_or(SourceDocDecline::NoDeclaration)?;
    if binding.kind() == SyntaxKind::VAL_DECL {
        return val_rule(&binding, range).map(Some);
    }
    if !in_head_pattern(&binding, range) {
        return Err(SourceDocDecline::PatternBinder);
    }
    let container = binding.parent().ok_or(SourceDocDecline::NoDeclaration)?;
    if !matches!(
        container.kind(),
        SyntaxKind::LET_DECL | SyntaxKind::LET_OR_USE_EXPR | SyntaxKind::MEMBER_LET_BINDINGS
    ) {
        return Err(SourceDocDecline::UnmodelledDeclaration(
            DeclShape::BindingContainer,
        ));
    }
    match previous_real_sibling(&binding) {
        Some(NodeOrToken::Token(t)) if t.kind() == SyntaxKind::AND_TOK => {
            let inner = first_real_token(&binding).ok_or(SourceDocDecline::NoDeclaration)?;
            Ok(Some(GrabRule::AndOrInner { and: t, inner }))
        }
        _ => {
            let first = first_real_token(&container).ok_or(SourceDocDecline::NoDeclaration)?;
            Ok(Some(GrabRule::At(first)))
        }
    }
}

/// Whether the value binder at `range` is a self identifier — the `x` of
/// `member x.M` or of `type T() as x` — which FCS binds as an undocumented
/// local.
fn is_self_identifier(element: &NodeOrToken<SyntaxNode, SyntaxToken>, range: TextRange) -> bool {
    if let NodeOrToken::Token(t) = element
        && t.parent()
            .is_some_and(|p| p.kind() == SyntaxKind::IMPLICIT_CTOR)
        && std::iter::successors(t.prev_token(), |p| p.prev_token())
            .find(is_real)
            .is_some_and(|p| p.kind() == SyntaxKind::AS_TOK)
    {
        return true;
    }
    let Some(name) = ancestors(element).find(|n| n.kind() == SyntaxKind::LONG_IDENT) else {
        return false;
    };
    let in_member_head = name.parent().is_some_and(|pat| {
        pat.kind() == SyntaxKind::LONG_IDENT_PAT
            && pat.parent().is_some_and(|p| match p.kind() {
                SyntaxKind::GET_SET_MEMBER => true,
                SyntaxKind::BINDING => p
                    .parent()
                    .is_some_and(|m| m.kind() == SyntaxKind::MEMBER_DEFN),
                _ => false,
            })
    });
    let mut tokens = name
        .children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .filter(|t| !t.kind().is_trivia());
    in_member_head
        && tokens
            .next()
            .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text_range() == range)
        && tokens
            .next()
            .is_some_and(|t| t.kind() == SyntaxKind::DOT_TOK)
}

/// A signature file's `val`: its first attribute or the `val` keyword.
fn val_rule(decl: &SyntaxNode, range: TextRange) -> Result<GrabRule, SourceDocDecline> {
    let names_it = decl
        .children()
        .filter(|n| n.kind() == SyntaxKind::VAL_SIG)
        .flat_map(|sig| {
            sig.children_with_tokens()
                .filter_map(NodeOrToken::into_token)
        })
        .any(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text_range() == range);
    if !names_it {
        return Err(SourceDocDecline::NoDeclaration);
    }
    let first = first_real_token(decl).ok_or(SourceDocDecline::NoDeclaration)?;
    Ok(GrabRule::At(first))
}

/// Whether the value binder at `range` is bound by `binding`'s head pattern —
/// its name (`let x`, `let f x`, `let (+++) a b`, `let (|A|B|) x`) or any name
/// of a destructuring head (`let a, b`, `let (c, d) as e`, `let Some s`).
/// Every value a binding's head binds carries the binding's doc in FCS.
/// (Function parameters are [`DefKind::Parameter`] and never reach here.)
fn in_head_pattern(binding: &SyntaxNode, range: TextRange) -> bool {
    binding
        .children()
        .find(|n| n.kind() != SyntaxKind::ATTRIBUTE_LIST)
        .is_some_and(|head| head.text_range().contains_range(range))
}

/// A `type` definition's name.
fn type_rule(
    element: &NodeOrToken<SyntaxNode, SyntaxToken>,
    range: TextRange,
) -> Result<GrabRule, SourceDocDecline> {
    let defn = ancestors(element)
        .find(|n| n.kind() == SyntaxKind::TYPE_DEFN)
        .ok_or(SourceDocDecline::NoDeclaration)?;
    let name = defn
        .children()
        .find(|n| n.kind() == SyntaxKind::LONG_IDENT)
        .ok_or(SourceDocDecline::NoDeclaration)?;
    if !name.text_range().contains_range(range) {
        return Err(SourceDocDecline::NoDeclaration);
    }
    let container = defn
        .parent()
        .and_then(|defns| defns.parent())
        .ok_or(SourceDocDecline::NoDeclaration)?;
    let namesakes = container
        .children()
        .filter(|n| n.kind() == SyntaxKind::TYPE_DEFNS)
        .flat_map(|defns| defns.children())
        .filter(|n| n.kind() == SyntaxKind::TYPE_DEFN)
        .filter_map(|n| n.children().find(|c| c.kind() == SyntaxKind::LONG_IDENT))
        .filter(|other| ident_text(other) == ident_text(&name))
        .count();
    if namesakes > 1 {
        return Err(SourceDocDecline::SameNameDeclarations);
    }
    let first = first_real_token(&defn).ok_or(SourceDocDecline::NoDeclaration)?;
    match first.kind() {
        SyntaxKind::AND_TOK => {
            let inner = defn
                .descendants_with_tokens()
                .filter_map(NodeOrToken::into_token)
                .filter(is_real)
                .nth(1)
                .ok_or(SourceDocDecline::NoDeclaration)?;
            Ok(GrabRule::AndOrInner { and: first, inner })
        }
        SyntaxKind::TYPE_TOK | SyntaxKind::LBRACK_LESS_TOK => Ok(GrabRule::At(first)),
        _ => Err(SourceDocDecline::UnmodelledDeclaration(
            DeclShape::TypeDefinition,
        )),
    }
}

/// A union or enum case: the `|` before it, or a bar-less first case's name.
fn case_rule(
    element: &NodeOrToken<SyntaxNode, SyntaxToken>,
    range: TextRange,
) -> Result<GrabRule, SourceDocDecline> {
    let case = ancestors(element)
        .find(|n| matches!(n.kind(), SyntaxKind::UNION_CASE | SyntaxKind::ENUM_CASE))
        .ok_or(SourceDocDecline::NoDeclaration)?;
    if !is_case_name(&case, range) {
        return Err(SourceDocDecline::NoDeclaration);
    }
    if let Some(repr) = case.parent() {
        let namesakes = repr
            .children()
            .filter(|n| n.kind() == case.kind())
            .filter(|other| ident_text(other) == ident_text(&case))
            .count();
        if namesakes > 1 {
            return Err(SourceDocDecline::SameNameDeclarations);
        }
    }
    match previous_real_sibling(&case) {
        Some(NodeOrToken::Token(bar)) if bar.kind() == SyntaxKind::BAR_TOK => Ok(GrabRule::At(bar)),
        None => {
            if case
                .children()
                .any(|n| n.kind() == SyntaxKind::ATTRIBUTE_LIST)
            {
                return Err(SourceDocDecline::UnmodelledDeclaration(
                    DeclShape::AttributedBarlessCase,
                ));
            }
            let first = first_real_token(&case).ok_or(SourceDocDecline::NoDeclaration)?;
            Ok(GrabRule::At(first))
        }
        Some(_) => Err(SourceDocDecline::NoDeclaration),
    }
}

fn is_case_name(case: &SyntaxNode, range: TextRange) -> bool {
    case.children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .any(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text_range() == range)
}

/// The identifier `node` declares (its direct `IDENT_TOK` children, joined
/// with `.`), double backticks stripped: `` ``A`` `` and `A` are one name.
fn ident_text(node: &SyntaxNode) -> String {
    node.children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
        .map(|t| {
            let text = t.text();
            text.strip_prefix("``")
                .and_then(|t| t.strip_suffix("``"))
                .unwrap_or(text)
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// An `exception` definition's constructor.
fn exception_rule(
    element: &NodeOrToken<SyntaxNode, SyntaxToken>,
    range: TextRange,
) -> Result<GrabRule, SourceDocDecline> {
    let defn = ancestors(element)
        .find(|n| n.kind() == SyntaxKind::EXCEPTION_DEFN)
        .ok_or(SourceDocDecline::NoDeclaration)?;
    let case = defn
        .children()
        .find(|n| n.kind() == SyntaxKind::UNION_CASE)
        .ok_or(SourceDocDecline::NoDeclaration)?;
    if !is_case_name(&case, range) {
        return Err(SourceDocDecline::NoDeclaration);
    }
    let first = first_real_token(&defn).ok_or(SourceDocDecline::NoDeclaration)?;
    Ok(GrabRule::At(first))
}

/// A member definition — a method, a property (with or without explicit
/// accessors), an auto-property: its first attribute or modifier or keyword.
fn member_rule(
    element: &NodeOrToken<SyntaxNode, SyntaxToken>,
) -> Result<GrabRule, SourceDocDecline> {
    let member = ancestors(element)
        .find(|n| {
            matches!(
                n.kind(),
                SyntaxKind::MEMBER_DEFN | SyntaxKind::AUTO_PROPERTY | SyntaxKind::GET_SET_MEMBER
            )
        })
        .ok_or(SourceDocDecline::NoDeclaration)?;
    let first = first_real_token(&member).ok_or(SourceDocDecline::NoDeclaration)?;
    Ok(GrabRule::At(first))
}

/// FCS's `XmlDoc.IsEmpty`: every line is blank (`String.IsNullOrWhiteSpace`,
/// whose whitespace set is Unicode `White_Space`, as [`char::is_whitespace`]).
pub fn is_blank(lines: &[String]) -> bool {
    lines.iter().all(|l| l.chars().all(char::is_whitespace))
}

/// FCS's `XmlDoc.GetElaboratedXmlLines`: drop leading lines that are empty
/// once leading **spaces** (only) are trimmed; if the first remaining line
/// then starts with `<`, the remaining lines are the XML verbatim; otherwise
/// they are XML-escaped and wrapped in an implicit `<summary>`.
pub fn elaborate(lines: &[String]) -> Vec<String> {
    let first = lines
        .iter()
        .position(|l| !l.trim_start_matches(' ').is_empty());
    let Some(first) = first else {
        return Vec::new();
    };
    let rest = &lines[first..];
    if rest[0].trim_start_matches(' ').starts_with('<') {
        return rest.to_vec();
    }
    let mut out = Vec::with_capacity(rest.len() + 2);
    out.push("<summary>".to_string());
    out.extend(rest.iter().map(|l| escape(l)));
    out.push("</summary>".to_string());
    out
}

/// `Internal.Utilities.XmlAdapters.escape`.
fn escape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '&' => out.push_str("&amp;"),
            c => out.push(c),
        }
    }
    out
}

/// The `<member>` element fsc writes `elaborated` lines into
/// (`XmlDocFileWriter`: the open tag, the lines joined by newlines, the close
/// tag, each on its own line), so a source doc parses exactly as the entry a
/// built assembly's `.xml` would carry for it.
pub fn member_text(elaborated: &[String]) -> String {
    let mut text = String::from("<member>\n");
    for line in elaborated {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str("</member>");
    text
}

/// Whether the name at `at` is the left of `=` in a parenthesised (possibly
/// tupled) application argument — `M(x = 1)`, `f (x = 1, y)` — which is a
/// named argument when the callee is a method or constructor and an equality
/// test otherwise; syntax cannot tell which.
fn is_named_argument_candidate(root: &SyntaxNode, at: TextRange) -> bool {
    if !root.text_range().contains_range(at) {
        return false;
    }
    let NodeOrToken::Token(token) = root.covering_element(at) else {
        return false;
    };
    let Some(lhs) = token
        .parent()
        .filter(|p| p.kind() == SyntaxKind::IDENT_EXPR)
    else {
        return false;
    };
    let Some(infix) = lhs
        .parent()
        .filter(|p| p.kind() == SyntaxKind::INFIX_APP_EXPR)
    else {
        return false;
    };
    let is_equals = infix.first_child().as_ref() == Some(&lhs)
        && infix.children().nth(1).is_some_and(|op| {
            op.kind() == SyntaxKind::LONG_IDENT_EXPR && op.text().to_string().trim() == "="
        });
    if !is_equals {
        return false;
    }
    let Some(mut arg) = infix.parent().filter(|p| p.kind() == SyntaxKind::APP_EXPR) else {
        return false;
    };
    if arg
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::TUPLE_EXPR)
    {
        arg = arg.parent().expect("checked");
    }
    arg.parent().is_some_and(|paren| {
        paren.kind() == SyntaxKind::PAREN_EXPR
            && paren
                .parent()
                .is_some_and(|app| app.kind() == SyntaxKind::APP_EXPR)
    })
}

/// Why attached `///` lines yield no documentation tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRenderError {
    /// The lines are all blank: FCS's `XmlDoc.IsEmpty`, which tooling shows
    /// as no documentation.
    Blank,
    /// The elaborated text is not well-formed XML (FCS reports FS3390 for it
    /// under `--warnon:3390`); what tooling shows for it is not established.
    Malformed(String),
    /// The elaborated text nests deeper than the renderer's bound.
    TooDeep,
}

/// The `<member>` documentation tree of attached `///` lines, exactly as a
/// built assembly's `.xml` would carry it for the same declaration.
pub fn member_element(lines: &[String]) -> Result<DocElement, SourceRenderError> {
    if is_blank(lines) {
        return Err(SourceRenderError::Blank);
    }
    let text = member_text(&elaborate(lines));
    let doc = parse_bounded(&text).map_err(|e| match e {
        BoundedParseError::TooDeep => SourceRenderError::TooDeep,
        BoundedParseError::Xml(e) => SourceRenderError::Malformed(e.to_string()),
    })?;
    DocElement::from_roxmltree(doc.root_element()).map_err(|TooDeep| SourceRenderError::TooDeep)
}

/// The documentation of project-local resolutions across a project's files:
/// the per-file token indexes, built on demand, and the signature pairing that
/// decides whose doc FCS shows.
///
/// FCS keeps an implementation's and its signature's docs on two symbols. The
/// implementation's `Val` shows its own doc, or the signature's when its own is
/// blank; a use in another file binds the signature's `Val`, which shows the
/// signature's doc only. So a resolution into a signature file is answered from
/// the signature, one into its own (paired) implementation file from the
/// implementation when that doc is not blank, and anything that would need the
/// other file's doc declines.
pub struct ProjectDocs<'a> {
    files: &'a [ProjectFile],
    resolved: &'a ResolvedProject,
    partners: Vec<Option<usize>>,
    indexes: HashMap<usize, SourceDocIndex>,
}

impl<'a> ProjectDocs<'a> {
    /// `resolved` must be a fold of (a prefix of) `files`.
    pub fn new(files: &'a [ProjectFile], resolved: &'a ResolvedProject) -> Self {
        ProjectDocs {
            files,
            resolved,
            partners: signature_partners(files),
            indexes: HashMap::new(),
        }
    }

    /// The documentation of what `res`, the occurrence at `at` in Compile-order
    /// file `from`, resolves to — `None` when it does not resolve to a binder
    /// of the project's own sources.
    pub fn doc(&mut self, from: usize, at: TextRange, res: Resolution) -> Option<SourceDoc> {
        let (file, def) = match res {
            Resolution::Local(id) => (from, self.resolved.file(from).def(id)),
            Resolution::Item(_) => self.resolved.item_def(res)?,
            Resolution::Entity(_)
            | Resolution::Member { .. }
            | Resolution::Deferred(_)
            | Resolution::Unresolved => return None,
        };
        let project_file = self.files.get(file)?;
        if !project_file
            .recovery
            .clean_through(project_file.file.syntax())
        {
            return Some(SourceDoc::Declined(SourceDocDecline::ParseErrors));
        }
        if self
            .files
            .get(from)
            .is_some_and(|f| is_named_argument_candidate(f.file.syntax(), at))
        {
            return Some(SourceDoc::Declined(
                SourceDocDecline::NamedArgumentCandidate,
            ));
        }
        let constrained = matches!(project_file.file, SourceFile::Impl(_))
            && self.partners.get(file).copied().flatten().is_some();
        if constrained && file != from {
            return Some(SourceDoc::Declined(SourceDocDecline::ThroughSignature));
        }
        let index = self
            .indexes
            .entry(file)
            .or_insert_with(|| SourceDocIndex::new(project_file.file.syntax()));
        Some(match index.doc_for(def) {
            SourceDoc::Attached(lines) if constrained && is_blank(&lines) => {
                SourceDoc::Declined(SourceDocDecline::SignatureFallback)
            }
            other => other,
        })
    }
}

#[cfg(test)]
mod tests {
    //! FCS-free properties of the collector and the elaboration. What FCS
    //! attaches is pinned by the `xml_doc_source_diff` differential; these
    //! pin what holds whatever FCS does — chiefly that a doc shown is made of
    //! the source's own `///` lines, never anything else.

    use super::*;
    use borzoi_cst::parser::parse;
    use borzoi_cst::syntax::{AstNode, ImplFile};
    use borzoi_sema::{AssemblyEnv, ProjectItems, SyntaxRecovery, resolve_file};
    use proptest::prelude::*;

    /// Every definition site in `src` with the doc we would attach to it.
    fn docs_of(src: &str) -> Vec<(String, SourceDoc)> {
        let parse = parse(src);
        let recovery = SyntaxRecovery::of(&parse);
        let file = ImplFile::cast(parse.root).expect("impl root");
        let resolved = resolve_file(
            &file,
            &ProjectItems::default(),
            &AssemblyEnv::default(),
            &recovery,
        );
        let index = SourceDocIndex::new(file.syntax());
        let mut defs: Vec<&Def> = resolved
            .resolutions()
            .iter()
            .filter_map(|(range, res)| resolved.resolved_def(*res).filter(|d| d.range == *range))
            .collect();
        defs.sort_by_key(|d| d.range.start());
        defs.dedup_by_key(|d| d.range);
        defs.into_iter()
            .map(|d| (d.name.clone(), index.doc_for(d)))
            .collect()
    }

    fn doc_of(src: &str, name: &str) -> SourceDoc {
        docs_of(src)
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no definition of {name}"))
            .1
    }

    fn attached(lines: &[&str]) -> SourceDoc {
        SourceDoc::Attached(lines.iter().map(|l| l.to_string()).collect())
    }

    #[test]
    fn three_slashes_exactly_make_a_doc_line() {
        assert_eq!(doc_line_text("/// a"), Some(" a"));
        assert_eq!(doc_line_text("///"), Some(""));
        assert_eq!(doc_line_text("///<summary>"), Some("<summary>"));
        assert_eq!(doc_line_text("//// a"), None);
        assert_eq!(doc_line_text("// a"), None);
        assert_eq!(doc_line_text("#!/usr/bin/env"), None);
    }

    #[test]
    fn elaboration_wraps_text_and_passes_xml_through() {
        let lines = |ls: &[&str]| ls.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        assert_eq!(
            elaborate(&lines(&[" a < b", " c"])),
            lines(&["<summary>", " a &lt; b", " c", "</summary>"])
        );
        assert_eq!(
            elaborate(&lines(&["", "  ", " <summary>x</summary>", "y"])),
            lines(&[" <summary>x</summary>", "y"])
        );
        assert_eq!(elaborate(&lines(&["", "   "])), Vec::<String>::new());
        // Only spaces are trimmed: a tab-led line is text.
        assert_eq!(
            elaborate(&lines(&["\t<b/>"])),
            lines(&["<summary>", "\t&lt;b/&gt;", "</summary>"])
        );
    }

    #[test]
    fn the_collector_follows_fcs_on_comments_between_doc_lines() {
        assert_eq!(
            doc_of("module M\n/// a\n// c\n/// b\nlet v = 1\n", "v"),
            attached(&[" b"])
        );
        assert_eq!(
            doc_of("module M\n/// a\n(* c *)\nlet v = 1\n", "v"),
            attached(&[" a"])
        );
        assert_eq!(doc_of("module M\nlet v = 1\n", "v"), attached(&[]));
    }

    #[test]
    fn unmodelled_trivia_declines() {
        assert_eq!(
            doc_of("module M\n/// a\n(* x (*) y *)\nlet v = 1\n", "v"),
            SourceDoc::Declined(SourceDocDecline::BlockCommentGrabPoint)
        );
        assert_eq!(
            doc_of("module M\n/// a\rlet v = 1\n", "v"),
            SourceDoc::Declined(SourceDocDecline::LoneCarriageReturn)
        );
        assert_eq!(
            doc_of("module M\n/// a\n# 3 \"x.fs\"\nlet v = 1\n", "v"),
            SourceDoc::Declined(SourceDocDecline::LineDirective)
        );
    }

    #[test]
    fn self_identifiers_and_parameters_carry_no_doc() {
        let src = "module M\ntype T() as self =\n    /// m\n    member x.M (p: int) = p\n";
        assert_eq!(doc_of(src, "self"), attached(&[]));
        assert_eq!(doc_of(src, "x"), attached(&[]));
        assert_eq!(doc_of(src, "p"), attached(&[]));
    }

    /// Source lines for the panic-freedom / provenance property: declarations,
    /// doc and ordinary comments, directives, and fragments that do not parse.
    const LINES: &[&str] = &[
        "/// doc",
        "///",
        "/// <summary>s</summary>",
        "//// four",
        "// c",
        "(* b *)",
        "(* (*) *)",
        "#if X",
        "#else",
        "#endif",
        "let v = 1",
        "let f x = x",
        "let rec g x = h x",
        "and h x = g x",
        "and",
        "type T = int",
        "type U =",
        "    | A",
        "    | B of int",
        "    C",
        "and Q = string",
        "exception E of string",
        "[<System.Obsolete>]",
        "let (|P|_|) x = Some x",
        "let a, b = 1, 2",
        "type K() as self =",
        "    member x.M = 1",
        "    static member S = 1",
        "    let y = 1",
        "let o () =",
        "    let z = 1",
        "    z",
        "  ) = (",
        "\t",
        "",
    ];

    proptest! {
        /// Whatever the source — well-formed or not — every attached line is
        /// the text of one of the source's own `///` comments, and the lines of
        /// one doc are distinct comments in source order: a doc is never made
        /// of anything else.
        #[test]
        fn attached_lines_are_the_sources_own_doc_comments(
            lines in proptest::collection::vec(proptest::sample::select(LINES), 0..24),
            crlf in any::<bool>(),
        ) {
            let src = format!("module M\n{}\n", lines.join(if crlf { "\r\n" } else { "\n" }));
            let parse = parse(&src);
            let comments: Vec<(usize, String)> = parse
                .root
                .descendants_with_tokens()
                .filter_map(NodeOrToken::into_token)
                .filter(|t| t.kind() == SyntaxKind::LINE_COMMENT)
                .filter_map(|t| {
                    doc_line_text(t.text()).map(|l| (usize::from(t.text_range().start()), l.to_string()))
                })
                .collect();
            for (name, doc) in docs_of(&src) {
                let SourceDoc::Attached(attached) = doc else { continue };
                // Some increasing run of doc comments spells exactly these lines.
                let mut from = 0;
                for line in &attached {
                    let found = comments[from..].iter().position(|(_, c)| c == line);
                    prop_assert!(found.is_some(), "{name}: {line:?} is no doc comment of\n{src}");
                    from += found.unwrap() + 1;
                }
            }
        }
    }
}
