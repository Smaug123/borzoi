//! The end-goal validation: bare `printfn` resolves against the *real*,
//! shipped `FSharp.Core.dll` — not the `autoopen_env` fixture that stands in
//! for it in `resolve_autoopen.rs`.
//!
//! This is the payoff of the whole assembly-reader + pickle-overlay stack. For
//! the resolver to land `printfn` on its IL `PrintFormat*` method it needs every
//! layer to agree on the genuine assembly:
//!   1. `enumerate_type_defs` must walk all of FSharp.Core's member signatures
//!      (the multi-dimensional-array / pointer IL-reader work);
//!   2. the auto-open overlay must flag `ExtraTopLevelOperators` `[<AutoOpen>]`
//!      so its members enter unqualified scope under the implicitly-opened
//!      `Microsoft.FSharp.Core`;
//!   3. the source-name overlay must recover `printfn` (the F# name) for the
//!      renamed IL method, so the bare identifier matches.
//!
//! `resolve_autoopen.rs` pins this shape against a hand-built fixture; this pins
//! it against the article itself, closing the gap between "the fixture mimics
//! FSharp.Core" and "FSharp.Core actually resolves".
//!
//! Requires the .NET 10 SDK on PATH (to build `tools/fcs-dump` once, which drops
//! the `FSharp.Core.dll` this reads); the Nix devShell provides it.

use crate::common::ensure_fsharp_core_dll;

use borzoi_assembly::{Ecma335Assembly, Member};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, DeferredReason, ProjectItems, Resolution, ResolvedFile, SemanticClass,
    resolve_file, resolve_project,
};
use rowan::TextRange;

/// Build an [`AssemblyEnv`] over the real, shipped FSharp.Core (parsed once per
/// test binary). `from_views` runs the single-CCU authoritative projection, so
/// the source-name and auto-open overlays are applied.
fn fsharp_core_env() -> AssemblyEnv {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core.dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view))
        .expect("FSharp.Core must project end-to-end into an AssemblyEnv")
}

fn impl_file(src: &str) -> ImplFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        p.errors
    );
    ImplFile::cast(p.root).expect("impl file")
}

fn resolve(src: &str, env: &AssemblyEnv) -> ResolvedFile {
    resolve_file(&impl_file(src), &ProjectItems::default(), env)
}

/// Range of `needle`'s only occurrence in `hay`.
fn at(hay: &str, needle: &str) -> TextRange {
    let s = hay
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {hay:?}"));
    let end = s + needle.len();
    TextRange::new(
        u32::try_from(s).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// Range of the `n`th (0-based) occurrence of `needle` in `hay`.
fn nth(hay: &str, needle: &str, n: usize) -> TextRange {
    let mut from = 0;
    for _ in 0..n {
        from = hay[from..].find(needle).expect("occurrence") + from + needle.len();
    }
    let s = hay[from..].find(needle).expect("occurrence") + from;
    TextRange::new(
        u32::try_from(s).unwrap().into(),
        u32::try_from(s + needle.len()).unwrap().into(),
    )
}

fn il_name(m: &Member) -> &str {
    match m {
        Member::Method(x) => &x.name,
        Member::Field(x) => &x.name,
        Member::Property(x) => &x.name,
        Member::Event(x) => &x.name,
    }
}

/// The `Microsoft.FSharp.Core.<name>` (non-generic) entity in the env.
fn core(env: &AssemblyEnv, name: &str) -> borzoi_sema::EntityHandle {
    env.lookup_type(
        &["Microsoft".into(), "FSharp".into(), "Core".into()],
        name,
        0,
    )
    .unwrap_or_else(|| panic!("real FSharp.Core must declare Microsoft.FSharp.Core.{name}"))
}

#[test]
fn bare_printfn_resolves_into_real_fsharp_core() {
    // `printfn` is a static of the auto-open `ExtraTopLevelOperators` module in
    // the implicitly-opened `Microsoft.FSharp.Core` namespace, so it resolves
    // with no `open`. Its IL method is a `PrintFormat*` overload, reached by the
    // F# source name the pickle overlay recovered. This is the milestone the
    // `autoopen_env` fixture stands in for — here against the real assembly.
    let env = fsharp_core_env();
    let src = "let test () = printfn \"%d\" 1\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "printfn")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(
                parent,
                core(&env, "ExtraTopLevelOperators"),
                "printfn must resolve into ExtraTopLevelOperators"
            );
            let il = il_name(env.member_at(parent, idx));
            assert!(
                il.starts_with("PrintFormat"),
                "printfn compiles to a PrintFormat* IL method; got {il:?}"
            );
        }
        other => panic!("expected Member for bare `printfn`, got {other:?}"),
    }
}

