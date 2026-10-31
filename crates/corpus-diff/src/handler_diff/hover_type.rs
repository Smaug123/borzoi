//! Whether a hover shows the symbol's **type**, and whether FCS agrees.
//!
//! [`super`] grades a hover on whether it names FCS's symbol. That says nothing
//! about the line a user reads most: `` `x : int` — value `` against
//! `` `x` — value ``. This module grades that line, per hover probe, into a
//! closed [`TypeVerdict`]:
//!
//! - the type is shown and FCS agrees ([`TypeVerdict::Agrees`]);
//! - the type is shown and FCS disagrees — a wrong answer, recorded by the
//!   caller as a [`super::HandlerDivergence`] like any other
//!   ([`TypeVerdict::Wrong`]);
//! - the type is withheld, and why ([`Withheld`]): which binder inference did
//!   not type, an earlier file's binder (and whether hovering its definition
//!   would show the type), or a referenced-assembly member whose metadata
//!   signature the hover declines to show;
//! - and the hovers that never had a type to show: a symbol that is not a value
//!   (a type, a union case), or a hover that describes no symbol at all.
//!
//! # The oracle and the currency
//!
//! FCS's type for each use rides on its `uses-project` record
//! ([`crate::OracleValue::type_canon`]): the symbol's *own* type, in the
//! canonical currency the inference differentials share (`System.Int32 ->
//! 'a`). The comparison is made in the **displayed** domain, which is the one
//! the user reads: FCS's canonical string is parsed back to a [`Ty`]
//! ([`parse_canonical`]) and rendered by the same [`Ty::render_fsharp`] the
//! hover renders with, and the two strings must be equal up to the names of
//! their type parameters ([`alpha_normalise`]). The renderer is therefore
//! trusted here, not graded; what is graded is that the type it was handed is
//! FCS's.
//!
//! Type parameters compare up to renaming because a hover shows a binder's type
//! on its own: `y : 'b` and `y : 'a` say the same thing about `y`. That leaves
//! one wrong answer unseen — a hover claiming two parameters' types are one
//! variable shows `'a` for each, alike under renaming — which no per-symbol
//! comparison can see.
//!
//! A referenced-assembly member's hover shows its signature as read from
//! metadata (`static member WriteLine: value: string -> unit`), not a
//! [`Ty`]; it is recorded as shown ([`TypeVerdict::ShownSignature`]) but not
//! graded here.

use std::collections::BTreeMap;

use borzoi_sema::Ty;
use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::{ProjectUse, UseDecl, ValueKind};

/// What a hovered use's symbol is, as the census groups it: FCS's facts about
/// the record, and whether its declaration is in the hovered file, an earlier
/// one, or a referenced assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SymbolClass {
    /// A parameter, a pattern binder or a local `let`.
    Local,
    /// A module-level `let` declared in the hovered file.
    InFileValue,
    /// A module-level `let` declared in an earlier file of the project.
    EarlierFileValue,
    /// A member of a type the project declares.
    ProjectMember,
    /// A field of a record or class the project declares.
    ProjectField,
    /// A type, module or namespace the project declares.
    ProjectEntity,
    /// A union case or exception the project declares.
    ProjectUnionCase,
    /// A referenced assembly's module-level value or function (`List.map`).
    AssemblyValue,
    /// A referenced assembly's member (`Console.WriteLine`, `s.Length`).
    AssemblyMember,
    /// A referenced assembly's field.
    AssemblyField,
    /// A referenced assembly's type, module or namespace.
    AssemblyEntity,
    /// A referenced assembly's union case or exception.
    AssemblyUnionCase,
    /// An active-pattern case, wherever it is declared.
    ActivePatternCase,
    /// A type parameter.
    TypeParameter,
    /// A record FCS gave no kind for.
    Unknown,
    /// A constructor FCS reports where a written type name calls one (`T()`,
    /// `[<Attr>]`): the hover describes the type.
    Constructor,
}

