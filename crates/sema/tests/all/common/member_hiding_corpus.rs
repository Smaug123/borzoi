//! The **member-hiding corpus** generator: an adversarial universe for the
//! data-member wake ([`AssemblyEnv::instance_data_member`]).
//!
//! `instance_data_member` / `data_member_at_level` encode a dozen claims about
//! how C# name hiding reaches an F# member access — a derived declaration hides
//! an inherited one *whatever kind it is*, a `static` of the name hides an
//! inherited instance member, a **non-public** derived member does not hide at
//! all across an assembly boundary, an interface receiver walks the DAG and
//! declines when two inherited levels declare the name, a private-getter
//! property is unreadable however public the property looks. Each claim is about
//! the *language*, so only FCS can settle it; each is otherwise carried by prose
//! and a handful of `System.String` examples; and each, if wrong, makes the LSP
//! serve a member resolution pointing at the wrong declaration.
//!
//! So this emits *two views of one universe*, in the [`overload_corpus`] mould:
//!
//! * a **C# assembly** (`HideCorpus`) whose grid pairs what a base declares
//!   under the probed name `P` with what its derived class declares under the
//!   same name — every [`Decl`] shape against every base shape — plus the
//!   families a two-level grid cannot express: interface receivers (own /
//!   inherited-from-one / inherited-from-two / grandparent), a closed generic
//!   base, and three-level chains that pin *which* level wins;
//! * an **F# access matrix**: one `let` binding per cell, reading `recv.P` off a
//!   receiver the engine grounds from a static factory.
//!
//! The consumer (`super::super::member_hiding_diff`) references the assembly
//! from both FCS and our [`AssemblyEnv`] and asserts, per cell: **we commit ⇒
//! FCS bound a member of the same name on the same declaring type**. Deferring
//! is always allowed (availability loss); a wrong declaring level is a failure.
//!
//! Three constraints shape the rendering, all load-bearing:
//!
//! * **The base and derived member types differ** (`int` at the base, `string`
//!   at the derived), so a wrong level is visible to the `types` oracle as well
//!   as to the declaring-entity comparison — two independent witnesses of one
//!   mistake.
//! * **No `open`, no attribute, no augmentation.** Every type is reached fully
//!   qualified, so nothing in the file can be an extension source.
//! * **Receivers come from static factories** (`HideCorpus.Make.New<n>()`), a
//!   single-candidate static call the engine commits, because it has no
//!   object-construction path. An interface-typed factory returns `null`: the
//!   corpus is compiled and type-checked, never run, and an implementing class
//!   would itself declare members and change what the DAG walk sees.
//!
//! [`AssemblyEnv::instance_data_member`]: borzoi_sema::AssemblyEnv::instance_data_member
//! [`overload_corpus`]: super::overload_corpus

#![allow(dead_code)] // the differential and the coverage report use different subsets.

/// The assembly's namespace, and the prefix every F# access is qualified by.
pub const NS: &str = "HideCorpus";

/// The member name the corpus declares at every level and every cell reads,
/// except where a family says otherwise ([`Site::probe`]).
pub const PROBE: &str = "P";

/// The BCL member the cross-assembly family probes: a settable `string` property
/// on `System.Exception`, so an inheriting corpus type reaches a data member
/// whose declaration is in *another* assembly — the base-chain step the grid
/// (whose every level is in one file) never takes.
const BCL_PROBE: &str = "HelpLink";

