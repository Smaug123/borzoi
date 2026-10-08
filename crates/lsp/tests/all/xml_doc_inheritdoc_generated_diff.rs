//! `<inheritdoc>` expansion against Roslyn's, over *generated* C# programs.
//!
//! The handwritten fixtures pin the rules one at a time; this draws class and
//! interface hierarchies — generic and not, with overrides, `new` members,
//! implicit and explicit interface implementations, re-implemented and
//! inherited interfaces, static interface members, generic methods (some
//! shadowing the type's `T`), constructors — over a type vocabulary that
//! includes every distinction metadata carries only in an attribute
//! (`dynamic`, tuple element names, `nint`), arrays and nested generic types,
//! and documents every symbol with one of the shapes shipped documentation
//! uses (full entries, bare `<inheritdoc/>`, own text plus `<inheritdoc/>`,
//! `<inheritdoc/>` inside a summary, a `path`, a `cref`). Each program is
//! compiled by the oracle and every inheritdoc-bearing entry compared,
//! certain-implies-exact, against Roslyn's expansion.
//!
//! Every generated program must compile: a generator that writes invalid C#
//! fails the property, so no dimension can drop out unnoticed. A liveness
//! check over a fixed sample asserts that each dimension of the vocabulary
//! reaches a compared entry, and that the comparison mostly expands.
//!
//! The properties run [`DEFAULT_CASES`] cases in the normal suite; the
//! `#[ignore]`d `deep_*` twins run [`DEEP_CASES`] (CI's `<inheritdoc>` sweep
//! step runs them). `BORZOI_INHERITDOC_CASES` overrides both.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};

use crate::common::inheritdoc_diff::{Census, Compared, Verdict};
use crate::xml_doc_inheritdoc_diff::{
    DYNAMIC_ATTRIBUTE, compare, fixture, fixture_without_core_library,
};

/// Cases per property in the normal suite: each case is a compilation and an
/// expansion on the one oracle child, which every case group shares.
const DEFAULT_CASES: u32 = 48;

/// Cases per property in the deep run.
const DEEP_CASES: u32 = 384;

fn cases(default: u32) -> u32 {
    std::env::var("BORZOI_INHERITDOC_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A type in a member signature, in the context of the declaring type (whose
/// type parameter is `T`) and method (whose type parameter, if any, is `M`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Ty {
    Int,
    Str,
    Object,
    /// `object` in metadata, plus `DynamicAttribute`.
    Dynamic,
    /// `System.IntPtr` in metadata (plus `NativeIntegerAttribute` where the
    /// runtime lacks numeric `IntPtr`).
    NInt,
    IntPtr,
    /// The declaring type's own type parameter `T`.
    T,
    /// The method's own type parameter.
    M,
    Array(Box<Ty>),
    /// `(a, b)`; with element names `a` and `b` when `named`, which metadata
    /// carries only in `TupleElementNamesAttribute`.
    Tuple(Box<Ty>, Box<Ty>, bool),
    /// `Outer<a>.Inner<b>`: a generic type nested in a generic type.
    Nested(Box<Ty>, Box<Ty>),
}

impl Ty {
    /// C# for this type, the method's type parameter written `m`.
    fn render(&self, m: &str) -> String {
        match self {
            Ty::Int => "int".into(),
            Ty::Str => "string".into(),
            Ty::Object => "object".into(),
            Ty::Dynamic => "dynamic".into(),
            Ty::NInt => "nint".into(),
            Ty::IntPtr => "System.IntPtr".into(),
            Ty::T => "T".into(),
            Ty::M => {
                assert!(!m.is_empty(), "a method type parameter outside a method");
                m.into()
            }
            Ty::Array(e) => format!("{}[]", e.render(m)),
            Ty::Tuple(a, b, true) => format!("({} a, {} b)", a.render(m), b.render(m)),
            Ty::Tuple(a, b, false) => format!("({}, {})", a.render(m), b.render(m)),
            Ty::Nested(a, b) => format!("Outer<{}>.Inner<{}>", a.render(m), b.render(m)),
        }
    }

    /// Rebuild bottom-up, replacing each node `f` maps.
    fn map(&self, f: &impl Fn(&Ty) -> Option<Ty>) -> Ty {
        if let Some(t) = f(self) {
            return t;
        }
        match self {
            Ty::Array(e) => Ty::Array(Box::new(e.map(f))),
            Ty::Tuple(a, b, n) => Ty::Tuple(Box::new(a.map(f)), Box::new(b.map(f)), *n),
            Ty::Nested(a, b) => Ty::Nested(Box::new(a.map(f)), Box::new(b.map(f))),
            leaf => leaf.clone(),
        }
    }

    fn any(&self, p: &impl Fn(&Ty) -> bool) -> bool {
        p(self)
            || match self {
                Ty::Array(e) => e.any(p),
                Ty::Tuple(a, b, _) | Ty::Nested(a, b) => a.any(p) || b.any(p),
                _ => false,
            }
    }

    /// This type, written in a type whose `T` is `arg`, seen from where
    /// `arg` is written.
    fn subst(&self, arg: &Ty) -> Ty {
        self.map(&|t| (*t == Ty::T).then(|| arg.clone()))
    }

    /// Inside a method whose own type parameter is named `T`, every `T`
    /// means the method's.
    fn shadowed(&self) -> Ty {
        self.map(&|t| (*t == Ty::T).then_some(Ty::M))
    }

    /// The type as the runtime sees it, which is what C# forbids two
    /// members' signatures to share: no `dynamic`, `nint` or tuple names.
    fn erased(&self) -> Ty {
        self.map(&|t| match t {
            Ty::Dynamic => Some(Ty::Object),
            Ty::NInt => Some(Ty::IntPtr),
            Ty::Tuple(a, b, _) => {
                Some(Ty::Tuple(Box::new(a.erased()), Box::new(b.erased()), false))
            }
            _ => None,
        })
    }

    fn mentions_t(&self) -> bool {
        self.any(&|t| *t == Ty::T)
    }

    fn dims(&self, out: &mut BTreeSet<Dim>) {
        let dim = match self {
            Ty::Object => Dim::Object,
            Ty::Dynamic => Dim::Dynamic,
            Ty::NInt => Dim::NInt,
            Ty::IntPtr => Dim::IntPtr,
            Ty::Array(e) => {
                e.dims(out);
                Dim::Array
            }
            Ty::Tuple(a, b, named) => {
                a.dims(out);
                b.dims(out);
                if *named { Dim::NamedTuple } else { Dim::Tuple }
            }
            Ty::Nested(a, b) => {
                a.dims(out);
                b.dims(out);
                Dim::Nested
            }
            Ty::Int | Ty::Str | Ty::T | Ty::M => return,
        };
        out.insert(dim);
    }
}

/// A dimension of the vocabulary, for the liveness check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Dim {
    Object,
    Dynamic,
    NInt,
    IntPtr,
    Array,
    Tuple,
    NamedTuple,
    Nested,
    GenericMethod,
    ShadowingT,
    Ref,
    In,
    Out,
    Static,
    Protected,
    PrivateProtected,
    ProtectedInternal,
    Constructor,
    /// An `<inheritdoc>` spelled otherwise: another case, a namespaced
    /// attribute, a namespaced element.
    Spelled,
    /// A reference set without its core library.
    NoCoreLibrary,
}

