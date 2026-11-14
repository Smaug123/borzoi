//! The closed set of types a typed-AST accessor may return, or a value enum's
//! variant may carry ([`Shape`]), read from Rust source with `syn`
//! ([`classify`]) and spelled in a lock file ([`Shape::render`] /
//! [`parse_shape`]).
//!
//! A shape is a container ([`Shape`]) around an element ([`Elem`]). The
//! elements are the facade's own types, named by what they are (a node
//! newtype, a dispatch enum, a value enum), the substrate's `SyntaxToken`, and
//! a handful of scalars. Anything else is not a shape, and [`classify`] says
//! so: a per-version facade re-casts each accessor's result into its own types
//! (`docs/ast-versioning-plan.md` D4), which it can do only for a type it
//! knows how to re-cast.

use std::collections::BTreeSet;
use std::fmt::Write as _;

/// The type names the facade's tables declare, by what each one is.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Names {
    /// Node newtypes (`AstNode` over one or more `SyntaxKind`s).
    pub nodes: BTreeSet<String>,
    /// Dispatch enums (closed sums over nodes).
    pub enums: BTreeSet<String>,
    /// Value enums (plain Rust enums an accessor computes).
    pub values: BTreeSet<String>,
}

impl Names {
    fn elem_for(&self, name: &str) -> Option<Elem> {
        if self.nodes.contains(name) {
            Some(Elem::Node(name.to_string()))
        } else if self.enums.contains(name) {
            Some(Elem::Enum(name.to_string()))
        } else if self.values.contains(name) {
            Some(Elem::Value(name.to_string()))
        } else {
            None
        }
    }

    /// Whether every facade type `shape` names is one of these.
    pub fn resolves(&self, shape: &Shape) -> bool {
        shape.elem().leaves().iter().all(|elem| match elem {
            Elem::Node(n) => self.nodes.contains(n),
            Elem::Enum(n) => self.enums.contains(n),
            Elem::Value(n) => self.values.contains(n),
            _ => true,
        })
    }
}

/// The element a [`Shape`] holds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Elem {
    /// A node newtype, e.g. `LongIdent`.
    Node(String),
    /// A dispatch enum, e.g. `Expr`.
    Enum(String),
    /// A value enum, e.g. `TyparDeclsForm`.
    Value(String),
    /// `SyntaxToken`.
    Token,
    /// `bool`.
    Bool,
    /// `usize`.
    Usize,
    /// `i32`.
    I32,
    /// `TextRange`.
    TextRange,
    /// `&'static str`.
    StaticStr,
    /// A tuple of two or more elements.
    Tuple(Vec<Elem>),
}

/// The container around an [`Elem`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Shape {
    /// The element itself.
    One(Elem),
    /// `Option<_>`.
    Option(Elem),
    /// `impl Iterator<Item = _>` (with or without a `+ '_` bound).
    Iter(Elem),
    /// `Vec<_>`.
    Vec(Elem),
}

impl Elem {
    /// This element, or for a tuple every element inside it.
    pub fn leaves(&self) -> Vec<&Elem> {
        match self {
            Elem::Tuple(items) => items.iter().flat_map(Elem::leaves).collect(),
            other => vec![other],
        }
    }

    fn render(&self, out: &mut String) {
        match self {
            Elem::Node(n) => write!(out, "node {n}").unwrap(),
            Elem::Enum(n) => write!(out, "enum {n}").unwrap(),
            Elem::Value(n) => write!(out, "value {n}").unwrap(),
            Elem::Token => out.push_str("token"),
            Elem::Bool => out.push_str("bool"),
            Elem::Usize => out.push_str("usize"),
            Elem::I32 => out.push_str("i32"),
            Elem::TextRange => out.push_str("text-range"),
            Elem::StaticStr => out.push_str("static-str"),
            Elem::Tuple(items) => {
                out.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.render(out);
                }
                out.push(')');
            }
        }
    }
}

impl Shape {
    /// The element inside the container.
    pub fn elem(&self) -> &Elem {
        match self {
            Shape::One(e) | Shape::Option(e) | Shape::Iter(e) | Shape::Vec(e) => e,
        }
    }

