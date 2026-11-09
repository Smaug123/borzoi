//! A generated, FCS-graded grid of **a union case and a type of the same name
//! in one namespace** (issue #497): `type U = X of unit | Y` and a type, module
//! or exception `X` declared in namespace `N`, in either order, then a bare `X`
//! read through `N`.
//!
//! FCS adds a namespace's contents to the name environment in a fixed order
//! (`AddModuleOrNamespaceContentsToNameEnv`, `NameResolution.fs`): its
//! exceptions, then its types in declaration order — each class, struct or
//! delegate type (an abbreviation of one included) entering the unqualified
//! slot as a constructor (`Item.UnqualifiedType`), each union's cases entering
//! it as cases — then its values, then its `[<AutoOpen>]` modules. The slot is
//! last-write-wins, so a later class takes `X` from an earlier case and a later
//! case takes it back. A pattern reads a separate table that no type enters,
//! and a type annotation reads the type tables, so neither contest exists
//! there.
//!
//! The axes:
//!
//! - the rival declaration of `X` ([`Kind`]): a class, struct, delegate,
//!   interface, record, union type or enum; an abbreviation of a class or of a
//!   record; a module; an exception;
//! - the declaration order ([`Order`]): the case first or the rival first, and
//!   two unions both declaring the case, with the rival last or between them;
//! - where the declarations sit relative to each other ([`Placement`]): one
//!   namespace block, one block each in a file, one file each, or one `and`
//!   group;
//! - where the probe reads them ([`Site`]): an `open N` from another file's
//!   module, the implicit open of `N` in a later file's `namespace N` block, or
//!   the declarations' own block;
//! - the probe: a bare expression, a pattern, an application `X ()`, a type
//!   annotation and, for a module, the dotted `X.v`.
//!
//! Issue #497's shape is the `caseCaseRival` order with a class: two unions
//! declaring the case, then the class, which binds a bare expression.
//!
//! The oracle is [`crate::common::use_grade`]'s: certain-implies-exact, and
//! [`must_commit`] keeps the grid from passing by declining everything. Sema
//! reads FSharp.Core, as FCS does, and each cell contests a name of its own,
//! `X{i}`.

use crate::common::fsharp_core_env;
use crate::common::use_grade::{
    FcsVerdict, FileBuilder, Graded, Site as Probe, assert_sound_and_live, grade_in,
};

/// The declaration contesting the union case's name `X`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// `type X() = class end`: takes the unqualified slot.
    Class,
    /// `type X = struct val xs: int end`: takes the unqualified slot.
    Struct,
    /// `type X = delegate of int -> int`: takes the unqualified slot.
    Delegate,
    /// `type X = interface end`.
    Interface,
    /// `type X = { xa: int }`.
    Record,
    /// `type X = XA | XB`.
    Union,
    /// `type X = XA = 0 | XB = 1`: a struct, so it takes the slot.
    Enum,
    /// `type X = Kn.K{i}` of a class: takes the slot as the class does.
    AbbrevClass,
    /// `type X = Kn.R{i}` of a record.
    AbbrevRecord,
    /// `module X = let v = 1`.
    Module,
    /// `exception X of unit`.
    Exception,
}

impl Kind {
    const ALL: [Kind; 11] = [
        Kind::Class,
        Kind::Struct,
        Kind::Delegate,
        Kind::Interface,
        Kind::Record,
        Kind::Union,
        Kind::Enum,
        Kind::AbbrevClass,
        Kind::AbbrevRecord,
        Kind::Module,
        Kind::Exception,
    ];

