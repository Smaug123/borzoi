//! The XPath an `<inheritdoc>` selects inherited nodes with, restricted to the
//! subset documentation comments use and evaluated exactly as Roslyn's IDE
//! does (`XPathEvaluate` over the inherited document, whose root is the
//! inherited `<member>`).
//!
//! Roslyn builds the expression one of two ways, and both are modelled:
//!
//! - with no `path` attribute (or an empty one), from the `<inheritdoc>`'s own
//!   ancestry: `/*/node()[not(self::overloads)]` at the top of an entry, and
//!   `/*/summary/node()[not(self::overloads)]` inside a `<summary>` (an
//!   ancestor named `member` or `doc` becomes `*`) — [`default_path`];
//! - from the attribute, with `/*` prepended when it starts with `/` (the
//!   author writes `/summary`, meaning the entry's summary) — [`authored_path`].
//!
//! The subset is a location path of child steps, each a name test, `*` or
//! `node()`, filtered by `[@attr='value']` and `[not(self::name)]` predicates.
//! That covers every `path` measured in shipped documentation but a handful of
//! malformed ones. Anything else — another axis, a function, a position, a
//! prefix, whitespace between tokens — is [`Unsupported`] rather than
//! approximated, and the caller declines.

use super::tree::{DocElement, DocNode, name_is};

