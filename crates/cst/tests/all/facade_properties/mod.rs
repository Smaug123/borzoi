//! Properties of the per-version typed facades (`docs/ast-versioning-plan.md`
//! P2–P4), over every facade, node type, dispatch enum and accessor.
//!
//! `matrix.rs` is generated from the facades' locks (`tools/astgen`), so the
//! properties range over everything a lock declares without a hand-written
//! list. On every node of a tree, for every distinct facade:
//!
//! - **P2 (totality).** A node is a *hole* in a facade when some union node
//!   type casts it and none of the facade's does. A hole's kind is exactly one
//!   the feature table puts outside the facade's versions
//!   ([`kind_in_surface`]), and, in a parse at a version the facade serves,
//!   every hole lies under a gate diagnostic for its own kind's feature: one
//!   whose [`gate`](field@ParseDiagnostic::gate) is that feature and whose
//!   class is [`DiagnosticClass::of_gate`] of it (`Legality { feature }`, or
//!   `Recovery` for a feature FCS reshapes, as nullness). So a parse with no
//!   gate diagnostic reads totally.
//! - **P3 (round trip).** A facade cast, lowered to the substrate or to the
//!   union, is the same node; a node type or enum casts exactly what the
//!   union's of the same name does, holes aside.
//! - **P4 (no coarsening).** Wherever a facade casts a node, its value prints
//!   (`Debug`) exactly as the union's does: the same variant over the same
//!   node, so no two kinds a facade tells apart collapse in the union, and
//!   every two facades agree where both cast. And wherever an accessor's owner
//!   has no hole beneath it, the facade's accessor returns what the union's
//!   does.
//!
//! The trees are hand-written fixtures (one per live `Kind` row of the feature
//! table, and ordinary code), generated programs, and a sample of the corpus
//! (all of it in the ignored [`corpus`] sweep), each parsed at the versions the
//! facades serve.
//!
//! Every diagnostic is also held to the gate field's own contract: a gate's
//! class is [`DiagnosticClass::of_gate`] of its feature, and a `Legality`
//! diagnostic is a gate on the feature it names.
//!
//! A frozen facade may depart from the union and the table where astgen's
//! `ACCEPTED_DIVERGENCES` says so. The table carries what each acceptance
//! touches ([`FacadeCase::exempt_kinds`], [`FacadeCase::exempt_values`]), and
//! the properties hold those kinds and accessors to the round trip alone.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{
    DiagnosticClass, FileKind, Parse, ParseDiagnostic, ParseOptions, parse_with_options,
};
use borzoi_cst::syntax::projection::first_out_of_surface;
use borzoi_cst::syntax::{self, AstNode, SyntaxKind, SyntaxNode, kind_feature, kind_in_surface};
use proptest::prelude::*;

/// One language version's facade module, through `Parse::view`.
pub struct View {
    pub module: &'static str,
    /// The distinct facade it is (itself, or the one it aliases).
    pub head: &'static str,
    pub version: LanguageVersion,
    pub versions: &'static [LanguageVersion],
    pub view: fn(&Parse) -> bool,
}

macro_rules! view_case {
    ($module:ident, $head:ident, $version:expr) => {
        View {
            module: stringify!($module),
            head: stringify!($head),
            version: $version,
            versions: <::borzoi_cst::syntax::$module::ParsedInput as ::borzoi_cst::syntax::Facade>::VERSIONS,
            view: |p| p.view::<::borzoi_cst::syntax::$module::ParsedInput>().is_ok(),
        }
    };
}

/// A union node type.
pub struct UnionNode {
    pub can_cast: fn(SyntaxKind) -> bool,
}

macro_rules! union_node {
    ($t:ident) => {
        UnionNode {
            can_cast: <::borzoi_cst::syntax::$t as ::borzoi_cst::syntax::AstNode>::can_cast,
        }
    };
}

/// A facade cast's lowering: the node it wraps, its union value's node, and
/// how it prints.
type Lowered = (SyntaxNode, SyntaxNode, String);