impl SymbolClass {
    pub const ALL: [SymbolClass; 16] = [
        SymbolClass::Local,
        SymbolClass::InFileValue,
        SymbolClass::EarlierFileValue,
        SymbolClass::ProjectMember,
        SymbolClass::ProjectField,
        SymbolClass::ProjectEntity,
        SymbolClass::ProjectUnionCase,
        SymbolClass::AssemblyValue,
        SymbolClass::AssemblyMember,
        SymbolClass::AssemblyField,
        SymbolClass::AssemblyEntity,
        SymbolClass::AssemblyUnionCase,
        SymbolClass::ActivePatternCase,
        SymbolClass::TypeParameter,
        SymbolClass::Unknown,
        SymbolClass::Constructor,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SymbolClass::Local => "local",
            SymbolClass::InFileValue => "in-file-value",
            SymbolClass::EarlierFileValue => "earlier-file-value",
            SymbolClass::ProjectMember => "project-member",
            SymbolClass::ProjectField => "project-field",
            SymbolClass::ProjectEntity => "project-entity",
            SymbolClass::ProjectUnionCase => "project-union-case",
            SymbolClass::AssemblyValue => "assembly-value",
            SymbolClass::AssemblyMember => "assembly-member",
            SymbolClass::AssemblyField => "assembly-field",
            SymbolClass::AssemblyEntity => "assembly-entity",
            SymbolClass::AssemblyUnionCase => "assembly-union-case",
            SymbolClass::ActivePatternCase => "active-pattern-case",
            SymbolClass::TypeParameter => "type-parameter",
            SymbolClass::Unknown => "unknown",
            SymbolClass::Constructor => "constructor",
        }
    }

    /// The one verdict a described hover of this class is expected to come
    /// to, which the manifest therefore only counts: a project value's type
    /// shown and FCS's, a referenced-assembly value's metadata signature, and
    /// nothing for a symbol that is not a value.
    ///
    /// One verdict, not a set, for the reason [`super::expected_outcome`] gives:
    /// two probes of one class can then trade verdicts only by one of them
    /// leaving the expected one, which lists it.
    pub fn expected_verdict(self) -> TypeVerdict {
        match self {
            SymbolClass::Local
            | SymbolClass::InFileValue
            | SymbolClass::EarlierFileValue
            | SymbolClass::ProjectMember
            | SymbolClass::ProjectField => TypeVerdict::Agrees,
            SymbolClass::AssemblyValue
            | SymbolClass::AssemblyMember
            | SymbolClass::AssemblyField => TypeVerdict::ShownSignature,
            SymbolClass::ProjectEntity
            | SymbolClass::ProjectUnionCase
            | SymbolClass::AssemblyEntity
            | SymbolClass::AssemblyUnionCase
            | SymbolClass::ActivePatternCase
            | SymbolClass::TypeParameter
            | SymbolClass::Unknown
            | SymbolClass::Constructor => TypeVerdict::NotAValue,
        }
    }

    /// The class of `u`, a record in the source `hovered` names.
    pub fn of(u: &ProjectUse, hovered: &std::path::Path) -> SymbolClass {
        if u.is_constructor {
            return SymbolClass::Constructor;
        }
        let declared = match &u.decl {
            UseDecl::InProject(d) if d.file == hovered => Declared::ThisFile,
            UseDecl::InProject(_) => Declared::EarlierFile,
            UseDecl::Unlocated | UseDecl::OutsideProject(_) => Declared::Assembly,
        };
        let project = declared != Declared::Assembly;
        match (
            u.value.as_ref().and_then(|v| v.kind),
            u.symbol_kind.as_deref(),
        ) {
            (Some(ValueKind::Local), _) => SymbolClass::Local,
            (Some(ValueKind::Module), _) => match declared {
                Declared::ThisFile => SymbolClass::InFileValue,
                Declared::EarlierFile => SymbolClass::EarlierFileValue,
                Declared::Assembly => SymbolClass::AssemblyValue,
            },
            (Some(ValueKind::Member), _) if project => SymbolClass::ProjectMember,
            (Some(ValueKind::Member), _) => SymbolClass::AssemblyMember,
            (Some(ValueKind::Field), _) if project => SymbolClass::ProjectField,
            (Some(ValueKind::Field), _) => SymbolClass::AssemblyField,
            (None, Some("type" | "module" | "namespace")) if project => SymbolClass::ProjectEntity,
            (None, Some("type" | "module" | "namespace")) => SymbolClass::AssemblyEntity,
            (None, Some("unioncase")) if project => SymbolClass::ProjectUnionCase,
            (None, Some("unioncase")) => SymbolClass::AssemblyUnionCase,
            (None, Some("activepatterncase")) => SymbolClass::ActivePatternCase,
            (None, Some("genericparameter")) => SymbolClass::TypeParameter,
            (None, _) => SymbolClass::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declared {
    ThisFile,
    EarlierFile,
    Assembly,
}

/// The kind a project hover labels a value-like binder with (`` — function ``),
/// which is how an in-file binder whose type is withheld is told apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValueLabel {
    Function,
    Value,
    Parameter,
    PatternLocal,
    StaticMember,
    ActivePattern,
    EnumCase,
}

impl ValueLabel {
    const ALL: [ValueLabel; 7] = [
        ValueLabel::Function,
        ValueLabel::Value,
        ValueLabel::Parameter,
        ValueLabel::PatternLocal,
        ValueLabel::StaticMember,
        ValueLabel::ActivePattern,
        ValueLabel::EnumCase,
    ];

    /// The label a project hover writes after its head, if it is one for a
    /// binder that has a value type.
    pub fn of_label(label: &str) -> Option<ValueLabel> {
        Some(match label {
            "function" => ValueLabel::Function,
            "value" => ValueLabel::Value,
            "parameter" => ValueLabel::Parameter,
            "pattern local" => ValueLabel::PatternLocal,
            "static member" => ValueLabel::StaticMember,
            "active pattern" => ValueLabel::ActivePattern,
            "enum case" => ValueLabel::EnumCase,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            ValueLabel::Function => "function",
            ValueLabel::Value => "value",
            ValueLabel::Parameter => "parameter",
            ValueLabel::PatternLocal => "pattern-local",
            ValueLabel::StaticMember => "static-member",
            ValueLabel::ActivePattern => "active-pattern",
            ValueLabel::EnumCase => "enum-case",
        }
    }
}

/// Why a hover describing a value shows no type for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Withheld {
    /// A binder of the hovered file that the file's inference left untyped.
    InFile(ValueLabel),
    /// A binder of an earlier file. The hover reads only the hovered file's
    /// inference; hovering the binder's definition shows its type.
    EarlierFileTypedAtDefinition,
    /// As [`Self::EarlierFileTypedAtDefinition`], but the definition's hover
    /// shows no type either.
    EarlierFileUntypedAtDefinition,
    /// A referenced-assembly member whose types the hover withholds because
    /// the compiled signature may have dropped a unit of measure.
    Measures,
    /// … because its curried argument groups were not read.
    ArgGroups,
    /// … because its type parameters cannot be told from its target type's.
    TargetTypars,
    /// … for a reason this module does not recognise.
    AssemblyOther,
    /// A project hover on a record FCS declares outside the project.
    Elsewhere,
}

impl Withheld {
    fn all() -> Vec<Withheld> {
        let mut all: Vec<Withheld> = ValueLabel::ALL.into_iter().map(Withheld::InFile).collect();
        all.extend([
            Withheld::EarlierFileTypedAtDefinition,
            Withheld::EarlierFileUntypedAtDefinition,
            Withheld::Measures,
            Withheld::ArgGroups,
            Withheld::TargetTypars,
            Withheld::AssemblyOther,
            Withheld::Elsewhere,
        ]);
        all
    }

    fn label(self) -> String {
        match self {
            Withheld::InFile(label) => format!("in-file-{}", label.label()),
            Withheld::EarlierFileTypedAtDefinition => "earlier-file-typed-at-definition".into(),
            Withheld::EarlierFileUntypedAtDefinition => "earlier-file-untyped-at-definition".into(),
            Withheld::Measures => "assembly-measures".into(),
            Withheld::ArgGroups => "assembly-arg-groups".into(),
            Withheld::TargetTypars => "assembly-target-typars".into(),
            Withheld::AssemblyOther => "assembly-other".into(),
            Withheld::Elsewhere => "declared-elsewhere".into(),
        }
    }
}

/// What a hover probe showed of its symbol's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypeVerdict {
    /// A type, and FCS's.
    Agrees,
    /// A type that is not FCS's, or a hover that is wrong in another way.
    Wrong,
    /// A type FCS gave none for, or one this module cannot read back
    /// ([`parse_canonical`] declined it: a measure, a byref, a struct tuple).
    ShownUngraded,
    /// A referenced-assembly member's metadata signature, not graded here.
    ShownSignature,
    /// No type, and why.
    Withheld(Withheld),
    /// The hover describes a symbol that has no value type: a type, a module,
    /// a union case, a type parameter.
    NotAValue,
    /// A referenced-assembly hover whose head [`super`] does not read.
    Unread,
    /// "No definition available": the resolver named no symbol here.
    ResolverDeclined,
    /// No hover at all.
    NoHover,
    /// A hover describing something other than a symbol (an expression's type).
    NotASymbol,
}

