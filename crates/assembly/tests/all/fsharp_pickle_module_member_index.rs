//! Always-on integration test: the module member-val index
//! (`collect_module_member_targets`, pickle member-projection Slice B) built
//! from the *real, shipped* `FSharp.Core.dll`.
//!
//! Pins the index facts the Slice C member-list cutover will consume, on the
//! two modules the plan names (`docs/completed/fsharp-pickle-member-projection-plan.md`
//! §3 Slice B): `Microsoft.FSharp.Core.Operators` (generic operator vals with
//! no explicit compiled name) and `…Core.PrintfModule` (a suffix module whose
//! vals are all `[<CompiledName>]`-renamed, including the `sprintf`/`ksprintf`
//! compiled-name collision that only arity can break).
//!
//! Expected values were pinned by probing the decoded pickle directly (not by
//! running the index), so this test is an independent oracle for the walk's
//! namespace/type-chain split and the per-val projection.
//!
//! Requires the .NET 10 SDK on PATH (to build `tools/fcs-dump` once); the Nix
//! devShell provides it.

use borzoi_assembly::{
    Ecma335Assembly, EcmaView, FSharpResource, ModuleMemberTarget, ResourceKind,
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

fn fsharp_core_targets() -> Vec<ModuleMemberTarget> {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse FSharp.Core");
    let resources = view
        .fsharp_resources()
        .expect("fsharp_resources on FSharp.Core");
    let ccu = unpickle_signature(&primary(&resources).payload, b_stream(&resources))
        .expect("unpickle real FSharp.Core signature");
    collect_module_member_targets(&ccu).expect("collect_module_member_targets on FSharp.Core")
}

/// Find the single target at `namespace` with the given type chain.
fn module_target<'a>(
    targets: &'a [ModuleMemberTarget],
    namespace: &[&str],
    type_chain: &[&str],
) -> &'a ModuleMemberTarget {
    let mut found = targets
        .iter()
        .filter(|t| t.namespace == namespace && t.type_chain == type_chain);
    let first = found
        .next()
        .unwrap_or_else(|| panic!("no target for {namespace:?} {type_chain:?}"));
    assert!(
        found.next().is_none(),
        "multiple targets for {namespace:?} {type_chain:?}"
    );
    first
}

/// Find the single target for a top-level `Microsoft.FSharp.Core` module.
fn core_module<'a>(targets: &'a [ModuleMemberTarget], name: &str) -> &'a ModuleMemberTarget {
    module_target(targets, &["Microsoft", "FSharp", "Core"], &[name])
}

#[test]
fn indexes_real_fsharp_core_operators_and_printf() {
    let targets = fsharp_core_targets();

    // `Operators`: a plain (non-suffix) module of mostly-generic vals. The
    // operator vals carry no explicit compiled name — their IL name *is* the
    // mangled logical name (`op_Addition`), the surface A4/S4 demangling will
    // later map back to `+`.
    let operators = core_module(&targets, "Operators");
    assert!(
        operators.vals.len() > 100,
        "Operators has {} vals; expected the full module surface",
        operators.vals.len()
    );
    let addition = operators
        .vals
        .iter()
        .find(|v| v.logical_name == "op_Addition")
        .expect("Operators has op_Addition");
    assert_eq!(addition.compiled_name, None);
    assert_eq!(addition.il_name(), "op_Addition");
    assert_eq!(addition.il_arity, Some(2));
    assert!(addition.is_generic, "`(+)` is generic");
    assert!(!addition.is_instance);
    assert!(!addition.is_extension);

    // A renamed val in the same module: `let raise (e: exn)` compiles to
    // `Raise`.
    let raise = operators
        .vals
        .iter()
        .find(|v| v.logical_name == "raise")
        .expect("Operators has raise");
    assert_eq!(raise.compiled_name.as_deref(), Some("Raise"));
    assert_eq!(raise.il_name(), "Raise");
    assert_eq!(raise.il_arity, Some(1));

    // `PrintfModule`: the `Printf` module compiles with the `Module` suffix
    // (the type chain holds the CLR name; the source name `Printf` is the
    // source-name overlay's business, not this index's), and every val is
    // `[<CompiledName>]`-renamed and generic.
    let printf = core_module(&targets, "PrintfModule");
    assert_eq!(printf.vals.len(), 13);
    assert!(printf.vals.iter().all(|v| v.is_generic));
    assert!(printf.vals.iter().all(|v| v.compiled_name.is_some()));

    let printfn = printf
        .vals
        .iter()
        .find(|v| v.logical_name == "printfn")
        .expect("PrintfModule has printfn");
    assert_eq!(printfn.compiled_name.as_deref(), Some("PrintFormatLine"));
    assert_eq!(printfn.il_arity, Some(1));
    assert!(!printfn.is_instance && !printfn.is_extension);

    // The compiled-name collision the consumer must break by arity: `sprintf`
    // and `ksprintf` both compile to `PrintFormatToStringThen`, at arity 1
    // and 2 respectively. The index keeps both, in pickle order.
    let collision: Vec<_> = printf
        .vals
        .iter()
        .filter(|v| v.il_name() == "PrintFormatToStringThen")
        .collect();
    assert_eq!(
        collision
            .iter()
            .map(|v| (v.logical_name.as_str(), v.il_arity))
            .collect::<Vec<_>>(),
        [("sprintf", Some(1)), ("ksprintf", Some(2))]
    );
}

