//! Inference's incremental fold, held to the cold one.
//!
//! [`infer_project_reusing`](borzoi_sema::infer_project_reusing) reuses a
//! file's previous inference when nothing it reads can have changed: its
//! resolution is the same allocation, its pairing is the same, and every
//! earlier file contributed to the published table exactly what it
//! contributed before. Its contract is one equation, checked after every step
//! of generated edit scripts over generated multi-file projects (the
//! `common::edit_script` scenarios, which `resolve_incremental_diff.rs` folds
//! too), and over a sample of the corpus's real Compile lists:
//!
//! ```text
//! infer_project_reusing(files, resolve_incremental(files, ..), env, prev)
//!   == infer_project(files, resolve_project_files(files), env)
//! ```
//!
//! Each step's incremental result is the next step's `prev`, so reuse drifts
//! over several edits, and a step may fold only a prefix of the Compile list,
//! as an LSP request for an early file does, so `prev` is sometimes shallower
//! and sometimes deeper than the fold reusing it.
//!
//! The equation cannot see a missed reuse: inferring every file afresh
//! satisfies it. [`body_edit_preserves_full_inference_reuse`] guards that
//! direction, and [`generator_exercises_the_cutoff`] checks that the scripts
//! reach both sides of the cutoff: a file reused after an earlier one was
//! inferred afresh, and a file whose resolution was reused but whose inference
//! had to be redone, and came out different, because an earlier file's exports
//! kept their names and changed their types.
//!
//! No FCS: the cold fold is the oracle.

use std::path::{Path, PathBuf};

use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::{AstNode, ImplFile, LetDecl, SigFile, SyntaxKind};
use borzoi_sema::{
    AssemblyEnv, InferredProject, ProjectFile, ResolvedProject, SourceFile, SyntaxRecovery,
    infer_project, infer_project_reusing_with_reuse, qualified_names, resolve_project_files,
    resolve_project_files_prefix, resolve_project_files_prefix_incremental,
};
use proptest::prelude::*;

use crate::common::edit_script::{Body, Decl, File, generate, render_all, seeded_tape};
use crate::common::{env_usize_or, full_bcl_env};

// ============================================================================
// Folding a Compile list, cold and incrementally
// ============================================================================

/// Parse one source, as a signature when its path says so.
fn parse_source(path: &Path, src: &str) -> (SourceFile, SyntaxRecovery) {
    if path.extension().is_some_and(|e| e == "fsi") {
        let p = parse_sig(src);
        let recovery = SyntaxRecovery::of(&p);
        (
            SourceFile::Sig(SigFile::cast(p.root).expect("sig file")),
            recovery,
        )
    } else {
        let p = parse(src);
        let recovery = SyntaxRecovery::of(&p);
        (
            SourceFile::Impl(ImplFile::cast(p.root).expect("impl file")),
            recovery,
        )
    }
}

/// One fold of a Compile list: its sources, the parsed files, and the
/// resolution and inference over its first `resolved.len()` files.
struct Fold {
    paths: Vec<PathBuf>,
    sources: Vec<String>,
    files: Vec<ProjectFile>,
    resolved: ResolvedProject,
    inferred: InferredProject,
}

/// The parsed Compile list for `sources`. A source whose text equals the one
/// `prev` held at the same index keeps `prev`'s tree, which is how the LSP's
/// parse cache shares trees and how the resolver knows a file unchanged.
fn compile_list(paths: &[PathBuf], sources: &[String], prev: Option<&Fold>) -> Vec<ProjectFile> {
    let parsed: Vec<(SourceFile, SyntaxRecovery)> = sources
        .iter()
        .enumerate()
        .map(|(i, src)| match prev {
            Some(prev) if prev.sources.get(i) == Some(src) && prev.paths.get(i) == paths.get(i) => {
                (prev.files[i].file.clone(), prev.files[i].recovery.clone())
            }
            _ => parse_source(&paths[i], src),
        })
        .collect();
    let trees: Vec<SourceFile> = parsed.iter().map(|(f, _)| f.clone()).collect();
    parsed
        .into_iter()
        .zip(qualified_names(&trees, paths))
        .map(|((file, recovery), qnof)| ProjectFile::new(file, qnof, recovery))
        .collect()
}

