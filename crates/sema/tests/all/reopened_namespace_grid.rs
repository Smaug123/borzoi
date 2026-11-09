//! A generated, FCS-graded grid of **re-opening the enclosing namespace**
//! (issue #489).
//!
//! FCS's name environment is last-write-wins over an ordered sequence of
//! additions. Inside `namespace N`, the implicit open of `N` at the block's
//! start adds what `N` already holds, and the block's own declarations are
//! added as they are checked. An explicit `open N` written later in the block
//! adds `N`'s contents again, at the `open`'s position, over whatever came in
//! between. Which contents it adds is the subject: `N`'s fragments from
//! earlier files and earlier blocks are registered in the environment by the
//! time the `open` is checked; the current block's own fragment is not.
//!
//! The axes:
//!
//! - what `N` holds under the name `X` ([`Subject`]): a namespace-level union
//!   case, exception, module or type, or a value, union case, active pattern
//!   or module of an `[<AutoOpen>]` module (one nested a level down too), or
//!   a namespace-level class contesting the constructor slot with a case or
//!   an `[<AutoOpen>]` module's value;
//! - what sits between the block's start and the probe and claims `X` too
//!   ([`Intervener`]): an `open` of a module supplying `X` (a value, or a
//!   literal, which a pattern sees), a local declaration, or an
//!   `[<AutoOpen>]` sibling in the current block;
//! - how `N` is re-opened ([`Reopen`]): not at all, `open N` after the
//!   intervener or before it, `open global.N`, or `open R` then `open N`;
//! - where `N`'s `X` lives ([`Placement`]): an earlier file, an earlier block
//!   of the same file, or the current block;
//! - whether the probe sits in a module of the block or a module nested in
//!   one, and (separately) in a file whose header is `module N.M`.
//!
//! The oracle is [`crate::common::use_grade`]'s: certain-implies-exact, and
//! [`must_commit`] keeps the grid from passing by declining everything.

use crate::common::use_grade::{
    FcsVerdict, FileBuilder, Graded, Site, assert_sound_and_live, grade,
};

/// What the re-opened namespace holds under the probed name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subject {
    /// `type U = X | Y` at namespace level.
    NsCase,
    /// `exception X` at namespace level.
    NsException,
    /// `module X = let v = 1` at namespace level.
    NsModule,
    /// `type X = XN` at namespace level, probed in type position.
    NsType,
    /// `[<AutoOpen>] module A = let X = 1`.
    AoValue,
    /// `[<AutoOpen>] module A = [<AutoOpen>] module A2 = let X = 1`.
    AoNestedValue,
    /// `[<AutoOpen>] module A = type U = X | Y`.
    AoCase,
    /// `[<AutoOpen>] module A = let (|X|_|) …`, probed in a pattern.
    AoActive,
    /// `[<AutoOpen>] module A = module X = let v = 1`.
    AoModule,
    /// `type U = X of unit | Y` then `type X() = class end`: the class takes
    /// the constructor slot from the case. Probed applied, `X ()`.
    NsCaseThenClass,
    /// `type X() = class end` then `type U = X of unit | Y`: the case takes it
    /// back.
    NsClassThenCase,
    /// `type X() = class end` and `[<AutoOpen>] module A = let X () = 1`: a
    /// namespace folds its types before its auto-open modules' values.
    AoValueOverClass,
}

/// The name space a subject's `X` is probed in, which decides what claims
/// the same name between the block's start and the probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// A value, union case or exception: probed in an expression and in a
    /// pattern.
    Value,
    /// An active pattern: probed in a pattern only.
    Pattern,
    /// A module: probed as the head of `X.v`.
    Module,
    /// A type: probed in an annotation.
    Type,
    /// A constructor slot a class and a case or value contest: probed
    /// applied to `()`.
    Applied,
}

