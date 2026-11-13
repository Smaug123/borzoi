//! Stage 2 of incremental resolution: the differential that guards
//! [`resolve_project_incremental`] against the cold [`resolve_project`].
//!
//! The incremental fold reuses per-file [`ResolvedFile`]s from a previous fold
//! wherever an edit cannot have changed them. Its correctness contract is a
//! single equation:
//!
//! ```text
//! resolve_project_incremental(prev_files, resolve_project(prev_files), new_files)
//!   ==  resolve_project(new_files)
//! ```
//!
//! i.e. *whatever* it reuses, it must return exactly what a cold fold of the new
//! files would. This is the machine check on the reuse logic — in particular on
//! the lockstep between [`ProjectItems::extend_with`] and
//! `ResolvedFile::same_export_contribution` (mutate either out of step and a
//! generated edit that changes a file's exports will make the incremental fold
//! reuse a stale suffix, which this differential catches).
//!
//! A generator (`common::edit_script`, shared with the inference differential)
//! produces a fixed set of cross-referencing files and a sequence of edits
//! (body tweaks, added/removed declarations, attribute and `[<AutoOpen>]`
//! toggles, new union types). Each edit yields a new snapshot; the harness folds
//! the sequence incrementally — reusing a file's parse tree verbatim (a syntax
//! handle clone, preserving *identity*, exactly as the LSP's stage-1 parse cache
//! does) whenever its source text is unchanged — and asserts the incremental
//! result equals a cold fold at every step. Feeding each step's incremental
//! result forward as the next `prev` also exercises multi-edit drift.
//!
//! No FCS: the cold fold is the oracle. Structural add/remove-file cases (which
//! the generator holds the file set fixed to avoid) are pinned by the explicit
//! unit tests at the foot of the file.

use std::sync::Arc;

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, Resolution, resolve_project, resolve_project_incremental,
    resolve_project_incremental_with_reuse,
};
use proptest::prelude::*;

use crate::common::edit_script::{Scenario, generate, impl_file, seeded_tape};

// ============================================================================
// The differential
// ============================================================================

/// How much reuse a run of [`check`] observed, accumulated so a batch test can
/// assert the generator actually *exercises* the reuse the optimization exists
/// for — a property that only ever recomputed everything would still satisfy
/// `incremental ≡ batch` vacuously.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    /// Edit steps folded incrementally.
    steps: usize,
    /// Steps at which at least one file was reused verbatim.
    steps_with_reuse: usize,
    /// Steps at which a file was reused *after* an earlier file was recomputed —
    /// i.e. genuine **suffix** reuse (the export-neutral-edit win), not just an
    /// unchanged prefix.
    steps_with_suffix_reuse: usize,
    /// Total files reused across all steps.
    files_reused: usize,
    /// Files reused after a recomputed one that name an earlier file's type
    /// (`Resolution::EarlierFileType`): the identity a stale reuse would
    /// leave pointing at the wrong type.
    suffix_reused_earlier_types: usize,
}

impl Tally {
    fn add(&mut self, other: Tally) {
        self.steps += other.steps;
        self.steps_with_reuse += other.steps_with_reuse;
        self.steps_with_suffix_reuse += other.steps_with_suffix_reuse;
        self.files_reused += other.files_reused;
        self.suffix_reused_earlier_types += other.suffix_reused_earlier_types;
    }
}