#[test]
fn bare_sprintf_resolves_into_real_fsharp_core() {
    // A second auto-open printf entry, exercising the arity-disambiguated
    // source-name recovery (`sprintf` vs `ksprintf` share the IL stem).
    let env = fsharp_core_env();
    let src = "let test () = sprintf \"%d\" 1\n";
    let rf = resolve(src, &env);
    match rf.resolution_at(at(src, "sprintf")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(
                parent,
                core(&env, "ExtraTopLevelOperators"),
                "sprintf must resolve into ExtraTopLevelOperators"
            );
            let il = il_name(env.member_at(parent, idx));
            assert!(
                il.starts_with("PrintFormat"),
                "sprintf compiles to a PrintFormat* IL method; got {il:?}"
            );
        }
        other => panic!("expected Member for bare `sprintf`, got {other:?}"),
    }
}

#[test]
fn bare_primitive_annotation_resolves_against_real_fsharp_core() {
    // A bare `int64` annotation head binds FSharp.Core's own abbreviation
    // marker under the implicit `Microsoft.FSharp.Core` open — the mechanism
    // that replaced the hard-coded primitive-alias table. With the full BCL
    // closure loaded the marker's target chases, so the head records the
    // marker entity (what FCS names at the use).
    //
    // (This supersedes the R2-era pin that `int64` *recorded nothing* here —
    // that "no shadow possible" signal existed for the alias-table gate,
    // which is gone. The auto-open shadow regression it guarded — a
    // children-presence check deferring every bare annotation — would now
    // surface as a `ShadowableType` defer instead of the marker entity, so
    // this assertion still catches it.)
    let env = crate::common::full_bcl_env();
    let src = "module M\nlet f (x : int64) = x\n";
    let rf = resolve(src, env);
    let marker = core(env, "int64");
    assert!(env.is_abbreviation(marker), "int64 is a marker");
    assert_eq!(
        rf.resolution_at(at(src, "int64")),
        Some(Resolution::Entity(marker)),
        "a bare primitive annotation binds FSharp.Core's abbreviation marker"
    );

    // Without the BCL half of the closure the target cannot chase, and the
    // marker honestly defers — bounded uncertainty, never a wrong target.
    let core_only = fsharp_core_env();
    let rf = resolve(src, &core_only);
    assert_eq!(
        rf.resolution_at(at(src, "int64")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "with FSharp.Core alone the chase declines and the marker defers"
    );
}

#[test]
fn real_fsharp_core_auto_open_module_shadow_is_name_keyed() {
    // The flip side of the regression above, pinned against the real assembly
    // rather than a hand-built fixture: `Checked` (and `Unchecked`) *are*
    // public nested modules of the auto-open `Operators` module in
    // `Microsoft.FSharp.Core`, so a bare annotation actually named like one of
    // them must still defer — the fix must key on the requested name, not drop
    // the check entirely.
    let env = fsharp_core_env();
    let src = "module M\nlet f (x : Checked) = x\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Checked")),
        Some(Resolution::Deferred(DeferredReason::ShadowableType)),
        "Checked is a real accessible nested module of the auto-open Operators module"
    );
}

// ===== Assembly active-pattern shape against real FSharp.Core (Stage 3b) =====
//
// `(|KeyValue|)` and `(|Failure|_|)` are recognizers in the auto-open
// `Microsoft.FSharp.Core.Operators` module. Stage 3b attaches a demangled shape
// to assembly recognizers folded in through an **explicit** `open <module>` /
// `open <namespace>`. FSharp.Core's Operators is reached instead through the
// *implicit* `[<assembly: AutoOpen>]` path (`open_type_statics`), which Stage 3b
// deliberately does NOT touch — that path lacks the fold's residue / collision /
// constant-pattern-shadow demotions, so trusting a shape there could be a wrong
// commit (codex round 4). It keeps today's sound behaviour: the recognizer's
// cases are not folded into pattern scope, so a bare pattern use declines. Making
// the implicit path fold recognizers soundly is a documented follow-up
// (`docs/export-decl-model-plan.md` Stage 3b).

#[test]
fn real_fsharp_core_active_pattern_cases_decline_through_the_implicit_auto_open() {
    let env = fsharp_core_env();
    for (src, head) in [
        (
            "let f m = match m with KeyValue (key, value) -> key\n",
            "KeyValue",
        ),
        (
            "let f e = match e with Failure msg -> msg | _ -> \"\"\n",
            "Failure",
        ),
    ] {
        let rf = resolve(src, &env);
        assert_eq!(
            rf.resolution_at(nth(src, head, 0)),
            None,
            "`{head}` (implicit-auto-open recognizer) declines in pattern position — the \
             implicit path is a Stage-3b follow-up"
        );
    }
}

