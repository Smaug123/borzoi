//! Documentation trees to Markdown.
//!
//! [`render_member`] turns one `<member>` element into [`Block`]s: the summary
//! and any loose content first, then labelled sections in a fixed order (type
//! parameters, parameters, returns, value, exceptions, permissions, remarks,
//! examples, see-also), then any section tag this renderer does not know, under
//! its own name. It is a total function over arbitrary element trees: every
//! element either has a mapping here or falls back to its content, so nothing an
//! author wrote is dropped except the handful of tags that are metadata rather
//! than prose (`is_ignored`). Each fallback is recorded in the
//! [`RenderReport`], which is what the shipped-file sweep censuses.
//!
//! `<inheritdoc>` and `<include>` are not resolved here: hover hands this the
//! entry [`super::inherit`] expanded, so an `<inheritdoc>` that reaches the
//! renderer is one that could not be expanded exactly (and `<include>` never
//! can be). Where one appears, the rendering says so in place, and the report
//! records it, so an incomplete documentation is never presented as a
//! complete one.

use super::markdown::{Block, Inline};
use super::tree::{DocElement, DocNode};

/// What rendering one tree had to fall back on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderReport {
    /// Element names (lower-cased) with no mapping, rendered as their content —
    /// or, at member level, as a section labelled with the name. One entry per
    /// occurrence.
    pub unknown_tags: Vec<String>,
    /// The tree contains an `<inheritdoc>`, whose inherited text is not shown.
    pub unresolved_inheritdoc: bool,
    /// The tree contains an `<include>`, whose included text is not shown.
    pub unresolved_include: bool,
}

/// Render a `<member>` element (or any element whose children are member-level
/// documentation, such as a `///` comment's synthetic root).
pub fn render_member(member: &DocElement) -> (Vec<Block>, RenderReport) {
    let mut r = Renderer::default();
    let blocks = r.member(member);
    (blocks, r.report)
}

/// Render content (the inside of a `<summary>`, `<remarks>`, …) as blocks,
/// preserving document order.
pub fn render_content(nodes: &[DocNode]) -> (Vec<Block>, RenderReport) {
    let mut r = Renderer::default();
    let blocks = r.flow_of(nodes, Flow::default());
    (blocks, r.report)
}

/// Tags that are metadata — tooling hints, visibility flags, a namespace's
/// documentation parked on one of its types — rather than documentation of the
/// member, and are deliberately not rendered.
///
/// - `filterpriority`: an IntelliSense ordering hint (old BCL docs).
/// - `exclude`, `nodoc`, `internalonly`, `forinternaluseonly`: "do not
///   document" flags for doc generators.
/// - `category`: a doc-site grouping label (FSharp.Core).
/// - `namespacedoc`: documentation of the enclosing *namespace*, which fsc
///   attaches to an arbitrary type in it (FSharp.Core).
/// - `example-tbd`: an empty "example to be written" placeholder (FSharp.Core).
/// - `permissionset`, `ipermission`: Code Access Security declarations, a
///   retired .NET Framework mechanism with no meaning on .NET Core.
/// - `script`: executable page script from the docs pipeline, not prose.
/// - `member`: a `<member>` nested inside an entry documents some *other* key
///   (and the index serves it as that key's entry); showing it under this
///   symbol would misattribute it.
fn is_ignored(name: &str) -> bool {
    matches!(
        name,
        "script"
            | "member"
            | "filterpriority"
            | "exclude"
            | "nodoc"
            | "internalonly"
            | "forinternaluseonly"
            | "category"
            | "namespacedoc"
            | "example-tbd"
            | "permissionset"
            | "ipermission"
    )
}

/// A readable rendering of a `cref`: the documentation-comment ID without its
/// kind prefix (`T:`, `M:`, …, and Roslyn's `!:` for an unresolved reference or
/// DocFX's `Overload:`), with generic braces back as angle brackets —
/// `M:System.String.Join(System.String,System.String[])` reads
/// `System.String.Join(System.String,System.String[])`. The full name is kept:
/// it is unambiguous, and the reader may need it to look the target up.
pub fn readable_cref(cref: &str) -> String {
    let mut rest = cref.trim();
    rest = rest.strip_prefix("!:").unwrap_or(rest);
    rest = rest.strip_prefix("Overload:").unwrap_or(rest);
    if let [kind, b':', ..] = rest.as_bytes()
        && b"TMPFENOtmpfeno".contains(kind)
    {
        rest = &rest[2..];
    }
    rest.replace('{', "<").replace('}', ">")
}