impl Subject {
    const ALL: [Subject; 12] = [
        Subject::NsCase,
        Subject::NsException,
        Subject::NsModule,
        Subject::NsType,
        Subject::AoValue,
        Subject::AoNestedValue,
        Subject::AoCase,
        Subject::AoActive,
        Subject::AoModule,
        Subject::NsCaseThenClass,
        Subject::NsClassThenCase,
        Subject::AoValueOverClass,
    ];

    fn tag(self) -> &'static str {
        match self {
            Subject::NsCase => "nsCase",
            Subject::NsException => "nsException",
            Subject::NsModule => "nsModule",
            Subject::NsType => "nsType",
            Subject::AoValue => "aoValue",
            Subject::AoNestedValue => "aoNestedValue",
            Subject::AoCase => "aoCase",
            Subject::AoActive => "aoActive",
            Subject::AoModule => "aoModule",
            Subject::NsCaseThenClass => "nsCaseThenClass",
            Subject::NsClassThenCase => "nsClassThenCase",
            Subject::AoValueOverClass => "aoValueOverClass",
        }
    }

    fn class(self) -> Class {
        match self {
            Subject::NsCase
            | Subject::NsException
            | Subject::AoValue
            | Subject::AoNestedValue
            | Subject::AoCase => Class::Value,
            Subject::AoActive => Class::Pattern,
            Subject::NsModule | Subject::AoModule => Class::Module,
            Subject::NsType => Class::Type,
            Subject::NsCaseThenClass | Subject::NsClassThenCase | Subject::AoValueOverClass => {
                Class::Applied
            }
        }
    }

    /// The declaration, at a namespace block's top level, of `name` (the
    /// auto-open module, where there is one, is `ao`).
    fn decl(self, name: &str, ao: &str) -> Vec<String> {
        let auto_open = |body: &[String]| {
            let mut out = vec!["[<AutoOpen>]".to_string(), format!("module {ao} =")];
            out.extend(body.iter().map(|l| format!("    {l}")));
            out
        };
        match self {
            Subject::NsCase => vec![format!("type U{name} = {name} | Y{name}")],
            Subject::NsException => vec![format!("exception {name}")],
            Subject::NsModule => vec![format!("module {name} ="), "    let v = 1".to_string()],
            Subject::NsType => vec![format!("type {name} = {name}N")],
            Subject::AoValue => auto_open(&[format!("let {name} = 1")]),
            Subject::AoNestedValue => auto_open(&[
                "[<AutoOpen>]".to_string(),
                format!("module {ao}2 ="),
                format!("    let {name} = 1"),
            ]),
            Subject::AoCase => auto_open(&[format!("type U{name} = {name} | Y{name}")]),
            Subject::AoActive => auto_open(&[format!(
                "let (|{name}|_|) (v: int) = if v = 1 then Some () else None"
            )]),
            Subject::AoModule => {
                auto_open(&[format!("module {name} ="), "    let v = 1".to_string()])
            }
            Subject::NsCaseThenClass => vec![
                format!("type U{name} = {name} of unit | Y{name}"),
                format!("type {name}() = class end"),
            ],
            Subject::NsClassThenCase => vec![
                format!("type {name}() = class end"),
                format!("type U{name} = {name} of unit | Y{name}"),
            ],
            Subject::AoValueOverClass => {
                let mut out = vec![format!("type {name}() = class end")];
                out.extend(auto_open(&[format!("let {name} () = 1")]));
                out
            }
        }
    }
}

/// A declaration of `name` in `class`'s name space, claiming it from a
/// subject. `literal` makes a value a `[<Literal>]`, which a pattern sees;
/// an active pattern's rival is always one. `tag` keeps a type's case names
/// apart.
fn rival_decl(class: Class, literal: bool, name: &str, tag: &str) -> Vec<String> {
    match class {
        Class::Value if !literal => vec![format!("let {name} = 0")],
        Class::Value | Class::Pattern => {
            vec!["[<Literal>]".to_string(), format!("let {name} = 0")]
        }
        Class::Module => vec![format!("module {name} ="), "    let v = 0".to_string()],
        Class::Type => vec![format!("type {name} = {name}{tag}")],
        Class::Applied => vec![format!("let {name} () = 0")],
    }
}