// ===== Data-driven implicit opens (plan A3/S3) =====
//
// FSharp.Core's manifest carries assembly-level `[<assembly: AutoOpen("…")>]`
// attributes; the resolver's implicit opens come from that list (FCS has no
// hardcoded list, and additionally opens `Microsoft` for FSharp.Core itself —
// `AddCcuToTcEnv`, CheckDeclarations.fs). These pin the entries the old
// hardcoded three-namespace seed could not express.

#[test]
fn qualified_path_through_microsoft_fsharp_open_resolves() {
    // `Collections.List.map` is only reachable because the manifest's
    // `AutoOpen("Microsoft.FSharp")` opens that namespace implicitly
    // (fsi-verified: `Collections.List.map (fun x -> x + 1) [1]` compiles).
    let env = fsharp_core_env();
    let src = "let test () = Collections.List.map id []\n";
    let rf = resolve(src, &env);
    let list_module = env
        .lookup_type(
            &["Microsoft".into(), "FSharp".into(), "Collections".into()],
            "List",
            0,
        )
        .expect("real FSharp.Core must declare the List module");
    match rf.resolution_at(at(src, "Collections.List.map")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, list_module, "map resolves into the List module");
            assert_eq!(il_name(env.member_at(parent, idx)), "Map");
        }
        other => panic!("expected Member for Collections.List.map, got {other:?}"),
    }
}

#[test]
fn qualified_path_through_microsoft_open_resolves() {
    // `FSharp.Collections.List.map` needs the `Microsoft` namespace itself
    // open — FCS prepends AutoOpen("Microsoft") for FSharp.Core even though
    // no manifest attribute says so (`AddCcuToTcEnv`'s fslib special case;
    // fsi-verified: `FSharp.Collections.List.map (fun x -> x + 1) [1]`).
    let env = fsharp_core_env();
    let src = "let test () = FSharp.Collections.List.map id []\n";
    let rf = resolve(src, &env);
    let list_module = env
        .lookup_type(
            &["Microsoft".into(), "FSharp".into(), "Collections".into()],
            "List",
            0,
        )
        .expect("real FSharp.Core must declare the List module");
    match rf.resolution_at(at(src, "FSharp.Collections.List.map")) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, list_module, "map resolves into the List module");
            assert_eq!(il_name(env.member_at(parent, idx)), "Map");
        }
        other => panic!("expected Member for FSharp.Collections.List.map, got {other:?}"),
    }
}

/// The real, shipped FSharp.Core `Microsoft.FSharp.Collections.List` module.
fn list_module(env: &AssemblyEnv) -> borzoi_sema::EntityHandle {
    env.lookup_type(
        &["Microsoft".into(), "FSharp".into(), "Collections".into()],
        "List",
        0,
    )
    .expect("real FSharp.Core must declare the List module")
}

#[test]
fn bare_list_call_inside_a_module_named_list_resolves_into_fsharp_core() {
    // The `module List = …` augmentation idiom (WoofWare.Myriad `List.fs`): a
    // file declares its own `[<RequireQualifiedAccess>] module private List` and,
    // *inside* that module, calls `List.fold` / `List.rev`. FCS resolves both the
    // head `List` and the member to FSharp.Core — the current module's own name is
    // NOT in scope as a self-qualifier (FS0039 for a self-member like
    // `List.partitionChoice`), so the name falls through to the auto-opened
    // `Microsoft.FSharp.Collections.List` (fcs-dump `uses`:
    //   L8  List -> List (FSharp.Core);  fold -> Microsoft.FSharp.Collections.List.fold
    //   L13 List -> List (FSharp.Core);  rev  -> Microsoft.FSharp.Collections.List.rev).
    // A same-file `module List` used to make the whole path defer as an unbound
    // name — the as-written self-module shadow preempted the opens tier where
    // FSharp.Core lives.
    let env = fsharp_core_env();
    let list = list_module(&env);
    let src = "namespace N\n\
               \n\
               [<RequireQualifiedAccess>]\n\
               module private List =\n\
               \x20\x20\x20\x20let f xs = List.fold (fun a b -> a + b) 0 xs\n\
               \x20\x20\x20\x20let g xs = List.rev xs\n";

    // `List.fold`: head → the FSharp.Core List module, whole path → its `Fold`.
    assert_eq!(
        rf_head(&env, src, "List", 1),
        Some(Resolution::Entity(list)),
        "the head `List` of `List.fold` (occ 1, after the `module List` header) must \
         resolve to Microsoft.FSharp.Collections.List"
    );
    match resolve(src, &env).resolution_at(nth(src, "List.fold", 0)) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, list, "fold resolves into the List module");
            assert_eq!(il_name(env.member_at(parent, idx)), "Fold");
        }
        other => panic!("expected Member for `List.fold`, got {other:?}"),
    }

    // `List.rev`: head (occ 2) → the FSharp.Core List module, whole path → `Reverse`.
    assert_eq!(
        rf_head(&env, src, "List", 2),
        Some(Resolution::Entity(list)),
        "the head `List` of `List.rev` must resolve to Microsoft.FSharp.Collections.List"
    );
    match resolve(src, &env).resolution_at(nth(src, "List.rev", 0)) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, list, "rev resolves into the List module");
            // `List.rev`'s F# source name is `rev`; its compiled IL method is `Reverse`.
            assert_eq!(il_name(env.member_at(parent, idx)), "Reverse");
        }
        other => panic!("expected Member for `List.rev`, got {other:?}"),
    }
}

