//! The reuse oracle: an evaluation that asks each query once
//! ([`super::Observer::new`]) and reuses parsed imports
//! ([`super::ImportDocuments`]) equals the reference, which asks every query
//! every time and parses every file it reads
//! ([`super::Observer::asking_every_time`]).
//!
//! The two must agree on everything but the counts, which is what reuse
//! changes: the reused evaluation asks each query it records exactly once,
//! and never more often than the reference did.
//!
//! This is test support. Every helper in the crate's with-imports tests holds
//! its evaluations to it at every step of the soundness sweep, and the
//! integration tests hold theirs to it through their shared entry point.

use super::{Observation, ReadCounts, ReadSet};
use crate::ParsedProject;

/// `reused`, once it is `reference` in all but its counts, and its counts are
/// one query per read in its read set, none more than the reference asked.
///
/// Panics on a difference.
pub fn same_as_reference(reference: ParsedProject, reused: ParsedProject) -> ParsedProject {
    assert!(
        reused
            == ParsedProject {
                read_counts: reused.read_counts,
                ..reference.clone()
            },
        "reusing parses and answers changed the evaluation:\nreference: {reference:#?}\nreused: {reused:#?}"
    );
    let once = one_query_per_read(&reused.read_set);
    assert_eq!(
        reused.read_counts, once,
        "the evaluation asked some query more than once"
    );
    let asked = reference.read_counts;
    assert!(
        once.opens <= asked.opens
            && once.stats <= asked.stats
            && once.listings <= asked.listings
            && once.canonicalizations <= asked.canonicalizations,
        "the reference asked fewer queries ({asked:?}) than its distinct reads ({once:?})"
    );
    reused
}

/// The counts of an evaluation that asked each query in `reads` once.
pub fn one_query_per_read(reads: &ReadSet) -> ReadCounts {
    let mut counts = ReadCounts::default();
    for read in reads.reads() {
        match read.observation {
            Observation::Text(_) => counts.opens += 1,
            Observation::Kind(_) | Observation::LinkKind(_) | Observation::Stamp(_) => {
                counts.stats += 1;
            }
            Observation::Listing(_) => counts.listings += 1,
            Observation::Canonical(_) => counts.canonicalizations += 1,
        }
    }
    counts
}
