//! Stage 1 of `docs/fsi-signature-restriction-plan.md`: `.fsi` signature
//! files interleave into the Compile-order fold, and a paired
//! implementation's **value/case identity exports are dropped** at the
//! cross-file boundary — a signature-hidden member no longer resolves to the
//! impl binder. The signature itself is inert (Stage 2 exports its surface),
//! so a signatured module's members resolve to:
//!
//! - the **merged referenced assembly**, when the assembly provides the path
//!   and the signature provably cannot expose it (the name is absent from the
//!   signature's token set) — FCS-probed: hidden `Shared.bar` → the assembly;
//! - **`Deferred`** otherwise — including every name the signature *may*
//!   expose (the Stage-1 **screen**: FCS binds the `.fsi` even when a
//!   referenced assembly collides, probe `Shared.shown` → the `.fsi`, so an
//!   assembly commit there would be wrong).
//!
//! Pairing is FCS's `QualifiedNameOfFile`: a module-headed file is named by
//! its module path, anything else by its capitalised filename stem, then the
//! per-directory deduplication suffixes `___<n>` — pinned here both FCS-free
//! (the fold's observable behaviour) and differentially (the
//! `*_agrees_with_fcs` tests feed FCS real `.fsi`-bearing file sets and
//! assert certain-implies-exact plus non-vacuous agreement counts).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_oracle_harness::BoundedCommand;
use borzoi_sema::{
    AssemblyEnv, ProjectFile, Resolution, ResolvedProject, SourceFile, qualified_names,
    resolve_project_files, resolve_project_files_incremental, resolve_project_files_prefix,
};
use rowan::TextRange;

use crate::common::{invoke_fcs_dump_project_with_refs, parse_fcs_uses_project, temp_fs_tree};

/// Parse one Compile item under the grammar its path selects and wrap it.
fn source_file(path: &str, src: &str) -> SourceFile {
    if path.ends_with(".fsi") {
        let p = parse_sig(src);
        assert!(
            p.errors.is_empty(),
            "sig parse errors in {src:?}: {:?}",
            p.errors
        );
        SourceFile::Sig(SigFile::cast(p.root).expect("sig file"))
    } else {
        let p = parse(src);
        assert!(
            p.errors.is_empty(),
            "parse errors in {src:?}: {:?}",
            p.errors
        );
        SourceFile::Impl(ImplFile::cast(p.root).expect("impl file"))
    }
}

