//! Field access on the project's own records (`r.A`, `r.I.X`), graded
//! against FCS checking the same files as one project.
//!
//! A field's type comes from its declaration: its written type, read in the
//! record's own type-parameter scope, instantiated at the receiver's type
//! arguments. FCS looks a field up by the receiver's type when it knows that
//! type at the access, and through the record-label environment when it
//! does not; inference reads a field only through a receiver it knows, so a
//! program on which the two lookups differ is one FCS rejects.
//!
//! The grid crosses record declarations (plain, generic, nested, mutable,
//! struct, a function-typed field, two records sharing a field name, a
//! measured field, an obsolete field, a private representation, members, a
//! class with a property) with where the record is declared (the using
//! file, an earlier file reached by an `open`, by module-qualified paths,
//! behind a signature) and with how the receiver is typed (an annotated
//! parameter, an earlier annotated value, a case's payload). Every node and
//! binder inference types in any file must be FCS's; each modelled cell FCS
//! checks cleanly must commit, and nothing may commit on a line FCS rejects.

use crate::common::project_grade::{Cell, Tally, assert_clean, check};

/// A record declaration and the accesses made on it.
struct Record {
    id: &'static str,
    /// The declaration(s), at the top level of their file. `{T}` in a
    /// context is `ty`.
    decl: &'static str,
    /// A ground instance of the record, as written where it is declared.
    ty: &'static str,
    /// `(access, modelled)`: the field path after the receiver (`.A`).
    accesses: &'static [(&'static str, bool)],
}

const fn r(
    id: &'static str,
    decl: &'static str,
    ty: &'static str,
    accesses: &'static [(&'static str, bool)],
) -> Record {
    Record {
        id,
        decl,
        ty,
        accesses,
    }
}

const RECORDS: &[Record] = &[
    r(
        "plain",
        "type R = { A: int; B: string }\n",
        "R",
        &[(".A", true), (".B", true)],
    ),
    r(
        "generic",
        "type G<'a> = { V: 'a; N: int; L: 'a list }\n",
        "G<string>",
        &[(".V", true), (".N", true), (".L", true)],
    ),
    r(
        "twoParams",
        "type P2<'a, 'b> = { Fst: 'a; Snd: 'b }\n",
        "P2<int, string>",
        &[(".Fst", true), (".Snd", true)],
    ),
    r(
        "nested",
        "type In = { X: int }\ntype Out = { I: In; Y: string }\n",
        "Out",
        &[(".I", true), (".I.X", true), (".Y", true)],
    ),
    r(
        "recursive",
        "type Node = { Val: int; Next: Node option }\n",
        "Node",
        &[(".Val", true), (".Next", true)],
    ),
    r(
        "mutual",
        "type Tree = { Kids: Forest; Label: string }\nand Forest = { Trees: Tree list }\n",
        "Tree",
        &[(".Kids", true), (".Kids.Trees", true), (".Label", true)],
    ),
    r(
        "mutable",
        "type Mu = { mutable C: int; D: bool }\n",
        "Mu",
        &[(".C", true), (".D", true)],
    ),
    r(
        "struct",
        "[<Struct>]\ntype S = { P: float; Q: char }\n",
        "S",
        &[(".P", true), (".Q", true)],
    ),
    r(
        "fnField",
        "type F = { Fn: int -> int; Name: string }\n",
        "F",
        // `r.Fn 1` applies the field: a method-call access, not read here.
        &[(".Fn", true), (".Name", true), (".Fn 1", false)],
    ),
    r(
        "sharedName",
        // `K` is a field of both; a receiver of `R1` reads `R1`'s.
        "type R1 = { K: int; Only1: bool }\ntype R2 = { K: string }\n",
        "R1",
        &[(".K", true), (".Only1", true)],
    ),
    r(
        "sharedNameLater",
        "type R3 = { K: int }\ntype R4 = { K: string; Only4: bool }\n",
        "R3",
        &[(".K", true)],
    ),
    r(
        "objField",
        "type Ob = { O: obj; Arr: int[] }\n",
        "Ob",
        &[(".O", true), (".Arr", true)],
    ),
    r(
        "assemblyFields",
        "type As = { Opt: int option; Res: Result<int, string>; M: Map<string, int> }\n",
        "As",
        // `Map<string, int>` is not bridged.
        &[(".Opt", true), (".Res", true), (".M", false)],
    ),
    r(
        "measured",
        "[<Measure>] type m\ntype Me = { Dist: float<m>; E: int }\n",
        "Me",
        &[(".Dist", false), (".E", true)],
    ),
    r(
        "obsoleteField",
        "type Ob2 = { [<System.Obsolete(\"no\", true)>] Old: int; New: int }\n",
        "Ob2",
        &[(".Old", false), (".New", true)],
    ),
    r(
        "privateRepr",
        // Accessible only in the record's module: from another file FCS
        // rejects every access, and nothing may commit there.
        "type PR = private { Hidden: int }\n",
        "PR",
        &[(".Hidden", true)],
    ),
    r(
        "withMembers",
        "type WM =\n    { Z: int }\n    member this.Twice = this.Z * 2\n",
        "WM",
        &[(".Z", true), (".Twice", false)],
    ),
    r(
        "escaped",
        "type Esc = { ``my field``: int; ``type``: string }\n",
        "Esc",
        &[(".``my field``", true), (".``type``", true)],
    ),
    r(
        "classProperty",
        "type C() =\n    member _.Prop = 1\n",
        "C",
        &[(".Prop", false)],
    ),
];

