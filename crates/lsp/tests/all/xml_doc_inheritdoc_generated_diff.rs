//! `<inheritdoc>` expansion against Roslyn's, over *generated* C# programs.
//!
//! The handwritten fixtures pin the rules one at a time; this draws class and
//! interface hierarchies — generic and not, with overrides, `new` members,
//! implicit and explicit interface implementations, re-implemented and
//! inherited interfaces, constructors — and documents every symbol with one
//! of the shapes shipped documentation uses (full entries, bare
//! `<inheritdoc/>`, own text plus `<inheritdoc/>`, `<inheritdoc/>` inside a
//! summary, a `path`, a `cref`). Each program is compiled by the oracle and
//! every inheritdoc-bearing entry compared, certain-implies-exact, against
//! Roslyn's expansion. A liveness check keeps the property from passing on
//! programs that do not compile or entries that all decline.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use proptest::prelude::*;

use crate::common::inheritdoc_diff::Census;
use crate::xml_doc_inheritdoc_diff::{compare, fixture};

/// A type in a member signature, in the context of the declaring type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Int,
    Str,
    /// The declaring type's own type parameter `T`.
    T,
}

impl Ty {
    fn render(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Str => "string",
            Ty::T => "T",
        }
    }

    /// This type, written in a type whose `T` is `arg`, seen from where
    /// `arg` is written.
    fn subst(self, arg: Ty) -> Ty {
        match self {
            Ty::T => arg,
            other => other,
        }
    }
}

/// How a symbol is documented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Doc {
    None,
    Full,
    Bare,
    OwnPlusBare,
    InSummary,
    PathRemarks,
    PathParam,
    /// `<inheritdoc cref="…"/>` naming the class of this index (mod the
    /// number of classes).
    CrefClass(usize),
}

fn doc() -> impl Strategy<Value = Doc> {
    prop_oneof![
        2 => Just(Doc::Full),
        3 => Just(Doc::Bare),
        1 => Just(Doc::None),
        1 => Just(Doc::OwnPlusBare),
        1 => Just(Doc::InSummary),
        1 => Just(Doc::PathRemarks),
        1 => Just(Doc::PathParam),
        1 => (0..3usize).prop_map(Doc::CrefClass),
    ]
}

/// How an interface method is declared: abstract, or with a default
/// implementation that a class member can implement (virtual) or cannot
/// (sealed, private) — the last two leave a same-named public class member
/// unrelated to the interface's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
    Abstract,
    Virtual,
    Sealed,
    Private,
}

fn body() -> impl Strategy<Value = Body> {
    prop_oneof![
        4 => Just(Body::Abstract),
        1 => Just(Body::Virtual),
        1 => Just(Body::Sealed),
        1 => Just(Body::Private),
    ]
}

/// Docs for interface members — the roots most inheritance ends at — so
/// mostly written out in full.
fn iface_doc() -> impl Strategy<Value = Doc> {
    prop_oneof![
        5 => Just(Doc::Full),
        1 => Just(Doc::None),
        1 => Just(Doc::Bare),
        1 => Just(Doc::InSummary),
    ]
}

fn ty(generic: bool) -> impl Strategy<Value = Ty> {
    if generic {
        prop_oneof![Just(Ty::Int), Just(Ty::Str), Just(Ty::T)].boxed()
    } else {
        prop_oneof![Just(Ty::Int), Just(Ty::Str)].boxed()
    }
}

#[derive(Debug, Clone)]
struct IfaceSpec {
    generic: bool,
    /// A base interface (an earlier one) and its type argument.
    base: Option<(usize, Ty)>,
    /// Methods `N{i}`: return and parameter types, their docs, and whether
    /// (and how) they carry a default implementation.
    methods: Vec<(Ty, Ty, Doc, Body)>,
    property: Option<(Ty, Doc)>,
    doc: Doc,
}

