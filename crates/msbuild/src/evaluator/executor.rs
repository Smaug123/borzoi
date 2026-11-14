//! Running targets on the evaluation: stage A₁.3c of
//! `docs/nuget-dgspec-boundary-plan.md`.
//!
//! The restore targets run on the evaluation MSBuild finishes with, and what
//! they leave is the restore graph. This module runs targets the walk retained
//! ([`super::reads::TargetDefinition`]) over a copy of the walk's end state,
//! and answers only when it reproduces MSBuild exactly. Anything it does not
//! model declines the whole run, with the reason.
//!
//! It models:
//! - the schedule: `DependsOnTargets` (expanded when the target comes up),
//!   `BeforeTargets` and `AfterTargets`, a target's `Condition` decided when
//!   the target comes up, and each target run at most once;
//! - intrinsic `PropertyGroup`s in a target body;
//! - intrinsic `ItemGroup` `Include`s without batching;
//! - the `Message` task, which changes nothing.
//!
//! Every read must be one the walk vouches for: a property it holds a trusted
//! value for or knows is unset, an item list it holds exactly. A read of
//! anything else declines.

use std::collections::{BTreeMap, HashMap, HashSet};

use roxmltree::{Document, Node};

use super::State;
use super::reads::PropertyRead;
use crate::condition;
use crate::properties::{self, Issue, PropertyMap, escaping};

/// Targets to run, and what to read once they have run.
#[derive(Debug, Clone, Default)]
pub struct ExecutionProbe {
    /// The targets the build is asked for, in order.
    pub targets: Vec<String>,
    /// Property names to read after the run.
    pub properties: Vec<String>,
    /// Item types to read after the run.
    pub item_types: Vec<String>,
}

/// The outcome of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionRead {
    /// The run, as MSBuild runs it.
    Ran {
        /// Every target that executed (its condition held), in order, by its
        /// declared name.
        targets: Vec<String>,
        /// Each probed property's value, `None` when unset.
        properties: BTreeMap<String, Option<String>>,
        /// Each probed item type's items, in order.
        items: BTreeMap<String, Vec<ExecutedItem>>,
    },
    /// The run reached something not modelled, or a value the walk does not
    /// vouch for.
    Declined(String),
}

/// One item after the run: its identity and every metadatum, names lowercase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutedItem {
    pub identity: String,
    pub metadata: BTreeMap<String, String>,
}

/// Run `probe` on the walk's end state.
pub(super) fn execute(state: &State<'_>, probe: &ExecutionProbe) -> ExecutionRead {
    match Engine::new(state).and_then(|mut engine| engine.run(probe)) {
        Ok(read) => read,
        Err(Decline(why)) => ExecutionRead::Declined(why),
    }
}

/// How deep a chain of targets bringing each other up may run before the run
/// declines. It bounds the recursion of the cycle
/// check; the SDK's restore targets reach a few dozen.
const MAX_DEPTH: usize = 256;

/// How many stack steps a run may take before it declines: a bound on a run
/// MSBuild itself may never finish.
const MAX_STEPS: usize = 100_000;

/// Why a target entry was pushed, as far as `PushTargets` distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    Requested,
    DependsOn,
    Before,
    After,
}

/// Where a target entry is in MSBuild's `TargetEntryState` sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryState {
    Dependencies,
    Execution,
    /// Done: `true` when it executed, `false` when its condition skipped it.
    Completed(bool),
}

/// One push of a target onto the stack.
struct Entry {
    key: String,
    /// The entry that pushed it as a dependency or to run before it.
    parent: Option<usize>,
    state: EntryState,
}

/// The prefix of the property a plain `@(Type)` is read through
/// ([`Engine::with_items`]): no name a project could write, since neither an
/// XML element name nor an environment variable promoted to a property can
/// start with `-`, though a reference reads it as a name.
const ITEM_LIST_PROPERTY: &str = "-ItemList-";

struct Decline(String);

/// A decline, its reason on one line: the expressions it quotes are read with
/// their whitespace kept ([`expression_attribute`]), and are shown as the XML
/// reader would give them, each tab, line break and CRLF a space.
fn decline<T>(why: impl Into<String>) -> Result<T, Decline> {
    let why: String = why.into();
    Err(Decline(
        why.replace("\r\n", " ").replace(['\t', '\n', '\r'], " "),
    ))
}

/// A target definition, parsed.
struct Target {
    name: String,
    xml: String,
}

impl Target {
    fn document(&self) -> Result<Document<'_>, Decline> {
        Document::parse(&self.xml)
            .or_else(|e| decline(format!("target {} does not parse alone: {e}", self.name)))
    }
}

