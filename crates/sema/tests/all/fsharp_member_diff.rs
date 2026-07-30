//! The **F#-authored referenced assembly** member differential: the data-member
//! wake against FCS over an assembly an F# compiler emitted
//! ([`common::fsharp_member_corpus`](crate::common::fsharp_member_corpus)).
//!
//! [`member_hiding_diff`](crate::member_hiding_diff) covers the same wake over a
//! C# universe, which settles the hiding rule but not the *shapes*: every
//! `<ProjectReference>` in a real solution points at an F#-authored assembly,
//! and the metadata there carries forms C# never produces. The wake reads one
//! entity model for both, so a shape it mishandles here is a wrong
//! go-to-definition target on the most ordinary reference a project has, and
//! nothing else in the suite would say so — the pinned project corpus commits
//! nothing on this surface.
//!
//! # The property
//!
//! Per access site, the two directions the C# differential asserts, plus one it
//! has no need of:
//!
//! 1. **We commit ⇒ FCS bound a member of that name on the same declaring
//!    type.** The currency is the declaring entity's *compiled* name, since a
//!    rendered full name arrives decorated by `NicePrint`. Committing where FCS
//!    bound nothing is a failure too: that is a wrong answer with a source
//!    location attached.
//! 2. **Every type we publish is confirmed by the `types` oracle at its exact
//!    range** — the soundness net the other inference differentials use.
//! 3. **Every member FCS bound is present on the entity we project for its
//!    declaring type**, whether or not the wake reached it. Directions 1 and 2
//!    are about a *commit*, so a cell that declines satisfies both while the
//!    projection underneath it has silently lost the member; this one holds the
//!    projection to account independently of the wake.
//!
//! Deferring is always sound, so the first two directions are satisfied
//! vacuously by an engine that answers nothing. [`MIN_COMMITS`] is the floor against a run that
//! compares nothing, and [`ANSWERED_CELLS`] is a two-sided ratchet on *which*
//! cells answer, so a cell that silently stops being reached is a review
//! conversation rather than a passing suite.
//!
//! # What the shapes are
//!
//! Measured from the oracle before any of it was asserted, because three of the
//! obvious guesses were wrong. FCS reports a record field as `field` rather than
//! `member`; a union's `Tag` draws no use at all, so a cell reading it would
//! assert nothing while looking like coverage; and a `[<CompiledName>]` member is
//! reported under its **source** name while the metadata we read carries the
//! compiled one.
//!
//! # What it found
//!
//! The wake reaches record fields, a class property, either of those through an
//! abbreviation, and a union's case-test property (`IsOne`). It **declines** a
//! `[<CompiledName>]` member, directly and through an abbreviation: the metadata
//! carries `CompiledRenamed` and every F# source writes `Renamed`, so a lookup
//! keyed on the written name finds nothing. Nothing in the assembly records the
//! source name — the `[CompilationSourceName]` on the getter carries the
//! *getter's* own name, `get_Renamed` — so recovering it is a derivation rather
//! than a read, and [`ANSWERED_CELLS`] pins the decline until one is justified.
//!
//! The decline is never a wrong answer, so this is a silence rather than a
//! soundness bug — but it is silence on the most ordinary reference a solution
//! has. The ratchet is what turns closing it into a review conversation instead
//! of a quietly-changed test.
//!
//! The case-test property was the same kind of silence and is now closed: FCS
//! surfaces an `Is<Case>` per union case, and the projector was dropping every
//! union property because no attribute tells one from the `Tag` discriminant FCS
//! hides. `retain_published_union_properties` (in `borzoi-assembly`) reads the
//! host pickle's published member list — the same list FCS answers from — rather
//! than deciding from the metadata rows, which are identical whether or not the
//! member can be named.
//!
//! # The generic receiver, which no union work reaches
//!
//! The four FSharp.Core cells all decline, and **not** for any reason this
//! corpus is otherwise about. [`borzoi_sema::Ty::Named`] carries a dotted path
//! and no generic-argument list, so `int option` never grounds: the receiver
//! gets no published type at all, the access stays a `Ty::Var`, and the wake
//! never sees a head to look a member up on. `x.IsSome` is about as common as F#
//! member access gets, and it is silence — as is every member of `list`, `Map`,
//! `Dictionary<_,_>`, or any other instantiated receiver.
//!
//! So these cells are a tripwire on a gap far larger than the union rule. They
//! are pinned as declines exactly like the `[<CompiledName>]` cells, so whichever
//! change first makes a generic receiver ground arrives as a review conversation
//! about four now-answering cells.
//!
//! Direction 3 is what keeps them from being *only* a tripwire. Their decline is
//! upstream of everything else, so it would equally hide an env that had lost
//! FSharp.Core or a projection that had dropped the member — and both would leave
//! the cells looking exactly as they do now. Holding the projection to account
//! separately is also the only check the published-member rule gets against
//! FSharp.Core's **hand-written** unions: `list`'s cases are `op_Nil` and
//! `op_ColonColon`, so a rule reading case names instead of `tcaug.adhoc`
//! surfaces neither `IsEmpty` nor `Head`, and loses `option`'s `Value` besides.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::common::fsharp_member_corpus::{Corpus, Site, corpus};
use crate::common::{
    ensure_fsharp_core_dll, ensure_fsharp_member_corpus_built, ensure_system_runtime_dll,
    invoke_fcs_dump_with_refs, parse_fcs_types_with_errors, parse_fcs_uses, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, InferredFile, ProjectItems, Resolution, infer_file, resolve_file};

