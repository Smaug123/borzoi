//! A facade's lock file (`docs/ast-versioning-plan.md` D6): its typed surface,
//! one fact per line.
//!
//! Each line is a tab-separated record whose first field is its tag:
//!
//! | tag        | fields                      | one line per                     |
//! |------------|-----------------------------|----------------------------------|
//! | `node`     | node, `SyntaxKind`          | kind a node newtype casts from   |
//! | `enum`     | dispatch enum, variant, node| dispatch-enum variant            |
//! | `value`    | value enum, variant, payload| value-enum variant               |
//! | `derive`   | value enum, trait           | trait a value enum derives       |
//! | `accessor` | owner node, method, shape   | public accessor                  |
//!
//! A shape is [`Shape::render`]'s spelling (`option enum Expr`, `iter node
//! Binding`, `bool`); a payload is `unit`, a tuple of shapes `(token)`, or
//! named fields `{expr = enum Expr, qualifier = option token}`. Node newtypes
//! and dispatch enums all derive the same traits, so only value enums carry
//! `derive` lines. Lines starting `#` are comments.
//!
//! The records are sorted by tag (in the table's order), then field by field,
//! with no duplicates; [`parse`] refuses any other order, so a lock has one
//! spelling and [`render`] is its inverse. Every fact is its own line, so the
//! changes a frozen surface may take — a new accessor, a new kind for an
//! existing node — are added lines, and everything else (a removed or changed
//! accessor, a new variant in an exhaustively matched enum) removes or
//! rewrites one.

use std::collections::{BTreeMap, BTreeSet};

use crate::shape::{Cursor, Names, Shape, Tok, parse_shape, parse_shape_at, tokenize};

/// What a value-enum variant carries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Payload {
    /// Nothing: `Variant`.
    Unit,
    /// Positional fields: `Variant(A, B)`.
    Tuple(Vec<Shape>),
    /// Named fields, in declaration order: `Variant { a: A, b: B }`.
    Named(Vec<(String, Shape)>),
}

impl Payload {
    fn render(&self) -> String {
        match self {
            Payload::Unit => "unit".to_string(),
            Payload::Tuple(shapes) => {
                let parts: Vec<String> = shapes.iter().map(Shape::render).collect();
                format!("({})", parts.join(", "))
            }
            Payload::Named(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(name, shape)| format!("{name} = {}", shape.render()))
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
        }
    }

    fn parse(text: &str) -> Result<Payload, String> {
        let mut cur = Cursor {
            toks: tokenize(text)?,
            pos: 0,
        };
        let payload = match cur.next()? {
            Tok::Word("unit") => Payload::Unit,
            Tok::Punct('(') => {
                let mut shapes = vec![parse_shape_at(&mut cur)?];
                while cur.peek() == Some(Tok::Punct(',')) {
                    cur.next()?;
                    shapes.push(parse_shape_at(&mut cur)?);
                }
                cur.punct(')')?;
                Payload::Tuple(shapes)
            }
            Tok::Punct('{') => {
                let mut fields = Vec::new();
                loop {
                    let name = field_name(cur.word()?)?;
                    cur.punct('=')?;
                    fields.push((name, parse_shape_at(&mut cur)?));
                    match cur.next()? {
                        Tok::Punct(',') => {}
                        Tok::Punct('}') => break,
                        other => return Err(format!("expected ',' or '}}', found {other:?}")),
                    }
                }
                Payload::Named(fields)
            }
            other => return Err(format!("{other:?} does not begin a payload")),
        };
        if !cur.at_end() {
            return Err(format!("trailing text after the payload in {text:?}"));
        }
        Ok(payload)
    }

    fn shapes(&self) -> Vec<&Shape> {
        match self {
            Payload::Unit => Vec::new(),
            Payload::Tuple(shapes) => shapes.iter().collect(),
            Payload::Named(fields) => fields.iter().map(|(_, s)| s).collect(),
        }
    }
}

