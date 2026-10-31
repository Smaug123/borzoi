//! **Custom operations** of a computation expression's builder
//! (`docs/unmodelled-contributors.md`, route 12).
//!
//! Inside `b { … }`, FCS puts a name-environment entry for each of the
//! builder's custom operations — the methods carrying `[<CustomOperation>]` —
//! in front of the expression's body (`CheckComputationExpressions.fs`,
//! `TcComputationExpression` → `AddFakeNameToNameEnv`). A bare use of an
//! operation's name, or a dotted path it heads, then finds the operation
//! rather than any binding made outside the expression. A binding made inside
//! the expression comes later and wins, except where the translation reads a
//! clause by its name alone (`CustomOperationClause`): the head of a
//! statement is the operation whatever is bound.
//!
//! The walk answers both with [`Resolver::enter_computation_expression`]. It
//! lists the names the builder's operations may take ([`CeOps`]); pushes a
//! declining mark for each at the expression's start, in a frame of its own
//! that every later binding shadows; and records the clause heads among them
//! as declines outright. A builder whose operations it cannot list — its type
//! unknown, or some route that could add one unread — takes every name spelled
//! in the expression.
//!
//! The operations are those of the builder's type and its base types, and of
//! every extension of it in scope (`getCustomOperationMethods`, over
//! `AllMethInfosOfTypeInScope`), named by the attribute's string argument or,
//! with none, by the method. Extensions are counted without asking whether
//! they are in scope or what they extend: every `[<CustomOperation>]` the
//! project declares in a type augmentation or on a static member (a C#-style
//! `[<Extension>]` method), and every one a referenced assembly declares on
//! an extension member, is an operation of every builder.

use std::collections::BTreeSet;

use borzoi_cst::syntax::{
    AstNode, Attribute, AttributeList, Binding, ComputationExpr, Expr, InheritMember, Pat,
    SyntaxKind, SyntaxNode, SyntaxToken, Type, TypeDefn, TypeDefnRepr,
};

use super::Resolver;
use super::model::{DeclineCause, Resolution};
use super::state::{Frame, MarkReach, ScopeEntry};
use crate::assembly_env::{BuilderValue, CustomOperationNames};
use crate::def::{DefId, DefKind, id_text};

/// The names a builder's custom operations may take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CeOps {
    /// A closed over-approximation of them.
    Known(BTreeSet<String>),
    /// Some route that could add one is unread.
    Unknown,
}

impl CeOps {
    fn from_names(names: CustomOperationNames) -> CeOps {
        if names.unlisted {
            CeOps::Unknown
        } else {
            CeOps::Known(names.names)
        }
    }
}

/// The spellings of FSharp.Core's `CustomOperationAttribute`.
pub(super) const CUSTOM_OPERATION_SPELLINGS: &[&str] =
    &["CustomOperation", "CustomOperationAttribute"];

/// Whether `names` holds a type F# may find for an attribute written
/// `spelling`: one spelled so, or spelled so with `Attribute` after it, the
/// suffix F# also looks up (`[<CO>]` finds `COAttribute`).
pub(super) fn spelling_may_name(names: &std::collections::HashSet<String>, spelling: &str) -> bool {
    names.contains(spelling) || names.contains(&format!("{spelling}Attribute"))
}

/// Whether `defn` declares a type spelled like `CustomOperationAttribute`, or
/// an abbreviation that mentions it: after one, an attribute spelled any way
/// may be a custom operation.
pub(super) fn type_defn_renames_custom_operation(defn: &TypeDefn) -> bool {
    let spelled = |t: &SyntaxToken| CUSTOM_OPERATION_SPELLINGS.contains(&&*id_text(t));
    let named = defn
        .long_id()
        .and_then(|li| li.idents().last())
        .is_some_and(|name| spelled(&name));
    let aliases = matches!(defn.repr(), Some(TypeDefnRepr::Abbrev(a))
        if a.syntax()
            .descendants_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| spelled(&t)));
    named || aliases
}

