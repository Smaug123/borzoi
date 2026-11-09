//! A generated, FCS-graded grid of **a union case reached through an `open` of
//! its container** (issue #44): `open N.M` then a bare `X`, for `type U = X |
//! Y` declared in `N.M` in an earlier file or an earlier block.
//!
//! FCS's name environment is last-write-wins over an ordered sequence of
//! additions, so an `open` adds the container's accessible types and their
//! cases at the `open`, over anything that came before it, and anything added
//! after it claims the name back. The axes:
//!
//! - the union ([`Union`]): a plain one, and the controls a bare use must
//!   never commit to: `[<RequireQualifiedAccess>]` (the `open` adds the type
//!   but not its cases), a `private` representation (the `open` adds the case
//!   and the use is FS1093), and a `private` type (the `open` adds nothing);
//! - the container ([`Container`]): a module, a module nested in one, a
//!   namespace, and an `[<AutoOpen>]` module;
//! - where it is declared ([`Placement`]): an earlier file, an earlier block
//!   of the same file, or the current block;
//! - how the `open` spells it ([`Spelling`]): the full path, a partial path
//!   after an `open` of the outer namespace, `global.`, a path relative to the
//!   enclosing namespace, and an `open` of the namespace that folds an
//!   `[<AutoOpen>]` container;
//! - what claims `X` too, before or after the `open` ([`Rival`]): another
//!   `open` supplying a value, a literal, an active pattern, a union case, an
//!   exception or a class, and a local value;
//! - the use: an expression and a pattern.
//!
//! The oracle is [`crate::common::use_grade`]'s: certain-implies-exact, and
//! [`must_commit`] keeps the grid from passing by declining everything.

use crate::common::fsharp_core_env;
use crate::common::use_grade::{
    FcsVerdict, FileBuilder, Graded, Site, assert_sound_and_live, grade_in,
};

/// The union declared in the container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Union {
    /// `type U = X | Y`.
    Plain,
    /// `[<RequireQualifiedAccess>] type U = X | Y`.
    Rqa,
    /// `type U = private X | Y`.
    PrivateRepr,
    /// `type private U = X | Y`.
    PrivateType,
    /// `type U = X`: an abbreviation of a type `X` if one is in scope, and
    /// otherwise a union with the nullary case `X`
    /// (`TyconCoreAbbrevThatIsReallyAUnion`), which the walk does not intern.
    Abbrev,
}

impl Union {
    const ALL: [Union; 5] = [
        Union::Plain,
        Union::Rqa,
        Union::PrivateRepr,
        Union::PrivateType,
        Union::Abbrev,
    ];

    fn tag(self) -> &'static str {
        match self {
            Union::Plain => "plain",
            Union::Rqa => "rqa",
            Union::PrivateRepr => "privateRepr",
            Union::PrivateType => "privateType",
            Union::Abbrev => "abbrev",
        }
    }

    fn decl(self) -> Vec<String> {
        match self {
            Union::Plain => vec!["type U = X | Y".to_string()],
            Union::Rqa => vec![
                "[<RequireQualifiedAccess>]".to_string(),
                "type U = X | Y".to_string(),
            ],
            Union::PrivateRepr => vec!["type U = private X | Y".to_string()],
            Union::PrivateType => vec!["type private U = X | Y".to_string()],
            Union::Abbrev => vec!["type U = X".to_string()],
        }
    }
}

/// What holds the union, inside namespace `C{i}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    /// `module M = type U = …`.
    Module,
    /// `module M = module Inner = type U = …`.
    Nested,
    /// `namespace C{i}.M` then `type U = …`.
    Namespace,
    /// `[<AutoOpen>] module M = type U = …`.
    AutoOpen,
}

impl Container {
    const ALL: [Container; 4] = [
        Container::Module,
        Container::Nested,
        Container::Namespace,
        Container::AutoOpen,
    ];