/// Resolve `src` and read the resolution at the `n`th occurrence of `head`.
fn rf_head(env: &AssemblyEnv, src: &str, head: &str, n: usize) -> Option<Resolution> {
    resolve(src, env).resolution_at(nth(src, head, n))
}

#[test]
fn self_qualified_member_of_a_split_module_does_not_bind_fsharp_core() {
    // A module split across files: FCS merges `module N.List` over the namespace,
    // so a self-qualified `List.fold2` inside the *later* fragment binds the
    // project's own `N.List.fold2` (defined in file1), NOT FSharp.Core's
    // `List.fold2` — even though FSharp.Core defines a `fold2`. The merge is
    // *per member* (fcs-dump `uses-project`): a name the project fragment supplies
    // resolves to the project, a name only FSharp.Core defines still falls through
    // to it. Committing FSharp.Core's `fold2` here would be a wrong go-to-def (D5),
    // which the self-module relaxation must not do — so the tail with a project
    // member stays a conservative deferral, while `List.rev` (no project member)
    // still resolves into FSharp.Core.
    let env = fsharp_core_env();
    let list = list_module(&env);
    let file1 = "namespace N\n\nmodule List =\n    let fold2 = 1\n";
    let file2 = "namespace N\n\n\
                 module List =\n\
                 \x20\x20\x20\x20let g = List.fold2\n\
                 \x20\x20\x20\x20let h = List.rev [ 1 ]\n";
    let proj = resolve_project(&[impl_file(file1), impl_file(file2)], &env);
    let f2 = proj.file(1);

    // `List.fold2`: the tail names the merged module's own project member, so we
    // must NOT commit FSharp.Core's `List.fold2`.
    let fold2 = f2.resolution_at(nth(file2, "List.fold2", 0));
    assert!(
        !matches!(fold2, Some(Resolution::Member { .. })),
        "`List.fold2` names the split module's own project member, not FSharp.Core's; got {fold2:?}"
    );
    assert_ne!(
        f2.resolution_at(nth(file2, "List", 1)),
        Some(Resolution::Entity(list)),
        "the head `List` of the self-qualified project member must not bind FSharp.Core's List"
    );

    // `List.rev`: FSharp.Core supplies this one (the project fragment does not),
    // so the per-member merge still lets it resolve into FSharp.Core.
    match f2.resolution_at(nth(file2, "List.rev", 0)) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, list, "rev resolves into FSharp.Core's List module");
            assert_eq!(il_name(env.member_at(parent, idx)), "Reverse");
        }
        other => panic!("expected FSharp.Core Member for `List.rev`, got {other:?}"),
    }
}

#[test]
fn self_qualified_call_inside_a_module_nested_under_a_module_resolves_into_fsharp_core() {
    // The self-qualifier idiom also applies when `module List` is nested under
    // another *module* (not just a namespace): inside `module Top` / `module List`,
    // `List.fold` still binds FSharp.Core (fcs-dump: `List -> List (FSharp.Core)`,
    // `fold -> Microsoft.FSharp.Collections.List.fold`). The self name is the last
    // segment of the module chain `[Top, List]`, a *suffix*, so a prefix-only self
    // test would miss it and keep deferring.
    let env = fsharp_core_env();
    let list = list_module(&env);
    let src = "module Top\n\n\
               module List =\n\
               \x20\x20\x20\x20let g xs = List.fold (fun a b -> a + b) 0 xs\n";
    assert_eq!(
        rf_head(&env, src, "List", 1),
        Some(Resolution::Entity(list)),
        "`List` nested under `module Top` must still resolve to FSharp.Core's List"
    );
    match resolve(src, &env).resolution_at(nth(src, "List.fold", 0)) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, list);
            assert_eq!(il_name(env.member_at(parent, idx)), "Fold");
        }
        other => panic!("expected FSharp.Core Member for nested `List.fold`, got {other:?}"),
    }
}

