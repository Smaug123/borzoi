//! A generated, FCS-graded grid of **project accessibility through `open`**
//! (issue #57, `docs/assembly-module-open-plan.md` "Value-binding
//! accessibility").
//!
//! Two halves, both graded per reference site against `fcs-dump
//! uses-project` and its diagnostics:
//!
//! - **Values.** A `secret` binding under every accessibility (`private`,
//!   `internal`, `public`, none) in every container shape that restricts it
//!   differently — a plain module, a nested module, a `module private` at
//!   either depth, an `[<AutoOpen>]` module (public or `private`) and an
//!   `[<AutoOpen>]` nested module — referenced from inside its module, from
//!   the enclosing module, from a sibling module of the same file, and from a
//!   descendant and an unrelated namespace in the same file and in a later
//!   file, each through an `open` of its module, a qualified path, a bare name
//!   with no `open`, and an `open` of the enclosing namespace.
//! - **Type contestants.** A constructible type `X` under each accessibility
//!   next to an `[<AutoOpen>]` module's value `X`, in every file and block
//!   order, referenced bare from the namespace, a descendant and an unrelated
//!   namespace. FCS's `open` adds only the types accessible where it is
//!   written (`AddModuleOrNamespaceContentsToNameEnv` filters by
//!   `IsEntityAccessible`), and a type it adds takes the unqualified slot from
//!   a same-named value, so whether a `type private X` evicts the value
//!   depends on the reference site.
//!
//! The oracle is the diagnostic-aware one of
//! `resolve_qualified_path_access_gen_diff`: FCS error-recovers an
//! inaccessible use to its declaration (FS1092/1094 and kin), so a use-site
//! accessibility error means the only correct answer is to decline. Otherwise
//! certain-implies-exact: a committed target must be FCS's `(file, range)`,
//! and a site FCS leaves unbound must not commit.

use crate::common::use_grade::{
    FcsVerdict, FileBuilder, Graded, OurVerdict, Site, assert_sound_and_live, grade,
};

// ---- the value half ---------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Access {
    None,
    Private,
    Internal,
    Public,
}

impl Access {
    const ALL: [Access; 4] = [
        Access::None,
        Access::Private,
        Access::Internal,
        Access::Public,
    ];

    fn keyword(self) -> &'static str {
        match self {
            Access::None => "",
            Access::Private => "private ",
            Access::Internal => "internal ",
            Access::Public => "public ",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Access::None => "none",
            Access::Private => "private",
            Access::Internal => "internal",
            Access::Public => "public",
        }
    }
}

/// Where `secret` is declared, relative to the cell's namespace.
#[derive(Clone, Copy, Debug)]
enum Container {
    /// `module A = let secret`.
    Mod,
    /// `module private A = let secret`.
    PrivMod,
    /// `[<AutoOpen>] module A = let secret`.
    AutoOpen,
    /// `[<AutoOpen>] module private A = let secret`.
    PrivAutoOpen,
    /// `module A = module B = let secret`.
    Nested,
    /// `module A = module private B = let secret` (issue #43's value half: the
    /// value inherits `B`'s restriction).
    PrivNested,
    /// `module A = [<AutoOpen>] module B = let secret`.
    AutoOpenNested,
}

impl Container {
    const ALL: [Container; 7] = [
        Container::Mod,
        Container::PrivMod,
        Container::AutoOpen,
        Container::PrivAutoOpen,
        Container::Nested,
        Container::PrivNested,
        Container::AutoOpenNested,
    ];

    fn tag(self) -> &'static str {
        match self {
            Container::Mod => "mod",
            Container::PrivMod => "privMod",
            Container::AutoOpen => "autoOpen",
            Container::PrivAutoOpen => "privAutoOpen",
            Container::Nested => "nested",
            Container::PrivNested => "privNested",
            Container::AutoOpenNested => "autoOpenNested",
        }
    }

    /// The module path holding `secret`, relative to the cell's namespace.
    fn rel(self) -> &'static str {
        match self {
            Container::Mod | Container::PrivMod | Container::AutoOpen | Container::PrivAutoOpen => {
                "A"
            }
            Container::Nested | Container::PrivNested | Container::AutoOpenNested => "A.B",
        }
    }

    fn nested(self) -> bool {
        self.rel() == "A.B"
    }
}