/// A parsed expression in the supported subset: child steps from the
/// document node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Step {
    test: Test,
    predicates: Vec<Predicate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Test {
    /// An element of this (unprefixed) name.
    Name(String),
    /// `*`: any element.
    AnyElement,
    /// `node()`: any child node.
    AnyNode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Predicate {
    /// `[@name='value']`: an element whose unnamespaced attribute `name` is
    /// exactly `value`.
    AttributeEquals(String, String),
    /// `[not(self::name)]`: anything but an element named `name`.
    NotSelf(String),
}

/// An expression outside the modelled subset (or not XPath at all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported(pub String);

/// The expression Roslyn evaluates for an `<inheritdoc>` without a `path`:
/// `ancestry` is the element names from the entry's root down to the
/// `<inheritdoc>`'s parent, inclusive.
pub fn default_path(ancestry: &[&str]) -> Result<Path, Unsupported> {
    // Roslyn tests these names `OrdinalIgnoreCase`, as it does every
    // documentation element name; the names it writes into the path are
    // matched by XPath, case-sensitively, as written.
    let member_or_doc = |name: &str| name_is(name, "member") || name_is(name, "doc");
    let mut text = String::new();
    match ancestry.last() {
        // Roslyn's shortcut for a top-level `<inheritdoc>`, and its general
        // form agree; a parent named `member`/`doc` anywhere takes it.
        Some(last) if member_or_doc(last) => text.push_str("/*"),
        None => text.push_str("/*"),
        Some(_) => {
            for name in ancestry {
                text.push('/');
                text.push_str(if member_or_doc(name) { "*" } else { name });
            }
        }
    }
    text.push_str("/node()[not(self::overloads)]");
    parse(&text)
}

/// The expression Roslyn evaluates for an authored `path` attribute.
pub fn authored_path(path: &str) -> Result<Path, Unsupported> {
    if path.starts_with('/') {
        parse(&format!("/*{path}"))
    } else {
        parse(path)
    }
}

/// Parse an expression of the subset.
pub fn parse(text: &str) -> Result<Path, Unsupported> {
    let unsupported = || Unsupported(text.to_string());
    let mut rest = text.strip_prefix('/').unwrap_or(text);
    let mut steps = Vec::new();
    loop {
        let (step, after) = parse_step(rest).ok_or_else(unsupported)?;
        steps.push(step);
        if after.is_empty() {
            break;
        }
        // `//` (descendant-or-self) is outside the subset: a step must
        // follow every `/`.
        rest = after.strip_prefix('/').ok_or_else(unsupported)?;
        if rest.is_empty() || rest.starts_with('/') {
            return Err(unsupported());
        }
    }
    Ok(Path { steps })
}

fn parse_step(text: &str) -> Option<(Step, &str)> {
    let (test, mut rest) = if let Some(rest) = text.strip_prefix("node()") {
        (Test::AnyNode, rest)
    } else if let Some(rest) = text.strip_prefix('*') {
        (Test::AnyElement, rest)
    } else {
        let (name, rest) = ncname(text)?;
        // A name followed by `(` is a function call or node-type test
        // (`text()`, `comment()`); `::` an axis.
        if rest.starts_with('(') || rest.starts_with(':') {
            return None;
        }
        (Test::Name(name.to_string()), rest)
    };
    let mut predicates = Vec::new();
    while let Some(inner) = rest.strip_prefix('[') {
        let (predicate, after) = parse_predicate(inner)?;
        predicates.push(predicate);
        rest = after.strip_prefix(']')?;
    }
    Some((Step { test, predicates }, rest))
}

fn parse_predicate(text: &str) -> Option<(Predicate, &str)> {
    if let Some(rest) = text.strip_prefix('@') {
        let (name, rest) = ncname(rest)?;
        let rest = rest.strip_prefix('=')?;
        let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
        let body = &rest[1..];
        let end = body.find(quote)?;
        return Some((
            Predicate::AttributeEquals(name.to_string(), body[..end].to_string()),
            &body[end + 1..],
        ));
    }
    let rest = text.strip_prefix("not(self::")?;
    let (name, rest) = ncname(rest)?;
    Some((
        Predicate::NotSelf(name.to_string()),
        rest.strip_prefix(')')?,
    ))
}

/// An ASCII NCName at the start of `text`, and what follows it. Non-ASCII
/// name characters are legal XPath but outside the subset.
fn ncname(text: &str) -> Option<(&str, &str)> {
    let mut chars = text.char_indices();
    let (_, first) = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let end = chars
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .map_or(text.len(), |(i, _)| i);
    if text[end..].starts_with(|c: char| !c.is_ascii()) {
        return None;
    }
    Some((&text[..end], &text[end..]))
}

impl Path {
    /// The nodes the expression selects from the document whose only child
    /// is `root`, in document order, cloned.
    pub fn select(&self, root: &DocElement) -> Vec<DocNode> {
        // The document node is not an element; model it as one whose only
        // child is the root.
        let document = DocElement::new("", Vec::new(), vec![DocNode::Element(root.clone())]);
        let mut current: Vec<&DocNode> = Vec::new();
        let document_node = DocNode::Element(document);
        current.push(&document_node);
        for step in &self.steps {
            let mut next: Vec<&DocNode> = Vec::new();
            for node in current {
                if let DocNode::Element(parent) = node {
                    next.extend(parent.children.iter().filter(|c| step.matches(c)));
                }
            }
            current = next;
        }
        current.into_iter().cloned().collect()
    }
}

impl Step {
    fn matches(&self, node: &DocNode) -> bool {
        let test = match (&self.test, node) {
            (Test::AnyNode, _) => true,
            (Test::AnyElement, DocNode::Element(_)) => true,
            (Test::Name(name), DocNode::Element(e)) => e.name == *name,
            (_, DocNode::Text(_)) => false,
        };
        test && self.predicates.iter().all(|p| match (p, node) {
            (Predicate::AttributeEquals(name, value), DocNode::Element(e)) => {
                e.attribute(name) == Some(value.as_str())
            }
            (Predicate::AttributeEquals(..), DocNode::Text(_)) => false,
            (Predicate::NotSelf(name), DocNode::Element(e)) => e.name != *name,
            (Predicate::NotSelf(_), DocNode::Text(_)) => true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(xml: &str) -> DocElement {
        let doc = roxmltree::Document::parse(xml).unwrap();
        DocElement::from_roxmltree(doc.root_element()).unwrap()
    }

    fn names(nodes: &[DocNode]) -> Vec<String> {
        nodes
            .iter()
            .map(|n| match n {
                DocNode::Element(e) => format!("<{}>{}", e.name, e.text_content()),
                DocNode::Text(t) => format!("{t:?}"),
            })
            .collect()
    }

    #[test]
    fn the_default_path_follows_the_ancestry() {
        assert_eq!(
            default_path(&["member"]),
            parse("/*/node()[not(self::overloads)]")
        );
        assert_eq!(
            default_path(&["member", "summary"]),
            parse("/*/summary/node()[not(self::overloads)]")
        );
        assert_eq!(
            default_path(&["doc", "param", "member", "list"]),
            parse("/*/param/*/list/node()[not(self::overloads)]")
        );
    }

    #[test]
    fn an_authored_absolute_path_is_rooted_at_the_entry() {
        assert_eq!(authored_path("/summary"), parse("/*/summary"));
        assert_eq!(authored_path("summary"), parse("summary"));
    }

    #[test]
    fn selection_is_children_in_document_order() {
        let root = tree(
            r#"<member name="X"><summary>s</summary><overloads>o</overloads><param name="a">pa</param><param name="b">pb</param></member>"#,
        );
        let all = default_path(&["member"]).unwrap().select(&root);
        assert_eq!(names(&all), ["<summary>s", "<param>pa", "<param>pb"]);
        let b = authored_path("/param[@name='b']").unwrap().select(&root);
        assert_eq!(names(&b), ["<param>pb"]);
        let inside = default_path(&["member", "summary"]).unwrap().select(&root);
        assert_eq!(names(&inside), ["\"s\""]);
        // Relative to the document node, `summary` names nothing: the
        // document's one child is the entry.
        assert!(authored_path("summary").unwrap().select(&root).is_empty());
    }

    #[test]
    fn outside_the_subset_is_unsupported() {
        for text in [
            "//summary",
            "/*//summary",
            "/*/summary/text()",
            "/*/param[1]",
            "/*/param[@name = 'a']",
            "/*/x:summary",
            "/*/param[@name='unterminated]",
            "/",
            "",
            "/*/",
            "/*/following-sibling::x",
            "count(/*)",
        ] {
            assert!(parse(text).is_err(), "{text:?} parsed");
        }
    }
}
