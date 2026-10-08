//! The differential over a generated case space: every declaration kind hover
//! can reach, crossed with the attachment hazards — attributes, blank lines,
//! ordinary comments between doc lines, `(*)` inside a block comment, `#if`
//! regions live and dead, a doc comment trailing code, `and` groups, the doc
//! text shapes the implicit-`<summary>` rule distinguishes.
//!
//! [`table`] is the exhaustive cross product (each hazard applied to every
//! slot of every template) as one project; [`random_files`] composes
//! templates and hazards freely, so interactions between neighbouring
//! declarations — a doc dropped by one landing on the next — are searched for
//! rather than enumerated.

use proptest::prelude::*;

use super::harness::{Graded, Verdict, census, describe, failures, run_fixture};

/// Doc-comment texts (what follows `///`), chosen for the elaboration rule:
/// leading `<` or not, blank, spaces vs tab, characters the implicit summary
/// escapes, well- and ill-formed XML.
const DOC_TEXTS: &[&str] = &[
    " plain text",
    "",
    "   ",
    "<summary>sum</summary>",
    " <summary>",
    " </summary>",
    " a & b < c > d \"q\" 'a'",
    "<para>para</para>",
    "\ttabbed",
    " <param name=\"x\">the x</param>",
    " unclosed <b",
    "<returns>r</returns>",
    " <inheritdoc/>",
    "\t",
    "\t<para>t</para>",
    " caf\u{e9} \u{65e5}\u{672c} \u{1f600} wide",
];

/// One line (or block) of trivia before a declaration.
#[derive(Debug, Clone)]
enum Piece {
    Doc(usize),
    Blank,
    LineComment,
    FourSlash,
    Block,
    /// A block comment containing `(*)`, an immediate grab point in FCS.
    BlockGrab,
    BlockMultiline,
    /// `#if DEFINED_X` (live) or `#if UNDEFINED_X` (dead) around doc lines.
    IfDef {
        live: bool,
        inner: Vec<usize>,
    },
    /// `#if UNDEFINED_X` … `#else` … `#endif`.
    IfElse {
        dead: Vec<usize>,
        live: Vec<usize>,
    },
    /// A warning directive, consumed by the lexer like a conditional one.
    Nowarn,
    /// A doc line indented by a tab.
    TabDoc(usize),
    /// `#light "on"`, which FCS's lexer consumes without a grab point.
    Light,
}

impl Piece {
    fn render(&self, indent: &str, out: &mut String) {
        let doc = |i: &usize, out: &mut String| {
            out.push_str(indent);
            out.push_str("///");
            out.push_str(DOC_TEXTS[*i]);
            out.push('\n');
        };
        match self {
            Piece::Doc(i) => doc(i, out),
            Piece::Blank => out.push('\n'),
            Piece::LineComment => {
                out.push_str(indent);
                out.push_str("// ordinary\n");
            }
            Piece::FourSlash => {
                out.push_str(indent);
                out.push_str("//// four\n");
            }
            Piece::Block => {
                out.push_str(indent);
                out.push_str("(* block *)\n");
            }
            Piece::BlockGrab => {
                out.push_str(indent);
                out.push_str("(* a (*) b *)\n");
            }
            Piece::BlockMultiline => {
                out.push_str(indent);
                out.push_str("(* one\n");
                out.push_str(indent);
                out.push_str("   two *)\n");
            }
            Piece::IfDef { live, inner } => {
                out.push_str(if *live {
                    "#if DEFINED_X\n"
                } else {
                    "#if UNDEFINED_X\n"
                });
                for i in inner {
                    doc(i, out);
                }
                out.push_str("#endif\n");
            }
            Piece::Nowarn => out.push_str("#nowarn \"40\"\n"),
            Piece::Light => out.push_str("#light \"on\"\n"),
            Piece::TabDoc(i) => {
                out.push_str(indent);
                out.push('\t');
                out.push_str("///");
                out.push_str(DOC_TEXTS[*i]);
                out.push('\n');
            }
            Piece::IfElse { dead, live } => {
                out.push_str("#if UNDEFINED_X\n");
                for i in dead {
                    doc(i, out);
                }
                out.push_str("#else\n");
                for i in live {
                    doc(i, out);
                }
                out.push_str("#endif\n");
            }
        }
    }
}

