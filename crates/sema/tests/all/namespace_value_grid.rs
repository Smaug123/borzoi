//! A generated, FCS-graded grid of **values declared directly in a
//! namespace**.
//!
//! A `let`, `do`, standalone expression or `extern` written directly in a
//! namespace is FS0201 ("Namespaces cannot contain values"), and FCS drops the
//! declaration whole (`CheckLetOrDoInNamespace`; `extern` and a standalone
//! expression reach it as a `let`): its binders join no environment, its
//! attributes are not resolved, and its right-hand side is never checked. So a
//! later use of the name binds whatever else is in scope under it, or nothing,
//! and a use inside the dropped declaration has no answer at all.
//!
//! The axes:
//!
//! - the dropped declaration of `X` ([`Subject`]): a value, `let rec`,
//!   `let mutable`, `[<Literal>]`, function, tuple head, active pattern,
//!   `extern`, `do`, and a standalone expression (the last two bind nothing:
//!   they are the controls whose later uses FCS binds as if they were absent);
//! - what else claims `X` before it ([`Rival`]): nothing, an `open` of a
//!   module with a literal `X`, or an `[<AutoOpen>]` module of the block with
//!   one;
//! - the block's header ([`Header`]): `namespace R.N`, `namespace rec R.N`, or
//!   `namespace global`;
//! - where `X` is used: inside the dropped declaration (its right-hand side
//!   and its parameters), in a module later in the block, in a later file's
//!   block of the same namespace, and in a later file through `open R.N` —
//!   each bare in an expression, bare in a pattern, and dotted (`R.N.X`).
//!
//! The oracle is [`crate::common::use_grade`]'s, which reads FCS's answer per
//! site from the erroring project (an error at the site rejects it, so the
//! dropped declaration's own FS0201 implicates no use): certain-implies-exact,
//! with [`must_commit`] keeping the grid from passing by declining everything.

use crate::common::use_grade::{
    FcsVerdict, FileBuilder, Graded, OurVerdict, Site, assert_sound_and_live, grade,
};

/// The declaration FCS drops, of the name `X`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subject {
    /// `let X = K`.
    Value,
    /// `let rec X () = K`.
    Rec,
    /// `let mutable X = K`.
    Mutable,
    /// `[<Literal>] let X = K`, which a pattern would see.
    Literal,
    /// `let X (v: int) = v + K`.
    Function,
    /// `let X, W = K, 0`.
    Tuple,
    /// `let (|X|_|) (v: int) = …`, a pattern constructor.
    Active,
    /// `[<DllImport("c")>] extern int X(int v)`.
    Extern,
    /// `do ignore K`: binds nothing.
    Do,
    /// `ignore K`: binds nothing.
    Expr,
}

impl Subject {
    const ALL: [Subject; 10] = [
        Subject::Value,
        Subject::Rec,
        Subject::Mutable,
        Subject::Literal,
        Subject::Function,
        Subject::Tuple,
        Subject::Active,
        Subject::Extern,
        Subject::Do,
        Subject::Expr,
    ];

