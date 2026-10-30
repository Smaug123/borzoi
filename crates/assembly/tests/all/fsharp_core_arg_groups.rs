//! The argument groups the projection records for each member of the real,
//! shipped `FSharp.Core.dll` ([`MethodLike::arg_groups`]), against the
//! signature pickle they are read from.
//!
//! A hover renders a referenced F# function's parameters in these groups, so a
//! group lost (`a: int * b: int` for `a: int -> b: int`) or invented is a wrong
//! type shown to the user. The pickle is read here directly — each module val's
//! `ValReprInfo`, its groups as a caller writes them — and every projected
//! module method is held to it:
//!
//! - a method whose val is the only one of its compiled name and IL arity, and
//!   the only method of that name and arity, carries exactly the val's groups;
//! - any other carries the groups of one of the vals that could have claimed
//!   it, or none ([`ArgGroups::Unknowable`]) — never a grouping no val has;
//! - every grouping recorded accounts for exactly the method's parameters.
//!
//! The type members are graded by the same pickle through the hover
//! differential (`crates/lsp/tests/all/hover_curried_signatures.rs`); here a
//! census pins how many of FSharp.Core's type methods are left unknown.

use borzoi_assembly::{
    Access, ArgGroup, ArgGroups, CurriedArg, CurriedArgs, Ecma335Assembly, EcmaView, Entity,
    EntityKind, FSharpResource, Member, MethodLike, ModuleMemberTarget, ResourceKind,
    collect_module_member_targets, unpickle_signature,
};

use crate::common::ensure_fsharp_core_dll;

fn primary(rs: &[FSharpResource]) -> &FSharpResource {
    rs.iter()
        .find(|r| {
            matches!(
                r.kind,
                ResourceKind::SignatureData
                    | ResourceKind::SignatureCompressedData
                    | ResourceKind::SignatureDataFSharpCore
            )
        })
        .expect("no primary signature resource on FSharp.Core")
}

fn b_stream(rs: &[FSharpResource]) -> Option<&[u8]> {
    rs.iter()
        .find(|r| {
            matches!(
                r.kind,
                ResourceKind::SignatureDataB | ResourceKind::SignatureCompressedDataB
            )
        })
        .map(|r| r.payload.as_slice())
}

/// One pickled module val's expected reading.
struct Expected {
    il_name: String,
    /// The IL parameter count: every group's arguments, receiver included.
    il_arity: usize,
    groups: CurriedArgs,
}

/// The groups a caller writes, read straight off the val's `ValReprInfo`: an
/// instance augmentation's first group is its receiver, its module method's
/// first parameter. `None` for a val with no `ValReprInfo`, or a literal.
fn expected(ccu: &borzoi_assembly::PickledCcu, val_index: u32) -> Option<Expected> {
    let v = &ccu.tables.vals[val_index as usize];
    if v.literal_value.is_some() {
        return None;
    }
    let repr = v.repr_info.as_ref()?;
    let instance = v
        .member_info
        .as_ref()
        .is_some_and(|mi| mi.flags.is_instance);
    let groups: Vec<ArgGroup> = repr
        .arg_repr
        .iter()
        .map(|g| ArgGroup {
            args: g
                .iter()
                .map(|a| CurriedArg {
                    name: a.name.as_ref().map(|i| i.name.clone()),
                })
                .collect(),
        })
        .collect();
    let il_arity = groups.iter().map(|g| g.args.len()).sum();
    let groups = if instance {
        assert_eq!(
            groups.first().map(|g| g.args.len()),
            Some(1),
            "{}: an instance augmentation's first group is its receiver",
            v.logical_name
        );
        groups[1..].to_vec()
    } else {
        groups
    };
    Some(Expected {
        il_name: v
            .compiled_name
            .clone()
            .unwrap_or_else(|| v.logical_name.clone()),
        il_arity,
        groups: CurriedArgs {
            leading_receiver: instance,
            groups,
        },
    })
}

/// The entity a module target names: `namespace`, then the nesting chain.
fn locate<'a>(entities: &'a [Entity], target: &ModuleMemberTarget) -> Option<&'a Entity> {
    let (first, rest) = target.type_chain.split_first()?;
    let mut entity = entities
        .iter()
        .find(|e| e.namespace == target.namespace && e.name == *first)?;
    for segment in rest {
        entity = entity.nested_types.iter().find(|e| e.name == *segment)?;
    }
    Some(entity)
}

/// The projected method's IL parameter count, counting a module value (a
/// rebranded property getter) as the zero it compiles to.
fn method_arity(m: &MethodLike) -> usize {
    m.signature.parameters.len()
}

