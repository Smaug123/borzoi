//! References for the walks that ask a question at every enclosing
//! container — [`Resolver::head_contested_by_project_module`],
//! [`Resolver::module_like_transparent_for`],
//! [`Resolver::open_tier_candidates`] (as its two consumers read it),
//! [`Resolver::resolved_project_module`] and
//! [`Resolver::cross_file_type_case_located`]: each container's completion
//! built and looked up afresh, as plainly as the rule reads, at a cost
//! quadratic in the nesting. The fast versions ask every enclosing container
//! in one walk along the container path. In this crate's unit tests, every
//! call to them is checked against these, and
//! [`fast_head_contest_agrees_with_the_reference`] and
//! [`fast_open_and_case_walks_agree_with_their_references`] drive that over
//! generated projects.

use std::cell::Cell;

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

use super::super::state::{CaseTier, CrossFileCase, OpenInterpretation};
use super::{OpenTierCandidates, Resolver};

thread_local! {
    /// What the checked calls have seen on this thread, for the property
    /// test's census.
    static CENSUS: Cell<Census> = const { Cell::new(Census::new()) };
}

/// How many checked calls of [`Resolver::head_contested_by_project_module`]
/// had each outcome.
#[derive(Clone, Copy, Debug)]
struct Census {
    calls: usize,
    contested: usize,
    /// Uncontested although a module of the head's name stands at some
    /// completion: transparency decided it.
    passed_a_module: usize,
    /// Of those, passed a module that is not the walk's own container or one
    /// enclosing it: a same-file module that cannot own the residual.
    passed_beside: usize,
    /// Contested through an `open`'s completion and no written one.
    contested_by_open: usize,
    /// Asked under three or more containers.
    deep: usize,
    /// Checked `open`s.
    opens: usize,
    /// Of those, ones that left an enclosing container's completion unlisted.
    opens_with_unlisted: usize,
    /// Of those, ones under a container the assemblies reach into.
    opens_reached: usize,
    /// `open`s read as an assembly module at an enclosing container.
    enclosing_assembly_modules: usize,
    /// `open`s whose dropped-path hazard only an unlisted completion raised.
    dropped_by_unlisted: usize,
    /// `open`s whose dropped-path hazard a listed candidate raised.
    dropped_by_listed: usize,
    /// Project modules resolved at an enclosing container.
    enclosing_project_modules: usize,
    /// Cross-file cases found at an enclosing container.
    enclosing_cases: usize,
    /// Cross-file case searches that missed.
    case_misses: usize,
}

impl Census {
    const fn new() -> Self {
        Census {
            calls: 0,
            contested: 0,
            passed_a_module: 0,
            passed_beside: 0,
            contested_by_open: 0,
            deep: 0,
            opens: 0,
            opens_with_unlisted: 0,
            opens_reached: 0,
            enclosing_assembly_modules: 0,
            dropped_by_unlisted: 0,
            dropped_by_listed: 0,
            enclosing_project_modules: 0,
            enclosing_cases: 0,
            case_misses: 0,
        }
    }
}