    fn tag(self) -> &'static str {
        match self {
            Kind::Class => "class",
            Kind::Struct => "struct",
            Kind::Delegate => "delegate",
            Kind::Interface => "interface",
            Kind::Record => "record",
            Kind::Union => "union",
            Kind::Enum => "enum",
            Kind::AbbrevClass => "abbrevClass",
            Kind::AbbrevRecord => "abbrevRecord",
            Kind::Module => "module",
            Kind::Exception => "exception",
        }
    }

    /// Whether the rival is a type, so a type annotation can name it and an
    /// `and` group can hold it.
    fn is_type(self) -> bool {
        !matches!(self, Kind::Module | Kind::Exception)
    }

    /// The rival's declaration of `x`, `and`-joined when `and` is set, for
    /// the cell numbered `i` (whose abbreviation targets are `Kn.K{i}` and
    /// `Kn.R{i}`).
    fn decl(self, x: &str, i: usize, and: bool) -> Vec<String> {
        let kw = if and { "and" } else { "type" };
        match self {
            Kind::Class => vec![format!("{kw} {x}() = class end")],
            Kind::Struct => vec![format!("{kw} {x} = struct val xs: int end")],
            Kind::Delegate => vec![format!("{kw} {x} = delegate of int -> int")],
            Kind::Interface => vec![format!("{kw} {x} = interface end")],
            Kind::Record => vec![format!("{kw} {x} = {{ xa: int }}")],
            Kind::Union => vec![format!("{kw} {x} = {x}A | {x}B")],
            Kind::Enum => vec![format!("{kw} {x} = {x}A = 0 | {x}B = 1")],
            Kind::AbbrevClass => vec![format!("{kw} {x} = Kn.K{i}")],
            Kind::AbbrevRecord => vec![format!("{kw} {x} = Kn.R{i}")],
            Kind::Module => vec![format!("module {x} ="), "    let v = 1".to_string()],
            Kind::Exception => vec![format!("exception {x} of unit")],
        }
    }
}

/// The order of the namespace's declarations of `X`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Order {
    /// `type U1 = X of unit | Y1`, then the rival.
    CaseRival,
    /// The rival, then `U1`.
    RivalCase,
    /// `U1`, `U2` (both declaring `X`), then the rival.
    CaseCaseRival,
    /// `U1`, the rival, then `U2`.
    CaseRivalCase,
    /// The rival alone: the control for what the case changes.
    RivalOnly,
}

/// One declaration of the namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decl {
    /// `type U{n} = X of unit | Y{n}`.
    Case(usize),
    Rival,
}

impl Order {
    const ALL: [Order; 5] = [
        Order::CaseRival,
        Order::RivalCase,
        Order::CaseCaseRival,
        Order::CaseRivalCase,
        Order::RivalOnly,
    ];

    fn tag(self) -> &'static str {
        match self {
            Order::CaseRival => "caseRival",
            Order::RivalCase => "rivalCase",
            Order::CaseCaseRival => "caseCaseRival",
            Order::CaseRivalCase => "caseRivalCase",
            Order::RivalOnly => "rivalOnly",
        }
    }

    fn decls(self) -> Vec<Decl> {
        match self {
            Order::CaseRival => vec![Decl::Case(1), Decl::Rival],
            Order::RivalCase => vec![Decl::Rival, Decl::Case(1)],
            Order::CaseCaseRival => vec![Decl::Case(1), Decl::Case(2), Decl::Rival],
            Order::CaseRivalCase => vec![Decl::Case(1), Decl::Rival, Decl::Case(2)],
            Order::RivalOnly => vec![Decl::Rival],
        }
    }
}

/// What holds the declarations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    /// Namespace `N{i}` itself.
    Namespace,
    /// `module M` of namespace `N{i}`: one fragment, read through `open
    /// N{i}.M`.
    Module,
}

impl Container {
    const ALL: [Container; 2] = [Container::Namespace, Container::Module];

    fn tag(self) -> &'static str {
        match self {
            Container::Namespace => "namespace",
            Container::Module => "module",
        }
    }

    /// A module is one fragment, read only through an `open`.
    fn applies_to(self, placement: Placement, site: Site) -> bool {
        self == Container::Namespace
            || (matches!(placement, Placement::OneBlock | Placement::AndGroup)
                && site == Site::Open)
    }
}

