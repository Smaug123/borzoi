//! The definition model: a name introduced by F# source, and what sort of
//! binder it is.

use std::borrow::Cow;

use borzoi_assembly::op_name::{compile_op_name, is_operator_spelling, prefix_op_name};
use borzoi_cst::syntax::SyntaxToken;
use borzoi_cst::syntax::TextRange;

use crate::AssemblyEnv;
use crate::names::{Name, NameTable};

/// A name introduced by F# source — a `let`-bound value or function, a
/// parameter, a type, a case, a member, a type parameter.
///
/// It holds its name twice over, against its env's name table: the
/// *logical* name the walk keys it by (backticks stripped, a bare operator
/// compiled, as FCS's `idText`), and how the source spelled that name, which
/// is what a consumer shows ([`Def::spelling`]). Like every id, both mean
/// something only beside the env the file was resolved against.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Def {
    /// The logical name: [`id_text`] of the spelling.
    name: Name,
    /// How the source spelled [`Self::name`].
    spelling: Spelling,
    /// Source range of the defining identifier token, as a byte range into
    /// the file (a [`TextRange`]).
    pub range: TextRange,
    pub kind: DefKind,
    /// `true` for a *provisional* maybe-var binder: a nullary single-segment
    /// `LongIdent` head reached below a `let` head or in a parameter / match
    /// pattern (FCS's `mkSynPatMaybeVar`). The parser routes lowercase idents
    /// to `Named` and upper-case ones to `LongIdent`, so these are exactly the
    /// constructor-shaped names (`None`, `Empty`) that bind *only if* they do
    /// not resolve to a nullary constructor / literal. Binder extraction cannot
    /// decide that — it has no resolution environment — so it flags them and
    /// the resolution stage drops the ones that are really constructor
    /// references. Definite binders (`Named` leaves, the direct `let` head)
    /// are never provisional.
    pub provisional: bool,
}

/// How a [`Def`]'s source spelled its logical name.
///
/// Almost every definition is spelled as its logical name or as that name in
/// double backticks, so neither of those costs a string. The rest keep their
/// spelling: an operator binder (`+`, whose name is `op_Addition`) and an
/// active-pattern recognizer (`(|Even|Odd|)`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Spelling {
    /// The logical name's text.
    AsName,
    /// The logical name's text in double backticks.
    Quoted,
    /// Anything else, as written.
    Verbatim(Box<str>),
}

impl Spelling {
    /// How `spelling` writes `logical`, its [`ident_name`].
    fn of(spelling: &str, logical: &str) -> Self {
        if spelling == logical {
            Spelling::AsName
        } else if spelling.len() == logical.len() + 4
            && spelling.starts_with("``")
            && spelling.ends_with("``")
            && &spelling[2..spelling.len() - 2] == logical
        {
            Spelling::Quoted
        } else {
            Spelling::Verbatim(spelling.into())
        }
    }
}