/// Build the fold input from `(path, source)` rows: parse each file under its
/// extension's grammar and derive the QNOFs over the whole Compile order.
fn project(files: &[(&str, &str)]) -> Vec<ProjectFile> {
    let srcs: Vec<SourceFile> = files.iter().map(|(p, s)| source_file(p, s)).collect();
    let paths: Vec<PathBuf> = files.iter().map(|(p, _)| PathBuf::from(p)).collect();
    let qnofs = qualified_names(&srcs, &paths);
    srcs.into_iter()
        .zip(qnofs)
        .map(|(file, qnof)| ProjectFile::new(file, qnof))
        .collect()
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// The resolution recorded over the (unique) occurrence of `needle` in file
/// `file_idx`'s source.
fn res_at(
    proj: &ResolvedProject,
    files: &[(&str, &str)],
    file_idx: usize,
    needle: &str,
) -> Option<Resolution> {
    let src = files[file_idx].1;
    let start = src.find(needle).expect("needle present in source");
    assert!(
        src[start + 1..].find(needle).is_none(),
        "needle {needle:?} is ambiguous in {src:?}"
    );
    proj.file(file_idx)
        .resolution_at(span(start, start + needle.len()))
}

/// Assert the use is *not* committed to any project binder — `None` or
/// `Deferred`, never `Item`/`Local` (and, with an empty env, never assembly).
fn assert_uncommitted(res: Option<Resolution>, what: &str) {
    match res {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("{what}: expected no commitment, got {other:?}"),
    }
}

/// Assert the use committed to a cross-file `Item` declared by file
/// `def_idx`.
fn assert_item_in(proj: &ResolvedProject, res: Option<Resolution>, def_idx: usize, what: &str) {
    let res = res.unwrap_or_else(|| panic!("{what}: expected an Item, got no resolution"));
    assert!(
        matches!(res, Resolution::Item(_)),
        "{what}: expected an Item, got {res:?}"
    );
    let (idx, _) = proj.item_def(res).expect("item def");
    assert_eq!(idx, def_idx, "{what}: wrong declaring file");
}

// ---------------------------------------------------------------------------
// FCS-free: the drop, the pairing, and the screen (empty assembly env).
// ---------------------------------------------------------------------------

/// The core Stage-1 behaviour change: a signatured module's members — hidden
/// *and* exposed — no longer resolve to the impl binder (the sig exports
/// nothing yet; the exposed half becomes Stage 2's signature-identity
/// commit).
#[test]
fn signature_drops_paired_impl_value_exports() {
    let files = [
        ("/p/A.fsi", "module A\n\nval shown: int\n"),
        ("/p/A.fs", "module A\n\nlet shown = 1\nlet hidden = 2\n"),
        (
            "/p/B.fs",
            "module B\n\nlet u1 = A.shown\nlet u2 = A.hidden\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(res_at(&proj, &files, 2, "A.shown"), "sig-exposed A.shown");
    assert_uncommitted(res_at(&proj, &files, 2, "A.hidden"), "sig-hidden A.hidden");
    // The signature slot itself is inert.
    assert!(proj.file(0).exports().is_empty());
    // The impl's own surface is untouched (conclusion 2) — only the boundary
    // contribution is dropped.
    assert_eq!(proj.file(1).exports().len(), 2);
}

/// Control: the identical project without the `.fsi` still exports both
/// values — the drop is signature-caused, not a general regression.
#[test]
fn unsigned_project_still_exports() {
    let files = [
        ("/p/A.fs", "module A\n\nlet shown = 1\nlet hidden = 2\n"),
        (
            "/p/B.fs",
            "module B\n\nlet u1 = A.shown\nlet u2 = A.hidden\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_item_in(&proj, res_at(&proj, &files, 1, "A.shown"), 0, "A.shown");
    assert_item_in(&proj, res_at(&proj, &files, 1, "A.hidden"), 0, "A.hidden");
}

/// An unsigned sibling module in a `.fsi`-bearing project is untouched
/// (probes J/M): only the *paired* impl's boundary changes.
#[test]
fn unsigned_sibling_module_still_exports() {
    let files = [
        ("/p/A.fsi", "module A\n\nval shown: int\n"),
        ("/p/A.fs", "module A\n\nlet shown = 1\n"),
        ("/p/C.fs", "module C\n\nlet c = 3\n"),
        ("/p/B.fs", "module B\n\nlet u1 = C.c\nlet u2 = A.shown\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_item_in(&proj, res_at(&proj, &files, 3, "C.c"), 2, "C.c");
    assert_uncommitted(res_at(&proj, &files, 3, "A.shown"), "A.shown");
}

/// Probe X3: pairing is first-following-impl of equal QNOF. A same-named
/// `module M` fragment in *another directory* deduplicates to `M___2`, so it
/// is an independent unsigned fragment — its exports survive.
#[test]
fn pairing_is_first_following_impl_of_equal_qnof() {
    let files = [
        ("/d1/Pair.fsi", "module M\n\nval shown: int\n"),
        ("/d1/Pair.fs", "module M\n\nlet shown = 1\nlet hidden = 2\n"),
        ("/d2/Extra.fs", "module M\n\nlet extra = 3\n"),
        (
            "/u/Use.fs",
            "module Use\n\nlet a = M.shown\nlet b = M.hidden\nlet c = M.extra\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(res_at(&proj, &files, 3, "M.shown"), "M.shown");
    assert_uncommitted(res_at(&proj, &files, 3, "M.hidden"), "M.hidden");
    assert_item_in(&proj, res_at(&proj, &files, 3, "M.extra"), 2, "M.extra");
}

/// The dual of probe X3, pinned against FCS's `DeduplicateModuleName` source:
/// a module-headed sig and impl in *different directories* deduplicate apart
/// (`M` vs `M___2`), so they do **not** pair — the impl exports fully.
/// (Differentially confirmed by
/// [`cross_directory_module_headed_sig_agrees_with_fcs`].)
#[test]
fn cross_directory_module_headed_sig_does_not_pair() {
    let files = [
        ("/d1/Sig.fsi", "module M\n\nval shown: int\n"),
        ("/d2/Imp.fs", "module M\n\nlet shown = 1\nlet hidden = 2\n"),
        (
            "/d3/Use.fs",
            "module Use\n\nlet a = M.shown\nlet b = M.hidden\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_item_in(&proj, res_at(&proj, &files, 2, "M.shown"), 1, "M.shown");
    assert_item_in(&proj, res_at(&proj, &files, 2, "M.hidden"), 1, "M.hidden");
}

/// A namespace-headed pair goes through the filename-derived QNOF
/// (`A.fsi`/`A.fs` → `A`; probes G/G2) — the paired module's exports drop.
#[test]
fn namespace_headed_signature_pairs_by_filename() {
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
    assert_uncommitted(res_at(&proj, &files, 2, "N.A.shown"), "N.A.shown");
    assert_uncommitted(res_at(&proj, &files, 2, "N.A.hidden"), "N.A.hidden");
}

/// Filename-derivation control: rename the signature so the stems differ and
/// nothing pairs — the impl exports fully.
#[test]
fn namespace_headed_signature_with_other_stem_does_not_pair() {
    let files = [
        (
            "/p/Other.fsi",
            "namespace N\n\nmodule A =\n    val shown: int\n",
        ),
        (
            "/p/A.fs",
            "namespace N\n\nmodule A =\n    let shown = 1\n    let hidden = 2\n",
        ),
        ("/p/B.fs", "module B\n\nlet u2 = N.A.hidden\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_item_in(
        &proj,
        res_at(&proj, &files, 2, "N.A.hidden"),
        1,
        "N.A.hidden",
    );
}

/// `open` of a signatured module: the module header survives (the open is not
/// an unknown-module error path), and bare uses of dropped values defer.
#[test]
fn open_of_signatured_module_defers_bare_uses() {
    let files = [
        ("/p/A.fsi", "module A\n\nval shown: int\n"),
        ("/p/A.fs", "module A\n\nlet shown = 1\n"),
        ("/p/B.fs", "module B\n\nopen A\n\nlet u = shown\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(res_at(&proj, &files, 2, "shown"), "bare shown after open");
}

/// Codex review P1 (probe 2026-07-18): a signatured *relative* module
/// outranks a root module of the same simple name. Inside `namespace A`,
/// `M.x` with a root `module M; let x` and a signatured `module A.M`
/// exposing `x` binds the `.fsi` in FCS — so the root fallback must not
/// commit while the relative reading is screened.
#[test]
fn screened_relative_reading_withholds_root_module_commit() {
    let files = [
        ("/p/M.fs", "module M\n\nlet x = 0\n"),
        ("/p/AM.fsi", "module A.M\n\nval x: int\n"),
        ("/p/AM.fs", "module A.M\n\nlet x = 1\n"),
        (
            "/p/Use.fs",
            "namespace A\n\nmodule Use =\n    let y = M.x\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(
        res_at(&proj, &files, 3, "M.x"),
        "M.x with a screened relative reading (FCS binds the .fsi)",
    );

    // Control: without the signature the relative module commits normally.
    let control = [
        ("/p/M.fs", "module M\n\nlet x = 0\n"),
        ("/p/AM.fs", "module A.M\n\nlet x = 1\n"),
        (
            "/p/Use.fs",
            "namespace A\n\nmodule Use =\n    let y = M.x\n",
        ),
    ];
    let cproj = resolve_project_files(&project(&control), &AssemblyEnv::default());
    assert_item_in(
        &cproj,
        res_at(&cproj, &control, 2, "M.x"),
        1,
        "unsigned relative M.x",
    );
}

/// Codex review P2 (probe 2026-07-18): an implementation-only `[<AutoOpen>]`
/// is ignored by FCS (the bare use is FS0039 — the signature's verdict is
/// authoritative in both directions), so the paired module publishes no
/// auto-open and an earlier open's value must stay committed rather than be
/// staled by a phantom auto-open fold.
#[test]
fn impl_only_auto_open_is_not_published() {
    let files = [
        ("/p/Lib.fs", "module Lib\n\nlet marker = 1\n"),
        ("/p/A.fsi", "namespace N\n\nmodule A =\n    val x: int\n"),
        (
            "/p/A.fs",
            "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let x = 1\n",
        ),
        (
            "/p/Use.fs",
            "module U\n\nopen Lib\nopen N\n\nlet y = marker\n",
        ),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_item_in(
        &proj,
        res_at(&proj, &files, 3, "marker"),
        0,
        "marker after `open N` (the impl-only auto-open publishes nothing)",
    );
}

// ---------------------------------------------------------------------------
// The assembly merge: fall-through for provably-hidden names, the screen for
// possibly-exposed ones (the built RefLib fixture; FCS-probed 2026-07-18).
// ---------------------------------------------------------------------------

/// Budget for one fixture `dotnet build` — see `resolve_fsharp_abbrev.rs`'s
/// identically-motivated bound.
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

fn ensure_reflib_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signature_reflib");
            let mut cmd = Command::new("dotnet");
            cmd.args(["build", "-c", "Release", "--nologo"])
                .arg(&project);
            BoundedCommand::new(cmd)
                .timeout(BUILD_TIMEOUT)
                .run_ok("dotnet build signature RefLib fixture");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("SemaSignatureRefLib.dll")
        })
        .as_path()
}

fn reflib_env() -> AssemblyEnv {
    let bytes = std::fs::read(ensure_reflib_built()).expect("read signature RefLib fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse signature RefLib fixture dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv")
}

/// The RefLib collision project: `ProbeNs.Shared` exists both as the
/// signatured project module and in the referenced assembly (members `shown`,
/// `bar`, `asmOnly`; the sig exposes only `shown`, the impl also defines
/// `bar`).
fn reflib_project_files() -> [(&'static str, &'static str); 3] {
    [
        ("/p/A.fsi", "module ProbeNs.Shared\n\nval shown: int\n"),
        (
            "/p/A.fs",
            "module ProbeNs.Shared\n\nlet shown = 1\nlet bar = 2\n",
        ),
        (
            "/p/Use.fs",
            "module Use\n\nlet a = ProbeNs.Shared.shown\nlet b = ProbeNs.Shared.bar\nlet c = ProbeNs.Shared.asmOnly\n",
        ),
    ]
}

/// The probe matrix (FCS `uses-project`, 2026-07-18, RefLib collision):
///
/// | use | FCS | Stage 1 |
/// |---|---|---|
/// | `Shared.shown` (sig-exposed, in assembly) | the `.fsi` | **Deferred** (the screen — an assembly commit would be wrong) |
/// | `Shared.bar` (hidden, in assembly) | the assembly | the assembly (`Member`) |
/// | `Shared.asmOnly` (assembly only) | the assembly | the assembly (`Member`) |
#[test]
fn hidden_member_falls_through_to_assembly_and_exposed_member_is_screened() {
    let files = reflib_project_files();
    let env = reflib_env();
    let proj = resolve_project_files(&project(&files), &env);

    assert_uncommitted(
        res_at(&proj, &files, 2, "ProbeNs.Shared.shown"),
        "sig-exposed shown with an assembly collision (FCS binds the .fsi)",
    );
    assert!(
        matches!(
            res_at(&proj, &files, 2, "ProbeNs.Shared.bar"),
            Some(Resolution::Member { .. })
        ),
        "sig-hidden bar must fall through to the merged assembly member (probe), got {:?}",
        res_at(&proj, &files, 2, "ProbeNs.Shared.bar"),
    );
    assert!(
        matches!(
            res_at(&proj, &files, 2, "ProbeNs.Shared.asmOnly"),
            Some(Resolution::Member { .. })
        ),
        "assembly-only asmOnly must resolve to the assembly member, got {:?}",
        res_at(&proj, &files, 2, "ProbeNs.Shared.asmOnly"),
    );
}

/// The `open` half of the same probe. FCS binds bare `shown` to the `.fsi`
/// and bare `bar`/`asmOnly` to RefLib. Stage 1 commits **none of them**: the
/// signatured module is hidden-valued (its exports are dropped but the
/// signature may expose names sema cannot enumerate yet), so opening it takes
/// the conservative project-module-open path (`opaque_value_open`) and every
/// later bare name defers. That deferral is load-bearing — a sig-exposed name
/// must shadow an earlier open's same-named value, and with no signature
/// exports the opaque/generation machinery is the only lever — and it is
/// what keeps bare `shown` off the assembly (FCS binds the `.fsi`). The
/// `bar`/`asmOnly` deferrals are the honest D5 cost on this path (the
/// *qualified* form does commit them to the assembly — the test above);
/// Stage 2's signature exports sharpen the open fold.
#[test]
fn open_of_signatured_module_screens_bare_names_from_the_assembly() {
    let files = [
        ("/p/A.fsi", "module ProbeNs.Shared\n\nval shown: int\n"),
        (
            "/p/A.fs",
            "module ProbeNs.Shared\n\nlet shown = 1\nlet bar = 2\n",
        ),
        (
            "/p/Use.fs",
            "module Use\n\nopen ProbeNs.Shared\n\nlet d = shown\nlet e = bar\nlet f = asmOnly\n",
        ),
    ];
    let env = reflib_env();
    let proj = resolve_project_files(&project(&files), &env);

    assert_uncommitted(
        res_at(&proj, &files, 2, "shown"),
        "bare sig-exposed shown after open (FCS binds the .fsi — an assembly \
         commit would be wrong)",
    );
    assert_uncommitted(
        res_at(&proj, &files, 2, "bar"),
        "bare sig-hidden bar after open (FCS binds the assembly; Stage 1 \
         defers on the open path — never the impl binder)",
    );
    assert_uncommitted(
        res_at(&proj, &files, 2, "asmOnly"),
        "bare assembly-only asmOnly after open (deferred on the open path)",
    );
}

/// Codex review P1 (FCS-checked in review): a **namespace-direct case** the
/// signature exposes owns its path outright. With sig/impl `namespace
/// ProbeNs; type Color = Shared` and RefLib's `ProbeNs.Shared.shown`, FCS
/// binds `Shared` to the `.fsi` case and FS0039s the member — an assembly
/// commit anywhere at or under the case path is wrong. (The sig spelling
/// parses as an abbreviation whose single-ident target FCS reads as a
/// nullary case, so the screen's value-path collection must cover that
/// shape too.)
#[test]
fn namespace_direct_case_screens_assembly_paths() {
    let files = [
        ("/p/A.fsi", "namespace ProbeNs\n\ntype Color = Shared\n"),
        ("/p/A.fs", "namespace ProbeNs\n\ntype Color = Shared\n"),
        ("/p/Use.fs", "module Use\n\nlet a = ProbeNs.Shared.shown\n"),
    ];
    let proj = resolve_project_files(&project(&files), &reflib_env());
    assert_uncommitted(
        res_at(&proj, &files, 2, "ProbeNs.Shared.shown"),
        "member path under a sig-exposed namespace-direct case",
    );
    // The `Shared` segment itself must not carry an assembly Entity either.
    assert_uncommitted(
        res_at(&proj, &files, 2, "Shared"),
        "the case segment under a sig-exposed namespace-direct case",
    );
}

/// Timing (probe L + the intervening-collision probe): between the sig and
/// the impl the module has not published, and FCS resolves an assembly
/// collision to the assembly. The Stage-1 screen is pushed at the *sig's*
/// slot, so sema defers there instead — sound (never a wrong commit), just
/// under-resolved; this pins that it at least never commits the *impl*.
#[test]
fn intervening_file_between_sig_and_impl_commits_nothing_in_project() {
    let files = [
        ("/p/A.fsi", "module ProbeNs.Shared\n\nval shown: int\n"),
        (
            "/p/Between.fs",
            "module Between\n\nlet g = ProbeNs.Shared.shown\n",
        ),
        ("/p/A.fs", "module ProbeNs.Shared\n\nlet shown = 1\n"),
    ];
    let proj = resolve_project_files(&project(&files), &AssemblyEnv::default());
    assert_uncommitted(
        res_at(&proj, &files, 1, "ProbeNs.Shared.shown"),
        "intervening use of a not-yet-published signatured module",
    );
}

/// Codex review round 2: a **prefix** fold derives the pairing from the
/// whole Compile list, so a signature whose implementation lies past the
/// horizon still publishes its screen — every folded slot answers exactly
/// like the full fold's same slot (prefix-monotonicity), instead of the
/// answer depending on the query depth that populated a cache.
#[test]
fn prefix_fold_pairs_from_the_whole_compile_list() {
    let files = [
        ("/p/M.fs", "module M\n\nlet x = 0\n"),
        ("/p/AM.fsi", "module A.M\n\nval x: int\n"),
        (
            "/p/Between.fs",
            "namespace A\n\nmodule Between =\n    let y = M.x\n",
        ),
        ("/p/AM.fs", "module A.M\n\nlet x = 1\n"),
    ];
    let input = project(&files);
    let env = AssemblyEnv::default();
    let full = resolve_project_files(&input, &env);
    for len in 1..=input.len() {
        let prefix = resolve_project_files_prefix(&input, len, &env);
        assert_eq!(prefix.len(), len);
        for i in 0..len {
            assert_eq!(
                prefix.file(i),
                full.file(i),
                "prefix fold (len {len}) diverges from the full fold at slot {i}"
            );
        }
    }
    // The screened intervening use defers — in the full fold and, by the
    // equality above, in every prefix that contains it.
    assert_uncommitted(
        res_at(&full, &files, 2, "M.x"),
        "intervening M.x under a screen whose impl lies later",
    );
}

// ---------------------------------------------------------------------------
// Incremental ≡ batch across signature edits.
// ---------------------------------------------------------------------------

/// A `.fsi` edit changes the paired impl's boundary contribution, so the
/// suffix must re-resolve — and the incremental result must equal a cold
/// fold. A body-only impl edit keeps the suffix reusable.
#[test]
fn incremental_matches_cold_across_signature_edits() {
    let env = AssemblyEnv::default();
    let v1 = [
        ("/p/A.fsi", "module A\n\nval shown: int\n"),
        ("/p/A.fs", "module A\n\nlet shown = 1\nlet hidden = 2\n"),
        ("/p/B.fs", "module B\n\nlet u = A.shown\n"),
    ];
    let prev_files = project(&v1);
    let prev = resolve_project_files(&prev_files, &env);

    // Sig-content edit: reparse only the sig; reuse the impl trees verbatim.
    let v2_sig = source_file("/p/A.fsi", "module A\n\nval hidden: int\n");
    let mut new_files = prev_files.clone();
    new_files[0] = ProjectFile::new(v2_sig, new_files[0].qnof.clone());
    let (incr, reused) = resolve_project_files_incremental(&prev_files, &prev, &new_files, &env);
    let cold = resolve_project_files(&new_files, &env);
    assert_eq!(incr, cold, "incremental ≡ batch after a .fsi edit");
    assert!(
        !reused[2],
        "a screen-changing .fsi edit must invalidate the suffix"
    );

    // Body-only impl edit (same exports): the suffix stays reusable.
    let v3_impl = source_file(
        "/p/A.fs",
        "module A\n\nlet shown = (let t = 1 in t)\nlet hidden = 2\n",
    );
    let mut new_files3 = prev_files.clone();
    new_files3[1] = ProjectFile::new(v3_impl, new_files3[1].qnof.clone());
    let (incr3, reused3) = resolve_project_files_incremental(&prev_files, &prev, &new_files3, &env);
    assert_eq!(
        incr3,
        resolve_project_files(&new_files3, &env),
        "incremental ≡ batch after a body-only impl edit"
    );
    assert!(
        reused3[2],
        "a body-only impl edit must keep the suffix reusable"
    );
}

/// Adding / removing the signature itself re-pairs, so the impl (and suffix)
/// re-resolve; the incremental result must equal a cold fold of the new set.
#[test]
fn incremental_matches_cold_when_signature_is_added() {
    let env = AssemblyEnv::default();
    let without = [
        ("/p/A.fs", "module A\n\nlet shown = 1\nlet hidden = 2\n"),
        ("/p/B.fs", "module B\n\nlet u = A.hidden\n"),
    ];
    let prev_files = project(&without);
    let prev = resolve_project_files(&prev_files, &env);

    let with = [
        ("/p/A.fsi", "module A\n\nval shown: int\n"),
        ("/p/A.fs", "module A\n\nlet shown = 1\nlet hidden = 2\n"),
        ("/p/B.fs", "module B\n\nlet u = A.hidden\n"),
    ];
    let new_files = project(&with);
    let (incr, _) = resolve_project_files_incremental(&prev_files, &prev, &new_files, &env);
    let cold = resolve_project_files(&new_files, &env);
    assert_eq!(incr, cold, "incremental ≡ batch after inserting a .fsi");
    assert_uncommitted(
        res_at(&cold, &with, 2, "A.hidden"),
        "A.hidden once the signature pairs",
    );
}

// ---------------------------------------------------------------------------
// FCS differentials: pairing and the drop, certain-implies-exact.
// ---------------------------------------------------------------------------

/// One signature-aware differential fixture: files (relative path → source),
/// how many cross-file commits must agree exactly with FCS (non-vacuity), and
/// which use texts FCS itself must leave *without* an in-project declaration
/// (pinning that the FCS-side hiding genuinely fired).
struct SigProject {
    label: &'static str,
    files: Vec<(&'static str, &'static str)>,
    refs: Vec<&'static Path>,
    expected_cross_file: usize,
    fcs_must_not_declare: Vec<&'static str>,
}

/// The signature-aware sibling of `resolve_project_diff`'s
/// `assert_matches_fcs`: materialise the tree (real stems + directories — the
/// QNOF inputs), feed FCS the interleaved Compile order, resolve with the
/// signature-aware fold, and assert certain-implies-exact plus the expected
/// agreement count.
fn assert_sig_matches_fcs(p: &SigProject) {
    let (root, written) = temp_fs_tree(p.label, &p.files);
    let paths: Vec<&Path> = written.iter().map(|(path, _)| path.as_path()).collect();

    let json = invoke_fcs_dump_project_with_refs(&paths, &p.refs);
    let fcs_files = parse_fcs_uses_project(&json, &written);

    let srcs: Vec<SourceFile> = p
        .files
        .iter()
        .zip(&written)
        .map(|((rel, src), _)| source_file(rel, src))
        .collect();
    let full_paths: Vec<PathBuf> = written.iter().map(|(path, _)| path.clone()).collect();
    let qnofs = qualified_names(&srcs, &full_paths);
    let input: Vec<ProjectFile> = srcs
        .into_iter()
        .zip(qnofs)
        .map(|(file, qnof)| ProjectFile::new(file, qnof))
        .collect();
    let env = if p.refs.is_empty() {
        AssemblyEnv::default()
    } else {
        reflib_env()
    };
    let proj = resolve_project_files(&input, &env);

    let _ = std::fs::remove_dir_all(&root);

    // FCS-side hiding really fired: no use whose text matches a
    // `fcs_must_not_declare` entry may carry an in-project decl.
    for (path, src) in &written {
        let Some(fu) = fcs_files
            .iter()
            .find(|f| f.path.file_name() == path.file_name())
        else {
            continue;
        };
        for u in &fu.uses {
            let Some(decl) = &u.decl else { continue };
            let use_text = &src[u.start..u.end];
            assert!(
                !p.fcs_must_not_declare.contains(&use_text),
                "{}: FCS declared {use_text:?} in-project at {:?} — the fixture's \
                 hiding premise is wrong",
                p.label,
                decl.file,
            );
        }
    }

    let mut cross_file_agreed = 0usize;
    for (i, (path, _)) in written.iter().enumerate() {
        let Some(fu) = fcs_files
            .iter()
            .find(|f| f.path.file_name() == path.file_name())
        else {
            continue;
        };
        let rf = proj.file(i);
        for u in &fu.uses {
            if u.start == u.end {
                continue;
            }
            let Some(decl) = &u.decl else { continue };
            let use_range = span(u.start, u.end);
            match rf.resolution_at(use_range) {
                None | Some(Resolution::Deferred(_)) => {}
                Some(Resolution::Unresolved) => {
                    panic!(
                        "{}: Unresolved where FCS resolved: {:?} at {use_range:?} in {path:?}",
                        p.label, u.name
                    );
                }
                Some(res @ (Resolution::Item(_) | Resolution::Local(_))) => {
                    let (def_idx, def_range) = match res {
                        Resolution::Item(_) => {
                            let (idx, def) = proj.item_def(res).expect("item def for Item");
                            (idx, def.range)
                        }
                        _ => (i, rf.resolved_def(res).expect("local def").range),
                    };
                    let def_path = &written[def_idx].0;
                    assert_eq!(
                        def_path.file_name(),
                        decl.file.file_name(),
                        "{}: use {:?} at {use_range:?}: we point into {def_path:?}, \
                         FCS declares in {:?}",
                        p.label,
                        u.name,
                        decl.file,
                    );
                    assert_eq!(
                        def_range,
                        span(decl.start, decl.end),
                        "{}: use {:?}: def range disagrees with FCS",
                        p.label,
                        u.name,
                    );
                    if decl.file.file_name() != path.file_name() {
                        cross_file_agreed += 1;
                    }
                }
                // Assembly commits are checked against FCS's assembly verdict:
                // FCS must NOT have an in-project decl for them, and we only
                // reach here when it does — so an assembly commit at an
                // in-project-declared use is a wrong commit.
                Some(res @ (Resolution::Entity(_) | Resolution::Member { .. })) => {
                    panic!(
                        "{}: assembly commit {res:?} at {use_range:?} ({:?}) where FCS \
                         declares in-project at {:?}",
                        p.label, u.name, decl.file,
                    );
                }
            }
        }
    }
    assert_eq!(
        cross_file_agreed, p.expected_cross_file,
        "{}: cross-file agreements",
        p.label,
    );
}

/// The core pairing shapes, FCS-differential. Uses of hidden members are FCS
/// errors (FS0039) — `uses-project` then reports no in-project decl for them,
/// which `fcs_must_not_declare` pins; our side never commits them (the
/// certain-implies-exact loop would fail on any commit FCS lacks a matching
/// decl for).
#[test]
fn signature_pairing_agrees_with_fcs() {
    let fixtures = [
        // Module-headed pair, same directory: `shown` declares in the .fsi
        // (we defer — allowed); `hidden` is FS0039 (no in-project decl).
        SigProject {
            label: "sigdiff_pair",
            files: vec![
                ("A.fsi", "module M\n\nval shown: int\n"),
                ("A.fs", "module M\n\nlet shown = 1\nlet hidden = 2\n"),
                (
                    "Use.fs",
                    "module Use\n\nlet a = M.shown\nlet b = M.hidden\n",
                ),
            ],
            refs: vec![],
            expected_cross_file: 0,
            fcs_must_not_declare: vec!["M.hidden"],
        },
        // Probe X3: the d2 fragment deduplicates apart and stays unsigned —
        // `M.extra` must agree exactly (non-vacuity: 1 cross-file commit).
        SigProject {
            label: "sigdiff_x3",
            files: vec![
                ("d1/Pair.fsi", "module M\n\nval shown: int\n"),
                ("d1/Pair.fs", "module M\n\nlet shown = 1\nlet hidden = 2\n"),
                ("d2/Extra.fs", "module M\n\nlet extra = 3\n"),
                ("Use.fs", "module Use\n\nlet a = M.shown\nlet c = M.extra\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        // Cross-directory module-headed sig/impl: FCS deduplicates them apart
        // (probed 2026-07-18: `M.hidden` resolves to Imp.fs), so nothing
        // pairs and BOTH uses must agree exactly — this pins the
        // `DeduplicateModuleName` port in the direction where over-pairing
        // would silently defer (the agreement count catches it).
        SigProject {
            label: "sigdiff_crossdir",
            files: vec![
                ("d1/Sig.fsi", "module M\n\nval shown: int\n"),
                ("d2/Imp.fs", "module M\n\nlet shown = 1\nlet hidden = 2\n"),
                (
                    "d3/Use.fs",
                    "module Use\n\nlet a = M.shown\nlet b = M.hidden\n",
                ),
            ],
            refs: vec![],
            expected_cross_file: 2,
            fcs_must_not_declare: vec![],
        },
        // Namespace-headed pair (filename-derived QNOF, probes G/G2):
        // `shown` declares in the .fsi (we defer); `hidden` is FS0039.
        SigProject {
            label: "sigdiff_ns",
            files: vec![
                ("A.fsi", "namespace N\n\nmodule A =\n    val shown: int\n"),
                (
                    "A.fs",
                    "namespace N\n\nmodule A =\n    let shown = 1\n    let hidden = 2\n",
                ),
                (
                    "Use.fs",
                    "module Use\n\nlet a = N.A.shown\nlet b = N.A.hidden\n",
                ),
            ],
            refs: vec![],
            expected_cross_file: 0,
            fcs_must_not_declare: vec!["N.A.hidden"],
        },
        // Filename-derivation control: a different stem does not pair, so the
        // impl's members resolve to it — 2 exact cross-file agreements (an
        // over-pairing port would defer and fail the count).
        SigProject {
            label: "sigdiff_ns_other",
            files: vec![
                (
                    "Other.fsi",
                    "namespace N\n\nmodule A =\n    val shown: int\n",
                ),
                (
                    "A.fs",
                    "namespace N\n\nmodule A =\n    let shown = 1\n    let hidden = 2\n",
                ),
                (
                    "Use.fs",
                    "module Use\n\nlet a = N.A.shown\nlet b = N.A.hidden\n",
                ),
            ],
            refs: vec![],
            expected_cross_file: 2,
            fcs_must_not_declare: vec![],
        },
        // Codex review P1: inside `namespace A`, `M.x` binds the signatured
        // relative `A.M` (the `.fsi`), not the root `module M` — the
        // project-side screen veto (we defer; a root `Item` commit would
        // fail the exactness check against FCS's `.fsi` decl).
        SigProject {
            label: "sigdiff_relative",
            files: vec![
                ("M.fs", "module M\n\nlet x = 0\n"),
                ("AM.fsi", "module A.M\n\nval x: int\n"),
                ("AM.fs", "module A.M\n\nlet x = 1\n"),
                ("Use.fs", "namespace A\n\nmodule Use =\n    let y = M.x\n"),
            ],
            refs: vec![],
            expected_cross_file: 0,
            fcs_must_not_declare: vec![],
        },
        // …and the unsigned control: the relative module commits (1 exact
        // agreement — catches an over-screening regression).
        SigProject {
            label: "sigdiff_relative_unsigned",
            files: vec![
                ("M.fs", "module M\n\nlet x = 0\n"),
                ("AM.fs", "module A.M\n\nlet x = 1\n"),
                ("Use.fs", "namespace A\n\nmodule Use =\n    let y = M.x\n"),
            ],
            refs: vec![],
            expected_cross_file: 1,
            fcs_must_not_declare: vec![],
        },
        // Codex review P2 (probe): an implementation-only `[<AutoOpen>]` is
        // ignored by FCS, so `open N` publishes nothing and the earlier
        // open's `marker` stays bound (1 exact agreement — catches a phantom
        // auto-open staling it into a defer).
        SigProject {
            label: "sigdiff_impl_autoopen",
            files: vec![
                ("Lib.fs", "module Lib\n\nlet marker = 1\n"),
                ("A.fsi", "namespace N\n\nmodule A =\n    val x: int\n"),
                (
                    "A.fs",
                    "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let x = 1\n",
                ),
                ("Use.fs", "module U\n\nopen Lib\nopen N\n\nlet y = marker\n"),
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

/// The exhaustive Stage-1 matrix, per-reference against FCS: every
/// combination of header shape × signature exposure × use style × assembly
/// collision, checked at the two probe references (`shown`, `hidden`) with a
/// **site-keyed** oracle. Unlike the fixture harness above (which iterates
/// the uses *FCS resolved*), this reads FCS's answer at each written
/// reference site, so a wrong commit where FCS is *unbound* (FS0039 — the
/// hidden-member case) is caught too — the straddle gen-diff's oracle
/// discipline. The codex round-1 holes (screen applied at some but not all
/// commit surfaces) are exactly the class this sweep pins mechanically.
///
/// Verdict per reference:
/// - FCS resolved **in-project** → we match the decl exactly, or defer;
/// - FCS resolved **to the assembly** → we commit the assembly or defer —
///   never a project binder;
/// - FCS **unbound** → we defer.
///
/// Tally floors keep the sweep non-vacuous in all three verdict families.
#[test]
fn signature_matrix_agrees_with_fcs_per_reference() {
    #[derive(Clone, Copy, Debug)]
    enum Header {
        Module,
        Namespace,
    }
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Sig {
        None,
        ShownOnly,
        Both,
    }
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum UseStyle {
        Qualified,
        OpenBare,
    }

    let reflib = ensure_reflib_built();
    let reflib_environment = reflib_env();
    let empty_environment = AssemblyEnv::default();

    let mut item_agreements = 0usize;
    let mut assembly_agreements = 0usize;
    let mut deferrals = 0usize;

    for collision in [false, true] {
        for header in [Header::Module, Header::Namespace] {
            for sig in [Sig::None, Sig::ShownOnly, Sig::Both] {
                for style in [UseStyle::Qualified, UseStyle::OpenBare] {
                    // The module's qualified path: the RefLib-colliding
                    // `ProbeNs.Shared` when sweeping the merge, a neutral
                    // `Pn.Md` otherwise.
                    let (ns, md) = if collision {
                        ("ProbeNs", "Shared")
                    } else {
                        ("Pn", "Md")
                    };
                    let dotted = format!("{ns}.{md}");

                    let sig_src = match header {
                        Header::Module => {
                            let mut s = format!("module {dotted}\n\nval shown: int\n");
                            if sig == Sig::Both {
                                s.push_str("val hidden: int\n");
                            }
                            s
                        }
                        Header::Namespace => {
                            let mut s =
                                format!("namespace {ns}\n\nmodule {md} =\n    val shown: int\n");
                            if sig == Sig::Both {
                                s.push_str("    val hidden: int\n");
                            }
                            s
                        }
                    };
                    let impl_src = match header {
                        Header::Module => {
                            format!("module {dotted}\n\nlet shown = 1\nlet hidden = 2\n")
                        }
                        Header::Namespace => format!(
                            "namespace {ns}\n\nmodule {md} =\n    let shown = 1\n    let hidden = 2\n"
                        ),
                    };
                    // Collision cells also probe `asmOnly` — a name only the
                    // assembly provides, so FCS's verdict there is the
                    // merged-assembly member (the fall-through family).
                    let probe_names: &[&str] = if collision {
                        &["shown", "hidden", "asmOnly"]
                    } else {
                        &["shown", "hidden"]
                    };
                    let use_src = match style {
                        UseStyle::Qualified => {
                            let mut s = "module Use\n\n".to_string();
                            for (i, name) in probe_names.iter().enumerate() {
                                s.push_str(&format!("let u{i} = {dotted}.{name}\n"));
                            }
                            s
                        }
                        UseStyle::OpenBare => {
                            let mut s = format!("module Use\n\nopen {dotted}\n\n");
                            for (i, name) in probe_names.iter().enumerate() {
                                s.push_str(&format!("let u{i} = {name}\n"));
                            }
                            s
                        }
                    };

                    let mut rows: Vec<(String, String)> = Vec::new();
                    if sig != Sig::None {
                        rows.push((format!("{md}.fsi"), sig_src));
                    }
                    rows.push((format!("{md}.fs"), impl_src));
                    rows.push(("Use.fs".to_string(), use_src));
                    let row_refs: Vec<(&str, &str)> = rows
                        .iter()
                        .map(|(rel, src)| (rel.as_str(), src.as_str()))
                        .collect();

                    let label = format!(
                        "sigmatrix_{header:?}_{sig:?}_{style:?}_{}",
                        if collision { "reflib" } else { "plain" }
                    )
                    .to_lowercase();
                    let (root, written) = temp_fs_tree(&label, &row_refs);
                    let paths: Vec<&Path> =
                        written.iter().map(|(path, _)| path.as_path()).collect();
                    let refs: Vec<&Path> = if collision { vec![reflib] } else { vec![] };

                    let json = invoke_fcs_dump_project_with_refs(&paths, &refs);
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
                    let env = if collision {
                        &reflib_environment
                    } else {
                        &empty_environment
                    };
                    let proj = resolve_project_files(&input, env);

                    let _ = std::fs::remove_dir_all(&root);

                    // The Use.fs slot is always last.
                    let use_idx = written.len() - 1;
                    let (use_path, use_source) = &written[use_idx];
                    let fcs_use_file = fcs_files
                        .iter()
                        .find(|f| f.path.file_name() == use_path.file_name());

                    for &name in probe_names {
                        // The written reference site: the whole dotted path
                        // (qualified) or the bare name (open) — both are the
                        // span FCS reports and the span we record.
                        let needle = match style {
                            UseStyle::Qualified => format!("{dotted}.{name}"),
                            UseStyle::OpenBare => name.to_string(),
                        };
                        let start = use_source.find(&needle).expect("probe site present");
                        let site = span(start, start + needle.len());

                        let fcs_at_site = fcs_use_file.and_then(|f| {
                            f.uses.iter().find(|u| {
                                u.start == usize::from(site.start())
                                    && u.end == usize::from(site.end())
                            })
                        });
                        let ours = proj.file(use_idx).resolution_at(site);

                        let what = format!("{label}: {needle}");
                        match fcs_at_site {
                            Some(u) if u.decl.is_some() => {
                                let decl = u.decl.as_ref().expect("checked");
                                match ours {
                                    None | Some(Resolution::Deferred(_)) => deferrals += 1,
                                    Some(res @ (Resolution::Item(_) | Resolution::Local(_))) => {
                                        let (def_idx, def_range) = match res {
                                            Resolution::Item(_) => {
                                                let (idx, def) =
                                                    proj.item_def(res).expect("item def");
                                                (idx, def.range)
                                            }
                                            _ => (
                                                use_idx,
                                                proj.file(use_idx)
                                                    .resolved_def(res)
                                                    .expect("local def")
                                                    .range,
                                            ),
                                        };
                                        assert_eq!(
                                            written[def_idx].0.file_name(),
                                            decl.file.file_name(),
                                            "{what}: wrong declaring file"
                                        );
                                        assert_eq!(
                                            def_range,
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
                            Some(u) if u.assembly.is_some() => match ours {
                                None | Some(Resolution::Deferred(_)) => deferrals += 1,
                                Some(Resolution::Member { .. } | Resolution::Entity(_)) => {
                                    assembly_agreements += 1;
                                }
                                other => panic!(
                                    "{what}: FCS resolves to assembly {:?}, we committed a \
                                     project binder {other:?}",
                                    u.assembly
                                ),
                            },
                            // FCS unbound (FS0039) or an unclassifiable use:
                            // we must say nothing.
                            _ => match ours {
                                None | Some(Resolution::Deferred(_)) => deferrals += 1,
                                other => panic!(
                                    "{what}: FCS leaves the reference unbound, we committed \
                                     {other:?}"
                                ),
                            },
                        }
                    }
                }
            }
        }
    }

    // Non-vacuity floors: the sweep must exercise all three verdict families.
    // Unsigned cells commit project Items; SigShownOnly × qualified ×
    // collision falls through to the assembly for `hidden`; every screened
    // cell defers.
    assert!(item_agreements >= 12, "item agreements: {item_agreements}");
    assert!(
        assembly_agreements >= 2,
        "assembly agreements: {assembly_agreements}"
    );
    assert!(deferrals >= 12, "deferrals: {deferrals}");
}

/// Codex review P1, the namespace-direct-case × assembly-collision cell as a
/// live differential: FCS binds `Shared` (of `ProbeNs.Shared.shown`) to the
/// `.fsi`'s case and FS0039s the member; any assembly commit on our side
/// trips the exactness loop's assembly arm.
#[test]
fn namespace_direct_case_collision_agrees_with_fcs() {
    let reflib = ensure_reflib_built();
    assert_sig_matches_fcs(&SigProject {
        label: "sigdiff_ns_case",
        files: vec![
            ("A.fsi", "namespace ProbeNs\n\ntype Color = Shared\n"),
            ("A.fs", "namespace ProbeNs\n\ntype Color = Shared\n"),
            ("Use.fs", "module Use\n\nlet a = ProbeNs.Shared.shown\n"),
        ],
        refs: vec![reflib],
        expected_cross_file: 0,
        fcs_must_not_declare: vec![],
    });
}

/// The assembly-collision matrix as a live differential (the probe that
/// grounded the Stage-1 screen): FCS binds sig-exposed `shown` to the `.fsi`
/// and hidden `bar` / `asmOnly` to RefLib; our side defers `shown` (never an
/// assembly commit — the screen) and commits `bar`/`asmOnly` to the assembly.
#[test]
fn assembly_fall_through_agrees_with_fcs() {
    let reflib = ensure_reflib_built();
    let files = vec![
        ("A.fsi", "module ProbeNs.Shared\n\nval shown: int\n"),
        (
            "A.fs",
            "module ProbeNs.Shared\n\nlet shown = 1\nlet bar = 2\n",
        ),
        (
            "Use.fs",
            "module Use\n\nlet a = ProbeNs.Shared.shown\nlet b = ProbeNs.Shared.bar\nlet c = ProbeNs.Shared.asmOnly\n",
        ),
    ];
    let p = SigProject {
        label: "sigdiff_reflib",
        files,
        refs: vec![reflib],
        expected_cross_file: 0,
        fcs_must_not_declare: vec!["ProbeNs.Shared.bar", "ProbeNs.Shared.asmOnly"],
    };
    assert_sig_matches_fcs(&p);

    // And the FCS side of the screen premise: `shown` declares in the .fsi
    // (in-project), `bar` in RefLib — re-assert directly for loudness.
    let (root, written) = temp_fs_tree("sigdiff_reflib_premise", &p.files);
    let paths: Vec<&Path> = written.iter().map(|(path, _)| path.as_path()).collect();
    let json = invoke_fcs_dump_project_with_refs(&paths, &p.refs);
    let fcs_files = parse_fcs_uses_project(&json, &written);
    let _ = std::fs::remove_dir_all(&root);
    let use_file = fcs_files
        .iter()
        .find(|f| f.path.file_name() == written[2].0.file_name())
        .expect("FCS uses for Use.fs");
    let shown = use_file
        .uses
        .iter()
        .find(|u| u.name == "shown" && u.decl.is_some())
        .expect("FCS resolves shown in-project");
    assert_eq!(
        shown.decl.as_ref().unwrap().file.file_name(),
        written[0].0.file_name(),
        "FCS binds sig-exposed shown to the .fsi even with the RefLib collision"
    );
    let bar = use_file
        .uses
        .iter()
        .find(|u| u.name == "bar")
        .expect("FCS reports the bar use");
    assert_eq!(
        bar.assembly.as_deref(),
        Some("SemaSignatureRefLib"),
        "FCS resolves hidden bar to the merged assembly"
    );
}