/// The three routes a sibling site can take to `secret`, plus the namespace
/// `open` (which only an auto-open module answers).
fn sibling_sites(
    f: &mut FileBuilder,
    sites: &mut Vec<Site>,
    prefix: &str,
    open_path: &str,
    namespace: Option<&str>,
    label: &str,
) {
    f.line(&format!("module {prefix}Open ="));
    f.line(&format!("    open {open_path}"));
    f.probe(
        "    let v = secret",
        "secret",
        format!("{label}/open"),
        sites,
    );
    f.line(&format!("module {prefix}Qual ="));
    let qual = format!("{open_path}.secret");
    f.probe(
        &format!("    let v = {qual}"),
        &qual,
        format!("{label}/qualified"),
        sites,
    );
    f.line(&format!("module {prefix}Bare ="));
    f.probe(
        "    let v = secret",
        "secret",
        format!("{label}/bare"),
        sites,
    );
    if let Some(ns) = namespace {
        f.line(&format!("module {prefix}NsOpen ="));
        f.line(&format!("    open {ns}"));
        f.probe(
            "    let v = secret",
            "secret",
            format!("{label}/nsOpen"),
            sites,
        );
    }
}

/// One value cell: its definitions and same-file sites appended to `defs`, its
/// later-file sites to `later`.
fn value_cell(
    i: usize,
    access: Access,
    container: Container,
    defs: &mut FileBuilder,
    later: &mut FileBuilder,
    sites: &mut Vec<Site>,
) {
    let ns = format!("N{i}");
    let label = format!("{}/{}", container.tag(), access.tag());
    let acc = access.keyword();
    defs.line(&format!("namespace {ns}"));
    defs.line("");
    match container {
        Container::Mod | Container::PrivMod | Container::AutoOpen | Container::PrivAutoOpen => {
            if matches!(container, Container::AutoOpen | Container::PrivAutoOpen) {
                defs.line("[<AutoOpen>]");
            }
            let private = matches!(container, Container::PrivMod | Container::PrivAutoOpen);
            defs.line(&format!(
                "module {}A =",
                if private { "private " } else { "" }
            ));
            defs.line(&format!("    let {acc}secret = 1"));
            defs.probe(
                "    let inside = secret",
                "secret",
                format!("{label}/inside/bare"),
                sites,
            );
        }
        Container::Nested | Container::PrivNested | Container::AutoOpenNested => {
            defs.line("module A =");
            if matches!(container, Container::AutoOpenNested) {
                defs.line("    [<AutoOpen>]");
            }
            let private = matches!(container, Container::PrivNested);
            defs.line(&format!(
                "    module {}B =",
                if private { "private " } else { "" }
            ));
            defs.line(&format!("        let {acc}secret = 1"));
            defs.probe(
                "        let inside = secret",
                "secret",
                format!("{label}/inside/bare"),
                sites,
            );
            // The enclosing module, after `B`.
            defs.line("    module POpen =");
            defs.line("        open B");
            defs.probe(
                "        let v = secret",
                "secret",
                format!("{label}/parent/open"),
                sites,
            );
            defs.line("    module PQual =");
            defs.probe(
                "        let v = B.secret",
                "B.secret",
                format!("{label}/parent/qualified"),
                sites,
            );
            defs.line("    module PBare =");
            defs.probe(
                "        let v = secret",
                "secret",
                format!("{label}/parent/bare"),
                sites,
            );
        }
    }
    debug_assert_eq!(container.nested(), container.rel() == "A.B");
    let rel = container.rel();
    let full = format!("{ns}.{rel}");
    sibling_sites(
        defs,
        sites,
        "S",
        rel,
        None,
        &format!("{label}/sameFileSibling"),
    );
    defs.line(&format!("namespace {ns}.Sub"));
    defs.line("");
    sibling_sites(
        defs,
        sites,
        "S",
        &full,
        Some(&ns),
        &format!("{label}/sameFileSubNs"),
    );
    defs.line(&format!("namespace O{i}"));
    defs.line("");
    sibling_sites(
        defs,
        sites,
        "S",
        &full,
        Some(&ns),
        &format!("{label}/sameFileOtherNs"),
    );

    later.line(&format!("namespace {ns}"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        rel,
        None,
        &format!("{label}/laterSameNs"),
    );
    later.line(&format!("namespace {ns}.Sub"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        &full,
        Some(&ns),
        &format!("{label}/laterSubNs"),
    );
    later.line(&format!("namespace O{i}"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        &full,
        Some(&ns),
        &format!("{label}/laterOtherNs"),
    );
}

// ---- the type-contestant half ----------------------------------------------

/// Where the constructible type `X` sits relative to the `[<AutoOpen>]`
/// module `A` whose value `X` it may evict.
#[derive(Clone, Copy, Debug)]
enum TypePlacement {
    /// Same file, the type's block after `A`'s.
    SameFileAfter,
    /// Same file, the type's block before `A`'s.
    SameFileBefore,
    /// `A` in the first file, the type in the second.
    LaterFile,
    /// The type in the first file, `A` in the second.
    EarlierFile,
    /// The type inside a later `[<AutoOpen>]` module `B` of the same file.
    LaterFragment,
    /// The type inside an `[<AutoOpen>]` module `B` of the second file.
    LaterFileFragment,
}

impl TypePlacement {
    const ALL: [TypePlacement; 6] = [
        TypePlacement::SameFileAfter,
        TypePlacement::SameFileBefore,
        TypePlacement::LaterFile,
        TypePlacement::EarlierFile,
        TypePlacement::LaterFragment,
        TypePlacement::LaterFileFragment,
    ];

    fn tag(self) -> &'static str {
        match self {
            TypePlacement::SameFileAfter => "sameFileAfter",
            TypePlacement::SameFileBefore => "sameFileBefore",
            TypePlacement::LaterFile => "laterFile",
            TypePlacement::EarlierFile => "earlierFile",
            TypePlacement::LaterFragment => "laterFragment",
            TypePlacement::LaterFileFragment => "laterFileFragment",
        }
    }
}

fn auto_open_value(f: &mut FileBuilder) {
    f.line("[<AutoOpen>]");
    f.line("module A =");
    f.line("    let X = {| M = 1 |}");
}

fn constructible_type(f: &mut FileBuilder, acc: &str, fragment: bool) {
    if fragment {
        f.line("[<AutoOpen>]");
        f.line("module B =");
        f.line(&format!("    type {acc}X() ="));
        f.line("        static member M = 0");
    } else {
        f.line(&format!("type {acc}X() ="));
        f.line("    static member M = 0");
    }
}

/// A bare `X` and the head of `X.M`, as two probes.
fn probe_x(f: &mut FileBuilder, sites: &mut Vec<Site>, label: String) {
    f.probe("    let v = X", "X", format!("{label}/bare"), sites);
    f.probe("    let w = X.M", "X", format!("{label}/head"), sites);
}

/// The sites of a type cell: the namespace itself, a descendant namespace
/// (bare, and through an `open` of the namespace) and an unrelated namespace
/// that opens it, each probing a bare `X` and the head of `X.M` (member access
/// on the value, or the type's static member).
fn contestant_sites(
    f: &mut FileBuilder,
    sites: &mut Vec<Site>,
    ns: &str,
    i: usize,
    prefix: &str,
    label: &str,
) {
    f.line(&format!("namespace {ns}"));
    f.line("");
    f.line(&format!("module {prefix}Here ="));
    probe_x(f, sites, format!("{label}/sameNs"));
    f.line(&format!("namespace {ns}.Sub"));
    f.line("");
    f.line(&format!("module {prefix}Bare ="));
    probe_x(f, sites, format!("{label}/subNs/noOpen"));
    f.line(&format!("module {prefix}Open ="));
    f.line(&format!("    open {ns}"));
    probe_x(f, sites, format!("{label}/subNs/nsOpen"));
    f.line(&format!("namespace Q{i}"));
    f.line("");
    f.line(&format!("module {prefix}Open ="));
    f.line(&format!("    open {ns}"));
    probe_x(f, sites, format!("{label}/otherNs/nsOpen"));
}

/// One type cell over three files: the two definition files (with same-file
/// sites closing the second) and a later file of sites.
fn type_cell(
    i: usize,
    access: Access,
    placement: TypePlacement,
    files: &mut [FileBuilder; 3],
    sites: &mut Vec<Site>,
) {
    let ns = format!("T{i}");
    let label = format!("type/{}/{}", placement.tag(), access.tag());
    let acc = access.keyword();
    let [first, second, third] = files;
    first.line(&format!("namespace {ns}"));
    first.line("");
    let two_files = match placement {
        TypePlacement::SameFileAfter => {
            auto_open_value(first);
            constructible_type(first, acc, false);
            false
        }
        TypePlacement::SameFileBefore => {
            constructible_type(first, acc, false);
            auto_open_value(first);
            false
        }
        TypePlacement::LaterFragment => {
            auto_open_value(first);
            constructible_type(first, acc, true);
            false
        }
        TypePlacement::LaterFile
        | TypePlacement::EarlierFile
        | TypePlacement::LaterFileFragment => {
            second.line(&format!("namespace {ns}"));
            second.line("");
            match placement {
                TypePlacement::LaterFile => {
                    auto_open_value(first);
                    constructible_type(second, acc, false);
                }
                TypePlacement::EarlierFile => {
                    constructible_type(first, acc, false);
                    auto_open_value(second);
                }
                _ => {
                    auto_open_value(first);
                    constructible_type(second, acc, true);
                }
            }
            true
        }
    };
    // Same-file sites follow the last definition; later-file sites get a file
    // of their own.
    let last_def = if two_files { second } else { first };
    contestant_sites(last_def, sites, &ns, i, "S", &format!("{label}/sameFile"));
    contestant_sites(third, sites, &ns, i, "L", &format!("{label}/laterFile"));
}

// ---- the signature half -----------------------------------------------------

/// How a signature file declares the implementation's public `secret`.
#[derive(Clone, Copy, Debug)]
enum SigAccess {
    /// `val secret`, `val private secret`, `val internal secret`.
    Declared(Access),
    /// Not declared at all.
    Hidden,
}

impl SigAccess {
    const ALL: [SigAccess; 4] = [
        SigAccess::Declared(Access::None),
        SigAccess::Declared(Access::Private),
        SigAccess::Declared(Access::Internal),
        SigAccess::Hidden,
    ];

    fn tag(self) -> &'static str {
        match self {
            SigAccess::Declared(a) => a.tag(),
            SigAccess::Hidden => "hidden",
        }
    }
}

/// One signature cell: the signature and implementation of `secret` in a plain
/// or nested module, with every site in a later file (the implementation file
/// itself sees the implementation, not the signature).
fn sig_cell(
    i: usize,
    access: SigAccess,
    nested: bool,
    sig: &mut FileBuilder,
    imp: &mut FileBuilder,
    later: &mut FileBuilder,
    sites: &mut Vec<Site>,
) {
    let ns = format!("N{i}");
    let label = format!(
        "sig/{}/{}",
        if nested { "nested" } else { "mod" },
        access.tag()
    );
    let declared = match access {
        SigAccess::Declared(a) => Some(format!("val {}secret: int", a.keyword())),
        SigAccess::Hidden => None,
    };
    for f in [&mut *sig, &mut *imp] {
        f.line(&format!("namespace {ns}"));
        f.line("");
        f.line("module A =");
    }
    let indent = if nested {
        sig.line("    module B =");
        imp.line("    module B =");
        "        "
    } else {
        "    "
    };
    sig.line(&format!("{indent}val filler: int"));
    if let Some(declared) = &declared {
        sig.line(&format!("{indent}{declared}"));
    }
    imp.line(&format!("{indent}let filler = 0"));
    imp.line(&format!("{indent}let secret = 1"));
    let rel = if nested { "A.B" } else { "A" };
    let full = format!("{ns}.{rel}");
    later.line(&format!("namespace {ns}"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        rel,
        None,
        &format!("{label}/laterSameNs"),
    );
    later.line(&format!("namespace {ns}.Sub"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        &full,
        Some(&ns),
        &format!("{label}/laterSubNs"),
    );
    later.line(&format!("namespace O{i}"));
    later.line("");
    sibling_sites(
        later,
        sites,
        "L",
        &full,
        Some(&ns),
        &format!("{label}/laterOtherNs"),
    );
}

// ---- grading ----------------------------------------------------------------

#[test]
fn value_accessibility_through_open_agrees_with_fcs() {
    let mut defs = FileBuilder::new(0);
    let mut later = FileBuilder::new(1);
    let mut sites = Vec::new();
    let mut i = 0;
    for container in Container::ALL {
        for access in Access::ALL {
            value_cell(i, access, container, &mut defs, &mut later, &mut sites);
            i += 1;
        }
    }
    let graded = grade(
        "vaog",
        &[("defs.fs", defs.text), ("later.fs", later.text)],
        &sites,
    );

    // Non-vacuity: every arm of the oracle is exercised, and agreement is
    // reached across files.
    let count = |p: &dyn Fn(&Graded) -> bool| graded.iter().filter(|g| p(g)).count();
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Inaccessible)) > 0);
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Rejected)) > 0);
    assert!(
        count(&|g| matches!(
            (&g.fcs, &g.ours),
            (FcsVerdict::Resolved(0, _), OurVerdict::Committed(0, _))
        ) && sites.iter().any(|s| s.label == g.label && s.file == 1))
            > 0,
        "no later-file site agreed on a first-file declaration"
    );
    assert_sound_and_live("value", &graded, &[], &[]);
}