#[test]
fn fsharp_core_module_methods_carry_the_pickles_argument_groups() {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).expect("read FSharp.Core.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core.dll");
    let entities = view.enumerate_type_defs().expect("enumerate FSharp.Core");
    let resources = view.fsharp_resources().expect("FSharp.Core's F# resources");
    let ccu = unpickle_signature(&primary(&resources).payload, b_stream(&resources))
        .expect("unpickle FSharp.Core's signature");
    let targets = collect_module_member_targets(&ccu).expect("FSharp.Core's module vals");

    let mut wrong = Vec::new();
    let mut exact = 0usize;
    let mut exact_curried = 0usize;
    let mut exact_tupled = 0usize;
    let mut exact_named_differently = 0usize;
    let mut modules = 0usize;
    for target in &targets {
        let Some(entity) = locate(&entities, target) else {
            continue;
        };
        if entity.kind != EntityKind::Module {
            continue;
        }
        modules += 1;
        let expected: Vec<Expected> = target
            .vals
            .iter()
            .filter_map(|v| expected(&ccu, v.val_index))
            .collect();
        for member in &entity.members {
            let Member::Method(m) = member else { continue };
            let here = format!(
                "{}.{}",
                entity.name,
                m.source_name.as_deref().unwrap_or(&m.name)
            );
            // Every grouping recorded accounts for the method's parameters.
            if let ArgGroups::Read(groups) = &m.arg_groups
                && groups.split(&m.signature.parameters).is_none()
            {
                wrong.push(format!(
                    "  {here}: {groups:?} does not split its parameters"
                ));
            }
            // The count the overload engine reads never contradicts the groups.
            if let (Some(count), Some(groups)) = (m.arg_group_count, m.arg_groups.count())
                && count != groups
            {
                wrong.push(format!(
                    "  {here}: {count} argument groups counted, {groups} recorded"
                ));
            }
            if m.arg_groups == ArgGroups::NotFSharp {
                wrong.push(format!(
                    "  {here}: an F# assembly's method reads `NotFSharp`"
                ));
            }
            let name_matches = |e: &&Expected| e.il_name == m.name && e.il_arity == method_arity(m);
            let claimants: Vec<&Expected> = expected.iter().filter(name_matches).collect();
            let siblings = entity
                .members
                .iter()
                .filter(|o| {
                    matches!(o, Member::Method(o) if o.name == m.name
                        && method_arity(o) == method_arity(m))
                })
                .count();
            match (claimants.as_slice(), &m.arg_groups) {
                // No val could have claimed it: a non-public helper the pickle
                // does not describe.
                ([], ArgGroups::Unknowable) => {}
                ([], other) => wrong.push(format!("  {here}: no val claims it, yet {other:?}")),
                ([only], ArgGroups::Read(groups)) if siblings == 1 => {
                    if **groups == only.groups {
                        exact += 1;
                        if groups.groups.len() >= 2 {
                            exact_curried += 1;
                        }
                        if groups.groups.iter().any(|g| g.args.len() >= 2) {
                            exact_tupled += 1;
                        }
                        let il_names: Vec<Option<&str>> = m
                            .signature
                            .parameters
                            .iter()
                            .skip(usize::from(groups.leading_receiver))
                            .map(|p| p.name.as_deref())
                            .collect();
                        let pickled: Vec<Option<&str>> = groups
                            .groups
                            .iter()
                            .flat_map(|g| g.args.iter().map(|a| a.name.as_deref()))
                            .collect();
                        if il_names != pickled {
                            exact_named_differently += 1;
                        }
                    } else {
                        wrong.push(format!(
                            "  {here}: {groups:?}, where its one val reads {:?}",
                            only.groups
                        ));
                    }
                }
                ([_], other) if siblings == 1 => {
                    wrong.push(format!("  {here}: its one val is read, yet {other:?}"))
                }
                (_, ArgGroups::Read(groups)) => {
                    if !claimants.iter().any(|e| e.groups == **groups) {
                        wrong.push(format!(
                            "  {here}: {groups:?} is no claimant's grouping ({} claimants)",
                            claimants.len()
                        ));
                    }
                }
                (_, ArgGroups::Unknowable | ArgGroups::NotFSharp) => {}
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} FSharp.Core module method(s) carry argument groups the pickle does not:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    // Non-vacuity: the modules were found, and the exact reading reaches the
    // shapes the IL cannot show — curried groups, tupled groups, and argument
    // names the IL spells differently.
    eprintln!(
        "modules {modules}, exact {exact}, curried {exact_curried}, tupled {exact_tupled}, \
         renamed {exact_named_differently}"
    );
    assert!(modules > 70, "only {modules} FSharp.Core modules located");
    assert!(exact > 1_500, "only {exact} module methods read exactly");
    assert!(
        exact_curried > 750,
        "only {exact_curried} curried module methods read exactly"
    );
    assert!(
        exact_tupled > 40,
        "only {exact_tupled} module methods with a tupled group"
    );
    assert!(
        exact_named_differently > 140,
        "only {exact_named_differently} module methods whose IL names differ from F#'s"
    );
}

/// FSharp.Core's **type** methods whose grouping matters — public, on a public
/// type, of two or more parameters — are read, all but a pinned few: its
/// delegates' `BeginInvoke`, which the runtime implements and no val
/// describes. An unknown grouping is shown without types, so the pin fails on
/// any method that joins the unknown set, and on any that leaves it.
#[test]
fn fsharp_core_type_methods_are_read_but_for_a_pinned_few() {
    fn walk(entities: &[Entity], path: &str, read: &mut usize, unknown: &mut Vec<String>) {
        for e in entities {
            if !matches!(e.access, Access::Public) {
                continue;
            }
            let here = format!("{path}{}", e.name);
            if e.kind != EntityKind::Module {
                for m in &e.members {
                    let Member::Method(m) = m else { continue };
                    if m.signature.parameters.len() < 2
                        || m.is_constructor
                        || !matches!(m.access, Access::Public)
                    {
                        continue;
                    }
                    match &m.arg_groups {
                        ArgGroups::Read(_) => *read += 1,
                        _ => unknown.push(format!(
                            "{here}.{}/{}",
                            m.name,
                            m.signature.parameters.len()
                        )),
                    }
                }
            }
            walk(&e.nested_types, &format!("{here}."), read, unknown);
        }
    }
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).expect("read FSharp.Core.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core.dll");
    let entities = view.enumerate_type_defs().expect("enumerate FSharp.Core");
    let mut read = 0;
    let mut unknown = Vec::new();
    walk(&entities, "", &mut read, &mut unknown);
    unknown.sort();
    assert_eq!(
        unknown,
        [
            "AfterCode.BeginInvoke/3",
            "FSharpHandler.BeginInvoke/4",
            "MoveNextMethodImpl.BeginInvoke/3",
            "ResumableCode.BeginInvoke/3",
            "ResumptionFunc.BeginInvoke/3",
            "SetStateMachineMethodImpl.BeginInvoke/4",
        ],
        "the FSharp.Core type methods whose argument groups are unknown"
    );
    assert!(
        read > 150,
        "only {read} FSharp.Core type methods' groups are read"
    );
}

