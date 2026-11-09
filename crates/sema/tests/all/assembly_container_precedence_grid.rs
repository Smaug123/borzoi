//! The **assembly container precedence grid**: which of several members named
//! `GridC` a referenced assembly's module or namespace gives `M.GridC` in an
//! expression, graded against FCS.
//!
//! FCS reads a name inside a module or namespace through
//! `ResolveExprLongIdentInModuleOrNamespace` (`NameResolution.fs`), which asks
//! the container, in order, for
//!
//! 1. an accessible **value** (`AllValsByLogicalName` — an active pattern's
//!    logical name is `|GridC|_|`, so it never answers here);
//! 2. an accessible **exception**;
//! 3. the case of the **first** entity declaring a case of the name
//!    (`TryFindTypeWithUnionCase`, which does not look at accessibility): taken
//!    when its representation is accessible and it is not
//!    `[<RequireQualifiedAccess>]`;
//! 4. a **type** of the name;
//! 5. with a tail, a **module** of the name;
//! 6. the `[<RequireQualifiedAccess>]` case kept from step 3.
//!
//! A type of the name therefore loses to a value, an exception or a plain
//! union case beside it, whatever their order. The grid crosses:
//!
//! - the **members** a container declares under the name, and their order:
//!   values (public and `private`), active patterns, exceptions, unions
//!   declaring the case (plain, `[<RequireQualifiedAccess>]`, with a
//!   `private` representation, `private`), a union named like its case,
//!   classes, records, abbreviations and modules;
//! - the **container**: a module, or a namespace (no values there);
//! - the **use**: the fully-qualified path, the path shortened by an `open`
//!   of the enclosing namespace, the path with a member tail (`.Make ()`),
//!   the bare name after an `open` of the container — a different rule (the
//!   name environment's latest write), graded only for certain-implies-exact —
//!   and, for a module, the path shortened by `open`s of two namespaces that
//!   each declare a module of its name, the later one the setup's: where the
//!   setup's module reads nothing, FCS goes on to the other.
//!
//! Every use is certain-implies-exact; the control uses must commit, so the
//! grid cannot pass by declining; FCS's reading of every in-container use is
//! checked against the order above, so a premise the fixture does not
//! actually set up fails rather than grading the wrong question; and every
//! use's two answers are pinned in
//! `tests/manifests/assembly_container_precedence_grid.txt`.
//!
//! The fixture assembly is generated from [`setups`]
//! (`tests/fixtures/container_precedence_env`).

use std::path::{Path, PathBuf};

use crate::common::corpus_manifest::{check_manifest, regenerate};
use crate::common::{
    FileUses, ensure_container_precedence_fixture_built, ensure_fsharp_core_dll,
    invoke_fcs_dump_project_with_refs, parse_fcs_uses_project, temp_fs_tree,
};
use crate::resolve_signatures::project;
use crate::unmodelled_contributor_grid::{agrees, env_of, fcs_at, render_fcs};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_sema::{Resolution, resolve_project_files};
use rowan::{TextRange, TextSize};

/// The fixture assembly's simple name.
pub(crate) const FIXTURE_ASM: &str = "SemaContainerPrecedenceFixture";

/// How a union declaring the case `GridC` is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnionKind {
    /// `| GridC of int`.
    Unary,
    /// `| GridC`.
    Nullary,
    /// `[<RequireQualifiedAccess>]`.
    Rqa,
    /// `type GridU = private | GridC | …`: FCS finds the case and stops, but
    /// cannot read it.
    PrivateRepr,
    /// `type private GridU`.
    Private,
}

/// One member of a container named — or declaring a case named — `GridC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Member {
    /// `let GridC = 1`.
    Val,
    /// `let private GridC = 2`.
    PrivateVal,
    /// `let (|GridC|_|) …`.
    ActivePattern,
    /// A union `GridU{n}` declaring the case `GridC`.
    Union(UnionKind),
    /// `exception GridC of int`.
    Exn,
    /// `type GridC() = static member Make () = 1`.
    Class,
    /// `type GridC = { GridX: int } with static member Make`.
    Record,
    /// `type GridC = System.Version`.
    Abbrev,
    /// `type GridC = | GridC | GridE`: a union whose case shares its name.
    SelfUnion,
    /// `module GridC = let Make () = 1`.
    SubModule,
}

