//! The **handler-level** differential: the real request handlers, asked as an
//! LSP client asks them, graded against FCS.
//!
//! [`crate::compare_project_uses`] grades what the LSP's serving rule
//! ([`borzoi::handlers::served_resolution_with_range`]) answers at a byte
//! offset. That rule sits below the handlers, so it cannot see what the
//! handlers do around it: turn a `(line, UTF-16 column)` position into a byte,
//! choose a resolution, render a range back, pick the URI a location names,
//! decide what a hover says. This module drives the server over the protocol
//! ([`crate::lsp_client`]) and reads its JSON answers back, with positions
//! converted by an independent implementation ([`crate::utf16`]), so a defect
//! in any of those steps is a wrong answer here.
//!
//! # What is asked
//!
//! For an oracle record the comparator graded (a match or a deferral), the
//! client asks `textDocument/definition`, `textDocument/hover` and
//! `textDocument/references` at the start of the use's first, middle and last
//! character. A probe is skipped where another record no longer than this one
//! also contains its byte — the oracle then has two answers for one cursor —
//! which is the comparator's ownership rule applied at each probe rather than
//! only at the last byte.
//!
//! Every record the comparator matched in the project is asked
//! go-to-definition at its last character, which is cheap. Everything else is
//! asked of a deterministic sample of records ([`SAMPLE_STRIDE`]): a hover, or
//! a definition the resolver declined, costs the server a whole-file inference
//! today, and asking every record everything would take the better part of an
//! hour on the pinned corpus.
//!
//! # What must hold
//!
//! Hard — a violation is a [`HandlerDivergence`], and the run fails:
//!
//! - **definition**: a location is FCS's declaration exactly — in the file
//!   under the URI the client opened it by, at the declaration's byte range; a
//!   referenced-assembly symbol is never located in a project source, and a
//!   project symbol never outside one.
//! - **hover**: its range contains the cursor. A hover describing a symbol
//!   names FCS's symbol: its name and kind, and for a referenced-assembly
//!   symbol its qualified name; a project symbol's hover range ends where FCS's
//!   use ends. A comparable project is never served the degraded single-file
//!   note.
//! - **references**: every location is a use of the same symbol according to
//!   FCS — ending where FCS's use ends and within it, since FCS spans `A.b`
//!   from `A` where the handler marks `b` — and no location twice.
//! - **the comparator's word**: where the comparator matched a record in the
//!   project, go-to-definition at its last character serves that match. If it
//!   did not, the comparator would be grading an answer no user is shown.
//! - Every returned position names a byte of the file it is in.
//!
//! Recorded — in the exact manifest, so every movement is visible: per file,
//! how many probes each handler answered, declined and so on; and, item by
//! item, every probe whose answer is not the one the comparator's verdict
//! implies ([`consistent`]): a handler declining where the comparator matched,
//! answering where it declined, or answering a references request for a symbol
//! with a different set of (correct) uses than it gave for the same symbol
//! from another of its uses. A use in the perturbed run's appended code has no
//! comparator verdict, so every probe of one that is not answered is listed.
//!
//! The second and third are what the handlers' **single-file fallback** looks
//! like from outside. Go-to-definition and hover fall back to resolving the
//! cursor's file on its own when the project's resolution declines the cursor
//! or records nothing there, and find-references when it records nothing; and
//! that fallback can answer a cursor the project's resolver did not — so the
//! comparator, which grades the project's answer, says "declined" where the
//! user is shown a target. Those answers are graded here
//! against FCS like any other, and they are right on the pinned corpus; but a
//! references answer from such a cursor names the uses the *single-file*
//! resolution found, not the project's, so it differs from the answer at
//! another use of the same symbol.
//!
//! # The perturbed run
//!
//! The same questions are asked again of a copy of the project with non-ASCII
//! text substituted into comments and strings and a module of non-ASCII names
//! appended to each implementation file ([`crate::perturb`]). FCS type-checks
//! the copy, and three more things must hold:
//!
//! - **Inertness.** FCS's records on the copy are its records on the original,
//!   each at its mapped place with its declaration mapped, plus the appended
//!   code's. Otherwise the edits changed meaning, and the next check would not
//!   be a metamorphic one.
//! - **Invariance.** The substitutions keep every LSP position where it was,
//!   so at every probe of an original record the server must send the
//!   *identical* answer it sent for the original text. A server that counts
//!   bytes or characters rather than UTF-16 units answers differently.
//! - Everything above, against FCS's answer on the copy — the only oracle the
//!   appended code has.
//!
//! The perturbed run asks every record on a line with a substitution before it
//! (or whose declaration is on one), and every use in the appended code. A
//! record anywhere else has the same bytes before it on its line in both
//! copies, so asking it again would test nothing the plain run did not.
//!
//! Why FCS on the copy, rather than the metamorphic relation alone: invariance
//! is only evidence if the copy means what the original means, and that is a
//! claim about F#'s lexer, not about this module. The perturbation is chosen by
//! *our* lexer's idea of where comments and strings are; if it were wrong, the
//! copy would differ in meaning, and both servers' answers might move together
//! or apart for reasons that are not a position bug. Inertness asks the
//! compiler.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::lsp_client::{LspClient, file_uri};
use crate::perturb::Perturbed;
use crate::utf16::{line_starts, offset_of, position_of};
use crate::{
    AnswerSurface, Comparison, FileUses, Graded, ItemOutcome, ProjectUse, UseDecl, path_key,
};

/// One record in this many is asked everything; the rest only what is cheap.
/// See the module docs. Whether a record is sampled is a function of its own
/// offsets, so the sample is the same on every host.
pub const SAMPLE_STRIDE: NonZeroUsize = NonZeroUsize::new(7).expect("non-zero");

/// Which copy of the project a probe asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Variant {
    Plain,
    Perturbed,
}

impl Variant {
    pub fn label(self) -> &'static str {
        match self {
            Variant::Plain => "plain",
            Variant::Perturbed => "perturbed",
        }
    }
}

/// Where in a use a probe asks: the start of its first, middle or last
/// character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Probe {
    First,
    Middle,
    Last,
}

impl Probe {
    const ALL: [Probe; 3] = [Probe::First, Probe::Middle, Probe::Last];