/// What one inheritance level declares under the probed name `P`.
///
/// Every variant is a shape `data_member_at_level` reasons about explicitly: the
/// readable data members it resolves, the members that *hide* an inherited one
/// without being resolvable themselves (a method, an event, a static, a
/// write-only or private-getter property), and the non-public ones that — being
/// invisible across an assembly boundary — must not hide at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decl {
    /// The level is silent about `P`.
    None,
    /// `public T P { get; set; }` — the resolvable shape.
    Prop,
    /// `public T P;` — the other resolvable shape.
    Field,
    /// `public T P { set { } }` — hides, but a read has no getter.
    WriteOnlyProp,
    /// `public T P { private get; set; }` — property-level access says `public`
    /// while the *getter* is private, so a cross-assembly read is impossible.
    PrivateGetProp,
    /// `public static T P { get; set; }` — unreachable through a value receiver,
    /// yet it still hides.
    StaticProp,
    /// `public static T P;`
    StaticField,
    /// `public T P() => …` — a method, not a data member.
    Method,
    /// Two overloads of `P` — two public members of one name at one level.
    MethodGroup,
    /// `public event System.Action P;`
    Event,
    /// `internal T P { get; set; }` — invisible across the assembly boundary.
    InternalProp,
    /// `private T P { get; set; }`
    PrivateProp,
    /// `protected T P { get; set; }`
    ProtectedProp,
    /// `public class P { }` — a nested *type* of the name, which is not a member
    /// at all.
    NestedType,
}

impl Decl {
    /// A short tag naming the shape in a case label / failure message.
    pub fn tag(self) -> &'static str {
        match self {
            Decl::None => "none",
            Decl::Prop => "prop",
            Decl::Field => "field",
            Decl::WriteOnlyProp => "writeonly",
            Decl::PrivateGetProp => "privateget",
            Decl::StaticProp => "staticprop",
            Decl::StaticField => "staticfield",
            Decl::Method => "method",
            Decl::MethodGroup => "methodgroup",
            Decl::Event => "event",
            Decl::InternalProp => "internalprop",
            Decl::PrivateProp => "privateprop",
            Decl::ProtectedProp => "protectedprop",
            Decl::NestedType => "nestedtype",
        }
    }

    /// The C# declarations this shape contributes, one per line, at member type
    /// `ty`. `hides` adds the `new` modifier — required whenever the level above
    /// declares `P`, whatever kind either side is.
    fn csharp(self, ty: &str, hides: bool) -> Vec<String> {
        let n = if hides { "new " } else { "" };
        match self {
            Decl::None => vec![],
            Decl::Prop => vec![format!("public {n}{ty} P {{ get; set; }}")],
            Decl::Field => vec![format!("public {n}{ty} P;")],
            Decl::WriteOnlyProp => vec![format!("public {n}{ty} P {{ set {{ }} }}")],
            Decl::PrivateGetProp => vec![format!("public {n}{ty} P {{ private get; set; }}")],
            Decl::StaticProp => vec![format!("public static {n}{ty} P {{ get; set; }}")],
            Decl::StaticField => vec![format!("public static {n}{ty} P;")],
            Decl::Method => vec![format!("public {n}{ty} P() => default({ty});")],
            Decl::MethodGroup => vec![
                format!("public {n}{ty} P() => default({ty});"),
                format!("public {ty} P(int x) => default({ty});"),
            ],
            Decl::Event => vec![format!("public {n}event System.Action P;")],
            Decl::InternalProp => vec![format!("internal {n}{ty} P {{ get; set; }}")],
            Decl::PrivateProp => vec![format!("private {n}{ty} P {{ get; set; }}")],
            Decl::ProtectedProp => vec![format!("protected {n}{ty} P {{ get; set; }}")],
            Decl::NestedType => vec![format!("public {n}class P {{ }}")],
        }
    }
}

/// The base shapes the grid sweeps: the control (silent), one resolvable data
/// member of each kind, a static, and the two public non-data kinds.
const BASE_SHAPES: [Decl; 7] = [
    Decl::None,
    Decl::Prop,
    Decl::Field,
    Decl::StaticProp,
    Decl::Method,
    Decl::MethodGroup,
    Decl::Event,
];

/// The derived shapes the grid sweeps: every [`Decl`].
const DERIVED_SHAPES: [Decl; 14] = [
    Decl::None,
    Decl::Prop,
    Decl::Field,
    Decl::WriteOnlyProp,
    Decl::PrivateGetProp,
    Decl::StaticProp,
    Decl::StaticField,
    Decl::Method,
    Decl::MethodGroup,
    Decl::Event,
    Decl::InternalProp,
    Decl::PrivateProp,
    Decl::ProtectedProp,
    Decl::NestedType,
];

/// The member type declared at each level. Distinct on purpose: the level that
/// answered is then visible in the *type* as well as in the declaring entity.
const BASE_TY: &str = "int";
const DERIVED_TY: &str = "string";

