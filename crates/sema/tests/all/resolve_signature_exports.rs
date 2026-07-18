//! Stage 2 of `docs/fsi-signature-restriction-plan.md`: the signature
//! **exports its surviving surface with signature identity**. A cross-file
//! use of a sig-exposed value / visible union or enum case resolves to an
//! `Item` whose def is the ident in the `.fsi` (World A), while the export
//! folds at the **implementation's** Compile slot (conclusion 4: provenance =
//! impl, def = sig). Hidden / opaque / not-yet-modelled surface stays exactly
//! as Stage 1 left it: assembly fall-through or `Deferred`, never a wrong
//! commit.
//!
//! FCS-free tests pin the fold's observable behaviour; the `*_agrees_with_fcs`
//! tests feed FCS real `.fsi`-bearing file sets and assert
//! certain-implies-exact (decl file **and** def range) plus non-vacuous
//! agreement counts.

use std::path::{Path, PathBuf};

use borzoi_sema::{
    AssemblyEnv, ProjectFile, Resolution, ResolvedProject, SourceFile, qualified_names,
    resolve_project_files, resolve_project_files_incremental,
};

use crate::common::{invoke_fcs_dump_project_with_refs, parse_fcs_uses_project, temp_fs_tree};
use crate::resolve_signatures::{
    SigProject, assert_item_in, assert_sig_matches_fcs, assert_uncommitted, ensure_reflib_built,
    project, reflib_env, res_at, source_file, span,
};

/// Assert the use committed to an `Item` whose def is the (unique) `ident`
/// token in file `def_idx` — the go-to-definition target is the signature
/// ident itself, not merely "somewhere in the right file".
fn assert_def_ident(
    proj: &ResolvedProject,
    files: &[(&str, &str)],
    res: Option<Resolution>,
    def_idx: usize,
    ident: &str,
    what: &str,
) {
    let res = res.unwrap_or_else(|| panic!("{what}: expected an Item, got no resolution"));
    assert!(
        matches!(res, Resolution::Item(_)),
        "{what}: expected an Item, got {res:?}"
    );
    let (idx, def) = proj.item_def(res).expect("item def");
    assert_eq!(idx, def_idx, "{what}: wrong declaring file");
    let src = files[def_idx].1;
    let start = src.find(ident).expect("ident present in def file");
    assert!(
        src[start + 1..].find(ident).is_none(),
        "ident {ident:?} is ambiguous in {src:?}"
    );
    assert_eq!(
        def.range,
        span(start, start + ident.len()),
        "{what}: def range is not the signature ident"
    );
}

// ---------------------------------------------------------------------------
// FCS-free: the signature-identity commit (empty assembly env).
// ---------------------------------------------------------------------------

