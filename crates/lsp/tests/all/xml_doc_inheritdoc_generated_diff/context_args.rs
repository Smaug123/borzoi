//! Base-type arguments built from the context's own type parameters.
//!
//! A `<typeparamref>` inherited from `B<X>` is rewritten to the ID of the type
//! `X` stands for, and Roslyn renders a type constructed from its own type
//! parameters as its definition. Whether a type's arguments are its own
//! depends on where the inheriting context sits: a type nested in the context
//! (or in the context's host) shares the context's parameters, one nested in
//! another context does not. This draws chains of contexts — top-level or
//! nested in a generic host, with one parameter or two — whose base types are
//! `B<…>` or an earlier context, instantiated with arguments built from the
//! context itself, types nested in it or beside it, other contexts, and
//! unrelated generics, some duplicating their argument (`Pair<T, T>`) so that
//! an instantiation grows with every hop.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use proptest::prelude::*;

use super::{
    DEEP_CASES, DEFAULT_CASES, Reach, assert_every_dimension_expands, assert_sound, cases,
    check_each, reach_by, sample,
};
use crate::common::inheritdoc_diff::{Compared, Verdict};
use crate::xml_doc_inheritdoc_diff::{compare, fixture};

/// A type argument, independent of where it is written: a context's
/// parameters are positional, so substituting a base's arguments is plain
/// replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Arg {
    Int,
    /// The `n`th type parameter (`T`, then `U`) of the context it is written
    /// in.
    Param(usize),
    /// Context `k`, its parameters set to the arguments.
    Ctx(usize, Vec<Arg>),
    /// The type `S` nested in context `k`, so instantiated.
    CtxS(usize, Vec<Arg>),
    /// The generic type `N<a>` nested in context `k`, so instantiated.
    CtxN(usize, Vec<Arg>, Box<Arg>),
    /// The type `S` nested in context `k`'s host, beside the context.
    HostS(usize, Box<Arg>),
    /// `G<a>`: a generic type unrelated to any context.
    G(Box<Arg>),
    /// `Pair<a, b>`.
    Pair(Box<Arg>, Box<Arg>),
    Array(Box<Arg>),
}

impl Arg {
    fn subst(&self, params: &[Arg]) -> Arg {
        let all = |args: &[Arg]| args.iter().map(|a| a.subst(params)).collect();
        match self {
            Arg::Int => Arg::Int,
            Arg::Param(n) => params[*n].clone(),
            Arg::Ctx(k, args) => Arg::Ctx(*k, all(args)),
            Arg::CtxS(k, args) => Arg::CtxS(*k, all(args)),
            Arg::CtxN(k, args, a) => Arg::CtxN(*k, all(args), Box::new(a.subst(params))),
            Arg::HostS(k, a) => Arg::HostS(*k, Box::new(a.subst(params))),
            Arg::G(a) => Arg::G(Box::new(a.subst(params))),
            Arg::Pair(a, b) => Arg::Pair(Box::new(a.subst(params)), Box::new(b.subst(params))),
            Arg::Array(a) => Arg::Array(Box::new(a.subst(params))),
        }
    }

    /// Nodes in the type: what an instantiation's size is counted in.
    fn size(&self) -> usize {
        1 + match self {
            Arg::Int | Arg::Param(_) => 0,
            Arg::Ctx(_, args) | Arg::CtxS(_, args) => args.iter().map(Arg::size).sum(),
            Arg::CtxN(_, args, a) => args.iter().map(Arg::size).sum::<usize>() + a.size(),
            Arg::HostS(_, a) | Arg::G(a) | Arg::Array(a) => a.size(),
            Arg::Pair(a, b) => a.size() + b.size(),
        }
    }