impl TypeVerdict {
    /// Every verdict, so a census can report the zeros.
    pub fn all() -> Vec<TypeVerdict> {
        let mut all = vec![
            TypeVerdict::Agrees,
            TypeVerdict::Wrong,
            TypeVerdict::ShownUngraded,
            TypeVerdict::ShownSignature,
        ];
        all.extend(Withheld::all().into_iter().map(TypeVerdict::Withheld));
        all.extend([
            TypeVerdict::NotAValue,
            TypeVerdict::Unread,
            TypeVerdict::ResolverDeclined,
            TypeVerdict::NoHover,
            TypeVerdict::NotASymbol,
        ]);
        all
    }

    pub fn label(self) -> String {
        match self {
            TypeVerdict::Agrees => "agrees".into(),
            TypeVerdict::Wrong => "wrong".into(),
            TypeVerdict::ShownUngraded => "shown-ungraded".into(),
            TypeVerdict::ShownSignature => "shown-signature".into(),
            TypeVerdict::Withheld(why) => format!("withheld-{}", why.label()),
            TypeVerdict::NotAValue => "not-a-value".into(),
            TypeVerdict::Unread => "unread".into(),
            TypeVerdict::ResolverDeclined => "resolver-declined".into(),
            TypeVerdict::NoHover => "no-hover".into(),
            TypeVerdict::NotASymbol => "not-a-symbol".into(),
        }
    }

