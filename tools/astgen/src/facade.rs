//! The per-version typed facades (`docs/ast-versioning-plan.md` D4, D6, D8):
//! one module per `LanguageVersion`, each owning every node type and enum of
//! its surface.
//!
//! A facade is generated from its **lock** (`crates/cst/facades/<module>.lock`,
//! [`crate::lock`]) and the union's lock, and nothing else. For an unpublished
//! facade the lock is itself derived from the union's ([`derived_lock`]):
//! every type whose kinds the feature table's live `Kind` rows admit at the
//! version, and every accessor whose owner and return type survive.
//!
//! A **published** facade ([`PUBLISHED`]) is frozen. Its lock is derived and
//! written once, stamped with [`FREEZE_EPOCH`], and from then on astgen reads
//! it and never writes it ([`Standing`]); a consumer fixture written beside it
//! ([`crate::consumer`]) is never rewritten either. Each generation still
//! derives the surface the version would have today and holds the frozen lock
//! to it ([`facade_locks`]): an accessor or derive the union has gained is
//! the frozen facade's to opt into ([`is_opt_in`]), and any other difference,
//! which would break the facade or leave a hole in it, fails, naming each
//! line, until the lock takes it or [`ACCEPTED_DIVERGENCES`] records why the
//! frozen facade keeps its surface. `tools/ci/facade-lock-check.sh`
//! holds the lock's history to the same allowed additions.
//!
//! A version whose derived lock equals the one before it is an **alias**: its
//! lock is `alias-of <module>` and its module is `pub use super::<module>::*`,
//! the same types. `preview` is never an alias and never aliased, so a stable
//! facade's `Facade::VERSIONS` never names the unstable preview version, and
//! an unpublished version never aliases a published one, so a frozen
//! facade's `VERSIONS` never follows an unpublished version.
//!
//! In the code, a node is a newtype over the union's node, and an accessor is
//! one line: call the union accessor its lock line delegates to, and re-cast
//! the result into the facade's types. A re-cast of a node or dispatch enum is
//! taken to be able to fail even where the union holds nothing more today, so
//! the code depends on the union only for whether it still backs the lock, and
//! the union's growth leaves a frozen facade's code byte-identical. A failed
//! re-cast drops the element: an `Option` becomes `None`, an iterator or `Vec`
//! skips it, which is how a node outside the surface reads as an absent child.
//! An accessor returning such a type bare cannot drop it, and the generator
//! refuses it, as it refuses a facade type or accessor with no union
//! counterpart: those need a hand-written compat shim.

use std::collections::{BTreeMap, BTreeSet};

use crate::consumer;
use crate::features::{self, FeatureRow};
use crate::lock::{self, Line, Lock, Payload};
use crate::shape::{Elem, Names, Shape};
use crate::{CATEGORIES, reformat};

/// One `LanguageVersion` and its facade module.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Version {
    /// The table's spelling, `"4.6"` … `"11.0"`, `"preview"`.
    pub text: &'static str,
    /// The `LanguageVersion` variant, `"V4_6"`.
    pub variant: &'static str,
    /// FCS's decimal for it (`preview` is 9999).
    pub decimal: f64,
}

impl Version {
    /// The facade module: `v4_6`, `v5`, `v10`, `preview`.
    pub fn module(&self) -> String {
        if self.is_preview() {
            return "preview".to_string();
        }
        let text = self.text.strip_suffix(".0").unwrap_or(self.text);
        format!("v{}", text.replace('.', "_"))
    }

    /// The cargo feature that exposes the module: the module's name, or
    /// `unstable-preview` for preview.
    pub fn cargo_feature(&self) -> String {
        if self.is_preview() {
            "unstable-preview".to_string()
        } else {
            self.module()
        }
    }

    /// Whether this is FCS's `preview`.
    pub fn is_preview(&self) -> bool {
        self.text == "preview"
    }

    /// The facade's lock, repo-relative.
    pub fn lock_path(&self) -> String {
        format!("crates/cst/facades/{}.lock", self.module())
    }

    /// The facade's code, repo-relative.
    pub fn code_path(&self) -> String {
        format!(
            "crates/cst/src/syntax/generated/facades/{}.rs",
            self.module()
        )
    }
}

/// Every language version, ascending: one facade each.
pub fn versions() -> Vec<Version> {
    features::VERSIONS
        .iter()
        .map(|&(text, variant, decimal)| Version {
            text,
            variant,
            decimal,
        })
        .collect()
}

/// The published facades, by module. A published facade is frozen: astgen
/// generates it from its checked-in lock, which it reads and never writes
/// (once [`FREEZE_EPOCH`] has stamped it), and `tools/ci/facade-lock-check.sh`
/// lets the lock change only by the additions a consumer cannot observe as a
/// break. Each of these modules has a frozen consumer fixture
/// (`crates/cst/tests/all/facade_consumers/`) if it is a distinct facade.
///
/// The numbered versions up to F# 10.0, the compiler's default, are published;
/// `v11` waits for the compiler upgrade that completes its surface, and
/// `preview` is never published.
pub const PUBLISHED: &[&str] = &["v4_6", "v4_7", "v5", "v6", "v7", "v8", "v9", "v10"];

/// The freeze epoch: the only route by which a published facade's lock is
/// re-baselined rather than evolved. Every published lock and consumer
/// fixture records the epoch it was written at (`# freeze-epoch: N`); one
/// older than this constant is derived and written afresh by the next
/// generation, as at its first publication. Bumping it therefore rewrites
/// every published lock and fixture, and the CI lock check waives its rules
/// for a change that bumps it. That is allowed only until the first external
/// release of `borzoi-cst`; after it a published surface only grows.
pub const FREEZE_EPOCH: u32 = 1;

/// A difference between a frozen facade's lock and the surface astgen derives
/// for its version today, kept deliberately: the frozen facade keeps its
/// surface, and this records why. [`facade_locks`] refuses any divergence that
/// is neither accepted here nor reconciled by appending lines to the lock, and
/// any accepted one that no longer diverges.
#[derive(Debug, Clone, Copy)]
pub struct Accepted {
    /// The facade module whose lock diverges.
    pub module: &'static str,
    /// The diverging lock line, as rendered (tab-separated).
    pub line: &'static str,
    /// Why the frozen facade keeps its surface.
    pub reason: &'static str,
}

/// Every accepted divergence of a frozen facade ([`Accepted`]).
pub const ACCEPTED_DIVERGENCES: &[Accepted] = &[];

/// What freezes the facades: which are published, at which epoch, and which
/// divergences of theirs are accepted. [`CURRENT_FREEZE`] is the checked-in
/// one; tests build others.
#[derive(Debug, Clone, Copy)]
pub struct Freeze<'a> {
    pub published: &'a [&'a str],
    pub epoch: u32,
    pub accepted: &'a [Accepted],
}

/// [`PUBLISHED`], [`FREEZE_EPOCH`] and [`ACCEPTED_DIVERGENCES`].
pub const CURRENT_FREEZE: Freeze<'static> = Freeze {
    published: PUBLISHED,
    epoch: FREEZE_EPOCH,
    accepted: ACCEPTED_DIVERGENCES,
};

/// How a facade's lock is produced by a generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Unpublished: derived from the union's lock and written every time.
    Derived,
    /// Published, and stamped by this generation: derived, and written with
    /// the current epoch, beside a fresh consumer fixture if it is a distinct
    /// facade. A published facade is in this standing exactly when its lock
    /// is missing, carries no epoch, or carries an older one.
    Publishing,
    /// Published and stamped at the current epoch: read from its lock, which
    /// is never written.
    Frozen,
}

/// What a generation reads.
pub struct Inputs<'a> {
    /// The feature table's rows.
    pub rows: &'a [FeatureRow],
    /// The union's lock.
    pub union: &'a Lock,
    /// Every language version, ascending; one facade each.
    pub versions: &'a [Version],
    pub freeze: Freeze<'a>,
    /// A facade's lock text as checked in, if it has one.
    pub on_disk: &'a dyn Fn(&Version) -> Option<String>,
}

/// Every facade's lock, ascending by version, with how this generation
/// produces it.
#[derive(Debug, Clone, PartialEq)]
pub struct Generation {
    pub locks: Vec<(Version, Lock)>,
    pub standings: Vec<Standing>,
    /// `(module, line)`: each line a frozen head facade may opt into
    /// ([`is_opt_in`]) by appending it to its lock, which it does not offer
    /// until then.
    pub opt_in: Vec<(String, Line)>,
    /// `(module, line)`: each divergence of a frozen facade that
    /// [`ACCEPTED_DIVERGENCES`] accepts. The property suite exempts what each
    /// one touches from the facade's comparison with the union and the table
    /// ([`exemptions`]).
    pub accepted: Vec<(String, Line)>,
}

/// The facade module declarations, repo-relative.
pub const FACADES_MOD_PATH: &str = "crates/cst/src/syntax/generated/facades/mod.rs";

/// Whether `node`'s kinds are all legal at `lang` by the live `Kind` rows.
fn kinds_in_surface(rows: &[FeatureRow], kinds: &[&String], lang: f64) -> bool {
    kinds
        .iter()
        .all(|k| features::live_kind_introduced(rows, k).is_none_or(|intro| lang >= intro))
}

/// The lock of `union` restricted to what is legal at `lang`: every node all
/// of whose kinds are, the enum and value-enum variants over what survives,
/// and the accessors whose owner and return type survive, each delegating to
/// the union accessor of its own name.
pub fn derived_lock(union: &Lock, rows: &[FeatureRow], lang: f64) -> Lock {
    let mut node_kinds: BTreeMap<&String, Vec<&String>> = BTreeMap::new();
    for line in &union.lines {
        if let Line::Node { node, kind } = line {
            node_kinds.entry(node).or_default().push(kind);
        }
    }
    let nodes: BTreeSet<&String> = node_kinds
        .iter()
        .filter(|(_, kinds)| kinds_in_surface(rows, kinds, lang))
        .map(|(node, _)| *node)
        .collect();
    let mut lines: BTreeSet<Line> = union
        .lines
        .iter()
        .filter(|line| match line {
            Line::Node { node, .. } => nodes.contains(node),
            Line::Enum { node, .. } => nodes.contains(node),
            _ => false,
        })
        .cloned()
        .collect();
    // A value-enum variant survives when every type its payload names does,
    // and a value enum may carry another, so iterate to the fixpoint.
    loop {
        let names = Lock {
            lines: lines.clone(),
        }
        .names();
        let before = lines.len();
        for line in &union.lines {
            if let Line::Value { payload, .. } = line
                && payload_resolves(payload, &names)
            {
                lines.insert(line.clone());
            }
        }
        if lines.len() == before {
            break;
        }
    }
    let names = Lock {
        lines: lines.clone(),
    }
    .names();
    for line in &union.lines {
        match line {
            Line::Derive { name, .. } if names.values.contains(name) => {
                lines.insert(line.clone());
            }
            Line::Accessor {
                owner, name, shape, ..
            } if names.nodes.contains(owner) && names.resolves(shape) => {
                lines.insert(Line::Accessor {
                    owner: owner.clone(),
                    name: name.clone(),
                    shape: shape.clone(),
                    delegate: Some(name.clone()),
                });
            }
            _ => {}
        }
    }
    Lock { lines }
}

