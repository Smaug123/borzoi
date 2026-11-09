//! Generative differential for the documentation-comment keys of F# **overload
//! groups**, against the keys fsc itself writes into the doc XML.
//!
//! `doc_id_fsharp_diff.rs` grades whole real libraries, so it sees an overload
//! group only where one happens to exist. This harness searches the shape
//! instead: each case generates members that share a name — instance and static
//! methods, instance and static extension members, and conversion operators —
//! whose overloads take different parameter types, some of the same arity and
//! some not. Every member carries a `///` comment naming it (`doc-g3-o1.`:
//! group 3, overload 1), and the case is compiled by the real fsc with its
//! documentation file on, so the XML says which key fsc wrote for which
//! declaration.
//!
//! The property, for every generated overload: the projected method that
//! compiles it (found by its IL name and parameter types) is keyed by exactly
//! the key fsc wrote for it, and no other member of the assembly generates that
//! key. The first half is coverage; the second is attribution — a key handed to
//! a sibling overload would show the sibling's documentation on hover.
//!
//! The parameter types are chosen where fsc's key and the computed (Roslyn)
//! form differ, so a member left without its pickled key misses: a 2-D array
//! (`[0:]` against `[0:,0:]`), an extension member's dotted name, a conversion
//! operator's return-type suffix. Plain types and a measure-annotated `float`
//! sit beside them, and generic overloads, whose keys count the method's type
//! parameters.
//!
//! Each case is one `dotnet build`, so the default case count is small:
//!
//! ```sh
//! BORZOI_GENERATIVE_CASES=50 nix develop -c cargo test -p borzoi-assembly \
//!     --test all doc_id_fsharp_overload_diff::
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;

use borzoi_assembly::doc_id::{member_doc_ids, type_doc_name, type_enc, walk_doc_ids};
use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, Member, MethodLike};
use proptest::prelude::*;

use crate::common::{Lang, compile_generated_documented};

/// A parameter type.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ty {
    Int,
    Int64,
    String,
    Bool,
    Float,
    /// `float<m>`: erased to `System.Double` in the IL and in the key.
    FloatM,
    /// The member's own type parameter `'a`.
    Gen,
    Option(Box<Ty>),
    List(Box<Ty>),
    Array(Box<Ty>),
    /// A two-dimensional array, which fsc keys `[0:]` and the IL `[0:,0:]`.
    Array2(Box<Ty>),
    Pair(Box<Ty>, Box<Ty>),
}

impl Ty {
    fn mentions_gen(&self) -> bool {
        match self {
            Ty::Gen => true,
            Ty::Option(t) | Ty::List(t) | Ty::Array(t) | Ty::Array2(t) => t.mentions_gen(),
            Ty::Pair(a, b) => a.mentions_gen() || b.mentions_gen(),
            _ => false,
        }
    }
}

/// Where a group's members are declared, which fixes their IL name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Host {
    /// `member _.I<g> (…)` on `Host`.
    Instance,
    /// `static member S<g> (…)` on `Host`.
    Static,
    /// `member _.X<g> (…)` in `type Host with` inside `module Ext`: an
    /// optional extension, compiled as a static `Host.X<g>` on `Ext` taking
    /// the receiver first.
    ExtInstance,
    /// `static member Y<g> (…)` in the same extension: `Host.Y<g>.Static` on
    /// `Ext`.
    ExtStatic,
    /// `static member op_Implicit (q0: …) : Host` on `Host`.
    Implicit,
    /// `static member op_Explicit (q0: …) : Host` on `Host`.
    Explicit,
}

/// Members sharing one name, each overload a parameter list.
#[derive(Debug, Clone)]
struct Group {
    host: Host,
    overloads: Vec<Vec<Ty>>,
}

fn render_ty(ty: &Ty) -> String {
    match ty {
        Ty::Int => "int".into(),
        Ty::Int64 => "int64".into(),
        Ty::String => "string".into(),
        Ty::Bool => "bool".into(),
        Ty::Float => "float".into(),
        Ty::FloatM => "float<m>".into(),
        Ty::Gen => "'a".into(),
        Ty::Option(t) => format!("{} option", render_ty(t)),
        Ty::List(t) => format!("{} list", render_ty(t)),
        Ty::Array(t) => format!("{}[]", render_ty(t)),
        Ty::Array2(t) => format!("{}[,]", render_ty(t)),
        Ty::Pair(a, b) => format!("({} * {})", render_ty(a), render_ty(b)),
    }
}

