//! Does an in-scope **extension member** displace the intrinsic one a member
//! access would otherwise read?
//!
//! The overload engine treats an extension of the call's name as a landmine and
//! defers on it: an applicable extension joins FCS's method group and can beat an
//! applicable intrinsic on any betterness rule
//! ([`ExtensionScope`](borzoi_sema::AssemblyEnv), `docs/extension-scope-enumeration-plan.md`).
//! The Stage 3.3a **data**-member wake consults no such gate — it commits the
//! single unambiguous public readable instance field / non-indexer property and
//! never asks whether an extension of the name is in scope.
//!
//! That is sound only if F# resolves a data access to the intrinsic member
//! whenever one exists. This differential is where that claim lives, because it
//! is a claim about the language: nothing in the engine enforces it, and if it is
//! wrong the LSP serves go-to-definition pointing at the intrinsic member while
//! the compiler reads the extension.
//!
//! # The universe
//!
//! The C# assembly is [`member_hiding_corpus`](crate::common::member_hiding_corpus)'s
//! — it already declares, under one probed name `P`, every shape the wake
//! distinguishes: a readable property, a field, an inherited member, a
//! write-only property, a static, a method group, and nothing at all. What
//! changes here is the F# side, which the hiding corpus deliberately keeps free
//! of extension sources: each probe file augments the receiver's type with a
//! member **of the same name `P`**, in the file's own module so that it is in
//! scope for everything after it, then reads `v.P`.
//!
//! Both extension shapes are swept, since they compete differently: an extension
//! **property** is a data member of the name (the direct contest), and an
//! extension **method** puts a method of the name beside an intrinsic property
//! (the kind-crossing contest).
//!
//! # The property
//!
//! Per probe, exactly the hiding corpus's first gate: **we commit ⇒ FCS bound a
//! data member of that name on that declaring type, on a line it checked without
//! error.** Since the extension is declared in the probe file, its declaring
//! entity is the F# module `Ext` — so "we answered the intrinsic and FCS read the
//! extension" is a declaring-entity mismatch, which is exactly what fails.
//!
//! [`SHADOWED_CELLS`] ratchets which cells still answer with an extension in
//! scope: an engine that defers everything satisfies the property vacuously, and
//! a cell that stops answering is availability lost to a gate someone added.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::common::member_hiding_corpus::{NS, PROBE, Site, corpus};
use crate::common::{
    NormalisedUse, ensure_member_hiding_corpus_built, ensure_system_runtime_dll,
    invoke_fcs_dump_with_refs, parse_fcs_types_with_errors, parse_fcs_uses, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, infer_file, resolve_file};

/// The hiding-corpus cells this sweep re-probes with an extension in scope: one
/// per shape the wake distinguishes, chosen so that both its answering and its
/// declining verdicts are represented. A cell is named by its corpus label.
const PROBED_CELLS: [&str; 7] = [
    // We answer these — the contest the claim is about.
    "grid base=none derived=prop",
    "grid base=none derived=field",
    "grid base=prop derived=none",
    // We decline these: no intrinsic data member is readable, so the extension is
    // the only `P` there is, and FCS binds it.
    "grid base=none derived=none",
    "grid base=none derived=writeonly",
    "grid base=none derived=staticprop",
    "grid base=none derived=methodgroup",
];

/// The cells that still answer with an extension member of the name in scope,
/// per extension shape. A two-sided ratchet: the soundness property is satisfied
/// vacuously by deferring, so a cell that stops answering has to be explained.
const SHADOWED_CELLS: [(ExtKind, &[&str]); 2] = [
    (
        ExtKind::Property,
        &[
            "grid base=none derived=prop",
            "grid base=none derived=field",
            "grid base=prop derived=none",
        ],
    ),
    (
        ExtKind::Method,
        &[
            "grid base=none derived=prop",
            "grid base=none derived=field",
            "grid base=prop derived=none",
        ],
    ),
];

/// How the probe file extends the receiver's type under the probed name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtKind {
    /// `member this.P = "ext"` — an extension **property**, a data member of the
    /// name standing directly against the intrinsic one.
    Property,
    /// `member this.P () = "ext"` — an extension **method**, which puts a member
    /// of another *kind* at the name.
    Method,
}