/// What claims the probed name between the block's start and the probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Intervener {
    None,
    /// `open Fb.F` of an earlier file's module declaring `X`.
    Open,
    /// The same, with `X` a `[<Literal>]` (a value subject only).
    OpenLiteral,
    /// A declaration of `X` in the probe's module, before the probe.
    Local,
    /// `[<AutoOpen>] module B` declaring `X`, in the current block before the
    /// probe's module.
    AutoOpenSibling,
    /// `open Fn` of an earlier file's namespace holding `X`, first in the
    /// current block: before the block's own declarations, which outrank it.
    BlockOpen,
}

impl Intervener {
    const ALL: [Intervener; 6] = [
        Intervener::None,
        Intervener::Open,
        Intervener::OpenLiteral,
        Intervener::Local,
        Intervener::AutoOpenSibling,
        Intervener::BlockOpen,
    ];

    fn tag(self) -> &'static str {
        match self {
            Intervener::None => "none",
            Intervener::Open => "open",
            Intervener::OpenLiteral => "openLiteral",
            Intervener::Local => "local",
            Intervener::AutoOpenSibling => "autoOpenSibling",
            Intervener::BlockOpen => "blockOpen",
        }
    }

    fn applies_to(self, class: Class) -> bool {
        self != Intervener::OpenLiteral || class == Class::Value
    }

    fn literal(self) -> bool {
        self == Intervener::OpenLiteral
    }
}

/// How the probe's module re-opens the enclosing namespace `R.N`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reopen {
    /// Not at all: only the block's implicit open.
    Implicit,
    /// `open R.N`, after the intervener.
    Explicit,
    /// `open R.N`, before the intervener.
    ExplicitBefore,
    /// `open global.R.N`.
    Global,
    /// `open R` then `open N`.
    Ancestor,
    /// The control: `open U` of an unrelated namespace holding the subject
    /// (declared in an earlier file), after the intervener.
    Unrelated,
    /// The same, before the intervener.
    UnrelatedBefore,
}

impl Reopen {
    const ALL: [Reopen; 7] = [
        Reopen::Implicit,
        Reopen::Explicit,
        Reopen::ExplicitBefore,
        Reopen::Global,
        Reopen::Ancestor,
        Reopen::Unrelated,
        Reopen::UnrelatedBefore,
    ];

    fn tag(self) -> &'static str {
        match self {
            Reopen::Implicit => "implicit",
            Reopen::Explicit => "explicit",
            Reopen::ExplicitBefore => "explicitBefore",
            Reopen::Global => "global",
            Reopen::Ancestor => "ancestor",
            Reopen::Unrelated => "unrelated",
            Reopen::UnrelatedBefore => "unrelatedBefore",
        }
    }

    /// The `open`s, for the cell whose namespace is `R{i}.N` (`root` is
    /// `R{i}`) and whose unrelated namespace is `U{i}`.
    fn lines(self, root: &str, unrelated: &str) -> Vec<String> {
        match self {
            Reopen::Implicit => Vec::new(),
            Reopen::Explicit | Reopen::ExplicitBefore => vec![format!("open {root}.N")],
            Reopen::Global => vec![format!("open global.{root}.N")],
            Reopen::Ancestor => vec![format!("open {root}"), "open N".to_string()],
            Reopen::Unrelated | Reopen::UnrelatedBefore => vec![format!("open {unrelated}")],
        }
    }

    fn before(self) -> bool {
        matches!(self, Reopen::ExplicitBefore | Reopen::UnrelatedBefore)
    }

    fn unrelated(self) -> bool {
        matches!(self, Reopen::Unrelated | Reopen::UnrelatedBefore)
    }
}

/// Where the re-opened namespace's `X` is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    EarlierFile,
    EarlierBlock,
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

