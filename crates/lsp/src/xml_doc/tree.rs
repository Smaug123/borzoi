//! The owned XML element tree a documentation entry is rendered from.
//!
//! Deliberately a *generic* element tree rather than a model of the doc-comment
//! vocabulary: the vocabulary in the wild is open-ended (the NuGet cache carries
//! typos, HTML, and MSBuild snippets inside doc comments), so the renderer is a
//! total function over arbitrary trees and the tag knowledge lives there, in one
//! match, instead of in a parser that would have to reject or drop what it does
//! not know. It is also source-agnostic: an assembly's sidecar `.xml` builds it
//! here, and a project-local `///` comment can build the same tree.

/// One node of a documentation tree: an element or a run of character data.
///
/// Comments and processing instructions are not represented — they are not
/// documentation. CDATA sections arrive as [`DocNode::Text`], which is what they
/// mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocNode {
    Element(DocElement),
    Text(String),
}

/// An element: its *local* name, its attributes in document order, and its
/// children.
///
/// Names are local names: no doc-comment tag is namespaced, and a stray default
/// namespace on an ancestor must not stop `<summary>` from being a summary.
/// What the local name forgets is kept as one bit, [`Self::namespaced`]: the
/// renderer ignores it, but `<inheritdoc>` expansion reproduces a consumer
/// (Roslyn) for which a namespaced `<inheritdoc>`, `cref` or path step is a
/// different name, so it declines wherever the bit is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocElement {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<DocNode>,
    /// The element, or one of its attributes, has a non-empty namespace URI.
    pub namespaced: bool,
}

impl DocElement {
    /// The value of the attribute named `name`, if present.
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The concatenated character data of this element and all its
    /// descendants, in document order.
    pub fn text_content(&self) -> String {
        let mut out = String::new();
        push_text_content(&self.children, &mut out);
        out
    }

    /// Build the owned tree for a parsed `roxmltree` element.
    ///
    /// `roxmltree` parses iteratively and imposes no nesting limit, while this
    /// conversion — and everything that later walks the tree — recurses, so a
    /// hostile file nesting a million elements would overflow the stack. Trees
    /// deeper than [`MAX_DEPTH`] are therefore refused here, at the one door
    /// every tree comes through, and nothing downstream needs its own guard.
    pub fn from_roxmltree(node: roxmltree::Node<'_, '_>) -> Result<Self, TooDeep> {
        Self::from_roxmltree_at(node, 0)
    }

    fn from_roxmltree_at(node: roxmltree::Node<'_, '_>, depth: usize) -> Result<Self, TooDeep> {
        debug_assert!(node.is_element(), "only elements become a DocElement");
        if depth > MAX_DEPTH {
            return Err(TooDeep);
        }
        let mut children = Vec::new();
        for child in node.children() {
            if child.is_element() {
                children.push(DocNode::Element(Self::from_roxmltree_at(child, depth + 1)?));
            } else if let Some(text) = child.text().filter(|_| child.is_text()) {
                children.push(DocNode::Text(text.to_string()));
            }
        }
        Ok(DocElement {
            name: node.tag_name().name().to_string(),
            attributes: node
                .attributes()
                .map(|a| (a.name().to_string(), a.value().to_string()))
                .collect(),
            children,
            namespaced: node.tag_name().namespace().is_some()
                || node.attributes().any(|a| a.namespace().is_some()),
        })
    }

    /// An element with no namespace, as a consumer would build it.
    pub fn new(name: &str, attributes: Vec<(String, String)>, children: Vec<DocNode>) -> Self {
        DocElement {
            name: name.to_string(),
            attributes,
            children,
            namespaced: false,
        }
    }

    /// Whether this element or any descendant is [`Self::namespaced`].
    pub fn any_namespaced(&self) -> bool {
        self.namespaced
            || self.children.iter().any(|c| match c {
                DocNode::Element(e) => e.any_namespaced(),
                DocNode::Text(_) => false,
            })
    }

    /// The deepest element nesting below this element (0 for a leaf).
    pub fn depth(&self) -> usize {
        self.children
            .iter()
            .filter_map(|c| match c {
                DocNode::Element(e) => Some(1 + e.depth()),
                DocNode::Text(_) => None,
            })
            .max()
            .unwrap_or(0)
    }

    /// How many nodes (elements and text runs) the tree holds, this element
    /// included.
    pub fn node_count(&self) -> usize {
        1 + self
            .children
            .iter()
            .map(|c| match c {
                DocNode::Element(e) => e.node_count(),
                DocNode::Text(_) => 1,
            })
            .sum::<usize>()
    }

    /// The same tree with adjacent text runs merged and empty ones dropped,
    /// at every level: the form an XML parser hands back for the
    /// serialisation of this tree, so two trees that serialise identically
    /// compare equal.
    pub fn normalized(self) -> Self {
        let mut children: Vec<DocNode> = Vec::with_capacity(self.children.len());
        for child in self.children {
            match child {
                DocNode::Text(t) if t.is_empty() => {}
                DocNode::Text(t) => match children.last_mut() {
                    Some(DocNode::Text(prev)) => prev.push_str(&t),
                    _ => children.push(DocNode::Text(t)),
                },
                DocNode::Element(e) => children.push(DocNode::Element(e.normalized())),
            }
        }
        DocElement { children, ..self }
    }
}

/// .NET's `OrdinalIgnoreCase` equality of an element name against an ASCII
/// lower-case word — how C# compares documentation element names
/// (`DocumentationCommentXmlNames.ElementEquals`): per character, equal, or
/// equal once upper-cased by its simple mapping, which also lets `ı` (U+0131,
/// upper-casing to `I`) stand for `i`.
pub fn name_is(name: &str, word: &str) -> bool {
    debug_assert!(word.bytes().all(|b| b.is_ascii_lowercase()));
    let mut a = name.chars();
    let mut b = word.chars();
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) => {
                let mut upper = x.to_uppercase();
                let single = match (upper.next(), upper.next()) {
                    (Some(u), None) => u,
                    _ => return false,
                };
                if x != y && single != y.to_ascii_uppercase() {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// The deepest element nesting (below the converted element) a [`DocElement`]
/// may have — the same bound [`super::depth::parse_bounded`] puts on a whole
/// file, so a tree from a bounded parse always converts.
pub const MAX_DEPTH: usize = super::depth::MAX_FILE_DEPTH;

/// A documentation tree nested deeper than [`MAX_DEPTH`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooDeep;

fn push_text_content(nodes: &[DocNode], out: &mut String) {
    for node in nodes {
        match node {
            DocNode::Text(t) => out.push_str(t),
            DocNode::Element(e) => push_text_content(&e.children, out),
        }
    }
}
