//! The **union `[<RequireQualifiedAccess>]` grid**: whether a union's cases
//! enter bare scope, graded against FCS over every way of writing (or
//! appearing to write) the attribute.
//!
//! FCS holds a union's cases qualified-only exactly when one of its attributes
//! *resolves* to FSharp.Core's `Microsoft.FSharp.Core.RequireQualifiedAccessAttribute`
//! (`TypedTreeOps.Attributes.fs`, `resolveAttribPath`: a non-local reference
//! into FSharp.Core's CCU at that path; read by
//! `AddStaticPartsOfTyconRefToNameEnv`). A written name is evidence of that
//! only while nothing else can be what the name resolves to, so the grid
//! crosses:
//!
//! - **spelling** ([`Spelling`]): short, long, qualified through
//!   `Microsoft.FSharp.Core` or `FSharp.Core`, with a target, through an
//!   abbreviation, through a module holding an impersonator, an unrelated
//!   attribute, none;
//! - **environment** ([`Env`]): what else the union's container can see — a
//!   type named like the attribute in scope or out of it (declared here or in
//!   an earlier file), one named like the short spelling, an `exception` named
//!   like it, an abbreviation renaming the name to another attribute;
//! - **location** ([`Location`]): the union in the probe's own module, in a
//!   nested module the probe opens, in an earlier file's module (with and
//!   without a signature) or namespace, or in a referenced assembly;
//! - **use** ([`Use`]): after an `open` of the union's container, the case's
//!   bare name in an expression and in a pattern, and `GridU.GridC`; without
//!   one, `GridLib.GridC` and `GridLib.GridU.GridC`.
//!
//! An earlier union in the same container declares a same-named case, so each
//! reading binds something different: the union's case when its cases are
//! bare, the earlier case when they are not.
//!
//! The property is certain-implies-exact: wherever we commit, FCS reports
//! that very symbol. The control cells — FSharp.Core's attribute written
//! plainly, or no attribute at all, with nothing to impersonate it — must
//! commit, so the grid cannot pass by declining. Every cell's two answers are
//! pinned in `tests/manifests/union_rqa_attribute_grid.txt`.

use std::path::{Path, PathBuf};

use crate::common::corpus_manifest::{check_manifest, regenerate};
use crate::common::{
    ensure_fsharp_core_dll, ensure_rqa_collision_fixture_built,
    ensure_rqa_double_suffix_fixture_built, ensure_rqa_union_fixture_built,
    invoke_fcs_dump_project_with_refs, parse_fcs_uses_project, temp_fs_tree,
    unmodelled_project_binder_failures,
};
use crate::resolve_signatures::project;
use crate::unmodelled_contributor_grid::{agrees, env_of, fcs_at, render_fcs};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_sema::{AssemblyEnv, Resolution, resolve_project_files};
use rowan::TextRange;

/// How the union's attribute is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spelling {
    /// No attribute.
    Absent,
    /// An unrelated FSharp.Core attribute.
    Unrelated,
    /// `[<RequireQualifiedAccess>]`.
    Short,
    /// `[<RequireQualifiedAccessAttribute>]`, which FCS looks up first as
    /// `RequireQualifiedAccessAttributeAttribute`.
    Long,
    /// `[<Microsoft.FSharp.Core.RequireQualifiedAccess>]`.
    CoreQualified,
    /// `[<FSharp.Core.RequireQualifiedAccess>]`.
    FSharpQualified,
    /// `[<Microsoft.FSharp.Core.RequireQualifiedAccessAttribute>]`.
    CoreQualifiedLong,
    /// `[<type: RequireQualifiedAccess>]`.
    TypeTarget,
    /// `[<GridRqa>]`, under `type GridRqa = RequireQualifiedAccessAttribute`.
    Abbrev,
    /// `[<GridImp.RequireQualifiedAccess>]`: the impersonator's module.
    ImpQualified,
}

impl Spelling {
    const ALL: [Spelling; 10] = [
        Spelling::Absent,
        Spelling::Unrelated,
        Spelling::Short,
        Spelling::Long,
        Spelling::CoreQualified,
        Spelling::FSharpQualified,
        Spelling::CoreQualifiedLong,
        Spelling::TypeTarget,
        Spelling::Abbrev,
        Spelling::ImpQualified,
    ];

