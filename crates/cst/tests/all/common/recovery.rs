//! Grading a recovered tree against FCS's, when at least one side reported a
//! parse error.
//!
//! Two parsers that both reject a file need not recover the same way: where
//! the damage is, each may legitimately build a different placeholder. So the
//! comparison is not whole-tree equality but a relation over **units**, the
//! pieces of a file that can be damaged independently:
//!
//! * each module/namespace header ([`UnitShape::ImplModule`] /
//!   [`UnitShape::SigModule`], body declarations elided),
//! * each module-level declaration, recursively through nested modules; a
//!   nested module is a unit for its *header* (its body elided), and every
//!   body declaration is a unit of its own.
//!
//! Every unit is keyed by where it sits ([`UnitKey`]): the chain of enclosing
//! modules, its start offset, and its kind. Both sides key by the FCS-faithful
//! ranges the range audit already proves on clean files.
//!
//! **Damage** ([`Damage`]) is the union of both sides' error spans, each one
//! widened back to the end of the last significant token before it. The widening
//! is because both parsers habitually report an error at the token *after* the
//! damage (FCS's "unexpected keyword `let`" sits on the next declaration's
//! `let`), so a unit that ends where an error begins is damaged too. Spans are
//! closed intervals for the same reason.
//!
//! A unit is **damaged** when its range touches the damage on *either* side
//! (the two parsers often disagree about where a broken declaration ends).
//! The relation [`Relation::OutsideDamage`] holds when
//!
//! * the two sides' undamaged units are the *same set of keys* and every pair
//!   is shape-equal, and
//! * every unit whose *start* is undamaged starts on both sides (the boundary
//!   check, which catches the remains of an abandoned construct parsed as
//!   declarations of their own: each sits next to one of our errors, so the
//!   unit is damaged, but where it begins is not).
//!
//! It is symmetric on purpose. FCS ⊆ ours alone would let our parser invent an
//! extra clean-looking declaration out of a damaged one (which name resolution
//! would then believe); ours ⊆ FCS alone would let it drop a declaration FCS
//! kept. The one exemption is past FCS's reach ([`fcs_reach`]): FCS drops the
//! rest of a module after an error it cannot resynchronise from, so one of our
//! units starting there, on its module's offside column, has nothing to be
//! compared with. It is counted (`beyond-fcs`), not graded.
//!
//! [`Relation::Exact`] is the stronger whole-tree equality the clean-file
//! differential uses, recovery placeholders included.
//!
//! A unit whose shape one side's normaliser does not model is *not compared*
//! (there is nothing to compare it with), but it is counted: a verdict carries
//! how many undamaged units were compared out of how many there were, and the
//! manifest pins both numbers. A mutation that hides a declaration by growing
//! the damage, or by making it unmodelled, therefore moves a pinned line even
//! when the relation itself still holds.
//!
//! `docs/parser-recovery-oracle.md` describes the gates built on this and what
//! they still find divergent.

use std::collections::BTreeMap;
use std::ops::Range;

use borzoi_cst::parser::Parse;
use borzoi_cst::syntax::{
    AstNode, ImplFile, ModuleDecl, ModuleOrNamespace, SigDecl, SigFile, SyntaxKind,
};
use serde::Deserialize;
use serde_json::Value;

use super::LineIndex;
use super::catch_unwind_silent;
use super::normalised_ast::{
    NormalisedDecl, NormalisedModule, NormalisedSigDecl, NormalisedSigModule, fcs_impl_decl_unit,
    fcs_impl_module_header, fcs_sig_decl_unit, fcs_sig_module_header, impl_decl_unit,
    impl_module_header, normalise_fcs_dump, normalise_parse, sig_decl_unit, sig_module_header,
};
use super::range_audit::{
    fcs_byte_range, fcs_impl_decl_range, fcs_sig_decl_range, impl_decl_ast_range, impl_decl_kind,
    is_light_hash_directive, node_source_range, sig_decl_ast_range, sig_decl_kind,
};

/// One step of a unit's enclosing-module chain.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// The `n`th top-level module/namespace of the file.
    Module(usize),
    /// The nested module starting at this byte offset.
    Nested(usize),
}

/// Where a unit sits: its enclosing modules, its start offset (a top-level
/// module header's is its ordinal instead), and its FCS case name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnitKey {
    pub scope: Vec<Scope>,
    pub start: usize,
    pub kind: String,
}