#[test]
fn self_qualified_project_type_shadowing_a_fsharp_core_nested_module_does_not_bind_it() {
    // A module augmentation may add a *type* colliding with a FSharp.Core member
    // that FSharp.Core resolves as a *whole* path: file1's `N.Operators` defines
    // `type Checked`, and `Microsoft.FSharp.Core.Operators` has a nested module
    // `Checked`. Inside file2's `N.Operators`, `Operators.Checked` binds the
    // project type (fcs-dump: `Operators -> N.Operators`, `Checked ->
    // N.Operators.Checked`) — but FSharp.Core's `Operators.Checked` *owns* its path
    // (a nested module), so the opens tier would resolve it outright, committing the
    // head to FSharp.Core's `Operators` Entity. `Checked` is a type, not a value, so
    // only the exported-*type* index vetoes that fallthrough.
    let env = fsharp_core_env();
    let core_operators = core(&env, "Operators");
    let file1 = "namespace N\n\nmodule Operators =\n    type Checked() = class end\n";
    let file2 = "namespace N\n\nmodule Operators =\n    let x = Operators.Checked()\n";
    let proj = resolve_project(&[impl_file(file1), impl_file(file2)], &env);
    let f2 = proj.file(1);
    let checked = f2.resolution_at(nth(file2, "Operators.Checked", 0));
    assert!(
        !matches!(checked, Some(Resolution::Entity(e)) if e != core_operators)
            && !matches!(checked, Some(Resolution::Member { .. })),
        "`Operators.Checked` names the project type, not FSharp.Core's nested Checked module; got {checked:?}"
    );
    assert_ne!(
        f2.resolution_at(nth(file2, "Operators", 1)),
        Some(Resolution::Entity(core_operators)),
        "the head `Operators` must bind the project `N.Operators` (which owns `Checked`), not FSharp.Core"
    );
}

#[test]
fn self_qualified_name_captured_by_a_same_file_child_module_does_not_bind_fsharp_core() {
    // A same-file *child* module of the same name captures a self-qualified head:
    // inside `module List`, a child `module List` with `type rev` means `List.rev()`
    // binds the child's type, not FSharp.Core (fcs-dump: `List -> N.List.List`,
    // `rev -> N.List.List.rev`). FCS resolves a self-qualified head to the nearest
    // *non-self* `List`, so the child wins; the relaxation must not commit
    // FSharp.Core's `List.rev` here (a wrong go-to-def).
    let env = fsharp_core_env();
    let list = list_module(&env);
    let src = "namespace N\n\n\
               module List =\n\
               \x20\x20\x20\x20module List =\n\
               \x20\x20\x20\x20\x20\x20\x20\x20type rev() = class end\n\
               \x20\x20\x20\x20let x = List.rev()\n";
    let rf = resolve(src, &env);
    let whole = rf.resolution_at(nth(src, "List.rev", 0));
    assert!(
        !matches!(whole, Some(Resolution::Member { .. })),
        "`List.rev` is captured by the same-file child `module List`, not FSharp.Core; got {whole:?}"
    );
    // The head `List` (occ 2 — after the two `module List` headers) must not bind
    // FSharp.Core's List module either.
    assert_ne!(
        rf.resolution_at(nth(src, "List", 2)),
        Some(Resolution::Entity(list)),
        "the head `List` is captured by the child module, so it must not bind FSharp.Core"
    );
}

#[test]
fn companion_type_does_not_block_the_per_member_fallthrough_to_fsharp_core() {
    // The `type List` / `module List` companion pattern (FSharp.Core's own shape)
    // is *per member*: inside the module, `List.length` — which the sibling type
    // lacks but FSharp.Core supplies — still resolves to FSharp.Core (fcs-dump:
    // `List -> List (FSharp.Core)`, `length -> Microsoft.FSharp.Collections.List.length`).
    // So a same-name sibling type must NOT blanket-defer self-qualified references.
    let env = fsharp_core_env();
    let list = list_module(&env);
    let src = "namespace N\n\n\
               type List =\n\
               \x20\x20\x20\x20static member Go () = 1\n\
               \n\
               module List =\n\
               \x20\x20\x20\x20let y = List.length [ 1 ]\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(nth(src, "List", 2)),
        Some(Resolution::Entity(list)),
        "the companion type does not shadow `List.length`, which is FSharp.Core's"
    );
    match rf.resolution_at(nth(src, "List.length", 0)) {
        Some(Resolution::Member { parent, idx }) => {
            assert_eq!(parent, list);
            assert_eq!(il_name(env.member_at(parent, idx)), "Length");
        }
        other => panic!("expected FSharp.Core Member for `List.length`, got {other:?}"),
    }
}