    fn attribute(self) -> Option<&'static str> {
        Some(match self {
            Spelling::Absent => return None,
            Spelling::Unrelated => "[<NoComparison>]",
            Spelling::Short => "[<RequireQualifiedAccess>]",
            Spelling::Long => "[<RequireQualifiedAccessAttribute>]",
            Spelling::CoreQualified => "[<Microsoft.FSharp.Core.RequireQualifiedAccess>]",
            Spelling::FSharpQualified => "[<FSharp.Core.RequireQualifiedAccess>]",
            Spelling::CoreQualifiedLong => {
                "[<Microsoft.FSharp.Core.RequireQualifiedAccessAttribute>]"
            }
            Spelling::TypeTarget => "[<type: RequireQualifiedAccess>]",
            Spelling::Abbrev => "[<GridRqa>]",
            Spelling::ImpQualified => "[<GridImp.RequireQualifiedAccess>]",
        })
    }

    /// What the spelling needs declared ahead of the union, in its container.
    fn prelude(self) -> &'static [&'static str] {
        match self {
            Spelling::Abbrev => &["type GridRqa = RequireQualifiedAccessAttribute"],
            _ => &[],
        }
    }
}

/// What else the union's container can see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Env {
    /// Nothing.
    Plain,
    /// A type named `RequireQualifiedAccessAttribute` in an opened module.
    ImpInScope,
    /// The same type in a module that is not opened.
    ImpOutOfScope,
    /// A type named `RequireQualifiedAccess`, in scope.
    ImpShort,
    /// An `exception RequireQualifiedAccessAttribute`, in scope.
    ImpException,
    /// `type RequireQualifiedAccessAttribute = NoComparisonAttribute`.
    RenamingAbbrev,
    /// The impersonator in an earlier file, opened.
    ImpEarlierFile,
    /// A type named `RequireQualifiedAccessAttributeAttribute`, in scope: the
    /// long spelling's first lookup.
    ImpDoubleSuffix,
    /// `type RequireQualifiedAccessAttributeAttribute = NoComparisonAttribute`.
    DoubleSuffixAbbrev,
    /// The impersonator in a referenced assembly's namespace, opened.
    ImpAssembly,
    /// A referenced `RequireQualifiedAccessAttributeAttribute`, opened.
    ImpAssemblyDoubleSuffix,
}

impl Env {
    const ALL: [Env; 11] = [
        Env::Plain,
        Env::ImpInScope,
        Env::ImpOutOfScope,
        Env::ImpShort,
        Env::ImpException,
        Env::RenamingAbbrev,
        Env::ImpEarlierFile,
        Env::ImpDoubleSuffix,
        Env::DoubleSuffixAbbrev,
        Env::ImpAssembly,
        Env::ImpAssemblyDoubleSuffix,
    ];

    /// Declarations in the union's container ahead of it.
    fn container_lines(self) -> &'static [&'static str] {
        match self {
            Env::Plain => &[],
            Env::ImpInScope => &[
                "module GridImp =",
                "    type RequireQualifiedAccessAttribute() =",
                "        inherit System.Attribute()",
                "open GridImp",
            ],
            Env::ImpOutOfScope => &[
                "module GridImp =",
                "    type RequireQualifiedAccessAttribute() =",
                "        inherit System.Attribute()",
            ],
            Env::ImpShort => &[
                "type RequireQualifiedAccess() =",
                "    inherit System.Attribute()",
            ],
            Env::ImpException => &["exception RequireQualifiedAccessAttribute of string"],
            Env::RenamingAbbrev => {
                &["type RequireQualifiedAccessAttribute = NoComparisonAttribute"]
            }
            Env::ImpEarlierFile => &["open GridImp"],
            Env::ImpDoubleSuffix => &[
                "type RequireQualifiedAccessAttributeAttribute() =",
                "    inherit System.Attribute()",
            ],
            Env::DoubleSuffixAbbrev => {
                &["type RequireQualifiedAccessAttributeAttribute = NoComparisonAttribute"]
            }
            Env::ImpAssembly => &["open RqaFix.ImpType"],
            Env::ImpAssemblyDoubleSuffix => &["open RqaDouble"],
        }
    }

    /// A file ahead of every other.
    fn earlier_file(self) -> Option<(&'static str, &'static str)> {
        (self == Env::ImpEarlierFile).then_some((
            "GridImp.fs",
            "module GridImp\n\ntype RequireQualifiedAccessAttribute() =\n    inherit System.Attribute()\n",
        ))
    }

    /// The fixture assembly the environment references.
    fn fixture(self) -> Fixture {
        match self {
            Env::ImpAssembly => Fixture::Union,
            Env::ImpAssemblyDoubleSuffix => Fixture::DoubleSuffix,
            _ => Fixture::None,
        }
    }

    /// Whether a `GridImp` module holding an impersonator exists.
    fn has_grid_imp(self) -> bool {
        matches!(
            self,
            Env::ImpInScope | Env::ImpOutOfScope | Env::ImpEarlierFile
        )
    }
}

