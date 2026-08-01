//! Adversarial **recovered-annotation** differential: a table of malformed type
//! annotations, each diffed against the `binder-types` oracle.
//!
//! Every other annotation differential in this crate feeds the subject
//! *well-formed* input — the generated sweep
//! (`infer_annotation_shape_gen_diff`) builds its shapes from a grammar, and the
//! project corpus is real code that compiles. So none of them can see the defect
//! this table exists for: **parser recovery drops what it cannot parse**, which
//! leaves the surviving children of a malformed annotation looking well-formed,
//! and inference publishes a type for an annotation nobody wrote.
//!
//! The contract is the one the rest of the annotation surface already holds
//! (see `infer_annotation_shape_gen_diff`'s module docs), applied to input that
//! does not parse:
//!
//! 1. FCS checks the binder's line cleanly and we commit → the renderings must
//!    match exactly;
//! 2. FCS checks it cleanly and we decline → allowed, always;
//! 3. FCS reports an **error on the binder's line** → we must commit *nothing*.
//!    A binder record still arrives in that case (FCS error-recovers), but the
//!    record cannot be read as FCS's answer: for
//!    `let v : System.String. = …` it says `System.String`, and for
//!    `let v : KeyValuePair<int, string>. = …` it says `System.Object`. Which of
//!    those a recovery produces is FCS's business, not a rule we can reproduce,
//!    so the only sound verdict on a rejected annotation is silence.
//!
//! **One case per file** is load-bearing, not tidiness: a parse error stops
//! FCS's later checking phases, so a second case sharing the file comes back
//! with no diagnostics attributed to it and reads as *accepted*. The oracle
//! calls are batched through the resident pool, so a file per case is cheap.
//!
//! [`CLEAN`] is the other half of the instrument. Arm 2 permits declining
//! everything, so a table of malformed spellings alone would stay green under a
//! guard that switched the feature off; the clean controls pin that the same
//! guard still commits on annotations that parse.

use crate::common::{invoke_fcs_dump, parse_fcs_binder_types_with_errors, temp_fs_file};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{ProjectItems, SyntaxRecovery, infer_file, resolve_file};

/// Annotation bodies whose text the parser cannot fully consume. Each is
/// spliced into `let v : {body} = failwith ""` on line 2 of its own file.
///
/// The shapes are the ways a half-written annotation ends in an editor —
/// mid-path, mid-argument-list, mid-suffix — plus the two that a purely
/// tree-local guard cannot see, where recovery flushes the junk *out* of the
/// declaration node it broke out of (`…>.`) or consumes a following token
/// *into* the annotation (`…<int,`).
const RECOVERED: &[&str] = &[
    // Trailing junk after a complete type: the junk becomes a sibling of the
    // enclosing `LET_DECL`, so neither the annotation slot's range nor the type
    // node's range nor the application's own punctuation is disturbed.
    "System.String.",
    "System.String..",
    "System.Collections.Generic.KeyValuePair<int, string>.",
    "System.Collections.Generic.List<int>..",
    // A path that stops at a separator.
    "System.",
    "System.Collections.",
    // An unterminated suffix.
    "System.String[",
    "System.String[,",
    // An unterminated argument list — the `=` that follows is consumed *into*
    // the annotation, so no text is dropped at all.
    "System.Collections.Generic.KeyValuePair<int,",
    "System.Collections.Generic.List<",
    // A list closed with a hole in it.
    "System.Collections.Generic.KeyValuePair<int, string,>",
    "System.Collections.Generic.KeyValuePair<, string>",
    // A binary type operator with nothing on its right.
    "System.String *",
    "System.String ->",
    // An unmatched opener.
    "(System.String",
    // Junk that recovery hoists into a *node* rather than loose tokens, so the
    // extent stops dead at it and the error lands on exactly the boundary
    // offset: `LET_DECL@9..30`, `EXPR_DECL@30..45`, error at `30..31`. The
    // ambiguity — this declaration's failure, or the next one's first token? —
    // is resolved towards declining.
    "System.String(",
    "System.String{",
    // A postfix application whose head is missing.
    "System.String option.",
];

