//! A generated, FCS-graded grid of **a type declared in an earlier namespace
//! block of the file**.
//!
//! FCS checks each `namespace` block of a file afresh
//! (`CheckDeclarations.fs`, `SynModuleDecl.NamespaceFragment`): the block's
//! environment is the one the previous block ended with, which holds the
//! earlier blocks' root namespaces but none of their contents, and the block
//! then opens its own namespace (`ImplicitlyOpenOwnNamespace`), and nothing
//! else. So a type declared in block k is never lexically in scope in a later
//! block j:
//!
//! - a later block of the same namespace sees it only through that implicit
//!   `open`, at the block's start, which adds the namespace's fragments in
//!   order, each fragment's types before its `[<AutoOpen>]` modules;
//! - a block of a descendant, ancestor or unrelated namespace does not see it
//!   unless an `open` of its namespace brings it.
//!
//! The axes:
//!
//! - the type's kind ([`TypeKind`]): a class, which takes the unqualified
//!   name slot from a value, or a record, which does not;
//! - a rival for the name in the type's namespace ([`Rival`]): an
//!   `[<AutoOpen>]` module's value, nested module or type, or none;
//! - where the rival sits ([`RivalPlacement`]): in the type's block before or
//!   after it, in a separate earlier or later block of the namespace, or in
//!   an earlier file. An `open` of the namespace, implicit or written, adds
//!   this file's earlier blocks one fragment at a time, so the block decides
//!   the order; a later file sees the whole file as one fragment;
//! - where the probe sits ([`SiteNs`]): the type's own block (the lexical
//!   control), or a later block of the same, a descendant, an ancestor or an
//!   unrelated namespace, in the type's file or in a later file;
//! - whether the probe's module opens the type's namespace;
//! - the use: a bare name, the head of `X.M`, a type annotation and a
//!   constructor call.
//!
//! The oracle is [`crate::common::use_grade`]'s: certain-implies-exact, and
//! [`MUST_COMMIT`] keeps the grid from passing by declining everything.

