//! Generative differential: whole `.fsproj` documents built from the XML
//! grammar the evaluator models, against the real MSBuild evaluator's `project`
//! and `items` ops.
//!
//! ## Why this exists
//!
//! Every other whole-document differential in this crate starts from a fixed
//! shape — a list of property writes (`fsproj_property_table_diff`), an item
//! spec over an escape alphabet (`fsproj_item_escape_generative_diff`), a corner
//! list of global routes (`fsproj_global_perturbation_diff`) — or from the real
//! projects in the corpus. None of them composes the constructs: a `<Choose>`
//! arm holding an `<ItemGroup>` whose `Exclude` names a property written by an
//! import selected by another property, under a global the document opts out of
//! with `TreatAsLocalProperty`. The evaluator's bugs live in those compositions.
//!
//! So this file generates documents from the grammar itself: `PropertyGroup` /
//! `ItemGroup` with conditions; `Choose` / `When` / `Otherwise`, nested;
//! property self-reference and forward reference; `Import` with conditions and
//! property-selected paths, of files the generator also writes; `Compile`
//! `Include` / `Exclude` / `Remove` / `Update`, with wildcards over a generated
//! file tree resolved by the **shipped** glob resolver
//! ([`borzoi_msbuild::glob_resolver::resolve`]); `TreatAsLocalProperty`; and
//! XML-layer variation (insignificant whitespace, entities, CDATA,
//! comment-split text). Every case-insensitive lexeme is respelt — item types,
//! property names in definitions and references, global names, condition
//! keywords and operands.
//!
//! What it leaves out: documents MSBuild *rejects* (an unknown or misspelt
//! element or attribute, a non-ASCII property name, an unguarded import of a
//! missing file). The evaluator does not model MSBuild's project-load
//! validation, so it still commits values for those, and that is a separate
//! piece of work rather than something this harness can hold to account yet.
//!
//! ## The contract
//!
//! Certain-implies-exact, per property and for the Compile list, under each of
//! a small set of global properties:
//!
//! - a read-back property we committed (present, provenance trusted) must equal
//!   MSBuild's value;
//! - when `items_uncertain` is false, our `Compile` items must be MSBuild's, in
//!   MSBuild's order — `items_uncertain` is the flag the LSP gates the Compile
//!   order on, so that is the claim checked, not a stricter one.
//!
//! The grammar builds only documents MSBuild accepts, and asserts that it did.
//!
//! ## The must-commit obligation
//!
//! A one-sided contract is satisfied by declining everything, and a floor on a
//! commit *count* rots the moment a declining axis is added. So a generated
//! document also carries an obligation: every read-back name MSBuild defines
//! must be committed, no committed name may be untrusted, and the Compile list
//! must be certain. A decline is reported as a failure exactly like a wrong
//! commit. The exceptions are the constructs listed in [`KNOWN_DECLINES`], each
//! with its reason; a document holding one is held to exactness only, and half
//! the documents never use one. A census then checks that every other construct
//! appears in enough obligation-bound documents for the obligation to bind on
//! it.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use borzoi_msbuild::{ItemKind, ParsedProject, glob_resolver, parse_fsproj_with_imports};
use common::case_axis::{CaseContext, respell_case, respell_name};
use common::{Oracle, SplitMix64, soak_parameters};

/// The source tree every case lays down. No two names differ only in case, so
/// the glob resolver's case-decided-order declines never fire on it.
const SOURCES: &[&str] = &[
    "src/A.fs",
    "src/B.fs",
    "src/sub/C.fs",
    "gen/G.fs",
    "Z.fs",
    "notes.txt",
];

/// Property names the documents write and that both sides read back.
const NAMES: &[&str] = &["Alpha", "Beta", "Gamma", "Sel", "Dir", "Configuration"];

/// The global sets each document is evaluated under: none, what the LSP
/// injects, and a respelt `Configuration` (global names are case-insensitive).
fn global_sets() -> Vec<Vec<(String, String)>> {
    vec![
        vec![],
        vec![
            ("Configuration".to_string(), "Debug".to_string()),
            ("Platform".to_string(), "AnyCPU".to_string()),
        ],
        vec![("CONFIGURATION".to_string(), "Release".to_string())],
    ]
}