/// The assembly a cell references beside FSharp.Core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fixture {
    None,
    /// `rqa_union_env`: the assembly unions, and a type named like the
    /// attribute.
    Union,
    /// `rqa_double_suffix_env`: a type named like the long spelling's first
    /// lookup, alone.
    DoubleSuffix,
    /// `rqa_collision_env`: a class `GridLib.GridC`, at the path a case of a
    /// union in a project `namespace GridLib` is named by.
    Collision,
}

/// The union in a referenced assembly: the fixture's namespace for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssemblyUnion {
    Plain,
    Short,
    Long,
    Abbrev,
    Imp,
}

impl AssemblyUnion {
    const ALL: [AssemblyUnion; 5] = [
        AssemblyUnion::Plain,
        AssemblyUnion::Short,
        AssemblyUnion::Long,
        AssemblyUnion::Abbrev,
        AssemblyUnion::Imp,
    ];
}

/// Where the union is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Location {
    /// The probe's own module, ahead of the use.
    SameModule,
    /// A nested `module GridLib` of the probe file.
    Nested,
    /// An earlier file's `module GridLib`.
    EarlierFile,
    /// An earlier file's `module GridLib` with a signature.
    EarlierSig,
    /// An earlier file's `namespace GridLib`, which a referenced assembly
    /// also declares, with a class named like the case
    /// ([`Fixture::Collision`]).
    EarlierNamespace,
    /// A referenced assembly's namespace.
    Assembly(AssemblyUnion),
}

impl Location {
    /// The `open` that puts the union's container in scope, if any.
    fn open(self) -> Option<String> {
        match self {
            Location::SameModule => None,
            Location::Nested
            | Location::EarlierFile
            | Location::EarlierSig
            | Location::EarlierNamespace => Some("open GridLib".to_string()),
            Location::Assembly(u) => Some(format!("open RqaFix.{u:?}")),
        }
    }
}

/// How the case is used. The first three follow an `open` of the union's
/// container; the last two name the container instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Use {
    BareExpr,
    BarePat,
    TypeQualified,
    /// `GridLib.GridC`: the case through its module, type elided.
    ModuleQualified,
    /// `GridLib.GridU.GridC`.
    FullyQualified,
}

impl Use {
    const ALL: [Use; 5] = [
        Use::BareExpr,
        Use::BarePat,
        Use::TypeQualified,
        Use::ModuleQualified,
        Use::FullyQualified,
    ];

    /// The use's line and the probed text in it (its first occurrence).
    fn line(self) -> (&'static str, &'static str) {
        match self {
            Use::BareExpr => ("let probe = GridC", "GridC"),
            Use::BarePat => (
                "let probe = match Unchecked.defaultof<_> with GridC -> 1 | _ -> 0",
                "GridC",
            ),
            Use::TypeQualified => ("let probe = GridU.GridC", "GridU.GridC"),
            Use::ModuleQualified => ("let probe = GridLib.GridC", "GridLib.GridC"),
            Use::FullyQualified => ("let probe = GridLib.GridU.GridC", "GridLib.GridU.GridC"),
        }
    }

    /// Whether the use names the union's container itself, with no `open`.
    fn names_container(self) -> bool {
        matches!(self, Use::ModuleQualified | Use::FullyQualified)
    }

