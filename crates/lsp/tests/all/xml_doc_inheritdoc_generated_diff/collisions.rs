//! Same-named types across assemblies, whose accessor and method IDs collide.
//!
//! A property's or event's accessor is a method with a documentation ID of its
//! own (`M:F.Mod.get_x`), and an ordinary method of the accessor's name has
//! the same ID. Two assemblies declaring one full name `F.Mod` — one with the
//! property, one with the method — make a `cref` naming that ID ambiguous, and
//! which symbol Roslyn's "first symbol" picks depends on reference order. This
//! draws the declaring type's shape (generic, nested in a generic or not,
//! generic itself inside a generic), and for each assembly, independently,
//! each member group — a property, an indexer, an event, a static property —
//! as absent, as the property or event, or as ordinary methods of its
//! accessors' names. Every ID of the vocabulary is named by a `cref`, the two
//! assemblies come in either order, and a derived class may override what the
//! assembly it was compiled against declares.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use proptest::prelude::*;

use super::{
    DEEP_CASES, DEFAULT_CASES, Reach, assert_every_dimension_expands, assert_sound, cases,
    check_each, reach_by, sample,
};
use crate::common::inheritdoc_diff::{Compared, Verdict};
use crate::xml_doc_inheritdoc_diff::{Dep, compare, fixture_with};

/// Where `Mod` is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    /// `F.Mod`.
    Plain,
    /// `` F.Mod`1 ``.
    Generic,
    /// `F.Outer.Mod`.
    NestedInPlain,
    /// `` F.Outer`1.Mod ``.
    NestedInGeneric,
    /// `` F.Outer`1.Mod`1 ``.
    GenericInGeneric,
}

impl Shape {
    /// The type's ID, without its `T:`.
    fn id(self) -> &'static str {
        match self {
            Shape::Plain => "F.Mod",
            Shape::Generic => "F.Mod`1",
            Shape::NestedInPlain => "F.Outer.Mod",
            Shape::NestedInGeneric => "F.Outer`1.Mod",
            Shape::GenericInGeneric => "F.Outer`1.Mod`1",
        }
    }

    /// The type as a derived class names it.
    fn constructed(self) -> &'static str {
        match self {
            Shape::Plain => "F.Mod",
            Shape::Generic => "F.Mod<int>",
            Shape::NestedInPlain => "F.Outer.Mod",
            Shape::NestedInGeneric => "F.Outer<int>.Mod",
            Shape::GenericInGeneric => "F.Outer<int>.Mod<int>",
        }
    }

    /// `body`, `Mod`'s members, wrapped in its declaration.
    fn declare(self, who: &str, body: &str) -> String {
        let mod_tp = if matches!(self, Shape::Generic | Shape::GenericInGeneric) {
            "<T>"
        } else {
            ""
        };
        let decl = format!(
            "/// <summary>{who}'s Mod.</summary>\npublic class Mod{mod_tp}\n{{\n{body}}}\n"
        );
        match self {
            Shape::Plain | Shape::Generic => decl,
            Shape::NestedInPlain | Shape::NestedInGeneric | Shape::GenericInGeneric => {
                let outer_tp = if self == Shape::NestedInPlain {
                    ""
                } else {
                    "<T0>"
                };
                format!(
                    "/// <summary>{who}'s Outer.</summary>\npublic class Outer{outer_tp}\n{{\n{decl}}}\n"
                )
            }
        }
    }
}

/// How one assembly declares a member group: not at all, as the property
/// (with these accessors) or event, or as ordinary methods of those
/// accessors' names. C# reserves both accessor names of a property, so a
/// type has one form or the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Absent,
    /// The property or event, with a getter (adder) and/or setter (remover).
    Accessors {
        first: bool,
        second: bool,
    },
    /// Ordinary methods named as those accessors would be.
    Methods {
        first: bool,
        second: bool,
    },
}

impl Form {
    fn is_accessors(self) -> bool {
        matches!(self, Form::Accessors { .. })
    }
}

fn form(event: bool) -> impl Strategy<Value = Form> {
    // An event has both accessors or none.
    let pair = move || {
        if event {
            Just((true, true)).boxed()
        } else {
            prop_oneof![Just((true, false)), Just((false, true)), Just((true, true)),].boxed()
        }
    };
    let methods = prop_oneof![Just((true, false)), Just((false, true)), Just((true, true)),];
    prop_oneof![
        1 => Just(Form::Absent),
        2 => pair().prop_map(|(first, second)| Form::Accessors { first, second }),
        2 => methods.prop_map(|(first, second)| Form::Methods { first, second }),
    ]
}