fn cold_fold(paths: &[PathBuf], sources: Vec<String>, env: &AssemblyEnv) -> Fold {
    let files = compile_list(paths, &sources, None);
    let resolved = resolve_project_files(&files, env);
    let inferred = infer_project(&files, &resolved, env);
    Fold {
        paths: paths.to_vec(),
        sources,
        files,
        resolved,
        inferred,
    }
}

/// What one incremental step reused, per file of its horizon.
struct StepReuse {
    resolve: Vec<bool>,
    infer: Vec<bool>,
    /// Files whose resolution was reused but whose inference was redone and
    /// came out different from `prev`'s: the files a cutoff that took unequal
    /// contributions for equal would have served stale.
    withheld_and_changed: usize,
}

/// Fold `sources` (the first `horizon` files) incrementally from `prev`, and
/// check the inference equals a cold fold's.
fn fold_step(
    prev: &Fold,
    sources: Vec<String>,
    horizon: usize,
    env: &AssemblyEnv,
) -> Result<(Fold, StepReuse), String> {
    let files = compile_list(&prev.paths, &sources, Some(prev));
    let (resolved, resolve) =
        resolve_project_files_prefix_incremental(&prev.files, &prev.resolved, &files, horizon, env);
    let (inferred, infer) =
        infer_project_reusing_with_reuse(&files, &resolved, env, &prev.inferred);
    let cold = infer_project(
        &files,
        &resolve_project_files_prefix(&files, horizon, env),
        env,
    );
    if inferred != cold {
        let first = (0..horizon).find(|&i| inferred.file(i) != cold.file(i));
        return Err(format!(
            "incremental != batch inference over {horizon} files; first differing file \
             {first:?} (None: only the published tables differ); resolver reuse {resolve:?}, \
             inference reuse {infer:?}\n--- previous ---\n{}\n--- this ---\n{}",
            prev.sources.join("\n----\n"),
            sources.join("\n----\n"),
        ));
    }
    let withheld_and_changed = (0..horizon)
        .filter(|&i| resolve[i] && !infer[i] && inferred.file(i) != prev.inferred.file(i))
        .count();
    let reuse = StepReuse {
        resolve,
        infer,
        withheld_and_changed,
    };
    let fold = Fold {
        paths: prev.paths.clone(),
        sources,
        files,
        resolved,
        inferred,
    };
    Ok((fold, reuse))
}

fn generated_paths(n: usize) -> Vec<PathBuf> {
    (0..n)
        .map(|i| PathBuf::from(format!("/p/F{i}.fs")))
        .collect()
}

// ============================================================================
// The differential over generated edit scripts
// ============================================================================

/// What the generated scripts made the incremental fold do, so a batch run can
/// assert they reach both sides of the cutoff.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    steps: usize,
    /// Steps where a file was reused after an earlier one was inferred
    /// afresh: the cutoff kept the suffix.
    steps_with_suffix_reuse: usize,
    /// Files whose resolution was reused and whose inference the cutoff
    /// redid, and which came out different from before.
    withheld_and_changed: usize,
}

impl Tally {
    fn record(&mut self, reuse: &StepReuse) {
        self.steps += 1;
        if let Some(first) = reuse.infer.iter().position(|r| !r)
            && reuse.infer[first..].iter().any(|&r| r)
        {
            self.steps_with_suffix_reuse += 1;
        }
        self.withheld_and_changed += reuse.withheld_and_changed;
    }
}

/// The horizon of step `k` over `n` files: usually the whole list, sometimes
/// a prefix, as the LSP folds for a request on an early file.
fn horizon(cuts: &[u8], k: usize, n: usize) -> usize {
    match cuts.get(k) {
        Some(&c) if c % 3 == 0 => 1 + usize::from(c) % n,
        _ => n,
    }
}