    fn tag(self) -> &'static str {
        match self {
            Subject::Value => "value",
            Subject::Rec => "rec",
            Subject::Mutable => "mutable",
            Subject::Literal => "literal",
            Subject::Function => "function",
            Subject::Tuple => "tuple",
            Subject::Active => "active",
            Subject::Extern => "extern",
            Subject::Do => "do",
            Subject::Expr => "expr",
        }
    }

    /// Append the declaration of `name` (a tuple's second binder is `other`),
    /// probing the uses inside it, and record its binder of `name` in
    /// `binders`.
    fn declare(
        self,
        f: &mut FileBuilder,
        sites: &mut Vec<Site>,
        binders: &mut Vec<Site>,
        name: &str,
        other: &str,
        label: &str,
    ) {
        let rhs = format!("{label}/inside/rhs");
        let param = format!("{label}/inside/param");
        // The binder is the last occurrence of `name` in its line; each use,
        // the last occurrence of its spelling (`v` in the body, not the
        // parameter).
        let mut decl = |f: &mut FileBuilder, line: String, uses: &[(&str, &String)]| {
            binders.push(f.site_in_next(&line, name, label.to_string()));
            for (spelling, use_label) in uses {
                sites.push(f.site_in_next(&line, spelling, (*use_label).clone()));
            }
            f.line(&line);
        };
        match self {
            Subject::Value => decl(f, format!("let {name} = K"), &[("K", &rhs)]),
            Subject::Rec => decl(f, format!("let rec {name} () = K"), &[("K", &rhs)]),
            Subject::Mutable => decl(f, format!("let mutable {name} = K"), &[("K", &rhs)]),
            Subject::Literal => {
                f.line("[<Literal>]");
                decl(f, format!("let {name} = K"), &[("K", &rhs)]);
            }
            Subject::Function => decl(
                f,
                format!("let {name} (v: int) = v + K"),
                &[("v", &param), ("K", &rhs)],
            ),
            Subject::Tuple => decl(f, format!("let {name}, {other} = K, 0"), &[("K", &rhs)]),
            Subject::Active => decl(
                f,
                format!("let (|{name}|_|) (v: int) = if v = K then Some () else None"),
                &[("v", &param), ("K", &rhs)],
            ),
            Subject::Extern => {
                f.line("[<System.Runtime.InteropServices.DllImport(\"c\")>]");
                decl(f, format!("extern int {name}(int v)"), &[]);
            }
            Subject::Do => f.probe("do ignore K", "K", rhs, sites),
            Subject::Expr => f.probe("ignore K", "K", rhs, sites),
        }
    }
}

/// What claims `X` before the dropped declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rival {
    None,
    /// `open Fb.F{i}`, a module of an earlier file declaring a literal `X`,
    /// first in every block of the cell.
    Open,
    /// `[<AutoOpen>] module A{i}` declaring a literal `X`, in the block before
    /// the dropped declaration.
    AutoOpen,
}

impl Rival {
    const ALL: [Rival; 3] = [Rival::None, Rival::Open, Rival::AutoOpen];

    fn tag(self) -> &'static str {
        match self {
            Rival::None => "none",
            Rival::Open => "open",
            Rival::AutoOpen => "autoOpen",
        }
    }
}

/// The header of the block holding the dropped declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Header {
    /// `namespace R{i}.N`.
    Plain,
    /// `namespace rec R{i}.N`.
    Rec,
    /// `namespace global`, where every cell shares one namespace, so each
    /// names its subject `X{i}`.
    Global,
}

impl Header {
    const ALL: [Header; 3] = [Header::Plain, Header::Rec, Header::Global];

    fn tag(self) -> &'static str {
        match self {
            Header::Plain => "plain",
            Header::Rec => "rec",
            Header::Global => "global",
        }
    }
}

/// The literal rival's declaration of `name`, at indentation `ind`.
fn rival_lines(name: &str, ind: &str) -> Vec<String> {
    vec![format!("{ind}[<Literal>]"), format!("{ind}let {name} = 0")]
}

/// The probes of `name` at indentation `ind`: bare in an expression, bare in a
/// pattern, and — where the namespace has a name, `ns` — dotted.
fn probes(
    f: &mut FileBuilder,
    sites: &mut Vec<Site>,
    name: &str,
    ns: Option<&str>,
    ind: &str,
    label: &str,
) {
    f.probe(
        &format!("{ind}let e{name} = {name}"),
        name,
        format!("{label}/expr"),
        sites,
    );
    f.probe(
        &format!("{ind}let p{name} = match 0 with {name} -> 0 | _ -> 1"),
        name,
        format!("{label}/pat"),
        sites,
    );
    if let Some(ns) = ns {
        let dotted = format!("{ns}.{name}");
        f.probe(
            &format!("{ind}let d{name} = {dotted}"),
            &dotted,
            format!("{label}/dotted"),
            sites,
        );
    }
}