/// One member group: its accessor-method names, parameters and declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    /// `int x { get; set; }`.
    Property,
    /// `int this[int i] { get; set; }`.
    Indexer,
    /// `event System.EventHandler? E`.
    Event,
    /// `static int s { get; set; }`.
    Static,
}

const GROUPS: [Group; 4] = [Group::Property, Group::Indexer, Group::Event, Group::Static];

impl Group {
    /// The member's own ID suffix (after the type), for `P:`/`E:`.
    fn member_id(self) -> &'static str {
        match self {
            Group::Property => "P:{}.x",
            Group::Indexer => "P:{}.Item(System.Int32)",
            Group::Event => "E:{}.E",
            Group::Static => "P:{}.s",
        }
    }

    /// The two accessor IDs, `M:` and the type left as `{}`.
    fn accessor_ids(self) -> [&'static str; 2] {
        match self {
            Group::Property => ["M:{}.get_x", "M:{}.set_x(System.Int32)"],
            Group::Indexer => [
                "M:{}.get_Item(System.Int32)",
                "M:{}.set_Item(System.Int32,System.Int32)",
            ],
            Group::Event => [
                "M:{}.add_E(System.EventHandler)",
                "M:{}.remove_E(System.EventHandler)",
            ],
            Group::Static => ["M:{}.get_s", "M:{}.set_s(System.Int32)"],
        }
    }

    /// C# for the group in `form`, documented by `who`, with `modifier`
    /// (`virtual`, or `override` in a derived class).
    fn declare(self, form: Form, who: &str, modifier: &str, out: &mut String) {
        let doc = |out: &mut String, what: &str| {
            if !who.is_empty() {
                writeln!(out, "    /// <summary>{who}'s {what}.</summary>").unwrap();
            } else {
                writeln!(out, "    /// <inheritdoc/>").unwrap();
            }
        };
        let accessors = |first: bool, second: bool, get: &str, set: &str| match (first, second) {
            (true, false) => format!("{{ {get} }}"),
            (false, true) => format!("{{ {set} }}"),
            _ => format!("{{ {get} {set} }}"),
        };
        match (self, form) {
            (_, Form::Absent) => {}
            (Group::Property, Form::Accessors { first, second }) => {
                doc(out, "x");
                let a = accessors(first, second, "get => 0;", "set { }");
                writeln!(out, "    public {modifier} int x {a}").unwrap();
            }
            (Group::Indexer, Form::Accessors { first, second }) => {
                doc(out, "indexer");
                let a = accessors(first, second, "get => 0;", "set { }");
                writeln!(out, "    public {modifier} int this[int i] {a}").unwrap();
            }
            (Group::Event, Form::Accessors { .. }) => {
                doc(out, "E");
                writeln!(out, "    public {modifier} event System.EventHandler? E;").unwrap();
            }
            (Group::Static, Form::Accessors { first, second }) => {
                if modifier == "override" {
                    return;
                }
                doc(out, "s");
                let a = accessors(first, second, "get => 0;", "set { }");
                writeln!(out, "    public static int s {a}").unwrap();
            }
            (group, Form::Methods { first, second }) => {
                let (get, set) = match group {
                    Group::Property => ("int get_x()", "void set_x(int value)"),
                    Group::Indexer => ("int get_Item(int i)", "void set_Item(int i, int value)"),
                    Group::Event => (
                        "void add_E(System.EventHandler h)",
                        "void remove_E(System.EventHandler h)",
                    ),
                    Group::Static => ("int get_s()", "void set_s(int value)"),
                };
                let static_ = group == Group::Static;
                if static_ && modifier == "override" {
                    return;
                }
                let modifier = if static_ { "static" } else { modifier };
                for (on, sig) in [(first, get), (second, set)] {
                    if !on {
                        continue;
                    }
                    let name = sig.split(['(', ' ']).nth(1).unwrap();
                    doc(out, name);
                    let body = if sig.starts_with("int") {
                        "=> 0;"
                    } else {
                        "{ }"
                    };
                    writeln!(out, "    public {modifier} {sig} {body}").unwrap();
                }
            }
        }
    }
}

