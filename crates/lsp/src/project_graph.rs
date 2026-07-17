//! Pure inter-project dependency-graph builder (consumer #3, stage 3.0 of
//! `docs/completed/fsproj-project-graph-plan.md`).
//!
//! Given an entry F# project and an **injected resolver** that maps an F#
//! project path to its `<ProjectReference>` edges, [`build_graph`] produces the
//! transitive project→project graph: the nodes in the dependency closure
//! (classified F# / C# / other), plus the problems found while walking
//! (missing targets, reference cycles, unsupported project kinds). It does no
//! IO — the resolver is the only door to the filesystem (dependency
//! rejection), so the traversal, cycle-breaking, and ordering are
//! property-testable in isolation. The filesystem-backed resolver
//! (`Workspace`-evaluated `project_references`) is wired in stage 3.1.
//!
//! Two structural rules (`docs/completed/fsproj-project-graph-plan.md` E3, E5):
//! - **Recurse only through F# nodes.** A `.csproj` is a *terminal boundary* —
//!   the C# sidecar owns that subtree (it builds its own `<ProjectReference>`
//!   closure), so we record the edge and stop rather than re-deriving C#
//!   project semantics in Rust.
//! - **Report, don't abort.** A cycle or missing/unsupported target becomes a
//!   [`GraphProblem`] (carrying the offending edge's XML span) and the walk
//!   continues, so the rest of the graph still surfaces.
//!
//! Node identity is the [`lexically_normalize`]d path. Like consumer #1's
//! membership check this folds `.`/`..`/separator spelling but not symlinks;
//! unlike it, it is *not* case-folded — two references to one project written
//! with different casing would be seen as two nodes on a case-insensitive
//! filesystem. Project-reference paths are authored consistently in practice,
//! so this is a documented residual rather than handled here.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::paths::lexically_normalize;

/// How a referenced project file is classified, by extension (case-insensitive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    /// `.fsproj` — recursed into (its own `project_references` are followed).
    FSharp,
    /// `.csproj` — a terminal boundary; the C# sidecar owns its subtree.
    CSharp,
    /// Anything else (`.vbproj`, …) — unsupported as a reference target.
    Other,
}

/// Classify a project path by its extension.
pub fn classify(path: &Path) -> ProjectKind {
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("fsproj") => ProjectKind::FSharp,
        Some(e) if e.eq_ignore_ascii_case("csproj") => ProjectKind::CSharp,
        _ => ProjectKind::Other,
    }
}

/// What following an [`Edge`] contributes to the graph.
///
/// MSBuild ground truth (dotnet 10 probes, 2026-07): a `<ProjectReference>`
/// with `ExcludeAssets` covering `compile` still puts the referenced
/// project's **own** output in the consumer's `ReferencePath` — the exclusion
/// filters what flows *through* the reference, not the direct output. The
/// resolver models that as an [`EdgeKind::OutputOnly`] edge; the plain case
/// is [`EdgeKind::Full`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// The target node and everything reachable through it: the walk
    /// recurses into the target's own references.
    Full,
    /// The target node only: it is recorded (existence-checked, classified,
    /// TFM-resolved) but its references are **not** followed. Processed
    /// after the full walk, so a target that is *also* reachable through
    /// [`EdgeKind::Full`] edges still gets its subtree (any transparent
    /// path wins — the probes' diamond case), regardless of edge order.
    OutputOnly,
}

/// An edge to a referenced project: the (already lexically-normalised) target
/// path, the byte span of the originating `<ProjectReference>` element in
/// the referrer's XML (for anchoring diagnostics), and what the edge
/// contributes ([`EdgeKind`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub target: PathBuf,
    pub span: Range<usize>,
    pub kind: EdgeKind,
}