    fn tag(self) -> &'static str {
        match self {
            Container::Module => "module",
            Container::Nested => "nested",
            Container::Namespace => "namespace",
            Container::AutoOpen => "autoOpen",
        }
    }

    /// The container's path relative to `C{i}`.
    fn rel(self) -> &'static str {
        match self {
            Container::Nested => "M.Inner",
            Container::Module | Container::Namespace | Container::AutoOpen => "M",
        }
    }

    /// The container's declaration, starting with its namespace header.
    fn decl(self, ns: &str, union: Union) -> Vec<String> {
        let indent = |ind: &str| -> Vec<String> {
            union
                .decl()
                .into_iter()
                .map(|l| format!("{ind}{l}"))
                .collect()
        };
        let mut out = Vec::new();
        match self {
            Container::Module => {
                out.push(format!("namespace {ns}"));
                out.push("module M =".to_string());
                out.extend(indent("    "));
            }
            Container::Nested => {
                out.push(format!("namespace {ns}"));
                out.push("module M =".to_string());
                out.push("    module Inner =".to_string());
                out.extend(indent("        "));
            }
            Container::Namespace => {
                out.push(format!("namespace {ns}.M"));
                out.extend(indent(""));
            }
            Container::AutoOpen => {
                out.push(format!("namespace {ns}"));
                out.push("[<AutoOpen>]".to_string());
                out.push("module M =".to_string());
                out.extend(indent("    "));
            }
        }
        out
    }
}

/// Where the container is declared, relative to the probe's block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    EarlierFile,
    EarlierBlock,
    /// The probe's own namespace block, before the probe's module.
    CurrentBlock,
}

impl Placement {
    const ALL: [Placement; 3] = [
        Placement::EarlierFile,
        Placement::EarlierBlock,
        Placement::CurrentBlock,
    ];

    fn tag(self) -> &'static str {
        match self {
            Placement::EarlierFile => "earlierFile",
            Placement::EarlierBlock => "earlierBlock",
            Placement::CurrentBlock => "currentBlock",
        }
    }
}

/// How the probe's module opens the container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spelling {
    /// `open C{i}.M`, from namespace `S{i}`.
    Full,
    /// `open C{i}` then `open M`, from namespace `S{i}`.
    Partial,
    /// `open global.C{i}.M`, from namespace `S{i}`.
    Global,
    /// `open M`, from namespace `C{i}`.
    Relative,
    /// `open C{i}`, from namespace `S{i}`: folds an `[<AutoOpen>]` container.
    Namespace,
}

impl Spelling {
    const ALL: [Spelling; 5] = [
        Spelling::Full,
        Spelling::Partial,
        Spelling::Global,
        Spelling::Relative,
        Spelling::Namespace,
    ];

    fn tag(self) -> &'static str {
        match self {
            Spelling::Full => "full",
            Spelling::Partial => "partial",
            Spelling::Global => "global",
            Spelling::Relative => "relative",
            Spelling::Namespace => "nsOpen",
        }
    }

    fn applies_to(self, container: Container, placement: Placement) -> bool {
        match self {
            // Only the container's own namespace can hold it in the probe's
            // block.
            _ if placement == Placement::CurrentBlock => {
                self == Spelling::Relative && container != Container::Namespace
            }
            Spelling::Namespace => container == Container::AutoOpen,
            _ => true,
        }
    }

    /// The namespace the probe's block declares.
    fn site_namespace(self, i: usize) -> String {
        match self {
            Spelling::Relative => format!("C{i}"),
            _ => format!("S{i}"),
        }
    }

    fn lines(self, ns: &str, rel: &str) -> Vec<String> {
        match self {
            Spelling::Full => vec![format!("open {ns}.{rel}")],
            Spelling::Partial => vec![format!("open {ns}"), format!("open {rel}")],
            Spelling::Global => vec![format!("open global.{ns}.{rel}")],
            Spelling::Relative => vec![format!("open {rel}")],
            Spelling::Namespace => vec![format!("open {ns}")],
        }
    }
}