    fn applies_to(self, location: Location) -> bool {
        !self.names_container()
            || matches!(
                location,
                Location::Nested
                    | Location::EarlierFile
                    | Location::EarlierSig
                    | Location::EarlierNamespace
            )
    }
}

/// The earlier union whose case a bare probe binds when the grid's union
/// holds its cases qualified-only. It sits in the grid's union's own
/// container, ahead of it, so that an `open` of the container brings both.
///
/// A probe through the container (`GridLib.GridC`) is the exception, with the
/// fallback in the probe file: FCS reads `M.C` as the case of the *first*
/// union of `M` declaring `C`, whether or not that union requires qualified
/// access (`TryFindTypeWithUnionCase`; an RQA one only adds an error), so a
/// fallback in the container would grade the precedence of module-qualified
/// lookup rather than the attribute.
const FALLBACK: [&str; 3] = ["type GridE =", "    | GridC", "    | GridF"];

/// The union's declaration, preceded by the fallback union when `fallback`,
/// the environment and the spelling's prelude, every line indented by
/// `indent`.
fn union_block(spelling: Spelling, env: Env, fallback: bool, indent: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    if fallback {
        lines.extend(FALLBACK);
    }
    lines.extend(env.container_lines());
    lines.extend(spelling.prelude());
    lines.extend(spelling.attribute());
    lines.extend(["type GridU =", "    | GridC", "    | GridD"]);
    lines.iter().map(|l| format!("{indent}{l}\n")).collect()
}

/// One generated cell.
struct Cell {
    label: String,
    /// `(relative path, source)` in Compile order, the probe file last.
    files: Vec<(String, String)>,
    probe: TextRange,
    /// We must commit here.
    control: bool,
    /// The assembly referenced beside FSharp.Core.
    fixture: Fixture,
}

/// The probe file: the fallback union when `fallback`, `body`, then the use.
fn probe_file(body: &str, fallback: bool, use_: Use) -> (String, TextRange) {
    let (line, probed) = use_.line();
    let fallback = if fallback {
        FALLBACK.map(|l| format!("{l}\n")).concat()
    } else {
        String::new()
    };
    let mut src = format!("module GridProbe\n\n{fallback}\n{body}\n");
    let at = src.len() + line.find(probed).expect("the probed text is in the line");
    src.push_str(line);
    src.push('\n');
    let range = TextRange::new(
        u32::try_from(at).unwrap().into(),
        u32::try_from(at + probed.len()).unwrap().into(),
    );
    (src, range)
}

fn cell(spelling: Spelling, env: Env, location: Location, use_: Use) -> Cell {
    let mut files: Vec<(String, String)> = env
        .earlier_file()
        .into_iter()
        .map(|(p, s)| (p.to_string(), s.to_string()))
        .collect();
    let in_container = !matches!(location, Location::Assembly(_)) && !use_.names_container();
    let body = match location {
        Location::SameModule => union_block(spelling, env, in_container, ""),
        Location::Nested => format!(
            "module GridLib =\n{}",
            union_block(spelling, env, in_container, "    ")
        ),
        Location::EarlierFile | Location::EarlierSig => {
            let lib = format!(
                "module GridLib\n\n{}",
                union_block(spelling, env, in_container, "")
            );
            if location == Location::EarlierSig {
                files.push(("GridLib.fsi".to_string(), lib.clone()));
            }
            files.push(("GridLib.fs".to_string(), lib));
            String::new()
        }
        Location::EarlierNamespace => {
            let lib = format!(
                "namespace GridLib\n\n{}",
                union_block(spelling, env, in_container, "")
            );
            files.push(("GridLib.fs".to_string(), lib));
            String::new()
        }
        Location::Assembly(_) => String::new(),
    };
    let body = match location.open() {
        Some(open) if !use_.names_container() => format!("{body}\n{open}\n"),
        _ => body,
    };
    let (src, probe) = probe_file(&body, !in_container, use_);
    files.push(("GridProbe.fs".to_string(), src));
    // FSharp.Core's attribute written plainly, or no attribute, with nothing
    // that could impersonate it. A dotted value path after an `open` (the open
    // stales its head) and every use of a signature's union decline for
    // reasons of their own, and `GridLib.GridC` has nothing to commit to
    // under the attribute.
    let plain = env == Env::Plain && matches!(spelling, Spelling::Absent | Spelling::Short);
    let control = match location {
        Location::SameModule => plain,
        Location::Nested | Location::EarlierFile => {
            plain
                && match use_ {
                    Use::BareExpr | Use::BarePat | Use::FullyQualified => true,
                    Use::TypeQualified => false,
                    Use::ModuleQualified => spelling == Spelling::Absent,
                }
        }
        Location::EarlierSig | Location::EarlierNamespace => false,
        Location::Assembly(_) => true,
    };
    let label = match location {
        Location::Assembly(u) => format!("Assembly({u:?}) / {use_:?}"),
        _ => format!("{location:?} / {spelling:?} / {env:?} / {use_:?}"),
    };
    Cell {
        label,
        files,
        probe,
        control,
        fixture: match location {
            Location::Assembly(_) => Fixture::Union,
            Location::EarlierNamespace => Fixture::Collision,
            _ => env.fixture(),
        },
    }
}