    /// The lock spelling: `bool`, `option node LongIdent`, `iter enum Expr`,
    /// `vec value TupleSegment`, `option (static-str, text-range)`.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out);
        out
    }

    pub(crate) fn render_into(&self, out: &mut String) {
        match self {
            Shape::One(_) => {}
            Shape::Option(_) => out.push_str("option "),
            Shape::Iter(_) => out.push_str("iter "),
            Shape::Vec(_) => out.push_str("vec "),
        }
        self.elem().render(out);
    }
}

/// The last segment of a path type, when the path has no `qself` and every
/// earlier segment is one of `prefixes` in order (an empty `prefixes` allows
/// a single segment only; `["crate", "syntax"]` also allows
/// `crate::syntax::X`).
fn path_tail<'a>(ty: &'a syn::TypePath, prefixes: &[&str]) -> Option<&'a syn::PathSegment> {
    if ty.qself.is_some() || ty.path.leading_colon.is_some() {
        return None;
    }
    let segments: Vec<&syn::PathSegment> = ty.path.segments.iter().collect();
    let (last, init) = segments.split_last()?;
    let plain = |s: &&syn::PathSegment| matches!(s.arguments, syn::PathArguments::None);
    let prefix_ok = init.is_empty()
        || (init.len() == prefixes.len()
            && init.iter().all(plain)
            && init.iter().zip(prefixes).all(|(s, p)| s.ident == p));
    prefix_ok.then_some(*last)
}

/// The single type argument of `Name<T>`.
fn single_type_arg(segment: &syn::PathSegment) -> Option<&syn::Type> {
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let mut iter = args.args.iter();
    match (iter.next(), iter.next()) {
        (Some(syn::GenericArgument::Type(t)), None) => Some(t),
        _ => None,
    }
}

/// Read an [`Elem`] from a Rust type.
fn classify_elem(ty: &syn::Type, names: &Names) -> Result<Elem, String> {
    match ty {
        syn::Type::Path(p) => {
            let Some(segment) = path_tail(p, &["crate", "syntax"]) else {
                return Err("a qualified path outside `crate::syntax`".to_string());
            };
            if !matches!(segment.arguments, syn::PathArguments::None) {
                return Err(format!("`{}` with type arguments", segment.ident));
            }
            let name = segment.ident.to_string();
            let elem = match name.as_str() {
                "SyntaxToken" => Some(Elem::Token),
                "bool" => Some(Elem::Bool),
                "usize" => Some(Elem::Usize),
                "i32" => Some(Elem::I32),
                "TextRange" => Some(Elem::TextRange),
                _ => names.elem_for(&name),
            };
            elem.ok_or_else(|| {
                format!(
                    "`{name}`, which is neither a scalar the shape set knows nor a node, \
                     dispatch enum or value enum in astgen's tables"
                )
            })
        }
        syn::Type::Reference(r)
            if r.mutability.is_none()
                && r.lifetime.as_ref().is_some_and(|l| l.ident == "static") =>
        {
            match &*r.elem {
                syn::Type::Path(p)
                    if path_tail(p, &[]).is_some_and(|s| {
                        s.ident == "str" && matches!(s.arguments, syn::PathArguments::None)
                    }) =>
                {
                    Ok(Elem::StaticStr)
                }
                _ => Err("a `&'static` reference to something other than `str`".to_string()),
            }
        }
        syn::Type::Tuple(t) if t.elems.len() >= 2 => t
            .elems
            .iter()
            .map(|e| classify_elem(e, names))
            .collect::<Result<Vec<_>, _>>()
            .map(Elem::Tuple),
        _ => Err("a type outside the shape set (a reference, slice, unit, trait object, …)".into()),
    }
}

/// The `Item` of `impl Iterator<Item = T> [+ 'lifetime]`, or why it is not one.
fn iterator_item(
    bounds: &syn::punctuated::Punctuated<syn::TypeParamBound, syn::Token![+]>,
) -> Result<&syn::Type, String> {
    let mut item = None;
    for bound in bounds {
        match bound {
            syn::TypeParamBound::Lifetime(_) => {}
            syn::TypeParamBound::Trait(t)
                if item.is_none()
                    && matches!(t.modifier, syn::TraitBoundModifier::None)
                    && t.lifetimes.is_none() =>
            {
                let [segment] = t.path.segments.iter().collect::<Vec<_>>()[..] else {
                    return Err("an `impl` trait other than `Iterator`".to_string());
                };
                if segment.ident != "Iterator" || t.path.leading_colon.is_some() {
                    return Err(format!("`impl {}`, not `impl Iterator`", segment.ident));
                }
                let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
                    return Err("`impl Iterator` without `Item = …`".to_string());
                };
                let [syn::GenericArgument::AssocType(assoc)] =
                    args.args.iter().collect::<Vec<_>>()[..]
                else {
                    return Err("`impl Iterator<…>` other than `Item = …`".to_string());
                };
                if assoc.ident != "Item" || assoc.generics.is_some() {
                    return Err("`impl Iterator<…>` other than `Item = …`".to_string());
                }
                item = Some(&assoc.ty);
            }
            _ => return Err("an `impl` bound other than `Iterator` and a lifetime".to_string()),
        }
    }
    item.ok_or_else(|| "an `impl` type with no `Iterator` bound".to_string())
}

