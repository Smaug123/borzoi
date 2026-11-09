//! Grading generated probe sites against FCS checking the same files as one
//! project — the diagnostic-aware use oracle the accessibility grids share
//! (`value_access_open_grid`, `case_access_grid`, `reopened_namespace_grid`).
//!
//! FCS error-recovers an inaccessible use to its declaration, so a use-site
//! accessibility error means the only correct answer is to decline. Otherwise
//! certain-implies-exact: a committed target must be FCS's `(file, range)`,
//! and a site FCS leaves unbound or rejects must not commit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{LineIndex, invoke_fcs_dump_project, parse_fcs_uses_project, temp_fs_tree};
use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_sema::{
    AssemblyEnv, ProjectFile, Resolution, SourceFile, SyntaxRecovery, qualified_names,
    resolve_project_files,
};
use rowan::TextRange;

/// A probed reference: which generated file, the byte range FCS reports the
/// use at (the whole dotted path for a qualified reference, the identifier
/// otherwise), and a label naming the grid cell.
#[derive(Clone, Debug)]
pub struct Site {
    pub file: usize,
    pub range: TextRange,
    pub label: String,
}

/// A generated file under construction, recording probe sites as lines are
/// appended.
pub struct FileBuilder {
    pub index: usize,
    pub text: String,
}

impl FileBuilder {
    pub fn new(index: usize) -> Self {
        Self {
            index,
            text: String::new(),
        }
    }

    pub fn line(&mut self, line: &str) {
        self.text.push_str(line);
        self.text.push('\n');
    }

    /// Append `line` and record a probe at the last occurrence of `probe` in
    /// it.
    pub fn probe(&mut self, line: &str, probe: &str, label: String, sites: &mut Vec<Site>) {
        let at = line.rfind(probe).expect("probe text occurs in its line");
        let start = self.text.len() + at;
        self.line(line);
        sites.push(Site {
            file: self.index,
            range: TextRange::new(
                u32::try_from(start).unwrap().into(),
                u32::try_from(start + probe.len()).unwrap().into(),
            ),
            label,
        });
    }
}

/// FCS's accessibility diagnostics: FS0491 (not accessible from this code
/// location), FS1092/FS1093/FS1094 (type, union cases or field, value).
fn is_accessibility_error(number: i64) -> bool {
    matches!(number, 491 | 1092 | 1093 | 1094)
}

/// Every error diagnostic FCS reported, as `(file name, byte range, number)`.
fn error_diagnostics(
    json: &str,
    sources: &[(PathBuf, String)],
) -> Vec<(PathBuf, usize, usize, i64)> {
    let dump: serde_json::Value = serde_json::from_str(json).expect("uses-project JSON");
    let mut out = Vec::new();
    for file in dump["Files"].as_array().expect("Files array") {
        for d in file["Diagnostics"].as_array().into_iter().flatten() {
            if d["Severity"] != "Error" {
                continue;
            }
            let number = d["ErrorNumber"].as_i64().unwrap_or(-1);
            let range = &d["Range"];
            let name = PathBuf::from(
                Path::new(range["File"].as_str().expect("diagnostic file"))
                    .file_name()
                    .expect("file name"),
            );
            let Some((_, src)) = sources
                .iter()
                .find(|(p, _)| p.file_name() == Some(name.as_os_str()))
            else {
                continue;
            };
            let idx = LineIndex::new(src);
            let pos = |p: &serde_json::Value| {
                idx.offset(
                    u32::try_from(p["Line"].as_u64().unwrap()).unwrap(),
                    u32::try_from(p["Col"].as_u64().unwrap()).unwrap(),
                )
            };
            out.push((name, pos(&range["Start"]), pos(&range["End"]), number));
        }
    }
    out
}

/// What FCS says at one site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FcsVerdict {
    /// Resolved, cleanly, to a declaration in `(file index, range)`.
    Resolved(usize, TextRange),
    /// A use-site accessibility error: inaccessible here.
    Inaccessible,
    /// No use recorded at the site (unbound), or an error at the site.
    Rejected,
}