/// The cells that commit an answer, exactly. A two-sided ratchet: a cell that
/// starts answering and a cell that stops are both a diff here, so neither
/// passes unremarked.
const ANSWERED_CELLS: &[&str] = &[
    "class property (control) (FSharpMember.Klass.Plain)",
    "class property through an abbreviation (FSharpMember.Alias.Plain)",
    "record field (FSharpMember.Rec.Payload)",
    "record field, second (FSharpMember.Rec.Label)",
    "union case test property (FSharpMember.Union.IsOne)",
];

/// The floor on committed answers, against a run that compares nothing — the
/// trap `member_commits_compared` fell into on the project corpus, where a clean
/// run compared zero. Set below the answered count ([`ANSWERED_CELLS`] holds the
/// exact set) so it is a tripwire rather than a second copy of it.
const MIN_COMMITS: usize = 3;

/// The `SymbolKind`s that name a **data member** — the only subjects this wake
/// answers about. A record field arrives as `field` and a property as `member`;
/// anything else at the range (a union *case* beside its case-test property, a
/// constructor beside a type) is a different subject, and grading against it
/// would let FCS ratify an answer about something else.
const DATA_MEMBER_KINDS: [&str; 2] = ["member", "field"];

fn is_data_member(kind: Option<&str>) -> bool {
    kind.is_some_and(|k| DATA_MEMBER_KINDS.contains(&k))
}

/// What we answered at one site.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ours {
    Member { declaring: String, member: String },
    Deferred,
}

/// What FCS answered at one site. The kind rides along because a name alone does
/// not identify the subject.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Theirs {
    Bound {
        declaring: String,
        kind: Option<String>,
    },
    Nothing,
}

struct Run {
    corpus: Corpus,
    env: AssemblyEnv,
    inferred: InferredFile,
    ours: HashMap<usize, Ours>,
    theirs: HashMap<usize, Theirs>,
    fcs_types: HashMap<(usize, usize), String>,
    error_lines: HashSet<usize>,
}

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

fn line_at(starts: &[usize], at: usize) -> usize {
    match starts.binary_search(&at) {
        Ok(i) => i + 1,
        Err(i) => i,
    }
}

/// The last segment of a use's structural declaring path — the compiled name of
/// the entity that declares the member.
fn declaring_name(use_: &crate::common::NormalisedUse) -> String {
    match use_.declaring_path.as_ref().and_then(|path| path.last()) {
        Some((name, _)) => name.clone(),
        None => format!("<no-declaring-path:{:?}>", use_.full_name),
    }
}