/// The interfaces the DAG family declares, and what each contributes.
const INTERFACES: [(&str, &str, &[&str]); 6] = [
    ("IA", "", &["int P { get; }"]),
    ("IB", "", &["string P { get; }"]),
    ("IOwn", " : IA", &["new string P { get; }"]),
    ("IOne", " : IA", &[]),
    ("ITwo", " : IA, IB", &[]),
    ("IDeep", " : IOne", &[]),
];

/// One access site: a single `let r<n> = v<n>.<probe>` in the generated F#.
#[derive(Debug, Clone)]
pub struct Site {
    /// 1-based line of the access — every site sits on its own line, so the
    /// differential keys our answer, FCS's, and the case label by line alone.
    pub line: usize,
    /// Human-readable cell identity, printed verbatim on failure.
    pub label: String,
    /// The receiver's static type — the class, struct or interface the access is
    /// on.
    pub receiver_ty: String,
    /// The `HideCorpus.Make` factory that produces the receiver, so another
    /// harness can build its own F# matrix over the same universe.
    pub factory: String,
    /// The member name this cell reads. [`PROBE`] for everything the corpus
    /// declares itself; the cross-assembly family probes a **BCL** member name
    /// instead, since its point is a base chain that leaves this assembly.
    pub probe: String,
    /// Half-open byte range of the **access expression** (`v<n>.<probe>`) in the
    /// generated F#. The `types` oracle is keyed by range, so this is where a
    /// consumer asks FCS what the access is typed as.
    pub access: (usize, usize),
    /// The line's source text.
    pub text: String,
}

/// The generated universe: one C# source, one F# source, and the site index.
#[derive(Debug)]
pub struct Corpus {
    pub csharp: String,
    pub fsharp: String,
    pub sites: Vec<Site>,
}

/// A C# type to emit: its declaration header (through the base list) and body.
struct Emitted {
    header: String,
    body: Vec<String>,
}

/// One access cell: the receiver's type, the factory that produces it, the
/// member it reads, and the label the differential prints.
struct Cell {
    receiver_ty: String,
    factory: String,
    probe: String,
    label: String,
}

impl Cell {
    /// A cell reading the corpus's own [`PROBE`] off a receiver its own factory
    /// produces — the shape every family but the cross-assembly one uses.
    fn probing_p(receiver_ty: &str, label: &str) -> Cell {
        Cell {
            receiver_ty: receiver_ty.to_string(),
            factory: format!("New{receiver_ty}"),
            probe: PROBE.to_string(),
            label: label.to_string(),
        }
    }
}