/// The custom-operation names a file's declarations may add to a type they
/// do not define, and so to any builder: every operation a type
/// **augmentation** declares, and every one on a **static** member, which may
/// be a C#-style `[<Extension>]` method. `may_be_custom_operation` says which
/// written attribute names may be `[<CustomOperation>]`.
pub(super) fn file_custom_operations(
    root: &SyntaxNode,
    may_be_custom_operation: &dyn Fn(&str) -> bool,
) -> CustomOperationNames {
    let mut out = CustomOperationNames::default();
    for defn in root.descendants().filter_map(TypeDefn::cast) {
        let scope = if super::is_type_augmentation(&defn) {
            Members::All
        } else {
            Members::Static
        };
        out.merge(&type_defn_custom_operations(
            &defn,
            scope,
            may_be_custom_operation,
        ));
    }
    out
}

/// The custom-operation names a signature file may declare. A signature's
/// attributes are the ones later files see, and its declaration shapes are
/// its own, so every attribute in it that may be `[<CustomOperation>]`
/// counts, wherever it sits, naming by its arguments or by every identifier
/// the declaration it decorates declares.
pub(super) fn signature_custom_operations(
    root: &SyntaxNode,
    may_be_custom_operation: &dyn Fn(&str) -> bool,
) -> CustomOperationNames {
    let mut out = CustomOperationNames::default();
    for list in root.descendants().filter_map(AttributeList::cast) {
        let decorated = list.syntax().parent();
        for attr in list.attributes() {
            let Some(spelling) = attr
                .type_name()
                .and_then(|li| li.idents().last())
                .map(|t| id_text(&t).to_string())
            else {
                continue;
            };
            if may_be_custom_operation(&spelling) {
                attribute_custom_operations(decorated.as_ref(), &attr, &mut out);
            }
        }
    }
    out
}

/// Whether a signature file spells `CustomOperationAttribute` outside an
/// attribute — declaring a type of the name or an abbreviation of it, for
/// all a token can say.
pub(super) fn signature_renames_custom_operation(root: &SyntaxNode) -> bool {
    root.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
        .filter(|t| CUSTOM_OPERATION_SPELLINGS.contains(&&*id_text(t)))
        .any(|t| {
            !t.parent_ancestors()
                .any(|n| AttributeList::can_cast(n.kind()))
        })
}

/// Which of a type definition's members [`type_defn_custom_operations`]
/// reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Members {
    All,
    /// The static ones, and any whose staticness cannot be read.
    Static,
}

/// The custom-operation names the members of `defn` in `scope` may take:
/// for each attribute that may be `[<CustomOperation>]`, the name its
/// argument gives, or the name of the member it decorates where the
/// argument is missing or empty, and every string a named argument gives (a
/// join's condition word).
fn type_defn_custom_operations(
    defn: &TypeDefn,
    scope: Members,
    may_be_custom_operation: &dyn Fn(&str) -> bool,
) -> CustomOperationNames {
    let mut out = CustomOperationNames::default();
    for list in defn.syntax().descendants().filter_map(AttributeList::cast) {
        let member = decorated_member(&list);
        if scope == Members::Static
            && member.as_ref().is_some_and(|m| {
                !m.children_with_tokens()
                    .any(|t| t.kind() == SyntaxKind::STATIC_TOK)
            })
        {
            continue;
        }
        for attr in list.attributes() {
            let Some(spelling) = attr
                .type_name()
                .and_then(|li| li.idents().last())
                .map(|t| id_text(&t).to_string())
            else {
                continue;
            };
            if may_be_custom_operation(&spelling) {
                attribute_custom_operations(member.as_ref(), &attr, &mut out);
            }
        }
    }
    out
}

/// The member an attribute list decorates, if it decorates one.
fn decorated_member(list: &AttributeList) -> Option<SyntaxNode> {
    list.syntax()
        .ancestors()
        .take_while(|n| n.kind() != SyntaxKind::TYPE_DEFN)
        .find(|n| {
            matches!(
                n.kind(),
                SyntaxKind::MEMBER_DEFN
                    | SyntaxKind::ABSTRACT_SLOT
                    | SyntaxKind::GET_SET_MEMBER
                    | SyntaxKind::AUTO_PROPERTY
            )
        })
}

/// What an attribute argument is, as far as a custom operation's name goes.
enum Literal {
    Str(String),
    /// A literal that is no string: a flag's `true`.
    Other,
    /// Not a literal this reads: a `[<Literal>]` constant, a fold of
    /// literals (`"sk" + "ip"`), a string with an escape.
    Unread,
}