    pub fn label(self) -> &'static str {
        match self {
            Probe::First => "first",
            Probe::Middle => "middle",
            Probe::Last => "last",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Handler {
    Definition,
    Hover,
    References,
}

impl Handler {
    const ALL: [Handler; 3] = [Handler::Definition, Handler::Hover, Handler::References];

    pub fn label(self) -> &'static str {
        match self {
            Handler::Definition => "definition",
            Handler::Hover => "hover",
            Handler::References => "references",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Handler::Definition => "textDocument/definition",
            Handler::Hover => "textDocument/hover",
            Handler::References => "textDocument/references",
        }
    }
}

/// What the comparator said about the record a probe asks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expectation {
    /// The comparator matched the served answer against the oracle.
    Served(Graded, AnswerSurface),
    /// The comparator found the LSP serving no claim.
    Declined(Graded),
    /// No comparator verdict: a use in the perturbed run's appended code,
    /// declared in the project or in a referenced assembly.
    Fresh(Graded),
}

impl Expectation {
    pub fn label(self) -> String {
        let graded = |g: Graded| match g {
            Graded::Project => "project",
            Graded::Assembly => "assembly",
        };
        match self {
            Expectation::Served(g, surface) => format!(
                "served-{}{}",
                graded(g),
                match surface {
                    AnswerSurface::Resolver => "",
                    AnswerSurface::Attribute => "-attribute",
                    AnswerSurface::Member => "-member",
                }
            ),
            Expectation::Declined(g) => format!("declined-{}", graded(g)),
            Expectation::Fresh(g) => format!("fresh-{}", graded(g)),
        }
    }
}

/// What a probe's answer came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Outcome {
    /// Nothing: `null`, or an empty list.
    Declined,
    /// definition: FCS's declaration.
    Located,
    /// definition: a place outside the project, for a referenced-assembly
    /// symbol (its PDB source or SourceLink URL), which FCS does not locate.
    LocatedElsewhere,
    /// hover: a description naming FCS's symbol.
    Described,
    /// hover: a referenced-assembly description whose head this module does not
    /// read, so only its range was checked.
    DescribedUnread,
    /// hover: "No definition available", with the reason.
    Explained,
    /// hover: something that describes no symbol, such as an expression's type.
    Other,
    /// references: a non-empty answer, every location confirmed by FCS.
    Referenced,
    /// references: as [`Self::Referenced`], but a different answer from the
    /// one an earlier probe got for the same symbol.
    ReferencedDifferently,
    /// A wrong answer; its detail is a [`HandlerDivergence`].
    Wrong,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Declined => "declined",
            Outcome::Located => "located",
            Outcome::LocatedElsewhere => "located-elsewhere",
            Outcome::Described => "described",
            Outcome::DescribedUnread => "described-unread",
            Outcome::Explained => "explained",
            Outcome::Other => "other",
            Outcome::Referenced => "referenced",
            Outcome::ReferencedDifferently => "referenced-differently",
            Outcome::Wrong => "wrong",
        }
    }
}

/// The one outcome the comparator's verdict on a record implies the handler
/// gives there. Every other outcome is listed in the manifest probe by probe.
///
/// One outcome, not a set: where two outcomes were both "as expected", two
/// records could trade them and leave every count the same, so a lost answer
/// would pass the exact gate offset by a gained one. With one expected outcome,
/// any trade moves a listed line.
pub fn expected_outcome(handler: Handler, expectation: Expectation) -> Outcome {
    use Outcome as O;
    let answered = |graded| match (handler, graded) {
        (Handler::Definition, Graded::Project) => O::Located,
        // Answered when its PDB maps it; a referenced-assembly symbol without
        // one is declined, and listed.
        (Handler::Definition, Graded::Assembly) => O::LocatedElsewhere,
        (Handler::Hover, _) => O::Described,
        (Handler::References, _) => O::Referenced,
    };
    match expectation {
        // Find-references reads the resolver's maps, not inference's member
        // table.
        Expectation::Served(_, AnswerSurface::Member) if handler == Handler::References => {
            O::Declined
        }
        Expectation::Served(graded, _) | Expectation::Fresh(graded) => answered(graded),
        Expectation::Declined(_) => match handler {
            Handler::Hover => O::Explained,
            Handler::Definition | Handler::References => O::Declined,
        },
    }
}

/// Whether `outcome` is [`expected_outcome`].
pub fn consistent(handler: Handler, expectation: Expectation, outcome: Outcome) -> bool {
    outcome == expected_outcome(handler, expectation)
}

/// One probe and what it came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeItem {
    pub variant: Variant,
    /// The source the record is in, as the loaded project spells it.
    pub file: PathBuf,
    /// The record's range in the probed copy's text.
    pub range: (usize, usize),
    pub name: String,
    pub handler: Handler,
    pub probe: Probe,
    pub expectation: Expectation,
    pub outcome: Outcome,
}

/// A wrong answer from a handler, or a perturbed copy that is not a copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerDivergence {
    pub variant: Variant,
    pub file: PathBuf,
    pub range: (usize, usize),
    pub name: String,
    /// The request, where one was asked.
    pub asked: Option<(Handler, Probe)>,
    pub detail: String,
}

impl HandlerDivergence {
    pub fn render(&self) -> String {
        let asked = match self.asked {
            Some((handler, probe)) => format!(" {} at {}", handler.label(), probe.label()),
            None => String::new(),
        };
        format!(
            "handler divergence ({}{asked}): {}:{}..{} {:?}: {}",
            self.variant.label(),
            self.file.display(),
            self.range.0,
            self.range.1,
            self.name,
            self.detail
        )
    }
}

/// Why the perturbed run of a project did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerturbedSkip {
    /// FCS could not be asked about the copy, or its answer was unreadable.
    Fcs(String),
    /// FCS type-checked the copy with errors, so it is not a copy with the
    /// original's meaning: `(file, line, column, error number)` for each.
    FcsErrors(Vec<(PathBuf, u32, u32, i32)>),
}