/// One record of a lock. The variant order is the lock's section order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Line {
    /// Node newtype `node` casts from `kind`.
    Node { node: String, kind: String },
    /// Dispatch enum `name` has `variant(node)`.
    Enum {
        name: String,
        variant: String,
        node: String,
    },
    /// Value enum `name` has `variant` carrying `payload`.
    Value {
        name: String,
        variant: String,
        payload: Payload,
    },
    /// Value enum `name` derives `trait_`.
    Derive { name: String, trait_: String },
    /// Node `owner` has `pub fn name(&self) -> shape`.
    Accessor {
        owner: String,
        name: String,
        shape: Shape,
    },
}

impl Line {
    fn render(&self) -> String {
        match self {
            Line::Node { node, kind } => format!("node\t{node}\t{kind}"),
            Line::Enum {
                name,
                variant,
                node,
            } => format!("enum\t{name}\t{variant}\t{node}"),
            Line::Value {
                name,
                variant,
                payload,
            } => format!("value\t{name}\t{variant}\t{}", payload.render()),
            Line::Derive { name, trait_ } => format!("derive\t{name}\t{trait_}"),
            Line::Accessor { owner, name, shape } => {
                format!("accessor\t{owner}\t{name}\t{}", shape.render())
            }
        }
    }

    fn parse(text: &str) -> Result<Line, String> {
        let fields: Vec<&str> = text.split('\t').collect();
        let ty = |s: &str| type_name(s);
        match fields[..] {
            ["node", node, kind] => Ok(Line::Node {
                node: ty(node)?,
                kind: kind_name(kind)?,
            }),
            ["enum", name, variant, node] => Ok(Line::Enum {
                name: ty(name)?,
                variant: ty(variant)?,
                node: ty(node)?,
            }),
            ["value", name, variant, payload] => Ok(Line::Value {
                name: ty(name)?,
                variant: ty(variant)?,
                payload: Payload::parse(payload)?,
            }),
            ["derive", name, trait_] => Ok(Line::Derive {
                name: ty(name)?,
                trait_: ty(trait_)?,
            }),
            ["accessor", owner, name, shape] => Ok(Line::Accessor {
                owner: ty(owner)?,
                name: field_name(name)?,
                shape: parse_shape(shape)?,
            }),
            _ => Err("not a record: an unknown tag, or the wrong number of fields".to_string()),
        }
    }
}

fn check_word(
    text: &str,
    first: fn(char) -> bool,
    rest: fn(char) -> bool,
    what: &str,
) -> Result<String, String> {
    let mut chars = text.chars();
    if chars.next().is_some_and(first) && chars.all(rest) {
        Ok(text.to_string())
    } else {
        Err(format!("{text:?} is not {what}"))
    }
}

/// `CamelCase` Rust type or variant name.
fn type_name(text: &str) -> Result<String, String> {
    check_word(
        text,
        |c| c.is_ascii_uppercase(),
        |c| c.is_ascii_alphanumeric(),
        "a type name",
    )
}

/// `snake_case` method or field name.
fn field_name(text: &str) -> Result<String, String> {
    check_word(
        text,
        |c| c.is_ascii_lowercase() || c == '_',
        |c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_',
        "a method or field name",
    )
}

/// `SCREAMING_SNAKE` `SyntaxKind` name.
fn kind_name(text: &str) -> Result<String, String> {
    check_word(
        text,
        |c| c.is_ascii_uppercase(),
        |c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_',
        "a SyntaxKind name",
    )
}

/// A facade's typed surface: the set of its lock's records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lock {
    pub lines: BTreeSet<Line>,
}

/// The lock's text: a comment header naming `facade`, then every record in
/// order.
pub fn render(facade: &str, lock: &Lock) -> String {
    let mut out = format!(
        "# The `{facade}` typed facade's surface, one fact per line; the format is \
         documented in\n\
         # `tools/astgen/src/lock.rs`. GENERATED by `tools/astgen`: regenerate with\n\
         # `cargo run -p borzoi-astgen --example generate`.\n"
    );
    for line in &lock.lines {
        out.push_str(&line.render());
        out.push('\n');
    }
    out
}

