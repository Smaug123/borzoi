//! Stages 2–3 of `docs/fsi-signature-restriction-plan.md`: the signature
//! **exports its surviving surface with signature identity**. A cross-file
//! use of a sig-exposed value / visible union or enum case resolves to an
//! `Item` whose def is the ident in the `.fsi` (World A), while the export
//! folds at the **implementation's** Compile slot (conclusion 4: provenance =
//! impl, def = sig). The Stage-3 accessibility slice widens the surface —
//! `val internal` / `val public` / `module internal` export like the plain
//! public surface, and `val private` genuinely drops (falls through). Hidden
//! / opaque / not-yet-modelled surface stays exactly as Stage 1 left it:
//! assembly fall-through or `Deferred`, never a wrong commit.
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
            for access in ["", "public ", "internal ", "private "] {
                let (sig_src, impl_src, stem, dotted) = match header {
                    "module" => (
                        format!("module Pn.Md\n\nval {access}shown: int\n"),
                        "module Pn.Md\n\nlet shown = 1\nlet hidden = 2\n".to_string(),
                        "/p/Md",
                        "Pn.Md",
                    ),
                    "namespace" => (
                        format!("namespace Pn\n\nmodule Md =\n    val {access}shown: int\n"),
                        "namespace Pn\n\nmodule Md =\n    let shown = 1\n    let hidden = 2\n"
                            .to_string(),
                        "/p/Md",
                        "Pn.Md",
                    ),
                    _ => (
                        format!("val {access}shown: int\n"),
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
                let what = format!("{header}/{style}/{}", access.trim());
                let shown = match style {
                    "qualified" => res_at(&proj, &files, 2, &format!("{dotted}.shown")),
                    _ => res_at(&proj, &files, 2, "shown"),
                };
                if access == "private " {
                    // A sig-private val is dropped: never a clean FCS commit
                    // (FS1094), and nothing here to fall through to.
                    assert_uncommitted(shown, &format!("{what}: private shown"));
                } else {
                    assert_def_ident(&proj, &files, shown, 0, "shown", &format!("{what}: shown"));
                }
                let hidden = match style {
                    "qualified" => res_at(&proj, &files, 2, &format!("{dotted}.hidden")),
                    _ => res_at(&proj, &files, 2, "hidden"),
                };
                assert_uncommitted(hidden, &format!("{what}: hidden"));
            }
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

// ---------------------------------------------------------------------------
// Codex round-1 findings, each FCS-probed before fixing (2026-07-18).
// ---------------------------------------------------------------------------

/// Codex P1 (FCS-probed): a signatured RQA union's type-qualified case beats
/// an earlier fragment's same-path nested-module VALUE — `M.T.C` binds the
/// `.fsi` case, so the qualified-value lookup must stay vetoed on a
/// case-exported path (only the type-qualified case lookup is exempt).
#[test]
fn rqa_case_beats_earlier_nested_module_value() {
    let files = [
        (
            "/p/First.fs",
            "module M

module T =
    let CaseC = 0
",
        ),
        (
            "/p/Col.fsi",
            "module M

[<RequireQualifiedAccess>]
type T = CaseC | D
",
        ),
        (
            "/p/Col.fs",
            "module M

[<RequireQualifiedAccess>]
type T = CaseC | D
",
        ),
        (
            "/p/Use.fs",
            "module Use

let u = M.T.CaseC
",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 3, "M.T.CaseC"),
        1,
        "CaseC",
        "type-qualified RQA case colliding with an earlier nested-module value",
    );
}

/// Codex P2 (FCS-probed): with two paired fragments of one module, a LATER
/// signature's exactly-exported `val x` overrides an EARLIER screen's
/// name-set veto (the earlier sig mentions `x` only inside an unmodelled
/// nested module) — `M.x` binds the later `.fsi`.
#[test]
fn later_fragment_exposed_val_overrides_earlier_screen() {
    let files = [
        (
            "/d1/Pair.fsi",
            "module M

module Inner =
    val x: int
",
        ),
        (
            "/d1/Pair.fs",
            "module M

module Inner =
    let x = 9
",
        ),
        (
            "/d2/Pair.fsi",
            "module M

val x: int
",
        ),
        (
            "/d2/Pair.fs",
            "module M

let x = 1
",
        ),
        (
            "/u/Use.fs",
            "module Use

let u = M.x
",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 4, "M.x"),
        2,
        "x",
        "later fragment's exposed val over an earlier fragment's screen",
    );
}

/// The sound flip of the above: when the EARLIER fragment exports `x` and a
/// LATER screen may expose an unmodelled `x`, the later screen's veto stands
/// — the later fragment could shadow the export, so committing the earlier
/// identity would be a guess. Deferral only (FCS resolves to one of them).
#[test]
fn later_screen_still_vetoes_earlier_export() {
    let files = [
        (
            "/d1/Pair.fsi",
            "module M

val x: int
",
        ),
        (
            "/d1/Pair.fs",
            "module M

let x = 1
",
        ),
        (
            "/d2/Pair.fsi",
            "module M

module Inner =
    val x: int
",
        ),
        (
            "/d2/Pair.fs",
            "module M

module Inner =
    let x = 9
",
        ),
        (
            "/u/Use.fs",
            "module Use

let u = M.x
",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(
        res_at(&proj, &files, 4, "M.x"),
        "an earlier export under a later fragment's possibly-exposing screen",
    );
}

/// Codex P3 (FCS-probed): a headerless pair's dotted implicit module
/// (`Pn.Md.fsi`) also establishes its ancestor namespace, so the recovery
/// form `open Pn; Md.shown` reaches the signature export.
#[test]
fn open_of_implicit_ancestor_namespace_reaches_export() {
    let files = [
        (
            "/p/Pn.Md.fsi",
            "val shown: int
",
        ),
        (
            "/p/Pn.Md.fs",
            "let shown = 1
",
        ),
        (
            "/p/Use.fs",
            "module Use

open Pn

let u = Md.shown
",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "Md.shown"),
        0,
        "shown",
        "namespace-opened implicit-module export",
    );
}

/// The three shapes as live FCS differentials (certain-implies-exact + the
/// agreement counts force the commits to exist and land on the right decl).
#[test]
fn codex_round1_shapes_agree_with_fcs() {
    let fixtures = [
        SigProject {
            label: "sig2_rqa_vs_value",
            files: vec![
                (
                    "First.fs",
                    "module M

module T =
    let CaseC = 0
",
                ),
                (
                    "Col.fsi",
                    "module M

[<RequireQualifiedAccess>]
type T = CaseC | D
",
                ),
                (
                    "Col.fs",
                    "module M

[<RequireQualifiedAccess>]
type T = CaseC | D
",
                ),
                (
                    "Use.fs",
                    "module Use

let u = M.T.CaseC
",
                ),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        SigProject {
            label: "sig2_later_fragment_export",
            files: vec![
                (
                    "d1/Pair.fsi",
                    "module M

module Inner =
    val x: int
",
                ),
                (
                    "d1/Pair.fs",
                    "module M

module Inner =
    let x = 9
",
                ),
                (
                    "d2/Pair.fsi",
                    "module M

val x: int
",
                ),
                (
                    "d2/Pair.fs",
                    "module M

let x = 1
",
                ),
                (
                    "Use.fs",
                    "module Use

let u = M.x
",
                ),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        SigProject {
            label: "sig2_implicit_ancestor_open",
            files: vec![
                (
                    "Pn.Md.fsi",
                    "val shown: int
",
                ),
                (
                    "Pn.Md.fs",
                    "let shown = 1
",
                ),
                (
                    "Use.fs",
                    "module Use

open Pn

let u = Md.shown
",
                ),
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

/// Codex round 2 (FCS-probed): screen precedence is **materialisation**
/// (paired-implementation) order, not signature-slot order. With the valid
/// interleaving `[A.fsi, B.fsi, B.fs, A.fs]`, `A.fs` contributes last, so
/// FCS binds `N.M.x` to **A**'s signature. A's `val internal x` is modelled
/// (Stage 3), so the commit is A's signature identity — the
/// later-materialising fragment's export outranks B's.
#[test]
fn reversed_interleaving_commits_the_last_materialising_export() {
    let files = [
        (
            "/p/A.fsi",
            "namespace N\n\nmodule M =\n    val internal x: int\n",
        ),
        ("/p/B.fsi", "namespace N\n\nmodule M =\n    val x: int\n"),
        ("/p/B.fs", "namespace N\n\nmodule M =\n    let x = 2\n"),
        ("/p/A.fs", "namespace N\n\nmodule M =\n    let x = 1\n"),
        ("/p/Use.fs", "module Use\n\nlet u = N.M.x\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 4, "N.M.x"),
        0,
        "x",
        "reversed interleaving: the last-materialising fragment's export",
    );
}

/// The round-2 guard proper, with a mention Stage 3 still does not model
/// (an `exception X` — FCS-probed: `N.M.X` binds A.fsi's exception ctor,
/// diagnostics-clean): A's fragment materialises last, so B's
/// earlier-materialising exactly-exported `X` must NOT override A's veto —
/// committing B's stale item where FCS binds A's exception would be wrong.
/// The reading defers.
#[test]
fn reversed_interleaving_defers_to_the_later_impl_screen() {
    let files = [
        (
            "/p/A.fsi",
            "namespace N\n\nmodule M =\n    exception X of int\n    val otherA: int\n",
        ),
        ("/p/B.fsi", "namespace N\n\nmodule M =\n    val X: int\n"),
        ("/p/B.fs", "namespace N\n\nmodule M =\n    let X = 2\n"),
        (
            "/p/A.fs",
            "namespace N\n\nmodule M =\n    exception X of int\n    let otherA = 1\n",
        ),
        ("/p/Use.fs", "module Use\n\nlet u = N.M.X\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(
        res_at(&proj, &files, 4, "N.M.X"),
        "reversed interleaving: FCS binds A's unmodelled exception, so \
         B's earlier-materialising export must not commit",
    );
}

/// …and the in-order control: with `[A.fsi, A.fs, B.fsi, B.fs]`, B's impl
/// contributes last, so its exported `x` wins — FCS binds B's `.fsi` and so
/// do we.
#[test]
fn in_order_interleaving_commits_the_later_fragment_export() {
    let files = [
        (
            "/p/A.fsi",
            "namespace N\n\nmodule M =\n    val internal x: int\n",
        ),
        ("/p/A.fs", "namespace N\n\nmodule M =\n    let x = 1\n"),
        ("/p/B.fsi", "namespace N\n\nmodule M =\n    val x: int\n"),
        ("/p/B.fs", "namespace N\n\nmodule M =\n    let x = 2\n"),
        ("/p/Use.fs", "module Use\n\nlet u = N.M.x\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 4, "N.M.x"),
        2,
        "x",
        "in-order interleaving: the later-materialising fragment's export",
    );
}

/// The interleavings as live FCS differentials: the reversed one commits
/// **A** (FCS declares in A.fsi — the last-materialising fragment; a B
/// commit fails the exactness check), the in-order one commits B, and the
/// exception-mention reversed variant must not commit at all (FCS binds
/// A.fsi's exception, which we do not model — B's item would be wrong).
#[test]
fn codex_round2_shapes_agree_with_fcs() {
    let fixtures = [
        SigProject {
            label: "sig2_reversed_interleaving",
            files: vec![
                (
                    "A.fsi",
                    "namespace N\n\nmodule M =\n    val internal x: int\n",
                ),
                ("B.fsi", "namespace N\n\nmodule M =\n    val x: int\n"),
                ("B.fs", "namespace N\n\nmodule M =\n    let x = 2\n"),
                ("A.fs", "namespace N\n\nmodule M =\n    let x = 1\n"),
                ("Use.fs", "module Use\n\nlet u = N.M.x\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        SigProject {
            label: "sig3_reversed_interleaving_exception",
            files: vec![
                (
                    "A.fsi",
                    "namespace N\n\nmodule M =\n    exception X of int\n    val otherA: int\n",
                ),
                ("B.fsi", "namespace N\n\nmodule M =\n    val X: int\n"),
                ("B.fs", "namespace N\n\nmodule M =\n    let X = 2\n"),
                (
                    "A.fs",
                    "namespace N\n\nmodule M =\n    exception X of int\n    let otherA = 1\n",
                ),
                ("Use.fs", "module Use\n\nlet u = N.M.X\n"),
            ],
            refs: vec![],
            expected_cross_file: 0,
            fcs_must_not_declare: vec![],
        },
        SigProject {
            label: "sig2_inorder_interleaving",
            files: vec![
                (
                    "A.fsi",
                    "namespace N\n\nmodule M =\n    val internal x: int\n",
                ),
                ("A.fs", "namespace N\n\nmodule M =\n    let x = 1\n"),
                ("B.fsi", "namespace N\n\nmodule M =\n    val x: int\n"),
                ("B.fs", "namespace N\n\nmodule M =\n    let x = 2\n"),
                ("Use.fs", "module Use\n\nlet u = N.M.x\n"),
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

/// The **fragment-interleaving matrix**: the codex rounds 1–2 findings were
/// all interactions between the screen exemption and multi-fragment merges,
/// so this sweeps that class mechanically instead of fixture-by-fixture.
/// Two namespace-headed fragment pairs (`A.fsi`/`A.fs`, `B.fsi`/`B.fs`)
/// both contribute `module N.M`; axes = every valid sig-before-impl
/// interleaving × each fragment's exposure of the probe val `X`
/// (exactly-modelled `val X` / `val internal X` (Stage 3: also modelled) /
/// `val private X` (Stage 3: dropped) / an unmodelled value-namespace
/// mention (`exception X`) / hidden). The site-keyed oracle is the same as
/// `signature_matrix_agrees_with_fcs_per_reference`: FCS in-project → we
/// match the decl exactly or defer; FCS unbound → we defer. Floors keep all
/// verdict families non-vacuous.
#[test]
fn fragment_interleaving_matrix_agrees_with_fcs() {
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Exposure {
        Modelled,
        Internal,
        Private,
        Unmodelled,
        Hidden,
    }
    use Exposure::*;
    // Compile orders as (label, row order); rows are S1/I1/S2/I2.
    let interleavings: &[(&str, [usize; 4])] = &[
        ("adjacent", [0, 1, 2, 3]), // S1 I1 S2 I2
        ("grouped", [0, 2, 1, 3]),  // S1 S2 I1 I2
        ("reversed", [0, 2, 3, 1]), // S1 S2 I2 I1 — impl order flips
    ];
    let sig_src = |e: Exposure, k: usize| match e {
        Modelled => {
            format!("namespace N\n\nmodule M =\n    val X: int\n    val other{k}: int\n")
        }
        Internal => {
            format!("namespace N\n\nmodule M =\n    val internal X: int\n    val other{k}: int\n")
        }
        Private => {
            format!("namespace N\n\nmodule M =\n    val private X: int\n    val other{k}: int\n")
        }
        Unmodelled => {
            format!("namespace N\n\nmodule M =\n    exception X of int\n    val other{k}: int\n")
        }
        Hidden => format!("namespace N\n\nmodule M =\n    val other{k}: int\n"),
    };
    let impl_src = |e: Exposure, k: usize| match e {
        Unmodelled => {
            format!("namespace N\n\nmodule M =\n    exception X of int\n    let other{k} = {k}\n")
        }
        _ => format!("namespace N\n\nmodule M =\n    let X = {k}\n    let other{k} = {k}\n"),
    };

    let mut item_agreements = 0usize;
    let mut deferrals = 0usize;

    for (ilabel, order) in interleavings {
        for e1 in [Modelled, Internal, Private, Unmodelled, Hidden] {
            for e2 in [Modelled, Internal, Private, Unmodelled, Hidden] {
                let rows_base: [(String, String); 4] = [
                    ("A.fsi".to_string(), sig_src(e1, 1)),
                    ("A.fs".to_string(), impl_src(e1, 1)),
                    ("B.fsi".to_string(), sig_src(e2, 2)),
                    ("B.fs".to_string(), impl_src(e2, 2)),
                ];
                let mut rows: Vec<(String, String)> =
                    order.iter().map(|&i| rows_base[i].clone()).collect();
                rows.push((
                    "Use.fs".to_string(),
                    "module Use\n\nlet u = N.M.X\n".to_string(),
                ));
                let row_refs: Vec<(&str, &str)> = rows
                    .iter()
                    .map(|(rel, src)| (rel.as_str(), src.as_str()))
                    .collect();
                let label = format!("sig2frag_{ilabel}_{e1:?}_{e2:?}").to_lowercase();

                let (root, written) = temp_fs_tree(&label, &row_refs);
                let paths: Vec<&Path> = written.iter().map(|(path, _)| path.as_path()).collect();
                let json = invoke_fcs_dump_project_with_refs(&paths, &[]);
                let fcs_files = parse_fcs_uses_project(&json, &written);

                let srcs: Vec<SourceFile> = row_refs
                    .iter()
                    .zip(&written)
                    .map(|((rel, src), _)| source_file(rel, src))
                    .collect();
                let full_paths: Vec<PathBuf> =
                    written.iter().map(|(path, _)| path.clone()).collect();
                let qnofs = qualified_names(&srcs, &full_paths);
                let input: Vec<ProjectFile> = srcs
                    .into_iter()
                    .zip(qnofs)
                    .map(|(file, qnof)| ProjectFile::new(file, qnof))
                    .collect();
                let proj = resolve_project_files(&input, &AssemblyEnv::default());
                let _ = std::fs::remove_dir_all(&root);

                let use_idx = written.len() - 1;
                let (use_path, use_source) = &written[use_idx];
                let needle = "N.M.X";
                let start = use_source.find(needle).expect("probe site present");
                let site = span(start, start + needle.len());
                let fcs_at_site = fcs_files
                    .iter()
                    .find(|f| f.path.file_name() == use_path.file_name())
                    .and_then(|f| {
                        f.uses.iter().find(|u| {
                            u.start == usize::from(site.start()) && u.end == usize::from(site.end())
                        })
                    });
                let ours = proj.file(use_idx).resolution_at(site);
                let what = format!("{label}: {needle}");
                match fcs_at_site {
                    Some(u) if u.decl.is_some() => {
                        let decl = u.decl.as_ref().expect("checked");
                        match ours {
                            None | Some(Resolution::Deferred(_)) => deferrals += 1,
                            Some(res @ Resolution::Item(_)) => {
                                let (idx, def) = proj.item_def(res).expect("item def");
                                assert_eq!(
                                    written[idx].0.file_name(),
                                    decl.file.file_name(),
                                    "{what}: wrong declaring file"
                                );
                                assert_eq!(
                                    def.range,
                                    span(decl.start, decl.end),
                                    "{what}: wrong def range"
                                );
                                item_agreements += 1;
                            }
                            other => panic!(
                                "{what}: FCS declares in-project at {:?}, we committed {other:?}",
                                decl.file
                            ),
                        }
                    }
                    // FCS unbound or unreported: we must say nothing.
                    _ => match ours {
                        None | Some(Resolution::Deferred(_)) => deferrals += 1,
                        other => panic!("{what}: FCS is unbound here, we committed {other:?}"),
                    },
                }
            }
        }
    }
    // Non-vacuity floors: the cells whose last-materialising fragment
    // exposes a modelled val (plain or internal) commit, as do the
    // dropped/hidden-last cells falling back to a modelled earlier fragment
    // (observed 42); the unmodelled-mention cells and the FCS-unbound cells
    // defer (observed 33). Zero wrong commits is the assertion above.
    println!("interleaving matrix: {item_agreements} item agreements, {deferrals} deferrals");
    assert!(item_agreements >= 30, "item agreements: {item_agreements}");
    assert!(deferrals >= 20, "deferrals: {deferrals}");
}

/// Codex round 3 (FCS-probed): in `[First.fs, A.fsi, A.fs, B.fsi,
/// Between.fs, B.fs]`, FCS binds Between's `N.M.x` to **A.fsi** (A's
/// fragment is the latest materialised, B's export is still pending). A's
/// `val internal x` is modelled (Stage 3), so Between's reading commits A's
/// signature identity — never First.fs's stale public `x`, and never B's
/// pending export.
#[test]
fn pending_fragment_does_not_outrank_the_latest_materialised_export() {
    let files = [
        ("/p/First.fs", "namespace N\n\nmodule M =\n    let x = 0\n"),
        (
            "/p/A.fsi",
            "namespace N\n\nmodule M =\n    val internal x: int\n",
        ),
        ("/p/A.fs", "namespace N\n\nmodule M =\n    let x = 1\n"),
        ("/p/B.fsi", "namespace N\n\nmodule M =\n    val x: int\n"),
        ("/p/Between.fs", "module Between\n\nlet u = N.M.x\n"),
        ("/p/B.fs", "namespace N\n\nmodule M =\n    let x = 2\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 4, "N.M.x"),
        1,
        "x",
        "a reader between B.fsi and B.fs binds the latest materialised export",
    );
}

/// The round-3 guard proper, with a mention Stage 3 still does not model:
/// an **unmaterialised** signature's export — its implementation still past
/// the reader — must not cancel an already-materialised earlier screen. A's
/// fragment exposes an `exception X` (FCS-probed: Between's `N.M.X` binds
/// A.fsi's exception, diagnostics-clean), which we do not model, so A's
/// screen vetoes; committing First.fs's stale public `X` — what cancelling
/// A's screen with B's pending `val X` would do — is a wrong target, so the
/// reading defers.
#[test]
fn unmaterialised_export_does_not_cancel_active_screen() {
    let files = [
        ("/p/First.fs", "namespace N\n\nmodule M =\n    let X = 0\n"),
        (
            "/p/A.fsi",
            "namespace N\n\nmodule M =\n    exception X of int\n    val otherA: int\n",
        ),
        (
            "/p/A.fs",
            "namespace N\n\nmodule M =\n    exception X of int\n    let otherA = 1\n",
        ),
        ("/p/B.fsi", "namespace N\n\nmodule M =\n    val X: int\n"),
        ("/p/Between.fs", "module Between\n\nlet u = N.M.X\n"),
        ("/p/B.fs", "namespace N\n\nmodule M =\n    let X = 2\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(
        res_at(&proj, &files, 4, "N.M.X"),
        "a reader between B.fsi and B.fs, under A's still-active screen",
    );
    // After B.fs materialises, B's export is the latest fragment and wins.
    // (FCS binds B.fsi for a use after B.fs — the in-order rule.)
}

/// …both round-3 shapes as live FCS differentials: FCS declares Between's
/// use in A.fsi both times. The `val internal` variant commits it exactly
/// (1 agreement); the exception variant must not commit at all (a First.fs
/// or B commit fails the exactness check; expected agreements: 0 — we
/// defer, honestly).
#[test]
fn codex_round3_shape_agrees_with_fcs() {
    let fixtures = [
        SigProject {
            label: "sig2_unmaterialised_exemption",
            files: vec![
                ("First.fs", "namespace N\n\nmodule M =\n    let x = 0\n"),
                (
                    "A.fsi",
                    "namespace N\n\nmodule M =\n    val internal x: int\n",
                ),
                ("A.fs", "namespace N\n\nmodule M =\n    let x = 1\n"),
                ("B.fsi", "namespace N\n\nmodule M =\n    val x: int\n"),
                ("Between.fs", "module Between\n\nlet u = N.M.x\n"),
                ("B.fs", "namespace N\n\nmodule M =\n    let x = 2\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        SigProject {
            label: "sig3_unmaterialised_exemption_exception",
            files: vec![
                ("First.fs", "namespace N\n\nmodule M =\n    let X = 0\n"),
                (
                    "A.fsi",
                    "namespace N\n\nmodule M =\n    exception X of int\n    val otherA: int\n",
                ),
                (
                    "A.fs",
                    "namespace N\n\nmodule M =\n    exception X of int\n    let otherA = 1\n",
                ),
                ("B.fsi", "namespace N\n\nmodule M =\n    val X: int\n"),
                ("Between.fs", "module Between\n\nlet u = N.M.X\n"),
                ("B.fs", "namespace N\n\nmodule M =\n    let X = 2\n"),
            ],
            refs: vec![],
            expected_cross_file: 0,
            fcs_must_not_declare: vec![],
        },
    ];
    for fixture in &fixtures {
        assert_sig_matches_fcs(fixture);
    }
}

/// Codex round 4: the sig-derived def's **function kind** looks through a
/// `when …` constraint wrapper (`'T -> 'T when 'T : comparison` is still a
/// function — FCS's classification), while a *parenthesised* arrow stays a
/// value of function type (the arity distinction).
#[test]
fn sig_val_function_kind_looks_through_constraints_not_parens() {
    use borzoi_sema::DefKind;
    let files = [
        (
            "/p/A.fsi",
            "module A\n\nval f: 'T -> 'T when 'T : comparison\nval g: (int -> int)\nval h: int -> int\nval v: int\n",
        ),
        (
            "/p/A.fs",
            "module A\n\nlet f x = x\nlet g = id\nlet h x = x + 1\nlet v = 3\n",
        ),
        (
            "/p/B.fs",
            "module B\n\nlet u1 = A.f\nlet u2 = A.g\nlet u3 = A.h\nlet u4 = A.v\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    let kind_of = |needle: &str| {
        let res = res_at(&proj, &files, 2, needle).expect("resolved");
        proj.item_def(res).expect("item def").1.kind
    };
    assert_eq!(
        kind_of("A.f"),
        DefKind::Value { is_function: true },
        "constrained arrow is a function"
    );
    assert_eq!(
        kind_of("A.g"),
        DefKind::Value { is_function: false },
        "parenthesised arrow is a value of function type"
    );
    assert_eq!(
        kind_of("A.h"),
        DefKind::Value { is_function: true },
        "plain arrow is a function"
    );
    assert_eq!(
        kind_of("A.v"),
        DefKind::Value { is_function: false },
        "non-arrow is a value"
    );
}

// ---------------------------------------------------------------------------
// Stage 3, accessibility slice (FCS-probed 2026-07-18): `val internal` /
// `val public` / `module internal` headers export exactly like the plain
// public surface (one project = one assembly, so `internal` restricts
// nothing sema can see), and `val private` is a genuine **drop** — FCS
// resolves a reading of it only with an FS1094 error, an earlier public
// fragment's same-path value binds *cleanly*, and a colliding assembly
// member binds cleanly too. `module private` stays screen-deferred (FCS
// resolves through it only with FS1092/FS1094 errors).
// ---------------------------------------------------------------------------

/// `val internal` / `val public` commit with signature identity, exactly as
/// an unannotated `val` (probe: both diagnostics-clean, decl = the `.fsi`).
#[test]
fn internal_and_public_vals_commit_with_signature_identity() {
    let files = [
        (
            "/p/A.fsi",
            "module M\n\nval internal shown: int\nval public seen: int\n",
        ),
        ("/p/A.fs", "module M\n\nlet shown = 1\nlet seen = 2\n"),
        ("/p/B.fs", "module B\n\nlet u1 = M.shown\nlet u2 = M.seen\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "M.shown"),
        0,
        "shown",
        "val internal",
    );
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "M.seen"),
        0,
        "seen",
        "val public",
    );
}

/// A `module internal` header (top-level and namespace-direct) exports its
/// surface: project-visible accessibility restricts nothing within one
/// assembly (probe: clean, decl = the `.fsi`, impl header annotated or not).
#[test]
fn module_internal_headers_export_their_surface() {
    let toplevel = [
        ("/p/A.fsi", "module internal M\n\nval shown: int\n"),
        ("/p/A.fs", "module M\n\nlet shown = 1\n"),
        ("/p/B.fs", "module B\n\nlet u = M.shown\n"),
    ];
    let proj = resolve_project_files(&project(&toplevel), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &toplevel,
        res_at(&proj, &toplevel, 2, "M.shown"),
        0,
        "shown",
        "top-level module internal",
    );

    let nsdirect = [
        (
            "/p/A.fsi",
            "namespace N\n\nmodule internal A =\n    val y: int\n",
        ),
        ("/p/A.fs", "namespace N\n\nmodule A =\n    let y = 1\n"),
        ("/p/B.fs", "module B\n\nlet u = N.A.y\n"),
    ];
    let proj = resolve_project_files(&project(&nsdirect), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &nsdirect,
        res_at(&proj, &nsdirect, 2, "N.A.y"),
        0,
        "y",
        "namespace-direct module internal",
    );
}

/// The drop, fragment half (probe: **diagnostics-clean**): a signatured
/// fragment's `val private x` contributes nothing, so an earlier unsigned
/// fragment's public `x` is the binding FCS makes — a deferral here is the
/// Stage-2 over-deferral this slice removes.
#[test]
fn private_val_drops_to_the_earlier_public_fragment() {
    let files = [
        ("/p/First.fs", "namespace N\n\nmodule A =\n    let x = 1\n"),
        (
            "/p/P.fsi",
            "namespace N\n\nmodule A =\n    val private x: int\n",
        ),
        ("/p/P.fs", "namespace N\n\nmodule A =\n    let x = 2\n"),
        ("/p/Use.fs", "module Use\n\nlet d = N.A.x\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 3, "N.A.x"),
        0,
        "x",
        "earlier public fragment past a private-val drop",
    );
}

/// The drop, assembly half (probe: **diagnostics-clean**): a sig-private
/// member falls through to the colliding referenced-assembly member, exactly
/// like a member the signature never mentions; the exposed sibling still
/// commits the signature identity.
#[test]
fn private_val_drops_to_the_colliding_assembly_member() {
    let files = [
        (
            "/p/A.fsi",
            "module ProbeNs.Shared\n\nval shown: int\nval private bar: int\n",
        ),
        (
            "/p/A.fs",
            "module ProbeNs.Shared\n\nlet shown = 1\nlet bar = 2\n",
        ),
        (
            "/p/Use.fs",
            "module Use\n\nlet a = ProbeNs.Shared.shown\nlet b = ProbeNs.Shared.bar\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &reflib_env());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 2, "ProbeNs.Shared.shown"),
        0,
        "shown",
        "exposed sibling of a private val",
    );
    let bar = res_at(&proj, &files, 2, "ProbeNs.Shared.bar");
    assert!(
        matches!(bar, Some(Resolution::Member { .. })),
        "sig-private bar must fall through to the assembly member, got {bar:?}"
    );
}

/// A lone private val (no collision, no other fragment): the reading is an
/// FCS error (FS1094 — never a clean commit), so it stays uncommitted.
#[test]
fn lone_private_val_stays_uncommitted() {
    let files = [
        ("/p/A.fsi", "module M\n\nval private x: int\n"),
        ("/p/A.fs", "module M\n\nlet x = 1\n"),
        ("/p/B.fs", "module B\n\nlet u = M.x\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(res_at(&proj, &files, 2, "M.x"), "lone private val");
}

/// A same-name unmodelled mention beside the private val (here a nested
/// `module X`) does not disturb the fall-through: a module is not a value,
/// so FCS still binds the earlier public fragment's `X` cleanly
/// (FCS-probed), and the value-namespace surface at the path stays exactly
/// modelled by the private export.
#[test]
fn private_val_beside_an_unmodelled_module_still_falls_through() {
    let files = [
        ("/p/First.fs", "namespace N\n\nmodule A =\n    let X = 1\n"),
        (
            "/p/P.fsi",
            "namespace N\n\nmodule A =\n    val private X: int\n\n    module X =\n        val inner: int\n",
        ),
        (
            "/p/P.fs",
            "namespace N\n\nmodule A =\n    let X = 2\n\n    module X =\n        let inner = 3\n",
        ),
        ("/p/Use.fs", "module Use\n\nlet d = N.A.X\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 3, "N.A.X"),
        0,
        "X",
        "earlier public fragment past a private val with a module-mention sibling",
    );
}

/// Codex Stage-3 round 1, P1 (FCS-probed): a `val private` is **not** a
/// plain drop — it is accessible from within its module's subtree. A later
/// file's fragment of the same module reads `N.M.x` diagnostics-clean, decl
/// = the private `.fsi` ident; the earlier fragment's public `x` must NOT
/// win there. Outside the subtree the restricted export behaves as dropped
/// (the sibling tests above).
#[test]
fn private_val_is_accessible_to_a_later_same_module_fragment() {
    let files = [
        ("/p/First.fs", "namespace N\n\nmodule M =\n    let x = 1\n"),
        (
            "/p/P.fsi",
            "namespace N\n\nmodule M =\n    val private x: int\n    val shown: int\n",
        ),
        (
            "/p/P.fs",
            "namespace N\n\nmodule M =\n    let x = 2\n    let shown = 3\n",
        ),
        (
            "/p/Later.fs",
            "namespace N\n\nmodule M =\n    let y = N.M.x\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_def_ident(
        &proj,
        &files,
        res_at(&proj, &files, 3, "N.M.x"),
        1,
        "x",
        "same-module-subtree reading of the sig-private val",
    );
}

/// Codex Stage-3 round 1, P2, pinned as a known over-deferral: after `open`
/// of the signatured module, a bare reading of the sig-private name that
/// collides with an assembly member binds the **assembly** in FCS
/// (diagnostics-clean, probed), but the open fold still defers every
/// assembly entry of a signatured root (the Stage-1 blanket
/// hidden-valued marking — same class as the documented hidden-name open
/// over-deferral). Deferral is sound; a *project* commit here would be
/// wrong, which is what this pins.
#[test]
fn open_bare_private_collision_defers_but_never_commits_a_project_item() {
    let files = [
        (
            "/p/A.fsi",
            "module ProbeNs.Shared\n\nval shown: int\nval private bar: int\n",
        ),
        (
            "/p/A.fs",
            "module ProbeNs.Shared\n\nlet shown = 1\nlet bar = 2\n",
        ),
        (
            "/p/Use.fs",
            "module Use\n\nopen ProbeNs.Shared\n\nlet b = bar\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &reflib_env());
    let bar = res_at(&proj, &files, 2, "bar");
    assert!(
        matches!(
            bar,
            None | Some(
                Resolution::Deferred(_) | Resolution::Member { .. } | Resolution::Entity(_)
            )
        ),
        "FCS binds the assembly member; a project commit is wrong, got {bar:?}"
    );
}

/// `module private` stays screen-deferred: FCS resolves through it only
/// with FS1092/FS1094 errors, so there is no clean commit to make.
#[test]
fn module_private_stays_deferred() {
    let files = [
        (
            "/p/A.fsi",
            "namespace N\n\nmodule private A =\n    val y: int\n",
        ),
        ("/p/A.fs", "namespace N\n\nmodule A =\n    let y = 1\n"),
        ("/p/B.fs", "module B\n\nlet u = N.A.y\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(res_at(&proj, &files, 2, "N.A.y"), "module private member");
}

/// Incremental ≡ batch when a `.fsi` edit flips a val's accessibility: the
/// screen and export surface both change, so the suffix must re-fold.
#[test]
fn incremental_matches_cold_when_val_access_flips_to_private() {
    let env = AssemblyEnv::default();
    let before = [
        ("/p/A.fsi", "module M\n\nval shown: int\n"),
        ("/p/A.fs", "module M\n\nlet shown = 1\n"),
        ("/p/B.fs", "module B\n\nlet u = M.shown\n"),
    ];
    let prev_files = project(&before);
    let prev = resolve_project_files(&prev_files, &env);

    let after = [
        ("/p/A.fsi", "module M\n\nval private shown: int\n"),
        ("/p/A.fs", "module M\n\nlet shown = 1\n"),
        ("/p/B.fs", "module B\n\nlet u = M.shown\n"),
    ];
    let new_files = project(&after);
    let (incr, _) = resolve_project_files_incremental(&prev_files, &prev, &new_files, &env);
    let cold = resolve_project_files(&new_files, &env);
    assert_eq!(incr, cold, "incremental ≡ batch after a val-access edit");
    assert_uncommitted(
        res_at(&cold, &after, 2, "M.shown"),
        "M.shown once the signature makes it private",
    );
}

/// The accessibility shapes as FCS differentials (certain-implies-exact plus
/// the agreement counts). The private/module-private fixtures are NOT
/// diagnostics-clean (FS1094/FS1092) — they are included with
/// `expected_cross_file: 0` precisely because FCS *does* declare the use in
/// the `.fsi` there: a wrong sig-identity commit on our side would match
/// FCS's decl and bump the count, failing the assertion.
#[test]
fn sig_accessibility_agrees_with_fcs() {
    let fixtures = [
        SigProject {
            label: "sig3_internal_val",
            files: vec![
                ("A.fsi", "module M\n\nval internal shown: int\n"),
                ("A.fs", "module M\n\nlet shown = 1\n"),
                ("Use.fs", "module Use\n\nlet a = M.shown\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        SigProject {
            label: "sig3_public_val",
            files: vec![
                ("A.fsi", "module M\n\nval public shown: int\n"),
                ("A.fs", "module M\n\nlet shown = 1\n"),
                ("Use.fs", "module Use\n\nlet a = M.shown\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        SigProject {
            label: "sig3_module_internal",
            files: vec![
                ("A.fsi", "module internal M\n\nval shown: int\n"),
                ("A.fs", "module M\n\nlet shown = 1\n"),
                ("Use.fs", "module Use\n\nlet a = M.shown\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        SigProject {
            label: "sig3_nsdirect_module_internal",
            files: vec![
                (
                    "A.fsi",
                    "namespace N\n\nmodule internal A =\n    val y: int\n",
                ),
                ("A.fs", "namespace N\n\nmodule A =\n    let y = 1\n"),
                ("Use.fs", "module Use\n\nlet b = N.A.y\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        // Diagnostics-clean (probed): the private val drops and First.fs's
        // public x is the binding.
        SigProject {
            label: "sig3_private_fragment",
            files: vec![
                ("First.fs", "namespace N\n\nmodule A =\n    let x = 1\n"),
                (
                    "P.fsi",
                    "namespace N\n\nmodule A =\n    val private x: int\n",
                ),
                ("P.fs", "namespace N\n\nmodule A =\n    let x = 2\n"),
                ("Use.fs", "module Use\n\nlet d = N.A.x\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        // Codex Stage-3 round 1, P1 (diagnostics-clean, probed): the
        // private val is accessible from a later fragment of its own
        // module — FCS declares the use at the private `.fsi` ident, and
        // so must we (a First.fs commit fails the exactness check).
        SigProject {
            label: "sig3_private_same_module_subtree",
            files: vec![
                ("First.fs", "namespace N\n\nmodule M =\n    let x = 1\n"),
                (
                    "P.fsi",
                    "namespace N\n\nmodule M =\n    val private x: int\n    val shown: int\n",
                ),
                (
                    "P.fs",
                    "namespace N\n\nmodule M =\n    let x = 2\n    let shown = 3\n",
                ),
                ("Later.fs", "namespace N\n\nmodule M =\n    let y = N.M.x\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        SigProject {
            label: "sig3_private_same_module_subtree_sole_fragment",
            files: vec![
                (
                    "P.fsi",
                    "namespace N\n\nmodule M =\n    val private x: int\n    val shown: int\n",
                ),
                (
                    "P.fs",
                    "namespace N\n\nmodule M =\n    let x = 2\n    let shown = 3\n",
                ),
                ("Later.fs", "namespace N\n\nmodule M =\n    let y = N.M.x\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        SigProject {
            label: "sig3_lone_private_val",
            files: vec![
                ("A.fsi", "module M\n\nval private x: int\n"),
                ("A.fs", "module M\n\nlet x = 1\n"),
                ("Use.fs", "module Use\n\nlet c = M.x\n"),
            ],
            refs: vec![],
            expected_cross_file: 0,
            fcs_must_not_declare: vec![],
        },
        SigProject {
            label: "sig3_module_private",
            files: vec![
                (
                    "A.fsi",
                    "namespace N\n\nmodule private A =\n    val y: int\n",
                ),
                ("A.fs", "namespace N\n\nmodule A =\n    let y = 1\n"),
                ("Use.fs", "module Use\n\nlet b = N.A.y\n"),
            ],
            refs: vec![],
            expected_cross_file: 0,
            fcs_must_not_declare: vec![],
        },
    ];
    for fixture in &fixtures {
        assert_sig_matches_fcs(fixture);
    }
}

/// The systematic sweep for this slice: accessibility × assembly collision ×
/// reading site, site-keyed against FCS. For every access flavour the
/// intervening reading falls through (assembly under a collision, unbound
/// otherwise); after the impl's slot the exposed flavours bind the `.fsi`
/// and `private` falls through — including the **clean** private+collision
/// assembly binding this slice exists to commit.
#[test]
fn accessibility_matrix_agrees_with_fcs() {
    let reflib = ensure_reflib_built();
    let mut sig_commits = 0usize;
    let mut assembly_commits = 0usize;
    let mut deferrals = 0usize;
    for access in ["", "public ", "internal ", "private "] {
        for collision in [false, true] {
            let sig_src = format!("module ProbeNs.Shared\n\nval {access}bar: int\n");
            let files: Vec<(&str, &str)> = vec![
                ("A.fsi", sig_src.as_str()),
                (
                    "Between.fs",
                    "module Between\n\nlet g = ProbeNs.Shared.bar\n",
                ),
                ("A.fs", "module ProbeNs.Shared\n\nlet bar = 2\n"),
                ("After.fs", "module After\n\nlet h = ProbeNs.Shared.bar\n"),
            ];
            let label = format!(
                "sig3acc_{}_{}",
                access.trim().replace(' ', "_"),
                if collision { "coll" } else { "nocoll" }
            );
            let (root, written) = temp_fs_tree(&label, &files);
            let paths: Vec<&Path> = written.iter().map(|(path, _)| path.as_path()).collect();
            let refs: Vec<&Path> = if collision { vec![reflib] } else { vec![] };
            let json = invoke_fcs_dump_project_with_refs(&paths, &refs);
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
            let env = if collision {
                reflib_env()
            } else {
                AssemblyEnv::default()
            };
            let proj = resolve_project_files(&input, &env);
            let _ = std::fs::remove_dir_all(&root);

            // Site-keyed: for each reading site, our verdict must match
            // FCS's — in-project decl ⇒ exact Item or deferral; assembly ⇒
            // assembly member or deferral; unbound ⇒ nothing.
            for (site_idx, needle_owner) in [(1usize, "Between.fs"), (3usize, "After.fs")] {
                let (site_path, site_src) = &written[site_idx];
                assert_eq!(
                    site_path.file_name().and_then(|n| n.to_str()),
                    Some(needle_owner),
                    "fixture rows moved"
                );
                let needle = "ProbeNs.Shared.bar";
                let start = site_src.find(needle).expect("probe site present");
                let site = span(start, start + needle.len());
                let fcs_at_site = fcs_files
                    .iter()
                    .find(|f| f.path.file_name() == site_path.file_name())
                    .and_then(|f| {
                        f.uses.iter().find(|u| {
                            u.start == usize::from(site.start()) && u.end == usize::from(site.end())
                        })
                    });
                let ours = proj.file(site_idx).resolution_at(site);
                let what = format!("{label}: {needle_owner}");
                match fcs_at_site {
                    Some(u) if u.decl.is_some() => {
                        let decl = u.decl.as_ref().expect("checked");
                        match ours {
                            None | Some(Resolution::Deferred(_)) => deferrals += 1,
                            Some(res @ Resolution::Item(_)) => {
                                let (idx, def) = proj.item_def(res).expect("item def");
                                assert_eq!(
                                    written[idx].0.file_name(),
                                    decl.file.file_name(),
                                    "{what}: wrong declaring file"
                                );
                                assert_eq!(
                                    def.range,
                                    span(decl.start, decl.end),
                                    "{what}: wrong def range"
                                );
                                sig_commits += 1;
                            }
                            other => panic!(
                                "{what}: FCS declares in-project at {:?}, we committed {other:?}",
                                decl.file
                            ),
                        }
                    }
                    Some(u) if u.assembly.as_deref() == Some("SemaSignatureRefLib") => match ours {
                        None | Some(Resolution::Deferred(_)) => deferrals += 1,
                        Some(Resolution::Member { .. } | Resolution::Entity(_)) => {
                            assembly_commits += 1;
                        }
                        other => panic!(
                            "{what}: FCS binds the assembly, we committed {other:?} in-project"
                        ),
                    },
                    _ => match ours {
                        None | Some(Resolution::Deferred(_)) => deferrals += 1,
                        other => panic!("{what}: FCS is unbound here, we committed {other:?}"),
                    },
                }
            }
        }
    }
    // Non-vacuity floors: the exposed-after cells commit the sig identity
    // (6: three exposed flavours × both collision states), the collision
    // fall-throughs commit the assembly (4: three exposed intervening + the
    // private cells), the unbound cells defer.
    assert!(sig_commits >= 6, "sig-identity commits: {sig_commits}");
    assert!(
        assembly_commits >= 4,
        "assembly commits: {assembly_commits}"
    );
    assert!(deferrals >= 4, "deferrals: {deferrals}");
}