/// One node type or dispatch enum of a facade, beside the union's.
pub struct Cast {
    pub name: &'static str,
    pub can_cast: fn(SyntaxKind) -> bool,
    pub union_can_cast: fn(SyntaxKind) -> bool,
    pub facade: fn(&SyntaxNode) -> Option<Lowered>,
    pub union: fn(&SyntaxNode) -> Option<String>,
}

macro_rules! cast_case {
    ($module:ident, $t:ident) => {
        Cast {
            name: stringify!($t),
            can_cast:
                <::borzoi_cst::syntax::$module::$t as ::borzoi_cst::syntax::AstNode>::can_cast,
            union_can_cast: <::borzoi_cst::syntax::$t as ::borzoi_cst::syntax::AstNode>::can_cast,
            facade: |n| {
                <::borzoi_cst::syntax::$module::$t as ::borzoi_cst::syntax::AstNode>::cast(
                    n.clone(),
                )
                .map(|x| {
                    (
                        ::borzoi_cst::syntax::AstNode::syntax(&x).clone(),
                        ::borzoi_cst::syntax::AstNode::syntax(&x.to_union()).clone(),
                        format!("{x:?}"),
                    )
                })
            },
            union: |n| {
                <::borzoi_cst::syntax::$t as ::borzoi_cst::syntax::AstNode>::cast(n.clone())
                    .map(|x| format!("{x:?}"))
            },
        }
    };
}

/// One accessor of a facade: on a node its owner casts, the facade's result
/// and the union accessor's it delegates to (its lock line's `union.<method>`),
/// printed.
pub struct Accessor {
    pub owner: &'static str,
    pub name: &'static str,
    /// Its return shape, as the lock spells it.
    pub shape: &'static str,
    pub call: fn(&SyntaxNode) -> Option<(String, String)>,
}

macro_rules! accessor_case {
    ($module:ident, $owner:ident, $name:ident, $delegate:ident, one, $shape:literal) => {
        Accessor {
            owner: stringify!($owner),
            shape: $shape,
            name: stringify!($name),
            call: |n| {
                let facade =
                    <::borzoi_cst::syntax::$module::$owner as ::borzoi_cst::syntax::AstNode>::cast(
                        n.clone(),
                    )?;
                let union = <::borzoi_cst::syntax::$owner as ::borzoi_cst::syntax::AstNode>::cast(
                    n.clone(),
                )?;
                Some((
                    format!("{:?}", facade.$name()),
                    format!("{:?}", union.$delegate()),
                ))
            },
        }
    };
    ($module:ident, $owner:ident, $name:ident, $delegate:ident, iter, $shape:literal) => {
        Accessor {
            owner: stringify!($owner),
            shape: $shape,
            name: stringify!($name),
            call: |n| {
                let facade =
                    <::borzoi_cst::syntax::$module::$owner as ::borzoi_cst::syntax::AstNode>::cast(
                        n.clone(),
                    )?;
                let union = <::borzoi_cst::syntax::$owner as ::borzoi_cst::syntax::AstNode>::cast(
                    n.clone(),
                )?;
                Some((
                    format!("{:?}", facade.$name().collect::<Vec<_>>()),
                    format!("{:?}", union.$delegate().collect::<Vec<_>>()),
                ))
            },
        }
    };
}

/// A distinct facade: the versions it serves, its casts and its accessors.
pub struct FacadeCase {
    pub module: &'static str,
    pub versions: &'static [LanguageVersion],
    pub casts: &'static [Cast],
    pub accessors: &'static [Accessor],
    /// Kinds this frozen facade's accepted divergences let it read apart
    /// from the union and the table (astgen's `facade::Exemption`).
    pub exempt_kinds: &'static [SyntaxKind],
    /// Value enums its accepted divergences let it read apart from the
    /// union: an accessor whose shape names one is not compared.
    pub exempt_values: &'static [&'static str],
}

mod matrix;

use matrix::{FACADES, UNION_NODES, VIEWS};