#[derive(Debug, Clone)]
struct ClassSpec {
    generic: bool,
    /// A base class (an earlier one) and its type argument.
    base: Option<(usize, Ty)>,
    /// Interfaces this class lists, each with its argument and whether its
    /// members are implemented explicitly.
    interfaces: Vec<(usize, Ty, bool)>,
    /// Per inherited virtual member, by position: skip, override, or `new`.
    inherited: Vec<(u8, Doc)>,
    /// New virtual methods `M{class}_{i}`.
    /// New virtual methods: `M{class}_{i}`, or — when the index names an
    /// inherited method and the parameter type differs from every inherited
    /// one of that name — an overload of it.
    own: Vec<(Ty, Ty, Doc, Option<usize>)>,
    /// Docs for interface implementations, by position (cycled).
    impl_docs: Vec<Doc>,
    ctor_doc: Doc,
    doc: Doc,
}

#[derive(Debug, Clone)]
struct Program {
    interfaces: Vec<IfaceSpec>,
    classes: Vec<ClassSpec>,
}

fn program() -> impl Strategy<Value = Program> {
    let iface = |i: usize| {
        any::<bool>().prop_flat_map(move |generic| {
            (
                Just(generic),
                if i == 0 {
                    Just(None).boxed()
                } else {
                    prop::option::of((0..i, ty(generic))).boxed()
                },
                prop::collection::vec((ty(generic), ty(generic), iface_doc(), body()), 1..3),
                prop::option::of((ty(generic), iface_doc())),
                doc(),
            )
                .prop_map(|(generic, base, methods, property, doc)| IfaceSpec {
                    generic,
                    base,
                    methods,
                    property,
                    doc,
                })
        })
    };
    let class = |k: usize| {
        any::<bool>().prop_flat_map(move |generic| {
            (
                Just(generic),
                if k == 0 {
                    Just(None).boxed()
                } else {
                    prop::option::of((0..k, ty(generic))).boxed()
                },
                prop::collection::vec((0..2usize, ty(generic), any::<bool>()), 0..3),
                prop::collection::vec((0..5u8, doc()), 0..6),
                prop::collection::vec(
                    (ty(generic), ty(generic), doc(), prop::option::of(0..4usize)),
                    0..3,
                ),
                prop::collection::vec(doc(), 1..6),
                doc(),
                doc(),
            )
                .prop_map(
                    |(generic, base, interfaces, inherited, own, impl_docs, ctor_doc, doc)| {
                        ClassSpec {
                            generic,
                            base,
                            interfaces,
                            inherited,
                            own,
                            impl_docs,
                            ctor_doc,
                            doc,
                        }
                    },
                )
        })
    };
    (
        iface(0),
        iface(1),
        class(0),
        class(1),
        class(2),
        prop::option::of(Just(())),
    )
        .prop_map(|(i0, i1, c0, c1, c2, _)| Program {
            interfaces: vec![i0, i1],
            classes: vec![c0, c1, c2],
        })
}

/// The documentation comment for one symbol. `id` is a unique token for its
/// text; `generic` whether the declaring type has a `T` to refer to;
/// `params`/`returns` whether to document those.
fn render_doc(
    out: &mut String,
    d: Doc,
    id: &str,
    generic: bool,
    params: bool,
    returns: bool,
    classes: &[String],
) {
    let tp = if generic {
        " of <typeparamref name=\"T\"/>"
    } else {
        ""
    };
    let full = |out: &mut String| {
        writeln!(out, "/// <summary>Summary of {id}{tp}.</summary>").unwrap();
        if params {
            writeln!(out, "/// <param name=\"x\">X of {id}{tp}.</param>").unwrap();
        }
        if returns {
            writeln!(out, "/// <returns>Returns of {id}.</returns>").unwrap();
        }
        writeln!(out, "/// <remarks>Remarks of {id}.</remarks>").unwrap();
    };
    match d {
        Doc::None => {}
        Doc::Full => full(out),
        Doc::Bare => writeln!(out, "/// <inheritdoc/>").unwrap(),
        Doc::OwnPlusBare => {
            writeln!(out, "/// <summary>Own summary of {id}.</summary>").unwrap();
            writeln!(out, "/// <inheritdoc/>").unwrap();
        }
        Doc::InSummary => writeln!(
            out,
            "/// <summary>Own words of {id}: <inheritdoc/></summary>"
        )
        .unwrap(),
        Doc::PathRemarks => writeln!(out, "/// <inheritdoc path=\"/remarks\"/>").unwrap(),
        Doc::PathParam => {
            writeln!(out, "/// <inheritdoc path=\"/param[@name='x']/node()\"/>").unwrap()
        }
        Doc::CrefClass(k) => writeln!(
            out,
            "/// <inheritdoc cref=\"{}\"/>",
            classes[k % classes.len()]
        )
        .unwrap(),
    }
}