/// The spelling × environment pairs the grid crosses: every spelling in a
/// plain environment, and the spellings an impersonator could hijack in each
/// other one.
fn setups() -> Vec<(Spelling, Env)> {
    let mut out = Vec::new();
    for env in Env::ALL {
        for spelling in Spelling::ALL {
            let wanted = match env {
                Env::Plain => spelling != Spelling::ImpQualified,
                _ => {
                    matches!(
                        spelling,
                        Spelling::Absent
                            | Spelling::Unrelated
                            | Spelling::Short
                            | Spelling::Long
                            | Spelling::CoreQualified
                            | Spelling::CoreQualifiedLong
                    ) || (spelling == Spelling::ImpQualified && env.has_grid_imp())
                }
            };
            if wanted {
                out.push((spelling, env));
            }
        }
    }
    out
}

fn cells() -> Vec<Cell> {
    let mut out = Vec::new();
    for (spelling, env) in setups() {
        for location in [
            Location::SameModule,
            Location::Nested,
            Location::EarlierFile,
        ] {
            for use_ in Use::ALL {
                if use_.applies_to(location) {
                    out.push(cell(spelling, env, location, use_));
                }
            }
        }
        // A signature file states the environment through `open`s only.
        if matches!(env, Env::Plain | Env::ImpEarlierFile) {
            for use_ in Use::ALL {
                out.push(cell(spelling, env, Location::EarlierSig, use_));
            }
        }
        // The namespace's cells reference the collision fixture, so an
        // environment with an assembly of its own is not crossed with them.
        if env.fixture() == Fixture::None {
            for use_ in Use::ALL {
                out.push(cell(spelling, env, Location::EarlierNamespace, use_));
            }
        }
    }
    for u in AssemblyUnion::ALL {
        for use_ in Use::ALL {
            let location = Location::Assembly(u);
            if use_.applies_to(location) {
                out.push(cell(Spelling::Absent, Env::Plain, location, use_));
            }
        }
    }
    out
}