impl Resolver<'_> {
    /// Whether `prefix + [head]` is a module that could contest a dotted head.
    fn reference_is_contesting_module(&self, path: &[String]) -> bool {
        self.module_paths
            .contains_path(self.assemblies.names(), path)
            || self
                .real_nested_module_exports
                .contains_path(self.assemblies.names(), path)
            || self
                .preceding
                .is_exact_project_module(self.assemblies.names(), self.path_key(path))
            || self
                .preceding
                .is_real_nested_module(self.assemblies.names(), self.path_key(path))
    }

    /// [`Self::module_like_transparent_for`], with the module's path built
    /// for every question.
    pub(super) fn reference_module_like_transparent_for(
        &self,
        prefix: &[String],
        name: &str,
        member: &str,
    ) -> bool {
        let mut mp = prefix.to_vec();
        mp.push(name.to_string());
        if self.container_path.starts_with(&mp) {
            return true;
        }
        if self.scope.recursive_module_active {
            return false;
        }
        let Some(&kinds) = self.declared_in(prefix).and_then(|d| d.get(name)) else {
            return false;
        };
        if !kinds.module || kinds.alias {
            return false;
        }
        if self
            .preceding
            .is_exact_project_module(self.assemblies.names(), self.path_key(&mp))
            || self
                .preceding
                .is_exact_nested_module(self.assemblies.names(), self.path_key(&mp))
            || self.is_project_namespace_path(&mp)
            || !self.assemblies_provably_lack_module_path(&mp)
            || self.module_has_hidden_values(&mp)
        {
            return false;
        }
        !self
            .declared_in(&mp)
            .is_some_and(|d| d.contains_key(member))
    }

    /// [`Self::head_contested_by_project_module`] over every completion's
    /// path, each built afresh: the written ones (the enclosing containers
    /// from the namespace depth in, and the root), then the opened ones.
    fn reference_head_contested(&self, head: &str, residual: &str) -> bool {
        let relative = (self.namespace_depth.max(1)..=self.container_path.len()).rev();
        let written = relative
            .chain(std::iter::once(0))
            .map(|k| (self.container_path[..k].to_vec(), false));
        let opened = self
            .scope
            .explicit_open_prefixes
            .iter()
            .map(|open| (open.path.clone(), true));
        let mut passed_a_module = false;
        let mut passed_beside = false;
        let mut contested_by = None;
        for (prefix, is_open) in written.chain(opened) {
            let mut path = prefix.clone();
            path.push(head.to_string());
            let module = self.reference_is_contesting_module(&path);
            let own = self.container_path.starts_with(&path);
            if self.reference_module_like_transparent_for(&prefix, head, residual) || own {
                passed_a_module |= module;
                passed_beside |= module && !own;
                continue;
            }
            if module && contested_by.is_none() {
                contested_by = Some(is_open);
            }
        }
        CENSUS.with(|c| {
            let mut census = c.get();
            census.calls += 1;
            census.contested += usize::from(contested_by.is_some());
            census.passed_a_module += usize::from(contested_by.is_none() && passed_a_module);
            census.passed_beside += usize::from(contested_by.is_none() && passed_beside);
            census.contested_by_open += usize::from(contested_by == Some(true));
            census.deep += usize::from(self.container_path.len() >= 3);
            c.set(census);
        });
        contested_by.is_some()
    }

    /// Assert that the fast answer `contested` is the reference's.
    pub(super) fn check_head_contested(&self, head: &str, residual: &str, contested: bool) {
        let reference = self.reference_head_contested(head, residual);
        assert_eq!(
            contested,
            reference,
            "head_contested_by_project_module({head}, {residual}) under {:?} (namespace depth \
             {}, opens {:?})",
            &self.container_path[..],
            self.namespace_depth,
            self.scope.explicit_open_prefixes
        );
    }

    /// [`Self::open_tier_candidates`] listing every enclosing container's
    /// completion.
    fn reference_open_tier_candidates(
        &self,
        written: &[String],
        rooted: bool,
    ) -> Vec<(Vec<String>, bool)> {
        let mut out: Vec<(Vec<String>, bool)> = Vec::new();
        if !rooted {
            if let Some((head, rest)) = written.split_first()
                && let Some(target) = self.lexical_alias_target(head)
            {
                let mut full = target.to_vec();
                full.extend_from_slice(rest);
                out.push((full, false));
            }
            for prefix in self.scope.open_shortening_prefixes.iter().rev() {
                for auto in self.auto_open_shortening_prefixes(prefix) {
                    let mut full = auto;
                    full.extend_from_slice(written);
                    out.push((full, false));
                }
                let mut full = prefix.path.clone();
                full.extend_from_slice(written);
                out.push((full, true));
            }
            for k in (self.namespace_depth.max(1)..=self.container_path.len()).rev() {
                let mut full = self.container_path[..k].to_vec();
                full.extend_from_slice(written);
                out.push((full, k == self.namespace_depth));
            }
        }
        out.push((written.to_vec(), true));
        out
    }

    /// Assert that `open <written>`'s interpretations `fast` are what the
    /// reference candidates read as.
    pub(super) fn check_open_interpretations(
        &self,
        written: &[String],
        rooted: bool,
        fast: &[OpenInterpretation],
    ) {
        let reference =
            self.interpretations_of(self.reference_open_tier_candidates(written, rooted));
        assert_eq!(
            fast,
            &reference[..],
            "open_interpretations({written:?}, rooted {rooted}) under {:?}",
            &self.container_path[..]
        );
        let tier_2 = (self.namespace_depth.max(1)..=self.container_path.len())
            .map(|k| [&self.container_path[..k], written].concat())
            .collect::<Vec<_>>();
        let enclosing_assembly_module = fast.iter().any(
            |i| matches!(i, OpenInterpretation::AssemblyModule(p) if !rooted && tier_2.contains(p)),
        );
        CENSUS.with(|c| {
            let mut census = c.get();
            census.enclosing_assembly_modules += usize::from(enclosing_assembly_module);
            c.set(census);
        });
    }

    /// Assert that the fast dropped-path hazard `uncovered`, from
    /// `candidates`, is what the reference candidates raise.
    pub(super) fn check_uncovered_dropped_path(
        &self,
        written: &[String],
        rooted: bool,
        interps: &[OpenInterpretation],
        candidates: &OpenTierCandidates,
        uncovered: bool,
    ) {
        let raises = |full: &Vec<String>| {
            self.assemblies
                .any_split_of_a_module_path_has_a_dropped_type(full)
                && !interps.iter().any(|i| match i {
                    OpenInterpretation::Module(p)
                    | OpenInterpretation::AssemblyModule(p)
                    | OpenInterpretation::Reading(p) => p == full,
                })
        };
        let reference = self
            .reference_open_tier_candidates(written, rooted)
            .iter()
            .any(|(full, _)| raises(full));
        assert_eq!(
            uncovered,
            reference,
            "open_names_an_uncovered_dropped_path({written:?}, rooted {rooted}) under {:?}",
            &self.container_path[..]
        );
        let listed = candidates.paths.iter().any(|(full, _)| raises(full));
        CENSUS.with(|c| {
            let mut census = c.get();
            census.opens += 1;
            census.opens_with_unlisted += usize::from(candidates.unnamed_beyond.is_some());
            census.opens_reached += usize::from(candidates.unnamed_beyond.is_some_and(|r| r > 0));
            census.dropped_by_listed += usize::from(listed);
            census.dropped_by_unlisted += usize::from(uncovered && !listed);
            c.set(census);
        });
    }

    /// [`Self::resolved_project_module`], building every enclosing
    /// container's completion.
    fn reference_resolved_project_module(
        &self,
        written: &[String],
        rooted: bool,
    ) -> Option<Vec<String>> {
        let (head, rest) = written.split_first()?;
        if !rooted {
            if let Some(target) = self.lexical_alias_target(head) {
                let mut full = target.to_vec();
                full.extend_from_slice(rest);
                return self.is_project_module_path(&full).then_some(full);
            }
            for prefix in self.scope.open_shortening_prefixes.iter().rev() {
                let mut full = prefix.path.clone();
                full.extend_from_slice(written);
                if self.is_project_module_path(&full) {
                    return Some(full);
                }
            }
            for k in (1..=self.container_path.len()).rev() {
                let mut full = self.container_path[..k].to_vec();
                full.extend_from_slice(written);
                if self.is_project_module_path(&full) {
                    CENSUS.with(|c| {
                        let mut census = c.get();
                        census.enclosing_project_modules += 1;
                        c.set(census);
                    });
                    return Some(full);
                }
            }
        }
        if self.is_project_module_path(written) {
            return Some(written.to_vec());
        }
        None
    }

    /// Assert that the fast [`Self::resolved_project_module`] answer `fast` is
    /// the reference's.
    pub(super) fn check_resolved_project_module(
        &self,
        written: &[String],
        rooted: bool,
        fast: &Option<Vec<String>>,
    ) {
        assert_eq!(
            fast,
            &self.reference_resolved_project_module(written, rooted),
            "resolved_project_module({written:?}, rooted {rooted}) under {:?}",
            &self.container_path[..]
        );
    }

    /// [`Self::cross_file_type_case_located`], building every enclosing
    /// container's completion.
    fn reference_cross_file_type_case(
        &self,
        written: &[String],
        rooted: bool,
    ) -> Option<(CrossFileCase, Vec<String>)> {
        if !rooted && self.scope.opacity.qualified_paths_unmodelled() {
            return None;
        }
        let hit = |full: Vec<String>, tier: CaseTier| -> Option<(CrossFileCase, Vec<String>)> {
            match self
                .preceding
                .type_qualified_case(&full, &self.container_path)
                .filter(|_| !self.value_shadows_case(&full))
            {
                None => None,
                Some(_) if self.earlier_case_type_namesake_possible(&full, written.len() > 2) => {
                    Some((CrossFileCase::Declined, full))
                }
                Some(id) => Some((CrossFileCase::Hit(tier, id), full)),
            }
        };
        if !rooted {
            if let Some((head, rest)) = written.split_first()
                && let Some(target) = self.lexical_alias_target(head)
            {
                let mut full = target.to_vec();
                full.extend_from_slice(rest);
                return hit(full, CaseTier::Alias);
            }
            for prefix in self.scope.open_shortening_prefixes.iter().rev() {
                let mut full = prefix.path.clone();
                full.extend_from_slice(written);
                if let Some(found) = hit(full, CaseTier::Namespace) {
                    return Some(found);
                }
            }
            for k in (1..=self.container_path.len()).rev() {
                let mut full = self.container_path[..k].to_vec();
                full.extend_from_slice(written);
                if let Some(found) = hit(full, CaseTier::Namespace) {
                    CENSUS.with(|c| {
                        let mut census = c.get();
                        census.enclosing_cases += 1;
                        c.set(census);
                    });
                    return Some(found);
                }
            }
        }
        hit(written.to_vec(), CaseTier::Namespace)
    }

    /// Assert that the fast [`Self::cross_file_type_case_located`] answer
    /// `fast` is the reference's.
    pub(super) fn check_cross_file_type_case(
        &self,
        written: &[String],
        rooted: bool,
        fast: &Option<(CrossFileCase, Vec<String>)>,
    ) {
        let reference = self.reference_cross_file_type_case(written, rooted);
        CENSUS.with(|c| {
            let mut census = c.get();
            census.case_misses += usize::from(reference.is_none());
            c.set(census);
        });
        assert_eq!(
            fast,
            &reference,
            "cross_file_type_case_located({written:?}, rooted {rooted}) under {:?}",
            &self.container_path[..]
        );
    }
}