impl ExtKind {
    fn tag(self) -> &'static str {
        match self {
            ExtKind::Property => "extension property",
            ExtKind::Method => "extension method",
        }
    }

    /// The member declaration this shape contributes to the `Ext` module.
    fn declaration(self) -> String {
        match self {
            ExtKind::Property => format!("    member this.{PROBE} = \"ext\""),
            ExtKind::Method => format!("    member this.{PROBE} () = \"ext\""),
        }
    }
}

/// One augmentation the probe file declares: which type it extends, and where.
struct Augmentation {
    receiver_ty: String,
    /// 1-based line of the `type X with` header.
    header_line: usize,
    /// 1-based line of the `member this.P …` declaration.
    member_line: usize,
}

/// One probe: a cell of the hiding corpus, read with an extension in scope.
struct Probe {
    label: String,
    receiver_ty: String,
    line: usize,
    text: String,
}

/// Render the probe file for `kind`: an `Ext` module extending each probed cell's
/// receiver type under the probed name, then one access per cell.
fn probe_source(sites: &[&Site], kind: ExtKind) -> (String, Vec<Probe>, Vec<Augmentation>) {
    let mut src = String::from("module Gen\n");
    let mut line = 1usize;
    let mut augmentations = Vec::new();
    // The augmentations sit in the file's own module, so they are in scope for
    // everything after them without an `open`. One per *distinct* receiver type:
    // two cells can share one (the corpus does not, today, but the rendering must
    // not depend on that).
    let mut extended: BTreeSet<&str> = BTreeSet::new();
    for site in sites {
        if !extended.insert(site.receiver_ty.as_str()) {
            continue;
        }
        src.push_str(&format!("type {NS}.{} with\n", site.receiver_ty));
        src.push_str(&kind.declaration());
        src.push('\n');
        line += 2;
        augmentations.push(Augmentation {
            receiver_ty: site.receiver_ty.clone(),
            header_line: line - 1,
            member_line: line,
        });
    }

    let mut probes = Vec::new();
    for (i, site) in sites.iter().enumerate() {
        src.push_str(&format!("let v{i} = {NS}.Make.{}()\n", site.factory));
        line += 1;
        let text = format!("let r{i} = v{i}.{PROBE}");
        src.push_str(&text);
        src.push('\n');
        line += 1;
        probes.push(Probe {
            label: site.label.clone(),
            receiver_ty: site.receiver_ty.clone(),
            line,
            text,
        });
    }
    (src, probes, augmentations)
}

/// What one side answered at a probe: the declaring entity's compiled name, or
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    Declaring(String),
    None,
}

/// The module the probe file's augmentations live in — the file's own module, so
/// its members are in scope for everything after them. FCS names it as the
/// declaring entity of an extension member it binds.
const EXT_MODULE: &str = "Gen";

/// The `SymbolKind`s FCS reports for a data member — the only answers that can
/// confirm one of ours. Kept in step with `member_hiding_diff`'s.
const DATA_MEMBER_KINDS: [&str; 2] = ["member", "field"];

/// Byte offset of the start of every line in `src`.
fn line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        src.bytes()
            .enumerate()
            .filter(|&(_, b)| b == b'\n')
            .map(|(i, _)| i + 1),
    );
    starts
}

/// The 1-based line containing byte offset `at`.
fn line_at(starts: &[usize], at: usize) -> usize {
    match starts.binary_search(&at) {
        Ok(i) => i + 1,
        Err(i) => i,
    }
}

/// The compiled name of the entity FCS says declares a use's symbol. A use with
/// no declaring path renders as a marker that can never compare equal.
fn declaring_name(use_: &NormalisedUse) -> String {
    match use_.declaring_path.as_ref().and_then(|path| path.last()) {
        Some((name, _)) => name.clone(),
        None => format!("<no-declaring-path:{:?}>", use_.full_name),
    }
}

