//! Signature files: which of an `.fsi`'s and its implementation's docs FCS
//! shows, at the definitions and at uses inside and outside the implementation
//! file — every combination of "no doc", "blank doc" and a real doc on each
//! side, on every surface a signature pairs: `val`/`let`, a type, a union
//! case, a record field, a static member, an exception, and a nested module's
//! value.
//!
//! FCS keeps two symbols. The implementation's shows its own doc, or the
//! signature's when its own is blank (`SetOtherXmlDoc` in
//! `SignatureConformance`); a use in another file binds the signature's, which
//! shows the signature's doc alone.

use borzoi::xml_doc::source::SourceDocDecline;
use proptest::prelude::*;

use super::harness::{Graded, Verdict, census, describe, failures, run_fixture};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Doc {
    None,
    Blank,
    Text(&'static str),
}

impl Doc {
    fn render(self, indent: &str) -> String {
        match self {
            Doc::None => String::new(),
            Doc::Blank => format!("{indent}///\n"),
            Doc::Text(t) => format!("{indent}/// {t}\n"),
        }
    }
}

const SIG_TEXT: &str = "from the signature";
const IMPL_TEXT: &str = "from the implementation";
const DOCS_SIG: [Doc; 3] = [Doc::None, Doc::Blank, Doc::Text(SIG_TEXT)];
const DOCS_IMPL: [Doc; 3] = [Doc::None, Doc::Blank, Doc::Text(IMPL_TEXT)];

fn project(sig: Doc, imp: Doc) -> [(&'static str, String); 3] {
    let s = |indent| sig.render(indent);
    let i = |indent| imp.render(indent);
    let fsi = format!(
        "module A\n\
         {}val v: int\n\
         {}type U =\n{}    | C1\n    | C2\n\
         {}type K =\n    new: unit -> K\n{}    static member M: int\n\
         {}exception Ex of string\n\
         {}type R =\n    {{ {}F: int }}\n\
         module Inner =\n{}    val iv: int\n",
        s(""),
        s(""),
        s("    "),
        s(""),
        s("    "),
        s(""),
        s(""),
        s("\n      "),
        s("    "),
    );
    let fs = format!(
        "module A\n\
         {}let v = 1\nlet w = v\n\
         {}type U =\n{}    | C1\n    | C2\n\
         {}type K() =\n{}    static member M = 1\n\
         {}exception Ex of string\n\
         {}type R =\n    {{ {}F: int }}\n\
         module Inner =\n{}    let iv = 2\n\
         let z = C1, (C2 : U), K.M, Ex \"m\", {{ F = 1 }}, Inner.iv\n",
        i(""),
        i(""),
        i("    "),
        i(""),
        i("    "),
        i(""),
        i(""),
        i("\n      "),
        i("    "),
    );
    let user = "module B\nlet x = A.v\nlet y = A.C1\nlet t = (A.C2 : A.U)\nlet m = A.K.M\n\
                let e = A.Ex \"m\"\nlet r : A.R = { A.F = 1 }\nlet i = A.Inner.iv\n"
        .to_string();
    [("A.fsi", fsi), ("A.fs", fs), ("B.fs", user)]
}

fn graded_for(sig: Doc, imp: Doc) -> (Vec<Graded>, [(&'static str, String); 3]) {
    let owned = project(sig, imp);
    let files: Vec<(&str, &str)> = owned.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let (graded, fcs) = run_fixture(&files, &[]);
    for f in &fcs {
        let errors: Vec<_> = f
            .diagnostics
            .iter()
            .filter(|d| d.severity == "Error")
            .map(|d| d.message.clone())
            .collect();
        assert!(errors.is_empty(), "{}: {errors:?}\n{owned:#?}", f.path);
    }
    (graded, owned)
}

#[test]
fn signature_and_implementation_docs_agree_with_fcs() {
    let mut total = std::collections::BTreeMap::new();
    for sig in DOCS_SIG {
        for imp in DOCS_IMPL {
            let (graded, owned) = graded_for(sig, imp);
            let files: Vec<(&str, &str)> = owned.iter().map(|(n, t)| (*n, t.as_str())).collect();
            let bad = failures(&graded);
            assert!(
                bad.is_empty(),
                "sig {sig:?} / impl {imp:?}:\n{}",
                bad.iter()
                    .map(|g| describe(&files, g))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            for (k, v) in census(&graded) {
                *total.entry(k).or_insert(0) += v;
            }
        }
    }
    eprintln!("signature census: {total:#?}");
}

/// The binders inside the implementation, on every surface, whose occurrences
/// there must show the signature's doc when their own is blank or absent.
const FALLBACK_SURFACES: &[&str] = &["v", "U", "C1", "M", "Ex", "iv"];

/// Every occurrence of a surface binder in `A.fs` is graded as agreeing on
/// the signature's doc when the implementation's is blank or absent — the
/// fallback FCS performs — and on the implementation's own doc otherwise.
#[test]
fn an_implementations_blank_doc_falls_back_to_its_signatures() {
    for imp in DOCS_IMPL {
        let (graded, _) = graded_for(Doc::Text(SIG_TEXT), imp);
        let expected = match imp {
            Doc::Text(t) => format!(" {t}"),
            Doc::None | Doc::Blank => format!(" {SIG_TEXT}"),
        };
        for name in FALLBACK_SURFACES {
            let sites: Vec<&Graded> = graded
                .iter()
                .filter(|g| g.file == 1 && g.name == *name)
                .collect();
            assert!(
                !sites.is_empty(),
                "impl {imp:?}: no graded `{name}` in A.fs"
            );
            for g in sites {
                assert_eq!(
                    (&g.verdict, g.fcs_lines.as_deref()),
                    (
                        &Verdict::Agree { attached: true },
                        Some(&[expected.clone()][..])
                    ),
                    "impl {imp:?}: `{name}` at {:?}",
                    g.range
                );
            }
        }
    }
}

/// A use in another file binds the signature's symbol: the signature's doc,
/// whatever the implementation's. (Resolution reaches the signature's binder
/// for these; a use it resolves into the implementation instead is answered
/// from the paired signature declaration, the same doc.)
#[test]
fn a_use_outside_the_implementation_shows_the_signature_doc_only() {
    for imp in DOCS_IMPL {
        let (graded, _) = graded_for(Doc::Text(SIG_TEXT), imp);
        for name in ["v", "C1"] {
            let sites: Vec<&Graded> = graded
                .iter()
                .filter(|g| g.file == 2 && g.name == name)
                .collect();
            assert!(
                !sites.is_empty(),
                "impl {imp:?}: no graded `{name}` in B.fs"
            );
            for g in sites {
                assert_eq!(
                    (&g.verdict, g.fcs_lines.as_deref()),
                    (
                        &Verdict::Agree { attached: true },
                        Some(&[format!(" {SIG_TEXT}")][..])
                    ),
                    "impl {imp:?}: `{name}` in B.fs at {:?}",
                    g.range
                );
            }
        }
    }
}

fn assert_compiles(fcs: &[super::harness::OracleFile], files: &[(&str, &str)]) {
    for f in fcs {
        let errors: Vec<_> = f
            .diagnostics
            .iter()
            .filter(|d| d.severity == "Error")
            .map(|d| format!("FS{:04} {}", d.error_number, d.message))
            .collect();
        assert!(errors.is_empty(), "{}: {errors:?}\n{files:#?}", f.path);
    }
}

/// A property the signature declares twice (getter and setter apart) has two
/// signature declarations of one name under one type: names cannot say which
/// FCS pairs, so a blank implementation doc declines rather than guess.
#[test]
fn a_name_the_signature_declares_twice_declines() {
    let fsi = "module A\ntype K =\n    new: unit -> K\n    /// get\n    static member P: int with get\n    /// set\n    static member P: int with set\n";
    let fs = "module A\ntype K() =\n    static member P with get () = 1 and set (_: int) = ()\nlet z = K.P\n";
    let files = [("A.fsi", fsi), ("A.fs", fs)];
    let (graded, fcs) = run_fixture(&files, &[]);
    assert_compiles(&fcs, &files);
    assert!(failures(&graded).is_empty(), "{graded:#?}");
    let sites: Vec<&Graded> = graded
        .iter()
        .filter(|g| g.name == "P" && g.file == 1)
        .collect();
    assert!(!sites.is_empty(), "no graded `P`: {graded:#?}");
    for g in sites {
        assert_eq!(
            g.verdict,
            Verdict::Declined(SourceDocDecline::SignaturePairingAmbiguous),
            "{g:?}"
        );
    }
}

/// Trivia before a declaration on one side of the pair, in the shapes that
/// decide what the collector attaches and whether it is blank.
#[derive(Debug, Clone, Copy)]
enum Piece {
    Doc(&'static str),
    LineComment,
    /// A doc line in a dead `#if` region.
    Dead(&'static str),
    Blank,
}

/// Per-side hazards: absent, real, empty, whitespace-only, a block a comment
/// restarts, a block a comment trails, a dead region, XML, a blank line before
/// text, a blank source line after text.
const HAZARDS: &[&[Piece]] = &[
    &[],
    &[Piece::Doc(" text")],
    &[Piece::Doc("")],
    &[Piece::Doc("   ")],
    &[
        Piece::Doc(" first"),
        Piece::LineComment,
        Piece::Doc(" second"),
    ],
    &[Piece::Doc(" kept"), Piece::LineComment],
    &[Piece::Dead(" dead")],
    &[Piece::Doc("<summary>x</summary>")],
    &[Piece::Doc(""), Piece::Doc(" after blank")],
    &[Piece::Doc(" text"), Piece::Blank],
];

/// One side's rendering context: its hazard, and a mark appended to every
/// non-blank doc line — the side and the slot's number — so no two
/// declarations' docs coincide: an answer from the wrong side, or from a
/// neighbour, cannot agree by accident.
struct Side {
    hazard: &'static [Piece],
    mark: &'static str,
    slots: std::cell::Cell<usize>,
}

impl Side {
    fn new(hazard: &'static [Piece], mark: &'static str) -> Self {
        Side {
            hazard,
            mark,
            slots: std::cell::Cell::new(0),
        }
    }

    fn doc(&self, indent: &str, out: &mut String) {
        let slot = self.slots.get();
        self.slots.set(slot + 1);
        let mark = format!("{}{slot}", self.mark);
        for p in self.hazard {
            match p {
                Piece::Doc(t) => {
                    let mark = if t.trim().is_empty() { "" } else { &mark };
                    out.push_str(&format!("{indent}///{t}{mark}\n"));
                }
                Piece::LineComment => out.push_str(&format!("{indent}// ordinary\n")),
                Piece::Dead(t) => {
                    out.push_str(&format!("#if UNDEFINED_X\n{indent}///{t}{mark}\n#endif\n"))
                }
                Piece::Blank => out.push('\n'),
            }
        }
    }
}

/// A pairing surface: a declaration as the signature and the implementation
/// spell it, with uses in the implementation and from another file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    Let,
    Function,
    Type,
    Union,
    RecordField,
    /// Static members (a property, a method, an auto-property whose
    /// initialiser names another member), which resolution reaches, and an
    /// instance member whose self identifier is a local.
    Member,
    Exception,
    NestedModule,
    Enum,
    /// A namesake in the values-and-members table under another parent: a
    /// module value and a static member of one name.
    MemberNamesake,
    /// An implementation declaration the signature does not mention.
    Hidden,
}

const SURFACES: &[Surface] = &[
    Surface::Let,
    Surface::Function,
    Surface::Type,
    Surface::Union,
    Surface::RecordField,
    Surface::Member,
    Surface::Exception,
    Surface::NestedModule,
    Surface::Enum,
    Surface::MemberNamesake,
    Surface::Hidden,
];

impl Surface {
    fn signature(self, n: usize, s: &Side, out: &mut String) {
        let d = |indent: &str, out: &mut String| s.doc(indent, out);
        match self {
            Surface::Let => {
                d("", out);
                out.push_str(&format!("val v{n}: int\n"));
            }
            Surface::Function => {
                d("", out);
                out.push_str(&format!("val f{n}: int -> int\n"));
            }
            Surface::Type => {
                d("", out);
                out.push_str(&format!("type T{n} = int\n"));
            }
            Surface::Union => {
                d("", out);
                out.push_str(&format!("type U{n} =\n"));
                d("    ", out);
                out.push_str(&format!("    | A{n}\n"));
                d("    ", out);
                out.push_str(&format!("    | B{n} of int\n"));
            }
            Surface::RecordField => {
                d("", out);
                out.push_str(&format!("type R{n} =\n    {{\n"));
                d("      ", out);
                out.push_str(&format!("      RF{n}: int\n"));
                d("      ", out);
                out.push_str(&format!("      RG{n}: string\n    }}\n"));
            }
            Surface::Member => {
                d("", out);
                out.push_str(&format!("type K{n} =\n    new: unit -> K{n}\n"));
                d("    ", out);
                out.push_str(&format!("    static member S{n}: int\n"));
                d("    ", out);
                out.push_str(&format!("    static member M{n}: x: int -> int\n"));
                d("    ", out);
                out.push_str(&format!("    member I{n}: int\n"));
                d("    ", out);
                out.push_str(&format!("    static member V{n}: int with get, set\n"));
            }
            Surface::Exception => {
                d("", out);
                out.push_str(&format!("exception E{n} of string\n"));
            }
            Surface::NestedModule => {
                d("", out);
                out.push_str(&format!("module N{n} =\n"));
                d("    ", out);
                out.push_str(&format!("    val nv{n}: int\n"));
            }
            Surface::Enum => {
                d("", out);
                out.push_str(&format!("type C{n} =\n"));
                d("    ", out);
                out.push_str(&format!("    | Red{n} = 0\n"));
                d("    ", out);
                out.push_str(&format!("    | Green{n} = 1\n"));
            }
            Surface::MemberNamesake => {
                d("", out);
                out.push_str(&format!("val nm{n}: int\n"));
                out.push_str(&format!("type NK{n} =\n    new: unit -> NK{n}\n"));
                d("    ", out);
                out.push_str(&format!("    static member nm{n}: int\n"));
            }
            Surface::Hidden => {}
        }
    }

    fn implementation(self, n: usize, s: &Side, out: &mut String) {
        let d = |indent: &str, out: &mut String| s.doc(indent, out);
        match self {
            Surface::Let => {
                d("", out);
                out.push_str(&format!("let v{n} = 1\nlet _ = v{n}\n"));
            }
            Surface::Function => {
                d("", out);
                out.push_str(&format!("let f{n} x = x + 1\nlet _ = f{n} 1\n"));
            }
            Surface::Type => {
                d("", out);
                out.push_str(&format!("type T{n} = int\nlet _ = (1 : T{n})\n"));
            }
            Surface::Union => {
                d("", out);
                out.push_str(&format!("type U{n} =\n"));
                d("    ", out);
                out.push_str(&format!("    | A{n}\n"));
                d("    ", out);
                out.push_str(&format!(
                    "    | B{n} of int\nlet _ = B{n} 1, A{n}, (A{n} : U{n})\n"
                ));
            }
            Surface::RecordField => {
                d("", out);
                out.push_str(&format!("type R{n} =\n    {{\n"));
                d("      ", out);
                out.push_str(&format!("      RF{n}: int\n"));
                d("      ", out);
                out.push_str(&format!(
                    "      RG{n}: string\n    }}\nlet _ = ({{ RF{n} = 1; RG{n} = \"\" }} : R{n}).RF{n}\n"
                ));
            }
            Surface::Member => {
                d("", out);
                out.push_str(&format!("type K{n}() =\n"));
                d("    ", out);
                out.push_str(&format!("    static member S{n} = 1\n"));
                d("    ", out);
                out.push_str(&format!("    static member M{n} (x: int) = x\n"));
                d("    ", out);
                out.push_str(&format!("    member this.I{n} = this.GetHashCode()\n"));
                d("    ", out);
                out.push_str(&format!(
                    "    static member val V{n} = K{n}.S{n} with get, set\nlet _ = K{n}.S{n}, K{n}.M{n} 1, K{n}().I{n}, K{n}.V{n}\n"
                ));
            }
            Surface::Exception => {
                d("", out);
                out.push_str(&format!("exception E{n} of string\nlet _ = E{n} \"m\"\n"));
            }
            Surface::NestedModule => {
                d("", out);
                out.push_str(&format!("module N{n} =\n"));
                d("    ", out);
                out.push_str(&format!("    let nv{n} = 1\nlet _ = N{n}.nv{n}\n"));
            }
            Surface::Enum => {
                d("", out);
                out.push_str(&format!("type C{n} =\n"));
                d("    ", out);
                out.push_str(&format!("    | Red{n} = 0\n"));
                d("    ", out);
                out.push_str(&format!("    | Green{n} = 1\nlet _ = C{n}.Red{n}\n"));
            }
            Surface::MemberNamesake => {
                d("", out);
                out.push_str(&format!("let nm{n} = 1\ntype NK{n}() =\n"));
                d("    ", out);
                out.push_str(&format!(
                    "    static member nm{n} = 2\nlet _ = nm{n}, NK{n}.nm{n}\n"
                ));
            }
            Surface::Hidden => {
                d("", out);
                out.push_str(&format!("let h{n} = 1\nlet _ = h{n}\n"));
            }
        }
    }

    /// Uses from another file, qualified by the implementation's module `m`.
    fn uses(self, n: usize, m: &str, out: &mut String) {
        let line = match self {
            Surface::Let => format!("let _ = {m}.v{n}"),
            Surface::Function => format!("let _ = {m}.f{n} 1"),
            Surface::Type => format!("let _ = (1 : {m}.T{n})"),
            Surface::Union => format!("let _ = {m}.B{n} 1, {m}.A{n}, ({m}.A{n} : {m}.U{n})"),
            Surface::RecordField => {
                format!("let _ = ({{ {m}.RF{n} = 1; {m}.RG{n} = \"\" }} : {m}.R{n}).RF{n}")
            }
            Surface::Member => format!("let _ = {m}.K{n}.S{n}, {m}.K{n}.M{n} 1, {m}.K{n}.V{n}"),
            Surface::Exception => format!("let _ = {m}.E{n} \"m\""),
            Surface::NestedModule => format!("let _ = {m}.N{n}.nv{n}"),
            Surface::Enum => format!("let _ = {m}.C{n}.Red{n}"),
            Surface::MemberNamesake => format!("let _ = {m}.nm{n}, {m}.NK{n}.nm{n}"),
            Surface::Hidden => return,
        };
        out.push_str(&line);
        out.push('\n');
    }
}

const SIG_MARK: &str = " @sig";
const IMPL_MARK: &str = " @impl";

/// The sig × impl matrix as one project: a signature and implementation pair
/// per (signature hazard, implementation hazard), every surface in each, and a
/// last file using every surface of every pair.
fn matrix(surfaces: &[Surface], pairs: &[(usize, usize)]) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut user = String::from("module User\n");
    for (k, &(sh, ih)) in pairs.iter().enumerate() {
        let module = format!("P{k}");
        let sig = Side::new(HAZARDS[sh], SIG_MARK);
        let imp = Side::new(HAZARDS[ih], IMPL_MARK);
        let mut fsi = format!("module {module}\n");
        let mut fs = format!("module {module}\n");
        for (n, surface) in surfaces.iter().enumerate() {
            surface.signature(n, &sig, &mut fsi);
            surface.implementation(n, &imp, &mut fs);
            surface.uses(n, &module, &mut user);
        }
        files.push((format!("{module}.fsi"), fsi));
        files.push((format!("{module}.fs"), fs));
    }
    files.push(("User.fs".to_string(), user));
    files
}

/// Every pair of hazards, signature side × implementation side.
fn all_pairs() -> Vec<(usize, usize)> {
    (0..HAZARDS.len())
        .flat_map(|s| (0..HAZARDS.len()).map(move |i| (s, i)))
        .collect()
}

#[test]
fn generated_signature_matrix_agrees_with_fcs() {
    let owned = matrix(SURFACES, &all_pairs());
    let files: Vec<(&str, &str)> = owned
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    let (graded, fcs) = run_fixture(&files, &[]);
    assert_compiles(&fcs, &files);
    eprintln!("signature matrix census: {:#?}", census(&graded));
    let bad = failures(&graded);
    assert!(
        bad.is_empty(),
        "{} occurrence(s) where hover would show a doc FCS does not attach:\n{}",
        bad.len(),
        bad.iter()
            .take(30)
            .map(|g| describe(&files, g))
            .collect::<Vec<_>>()
            .join("\n")
    );
    // Every surface pairs exactly by name, so nothing in the matrix may
    // decline for want of a pairing.
    let unpaired: Vec<String> = graded
        .iter()
        .filter(|g| {
            matches!(
                g.verdict,
                Verdict::Declined(
                    SourceDocDecline::SignaturePairingAmbiguous
                        | SourceDocDecline::SignatureSurfaceUnmodelled
                )
            )
        })
        .map(|g| describe(&files, g))
        .collect();
    assert!(
        unpaired.is_empty(),
        "{} pairing decline(s):\n{}",
        unpaired.len(),
        unpaired.join("\n")
    );
    // Non-vacuity: every kind that can carry a doc shows the signature's doc
    // at an implementation-file occurrence somewhere — the fallback ran.
    let fell_back: std::collections::BTreeSet<String> = graded
        .iter()
        .filter(|g| files[g.file].0.ends_with(".fs") && files[g.file].0 != "User.fs")
        .filter(|g| matches!(g.verdict, Verdict::Agree { attached: true }))
        .filter(|g| {
            g.fcs_lines
                .as_ref()
                .is_some_and(|l| l.iter().any(|l| l.contains(SIG_MARK)))
        })
        .map(|g| format!("{:?}", g.def_kind))
        .collect();
    eprintln!("kinds shown the signature's doc in the implementation: {fell_back:?}");
    for kind in [
        "Value { is_function: false }",
        "Value { is_function: true }",
        "Type",
        "UnionCase",
        "EnumCase",
        "ExceptionCase",
        "Member",
    ] {
        assert!(
            fell_back.contains(kind),
            "no fallback to the signature's doc for {kind} — the matrix no longer exercises it"
        );
    }
}

fn pair_strategy() -> impl Strategy<Value = (usize, usize)> {
    (0..HAZARDS.len(), 0..HAZARDS.len())
}

proptest! {
    /// Random subsets and orders of surfaces over random hazard pairs: hover
    /// never shows a doc FCS does not attach.
    #[test]
    fn random_signature_pairs_agree_with_fcs(
        surfaces in proptest::collection::vec(proptest::sample::select(SURFACES), 1..5),
        pairs in proptest::collection::vec(pair_strategy(), 1..3),
    ) {
        let owned = matrix(&surfaces, &pairs);
        let files: Vec<(&str, &str)> = owned.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
        let (graded, _) = run_fixture(&files, &[]);
        let bad = failures(&graded);
        prop_assert!(
            bad.is_empty(),
            "divergence in\n{owned:#?}\n{}",
            bad.iter().map(|g| describe(&files, g)).collect::<Vec<_>>().join("\n")
        );
    }
}