fn check(nums: Vec<u32>, cuts: &[u8]) -> Result<Tally, TestCaseError> {
    let env = full_bcl_env();
    let scenario = generate(nums);
    let paths = generated_paths(scenario.snapshots[0].len());
    let mut fold = cold_fold(&paths, scenario.snapshots[0].clone(), env);
    let mut tally = Tally::default();
    for (k, sources) in scenario.snapshots.iter().enumerate().skip(1) {
        let h = horizon(cuts, k, sources.len());
        let (next, reuse) =
            fold_step(&fold, sources.clone(), h, env).map_err(TestCaseError::fail)?;
        tally.record(&reuse);
        fold = next;
    }
    Ok(tally)
}

// ============================================================================
// Edits that leave every contribution as it was
// ============================================================================

/// Edits to `file` that change its text and leave what it contributes to the
/// published table as it was, by label: a leading comment (every position in
/// the file moves), every literal bumped (the tree changes, no type does), and
/// every literal body swapped with a `let` body of the same type (every
/// definition after it is renumbered).
fn contribution_preserving_edits(file: &File) -> Vec<(&'static str, String)> {
    let rewrite = |f: &dyn Fn(&Body) -> Body| {
        let mut edited = file.clone();
        for decl in &mut edited.decls {
            if let Decl::Val { body, .. } | Decl::Func { body, .. } = decl {
                *body = f(body);
            }
        }
        render_all(&[edited]).remove(0)
    };
    let original = render_all(std::slice::from_ref(file)).remove(0);
    let edits = vec![
        ("comment", format!("// edit\n{original}")),
        (
            "bump",
            rewrite(&|b| match b {
                Body::Lit(n) => Body::Lit(n + 1),
                Body::LetIn(n) => Body::LetIn(n + 1),
                other => other.clone(),
            }),
        ),
        (
            "swap",
            rewrite(&|b| match b {
                Body::Lit(n) => Body::LetIn(*n),
                Body::LetIn(n) => Body::Lit(*n),
                other => other.clone(),
            }),
        ),
    ];
    edits
        .into_iter()
        .filter(|(_, edited)| *edited != original)
        .collect()
}