fn run() -> Run {
    let corpus = corpus();
    let dll = ensure_fsharp_member_corpus_built();
    let system_runtime = ensure_system_runtime_dll();

    // The oracle compiles the probe as a script, so FCS resolves it against the
    // framework *and* FSharp.Core. Our env carries the same three: reading less
    // than the oracle would make an FSharp.Core cell decline for want of the
    // assembly rather than for anything the wake decided, and assert nothing.
    // `netstandard` rides along because FSharp.Core's pickle names its BCL
    // abbreviation targets through that CCU.
    let netstandard = system_runtime
        .parent()
        .expect("System.Runtime.dll sits in a ref dir")
        .join("netstandard.dll");
    let bcl_bytes = std::fs::read(&system_runtime).expect("read System.Runtime.dll");
    let netstandard_bytes = std::fs::read(&netstandard).expect("read netstandard.dll");
    let core_bytes = std::fs::read(ensure_fsharp_core_dll()).expect("read FSharp.Core.dll");
    let fixture_bytes = std::fs::read(dll).expect("read the F# fixture dll");
    let bcl = Ecma335Assembly::parse(&bcl_bytes).expect("parse System.Runtime.dll");
    let netstandard = Ecma335Assembly::parse(&netstandard_bytes).expect("parse netstandard.dll");
    let core = Ecma335Assembly::parse(&core_bytes).expect("parse FSharp.Core.dll");
    let fixture = Ecma335Assembly::parse(&fixture_bytes).expect("parse the F# fixture dll");
    let env =
        AssemblyEnv::from_views(&[bcl, netstandard, core, fixture]).expect("build AssemblyEnv");

    let parsed = parse(&corpus.probe);
    assert!(
        parsed.errors.is_empty(),
        "the generated probe must parse cleanly: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);

    let starts = line_starts(&corpus.probe);
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

    let path = temp_fs_file("fsharp_member", &corpus.probe);
    let uses_json = invoke_fcs_dump_with_refs("uses", &path, &[dll]);
    let types_json = invoke_fcs_dump_with_refs("types", &path, &[dll]);
    let _ = std::fs::remove_file(&path);
    let (fcs_types, type_errors) = parse_fcs_types_with_errors(&types_json, &corpus.probe);
    let error_lines: HashSet<usize> = type_errors.iter().map(|e| e.line as usize).collect();

    // Only the probed member names. A union case-test cell's range also carries
    // a record for the *case* it tests (`One` beside `IsOne`), which names a
    // different subject; filtering by the probed name drops it, the way the C#
    // differential drops a constructor record at a nested-type access.
    let probes: HashSet<&str> = corpus.sites.iter().map(|s| s.member.as_str()).collect();
    let mut theirs: HashMap<usize, Theirs> = HashMap::new();
    for use_ in parse_fcs_uses(&uses_json, &corpus.probe) {
        if !probes.contains(use_.name.as_str()) || use_.is_from_definition || use_.is_constructor {
            continue;
        }
        let line = line_at(&starts, use_.end);
        let answer = Theirs::Bound {
            declaring: declaring_name(&use_),
            kind: use_.kind.clone(),
        };
        if let Some(previous) = theirs.insert(line, answer.clone()) {
            panic!("two oracle answers on line {line}: {previous:?} and {answer:?}");
        }
    }

    Run {
        corpus,
        env,
        inferred,
        ours,
        theirs,
        fcs_types,
        error_lines,
    }
}

impl Run {
    /// The type we published at `range`, if any.
    fn our_type(&self, range: (usize, usize)) -> Option<String> {
        self.inferred
            .types()
            .iter()
            .find(|(r, _)| (usize::from(r.start()), usize::from(r.end())) == range)
            .map(|(_, ty)| ty.render())
    }
}

fn site_ours<'a>(run: &'a Run, site: &Site) -> &'a Ours {
    run.ours.get(&site.line).unwrap_or(&Ours::Deferred)
}

fn site_theirs<'a>(run: &'a Run, site: &Site) -> &'a Theirs {
    run.theirs.get(&site.line).unwrap_or(&Theirs::Nothing)
}