fn render_prelude(pieces: &[Piece], indent: &str, out: &mut String) {
    for p in pieces {
        p.render(indent, out);
    }
}

/// A declaration template. Each declares names suffixed with the item's index
/// and uses them once afterwards, so use sites are graded too.
#[derive(Debug, Clone, Copy)]
enum Template {
    Value,
    Function,
    Inline,
    Mutable,
    Private,
    LiteralAfterLet,
    Tuple,
    AsPattern,
    ArrayPattern,
    OptionPattern,
    RecordPattern,
    ParenHead,
    TypedHead,
    RecAnd,
    ActivePattern,
    PartialActivePattern,
    Abbrev,
    Union,
    BarlessUnion,
    Enum,
    Class,
    Exception,
    TypeAnd,
    Local,
    LocalUse,
    StaticLet,
    Augmentation,
    NestedModule,
    NamedArguments,
    ArityNamesakes,
    ExplicitConstructor,
    PrimaryConstructorDoc,
    AttributeType,
    TypeArgumentPositions,
    Measure,
    QualifierNamesakes,
    AccessorDoc,
    InnerTypeDoc,
    ValField,
    AutoOpenModule,
    RecModule,
    Struct,
}

const TEMPLATES: &[Template] = &[
    Template::Value,
    Template::Function,
    Template::Inline,
    Template::Mutable,
    Template::Private,
    Template::LiteralAfterLet,
    Template::Tuple,
    Template::AsPattern,
    Template::ArrayPattern,
    Template::OptionPattern,
    Template::RecordPattern,
    Template::ParenHead,
    Template::TypedHead,
    Template::RecAnd,
    Template::ActivePattern,
    Template::PartialActivePattern,
    Template::Abbrev,
    Template::Union,
    Template::BarlessUnion,
    Template::Enum,
    Template::Class,
    Template::Exception,
    Template::TypeAnd,
    Template::Local,
    Template::LocalUse,
    Template::StaticLet,
    Template::Augmentation,
    Template::NestedModule,
    Template::NamedArguments,
    Template::ArityNamesakes,
    Template::ExplicitConstructor,
    Template::PrimaryConstructorDoc,
    Template::AttributeType,
    Template::TypeArgumentPositions,
    Template::Measure,
    Template::QualifierNamesakes,
    Template::AccessorDoc,
    Template::InnerTypeDoc,
    Template::ValField,
    Template::AutoOpenModule,
    Template::RecModule,
    Template::Struct,
];

/// How many inner prelude slots a template has.
const SLOTS: usize = 5;

const ATTR: &str = "[<System.Obsolete(\"o\")>]";

/// One top-level item: trivia before it, the declaration, its inner slots, an
/// optional attribute (with a slot between it and the keyword) and an optional
/// doc comment trailing its last line.
#[derive(Debug, Clone)]
struct Item {
    prelude: Vec<Piece>,
    template: Template,
    attr: bool,
    slots: Vec<Vec<Piece>>,
    trailing: Option<usize>,
}

