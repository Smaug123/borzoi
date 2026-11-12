//! A reference for [`Resolver::head_contested_by_project_module`] and
//! [`Resolver::module_like_transparent_for`]: each written completion's path
//! built and looked up afresh, as plainly as the rule reads, at a cost
//! quadratic in the nesting. The fast versions ask every enclosing container
//! in one walk along the container path. In this crate's unit tests, every
//! call to them is checked against these, and
//! [`fast_head_contest_agrees_with_the_reference`] drives that over generated
//! projects.

use std::cell::Cell;

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

use super::Resolver;

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
        }
    }
}

impl Resolver<'_> {
    /// Whether `prefix + [head]` is a module that could contest a dotted head.
    fn reference_is_contesting_module(&self, path: &[String]) -> bool {
        self.module_paths.contains(path)
            || self.real_nested_module_exports.contains(path)
            || self.preceding.is_exact_project_module(path)
            || self.preceding.is_real_nested_module(path)
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
        if self.recursive_module_active {
            return false;
        }
        let Some(&kinds) = self.declared_in(prefix).and_then(|d| d.get(name)) else {
            return false;
        };
        if !kinds.module || kinds.alias {
            return false;
        }
        if self.preceding.is_exact_project_module(&mp)
            || self.preceding.is_exact_nested_module(&mp)
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
            .explicit_open_prefixes
            .iter()
            .map(|(_, prefix)| (prefix.clone(), true));
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
            self.explicit_open_prefixes
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
    /// `let _ = <head>.<residual>`: the dotted use the property is about.
    Use(&'static str, &'static str),
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

fn items() -> impl Strategy<Value = Vec<Item>> {
    let leaf = prop_oneof![
        2 => name().prop_map(Item::Union),
        2 => name().prop_map(Item::Enum),
        1 => (name(), member()).prop_map(|(n, m)| Item::Static(n, m)),
        2 => prop_oneof![name(), member()].prop_map(Item::Let),
        3 => path().prop_map(Item::Open),
        1 => (name(), path()).prop_map(|(n, p)| Item::Alias(n, p)),
        6 => (name(), member()).prop_map(|(h, r)| Item::Use(h, r)),
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
            Item::Let(_) | Item::Use(..) if in_namespace => {}
            Item::Let(name) => out.push_str(&format!("{pad}let {name} = 0\n")),
            Item::Use(head, residual) => {
                out.push_str(&format!("{pad}let _ = {head}.{residual}\n"));
            }
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

/// The fast [`Resolver::head_contested_by_project_module`] answers as the
/// reference does at every dotted head of generated projects: up to three
/// files, each a module or namespace, with nested and recursive modules,
/// abbreviations, types and values whose names collide with the heads, and
/// `open`s. The check itself runs inside the fold, at every call; this test
/// generates the calls, and its census asserts each way of answering was
/// reached.
#[test]
fn fast_head_contest_agrees_with_the_reference() {
    use std::path::PathBuf;

    use borzoi_cst::parser::parse;
    use borzoi_cst::syntax::{AstNode, ImplFile};

    use crate::assembly_env::AssemblyEnv;
    use crate::recovery::SyntaxRecovery;
    use crate::resolve::{ProjectFile, SourceFile, resolve_project_files};

    CENSUS.with(|c| c.set(Census::new()));
    let files = prop::collection::vec((header(), items()), 1..4);
    // A fixed seed, so the census below asserts the same cases every run.
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
        .run(&files, |files| {
            generated.set(generated.get() + 1);
            let sources: Vec<String> = files.iter().map(|(h, i)| render(h, i)).collect();
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
            resolve_project_files(&project, &AssemblyEnv::default());
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));
    let census = CENSUS.with(Cell::get);
    let (generated, parsed_clean) = (generated.get(), parsed_clean.get());
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