/// What else claims `X` in the probe's module, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rival {
    None,
    /// `open Fb.R{i}` of a module with `let X = 0`, before the container's
    /// `open`.
    ValueBefore,
    /// The same, after it.
    ValueAfter,
    /// `[<Literal>] let X = 0`, after: a pattern sees it.
    LiteralAfter,
    /// `let (|X|_|) …`, after: a pattern sees it, an expression does not.
    ActiveAfter,
    /// `type V = X | Z`, before.
    CaseBefore,
    /// The same, after.
    CaseAfter,
    /// `exception X`, after.
    ExceptionAfter,
    /// `type X() = class end`, after: it takes the constructor slot.
    ClassAfter,
    /// `let X = 0` in the probe's module, before the `open`.
    LocalBefore,
    /// The same, after.
    LocalAfter,
}

impl Rival {
    const ALL: [Rival; 11] = [
        Rival::None,
        Rival::ValueBefore,
        Rival::ValueAfter,
        Rival::LiteralAfter,
        Rival::ActiveAfter,
        Rival::CaseBefore,
        Rival::CaseAfter,
        Rival::ExceptionAfter,
        Rival::ClassAfter,
        Rival::LocalBefore,
        Rival::LocalAfter,
    ];

    fn tag(self) -> &'static str {
        match self {
            Rival::None => "none",
            Rival::ValueBefore => "valueBefore",
            Rival::ValueAfter => "valueAfter",
            Rival::LiteralAfter => "literalAfter",
            Rival::ActiveAfter => "activeAfter",
            Rival::CaseBefore => "caseBefore",
            Rival::CaseAfter => "caseAfter",
            Rival::ExceptionAfter => "exceptionAfter",
            Rival::ClassAfter => "classAfter",
            Rival::LocalBefore => "localBefore",
            Rival::LocalAfter => "localAfter",
        }
    }

    /// The rival module's body, for a rival supplied by an `open`.
    fn module_body(self) -> Option<Vec<String>> {
        Some(match self {
            Rival::ValueBefore | Rival::ValueAfter => vec!["let X = 0".to_string()],
            Rival::LiteralAfter => vec!["[<Literal>]".to_string(), "let X = 0".to_string()],
            Rival::ActiveAfter => {
                vec!["let (|X|_|) (v: int) = if v = 1 then Some () else None".to_string()]
            }
            Rival::CaseBefore | Rival::CaseAfter => vec!["type V = X | Z".to_string()],
            Rival::ExceptionAfter => vec!["exception X".to_string()],
            Rival::ClassAfter => vec!["type X() = class end".to_string()],
            Rival::None | Rival::LocalBefore | Rival::LocalAfter => return None,
        })
    }

    fn before(self) -> bool {
        matches!(
            self,
            Rival::ValueBefore | Rival::CaseBefore | Rival::LocalBefore
        )
    }

    /// The rival's lines in the probe's module.
    fn lines(self, i: usize) -> Vec<String> {
        match self {
            Rival::None => Vec::new(),
            Rival::LocalBefore | Rival::LocalAfter => vec!["let X = 0".to_string()],
            _ => vec![format!("open Fb.R{i}")],
        }
    }
}

