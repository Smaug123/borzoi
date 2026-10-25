//! Exhaustive fact matrices for the assembly differential.
//!
//! `generative_source_diff` samples a random program per case, so a fact that
//! only some shapes exercise is reached by chance, if at all. The facts here are
//! the ones sema reads off the projection and whose every axis has a short,
//! enumerable domain, so each matrix renders the **whole** cross product into
//! one source file, compiles it once, and diffs the result against `fcs-dump
//! entities` through the same normaliser:
//!
//! - **union cases** (`Entity::union_cases`, folded into an `open`'s bare-name
//!   surface): representation access × struct × `[<RequireQualifiedAccess>]` ×
//!   a `[<CompiledName>]`-renamed case × namespace- or module-nested;
//! - **argument groups** (`MethodLike::arg_group_count`, read by the overload
//!   engine and the active-pattern splitter): module functions, class members
//!   and optional extension members, each over the group shapes a caller can
//!   write — none, `()`, one, curried, tupled, and mixed;
//! - **literals** (`Field::is_literal`, the constant-pattern test): every
//!   literal-expressible type, plus `decimal` (which the CLI cannot encode as a
//!   literal), an enum-typed literal and the accessibility filter;
//! - **getter accessibility** (`Property::getter_access`, which gates a typed
//!   read): every accessor-access combination on explicit and auto properties,
//!   static and instance, in F# and in C# (the two take different paths through
//!   `fcs-dump` — the F# one reads the pickle, the C# one the raw IL rows);
//! - **assembly-level `[<AutoOpen>]`** (`EcmaView::assembly_auto_opens`, the
//!   implicit opens a reference brings): the string form, the no-argument form,
//!   and a same-named attribute from another namespace, which must not count.
//!
//! Each matrix is one `dotnet build` and one `fcs-dump` run.

use std::fmt::Write as _;

use borzoi_assembly::test_support::{NormalisedAssembly, NormalisedEntity, NormalisedMember};

use crate::common::{
    ArgGroupObligation, Lang, assert_dll_projections_agree, compile_generated,
    diff_dll_expecting_overlay_skip,
};

const ACCESS: [&str; 3] = ["", "internal ", "private "];

fn union_matrix(out: &mut String) {
    let mut i = 0;
    for repr_access in ACCESS {
        for is_struct in [false, true] {
            for rqa in [false, true] {
                for renamed in [false, true] {
                    for nested in [false, true] {
                        let indent = if nested { "    " } else { "" };
                        if nested {
                            let _ = writeln!(out, "module UnionHost{i} =");
                        }
                        if is_struct {
                            let _ = writeln!(out, "{indent}[<Struct>]");
                        }
                        if rqa {
                            let _ = writeln!(out, "{indent}[<RequireQualifiedAccess>]");
                        }
                        let _ = writeln!(out, "{indent}type U{i} =");
                        if !repr_access.is_empty() {
                            let _ = writeln!(out, "{indent}    {}", repr_access.trim_end());
                        }
                        let rename = if renamed {
                            format!("[<CompiledName(\"Renamed{i}\")>] ")
                        } else {
                            String::new()
                        };
                        let _ = writeln!(out, "{indent}    | {rename}A");
                        let _ = writeln!(out, "{indent}    | B of b{i}: int");
                        let _ = writeln!(out, "{indent}    | C");
                        i += 1;
                    }
                }
            }
        }
    }
}

/// Parameter lists over the group shapes a caller can write. Each element is
/// one group; a group of more than one type is tupled.
const GROUP_SHAPES: &[&[&[&str]]] = &[
    &[&[]],
    &[&["int"]],
    &[&["int"], &["int"]],
    &[&["int", "string"]],
    &[&["int"], &["int", "string"]],
    &[&["int", "string"], &["int"], &["bool"]],
];

