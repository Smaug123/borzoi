//! The **qualified case precedence grid**: which of several same-named
//! members a container-qualified `M.C` binds, graded against FCS.
//!
//! FCS reads `M.C` in an expression through
//! `ResolveExprLongIdentInModuleOrNamespace` (`NameResolution.fs`): an
//! accessible *value* of the container first, then an accessible
//! *exception*, then the case of the **first** entity of the container's
//! entity list that declares the name (`TryFindTypeWithUnionCase`, a
//! `QueueList.tryFind` that does not look at accessibility, so an
//! inaccessible first union hides every later one). A pattern
//! (`ResolvePatternLongIdentInModuleOrNamespace`) asks for the union case
//! first, then the exception, then an active pattern, then a value. A bare
//! name after `open M` is different again: the name environment's
//! latest-write-wins over `AddModuleOrNamespaceContentsToNameEnv`'s kind
//! order ([`fold_rank`](borzoi_sema)).
//!
//! The entity list is declaration order for a module, and for a namespace
//! declared in one file; a namespace declared across several files lists the
//! later file's entities first (`CombineCcuContentFragments [implFileSigType;
//! tcsCcuSig]`), each file's in declaration order.
//!
//! The grid crosses:
//!
//! - the **unions** declaring the case (one to three) and their **case
//!   shapes** (nullary, one field, two fields; equal and differing arity);
//! - an **extra** member of the name: a value ahead of the unions, an
//!   exception ahead of or behind them, an active pattern `(|C|_|)` ahead of
//!   or behind them, an abbreviation of the first union behind them, a
//!   possible case `type A = C` ahead of them, an `extern` behind them, or
//!   the first union `private`,
//!   with a `private` representation, or `[<RequireQualifiedAccess>]`;
//! - the **container**: a nested module of the probe file, an earlier file's
//!   module (with a signature stating the same members, and with one hiding
//!   the extra), a module nested in an earlier file's namespace, an earlier
//!   file's namespace, a namespace split across two earlier files or within
//!   one, and a referenced assembly's module and namespace;
//! - the **use**: `M.C` in an expression and in a pattern, `M.Un.C`, and the
//!   bare name after `open M` in an expression and in a pattern.
//!
//! The property is certain-implies-exact at every use; the control uses must
//! commit, so the grid cannot pass by declining. Every use's two answers are
//! pinned in `tests/manifests/qualified_case_precedence_grid.txt`.

use std::path::{Path, PathBuf};

use crate::common::corpus_manifest::{check_manifest, regenerate};
use crate::common::{
    ensure_fsharp_core_dll, ensure_qualified_case_fixture_built, invoke_fcs_dump_project_with_refs,
    parse_fcs_uses_project, temp_fs_tree, unmodelled_project_binder_failures,
};
use crate::resolve_signatures::project;
use crate::unmodelled_contributor_grid::{agrees, env_of, fcs_at, render_fcs};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_sema::{Resolution, resolve_project_files};
use rowan::TextRange;

/// The fields of one union's case `GridC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Nullary,
    Unary,
    Binary,
}

impl Shape {
    fn fields(self) -> &'static str {
        match self {
            Shape::Nullary => "",
            Shape::Unary => " of int",
            Shape::Binary => " of int * string",
        }
    }

    /// The wildcard arguments a pattern of this case takes.
    fn pattern_args(self) -> &'static str {
        match self {
            Shape::Nullary => "",
            Shape::Unary => " _",
            Shape::Binary => " (_, _)",
        }
    }
}