impl Member {
    fn fits_namespace(self) -> bool {
        !matches!(
            self,
            Member::Val
                | Member::PrivateVal
                | Member::ActivePattern
                | Member::Union(UnionKind::Private)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    Module,
    Namespace,
}

/// One container and the members it declares under the name, in order.
struct Setup {
    container: Container,
    members: &'static [Member],
}

use Member::*;
use UnionKind::*;

/// Every setup the fixture declares, each in a namespace of its own.
fn setups() -> Vec<Setup> {
    let module = |members| Setup {
        container: Container::Module,
        members,
    };
    let namespace = |members| Setup {
        container: Container::Namespace,
        members,
    };
    let out = vec![
        // Controls: one member of the name.
        module(&[Class]),
        module(&[Val]),
        module(&[SubModule]),
        module(&[Union(Unary)]),
        module(&[Exn]),
        module(&[SelfUnion]),
        module(&[Record]),
        module(&[Abbrev]),
        // The issue's shape (#498), and its neighbours: a plain case beside a
        // type, in either order.
        module(&[Union(Unary), Union(Nullary), Class]),
        module(&[Union(Nullary), Class]),
        module(&[Class, Union(Unary)]),
        module(&[Union(Unary), Record]),
        module(&[Union(Unary), Abbrev]),
        module(&[Union(Unary), SubModule]),
        module(&[SubModule, Union(Nullary)]),
        // A value takes the name ahead of a type, a module and a case.
        module(&[Val, Class]),
        module(&[Class, Val]),
        module(&[Val, SubModule]),
        module(&[Val, Union(Nullary)]),
        module(&[Union(Unary), Val]),
        // …unless it is out of reach.
        module(&[PrivateVal, Class]),
        // An active pattern answers nothing here.
        module(&[ActivePattern, Class]),
        module(&[Class, ActivePattern]),
        // An exception takes the name ahead of a case.
        module(&[Union(Unary), Exn]),
        module(&[Exn, Union(Nullary)]),
        // A case FCS reads past: the first union declaring it requires
        // qualified access, or hides its representation, or is private.
        module(&[Union(Rqa), Class]),
        module(&[Class, Union(Rqa)]),
        module(&[Union(Rqa), Union(Nullary), Class]),
        module(&[Union(Nullary), Union(Rqa), Class]),
        module(&[Union(Rqa), Record]),
        module(&[Union(Rqa), SubModule]),
        module(&[Union(PrivateRepr), Class]),
        module(&[Union(PrivateRepr), Union(Nullary), Class]),
        module(&[Union(Private), Class]),
        module(&[Union(Private), Union(Nullary), Class]),
        // An inaccessible first union, then a type that is no value: the
        // module reads nothing, though a public union declares the case.
        module(&[Union(PrivateRepr), Union(Nullary), Record]),
        module(&[Union(Private), Union(Nullary), Record]),
        // The same shapes directly in a namespace.
        namespace(&[Class]),
        namespace(&[SubModule]),
        namespace(&[Union(Unary)]),
        namespace(&[Exn]),
        namespace(&[SelfUnion]),
        namespace(&[Union(Unary), Union(Nullary), Class]),
        namespace(&[Class, Union(Unary)]),
        namespace(&[Union(Unary), Record]),
        namespace(&[Union(Unary), Abbrev]),
        namespace(&[Union(Unary), SubModule]),
        namespace(&[Union(Unary), Exn]),
        namespace(&[Union(Rqa), Class]),
        namespace(&[Union(Rqa), Union(Nullary), Class]),
        namespace(&[Union(Rqa), SubModule]),
        namespace(&[Union(PrivateRepr), Class]),
    ];
    for s in &out {
        assert!(
            s.container == Container::Module || s.members.iter().all(|m| m.fits_namespace()),
            "{:?} cannot sit in a namespace",
            s.members
        );
    }
    out
}

/// The namespace holding setup `k`: its module `GridLib`, or its members.
fn setup_namespace(k: usize) -> String {
    format!("AqFix.S{k}")
}

/// The source of one member, unindented.
fn member_source(member: Member, union_no: usize) -> String {
    let union = |attr: &str, access: &str, repr: &str, case: &str| {
        format!("{attr}type {access}GridU{union_no} =\n    {repr}| {case}\n    | GridD{union_no}\n")
    };
    match member {
        Val => "let GridC = 1\n".to_string(),
        PrivateVal => "let private GridC = 2\n".to_string(),
        ActivePattern => "let (|GridC|_|) (x: int) = if x > 0 then Some x else None\n".to_string(),
        Union(Unary) => union("", "", "", "GridC of int"),
        Union(Nullary) => union("", "", "", "GridC"),
        Union(Rqa) => union("[<RequireQualifiedAccess>]\n", "", "", "GridC"),
        Union(PrivateRepr) => union("", "", "private ", "GridC"),
        Union(Private) => union("", "private ", "", "GridC"),
        Exn => "exception GridC of int\n".to_string(),
        Class => "type GridC() =\n    static member Make () = 1\n".to_string(),
        Record => "type GridC =\n    { GridX: int }\n    static member Make () = 1\n".to_string(),
        Abbrev => "type GridC = System.Version\n".to_string(),
        SelfUnion => "type GridC =\n    | GridC\n    | GridE\n".to_string(),
        SubModule => "module GridC =\n    let Make () = 1\n".to_string(),
    }
}

/// The source of the fixture assembly (`tests/fixtures/container_precedence_env`).
pub(crate) fn fixture_source() -> String {
    let mut src = String::new();
    for (k, setup) in setups().iter().enumerate() {
        src.push_str(&format!("namespace {}\n\n", setup_namespace(k)));
        let indent = match setup.container {
            Container::Module => {
                src.push_str("module GridLib =\n");
                "    "
            }
            Container::Namespace => "",
        };
        let mut unions = 0;
        for &member in setup.members {
            if matches!(member, Union(_)) {
                unions += 1;
            }
            for line in member_source(member, unions).lines() {
                src.push_str(indent);
                src.push_str(line);
                src.push('\n');
            }
        }
        src.push('\n');
        if setup.container == Container::Module {
            src.push_str(&format!(
                "namespace {}\n\nmodule GridLib =\n    let GridC = 42\n\n",
                fallback_namespace(k)
            ));
        }
    }
    src
}

/// The namespace holding setup `k`'s fallback: a module `GridLib` of its own
/// with a value `GridC`, which a [`Use::Fallback`] probe opens ahead of the
/// setup's.
fn fallback_namespace(k: usize) -> String {
    format!("AqFix.L{k}")
}

/// How a probe writes the name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Use {
    /// `AqFix.S{k}.GridLib.GridC` / `AqFix.S{k}.GridC`.
    Qual,
    /// The same, shortened by `open AqFix`.
    OpenQual,
    /// The fully-qualified path with a member tail, applied: `….GridC.Make ()`.
    Tail,
    /// `GridC` after an `open` of the container.
    Bare,
    /// `GridLib.GridC` after an `open` of the fallback namespace and then of
    /// the setup's: where the setup's module reads nothing, FCS goes on to the
    /// fallback's `GridLib`. Modules only.
    Fallback,
}

impl Use {
    const ALL: [Use; 5] = [
        Use::Qual,
        Use::OpenQual,
        Use::Tail,
        Use::Bare,
        Use::Fallback,
    ];