#[test]
fn self_qualified_call_in_a_recursive_module_does_not_bind_fsharp_core() {
    // `module rec` / `namespace rec` DO put the module's own name in scope, so the
    // "self is FS0039" premise is void: inside `module rec List`, `List.rev` binds
    // the project's own `N.List.rev` (fcs-dump: `List -> N.List`, `rev -> N.List.rev`),
    // NOT FSharp.Core's `List.rev`. The relaxation must not fire for a recursive
    // module.
    let env = fsharp_core_env();
    let list = list_module(&env);
    let src = "namespace N\n\n\
               module rec List =\n\
               \x20\x20\x20\x20let rev (x: int) = x\n\
               \x20\x20\x20\x20let y = List.rev 1\n";
    let rf = resolve(src, &env);
    let whole = rf.resolution_at(nth(src, "List.rev", 0));
    assert!(
        !matches!(whole, Some(Resolution::Member { .. })),
        "`List.rev` in a `module rec List` binds the project's own `rev`, not FSharp.Core; got {whole:?}"
    );
    assert_ne!(
        rf.resolution_at(nth(src, "List", 1)),
        Some(Resolution::Entity(list)),
        "the recursive module's own name is in scope, so the head must not bind FSharp.Core"
    );
}

#[test]
fn task_builder_extension_members_do_not_resolve_bare() {
    // The manifest also auto-opens MODULE paths
    // (`TaskBuilderExtensions.{Low,LowPlus,Medium,High}Priority`,
    // `QueryRunExtensions.*`, `IntrinsicOperators`) whose statics are
    // extension members / operators. FCS never makes an extension member
    // bare-resolvable (fsi: bare `Bind` is FS0039), so sema skips
    // module-shaped entries entirely: the bare name must stay Deferred —
    // a wrong `Member` here would be a D5 soundness violation.
    let env = fsharp_core_env();
    let src = "let test () = Bind\n";
    let rf = resolve(src, &env);
    assert_eq!(
        rf.resolution_at(at(src, "Bind")),
        Some(Resolution::Deferred(DeferredReason::UnboundName)),
        "TaskBuilderExtensions' Bind must not bare-resolve"
    );
}

#[test]
fn processed_implicit_open_list_for_real_fsharp_core() {
    // The manifest carries 11 AutoOpen attributes; after processing
    // (`record_assembly_auto_opens`): `Microsoft` is prepended (FCS's fslib
    // special case), the namespace entries survive in manifest order, and the
    // six module-shaped entries (`IntrinsicOperators`,
    // `TaskBuilderExtensions.*`, `QueryRunExtensions.*`) are conservatively
    // dropped. This is the exact seed the resolver unions with its fallback.
    let env = fsharp_core_env();
    let expected: Vec<Vec<String>> = [
        "Microsoft",
        "Microsoft.FSharp",
        "Microsoft.FSharp.Core",
        "Microsoft.FSharp.Collections",
        "Microsoft.FSharp.Control",
    ]
    .iter()
    .map(|ns| ns.split('.').map(str::to_string).collect())
    .collect();
    assert_eq!(env.implicit_open_namespace_paths(), expected.as_slice());
}

// ===== Extension members are never bare-resolvable (autoopen plan ⚠) =====
//
// FCS keeps extension members out of the *unqualified* name environment
// entirely: a module's contents enter it through `AddValRefsToItems`, which
// filters `not vref.IsMember` (NameResolution.fs), and an `open type`'s statics
// through `ChooseMethInfosForNameEnv`, which filters
// `IsMethInfoPlainCSharpStyleExtensionMember`. An extension member is reachable
// only through the dot on its *target* (`l.Force()`), never as a bare name and
// — for an F#-native one — never module-qualified either.
//
// All four resolutions below were wrong before this fix (each fsi-verified as
// FS0039 against the real compiler).