    /// Whether this is a verdict on a hover that describes a symbol, rather
    /// than one fixed by the hover's [`super::Outcome`].
    fn of_a_description(self) -> bool {
        match self {
            TypeVerdict::Agrees
            | TypeVerdict::ShownUngraded
            | TypeVerdict::ShownSignature
            | TypeVerdict::Withheld(_)
            | TypeVerdict::NotAValue => true,
            TypeVerdict::Wrong
            | TypeVerdict::Unread
            | TypeVerdict::ResolverDeclined
            | TypeVerdict::NoHover
            | TypeVerdict::NotASymbol => false,
        }
    }
}

/// One hover probe's grade: its symbol's class and what the hover showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HoverType {
    pub class: SymbolClass,
    pub verdict: TypeVerdict,
}

impl HoverType {
    /// Whether the manifest lists the probe, rather than only counting it: a
    /// hover describing its symbol, with any verdict but its class's
    /// [expected one](SymbolClass::expected_verdict).
    ///
    /// The rest are fixed by the hover's outcome — no hover, "No definition
    /// available", a wrong answer — and the manifest already lists every probe
    /// whose outcome is not the one the comparator's verdict implies
    /// ([`super::consistent`]).
    pub fn listed(self) -> bool {
        self.verdict.of_a_description() && self.verdict != self.class.expected_verdict()
    }
}

/// How many hover probes came to each [`HoverType`].
///
/// Serialised — and rendered by [`Self::by_class`] and [`Self::by_verdict`] —
/// with **every** class and verdict, the zeros included, so a metric never
/// vanishes between runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HoverTypeCensus {
    counts: BTreeMap<HoverType, usize>,
}

impl HoverTypeCensus {
    pub fn record(&mut self, grade: HoverType) {
        *self.counts.entry(grade).or_default() += 1;
    }

    pub fn total(&self) -> usize {
        self.counts.values().sum()
    }

    pub fn count(&self, grade: HoverType) -> usize {
        self.counts.get(&grade).copied().unwrap_or(0)
    }

    pub fn by_verdict(&self) -> BTreeMap<String, usize> {
        TypeVerdict::all()
            .into_iter()
            .map(|verdict| {
                let n = SymbolClass::ALL
                    .iter()
                    .map(|&class| self.count(HoverType { class, verdict }))
                    .sum();
                (verdict.label(), n)
            })
            .collect()
    }

