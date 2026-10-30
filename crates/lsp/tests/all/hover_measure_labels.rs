//! The **unit-of-measure** hover differential: the signature hover shows for a
//! referenced F# member, against the type FCS gives an access of it.
//!
//! A hover on a referenced-assembly member renders the member's signature from
//! its IL, and IL erases units of measure: a member declared `float<m>` has a
//! `float64` signature, so a label read off it says `float` where FCS says
//! `float<m>`. That is a wrong type shown to the user, and it passes every
//! check that compares names.
//!
//! The universe is the sema crate's measure corpus
//! (`crates/sema/tests/all/common/measure_corpus.rs`): every shape a member can
//! be declared with × every carrier a member can sit on, type members and module
//! bindings alike, each on its own declaring type, plus the cells outside that
//! product. Each cell is hovered on its member name through the real hover
//! handler, against a "restored" project referencing the built fixture, and the
//! label is graded against FCS's type of the same access (`fcs-dump types`).
//!
//! # The properties
//!
//! 1. **Soundness.** A label that shows a type shows FCS's: the access's type
//!    (the whole function for an unapplied access, the result for an applied
//!    one), and for an applied access the argument's type as the parameter's.
//!    The label's types are read back from its text into FCS's canonical
//!    currency by a small table here, which knows only the fixture's
//!    vocabulary and fails loudly on anything else, so the comparison does not
//!    lean on how the label was produced.
//! 2. **Identity survives.** Every shape × carrier cell still shows a member
//!    label naming its member: declining the type must not cost the hover.
//! 3. **Non-vacuity.** Every measure-free control shows a type, so the
//!    properties above are not satisfied by a label that never shows one; and
//!    the shipped FSharp.Core's everyday modules keep theirs, since a module's
//!    verdict covers all of its bindings at once.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[path = "../../../sema/tests/all/common/measure_corpus.rs"]
#[allow(dead_code)]
mod measure_corpus;

use borzoi::handlers::hover;
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::server::State;
use borzoi::uri::UriExt as _;
use borzoi::workspace::Workspace;
use borzoi_spawn::BoundedCommand;
use lsp_types::{
    HoverContents, HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
    WorkDoneProgressParams,
};
use measure_corpus::{Corpus, Expect, Site, corpus};
use serde::Deserialize;
use tempfile::TempDir;

use crate::common::{
    LineIndex, ensure_fsharp_core_dll, ensure_system_runtime_dll, fcs_dump_command,
};

/// The measure fixture, built once per test binary into a directory of its own
/// (never the sema crate's checkout copy, which that crate's tests build
/// concurrently).
fn fixture_dll() -> &'static Path {
    static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    let (_, dll) = BUILT.get_or_init(|| {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../sema/tests/fixtures/measure_env/SemaMeasureFixture.fsproj");
        let build = TempDir::new().unwrap();
        fs::copy(&source, build.path().join("SemaMeasureFixture.fsproj")).unwrap();
        fs::write(
            build.path().join("Generated.fs"),
            measure_corpus::fixture_source(),
        )
        .unwrap();
        let out = build.path().join("out");
        let mut cmd = std::process::Command::new("dotnet");
        cmd.args(["build", "-c", "Release", "--nologo", "-o"])
            .arg(&out)
            .arg(build.path().join("SemaMeasureFixture.fsproj"));
        BoundedCommand::new(cmd)
            .timeout(std::time::Duration::from_secs(1800))
            .run_ok("dotnet build measure fixture");
        let dll = out.join(format!("{}.dll", measure_corpus::FIXTURE_ASM));
        (build, dll)
    });
    dll
}

