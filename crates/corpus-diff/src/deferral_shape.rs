//! What a deferral **no guard accounted for** is waiting on, read off the
//! syntax around it and what the resolver recorded beside it.
//!
//! A [`DeclineSite`] names the precedence-ladder guard
//! that declined a name, and sema records one only where a guard actually spoke
//! ([`ResolvedFile::decline_site`]). Most deferrals on real projects are not
//! that: they are names the resolver has **no model for at all** — a generic
//! parameter, a record field label, the member tail of `x.Length` — and no
//! guard is involved, so there is no site to record. Inventing one inside the
//! resolver would mean adding a recording to every place it *does not* look,
//! which is exactly the set nothing enumerates.
//!
//! So this is a classification by **what the resolver lacks**, decided where
//! the facts are: the oracle record's span, the syntax tree around its last
//! byte, and the resolutions sema recorded on the path it sits in. Each
//! [`DeferralShape`] names one missing model and says why no resolution exists
//! there; fixing a shape means building that model. A record that fits none is
//! left *unexplained* — that residue, not the shaped count, is the number the
//! census exists to drive down.
//!
//! The classification never reads what the oracle *says the symbol is*. That
//! would be a histogram by symptom: "FCS calls it a field" does not say whether
//! the resolver lacked a field model or a guard declined a field. Every test
//! here is on our own syntax and our own recorded answers, so a shape stays
//! true when the oracle's vocabulary changes, and moves when the resolver does.

use borzoi_cst::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use borzoi_sema::{DeclineSite, Resolution, ResolvedFile};
use rowan::{TextRange, TextSize};

/// What a deferral is attributed to: a guard that declined, a model the
/// resolver lacks, or neither.
///
/// One value rather than two options, because a deferral the resolver's guard
/// accounted for is never also classified by shape — the guard's site is the
/// specific claim, and a shape is only consulted where there is none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferralCause {
    /// A guard declined, and recorded itself at the record's range
    /// ([`ResolvedFile::decline_site`]).
    Guard(DeclineSite),
    /// No guard spoke; the resolver has no model for what sits here.
    Shape(DeferralShape),
    /// Neither: the census cannot say why this record deferred.
    Unexplained,
}

impl DeferralCause {
    /// The cause of the deferral at oracle record `record`: the guard's site
    /// when one recorded itself there, else the shape, else unexplained.
    pub fn of(root: &SyntaxNode, rf: &ResolvedFile, record: TextRange) -> DeferralCause {
        match rf.decline_site(record) {
            Some(site) => DeferralCause::Guard(site),
            None => match classify(root, rf, record) {
                Some(shape) => DeferralCause::Shape(shape),
                None => DeferralCause::Unexplained,
            },
        }
    }
}

