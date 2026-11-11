//! Every FCS-side reader, and our side of each comparison, survives the
//! deepest record fcs-dump emits, on half of [`DEEP_STACK_BYTES`].
//!
//! The differentials read FCS's tree from AdjacentTag JSON with no recursion
//! limit ([`read_fcs_record`]), so what bounds the stack they need is how
//! deep a record can be: fcs-dump refuses to serialise a tree past its
//! `MaxDepth` ([`FCS_DUMP_MAX_DEPTH`]). This checks that the bound sits inside
//! [`DEEP_STACK_BYTES`] with a margin of two.
//!
//! For every repetition form of the grammar
//! ([`repetition_shapes`](borzoi_oracle_harness::repetition_shapes)), the case
//! is the repeat count whose record is the deepest fcs-dump emits ([`Count`]).
//! Each runs in a child copy of this binary ([`stack_probe`]) on a thread of
//! half the deep stack, through what a corpus sweep runs on a file: the FCS
//! normaliser, the range audit and the recovered-tree grader over that record
//! and our parse of the same source, and our normaliser over the tallest tree
//! our parser returns cleanly for the shape. A reader that stops near the top of a
//! tree survives any stack, so which stages ran to the end, and how deep each
//! case's record is, are pinned exactly in
//! `tests/manifests/fcs_record_stack_bound.txt`.
//!
//! A shape alone is enough: nested in modules, its record's deepest path is
//! the module chain, which the `nested_modules` shape takes to the bound by
//! itself, and the breadth of the shape under it costs the readers time and
//! no stack.

use std::collections::HashSet;

use borzoi_cst::parser::{parse_sig_with_symbols, parse_with_symbols};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_oracle_harness::repetition_shapes::{
    Case, Form, Kind, SHAPES, SHORT, tallest_clean_count,
};
use borzoi_oracle_harness::stack_probe::{self, Outcome};
use serde::Deserialize;
use tempfile::NamedTempFile;

use crate::common::corpus_manifest::{check_manifest, regenerate};
use crate::common::fcs_record::{
    DEEP_STACK_BYTES, FCS_DUMP_MAX_DEPTH, json_depth, on_stack_marked_deep, read_fcs_record,
};
use crate::common::normalised_ast::{normalise_fcs_dump, normalise_parse};
use crate::common::recovery::grade;
use crate::common::{ast_ranges_match, catch_unwind_silent, fcs_ast_batch, par_map};

/// The stack every case runs on: half of [`DEEP_STACK_BYTES`], the margin
/// the bound promises.
const PROBE_STACK_BYTES: usize = DEEP_STACK_BYTES / 2;

/// The child entry's full test path, for [`stack_probe::run_case`].
const CHILD: &str = "fcs_record_stack_bound::probe_child";

/// The variable that hands a child its case.
const CASE_VAR: &str = "BORZOI_CST_FCS_RECORD_STACK_CASE";

/// The deepest record among the cases whose every stage ran to the end must
/// come this close to [`FCS_DUMP_MAX_DEPTH`], or the cases do not reach the
/// depth the bound is about. One repeat of a shape adds a few levels, so the
/// deepest record fcs-dump still emits sits just short of its limit.
const REACH: usize = FCS_DUMP_MAX_DEPTH - 32;

/// The shapes whose record grows exponentially with the repeat count.
///
/// FCS's `SynExpr.Lambda` carries its body twice, once desugared and once as
/// the parser wrote it (`parsedData`), so a chain of `fun`s doubles its record
/// per repeat: fcs-dump runs out of memory near twenty repeats, a few hundred
/// levels deep, long before the depth bound. The test checks the list against
/// the records' growth both ways.
const EXPONENTIAL: &[&str] = &["lambda_chain"];

/// Every shape, in a fixed order, so a child process can be handed a case by
/// its index.
fn cases() -> Vec<Case> {
    SHAPES
        .iter()
        .map(|shape| Case {
            shape,
            nested: false,
            form: Form::Long,
        })
        .collect()
}

