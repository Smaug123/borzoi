//! Each F# union case's **maker**: the static method fsc emits to build the
//! case (`NewOk(T)`, FSharp.Core's specially named `Some(T)` and
//! `Cons(T, FSharpList<T>)`), or for a nullary case its getter (`get_None()`).
//! fsc tags every one `[CompilationMapping(SourceConstructFlags.UnionCase, n)]`,
//! where `n` is the case's position among *all* the union's cases in
//! declaration order — the same position the signature pickle lists the case
//! at. So the projector keeps the makers by that position
//! ([`Entity::union_case_makers`]) and the pickle overlay records each listed
//! case's position and what its pickled field types say
//! ([`UnionCase::declaration_index`], [`UnionCase::fields`]); a consumer joins
//! the two by position, never by the maker's name.

use std::path::Path;

use borzoi_assembly::{
    CaseFields, Ecma335Assembly, EcmaView, Entity, EntityKind, TypeRef, UnionCase, UnionCases,
    ValMeasures,
};

use crate::common::ensure_fsharp_core_dll;

fn load(dll: &Path) -> Vec<Entity> {
    let bytes = std::fs::read(dll).expect("read dll");
    Ecma335Assembly::parse(&bytes)
        .expect("parse dll")
        .enumerate_type_defs()
        .expect("enumerate types")
}

fn all_entities(entities: &[Entity]) -> Vec<&Entity> {
    let mut out = Vec::new();
    let mut stack: Vec<&Entity> = entities.iter().collect();
    while let Some(e) = stack.pop() {
        out.push(e);
        stack.extend(e.nested_types.iter());
    }
    out
}

fn union_named<'a>(entities: &'a [Entity], namespace: &[&str], name: &str) -> &'a Entity {
    all_entities(entities)
        .into_iter()
        .find(|e| {
            e.kind == EntityKind::Union
                && e.name == name
                && e.namespace
                    .iter()
                    .map(String::as_str)
                    .eq(namespace.iter().copied())
        })
        .unwrap_or_else(|| panic!("union {name:?} not found"))
}

fn cases(e: &Entity) -> &[UnionCase] {
    match &e.union_cases {
        UnionCases::Known(cases) => cases,
        UnionCases::Unknowable => panic!("{}: cases unknowable", e.name),
    }
}

/// The maker of the case at declaration position `index`: its IL name and
/// parameter types.
fn maker(e: &Entity, index: u32) -> (String, Vec<TypeRef>) {
    let found: Vec<_> = e
        .union_case_makers
        .iter()
        .filter(|m| m.declaration_index == index)
        .collect();
    assert_eq!(found.len(), 1, "{}: one maker at position {index}", e.name);
    let m = &found[0].method;
    (
        m.name.clone(),
        m.signature
            .parameters
            .iter()
            .map(|p| p.ty.clone())
            .collect(),
    )
}

fn type_var(index: u16) -> TypeRef {
    TypeRef::Var {
        index,
        is_method: false,
    }
}

/// The case's name, declaration position, and pickled field count.
fn shape(c: &UnionCase) -> (&str, u32, Option<usize>) {
    let count = match &c.fields {
        CaseFields::Read { count, .. } => Some(*count),
        CaseFields::Unknowable => None,
    };
    (c.name.as_str(), c.declaration_index, count)
}

#[test]
fn fsharp_core_union_cases_carry_their_makers_by_declaration_position() {
    let entities = load(&ensure_fsharp_core_dll());
    let core = ["Microsoft", "FSharp", "Core"];

    let option = union_named(&entities, &core, "FSharpOption");
    assert_eq!(
        cases(option).iter().map(shape).collect::<Vec<_>>(),
        [("None", 0, Some(0)), ("Some", 1, Some(1))]
    );
    assert_eq!(maker(option, 0), ("get_None".to_string(), vec![]));
    assert_eq!(maker(option, 1), ("Some".to_string(), vec![type_var(0)]));

    let voption = union_named(&entities, &core, "FSharpValueOption");
    assert_eq!(
        cases(voption).iter().map(shape).collect::<Vec<_>>(),
        [("ValueNone", 0, Some(0)), ("ValueSome", 1, Some(1))]
    );
    assert_eq!(
        maker(voption, 1),
        ("NewValueSome".to_string(), vec![type_var(0)])
    );

    let result = union_named(&entities, &core, "FSharpResult");
    assert_eq!(
        cases(result).iter().map(shape).collect::<Vec<_>>(),
        [("Ok", 0, Some(1)), ("Error", 1, Some(1))]
    );
    assert_eq!(maker(result, 0), ("NewOk".to_string(), vec![type_var(0)]));
    assert_eq!(
        maker(result, 1),
        ("NewError".to_string(), vec![type_var(1)])
    );

    let choice2 = all_entities(&entities)
        .into_iter()
        .find(|e| {
            e.kind == EntityKind::Union
                && e.name == "FSharpChoice"
                && e.generic_parameters.len() == 2
        })
        .expect("Choice<_, _>");
    assert_eq!(
        cases(choice2).iter().map(shape).collect::<Vec<_>>(),
        [("Choice1Of2", 0, Some(1)), ("Choice2Of2", 1, Some(1))]
    );
    assert_eq!(
        maker(choice2, 1),
        ("NewChoice2Of2".to_string(), vec![type_var(1)])
    );

    let list = union_named(
        &entities,
        &["Microsoft", "FSharp", "Collections"],
        "FSharpList",
    );
    assert_eq!(
        cases(list).iter().map(shape).collect::<Vec<_>>(),
        [("op_Nil", 0, Some(0)), ("op_ColonColon", 1, Some(2))]
    );
    let (name, params) = maker(list, 1);
    assert_eq!(name, "Cons");
    assert_eq!(params.len(), 2);
    assert_eq!(params[0], type_var(0));
}

/// For every union whose cases the pickle listed: each case has exactly one
/// maker at its declaration position, taking one parameter per pickled field,
/// and no case of these measure-free unions reads as measured.
#[test]
fn every_fsharp_core_listed_case_has_one_maker_of_its_field_count() {
    let entities = load(&ensure_fsharp_core_dll());
    let mut checked = 0;
    for e in all_entities(&entities) {
        let UnionCases::Known(cases) = &e.union_cases else {
            continue;
        };
        for c in cases {
            let CaseFields::Read {
                count, measures, ..
            } = &c.fields
            else {
                panic!("{}.{}: fields unknowable", e.name, c.name);
            };
            let (_, params) = maker(e, c.declaration_index);
            assert_eq!(params.len(), *count, "{}.{}", e.name, c.name);
            assert!(
                matches!(measures, ValMeasures::NoDirectMeasure { .. }),
                "{}.{}",
                e.name,
                c.name
            );
            checked += 1;
        }
    }
    assert!(checked >= 20, "only {checked} cases checked");
}