/// Every cell of the grid agrees with FCS wherever we commit, and every
/// control cell commits.
#[test]
fn every_union_rqa_cell_agrees_with_fcs() {
    let all = cells();
    let fsharp_core = ensure_fsharp_core_dll();
    let union_dll = ensure_rqa_union_fixture_built();
    let double_suffix_dll = ensure_rqa_double_suffix_fixture_built();
    let collision_dll = ensure_rqa_collision_fixture_built();
    let refs_for = |fixture: Fixture| -> Vec<&Path> {
        match fixture {
            Fixture::None => vec![],
            Fixture::Union => vec![union_dll],
            Fixture::DoubleSuffix => vec![double_suffix_dll],
            Fixture::Collision => vec![collision_dll],
        }
    };
    let envs: Vec<(Fixture, AssemblyEnv)> = [
        Fixture::None,
        Fixture::Union,
        Fixture::DoubleSuffix,
        Fixture::Collision,
    ]
    .into_iter()
    .map(|f| {
        let dlls: Vec<&Path> = std::iter::once(fsharp_core.as_path())
            .chain(refs_for(f))
            .collect();
        (f, env_of(&dlls))
    })
    .collect();
    let env_for = |cell: &Cell| -> &AssemblyEnv {
        &envs
            .iter()
            .find(|(f, _)| *f == cell.fixture)
            .expect("an env per fixture")
            .1
    };

    // Each cell is its own FCS project; the resident pool serves several at
    // once, so ask in parallel and grade in order.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let answers: Vec<std::sync::Mutex<Option<_>>> =
        all.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(cell) = all.get(i) else { break };
                    let rel: Vec<(&str, &str)> = cell
                        .files
                        .iter()
                        .map(|(p, s)| (p.as_str(), s.as_str()))
                        .collect();
                    let (root, written) = temp_fs_tree("union_rqa_grid", &rel);
                    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
                    let refs = refs_for(cell.fixture);
                    let json = invoke_fcs_dump_project_with_refs(&paths, &refs);
                    let fcs = parse_fcs_uses_project(&json, &written);
                    let _ = std::fs::remove_dir_all(&root);
                    *answers[i].lock().unwrap() = Some((written, fcs));
                }
            });
        }
    });

    let mut failures = Vec::new();
    let mut entries = Vec::new();
    // Non-vacuity: commits to each reading.
    let mut committed_qualified_only = 0;
    let mut committed_bare = 0;
    for (cell, answer) in all.iter().zip(answers) {
        let (written, fcs) = answer.into_inner().unwrap().expect("every cell was asked");
        let env = env_for(cell);
        let rel: Vec<(&str, &str)> = cell
            .files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        let probe_idx = written.len() - 1;
        let probe_path = &written[probe_idx].0;
        let fu = fcs
            .iter()
            .find(|f| f.path.file_name() == probe_path.file_name())
            .unwrap_or_else(|| panic!("no FCS uses for {}", cell.label));
        let errors: Vec<&String> = fcs.iter().flat_map(|f| &f.errors).collect();
        let theirs = fcs_at(fu, cell.probe);
        let rendered = render_fcs(theirs);

        let proj = resolve_project_files(&project(&rel), env);
        if errors.is_empty() {
            failures.extend(unmodelled_project_binder_failures(
                &cell.label,
                &proj,
                &written,
                &fcs,
            ));
        }
        let ours = proj
            .file(probe_idx)
            .resolution_at(cell.probe)
            .filter(|r| !matches!(r, Resolution::Deferred(_) | Resolution::Unresolved));
        let file_paths: Vec<PathBuf> = written.iter().map(|(p, _)| p.clone()).collect();
        let verdict = match ours {
            Some(res) => {
                if theirs.is_some_and(|u| agrees(env, &proj, &file_paths, probe_idx, res, u)) {
                    if rendered.contains("GridE.GridC") {
                        committed_qualified_only += 1;
                    } else {
                        committed_bare += 1;
                    }
                } else {
                    failures.push(format!(
                        "  {} [WRONG]\n    FCS:  {rendered}\n    ours: {res:?}\n    errors: {errors:?}\n    files: {:?}",
                        cell.label, cell.files
                    ));
                }
                "commit"
            }
            None => {
                if cell.control {
                    failures.push(format!(
                        "  {} [MUST COMMIT]\n    FCS:  {rendered}\n    ours: {:?}\n    files: {:?}",
                        cell.label,
                        proj.file(probe_idx).decline_site(cell.probe),
                        cell.files
                    ));
                }
                "decline"
            }
        };
        // A dotted probe's last segment carries a record of its own, which a
        // decline of the whole path does not withdraw: it must name FCS's
        // symbol too (FCS keys the access by the whole path).
        let probe_src = &cell.files.last().expect("the probe file").1;
        let probe_text = &probe_src[usize::from(cell.probe.start())..usize::from(cell.probe.end())];
        if let Some(dot) = probe_text.rfind('.') {
            let leaf = TextRange::new(
                cell.probe.start() + rowan::TextSize::from(u32::try_from(dot + 1).unwrap()),
                cell.probe.end(),
            );
            let at_leaf = fcs_at(fu, leaf).or(theirs);
            if let Some(res) = proj
                .file(probe_idx)
                .resolution_at(leaf)
                .filter(|r| !matches!(r, Resolution::Deferred(_) | Resolution::Unresolved))
                && !at_leaf.is_some_and(|u| agrees(env, &proj, &file_paths, probe_idx, res, u))
            {
                failures.push(format!(
                    "  {} [WRONG LEAF]\n    FCS:  {}\n    ours: {res:?}\n    errors: {errors:?}\n    files: {:?}",
                    cell.label,
                    render_fcs(at_leaf),
                    cell.files
                ));
            }
        }
        let clean = if errors.is_empty() {
            "clean"
        } else {
            "erroring"
        };
        entries.push(format!(
            "{} | fcs {rendered} ({clean}) | ours {verdict}",
            cell.label
        ));
    }
    if !failures.is_empty() {
        for e in &entries {
            eprintln!("cell: {e}");
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cells disagree with FCS:\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
    assert!(
        committed_qualified_only > 0 && committed_bare > 0,
        "the grid must commit under both readings \
         (qualified-only: {committed_qualified_only}, bare: {committed_bare})"
    );
    let manifest_entries = Manifest::from_entries(entries).expect("manifest entries");
    check_manifest(
        "union_rqa_attribute_grid",
        &manifest_entries,
        &regenerate("union_rqa_attribute_grid::every_union_rqa_cell_agrees_with_fcs"),
    );
}

/// Grade one project against FCS: the probe file is the last of `files`, and
/// `probed` its last occurrence of the probed text. Wherever we commit — the
/// whole text, or a dotted text's last segment — FCS must report that very
/// symbol; the error names the first commit it does not.
fn project_probe_agrees(files: &[(&str, &str)], probed: &str) -> Result<(), String> {
    let fsharp_core = ensure_fsharp_core_dll();
    let env = env_of(&[fsharp_core.as_path()]);
    let (root, written) = temp_fs_tree("union_rqa_probe", files);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let fcs = parse_fcs_uses_project(&invoke_fcs_dump_project_with_refs(&paths, &[]), &written);
    let _ = std::fs::remove_dir_all(&root);
    let last = written.len() - 1;
    let fu = fcs
        .iter()
        .find(|f| f.path.file_name() == written[last].0.file_name())
        .expect("the probe file's uses");
    let src = files[last].1;
    let start = src.rfind(probed).expect("the probed text");
    let whole = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + probed.len()).unwrap().into(),
    );
    let leaf_start = start + probed.rfind('.').map_or(0, |d| d + 1);
    let leaf = TextRange::new(u32::try_from(leaf_start).unwrap().into(), whole.end());
    let theirs = fcs_at(fu, whole);
    assert!(theirs.is_some(), "FCS records no symbol at {probed}");
    let proj = resolve_project_files(&project(files), &env);
    let file_paths: Vec<PathBuf> = written.iter().map(|(p, _)| p.clone()).collect();
    for range in [whole, leaf] {
        let at = fcs_at(fu, range).or(theirs);
        if let Some(res) = proj
            .file(last)
            .resolution_at(range)
            .filter(|r| !matches!(r, Resolution::Deferred(_) | Resolution::Unresolved))
            && !at.is_some_and(|u| agrees(&env, &proj, &file_paths, last, res, u))
        {
            return Err(format!(
                "{probed} at {range:?}: ours {res:?}, FCS {}",
                render_fcs(at)
            ));
        }
    }
    Ok(())
}