/// A member of the container besides the unions, named like the case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Extra {
    None,
    /// `let GridC (x: int) = x`, ahead of the unions: behind one, the head
    /// would be a case pattern.
    ValueFirst,
    ExnFirst,
    ExnLast,
    ApFirst,
    ApLast,
    /// `type GridA = GridU1`, behind the unions.
    AbbrevLast,
    /// The first union is `private`.
    PrivateFirst,
    /// The first union's representation is `private` (`type U = private …`).
    PrivateReprFirst,
    /// An `extern` of the name behind the unions: a value FCS reads first,
    /// which the walk interns no identity for.
    ExternLast,
    /// The first union is `[<RequireQualifiedAccess>]`.
    RqaFirst,
    /// `type GridA = GridC` ahead of the unions: a union with the nullary
    /// case `GridC`, since no type of that name is in scope
    /// (`TyconCoreAbbrevThatIsReallyAUnion`).
    AbbrevCaseFirst,
}

impl Extra {
    const ALL: [Extra; 11] = [
        Extra::ValueFirst,
        Extra::ExnFirst,
        Extra::ExnLast,
        Extra::ApFirst,
        Extra::ApLast,
        Extra::AbbrevLast,
        Extra::PrivateFirst,
        Extra::PrivateReprFirst,
        Extra::ExternLast,
        Extra::RqaFirst,
        Extra::AbbrevCaseFirst,
    ];

    /// Whether a namespace can hold it: no `let` there, and a `private`
    /// namespace member is scoped to the namespace, not to a module.
    fn fits_namespace(self) -> bool {
        !matches!(
            self,
            Extra::ValueFirst
                | Extra::ApFirst
                | Extra::ApLast
                | Extra::PrivateFirst
                | Extra::PrivateReprFirst
                | Extra::ExternLast
        )
    }

    /// Whether a signature file can state it: a `private` type in a
    /// signature is a different question from the one this grid asks.
    fn fits_signature(self) -> bool {
        !matches!(
            self,
            Extra::PrivateFirst | Extra::PrivateReprFirst | Extra::ExternLast
        )
    }

    /// Whether a signature can leave it out: a member of its own, not a
    /// change to a union the signature states.
    fn hides_behind_signature(self) -> bool {
        !matches!(
            self,
            Extra::None | Extra::PrivateFirst | Extra::PrivateReprFirst | Extra::RqaFirst
        )
    }

    /// The member's line, in an implementation file or, when `sig`, in a
    /// signature.
    fn line(self, sig: bool) -> Option<&'static str> {
        match self {
            Extra::ValueFirst if sig => Some("val GridC: x: int -> int"),
            Extra::ValueFirst => Some("let GridC (x: int) = x"),
            Extra::ExnFirst | Extra::ExnLast => Some("exception GridC of string"),
            Extra::ApFirst | Extra::ApLast if sig => Some("val (|GridC|_|): obj -> unit option"),
            Extra::ApFirst | Extra::ApLast => Some("let (|GridC|_|) (_: obj) : unit option = None"),
            Extra::AbbrevLast => Some("type GridA = GridU1"),
            Extra::AbbrevCaseFirst => Some("type GridA = GridC"),
            Extra::ExternLast => Some(
                "[<System.Runtime.InteropServices.DllImport(\"grid\")>] extern int GridC(int x)",
            ),
            Extra::None | Extra::PrivateFirst | Extra::PrivateReprFirst | Extra::RqaFirst => None,
        }
    }

    fn first(self) -> bool {
        matches!(
            self,
            Extra::ValueFirst | Extra::ExnFirst | Extra::ApFirst | Extra::AbbrevCaseFirst
        )
    }
}

/// The container's members named like the case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Setup {
    /// One union per entry, `GridU1` first, each declaring `GridC` of that
    /// shape and a case `GridD<n>` of its own.
    shapes: Vec<Shape>,
    extra: Extra,
}

impl Setup {
    fn label(&self) -> String {
        let shapes: Vec<String> = self.shapes.iter().map(|s| format!("{s:?}")).collect();
        format!("[{}] {:?}", shapes.join(","), self.extra)
    }