fn payload_resolves(payload: &Payload, names: &Names) -> bool {
    match payload {
        Payload::Unit => true,
        Payload::Tuple(shapes) => shapes.iter().all(|s| names.resolves(s)),
        Payload::Named(fields) => fields.iter().all(|(_, s)| names.resolves(s)),
    }
}

/// The checked-in lock text of `version`, if it has one.
pub fn checked_in_lock(version: &Version) -> Option<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(version.lock_path());
    std::fs::read_to_string(path).ok()
}

/// Why a frozen facade's lock and today's derived surface differ by one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drift {
    /// Derived but not locked: a kind the frozen facade does not cast (a new
    /// kind for one of its nodes, or a new node type). Nodes of that kind
    /// would read through it as holes no gate diagnostic covers.
    Hole,
    /// Derived but not locked, and no allowed evolution adds it: a new
    /// variant of an exhaustively matched enum.
    Blocked,
    /// Locked but no longer derived.
    Departed,
    /// The version is frozen as an alias, but its derived surface and its
    /// head's differ by this line.
    AliasBroken,
}

/// Whether a lock line may be added to a published lock: a new accessor, a
/// new kind for a node (or a new node type, which no frozen enum holds), or a
/// new trait on a value enum. `tools/ci/facade-lock-check.sh` allows exactly
/// these tags.
pub fn is_allowed_addition(line: &Line) -> bool {
    matches!(
        line,
        Line::Accessor { .. } | Line::Node { .. } | Line::Derive { .. }
    )
}

/// Whether a line derived for a frozen facade but missing from its lock is
/// the facade's to opt into, rather than a divergence: a new accessor or a
/// new trait on a value enum. Leaving it out breaks no caller and leaves no
/// node unreadable; the frozen facade just does not offer it until the line
/// is appended. A new kind is not opt-in: left out, it is a hole.
pub fn is_opt_in(line: &Line) -> bool {
    matches!(line, Line::Accessor { .. } | Line::Derive { .. })
}

/// An accessor line a frozen lock keeps for a renamed union accessor: its
/// delegate names another method, which the derivation never does.
fn is_compat_line(line: &Line) -> bool {
    matches!(line, Line::Accessor { name, delegate: Some(d), .. } if d != name)
}

/// The divergence of the frozen head facade's `locked` surface from today's
/// `derived` one, and the derived lines it may opt into ([`is_opt_in`]).
fn head_drift(locked: &Lock, derived: &Lock) -> (Vec<(Line, Drift)>, Vec<Line>) {
    let mut drift = Vec::new();
    let mut opt_in = Vec::new();
    for line in derived.lines.difference(&locked.lines) {
        if is_opt_in(line) {
            opt_in.push(line.clone());
        } else if matches!(line, Line::Node { .. }) {
            drift.push((line.clone(), Drift::Hole));
        } else {
            drift.push((line.clone(), Drift::Blocked));
        }
    }
    for line in locked.lines.difference(&derived.lines) {
        if !is_compat_line(line) {
            drift.push((line.clone(), Drift::Departed));
        }
    }
    (drift, opt_in)
}

/// The explanation a frozen facade's unaccepted divergence fails with.
fn drift_report(version: &Version, alias_of: Option<&str>, drift: &[(Line, Drift)]) -> String {
    let module = version.module();
    let mut out = match alias_of {
        Some(head) => format!(
            "the published `{module}` facade is frozen as an alias of `{head}`, but the \
             surface astgen derives for {} no longer equals {head}'s:\n",
            fsharp(version)
        ),
        None => format!(
            "the published `{module}` facade's lock ({}) is frozen, but it no longer \
             matches the surface astgen derives for {}:\n",
            version.lock_path(),
            fsharp(version)
        ),
    };
    for (kind, heading) in [
        (
            Drift::Hole,
            "derived but not locked; a kind the frozen facade would read as a hole that no \
             gate covers, so append it to the lock (an allowed addition, in its sorted \
             place) or accept it:",
        ),
        (
            Drift::Blocked,
            "derived but not locked; no allowed evolution adds it (a new variant breaks \
             every exhaustive match on its enum):",
        ),
        (Drift::Departed, "locked but no longer derived:"),
        (Drift::AliasBroken, "in one surface and not the other:"),
    ] {
        let lines: Vec<String> = drift
            .iter()
            .filter(|(_, d)| *d == kind)
            .map(|(l, _)| format!("    {}", l.render()))
            .collect();
        if !lines.is_empty() {
            out.push_str(&format!("  {heading}\n{}\n", lines.join("\n")));
        }
    }
    out.push_str(
        "  A published lock is the generator's input: astgen reads it and never writes \
         it, and `tools/ci/facade-lock-check.sh` lets it change only by added `accessor`, \
         `node` and `derive` lines. For each line above that the lock does not take, add \
         an `Accepted` entry to `ACCEPTED_DIVERGENCES` (tools/astgen/src/facade.rs) saying \
         why the frozen facade keeps its surface; a union accessor the facade still needs \
         under a new name takes a delegate (`union.<new>`) in place of an entry. Until the \
         first external release, bumping `FREEZE_EPOCH` instead re-derives every published \
         lock (docs/ast-versioning-plan.md, \"Publishing a facade\").",
    );
    out
}

/// Every facade's lock, ascending by version, and how the generation
/// produces each ([`Standing`]); or every reason the freeze is broken.
///
/// A numbered version whose lock equals the current run's head lock is an
/// alias of it, unless it is frozen, when its lock says what it is, or it is
/// unpublished and the head is published: an alias's versions are its head's
/// `Facade::VERSIONS`, so an unpublished alias of a frozen head would tie
/// that frozen code to whatever the unpublished version derives. A frozen
/// facade's lock is held to today's derivation: a frozen head's to its own
/// version's, every difference appended to the lock or accepted
/// ([`Accepted`]) except the lines it may opt into ([`is_opt_in`]), which
/// [`Generation::opt_in`] lists; a frozen alias's derived surface to its
/// head's.
pub fn facade_locks(inputs: &Inputs<'_>) -> Result<Generation, String> {
    let Inputs {
        rows,
        union,
        versions,
        freeze,
        on_disk,
    } = inputs;
    let mut errors: Vec<String> = Vec::new();
    for module in freeze.published {
        match versions.iter().find(|v| v.module() == *module) {
            None => errors.push(format!("PUBLISHED names `{module}`, which is no facade")),
            Some(v) if v.is_preview() => {
                errors.push("`preview` is never published".to_string());
            }
            Some(_) => {}
        }
    }
    let mut locks: Vec<(Version, Lock)> = Vec::new();
    let mut standings: Vec<Standing> = Vec::new();
    let mut used_acceptances: BTreeSet<(String, String)> = BTreeSet::new();
    let mut reports: Vec<String> = Vec::new();
    let mut opt_in: Vec<(String, Line)> = Vec::new();
    let mut accepted_lines: Vec<(String, Line)> = Vec::new();
    // The module, lock and standing of the current run of equal surfaces.
    let mut head: Option<(String, Lock, Standing)> = None;
    for version in versions.iter() {
        let module = version.module();
        let text = on_disk(version);
        let epoch = match text.as_deref().map(lock::freeze_epoch).transpose() {
            Ok(epoch) => epoch.flatten(),
            Err(e) => {
                errors.push(format!("{}: {e}", version.lock_path()));
                None
            }
        };
        let published = freeze.published.contains(&module.as_str());
        let standing = match (published, epoch) {
            (true, Some(e)) if e == freeze.epoch => Standing::Frozen,
            (true, Some(e)) if e > freeze.epoch => {
                errors.push(format!(
                    "{} is stamped at freeze epoch {e}, after FREEZE_EPOCH ({})",
                    version.lock_path(),
                    freeze.epoch
                ));
                Standing::Publishing
            }
            (true, _) => Standing::Publishing,
            (false, Some(e)) if e >= freeze.epoch => {
                errors.push(format!(
                    "{} is published (freeze epoch {e}), but `{module}` is not in PUBLISHED: \
                     a published facade is withdrawn only by bumping FREEZE_EPOCH",
                    version.lock_path()
                ));
                Standing::Derived
            }
            (false, _) => Standing::Derived,
        };
        let derived = derived_lock(union, rows, version.decimal);
        let lock = match standing {
            Standing::Frozen => match lock::parse(text.as_deref().unwrap_or_default()) {
                Ok(lock) => lock,
                Err(e) => {
                    errors.push(format!("{}: {e}", version.lock_path()));
                    continue;
                }
            },
            Standing::Derived | Standing::Publishing => derived.clone(),
        };
        let incoherent = lock.check();
        if !incoherent.is_empty() {
            errors.push(format!(
                "the `{module}` facade's lock is incoherent:\n  {}",
                incoherent.join("\n  ")
            ));
            continue;
        }
        let alias = match standing {
            Standing::Frozen => lock.alias_of().map(str::to_string),
            Standing::Derived | Standing::Publishing => head
                .as_ref()
                .filter(|(_, head_lock, head_standing)| {
                    !version.is_preview()
                        && *head_lock == lock
                        && (standing == Standing::Publishing || *head_standing == Standing::Derived)
                })
                .map(|(module, _, _)| module.clone()),
        };
        if let Some(target) = &alias {
            let head_standing = locks
                .iter()
                .zip(&standings)
                .find(|((v, l), _)| v.module() == *target && l.alias_of().is_none());
            match head_standing {
                None => errors.push(format!(
                    "`{module}` is an alias of `{target}`, which is no earlier distinct facade"
                )),
                Some((_, Standing::Derived)) if published => errors.push(format!(
                    "the published `{module}` is an alias of `{target}`, which is not \
                     published: an alias's types are its head's, so the head must be \
                     frozen too"
                )),
                Some(_) => {}
            }
        }
        if standing == Standing::Frozen {
            let drift: Vec<(Line, Drift)> = match &alias {
                Some(target) => {
                    let head_version = versions.iter().find(|v| v.module() == *target);
                    match head_version {
                        Some(hv) => {
                            let head_derived = derived_lock(union, rows, hv.decimal);
                            derived
                                .lines
                                .symmetric_difference(&head_derived.lines)
                                .map(|l| (l.clone(), Drift::AliasBroken))
                                .collect()
                        }
                        None => Vec::new(),
                    }
                }
                None => {
                    let (drift, lines) = head_drift(&lock, &derived);
                    opt_in.extend(lines.into_iter().map(|l| (module.clone(), l)));
                    drift
                }
            };
            let unaccepted: Vec<(Line, Drift)> = drift
                .into_iter()
                .filter(|(line, _)| {
                    let key = (module.clone(), line.render());
                    let accepted = freeze
                        .accepted
                        .iter()
                        .any(|a| a.module == key.0 && a.line == key.1);
                    if accepted {
                        used_acceptances.insert(key);
                        accepted_lines.push((module.clone(), line.clone()));
                    }
                    !accepted
                })
                .collect();
            if !unaccepted.is_empty() {
                reports.push(drift_report(version, alias.as_deref(), &unaccepted));
            }
        }
        match alias {
            Some(target) => locks.push((
                *version,
                Lock {
                    lines: [Line::AliasOf { target }].into(),
                },
            )),
            None => {
                head = Some((module.clone(), lock.clone(), standing));
                locks.push((*version, lock));
            }
        }
        standings.push(standing);
    }
    for a in freeze.accepted {
        if !used_acceptances.contains(&(a.module.to_string(), a.line.to_string())) {
            errors.push(format!(
                "ACCEPTED_DIVERGENCES has `{}` {:?}, which is no divergence of a frozen \
                 facade: remove it",
                a.module, a.line
            ));
        }
    }
    errors.extend(reports);
    if errors.is_empty() {
        Ok(Generation {
            locks,
            standings,
            opt_in,
            accepted: accepted_lines,
        })
    } else {
        Err(errors.join("\n\n"))
    }
}