/// Why no resolution exists at a deferral no guard recorded itself for: the
/// model the resolver lacks there. Closed, and iterated whole by the census
/// ([`Self::ALL`]) so a shape that did not occur still reports a zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeferralShape {
    /// The record sits in a **signature file**. Sema folds a `.fsi` into an
    /// inert slot carrying its screen and exported surface, and records no
    /// resolution of its own inside it.
    SignatureFile,
    /// The record sits inside an **object expression's** member, binding or
    /// interface-implementation block. Those bodies introduce their own `this`
    /// and parameter binders and are not walked at all; only the object type and
    /// its constructor argument are.
    ObjectExpressionBody,
    /// The record sits in an **attribute's arguments** (`[<A(Flags.X)>]`,
    /// `[<A(Name = v)>]`). Only the attribute's type name is resolved.
    AttributeArgument,
    /// A type in an **abstract slot**, a class **`val` field** or a **member
    /// signature** (`abstract Apply : T -> U`, `val Next : T`). The resolver
    /// walks member bodies but not these declarations' types.
    MemberSignature,
    /// A **generic type parameter** (`'a`, `^T`) the resolver left
    /// unanswered. It binds a parameter only to an explicit `<'a>` declaration
    /// of an enclosing type, member or `let` in scope; an implicitly declared
    /// one (`(x : 'a)` with no `<'a>`) and a constraint's subject (`when 'a :
    /// equality`) are not modelled.
    TypeParameter,
    /// A **unit-of-measure** name (`1<pos>`). Measures are not resolved.
    Measure,
    /// A segment of a `namespace`, `module` or `open` **declaration path**, or a
    /// module abbreviation's target. Namespaces and modules are not resolution
    /// targets.
    DeclarationPath,
    /// The name a **member definition** defines or overrides (`member
    /// this.Apply`, `override _.ToString`, `interface I with member …`). FCS
    /// reports the slot it implements; the resolver does not model slots.
    MemberDefinitionName,
    /// A **field label**: a record field in `{ F = … }` / `{ r with F = … }` /
    /// `{ F = p }`, or a union field named in `Case (field = p)`. Labels are
    /// not resolved.
    FieldLabel,
    /// The name of a **named argument** (`M (Name = v)`, an attribute's
    /// `Name = v`): the resolver deliberately leaves an argument that may be
    /// named unrecorded, since it cannot tell a setter from an equality test.
    NamedArgument,
    /// A **symbolic operator** (`&&`, `|>`, `::`, `..`, `&`). Operators are not
    /// resolved.
    Operator,
    /// A **qualifier** — a segment of a dotted path before the one the record
    /// names, reached without a value on its left (`List` in `List.map`,
    /// `Color` in `Color.Red`). The resolver defers every qualifier segment,
    /// even when the whole path resolves: it has no module-, namespace- or
    /// type-as-definition model.
    Qualifier,
    /// A member or field reached through a **value**: some prefix of the path
    /// resolved to a local, a module-level value or a referenced static member,
    /// and the record lies past it (`s.Length`, `state.Nodes.Count`). Which
    /// member that is depends on the receiver's type, which is inference's
    /// question, not name resolution's.
    MemberAccessOnValue,
    /// A member reached through a receiver that is not a name path at all
    /// (`f().ToString`, `(x : T).Field`). As [`Self::MemberAccessOnValue`], it
    /// needs the receiver's type.
    MemberAccessOnExpression,
    /// An **indexer** (`xs.[i]`): the `Item` property of the receiver's type.
    Indexer,
    /// A member of a referenced-assembly **type** the head resolved to, which
    /// the resolver left unselected (`Path.Combine`, `String.Join`): an
    /// overloaded method, an instance member named through its type, or one the
    /// projection does not carry.
    AssemblyTypeMember,
}

impl DeferralShape {
    /// Every variant, for a census that must emit a count per shape including
    /// the zeros.
    pub const ALL: &'static [DeferralShape] = &[
        DeferralShape::SignatureFile,
        DeferralShape::ObjectExpressionBody,
        DeferralShape::AttributeArgument,
        DeferralShape::MemberSignature,
        DeferralShape::TypeParameter,
        DeferralShape::Measure,
        DeferralShape::DeclarationPath,
        DeferralShape::MemberDefinitionName,
        DeferralShape::FieldLabel,
        DeferralShape::NamedArgument,
        DeferralShape::Operator,
        DeferralShape::Qualifier,
        DeferralShape::MemberAccessOnValue,
        DeferralShape::MemberAccessOnExpression,
        DeferralShape::Indexer,
        DeferralShape::AssemblyTypeMember,
    ];

    /// A stable snake-case label: a census key must mean the same thing in
    /// every run of a series.
    pub fn label(self) -> &'static str {
        match self {
            DeferralShape::SignatureFile => "signature_file",
            DeferralShape::ObjectExpressionBody => "object_expression_body",
            DeferralShape::AttributeArgument => "attribute_argument",
            DeferralShape::MemberSignature => "member_signature",
            DeferralShape::TypeParameter => "type_parameter",
            DeferralShape::Measure => "measure",
            DeferralShape::DeclarationPath => "declaration_path",
            DeferralShape::MemberDefinitionName => "member_definition_name",
            DeferralShape::FieldLabel => "field_label",
            DeferralShape::NamedArgument => "named_argument",
            DeferralShape::Operator => "operator",
            DeferralShape::Qualifier => "qualifier",
            DeferralShape::MemberAccessOnValue => "member_access_on_value",
            DeferralShape::MemberAccessOnExpression => "member_access_on_expression",
            DeferralShape::Indexer => "indexer",
            DeferralShape::AssemblyTypeMember => "assembly_type_member",
        }
    }
}

