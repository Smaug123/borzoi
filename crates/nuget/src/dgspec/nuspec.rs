//! The nuspec domain B₁ commits in.
//!
//! Restore reads a package's nuspec even from a warm cache, and throws or
//! fails on what it rejects. B₁ does not model what restore rejects: it states
//! what it *admits*, layer by layer, and [`nuspec_is_admitted`] walks every
//! byte, declaration pseudo-attribute, element, attribute and text node of the
//! document, declining anything the domain does not name. That is the closure
//! argument: a nuspec outside the domain cannot commit, whatever restore does
//! with it, so a disagreement with restore can only be found inside the
//! domain, where `tests/all/dgspec_resolve_diff.rs` grades it.
//!
//! The layers, each with its own positive domain, because a sweep at one layer
//! says nothing about the next (single-slot text perturbation cannot re-encode
//! the file or add an element):
//!
//! - **Bytes.** UTF-8, with or without a byte order mark. UTF-16 declines,
//!   though [`crate::parse_nuspec`]'s callers decode it: whether `System.Xml`
//!   reads it as this crate does is not graded.
//! - **The XML declaration.** Absent, or `version="1.0"`, an `encoding` that
//!   is absent or `utf-8` ignoring case, and a `standalone` that is absent,
//!   `yes` or `no`. `System.Xml` throws on `version="1.1"`, on an encoding it
//!   does not know and on `utf-16` over UTF-8 bytes; `roxmltree` reads past
//!   all three, so without this layer B₁ committed where restore threw.
//! - **The document.** What `roxmltree` parses with its default options,
//!   which refuse a DTD as `System.Xml`'s default settings do. Comments and
//!   processing instructions are admitted anywhere: no XML reader surfaces
//!   them as content. Text between elements must be whitespace.
//! - **The vocabulary.** A `<package>` in one of the published nuspec
//!   namespaces or none, holding one `<metadata>`, both spelt in the default
//!   namespace (NuGet's reader looks for `<metadata>`'s children there, so a
//!   nuspec prefixed throughout has no identity to it). `<metadata>`'s
//!   children are each in [`DESCRIPTIVE_ELEMENTS`], whose subtrees restore
//!   never reads and which may hold anything, or one of the elements restore
//!   reads and B₁ models, each with exactly the attributes and children the
//!   SDK's `pack` writes: `id` and `version` once each; `dependencies`,
//!   `references`, `packageTypes` and `contentFiles` at most once (NuGet's reader takes the
//!   first of each, which the lenient reader does not). Two `<group>`s for
//!   one framework are admitted: the first wins on both sides, and the
//!   generated sweep grades it. The elements restore reads
//!   and B₁ does not model (`frameworkAssemblies`, `frameworkReferences`)
//!   decline by name, as does any other.
//! - **Identity.** `<id>` and `<version>` name the folder the package is in;
//!   every `<packageType>` is `Dependency` without a version (restore refuses
//!   other types in a `PackageReference` project, `DotnetTool` with `NU1212`,
//!   and throws on a malformed version); `minClientVersion`, if present,
//!   parses without a release label and is below the restoring NuGet's
//!   version (strictly, because the verified NuGets are release candidates,
//!   which sort below the release of the same number).
//!
//! The vocabulary was measured, not guessed: over the 3,910 nuspecs of a
//! developer's package cache, every one has exactly one `<metadata>` in the
//! root's namespace, no attribute but `minClientVersion` on it, no attribute on
//! `<dependency>` but `id`, `version`, `include` and `exclude`, no child of
//! `<group>` but `<dependency>`, and no repeated `<dependencies>` or
//! `<group>` framework; 3,837 open with a UTF-8 byte order mark and 2 are
//! UTF-16. What that cache has and this domain declines is
//! `<frameworkAssemblies>` (839 nuspecs) and `<frameworkReferences>` (74):
//! later stages.

