//! Generative **same-file 3-segment module-qualified-path** differential vs FCS.
//!
//! The accessibility audit of the direct qualified-path channels (PR #1000
//! cross-file case, #1003 cross-file value, #1005 same-file value, #1009 same-file
//! module-qualified case + static member) was driven by *curated* fcs-pinned tests
//! that codex found one corner at a time — the same-file `A.Foo.Red` resolver
//! collided with a different downstream branch (cross-file fallback, farther
//! same-file candidate, companion module, inaccessible companion value) on each of
//! four review rounds. This harness closes that gap systematically: it *enumerates*
//! programs that write a same-file `A.Foo.Red` and sweeps the dimensions those
//! corners lived in —
//!
//! - **how `Foo` (and its `Red`) is declared** in the nearer `module A`: a union
//!   case, a class static member, a companion module value, a `type private Foo`
//!   with a public companion module (the FS1092-type-but-accessible-companion
//!   corner), or absent;
//! - **the accessibility** of that reading: a public type, a `type private Foo`
//!   (FS1092 from a sibling), a `module private A` (private-but-sibling-visible,
//!   which must *not* be over-blocked), or an inaccessible companion value
//!   (`module private Foo` / `let private Red`);
//! - **whether a farther same-named candidate exists** — an outer public
//!   `module A = type Foo = | Red` the walk must fall back to.
//!
//! Two sweeps over that space: a **single-file** sweep (findings 1-3 — the
//! sole-inaccessible over-bind, the walk to a farther same-file candidate, the
//! accessible-companion win) and an **earlier-export** sweep (finding 4 — a second,
//! earlier file that exports the literal path `A.Foo.Red`, so that when the nearer
//! reading *and* its companion value are both inaccessible, the same-file walk must
//! still reach the farther same-file `A.Foo.Red` rather than commit the earlier
//! file's export). Each diffs FCS's resolution of the sibling `A.Foo.Red` against
//! ours.
//!
//! **The oracle is diagnostic-aware.** A plain resolution diff cannot see the
//! *sole-inaccessible* corner (#1009's headline: a `type private Foo` referenced
//! from a sibling with no farther candidate): FCS **error-recovers** to the
//! inaccessible declaration and still reports it as the symbol's decl (with FS1092),
//! so "we bound it too" reads as agreement and "we deferred" reads as an allowed
//! availability loss — the bug is invisible. So when FCS emits a **use-site
//! accessibility error** (FS491/1092/1093/1094 overlapping the reference), the
//! reference is genuinely inaccessible and the *only* correct answer is to **defer**:
//! committing any target there is a wrong go-to-def. FS410 ("less accessible", a
//! declaration-site diagnostic where the use still resolves) is neither in the set
//! nor on the reference line, so it is doubly excluded.
//!
//! Certain-implies-exact: when FCS resolves and we resolve, the `(file, range)`
//! must match; when FCS resolves and we defer, that is allowed; when FCS is
//! inaccessible or unbound, we must not commit a target. Deferring is always the
//! fail-safe; a wrong or divergent target fails.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::common::{LineIndex, invoke_fcs_dump_project, parse_fcs_uses_project, temp_fs_file};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, Resolution, resolve_project};
use rowan::TextRange;
use serde::Deserialize;

fn impl_file(src: &str) -> ImplFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        p.errors
    );
    ImplFile::cast(p.root).expect("impl file")
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// How the probed `Foo` (owning `Red`) is declared in the nearer `module A`. The
/// body is emitted at an 8-space indent (under `module [private] A =` at 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NearFoo {
    /// `type Foo = | Red` — `Red` is a union case (a genuine case needs the leading
    /// bar; `type Foo = Red` would be a type *abbreviation*).
    UnionPub,
    /// `type private Foo = | Red` — inaccessible from a sibling (FS1092).
    UnionPriv,
    /// `type Foo() = static member Red` — `Red` is a static member.
    StaticPub,
    /// `type private Foo() = static member Red` — inaccessible from a sibling.
    StaticPriv,
    /// `module Foo = let Red` — `Red` is a companion-module value (`A.Foo.Red` is a
    /// qualified value path, not a case/member).
    CompanionPub,
    /// `module private Foo = let Red` — the value is inaccessible from a sibling.
    CompanionPriv,
    /// `type private Foo = | Hidden` + `module Foo = let Red` — an inaccessible type
    /// with an **accessible companion module** owning `Red` (FCS binds the companion
    /// `FooModule.Red`; #1009's companion corner). The union+module coexistence is
    /// legal (FCS renames the module to `FooModule`).
    UnionPrivCompanion,
    /// `let filler = 0` — `A` exists but owns no `Foo`, so the walk must fall through
    /// (FS39 when there is no farther candidate).
    Absent,
}

