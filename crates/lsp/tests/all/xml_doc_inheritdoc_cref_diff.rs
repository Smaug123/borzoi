//! `<inheritdoc cref>` binding against Roslyn over generated reference sets.
//!
//! The handwritten cases in [`crate::xml_doc_inheritdoc_diff`] each pin one
//! way Roslyn's declaration-ID lookup sees a reference set differently from
//! the env's documentation-ID index. This generates the space those cases
//! came from: up to three C# dependencies (two may share a name), each
//! declaring a few types drawn from shapes and namespaces that collide across
//! assemblies (a nested `Outer.Inner` beside a top-level `Inner` in
//! `F.Outer`; FSharp.Core's own namespace), carrying the attributes and
//! members Roslyn's importer treats specially (`[TypeIdentifier]`, a
//! `_VtblGap` method, a function-pointer property the projector drops, a
//! `get_P` method beside a property, overloads, a generic explicit
//! implementation, a parameter type several assemblies define), optionally
//! with FSharp.Core in the reference set. Every documentation ID the
//! dependencies' compiled `.xml` holds, and every property's accessor ID, is
//! then the `cref` of an `<inheritdoc>`, compared certain-implies-exact.
//!
//! A liveness check keeps the property from passing by declining everything,
//! and checks that every dimension is drawn. `BORZOI_INHERITDOC_CASES`
//! overrides the case count, as in the other generators.

use std::collections::BTreeSet;
use std::path::PathBuf;

use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
use tempfile::TempDir;

use crate::common::inheritdoc_diff::Census;
use crate::common::inheritdoc_oracle::oracle;
use crate::common::{ensure_fsharp_core_dll, ensure_system_runtime_dll};
use crate::xml_doc_inheritdoc_diff::{compare, fixture_from};

fn cases(default: u32) -> u32 {
    std::env::var("BORZOI_INHERITDOC_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A dependency's simple name: two dependencies may share one, which makes
/// two DLLs of one identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DepName {
    Alpha,
    Beta,
}

/// The namespace a type is declared in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Ns {
    F,
    /// FSharp.Core's, where its projection hides metadata types.
    FSharpCore,
}