/// One assembly's `Mod`: each group's form.
#[derive(Debug, Clone)]
struct Side {
    forms: [Form; 4],
}

impl Side {
    fn body(&self, who: &str, modifier: &str) -> String {
        let mut out = String::new();
        for (g, f) in GROUPS.iter().zip(&self.forms) {
            g.declare(*f, who, modifier, &mut out);
        }
        out
    }
}

fn side() -> impl Strategy<Value = Side> {
    (form(false), form(false), form(true), form(false)).prop_map(|(p, i, e, s)| Side {
        forms: [p, i, e, s],
    })
}

/// Which assembly a derived class is compiled against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Derived {
    None,
    OfA,
    OfB,
}

#[derive(Debug, Clone)]
struct Case {
    shape: Shape,
    a: Side,
    b: Side,
    /// Whether `B` precedes `A` in the reference set.
    b_first: bool,
    derived: Derived,
}

fn strategy() -> impl Strategy<Value = Case> {
    (
        prop_oneof![
            Just(Shape::Plain),
            Just(Shape::Generic),
            Just(Shape::NestedInPlain),
            Just(Shape::NestedInGeneric),
            Just(Shape::GenericInGeneric),
        ],
        side(),
        side(),
        any::<bool>(),
        prop_oneof![
            1 => Just(Derived::None),
            1 => Just(Derived::OfA),
            1 => Just(Derived::OfB),
        ],
    )
        .prop_map(|(shape, a, b, b_first, derived)| Case {
            shape,
            a,
            b,
            b_first,
            derived,
        })
}

/// A dimension of the vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Dim {
    Shape(Shape),
    /// A `cref` naming a property's or event's own ID, of this group.
    MemberCref(Group),
    /// A `cref` naming an accessor-shaped method ID of this group, which no
    /// assembly declares as an accessor.
    MethodCref(Group),
    /// A `cref` naming an accessor-shaped method ID that some assembly
    /// declares as an accessor.
    AccessorCref(Group),
    /// A `cref` naming the type.
    TypeCref,
    /// An override in a class derived from one of the two `Mod`s.
    Override,
    /// The assembly declaring the accessor form comes first.
    AccessorsFirst,
    /// The assembly declaring the method form comes first.
    MethodsFirst,
}

fn all_dims() -> Vec<Dim> {
    let mut out: Vec<Dim> = [
        Shape::Plain,
        Shape::Generic,
        Shape::NestedInPlain,
        Shape::NestedInGeneric,
        Shape::GenericInGeneric,
    ]
    .into_iter()
    .map(Dim::Shape)
    .collect();
    for g in GROUPS {
        out.push(Dim::MemberCref(g));
        out.push(Dim::MethodCref(g));
    }
    out.extend([Dim::Override, Dim::AccessorsFirst, Dim::MethodsFirst]);
    out
}

impl Case {
    /// Each `cref`'s ID, with its dimensions.
    fn crefs(&self) -> Vec<(String, BTreeSet<Dim>)> {
        let ty = self.shape.id();
        let mut out = vec![(format!("T:{ty}"), BTreeSet::from([Dim::TypeCref]))];
        let (first, second) = if self.b_first {
            (&self.b, &self.a)
        } else {
            (&self.a, &self.b)
        };
        for (g, group) in GROUPS.iter().enumerate() {
            out.push((
                group.member_id().replace("{}", ty),
                BTreeSet::from([Dim::MemberCref(*group)]),
            ));
            let some_accessors = self.a.forms[g].is_accessors() || self.b.forms[g].is_accessors();
            let order = match (first.forms[g], second.forms[g]) {
                (Form::Accessors { .. }, Form::Methods { .. }) => Some(Dim::AccessorsFirst),
                (Form::Methods { .. }, Form::Accessors { .. }) => Some(Dim::MethodsFirst),
                _ => None,
            };
            for id in group.accessor_ids() {
                let mut d = BTreeSet::from([if some_accessors {
                    Dim::AccessorCref(*group)
                } else {
                    Dim::MethodCref(*group)
                }]);
                d.extend(order);
                out.push((id.replace("{}", ty), d));
            }
        }
        for (_, d) in &mut out {
            d.insert(Dim::Shape(self.shape));
        }
        out
    }

