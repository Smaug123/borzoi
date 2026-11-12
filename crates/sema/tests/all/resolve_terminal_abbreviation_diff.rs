//! A type-position name that ends on an assembly abbreviation whose target is
//! structural — FSharp.Core's `'T array = 'T[]`, a tuple, a function — against
//! FCS.
//!
//! Nothing walks past a terminal name, so where the target leads is not the
//! question: FCS binds the abbreviation itself and reports the use as it
//! (`Microsoft.FSharp.Core.array`). A path that continues past one still has
//! to chase it, and keeps declining.
//!
//! The grid crosses the ways a same-named type can contest the name — a
//! project type, an opened project module's type, an opened assembly
//! namespace's structural or nominal abbreviation — with type-position uses
//! (postfix, prefix, fully qualified, a nested module's abbreviation), and
//! asserts certain-implies-exact at every use FCS reports on a program it
//! accepts: we bind FCS's declaration or decline. A control set must commit,
//! so the grid cannot pass by declining everything; and the paths that walk
//! past such an abbreviation, which FCS rejects, must commit nothing there.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::common::{
    ensure_fsharp_core_dll, ensure_system_runtime_dll, invoke_fcs_dump_project_with_refs,
    parse_fcs_uses_project, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_oracle_harness::BoundedCommand;
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, resolve_file};
use rowan::TextRange;

pub(crate) fn fixture_dll() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let project =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/abbrev_structural_env");
        let mut cmd = std::process::Command::new("dotnet");
        cmd.args(["build", "-c", "Release", "--nologo"])
            .arg(&project);
        BoundedCommand::new(cmd)
            .timeout(std::time::Duration::from_secs(1800))
            .run_ok("dotnet build structural-abbreviation fixture");
        project.join("bin/Release/net10.0/SemaAbbrevStructuralFixture.dll")
    })
}

pub(crate) fn env() -> &'static AssemblyEnv {
    static ENV: OnceLock<AssemblyEnv> = OnceLock::new();
    ENV.get_or_init(|| {
        let sysrt_path = ensure_system_runtime_dll();
        let netstd_path = sysrt_path
            .parent()
            .expect("ref dir")
            .join("netstandard.dll");
        let bytes = [
            std::fs::read(ensure_fsharp_core_dll()).expect("read FSharp.Core.dll"),
            std::fs::read(&sysrt_path).expect("read System.Runtime.dll"),
            std::fs::read(&netstd_path).expect("read netstandard.dll"),
            std::fs::read(fixture_dll()).expect("read fixture dll"),
        ];
        let views: Vec<_> = bytes
            .iter()
            .map(|b| Ecma335Assembly::parse(b).expect("parse dll"))
            .collect();
        AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
    })
}

/// What may contest the name, declared before the use in `module M`.
const CONTEXTS: &[(&str, &str)] = &[
    ("none", ""),
    ("project-type", "type array<'T> = { V : 'T }\n"),
    (
        "opened-project-module",
        "module X =\n    type array<'T> = { V : 'T }\nopen X\n",
    ),
    ("opened-structural", "open AbbrevStruct\n"),
    ("opened-nominal", "open AbbrevNominal\n"),
];

/// Type-position uses, each a module-level declaration.
const USES: &[(&str, &str)] = &[
    ("postfix", "let f (x : int array) = ()\n"),
    ("prefix", "let f (x : array<int>) = ()\n"),
    (
        "return",
        "let f () : string array = Unchecked.defaultof<_>\n",
    ),
    (
        "fully-qualified",
        "let f (x : int Microsoft.FSharp.Core.array) = ()\n",
    ),
    (
        "fixture-qualified",
        "let f (x : int AbbrevStruct.array) = ()\n",
    ),
    (
        "function-target",
        "let f (x : AbbrevStruct.endo<int>) = ()\n",
    ),
    ("nested", "let f (x : AbbrevStruct.Holder.pair<int>) = ()\n"),
];

/// The cells that must commit: no contest, or one FCS settles in favour of a
/// structural abbreviation we can see.
const MUST_COMMIT: &[(&str, &str)] = &[
    ("none", "postfix"),
    ("none", "prefix"),
    ("none", "return"),
    ("none", "fully-qualified"),
    ("none", "fixture-qualified"),
    ("none", "function-target"),
    ("none", "nested"),
];

/// The abbreviation names the uses spell; a use FCS reports at any of them is
/// graded.
const NAMES: &[&str] = &["array", "endo", "pair"];

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