/// Emit the universe. Deterministic: the same corpus every run, so a failing
/// cell reproduces from its label alone.
pub fn corpus() -> Corpus {
    let mut types: Vec<Emitted> = Vec::new();
    let mut cells: Vec<Cell> = Vec::new();

    // ── The two-level grid ───────────────────────────────────────────────────
    for base in BASE_SHAPES {
        for derived in DERIVED_SHAPES {
            let n = cells.len();
            let (b, c) = (format!("B{n}"), format!("C{n}"));
            types.push(Emitted {
                header: format!("public class {b}"),
                body: base.csharp(BASE_TY, false),
            });
            types.push(Emitted {
                header: format!("public class {c} : {b}"),
                body: derived.csharp(DERIVED_TY, base != Decl::None),
            });
            cells.push(Cell {
                receiver_ty: c,
                factory: format!("New{n}"),
                probe: PROBE.to_string(),
                label: format!("grid base={} derived={}", base.tag(), derived.tag()),
            });
        }
    }
    // Every class cell's receiver comes from its own factory.
    let class_factories: Vec<(String, String)> = cells
        .iter()
        .map(|c| (c.factory.clone(), c.receiver_ty.clone()))
        .collect();

    // ── Interface receivers ──────────────────────────────────────────────────
    //
    // The interface walk has no single base chain to follow, so its rules are
    // about the DAG: an own declaration wins, one inherited declaring level
    // resolves, two are an ambiguity v1 declines, and a grandparent is reached
    // through an intermediate that declares nothing.
    for (name, bases, body) in INTERFACES {
        types.push(Emitted {
            header: format!("public interface {name}{bases}"),
            body: body.iter().map(|s| (*s).to_string()).collect(),
        });
    }
    for (iface, label) in [
        ("IA", "interface own-declaration"),
        ("IOwn", "interface own-declaration hiding an inherited one"),
        ("IOne", "interface inherits from one declaring level"),
        ("ITwo", "interface inherits from two declaring levels"),
        ("IDeep", "interface inherits through a silent intermediate"),
    ] {
        cells.push(Cell {
            receiver_ty: iface.to_string(),
            factory: format!("Null{iface}"),
            probe: PROBE.to_string(),
            label: label.to_string(),
        });
    }

    // ── Implementing an interface ────────────────────────────────────────────
    //
    // An *implicit* implementation is an ordinary public member of the class, so
    // it resolves like any other; an **explicit** one compiles to a private
    // member whose IL name is the dotted `HideCorpus.IA.P`, which no `c.P` can
    // reach.
    types.push(Emitted {
        header: "public class ImplicitImpl : IA".to_string(),
        body: vec![format!("public {BASE_TY} P => default({BASE_TY});")],
    });
    types.push(Emitted {
        header: "public class ExplicitImpl : IA".to_string(),
        body: vec![format!("{BASE_TY} IA.P => default({BASE_TY});")],
    });

    // ── A struct receiver ────────────────────────────────────────────────────
    //
    // A value type's base chain runs to `System.ValueType`, not straight to
    // `System.Object` — a different `EntityKind` at a different cap.
    types.push(Emitted {
        header: "public struct StructRecv".to_string(),
        body: vec![format!("public {BASE_TY} P {{ get; set; }}")],
    });

    // ── A closed generic base ────────────────────────────────────────────────
    //
    // The base chain runs into `GBase<int>`, an instantiation the entity model
    // does not carry, so the chain is incomplete: an inherited member is not
    // reachable, but one the receiver declares itself still is.
    types.push(Emitted {
        header: "public class GBase<T>".to_string(),
        body: vec!["public T P { get; set; }".to_string()],
    });
    types.push(Emitted {
        header: "public class GClosed : GBase<int>".to_string(),
        body: vec![],
    });
    types.push(Emitted {
        header: "public class GOwn : GBase<int>".to_string(),
        body: vec![format!("public new {DERIVED_TY} P {{ get; set; }}")],
    });

    // ── Three-level chains ───────────────────────────────────────────────────
    //
    // Which level wins when more than one declares: the *nearest*, and only it.
    types.push(Emitted {
        header: "public class DTop".to_string(),
        body: vec![format!("public {BASE_TY} P {{ get; set; }}")],
    });
    types.push(Emitted {
        header: "public class DMid : DTop".to_string(),
        body: vec![format!("public new {DERIVED_TY} P {{ get; set; }}")],
    });
    types.push(Emitted {
        header: "public class DLeaf : DMid".to_string(),
        body: vec![],
    });
    types.push(Emitted {
        header: "public class ETop".to_string(),
        body: vec![format!("public {BASE_TY} P {{ get; set; }}")],
    });
    types.push(Emitted {
        header: "public class EMid : ETop".to_string(),
        body: vec![],
    });
    types.push(Emitted {
        header: "public class ELeaf : EMid".to_string(),
        body: vec![],
    });
    // A member whose *type* the `TypeRef → Ty` bridge does not render. The
    // member is found and named; only its type is unavailable, so this cell asks
    // whether a member resolution is published when the type is not.
    types.push(Emitted {
        header: "public class UnrenderableTy".to_string(),
        body: vec!["public System.Collections.Generic.List<int> P { get; set; }".to_string()],
    });

    // ── A base chain that leaves this assembly ───────────────────────────────
    //
    // Every grid level lives in one assembly, so the walk never has to *cross*
    // one. These derive from `System.Exception` and probe a member it declares:
    // inherited across the boundary, hidden by a derived declaration, and hidden
    // by a static.
    types.push(Emitted {
        header: "public class BclInherit : System.Exception".to_string(),
        body: vec![],
    });
    types.push(Emitted {
        header: "public class BclHide : System.Exception".to_string(),
        body: vec![format!(
            "public new {DERIVED_TY} {BCL_PROBE} {{ get; set; }}"
        )],
    });
    types.push(Emitted {
        header: "public class BclHideStatic : System.Exception".to_string(),
        body: vec![format!(
            "public static new {DERIVED_TY} {BCL_PROBE} {{ get; set; }}"
        )],
    });

    let mut extra_factories: Vec<(String, String)> = Vec::new();
    for (ty, label) in [
        ("GClosed", "generic base, member inherited"),
        ("GOwn", "generic base, member declared on the receiver"),
        (
            "DLeaf",
            "three-level chain, nearest declaring level is the middle",
        ),
        ("ELeaf", "three-level chain, only the top declares"),
        ("UnrenderableTy", "member type the Ty bridge declines"),
        ("ImplicitImpl", "implicit interface implementation"),
        ("ExplicitImpl", "explicit interface implementation"),
        ("StructRecv", "struct receiver"),
    ] {
        extra_factories.push((format!("New{ty}"), ty.to_string()));
        cells.push(Cell::probing_p(ty, label));
    }
    for (ty, label) in [
        ("BclInherit", "cross-assembly base, member inherited"),
        (
            "BclHide",
            "cross-assembly base, member hidden by the receiver",
        ),
        (
            "BclHideStatic",
            "cross-assembly base, member hidden by a static",
        ),
    ] {
        extra_factories.push((format!("New{ty}"), ty.to_string()));
        cells.push(Cell {
            receiver_ty: ty.to_string(),
            factory: format!("New{ty}"),
            probe: BCL_PROBE.to_string(),
            label: label.to_string(),
        });
    }

    let factories: Vec<(String, String)> =
        class_factories.into_iter().chain(extra_factories).collect();
    let csharp = render_csharp(&types, &factories);
    let (fsharp, sites) = render_fsharp(&cells);
    Corpus {
        csharp,
        fsharp,
        sites,
    }
}