fn render_groups(shape: &[&[&str]], start: &mut usize) -> String {
    shape
        .iter()
        .map(|group| {
            if group.is_empty() {
                " ()".to_string()
            } else {
                let inner = group
                    .iter()
                    .map(|ty| {
                        let s = format!("p{}: {ty}", *start);
                        *start += 1;
                        s
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" ({inner})")
            }
        })
        .collect()
}

fn arg_group_matrix(out: &mut String) {
    let _ = writeln!(out, "module Groups =");
    let _ = writeln!(out, "    let value = 1");
    for (si, shape) in GROUP_SHAPES.iter().enumerate() {
        for (ai, access) in ["", "private "].iter().enumerate() {
            let mut p = 0;
            let params = render_groups(shape, &mut p);
            let _ = writeln!(out, "    let {access}f{si}_{ai}{params} = 0");
            let mut p = 0;
            let params = render_groups(shape, &mut p);
            let _ = writeln!(out, "    let {access}g{si}_{ai}<'T>{params} : 'T list = []");
        }
    }
    let _ = writeln!(out, "    type System.String with");
    for (si, shape) in GROUP_SHAPES.iter().enumerate() {
        let mut p = 0;
        let params = render_groups(shape, &mut p);
        let _ = writeln!(out, "        member x.Ext{si}{params} = 0");
        let mut p = 0;
        let params = render_groups(shape, &mut p);
        let _ = writeln!(out, "        static member SExt{si}{params} = 0");
    }
    let _ = writeln!(out, "type GroupHost() =");
    for (si, shape) in GROUP_SHAPES.iter().enumerate() {
        let mut p = 0;
        let params = render_groups(shape, &mut p);
        let _ = writeln!(out, "    member _.M{si}{params} = 0");
        let mut p = 0;
        let params = render_groups(shape, &mut p);
        let _ = writeln!(out, "    static member S{si}{params} = 0");
    }
}

fn literal_matrix(out: &mut String) {
    let _ = writeln!(out, "type LiteralEnum =");
    let _ = writeln!(out, "    | First = 0");
    let _ = writeln!(out, "    | Second = 1");
    let _ = writeln!(out, "module Literals =");
    let values = [
        "1",
        "\"s\"",
        "true",
        "'c'",
        "1uy",
        "1y",
        "1s",
        "1us",
        "1u",
        "1L",
        "1UL",
        "1.0f",
        "1.0",
        "1.5M",
        "LiteralEnum.Second",
    ];
    for (vi, v) in values.iter().enumerate() {
        for (ai, access) in ACCESS.iter().enumerate() {
            let _ = writeln!(out, "    [<Literal>]");
            let _ = writeln!(out, "    let {access}L{vi}_{ai} = {v}");
        }
    }
}

fn fsharp_property_matrix(out: &mut String) {
    let _ = writeln!(out, "type PropertyHost() =");
    let mut i = 0;
    for is_static in [false, true] {
        let head = if is_static {
            "static member"
        } else {
            "member _."
        };
        let sep = if is_static { " " } else { "" };
        for ga in ACCESS {
            let _ = writeln!(out, "    {head}{sep}G{i} with {ga}get () = 0");
            i += 1;
            for sa in ACCESS {
                let _ = writeln!(
                    out,
                    "    {head}{sep}GS{i} with {ga}get () = 0 and {sa}set (_: int) = ()"
                );
                i += 1;
            }
        }
        for sa in ACCESS {
            let _ = writeln!(out, "    {head}{sep}S{i} with {sa}set (_: int) = ()");
            i += 1;
        }
        let auto_head = if is_static {
            "static member val"
        } else {
            "member val"
        };
        for sa in ACCESS {
            let _ = writeln!(out, "    {auto_head} A{i} = 0 with get, {sa}set");
            i += 1;
        }
    }
}

fn fsharp_matrix_source() -> String {
    let mut out = String::from(
        "// Generated by crates/assembly/tests/all/fact_matrix_diff.rs — do not edit.\n\
         namespace Generated\n\
         \n\
         module AssemblyAttributes =\n\
         \x20   [<assembly: AutoOpen(\"Generated.Opened1\")>]\n\
         \x20   [<assembly: AutoOpen>]\n\
         \x20   [<assembly: AutoOpen(\"Generated.Opened0\")>]\n\
         \x20   [<assembly: AutoOpen(\"Generated.Opened1\")>]\n\
         \x20   do ()\n\
         \n\
         module Opened0 =\n    let zero = 0\n\
         module Opened1 =\n    let one = 1\n\n",
    );
    union_matrix(&mut out);
    arg_group_matrix(&mut out);
    literal_matrix(&mut out);
    fsharp_property_matrix(&mut out);
    out
}

/// C# accessibilities, each with the accessor modifiers C# admits under it —
/// an accessor modifier must be strictly more restrictive than the property's.
const CSHARP_ACCESS: [(&str, &[&str]); 5] = [
    (
        "public",
        &[
            "protected internal",
            "protected",
            "internal",
            "private protected",
            "private",
        ],
    ),
    (
        "protected internal",
        &["protected", "internal", "private protected", "private"],
    ),
    ("protected", &["private protected", "private"]),
    ("internal", &["private protected", "private"]),
    ("private protected", &["private"]),
];

fn csharp_matrix_source() -> String {
    let mut out = String::from(
        "// Generated by crates/assembly/tests/all/fact_matrix_diff.rs — do not edit.\n\
         namespace GeneratedCs;\n\
         \n\
         public class PropertyHost\n{\n",
    );
    let mut i = 0;
    for is_static in [false, true] {
        let st = if is_static { "static " } else { "" };
        for (prop, restricted) in CSHARP_ACCESS {
            let _ = writeln!(out, "    {prop} {st}int P{i} {{ get; set; }}");
            i += 1;
            let _ = writeln!(out, "    {prop} {st}int P{i} {{ get => 0; }}");
            i += 1;
            for accessor in restricted {
                let _ = writeln!(out, "    {prop} {st}int P{i} {{ {accessor} get; set; }}");
                i += 1;
                let _ = writeln!(out, "    {prop} {st}int P{i} {{ get; {accessor} set; }}");
                i += 1;
            }
        }
    }
    out.push_str("}\n");
    out
}

/// Every member of every entity in `n`, nested ones included, with its
/// entity's kind.
fn all_members(n: &NormalisedAssembly) -> Vec<(&str, &NormalisedMember)> {
    fn walk<'a>(es: &'a [NormalisedEntity], out: &mut Vec<(&'a str, &'a NormalisedMember)>) {
        for e in es {
            out.extend(e.members.iter().map(|m| (e.kind.as_str(), m)));
            walk(&e.nested_types, out);
        }
    }
    let mut out = Vec::new();
    walk(&n.entities, &mut out);
    out
}

