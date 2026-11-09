//! What a type's members see, against FCS, for the places a member can be
//! declared: the type's own definition, an intrinsic augmentation (`type T
//! with` in the module that declares `T`), and an optional extension (any
//! other module, or any interface).
//!
//! The facts the grid checks:
//!
//! * a type's own union cases sit above the module scope in its own members
//!   and its intrinsic augmentations' members, `[<RequireQualifiedAccess>]` or
//!   not, so they shadow a same-named module value even one declared later;
//!   an optional extension sees the module value;
//! * a class's constructor parameters and `let` fields are visible only in
//!   its own definition; an augmentation of either kind sees the module value;
//! * an intrinsic augmentation's `'a` is the augmented type's own parameter,
//!   by the type's name for it even when the header renames it; an optional
//!   extension's, an interface's included, is its header's;
//! * an `open` between the type and `type T<'a> with` that brings another
//!   `T` makes the augmentation an optional extension of that one, and makes
//!   a bare `T<int>` that one too;
//! * in a `rec` module an augmentation may precede its type, and is still
//!   intrinsic, and its members see the module's later values;
//! * an intrinsic augmentation's own `static let` shadows the module value;
//! * a `[<RequireQualifiedAccess>]` `type T = T` case shadows no module value,
//!   except in the type's own members;
//! * a class's name is its constructor in its augmentations, ahead of a
//!   module value of the name;
//! * a secondary constructor's `as self` binds its own self identifier.
//!
//! Each program declares one type, module values named like its cases or its
//! constructor parameter and field, and a member that uses every one of those
//! names and the member's own parameter. Every use FCS declares in the file
//! must be committed to FCS's declaration where the resolver walks the member
//! with a proven scope, and must never be committed elsewhere where it does
//! not ([`SUBSET_ROWS`], [`Host::must_commit`]).

use std::path::PathBuf;

use crate::common::{
    CensusDecl, census_oracle_binders, census_resolve_uses, invoke_fcs_dump_census,
    parse_census_jsonl, temp_fs_file, unmodelled_binder_failures,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::test_support::binder_completeness::{BinderVerdict, binder_verdict};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, resolve_file};
use rowan::TextRange;

/// A type kind: its declaration, the module values shadowing the names its
/// members may see from it, and the member using them.
struct Kind {
    label: &'static str,
    decl: &'static str,
    /// The declaration in the `Host::Own` programs, where it differs
    /// (`type T = T with`, which takes members only so).
    own_decl: Option<&'static str>,
    /// What follows the member in the `Host::Own` programs (a later `and`).
    own_after: &'static str,
    values: &'static str,
    /// The augmentation head, and a renaming of its parameters if generic.
    header: &'static str,
    renamed: Option<&'static str>,
    member: &'static str,
    body: &'static str,
    /// A type-position use of the type, after an `open` that could redirect it.
    annotation: &'static str,
    /// Whether the type can carry its own members (`Host::Own`).
    own_members: bool,
    /// Whether an intrinsic augmentation of it may declare `static let`.
    static_let: bool,
    /// Whether an optional extension may declare the member (not a
    /// constructor).
    extension_member: bool,
    /// Whether a value-position `T` in an augmentation's member body must be
    /// committed (a non-qualified `type T = T` case is deliberately declined,
    /// as is the case its own members see).
    body_t_commits: bool,
}

const GENERIC_MEMBER: &str = "member this.M (p: 'a) : int =";