/// One grammar construct. The census reports each, and the obligation must bind
/// on each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Construct {
    PropertyGroupCondition,
    PropertyCondition,
    SelfReference,
    ForwardReference,
    XmlWhitespace,
    XmlEntity,
    XmlCdata,
    XmlComment,
    ItemGroupCondition,
    ItemCondition,
    CompileInclude,
    CompileExclude,
    CompileRemove,
    CompileUpdate,
    Wildcard,
    RecursiveWildcard,
    PropertyInItemSpec,
    ChooseWhen,
    ChooseOtherwise,
    ChooseNested,
    Import,
    ImportConditioned,
    ImportPropertySelected,
    TreatAsLocalProperty,
    RespeltItemType,
    RespeltPropertyName,
    RespeltCondition,
}

/// Constructs the evaluator declines by design, each with the reason. A
/// document holding one is still held to certain-implies-exact, but not to the
/// must-commit obligation. Every other construct is.
///
/// Adding an entry here is a statement that the decline is *legitimate*; it is
/// the place a reviewer should push back.
const KNOWN_DECLINES: &[(Construct, &str)] = &[
    (
        Construct::XmlCdata,
        "roxmltree merges CDATA with adjacent text and truncates its range, so \
         `collect_element_text` cannot tell a CDATA space from insignificant \
         literal whitespace and declines any body holding CDATA. The decline \
         cascades to every condition and body that reads the property.",
    ),
    (
        Construct::CompileRemove,
        "`<Compile Remove>` is not modelled: a Remove that runs marks the Compile \
         list uncertain (`ParsedProject::items_uncertain`).",
    ),
];

fn is_known_decline(construct: Construct) -> bool {
    KNOWN_DECLINES.iter().any(|(c, _)| *c == construct)
}

/// Every construct, for the census's non-vacuity check.
const CONSTRUCTS: &[Construct] = &[
    Construct::PropertyGroupCondition,
    Construct::PropertyCondition,
    Construct::SelfReference,
    Construct::ForwardReference,
    Construct::XmlWhitespace,
    Construct::XmlEntity,
    Construct::XmlCdata,
    Construct::XmlComment,
    Construct::ItemGroupCondition,
    Construct::ItemCondition,
    Construct::CompileInclude,
    Construct::CompileExclude,
    Construct::CompileRemove,
    Construct::CompileUpdate,
    Construct::Wildcard,
    Construct::RecursiveWildcard,
    Construct::PropertyInItemSpec,
    Construct::ChooseWhen,
    Construct::ChooseOtherwise,
    Construct::ChooseNested,
    Construct::Import,
    Construct::ImportConditioned,
    Construct::ImportPropertySelected,
    Construct::TreatAsLocalProperty,
    Construct::RespeltItemType,
    Construct::RespeltPropertyName,
    Construct::RespeltCondition,
];

/// Attribute-value escaping: the generator's own text, made safe inside `"…"`.
fn attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

struct Gen<'r> {
    rng: &'r mut SplitMix64,
    /// Whether this document may use a [`KNOWN_DECLINES`] construct. Half the
    /// documents may not, so the obligation binds on a healthy share of them
    /// rather than on whatever few happen to avoid every exempt construct.
    known_declines: bool,
    used: BTreeSet<Construct>,
    /// Names written so far, in document order — so a reference can be made
    /// forward (to a name not yet written) on purpose.
    written: BTreeSet<&'static str>,
}

