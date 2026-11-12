//! Generative differential: **certainty implies exactness** for the F#
//! compile order.
//!
//! `fsproj_msbuild_diff` pins a handful of hand-written compile-order fixtures;
//! this harness generates small projects plus a synthetic SDK, evaluates each
//! with our walker, and — whenever we claim `items_uncertain == false` — diffs
//! our ordered Compile list against the order MSBuild hands the compiler. That
//! order is not the evaluated `@(Compile)`: F#'s `FSharpSourceCodeCompileOrder`
//! target re-sorts the evaluated items by their `CompileOrder` metadata into
//! five buckets around the explicit `CompileBefore`/`CompileAfter` lists, and
//! drops an item whose `CompileOrder` names no bucket. So the oracle *runs* the
//! target (the `targetItems` op), in-process and resident, and reads back the
//! Compile list it leaves.
//!
//! The target is the SDK's own: its text is lifted, at test time, out of the
//! `Microsoft.FSharp.Targets` installed beside the MSBuild the oracle runs, and
//! written into the synthetic SDK's `Sdk.targets`. Importing that whole file
//! instead would drag `Microsoft.Common.targets` in after it; the target reads
//! nothing but the three item lists, so the lifted copy runs exactly as it runs
//! in a real build.
//!
//! The synthetic SDK is imported through the `Sdk` attribute on both sides —
//! the oracle's default SDK resolver is pointed at it (`--sdks-path`) — so our
//! walker treats its groups as SDK groups, as it does a real SDK's. Each case
//! gets an SDK of its own name, so no resolution or file can be served from an
//! earlier case.
//!
//! ## The generated space
//!
//! It starts inside the domain the evaluator models today (stage F0 of
//! `docs/compile-item-fidelity-plan.md`): literal, `$(…)`-valued and
//! `$(MSBuildProjectDirectory)`-rooted includes across `Compile`,
//! `CompileBefore` and `CompileAfter`; `CompileOrder` metadata on includes and
//! on metadata-only `<Compile Update>`s, as an attribute, as conditioned child
//! elements, and through a property, including a value in another case and a
//! value naming no bucket; defined- and undefined-property conditions; property
//! and item groups interleaved across `Sdk.props`, the project and
//! `Sdk.targets`.
//!
//! It stays out of three things the later stages turn on, each a known way for
//! the evaluator to commit a wrong order today:
//!
//! - **alias spellings** (`./A.fs`, `sub/../A.fs`, a case variant): stage F2;
//! - **escapes** in a spec (the escape generative differential covers those);
//! - **an `Update` spec that names one file twice**, or names a file after a
//!   fragment MSBuild cannot batch: the lazy evaluator's stranded-prefix bug
//!   (`LazyItemEvaluator.ComputeItems` removes the batched prefix under the
//!   wrong key), stage F3. A spec of distinct literals, or a single property
//!   whose value lists distinct files, cannot reach it.
//!
//! Inputs come from a proptest runner with its persisted-failure file, so a
//! divergence shrinks to a small witness and reproduces.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

mod common;

use borzoi_msbuild::{
    ParsedProject, SdkPaths, SdkResolution, SdkResolveError, parse_fsproj_with_imports,
};
use common::Oracle;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};

// ---------------------------------------------------------------------------
// Fixture model. Everything renders from small index pools so shrinking is
// meaningful and the XML stays well-formed by construction.
// ---------------------------------------------------------------------------

/// Source files. Distinct under case folding, so no two can collide in
/// MSBuild's case-insensitive batching of `Update` targets.
const FILES: [&str; 5] = ["A.fs", "B.fs", "C.fs", "D.fs", "E.fs"];

const PROP_NAMES: [&str; 3] = ["Src", "Pair", "Order"];

/// `CompileOrder` values: the five buckets, two in another case
/// (`WithMetadataValue` compares ignoring case), and one naming no bucket,
/// which the target drops from the compile list altogether.
const ORDERS: [&str; 8] = [
    "CompileFirst",
    "CompileBefore",
    "",
    "CompileAfter",
    "CompileLast",
    "compilelast",
    "COMPILEBEFORE",
    "Middle",
];

/// A property value.
#[derive(Debug, Clone, Copy)]
enum PropValue {
    File(usize),
    /// Two *distinct* files, `;`-separated, so the value is an item list and
    /// an `Update` through it never names one file twice.
    Pair(usize, usize),
    Order(usize),
    /// `$(Name)`: a reference to another property.
    Ref(usize),
}