/// What sort of binder a [`Def`] is.
///
/// `Module` (from the module/namespace walker) arrives with its producer in a
/// later stage, alongside value mutability (a binding-level property the
/// headPat alone does not carry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefKind {
    /// A value or function bound by a `let` head: `is_function` is `true` for
    /// the curried function form (`let f x = …`) and `false` for a plain
    /// value (`let x = …`).
    Value { is_function: bool },
    /// A parameter bound by a lambda or function-binding argument pattern.
    Parameter,
    /// A local bound by a refutable pattern that is not a parameter — the
    /// binders of a `match` clause pattern (`match e with x -> …`). Distinct
    /// from [`DefKind::Parameter`] because a future consumer (completion,
    /// hover) will want to label the two differently, mirroring FCS treating
    /// both as locals but a `match` binder not being a formal parameter.
    PatternLocal,
    /// A type introduced by a `type` definition — an abbreviation, record,
    /// union, enum, or class (`type T = …`). The bound name occupies F#'s
    /// *type* namespace, disjoint from the value namespace the other kinds
    /// live in, so a type [`Def`] is referenced only from type-syntactic
    /// positions (annotations, abbreviation right-hand sides, field types,
    /// cast/`new`/type-application targets), never from an expression-level
    /// value name use.
    Type,
    /// A union case introduced by `type T = A | B of …`. Interned when the type
    /// is walked, like [`DefKind::Type`], but it lives in the *value* namespace:
    /// a case is used as a constructor (`B 3`) and as a pattern head
    /// (`match x with B n -> …`), so it is added to the value scope and found by
    /// the ordinary value lookup. Resolving a constructor-shaped name (an
    /// upper-case single segment) to a case is what lets a nullary pattern head
    /// like `None` / `Red` be told apart from a fresh binder (the provisional
    /// maybe-var, [`Def::provisional`]).
    UnionCase,
    /// A constructor introduced by an `exception E of …` definition (or its
    /// abbreviation `exception Alias = E`). Like [`DefKind::UnionCase`] it lives
    /// in the *value* namespace — used as a constructor (`raise (E x)`, `E "x"`)
    /// and as a pattern head (`try … with E m -> …`) — so it is added to the
    /// value scope, found by the ordinary value lookup, and recognised in
    /// pattern position by the case-reference lookup, exactly as a union case is.
    /// An exception is never `[<RequireQualifiedAccess>]`, so the constructor is
    /// always in unqualified scope. (An `exception` also introduces an exception
    /// *type* in the disjoint type namespace; modelling that facet is a later
    /// slice — this binder is the value-namespace constructor.)
    ExceptionCase,
    /// The *recognizer* value of an active-pattern definition
    /// (`let (|Even|Odd|) … = …`) — the top-level function the cases dispatch to.
    /// Its [`range`](Def::range) is the `|Even|Odd|` name span (parens excluded),
    /// matching FCS, which resolves both the recognizer's own occurrence and
    /// every case *use* (`match x with Even`, or `then Even` constructing a case
    /// in the recognizer body) to this span. So each case name is a value-frame
    /// entry pointing at *this* def, and it is recognised in pattern position by
    /// the case-reference lookup, exactly as a union / exception case is.
    ActivePattern,
    /// A case *token* inside an active-pattern name (`Even`, `Odd` of
    /// `(|Even|Odd|)`). Interned only so the case's *defining occurrence* (the
    /// token in the `(|…|)` name) resolves to itself, as FCS reports it — a
    /// distinct symbol from the [`DefKind::ActivePattern`] recognizer. Case
    /// *uses* resolve to the recognizer, not here (the trailing `_` of a partial
    /// pattern is not a case and is never interned).
    ActivePatternCase,
    /// A case of an `enum` definition (`type Color = Red = 0 | Green = 1`). Enum
    /// cases are **require-qualified**: reachable only as `Color.Red`, never bare
    /// `Red` (FCS reports bare `Red` as `FS0039`), so — unlike a union case — an
    /// enum case is *not* added to the unqualified value frame. Its defining
    /// occurrence self-resolves, and a qualified use `Color.Red` resolves to it
    /// (via the enum-case index), with the head `Color` resolving to the enum
    /// [`DefKind::Type`].
    EnumCase,
    /// A **static member** of a type definition or same-file augmentation
    /// (`type Color() = static member Red = 1`, `member val`, a get/set
    /// property, or a single un-overloaded static method). Interned only for
    /// the emit-eligible subset of the type-member index (see
    /// `docs/project-type-member-plan.md`): a qualified `Color.Red` /
    /// `Pal.Color.Red` use in expression position resolves to it (FCS-pinned,
    /// probes M1/M2a/M2d/M4b), with the type segment resolving to the
    /// [`DefKind::Type`]. Its defining occurrence is *not* self-recorded (FCS
    /// reports synthetic symbols — a `member val`'s backing field — at the name
    /// token, so recording there would disagree with the oracle). Like an enum
    /// case it is reachable only through its type's name, never bare — it is
    /// *not* added to any value frame.
    Member,
    /// A **type parameter** declared by a `type` / `let` / `member` header
    /// (`type Foo<'T>`, `let f<'T>`, `member _.M<'T>`). Lives in F#'s *type*
    /// namespace, disjoint from values, and is **definition-scoped** — visible
    /// only inside the declaring definition, so it is held in a stack of typar
    /// frames rather than the value scope or the container-keyed type index. Its
    /// [`range`](Def::range) is the whole `'T` / `^T` occurrence *including the
    /// sigil* (the `TYPAR_DECL` node range), matching FCS, which reports the
    /// declaring occurrence and every `'T` use at the apostrophe-inclusive span.
    TypeParam,
}