impl Gen<'_> {
    fn chance(&mut self, one_in: usize) -> bool {
        self.rng.below(one_in) == 0
    }

    fn mark(&mut self, construct: Construct) {
        self.used.insert(construct);
    }

    /// A property name as written in a definition or reference: respelt now and
    /// then, which MSBuild reads case-insensitively.
    fn name_spelling(&mut self, name: &str) -> String {
        if self.chance(3) {
            let respelt = respell_name(self.rng, name);
            if respelt != name {
                self.mark(Construct::RespeltPropertyName);
            }
            respelt
        } else {
            name.to_string()
        }
    }

    fn reference(&mut self, name: &'static str) -> String {
        if !self.written.contains(name) {
            self.mark(Construct::ForwardReference);
        }
        let spelt = self.name_spelling(name);
        format!("$({spelt})")
    }

    /// A condition, as raw text (not yet attribute-escaped).
    fn condition(&mut self) -> String {
        let pick = self.rng.below(10);
        let raw = match pick {
            0 => format!("'{}' == 'x'", self.reference("Alpha")),
            1 => format!("'{}' == 'Debug'", self.reference("Configuration")),
            2 => format!("'{}' != ''", self.reference("Beta")),
            3 => format!("'{}' == 'a'", self.reference("Sel")),
            4 => "Exists('src/A.fs')".to_string(),
            5 => "Exists('missing.props')".to_string(),
            6 => "'true' == 'true'".to_string(),
            7 => format!(
                "'{}' == 'x' and '{}' != 'y'",
                self.reference("Alpha"),
                self.reference("Gamma")
            ),
            8 => format!("!('{}' == '')", self.reference("Dir")),
            _ => format!(
                "'{}' == 'Release' or '{}' == 'src'",
                self.reference("Configuration"),
                self.reference("Dir")
            ),
        };
        if self.chance(2) {
            let respelt = respell_case(self.rng, &raw, CaseContext::Condition);
            if respelt != raw {
                self.mark(Construct::RespeltCondition);
            }
            respelt
        } else {
            raw
        }
    }

    /// ` Condition="…"` some of the time, with `construct` marked when present.
    fn maybe_condition(&mut self, one_in: usize, construct: Construct) -> String {
        if !self.chance(one_in) {
            return String::new();
        }
        self.mark(construct);
        let raw = self.condition();
        // The XML layer: a condition's quotes as entities now and then.
        let text = if self.chance(4) {
            self.mark(Construct::XmlEntity);
            attr(&raw).replace('\'', "&apos;")
        } else {
            attr(&raw)
        };
        format!(" Condition=\"{text}\"")
    }

    /// A property body for `name`, as XML element content.
    fn body(&mut self, name: &'static str) -> String {
        match self.rng.below(14) {
            0 => "x".to_string(),
            1 => "src".to_string(),
            2 => if self.chance(2) { "a" } else { "b" }.to_string(),
            3 => {
                self.mark(Construct::SelfReference);
                let r = self.reference(name);
                format!("{r};more")
            }
            4 => {
                let other = *self.rng.pick(NAMES);
                let r = self.reference(other);
                format!("[{r}]")
            }
            5 => {
                self.mark(Construct::XmlWhitespace);
                "  x  ".to_string()
            }
            6 => {
                self.mark(Construct::XmlWhitespace);
                " \r\n ".to_string()
            }
            7 => {
                self.mark(Construct::XmlEntity);
                "a&amp;b".to_string()
            }
            8 => {
                self.mark(Construct::XmlEntity);
                "&#120;".to_string()
            }
            9 if self.known_declines => {
                self.mark(Construct::XmlCdata);
                "<![CDATA[x]]>".to_string()
            }
            10 => {
                self.mark(Construct::XmlComment);
                "  <!-- c -->x".to_string()
            }
            11 => {
                self.mark(Construct::XmlComment);
                "A<!-- c -->B".to_string()
            }
            9 | 12 => "Debug".to_string(),
            _ => {
                let r = self.reference("Configuration");
                format!("{r}-x")
            }
        }
    }

    fn property_group(&mut self) -> String {
        let cond = self.maybe_condition(4, Construct::PropertyGroupCondition);
        let mut s = format!("  <PropertyGroup{cond}>\n");
        for _ in 0..1 + self.rng.below(3) {
            let name = *self.rng.pick(NAMES);
            let spelt = self.name_spelling(name);
            let cond = self.maybe_condition(4, Construct::PropertyCondition);
            let body = self.body(name);
            s.push_str(&format!("    <{spelt}{cond}>{body}</{spelt}>\n"));
            self.written.insert(name);
        }
        s.push_str("  </PropertyGroup>\n");
        s
    }

    /// An item spec, as raw text (not yet attribute-escaped).
    fn spec(&mut self) -> String {
        let fragment = |g: &mut Self| -> String {
            match g.rng.below(11) {
                0 => "src/A.fs".to_string(),
                1 => "src/B.fs".to_string(),
                2 => "Z.fs".to_string(),
                3 => "gen/G.fs".to_string(),
                4 => {
                    g.mark(Construct::Wildcard);
                    "src/*.fs".to_string()
                }
                5 => {
                    g.mark(Construct::Wildcard);
                    "*.fs".to_string()
                }
                6 => {
                    g.mark(Construct::Wildcard);
                    g.mark(Construct::RecursiveWildcard);
                    "src/**/*.fs".to_string()
                }
                7 => {
                    g.mark(Construct::Wildcard);
                    g.mark(Construct::RecursiveWildcard);
                    "**/*.fs".to_string()
                }
                8 => {
                    g.mark(Construct::PropertyInItemSpec);
                    let r = g.reference("Dir");
                    format!("{r}/A.fs")
                }
                9 => {
                    g.mark(Construct::PropertyInItemSpec);
                    g.mark(Construct::Wildcard);
                    let r = g.reference("Dir");
                    format!("{r}/*.fs")
                }
                _ => "src/sub/C.fs".to_string(),
            }
        };
        let first = fragment(self);
        if self.chance(3) {
            let second = fragment(self);
            format!("{first};{second}")
        } else {
            first
        }
    }

    fn item_type(&mut self) -> String {
        if self.chance(3) {
            let respelt = respell_name(self.rng, "Compile");
            if respelt != "Compile" {
                self.mark(Construct::RespeltItemType);
            }
            respelt
        } else {
            "Compile".to_string()
        }
    }

    fn item_group(&mut self) -> String {
        let cond = self.maybe_condition(4, Construct::ItemGroupCondition);
        let mut s = format!("  <ItemGroup{cond}>\n");
        for _ in 0..1 + self.rng.below(3) {
            let ty = self.item_type();
            let cond = self.maybe_condition(5, Construct::ItemCondition);
            let spec = attr(&self.spec());
            match self.rng.below(6) {
                0 if self.known_declines => {
                    self.mark(Construct::CompileRemove);
                    s.push_str(&format!("    <{ty} Remove=\"{spec}\"{cond} />\n"));
                }
                1 => {
                    self.mark(Construct::CompileUpdate);
                    s.push_str(&format!(
                        "    <{ty} Update=\"{spec}\"{cond}>\n      <Link>x</Link>\n    </{ty}>\n"
                    ));
                }
                2 => {
                    self.mark(Construct::CompileInclude);
                    self.mark(Construct::CompileExclude);
                    let exclude = attr(&self.spec());
                    s.push_str(&format!(
                        "    <{ty} Include=\"{spec}\" Exclude=\"{exclude}\"{cond} />\n"
                    ));
                }
                _ => {
                    self.mark(Construct::CompileInclude);
                    s.push_str(&format!("    <{ty} Include=\"{spec}\"{cond} />\n"));
                }
            }
        }
        s.push_str("  </ItemGroup>\n");
        s
    }

    fn choose(&mut self, depth: u32) -> String {
        self.mark(Construct::ChooseWhen);
        let mut s = String::from("  <Choose>\n");
        for _ in 0..1 + self.rng.below(2) {
            let cond = attr(&self.condition());
            s.push_str(&format!("    <When Condition=\"{cond}\">\n"));
            s.push_str(&self.choose_arm(depth));
            s.push_str("    </When>\n");
        }
        if self.chance(2) {
            self.mark(Construct::ChooseOtherwise);
            s.push_str("    <Otherwise>\n");
            s.push_str(&self.choose_arm(depth));
            s.push_str("    </Otherwise>\n");
        }
        s.push_str("  </Choose>\n");
        s
    }

    fn choose_arm(&mut self, depth: u32) -> String {
        let mut s = String::new();
        for _ in 0..1 + self.rng.below(2) {
            match self.rng.below(5) {
                0 | 1 => s.push_str(&self.property_group()),
                2 | 3 => s.push_str(&self.item_group()),
                _ if depth > 0 => {
                    self.mark(Construct::ChooseNested);
                    s.push_str(&self.choose(depth - 1));
                }
                _ => s.push_str(&self.property_group()),
            }
        }
        s
    }

    fn import(&mut self) -> String {
        self.mark(Construct::Import);
        let pick = self.rng.below(5);
        match pick {
            0 => "  <Import Project=\"a.props\" />\n".to_string(),
            1 => {
                self.mark(Construct::ImportPropertySelected);
                let r = self.reference("Sel");
                // Guarded: an unset `Sel` names `.props`, which does not exist.
                format!("  <Import Project=\"{r}.props\" Condition=\"Exists('{r}.props')\" />\n")
            }
            2 => {
                self.mark(Construct::ImportConditioned);
                "  <Import Project=\"missing.props\" Condition=\"Exists('missing.props')\" />\n"
                    .to_string()
            }
            _ => {
                self.mark(Construct::ImportConditioned);
                let cond = attr(&self.condition());
                format!("  <Import Project=\"b.props\" Condition=\"{cond}\" />\n")
            }
        }
    }

    /// The body of a `<Project>`: `depth` bounds `Choose` nesting, `imports`
    /// whether `<Import>` may appear.
    fn project_body(&mut self, depth: u32, imports: bool) -> String {
        let mut s = String::new();
        for _ in 0..1 + self.rng.below(5) {
            match self.rng.below(7) {
                0 | 1 => s.push_str(&self.property_group()),
                2 | 3 => s.push_str(&self.item_group()),
                4 => s.push_str(&self.choose(depth)),
                5 if imports => s.push_str(&self.import()),
                _ => s.push_str(&self.property_group()),
            }
        }
        s
    }

    fn project(&mut self, depth: u32, imports: bool) -> String {
        let tlp = if self.chance(4) {
            self.mark(Construct::TreatAsLocalProperty);
            let spelt = self.name_spelling("Configuration");
            format!(" TreatAsLocalProperty=\"{spelt}\"")
        } else {
            String::new()
        };
        let body = self.project_body(depth, imports);
        format!("<Project{tlp}>\n{body}</Project>\n")
    }
}