#[test]
fn bare_fsharp_native_extension_members_do_not_resolve() {
    // `Microsoft.FSharp.Control.LazyExtensions` is an `[<AutoOpen>]` module in an
    // implicitly-opened namespace, so its statics are pushed into bare scope. But
    // every one of them is an extension member on `System.Lazy<'T>` — the instance
    // `Force` and the static `Create`/`CreateFromValue` — so FCS reports FS0039 for
    // all three bare (fsi: `let v = Force (lazy 1)` ⇒ "The value or constructor
    // 'Force' is not defined").
    let env = fsharp_core_env();

    // The currency: what a bare name that names *nothing* resolves to. A
    // single-segment unbound name is deferred, not errored (Phase 4 owns
    // diagnostics), so "FCS says FS0039" means "indistinguishable from this".
    let nowhere = "let test l = zzzNoSuchName l\n";
    let unbound = resolve(nowhere, &env).resolution_at(at(nowhere, "zzzNoSuchName"));
    assert_eq!(
        unbound,
        Some(Resolution::Deferred(DeferredReason::UnboundName))
    );

    for name in ["Force", "Create", "CreateFromValue"] {
        let src = format!("let test l = {name} l\n");
        let rf = resolve(&src, &env);
        assert_eq!(
            rf.resolution_at(at(&src, name)),
            unbound,
            "bare `{name}` is an extension member of the auto-open LazyExtensions, \
             so it must resolve exactly as an unbound name does: FCS says FS0039"
        );
    }
}

#[test]
fn module_qualified_fsharp_native_extension_member_does_not_resolve() {
    // The same members are not reachable *qualified* either: FCS resolves a
    // module-qualified path against the module's vals, and an extension member
    // is a member, not a value (fsi: `Microsoft.FSharp.Control.LazyExtensions.Force l`
    // ⇒ FS0039). Only `l.Force()` — the dot on the target type — reaches it.
    let env = fsharp_core_env();
    let src = "let test l = Microsoft.FSharp.Control.LazyExtensions.Force l\n";
    let rf = resolve(src, &env);
    let path = at(src, "Microsoft.FSharp.Control.LazyExtensions.Force");
    assert!(
        !matches!(rf.resolution_at(path), Some(Resolution::Member { .. })),
        "a module-qualified F#-native extension member must not resolve to a member"
    );
}

// ===== Cross-assembly semantic-token classification against real FSharp.Core =====
//
// The C#-fixture cross-assembly differential (`classify_assembly_diff.rs`) can
// exercise C# static members but not F# module members. Real FSharp.Core is the
// article: its `Operators` module holds every module-member shape — plain values,
// generic *generalizable* values (`typeof`/`sizeof`), functions — auto-opened so
// they bare-resolve, so the project-level `token_classifier` classifies them here.

/// Classify the single occurrence of `name` in `src` against real FSharp.Core.
fn classify_core(env: &AssemblyEnv, src: &str, name: &str) -> Option<SemanticClass> {
    let proj = resolve_project(&[impl_file(src)], env);
    proj.token_classifier(0, env)(at(src, name))
}

#[test]
fn generalizable_module_values_classify_as_values_not_functions() {
    // `typeof<'T>`/`sizeof<'T>`/`typedefof<'T>` are F# module *values* (zero
    // argument groups), but being *generic* they cannot be a CLR property, so fsc
    // emits each as a generic MethodDef — `module_value` is `None`. The pickle's
    // zero arg-group count (surfaced as `MethodLike::is_module_value_binding`) is
    // what keeps them values. (`Unchecked.defaultof` is the same shape but sits in
    // the nested `Operators.Unchecked` module, whose qualified members our resolver
    // does not yet reach — a separate gap; it declines soundly rather than
    // mis-colouring, so it is left out here.)
    //
    // FCS agrees they are values, not functions — probed on this exact assembly via
    // `fcs-dump uses-census-batch` on `let x = typeof<int>` etc.:
    //   class=Mfv  IsValue=true  IsFunction=false.
    // Before the fix `member_class` coloured every untagged module method a
    // function; this is the regression pin (codex review, cross-assembly stage).
    let env = fsharp_core_env();
    // A `Some(Value)` here also *implies* the name bare-resolved to a
    // `Resolution::Member` — `member_class` is only reached through one.
    for (src, name) in [
        ("let test () = typeof<int>\n", "typeof"),
        ("let test () = sizeof<int>\n", "sizeof"),
        ("let test () = typedefof<int list>\n", "typedefof"),
    ] {
        assert_eq!(
            classify_core(&env, src, name),
            Some(SemanticClass::Value),
            "generic module value `{name}` must classify as a value, not a function"
        );
    }
}

#[test]
fn plain_module_function_still_classifies_as_a_function() {
    // The counterweight: `id` is a genuine module *function* (`let id x = x`, one
    // argument group), so it must stay a function — the value fix must not spill
    // over. FCS: `id` has one curried parameter group ⇒ IsFunction=true.
    let env = fsharp_core_env();
    assert_eq!(
        classify_core(&env, "let test x = id x\n", "id"),
        Some(SemanticClass::Function),
        "bare `id` is a module function and must classify as a function"
    );
}