    /// Whether FCS reads it through `ResolveExprLongIdentInModuleOrNamespace`.
    fn in_container(self) -> bool {
        self != Use::Bare
    }
}

/// What FCS's in-container search reads, by the order in the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    Value,
    Exception,
    /// The case of the `n`th union (1-based; 0 is a [`Member::SelfUnion`]).
    Case(usize),
    /// A type of the name: its constructor, or its static `Make`.
    Type,
    /// The module of the name's `Make`.
    Module,
    /// Nothing reads the name.
    Nothing,
    /// The fallback module's value, read where the setup's module reads
    /// nothing.
    Fallback,
}

/// FCS's reading of `use_` in setup `setup`.
///
/// Where the setup's container reads nothing accessible, FCS goes on to the
/// fallback module if the use has one; if not, it searches again ignoring
/// accessibility (`AccessibleFromSomeFSharpCode`), and reports an inaccessible
/// first union's case with an error.
fn expected(setup: &Setup, use_: Use) -> Reading {
    match fcs_reading(setup, use_ == Use::Tail) {
        Reading::Nothing if use_ == Use::Fallback => Reading::Fallback,
        Reading::Nothing => first_declaring_union(setup)
            .filter(|&(_, kind)| matches!(kind, UnionKind::PrivateRepr | UnionKind::Private))
            .map_or(Reading::Nothing, |(n, _)| Reading::Case(n)),
        reading => reading,
    }
}