impl Ns {
    fn name(self) -> &'static str {
        match self {
            Ns::F => "F",
            Ns::FSharpCore => "Microsoft.FSharp.Core",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    /// `class C`.
    Class,
    /// `class G<T>`.
    Generic,
    /// `class Outer { class Inner }`: the members are `Inner`'s.
    Nested,
    /// `class Inner` in namespace `<ns>.Outer`, which shares the nested
    /// shape's ID.
    TopInner,
    /// `class FSharpOption<T> { class Tags }`, FSharp.Core's union and the
    /// nested class its projection hides: the members are `Tags`'.
    OptionTags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Attr {
    None,
    /// `[TypeIdentifier]`: a No-PIA local type, which Roslyn's importer hides.
    TypeIdentifier,
    /// `[DefaultMember(null)]`, which the projector refuses: the type is
    /// dropped.
    DefaultMemberNull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Param {
    None,
    Int,
    Str,
    /// `F.X`, which every dependency using it defines.
    X,
    IntArray,
    /// `Microsoft.FSharp.Core.FSharpOption<int>.Tags`, which every dependency
    /// using it defines too: FSharp.Core has one its projection hides.
    OptionTags,
}

impl Param {
    fn decl(self) -> &'static str {
        match self {
            Param::None => "",
            Param::Int => "int x",
            Param::Str => "string x",
            Param::X => "F.X x",
            Param::IntArray => "int[] x",
            Param::OptionTags => "Microsoft.FSharp.Core.FSharpOption<int>.Tags x",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Member {
    Method(Param),
    /// `M(int)` and `M(string)`.
    Overloads,
    /// `_VtblGap1_1()`, which Roslyn's importer never imports.
    VtblGap,
    /// `int P { get; }`.
    Property,
    /// A function-pointer property, which the projector drops.
    FnPtrProperty,
    /// A method named `get_P`, beside no property of its own.
    GetP,
    /// An explicit implementation of `IG<int>.M(int)`.
    ExplicitGeneric,
}

#[derive(Debug, Clone)]
struct TypeSpec {
    ns: Ns,
    shape: Shape,
    attr: Attr,
    members: Vec<Member>,
}

#[derive(Debug, Clone)]
struct DepSpec {
    name: DepName,
    types: Vec<TypeSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FSharpCore {
    Absent,
    /// Right after System.Runtime, before every dependency.
    First,
    /// After every dependency.
    Last,
}

#[derive(Debug, Clone)]
struct World {
    deps: Vec<DepSpec>,
    fsharp_core: FSharpCore,
}

/// A dimension of the world, for the liveness check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Dim {
    DuplicateName,
    Ns(Ns),
    Shape(Shape),
    Attr(Attr),
    Member(Member),
    FSharpCore(FSharpCore),
}

impl World {
    fn dims(&self) -> BTreeSet<Dim> {
        let mut dims = BTreeSet::from([Dim::FSharpCore(self.fsharp_core)]);
        let names: Vec<DepName> = self.deps.iter().map(|d| d.name).collect();
        if names.iter().collect::<BTreeSet<_>>().len() < names.len() {
            dims.insert(Dim::DuplicateName);
        }
        for t in self.deps.iter().flat_map(|d| &d.types) {
            dims.insert(Dim::Ns(t.ns));
            dims.insert(Dim::Shape(t.shape));
            dims.insert(Dim::Attr(t.attr));
            dims.extend(t.members.iter().map(|m| Dim::Member(*m)));
        }
        dims
    }
}

fn param() -> impl Strategy<Value = Param> {
    prop_oneof![
        Just(Param::None),
        Just(Param::Int),
        Just(Param::Str),
        Just(Param::X),
        Just(Param::IntArray),
        Just(Param::OptionTags),
    ]
}

fn member() -> impl Strategy<Value = Member> {
    prop_oneof![
        4 => param().prop_map(Member::Method),
        1 => Just(Member::Overloads),
        1 => Just(Member::VtblGap),
        2 => Just(Member::Property),
        1 => Just(Member::FnPtrProperty),
        1 => Just(Member::GetP),
        1 => Just(Member::ExplicitGeneric),
    ]
}

fn type_spec() -> impl Strategy<Value = TypeSpec> {
    (
        prop_oneof![4 => Just(Ns::F), 1 => Just(Ns::FSharpCore)],
        prop_oneof![
            3 => Just(Shape::Class),
            1 => Just(Shape::Generic),
            1 => Just(Shape::Nested),
            1 => Just(Shape::TopInner),
            1 => Just(Shape::OptionTags),
        ],
        prop_oneof![
            6 => Just(Attr::None),
            1 => Just(Attr::TypeIdentifier),
            1 => Just(Attr::DefaultMemberNull),
        ],
        prop::collection::vec(member(), 1..4),
    )
        .prop_map(|(ns, shape, attr, members)| TypeSpec {
            ns,
            shape,
            attr,
            members,
        })
}

fn world() -> impl Strategy<Value = World> {
    (
        prop::collection::vec(
            (
                prop_oneof![Just(DepName::Alpha), Just(DepName::Beta)],
                prop::collection::vec(type_spec(), 1..3),
            )
                .prop_map(|(name, types)| DepSpec { name, types }),
            1..4,
        ),
        prop_oneof![
            3 => Just(FSharpCore::Absent),
            1 => Just(FSharpCore::First),
            1 => Just(FSharpCore::Last),
        ],
    )
        .prop_map(|(deps, fsharp_core)| World { deps, fsharp_core })
}

/// The members of one type, each documented with `tag` (which tells the
/// dependencies' copies apart).
fn members_source(members: &[Member], tag: &str, out: &mut String) {
    let doc = |out: &mut String, what: &str| {
        out.push_str(&format!("/// <summary>{tag} {what}.</summary>\n"));
    };
    let mut property = false;
    // A member drawn twice is one member, not a duplicate definition.
    let members: BTreeSet<Member> = members.iter().copied().collect();
    for m in &members {
        match m {
            Member::Method(p) => {
                doc(out, &format!("M({})", p.decl()));
                out.push_str(&format!("public void M({}) {{ }}\n", p.decl()));
            }
            Member::Overloads => {
                doc(out, "N(int)");
                out.push_str("public void N(int x) { }\n");
                doc(out, "N(string)");
                out.push_str("public void N(string x) { }\n");
            }
            Member::VtblGap => {
                doc(out, "_VtblGap1_1");
                out.push_str("public void _VtblGap1_1() { }\n");
            }
            Member::Property | Member::FnPtrProperty if property => {}
            Member::Property => {
                property = true;
                doc(out, "P");
                out.push_str("public int P => 0;\n");
            }
            Member::FnPtrProperty => {
                property = true;
                doc(out, "P");
                out.push_str("public unsafe delegate*<void> P => null;\n");
            }
            Member::GetP => {
                doc(out, "get_P");
                out.push_str("public int get_P() => 0;\n");
            }
            Member::ExplicitGeneric => {
                doc(out, "IG<int>.M");
                out.push_str("void F.IG<int>.M(int t) { }\n");
            }
        }
    }
}

/// A dependency's C# source; `index` tells same-named dependencies apart in
/// their documentation.
fn dep_source(dep: &DepSpec, index: usize) -> Option<String> {
    let tag = format!("{:?}{index}", dep.name);
    let mut out = String::new();
    let all: Vec<Member> = dep.types.iter().flat_map(|t| t.members.clone()).collect();
    if all.iter().any(|m| matches!(m, Member::Method(Param::X))) {
        out.push_str(&format!(
            "namespace F {{ /// <summary>{tag} X.</summary>\npublic class X {{ }} }}\n"
        ));
    }
    let declares_tags = dep
        .types
        .iter()
        .any(|t| t.ns == Ns::FSharpCore && t.shape == Shape::OptionTags);
    if !declares_tags && all.contains(&Member::Method(Param::OptionTags)) {
        out.push_str(&format!(
            "namespace Microsoft.FSharp.Core {{ /// <summary>{tag} FSharpOption.</summary>\n\
             public class FSharpOption<T> {{ /// <summary>{tag} Tags.</summary>\n\
             public class Tags {{ }} }} }}\n"
        ));
    }
    if all.contains(&Member::ExplicitGeneric) {
        out.push_str(&format!(
            "namespace F {{ /// <summary>{tag} IG.</summary>\npublic interface IG<T> {{\n/// <summary>{tag} IG.M.</summary>\nvoid M(T t); }} }}\n"
        ));
    }
    if dep.types.iter().any(|t| t.attr == Attr::TypeIdentifier) {
        // The reference pack's System.Runtime does not carry it; Roslyn
        // recognises the attribute by name, wherever it is defined.
        out.push_str(
            "namespace System.Runtime.InteropServices {\n\
             internal sealed class TypeIdentifierAttribute : System.Attribute {\n\
             public TypeIdentifierAttribute(string scope, string identifier) { } } }\n",
        );
    }
    let mut declared = BTreeSet::new();
    for t in &dep.types {
        // Two types of one shape in one namespace of one assembly would be
        // a duplicate definition.
        if !declared.insert((t.ns, t.shape)) {
            return None;
        }
        // Nor may one assembly have both a class and a namespace `Outer`.
        let other = match t.shape {
            Shape::Nested => Some(Shape::TopInner),
            Shape::TopInner => Some(Shape::Nested),
            _ => None,
        };
        if other.is_some_and(|o| declared.contains(&(t.ns, o))) {
            return None;
        }
        // A `get_P` beside a property of the same type reserves the name
        // twice (CS0082).
        let has_property = t
            .members
            .iter()
            .any(|m| matches!(m, Member::Property | Member::FnPtrProperty));
        if has_property && t.members.contains(&Member::GetP) {
            return None;
        }
        let attr = match t.attr {
            Attr::None => "",
            Attr::TypeIdentifier => {
                "[System.Runtime.InteropServices.TypeIdentifier(\"scope\", \"id\")]\n"
            }
            Attr::DefaultMemberNull => "[System.Reflection.DefaultMember(null)]\n",
        };
        let base = if t.members.contains(&Member::ExplicitGeneric) {
            " : F.IG<int>"
        } else {
            ""
        };
        let mut body = String::new();
        members_source(&t.members, &tag, &mut body);
        let ns = t.ns.name();
        let ty = |name: &str, body: &str| {
            format!(
                "/// <summary>{tag} {name}.</summary>\n{attr}public class {name}{base} {{\n{body}}}\n"
            )
        };
        let plain = |name: &str, body: &str| {
            format!("/// <summary>{tag} {name}.</summary>\npublic class {name} {{\n{body}}}\n")
        };
        let decl = match t.shape {
            Shape::Class => format!("namespace {ns} {{\n{}}}\n", ty("C", &body)),
            Shape::Generic => format!("namespace {ns} {{\n{}}}\n", ty("G<T>", &body)),
            Shape::Nested => format!(
                "namespace {ns} {{\n{}}}\n",
                plain("Outer", &ty("Inner", &body))
            ),
            Shape::TopInner => format!("namespace {ns}.Outer {{\n{}}}\n", ty("Inner", &body)),
            Shape::OptionTags => format!(
                "namespace {ns} {{\n{}}}\n",
                plain("FSharpOption<T>", &ty("Tags", &body))
            ),
        };
        out.push_str(&decl);
    }
    Some(out)
}

/// The documentation IDs `xml` documents, and each property's accessor ID.
fn harvest(xml: &str, ids: &mut BTreeSet<String>) {
    let doc = roxmltree::Document::parse(xml).expect("the compiler's doc file");
    for name in doc
        .descendants()
        .filter(|n| n.has_tag_name("member"))
        .filter_map(|n| n.attribute("name"))
    {
        ids.insert(name.to_string());
        if let Some(rest) = name.strip_prefix("P:")
            && let Some(dot) = rest.rfind('.')
        {
            ids.insert(format!("M:{}.get_{}", &rest[..dot], &rest[dot + 1..]));
        }
    }
}

/// Build `w`, expand every `cref`, and return the census; `None` when C#
/// rejects the world.
fn run(w: &World) -> Option<Census> {
    let dir = TempDir::new().unwrap();
    let runtime = ensure_system_runtime_dll();
    let mut built: Vec<PathBuf> = Vec::new();
    let mut ids = BTreeSet::new();
    for (i, dep) in w.deps.iter().enumerate() {
        let source = dep_source(dep, i)?;
        let dll = oracle()
            .lock()
            .unwrap()
            .compile(
                &source,
                &format!("{:?}", dep.name),
                &dir.path().join(format!("dep{i}")),
                std::slice::from_ref(&runtime),
            )
            .unwrap_or_else(|e| panic!("dependency {i} does not compile: {e:#?}\n{source}"));
        harvest(
            &std::fs::read_to_string(dll.with_extension("xml")).unwrap(),
            &mut ids,
        );
        built.push(dll);
    }
    let mut user = String::from("namespace User;\n/// <summary>U.</summary>\npublic class U {\n");
    for (i, id) in ids.iter().enumerate() {
        user.push_str(&format!(
            "/// <inheritdoc cref=\"{}\"/>\npublic void R{i}() {{ }}\n",
            id.replace('&', "&amp;").replace('"', "&quot;")
        ));
    }
    user.push_str("}\n");
    let fx = oracle()
        .lock()
        .unwrap()
        .compile(&user, "Fx", dir.path(), std::slice::from_ref(&runtime))
        .unwrap_or_else(|e| panic!("the user does not compile: {e:#?}\n{user}"));
    let mut references = vec![runtime];
    if w.fsharp_core == FSharpCore::First {
        references.push(ensure_fsharp_core_dll());
    }
    references.extend(built);
    if w.fsharp_core == FSharpCore::Last {
        references.push(ensure_fsharp_core_dll());
    }
    references.push(fx.clone());
    let fixture = fixture_from(dir, fx, references);
    let mut census = Census::default();
    census.add(&compare(&fixture));
    Some(census)
}

fn check(cases: u32) {
    let config = Config {
        cases,
        source_file: Some(file!()),
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    runner
        .run(&world(), |w| {
            if let Some(census) = run(&w) {
                if !census.disagrees.is_empty() {
                    census.print("generated crefs");
                }
                prop_assert!(
                    census.disagrees.is_empty(),
                    "{} expansion(s) disagree with Roslyn; first: {:?}",
                    census.disagrees.len(),
                    census.disagrees.first()
                );
            }
            Ok(())
        })
        .unwrap();
}

/// Every `cref` the generated reference sets offer expands exactly as
/// Roslyn's, or declines.
#[test]
fn generated_crefs_expand_exactly_as_roslyn() {
    check(cases(24));
}

/// [`generated_crefs_expand_exactly_as_roslyn`], deep.
#[test]
#[ignore = "hundreds of compiled reference sets; run with --ignored"]
fn deep_generated_crefs_expand_exactly_as_roslyn() {
    check(cases(400));
}

/// The generator reaches every dimension, and the expansion commits on a
/// fair share of what it draws.
#[test]
fn the_cref_generator_reaches_every_dimension() {
    use proptest::strategy::ValueTree;
    let mut runner = TestRunner::deterministic();
    let strategy = world();
    let mut drawn = BTreeSet::new();
    let mut total = Census::default();
    let mut built = 0;
    for _ in 0..40 {
        let w = strategy.new_tree(&mut runner).unwrap().current();
        if let Some(census) = run(&w) {
            built += 1;
            drawn.extend(w.dims());
            total.merge(census);
        }
    }
    total.print("generated crefs (liveness sample)");
    assert!(built >= 20, "only {built}/40 worlds compile");
    assert!(
        total.agrees >= 20,
        "only {} expansions commit",
        total.agrees
    );
    let mut every = vec![
        Dim::DuplicateName,
        Dim::Ns(Ns::F),
        Dim::Ns(Ns::FSharpCore),
        Dim::Attr(Attr::None),
        Dim::Attr(Attr::TypeIdentifier),
        Dim::Attr(Attr::DefaultMemberNull),
        Dim::FSharpCore(FSharpCore::Absent),
        Dim::FSharpCore(FSharpCore::First),
        Dim::FSharpCore(FSharpCore::Last),
        Dim::Member(Member::Overloads),
        Dim::Member(Member::VtblGap),
        Dim::Member(Member::Property),
        Dim::Member(Member::FnPtrProperty),
        Dim::Member(Member::GetP),
        Dim::Member(Member::ExplicitGeneric),
    ];
    every.extend(
        [
            Shape::Class,
            Shape::Generic,
            Shape::Nested,
            Shape::TopInner,
            Shape::OptionTags,
        ]
        .map(Dim::Shape),
    );
    every.extend(
        [
            Param::None,
            Param::Int,
            Param::Str,
            Param::X,
            Param::IntArray,
            Param::OptionTags,
        ]
        .map(|p| Dim::Member(Member::Method(p))),
    );
    let missing: Vec<&Dim> = every.iter().filter(|d| !drawn.contains(d)).collect();
    assert!(missing.is_empty(), "never drawn: {missing:?}");
}

/// Worlds the generator found, or that pin a dimension's subject, each
/// agreeing with Roslyn.
#[test]
fn the_pinned_worlds_agree_with_roslyn() {
    let one = |ns: Ns, shape: Shape, attr: Attr, members: Vec<Member>| DepSpec {
        name: DepName::Alpha,
        types: vec![TypeSpec {
            ns,
            shape,
            attr,
            members,
        }],
    };
    let worlds = [
        // A parameter type FSharp.Core also defines, hidden by its projection.
        World {
            deps: vec![one(
                Ns::F,
                Shape::Class,
                Attr::None,
                vec![Member::Method(Param::OptionTags)],
            )],
            fsharp_core: FSharpCore::First,
        },
        // A vtable gap, which Roslyn never imports.
        World {
            deps: vec![one(Ns::F, Shape::Class, Attr::None, vec![Member::VtblGap])],
            fsharp_core: FSharpCore::Absent,
        },
        // A No-PIA local type, which Roslyn's importer hides.
        World {
            deps: vec![one(
                Ns::F,
                Shape::Class,
                Attr::TypeIdentifier,
                vec![Member::Method(Param::Int)],
            )],
            fsharp_core: FSharpCore::Absent,
        },
    ];
    for w in worlds {
        let census = run(&w).expect("the world compiles");
        census.print(&format!("{w:?}"));
        census.assert_sound();
    }
}