/// One cell: the dropped declaration's block in `blocks`, then a block of the
/// same namespace and a block opening it in `later`. The open rival's module
/// goes in `defs`.
#[allow(clippy::too_many_arguments)]
fn cell(
    i: usize,
    subject: Subject,
    rival: Rival,
    header: Header,
    defs: &mut FileBuilder,
    blocks: &mut FileBuilder,
    later: &mut FileBuilder,
    sites: &mut Vec<Site>,
    binders: &mut Vec<Site>,
) {
    let label = format!("{}/{}/{}", subject.tag(), rival.tag(), header.tag());
    let (name, ns) = match header {
        Header::Global => (format!("X{i}"), None),
        Header::Plain | Header::Rec => ("X".to_string(), Some(format!("R{i}.N"))),
    };
    if rival == Rival::Open {
        defs.line(&format!("module F{i} ="));
        for l in rival_lines(&name, "    ") {
            defs.line(&l);
        }
    }
    let open_rival = |f: &mut FileBuilder| {
        if rival == Rival::Open {
            f.line(&format!("open Fb.F{i}"));
        }
    };

    match (header, &ns) {
        (Header::Plain, Some(ns)) => blocks.line(&format!("namespace {ns}")),
        (Header::Rec, Some(ns)) => blocks.line(&format!("namespace rec {ns}")),
        _ => blocks.line("namespace global"),
    }
    blocks.line("open Fb.Kb");
    open_rival(blocks);
    if rival == Rival::AutoOpen {
        blocks.line("[<AutoOpen>]");
        blocks.line(&format!("module A{i} ="));
        for l in rival_lines(&name, "    ") {
            blocks.line(&l);
        }
    }
    subject.declare(blocks, sites, binders, &name, &format!("W{i}"), &label);
    blocks.line(&format!("module M{i} ="));
    probes(
        blocks,
        sites,
        &name,
        ns.as_deref(),
        "    ",
        &format!("{label}/block"),
    );

    // A later file: a block of the same namespace, then one opening it.
    match &ns {
        Some(ns) => later.line(&format!("namespace {ns}")),
        None => later.line("namespace global"),
    }
    open_rival(later);
    later.line(&format!("module L{i} ="));
    probes(
        later,
        sites,
        &name,
        ns.as_deref(),
        "    ",
        &format!("{label}/laterNs"),
    );
    if let Some(ns) = &ns {
        later.line(&format!("namespace Use{i}"));
        later.line("module Q =");
        let mut q = |line: String| later.line(&format!("    {line}"));
        if rival == Rival::Open {
            q(format!("open Fb.F{i}"));
        }
        q(format!("open {ns}"));
        probes(
            later,
            sites,
            &name,
            Some(ns),
            "    ",
            &format!("{label}/laterOpen"),
        );
    }
}

/// The sites that must commit FCS's answer, when FCS binds them cleanly (to
/// a declaration, or as a fresh pattern binder, which a dropped literal's or
/// active pattern's name becomes): every use of `X` outside the dropped declaration whose twin in the `do`
/// control's cell — the same rival, header, position and probe, after a
/// declaration binding nothing — commits. FCS reads a dropped declaration as
/// absent, so it may cost no coverage: what the walk answers past a `do`, it
/// answers past a dropped `let`.
fn must_commit(graded: &[Graded]) -> Vec<&str> {
    graded
        .iter()
        .filter(|g| {
            matches!(
                g.fcs,
                FcsVerdict::Resolved(..) | FcsVerdict::FreshBinder(..)
            ) && !g.label.contains("/inside/")
        })
        .filter(|g| {
            let (_, rest) = g.label.split_once('/').expect("a labelled cell");
            let control = format!("{}/{rest}", Subject::Do.tag());
            graded
                .iter()
                .any(|c| c.label == control && matches!(c.ours, OurVerdict::Committed(..)))
        })
        .map(|g| g.label.as_str())
        .collect()
}

