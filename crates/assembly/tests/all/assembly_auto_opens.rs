//! `EcmaView::assembly_auto_opens` — the assembly-level `[<AutoOpen("path")>]`
//! list read from the manifest's custom attributes (plan
//! `docs/fsharp-core-autoopen-resolution-plan.md`, Stage A3; the analogue of
//! FCS's `GetAutoOpenAttributes`, `CompilerImports.fs`).
//!
//! The real `FSharp.Core.dll` is the load-bearing target: its assembly-level
//! AutoOpen list is what drives the compiler's implicit opens (there is no
//! hardcoded list in FCS), so the exact paths *and their manifest order* are
//! pinned — order is the order FCS applies the opens in, which decides
//! shadowing among them. These are stable F# API facts, like the rest of
//! `projector_fsharp_core.rs`.
//!
//! Requires the .NET 10 SDK on PATH (to build `tools/fcs-dump` once, which
//! drops the `FSharp.Core.dll` this reads); the Nix devShell provides it.

use borzoi_assembly::{Ecma335Assembly, EcmaView};

use crate::common::{ensure_fsharp_core_dll, ensure_minilib_built, ensure_minilib_fs_built};

fn view_of(path: &std::path::Path) -> Ecma335Assembly {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    Ecma335Assembly::parse(&bytes).unwrap_or_else(|e| panic!("parse {path:?}: {e:?}"))
}

/// The genuine shipped FSharp.Core carries the full assembly-level AutoOpen
/// set, in this manifest order (verified independently via
/// `System.Reflection.Metadata` over the same DLL). Note the mix: most name
/// a **namespace**, but `…LanguagePrimitives.IntrinsicOperators` and the
/// TaskBuilder/QueryRun priority entries name **modules** — the trait returns
/// the dotted paths verbatim; namespace-vs-module classification is the
/// consumer's (sema's) job.
#[test]
fn fsharp_core_assembly_auto_opens_exact_ordered_list() {
    let view = view_of(&ensure_fsharp_core_dll());
    let auto_opens = view
        .assembly_auto_opens()
        .expect("FSharp.Core manifest attributes must decode");
    assert_eq!(
        auto_opens,
        [
            "Microsoft.FSharp",
            "Microsoft.FSharp.Core.LanguagePrimitives.IntrinsicOperators",
            "Microsoft.FSharp.Core",
            "Microsoft.FSharp.Collections",
            "Microsoft.FSharp.Control",
            "Microsoft.FSharp.Control.TaskBuilderExtensions.LowPriority",
            "Microsoft.FSharp.Control.TaskBuilderExtensions.LowPlusPriority",
            "Microsoft.FSharp.Control.TaskBuilderExtensions.MediumPriority",
            "Microsoft.FSharp.Control.TaskBuilderExtensions.HighPriority",
            "Microsoft.FSharp.Linq.QueryRunExtensions.LowPriority",
            "Microsoft.FSharp.Linq.QueryRunExtensions.HighPriority",
        ]
        .map(String::from)
    );
}

/// A C# assembly has no `[<assembly: AutoOpen>]` (the attribute type is
/// FSharp.Core's) — the list is empty, not an error.
#[test]
fn csharp_assembly_has_no_auto_opens() {
    let view = view_of(ensure_minilib_built());
    assert_eq!(view.assembly_auto_opens().unwrap(), Vec::<String>::new());
}

/// An F# assembly that declares only *module-level* `[<AutoOpen>]` (MiniLibFs
/// has one) contributes nothing at assembly level: the manifest list is
/// exactly the `[<assembly: AutoOpen("…")>]` occurrences, not the per-module
/// markers (`Entity::is_auto_open` carries those).
#[test]
fn fsharp_module_level_auto_open_does_not_leak_into_assembly_list() {
    let view = view_of(ensure_minilib_fs_built());
    assert_eq!(view.assembly_auto_opens().unwrap(), Vec::<String>::new());
}