/// Assert each `(attribute, known)` row of `rows`, the union declared under
/// `attribute` by `files_for`: certain-implies-exact where `known` is `None`,
/// and where it names an issue, the pre-existing qualified-path divergence
/// that issue tracks — pinned, so that fixing it fails here until the row
/// moves.
fn assert_rows(
    rows: &[(&str, Option<&str>)],
    probed: &str,
    files_for: impl Fn(&str) -> Vec<(String, String)>,
) {
    let mut failures = Vec::new();
    for (attr, known) in rows {
        let files = files_for(attr);
        let rel: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        match (project_probe_agrees(&rel, probed), known) {
            (Ok(()), None) | (Err(_), Some(_)) => {}
            (Err(e), None) => failures.push(format!("{attr}: {e}")),
            (Ok(()), Some(issue)) => {
                failures.push(format!("{attr}: now agrees with FCS; {issue} may be fixed"))
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `GridLib.GridC` where `GridLib` declares an earlier union with the case
/// too: FCS reads the first union's case (`TryFindTypeWithUnionCase`),
/// whatever the later union's attribute. The grid keeps its fallback out of
/// the container for this probe (see [`FALLBACK`]), so the shape is graded
/// here. A union exported with a value path takes the path from the earlier
/// one, the walk's latest-wins reading of a module-qualified case (#469).
#[test]
fn a_module_qualified_case_through_an_undecided_union() {
    let rows = [
        ("[<RequireQualifiedAccess>]", None),
        ("[<Microsoft.FSharp.Core.RequireQualifiedAccess>]", None),
        ("[<RequireQualifiedAccess(); NoComparison>]", None),
        ("[<GridRqa>]", Some("#469")),
        ("[<NoComparison>]", Some("#469")),
    ];
    assert_rows(&rows, "GridLib.GridC", |attr| {
        vec![
            (
                "GridLib.fs".to_string(),
                format!(
                    "module GridLib\n\ntype GridRqa = RequireQualifiedAccessAttribute\ntype GridE =\n    | GridC\n    | GridF\n{attr}\ntype GridU =\n    | GridC\n    | GridD\n"
                ),
            ),
            (
                "GridProbe.fs".to_string(),
                "module GridProbe\n\nlet probe = GridLib.GridC\n".to_string(),
            ),
        ]
    });
}

/// `N.GridC` where two earlier files' fragments of `namespace N` each declare
/// a union with the case: FCS reads the later fragment's, whatever its
/// attribute. A union exported qualified-only leaves the path to the earlier
/// fragment's case (#472).
#[test]
fn a_namespace_qualified_case_through_an_undecided_union() {
    let rows = [
        ("[<RequireQualifiedAccess>]", Some("#472")),
        (
            "[<Microsoft.FSharp.Core.RequireQualifiedAccess>]",
            Some("#472"),
        ),
        ("[<RequireQualifiedAccess(); NoComparison>]", Some("#472")),
        ("[<GridRqa>]", None),
        ("[<NoComparison>]", None),
    ];
    assert_rows(&rows, "N.GridC", |attr| {
        vec![
            (
                "First.fs".to_string(),
                "namespace N\n\ntype GridE =\n    | GridC\n    | GridF\n".to_string(),
            ),
            (
                "Second.fs".to_string(),
                format!(
                    "namespace N\n\ntype GridRqa = RequireQualifiedAccessAttribute\n{attr}\ntype GridU =\n    | GridC\n    | GridD\n"
                ),
            ),
            (
                "GridProbe.fs".to_string(),
                "module GridProbe\n\nlet probe = N.GridC\n".to_string(),
            ),
        ]
    });
}

/// A qualified-only case named like a module puts no bare name in scope, so
/// it hides no qualified path that module heads: `List.map` after
/// `[<RequireQualifiedAccess>] type T = | List | Other` is FSharp.Core's, in
/// the case's own file and in a later one.
#[test]
fn a_qualified_only_case_hides_no_bare_qualifier() {
    let lib = "module GridLib\n\n[<RequireQualifiedAccess>]\ntype T =\n    | List\n    | Other\n\nlet here = List.map id [ 1 ]\n";
    let later = "module GridProbe\n\nlet there = GridLib.here, List.map id [ 1 ]\n";
    let files = [("GridLib.fs", lib), ("GridProbe.fs", later)];
    let env = env_of(&[ensure_fsharp_core_dll().as_path()]);
    let proj = resolve_project_files(&project(&files), &env);
    for (idx, (_, src)) in files.iter().enumerate() {
        let start = src.rfind("List.map").expect("the probe");
        let end = start + "List.map".len();
        let span = |from: usize| {
            TextRange::new(
                u32::try_from(from).unwrap().into(),
                u32::try_from(end).unwrap().into(),
            )
        };
        let commits = [span(start), span(start + "List.".len())]
            .into_iter()
            .filter_map(|r| proj.file(idx).resolution_at(r))
            .any(|r| matches!(r, Resolution::Member { .. } | Resolution::Entity(_)));
        assert!(
            commits,
            "file {idx}: `List.map` is not committed: {:?}",
            [span(start), span(start + "List.".len())].map(|r| proj.file(idx).decline_site(r))
        );
    }
    project_probe_agrees(&files, "List.map").expect("the later file agrees with FCS");
}
