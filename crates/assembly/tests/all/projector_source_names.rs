//! F# *source name* recovery from ECMA-335 metadata against the real
//! `MiniLibFs.dll`, read through `Ecma335Assembly::parse`.
//!
//! Two facts, both needed before the sema layer can resolve unqualified F#
//! names (`printfn`, `List.map`) that the IL renames away:
//!
//!   - **Member source name** — `[<CompiledName("RenamedAtIl")>] let renamed`
//!     compiles to an IL method `RenamedAtIl` carrying
//!     `[CompilationSourceName("renamed")]`. The projector keeps
//!     `MethodLike::name = "RenamedAtIl"` (the IL/`CompiledName`, what the
//!     differential compares) and surfaces `source_name = Some("renamed")`.
//!
//!   - **Module source name** — `[<CompilationRepresentation(ModuleSuffix)>]`
//!     compiles `module Suffixed` to an IL class `SuffixedModule`; the
//!     projector records `Entity::source_name = Some("Suffixed")` by stripping
//!     the `"Module"` suffix (FSharp.Core's `List` ⇒ `ListModule` shape).
//!
//! These are absolute value pins. `assembly_diff` proves the Rust and FCS
//! projections *agree* (FCS renders entities by `DisplayName` = source name,
//! members by `CompiledName`), but agreement alone would survive both sides
//! dropping the source name, so this file pins the concrete `Some`/`None`
//! values.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{
    Ecma335Assembly, EcmaView, Entity, Member, MethodLike, ModuleValue, ParamDefault,
};

use crate::common::ensure_minilib_fs_built;

fn load() -> Vec<Entity> {
    let dll = ensure_minilib_fs_built();
    let bytes = std::fs::read(dll).expect("read MiniLibFs.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse MiniLibFs");
    view.enumerate_type_defs()
        .expect("enumerate MiniLibFs types")
}

fn entity<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
    entities.iter().find(|e| e.name == name).unwrap_or_else(|| {
        panic!(
            "entity {name:?} not found among {:?}",
            entities.iter().map(|e| &e.name).collect::<Vec<_>>()
        )
    })
}

fn method<'a>(e: &'a Entity, il_name: &str) -> &'a MethodLike {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Method(m) if m.name == il_name => Some(m),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "method {il_name:?} not found on {:?}; methods: {:?}",
                e.name,
                e.members
                    .iter()
                    .filter_map(|m| match m {
                        Member::Method(m) => Some(&m.name),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            )
        })
}

#[test]
fn module_let_value_vs_function_carries_module_value() {
    // `module Hello` mixes `let` values and functions. A value is rebranded from
    // its property getter to a 0-parameter method tagged `module_value`; a
    // function (including the unit-taking `let ping ()`) stays a plain method.
    let entities = load();
    let hello = entity(&entities, "Hello");

    // `let answer = 42` — a value.
    assert_eq!(
        method(hello, "answer").module_value,
        Some(ModuleValue { is_mutable: false })
    );
    // `let mutable counter = 0` — a mutable value (the rebrand recovers the
    // setter from the get/set property).
    assert_eq!(
        method(hello, "counter").module_value,
        Some(ModuleValue { is_mutable: true })
    );
    // `let inc x = …` (a function) and `let ping () = …` (a unit-function) are
    // genuine methods, not values.
    assert_eq!(method(hello, "inc").module_value, None);
    assert_eq!(method(hello, "ping").module_value, None);
}

#[test]
fn fsharp_optional_parameter_classifies_as_fsharp_optional() {
    // `member _.WithOptional(?count: int)` compiles to a parameter typed
    // `FSharpOption<int>` carrying `[<OptionalArgument>]`; the projector
    // classifies it as the F# optional form, distinct from a .NET `[Optional]`.
    let entities = load();
    let host = entity(&entities, "OptionalArgHost");
    let m = method(host, "WithOptional");
    let p = &m.signature.parameters[0];
    assert_eq!(p.default, ParamDefault::FSharpOptional);
}

#[test]
fn compiled_name_member_surfaces_fsharp_source_name() {
    // `[<CompiledName("RenamedAtIl")>] let renamed x = x + 2`: the IL name is
    // the compiled name, `source_name` is the F# identifier.
    let entities = load();
    let renamed = method(entity(&entities, "Hello"), "RenamedAtIl");
    assert_eq!(renamed.source_name.as_deref(), Some("renamed"));
}

#[test]
fn unrenamed_member_has_no_source_name() {
    // Negative control: a plain `let inc x = x + 1` is not renamed, so it
    // carries no `CompilationSourceName` and `source_name` stays `None` (the
    // IL name already *is* the source name).
    let entities = load();
    let inc = method(entity(&entities, "Hello"), "inc");
    assert_eq!(inc.source_name, None);
}

#[test]
fn module_suffix_entity_surfaces_stripped_source_name() {
    // `[<CompilationRepresentation(ModuleSuffix)>] module Suffixed`: IL class
    // `SuffixedModule`, F# source name `Suffixed`.
    let entities = load();
    let suffixed = entity(&entities, "SuffixedModule");
    assert_eq!(suffixed.source_name.as_deref(), Some("Suffixed"));
    // The member inside doubles as a second member-source-name pin.
    let make = method(suffixed, "Make");
    assert_eq!(make.source_name.as_deref(), Some("create"));
}

#[test]
fn fsharp_witness_method_is_dropped_but_real_generic_member_kept() {
    // `module Witness` has `let inline addThem (x: ^a) (y: ^a)`, which the F#
    // compiler emits as the real `addThem` plus an `addThem$W` witness twin. The
    // projector keeps the real (generic) member — name resolution needs it — but
    // drops the `$W` duplicate, which otherwise collides on the shared source
    // name and makes the name ambiguous.
    let entities = load();
    let witness = entity(&entities, "Witness");
    let method_names: Vec<&str> = witness
        .members
        .iter()
        .filter_map(|m| match m {
            Member::Method(x) => Some(x.name.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        method_names.contains(&"addThem"),
        "the real generic member must be kept; got {method_names:?}"
    );
    assert!(
        !method_names.contains(&"addThem$W"),
        "the `$W` witness twin must be dropped; got {method_names:?}"
    );
    // A real member whose compiled name merely embeds `$W` (`Keep$Wrapper`) must
    // survive — the filter matches the `$W` suffix, not a substring.
    assert!(
        method_names.contains(&"Keep$Wrapper"),
        "a non-witness member embedding `$W` must be kept; got {method_names:?}"
    );
    // A lone `$W`-suffixed member with no real sibling (`Lone$W`, no `Lone`) is a
    // genuine method, not a witness duplicate — it must survive.
    assert!(
        method_names.contains(&"Lone$W"),
        "a lone `$W` member with no sibling must be kept; got {method_names:?}"
    );
}

#[test]
fn non_suffixed_module_has_no_source_name() {
    // Negative control: `module Hello` is not suffixed, so its IL name *is*
    // the source name and `source_name` stays `None`.
    let entities = load();
    assert_eq!(entity(&entities, "Hello").source_name, None);
}
