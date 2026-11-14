//! Where FCS takes its order-dependent decisions, pinned on the shapes that
//! tell its checking order from the order of the text.
//!
//! FCS checks an application's argument with the expected type the function
//! gives it, so it unifies the application's result with what the context
//! expects before it checks the arguments (`Propagate`), and a lambda's
//! parameters with its expected domain before its body. An argument's
//! relation is decided at the argument's **entry**: whatever the argument's
//! sub-expressions then do, they do knowing the domain. Inference today
//! decides its argument checks at the binding's end, after the argument has
//! been walked, which is the **exit** order. The two agree on most programs;
//! these are the shapes where they do not, beside controls where they do.
//!
//! Each case is one `let` of the file [`SOURCE`], against FCS. For every FCS
//! expression node and binder in the case, and every type we commit there,
//! the manifest pins both sides, and it pins the case's census entry and FCS's
//! error codes, so a stage of #662 that moves a decision from exit to entry
//! shows up as a line diff here. Certain-implies-exact is asserted on top:
//! what we commit, FCS has, the same.
//!
//! Regenerate with
//! `BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-sema --test all infer_check_order_pins::`.

use std::collections::BTreeMap;

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_sema::{PrecedingTypes, ProjectItems, SyntaxRecovery, infer_file, resolve_file};

use crate::common::corpus_manifest::{check_manifest, regenerate};
use crate::common::{GradedDumps, full_bcl_env, invoke_fcs_dump, temp_fs_file};

/// The shared header: the functions the cases apply.
const HEADER: &str = "module Pins\n\
                      let idf x = x\n\
                      let singleton x = [x]\n\
                      let g (xs: obj list) = 0\n\
                      let gs (xs: string list) = 0\n\
                      let apply (k: string -> int) = k \"x\"\n\
                      let monos (t: string) = 2\n\
                      let uu = fst ((7, \"u\"), 1)\n";

/// Each case: its name, its `let`, and what entry and exit order each make of
/// it.
const CASES: &[(&str, &str, &str)] = &[
    (
        "singleton-into-obj-list",
        "let r1 (y: string) = g (singleton y)",
        "entry: `singleton y` is checked against `obj list`, so its element type is `obj` \
         before `y` is checked, and `y` coerces to it; FCS accepts. exit: walking \
         `singleton y` first fixes its element type to `string`, and `string list` is \
         not `obj list`",
    ),
    (
        "singleton-into-string-list",
        "let r2 (y: string) = gs (singleton y)",
        "control: both orders give `string list`",
    ),
    (
        "id-wrapped-lambda-access",
        "let r3 = apply (idf (fun s -> s.Length))",
        "entry: `idf`'s result is unified with `string -> int` before its argument is \
         checked, so the lambda's `s` is a string at `s.Length`. exit: `s` is open at the \
         access (FS0072)",
    ),
    (
        "lambda-access",
        "let r4 = apply (fun s -> s.Length)",
        "entry: the lambda is checked against `string -> int`, so `s` is a string at the \
         access. exit: `s` is open there",
    ),
    (
        "id-wrapped-access",
        "let r5 (x: string) = (idf x).Length",
        "control: the receiver's type is known once `idf x` is checked, in either order",
    ),
    (
        "access-before-grounding",
        "let r6 x = (x.Length, monos x)",
        "control: in either order `x` is open at the access, which FCS rejects (FS0072) \
         before `monos x` grounds it",
    ),
    (
        "argument-check-before-a-complete-suffix",
        "let r7 () = (idf uu, None)",
        "the argument check of `idf uu` fires only in a complete binding today, as at the \
         binding's end; FCS decides it at the argument",
    ),
    (
        "argument-check-before-an-unmodelled-suffix",
        "let r8 () = (idf uu, Unchecked.defaultof<_>)",
        "as above, with a suffix inference does not model: today the binding is \
         incomplete and the check never fires; FCS's type for `idf uu` is the same",
    ),
];