/// The versions a facade module serves: itself and its aliases.
fn served_versions<'a>(module: &str, locks: &'a [(Version, Lock)]) -> Vec<&'a Version> {
    locks
        .iter()
        .filter(|(v, lock)| v.module() == module || lock.alias_of() == Some(module))
        .map(|(v, _)| v)
        .collect()
}

/// Every generated facade file as `(repo-relative path, contents)`, from the
/// checked-in table, union and locks ([`outputs_from`]). Panics with every
/// reason the facades cannot be generated.
pub fn outputs(rows: &[FeatureRow], union: &Lock) -> Vec<(String, String)> {
    let versions = versions();
    outputs_from(&Inputs {
        rows,
        union,
        versions: &versions,
        freeze: CURRENT_FREEZE,
        on_disk: &checked_in_lock,
    })
    .unwrap_or_else(|e| panic!("{e}"))
}

/// The generation of the checked-in table, union and locks
/// ([`facade_locks`]).
pub fn checked_in_generation(rows: &[FeatureRow], union: &Lock) -> Result<Generation, String> {
    let versions = versions();
    facade_locks(&Inputs {
        rows,
        union,
        versions: &versions,
        freeze: CURRENT_FREEZE,
        on_disk: &checked_in_lock,
    })
}

/// The modules whose consumer fixture is on disk.
pub fn checked_in_fixtures() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(consumer::FIXTURES_DIR);
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let module = name.strip_suffix(".rs")?;
            (module != "mod").then(|| module.to_string())
        })
        .collect();
    out.sort();
    out
}

/// The fixtures, repo-relative, of the modules in `present` that are no
/// distinct published facade in `generation`: a re-baseline that turns a
/// head into an alias, or withdraws it, leaves them behind, and the generator
/// removes them. (Removing a frozen fixture outside a re-baseline is what
/// `tools/ci/facade-lock-check.sh` refuses.)
pub fn obsolete_fixtures(generation: &Generation, present: &[String]) -> Vec<String> {
    let heads: BTreeSet<String> = generation
        .locks
        .iter()
        .zip(&generation.standings)
        .filter(|((_, lock), standing)| {
            **standing != Standing::Derived && lock.alias_of().is_none()
        })
        .map(|((v, _), _)| v.module())
        .collect();
    present
        .iter()
        .filter(|m| !heads.contains(*m))
        .map(|m| consumer::fixture_path(m))
        .collect()
}

/// The lines each frozen head facade may opt into ([`Generation::opt_in`]),
/// from the checked-in table, union and locks: what the generator reports
/// beside what it writes. Empty when the freeze is broken, which
/// [`outputs`] reports.
pub fn opt_in(rows: &[FeatureRow], union: &Lock) -> Vec<(String, Line)> {
    checked_in_generation(rows, union)
        .map(|g| g.opt_in)
        .unwrap_or_default()
}

/// Every generated facade file as `(repo-relative path, contents)`: each
/// unpublished facade's lock; each published facade's lock and consumer
/// fixture when this generation stamps it ([`Standing::Publishing`]), and
/// never otherwise; each facade's code; and the module declarations, the
/// consumer fixtures' declarations and the property table. Fails with every
/// reason the freeze is broken or a facade cannot be generated from its lock.
pub fn outputs_from(inputs: &Inputs<'_>) -> Result<Vec<(String, String)>, String> {
    let Generation {
        locks,
        standings,
        accepted,
        ..
    } = facade_locks(inputs)?;
    let epoch = inputs.freeze.epoch;
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut fixtures: Vec<String> = Vec::new();
    for ((version, lock), standing) in locks.iter().zip(&standings) {
        let module = version.module();
        match standing {
            Standing::Derived => out.push((version.lock_path(), lock::render(&module, lock))),
            Standing::Publishing => out.push((
                version.lock_path(),
                lock::render_published(&module, lock, epoch),
            )),
            Standing::Frozen => {}
        }
        let is_head = lock.alias_of().is_none();
        if is_head && *standing != Standing::Derived {
            fixtures.push(module.clone());
            if *standing == Standing::Publishing {
                out.push((
                    consumer::fixture_path(&module),
                    consumer::generate_fixture(version, lock, epoch),
                ));
            }
        }
        let code = match lock.alias_of() {
            Some(target) => generate_alias(version, target),
            None => match generate_facade(
                version,
                lock,
                inputs.union,
                &served_versions(&module, &locks),
            ) {
                Ok(code) => code,
                Err(reasons) => {
                    errors.push(format!(
                        "the `{module}` facade cannot be generated from its lock:\n  {}",
                        reasons.join("\n  ")
                    ));
                    continue;
                }
            },
        };
        out.push((version.code_path(), code));
    }
    if !errors.is_empty() {
        return Err(errors.join("\n\n"));
    }
    out.push((FACADES_MOD_PATH.to_string(), generate_mod(&locks)));
    out.push((
        consumer::FIXTURES_MOD_PATH.to_string(),
        consumer::generate_fixtures_mod(&fixtures),
    ));
    out.push((
        MATRIX_PATH.to_string(),
        generate_matrix(
            &locks,
            inputs.union,
            &exemptions(&locks, inputs.union, &accepted),
        ),
    ));
    Ok(out)
}

/// What the property suite holds a distinct facade to less strictly, because
/// an accepted divergence ([`Accepted`]) made the frozen facade depart from
/// the union and the table there on purpose.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Exemption {
    /// Kinds whose nodes the facade may read apart from the union and the
    /// table: not cast where the table admits them, cast where it does not,
    /// or left out of a dispatch enum the union's holds them in.
    pub kinds: BTreeSet<String>,
    /// Value enums the facade may read apart from the union: an accessor
    /// whose shape names one is not compared.
    pub values: BTreeSet<String>,
}

/// Each distinct facade's [`Exemption`], by module: what the accepted lines
/// touch, credited to the facade that serves their module (an alias's to its
/// head). A `node` line touches its kind; an `enum` line its node's kinds, in
/// the union and in the facade; a `value` line its value enum. An `accessor`
/// or `derive` line touches nothing the comparison reads beyond what its
/// owner's or its enum's lines touch.
pub fn exemptions(
    locks: &[(Version, Lock)],
    union: &Lock,
    accepted: &[(String, Line)],
) -> BTreeMap<String, Exemption> {
    let node_kinds = |lock: &Lock, node: &str| -> Vec<String> {
        lock.lines
            .iter()
            .filter_map(|l| match l {
                Line::Node { node: n, kind } if n == node => Some(kind.clone()),
                _ => None,
            })
            .collect()
    };
    let mut out: BTreeMap<String, Exemption> = BTreeMap::new();
    for (module, line) in accepted {
        let Some((_, lock)) = locks.iter().find(|(v, _)| v.module() == *module) else {
            continue;
        };
        let head = lock.alias_of().unwrap_or(module).to_string();
        let head_lock = locks
            .iter()
            .find(|(v, _)| v.module() == head)
            .map(|(_, l)| l);
        let entry = out.entry(head).or_default();
        match line {
            Line::Node { kind, .. } => {
                entry.kinds.insert(kind.clone());
            }
            Line::Enum { node, .. } => {
                entry.kinds.extend(node_kinds(union, node));
                if let Some(head_lock) = head_lock {
                    entry.kinds.extend(node_kinds(head_lock, node));
                }
            }
            Line::Value { name, .. } => {
                entry.values.insert(name.clone());
            }
            Line::AliasOf { .. } | Line::Derive { .. } | Line::Accessor { .. } => {}
        }
    }
    out
}

/// The property suite's table of every facade, repo-relative.
pub const MATRIX_PATH: &str = "crates/cst/tests/all/facade_properties/matrix.rs";