/// What *sort of thing* a resolved name refers to, at the granularity a
/// consumer highlighting or labelling the name needs — the semantic-classification
/// currency (an LSP semantic-tokens pass maps each variant to a token type).
///
/// This is a **presentation-neutral semantic fact**, deliberately coarser than
/// [`DefKind`] in places (it does not distinguish a parameter from a `match`
/// local — both are locals a highlighter colours alike) and finer in others
/// (later stages classify referenced-assembly members into
/// property/method/field/event, distinctions [`DefKind::Member`] does not draw).
/// It is the say-something half of resolution's say-nothing-when-unsure
/// contract: it is only ever produced where name resolution has *committed* to a
/// binder, so — like [`Resolution`](crate::Resolution) — every value here is a
/// claim the FCS differential can hold us to.
///
/// The in-file ([`DefKind`]-derived) variants come from a file's own binders;
/// the referenced-assembly variants (`Module`, `Method`, `Property`, `Event`)
/// come from classifying a [`Resolution::Entity`](crate::Resolution) /
/// [`Resolution::Member`](crate::Resolution) against the
/// [`AssemblyEnv`] (see
/// [`AssemblyEnv::entity_class`](crate::AssemblyEnv::entity_class) /
/// [`AssemblyEnv::member_class`](crate::AssemblyEnv::member_class)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SemanticClass {
    /// A curried function value (`let f x = …`).
    Function,
    /// A non-function value binding (`let x = …`) — a module value or a local
    /// `let`. Function-ness here is *syntactic* (whether the binding has
    /// parameter patterns), so a value of function type bound without parameters
    /// (`let g = fun x -> x`) is a `Value`; the differential never asserts
    /// value-vs-function on this variant for exactly that reason.
    Value,
    /// A function / lambda formal parameter.
    Parameter,
    /// A `match`-clause (or other refutable-pattern) local. Kept distinct from
    /// [`Self::Parameter`] because [`DefKind`] does; a highlighter may collapse
    /// the two.
    PatternLocal,
    /// A type name (`type T = …` — abbreviation, record, union, enum, or class).
    /// Coarse: the record-vs-union-vs-class flavour is a later refinement.
    Type,
    /// A union case (`type T = A | B of …`), used as a constructor or a pattern
    /// head.
    UnionCase,
    /// An `exception` constructor (`exception E of …`).
    ExceptionCase,
    /// An active-pattern recognizer (`(|Even|Odd|)`) or one of its case tokens.
    ActivePattern,
    /// An `enum` case (`type Color = Red = 0`), reachable only qualified.
    EnumCase,
    /// A static member of an in-file type definition or augmentation, of a
    /// flavour [`DefKind::Member`] does not pin down (method vs property).
    Member,
    /// A type parameter (`'T` / `^T`) declared by a `type` / `let` / `member`
    /// header — see [`DefKind::TypeParam`].
    TypeParameter,
    /// A module or namespace-like entity in a referenced assembly (an F#
    /// `module`), the head a qualified assembly path roots at.
    Module,
    /// A method of a referenced-assembly type.
    Method,
    /// A property or field of a referenced-assembly type (the standard token
    /// legend has no distinct `field`, so both read as a property).
    Property,
    /// An event of a referenced-assembly type.
    Event,
}

impl DefKind {
    /// The presentation-neutral [`SemanticClass`] of a name bound by this kind.
    /// Total and pure: every [`DefKind`] maps to exactly one class.
    pub fn semantic_class(self) -> SemanticClass {
        match self {
            DefKind::Value { is_function: true } => SemanticClass::Function,
            DefKind::Value { is_function: false } => SemanticClass::Value,
            DefKind::Parameter => SemanticClass::Parameter,
            DefKind::PatternLocal => SemanticClass::PatternLocal,
            DefKind::Type => SemanticClass::Type,
            DefKind::UnionCase => SemanticClass::UnionCase,
            DefKind::ExceptionCase => SemanticClass::ExceptionCase,
            DefKind::ActivePattern | DefKind::ActivePatternCase => SemanticClass::ActivePattern,
            DefKind::EnumCase => SemanticClass::EnumCase,
            DefKind::Member => SemanticClass::Member,
            DefKind::TypeParam => SemanticClass::TypeParameter,
        }
    }
}