struct Case {
    xml: String,
    /// `(relative path, contents)` for every file besides the entry project.
    files: Vec<(String, String)>,
    constructs: BTreeSet<Construct>,
}

fn generate(rng: &mut SplitMix64, known_declines: bool) -> Case {
    let mut g = Gen {
        rng,
        known_declines,
        used: BTreeSet::new(),
        written: BTreeSet::new(),
    };
    let xml = g.project(1, true);
    // Imported files are generated from the same grammar, one level shallower
    // and without further imports. Each starts its own write history: what an
    // import reads forward is not knowable from the entry document's order.
    let mut files: Vec<(String, String)> = Vec::new();
    for name in ["a.props", "b.props"] {
        g.written = BTreeSet::new();
        files.push((name.to_string(), g.project(0, false)));
    }
    let constructs = g.used;
    for source in SOURCES {
        files.push((source.to_string(), "module M\n".to_string()));
    }
    Case {
        xml,
        files,
        constructs,
    }
}

/// Lexical normalisation, as `Path.GetFullPath` does it: no filesystem access,
/// so a name that does not exist still compares by its spelling.
fn lexical(path: &Path) -> String {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().replace('\\', "/")
}

/// Our value for `name`, matched case-insensitively (a respelt definition is
/// stored under the spelling it was first written with).
fn our_property<'a>(parsed: &'a ParsedProject, name: &str) -> Option<&'a String> {
    parsed
        .properties
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