/// Where the record is declared, and how the using file reaches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// Above its uses, in the using file.
    InFile,
    /// In an earlier file, `open`ed.
    EarlierOpened,
    /// In an earlier file, every name qualified by its module.
    EarlierQualified,
    /// In an earlier file that has a signature: inference reads nothing of
    /// a screened file, so nothing is obligated.
    EarlierSignatured,
}

const PLACEMENTS: &[Placement] = &[
    Placement::InFile,
    Placement::EarlierOpened,
    Placement::EarlierQualified,
    Placement::EarlierSignatured,
];

/// `(id, template, needs an annotation)`: how the receiver of `{a}` is
/// typed. `{L}` qualifies a name the record's file declares; `{T}` is the
/// record's type there. An annotation is not bridged under an `open` of a
/// project module or through a qualified path to a project type
/// (`docs/sema-phase3-impl-plan.md`, *Earlier files' types*).
const CONTEXTS: &[(&str, &str, bool)] = &[
    ("param", "let f{i} (r: {LT}) = r{a}\n", true),
    ("value", "let f{i} () = ({L}r0){a}\n", false),
    ("bareValue", "let f{i} () = {L}r0{a}\n", false),
    (
        "payload",
        "let f{i} w = match w with {L}W r -> r{a} | _ -> failwith \"d\"\n",
        false,
    ),
];

/// The declarations the record's file adds: an annotated value of the
/// record's type, and a union carrying one.
fn support(record: &Record) -> String {
    format!(
        "let r0 : {t} = failwith \"r0\"\ntype W = W of {t} | WN\n",
        t = record.ty
    )
}

/// The signature of [`support`] and of the record's declarations.
fn signature(record: &Record) -> String {
    format!(
        "{}val r0 : {t}\ntype W = W of {t} | WN\n",
        record
            .decl
            .replace("member this.Twice = this.Z * 2", "member Twice : int")
            .replace(
                "type C() =\n    member _.Prop = 1\n",
                "type C =\n    new : unit -> C\n    member Prop : int\n"
            ),
        t = record.ty
    )
}

impl Placement {
    fn qualifier(self) -> &'static str {
        match self {
            Placement::EarlierQualified => "Lib.",
            _ => "",
        }
    }

    /// Whether a cell in context `cid` (needing an annotation or not) must
    /// commit here. Through module-qualified paths, the resolver does not
    /// resolve a case pattern across files (`| Lib.W r ->`), and inference
    /// does not split a module value from the members after it in one path
    /// (`Lib.r0.A`).
    fn obligates(self, cid: &str, annotated: bool) -> bool {
        match self {
            Placement::InFile => true,
            Placement::EarlierSignatured => false,
            Placement::EarlierOpened => !annotated,
            Placement::EarlierQualified => !annotated && !matches!(cid, "payload" | "bareValue"),
        }
    }
}

fn files(record: &Record, placement: Placement, body: &str) -> Vec<(String, String)> {
    let decls = format!("{}{}", record.decl, support(record));
    match placement {
        Placement::InFile => vec![("Use.fs".into(), format!("module M\n{decls}{body}"))],
        Placement::EarlierOpened | Placement::EarlierQualified => {
            let open = if placement == Placement::EarlierOpened {
                "open Lib\n"
            } else {
                ""
            };
            vec![
                ("Lib.fs".into(), format!("module Lib\n{decls}")),
                ("Use.fs".into(), format!("module M\n{open}{body}")),
            ]
        }
        Placement::EarlierSignatured => vec![
            (
                "Lib.fsi".into(),
                format!("module Lib\n{}", signature(record)),
            ),
            ("Lib.fs".into(), format!("module Lib\n{decls}")),
            ("Use.fs".into(), format!("module M\nopen Lib\n{body}")),
        ],
    }
}