impl Item {
    fn render(&self, n: usize, out: &mut String) {
        render_prelude(&self.prelude, "", out);
        let slot = |k: usize, indent: &str, out: &mut String| {
            if let Some(s) = self.slots.get(k) {
                render_prelude(s, indent, out);
            }
        };
        // An attribute line before the keyword, with a slot between the two.
        let attr = |out: &mut String| {
            if self.attr {
                out.push_str(ATTR);
                out.push('\n');
                slot(4, "", out);
            }
        };
        match self.template {
            Template::Value => {
                attr(out);
                out.push_str(&format!("let v{n} = 1\nlet _ = v{n}"));
            }
            Template::Function => {
                attr(out);
                out.push_str(&format!("let f{n} (x: int) = x\nlet _ = f{n} 1"));
            }
            Template::Inline => {
                attr(out);
                out.push_str(&format!("let inline g{n} x = x\nlet _ = g{n} 1"));
            }
            Template::Mutable => {
                attr(out);
                out.push_str(&format!("let mutable mu{n} = 0\nlet _ = mu{n}"));
            }
            Template::Private => {
                attr(out);
                out.push_str(&format!("let private p{n} = 1\nlet _ = p{n}"));
            }
            Template::LiteralAfterLet => {
                out.push_str(&format!("let [<Literal>] L{n} = 1\nlet _ = L{n}"));
            }
            Template::Tuple => {
                attr(out);
                out.push_str(&format!("let a{n}, b{n} = 1, 2\nlet _ = a{n} + b{n}"));
            }
            Template::AsPattern => {
                attr(out);
                out.push_str(&format!(
                    "let (c{n}, d{n}) as e{n} = 1, 2\nlet _ = c{n}, d{n}, e{n}"
                ));
            }
            Template::ArrayPattern => {
                attr(out);
                out.push_str(&format!(
                    "let [| ap{n}; aq{n} |] = [| 1; 2 |]\nlet _ = ap{n} + aq{n}"
                ));
            }
            Template::OptionPattern => {
                attr(out);
                out.push_str(&format!("let (Some os{n}) = Some 1\nlet _ = os{n}"));
            }
            Template::RecordPattern => {
                out.push_str(&format!("type RP{n} = {{ RF{n}: int }}\n"));
                slot(0, "", out);
                out.push_str(&format!(
                    "let {{ RF{n} = rf{n} }} = {{ RF{n} = 1 }}\nlet _ = rf{n}"
                ));
            }
            Template::ParenHead => {
                attr(out);
                out.push_str(&format!("let (pv{n}) = 1\nlet _ = pv{n}"));
            }
            Template::TypedHead => {
                attr(out);
                out.push_str(&format!("let (tv{n}: int) = 1\nlet _ = tv{n}"));
            }
            Template::RecAnd => {
                attr(out);
                out.push_str(&format!(
                    "let rec r{n} x = if x > 0 then s{n} (x - 1) else 0\n"
                ));
                slot(0, "", out);
                out.push_str("and\n");
                slot(1, "    ", out);
                out.push_str(&format!("    s{n} x = r{n} x\nlet _ = s{n} 1"));
            }
            Template::ActivePattern => {
                attr(out);
                out.push_str(&format!(
                    "let (|Ev{n}|Od{n}|) x = if x % 2 = 0 then Ev{n} else Od{n}\n\
                     let _ = match 1 with Ev{n} -> 0 | Od{n} -> 1"
                ));
            }
            Template::PartialActivePattern => {
                out.push_str(&format!(
                    "let (|Pos{n}|_|) x = if x > 0 then Some x else None\n\
                     let _ = match 1 with Pos{n} p -> p | _ -> 0"
                ));
            }
            Template::Abbrev => {
                attr(out);
                out.push_str(&format!("type T{n} = int\nlet _ = (1 : T{n})"));
            }
            Template::Union => {
                attr(out);
                out.push_str(&format!("type U{n} =\n"));
                slot(0, "    ", out);
                out.push_str(&format!("    | A{n}\n"));
                slot(1, "    ", out);
                out.push_str(&format!("    | B{n} of int\nlet _ = B{n} 1, A{n}"));
            }
            Template::BarlessUnion => {
                out.push_str(&format!("type W{n} =\n"));
                slot(0, "    ", out);
                out.push_str(&format!("    C{n} of int\n"));
                slot(1, "    ", out);
                out.push_str(&format!("    | D{n}\nlet _ = C{n} 1, D{n}"));
            }
            Template::Enum => {
                attr(out);
                out.push_str(&format!("type E{n} =\n"));
                slot(0, "    ", out);
                out.push_str(&format!("    | X{n} = 0\n"));
                slot(1, "    ", out);
                out.push_str(&format!("    | Y{n} = 1\nlet _ = E{n}.Y{n}"));
            }
            Template::Class => {
                attr(out);
                out.push_str(&format!("type K{n}() as self{n} =\n"));
                slot(0, "    ", out);
                out.push_str(&format!("    let y{n} = 1\n"));
                slot(1, "    ", out);
                out.push_str(&format!("    static member S{n} = 1\n"));
                slot(2, "    ", out);
                out.push_str(&format!("    member x.I{n} = y{n}, self{n}, x\n"));
                slot(3, "    ", out);
                out.push_str(&format!("    static member val V{n} = 1 with get, set\n"));
                slot(4, "    ", out);
                out.push_str(&format!(
                    "    static member P{n} with get () = 1 and set (v: int) = ()\n"
                ));
                out.push_str(&format!(
                    "let _ = K{n}.S{n}, K{n}.V{n}, K{n}.P{n}, K{n}(), new K{n}()"
                ));
            }
            Template::Exception => {
                attr(out);
                out.push_str(&format!("exception Ex{n} of string\nlet _ = Ex{n} \"m\""));
            }
            Template::TypeAnd => {
                attr(out);
                out.push_str(&format!("type T{n} = int\n"));
                slot(0, "", out);
                out.push_str("and\n");
                slot(1, "    ", out);
                out.push_str(&format!("    Q{n} = string\nlet _ = (\"\" : Q{n})"));
            }
            Template::Local => {
                out.push_str(&format!("let outer{n} () =\n"));
                slot(0, "    ", out);
                out.push_str(&format!("    let z{n} = 1\n"));
                slot(1, "    ", out);
                out.push_str(&format!("    let inline w{n} q = q + z{n}\n    w{n} z{n}"));
            }
            Template::LocalUse => {
                out.push_str(&format!("let user{n} () =\n"));
                slot(0, "    ", out);
                out.push_str(&format!(
                    "    use d{n} = {{ new System.IDisposable with member _.Dispose() = () }}\n    d{n}.Dispose()"
                ));
            }
            Template::Augmentation => {
                attr(out);
                out.push_str(&format!("type G{n} =\n    | GA{n}\n"));
                slot(0, "", out);
                out.push_str(&format!(
                    "type G{n} with\n    static member Z{n} = GA{n}\nlet _ = G{n}.Z{n}, (GA{n} : G{n})"
                ));
            }
            Template::NestedModule => {
                attr(out);
                out.push_str(&format!("module N{n} =\n"));
                slot(0, "    ", out);
                out.push_str(&format!("    let nv{n} = 1\nlet _ = N{n}.nv{n}"));
            }
            // Named and optional named arguments whose labels are also
            // documented locals: a label is the parameter, never the local.
            Template::NamedArguments => {
                out.push_str(&format!(
                    "type NA{n}(q{n}: int, ?o{n}: int) =\n    member _.X = q{n}\nlet call{n} () =\n"
                ));
                slot(0, "    ", out);
                out.push_str(&format!("    let o{n} = 1\n"));
                slot(1, "    ", out);
                out.push_str(&format!(
                    "    let q{n} = 2\n    NA{n}(?o{n} = Some o{n}, q{n} = q{n}), (q{n} = 2)"
                ));
            }
            // Same-named types of different arity (resolution does not choose
            // between them by arity yet, #323).
            Template::ArityNamesakes => {
                attr(out);
                out.push_str(&format!("type AN{n}<'a> = 'a list\n"));
                slot(0, "", out);
                out.push_str(&format!(
                    "type AN{n} = int\nlet _ = ([1] : AN{n}<int>), (1 : AN{n})"
                ));
            }
            // Constructor calls bind the constructor: an explicit `new`'s doc.
            Template::ExplicitConstructor => {
                attr(out);
                out.push_str(&format!("type X{n}(a: int) =\n"));
                slot(0, "    ", out);
                out.push_str(&format!(
                    "    new() = X{n}(1)\n    member _.A = a\nlet _ = new X{n}(), X{n}(2), new X{n}(3)"
                ));
            }
            // The primary constructor's own doc, between the name and `(`.
            Template::PrimaryConstructorDoc => {
                attr(out);
                out.push_str(&format!("type Y{n}\n"));
                slot(0, "    ", out);
                out.push_str(&format!(
                    "    (a: int) =\n    member _.A = a\nlet _ = new Y{n}(1), Y{n}(2), (Y{n} : int -> Y{n})"
                ));
            }
            // An attribute's name is a constructor call of the attribute type.
            Template::AttributeType => {
                attr(out);
                out.push_str(&format!("type Doc{n}Attribute\n"));
                slot(0, "    ", out);
                out.push_str(&format!(
                    "    () =\n    inherit System.Attribute()\n[<Doc{n}>]\nlet attributed{n} = 1\n[<Doc{n}Attribute>]\nlet attributedFull{n} = 2"
                ));
            }
            // A documented type in argument positions of other types' applications.
            Template::TypeArgumentPositions => {
                attr(out);
                out.push_str(&format!("type TA{n} = {{ V{n}: int }}\n"));
                out.push_str(&format!(
                    "let ta{n} : TA{n} list = []\nlet tb{n} = System.Collections.Generic.Stack<TA{n}>()\n\
                     let tc{n} = Unchecked.defaultof<TA{n}>\n\
                     let td{n} : System.Collections.Generic.Dictionary<string, TA{n}> = null\n\
                     let te{n} : (TA{n} * int) list = []"
                ));
            }
            Template::Measure => {
                out.push_str("[<Measure>]\n");
                slot(0, "", out);
                out.push_str(&format!(
                    "type ms{n}\nlet me{n} = 1.0<ms{n}>\nlet mf{n} (x: float<ms{n}>) = x"
                ));
            }
            // `CN.M` binds whichever `CN` declares `M`: here the generic one.
            // Members of each, documented, including one both declare.
            Template::QualifierNamesakes => {
                attr(out);
                out.push_str(&format!("type CN{n} =\n"));
                slot(1, "    ", out);
                out.push_str(&format!("    static member Other{n} = 0\n"));
                slot(2, "    ", out);
                out.push_str(&format!("    static member Both{n} = 0\n"));
                slot(0, "", out);
                out.push_str(&format!("type CN{n}<'a> =\n"));
                slot(3, "    ", out);
                out.push_str(&format!("    static member M{n} = 1\n"));
                slot(1, "    ", out);
                out.push_str(&format!(
                    "    static member Both{n} = 1\nlet _ = CN{n}.M{n}, CN{n}.Other{n}, CN{n}.Both{n}, CN{n}<int>.Both{n}"
                ));
            }
            // A struct's call may bind its generated parameterless constructor.
            Template::Struct => {
                out.push_str("[<Struct>]\n");
                out.push_str(&format!("type ST{n}\n"));
                slot(0, "    ", out);
                out.push_str(&format!(
                    "    (x: int) =\n    member _.X = x\nlet _ = new ST{n}(), new ST{n}(1), ST{n}(2)"
                ));
            }
            // A doc on an accessor, merged by FCS into the property's.
            Template::AccessorDoc => {
                out.push_str(&format!("type PA{n}() =\n"));
                slot(0, "    ", out);
                // `with /// d` then the accessor aligned under it: the only
                // layout the offside rule accepts for a doc before `get`.
                let accessor_doc = self
                    .slots
                    .get(1)
                    .and_then(|s| {
                        s.iter().find_map(|p| match p {
                            Piece::Doc(i) => Some(*i),
                            _ => None,
                        })
                    })
                    .map_or(String::new(), |i| format!(" ///{}", DOC_TEXTS[i]));
                out.push_str(&format!(
                    "    static member Acc{n}\n        with{accessor_doc}\n             get () = 1\nlet _ = PA{n}.Acc{n}"
                ));
            }
            // A doc between `type` and the type's own attributes.
            Template::InnerTypeDoc => {
                out.push_str("type\n");
                slot(0, "    ", out);
                out.push_str(&format!(
                    "    [<System.Obsolete(\"o\")>] TI{n} = int\nlet _ = (1 : TI{n})"
                ));
            }
            Template::ValField => {
                out.push_str(&format!("type VF{n} =\n"));
                slot(0, "    ", out);
                out.push_str(&format!(
                    "    val mutable F{n}: int\n    new() = {{ F{n} = 1 }}\nlet _ = VF{n}().F{n}"
                ));
            }
            Template::AutoOpenModule => {
                out.push_str("[<AutoOpen>]\n");
                slot(0, "", out);
                out.push_str(&format!("module AO{n} =\n"));
                slot(1, "    ", out);
                out.push_str(&format!("    let ao{n} = 1\nlet _ = ao{n}"));
            }
            Template::RecModule => {
                out.push_str(&format!("module rec MR{n} =\n"));
                slot(0, "    ", out);
                out.push_str(&format!("    let mv{n} : MT{n} = 1\n"));
                slot(1, "    ", out);
                out.push_str(&format!("    type MT{n} = int\nlet _ = MR{n}.mv{n}"));
            }
            Template::StaticLet => {
                out.push_str(&format!("type H{n}() =\n"));
                slot(0, "    ", out);
                out.push_str(&format!("    static let sl{n} = 1\n"));
                out.push_str(&format!("    static member G{n} = sl{n}"));
            }
        }
        if let Some(t) = self.trailing {
            out.push_str(" ///");
            out.push_str(DOC_TEXTS[t]);
        }
        out.push('\n');
    }
}