#[test]
fn plain_auto_open_module_statics_still_resolve() {
    // The filter is extension-keyed, not module-keyed: an ordinary `let` in an
    // auto-open module keeps resolving bare. `Microsoft.FSharp.Core.Operators.id`
    // sits in the same shape as `Force` (public static of an auto-open module in
    // an implicitly-opened namespace) and differs only in not being an extension.
    let env = fsharp_core_env();
    let src = "let test x = id x\n";
    let rf = resolve(src, &env);
    assert!(
        matches!(
            rf.resolution_at(at(src, "id")),
            Some(Resolution::Member { .. })
        ),
        "bare `id` is a plain auto-open module val and must still resolve"
    );
}
/// The morally-load-bearing sweep: the F# primitive aliases' semantics come
/// from FSharp.Core's own signature pickle — a marker per abbreviation, its
/// decoded target chased through the abbreviation chain and, for BCL
/// terminals, through `netstandard`'s type forwarders — with **no hard-coded
/// alias table anywhere**. This pins that the chase reproduces, name for
/// name, exactly the FQNs the deleted `fsharp_primitive_alias` table used to
/// hard-code (including the source synonyms `int8`/`uint8`/`single`/…), so
/// the table's semantics can never silently drift out of the mechanism that
/// replaced it.
///
/// FCS resolves these the same way: `type int = int32` is ordinary F# source
/// in FSharp.Core (`prim-types-prelude.fs`), not a compiler special case.
#[test]
fn primitive_alias_chases_reproduce_the_old_table() {
    let env = crate::common::full_bcl_env();
    let mfc: Vec<String> = ["Microsoft", "FSharp", "Core"].map(String::from).into();
    let expected: &[(&str, &str)] = &[
        ("bool", "System.Boolean"),
        ("char", "System.Char"),
        ("sbyte", "System.SByte"),
        ("int8", "System.SByte"),
        ("byte", "System.Byte"),
        ("uint8", "System.Byte"),
        ("int16", "System.Int16"),
        ("uint16", "System.UInt16"),
        ("int", "System.Int32"),
        ("int32", "System.Int32"),
        ("uint", "System.UInt32"),
        ("uint32", "System.UInt32"),
        ("int64", "System.Int64"),
        ("uint64", "System.UInt64"),
        ("float32", "System.Single"),
        ("single", "System.Single"),
        ("float", "System.Double"),
        ("double", "System.Double"),
        ("nativeint", "System.IntPtr"),
        ("unativeint", "System.UIntPtr"),
        ("obj", "System.Object"),
        ("string", "System.String"),
        ("decimal", "System.Decimal"),
    ];
    for (alias, fqn) in expected {
        let marker = env
            .lookup_type(&mfc, alias, 0)
            .unwrap_or_else(|| panic!("FSharp.Core must surface a `{alias}` marker"));
        assert!(
            env.is_abbreviation(marker),
            "`{alias}` must be an abbreviation marker"
        );
        let terminal = env
            .resolve_abbreviation_tycon(marker)
            .unwrap_or_else(|| panic!("`{alias}`'s target must chase"));
        assert_eq!(
            env.entity_full_name(terminal),
            *fqn,
            "`{alias}` must chase to exactly the FQN the old alias table hard-coded"
        );
    }
    // The generic abbreviations the old table could never carry — the
    // motivating gap (`option` hovered as "No definition available") — chase
    // to their FSharp.Core-internal targets.
    let mfcoll: Vec<String> = ["Microsoft", "FSharp", "Collections"]
        .map(String::from)
        .into();
    for (ns, alias, arity, fqn) in [
        (&mfc, "option", 1, "Microsoft.FSharp.Core.Option"),
        (&mfc, "ref", 1, "Microsoft.FSharp.Core.Ref"),
        (&mfc, "unit", 0, "Microsoft.FSharp.Core.Unit"),
        (&mfcoll, "list", 1, "Microsoft.FSharp.Collections.List"),
        (&mfcoll, "seq", 1, "System.Collections.Generic.IEnumerable"),
    ] {
        let marker = env
            .lookup_type(ns, alias, arity)
            .unwrap_or_else(|| panic!("FSharp.Core must surface a `{alias}` marker"));
        let terminal = env
            .resolve_abbreviation_tycon(marker)
            .unwrap_or_else(|| panic!("`{alias}`'s target must chase"));
        assert_eq!(env.entity_full_name(terminal), fqn, "`{alias}` terminal");
    }
}