#[test]
fn type_contestant_accessibility_agrees_with_fcs() {
    let mut files = [
        FileBuilder::new(0),
        FileBuilder::new(1),
        FileBuilder::new(2),
    ];
    let mut sites = Vec::new();
    let mut i = 0;
    for placement in TypePlacement::ALL {
        for access in [Access::None, Access::Private, Access::Internal] {
            type_cell(i, access, placement, &mut files, &mut sites);
            i += 1;
        }
    }
    let [a, b, c] = files;
    let graded = grade(
        "vaog",
        &[("a.fs", a.text), ("b.fs", b.text), ("c.fs", c.text)],
        &sites,
    );
    let count = |p: &dyn Fn(&Graded) -> bool| graded.iter().filter(|g| p(g)).count();
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Resolved(..))) > 0);
    assert_sound_and_live("type contestant", &graded, &[], &[]);
}

#[test]
fn signature_restricted_values_agree_with_fcs() {
    let mut sig = FileBuilder::new(0);
    let mut imp = FileBuilder::new(1);
    let mut later = FileBuilder::new(2);
    let mut sites = Vec::new();
    let mut i = 0;
    for nested in [false, true] {
        for access in SigAccess::ALL {
            sig_cell(
                i, access, nested, &mut sig, &mut imp, &mut later, &mut sites,
            );
            i += 1;
        }
    }
    let graded = grade(
        "vaog",
        &[
            ("defs.fsi", sig.text),
            ("defs.fs", imp.text),
            ("later.fs", later.text),
        ],
        &sites,
    );
    let count = |p: &dyn Fn(&Graded) -> bool| graded.iter().filter(|g| p(g)).count();
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Resolved(..))) > 0);
    assert!(count(&|g| !matches!(g.fcs, FcsVerdict::Resolved(..))) > 0);
    assert_sound_and_live("signature", &graded, &[], &[]);
}