/// Append the probes of `subject`'s name `name` at indentation `ind`.
fn probes(
    f: &mut FileBuilder,
    sites: &mut Vec<Site>,
    class: Class,
    name: &str,
    ind: &str,
    label: &str,
) {
    match class {
        Class::Value => {
            f.probe(
                &format!("{ind}let v{name} = {name}"),
                name,
                format!("{label}/expr"),
                sites,
            );
            f.probe(
                &format!(
                    "{ind}let p{name} = match Unchecked.defaultof<_> with {name} -> 0 | _ -> 1"
                ),
                name,
                format!("{label}/pat"),
                sites,
            );
        }
        Class::Pattern => f.probe(
            &format!("{ind}let p{name} = match 1 with {name} -> 0 | _ -> 1"),
            name,
            format!("{label}/pat"),
            sites,
        ),
        Class::Module => {
            let dotted = format!("{name}.v");
            f.probe(
                &format!("{ind}let w{name} = {dotted}"),
                &dotted,
                format!("{label}/dotted"),
                sites,
            );
        }
        Class::Type => f.probe(
            &format!("{ind}let f{name} (q: {name}) = q"),
            name,
            format!("{label}/annotation"),
            sites,
        ),
        Class::Applied => f.probe(
            &format!("{ind}let a{name} = {name} ()"),
            name,
            format!("{label}/applied"),
            sites,
        ),
    }
}

fn indented(lines: &[String], ind: &str) -> Vec<String> {
    lines.iter().map(|l| format!("{ind}{l}")).collect()
}

/// One cell of the main grid, in namespace `R{i}.N`. The intervening `open`'s
/// module is `Fb.F{i}`, declared in `defs`.
#[allow(clippy::too_many_arguments)]
fn cell(
    i: usize,
    subject: Subject,
    intervener: Intervener,
    reopen: Reopen,
    placement: Placement,
    nested: bool,
    defs: &mut FileBuilder,
    later: &mut FileBuilder,
    sites: &mut Vec<Site>,
) {
    let root = format!("R{i}");
    let class = subject.class();
    let label = format!(
        "{}/{}/{}/{}/{}",
        subject.tag(),
        intervener.tag(),
        reopen.tag(),
        placement.tag(),
        if nested { "nested" } else { "module" }
    );
    let subject_decl = subject.decl("X", "A");
    if matches!(intervener, Intervener::Open | Intervener::OpenLiteral) {
        defs.line("namespace Fb");
        defs.line(&format!("module F{i} ="));
        for l in indented(&rival_decl(class, intervener.literal(), "X", "F"), "    ") {
            defs.line(&l);
        }
    }
    if intervener == Intervener::BlockOpen {
        // A namespace holds a module or a type itself, and a value only
        // through an `[<AutoOpen>]` module.
        defs.line(&format!("namespace Fn{i}"));
        let rival = rival_decl(class, false, "X", "G");
        if matches!(class, Class::Module | Class::Type) {
            for l in &rival {
                defs.line(l);
            }
        } else {
            defs.line("[<AutoOpen>]");
            defs.line("module G =");
            for l in indented(&rival, "    ") {
                defs.line(&l);
            }
        }
    }
    let unrelated = format!("U{i}");
    let home = if reopen.unrelated() {
        unrelated.clone()
    } else {
        format!("{root}.N")
    };
    match placement {
        Placement::EarlierFile => {
            defs.line(&format!("namespace {home}"));
            for l in &subject_decl {
                defs.line(l);
            }
        }
        Placement::EarlierBlock => {
            later.line(&format!("namespace {root}.N"));
            for l in &subject_decl {
                later.line(l);
            }
        }
        Placement::CurrentBlock => {}
    }
    later.line(&format!("namespace {root}.N"));
    if intervener == Intervener::BlockOpen {
        later.line(&format!("open Fn{i}"));
    }
    if placement == Placement::CurrentBlock {
        for l in &subject_decl {
            later.line(l);
        }
    }
    if intervener == Intervener::AutoOpenSibling {
        later.line("[<AutoOpen>]");
        later.line("module B =");
        for l in indented(&rival_decl(class, false, "X", "B"), "    ") {
            later.line(&l);
        }
    }
    later.line("module M =");
    let ind = if nested {
        later.line("    module Q =");
        "        "
    } else {
        "    "
    };
    let rival: Vec<String> = match intervener {
        Intervener::Open | Intervener::OpenLiteral => vec![format!("open Fb.F{i}")],
        Intervener::Local => rival_decl(class, false, "X", "L"),
        Intervener::None | Intervener::AutoOpenSibling | Intervener::BlockOpen => Vec::new(),
    };
    let reopen_lines = reopen.lines(&root, &unrelated);
    let body: Vec<String> = if reopen.before() {
        reopen_lines.into_iter().chain(rival).collect()
    } else {
        rival.into_iter().chain(reopen_lines).collect()
    };
    for l in indented(&body, ind) {
        later.line(&l);
    }
    probes(later, sites, class, "X", ind, &label);
}