    fn union_lines(&self, n: usize) -> Vec<String> {
        let private = if n == 1 && self.extra == Extra::PrivateFirst {
            "private "
        } else {
            ""
        };
        let rqa = (n == 1 && self.extra == Extra::RqaFirst)
            .then(|| "[<RequireQualifiedAccess>]".to_string());
        let repr_private =
            (n == 1 && self.extra == Extra::PrivateReprFirst).then(|| "    private".to_string());
        rqa.into_iter()
            .chain([format!("type {private}GridU{n} =")])
            .chain(repr_private)
            .chain([
                format!("    | GridC{}", self.shapes[n - 1].fields()),
                format!("    | GridD{n}"),
            ])
            .collect()
    }

    /// The member lines of unions `from..=to` (1-based), with the extra
    /// member where `head`/`tail` say it goes.
    fn lines(&self, from: usize, to: usize, head: bool, tail: bool, sig: bool) -> Vec<String> {
        let mut out = Vec::new();
        let extra = self.extra.line(sig);
        if head && self.extra.first() {
            out.extend(extra.map(str::to_string));
        }
        for n in from..=to {
            out.extend(self.union_lines(n));
        }
        if tail && !self.extra.first() {
            out.extend(extra.map(str::to_string));
        }
        out
    }

    /// Every member line, in one declaration group.
    fn all_lines(&self) -> Vec<String> {
        self.lines(1, self.shapes.len(), true, true, false)
    }

    /// The union a pattern `M.C` reads after the container's entity list is
    /// ordered: the first in it, or, split across two files, the first of the
    /// later file's.
    fn qualified_pattern_shape(&self, split: bool) -> Shape {
        if split && self.shapes.len() > 1 {
            self.shapes[1]
        } else if self.extra == Extra::AbbrevCaseFirst {
            Shape::Nullary
        } else {
            self.shapes[0]
        }
    }

    /// The pattern arguments the bare name takes after an `open`: an active
    /// pattern defined behind the unions or ahead of them is a value, which
    /// outranks a case; otherwise the latest union's case.
    fn bare_pattern_args(&self) -> &'static str {
        if matches!(self.extra, Extra::ApFirst | Extra::ApLast) {
            ""
        } else {
            self.shapes.last().expect("a union").pattern_args()
        }
    }
}

/// Every setup the grid crosses: the case shapes with nothing else, then each
/// extra member with two and with three unions.
pub(crate) fn setups() -> Vec<Setup> {
    use Shape::{Binary as B, Nullary as N, Unary as U};
    let mut out: Vec<Setup> = [
        vec![N],
        vec![U],
        vec![N, N],
        vec![U, U],
        vec![N, U],
        vec![U, N],
        vec![U, B],
        vec![N, N, N],
        vec![N, U, B],
        vec![B, U, N],
    ]
    .into_iter()
    .map(|shapes| Setup {
        shapes,
        extra: Extra::None,
    })
    .collect();
    for extra in Extra::ALL {
        for shapes in [vec![U, N], vec![U, N, B]] {
            out.push(Setup { shapes, extra });
        }
    }
    out
}

/// The fixture assembly's namespace holding setup `k`'s module.
fn assembly_module_ns(k: usize) -> String {
    format!("QcFix.M{k}")
}

/// The fixture assembly's namespace holding setup `k`'s members directly.
fn assembly_namespace(k: usize) -> String {
    format!("QcFix.N{k}")
}

/// The source of the fixture assembly (`tests/fixtures/qualified_case_env`):
/// each setup in a module of its own namespace, and, where a namespace can
/// hold it, directly in another namespace.
pub(crate) fn fixture_source() -> String {
    let mut src = String::new();
    for (k, setup) in setups().iter().enumerate() {
        src.push_str(&format!(
            "namespace {}\n\nmodule GridLib =\n",
            assembly_module_ns(k)
        ));
        for line in setup.all_lines() {
            src.push_str(&format!("    {line}\n"));
        }
        src.push('\n');
        if setup.extra.fits_namespace() {
            src.push_str(&format!("namespace {}\n\n", assembly_namespace(k)));
            for line in setup.all_lines() {
                src.push_str(&format!("{line}\n"));
            }
            src.push('\n');
        }
    }
    src
}