impl std::fmt::Display for UnitKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for s in &self.scope {
            match s {
                Scope::Module(i) => write!(f, "module[{i}]/")?,
                Scope::Nested(at) => write!(f, "nested@{at}/")?,
            }
        }
        write!(f, "{}@{}", self.kind, self.start)
    }
}

/// A unit's normalised shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitShape {
    ImplModule(NormalisedModule),
    SigModule(NormalisedSigModule),
    Decl(NormalisedDecl),
    SigDecl(NormalisedSigDecl),
}

/// One comparison unit. `shape` is `Err` (the normaliser's panic message) when
/// that side's normaliser does not model the construct.
#[derive(Debug, Clone)]
pub struct Unit {
    pub key: UnitKey,
    pub range: Range<usize>,
    pub shape: Result<UnitShape, String>,
}

fn project<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind_silent(f).map_err(|payload| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "<non-string panic>".to_string())
    })
}

// ---------------------------------------------------------------------------
// Our side
// ---------------------------------------------------------------------------

/// Every unit of our tree, in source order.
pub fn our_units(parse: &Parse) -> Vec<Unit> {
    let mut out = Vec::new();
    match parse.root.kind() {
        SyntaxKind::IMPL_FILE => {
            let file = ImplFile::cast(parse.root.clone()).expect("kind already checked");
            for (i, module) in file.modules().enumerate() {
                let decls: Vec<ModuleDecl> = module
                    .decls()
                    .filter(|d| !is_light_hash_directive(d.syntax()))
                    .collect();
                let first = decls.first().map(|d| impl_decl_ast_range(d).start);
                out.push(Unit {
                    key: UnitKey {
                        scope: Vec::new(),
                        start: i,
                        kind: "Module".to_string(),
                    },
                    range: header_range(&module, first),
                    shape: project(|| impl_module_header(&module)).map(UnitShape::ImplModule),
                });
                our_impl_decls(&decls, vec![Scope::Module(i)], &mut out);
            }
        }
        SyntaxKind::SIG_FILE => {
            let file = SigFile::cast(parse.root.clone()).expect("kind already checked");
            for (i, module) in file.modules().enumerate() {
                let decls: Vec<SigDecl> = module
                    .sig_decls()
                    .filter(|d| !is_light_hash_directive(d.syntax()))
                    .collect();
                let first = decls.first().map(|d| sig_decl_ast_range(d).start);
                out.push(Unit {
                    key: UnitKey {
                        scope: Vec::new(),
                        start: i,
                        kind: "Module".to_string(),
                    },
                    range: header_range(&module, first),
                    shape: project(|| sig_module_header(&module)).map(UnitShape::SigModule),
                });
                our_sig_decls(&decls, vec![Scope::Module(i)], &mut out);
            }
        }
        other => panic!("unexpected root kind {other:?}"),
    }
    out
}

/// A module header spans from the module's first significant token to its
/// first declaration (which may start earlier, at an XML doc comment).
fn header_range(module: &ModuleOrNamespace, first_decl: Option<usize>) -> Range<usize> {
    let whole = node_source_range(module.syntax());
    match first_decl {
        Some(first) => whole.start.min(first)..first,
        None => whole,
    }
}

fn our_impl_decls(decls: &[ModuleDecl], scope: Vec<Scope>, out: &mut Vec<Unit>) {
    for decl in decls {
        let range = impl_decl_ast_range(decl);
        let kind = impl_decl_kind(decl).to_string();
        let shape = project(|| impl_decl_unit(decl)).map(UnitShape::Decl);
        if let ModuleDecl::NestedModule(nested) = decl {
            let children: Vec<ModuleDecl> = nested
                .decls()
                .filter(|d| !is_light_hash_directive(d.syntax()))
                .collect();
            let body = children.first().map(|d| impl_decl_ast_range(d).start);
            out.push(Unit {
                key: UnitKey {
                    scope: scope.clone(),
                    start: range.start,
                    kind,
                },
                range: range.start..body.unwrap_or(range.end).max(range.start),
                shape,
            });
            let mut inner = scope.clone();
            inner.push(Scope::Nested(range.start));
            our_impl_decls(&children, inner, out);
        } else {
            out.push(Unit {
                key: UnitKey {
                    scope: scope.clone(),
                    start: range.start,
                    kind,
                },
                range,
                shape,
            });
        }
    }
}