/// Direction 1: a commit of ours names a member FCS bound on the same type.
#[test]
fn every_committed_member_names_what_fcs_bound() {
    let run = run();
    let mut commits = 0usize;
    for site in &run.corpus.sites {
        let Ours::Member { declaring, member } = site_ours(&run, site) else {
            continue;
        };
        commits += 1;
        match site_theirs(&run, site) {
            Theirs::Bound {
                declaring: theirs,
                kind,
            } => {
                // FCS still emits a symbol use for an access it errored on, and
                // a use of a non-data-member subject at the same range names
                // something else entirely. Neither may ratify a commit.
                assert!(
                    !run.error_lines.contains(&site.line),
                    "cell {}: we committed {declaring}.{member} on a line FCS reported an \
                     error for, so its use is no evidence the read is legal",
                    site.label
                );
                assert!(
                    is_data_member(kind.as_deref()),
                    "cell {}: we committed {declaring}.{member} but FCS bound a \
                     {} there, which is a different subject",
                    site.label,
                    kind.as_deref().unwrap_or("kind-less symbol")
                );
                // Both halves. Two cells here read *different* members off one
                // type (`Rec.Payload` and `Rec.Label`), so a declaring-type
                // comparison alone ratifies answering either for the other —
                // which is exactly the wrong go-to-definition target this gate
                // exists to reject.
                assert_eq!(
                    (declaring.as_str(), member.as_str()),
                    (theirs.as_str(), site.member.as_str()),
                    "cell {}: we committed {declaring}.{member}, FCS bound {theirs}.{}",
                    site.label,
                    site.member
                );
            }
            Theirs::Nothing => panic!(
                "cell {}: we committed {declaring}.{member} where FCS bound nothing — a wrong \
                 answer with a source location attached",
                site.label
            ),
        }
    }
    assert!(
        commits >= MIN_COMMITS,
        "the corpus committed {commits} member resolutions, below the {MIN_COMMITS} floor: \
         the differential is comparing nothing"
    );
}