use super::resolve::NuGetIdentity;
use crate::{NuGetFramework, NuGetVersion, PackageId, PackageIdentity, VersionRange};

use std::collections::BTreeSet;
use std::fmt;

/// Why a nuspec is outside the domain, by layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NuspecDecline {
    /// The bytes are not UTF-8.
    Bytes(String),
    /// The XML declaration is outside the domain; carries the declaration.
    Declaration(String),
    /// `roxmltree` rejected the document.
    Xml(String),
    /// An element, attribute or text node outside the vocabulary; `path` is
    /// the element it was found at.
    Vocabulary { path: String, reason: String },
    /// Metadata inside the vocabulary that restore validates against the
    /// package and the restoring NuGet: identity, package type, minimum
    /// client version.
    Identity(String),
}

impl NuspecDecline {
    /// The layer the decline is at, for a census.
    pub fn layer(&self) -> &'static str {
        match self {
            NuspecDecline::Bytes(_) => "bytes",
            NuspecDecline::Declaration(_) => "declaration",
            NuspecDecline::Xml(_) => "xml",
            NuspecDecline::Vocabulary { .. } => "vocabulary",
            NuspecDecline::Identity(_) => "identity",
        }
    }
}

impl fmt::Display for NuspecDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NuspecDecline::Bytes(reason) => write!(f, "nuspec bytes: {reason}"),
            NuspecDecline::Declaration(declaration) => {
                write!(
                    f,
                    "nuspec XML declaration outside the domain: {declaration}"
                )
            }
            NuspecDecline::Xml(reason) => write!(f, "nuspec is not XML: {reason}"),
            NuspecDecline::Vocabulary { path, reason } => {
                write!(f, "nuspec outside the vocabulary at {path}: {reason}")
            }
            NuspecDecline::Identity(reason) => write!(f, "nuspec metadata: {reason}"),
        }
    }
}

/// The namespaces the published nuspec schemas have had.
const NUSPEC_NAMESPACES: &[&str] = &[
    "http://schemas.microsoft.com/packaging/2010/07/nuspec.xsd",
    "http://schemas.microsoft.com/packaging/2011/08/nuspec.xsd",
    "http://schemas.microsoft.com/packaging/2011/10/nuspec.xsd",
    "http://schemas.microsoft.com/packaging/2012/06/nuspec.xsd",
    "http://schemas.microsoft.com/packaging/2013/01/nuspec.xsd",
    "http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd",
];

/// The `<metadata>` elements restore never reads: what a package says about
/// itself for people and galleries, and `developmentDependency`, which restore
/// ignores whatever its value. Their subtrees may hold anything. The
/// differential's generator writes them, malformed ones included, so the
/// claim is graded.
pub const DESCRIPTIVE_ELEMENTS: &[&str] = &[
    "authors",
    "owners",
    "title",
    "description",
    "summary",
    "releaseNotes",
    "copyright",
    "language",
    "tags",
    "projectUrl",
    "iconUrl",
    "icon",
    "licenseUrl",
    "license",
    "readme",
    "repository",
    "requireLicenseAcceptance",
    "serviceable",
    "developmentDependency",
];

/// The `<metadata>` elements restore reads and B₁ does not model.
const UNMODELLED_ELEMENTS: &[&str] = &["frameworkAssemblies", "frameworkReferences"];

/// Whether `bytes`, the nuspec of the package `identity` is installed as, is
/// in the domain B₁ commits in under the restoring NuGet `nuget`.
pub fn nuspec_is_admitted(
    bytes: &[u8],
    identity: &PackageIdentity,
    nuget: &NuGetIdentity,
) -> Result<(), NuspecDecline> {
    let text = admitted_text(bytes)?;
    admitted_declaration(text)?;
    let document =
        roxmltree::Document::parse(text).map_err(|error| NuspecDecline::Xml(error.to_string()))?;
    let metadata = admitted_root(&document)?;
    admitted_metadata(metadata, identity, nuget)
}