/// The handler differential's verdict on one project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HandlerReport {
    pub probes: Vec<ProbeItem>,
    pub divergences: Vec<HandlerDivergence>,
    /// Set when the perturbed run could not be made.
    pub perturbed_skip: Option<PerturbedSkip>,
    /// The perturbed copy's texts, which its probes' ranges index.
    pub perturbed_texts: Vec<Arc<str>>,
    /// Sources the server answers as another project's, so not probed.
    pub owned_elsewhere: Vec<PathBuf>,
}

/// A record's identity across the two copies: the original file, its range in
/// the original text, and the oracle's name for it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct RecordKey {
    file: usize,
    start: usize,
    end: usize,
    name: String,
}

/// One copy of the project as the client sees it.
struct Copy<'a> {
    variant: Variant,
    /// The loaded project's spelling of each source path, in Compile order.
    paths: &'a [PathBuf],
    texts: Vec<Arc<str>>,
    starts: Vec<Vec<usize>>,
    /// The URI each source was opened under.
    uris: Vec<String>,
    /// FCS's records per source; `None` for a file FCS did not report on or
    /// did not check cleanly.
    fcs: Vec<Option<&'a FileUses>>,
}

impl<'a> Copy<'a> {
    fn new(
        variant: Variant,
        paths: &'a [PathBuf],
        texts: Vec<Arc<str>>,
        fcs: &'a [FileUses],
    ) -> Self {
        let by_key: HashMap<PathBuf, usize> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| (path_key(p), i))
            .collect();
        let mut per_file: Vec<Option<&FileUses>> = vec![None; paths.len()];
        for file in fcs {
            if file.has_error_diagnostics() {
                continue;
            }
            if let Some(&i) = by_key.get(&path_key(&file.path)) {
                per_file[i] = Some(file);
            }
        }
        Self {
            variant,
            paths,
            starts: texts.iter().map(|t| line_starts(t)).collect(),
            uris: paths.iter().map(|p| file_uri(p)).collect(),
            texts,
            fcs: per_file,
        }
    }

    fn file_of_uri(&self, uri: &str) -> Option<usize> {
        self.uris.iter().position(|u| u == uri)
    }

    /// The byte range a JSON `Range` names in source `file`, if both ends name
    /// a byte.
    fn byte_range(&self, file: usize, range: &Value) -> Option<(usize, usize)> {
        let at = |p: &Value| -> Option<usize> {
            let line = u32::try_from(p["line"].as_u64()?).ok()?;
            let character = u32::try_from(p["character"].as_u64()?).ok()?;
            offset_of(&self.texts[file], &self.starts[file], line, character)
        };
        Some((at(&range["start"])?, at(&range["end"])?))
    }
}

/// A location read back: a byte range in a source the client opened, or
/// somewhere else.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Located {
    Source {
        file: usize,
        start: usize,
        end: usize,
    },
    Elsewhere(String),
}

fn read_locations(copy: &Copy<'_>, value: &Value) -> Result<Vec<Located>, String> {
    let items: Vec<&Value> = match value {
        Value::Null => vec![],
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    items
        .into_iter()
        .map(|loc| {
            let uri = loc["uri"]
                .as_str()
                .ok_or_else(|| format!("a location without a URI: {loc}"))?;
            match copy.file_of_uri(uri) {
                Some(file) => match copy.byte_range(file, &loc["range"]) {
                    Some((start, end)) if start <= end => Ok(Located::Source { file, start, end }),
                    _ => Err(format!(
                        "a range that names no bytes of {uri}: {}",
                        loc["range"]
                    )),
                },
                None if names_an_open_source(copy, uri) => Err(format!(
                    "{uri} names an open source by a URI the client did not open it under"
                )),
                None => Ok(Located::Elsewhere(uri.to_string())),
            }
        })
        .collect()
}

/// Whether `uri` names one of the project's sources under another spelling —
/// an answer that would make the client open a second buffer for the file.
fn names_an_open_source(copy: &Copy<'_>, uri: &str) -> bool {
    lsp_types::Url::parse(uri)
        .ok()
        .and_then(|u| u.to_file_path().ok())
        .is_some_and(|path| {
            let key = path_key(&path);
            copy.paths.iter().any(|p| path_key(p) == key)
        })
}

/// The start of the use's first, middle or last character.
fn probe_byte(text: &str, u: &ProjectUse, probe: Probe) -> Option<usize> {
    let starts: Vec<usize> = text
        .get(u.start..u.end)?
        .char_indices()
        .map(|(i, _)| u.start + i)
        .collect();
    let last = starts.len().checked_sub(1)?;
    Some(match probe {
        Probe::First => starts[0],
        Probe::Middle => starts[starts.len() / 2],
        Probe::Last => starts[last],
    })
}

/// Whether another record no longer than `u` contains `byte`, its end
/// included — the comparator's ownership rule (see `served_answer`), applied
/// at any byte of the use.
fn contested(file: &FileUses, u: &ProjectUse, byte: usize) -> bool {
    let len = u.end - u.start;
    file.uses.iter().any(|other| {
        (other.start, other.end) != (u.start, u.end)
            && !other.is_constructor
            && other.start <= byte
            && byte <= other.end
            && other.end - other.start <= len
    })
}

/// Which handlers to ask about a record, and at which characters.
#[derive(Debug, Clone, Default)]
struct Ask {
    handlers: BTreeSet<Handler>,
    probes: BTreeSet<Probe>,
}

impl Ask {
    fn everything() -> Self {
        Self {
            handlers: Handler::ALL.into(),
            probes: Probe::ALL.into(),
        }
    }

    fn definition_at_last() -> Self {
        Self {
            handlers: [Handler::Definition].into(),
            probes: [Probe::Last].into(),
        }
    }

    fn merge(&mut self, other: Ask) {
        self.handlers.extend(other.handlers);
        self.probes.extend(other.probes);
    }
}

/// One record to ask about, in the probed copy's coordinates.
struct Target<'a> {
    file: usize,
    use_: &'a ProjectUse,
    key: RecordKey,
    expectation: Expectation,
    ask: Ask,
}

/// Each request's exact answer, kept to compare the two runs.
type Answers = HashMap<(RecordKey, Handler, Probe), Value>;

fn sampled(u: &ProjectUse, stride: NonZeroUsize) -> bool {
    (u.start.wrapping_mul(31) ^ u.end).is_multiple_of(stride.get())
}