fn literal(e: &Expr) -> Literal {
    match e {
        Expr::Paren(p) => p.inner().as_ref().map_or(Literal::Unread, literal),
        Expr::Const(c) => {
            let tokens: Vec<SyntaxToken> = c
                .syntax()
                .descendants_with_tokens()
                .filter_map(|el| el.into_token())
                .filter(|t| !t.kind().is_trivia())
                .collect();
            match tokens.as_slice() {
                [t] => match string_literal_text(t) {
                    Some(Some(s)) => Literal::Str(s),
                    Some(None) => Literal::Unread,
                    None => Literal::Other,
                },
                _ => Literal::Unread,
            }
        }
        _ => Literal::Unread,
    }
}

/// The names one attribute that may be `[<CustomOperation>]` supplies:
/// `CustomOperationAttribute`'s one positional argument names the
/// operation, or with none or an empty one the member it decorates does; a
/// named argument's string is a word a clause reads by name.
fn attribute_custom_operations(
    member: Option<&SyntaxNode>,
    attr: &Attribute,
    out: &mut CustomOperationNames,
) {
    let args: Vec<Expr> = match attr.arg() {
        None => Vec::new(),
        Some(Expr::Paren(p)) => match p.inner() {
            None => Vec::new(),
            Some(Expr::Tuple(t)) => t.elements().collect(),
            Some(e) => vec![e],
        },
        Some(e) => vec![e],
    };
    let mut positional = Vec::new();
    for arg in &args {
        match super::exprs::named_arg_shape(arg) {
            Some((_, value)) => match value.as_ref().map(literal) {
                Some(Literal::Str(s)) => {
                    out.names.insert(s);
                }
                Some(Literal::Other) => {}
                Some(Literal::Unread) | None => out.unlisted = true,
            },
            None => positional.push(arg),
        }
    }
    let named_by_member = match positional.as_slice() {
        [] => true,
        [only] => match literal(only) {
            Literal::Str(s) if s.is_empty() => true,
            Literal::Str(s) => {
                out.names.insert(s);
                false
            }
            Literal::Other | Literal::Unread => {
                out.unlisted = true;
                false
            }
        },
        _ => {
            out.unlisted = true;
            false
        }
    };
    if named_by_member {
        match member {
            Some(m) => out.names.extend(decorated_member_names(m)),
            None => out.unlisted = true,
        }
    }
    // A clause's `into` and a join's condition word are read by name too.
    out.names.insert("into".to_string());
    out.names.insert("on".to_string());
}

/// The names the member `member` may bear: every identifier it declares
/// outside an expression or an attribute — its name among them.
fn decorated_member_names(member: &SyntaxNode) -> BTreeSet<String> {
    member
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
        .filter(|t| {
            !t.parent_ancestors()
                .take_while(|n| n != member)
                .any(|n| Expr::can_cast(n.kind()) || AttributeList::can_cast(n.kind()))
        })
        .map(|t| id_text(&t).to_string())
        .collect()
}

/// The text of a string literal token: `Some(Some(text))` for one this reads,
/// `Some(None)` for one it cannot (an escape, say), `None` for a token that
/// is not a string literal.
fn string_literal_text(tok: &SyntaxToken) -> Option<Option<String>> {
    let text = tok.text();
    let body = match tok.kind() {
        SyntaxKind::STRING_LIT => text.strip_prefix('"').and_then(|t| t.strip_suffix('"')),
        SyntaxKind::VERBATIM_STRING_LIT => {
            text.strip_prefix("@\"").and_then(|t| t.strip_suffix('"'))
        }
        SyntaxKind::TRIPLE_STRING_LIT => text
            .strip_prefix("\"\"\"")
            .and_then(|t| t.strip_suffix("\"\"\"")),
        _ => return None,
    };
    Some(
        body.filter(|b| !b.contains(['\\', '"']))
            .map(str::to_string),
    )
}

