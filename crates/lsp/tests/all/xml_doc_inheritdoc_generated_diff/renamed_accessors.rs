//! Accessors named off the `get_`/`set_` convention, through an interface in
//! another assembly.
//!
//! A property's or event's accessors are tied to it by `MethodSemantics`, not
//! by name, and an explicit implementation of an external interface's
//! property is a `MethodImpl` row naming the interface's accessor *method*.
//! So which interface member an implementation implements is read off the
//! interface's accessor ownership, never off an accessor's name. This draws
//! an interface — two properties, an indexer, an event, an ordinary method,
//! each present or not, with these accessors or those — and renames its
//! methods by swapping `MethodDef` names (with each other's, and with a
//! neighbouring class's), which no compiler writes. The implementing class
//! implements each member explicitly, implicitly, or explicitly beside a
//! public member of the same shape. It is compiled against the interface it
//! is loaded beside, or against the other one — the original while the
//! renamed one is loaded, or the reverse — so that its `MethodImpl` rows
//! name methods by names they no longer have.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use proptest::prelude::*;

use super::{
    DEEP_CASES, DEFAULT_CASES, Reach, assert_every_dimension_expands, assert_sound, cases,
    check_each, oracle_guard, reach_by, sample,
};
use crate::common::inheritdoc_diff::{Compared, Verdict};
use crate::xml_doc_inheritdoc_diff::{Dep, compare, fixture_with};

/// A member of the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Member {
    P,
    Q,
    Indexer,
    Event,
    /// The ordinary method `Mi`.
    Method,
}

const MEMBERS: [Member; 5] = [
    Member::P,
    Member::Q,
    Member::Indexer,
    Member::Event,
    Member::Method,
];

/// Which accessors a property or indexer has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Accessors {
    Get,
    Set,
    Both,
}

impl Accessors {
    fn get(self) -> bool {
        self != Accessors::Set
    }

    fn set(self) -> bool {
        self != Accessors::Get
    }

    /// Accessor bodies of an implementation.
    fn bodies(self) -> &'static str {
        match self {
            Accessors::Get => "{ get => 0; }",
            Accessors::Set => "{ set { } }",
            Accessors::Both => "{ get => 0; set { } }",
        }
    }

    /// Accessor declarations of the interface's member.
    fn declarations(self) -> &'static str {
        match self {
            Accessors::Get => "{ get; }",
            Accessors::Set => "{ set; }",
            Accessors::Both => "{ get; set; }",
        }
    }
}

/// How the class implements one member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Explicit,
    Implicit,
    /// Explicitly, beside a public member of the same name and shape, which
    /// the explicit implementation outranks.
    ExplicitBesidePublic,
}

/// How an implementing member is documented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Doc {
    Bare,
    Full,
    None,
}

fn doc() -> impl Strategy<Value = Doc> {
    prop_oneof![4 => Just(Doc::Bare), 1 => Just(Doc::Full), 1 => Just(Doc::None)]
}

/// One interface member and its implementation.
#[derive(Debug, Clone, Copy)]
struct Slot {
    accessors: Accessors,
    mode: Mode,
    /// The explicit implementation's documentation, and the public one's.
    docs: (Doc, Doc),
}

#[derive(Debug, Clone)]
struct Case {
    /// Per member of [`MEMBERS`], whether the interface declares it and how
    /// it is implemented. An event always has both accessors.
    slots: [Option<Slot>; 5],
    /// Name swaps, applied in turn: indices into [`Case::names`] (modulo
    /// their number), each naming the method that currently has that name.
    swaps: Vec<(usize, usize)>,
    /// Which interface the class is compiled against, and which it is
    /// loaded beside.
    order: Order,
}

/// Which interface the class is compiled against and which it is loaded
/// beside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Order {
    /// Compiled against the renamed interface, loaded beside it.
    Renamed,
    /// Compiled against the original, loaded beside the renamed one: its
    /// rows name the old names.
    OriginalThenRenamed,
    /// Compiled against the renamed interface, loaded beside the original:
    /// its rows name the new names.
    RenamedThenOriginal,
}

impl Order {
    /// Whether the class's source sees the renamed interface.
    fn compiled_renamed(self) -> bool {
        self != Order::OriginalThenRenamed
    }
}

/// The neighbouring class's methods, whose names a swap may give the
/// interface's.
const OTHERS: [&str; 3] = ["Read", "Write", "get_R"];