fn render_file(module: usize, items: &[Item]) -> String {
    let mut out = format!("module M{module}\n\n");
    for (n, item) in items.iter().enumerate() {
        item.render(n, &mut out);
    }
    out
}

/// The hazards the table applies to every slot at once.
fn hazards() -> Vec<Vec<Piece>> {
    use Piece::*;
    vec![
        vec![],
        vec![Doc(0)],
        vec![Doc(0), Doc(3)],
        vec![Doc(0), Blank],
        vec![Blank, Doc(0), Blank, Blank],
        vec![Doc(0), LineComment],
        vec![Doc(0), LineComment, Doc(7)],
        vec![LineComment, Doc(0)],
        vec![Doc(0), FourSlash],
        vec![Doc(0), FourSlash, Doc(3)],
        vec![Doc(0), Block],
        vec![Doc(0), Block, Doc(3)],
        vec![Doc(0), BlockMultiline, Doc(9)],
        vec![Doc(0), BlockGrab],
        vec![Doc(1)],
        vec![Doc(2)],
        vec![Doc(2), Doc(0)],
        vec![Doc(1), Doc(3)],
        vec![Doc(4), Doc(0), Doc(5)],
        vec![Doc(6)],
        vec![Doc(8)],
        vec![Doc(8), Doc(3)],
        vec![Doc(10)],
        vec![Doc(11), Doc(0)],
        vec![Doc(12)],
        vec![IfDef {
            live: true,
            inner: vec![0],
        }],
        vec![IfDef {
            live: false,
            inner: vec![0],
        }],
        vec![
            Doc(3),
            IfDef {
                live: false,
                inner: vec![0],
            },
        ],
        vec![
            Doc(3),
            IfDef {
                live: true,
                inner: vec![0],
            },
            LineComment,
        ],
        vec![IfElse {
            dead: vec![3],
            live: vec![0],
        }],
        vec![Doc(0), Nowarn, Doc(3)],
        vec![TabDoc(0)],
        vec![Doc(0), Light, Doc(3)],
        vec![Doc(15)],
    ]
}