/// A member signature as seen from a class: name, return type, parameter
/// type (`None` for a property).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sig {
    name: String,
    ret: Ty,
    param: Option<Ty>,
}

/// One interface instance in a class's closure: the interface and its
/// argument in the class's context.
fn closure(p: &Program, listed: &[(usize, Ty)]) -> Vec<(usize, Ty)> {
    let mut out: Vec<(usize, Ty)> = Vec::new();
    for &(i, arg) in listed {
        let mut cur = Some((i, arg));
        while let Some((i, arg)) = cur {
            out.push((i, arg));
            cur = p.interfaces[i].base.map(|(b, barg)| {
                (
                    b,
                    if p.interfaces[i].generic {
                        barg.subst(arg)
                    } else {
                        barg
                    },
                )
            });
        }
    }
    out
}

fn iface_name(i: usize, generic: bool, arg: Ty) -> String {
    if generic {
        format!("I{i}<{}>", arg.render())
    } else {
        format!("I{i}")
    }
}

/// Render the program to C#.
fn render(p: &Program) -> String {
    let mut out = String::from("namespace Gen;\n\n");
    let class_crefs: Vec<String> = p
        .classes
        .iter()
        .enumerate()
        .map(|(k, c)| {
            if c.generic {
                format!("C{k}{{T}}")
            } else {
                format!("C{k}")
            }
        })
        .collect();
    for (i, spec) in p.interfaces.iter().enumerate() {
        let id = format!("I{i}");
        render_doc(
            &mut out,
            spec.doc,
            &id,
            spec.generic,
            false,
            false,
            &class_crefs,
        );
        let tp = if spec.generic { "<T>" } else { "" };
        let base = spec
            .base
            .map(|(b, arg)| format!(" : {}", iface_name(b, p.interfaces[b].generic, arg)))
            .unwrap_or_default();
        writeln!(out, "public interface I{i}{tp}{base}\n{{").unwrap();
        for (m, (ret, param, d, body)) in spec.methods.iter().enumerate() {
            render_doc(
                &mut out,
                *d,
                &format!("I{i}.N{m}"),
                spec.generic,
                true,
                true,
                &class_crefs,
            );
            let (ret, param) = (ret.render(), param.render());
            match body {
                Body::Abstract => writeln!(out, "    {ret} N{m}({param} x);"),
                Body::Virtual => writeln!(out, "    {ret} N{m}({param} x) => default!;"),
                Body::Sealed => {
                    writeln!(out, "    public sealed {ret} N{m}({param} x) => default!;")
                }
                Body::Private => writeln!(out, "    private {ret} N{m}({param} x) => default!;"),
            }
            .unwrap();
        }
        if let Some((t, d)) = spec.property {
            render_doc(
                &mut out,
                d,
                &format!("I{i}.Q"),
                spec.generic,
                false,
                false,
                &class_crefs,
            );
            writeln!(out, "    {} Q {{ get; }}", t.render()).unwrap();
        }
        out.push_str("}\n\n");
    }
    // Each class's virtual methods as seen from it, for its derived classes.
    let mut virtuals: Vec<Vec<Sig>> = Vec::new();
    for (k, c) in p.classes.iter().enumerate() {
        let id = format!("C{k}");
        render_doc(&mut out, c.doc, &id, c.generic, false, false, &class_crefs);
        let tp = if c.generic { "<T>" } else { "" };
        let mut supers: Vec<String> = Vec::new();
        let inherited: Vec<Sig> = match c.base {
            Some((b, arg)) => {
                let base = &p.classes[b];
                supers.push(if base.generic {
                    format!("C{b}<{}>", arg.render())
                } else {
                    format!("C{b}")
                });
                virtuals[b]
                    .iter()
                    .map(|s| {
                        let sub = |t: Ty| if base.generic { t.subst(arg) } else { t };
                        Sig {
                            name: s.name.clone(),
                            ret: sub(s.ret),
                            param: s.param.map(sub),
                        }
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        // Interfaces: each definition at most once in the closure (two
        // instances of one generic interface may unify, which C# rejects).
        let mut listed: Vec<(usize, Ty, bool)> = Vec::new();
        let mut defs: Vec<usize> = Vec::new();
        for &(i, arg, explicit) in &c.interfaces {
            let arg = if p.interfaces[i].generic {
                arg
            } else {
                Ty::Int
            };
            let more = closure(p, &[(i, arg)]);
            if more.iter().any(|(d, _)| defs.contains(d)) {
                continue;
            }
            defs.extend(more.iter().map(|(d, _)| *d));
            listed.push((i, arg, explicit));
            supers.push(iface_name(i, p.interfaces[i].generic, arg));
        }
        let supers = if supers.is_empty() {
            String::new()
        } else {
            format!(" : {}", supers.join(", "))
        };
        writeln!(out, "public class C{k}{tp}{supers}\n{{").unwrap();
        render_doc(
            &mut out,
            c.ctor_doc,
            &format!("C{k}.ctor"),
            c.generic,
            true,
            false,
            &class_crefs,
        );
        let chain = if c.base.is_some() { " : base(x)" } else { "" };
        writeln!(out, "    public C{k}(int x){chain} {{ }}").unwrap();

        let mut mine: Vec<Sig> = Vec::new();
        for (j, s) in inherited.iter().enumerate() {
            let (choice, d) = c.inherited.get(j).copied().unwrap_or((0, Doc::None));
            let param = s.param.expect("virtual methods take x");
            match choice {
                1 | 3 | 4 => {
                    render_doc(
                        &mut out,
                        d,
                        &format!("C{k}.{}", s.name),
                        c.generic,
                        true,
                        true,
                        &class_crefs,
                    );
                    writeln!(
                        out,
                        "    public override {} {}({} x) => default!;",
                        s.ret.render(),
                        s.name,
                        param.render()
                    )
                    .unwrap();
                    mine.push(s.clone());
                }
                2 => {
                    render_doc(
                        &mut out,
                        d,
                        &format!("C{k}.{}", s.name),
                        c.generic,
                        true,
                        true,
                        &class_crefs,
                    );
                    writeln!(
                        out,
                        "    public new virtual {} {}({} x) => default!;",
                        s.ret.render(),
                        s.name,
                        param.render()
                    )
                    .unwrap();
                    mine.push(s.clone());
                }
                _ => mine.push(s.clone()),
            }
        }
        for (j, (ret, param, d, overload)) in c.own.iter().enumerate() {
            // An overload of an inherited name, when no method of the name
            // already takes this parameter — and neither parameter is `T`,
            // which a derived instantiation could make collide.
            let overloaded = overload
                .and_then(|o| inherited.get(o % inherited.len().max(1)))
                .filter(|s| {
                    *param != Ty::T
                        && mine
                            .iter()
                            .filter(|m| m.name == s.name)
                            .all(|m| m.param != Some(*param) && m.param != Some(Ty::T))
                });
            let name = match overloaded {
                Some(s) => s.name.clone(),
                None => format!("M{k}_{j}"),
            };
            render_doc(
                &mut out,
                *d,
                &format!("C{k}.{name}"),
                c.generic,
                true,
                true,
                &class_crefs,
            );
            writeln!(
                out,
                "    public virtual {} {name}({} x) => default!;",
                ret.render(),
                param.render()
            )
            .unwrap();
            mine.push(Sig {
                name,
                ret: *ret,
                param: Some(*param),
            });
        }
        // Interface members: implicit ones are shared by every instance that
        // needs the same name and parameter; one with the same parameter but
        // another return type must be explicit.
        let mut implicit: BTreeMap<(String, Option<&'static str>), &'static str> = BTreeMap::new();
        let mut doc_cursor = 0;
        let mut next_doc = || {
            let d = c.impl_docs[doc_cursor % c.impl_docs.len()];
            doc_cursor += 1;
            d
        };
        for &(i, arg, explicit) in &listed {
            for (d, darg) in closure(p, &[(i, arg)]) {
                let spec = &p.interfaces[d];
                let sub = |t: Ty| if spec.generic { t.subst(darg) } else { t };
                let qualifier = iface_name(d, spec.generic, darg);
                for (m, (ret, param, _, body)) in spec.methods.iter().enumerate() {
                    let (ret, param) = (sub(*ret), sub(*param));
                    let name = format!("N{m}");
                    let key = (name.clone(), Some(param.render()));
                    let clash = implicit.get(&key).is_some_and(|r| *r != ret.render());
                    // A sealed or private default cannot be implemented, so
                    // not explicitly either: a same-named public member is
                    // declared (unrelated to it) unless that would clash.
                    let unimplementable = matches!(body, Body::Sealed | Body::Private);
                    if unimplementable && clash {
                        continue;
                    }
                    let explicit = !unimplementable && (explicit || clash);
                    let doc = next_doc();
                    if explicit {
                        render_doc(
                            &mut out,
                            doc,
                            &format!("C{k}.{qualifier}.{name}"),
                            c.generic,
                            true,
                            true,
                            &class_crefs,
                        );
                        writeln!(
                            out,
                            "    {} {qualifier}.{name}({} x) => default!;",
                            ret.render(),
                            param.render()
                        )
                        .unwrap();
                    } else if let std::collections::btree_map::Entry::Vacant(e) =
                        implicit.entry(key)
                    {
                        e.insert(ret.render());
                        render_doc(
                            &mut out,
                            doc,
                            &format!("C{k}.{name}"),
                            c.generic,
                            true,
                            true,
                            &class_crefs,
                        );
                        writeln!(
                            out,
                            "    public {} {name}({} x) => default!;",
                            ret.render(),
                            param.render()
                        )
                        .unwrap();
                    }
                }
                if let Some((t, _)) = spec.property {
                    let t = sub(t);
                    let key = ("Q".to_string(), None);
                    let explicit = explicit || implicit.get(&key).is_some_and(|r| *r != t.render());
                    let doc = next_doc();
                    if explicit {
                        render_doc(
                            &mut out,
                            doc,
                            &format!("C{k}.{qualifier}.Q"),
                            c.generic,
                            false,
                            false,
                            &class_crefs,
                        );
                        writeln!(out, "    {} {qualifier}.Q => default!;", t.render()).unwrap();
                    } else if let std::collections::btree_map::Entry::Vacant(e) =
                        implicit.entry(key)
                    {
                        e.insert(t.render());
                        render_doc(
                            &mut out,
                            doc,
                            &format!("C{k}.Q"),
                            c.generic,
                            false,
                            false,
                            &class_crefs,
                        );
                        writeln!(out, "    public {} Q => default!;", t.render()).unwrap();
                    }
                }
            }
        }
        out.push_str("}\n\n");
        virtuals.push(mine);
    }
    out
}

/// One generated program's census, or the compiler's errors.
fn run(p: &Program) -> Result<Census, Vec<String>> {
    let source = render(p);
    let fx = fixture(&source)?;
    let mut census = Census::default();
    census.add(&compare(&fx));
    if let Some((key, ours, roslyn)) = census.disagrees.first() {
        panic!("{key} disagrees with Roslyn:\nours: {ours}\nroslyn: {roslyn}\nprogram:\n{source}");
    }
    Ok(census)
}

proptest! {
    #[test]
    fn generated_hierarchies_expand_exactly_as_roslyn(p in program()) {
        // `run` panics on a disagreement. A program the generator got wrong
        // (`Err`, the compiler's errors) makes no claim; liveness below keeps
        // that rare.
        let _ = run(&p);
    }
}

/// The generator mostly writes valid C#, and the comparison mostly expands:
/// without this the property could pass on programs that do not compile or
/// entries that all decline.
#[test]
fn the_generator_compiles_and_expands() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strategy = program();
    let (mut compiled, mut total) = (0, 0);
    let mut census = Census::default();
    let mut errors: Vec<String> = Vec::new();
    for _ in 0..20 {
        let p = strategy.new_tree(&mut runner).unwrap().current();
        total += 1;
        match run(&p) {
            Ok(c) => {
                compiled += 1;
                census.merge(c);
            }
            Err(e) => errors.extend(e.into_iter().take(2)),
        }
    }
    census.print("generated (liveness sample)");
    assert!(
        compiled * 10 >= total * 9,
        "only {compiled}/{total} generated programs compile; e.g. {errors:#?}"
    );
    let expanded = census.agrees;
    let declined = census.declined_total();
    assert!(
        expanded >= 80,
        "too few expansions: {expanded} expanded, {declined} declined"
    );
}
