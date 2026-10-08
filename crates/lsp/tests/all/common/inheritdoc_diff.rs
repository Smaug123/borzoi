//! The `<inheritdoc>` differential: every inheritdoc-bearing entry of one
//! assembly, expanded by `xml_doc::inherit` over the env and by Roslyn's IDE
//! over a compilation of exactly the same references, compared under
//! certain-implies-exact — an entry we expand must equal Roslyn's expansion,
//! node for node; a decline makes no claim.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use borzoi::xml_doc::file::DocEntry;
use borzoi::xml_doc::inherit::{Outcome, expand};
use borzoi::xml_doc::key::{DocIdIndex, DocTarget};
use borzoi::xml_doc::lookup::{DocSources, xml_path_for};
use borzoi::xml_doc::tree::{DocElement, DocNode};
use borzoi_sema::AssemblyEnv;

use super::inheritdoc_oracle::{Expanded, oracle};

/// One entry's comparison.
#[derive(Debug, Clone)]
pub struct Compared {
    pub key: String,
    pub verdict: Verdict,
}

#[derive(Debug, Clone)]
pub enum Verdict {
    /// We expanded it, exactly as Roslyn does.
    Agrees,
    /// We expanded it, differently from Roslyn: a soundness failure.
    Disagrees { ours: String, roslyn: String },
    /// We declined (the cause, as a census bucket); what Roslyn's own
    /// expansion did with the element, for the census.
    Declined { cause: String, roslyn_left_it: bool },
    /// No unique start symbol on one side or the other.
    NoStart(String),
}

/// Whether the tree holds anything that might be an `<inheritdoc>`: a looser
/// test than the expansion's own (any namespace, any case, Unicode lookalikes),
/// so an entry the expansion wrongly passes over still gets compared.
fn has_inheritdoc(e: &DocElement) -> bool {
    e.children.iter().any(|c| match c {
        DocNode::Element(c) => looks_like_inheritdoc(&c.name) || has_inheritdoc(c),
        DocNode::Text(_) => false,
    })
}

fn looks_like_inheritdoc(name: &str) -> bool {
    name.to_lowercase().replace('ı', "i") == "inheritdoc"
}

fn parse(xml: &str) -> DocElement {
    let doc =
        roxmltree::Document::parse(xml).unwrap_or_else(|e| panic!("Roslyn's XML: {e}: {xml}"));
    DocElement::from_roxmltree(doc.root_element())
        .expect("Roslyn's expansion within the depth bound")
        .normalized()
}

/// A short census bucket for a decline: the variant path of its `Debug`
/// form, payload stripped of anything instance-specific.
fn bucket(why: &borzoi::xml_doc::inherit::Decline) -> String {
    let full = format!("{why:?}");
    match full.find(['{', '"']) {
        Some(i) => full[..i].trim_end_matches(['(', ' ']).to_string(),
        None => full,
    }
}

/// Compare every inheritdoc-bearing entry of `dll` (one of `references`,
/// all of which `env` was built from).
pub fn compare_assembly(
    env: &Arc<AssemblyEnv>,
    sources: &mut DocSources,
    references: &[PathBuf],
    dll: &Path,
) -> Vec<Compared> {
    let Ok(file) = sources.files.load(&xml_path_for(dll)) else {
        return Vec::new();
    };
    let mut keys: Vec<String> = file
        .keys()
        .filter(|k| match file.entry(k) {
            Some(DocEntry::Unique(range)) => file
                .member_element(range.clone())
                .is_ok_and(|m| has_inheritdoc(&m)),
            _ => false,
        })
        .map(str::to_string)
        .collect();
    if keys.is_empty() {
        return Vec::new();
    }
    keys.sort();
    let roslyn = oracle().lock().unwrap().expand(references, dll, &keys);
    // The assembly's own targets: the start of each comparison is a symbol of
    // `dll` (an env-wide index is built only if a `cref` asks for one).
    let index = DocIdIndex::of_assembly(env, dll);
    let mut out = Vec::with_capacity(keys.len());
    for (key, theirs) in keys.into_iter().zip(roslyn) {
        let mine: Vec<DocTarget> = index
            .targets(&key)
            .iter()
            .copied()
            .filter(|t| env.assembly_path(t.owner()) == Some(dll))
            .collect();
        let verdict = match (mine.as_slice(), &theirs) {
            ([target], Expanded::Ok { xml, .. }) => match sources.locate(env, *target) {
                Err(miss) => Verdict::NoStart(format!("ours: {miss:?}")),
                Ok(located) => {
                    let expansion = expand(sources, env, *target, located);
                    match expansion.outcome {
                        // "Nothing to expand" is a claim too — that Roslyn
                        // leaves the entry as it is — and this harness picks
                        // entries by a looser test than the expansion's own,
                        // so a spelling the expansion misses is caught here.
                        Outcome::Inherited | Outcome::NoInheritdoc => {
                            let roslyn = parse(xml);
                            if roslyn == expansion.member.clone().normalized() {
                                Verdict::Agrees
                            } else {
                                Verdict::Disagrees {
                                    ours: format!("{:#?}", expansion.member),
                                    roslyn: xml.clone(),
                                }
                            }
                        }
                        Outcome::Declined(why) => Verdict::Declined {
                            cause: bucket(&why),
                            roslyn_left_it: has_inheritdoc(&parse(xml)),
                        },
                    }
                }
            },
            ([], Expanded::Ok { .. }) => Verdict::NoStart("ours: no target".into()),
            ([_, ..], Expanded::Ok { .. }) => Verdict::NoStart("ours: several targets".into()),
            (ours, other) => {
                Verdict::NoStart(format!("roslyn: {other:?}, ours: {} target(s)", ours.len()))
            }
        };
        out.push(Compared { key, verdict });
    }
    out
}