/// A stable identifier for a [`Def`] within a single resolved file — an index
/// into the file's definition arena (see [`crate::ResolvedFile`]). A newtype
/// rather than a bare index per "no primitive obsession": a `DefId` from one
/// file is meaningless against another's arena, and the type makes that
/// mismatch a compile error rather than a silently-wrong lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DefId(u32);

impl DefId {
    pub(crate) fn new(index: usize) -> Self {
        DefId(u32::try_from(index).expect("more than u32::MAX defs in one file"))
    }

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

impl Def {
    /// A `Def` spelled `spelling` (verbatim, as the source wrote it), whose
    /// logical name is interned in `names`.
    fn new(
        names: &NameTable,
        spelling: &str,
        range: TextRange,
        kind: DefKind,
        provisional: bool,
    ) -> Self {
        let logical = ident_name(spelling);
        Self {
            spelling: Spelling::of(spelling, &logical),
            name: names.logical(&logical),
            range,
            kind,
            provisional,
        }
    }

    /// Build a definite (non-provisional) `Def` from an identifier token and
    /// its kind.
    pub(crate) fn from_token(names: &NameTable, token: &SyntaxToken, kind: DefKind) -> Self {
        Self::new(names, token.text(), token.text_range(), kind, false)
    }

    /// Build a definite (non-provisional) `Def` from a spelling and an
    /// explicit range, for a binder whose span is not a single identifier
    /// token: a type parameter's `'T` occurrence (sigil + name, the
    /// `TYPAR_DECL` node), an active pattern (whose recognizer is spelled
    /// with its parentheses), or an operator-named case (see
    /// [`Self::from_op_name`]).
    pub(crate) fn from_range(
        names: &NameTable,
        spelling: &str,
        range: TextRange,
        kind: DefKind,
    ) -> Self {
        Self::new(names, spelling, range, kind, false)
    }

    /// Build a `Def` for an operator-named union/enum case — its compiled
    /// `op_Nil` / `op_ColonColon` name and the source range of the operator
    /// tokens (see `UnionCase::operator_name`). Unlike [`Self::from_token`] there
    /// is no single identifier token, so the name and range are passed directly.
    pub(crate) fn from_op_name(
        names: &NameTable,
        name: &str,
        range: TextRange,
        kind: DefKind,
    ) -> Self {
        Self::from_range(names, name, range, kind)
    }

    /// Build a *provisional* maybe-var `Def` (see [`Def::provisional`]).
    pub(crate) fn provisional_from_token(
        names: &NameTable,
        token: &SyntaxToken,
        kind: DefKind,
    ) -> Self {
        Self::new(names, token.text(), token.text_range(), kind, true)
    }

    /// The name this binder is keyed by in the name environment: [`id_text`]
    /// of its spelling.
    pub(crate) fn name(&self) -> Name {
        self.name
    }

    /// The name as the source spelled it, which is what a consumer shows:
    /// backticks kept, an operator binder uncompiled (`+`, not
    /// `op_Addition`). `env` must be the env the definition was resolved
    /// against.
    pub fn spelling<'s>(&'s self, env: &'s AssemblyEnv) -> Cow<'s, str> {
        self.spelling_in(env.names())
    }

    /// [`Self::spelling`], read from the table.
    pub(crate) fn spelling_in<'s>(&'s self, names: &'s NameTable) -> Cow<'s, str> {
        let text = names.text(self.name);
        match &self.spelling {
            Spelling::AsName => Cow::Borrowed(text),
            Spelling::Quoted => Cow::Owned(format!("``{text}``")),
            Spelling::Verbatim(spelling) => Cow::Borrowed(&**spelling),
        }
    }