fn all_entities(n: &NormalisedAssembly) -> Vec<&NormalisedEntity> {
    fn walk<'a>(es: &'a [NormalisedEntity], out: &mut Vec<&'a NormalisedEntity>) {
        for e in es {
            out.push(e);
            walk(&e.nested_types, out);
        }
    }
    let mut out = Vec::new();
    walk(&n.entities, &mut out);
    out
}

/// The getter-access fact is non-vacuous only where it differs from the
/// property's own (joined) access; count those.
fn properties_with_a_narrower_getter(n: &NormalisedAssembly) -> usize {
    all_members(n)
        .into_iter()
        .filter(|(_, m)| {
            m.kind == "Property" && m.getter_access.as_deref().is_some_and(|g| g != m.access)
        })
        .count()
}

#[test]
fn csharp_getter_access_matrix_agrees() {
    let source = csharp_matrix_source();
    let dll = compile_generated(Lang::CSharp, &source)
        .unwrap_or_else(|e| panic!("compile: {e}\n--- source ---\n{source}"));
    let agreed = assert_dll_projections_agree(&dll, ArgGroupObligation::AllCommit);
    // Each restricted *getter* that survives the visibility filter: under a
    // public property every modifier but `private protected`/`private` keeps
    // the getter distinct-and-visible-or-not, but the fact is compared for
    // every one of them. Pin the count so a matrix that stopped generating the
    // shape cannot pass vacuously.
    assert!(
        properties_with_a_narrower_getter(&agreed.ours) >= 20,
        "the C# matrix must exercise getters narrower than their property; saw {}",
        properties_with_a_narrower_getter(&agreed.ours),
    );
}