/// Every language version, ascending.
fn all_versions() -> Vec<LanguageVersion> {
    let mut all = LanguageVersion::NUMBERED.to_vec();
    all.push(LanguageVersion::Preview);
    all
}

fn parse_at(src: &str, file_kind: FileKind, lang: LanguageVersion) -> Parse {
    let symbols = HashSet::new();
    parse_with_options(
        src,
        ParseOptions {
            file_kind,
            symbols: &symbols,
            lang,
        },
    )
}

fn range_of(node: &SyntaxNode) -> Range<usize> {
    let range = node.text_range();
    usize::from(range.start())..usize::from(range.end())
}

/// Per-kind lookups, built once per facade as kinds are met.
#[derive(Default)]
struct Index {
    /// The casts whose `can_cast` takes the kind.
    casts: HashMap<SyntaxKind, Vec<usize>>,
    /// The accessors whose owner casts the kind.
    accessors: HashMap<SyntaxKind, Vec<usize>>,
    /// The union casts the kind's node type does (some union node type casts it).
    in_union: HashMap<SyntaxKind, bool>,
    /// The accessors that have run on a hole-free owner.
    ran: HashSet<usize>,
}

impl Index {
    fn casts(&mut self, facade: &FacadeCase, kind: SyntaxKind) -> &[usize] {
        self.casts.entry(kind).or_insert_with(|| {
            (0..facade.casts.len())
                .filter(|&i| (facade.casts[i].can_cast)(kind))
                .collect()
        })
    }

    fn accessors(&mut self, facade: &FacadeCase, kind: SyntaxKind) -> &[usize] {
        self.accessors.entry(kind).or_insert_with(|| {
            let owners: HashSet<&str> = facade
                .casts
                .iter()
                .filter(|c| (c.can_cast)(kind))
                .map(|c| c.name)
                .collect();
            (0..facade.accessors.len())
                .filter(|&i| owners.contains(facade.accessors[i].owner))
                .collect()
        })
    }

    fn in_union(&mut self, kind: SyntaxKind) -> bool {
        *self
            .in_union
            .entry(kind)
            .or_insert_with(|| UNION_NODES.iter().any(|n| (n.can_cast)(kind)))
    }
}

/// Holds one diagnostic to the contract of its
/// [`gate`](field@ParseDiagnostic::gate) field: a gate is on a feature newer
/// than the parse's version and classed
/// [`DiagnosticClass::of_gate`] of it, and a `Legality` diagnostic is a gate
/// on the feature it names.
pub fn check_gate_field(d: &ParseDiagnostic, lang: LanguageVersion, what: &str) {
    if let Some(feature) = d.gate {
        assert!(
            !lang.supports(feature),
            "{what}: a gate on {feature:?} at {lang}, which supports it: {d:?}",
        );
        assert_eq!(
            d.class,
            DiagnosticClass::of_gate(feature),
            "{what}: a gate on {feature:?} is classed apart from the table: {d:?}",
        );
    }
    if let DiagnosticClass::Legality { feature } = d.class {
        assert_eq!(
            d.gate,
            Some(feature),
            "{what}: a Legality diagnostic that is no gate on its feature: {d:?}",
        );
    }
}

/// Whether the facade's accessor is held to the union's: not when its shape
/// names a value enum an accepted divergence exempts.
fn compared(facade: &FacadeCase, accessor: &Accessor) -> bool {
    !accessor
        .shape
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| facade.exempt_values.contains(&word))
}

/// Checks P2–P4 on one parse for every distinct facade, panicking with `what`
/// and the facade on the first violation. Returns how many holes it met.
fn check_parse(parse: &Parse, what: &str, indexes: &mut [Index], only_serving: bool) -> usize {
    check_parse_with(FACADES, parse, what, indexes, only_serving)
}