/// What one evaluation found. Both lists are `(kind, witness)`, collected
/// rather than asserted on the spot, so a run reports every class it found and
/// how often, not only the first.
#[derive(Default)]
struct Outcome {
    committed_names: usize,
    items_committed: bool,
    /// Certain-implies-exact violations: a committed value MSBuild disagrees
    /// with.
    violations: Vec<(String, String)>,
    /// Must-commit shortfalls: a decline where nothing in the document gives
    /// the evaluator a reason to.
    shortfalls: Vec<(String, String)>,
}

fn check(oracle: &mut Oracle, dir: &Path, case: &Case, globals: &[(String, String)]) -> Outcome {
    let project_path = dir.join("Demo.fsproj");
    std::fs::write(&project_path, &case.xml).expect("write project");
    let extra: HashMap<String, String> = globals.iter().cloned().collect();
    let resolver: &borzoi_msbuild::GlobResolver<'_> = &glob_resolver::resolve;
    let parsed = parse_fsproj_with_imports(
        &case.xml,
        &project_path,
        &extra,
        &common::oracle_environment(),
        None,
        Some(resolver),
    )
    .expect("the generator writes well-formed XML");

    let context = || {
        let mut s = format!(
            "--- globals {globals:?} ---\n--- Demo.fsproj ---\n{}",
            case.xml
        );
        for (name, contents) in &case.files {
            if name.ends_with(".props") {
                s.push_str(&format!("--- {name} ---\n{contents}"));
            }
        }
        s
    };

    let names: Vec<String> = NAMES.iter().map(|n| (*n).to_string()).collect();
    let theirs = oracle
        .project(&case.xml, &names, Some(&project_path), globals)
        .unwrap_or_else(|| {
            panic!(
                "MSBuild rejects a generated document — the grammar only builds \
                 documents MSBuild accepts\n{}",
                context()
            )
        });
    let theirs_items: Vec<String> = oracle
        .items(&case.xml, &project_path, "Compile", globals)
        .expect("the items op accepts what the project op accepted")
        .iter()
        .map(|p| lexical(Path::new(p)))
        .collect();

    let mut outcome = Outcome::default();
    for name in NAMES {
        let their_value = &theirs[*name];
        let untrusted = parsed.property_provenance_untrusted(name);
        let ours = our_property(&parsed, name);
        match ours {
            Some(ours) if !untrusted => {
                outcome.committed_names += 1;
                if ours != their_value {
                    outcome.violations.push((
                        format!("a committed $({name}) differs from MSBuild's"),
                        format!(
                            "we evaluate {ours:?}, MSBuild {their_value:?}\n{}",
                            context()
                        ),
                    ));
                }
            }
            Some(_) => outcome.shortfalls.push((
                format!("$({name}) untrusted"),
                format!(
                    "MSBuild={their_value:?}; diagnostics: {:?}\n{}",
                    parsed.diagnostics,
                    context()
                ),
            )),
            // A global the document never overrides is not in the property
            // table by design: it is an input, not something the document
            // computed.
            None if their_value.is_empty()
                || globals.iter().any(|(k, _)| k.eq_ignore_ascii_case(name)) => {}
            None => outcome.shortfalls.push((
                format!("$({name}) absent"),
                format!(
                    "MSBuild={their_value:?}; diagnostics: {:?}\n{}",
                    parsed.diagnostics,
                    context()
                ),
            )),
        }
    }

    let ours_items: Vec<String> = parsed
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Compile)
        .map(|i| lexical(&i.include))
        .collect();
    if parsed.items_uncertain {
        let cause = parsed
            .compile_item_uncertainties
            .first()
            .map(|c| {
                // `…{ kind: Diagnostic(UnsupportedItemOperation { operation: "Remove=…`
                // → `Diagnostic(UnsupportedItemOperation { operation: Remove`.
                let debug = format!("{c:?}");
                let kind = debug.split_once("kind: ").map_or(&debug[..], |(_, k)| k);
                let (head, rest) = kind.split_once('"').unwrap_or((kind, ""));
                let word: String = rest.chars().take_while(|ch| ch.is_alphanumeric()).collect();
                format!("{head}{word}")
            })
            .unwrap_or_default();
        outcome.shortfalls.push((
            format!("the Compile list declined ({cause})"),
            format!(
                "uncertainties: {:?}\n{}",
                parsed.compile_item_uncertainties,
                context()
            ),
        ));
    } else {
        outcome.items_committed = true;
        if ours_items != theirs_items {
            outcome.violations.push((
                "the committed Compile list differs from MSBuild's".to_string(),
                format!(
                    "ours:   {ours_items:#?}\ntheirs: {theirs_items:#?}\n(diagnostics: {:?})\n{}",
                    parsed.diagnostics,
                    context()
                ),
            ));
        }
    }
    outcome
}