/// The shape of the deferral at oracle record `record` in the file rooted at
/// `root`, or `None` where none of the models above is what is missing.
///
/// Read at the record's **last byte**, the byte the comparator probes
/// (`served_answer`): that is the token the record names.
pub fn classify(root: &SyntaxNode, rf: &ResolvedFile, record: TextRange) -> Option<DeferralShape> {
    if root.kind() == SyntaxKind::SIG_FILE {
        return Some(DeferralShape::SignatureFile);
    }
    if record.is_empty() {
        return None;
    }
    let probe = record.end() - TextSize::from(1);
    let token = root.token_at_offset(probe).right_biased()?;
    if let Some(region) = unwalked_region(&token) {
        return Some(region);
    }
    let parent = token.parent()?;
    match parent.kind() {
        SyntaxKind::TYPAR_DECL | SyntaxKind::VAR_TYPE | SyntaxKind::TYPAR_EXPR => {
            return Some(DeferralShape::TypeParameter);
        }
        SyntaxKind::DOT_INDEXED_GET_EXPR => return Some(DeferralShape::Indexer),
        SyntaxKind::NAME_PAT_PAIR => return Some(DeferralShape::FieldLabel),
        _ => {}
    }
    if is_operator(&token) {
        return Some(DeferralShape::Operator);
    }
    if parent.kind() != SyntaxKind::LONG_IDENT {
        return single_name_shape(rf, &token, &parent);
    }
    let path = parent;
    let holder = path.parent()?;
    match holder.kind() {
        SyntaxKind::MODULE_OR_NAMESPACE
        | SyntaxKind::OPEN_DECL
        | SyntaxKind::NESTED_MODULE_DECL
        | SyntaxKind::MODULE_ABBREV_DECL => return Some(DeferralShape::DeclarationPath),
        SyntaxKind::MEASURE_NAMED => return Some(DeferralShape::Measure),
        SyntaxKind::RECORD_FIELD | SyntaxKind::RECORD_PAT_FIELD => {
            return Some(DeferralShape::FieldLabel);
        }
        SyntaxKind::DOT_GET_EXPR => return Some(DeferralShape::MemberAccessOnExpression),
        SyntaxKind::LONG_IDENT_PAT if is_member_head(&holder) => {
            return Some(DeferralShape::MemberDefinitionName);
        }
        _ => {}
    }
    let segments: Vec<SyntaxToken> = path
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
        .collect();
    let (Some(head), Some(last)) = (segments.first(), segments.last()) else {
        return None;
    };
    // The rightmost committed answer on the path that ends before the record
    // does: the receiver the record is a member of. The resolver keys a whole
    // path's answer by the prefix it spans and an assembly type's by its own
    // segment, so both are asked, the longer first.
    let receiver = segments
        .iter()
        .rev()
        .filter(|seg| seg.text_range().end() < record.end())
        .flat_map(|seg| {
            [
                TextRange::new(head.text_range().start(), seg.text_range().end()),
                seg.text_range(),
            ]
        })
        .find_map(|range| match rf.committed_resolution_at(range)? {
            Resolution::Deferred(_) | Resolution::Unresolved => None,
            committed => Some(committed),
        });
    match receiver {
        Some(Resolution::Local(_) | Resolution::Item(_) | Resolution::Member { .. }) => {
            return Some(DeferralShape::MemberAccessOnValue);
        }
        Some(Resolution::Entity(_)) => return Some(DeferralShape::AssemblyTypeMember),
        Some(Resolution::Deferred(_) | Resolution::Unresolved) | None => {}
    }
    if record.end() < last.text_range().end() {
        return Some(DeferralShape::Qualifier);
    }
    None
}

