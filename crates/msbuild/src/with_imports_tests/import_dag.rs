//! Property test: random acyclic import DAGs propagate properties to the
//! entry project.

use super::*;
use proptest::prelude::*;
use tempfile::TempDir;

// ----- Property test -----------------------------------------------------

/// Random acyclic import DAG with node 0 as the entry project.
/// `imports[i]` may reference any node with index `> i`, so node 0
/// (the root) can reach anything and node `n-1` is always a leaf.
/// We build the corresponding files, walk from `nodes[0]`, and check
/// the invariants below. Acyclic by construction (edges only go to
/// strictly larger indices), so the only duplicate-import skips
/// exercised here are diamonds — the cyclic cases are covered by the
/// `cycles_and_failures` unit tests; here we're stressing the merging
/// path.
///
/// An earlier version of this generator had the orientation flipped
/// (`imports[i]` references `< i`, with `imports[0] = empty`), which
/// made the root unreachable from itself's imports — the proptest
/// only ever checked `Prop0` and silently passed for any walker.
#[derive(Debug, Clone)]
struct Dag {
    /// Adjacency list: `imports[i]` is the (sorted, deduped) list of
    /// indices `i` imports. Always satisfies `j > i` for every `j`.
    imports: Vec<Vec<usize>>,
}

impl Dag {
    fn n(&self) -> usize {
        self.imports.len()
    }

    /// Indices reachable from node 0, including 0 itself.
    fn reachable_from_root(&self) -> std::collections::HashSet<usize> {
        let mut out = std::collections::HashSet::new();
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            if out.insert(i) {
                for &j in &self.imports[i] {
                    stack.push(j);
                }
            }
        }
        out
    }
}

/// Generate a DAG with `n` nodes where each node imports a uniformly
/// random subset of its strictly-greater successors. Node 0 — the
/// entry project — can import anything; node `n-1` is necessarily a
/// leaf. Acyclic by construction (edges only ever increase the
/// index).
fn dag_strategy(n: usize) -> impl Strategy<Value = Dag> {
    let mut rows: Vec<BoxedStrategy<Vec<usize>>> = Vec::with_capacity(n);
    for i in 0..n {
        let successors: Vec<usize> = ((i + 1)..n).collect();
        let row = if successors.is_empty() {
            Just(Vec::new()).boxed()
        } else {
            let max = successors.len();
            proptest::sample::subsequence(successors, 0..=max)
                .prop_map(|v| {
                    let mut sorted = v;
                    sorted.sort_unstable();
                    sorted.dedup();
                    sorted
                })
                .boxed()
        };
        rows.push(row);
    }
    rows.prop_map(|imports| Dag { imports })
}

/// Materialise the DAG as a tree of fsproj files. Node 0 becomes
/// `node0.fsproj`; other nodes become `nodeI.props`. Returns the
/// project path (canonicalised).
fn write_dag(dag: &Dag, tmp: &Path) -> PathBuf {
    for i in 0..dag.n() {
        let name = if i == 0 {
            "node0.fsproj".to_string()
        } else {
            format!("node{i}.props")
        };
        let mut body = String::from("<Project>\n");
        for &j in &dag.imports[i] {
            body.push_str(&format!("  <Import Project=\"node{j}.props\" />\n"));
        }
        // Each node writes a unique property so we can check
        // propagation (PropI=I), and appends itself to a shared
        // accumulator so we can check *evaluation counts*: MSBuild
        // imports each file at most once per evaluation (later imports
        // are skipped with warning MSB4011), so a node reachable along
        // two DAG paths must still appear exactly once in the trace.
        body.push_str(&format!(
            "  <PropertyGroup><Prop{i}>{i}</Prop{i}><Trace>$(Trace);{i}</Trace></PropertyGroup>\n"
        ));
        body.push_str("</Project>\n");
        write_at(tmp, &name, &body);
    }
    canon(&tmp.join("node0.fsproj"))
}