/// The type as the projection's [`type_enc`] spells the IL parameter: what
/// finds an overload's method.
fn il_enc(ty: &Ty) -> String {
    match ty {
        Ty::Int => "System.Int32".into(),
        Ty::Int64 => "System.Int64".into(),
        Ty::String => "System.String".into(),
        Ty::Bool => "System.Boolean".into(),
        Ty::Float | Ty::FloatM => "System.Double".into(),
        Ty::Gen => "``0".into(),
        Ty::Option(t) => format!("Microsoft.FSharp.Core.FSharpOption{{{}}}", il_enc(t)),
        Ty::List(t) => format!("Microsoft.FSharp.Collections.FSharpList{{{}}}", il_enc(t)),
        Ty::Array(t) => format!("{}[]", il_enc(t)),
        Ty::Array2(t) => format!("{}[0:,0:]", il_enc(t)),
        Ty::Pair(a, b) => format!("System.Tuple{{{},{}}}", il_enc(a), il_enc(b)),
    }
}

fn il_params(params: &[Ty]) -> Vec<String> {
    params.iter().map(il_enc).collect()
}

/// The doc comment that names group `g`'s overload `o`. The trailing `.` keeps
/// `doc-g1-o1.` from matching inside `doc-g1-o10.`.
fn marker(g: usize, o: usize) -> String {
    format!("doc-g{g}-o{o}.")
}

/// The IL name a group's members compile to, and the entity that declares them.
fn il_name(g: usize, host: Host) -> (String, &'static str) {
    match host {
        Host::Instance => (format!("I{g}"), "Host"),
        Host::Static => (format!("S{g}"), "Host"),
        Host::ExtInstance => (format!("Host.X{g}"), "Ext"),
        Host::ExtStatic => (format!("Host.Y{g}.Static"), "Ext"),
        Host::Implicit => ("op_Implicit".into(), "Host"),
        Host::Explicit => ("op_Explicit".into(), "Host"),
    }
}

fn render_member(out: &mut String, indent: &str, g: usize, o: usize, host: Host, params: &[Ty]) {
    let _ = writeln!(out, "{indent}/// {}", marker(g, o));
    let typars = if params.iter().any(Ty::mentions_gen) {
        "<'a>"
    } else {
        ""
    };
    let args = params
        .iter()
        .enumerate()
        .map(|(i, p)| format!("q{i}: {}", render_ty(p)))
        .collect::<Vec<_>>()
        .join(", ");
    let line = match host {
        Host::Instance => format!("member _.I{g}{typars} ({args}) : int = 0"),
        Host::Static => format!("static member S{g}{typars} ({args}) : int = 0"),
        Host::ExtInstance => format!("member _.X{g}{typars} ({args}) : int = 0"),
        Host::ExtStatic => format!("static member Y{g}{typars} ({args}) : int = 0"),
        Host::Implicit => {
            format!("static member op_Implicit ({args}) : Host = Unchecked.defaultof<Host>")
        }
        Host::Explicit => {
            format!("static member op_Explicit ({args}) : Host = Unchecked.defaultof<Host>")
        }
    };
    let _ = writeln!(out, "{indent}{line}");
}

fn render_source(groups: &[Group]) -> String {
    let mut out = String::from(
        "// Generated by crates/assembly/tests/all/doc_id_fsharp_overload_diff.rs — do not edit.\n\
         namespace Generated\n\n\
         [<Measure>] type m\n\n\
         type Host() =\n",
    );
    let on_host = |h: Host| !matches!(h, Host::ExtInstance | Host::ExtStatic);
    let mut any = false;
    for (g, group) in groups.iter().enumerate() {
        if on_host(group.host) {
            for (o, params) in group.overloads.iter().enumerate() {
                render_member(&mut out, "    ", g, o, group.host, params);
                any = true;
            }
        }
    }
    if !any {
        out.push_str("    class end\n");
    }
    if groups.iter().any(|g| !on_host(g.host)) {
        out.push_str("\nmodule Ext =\n    type Host with\n");
        for (g, group) in groups.iter().enumerate() {
            if !on_host(group.host) {
                for (o, params) in group.overloads.iter().enumerate() {
                    render_member(&mut out, "        ", g, o, group.host, params);
                }
            }
        }
    }
    out
}