const KINDS: &[Kind] = &[
    Kind {
        label: "rqa-union",
        own_decl: None,
        own_after: "",
        decl: "[<RequireQualifiedAccess>]\ntype T<'a> =\n    | A\n    | B of 'a\n",
        values: "let A = 1\nlet B = 2\n",
        header: "T<'a>",
        renamed: Some("T<'b>"),
        member: GENERIC_MEMBER,
        body: "(ignore A; ignore B; ignore p; 0)",
        annotation: "T<int>",
        own_members: true,
        static_let: false,
        extension_member: true,
        body_t_commits: false,
    },
    Kind {
        label: "union",
        own_decl: None,
        own_after: "",
        decl: "type T<'a> =\n    | A\n    | B of 'a\n",
        values: "let A = 1\nlet B = 2\n",
        header: "T<'a>",
        renamed: Some("T<'b>"),
        member: GENERIC_MEMBER,
        body: "(ignore A; ignore B; ignore p; 0)",
        annotation: "T<int>",
        own_members: true,
        static_let: false,
        extension_member: true,
        body_t_commits: false,
    },
    Kind {
        label: "class",
        own_decl: None,
        own_after: "",
        decl: "type T<'a>(a: 'a) =\n    let b = 3\n    member _.X = (a, b)\n",
        values: "let a = 1\nlet b = 2\n",
        header: "T<'a>",
        renamed: Some("T<'b>"),
        member: GENERIC_MEMBER,
        body: "(ignore a; ignore b; ignore p; 0)",
        annotation: "T<int>",
        own_members: true,
        static_let: false,
        extension_member: true,
        body_t_commits: false,
    },
    Kind {
        label: "plain-class",
        own_decl: None,
        own_after: "",
        decl: "type T(a: int) =\n    let b = 3\n    member _.X = (a, b)\n",
        values: "let a = 1\nlet b = 2\n",
        header: "T",
        renamed: None,
        member: "member this.M (p: int) : int =",
        body: "(ignore a; ignore b; ignore p; 0)",
        annotation: "T",
        own_members: true,
        static_let: true,
        extension_member: true,
        body_t_commits: false,
    },
    Kind {
        label: "interface",
        own_decl: None,
        own_after: "",
        decl: "type T<'a> =\n    abstract X: 'a\n",
        values: "",
        header: "T<'a>",
        renamed: Some("T<'b>"),
        member: GENERIC_MEMBER,
        body: "(ignore p; 0)",
        annotation: "T<int>",
        own_members: false,
        static_let: false,
        extension_member: true,
        body_t_commits: false,
    },
    Kind {
        // FCS reads `type T = T` as a union with the case `T`.
        label: "abbreviation-union",
        own_decl: Some("type T = T with\n"),
        own_after: "",
        decl: "type T = T\n",
        values: "let T = 1\n",
        header: "T",
        renamed: None,
        member: "member this.M (p: int) : int =",
        body: "(ignore T; ignore p; 0)",
        annotation: "T",
        own_members: true,
        static_let: false,
        extension_member: true,
        body_t_commits: false,
    },
    Kind {
        // A `[<RequireQualifiedAccess>]` case shadows no module value.
        label: "rqa-abbreviation-union",
        own_decl: Some("[<RequireQualifiedAccess>]\ntype T = T with\n"),
        own_after: "",
        decl: "[<RequireQualifiedAccess>]\ntype T = T\n",
        values: "let T = 1\n",
        header: "T",
        renamed: None,
        member: "member this.M (p: int) : int =",
        body: "(ignore T; ignore p; 0)",
        annotation: "T",
        own_members: true,
        static_let: false,
        extension_member: true,
        body_t_commits: true,
    },
    Kind {
        // `type T = T`'s case belongs to it, not to a same-named type of
        // another arity later in its group.
        label: "rqa-abbreviation-union-and-generic",
        own_decl: Some("[<RequireQualifiedAccess>]\ntype T = T with\n"),
        own_after: "and T<'a> =\n    | B of 'a\n",
        decl: "[<RequireQualifiedAccess>]\ntype T = T\nand T<'a> =\n    | B of 'a\n",
        values: "let T = 1\n",
        header: "T",
        renamed: None,
        member: "member this.M (p: int) : int =",
        body: "(ignore T; ignore p; 0)",
        annotation: "T",
        own_members: true,
        static_let: false,
        extension_member: true,
        body_t_commits: false,
    },
    Kind {
        // A non-generic union: inside the proven subset.
        label: "plain-union",
        own_decl: None,
        own_after: "",
        decl: "type T =\n    | A\n    | B of int\n",
        values: "let A = 1\nlet B = 2\n",
        header: "T",
        renamed: None,
        member: "member this.M (p: int) : int =",
        body: "(ignore A; ignore B; ignore p; 0)",
        annotation: "T",
        own_members: true,
        static_let: false,
        extension_member: true,
        body_t_commits: false,
    },
    Kind {
        // A non-generic record: inside the proven subset, and its field
        // labels are no bare names. Its own members would see only earlier
        // values, which the grid also writes after the type.
        label: "plain-record",
        own_decl: None,
        own_after: "",
        decl: "type T =\n    { a: int; b: int }\n",
        values: "let a = 1\nlet b = 2\n",
        header: "T",
        renamed: None,
        member: "member this.M (p: int) : int =",
        body: "(ignore a; ignore b; ignore p; 0)",
        annotation: "T",
        own_members: false,
        static_let: false,
        extension_member: true,
        body_t_commits: false,
    },
    Kind {
        // A class's own name is its constructor in its intrinsic
        // augmentation, ahead of a module value of the name.
        label: "constructor-class",
        own_decl: None,
        own_after: "",
        decl: "type T() =\n    member _.X = 1\n",
        values: "let T () = 42\n",
        header: "T",
        renamed: None,
        member: "static member Create () =",
        body: "T ()",
        annotation: "T",
        own_members: true,
        static_let: false,
        extension_member: true,
        body_t_commits: false,
    },
    Kind {
        // A secondary constructor's `as self` binds its own self identifier.
        label: "constructor-as-self",
        own_decl: None,
        own_after: "",
        decl: "type T() =\n    member _.X = 1\n",
        values: "let self = 42\nlet x = 1\n",
        header: "T",
        renamed: None,
        member: "new (x: int) as self =",
        body: "T () then (ignore self; ignore x)",
        annotation: "T",
        own_members: true,
        static_let: false,
        extension_member: false,
        body_t_commits: false,
    },
];