/// Every bare identifier the translation of the expression whose body is
/// `comp` may read as a custom-operation clause by name alone: the head of
/// each clause, and the identifiers applied in its spine (a join's condition
/// word, `into`). Follows the clause positions of
/// `TryTranslateComputationExpression` — sequenced statements, a `let`'s body,
/// the branches of an `if`, a `match` or a `try`, a loop's body — and nothing
/// a clause applies to beyond its spine.
fn clause_identifiers(comp: &Expr, out: &mut Vec<SyntaxToken>) {
    match comp {
        Expr::Sequential(e) => {
            for s in e.statements() {
                clause_identifiers(&s, out);
            }
        }
        Expr::LetOrUse(e) => {
            if let Some(body) = e.body() {
                clause_identifiers(&body, out);
            }
        }
        Expr::IfThenElse(e) => {
            for branch in [e.then_branch(), e.else_branch()].into_iter().flatten() {
                clause_identifiers(&branch, out);
            }
        }
        Expr::Match(e) => {
            for c in e.clauses() {
                if let Some(r) = c.result() {
                    clause_identifiers(&r, out);
                }
            }
        }
        Expr::MatchBang(e) => {
            for c in e.clauses() {
                if let Some(r) = c.result() {
                    clause_identifiers(&r, out);
                }
            }
        }
        Expr::Try(e) => {
            if let Some(t) = e.try_expr() {
                clause_identifiers(&t, out);
            }
            for c in e.with_clauses() {
                if let Some(r) = c.result() {
                    clause_identifiers(&r, out);
                }
            }
            if let Some(f) = e.finally_expr() {
                clause_identifiers(&f, out);
            }
        }
        Expr::While(e) => {
            if let Some(b) = e.body() {
                clause_identifiers(&b, out);
            }
        }
        Expr::WhileBang(e) => {
            if let Some(b) = e.body() {
                clause_identifiers(&b, out);
            }
        }
        Expr::ForEach(e) => {
            if let Some(b) = e.body() {
                clause_identifiers(&b, out);
            }
        }
        Expr::For(e) => {
            if let Some(b) = e.body() {
                clause_identifiers(&b, out);
            }
        }
        _ => clause_spine(comp, out),
    }
}

/// The identifiers of one clause's application spine the translation reads
/// by name: its head (`StripApps`), an `into` it applies (`IntoSuffix`),
/// and — in a join, whose sources a `JoinIn` separates — the condition word
/// it applies (`OnExpr`), which the operation names as it likes. An argument
/// is otherwise an ordinary expression.
fn clause_spine(e: &Expr, out: &mut Vec<SyntaxToken>) {
    let mut args = Vec::new();
    let mut joins = false;
    let mut head = e.clone();
    loop {
        match head {
            Expr::App(app) if !app.is_infix() => {
                if let Some(Expr::Ident(i)) = app.arg() {
                    args.extend(i.ident());
                }
                match app.func() {
                    Some(f) => head = f,
                    None => break,
                }
            }
            Expr::JoinIn(j) => {
                joins = true;
                match j.lhs() {
                    Some(l) => head = l,
                    None => break,
                }
            }
            Expr::Ident(i) => {
                out.extend(i.ident());
                break;
            }
            Expr::LongIdent(li) => {
                if let Some(li) = li.long_ident() {
                    let segs: Vec<SyntaxToken> = li.idents().collect();
                    if let [only] = segs.as_slice() {
                        out.push(only.clone());
                    }
                }
                break;
            }
            _ => break,
        }
    }
    out.extend(args.into_iter().filter(|t| joins || &*id_text(t) == "into"));
}

/// The token of the name an expression applies or constructs from, for
/// `T(…)`, `T<…>(…)` and `new T(…)`.
fn constructed_name(rhs: &Expr) -> Option<SyntaxToken> {
    match rhs {
        Expr::Paren(p) => p.inner().as_ref().and_then(constructed_name),
        Expr::App(app) if !app.is_infix() => match app.func()? {
            Expr::Ident(i) => i.ident(),
            Expr::TypeApp(t) => match t.expr()? {
                Expr::Ident(i) => i.ident(),
                _ => None,
            },
            _ => None,
        },
        Expr::New(n) => type_head(&n.target_type()?),
        _ => None,
    }
}

/// The last segment of a named type's head, not of its arguments:
/// `B` in `A.B<Arg>`.
fn type_head(ty: &Type) -> Option<SyntaxToken> {
    match ty {
        Type::LongIdent(t) => t.long_ident()?.idents().last(),
        Type::App(t) => type_head(&t.type_name()?),
        _ => None,
    }
}

/// The identifier token at exactly `range` under `root`.
fn ident_at(root: &SyntaxNode, range: rowan::TextRange) -> Option<SyntaxToken> {
    root.token_at_offset(range.start())
        .find(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text_range() == range)
}