impl Case {
    /// Every method of the interface's assembly, by original name: the
    /// interface's accessors and method, then the neighbour's.
    fn names(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (m, slot) in MEMBERS.iter().zip(&self.slots) {
            let Some(slot) = slot else { continue };
            let (get, set) = (slot.accessors.get(), slot.accessors.set());
            match m {
                Member::P | Member::Q => {
                    let n = if *m == Member::P { "P" } else { "Q" };
                    if get {
                        out.push(format!("get_{n}"));
                    }
                    if set {
                        out.push(format!("set_{n}"));
                    }
                }
                Member::Indexer => {
                    if get {
                        out.push("get_Item".into());
                    }
                    if set {
                        out.push("set_Item".into());
                    }
                }
                Member::Event => out.extend(["add_E".into(), "remove_E".into()]),
                Member::Method => out.push("Mi".into()),
            }
        }
        out.extend(OTHERS.iter().map(|s| s.to_string()));
        out
    }

    /// The name swaps by the names they exchange at the time, and each
    /// original name's final one.
    fn renaming(&self) -> (Vec<(String, String)>, BTreeMap<String, String>) {
        let names = self.names();
        // `current[r]` is the name of the row first called `names[r]`.
        let mut current = names.clone();
        let mut swaps = Vec::new();
        for &(a, b) in &self.swaps {
            let (a, b) = (a % names.len(), b % names.len());
            if a == b {
                continue;
            }
            let (x, y) = (names[a].clone(), names[b].clone());
            let rx = current.iter().position(|n| *n == x).unwrap();
            let ry = current.iter().position(|n| *n == y).unwrap();
            current.swap(rx, ry);
            swaps.push((x, y));
        }
        (swaps, names.into_iter().zip(current).collect())
    }

    fn interface_source(&self) -> String {
        let mut out =
            String::from("namespace N {\n/// <summary>I.</summary>\npublic interface I\n{\n");
        for (m, slot) in MEMBERS.iter().zip(&self.slots) {
            let Some(slot) = slot else { continue };
            let a = slot.accessors.declarations();
            let decl = match m {
                Member::P => format!("int P {a}"),
                Member::Q => format!("int Q {a}"),
                Member::Indexer => format!("int this[int i] {a}"),
                Member::Event => "event System.EventHandler E;".into(),
                Member::Method => "int Mi();".into(),
            };
            writeln!(out, "    /// <summary>I's {m:?}.</summary>\n    {decl}").unwrap();
        }
        out.push_str(
            "}\n\n/// <summary>Other.</summary>\npublic class Other\n{\n    \
             /// <summary>Other.Read.</summary>\n    public int Read() => 0;\n    \
             /// <summary>Other.Write.</summary>\n    public void Write(int value) { }\n    \
             /// <summary>Other.get_R.</summary>\n    public int get_R() => 0;\n}\n}\n",
        );
        out
    }