/// What we say at one site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OurVerdict {
    Committed(usize, TextRange),
    Declined,
}

pub struct Graded {
    pub label: String,
    pub fcs: FcsVerdict,
    pub ours: OurVerdict,
}

/// Fold `files` (Compile order, `(relative name, source)`; a `.fsi` is a
/// signature) as one project, both through FCS and through sema, and grade
/// every site. `tag` names the temporary directory. Sema reads no assembly.
pub fn grade(tag: &str, files: &[(&str, String)], sites: &[Site]) -> Vec<Graded> {
    grade_in(tag, files, sites, &AssemblyEnv::default())
}

/// [`grade`], with sema reading `env`.
pub fn grade_in(
    tag: &str,
    files: &[(&str, String)],
    sites: &[Site],
    env: &AssemblyEnv,
) -> Vec<Graded> {
    let tree: Vec<(&str, &str)> = files.iter().map(|(n, s)| (*n, s.as_str())).collect();
    let (root, written) = temp_fs_tree(tag, &tree);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project(&paths);
    let fcs = parse_fcs_uses_project(&json, &written);
    let errors = error_diagnostics(&json, &written);
    let parsed: Vec<(SourceFile, SyntaxRecovery)> = files
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
                assert!(p.errors.is_empty(), "{name}: {:?}\n{src}", p.errors);
                let recovery = SyntaxRecovery::of(&p);
                (
                    SourceFile::Impl(ImplFile::cast(p.root).expect("impl file")),
                    recovery,
                )
            }
        })
        .collect();
    let disk: Vec<PathBuf> = written.iter().map(|(p, _)| p.clone()).collect();
    let sources: Vec<SourceFile> = parsed.iter().map(|(f, _)| f.clone()).collect();
    let qnofs = qualified_names(&sources, &disk);
    let project: Vec<ProjectFile> = parsed
        .into_iter()
        .zip(qnofs)
        .map(|((file, recovery), qnof)| ProjectFile::new(file, qnof, recovery))
        .collect();
    let proj = resolve_project_files(&project, env);
    let _ = std::fs::remove_dir_all(&root);
    let index_of = |path: &Path| {
        written
            .iter()
            .position(|(p, _)| p.file_name() == path.file_name())
            .expect("declaration in a generated file")
    };

    sites
        .iter()
        .map(|site| {
            let path = &written[site.file].0;
            let start = usize::from(site.range.start());
            let end = usize::from(site.range.end());
            let here: Vec<i64> = errors
                .iter()
                .filter(|(f, s, e, _)| {
                    path.file_name() == Some(f.as_os_str()) && *s < end && start < *e
                })
                .map(|(_, _, _, n)| *n)
                .collect();
            let decl = fcs
                .iter()
                .find(|f| f.path.file_name() == path.file_name())
                .and_then(|f| {
                    f.uses
                        .iter()
                        .find(|u| u.start == start && u.end == end && !u.is_from_definition)
                })
                .and_then(|u| u.decl.as_ref());
            let fcs = if here.iter().any(|n| is_accessibility_error(*n)) {
                FcsVerdict::Inaccessible
            } else if !here.is_empty() {
                FcsVerdict::Rejected
            } else {
                match decl {
                    Some(d) => FcsVerdict::Resolved(
                        index_of(&d.file),
                        TextRange::new(
                            u32::try_from(d.start).unwrap().into(),
                            u32::try_from(d.end).unwrap().into(),
                        ),
                    ),
                    None => FcsVerdict::Rejected,
                }
            };
            let res = proj.file(site.file).resolution_at(site.range);
            let ours = match res {
                Some(r @ (Resolution::Item(_) | Resolution::EarlierFileType { .. })) => {
                    proj.item_def(r).map_or(OurVerdict::Declined, |(f, def)| {
                        OurVerdict::Committed(f, def.range)
                    })
                }
                Some(r @ Resolution::Local(_)) => proj
                    .file(site.file)
                    .resolved_def(r)
                    .map_or(OurVerdict::Declined, |def| {
                        OurVerdict::Committed(site.file, def.range)
                    }),
                _ => OurVerdict::Declined,
            };
            Graded {
                label: site.label.clone(),
                fcs,
                ours,
            }
        })
        .collect()
}