/// Where the container is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    /// A nested `module GridLib` of the probe file.
    Nested,
    /// An earlier file's `module GridLib`.
    EarlierModule,
    /// An earlier file's `module GridLib` with a signature declaring the same
    /// members in the same order.
    EarlierSig,
    /// An earlier file's `module GridLib` whose signature declares only the
    /// unions, hiding the extra member.
    EarlierSigHiding,
    /// `module GridLib` nested in an earlier file's `namespace GridNs`.
    EarlierNested,
    /// An earlier file's `namespace GridLib`.
    EarlierNamespace,
    /// `namespace GridLib` in two earlier files: the first union in the
    /// first, the rest in the second.
    SplitNamespace,
    /// `namespace GridLib` twice in one earlier file, split like
    /// [`Container::SplitNamespace`].
    SplitNamespaceOneFile,
    /// A module of the fixture assembly.
    AssemblyModule,
    /// A namespace of the fixture assembly.
    AssemblyNamespace,
}

impl Container {
    const ALL: [Container; 10] = [
        Container::Nested,
        Container::EarlierModule,
        Container::EarlierSig,
        Container::EarlierSigHiding,
        Container::EarlierNested,
        Container::EarlierNamespace,
        Container::SplitNamespace,
        Container::SplitNamespaceOneFile,
        Container::AssemblyModule,
        Container::AssemblyNamespace,
    ];

    fn is_namespace(self) -> bool {
        matches!(
            self,
            Container::EarlierNamespace
                | Container::SplitNamespace
                | Container::SplitNamespaceOneFile
                | Container::AssemblyNamespace
        )
    }

    fn is_assembly(self) -> bool {
        matches!(
            self,
            Container::AssemblyModule | Container::AssemblyNamespace
        )
    }

    /// The qualified path of the container, for setup `k`.
    fn path(self, k: usize) -> String {
        match self {
            Container::Nested
            | Container::EarlierModule
            | Container::EarlierSig
            | Container::EarlierSigHiding
            | Container::EarlierNamespace
            | Container::SplitNamespace
            | Container::SplitNamespaceOneFile => "GridLib".to_string(),
            Container::EarlierNested => "GridNs.GridLib".to_string(),
            Container::AssemblyModule => format!("{}.GridLib", assembly_module_ns(k)),
            Container::AssemblyNamespace => assembly_namespace(k),
        }
    }
}

/// How the case is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Use {
    /// `M.GridC` in an expression.
    QualExpr,
    /// `M.GridC` in a pattern.
    QualPat,
    /// `M.GridU<last>.GridC`.
    TypeQual,
    /// `GridC` in an expression after `open M`.
    BareExpr,
    /// `GridC` in a pattern after `open M`.
    BarePat,
}

/// One generated project and its probes.
struct Cell {
    label: String,
    /// `(relative path, source)` in Compile order, the probe file last.
    files: Vec<(String, String)>,
    /// Each use and the probed range in the probe file.
    probes: Vec<(Use, TextRange)>,
    container: Container,
    setup: Setup,
}

/// Indent every line of `lines` by `indent`, newline-terminated.
fn block(lines: &[String], indent: &str) -> String {
    lines.iter().map(|l| format!("{indent}{l}\n")).collect()
}