/// One cell, in namespace `C{i}` (the container's) and `S{i}` (the probe's,
/// unless the spelling is relative). The rival module is `Fb.R{i}`.
#[allow(clippy::too_many_arguments)]
fn cell(
    i: usize,
    union: Union,
    container: Container,
    placement: Placement,
    spelling: Spelling,
    rival: Rival,
    defs: &mut FileBuilder,
    later: &mut FileBuilder,
    sites: &mut Vec<Site>,
) {
    let ns = format!("C{i}");
    let label = format!(
        "{}/{}/{}/{}/{}",
        union.tag(),
        container.tag(),
        placement.tag(),
        spelling.tag(),
        rival.tag()
    );
    if let Some(body) = rival.module_body() {
        defs.line("namespace Fb");
        defs.line(&format!("module R{i} ="));
        for l in body {
            defs.line(&format!("    {l}"));
        }
    }
    let container_decl = container.decl(&ns, union);
    match placement {
        Placement::EarlierFile => {
            for l in &container_decl {
                defs.line(l);
            }
        }
        Placement::EarlierBlock => {
            for l in &container_decl {
                later.line(l);
            }
        }
        Placement::CurrentBlock => {}
    }
    later.line(&format!("namespace {}", spelling.site_namespace(i)));
    if placement == Placement::CurrentBlock {
        // The header is the probe block's own.
        for l in &container_decl[1..] {
            later.line(l);
        }
    }
    later.line("module P =");
    let opens = spelling.lines(&ns, container.rel());
    let rival_lines = rival.lines(i);
    let body: Vec<String> = if rival.before() {
        rival_lines.into_iter().chain(opens).collect()
    } else {
        opens.into_iter().chain(rival_lines).collect()
    };
    for l in body {
        later.line(&format!("    {l}"));
    }
    later.probe("    let v = X", "X", format!("{label}/expr"), sites);
    later.probe(
        "    let p = match Unchecked.defaultof<_> with X -> 0 | _ -> 1",
        "X",
        format!("{label}/pat"),
        sites,
    );
}

/// The sites that must commit FCS's answer: every site FCS binds cleanly,
/// except three classes the resolver declines for reasons outside this grid's
/// subject, each selected by its axes:
///
/// - a pattern over a later `open`'s `[<Literal>]` (`literalAfter`, `pat`):
///   FCS binds the literal, a constant pattern, which declines;
/// - an expression over a later `open`'s class (`classAfter`, `expr`): FCS
///   binds the class's constructor, which sema does not model;
/// - the `type U = X` union (`abbrev`): whether `X` is a case or a type
///   depends on every type name in scope at the declaration, so the walk
///   interns no case for it, and an `open` of its container declines `X`.
fn must_commit(graded: &[Graded]) -> Vec<&str> {
    graded
        .iter()
        .filter(|g| matches!(g.fcs, FcsVerdict::Resolved(..)))
        .map(|g| g.label.as_str())
        .filter(|label| {
            let parts: Vec<&str> = label.split('/').collect();
            let (union, rival, probe) = (parts[0], parts[4], parts[5]);
            !(union == "abbrev"
                || (rival, probe) == ("literalAfter", "pat")
                || (rival, probe) == ("classAfter", "expr"))
        })
        .collect()
}

#[test]
fn a_case_through_an_open_of_its_container_agrees_with_fcs() {
    let mut defs = FileBuilder::new(0);
    let mut later = FileBuilder::new(1);
    let mut sites = Vec::new();
    let mut i = 0;
    for union in Union::ALL {
        for container in Container::ALL {
            for placement in Placement::ALL {
                for spelling in Spelling::ALL {
                    if !spelling.applies_to(container, placement) {
                        continue;
                    }
                    for rival in Rival::ALL {
                        cell(
                            i, union, container, placement, spelling, rival, &mut defs, &mut later,
                            &mut sites,
                        );
                        i += 1;
                    }
                }
            }
        }
    }
    // FCS reads FSharp.Core, and `[<RequireQualifiedAccess>]` is certain
    // only where FSharp.Core declares it.
    let graded = grade_in(
        "cog",
        &[("defs.fs", defs.text), ("later.fs", later.text)],
        &sites,
        fsharp_core_env(),
    );
    let count = |p: &dyn Fn(&Graded) -> bool| graded.iter().filter(|g| p(g)).count();
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Inaccessible)) > 0);
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Rejected)) > 0);
    let must = must_commit(&graded);
    // Non-vacuity: the must-commit set holds the subject itself, a plain case
    // through every container, placement and spelling.
    for label in [
        "plain/module/earlierFile/full/none/expr",
        "plain/nested/earlierBlock/partial/none/pat",
        "plain/namespace/earlierFile/global/none/expr",
        "plain/autoOpen/earlierFile/nsOpen/none/pat",
        "plain/module/currentBlock/relative/none/expr",
    ] {
        assert!(must.contains(&label), "{label} is not a must-commit site");
    }
    assert_sound_and_live("case through open", &graded, &[], &must);
}