#[derive(Default)]
struct ConstructCensus {
    documents: usize,
    /// Documents carrying the must-commit obligation (no known decline).
    bound_documents: usize,
    property_commits: usize,
    item_commits: usize,
}

/// `kind → (count, first witness)`.
type Tally = BTreeMap<String, (usize, String)>;

#[derive(Default)]
struct Census {
    by_construct: BTreeMap<Construct, ConstructCensus>,
    evaluations: usize,
    property_commits: usize,
    item_commits: usize,
    violations: Tally,
    shortfalls: Tally,
}

fn tally(into: &mut Tally, found: Vec<(String, String)>) {
    for (kind, witness) in found {
        into.entry(kind).or_insert((0, witness)).0 += 1;
    }
}

fn sweep(oracle: &mut Oracle, rng: &mut SplitMix64, cases: usize) -> Census {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
    let sets = global_sets();
    let mut census = Census::default();
    for index in 0..cases {
        let case = generate(rng, index % 2 == 1);
        let dir = root.join(format!("case{index}"));
        for (name, contents) in &case.files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, contents).expect("write file");
        }
        let bound = !case.constructs.iter().any(|c| is_known_decline(*c));
        let mut property_commits = 0;
        let mut item_commits = 0;
        for globals in &sets {
            let outcome = check(oracle, &dir, &case, globals);
            census.evaluations += 1;
            property_commits += outcome.committed_names;
            item_commits += usize::from(outcome.items_committed);
            tally(&mut census.violations, outcome.violations);
            if bound {
                tally(&mut census.shortfalls, outcome.shortfalls);
            }
        }
        census.property_commits += property_commits;
        census.item_commits += item_commits;
        for construct in &case.constructs {
            let row = census.by_construct.entry(*construct).or_default();
            row.documents += 1;
            row.bound_documents += usize::from(bound);
            row.property_commits += property_commits;
            row.item_commits += item_commits;
        }
    }
    census
}