/// Read a lock's text, refusing a malformed record and any record out of order
/// or repeated (so a lock has exactly one spelling).
pub fn parse(text: &str) -> Result<Lock, String> {
    let mut lines = BTreeSet::new();
    let mut previous: Option<Line> = None;
    for (number, raw) in text.lines().enumerate() {
        let number = number + 1;
        if raw.starts_with('#') {
            continue;
        }
        let line = Line::parse(raw).map_err(|e| format!("line {number}: {e}: {raw:?}"))?;
        if line.render() != raw {
            return Err(format!(
                "line {number} is not in canonical spelling ({:?}): {raw:?}",
                line.render()
            ));
        }
        if previous.as_ref().is_some_and(|p| *p >= line) {
            return Err(format!(
                "line {number} is out of order or repeated: {raw:?}"
            ));
        }
        previous = Some(line.clone());
        lines.insert(line);
    }
    Ok(Lock { lines })
}

impl Lock {
    /// The type names this lock declares, by what each one is.
    pub fn names(&self) -> Names {
        let mut names = Names::default();
        for line in &self.lines {
            match line {
                Line::Node { node, .. } => {
                    names.nodes.insert(node.clone());
                }
                Line::Enum { name, .. } => {
                    names.enums.insert(name.clone());
                }
                Line::Value { name, .. } => {
                    names.values.insert(name.clone());
                }
                Line::Derive { .. } | Line::Accessor { .. } => {}
            }
        }
        names
    }

