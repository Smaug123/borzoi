//! No F# use resolves to an explicit interface implementation of an assembly
//! type.
//!
//! C# emits an explicit interface implementation as a `private` method whose IL
//! name embeds the interface (`System.IConvertible.ToInt32`,
//! `System.Numerics.IAdditionOperators<System.Byte,System.Byte,System.Byte>.op_Addition`).
//! FCS never offers one: a use through the interface names the interface's
//! member. Hover reads XML docs for exactly the member a use resolves to, so
//! whether the docs of these members can ever be wanted — the question issue #96
//! (a lookup retry for their docs-pipeline key spellings) turns on — is whether
//! resolution ever names one.
//!
//! It should not: every lookup that turns a name into an assembly
//! [`Resolution::Member`] keeps only `public` members. This checks that over
//! every explicit implementation in `System.Runtime`, reached the only way F#
//! source can spell one — its IL name in double backticks — both on an instance
//! (`x.``I.M```, `x.``I.M``()`) and on the type (`T.``I.M```). Every member
//! resolution the resolver or inference records, for those uses and any other,
//! must name a public member. A public static member of each type is used
//! alongside, so the run is shown to commit member resolutions at all.

use std::fmt::Write as _;

use borzoi_assembly::{Access, Ecma335Assembly, EcmaView, Entity, EntityKind, Member};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::AstNode;
use borzoi_cst::syntax::ImplFile;
use borzoi_sema::{
    AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, infer_file, resolve_file,
};

use crate::common::ensure_system_runtime_dll;

fn access(m: &Member) -> Access {
    match m {
        Member::Method(m) => m.access,
        Member::Field(f) => f.access,
        Member::Property(p) => p.access,
        Member::Event(e) => e.access,
    }
}

fn name(m: &Member) -> &str {
    match m {
        Member::Method(m) => &m.name,
        Member::Field(f) => &f.name,
        Member::Property(p) => &p.name,
        Member::Event(e) => &e.name,
    }
}

fn is_static(m: &Member) -> bool {
    match m {
        Member::Method(m) => m.is_static,
        Member::Field(f) => f.is_static,
        Member::Property(p) => p.is_static,
        Member::Event(e) => e.is_static,
    }
}

#[test]
fn no_use_resolves_to_an_explicit_interface_implementation() {
    let dll = ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let entities: Vec<Entity> = view.enumerate_type_defs().expect("enumerate");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");

    // One binding per use, over every public non-generic top-level class or
    // struct that has explicit implementations.
    let mut src = String::from("module M\n");
    let (mut impls, mut controls) = (0usize, 0usize);
    for e in &entities {
        if e.access != Access::Public
            || !e.generic_parameters.is_empty()
            || !matches!(e.kind, EntityKind::Class | EntityKind::Struct)
            || e.namespace.is_empty()
        {
            continue;
        }
        let ty = format!("{}.{}", e.namespace.join("."), e.name);
        let explicit: Vec<&Member> = e
            .members
            .iter()
            .filter(|m| name(m).contains('.') && !name(m).starts_with('.'))
            .collect();
        if explicit.is_empty() {
            continue;
        }
        for m in explicit {
            assert_ne!(
                access(m),
                Access::Public,
                "{ty}: a public member with an explicit-implementation name, {}",
                name(m)
            );
            let n = name(m);
            if n.contains("``") {
                continue;
            }
            let i = impls;
            if is_static(m) {
                writeln!(src, "let s{i} () = {ty}.``{n}``").unwrap();
            } else {
                writeln!(src, "let p{i} (x : {ty}) = x.``{n}``").unwrap();
                writeln!(src, "let c{i} (x : {ty}) = x.``{n}``()").unwrap();
            }
            impls += 1;
        }
        if let Some(control) = e
            .members
            .iter()
            .find(|m| is_static(m) && access(m) == Access::Public && matches!(m, Member::Field(_)))
        {
            writeln!(src, "let k{controls} () = {ty}.{}", name(control)).unwrap();
            controls += 1;
        }
    }
    assert!(
        impls > 500,
        "only {impls} explicit implementations used — vacuous"
    );

    let parsed = parse(&src);
    assert!(
        parsed.errors.is_empty(),
        "the generated uses do not parse: {:?}",
        parsed.errors.iter().take(5).collect::<Vec<_>>()
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env, &recovery);
    let inferred = infer_file(&file, &resolved, &env);

    let mut committed = 0usize;
    let mut private = Vec::new();
    for (range, res) in resolved
        .resolutions()
        .iter()
        .chain(inferred.member_resolutions().iter())
    {
        if let Resolution::Member { parent, idx } = *res {
            committed += 1;
            let member = env.member_at(parent, idx);
            if access(member) != Access::Public {
                private.push(format!(
                    "{}: {}",
                    &src[usize::from(range.start())..usize::from(range.end())],
                    name(member)
                ));
            }
        }
    }
    eprintln!(
        "[explicit_impl_unreachable] {impls} explicit implementations used, {controls} controls, \
         {committed} member resolutions, {} non-public",
        private.len()
    );
    assert!(
        committed >= controls / 2,
        "only {committed} member resolutions for {controls} public controls — the run commits \
         nothing, so it shows nothing"
    );
    assert!(
        private.is_empty(),
        "uses resolved to a non-public member:\n{}",
        private.join("\n")
    );
}