/// A "restored" project compiling `src` against the framework's
/// `System.Runtime` and `netstandard` (through which FSharp.Core's pickle names
/// its BCL abbreviation targets), FSharp.Core, and the fixture — the last two as
/// NuGet packages' compile assets.
fn project(src: &str) -> (State, Uri, TempDir) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let dotnet_root = root.join("dotnet");
    let pack = dotnet_root.join("packs/Microsoft.NETCore.App.Ref/10.0.0/ref/net10.0");
    fs::create_dir_all(&pack).unwrap();
    let runtime = ensure_system_runtime_dll();
    let ref_dir = runtime
        .parent()
        .expect("System.Runtime.dll sits in a ref dir");
    for dll in ["System.Runtime.dll", "netstandard.dll"] {
        fs::copy(ref_dir.join(dll), pack.join(dll)).unwrap();
    }

    let pkgs = root.join("pkgs");
    let mut target = serde_json::Map::new();
    let mut libraries = serde_json::Map::new();
    for (id, dll) in [
        ("FSharp.Core", ensure_fsharp_core_dll()),
        (measure_corpus::FIXTURE_ASM, fixture_dll().to_path_buf()),
    ] {
        let folder = format!("{}/10.0.0", id.to_lowercase());
        let lib = pkgs.join(&folder).join("lib/netstandard2.1");
        fs::create_dir_all(&lib).unwrap();
        let file = dll.file_name().unwrap().to_str().unwrap().to_owned();
        fs::copy(&dll, lib.join(&file)).unwrap();
        let key = format!("{id}/10.0.0");
        target.insert(
            key.clone(),
            serde_json::json!({
                "type": "package",
                "compile": { format!("lib/netstandard2.1/{file}"): {} }
            }),
        );
        libraries.insert(
            key,
            serde_json::json!({ "type": "package", "path": folder }),
        );
    }
    let assets = serde_json::json!({
        "version": 3,
        "targets": { "net10.0": target },
        "libraries": libraries,
        "packageFolders": { pkgs.to_str().unwrap(): {} },
        "project": { "frameworks": { "net10.0": {
            "frameworkReferences": { "Microsoft.NETCore.App": {} }
        } } }
    });
    fs::create_dir_all(root.join("obj")).unwrap();
    fs::write(root.join("obj/project.assets.json"), assets.to_string()).unwrap();
    fs::write(
        root.join("P.fsproj"),
        r#"<Project><ItemGroup><Compile Include="Probe.fs" /></ItemGroup></Project>"#,
    )
    .unwrap();
    let src_path = root.join("Probe.fs");
    fs::write(&src_path, src).unwrap();

    let mut state = State::default();
    state.workspace = Workspace::with_env(SdkDiscoveryEnv {
        dotnet_root: Some(dotnet_root),
        ..SdkDiscoveryEnv::default()
    });
    let uri = Uri::from_file_path(&src_path).unwrap();
    state.docs.insert(uri.clone(), src.to_string());
    (state, uri, tmp)
}

#[derive(Deserialize)]
struct TypesDump {
    #[serde(rename = "Exprs")]
    exprs: Vec<RawExpr>,
    #[serde(rename = "Errors")]
    errors: Vec<RawError>,
    /// Declarations fcs-dump could not convert, and so reported no node in.
    /// The probe reads a missing node as FCS's answer, so it must have none.
    #[serde(rename = "Unconverted")]
    unconverted: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawExpr {
    #[serde(rename = "Range")]
    range: RawRange,
    #[serde(rename = "TypeCanon")]
    type_canon: String,
}

#[derive(Deserialize)]
struct RawRange {
    #[serde(rename = "Start")]
    start: RawPos,
    #[serde(rename = "End")]
    end: RawPos,
}

#[derive(Deserialize)]
struct RawPos {
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Col")]
    col: u32,
}

#[derive(Deserialize)]
struct RawError {
    #[serde(rename = "Line")]
    line: u32,
}

/// A half-open byte range into the probe.
type Span = (usize, usize);

/// FCS's canonical type at every expression range of `probe`, and the 1-based
/// lines it reported an error on.
fn fcs_types(probe: &str) -> (HashMap<Span, String>, Vec<usize>) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("MeasureProbe.fs");
    fs::write(&path, probe).unwrap();
    let mut cmd = fcs_dump_command("types");
    cmd.arg(&path)
        .env("BORZOI_FCS_EXTRA_REFS", fixture_dll())
        .env_remove("BORZOI_FCS_EXCLUSIVE_REFS");
    let out = BoundedCommand::new(cmd).run_ok("fcs-dump types (measure probe)");
    let json = String::from_utf8(out.stdout).expect("fcs-dump stdout is UTF-8");
    let dump: TypesDump = serde_json::from_str(&json).expect("fcs-dump types JSON shape");
    assert!(
        dump.unconverted.is_empty(),
        "fcs-dump could not convert part of the measure probe: {:?}",
        dump.unconverted
    );
    let idx = LineIndex::new(probe);
    let types = dump
        .exprs
        .into_iter()
        .map(|e| {
            let start = idx.offset(e.range.start.line, e.range.start.col);
            let end = idx.offset(e.range.end.line, e.range.end.col);
            ((start, end), e.type_canon)
        })
        .collect();
    let errors = dump.errors.into_iter().map(|e| e.line as usize).collect();
    (types, errors)
}