/// The intrinsic-augmentation rows inside the subset whose scope sema proves
/// (`Resolver::proven_augmentation_target`), by kind label: every graded use in
/// them must commit, and the member's parameter and self identifier must be
/// binders of the walk's own, which binder completeness then grades. Every
/// other augmentation row is left unwalked, and every use in its members must
/// decline. Listed by name, so a change that narrows the subset has to name
/// each row it gives up rather than lower a count.
const SUBSET_ROWS: &[&str] = &[
    "plain-class",
    "plain-union",
    "plain-record",
    "constructor-class",
    "constructor-as-self",
];

/// Where the member is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Host {
    /// In the type's own definition (appended to its declaration).
    Own,
    /// `type T<'a> with` in the same module.
    Intrinsic,
    /// `type T<'a> with` in a nested module.
    Optional,
    /// `type T<'b> with` in the same module: the header renames `'a`.
    IntrinsicRenamed,
    /// `type T<'a> with` in the same module after `open O`, where `O`
    /// declares a `T` of the same shape, and a `T<int>` annotation.
    AfterOpen,
    /// `module rec`, with the augmentation before its type.
    RecForward,
    /// `type T with` in the same module, declaring `static let a` first.
    StaticLet,
    /// A nested `module rec` after outer values, redeclaring them after the
    /// augmentation: its members see the later ones.
    RecLaterValues,
}

impl Host {
    const ALL: [Host; 8] = [
        Host::Own,
        Host::Intrinsic,
        Host::Optional,
        Host::IntrinsicRenamed,
        Host::AfterOpen,
        Host::RecForward,
        Host::StaticLet,
        Host::RecLaterValues,
    ];

