//! The two sides of the source-doc differential and the comparison between
//! them.
//!
//! **FCS** type-checks a fixture as one project (`fcs-dump xmldoc-batch`) and
//! reports, for every symbol use, the doc FCS attaches to the symbol bound
//! there (`XmlDoc.UnprocessedLines` and the elaborated lines).
//!
//! **We** parse and fold the same files exactly as the LSP does, and for every
//! occurrence the resolver recorded ask [`ProjectDocs`] — the function hover
//! calls — what it would show.
//!
//! The comparison is *certain-implies-exact*: an [`SourceDoc::Attached`] answer
//! must equal FCS's lines (and our elaboration FCS's elaboration) at the same
//! site; a decline makes no claim and is counted by cause.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use borzoi::xml_doc::source::{
    ProjectDocs, SourceDoc, SourceDocDecline, SourceRenderError, elaborate, is_blank,
    member_element,
};
use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_oracle_harness::BatchChild;
use borzoi_sema::{
    AssemblyEnv, Def, DefKind, ProjectFile, Resolution, ResolvedProject, SourceFile,
    SyntaxRecovery, qualified_names, resolve_project_files,
};
use rowan::TextRange;
use serde::Deserialize;

use crate::common::{LineIndex, fcs_dump_batch_child};

// ============================================================================
// FCS side
// ============================================================================