/// Remove the indentation common to every non-blank line, and blank lines at
/// either end — the shape `<code>` content takes when it is indented to sit
/// inside the surrounding XML. Everything else is kept verbatim. Idempotent.
pub fn dedent(code: &str) -> String {
    let code = code.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = code.split('\n').collect();
    let blank = |l: &str| l.chars().all(|c| c == ' ' || c == '\t');
    let Some(start) = lines.iter().position(|l| !blank(l)) else {
        return String::new();
    };
    let end = lines
        .iter()
        .rposition(|l| !blank(l))
        .map_or(start, |i| i + 1);
    let lines = &lines[start..end];
    let indent_of = |l: &str| -> usize { l.len() - l.trim_start_matches([' ', '\t']).len() };
    // The common *prefix* (not merely the common width), so a tab and four
    // spaces are not taken for the same indentation.
    let mut common: Option<&str> = None;
    for line in lines.iter().filter(|l| !blank(l)) {
        let indent = &line[..indent_of(line)];
        common = Some(match common {
            None => indent,
            Some(c) => {
                let shared = c
                    .bytes()
                    .zip(indent.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                &c[..shared]
            }
        });
    }
    let common = common.unwrap_or("");
    lines
        .iter()
        .map(|l| {
            if blank(l) {
                ""
            } else {
                l.strip_prefix(common).unwrap_or(l)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A paragraph under construction plus the blocks before it.
#[derive(Default)]
struct Flow {
    blocks: Vec<Block>,
    para: Vec<Inline>,
    /// Some block boundary was met (a paragraph ended, a block began, a line
    /// broke) before the end — so the content is not plain inline text, even if it
    /// ends up a single paragraph.
    broke: bool,
}

impl Flow {
    fn inline(&mut self, inline: Inline) {
        // A line break is a boundary too: flattened into one inline's text it
        // would sit at an edge that gets trimmed.
        self.broke |= matches!(inline, Inline::LineBreak);
        self.para.push(inline);
    }

    /// End the paragraph under construction at a block boundary.
    fn flush(&mut self) {
        self.broke = true;
        self.end_paragraph();
    }

    /// End the paragraph under construction. One holding nothing visible —
    /// the whitespace between XML tags — is dropped here rather than left for
    /// normalisation, so that [`labelled`] sees the block that really opens
    /// the content.
    fn end_paragraph(&mut self) {
        let para = std::mem::take(&mut self.para);
        let visible = para.iter().any(|i| match i {
            Inline::Text(t) => !t.trim().is_empty(),
            Inline::LineBreak => false,
            Inline::Code(t) | Inline::Strong(t) | Inline::Emphasis(t) => !t.trim().is_empty(),
            Inline::Link { .. } => true,
        });
        if visible {
            self.blocks.push(Block::Paragraph(para));
        }
    }

    fn block(&mut self, block: Block) {
        self.flush();
        self.blocks.push(block);
    }

    fn finish(mut self) -> Vec<Block> {
        self.end_paragraph();
        self.blocks
    }

    /// As [`Self::finish`], also saying whether a block boundary was met —
    /// for content about to be flattened into one inline, so its final
    /// paragraph is kept even when it is only whitespace: `<b> </b>` between
    /// two words is still the space between them.
    fn finish_structured(mut self) -> (Vec<Block>, bool) {
        if !self.para.is_empty() {
            let para = std::mem::take(&mut self.para);
            self.blocks.push(Block::Paragraph(para));
        }
        (self.blocks, self.broke)
    }
}

#[derive(Default)]
struct Renderer {
    report: RenderReport,
    /// Test-only: the address of every node rendered, to catch one rendered
    /// twice — a node rendered more than once makes nesting exponential.
    #[cfg(test)]
    rendered: std::collections::HashSet<usize>,
    #[cfg(test)]
    rerendered: usize,
}

/// [`render_content`], also counting the nodes rendered more than once.
#[cfg(test)]
fn render_content_counting(nodes: &[DocNode]) -> (Vec<Block>, usize) {
    let mut r = Renderer::default();
    let blocks = r.flow_of(nodes, Flow::default());
    (blocks, r.rerendered)
}

/// Lower-cased element name: tags in the wild come as `<Summary>` and `<B>`
/// often enough that matching them case-sensitively would drop real content
/// into the fallback for no gain.
fn name_of(e: &DocElement) -> String {
    e.name.to_ascii_lowercase()
}

/// Whether `nodes` carry anything a reader would see: an element, or
/// non-whitespace text.
fn has_content(nodes: &[DocNode]) -> bool {
    nodes.iter().any(|n| match n {
        DocNode::Text(t) => !t.trim().is_empty(),
        DocNode::Element(_) => true,
    })
}

impl Renderer {
    fn member(&mut self, member: &DocElement) -> Vec<Block> {
        let mut markers = Flow::default();
        let mut summary = Flow::default();
        let mut loose = Flow::default();
        let mut typeparams: Vec<Vec<Block>> = Vec::new();
        let mut params: Vec<Vec<Block>> = Vec::new();
        let mut returns = Vec::new();
        let mut value = Vec::new();
        let mut exceptions: Vec<Vec<Block>> = Vec::new();
        let mut permissions: Vec<Vec<Block>> = Vec::new();
        let mut remarks = Vec::new();
        let mut examples: Vec<Vec<Block>> = Vec::new();
        let mut see_also: Vec<Vec<Block>> = Vec::new();
        let mut unknown: Vec<Block> = Vec::new();

        for node in &member.children {
            let DocNode::Element(e) = node else {
                self.node(node, &mut loose);
                continue;
            };
            let name = name_of(e);
            // A section (or marker) is lifted out of the prose around it, so
            // the prose either side of it stays two paragraphs: `before
            // <summary>…</summary> after` must not leave `beforeafter`.
            if !is_ignored(&name) && !is_content_tag(&name) {
                loose.flush();
            }
            match name.as_str() {
                "summary" => {
                    summary.flush();
                    self.flow(&e.children, &mut summary);
                    summary.flush();
                }
                "typeparam" => typeparams.push(self.named_item(e, "name")),
                "param" => params.push(self.named_item(e, "name")),
                // Each section tag also matches the misspellings shipped
                // Microsoft packs carry (`<return>`, `<remark>`, `<examples>`,
                // `<inhertidoc>`): their meaning is not in doubt.
                "returns" | "return" => returns.extend(self.flow_of(&e.children, Flow::default())),
                "value" => value.extend(self.flow_of(&e.children, Flow::default())),
                "exception" => exceptions.push(self.cref_item(e)),
                "permission" => permissions.push(self.cref_item(e)),
                "remarks" | "remark" => {
                    remarks.extend(self.flow_of(&e.children, Flow::default()));
                }
                "example" | "examples" => examples.push(self.flow_of(&e.children, Flow::default())),
                // Notes for the library's own developers, which the docs
                // pipeline left in: still documentation, so shown, labelled.
                "devremarks" | "devdoc" => {
                    let label = if name == "devdoc" {
                        "Developer documentation"
                    } else {
                        "Developer remarks"
                    };
                    let content = self.flow_of(&e.children, Flow::default());
                    unknown.extend(labelled(label, content));
                }
                "seealso" => {
                    let mut item = Flow::default();
                    self.reference(e, &mut item);
                    see_also.push(item.finish());
                }
                _ if is_inheritdoc(&name) => {
                    self.report.unresolved_inheritdoc = true;
                    markers.block(Block::Paragraph(inheritdoc_marker(e)));
                }
                "include" => {
                    self.report.unresolved_include = true;
                    markers.block(Block::Paragraph(include_marker(e)));
                }
                _ if is_ignored(&name) => {}
                _ if is_content_tag(&name) => self.node(node, &mut loose),
                _ => {
                    self.report.unknown_tags.push(name.clone());
                    let content = self.flow_of(&e.children, Flow::default());
                    unknown.extend(labelled(&e.name, content));
                }
            }
        }

        let mut out = markers.finish();
        out.extend(summary.finish());
        out.extend(loose.finish());
        out.extend(list_section("Type parameters", typeparams));
        out.extend(list_section("Parameters", params));
        out.extend(labelled("Returns", returns));
        out.extend(labelled("Value", value));
        out.extend(list_section("Exceptions", exceptions));
        out.extend(list_section("Permissions", permissions));
        out.extend(labelled("Remarks", remarks));
        for example in examples {
            out.extend(labelled("Example", example));
        }
        out.extend(list_section("See also", see_also));
        out.extend(unknown);
        out
    }

    /// A `<param name="x">…</param>`-shaped item: `` `x` — … ``.
    fn named_item(&mut self, e: &DocElement, attr: &str) -> Vec<Block> {
        let mut item = Flow::default();
        if let Some(name) = e.attribute(attr) {
            item.inline(Inline::Code(name.to_string()));
            if has_content(&e.children) {
                item.inline(Inline::Text(" — ".to_string()));
            }
        }
        self.flow(&e.children, &mut item);
        item.finish()
    }

    /// An `<exception cref="T:X">…</exception>`-shaped item: `` `X` — … ``.
    fn cref_item(&mut self, e: &DocElement) -> Vec<Block> {
        let mut item = Flow::default();
        if let Some(cref) = e.attribute("cref") {
            item.inline(Inline::Code(readable_cref(cref)));
            if has_content(&e.children) {
                item.inline(Inline::Text(" — ".to_string()));
            }
        }
        self.flow(&e.children, &mut item);
        item.finish()
    }

    fn flow_of(&mut self, nodes: &[DocNode], mut flow: Flow) -> Vec<Block> {
        self.flow(nodes, &mut flow);
        flow.finish()
    }

    fn flow(&mut self, nodes: &[DocNode], flow: &mut Flow) {
        for node in nodes {
            self.node(node, flow);
        }
    }

    fn node(&mut self, node: &DocNode, flow: &mut Flow) {
        #[cfg(test)]
        if !self.rendered.insert(std::ptr::from_ref(node) as usize) {
            self.rerendered += 1;
        }
        let e = match node {
            DocNode::Text(t) => {
                flow.inline(Inline::Text(t.clone()));
                return;
            }
            DocNode::Element(e) => e,
        };
        let name = name_of(e);
        match name.as_str() {
            "see" | "seealso" => self.reference(e, flow),
            "paramref" | "typeparamref" => match e.attribute("name") {
                Some(n) => in_place_of(e, flow, Inline::Code(n.to_string())),
                None => self.inline_content(&e.children).place(flow, Inline::Code),
            },
            "c" | "tt" => self.inline_content(&e.children).place_code(flow),
            "code" => {
                let content = self.inline_content(&e.children);
                // `data-dev-comment-type` marks a `<code>` the docs pipeline
                // rewrote from a `<paramref>`/`<see langword>` — inline by
                // construction. Otherwise a one-line `<code>` is a span too.
                if e.attribute("data-dev-comment-type").is_some() || !content.text.contains('\n') {
                    content.place_code(flow);
                } else {
                    let text = content.text;
                    let language = e
                        .attribute("lang")
                        .or_else(|| e.attribute("language"))
                        .map(str::to_string);
                    flow.block(Block::CodeBlock {
                        language,
                        code: dedent(&text),
                    });
                }
            }
            "pre" => {
                let text = self.inline_text(&e.children);
                flow.block(Block::CodeBlock {
                    language: None,
                    code: dedent(&text),
                });
            }
            "para" | "p" | "div" | "blockquote" => {
                flow.flush();
                self.flow(&e.children, flow);
                flow.flush();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                flow.flush();
                let text = self.inline_text(&e.children);
                flow.inline(Inline::Strong(text));
                flow.flush();
            }
            "table" => {
                let items = self.table_rows(e);
                flow.block(Block::List {
                    ordered: false,
                    items,
                });
            }
            "br" => {
                // `<br/>` is empty; should one carry content, that content is
                // a line of its own.
                flow.inline(Inline::LineBreak);
                if has_content(&e.children) {
                    self.flow(&e.children, flow);
                    flow.inline(Inline::LineBreak);
                }
            }
            "b" | "strong" => self.inline_content(&e.children).place(flow, Inline::Strong),
            "i" | "em" | "u" => self
                .inline_content(&e.children)
                .place(flow, Inline::Emphasis),
            "a" => match e.attribute("href") {
                Some(href) => self
                    .inline_content(&e.children)
                    .place(flow, |text| Inline::Link {
                        text,
                        url: href.to_string(),
                    }),
                None => self.flow(&e.children, flow),
            },
            // A superscript is an exponent or a footnote mark; `^` keeps it
            // one (`10^x`, not `10x`).
            "sup" => {
                flow.inline(Inline::Text("^".to_string()));
                self.flow(&e.children, flow);
            }
            "xref" => match e.attribute("uid") {
                Some(uid) if !has_content(&e.children) => {
                    in_place_of(e, flow, Inline::Code(readable_cref(uid)));
                }
                _ => self.flow(&e.children, flow),
            },
            "list" => {
                let ordered = e.attribute("type").is_some_and(|t| t.trim() == "number");
                let items = self.list_items(e, &["item", "listheader"]);
                flow.block(Block::List { ordered, items });
            }
            "ul" | "ol" => {
                let items = self.list_items(e, &["li"]);
                flow.block(Block::List {
                    ordered: name == "ol",
                    items,
                });
            }
            "note" => {
                flow.flush();
                let label = e.attribute("type").map_or("Note".to_string(), capitalise);
                flow.inline(Inline::Strong(label));
                flow.inline(Inline::Text(": ".to_string()));
                self.flow(&e.children, flow);
                flow.flush();
            }
            _ if is_inheritdoc(&name) => {
                self.report.unresolved_inheritdoc = true;
                for inline in inheritdoc_marker(e) {
                    flow.inline(inline);
                }
            }
            "include" => {
                self.report.unresolved_include = true;
                for inline in include_marker(e) {
                    flow.inline(inline);
                }
            }
            // Wrappers whose content reads as is, inline: a subscript, a format
            // hint, an HTML span, an unspecified `<block>`.
            "sub" | "format" | "span" | "block" => self.flow(&e.children, flow),
            // Structure met where it does not belong — list and table parts
            // outside a list or table, section tags nested inside content — is
            // still structure: its content is a block of its own, never run
            // into its neighbours' words.
            "tr" => {
                flow.flush();
                for block in self.table_row(e) {
                    flow.block(block);
                }
            }
            "item" | "term" | "description" | "listheader" | "li" | "thead" | "tbody" | "tfoot"
            | "td" | "th" | "summary" | "remarks" | "remark" | "returns" | "return" | "value"
            | "example" | "examples" | "param" | "typeparam" | "exception" | "permission"
            | "devremarks" | "devdoc" => {
                flow.flush();
                self.flow(&e.children, flow);
                flow.flush();
            }
            _ if is_ignored(&name) => {}
            _ => {
                self.report.unknown_tags.push(name);
                self.flow(&e.children, flow);
            }
        }
    }

    /// `<see>` / `<seealso>`: a `langword` as code; a `cref` as its readable
    /// name in code, unless the author supplied the words to show; an `href` as
    /// a link.
    fn reference(&mut self, e: &DocElement, flow: &mut Flow) {
        // `langref` is a misspelling of `langword` shipped in ASP.NET Core.
        if let Some(word) = e.attribute("langword").or_else(|| e.attribute("langref")) {
            in_place_of(e, flow, Inline::Code(word.to_string()));
        } else if let Some(href) = e.attribute("href") {
            self.inline_content(&e.children)
                .place(flow, |text| Inline::Link {
                    text,
                    url: href.to_string(),
                });
        } else if has_content(&e.children) {
            // The author's words — and the target too, unless those words
            // already name it (`<see cref="T:System.String">string</see>`):
            // `<see cref="M:…QuicStream.Abort(…)">aborted</see>` would
            // otherwise leave the reader no way to tell what is referred to.
            // The comparison reads the words with `label_text` rather than
            // rendering them a second time (which, nested, is exponential).
            self.flow(&e.children, flow);
            if let Some(cref) = e.attribute("cref") {
                let target = readable_cref(cref);
                let words = label_text(&e.children);
                let words = words.trim();
                if words.is_empty() || !target.to_lowercase().contains(&words.to_lowercase()) {
                    flow.inline(Inline::Text(" (".to_string()));
                    flow.inline(Inline::Code(target));
                    flow.inline(Inline::Text(")".to_string()));
                }
            }
        } else if let Some(cref) = e.attribute("cref") {
            in_place_of(e, flow, Inline::Code(readable_cref(cref)));
        } else if let Some(name) = e.attribute("name").or_else(|| e.attribute("paramref")) {
            // `<see name="Action"/>`, `<see paramref="x"/>`: a name in code.
            in_place_of(e, flow, Inline::Code(name.to_string()));
        } else {
            in_place_of(e, flow, Inline::Text(String::new()));
            // No attribute we know, and no content: nothing to show. Record it,
            // since an attribute we do not know may be what carried the target.
            self.report
                .unknown_tags
                .push(format!("{}(no target)", name_of(e)));
        }
    }

    /// The items of a list: each child element named one of `item_tags`. An
    /// item with `<term>`/`<description>` renders `term — description`; a
    /// header row's cells render bold. Any other child (stray text aside)
    /// becomes an item of its own, so nothing inside the list is lost.
    fn list_items(&mut self, list: &DocElement, item_tags: &[&str]) -> Vec<Vec<Block>> {
        let mut items = Vec::new();
        for child in &list.children {
            match child {
                DocNode::Text(t) if t.trim().is_empty() => {}
                DocNode::Element(e) if item_tags.contains(&name_of(e).as_str()) => {
                    items.push(self.list_item(e, name_of(e) == "listheader"));
                }
                other => {
                    let mut flow = Flow::default();
                    self.node(other, &mut flow);
                    items.push(flow.finish());
                }
            }
        }
        items
    }

    /// An HTML `<table>` as a list of rows, each row's cells joined by ` — `
    /// (header cells bold) — the same shape a `<list type="table">` takes.
    /// Rows may sit directly in the table or in `thead`/`tbody`/`tfoot`; any
    /// other child becomes an item of its own.
    fn table_rows(&mut self, table: &DocElement) -> Vec<Vec<Block>> {
        let mut items = Vec::new();
        let visit = |r: &mut Self, nodes: &[DocNode], items: &mut Vec<Vec<Block>>| {
            for child in nodes {
                match child {
                    DocNode::Text(t) if t.trim().is_empty() => {}
                    DocNode::Element(e) if name_of(e) == "tr" => items.push(r.table_row(e)),
                    other => {
                        let mut flow = Flow::default();
                        r.node(other, &mut flow);
                        items.push(flow.finish());
                    }
                }
            }
        };
        for child in &table.children {
            match child {
                DocNode::Element(e)
                    if matches!(name_of(e).as_str(), "thead" | "tbody" | "tfoot") =>
                {
                    visit(self, &e.children, &mut items);
                }
                other => visit(self, std::slice::from_ref(other), &mut items),
            }
        }
        items
    }

    fn table_row(&mut self, row: &DocElement) -> Vec<Block> {
        self.cells(
            &row.children,
            |e| matches!(name_of(e).as_str(), "td" | "th"),
            |e| name_of(e) == "th",
        )
    }

    /// One list item. An item made of `<term>`/`<description>` cells renders
    /// them in document order joined by ` — ` (header cells bold) — every
    /// cell, since table lists carry as many `<description>` columns as they
    /// like; anything else in the item stays where it is.
    fn list_item(&mut self, item: &DocElement, header: bool) -> Vec<Block> {
        let is_cell = |n: &DocNode| matches!(n, DocNode::Element(e) if matches!(name_of(e).as_str(), "term" | "description"));
        if !item.children.iter().any(is_cell) {
            return self.flow_of(&item.children, Flow::default());
        }
        self.cells(
            &item.children,
            |e| matches!(name_of(e).as_str(), "term" | "description"),
            |_| header,
        )
    }

    /// A row of cells (`is_cell` picks them out) in document order, joined by
    /// ` — `, a cell bold when `bold` says so. Content between cells (stray
    /// text, other elements) keeps its place, separated from the cells on
    /// either side so no two run into one word.
    fn cells(
        &mut self,
        children: &[DocNode],
        is_cell: impl Fn(&DocElement) -> bool,
        bold: impl Fn(&DocElement) -> bool,
    ) -> Vec<Block> {
        let mut flow = Flow::default();
        // What the last visible piece was: nothing yet, a cell, or other content.
        #[derive(PartialEq)]
        enum Last {
            Nothing,
            Cell,
            Other,
        }
        let mut last = Last::Nothing;
        for child in children {
            match child {
                DocNode::Element(e) if is_cell(e) => {
                    if last != Last::Nothing {
                        flow.inline(Inline::Text(" — ".to_string()));
                    }
                    if bold(e) {
                        let text = self.inline_text(&e.children);
                        flow.inline(Inline::Strong(text));
                    } else {
                        self.flow(&e.children, &mut flow);
                    }
                    last = Last::Cell;
                }
                DocNode::Text(t) if t.trim().is_empty() => self.node(child, &mut flow),
                other => {
                    if last == Last::Cell {
                        flow.inline(Inline::Text(" — ".to_string()));
                    }
                    self.node(other, &mut flow);
                    last = Last::Other;
                }
            }
        }
        flow.finish()
    }

    /// The text of `nodes` rendered as content and then flattened — for the
    /// inlines that hold plain text (a code span, emphasis, a link's words):
    /// a reference inside one keeps its name (`<c>10<sup><paramref
    /// name="x"/></sup></c>` is `10^x`), where the raw character data would
    /// drop it. Whitespace is kept as written.
    fn inline_text(&mut self, nodes: &[DocNode]) -> String {
        self.inline_content(nodes).text
    }

    /// As [`Self::inline_text`], also saying whether the content was more than
    /// one paragraph's worth — a list, a code block, several paragraphs —
    /// which [`InlineContent::place`] then keeps apart from its neighbours.
    fn inline_content(&mut self, nodes: &[DocNode]) -> InlineContent {
        let mut flow = Flow::default();
        self.flow(nodes, &mut flow);
        let (blocks, broke) = flow.finish_structured();
        let structured = broke || !matches!(blocks.as_slice(), [] | [Block::Paragraph(_)]);
        let mut text = String::new();
        flatten(&blocks, &mut text);
        InlineContent { text, structured }
    }
}

/// Content flattened into one inline's text; see [`Renderer::inline_content`].
struct InlineContent {
    text: String,
    structured: bool,
}

impl InlineContent {
    /// Put the content into `flow` as a code span. The whitespace at its
    /// edges is the XML's layout (`<c>\n  x\n</c>`), the whitespace inside it
    /// the code's (`"a   b"`): the inside is kept verbatim, the edges become
    /// one space *outside* the span — still a word break, no longer code.
    fn place_code(self, flow: &mut Flow) {
        let text = self.text.clone();
        let inner = text.trim_matches(|c: char| c.is_ascii_whitespace());
        if inner.is_empty() {
            return self.place(flow, Inline::Text);
        }
        let is_space = |c: char| c.is_ascii_whitespace();
        let (lead, trail) = (text.starts_with(is_space), text.ends_with(is_space));
        let inner = inner.to_string();
        let structured = self.structured;
        if structured {
            flow.flush();
        }
        if lead {
            flow.inline(Inline::Text(" ".to_string()));
        }
        flow.inline(Inline::Code(inner));
        if trail {
            flow.inline(Inline::Text(" ".to_string()));
        }
        if structured {
            flow.flush();
        }
    }

    /// Put `make(text)` into `flow` — as a paragraph of its own when the
    /// content was structured, so a table inside `<c>` does not fuse with the
    /// words around the span.
    fn place(self, flow: &mut Flow, make: impl FnOnce(String) -> Inline) {
        if self.structured {
            flow.flush();
            flow.inline(make(self.text));
            flow.flush();
        } else {
            flow.inline(make(self.text));
        }
    }
}

/// The text of `blocks`, blocks and list items on their own lines.
fn flatten(blocks: &[Block], out: &mut String) {
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        match block {
            Block::Paragraph(inlines) => {
                for inline in inlines {
                    match inline {
                        Inline::Text(t)
                        | Inline::Code(t)
                        | Inline::Strong(t)
                        | Inline::Emphasis(t) => out.push_str(t),
                        // A link flattened into plain text keeps where it
                        // led: `<b><a href="u">BYTE</a></b>` is `BYTE (u)`.
                        Inline::Link { text, url } if text.trim().is_empty() || text == url => {
                            out.push_str(url);
                        }
                        Inline::Link { text, url } => {
                            out.push_str(text);
                            out.push_str(" (");
                            out.push_str(url);
                            out.push(')');
                        }
                        Inline::LineBreak => out.push('\n'),
                    }
                }
            }
            Block::CodeBlock { code, .. } => out.push_str(code),
            Block::List { items, .. } => {
                for (j, item) in items.iter().enumerate() {
                    if j > 0 {
                        out.push('\n');
                    }
                    flatten(item, out);
                }
            }
        }
    }
}

/// The words a reference's content shows, read without rendering it: the
/// character data, plus the names a nested reference draws from an attribute
/// (`<see cref="M:T.Dispose"><paramref name="resource"/></see>` is labelled
/// `resource`). Used only to decide whether those words already name the
/// target; an empty label names nothing.
fn label_text(nodes: &[DocNode]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            DocNode::Text(t) => out.push_str(t),
            DocNode::Element(e) => {
                let name = name_of(e);
                let borne = match name.as_str() {
                    "paramref" | "typeparamref" => e.attribute("name"),
                    "see" | "seealso" => e.attribute("langword").or_else(|| e.attribute("langref")),
                    _ => None,
                };
                match borne {
                    Some(word) => out.push_str(word),
                    None if is_ignored(&name) => {}
                    None => out.push_str(&label_text(&e.children)),
                }
            }
        }
    }
    out
}

/// Put `inline` — drawn from `e`'s attributes — into `flow` in place of `e`,
/// keeping any whitespace at the edges of `e`'s own content as the word break
/// it is: `a<see langword="null"> </see>b` reads `a null b`, not `anullb`.
fn in_place_of(e: &DocElement, flow: &mut Flow, inline: Inline) {
    let content = e.text_content();
    let is_space = |c: char| c.is_ascii_whitespace();
    if content.starts_with(is_space) {
        flow.inline(Inline::Text(" ".to_string()));
    }
    flow.inline(inline);
    if content.ends_with(is_space) {
        flow.inline(Inline::Text(" ".to_string()));
    }
}

/// `<inheritdoc>`, and the misspellings of it shipped packages carry.
fn is_inheritdoc(name: &str) -> bool {
    matches!(
        name,
        "inheritdoc" | "inhertidoc" | "inheritdocs" | "inheriteddoc"
    )
}

/// The tags [`Renderer::member`] treats as sections of a member (or as the
/// member-level markers), rather than as content — for the property that holds
/// member-level and content-level rendering of everything else together. (A
/// section missing here fails that property; it cannot hide one.)
#[cfg(test)]
fn is_member_section(name: &str) -> bool {
    matches!(
        name,
        "summary"
            | "typeparam"
            | "param"
            | "returns"
            | "return"
            | "value"
            | "exception"
            | "permission"
            | "remarks"
            | "remark"
            | "example"
            | "examples"
            | "seealso"
            | "devremarks"
            | "devdoc"
            | "include"
    ) || is_inheritdoc(name)
}

/// Element names that are documentation *content* rather than a section, so
/// that at member level they join the loose content after the summary — every
/// tag [`Renderer::node`] maps explicitly that is not a member section (the
/// `a_member_without_sections_renders_as_its_content` property holds the two
/// lists together).
fn is_content_tag(name: &str) -> bool {
    matches!(
        name,
        "span"
            | "block"
            | "tr"
            | "item"
            | "term"
            | "description"
            | "listheader"
            | "li"
            | "thead"
            | "tbody"
            | "tfoot"
            | "td"
            | "th"
            | "see"
            | "paramref"
            | "typeparamref"
            | "c"
            | "tt"
            | "code"
            | "pre"
            | "para"
            | "p"
            | "br"
            | "b"
            | "strong"
            | "i"
            | "em"
            | "u"
            | "a"
            | "xref"
            | "list"
            | "ul"
            | "ol"
            | "note"
            | "sup"
            | "sub"
            | "format"
            | "table"
            | "div"
            | "blockquote"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
    )
}

fn capitalise(word: &str) -> String {
    let word = word.trim();
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Note".to_string(),
    }
}

/// The in-place marker for an unresolved `<inheritdoc>`.
fn inheritdoc_marker(e: &DocElement) -> Vec<Inline> {
    let mut out = vec![Inline::Text("(Documentation inherited".to_string())];
    if let Some(cref) = e.attribute("cref") {
        out.push(Inline::Text(" from ".to_string()));
        out.push(Inline::Code(readable_cref(cref)));
    }
    out.push(Inline::Text(" is not shown.)".to_string()));
    out
}

/// The in-place marker for an unresolved `<include>`.
fn include_marker(e: &DocElement) -> Vec<Inline> {
    let mut out = vec![Inline::Text("(Documentation included".to_string())];
    if let Some(file) = e.attribute("file") {
        out.push(Inline::Text(" from ".to_string()));
        out.push(Inline::Code(file.to_string()));
    }
    out.push(Inline::Text(" is not shown.)".to_string()));
    out
}

/// `label` over `content`: inline (`**Returns**: …`) when the content opens
/// with a paragraph, as a heading line otherwise. Nothing for empty content.
fn labelled(label: &str, mut content: Vec<Block>) -> Vec<Block> {
    if content.is_empty() {
        return content;
    }
    let prefix = [
        Inline::Strong(label.to_string()),
        Inline::Text(": ".to_string()),
    ];
    match content.first_mut() {
        Some(Block::Paragraph(inlines)) => {
            inlines.splice(0..0, prefix);
            content
        }
        _ => {
            let mut out = vec![Block::Paragraph(vec![Inline::Strong(label.to_string())])];
            out.extend(content);
            out
        }
    }
}

/// `label` over a list of `items`. Nothing when there are none.
fn list_section(label: &str, items: Vec<Vec<Block>>) -> Vec<Block> {
    if items.is_empty() {
        return Vec::new();
    }
    vec![
        Block::Paragraph(vec![Inline::Strong(label.to_string())]),
        Block::List {
            ordered: false,
            items,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml_doc::markdown::oracle::{parse_back, text_and_urls_of, text_of};
    use crate::xml_doc::markdown::{normalize, to_markdown};
    use proptest::prelude::*;

    fn parse_member(xml: &str) -> DocElement {
        let doc = roxmltree::Document::parse(xml).expect("test XML parses");
        DocElement::from_roxmltree(doc.root_element()).expect("shallow")
    }

    fn markdown_of(xml: &str) -> String {
        to_markdown(&render_member(&parse_member(xml)).0)
    }

    /// The real `List.map` entry from FSharp.Core.xml, verbatim.
    #[test]
    fn renders_list_map_completely() {
        let xml = r#"<member name="M:Microsoft.FSharp.Collections.ListModule.Map``2(Microsoft.FSharp.Core.FSharpFunc{``0,``1},Microsoft.FSharp.Collections.FSharpList{``0})">
 <summary>Builds a new collection whose elements are the results of applying the given function
 to each of the elements of the collection.</summary>

 <param name="mapping">The function to transform elements from the input list.</param>
 <param name="list">The input list.</param>

 <returns>The list of transformed elements.</returns>

 <example id="map-1">
 <code lang="fsharp">
 let inputs = [ "a"; "bbb"; "cc" ]

 inputs |> List.map (fun x -> x.Length)
 </code>
 Evaluates to <c>[ 1; 3; 2 ]</c>
 </example>

 <remarks>This is an O(n) operation, where n is the length of the list.</remarks>
</member>"#;
        assert_eq!(
            markdown_of(xml),
            "Builds a new collection whose elements are the results of applying the given \
             function to each of the elements of the collection.\n\n\
             **Parameters**\n\n\
             - `mapping` — The function to transform elements from the input list.\n\
             - `list` — The input list.\n\n\
             **Returns**: The list of transformed elements.\n\n\
             **Remarks**: This is an O(n) operation, where n is the length of the list.\n\n\
             **Example**\n\n\
             ```fsharp\n\
             let inputs = [ \"a\"; \"bbb\"; \"cc\" ]\n\
             \n\
             inputs |> List.map (fun x -> x.Length)\n\
             ```\n\n\
             Evaluates to `[ 1; 3; 2 ]`"
        );
    }

    /// A BCL-shaped entry: `see cref`/`langword`, `paramref`, exceptions, a
    /// table list with a header row.
    #[test]
    fn renders_bcl_markup() {
        let xml = r#"<member name="M:X.F(System.String)">
      <summary>Converts <paramref name="s" /> to a <see cref="T:System.Int32" />, or returns <see langword="null" />.</summary>
      <param name="s">The text.</param>
      <returns>The <see cref="T:System.Collections.Generic.List`1">list</see>.</returns>
      <exception cref="T:System.ArgumentNullException">
        <paramref name="s" /> is <see langword="null" />.</exception>
      <remarks><list type="table"><listheader><term>Value</term><description>Meaning</description></listheader>
      <item><term>0</term><description>Zero.</description></item></list></remarks>
    </member>"#;
        assert_eq!(
            markdown_of(xml),
            "Converts `s` to a `System.Int32`, or returns `null`.\n\n\
             **Parameters**\n\n\
             - `s` — The text.\n\n\
             **Returns**: The list.\n\n\
             **Exceptions**\n\n\
             - `System.ArgumentNullException` — `s` is `null`.\n\n\
             **Remarks**\n\n\
             - **Value** — **Meaning**\n\
             - 0 — Zero."
        );
    }

    /// HTML tables (seen in NuGet packages' docs) read as rows, not as their
    /// cells' text run together.
    #[test]
    fn renders_an_html_table_as_rows() {
        let xml = r#"<member name="T:X"><remarks><table><thead><tr><th>Key</th><th>Use</th></tr></thead>
            <tbody><tr><td>a</td><td>first</td></tr><tr><td>b</td><td>second</td></tr></tbody></table></remarks></member>"#;
        assert_eq!(
            markdown_of(xml),
            "**Remarks**\n\n- **Key** — **Use**\n- a — first\n- b — second"
        );
    }

    /// Shipped shapes a review found losing content: a reference inside
    /// inline code (`Double.Exp10`), a reference inside emphasis, significant
    /// spaces in a code span (FSharp.Core `String.collect`), and a table row
    /// with several descriptions (`DbProviderFactories.GetFactoryClasses`).
    #[test]
    fn content_nested_in_inline_markup_survives() {
        assert_eq!(
            markdown_of(
                r#"<member name="M:X"><returns><c>10<sup><paramref name="x"/></sup></c></returns></member>"#
            ),
            "**Returns**: `10^x`"
        );
        assert_eq!(
            markdown_of(
                r#"<member name="M:X"><summary>See <b><see cref="M:Foo.Bar"/></b>.</summary></member>"#
            ),
            "See **Foo.Bar**."
        );
        assert_eq!(
            markdown_of(
                r#"<member name="M:X"><summary>Gives <c>"S t e f a n   s a y s :   H i ! "</c></summary></member>"#
            ),
            "Gives `\"S t e f a n   s a y s :   H i ! \"`"
        );
        assert_eq!(
            markdown_of(
                r#"<member name="M:X"><remarks><list type="table"><item><term>Name</term><description>A</description><description>B</description></item></list></remarks></member>"#
            ),
            "**Remarks**\n\n- Name — A — B"
        );
    }

    /// Section tags met inside content are blocks: their texts stay apart.
    #[test]
    fn nested_sections_do_not_run_together() {
        assert_eq!(
            markdown_of(
                r#"<member name="T:X"><summary><examples><example>A</example><example>B</example></examples></summary></member>"#
            ),
            "A\n\nB"
        );
        assert_eq!(
            markdown_of(
                r#"<member name="T:X"><summary><tr>x<td>a</td>y<td>b</td></tr></summary></member>"#
            ),
            "x — a — y — b"
        );
    }

    /// Block structure inside inline markup — found by `sibling_blocks_never_fuse`
    /// — keeps the span apart from its neighbours rather than fusing words.
    #[test]
    fn blocks_inside_inline_markup_stay_apart() {
        let summary = |inner: &str| {
            markdown_of(&format!(
                r#"<member name="T:X"><summary>{inner}</summary></member>"#
            ))
        };
        assert_eq!(
            summary("a<paramref><table><tr><td>b</td></tr></table></paramref>"),
            "a\n\n`b`"
        );
        assert_eq!(
            summary("a<paramref><summary>b</summary></paramref>"),
            "a\n\n`b`"
        );
        assert_eq!(summary("<br>a</br>b"), "a\\\nb");
        assert_eq!(summary("<c><br>a</br></c><sup>b</sup>"), "`a`\n\n^b");
    }

    /// Found by review: a reference labelled only by an attribute keeps its
    /// target, and loose prose either side of a section stays two paragraphs.
    #[test]
    fn attribute_labels_and_sections_between_prose() {
        assert_eq!(
            markdown_of(
                r#"<member name="T:X"><summary>Frees <see cref="M:MyType.Dispose"><paramref name="resource"/></see>.</summary></member>"#
            ),
            "Frees `resource` (`MyType.Dispose`)."
        );
        assert_eq!(
            markdown_of(r#"<member name="T:X">before<summary>Summary.</summary>after</member>"#),
            "Summary.\n\nbefore\n\nafter"
        );
    }

    /// Found by review: a link inside formatting keeps its destination, and
    /// a transparent wrapper at member level stays in the sentence.
    #[test]
    fn flattened_links_and_member_level_wrappers_keep_their_content() {
        assert_eq!(
            markdown_of(
                r#"<member name="T:X"><summary><b><a href="https://x.org/b">BYTE</a></b></summary></member>"#
            ),
            // Bold cannot be spelled around text ending in `)`; the words and
            // the destination are what matter.
            "BYTE (https://x.org/b)"
        );
        assert_eq!(
            markdown_of(r#"<member name="T:X">Use <span>this</span> value.</member>"#),
            "Use this value."
        );
    }

    /// Found by review: whitespace-only emphasis still separates words, and
    /// a code span's edge whitespace still separates it from its neighbours.
    #[test]
    fn whitespace_in_inline_markup_still_separates_words() {
        let summary = |inner: &str| {
            markdown_of(&format!(
                r#"<member name="T:X"><summary>{inner}</summary></member>"#
            ))
        };
        assert_eq!(summary("first<b> </b>second"), "first second");
        assert_eq!(summary("a<c> b </c>c"), "a `b` c");
    }

    /// Found by review: nested references rendered their content twice per
    /// level. Twenty-five levels must render instantly, and reach the reader.
    #[test]
    fn nested_references_render_once() {
        let depth = 25;
        let xml = format!(
            r#"<member name="T:X"><summary>{}x{}</summary></member>"#,
            r#"<see cref="T:A">"#.repeat(depth),
            "</see>".repeat(depth)
        );
        let started = std::time::Instant::now();
        assert!(markdown_of(&xml).starts_with('x'));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    /// A `cref` whose shown words do not name its target keeps the target
    /// (real NETCore/ASP.NET shapes); words that do name it stand alone.
    #[test]
    fn a_reference_shows_its_target_unless_its_words_name_it() {
        assert_eq!(
            markdown_of(
                r#"<member name="T:X"><summary>Limits <see cref="T:System.Net.Quic.QuicConnection">Quic connections</see>.</summary></member>"#
            ),
            "Limits Quic connections (`System.Net.Quic.QuicConnection`)."
        );
        assert_eq!(
            markdown_of(
                r#"<member name="T:X"><summary>A <see cref="T:System.String">string</see>.</summary></member>"#
            ),
            "A string."
        );
    }

    /// Shrunk inputs once found by the property tests below (kept as examples:
    /// a saved seed reproduces only while the strategy is byte-identical).
    #[test]
    fn found_by_the_properties() {
        let (blocks, _) = render_content(&[DocNode::Element(element(
            "summary",
            Vec::new(),
            vec![DocNode::Element(element(
                "c",
                Vec::new(),
                vec![DocNode::Text("\\".into())],
            ))],
        ))]);
        assert_eq!(text_of(&parse_back(&to_markdown(&blocks))).trim(), "\\");
        let (blocks, _) = render_content(&[DocNode::Element(element(
            "frob",
            Vec::new(),
            vec![DocNode::Text("0".into())],
        ))]);
        assert_eq!(text_of(&parse_back(&to_markdown(&blocks))).trim(), "0");
    }

    #[test]
    fn an_unresolved_inheritdoc_is_marked_not_hidden() {
        let (blocks, report) = render_member(&parse_member(
            r#"<member name="M:X.F"><inheritdoc cref="M:Y.F"/><remarks>More.</remarks></member>"#,
        ));
        assert!(report.unresolved_inheritdoc);
        assert_eq!(
            to_markdown(&blocks),
            "(Documentation inherited from `Y.F` is not shown.)\n\n**Remarks**: More."
        );
    }

    /// An unexpanded `<include>` means the content is elsewhere and
    /// unavailable; like `<inheritdoc>`, it is marked, not silently dropped.
    #[test]
    fn an_unexpanded_include_is_marked_not_hidden() {
        let (blocks, report) = render_member(&parse_member(
            r#"<member name="T:X"><include file='Doc.xml' path='docs/members[@name="X"]/*'/></member>"#,
        ));
        assert!(report.unresolved_include);
        assert_eq!(
            to_markdown(&blocks),
            "(Documentation included from `Doc.xml` is not shown.)"
        );
    }

    /// Shapes from shipped files: text directly under `<member>`, CDATA (which
    /// arrives as text), an empty `<member/>`, an unprefixed `cref` (fsc does
    /// not resolve crefs), a `langword` that is no C# keyword.
    #[test]
    fn renders_loose_text_and_unusual_references() {
        assert_eq!(
            markdown_of(
                r#"<member name="T:X">Loose <![CDATA[a < b]]> text, see <see cref="System.ArgumentException"/> and <see langword="Nothing"/>.</member>"#
            ),
            "Loose a \\< b text, see `System.ArgumentException` and `Nothing`."
        );
        assert_eq!(markdown_of(r#"<member name="T:X"/>"#), "");
    }

    #[test]
    fn unknown_tags_fall_back_to_their_content_and_are_reported() {
        let (blocks, report) = render_member(&parse_member(
            r#"<member name="T:X"><summary>A <frob>b</frob> c</summary><frobnote>internal</frobnote><exclude/></member>"#,
        ));
        assert_eq!(report.unknown_tags, ["frob", "frobnote"]);
        assert_eq!(to_markdown(&blocks), "A b c\n\n**frobnote**: internal");
    }

    #[test]
    fn readable_cref_strips_the_kind_and_restores_angle_brackets() {
        assert_eq!(readable_cref("T:System.String"), "System.String");
        assert_eq!(
            readable_cref(
                "M:System.Linq.Enumerable.Select``2(System.Collections.Generic.IEnumerable{``0})"
            ),
            "System.Linq.Enumerable.Select``2(System.Collections.Generic.IEnumerable<``0>)"
        );
        assert_eq!(readable_cref("!:Missing"), "Missing");
        assert_eq!(
            readable_cref("Overload:System.Object.Equals"),
            "System.Object.Equals"
        );
        assert_eq!(readable_cref("System.String"), "System.String");
    }

    #[test]
    fn dedent_removes_common_indentation_only() {
        assert_eq!(dedent("\n    a\n      b\n\n    c\n  "), "a\n  b\n\nc");
        // A tab and spaces are different indentation.
        assert_eq!(dedent("\ta\n    b"), "\ta\n    b");
    }

    // -- generators -----------------------------------------------------------

    /// Every tag the renderer maps explicitly (aliases included), so each
    /// property reaches every arm.
    const KNOWN: &[&str] = &[
        "summary",
        "remarks",
        "remark",
        "returns",
        "return",
        "value",
        "example",
        "examples",
        "param",
        "typeparam",
        "exception",
        "permission",
        "seealso",
        "devremarks",
        "devdoc",
        "see",
        "paramref",
        "typeparamref",
        "c",
        "tt",
        "code",
        "pre",
        "para",
        "p",
        "br",
        "b",
        "strong",
        "i",
        "em",
        "u",
        "a",
        "xref",
        "list",
        "ul",
        "ol",
        "li",
        "item",
        "listheader",
        "term",
        "description",
        "note",
        "sup",
        "sub",
        "format",
        "span",
        "block",
        "inheritdoc",
        "inhertidoc",
        "inheritdocs",
        "inheriteddoc",
        "include",
        "table",
        "thead",
        "tbody",
        "tfoot",
        "tr",
        "td",
        "th",
        "div",
        "blockquote",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
    ];
    const IGNORED: &[&str] = &[
        "filterpriority",
        "exclude",
        "category",
        "namespacedoc",
        "example-tbd",
        "script",
        "member",
    ];
    const UNKNOWN: &[&str] = &[
        "frob", "frobdoc", "Summary", "B", "PARA", "remarkz", "Target",
    ];
    const ATTRS: &[&str] = &[
        "name",
        "cref",
        "langword",
        "href",
        "type",
        "lang",
        "language",
        "uid",
        "data-dev-comment-type",
        "file",
        "path",
        "id",
    ];

    fn text() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                3 => "[a-zA-Z0-9]{1,5}",
                2 => proptest::sample::select(vec![" ", "\n", "\n    ", "\t", "  "])
                    .prop_map(str::to_string),
                2 => proptest::sample::select(
                    "\\`*_[]<>&|~#+-=.)(!:{}\"'".chars().collect::<Vec<_>>()
                ).prop_map(|c| c.to_string()),
                1 => proptest::sample::select(vec!["1. ", "- ", "# ", "```", "**x**"])
                    .prop_map(str::to_string),
            ],
            0..6,
        )
        .prop_map(|p| p.concat())
    }

    /// Attribute values: generated text, and the shapes shipped files carry —
    /// crefs with every prefix (and none), `langword`s that are not C#
    /// keywords, `code` languages, `list` types, URLs.
    fn attr_value() -> impl Strategy<Value = String> {
        prop_oneof![
            2 => text(),
            3 => proptest::sample::select(vec![
                "T:System.String", "M:System.String.Join(System.String,System.String[])",
                "System.ArgumentException", "!:ReadOnlySpan<T>", "!:global::System.Int32",
                "O:System.Object.Equals", "Overload:X.Y", "m:a.b", "t:A", "N:System",
                "T:Microsoft.FSharp.Collections.list`1", "M:X.<G>$1.M(System.Int32)",
                "null", "Nothing", "<Transform>", "xml:space", ".Rest.Item1",
                "fsharp", "csharp", "c#", "table", "bullet", "number", "bullet|number|table",
                "https://learn.microsoft.com/dotnet/api/system.string", "map-1", "",
            ]).prop_map(str::to_string),
        ]
    }

    fn tree(names: Vec<&'static str>, with_attrs: bool) -> impl Strategy<Value = DocNode> {
        let attrs = if with_attrs {
            proptest::collection::vec((proptest::sample::select(ATTRS), attr_value()), 0..3).boxed()
        } else {
            Just(Vec::new()).boxed()
        };
        let leaf = prop_oneof![
            3 => text().prop_map(DocNode::Text),
            1 => (proptest::sample::select(names.clone()), attrs.clone()).prop_map(|(n, a)| {
                DocNode::Element(element(n, a, Vec::new()))
            }),
        ];
        leaf.prop_recursive(5, 48, 5, move |inner| {
            (
                proptest::sample::select(names.clone()),
                attrs.clone(),
                proptest::collection::vec(inner, 0..5),
            )
                .prop_map(|(n, a, children)| DocNode::Element(element(n, a, children)))
        })
    }

    fn element(name: &str, attrs: Vec<(&str, String)>, children: Vec<DocNode>) -> DocElement {
        let mut attributes: Vec<(String, String)> = Vec::new();
        for (k, v) in attrs {
            if !attributes.iter().any(|(e, _)| e == k) {
                attributes.push((k.to_string(), v));
            }
        }
        DocElement::new(name, attributes, children)
    }

    fn all_names() -> Vec<&'static str> {
        KNOWN
            .iter()
            .chain(IGNORED)
            .chain(UNKNOWN)
            .copied()
            .collect()
    }

    fn visible(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// The words a reader must be shown for `nodes`, in document order,
    /// written independently of the renderer from the tag vocabulary's
    /// meaning: all character data, plus the names a reference carries in an
    /// attribute rather than as content — and nothing from a tag that is
    /// metadata or a marker for content that is not available.
    fn expected_text(nodes: &[DocNode]) -> String {
        let mut out = String::new();
        for node in nodes {
            let e = match node {
                DocNode::Text(t) => {
                    out.push_str(t);
                    continue;
                }
                DocNode::Element(e) => e,
            };
            let name = e.name.to_ascii_lowercase();
            let attr = |a: &str| e.attribute(a);
            let content = || expected_text(&e.children);
            let has_content = e.children.iter().any(|c| match c {
                DocNode::Text(t) => !t.trim().is_empty(),
                DocNode::Element(_) => true,
            });
            match name.as_str() {
                n if is_ignored(n) || is_inheritdoc(n) || n == "include" => {}
                "see" | "seealso" => {
                    if let Some(w) = attr("langword").or_else(|| attr("langref")) {
                        out.push_str(w);
                    } else if let Some(href) = attr("href") {
                        // The words, and the destination they lead to.
                        out.push_str(&content());
                        out.push_str(href);
                    } else if has_content {
                        // The words shown, and the target unless those words
                        // already name it: a reader must be able to tell what
                        // `<see cref="M:…Abort">aborted</see>` points at.
                        out.push_str(&content());
                        if let Some(c) = attr("cref") {
                            let target = readable_cref(c);
                            // The words as a reader sees them — including a
                            // name drawn from an attribute — and they must say
                            // something: an empty label names nothing.
                            let words = expected_text(&e.children);
                            let words = words.trim();
                            if words.is_empty()
                                || !target.to_lowercase().contains(&words.to_lowercase())
                            {
                                out.push_str(&target);
                            }
                        }
                    } else if let Some(c) = attr("cref") {
                        out.push_str(&readable_cref(c));
                    } else if let Some(n) = attr("name").or_else(|| attr("paramref")) {
                        out.push_str(n);
                    }
                }
                "paramref" | "typeparamref" => match attr("name") {
                    Some(n) => out.push_str(n),
                    None => out.push_str(&content()),
                },
                "xref" => match attr("uid") {
                    Some(u) if !has_content => out.push_str(&readable_cref(u)),
                    _ => out.push_str(&content()),
                },
                "a" => {
                    out.push_str(&content());
                    if let Some(href) = attr("href") {
                        out.push_str(href);
                    }
                }
                _ => out.push_str(&content()),
            }
        }
        out
    }

    /// Tags whose content must stay apart from its neighbours' — paragraph
    /// and section structure, list and table parts, line breaks — written
    /// from the vocabulary's meaning, not from the renderer. (Not `code`:
    /// whether it is a block depends on its content.)
    fn is_block_tag(name: &str) -> bool {
        matches!(
            name,
            "para"
                | "p"
                | "div"
                | "blockquote"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "table"
                | "thead"
                | "tbody"
                | "tfoot"
                | "tr"
                | "td"
                | "th"
                | "list"
                | "item"
                | "listheader"
                | "term"
                | "description"
                | "ul"
                | "ol"
                | "li"
                | "note"
                | "pre"
                | "br"
                | "summary"
                | "remarks"
                | "remark"
                | "returns"
                | "return"
                | "value"
                | "example"
                | "examples"
                | "param"
                | "typeparam"
                | "exception"
                | "permission"
                | "devremarks"
                | "devdoc"
        )
    }

    /// Replace every text leaf with a unique token `§n§` (a whitespace-only
    /// leaf with one space), and record each token's *context*: a counter
    /// bumped on entering and leaving each block-level element that holds any
    /// text, and at each whitespace leaf, so two tokens share a context
    /// exactly when neither a block boundary nor whitespace separates them —
    /// when nothing entitles them to be one word. Tokens inside a tag whose content is
    /// not rendered are not recorded.
    fn tokenise(
        nodes: &mut [DocNode],
        next: &mut usize,
        ctx: &mut usize,
        out: &mut Vec<usize>,
        live: bool,
    ) {
        for node in nodes {
            match node {
                // Whitespace stays whitespace (a word break the reader must
                // see), and bumps the context like a block boundary does.
                DocNode::Text(t) if !t.is_empty() && t.trim().is_empty() => {
                    *t = " ".to_string();
                    if live {
                        *ctx += 1;
                    }
                }
                DocNode::Text(t) => {
                    *t = format!("§{}§", *next);
                    if live {
                        out.push(*ctx);
                    } else {
                        out.push(usize::MAX);
                    }
                    *next += 1;
                }
                DocNode::Element(e) => {
                    let name = e.name.to_ascii_lowercase();
                    // Content not rendered: a tag that is metadata or a
                    // marker, and an element whose meaning an attribute
                    // carries (`<see langword>`, `<paramref name>`), whose
                    // children are not its content.
                    let attribute_borne = match name.as_str() {
                        "see" | "seealso" => {
                            e.attribute("langword").or(e.attribute("langref")).is_some()
                        }
                        "paramref" | "typeparamref" => e.attribute("name").is_some(),
                        _ => false,
                    };
                    let live = live
                        && !(is_ignored(&name)
                            || is_inheritdoc(&name)
                            || name == "include"
                            || attribute_borne);
                    // A block with no text in it has nothing to keep apart.
                    let block = live && is_block_tag(&name) && !e.text_content().is_empty();
                    if block {
                        *ctx += 1;
                    }
                    tokenise(&mut e.children, next, ctx, out, live);
                    if block {
                        *ctx += 1;
                    }
                }
            }
        }
    }

    /// Panic if a word of `shown` holds `§n§` tokens from two `contexts`.
    fn assert_no_fusion(shown: &str, contexts: &[usize], printed: &str) {
        for word in shown.split_whitespace() {
            // Only `§n§` is a token: an attribute's rendered value may be a
            // bare number.
            let mut tokens: Vec<usize> = Vec::new();
            let mut rest = word;
            while let Some(start) = rest.find('§') {
                let after = &rest[start + '§'.len_utf8()..];
                let Some(end) = after.find('§') else { break };
                match after[..end].parse::<usize>() {
                    Ok(n) => {
                        tokens.push(n);
                        rest = &after[end + '§'.len_utf8()..];
                    }
                    Err(_) => rest = after,
                }
            }
            let mut ctxs: Vec<usize> = tokens.iter().map(|&t| contexts[t]).collect();
            ctxs.dedup();
            assert!(
                ctxs.len() <= 1,
                "{word:?} joins separated text in:\n{printed}"
            );
        }
    }

    /// Whether `needle` is a subsequence of `hay`.
    fn is_subsequence(needle: &str, hay: &str) -> bool {
        let mut hay = hay.chars();
        needle.chars().all(|c| hay.any(|h| h == c))
    }

    proptest! {
        /// Nothing an author wrote is lost: over arbitrary content trees — every
        /// tag, with attributes — every visible character [`expected_text`]
        /// says a reader must see reaches the rendered Markdown, in order (the
        /// renderer may add labels and separators around it).
        #[test]
        fn no_content_is_lost_in_order(
            nodes in proptest::collection::vec(tree(all_names(), true), 0..6)
        ) {
            let (blocks, _) = render_content(&nodes);
            let printed = to_markdown(&blocks);
            let shown = visible(&text_and_urls_of(&parse_back(&printed)));
            let wanted = visible(&expected_text(&nodes));
            prop_assert!(
                is_subsequence(&wanted, &shown),
                "wanted (in order) {:?}\nshown {:?}\nprinted:\n{}", wanted, shown, printed
            );
        }

        /// Text on either side of a block boundary or of whitespace never
        /// runs together into one word: `<examples><example>A</example>
        /// <example>B</example>` must not read `AB`, nor `first<b> </b>second`
        /// `firstsecond`. Every text leaf becomes a unique token, and no word
        /// of the rendered text may hold tokens from two contexts.
        #[test]
        fn separated_text_never_fuses(
            mut nodes in proptest::collection::vec(tree(all_names(), true), 0..6)
        ) {
            let mut contexts = Vec::new();
            tokenise(&mut nodes, &mut 0, &mut 0, &mut contexts, true);
            let (blocks, _) = render_content(&nodes);
            let printed = to_markdown(&blocks);
            let shown = text_and_urls_of(&parse_back(&printed));
            assert_no_fusion(&shown, &contexts, &printed);
        }

        /// The same at member level, where sections are pulled out of the
        /// loose prose: `before<summary>S</summary>after` must not leave
        /// `beforeafter` behind.
        #[test]
        fn separated_text_never_fuses_at_member_level(
            mut children in proptest::collection::vec(tree(all_names(), true), 0..6)
        ) {
            let mut contexts = Vec::new();
            let (mut next, mut ctx) = (0, 0);
            for child in &mut children {
                // A top-level element that is not content is a section (or a
                // marker, or an unknown section): a boundary of its own.
                let section = matches!(child, DocNode::Element(e)
                    if !is_content_tag(&e.name.to_ascii_lowercase())
                        && !is_ignored(&e.name.to_ascii_lowercase()));
                ctx += usize::from(section);
                tokenise(std::slice::from_mut(child), &mut next, &mut ctx, &mut contexts, true);
                ctx += usize::from(section);
            }
            let member = element("member", Vec::new(), children);
            let (blocks, _) = render_member(&member);
            let printed = to_markdown(&blocks);
            let shown = text_and_urls_of(&parse_back(&printed));
            assert_no_fusion(&shown, &contexts, &printed);
        }

        /// At member level, anything that is not a section reads exactly as
        /// it would as content: the member-level dispatch cannot drift from
        /// the content-level one (`<member>Use <span>this</span> value.`
        /// must not split `this` off into a section of its own).
        #[test]
        fn a_member_without_sections_renders_as_its_content(
            children in proptest::collection::vec(
                prop_oneof![
                    text().prop_map(DocNode::Text),
                    (
                        proptest::sample::select(
                            KNOWN.iter().chain(IGNORED).copied()
                                .filter(|n| !is_member_section(n))
                                .collect::<Vec<_>>(),
                        ),
                        proptest::collection::vec((proptest::sample::select(ATTRS), attr_value()), 0..3),
                        proptest::collection::vec(tree(all_names(), true), 0..4),
                    )
                        .prop_map(|(n, a, c)| DocNode::Element(element(n, a, c))),
                ],
                0..6,
            )
        ) {
            let member = element("member", Vec::new(), children.clone());
            prop_assert_eq!(render_member(&member).0, render_content(&children).0);
        }

        /// Every node is rendered exactly once — rendering one twice (to
        /// measure it, then to show it) makes nested markup exponential.
        #[test]
        fn every_node_is_rendered_at_most_once(
            nodes in proptest::collection::vec(tree(all_names(), true), 0..6)
        ) {
            let (_, rerendered) = render_content_counting(&nodes);
            prop_assert_eq!(rerendered, 0);
        }

        /// Member level reorders sections, so the check there is a multiset
        /// one: every visible character, as many times as the source has it.
        #[test]
        fn no_member_content_is_lost(
            children in proptest::collection::vec(tree(all_names(), true), 0..6)
        ) {
            let member = element("member", Vec::new(), children);
            let (blocks, _) = render_member(&member);
            let printed = to_markdown(&blocks);
            let count = |s: &str| {
                let mut m = std::collections::BTreeMap::<char, usize>::new();
                for c in visible(s).chars() { *m.entry(c).or_default() += 1; }
                m
            };
            let shown = count(&text_and_urls_of(&parse_back(&printed)));
            for (c, n) in count(&expected_text(&member.children)) {
                prop_assert!(
                    shown.get(&c).copied().unwrap_or(0) >= n,
                    "{:?} appears {} times in the source but {} in:\n{}",
                    c, n, shown.get(&c).copied().unwrap_or(0), printed
                );
            }
        }

        /// Total over arbitrary trees, and what it prints reads back as exactly
        /// what it built — no doc text becomes Markdown structure.
        #[test]
        fn rendering_any_member_is_total_and_reads_back_faithfully(
            children in proptest::collection::vec(tree(all_names(), true), 0..6)
        ) {
            let member = element("member", Vec::new(), children);
            let (blocks, _) = render_member(&member);
            let printed = to_markdown(&blocks);
            prop_assert_eq!(parse_back(&printed), normalize(blocks), "printed:\n{}", printed);
        }

        /// The Markdown oracle recovers exactly the document's text: for
        /// content built from tags that add no words of their own (no
        /// attributes, no lists, notes or unresolved-reference markers), every
        /// visible character of the XML text reaches the reader, in order,
        /// and nothing else does — unknown tags included, through the fallback.
        #[test]
        fn rendered_text_is_exactly_the_document_text(
            nodes in proptest::collection::vec(tree(
                KNOWN.iter().chain(UNKNOWN).copied()
                    .filter(|n| !is_inheritdoc(n) && !matches!(*n, "list" | "ul" | "ol" | "note" | "include" | "table" | "tr" | "sup"))
                    .collect(),
                false,
            ), 0..6)
        ) {
            let expected: String = nodes.iter().map(|n| match n {
                DocNode::Text(t) => t.clone(),
                DocNode::Element(e) => e.text_content(),
            }).collect();
            let (blocks, _) = render_content(&nodes);
            let printed = to_markdown(&blocks);
            prop_assert_eq!(
                visible(&text_of(&parse_back(&printed))),
                visible(&expected),
                "printed:\n{}", printed
            );
        }

        #[test]
        fn dedent_is_idempotent_and_removes_only_whitespace(code in
            proptest::collection::vec(
                prop_oneof![
                    "[a-z]{0,4}".boxed(),
                    proptest::sample::select(vec!["\n", "  ", "\t", "    ", "\r\n", "\r"])
                        .prop_map(str::to_string).boxed(),
                ],
                0..12,
            ).prop_map(|p| p.concat())
        ) {
            let once = dedent(&code);
            prop_assert_eq!(dedent(&once), once.clone());
            prop_assert_eq!(visible(&once), visible(&code));
        }
    }
}