#[test]
fn values_directly_in_a_namespace_agree_with_fcs() {
    let mut defs = FileBuilder::new(0);
    let mut blocks = FileBuilder::new(1);
    let mut later = FileBuilder::new(2);
    let mut sites = Vec::new();
    let mut binders = Vec::new();
    defs.line("namespace Fb");
    defs.line("module Kb =");
    for l in rival_lines("K", "    ") {
        defs.line(&l);
    }
    let mut i = 0;
    for subject in Subject::ALL {
        for rival in Rival::ALL {
            for header in Header::ALL {
                cell(
                    i,
                    subject,
                    rival,
                    header,
                    &mut defs,
                    &mut blocks,
                    &mut later,
                    &mut sites,
                    &mut binders,
                );
                i += 1;
            }
        }
    }
    let files = [
        ("defs.fs", defs.text),
        ("blocks.fs", blocks.text),
        ("later.fs", later.text),
    ];
    let graded = grade("nsv", &files, &sites);

    // The premise, per cell: FCS answers nothing inside a dropped
    // declaration, and binds no use to its binder.
    assert_eq!(
        binders.len(),
        8 * Rival::ALL.len() * Header::ALL.len(),
        "every binding subject records its binder"
    );
    let premise: Vec<String> = graded
        .iter()
        .filter(|g| match &g.fcs {
            FcsVerdict::Resolved(file, range) | FcsVerdict::FreshBinder(file, range) => {
                g.label.contains("/inside/")
                    || binders.iter().any(|b| b.file == *file && b.range == *range)
            }
            FcsVerdict::Inaccessible | FcsVerdict::Rejected => false,
        })
        .map(|g| format!("  [{}] FCS {:?}", g.label, g.fcs))
        .collect();
    assert!(
        premise.is_empty(),
        "FCS binds a use inside, or to, a declaration it drops:\n{}",
        premise.join("\n")
    );
    // Non-vacuity: the controls commit, so the grid grades live bindings.
    let must = must_commit(&graded);
    eprintln!("namespace value must-commit sites: {}", must.len());
    assert!(
        must.len() >= 100,
        "only {} must-commit sites: the controls have stopped committing",
        must.len()
    );
    assert_sound_and_live("namespace value", &graded, &[], &must);
}

/// Inference types nothing in a declaration FCS drops — FCS checks none of
/// it, so any type there is an answer FCS never gives — while the module
/// after it is typed as usual.
#[test]
fn inference_types_nothing_in_a_dropped_declaration() {
    use borzoi_cst::parser::parse;
    use borzoi_cst::syntax::{AstNode, ImplFile, ModuleDecl};
    use borzoi_sema::{PrecedingTypes, ProjectItems, SyntaxRecovery, infer_file, resolve_file};

    let src = "namespace N\n\
               let x = 1 + 2\n\
               let s = \"s\"\n\
               let t = (1, \"s\")\n\
               let g () = 3\n\
               let rec f (v: int) = v + 1\n\
               [<Literal>]\n\
               let L = 3\n\
               do ignore 4\n\
               ignore 5\n\
               module M =\n    \
                   let y = 6 + 7\n";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = crate::common::full_bcl_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(&file, &resolved, env, &PrecedingTypes::default());
    let block = file.modules().next().expect("the namespace block");
    let dropped: Vec<rowan::TextRange> = block
        .decls()
        .filter(|d| !matches!(d, ModuleDecl::NestedModule(_)))
        .map(|d| d.syntax().text_range())
        .collect();
    assert_eq!(dropped.len(), 8, "eight dropped declarations");
    let inside: Vec<(rowan::TextRange, String)> = inferred
        .types()
        .iter()
        .filter(|(r, _)| dropped.iter().any(|d| d.contains_range(**r)))
        .map(|(r, ty)| (*r, format!("{ty:?}")))
        .collect();
    assert!(
        inside.is_empty(),
        "inference types a declaration FCS drops: {inside:?}"
    );
    let module_start = u32::try_from(src.find("module M").unwrap()).unwrap();
    assert!(
        inferred
            .types()
            .keys()
            .any(|r| u32::from(r.start()) > module_start),
        "the module after the dropped declarations is typed"
    );
}
