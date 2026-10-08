//! Signature files: which of an `.fsi`'s and its implementation's docs FCS
//! shows, at the definitions and at uses inside and outside the implementation
//! file — every combination of "no doc", "blank doc" and a real doc on each
//! side.

use super::harness::{Graded, Verdict, census, describe, failures, run_fixture};

#[derive(Debug, Clone, Copy)]
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

const DOCS_SIG: [Doc; 3] = [Doc::None, Doc::Blank, Doc::Text("from the signature")];
const DOCS_IMPL: [Doc; 3] = [Doc::None, Doc::Blank, Doc::Text("from the implementation")];

fn project(sig: Doc, imp: Doc) -> [(&'static str, String); 3] {
    let fsi = format!(
        "module A\n{}val v: int\n{}type U =\n{}    | C1\n    | C2\n",
        sig.render(""),
        sig.render(""),
        sig.render("    "),
    );
    let fs = format!(
        "module A\n{}let v = 1\nlet w = v\n{}type U =\n{}    | C1\n    | C2\nlet z = C1, (C2 : U)\n",
        imp.render(""),
        imp.render(""),
        imp.render("    "),
    );
    let user = "module B\nlet x = A.v\nlet y = A.C1\nlet t = (A.C2 : A.U)\n".to_string();
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
        assert!(errors.is_empty(), "{}: {errors:?}", f.path);
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

/// The verdict and FCS's lines at the occurrence of `name` in `file`, for the
/// `n`th such occurrence.
fn at(graded: &[Graded], file: usize, name: &str, n: usize) -> (Verdict, Vec<String>) {
    let g = graded
        .iter()
        .filter(|g| g.file == file && g.name == name)
        .nth(n)
        .unwrap_or_else(|| panic!("no occurrence {n} of {name} in file {file}: {graded:#?}"));
    (g.verdict.clone(), g.fcs_lines.clone().unwrap_or_default())
}

#[test]
fn a_use_outside_the_implementation_shows_the_signature_doc_only() {
    let (graded, _) = graded_for(Doc::Text("s"), Doc::Text("i"));
    // `A.v` in B.fs: FCS binds the signature's value.
    let (verdict, fcs) = at(&graded, 2, "v", 0);
    assert_eq!(fcs, [" s"]);
    assert_eq!(verdict, Verdict::Agree { attached: true });
    // Inside A.fs the implementation's own doc wins.
    let (verdict, fcs) = at(&graded, 1, "v", 0);
    assert_eq!(fcs, [" i"]);
    assert_eq!(verdict, Verdict::Agree { attached: true });
}

#[test]
fn a_blank_implementation_doc_falls_back_to_the_signature_and_we_decline() {
    let (graded, _) = graded_for(Doc::Text("s"), Doc::Blank);
    let (verdict, fcs) = at(&graded, 1, "v", 0);
    assert_eq!(fcs, [" s"]);
    assert_eq!(
        verdict,
        Verdict::Declined(borzoi::xml_doc::source::SourceDocDecline::SignatureFallback)
    );
}
