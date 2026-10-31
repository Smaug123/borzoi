//! `xml_doc::xpath` against Roslyn's own selection (`XPathEvaluate` over the
//! inherited document, through `tools/inheritdoc-oracle`'s `xpath` op).
//!
//! Certain-implies-exact: whenever our subset parses an expression, Roslyn
//! selects exactly the nodes we do. Expressions we refuse make no claim. The
//! generator draws trees and expressions from the vocabulary documentation
//! uses (and some it does not), so both the modelled shapes and the edge of
//! the subset are exercised; a liveness check keeps the property from passing
//! by refusing everything.

use borzoi::xml_doc::tree::{DocElement, DocNode};
use borzoi::xml_doc::xpath::{authored_path, default_path};
use proptest::prelude::*;

use crate::common::inheritdoc_oracle::oracle;

fn parse_tree(xml: &str) -> DocElement {
    let doc = roxmltree::Document::parse(xml).unwrap_or_else(|e| panic!("{e}: {xml}"));
    DocElement::from_roxmltree(doc.root_element()).unwrap()
}

/// Serialise a tree back to XML (attribute values and text escaped).
fn to_xml(e: &DocElement, out: &mut String) {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    out.push('<');
    out.push_str(&e.name);
    for (k, v) in &e.attributes {
        out.push_str(&format!(" {k}=\"{}\"", esc(v)));
    }
    out.push('>');
    for c in &e.children {
        match c {
            DocNode::Element(c) => to_xml(c, out),
            DocNode::Text(t) => out.push_str(&esc(t)),
        }
    }
    out.push_str("</");
    out.push_str(&e.name);
    out.push('>');
}

const NAMES: &[&str] = &[
    "summary",
    "param",
    "remarks",
    "para",
    "overloads",
    "c",
    "member",
];

/// The namespacing an element of a generated tree may carry (a default
/// namespace, which its descendants inherit, or a namespaced attribute):
/// unprefixed XPath name tests match only elements in no namespace, and
/// attribute tests only attributes in none. Rare, so that most trees are
/// namespace-free and the selection makes a claim.
fn namespacing() -> impl Strategy<Value = &'static [(&'static str, &'static str)]> {
    prop_oneof![
        18 => Just(&[][..]),
        1 => Just(&[("xmlns", "urn:x")][..]),
        1 => Just(&[("xml:lang", "en")][..]),
    ]
}

fn tree() -> impl Strategy<Value = DocElement> {
    let leaf = prop_oneof![
        Just(DocNode::Text("x".into())),
        Just(DocNode::Text(" ".into())),
        Just(DocNode::Text("y\n".into())),
    ];
    let node = leaf.prop_recursive(3, 24, 4, |inner| {
        (
            prop::sample::select(NAMES),
            prop::option::of(prop::sample::select(&["a", "b"][..])),
            namespacing(),
            prop::collection::vec(inner, 0..4),
        )
            .prop_map(|(name, attr, namespacing, children)| {
                let mut attributes: Vec<(String, String)> = attr
                    .map(|a| vec![("name".to_string(), a.to_string())])
                    .unwrap_or_default();
                attributes.extend(
                    namespacing
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string())),
                );
                DocNode::Element(DocElement::new(name, attributes, children))
            })
    });
    prop::collection::vec(node, 0..5).prop_map(|children| {
        DocElement::new("member", vec![("name".into(), "M:X".into())], children)
    })
}

/// Expressions near the modelled subset: its own steps and predicates,
/// joined with the separators and spacing XPath allows and some it does not.
fn expression() -> impl Strategy<Value = String> {
    let step = prop_oneof![
        Just("*".to_string()),
        Just("node()".to_string()),
        Just("text()".to_string()),
        prop::sample::select(NAMES).prop_map(str::to_string),
    ];
    let predicate = prop_oneof![
        Just("[@name='a']".to_string()),
        Just("[@name=\"b\"]".to_string()),
        Just("[@name = 'a']".to_string()),
        Just("[not(self::overloads)]".to_string()),
        Just("[not(self::para)]".to_string()),
        Just("[1]".to_string()),
    ];
    let full_step = (step, prop::collection::vec(predicate, 0..2))
        .prop_map(|(s, ps)| format!("{s}{}", ps.concat()));
    (
        prop::sample::select(&["/", "", "//"][..]),
        prop::collection::vec(full_step, 1..4),
        prop::sample::select(&["/", "//"][..]),
    )
        .prop_map(|(lead, steps, sep)| format!("{lead}{}", steps.join(sep)))
}