/// The records the comparator grades: uses, not definitions, that the author
/// wrote and that span something.
fn gradable(u: &ProjectUse) -> bool {
    !u.is_from_definition && u.start < u.end && !u.is_compiler_generated
}

/// Ask the server about every target in `copy`, grading each answer.
fn run_copy(
    copy: &Copy<'_>,
    client: &mut LspClient,
    targets: &[Target<'_>],
    report: &mut HandlerReport,
    answers: &mut Answers,
) {
    // The first non-empty references answer for each symbol.
    let mut references_by_symbol: HashMap<SymbolId, BTreeSet<Located>> = HashMap::new();
    for target in targets {
        let file = copy.fcs[target.file].expect("targets are in files FCS reported on");
        let text = &copy.texts[target.file];
        let u = target.use_;
        let divergence = |asked, detail: String| HandlerDivergence {
            variant: copy.variant,
            file: copy.paths[target.file].clone(),
            range: (u.start, u.end),
            name: u.name.clone(),
            asked: Some(asked),
            detail,
        };
        for &probe in &target.ask.probes {
            let Some(byte) = probe_byte(text, u, probe) else {
                continue;
            };
            if contested(file, u, byte) {
                continue;
            }
            let (line, character) = position_of(text, &copy.starts[target.file], byte)
                .expect("a character's start is a position");
            let at = json!({
                "textDocument": { "uri": copy.uris[target.file] },
                "position": { "line": line, "character": character },
            });
            for &handler in &target.ask.handlers {
                let mut params = at.clone();
                if handler == Handler::References {
                    params["context"] = json!({ "includeDeclaration": true });
                }
                let value = match client.request(handler.method(), params) {
                    Ok(value) => value,
                    Err(error) => {
                        report.divergences.push(divergence(
                            (handler, probe),
                            format!("the server answered with an error: {error}"),
                        ));
                        continue;
                    }
                };
                let graded = match handler {
                    Handler::Definition => grade_definition(copy, u, &value),
                    Handler::Hover => grade_hover(copy, target.file, u, byte, &value),
                    Handler::References => {
                        grade_references(copy, u, &value).map(|(outcome, locations)| {
                            if locations.is_empty()
                                || same_as_first_answer(&mut references_by_symbol, u, locations)
                            {
                                outcome
                            } else {
                                Outcome::ReferencedDifferently
                            }
                        })
                    }
                };
                let outcome = graded.unwrap_or_else(|detail| {
                    report
                        .divergences
                        .push(divergence((handler, probe), detail));
                    Outcome::Wrong
                });
                if handler == Handler::Definition
                    && probe == Probe::Last
                    && matches!(target.expectation, Expectation::Served(Graded::Project, _))
                    && outcome == Outcome::Declined
                {
                    report.divergences.push(divergence(
                        (handler, probe),
                        "the comparator matched this record, but go-to-definition at its last \
                         character serves nothing"
                            .to_string(),
                    ));
                }
                answers.insert(
                    (target.key.clone(), handler, probe),
                    normalise(handler, value),
                );
                report.probes.push(ProbeItem {
                    variant: copy.variant,
                    file: copy.paths[target.file].clone(),
                    range: (u.start, u.end),
                    name: u.name.clone(),
                    handler,
                    probe,
                    expectation: target.expectation,
                    outcome,
                });
            }
        }
    }
}

/// An answer in a form two runs can be compared in: a references list sorted,
/// since its order is not part of its meaning.
fn normalise(handler: Handler, value: Value) -> Value {
    match (handler, value) {
        (Handler::References, Value::Array(mut items)) => {
            items.sort_by_key(|item| item.to_string());
            Value::Array(items)
        }
        (_, value) => value,
    }
}

/// The oracle's identity for a symbol: what two records must share to be uses
/// of one symbol.
///
/// Read from FCS's structural facts, not its rendering. `FullName` prints a
/// member's declaring type *as instantiated at the use*
/// (`ImmutableArray<(int -> string)>.Empty`), so two uses of one member carry
/// two spellings of it; the declaring entity's compiled names and arities name
/// it once. Where there is no declaring entity (a top-level type, a module, a
/// project symbol) the full name stands in, and its kind and generic arity keep
/// apart what the name alone does not: `IComparable` and `IComparable<'T>`, a
/// type and its companion module.
/// A declaring entity as a hashable value: namespace, the path of compiled names
/// and arities, and whether the use is a constructor.
type DeclaringKey = (Vec<String>, Vec<(String, usize)>, bool);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SymbolId {
    name: String,
    decl: Option<(PathBuf, usize, usize)>,
    assembly: Option<String>,
    kind: Option<String>,
    arity: Option<usize>,
    declaring: Option<DeclaringKey>,
    /// Only where `declaring` is `None`.
    full_name: Option<String>,
}

fn symbol_id(u: &ProjectUse) -> SymbolId {
    let (kind, arity, declaring) = match &u.declaring {
        // A constructor record names the type the author wrote (`T(x)`), and
        // find-references lists it among the type's uses, so it is identified
        // as that type, by the type's full name — which is what FCS reports
        // for it — and arity. Not by the declaring path: the constructor's
        // runs through the type, and how far up it reaches is not the type's
        // own path (a BCL type's carries a namespace segment).
        Some(d) if u.is_constructor => (
            Some("type".to_string()),
            d.path.last().map(|(_, arity)| *arity),
            None,
        ),
        // An entity is named by its full name too, so that a type use and a
        // constructor use of it agree.
        Some(_) if matches!(u.symbol_kind.as_deref(), Some("type" | "module")) => {
            (u.symbol_kind.clone(), u.generic_arity, None)
        }
        Some(d) => (
            u.symbol_kind.clone(),
            u.generic_arity,
            Some((d.namespace.clone(), d.path.clone(), d.is_constructor)),
        ),
        None => (u.symbol_kind.clone(), u.generic_arity, None),
    };
    SymbolId {
        name: u.name.clone(),
        decl: match &u.decl {
            UseDecl::InProject(d) => Some((d.file.clone(), d.start, d.end)),
            UseDecl::Unlocated | UseDecl::OutsideProject(_) => None,
        },
        assembly: u.assembly.clone(),
        kind,
        arity,
        full_name: if declaring.is_some() {
            None
        } else {
            u.full_name.as_deref().map(bare_qualified)
        },
        declaring,
    }
}

fn grade_definition(copy: &Copy<'_>, u: &ProjectUse, value: &Value) -> Result<Outcome, String> {
    let locations = read_locations(copy, value)?;
    match (locations.as_slice(), &u.decl) {
        ([], _) => Ok(Outcome::Declined),
        ([Located::Source { file, start, end }], UseDecl::InProject(decl)) => {
            if copy.paths[*file] == decl.file && (*start, *end) == (decl.start, decl.end) {
                Ok(Outcome::Located)
            } else {
                Err(format!(
                    "located {}:{start}..{end} {:?}; FCS declares it at {}:{}..{}",
                    copy.paths[*file].display(),
                    &copy.texts[*file][*start..*end],
                    decl.file.display(),
                    decl.start,
                    decl.end
                ))
            }
        }
        ([Located::Elsewhere(_)], UseDecl::Unlocated | UseDecl::OutsideProject(_)) => {
            Ok(Outcome::LocatedElsewhere)
        }
        (locations, decl) => Err(format!(
            "located {locations:?} for a symbol FCS declares at {decl:?}"
        )),
    }
}

/// The FCS symbol kinds a project definition's hover label may name.
fn project_label_kinds(label: &str) -> Option<&'static [&'static str]> {
    Some(match label {
        "function" | "value" | "parameter" | "pattern local" | "static member" => &["member"],
        // A use of an active-pattern case resolves to its recognizer (sema
        // gives a case use the recognizer's definition), while FCS names the
        // case.
        "active pattern" => &["member", "activepatterncase"],
        "type" | "exception" => &["type"],
        "union case" => &["unioncase"],
        "enum case" => &["field"],
        "active pattern case" => &["activepatterncase"],
        "type parameter" => &["genericparameter"],
        _ => return None,
    })
}