/// The first union of setup `setup` declaring the case, numbered as
/// [`Reading::Case`] numbers it, and how it is declared.
fn first_declaring_union(setup: &Setup) -> Option<(usize, UnionKind)> {
    setup.members.iter().find_map(|m| match m {
        Union(kind) => Some((1, *kind)),
        SelfUnion => Some((0, UnionKind::Nullary)),
        _ => None,
    })
}

/// FCS's reading of setup `setup` through `ResolveExprLongIdentInModuleOrNamespace`,
/// with a tail when `tail`. The fixture's premise: the grid asserts FCS
/// agrees with it.
fn fcs_reading(setup: &Setup, tail: bool) -> Reading {
    let members = setup.members;
    if members.contains(&Val) {
        return Reading::Value;
    }
    if members.contains(&Exn) {
        return Reading::Exception;
    }
    // The first entity declaring the case, and whether it is read here or
    // kept for the last step.
    let mut rqa_case = None;
    let mut unions = 0;
    for &m in members {
        let declares = match m {
            Union(_) => {
                unions += 1;
                Some(unions)
            }
            SelfUnion => Some(0),
            _ => None,
        };
        if let Some(n) = declares {
            match m {
                Union(Rqa) => rqa_case = Some(n),
                Union(PrivateRepr | Private) => {}
                _ => return Reading::Case(n),
            }
            break;
        }
    }
    let type_reads = members.iter().any(|m| match m {
        Class => true,
        // A record has no constructor to read, but its static member does.
        Record => tail,
        // `System.Version` has a constructor, but no `Make`.
        Abbrev => !tail,
        _ => false,
    });
    if type_reads {
        return Reading::Type;
    }
    if tail && members.contains(&SubModule) {
        return Reading::Module;
    }
    match rqa_case {
        Some(n) => Reading::Case(n),
        None => Reading::Nothing,
    }
}

/// The uses we must commit: a value (FCS reads it first, whatever else the
/// module declares); a class, and a module's member, wherever nothing that is
/// not out of reach can read the name ahead of them; and an exception alone.
fn control(setup: &Setup, use_: Use) -> bool {
    if !use_.in_container() {
        return false;
    }
    let members = setup.members;
    let unopposed = |kind: Member| {
        members.contains(&kind)
            && members.iter().all(|m| {
                *m == kind
                    || matches!(
                        m,
                        PrivateVal
                            | ActivePattern
                            | Union(Rqa)
                            | Union(Private)
                            | Union(PrivateRepr)
                    )
            })
    };
    match expected(setup, use_) {
        Reading::Value => true,
        // The setup's module is no reading at all, and nothing in it is
        // uncertain.
        Reading::Fallback => matches!(members, [Record] | [SubModule]),
        Reading::Type => unopposed(Class),
        Reading::Module => unopposed(SubModule),
        Reading::Exception => use_ != Use::Tail && members.iter().all(|m| *m == Exn),
        _ => false,
    }
}

/// Whether FCS's record `rendered` is the reading `reading` of setup `k`.
fn rendered_is(rendered: &str, k: usize, setup: &Setup, reading: Reading) -> bool {
    let container = match setup.container {
        Container::Module => format!("{}.GridLib", setup_namespace(k)),
        Container::Namespace => setup_namespace(k),
    };
    let asm = FIXTURE_ASM;
    // FCS renders a module value, and a module's function, as a `member`; an
    // exception as a `type`.
    match reading {
        Reading::Value => rendered == format!("{asm}:{container}.GridC:member"),
        Reading::Exception => rendered == format!("{asm}:{container}.GridC:type"),
        Reading::Case(0) => rendered == format!("{asm}:{container}.GridC.GridC:unioncase"),
        Reading::Case(n) => rendered == format!("{asm}:{container}.GridU{n}.GridC:unioncase"),
        Reading::Type => {
            rendered == format!("{asm}:{container}.GridC:ctor")
                || rendered == format!("{asm}:{container}.GridC.Make:member")
                || rendered == "System.Runtime:System.Version:ctor"
        }
        Reading::Module => rendered == format!("{asm}:{container}.GridC.Make:member"),
        Reading::Nothing => rendered == "-",
        Reading::Fallback => {
            rendered == format!("{asm}:{}.GridLib.GridC:member", fallback_namespace(k))
        }
    }
}