/// Fold `scenario`'s snapshots incrementally and assert each step equals a cold
/// fold. Files whose source text is unchanged between snapshots are reused as
/// the *same* parse-tree instance (a clone), so the incremental fold's identity
/// check ([`resolve_project_incremental`]'s `same_tree`) actually fires — the
/// reuse it is built to exploit. Any other construction (re-parsing every file)
/// would make the incremental fold silently degenerate to the cold fold and the
/// test vacuous. Returns a [`Tally`] of the reuse observed so a caller can assert
/// the reuse path is genuinely exercised.
fn check(scenario: &Scenario) -> Result<Tally, TestCaseError> {
    let env = AssemblyEnv::default();
    let snaps = &scenario.snapshots;

    let mut prev_sources = snaps[0].clone();
    let mut prev_files: Vec<ImplFile> = prev_sources.iter().map(|s| impl_file(s)).collect();
    let mut prev = resolve_project(&prev_files, &env);
    let mut tally = Tally::default();

    for (k, sources) in snaps.iter().enumerate().skip(1) {
        // Reuse the previous tree instance where text is unchanged (parse-cache
        // hit), fresh-parse where it changed.
        let new_files: Vec<ImplFile> = sources
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if *s == prev_sources[i] {
                    prev_files[i].clone()
                } else {
                    impl_file(s)
                }
            })
            .collect();

        let (incr, reused) =
            resolve_project_incremental_with_reuse(&prev_files, &prev, &new_files, &env);
        let cold = resolve_project(&new_files, &env);

        if incr != cold {
            let diff = (0..incr.len().max(cold.len()))
                .find(|&i| incr.files().get(i) != cold.files().get(i));
            return Err(TestCaseError::fail(format!(
                "incremental != batch at edit {k}; first differing file index {diff:?}\n\
                 --- previous snapshot ---\n{}\n--- this snapshot ---\n{}",
                prev_sources.join("\n----\n"),
                sources.join("\n----\n"),
            )));
        }

        // Tally the reuse. "Suffix reuse" = a reused file sitting after a
        // recomputed one, which is the export-neutral-edit win (as opposed to an
        // unchanged prefix that trivially reuses).
        tally.steps += 1;
        let any_reuse = reused.iter().any(|&r| r);
        if any_reuse {
            tally.steps_with_reuse += 1;
        }
        tally.files_reused += reused.iter().filter(|&&r| r).count();
        let first_recompute = reused.iter().position(|&r| !r);
        if first_recompute.is_some_and(|fr| reused[fr..].iter().any(|&r| r)) {
            tally.steps_with_suffix_reuse += 1;
        }
        if let Some(fr) = first_recompute {
            tally.suffix_reused_earlier_types += (fr..reused.len())
                .filter(|&i| reused[i])
                .filter(|&i| {
                    incr.file(i)
                        .resolutions()
                        .values()
                        .any(|r| matches!(r, Resolution::EarlierFileType { .. }))
                })
                .count();
        }

        prev_sources = sources.clone();
        prev_files = new_files;
        prev = incr;
    }
    Ok(tally)
}