/// The member a site's access names: the last dotted segment before any
/// application (`v3.M1(1)` → `M1`, `C1S2.F1 1` → `F1`), and whether the access
/// applies it.
fn member_name(access: &str) -> (&str, bool) {
    let head_end = access.find(['(', ' ']).unwrap_or(access.len());
    let head = &access[..head_end];
    let name = head.rsplit('.').next().expect("a dotted access");
    (name, head_end < access.len())
}

/// The hover's head line with its code-span fence removed — the label itself.
fn label_head(body: &str) -> Option<&str> {
    let first = body.split("\n\n").next()?;
    let inner = first.trim_matches('`');
    (inner.len() + 2 <= first.len()).then_some(inner.trim())
}

/// The member keyword families a member label opens with.
const MEMBER_KEYWORDS: &[&str] = &[
    "member ",
    "static member ",
    "abstract member ",
    "static abstract member ",
    "val ",
    "val mutable ",
    "static val ",
];

/// Whether `head` is a member label for `name`, and if so the type it claims,
/// as written after the name (`None` for a label that claims none).
fn parse_label<'a>(head: &'a str, name: &str) -> Option<Option<&'a str>> {
    let rest = MEMBER_KEYWORDS
        .iter()
        .filter_map(|k| head.strip_prefix(k))
        .min_by_key(|r| r.len())?;
    let rest = rest.strip_prefix("mutable ").unwrap_or(rest);
    let after = rest.strip_prefix(name)?;
    let after = [" with get, set", " with get", " with set"]
        .iter()
        .find_map(|s| after.strip_suffix(s))
        .unwrap_or(after);
    if after.is_empty() {
        return Some(None);
    }
    Some(Some(after.strip_prefix(": ")?))
}

/// One display-form type atom of the fixture's vocabulary, in FCS's canonical
/// currency. `None` for anything else, which the caller reports rather than
/// guessing at.
fn canon_atom(atom: &str) -> Option<String> {
    if let Some(elem) = atom.strip_suffix("[]") {
        return canon_atom(elem).map(|e| format!("{e}[]"));
    }
    let canon = match atom {
        "float" => "System.Double",
        "float32" => "System.Single",
        "decimal" => "System.Decimal",
        "int" => "System.Int32",
        "int64" => "System.Int64",
        "int16" => "System.Int16",
        "sbyte" => "System.SByte",
        "byte" => "System.Byte",
        "uint64" => "System.UInt64",
        "string" => "System.String",
        "unit" => "Microsoft.FSharp.Core.Unit",
        "Vec" => "FSharpMeasure.Vec",
        _ => return None,
    };
    Some(canon.to_owned())
}

/// One arrow-separated piece of a label's type in canonical currency, its
/// parameter `name: ` dropped. A tupled domain does not occur in the fixture and
/// is refused.
fn canon_piece(piece: &str, label_ty: &str) -> Result<String, String> {
    let atom = match piece.split_once(": ") {
        Some((_, ty)) => ty,
        None => piece,
    };
    if atom.contains(" * ") {
        return Err(format!("tupled domain `{atom}` in `{label_ty}`"));
    }
    canon_atom(atom)
        .ok_or_else(|| format!("label type `{atom}` (in `{label_ty}`) is outside the vocabulary"))
}

/// The argument of an applied access, as a byte range into the access, or
/// `None` for a unit argument: `M1(1)` → `1`, `F1 1` → `1`, `F ()` → `None`.
fn argument_range(access: &str) -> Option<Span> {
    let open = access.find(['(', ' '])?;
    let (start, end) = if access[open..].starts_with('(') {
        (open + 1, access.rfind(')').expect("a closed argument"))
    } else {
        (open + 1, access.len())
    };
    let arg = access[start..end].trim();
    (!arg.is_empty() && arg != "()").then_some((start, end))
}