/// A name with its quoting removed: FCS's display name keeps the backticks a
/// name needs and parenthesises an operator, and a hover shows a type
/// parameter with its tick.
fn bare_name(name: &str) -> &str {
    let name = name.strip_prefix('\'').unwrap_or(name);
    let name = name
        .strip_prefix("``")
        .and_then(|n| n.strip_suffix("``"))
        .unwrap_or(name);
    match name.strip_prefix('(').and_then(|n| n.strip_suffix(')')) {
        Some(op) => op.trim(),
        None => name,
    }
}

/// A qualified name with generic arguments and quoting removed, for comparing
/// a hover's rendering with FCS's.
///
/// The `>` of a function type's `->` closes nothing: `Holder<int -> string>`
/// is one argument.
fn bare_qualified(name: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut previous = None;
    for c in name.replace("``", "").chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 && previous != Some('-') => depth -= 1,
            c if depth == 0 => out.push(c),
            _ => {}
        }
        previous = Some(c);
    }
    out
}

/// The name a project definition's hover head names: `name` or `name : type`,
/// where a quoted name may itself contain ` : `.
fn hovered_name(head: &str) -> &str {
    if let Some(quoted) = head.strip_prefix("``")
        && let Some(close) = quoted.find("``")
    {
        return &head[..close + 4];
    }
    head.split(" : ").next().unwrap_or(head)
}

/// The leading Markdown code span of `text`, unfenced, and what follows it.
fn code_span(text: &str) -> Option<(&str, &str)> {
    let fence = text.len() - text.trim_start_matches('`').len();
    if fence == 0 {
        return None;
    }
    let rest = &text[fence..];
    let close = rest.find(&"`".repeat(fence))?;
    let content = &rest[..close];
    let content = match content.strip_prefix(' ').and_then(|c| c.strip_suffix(' ')) {
        Some(inner) if !inner.is_empty() => inner,
        _ => content,
    };
    Some((content, &rest[close + fence..]))
}

/// What a referenced-assembly hover's head declares: the FCS symbol kinds it
/// may be, and its name.
fn assembly_head(head: &str) -> Option<(&'static [&'static str], &str)> {
    let mut head = head;
    while let Some(rest) = head.strip_prefix("[<") {
        head = rest.split_once(">] ")?.1;
    }
    const KEYWORDS: &[(&str, &[&str])] = &[
        ("type ", &["type"]),
        ("module ", &["module"]),
        ("exception ", &["type"]),
        ("union case ", &["unioncase"]),
        ("val mutable ", &["field", "member"]),
        ("val ", &["field", "member"]),
        ("static member ", &["member"]),
        ("abstract member ", &["member"]),
        ("member ", &["member"]),
    ];
    let (kinds, rest) = KEYWORDS
        .iter()
        .find_map(|(keyword, kinds)| head.strip_prefix(keyword).map(|rest| (*kinds, rest)))?;
    let name = match rest.strip_prefix("``") {
        Some(quoted) => &rest[..quoted.find("``")? + 4],
        None => {
            let end = rest.find([':', '<', ' ', '(']).unwrap_or(rest.len());
            &rest[..end]
        }
    };
    (!name.is_empty()).then_some((kinds, name))
}