/// Annotation bodies that parse cleanly, spliced the same way. These must keep
/// committing — the guard under test declines on recovery, and a guard that
/// declined on anything else would be caught here rather than by an aggregate
/// going quietly down.
///
/// A *generic application* (`System.String option`, `List<int>`) is absent
/// because `annotation_ty` declines every one of them for a reason that predates
/// this guard, so its presence here would pin someone else's deferral rather
/// than this one's absence. The recovered table carries generic spellings
/// regardless — arm 3 binds whether we reach a shape or not.
const CLEAN: &[&str] = &[
    "System.String",
    "System.Int32",
    "System.String[]",
    "System.String[,]",
    "System.String * System.Int32",
    "System.String -> System.Int32",
    "(System.String)",
];

/// What we publish for the binder `v`, if anything.
fn ours(source: &str) -> Option<String> {
    // Deliberately *not* asserting the parse is clean: the whole table is input
    // the parser must recover from, and inference runs on recovered trees in the
    // editor every keystroke.
    let parsed = parse(source);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = crate::common::full_bcl_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(&file, &resolved, env);
    inferred
        .def_types()
        .iter()
        .find(|(id, _)| resolved.def(**id).name == "v")
        .map(|(_, ty)| ty.render())
}

/// FCS's verdict for the binder on line 2: `Err(diagnostic)` if it reported an
/// error there, `Ok(Some(ty))` for a clean check, `Ok(None)` if it emitted
/// neither.
fn fcs(source: &str) -> Result<Option<String>, String> {
    let path = temp_fs_file("infer_recovered", source);
    let json = invoke_fcs_dump("binder-types", &path);
    let _ = std::fs::remove_file(&path);
    let (types, errors) = parse_fcs_binder_types_with_errors(&json, source);

    if let Some(e) = errors.iter().find(|e| e.line == 2) {
        return Err(format!("FS{:04}: {}", e.code, e.message));
    }
    // The binder `v` is the sole declaration on line 2; its declaration range is
    // the `v` token, which the oracle keys by the same byte offsets we do.
    let start = source.find("let v").expect("the harness wrote `let v`") + 4;
    Ok(types.get(&(start, start + 1)).cloned())
}

/// Splice an annotation body into the one-binder file the table is written
/// against.
fn file_for(annotation: &str) -> String {
    format!("module M\nlet v : {annotation} = failwith \"\"\n")
}