/// For each file of `base`, and each contribution-preserving edit to it, the
/// incremental fold infers only that file afresh and reuses every other.
fn check_body_edits_reinfer_one_file(base: &[File]) -> Result<(), TestCaseError> {
    let env = full_bcl_env();
    let paths = generated_paths(base.len());
    let sources = render_all(base);
    let prev = cold_fold(&paths, sources.clone(), env);
    for (i, file) in base.iter().enumerate() {
        for (label, edited) in contribution_preserving_edits(file) {
            let mut after = sources.clone();
            after[i] = edited;
            let (_, reuse) =
                fold_step(&prev, after, base.len(), env).map_err(TestCaseError::fail)?;
            let expected: Vec<bool> = (0..base.len()).map(|j| j != i).collect();
            prop_assert_eq!(
                &reuse.resolve,
                &expected,
                "the resolver must reuse every file but {} after a `{}` edit to it",
                i,
                label
            );
            prop_assert_eq!(
                &reuse.infer,
                &expected,
                "a `{}` edit to file {} leaves its contribution as it was, so only it is \
                 inferred afresh:\n{}",
                label,
                i,
                sources[i]
            );
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    /// Incremental inference equals cold inference after every step of a
    /// generated edit script, each step's result the next one's `prev`.
    #[test]
    fn incremental_equals_batch(
        nums in prop::collection::vec(any::<u32>(), 0..96),
        cuts in prop::collection::vec(any::<u8>(), 0..8),
    ) {
        check(nums, &cuts)?;
    }

    /// An edit that leaves a file's contribution as it was infers that file
    /// afresh and reuses every other: the missed-reuse direction, which
    /// `incremental_equals_batch` cannot see. A cutoff whose equality saw
    /// something no later file reads (a position, a renumbered definition)
    /// would infer the whole suffix afresh here.
    #[test]
    fn body_edit_preserves_full_inference_reuse(
        nums in prop::collection::vec(any::<u32>(), 0..96),
    ) {
        check_body_edits_reinfer_one_file(&generate(nums).initial)?;
    }
}

/// The generated scripts reach both sides of the cutoff, so
/// `incremental_equals_batch` is not passing vacuously: files are reused after
/// an earlier file was inferred afresh, and files whose resolution the
/// resolver reused have their inference redone, with a different result.
#[test]
fn generator_exercises_the_cutoff() {
    let mut total = Tally::default();
    for seed in 0..600u64 {
        let t = check(seeded_tape(seed), &[]).expect("incremental == batch over seeded scripts");
        total.steps += t.steps;
        total.steps_with_suffix_reuse += t.steps_with_suffix_reuse;
        total.withheld_and_changed += t.withheld_and_changed;
    }
    assert!(total.steps > 0, "no edit steps were folded");
    assert!(
        total.steps_with_suffix_reuse > 0,
        "no file was reused after an earlier one was inferred afresh: {total:?}"
    );
    assert!(
        total.withheld_and_changed > 0,
        "no file the resolver reused had its inference redone with a different result, so \
         nothing tests that the cutoff withholds reuse when an export's type changes: {total:?}"
    );
}

// ============================================================================
// Pinned cases
// ============================================================================

fn fold_of(sources: &[&str]) -> Fold {
    let sources: Vec<String> = sources.iter().map(|s| (*s).to_string()).collect();
    cold_fold(&generated_paths(sources.len()), sources, full_bcl_env())
}

fn edit(prev: &Fold, i: usize, src: &str) -> (Fold, StepReuse) {
    let mut sources = prev.sources.clone();
    sources[i] = src.to_string();
    let n = sources.len();
    fold_step(prev, sources, n, full_bcl_env()).unwrap_or_else(|e| panic!("{e}"))
}

/// A body edit that keeps every export's type reuses the whole suffix.
#[test]
fn a_type_preserving_body_edit_reuses_the_suffix() {
    let prev = fold_of(&[
        "module A\nlet a = 1\n",
        "module B\nlet b = A.a\n",
        "module C\nlet c = B.b\n",
    ]);
    let (_, reuse) = edit(&prev, 0, "module A\nlet a = (2)\n");
    assert_eq!(reuse.resolve, vec![false, true, true]);
    assert_eq!(reuse.infer, vec![false, true, true]);
}

/// A body edit that changes an export's type, and no export's name, keeps
/// every later file's resolution and redoes every later file's inference:
/// `B` reads `A.a`'s new type, and `C` reads `B.b`'s.
#[test]
fn a_type_changing_body_edit_reinfers_the_suffix() {
    let env = full_bcl_env();
    let prev = fold_of(&[
        "module A\nlet a = 1\n",
        "module B\nlet b = A.a\n",
        "module C\nlet c = B.b\n",
    ]);
    let (after, reuse) = edit(&prev, 0, "module A\nlet a = \"s\"\n");
    assert_eq!(reuse.resolve, vec![false, true, true]);
    assert_eq!(reuse.infer, vec![false, false, false]);
    assert_eq!(reuse.withheld_and_changed, 2);
    let c = after.inferred.file(2).expect("C is inferred");
    let rendered: Vec<String> = c.def_types().values().map(|t| t.render(&env)).collect();
    assert_eq!(
        rendered,
        vec!["System.String".to_string()],
        "C's `c` is a string"
    );
}

/// A union case gaining a payload keeps the file's exports and changes the
/// type of a later use of the case.
#[test]
fn a_case_payload_reinfers_a_reader_of_the_case() {
    let prev = fold_of(&[
        "module A\ntype U = X | Y\n",
        "module B\nlet b = A.X\n",
        "module C\nlet c = 0\n",
    ]);
    let (_, reuse) = edit(&prev, 0, "module A\ntype U = X of int | Y\n");
    assert_eq!(reuse.resolve, vec![false, true, true]);
    assert_eq!(&reuse.infer[..2], &[false, false]);
}

/// A `prev` that covers only a prefix of the Compile list, extended to the
/// whole list: its files are reused, the rest inferred afresh.
#[test]
fn a_shallower_prev_is_extended() {
    let sources = [
        "module A\nlet a = 1\n",
        "module B\nlet b = A.a\n",
        "module C\nlet c = B.b\n",
    ];
    let full = fold_of(&sources);
    let shallow_resolved = resolve_project_files_prefix(&full.files, 1, full_bcl_env());
    let shallow = Fold {
        inferred: infer_project(&full.files, &shallow_resolved, full_bcl_env()),
        resolved: shallow_resolved,
        paths: full.paths.clone(),
        sources: full.sources.clone(),
        files: full.files.clone(),
    };
    let (_, reuse) = fold_step(&shallow, full.sources.clone(), 3, full_bcl_env())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(reuse.infer, vec![true, false, false]);
}

/// A signature pairs with its implementation, which is then screened: the
/// implementation's body edit changes no type a later file sees, so the later
/// file is reused, whatever the implementation's types became.
#[test]
fn a_screened_implementation_edit_reuses_the_suffix() {
    let paths: Vec<PathBuf> = ["/p/A.fsi", "/p/A.fs", "/p/B.fs"]
        .iter()
        .map(PathBuf::from)
        .collect();
    let sources: Vec<String> = [
        "module A\nval a : int\n",
        "module A\nlet a = 1\n",
        "module B\nlet b = A.a\n",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let prev = cold_fold(&paths, sources.clone(), full_bcl_env());
    let mut after = sources;
    after[1] = "module A\nlet a = 2\n".to_string();
    let (_, reuse) = fold_step(&prev, after, 3, full_bcl_env()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(reuse.infer, vec![true, false, true]);
}

// ============================================================================
// A corpus sample
// ============================================================================

/// The corpus's Compile lists, read off each `.fsproj`'s `<Compile Include>`
/// items in order, as windows of consecutive items: real files, in an order
/// in which a later file reads an earlier one's exports, signatures included.
fn corpus_windows(root: &Path, window: usize, max_windows: usize) -> Vec<Vec<PathBuf>> {
    let mut projects = Vec::new();
    collect_fsproj(root, &mut projects);
    projects.sort();
    let mut windows = Vec::new();
    for project in projects {
        let Ok(text) = std::fs::read_to_string(&project) else {
            continue;
        };
        let dir = project.parent().expect("a project's directory");
        let items: Vec<PathBuf> = text
            .split("<Compile Include=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .filter(|item| !item.contains('$') && !item.contains('*'))
            .map(|item| dir.join(item.replace('\\', "/")))
            .filter(|path| {
                path.extension().is_some_and(|e| e == "fs" || e == "fsi")
                    && std::fs::metadata(path).is_ok_and(|m| m.len() <= 24 * 1024)
            })
            .collect();
        for chunk in items.chunks(window).filter(|c| c.len() >= 2) {
            windows.push(chunk.to_vec());
        }
    }
    // Spread the sample over the whole corpus rather than its first projects.
    let stride = windows.len().div_ceil(max_windows).max(1);
    windows.into_iter().step_by(stride).collect()
}

fn collect_fsproj(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if !matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some(".git" | "target" | "artifacts" | "bin" | "obj")
            ) {
                collect_fsproj(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "fsproj") {
            out.push(path);
        }
    }
}

/// Edits to a real file, by label: a comment on its first line, which leaves
/// its contribution as it was; its first `int` literal made a string, and its
/// first module-level binding's body made `failwith "edit"`, either of which
/// may change an export's type and no export's name; and a binding appended
/// at the end, which changes its exports (or breaks the file, which both folds
/// then recover from alike).
fn corpus_edits(src: &str) -> Vec<(&'static str, String)> {
    // The comment goes after a byte-order mark, which only leads a file.
    let bom = if src.starts_with('\u{feff}') {
        "\u{feff}".len()
    } else {
        0
    };
    let mut edits = vec![
        (
            "comment",
            format!("{}// edit\n{}", &src[..bom], &src[bom..]),
        ),
        ("append", format!("{src}\nlet borzoiIncrementalEdit = 0\n")),
    ];
    let literal = parse(src)
        .root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::INT32_LIT);
    let body = parse(src)
        .root
        .descendants()
        .filter_map(LetDecl::cast)
        .flat_map(|decl| decl.bindings().collect::<Vec<_>>())
        .find(|binding| binding.return_type().is_none())
        .and_then(|binding| binding.expr());
    if let Some(body) = body {
        let range = body.syntax().text_range();
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        edits.push((
            "retype",
            format!("{}failwith \"edit\"{}", &src[..start], &src[end..]),
        ));
    }
    if let Some(token) = literal {
        let range = token.text_range();
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        edits.push(("literal", format!("{}\"s\"{}", &src[..start], &src[end..])));
    }
    edits
}

/// Incremental inference equals cold inference after edits to every file of
/// windows of the corpus's real Compile lists, and a leading comment infers
/// only the commented file afresh wherever the resolver reused every other.
#[test]
#[ignore = "walks the corpus; run with BORZOI_CORPUS set"]
fn incremental_equals_batch_over_a_corpus_sample() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!("BORZOI_CORPUS unset; skipping the incremental inference corpus sample.");
        return;
    };
    let window = env_usize_or("BORZOI_INCREMENTAL_WINDOW", 6);
    let max_windows = env_usize_or("BORZOI_INCREMENTAL_WINDOWS", 24);
    let windows = corpus_windows(Path::new(&root), window, max_windows);
    assert!(
        !windows.is_empty(),
        "no Compile list found under the corpus"
    );
    let env = full_bcl_env();
    let (mut steps, mut comment_steps, mut comment_full_reuse, mut withheld) = (0, 0, 0, 0);
    let mut suffix_reuse = 0;
    for paths in &windows {
        let sources: Vec<String> = paths
            .iter()
            .map(|p| std::fs::read_to_string(p).expect("read a corpus file"))
            .collect();
        let base = cold_fold(paths, sources.clone(), env);
        for i in 0..paths.len() {
            for (label, edited) in corpus_edits(&sources[i]) {
                let mut after = sources.clone();
                after[i] = edited;
                let (_, reuse) = fold_step(&base, after, paths.len(), env)
                    .unwrap_or_else(|e| panic!("{} [{label}]: {e}", paths[i].display()));
                steps += 1;
                withheld += reuse.withheld_and_changed;
                if reuse.infer[i + 1..].iter().any(|&r| r) {
                    suffix_reuse += 1;
                }
                let expected: Vec<bool> = (0..paths.len()).map(|j| j != i).collect();
                if label == "comment" && reuse.resolve == expected {
                    comment_steps += 1;
                    assert_eq!(
                        reuse.infer,
                        expected,
                        "{}: a leading comment leaves the file's contribution as it was, so \
                         only it is inferred afresh",
                        paths[i].display()
                    );
                    comment_full_reuse += 1;
                }
            }
        }
    }
    eprintln!(
        "incremental inference corpus sample: {} windows, {steps} edits; {comment_full_reuse} of \
         {comment_steps} comment edits the resolver fully reused were fully reused; \
         {suffix_reuse} edits reused a later file; {withheld} files redone with a changed result \
         behind a reused resolution",
        windows.len()
    );
    assert!(
        comment_steps > 0,
        "no comment edit was fully reused by the resolver"
    );
    assert!(
        suffix_reuse > 0,
        "no edit reused a file after the edited one"
    );
}