fn grade_hover(
    copy: &Copy<'_>,
    file: usize,
    u: &ProjectUse,
    byte: usize,
    value: &Value,
) -> Result<Outcome, String> {
    if value.is_null() {
        return Ok(Outcome::Declined);
    }
    let body = value["contents"]["value"]
        .as_str()
        .ok_or_else(|| format!("a hover without Markdown contents: {value}"))?;
    let (start, end) = copy
        .byte_range(file, &value["range"])
        .ok_or_else(|| format!("a hover range that names no bytes: {}", value["range"]))?;
    if !(start <= byte && byte <= end) {
        return Err(format!(
            "the hover's range {start}..{end} does not contain the cursor at {byte}"
        ));
    }
    if body.starts_with("**No definition available**") {
        // The note for a file no evaluated project compiles. Every file here
        // is a Compile item of a project that evaluated.
        if body.contains("didn't evaluate") {
            return Err(
                "the note for a file outside any evaluated project, in a comparable project"
                    .to_string(),
            );
        }
        return Ok(Outcome::Explained);
    }
    let mut paragraphs = body.split("\n\n");
    let first = paragraphs.next().unwrap_or_default();
    let (content, after) =
        code_span(first).ok_or_else(|| format!("a hover with no code span: {body:?}"))?;
    let check = |name: &str, kinds: &[&str]| -> Result<(), String> {
        if bare_name(name) != bare_name(&u.name) {
            return Err(format!(
                "the hover {first:?} names another symbol than FCS's {:?}",
                u.name
            ));
        }
        // A constructor record names the type the author wrote (FCS reports
        // `[<Literal>]` and `BinaryReader(s)` as a use of the constructor), and
        // a hover on it describes that type.
        let kind = match u.symbol_kind.as_deref() {
            Some("member") if u.is_constructor && kinds.contains(&"type") => None,
            kind => kind,
        };
        match kind {
            Some(kind) if !kinds.contains(&kind) => Err(format!(
                "the hover {first:?} describes a {kinds:?}; FCS's {:?} is a {kind}",
                u.name
            )),
            _ => Ok(()),
        }
    };
    if let Some(label) = after.strip_prefix(" — ") {
        let Some(kinds) = project_label_kinds(label) else {
            return Ok(Outcome::Other);
        };
        check(hovered_name(content), kinds)?;
        if !(u.start <= start && end == u.end) {
            return Err(format!(
                "the hover's range {start}..{end} does not end FCS's use {}..{}",
                u.start, u.end
            ));
        }
        return Ok(Outcome::Described);
    }
    if !after.is_empty() {
        return Ok(Outcome::Other);
    }
    let Some((kinds, name)) = assembly_head(content) else {
        return Ok(Outcome::DescribedUnread);
    };
    check(name, kinds)?;
    // The context line names where the symbol is declared: `in X.Y`, possibly
    // after a kind (`class, in X.Y`).
    let context = paragraphs
        .next()
        .and_then(|p| p.split_once("in ").map(|(_, rest)| rest))
        .and_then(|rest| code_span(rest).map(|(c, _)| c));
    if let (Some(context), Some(full_name)) = (context, u.full_name.as_deref()) {
        let ours = bare_qualified(&format!("{context}.{name}"));
        let theirs = bare_qualified(full_name);
        // A constructor's full name is its type's, with the constructor after.
        let theirs = match theirs.strip_suffix("..ctor") {
            Some(type_name) if u.is_constructor => type_name.to_string(),
            _ => theirs,
        };
        if ours != theirs {
            return Err(format!(
                "the hover {first:?} in {context:?} names {ours}; FCS's is {theirs}"
            ));
        }
    }
    Ok(Outcome::Described)
}

/// Grade a references answer, returning its locations for the
/// cursor-independence check.
fn grade_references(
    copy: &Copy<'_>,
    u: &ProjectUse,
    value: &Value,
) -> Result<(Outcome, BTreeSet<Located>), String> {
    let locations = read_locations(copy, value)?;
    if locations.is_empty() {
        return Ok((Outcome::Declined, BTreeSet::new()));
    }
    let id = symbol_id(u);
    let mut seen = BTreeSet::new();
    for location in locations {
        let Located::Source { file, start, end } = &location else {
            return Err(format!("a reference outside the project: {location:?}"));
        };
        // The request includes the declaration, which is FCS's declaration
        // range for the symbol — not always a record of the symbol's own name:
        // an active-pattern case declares at its recognizer, `(|Foo|_|)`.
        let declaration = matches!(&u.decl, UseDecl::InProject(d)
            if copy.paths[*file] == d.file && (*start, *end) == (d.start, d.end));
        let confirmed = declaration
            || copy.fcs[*file].is_some_and(|f| {
                f.uses.iter().any(|o| {
                    o.start <= *start && o.end == *end && start < end && symbol_id(o) == id
                })
            });
        if !confirmed {
            let there: Vec<String> = copy.fcs[*file]
                .iter()
                .flat_map(|f| &f.uses)
                .filter(|o| o.end == *end)
                .map(|o| format!("{:?}", symbol_id(o)))
                .collect();
            return Err(format!(
                "{}:{start}..{end} {:?} is not a use of FCS's {:?} ({id:?}); FCS has {there:?} \
                 there",
                copy.paths[*file].display(),
                &copy.texts[*file][*start..*end],
                u.name,
            ));
        }
        if !seen.insert(location.clone()) {
            return Err(format!("{location:?} twice"));
        }
    }
    Ok((Outcome::Referenced, seen))
}

/// Whether a non-empty references answer for `u`'s symbol is the first one this
/// copy got for that symbol, or the same as it. Two different answers for one
/// symbol are each confirmed by FCS by now, so neither is wrong; but which uses
/// the user is shown depends on where they asked, and that is recorded.
fn same_as_first_answer(
    first_answers: &mut HashMap<SymbolId, BTreeSet<Located>>,
    u: &ProjectUse,
    locations: BTreeSet<Located>,
) -> bool {
    match first_answers.entry(symbol_id(u)) {
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(locations);
            true
        }
        std::collections::hash_map::Entry::Occupied(slot) => *slot.get() == locations,
    }
}

/// The comparator's verdict on each record it graded.
fn expectations(comparison: &Comparison, paths: &[PathBuf]) -> HashMap<RecordKey, Expectation> {
    let index: HashMap<&Path, usize> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_path(), i))
        .collect();
    let mut out = HashMap::new();
    for item in &comparison.ledger {
        let expectation = match item.outcome {
            ItemOutcome::Match(graded, surface) => Expectation::Served(graded, surface),
            ItemOutcome::Deferral { graded, .. } => Expectation::Declined(graded),
            _ => continue,
        };
        let Some(&file) = index.get(item.file.as_path()) else {
            continue;
        };
        out.entry(RecordKey {
            file,
            start: item.range.0,
            end: item.range.1,
            name: item.name.clone(),
        })
        .or_insert(expectation);
    }
    out
}

/// The perturbed copy of a project, and FCS's answer on it.
pub struct PerturbedProject {
    /// One per source, in Compile order.
    pub files: Vec<Perturbed>,
    /// FCS's records on the copy, in the copy's offsets, with every path
    /// spelled as the original project spells it.
    pub fcs: Result<Vec<FileUses>, PerturbedSkip>,
}