/// One probe file: its path, source, and the span of the written name.
struct Probe {
    setup: usize,
    use_: Use,
    path: String,
    src: String,
    span: TextRange,
}

fn probes(setups: &[Setup]) -> Vec<Probe> {
    let mut out = Vec::new();
    for (k, setup) in setups.iter().enumerate() {
        let ns = setup_namespace(k);
        let container = match setup.container {
            Container::Module => format!("{ns}.GridLib"),
            Container::Namespace => ns.clone(),
        };
        let short = match setup.container {
            Container::Module => "GridLib".to_string(),
            Container::Namespace => format!("S{k}"),
        };
        for use_ in Use::ALL {
            if use_ == Use::Fallback && setup.container != Container::Module {
                continue;
            }
            let (open, written, applied) = match use_ {
                Use::Qual => (None, format!("{container}.GridC"), ""),
                Use::OpenQual => {
                    let opened = match setup.container {
                        Container::Module => ns.clone(),
                        Container::Namespace => "AqFix".to_string(),
                    };
                    (Some(opened), format!("{short}.GridC"), "")
                }
                Use::Tail => (None, format!("{container}.GridC.Make"), " ()"),
                Use::Bare => (Some(container.clone()), "GridC".to_string(), ""),
                Use::Fallback => (
                    Some(format!("{}\nopen {ns}", fallback_namespace(k))),
                    "GridLib.GridC".to_string(),
                    "",
                ),
            };
            let module = format!("AqProbe{k}{use_:?}");
            let mut src = format!("module {module}\n\n");
            if let Some(open) = open {
                src.push_str(&format!("open {open}\n\n"));
            }
            src.push_str("let probe = ");
            let start = src.len();
            src.push_str(&written);
            let end = src.len();
            src.push_str(applied);
            src.push('\n');
            out.push(Probe {
                setup: k,
                use_,
                path: format!("{module}.fs"),
                src,
                span: TextRange::new(
                    TextSize::from(u32::try_from(start).unwrap()),
                    TextSize::from(u32::try_from(end).unwrap()),
                ),
            });
        }
    }
    out
}

/// Every contiguous run of the path's segments, as a range: where either side
/// may record a symbol.
fn segment_runs(src: &str, span: TextRange) -> Vec<TextRange> {
    let start = usize::from(span.start());
    let text = &src[start..usize::from(span.end())];
    let mut bounds = Vec::new();
    let mut seg_start = 0;
    for (i, c) in text.char_indices() {
        if c == '.' {
            bounds.push((seg_start, i));
            seg_start = i + 1;
        }
    }
    bounds.push((seg_start, text.len()));
    let at = |off: usize| TextSize::from(u32::try_from(start + off).unwrap());
    let mut out = Vec::new();
    for i in 0..bounds.len() {
        for j in i..bounds.len() {
            out.push(TextRange::new(at(bounds[i].0), at(bounds[j].1)));
        }
    }
    out
}