/// Arm 3, the one the table exists for: on every spelling FCS rejects, we commit
/// nothing. Arms 1 and 2 are checked too — a spelling FCS happens to accept is
/// held to exact agreement rather than being waved through.
#[test]
fn a_recovered_annotation_publishes_nothing() {
    let mut rejected = 0usize;
    let mut wrong = Vec::new();
    for annotation in RECOVERED {
        let source = file_for(annotation);
        let ours = ours(&source);
        match fcs(&source) {
            Err(diagnostic) => {
                rejected += 1;
                if let Some(ours) = ours {
                    wrong.push(format!(
                        "`{annotation}`: FCS rejects it ({diagnostic}) but we publish `{ours}`"
                    ));
                }
            }
            Ok(Some(fcs)) => {
                if let Some(ours) = ours
                    && ours != fcs
                {
                    wrong.push(format!(
                        "`{annotation}`: ours=`{ours}` FCS=`{fcs}` (FCS checked the line cleanly)"
                    ));
                }
            }
            Ok(None) => {
                if let Some(ours) = ours {
                    wrong.push(format!(
                        "`{annotation}`: we publish `{ours}` where FCS emitted no binder record \
                         and no error"
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} recovered annotations published a type FCS does not hold:\n  {}",
        wrong.len(),
        RECOVERED.len(),
        wrong.join("\n  ")
    );
    // A floor, not a target: if a future parser change made these spellings
    // legal the table would silently stop testing anything.
    assert!(
        rejected >= RECOVERED.len() - 2,
        "only {rejected} of {} spellings are still rejected by FCS — the table has drifted \
         out of the adversarial domain it was written for",
        RECOVERED.len()
    );
}

/// The clean controls still commit, and agree exactly. Without this, arm 2 makes
/// [`a_recovered_annotation_publishes_nothing`] satisfiable by declining
/// everything.
#[test]
fn a_well_formed_annotation_still_publishes() {
    let mut missing = Vec::new();
    for annotation in CLEAN {
        let source = file_for(annotation);
        let parsed = parse(&source);
        assert!(
            parsed.errors.is_empty(),
            "`{annotation}` is a clean control but our parser reports {:?}",
            parsed.errors
        );
        let fcs = fcs(&source).expect("a clean control must check cleanly under FCS");
        let fcs = fcs.expect("a clean control must produce an FCS binder record");
        match ours(&source) {
            None => missing.push(format!(
                "`{annotation}`: we publish nothing, FCS says `{fcs}`"
            )),
            Some(ours) if ours != fcs => {
                missing.push(format!("`{annotation}`: ours=`{ours}` FCS=`{fcs}`"))
            }
            Some(_) => {}
        }
    }
    assert!(
        missing.is_empty(),
        "{} of {} clean annotations regressed:\n  {}",
        missing.len(),
        CLEAN.len(),
        missing.join("\n  ")
    );
}

/// The alphabet of characters a half-written annotation can end on. Each is
/// appended to a well-formed base, which is how the shapes that defeat a
/// tree-local guard arise: some become loose sibling tokens, some get hoisted
/// into a whole sibling *node*, and some are swallowed back into the annotation.
const TRAILING: &[&str] = &[
    ".", "..", "(", ")", "[", "]", "<", ">", ",", "*", "->", "{", "}", "&", "^", "'", ":", "|",
];

/// Well-formed annotations to perturb. Kept small and structurally varied — a
/// bare path, a dotted path, a generic application, a suffix, and an operator —
/// since the generator multiplies them.
const BASES: &[&str] = &[
    "System.String",
    "System.Collections.Generic.KeyValuePair<int, string>",
    "System.String[]",
    "System.String * System.Int32",
];

/// The generated half of the instrument: every prefix of every base, and every
/// base with every trailing character appended.
///
/// [`RECOVERED`] is a hand-written table, and a hand-written table enumerates
/// the shapes its author thought of. It listed the junk-as-loose-tokens case and
/// missed junk **hoisted into a sibling node** (`System.String(`), where the
/// declaration closes early and the diagnostic lands on exactly the boundary
/// offset — a distinct structural class, not a straggler. Truncation is also
/// literally what an editor buffer holds between keystrokes, so this is the
/// realistic input, not an exotic one.
///
/// The contract is the same three arms. Almost every case here is arm 3 (FCS
/// rejects, we must be silent); the ones that happen to be legal exercise arm 1,
/// which is why the check is not simply "commit nothing".
#[test]
fn every_truncation_and_trailing_character_agrees_or_is_silent() {
    let mut cases: Vec<String> = Vec::new();
    for base in BASES {
        // Every proper prefix, on char boundaries. `1..` skips the empty
        // annotation, which is not a recovery case but a different grammar
        // production.
        for end in 1..base.len() {
            if base.is_char_boundary(end) {
                cases.push(base[..end].to_owned());
            }
        }
        for tail in TRAILING {
            cases.push(format!("{base}{tail}"));
        }
    }

    let mut wrong = Vec::new();
    let mut rejected = 0usize;
    let mut agreed = 0usize;
    for annotation in &cases {
        let source = file_for(annotation);
        let ours = ours(&source);
        match fcs(&source) {
            Err(diagnostic) => {
                rejected += 1;
                if let Some(ours) = ours {
                    wrong.push(format!(
                        "`{annotation}`: FCS rejects it ({diagnostic}) but we publish `{ours}`"
                    ));
                }
            }
            Ok(Some(fcs)) => match ours {
                Some(ours) if ours != fcs => wrong.push(format!(
                    "`{annotation}`: ours=`{ours}` FCS=`{fcs}` (FCS checked the line cleanly)"
                )),
                Some(_) => agreed += 1,
                None => {}
            },
            Ok(None) => {
                if let Some(ours) = ours {
                    wrong.push(format!(
                        "`{annotation}`: we publish `{ours}` where FCS emitted neither a binder \
                         record nor an error"
                    ));
                }
            }
        }
    }

    assert!(
        wrong.is_empty(),
        "{} of {} generated annotations published a type FCS does not hold:\n  {}",
        wrong.len(),
        cases.len(),
        wrong.join("\n  ")
    );
    // Floors, both one-sided and both about the sweep still having a subject:
    // most of the space must still be rejected (else the generator has drifted
    // out of the adversarial domain), and some of it must still be agreed (else
    // the guard has switched the feature off and every arm is vacuous).
    assert!(
        rejected >= cases.len() / 2,
        "only {rejected} of {} generated spellings are rejected by FCS",
        cases.len()
    );
    assert!(
        agreed > 0,
        "no generated spelling was checked cleanly *and* committed — the positive \
         direction is untested"
    );
}