/// [`CurriedArgs::split`] over every grouping of up to four groups of up to
/// three arguments, against parameter lists of every length up to the
/// arguments plus two: it splits exactly when the groups (and receiver)
/// account for every parameter, and then each group is its own run of them,
/// in order.
#[test]
fn split_partitions_exactly_the_parameters() {
    let mut groupings: Vec<Vec<usize>> = vec![vec![]];
    for _ in 0..4 {
        let longer: Vec<Vec<usize>> = groupings
            .iter()
            .flat_map(|g| {
                (0..=3).map(move |n| {
                    let mut g = g.clone();
                    g.push(n);
                    g
                })
            })
            .collect();
        groupings.extend(longer);
    }
    groupings.sort();
    groupings.dedup();
    let mut checked = 0;
    for sizes in &groupings {
        for leading_receiver in [false, true] {
            let curried = CurriedArgs {
                leading_receiver,
                groups: sizes
                    .iter()
                    .map(|&n| ArgGroup {
                        args: (0..n).map(|_| CurriedArg { name: None }).collect(),
                    })
                    .collect(),
            };
            let needed = sizes.iter().sum::<usize>() + usize::from(leading_receiver);
            for len in 0..=needed + 2 {
                checked += 1;
                let params: Vec<usize> = (0..len).collect();
                let split = curried.split(&params);
                assert_eq!(split.is_some(), len == needed, "{sizes:?} over {len}");
                let Some(split) = split else { continue };
                assert_eq!(split.receiver.is_some(), leading_receiver);
                let rejoined: Vec<usize> = split
                    .receiver
                    .into_iter()
                    .chain(split.groups.iter().flat_map(|g| g.iter()))
                    .copied()
                    .collect();
                assert_eq!(rejoined, params, "{sizes:?}");
                let lens: Vec<usize> = split.groups.iter().map(|g| g.len()).collect();
                assert_eq!(&lens, sizes);
            }
        }
    }
    assert!(checked > 1_000, "only {checked} cases");
}