/// The two IL-shape facts a naive val read gets wrong, pinned on real
/// FSharp.Core: a *value* binding pickles zero argument groups (fsc emits a
/// static property, not a MethodDef of the val's name), and measure-only
/// genericity is erased from IL.
#[test]
fn indexes_real_fsharp_core_value_shapes() {
    let targets = fsharp_core_targets();

    // `ExtraTopLevelOperators.async` is `let async = AsyncBuilder()` renamed
    // to `DefaultAsyncBuilder` — a value binding: zero curried groups, so its
    // IL artefact is the `DefaultAsyncBuilder` static property (getter
    // `get_DefaultAsyncBuilder`), which shares `il_arity = Some(0)` with any
    // unit-taking function.
    let extra = core_module(&targets, "ExtraTopLevelOperators");
    let async_val = extra
        .vals
        .iter()
        .find(|v| v.logical_name == "async")
        .expect("ExtraTopLevelOperators has async");
    assert_eq!(
        async_val.compiled_name.as_deref(),
        Some("DefaultAsyncBuilder")
    );
    assert_eq!(async_val.arg_group_count, Some(0));
    assert_eq!(async_val.il_arity, Some(0));
    assert!(!async_val.is_literal);

    // `ExperimentalAttributeMessages.RequiresPreview` is a `[<Literal>]`:
    // a value binding with a pickled constant — no MethodDef, no property.
    let messages = core_module(&targets, "ExperimentalAttributeMessages");
    let literal = messages
        .vals
        .iter()
        .find(|v| v.logical_name == "RequiresPreview")
        .expect("ExperimentalAttributeMessages has RequiresPreview");
    assert!(literal.is_literal);
    assert_eq!(literal.arg_group_count, Some(0));

    // `LanguagePrimitives.FloatWithMeasure : float -> float<'m>` is generic
    // only over a measure; the measure typar is erased from IL, so the
    // MethodDef has zero generic parameters and the index must not report it
    // IL-generic. `op_Addition` (checked in the other test) pins the converse.
    let lang = core_module(&targets, "LanguagePrimitives");
    let with_measure = lang
        .vals
        .iter()
        .find(|v| v.logical_name == "FloatWithMeasure")
        .expect("LanguagePrimitives has FloatWithMeasure");
    assert!(!with_measure.is_generic);
    assert_eq!(with_measure.arg_group_count, Some(1));
    assert_eq!(with_measure.il_arity, Some(1));
}

/// FCS stores the intrinsic members of module-nested types in the module's
/// own val list; the index must not surface them as module members. Pinned on
/// `CompilerServices.RuntimeHelpers`, whose pickled vals carry the nested
/// `StructBox`'s `.ctor`/`get_Value`/`get_Comparer` next to the module's real
/// functions.
#[test]
fn indexes_real_fsharp_core_excludes_nested_type_members() {
    let targets = fsharp_core_targets();
    let helpers = module_target(
        &targets,
        &["Microsoft", "FSharp", "Core", "CompilerServices"],
        &["RuntimeHelpers"],
    );
    let names: Vec<&str> = helpers
        .vals
        .iter()
        .map(|v| v.logical_name.as_str())
        .collect();
    assert!(
        names.contains(&"EnumerateWhile") && names.contains(&"CreateEvent"),
        "RuntimeHelpers module functions missing from {names:?}"
    );
    for nested_member in [".ctor", "get_Value", "get_Comparer"] {
        assert!(
            !names.contains(&nested_member),
            "nested StructBox member {nested_member:?} leaked into the module list: {names:?}"
        );
    }
}

/// A same-`(il_name, il_arity)` overload set is real: the
/// `TaskBuilderExtensions.MediumPriority` module (`TaskBuilderExtensions`
/// pickles as a namespace fragment) holds five `TaskBuilder.MergeSources`
/// instance-extension vals at arity 3. The index keeps them all, and their
/// `val_index` handles stay distinct — the hook a consumer needs for the
/// signature-level matching that arity cannot do.
#[test]
fn indexes_real_fsharp_core_same_arity_overloads() {
    let targets = fsharp_core_targets();
    let medium = module_target(
        &targets,
        &["Microsoft", "FSharp", "Control", "TaskBuilderExtensions"],
        &["MediumPriority"],
    );
    let merge_sources: Vec<_> = medium
        .vals
        .iter()
        .filter(|v| v.il_name() == "TaskBuilder.MergeSources")
        .collect();
    assert_eq!(
        merge_sources.len(),
        5,
        "expected five MergeSources overloads"
    );
    assert!(
        merge_sources
            .iter()
            .all(|v| v.il_arity == Some(3) && v.is_extension && v.is_instance)
    );
    let mut stamps: Vec<u32> = merge_sources.iter().map(|v| v.val_index).collect();
    stamps.sort_unstable();
    stamps.dedup();
    assert_eq!(stamps.len(), 5, "val_index handles must stay distinct");
}