    /// C#, written in a context with `ctxs`' shapes.
    fn render(&self, ctxs: &[Shape]) -> String {
        match self {
            Arg::Int => "int".into(),
            Arg::Param(0) => "T".into(),
            Arg::Param(_) => "U".into(),
            Arg::Ctx(k, args) => ctxs[*k].path(*k, &list_each(args, ctxs)),
            Arg::CtxS(k, args) => format!("{}.S", ctxs[*k].path(*k, &list_each(args, ctxs))),
            Arg::CtxN(k, args, a) => format!(
                "{}.N<{}>",
                ctxs[*k].path(*k, &list_each(args, ctxs)),
                a.render(ctxs)
            ),
            Arg::HostS(k, a) => format!("H{k}<{}>.S", a.render(ctxs)),
            Arg::G(a) => format!("G<{}>", a.render(ctxs)),
            Arg::Pair(a, b) => format!("Pair<{}, {}>", a.render(ctxs), b.render(ctxs)),
            Arg::Array(a) => format!("{}[]", a.render(ctxs)),
        }
    }

    /// The dimensions of this type as seen from context `i`.
    fn dims(&self, i: usize, ctxs: &[Shape], out: &mut BTreeSet<Dim>) {
        let own = |k: usize, args: &[Arg]| k == i && args == ctxs[i].own_params().as_slice();
        let dim = match self {
            Arg::Int => return,
            Arg::Param(0) => Dim::ParamT,
            Arg::Param(_) => Dim::ParamU,
            Arg::Ctx(k, args) => {
                args.iter().for_each(|a| a.dims(i, ctxs, out));
                if own(*k, args) {
                    Dim::SelfTy
                } else if *k == i {
                    Dim::SelfOtherwise
                } else {
                    Dim::OtherContext
                }
            }
            Arg::CtxS(k, args) => {
                args.iter().for_each(|a| a.dims(i, ctxs, out));
                if own(*k, args) {
                    Dim::NestedInContext
                } else {
                    Dim::NestedElsewhere
                }
            }
            Arg::CtxN(k, args, a) => {
                args.iter().for_each(|a| a.dims(i, ctxs, out));
                a.dims(i, ctxs, out);
                if own(*k, args) {
                    Dim::GenericNestedInContext
                } else {
                    Dim::NestedElsewhere
                }
            }
            Arg::HostS(k, a) => {
                a.dims(i, ctxs, out);
                if *k == i && **a == Arg::Param(0) {
                    Dim::BesideContext
                } else {
                    Dim::NestedElsewhere
                }
            }
            Arg::G(a) => {
                a.dims(i, ctxs, out);
                Dim::Unrelated
            }
            Arg::Pair(a, b) => {
                a.dims(i, ctxs, out);
                b.dims(i, ctxs, out);
                Dim::Pair
            }
            Arg::Array(a) => {
                a.dims(i, ctxs, out);
                Dim::Array
            }
        };
        out.insert(dim);
    }
}

/// Each argument rendered, joined by commas.
fn list_each(args: &[Arg], ctxs: &[Shape]) -> String {
    args.iter()
        .map(|a| a.render(ctxs))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where a context sits and how many parameters it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shape {
    /// Nested in a generic host `H{k}<T>`, whose `T` is the context's first
    /// parameter.
    host: bool,
    /// A second parameter `U`, the context's own.
    u: bool,
}

impl Shape {
    fn arity(self) -> usize {
        1 + usize::from(self.u)
    }

    /// The context's own parameters, as written inside it.
    fn own_params(self) -> Vec<Arg> {
        (0..self.arity()).map(Arg::Param).collect()
    }

    /// Context `k`'s path with its parameters set to `args` (rendered).
    fn path(self, k: usize, args: &str) -> String {
        match (self.host, self.u) {
            (false, _) => format!("K{k}<{args}>"),
            (true, false) => format!("H{k}<{args}>.K{k}"),
            (true, true) => {
                let (t, u) = split_first_arg(args);
                format!("H{k}<{t}>.K{k}<{u}>")
            }
        }
    }
}

/// Split a rendered argument list at its first top-level comma.
fn split_first_arg(args: &str) -> (&str, &str) {
    let mut depth = 0usize;
    for (i, c) in args.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => return (&args[..i], args[i + 1..].trim_start()),
            _ => {}
        }
    }
    panic!("a two-parameter context's arguments without a comma: {args}")
}