fn cell(k: usize, setup: &Setup, container: Container) -> Cell {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut src = "module GridProbe\n\n".to_string();
    match container {
        Container::Nested => {
            src.push_str("module GridLib =\n");
            src.push_str(&block(&setup.all_lines(), "    "));
            src.push('\n');
        }
        Container::EarlierModule => files.push((
            "GridLib.fs".to_string(),
            format!("module GridLib\n\n{}", block(&setup.all_lines(), "")),
        )),
        Container::EarlierSig => {
            let n = setup.shapes.len();
            files.push((
                "GridLib.fsi".to_string(),
                format!(
                    "module GridLib\n\n{}",
                    block(&setup.lines(1, n, true, true, true), "")
                ),
            ));
            files.push((
                "GridLib.fs".to_string(),
                format!("module GridLib\n\n{}", block(&setup.all_lines(), "")),
            ));
        }
        Container::EarlierSigHiding => {
            let unions = Setup {
                shapes: setup.shapes.clone(),
                extra: Extra::None,
            };
            files.push((
                "GridLib.fsi".to_string(),
                format!("module GridLib\n\n{}", block(&unions.all_lines(), "")),
            ));
            files.push((
                "GridLib.fs".to_string(),
                format!("module GridLib\n\n{}", block(&setup.all_lines(), "")),
            ));
        }
        Container::EarlierNested => files.push((
            "GridLib.fs".to_string(),
            format!(
                "namespace GridNs\n\nmodule GridLib =\n{}",
                block(&setup.all_lines(), "    ")
            ),
        )),
        Container::EarlierNamespace => files.push((
            "GridLib.fs".to_string(),
            format!("namespace GridLib\n\n{}", block(&setup.all_lines(), "")),
        )),
        Container::SplitNamespace => {
            let n = setup.shapes.len();
            files.push((
                "First.fs".to_string(),
                format!(
                    "namespace GridLib\n\n{}",
                    block(&setup.lines(1, 1, true, n == 1, false), "")
                ),
            ));
            if n > 1 {
                files.push((
                    "Second.fs".to_string(),
                    format!(
                        "namespace GridLib\n\n{}",
                        block(&setup.lines(2, n, false, true, false), "")
                    ),
                ));
            }
        }
        Container::SplitNamespaceOneFile => {
            let n = setup.shapes.len();
            let mut src = format!(
                "namespace GridLib\n\n{}",
                block(&setup.lines(1, 1, true, n == 1, false), "")
            );
            if n > 1 {
                src.push_str(&format!(
                    "\nnamespace GridLib\n\n{}",
                    block(&setup.lines(2, n, false, true, false), "")
                ));
            }
            files.push(("GridLib.fs".to_string(), src));
        }
        Container::AssemblyModule | Container::AssemblyNamespace => {}
    }
    let path = container.path(k);
    let last = setup.shapes.len();
    let qual_args = setup
        .qualified_pattern_shape(container == Container::SplitNamespace)
        .pattern_args();
    let lines: Vec<(Use, String, String)> = vec![
        (
            Use::QualExpr,
            format!("let qualExpr = {path}.GridC"),
            format!("{path}.GridC"),
        ),
        (
            Use::QualPat,
            format!(
                "let qualPat = match Unchecked.defaultof<_> with {path}.GridC{qual_args} -> 1 | _ -> 0"
            ),
            format!("{path}.GridC"),
        ),
        (
            Use::TypeQual,
            format!("let typeQual = {path}.GridU{last}.GridC"),
            format!("{path}.GridU{last}.GridC"),
        ),
    ];
    let mut probes = Vec::new();
    for (use_, line, probed) in lines {
        let at = src.len() + line.find(&probed).expect("the probed text is in the line");
        probes.push((use_, range(at, probed.len())));
        src.push_str(&line);
        src.push('\n');
    }
    src.push_str(&format!("\nmodule Bare =\n    open {path}\n"));
    let bare = [
        (Use::BareExpr, "    let bareExpr = GridC".to_string()),
        (
            Use::BarePat,
            format!(
                "    let barePat = match Unchecked.defaultof<_> with GridC{} -> 1 | _ -> 0",
                setup.bare_pattern_args()
            ),
        ),
    ];
    for (use_, line) in bare {
        let at = src.len() + line.find("GridC").expect("the probed text is in the line");
        probes.push((use_, range(at, "GridC".len())));
        src.push_str(&line);
        src.push('\n');
    }
    files.push(("GridProbe.fs".to_string(), src));
    Cell {
        label: format!("{container:?} / {}", setup.label()),
        files,
        probes,
        container,
        setup: setup.clone(),
    }
}