/// For each file in `base`, prepend a line comment to it and assert the
/// incremental fold recomputes **only** that file and reuses every other verbatim
/// — prefix *and* suffix.
///
/// A leading line comment is trivia: the parse tree, and therefore every export,
/// is byte-for-byte identical — only source *positions* move, and they move for
/// *every* declaration in the file (unlike an edit inside the last body, which
/// shifts nothing after it). That is exactly the spurious-invalidation trap:
/// [`ResolvedFile::same_export_contribution`] must ignore declaration provenance
/// (a decl's `pos`), or this maximal position shift would wrongly report an
/// export change and recompute the whole suffix. This property is the generative
/// guard on that direction — the one [`incremental_equals_batch`] is blind to,
/// since a *missed* reuse still yields the correct result. (A `pos`-comparing
/// regression once passed the batch differential and its unit tests yet failed
/// here on any file with a declaration after the edit point.)
fn check_body_edit_reuses_all_others(base: &[String]) -> Result<(), TestCaseError> {
    let env = AssemblyEnv::default();
    let base_files: Vec<ImplFile> = base.iter().map(|s| impl_file(s)).collect();
    let prev = resolve_project(&base_files, &env);

    for i in 0..base.len() {
        let edited = format!("// edit {i}\n{}", base[i]);
        // Reuse the same tree instance for every unchanged file (parse-cache hit);
        // fresh-parse only the commented one.
        let new_files: Vec<ImplFile> = (0..base.len())
            .map(|j| {
                if j == i {
                    impl_file(&edited)
                } else {
                    base_files[j].clone()
                }
            })
            .collect();

        let (incr, reused) =
            resolve_project_incremental_with_reuse(&base_files, &prev, &new_files, &env);

        // Sanity: whatever it reuses, the result still matches a cold fold.
        prop_assert!(
            incr == resolve_project(&new_files, &env),
            "incremental != batch when a comment is prepended to file {i}"
        );

        // Only file `i` (its tree changed) is recomputed; its export contribution
        // is unchanged, so the threaded state entering every later file is
        // identical and the whole suffix — like the prefix — is reused verbatim.
        let expected: Vec<bool> = (0..base.len()).map(|j| j != i).collect();
        prop_assert_eq!(
            reused,
            expected,
            "a comment prepended to file {} leaves its exports unchanged, so only it \
             should recompute and every other file (prefix and suffix) reuse:\n{}",
            i,
            base[i]
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// `resolve_project_incremental(prev_files, prev, new_files) == resolve_project(new_files)`
    /// across a generated edit sequence, threading each step's incremental result
    /// forward as the next `prev`.
    #[test]
    fn incremental_equals_batch(nums in prop::collection::vec(any::<u32>(), 0..96)) {
        check(&generate(nums))?;
    }

    /// A **prefix fold is a prefix of the full fold**: resolving `files[..m]`
    /// yields exactly the first `m` files of resolving all of `files`. This is
    /// the soundness foundation of the LSP's Compile-index resolution slice — a
    /// single-file request may fold only up to that file's Compile index and get
    /// the identical result, because `resolve_file`'s output depends only on the
    /// file and its (prefix) `preceding` state, never on any later file. F# is
    /// order-sensitive, so this can never regress unless the fold starts peeking
    /// forward.
    #[test]
    fn prefix_fold_is_a_prefix_of_the_full_fold(
        nums in prop::collection::vec(any::<u32>(), 0..64),
    ) {
        let env = AssemblyEnv::default();
        let sources = &generate(nums).snapshots[0];
        let files: Vec<ImplFile> = sources.iter().map(|s| impl_file(s)).collect();
        let full = resolve_project(&files, &env);
        for m in 0..=files.len() {
            let prefix = resolve_project(&files[..m], &env);
            prop_assert_eq!(prefix.len(), m);
            for k in 0..m {
                prop_assert_eq!(
                    prefix.file(k),
                    full.file(k),
                    "resolve_project(&files[..{}]).file({}) != resolve_project(&files).file({})",
                    m,
                    k,
                    k,
                );
            }
        }
    }

    /// A body-neutral edit (a prepended comment, shifting every declaration's
    /// position while leaving the parse tree — hence every export — identical) to
    /// any one file must recompute only that file and reuse every other verbatim.
    /// Guards the spurious-invalidation direction `incremental_equals_batch` is
    /// blind to (see [`check_body_edit_reuses_all_others`]). The initial project of
    /// a generated scenario supplies the cross-referencing multi-file shapes.
    #[test]
    fn body_edit_preserves_full_suffix_reuse(nums in prop::collection::vec(any::<u32>(), 0..96)) {
        check_body_edit_reuses_all_others(&generate(nums).snapshots[0])?;
    }
}

/// The generator genuinely exercises reuse — including *suffix* reuse (a file
/// kept across an earlier file's recompute, the export-neutral-edit win). If
/// this ever fails, `incremental_equals_batch` has quietly become vacuous (it
/// would still pass while proving nothing about the reuse path). Asserts the
/// observed distribution rather than merely printing it (property-based-testing
/// discipline: a test that doesn't explore the intended space is itself a bug).
#[test]
fn generator_exercises_suffix_reuse() {
    let mut total = Tally::default();
    for seed in 0..3000u64 {
        let scenario = generate(seeded_tape(seed));
        let t = check(&scenario).expect("incremental == batch over the seeded scenarios");
        total.add(t);
    }
    assert!(total.steps > 0, "no edit steps were folded");
    assert!(
        total.files_reused > 0,
        "no file was ever reused across {} steps — the differential is vacuous",
        total.steps
    );
    assert!(
        total.steps_with_suffix_reuse > 0,
        "suffix reuse (a reused file after a recomputed one) never occurred across {} \
         steps — the export-neutral-edit optimization is untested by the generator: {total:?}",
        total.steps
    );
    assert!(
        total.suffix_reused_earlier_types > 0,
        "no file naming an earlier file's type was reused after a recomputed one: {total:?}"
    );
}

// ============================================================================
// Explicit structural cases (file set changes — the generator holds it fixed)
// ============================================================================

/// A trailing file appended to the Compile order (more `new_files` than `prev`):
/// the extra file has no `prev` counterpart and must be resolved fresh, the rest
/// reused — result equal to a cold fold.
#[test]
fn appended_trailing_file_matches_cold() {
    let env = AssemblyEnv::default();
    let a = impl_file("module A\nlet x = 1\n");
    let b = impl_file("module B\nlet y = A.x\n");
    let prev_files = vec![a.clone(), b.clone()];
    let prev = resolve_project(&prev_files, &env);

    let c = impl_file("module C\nlet z = A.x\n");
    let new_files = vec![a, b, c];
    let incr = resolve_project_incremental(&prev_files, &prev, &new_files, &env);
    let cold = resolve_project(&new_files, &env);
    assert_eq!(incr, cold);
    assert_eq!(incr.len(), 3);
}

/// A file removed from the tail (fewer `new_files` than `prev`): the surviving
/// prefix is reused, and the result equals a cold fold of the shorter list.
#[test]
fn removed_trailing_file_matches_cold() {
    let env = AssemblyEnv::default();
    let a = impl_file("module A\nlet x = 1\n");
    let b = impl_file("module B\nlet y = A.x\n");
    let c = impl_file("module C\nlet z = 2\n");
    let prev_files = vec![a.clone(), b.clone(), c];
    let prev = resolve_project(&prev_files, &env);

    let new_files = vec![a, b];
    let incr = resolve_project_incremental(&prev_files, &prev, &new_files, &env);
    let cold = resolve_project(&new_files, &env);
    assert_eq!(incr, cold);
    assert_eq!(incr.len(), 2);
}

/// Editing an early file's *exports* (adding a binding shifts every later file's
/// item-id base) then editing it back must both track the cold fold — the
/// suffix cannot be reused across an export change, and reverting restores it.
#[test]
fn export_change_then_revert_matches_cold() {
    let env = AssemblyEnv::default();
    let a0 = impl_file("module A\nlet x = 1\n");
    let b = impl_file("module B\nlet y = A.x\n");
    let prev_files = vec![a0.clone(), b.clone()];
    let prev = resolve_project(&prev_files, &env);

    // Add an export to A: B's item-id base shifts, so B must be recomputed.
    let a1 = impl_file("module A\nlet w = 0\nlet x = 1\n");
    let step1_files = vec![a1, b.clone()];
    let incr1 = resolve_project_incremental(&prev_files, &prev, &step1_files, &env);
    assert_eq!(incr1, resolve_project(&step1_files, &env));

    // Revert A to its original text: reuse resumes, still equal to a cold fold.
    let new_files = vec![a0, b];
    let incr2 = resolve_project_incremental(&step1_files, &incr1, &new_files, &env);
    assert_eq!(incr2, resolve_project(&new_files, &env));
}

/// The core suffix-reuse win, pinned deterministically: a *body-only* edit to an
/// early file (its exports byte-identical) recomputes only that file and reuses
/// the entire tail verbatim. Reads the per-file reuse vector so it asserts the
/// reuse itself, not just that the result matches a cold fold (which holds even
/// with no reuse at all).
#[test]
fn body_only_edit_reuses_the_suffix() {
    let env = AssemblyEnv::default();
    let a0 = impl_file("module A\nlet a = 1\n");
    let b = impl_file("module B\nlet b = A.a\n");
    let c = impl_file("module C\nlet c = B.b\n");
    let prev_files = vec![a0, b.clone(), c.clone()];
    let prev = resolve_project(&prev_files, &env);

    // Body-only edit to A: `1` -> `(1)`. The export `A.a` is unchanged (same
    // path, ItemId, accessibility), so B and C must be reused verbatim.
    let a1 = impl_file("module A\nlet a = (1)\n");
    let new_files = vec![a1, b, c];
    let (incr, reused) =
        resolve_project_incremental_with_reuse(&prev_files, &prev, &new_files, &env);

    assert_eq!(
        reused,
        vec![false, true, true],
        "A recomputed (its tree changed); B and C reused across the export-neutral edit"
    );
    // Reuse must be an `Arc` share, not a deep clone — otherwise a keystroke still
    // pays O(occurrences) to *copy* every reused file's resolution map. A reused
    // file is the same allocation as in `prev`; the recomputed one is fresh.
    assert!(
        !Arc::ptr_eq(&prev.files()[0], &incr.files()[0]),
        "the edited file A is a fresh allocation"
    );
    assert!(
        Arc::ptr_eq(&prev.files()[1], &incr.files()[1]),
        "reused B must share prev's allocation, not be deep-cloned"
    );
    assert!(
        Arc::ptr_eq(&prev.files()[2], &incr.files()[2]),
        "reused C must share prev's allocation, not be deep-cloned"
    );
    assert_eq!(
        incr,
        resolve_project(&new_files, &env),
        "and the reused result still equals a cold fold"
    );
}

/// The contrast: an edit that changes an early file's *exports* (adding a
/// binding shifts every later file's item-id base) forces the whole suffix to be
/// recomputed — no file after the edit can be reused. Pins that reuse is
/// correctly *withheld* when it would be unsound, the flip side of
/// [`body_only_edit_reuses_the_suffix`].
#[test]
fn export_edit_recomputes_the_suffix() {
    let env = AssemblyEnv::default();
    let a0 = impl_file("module A\nlet a = 1\n");
    let b = impl_file("module B\nlet b = A.a\n");
    let c = impl_file("module C\nlet c = B.b\n");
    let prev_files = vec![a0, b.clone(), c.clone()];
    let prev = resolve_project(&prev_files, &env);

    // Add an export to A: item bases shift, so B and C cannot be reused.
    let a1 = impl_file("module A\nlet a = 1\nlet a2 = 2\n");
    let new_files = vec![a1, b, c];
    let (incr, reused) =
        resolve_project_incremental_with_reuse(&prev_files, &prev, &new_files, &env);

    assert_eq!(
        reused,
        vec![false, false, false],
        "A's export set changed, so the whole suffix is recomputed"
    );
    assert_eq!(incr, resolve_project(&new_files, &env));
}

/// The same tree, read with a *different* [`SyntaxRecovery`], is not the same
/// input — so it must not be reused.
///
/// `resolve_file`'s output is a pure function of `(file, preceding, assemblies,
/// recovery)`, and recovery is load-bearing: `Unretained` makes every
/// annotation in the file decline. Reuse keys on tree *identity*, and a tree is
/// a syntax handle that can be cloned into two `ProjectFile`s carrying different
/// readings — the bare-`ImplFile` fold entry points supply `Unretained` for the
/// very trees `resolve_project_files` gives a real reading. Without recovery in
/// the key the incremental fold serves the stale resolution and silently
/// disagrees with a cold fold, which is the one equation this file exists to
/// hold.
#[test]
fn a_different_recovery_reading_of_one_tree_is_not_reused() {
    use borzoi_sema::{ProjectFile, SourceFile, SyntaxRecovery, resolve_project_files};

    let env = AssemblyEnv::default();
    let src = "module A\nlet v : System.String = failwith \"\"\n";
    let parsed = parse(src);
    let reported = SyntaxRecovery::of(&parsed);
    // One tree, cloned — handle equality is identity, so both files are the
    // *same* tree as far as the reuse key is concerned.
    let tree = ImplFile::cast(parsed.root).expect("impl file");
    let qnof = borzoi_sema::qualified_names(
        &[SourceFile::Impl(tree.clone())],
        &[std::path::PathBuf::from("/p/A.fs")],
    );

    let prev_files = vec![ProjectFile::new(
        SourceFile::Impl(tree.clone()),
        qnof[0].clone(),
        SyntaxRecovery::Unretained,
    )];
    let new_files = vec![ProjectFile::new(
        SourceFile::Impl(tree),
        qnof[0].clone(),
        reported,
    )];

    let prev = resolve_project_files(&prev_files, &env);
    let (incr, reused) =
        borzoi_sema::resolve_project_files_incremental(&prev_files, &prev, &new_files, &env);

    assert_eq!(
        reused,
        vec![false],
        "the recovery reading changed, so the file must be recomputed"
    );
    assert_eq!(
        incr,
        resolve_project_files(&new_files, &env),
        "incremental ≡ batch"
    );
}

/// A signature slot contributes its type names to later files, so the
/// incremental fold must build it exactly as the cold fold does: extending a
/// cached prefix across a `.fsi` that declares another `T` must decline the
/// later file's bare `T`, as the cold fold does, rather than bind the
/// earlier `Lib.T`.
#[test]
fn a_signature_slot_contributes_its_types_in_the_incremental_fold() {
    use borzoi_cst::parser::parse_sig;
    use borzoi_cst::syntax::SigFile;
    use borzoi_sema::{
        ProjectFile, SourceFile, SyntaxRecovery, qualified_names, resolve_project_files,
        resolve_project_files_prefix, resolve_project_files_prefix_incremental,
    };
    let sources = [
        ("P.fs", "namespace Lib\ntype T = { X : int }\n"),
        ("A.fsi", "namespace Other\ntype T = { Y : int }\n"),
        ("B.fs", "namespace Lib\nmodule B =\n    let f (x : T) = x\n"),
        ("A.fs", "namespace Other\ntype T = { Y : int }\n"),
    ];
    let parsed: Vec<(SourceFile, SyntaxRecovery)> = sources
        .iter()
        .map(|(name, src)| {
            if name.ends_with(".fsi") {
                let p = parse_sig(src);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Sig(SigFile::cast(p.root).expect("sig")),
                    recovery,
                )
            } else {
                let p = parse(src);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Impl(ImplFile::cast(p.root).expect("impl")),
                    recovery,
                )
            }
        })
        .collect();
    let paths: Vec<std::path::PathBuf> = sources.iter().map(|(n, _)| n.into()).collect();
    let srcs: Vec<SourceFile> = parsed.iter().map(|(f, _)| f.clone()).collect();
    let files: Vec<ProjectFile> = parsed
        .into_iter()
        .zip(qualified_names(&srcs, &paths))
        .map(|((file, recovery), qnof)| ProjectFile::new(file, qnof, recovery))
        .collect();
    let env = AssemblyEnv::default();
    let cold = resolve_project_files(&files, &env);
    let prefix = resolve_project_files_prefix(&files, 1, &env);
    let (extended, _) =
        resolve_project_files_prefix_incremental(&files, &prefix, &files, files.len(), &env);
    assert!(
        extended == cold,
        "extending a cached prefix across the signature differs from a cold fold"
    );
    let use_at = sources[2].1.find("T)").expect("the use");
    let at = borzoi_cst::syntax::TextRange::new(
        u32::try_from(use_at).unwrap().into(),
        u32::try_from(use_at + 1).unwrap().into(),
    );
    assert!(
        !matches!(
            cold.file(2).resolution_at(at),
            Some(Resolution::EarlierFileType { .. })
        ),
        "a signature declaring `T` contests the earlier file's `Lib.T`"
    );
}
