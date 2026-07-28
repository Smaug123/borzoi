//! The **member-hiding differential**: the data-member wake against FCS over an
//! adversarial universe of C# name hiding
//! ([`common::member_hiding_corpus`](crate::common::member_hiding_corpus)).
//!
//! `InferredFile::member_resolutions` is the table the LSP serves
//! go-to-definition and hover from when the resolver can only defer, so an entry
//! naming the wrong *declaration* is a wrong answer with a source location
//! attached. What decides that declaration is C#'s hiding rule reaching an F#
//! member access — a question about the language, currently answered by prose
//! and a handful of `System.String` examples, and one the pinned project corpus
//! cannot help with: real F# does not hide members adversarially, and it commits
//! nothing at all on this surface.
//!
//! # The property
//!
//! Per access site, both directions of what the LSP would serve:
//!
//! 1. **We commit ⇒ FCS bound a member of the same name on the same declaring
//!    type.** The comparison currency is the *declaring entity's compiled name*
//!    (every corpus type has a distinct one), not the rendered full name, which
//!    arrives decorated by `NicePrint`. Committing where FCS bound nothing at
//!    all is a failure too — that is the shape of a wrong answer where the user
//!    would see an error.
//! 2. **Every type we publish is confirmed by the `types` oracle at its exact
//!    range** — the D5 soundness net the other inference differentials use. The
//!    base and derived members are declared at *different* types (`int` /
//!    `string`), so reaching the wrong level fails here as well as in (1): two
//!    independent witnesses of the same mistake.
//!
//! Deferring is always allowed, so both directions are satisfied vacuously by an
//! engine that answers nothing. Two things stop that. The **commit floor**
//! ([`MIN_COMMITS`]) is the tripwire against a run that compares nothing — the
//! trap `member_commits_compared` fell into on the project corpus, where a clean
//! run compared zero answers. And [`ANSWERED_CELLS`] is a **two-sided ratchet**
//! on which cells answer at all, so a cell that silently stops being reached is a
//! review conversation rather than a passing suite.
//!
//! [`verdict_report`] (`#[ignore]`d — a measurement, not a gate) prints the
//! per-cell table: what we answered and what FCS answered, for every cell.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::common::member_hiding_corpus::{Corpus, Site, corpus};
use crate::common::{
    NormalisedUse, ensure_member_hiding_corpus_built, ensure_system_runtime_dll,
    invoke_fcs_dump_with_refs, parse_fcs_types_with_errors, parse_fcs_uses, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, InferredFile, ProjectItems, Resolution, infer_file, resolve_file};

/// The `SymbolKind`s FCS reports for a **data member** — the only answers that
/// can confirm one of ours. A closed set on purpose: a kind outside it (a nested
/// `type` of the probed name, say) is a different subject, and grading against it
/// would let FCS ratify an answer about something else. A kind we have not
/// classified fails the cell rather than passing it.
const DATA_MEMBER_KINDS: [&str; 2] = ["member", "field"];

/// Whether FCS's `SymbolKind` names a data member.
fn is_data_member(kind: Option<&str>) -> bool {
    kind.is_some_and(|k| DATA_MEMBER_KINDS.contains(&k))
}