/// How the resolver pinned (or failed to pin) the target framework a node's
/// edges were read under. The builder copies it verbatim onto the node; the
/// assembly-env fold uses it to locate the node's output DLL — a
/// [`NodeTfm::Unresolved`] node's outputs cannot be trusted (any single
/// on-disk variant may be a stale build of a TFM the real build wouldn't
/// select), so the fold skips them (under-resolve, never wrong).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeTfm {
    /// Evaluated under this specific TFM: a caller-owned global, the
    /// project's own body-written singular `TargetFramework`, a producer
    /// seed validated against the current declarations, or the sole
    /// declared TFM.
    Known(String),
    /// The project declares no TFM anywhere — there is only one build it
    /// can produce, so any located output is that build's.
    NoneDeclared,
    /// Multi-targeted with no way to select (no seed, or a stale one): the
    /// TFM the real build would use is unknown.
    Unresolved,
    /// The resolver never evaluated the project (C# terminal nodes, or an
    /// evaluation failure).
    NotEvaluated,
}

/// The injected resolver's answer for an F# project path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeResult {
    /// The project parsed; its `<ProjectReference>` edges in document order,
    /// plus the TFM state they were read under and the evaluation's
    /// output-file base name. (A project that exists but couldn't be
    /// evaluated resolves to an empty edge list — it's a node in the closure,
    /// we just can't see past it.)
    Resolved {
        edges: Vec<Edge>,
        tfm: NodeTfm,
        /// See [`ProjectNode::output_name`]; from the same evaluation the
        /// TFM verdict came from.
        output_name: Option<String>,
    },
    /// The project file does not exist on disk.
    NotFound,
}

impl NodeResult {
    /// A [`NodeResult::Resolved`] with no TFM or output-name information —
    /// the shape every resolver produced before TFM tracking; used by tests
    /// and the non-F#-terminal short-circuit.
    pub fn resolved(edges: Vec<Edge>) -> NodeResult {
        NodeResult::Resolved {
            edges,
            tfm: NodeTfm::NotEvaluated,
            output_name: None,
        }
    }
}

/// A discovered project in the dependency closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectNode {
    /// Lexically-normalised project path.
    pub path: PathBuf,
    pub kind: ProjectKind,
    /// The project's `<ProjectReference>` edges in document order. Populated
    /// for F# nodes; empty for C# terminal nodes (the sidecar owns those).
    pub references: Vec<Edge>,
    /// The TFM state the resolver read this node under (see [`NodeTfm`]).
    pub tfm: NodeTfm,
    /// The node's effective output-file base name, from the same evaluation
    /// as [`Self::tfm`]: the trusted evaluated `$(TargetName)` (which
    /// defaults to `$(AssemblyName)` — MSBuild writes
    /// `$(TargetName)$(TargetExt)`), else the project-file stem (MSBuild's
    /// default). `None` when unknowable — a C# terminal node, an evaluation
    /// failure, a name whose provenance the evaluator can't trust
    /// ([`borzoi_msbuild::ParsedProject::target_name`]), or a
    /// TFM-unresolved node (whose per-TFM name can't be pinned either). The
    /// env fold must decline to locate a `None`-named node's output rather
    /// than guess by stem: a producer renamed by `<TargetName>` or
    /// `<AssemblyName>` may leave a stale stem-named DLL on disk, and
    /// folding it would fabricate.
    pub output_name: Option<String>,
}

/// A problem found while building the graph. Each carries the offending
/// `<ProjectReference>`'s `target`, the `referrer` project whose XML contains
/// that element, and the `span` of the element **within `referrer`'s text**.
/// The diagnostics layer (stage 3.2) anchors the squiggle at `span` *in
/// `referrer`* — transitive problems live in a referrer other than the entry,
/// so the referrer is what disambiguates a (target, span) pair that could
/// otherwise occur in more than one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphProblem {
    /// A `<ProjectReference>` target file does not exist on disk.
    NotFound {
        referrer: PathBuf,
        target: PathBuf,
        span: Range<usize>,
    },
    /// Following this edge would close a reference cycle (MSBuild rejects
    /// these); `span` is the back-edge that closes it.
    Cycle {
        referrer: PathBuf,
        target: PathBuf,
        span: Range<usize>,
    },
    /// A `<ProjectReference>` to a project kind we don't model (not
    /// `.fsproj`/`.csproj`).
    UnsupportedKind {
        referrer: PathBuf,
        target: PathBuf,
        span: Range<usize>,
    },
}