const ALL_DIMS: [Dim; 20] = [
    Dim::Object,
    Dim::Dynamic,
    Dim::NInt,
    Dim::IntPtr,
    Dim::Array,
    Dim::Tuple,
    Dim::NamedTuple,
    Dim::Nested,
    Dim::GenericMethod,
    Dim::ShadowingT,
    Dim::Ref,
    Dim::In,
    Dim::Out,
    Dim::Static,
    Dim::Protected,
    Dim::PrivateProtected,
    Dim::ProtectedInternal,
    Dim::Constructor,
    Dim::Spelled,
    Dim::NoCoreLibrary,
];

fn leaf(generic: bool, method: bool) -> BoxedStrategy<Ty> {
    let mut options: Vec<(u32, BoxedStrategy<Ty>)> = vec![
        (4, Just(Ty::Int).boxed()),
        (2, Just(Ty::Str).boxed()),
        (1, Just(Ty::Object).boxed()),
        (1, Just(Ty::Dynamic).boxed()),
        (1, Just(Ty::NInt).boxed()),
        (1, Just(Ty::IntPtr).boxed()),
    ];
    if generic {
        options.push((4, Just(Ty::T).boxed()));
    }
    if method {
        options.push((4, Just(Ty::M).boxed()));
    }
    prop::strategy::Union::new_weighted(options).boxed()
}

/// A type: mostly a leaf, sometimes an array, tuple or nested generic of
/// leaves (or of those).
fn ty(generic: bool, method: bool) -> BoxedStrategy<Ty> {
    let leaves = leaf(generic, method);
    let compound = leaves.clone().prop_recursive(2, 6, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|e| Ty::Array(Box::new(e))),
            (inner.clone(), inner.clone(), any::<bool>()).prop_map(|(a, b, n)| Ty::Tuple(
                Box::new(a),
                Box::new(b),
                n
            )),
            (inner.clone(), inner).prop_map(|(a, b)| Ty::Nested(Box::new(a), Box::new(b))),
        ]
    });
    prop_oneof![3 => leaves, 1 => compound].boxed()
}

/// A type argument of a base type or interface. C# rejects `dynamic`
/// anywhere in an implemented interface (CS1966).
fn type_arg(generic: bool, interface: bool) -> BoxedStrategy<Ty> {
    ty(generic, false)
        .prop_map(move |t| {
            if interface {
                t.map(&|t| (*t == Ty::Dynamic).then_some(Ty::Object))
            } else {
                t
            }
        })
        .boxed()
}

/// How a parameter is passed: by value, `ref`, `in` (which carries a custom
/// modifier Roslyn's comparers see on a virtual member), or `out`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum RefKind {
    Value,
    Ref,
    In,
    Out,
}

/// A method's one parameter `x`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Param {
    ty: Ty,
    rk: RefKind,
}

impl Param {
    fn render(&self, m: &str) -> String {
        let ty = self.ty.render(m);
        match self.rk {
            RefKind::Value => ty,
            RefKind::Ref => format!("ref {ty}"),
            RefKind::In => format!("in {ty}"),
            RefKind::Out => format!("out {ty}"),
        }
    }

    fn subst(&self, arg: &Ty) -> Param {
        Param {
            ty: self.ty.subst(arg),
            rk: self.rk,
        }
    }

    fn dims(&self, out: &mut BTreeSet<Dim>) {
        self.ty.dims(out);
        match self.rk {
            RefKind::Value => {}
            RefKind::Ref => {
                out.insert(Dim::Ref);
            }
            RefKind::In => {
                out.insert(Dim::In);
            }
            RefKind::Out => {
                out.insert(Dim::Out);
            }
        }
    }
}

fn ref_kind() -> impl Strategy<Value = RefKind> {
    prop_oneof![
        5 => Just(RefKind::Value),
        1 => Just(RefKind::Ref),
        1 => Just(RefKind::In),
        1 => Just(RefKind::Out),
    ]
}

fn param(generic: bool, method: bool) -> impl Strategy<Value = Param> {
    (ty(generic, method), ref_kind()).prop_map(|(ty, rk)| Param { ty, rk })
}

/// A constructor's parameter: weighted to the types whose distinctions live
/// in attributes, which Roslyn's base-constructor rule compares.
fn ctor_param(generic: bool) -> impl Strategy<Value = Param> {
    let sensitive = prop_oneof![
        Just(Ty::Object),
        Just(Ty::Dynamic),
        Just(Ty::NInt),
        Just(Ty::IntPtr),
        Just(Ty::Tuple(Box::new(Ty::Int), Box::new(Ty::Str), true)),
        Just(Ty::Tuple(Box::new(Ty::Int), Box::new(Ty::Str), false)),
        Just(Ty::Array(Box::new(Ty::Dynamic))),
        Just(Ty::Array(Box::new(Ty::Object))),
    ];
    (
        prop_oneof![2 => sensitive, 1 => ty(generic, false)],
        prop_oneof![6 => Just(RefKind::Value), 1 => ref_kind()],
    )
        .prop_map(|(ty, rk)| Param { ty, rk })
}