    /// The implementing class, and the dimensions of each of its entries by
    /// documentation ID.
    fn class_source(
        &self,
        final_names: &BTreeMap<String, String>,
    ) -> (String, BTreeMap<String, BTreeSet<Dim>>) {
        let mut out = String::from(
            "namespace F;\n\n/// <summary>The implementer.</summary>\npublic class C : N.I\n{\n",
        );
        let mut keys: BTreeMap<String, BTreeSet<Dim>> = BTreeMap::new();
        let order = Dim::Order(self.order);
        for (m, slot) in MEMBERS.iter().zip(&self.slots) {
            let Some(slot) = slot else { continue };
            // The method names this member's accessors (or itself) had, and
            // whether any was renamed, within the interface or out of it.
            let own: Vec<String> = self
                .names()
                .into_iter()
                .filter(|n| owner(n) == Some(*m))
                .collect();
            let mut renamed = BTreeSet::new();
            for n in &own {
                let to = &final_names[n];
                if to != n {
                    renamed.insert(Dim::Renamed);
                    renamed.insert(if OTHERS.contains(&to.as_str()) {
                        Dim::RenamedOutOfInterface
                    } else {
                        Dim::RenamedWithinInterface
                    });
                }
            }
            // The ordinary method is implemented by the name it has where
            // the class is compiled; a name an accessor of the class might
            // take is implemented explicitly, with no public method beside.
            let method_name = match final_names.get("Mi") {
                Some(renamed) if self.order.compiled_renamed() => renamed.clone(),
                _ => "Mi".to_string(),
            };
            let accessor_like = ["get_", "set_", "add_", "remove_"]
                .iter()
                .any(|p| method_name.starts_with(p));
            let mode = match (m, slot.mode) {
                (Member::Method, Mode::Implicit | Mode::ExplicitBesidePublic) if accessor_like => {
                    Mode::Explicit
                }
                (_, mode) => mode,
            };
            let b = slot.accessors.bodies();
            let (explicit, public, explicit_key, public_key) = match m {
                Member::P | Member::Q => {
                    let n = if *m == Member::P { "P" } else { "Q" };
                    (
                        format!("int N.I.{n} {b}"),
                        format!("public int {n} {b}"),
                        format!("P:F.C.N#I#{n}"),
                        format!("P:F.C.{n}"),
                    )
                }
                Member::Indexer => (
                    format!("int N.I.this[int i] {b}"),
                    format!("public int this[int i] {b}"),
                    "P:F.C.N#I#Item(System.Int32)".into(),
                    "P:F.C.Item(System.Int32)".into(),
                ),
                Member::Event => (
                    "event System.EventHandler N.I.E { add { } remove { } }".into(),
                    "public event System.EventHandler E { add { } remove { } }".into(),
                    "E:F.C.N#I#E".into(),
                    "E:F.C.E".into(),
                ),
                Member::Method => (
                    format!("int N.I.{method_name}() => 0;"),
                    format!("public int {method_name}() => 0;"),
                    format!("M:F.C.N#I#{method_name}"),
                    format!("M:F.C.{method_name}"),
                ),
            };
            let write = |out: &mut String, d: Doc, what: &str, decl: &str| {
                match d {
                    Doc::Bare => writeln!(out, "    /// <inheritdoc/>").unwrap(),
                    Doc::Full => writeln!(out, "    /// <summary>C's {what}.</summary>").unwrap(),
                    Doc::None => {}
                }
                writeln!(out, "    {decl}\n").unwrap();
            };
            let mut dims = |key: String, d: Dim| {
                let slot = keys.entry(key).or_default();
                slot.extend([d, Dim::Member(*m), order]);
                slot.extend(renamed.iter().copied());
                if !renamed.is_empty() {
                    slot.insert(match d {
                        Dim::Explicit => Dim::RenamedExplicit,
                        Dim::Implicit => Dim::RenamedImplicit,
                        _ => Dim::RenamedBeside,
                    });
                }
            };
            match mode {
                Mode::Explicit => {
                    write(&mut out, slot.docs.0, "explicit", &explicit);
                    dims(explicit_key, Dim::Explicit);
                }
                Mode::Implicit => {
                    write(&mut out, slot.docs.1, "implicit", &public);
                    dims(public_key, Dim::Implicit);
                }
                Mode::ExplicitBesidePublic => {
                    write(&mut out, slot.docs.0, "explicit", &explicit);
                    write(&mut out, slot.docs.1, "public", &public);
                    dims(explicit_key, Dim::ExplicitBesidePublic);
                    dims(public_key, Dim::PublicBesideExplicit);
                }
            }
        }
        out.push_str("}\n");
        (out, keys)
    }
}

/// The interface member an original method name belongs to.
fn owner(name: &str) -> Option<Member> {
    match name {
        "get_P" | "set_P" => Some(Member::P),
        "get_Q" | "set_Q" => Some(Member::Q),
        "get_Item" | "set_Item" => Some(Member::Indexer),
        "add_E" | "remove_E" => Some(Member::Event),
        "Mi" => Some(Member::Method),
        _ => None,
    }
}

fn accessors() -> impl Strategy<Value = Accessors> {
    prop_oneof![
        Just(Accessors::Get),
        Just(Accessors::Set),
        Just(Accessors::Both)
    ]
}

fn mode() -> impl Strategy<Value = Mode> {
    prop_oneof![
        Just(Mode::Explicit),
        Just(Mode::Implicit),
        Just(Mode::ExplicitBesidePublic)
    ]
}

fn slot(event: bool) -> impl Strategy<Value = Option<Slot>> {
    let accessors = if event {
        Just(Accessors::Both).boxed()
    } else {
        accessors().boxed()
    };
    prop::option::weighted(0.7, (accessors, mode(), (doc(), doc()))).prop_map(|s| {
        s.map(|(accessors, mode, docs)| Slot {
            accessors,
            mode,
            docs,
        })
    })
}

fn strategy() -> impl Strategy<Value = Case> {
    (
        (
            slot(false),
            slot(false),
            slot(false),
            slot(true),
            slot(false),
        ),
        prop::collection::vec((0..16usize, 0..16usize), 0..4),
        prop_oneof![
            Just(Order::Renamed),
            Just(Order::OriginalThenRenamed),
            Just(Order::RenamedThenOriginal),
        ],
    )
        .prop_map(|((p, q, i, e, m), swaps, order)| Case {
            slots: [p, q, i, e, m],
            swaps,
            order,
        })
}