/// The result of [`build_graph`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGraph {
    /// Closure nodes in deterministic discovery order (entry first, then
    /// depth-first in document order; each node once).
    pub nodes: Vec<ProjectNode>,
    /// Problems found while walking.
    pub problems: Vec<GraphProblem>,
}

/// Build the transitive dependency graph for `entry` (an F# project), calling
/// `resolve` to obtain each F# project's edges. Pure: the only IO is whatever
/// `resolve` does. Terminates on any input — cycles are broken by a visited
/// set. See the module docs for the structural rules.
pub fn build_graph(entry: &Path, mut resolve: impl FnMut(&Path) -> NodeResult) -> ProjectGraph {
    let mut builder = Builder {
        nodes: Vec::new(),
        problems: Vec::new(),
        visited: HashSet::new(),
        on_stack: HashSet::new(),
        deferred: Vec::new(),
    };
    builder.walk_fsharp(entry, None, &mut resolve);
    // OutputOnly edges are processed only after the full walk, so that a
    // target that is also transparently reachable keeps its subtree no
    // matter which edge the walk encountered first (see [`EdgeKind`]).
    let deferred = std::mem::take(&mut builder.deferred);
    for (referrer, edge) in deferred {
        builder.record_output_only(&referrer, edge, &mut resolve);
    }
    ProjectGraph {
        nodes: builder.nodes,
        problems: builder.problems,
    }
}

struct Builder {
    nodes: Vec<ProjectNode>,
    problems: Vec<GraphProblem>,
    /// Node keys ever entered (for dedup across diamonds).
    visited: HashSet<PathBuf>,
    /// Node keys currently on the recursion path (for cycle detection): an
    /// edge back to one of these is a back-edge, i.e. a cycle.
    on_stack: HashSet<PathBuf>,
    /// [`EdgeKind::OutputOnly`] edges (with their referrer), collected in
    /// encounter order during the walk and processed after it completes.
    deferred: Vec<(PathBuf, Edge)>,
}

impl Builder {
    /// Visit an F# project. `via` is the `(referrer, span)` of the
    /// `<ProjectReference>` that led here — `referrer` is the project whose
    /// text `span` indexes — or `None` for the entry project.
    fn walk_fsharp(
        &mut self,
        path: &Path,
        via: Option<(PathBuf, Range<usize>)>,
        resolve: &mut impl FnMut(&Path) -> NodeResult,
    ) {
        match resolve(path) {
            NodeResult::NotFound => {
                // A missing project is not a node: do *not* mark it visited or
                // push it onto the stack. Marking it visited would make a
                // second edge to the same missing target take the dedup path
                // and skip its own NotFound — but each broken
                // `<ProjectReference>` must be reported against its own
                // referrer/span. A missing *entry* (via `None`) is surfaced by
                // the buffer's own parse path, not the graph.
                if let Some((referrer, span)) = via {
                    self.problems.push(GraphProblem::NotFound {
                        referrer,
                        target: lexically_normalize(path),
                        span,
                    });
                }
            }
            NodeResult::Resolved {
                edges: references,
                tfm,
                output_name,
            } => {
                let key = lexically_normalize(path);
                self.visited.insert(key.clone());
                self.on_stack.insert(key.clone());
                self.nodes.push(ProjectNode {
                    path: key.clone(),
                    kind: ProjectKind::FSharp,
                    references: references.clone(),
                    tfm,
                    output_name,
                });
                for edge in references {
                    match edge.kind {
                        EdgeKind::Full => self.follow_edge(&key, edge, resolve),
                        EdgeKind::OutputOnly => self.deferred.push((key.clone(), edge)),
                    }
                }
                self.on_stack.remove(&key);
            }
        }
    }

