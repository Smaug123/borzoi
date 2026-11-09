//! A generated, FCS-graded grid of **union-case accessibility** (issues #474
//! and #43, `docs/sema-accessibility-collapse-foundation.md`).
//!
//! A union case can be out of reach for four reasons, and FCS treats them
//! differently by route:
//!
//! - the **representation** is restricted (`type U = private X | Y`): the type
//!   stays public, so an `open` still adds the cases, a type-qualified `U.X`
//!   still finds them, and either use is then an FS1093 error; a
//!   module-qualified `M.X` skips them (`IsTyconReprAccessible`) and looks on;
//! - the **type** is restricted (`type private U = X | Y`): an `open` does not
//!   add the type or its cases at all, so an earlier same-named name stays
//!   visible;
//! - an **enclosing module** is restricted, the union's own (`module private A`)
//!   or one further out (`module private A = module B = …`);
//! - a **signature file** restricts any of these, or hides the representation
//!   or the type.
//!
//! Each restriction is crossed with the accessibility it is written with
//! (`private`, `internal`, `public`; one project is one assembly, so only
//! `private` restricts anything here) and with reference sites inside the
//! union's module, in a nested module of it, in the enclosing module, in a
//! sibling module and a module nested in one, in a descendant and an unrelated
//! namespace, in the same file and in a later file. Every site takes each route:
//! expression `P.X`, pattern `P.X`, `open P` then `X` (in an expression and in a
//! pattern, and over an earlier `open` that supplies a value `X`), and the
//! type-qualified `P.U.X`.
//!
//! The oracle is [`crate::common::use_grade`]'s diagnostic-aware one: FCS
//! error-recovers an inaccessible use to its declaration, so a use-site
//! accessibility error means the only correct answer is to decline. Otherwise
//! certain-implies-exact: a committed target must be FCS's `(file, range)`, and
//! a site FCS leaves unbound or rejects must not commit. [`MUST_COMMIT`] keeps
//! the grid from passing by declining everything.

use crate::common::use_grade::{
    FcsVerdict, FileBuilder, Graded, Site, assert_sound_and_live, grade,
};

// ---- the grid's axes ---------------------------------------------------------

/// The accessibility a restriction is written with.
#[derive(Clone, Copy, Debug)]
enum Access {
    Private,
    Internal,
    Public,
}

impl Access {
    const ALL: [Access; 3] = [Access::Private, Access::Internal, Access::Public];

    fn keyword(self) -> &'static str {
        match self {
            Access::Private => "private ",
            Access::Internal => "internal ",
            Access::Public => "public ",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Access::Private => "private",
            Access::Internal => "internal",
            Access::Public => "public",
        }
    }
}

/// Where a restriction is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// `module A = type U = {acc}X | Y`.
    Repr,
    /// `module A = type {acc}U = X | Y`.
    Type,
    /// `module {acc}A = type U = X | Y`: the union's own module.
    Parent,
    /// `module A = module {acc}B = type U = X | Y`: the union's own module, one
    /// level down.
    NestedParent,
    /// `module {acc}A = module B = type U = X | Y`: a module two levels out.
    Grandparent,
}

impl Source {
    const ALL: [Source; 5] = [
        Source::Repr,
        Source::Type,
        Source::Parent,
        Source::NestedParent,
        Source::Grandparent,
    ];

    fn tag(self) -> &'static str {
        match self {
            Source::Repr => "repr",
            Source::Type => "type",
            Source::Parent => "parent",
            Source::NestedParent => "nestedParent",
            Source::Grandparent => "grandparent",
        }
    }

    fn nested(self) -> bool {
        matches!(self, Source::NestedParent | Source::Grandparent)
    }
}

/// A cell's restriction: none, or one source written with one accessibility.
#[derive(Clone, Copy, Debug)]
enum Restriction {
    Plain,
    At(Source, Access),
}

impl Restriction {
    /// Every restriction whose source is one of `sources`, and none.
    fn over(sources: &[Source]) -> Vec<Restriction> {
        let mut out = vec![Restriction::Plain];
        for &source in sources {
            for access in Access::ALL {
                out.push(Restriction::At(source, access));
            }
        }
        out
    }

    fn tag(self) -> String {
        match self {
            Restriction::Plain => "plain".to_string(),
            Restriction::At(s, a) => format!("{}/{}", s.tag(), a.tag()),
        }
    }