    pub fn by_class(&self) -> BTreeMap<&'static str, BTreeMap<String, usize>> {
        SymbolClass::ALL
            .iter()
            .map(|&class| {
                let verdicts = TypeVerdict::all()
                    .into_iter()
                    .map(|verdict| (verdict.label(), self.count(HoverType { class, verdict })))
                    .collect();
                (class.label(), verdicts)
            })
            .collect()
    }

    /// The text report's lines: the verdicts, then each class's, zeros left
    /// out.
    pub fn render_text(&self) -> String {
        let nonzero = |m: &BTreeMap<String, usize>| {
            m.iter()
                .filter(|(_, n)| **n > 0)
                .map(|(k, n)| format!("{k}={n}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut out = format!(
            "project-corpus-diff hover types: {} probes | {}\n",
            self.total(),
            nonzero(&self.by_verdict())
        );
        for (class, verdicts) in self.by_class() {
            if verdicts.values().any(|n| *n > 0) {
                out.push_str(&format!(
                    "project-corpus-diff hover types {class}: {}\n",
                    nonzero(&verdicts)
                ));
            }
        }
        out
    }
}

impl Serialize for HoverTypeCensus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("total", &self.total())?;
        map.serialize_entry("by_verdict", &self.by_verdict())?;
        map.serialize_entry("by_class", &self.by_class())?;
        map.end()
    }
}

/// The paragraph `handlers/hover.rs` writes in place of a referenced-assembly
/// member's types, and why it withholds them.
pub fn assembly_withheld(body: &str) -> Option<Withheld> {
    let (_, why) = body.split_once("Types not shown: ")?;
    Some(if why.contains("drops units of measure") {
        Withheld::Measures
    } else if why.contains("group into curried arguments") {
        Withheld::ArgGroups
    } else if why.contains("type parameters are its own") {
        Withheld::TargetTypars
    } else {
        Withheld::AssemblyOther
    })
}

/// FCS's type for a record, as the hover would display it: `None` where FCS
/// gave none or [`parse_canonical`] cannot read what it gave.
pub fn displayed(u: &ProjectUse) -> Option<String> {
    let canon = u.value.as_ref()?.type_canon.as_deref()?;
    Some(alpha_normalise(&parse_canonical(canon)?.render_fsharp()))
}

/// Grade `shown`, the type a hover displays for `u`, against FCS's.
pub fn grade_shown(u: &ProjectUse, shown: &str) -> Result<TypeVerdict, String> {
    let Some(theirs) = displayed(u) else {
        return Ok(TypeVerdict::ShownUngraded);
    };
    let ours = alpha_normalise(shown);
    if ours == theirs {
        Ok(TypeVerdict::Agrees)
    } else {
        Err(format!(
            "the hover shows {:?} as type {shown:?}; FCS's is {theirs:?} ({:?})",
            u.name,
            u.value.as_ref().and_then(|v| v.type_canon.as_deref())
        ))
    }
}

/// Rename the type parameters of a displayed type by first appearance — `'a`,
/// `'b`, … as [`Ty::render_fsharp`] spells [`Ty::Param`] — so two renderings
/// that differ only in what they call their parameters are equal.
///
/// A parameter is a `'` that does not continue an identifier, followed by an
/// identifier.
pub fn alpha_normalise(text: &str) -> String {
    let mut names: Vec<String> = Vec::new();
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '\'';
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let starts_param = c == '\''
            && (i == 0 || !ident(chars[i - 1]) && chars[i - 1] != '`')
            && chars
                .get(i + 1)
                .is_some_and(|n| n.is_alphabetic() || *n == '_');
        if !starts_param {
            out.push(c);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && ident(chars[j]) {
            j += 1;
        }
        let name: String = chars[i + 1..j].iter().collect();
        let index = match names.iter().position(|n| *n == name) {
            Some(index) => index,
            None => {
                names.push(name);
                names.len() - 1
            }
        };
        out.push_str(&Ty::Param(u32::try_from(index).expect("few parameters")).render_fsharp());
        i = j;
    }
    out
}