/// The segments every generated name is drawn from: few, so heads, modules,
/// types and namespaces collide.
const NAMES: [&str; 2] = ["A", "B"];
/// The residuals a use asks for, and the members a type or module declares.
const MEMBERS: [&str; 2] = ["Red", "Blue"];

/// A declaration in a generated container.
#[derive(Clone, Debug)]
enum Item {
    /// `module <name> = …`, `rec` or not.
    Module(&'static str, bool, Vec<Item>),
    /// `module <name> = <path>`, an abbreviation.
    Alias(&'static str, Vec<&'static str>),
    /// `type <name> = | Red | Blue`.
    Union(&'static str),
    /// `type <name> = Red = 0 | Blue = 1`.
    Enum(&'static str),
    /// `type <name>() =` with a static member.
    Static(&'static str, &'static str),
    /// `let <name> = 0`.
    Let(&'static str),
    /// `open <path>`.
    Open(Vec<&'static str>),
    /// `let _ = <head>.<residual>`: a two-segment dotted use.
    Use(&'static str, &'static str),
    /// `let _ = <path>.<member>`: a longer dotted use.
    LongUse(Vec<&'static str>, &'static str),
    /// `let _ = function <path>.<case> -> 0 | _ -> 1`: a qualified case
    /// pattern.
    CasePattern(Vec<&'static str>, &'static str),
}

/// A generated file's header.
#[derive(Clone, Debug)]
enum Header {
    /// `module <path>`, with its body at the margin.
    Module(Vec<&'static str>),
    /// `namespace <path>`, `rec` or not.
    Namespace(Vec<&'static str>, bool),
}

fn name() -> impl Strategy<Value = &'static str> {
    prop::sample::select(&NAMES[..])
}

fn member() -> impl Strategy<Value = &'static str> {
    prop::sample::select(&MEMBERS[..])
}

fn path() -> impl Strategy<Value = Vec<&'static str>> {
    prop::collection::vec(name(), 1..3)
}

/// A container's declarations; with `long_uses`, dotted uses and case
/// patterns of more than one qualifying segment among them.
fn items(long_uses: bool) -> impl Strategy<Value = Vec<Item>> {
    let long_weight = if long_uses { 2 } else { 0 };
    let leaf = prop_oneof![
        2 => name().prop_map(Item::Union),
        2 => name().prop_map(Item::Enum),
        1 => (name(), member()).prop_map(|(n, m)| Item::Static(n, m)),
        2 => prop_oneof![name(), member()].prop_map(Item::Let),
        3 => path().prop_map(Item::Open),
        1 => (name(), path()).prop_map(|(n, p)| Item::Alias(n, p)),
        6 => (name(), member()).prop_map(|(h, r)| Item::Use(h, r)),
        long_weight => (path(), member()).prop_map(|(p, m)| Item::LongUse(p, m)),
        long_weight => (path(), member()).prop_map(|(p, m)| Item::CasePattern(p, m)),
    ];
    let item = leaf.prop_recursive(5, 48, 6, |inner| {
        (
            name(),
            prop::bool::weighted(0.1),
            prop::collection::vec(inner, 0..6),
        )
            .prop_map(|(n, rec_, body)| Item::Module(n, rec_, body))
    });
    prop::collection::vec(item, 0..8)
}

fn header() -> impl Strategy<Value = Header> {
    prop_oneof![
        path().prop_map(Header::Module),
        (path(), prop::bool::weighted(0.1)).prop_map(|(p, r)| Header::Namespace(p, r)),
    ]
}

fn render_items(out: &mut String, items: &[Item], indent: usize, in_namespace: bool) {
    let pad = " ".repeat(indent);
    for item in items {
        match item {
            Item::Module(name, rec_, body) => {
                let rec_ = if *rec_ { "rec " } else { "" };
                out.push_str(&format!("{pad}module {rec_}{name} =\n"));
                out.push_str(&format!("{pad}    let private z = 0\n"));
                render_items(out, body, indent + 4, false);
            }
            Item::Alias(name, path) => {
                out.push_str(&format!("{pad}module {name} = {}\n", path.join(".")));
            }
            Item::Union(name) => out.push_str(&format!("{pad}type {name} = | Red | Blue\n")),
            Item::Enum(name) => out.push_str(&format!("{pad}type {name} = Red = 0 | Blue = 1\n")),
            Item::Static(name, member) => out.push_str(&format!(
                "{pad}type {name}() =\n{pad}    static member {member} = 1\n"
            )),
            Item::Open(path) => out.push_str(&format!("{pad}open {}\n", path.join("."))),
            // A namespace holds no values.
            Item::Let(_) | Item::Use(..) | Item::LongUse(..) | Item::CasePattern(..)
                if in_namespace => {}
            Item::Let(name) => out.push_str(&format!("{pad}let {name} = 0\n")),
            Item::Use(head, residual) => {
                out.push_str(&format!("{pad}let _ = {head}.{residual}\n"));
            }
            Item::LongUse(path, member) => {
                out.push_str(&format!("{pad}let _ = {}.{member}\n", path.join(".")));
            }
            Item::CasePattern(path, case) => out.push_str(&format!(
                "{pad}let _ = function {}.{case} -> 0 | _ -> 1\n",
                path.join(".")
            )),
        }
    }
}

fn render(header: &Header, items: &[Item]) -> String {
    let mut out = String::new();
    let in_namespace = match header {
        Header::Module(path) => {
            out.push_str(&format!("module {}\n\n", path.join(".")));
            false
        }
        Header::Namespace(path, rec_) => {
            let rec_ = if *rec_ { "rec " } else { "" };
            out.push_str(&format!("namespace {rec_}{}\n\n", path.join(".")));
            true
        }
    };
    render_items(&mut out, items, 0, in_namespace);
    out
}

/// A module a generated referenced assembly declares: its namespace, then
/// the top-level module and the modules nested in it, outermost first.
#[derive(Clone, Debug)]
struct AssemblyModule {
    namespace: Vec<&'static str>,
    chain: Vec<&'static str>,
}

/// A generated referenced assembly: its modules, and the namespaces it
/// dropped a type in.
#[derive(Clone, Debug)]
struct Assembly {
    modules: Vec<AssemblyModule>,
    dropped: Vec<Vec<&'static str>>,
}

fn assembly() -> impl Strategy<Value = Assembly> {
    let module = (
        prop::collection::vec(name(), 0..3),
        prop::collection::vec(name(), 1..4),
    )
        .prop_map(|(namespace, chain)| AssemblyModule { namespace, chain });
    (
        prop::collection::vec(module, 0..4),
        prop::collection::vec(prop::collection::vec(name(), 0..3), 0..2),
    )
        .prop_map(|(modules, dropped)| Assembly { modules, dropped })
}

fn segments(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

fn assembly_env(assembly: &Assembly) -> crate::assembly_env::AssemblyEnv {
    use crate::assembly_env::AssemblyEnv;
    use crate::assembly_env::from_views_tests::module_entity;
    let roots = assembly
        .modules
        .iter()
        .map(|m| {
            let (innermost, outer) = m.chain.split_last().expect("a module chain is not empty");
            let mut entity = module_entity("Probe", &[], innermost);
            for name in outer.iter().rev() {
                entity = borzoi_assembly::Entity {
                    nested_types: vec![entity],
                    ..module_entity("Probe", &[], name)
                };
            }
            entity.namespace = segments(&m.namespace);
            entity
        })
        .collect();
    let mut env = AssemblyEnv::from_entities(roots);
    for namespace in &assembly.dropped {
        env.mark_namespace_dropped_type(segments(namespace));
    }
    env
}

/// Resolve each generated project that parses cleanly against `env`, over a
/// fixed seed, so a census asserts the same cases every run. Returns how many
/// projects were generated and how many parsed.
fn resolve_generated<S>(
    strategy: S,
    env: impl Fn(&S::Value) -> crate::assembly_env::AssemblyEnv,
    files: impl Fn(&S::Value) -> &[(Header, Vec<Item>)],
) -> (usize, usize)
where
    S: Strategy,
{
    use std::path::PathBuf;

    use borzoi_cst::parser::parse;
    use borzoi_cst::syntax::{AstNode, ImplFile};

    use crate::recovery::SyntaxRecovery;
    use crate::resolve::{ProjectFile, SourceFile, resolve_project_files};

    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: 1024,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::deterministic_rng(RngAlgorithm::ChaCha),
    );
    let parsed_clean = Cell::new(0usize);
    let generated = Cell::new(0usize);
    runner
        .run(&strategy, |case| {
            generated.set(generated.get() + 1);
            let sources: Vec<String> = files(&case).iter().map(|(h, i)| render(h, i)).collect();
            let parses: Vec<_> = sources.iter().map(|s| parse(s)).collect();
            if parses.iter().any(|p| !p.errors.is_empty()) {
                return Ok(());
            }
            parsed_clean.set(parsed_clean.get() + 1);
            let source_files: Vec<SourceFile> = parses
                .iter()
                .map(|p| SourceFile::Impl(ImplFile::cast(p.root.clone()).expect("an impl file")))
                .collect();
            let paths: Vec<PathBuf> = (0..source_files.len())
                .map(|i| PathBuf::from(format!("/probe/File{i}.fs")))
                .collect();
            let qnofs = crate::qualified_names(&source_files, &paths);
            let project: Vec<ProjectFile> = source_files
                .into_iter()
                .zip(qnofs)
                .zip(&parses)
                .map(|((file, qnof), p)| ProjectFile::new(file, qnof, SyntaxRecovery::of(p)))
                .collect();
            resolve_project_files(&project, &env(&case));
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));
    (generated.get(), parsed_clean.get())
}

/// Up to three files ([`items`]).
fn project(long_uses: bool) -> impl Strategy<Value = Vec<(Header, Vec<Item>)>> {
    prop::collection::vec((header(), items(long_uses)), 1..4)
}

/// The fast [`Resolver::head_contested_by_project_module`] answers as the
/// reference does at every dotted head of generated projects: up to three
/// files, each a module or namespace, with nested and recursive modules,
/// abbreviations, types and values whose names collide with the heads, and
/// `open`s. The check itself runs inside the fold, at every call; this test
/// generates the calls, and its census asserts each way of answering was
/// reached.
#[test]
fn fast_head_contest_agrees_with_the_reference() {
    CENSUS.with(|c| c.set(Census::new()));
    let (generated, parsed_clean) = resolve_generated(
        project(false),
        |_| crate::assembly_env::AssemblyEnv::default(),
        |files| files,
    );
    let census = CENSUS.with(Cell::get);
    println!("{generated} projects, {parsed_clean} parsed cleanly: {census:?}");
    assert!(
        parsed_clean * 2 > generated,
        "most generated projects parse"
    );
    assert!(
        census.calls >= 5000
            && census.contested >= 500
            && census.passed_a_module >= 500
            && census.passed_beside >= 200
            && census.contested_by_open >= 10
            && census.deep >= 1000,
        "the generated projects reach every way of answering: {census:?}"
    );
}

/// The fast [`Resolver::open_tier_candidates`] (as
/// [`Resolver::open_interpretations`] and
/// [`Resolver::open_names_an_uncovered_dropped_path`] read it),
/// [`Resolver::resolved_project_module`] and
/// [`Resolver::cross_file_type_case_located`] answer as their references do
/// over the generated projects of
/// [`fast_head_contest_agrees_with_the_reference`], each against a generated
/// referenced assembly whose namespaces, modules and dropped types are named
/// like the project's containers. Its census asserts that `open`s leave
/// completions unlisted, under containers the assembly does and does not
/// reach into, and that the dropped-path hazard is raised both ways.
#[test]
fn fast_open_and_case_walks_agree_with_their_references() {
    CENSUS.with(|c| c.set(Census::new()));
    let (generated, parsed_clean) = resolve_generated(
        (project(true), assembly()),
        |(_, assembly)| assembly_env(assembly),
        |(files, _)| files,
    );
    let census = CENSUS.with(Cell::get);
    println!("{generated} projects, {parsed_clean} parsed cleanly: {census:?}");
    assert!(
        parsed_clean * 2 > generated,
        "most generated projects parse"
    );
    assert!(
        census.opens >= 2000
            && census.opens_with_unlisted >= 1000
            && census.opens_reached >= 500
            && census.enclosing_assembly_modules >= 200
            && census.dropped_by_unlisted >= 50
            && census.dropped_by_listed >= 500
            && census.enclosing_project_modules >= 1000
            && census.enclosing_cases >= 40
            && census.case_misses >= 1000,
        "the generated projects reach every way of answering: {census:?}"
    );
}
