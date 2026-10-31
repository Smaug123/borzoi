//! `[<CustomOperation>]` as the projection reads it
//! ([`MethodLike::custom_operation`]), off the real, shipped `FSharp.Core.dll`:
//! the `query` builder's operations by the name the attribute gives them, a
//! join's condition word, and no attribute left unread. A computation
//! expression's custom operations take bare names ahead of every binding
//! outside it, so a name missed here is a wrong answer in the resolver.

use std::collections::BTreeSet;

use borzoi_assembly::{CustomOperationName, Ecma335Assembly, EcmaView, Entity, Member, MethodLike};

use crate::common::ensure_fsharp_core_dll;

fn all_entities(entities: &[Entity]) -> Vec<&Entity> {
    let mut out = Vec::new();
    let mut stack: Vec<&Entity> = entities.iter().collect();
    while let Some(e) = stack.pop() {
        out.push(e);
        stack.extend(e.nested_types.iter());
    }
    out
}

fn methods(e: &Entity) -> impl Iterator<Item = &MethodLike> {
    e.members.iter().filter_map(|m| match m {
        Member::Method(m) => Some(&**m),
        _ => None,
    })
}

#[test]
fn fsharp_core_custom_operations_are_read() {
    let bytes = std::fs::read(ensure_fsharp_core_dll()).expect("read FSharp.Core.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core.dll");
    let entities = view.enumerate_type_defs().expect("enumerate FSharp.Core");
    let all = all_entities(&entities);

    let unreadable: Vec<String> = all
        .iter()
        .flat_map(|e| methods(e).map(move |m| (e, m)))
        .filter(|(_, m)| {
            m.custom_operation
                .as_ref()
                .is_some_and(|op| op.name == CustomOperationName::Unreadable)
        })
        .map(|(e, m)| format!("{}.{}", e.name, m.name))
        .collect();
    assert!(
        unreadable.is_empty(),
        "unread custom operations: {unreadable:?}"
    );

    let builder = |name: &str| {
        all.iter()
            .find(|e| e.namespace.join(".") == "Microsoft.FSharp.Linq" && e.name == name)
            .copied()
            .unwrap_or_else(|| panic!("no {name}"))
    };
    let query = builder("QueryBuilder");
    let names: BTreeSet<String> = methods(query)
        .filter_map(|m| match &m.custom_operation.as_ref()?.name {
            CustomOperationName::Named(n) => Some(n.clone()),
            other => panic!("{}: {other:?}", m.name),
        })
        .collect();
    // `Query.fsi`'s operations, by the attribute's name, not the method's
    // (`Where` is `where`).
    for expected in [
        "all",
        "averageBy",
        "averageByNullable",
        "contains",
        "count",
        "distinct",
        "exactlyOne",
        "exactlyOneOrDefault",
        "exists",
        "find",
        "groupBy",
        "groupJoin",
        "groupValBy",
        "head",
        "headOrDefault",
        "join",
        "last",
        "lastOrDefault",
        "leftOuterJoin",
        "maxBy",
        "maxByNullable",
        "minBy",
        "minByNullable",
        "nth",
        "select",
        "skip",
        "skipWhile",
        "sortBy",
        "sortByDescending",
        "sortByNullable",
        "sortByNullableDescending",
        "sumBy",
        "sumByNullable",
        "take",
        "takeWhile",
        "thenBy",
        "thenByDescending",
        "thenByNullable",
        "thenByNullableDescending",
        "where",
    ] {
        assert!(
            names.contains(expected),
            "{expected} missing from {names:?}"
        );
    }
    let join = methods(query)
        .find(|m| m.name == "Join")
        .and_then(|m| m.custom_operation.as_ref())
        .expect("query's Join is a custom operation");
    assert_eq!(join.join_condition_word.as_deref(), Some("on"));

    for (namespace, name) in [
        ("Microsoft.FSharp.Control", "FSharpAsyncBuilder"),
        ("Microsoft.FSharp.Control", "TaskBuilder"),
        ("Microsoft.FSharp.Control", "TaskBuilderBase"),
    ] {
        let e = all
            .iter()
            .find(|e| e.namespace.join(".") == namespace && e.name == name)
            .unwrap_or_else(|| panic!("no {namespace}.{name}"));
        assert!(
            methods(e).all(|m| m.custom_operation.is_none()),
            "{name} has a custom operation"
        );
    }
}