/// The cells the wake **answers**, as a two-sided ratchet.
///
/// The soundness gates below can only fail on a *wrong* answer, so an engine
/// that quietly stops reaching a cell passes them (a lost commit is availability,
/// not correctness). This table makes that a review conversation instead: a cell
/// that starts answering must be checked against FCS, and one that stops must be
/// explained. Deliberately the answered set and not the declined one — it is the
/// smaller list, and it is what the LSP serves.
const ANSWERED_CELLS: &[&str] = &[
    "grid base=none derived=prop",
    "grid base=none derived=field",
    "grid base=prop derived=none",
    "grid base=prop derived=prop",
    "grid base=prop derived=field",
    "grid base=prop derived=internalprop",
    "grid base=prop derived=privateprop",
    "grid base=prop derived=protectedprop",
    "grid base=prop derived=nestedtype",
    "grid base=field derived=none",
    "grid base=field derived=prop",
    "grid base=field derived=field",
    "grid base=field derived=internalprop",
    "grid base=field derived=privateprop",
    "grid base=field derived=protectedprop",
    "grid base=field derived=nestedtype",
    "grid base=staticprop derived=prop",
    "grid base=staticprop derived=field",
    "grid base=method derived=prop",
    "grid base=method derived=field",
    "grid base=methodgroup derived=prop",
    "grid base=methodgroup derived=field",
    "grid base=event derived=prop",
    "grid base=event derived=field",
    "interface own-declaration",
    "interface own-declaration hiding an inherited one",
    "interface inherits from one declaring level",
    "interface inherits through a silent intermediate",
    "generic base, member declared on the receiver",
    "three-level chain, nearest declaring level is the middle",
    "three-level chain, only the top declares",
    "member type the Ty bridge declines",
    "implicit interface implementation",
    "struct receiver",
    "cross-assembly base, member inherited",
    "cross-assembly base, member hidden by the receiver",
];

/// The floor on committed member resolutions across the corpus. Deliberately well
/// under the answered count ([`ANSWERED_CELLS`] holds the exact set): this is the
/// "the differential still compares something" tripwire, and the ratchet is what
/// tracks the engine's reach.
const MIN_COMMITS: usize = 20;

/// What we answered at one site.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ours {
    /// A member resolution: the declaring entity's compiled name and the
    /// member's display name.
    Member { declaring: String, member: String },
    /// No member resolution recorded — the access deferred.
    Deferred,
}

/// What FCS answered at one site.
///
/// The *kind* rides along because a name is not enough to identify the subject:
/// a nested type named `P` and a member named `P` both arrive as a use of `P`,
/// and only `SymbolKind` says which was bound. An answer we would grade against
/// must be a member.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Theirs {
    /// A symbol use of the probed name, declared on this entity (the last
    /// segment of the use's structural declaring path).
    Bound {
        declaring: String,
        kind: Option<String>,
    },
    /// FCS reported no use of the probed name on the line — it bound nothing.
    Nothing,
}

impl Theirs {
    /// How the cell reads in the report.
    fn render(&self, probe: &str) -> String {
        match self {
            Theirs::Bound { declaring, kind } if is_data_member(kind.as_deref()) => {
                format!("{declaring}.{probe}")
            }
            Theirs::Bound { declaring, kind } => format!(
                "{declaring}.{probe} [{}]",
                kind.as_deref().unwrap_or("kind unreported")
            ),
            Theirs::Nothing => "—".to_string(),
        }
    }
}

/// One loaded run of the corpus: both sides' answers, indexed by site line.
struct Run {
    corpus: Corpus,
    inferred: InferredFile,
    line_starts: Vec<usize>,
    ours: HashMap<usize, Ours>,
    theirs: HashMap<usize, Theirs>,
    fcs_types: HashMap<(usize, usize), String>,
    /// The 1-based lines FCS reported an error on. Its typed tree omits the
    /// enclosing expression there, and a symbol use it still reports is not
    /// evidence the read is legal.
    error_lines: HashSet<usize>,
    type_errors: Vec<crate::common::FcsCheckError>,
}

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