fn header_lines(record: &Record, placement: Placement) -> u32 {
    match placement {
        Placement::InFile => {
            1 + format!("{}{}", record.decl, support(record))
                .matches('\n')
                .count() as u32
        }
        Placement::EarlierOpened | Placement::EarlierSignatured => 2,
        Placement::EarlierQualified => 1,
    }
}

fn program(record: &Record, placement: Placement) -> (Vec<(String, String)>, Vec<Cell>) {
    let mut body = String::new();
    let mut cells = Vec::new();
    let first = header_lines(record, placement) + 1;
    let q = placement.qualifier();
    for (i, (access, modelled)) in record.accesses.iter().enumerate() {
        for (j, (cid, template, annotated)) in CONTEXTS.iter().enumerate() {
            let n = i * CONTEXTS.len() + j;
            let line = first + body.matches('\n').count() as u32;
            body.push_str(
                &template
                    .replace("{LT}", &format!("{q}{}", record.ty))
                    .replace("{L}", q)
                    .replace("{a}", access)
                    .replace("{i}", &n.to_string()),
            );
            cells.push((
                line,
                format!("f{n}"),
                *modelled && placement.obligates(cid, *annotated),
                format!("{}/{placement:?}/{cid}/{access}", record.id),
            ));
        }
    }
    (files(record, placement, &body), cells)
}

#[test]
fn project_record_fields_agree_with_fcs_by_declaration_and_placement() {
    let mut tally = Tally::default();
    for record in RECORDS {
        for &placement in PLACEMENTS {
            let (sources, cells) = program(record, placement);
            check(&sources, &cells, &mut tally);
        }
    }
    assert_eq!(
        tally.committed_erroring, 0,
        "cells committed on lines FCS rejects: {tally:?}"
    );
    assert_clean(&tally, 150);
}

/// Accesses FCS rejects or resolves through the label environment: a field
/// the receiver's record lacks, a receiver known only after the access (FCS
/// looks the label up then, and a later annotation of another record
/// contradicts it), a field of a record shadowed by a later one sharing its
/// label, a private field from another module. Graded for agreement only.
const ILL_TYPED: &[(&str, &str)] = &[
    ("type R = { A: int }\n", "let f0 (r: R) = r.Nope\n"),
    (
        "type R = { A: int }\ntype R2 = { A: string }\n",
        "let f1 r = (r.A, (r: R))\n",
    ),
    (
        "type R = { A: int }\ntype R2 = { B: string }\n",
        "let f2 r = (r.A, (r: R2))\n",
    ),
    ("type R = { A: int }\n", "let f3 (r: R) = r.A + \"s\"\n"),
    (
        "module Inner =\n    type H = private { Secret: int }\n    let h0 : H = failwith \"h\"\n",
        "let f4 () = Inner.h0.Secret\n",
    ),
    ("type R = { A: int }\n", "let f5 (r: R) = r.A.B\n"),
];

#[test]
fn ill_typed_record_field_accesses_commit_only_what_fcs_kept() {
    let mut tally = Tally::default();
    for (decl, body) in ILL_TYPED {
        let sources = vec![("Use.fs".to_string(), format!("module M\n{decl}{body}"))];
        check(&sources, &[], &mut tally);
        let sources = vec![
            ("Lib.fs".to_string(), format!("module Lib\n{decl}")),
            ("Use.fs".to_string(), format!("module M\nopen Lib\n{body}")),
        ];
        check(&sources, &[], &mut tally);
    }
    assert_clean(&tally, 0);
}

/// A `private` record representation inside a nested module: its fields are
/// read inside the module, and nothing commits where FCS rejects an access
/// from outside it.
#[test]
fn a_private_record_representation_is_read_only_in_its_module() {
    let src = "module M\n\
               module Hidden =\n    \
                   type HR = private { S: int }\n    \
                   let mk () : HR = failwith \"h\"\n    \
                   let inside () = (mk ()).S\n\
               open Hidden\n\
               let outside () = (mk ()).S\n";
    let sources = vec![("Use.fs".to_string(), src.to_string())];
    let cells: Vec<Cell> = vec![
        (5, "inside".into(), true, "inside".into()),
        (7, "outside".into(), false, "outside".into()),
    ];
    let mut tally = Tally::default();
    check(&sources, &cells, &mut tally);
    assert_eq!(
        tally.committed_erroring, 0,
        "a field committed where FCS rejects it: {tally:?}"
    );
    assert_clean(&tally, 1);
}