/// [`check_parse`] over `facades`.
///
/// A node whose kind the facade's accepted divergences exempt
/// ([`FacadeCase::exempt_kinds`]) is held to nothing beyond P3's round trip of
/// the casts the facade makes of it: it may be a hole the table does not
/// predict, with no gate over it, or a kind the facade's dispatch enum leaves
/// out though the union's holds it. Like a hole, it keeps the accessors of
/// the nodes above it from being compared; its own are compared, so an
/// exemption costs no accessor its coverage.
fn check_parse_with(
    facades: &[FacadeCase],
    parse: &Parse,
    what: &str,
    indexes: &mut [Index],
    only_serving: bool,
) -> usize {
    for d in parse.errors.iter().chain(&parse.warnings) {
        check_gate_field(d, parse.lang, what);
    }
    let mut holes_met = 0;
    let nodes: Vec<SyntaxNode> = parse.root.descendants().collect();
    for (facade, index) in facades.iter().zip(indexes.iter_mut()) {
        let module = facade.module;
        if only_serving && !facade.versions.contains(&parse.lang) {
            continue;
        }
        let surface_version = facade.versions[0];
        let exempt = |kind: SyntaxKind| facade.exempt_kinds.contains(&kind);
        // Holes, the exempt nodes, and every node with either at or below it.
        let mut holes = Vec::new();
        let mut exempted = Vec::new();
        for node in &nodes {
            let kind = node.kind();
            if exempt(kind) {
                exempted.push(node.clone());
                continue;
            }
            let facade_casts = !index.casts(facade, kind).is_empty();
            let hole = index.in_union(kind) && !facade_casts;
            assert_eq!(
                hole,
                index.in_union(kind) && !kind_in_surface(kind, surface_version),
                "{what}: {module}: {kind:?} is a hole iff the table puts it outside {surface_version}",
            );
            if hole {
                holes.push(node.clone());
            }
        }
        if exempted.is_empty() {
            assert_eq!(
                first_out_of_surface(&parse.root, surface_version),
                holes.first().cloned(),
                "{what}: {module}: the projection gate finds the first hole",
            );
        }
        holes_met += holes.len();
        let mut above_hole: HashSet<SyntaxNode> = HashSet::new();
        for hole in &holes {
            above_hole.extend(hole.ancestors());
        }
        // An exempt node's own accessors read what the union's do, but an
        // accessor above it may reach it through a narrowed type.
        for node in &exempted {
            above_hole.extend(node.ancestors().skip(1));
        }

        // P2.
        if facade.versions.contains(&parse.lang) {
            for hole in &holes {
                let feature = kind_feature(hole.kind()).unwrap_or_else(|| {
                    panic!("{what}: {module}: the hole {hole:?} has no feature in the table")
                });
                let range = range_of(hole);
                let covered = parse.errors.iter().any(|d| {
                    d.gate == Some(feature)
                        && d.class == DiagnosticClass::of_gate(feature)
                        && range.start <= d.span.start
                        && d.span.end <= range.end
                });
                assert!(
                    covered,
                    "{what}: {module}: the hole {hole:?} (feature {feature:?}) lies under no \
                     gate diagnostic at {}: {:#?}",
                    parse.lang, parse.errors,
                );
            }
        }

        for node in &nodes {
            let kind = node.kind();
            // P3 and P4 over the casts.
            for &i in index.casts(facade, kind).to_vec().iter() {
                let cast = &facade.casts[i];
                let (inner, lowered, printed) = (cast.facade)(node).unwrap_or_else(|| {
                    panic!(
                        "{what}: {module}::{} takes {kind:?} but does not cast it",
                        cast.name
                    )
                });
                assert_eq!(
                    &inner, node,
                    "{what}: {module}::{} wraps another node",
                    cast.name
                );
                assert_eq!(
                    &lowered, node,
                    "{what}: {module}::{} lowers to another node",
                    cast.name
                );
                if exempt(kind) {
                    continue;
                }
                let union = (cast.union)(node).unwrap_or_else(|| {
                    panic!(
                        "{what}: {module}::{} casts {kind:?}, the union's does not",
                        cast.name
                    )
                });
                assert_eq!(
                    printed, union,
                    "{what}: {module}::{} reads {kind:?} apart from the union",
                    cast.name
                );
            }
            // Exactness: away from holes, a facade type takes what the
            // union's takes.
            if !exempt(kind) && index.in_union(kind) && !index.casts(facade, kind).is_empty() {
                for cast in facade.casts {
                    assert_eq!(
                        (cast.can_cast)(kind),
                        (cast.union_can_cast)(kind),
                        "{what}: {module}::{} and the union's disagree on {kind:?}",
                        cast.name
                    );
                }
            }
            // Accessors, where nothing beneath the owner is a hole.
            if above_hole.contains(node) {
                continue;
            }
            for &i in index.accessors(facade, kind).to_vec().iter() {
                let accessor = &facade.accessors[i];
                if !compared(facade, accessor) {
                    continue;
                }
                index.ran.insert(i);
                let (got, want) = (accessor.call)(node).unwrap_or_else(|| {
                    panic!(
                        "{what}: {module}::{}::{} did not run on {kind:?}",
                        accessor.owner, accessor.name
                    )
                });
                assert_eq!(
                    got, want,
                    "{what}: {module}::{}::{} on {kind:?} differs from the union's",
                    accessor.owner, accessor.name
                );
            }
        }
    }
    holes_met
}