impl Resolver<'_> {
    /// Enter a computation expression whose body the walk is about to
    /// resolve (module docs): mark every name its builder's custom
    /// operations may take, and decline the clause heads among them. Returns
    /// whether a frame was pushed, for [`Self::leave_computation_expression`].
    pub(super) fn enter_computation_expression(&mut self, ce: &ComputationExpr) -> bool {
        let ops = self.builder_custom_operations(ce);
        let (names, cause) = match ops {
            CeOps::Known(names) => (names, DeclineCause::CustomOperationMayTake),
            CeOps::Unknown => (
                ce.syntax()
                    .descendants_with_tokens()
                    .filter_map(|el| el.into_token())
                    .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
                    .map(|t| id_text(&t).to_string())
                    .collect(),
                DeclineCause::CustomOperationsUnknown,
            ),
        };
        if names.is_empty() {
            return false;
        }
        if let Some(inner) = ce.inner() {
            let mut heads = Vec::new();
            clause_identifiers(&inner, &mut heads);
            for tok in heads {
                if names.contains(&*id_text(&tok)) {
                    self.custom_operation_clauses
                        .insert(tok.text_range(), cause);
                }
            }
        }
        let pos = u32::from(ce.syntax().text_range().start());
        let generation = self.open_generation;
        let entries = names
            .into_iter()
            .map(|name| {
                let mut mark =
                    ScopeEntry::contributor_mark(name, MarkReach::Expression, generation, pos);
                mark.decline = Some(cause);
                mark
            })
            .collect();
        self.scopes.push(Frame { entries });
        true
    }

    /// Leave a computation expression [`Self::enter_computation_expression`]
    /// entered.
    pub(super) fn leave_computation_expression(&mut self, pushed: bool) {
        if pushed {
            self.scopes.pop();
        }
    }

    /// Why a bare use at `tok` declines because a custom operation takes it
    /// by name alone, if one may.
    pub(super) fn custom_operation_clause_decline(
        &self,
        tok: &SyntaxToken,
    ) -> Option<DeclineCause> {
        self.custom_operation_clauses
            .get(&tok.text_range())
            .copied()
    }

    /// The names the custom operations of `ce`'s builder may take.
    fn builder_custom_operations(&self, ce: &ComputationExpr) -> CeOps {
        // A `{ … }` that no expression applies is a sequence expression, and
        // so is one a function applies (`seq { … }`): neither has a builder.
        let Some(app) = ce
            .syntax()
            .parent()
            .and_then(borzoi_cst::syntax::AppExpr::cast)
        else {
            return CeOps::Known(BTreeSet::new());
        };
        if app.is_infix() || app.arg().is_none_or(|a| a.syntax() != ce.syntax()) {
            return CeOps::Known(BTreeSet::new());
        }
        let Some(func) = app.func() else {
            return CeOps::Unknown;
        };
        let resolution = match &func {
            Expr::Ident(i) => i
                .ident()
                .and_then(|t| self.resolutions.get(&t.text_range()).copied()),
            Expr::LongIdent(li) => li.long_ident().and_then(|li| {
                let last = li.idents().last()?;
                self.resolutions
                    .get(&last.text_range())
                    .or_else(|| self.resolutions.get(&li.syntax().text_range()))
                    .copied()
            }),
            _ => None,
        };
        let root = ce.syntax().ancestors().last().expect("a node has a root");
        let builder_type_ops = match resolution {
            Some(Resolution::Member { parent, idx }) => {
                match self.assemblies.builder_value(parent, idx) {
                    BuilderValue::Function => return CeOps::Known(BTreeSet::new()),
                    BuilderValue::Of(handle) => {
                        CeOps::from_names(self.assemblies.type_custom_operations(handle))
                    }
                    BuilderValue::Unknown => CeOps::Unknown,
                }
            }
            Some(res @ (Resolution::Local(_) | Resolution::Item(_))) => {
                match self.same_file_def(res) {
                    Some(def) => self.project_builder_operations(def, &root),
                    None => CeOps::Unknown,
                }
            }
            _ => CeOps::Unknown,
        };
        let CeOps::Known(mut names) = builder_type_ops else {
            return CeOps::Unknown;
        };
        // Every extension, wherever it is declared and whatever it extends:
        // the project's augmentations, earlier files' and this one's.
        let project = &self.preceding.custom_operations;
        let own = &self.contributions.custom_operations;
        let assembly = self.assemblies.extension_custom_operations();
        if project.unlisted || own.unlisted || assembly.unlisted {
            return CeOps::Unknown;
        }
        names.extend(project.names.iter().cloned());
        names.extend(own.names.iter().cloned());
        names.extend(assembly.names.iter().cloned());
        CeOps::Known(names)
    }

    /// Note what the application whose function is `func` constructs, when
    /// it is `T` or `T<…>` naming a type of this file
    /// ([`Self::in_file_constructions`]). The type-position resolver decides
    /// which type the bare name denotes, as it does for an assembly type used
    /// as a constructor, behind the same guards against a value the value
    /// frame misses; a value in the frame takes the name outright. Must be
    /// asked after `func` is resolved.
    pub(super) fn note_in_file_construction(&mut self, func: &Expr) {
        let (tok, arity) = match func {
            Expr::Ident(i) => (i.ident(), 0),
            Expr::TypeApp(t) => match t.expr() {
                Some(Expr::Ident(i)) => (i.ident(), t.type_args().len()),
                _ => return,
            },
            _ => return,
        };
        let Some(tok) = tok else {
            return;
        };
        let name = id_text(&tok).to_string();
        if !self.own_type_simple_names.contains(&name)
            || !matches!(
                self.resolutions.get(&tok.text_range()),
                Some(Resolution::Deferred(_))
            )
            || self.latest_entry_exists(&name)
            || !self.bare_constructor_unshadowed(&name)
        {
            return;
        }
        if let super::types::TypePathResolution::InFileType(id) =
            self.decide_type_path(std::slice::from_ref(&name), arity)
        {
            self.in_file_constructions.insert(tok.text_range(), id);
        }
    }

    /// Whether an attribute written `spelling` may be `[<CustomOperation>]`:
    /// it is spelled so, or some file so far or a referenced assembly may
    /// rename it.
    pub(super) fn may_be_custom_operation(&self, spelling: &str) -> bool {
        let aliases = self.assemblies.custom_operation_aliases();
        CUSTOM_OPERATION_SPELLINGS.contains(&spelling)
            || aliases.certain
            || spelling_may_name(&aliases.possible, spelling)
            || self.preceding.custom_operation_alias_declared
            || self.contributions.declares_custom_operation_alias
    }

    /// The def a resolution into this file names.
    fn same_file_def(&self, res: Resolution) -> Option<DefId> {
        match res {
            Resolution::Local(id) => Some(id),
            Resolution::Item(id) => {
                let local = id.index().checked_sub(self.item_base as usize)?;
                self.items.get(local)?.def()
            }
            _ => None,
        }
    }

    /// The operations of a builder bound in this file as `let b = T(…)`,
    /// with `T` a type of this file.
    fn project_builder_operations(&self, def: DefId, root: &SyntaxNode) -> CeOps {
        let d = &self.defs[def.index()];
        if d.kind != (DefKind::Value { is_function: false }) {
            return CeOps::Unknown;
        }
        let Some(binder) = ident_at(root, d.range) else {
            return CeOps::Unknown;
        };
        let Some(binding) = binder.parent_ancestors().find_map(Binding::cast) else {
            return CeOps::Unknown;
        };
        let names_binder = matches!(binding.pat(), Some(Pat::Named(n))
            if n.ident().is_some_and(|t| t.text_range() == d.range));
        if !names_binder {
            return CeOps::Unknown;
        }
        let Some(ty) = binding.expr().as_ref().and_then(constructed_name) else {
            return CeOps::Unknown;
        };
        let constructed = self
            .in_file_constructions
            .get(&ty.text_range())
            .copied()
            .or_else(|| {
                // `new T(…)` names `T` in type position, which the walk resolves.
                self.resolutions
                    .get(&ty.text_range())
                    .and_then(|r| self.same_file_def(*r))
            });
        match constructed {
            Some(t) if self.defs[t.index()].kind == DefKind::Type => {
                self.project_type_operations(t, root, 0)
            }
            _ => CeOps::Unknown,
        }
    }

    /// The operations the type `type_def` of this file and its base types
    /// declare, `depth` levels up.
    fn project_type_operations(&self, type_def: DefId, root: &SyntaxNode, depth: usize) -> CeOps {
        if depth > 32 {
            return CeOps::Unknown;
        }
        let Some(name) = ident_at(root, self.defs[type_def.index()].range) else {
            return CeOps::Unknown;
        };
        let Some(defn) = name.parent_ancestors().find_map(TypeDefn::cast) else {
            return CeOps::Unknown;
        };
        let inherits: Vec<InheritMember> = defn
            .syntax()
            .descendants()
            .filter_map(InheritMember::cast)
            .filter(|i| i.syntax().ancestors().find_map(TypeDefn::cast).as_ref() == Some(&defn))
            .collect();
        // An abbreviation's operations are its target's, and a delegate's or
        // inline IL's are not source.
        if matches!(
            defn.repr(),
            Some(TypeDefnRepr::Abbrev(_) | TypeDefnRepr::Delegate(_) | TypeDefnRepr::InlineIl(_))
        ) {
            return CeOps::Unknown;
        }
        let own =
            type_defn_custom_operations(&defn, Members::All, &|s| self.may_be_custom_operation(s));
        if own.unlisted {
            return CeOps::Unknown;
        }
        let mut out = own.names;
        for inherit in inherits {
            let Some(base) = inherit.base_type().as_ref().and_then(type_head) else {
                return CeOps::Unknown;
            };
            let resolved = self.resolutions.get(&base.text_range()).copied();
            let ops = match (resolved, resolved.and_then(|r| self.same_file_def(r))) {
                (_, Some(t)) if self.defs[t.index()].kind == DefKind::Type => {
                    self.project_type_operations(t, root, depth + 1)
                }
                (Some(Resolution::Entity(h)), _) => {
                    CeOps::from_names(self.assemblies.type_custom_operations(h))
                }
                _ => CeOps::Unknown,
            };
            match ops {
                CeOps::Known(names) => out.extend(names),
                CeOps::Unknown => return CeOps::Unknown,
            }
        }
        CeOps::Known(out)
    }
}