/// A context's base type.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Base {
    /// `B<a>`.
    B(Arg),
    /// An earlier context, so instantiated.
    Ctx(usize, Vec<Arg>),
}

/// How a context or its method is documented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Doc {
    Bare,
    /// Its own summary, naming its first parameter.
    Full,
    None,
}

#[derive(Debug, Clone)]
struct Context {
    shape: Shape,
    base: Base,
    doc: Doc,
    m_doc: Doc,
}

#[derive(Debug, Clone)]
struct Program {
    contexts: Vec<Context>,
    /// Whether `B.M`'s documentation names `X` (its summary always does), so
    /// that an inherited method entry is sometimes free of any rewrite.
    m_names_x: bool,
}

/// A dimension of the vocabulary, as seen from the inheriting context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Dim {
    ParamT,
    ParamU,
    /// The context itself, from its own parameters.
    SelfTy,
    /// The context itself, from other arguments.
    SelfOtherwise,
    /// Another context.
    OtherContext,
    /// `S` nested in the context, from its own parameters.
    NestedInContext,
    /// `N<a>` nested in the context, from its own parameters.
    GenericNestedInContext,
    /// `S` nested in the context's host.
    BesideContext,
    /// A type nested in another context, or in this one from other
    /// arguments.
    NestedElsewhere,
    Unrelated,
    Pair,
    Array,
    /// The context is nested in a generic host.
    HostedContext,
    /// The context has a parameter of its own besides its host's.
    TwoParameters,
    /// The base is an earlier context.
    Chain,
    /// The instantiation reaching `X` has at least [`GROWN`] nodes.
    Grown,
}

const ALL_DIMS: [Dim; 16] = [
    Dim::ParamT,
    Dim::ParamU,
    Dim::SelfTy,
    Dim::SelfOtherwise,
    Dim::OtherContext,
    Dim::NestedInContext,
    Dim::GenericNestedInContext,
    Dim::BesideContext,
    Dim::NestedElsewhere,
    Dim::Unrelated,
    Dim::Pair,
    Dim::Array,
    Dim::HostedContext,
    Dim::TwoParameters,
    Dim::Chain,
    Dim::Grown,
];

/// An instantiation this large has grown along its chain.
const GROWN: usize = 24;

/// An instantiation larger than this is not written out: its C# would be
/// megabytes long. It is still far past the expansion's own bound.
const RENDER_LIMIT: usize = 1500;

fn shape() -> impl Strategy<Value = Shape> {
    (prop::bool::weighted(0.4), prop::bool::weighted(0.3)).prop_map(|(host, u)| Shape { host, u })
}

fn doc() -> impl Strategy<Value = Doc> {
    prop_oneof![4 => Just(Doc::Bare), 1 => Just(Doc::Full), 1 => Just(Doc::None)]
}