/// Render the C# assembly: the factory class, then every declared type.
fn render_csharp(types: &[Emitted], factories: &[(String, String)]) -> String {
    let mut cs = String::new();
    cs.push_str(
        "// GENERATED by crates/sema/tests/all/common/member_hiding_corpus.rs — do not edit.\n\
         // The member-hiding matrix for the Stage 3.3a data-member wake.\n",
    );
    cs.push_str(&format!("namespace {NS}\n{{\n"));
    cs.push_str("    public static class Make\n    {\n");
    for (factory, ty) in factories {
        cs.push_str(&format!(
            "        public static {ty} {factory}() => new {ty}();\n"
        ));
    }
    for (name, _, _) in INTERFACES {
        cs.push_str(&format!(
            "        public static {name} Null{name}() => null;\n"
        ));
    }
    cs.push_str("    }\n\n");
    for t in types {
        cs.push_str(&format!("    {}\n    {{\n", t.header));
        for line in &t.body {
            cs.push_str(&format!("        {line}\n"));
        }
        cs.push_str("    }\n\n");
    }
    cs.push_str("}\n");
    cs
}

/// Render the F# access matrix over `cells`, and index the site on each access
/// line.
fn render_fsharp(cells: &[Cell]) -> (String, Vec<Site>) {
    let mut fs = String::from("module Gen\n");
    let mut line = 1usize;
    let mut sites = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        fs.push_str(&format!("let v{i} = {NS}.Make.{}()\n", cell.factory));
        line += 1;
        let binding = format!("let r{i} = ");
        let access = format!("v{i}.{}", cell.probe);
        let start = fs.len() + binding.len();
        let text = format!("{binding}{access}");
        fs.push_str(&text);
        fs.push('\n');
        line += 1;
        sites.push(Site {
            line,
            label: cell.label.clone(),
            receiver_ty: cell.receiver_ty.clone(),
            factory: cell.factory.clone(),
            probe: cell.probe.clone(),
            access: (start, start + access.len()),
            text,
        });
    }
    (fs, sites)
}
