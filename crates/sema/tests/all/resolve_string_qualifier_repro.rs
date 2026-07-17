//! End-to-end pin for the name-resolution **precedence** rule a whole-project
//! differential divergence (`crates/lsp/tests/all/resolve_real_project_diff.rs`,
//! against `WoofWare.PawPrint.Domain`) once caught us breaking:
//!
//! ```fsharp
//! open System
//! // ...
//! if String.Equals (name, resourceName, StringComparison.Ordinal) then
//! ```
//!
//! Both `String` candidates are legitimately in scope (`open System` brings the
//! **type** `System.String`; FSharp.Core's `[<AutoOpen>]` — and here an explicit
//! later `open Microsoft.FSharp.Core` — brings the `String.length`/`String.concat`
//! functions **module**), so the qualifier pick is a precedence question. F#'s
//! rule (`ResolveExprLongIdentPrim`, NameResolution.fs): the module search runs
//! first, but a module reading whose member lookup *fails inside the module*
//! razes `UndefinedName` and does **not** own the path — `AtMostOneResultQuery`
//! lets the type search re-root it, and `System.String`'s static `Equals` wins.
//! FCS therefore resolves the qualifier to the type (assembly `System.Runtime`).
//!
//! We used to get this wrong: [`AssemblyEnv::static_lookup`]'s ownership
//! fall-through treated the module like a class and walked the compiled class's
//! base chain, where `Object`'s `Equals` made the name look "occupied" — so the
//! later-open module reading wrongly owned the path (recording the FSharp.Core
//! `String` module at the qualifier, a wrong go-to-definition) and the
//! `open System` tier was never consulted. `module_qualified_occupied` now
//! restricts a module receiver to FCS's in-module search domain; the sibling
//! `static_lookup` pins live in `assembly_env.rs`
//! (`static_lookup_on_a_module_ignores_object_members`).
//!
//! Deterministic (no FCS): the env is the real, shipped `FSharp.Core.dll` +
//! `System.Runtime.dll`, and the test asserts the FCS answer directly (pinned by
//! the whole-project differential above).

use crate::common::{ensure_fsharp_core_dll, ensure_system_runtime_dll};

use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, resolve_file};
use rowan::TextRange;

/// An [`AssemblyEnv`] over the real, shipped `FSharp.Core.dll` **and** a real BCL
/// `System.Runtime.dll`, so both `String` candidates — the FSharp.Core `String`
/// module and the `System.String` type — are present and in scope (FSharp.Core's
/// auto-opens are applied by `from_views`).
fn fsharp_core_plus_bcl_env() -> AssemblyEnv {
    let core_bytes = std::fs::read(ensure_fsharp_core_dll()).expect("read FSharp.Core.dll");
    let bcl_bytes = std::fs::read(ensure_system_runtime_dll()).expect("read System.Runtime.dll");
    let core = Ecma335Assembly::parse(&core_bytes).expect("parse FSharp.Core.dll");
    let bcl = Ecma335Assembly::parse(&bcl_bytes).expect("parse System.Runtime.dll");
    AssemblyEnv::from_views(&[core, bcl])
        .expect("build AssemblyEnv over FSharp.Core + System.Runtime")
}

/// Byte range of `needle`'s first occurrence in `src`.
fn at(src: &str, needle: &str) -> TextRange {
    let i = src
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {src:?}"));
    TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + needle.len()).unwrap().into(),
    )
}

#[test]
fn string_qualifier_of_static_call_resolves_to_the_bcl_type_not_the_fsharp_core_module() {
    // `open System` makes the type `System.String` a candidate; FSharp.Core's
    // `String` module is already in scope via its assembly `[<AutoOpen>]`. Only
    // the type carries a static `Equals`, so FCS resolves the qualifier to it.
    // `open Microsoft.FSharp.Core` *after* `open System` mirrors the real file
    // (WoofWare's `Assembly.fs` opens `System` then, later, `Microsoft.FSharp.Core`):
    // the FSharp.Core `String` module is re-introduced by a *later* open, so its
    // reading is the tier tried first — the member-absent module must not own the
    // path there.
    let src =
        "module M\nopen System\nopen Microsoft.FSharp.Core\nlet b = String.Equals (\"a\", \"b\")\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");

    let env = fsharp_core_plus_bcl_env();
    let rf = resolve_file(&file, &ProjectItems::default(), &env);

    // The `String` qualifier token of `String.Equals`.
    let head = at(src, "String");
    let res = rf
        .resolution_at(head)
        .expect("the `String` qualifier resolves to something");

    let Resolution::Entity(h) = res else {
        panic!("expected `String` to resolve to an assembly Entity, got {res:?}");
    };
    let entity = env.entity(h);
    let qualified = if entity.namespace.is_empty() {
        entity.name.clone()
    } else {
        format!("{}.{}", entity.namespace.join("."), entity.name)
    };

    assert_eq!(
        (entity.assembly.name.as_str(), qualified.as_str()),
        ("System.Runtime", "System.String"),
        "the `String` qualifier of a static `String.Equals` call must resolve to \
         the `System.String` type, not the FSharp.Core `String` module",
    );
}