fn report(census: &Census) {
    eprintln!(
        "fsproj grammar sweep: {} evaluations; {} property commits, {} Compile-list commits",
        census.evaluations, census.property_commits, census.item_commits
    );
    eprintln!(
        "  {:<24} {:>6} {:>6} {:>8} {:>8}",
        "construct", "docs", "bound", "prop-ok", "items-ok"
    );
    for (construct, row) in &census.by_construct {
        eprintln!(
            "  {:<24} {:>6} {:>6} {:>8} {:>8}",
            format!("{construct:?}"),
            row.documents,
            row.bound_documents,
            row.property_commits,
            row.item_commits
        );
    }
    for (title, found) in [
        ("violations", &census.violations),
        ("shortfalls", &census.shortfalls),
    ] {
        for (kind, (count, _)) in found {
            eprintln!("  {title}: {count:>5}  {kind}");
        }
    }
}

fn render(found: &Tally) -> String {
    found
        .iter()
        .map(|(kind, (count, witness))| format!("{kind}: {count} evaluations\n{witness}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The contract, the obligation, and the obligation's non-vacuity, shared by the
/// fixed-seed sweep and the soak.
fn assert_obligations(census: &Census, min_documents: usize) {
    assert!(
        census.violations.is_empty(),
        "certain-implies-exact violated in {} classes:\n\n{}",
        census.violations.len(),
        render(&census.violations)
    );
    assert!(
        census.shortfalls.is_empty(),
        "must-commit obligation failed in {} classes:\n\n{}",
        census.shortfalls.len(),
        render(&census.shortfalls)
    );
    for construct in CONSTRUCTS {
        let (documents, bound) = census
            .by_construct
            .get(construct)
            .map_or((0, 0), |row| (row.documents, row.bound_documents));
        assert!(
            documents >= min_documents,
            "{construct:?} appeared in only {documents} documents"
        );
        assert!(
            is_known_decline(*construct) || bound >= min_documents,
            "{construct:?} appeared in only {bound} documents carrying the must-commit \
             obligation, so it does not bind on it"
        );
    }
}

/// The grammar sweep at a fixed seed.
#[test]
fn generated_documents_are_exact_and_committed() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0xf5_9a0a_c0de);
    let census = sweep(&mut oracle, &mut rng, 240);
    report(&census);
    assert_obligations(&census, 10);
}

/// Fresh-seed twin of [`generated_documents_are_exact_and_committed`].
#[test]
#[ignore = "fresh-seed soak; run it when touching the evaluator"]
fn generated_documents_soak() {
    let (seed, cases) = soak_parameters("fsproj grammar", 3000);
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(seed);
    let census = sweep(&mut oracle, &mut rng, cases);
    report(&census);
    assert_obligations(&census, 10);
}