impl NearFoo {
    fn body(self) -> &'static str {
        match self {
            NearFoo::UnionPub => "        type Foo =\n            | Red\n            | Blue\n",
            NearFoo::UnionPriv => {
                "        type private Foo =\n            | Red\n            | Blue\n"
            }
            NearFoo::StaticPub => "        type Foo() =\n            static member Red = 0\n",
            NearFoo::StaticPriv => {
                "        type private Foo() =\n            static member Red = 0\n"
            }
            NearFoo::CompanionPub => "        module Foo =\n            let Red = 0\n",
            NearFoo::CompanionPriv => "        module private Foo =\n            let Red = 0\n",
            NearFoo::UnionPrivCompanion => {
                "        type private Foo =\n            | Hidden\n        module Foo =\n            let Red = 0\n"
            }
            NearFoo::Absent => "        let filler = 0\n",
        }
    }

    /// Whether `A.Foo.Red` is a valid **case pattern** for the nearer reading — only
    /// a union case is (a static member / companion value / abbreviation is not a
    /// pattern), so only these get a pattern probe.
    fn is_case(self) -> bool {
        matches!(self, NearFoo::UnionPub | NearFoo::UnionPriv)
    }

    fn tag(self) -> &'static str {
        match self {
            NearFoo::UnionPub => "unionPub",
            NearFoo::UnionPriv => "unionPriv",
            NearFoo::StaticPub => "staticPub",
            NearFoo::StaticPriv => "staticPriv",
            NearFoo::CompanionPub => "compPub",
            NearFoo::CompanionPriv => "compPriv",
            NearFoo::UnionPrivCompanion => "unionPrivComp",
            NearFoo::Absent => "absent",
        }
    }
}

/// Whether the nearer `A` is a plain or a `private` module. A private module is
/// visible from a *sibling* in its enclosing scope, so it must **not** block the
/// reference; this dimension guards against over-blocking on module privacy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NearMod {
    Pub,
    Priv,
}

impl NearMod {
    fn header(self) -> &'static str {
        match self {
            NearMod::Pub => "    module A =\n",
            NearMod::Priv => "    module private A =\n",
        }
    }
    fn tag(self) -> &'static str {
        match self {
            NearMod::Pub => "modPub",
            NearMod::Priv => "modPriv",
        }
    }
}

/// One generated program: the nearer `A.Foo` shape and module privacy, and whether
/// a farther outer `module A = type Foo = | Red` exists for the walk to fall to.
#[derive(Clone)]
struct Scenario {
    near_foo: NearFoo,
    near_mod: NearMod,
    outer: bool,
    label: String,
}