struct Engine<'s, 'r> {
    state: &'s State<'r>,
    /// The property table as the run leaves it.
    lookup: PropertyMap,
    /// Names the run wrote, lowercase: their value is the run's.
    written: HashSet<String>,
    /// Item tables the run has read or written, by lowercase type.
    items: HashMap<String, Vec<ExecutedItem>>,
    /// The definition each name keeps, by lowercase name.
    targets: HashMap<String, Target>,
    /// For each lowercase target name, the targets that name it in
    /// `BeforeTargets` and in `AfterTargets`, in definition order.
    before: HashMap<String, Vec<String>>,
    after: HashMap<String, Vec<String>>,
    /// Each target's result so far, by lowercase name: `true` when it
    /// executed, `false` when its condition skipped it. A skipped target is
    /// decided again the next time it comes up; an executed one never runs
    /// again.
    results: HashMap<String, bool>,
    /// Every target entry pushed, by its index; and the stack of entries
    /// still to process, top last.
    entries: Vec<Entry>,
    stack: Vec<usize>,
    /// Targets that executed, in order.
    order: Vec<String>,
}

impl<'s, 'r> Engine<'s, 'r> {
    fn new(state: &'s State<'r>) -> Result<Self, Decline> {
        let mut engine = Engine {
            state,
            lookup: state.lookup.clone(),
            written: HashSet::new(),
            items: HashMap::new(),
            targets: HashMap::new(),
            before: HashMap::new(),
            after: HashMap::new(),
            results: HashMap::new(),
            entries: Vec::new(),
            stack: Vec::new(),
            order: Vec::new(),
        };
        // The definition each name keeps is its last, and it stands where that
        // last one does (MSBuild's `Evaluator.ReadTargetElement`).
        let mut kept: Vec<String> = Vec::new();
        for (name, _) in &state.target_definitions {
            let key = name.to_ascii_lowercase();
            kept.retain(|k| *k != key);
            kept.push(key.clone());
            if engine.targets.contains_key(&key) {
                continue;
            }
            let read = state.read_target(name);
            if !read.exact {
                return decline(format!("target {name} is not known exactly"));
            }
            let definition = read.definition.expect("a recorded target reads back");
            let declared = Document::parse(&definition.xml)
                .ok()
                .and_then(|d| d.root_element().attribute("Name").map(str::to_owned))
                .unwrap_or_else(|| name.clone());
            engine.targets.insert(
                key,
                Target {
                    name: declared,
                    xml: definition.xml,
                },
            );
        }
        // Each kept target, in that order, hooks onto the targets its
        // `BeforeTargets` and `AfterTargets` name, expanded against the
        // evaluation and kept only where that target exists
        // (`Evaluator.AddBeforeAndAfterTargetMappings`).
        for key in &kept {
            let xml = engine.targets[key].xml.clone();
            let name = engine.targets[key].name.clone();
            let document = Document::parse(&xml)
                .or_else(|e| decline(format!("target {name} does not parse alone: {e}")))?;
            let root = document.root_element();
            for (attribute, before) in [("BeforeTargets", true), ("AfterTargets", false)] {
                let Some(raw) = expression_attribute(root, attribute)? else {
                    continue;
                };
                let expanded = engine.expand(&raw)?;
                for named in expanded
                    .as_escaped()
                    .split(';')
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                {
                    let named = escaping::unescape(named).to_ascii_lowercase();
                    if !engine.targets.contains_key(&named) {
                        continue;
                    }
                    let map = if before {
                        &mut engine.before
                    } else {
                        &mut engine.after
                    };
                    map.entry(named).or_default().push(key.clone());
                }
            }
        }
        engine.check_trigger_cycles()?;
        Ok(engine)
    }