/// How the namespace's declarations are split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// All in one `namespace N` block.
    OneBlock,
    /// Each in its own `namespace N` block of one file.
    Blocks,
    /// Each in its own file.
    Files,
    /// One `type … and …` group.
    AndGroup,
}

impl Placement {
    const ALL: [Placement; 4] = [
        Placement::OneBlock,
        Placement::Blocks,
        Placement::Files,
        Placement::AndGroup,
    ];

    fn tag(self) -> &'static str {
        match self {
            Placement::OneBlock => "oneBlock",
            Placement::Blocks => "blocks",
            Placement::Files => "files",
            Placement::AndGroup => "andGroup",
        }
    }
}

/// Where the probe reads the namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Site {
    /// `open N{i}` in a module of a later file's top-level module.
    Open,
    /// A module of a later file's `namespace N{i}` block: the block's implicit
    /// open of its namespace.
    Implicit,
    /// A module of the declarations' own block, in the probe file.
    OwnBlock,
}

impl Site {
    const ALL: [Site; 3] = [Site::Open, Site::Implicit, Site::OwnBlock];

    fn tag(self) -> &'static str {
        match self {
            Site::Open => "open",
            Site::Implicit => "implicit",
            Site::OwnBlock => "ownBlock",
        }
    }
}

/// The generated files: three of declarations, a top-level module of `open`
/// probes, and a file of namespace blocks for the implicit and own-block
/// probes.
struct Files {
    defs: [FileBuilder; 3],
    opens: FileBuilder,
    blocks: FileBuilder,
}

/// The declaration's lines, for the contested name `x`; `and` continues an
/// `and` group.
fn decl_lines(decl: Decl, kind: Kind, x: &str, i: usize, and: bool) -> Vec<String> {
    match decl {
        Decl::Case(n) => {
            let kw = if and { "and" } else { "type" };
            vec![format!("{kw} U{n} = {x} of unit | Y{n}")]
        }
        Decl::Rival => kind.decl(x, i, and),
    }
}