/// Run one extension shape over the probed cells and return, per probe, what each
/// side answered plus the lines FCS reported an error on.
fn run(kind: ExtKind) -> (Vec<Probe>, Vec<(Answer, Answer)>, HashSet<usize>) {
    let corpus = corpus();
    let dll = ensure_member_hiding_corpus_built(&corpus.csharp);
    let system_runtime = ensure_system_runtime_dll();

    let sites: Vec<&Site> = PROBED_CELLS
        .iter()
        .map(|label| {
            corpus
                .sites
                .iter()
                .find(|site| site.label == *label)
                .unwrap_or_else(|| panic!("no hiding-corpus cell labelled {label:?}"))
        })
        .collect();
    let (src, probes, augmentations) = probe_source(&sites, kind);

    let bcl_bytes = std::fs::read(&system_runtime).expect("read System.Runtime.dll");
    let corpus_bytes = std::fs::read(dll).expect("read MemberHiding.dll");
    let bcl = Ecma335Assembly::parse(&bcl_bytes).expect("parse System.Runtime.dll");
    let hiding = Ecma335Assembly::parse(&corpus_bytes).expect("parse MemberHiding.dll");
    let env = AssemblyEnv::from_views(&[bcl, hiding]).expect("build AssemblyEnv");

    let parsed = parse(&src);
    assert!(
        parsed.errors.is_empty(),
        "the probe file must parse cleanly: {:?}\n{src}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);

    let starts = line_starts(&src);
    let mut ours: HashMap<usize, String> = HashMap::new();
    for (range, res) in inferred.member_resolutions() {
        let Resolution::Member { parent, .. } = res else {
            continue;
        };
        let line = line_at(&starts, usize::from(range.start()));
        ours.insert(line, env.entity(*parent).name.clone());
    }

    let path = temp_fs_file("extension_shadow", &src);
    let uses_json = invoke_fcs_dump_with_refs("uses", &path, &[dll]);
    let types_json = invoke_fcs_dump_with_refs("types", &path, &[dll]);
    let _ = std::fs::remove_file(&path);
    let (_, type_errors) = parse_fcs_types_with_errors(&types_json, &src);
    let error_lines: HashSet<usize> = type_errors.iter().map(|e| e.line as usize).collect();

    let mut theirs: HashMap<usize, String> = HashMap::new();
    for use_ in parse_fcs_uses(&uses_json, &src) {
        // The `Ext` module's own declarations are definitions, not reads.
        if use_.name != PROBE || use_.is_from_definition || use_.is_constructor {
            continue;
        }
        if !use_
            .kind
            .as_deref()
            .is_some_and(|k| DATA_MEMBER_KINDS.contains(&k))
        {
            continue;
        }
        theirs.insert(line_at(&starts, use_.end), declaring_name(&use_));
    }

    // The premise, checked rather than assumed, and checked **per receiver**.
    // Every gate below survives an augmentation FCS silently rejected: the
    // intrinsic probes still agree with the intrinsic, the no-intrinsic controls
    // are skipped because *we* deferred, and the ratchet only reads our own
    // answers. A global "some extension exists" check is not enough either — FCS
    // rejecting exactly the *contested* augmentations (the ones whose name
    // collides with an intrinsic, which is the interesting case) leaves the
    // controls standing and the sweep proving nothing about the cells it commits
    // on. So each augmentation must be one FCS accepted: a definition of `P`
    // declared in this file's module, on a line it reported no error for.
    let defined_extensions: HashSet<usize> = parse_fcs_uses(&uses_json, &src)
        .iter()
        .filter(|use_| use_.name == PROBE && use_.is_from_definition)
        .filter(|use_| declaring_name(use_) == EXT_MODULE)
        .map(|use_| line_at(&starts, use_.start))
        .collect();
    for aug in &augmentations {
        assert!(
            !error_lines.contains(&aug.header_line) && !error_lines.contains(&aug.member_line),
            "FCS rejected the {} on `{}` — this sweep would then compare the intrinsic \
             against nothing\n{src}",
            kind.tag(),
            aug.receiver_ty,
        );
        assert!(
            defined_extensions.contains(&aug.member_line),
            "FCS reports no definition of `{PROBE}` in `{EXT_MODULE}` at line {} — the {} \
             on `{}` is not an extension member it accepted\n{src}",
            aug.member_line,
            kind.tag(),
            aug.receiver_ty,
        );
    }
    // And at least one probe must *bind* an extension, so the members are not
    // merely accepted but reachable from a use site.
    assert!(
        probes
            .iter()
            .filter(|probe| !error_lines.contains(&probe.line))
            .any(|probe| theirs.get(&probe.line).is_some_and(|d| d == EXT_MODULE)),
        "no probe binds an {} — the extension members are accepted but not in scope \
         at any access\n{src}",
        kind.tag(),
    );

    let answers = probes
        .iter()
        .map(|probe| {
            let mine = match ours.get(&probe.line) {
                Some(declaring) => Answer::Declaring(declaring.clone()),
                None => Answer::None,
            };
            let theirs = match theirs.get(&probe.line) {
                Some(declaring) => Answer::Declaring(declaring.clone()),
                None => Answer::None,
            };
            (mine, theirs)
        })
        .collect();
    (probes, answers, error_lines)
}

/// The claim the missing gate rests on: with an extension member of the name in
/// scope, whatever we commit is still what FCS binds.
#[test]
fn a_commit_beside_an_in_scope_extension_is_what_fcs_binds() {
    for kind in [ExtKind::Property, ExtKind::Method] {
        let (probes, answers, error_lines) = run(kind);
        let mut wrong: Vec<String> = Vec::new();
        for (probe, (ours, theirs)) in probes.iter().zip(&answers) {
            let Answer::Declaring(ours) = ours else {
                continue;
            };
            let complaint = if error_lines.contains(&probe.line) {
                Some("FCS reported an error on the line".to_string())
            } else {
                match theirs {
                    Answer::None => Some("FCS bound no data member there".to_string()),
                    Answer::Declaring(fcs) if fcs != ours => {
                        Some(format!("FCS bound it on `{fcs}`"))
                    }
                    Answer::Declaring(_) => None,
                }
            };
            if let Some(complaint) = complaint {
                wrong.push(format!(
                    "  line {:>3} [{} + {}]\n    {}\n    we answered `{ours}.{PROBE}`; {complaint}",
                    probe.line,
                    probe.label,
                    kind.tag(),
                    probe.text,
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} access(es) beside an {} name a declaration FCS did not bind:\n{}",
            wrong.len(),
            kind.tag(),
            wrong.join("\n"),
        );
    }
}

/// The two-sided ratchet on [`SHADOWED_CELLS`]: the property above is satisfied
/// by an engine that defers everything, so which cells still answer is pinned.
#[test]
fn the_cells_that_answer_beside_an_extension_are_the_recorded_ones() {
    for (kind, recorded) in SHADOWED_CELLS {
        let (probes, answers, _) = run(kind);
        let answered: BTreeSet<&str> = probes
            .iter()
            .zip(&answers)
            .filter(|(_, (ours, _))| matches!(ours, Answer::Declaring(_)))
            .map(|(probe, _)| probe.label.as_str())
            .collect();
        let recorded: BTreeSet<&str> = recorded.iter().copied().collect();
        let started: Vec<&&str> = answered.difference(&recorded).collect();
        let stopped: Vec<&&str> = recorded.difference(&answered).collect();
        assert!(
            started.is_empty() && stopped.is_empty(),
            "the cells answering beside an {} moved.\n  newly answered: {started:#?}\n  \
             no longer answered: {stopped:#?}\n\
             Update SHADOWED_CELLS once every move is understood.",
            kind.tag(),
        );
    }
}

/// The per-probe table. A measurement, not a gate.
///
/// ```text
/// nix develop -c cargo test -p borzoi-sema --test all \
///   extension_shadow_diff::shadow_report -- --ignored --nocapture
/// ```
#[test]
#[ignore = "report generator"]
fn shadow_report() {
    for kind in [ExtKind::Property, ExtKind::Method] {
        println!("\n== {} ==", kind.tag());
        let (probes, answers, error_lines) = run(kind);
        for (probe, (ours, theirs)) in probes.iter().zip(&answers) {
            let render = |a: &Answer| match a {
                Answer::Declaring(d) => format!("{d}.{PROBE}"),
                Answer::None => "—".to_string(),
            };
            println!(
                "{:<14} | {:<14} | {:<8} | {} (receiver {})",
                render(ours),
                render(theirs),
                if error_lines.contains(&probe.line) {
                    "errored"
                } else {
                    "clean"
                },
                probe.label,
                probe.receiver_ty,
            );
        }
    }
}