    fn keyword_at(self, source: Source) -> &'static str {
        match self {
            Restriction::At(s, a) if s == source => a.keyword(),
            _ => "",
        }
    }

    fn nested(self) -> bool {
        matches!(self, Restriction::At(s, _) if s.nested())
    }
}

/// What holds the union.
#[derive(Clone, Copy, Debug)]
enum Container {
    /// `module A` (or `module A = module B` for a nested restriction).
    Module,
    /// `[<AutoOpen>] module A`, whose cases an `open` of the namespace folds.
    AutoOpen,
    /// The namespace itself.
    Namespace,
}

impl Container {
    const ALL: [Container; 3] = [Container::Module, Container::AutoOpen, Container::Namespace];

    fn tag(self) -> &'static str {
        match self {
            Container::Module => "",
            Container::AutoOpen => "autoOpen/",
            Container::Namespace => "namespace/",
        }
    }

    /// The restrictions this container can carry: a module restriction needs a
    /// module.
    fn restrictions(self) -> Vec<Restriction> {
        match self {
            Container::Module => Restriction::over(&Source::ALL),
            Container::AutoOpen | Container::Namespace => {
                Restriction::over(&[Source::Repr, Source::Type])
            }
        }
    }
}

/// The sites inside the union's container, at indentation `ind`.
fn inside_sites(f: &mut FileBuilder, sites: &mut Vec<Site>, ind: &str, label: &str) {
    f.probe(
        &format!("{ind}let insideBare = X"),
        "X",
        format!("{label}/inside/bare"),
        sites,
    );
    f.probe(
        &format!("{ind}let insideTq = U.X"),
        "U.X",
        format!("{label}/inside/typeQualified"),
        sites,
    );
    f.probe(
        &format!("{ind}let insidePat u = match u with X -> 1 | _ -> 0"),
        "X",
        format!("{label}/inside/pattern"),
        sites,
    );
    f.line(&format!("{ind}module Inner ="));
    f.probe(
        &format!("{ind}    let v = X"),
        "X",
        format!("{label}/innerModule/bare"),
        sites,
    );
    f.probe(
        &format!("{ind}    let w = U.X"),
        "U.X",
        format!("{label}/innerModule/typeQualified"),
        sites,
    );
}

/// Write the union in `container` (and, under `nested`, the module `B`
/// between), with the accessibility keywords `restriction` puts at each slot,
/// followed, given `with_sites`, by the sites inside the union's container.
/// The text is valid in a signature file too.
fn union_decl(
    f: &mut FileBuilder,
    container: Container,
    restriction: Restriction,
    nested: bool,
    with_sites: Option<(&mut Vec<Site>, &str)>,
) {
    let ind = match container {
        Container::Namespace => "",
        Container::Module | Container::AutoOpen => {
            if matches!(container, Container::AutoOpen) {
                f.line("[<AutoOpen>]");
            }
            let a = restriction.keyword_at(Source::Parent).to_string()
                + restriction.keyword_at(Source::Grandparent);
            f.line(&format!("module {a}A ="));
            if nested {
                let b = restriction.keyword_at(Source::NestedParent);
                f.line(&format!("    module {b}B ="));
                "        "
            } else {
                "    "
            }
        }
    };
    let ty = restriction.keyword_at(Source::Type);
    let repr = restriction.keyword_at(Source::Repr);
    f.line(&format!("{ind}type {ty}U = {repr}X | Y"));
    if let Some((sites, label)) = with_sites {
        if matches!(container, Container::Namespace) {
            f.line("module Here =");
            inside_sites(f, sites, "    ", label);
        } else {
            inside_sites(f, sites, ind, label);
        }
    }
}

/// How a site spells its way to the union: `path` names the union's container
/// (a module, or the namespace itself), `fallback` the module `F` whose value
/// `X` an earlier `open` brings in, and `namespace`, where given, the
/// namespace an `open` of which folds an `[<AutoOpen>]` container.
struct Spelling<'a> {
    path: &'a str,
    fallback: &'a str,
    namespace: Option<&'a str>,
}