/// Read a [`Shape`] from a Rust type, or say why it is outside the set.
pub fn classify(ty: &syn::Type, names: &Names) -> Result<Shape, String> {
    match ty {
        syn::Type::ImplTrait(imp) => {
            let item = iterator_item(&imp.bounds)?;
            classify_elem(item, names).map(Shape::Iter)
        }
        syn::Type::Path(p) => {
            if let Some(segment) = path_tail(p, &[]) {
                let container = segment.ident.to_string();
                if container == "Option" || container == "Vec" {
                    let inner = single_type_arg(segment)
                        .ok_or_else(|| format!("`{container}` without one type argument"))?;
                    let elem = classify_elem(inner, names)?;
                    return Ok(if container == "Option" {
                        Shape::Option(elem)
                    } else {
                        Shape::Vec(elem)
                    });
                }
            }
            classify_elem(ty, names).map(Shape::One)
        }
        _ => classify_elem(ty, names).map(Shape::One),
    }
}

/// Read a [`Shape`] from Rust source text (a table's value-enum field type).
pub fn classify_str(ty: &str, names: &Names) -> Result<Shape, String> {
    let parsed: syn::Type =
        syn::parse_str(ty).map_err(|e| format!("`{ty}` is not a Rust type: {e}"))?;
    classify(&parsed, names)
}

/// A token of the lock spelling: a word, or one of `( ) { } , =`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tok<'a> {
    Word(&'a str),
    Punct(char),
}

pub(crate) fn tokenize(text: &str) -> Result<Vec<Tok<'_>>, String> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, ch) in text.char_indices() {
        let word_char = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-';
        if word_char {
            start.get_or_insert(i);
            continue;
        }
        if let Some(s) = start.take() {
            out.push(Tok::Word(&text[s..i]));
        }
        match ch {
            ' ' => {}
            '(' | ')' | '{' | '}' | ',' | '=' => out.push(Tok::Punct(ch)),
            _ => return Err(format!("unexpected {ch:?} in {text:?}")),
        }
    }
    if let Some(s) = start {
        out.push(Tok::Word(&text[s..]));
    }
    Ok(out)
}

/// A cursor over [`Tok`]s, for the lock spelling's small grammar.
pub(crate) struct Cursor<'a> {
    pub(crate) toks: Vec<Tok<'a>>,
    pub(crate) pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn peek(&self) -> Option<Tok<'a>> {
        self.toks.get(self.pos).copied()
    }

    pub(crate) fn next(&mut self) -> Result<Tok<'a>, String> {
        let tok = self.peek().ok_or("unexpected end")?;
        self.pos += 1;
        Ok(tok)
    }

    pub(crate) fn word(&mut self) -> Result<&'a str, String> {
        match self.next()? {
            Tok::Word(w) => Ok(w),
            Tok::Punct(p) => Err(format!("expected a word, found {p:?}")),
        }
    }

    pub(crate) fn punct(&mut self, want: char) -> Result<(), String> {
        match self.next()? {
            Tok::Punct(p) if p == want => Ok(()),
            other => Err(format!("expected {want:?}, found {other:?}")),
        }
    }

    pub(crate) fn at_end(&self) -> bool {
        self.pos == self.toks.len()
    }
}

fn ident(word: &str) -> Result<String, String> {
    let ok = word.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(word.to_string())
    } else {
        Err(format!("{word:?} is not a type name"))
    }
}