/// A method's own type parameter, if it has one, and its name: `T` shadows a
/// generic declaring type's `T` (C# warns, CS0693, but accepts it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum MName {
    U,
    T,
}

impl MName {
    fn render(self) -> &'static str {
        match self {
            MName::U => "U",
            MName::T => "T",
        }
    }
}

fn mtp(weight_none: u32) -> impl Strategy<Value = Option<MName>> {
    prop_oneof![
        weight_none => Just(None),
        1 => Just(Some(MName::U)),
        1 => Just(Some(MName::T)),
    ]
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
    /// A bare `<inheritdoc/>` spelled otherwise.
    Spelled(Spelling),
}

/// How an `<inheritdoc>` may be spelled besides `<inheritdoc/>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// `<InheritDoc/>`: C# compares element names ignoring case.
    Case,
    /// `<inheritdoc xml:lang="en"/>`: a namespaced attribute.
    Lang,
    /// `<inheritdoc xmlns="urn:x"/>`: a namespaced element, which Roslyn
    /// does not expand.
    Namespaced,
}

impl Doc {
    fn dims(self) -> BTreeSet<Dim> {
        match self {
            Doc::Spelled(_) => BTreeSet::from([Dim::Spelled]),
            _ => BTreeSet::new(),
        }
    }
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
        1 => prop_oneof![
            Just(Spelling::Case),
            Just(Spelling::Lang),
            Just(Spelling::Namespaced),
        ]
        .prop_map(Doc::Spelled),
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

/// A method signature with its doc: return and parameter types (in the
/// declaring type's context), and its own type parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Method {
    ret: Ty,
    param: Param,
    mtp: Option<MName>,
    doc: Doc,
}

impl Method {
    fn dims(&self, generic: bool, out: &mut BTreeSet<Dim>) {
        self.ret.dims(out);
        self.param.dims(out);
        if self.mtp.is_some() {
            out.insert(Dim::GenericMethod);
        }
        if generic && self.mtp == Some(MName::T) {
            out.insert(Dim::ShadowingT);
        }
    }
}

/// A method of a type that is `generic` or not, documented by `doc`.
fn method(generic: bool, weight_none: u32, doc: BoxedStrategy<Doc>) -> BoxedStrategy<Method> {
    mtp(weight_none)
        .prop_flat_map(move |mtp| {
            let m = mtp.is_some();
            (Just(mtp), ty(generic, m), param(generic, m), doc.clone())
        })
        .prop_map(move |(mtp, ret, param, doc)| {
            let shadow = generic && mtp == Some(MName::T);
            let fix = |t: Ty| if shadow { t.shadowed() } else { t };
            Method {
                ret: fix(ret),
                param: Param {
                    ty: fix(param.ty),
                    rk: param.rk,
                },
                mtp,
                doc,
            }
        })
        .boxed()
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

/// An interface method: its signature, body, and whether it is a static
/// (abstract or virtual) member.
#[derive(Debug, Clone)]
struct IfaceMethod {
    m: Method,
    body: Body,
    stat: bool,
}

#[derive(Debug, Clone)]
struct IfaceSpec {
    generic: bool,
    /// A base interface (an earlier one) and its type argument.
    base: Option<(usize, Ty)>,
    /// Methods `N{i}`.
    methods: Vec<IfaceMethod>,
    property: Option<(Ty, Doc)>,
    doc: Doc,
}

/// Member accessibility of a class's own virtual method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Public,
    Protected,
    PrivateProtected,
    ProtectedInternal,
}

impl Access {
    fn render(self) -> &'static str {
        match self {
            Access::Public => "public",
            Access::Protected => "protected",
            Access::PrivateProtected => "private protected",
            Access::ProtectedInternal => "protected internal",
        }
    }

    fn dim(self) -> Option<Dim> {
        match self {
            Access::Public => None,
            Access::Protected => Some(Dim::Protected),
            Access::PrivateProtected => Some(Dim::PrivateProtected),
            Access::ProtectedInternal => Some(Dim::ProtectedInternal),
        }
    }
}

fn access() -> impl Strategy<Value = Access> {
    prop_oneof![
        4 => Just(Access::Public),
        1 => Just(Access::Protected),
        1 => Just(Access::PrivateProtected),
        1 => Just(Access::ProtectedInternal),
    ]
}

/// A class's new virtual method, and the inherited method (by position) it
/// overloads and whether by ref-kind alone.
#[derive(Debug, Clone)]
struct OwnMethod {
    m: Method,
    access: Access,
    overload: Option<(usize, bool)>,
}

/// A class's static method, and whether it takes an interface method's name
/// `N{i}` (when no member of that name and shape is already declared).
#[derive(Debug, Clone)]
struct StaticMethod {
    m: Method,
    named_like: Option<usize>,
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
    /// New virtual methods: `M{class}_{i}`, or — when the overload names an
    /// inherited method and the parameter differs from every inherited one of
    /// that name — an overload of it (with `flip`, by ref-kind alone).
    own: Vec<OwnMethod>,
    statics: Vec<StaticMethod>,
    /// Docs for interface implementations, by position (cycled).
    impl_docs: Vec<Doc>,
    /// The documented constructor's parameter (beside a parameterless
    /// constructor every derived constructor chains to).
    ctor: Param,
    ctor_doc: Doc,
    doc: Doc,
}

#[derive(Debug, Clone)]
struct Program {
    interfaces: Vec<IfaceSpec>,
    classes: Vec<ClassSpec>,
    /// Whether the reference set holds the core library (System.Runtime).
    core: bool,
}