/// The routes from one site to the union's case, each in its own module named
/// `{prefix}{Route}` at indentation `ind`.
fn route_sites(
    f: &mut FileBuilder,
    sites: &mut Vec<Site>,
    ind: &str,
    prefix: &str,
    to: &Spelling,
    label: &str,
) {
    let path = to.path;
    let qual = format!("{path}.X");
    f.line(&format!("{ind}module {prefix}Expr ="));
    f.probe(
        &format!("{ind}    let v = {qual}"),
        &qual,
        format!("{label}/qualifiedExpr"),
        sites,
    );
    f.line(&format!("{ind}module {prefix}Pat ="));
    f.probe(
        &format!("{ind}    let g u = match u with {qual} -> 1 | _ -> 0"),
        &qual,
        format!("{label}/qualifiedPat"),
        sites,
    );
    f.line(&format!("{ind}module {prefix}Open ="));
    f.line(&format!("{ind}    open {path}"));
    f.probe(
        &format!("{ind}    let v = X"),
        "X",
        format!("{label}/openExpr"),
        sites,
    );
    f.probe(
        &format!("{ind}    let g u = match u with X -> 1 | _ -> 0"),
        "X",
        format!("{label}/openPat"),
        sites,
    );
    f.line(&format!("{ind}module {prefix}Shadow ="));
    f.line(&format!("{ind}    open {}", to.fallback));
    f.line(&format!("{ind}    open {path}"));
    f.probe(
        &format!("{ind}    let v = X"),
        "X",
        format!("{label}/openOverFallback"),
        sites,
    );
    let tq = format!("{path}.U.X");
    f.line(&format!("{ind}module {prefix}Tq ="));
    f.probe(
        &format!("{ind}    let v = {tq}"),
        &tq,
        format!("{label}/typeQualified"),
        sites,
    );
    f.line(&format!("{ind}module {prefix}Bare ="));
    f.probe(
        &format!("{ind}    let v = X"),
        "X",
        format!("{label}/bare"),
        sites,
    );
    if let Some(ns) = to.namespace {
        f.line(&format!("{ind}module {prefix}NsOpen ="));
        f.line(&format!("{ind}    open {ns}"));
        f.probe(
            &format!("{ind}    let v = X"),
            "X",
            format!("{label}/nsOpen"),
            sites,
        );
    }
}

/// The fallback module every cell's namespace opens to bring a value `X` into
/// scope ahead of the union's module.
fn fallback_module(f: &mut FileBuilder) {
    f.line("module F =");
    f.line("    let X = 0");
}

/// The sites outside the union's container, in namespace `ns` (whose union
/// container is `rel` within it, `None` for the namespace itself) and in its
/// descendant and an unrelated namespace `O{i}`, written as blocks of `f` with
/// module prefix `prefix`.
fn outside_sites(
    f: &mut FileBuilder,
    sites: &mut Vec<Site>,
    ns: &str,
    rel: Option<&str>,
    i: usize,
    prefix: &str,
    label: &str,
) {
    let full = rel.map_or_else(|| ns.to_string(), |rel| format!("{ns}.{rel}"));
    let full_fallback = format!("{ns}.F");
    let near = Spelling {
        path: rel.unwrap_or(ns),
        fallback: "F",
        namespace: None,
    };
    let far = Spelling {
        path: &full,
        fallback: &full_fallback,
        // For the namespace container, an `open` of the namespace is already
        // the `open` route.
        namespace: rel.map(|_| ns),
    };
    route_sites(f, sites, "", prefix, &near, &format!("{label}/sameNs"));
    f.line(&format!("module {prefix}Deep ="));
    route_sites(
        f,
        sites,
        "    ",
        prefix,
        &near,
        &format!("{label}/sameNsNested"),
    );
    f.line(&format!("namespace {ns}.Sub"));
    f.line("");
    route_sites(f, sites, "", prefix, &far, &format!("{label}/subNs"));
    f.line(&format!("namespace O{i}"));
    f.line("");
    route_sites(f, sites, "", prefix, &far, &format!("{label}/otherNs"));
}

/// One implementation cell: its definitions and same-file sites appended to
/// `defs`, its later-file sites to `later`.
fn impl_cell(
    i: usize,
    container: Container,
    restriction: Restriction,
    defs: &mut FileBuilder,
    later: &mut FileBuilder,
    sites: &mut Vec<Site>,
) {
    let ns = format!("C{i}");
    let label = format!("{}{}", container.tag(), restriction.tag());
    let nested = restriction.nested();
    let rel = match container {
        Container::Namespace => None,
        Container::Module | Container::AutoOpen if nested => Some("A.B"),
        Container::Module | Container::AutoOpen => Some("A"),
    };
    defs.line(&format!("namespace {ns}"));
    defs.line("");
    fallback_module(defs);
    union_decl(defs, container, restriction, nested, Some((sites, &label)));
    if nested {
        // The enclosing module `A`, after `B`.
        let parent = Spelling {
            path: "B",
            fallback: "F",
            namespace: None,
        };
        route_sites(
            defs,
            sites,
            "    ",
            "P",
            &parent,
            &format!("{label}/parent"),
        );
    }
    outside_sites(defs, sites, &ns, rel, i, "S", &format!("{label}/sameFile"));

    later.line(&format!("namespace {ns}"));
    later.line("");
    outside_sites(
        later,
        sites,
        &ns,
        rel,
        i,
        "L",
        &format!("{label}/laterFile"),
    );
}