#[cfg(test)]
mod tests {
    use borzoi_cst::parser::parse;

    use super::{CUSTOM_OPERATION_SPELLINGS, file_custom_operations};

    fn read(attribute: &str) -> crate::assembly_env::CustomOperationNames {
        let src = format!(
            "module M\ntype T() = class end\ntype T with\n    [<{attribute}>]\n    member _.Method(s: int, v: int) = s\n"
        );
        let parsed = parse(&src);
        file_custom_operations(&parsed.root, &|s| CUSTOM_OPERATION_SPELLINGS.contains(&s))
    }

    /// A referenced abbreviation whose target the projection could not read
    /// may be `CustomOperationAttribute`, and F# finds it for an attribute
    /// written without its `Attribute` suffix: `[<GridCO("foo")>]` names
    /// `GridCOAttribute`. The operation it may declare takes `foo` from the
    /// file's own `foo` inside the expression.
    #[test]
    fn an_unread_abbreviation_named_with_the_suffix_may_be_custom_operation() {
        use crate::assembly_env::from_views_tests::abbrev_marker;
        use crate::{AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, resolve_file};
        use borzoi_cst::syntax::{AstNode, ImplFile};

        let src = "module M\ntype B() =\n    member _.Yield(()) = 0\n    member _.Return(_: 'a) = 0\n    [<GridCO(\"foo\")>]\n    member _.Op(s: int, v: int) = s + v\nlet b = B()\nlet foo (x: int) = x\nlet probe = b { return foo 1 }\n";
        for (alias, declines) in [
            ("GridCOAttribute", true),
            ("GridCO", true),
            ("Unrelated", false),
        ] {
            let env = AssemblyEnv::from_entities(vec![abbrev_marker("Lib", &["Lib"], alias, None)]);
            let parsed = parse(src);
            assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
            let recovery = SyntaxRecovery::of(&parsed);
            let file = ImplFile::cast(parsed.root).expect("impl file");
            let rf = resolve_file(&file, &ProjectItems::default(), &env, &recovery);
            let at = src.rfind("foo 1").expect("the use");
            let range = rowan::TextRange::new(
                u32::try_from(at).unwrap().into(),
                u32::try_from(at + 3).unwrap().into(),
            );
            let committed = rf
                .resolution_at(range)
                .is_some_and(|r| matches!(r, Resolution::Local(_) | Resolution::Item(_)));
            assert_eq!(
                !committed,
                declines,
                "{alias}: {:?}",
                rf.resolution_at(range)
            );
        }
    }