use crate::common::use_grade::{
    FcsVerdict, FileBuilder, Graded, Site, assert_sound_and_live, grade,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypeKind {
    /// `type X() = static member M = 0`.
    Class,
    /// `type X = { F: int } static member M = 0`.
    Record,
}

impl TypeKind {
    const ALL: [TypeKind; 2] = [TypeKind::Class, TypeKind::Record];

    fn tag(self) -> &'static str {
        match self {
            TypeKind::Class => "class",
            TypeKind::Record => "record",
        }
    }

    fn decl(self) -> Vec<String> {
        match self {
            TypeKind::Class => vec![
                "type X() =".to_string(),
                "    static member M = 0".to_string(),
            ],
            TypeKind::Record => vec![
                "type X =".to_string(),
                "    { F: int }".to_string(),
                "    static member M = 0".to_string(),
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rival {
    None,
    /// `[<AutoOpen>] module A = let X = {| M = 1 |}`.
    AoValue,
    /// `[<AutoOpen>] module A = module X = let M = 1`.
    AoModule,
    /// `[<AutoOpen>] module A = type X() = static member M = 1`.
    AoType,
}

impl Rival {
    const ALL: [Rival; 4] = [Rival::None, Rival::AoValue, Rival::AoModule, Rival::AoType];

    fn tag(self) -> &'static str {
        match self {
            Rival::None => "none",
            Rival::AoValue => "aoValue",
            Rival::AoModule => "aoModule",
            Rival::AoType => "aoType",
        }
    }

    fn decl(self) -> Vec<String> {
        let body: &[&str] = match self {
            Rival::None => return Vec::new(),
            Rival::AoValue => &["let X = {| M = 1 |}"],
            Rival::AoModule => &["module X =", "    let M = 1"],
            Rival::AoType => &["type X() =", "    static member M = 1"],
        };
        let mut out = vec!["[<AutoOpen>]".to_string(), "module A =".to_string()];
        out.extend(body.iter().map(|l| format!("    {l}")));
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RivalPlacement {
    /// In the type's block, before the type.
    BlockBefore,
    /// In the type's block, after the type.
    BlockAfter,
    /// In a block of the type's namespace before the type's block.
    EarlierBlock,
    /// In a block of the type's namespace between the type's block and the
    /// probes.
    LaterBlock,
    /// In the type's namespace, in the earlier file.
    EarlierFile,
}

impl RivalPlacement {
    const ALL: [RivalPlacement; 5] = [
        RivalPlacement::BlockBefore,
        RivalPlacement::BlockAfter,
        RivalPlacement::EarlierBlock,
        RivalPlacement::LaterBlock,
        RivalPlacement::EarlierFile,
    ];

    fn tag(self) -> &'static str {
        match self {
            RivalPlacement::BlockBefore => "blockBefore",
            RivalPlacement::BlockAfter => "blockAfter",
            RivalPlacement::EarlierBlock => "earlierBlock",
            RivalPlacement::LaterBlock => "laterBlock",
            RivalPlacement::EarlierFile => "earlierFile",
        }
    }
}

/// Where a probe module sits, relative to the type's namespace `R{i}.T`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SiteNs {
    /// A later block of `R{i}.T`.
    Same,
    /// A block of `R{i}.T.Sub`.
    Descendant,
    /// A block of `R{i}`.
    Ancestor,
    /// A block of `Q{i}`.
    Unrelated,
}

impl SiteNs {
    const ALL: [SiteNs; 4] = [
        SiteNs::Same,
        SiteNs::Descendant,
        SiteNs::Ancestor,
        SiteNs::Unrelated,
    ];

    fn tag(self) -> &'static str {
        match self {
            SiteNs::Same => "sameNs",
            SiteNs::Descendant => "subNs",
            SiteNs::Ancestor => "ancestorNs",
            SiteNs::Unrelated => "otherNs",
        }
    }

    fn namespace(self, i: usize) -> String {
        match self {
            SiteNs::Same => format!("R{i}.T"),
            SiteNs::Descendant => format!("R{i}.T.Sub"),
            SiteNs::Ancestor => format!("R{i}"),
            SiteNs::Unrelated => format!("Q{i}"),
        }
    }
}

fn lines(f: &mut FileBuilder, ls: &[String]) {
    for l in ls {
        f.line(l);
    }
}

/// The uses of `X`, in a module at one indent. A value rival is no
/// constructor, so where it wins `X ()` is a type error FCS reports at the
/// name; the constructor is probed only without one.
fn probes(f: &mut FileBuilder, sites: &mut Vec<Site>, rival: Rival, label: &str) {
    f.probe("    let v = X", "X", format!("{label}/bare"), sites);
    f.probe("    let w = X.M", "X", format!("{label}/head"), sites);
    f.probe(
        "    let f (q: X) = q",
        "X",
        format!("{label}/annotation"),
        sites,
    );
    if rival != Rival::AoValue {
        f.probe("    let c = X ()", "X", format!("{label}/ctor"), sites);
    }
}

/// A module `{prefix}` probing `X`, opening the type's namespace or not.
fn probe_module(
    f: &mut FileBuilder,
    sites: &mut Vec<Site>,
    i: usize,
    prefix: &str,
    open: bool,
    rival: Rival,
    label: &str,
) {
    f.line(&format!(
        "module {prefix}{} =",
        if open { "Open" } else { "Bare" }
    ));
    if open {
        f.line(&format!("    open R{i}.T"));
    }
    probes(
        f,
        sites,
        rival,
        &format!("{label}/{}", if open { "open" } else { "noOpen" }),
    );
}

/// The later blocks' probe modules, every namespace and both `open` choices.
fn later_blocks(
    f: &mut FileBuilder,
    sites: &mut Vec<Site>,
    i: usize,
    prefix: &str,
    rival: Rival,
    label: &str,
) {
    for ns in SiteNs::ALL {
        f.line(&format!("namespace {}", ns.namespace(i)));
        f.line("");
        for open in [false, true] {
            probe_module(
                f,
                sites,
                i,
                prefix,
                open,
                rival,
                &format!("{label}/{}", ns.tag()),
            );
        }
    }
}

/// One cell, over the earlier file `a`, the type's file `b` and the later
/// file `c`.
#[allow(clippy::too_many_arguments)]
fn cell(
    i: usize,
    kind: TypeKind,
    rival: Rival,
    placement: RivalPlacement,
    a: &mut FileBuilder,
    b: &mut FileBuilder,
    c: &mut FileBuilder,
    sites: &mut Vec<Site>,
) {
    let label = format!("{}/{}/{}", kind.tag(), rival.tag(), placement.tag());
    let ns = format!("R{i}.T");
    if placement == RivalPlacement::EarlierFile && rival != Rival::None {
        a.line(&format!("namespace {ns}"));
        a.line("");
        lines(a, &rival.decl());
    }
    let rival_block = |f: &mut FileBuilder| {
        f.line(&format!("namespace {ns}"));
        f.line("");
        lines(f, &rival.decl());
    };
    if placement == RivalPlacement::EarlierBlock && rival != Rival::None {
        rival_block(b);
    }
    b.line(&format!("namespace {ns}"));
    b.line("");
    if placement == RivalPlacement::BlockBefore {
        lines(b, &rival.decl());
    }
    lines(b, &kind.decl());
    if placement == RivalPlacement::BlockAfter {
        lines(b, &rival.decl());
    }
    // The lexical control: the type's own block.
    for open in [false, true] {
        probe_module(b, sites, i, "H", open, rival, &format!("{label}/sameBlock"));
    }
    if placement == RivalPlacement::LaterBlock && rival != Rival::None {
        rival_block(b);
    }
    later_blocks(b, sites, i, "S", rival, &format!("{label}/sameFile"));
    later_blocks(c, sites, i, "L", rival, &format!("{label}/laterFile"));
}

/// Sites that must commit FCS's answer: the lexical control, and a later
/// block of the type's namespace, with no rival and with a rival value that
/// the fragments' order puts before or after the type.
const MUST_COMMIT: &[&str] = &[
    "class/none/blockAfter/sameBlock/noOpen/head",
    "class/none/blockAfter/sameBlock/noOpen/annotation",
    "record/none/blockAfter/sameBlock/noOpen/head",
    "record/none/blockAfter/sameBlock/noOpen/annotation",
    "class/none/blockAfter/sameFile/sameNs/noOpen/head",
    "class/none/blockAfter/sameFile/sameNs/noOpen/annotation",
    "record/none/blockAfter/sameFile/sameNs/noOpen/head",
    "record/none/blockAfter/sameFile/sameNs/noOpen/annotation",
    // The value's fragment precedes the class's: the class evicts it.
    "class/aoValue/earlierBlock/sameFile/sameNs/noOpen/head",
    // The value's fragment is the class's or a later one: the value holds.
    "class/aoValue/blockBefore/sameFile/sameNs/noOpen/head",
    "class/aoValue/blockAfter/sameFile/sameNs/noOpen/head",
    "class/aoValue/laterBlock/sameFile/sameNs/noOpen/head",
    // A record never takes the slot.
    "record/aoValue/earlierBlock/sameFile/sameNs/noOpen/head",
    "record/aoValue/earlierFile/sameFile/sameNs/noOpen/head",
];

/// A dotted head `X.M` where an `[<AutoOpen>]` module's child module `X`
/// contests the type `X`: FCS enters the child as `X` wherever the
/// `[<AutoOpen>]` module is folded, and modules come first for a dotted
/// head, so it binds the module; the walk enters the child nowhere and
/// commits the type (#462 enters it; `reopened_namespace_grid`'s
/// `AUTO_OPEN_CHILD_MODULE`). Not about namespace blocks: the `sameBlock`
/// rows, where the type is lexical, have it too.
const AUTO_OPEN_CHILD_MODULE: &[&str] = &[
    "class/aoModule/blockBefore/sameBlock/noOpen/head",
    "class/aoModule/blockBefore/sameBlock/open/head",
    "class/aoModule/blockBefore/sameFile/sameNs/noOpen/head",
    "class/aoModule/blockBefore/sameFile/sameNs/open/head",
    "class/aoModule/blockAfter/sameBlock/noOpen/head",
    "class/aoModule/blockAfter/sameBlock/open/head",
    "class/aoModule/blockAfter/sameFile/sameNs/noOpen/head",
    "class/aoModule/blockAfter/sameFile/sameNs/open/head",
    "class/aoModule/earlierBlock/sameBlock/noOpen/head",
    "class/aoModule/earlierBlock/sameBlock/open/head",
    "class/aoModule/earlierBlock/sameFile/sameNs/noOpen/head",
    "class/aoModule/earlierBlock/sameFile/sameNs/open/head",
    "class/aoModule/laterBlock/sameFile/sameNs/noOpen/head",
    "class/aoModule/laterBlock/sameFile/sameNs/open/head",
    "class/aoModule/earlierFile/sameBlock/noOpen/head",
    "class/aoModule/earlierFile/sameBlock/open/head",
    "class/aoModule/earlierFile/sameFile/sameNs/noOpen/head",
    "class/aoModule/earlierFile/sameFile/sameNs/open/head",
    "record/aoModule/blockBefore/sameBlock/noOpen/head",
    "record/aoModule/blockBefore/sameBlock/open/head",
    "record/aoModule/blockBefore/sameFile/sameNs/noOpen/head",
    "record/aoModule/blockBefore/sameFile/sameNs/open/head",
    "record/aoModule/blockAfter/sameBlock/noOpen/head",
    "record/aoModule/blockAfter/sameBlock/open/head",
    "record/aoModule/blockAfter/sameFile/sameNs/noOpen/head",
    "record/aoModule/blockAfter/sameFile/sameNs/open/head",
    "record/aoModule/earlierBlock/sameBlock/noOpen/head",
    "record/aoModule/earlierBlock/sameBlock/open/head",
    "record/aoModule/earlierBlock/sameFile/sameNs/noOpen/head",
    "record/aoModule/earlierBlock/sameFile/sameNs/open/head",
    "record/aoModule/laterBlock/sameFile/sameNs/noOpen/head",
    "record/aoModule/laterBlock/sameFile/sameNs/open/head",
    "record/aoModule/earlierFile/sameBlock/noOpen/head",
    "record/aoModule/earlierFile/sameBlock/open/head",
    "record/aoModule/earlierFile/sameFile/sameNs/noOpen/head",
    "record/aoModule/earlierFile/sameFile/sameNs/open/head",
];

#[test]
fn a_type_in_an_earlier_namespace_block_agrees_with_fcs() {
    let mut a = FileBuilder::new(0);
    let mut b = FileBuilder::new(1);
    let mut c = FileBuilder::new(2);
    let mut sites = Vec::new();
    let mut i = 0;
    for kind in TypeKind::ALL {
        for rival in Rival::ALL {
            // A cell with no rival has no placement to vary.
            let placements: &[RivalPlacement] = if rival == Rival::None {
                &[RivalPlacement::BlockAfter]
            } else {
                &RivalPlacement::ALL
            };
            for &placement in placements {
                cell(
                    i, kind, rival, placement, &mut a, &mut b, &mut c, &mut sites,
                );
                i += 1;
            }
        }
    }
    let graded = grade(
        "nbtg",
        &[("a.fs", a.text), ("b.fs", b.text), ("c.fs", c.text)],
        &sites,
    );
    let count = |p: &dyn Fn(&Graded) -> bool| graded.iter().filter(|g| p(g)).count();
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Resolved(..))) > 0);
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Rejected)) > 0);
    assert_sound_and_live(
        "namespace block type",
        &graded,
        AUTO_OPEN_CHILD_MODULE,
        MUST_COMMIT,
    );
}