/// Append the probes of `x` at indentation `ind`.
fn probes(
    f: &mut FileBuilder,
    sites: &mut Vec<Probe>,
    kind: Kind,
    x: &str,
    ind: &str,
    label: &str,
) {
    f.probe(
        &format!("{ind}let v = {x}"),
        x,
        format!("{label}/expr"),
        sites,
    );
    f.probe(
        &format!("{ind}let p = match Unchecked.defaultof<_> with {x} _ -> 0 | _ -> 1"),
        x,
        format!("{label}/pat"),
        sites,
    );
    f.probe(
        &format!("{ind}let a = {x} ()"),
        x,
        format!("{label}/applied"),
        sites,
    );
    if kind.is_type() {
        f.probe(
            &format!("{ind}let f (q: {x}) = q"),
            x,
            format!("{label}/annotation"),
            sites,
        );
    }
    if kind == Kind::Module {
        let dotted = format!("{x}.v");
        f.probe(
            &format!("{ind}let w = {dotted}"),
            &dotted,
            format!("{label}/dotted"),
            sites,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn cell(
    i: usize,
    container: Container,
    kind: Kind,
    order: Order,
    placement: Placement,
    site: Site,
    files: &mut Files,
    sites: &mut Vec<Probe>,
) {
    let ns = format!("N{i}");
    // The contested name is the cell's own: sema declines a bare type name
    // another cell's type reuses, which would hide the annotation probes.
    let x = format!("X{i}");
    let label = format!(
        "{}/{}/{}/{}/{}",
        container.tag(),
        kind.tag(),
        order.tag(),
        placement.tag(),
        site.tag()
    );
    if matches!(kind, Kind::AbbrevClass | Kind::AbbrevRecord) {
        files.defs[0].line("namespace Kn");
        files.defs[0].line(&format!("type K{i}() = class end"));
        files.defs[0].line(&format!("type R{i} = {{ ra: int }}"));
    }
    let decls = order.decls();
    // Each declaration's block: (file, lines). The own-block site writes the
    // last block into the probe file, where the probe follows it.
    let mut blocks: Vec<(usize, Vec<String>)> = Vec::new();
    match placement {
        Placement::OneBlock | Placement::AndGroup => {
            let and = placement == Placement::AndGroup;
            let lines = decls
                .iter()
                .enumerate()
                .flat_map(|(k, d)| decl_lines(*d, kind, &x, i, and && k > 0))
                .collect();
            blocks.push((0, lines));
        }
        Placement::Blocks => {
            for d in &decls {
                blocks.push((0, decl_lines(*d, kind, &x, i, false)));
            }
        }
        Placement::Files => {
            for (k, d) in decls.iter().enumerate() {
                blocks.push((k, decl_lines(*d, kind, &x, i, false)));
            }
        }
    }
    let own = if site == Site::OwnBlock {
        blocks.pop()
    } else {
        None
    };
    for (file, lines) in blocks {
        let f = &mut files.defs[file];
        f.line(&format!("namespace {ns}"));
        let ind = match container {
            Container::Namespace => "",
            Container::Module => {
                f.line("module M =");
                "    "
            }
        };
        for l in lines {
            f.line(&format!("{ind}{l}"));
        }
    }
    match site {
        Site::Open => {
            let f = &mut files.opens;
            f.line(&format!("module C{i} ="));
            match container {
                Container::Namespace => f.line(&format!("    open {ns}")),
                Container::Module => f.line(&format!("    open {ns}.M")),
            }
            probes(f, sites, kind, &x, "    ", &label);
        }
        Site::Implicit | Site::OwnBlock => {
            let f = &mut files.blocks;
            f.line(&format!("namespace {ns}"));
            if let Some((_, lines)) = own {
                for l in lines {
                    f.line(&l);
                }
            }
            f.line("module P =");
            probes(f, sites, kind, &x, "    ", &label);
        }
    }
}

/// The cells: every container, kind, order, placement and site, except an
/// `and` group of a rival that is no type, an own-block probe of declarations
/// split across files (whose last block is then the only one in the probe's
/// file, which the `blocks` placement already covers), and a module split
/// across fragments or read other than through an `open`.
fn cells() -> Vec<(Container, Kind, Order, Placement, Site)> {
    let mut out = Vec::new();
    for container in Container::ALL {
        for kind in Kind::ALL {
            for order in Order::ALL {
                for placement in Placement::ALL {
                    if placement == Placement::AndGroup && !kind.is_type() {
                        continue;
                    }
                    for site in Site::ALL {
                        if (site == Site::OwnBlock && placement == Placement::Files)
                            || !container.applies_to(placement, site)
                        {
                            continue;
                        }
                        out.push((container, kind, order, placement, site));
                    }
                }
            }
        }
    }
    out
}

/// Whether FCS's declaration is a union case of one of the `U{n}` unions:
/// the declaration's line is `type U{n} = …` or `and U{n} = …`.
fn binds_a_case(files: &[(&str, String)], fcs: &FcsVerdict) -> bool {
    let FcsVerdict::Resolved(file, range) = fcs else {
        return false;
    };
    let text = &files[*file].1;
    let start = usize::from(range.start());
    let line_start = text[..start].rfind('\n').map_or(0, |n| n + 1);
    let line = text[line_start..].lines().next().unwrap_or("").trim_start();
    line.starts_with("type U") || line.starts_with("and U")
}

/// The sites that must commit FCS's answer: every site FCS binds cleanly to
/// a union case, a type or a module, except the classes below, each selected
/// by its axes and each declining for a reason the case-against-type order
/// does not decide. A class-like rival's constructor (`Item.UnqualifiedType`)
/// is never required: sema does not model it.
///
/// - A module container's case over a class-like rival declared before it
///   (`module`, a slot-taking kind, `expr`/`applied`): the module's
///   constructible type is a contributor the open's fold does not model, so
///   it marks its name after the fold, over the module's cases in whatever
///   order they were declared.
/// - A case under a later abbreviation of a record (`abbrevRecord`,
///   `caseRival`/`caseCaseRival`, `expr`/`applied`): whether an abbreviation
///   takes the slot depends on its target, which the walk does not chase, so
///   a later one leaves the slot unordered. An earlier one cannot win it.
/// - A type annotation read through an `open` (`open`, `annotation`), and a
///   module container's type annotation or dotted `X.v`: these decline
///   whether or not a case shares the name, as the `rivalOnly` rows show (a
///   type an earlier file declares, opened over, and an opened project
///   module's unmodelled contents).
fn must_commit<'g>(files: &[(&str, String)], graded: &'g [Graded]) -> Vec<&'g str> {
    graded
        .iter()
        .filter(|g| matches!(g.fcs, FcsVerdict::Resolved(..)))
        .filter(|g| {
            let parts: Vec<&str> = g.label.split('/').collect();
            let (container, kind, order, site, probe) =
                (parts[0], parts[1], parts[2], parts[4], parts[5]);
            let class_like = matches!(
                kind,
                "class" | "struct" | "delegate" | "enum" | "abbrevClass" | "abbrevRecord"
            );
            let value_probe = matches!(probe, "expr" | "applied");
            let excluded = (container == "module" && class_like && value_probe)
                || (kind == "abbrevRecord"
                    && matches!(order, "caseRival" | "caseCaseRival")
                    && value_probe)
                || (site == "open" && probe == "annotation")
                || (container == "module" && matches!(probe, "annotation" | "dotted"));
            let bound = match probe {
                "expr" | "pat" | "applied" => binds_a_case(files, &g.fcs),
                _ => true,
            };
            bound && !excluded
        })
        .map(|g| g.label.as_str())
        .collect()
}

#[test]
fn a_case_and_a_type_of_one_name_in_a_namespace_agree_with_fcs() {
    let mut files = Files {
        defs: [
            FileBuilder::new(0),
            FileBuilder::new(1),
            FileBuilder::new(2),
        ],
        opens: FileBuilder::new(3),
        blocks: FileBuilder::new(4),
    };
    files.opens.line("module Opens");
    let mut sites = Vec::new();
    for (i, (container, kind, order, placement, site)) in cells().into_iter().enumerate() {
        cell(
            i, container, kind, order, placement, site, &mut files, &mut sites,
        );
    }
    let [d0, d1, d2] = files.defs;
    let sources: Vec<(&str, String)> = vec![
        ("defs0.fs", d0.text),
        ("defs1.fs", d1.text),
        ("defs2.fs", d2.text),
        ("opens.fs", files.opens.text),
        ("blocks.fs", files.blocks.text),
    ];
    let graded = grade_in("cto", &sources, &sites, fsharp_core_env());
    let fcs_binds_case = |label: &str| {
        let g = graded
            .iter()
            .find(|g| g.label == label)
            .unwrap_or_else(|| panic!("{label} is not a site of the grid"));
        binds_a_case(&sources, &g.fcs)
    };
    // Non-vacuity: the declaration order decides the expression slot, both
    // ways, and never the pattern.
    for site in ["open", "implicit", "ownBlock"] {
        assert!(fcs_binds_case(&format!(
            "namespace/class/rivalCase/oneBlock/{site}/expr"
        )));
        assert!(!fcs_binds_case(&format!(
            "namespace/class/caseRival/oneBlock/{site}/expr"
        )));
        assert!(fcs_binds_case(&format!(
            "namespace/class/caseRival/oneBlock/{site}/pat"
        )));
    }
    let must = must_commit(&sources, &graded);
    for label in [
        "namespace/class/rivalCase/oneBlock/open/expr",
        "namespace/class/caseRivalCase/files/implicit/expr",
        "namespace/record/caseRival/blocks/open/expr",
        "namespace/class/caseRival/oneBlock/open/pat",
        "namespace/class/rivalOnly/files/implicit/annotation",
        "module/class/caseRival/oneBlock/open/pat",
    ] {
        assert!(must.contains(&label), "{label} is not a must-commit site");
    }
    assert_sound_and_live("case and type of one name", &graded, &[], &must);
}

/// The declaration order a signature gives: an `open` of a signatured
/// namespace or module reads the signature's contents, in the signature's
/// order, whatever order the implementation declares them in. Each cell
/// crosses the signature's order with the implementation's, for a class `X`
/// and a union case `X` in namespace `S{i}` or its module `S{i}.M`, read
/// through an `open` from a later file and (a namespace) through a later
/// `namespace S{i}` block's implicit open.
#[test]
fn a_signature_orders_the_case_and_the_class() {
    let mut sig = FileBuilder::new(0);
    let mut imp = FileBuilder::new(1);
    let mut later = FileBuilder::new(2);
    let mut blocks = FileBuilder::new(3);
    later.line("module Later");
    let mut sites = Vec::new();
    let mut i = 0;
    for module in [false, true] {
        for sig_case_first in [false, true] {
            for impl_case_first in [false, true] {
                let ns = format!("S{i}");
                let label = format!(
                    "{}/sig{}/impl{}",
                    if module { "module" } else { "namespace" },
                    if sig_case_first {
                        "CaseClass"
                    } else {
                        "ClassCase"
                    },
                    if impl_case_first {
                        "CaseClass"
                    } else {
                        "ClassCase"
                    },
                );
                let x = format!("X{i}");
                let case = format!("type U{i} = {x} of unit | Y{i}");
                let mut sig_decls = vec![
                    format!("type {x} ="),
                    "    class".to_string(),
                    format!("        new: unit -> {x}"),
                    "    end".to_string(),
                ];
                let mut imp_decls = vec![format!("type {x}() = class end")];
                if sig_case_first {
                    sig_decls.insert(0, case.clone());
                } else {
                    sig_decls.push(case.clone());
                }
                if impl_case_first {
                    imp_decls.insert(0, case.clone());
                } else {
                    imp_decls.push(case.clone());
                }
                for (f, decls) in [(&mut sig, sig_decls), (&mut imp, imp_decls)] {
                    f.line(&format!("namespace {ns}"));
                    let ind = if module {
                        f.line("module M =");
                        "    "
                    } else {
                        ""
                    };
                    for l in decls {
                        f.line(&format!("{ind}{l}"));
                    }
                }
                later.line(&format!("module C{i} ="));
                let opened = if module {
                    format!("{ns}.M")
                } else {
                    ns.clone()
                };
                later.line(&format!("    open {opened}"));
                probes(
                    &mut later,
                    &mut sites,
                    Kind::Class,
                    &x,
                    "    ",
                    &format!("{label}/open"),
                );
                if !module {
                    blocks.line(&format!("namespace {ns}"));
                    blocks.line("module P =");
                    probes(
                        &mut blocks,
                        &mut sites,
                        Kind::Class,
                        &x,
                        "    ",
                        &format!("{label}/implicit"),
                    );
                }
                i += 1;
            }
        }
    }
    let sources: Vec<(&str, String)> = vec![
        ("defs.fsi", sig.text),
        ("defs.fs", imp.text),
        ("later.fs", later.text),
        ("blocks.fs", blocks.text),
    ];
    let graded = grade_in("cts", &sources, &sites, fsharp_core_env());
    // Non-vacuity: the signature's order, not the implementation's, decides
    // the expression slot.
    for site in ["open", "implicit"] {
        let fcs = |label: String| {
            let g = graded
                .iter()
                .find(|g| g.label == label)
                .unwrap_or_else(|| panic!("{label} is not a site of the grid"));
            binds_a_case(&sources, &g.fcs)
        };
        assert!(fcs(format!(
            "namespace/sigClassCase/implCaseClass/{site}/expr"
        )));
        assert!(!fcs(format!(
            "namespace/sigCaseClass/implClassCase/{site}/expr"
        )));
    }
    assert_sound_and_live("signature-ordered case and class", &graded, &[], &[]);
}