/// How a signature file restricts an implementation whose union is plain.
#[derive(Clone, Copy, Debug)]
enum SigShape {
    /// The signature repeats the implementation.
    Plain,
    /// The signature writes the restriction (repr, type, or a module).
    Restricted(Source, Access),
    /// `type U`: the representation is hidden.
    Abstract,
    /// The type is left out.
    Omitted,
}

impl SigShape {
    fn all() -> Vec<SigShape> {
        let mut out = vec![SigShape::Plain, SigShape::Abstract, SigShape::Omitted];
        for source in Source::ALL {
            for access in Access::ALL {
                out.push(SigShape::Restricted(source, access));
            }
        }
        out
    }

    fn tag(self) -> String {
        match self {
            SigShape::Plain => "sig/plain".to_string(),
            SigShape::Restricted(s, a) => format!("sig/{}/{}", s.tag(), a.tag()),
            SigShape::Abstract => "sig/abstract".to_string(),
            SigShape::Omitted => "sig/omitted".to_string(),
        }
    }

    fn nested(self) -> bool {
        matches!(self, SigShape::Restricted(s, _) if s.nested())
    }
}

/// One signature cell: the signature and plain implementation of the union,
/// with every site in a later file (the implementation file itself sees the
/// implementation, not the signature).
fn sig_cell(
    i: usize,
    shape: SigShape,
    sig: &mut FileBuilder,
    imp: &mut FileBuilder,
    later: &mut FileBuilder,
    sites: &mut Vec<Site>,
) {
    let ns = format!("C{i}");
    let label = shape.tag();
    let nested = shape.nested();
    let rel = if nested { "A.B" } else { "A" };
    for f in [&mut *sig, &mut *imp] {
        f.line(&format!("namespace {ns}"));
        f.line("");
    }
    sig.line("module F =");
    sig.line("    val X: int");
    fallback_module(imp);
    let module = Container::Module;
    union_decl(imp, module, Restriction::Plain, nested, None);
    let ind = if nested { "        " } else { "    " };
    match shape {
        SigShape::Plain => union_decl(sig, module, Restriction::Plain, nested, None),
        SigShape::Restricted(source, access) => {
            union_decl(sig, module, Restriction::At(source, access), nested, None);
        }
        SigShape::Abstract => {
            sig.line("module A =");
            sig.line(&format!("{ind}type U"));
        }
        SigShape::Omitted => {
            sig.line("module A =");
            sig.line(&format!("{ind}val filler: int"));
        }
    }
    if matches!(shape, SigShape::Omitted) {
        imp.line(&format!("{ind}let filler = 0"));
    }
    later.line(&format!("namespace {ns}"));
    later.line("");
    let label = format!("{label}/laterFile");
    outside_sites(later, sites, &ns, Some(rel), i, "L", &label);
}

// ---- grading ----------------------------------------------------------------