/// An argument written in context `i` of `shapes`.
fn arg(i: usize, shapes: Vec<Shape>) -> BoxedStrategy<Arg> {
    let me = shapes[i];
    let own = me.own_params();
    let mut leaves: Vec<(u32, BoxedStrategy<Arg>)> = vec![
        (1, Just(Arg::Int).boxed()),
        (4, Just(Arg::Param(0)).boxed()),
        (2, Just(Arg::Ctx(i, own.clone())).boxed()),
        (2, Just(Arg::CtxS(i, own.clone())).boxed()),
    ];
    if me.u {
        leaves.push((2, Just(Arg::Param(1)).boxed()));
    }
    if me.host {
        leaves.push((2, Just(Arg::HostS(i, Box::new(Arg::Param(0)))).boxed()));
    }
    let leaves = prop::strategy::Union::new_weighted(leaves).boxed();
    let n = shapes.len();
    leaves
        .clone()
        .prop_recursive(3, 12, 2, move |inner| {
            let shapes = shapes.clone();
            let own = own.clone();
            let instantiate = {
                let shapes = shapes.clone();
                let inner = inner.clone();
                move |k: usize| prop::collection::vec(inner.clone(), shapes[k].arity())
            };
            let other = (0..n).prop_filter("another context", move |k| *k != i || n == 1);
            let mut options: Vec<(u32, BoxedStrategy<Arg>)> = vec![
                (
                    2,
                    inner
                        .clone()
                        .prop_map({
                            let own = own.clone();
                            move |a| Arg::CtxN(i, own.clone(), Box::new(a))
                        })
                        .boxed(),
                ),
                (
                    2,
                    other
                        .clone()
                        .prop_flat_map({
                            let instantiate = instantiate.clone();
                            move |k| (Just(k), instantiate(k))
                        })
                        .prop_map(|(k, args)| Arg::CtxS(k, args))
                        .boxed(),
                ),
                (
                    1,
                    other
                        .prop_flat_map({
                            let instantiate = instantiate.clone();
                            move |k| (Just(k), instantiate(k))
                        })
                        .prop_map(|(k, args)| Arg::Ctx(k, args))
                        .boxed(),
                ),
                (
                    1,
                    instantiate(i)
                        .prop_map(move |args| Arg::Ctx(i, args))
                        .boxed(),
                ),
                (1, inner.clone().prop_map(|a| Arg::G(Box::new(a))).boxed()),
                (
                    3,
                    (inner.clone(), inner.clone())
                        .prop_map(|(a, b)| Arg::Pair(Box::new(a), Box::new(b)))
                        .boxed(),
                ),
                (
                    1,
                    inner.clone().prop_map(|a| Arg::Array(Box::new(a))).boxed(),
                ),
            ];
            let hosts: Vec<usize> = (0..n).filter(|&k| shapes[k].host).collect();
            if !hosts.is_empty() {
                options.push((
                    1,
                    (prop::sample::select(hosts), inner)
                        .prop_map(|(k, a)| Arg::HostS(k, Box::new(a)))
                        .boxed(),
                ));
            }
            prop::strategy::Union::new_weighted(options)
        })
        .boxed()
}

/// Context `i`'s base: `B<…>`, or (mostly) an earlier context, its arguments
/// often duplicating the context's parameter so that the chain grows.
fn base(i: usize, shapes: Vec<Shape>) -> BoxedStrategy<Base> {
    let b = arg(i, shapes.clone()).prop_map(Base::B).boxed();
    if i == 0 {
        return b;
    }
    let grow = Just(Arg::Pair(Box::new(Arg::Param(0)), Box::new(Arg::Param(0))));
    let one = prop_oneof![1 => arg(i, shapes.clone()), 1 => grow].boxed();
    let chain = (0..i)
        .prop_flat_map(move |j| {
            (
                Just(j),
                prop::collection::vec(one.clone(), shapes[j].arity()),
            )
        })
        .prop_map(|(j, args)| Base::Ctx(j, args));
    prop_oneof![1 => b, 3 => chain].boxed()
}

fn program() -> impl Strategy<Value = Program> {
    prop::collection::vec(shape(), 1..12)
        .prop_flat_map(|shapes| {
            let contexts: Vec<_> = (0..shapes.len())
                .map(|i| {
                    let shape = shapes[i];
                    (base(i, shapes.clone()), doc(), doc()).prop_map(move |(base, doc, m_doc)| {
                        Context {
                            shape,
                            base,
                            doc,
                            m_doc,
                        }
                    })
                })
                .collect();
            (contexts, prop::bool::weighted(0.6))
        })
        .prop_map(|(contexts, m_names_x)| Program {
            contexts,
            m_names_x,
        })
}