/// Every use agrees with FCS wherever we commit, every control commits, and
/// FCS's in-container readings are the ones the precedence order predicts.
#[test]
fn every_container_member_order_agrees_with_fcs() {
    let setups = setups();
    let probes = probes(&setups);
    let fsharp_core = ensure_fsharp_core_dll();
    let fixture = ensure_container_precedence_fixture_built();
    let env = env_of(&[fsharp_core.as_path(), fixture]);

    let rel: Vec<(&str, &str)> = probes
        .iter()
        .map(|p| (p.path.as_str(), p.src.as_str()))
        .collect();
    let (root, written) = temp_fs_tree("container_precedence_grid", &rel);
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project_with_refs(&paths, &[fixture]);
    let fcs = parse_fcs_uses_project(&json, &written);
    let _ = std::fs::remove_dir_all(&root);
    let file_paths: Vec<PathBuf> = written.iter().map(|(p, _)| p.clone()).collect();
    let proj = resolve_project_files(&project(&rel), &env);

    let mut failures = Vec::new();
    let mut entries = Vec::new();
    let mut commits = 0;
    for (idx, probe) in probes.iter().enumerate() {
        let setup = &setups[probe.setup];
        let label = format!(
            "S{} {:?} {:?} / {:?}",
            probe.setup, setup.container, setup.members, probe.use_
        );
        let fu: &FileUses = fcs
            .iter()
            .find(|f| f.path.file_name() == file_paths[idx].file_name())
            .unwrap_or_else(|| panic!("no FCS uses for {label}"));
        let theirs = fcs_at(fu, probe.span);
        // A tail's own record names `Make`; the reading of `GridC` sits on
        // the path without it.
        let read_span = if probe.use_ == Use::Tail {
            let text = &probe.src[usize::from(probe.span.start())..usize::from(probe.span.end())];
            let cut = text.rfind('.').expect("a tail");
            TextRange::new(
                probe.span.start(),
                probe.span.start() + TextSize::from(u32::try_from(cut).unwrap()),
            )
        } else {
            probe.span
        };
        let rendered = render_fcs(theirs);
        let read = render_fcs(fcs_at(fu, read_span));
        if probe.use_.in_container() {
            let expected = expected(setup, probe.use_);
            // The tail's `Make` is recorded over the whole path when a type or
            // module supplies it; anything else is read at the path without it.
            let shown = match expected {
                Reading::Type | Reading::Module if probe.use_ == Use::Tail => &rendered,
                _ => &read,
            };
            if !rendered_is(shown, probe.setup, setup, expected) {
                failures.push(format!(
                    "  {label} [PREMISE]\n    expected FCS to read {expected:?}\n    FCS:  {shown} (whole: {rendered})\n    errors: {:?}",
                    fu.errors
                ));
            }
        }

        let mut verdict = "decline";
        for range in segment_runs(&probe.src, probe.span) {
            let Some(res) = proj
                .file(idx)
                .resolution_at(range)
                .filter(|r| !matches!(r, Resolution::Deferred(_) | Resolution::Unresolved))
            else {
                continue;
            };
            // We record an entity at its own segment where FCS records the
            // name over the path ending there: the name's own segment grades
            // against that record. A qualifier segment we commit (a module)
            // carries no record of its own when FCS reads the path through it.
            let names_the_reading =
                range.end() == probe.span.end() || range.end() == read_span.end();
            let at = fcs_at(fu, range).or_else(|| {
                if range.end() == probe.span.end() {
                    theirs
                } else if range.end() == read_span.end() {
                    fcs_at(fu, read_span)
                } else {
                    None
                }
            });
            match at {
                Some(u) if agrees(&env, &proj, &file_paths, idx, res, u) => {
                    if names_the_reading && verdict != "WRONG" {
                        verdict = "commit";
                    }
                }
                None if !names_the_reading => {}
                // The partial reading's record of a type or module that is not
                // a value, where FCS rejects the use and reads nothing.
                None if probe.use_.in_container()
                    && expected(setup, probe.use_) == Reading::Nothing
                    && matches!(res, Resolution::Entity(_))
                    && !fu.errors.is_empty() =>
                {
                    if verdict == "decline" {
                        verdict = "entity on error";
                    }
                }
                _ => {
                    verdict = "WRONG";
                    failures.push(format!(
                        "  {label} [WRONG] at {range:?}\n    FCS:  {}\n    ours: {res:?}\n    errors: {:?}",
                        render_fcs(at),
                        fu.errors
                    ));
                }
            }
        }
        if verdict == "commit" {
            commits += 1;
        }
        if verdict == "decline" && control(setup, probe.use_) {
            failures.push(format!(
                "  {label} [MUST COMMIT]\n    FCS:  {rendered}\n    ours: {:?}",
                proj.file(idx).decline_site(probe.span)
            ));
        }
        let clean = if fu.errors.is_empty() {
            "clean"
        } else {
            "erroring"
        };
        entries.push(format!(
            "{label} | fcs {rendered} / {read} ({clean}) | ours {verdict}"
        ));
    }
    if !failures.is_empty() {
        for e in &entries {
            eprintln!("use: {e}");
        }
    }
    assert!(
        failures.is_empty(),
        "{} disagreements with FCS over {} probes:\n{}",
        failures.len(),
        probes.len(),
        failures.join("\n")
    );
    assert!(commits > 0, "the grid must commit somewhere");
    let manifest = Manifest::from_entries(entries).expect("manifest entries");
    check_manifest(
        "assembly_container_precedence_grid",
        &manifest,
        &regenerate(
            "assembly_container_precedence_grid::every_container_member_order_agrees_with_fcs",
        ),
    );
}