    /// Whether every graded use must be committed, not merely never wrong.
    fn must_commit(self, kind: &Kind) -> bool {
        match self {
            Host::Own => true,
            // An extension is outside the subset whose scope sema proves.
            Host::Optional => false,
            Host::Intrinsic | Host::IntrinsicRenamed => SUBSET_ROWS.contains(&kind.label),
            Host::AfterOpen | Host::RecForward | Host::StaticLet | Host::RecLaterValues => false,
        }
    }
}

/// Where the shadowing module values sit.
#[derive(Clone, Copy)]
enum Values {
    BeforeType,
    AfterType,
}

fn augmentation(header: &str, kind: &Kind, indent: &str) -> String {
    format!(
        "{indent}type {header} with\n{indent}    {}\n{indent}        {}\n",
        kind.member, kind.body
    )
}

fn program(kind: &Kind, host: Host, values: Values) -> Option<String> {
    let header = match host {
        Host::IntrinsicRenamed => kind.renamed?,
        _ => kind.header,
    };
    if matches!(host, Host::Own) && !kind.own_members
        || matches!(host, Host::StaticLet) && !kind.static_let
        || matches!(host, Host::Optional | Host::AfterOpen) && !kind.extension_member
    {
        return None;
    }
    if let Host::RecLaterValues = host {
        let indent =
            |text: &str| -> String { text.lines().map(|l| format!("    {l}\n")).collect() };
        return Some(format!(
            "module Prog\n\n{}module rec N =\n{}{}{}",
            kind.values,
            indent(kind.decl),
            augmentation(header, kind, "    "),
            indent(kind.values)
        ));
    }
    if let Host::RecForward = host {
        return Some(format!(
            "module rec Prog\n\n{}{}{}",
            kind.values,
            augmentation(header, kind, ""),
            kind.decl
        ));
    }
    let mut s = String::from("module Prog\n\n");
    if let Values::BeforeType = values {
        s += kind.values;
    }
    s += match host {
        Host::Own => kind.own_decl.unwrap_or(kind.decl),
        _ => kind.decl,
    };
    if let Host::Own = host {
        s += &format!("    {}\n        {}\n", kind.member, kind.body);
        s += kind.own_after;
    }
    if let Values::AfterType = values {
        s += kind.values;
    }
    match host {
        Host::Own | Host::RecForward | Host::RecLaterValues => {}
        Host::Intrinsic | Host::IntrinsicRenamed => s += &augmentation(header, kind, ""),
        Host::StaticLet => {
            s += &format!(
                "type {header} with\n    static let a = 5\n    {}\n        {}\n",
                kind.member, kind.body
            );
        }
        Host::Optional => {
            s += "module N =\n";
            s += &augmentation(header, kind, "    ");
        }
        Host::AfterOpen => {
            s += "module O =\n";
            for line in kind.decl.lines() {
                s += &format!("    {line}\n");
            }
            s += &format!("open O\nlet z (q: {}) = 0\n", kind.annotation);
            s += &augmentation(header, kind, "");
        }
    }
    Some(s)
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

#[test]
fn member_scopes_agree_with_fcs() {
    let mut programs = Vec::new();
    for kind in KINDS {
        for host in Host::ALL {
            for values in [Values::BeforeType, Values::AfterType] {
                if let Some(src) = program(kind, host, values) {
                    programs.push((kind, host, src));
                }
            }
        }
    }
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|(_, _, src)| temp_fs_file("augmentation_scope", src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), programs.len(), "one census line per program");

    let mut rejected = Vec::new();
    let mut failures = Vec::new();
    let mut graded = 0usize;
    let mut committed = 0usize;
    let mut must_graded = 0usize;
    // Must-commit uses graded per subset row, so each row is shown to be
    // exercised, not only the subset as a whole.
    let mut subset_graded: std::collections::BTreeMap<&str, usize> =
        SUBSET_ROWS.iter().map(|row| (*row, 0)).collect();
    // Member binders interned per subset row.
    let mut subset_interned: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    // Uses graded inside unwalked augmentations' members.
    let mut unwalked_graded = 0usize;
    for ((kind, host, src), file) in programs.iter().zip(&census) {
        let label = kind.label;
        if !file.ok || file.has_check_errors {
            rejected.push(format!("{label} {host:?}:\n{src}"));
            continue;
        }
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{src}\n{:?}", parsed.errors);
        let recovery = SyntaxRecovery::of(&parsed);
        let rf = resolve_file(
            &ImplFile::cast(parsed.root).unwrap(),
            &ProjectItems::default(),
            &AssemblyEnv::default(),
            &recovery,
        );
        // The member's own line and body are graded, and the annotation after
        // an `open`.
        let member_at = src.find(kind.member).expect("the member");
        // The member's line through its body, which is one line.
        let member_end =
            member_at + src[member_at..].find(kind.body).expect("the body") + kind.body.len();
        let in_subset = *host == Host::Intrinsic && SUBSET_ROWS.contains(&label);
        // Binder completeness over the whole program, and inside a walked
        // augmentation the member's own binders are the walk's.
        let binders = census_oracle_binders(file, src);
        failures.extend(unmodelled_binder_failures(
            &format!("{label} {host:?}"),
            src,
            &rf,
            &binders,
        ));
        if in_subset {
            for b in binders.iter().filter(|b| {
                // Declared in the member's head: its self identifier and
                // parameter.
                (member_at..member_at + kind.member.len()).contains(&usize::from(b.range.start()))
                    && ["p", "x", "this", "self"].contains(&&*b.name)
            }) {
                if binder_verdict(&rf, b) == BinderVerdict::Interned {
                    *subset_interned.entry(label).or_default() += 1;
                } else {
                    failures.push(format!(
                        "{label} {host:?}: the member's binder `{}` at {:?} is not the walk's own\n{src}",
                        b.name, b.range
                    ));
                }
            }
        }
        let annotation_at = src.find("let z").unwrap_or(member_at);
        for u in census_resolve_uses(file, src) {
            if u.is_from_definition || u.start == u.end || u.start < member_at.min(annotation_at) {
                continue;
            }
            let text = &src[u.start..u.end];
            if !["A", "B", "a", "b", "p", "'a", "T", "self", "x"].contains(&text) {
                continue;
            }
            let at = span(u.start, u.end);
            let res = rf.resolution_at(at);
            let ours = res
                .filter(|r| matches!(r, Resolution::Local(_) | Resolution::Item(_)))
                .and_then(|r| rf.resolved_def(r))
                .map(|d| d.range);
            graded += 1;
            if host.must_commit(kind) {
                must_graded += 1;
                if *host == Host::Intrinsic
                    && let Some(n) = subset_graded.get_mut(kind.label)
                {
                    *n += 1;
                }
            }
            // Outside the subset an augmentation's member bodies are unwalked:
            // every value-position use there declines. A type parameter or a
            // type name in the member's signature is the type resolver's,
            // which reads the header's frame whatever the subset says.
            let in_augmentation = *host != Host::Own && (member_at..member_end).contains(&u.start);
            if in_augmentation && !in_subset && !["'a", "T"].contains(&text) {
                unwalked_graded += 1;
                if res.is_some() {
                    failures.push(format!(
                        "{label} {host:?}: `{text}` at {at:?} in an augmentation outside the \
                         subset recorded {res:?}\n{src}"
                    ));
                }
                continue;
            }
            match (u.decl, ours) {
                (CensusDecl::InFile(s, e), Some(def)) if def == span(s, e) => committed += 1,
                (_, None)
                    if !host.must_commit(kind)
                        || text == "T"
                            && (u.start < member_at
                                || !kind.body_t_commits
                                // Its own members see the case.
                                || matches!(host, Host::Own)) => {}
                (decl, _) => failures.push(format!(
                    "{label} {host:?}: `{text}` at {at:?}: we gave {res:?} ({ours:?}), FCS {decl:?}\n{src}"
                )),
            }
        }
    }
    eprintln!(
        "augmentation-scope grid: {} programs, {graded} uses graded, {committed} committed",
        programs.len()
    );
    assert!(
        rejected.is_empty(),
        "FCS rejected a program, so its answers are recovery:\n{}",
        rejected.join("\n")
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let unexercised: Vec<&str> = subset_graded
        .iter()
        // `constructor-class`'s member, `static member Create ()`, declares
        // no binder to intern.
        .filter(|(row, n)| {
            **n == 0 || (**row != "constructor-class" && !subset_interned.contains_key(*row))
        })
        .map(|(row, _)| *row)
        .collect();
    assert!(
        unexercised.is_empty(),
        "subset rows with no must-commit use graded or no member binder interned: {unexercised:?}"
    );
    assert!(
        graded >= programs.len() * 2
            && must_graded > 0
            && committed >= must_graded
            && unwalked_graded > 0,
        "the grid is vacuous: {committed} of {graded} uses committed, {must_graded} must commit"
    );
}

/// How an augmentation's header spells its type's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spelling {
    /// `T`.
    Plain,
    /// ``` ``T`` ```.
    Backticked,
    /// The compiled name ``` ``T`N`` ```, for N = 0, 1 and 2.
    Mangled(usize),
    /// A backticked name with a space, ``` ``T x`` ```.
    Spaced,
    /// `Prog.T`, or `O.T` for a target of another module.
    Qualified,
    /// ``` ``T`1`` ``` where the container also declares a type literally
    /// named so.
    MangledWithLiteral,
}