/// The property suite's table (`crates/cst/tests/all/facade_properties`):
/// for each version, its `Parse::view`; for each distinct facade, every node
/// type and dispatch enum (its casts, beside the union's) and every accessor
/// (its result, beside the union accessor's); and every union node type. The
/// macros it invokes are the suite's.
fn generate_matrix(
    locks: &[(Version, Lock)],
    union: &Lock,
    exemptions: &BTreeMap<String, Exemption>,
) -> String {
    let mut out = format!(
        "{HEADER}//!\n\
         //! The facade property suite's table: every facade, node type, dispatch enum\n\
         //! and accessor, from the locks, so the properties range over all of them.\n\n\
         use borzoi_cst::language_version::LanguageVersion;\n\n\
         use super::{{Accessor, Cast, FacadeCase, UnionNode, View}};\n\n"
    );
    out.push_str("pub const VIEWS: &[View] = &[\n");
    for (version, lock) in locks {
        let module = version.module();
        let head = lock.alias_of().map_or(module.clone(), str::to_string);
        out.push_str(&format!(
            "view_case!({module}, {head}, LanguageVersion::{}),\n",
            version.variant
        ));
    }
    out.push_str("];\n\n");
    out.push_str("pub const UNION_NODES: &[UnionNode] = &[\n");
    for node in union.names().nodes {
        out.push_str(&format!("union_node!({node}),\n"));
    }
    out.push_str("];\n\n");
    out.push_str("pub const FACADES: &[FacadeCase] = &[\n");
    for (version, lock) in locks {
        if lock.alias_of().is_some() {
            continue;
        }
        let module = version.module();
        let served: Vec<String> = served_versions(&module, locks)
            .iter()
            .map(|v| format!("LanguageVersion::{}", v.variant))
            .collect();
        let names = lock.names();
        out.push_str(&format!(
            "FacadeCase {{\nmodule: \"{module}\",\nversions: &[{}],\ncasts: &[\n",
            served.join(", ")
        ));
        for name in names.nodes.iter().chain(&names.enums) {
            out.push_str(&format!("cast_case!({module}, {name}),\n"));
        }
        out.push_str("],\naccessors: &[\n");
        for line in &lock.lines {
            if let Line::Accessor {
                owner,
                name,
                shape,
                delegate,
            } = line
            {
                let form = if matches!(shape, Shape::Iter(_)) {
                    "iter"
                } else {
                    "one"
                };
                let delegate = delegate.as_deref().unwrap_or(name);
                out.push_str(&format!(
                    "accessor_case!({module}, {owner}, {name}, {delegate}, {form}, \"{}\"),\n",
                    shape.render()
                ));
            }
        }
        let exemption = exemptions.get(&module).cloned().unwrap_or_default();
        let kinds: Vec<String> = exemption
            .kinds
            .iter()
            .map(|k| format!("::borzoi_cst::syntax::SyntaxKind::{k}"))
            .collect();
        let values: Vec<String> = exemption
            .values
            .iter()
            .map(|v| format!("\"{v}\""))
            .collect();
        out.push_str(&format!(
            "],\nexempt_kinds: &[{}],\nexempt_values: &[{}],\n}},\n",
            kinds.join(", "),
            values.join(", ")
        ));
    }
    out.push_str("];\n");
    reformat(&out)
}

const HEADER: &str = "//! GENERATED by `tools/astgen` — do not edit by hand.\n\
                      //! Regenerate with `cargo run -p borzoi-astgen --example generate`; kept honest\n\
                      //! by borzoi-astgen's `tests/up_to_date.rs` staleness test.\n";

/// `F# 8.0`, `F# preview`.
fn fsharp(version: &Version) -> String {
    format!("F# {}", version.text)
}

/// The facade module declarations: each distinct facade compiled when its own
/// feature or any alias's is on, each alias under its own.
fn generate_mod(locks: &[(Version, Lock)]) -> String {
    let mut out = format!(
        "{HEADER}//!\n\
         //! The per-version typed facades (`docs/ast-versioning-plan.md` D4, D8): one\n\
         //! module per `LanguageVersion`, each behind the cargo feature of its name\n\
         //! (`unstable-preview` for `preview`). An alias's feature also compiles, and\n\
         //! so exposes, the module it aliases.\n\n"
    );
    for (version, lock) in locks {
        let module = version.module();
        let features: Vec<String> = if lock.alias_of().is_some() {
            vec![version.cargo_feature()]
        } else {
            locks
                .iter()
                .filter(|(v, l)| v.module() == module || l.alias_of() == Some(module.as_str()))
                .map(|(v, _)| v.cargo_feature())
                .collect()
        };
        let cfg = if let [one] = &features[..] {
            format!("feature = \"{one}\"")
        } else {
            let all: Vec<String> = features
                .iter()
                .map(|f| format!("feature = \"{f}\""))
                .collect();
            format!("any({})", all.join(", "))
        };
        out.push_str(&format!("#[cfg({cfg})]\npub mod {module};\n"));
    }
    reformat(&out)
}

/// An alias facade: the earlier facade's types.
fn generate_alias(version: &Version, target: &str) -> String {
    reformat(&format!(
        "{HEADER}//!\n\
         //! The **{fsharp}** typed facade. Its surface equals [`{target}`](super::{target})'s,\n\
         //! so it is that module's types (`crates/cst/facades/{module}.lock` is\n\
         //! `alias-of {target}`).\n\n\
         pub use super::{target}::*;\n",
        fsharp = fsharp(version),
        module = version.module(),
    ))
}

/// What the union's lock says about each type and accessor, for the checks
/// and re-casts a facade's code needs.
struct Union<'a> {
    node_kinds: BTreeMap<&'a str, BTreeSet<&'a str>>,
    enum_variants: BTreeMap<&'a str, BTreeMap<&'a str, &'a str>>,
    value_variants: BTreeMap<&'a str, BTreeMap<&'a str, &'a Payload>>,
    accessors: BTreeMap<(&'a str, &'a str), &'a Shape>,
}

impl<'a> Union<'a> {
    fn new(lock: &'a Lock) -> Self {
        let mut u = Union {
            node_kinds: BTreeMap::new(),
            enum_variants: BTreeMap::new(),
            value_variants: BTreeMap::new(),
            accessors: BTreeMap::new(),
        };
        for line in &lock.lines {
            match line {
                Line::Node { node, kind } => {
                    u.node_kinds.entry(node).or_default().insert(kind);
                }
                Line::Enum {
                    name,
                    variant,
                    node,
                } => {
                    u.enum_variants
                        .entry(name)
                        .or_default()
                        .insert(variant, node);
                }
                Line::Value {
                    name,
                    variant,
                    payload,
                } => {
                    u.value_variants
                        .entry(name)
                        .or_default()
                        .insert(variant, payload);
                }
                Line::Accessor {
                    owner, name, shape, ..
                } => {
                    u.accessors.insert((owner, name), shape);
                }
                Line::AliasOf { .. } | Line::Derive { .. } => {}
            }
        }
        u
    }
}

/// A facade's own types, read from its lock.
struct Own<'a> {
    node_kinds: BTreeMap<&'a str, Vec<&'a str>>,
    enum_variants: BTreeMap<&'a str, Vec<(&'a str, &'a str)>>,
    value_variants: BTreeMap<&'a str, Vec<(&'a str, &'a Payload)>>,
    derives: BTreeMap<&'a str, Vec<&'a str>>,
    /// `(owner, [(method, shape, delegate)])`.
    accessors: BTreeMap<&'a str, Vec<(&'a str, &'a Shape, &'a str)>>,
}

impl<'a> Own<'a> {
    fn new(lock: &'a Lock) -> Self {
        let mut own = Own {
            node_kinds: BTreeMap::new(),
            enum_variants: BTreeMap::new(),
            value_variants: BTreeMap::new(),
            derives: BTreeMap::new(),
            accessors: BTreeMap::new(),
        };
        for line in &lock.lines {
            match line {
                Line::Node { node, kind } => own.node_kinds.entry(node).or_default().push(kind),
                Line::Enum {
                    name,
                    variant,
                    node,
                } => own
                    .enum_variants
                    .entry(name)
                    .or_default()
                    .push((variant, node)),
                Line::Value {
                    name,
                    variant,
                    payload,
                } => own
                    .value_variants
                    .entry(name)
                    .or_default()
                    .push((variant, payload)),
                Line::Derive { name, trait_ } => own.derives.entry(name).or_default().push(trait_),
                Line::Accessor {
                    owner,
                    name,
                    shape,
                    delegate,
                } => own.accessors.entry(owner).or_default().push((
                    name,
                    shape,
                    delegate.as_deref().unwrap_or(name),
                )),
                Line::AliasOf { .. } => {}
            }
        }
        own
    }
}

/// How the facade turns a union value into its own: `None` when the value is
/// the same type in both (a token or scalar), otherwise the function to apply,
/// and whether it can fail (returns an `Option`).
#[derive(Clone)]
struct Conv {
    func: Option<String>,
    partial: bool,
}

/// Re-cast planning for one facade: which conversions can fail, and which
/// types' `from_union` the code calls.
struct Plan<'a> {
    union: &'a Union<'a>,
    own: &'a Own<'a>,
    partial: BTreeMap<String, bool>,
    needed: BTreeSet<String>,
    errors: Vec<String>,
}

impl Plan<'_> {
    /// Whether converting a union value of the facade type `name` can fail.
    ///
    /// A node or dispatch enum always can, whatever the union holds today:
    /// the union gains a kind or a variant whenever a construct is modelled,
    /// and a frozen facade cannot follow it. Assuming so up front keeps a
    /// facade's code a function of its own lock, so the union's growth leaves
    /// every published facade byte-identical (astgen's synthetic-feature
    /// test), and keeps an accessor that would one day need a compat shim
    /// from ever being published. A value enum fails when the union's has a
    /// variant the facade lacks, or a variant's bare field can fail.
    fn is_partial(&mut self, name: &str) -> bool {
        if let Some(&p) = self.partial.get(name) {
            return p;
        }
        // A type is never its own payload, but guard the recursion anyway.
        self.partial.insert(name.to_string(), true);
        let p = if let Some(variants) = self.own.value_variants.get(name) {
            let union_count = self.union.value_variants.get(name).map_or(0, BTreeMap::len);
            let variants = variants.clone();
            union_count != variants.len()
                || variants.iter().any(|(_, payload)| {
                    payload_shapes(payload)
                        .into_iter()
                        .any(|s| self.shape_drops(s))
                })
        } else {
            true
        };
        self.partial.insert(name.to_string(), p);
        p
    }

    /// Whether a payload field of shape `s` fails when its element does: only
    /// a bare element does (a container absorbs the failure).
    fn shape_drops(&mut self, s: &Shape) -> bool {
        match s {
            Shape::One(e) => self.elem(e).partial,
            Shape::Option(_) | Shape::Iter(_) | Shape::Vec(_) => false,
        }
    }

    /// The conversion of one element.
    fn elem(&mut self, e: &Elem) -> Conv {
        match e {
            Elem::Node(n) => {
                self.needed.insert(n.clone());
                Conv {
                    func: Some(format!("{n}::from_union")),
                    partial: true,
                }
            }
            Elem::Enum(n) | Elem::Value(n) => {
                self.needed.insert(n.clone());
                Conv {
                    func: Some(format!("{n}::from_union")),
                    partial: self.is_partial(n),
                }
            }
            Elem::Tuple(items) => {
                let convs: Vec<Conv> = items.iter().map(|i| self.elem(i)).collect();
                if convs.iter().all(|c| c.func.is_none()) {
                    return Conv {
                        func: None,
                        partial: false,
                    };
                }
                let partial = convs.iter().any(|c| c.partial);
                let vars: Vec<String> = (0..convs.len()).map(|i| format!("x{i}")).collect();
                let parts: Vec<String> = convs
                    .iter()
                    .zip(&vars)
                    .map(|(c, v)| apply_one(c, v, partial))
                    .collect();
                let body = format!("({})", parts.join(", "));
                let body = if partial {
                    format!("Some({body})")
                } else {
                    body
                };
                Conv {
                    func: Some(format!("|({})| {body}", vars.join(", "))),
                    partial,
                }
            }
            Elem::Token
            | Elem::Bool
            | Elem::Usize
            | Elem::I32
            | Elem::TextRange
            | Elem::StaticStr => Conv {
                func: None,
                partial: false,
            },
        }
    }

    /// The expression re-casting `input`, a union value of shape `shape`, into
    /// the facade's; `inside_option` when the expression may use `?` (the
    /// enclosing function returns an `Option`).
    fn shape_expr(
        &mut self,
        shape: &Shape,
        input: &str,
        inside_option: bool,
        what: &str,
    ) -> String {
        match shape {
            Shape::One(e) => {
                let conv = self.elem(e);
                if conv.partial && !inside_option {
                    self.errors.push(format!(
                        "{what} returns `{}` bare, but the union's can hold a value \
                         outside this facade; it needs a compat shim",
                        shape.render()
                    ));
                }
                apply_one(&conv, input, inside_option)
            }
            Shape::Option(e) => {
                let conv = self.elem(e);
                match conv.func {
                    None => input.to_string(),
                    Some(f) if conv.partial => format!("{input}.and_then({f})"),
                    Some(f) => format!("{input}.map({f})"),
                }
            }
            Shape::Iter(e) => {
                let conv = self.elem(e);
                match conv.func {
                    None => input.to_string(),
                    Some(f) if conv.partial => format!("{input}.filter_map({f})"),
                    Some(f) => format!("{input}.map({f})"),
                }
            }
            Shape::Vec(e) => {
                let conv = self.elem(e);
                match conv.func {
                    None => input.to_string(),
                    Some(f) if conv.partial => {
                        format!("{input}.into_iter().filter_map({f}).collect()")
                    }
                    Some(f) => format!("{input}.into_iter().map({f}).collect()"),
                }
            }
        }
    }
}