impl Program {
    fn shapes(&self) -> Vec<Shape> {
        self.contexts.iter().map(|c| c.shape).collect()
    }

    /// What `X` stands for, seen from each context in turn: the type its
    /// `M`'s parameter takes.
    fn instantiations(&self) -> Vec<Arg> {
        let mut out: Vec<Arg> = Vec::new();
        for c in &self.contexts {
            out.push(match &c.base {
                Base::B(a) => a.clone(),
                Base::Ctx(j, args) => out[*j].subst(args),
            });
        }
        out
    }

    /// Contexts whose instantiation is too large to write out take `B<T>`.
    fn bounded(mut self) -> Program {
        let mut out: Vec<Arg> = Vec::new();
        for c in &mut self.contexts {
            let x = match &c.base {
                Base::B(a) => a.clone(),
                Base::Ctx(j, args) => out[*j].subst(args),
            };
            let x = if x.size() > RENDER_LIMIT {
                c.base = Base::B(Arg::Param(0));
                Arg::Param(0)
            } else {
                x
            };
            out.push(x);
        }
        self
    }

    /// The contexts along `i`'s chain of bases, `i` first.
    fn chain(&self, i: usize) -> Vec<usize> {
        let mut out = vec![i];
        while let Base::Ctx(j, _) = &self.contexts[*out.last().unwrap()].base {
            out.push(*j);
        }
        out
    }
}

fn render(p: &Program) -> String {
    let shapes = p.shapes();
    let x = p.instantiations();
    let mut out = String::from(
        "namespace F;\n\n\
         /// <summary>B of <typeparamref name=\"X\"/>.</summary>\n\
         public class B<X>\n{\n",
    );
    if p.m_names_x {
        out.push_str(
            "    /// <summary>B.M of <typeparamref name=\"X\"/>.</summary>\n    \
             /// <param name=\"x\">The x of <typeparamref name=\"X\"/>.</param>\n",
        );
    } else {
        out.push_str(
            "    /// <summary>B.M's own.</summary>\n    \
             /// <param name=\"x\">The x.</param>\n",
        );
    }
    out.push_str(
        "    public virtual void M(X x) { }\n}\n\n\
         /// <summary>G of <typeparamref name=\"A\"/>.</summary>\n\
         public class G<A> { }\n\n\
         /// <summary>A pair.</summary>\n\
         public class Pair<A, C> { }\n\n",
    );
    let doc = |out: &mut String, d: Doc, indent: &str, what: &str| match d {
        Doc::Bare => writeln!(out, "{indent}/// <inheritdoc/>").unwrap(),
        Doc::Full => writeln!(
            out,
            "{indent}/// <summary>{what} of <typeparamref name=\"T\"/>.</summary>"
        )
        .unwrap(),
        Doc::None => {}
    };
    for (i, c) in p.contexts.iter().enumerate() {
        let base = match &c.base {
            Base::B(a) => format!("B<{}>", a.render(&shapes)),
            Base::Ctx(j, args) => shapes[*j].path(*j, &list_each(args, &shapes)),
        };
        let indent = if c.shape.host { "    " } else { "" };
        if c.shape.host {
            writeln!(
                out,
                "/// <summary>H{i}.</summary>\npublic class H{i}<T>\n{{\n    \
                 /// <summary>H{i}.S.</summary>\n    public class S {{ }}\n"
            )
            .unwrap();
        }
        doc(&mut out, c.doc, indent, &format!("K{i}"));
        let tp = match (c.shape.host, c.shape.u) {
            (false, false) => "<T>",
            (false, true) => "<T, U>",
            (true, false) => "",
            (true, true) => "<U>",
        };
        writeln!(out, "{indent}public class K{i}{tp} : {base}\n{indent}{{").unwrap();
        writeln!(
            out,
            "{indent}    /// <summary>K{i}.S.</summary>\n{indent}    public class S {{ }}\n\n\
             {indent}    /// <summary>K{i}.N.</summary>\n{indent}    public class N<V> {{ }}\n"
        )
        .unwrap();
        let m_doc = match c.m_doc {
            Doc::None => Doc::Bare,
            d => d,
        };
        doc(
            &mut out,
            m_doc,
            &format!("{indent}    "),
            &format!("K{i}.M"),
        );
        writeln!(
            out,
            "{indent}    public override void M({} x) {{ }}\n{indent}}}",
            x[i].render(&shapes)
        )
        .unwrap();
        if c.shape.host {
            out.push_str("}\n");
        }
        out.push('\n');
    }
    out
}