/// Each `<member name="…">` key of a doc XML, with the element's text.
fn shipped(xml: &str) -> BTreeMap<String, String> {
    const OPEN: &str = "<member name=\"";
    let mut out = BTreeMap::new();
    let mut rest = xml;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let end = after.find('"').expect("unterminated member name attribute");
        let key = after[..end].to_string();
        let body_end = after
            .find("</member>")
            .expect("unterminated member element");
        out.insert(key, after[end..body_end].to_string());
        rest = &after[body_end..];
    }
    out
}

/// Compile the groups and check the property, or describe the failure.
fn check(groups: &[Group]) -> Result<(), String> {
    let source = render_source(groups);
    let fail = |stage: &str, detail: String| {
        format!("{stage}: {detail}\n--- generated source ---\n{source}")
    };
    let dll =
        compile_generated_documented(Lang::FSharp, &source).map_err(|e| fail("compile", e))?;
    let xml_path = dll.with_extension("xml");
    let xml = std::fs::read_to_string(&xml_path)
        .map_err(|e| fail("read xml", format!("{}: {e}", xml_path.display())))?;
    let keys = shipped(&xml);
    let bytes = std::fs::read(&dll).map_err(|e| fail("read dll", e.to_string()))?;
    let view = Ecma335Assembly::parse(&bytes).map_err(|e| fail("parse", e.to_string()))?;
    let types = view
        .enumerate_type_defs()
        .map_err(|e| fail("enumerate", e.to_string()))?;
    let mut generated: BTreeMap<String, usize> = BTreeMap::new();
    for e in &types {
        walk_doc_ids(e, None, &mut |id| *generated.entry(id).or_default() += 1);
    }
    let entity = |name: &str| -> Result<&Entity, String> {
        types
            .iter()
            .find(|e| e.namespace == ["Generated"] && e.name == name)
            .ok_or_else(|| fail("projection", format!("no entity Generated.{name}")))
    };

    let mut problems = Vec::new();
    for (g, group) in groups.iter().enumerate() {
        let (name, owner) = il_name(g, group.host);
        let owner = entity(owner)?;
        let decl = type_doc_name(owner, None);
        let receiver = usize::from(group.host == Host::ExtInstance);
        for (o, params) in group.overloads.iter().enumerate() {
            let tag = marker(g, o);
            let fsc: Vec<&String> = keys
                .iter()
                .filter(|(_, body)| body.contains(&tag))
                .map(|(k, _)| k)
                .collect();
            let [fsc] = fsc.as_slice() else {
                return Err(fail(
                    "oracle",
                    format!("expected one key documented `{tag}`, found {fsc:?}"),
                ));
            };
            let want = il_params(params);
            let compiles_it = |m: &MethodLike| {
                m.name == name
                    && m.signature.parameters.len() == want.len() + receiver
                    && m.signature.parameters[receiver..]
                        .iter()
                        .map(|p| type_enc(&p.ty))
                        .eq(want.iter().cloned())
            };
            let methods: Vec<&Member> = owner
                .members
                .iter()
                .filter(|m| matches!(m, Member::Method(m) if compiles_it(m)))
                .collect();
            let [method] = methods.as_slice() else {
                problems.push(format!(
                    "{tag} ({name}({})): {} projected methods compile it",
                    want.join(","),
                    methods.len()
                ));
                continue;
            };
            let ids = member_doc_ids(&decl, method);
            if !ids.contains(fsc) {
                problems.push(format!(
                    "{tag}: fsc wrote {fsc}, the method generates {ids:?}"
                ));
            }
            match generated.get(*fsc) {
                Some(1) | None => {}
                Some(n) => problems.push(format!("{tag}: {fsc} is generated {n} times")),
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(fail("doc keys", problems.join("\n")))
    }
}

fn leaf(generic: bool) -> BoxedStrategy<Ty> {
    let plain = prop_oneof![
        Just(Ty::Int),
        Just(Ty::Int64),
        Just(Ty::String),
        Just(Ty::Bool),
        Just(Ty::Float),
        Just(Ty::FloatM),
    ];
    if generic {
        prop_oneof![5 => plain, 1 => Just(Ty::Gen)].boxed()
    } else {
        plain.boxed()
    }
}

fn ty_strategy(generic: bool) -> impl Strategy<Value = Ty> {
    leaf(generic).prop_recursive(2, 4, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|t| Ty::Option(Box::new(t))),
            inner.clone().prop_map(|t| Ty::List(Box::new(t))),
            inner.clone().prop_map(|t| Ty::Array(Box::new(t))),
            inner.clone().prop_map(|t| Ty::Array2(Box::new(t))),
            (inner.clone(), inner).prop_map(|(a, b)| Ty::Pair(Box::new(a), Box::new(b))),
        ]
    })
}