/// Every type a label claims, keyed by the probe range FCS types it at: the
/// access itself — the whole signature when unapplied (`x: float -> int` →
/// `System.Double -> System.Int32`), the result when applied — and, for an
/// applied access, its argument as the parameter's type.
fn claims(
    label_ty: &str,
    site_access: Span,
    access: &str,
    applied: bool,
) -> Result<Vec<(Span, String)>, String> {
    let pieces: Vec<&str> = label_ty.split(" -> ").collect();
    if !applied {
        let canon = pieces
            .iter()
            .map(|p| canon_piece(p, label_ty))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(vec![(site_access, canon.join(" -> "))]);
    }
    let Some((ret, domain)) = pieces.split_last().filter(|(_, d)| !d.is_empty()) else {
        return Err(format!(
            "an applied access's label `{label_ty}` has no arrow"
        ));
    };
    let mut out = vec![(site_access, canon_piece(ret, label_ty)?)];
    if let Some((start, end)) = argument_range(access) {
        let [param] = domain else {
            return Err(format!("`{label_ty}` takes {} arguments", domain.len()));
        };
        out.push((
            (site_access.0 + start, site_access.0 + end),
            canon_piece(param, label_ty)?,
        ));
    }
    Ok(out)
}

struct Run {
    corpus: Corpus,
    /// Per site, the hover body at its member name (`None`: no hover).
    hovers: Vec<Option<String>>,
    fcs: HashMap<Span, String>,
    error_lines: Vec<usize>,
}

fn run() -> &'static Run {
    static RUN: OnceLock<Run> = OnceLock::new();
    RUN.get_or_init(|| {
        let corpus = corpus();
        let (mut state, uri, _tmp) = project(&corpus.probe);
        let hovers = corpus
            .sites
            .iter()
            .map(|site| {
                let access = &corpus.probe[site.access.0..site.access.1];
                let (name, _) = member_name(access);
                let head_end = access.find(['(', ' ']).unwrap_or(access.len());
                let at = site.access.0 + head_end - name.len();
                let line_start = corpus.probe[..at].rfind('\n').map_or(0, |i| i + 1);
                let params = HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: Position {
                            line: (site.line - 1) as u32,
                            character: (at - line_start) as u32,
                        },
                    },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                };
                hover::handle(&mut state, params).map(|h| match h.contents {
                    HoverContents::Markup(m) => m.value,
                    other => panic!("expected markup, got {other:?}"),
                })
            })
            .collect();
        let (fcs, error_lines) = fcs_types(&corpus.probe);
        Run {
            corpus,
            hovers,
            fcs,
            error_lines,
        }
    })
}

impl Run {
    /// The site's label, parsed: `Some(claimed)` when the hover is a member
    /// label for the site's member.
    fn label(&self, i: usize) -> Option<Option<&str>> {
        let site = &self.corpus.sites[i];
        let access = &self.corpus.probe[site.access.0..site.access.1];
        let (name, _) = member_name(access);
        let head = label_head(self.hovers[i].as_deref()?)?;
        parse_label(head, name)
    }

    fn accepted(&self, site: &Site) -> bool {
        matches!(site.expect, Expect::Accepts { .. }) && !self.error_lines.contains(&site.line)
    }
}

