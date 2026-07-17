//! Minimal repro for a name-resolution **precedence** bug found by the
//! whole-project differential (`crates/lsp/tests/all/resolve_real_project_diff.rs`)
//! against `WoofWare.PawPrint.Domain`:
//!
//! ```fsharp
//! open System
//! // ...
//! if String.Equals (name, resourceName, StringComparison.Ordinal) then
//! ```
//!
//! FCS resolves the `String` qualifier of `String.Equals(...)` to the **type**
//! `System.String` (assembly `System.Runtime`); we resolve it to the FSharp.Core
//! **module** `Microsoft.FSharp.Core.String` (the `String.length`/`String.concat`
//! functions module), which auto-opens under the implicitly-opened
//! `Microsoft.FSharp.Core`. `Equals` is a static method on the type, absent from
//! the module, so ours is a wrong go-to-definition on that qualifier.
//!
//! Both are legitimately in scope (`open System` brings the type; FSharp.Core's
//! `[<AutoOpen>]` brings the module), so this is a *precedence* question. F#'s
//! rule: for `String.Equals`, name resolution of the long-identifier qualifier
//! must consider the **type** `System.String` and find its static `Equals`, not
//! stop at the same-named module whose value set has no `Equals`.
//!
//! **What triggers it (localised while writing this test):** the explicit
//! `open Microsoft.FSharp.Core` *after* `open System`. With `open System` alone,
//! the qualifier resolves correctly to `System.String` — so the module's
//! auto-open is not enough on its own. It is the *later* explicit open that flips
//! the pick: our resolver applies latest-open-wins to the qualifier and lands on
//! the FSharp.Core `String` module, without checking that the member being
//! accessed (`Equals`) exists only on the `System.String` type. So the fault is
//! in the open-precedence step, not in member lookup.
//!
//! This is a **sema** bug, not a dependency-resolution one — the "we gave" side
//! names a FSharp.Core entity, which proves both FSharp.Core and System.Runtime
//! were read into the [`AssemblyEnv`] correctly; only the pick is wrong.
//!
//! Deterministic (no FCS): the env is the real, shipped `FSharp.Core.dll` +
//! `System.Runtime.dll`, and the test asserts the *correct* FCS answer directly,
//! so it fails today and will pass once the precedence is fixed. `#[ignore]`d so
//! it documents the bug without reddening the gate; run it explicitly:
//!
//! ```text
//! nix develop -c cargo test -p borzoi-sema --test all \
//!   resolve_string_qualifier_repro:: -- --ignored --nocapture
//! ```

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
#[ignore = "known sema precedence bug: the `String` qualifier of `String.Equals` \
            resolves to the FSharp.Core `String` module instead of `System.String`; \
            run with --ignored to reproduce"]
fn string_qualifier_of_static_call_resolves_to_the_bcl_type_not_the_fsharp_core_module() {
    // `open System` makes the type `System.String` a candidate; FSharp.Core's
    // `String` module is already in scope via its assembly `[<AutoOpen>]`. Only
    // the type carries a static `Equals`, so FCS resolves the qualifier to it.
    // `open Microsoft.FSharp.Core` *after* `open System` mirrors the real file
    // (WoofWare's `Assembly.fs` opens `System` then, later, `Microsoft.FSharp.Core`):
    // the FSharp.Core `String` module is re-introduced by a *later* open, and our
    // resolver lets latest-open-wins pick it even though the `Equals` member lives
    // only on the `System.String` type.
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