// ----------------------------------------------------------------------------
// Bytes
// ----------------------------------------------------------------------------

fn admitted_text(bytes: &[u8]) -> Result<&str, NuspecDecline> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        return Err(NuspecDecline::Bytes("a UTF-16 byte order mark".to_owned()));
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    std::str::from_utf8(bytes).map_err(|error| NuspecDecline::Bytes(error.to_string()))
}

// ----------------------------------------------------------------------------
// The XML declaration
// ----------------------------------------------------------------------------

pub(crate) fn admitted_declaration(text: &str) -> Result<(), NuspecDecline> {
    let Some(rest) = text.strip_prefix("<?xml") else {
        return Ok(());
    };
    // `<?xml-stylesheet …?>` is a processing instruction, not a declaration;
    // `roxmltree` decides whether its target is legal.
    if !rest.starts_with(is_xml_space) {
        return Ok(());
    }
    let decline = || {
        let end = text.find("?>").map_or_else(
            || text.char_indices().nth(80).map_or(text.len(), |(i, _)| i),
            |end| end + 2,
        );
        NuspecDecline::Declaration(text[..end].to_owned())
    };
    let Some(end) = rest.find("?>") else {
        return Err(decline());
    };
    let mut seen = BTreeSet::new();
    let mut body = rest[..end].trim_matches(is_xml_space);
    while !body.is_empty() {
        let name_end = body
            .find(|c: char| c == '=' || is_xml_space(c))
            .ok_or_else(decline)?;
        let name = &body[..name_end];
        let after_name = body[name_end..].trim_start_matches(is_xml_space);
        let after_eq = after_name.strip_prefix('=').ok_or_else(decline)?;
        let after_eq = after_eq.trim_start_matches(is_xml_space);
        let quote = after_eq.chars().next().ok_or_else(decline)?;
        if quote != '"' && quote != '\'' {
            return Err(decline());
        }
        let value_end = after_eq[1..].find(quote).ok_or_else(decline)?;
        let value = &after_eq[1..1 + value_end];
        body = after_eq[2 + value_end..].trim_start_matches(is_xml_space);
        if !seen.insert(name) {
            return Err(decline());
        }
        let admitted = match name {
            "version" => value == "1.0",
            "encoding" => value.eq_ignore_ascii_case("utf-8"),
            "standalone" => value == "yes" || value == "no",
            _ => false,
        };
        if !admitted {
            return Err(decline());
        }
    }
    if !seen.contains("version") {
        return Err(decline());
    }
    Ok(())
}

fn is_xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

// ----------------------------------------------------------------------------
// The tree
// ----------------------------------------------------------------------------

type Node<'a, 'input> = roxmltree::Node<'a, 'input>;

/// The nuspec namespace of a document: its root's.
#[derive(Clone, Copy)]
struct Vocabulary<'a> {
    namespace: Option<&'a str>,
}

impl Vocabulary<'_> {
    /// Whether `node` is an element of the nuspec namespace spelt in the
    /// default namespace, which is where NuGet's reader looks for every
    /// element it reads.
    fn plain(self, node: Node<'_, '_>) -> bool {
        node.is_element()
            && node.tag_name().namespace() == self.namespace
            && node.tag_name().namespace() == node.default_namespace()
    }
}