/// Drop each overload whose IL parameter list repeats an earlier one's: fsc
/// rejects a duplicate signature, and `float<m>` erases to `float`.
fn distinct(overloads: Vec<Vec<Ty>>) -> Vec<Vec<Ty>> {
    let mut seen = std::collections::HashSet::new();
    overloads
        .into_iter()
        .filter(|o| seen.insert(il_params(o)))
        .collect()
}

fn method_group() -> impl Strategy<Value = Group> {
    let host = prop_oneof![
        Just(Host::Instance),
        Just(Host::Static),
        Just(Host::ExtInstance),
        Just(Host::ExtStatic),
    ];
    // Arities 1–2, so same-arity overloads are common.
    (
        host,
        prop::collection::vec(prop::collection::vec(ty_strategy(true), 1..3), 1..5),
    )
        .prop_map(|(host, overloads)| Group {
            host,
            overloads: distinct(overloads),
        })
}

fn conversion_group(host: Host) -> impl Strategy<Value = Group> {
    prop::collection::vec(ty_strategy(false), 0..4).prop_map(move |tys| Group {
        host,
        overloads: distinct(tys.into_iter().map(|t| vec![t]).collect()),
    })
}

fn program() -> impl Strategy<Value = Vec<Group>> {
    (
        prop::collection::vec(method_group(), 1..5),
        conversion_group(Host::Implicit),
        conversion_group(Host::Explicit),
    )
        .prop_map(|(mut groups, implicit, explicit)| {
            groups.extend(
                [implicit, explicit]
                    .into_iter()
                    .filter(|g| !g.overloads.is_empty()),
            );
            groups
        })
}

/// One program reaching every host, with a same-arity group on each whose keys
/// the computed form cannot produce.
#[test]
fn smoke_every_host() {
    let arr2 = || Ty::Array2(Box::new(Ty::Int));
    let groups = vec![
        Group {
            host: Host::Instance,
            overloads: vec![vec![arr2()], vec![Ty::String], vec![Ty::Gen]],
        },
        Group {
            host: Host::Static,
            overloads: vec![
                vec![arr2(), Ty::Int],
                vec![Ty::FloatM, Ty::Int],
                vec![Ty::Int],
            ],
        },
        Group {
            host: Host::ExtInstance,
            overloads: vec![vec![Ty::Int], vec![Ty::String]],
        },
        Group {
            host: Host::ExtStatic,
            overloads: vec![
                vec![Ty::Option(Box::new(Ty::Gen))],
                vec![Ty::List(Box::new(Ty::Int))],
            ],
        },
        Group {
            host: Host::Implicit,
            overloads: vec![vec![Ty::Int], vec![Ty::String]],
        },
        Group {
            host: Host::Explicit,
            overloads: vec![vec![Ty::Bool], vec![arr2()]],
        },
    ];
    if let Err(msg) = check(&groups) {
        panic!("{msg}");
    }
}

fn generative_cases() -> u32 {
    match std::env::var("BORZOI_GENERATIVE_CASES") {
        Ok(v) => v.parse().expect("BORZOI_GENERATIVE_CASES must be a u32"),
        Err(_) => 4,
    }
}

proptest! {
    // No `lib.rs` anchor for a regression directory in an integration-test
    // binary; the shrunk groups and the source in the failure are the
    // reproduction. Each shrink step is a `dotnet build`, hence the bound.
    #![proptest_config(ProptestConfig {
        cases: generative_cases(),
        failure_persistence: None,
        max_shrink_iters: 100,
        ..ProptestConfig::default()
    })]

    /// For all generated overload groups, each overload's method is keyed by
    /// the key fsc wrote for it, and by no other member.
    #[test]
    fn overload_groups_take_fscs_keys(groups in program()) {
        if let Err(msg) = check(&groups) {
            return Err(TestCaseError::fail(msg));
        }
    }
}