fn iface_spec(i: usize) -> BoxedStrategy<IfaceSpec> {
    any::<bool>()
        .prop_flat_map(move |generic| {
            let iface_method = (
                method(generic, 3, iface_doc().boxed()),
                body(),
                prop::bool::weighted(0.25),
            )
                .prop_map(|(m, body, stat)| IfaceMethod {
                    m,
                    body,
                    stat: stat && matches!(body, Body::Abstract | Body::Virtual),
                });
            (
                Just(generic),
                if i == 0 {
                    Just(None).boxed()
                } else {
                    prop::option::of((0..i, type_arg(generic, true))).boxed()
                },
                prop::collection::vec(iface_method, 1..3),
                prop::option::of((ty(generic, false), iface_doc())),
                doc(),
            )
        })
        .prop_map(|(generic, base, methods, property, doc)| IfaceSpec {
            generic,
            base,
            methods,
            property,
            doc,
        })
        .boxed()
}

fn class_spec(k: usize) -> BoxedStrategy<ClassSpec> {
    any::<bool>()
        .prop_flat_map(move |generic| {
            (
                Just(generic),
                if k == 0 {
                    Just(None).boxed()
                } else {
                    prop::option::of((0..k, type_arg(generic, false))).boxed()
                },
                prop::collection::vec((0..2usize, type_arg(generic, true), any::<bool>()), 0..3),
                prop::collection::vec((0..5u8, doc()), 0..6),
                prop::collection::vec(
                    (
                        method(generic, 3, doc().boxed()),
                        access(),
                        prop::option::of((0..4usize, prop::bool::weighted(0.6))),
                    )
                        .prop_map(|(m, access, overload)| OwnMethod {
                            m,
                            access,
                            overload,
                        }),
                    0..3,
                ),
                prop::collection::vec(
                    (
                        method(generic, 3, doc().boxed()),
                        prop::option::of(0..2usize),
                    )
                        .prop_map(|(m, named_like)| StaticMethod { m, named_like }),
                    0..2,
                ),
                prop::collection::vec(doc(), 1..6),
                ctor_param(generic),
                doc(),
                doc(),
            )
        })
        .prop_map(
            |(
                generic,
                base,
                interfaces,
                inherited,
                own,
                statics,
                impl_docs,
                ctor,
                ctor_doc,
                doc,
            )| {
                ClassSpec {
                    generic,
                    base,
                    interfaces,
                    inherited,
                    own,
                    statics,
                    impl_docs,
                    ctor,
                    ctor_doc,
                    doc,
                }
            },
        )
        .boxed()
}

fn program() -> impl Strategy<Value = Program> {
    (
        iface_spec(0),
        iface_spec(1),
        class_spec(0),
        class_spec(1),
        class_spec(2),
        prop::bool::weighted(0.75),
    )
        .prop_map(|(i0, i1, c0, c1, c2, core)| Program {
            interfaces: vec![i0, i1],
            classes: vec![c0, c1, c2],
            core,
        })
}

/// The documentation comment for one symbol. `id` is a unique token for its
/// text; `tparams` the type parameters in scope to refer to; `params`/
/// `returns` whether to document those.
fn render_doc(
    out: &mut String,
    d: Doc,
    id: &str,
    tparams: &[&str],
    params: bool,
    returns: bool,
    classes: &[String],
) {
    let tp: String = tparams
        .iter()
        .map(|t| format!(" of <typeparamref name=\"{t}\"/>"))
        .collect();
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
        Doc::Spelled(Spelling::Case) => writeln!(out, "/// <InheritDoc/>").unwrap(),
        Doc::Spelled(Spelling::Lang) => writeln!(out, "/// <inheritdoc xml:lang=\"en\"/>").unwrap(),
        Doc::Spelled(Spelling::Namespaced) => {
            writeln!(out, "/// <inheritdoc xmlns=\"urn:x\"/>").unwrap()
        }
    }
}

/// The type parameters a method's doc can name: the type's `T` (unless the
/// method's own shadows it) and the method's.
fn tparams_of(generic: bool, mtp: Option<MName>) -> Vec<&'static str> {
    let mut out = Vec::new();
    if generic && mtp != Some(MName::T) {
        out.push("T");
    }
    if let Some(m) = mtp {
        out.push(m.render());
    }
    out
}

/// `ret name<M>(param x)`.
fn head(ret: &Ty, name: &str, mtp: Option<MName>, param: &Param) -> String {
    let m = mtp.map_or("", MName::render);
    let tp = mtp.map(|n| format!("<{}>", n.render())).unwrap_or_default();
    format!("{} {name}{tp}({} x)", ret.render(m), param.render(m))
}

/// A method body: an `out` parameter must be assigned.
fn body_for(param: &Param) -> &'static str {
    if param.rk == RefKind::Out {
        "{ x = default!; return default!; }"
    } else {
        "=> default!;"
    }
}

/// A virtual method signature as seen from a class.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sig {
    name: String,
    ret: Ty,
    param: Param,
    mtp: Option<MName>,
    access: Access,
}

impl Sig {
    /// What tells two overloads of one name apart to the runtime.
    fn ident(&self) -> (Ty, RefKind, bool) {
        (self.param.ty.erased(), self.param.rk, self.mtp.is_some())
    }

    /// What C# forbids two members of one type to differ by alone: the
    /// runtime type, whether it is passed by reference at all, and arity.
    fn shape(&self) -> (Ty, bool, bool) {
        (
            self.param.ty.erased(),
            self.param.rk != RefKind::Value,
            self.mtp.is_some(),
        )
    }

    fn subst(&self, arg: &Ty) -> Sig {
        Sig {
            name: self.name.clone(),
            ret: self.ret.subst(arg),
            param: self.param.subst(arg),
            mtp: self.mtp,
            access: self.access,
        }
    }

    fn dims(&self, generic: bool) -> BTreeSet<Dim> {
        let mut out = BTreeSet::new();
        Method {
            ret: self.ret.clone(),
            param: self.param.clone(),
            mtp: self.mtp,
            doc: Doc::None,
        }
        .dims(generic, &mut out);
        out.extend(self.access.dim());
        out
    }
}