fn vocabulary_decline(path: &str, reason: impl Into<String>) -> NuspecDecline {
    NuspecDecline::Vocabulary {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

/// The element children of `node`, with every non-element child shown to be
/// inert: a comment, a processing instruction, or whitespace.
fn element_children<'a, 'input>(
    node: Node<'a, 'input>,
    path: &str,
) -> Result<Vec<Node<'a, 'input>>, NuspecDecline> {
    let mut elements = Vec::new();
    for child in node.children() {
        if child.is_element() {
            elements.push(child);
        } else if child.is_text()
            && !child
                .text()
                .unwrap_or_default()
                .trim_matches(is_xml_space)
                .is_empty()
        {
            return Err(vocabulary_decline(path, "text between elements"));
        }
    }
    Ok(elements)
}

/// The text of `node`, which must hold no element: its text and CDATA nodes
/// concatenated, as `XElement.Value` gives it.
fn text_of(node: Node<'_, '_>, path: &str) -> Result<String, NuspecDecline> {
    let mut text = String::new();
    for child in node.children() {
        if child.is_element() {
            return Err(vocabulary_decline(path, "an element where text belongs"));
        }
        if child.is_text() {
            text.push_str(child.text().unwrap_or_default());
        }
    }
    Ok(text)
}

/// Every attribute of `node` is unnamespaced and in `allowed`.
fn attributes_in(node: Node<'_, '_>, path: &str, allowed: &[&str]) -> Result<(), NuspecDecline> {
    for attribute in node.attributes() {
        if attribute.namespace().is_some() {
            return Err(vocabulary_decline(
                path,
                format!("namespaced attribute {}", attribute.name()),
            ));
        }
        if !allowed.contains(&attribute.name()) {
            return Err(vocabulary_decline(
                path,
                format!("attribute {}", attribute.name()),
            ));
        }
    }
    Ok(())
}

/// `<package>` in a published namespace or none, spelt in the default
/// namespace, without attributes, holding exactly one element, `<metadata>`,
/// spelt the same way. Returns the `<metadata>` element.
fn admitted_root<'a, 'input>(
    document: &'a roxmltree::Document<'input>,
) -> Result<(Vocabulary<'a>, Node<'a, 'input>), NuspecDecline> {
    let root = document.root_element();
    let namespace = root.tag_name().namespace();
    if let Some(namespace) = namespace
        && !NUSPEC_NAMESPACES.contains(&namespace)
    {
        return Err(vocabulary_decline(
            "package",
            format!("namespace {namespace}"),
        ));
    }
    let vocabulary = Vocabulary { namespace };
    if root.tag_name().name() != "package" || !vocabulary.plain(root) {
        return Err(vocabulary_decline(
            "package",
            format!(
                "root element {} is not <package> in the default namespace",
                root.tag_name().name()
            ),
        ));
    }
    attributes_in(root, "package", &[])?;
    let children = element_children(root, "package")?;
    let [metadata] = children.as_slice() else {
        return Err(vocabulary_decline(
            "package",
            format!("{} elements where one <metadata> belongs", children.len()),
        ));
    };
    if metadata.tag_name().name() != "metadata" || !vocabulary.plain(*metadata) {
        return Err(vocabulary_decline(
            "package",
            format!(
                "element {} where <metadata> in the default namespace belongs",
                metadata.tag_name().name()
            ),
        ));
    }
    Ok((vocabulary, *metadata))
}

/// `<metadata>`'s attributes and children, and the identity they state.
fn admitted_metadata(
    (vocabulary, metadata): (Vocabulary<'_>, Node<'_, '_>),
    identity: &PackageIdentity,
    nuget: &NuGetIdentity,
) -> Result<(), NuspecDecline> {
    attributes_in(metadata, "metadata", &["minClientVersion"])?;
    let mut ids = 0;
    let mut versions = 0;
    let mut once: BTreeSet<&str> = BTreeSet::new();
    for child in element_children(metadata, "metadata")? {
        let name = child.tag_name().name();
        if !vocabulary.plain(child) {
            return Err(vocabulary_decline(
                "metadata",
                format!("element {name} outside the default namespace"),
            ));
        }
        if DESCRIPTIVE_ELEMENTS.contains(&name) {
            continue;
        }
        if UNMODELLED_ELEMENTS.contains(&name) {
            return Err(vocabulary_decline(
                "metadata",
                format!("element {name}, which restore reads and B₁ does not model"),
            ));
        }
        let path = format!("metadata/{name}");
        match name {
            "id" => {
                ids += 1;
                attributes_in(child, &path, &[])?;
                let text = text_of(child, &path)?;
                if !text.eq_ignore_ascii_case(identity.id.as_str()) {
                    return Err(NuspecDecline::Identity(format!(
                        "<id> {text:?} is not the folder's {}",
                        identity.id
                    )));
                }
            }
            "version" => {
                versions += 1;
                attributes_in(child, &path, &[])?;
                let text = text_of(child, &path)?;
                let stated = NuGetVersion::parse(&text)
                    .ok()
                    .filter(|v| v.eq_strict(&identity.version));
                if stated.is_none() {
                    return Err(NuspecDecline::Identity(format!(
                        "<version> {text:?} is not the folder's {}",
                        identity.version
                    )));
                }
            }
            "dependencies" | "references" | "packageTypes" | "contentFiles" => {
                if !once.insert(name) {
                    return Err(vocabulary_decline("metadata", format!("a second <{name}>")));
                }
                attributes_in(child, &path, &[])?;
                match name {
                    "dependencies" => admitted_dependencies(vocabulary, child, &path)?,
                    "references" => admitted_references(vocabulary, child, &path)?,
                    "packageTypes" => admitted_package_types(vocabulary, child, &path)?,
                    _ => admitted_content_files(vocabulary, child, &path)?,
                }
            }
            _ => {
                return Err(vocabulary_decline("metadata", format!("element {name}")));
            }
        }
    }
    if ids != 1 || versions != 1 {
        return Err(NuspecDecline::Identity(format!(
            "{ids} <id> and {versions} <version> elements where one of each belongs"
        )));
    }
    admitted_min_client_version(metadata.attribute("minClientVersion"), nuget)
}

/// The children of a container are all `group`s or all `leaf`s (`pack` writes
/// one or the other), each `plain`. Returns them with which it was.
fn homogeneous_children<'a, 'input>(
    vocabulary: Vocabulary<'_>,
    node: Node<'a, 'input>,
    path: &str,
    leaf: &str,
) -> Result<(bool, Vec<Node<'a, 'input>>), NuspecDecline> {
    let children = element_children(node, path)?;
    let mut grouped = None;
    for child in &children {
        let name = child.tag_name().name();
        if !vocabulary.plain(*child) {
            return Err(vocabulary_decline(
                path,
                format!("element {name} outside the default namespace"),
            ));
        }
        let is_group = match name {
            "group" => true,
            _ if name == leaf => false,
            _ => return Err(vocabulary_decline(path, format!("element {name}"))),
        };
        if *grouped.get_or_insert(is_group) != is_group {
            return Err(vocabulary_decline(
                path,
                format!("<group> and <{leaf}> elements side by side"),
            ));
        }
    }
    Ok((grouped.unwrap_or(false), children))
}

/// A `<group>`'s framework: absent or empty is NuGet's `Any`; otherwise it
/// parses, and is spelt without surrounding whitespace.
fn group_framework(group: Node<'_, '_>, path: &str) -> Result<(), NuspecDecline> {
    attributes_in(group, path, &["targetFramework"])?;
    match group.attribute("targetFramework") {
        None | Some("") => Ok(()),
        Some(raw) => {
            if raw.trim_matches(is_xml_space) != raw {
                return Err(vocabulary_decline(
                    path,
                    format!("targetFramework {raw:?} has surrounding whitespace"),
                ));
            }
            NuGetFramework::parse(raw).map(|_| ()).map_err(|error| {
                vocabulary_decline(path, format!("targetFramework {raw:?}: {error}"))
            })
        }
    }
}

fn admitted_dependencies(
    vocabulary: Vocabulary<'_>,
    dependencies: Node<'_, '_>,
    path: &str,
) -> Result<(), NuspecDecline> {
    let (grouped, children) = homogeneous_children(vocabulary, dependencies, path, "dependency")?;
    if !grouped {
        for dependency in children {
            admitted_dependency(dependency, &format!("{path}/dependency"))?;
        }
        return Ok(());
    }
    for group in children {
        let group_path = format!("{path}/group");
        group_framework(group, &group_path)?;
        for dependency in element_children(group, &group_path)? {
            let dependency_path = format!("{group_path}/dependency");
            if dependency.tag_name().name() != "dependency" || !vocabulary.plain(dependency) {
                return Err(vocabulary_decline(
                    &group_path,
                    format!("element {}", dependency.tag_name().name()),
                ));
            }
            admitted_dependency(dependency, &dependency_path)?;
        }
    }
    Ok(())
}

/// `<dependency id version include exclude />`: an `id` spelt as a package id
/// without surrounding whitespace, and a `version` that is absent, empty, or
/// a range.
fn admitted_dependency(dependency: Node<'_, '_>, path: &str) -> Result<(), NuspecDecline> {
    attributes_in(dependency, path, &["id", "version", "include", "exclude"])?;
    if !element_children(dependency, path)?.is_empty() {
        return Err(vocabulary_decline(path, "an element inside <dependency>"));
    }
    let Some(id) = dependency.attribute("id") else {
        return Err(vocabulary_decline(path, "no id"));
    };
    if id.trim_matches(is_xml_space) != id || PackageId::parse(id).is_err() {
        return Err(vocabulary_decline(path, format!("id {id:?}")));
    }
    if let Some(range) = dependency.attribute("version") {
        let trimmed = range.trim_matches(is_xml_space);
        if !trimmed.is_empty() && VersionRange::parse(trimmed).is_err() {
            return Err(vocabulary_decline(path, format!("version {range:?}")));
        }
    }
    Ok(())
}

fn admitted_references(
    vocabulary: Vocabulary<'_>,
    references: Node<'_, '_>,
    path: &str,
) -> Result<(), NuspecDecline> {
    let (grouped, children) = homogeneous_children(vocabulary, references, path, "reference")?;
    if !grouped {
        for reference in children {
            admitted_reference(reference, &format!("{path}/reference"))?;
        }
        return Ok(());
    }
    for group in children {
        let group_path = format!("{path}/group");
        group_framework(group, &group_path)?;
        for reference in element_children(group, &group_path)? {
            if reference.tag_name().name() != "reference" || !vocabulary.plain(reference) {
                return Err(vocabulary_decline(
                    &group_path,
                    format!("element {}", reference.tag_name().name()),
                ));
            }
            admitted_reference(reference, &format!("{group_path}/reference"))?;
        }
    }
    Ok(())
}

/// `<reference file />`.
fn admitted_reference(reference: Node<'_, '_>, path: &str) -> Result<(), NuspecDecline> {
    attributes_in(reference, path, &["file"])?;
    if !element_children(reference, path)?.is_empty() {
        return Err(vocabulary_decline(path, "an element inside <reference>"));
    }
    Ok(())
}

/// `<packageTypes>` holding `<packageType name="Dependency" />` elements and
/// nothing else.
fn admitted_package_types(
    vocabulary: Vocabulary<'_>,
    package_types: Node<'_, '_>,
    path: &str,
) -> Result<(), NuspecDecline> {
    for package_type in element_children(package_types, path)? {
        let type_path = format!("{path}/packageType");
        if package_type.tag_name().name() != "packageType" || !vocabulary.plain(package_type) {
            return Err(vocabulary_decline(
                path,
                format!("element {}", package_type.tag_name().name()),
            ));
        }
        attributes_in(package_type, &type_path, &["name", "version"])?;
        if !element_children(package_type, &type_path)?.is_empty() {
            return Err(vocabulary_decline(
                &type_path,
                "an element inside <packageType>",
            ));
        }
        if package_type.attribute("name") != Some("Dependency")
            || package_type.attribute("version").is_some()
        {
            return Err(NuspecDecline::Identity(format!(
                "package type {:?}{} is not Dependency without a version",
                package_type.attribute("name").unwrap_or_default(),
                package_type
                    .attribute("version")
                    .map(|v| format!(" version {v:?}"))
                    .unwrap_or_default()
            )));
        }
    }
    Ok(())
}

/// `<contentFiles>` holding `<files>` rules in the subset restore reads
/// without throwing: a non-empty `include` and an optional `exclude`, both in
/// the glob subset of [`glob_is_admitted`]; a `buildAction` among `None`,
/// `Compile`, `Content` and `EmbeddedResource` (compared ignoring case, as
/// restore does); and `copyToOutput` and `flatten`, if present, `true` or
/// `false` ignoring case. Restore throws on a rule without an `include`, on a
/// `buildAction` it does not know, and on a `copyToOutput` or `flatten` that
/// is not a boolean.
fn admitted_content_files(
    vocabulary: Vocabulary<'_>,
    content_files: Node<'_, '_>,
    path: &str,
) -> Result<(), NuspecDecline> {
    let boolean =
        |value: &str| value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false");
    for rule in element_children(content_files, path)? {
        let rule_path = format!("{path}/files");
        if rule.tag_name().name() != "files" || !vocabulary.plain(rule) {
            return Err(vocabulary_decline(
                path,
                format!("element {}", rule.tag_name().name()),
            ));
        }
        attributes_in(
            rule,
            &rule_path,
            &[
                "include",
                "exclude",
                "buildAction",
                "copyToOutput",
                "flatten",
            ],
        )?;
        if !element_children(rule, &rule_path)?.is_empty() {
            return Err(vocabulary_decline(&rule_path, "an element inside <files>"));
        }
        let Some(include) = rule.attribute("include") else {
            return Err(vocabulary_decline(&rule_path, "no include"));
        };
        if !glob_is_admitted(include) {
            return Err(vocabulary_decline(
                &rule_path,
                format!("include {include:?}"),
            ));
        }
        if let Some(exclude) = rule.attribute("exclude")
            && !glob_is_admitted(exclude)
        {
            return Err(vocabulary_decline(
                &rule_path,
                format!("exclude {exclude:?}"),
            ));
        }
        if let Some(action) = rule.attribute("buildAction")
            && !["None", "Compile", "Content", "EmbeddedResource"]
                .iter()
                .any(|known| known.eq_ignore_ascii_case(action))
        {
            return Err(vocabulary_decline(
                &rule_path,
                format!("buildAction {action:?}"),
            ));
        }
        for flag in ["copyToOutput", "flatten"] {
            if let Some(value) = rule.attribute(flag)
                && !boolean(value)
            {
                return Err(vocabulary_decline(&rule_path, format!("{flag} {value:?}")));
            }
        }
    }
    Ok(())
}

/// Whether a content-file glob is in the subset B₁ admits: `/`-separated
/// segments, each non-empty and drawn from ASCII letters, digits, `.`, `_`,
/// `-` and `*`, none of them `.` or `..`. Restore throws on a `..` anywhere but
/// the start of a pattern, and reads `\` as a separator; neither is admitted.
fn glob_is_admitted(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '*'))
        })
}

/// `minClientVersion`, if present, parses without a release label and is
/// strictly below the restoring NuGet's release version.
fn admitted_min_client_version(
    minimum: Option<&str>,
    nuget: &NuGetIdentity,
) -> Result<(), NuspecDecline> {
    let Some(minimum) = minimum else {
        return Ok(());
    };
    let release = nuget
        .as_str()
        .split(['-', '+'])
        .next()
        .and_then(|release| NuGetVersion::parse(release).ok());
    match (NuGetVersion::parse(minimum), release) {
        (Ok(parsed), Some(release)) if !parsed.is_prerelease() && parsed < release => Ok(()),
        _ => Err(NuspecDecline::Identity(format!(
            "minClientVersion {minimum:?} is not shown to be below NuGet {nuget}"
        ))),
    }
}