impl PropValue {
    fn render(self) -> String {
        match self {
            PropValue::File(i) => FILES[i].to_string(),
            PropValue::Pair(a, b) => format!("{};{}", FILES[a], FILES[b]),
            PropValue::Order(i) => ORDERS[i].to_string(),
            PropValue::Ref(i) => format!("$({})", PROP_NAMES[i]),
        }
    }
}

/// A `Condition` attribute. `Switch` is always defined (`true`, first in
/// `Sdk.props`) so the `SwitchEq` variants evaluate cleanly; `NeverSet` is never
/// written, so both `Undef*` variants lean on an undefined property.
#[derive(Debug, Clone, Copy)]
enum Cond {
    None,
    SwitchEq(bool),
    UndefEmpty,
    UndefEqTrue,
}

impl Cond {
    fn render(self) -> String {
        match self {
            Cond::None => String::new(),
            Cond::SwitchEq(want) => format!(" Condition=\"'$(Switch)' == '{want}'\""),
            Cond::UndefEmpty => " Condition=\"'$(NeverSet)' == ''\"".to_string(),
            Cond::UndefEqTrue => " Condition=\"'$(NeverSet)' == 'true'\"".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PropWrite {
    name: usize,
    value: PropValue,
    cond: Cond,
}

#[derive(Debug, Clone, Copy)]
enum ItemType {
    Compile,
    CompileBefore,
    CompileAfter,
}

impl ItemType {
    fn tag(self) -> &'static str {
        match self {
            ItemType::Compile => "Compile",
            ItemType::CompileBefore => "CompileBefore",
            ItemType::CompileAfter => "CompileAfter",
        }
    }
}

/// An `Include` spec.
#[derive(Debug, Clone, Copy)]
enum IncludeSpec {
    File(usize),
    /// `a;b` — possibly the same file twice, which makes two items.
    Two(usize, usize),
    Prop(usize),
    /// `$(MSBuildProjectDirectory)/A.fs`: an absolute spelling of the same
    /// identity a bare `A.fs` gets.
    Rooted(usize),
}

impl IncludeSpec {
    fn render(self) -> String {
        match self {
            IncludeSpec::File(i) => FILES[i].to_string(),
            IncludeSpec::Two(a, b) => format!("{};{}", FILES[a], FILES[b]),
            IncludeSpec::Prop(i) => format!("$({})", PROP_NAMES[i]),
            IncludeSpec::Rooted(i) => format!("$(MSBuildProjectDirectory)/{}", FILES[i]),
        }
    }
}

/// An `Update` spec: distinct files (each bare or rooted), or one property.
/// Neither names a file twice or follows a literal with a fragment MSBuild
/// cannot batch (see the module doc).
#[derive(Debug, Clone)]
enum UpdateSpec {
    Files(Vec<(usize, bool)>),
    Prop(usize),
}

impl UpdateSpec {
    fn render(&self) -> String {
        match self {
            UpdateSpec::Files(files) => files
                .iter()
                .map(|&(i, rooted)| {
                    if rooted {
                        IncludeSpec::Rooted(i).render()
                    } else {
                        FILES[i].to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(";"),
            UpdateSpec::Prop(i) => format!("$({})", PROP_NAMES[*i]),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum OrderValue {
    Lit(usize),
    Prop(usize),
}

impl OrderValue {
    fn render(self) -> String {
        match self {
            OrderValue::Lit(i) => ORDERS[i].to_string(),
            OrderValue::Prop(i) => format!("$({})", PROP_NAMES[i]),
        }
    }
}

/// `CompileOrder` metadata: an optional attribute, then child elements, each
/// with its own condition. The last write that runs wins.
#[derive(Debug, Clone, Default)]
struct OrderMeta {
    attr: Option<OrderValue>,
    children: Vec<(OrderValue, Cond)>,
}

impl OrderMeta {
    fn attr(&self) -> String {
        self.attr
            .map(|v| format!(" CompileOrder=\"{}\"", v.render()))
            .unwrap_or_default()
    }

    fn is_empty(&self) -> bool {
        self.attr.is_none() && self.children.is_empty()
    }
}

#[derive(Debug, Clone)]
enum ItemDecl {
    Include {
        item_type: ItemType,
        spec: IncludeSpec,
        order: OrderMeta,
        cond: Cond,
    },
    /// `<Compile Update="…">` carrying only `CompileOrder`: the one `Update`
    /// the evaluator models. It moves every *earlier* matching item.
    OrderUpdate {
        spec: UpdateSpec,
        order: OrderMeta,
        cond: Cond,
    },
    /// An operation outside the modelled set, which must decline or be right.
    Boundary(Boundary),
}

/// Rare probes of the edge of the modelled domain: each is something the
/// evaluator either models exactly or must decline, and the property holds it
/// to that like any other case.
#[derive(Debug, Clone)]
enum Boundary {
    /// `<Compile Remove="…">`.
    Remove(UpdateSpec),
    /// `<Compile Update="…" Visible="false">`: an `Update` that writes no
    /// `CompileOrder`, so it cannot move anything.
    OtherMetadataUpdate(UpdateSpec),
    /// `<Compile Include="a" Exclude="b">`.
    Exclude(IncludeSpec, usize),
    /// `<Compile Include="@(Compile)">`: duplicates every item so far.
    SelfReference,
}

impl ItemDecl {
    fn render(&self, out: &mut String) {
        let (open, order, cond) = match self {
            ItemDecl::Boundary(boundary) => {
                let line = match boundary {
                    Boundary::Remove(spec) => format!("<Compile Remove=\"{}\" />", spec.render()),
                    Boundary::OtherMetadataUpdate(spec) => {
                        format!("<Compile Update=\"{}\" Visible=\"false\" />", spec.render())
                    }
                    Boundary::Exclude(spec, file) => format!(
                        "<Compile Include=\"{}\" Exclude=\"{}\" />",
                        spec.render(),
                        FILES[*file]
                    ),
                    Boundary::SelfReference => "<Compile Include=\"@(Compile)\" />".to_string(),
                };
                let _ = writeln!(out, "    {line}");
                return;
            }
            ItemDecl::Include {
                item_type,
                spec,
                order,
                cond,
            } => (
                format!("{} Include=\"{}\"", item_type.tag(), spec.render()),
                order,
                cond,
            ),
            ItemDecl::OrderUpdate { spec, order, cond } => {
                (format!("Compile Update=\"{}\"", spec.render()), order, cond)
            }
        };
        let tag = open.split(' ').next().expect("tag");
        let attrs = format!("{open}{}{}", order.attr(), cond.render());
        if order.children.is_empty() {
            let _ = writeln!(out, "    <{attrs} />");
        } else {
            let _ = writeln!(out, "    <{attrs}>");
            for (value, cond) in &order.children {
                let _ = writeln!(
                    out,
                    "      <CompileOrder{}>{}</CompileOrder>",
                    cond.render(),
                    value.render()
                );
            }
            let _ = writeln!(out, "    </{tag}>");
        }
    }
}

#[derive(Debug, Clone)]
enum Group {
    Props {
        cond: Cond,
        writes: Vec<PropWrite>,
    },
    Items {
        cond: Cond,
        items: Vec<ItemDecl>,
    },
    /// `<ItemDefinitionGroup>` with a default `CompileOrder` for `Compile`.
    CompileOrderDefault(OrderValue),
}

impl Group {
    fn render(&self, out: &mut String) {
        match self {
            Group::CompileOrderDefault(value) => {
                let _ = writeln!(
                    out,
                    "  <ItemDefinitionGroup>\n    <Compile>\n      <CompileOrder>{}</CompileOrder>\n    </Compile>\n  </ItemDefinitionGroup>",
                    value.render()
                );
            }
            Group::Props { cond, writes } => {
                let _ = writeln!(out, "  <PropertyGroup{}>", cond.render());
                for write in writes {
                    let _ = writeln!(
                        out,
                        "    <{name}{cond}>{value}</{name}>",
                        name = PROP_NAMES[write.name],
                        cond = write.cond.render(),
                        value = write.value.render(),
                    );
                }
                let _ = writeln!(out, "  </PropertyGroup>");
            }
            Group::Items { cond, items } => {
                let _ = writeln!(out, "  <ItemGroup{}>", cond.render());
                for item in items {
                    item.render(out);
                }
                let _ = writeln!(out, "  </ItemGroup>");
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Fixture {
    sdk_props: Vec<Group>,
    project: Vec<Group>,
    sdk_targets: Vec<Group>,
}

impl Fixture {
    fn sdk_props_xml(&self) -> String {
        // `Switch` is pinned first so `SwitchEq` conditions are deterministic
        // in both walkers.
        let mut out = String::from(
            "<Project>\n  <PropertyGroup>\n    <Switch>true</Switch>\n  </PropertyGroup>\n",
        );
        for group in &self.sdk_props {
            group.render(&mut out);
        }
        out.push_str("</Project>\n");
        out
    }

    fn project_xml(&self, sdk: &str) -> String {
        let mut out = format!("<Project Sdk=\"{sdk}\">\n");
        for group in &self.project {
            group.render(&mut out);
        }
        out.push_str("</Project>\n");
        out
    }

    fn sdk_targets_xml(&self, order_target: &str) -> String {
        let mut out = String::from("<Project>\n");
        for group in &self.sdk_targets {
            group.render(&mut out);
        }
        out.push_str("  ");
        out.push_str(order_target);
        out.push_str("\n</Project>\n");
        out
    }

    fn describe(&self) -> String {
        format!(
            "--- Sdk.props ---\n{}--- project ---\n{}--- Sdk.targets (order target elided) ---\n{}",
            self.sdk_props_xml(),
            self.project_xml("Sdk"),
            self.sdk_targets_xml("<!-- FSharpSourceCodeCompileOrder -->"),
        )
    }
}

// ---------------------------------------------------------------------------
// Strategies. Weights keep the undecidable constructs in the pool (the
// boundary is the point) but rare enough that most cases stay certain and
// reach the oracle — `most_generated_cases_are_certain` enforces that.
// ---------------------------------------------------------------------------

fn file() -> impl Strategy<Value = usize> {
    0..FILES.len()
}

fn prop_value_strategy() -> impl Strategy<Value = PropValue> {
    prop_oneof![
        4 => file().prop_map(PropValue::File),
        3 => (file(), 1..FILES.len())
            .prop_map(|(a, step)| PropValue::Pair(a, (a + step) % FILES.len())),
        3 => (0..ORDERS.len()).prop_map(PropValue::Order),
        1 => (0..PROP_NAMES.len()).prop_map(PropValue::Ref),
    ]
}

fn cond_strategy() -> impl Strategy<Value = Cond> {
    // A cleanly-false gate on a certain item is the sharpest probe (an
    // evaluator that over-resolves it stays certain and diverges); the
    // `Undef*` variants mostly push a case into the skipped uncertain bucket.
    prop_oneof![
        8 => Just(Cond::None),
        5 => proptest::bool::ANY.prop_map(Cond::SwitchEq),
        1 => Just(Cond::UndefEmpty),
        1 => Just(Cond::UndefEqTrue),
    ]
}

fn prop_write_strategy() -> impl Strategy<Value = PropWrite> {
    (0..PROP_NAMES.len(), prop_value_strategy(), cond_strategy())
        .prop_map(|(name, value, cond)| PropWrite { name, value, cond })
}

fn include_spec_strategy() -> impl Strategy<Value = IncludeSpec> {
    prop_oneof![
        6 => file().prop_map(IncludeSpec::File),
        2 => (file(), file()).prop_map(|(a, b)| IncludeSpec::Two(a, b)),
        2 => (0..PROP_NAMES.len()).prop_map(IncludeSpec::Prop),
        2 => file().prop_map(IncludeSpec::Rooted),
    ]
}

fn update_spec_strategy() -> impl Strategy<Value = UpdateSpec> {
    prop_oneof![
        5 => proptest::collection::btree_map(file(), proptest::bool::weighted(0.25), 1..3)
            .prop_map(|files| UpdateSpec::Files(files.into_iter().collect())),
        1 => (0..PROP_NAMES.len()).prop_map(UpdateSpec::Prop),
    ]
}

fn order_value_strategy() -> impl Strategy<Value = OrderValue> {
    prop_oneof![
        // Weighted towards the buckets: the case variants and the bucketless
        // value are single, sharp probes.
        8 => (0..5usize).prop_map(OrderValue::Lit),
        2 => (5..ORDERS.len()).prop_map(OrderValue::Lit),
        1 => (0..PROP_NAMES.len()).prop_map(OrderValue::Prop),
    ]
}

fn order_meta_strategy() -> impl Strategy<Value = OrderMeta> {
    (
        proptest::option::weighted(0.7, order_value_strategy()),
        proptest::collection::vec((order_value_strategy(), cond_strategy()), 0..3),
    )
        .prop_map(|(attr, children)| OrderMeta { attr, children })
}

fn item_type_strategy() -> impl Strategy<Value = ItemType> {
    prop_oneof![
        5 => Just(ItemType::Compile),
        1 => Just(ItemType::CompileBefore),
        1 => Just(ItemType::CompileAfter),
    ]
}

fn item_strategy() -> impl Strategy<Value = ItemDecl> {
    prop_oneof![
        6 => (
            item_type_strategy(),
            include_spec_strategy(),
            prop_oneof![1 => Just(OrderMeta::default()), 1 => order_meta_strategy()],
            cond_strategy(),
        )
            .prop_map(|(item_type, spec, order, cond)| ItemDecl::Include {
                item_type,
                spec,
                order,
                cond,
            }),
        3 => (
            update_spec_strategy(),
            order_meta_strategy().prop_filter("an order update writes CompileOrder", |o| {
                !o.is_empty()
            }),
            cond_strategy(),
        )
            .prop_map(|(spec, order, cond)| ItemDecl::OrderUpdate { spec, order, cond }),
        1 => boundary_strategy().prop_map(ItemDecl::Boundary),
    ]
}

fn boundary_strategy() -> impl Strategy<Value = Boundary> {
    prop_oneof![
        update_spec_strategy().prop_map(Boundary::Remove),
        update_spec_strategy().prop_map(Boundary::OtherMetadataUpdate),
        (include_spec_strategy(), file()).prop_map(|(spec, file)| Boundary::Exclude(spec, file)),
        Just(Boundary::SelfReference),
    ]
}

/// `<ItemDefinitionGroup>` giving every later `Compile` a default
/// `CompileOrder`: rare, and a probe like [`Boundary`].
fn item_definition_strategy() -> impl Strategy<Value = Group> {
    order_value_strategy().prop_map(Group::CompileOrderDefault)
}

fn group_strategy() -> impl Strategy<Value = Group> {
    prop_oneof![
        10 => (cond_strategy(), proptest::collection::vec(prop_write_strategy(), 1..3))
            .prop_map(|(cond, writes)| Group::Props { cond, writes }),
        20 => (cond_strategy(), proptest::collection::vec(item_strategy(), 1..4))
            .prop_map(|(cond, items)| Group::Items { cond, items }),
        1 => item_definition_strategy(),
    ]
}

fn fixture_strategy() -> impl Strategy<Value = Fixture> {
    (
        proptest::collection::vec(group_strategy(), 0..3),
        proptest::collection::vec(group_strategy(), 1..4),
        proptest::collection::vec(group_strategy(), 0..3),
    )
        .prop_map(|(sdk_props, project, sdk_targets)| Fixture {
            sdk_props,
            project,
            sdk_targets,
        })
}

// ---------------------------------------------------------------------------
// Both-ways evaluation.
// ---------------------------------------------------------------------------

/// The resident oracle, the `Sdks` directory its SDK resolver reads, and the
/// lifted ordering target, shared by every test in the binary.
struct Shared {
    root: PathBuf,
    oracle: Mutex<Oracle>,
    order_target: String,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("compile-order-generative-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Sdks")).expect("mkdir Sdks");
        let root = std::fs::canonicalize(root).expect("canonicalize root");
        let mut oracle = Oracle::spawn_with_sdks_path(&root.join("Sdks"));
        let order_target = lift_order_target(&mut oracle);
        Shared {
            root,
            oracle: Mutex::new(oracle),
            order_target,
        }
    })
}

/// The `FSharpSourceCodeCompileOrder` target's text, verbatim, from the
/// `Microsoft.FSharp.Targets` of the MSBuild the oracle runs.
fn lift_order_target(oracle: &mut Oracle) -> String {
    let names = ["MSBuildBinPath".to_string()];
    let bin = oracle
        .project("<Project />", &names, None, &[])
        .expect("an empty project evaluates")
        .remove("MSBuildBinPath")
        .expect("MSBuildBinPath is reported");
    let path = Path::new(&bin).join("FSharp/Microsoft.FSharp.Targets");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let doc = roxmltree::Document::parse(&text)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    let target = doc
        .descendants()
        .find(|n| {
            n.has_tag_name("Target") && n.attribute("Name") == Some("FSharpSourceCodeCompileOrder")
        })
        .unwrap_or_else(|| panic!("{} defines no FSharpSourceCodeCompileOrder", path.display()));
    text[target.range()].to_string()
}

/// One case laid out on disk: `Sdks/<sdk>/Sdk/Sdk.{props,targets}` under the
/// shared root, and the project in a directory of its own, removed on drop.
struct Laid {
    case_dir: PathBuf,
    sdk_dir: PathBuf,
    proj: PathBuf,
    sdk: String,
}

impl Drop for Laid {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.case_dir);
        let _ = std::fs::remove_dir_all(&self.sdk_dir);
    }
}

fn write_and_parse(fixture: &Fixture) -> (Laid, ParsedProject) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let shared = shared();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let sdk = format!("Sdk{n}");
    let sdks = shared.root.join("Sdks");
    let sdk_root = sdks.join(&sdk);
    let sdk_dir = sdk_root.join("Sdk");
    std::fs::create_dir_all(&sdk_dir).expect("mkdir sdk");
    let props = sdk_dir.join("Sdk.props");
    let targets = sdk_dir.join("Sdk.targets");
    std::fs::write(&props, fixture.sdk_props_xml()).expect("write Sdk.props");
    std::fs::write(&targets, fixture.sdk_targets_xml(&shared.order_target))
        .expect("write Sdk.targets");
    let case_dir = shared.root.join(format!("case{n}"));
    std::fs::create_dir_all(case_dir.join("proj")).expect("mkdir proj");
    let proj = case_dir.join("proj/Demo.fsproj");
    let project_xml = fixture.project_xml(&sdk);
    std::fs::write(&proj, &project_xml).expect("write project");

    let resolver = {
        let sdk = sdk.clone();
        let sdk_dir = sdk_dir.clone();
        move |name: &str| {
            if name == sdk {
                Ok(SdkResolution::Single(SdkPaths {
                    root: sdk_dir.clone(),
                    props: props.clone(),
                    targets: targets.clone(),
                }))
            } else {
                Err(SdkResolveError::NotFound)
            }
        }
    };
    // The oracle sets `MSBuildSDKsPath` in its own environment, which MSBuild
    // folds in as an initial property; our side sees the same snapshot.
    let mut environment = common::oracle_environment();
    environment.insert(
        "MSBuildSDKsPath".to_string(),
        sdks.to_string_lossy().into_owned(),
    );
    let parsed = parse_fsproj_with_imports(
        &project_xml,
        &proj,
        &HashMap::new(),
        &environment,
        Some(&resolver),
        None,
    )
    .unwrap_or_else(|e| panic!("our parse: {e}\n{}", fixture.describe()));
    (
        Laid {
            case_dir,
            sdk_dir: sdk_root,
            proj,
            sdk,
        },
        parsed,
    )
}

/// The property body: certain ⇒ our compile order is MSBuild's, exactly.
/// Returns whether the case was certain (i.e. the oracle ran).
fn check_certain_implies_exact(fixture: &Fixture) -> Result<bool, TestCaseError> {
    let (laid, parsed) = write_and_parse(fixture);
    if parsed.items_uncertain {
        return Ok(false);
    }
    let theirs = shared()
        .oracle
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .target_items(&laid.proj, "FSharpSourceCodeCompileOrder", "Compile")
        .unwrap_or_else(|| {
            panic!(
                "MSBuild rejected a generated project (SDK {}):\n{}",
                laid.sdk,
                fixture.describe()
            )
        });
    let ours: Vec<String> = parsed
        .items
        .iter()
        .map(|item| item.include.to_string_lossy().into_owned())
        .collect();
    prop_assert_eq!(
        &ours,
        &theirs,
        "claimed certain, but the compile order diverges.\n{}\nour diagnostics: {:#?}",
        fixture.describe(),
        parsed.diagnostics
    );
    Ok(true)
}

/// Driven through a manual [`TestRunner`] (rather than the `proptest!` macro)
/// so the run can prove it exercised the oracle: uncertain cases skip the
/// comparison, and a run where every case skipped would otherwise look like a
/// pass.
#[test]
fn certain_compile_order_matches_msbuild() {
    let mut runner = TestRunner::new(ProptestConfig {
        // 256 cases cost a couple of seconds; set
        // `BORZOI_COMPILE_ORDER_CASES` to sweep deeper.
        cases: std::env::var("BORZOI_COMPILE_ORDER_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256),
        source_file: Some(file!()),
        ..ProptestConfig::default()
    });
    let oracle_hits = std::cell::Cell::new(0u32);
    runner
        .run(&fixture_strategy(), |fixture| {
            if check_certain_implies_exact(&fixture)? {
                oracle_hits.set(oracle_hits.get() + 1);
            }
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        oracle_hits.get() > 0,
        "no generated case reached the MSBuild oracle — the property tested nothing"
    );
}

/// The property above skips uncertain cases, so on its own it could rot into
/// testing (almost) nothing if the generator, or a regression, made
/// uncertainty the norm. Sample the strategy deterministically — our parser
/// only, no oracle — and require a healthy certain fraction.
#[test]
fn most_generated_cases_are_certain() {
    let mut runner = TestRunner::deterministic();
    let strategy = fixture_strategy();
    let total = 200;
    let mut certain = 0;
    for _ in 0..total {
        let fixture = strategy
            .new_tree(&mut runner)
            .expect("generate fixture")
            .current();
        let (_laid, parsed) = write_and_parse(&fixture);
        if !parsed.items_uncertain {
            certain += 1;
        }
    }
    eprintln!("{certain}/{total} generated cases were certain");
    assert!(
        certain * 2 >= total,
        "only {certain}/{total} generated cases were certain; the differential \
         property is barely exercising the oracle — rebalance the generator \
         weights or investigate an over-degrading evaluator"
    );
}

/// Deterministic end-to-end pin of the harness: a fixture certain by
/// construction whose answer is known, so a rotted plumbing (SDK layout, the
/// lifted target, the oracle op) fails here without depending on generator
/// luck. It crosses every bucket, an explicit `CompileBefore`, a bucketless
/// value (dropped), an `Update` in `Sdk.targets` that moves a project item,
/// and a cleanly-false gate MSBuild must also skip.
#[test]
fn known_certain_fixture_reaches_oracle_and_matches() {
    let order = |i| OrderMeta {
        attr: Some(OrderValue::Lit(i)),
        children: Vec::new(),
    };
    let include = |spec, order| ItemDecl::Include {
        item_type: ItemType::Compile,
        spec,
        order,
        cond: Cond::None,
    };
    let fixture = Fixture {
        sdk_props: vec![Group::Items {
            cond: Cond::None,
            items: vec![ItemDecl::Include {
                item_type: ItemType::CompileBefore,
                spec: IncludeSpec::File(4),
                order: OrderMeta::default(),
                cond: Cond::None,
            }],
        }],
        project: vec![
            Group::Items {
                cond: Cond::SwitchEq(false),
                items: vec![include(IncludeSpec::File(0), OrderMeta::default())],
            },
            Group::Items {
                cond: Cond::None,
                items: vec![
                    include(IncludeSpec::File(0), order(4)),
                    include(IncludeSpec::File(1), OrderMeta::default()),
                    include(IncludeSpec::Rooted(2), order(7)),
                    include(IncludeSpec::File(3), order(1)),
                ],
            },
        ],
        sdk_targets: vec![Group::Items {
            cond: Cond::None,
            items: vec![ItemDecl::OrderUpdate {
                spec: UpdateSpec::Files(vec![(1, true)]),
                order: OrderMeta {
                    attr: Some(OrderValue::Lit(3)),
                    children: vec![(OrderValue::Lit(0), Cond::None)],
                },
                cond: Cond::None,
            }],
        }],
    };
    let (laid, parsed) = write_and_parse(&fixture);
    let names = |paths: &[String]| -> Vec<String> {
        paths
            .iter()
            .map(|p| {
                Path::new(p)
                    .strip_prefix(laid.proj.parent().expect("project dir"))
                    .unwrap_or_else(|_| panic!("{p} lies outside the project"))
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };
    let expected = ["B.fs", "E.fs", "D.fs", "A.fs"];
    let theirs = shared()
        .oracle
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .target_items(&laid.proj, "FSharpSourceCodeCompileOrder", "Compile")
        .expect("MSBuild evaluates the canary");
    assert_eq!(names(&theirs), expected, "MSBuild oracle changed");
    assert!(
        !parsed.items_uncertain,
        "the canary must be certain: {:#?}",
        parsed.diagnostics
    );
    let ours: Vec<String> = parsed
        .items
        .iter()
        .map(|item| item.include.to_string_lossy().into_owned())
        .collect();
    assert_eq!(names(&ours), expected);
}