/// `src` as a file of `kind`, for fcs-dump to read.
fn source_file(kind: Kind, src: &str) -> NamedTempFile {
    let suffix = match kind {
        Kind::Impl => ".fs",
        Kind::Sig => ".fsi",
    };
    let mut tmp = NamedTempFile::with_suffix(suffix).expect("create tempfile");
    std::io::Write::write_all(&mut tmp, src.as_bytes()).expect("write source");
    tmp
}

#[derive(Deserialize)]
struct Refusal {
    #[serde(rename = "Error")]
    error: Option<String>,
}

/// fcs-dump's record of `case` at repeat count `n`, or `None` when it refuses
/// the tree as too deep to serialise.
///
/// `System.Text.Json` words that refusal two ways: as a possible cycle "or
/// … depth larger than the maximum allowed", or, when the limit trips inside
/// a custom converter, as "could not be serialized". The test checks that
/// every refusal it saw was the depth limit: one more repeat's worth of
/// nesting on the longest record it emitted must reach [`FCS_DUMP_MAX_DEPTH`].
fn record(case: &Case, n: usize) -> Option<String> {
    let file = source_file(case.shape.kind, &case.source(n));
    let json = fcs_ast_batch(file.path());
    let refusal: Refusal = serde_json::from_str(&json).expect("an ast-batch record");
    match refusal.error {
        None => Some(json),
        Some(e)
            if e.contains("maximum allowed depth")
                || e.starts_with("The object or value could not be serialized.") =>
        {
            None
        }
        Some(e) => panic!("{}: fcs-dump failed at {n} repeats: {e}", case.name()),
    }
}

/// The repeat count a case takes, and why it gives the deepest record
/// fcs-dump emits for the shape.
#[derive(Clone, Copy, Debug)]
enum Count {
    /// The record is no deeper at twice [`SHORT`] repeats than at [`SHORT`]:
    /// the shape repeats side by side, so [`SHORT`] is as deep as any count.
    Flat,
    /// The shape is in [`EXPONENTIAL`]: [`SHORT`] repeats, since fcs-dump runs
    /// out of memory before the record gets deep.
    Exponential,
    /// [`Case::long`] repeats, still within the depth bound.
    Long(usize),
    /// The longest count fcs-dump emits; one more repeat is refused as too
    /// deep.
    Bound(usize),
}

impl Count {
    fn repeats(self) -> usize {
        match self {
            Count::Flat | Count::Exponential => SHORT,
            Count::Long(n) | Count::Bound(n) => n,
        }
    }

    fn spell(self) -> &'static str {
        match self {
            Count::Flat => "flat",
            Count::Exponential => "exponential",
            Count::Long(_) => "long",
            Count::Bound(_) => "at the bound",
        }
    }
}

/// The count whose record is the deepest fcs-dump emits for `case`.
///
/// A record's depth is affine in the count of a shape that nests, so the
/// count aimed at is where the line through [`SHORT`] and twice [`SHORT`]
/// meets [`FCS_DUMP_MAX_DEPTH`]; galloping out from there brackets the last
/// count fcs-dump emits, and bisection finds it. A tree too deep at one count
/// is too deep at every longer one, so the search is sound however far the
/// aim is off; the aim only saves fcs-dump calls, each of which parses a few
/// thousand repeats.
fn chosen_count(case: &Case) -> Count {
    let name = case.name();
    let short = record(case, SHORT).unwrap_or_else(|| panic!("{name}: the short form's record"));
    let twice =
        record(case, 2 * SHORT).unwrap_or_else(|| panic!("{name}: twice the short form's record"));
    let thrice = record(case, 3 * SHORT)
        .unwrap_or_else(|| panic!("{name}: three times the short form's record"));
    // Each further SHORT repeats adds the same bytes to a record that grows
    // linearly, and 2^SHORT times as many to one that doubles per repeat.
    let (first, second) = (twice.len() - short.len(), thrice.len() - twice.len());
    let exponential = second > 2 * first;
    assert_eq!(
        exponential,
        EXPONENTIAL.contains(&case.shape.name),
        "{name}: {SHORT} more repeats added {first} bytes to the record, then {second}, so \
         EXPONENTIAL must {} the shape",
        if exponential { "list" } else { "not list" }
    );
    if exponential {
        return Count::Exponential;
    }
    let (low, high) = (json_depth(&short), json_depth(&twice));
    if high == low {
        return Count::Flat;
    }
    let long = case.long();
    let per_repeat = (high - low) as f64 / SHORT as f64;
    let aim = SHORT + ((FCS_DUMP_MAX_DEPTH.saturating_sub(low)) as f64 / per_repeat) as usize;
    let probe = aim.clamp(2 * SHORT, long);
    let (mut emitted, mut refused);
    let mut step = 1;
    if record(case, probe).is_some() {
        emitted = probe;
        refused = loop {
            if emitted == long {
                return Count::Long(long);
            }
            let next = (emitted + step).min(long);
            if record(case, next).is_none() {
                break next;
            }
            emitted = next;
            step *= 2;
        };
    } else {
        refused = probe;
        // Twice SHORT was emitted, so this ends there at the latest.
        emitted = loop {
            let next = refused.saturating_sub(step).max(2 * SHORT);
            if record(case, next).is_some() {
                break next;
            }
            refused = next;
            step *= 2;
        };
    }
    while refused - emitted > 1 {
        let mid = emitted + (refused - emitted) / 2;
        if record(case, mid).is_some() {
            emitted = mid;
        } else {
            refused = mid;
        }
    }
    Count::Bound(emitted)
}