/// Read back a type in the canonical currency `fcs-dump`'s
/// `renderTypeCanonical` writes and [`Ty::render`] matches: dotted names with
/// `<a, b>` arguments, `a * b`, `a -> b`, `a[,]`, `'a`, with parentheses only
/// where grouping needs them. `None` for anything else — a measure or SRTP
/// parameter (`<measure:…>`, `<srtp:…>`), a byref, a struct tuple, or FCS's
/// display-text fallback — since a type read wrongly would grade a right hover
/// wrong or a wrong one right.
///
/// Type parameters become [`Ty::Param`]s numbered by first appearance.
pub fn parse_canonical(text: &str) -> Option<Ty> {
    let mut parser = Parser {
        text,
        at: 0,
        params: Vec::new(),
    };
    let ty = parser.function()?;
    (parser.at == text.len()).then_some(ty)
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
    params: Vec<&'a str>,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn eat(&mut self, token: &str) -> bool {
        if self.rest().starts_with(token) {
            self.at += token.len();
            true
        } else {
            false
        }
    }

    /// `a -> b`, right-associative.
    fn function(&mut self) -> Option<Ty> {
        let arg = self.tuple()?;
        if self.eat(" -> ") {
            let ret = self.function()?;
            return Some(Ty::Fun {
                arg: Box::new(arg),
                ret: Box::new(ret),
            });
        }
        Some(arg)
    }

    /// `a * b * …`, flat: a nested tuple is parenthesised.
    fn tuple(&mut self) -> Option<Ty> {
        let mut elems = vec![self.postfix()?];
        while self.eat(" * ") {
            elems.push(self.postfix()?);
        }
        Some(if elems.len() == 1 {
            elems.pop().expect("one element")
        } else {
            Ty::Tuple(elems)
        })
    }

    /// An atom followed by any number of array suffixes.
    fn postfix(&mut self) -> Option<Ty> {
        let mut ty = self.atom()?;
        while self.eat("[") {
            let mut rank = 1;
            while self.eat(",") {
                rank += 1;
            }
            if !self.eat("]") {
                return None;
            }
            ty = Ty::Array {
                elem: Box::new(ty),
                rank,
            };
        }
        Some(ty)
    }

    fn atom(&mut self) -> Option<Ty> {
        if self.eat("(") {
            let ty = self.function()?;
            return self.eat(")").then_some(ty);
        }
        if self.eat("'") {
            let len = self
                .rest()
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(self.rest().len());
            let name = &self.rest()[..len];
            if name.is_empty() {
                return None;
            }
            self.at += len;
            let index = match self.params.iter().position(|p| *p == name) {
                Some(index) => index,
                None => {
                    self.params.push(name);
                    self.params.len() - 1
                }
            };
            return Some(Ty::Param(u32::try_from(index).ok()?));
        }
        let name = self.name()?;
        let path: Vec<String> = name.split('.').map(str::to_string).collect();
        if path.iter().any(String::is_empty) {
            return None;
        }
        let mut args = Vec::new();
        if self.eat("<") {
            loop {
                args.push(self.function()?);
                if self.eat(", ") {
                    continue;
                }
                if self.eat(">") {
                    break;
                }
                return None;
            }
        }
        Some(Ty::Named { path, args })
    }

    /// A dotted name: everything up to a delimiter or a space.
    ///
    /// A quoted identifier can compile to a name with a space in it, which this
    /// declines along with FCS's display-text fallback (`int list`), whose
    /// spaces it cannot tell from one: read as a single name, the fallback
    /// would grade a right hover wrong.
    fn name(&mut self) -> Option<&'a str> {
        let rest = self.rest();
        let end = rest
            .find(['<', '>', ',', '(', ')', '[', ']', '&', '\'', ' '])
            .unwrap_or(rest.len());
        let name = &rest[..end];
        if name.is_empty() {
            return None;
        }
        self.at += end;
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn named(path: &str, args: Vec<Ty>) -> Ty {
        Ty::Named {
            path: path.split('.').map(str::to_string).collect(),
            args,
        }
    }

    #[test]
    fn the_oracle_currency_reads_back() {
        let int = || Ty::named("System.Int32");
        assert_eq!(parse_canonical("System.Int32"), Some(int()));
        assert_eq!(
            parse_canonical(
                "('a -> 'b) -> Microsoft.FSharp.Collections.FSharpList<'a> -> \
                 Microsoft.FSharp.Collections.FSharpList<'b>"
            )
            .map(|t| t.render_fsharp()),
            Some("('a -> 'b) -> list<'a> -> list<'b>".to_string())
        );
        assert_eq!(
            parse_canonical("System.Int32 * System.Int32 -> System.Int32"),
            Some(Ty::Fun {
                arg: Box::new(Ty::Tuple(vec![int(), int()])),
                ret: Box::new(int()),
            })
        );
        assert_eq!(
            parse_canonical(
                "Microsoft.FSharp.Collections.FSharpList<System.Int32 -> System.String>"
            )
            .map(|t| t.render_fsharp()),
            Some("list<int -> string>".to_string())
        );
        assert_eq!(
            parse_canonical("(System.Int32 * System.String)[,]"),
            Some(Ty::Array {
                elem: Box::new(Ty::Tuple(vec![int(), Ty::named("System.String")])),
                rank: 2,
            })
        );
        assert_eq!(
            parse_canonical("Zoo.Outer/Inner"),
            Some(named("Zoo.Outer/Inner", vec![]))
        );
    }

    #[test]
    fn what_the_currency_cannot_say_is_declined() {
        for text in [
            "<srtp:T1> -> <srtp:T2> -> <srtp:T3>",
            "System.Double<<measure:m>>",
            "System.Int32&",
            "struct (System.Int32 * System.Int32)",
            "int list",
            // A quoted identifier compiles to a name with a space in it, which
            // the display fallback's spaces make unreadable.
            "Zoo.Odd Type",
            "System.Int32 *",
            "",
        ] {
            assert_eq!(parse_canonical(text), None, "{text:?}");
        }
    }

    #[test]
    fn parameters_compare_up_to_renaming() {
        assert_eq!(alpha_normalise("'b -> 'a -> 'b"), "'a -> 'b -> 'a");
        assert_eq!(alpha_normalise("'T list -> 'U"), "'a list -> 'b");
        // An apostrophe inside a name or a quoted name is not a parameter.
        assert_eq!(alpha_normalise("x' -> ``a'b``"), "x' -> ``a'b``");
    }

    #[test]
    fn the_withheld_paragraphs_say_why() {
        assert_eq!(
            assembly_withheld(
                "`member D`\n\nin `M`\n\nTypes not shown: they are read from the compiled \
                 signature, which drops units of measure, and this member's F# type may carry one."
            ),
            Some(Withheld::Measures)
        );
        assert_eq!(assembly_withheld("`member D: int`\n\nin `M`"), None);
    }

    fn arb_ty() -> impl Strategy<Value = Ty> {
        let leaf = prop_oneof![
            prop::sample::select(vec![
                "System.Int32",
                "System.String",
                "Zoo.Point",
                "Microsoft.FSharp.Core.Unit",
                "A.B/C",
            ])
            .prop_map(Ty::named),
            (0u32..4).prop_map(Ty::Param),
        ];
        leaf.prop_recursive(4, 24, 3, |inner| {
            prop_oneof![
                (
                    prop::sample::select(vec![
                        "Microsoft.FSharp.Collections.FSharpList",
                        "System.Collections.Generic.Dictionary",
                    ]),
                    prop::collection::vec(inner.clone(), 1..3)
                )
                    .prop_map(|(path, args)| named(path, args)),
                (inner.clone(), 1u32..3).prop_map(|(elem, rank)| Ty::Array {
                    elem: Box::new(elem),
                    rank
                }),
                prop::collection::vec(inner.clone(), 2..4).prop_map(Ty::Tuple),
                (inner.clone(), inner).prop_map(|(arg, ret)| Ty::Fun {
                    arg: Box::new(arg),
                    ret: Box::new(ret)
                }),
            ]
        })
    }

    proptest! {
        /// The reader inverts the canonical renderer the oracle and
        /// [`Ty::render`] share: whatever the renderer writes reads back as
        /// the type it was, up to the numbering of its parameters, and so
        /// displays as that type does.
        #[test]
        fn a_rendered_type_reads_back(ty in arb_ty()) {
            let read = parse_canonical(&ty.render());
            prop_assert!(read.is_some(), "{:?} did not read back", ty.render());
            let read = read.expect("checked");
            prop_assert_eq!(read.render(), alpha_normalise(&ty.render()));
            prop_assert_eq!(
                alpha_normalise(&read.render_fsharp()),
                alpha_normalise(&ty.render_fsharp())
            );
        }
    }
}