fn new_indexes() -> Vec<Index> {
    FACADES.iter().map(|_| Index::default()).collect()
}

/// Sources holding every construct a live `Kind` row of the feature table
/// introduced, beside ordinary code.
const FIXTURES: &[(&str, FileKind)] = &[
    (
        "let f () = async { while! cond () do () }\n",
        FileKind::Impl,
    ),
    ("let f (x: 'T & #System.IDisposable) = x\n", FileKind::Impl),
    ("let x : string | null = failwith \"\"\n", FileKind::Impl),
    ("let x : (string | null) list = []\n", FileKind::Impl),
    (
        "type T = { A : int }\nlet g (t: T) = match t with { A = 1 } -> $\"{t.A}\" | _ -> \"\"\n",
        FileKind::Impl,
    ),
    (
        "module M\nlet inline f< ^T when ^T : (static member M : unit -> int)> () = 'T.M()\n",
        FileKind::Impl,
    ),
    (
        "let xs = [ for x in 1 .. 10 -> x * x ]\nlet y = xs |> List.map _.ToString()\n",
        FileKind::Impl,
    ),
    (
        "namespace N\ntype C =\n  new : unit -> C\n  member M : int -> string | null\n",
        FileKind::Sig,
    ),
    (
        "let f (x: {| A : int; B : string |}) = x\nlet h (cons: int list) = cons.( :: ).1\n",
        FileKind::Impl,
    ),
    (
        "[<DllImport(\"x.dll\")>]\nextern void* f(char** a, int b)\nextern int A([<In>] int a)\n",
        FileKind::Impl,
    ),
    (
        "let inline f x =\n    g x\n    when 'T : int and 'U : float = h x\n    when 'T : float = b x\n",
        FileKind::Impl,
    ),
    (
        "[<Measure>] type m\nlet f x = match x with 1<m> -> 0 | _ -> 1\n",
        FileKind::Impl,
    ),
];

/// Checks every fixture at every version into `indexes`; returns the holes met.
fn run_fixtures(indexes: &mut [Index]) -> usize {
    let mut holes = 0;
    for (src, kind) in FIXTURES {
        for lang in all_versions() {
            let parse = parse_at(src, *kind, lang);
            holes += check_parse(&parse, &format!("{src:?} at {lang}"), indexes, false);
        }
    }
    holes
}

#[test]
fn fixtures_hold_at_every_version() {
    assert!(
        run_fixtures(&mut new_indexes()) > 0,
        "the fixtures exercise no hole"
    );
}