/// The header files' cells: one file per intervener and re-open, whose header
/// `module H{j}.N.M` makes `H{j}.N` the enclosing namespace, probing every
/// subject (each under its own name) from the module's top level. The
/// subjects live in an earlier file.
fn header_files(defs: &mut FileBuilder, sites: &mut Vec<Site>, first: usize) -> Vec<FileBuilder> {
    let mut files = Vec::new();
    let shapes = [Intervener::Open, Intervener::Local]
        .into_iter()
        .flat_map(|iv| [Reopen::Implicit, Reopen::Explicit].map(|r| (iv, r)));
    for (j, (intervener, reopen)) in shapes.enumerate() {
        let root = format!("H{j}");
        let mut f = FileBuilder::new(first + j);
        defs.line(&format!("namespace {root}.N"));
        for (k, subject) in Subject::ALL.into_iter().enumerate() {
            for l in subject.decl(&format!("X{k}"), &format!("A{k}")) {
                defs.line(&l);
            }
        }
        let rivals: Vec<String> = Subject::ALL
            .into_iter()
            .enumerate()
            .flat_map(|(k, s)| rival_decl(s.class(), false, &format!("X{k}"), "R"))
            .collect();
        f.line(&format!("module {root}.N.M"));
        f.line("");
        match intervener {
            Intervener::Open => {
                defs.line("namespace Fb");
                defs.line(&format!("module FH{j} ="));
                for l in indented(&rivals, "    ") {
                    defs.line(&l);
                }
                f.line(&format!("open Fb.FH{j}"));
            }
            _ => {
                for l in &rivals {
                    f.line(l);
                }
            }
        }
        for l in reopen.lines(&root, "") {
            f.line(&l);
        }
        for (k, subject) in Subject::ALL.into_iter().enumerate() {
            let label = format!(
                "{}/{}/{}/earlierFile/header",
                subject.tag(),
                intervener.tag(),
                reopen.tag()
            );
            probes(&mut f, sites, subject.class(), &format!("X{k}"), "", &label);
        }
        files.push(f);
    }
    files
}

/// The sites that must commit FCS's answer, when FCS binds them cleanly:
///
/// - every value probed in an expression, wherever it is declared and
///   whatever claims its name in between;
/// - every union case or exception probed in a pattern over a rival that is
///   not a literal (a literal is a constant pattern, which declines);
/// - every dotted head through a namespace-level module, over no rival, or
///   over a local module where the module is an earlier file's or block's,
///   re-opened or not;
/// - every `[<AutoOpen>]` module's value applied over the namespace's class
///   of its name, through an `open` of a namespace an earlier file or block
///   declares (the namespace folds its types before its modules' values).
fn must_commit(graded: &[Graded]) -> Vec<&str> {
    graded
        .iter()
        .filter(|g| matches!(g.fcs, FcsVerdict::Resolved(..)))
        .map(|g| g.label.as_str())
        .filter(|label| {
            let parts: Vec<&str> = label.split('/').collect();
            let (subject, intervener, reopen, placement, probe) = (
                parts[0],
                parts[1],
                parts[2],
                parts[3],
                parts[parts.len() - 1],
            );
            match probe {
                "expr" => true,
                "pat" => {
                    matches!(subject, "nsCase" | "nsException" | "aoCase")
                        && intervener != "openLiteral"
                }
                "dotted" => {
                    subject == "nsModule"
                        && matches!(intervener, "none" | "local")
                        && matches!(reopen, "implicit" | "explicit" | "global" | "ancestor")
                        && (intervener == "none" || placement != "currentBlock")
                }
                "applied" => {
                    subject == "aoValueOverClass"
                        && matches!(reopen, "explicit" | "global" | "ancestor" | "unrelated")
                        && placement != "currentBlock"
                }
                _ => false,
            }
        })
        .collect()
}