    /// Dispatch one edge (owned by `referrer`) by the target's project kind.
    fn follow_edge(
        &mut self,
        referrer: &Path,
        edge: Edge,
        resolve: &mut impl FnMut(&Path) -> NodeResult,
    ) {
        let key = lexically_normalize(&edge.target);
        match classify(&edge.target) {
            ProjectKind::Other => self.problems.push(GraphProblem::UnsupportedKind {
                referrer: referrer.to_path_buf(),
                target: key,
                span: edge.span,
            }),
            ProjectKind::CSharp => self.record_csharp_boundary(referrer, key, edge, resolve),
            ProjectKind::FSharp => {
                if self.on_stack.contains(&key) {
                    self.problems.push(GraphProblem::Cycle {
                        referrer: referrer.to_path_buf(),
                        target: key,
                        span: edge.span,
                    });
                } else if !self.visited.contains(&key) {
                    self.walk_fsharp(
                        &edge.target,
                        Some((referrer.to_path_buf(), edge.span)),
                        resolve,
                    );
                }
                // visited but not on-stack → diamond: dedup, nothing to do.
            }
        }
    }

    /// A `.csproj` target is a terminal boundary: we *verify existence* (a
    /// broken reference is a problem like any other) but never recurse — the
    /// sidecar owns the C# subtree, so the resolver's edges are ignored. A
    /// missing csproj is not marked visited, so each referencing edge reports
    /// its own NotFound (mirroring the F# path).
    fn record_csharp_boundary(
        &mut self,
        referrer: &Path,
        key: PathBuf,
        edge: Edge,
        resolve: &mut impl FnMut(&Path) -> NodeResult,
    ) {
        match resolve(&edge.target) {
            NodeResult::NotFound => self.problems.push(GraphProblem::NotFound {
                referrer: referrer.to_path_buf(),
                target: key,
                span: edge.span,
            }),
            NodeResult::Resolved { .. } => {
                if self.visited.insert(key.clone()) {
                    self.nodes.push(ProjectNode {
                        path: key,
                        kind: ProjectKind::CSharp,
                        references: Vec::new(),
                        tfm: NodeTfm::NotEvaluated,
                        output_name: None,
                    });
                }
            }
        }
    }