/// A record whose last token is not on a dotted path.
fn single_name_shape(
    rf: &ResolvedFile,
    token: &SyntaxToken,
    parent: &SyntaxNode,
) -> Option<DeferralShape> {
    // The resolver leaves a maybe-named argument's label unrecorded; one it
    // read as an equality operand is recorded, and is not this shape.
    if parent.kind() == SyntaxKind::IDENT_EXPR
        && rf.committed_resolution_at(token.text_range()).is_none()
        && is_named_argument_name(parent)
    {
        return Some(DeferralShape::NamedArgument);
    }
    None
}

/// The innermost syntax region around `token` that the resolver does not walk
/// at all, if there is one:
///
/// - an `OBJ_EXPR`'s members, bindings and interface implementations — every
///   child but the `NEW_EXPR` carrying the object type and constructor
///   argument, which are resolved;
/// - an `ATTRIBUTE`'s arguments — every child but the attribute's own type
///   name, which is;
/// - an abstract slot, a `val` field or a member signature, whose types are
///   not.
fn unwalked_region(token: &SyntaxToken) -> Option<DeferralShape> {
    let mut child: Option<SyntaxNode> = None;
    for node in token.parent_ancestors() {
        let child_kind = child.as_ref().map(SyntaxNode::kind);
        match node.kind() {
            SyntaxKind::OBJ_EXPR if child_kind.is_some_and(|k| k != SyntaxKind::NEW_EXPR) => {
                return Some(DeferralShape::ObjectExpressionBody);
            }
            SyntaxKind::ATTRIBUTE
                if child_kind.is_some_and(|k| {
                    k != SyntaxKind::LONG_IDENT && k != SyntaxKind::ATTRIBUTE_TARGET
                }) =>
            {
                return Some(DeferralShape::AttributeArgument);
            }
            SyntaxKind::ABSTRACT_SLOT | SyntaxKind::VAL_FIELD | SyntaxKind::MEMBER_SIG => {
                return Some(DeferralShape::MemberSignature);
            }
            _ => {}
        }
        child = Some(node);
    }
    None
}

/// A symbolic operator token: anything that is not an identifier, or an
/// identifier spelled with operator characters (`&&`, `|>`).
fn is_operator(token: &SyntaxToken) -> bool {
    match token.kind() {
        SyntaxKind::IDENT_TOK => token
            .text()
            .chars()
            .next()
            .is_some_and(|c| !(c.is_alphanumeric() || c == '_' || c == '`')),
        SyntaxKind::COLON_COLON_TOK
        | SyntaxKind::DOT_DOT_TOK
        | SyntaxKind::AMP_TOK
        | SyntaxKind::GREATER_TOK
        | SyntaxKind::LESS_TOK
        | SyntaxKind::STAR_TOK
        | SyntaxKind::HAT_TOK => true,
        _ => false,
    }
}

/// Whether `pat` (a `LONG_IDENT_PAT`) is the head pattern of a member
/// definition's binding — the `this.Apply` of `member this.Apply x = …`.
fn is_member_head(pat: &SyntaxNode) -> bool {
    let Some(binding) = pat.parent() else {
        return false;
    };
    let head_of_binding = binding.kind() == SyntaxKind::BINDING
        && binding.children().next().is_some_and(|first| &first == pat);
    let owner = if head_of_binding {
        binding.parent()
    } else {
        Some(binding)
    };
    owner.is_some_and(|o| {
        matches!(
            o.kind(),
            SyntaxKind::MEMBER_DEFN | SyntaxKind::GET_SET_MEMBER | SyntaxKind::ABSTRACT_SLOT
        )
    })
}