fn selection(xml: &str) -> Vec<DocNode> {
    parse_tree(xml).normalized().children
}

fn ours(root: &DocElement, nodes: Vec<DocNode>) -> Vec<DocNode> {
    let _ = root;
    DocElement::new("selection", Vec::new(), nodes)
        .normalized()
        .children
}

proptest! {
    /// An authored `path`, as Roslyn rewrites and evaluates it.
    #[test]
    fn an_authored_path_selects_what_roslyn_selects(generated in tree(), path in expression()) {
        let Ok(parsed) = authored_path(&path) else { return Ok(()) };
        let mut xml = String::new();
        to_xml(&generated, &mut xml);
        let root = parse_tree(&xml);
        let Some(selected) = parsed.select(&root) else { return Ok(()) };
        let rooted = if path.starts_with('/') { format!("/*{path}") } else { path.clone() };
        let theirs = oracle().lock().unwrap().xpath(&xml, &rooted);
        let theirs = theirs.unwrap_or_else(|| panic!("we parse {path:?}, Roslyn's selection fails"));
        prop_assert_eq!(ours(&root, selected), selection(&theirs), "{} over {}", path, xml);
    }

    /// The path Roslyn builds from an `<inheritdoc>`'s ancestry.
    #[test]
    fn a_default_path_selects_what_roslyn_selects(
        generated in tree(),
        ancestry in prop::collection::vec(prop::sample::select(NAMES), 0..3),
    ) {
        let mut chain = vec!["member"];
        chain.extend(ancestry.iter().copied());
        let parsed = default_path(&chain).expect("a default path is always in the subset");
        // Roslyn's own construction, spelled out independently.
        let last = *chain.last().unwrap();
        let expr = if matches!(last, "member" | "doc") {
            "/*/node()[not(self::overloads)]".to_string()
        } else {
            let mut s = String::new();
            for name in &chain {
                s.push('/');
                s.push_str(if matches!(*name, "member" | "doc") { "*" } else { name });
            }
            s + "/node()[not(self::overloads)]"
        };
        let mut xml = String::new();
        to_xml(&generated, &mut xml);
        let root = parse_tree(&xml);
        let Some(selected) = parsed.select(&root) else { return Ok(()) };
        let theirs = oracle().lock().unwrap().xpath(&xml, &expr).expect("Roslyn evaluates its own default path");
        prop_assert_eq!(ours(&root, selected), selection(&theirs), "{} over {}", expr, xml);
    }
}

/// The generator reaches the subset: without this the authored-path property
/// could pass by our refusing every expression it draws.
#[test]
fn the_expression_generator_reaches_the_subset() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strategy = expression();
    let parsed = (0..500)
        .filter(|_| {
            let e = strategy.new_tree(&mut runner).unwrap().current();
            authored_path(&e).is_ok()
        })
        .count();
    assert!(parsed >= 100, "only {parsed}/500 expressions parse");
}

/// The tree generator reaches both sides of the namespace dimension: trees
/// the selection answers for, and trees it declines.
#[test]
fn the_tree_generator_reaches_both_namespacings() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strategy = tree();
    let path = default_path(&["member"]).unwrap();
    let answered = (0..500)
        .filter(|_| {
            let generated = strategy.new_tree(&mut runner).unwrap().current();
            let mut xml = String::new();
            to_xml(&generated, &mut xml);
            path.select(&parse_tree(&xml)).is_some()
        })
        .count();
    assert!(
        (150..=450).contains(&answered),
        "{answered}/500 trees are namespace-free"
    );
}