/// A materialised case: its files (`(name, source)`), which file holds the probed
/// reference, and the sites to probe — `(label, whole path range)`: an `"expr"` site
/// always, and a `"pat"` site when the nearer reading is a union case. Multi-file so
/// the *earlier-export* sweep (a farther same-file `A.Foo.Red` **plus** an earlier
/// file exporting the literal path `A.Foo.Red`) can be expressed.
#[derive(Clone)]
struct Case {
    files: Vec<(String, String)>,
    ref_local: usize,
    sites: Vec<(&'static str, TextRange)>,
    label: String,
}

const PATH: &str = "A.Foo.Red";

/// The probe sites for a reference source `src` written with the qualified `path`:
/// the expression `let y = <path>` (already in `src`) always, plus — when the nearer
/// reading is a union case — a pattern `match v with <path>` line appended to `src`.
fn probe_sites(src: &mut String, path: &str, is_case: bool) -> Vec<(&'static str, TextRange)> {
    // At this point `path` occurs once (the expression); `rfind` == `find`.
    let expr_off = src.rfind(path).expect("expression reference");
    let mut sites = vec![("expr", span(expr_off, expr_off + path.len()))];
    if is_case {
        src.push_str(&format!(
            "        let pf v = match v with {path} -> 0 | _ -> 1\n"
        ));
        let pat_off = src.rfind(path).expect("pattern reference");
        sites.push(("pat", span(pat_off, pat_off + path.len())));
    }
    sites
}

/// The single-file sweep: a scenario under a per-scenario module `P{gi}` (so
/// scenarios combined into one FCS invocation cannot interfere). The outer `module A`
/// (when present) is `P{gi}.A`; the nearer is `P{gi}.Nest.A`; the reference is the
/// sibling `P{gi}.Nest.B`.
fn single_file_case(gi: usize, sc: &Scenario) -> Case {
    let outer_block = if sc.outer {
        "module A =\n    type Foo =\n        | Red\n        | Blue\n\n"
    } else {
        ""
    };
    let mut src = format!(
        "module P{gi}\n\n{outer_block}module Nest =\n{}{}\n    module B =\n        let y = {PATH}\n",
        sc.near_mod.header(),
        sc.near_foo.body(),
    );
    let sites = probe_sites(&mut src, PATH, sc.near_foo.is_case());
    Case {
        files: vec![(format!("qpa_{gi}"), src)],
        ref_local: 0,
        sites,
        label: sc.label.clone(),
    }
}

/// The earlier-export sweep's file pair for scenario `gi`: an earlier file0 exporting
/// the literal path `A{gi}.Foo.Red`, and a reference file1 with the same shape built
/// around the per-scenario module name `A{gi}` (root-level, so distinct scenarios in
/// one FCS invocation cannot collide). This is the setup that exposes the
/// inaccessible-companion corner: when the nearer reading AND its companion value are
/// both inaccessible, the same-file walk must still reach the farther same-file
/// `A{gi}.Foo.Red` (or defer) rather than commit file0's earlier export.
fn earlier_export_case(gi: usize, sc: &Scenario) -> Case {
    let a = format!("A{gi}");
    let path = format!("{a}.Foo.Red");
    let file0 = format!("module {a}\n\nmodule Foo =\n    let Red = 111\n");
    let outer_block = if sc.outer {
        format!("module {a} =\n    type Foo =\n        | Red\n        | Blue\n\n")
    } else {
        String::new()
    };
    let near_header = match sc.near_mod {
        NearMod::Pub => format!("    module {a} =\n"),
        NearMod::Priv => format!("    module private {a} =\n"),
    };
    let mut file1 = format!(
        "module R{gi}\n\n{outer_block}module Nest =\n{near_header}{}\n    module B =\n        let y = {path}\n",
        sc.near_foo.body(),
    );
    let sites = probe_sites(&mut file1, &path, sc.near_foo.is_case());
    Case {
        files: vec![
            (format!("qpa_e{gi}_0"), file0),
            (format!("qpa_e{gi}_1"), file1),
        ],
        ref_local: 1,
        sites,
        label: format!("earlier/{}", sc.label),
    }
}

/// The headerless sweep: the same scenario shape but in a **headerless** file — no
/// file-module header, so the file's implicit module (named after its temp file) is
/// the anonymous root and nested-module bindings carry no `qualified` path. This is
/// the layout where an *accessible* companion value's accessibility is unprovable, so
/// the companion branch must keep its sound `Miss` delegation rather than step the
/// walk over it onto a farther candidate (the root union case) — a wrong target the
/// file-module sweeps cannot express. Distinct temp file names keep the implicit
/// modules distinct, so scenarios combine in one FCS invocation without colliding.
fn headerless_case(gi: usize, sc: &Scenario) -> Case {
    let outer_block = if sc.outer {
        "module A =\n    type Foo =\n        | Red\n        | Blue\n\n"
    } else {
        ""
    };
    let mut src = format!(
        "{outer_block}module Nest =\n{}{}\n    module B =\n        let y = {PATH}\n",
        sc.near_mod.header(),
        sc.near_foo.body(),
    );
    let sites = probe_sites(&mut src, PATH, sc.near_foo.is_case());
    Case {
        files: vec![(format!("qpa_h{gi}"), src)],
        ref_local: 0,
        sites,
        label: format!("headerless/{}", sc.label),
    }
}

// ---- FCS use-site accessibility diagnostics (the diagnostic-aware oracle) -------

/// The FCS use-site accessibility errors: type / union-cases-or-fields / value /
/// member "not accessible". FS410 ("less accessible", a declaration diagnostic) is
/// deliberately excluded.
fn is_accessibility_error(number: Option<i64>) -> bool {
    matches!(number, Some(491 | 1092 | 1093 | 1094))
}

#[derive(Deserialize)]
struct DiagDump {
    #[serde(rename = "Files")]
    files: Vec<DiagFile>,
}
#[derive(Deserialize)]
struct DiagFile {
    // `Path` is present in the JSON but unused: a diagnostic's own `Range.File` is
    // authoritative for which file (and text) it belongs to.
    #[serde(rename = "Diagnostics", default)]
    diagnostics: Vec<RawDiag>,
}
#[derive(Deserialize)]
struct RawDiag {
    #[serde(rename = "Severity")]
    severity: String,
    #[serde(rename = "ErrorNumber")]
    error_number: Option<i64>,
    #[serde(rename = "Range")]
    range: DiagRange,
}
#[derive(Deserialize)]
struct DiagRange {
    #[serde(rename = "File")]
    file: String,
    #[serde(rename = "Start")]
    start: DiagPos,
    #[serde(rename = "End")]
    end: DiagPos,
}
#[derive(Deserialize)]
struct DiagPos {
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Col")]
    col: u32,
}

/// Per-file byte ranges of use-site accessibility errors, keyed by file name.
/// `sources` supplies each file's text for the line→byte conversion.
fn accessibility_error_ranges(
    json: &str,
    sources: &[(PathBuf, String)],
) -> HashMap<PathBuf, Vec<(usize, usize)>> {
    let by_name: HashMap<&OsStr, &str> = sources
        .iter()
        .map(|(p, s)| (p.file_name().expect("file name"), s.as_str()))
        .collect();
    let dump: DiagDump = serde_json::from_str(json).expect("fcs-dump uses-project JSON shape");
    let mut out: HashMap<PathBuf, Vec<(usize, usize)>> = HashMap::new();
    for f in dump.files {
        for d in f.diagnostics {
            if d.severity != "Error" || !is_accessibility_error(d.error_number) {
                continue;
            }
            // The diagnostic's own range file is authoritative (a use-site error's
            // range is in the reference's file); convert against that file's text.
            let Some(name) = Path::new(&d.range.file).file_name() else {
                continue;
            };
            let Some(src) = by_name.get(name) else {
                continue;
            };
            let idx = LineIndex::new(src);
            let start = idx.offset(d.range.start.line, d.range.start.col);
            let end = idx.offset(d.range.end.line, d.range.end.col);
            out.entry(PathBuf::from(name))
                .or_default()
                .push((start, end));
        }
    }
    out
}

/// A disagreement between our resolution and FCS at one probed reference.
struct Divergence {
    label: String,
    fcs: String,
    ours: String,
}

/// FCS-grounded probe-site counts — the vacuity guards. `expr`/`pat` count sites FCS
/// resolved (kept apart so a pattern-range bug cannot pass silently); `blocked`
/// counts sites FCS ruled inaccessible (so the accessibility oracle is genuinely
/// exercised); `cross_file` counts sites where FCS resolved to a decl in a *different*
/// file and we agreed (so the earlier-export path is genuinely exercised).
#[derive(Default)]
struct Tally {
    expr: usize,
    pat: usize,
    blocked: usize,
    cross_file: usize,
}

/// Resolve and FCS-diff one chunk of cases (one `uses-project` invocation over every
/// file of every case). Each case's files are globally distinct (per-scenario module
/// names), so combining them cannot interfere.
fn diff_cases(cases: &[Case]) -> (Vec<Divergence>, Tally) {
    struct Probe {
        label: String,
        ref_global: usize,
        sites: Vec<(&'static str, TextRange)>,
    }
    let mut written: Vec<(PathBuf, String)> = Vec::new();
    let mut probes: Vec<Probe> = Vec::new();
    for case in cases {
        let base = written.len();
        for (name, src) in &case.files {
            let path = temp_fs_file(name, src);
            written.push((path, src.clone()));
        }
        probes.push(Probe {
            label: case.label.clone(),
            ref_global: base + case.ref_local,
            sites: case.sites.clone(),
        });
    }

    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project(&paths);
    let fcs = parse_fcs_uses_project(&json, &written);
    let blocked_ranges = accessibility_error_ranges(&json, &written);

    let asts: Vec<ImplFile> = written.iter().map(|(_, s)| impl_file(s)).collect();
    let proj = resolve_project(&asts, &AssemblyEnv::default());

    for (p, _) in &written {
        let _ = std::fs::remove_file(p);
    }

    let mut divergences = Vec::new();
    let mut tally = Tally::default();
    for probe in &probes {
        let path = &written[probe.ref_global].0;
        let empty = Vec::new();
        let blocked_here = blocked_ranges
            .get(&PathBuf::from(path.file_name().unwrap()))
            .unwrap_or(&empty);
        for (site, range) in &probe.sites {
            let label = format!("{}/{site}", probe.label);

            // Our answer, normalised to (declaring file name, def range).
            let ours = proj.file(probe.ref_global).resolution_at(*range);
            let our_target: Option<(PathBuf, TextRange)> = match ours {
                Some(Resolution::Item(_)) => proj
                    .item_def(ours.unwrap())
                    .map(|(idx, def)| (written[idx].0.clone(), def.range)),
                Some(Resolution::Local(_)) => proj
                    .file(probe.ref_global)
                    .resolved_def(ours.unwrap())
                    .map(|def| (path.clone(), def.range)),
                _ => None, // Deferred / Entity / Member / Unresolved / unrecorded
            };

            // A use-site accessibility error overlapping this reference (half-open):
            // FCS has no accessible resolution here, so the only correct answer is to
            // defer — committing a target is a wrong go-to-def.
            let start = usize::from(range.start());
            let end = usize::from(range.end());
            let blocked = blocked_here.iter().any(|&(bs, be)| bs < end && start < be);
            if blocked {
                tally.blocked += 1;
                if let Some((f, r)) = our_target {
                    divergences.push(Divergence {
                        label,
                        fcs: "inaccessible (FS1092/1094/491)".to_string(),
                        ours: format!("{:?} @{:?}", f.file_name(), r),
                    });
                }
                continue;
            }

            // FCS's answer at this reference site (the whole path span).
            let fcs_decl = fcs
                .iter()
                .find(|f| f.path.file_name() == path.file_name())
                .and_then(|f| f.uses.iter().find(|u| u.start == start && u.end == end))
                .and_then(|u| u.decl.as_ref());

            let bump = |t: &mut Tally| {
                if *site == "pat" {
                    t.pat += 1;
                } else {
                    t.expr += 1;
                }
            };
            match (fcs_decl, our_target) {
                (Some(decl), Some((our_file, our_range))) => {
                    bump(&mut tally);
                    let agree = our_file.file_name() == decl.file.file_name()
                        && our_range == span(decl.start, decl.end);
                    if !agree {
                        divergences.push(Divergence {
                            label,
                            fcs: format!(
                                "{:?} @{}..{}",
                                decl.file.file_name(),
                                decl.start,
                                decl.end
                            ),
                            ours: format!("{:?} @{:?}", our_file.file_name(), our_range),
                        });
                    } else if decl.file.file_name() != path.file_name() {
                        // Agreed on a decl in a *different* file — the earlier-export
                        // cross-file path is genuinely exercised.
                        tally.cross_file += 1;
                    }
                }
                (Some(_), None) => bump(&mut tally), // FCS resolved, we deferred — allowed.
                (None, Some((our_file, our_range))) => divergences.push(Divergence {
                    label,
                    fcs: "unbound".to_string(),
                    ours: format!("{:?} @{:?}", our_file.file_name(), our_range),
                }),
                (None, None) => {} // both decline — fine.
            }
        }
    }
    (divergences, tally)
}

/// Every `(NearFoo, NearMod, outer)` combination — the enumerated same-file
/// qualified-path space.
fn enumerate() -> Vec<Scenario> {
    const FOOS: [NearFoo; 8] = [
        NearFoo::UnionPub,
        NearFoo::UnionPriv,
        NearFoo::StaticPub,
        NearFoo::StaticPriv,
        NearFoo::CompanionPub,
        NearFoo::CompanionPriv,
        NearFoo::UnionPrivCompanion,
        NearFoo::Absent,
    ];
    let mut out = Vec::new();
    for near_foo in FOOS {
        for near_mod in [NearMod::Pub, NearMod::Priv] {
            for outer in [false, true] {
                let label = format!(
                    "{}/{}/{}",
                    near_foo.tag(),
                    near_mod.tag(),
                    if outer { "outer" } else { "noOuter" }
                );
                out.push(Scenario {
                    near_foo,
                    near_mod,
                    outer,
                    label,
                });
            }
        }
    }
    out
}

#[test]
fn qualified_path_accessibility_agrees_with_fcs() {
    let scenarios = enumerate();
    // The single-file sweep (findings 1-3: sole-inaccessible over-bind, walk to a
    // farther same-file candidate, companion-module win) and the earlier-export sweep
    // (finding 4: an inaccessible companion value must not mis-bind an earlier file's
    // `A.Foo.Red` export past a farther same-file target).
    let mut cases: Vec<Case> = scenarios
        .iter()
        .enumerate()
        .map(|(gi, sc)| single_file_case(gi, sc))
        .collect();
    cases.extend(
        scenarios
            .iter()
            .enumerate()
            .map(|(gi, sc)| earlier_export_case(gi, sc)),
    );
    // The headerless sweep (finding 5: an accessible companion value with no provable
    // `qualified` path must not be stepped over onto a farther candidate).
    cases.extend(
        scenarios
            .iter()
            .enumerate()
            .map(|(gi, sc)| headerless_case(gi, sc)),
    );

    let mut all_divergences: Vec<Divergence> = Vec::new();
    let mut tally = Tally::default();
    for chunk in cases.chunks(24) {
        let (divs, t) = diff_cases(chunk);
        all_divergences.extend(divs);
        tally.expr += t.expr;
        tally.pat += t.pat;
        tally.blocked += t.blocked;
        tally.cross_file += t.cross_file;
    }

    // Non-vacuity: the sweep must genuinely resolve expression and pattern sites,
    // genuinely rule some sites inaccessible, and genuinely resolve some references
    // across files (the earlier-export path) — else that arm of the oracle checks
    // nothing.
    assert!(
        tally.expr > 0,
        "vacuous: FCS resolved no expression `A.Foo.Red`"
    );
    assert!(
        tally.pat > 0,
        "vacuous: FCS resolved no pattern `A.Foo.Red` — the constructor namespace was not exercised"
    );
    assert!(
        tally.blocked > 0,
        "vacuous: FCS ruled no site inaccessible — the accessibility oracle was not exercised"
    );
    assert!(
        tally.cross_file > 0,
        "vacuous: no reference resolved across files — the earlier-export sweep was not exercised"
    );
    assert!(
        all_divergences.is_empty(),
        "{} qualified-path accessibility divergence(s) vs FCS:\n{}",
        all_divergences.len(),
        all_divergences
            .iter()
            .map(|d| format!("  [{}] FCS {} | we gave {}", d.label, d.fcs, d.ours))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