/// Every version's facade module serves exactly the versions its lock says,
/// and `Parse::view` reads a parse through it exactly at those.
#[test]
fn view_checks_the_version() {
    for view in VIEWS {
        assert!(view.versions.contains(&view.version), "{}", view.module);
        let head = VIEWS
            .iter()
            .find(|v| v.module == view.head)
            .expect("an alias's head is a view");
        assert_eq!(
            view.versions, head.versions,
            "{} vs {}",
            view.module, view.head
        );
        for lang in all_versions() {
            let parse = parse_at("let x = 1\n", FileKind::Impl, lang);
            assert_eq!(
                (view.view)(&parse),
                view.versions.contains(&lang),
                "{} viewing a parse at {lang}",
                view.module
            );
        }
    }
    // Each version is served by exactly one distinct facade, and preview by its
    // own.
    for lang in all_versions() {
        let serving: Vec<&str> = FACADES
            .iter()
            .filter(|f| f.versions.contains(&lang))
            .map(|f| f.module)
            .collect();
        assert_eq!(serving.len(), 1, "{lang}: {serving:?}");
    }
    let preview = FACADES.iter().find(|f| f.module == "preview").unwrap();
    assert_eq!(preview.versions, [LanguageVersion::Preview]);
}

#[test]
fn a_view_error_names_both_versions() {
    let parse = parse_at("let x = 1\n", FileKind::Impl, LanguageVersion::V9_0);
    let err = parse.view::<syntax::v8::ParsedInput>().unwrap_err();
    assert_eq!(err.parsed, LanguageVersion::V9_0);
    assert_eq!(err.facade, [LanguageVersion::V8_0]);
    assert!(parse.view::<syntax::v10::ParsedInput>().is_ok());
}

/// A node outside a facade's surface reads as an absent child: `None` from an
/// `Option` accessor, skipped by an iterator one.
#[test]
fn a_hole_reads_as_an_absent_child() {
    let parse = parse_at(
        "let x : (string | null) * int = failwith \"\"\n",
        FileKind::Impl,
        LanguageVersion::V8_0,
    );
    let syntax::v8::ParsedInput::ImplFile(file) = parse.view().unwrap() else {
        panic!("an implementation file")
    };
    let paren = file
        .syntax()
        .descendants()
        .find_map(syntax::v8::ParenType::cast)
        .expect("a paren type");
    assert_eq!(paren.inner(), None);
    assert!(paren.to_union().inner().is_some());
    let parse = parse_at(
        "let x : int * string | null = failwith \"\"\n",
        FileKind::Impl,
        LanguageVersion::V8_0,
    );
    let tuple = parse
        .root
        .descendants()
        .find_map(syntax::v8::TupleType::cast)
        .expect("a tuple type");
    // `int`, `*`, and the nullness type the facade cannot hold.
    assert_eq!(tuple.to_union().segments().len(), 3);
    assert_eq!(tuple.segments().len(), 2);
    // At 9.0 the same tree reads totally.
    let parse = parse_at(
        "let x : (string | null) * int = failwith \"\"\n",
        FileKind::Impl,
        LanguageVersion::V9_0,
    );
    let syntax::v9::ParsedInput::ImplFile(file) = parse.view().unwrap() else {
        panic!("an implementation file")
    };
    let paren = file
        .syntax()
        .descendants()
        .find_map(syntax::v9::ParenType::cast)
        .expect("a paren type");
    assert!(matches!(paren.inner(), Some(syntax::v9::Type::WithNull(_))));
}