/// What a child reports of its case: how deep the record is, and whether each
/// stage ran to the end rather than panicking.
#[derive(Debug, PartialEq, Eq)]
struct Facts {
    depth: usize,
    fcs_normalise: bool,
    our_normalise: bool,
    ranges: bool,
    recovery: bool,
}

impl Facts {
    fn spell(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.depth, self.fcs_normalise, self.our_normalise, self.ranges, self.recovery
        )
    }

    fn read(s: &str) -> Facts {
        let fields: Vec<&str> = s.split(' ').collect();
        let [depth, fcs_normalise, our_normalise, ranges, recovery] = fields[..] else {
            panic!("five fields: {s:?}")
        };
        let flag = |f: &str| f.parse::<bool>().expect("a bool");
        Facts {
            depth: depth.parse().expect("a depth"),
            fcs_normalise: flag(fcs_normalise),
            our_normalise: flag(our_normalise),
            ranges: flag(ranges),
            recovery: flag(recovery),
        }
    }

    fn complete(&self) -> bool {
        self.fcs_normalise && self.our_normalise && self.ranges && self.recovery
    }

    /// The case's manifest entry.
    fn entry(&self, case: &Case, count: Count) -> String {
        let stage = |ran: bool| if ran { "ran" } else { "panicked" };
        format!(
            "{}\t{} repeats ({}), depth {}, fcs-normalise {}, our-normalise {}, ranges {}, \
             recovery {}",
            case.name(),
            count.repeats(),
            count.spell(),
            self.depth,
            stage(self.fcs_normalise),
            stage(self.our_normalise),
            stage(self.ranges),
            stage(self.recovery),
        )
    }
}

/// Child entry: run one case's stages on a thread of the stack the parent
/// asked for, then report what ran.
///
/// The case arrives as `<index>\t<stack bytes>\t<source path>\t<record path>`:
/// the parent asked fcs-dump for the record, so the child needs no oracle.
/// Our normaliser reads the tallest tree our parser returns cleanly for the
/// shape ([`tallest_clean_count`]), since that, not the record, bounds the
/// stack our side needs. (A tree the parser stopped at its height bound is
/// cut off where it stopped, which for a chain built in a loop is its root,
/// so the normaliser would give up before descending at all.)
#[test]
#[ignore = "child-process entry for fcs_record_stack_bound"]
fn probe_child() {
    let Some(spec) = stack_probe::requested_case(CASE_VAR) else {
        return;
    };
    let [index, stack_bytes, source, record] = spec.split('\t').collect::<Vec<_>>()[..] else {
        panic!("four fields: {spec:?}")
    };
    let case = cases()[index.parse::<usize>().expect("a case index")];
    let stack_bytes: usize = stack_bytes.parse().expect("a stack size");
    let src = std::fs::read_to_string(source).expect("read the case's source");
    let json = std::fs::read_to_string(record).expect("read the case's record");
    let facts = on_stack_marked_deep(stack_bytes, move || {
        let depth = json_depth(&json);
        // Read once outside the stages, so a malformed record fails the case
        // rather than reading as a stage that panicked.
        drop(read_fcs_record(&json));
        let symbols: HashSet<String> = ["COMPILED", "EDITING"].map(String::from).into();
        let parse_of = |src: &str| match case.shape.kind {
            Kind::Impl => parse_with_symbols(src, &symbols),
            Kind::Sig => parse_sig_with_symbols(src, &symbols),
        };
        let parse = parse_of(&src);
        let clean = |src: &str| parse_of(src).errors.is_empty();
        let tallest = parse_of(&case.source(tallest_clean_count(&case, clean)));
        let ran = |f: &dyn Fn()| catch_unwind_silent(f).is_ok();
        Facts {
            depth,
            fcs_normalise: ran(&|| drop(normalise_fcs_dump(&json))),
            our_normalise: ran(&|| drop(normalise_parse(&tallest))),
            ranges: ran(&|| drop(ast_ranges_match(&parse, &json, &src))),
            recovery: ran(&|| drop(grade(&parse, &symbols, &json, &src))),
        }
    });
    stack_probe::report(&facts.spell());
}