fn resolve(src: &str) -> borzoi_sema::ResolvedFile {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{src}\n{:?}", parsed.errors);
    let recovery = SyntaxRecovery::of(&parsed);
    resolve_file(
        &ImplFile::cast(parsed.root).unwrap(),
        &ProjectItems::default(),
        env(),
        &recovery,
    )
}

/// The `(assembly, full name)` an entity resolution names, enclosing
/// entities included (FCS's `FullName`).
fn entity_identity(h: borzoi_sema::EntityHandle) -> (String, String) {
    (
        env().entity(h).assembly.name.clone(),
        env().entity_full_name(h),
    )
}

#[test]
fn a_terminal_structural_abbreviation_binds_as_fcs_does() {
    let mut wrong = Vec::new();
    let mut declined_controls = Vec::new();
    let mut graded = 0usize;
    let mut rejected = Vec::new();
    for (ctx, decl) in CONTEXTS {
        for (use_label, u) in USES {
            let src = format!("module M\n\n{decl}\n{u}");
            let path = temp_fs_file("terminal_abbrev", &src);
            let json = invoke_fcs_dump_project_with_refs(&[path.as_path()], &[fixture_dll()]);
            let files = parse_fcs_uses_project(&json, &[(path.clone(), src.clone())]);
            let _ = std::fs::remove_file(&path);
            let file = files.into_iter().next().expect("one file");
            if !file.errors.is_empty() {
                rejected.push(format!("{ctx}/{use_label}: {:?}", file.errors));
                continue;
            }
            let rf = resolve(&src);
            let mut committed = false;
            for u in &file.uses {
                // FCS spans a qualified type use whole (`Microsoft.FSharp.Core.array`);
                // the resolver records the type at its last segment.
                let last = src[u.start..u.end].rsplit('.').next().unwrap_or("");
                if u.is_from_definition || !NAMES.contains(&last) {
                    continue;
                }
                let at = span(u.end - last.len(), u.end);
                let res = rf.resolution_at(at);
                let verdict = match (res, &u.decl) {
                    (None | Some(Resolution::Deferred(_)), _) => None,
                    (Some(r @ (Resolution::Local(_) | Resolution::Item(_))), Some(d)) => {
                        let ours = rf.resolved_def(r).map(|d| d.range);
                        Some(ours == Some(span(d.start, d.end)))
                    }
                    (Some(Resolution::Entity(h)), None) => {
                        let fcs = (u.assembly.clone(), u.full_name.clone());
                        let (asm, full) = entity_identity(h);
                        Some(fcs == (Some(asm), Some(full)))
                    }
                    _ => Some(false),
                };
                match verdict {
                    None => {}
                    Some(true) => {
                        graded += 1;
                        committed = true;
                    }
                    Some(false) => wrong.push(format!(
                        "{ctx}/{use_label}: `{}` at {at:?} resolved {res:?}; FCS: {:?} {:?} decl {:?}\n{src}",
                        &src[u.start..u.end],
                        u.assembly,
                        u.full_name,
                        u.decl
                    )),
                }
            }
            if MUST_COMMIT.contains(&(ctx, use_label)) && !committed {
                declined_controls.push(format!("{ctx}/{use_label}\n{src}"));
            }
        }
    }
    assert!(wrong.is_empty(), "wrong targets:\n{}", wrong.join("\n"));
    assert!(
        declined_controls.is_empty(),
        "control cells that must commit declined:\n{}",
        declined_controls.join("\n")
    );
    assert!(
        rejected.is_empty(),
        "every grid cell is valid F#; FCS rejected:\n{}",
        rejected.join("\n")
    );
    assert!(graded >= MUST_COMMIT.len(), "graded only {graded}");
}

/// A path that continues past a structural abbreviation has nothing to walk
/// on — FCS rejects it — so the abbreviation is not committed there, nor any
/// segment after it.
#[test]
fn a_path_through_a_structural_abbreviation_commits_nothing() {
    for (src, alias) in [
        (
            "module M\nlet f (x : AbbrevStruct.array.Inner) = ()\n",
            "array",
        ),
        (
            "module M\nlet f (x : AbbrevStruct.Holder.pair.Inner) = ()\n",
            "pair",
        ),
        (
            "module M\nlet f (x : Microsoft.FSharp.Core.array.Inner) = ()\n",
            "array",
        ),
    ] {
        let rf = resolve(src);
        let start = src.find(alias).unwrap();
        for (range, res) in rf.resolutions() {
            if usize::from(range.start()) >= start {
                assert!(
                    !matches!(res, Resolution::Entity(_)),
                    "{src}: committed {res:?} at {range:?}"
                );
            }
        }
    }
}