/// Property 1: a label that shows a type shows FCS's.
#[test]
fn no_member_label_shows_a_type_fcs_contradicts() {
    let run = run();
    let mut wrong = Vec::new();
    for (i, site) in run.corpus.sites.iter().enumerate() {
        let Some(Some(label_ty)) = run.label(i) else {
            continue;
        };
        let access = &run.corpus.probe[site.access.0..site.access.1];
        let (_, applied) = member_name(access);
        if !run.accepted(site) {
            wrong.push(format!(
                "  {}: `{access}` hovers `{label_ty}`, and FCS rejects the line",
                site.label
            ));
            continue;
        }
        match claims(label_ty, site.access, access, applied) {
            Ok(claims) => {
                for (range, ours) in claims {
                    let fcs = run.fcs.get(&range);
                    if fcs != Some(&ours) {
                        wrong.push(format!(
                            "  {}: `{access}` hovers `{label_ty}`, claiming `{ours}` for \
                             `{}`; FCS: {fcs:?}",
                            site.label,
                            &run.corpus.probe[range.0..range.1]
                        ));
                    }
                }
            }
            Err(why) => wrong.push(format!("  {}: `{access}`: {why}", site.label)),
        }
    }
    assert!(
        wrong.is_empty(),
        "{} member label(s) show a type FCS does not confirm:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// Property 2: every shape × carrier cell still hovers as its member.
#[test]
fn every_cell_hovers_as_its_member() {
    let run = run();
    let mut missing = Vec::new();
    for (i, site) in run.corpus.sites.iter().enumerate() {
        if site.erased.is_none() {
            continue;
        }
        if run.label(i).is_none() {
            missing.push(format!(
                "  {}: hover {:?}",
                site.label,
                run.hovers[i].as_deref()
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "{} cell(s) did not hover as their member:\n{}",
        missing.len(),
        missing.join("\n")
    );
}

/// Property 3: every measure-free control shows its type.
#[test]
fn every_measure_free_control_shows_its_type() {
    let run = run();
    let mut untyped = Vec::new();
    let mut controls = 0;
    for (i, site) in run.corpus.sites.iter().enumerate() {
        if !site.is_control {
            continue;
        }
        controls += 1;
        if !matches!(run.label(i), Some(Some(_))) {
            untyped.push(format!(
                "  {}: hover {:?}",
                site.label,
                run.hovers[i].as_deref()
            ));
        }
    }
    assert!(controls > 0, "the corpus has no measure-free controls");
    assert!(
        untyped.is_empty(),
        "{} of {controls} measure-free control(s) show no type, so the labels beside \
         them prove nothing by showing none:\n{}",
        untyped.len(),
        untyped.join("\n")
    );
}

/// The shipped FSharp.Core's everyday modules keep typed hovers. A module's
/// verdict covers every binding it declares, so one measure-typed binding would
/// withhold the types of all of them; these are measure-free, and losing their
/// signatures (to a reading that stopped stamping modules, say) would be a
/// regression no fixture cell shows.
#[test]
fn fsharp_core_everyday_modules_keep_typed_hovers() {
    use borzoi_assembly::Ecma335Assembly;
    let runtime = ensure_system_runtime_dll();
    let netstandard = runtime
        .parent()
        .expect("System.Runtime.dll sits in a ref dir")
        .join("netstandard.dll");
    let bytes: Vec<Vec<u8>> = [runtime.clone(), netstandard, ensure_fsharp_core_dll()]
        .iter()
        .map(|p| fs::read(p).unwrap())
        .collect();
    let views: Vec<_> = bytes
        .iter()
        .map(|b| Ecma335Assembly::parse(b).unwrap())
        .collect();
    let env = borzoi_sema::AssemblyEnv::from_views(&views).unwrap();
    // Source names: `List` is the module `ListModule`.
    let modules = [
        "Microsoft.FSharp.Core.Operators",
        "Microsoft.FSharp.Core.ExtraTopLevelOperators",
        "Microsoft.FSharp.Core.Option",
        "Microsoft.FSharp.Core.String",
        "Microsoft.FSharp.Collections.List",
        "Microsoft.FSharp.Collections.Array",
        "Microsoft.FSharp.Collections.Seq",
        "Microsoft.FSharp.Collections.Map",
        "Microsoft.FSharp.Collections.Set",
    ];
    let by_name: HashMap<String, borzoi_sema::EntityHandle> = env
        .all_handles()
        .filter(|&h| env.entity(h).assembly.name == "FSharp.Core" && env.is_module(h))
        .map(|h| (env.entity_full_name(h), h))
        .collect();
    let untyped: Vec<String> = modules
        .iter()
        .filter_map(|name| {
            let handle = *by_name
                .get(*name)
                .unwrap_or_else(|| panic!("FSharp.Core declares the module {name}"));
            (!env.member_types_keep_their_measures(handle))
                .then(|| format!("  {name}: {:?}", env.entity(handle).member_measures))
        })
        .collect();
    assert!(
        untyped.is_empty(),
        "{} FSharp.Core module(s) withhold every member type:\n{}",
        untyped.len(),
        untyped.join("\n")
    );
}

/// The per-cell table — a measurement, not a gate.
#[test]
#[ignore = "report generator"]
fn label_report() {
    let run = run();
    for (i, site) in run.corpus.sites.iter().enumerate() {
        let head = run.hovers[i].as_deref().and_then(label_head).unwrap_or("—");
        let fcs = run.fcs.get(&site.access).map_or("—", String::as_str);
        println!("{:<64} label={:<40} fcs={fcs}", site.label, head);
    }
}