    /// Decline a graph in which a target can bring itself up again: the
    /// targets each one runs before or after it, and those it lists in a
    /// literal `DependsOnTargets`. MSBuild does not finish such a build
    /// (probed: two targets `AfterTargets` each other hang it).
    fn check_trigger_cycles(&self) -> Result<(), Decline> {
        let mut edges: HashMap<&str, Vec<String>> = HashMap::new();
        for (key, target) in &self.targets {
            let next = edges.entry(key.as_str()).or_default();
            next.extend(self.before.get(key).into_iter().flatten().cloned());
            next.extend(self.after.get(key).into_iter().flatten().cloned());
            let document = target.document()?;
            if let Some(raw) = expression_attribute(document.root_element(), "DependsOnTargets")?
                && !raw.contains('$')
            {
                next.extend(
                    raw.split(';')
                        .map(str::trim)
                        .filter(|d| !d.is_empty())
                        .map(str::to_ascii_lowercase),
                );
            }
        }
        // Depth-first, colouring each target on the path and each finished.
        // A path longer than [`MAX_DEPTH`] counts as a cycle: declining it
        // bounds the recursion.
        fn visit<'a>(
            key: &'a str,
            edges: &'a HashMap<&str, Vec<String>>,
            path: &mut HashSet<&'a str>,
            finished: &mut HashSet<&'a str>,
        ) -> bool {
            if finished.contains(key) {
                return false;
            }
            if path.len() > MAX_DEPTH || !path.insert(key) {
                return true;
            }
            let cyclic = edges
                .get(key)
                .into_iter()
                .flatten()
                .any(|next| visit(next, edges, path, finished));
            path.remove(key);
            finished.insert(key);
            cyclic
        }
        let mut finished = HashSet::new();
        for key in edges.keys() {
            if visit(key, &edges, &mut HashSet::new(), &mut finished) {
                return decline(format!("target {key} can bring itself up again"));
            }
        }
        Ok(())
    }

    fn run(&mut self, probe: &ExecutionProbe) -> Result<ExecutionRead, Decline> {
        if self.state.initial_targets_named {
            return decline("a file names InitialTargets");
        }
        // An empty request runs the default targets, which are not modelled.
        if probe.targets.is_empty() {
            return decline("the build asks for its default targets");
        }
        let requested: Vec<String> = probe.targets.clone();
        self.push(&requested, None, Reason::Requested)?;
        let mut steps = 0usize;
        while let Some(&top) = self.stack.last() {
            steps += 1;
            if steps > MAX_STEPS {
                return decline(format!("the run takes more than {MAX_STEPS} steps"));
            }
            self.step(top)?;
        }
        let mut properties = BTreeMap::new();
        for name in &probe.properties {
            self.check_read(name)?;
            properties.insert(name.clone(), self.lookup.get_unescaped(name));
        }
        let mut items = BTreeMap::new();
        for item_type in &probe.item_types {
            items.insert(item_type.clone(), self.table(item_type)?.clone());
        }
        Ok(ExecutionRead::Ran {
            targets: self.order.clone(),
            properties,
            items,
        })
    }

    /// Whether `key` already has a result that is not a skip: such a target
    /// never comes up again.
    fn built(&self, key: &str) -> bool {
        self.results.get(key) == Some(&true)
    }

    /// Process the entry on top of the stack, as MSBuild's
    /// `TargetBuilder.ProcessTargetStack` does.
    fn step(&mut self, top: usize) -> Result<(), Decline> {
        let key = self.entries[top].key.clone();
        match self.entries[top].state {
            EntryState::Dependencies => {
                if self.built(&key) {
                    self.stack.pop();
                    return Ok(());
                }
                if !self.targets.contains_key(&key) {
                    return decline(format!("target {key} is not defined"));
                }
                // The targets that run after it go beneath it, then it, then
                // the targets that run before it, then its dependencies on top.
                self.stack.pop();
                let parent = self.entries[top].parent;
                let after = self.after.get(&key).cloned().unwrap_or_default();
                self.push(&after, parent, Reason::After)?;
                self.stack.push(top);
                let dependencies = self.dependencies(top)?;
                let before = self.before.get(&key).cloned().unwrap_or_default();
                self.push(&before, Some(top), Reason::Before)?;
                self.push(&dependencies, Some(top), Reason::DependsOn)?;
            }
            EntryState::Execution => {
                if self.built(&key) {
                    self.stack.pop();
                    return Ok(());
                }
                let target = &self.targets[&key];
                let declared = target.name.clone();
                let xml = target.xml.clone();
                let document = Document::parse(&xml)
                    .or_else(|e| decline(format!("target {declared} does not parse alone: {e}")))?;
                self.order.push(declared.clone());
                self.body(document.root_element(), &declared)?;
                self.check_returns(document.root_element(), &declared)?;
                self.entries[top].state = EntryState::Completed(true);
            }
            EntryState::Completed(executed) => {
                self.stack.pop();
                // A skip never replaces an execution.
                if executed || !self.results.contains_key(&key) {
                    self.results.insert(key, executed);
                }
            }
        }
        Ok(())
    }

    /// Decide the target of entry `entry` and return its dependencies, as
    /// MSBuild's `TargetEntry.GetDependencies` does: a false condition
    /// completes the entry as skipped, with none.
    fn dependencies(&mut self, entry: usize) -> Result<Vec<String>, Decline> {
        let key = self.entries[entry].key.clone();
        let target = &self.targets[&key];
        let declared = target.name.clone();
        let xml = target.xml.clone();
        let document = Document::parse(&xml)
            .or_else(|e| decline(format!("target {declared} does not parse alone: {e}")))?;
        let root = document.root_element();
        // `Inputs`/`Outputs` decide whether the target is up to date, from the
        // file system; `KeepDuplicateOutputs` is a condition MSBuild evaluates
        // once the target has run. (`Returns` is checked then too: see
        // [`Self::check_returns`].)
        for unmodelled in ["Inputs", "Outputs", "KeepDuplicateOutputs"] {
            if root.attribute(unmodelled).is_some() {
                return decline(format!("target {declared} has {unmodelled}"));
            }
        }
        if !self.decide(root)? {
            self.entries[entry].state = EntryState::Completed(false);
            return Ok(Vec::new());
        }
        self.entries[entry].state = EntryState::Execution;
        let Some(raw) = expression_attribute(root, "DependsOnTargets")? else {
            return Ok(Vec::new());
        };
        let expanded = self.expand(&raw)?;
        Ok(expanded
            .as_escaped()
            .split(';')
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(|d| escaping::unescape(d).to_ascii_lowercase())
            .collect())
    }

    /// Push entries for `targets` (lowercase names) so that the first ends on
    /// top, as MSBuild's `TargetBuilder.PushTargets` does: a target that
    /// already executed is not pushed to run before or after another; one
    /// already anywhere on the stack is not pushed again to run after
    /// another; and one already in `parent`'s chain, pushed as a dependency or
    /// to run before, is a circular dependency, which fails the build.
    fn push(
        &mut self,
        targets: &[String],
        parent: Option<usize>,
        reason: Reason,
    ) -> Result<(), Decline> {
        let mut pushing = Vec::new();
        for target in targets.iter().rev() {
            let key = target.to_ascii_lowercase();
            if matches!(reason, Reason::Before | Reason::After) && self.built(&key) {
                continue;
            }
            if reason == Reason::After {
                if self.stack.iter().any(|&e| self.entries[e].key == key) {
                    continue;
                }
            } else {
                let mut ancestor = parent;
                while let Some(index) = ancestor {
                    if self.entries[index].key == key {
                        return decline(format!("target {key} depends on itself"));
                    }
                    ancestor = self.entries[index].parent;
                }
            }
            self.entries.push(Entry {
                key,
                parent,
                state: EntryState::Dependencies,
            });
            pushing.push(self.entries.len() - 1);
        }
        self.stack.extend(pushing);
        Ok(())
    }

    /// A target's `Returns` shapes only what it hands a caller, and no caller
    /// is modelled. But MSBuild expands it once the target has run, against
    /// the properties the target left, and an expression there can fail the
    /// build: a property function, a transform, an item list run together
    /// with text (`prefix@(I)`, `@(I)@(I)`). So each `;`-separated piece must
    /// be a lone plain `@(Type)`, or text with plain `$(Name)` references and
    /// no `@`, and no value it reads may hold what could assemble more.
    fn check_returns(&self, root: Node<'_, '_>, target: &str) -> Result<(), Decline> {
        let Some(returns) = attribute(root, "Returns")? else {
            return Ok(());
        };
        let declined = || decline(format!("target {target} returns {returns}"));
        if references_private(&returns) {
            return declined();
        }
        if returns.contains("%(") {
            return declined();
        }
        let Some(names) = plain_references(&returns) else {
            return declined();
        };
        for name in names {
            self.check_read(name)?;
            if self
                .lookup
                .get(name)
                .is_some_and(|value| value.as_escaped().contains(['@', '%', '$', ';']))
            {
                return declined();
            }
        }
        for piece in returns.split(';').map(str::trim) {
            if piece.contains('@') && !lone_item_reference(piece) {
                return declined();
            }
        }
        Ok(())
    }

    fn body(&mut self, root: Node<'_, '_>, target: &str) -> Result<(), Decline> {
        for child in root.children().filter(Node::is_element) {
            match child.tag_name().name() {
                "PropertyGroup" => self.property_group(child)?,
                "ItemGroup" => self.item_group(child)?,
                "Message" => self.message(child)?,
                // A task whose condition is false does not run: MSBuild neither
                // binds it nor reads its parameters.
                other => {
                    // MSBuild looks for batching in a task's parameters before
                    // its condition, and fails on batching it cannot do.
                    if task_batches(child) {
                        return decline(format!("target {target}'s {other} batches"));
                    }
                    if self.decide(child)? {
                        return decline(format!("target {target} runs {other}"));
                    }
                }
            }
        }
        Ok(())
    }

    /// The `Message` task writes nothing, but it fails the build on a
    /// parameter it rejects (an unknown `Importance`), so only the shapes it
    /// accepts run.
    fn message(&mut self, task: Node<'_, '_>) -> Result<(), Decline> {
        if self
            .state
            .using_task_names
            .iter()
            .any(|name| may_bind(name, "Message"))
        {
            return decline("a UsingTask may rebind Message");
        }
        for attr in task.attributes() {
            if !matches!(attr.name(), "Text" | "Importance" | "Condition") {
                return decline(format!("Message has {}", attr.name()));
            }
        }
        if task.children().any(|c| c.is_element()) {
            return decline("Message has a child element");
        }
        if !self.decide(task)? {
            return Ok(());
        }
        if let Some(raw) = attribute(task, "Text")? {
            self.expand(&raw)?;
        }
        if let Some(raw) = attribute(task, "Importance")? {
            let importance = escaping::unescape(self.expand(&raw)?.as_escaped());
            if !["high", "normal", "low"]
                .iter()
                .any(|known| importance.trim().eq_ignore_ascii_case(known))
            {
                return decline(format!("Message has Importance {importance}"));
            }
        }
        Ok(())
    }

    fn property_group(&mut self, group: Node<'_, '_>) -> Result<(), Decline> {
        if !self.decide(group)? {
            return Ok(());
        }
        for property in group.children().filter(Node::is_element) {
            if !self.decide(property)? {
                continue;
            }
            let name = property.tag_name().name();
            let lower = name.to_ascii_lowercase();
            if lower.starts_with("msbuild")
                || self.state.protected.contains(&lower)
                || self.state.is_sticky_global(name)
            {
                return decline(format!("a target writes the reserved or global {name}"));
            }
            let raw = element_text(property)?;
            let value = self.expand(&raw)?;
            self.lookup.insert_escaped(name, value);
            self.written.insert(lower);
        }
        Ok(())
    }

    fn item_group(&mut self, group: Node<'_, '_>) -> Result<(), Decline> {
        if !self.decide(group)? {
            return Ok(());
        }
        for item in group.children().filter(Node::is_element) {
            let item_type = item.tag_name().name();
            let Some(include) = attribute(item, "Include")? else {
                return decline(format!(
                    "a target changes {item_type} other than by Include"
                ));
            };
            if !self.decide(item)? {
                continue;
            }
            // The type's item-definition defaults go beneath everything else,
            // and the element's own metadata on top.
            let definitions = self.definitions(item_type)?;
            let mut metadata = BTreeMap::new();
            for attr in item.attributes() {
                match attr.name() {
                    // Structural, not metadata.
                    "Include" | "Condition" | "Label" => {}
                    "Exclude"
                    | "Remove"
                    | "Update"
                    | "KeepMetadata"
                    | "RemoveMetadata"
                    | "KeepDuplicates"
                    | "MatchOnMetadata"
                    | "MatchOnMetadataOptions" => {
                        return decline(format!("a target's {item_type} has {}", attr.name()));
                    }
                    name => {
                        let raw = attribute(item, name)?.expect("an attribute present");
                        let value = self.metadata_value(&raw)?;
                        metadata.insert(name.to_ascii_lowercase(), value);
                    }
                }
            }
            for child in item.children().filter(Node::is_element) {
                if !self.decide(child)? {
                    continue;
                }
                let raw = element_text(child)?;
                let value = self.metadata_value(&raw)?;
                metadata.insert(child.tag_name().name().to_ascii_lowercase(), value);
            }
            // Properties first, item references left in place; then each
            // `;`-separated piece. One that is a lone plain `@(Type)` copies
            // those items, metadata and all; any other item reference declines,
            // as does one properties assembled.
            let substituted = self.substitute_keeping_items(&include)?;
            let mut added = Vec::new();
            for fragment in substituted.split(';') {
                let fragment = fragment.trim();
                if fragment.is_empty() {
                    continue;
                }
                if lone_item_reference(fragment) {
                    let source = fragment[2..fragment.len() - 1].trim().to_owned();
                    let copies: Vec<ExecutedItem> = self.table(&source)?.clone();
                    for copy in copies {
                        let mut carried = definitions.clone();
                        carried.extend(copy.metadata);
                        carried.extend(metadata.clone());
                        added.push(ExecutedItem {
                            identity: copy.identity,
                            metadata: carried,
                        });
                    }
                    continue;
                }
                if fragment.contains("@(") {
                    return decline(format!("a target includes {fragment}"));
                }
                if escaping::filespec_has_wildcards(fragment) {
                    return decline(format!("a target includes the wildcard {fragment}"));
                }
                // MSBuild's Unix path fixup may respell a backslash.
                if fragment.contains('\\') {
                    return decline(format!("a target includes {fragment}, with a backslash"));
                }
                let mut carried = definitions.clone();
                carried.extend(metadata.clone());
                added.push(ExecutedItem {
                    identity: escaping::unescape(fragment),
                    metadata: carried,
                });
            }
            self.table(item_type)?.extend(added);
        }
        Ok(())
    }

    /// A metadatum's value: `raw` expanded and unescaped. A backslash is one
    /// MSBuild's Unix path fixup may respell, so it declines.
    fn metadata_value(&mut self, raw: &str) -> Result<String, Decline> {
        let value = self.expand(raw)?;
        if value.as_escaped().contains('\\') {
            return decline(format!("a target's metadatum {raw} holds a backslash"));
        }
        Ok(escaping::unescape(value.as_escaped()))
    }

    /// The item-definition defaults of `item_type`, as the evaluation leaves
    /// them, or decline when one is unknown.
    fn definitions(&self, item_type: &str) -> Result<BTreeMap<String, String>, Decline> {
        let mut defaults = BTreeMap::new();
        let Some(values) = self
            .state
            .item_definition_values
            .get(&item_type.to_ascii_lowercase())
        else {
            return Ok(defaults);
        };
        for (name, value) in values {
            let Some(value) = value else {
                return decline(format!(
                    "an item definition of {item_type} gives {name} a default the walk does not know"
                ));
            };
            defaults.insert(name.clone(), value.clone());
        }
        Ok(defaults)
    }

    /// The table of `item_type`, read from the walk the first time.
    fn table(&mut self, item_type: &str) -> Result<&mut Vec<ExecutedItem>, Decline> {
        let key = item_type.to_ascii_lowercase();
        if !self.items.contains_key(&key) {
            let read = self.state.read_items(item_type);
            let rows = match (&read.items, read.exact) {
                (Some(rows), true) => rows,
                _ => return decline(format!("the walk does not hold {item_type} exactly")),
            };
            let mut items = Vec::new();
            for row in rows {
                if !row.metadata_complete || !row.uncertain_metadata.is_empty() {
                    return decline(format!(
                        "an item of {item_type} has metadata the walk does not know"
                    ));
                }
                items.push(ExecutedItem {
                    identity: row.identity.clone(),
                    metadata: row.metadata.clone(),
                });
            }
            self.items.insert(key.clone(), items);
        }
        Ok(self.items.get_mut(&key).expect("just inserted"))
    }

    /// Decide `node`'s `Condition` (true when it has none).
    fn decide(&mut self, node: Node<'_, '_>) -> Result<bool, Decline> {
        let Some(raw) = expression_attribute(node, "Condition")? else {
            return Ok(true);
        };
        let raw = self.with_items(&raw, true)?;
        self.check_references(&raw)?;
        if holds_reference(properties::substitute(&raw, &self.lookup).0.as_escaped()) {
            return decline(format!(
                "the condition {raw} expands to an item or metadata reference"
            ));
        }
        match condition::evaluate(&raw, &self.lookup).outcome {
            condition::Outcome::True => Ok(true),
            condition::Outcome::False => Ok(false),
            condition::Outcome::Unsupported => {
                decline(format!("the condition {raw} is not modelled"))
            }
        }
    }

    /// Expand the `$(…)` and plain `@(Type)` in `raw` against the run's
    /// tables.
    fn expand(&mut self, raw: &str) -> Result<escaping::Escaped, Decline> {
        let raw = self.with_items(raw, false)?;
        self.substitute(&raw)
    }

    /// Expand the `$(…)` in `raw`, leaving its item references in place for
    /// the caller to split on: every one must be in `raw` itself, not
    /// assembled by a property.
    fn substitute_keeping_items(&self, raw: &str) -> Result<String, Decline> {
        if references_private(raw) {
            return decline(format!("{raw} reads a property no project can write"));
        }
        if raw.contains("%(") {
            return decline(format!("{raw} reads metadata"));
        }
        for name in crate::properties::property_references(raw) {
            self.check_read(name)?;
            if self
                .lookup
                .get(name)
                .is_some_and(|value| may_assemble(value.as_escaped()))
            {
                return decline(format!("{name} may assemble an item reference"));
            }
        }
        let (value, issues) = properties::substitute(raw, &self.lookup);
        if issues
            .iter()
            .any(|issue| !matches!(issue, Issue::Undefined { .. }))
        {
            return decline(format!("{raw} does not expand cleanly"));
        }
        Ok(value.as_escaped().to_owned())
    }

    /// Expand the `$(…)` in `raw`, which holds no item reference.
    fn substitute(&self, raw: &str) -> Result<escaping::Escaped, Decline> {
        self.check_references(raw)?;
        let (value, issues) = properties::substitute(raw, &self.lookup);
        if issues
            .iter()
            .any(|issue| !matches!(issue, Issue::Undefined { .. }))
        {
            return decline(format!("{raw} does not expand cleanly"));
        }
        // Properties can assemble an item or metadata reference, which MSBuild
        // then expands as such.
        if holds_reference(value.as_escaped()) {
            return decline(format!("{raw} expands to an item or metadata reference"));
        }
        Ok(value)
    }

    /// `raw` with each plain `@(Type)` read as a property holding the list as
    /// MSBuild expands it: each identity escaped, joined by `;`. An item
    /// expression that is more (a transform, a separator) declines.
    fn with_items(&mut self, raw: &str, condition: bool) -> Result<String, Decline> {
        if references_private(raw) {
            return decline(format!("{raw} reads a property no project can write"));
        }
        // MSBuild expands properties, functions included, before items, so a
        // function receives an item reference in its arguments as text.
        if item_reference_in_property(raw) {
            return decline(format!(
                "{raw} passes an item reference to a property function"
            ));
        }
        // The tables hold identities decoded, and MSBuild expands a list from
        // their escaped spellings, which decoding loses. In a value that
        // spelling is what the value holds, and a later read may run it into a
        // `%` (with `J` spelt `%34%31`, `%@(J)` reads `%41`, not `A`); a
        // condition compares decoded text, where only what the reference runs
        // into can tell spellings apart. So a value declines any item list,
        // and a condition one not bounded by quotes, parentheses, whitespace
        // or its ends.
        if !condition && raw.contains("@(") {
            return decline(format!(
                "{raw} reads an item list into a value, whose escaped spelling the tables do not keep"
            ));
        }
        let mut out = String::new();
        let mut rest = raw;
        while let Some(at) = rest.find("@(") {
            let before = rest[..at].chars().next_back();
            out.push_str(&rest[..at]);
            let interior = &rest[at + 2..];
            let Some(close) = interior.find(')') else {
                return decline(format!("{raw} has an unclosed item reference"));
            };
            let after = interior[close + 1..].chars().next();
            let bounded = |c: Option<char>, opening: bool| {
                c.is_none_or(|c| {
                    c.is_whitespace() || c == '\'' || c == if opening { '(' } else { ')' }
                })
            };
            if !bounded(before, true) || !bounded(after, false) {
                return decline(format!("{raw} runs an item list into other text"));
            }
            let item_type = interior[..close].trim();
            if !item_name(item_type) {
                return decline(format!("{raw} has an item expression"));
            }
            // The tables hold identities decoded, and MSBuild expands the list
            // from the escaped spelling, which decoding loses. Only an identity
            // every spelling of which reads the same wherever it lands passes.
            let table = self.table(item_type)?;
            if let Some(item) = table.iter().find(|item| !respellable(&item.identity)) {
                return decline(format!(
                    "an item of {item_type}, {:?}, may be spelt otherwise",
                    item.identity
                ));
            }
            let list = table
                .iter()
                .map(|item| escaping::escape(&item.identity))
                .collect::<Vec<_>>()
                .join(";");
            let name = format!("{ITEM_LIST_PROPERTY}{item_type}");
            self.lookup
                .insert_escaped(&name, escaping::Escaped::from_xml(list));
            self.written.insert(name.to_ascii_lowercase());
            out.push_str(&format!("$({name})"));
            rest = &interior[close + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    /// Every property `raw` reads, a function's receiver or one in its
    /// arguments, is one the run may read. Which functions run is the walk's
    /// expression evaluator's business: what it does not model it reports,
    /// and the run declines ([`Self::substitute`], [`Self::decide`]).
    fn check_references(&self, raw: &str) -> Result<(), Decline> {
        if raw.contains("@(") || raw.contains("%(") {
            return decline(format!("{raw} reads metadata or an item expression"));
        }
        for name in crate::properties::property_references(raw) {
            self.check_read(name)?;
        }
        Ok(())
    }

    /// The run may read `name`: it wrote it, or the walk vouches for it.
    fn check_read(&self, name: &str) -> Result<(), Decline> {
        let lower = name.to_ascii_lowercase();
        // A value that holds an item or metadata reference is one MSBuild
        // expands again as such once properties are in (`$(A)$(B)` with
        // `A=@`, `B=(I)` reads `@(I)`).
        if self
            .lookup
            .get(name)
            .is_some_and(|value| holds_reference(value.as_escaped()))
        {
            return decline(format!("{name} holds an item or metadata reference"));
        }
        if self.written.contains(&lower) {
            return Ok(());
        }
        // Within a target these name the file that defines it.
        if lower.starts_with("msbuildthisfile") {
            return decline(format!("a target reads {name}"));
        }
        match self.state.read_property(name) {
            PropertyRead::Value(_) | PropertyRead::Unset => Ok(()),
            other => decline(format!(
                "a target reads {name}, which the walk holds as {other:?}"
            )),
        }
    }
}

/// The names of `raw`'s `$(…)` references, or `None` when one is more than a
/// plain name (a property function, a static call).
fn plain_references(raw: &str) -> Option<Vec<&str>> {
    let mut names = Vec::new();
    let mut rest = raw;
    while let Some(at) = rest.find("$(") {
        let interior = &rest[at + 2..];
        let close = interior.find(')')?;
        let name = interior[..close].trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return None;
        }
        names.push(name);
        rest = &interior[close + 1..];
    }
    Some(names)
}

/// The text of `element` as MSBuild reads it ([`super::collect_element_text`]),
/// or decline when MSBuild reads it otherwise than as its source spells it: a
/// body holding more than text and comments (which MSBuild takes as inner
/// XML), or text that line-ending normalisation or entity decoding touched.
fn element_text(element: Node<'_, '_>) -> Result<String, Decline> {
    if !super::item_pass::body_is_plain(element) {
        return decline(format!(
            "the body of {} is not plain text",
            element.tag_name().name()
        ));
    }
    super::collect_element_text(element).map_or_else(
        || {
            decline(format!(
                "the body of {} is not modelled",
                element.tag_name().name()
            ))
        },
        Ok,
    )
}

/// The value of `node`'s attribute `name`, or decline when XML attribute-value
/// normalisation or entity decoding touched it: MSBuild reads such a value
/// its own way (a literal tab survives).
fn attribute(node: Node<'_, '_>, name: &str) -> Result<Option<String>, Decline> {
    let Some(attr) = node.attributes().find(|a| a.name() == name) else {
        return Ok(None);
    };
    if !super::item_pass::attribute_is_plain(node, &attr) {
        return decline(format!("the attribute {name} is not its source verbatim"));
    }
    Ok(Some(attr.value().to_owned()))
}

/// Whether a `UsingTask` of `task_name` may bind the task an element names
/// `name`. MSBuild matches a task by its name or by the last segment of a
/// qualified one, a namespace (`X.Message`) or a nested type
/// (`Outer+Message`), case-insensitively. Any name ending in `name` counts,
/// which over-approximates that, and so does a name it cannot read.
fn may_bind(task_name: &str, name: &str) -> bool {
    let task_name = task_name.trim();
    task_name.contains(['$', '@', '%'])
        || task_name
            .to_ascii_lowercase()
            .ends_with(&name.to_ascii_lowercase())
}

/// Whether escaped text holds an item or metadata reference.
fn holds_reference(escaped: &str) -> bool {
    escaped.contains("@(") || escaped.contains("%(")
}

/// Whether `piece` is exactly one plain `@(Type)` reference.
fn lone_item_reference(piece: &str) -> bool {
    piece
        .strip_prefix("@(")
        .and_then(|rest| rest.strip_suffix(')'))
        .map(str::trim)
        .is_some_and(item_name)
}

/// Whether `name` is an item type name MSBuild expands in `@(…)`: a letter or
/// `_`, then letters, digits, `_` or `-`. (`@(0)` and `@(-J)` stay literal
/// text, which the executor declines.)
fn item_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether `raw` references a property whose name starts with `-`: one no
/// project can write, which MSBuild reads as unset and the executor uses for
/// its item lists ([`ITEM_LIST_PROPERTY`]).
fn references_private(raw: &str) -> bool {
    crate::properties::property_references(raw).any(|name| name.starts_with('-'))
}

/// Whether escaped text could join with what sits beside it into an item or
/// metadata reference: it holds `@` or `%(`, or ends in `%`. An escape such as
/// `%3b` cannot.
fn may_assemble(escaped: &str) -> bool {
    escaped.contains('@') || escaped.contains("%(") || escaped.ends_with('%')
}

/// Whether every escaped spelling of a decoded item identity reads the same
/// wherever an expansion puts it. It must have no padding, which MSBuild keeps
/// escaped and a later split would trim. And it must hold none of the
/// characters whose raw and escaped spellings differ once joined with what
/// sits beside them: `%` can form a new escape, and `@`, `$`, `(`, `)` can join
/// an expression a later read expands, as can `*`, `?` a wildcard and `'` a
/// quote. A `;` is not among them: an identity holding one was spelt with it
/// escaped (raw, it would have split), as re-escaping spells it.
fn respellable(identity: &str) -> bool {
    identity.trim() == identity && !identity.contains(['%', '*', '?', '@', '$', '(', ')', '\''])
}

/// The quote characters of MSBuild's expressions: a condition's `'`, and the
/// `'`, `"` and `` ` `` a property function's string arguments may use.
const QUOTES: [char; 3] = ['\'', '"', '`'];

/// `node`'s attribute `name` holding an expression (a condition, a target
/// list), as MSBuild reads it: the source text with its entities decoded and
/// its whitespace kept. MSBuild normalises neither attribute values nor line
/// ends (probed: a quoted CRLF reaches a property function as two characters,
/// a lone CR as one), so a tab, line feed or carriage return reaches the
/// expression as itself, where the XML reader's value has a space for each
/// (and for each CRLF); that value serves only to check that this is the one
/// difference. A character reference or an entity other than XML's five
/// declines. (The walk still reads its conditions normalised: #750.)
fn expression_attribute(node: Node<'_, '_>, name: &str) -> Result<Option<String>, Decline> {
    let Some(attr) = node.attributes().find(|a| a.name() == name) else {
        return Ok(None);
    };
    let raw = node
        .document()
        .input_text()
        .get(attr.range_value())
        .unwrap_or_default();
    let Some(text) = decode_predefined_entities(raw) else {
        return decline(format!(
            "the {name} {raw} holds a character reference or an entity of its own"
        ));
    };
    if text.replace("\r\n", "\n").replace(['\t', '\n', '\r'], " ") != attr.value() {
        return decline(format!(
            "the {name} {raw} reads apart from the XML reader's value"
        ));
    }
    Ok(Some(text))
}

/// `raw` with XML's five predefined entities decoded; `None` for any other
/// `&`, a character reference among them.
fn decode_predefined_entities(raw: &str) -> Option<String> {
    const ENTITIES: [(&str, char); 5] = [
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&amp;", '&'),
        ("&quot;", '"'),
        ("&apos;", '\''),
    ];
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let (entity, c) = ENTITIES.iter().find(|(e, _)| rest.starts_with(e))?;
        out.push(*c);
        rest = &rest[entity.len()..];
    }
    out.push_str(rest);
    Some(out)
}