    /// The same in an earlier file's signature, whose attributes are the ones
    /// later files see: its augmentation's operation extends the later
    /// file's builder.
    #[test]
    fn a_signature_attribute_written_without_the_suffix_may_be_custom_operation() {
        use crate::assembly_env::from_views_tests::abbrev_marker;
        use crate::{
            AssemblyEnv, ProjectFile, Resolution, SourceFile, SyntaxRecovery, qualified_names,
            resolve_project_files,
        };
        use borzoi_cst::parser::parse_sig;
        use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
        use std::path::PathBuf;

        let files = [
            (
                "Ext.fsi",
                "module Ext\ntype System.Object with\n    [<GridCO(\"foo\")>]\n    member Op: s: int * v: int -> int\n",
            ),
            (
                "Ext.fs",
                "module Ext\ntype System.Object with\n    member _.Op(s: int, v: int) = s + v\n",
            ),
            (
                "P.fs",
                "module P\ntype B() =\n    member _.Yield(()) = 0\n    member _.Return(_: 'a) = 0\nlet b = B()\nlet foo (x: int) = x\nlet probe = b { return foo 1 }\n",
            ),
        ];
        for (alias, declines) in [("GridCOAttribute", true), ("Unrelated", false)] {
            let srcs: Vec<SourceFile> = files
                .iter()
                .map(|(path, src)| {
                    if path.ends_with(".fsi") {
                        let p = parse_sig(src);
                        assert!(p.errors.is_empty(), "{:?}", p.errors);
                        SourceFile::Sig(SigFile::cast(p.root).expect("sig file"))
                    } else {
                        let p = parse(src);
                        assert!(p.errors.is_empty(), "{:?}", p.errors);
                        SourceFile::Impl(ImplFile::cast(p.root).expect("impl file"))
                    }
                })
                .collect();
            let paths: Vec<PathBuf> = files.iter().map(|(p, _)| PathBuf::from(p)).collect();
            let qnofs = qualified_names(&srcs, &paths);
            let project: Vec<ProjectFile> = srcs
                .into_iter()
                .zip(qnofs)
                .map(|(f, q)| ProjectFile::new(f, q, SyntaxRecovery::Unretained))
                .collect();
            let env = AssemblyEnv::from_entities(vec![abbrev_marker("Lib", &["Lib"], alias, None)]);
            let resolved = resolve_project_files(&project, &env);
            let src = files[2].1;
            let at = src.rfind("foo 1").expect("the use");
            let range = rowan::TextRange::new(
                u32::try_from(at).unwrap().into(),
                u32::try_from(at + 3).unwrap().into(),
            );
            let res = resolved.file(2).resolution_at(range);
            let committed =
                res.is_some_and(|r| matches!(r, Resolution::Local(_) | Resolution::Item(_)));
            assert_eq!(!committed, declines, "{alias}: {res:?}");
        }
    }