#[test]
fn dag_strategy_distribution_is_non_trivial() {
    // Distribution sanity check for [`dag_strategy`]: a previous
    // version of the generator had every entry-project import slot
    // forced empty, so the property-propagation proptest below was
    // effectively a no-op — it only ever asserted that `Prop0`
    // showed up and that other nodes' properties didn't. This test
    // pins the generator itself to keep producing inputs where node
    // 0 actually reaches other nodes, so the proptest's invariants
    // continue to bite.
    //
    // P(reachable={0} | n) = 1/n (the proptest's `subsequence`
    // picks a size uniformly in 0..=successor_count). Averaging
    // over n in 1..=8 gives P(reachable={0}) ≈ 0.34, so the mean
    // number of non-trivial cases out of 200 is ≈ 132 with SD ≈ 6.7.
    // We assert at least 80 (≈ 7σ below the mean → false-positive
    // rate well below 1e-11) to keep the test non-flaky.
    use proptest::strategy::{Strategy, ValueTree};
    let mut runner = proptest::test_runner::TestRunner::default();
    let strategy = (1usize..=8).prop_flat_map(dag_strategy);
    let mut total = 0;
    let mut non_trivial = 0;
    for _ in 0..200 {
        let dag = strategy.new_tree(&mut runner).unwrap().current();
        total += 1;
        if dag.reachable_from_root().len() > 1 {
            non_trivial += 1;
        }
    }
    assert!(
        non_trivial >= 80,
        "expected the generator to produce ≥80 cases with reachable > 1 out of {}, \
         got only {}; the strategy may have regressed to never letting node 0 import",
        total,
        non_trivial,
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// For every random acyclic DAG of imports rooted at node 0, the
    /// walker:
    ///   - terminates (the test would hang otherwise);
    ///   - emits no ImportFailed diagnostic (no missing files,
    ///     well-formed XML);
    ///   - exposes every reachable node's distinguishing property in
    ///     the merged result;
    ///   - evaluates every reachable node's body *exactly once*
    ///     (MSBuild's duplicate-import skip: a diamond in the DAG must
    ///     not run the shared file twice).
    #[test]
    fn random_acyclic_import_dag_propagates_properties(
        dag in (1usize..=8).prop_flat_map(dag_strategy)
    ) {
        let tmp = TempDir::new().unwrap();
        let project_path = write_dag(&dag, tmp.path());
        let result = parse_file(&project_path);

        let import_failed: Vec<_> = result.diagnostics.iter().filter(|d|
            matches!(d.kind, DiagnosticKind::ImportFailed { .. })
        ).collect();
        prop_assert!(
            import_failed.is_empty(),
            "unexpected ImportFailed diagnostics: {:?}",
            import_failed
        );

        let reachable = dag.reachable_from_root();
        for &i in &reachable {
            let key = format!("Prop{i}");
            let want = i.to_string();
            prop_assert_eq!(
                result.properties.get(&key).map(String::as_str),
                Some(want.as_str()),
                "property {} should be present from reachable node {}; properties were: {:?}",
                key, i, result.properties
            );
        }
        // Nodes NOT reachable from the root should NOT appear: the
        // walker only sees files explicitly named via the import
        // chain, never the entire tempdir.
        for i in 0..dag.n() {
            if !reachable.contains(&i) {
                let key = format!("Prop{i}");
                prop_assert!(
                    !result.properties.contains_key(&key),
                    "property {} should be absent for unreachable node {}; properties were: {:?}",
                    key, i, result.properties
                );
            }
        }
        // Exactly-once evaluation: each reachable node appended itself to
        // the Trace accumulator when its body ran. A diamond (node
        // reachable along two import paths) must contribute one segment,
        // not two — MSBuild skips the second import of the same file.
        let trace = result.properties.get("Trace").map(String::as_str).unwrap_or("");
        for &i in &reachable {
            let occurrences = trace
                .split(';')
                .filter(|seg| *seg == i.to_string())
                .count();
            prop_assert_eq!(
                occurrences, 1,
                "node {}'s body must run exactly once; trace was {:?}",
                i, trace
            );
        }
    }
}