/// A dimension of the vocabulary, attached to the implementing entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Dim {
    Member(Member),
    Explicit,
    Implicit,
    ExplicitBesidePublic,
    PublicBesideExplicit,
    /// Some method of the implemented member was renamed…
    Renamed,
    /// …to another of the interface's methods' names…
    RenamedWithinInterface,
    /// …or to the neighbouring class's.
    RenamedOutOfInterface,
    /// A renamed member implemented explicitly, implicitly, or explicitly
    /// beside a public member.
    RenamedExplicit,
    RenamedImplicit,
    RenamedBeside,
    Order(Order),
}

/// The dimensions some sampled entry must expand under. The public member
/// beside an explicit implementation is not one: it implements nothing, so
/// its `<inheritdoc/>` has no candidate.
fn expanding() -> Vec<Dim> {
    let mut out: Vec<Dim> = MEMBERS.into_iter().map(Dim::Member).collect();
    out.extend([
        Dim::Explicit,
        Dim::Implicit,
        Dim::ExplicitBesidePublic,
        Dim::Renamed,
        Dim::RenamedWithinInterface,
        Dim::RenamedOutOfInterface,
        Dim::RenamedExplicit,
        Dim::RenamedImplicit,
        Dim::RenamedBeside,
        Dim::Order(Order::Renamed),
        Dim::Order(Order::OriginalThenRenamed),
        Dim::Order(Order::RenamedThenOriginal),
    ]);
    out
}

fn run(c: &Case) -> Vec<(Compared, BTreeSet<Dim>)> {
    let (swaps, final_names) = c.renaming();
    let interface = c.interface_source();
    let (source, keys) = c.class_source(&final_names);
    let rename = |dll: &std::path::Path| {
        for (a, b) in &swaps {
            oracle_guard().swap_method_names(dll, a, b);
        }
    };
    let dep = |renamed: bool, compiled: bool, loaded: bool| Dep {
        name: "Ifc",
        source: &interface,
        alias: None,
        referenced: loaded,
        against: &[],
        visible: compiled,
        patch: if renamed { Some(&rename) } else { None },
    };
    let deps: Vec<Dep<'_>> = match c.order {
        Order::Renamed => vec![dep(true, true, true)],
        Order::OriginalThenRenamed => vec![dep(false, true, false), dep(true, false, true)],
        Order::RenamedThenOriginal => vec![dep(true, true, false), dep(false, false, true)],
    };
    let program = format!("{source}\n--- Ifc ---\n{interface}\nswaps: {swaps:?}");
    let fx = fixture_with(&source, &deps)
        .unwrap_or_else(|e| panic!("the generated program does not compile: {e:#?}\n{program}"));
    let compared = compare(&fx);
    assert_sound(&compared, &program);
    compared
        .into_iter()
        .map(|cmp| {
            let dims = keys.get(&cmp.key).cloned().unwrap_or_default();
            (cmp, dims)
        })
        .collect()
}

#[test]
fn generated_renamed_accessors_expand_exactly_as_roslyn() {
    check_each(strategy(), cases(DEFAULT_CASES), |c| {
        run(&c);
    });
}

#[test]
#[ignore = "deep run: CI's <inheritdoc> sweep step"]
fn deep_generated_renamed_accessors_expand_exactly_as_roslyn() {
    check_each(strategy(), cases(DEEP_CASES), |c| {
        run(&c);
    });
}

/// Every dimension but one reaches an expanded entry; the public member
/// beside an explicit implementation reaches a decline where Roslyn, too,
/// leaves its `<inheritdoc/>` — the answer an implementation read off an
/// accessor's name gets wrong.
#[test]
fn the_renamed_accessor_generator_reaches_every_dimension() {
    let mut reach: BTreeMap<Dim, Reach> = BTreeMap::new();
    let mut outranked = 0;
    for case in sample(&strategy(), 96) {
        for (c, dims) in run(&case) {
            reach_by(&mut reach, dims.iter().copied(), &c.verdict);
            if dims.contains(&Dim::PublicBesideExplicit)
                && matches!(
                    c.verdict,
                    Verdict::Declined {
                        roslyn_left_it: true,
                        ..
                    }
                )
            {
                outranked += 1;
            }
        }
    }
    assert_every_dimension_expands("renamed accessors", &expanding(), &reach);
    assert!(
        outranked > 0,
        "no public member beside an explicit implementation reached a decline Roslyn agrees with"
    );
}