fn range(at: usize, len: usize) -> TextRange {
    TextRange::new(
        u32::try_from(at).unwrap().into(),
        u32::try_from(at + len).unwrap().into(),
    )
}

fn cells() -> Vec<Cell> {
    let mut out = Vec::new();
    for (k, setup) in setups().iter().enumerate() {
        for container in Container::ALL {
            if container.is_namespace() && !setup.extra.fits_namespace()
                || container == Container::EarlierSig && !setup.extra.fits_signature()
                || container == Container::EarlierSigHiding && !setup.extra.hides_behind_signature()
            {
                continue;
            }
            out.push(cell(k, setup, container));
        }
    }
    out
}

/// The uses we must commit: the type-qualified case always; `M.C` in an
/// expression in every project container (not a signatured one, whose
/// readings the signature screen declines) unless the first union is out of
/// reach, requires qualified access or may not be a union; and, with no other
/// member of the name, the bare name after an `open`. A pattern `M.C` is not
/// committed by the walk at all, and an assembly container's contested names
/// decline.
fn control(cell: &Cell, use_: Use) -> bool {
    let project = !cell.container.is_assembly()
        && !matches!(
            cell.container,
            Container::EarlierSig | Container::EarlierSigHiding
        );
    match use_ {
        Use::TypeQual => cell.setup.extra != Extra::PrivateFirst,
        Use::QualExpr => {
            project
                && !matches!(
                    cell.setup.extra,
                    Extra::PrivateFirst
                        | Extra::PrivateReprFirst
                        | Extra::ExternLast
                        | Extra::RqaFirst
                        | Extra::AbbrevCaseFirst
                )
        }
        Use::BareExpr | Use::BarePat => project && cell.setup.extra == Extra::None,
        Use::QualPat => false,
    }
}