/// Run the differential over one comparable project: `paths` and `texts` are
/// its sources in Compile order, `fcs` the oracle's records on them and
/// `comparison` the comparator's verdict. `owned` says, per source, whether the
/// server answers it as this project's: a source another project also compiles
/// may be answered as that one's, and is then not probed. One record in
/// `sample_stride` is asked everything ([`SAMPLE_STRIDE`]). `start_server`
/// starts a server for the project; it is called once per copy.
#[allow(clippy::too_many_arguments)]
pub fn compare_handlers(
    paths: &[PathBuf],
    texts: &[Arc<str>],
    owned: &[bool],
    sample_stride: NonZeroUsize,
    fcs: &[FileUses],
    comparison: &Comparison,
    perturbed: &PerturbedProject,
    start_server: &dyn Fn() -> LspClient,
) -> HandlerReport {
    assert_eq!(owned.len(), paths.len(), "one ownership verdict per source");
    let mut report = HandlerReport {
        owned_elsewhere: paths
            .iter()
            .zip(owned)
            .filter(|(_, owned)| !**owned)
            .map(|(path, _)| path.clone())
            .collect(),
        ..HandlerReport::default()
    };
    let expected = expectations(comparison, paths);
    let plain = Copy::new(Variant::Plain, paths, texts.to_vec(), fcs);
    let index: HashMap<&Path, usize> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_path(), i))
        .collect();
    let perturbed_fcs = perturbed.fcs.as_ref().ok();
    // An original record the perturbed run asks again: one on a line with a
    // substitution before it, or whose declaration is on one.
    let affected = |file: usize, u: &ProjectUse| -> bool {
        perturbed.files[file].substituted_before_on_line(&texts[file], u.start)
            || match &u.decl {
                UseDecl::InProject(d) => index.get(d.file.as_path()).is_some_and(|&df| {
                    perturbed.files[df].substituted_before_on_line(&texts[df], d.start)
                }),
                UseDecl::Unlocated | UseDecl::OutsideProject(_) => false,
            }
    };

    let mut plain_targets = Vec::new();
    let mut rerun: Vec<(usize, &ProjectUse, Expectation)> = Vec::new();
    for (file, uses) in plain.fcs.iter().enumerate() {
        let Some(uses) = uses.filter(|_| owned[file]) else {
            continue;
        };
        for u in uses.uses.iter().filter(|u| gradable(u)) {
            let key = RecordKey {
                file,
                start: u.start,
                end: u.end,
                name: u.name.clone(),
            };
            let Some(&expectation) = expected.get(&key) else {
                continue;
            };
            let mut ask = Ask::default();
            if sampled(u, sample_stride) {
                ask.merge(Ask::everything());
            }
            if matches!(expectation, Expectation::Served(Graded::Project, _)) {
                ask.merge(Ask::definition_at_last());
            }
            if perturbed_fcs.is_some() && affected(file, u) {
                ask.merge(Ask::everything());
                rerun.push((file, u, expectation));
            }
            if !ask.handlers.is_empty() {
                plain_targets.push(Target {
                    file,
                    use_: u,
                    key,
                    expectation,
                    ask,
                });
            }
        }
    }

    let mut plain_answers = Answers::new();
    {
        let mut client = start_server();
        for (path, text) in paths.iter().zip(texts) {
            client.open(path, text);
        }
        run_copy(
            &plain,
            &mut client,
            &plain_targets,
            &mut report,
            &mut plain_answers,
        );
    }

    let Some(perturbed_fcs) = perturbed_fcs else {
        report.perturbed_skip = perturbed.fcs.as_ref().err().cloned();
        return report;
    };
    let perturbed_texts: Vec<Arc<str>> = perturbed
        .files
        .iter()
        .map(|p| Arc::<str>::from(p.text.as_str()))
        .collect();
    report.perturbed_texts = perturbed_texts.clone();
    let copy = Copy::new(Variant::Perturbed, paths, perturbed_texts, perturbed_fcs);
    let before = report.divergences.len();
    check_inertness(&plain, &copy, perturbed, &index, &mut report);
    if report.divergences.len() > before {
        return report;
    }

    let mut targets = Vec::new();
    for &(file, u, expectation) in &rerun {
        let p = &perturbed.files[file];
        let (start, end) = (p.map(u.start), p.map(u.end));
        let Some(mapped) = copy.fcs[file].and_then(|f| {
            f.uses
                .iter()
                .find(|o| (o.start, o.end) == (start, end) && o.name == u.name)
        }) else {
            continue;
        };
        targets.push(Target {
            file,
            use_: mapped,
            key: RecordKey {
                file,
                start: u.start,
                end: u.end,
                name: u.name.clone(),
            },
            expectation,
            ask: Ask::everything(),
        });
    }
    for (file, uses) in copy.fcs.iter().enumerate() {
        let (Some(uses), Some(at), true) = (uses, perturbed.files[file].appended_at, owned[file])
        else {
            continue;
        };
        for u in uses.uses.iter().filter(|u| gradable(u) && u.start >= at) {
            targets.push(Target {
                file,
                use_: u,
                key: RecordKey {
                    file,
                    start: u.start,
                    end: u.end,
                    name: format!("appended {}", u.name),
                },
                expectation: Expectation::Fresh(match u.decl {
                    UseDecl::InProject(_) => Graded::Project,
                    UseDecl::Unlocated | UseDecl::OutsideProject(_) => Graded::Assembly,
                }),
                ask: Ask::everything(),
            });
        }
    }
    let mut perturbed_answers = Answers::new();
    {
        let mut client = start_server();
        for (path, text) in paths.iter().zip(&copy.texts) {
            client.open(path, text);
        }
        run_copy(
            &copy,
            &mut client,
            &targets,
            &mut report,
            &mut perturbed_answers,
        );
    }

    // Invariance: the substitutions moved no LSP position, so an original
    // record's answers must be exactly what the original text got — but for
    // the uses the appended code adds, which a references answer for a symbol
    // it also uses (`String.length`) rightly gains.
    let mut keys: Vec<_> = perturbed_answers.keys().collect();
    keys.sort();
    for key in keys {
        let Some(before) = plain_answers.get(key) else {
            continue;
        };
        let after = &without_appended(&copy, perturbed, &perturbed_answers[key]);
        if before != after {
            let (record, handler, probe) = key;
            report.divergences.push(HandlerDivergence {
                variant: Variant::Perturbed,
                file: paths[record.file].clone(),
                range: (record.start, record.end),
                name: record.name.clone(),
                asked: Some((*handler, *probe)),
                detail: format!(
                    "the answer moved under substitutions that move no position: {before} \
                     became {after}"
                ),
            });
        }
    }
    report
}