/// .NET's XPath evaluator caps an expression's complexity, failing past it
/// (and Roslyn then keeps the `<inheritdoc>`). The subset caps it lower: at
/// its bound both sides select alike, and one term more is refused.
#[test]
fn an_expression_at_the_subset_bound_is_roslyns_and_one_past_it_is_refused() {
    use borzoi::xml_doc::xpath::MAX_TERMS;
    let xml = "<member><summary>s</summary><remarks>r</remarks></member>";
    let root = parse_tree(xml);
    let path = |terms: usize| {
        format!(
            "/summary{}",
            "[not(self::overloads)]".repeat(terms.saturating_sub(1))
        )
    };
    // `authored_path` prepends `/*`, a term of its own.
    let at_bound = path(MAX_TERMS - 1);
    let parsed = authored_path(&at_bound).expect("the bound is in the subset");
    let theirs = oracle()
        .lock()
        .unwrap()
        .xpath(xml, &format!("/*{at_bound}"))
        .expect("Roslyn evaluates the subset's largest expression");
    assert_eq!(
        ours(&root, parsed.select(&root).expect("no namespace")),
        selection(&theirs)
    );
    assert!(authored_path(&path(MAX_TERMS)).is_err());
}

/// The oracle caches compilations and metadata references by path; a
/// fixture rewritten in place must be read afresh.
#[test]
fn the_oracle_reads_a_fixture_rewritten_in_place() {
    use crate::common::ensure_system_runtime_dll;
    use crate::common::inheritdoc_oracle::Expanded;
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = ensure_system_runtime_dll();
    let source = |summary: &str| {
        format!("namespace F;\n/// <summary>{summary}</summary>\npublic class C {{ }}")
    };
    let mut summaries = Vec::new();
    for summary in ["first", "second"] {
        let mut oracle = oracle().lock().unwrap();
        let dll = oracle
            .compile(
                &source(summary),
                "Fx",
                dir.path(),
                std::slice::from_ref(&runtime),
            )
            .unwrap_or_else(|e| panic!("{e:#?}"));
        let expanded = oracle.expand(
            &[runtime.clone(), dll.clone()],
            &dll,
            &["T:F.C".to_string()],
        );
        match expanded.as_slice() {
            [Expanded::Ok { xml, .. }] => summaries.push(xml.clone()),
            other => panic!("{other:?}"),
        }
    }
    assert!(summaries[0].contains("first"), "{summaries:?}");
    assert!(summaries[1].contains("second"), "{summaries:?}");
}

/// A path selecting the document node itself (`.`, `/`) is no selection
/// Roslyn can expand to: it keeps the `<inheritdoc>`, and the oracle says
/// its selection fails rather than failing itself.
#[test]
fn a_path_selecting_the_document_node_fails_in_the_oracle() {
    let xml = "<member><summary>s</summary></member>";
    for path in [".", "/"] {
        assert_eq!(oracle().lock().unwrap().xpath(xml, path), None, "{path}");
    }
}

/// Roslyn takes an `<inheritdoc>` by local name ignoring case, in no
/// namespace: the oracle's rule census counts each one it expands.
#[test]
fn the_oracles_rule_census_counts_every_spelling_roslyn_takes() {
    use crate::common::ensure_system_runtime_dll;
    use crate::common::inheritdoc_oracle::Expanded;
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = ensure_system_runtime_dll();
    let source = "namespace F;\n/// <summary>C.</summary>\npublic class C { }\n\
        /// <Inheritdoc cref=\"T:F.C\"/>\npublic class D { }\n\
        /// <inheritdoc xmlns=\"urn:x\" cref=\"T:F.C\"/>\npublic class E { }";
    let mut oracle = oracle().lock().unwrap();
    let dll = oracle
        .compile(source, "Fx", dir.path(), std::slice::from_ref(&runtime))
        .unwrap_or_else(|e| panic!("{e:#?}"));
    let expanded = oracle.expand(
        &[runtime.clone(), dll.clone()],
        &dll,
        &["T:F.D".to_string(), "T:F.E".to_string()],
    );
    match expanded.as_slice() {
        [Expanded::Ok { rules: d, .. }, Expanded::Ok { rules: e, .. }] => {
            assert_eq!(d.len(), 1, "{d:?}");
            assert!(e.is_empty(), "{e:?}");
        }
        other => panic!("{other:?}"),
    }
}