/// The core Stage-2 behaviour: a sig-exposed value resolves cross-file to an
/// `Item` whose def is the `.fsi` ident; the hidden sibling stays
/// uncommitted; the signature slot still owns no exports of its own (the
/// surface folds at the impl's slot).
#[test]
fn exposed_val_commits_with_signature_identity() {
    let files = [
        ("/p/A.fsi", "module A\n\nval shown: int\n"),
        ("/p/A.fs", "module A\n\nlet shown = 1\nlet hidden = 2\n"),
        (
            "/p/B.fs",
            "module B\n\nlet u1 = A.shown\nlet u2 = A.hidden\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "A.shown"),
        0,
        "shown",
        "sig-exposed A.shown",
    );
    assert_uncommitted(res_at(&proj, &files, 2, "A.hidden"), "sig-hidden A.hidden");
    // The signature slot owns no `ItemId` range of its own …
    assert!(proj.file(0).exports().is_empty());
    // … its surface rides the impl's slot: the impl's own two items plus the
    // signature's one appended export.
    assert_eq!(proj.file(1).exports().len(), 3);
}

/// The `open` half: the sig-exposed value is enumerable through the opened
/// module, so the bare use commits to the signature identity; the hidden one
/// stays uncommitted.
#[test]
fn open_bare_exposed_val_commits() {
    let files = [
        ("/p/A.fsi", "module A\n\nval shown: int\n"),
        ("/p/A.fs", "module A\n\nlet shown = 1\nlet hidden = 2\n"),
        (
            "/p/B.fs",
            "module B\n\nopen A\n\nlet u1 = shown\nlet u2 = hidden\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "shown"),
        0,
        "shown",
        "bare sig-exposed shown after open",
    );
    assert_uncommitted(res_at(&proj, &files, 2, "hidden"), "bare hidden after open");
}

/// A namespace-headed pair (the module is a namespace-direct `module A =` in
/// the sig): its exposed vals commit with signature identity too.
#[test]
fn namespace_headed_module_val_commits() {
    let files = [
        (
            "/p/A.fsi",
            "namespace N\n\nmodule A =\n    val shown: int\n",
        ),
        (
            "/p/A.fs",
            "namespace N\n\nmodule A =\n    let shown = 1\n    let hidden = 2\n",
        ),
        (
            "/p/B.fs",
            "module B\n\nlet u1 = N.A.shown\nlet u2 = N.A.hidden\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "N.A.shown"),
        0,
        "shown",
        "namespace-headed sig-exposed N.A.shown",
    );
    assert_uncommitted(res_at(&proj, &files, 2, "N.A.hidden"), "N.A.hidden");
}

/// A headerless pair: the signature's vals live under the implicit filename
/// module, and Stage 2 exports them there — a capability the unsigned
/// headerless impl does not have (its values are un-addressable).
#[test]
fn headerless_pair_exposes_implicit_module_val() {
    let files = [
        ("/p/A.fsi", "val shown: int\n"),
        ("/p/A.fs", "let shown = 1\nlet hidden = 2\n"),
        (
            "/p/B.fs",
            "module B\n\nlet u1 = A.shown\nlet u2 = A.hidden\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "A.shown"),
        0,
        "shown",
        "headerless sig-exposed A.shown",
    );
    assert_uncommitted(res_at(&proj, &files, 2, "A.hidden"), "headerless A.hidden");
}

/// A **visible** union representation in the signature exports its cases:
/// the type-qualified path, the module-qualified value path, and the
/// open-then-bare form all commit to the case ident in the `.fsi`.
#[test]
fn visible_union_cases_commit() {
    let files = [
        ("/p/Col.fsi", "module Col\n\ntype Color = Red | Green\n"),
        (
            "/p/Col.fs",
            "module Col\n\ntype Color = Red | Green\nlet helper = 1\n",
        ),
        (
            "/p/B.fs",
            "module B\n\nlet u1 = Col.Color.Red\nlet u2 = Col.Green\nlet u3 = Col.helper\n",
        ),
        ("/p/C.fs", "module C\n\nopen Col\n\nlet u4 = Red\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "Col.Color.Red"),
        0,
        "Red",
        "type-qualified visible case",
    );
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "Col.Green"),
        0,
        "Green",
        "module-qualified visible case",
    );
    // The helper is sig-hidden.
    assert_uncommitted(res_at(&proj, &files, 2, "Col.helper"), "sig-hidden helper");
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 3, "Red"),
        0,
        "Red",
        "bare visible case after open",
    );
}

/// An **opaque** signature (`type Color` with no representation) hides the
/// cases — the crux the impl walk cannot express. No case commits anywhere.
#[test]
fn opaque_type_hides_cases() {
    let files = [
        ("/p/Op.fsi", "module Op\n\ntype Color\n"),
        ("/p/Op.fs", "module Op\n\ntype Color = Red | Green\n"),
        (
            "/p/B.fs",
            "module B\n\nlet u1 = Op.Color.Red\nlet u2 = Op.Red\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(
        res_at(&proj, &files, 2, "Op.Color.Red"),
        "type-qualified case of an opaque type",
    );
    assert_uncommitted(
        res_at(&proj, &files, 2, "Op.Red"),
        "module-qualified case of an opaque type",
    );
}

/// A `[<RequireQualifiedAccess>]` union in the signature: the case commits
/// type-qualified only — the value-namespace paths stay uncommitted.
#[test]
fn rqa_union_case_commits_type_qualified_only() {
    let files = [
        (
            "/p/Col.fsi",
            "module Col\n\n[<RequireQualifiedAccess>]\ntype Color = Red | Green\n",
        ),
        (
            "/p/Col.fs",
            "module Col\n\n[<RequireQualifiedAccess>]\ntype Color = Red | Green\n",
        ),
        (
            "/p/B.fs",
            "module B\n\nlet u1 = Col.Color.Red\nlet u2 = Col.Green\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "Col.Color.Red"),
        0,
        "Red",
        "type-qualified RQA case",
    );
    assert_uncommitted(
        res_at(&proj, &files, 2, "Col.Green"),
        "module-qualified RQA case (not in the value namespace)",
    );
}

/// Enum cases in the signature commit type-qualified (an enum case is never
/// bare-reachable).
#[test]
fn enum_cases_commit_type_qualified() {
    let files = [
        ("/p/E.fsi", "module E\n\ntype Kind = A = 0 | B = 1\n"),
        ("/p/E.fs", "module E\n\ntype Kind = A = 0 | B = 1\n"),
        ("/p/B.fs", "module B\n\nlet u1 = E.Kind.A\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "E.Kind.A"),
        0,
        "A",
        "type-qualified enum case",
    );
}

/// `val private` in the signature is dropped (FS1094 cross-file — never a
/// clean commit), so the use stays uncommitted.
#[test]
fn private_val_is_not_exported() {
    let files = [
        (
            "/p/A.fsi",
            "module A\n\nval shown: int\nval private secret: int\n",
        ),
        (
            "/p/A.fs",
            "module A\n\nlet shown = 1\nlet private secret = 2\n",
        ),
        ("/p/B.fs", "module B\n\nlet u = A.secret\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(res_at(&proj, &files, 2, "A.secret"), "sig-private secret");
}

/// Multi-fragment recovery (the plan's fixture): with `module M` split across
/// an unsigned earlier fragment (public `x`) and a later signatured one that
/// hides `x`, dropping the signatured fragment's `x` leaves the earlier
/// public one as the surviving export.
#[test]
fn multi_fragment_earlier_public_fragment_survives() {
    let files = [
        ("/d1/First.fs", "module M\n\nlet x = 0\n"),
        ("/d2/Pair.fsi", "module M\n\nval other: int\n"),
        ("/d2/Pair.fs", "module M\n\nlet x = 1\nlet other = 2\n"),
        ("/u/Use.fs", "module Use\n\nlet a = M.x\nlet b = M.other\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_item_in(
        &proj,
        res_at(&proj, &files, 3, "M.x"),
        0,
        "M.x survives via the earlier unsigned fragment",
    );
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 3, "M.other"),
        1,
        "other",
        "M.other commits to the signatured fragment's .fsi",
    );
}

/// …and the flip: when the later signatured fragment *exposes* `x` too, the
/// latest fragment wins — the signature identity.
#[test]
fn multi_fragment_later_exposed_fragment_wins() {
    let files = [
        ("/d1/First.fs", "module M\n\nlet x = 0\n"),
        ("/d2/Pair.fsi", "module M\n\nval x: int\n"),
        ("/d2/Pair.fs", "module M\n\nlet x = 1\n"),
        ("/u/Use.fs", "module Use\n\nlet a = M.x\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 3, "M.x"),
        1,
        "x",
        "M.x commits to the latest (signatured) fragment's .fsi",
    );
}

/// Conclusion 4 (provenance = impl slot): with two `[<AutoOpen>]` modules in
/// a namespace, the signatured one's fold position is its **implementation's**
/// Compile slot — later than the colliding module between the sig and the
/// impl, so its member wins the bare-name collision. Were the surface
/// published at the *signature's* slot, the intervening module would win.
#[test]
fn auto_open_provenance_is_the_impl_slot() {
    let files = [
        (
            "/p/NA.fsi",
            "namespace N\n\n[<AutoOpen>]\nmodule A =\n    val shown: int\n",
        ),
        (
            "/p/NB.fs",
            "namespace N\n\n[<AutoOpen>]\nmodule B =\n    let shown = 2\n",
        ),
        ("/p/NA.fs", "namespace N\n\nmodule A =\n    let shown = 1\n"),
        ("/p/Use.fs", "module Use\n\nopen N\n\nlet u = shown\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 3, "shown"),
        0,
        "shown",
        "bare shown binds the sig-exposed auto-open member published at the \
         impl's (later) slot",
    );
}

// ---------------------------------------------------------------------------
// The assembly merge, sharpened: exposed names now commit the signature.
// ---------------------------------------------------------------------------

/// The intervening-collision probe (2026-07-18): between the sig and the
/// impl the merged module publishes only the assembly half, and FCS resolves
/// `Shared.shown` there to the **assembly**. Stage 2's screen exemption for
/// the exactly-modelled exposed surface makes the fall-through fire (Stage 1
/// over-deferred it).
#[test]
fn intervening_collision_falls_through_to_assembly() {
    let files = [
        ("/p/A.fsi", "module ProbeNs.Shared\n\nval shown: int\n"),
        (
            "/p/Between.fs",
            "module Between\n\nlet g = ProbeNs.Shared.shown\n",
        ),
        ("/p/A.fs", "module ProbeNs.Shared\n\nlet shown = 1\n"),
    ];
    let proj = resolve_project_files(&project(&files), &reflib_env());
    assert!(
        matches!(
            res_at(&proj, &files, 1, "ProbeNs.Shared.shown"),
            Some(Resolution::Member { .. })
        ),
        "intervening sig-exposed shown must fall through to the merged \
         assembly member (probe row 5), got {:?}",
        res_at(&proj, &files, 1, "ProbeNs.Shared.shown"),
    );
}

// ---------------------------------------------------------------------------
// Incremental ≡ batch across signature-surface edits.
// ---------------------------------------------------------------------------

/// A `.fsi` edit that *adds* an exposed val changes the impl's materialised
/// contribution, so the suffix re-resolves — and the incremental result must
/// equal a cold fold, with the new export committing.
#[test]
fn incremental_matches_cold_when_sig_gains_a_val() {
    let env = AssemblyEnv::default();
    let v1 = [
        ("/p/A.fsi", "module A\n\nval shown: int\n"),
        ("/p/A.fs", "module A\n\nlet shown = 1\nlet hidden = 2\n"),
        (
            "/p/B.fs",
            "module B\n\nlet u1 = A.shown\nlet u2 = A.hidden\n",
        ),
    ];
    let prev_files = project(&v1);
    let prev = resolve_project_files(&prev_files, &env);
    assert_uncommitted(res_at(&prev, &v1, 2, "A.hidden"), "A.hidden before edit");

    let v2 = [
        ("/p/A.fsi", "module A\n\nval shown: int\nval hidden: int\n"),
        ("/p/A.fs", "module A\n\nlet shown = 1\nlet hidden = 2\n"),
        (
            "/p/B.fs",
            "module B\n\nlet u1 = A.shown\nlet u2 = A.hidden\n",
        ),
    ];
    let v2_sig = source_file("/p/A.fsi", v2[0].1);
    let mut new_files = prev_files.clone();
    new_files[0] = ProjectFile::new(v2_sig, new_files[0].qnof.clone());
    let (incr, reused) = resolve_project_files_incremental(&prev_files, &prev, &new_files, &env);
    let cold = resolve_project_files(&new_files, &env);
    assert_eq!(incr, cold, "incremental ≡ batch after exposing a new val");
    assert!(
        !reused[2],
        "a surface-changing .fsi edit must invalidate the suffix"
    );
    assert_def_ident(
        &cold,
        &v2,
        res_at(&cold, &v2, 2, "A.hidden"),
        0,
        "hidden",
        "newly exposed val after the .fsi edit",
    );
}

// ---------------------------------------------------------------------------
// The FCS-free mini-matrix: header × exposure × use style, all cells.
// ---------------------------------------------------------------------------

/// Exhaustive FCS-free sweep of the Stage-2 commit surface (the FCS twin is
/// `signature_matrix_agrees_with_fcs_per_reference`): for every header shape
/// and use style, the exposed val commits to the `.fsi` and the hidden one
/// stays uncommitted.
#[test]
fn exposure_matrix_commits_exactly_the_exposed_surface() {
    for header in ["module", "namespace", "anon"] {
        for style in ["qualified", "open_bare"] {
            let (sig_src, impl_src, stem, dotted) = match header {
                "module" => (
                    "module Pn.Md\n\nval shown: int\n".to_string(),
                    "module Pn.Md\n\nlet shown = 1\nlet hidden = 2\n".to_string(),
                    "/p/Md",
                    "Pn.Md",
                ),
                "namespace" => (
                    "namespace Pn\n\nmodule Md =\n    val shown: int\n".to_string(),
                    "namespace Pn\n\nmodule Md =\n    let shown = 1\n    let hidden = 2\n"
                        .to_string(),
                    "/p/Md",
                    "Pn.Md",
                ),
                _ => (
                    "val shown: int\n".to_string(),
                    "let shown = 1\nlet hidden = 2\n".to_string(),
                    "/p/Pn.Md",
                    "Pn.Md",
                ),
            };
            let use_src = match style {
                "qualified" => {
                    format!("module Use\n\nlet a = {dotted}.shown\nlet b = {dotted}.hidden\n")
                }
                _ => format!("module Use\n\nopen {dotted}\n\nlet a = shown\nlet b = hidden\n"),
            };
            let sig_path = format!("{stem}.fsi");
            let impl_path = format!("{stem}.fs");
            let files = [
                (sig_path.as_str(), sig_src.as_str()),
                (impl_path.as_str(), impl_src.as_str()),
                ("/p/Use.fs", use_src.as_str()),
            ];
            let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
            let what = format!("{header}/{style}");
            let shown = match style {
                "qualified" => res_at(&proj, &files, 2, &format!("{dotted}.shown")),
                _ => res_at(&proj, &files, 2, "shown"),
            };
            assert_def_ident(&proj, &files, shown, 0, "shown", &format!("{what}: shown"));
            let hidden = match style {
                "qualified" => res_at(&proj, &files, 2, &format!("{dotted}.hidden")),
                _ => res_at(&proj, &files, 2, "hidden"),
            };
            assert_uncommitted(hidden, &format!("{what}: hidden"));
        }
    }
}

// ---------------------------------------------------------------------------
// FCS differentials: the new commits, certain-implies-exact.
// ---------------------------------------------------------------------------

/// The Stage-2 commit shapes as `assert_sig_matches_fcs` fixtures: every
/// `expected_cross_file` count below requires the signature-identity commit
/// to actually happen (a deferral fails the count; a wrong file or range
/// fails the exactness assertions).
#[test]
fn signature_exports_agree_with_fcs() {
    let fixtures = [
        // Visible union: the type-qualified, module-qualified, and
        // open-then-bare case forms plus the case's *pattern* use all land on
        // the .fsi's case idents; the sig-hidden helper is FS0039.
        SigProject {
            label: "sig2_cases",
            files: vec![
                ("Col.fsi", "module Col\n\ntype Color = Red | Green\n"),
                (
                    "Col.fs",
                    "module Col\n\ntype Color = Red | Green\nlet helper = 1\n",
                ),
                (
                    "Use.fs",
                    "module Use\n\nlet u1 = Col.Color.Red\nlet u2 = Col.Green\n",
                ),
                ("Open.fs", "module O\n\nopen Col\n\nlet u3 = Red\n"),
            ],
            refs: vec![],
            expected_cross_file: 3,
            fcs_must_not_declare: vec!["Col.helper"],
        },
        // Opaque type: no case declares in-project anywhere.
        SigProject {
            label: "sig2_opaque",
            files: vec![
                ("Op.fsi", "module Op\n\ntype Color\n"),
                ("Op.fs", "module Op\n\ntype Color = Red | Green\n"),
                ("Use.fs", "module Use\n\nlet u = Op.Red\n"),
            ],
            refs: vec![],
            expected_cross_file: 0,
            fcs_must_not_declare: vec!["Op.Red"],
        },
        // RQA union: the type-qualified form commits; nothing else reaches
        // the case.
        SigProject {
            label: "sig2_rqa",
            files: vec![
                (
                    "Col.fsi",
                    "module Col\n\n[<RequireQualifiedAccess>]\ntype Color = Red | Green\n",
                ),
                (
                    "Col.fs",
                    "module Col\n\n[<RequireQualifiedAccess>]\ntype Color = Red | Green\n",
                ),
                ("Use.fs", "module Use\n\nlet u1 = Col.Color.Red\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        // Enum cases.
        SigProject {
            label: "sig2_enum",
            files: vec![
                ("E.fsi", "module E\n\ntype Kind = A = 0 | B = 1\n"),
                ("E.fs", "module E\n\ntype Kind = A = 0 | B = 1\n"),
                ("Use.fs", "module Use\n\nlet u = E.Kind.A\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        // Conclusion 4 as a live differential: the sig-exposed auto-open
        // member publishes at the impl's slot, after the intervening
        // auto-open module, so FCS (and we) bind the .fsi's `shown`.
        SigProject {
            label: "sig2_autoopen_provenance",
            files: vec![
                (
                    "NA.fsi",
                    "namespace N\n\n[<AutoOpen>]\nmodule A =\n    val shown: int\n",
                ),
                (
                    "NB.fs",
                    "namespace N\n\n[<AutoOpen>]\nmodule B =\n    let shown = 2\n",
                ),
                ("NA.fs", "namespace N\n\nmodule A =\n    let shown = 1\n"),
                ("Use.fs", "module Use\n\nopen N\n\nlet u = shown\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        // Multi-fragment recovery: the earlier unsigned fragment's public
        // `x` survives the later signatured fragment's hiding; the exposed
        // `other` lands on the .fsi.
        SigProject {
            label: "sig2_multifrag",
            files: vec![
                ("d1/First.fs", "module M\n\nlet x = 0\n"),
                ("d2/Pair.fsi", "module M\n\nval other: int\n"),
                ("d2/Pair.fs", "module M\n\nlet x = 1\nlet other = 2\n"),
                ("Use.fs", "module Use\n\nlet a = M.x\nlet b = M.other\n"),
            ],
            refs: vec![],
            expected_cross_file: 2,
            fcs_must_not_declare: vec![],
        },
        // The intervening-file half of the codex-P1 relative shape: BETWEEN
        // the sig and the impl, the signatured `A.M` has not published, so
        // inside `namespace A` the reading falls to the root `module M` —
        // FCS binds M.fs there, and after the impl's slot it binds the
        // `.fsi` (both counted: 2 exact cross-file agreements).
        SigProject {
            label: "sig2_intervening_relative",
            files: vec![
                ("M.fs", "module M\n\nlet x = 0\n"),
                ("AM.fsi", "module A.M\n\nval x: int\n"),
                (
                    "Between.fs",
                    "namespace A\n\nmodule Between =\n    let y = M.x\n",
                ),
                ("AM.fs", "module A.M\n\nlet x = 1\n"),
                ("Use.fs", "namespace A\n\nmodule Use =\n    let z = M.x\n"),
            ],
            refs: vec![],
            expected_cross_file: 2,
            fcs_must_not_declare: vec![],
        },
        // …and the flip: the later signatured fragment exposes `x`, so the
        // latest fragment (the .fsi) wins.
        SigProject {
            label: "sig2_multifrag_flip",
            files: vec![
                ("d1/First.fs", "module M\n\nlet x = 0\n"),
                ("d2/Pair.fsi", "module M\n\nval x: int\n"),
                ("d2/Pair.fs", "module M\n\nlet x = 1\n"),
                ("Use.fs", "module Use\n\nlet a = M.x\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
    ];
    for fixture in &fixtures {
        assert_sig_matches_fcs(fixture);
    }
}

/// The intervening-collision cell as a live differential: FCS resolves the
/// between-file `Shared.shown` to the **assembly** (the merged module
/// publishes only at the impl's slot — probe row 5), and so do we.
#[test]
fn intervening_collision_agrees_with_fcs() {
    let reflib = ensure_reflib_built();
    let files: Vec<(&str, &str)> = vec![
        ("A.fsi", "module ProbeNs.Shared\n\nval shown: int\n"),
        (
            "Between.fs",
            "module Between\n\nlet g = ProbeNs.Shared.shown\n",
        ),
        ("A.fs", "module ProbeNs.Shared\n\nlet shown = 1\n"),
    ];
    let (root, written) = temp_fs_tree("sig2_intervening", &files);
    let paths: Vec<&Path> = written.iter().map(|(path, _)| path.as_path()).collect();
    let json = invoke_fcs_dump_project_with_refs(&paths, &[reflib]);
    let fcs_files = parse_fcs_uses_project(&json, &written);

    let srcs: Vec<SourceFile> = files
        .iter()
        .map(|(rel, src)| source_file(rel, src))
        .collect();
    let full_paths: Vec<PathBuf> = written.iter().map(|(path, _)| path.clone()).collect();
    let qnofs = qualified_names(&srcs, &full_paths);
    let input: Vec<ProjectFile> = srcs
        .into_iter()
        .zip(qnofs)
        .map(|(file, qnof)| ProjectFile::new(file, qnof))
        .collect();
    let proj = resolve_project_files(&input, &reflib_env());
    let _ = std::fs::remove_dir_all(&root);

    // The FCS premise: the intervening use resolves to the assembly.
    let between = fcs_files
        .iter()
        .find(|f| f.path.file_name() == written[1].0.file_name())
        .expect("FCS uses for Between.fs");
    let shown = between
        .uses
        .iter()
        .find(|u| u.name == "shown")
        .expect("FCS reports the intervening shown use");
    assert_eq!(
        shown.assembly.as_deref(),
        Some("SemaSignatureRefLib"),
        "FCS resolves the intervening sig-exposed shown to the merged assembly"
    );
    // And ours matches.
    assert!(
        matches!(
            res_at(&proj, &files, 1, "ProbeNs.Shared.shown"),
            Some(Resolution::Member { .. })
        ),
        "our intervening resolution must be the assembly member, got {:?}",
        res_at(&proj, &files, 1, "ProbeNs.Shared.shown"),
    );
}