#[derive(Debug, Deserialize)]
struct OracleResponse {
    #[serde(rename = "Files")]
    files: Option<Vec<OracleFile>>,
    #[serde(rename = "BatchError")]
    batch_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct OracleFile {
    pub path: String,
    pub ok: bool,
    pub error: String,
    pub diagnostics: Vec<OracleDiagnostic>,
    pub uses: Vec<OracleUse>,
}

impl OracleFile {
    /// Whether FCS reported an error (parse or check) in this file.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity == "Error")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct OracleDiagnostic {
    pub severity: String,
    pub error_number: i32,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct OracleUse {
    pub name: String,
    pub kind: String,
    pub range: FcsRange,
    pub unprocessed: Option<Vec<String>>,
    pub elaborated: Option<Vec<String>>,
    pub from_file: bool,
    pub error: Option<String>,
}

impl OracleUse {
    /// The lines FCS attaches, with "no `FromXmlText` doc" read as none.
    fn lines(&self) -> &[String] {
        self.unprocessed.as_deref().unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FcsRange {
    pub start: FcsPos,
    pub end: FcsPos,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FcsPos {
    pub line: u32,
    pub col: u32,
}

/// The process-wide resident oracle. One child serves every request, in
/// lock-step, so the suite pays .NET + FCS start-up once.
fn oracle() -> &'static Mutex<BatchChild> {
    static ORACLE: OnceLock<Mutex<BatchChild>> = OnceLock::new();
    ORACLE.get_or_init(|| Mutex::new(fcs_dump_batch_child("xmldoc-batch")))
}

/// Type-check `paths` (Compile order) as one project under `defines`.
pub fn fcs_check(paths: &[PathBuf], defines: &[&str]) -> Vec<OracleFile> {
    let request = serde_json::json!({
        "paths": paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "defines": defines,
    })
    .to_string();
    let line = oracle()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .request(&request);
    let response: OracleResponse = serde_json::from_str(&line)
        .unwrap_or_else(|e| panic!("unparseable xmldoc-batch response ({e}): {line}"));
    if let Some(err) = response.batch_error {
        panic!("xmldoc-batch failed: {err}");
    }
    response.files.expect("Files")
}

// ============================================================================
// Our side
// ============================================================================

/// One project, parsed and folded the way the LSP does it.
pub struct OurProject {
    pub texts: Vec<String>,
    pub files: Vec<ProjectFile>,
    pub resolved: ResolvedProject,
}

impl OurProject {
    pub fn new(paths: &[PathBuf], texts: Vec<String>, defines: &[&str]) -> Self {
        let symbols: HashSet<String> = defines.iter().map(|d| d.to_string()).collect();
        let mut sources = Vec::new();
        let mut recoveries = Vec::new();
        for (path, text) in paths.iter().zip(&texts) {
            let sig = path.extension().is_some_and(|e| e == "fsi");
            let parse = parse_with_options(
                text,
                ParseOptions {
                    file_kind: if sig { FileKind::Sig } else { FileKind::Impl },
                    symbols: &symbols,
                    lang: LanguageVersion::DEFAULT,
                },
            );
            recoveries.push(SyntaxRecovery::of(&parse));
            sources.push(if sig {
                SourceFile::Sig(SigFile::cast(parse.root).expect("sig root"))
            } else {
                SourceFile::Impl(ImplFile::cast(parse.root).expect("impl root"))
            });
        }
        let qnofs = qualified_names(&sources, paths);
        let files: Vec<ProjectFile> = sources
            .into_iter()
            .zip(qnofs)
            .zip(recoveries)
            .map(|((f, q), r)| ProjectFile::new(f, q, r))
            .collect();
        let resolved = resolve_project_files(&files, &AssemblyEnv::default());
        OurProject {
            texts,
            files,
            resolved,
        }
    }

    /// The binder `res` (an occurrence in file `from`) names.
    fn def_of(&self, from: usize, res: Resolution) -> Option<&Def> {
        match res {
            Resolution::Local(id) => Some(self.resolved.file(from).def(id)),
            Resolution::Item(_) => self.resolved.item_def(res).map(|(_, d)| d),
            _ => None,
        }
    }
}

// ============================================================================
// Comparison
// ============================================================================

/// One graded occurrence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// We attach these lines and FCS attaches the same.
    Agree { attached: bool },
    /// We decline; FCS's doc is unknown to us.
    Declined(SourceDocDecline),
    /// We attach lines FCS does not — the hard failure.
    Diverge { ours: Vec<String>, fcs: Vec<String> },
    /// Our lines agree but our elaboration does not.
    ElaborationDiverges { ours: Vec<String>, fcs: Vec<String> },
    /// FCS reports several symbols at this range with different docs and none
    /// is the kind our binder is.
    OracleAmbiguous,
    /// FCS reports no use at this range.
    Unpaired,
}

/// One graded occurrence with where it is.
#[derive(Debug, Clone)]
pub struct Graded {
    pub file: usize,
    pub range: TextRange,
    pub name: String,
    pub def_kind: DefKind,
    pub is_definition: bool,
    pub verdict: Verdict,
    /// Our attached lines (blank ones read as none), when we attached.
    pub ours: Option<Vec<String>>,
    /// The lines of the FCS use this occurrence was paired with.
    pub fcs_lines: Option<Vec<String>>,
    /// Our answer rendered, when we attached one: `Ok` the tree parsed, `Err`
    /// why hover shows nothing for it.
    pub render: Option<Result<(), SourceRenderError>>,
}

/// The FCS symbol kind an FCS use of our binder of `kind` carries.
fn expected_kind(kind: DefKind) -> &'static [&'static str] {
    match kind {
        DefKind::Type => &["entity"],
        DefKind::ExceptionCase => &["entity", "unioncase"],
        DefKind::UnionCase => &["unioncase"],
        DefKind::EnumCase => &["field"],
        DefKind::ActivePatternCase => &["activepatterncase"],
        DefKind::TypeParam => &["genericparameter"],
        DefKind::Value { .. }
        | DefKind::ActivePattern
        | DefKind::Member
        | DefKind::Parameter
        | DefKind::PatternLocal => &["member"],
    }
}

/// Grade every occurrence our resolver recorded in every file of `ours`
/// against `fcs` (parallel to the files).
pub fn grade(ours: &OurProject, fcs: &[OracleFile]) -> Vec<Graded> {
    let mut docs = ProjectDocs::new(&ours.files, &ours.resolved);
    let mut out = Vec::new();
    for (i, oracle_file) in fcs.iter().enumerate() {
        let text = &ours.texts[i];
        let lines = LineIndex::new(text);
        // FCS reads the text with a byte-order mark stripped, so its line-1
        // columns start after it.
        let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
        let offset = |p: FcsPos| lines.offset(p.line, p.col) + if p.line == 1 { bom } else { 0 };
        let mut by_range: HashMap<(usize, usize), Vec<&OracleUse>> = HashMap::new();
        let mut by_end: HashMap<usize, Vec<&OracleUse>> = HashMap::new();
        for u in &oracle_file.uses {
            let start = offset(u.range.start);
            let end = offset(u.range.end);
            by_range.entry((start, end)).or_default().push(u);
            by_end.entry(end).or_default().push(u);
        }
        let mut occurrences: Vec<(TextRange, Resolution)> = ours
            .resolved
            .file(i)
            .resolutions()
            .iter()
            .map(|(r, res)| (*r, *res))
            .collect();
        occurrences.sort_by_key(|(r, _)| (r.start(), r.end()));
        for (range, res) in occurrences {
            let Some(def) = ours.def_of(i, res) else {
                continue;
            };
            let Some(doc) = docs.doc(i, res) else {
                continue;
            };
            let key = (usize::from(range.start()), usize::from(range.end()));
            let candidates: Vec<&OracleUse> = match by_range.get(&key) {
                Some(c) => c.clone(),
                None => by_end
                    .get(&key.1)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|u| expected_kind(def.kind).contains(&u.kind.as_str()))
                    .collect(),
            };
            let is_definition = def.range == range;
            let (verdict, fcs_lines) = judge(def.kind, &doc, &candidates);
            let render = match &doc {
                SourceDoc::Attached(lines) => Some(member_element(lines).map(|_| ())),
                SourceDoc::Declined(_) => None,
            };
            let ours_lines = match &doc {
                SourceDoc::Attached(lines) if is_blank(lines) => Some(Vec::new()),
                SourceDoc::Attached(lines) => Some(lines.clone()),
                SourceDoc::Declined(_) => None,
            };
            out.push(Graded {
                file: i,
                range,
                name: def.name.clone(),
                def_kind: def.kind,
                is_definition,
                verdict,
                ours: ours_lines,
                fcs_lines,
                render,
            });
        }
    }
    out
}

fn judge(
    kind: DefKind,
    doc: &SourceDoc,
    candidates: &[&OracleUse],
) -> (Verdict, Option<Vec<String>>) {
    let chosen: Option<&OracleUse> = if candidates.is_empty() {
        None
    } else if candidates
        .iter()
        .all(|c| c.lines() == candidates[0].lines())
    {
        Some(candidates[0])
    } else {
        let of_kind: Vec<_> = candidates
            .iter()
            .filter(|c| expected_kind(kind).contains(&c.kind.as_str()))
            .collect();
        (of_kind.len() == 1).then(|| *of_kind[0])
    };
    if let Some(fcs) = chosen {
        assert!(
            !fcs.from_file && fcs.error.is_none(),
            "oracle surprise at {}: {fcs:?}",
            fcs.name
        );
    }
    let fcs_lines = chosen.map(|c| c.lines().to_vec());
    let ours = match doc {
        SourceDoc::Declined(why) => return (Verdict::Declined(*why), fcs_lines),
        SourceDoc::Attached(lines) => lines,
    };
    if candidates.is_empty() {
        return (Verdict::Unpaired, fcs_lines);
    }
    let Some(fcs) = chosen else {
        return (Verdict::OracleAmbiguous, fcs_lines);
    };
    // FCS reports a blank doc (`XmlDoc.IsEmpty`) as no doc at all, so the
    // oracle cannot tell the two apart; hover shows neither.
    let empty = Vec::new();
    let ours = if is_blank(ours) { &empty } else { ours };
    if fcs.lines() != ours.as_slice() {
        let verdict = Verdict::Diverge {
            ours: ours.clone(),
            fcs: fcs.lines().to_vec(),
        };
        return (verdict, fcs_lines);
    }
    let fcs_elaborated = fcs.elaborated.as_deref().unwrap_or(&[]);
    let our_elaborated = elaborate(ours);
    if our_elaborated != fcs_elaborated {
        let verdict = Verdict::ElaborationDiverges {
            ours: our_elaborated,
            fcs: fcs_elaborated.to_vec(),
        };
        return (verdict, fcs_lines);
    }
    let verdict = Verdict::Agree {
        attached: !ours.is_empty(),
    };
    (verdict, fcs_lines)
}

/// A census of graded verdicts by kind, for printing.
pub fn census(graded: &[Graded]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for g in graded {
        let key = match &g.verdict {
            Verdict::Agree { attached: true } => match &g.render {
                Some(Err(SourceRenderError::Malformed(_))) => {
                    "agree: doc (malformed XML, not shown)".to_string()
                }
                Some(Err(SourceRenderError::TooDeep)) => {
                    "agree: doc (too deep, not shown)".to_string()
                }
                _ => "agree: doc".to_string(),
            },
            Verdict::Agree { attached: false } => "agree: none".to_string(),
            // What the decline cost: whether FCS had a doc to show there.
            Verdict::Declined(why) => match g.fcs_lines.as_deref() {
                Some(lines) if !lines.is_empty() => format!("declined: {why:?} (FCS has a doc)"),
                Some(_) => format!("declined: {why:?} (FCS has none)"),
                None => format!("declined: {why:?} (unpaired)"),
            },
            Verdict::Diverge { .. } => "DIVERGE".to_string(),
            Verdict::ElaborationDiverges { .. } => "DIVERGE (elaboration)".to_string(),
            Verdict::OracleAmbiguous => "oracle ambiguous".to_string(),
            Verdict::Unpaired => match g.ours.as_deref() {
                Some(lines) if !lines.is_empty() => format!("unpaired: doc ({:?})", g.def_kind),
                _ => format!("unpaired: none ({:?})", g.def_kind),
            },
        };
        *counts.entry(key).or_default() += 1;
    }
    counts
}

/// Everything that must not happen: a divergence of either kind.
pub fn failures(graded: &[Graded]) -> Vec<&Graded> {
    graded
        .iter()
        .filter(|g| {
            matches!(
                g.verdict,
                Verdict::Diverge { .. } | Verdict::ElaborationDiverges { .. }
            )
        })
        .collect()
}

/// Write `files` (name, text) into a fresh directory, check them with FCS and
/// with us, and grade. Returns the graded occurrences and FCS's file reports.
pub fn run_fixture(files: &[(&str, &str)], defines: &[&str]) -> (Vec<Graded>, Vec<OracleFile>) {
    let dir = tempfile::TempDir::new().unwrap();
    let paths: Vec<PathBuf> = files
        .iter()
        .map(|(name, text)| {
            let p = dir.path().join(name);
            std::fs::write(&p, text).unwrap();
            p
        })
        .collect();
    let texts = files.iter().map(|(_, t)| t.to_string()).collect();
    let (graded, fcs) = check_paths(&paths, texts, defines);
    for f in &fcs {
        assert!(f.ok, "FCS could not check {}: {}", f.path, f.error);
    }
    (graded, fcs)
}

/// Check the files at `paths` (whose contents are `texts`) as one project with
/// FCS and with us, and grade.
pub fn check_paths(
    paths: &[PathBuf],
    texts: Vec<String>,
    defines: &[&str],
) -> (Vec<Graded>, Vec<OracleFile>) {
    let fcs = fcs_check(paths, defines);
    assert_eq!(fcs.len(), paths.len(), "one oracle record per file");
    let ours = OurProject::new(paths, texts, defines);
    (grade(&ours, &fcs), fcs)
}

/// Render a failure for a panic message.
pub fn describe(files: &[(&str, &str)], g: &Graded) -> String {
    let text = files[g.file].1;
    let start = usize::from(g.range.start());
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    format!(
        "{} `{}` ({:?}, {}) at {}:{}: {:?}\n  line: {}",
        files[g.file].0,
        g.name,
        g.def_kind,
        if g.is_definition { "definition" } else { "use" },
        files[g.file].0,
        start,
        g.verdict,
        &text[line_start..line_end],
    )
}