/// A dotted head `X.v` the walk resolves by tier rather than by position:
/// every `open`'s prefix outranks every lexical module, where FCS keeps one
/// latest-first table (#462 ranks prefixes by position). So a local `module
/// X` declared after an `open` supplying `X` loses to it, and so does one
/// declared before an `open` of the enclosing namespace whose current-block
/// `X` that `open` does not re-add, FCS registering the block's fragment only
/// when it ends. The `implicit` and `unrelated` rows show it is not the
/// re-open's doing.
const OPEN_PREFIX_RANKING: &[&str] = &[
    "nsModule/blockOpen/implicit/currentBlock/module/dotted",
    "nsModule/local/ancestor/currentBlock/module/dotted",
    "nsModule/local/explicit/currentBlock/module/dotted",
    "nsModule/local/explicitBefore/currentBlock/module/dotted",
    "nsModule/local/explicitBefore/earlierBlock/module/dotted",
    "nsModule/local/explicitBefore/earlierFile/module/dotted",
    "nsModule/local/explicitBefore/earlierFile/nested/dotted",
    "nsModule/local/global/currentBlock/module/dotted",
    "nsModule/local/unrelatedBefore/earlierFile/module/dotted",
    "nsModule/local/unrelatedBefore/earlierFile/nested/dotted",
];

/// A dotted head `X.v` whose module an `[<AutoOpen>]` module's child `X`
/// supplies by its short name: FCS enters the child as `X` wherever the
/// `[<AutoOpen>]` module is folded, and the walk enters it nowhere (#462
/// enters it). So the sibling `[<AutoOpen>] module B = module X` does not
/// shadow the namespace's own `X`, and an `open` folding `A`'s child `X`
/// does not shadow a local `module X`, or another `open`'s, before it.
/// Neither is about re-opening the enclosing namespace: the `implicit` and
/// `unrelated` rows have it too.
const AUTO_OPEN_CHILD_MODULE: &[&str] = &[
    "aoModule/blockOpen/ancestor/currentBlock/module/dotted",
    "aoModule/blockOpen/ancestor/earlierBlock/module/dotted",
    "aoModule/blockOpen/ancestor/earlierFile/module/dotted",
    "aoModule/blockOpen/ancestor/earlierFile/nested/dotted",
    "aoModule/blockOpen/explicit/currentBlock/module/dotted",
    "aoModule/blockOpen/explicit/earlierBlock/module/dotted",
    "aoModule/blockOpen/explicit/earlierFile/module/dotted",
    "aoModule/blockOpen/explicit/earlierFile/nested/dotted",
    "aoModule/blockOpen/explicitBefore/currentBlock/module/dotted",
    "aoModule/blockOpen/explicitBefore/earlierBlock/module/dotted",
    "aoModule/blockOpen/explicitBefore/earlierFile/module/dotted",
    "aoModule/blockOpen/explicitBefore/earlierFile/nested/dotted",
    "aoModule/blockOpen/global/currentBlock/module/dotted",
    "aoModule/blockOpen/global/earlierBlock/module/dotted",
    "aoModule/blockOpen/global/earlierFile/module/dotted",
    "aoModule/blockOpen/global/earlierFile/nested/dotted",
    "aoModule/blockOpen/implicit/currentBlock/module/dotted",
    "aoModule/blockOpen/unrelated/earlierFile/module/dotted",
    "aoModule/blockOpen/unrelated/earlierFile/nested/dotted",
    "aoModule/blockOpen/unrelatedBefore/earlierFile/module/dotted",
    "aoModule/blockOpen/unrelatedBefore/earlierFile/nested/dotted",
    "aoModule/local/ancestor/earlierBlock/module/dotted",
    "aoModule/local/ancestor/earlierFile/module/dotted",
    "aoModule/local/ancestor/earlierFile/nested/dotted",
    "aoModule/local/explicit/earlierBlock/module/dotted",
    "aoModule/local/explicit/earlierFile/header/dotted",
    "aoModule/local/explicit/earlierFile/module/dotted",
    "aoModule/local/explicit/earlierFile/nested/dotted",
    "aoModule/local/global/earlierBlock/module/dotted",
    "aoModule/local/global/earlierFile/module/dotted",
    "aoModule/local/global/earlierFile/nested/dotted",
    "aoModule/local/unrelated/earlierFile/module/dotted",
    "aoModule/local/unrelated/earlierFile/nested/dotted",
    "nsModule/autoOpenSibling/ancestor/currentBlock/module/dotted",
    "nsModule/autoOpenSibling/explicit/currentBlock/module/dotted",
    "nsModule/autoOpenSibling/explicitBefore/currentBlock/module/dotted",
    "nsModule/autoOpenSibling/global/currentBlock/module/dotted",
    "nsModule/autoOpenSibling/implicit/currentBlock/module/dotted",
    "nsModule/autoOpenSibling/implicit/earlierBlock/module/dotted",
    "nsModule/autoOpenSibling/implicit/earlierFile/module/dotted",
    "nsModule/autoOpenSibling/implicit/earlierFile/nested/dotted",
];