    fn fx_source(&self, crefs: &[(String, BTreeSet<Dim>)]) -> String {
        let mut out =
            String::from("namespace G;\n\n/// <summary>C.</summary>\npublic class C\n{\n");
        for (n, (id, _)) in crefs.iter().enumerate() {
            writeln!(
                out,
                "    /// <inheritdoc cref=\"{id}\"/>\n    public void R{n}() {{ }}\n"
            )
            .unwrap();
        }
        out.push_str("}\n");
        let side = match self.derived {
            Derived::None => None,
            Derived::OfA => Some(&self.a),
            Derived::OfB => Some(&self.b),
        };
        if let Some(side) = side {
            writeln!(
                out,
                "\n/// <inheritdoc/>\npublic class D : {}\n{{\n{}}}",
                self.shape.constructed(),
                side.body("", "override")
            )
            .unwrap();
        }
        out
    }
}

/// One case's comparisons, each with its dimensions.
fn run(c: &Case) -> Vec<(Compared, BTreeSet<Dim>)> {
    let crefs = c.crefs();
    let source = c.fx_source(&crefs);
    let a = format!(
        "namespace F;\n{}",
        c.shape.declare("A", &c.a.body("A", "virtual"))
    );
    let b = format!(
        "namespace F;\n{}",
        c.shape.declare("B", &c.b.body("B", "virtual"))
    );
    let dep = |name, source, visible| Dep {
        name,
        source,
        alias: None,
        referenced: true,
        against: &[],
        visible,
        patch: None,
    };
    let (visible_a, visible_b) = (c.derived == Derived::OfA, c.derived == Derived::OfB);
    let deps = if c.b_first {
        [dep("DepB", &b, visible_b), dep("DepA", &a, visible_a)]
    } else {
        [dep("DepA", &a, visible_a), dep("DepB", &b, visible_b)]
    };
    let program = format!(
        "{source}\n--- DepA ---\n{a}\n--- DepB ---\n{b}\nB first: {}",
        c.b_first
    );
    let fx = fixture_with(&source, &deps)
        .unwrap_or_else(|e| panic!("the generated program does not compile: {e:#?}\n{program}"));
    let compared = compare(&fx);
    assert_sound(&compared, &program);
    let by_method: BTreeMap<String, &BTreeSet<Dim>> = crefs
        .iter()
        .enumerate()
        .map(|(n, (_, d))| (format!("M:G.C.R{n}"), d))
        .collect();
    compared
        .into_iter()
        .map(|cmp| {
            let dims = match by_method.get(&cmp.key) {
                Some(d) => (*d).clone(),
                None if cmp.key.starts_with("T:G.D") || cmp.key.contains("G.D.") => {
                    BTreeSet::from([Dim::Override, Dim::Shape(c.shape)])
                }
                None => BTreeSet::new(),
            };
            (cmp, dims)
        })
        .collect()
}

#[test]
fn generated_accessor_id_collisions_expand_exactly_as_roslyn() {
    check_each(strategy(), cases(DEFAULT_CASES), |c| {
        run(&c);
    });
}

#[test]
#[ignore = "deep run: CI's <inheritdoc> sweep step"]
fn deep_generated_accessor_id_collisions_expand_exactly_as_roslyn() {
    check_each(strategy(), cases(DEEP_CASES), |c| {
        run(&c);
    });
}

/// Every dimension reaches an expanded entry, and the accessor guard fires
/// for every group: a `cref` naming an ID some assembly declares as an
/// accessor declines as `Cref(MaybeAccessor)` in some sampled case.
#[test]
fn the_collision_generator_reaches_every_dimension() {
    let mut reach: BTreeMap<Dim, Reach> = BTreeMap::new();
    let mut guarded: BTreeSet<Group> = BTreeSet::new();
    for case in sample(&strategy(), 40) {
        for (c, dims) in run(&case) {
            reach_by(&mut reach, dims.iter().copied(), &c.verdict);
            if matches!(&c.verdict, Verdict::Declined { cause, .. } if cause == "Cref(MaybeAccessor)")
            {
                guarded.extend(dims.iter().filter_map(|d| match d {
                    Dim::AccessorCref(g) => Some(*g),
                    _ => None,
                }));
            }
        }
    }
    assert_every_dimension_expands("accessor-ID collisions", &all_dims(), &reach);
    let unguarded: Vec<Group> = GROUPS
        .into_iter()
        .filter(|g| !guarded.contains(g))
        .collect();
    assert!(
        unguarded.is_empty(),
        "accessor-ID collisions: the accessor guard never fired for {unguarded:?}"
    );
}
