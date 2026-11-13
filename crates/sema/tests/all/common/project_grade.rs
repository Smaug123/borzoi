//! Grading a multi-file program against FCS checking it as one project:
//! every node and binder inference types in any implementation file must be
//! FCS's (`types-project`), and each cell of the using file (the last) that
//! is obligated must commit unless FCS rejects its line.

use std::path::{Path, PathBuf};

use super::{GradedDumps, full_bcl_env, invoke_fcs_dump_types_project, temp_fs_tree};
use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_sema::{
    AssemblyEnv, InferredProject, ProjectFile, ResolvedProject, SourceFile, SyntaxRecovery,
    infer_project, qualified_names, resolve_project_files,
};

/// One cell: its line in the using file, binder name, obligation and label.
pub type Cell = (u32, String, bool, String);

struct Folded {
    resolved: ResolvedProject,
    inferred: InferredProject,
}

fn fold(dir: &Path, sources: &[(String, String)], env: &AssemblyEnv, allow_errors: bool) -> Folded {
    let parsed: Vec<(SourceFile, SyntaxRecovery)> = sources
        .iter()
        .map(|(name, src)| {
            if name.ends_with(".fsi") {
                let p = parse_sig(src);
                assert!(p.errors.is_empty(), "{name}: {:?}\n{src}", p.errors);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Sig(SigFile::cast(p.root).expect("sig file")),
                    recovery,
                )
            } else {
                let p = parse(src);
                assert!(
                    allow_errors || p.errors.is_empty(),
                    "{name}: {:?}\n{src}",
                    p.errors
                );
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Impl(ImplFile::cast(p.root).expect("impl file")),
                    recovery,
                )
            }
        })
        .collect();
    let paths: Vec<PathBuf> = sources.iter().map(|(n, _)| dir.join(n)).collect();
    let srcs: Vec<SourceFile> = parsed.iter().map(|(f, _)| f.clone()).collect();
    let qnofs = qualified_names(&srcs, &paths);
    let files: Vec<ProjectFile> = parsed
        .into_iter()
        .zip(qnofs)
        .map(|((file, recovery), qnof)| ProjectFile::new(file, qnof, recovery))
        .collect();
    let resolved = resolve_project_files(&files, env);
    let inferred = infer_project(&files, &resolved, env);
    Folded { resolved, inferred }
}

#[derive(Default, Debug)]
pub struct Tally {
    pub cells: usize,
    pub committed: usize,
    pub committed_erroring: usize,
    pub exprs: usize,
    pub binders: usize,
    pub wrong: Vec<String>,
    pub missed: Vec<String>,
}

fn dump(files: &[(String, String)]) -> String {
    files
        .iter()
        .map(|(n, s)| format!("--- {n}\n{s}"))
        .collect::<Vec<_>>()
        .join("")
}

/// Grade one program: every node and binder inference types in any file
/// against FCS's, then each cell's obligation in the using file (the last).
pub fn check(sources: &[(String, String)], cells: &[Cell], tally: &mut Tally) {
    check_with(sources, cells, tally, false);
}

/// [`check`], permitting parse errors in the sources.
pub fn check_with(
    sources: &[(String, String)],
    cells: &[Cell],
    tally: &mut Tally,
    allow_errors: bool,
) {
    let named: Vec<(&str, &str)> = sources
        .iter()
        .map(|(n, s)| (n.as_str(), s.as_str()))
        .collect();
    let (root, written) = temp_fs_tree("project_grade", &named);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let types = invoke_fcs_dump_types_project(&paths);
    let folded = fold(&root, sources, full_bcl_env(), allow_errors);
    let _ = std::fs::remove_dir_all(&root);
    let last = sources.len() - 1;
    let mut ours_by_name = std::collections::HashMap::new();
    let mut using_errors = Vec::new();
    for (idx, (name, src)) in sources.iter().enumerate() {
        // FCS's `types-project` reports no node or binder for an
        // implementation file a signature pairs with, so such a file is not
        // graded here.
        let signatured = sources.iter().any(|(n, _)| *n == format!("{name}i"));
        if name.ends_with(".fsi") || signatured {
            continue;
        }
        let Some(inferred) = folded.inferred.file(idx) else {
            continue;
        };
        let parsed = parse(src);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let (types_json, binders_json) = &types[idx];
        let dumps = GradedDumps::read(types_json, binders_json, src, Some(&file))
            .expect("every declaration is placeable");
        if idx == last {
            using_errors = dumps.errors.iter().map(|e| e.line).collect();
        }
        let line_of = |offset: usize| src[..offset].matches('\n').count() as u32 + 1;
        for (range, ty) in inferred.types() {
            let key = (usize::from(range.start()), usize::from(range.end()));
            if dumps.is_ungraded(key) {
                continue;
            }
            let ours = ty.render(full_bcl_env());
            match dumps.types.get(&key) {
                Some(theirs) if *theirs == ours => tally.exprs += 1,
                theirs => tally.wrong.push(format!(
                    "{name} `{}` (line {}): ours `{ours}`, FCS {theirs:?}\n{}",
                    &src[key.0..key.1],
                    line_of(key.0),
                    dump(sources)
                )),
            }
        }
        let rf = folded.resolved.file(idx);
        for (def, ty) in inferred.def_types() {
            let d = rf.def(*def);
            let key = (usize::from(d.range.start()), usize::from(d.range.end()));
            let ours = ty.render(full_bcl_env());
            // A commit counts toward its cell even where FCS's dumps grade
            // nothing (an error-recovered declaration), so the erroring
            // count sees it.
            if idx == last {
                ours_by_name.insert(
                    (d.spelling(full_bcl_env()).into_owned(), line_of(key.0)),
                    ours.clone(),
                );
            }
            if dumps.is_ungraded(key) {
                continue;
            }
            match dumps.binders.get(&key) {
                Some(theirs) if *theirs == ours && !dumps.hides_constraint(key, &ours) => {
                    tally.binders += 1;
                }
                theirs => tally.wrong.push(format!(
                    "{name} binder `{}` (line {}): ours `{ours}`, FCS {theirs:?}\n{}",
                    d.spelling(full_bcl_env()),
                    line_of(key.0),
                    dump(sources)
                )),
            }
        }
    }
    for (line, name, obligated, label) in cells {
        tally.cells += 1;
        let erroring = using_errors.contains(line);
        match ours_by_name.get(&(name.clone(), *line)) {
            Some(_) => {
                tally.committed += 1;
                tally.committed_erroring += usize::from(erroring);
            }
            None if *obligated && !erroring => tally.missed.push(label.clone()),
            None => {}
        }
    }
}

pub fn assert_clean(tally: &Tally, floor: usize) {
    eprintln!(
        "cells {} committed {} (erroring {}) exprs {} binders {}",
        tally.cells, tally.committed, tally.committed_erroring, tally.exprs, tally.binders
    );
    assert!(
        tally.wrong.is_empty(),
        "{} disagreements with FCS:\n{}",
        tally.wrong.len(),
        tally.wrong.join("\n")
    );
    assert!(
        tally.missed.is_empty(),
        "{} obligated cells declined:\n{}",
        tally.missed.len(),
        tally.missed.join("\n")
    );
    assert!(tally.committed >= floor, "too little committed: {tally:?}");
}