/// A census of comparisons: counts per verdict and per decline cause.
#[derive(Debug, Default)]
pub struct Census {
    pub agrees: usize,
    pub disagrees: Vec<(String, String, String)>,
    pub declined: BTreeMap<String, (usize, usize)>,
    pub no_start: BTreeMap<String, usize>,
    /// Up to three keys per decline cause and per no-start reason.
    pub samples: BTreeMap<String, Vec<String>>,
}

impl Census {
    pub fn add(&mut self, compared: &[Compared]) {
        for c in compared {
            match &c.verdict {
                Verdict::Agrees => self.agrees += 1,
                Verdict::Disagrees { ours, roslyn } => {
                    self.disagrees
                        .push((c.key.clone(), ours.clone(), roslyn.clone()));
                }
                Verdict::Declined {
                    cause,
                    roslyn_left_it,
                } => {
                    let slot = self.declined.entry(cause.clone()).or_default();
                    slot.0 += 1;
                    let s = self.samples.entry(cause.clone()).or_default();
                    if s.len() < 3 {
                        s.push(c.key.clone());
                    }
                    if *roslyn_left_it {
                        slot.1 += 1;
                    }
                }
                Verdict::NoStart(why) => {
                    *self.no_start.entry(why.clone()).or_default() += 1;
                    let s = self.samples.entry(why.clone()).or_default();
                    if s.len() < 3 {
                        s.push(c.key.clone());
                    }
                }
            }
        }
    }

    /// Fold another census into this one.
    pub fn merge(&mut self, other: Census) {
        self.agrees += other.agrees;
        self.disagrees.extend(other.disagrees);
        for (k, (n, l)) in other.declined {
            let slot = self.declined.entry(k).or_default();
            slot.0 += n;
            slot.1 += l;
        }
        for (k, n) in other.no_start {
            *self.no_start.entry(k).or_default() += n;
        }
        for (k, keys) in other.samples {
            let s = self.samples.entry(k).or_default();
            s.extend(keys);
            s.truncate(3);
        }
    }

    pub fn declined_total(&self) -> usize {
        self.declined.values().map(|(n, _)| n).sum()
    }

    pub fn print(&self, title: &str) {
        eprintln!("== {title}");
        eprintln!("  expanded, agreeing with Roslyn: {}", self.agrees);
        eprintln!("  expanded, DISAGREEING: {}", self.disagrees.len());
        eprintln!(
            "  declined: {} (cause: count / of which Roslyn also left an <inheritdoc>)",
            self.declined_total()
        );
        for (cause, (n, left)) in &self.declined {
            eprintln!(
                "    {cause}: {n} / {left}  e.g. {:?}",
                self.samples.get(cause)
            );
        }
        eprintln!(
            "  no unique start: {}",
            self.no_start.values().sum::<usize>()
        );
        for (why, n) in &self.no_start {
            eprintln!("    {why}: {n}  e.g. {:?}", self.samples.get(why));
        }
    }

    /// Panic, showing the first few, if any expansion disagreed.
    pub fn assert_sound(&self) {
        if let Some((key, ours, roslyn)) = self.disagrees.first() {
            panic!(
                "{} expansion(s) disagree with Roslyn; first, {key}:\nours: {ours}\nroslyn: {roslyn}",
                self.disagrees.len()
            );
        }
    }
}