impl Spelling {
    const ALL: [Spelling; 8] = [
        Spelling::Plain,
        Spelling::Backticked,
        Spelling::Mangled(0),
        Spelling::Mangled(1),
        Spelling::Mangled(2),
        Spelling::Spaced,
        Spelling::Qualified,
        Spelling::MangledWithLiteral,
    ];
}

/// What the module holds under the header's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// `[<RequireQualifiedAccess>] type T<'a> = A | B of 'a`.
    GenericLocal,
    /// A class `type T()`.
    NonGenericLocal,
    /// `[<RequireQualifiedAccess>] type T = A | B of int`.
    Union,
    /// `type T = { F: int }`.
    Record,
    /// `module T`, and no type of the name.
    ModuleOnly,
    /// Nothing of the name.
    Absent,
    /// The union, in another module the file opens.
    Opened,
}

impl Target {
    const ALL: [Target; 7] = [
        Target::GenericLocal,
        Target::NonGenericLocal,
        Target::Union,
        Target::Record,
        Target::ModuleOnly,
        Target::Absent,
        Target::Opened,
    ];

    /// The declaration, the header's type parameters, and the module a
    /// qualified header names.
    fn decl(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Target::GenericLocal => (
                "[<RequireQualifiedAccess>]\ntype T<'a> =\n    | A\n    | B of 'a\n",
                "<'a>",
                "Prog",
            ),
            Target::NonGenericLocal => ("type T() =\n    member _.X = 1\n", "", "Prog"),
            Target::Union => (
                "[<RequireQualifiedAccess>]\ntype T =\n    | A\n    | B of int\n",
                "",
                "Prog",
            ),
            Target::Record => ("type T = { F: int }\n", "", "Prog"),
            Target::ModuleOnly => ("module T =\n    let x = 1\n", "", "Prog"),
            Target::Absent => ("", "", "Prog"),
            Target::Opened => (
                "module O =\n    [<RequireQualifiedAccess>]\n    type T =\n        | A\n        | B of int\nopen O\n",
                "",
                "O",
            ),
        }
    }
}