/// `f(input)`, with `?` when `f` can fail and the caller propagates.
fn apply_one(conv: &Conv, input: &str, propagate: bool) -> String {
    match &conv.func {
        None => input.to_string(),
        Some(f) if f.starts_with('|') => {
            let call = format!("({f})({input})");
            if conv.partial && propagate {
                format!("{call}?")
            } else {
                call
            }
        }
        Some(f) if conv.partial && propagate => format!("{f}({input})?"),
        Some(f) => format!("{f}({input})"),
    }
}

fn payload_shapes(payload: &Payload) -> Vec<&Shape> {
    match payload {
        Payload::Unit => Vec::new(),
        Payload::Tuple(shapes) => shapes.iter().collect(),
        Payload::Named(fields) => fields.iter().map(|(_, s)| s).collect(),
    }
}

/// The Rust spelling of an element in the facade's namespace.
fn elem_type(e: &Elem) -> String {
    match e {
        Elem::Node(n) | Elem::Enum(n) | Elem::Value(n) => n.clone(),
        Elem::Token => "crate::syntax::SyntaxToken".to_string(),
        Elem::Bool => "bool".to_string(),
        Elem::Usize => "usize".to_string(),
        Elem::I32 => "i32".to_string(),
        Elem::TextRange => "crate::syntax::TextRange".to_string(),
        Elem::StaticStr => "&'static str".to_string(),
        Elem::Tuple(items) => {
            let items: Vec<String> = items.iter().map(elem_type).collect();
            format!("({})", items.join(", "))
        }
    }
}

/// The Rust spelling of a shape in the facade's namespace.
fn shape_type(s: &Shape) -> String {
    match s {
        Shape::One(e) => elem_type(e),
        Shape::Option(e) => format!("Option<{}>", elem_type(e)),
        Shape::Iter(e) => format!("impl Iterator<Item = {}> + '_", elem_type(e)),
        Shape::Vec(e) => format!("Vec<{}>", elem_type(e)),
    }
}

/// The value enum `name`'s doc and its variants' docs, from astgen's tables
/// (docs are not part of a lock).
fn value_docs(name: &str) -> Option<&'static crate::ValueEnum> {
    CATEGORIES
        .iter()
        .flat_map(|c| c.value_enums)
        .find(|v| v.name == name)
}

fn push_doc_lines(doc: &[&str], indent: &str, out: &mut String) {
    for line in doc {
        if line.is_empty() {
            out.push_str(&format!("{indent}///\n"));
        } else {
            out.push_str(&format!("{indent}/// {line}\n"));
        }
    }
}

/// The union-lock checks a facade type or accessor must pass to be re-cast
/// mechanically.
fn check_against_union(own: &Own<'_>, union: &Union<'_>, errors: &mut Vec<String>) {
    for (node, kinds) in &own.node_kinds {
        match union.node_kinds.get(node) {
            None => errors.push(format!("node `{node}` is not in the union")),
            Some(u) => {
                for kind in kinds {
                    if !u.contains(kind) {
                        errors.push(format!(
                            "node `{node}` casts `{kind}`, which the union's `{node}` does not"
                        ));
                    }
                }
            }
        }
    }
    for (name, variants) in &own.enum_variants {
        for (variant, node) in variants {
            if union.enum_variants.get(name).and_then(|v| v.get(variant)) != Some(node) {
                errors.push(format!(
                    "`{name}::{variant}({node})` is not a variant of the union's `{name}`"
                ));
            }
        }
    }
    for (name, variants) in &own.value_variants {
        for (variant, payload) in variants {
            if union.value_variants.get(name).and_then(|v| v.get(variant)) != Some(payload) {
                errors.push(format!(
                    "value `{name}::{variant}` is not the union's (same name and payload)"
                ));
            }
        }
    }
    for (owner, accessors) in &own.accessors {
        for (name, shape, delegate) in accessors {
            if union.accessors.get(&(*owner, *delegate)) != Some(shape) {
                errors.push(format!(
                    "accessor `{owner}::{name}` delegates to `union.{delegate}`, which the \
                     union's `{owner}` lacks or declares with another shape"
                ));
            }
        }
    }
}