#[test]
fn reopening_the_enclosing_namespace_agrees_with_fcs() {
    let mut defs = FileBuilder::new(0);
    let mut later = FileBuilder::new(1);
    let mut sites = Vec::new();
    let mut i = 0;
    for subject in Subject::ALL {
        for intervener in Intervener::ALL {
            if !intervener.applies_to(subject.class()) {
                continue;
            }
            for reopen in Reopen::ALL {
                if intervener == Intervener::None && reopen.before() {
                    continue;
                }
                for placement in Placement::ALL {
                    // The unrelated namespace lives in an earlier file.
                    if reopen.unrelated() && placement != Placement::EarlierFile {
                        continue;
                    }
                    for nested in [false, true] {
                        if nested && placement != Placement::EarlierFile {
                            continue;
                        }
                        cell(
                            i, subject, intervener, reopen, placement, nested, &mut defs,
                            &mut later, &mut sites,
                        );
                        i += 1;
                    }
                }
            }
        }
    }
    let headers = header_files(&mut defs, &mut sites, 2);
    let mut files = vec![
        ("defs.fs".to_string(), defs.text),
        ("later.fs".to_string(), later.text),
    ];
    files.extend(
        headers
            .into_iter()
            .enumerate()
            .map(|(j, f)| (format!("header{j}.fs"), f.text)),
    );
    let files: Vec<(&str, String)> = files.iter().map(|(n, s)| (n.as_str(), s.clone())).collect();
    let graded = grade("rng", &files, &sites);
    // Non-vacuity: the explicit re-open decides cells, both ways.
    let fcs_target = |label: &str| {
        graded
            .iter()
            .find(|g| g.label == label)
            .map(|g| g.fcs.clone())
    };
    assert_ne!(
        fcs_target("nsCase/open/implicit/earlierFile/module/expr"),
        fcs_target("nsCase/open/explicit/earlierFile/module/expr"),
        "the explicit re-open moves a later file's binding"
    );
    let must = must_commit(&graded);
    let known_wrong: Vec<&str> = OPEN_PREFIX_RANKING
        .iter()
        .chain(AUTO_OPEN_CHILD_MODULE)
        .copied()
        .collect();
    assert_sound_and_live("reopened namespace", &graded, &known_wrong, &must);
}