/// Every header spelling against every kind of target, with the
/// augmentation beside the declaration (where an intrinsic one sits) and in a
/// nested module (where an extension sits). FCS's verdict on the member's
/// `A`, and whether it checks the program cleanly, are pinned cell by cell in a
/// manifest; ours must never commit where FCS binds otherwise.
#[test]
fn header_spellings_agree_with_fcs() {
    use crate::common::corpus_manifest::{check_manifest, regenerate};
    use borzoi_oracle_harness::manifest::Manifest;

    let mut cells = Vec::new();
    for target in Target::ALL {
        for spelling in Spelling::ALL {
            for nested in [false, true] {
                let (decl, typars, qualifier) = target.decl();
                let name = match spelling {
                    Spelling::Plain => "T".to_string(),
                    Spelling::Backticked => "``T``".to_string(),
                    Spelling::Mangled(n) => format!("``T`{n}``"),
                    Spelling::Spaced => "``T x``".to_string(),
                    Spelling::Qualified => format!("{qualifier}.T"),
                    Spelling::MangledWithLiteral => "``T`1``".to_string(),
                };
                let literal = if spelling == Spelling::MangledWithLiteral {
                    "type ``T`1`` =\n    | C\n"
                } else {
                    ""
                };
                let indent = if nested { "    " } else { "" };
                let host = if nested { "module N =\n" } else { "" };
                let src = format!(
                    "module Prog\n\nlet A = 10\n{decl}{literal}{host}{indent}type {name}{typars} with\n{indent}    member _.M = A\n"
                );
                let place = if nested { "nested" } else { "beside" };
                cells.push((format!("{target:?} / {spelling:?} / {place}"), src));
            }
        }
    }
    let paths: Vec<PathBuf> = cells
        .iter()
        .map(|(_, src)| temp_fs_file("augmentation_header", src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), cells.len(), "one census line per program");

    let mut failures = Vec::new();
    let mut entries = Vec::new();
    for ((label, src), file) in cells.iter().zip(&census) {
        let parsed = parse(src);
        let recovery = SyntaxRecovery::of(&parsed);
        // FCS reads FSharp.Core, and `[<RequireQualifiedAccess>]` is certain
        // only where FSharp.Core declares it.
        let rf = resolve_file(
            &ImplFile::cast(parsed.root).unwrap(),
            &ProjectItems::default(),
            crate::common::fsharp_core_env(),
            &recovery,
        );
        let at = src.rfind("= A").unwrap() + 2;
        let value_at = src.find("let A").unwrap() + 4;
        let describe = |start: usize| {
            if start == value_at {
                "value".to_string()
            } else if src[start..].starts_with('A') {
                "case".to_string()
            } else {
                format!("other@{start}")
            }
        };
        let theirs = census_resolve_uses(file, src)
            .into_iter()
            .find(|u| u.start == at && !u.is_from_definition)
            .map(|u| u.decl);
        let fcs = match theirs {
            Some(CensusDecl::InFile(s, _)) => describe(s),
            Some(CensusDecl::OtherFile) => "other-file".to_string(),
            Some(CensusDecl::Absent) | None => "-".to_string(),
        };
        let clean = if file.ok && !file.has_check_errors {
            "clean"
        } else {
            "erroring"
        };
        let ours = rf
            .resolution_at(span(at, at + 1))
            .filter(|r| matches!(r, Resolution::Local(_) | Resolution::Item(_)))
            .and_then(|r| rf.resolved_def(r))
            .map(|d| usize::from(d.range.start()));
        let verdict = match (ours, theirs) {
            (None, _) => "decline".to_string(),
            (Some(o), Some(CensusDecl::InFile(s, _))) if o == s => {
                format!("commit {}", describe(o))
            }
            (Some(o), _) => {
                failures.push(format!(
                    "  {label} [WRONG]: ours {}, FCS {fcs} ({clean})\n{src}",
                    describe(o)
                ));
                format!("WRONG {}", describe(o))
            }
        };
        entries.push(format!("{label} | fcs {fcs} ({clean}) | ours {verdict}"));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let manifest = Manifest::from_entries(entries).expect("manifest entries");
    check_manifest(
        "augmentation_header_spellings",
        &manifest,
        &regenerate("augmentation_scope_diff::header_spellings_agree_with_fcs"),
    );
}