    /// An augmentation's operation is named by the attribute's string, a
    /// named argument's label is not a name, and an argument that is not a
    /// literal leaves the names unread.
    #[test]
    fn an_augmentation_names_its_operations_by_the_attribute() {
        let named = read("CustomOperation(\"op\", MaintainsVariableSpace = true)");
        assert!(!named.unlisted, "{named:?}");
        assert!(named.names.contains("op"), "{named:?}");
        assert!(!named.names.contains("Method"), "{named:?}");

        let by_method = read("CustomOperation");
        assert!(!by_method.unlisted, "{by_method:?}");
        assert!(by_method.names.contains("Method"), "{by_method:?}");

        let constant = read("CustomOperation(Names.Op)");
        assert!(constant.unlisted, "{constant:?}");

        let escaped = read("CustomOperation(\"o\\u0070\")");
        assert!(escaped.unlisted, "{escaped:?}");

        let folded = read("CustomOperation(\"sk\" + \"ip\")");
        assert!(folded.unlisted, "{folded:?}");

        // A named argument's string does not stand in for the name.
        let worded = read("CustomOperation(\"\", JoinConditionWord = \"equals\")");
        assert!(!worded.unlisted, "{worded:?}");
        assert!(worded.names.contains("Method"), "{worded:?}");
        assert!(worded.names.contains("equals"), "{worded:?}");

        let other = read("Obsolete(\"op\")");
        assert!(other.names.is_empty() && !other.unlisted, "{other:?}");
    }
}