/// An accepted divergence holds end to end. A frozen `v8` whose `Expr` lacks
/// the `DotLambda` and `WhileBang` variants the union has (astgen accepting
/// both) is simulated with `v5`'s `Expr` in place of `v8`'s: the exemptions
/// astgen derives from the acceptances let the properties pass on trees
/// holding both constructs, and without them the properties fail.
#[test]
fn an_accepted_divergence_is_exempt_from_the_properties() {
    use borzoi_astgen::{facade, features, lock};

    let rows = features::feature_table();
    let union = borzoi_astgen::union_lock();
    let versions = facade::versions();
    let v8 = *versions.iter().find(|v| v.module() == "v8").unwrap();
    let mut frozen = lock::parse(&facade::checked_in_lock(&v8).unwrap()).unwrap();
    let dropped = [
        "enum\tExpr\tDotLambda\tDotLambdaExpr",
        "enum\tExpr\tWhileBang\tWhileBangExpr",
    ];
    frozen
        .lines
        .retain(|l| !dropped.contains(&l.render().as_str()));
    let text = lock::render_published("v8", &frozen, facade::FREEZE_EPOCH);
    let accepted = dropped.map(|line| facade::Accepted {
        module: "v8",
        line,
        reason: "test",
    });
    let generation = facade::facade_locks(&facade::Inputs {
        rows: &rows,
        union: &union,
        versions: &versions,
        freeze: facade::Freeze {
            published: facade::PUBLISHED,
            epoch: facade::FREEZE_EPOCH,
            accepted: &accepted,
        },
        on_disk: &|v| {
            if v.module() == "v8" {
                Some(text.clone())
            } else {
                facade::checked_in_lock(v)
            }
        },
    })
    .unwrap_or_else(|e| panic!("{e}"));
    let exempt_names = facade::exemptions(&generation.locks, &union, &generation.accepted)["v8"]
        .kinds
        .clone();
    assert_eq!(
        exempt_names,
        ["DOT_LAMBDA_EXPR", "WHILE_BANG_EXPR"]
            .map(String::from)
            .into()
    );

    let src = "let f () = async { while! cond () do () }\nlet y = xs |> List.map _.ToString()\n";
    let parse = parse_at(src, FileKind::Impl, LanguageVersion::V8_0);
    let mut exempt_kinds: Vec<SyntaxKind> = parse
        .root
        .descendants()
        .map(|n| n.kind())
        .filter(|k| exempt_names.contains(&format!("{k:?}")))
        .collect();
    exempt_kinds.sort();
    exempt_kinds.dedup();
    assert_eq!(exempt_kinds.len(), 2, "the tree holds both constructs");

    let real = FACADES.iter().find(|f| f.module == "v8").unwrap();
    let mut casts: Vec<Cast> = real
        .casts
        .iter()
        .filter(|c| c.name != "Expr")
        .map(|c| Cast {
            name: c.name,
            can_cast: c.can_cast,
            union_can_cast: c.union_can_cast,
            facade: c.facade,
            union: c.union,
        })
        .collect();
    casts.push(cast_case!(v5, Expr));
    let casts: &'static [Cast] = Box::leak(casts.into_boxed_slice());
    let facade_with = |exempt_kinds: &'static [SyntaxKind]| FacadeCase {
        module: "v8 (frozen without two Expr variants)",
        versions: real.versions,
        casts,
        accessors: real.accessors,
        exempt_kinds,
        exempt_values: &[],
    };
    let exempted = [facade_with(Box::leak(exempt_kinds.into_boxed_slice()))];
    let mut index = [Index::default()];
    check_parse_with(&exempted, &parse, src, &mut index, true);
    // The exempt nodes' own accessors still run, so an exemption leaves the
    // corpus sweep's coverage satisfiable.
    for owner in ["WhileBangExpr", "DotLambdaExpr"] {
        let ran = exempted[0]
            .accessors
            .iter()
            .enumerate()
            .filter(|(_, a)| a.owner == owner)
            .all(|(i, _)| index[0].ran.contains(&i));
        assert!(ran, "{owner}'s accessors did not all run");
    }
    let strict = [facade_with(&[])];
    let failed = {
        let _silence = borzoi_oracle_harness::panic_silence::silence_panics_here();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check_parse_with(&strict, &parse, src, &mut [Index::default()], true)
        }))
        .is_err()
    };
    assert!(failed, "without the exemptions the narrowed `Expr` fails");
}

