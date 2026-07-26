//! The unavailable-definition explanation under an **incomplete** referenced-
//! assembly projection.
//!
//! `handlers::definition_availability`'s own unit tests resolve against an empty
//! [`AssemblyEnv`], where no name reaches an assembly at all — so they cannot
//! reach the arm that matters here. These use a real `FSharp.Core.dll` and mark
//! the env incomplete, which is the state the LSP is in when a referenced DLL
//! could not be read.
//!
//! The case worth its own group is the **inference-side** one. A member access
//! (`recv.Name`) leaves the resolver deferring and inference holding the
//! verdict; go-to-definition acts on inference's answer, so the explanation must
//! come from there too. Explaining the resolver's leftover instead would tell
//! the user their member access "needs the receiver's inferred type" while the
//! real cause is a DLL that would not load.

use borzoi::handlers::definition_availability::{UnavailableReason, classify, explanation_range};
use borzoi_cst::syntax::{AstNode, ImplFile, SyntaxNode};
use borzoi_sema::{
    AssemblyEnv, InferredFile, ProjectItems, ResolvedFile, infer_file, resolve_file,
};

use crate::common::{ensure_fsharp_core_dll, ensure_system_runtime_dll};

/// A real env over `FSharp.Core.dll` + `System.Runtime.dll`, and the same env
/// marked incomplete — a host that skipped a DLL it could not project.
///
/// `System.Runtime` is not optional here: without it the receiver type of
/// `"hi".Length` has no declaration to look the member up on, and the cursor
/// falls all the way through to `UntrackedName` under *both* envs.
fn complete_and_incomplete() -> (AssemblyEnv, AssemblyEnv) {
    let core = std::fs::read(ensure_fsharp_core_dll()).expect("read FSharp.Core.dll");
    let sysrt = std::fs::read(ensure_system_runtime_dll()).expect("read System.Runtime.dll");
    let views = vec![
        borzoi_assembly::Ecma335Assembly::parse(&core).expect("parse FSharp.Core.dll"),
        borzoi_assembly::Ecma335Assembly::parse(&sysrt).expect("parse System.Runtime.dll"),
    ];
    let complete = AssemblyEnv::from_views(&views).expect("build AssemblyEnv");
    let mut incomplete = complete.clone();
    incomplete.mark_referenced_assemblies_incomplete();
    (complete, incomplete)
}

fn analyse(src: &str, env: &AssemblyEnv) -> (ResolvedFile, InferredFile, SyntaxNode) {
    let parse = borzoi_cst::parser::parse(src);
    let file = ImplFile::cast(parse.root).expect("source parses as an impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), env);
    let inferred = infer_file(&file, &resolved, env);
    (resolved, inferred, file.syntax().clone())
}

fn byte_of(src: &str, needle: &str) -> usize {
    src.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {src:?}"))
}

/// A member access whose verdict lives only in inference: with a complete
/// projection there is nothing to explain (it is navigable), and with an
/// incomplete one the explanation names the environmental cause rather than the
/// resolver's generic qualified-access leftover.
#[test]
fn an_inference_side_member_reports_the_environmental_cause() {
    let (complete, incomplete) = complete_and_incomplete();
    let src = "module M\nlet n = \"hi\".Length\n";
    let byte = byte_of(src, "Length");

    let (resolved, inferred, root) = analyse(src, &complete);
    assert_eq!(
        classify(&resolved, Some(&inferred), &root, byte, false),
        None,
        "a navigable member has nothing to explain — else the incomplete half \
         proves nothing"
    );

    let (resolved, inferred, root) = analyse(src, &incomplete);
    assert_eq!(
        classify(&resolved, Some(&inferred), &root, byte, false).map(|u| u.reason),
        Some(UnavailableReason::IncompleteAssemblies),
    );
    // The tooltip covers the member name inference spoke about, not whatever
    // span the resolver happened to leave behind.
    let range = explanation_range(&resolved, Some(&inferred), &root, byte)
        .expect("a classified cursor has somewhere to anchor");
    assert_eq!(
        &src[usize::from(range.start())..usize::from(range.end())],
        "Length",
    );
}

/// Dropping the side-table is what the bug was: the resolver's leftover verdict
/// at that cursor is a *different*, misleading reason. Pinned so the parameter
/// cannot quietly be passed `None` again at a call site that has an
/// [`InferredFile`] to hand.
#[test]
fn without_the_side_table_the_same_cursor_is_explained_wrongly() {
    let (_, incomplete) = complete_and_incomplete();
    let src = "module M\nlet n = \"hi\".Length\n";
    let byte = byte_of(src, "Length");
    let (resolved, inferred, root) = analyse(src, &incomplete);

    let with = classify(&resolved, Some(&inferred), &root, byte, false).map(|u| u.reason);
    let without = classify(&resolved, None, &root, byte, false).map(|u| u.reason);
    assert_eq!(with, Some(UnavailableReason::IncompleteAssemblies));
    assert_ne!(
        with, without,
        "if these agree the side-table is doing nothing and this group is vacuous"
    );
}