/// The code of the distinct facade `version` from its lock and the union's,
/// serving `served` (itself and its aliases) through `Parse::view`; or every
/// reason it cannot be generated mechanically.
pub fn generate_facade(
    version: &Version,
    lock: &Lock,
    union_lock: &Lock,
    served: &[&Version],
) -> Result<String, Vec<String>> {
    let union = Union::new(union_lock);
    let own = Own::new(lock);
    let mut errors = Vec::new();
    check_against_union(&own, &union, &mut errors);
    if !own.enum_variants.contains_key("ParsedInput") {
        errors.push("the facade has no `ParsedInput`, the root `Parse::view` returns".into());
    }
    let mut plan = Plan {
        union: &union,
        own: &own,
        partial: BTreeMap::new(),
        needed: BTreeSet::new(),
        errors: Vec::new(),
    };

    // Accessors first: they decide which types' `from_union` the code needs.
    let mut accessors = String::new();
    for (owner, methods) in &own.accessors {
        accessors.push_str(&format!("impl {owner} {{\n"));
        for (name, shape, delegate) in methods {
            let body = plan.shape_expr(
                shape,
                &format!("self.0.{delegate}()"),
                false,
                &format!("accessor `{owner}::{name}`"),
            );
            accessors.push_str(&format!(
                "pub fn {name}(&self) -> {} {{ {body} }}\n",
                shape_type(shape)
            ));
        }
        accessors.push_str("}\n\n");
    }

    // Each type's `from_union`, for every type the code converts, closing over
    // the types those convert in turn.
    let mut conversions = String::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    while let Some(name) = plan.needed.iter().find(|n| !done.contains(*n)).cloned() {
        done.insert(name.clone());
        let partial = plan.is_partial(&name);
        let ret = if partial { "Option<Self>" } else { "Self" };
        let wrap = |body: String| {
            if partial {
                format!("Some({body})")
            } else {
                body
            }
        };
        if own.node_kinds.contains_key(name.as_str()) {
            // Every node conversion can fail (see `Plan::is_partial`).
            conversions.push_str(&format!(
                "impl {name} {{\n\
                 pub(crate) fn from_union(node: u::{name}) -> Option<Self> {{\n\
                 Self::can_cast(node.syntax().kind()).then_some(Self(node))\n}}\n}}\n\n"
            ));
        } else if let Some(variants) = own.enum_variants.get(name.as_str()) {
            let mut arms = String::new();
            for (variant, node) in variants {
                let conv = plan.elem(&Elem::Node(node.to_string()));
                arms.push_str(&format!(
                    "u::{name}::{variant}(n) => {name}::{variant}({}),\n",
                    apply_one(&conv, "n", partial)
                ));
            }
            // Whatever the union's enum holds today, it may grow (see
            // `Plan::is_partial`), so the fallback is unconditional.
            arms.push_str("#[allow(unreachable_patterns)]\n_ => return None,\n");
            conversions.push_str(&format!(
                "impl {name} {{\n\
                 pub(crate) fn from_union(value: u::{name}) -> {ret} {{\n{}\n}}\n}}\n\n",
                wrap(format!("match value {{\n{arms}}}"))
            ));
        } else if let Some(variants) = own.value_variants.get(name.as_str()) {
            let mut arms = String::new();
            for (variant, payload) in variants {
                let what = format!("value `{name}::{variant}`");
                match payload {
                    Payload::Unit => {
                        arms.push_str(&format!("u::{name}::{variant} => {name}::{variant},\n"));
                    }
                    Payload::Tuple(shapes) => {
                        let vars: Vec<String> =
                            (0..shapes.len()).map(|i| format!("x{i}")).collect();
                        let exprs: Vec<String> = shapes
                            .iter()
                            .zip(&vars)
                            .map(|(s, v)| plan.shape_expr(s, v, partial, &what))
                            .collect();
                        arms.push_str(&format!(
                            "u::{name}::{variant}({}) => {name}::{variant}({}),\n",
                            vars.join(", "),
                            exprs.join(", ")
                        ));
                    }
                    Payload::Named(fields) => {
                        let names: Vec<&str> = fields.iter().map(|(f, _)| f.as_str()).collect();
                        let exprs: Vec<String> = fields
                            .iter()
                            .map(|(f, s)| {
                                let expr = plan.shape_expr(s, f, partial, &what);
                                if expr == *f {
                                    f.clone()
                                } else {
                                    format!("{f}: {expr}")
                                }
                            })
                            .collect();
                        arms.push_str(&format!(
                            "u::{name}::{variant} {{ {} }} => {name}::{variant} {{ {} }},\n",
                            names.join(", "),
                            exprs.join(", ")
                        ));
                    }
                }
            }
            if union
                .value_variants
                .get(name.as_str())
                .map_or(0, BTreeMap::len)
                != variants.len()
            {
                arms.push_str("_ => return None,\n");
            }
            conversions.push_str(&format!(
                "impl {name} {{\n\
                 pub(crate) fn from_union(value: u::{name}) -> {ret} {{\n{}\n}}\n}}\n\n",
                wrap(format!("match value {{\n{arms}}}"))
            ));
        }
    }
    errors.append(&mut plan.errors);
    if !errors.is_empty() {
        return Err(errors);
    }

    let module = version.module();
    let served_text: Vec<String> = served.iter().map(|v| fsharp(v)).collect();
    let aliases: Vec<String> = served[1..]
        .iter()
        .map(|v| format!("`{}`", v.module()))
        .collect();
    let alias_note = if aliases.is_empty() {
        String::new()
    } else {
        format!(
            "\n//! {} {} the same surface, so {} this module's types.",
            aliases.join(", "),
            if aliases.len() == 1 { "has" } else { "have" },
            if aliases.len() == 1 {
                "it re-exports"
            } else {
                "they re-export"
            },
        )
    };
    let mut out = format!(
        "{HEADER}//!\n//! Generated from `crates/cst/facades/{module}.lock` and the union's lock.\n\
         //!\n\
         //! The **{fsharp}** typed facade (`docs/ast-versioning-plan.md` D4): every node\n\
         //! type and enum of the {served} surface, each a newtype over the union's, with\n\
         //! accessors that re-cast the union's results. A node outside the surface\n\
         //! reads as an absent child. Read a parse through it with\n\
         //! [`Parse::view`](crate::parser::Parse::view).{alias_note}\n\n\
         use crate::language_version::LanguageVersion;\n\
         use crate::syntax::union as u;\n\
         use crate::syntax::{{AstNode, SyntaxKind, SyntaxNode}};\n\n",
        fsharp = fsharp(version),
        served = served_text.join(" / "),
    );

    for (node, kinds) in &own.node_kinds {
        out.push_str(&format!(
            "crate::syntax::facade_support::facade_node!({node}: {});\n",
            kinds.join(" | ")
        ));
    }
    out.push('\n');

    for (name, variants) in &own.enum_variants {
        let with_kinds: Vec<(&str, &str, Vec<String>)> = variants
            .iter()
            .map(|(variant, node)| {
                let kinds = own.node_kinds[node].iter().map(|k| k.to_string()).collect();
                (*variant, *node, kinds)
            })
            .collect();
        crate::gen_enum_with_kinds(name, &with_kinds, &mut out);
        let arms: Vec<String> = variants
            .iter()
            .map(|(variant, _)| {
                format!("{name}::{variant}(n) => u::{name}::{variant}(n.to_union()),")
            })
            .collect();
        out.push_str(&format!(
            "\nimpl {name} {{\n\
             /// This value as the union's [`{name}`](crate::syntax::{name}).\n\
             #[cfg(feature = \"unstable-union\")]\n\
             pub fn to_union(&self) -> u::{name} {{\nmatch self {{\n{}\n}}\n}}\n}}\n\n",
            arms.join("\n")
        ));
    }

    for (name, variants) in &own.value_variants {
        let docs = value_docs(name);
        if let Some(docs) = docs {
            push_doc_lines(docs.doc, "", &mut out);
        }
        out.push_str(&format!(
            "#[derive({})]\npub enum {name} {{\n",
            own.derives
                .get(name)
                .map(|d| d.join(", "))
                .unwrap_or_default()
        ));
        for (variant, payload) in variants {
            if let Some(v) = docs.and_then(|d| d.variants.iter().find(|v| v.name == *variant)) {
                push_doc_lines(v.doc, "    ", &mut out);
            }
            match payload {
                Payload::Unit => out.push_str(&format!("    {variant},\n")),
                Payload::Tuple(shapes) => {
                    let types: Vec<String> = shapes.iter().map(shape_type).collect();
                    out.push_str(&format!("    {variant}({}),\n", types.join(", ")));
                }
                Payload::Named(fields) => {
                    let fields: Vec<String> = fields
                        .iter()
                        .map(|(f, s)| format!("{f}: {}", shape_type(s)))
                        .collect();
                    out.push_str(&format!("    {variant} {{ {} }},\n", fields.join(", ")));
                }
            }
        }
        out.push_str("}\n\n");
    }

    out.push_str(&conversions);
    out.push_str(&accessors);

    let versions: Vec<String> = served
        .iter()
        .map(|v| format!("LanguageVersion::{}", v.variant))
        .collect();
    out.push_str(&format!(
        "impl crate::syntax::view::Sealed for ParsedInput {{}}\n\n\
         impl crate::syntax::Facade for ParsedInput {{\n\
         const VERSIONS: &'static [LanguageVersion] = &[{}];\n}}\n",
        versions.join(", ")
    ));
    Ok(reformat(&out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{Effect, Engaged, Modelling, Stage};

    fn union() -> Lock {
        crate::union_lock()
    }

    /// A freeze publishing `published` at `epoch`, accepting nothing.
    fn freeze<'a>(published: &'a [&'a str], epoch: u32) -> Freeze<'a> {
        Freeze {
            published,
            epoch,
            accepted: &[],
        }
    }

    /// A generation over `rows` and `union` at the real versions, reading
    /// lock texts from `disk` (by module).
    fn generation(
        rows: &[FeatureRow],
        union: &Lock,
        freeze: Freeze<'_>,
        disk: &BTreeMap<String, String>,
    ) -> Result<Generation, String> {
        let versions = versions();
        facade_locks(&Inputs {
            rows,
            union,
            versions: &versions,
            freeze,
            on_disk: &|v: &Version| disk.get(&v.module()).cloned(),
        })
    }

    /// [`outputs_from`] for [`generation`]'s inputs, by path.
    fn outputs_with(
        rows: &[FeatureRow],
        union: &Lock,
        versions: &[Version],
        freeze: Freeze<'_>,
        disk: &dyn Fn(&Version) -> Option<String>,
    ) -> Result<BTreeMap<String, String>, String> {
        outputs_from(&Inputs {
            rows,
            union,
            versions,
            freeze,
            on_disk: disk,
        })
        .map(|out| out.into_iter().collect())
    }

    /// The lock texts a generation writes, by module: what the disk holds
    /// after it.
    fn written(out: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        versions()
            .iter()
            .filter_map(|v| {
                out.get(&v.lock_path())
                    .map(|text| (v.module(), text.clone()))
            })
            .collect()
    }

    /// The disk after a first publication of `published` at `epoch`.
    fn published_disk(published: &[&str], epoch: u32) -> BTreeMap<String, String> {
        let rows = features::feature_table();
        let out = outputs_with(
            &rows,
            &union(),
            &versions(),
            freeze(published, epoch),
            &|_| None,
        )
        .unwrap();
        written(&out)
    }

    const UP_TO_V8: &[&str] = &["v4_6", "v4_7", "v5", "v6", "v7", "v8"];

    #[test]
    fn version_modules_and_features() {
        let names: Vec<(String, String)> = versions()
            .iter()
            .map(|v| (v.module(), v.cargo_feature()))
            .collect();
        let modules: Vec<&str> = names.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(
            modules,
            [
                "v4_6", "v4_7", "v5", "v6", "v7", "v8", "v9", "v10", "v11", "preview"
            ]
        );
        assert_eq!(names.last().unwrap().1, "unstable-preview");
        for v in versions() {
            assert_eq!(v.decimal, features::version_decimal(v.text), "{}", v.text);
        }
    }

    /// Publishing is a change of who writes the lock, not of the code: a
    /// first publication stamps each published lock and writes a fixture per
    /// distinct one, and a generation reading those locks back produces the
    /// same code and aliasing and writes neither again.
    #[test]
    fn a_published_lock_round_trips() {
        let rows = features::feature_table();
        let union = union();
        let versions = versions();
        let derived = outputs_with(&rows, &union, &versions, freeze(&[], 1), &|_| None).unwrap();
        let publishing =
            outputs_with(&rows, &union, &versions, freeze(UP_TO_V8, 1), &|_| None).unwrap();
        let disk = written(&publishing);
        let frozen = outputs_with(&rows, &union, &versions, freeze(UP_TO_V8, 1), &|v| {
            disk.get(&v.module()).cloned()
        })
        .unwrap();
        for v in &versions {
            let module = v.module();
            let published = UP_TO_V8.contains(&module.as_str());
            let lock = &publishing[&v.lock_path()];
            assert_eq!(
                lock::freeze_epoch(lock).unwrap(),
                published.then_some(1),
                "{module}"
            );
            assert_eq!(
                lock::parse(lock).unwrap(),
                lock::parse(&derived[&v.lock_path()]).unwrap()
            );
            assert_eq!(frozen.contains_key(&v.lock_path()), !published, "{module}");
            assert_eq!(derived[&v.code_path()], frozen[&v.code_path()], "{module}");
            assert_eq!(publishing[&v.code_path()], frozen[&v.code_path()]);
        }
        let fixtures: Vec<&String> = publishing
            .keys()
            .filter(|p| p.starts_with(consumer::FIXTURES_DIR) && !p.ends_with("mod.rs"))
            .collect();
        // One per distinct published facade: each published lock that is not
        // an alias.
        let mut heads: Vec<String> = versions
            .iter()
            .filter(|v| UP_TO_V8.contains(&v.module().as_str()))
            .filter(|v| {
                lock::parse(&publishing[&v.lock_path()])
                    .unwrap()
                    .alias_of()
                    .is_none()
            })
            .map(|v| consumer::fixture_path(&v.module()))
            .collect();
        heads.sort();
        assert!(heads.len() > 1, "{heads:?}");
        assert_eq!(fixtures, heads.iter().collect::<Vec<_>>());
        assert!(
            !frozen
                .keys()
                .any(|p| p.starts_with(consumer::FIXTURES_DIR) && !p.ends_with("mod.rs"))
        );
        assert_eq!(
            frozen[consumer::FIXTURES_MOD_PATH],
            publishing[consumer::FIXTURES_MOD_PATH]
        );
    }

    /// Every facade's lock is a subset of the union's, delegates aside, and a
    /// derived facade drops exactly the types a live `Kind` row introduced
    /// after its version, and whatever names them.
    #[test]
    fn derived_locks_restrict_the_union() {
        let rows = features::feature_table();
        let union = union();
        let undelegated = |line: &Line| match line {
            Line::Accessor {
                owner, name, shape, ..
            } => Line::Accessor {
                owner: owner.clone(),
                name: name.clone(),
                shape: shape.clone(),
                delegate: None,
            },
            other => other.clone(),
        };
        let generation = generation(&rows, &union, freeze(&[], 1), &BTreeMap::new()).unwrap();
        assert!(generation.standings.iter().all(|s| *s == Standing::Derived));
        for (version, lock) in generation.locks {
            for line in &lock.lines {
                if !matches!(line, Line::AliasOf { .. }) {
                    assert!(
                        union.lines.contains(&undelegated(line)),
                        "{}: {line:?} is not the union's",
                        version.module()
                    );
                }
            }
        }
        let v8 = derived_lock(&union, &rows, 8.0);
        assert!(!v8.names().nodes.contains("WithNullType"));
        assert!(v8.names().nodes.contains("IntersectionType"));
        assert!(v8.names().nodes.contains("DotLambdaExpr"));
        let v7 = derived_lock(&union, &rows, 7.0);
        assert!(!v7.names().nodes.contains("WhileBangExpr"));
        assert!(!v7.names().nodes.contains("DotLambdaExpr"));
        assert!(v7.names().nodes.contains("InterpStringExpr"));
        let v4_7 = derived_lock(&union, &rows, 4.7);
        assert!(!v4_7.names().nodes.contains("InterpStringExpr"));
        let v9 = derived_lock(&union, &rows, 9.0);
        assert!(!v9.names().nodes.contains("IndexFromEndExpr"));
        let preview = derived_lock(&union, &rows, 9999.0);
        assert!(preview.names().nodes.contains("IndexFromEndExpr"));
        assert!(v9.lines.is_subset(&preview.lines));
    }

    /// The epoch decides a published lock's standing: unstamped or older is
    /// (re)published, current is frozen, newer is refused; and a lock stamped
    /// at the current epoch whose facade is not published is refused.
    #[test]
    fn the_freeze_epoch_decides_a_published_lock_s_standing() {
        let rows = features::feature_table();
        let union = union();
        let disk = published_disk(UP_TO_V8, 2);
        let standing = |epoch: u32| {
            let g = generation(&rows, &union, freeze(UP_TO_V8, epoch), &disk).unwrap();
            g.standings[5]
        };
        assert_eq!(standing(2), Standing::Frozen);
        assert_eq!(standing(3), Standing::Publishing);
        let newer = generation(&rows, &union, freeze(UP_TO_V8, 1), &disk).unwrap_err();
        assert!(newer.contains("after FREEZE_EPOCH"), "{newer}");
        let withdrawn = generation(&rows, &union, freeze(&["v4_6"], 2), &disk).unwrap_err();
        assert!(
            withdrawn.contains("`v8` is not in PUBLISHED"),
            "{withdrawn}"
        );
        // A bump withdraws a facade without complaint.
        let bumped = generation(&rows, &union, freeze(&["v4_6"], 3), &disk).unwrap();
        assert_eq!(bumped.standings[5], Standing::Derived);
    }

    /// A published alias needs a published head, and `preview` and unknown
    /// modules are never published.
    #[test]
    fn what_may_be_published() {
        let rows = features::feature_table();
        let union = union();
        let none = BTreeMap::new();
        for (published, needle) in [
            (&["v4_7"][..], "which is not published"),
            (&["preview"][..], "`preview` is never published"),
            (&["v99"][..], "PUBLISHED names `v99`"),
        ] {
            let e = generation(&rows, &union, freeze(published, 1), &none).unwrap_err();
            assert!(e.contains(needle), "{published:?}: {e}");
        }
    }

    /// The lock of the frozen `v8`, with `edit` applied to its records.
    fn v8_disk(edit: impl FnOnce(&mut Lock)) -> BTreeMap<String, String> {
        let mut disk = published_disk(UP_TO_V8, 1);
        let mut lock = lock::parse(&disk["v8"]).unwrap();
        edit(&mut lock);
        disk.insert("v8".to_string(), lock::render_published("v8", &lock, 1));
        disk
    }

    fn some_accessor(lock: &Lock) -> Line {
        lock.lines
            .iter()
            .find(|l| matches!(l, Line::Accessor { owner, .. } if owner == "ParenType"))
            .cloned()
            .unwrap()
    }

    /// A frozen facade whose derived surface has moved in a way that would
    /// break it or open a hole fails, naming each line and what can be done,
    /// unless it is accepted; a stale acceptance fails too. A line it may
    /// opt into (an accessor, a derive) and a renamed accessor's compat line
    /// are no divergence.
    #[test]
    fn a_frozen_lock_that_diverges_is_refused_until_reconciled() {
        let rows = features::feature_table();
        let union = union();
        let run = |union: &Lock, disk: &BTreeMap<String, String>, accepted: &[Accepted]| {
            generation(
                &rows,
                union,
                Freeze {
                    published: UP_TO_V8,
                    epoch: 1,
                    accepted,
                },
                disk,
            )
        };
        let clean = published_disk(UP_TO_V8, 1);

        // The union grew an accessor and a derive the frozen lock lacks: the
        // facade may opt into them, and nothing fails.
        let mut missing = None;
        let disk = v8_disk(|lock| {
            let line = some_accessor(lock);
            lock.lines.remove(&line);
            missing = Some(line);
        });
        let g = run(&union, &disk, &[]).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(g.opt_in, [("v8".to_string(), missing.unwrap())]);
        let mut grown = union.clone();
        grown.lines.insert(Line::Derive {
            name: "TupleSegment".to_string(),
            trait_: "Ord".to_string(),
        });
        let g = run(&grown, &clean, &[]).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            g.opt_in
                .iter()
                .any(|(m, l)| m == "v8"
                    && matches!(l, Line::Derive { trait_, .. } if trait_ == "Ord")),
            "{:?}",
            g.opt_in
        );

        // A kind the frozen lock does not cast: a new one for its node, or a
        // new node type in the union.
        let disk = v8_disk(|lock| {
            lock.lines.remove(&Line::Node {
                node: "AppExpr".to_string(),
                kind: "INFIX_APP_EXPR".to_string(),
            });
        });
        let e = run(&union, &disk, &[]).unwrap_err();
        assert!(e.contains("read as a hole"), "{e}");
        assert!(e.contains("node\tAppExpr\tINFIX_APP_EXPR"), "{e}");
        let mut grown = union.clone();
        grown.lines.insert(Line::Node {
            node: "FreshNode".to_string(),
            kind: "FRESH_NODE".to_string(),
        });
        let e = run(&grown, &clean, &[]).unwrap_err();
        assert!(e.contains("node\tFreshNode\tFRESH_NODE"), "{e}");

        // The union grew a variant the frozen lock cannot take; accepting it
        // reconciles it.
        let disk = v8_disk(|lock| {
            lock.lines
                .retain(|l| !matches!(l, Line::Enum { variant, .. } if variant == "WhileBang"));
        });
        let blocked = "enum\tExpr\tWhileBang\tWhileBangExpr";
        let e = run(&union, &disk, &[]).unwrap_err();
        assert!(e.contains("no allowed evolution adds it"), "{e}");
        assert!(e.contains(blocked), "{e}");
        let accepted = [Accepted {
            module: "v8",
            line: blocked,
            reason: "test",
        }];
        assert!(run(&union, &disk, &accepted).is_ok());
        // The acceptance reaches the property suite: the generated table
        // exempts the variant's kind in `v8`, and in no other facade.
        let out = outputs_with(
            &rows,
            &union,
            &versions(),
            Freeze {
                published: UP_TO_V8,
                epoch: 1,
                accepted: &accepted,
            },
            &|v| disk.get(&v.module()).cloned(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let matrix = &out[MATRIX_PATH];
        assert_eq!(
            matrix
                .matches("::borzoi_cst::syntax::SyntaxKind::WHILE_BANG_EXPR")
                .count(),
            1,
            "{matrix}"
        );
        let v8_case = &matrix[matrix.find("module: \"v8\"").unwrap()..];
        let v8_case = &v8_case[..v8_case.find("FacadeCase {").unwrap_or(v8_case.len())];
        assert!(v8_case.contains("SyntaxKind::WHILE_BANG_EXPR"), "{v8_case}");

        // The derivation dropped what the frozen lock keeps.
        let v9 = derived_lock(&union, &rows, 9.0);
        let disk = v8_disk(|lock| *lock = v9.clone());
        let e = run(&union, &disk, &[]).unwrap_err();
        assert!(e.contains("locked but no longer derived"), "{e}");
        assert!(e.contains("node\tWithNullType\tWITH_NULL_TYPE"), "{e}");

        // An accessor the lock names whose union delegate changed shape.
        let mut reshaped = union.clone();
        let Line::Accessor {
            owner, name, shape, ..
        } = some_accessor(&lock::parse(&clean["v8"]).unwrap())
        else {
            unreachable!()
        };
        reshaped.lines.remove(&Line::Accessor {
            owner: owner.clone(),
            name: name.clone(),
            shape: shape.clone(),
            delegate: None,
        });
        reshaped.lines.insert(Line::Accessor {
            owner: owner.clone(),
            name: name.clone(),
            shape: Shape::One(Elem::Bool),
            delegate: None,
        });
        let e = run(&reshaped, &clean, &[]).unwrap_err();
        assert!(e.contains("locked but no longer derived"), "{e}");
        assert!(e.contains(&format!("accessor\t{owner}\t{name}")), "{e}");

        // A stale acceptance.
        let e = run(&union, &clean, &accepted).unwrap_err();
        assert!(e.contains("which is no divergence"), "{e}");

        // A compat line for a renamed union accessor is no divergence.
        let disk = v8_disk(|lock| {
            let Line::Accessor {
                owner, name, shape, ..
            } = some_accessor(lock)
            else {
                unreachable!()
            };
            lock.lines.insert(Line::Accessor {
                owner,
                name: format!("old_{name}"),
                shape,
                delegate: Some(name),
            });
        });
        assert!(run(&union, &disk, &[]).is_ok());
    }

    /// An unpublished version never aliases a published head, whose
    /// `Facade::VERSIONS` would otherwise follow it; a version being
    /// published does.
    #[test]
    fn only_a_published_version_aliases_a_published_head() {
        let rows = features::feature_table();
        let union = union();
        let disk = published_disk(UP_TO_V8, 1);
        let g = generation(&rows, &union, freeze(UP_TO_V8, 1), &disk).unwrap();
        let alias_of = |g: &Generation, module: &str| {
            g.locks
                .iter()
                .find(|(v, _)| v.module() == module)
                .and_then(|(_, l)| l.alias_of().map(str::to_string))
        };
        // v9 equals v8's surface plus `WithNull`, so it is distinct either way;
        // v10 and v11 equal v9's.
        assert_eq!(alias_of(&g, "v10"), Some("v9".to_string()));
        let published = &["v4_6", "v4_7", "v5", "v6", "v7", "v8", "v9"];
        let disk = published_disk(published, 1);
        let g = generation(&rows, &union, freeze(published, 1), &disk).unwrap();
        assert_eq!(alias_of(&g, "v10"), None, "an unpublished v10");
        let mut one_more = published.to_vec();
        one_more.push("v10");
        let g = generation(&rows, &union, freeze(&one_more, 1), &disk).unwrap();
        assert_eq!(g.standings[7], Standing::Publishing);
        assert_eq!(alias_of(&g, "v10"), Some("v9".to_string()), "v10 published");
    }

    /// A re-baseline that turns a distinct published facade into an alias
    /// leaves its fixture obsolete, and only that one.
    #[test]
    fn a_rebaseline_that_merges_a_head_obsoletes_its_fixture() {
        let mut rows = features::feature_table();
        let union = union();
        let published = &["v4_6", "v4_7", "v5", "v6", "v7", "v8", "v9"];
        let disk = published_disk(published, 1);
        let present: Vec<String> = ["v4_6", "v5", "v8", "v9"].map(String::from).into();
        let g = generation(&rows, &union, freeze(published, 1), &disk).unwrap();
        assert_eq!(obsolete_fixtures(&g, &present), Vec::<String>::new());
        // Without the nullness interval, `v9`'s surface is `v8`'s.
        let row = rows
            .iter_mut()
            .find(|r| r.kinds().any(|k| k == "WITH_NULL_TYPE"))
            .unwrap();
        row.engaged = Engaged::Deferred("test".to_string());
        let g = generation(&rows, &union, freeze(published, 2), &disk).unwrap();
        assert_eq!(
            g.locks[6].1.alias_of(),
            Some("v8"),
            "{:?}",
            g.locks[6].1.alias_of()
        );
        assert_eq!(
            obsolete_fixtures(&g, &present),
            [consumer::fixture_path("v9")]
        );
    }

    /// The property table calls the union accessor a compat line delegates
    /// to, not the frozen name.
    #[test]
    fn the_property_table_follows_a_compat_delegate() {
        let rows = features::feature_table();
        let union = union();
        let disk = v8_disk(|lock| {
            let Line::Accessor {
                owner, name, shape, ..
            } = some_accessor(lock)
            else {
                unreachable!()
            };
            lock.lines.insert(Line::Accessor {
                owner,
                name: format!("old_{name}"),
                shape,
                delegate: Some(name),
            });
        });
        let out = outputs_with(&rows, &union, &versions(), freeze(UP_TO_V8, 1), &|v| {
            disk.get(&v.module()).cloned()
        })
        .unwrap_or_else(|e| panic!("{e}"));
        let Line::Accessor { owner, name, .. } = some_accessor(&lock::parse(&disk["v8"]).unwrap())
        else {
            unreachable!()
        };
        let needle = format!("accessor_case!(v8, {owner}, old_{name}, {name}, ");
        assert!(out[MATRIX_PATH].contains(&needle), "no {needle:?}");
    }

    /// A frozen alias stays an alias only while its version's derived surface
    /// equals its head's.
    #[test]
    fn a_frozen_alias_whose_surface_moves_is_refused() {
        let mut rows = features::feature_table();
        let union = union();
        let disk = published_disk(UP_TO_V8, 1);
        assert!(generation(&rows, &union, freeze(UP_TO_V8, 1), &disk).is_ok());
        let row = rows
            .iter_mut()
            .find(|r| r.kinds().any(|k| k == "WHILE_BANG_EXPR"))
            .unwrap();
        row.introduced = "4.7".to_string();
        let e = generation(&rows, &union, freeze(UP_TO_V8, 1), &disk).unwrap_err();
        assert!(e.contains("frozen as an alias of `v4_6`"), "{e}");
        assert!(e.contains("WhileBangExpr"), "{e}");
    }

    /// The extensibility proof in miniature: a new language version, 12.0,
    /// whose one feature introduces one new kind of expression, and a new
    /// union accessor on a node every facade has. Every published facade's
    /// generated files are byte-identical with and without them, nothing
    /// fails, and each frozen head may opt into the accessor; the new
    /// facade's surface is the latest one's plus exactly that variant, and
    /// preview's is its own plus the variant and the accessor.
    #[test]
    fn a_new_language_version_leaves_every_published_facade_byte_identical() {
        let rows = features::feature_table();
        let union = union();
        let versions = versions();
        let real = outputs_with(&rows, &union, &versions, CURRENT_FREEZE, &checked_in_lock)
            .unwrap_or_else(|e| panic!("{e}"));

        let mut fake_rows = rows.clone();
        fake_rows.push(FeatureRow {
            name: "SyntheticFeature".to_string(),
            introduced: "12.0".to_string(),
            stages: vec![Stage::Parse],
            effects: vec![Effect::Diagnose],
            borzoi: vec![Modelling::Kind("SYNTHETIC_EXPR".to_string())],
            engaged: Engaged::Live,
            sites: Vec::new(),
            display_name: "synthetic feature".to_string(),
            notes: String::new(),
        });
        let added: BTreeSet<Line> = [
            Line::Node {
                node: "SyntheticExpr".to_string(),
                kind: "SYNTHETIC_EXPR".to_string(),
            },
            Line::Enum {
                name: "Expr".to_string(),
                variant: "Synthetic".to_string(),
                node: "SyntheticExpr".to_string(),
            },
        ]
        .into();
        let mut fake_union = union.clone();
        fake_union.lines.extend(added.iter().cloned());
        let accessor = |delegate: Option<String>| Line::Accessor {
            owner: "ParenType".to_string(),
            name: "synthetic_child".to_string(),
            shape: Shape::Option(Elem::Enum("Expr".to_string())),
            delegate,
        };
        fake_union.lines.insert(accessor(None));
        let v12 = Version {
            text: "12.0",
            variant: "V12_0",
            decimal: 12.0,
        };
        let mut fake_versions = versions.clone();
        fake_versions.insert(versions.len() - 1, v12);
        let fake = outputs_with(
            &fake_rows,
            &fake_union,
            &fake_versions,
            CURRENT_FREEZE,
            &checked_in_lock,
        )
        .unwrap_or_else(|e| panic!("{e}"));

        assert!(!PUBLISHED.is_empty());
        for module in PUBLISHED {
            let v = versions.iter().find(|v| v.module() == *module).unwrap();
            for path in [v.code_path(), v.lock_path(), consumer::fixture_path(module)] {
                assert_eq!(
                    real.get(&path),
                    fake.get(&path),
                    "{path} moved with a new language version"
                );
            }
        }

        let generation_of = |rows: &[FeatureRow], union: &Lock, versions: &[Version]| {
            facade_locks(&Inputs {
                rows,
                union,
                versions,
                freeze: CURRENT_FREEZE,
                on_disk: &checked_in_lock,
            })
            .unwrap()
        };
        let real_generation = generation_of(&rows, &union, &versions);
        let fake_generation = generation_of(&fake_rows, &fake_union, &fake_versions);
        // The new facade against the latest numbered one, and preview against
        // itself without the new version.
        for (module, generation, against) in [
            (
                "v12",
                &fake_generation,
                versions[versions.len() - 2].module(),
            ),
            ("preview", &real_generation, "preview".to_string()),
        ] {
            let lock = resolved(&fake_generation, module);
            let before = resolved(generation, &against);
            let mut expected = added.clone();
            if module == "preview" {
                expected.insert(accessor(Some("synthetic_child".to_string())));
            }
            assert_eq!(
                lock.lines
                    .difference(&before.lines)
                    .cloned()
                    .collect::<BTreeSet<_>>(),
                expected,
                "{module}"
            );
            assert!(before.lines.is_subset(&lock.lines), "{module}");
        }
        assert!(fake[&v12.code_path()].contains("Synthetic(SyntheticExpr)"));
        let frozen_heads: BTreeSet<String> = fake_generation
            .locks
            .iter()
            .zip(&fake_generation.standings)
            .filter(|((_, l), s)| **s == Standing::Frozen && l.alias_of().is_none())
            .map(|((v, _), _)| v.module())
            .collect();
        let offered: BTreeSet<String> = fake_generation
            .opt_in
            .iter()
            .filter(|(_, l)| *l == accessor(Some("synthetic_child".to_string())))
            .map(|(m, _)| m.clone())
            .collect();
        assert!(!frozen_heads.is_empty());
        assert_eq!(offered, frozen_heads);
    }

    /// The lock of `module`'s distinct facade in `generation`.
    fn resolved<'a>(generation: &'a Generation, module: &str) -> &'a Lock {
        let lock = |module: &str| {
            &generation
                .locks
                .iter()
                .find(|(v, _)| v.module() == module)
                .unwrap_or_else(|| panic!("no facade {module}"))
                .1
        };
        let own = lock(module);
        own.alias_of().map_or(own, lock)
    }

    /// The facade generator refuses, naming it, a facade type or accessor the
    /// union cannot back mechanically.
    #[test]
    fn a_lock_the_union_cannot_back_is_refused() {
        let rows = features::feature_table();
        let union = union();
        let version = versions()[5];
        let mut lock = derived_lock(&union, &rows, version.decimal);
        // A renamed delegate the union lacks.
        let accessor = lock
            .lines
            .iter()
            .find(|l| matches!(l, Line::Accessor { .. }))
            .cloned()
            .unwrap();
        lock.lines.remove(&accessor);
        let Line::Accessor {
            owner, name, shape, ..
        } = accessor
        else {
            unreachable!()
        };
        lock.lines.insert(Line::Accessor {
            owner: owner.clone(),
            name: name.clone(),
            shape,
            delegate: Some("no_such_accessor".to_string()),
        });
        let errors = generate_facade(&version, &lock, &union, &[&version]).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains(&format!("{owner}::{name}"))),
            "{errors:#?}"
        );
    }

    /// An accessor that returns a narrowed dispatch enum bare cannot drop an
    /// out-of-surface value, so it needs a compat shim and the generator says so.
    #[test]
    fn a_bare_narrowed_return_needs_a_shim() {
        let rows = features::feature_table();
        let mut union = union();
        union.lines.insert(Line::Accessor {
            owner: "ParenType".to_string(),
            name: "bare_inner".to_string(),
            shape: Shape::One(Elem::Enum("Type".to_string())),
            delegate: None,
        });
        let version = versions()[5];
        let lock = derived_lock(&union, &rows, version.decimal);
        let errors = generate_facade(&version, &lock, &union, &[&version]).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("ParenType::bare_inner") && e.contains("compat shim")),
            "{errors:#?}"
        );
    }
}