    /// This definition with its name interned in `to` rather than `from`.
    #[cfg(feature = "test-support")]
    pub(crate) fn rekey(&self, from: &NameTable, to: &NameTable) -> Self {
        Self {
            name: crate::names::rekey_name(from, to, self.name),
            ..self.clone()
        }
    }
}

/// FCS `idText` semantics: the name an identifier token denotes.
///
/// A double-backtick-quoted identifier (`` ``my name`` ``) denotes the same
/// name as the bare form, so the delimiters are stripped. An unquoted operator
/// spelling denotes its compiled name ([`compile_op_name`]: `+` is
/// `op_Addition`), which is what FCS's parser stores in `Ident.idText` and so
/// what its name environment is keyed by. A *quoted* operator spelling is not
/// compiled — `` ``+`` `` is the identifier `+`, a different name from the
/// operator. Every other spelling is returned unchanged.
///
/// The operator of a **prefix** application is named after its position
/// ([`prefix_op_name`]): `-x` uses `~-` (`op_UnaryNegation`), not the binary
/// `-` its token spells, so the token's place in the tree is part of its name.
///
/// It takes a **token**, not a string, because it is not idempotent: the
/// name of `` ``+`` `` is `+`, whose own name would be `op_Addition`. A
/// function handed a name that is already canonical must use it as it is, and
/// the type keeps it from normalising twice. A binder's spelling goes
/// through [`Def`]'s constructors, the one other entry point.
pub(crate) fn id_text(tok: &SyntaxToken) -> Cow<'_, str> {
    if is_prefix_operator(tok) {
        prefix_op_name(tok.text())
    } else {
        ident_name(tok.text())
    }
}

/// Whether `tok` is the operator of a prefix application (`-x`, `!r`).
///
/// The parser builds one, as FCS's `mkSynPrefix` does, as an `APP_EXPR` whose
/// function is a bare single-operator `LONG_IDENT_EXPR`; nothing else puts a
/// bare operator there. An infix operator sits in an `INFIX_APP_EXPR`, and an
/// operator value (`(+) a b`) keeps its parentheses in the `LONG_IDENT`.
fn is_prefix_operator(tok: &SyntaxToken) -> bool {
    use borzoi_cst::syntax::SyntaxKind;
    let Some(long_ident) = tok.parent() else {
        return false;
    };
    let Some(expr) = long_ident.parent() else {
        return false;
    };
    // `take(2)`: a dotted path's every segment comes through here, so counting
    // all of the path's children would make a walk over its segments
    // quadratic in its length.
    tok.kind() == SyntaxKind::IDENT_TOK
        && long_ident.kind() == SyntaxKind::LONG_IDENT
        && long_ident
            .children_with_tokens()
            .filter(|c| !c.kind().is_trivia())
            .take(2)
            .count()
            == 1
        && expr.kind() == SyntaxKind::LONG_IDENT_EXPR
        && expr.parent().is_some_and(|app| {
            app.kind() == SyntaxKind::APP_EXPR && app.first_child().as_ref() == Some(&expr)
        })
        && is_operator_spelling(tok.text())
}

/// [`id_text`] over a token's verbatim text. Private: a caller with a string
/// in hand cannot tell whether it is still verbatim.
fn ident_name(raw: &str) -> Cow<'_, str> {
    match raw.strip_prefix("``").and_then(|s| s.strip_suffix("``")) {
        Some(quoted) => Cow::Borrowed(quoted),
        None => compile_op_name(raw),
    }
}

#[cfg(test)]
mod tests {
    //! What a definition publishes about its name: the spelling a consumer
    //! shows, and the logical name the walk keys it by. Every way the walk
    //! makes a [`Def`] is generated, under spellings that differ from their
    //! logical names (quoted identifiers, operators, a quoted operator).

    use super::*;
    use crate::{AssemblyEnv, ProjectItems, SyntaxRecovery, resolve_file};
    use borzoi_cst::parser::parse;
    use borzoi_cst::syntax::{ActivePatName, AstNode, ImplFile, SyntaxKind};
    use proptest::prelude::*;

    /// A value's spellings: plain, quoted, a quoted keyword, a quoted
    /// operator (the identifier `+`, not `op_Addition`).
    const VALUES: &[&str] = &["x", "y", "``x``", "``a b``", "``type``", "``+``"];
    /// A type's, a case's or an active-pattern case's spellings.
    const UPPERS: &[&str] = &["A", "B", "``A``", "``A B``"];
    /// An operator binder's spellings, inside its parentheses.
    const OPERATORS: &[&str] = &["+", "+++", "<*>", "~~~", "|>>"];