/// Direction 2: the `types` oracle confirms what we publish, at the exact range.
#[test]
fn every_committed_access_is_confirmed_by_the_types_oracle() {
    let run = run();
    // Everything we publish, not only the accesses: an inference regression on a
    // receiver or a binding is a wrong type served by hover just as much as one
    // on the access itself. A *missing* FCS node is not disagreement — its typed
    // tree omits what it could not check and reshapes what it elaborates — but a
    // node that disagrees is.
    let mut wrong: Vec<String> = Vec::new();
    for (range, ty) in run.inferred.types() {
        let key = (usize::from(range.start()), usize::from(range.end()));
        if let Some(fcs) = run.fcs_types.get(&key)
            && *fcs != ty.render()
        {
            wrong.push(format!(
                "  {key:?}: we typed `{}`, FCS says `{fcs}`",
                ty.render()
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} published type(s) the oracle does not confirm:\n{}",
        wrong.len(),
        wrong.join("\n")
    );

    let mut checked = 0usize;
    for site in &run.corpus.sites {
        if !matches!(site_ours(&run, site), Ours::Member { .. }) {
            continue;
        }
        // An erroring line's typed tree omits the enclosing expression, so a
        // type reported there is not evidence the read is legal.
        if run.error_lines.contains(&site.line) {
            continue;
        }
        // The *value*, not merely a node at the range: a run where inference
        // regressed from `int` to `string` still emits a node, so presence
        // confirms nothing about what we published.
        let Some(ours) = run.our_type(site.access) else {
            // A cell can commit a member whose type the bridge declines; then we
            // publish nothing at the access and there is nothing to confirm.
            continue;
        };
        match run.fcs_types.get(&site.access) {
            Some(fcs) if *fcs == ours => checked += 1,
            Some(fcs) => panic!(
                "cell {}: we typed the access `{ours}`, FCS says `{fcs}`",
                site.label
            ),
            None => panic!(
                "cell {}: we typed the access `{ours}` but the types oracle has no node \
                 there, so nothing confirms it",
                site.label
            ),
        }
    }
    assert!(
        checked >= MIN_COMMITS,
        "only {checked} committed accesses were confirmed, below the {MIN_COMMITS} floor"
    );
}

/// The **oracle-side** invariant, which the ratchet cannot express: every cell
/// is still a legal access that FCS binds to a data member of the expected type.
///
/// Without it a dimension can vanish silently. Our answer at the three deferred
/// cells is `Deferred`, and `Deferred` is also what we would report if the
/// fixture stopped declaring `Renamed` at all — so [`ANSWERED_CELLS`] would not
/// move, every soundness gate would still pass, and the corpus would have
/// quietly lost the shape it exists to cover.
#[test]
fn every_cell_is_still_an_access_fcs_binds() {
    let run = run();
    for site in &run.corpus.sites {
        assert!(
            !run.error_lines.contains(&site.line),
            "cell {}: FCS reports an error on this line, so the probe is no longer a legal \
             access and the cell tests nothing",
            site.label
        );
        match site_theirs(&run, site) {
            Theirs::Bound { declaring, kind } => {
                assert_eq!(
                    declaring, &site.declaring_ty,
                    "cell {}: FCS now binds the member on {declaring}, not {}",
                    site.label, site.declaring_ty
                );
                assert!(
                    is_data_member(kind.as_deref()),
                    "cell {}: FCS binds a {} here rather than a data member",
                    site.label,
                    kind.as_deref().unwrap_or("kind-less symbol")
                );
            }
            Theirs::Nothing => panic!(
                "cell {}: FCS binds nothing at this access — the fixture or the probe stopped \
                 declaring what the cell is about",
                site.label
            ),
        }
    }
}

/// The two-sided ratchet: exactly [`ANSWERED_CELLS`] commit.
#[test]
fn exactly_the_recorded_cells_answer() {
    let run = run();
    let answered: BTreeSet<&str> = run
        .corpus
        .sites
        .iter()
        .filter(|s| matches!(site_ours(&run, s), Ours::Member { .. }))
        .map(|s| s.label.as_str())
        .collect();
    let recorded: BTreeSet<&str> = ANSWERED_CELLS.iter().copied().collect();
    assert_eq!(
        answered, recorded,
        "the set of cells that commit an answer moved. A cell that started \
         answering may be a gain and one that stopped may be a loss, but both are \
         changes to what the LSP serves on an F#-authored reference. Update \
         ANSWERED_CELLS once every move is understood."
    );
}

/// The **projection** half, which the wake cannot reach: every member FCS binds
/// is present on the entity our env projects for its declaring type.
///
/// The four FSharp.Core cells decline because their receiver is generic, and
/// that decline is upstream of everything else — so it would equally hide an env
/// that had lost FSharp.Core entirely, or a projection that had dropped the
/// member. Both would leave those cells looking exactly as they do now, and
/// every other gate here would still pass.
///
/// This is also the only place the published-member rule is checked against
/// FSharp.Core's **hand-written** unions rather than fsc-generated ones, and the
/// two `list` cells are what make that bite: `list`'s cases are `op_Nil` and
/// `op_ColonColon`, so nothing about `IsEmpty` or `Head` is derivable from a case
/// name. A rule that read the case names instead of `tcaug.adhoc` surfaces
/// neither.
#[test]
fn the_env_projects_every_member_fcs_bound() {
    let run = run();
    for site in &run.corpus.sites {
        let Theirs::Bound { .. } = site_theirs(&run, site) else {
            // `every_cell_is_still_an_access_fcs_binds` owns this failure.
            continue;
        };
        let candidates = run.env.public_types_named_at_arity(
            &site.declaring_ns,
            &site.declaring_src,
            site.declaring_arity(),
        );
        let [handle] = candidates.as_slice() else {
            panic!(
                "cell {}: our env holds {} public types at {}.{}, arity {} — the \
                 declaring entity has to be unique for the member check below to \
                 mean anything",
                site.label,
                candidates.len(),
                site.declaring_ns.join("."),
                site.declaring_src,
                site.declaring_arity()
            );
        };
        // The search key is an F# source name; FCS's currency is the compiled
        // one. Tying them here is what stops the lookup from being a fixture
        // claim: it must land on the entity the oracle named.
        assert_eq!(
            run.env.entity(*handle).name,
            site.declaring_stem(),
            "cell {}: {}.{} is the entity our env holds, but FCS declares the member \
             on {}",
            site.label,
            site.declaring_ns.join("."),
            site.declaring_src,
            site.declaring_ty
        );
        assert!(
            run.env.member(*handle, &site.compiled_name).is_some(),
            "cell {}: FCS binds {}.{}, but our projection of that entity carries no \
             member named {} — so the cell's decline is a missing member rather than \
             the wake's verdict, and closing the wake's gap would not fix it",
            site.label,
            site.declaring_ty,
            site.member,
            site.compiled_name
        );
    }
}

/// The per-cell table — a measurement, not a gate.
#[test]
#[ignore = "report generator"]
fn verdict_report() {
    let run = run();
    for site in &run.corpus.sites {
        let ours = match site_ours(&run, site) {
            Ours::Member { declaring, member } => format!("{declaring}.{member}"),
            Ours::Deferred => "—".to_string(),
        };
        let theirs = match site_theirs(&run, site) {
            Theirs::Bound { declaring, kind } => format!(
                "{declaring}.{} [{}]",
                site.member,
                kind.as_deref().unwrap_or("kind unreported")
            ),
            Theirs::Nothing => "—".to_string(),
        };
        println!(
            "{:<58} ours={:<28} fcs={} (metadata name {})",
            site.label, ours, theirs, site.compiled_name
        );
    }
}