/// The implementation grid's controls: sites where FCS binds cleanly and we
/// must commit its answer, covering every route that commits today, each
/// restriction source, and the fallback an inaccessible type or module leaves
/// visible — so a change that declines whole routes or cells fails here rather
/// than passing vacuously. (A module-qualified pattern never commits, #473.)
const MUST_COMMIT: &[&str] = &[
    // No restriction: every committing route, in the same file and a later one.
    "plain/inside/bare",
    "plain/inside/typeQualified",
    "plain/inside/pattern",
    "plain/innerModule/bare",
    "plain/sameFile/sameNs/qualifiedExpr",
    "plain/sameFile/sameNs/openExpr",
    "plain/sameFile/sameNs/openPat",
    "plain/sameFile/sameNs/openOverFallback",
    "plain/sameFile/sameNs/typeQualified",
    "plain/laterFile/sameNs/qualifiedExpr",
    "plain/laterFile/otherNs/qualifiedExpr",
    "plain/laterFile/otherNs/openExpr",
    "plain/laterFile/otherNs/openPat",
    "plain/laterFile/otherNs/openOverFallback",
    "plain/laterFile/otherNs/typeQualified",
    // A `private` representation: usable inside its module, at every depth.
    "repr/private/inside/bare",
    "repr/private/inside/typeQualified",
    "repr/private/inside/pattern",
    "repr/private/innerModule/bare",
    "repr/private/innerModule/typeQualified",
    // `internal` and `public` representations restrict nothing here.
    "repr/internal/laterFile/otherNs/qualifiedExpr",
    "repr/internal/laterFile/otherNs/openExpr",
    "repr/internal/laterFile/otherNs/typeQualified",
    "repr/public/laterFile/otherNs/openPat",
    "repr/public/laterFile/otherNs/typeQualified",
    // A `private` type hides its cases from an `open`, so the fallback binds.
    "type/private/inside/typeQualified",
    "type/private/laterFile/otherNs/openOverFallback",
    "type/private/sameFile/sameNs/openOverFallback",
    // A `private` module is visible within its parent, in any file.
    "parent/private/laterFile/sameNs/qualifiedExpr",
    "parent/private/laterFile/sameNs/openExpr",
    "parent/private/laterFile/subNs/typeQualified",
    "parent/private/laterFile/otherNs/openOverFallback",
    "nestedParent/private/parent/openExpr",
    "nestedParent/private/parent/typeQualified",
    "nestedParent/private/laterFile/sameNs/openOverFallback",
    "grandparent/private/laterFile/subNs/qualifiedExpr",
    "grandparent/private/laterFile/sameNs/openExpr",
    // An `[<AutoOpen>]` container folds through an `open` of its namespace.
    "autoOpen/plain/laterFile/otherNs/nsOpen",
    "autoOpen/repr/private/inside/bare",
    // A namespace-level union.
    "namespace/plain/laterFile/otherNs/openExpr",
    "namespace/repr/private/inside/bare",
];

/// The signature grid's controls (see [`MUST_COMMIT`]): a signature that
/// repeats the implementation, or restricts only a module the sites are inside
/// or one that restricts nothing in one project.
const SIG_MUST_COMMIT: &[&str] = &[
    "sig/plain/laterFile/sameNs/qualifiedExpr",
    "sig/plain/laterFile/otherNs/openExpr",
    "sig/plain/laterFile/otherNs/openOverFallback",
    "sig/plain/laterFile/otherNs/typeQualified",
    "sig/parent/private/laterFile/sameNs/openExpr",
    "sig/parent/internal/laterFile/otherNs/typeQualified",
];

#[test]
fn case_accessibility_agrees_with_fcs() {
    let mut defs = FileBuilder::new(0);
    let mut later = FileBuilder::new(1);
    let mut sites = Vec::new();
    let cells = Container::ALL
        .into_iter()
        .flat_map(|c| c.restrictions().into_iter().map(move |r| (c, r)));
    for (i, (container, restriction)) in cells.enumerate() {
        impl_cell(i, container, restriction, &mut defs, &mut later, &mut sites);
    }
    let graded = grade(
        "cag",
        &[("defs.fs", defs.text), ("later.fs", later.text)],
        &sites,
    );
    let count = |p: &dyn Fn(&Graded) -> bool| graded.iter().filter(|g| p(g)).count();
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Inaccessible)) > 0);
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Rejected)) > 0);
    assert_sound_and_live("case", &graded, &[], MUST_COMMIT);
}