/// Build the corpus, compile it, and run both sides over it.
fn run() -> Run {
    let corpus = corpus();
    let dll = ensure_member_hiding_corpus_built(&corpus.csharp);
    let system_runtime = ensure_system_runtime_dll();

    // Our env: the real BCL plus the corpus assembly. No FSharp.Core — its
    // assembly-level `[<AutoOpen>]`s are an extension *surface*, and while the
    // data-member wake does not consult the extension gate, keeping the env to
    // what the corpus needs keeps a failing cell attributable to the corpus.
    let bcl_bytes = std::fs::read(&system_runtime).expect("read System.Runtime.dll");
    let corpus_bytes = std::fs::read(dll).expect("read MemberHiding.dll");
    let bcl = Ecma335Assembly::parse(&bcl_bytes).expect("parse System.Runtime.dll");
    let hiding = Ecma335Assembly::parse(&corpus_bytes).expect("parse MemberHiding.dll");
    let env = AssemblyEnv::from_views(&[bcl, hiding]).expect("build AssemblyEnv");

    let parsed = parse(&corpus.fsharp);
    assert!(
        parsed.errors.is_empty(),
        "the generated corpus must parse cleanly: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);

    let starts = line_starts(&corpus.fsharp);
    let mut ours: HashMap<usize, Ours> = HashMap::new();
    for (range, res) in inferred.member_resolutions() {
        let Resolution::Member { parent, idx } = res else {
            panic!("member_resolutions holds only Resolution::Member, got {res:?}");
        };
        let line = line_at(&starts, usize::from(range.start()));
        let answer = Ours::Member {
            declaring: env.entity(*parent).name.clone(),
            member: env.member_display_name(*parent, *idx).to_string(),
        };
        if let Some(previous) = ours.insert(line, answer.clone()) {
            panic!("two member resolutions on line {line}: {previous:?} and {answer:?}");
        }
    }

    // FCS's side: the same file, the same reference.
    let path = temp_fs_file("member_hiding", &corpus.fsharp);
    let uses_json = invoke_fcs_dump_with_refs("uses", &path, &[dll]);
    let types_json = invoke_fcs_dump_with_refs("types", &path, &[dll]);
    let _ = std::fs::remove_file(&path);
    let (fcs_types, type_errors) = parse_fcs_types_with_errors(&types_json, &corpus.fsharp);
    let error_lines: HashSet<usize> = type_errors.iter().map(|e| e.line as usize).collect();

    // Every site's probe name, so a use of one is recognisable without knowing
    // which cell's line it landed on.
    let probes: HashSet<&str> = corpus.sites.iter().map(|s| s.probe.as_str()).collect();
    let mut theirs: HashMap<usize, Theirs> = HashMap::new();
    for use_ in parse_fcs_uses(&uses_json, &corpus.fsharp) {
        if !probes.contains(use_.name.as_str()) || use_.is_from_definition {
            continue;
        }
        // A **constructor** record names the type it constructs, not a member of
        // the receiver — the corpus's nested-type cells produce one at the very
        // range a member access would occupy. Grading against it would let FCS
        // ratify an answer about a different subject.
        if use_.is_constructor {
            continue;
        }
        // FCS reports a member access over the span of the *whole* access
        // (`v3.P`) named by its final segment, while we key the member-name
        // token. The two share their end, and every access sits alone on its
        // line, so the line of the end offset is the site key.
        let line = line_at(&starts, use_.end);
        let answer = Theirs::Bound {
            declaring: declaring_name(&use_),
            kind: use_.kind.clone(),
        };
        if let Some(previous) = theirs.insert(line, answer.clone()) {
            panic!(
                "two FCS uses of `{}` on line {line}: {previous:?} and {answer:?}",
                use_.name
            );
        }
    }

    Run {
        corpus,
        inferred,
        line_starts: starts,
        ours,
        theirs,
        fcs_types,
        error_lines,
        type_errors,
    }
}

/// The compiled name of the entity FCS says declares a use's symbol — the last
/// segment of its structural declaring path. A use with no declaring path (FCS
/// could not produce one) renders as a marker that can never compare equal, so
/// an unexpected shape fails loudly rather than silently matching.
fn declaring_name(use_: &NormalisedUse) -> String {
    match use_.declaring_path.as_ref().and_then(|path| path.last()) {
        Some((name, _)) => name.clone(),
        None => format!("<no-declaring-path:{:?}>", use_.full_name),
    }
}

impl Run {
    fn ours(&self, site: &Site) -> &Ours {
        self.ours.get(&site.line).unwrap_or(&Ours::Deferred)
    }

    fn theirs(&self, site: &Site) -> &Theirs {
        self.theirs.get(&site.line).unwrap_or(&Theirs::Nothing)
    }

    /// Whether we recorded a member resolution on `line`.
    fn committed_on(&self, line: usize) -> bool {
        matches!(self.ours.get(&line), Some(Ours::Member { .. }))
    }

    /// The type we published at `range`, if any.
    fn our_type(&self, range: (usize, usize)) -> Option<String> {
        self.inferred
            .types()
            .iter()
            .find(|(r, _)| (usize::from(r.start()), usize::from(r.end())) == range)
            .map(|(_, ty)| ty.render())
    }

    /// FCS's first error on `line`, rendered for a failure message.
    fn error_on(&self, line: usize) -> String {
        self.type_errors
            .iter()
            .find(|e| e.line as usize == line)
            .map(|e| format!("FS{:04}: {}", e.code, e.message))
            .unwrap_or_else(|| "<error line, message unrecorded>".to_string())
    }
}

/// Direction 1: wherever we publish a member resolution, FCS bound a member of
/// the same name on the same declaring type.
#[test]
fn a_committed_member_names_the_declaration_fcs_bound() {
    let run = run();
    let mut commits = 0usize;
    let mut wrong: Vec<String> = Vec::new();

    for site in &run.corpus.sites {
        let Ours::Member { declaring, member } = run.ours(site) else {
            continue;
        };
        commits += 1;
        let complaint = match run.theirs(site) {
            Theirs::Nothing => Some("FCS bound nothing there".to_string()),
            // FCS reports a symbol use even where reading it is illegal — a
            // `protected` property, a `static` reached through a value receiver.
            // Grading against that alone would let an *error* ratify a commit, so
            // the line must also have checked cleanly.
            _ if run.error_lines.contains(&site.line) => Some(format!(
                "FCS reported an error on the line: {}",
                run.error_on(site.line)
            )),
            // A use of the probed name that is not a *member* — a nested type of
            // the name, say — is not the thing we answered, however its declaring
            // path reads.
            Theirs::Bound { kind, .. } if !is_data_member(kind.as_deref()) => Some(format!(
                "FCS bound a {} of that name",
                kind.as_deref().unwrap_or("symbol of unreported kind")
            )),
            Theirs::Bound { declaring: fcs, .. } if fcs != declaring => {
                Some(format!("FCS bound it on `{fcs}`"))
            }
            _ if *member != site.probe => Some(format!("we named the member `{member}`")),
            Theirs::Bound { .. } => None,
        };
        if let Some(complaint) = complaint {
            wrong.push(format!(
                "  line {:>4} [{}]\n    {}\n    we answered `{declaring}.{member}`; {complaint}",
                site.line, site.label, site.text,
            ));
        }
    }

    assert!(
        wrong.is_empty(),
        "{} member resolution(s) name a declaration FCS did not bind:\n{}",
        wrong.len(),
        wrong.join("\n"),
    );
    assert!(
        commits >= MIN_COMMITS,
        "the corpus committed {commits} member resolutions, below the {MIN_COMMITS} floor — \
         a run that defers everything satisfies the agreement property vacuously",
    );
}

/// Direction 2: the D5 soundness net — every type we publish is confirmed by the
/// `types` oracle at its exact range. The two levels declare `P` at different
/// types, so reaching the wrong one fails here independently of the declaring
/// entity comparison.
///
/// Two thirds of this corpus is deliberately ill-typed (reading a method group,
/// an event, a write-only property), and FCS's typed tree omits the *whole*
/// binding on a line it could not check — receiver reference included. So a
/// missing node counts as a disagreement only on a line FCS checked cleanly;
/// elsewhere it is skipped, and the skips are counted rather than passed over in
/// silence.
#[test]
fn every_published_type_is_confirmed_by_the_types_oracle() {
    let run = run();
    let mut checked = 0usize;
    let mut skipped = 0usize;
    let mut wrong: Vec<String> = Vec::new();

    for (range, ty) in run.inferred.types() {
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        let line = line_at(&run.line_starts, start);
        match run.fcs_types.get(&(start, end)) {
            Some(fcs) if *fcs == ty.render() => checked += 1,
            Some(fcs) => wrong.push(format!(
                "  line {line:>4}: we typed {start}..{end} as `{}`, FCS says `{fcs}`",
                ty.render()
            )),
            // FCS's typed tree omits an expression it could not check, and
            // reshapes ones it elaborates — a method group read as a value becomes
            // a lambda, a struct receiver is taken by address — so a *missing*
            // node is not disagreement. The access expression of every committed
            // cell is separately required to be present, below, so this cannot
            // quietly become "we compared nothing".
            None => skipped += 1,
        }
    }

    assert!(
        wrong.is_empty(),
        "{} published type(s) the oracle does not confirm:\n{}",
        wrong.len(),
        wrong.join("\n"),
    );
    // The access expression of every committed cell: the node this differential
    // is actually about. FCS must have typed it, and agreed. (A cell can commit a
    // member whose *type* the bridge declines — then we publish no type at the
    // access and there is nothing to confirm.)
    let mut unconfirmed_accesses: Vec<String> = Vec::new();
    let mut accesses = 0usize;
    for site in &run.corpus.sites {
        if !run.committed_on(site.line) {
            continue;
        }
        let Some(ours) = run.our_type(site.access) else {
            continue;
        };
        match run.fcs_types.get(&site.access) {
            Some(fcs) if *fcs == ours => accesses += 1,
            Some(fcs) => unconfirmed_accesses.push(format!(
                "  line {:>4} [{}]: we typed the access `{}`, FCS says `{fcs}`",
                site.line, site.label, ours
            )),
            None => unconfirmed_accesses.push(format!(
                "  line {:>4} [{}]: we typed the access `{}`, FCS has no node there",
                site.line, site.label, ours
            )),
        }
    }
    assert!(
        unconfirmed_accesses.is_empty(),
        "{} committed access(es) the oracle does not confirm:\n{}",
        unconfirmed_accesses.len(),
        unconfirmed_accesses.join("\n"),
    );

    assert!(
        checked >= MIN_COMMITS,
        "only {checked} types confirmed ({skipped} nodes FCS reshaped or dropped), \
         below the {MIN_COMMITS} floor",
    );
    assert!(
        accesses >= MIN_COMMITS,
        "only {accesses} committed accesses confirmed, below the {MIN_COMMITS} floor",
    );
}

/// The per-cell table: what each side answered, for every cell. A measurement,
/// not a gate — run it to see where the wake declines and what FCS does there.
///
/// ```text
/// nix develop -c cargo test -p borzoi-sema --test all \
///   member_hiding_diff::verdict_report -- --ignored --nocapture
/// ```
#[test]
#[ignore = "report generator"]
fn verdict_report() {
    let run = run();
    let mut agreed = 0usize;
    let mut deferred = 0usize;
    println!(
        "\n== member-hiding corpus: {} cells ==",
        run.corpus.sites.len()
    );
    for site in &run.corpus.sites {
        let ours = match run.ours(site) {
            Ours::Member { declaring, member } => {
                agreed += 1;
                format!("{declaring}.{member}")
            }
            Ours::Deferred => {
                deferred += 1;
                "—".to_string()
            }
        };
        let theirs = run.theirs(site).render(&site.probe);
        println!("{ours:<24} | {theirs:<30} | {}", site.label);
    }
    println!("\ncommitted {agreed}, deferred {deferred}");
}

/// The two-sided ratchet on [`ANSWERED_CELLS`]: exactly these cells commit a
/// member resolution, no more and no fewer.
#[test]
fn the_cells_the_wake_answers_are_the_recorded_ones() {
    let run = run();
    let answered: BTreeSet<&str> = run
        .corpus
        .sites
        .iter()
        .filter(|site| run.committed_on(site.line))
        .map(|site| site.label.as_str())
        .collect();
    let recorded: BTreeSet<&str> = ANSWERED_CELLS.iter().copied().collect();

    let started: Vec<&&str> = answered.difference(&recorded).collect();
    let stopped: Vec<&&str> = recorded.difference(&answered).collect();
    assert!(
        started.is_empty() && stopped.is_empty(),
        "the answered cells moved.\n  newly answered (check each against FCS): {started:#?}\n  \
         no longer answered (availability lost): {stopped:#?}\n\
         Update ANSWERED_CELLS once every move is understood.",
    );
}