/// A site's verdict pair is wrong when we commit anything FCS does not
/// resolve to exactly that declaration.
pub fn is_wrong(g: &Graded) -> bool {
    match (&g.fcs, &g.ours) {
        (_, OurVerdict::Declined) => false,
        (FcsVerdict::Resolved(f, r), OurVerdict::Committed(of, or)) => (f, r) != (of, or),
        (FcsVerdict::Inaccessible | FcsVerdict::Rejected, OurVerdict::Committed(..)) => true,
    }
}

/// We commit, and to FCS's answer.
pub fn agrees(g: &Graded) -> bool {
    matches!(&g.ours, OurVerdict::Committed(..)) && !is_wrong(g)
}

pub fn census(graded: &[Graded]) -> String {
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for g in graded {
        let fcs = match g.fcs {
            FcsVerdict::Resolved(..) => "resolved",
            FcsVerdict::Inaccessible => "inaccessible",
            FcsVerdict::Rejected => "rejected",
        };
        let ours = match (&g.fcs, &g.ours) {
            (_, OurVerdict::Declined) => "declined",
            _ if is_wrong(g) => "WRONG",
            _ => "agreed",
        };
        *by.entry(format!("fcs {fcs:<12} ours {ours}")).or_default() += 1;
    }
    by.into_iter()
        .map(|(k, n)| format!("  {k}: {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn report(graded: &[Graded]) -> String {
    graded
        .iter()
        .map(|g| format!("  {:<72} fcs {:?} | ours {:?}", g.label, g.fcs, g.ours))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Assert that the sites committing a target FCS does not bind are exactly
/// `known_wrong` — a pin, so a site that starts committing wrongly fails, and
/// so does one a fix corrects until it leaves the list — and that every label
/// in `must_commit` commits FCS's answer. `BORZOI_GRID_REPORT` prints every
/// cell.
pub fn assert_sound_and_live(
    what: &str,
    graded: &[Graded],
    known_wrong: &[&str],
    must_commit: &[&str],
) {
    eprintln!("{what} census:\n{}", census(graded));
    if std::env::var_os("BORZOI_GRID_REPORT").is_some() {
        eprintln!("{what} cells:\n{}", report(graded));
    }
    let wrong: Vec<&Graded> = graded
        .iter()
        .filter(|g| is_wrong(g) && !known_wrong.contains(&g.label.as_str()))
        .collect();
    assert!(
        wrong.is_empty(),
        "{} {what} site(s) commit a target FCS does not bind there:\n{}",
        wrong.len(),
        wrong
            .iter()
            .map(|g| format!("  [{}] FCS {:?} | we gave {:?}", g.label, g.fcs, g.ours))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let corrected: Vec<&&str> = known_wrong
        .iter()
        .filter(|label| !graded.iter().any(|g| g.label == **label && is_wrong(g)))
        .collect();
    assert!(
        corrected.is_empty(),
        "{what} sites pinned as wrong no longer are; remove them from the pin:\n{corrected:#?}"
    );
    let missing: Vec<String> = must_commit
        .iter()
        .filter_map(|label| {
            let g = graded.iter().find(|g| g.label == *label);
            match g {
                Some(g) if agrees(g) => None,
                Some(g) => Some(format!(
                    "  [{label}] FCS {:?} | we gave {:?}",
                    g.fcs, g.ours
                )),
                None => Some(format!("  [{label}] is not a site of the grid")),
            }
        })
        .collect();
    assert!(
        missing.is_empty(),
        "{what}: must-commit controls do not commit FCS's answer:\n{}",
        missing.join("\n")
    );
}