/// Whether an `@(` in `raw` sits inside a `$(…)` (a property function's
/// receiver or arguments). Parentheses count from each `$(`, except inside a
/// quoted argument, and an `@(` anywhere before the `$(…)` closes counts,
/// quoted or not.
fn item_reference_in_property(raw: &str) -> bool {
    let chars: Vec<char> = raw.chars().collect();
    let mut open: Vec<usize> = Vec::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if !open.is_empty() && c == '@' && next == Some('(') {
            return true;
        }
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if !open.is_empty() && QUOTES.contains(&c) => quote = Some(c),
            None if c == '$' && next == Some('(') => {
                open.push(1);
                i += 2;
                continue;
            }
            None if c == '(' => {
                if let Some(depth) = open.last_mut() {
                    *depth += 1;
                }
            }
            None if c == ')' => {
                if let Some(depth) = open.last_mut() {
                    *depth -= 1;
                    if *depth == 0 {
                        open.pop();
                    }
                }
            }
            None => {}
        }
        i += 1;
    }
    false
}

/// Whether a task element's parameters or children mention metadata, which
/// MSBuild batches on.
fn task_batches(task: Node<'_, '_>) -> bool {
    task.descendants().any(|node| {
        node.attributes().any(|a| a.value().contains("%("))
            || (node.is_text() && node.text().is_some_and(|t| t.contains("%(")))
    })
}