/// Every use in the grid agrees with FCS wherever we commit, and every
/// control use commits.
#[test]
fn every_qualified_case_use_agrees_with_fcs() {
    let all = cells();
    let fsharp_core = ensure_fsharp_core_dll();
    let fixture = ensure_qualified_case_fixture_built();
    let env = env_of(&[fsharp_core.as_path(), fixture]);

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
                    let (root, written) = temp_fs_tree("qualified_case_grid", &rel);
                    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
                    let json = invoke_fcs_dump_project_with_refs(&paths, &[fixture]);
                    let fcs = parse_fcs_uses_project(&json, &written);
                    let _ = std::fs::remove_dir_all(&root);
                    *answers[i].lock().unwrap() = Some((written, fcs));
                }
            });
        }
    });

    let mut failures = Vec::new();
    let mut entries = Vec::new();
    // Non-vacuity: commits to a case of a union other than the latest, the
    // reading latest-write-wins cannot give.
    let mut committed_earlier_union = 0;
    for (cell, answer) in all.iter().zip(answers) {
        let (written, fcs) = answer.into_inner().unwrap().expect("every cell was asked");
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
        let proj = resolve_project_files(&project(&rel), &env);
        if errors.is_empty() {
            failures.extend(unmodelled_project_binder_failures(
                &cell.label,
                &proj,
                &written,
                &fcs,
            ));
        }
        let file_paths: Vec<PathBuf> = written.iter().map(|(p, _)| p.clone()).collect();
        let clean = if errors.is_empty() {
            "clean"
        } else {
            "erroring"
        };
        let probe_src = &cell.files.last().expect("the probe file").1;
        for &(use_, probe) in &cell.probes {
            let theirs = fcs_at(fu, probe);
            let rendered = render_fcs(theirs);
            let label = format!("{} / {use_:?}", cell.label);
            let committed = |range: TextRange| {
                proj.file(probe_idx)
                    .resolution_at(range)
                    .filter(|r| !matches!(r, Resolution::Deferred(_) | Resolution::Unresolved))
            };
            // The whole probe, and a dotted probe's last segment, which carries
            // a record of its own (FCS keys the access by the whole path).
            let text = &probe_src[usize::from(probe.start())..usize::from(probe.end())];
            let mut ranges = vec![probe];
            if let Some(dot) = text.rfind('.') {
                ranges.push(TextRange::new(
                    probe.start() + rowan::TextSize::from(u32::try_from(dot + 1).unwrap()),
                    probe.end(),
                ));
            }
            let mut verdict = "decline";
            for range in ranges {
                let Some(res) = committed(range) else {
                    continue;
                };
                let at = fcs_at(fu, range).or(theirs);
                if at.is_some_and(|u| agrees(&env, &proj, &file_paths, probe_idx, res, u)) {
                    verdict = "commit";
                    let latest = format!("GridU{}.GridC", cell.setup.shapes.len());
                    if range == probe
                        && rendered.contains(".GridC")
                        && rendered.contains("GridU")
                        && !rendered.contains(&latest)
                    {
                        committed_earlier_union += 1;
                    }
                } else {
                    verdict = "WRONG";
                    failures.push(format!(
                        "  {label} [WRONG]\n    FCS:  {}\n    ours: {res:?}\n    errors: {errors:?}\n    files: {:?}",
                        render_fcs(at),
                        cell.files
                    ));
                }
            }
            if verdict == "decline" && control(cell, use_) {
                failures.push(format!(
                    "  {label} [MUST COMMIT]\n    FCS:  {rendered}\n    ours: {:?}\n    files: {:?}",
                    proj.file(probe_idx).decline_site(probe),
                    cell.files
                ));
            }
            entries.push(format!(
                "{label} | fcs {rendered} ({clean}) | ours {verdict}"
            ));
        }
    }
    if !failures.is_empty() {
        for e in &entries {
            eprintln!("use: {e}");
        }
    }
    assert!(
        failures.is_empty(),
        "{} disagreements with FCS over {} cells:\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
    assert!(
        committed_earlier_union > 0,
        "the grid must commit to an earlier union's case somewhere"
    );
    let manifest_entries = Manifest::from_entries(entries).expect("manifest entries");
    check_manifest(
        "qualified_case_precedence_grid",
        &manifest_entries,
        &regenerate("qualified_case_precedence_grid::every_qualified_case_use_agrees_with_fcs"),
    );
}

/// A possible case (`type Hidden = GridC`) the implementation declares
/// ahead of the union its signature exposes orders nothing: FCS reads the
/// signature's module type, which has no `Hidden`, so `GridLib.GridC` is the
/// signature's `GridU.GridC`, and the walk commits it.
#[test]
fn a_signature_hidden_possible_case_contests_nothing() {
    let sig = "module GridLib\n\ntype GridU =\n    | GridC\n    | GridD\n";
    let imp = "module GridLib\n\ntype Hidden = GridC\ntype GridU =\n    | GridC\n    | GridD\n";
    let probe = "module GridProbe\n\nlet probe = GridLib.GridC\n";
    let files = [
        ("GridLib.fsi", sig),
        ("GridLib.fs", imp),
        ("GridProbe.fs", probe),
    ];
    crate::union_rqa_attribute_grid::project_probe_agrees(&files, "GridLib.GridC")
        .expect("agrees with FCS");
    let env = env_of(&[ensure_fsharp_core_dll().as_path()]);
    let proj = resolve_project_files(&project(&files), &env);
    let probed = range(
        probe.rfind("GridLib.GridC").expect("the probe"),
        "GridLib.GridC".len(),
    );
    assert!(
        matches!(
            proj.file(2).resolution_at(probed),
            Some(Resolution::Item(_))
        ),
        "the signature's case is committed: {:?}",
        proj.file(2).decline_site(probed)
    );
}