#[test]
fn every_reader_fits_half_the_deep_stack() {
    let cases = cases();
    let runs = par_map(cases.len(), |i| {
        let case = &cases[i];
        let count = chosen_count(case);
        let n = count.repeats();
        let source = source_file(case.shape.kind, &case.source(n));
        let json = record(case, n).expect("the chosen count's record");
        if let Count::Bound(_) = count {
            // fcs-dump refused `n + 1` repeats. Were that the depth limit, one
            // more repeat's worth of nesting reaches it (a record that reaches
            // it with a value in its deepest container is refused).
            let depth = json_depth(&json);
            let shorter = json_depth(&record(case, n - 1).expect("a shorter record"));
            assert!(
                depth + (depth - shorter) >= FCS_DUMP_MAX_DEPTH,
                "{}: fcs-dump refused {} repeats, but {n} gave a record only {depth} \
                 deep ({shorter} at {}): the refusal was not the depth limit",
                case.name(),
                n + 1,
                n - 1
            );
        }
        let record = source_file(case.shape.kind, &json);
        let spec = format!(
            "{i}\t{PROBE_STACK_BYTES}\t{}\t{}",
            source.path().display(),
            record.path().display()
        );
        (count, stack_probe::run_case(CHILD, CASE_VAR, &spec))
    });
    let mut overflowed = Vec::new();
    let mut entries = Vec::new();
    let mut deepest_complete = 0;
    for (case, (count, outcome)) in cases.iter().zip(runs) {
        match outcome {
            Outcome::Overflowed => overflowed.push(case.name()),
            Outcome::Survived(facts) => {
                let facts = Facts::read(&facts);
                assert!(
                    facts.depth <= FCS_DUMP_MAX_DEPTH,
                    "{}: fcs-dump emitted a record {} deep, past FCS_DUMP_MAX_DEPTH",
                    case.name(),
                    facts.depth
                );
                if facts.complete() {
                    deepest_complete = deepest_complete.max(facts.depth);
                }
                entries.push(facts.entry(case, count));
            }
        }
    }
    assert!(
        overflowed.is_empty(),
        "these cases overflowed a {} MiB stack, half of DEEP_STACK_BYTES: some reader's \
         cost per level has outgrown the margin (shrink the frame on its recursion, or \
         raise DEEP_STACK_BYTES):\n{}",
        PROBE_STACK_BYTES >> 20,
        overflowed.join("\n")
    );
    // Which stages ran to the end on each case is pinned exactly: a reader that
    // gives up near the top survives any stack, so a case it stopped reaching
    // would otherwise pass this test vacuously.
    check_manifest(
        "fcs_record_stack_bound",
        &Manifest::from_entries(entries).unwrap_or_else(|e| panic!("manifest entry: {e}")),
        &regenerate("fcs_record_stack_bound::every_reader_fits_half_the_deep_stack"),
    );
    assert!(
        deepest_complete >= REACH,
        "no case whose every stage ran reached the depth the bound is about: deepest \
         {deepest_complete}, against {REACH}"
    );
}