/// Whether `ident_expr` is the left operand of an `=` that is itself an
/// argument of a call or attribute: `Name` in `M (Name = v)` or
/// `[<A(Name = v)>]`.
fn is_named_argument_name(ident_expr: &SyntaxNode) -> bool {
    let Some(infix) = ident_expr.parent() else {
        return false;
    };
    let mut operands = infix.children();
    if infix.kind() != SyntaxKind::INFIX_APP_EXPR
        || operands.next().as_ref() != Some(ident_expr)
        || !operands
            .next()
            .is_some_and(|op| op.text().to_string().trim() == "=")
    {
        return false;
    }
    // `Name = v` parses as an application of the infix `Name =` to `v`.
    let Some(arg) = infix.parent() else {
        return false;
    };
    if arg.kind() != SyntaxKind::APP_EXPR || arg.children().next().as_ref() != Some(&infix) {
        return false;
    }
    let mut arg = arg;
    let mut parenthesised = false;
    while let Some(up) = arg.parent() {
        match up.kind() {
            SyntaxKind::TUPLE_EXPR => arg = up,
            SyntaxKind::PAREN_EXPR => {
                parenthesised = true;
                arg = up;
            }
            SyntaxKind::APP_EXPR | SyntaxKind::NEW_EXPR | SyntaxKind::ATTRIBUTE => {
                return parenthesised;
            }
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use borzoi::handlers::served_resolution_with_range;
    use borzoi_assembly::EntityKind;
    use borzoi_cst::parser::{parse, parse_sig};
    use borzoi_cst::syntax::{AstNode, ImplFile};
    use borzoi_sema::{AssemblyEnv, ProjectItems, SyntaxRecovery, resolve_file};

    /// One project file the rows below share: every shape has a use site in
    /// it, and nothing in it needs a referenced assembly but `Demo.Widget`.
    const FIXTURE: &str = "\
module Fixture

type N = N

type R = { Field : N }

type U<'a> =
    | Case of value : 'a

type I =
    abstract Apply : R -> N

type C () =
    static member Make (name : N) = name
    member this.Run () = this.Run ()

module Inner =
    let v = N

let w = Demo.Widget.Missing
let k (x : 'b) = x
let mk<'c when 'c : equality> (x : 'c) = x

open Inner

[<Measure>] type pos

[<Obsolete(Inner.v)>]
let f (r : R) = r.Field
let g () = { Field = N }
let h (x : U<N>) =
    match x with
    | Case (value = v) -> v
let o =
    { new I with
        member _.Apply r = r.Field }
let q = Inner.v
let s = 1<pos>
let n = C.Make (name = N)
let e = (g ()).Field
let arr = [| N |]
let i = arr.[0]
let p = g () |> f
";

    /// The env the fixture's one assembly-rooted path reaches into.
    fn env() -> AssemblyEnv {
        AssemblyEnv::from_entities(vec![crate::tests::fixture_entity(
            "Widget",
            EntityKind::Class,
            0,
            &["Value"],
        )])
    }

    /// The byte range of `record` inside the one occurrence of `context` in
    /// `src`: the context pins the site, the record is the span FCS reports.
    fn range_in(src: &str, context: &str, record: &str) -> TextRange {
        let mut at = src.match_indices(context);
        let (start, _) = at
            .next()
            .unwrap_or_else(|| panic!("{context:?} is not in the fixture"));
        assert!(at.next().is_none(), "{context:?} is not unique");
        let offset = context
            .find(record)
            .unwrap_or_else(|| panic!("{record:?} is not in {context:?}"));
        let start = start + offset;
        TextRange::new(
            TextSize::try_from(start).expect("fits"),
            TextSize::try_from(start + record.len()).expect("fits"),
        )
    }

    /// Every row is an oracle-shaped record — the span FCS would report — at
    /// which the LSP serves no claim, with the cause the census must give it.
    /// A row is checked to be a real deferral first, so a resolver change that
    /// starts answering one makes the row fail rather than pass vacuously.
    const ROWS: &[(&str, &str, DeferralShape)] = &[
        ("module Fixture", "Fixture", DeferralShape::DeclarationPath),
        ("open Inner", "Inner", DeferralShape::DeclarationPath),
        ("(x : 'b)", "'b", DeferralShape::TypeParameter),
        ("'c : equality", "'c", DeferralShape::TypeParameter),
        ("Apply : R", "R", DeferralShape::MemberSignature),
        (
            "Obsolete(Inner.v)",
            "Inner",
            DeferralShape::AttributeArgument,
        ),
        (
            "= r.Field\nlet g",
            "r.Field",
            DeferralShape::MemberAccessOnValue,
        ),
        ("{ Field = N }", "Field", DeferralShape::FieldLabel),
        ("(value = v)", "value", DeferralShape::FieldLabel),
        (
            "Apply r = r.Field",
            "r.Field",
            DeferralShape::ObjectExpressionBody,
        ),
        ("_.Apply", "Apply", DeferralShape::ObjectExpressionBody),
        ("q = Inner.v", "Inner", DeferralShape::Qualifier),
        ("1<pos>", "pos", DeferralShape::Measure),
        ("(name = N)", "name", DeferralShape::NamedArgument),
        (
            "(g ()).Field",
            "Field",
            DeferralShape::MemberAccessOnExpression,
        ),
        ("arr.[0]", "arr.[0]", DeferralShape::Indexer),
        ("|>", "|>", DeferralShape::Operator),
        (
            "member this.Run",
            "Run",
            DeferralShape::MemberDefinitionName,
        ),
        ("= this.Run", "this.Run", DeferralShape::MemberAccessOnValue),
        (
            "Demo.Widget.Missing",
            "Demo.Widget.Missing",
            DeferralShape::AssemblyTypeMember,
        ),
    ];

    #[test]
    fn every_shape_is_named_at_its_site() {
        let p = parse(FIXTURE);
        assert!(p.errors.is_empty(), "fixture parse errors: {:?}", p.errors);
        let recovery = SyntaxRecovery::of(&p);
        let root = p.root.clone();
        let file = ImplFile::cast(p.root).expect("impl file");
        let rf = resolve_file(&file, &ProjectItems::default(), &env(), &recovery);
        let mut wrong = Vec::new();
        for &(context, needle, shape) in ROWS {
            let record = range_in(FIXTURE, context, needle);
            let probe = usize::from(record.end()) - 1;
            if let Some((_, res @ (Resolution::Local(_) | Resolution::Item(_))))
            | Some((_, res @ (Resolution::Entity(_) | Resolution::Member { .. }))) =
                served_resolution_with_range(&rf, None, probe)
            {
                wrong.push(format!(
                    "{needle:?} in {context:?}: served {res:?}, not a deferral"
                ));
                continue;
            }
            let got = DeferralCause::of(&root, &rf, record);
            if got != DeferralCause::Shape(shape) {
                wrong.push(format!(
                    "{needle:?} in {context:?}: {got:?}, expected {shape:?}"
                ));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The rows above are the obligation, so every shape must have one: a
    /// shape no row reaches is a classification nothing checks.
    #[test]
    fn every_shape_has_a_row() {
        let missing: Vec<_> = DeferralShape::ALL
            .iter()
            .filter(|s| {
                **s != DeferralShape::SignatureFile && !ROWS.iter().any(|(_, _, r)| r == *s)
            })
            .collect();
        assert!(missing.is_empty(), "shapes with no row: {missing:?}");
    }

    /// A signature file is classified whole: sema records nothing inside one,
    /// whatever the syntax there.
    #[test]
    fn everything_in_a_signature_file_is_its_shape() {
        let sig = parse_sig("module Fixture\nval f : int -> int\n");
        let p = parse("module Fixture\n");
        let recovery = SyntaxRecovery::of(&p);
        let file = ImplFile::cast(p.root).expect("impl file");
        let rf = resolve_file(&file, &ProjectItems::default(), &env(), &recovery);
        let src = sig.root.text().to_string();
        for needle in ["Fixture", "f :", "int ->"] {
            assert_eq!(
                DeferralCause::of(&sig.root, &rf, range_in(&src, needle, needle)),
                DeferralCause::Shape(DeferralShape::SignatureFile),
                "{needle}"
            );
        }
    }
}