    /// One declaration, built from indices into the spelling pools.
    fn declaration() -> impl Strategy<Value = String> {
        let value = || prop::sample::select(VALUES);
        let upper = || prop::sample::select(UPPERS);
        let operator = || prop::sample::select(OPERATORS);
        prop_oneof![
            value().prop_map(|v| format!("let {v} = 0")),
            (value(), value()).prop_map(|(f, p)| format!("let {f} {p} = {p}")),
            operator().prop_map(|o| format!("let ( {o} ) a b = a")),
            (upper(), upper()).prop_map(|(t, c)| format!("type {t}<'T> = | {c} of int | Z")),
            (upper(), upper()).prop_map(|(t, c)| format!("type {t} = | {c} = 0 | Z = 1")),
            upper().prop_map(|e| format!("exception {e} of string")),
            upper().prop_map(|a| format!("let (|{a}|_|) x = None")),
            (upper(), upper()).prop_map(|(a, b)| format!("let (|{a}|{b}|) x = {a}")),
            Just("type L = | ([]) | ( :: ) of int * L".to_string()),
            value().prop_map(|m| format!("type R() =\n    static member {m} = 1")),
            value().prop_map(|v| format!("let m = match 1 with {v} -> {v}")),
            value().prop_map(|v| format!("let g = fun {v} -> {v}")),
        ]
    }

    /// The spelling `def` publishes.
    fn spelling<'e>(def: &'e Def, env: &'e AssemblyEnv) -> Cow<'e, str> {
        def.spelling(env)
    }

    /// The logical name `def` is keyed by.
    fn logical<'e>(def: &'e Def, env: &'e AssemblyEnv) -> Cow<'e, str> {
        Cow::Borrowed(env.names().text(def.name()))
    }

    proptest! {
        #[test]
        fn a_definition_publishes_its_spelling_and_keys_its_logical_name(
            decls in prop::collection::vec(declaration(), 1..6),
        ) {
            let src = format!("module M\n\n{}\n", decls.join("\n\n"));
            let parsed = parse(&src);
            prop_assert!(parsed.errors.is_empty(), "{src:?}: {:?}", parsed.errors);
            let recovery = SyntaxRecovery::of(&parsed);
            let root = parsed.root.clone();
            let file = ImplFile::cast(parsed.root).expect("an impl file");
            let env = AssemblyEnv::default();
            #[cfg(feature = "test-support")]
            let other = AssemblyEnv::default();
            let resolved = resolve_file(&file, &ProjectItems::default(), &env, &recovery);
            prop_assert!(resolved.defs().next().is_some());
            for def in resolved.defs() {
                let at = &src[usize::from(def.range.start())..usize::from(def.range.end())];
                let shown = spelling(def, &env);
                // The census's exceptions: a recognizer shows its parenthesised
                // name, a case use its case's name, an operator case its
                // compiled name. Every other definition shows its token.
                let expected: Vec<String> = match def.kind {
                    DefKind::ActivePattern => {
                        let apn = root
                            .covering_element(def.range)
                            .ancestors()
                            .find_map(ActivePatName::cast)
                            .expect("a recognizer span sits in its name");
                        std::iter::once(apn.syntax().text().to_string())
                            .chain(
                                apn.case_tokens()
                                    .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
                                    .map(|t| ident_name(t.text()).into_owned()),
                            )
                            .collect()
                    }
                    DefKind::UnionCase if at == "[]" || at.trim() == "::" => {
                        vec!["op_Nil".to_string(), "op_ColonColon".to_string()]
                    }
                    _ => vec![at.to_string()],
                };
                prop_assert!(
                    expected.iter().any(|e| *e == shown),
                    "{src:?}: {def:?} shows {shown:?}, not one of {expected:?}"
                );
                prop_assert_eq!(
                    &*logical(def, &env),
                    &*ident_name(&shown),
                    "{:?}: {:?}",
                    src,
                    def
                );
                // Re-keyed onto another table, it answers as it did.
                #[cfg(feature = "test-support")]
                {
                    let rekeyed = def.rekey(env.names(), other.names());
                    prop_assert_eq!(&spelling(&rekeyed, &other), &shown, "{:?}", src);
                    prop_assert_eq!(logical(&rekeyed, &other), logical(def, &env), "{:?}", src);
                }
            }
        }
    }
}