/// A references answer on the perturbed copy without the locations in the
/// appended code; any other answer as it is.
fn without_appended(copy: &Copy<'_>, perturbed: &PerturbedProject, value: &Value) -> Value {
    let Value::Array(items) = value else {
        return value.clone();
    };
    let original = |item: &Value| -> bool {
        let file = item["uri"].as_str().and_then(|uri| copy.file_of_uri(uri));
        match file.and_then(|f| Some((f, copy.byte_range(f, &item["range"])?))) {
            Some((f, (start, _))) => perturbed.files[f].appended_at.is_none_or(|at| start < at),
            // Not a location in a source: kept, so the comparison sees it.
            None => true,
        }
    };
    Value::Array(
        items
            .iter()
            .filter(|item| original(item))
            .cloned()
            .collect(),
    )
}

/// Check that FCS's answer on the copy is its answer on the original, mapped:
/// every original record at its mapped place with its declaration mapped, and
/// nothing else outside the appended code.
fn check_inertness(
    plain: &Copy<'_>,
    copy: &Copy<'_>,
    perturbed: &PerturbedProject,
    index: &HashMap<&Path, usize>,
    report: &mut HandlerReport,
) {
    /// What must survive the perturbation of one record, in the copy's offsets.
    type Shape = (
        String,
        (usize, usize),
        bool,
        Option<(PathBuf, usize, usize)>,
        Option<String>,
        Option<String>,
    );
    let shape = |u: &ProjectUse, file: usize, map: bool| -> Shape {
        let at = |f: usize, offset: usize| {
            if map {
                perturbed.files[f].map(offset)
            } else {
                offset
            }
        };
        (
            u.name.clone(),
            (at(file, u.start), at(file, u.end)),
            u.is_from_definition,
            match &u.decl {
                UseDecl::InProject(d) => {
                    let df = index[d.file.as_path()];
                    Some((d.file.clone(), at(df, d.start), at(df, d.end)))
                }
                UseDecl::Unlocated | UseDecl::OutsideProject(_) => None,
            },
            u.assembly.clone(),
            u.full_name.clone(),
        )
    };
    for file in 0..plain.paths.len() {
        let not_inert = |detail: String| HandlerDivergence {
            variant: Variant::Perturbed,
            file: plain.paths[file].clone(),
            range: (0, 0),
            name: "<file>".to_string(),
            asked: None,
            detail,
        };
        let (original, copied) = match (plain.fcs[file], copy.fcs[file]) {
            (Some(original), Some(copied)) => (original, copied),
            (None, None) => continue,
            (original, _) => {
                report.divergences.push(not_inert(format!(
                    "FCS checked the file cleanly on the {} copy only",
                    if original.is_some() {
                        "original"
                    } else {
                        "perturbed"
                    }
                )));
                continue;
            }
        };
        let mut expected: BTreeMap<Shape, usize> = BTreeMap::new();
        for u in &original.uses {
            *expected.entry(shape(u, file, true)).or_default() += 1;
        }
        let appended_at = perturbed.files[file].appended_at.unwrap_or(usize::MAX);
        let mut actual: BTreeMap<Shape, usize> = BTreeMap::new();
        for u in copied.uses.iter().filter(|u| u.start < appended_at) {
            *actual.entry(shape(u, file, false)).or_default() += 1;
        }
        if expected != actual {
            let missing: Vec<_> = expected
                .iter()
                .filter(|(k, n)| actual.get(*k) != Some(n))
                .take(3)
                .collect();
            let extra: Vec<_> = actual
                .iter()
                .filter(|(k, n)| expected.get(*k) != Some(n))
                .take(3)
                .collect();
            report.divergences.push(not_inert(format!(
                "the substitutions are not inert: FCS's records moved (expected but not found: \
                 {missing:?}; found but not expected: {extra:?})"
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_compare_without_their_quoting() {
        assert_eq!(bare_name("'Types"), "Types");
        assert_eq!(bare_name("``café🦀``"), "café🦀");
        assert_eq!(bare_name("(+)"), "+");
        assert_eq!(bare_name("( *** )"), "***");
        assert_eq!(bare_qualified("A.Holder<_>.Value"), "A.Holder.Value");
        assert_eq!(bare_qualified("System.List<'T>.``Add``"), "System.List.Add");
        assert_eq!(
            bare_qualified("Demo.Holder<(int -> string)>.Empty"),
            "Demo.Holder.Empty"
        );
        assert_eq!(
            bare_qualified("Demo.Holder<int -> string>.Empty"),
            "Demo.Holder.Empty"
        );
    }

    #[test]
    fn a_hovered_name_keeps_a_quoted_separator() {
        assert_eq!(hovered_name("x : int"), "x");
        assert_eq!(hovered_name("x"), "x");
        assert_eq!(hovered_name("``a : b`` : int"), "``a : b``");
        assert_eq!(hovered_name("``a : b``"), "``a : b``");
    }

    #[test]
    fn a_code_span_is_read_whatever_its_fence() {
        assert_eq!(
            code_span("`x : int` — value"),
            Some(("x : int", " — value"))
        );
        assert_eq!(code_span("`` `a` `` — type"), Some(("`a`", " — type")));
        assert_eq!(code_span("plain"), None);
    }

    #[test]
    fn an_assembly_head_names_its_kind_and_symbol() {
        assert_eq!(
            assembly_head("static member WriteLine: value: string -> unit"),
            Some((&["member"][..], "WriteLine"))
        );
        assert_eq!(
            assembly_head("[<Struct; IsReadOnly>] type StringHandle"),
            Some((&["type"][..], "StringHandle"))
        );
        assert_eq!(
            assembly_head("val id<'T>: x: 'T -> 'T"),
            Some((&["field", "member"][..], "id"))
        );
        assert_eq!(assembly_head("new: unit -> Foo"), None);
    }
}