#[test]
fn fsharp_fact_matrix_agrees() {
    let source = fsharp_matrix_source();
    let dll = compile_generated(Lang::FSharp, &source)
        .unwrap_or_else(|e| panic!("compile: {e}\n--- source ---\n{source}"));
    let agreed = assert_dll_projections_agree(&dll, ArgGroupObligation::ModulesCommit);
    let ours = &agreed.ours;

    // Absolute pins: agreement shows only that the two readers match, so each
    // fact is also checked to be *present* in the shape the source declares.
    assert_eq!(
        ours.auto_opens,
        [
            "Generated.Opened1",
            "Generated.Opened0",
            "Generated.Opened1"
        ],
        "the string-form assembly AutoOpens, in manifest order, duplicates kept; the \
         no-argument form contributes nothing",
    );
    let unions: Vec<_> = all_entities(ours)
        .into_iter()
        .filter(|e| e.kind.ends_with("Union"))
        .collect();
    assert_eq!(unions.len(), 48, "the union matrix's every point projects");
    let with_cases = |cases: &[&str]| {
        unions
            .iter()
            .filter(|e| {
                e.union_cases
                    .as_ref()
                    .is_some_and(|cs| cs.iter().map(String::as_str).eq(cases.iter().copied()))
            })
            .count()
    };
    assert_eq!(
        with_cases(&["A", "B", "C"]),
        16,
        "a public representation lists its cases"
    );
    assert_eq!(
        with_cases(&[]),
        32,
        "a private or internal representation hides every case"
    );

    let literals = all_members(ours)
        .into_iter()
        .filter(|(_, m)| m.kind == "Field" && m.flags.contains("literal"))
        .count();
    // Public module literals of every non-decimal value (14), and the enum's
    // two cases.
    assert_eq!(
        literals, 16,
        "module literals and enum cases carry the literal flag"
    );

    let curried_module_members = all_members(ours)
        .into_iter()
        .filter(|(kind, m)| kind.ends_with("Module") && m.arg_groups.is_some_and(|g| g >= 2))
        .count();
    assert!(
        curried_module_members >= 8,
        "module functions and extension members commit curried counts; saw {curried_module_members}",
    );
    assert!(
        agreed
            .declines
            .iter()
            .any(|d| d.entity.ends_with("GroupHost") && d.member.starts_with("M2 ")),
        "a curried class member declines (no overlay covers type members), so the \
         obligation is exercised: {:#?}",
        agreed.declines,
    );
    assert!(
        properties_with_a_narrower_getter(ours) >= 4,
        "the F# matrix must exercise getters narrower than their property; saw {}",
        properties_with_a_narrower_getter(ours),
    );
}

/// An attribute named `AutoOpenAttribute` outside `Microsoft.FSharp.Core` is
/// not an AutoOpen: FCS recognises the attribute by its full type name.
///
/// The impostor has to be declared in the compiled assembly itself, and an
/// assembly-level attribute whose type the same assembly declares is a shape
/// the signature-pickle decoder refuses today (one tycon slot is referenced but
/// never declared, which FCS's own reader only warns about), so every F#
/// overlay is skipped here. The skip is required rather than tolerated: when
/// the decoder handles the shape this goes red, and the case belongs in
/// [`fsharp_fact_matrix_agrees`].
#[test]
fn an_impostor_auto_open_attribute_opens_nothing() {
    let source = "// Generated by crates/assembly/tests/all/fact_matrix_diff.rs — do not edit.\n\
                  namespace Impostor\n\
                  \n\
                  [<System.AttributeUsage(System.AttributeTargets.All, AllowMultiple = true)>]\n\
                  type AutoOpenAttribute(path: string) =\n    inherit System.Attribute()\n    member _.Path = path\n\
                  \n\
                  namespace Generated\n\
                  \n\
                  module AssemblyAttributes =\n\
                  \x20   [<assembly: Impostor.AutoOpen(\"Generated.Impostor\")>]\n\
                  \x20   [<assembly: AutoOpen(\"Generated.Real\")>]\n\
                  \x20   do ()\n";
    let dll = compile_generated(Lang::FSharp, source)
        .unwrap_or_else(|e| panic!("compile: {e}\n--- source ---\n{source}"));
    let agreed = diff_dll_expecting_overlay_skip(
        &dll,
        ArgGroupObligation::ModulesCommit,
        "was never linked by u_osgn_decl",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(agreed.ours.auto_opens, ["Generated.Real"]);
}