#[test]
fn signature_restricted_cases_agree_with_fcs() {
    let mut sig = FileBuilder::new(0);
    let mut imp = FileBuilder::new(1);
    let mut later = FileBuilder::new(2);
    let mut sites = Vec::new();
    for (i, shape) in SigShape::all().into_iter().enumerate() {
        sig_cell(i, shape, &mut sig, &mut imp, &mut later, &mut sites);
    }
    let graded = grade(
        "cag",
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
    assert_sound_and_live("signature", &graded, &[], SIG_MUST_COMMIT);
}

/// A headerless file: its cases have no cross-file export and stay
/// file-local binders, which still reach a sibling module through `A.U.X`.
/// One project per restriction, since each reuses the module names.
#[test]
fn anonymous_root_cases_agree_with_fcs() {
    let restrictions = [
        Restriction::Plain,
        Restriction::At(Source::Repr, Access::Private),
        Restriction::At(Source::Type, Access::Private),
        Restriction::At(Source::Parent, Access::Private),
    ];
    let mut graded = Vec::new();
    for restriction in restrictions {
        let mut f = FileBuilder::new(0);
        let mut sites = Vec::new();
        let label = format!("anonymous/{}", restriction.tag());
        fallback_module(&mut f);
        union_decl(
            &mut f,
            Container::Module,
            restriction,
            false,
            Some((&mut sites, &label)),
        );
        let to = Spelling {
            path: "A",
            fallback: "F",
            namespace: None,
        };
        let sibling = format!("{label}/sibling");
        route_sites(&mut f, &mut sites, "", "S", &to, &sibling);
        graded.extend(grade("cag", &[("Main.fs", f.text)], &sites));
    }
    // An `open` of an anonymous-root module declines (its cases are not
    // exported, so the module is hidden); `A.U.X` is the route that reaches a
    // sibling.
    let must_commit = [
        "anonymous/plain/sibling/typeQualified",
        "anonymous/parent/private/sibling/typeQualified",
        "anonymous/repr/private/inside/bare",
        "anonymous/repr/private/innerModule/typeQualified",
    ];
    let count = |p: &dyn Fn(&Graded) -> bool| graded.iter().filter(|g| p(g)).count();
    assert!(count(&|g| matches!(g.fcs, FcsVerdict::Inaccessible)) > 0);
    assert_sound_and_live("anonymous-root", &graded, &[], &must_commit);
}

/// Two unions of one module both declaring `X`, one of them with a `private`
/// representation, in either order: the case an `open` adds last, the one
/// `U1.X`/`U2.X` names, and the first union `A.X` reads (#465) may each be
/// the unusable one.
#[test]
fn a_private_representation_among_duplicate_case_names_agrees_with_fcs() {
    let mut defs = FileBuilder::new(0);
    let mut later = FileBuilder::new(1);
    let mut sites = Vec::new();
    for (i, private_first) in [true, false].into_iter().enumerate() {
        let ns = format!("D{i}");
        let label = format!(
            "dup/{}",
            if private_first {
                "privateFirst"
            } else {
                "privateSecond"
            }
        );
        let (first, second) = if private_first {
            ("private ", "")
        } else {
            ("", "private ")
        };
        defs.line(&format!("namespace {ns}"));
        defs.line("");
        fallback_module(&mut defs);
        defs.line("module A =");
        defs.line(&format!("    type U1 = {first}X | Y"));
        defs.line(&format!("    type U2 = {second}X | Z"));
        defs.probe(
            "    let inside = X",
            "X",
            format!("{label}/inside/bare"),
            &mut sites,
        );
        for (f, prefix, site, path, fallback) in [
            (&mut defs, "S", "sameFile", "A".to_string(), "F".to_string()),
            (
                &mut later,
                "L",
                "laterFile",
                format!("{ns}.A"),
                format!("{ns}.F"),
            ),
        ] {
            if site == "laterFile" {
                f.line(&format!("namespace O{i}"));
                f.line("");
            }
            let to = Spelling {
                path: &path,
                fallback: &fallback,
                namespace: None,
            };
            let label = format!("{label}/{site}");
            route_sites(f, &mut sites, "", prefix, &to, &label);
            for ty in ["U1", "U2"] {
                let tq = format!("{path}.{ty}.X");
                f.line(&format!("module {prefix}{ty} ="));
                f.probe(
                    &format!("    let v = {tq}"),
                    &tq,
                    format!("{label}/typeQualified{ty}"),
                    &mut sites,
                );
            }
        }
    }
    let graded = grade(
        "cag",
        &[("defs.fs", defs.text), ("later.fs", later.text)],
        &sites,
    );
    // `A.X` takes the module's last union declaring `X`, where FCS reads the
    // first (#469, which #465 fixes by declining a name declared twice).
    let first_union_rule = [
        "dup/privateFirst/sameFile/qualifiedExpr",
        "dup/privateFirst/laterFile/qualifiedExpr",
    ];
    let must_commit = [
        "dup/privateFirst/inside/bare",
        "dup/privateFirst/laterFile/openExpr",
        "dup/privateFirst/laterFile/typeQualifiedU2",
        "dup/privateSecond/laterFile/typeQualifiedU1",
    ];
    assert_sound_and_live("duplicate", &graded, &first_union_rule, &must_commit);
}
