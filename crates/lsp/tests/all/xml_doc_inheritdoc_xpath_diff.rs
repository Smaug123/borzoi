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
            prop::collection::vec(inner, 0..4),
        )
            .prop_map(|(name, attr, children)| {
                let attributes = attr
                    .map(|a| vec![("name".to_string(), a.to_string())])
                    .unwrap_or_default();
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
    fn an_authored_path_selects_what_roslyn_selects(root in tree(), path in expression()) {
        let Ok(parsed) = authored_path(&path) else { return Ok(()) };
        let mut xml = String::new();
        to_xml(&root, &mut xml);
        let rooted = if path.starts_with('/') { format!("/*{path}") } else { path.clone() };
        let theirs = oracle().lock().unwrap().xpath(&xml, &rooted);
        let theirs = theirs.unwrap_or_else(|| panic!("we parse {path:?}, Roslyn's selection fails"));
        prop_assert_eq!(ours(&root, parsed.select(&root)), selection(&theirs), "{} over {}", path, xml);
    }

    /// The path Roslyn builds from an `<inheritdoc>`'s ancestry.
    #[test]
    fn a_default_path_selects_what_roslyn_selects(
        root in tree(),
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
        to_xml(&root, &mut xml);
        let theirs = oracle().lock().unwrap().xpath(&xml, &expr).expect("Roslyn evaluates its own default path");
        prop_assert_eq!(ours(&root, parsed.select(&root)), selection(&theirs), "{} over {}", expr, xml);
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