    /// Every way the records fail to describe one coherent surface: a name
    /// used for two sorts of type, a kind cast by two nodes, an enum variant
    /// declared twice or over an undeclared node, a value-enum variant
    /// declared twice, a `derive` on something other than a value enum, an
    /// accessor declared twice or on something other than a node, or a shape
    /// naming an undeclared type.
    pub fn check(&self) -> Vec<String> {
        let names = self.names();
        let mut errors = Vec::new();
        for (a, b, set_a, set_b) in [
            ("node", "dispatch enum", &names.nodes, &names.enums),
            ("node", "value enum", &names.nodes, &names.values),
            ("dispatch enum", "value enum", &names.enums, &names.values),
        ] {
            for name in set_a.intersection(set_b) {
                errors.push(format!("`{name}` is both a {a} and a {b}"));
            }
        }
        let mut kind_owner: BTreeMap<&str, &str> = BTreeMap::new();
        let mut enum_variants: BTreeMap<(&str, &str), &str> = BTreeMap::new();
        let mut value_variants: BTreeSet<(&str, &str)> = BTreeSet::new();
        let mut accessors: BTreeSet<(&str, &str)> = BTreeSet::new();
        let resolve = |shape: &Shape, what: String, errors: &mut Vec<String>| {
            if !names.resolves(shape) {
                errors.push(format!(
                    "{what} names an undeclared type: {}",
                    shape.render()
                ));
            }
        };
        for line in &self.lines {
            match line {
                Line::Node { node, kind } => {
                    if let Some(other) = kind_owner.insert(kind, node) {
                        errors.push(format!("`{kind}` is cast by both `{other}` and `{node}`"));
                    }
                }
                Line::Enum {
                    name,
                    variant,
                    node,
                } => {
                    if enum_variants.insert((name, variant), node).is_some() {
                        errors.push(format!("`{name}::{variant}` is declared twice"));
                    }
                    if !names.nodes.contains(node) {
                        errors.push(format!("`{name}::{variant}` holds `{node}`, not a node"));
                    }
                }
                Line::Value {
                    name,
                    variant,
                    payload,
                } => {
                    if !value_variants.insert((name, variant)) {
                        errors.push(format!("`{name}::{variant}` is declared twice"));
                    }
                    for shape in payload.shapes() {
                        resolve(shape, format!("`{name}::{variant}`"), &mut errors);
                    }
                }
                Line::Derive { name, trait_ } => {
                    if !names.values.contains(name) {
                        errors.push(format!("`{name}` derives `{trait_}` but is no value enum"));
                    }
                }
                Line::Accessor { owner, name, shape } => {
                    if !accessors.insert((owner, name)) {
                        errors.push(format!("accessor `{owner}::{name}` is declared twice"));
                    }
                    if !names.nodes.contains(owner) {
                        errors.push(format!("accessor `{owner}::{name}` is not on a node"));
                    }
                    resolve(shape, format!("accessor `{owner}::{name}`"), &mut errors);
                }
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::Elem;

    fn sample() -> Lock {
        let s = String::from;
        Lock {
            lines: [
                Line::Node {
                    node: s("AppExpr"),
                    kind: s("APP_EXPR"),
                },
                Line::Node {
                    node: s("AppExpr"),
                    kind: s("INFIX_APP_EXPR"),
                },
                Line::Enum {
                    name: s("Expr"),
                    variant: s("App"),
                    node: s("AppExpr"),
                },
                Line::Value {
                    name: s("Part"),
                    variant: s("Fill"),
                    payload: Payload::Named(vec![
                        (s("expr"), Shape::One(Elem::Enum(s("Expr")))),
                        (s("qualifier"), Shape::Option(Elem::Token)),
                    ]),
                },
                Line::Value {
                    name: s("Part"),
                    variant: s("Text"),
                    payload: Payload::Tuple(vec![Shape::One(Elem::Token)]),
                },
                Line::Value {
                    name: s("Part"),
                    variant: s("Empty"),
                    payload: Payload::Unit,
                },
                Line::Derive {
                    name: s("Part"),
                    trait_: s("Clone"),
                },
                Line::Accessor {
                    owner: s("AppExpr"),
                    name: s("func"),
                    shape: Shape::Option(Elem::Enum(s("Expr"))),
                },
                Line::Accessor {
                    owner: s("AppExpr"),
                    name: s("parts"),
                    shape: Shape::Vec(Elem::Value(s("Part"))),
                },
                Line::Accessor {
                    owner: s("AppExpr"),
                    name: s("op_name"),
                    shape: Shape::Option(Elem::Tuple(vec![Elem::StaticStr, Elem::TextRange])),
                },
            ]
            .into(),
        }
    }

    #[test]
    fn a_lock_round_trips_through_its_text() {
        let lock = sample();
        assert!(lock.check().is_empty(), "{:#?}", lock.check());
        let text = render("test", &lock);
        assert_eq!(parse(&text).unwrap(), lock);
        assert_eq!(render("test", &parse(&text).unwrap()), text);
    }

    #[test]
    fn a_lock_out_of_order_or_repeated_is_refused() {
        let text = render("test", &sample());
        let records: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        let mut swapped = records.clone();
        swapped.swap(0, 1);
        assert!(parse(&swapped.join("\n")).is_err());
        let mut repeated = records.clone();
        repeated.insert(1, records[0]);
        assert!(parse(&repeated.join("\n")).is_err());
    }

    #[test]
    fn a_malformed_record_is_refused() {
        for text in [
            "node\tAppExpr",
            "node\tAppExpr\tapp_expr",
            "nodes\tAppExpr\tAPP_EXPR",
            "enum\tExpr\tApp",
            "value\tPart\tFill\t{expr = enum Expr",
            "value\tPart\tFill\t()",
            "accessor\tAppExpr\tFunc\tbool",
            "accessor\tAppExpr\tfunc\tstring",
            "accessor\tAppExpr\tfunc\toption  token",
            "accessor AppExpr func bool",
        ] {
            assert!(parse(text).is_err(), "{text:?} parsed");
        }
    }

    #[test]
    fn an_incoherent_lock_is_reported() {
        let s = String::from;
        let mut lock = sample();
        lock.lines.insert(Line::Node {
            node: s("OtherExpr"),
            kind: s("APP_EXPR"),
        });
        lock.lines.insert(Line::Enum {
            name: s("Expr"),
            variant: s("App"),
            node: s("OtherExpr"),
        });
        lock.lines.insert(Line::Accessor {
            owner: s("Expr"),
            name: s("x"),
            shape: Shape::One(Elem::Node(s("Missing"))),
        });
        lock.lines.insert(Line::Derive {
            name: s("Expr"),
            trait_: s("Copy"),
        });
        let errors = lock.check().join("\n");
        for needle in [
            "`APP_EXPR` is cast by both",
            "`Expr::App` is declared twice",
            "accessor `Expr::x` is not on a node",
            "names an undeclared type",
            "`Expr` derives `Copy`",
        ] {
            assert!(errors.contains(needle), "missing {needle:?} in:\n{errors}");
        }
    }
}