/// The whole cross product as one project: one file per (hazard, attribute)
/// pair, every template in each, every slot carrying the hazard.
/// One table file: its name, its text, and whether FCS must check it with no
/// error — every file but those whose hazard is itself an error (a tab).
struct TableFile {
    name: String,
    text: String,
    must_compile: bool,
}

fn table() -> Vec<TableFile> {
    let mut files = Vec::new();
    for (h, hazard) in hazards().into_iter().enumerate() {
        for attr in [false, true] {
            let module = files.len();
            let items: Vec<Item> = TEMPLATES
                .iter()
                .enumerate()
                .map(|(t, template)| Item {
                    prelude: hazard.clone(),
                    template: *template,
                    attr,
                    slots: vec![hazard.clone(); SLOTS],
                    // A doc trailing the previous item's last line, on every
                    // other item.
                    trailing: (t % 2 == 1 && !hazard.is_empty()).then_some(h % DOC_TEXTS.len()),
                })
                .collect();
            files.push(TableFile {
                name: format!("M{module}.fs"),
                text: render_file(module, &items),
                must_compile: !hazard.iter().any(|p| matches!(p, Piece::TabDoc(_))),
            });
        }
    }
    files
}

fn assert_no_failures(files: &[(&str, &str)], graded: &[Graded]) {
    let bad = failures(graded);
    if let Ok(dump) = std::env::var("BORZOI_XMLDOC_DIFF_DUMP") {
        let mut by_shape: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        let ambiguous = graded
            .iter()
            .filter(|g| matches!(g.verdict, Verdict::OracleAmbiguous));
        for g in bad.iter().copied().chain(ambiguous) {
            by_shape
                .entry(format!(
                    "{:?} {}",
                    g.def_kind,
                    if g.is_definition { "def" } else { "use" }
                ))
                .or_default()
                .push(describe(files, g));
        }
        let mut text = String::new();
        for (shape, items) in &by_shape {
            text.push_str(&format!("== {shape}: {}\n", items.len()));
            for i in items.iter().take(8) {
                text.push_str(i);
                text.push('\n');
            }
        }
        std::fs::write(&dump, text).unwrap();
        let all: String = files
            .iter()
            .map(|(n, t)| format!("// ==== {n}\n{t}\n"))
            .collect();
        std::fs::write(format!("{dump}.fs"), all).unwrap();
    }
    assert!(
        bad.is_empty(),
        "{} occurrence(s) where hover would show a doc FCS does not attach:\n{}",
        bad.len(),
        bad.iter()
            .take(30)
            .map(|g| describe(files, g))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn generated_table_agrees_with_fcs() {
    let owned = table();
    let files: Vec<(&str, &str)> = owned
        .iter()
        .map(|f| (f.name.as_str(), f.text.as_str()))
        .collect();
    let (graded, fcs) = run_fixture(&files, &["DEFINED_X"]);
    let census = census(&graded);
    eprintln!("source-doc table census: {census:#?}");
    // A file FCS rejects grades only divergences (its records are recovery),
    // so a template that fails to compile would quietly switch off the
    // clean-file gates for its whole file.
    for (file, f) in owned.iter().zip(&fcs) {
        let errors: Vec<_> = f
            .diagnostics
            .iter()
            .filter(|d| d.severity == "Error")
            .map(|d| format!("FS{:04} {}", d.error_number, d.message))
            .collect();
        assert!(
            !file.must_compile || errors.is_empty(),
            "table file {} must compile: {errors:?}\n{}",
            file.name,
            file.text
        );
    }
    assert_no_failures(&files, &graded);
    // Non-vacuity: the table must actually commit, for every declaration kind
    // that can carry a doc (static members only at their uses: a member's
    // defining occurrence is not recorded).
    let attached: std::collections::BTreeSet<String> = graded
        .iter()
        .filter(|g| matches!(g.verdict, Verdict::Agree { attached: true }))
        .map(|g| format!("{:?}", g.def_kind))
        .collect();
    eprintln!("kinds with an agreed attached doc: {attached:?}");
    for kind in [
        "Value { is_function: false }",
        "Value { is_function: true }",
        "ActivePattern",
        "Type",
        "UnionCase",
        "EnumCase",
        "ExceptionCase",
        "Member",
    ] {
        assert!(
            attached.contains(kind),
            "no agreed attached doc for {kind} — the table no longer exercises it"
        );
    }
}

fn piece() -> impl Strategy<Value = Piece> {
    let doc = 0..DOC_TEXTS.len();
    prop_oneof![
        4 => doc.clone().prop_map(Piece::Doc),
        1 => Just(Piece::Blank),
        1 => Just(Piece::LineComment),
        1 => Just(Piece::FourSlash),
        1 => Just(Piece::Block),
        1 => Just(Piece::BlockMultiline),
        1 => Just(Piece::Nowarn),
        1 => (0..DOC_TEXTS.len()).prop_map(Piece::TabDoc),
        1 => Just(Piece::Light),
        1 => (any::<bool>(), proptest::collection::vec(doc.clone(), 0..3))
            .prop_map(|(live, inner)| Piece::IfDef { live, inner }),
        1 => (
            proptest::collection::vec(doc.clone(), 0..2),
            proptest::collection::vec(doc, 0..2)
        )
            .prop_map(|(dead, live)| Piece::IfElse { dead, live }),
    ]
}

fn item() -> impl Strategy<Value = Item> {
    let prelude = || proptest::collection::vec(piece(), 0..4);
    (
        prelude(),
        proptest::sample::select(TEMPLATES),
        any::<bool>(),
        proptest::collection::vec(prelude(), SLOTS),
        proptest::option::weighted(0.2, 0..DOC_TEXTS.len()),
    )
        .prop_map(|(prelude, template, attr, slots, trailing)| Item {
            prelude,
            template,
            attr,
            slots,
            trailing,
        })
}

proptest! {
    /// Random compositions of declarations and trivia, one file each: hover
    /// never shows a doc FCS does not attach.
    #[test]
    fn random_files_agree_with_fcs(
        items in proptest::collection::vec(item(), 1..6),
        crlf in any::<bool>(),
    ) {
        let text = render_file(0, &items);
        let text = if crlf { text.replace('\n', "\r\n") } else { text };
        let files = [("M0.fs", text.as_str())];
        let (graded, _) = run_fixture(&files, &["DEFINED_X"]);
        let bad = failures(&graded);
        prop_assert!(
            bad.is_empty(),
            "divergence in\n{text}\n{}",
            bad.iter().map(|g| describe(&files, g)).collect::<Vec<_>>().join("\n")
        );
    }
}