fn our_sig_decls(decls: &[SigDecl], scope: Vec<Scope>, out: &mut Vec<Unit>) {
    for decl in decls {
        let range = sig_decl_ast_range(decl);
        let kind = sig_decl_kind(decl).to_string();
        let shape = project(|| sig_decl_unit(decl)).map(UnitShape::SigDecl);
        if let SigDecl::NestedModule(nested) = decl {
            let children: Vec<SigDecl> = nested
                .sig_decls()
                .filter(|d| !is_light_hash_directive(d.syntax()))
                .collect();
            let body = children.first().map(|d| sig_decl_ast_range(d).start);
            out.push(Unit {
                key: UnitKey {
                    scope: scope.clone(),
                    start: range.start,
                    kind,
                },
                range: range.start..body.unwrap_or(range.end).max(range.start),
                shape,
            });
            let mut inner = scope.clone();
            inner.push(Scope::Nested(range.start));
            our_sig_decls(&children, inner, out);
        } else {
            out.push(Unit {
                key: UnitKey {
                    scope: scope.clone(),
                    start: range.start,
                    kind,
                },
                range,
                shape,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// FCS side
// ---------------------------------------------------------------------------

/// Every unit of FCS's tree, in source order, or `Err` when the record's JSON
/// is too deep for `serde_json` to read as a [`Value`].
pub fn fcs_units(json: &str, source: &str) -> Result<Vec<Unit>, String> {
    let dump: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let tree = dump.get("ParseTree").ok_or("record has no ParseTree")?;
    let line_index = LineIndex::new(source);
    let mut out = Vec::new();
    let sig = match case_name(tree) {
        "ImplFile" => false,
        "SigFile" => true,
        other => panic!("unknown ParsedInput case {other:?}"),
    };
    let file = &fields(tree)[0];
    let modules = fields(file)[if sig { 3 } else { 4 }]
        .as_array()
        .expect("ParsedInput modules must be an array");
    for (i, module) in modules.iter().enumerate() {
        let mf = fields(module);
        let whole = fcs_byte_range(&mf[7], &line_index);
        let decls = mf[3].as_array().expect("module decls must be an array");
        let first = decls
            .first()
            .map(|d| fcs_decl_range(d, sig, &line_index).start);
        out.push(Unit {
            key: UnitKey {
                scope: Vec::new(),
                start: i,
                kind: "Module".to_string(),
            },
            range: match first {
                Some(first) => whole.start.min(first)..first,
                None => whole,
            },
            shape: if sig {
                project(|| fcs_sig_module_header(module)).map(UnitShape::SigModule)
            } else {
                project(|| fcs_impl_module_header(module)).map(UnitShape::ImplModule)
            },
        });
        fcs_decls(decls, sig, vec![Scope::Module(i)], &line_index, &mut out);
    }
    Ok(out)
}

fn fcs_decl_range(decl: &Value, sig: bool, line_index: &LineIndex<'_>) -> Range<usize> {
    let kind = case_name(decl);
    let f = fields(decl);
    let range = if sig {
        fcs_sig_decl_range(kind, f)
    } else {
        fcs_impl_decl_range(kind, f)
    };
    fcs_byte_range(range, line_index)
}

fn fcs_decls(
    decls: &[Value],
    sig: bool,
    scope: Vec<Scope>,
    line_index: &LineIndex<'_>,
    out: &mut Vec<Unit>,
) {
    for decl in decls {
        let kind = case_name(decl).to_string();
        let range = fcs_decl_range(decl, sig, line_index);
        let shape = if sig {
            project(|| fcs_sig_decl_unit(decl)).map(UnitShape::SigDecl)
        } else {
            project(|| fcs_impl_decl_unit(decl)).map(UnitShape::Decl)
        };
        if kind == "NestedModule" {
            let children = fields(decl)[2]
                .as_array()
                .expect("NestedModule decls must be an array");
            let body = children
                .first()
                .map(|d| fcs_decl_range(d, sig, line_index).start);
            out.push(Unit {
                key: UnitKey {
                    scope: scope.clone(),
                    start: range.start,
                    kind,
                },
                range: range.start..body.unwrap_or(range.end).max(range.start),
                shape,
            });
            let mut inner = scope.clone();
            inner.push(Scope::Nested(range.start));
            fcs_decls(children, sig, inner, line_index, out);
        } else {
            out.push(Unit {
                key: UnitKey {
                    scope: scope.clone(),
                    start: range.start,
                    kind,
                },
                range,
                shape,
            });
        }
    }
}

fn case_name(v: &Value) -> &str {
    v.get("Case")
        .and_then(Value::as_str)
        .expect("FCS union value missing Case string")
}

fn fields(v: &Value) -> &Vec<Value> {
    v.get("Fields")
        .and_then(Value::as_array)
        .expect("FCS union value missing Fields array")
}

// ---------------------------------------------------------------------------
// Damage
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Diagnostics {
    #[serde(rename = "Diagnostics")]
    diagnostics: Vec<Diagnostic>,
}

#[derive(Deserialize)]
struct Diagnostic {
    #[serde(rename = "Severity")]
    severity: String,
    #[serde(rename = "Range")]
    range: Value,
}

/// The byte spans of every error-severity diagnostic in an `fcs-dump ast`
/// record. Warnings are not damage: FCS builds the tree as if they were absent.
pub fn fcs_error_spans(json: &str, source: &str) -> Vec<Range<usize>> {
    let dump: Diagnostics = serde_json::from_str(json).expect("fcs-dump record has Diagnostics");
    let line_index = LineIndex::new(source);
    dump.diagnostics
        .iter()
        .filter(|d| d.severity == "Error")
        .map(|d| fcs_byte_range(&d.range, &line_index))
        .collect()
}

/// The damaged region of one file: closed byte intervals.
#[derive(Debug, Clone, Default)]
pub struct Damage {
    spans: Vec<(usize, usize)>,
}

impl Damage {
    /// The damage of `errors`, each widened back to the end of the last
    /// significant token of `parse` (a tree of the same source) that ends at
    /// or before it.
    pub fn new(parse: &Parse, errors: impl IntoIterator<Item = Range<usize>>) -> Self {
        let ends: Vec<usize> = parse
            .root
            .descendants_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| !t.kind().is_trivia() && !t.text_range().is_empty())
            .map(|t| usize::from(t.text_range().end()))
            .collect();
        let spans = errors
            .into_iter()
            .map(|e| {
                let start = e.start.min(e.end);
                let end = e.start.max(e.end);
                // `ends` is sorted: tokens come in source order.
                let i = ends.partition_point(|&x| x <= start);
                let widened = if i == 0 { start } else { ends[i - 1] };
                (widened, end)
            })
            .collect();
        Damage { spans }
    }

    /// Whether the closed interval `[r.start, r.end]` meets any damage.
    pub fn touches(&self, r: &Range<usize>) -> bool {
        self.spans.iter().any(|&(a, b)| r.start <= b && a <= r.end)
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// The first damaged offset, or `None` when nothing is damaged.
    pub fn start(&self) -> Option<usize> {
        self.spans.iter().map(|&(a, _)| a).min()
    }

    /// The last damaged offset, or `None` when nothing is damaged.
    pub fn end(&self) -> Option<usize> {
        self.spans.iter().map(|&(_, b)| b).max()
    }
}

// ---------------------------------------------------------------------------
// The relation
// ---------------------------------------------------------------------------

/// The strongest relation a recovered pair satisfies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relation {
    /// The whole normalised trees are equal.
    Exact,
    /// The undamaged units agree, key for key and shape for shape.
    OutsideDamage,
    /// An undamaged unit is missing, misplaced or different on one side.
    Divergent,
}

impl Relation {
    pub fn name(&self) -> &'static str {
        match self {
            Relation::Exact => "exact",
            Relation::OutsideDamage => "outside-damage",
            Relation::Divergent => "divergent",
        }
    }
}

/// The graded comparison of one recovered pair.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub relation: Relation,
    /// Undamaged unit pairs both normalisers model, and so were compared.
    pub compared: usize,
    /// Undamaged units on the FCS side.
    pub undamaged: usize,
    /// All units on the FCS side.
    pub total: usize,
    /// Our undamaged units FCS has no counterpart for because its tree stops
    /// short of them (see [`fcs_reach`]): not graded, and counted so that a
    /// change moving declarations out of FCS's reach still moves a pinned line.
    pub beyond_fcs: usize,
    /// The first offending key and why, for a [`Relation::Divergent`] verdict.
    pub first_divergence: Option<String>,
}

impl Verdict {
    /// The manifest token: `<relation> <compared>/<undamaged>/<total>`, then
    /// ` beyond-fcs:<n>` when any of our units lie past FCS's reach.
    pub fn token(&self) -> String {
        let mut token = format!(
            "{} {}/{}/{}",
            self.relation.name(),
            self.compared,
            self.undamaged,
            self.total
        );
        if self.beyond_fcs > 0 {
            token.push_str(&format!(" beyond-fcs:{}", self.beyond_fcs));
        }
        token
    }
}

/// Grade our recovered `parse` of `source` against FCS's `json` record.
///
/// `Err` only when FCS's record is unreadable as a tree (too deep for
/// `serde_json`).
pub fn grade(parse: &Parse, json: &str, source: &str) -> Result<Verdict, String> {
    let fcs = fcs_units(json, source)?;
    let ours = our_units(parse);
    let damage = Damage::new(
        parse,
        parse
            .errors
            .iter()
            .map(|e| e.span.clone())
            .chain(fcs_error_spans(json, source)),
    );
    let mut verdict = compare_units(&fcs, &ours, &damage, source);
    if verdict.relation == Relation::OutsideDamage {
        let exact = matches!(
            (project(|| normalise_parse(parse)), project(|| normalise_fcs_dump(json))),
            (Ok(a), Ok(b)) if a == b
        );
        if exact {
            verdict.relation = Relation::Exact;
        }
    }
    Ok(verdict)
}

/// The undamaged units of `fcs` and `ours`, compared under `damage`.
pub fn compare_units(fcs: &[Unit], ours: &[Unit], damage: &Damage, source: &str) -> Verdict {
    // A unit is damaged when it touches the damage on *either* side: the two
    // parsers disagree about where a recovered declaration ends (FCS often
    // stops a broken type's range short of the error that broke it), and the
    // relation must not read that disagreement as one side keeping the unit
    // clean.
    let damaged: std::collections::BTreeSet<&UnitKey> = fcs
        .iter()
        .chain(ours)
        .filter(|u| damage.touches(&u.range))
        .map(|u| &u.key)
        .collect();
    let undamaged = |units: &[Unit]| -> BTreeMap<UnitKey, Option<UnitShape>> {
        units
            .iter()
            .filter(|u| !damaged.contains(&u.key))
            .map(|u| (u.key.clone(), u.shape.clone().ok()))
            .collect()
    };
    let f = undamaged(fcs);
    let o = undamaged(ours);
    let reach = fcs_reach(fcs);
    let mut compared = 0;
    let mut beyond_fcs = 0;
    let mut first_divergence = None;
    for (key, fs) in &f {
        match o.get(key) {
            None => {
                first_divergence.get_or_insert_with(|| format!("{key}: only FCS has it clean"));
            }
            Some(os) => {
                if let (Some(fs), Some(os)) = (fs, os) {
                    compared += 1;
                    if fs != os {
                        first_divergence.get_or_insert_with(|| {
                            format!("{key}: shapes differ\n  ours: {os:?}\n  fcs:  {fs:?}")
                        });
                    }
                }
            }
        }
    }
    for key in o.keys() {
        if f.contains_key(key) {
            continue;
        }
        if is_beyond(key, &reach) && is_aligned(key, ours, source) {
            beyond_fcs += 1;
        } else {
            first_divergence.get_or_insert_with(|| format!("{key}: only ours has it clean"));
        }
    }
    // Boundaries: a unit whose *start* is undamaged must start on both sides,
    // even when the rest of it is damaged. This is what catches the remains of
    // an abandoned construct parsed as declarations of their own: each sits
    // next to one of our own errors, so the whole unit is damaged, but where
    // it begins is not, and FCS begins nothing there.
    let starts = |units: &[Unit]| -> std::collections::BTreeSet<UnitKey> {
        units
            .iter()
            .filter(|u| !damage.touches(&(u.range.start..u.range.start)))
            .map(|u| u.key.clone())
            .collect()
    };
    let fs = starts(fcs);
    let os = starts(ours);
    let all_ours: std::collections::BTreeSet<&UnitKey> = ours.iter().map(|u| &u.key).collect();
    let all_fcs: std::collections::BTreeSet<&UnitKey> = fcs.iter().map(|u| &u.key).collect();
    for key in &fs {
        if !all_ours.contains(key) {
            first_divergence.get_or_insert_with(|| format!("{key}: only FCS starts a unit here"));
        }
    }
    for key in &os {
        if !all_fcs.contains(key) && !(is_beyond(key, &reach) && is_aligned(key, ours, source)) {
            first_divergence.get_or_insert_with(|| format!("{key}: only ours starts a unit here"));
        }
    }
    Verdict {
        relation: if first_divergence.is_some() {
            Relation::Divergent
        } else {
            Relation::OutsideDamage
        },
        compared,
        undamaged: f.len(),
        total: fcs.len(),
        beyond_fcs,
        first_divergence,
    }
}

/// How far into each top-level module FCS's tree reaches: the end of its last
/// declaration unit there (at any depth), or nowhere (offset 0) when it kept
/// none — FCS ranges an emptied anonymous module at the end of the file, which
/// is no measure of what it read.
///
/// FCS's implementation-file `recover` arm drops the rest of a module after an
/// error it cannot resynchronise from, so a declaration past that point has no
/// counterpart in FCS's tree, and no verdict FCS can give about it.
fn fcs_reach(fcs: &[Unit]) -> BTreeMap<usize, usize> {
    let mut reach = BTreeMap::new();
    for u in fcs {
        match u.key.scope.first() {
            None => {
                reach.entry(u.key.start).or_insert(0);
            }
            Some(Scope::Module(i)) => {
                let r = reach.entry(*i).or_insert(u.range.end);
                *r = (*r).max(u.range.end);
            }
            Some(Scope::Nested(_)) => unreachable!("a unit's scope starts at a top-level module"),
        }
    }
    reach
}

/// The byte column of `offset` in `source`.
fn column(source: &str, offset: usize) -> usize {
    let offset = offset.min(source.len());
    offset - source[..offset].rfind('\n').map_or(0, |nl| nl + 1)
}

/// Whether our unit `key` starts at the column of the first declaration of its
/// enclosing module — the offside line every declaration of that module sits
/// on. A unit past FCS's reach is only exempt from grading when it does: a
/// misaligned one is the debris of a construct our recovery abandoned (a
/// member body spilled out of its type as a module-level expression), and is
/// a divergence whether or not FCS kept anything there.
fn is_aligned(key: &UnitKey, ours: &[Unit], source: &str) -> bool {
    ours.iter()
        .find(|u| u.key.scope == key.scope)
        .is_some_and(|first| column(source, first.key.start) == column(source, key.start))
}

/// Whether our unit `key` starts where FCS's tree no longer reaches (see
/// [`fcs_reach`]). A unit in a module FCS does not have at all is not beyond
/// it: that is a module-structure divergence.
fn is_beyond(key: &UnitKey, reach: &BTreeMap<usize, usize>) -> bool {
    match key.scope.first() {
        Some(Scope::Module(i)) => reach.get(i).is_some_and(|&end| key.start >= end),
        _ => false,
    }
}

/// Grade our recovered tree of the implementation file `source` against FCS's,
/// asserting the relation holds (exactly or outside the damage) and that at
/// least `min_compared` undamaged units were compared, so the case cannot pass
/// by damaging everything. Returns the verdict for further assertions.
#[track_caller]
pub fn assert_recovered_trees_agree(source: &str, min_compared: usize) -> Verdict {
    let mut tmp = tempfile::NamedTempFile::with_suffix(".fs").expect("create tempfile");
    std::io::Write::write_all(&mut tmp, source.as_bytes()).expect("write source");
    let json = super::fcs_ast_batch(tmp.path());
    let parse = borzoi_cst::parser::parse(source);
    let verdict = grade(&parse, &json, source).expect("FCS record is readable");
    assert!(
        verdict.relation != Relation::Divergent,
        "recovered trees diverge for {source:?}: {}\n{}\n  our errors: {:?}",
        verdict.token(),
        verdict.first_divergence.as_deref().unwrap_or(""),
        parse.errors,
    );
    assert!(
        verdict.compared >= min_compared,
        "only {} undamaged units compared for {source:?} (want ≥ {min_compared}): {}",
        verdict.compared,
        verdict.token(),
    );
    verdict
}