/// The file every case is a `let` of.
fn source() -> (String, Vec<(usize, usize)>) {
    let mut src = HEADER.to_string();
    let mut spans = Vec::new();
    for (_, text, _) in CASES {
        let start = src.len();
        src.push_str(text);
        spans.push((start, src.len()));
        src.push('\n');
    }
    (src, spans)
}

#[test]
fn checking_order_cases_are_pinned() {
    let (src, spans) = source();
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}\n{src}", parsed.errors);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = full_bcl_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(&file, &resolved, env, &PrecedingTypes::default());

    let path = temp_fs_file("infer_check_order_pins", &src);
    let types_json = invoke_fcs_dump("types", &path);
    let binders_json = invoke_fcs_dump("binder-types", &path);
    let _ = std::fs::remove_file(&path);
    let dumps = GradedDumps::read(&types_json, &binders_json, &src, Some(&file))
        .expect("every declaration placed");

    // Header bindings come first in the census, one entry per counted binding.
    let census = inferred.incompleteness();
    let header_bindings = HEADER.matches("\nlet ").count();
    assert_eq!(
        census.len(),
        header_bindings + CASES.len(),
        "one census entry per binding: {census:?}"
    );
    let ours: BTreeMap<(usize, usize), String> = inferred
        .types()
        .iter()
        .map(|(r, t)| {
            (
                (u32::from(r.start()) as usize, u32::from(r.end()) as usize),
                t.render(env),
            )
        })
        .collect();
    let our_binders: BTreeMap<(usize, usize), String> = inferred
        .def_types()
        .iter()
        .map(|(d, t)| {
            let r = resolved.def(*d).range;
            (
                (u32::from(r.start()) as usize, u32::from(r.end()) as usize),
                t.render(env),
            )
        })
        .collect();

    let mut entries = Vec::new();
    let mut unsound = Vec::new();
    let line_of = |offset: usize| src[..offset].matches('\n').count() as u32 + 1;
    for (k, ((name, _, _), &(start, end))) in CASES.iter().zip(&spans).enumerate() {
        let mut codes: Vec<u32> = dumps
            .errors
            .iter()
            .filter(|e| e.line == line_of(start))
            .map(|e| e.code)
            .collect();
        codes.sort();
        codes.dedup();
        let fcs = if codes.is_empty() {
            "clean".to_string()
        } else {
            let codes: Vec<String> = codes.iter().map(|c| format!("FS{c:04}")).collect();
            format!("errors {}", codes.join(" "))
        };
        entries.push((format!("{name} fcs"), fcs));
        entries.push((
            format!("{name} census"),
            format!("{:?}", census[header_bindings + k]),
        ));
        for (kind, mine, theirs) in [
            ("node", &ours, &dumps.types),
            ("binder", &our_binders, &dumps.binders),
        ] {
            let mut at: Vec<(usize, usize)> = mine
                .keys()
                .chain(theirs.keys())
                .filter(|&&(s, e)| s >= start && e <= end)
                .copied()
                .collect();
            at.sort();
            at.dedup();
            for (s, e) in at {
                let (m, t) = (mine.get(&(s, e)), theirs.get(&(s, e)));
                if let Some(m) = m
                    && t != Some(m)
                {
                    unsound.push(format!("{name}: `{}` ours {m}, FCS {t:?}", &src[s..e]));
                }
                entries.push((
                    format!("{name} {kind} {}-{} `{}`", s - start, e - start, &src[s..e]),
                    format!(
                        "ours {} fcs {}",
                        m.map_or("-", String::as_str),
                        t.map_or("-", String::as_str)
                    ),
                ));
            }
        }
    }
    assert!(
        unsound.is_empty(),
        "committed types FCS does not have:\n{}",
        unsound.join("\n")
    );
    let manifest = Manifest::from_keyed(entries).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check_manifest(
        "infer_check_order_pins",
        &manifest,
        &regenerate("infer_check_order_pins::"),
    );
}