    /// Process one deferred [`EdgeKind::OutputOnly`] edge, after the main
    /// walk: the target joins the graph as a node — its own output is
    /// referenced — but its references are **not** followed. A target the
    /// transparent walk already visited needs nothing (its subtree flows via
    /// that path); a missing target reports NotFound against its own
    /// referrer/span, unmarked as visited, exactly like a Full edge's.
    fn record_output_only(
        &mut self,
        referrer: &Path,
        edge: Edge,
        resolve: &mut impl FnMut(&Path) -> NodeResult,
    ) {
        let key = lexically_normalize(&edge.target);
        match classify(&edge.target) {
            ProjectKind::Other => self.problems.push(GraphProblem::UnsupportedKind {
                referrer: referrer.to_path_buf(),
                target: key,
                span: edge.span,
            }),
            // For a terminal boundary "record without recursing" is what a
            // Full edge already does.
            ProjectKind::CSharp => self.record_csharp_boundary(referrer, key, edge, resolve),
            ProjectKind::FSharp => {
                if self.visited.contains(&key) {
                    return;
                }
                match resolve(&edge.target) {
                    NodeResult::NotFound => self.problems.push(GraphProblem::NotFound {
                        referrer: referrer.to_path_buf(),
                        target: key,
                        span: edge.span,
                    }),
                    NodeResult::Resolved {
                        edges: references,
                        tfm,
                        output_name,
                    } => {
                        self.visited.insert(key.clone());
                        self.nodes.push(ProjectNode {
                            path: key,
                            kind: ProjectKind::FSharp,
                            references,
                            tfm,
                            output_name,
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    fn fsproj(i: usize) -> PathBuf {
        PathBuf::from(format!("/p/P{i}.fsproj"))
    }

    fn edge(target: PathBuf) -> Edge {
        Edge {
            target,
            span: 0..0,
            kind: EdgeKind::Full,
        }
    }

    fn output_only(target: PathBuf) -> Edge {
        Edge {
            target,
            span: 0..0,
            kind: EdgeKind::OutputOnly,
        }
    }

    /// Index back out of a `/p/P{i}.fsproj` path produced by [`fsproj`].
    fn index_of(path: &Path) -> Option<usize> {
        path.file_stem()?.to_str()?.strip_prefix('P')?.parse().ok()
    }

    /// A resolver backed by an adjacency list of F# projects (index → indices).
    fn adjacency_resolver(adj: &[Vec<usize>]) -> impl FnMut(&Path) -> NodeResult + '_ {
        move |path: &Path| match index_of(path) {
            Some(i) if i < adj.len() => {
                NodeResult::resolved(adj[i].iter().map(|&j| edge(fsproj(j))).collect())
            }
            _ => NodeResult::NotFound,
        }
    }

    /// Reference reachability oracle: the set of indices reachable from 0.
    fn reachable_from_zero(adj: &[Vec<usize>]) -> BTreeSet<usize> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            if seen.insert(i) {
                for &j in &adj[i] {
                    stack.push(j);
                }
            }
        }
        seen
    }

    fn proj_strategy() -> impl Strategy<Value = Vec<Vec<usize>>> {
        // 1–5 projects; each references 0–4 raw indices (clamped mod n below).
        prop::collection::vec(prop::collection::vec(0usize..8, 0..5), 1..6)
    }

    proptest! {
        /// The node set equals the F# closure reachable from the entry — no
        /// more (no spurious nodes), no fewer (full closure), each once. Holds
        /// even when the adjacency encodes cycles (the walk still terminates).
        #[test]
        fn node_set_equals_reachable_closure(raw in proj_strategy()) {
            let n = raw.len();
            let adj: Vec<Vec<usize>> =
                raw.iter().map(|es| es.iter().map(|&e| e % n).collect()).collect();

            let graph = build_graph(&fsproj(0), adjacency_resolver(&adj));

            let got: BTreeSet<PathBuf> = graph.nodes.iter().map(|nd| nd.path.clone()).collect();
            let want: BTreeSet<PathBuf> =
                reachable_from_zero(&adj).iter().map(|&i| fsproj(i)).collect();
            prop_assert_eq!(got, want);
            // Each node appears exactly once.
            prop_assert_eq!(graph.nodes.len(), reachable_from_zero(&adj).len());
        }

        /// `build_graph` is a pure function of `(entry, resolver)`: same inputs,
        /// identical output (node order, edges, problems).
        #[test]
        fn build_graph_is_deterministic(raw in proj_strategy()) {
            let n = raw.len();
            let adj: Vec<Vec<usize>> =
                raw.iter().map(|es| es.iter().map(|&e| e % n).collect()).collect();
            let first = build_graph(&fsproj(0), adjacency_resolver(&adj));
            let second = build_graph(&fsproj(0), adjacency_resolver(&adj));
            prop_assert_eq!(first, second);
        }

        /// With [`EdgeKind::OutputOnly`] edges in play, the node set is the
        /// **Full-edge closure** plus the **OutputOnly fringe**: targets of
        /// OutputOnly edges out of fully-walked nodes join the graph but are
        /// never traversed, and a fringe node that is *also* Full-reachable
        /// keeps its subtree (any transparent path wins), regardless of the
        /// order edges are declared in.
        #[test]
        fn node_set_is_full_closure_plus_output_only_fringe(raw in kinded_strategy()) {
            let n = raw.len();
            let adj: Vec<Vec<(usize, EdgeKind)>> = raw
                .iter()
                .map(|es| es.iter().map(|&(e, oo)| {
                    (e % n, if oo { EdgeKind::OutputOnly } else { EdgeKind::Full })
                }).collect())
                .collect();

            let graph = build_graph(&fsproj(0), kinded_resolver(&adj));

            // Oracle: reachability over Full edges only…
            let mut full = BTreeSet::new();
            let mut stack = vec![0usize];
            while let Some(i) = stack.pop() {
                if full.insert(i) {
                    for &(j, kind) in &adj[i] {
                        if kind == EdgeKind::Full {
                            stack.push(j);
                        }
                    }
                }
            }
            // …plus the OutputOnly targets of those walked nodes.
            let want: BTreeSet<PathBuf> = full
                .iter()
                .flat_map(|&i| adj[i].iter().filter(|&&(_, k)| k == EdgeKind::OutputOnly).map(|&(j, _)| j))
                .chain(full.iter().copied())
                .map(fsproj)
                .collect();

            let got: BTreeSet<PathBuf> = graph.nodes.iter().map(|nd| nd.path.clone()).collect();
            prop_assert_eq!(&got, &want);
            // Each node appears exactly once.
            prop_assert_eq!(graph.nodes.len(), want.len());
        }
    }

    /// Adjacency with per-edge kinds: `(raw index, is_output_only)`.
    fn kinded_strategy() -> impl Strategy<Value = Vec<Vec<(usize, bool)>>> {
        prop::collection::vec(
            prop::collection::vec((0usize..8, prop::bool::ANY), 0..5),
            1..6,
        )
    }

    fn kinded_resolver(adj: &[Vec<(usize, EdgeKind)>]) -> impl FnMut(&Path) -> NodeResult + '_ {
        move |path: &Path| match index_of(path) {
            Some(i) if i < adj.len() => NodeResult::resolved(
                adj[i]
                    .iter()
                    .map(|&(j, kind)| Edge {
                        target: fsproj(j),
                        span: 0..0,
                        kind,
                    })
                    .collect(),
            ),
            _ => NodeResult::NotFound,
        }
    }

    #[test]
    fn output_only_edge_records_target_without_recursing() {
        // P0 -[OutputOnly]→ P1; P1 → P2. P1 contributes its own output (it is
        // a node), but nothing flows through the edge, so P2 must not appear.
        let mut resolve = |path: &Path| match index_of(path) {
            Some(0) => NodeResult::resolved(vec![output_only(fsproj(1))]),
            Some(1) => NodeResult::resolved(vec![edge(fsproj(2))]),
            Some(2) => NodeResult::resolved(vec![]),
            _ => NodeResult::NotFound,
        };
        let graph = build_graph(&fsproj(0), &mut resolve);
        assert!(graph.problems.is_empty(), "{:?}", graph.problems);
        let got: Vec<PathBuf> = graph.nodes.iter().map(|n| n.path.clone()).collect();
        assert_eq!(got, vec![fsproj(0), fsproj(1)]);
    }

    #[test]
    fn output_only_target_also_fully_reachable_keeps_its_subtree() {
        // P0 -[OutputOnly]→ P1 declared *first*, then P0 → P2 → P1 → P3.
        // The transparent path through P2 must win: P1's subtree (P3) is in
        // the graph even though the OutputOnly edge was encountered before
        // the Full path reached P1 (MSBuild diamond probe).
        let mut resolve = |path: &Path| match index_of(path) {
            Some(0) => NodeResult::resolved(vec![output_only(fsproj(1)), edge(fsproj(2))]),
            Some(1) => NodeResult::resolved(vec![edge(fsproj(3))]),
            Some(2) => NodeResult::resolved(vec![edge(fsproj(1))]),
            Some(3) => NodeResult::resolved(vec![]),
            _ => NodeResult::NotFound,
        };
        let graph = build_graph(&fsproj(0), &mut resolve);
        assert!(graph.problems.is_empty(), "{:?}", graph.problems);
        let got: BTreeSet<PathBuf> = graph.nodes.iter().map(|n| n.path.clone()).collect();
        let want: BTreeSet<PathBuf> = (0..=3).map(fsproj).collect();
        assert_eq!(got, want);
        assert_eq!(graph.nodes.len(), 4);
    }

    #[test]
    fn missing_output_only_target_reports_not_found() {
        let mut resolve = |path: &Path| {
            if index_of(path) == Some(0) {
                NodeResult::resolved(vec![Edge {
                    target: PathBuf::from("/p/Gone.fsproj"),
                    span: 7..9,
                    kind: EdgeKind::OutputOnly,
                }])
            } else {
                NodeResult::NotFound
            }
        };
        let graph = build_graph(&fsproj(0), &mut resolve);
        assert_eq!(
            graph.problems,
            vec![GraphProblem::NotFound {
                referrer: fsproj(0),
                target: PathBuf::from("/p/Gone.fsproj"),
                span: 7..9,
            }]
        );
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn output_only_csharp_target_is_recorded_as_terminal_boundary() {
        // Builder-level contract only: an OutputOnly edge to a `.csproj`
        // records the same terminal boundary node a Full edge would. (The
        // workspace resolver never emits OutputOnly edges to C# targets —
        // see `compile_edge_kind` — because the sidecar expands a boundary
        // node's whole subtree.)
        let mut resolve = |path: &Path| match classify(path) {
            ProjectKind::FSharp => {
                NodeResult::resolved(vec![output_only(PathBuf::from("/p/B.csproj"))])
            }
            _ => NodeResult::resolved(vec![]),
        };
        let graph = build_graph(&fsproj(0), &mut resolve);
        assert!(graph.problems.is_empty(), "{:?}", graph.problems);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.nodes[1].kind, ProjectKind::CSharp);
        assert!(graph.nodes[1].references.is_empty());
    }

    #[test]
    fn missing_target_is_one_not_found_problem() {
        // P0 references a project that resolves to NotFound.
        let mut resolve = |path: &Path| {
            if index_of(path) == Some(0) {
                NodeResult::resolved(vec![Edge {
                    target: PathBuf::from("/p/Gone.fsproj"),
                    span: 10..20,
                    kind: EdgeKind::Full,
                }])
            } else {
                NodeResult::NotFound
            }
        };
        let graph = build_graph(&fsproj(0), &mut resolve);
        assert_eq!(
            graph.problems,
            vec![GraphProblem::NotFound {
                referrer: fsproj(0),
                target: PathBuf::from("/p/Gone.fsproj"),
                span: 10..20,
            }]
        );
        // Only the entry is a node; the missing target is not.
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].path, fsproj(0));
    }

    #[test]
    fn transitive_problem_records_its_referrer_not_the_entry() {
        // P0 → P1 → Gone. The NotFound's span indexes into P1, so its referrer
        // must be P1 (not the entry P0) — this is what lets a consumer publish
        // the diagnostic to the right file.
        let mut resolve = |path: &Path| match index_of(path) {
            Some(0) => NodeResult::resolved(vec![edge(fsproj(1))]),
            Some(1) => NodeResult::resolved(vec![Edge {
                target: PathBuf::from("/p/Gone.fsproj"),
                span: 42..50,
                kind: EdgeKind::Full,
            }]),
            _ => NodeResult::NotFound,
        };
        let graph = build_graph(&fsproj(0), &mut resolve);
        assert_eq!(
            graph.problems,
            vec![GraphProblem::NotFound {
                referrer: fsproj(1),
                target: PathBuf::from("/p/Gone.fsproj"),
                span: 42..50,
            }]
        );
    }

    #[test]
    fn each_referrer_of_a_missing_target_is_reported() {
        // P0 → {P1, Gone}; P1 → Gone. The same missing target is referenced by
        // two distinct `<ProjectReference>` elements (in P0 and P1). Each must
        // produce its own NotFound, attributed to its own referrer/span — the
        // first NotFound must not mark Gone "visited" and suppress the second.
        let gone = PathBuf::from("/p/Gone.fsproj");
        let g = gone.clone();
        let mut resolve = move |path: &Path| match index_of(path) {
            Some(0) => NodeResult::resolved(vec![
                edge(fsproj(1)),
                Edge {
                    target: g.clone(),
                    span: 1..2,
                    kind: EdgeKind::Full,
                },
            ]),
            Some(1) => NodeResult::resolved(vec![Edge {
                target: g.clone(),
                span: 3..4,
                kind: EdgeKind::Full,
            }]),
            _ => NodeResult::NotFound,
        };
        let graph = build_graph(&fsproj(0), &mut resolve);
        let not_founds: Vec<_> = graph
            .problems
            .iter()
            .filter(|p| matches!(p, GraphProblem::NotFound { .. }))
            .collect();
        assert_eq!(not_founds.len(), 2, "{:?}", graph.problems);
        assert!(not_founds.contains(&&GraphProblem::NotFound {
            referrer: fsproj(0),
            target: gone.clone(),
            span: 1..2,
        }));
        assert!(not_founds.contains(&&GraphProblem::NotFound {
            referrer: fsproj(1),
            target: gone,
            span: 3..4,
        }));
    }

    #[test]
    fn cycle_is_reported_on_the_back_edge() {
        // P0 → P1 → P0.
        let adj = vec![vec![1], vec![0]];
        let graph = build_graph(&fsproj(0), adjacency_resolver(&adj));
        // Two nodes, exactly one cycle problem on the P1→P0 back-edge.
        let cycles: Vec<_> = graph
            .problems
            .iter()
            .filter(|p| matches!(p, GraphProblem::Cycle { .. }))
            .collect();
        assert_eq!(
            cycles,
            vec![&GraphProblem::Cycle {
                referrer: fsproj(1),
                target: fsproj(0),
                span: 0..0,
            }]
        );
        assert_eq!(graph.nodes.len(), 2);
    }

    #[test]
    fn missing_csharp_reference_is_reported() {
        // A `<ProjectReference>` to a non-existent .csproj is a broken
        // reference, reported like a missing .fsproj — even though we never
        // recurse into C#. Existence is checked for every node kind.
        let mut resolve = |path: &Path| match classify(path) {
            ProjectKind::FSharp if index_of(path) == Some(0) => NodeResult::resolved(vec![Edge {
                target: PathBuf::from("/p/Gone.csproj"),
                span: 5..11,
                kind: EdgeKind::Full,
            }]),
            // The referenced csproj does not exist.
            _ => NodeResult::NotFound,
        };
        let graph = build_graph(&fsproj(0), &mut resolve);
        assert_eq!(
            graph.problems,
            vec![GraphProblem::NotFound {
                referrer: fsproj(0),
                target: PathBuf::from("/p/Gone.csproj"),
                span: 5..11,
            }]
        );
        // The missing csproj is not a node.
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn csharp_reference_is_terminal_not_recursed() {
        // P0 → B.csproj (which exists). The resolver *would* give B.csproj an
        // edge to C, but csproj is a boundary: the builder checks B.csproj's
        // existence yet ignores its edges, so C must not appear.
        let mut resolve = |path: &Path| match path.extension().and_then(|e| e.to_str()) {
            Some("fsproj") => NodeResult::resolved(vec![edge(PathBuf::from("/p/B.csproj"))]),
            // The csproj exists; its (ignored) edge to C would otherwise pull
            // C into the graph if we recursed.
            Some("csproj") => NodeResult::resolved(vec![edge(PathBuf::from("/p/C.csproj"))]),
            _ => panic!("unexpected resolve for {path:?}"),
        };
        let graph = build_graph(&fsproj(0), &mut resolve);
        assert!(graph.problems.is_empty());
        // Only P0 and B.csproj — C is not pulled in (no recursion through C#).
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.nodes[1].kind, ProjectKind::CSharp);
        assert_eq!(graph.nodes[1].path, PathBuf::from("/p/B.csproj"));
        assert!(graph.nodes[1].references.is_empty());
        assert!(
            graph
                .nodes
                .iter()
                .all(|n| n.path != Path::new("/p/C.csproj"))
        );
    }

    #[test]
    fn unsupported_kind_is_reported() {
        let mut resolve = |path: &Path| {
            if classify(path) == ProjectKind::FSharp {
                NodeResult::resolved(vec![Edge {
                    target: PathBuf::from("/p/Legacy.vbproj"),
                    span: 3..9,
                    kind: EdgeKind::Full,
                }])
            } else {
                NodeResult::NotFound
            }
        };
        let graph = build_graph(&fsproj(0), &mut resolve);
        assert_eq!(
            graph.problems,
            vec![GraphProblem::UnsupportedKind {
                referrer: fsproj(0),
                target: PathBuf::from("/p/Legacy.vbproj"),
                span: 3..9,
            }]
        );
        // The unsupported target is not a node.
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn diamond_dedups_shared_dependency() {
        // P0 → {P1, P2}; P1 → P3; P2 → P3. P3 appears once.
        let adj = vec![vec![1, 2], vec![3], vec![3], vec![]];
        let graph = build_graph(&fsproj(0), adjacency_resolver(&adj));
        assert!(graph.problems.is_empty());
        let p3_count = graph.nodes.iter().filter(|nd| nd.path == fsproj(3)).count();
        assert_eq!(p3_count, 1);
        assert_eq!(graph.nodes.len(), 4);
    }
}