/// Snippets the generated programs are made of: each live `Kind` row's
/// construct, and ordinary declarations around them.
const SNIPPETS: &[&str] = &[
    "let a{i} = 1 + 2\n",
    "let b{i} : string | null = null\n",
    "let c{i} (x: 'T & #System.IDisposable) = x\n",
    "let d{i} () = async { while! f () do () }\n",
    "let e{i} : (int -> string | null) * int list = failwith \"\"\n",
    "type T{i} = { F : int option }\n",
    "let g{i} x = match x with Some (y: int) -> y | None -> 0\n",
    "let h{i} = $\"{1} and {2}\"\n",
    "module M{i} =\n    let x = [| 1; 2 |].[0]\n",
];

fn program(specs: &[usize]) -> String {
    specs
        .iter()
        .enumerate()
        .map(|(i, s)| SNIPPETS[s % SNIPPETS.len()].replace("{i}", &i.to_string()))
        .collect()
}

proptest! {
    #[test]
    fn generated_programs_hold(
        specs in prop::collection::vec(0usize..SNIPPETS.len(), 0..8),
        lang in prop::sample::select(all_versions()),
    ) {
        let src = program(&specs);
        let parse = parse_at(&src, FileKind::Impl, lang);
        check_parse(&parse, &format!("{src:?} at {lang}"), &mut new_indexes(), false);
    }
}

/// Every `step`th corpus file, parsed at a version each distinct facade
/// serves and read through that facade, into `indexes`.
fn corpus_holds(step: usize, indexes: &mut [Index]) {
    let root = crate::common::corpus_root();
    let files = crate::common::collect_fsharp_corpus_files(&root).expect("walk the corpus");
    let mut checked = 0;
    for path in files.iter().step_by(step) {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let kind = if path.extension().is_some_and(|e| e == "fsi") {
            FileKind::Sig
        } else {
            FileKind::Impl
        };
        let what = Path::new(path).display().to_string();
        for facade in FACADES {
            let lang = facade.versions[0];
            let parse = parse_at(&src, kind, lang);
            check_parse(&parse, &format!("{what} at {lang}"), indexes, true);
        }
        checked += 1;
    }
    assert!(checked > 0, "no corpus file was read");
}

/// The accessors no tree read through each facade, as `module::Owner::name`.
fn unexercised(indexes: &[Index]) -> Vec<String> {
    let mut out = Vec::new();
    for (facade, index) in FACADES.iter().zip(indexes) {
        for (i, accessor) in facade.accessors.iter().enumerate() {
            if compared(facade, accessor) && !index.ran.contains(&i) {
                out.push(format!(
                    "{}::{}::{}",
                    facade.module, accessor.owner, accessor.name
                ));
            }
        }
    }
    out
}

/// A sample of the corpus with the fixtures, in the normal suite. Each facade
/// re-casts by return shape, so between them they must run, in every facade,
/// an accessor of every shape that facade has.
#[test]
fn corpus_sample_holds() {
    let mut indexes = new_indexes();
    run_fixtures(&mut indexes);
    corpus_holds(40, &mut indexes);
    let mut missing = Vec::new();
    for (facade, index) in FACADES.iter().zip(&indexes) {
        let shapes: std::collections::BTreeSet<&str> = facade
            .accessors
            .iter()
            .filter(|a| compared(facade, a))
            .map(|a| a.shape)
            .collect();
        for shape in shapes {
            let ran = facade
                .accessors
                .iter()
                .enumerate()
                .any(|(i, a)| a.shape == shape && index.ran.contains(&i));
            if !ran {
                missing.push(format!("{}: {shape}", facade.module));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "no accessor of these shapes ran: {missing:#?}"
    );
}

/// The whole corpus, with the fixtures. Ignored: it parses every file once per
/// distinct facade. Between them they must run every accessor of every facade
/// on an owner with no hole beneath it; an accessor no corpus file reaches
/// needs a fixture in [`FIXTURES`].
#[test]
#[ignore]
fn corpus() {
    let mut indexes = new_indexes();
    run_fixtures(&mut indexes);
    corpus_holds(1, &mut indexes);
    let missed = unexercised(&indexes);
    assert!(
        missed.is_empty(),
        "no fixture or corpus file runs these accessors; add a fixture that does: {missed:#?}"
    );
}