/// The context a documentation ID's declaring type or member belongs to.
fn context_of(key: &str) -> Option<usize> {
    let head = key.get(2..)?.split('(').next()?;
    head.split('.')
        .filter_map(|seg| seg.strip_prefix('K'))
        .filter_map(|rest| rest.split('`').next()?.parse().ok())
        .next_back()
}

/// The dimensions of context `i`'s entries.
fn dims_of(p: &Program, x: &[Arg], i: usize) -> BTreeSet<Dim> {
    let shapes = p.shapes();
    let mut out = BTreeSet::new();
    x[i].dims(i, &shapes, &mut out);
    for &k in &p.chain(i) {
        if shapes[k].host {
            out.insert(Dim::HostedContext);
        }
        if shapes[k].u {
            out.insert(Dim::TwoParameters);
        }
    }
    if matches!(p.contexts[i].base, Base::Ctx(..)) {
        out.insert(Dim::Chain);
    }
    if x[i].size() >= GROWN {
        out.insert(Dim::Grown);
    }
    out
}

fn run(p: &Program) -> (Vec<Compared>, BTreeMap<usize, BTreeSet<Dim>>) {
    let source = render(p);
    let fx = fixture(&source)
        .unwrap_or_else(|e| panic!("the generated program does not compile: {e:#?}\n{source}"));
    let compared = compare(&fx);
    assert_sound(&compared, &source);
    let x = p.instantiations();
    let dims = (0..p.contexts.len())
        .map(|i| (i, dims_of(p, &x, i)))
        .collect();
    (compared, dims)
}

fn strategy() -> impl Strategy<Value = Program> {
    program().prop_map(Program::bounded)
}

#[test]
fn generated_context_arguments_expand_exactly_as_roslyn() {
    check_each(strategy(), cases(DEFAULT_CASES), |p| {
        run(&p);
    });
}

#[test]
#[ignore = "deep run: CI's <inheritdoc> sweep step"]
fn deep_generated_context_arguments_expand_exactly_as_roslyn() {
    check_each(strategy(), cases(DEEP_CASES), |p| {
        run(&p);
    });
}

/// Every dimension reaches an expanded entry, and the instantiation bound is
/// reached too: some entry declines as too large.
#[test]
fn the_context_argument_generator_reaches_every_dimension() {
    let mut reach: BTreeMap<Dim, Reach> = BTreeMap::new();
    let mut too_large = 0;
    let mut largest = 0;
    for p in sample(&strategy(), 32) {
        let (compared, dims) = run(&p);
        largest = p
            .instantiations()
            .iter()
            .map(Arg::size)
            .fold(largest, usize::max);
        for c in &compared {
            if matches!(&c.verdict, Verdict::Declined { cause, .. } if cause == "Undecidable(InstantiationTooLarge)")
            {
                too_large += 1;
            }
            if let Some(ds) = context_of(&c.key).and_then(|i| dims.get(&i)) {
                reach_by(&mut reach, ds.iter().copied(), &c.verdict);
            }
        }
    }
    assert_every_dimension_expands("context arguments", &ALL_DIMS, &reach);
    assert!(
        too_large > 0,
        "no sampled instantiation reached the bound; the largest had {largest} nodes"
    );
}