fn parse_elem(cur: &mut Cursor<'_>) -> Result<Elem, String> {
    match cur.next()? {
        Tok::Word("node") => Ok(Elem::Node(ident(cur.word()?)?)),
        Tok::Word("enum") => Ok(Elem::Enum(ident(cur.word()?)?)),
        Tok::Word("value") => Ok(Elem::Value(ident(cur.word()?)?)),
        Tok::Word("token") => Ok(Elem::Token),
        Tok::Word("bool") => Ok(Elem::Bool),
        Tok::Word("usize") => Ok(Elem::Usize),
        Tok::Word("i32") => Ok(Elem::I32),
        Tok::Word("text-range") => Ok(Elem::TextRange),
        Tok::Word("static-str") => Ok(Elem::StaticStr),
        Tok::Punct('(') => {
            let mut items = vec![parse_elem(cur)?];
            while cur.peek() == Some(Tok::Punct(',')) {
                cur.next()?;
                items.push(parse_elem(cur)?);
            }
            cur.punct(')')?;
            if items.len() < 2 {
                return Err("a tuple element has at least two members".to_string());
            }
            Ok(Elem::Tuple(items))
        }
        other => Err(format!("{other:?} does not begin an element")),
    }
}

pub(crate) fn parse_shape_at(cur: &mut Cursor<'_>) -> Result<Shape, String> {
    let wrap: fn(Elem) -> Shape = match cur.peek() {
        Some(Tok::Word("option")) => Shape::Option,
        Some(Tok::Word("iter")) => Shape::Iter,
        Some(Tok::Word("vec")) => Shape::Vec,
        _ => return parse_elem(cur).map(Shape::One),
    };
    cur.next()?;
    parse_elem(cur).map(wrap)
}

/// Read a [`Shape`] from its lock spelling ([`Shape::render`]'s inverse).
pub fn parse_shape(text: &str) -> Result<Shape, String> {
    let mut cur = Cursor {
        toks: tokenize(text)?,
        pos: 0,
    };
    let shape = parse_shape_at(&mut cur)?;
    if !cur.at_end() {
        return Err(format!("trailing text after the shape in {text:?}"));
    }
    Ok(shape)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        Names {
            nodes: ["LongIdent", "ParenType"].map(String::from).into(),
            enums: ["Expr", "Type"].map(String::from).into(),
            values: ["TupleSegment"].map(String::from).into(),
        }
    }

    #[track_caller]
    fn shape_of(ty: &str) -> String {
        classify_str(ty, &names())
            .unwrap_or_else(|e| panic!("{ty}: {e}"))
            .render()
    }

    #[test]
    fn the_shape_set_reads_every_form() {
        for (ty, want) in [
            ("bool", "bool"),
            ("usize", "usize"),
            ("Option<i32>", "option i32"),
            ("Option<Expr>", "option enum Expr"),
            ("Option<LongIdent>", "option node LongIdent"),
            ("Option<SyntaxToken>", "option token"),
            ("Option<crate::syntax::TextRange>", "option text-range"),
            (
                "Option<(&'static str, crate::syntax::TextRange)>",
                "option (static-str, text-range)",
            ),
            ("impl Iterator<Item = Type> + '_", "iter enum Type"),
            ("impl Iterator<Item = Type>", "iter enum Type"),
            ("impl Iterator<Item = SyntaxToken> + '_", "iter token"),
            ("Vec<TupleSegment>", "vec value TupleSegment"),
            ("TupleSegment", "value TupleSegment"),
        ] {
            assert_eq!(shape_of(ty), want, "{ty}");
            assert_eq!(parse_shape(want).unwrap().render(), want);
        }
    }

    #[test]
    fn a_type_outside_the_set_is_refused() {
        for ty in [
            "String",
            "&str",
            "Option<Option<Expr>>",
            "Vec<Vec<Type>>",
            "impl DoubleEndedIterator<Item = Type>",
            "impl Iterator<Item = Type> + Clone",
            "Box<dyn Iterator<Item = Type>>",
            "Unknown",
            "Option<Unknown>",
            "other::LongIdent",
            "()",
            "(Expr,)",
            "&'static Expr",
        ] {
            assert!(classify_str(ty, &names()).is_err(), "{ty} was classified");
        }
    }

    #[test]
    fn a_malformed_spelling_is_refused() {
        for text in [
            "",
            "option",
            "option option token",
            "node",
            "node Bad-Name",
            "(token)",
            "(token, bool",
            "token token",
            "string",
        ] {
            assert!(parse_shape(text).is_err(), "{text:?} parsed");
        }
    }
}