/// One interface instance in a class's closure: the interface and its
/// argument in the class's context.
fn closure(p: &Program, listed: &[(usize, Ty)]) -> Vec<(usize, Ty)> {
    let mut out: Vec<(usize, Ty)> = Vec::new();
    for (i, arg) in listed {
        let mut cur = Some((*i, arg.clone()));
        while let Some((i, arg)) = cur {
            cur = p.interfaces[i].base.as_ref().map(|(b, barg)| {
                (
                    *b,
                    if p.interfaces[i].generic {
                        barg.subst(&arg)
                    } else {
                        barg.clone()
                    },
                )
            });
            out.push((i, arg));
        }
    }
    out
}

fn iface_name(i: usize, generic: bool, arg: &Ty) -> String {
    if generic {
        format!("I{i}<{}>", arg.render(""))
    } else {
        format!("I{i}")
    }
}

/// The dimensions of each member a program declares, by declaring type and
/// member name (`""` for the type, `"ctor"` for its constructor).
type Dims = BTreeMap<(String, String), BTreeSet<Dim>>;

/// The implicit implementations (and statics) a class declares, by name and
/// signature shape, with the exact signature each was declared with.
type ImplicitMembers = BTreeMap<(String, Option<(Ty, bool, bool)>), String>;

/// Render the program to C#, with each member's dimensions.
fn render(p: &Program) -> (String, Dims) {
    let mut out = String::from(DYNAMIC_ATTRIBUTE);
    out.push_str(
        "namespace Gen {\n\n\
         /// <summary>A generic outer type.</summary>\n\
         public class Outer<A>\n{\n    \
         /// <summary>A generic type nested in a generic type.</summary>\n    \
         public class Inner<B> { }\n}\n\n",
    );
    let mut dims = Dims::new();
    let mut note = |owner: &str, name: &str, doc: Doc, d: BTreeSet<Dim>| {
        let slot = dims
            .entry((owner.to_string(), name.to_string()))
            .or_default();
        slot.extend(d);
        slot.extend(doc.dims());
    };
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
    let type_tp = |generic: bool| if generic { vec!["T"] } else { vec![] };
    for (i, spec) in p.interfaces.iter().enumerate() {
        let id = format!("I{i}");
        note(&id, "", spec.doc, BTreeSet::new());
        render_doc(
            &mut out,
            spec.doc,
            &id,
            &type_tp(spec.generic),
            false,
            false,
            &class_crefs,
        );
        let tp = if spec.generic { "<T>" } else { "" };
        let base = spec
            .base
            .as_ref()
            .map(|(b, arg)| format!(" : {}", iface_name(*b, p.interfaces[*b].generic, arg)))
            .unwrap_or_default();
        writeln!(out, "public interface I{i}{tp}{base}\n{{").unwrap();
        for (m, im) in spec.methods.iter().enumerate() {
            let name = format!("N{m}");
            render_doc(
                &mut out,
                im.m.doc,
                &format!("I{i}.{name}"),
                &tparams_of(spec.generic, im.m.mtp),
                true,
                true,
                &class_crefs,
            );
            let h = head(&im.m.ret, &name, im.m.mtp, &im.m.param);
            let b = body_for(&im.m.param);
            let stat = if im.stat { "static " } else { "" };
            match im.body {
                Body::Abstract => writeln!(out, "    {stat}abstract {h};"),
                Body::Virtual => writeln!(out, "    {stat}virtual {h} {b}"),
                Body::Sealed => writeln!(out, "    public sealed {h} {b}"),
                Body::Private => writeln!(out, "    private {h} {b}"),
            }
            .unwrap();
            let mut d = BTreeSet::new();
            im.m.dims(spec.generic, &mut d);
            if im.stat {
                d.insert(Dim::Static);
            }
            note(&id, &name, im.m.doc, d);
        }
        if let Some((t, doc)) = &spec.property {
            render_doc(
                &mut out,
                *doc,
                &format!("I{i}.Q"),
                &type_tp(spec.generic),
                false,
                false,
                &class_crefs,
            );
            writeln!(out, "    {} Q {{ get; }}", t.render("")).unwrap();
            let mut d = BTreeSet::new();
            t.dims(&mut d);
            note(&id, "Q", *doc, d);
        }
        out.push_str("}\n\n");
    }
    // Each class's virtual methods as seen from it, for its derived classes.
    let mut virtuals: Vec<Vec<Sig>> = Vec::new();
    for (k, c) in p.classes.iter().enumerate() {
        let id = format!("C{k}");
        let class_tp = type_tp(c.generic);
        render_doc(&mut out, c.doc, &id, &class_tp, false, false, &class_crefs);
        note(&id, "", c.doc, BTreeSet::new());
        let tp = if c.generic { "<T>" } else { "" };
        let mut supers: Vec<String> = Vec::new();
        let inherited: Vec<Sig> = match &c.base {
            Some((b, arg)) => {
                let base = &p.classes[*b];
                supers.push(if base.generic {
                    format!("C{b}<{}>", arg.render(""))
                } else {
                    format!("C{b}")
                });
                virtuals[*b]
                    .iter()
                    .map(|s| {
                        if base.generic {
                            s.subst(arg)
                        } else {
                            s.clone()
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
        for (i, arg, explicit) in &c.interfaces {
            let arg = if p.interfaces[*i].generic {
                arg.clone()
            } else {
                Ty::Int
            };
            let more = closure(p, &[(*i, arg.clone())]);
            if more.iter().any(|(d, _)| defs.contains(d)) {
                continue;
            }
            defs.extend(more.iter().map(|(d, _)| *d));
            supers.push(iface_name(*i, p.interfaces[*i].generic, &arg));
            listed.push((*i, arg, *explicit));
        }
        let supers = if supers.is_empty() {
            String::new()
        } else {
            format!(" : {}", supers.join(", "))
        };
        writeln!(out, "public class C{k}{tp}{supers}\n{{").unwrap();
        // The parameterless constructor every derived one chains to, so
        // that any parameter type (`dynamic`, `out`) chains alike.
        let chain = if c.base.is_some() { " : base()" } else { "" };
        writeln!(out, "    protected C{k}(){chain} {{ }}").unwrap();
        render_doc(
            &mut out,
            c.ctor_doc,
            &format!("C{k}.ctor"),
            &class_tp,
            true,
            false,
            &class_crefs,
        );
        let ctor_body = if c.ctor.rk == RefKind::Out {
            "{ x = default!; }"
        } else {
            "{ }"
        };
        writeln!(
            out,
            "    public C{k}({} x){chain} {ctor_body}",
            c.ctor.render("")
        )
        .unwrap();
        let mut d = BTreeSet::from([Dim::Constructor]);
        c.ctor.dims(&mut d);
        note(&id, "ctor", c.ctor_doc, d);

        let mut mine: Vec<Sig> = Vec::new();
        // What this class itself declares: C# forbids two of its own methods
        // to differ only by ref-kind.
        let mut declared_here: Vec<Sig> = Vec::new();
        for (j, s) in inherited.iter().enumerate() {
            let (choice, d) = c.inherited.get(j).copied().unwrap_or((0, Doc::None));
            // Two inherited overloads differing by ref-kind alone cannot both
            // be redeclared here (CS0663): the second is left inherited.
            let clashes = declared_here
                .iter()
                .any(|m| m.name == s.name && m.shape() == s.shape());
            let modifier = match choice {
                _ if clashes => {
                    mine.push(s.clone());
                    continue;
                }
                1 | 3 | 4 => "override",
                2 => "new virtual",
                _ => {
                    mine.push(s.clone());
                    continue;
                }
            };
            render_doc(
                &mut out,
                d,
                &format!("C{k}.{}", s.name),
                &tparams_of(c.generic, s.mtp),
                true,
                true,
                &class_crefs,
            );
            writeln!(
                out,
                "    {} {modifier} {} {}",
                s.access.render(),
                head(&s.ret, &s.name, s.mtp, &s.param),
                body_for(&s.param)
            )
            .unwrap();
            note(&id, &s.name, d, s.dims(c.generic));
            mine.push(s.clone());
            declared_here.push(s.clone());
        }
        for (j, own) in c.own.iter().enumerate() {
            // With `flip`, the overload takes the inherited method's own
            // parameter type by another ref-kind — the shape where only the
            // ref-kind (and the `in` modifier) tells two signatures apart.
            let flipped = own.overload.and_then(|(o, flip)| {
                let s = inherited.get(o % inherited.len().max(1))?;
                flip.then(|| (s.param.clone(), s.mtp))
            });
            let (param, mtp, ret) = match flipped.clone() {
                Some((q, mtp)) => {
                    // The overload takes the inherited method's type
                    // parameter, so its return type is redrawn into that
                    // scope: no method parameter without one, and the type's
                    // `T` shadowed by a method's `T`.
                    let ret = match mtp {
                        None => own.m.ret.map(&|t| (*t == Ty::M).then_some(Ty::Int)),
                        Some(MName::T) if c.generic => own.m.ret.shadowed(),
                        Some(_) => own.m.ret.clone(),
                    };
                    let rk = match q.rk {
                        RefKind::Value => RefKind::Ref,
                        RefKind::Ref => RefKind::In,
                        RefKind::In | RefKind::Out => RefKind::Ref,
                    };
                    (Param { ty: q.ty, rk }, mtp, ret)
                }
                None => (own.m.param.clone(), own.m.mtp, own.m.ret.clone()),
            };
            let candidate = Sig {
                name: String::new(),
                ret: ret.clone(),
                param: param.clone(),
                mtp,
                access: own.access,
            };
            // An overload of an inherited name, when no method of the name
            // has this runtime signature, none declared here differs from it
            // by ref-kind alone, and neither parameter mentions `T`, which a
            // derived instantiation could make collide. An inherited method
            // differing by ref-kind alone is allowed: that is the shape where
            // Roslyn's comparers see the `in` modifier.
            let overloaded = own
                .overload
                .and_then(|(o, _)| inherited.get(o % inherited.len().max(1)))
                .filter(|s| {
                    let named = |m: &&Sig| m.name == s.name;
                    !param.ty.mentions_t()
                        && mine
                            .iter()
                            .filter(named)
                            .all(|m| !m.param.ty.mentions_t() && m.ident() != candidate.ident())
                        && declared_here
                            .iter()
                            .filter(named)
                            .all(|m| m.shape() != candidate.shape())
                });
            let name = match overloaded {
                Some(s) => s.name.clone(),
                None => format!("M{k}_{j}"),
            };
            // A ref-kind overload is documented in full: it is a wrong answer
            // only when it has text to show.
            let doc = if flipped.is_some() {
                Doc::Full
            } else {
                own.m.doc
            };
            render_doc(
                &mut out,
                doc,
                &format!("C{k}.{name}"),
                &tparams_of(c.generic, mtp),
                true,
                true,
                &class_crefs,
            );
            writeln!(
                out,
                "    {} virtual {} {}",
                own.access.render(),
                head(&ret, &name, mtp, &param),
                body_for(&param)
            )
            .unwrap();
            let sig = Sig { name, ..candidate };
            note(&id, &sig.name, doc, sig.dims(c.generic));
            mine.push(sig.clone());
            declared_here.push(sig);
        }
        // Interface members: implicit ones are shared by every instance that
        // needs the same name and parameter; one with the same parameter but
        // another return type (or static-ness) must be explicit.
        let mut implicit: ImplicitMembers = BTreeMap::new();
        let mut doc_cursor = 0;
        let mut next_doc = || {
            let d = c.impl_docs[doc_cursor % c.impl_docs.len()];
            doc_cursor += 1;
            d
        };
        for (i, arg, explicit) in &listed {
            for (d, darg) in closure(p, &[(*i, arg.clone())]) {
                let spec = &p.interfaces[d];
                let sub = |t: &Ty| {
                    if spec.generic {
                        t.subst(&darg)
                    } else {
                        t.clone()
                    }
                };
                let qualifier = iface_name(d, spec.generic, &darg);
                for (m, im) in spec.methods.iter().enumerate() {
                    let ret = sub(&im.m.ret);
                    let param = Param {
                        ty: sub(&im.m.param.ty),
                        rk: im.m.param.rk,
                    };
                    let name = format!("N{m}");
                    let key = (
                        name.clone(),
                        Some((
                            param.ty.erased(),
                            param.rk != RefKind::Value,
                            im.m.mtp.is_some(),
                        )),
                    );
                    let stat = if im.stat { "static " } else { "" };
                    let exact = format!("{stat}{}", head(&ret, &name, im.m.mtp, &param));
                    let clash = implicit.get(&key).is_some_and(|r| *r != exact);
                    // A sealed or private default cannot be implemented, so
                    // not explicitly either: a same-named public member is
                    // declared (unrelated to it) unless that would clash.
                    let unimplementable = matches!(im.body, Body::Sealed | Body::Private);
                    if unimplementable && clash {
                        continue;
                    }
                    let explicit = !unimplementable && (*explicit || clash);
                    let doc = next_doc();
                    let mut dm = BTreeSet::new();
                    Method {
                        ret: ret.clone(),
                        param: param.clone(),
                        mtp: im.m.mtp,
                        doc,
                    }
                    .dims(c.generic, &mut dm);
                    if im.stat {
                        dm.insert(Dim::Static);
                    }
                    if explicit {
                        render_doc(
                            &mut out,
                            doc,
                            &format!("C{k}.{qualifier}.{name}"),
                            &tparams_of(c.generic, im.m.mtp),
                            true,
                            true,
                            &class_crefs,
                        );
                        writeln!(
                            out,
                            "    {stat}{} {}",
                            head(&ret, &format!("{qualifier}.{name}"), im.m.mtp, &param),
                            body_for(&param)
                        )
                        .unwrap();
                        note(&id, &name, doc, dm.clone());
                    } else if let std::collections::btree_map::Entry::Vacant(e) =
                        implicit.entry(key)
                    {
                        e.insert(exact);
                        render_doc(
                            &mut out,
                            doc,
                            &format!("C{k}.{name}"),
                            &tparams_of(c.generic, im.m.mtp),
                            true,
                            true,
                            &class_crefs,
                        );
                        writeln!(
                            out,
                            "    public {stat}{} {}",
                            head(&ret, &name, im.m.mtp, &param),
                            body_for(&param)
                        )
                        .unwrap();
                        note(&id, &name, doc, dm.clone());
                    }
                }
                if let Some((t, _)) = &spec.property {
                    let t = sub(t);
                    let key = ("Q".to_string(), None);
                    let explicit =
                        *explicit || implicit.get(&key).is_some_and(|r| *r != t.render(""));
                    let doc = next_doc();
                    let mut dq = BTreeSet::new();
                    t.dims(&mut dq);
                    if explicit {
                        render_doc(
                            &mut out,
                            doc,
                            &format!("C{k}.{qualifier}.Q"),
                            &class_tp,
                            false,
                            false,
                            &class_crefs,
                        );
                        writeln!(out, "    {} {qualifier}.Q => default!;", t.render("")).unwrap();
                        note(&id, "Q", doc, dq.clone());
                    } else if let std::collections::btree_map::Entry::Vacant(e) =
                        implicit.entry(key)
                    {
                        e.insert(t.render(""));
                        render_doc(
                            &mut out,
                            doc,
                            &format!("C{k}.Q"),
                            &class_tp,
                            false,
                            false,
                            &class_crefs,
                        );
                        writeln!(out, "    public {} Q => default!;", t.render("")).unwrap();
                        note(&id, "Q", doc, dq.clone());
                    }
                }
            }
        }
        // Statics, some taking an interface method's name: one that would
        // collide with a member already declared takes its own name.
        for (j, s) in c.statics.iter().enumerate() {
            let shape = Some((
                s.m.param.ty.erased(),
                s.m.param.rk != RefKind::Value,
                s.m.mtp.is_some(),
            ));
            let name = s
                .named_like
                .map(|n| format!("N{n}"))
                .filter(|n| !implicit.contains_key(&(n.clone(), shape.clone())))
                .unwrap_or_else(|| format!("S{k}_{j}"));
            implicit.insert((name.clone(), shape), String::new());
            render_doc(
                &mut out,
                s.m.doc,
                &format!("C{k}.{name}"),
                &tparams_of(c.generic, s.m.mtp),
                true,
                true,
                &class_crefs,
            );
            writeln!(
                out,
                "    public static {} {}",
                head(&s.m.ret, &name, s.m.mtp, &s.m.param),
                body_for(&s.m.param)
            )
            .unwrap();
            let mut d = BTreeSet::from([Dim::Static]);
            s.m.dims(c.generic, &mut d);
            note(&id, &name, s.m.doc, d);
        }
        out.push_str("}\n\n");
        virtuals.push(mine);
    }
    out.push_str("}\n");
    (out, dims)
}

/// The declaring type and member name a documentation ID names, as [`Dims`]
/// keys them: generic arities and explicit-implementation qualifiers dropped.
fn owner_and_name(key: &str) -> (String, String) {
    let id = key.get(2..).unwrap_or_default();
    let head = id.split('(').next().unwrap_or_default();
    let head = head.strip_prefix("Gen.").unwrap_or(head);
    let strip = |s: &str| s.split('`').next().unwrap_or_default().to_string();
    match head.split_once('.') {
        None => (strip(head), String::new()),
        Some((owner, member)) => {
            let member = member.rsplit('#').next().unwrap_or_default();
            (strip(owner), strip(member))
        }
    }
}

/// One generated program's comparisons, with each member's dimensions.
/// Panics on a disagreement, and on a program the compiler rejects — a
/// generator that writes invalid C# must be fixed, not sampled around.
fn run(p: &Program) -> (Vec<Compared>, Dims) {
    let (source, mut dims) = render(p);
    if !p.core {
        for d in dims.values_mut() {
            d.insert(Dim::NoCoreLibrary);
        }
    }
    let fx = if p.core {
        fixture(&source)
    } else {
        fixture_without_core_library(&source)
    }
    .unwrap_or_else(|e| panic!("the generated program does not compile: {e:#?}\n{source}"));
    let compared = compare(&fx);
    if let Some(c) = compared
        .iter()
        .find(|c| matches!(c.verdict, Verdict::Disagrees { .. }))
        && let Verdict::Disagrees { ours, roslyn } = &c.verdict
    {
        panic!(
            "{} disagrees with Roslyn:\nours: {ours}\nroslyn: {roslyn}\nprogram:\n{source}",
            c.key
        );
    }
    (compared, dims)
}

/// Override chains that stress signature comparison: `C2 : C1 : C0`, where
/// `C1` overloads `C0`'s methods by parameter type and by ref-kind alone, and
/// `C2` overrides what it inherits with `<inheritdoc/>`. Every method above
/// `C2` is documented in full, so inheriting from the wrong overload shows;
/// so is every constructor but `C2`'s, which inherits.
fn override_chain() -> impl Strategy<Value = Program> {
    let iface = IfaceSpec {
        generic: false,
        base: None,
        methods: vec![IfaceMethod {
            m: Method {
                ret: Ty::Int,
                param: Param {
                    ty: Ty::Int,
                    rk: RefKind::Value,
                },
                mtp: None,
                doc: Doc::Full,
            },
            body: Body::Abstract,
            stat: false,
        }],
        property: None,
        doc: Doc::Full,
    };
    let class = |base: Option<usize>, overloads: bool, inherit: Doc| {
        any::<bool>()
            .prop_flat_map(move |generic| {
                (
                    Just(generic),
                    match base {
                        Some(b) => type_arg(generic, false)
                            .prop_map(move |t| Some((b, t)))
                            .boxed(),
                        None => Just(None).boxed(),
                    },
                    prop::collection::vec(
                        (
                            prop_oneof![1 => Just(0u8), 3 => Just(1u8), 1 => Just(2u8)],
                            Just(inherit),
                        ),
                        0..6,
                    ),
                    prop::collection::vec(
                        (
                            method(generic, 1, Just(Doc::Full).boxed()),
                            access(),
                            if overloads {
                                prop::option::of((0..4usize, any::<bool>())).boxed()
                            } else {
                                Just(None).boxed()
                            },
                        )
                            .prop_map(|(m, access, overload)| OwnMethod {
                                m,
                                access,
                                overload,
                            }),
                        1..4,
                    ),
                    ctor_param(generic),
                )
            })
            .prop_map(move |(generic, base, inherited, own, ctor)| ClassSpec {
                generic,
                base,
                interfaces: Vec::new(),
                inherited,
                own,
                statics: Vec::new(),
                impl_docs: vec![Doc::Full],
                ctor,
                ctor_doc: inherit,
                doc: Doc::Full,
            })
    };
    (
        class(None, false, Doc::Full),
        class(Some(0), true, Doc::Full),
        class(Some(1), true, Doc::Bare),
    )
        .prop_map(move |(c0, c1, c2)| Program {
            interfaces: vec![iface.clone(), iface.clone()],
            classes: vec![c0, c1, c2],
            core: true,
        })
}

/// Run `run` over `cases` programs from `strategy`, persisting a failure's
/// seed beside this file as the `proptest!` macro does.
fn check(strategy: impl Strategy<Value = Program>, cases: u32) {
    let config = Config {
        cases,
        source_file: Some(file!()),
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    if let Err(e) = runner.run(&strategy, |p| {
        run(&p);
        Ok(())
    }) {
        panic!("{e}");
    }
}

#[test]
fn generated_hierarchies_expand_exactly_as_roslyn() {
    check(program(), cases(DEFAULT_CASES));
}

#[test]
fn generated_override_chains_expand_exactly_as_roslyn() {
    check(override_chain(), cases(DEFAULT_CASES));
}

#[test]
#[ignore = "deep run: CI's <inheritdoc> sweep step"]
fn deep_generated_hierarchies_expand_exactly_as_roslyn() {
    check(program(), cases(DEEP_CASES));
}

#[test]
#[ignore = "deep run: CI's <inheritdoc> sweep step"]
fn deep_generated_override_chains_expand_exactly_as_roslyn() {
    check(override_chain(), cases(DEEP_CASES));
}

/// Per dimension: the compared entries whose member carries it, and how many
/// of those expanded.
#[derive(Debug, Default)]
struct Reach {
    compared: usize,
    expanded: usize,
}

/// Over a fixed sample of each generator: every dimension of the vocabulary
/// reaches a compared entry — and, except where noted, an expanded one — and
/// the comparison mostly expands. Without this the property could pass on
/// a dimension the generator never emits, or one the expansion always
/// declines.
#[test]
fn the_generators_reach_every_dimension() {
    use proptest::strategy::ValueTree;
    fn sample(
        name: &str,
        strategy: impl Strategy<Value = Program>,
        programs: usize,
        min_expanded: usize,
        reach: &mut BTreeMap<Dim, Reach>,
    ) {
        let mut runner = TestRunner::deterministic();
        let mut census = Census::default();
        for _ in 0..programs {
            let p = strategy.new_tree(&mut runner).unwrap().current();
            let (compared, dims) = run(&p);
            for c in &compared {
                let Some(ds) = dims.get(&owner_and_name(&c.key)) else {
                    continue;
                };
                for d in ds {
                    let r = reach.entry(*d).or_default();
                    r.compared += 1;
                    if matches!(c.verdict, Verdict::Agrees) {
                        r.expanded += 1;
                    }
                }
            }
            census.add(&compared);
        }
        census.print(&format!("{name} (liveness sample)"));
        let expanded = census.agrees;
        let declined = census.declined_total();
        assert!(
            expanded >= min_expanded,
            "{name}: too few expansions: {expanded} expanded, {declined} declined"
        );
    }
    let mut reach: BTreeMap<Dim, Reach> = BTreeMap::new();
    sample("hierarchies", program(), 24, 60, &mut reach);
    sample("override chains", override_chain(), 16, 15, &mut reach);
    for d in ALL_DIMS {
        let r = reach.get(&d);
        eprintln!("  {d:?}: {r:?}");
    }
    let mut dead = Vec::new();
    for d in ALL_DIMS {
        match reach.get(&d) {
            Some(r) if r.expanded > 0 => {}
            other => dead.push(format!("{d:?}: {other:?}")),
        }
    }
    assert!(
        dead.is_empty(),
        "dimensions no sampled entry expands: {}",
        dead.join(", ")
    );
}
