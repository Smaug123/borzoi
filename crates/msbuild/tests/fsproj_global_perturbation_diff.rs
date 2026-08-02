//! Differential test: the walker's evaluated properties and items vs the real
//! MSBuild evaluator, **under a swept set of global properties**.
//!
//! ## Why the existing differentials cannot see this
//!
//! Every other harness in this crate — `condition_diff`, `property_expr_diff`,
//! `fsproj_property_table_diff`, `fsproj_msbuild_diff` — evaluates both sides
//! under the *same* globals (in practice, none). That makes them structurally
//! blind to one whole defect class: a value whose evaluation **depends on a
//! global the caller supplied**, through a route our walker does not model.
//! Under a fixed global set the two sides agree exactly, so certain-implies-
//! exact passes; the wrongness only appears when the global changes and one
//! side moves while the other does not.
//!
//! It is not a hypothetical. The LSP injects `Configuration=Debug,
//! Platform=AnyCPU` into every evaluation (`workspace::default_build_properties`
//! — a guess, since it cannot know what the user last built), and then publishes
//! what comes back — define constants, the Compile order, project references —
//! as though it were a fixed fact about the project. Any of those may in truth
//! be "the value under the globals the editor picked".
//!
//! It also seeds `TargetFramework` on every multi-targeted project, to serve the
//! first-declared inner build (`lsp::tfm_policy`, fsproj plan E1/E2/E7). That
//! one is not a guess — it is the policy — but it is the same structural
//! exposure, and it moves far more of the SDK chain than `Configuration` does
//! because the whole TFM inference runs under it. So the sweep carries an
//! inner-build global set: what the LSP evaluates a real user's multi-targeted
//! `.fsproj` under is checked exactly, not assumed.
//!
//! ## The contract
//!
//! Unchanged from the crate's other harnesses, with the global set as a new
//! axis quantified over rather than fixed:
//!
//! - We **commit** `N = V` under globals `G` ⟹ MSBuild evaluating the same
//!   document at the same path **with global properties `G`** gives exactly `V`.
//! - We **decline** (a diagnostic, an untrusted provenance, `items_uncertain`)
//!   ⟹ no claim.
//! - MSBuild **rejects** the document under `G` ⟹ we must not have committed
//!   anything for it.
//!
//! Note what this subsumes. If we commit the same `V` under two global sets but
//! MSBuild moves between them, one of the two per-set checks must fail — so the
//! blindness this file exists to catch *is* a certain-implies-exact violation,
//! once the axis is swept. No new contract was needed; only a harness that
//! varies the thing the others hold fixed.
//!
//! ## The census
//!
//! Soundness is one-sided, so a walker that declined every global-dependent
//! value would pass silently. The tests therefore also print, and floor, a
//! **movement census**: which `(document, name)` pairs MSBuild moves across the
//! sweep, and for each, what we did — tracked it (committed two or more distinct
//! values, so the exactness check above was genuinely exercised on this axis),
//! declined it as untrusted, or never reached it at all. The floors are on
//! *tracked*, since that is the only one of the three that proves anything was
//! checked — and over the names the *document derives*, never over
//! `Configuration`/`Platform` themselves, which move because they are what the
//! sweep varies (see [`swept_global_names`]).
//!
//! Each case additionally **pins** what MSBuild must evaluate its names to under
//! the LSP's own globals. Without that, a case's comment is a claim nothing
//! checks: the sweep asserts only that we agree with MSBuild, which a fixture
//! demonstrating the exact opposite of its stated route satisfies perfectly.
//!
//! That census is the measurement. It says how much of the evaluator's committed
//! surface is actually global-dependent, and — more usefully — where the
//! evaluator currently pays for its soundness in silence rather than in
//! wrongness, which is the input to deciding whether the provenance model needs
//! a "depends on a caller-supplied global" dimension at all.
//!
//! ## What it does not reach
//!
//! Unlike a self-perturbation oracle, this one has a reference that reads
//! everything, so it has no *structural* blind spot — but it is still a finite
//! sweep, and a green run means only what it covers.
//!
//! - **Two output channels**: the evaluated property table, over the names in
//!   [`READ_NAMES`] / the SDK test's own list, and the `Compile` item set. The
//!   other things the LSP publishes off the same evaluation —
//!   `define_constants`, `project_references`, `package_references`, the TFM
//!   selection — are their own surfaces with their own uncertainty flags, and a
//!   global-dependence bug in one of those would not be seen here.
//! - **The routes in [`CASES`], the generator's grammar, and five SDK
//!   projects.** A route none of them takes is untested. Widening means adding
//!   a case; the anti-vacuity floors refuse one that measures nothing.
//! - **Whatever the global sweep in [`global_sets`] does not vary.** A value
//!   that depends on, say, `RuntimeIdentifier` is a fixed fact as far as this
//!   file is concerned.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use borzoi_msbuild::{parse_fsproj_with_imports, resolve_sdk, workloads};
use common::{Oracle, SplitMix64};
use tempfile::TempDir;

/// Property names read back from both sides. `Configuration` and `Platform` are
/// in the list deliberately: a global of that name is *itself* a value the
/// document may try to overwrite, and whether the write wins is the whole
/// `TreatAsLocalProperty` question.
const READ_NAMES: &[&str] = &["Out", "Helper", "Configuration", "Platform"];

/// One document, with any sidecar files it imports or probes for.
struct Case {
    /// Unique — it names the case's own directory, so no case can see another's
    /// `Exists()` probes or `*.props`.
    name: &'static str,
    xml: &'static str,
    files: &'static [(&'static str, &'static str)],
    /// What **MSBuild** must evaluate these names to under [`PIN_GLOBALS`].
    ///
    /// Without this, a case's comment is a claim nothing checks: the sweep only
    /// asserts that *we* agree with MSBuild, which a fixture demonstrating the
    /// opposite of its stated route satisfies perfectly. (That is not
    /// hypothetical — `treat-as-local-in-import` was written asserting the
    /// import's opt-out was inert, and passed, because MSBuild honours it and so
    /// do we.) The pins are asserted against the oracle, never against our side:
    /// an expectation I reasoned out is exactly as untrustworthy as the code I
    /// reasoned out.
    pins: &'static [(&'static str, &'static str)],
}

/// The global set the [`Case::pins`] are stated under: what the LSP injects.
const PIN_GLOBALS: &[(&str, &str)] = &[("Configuration", "Debug"), ("Platform", "AnyCPU")];

const IMPORT_SETS_OUT: &str =
    r#"<Project><PropertyGroup><Out>from-import</Out></PropertyGroup></Project>"#;

/// The routes by which a global reaches a committed value. Each is a shape the
/// design notes for the output-directory work found leaking, written down as an
/// input rather than as a thing to remember.
const CASES: &[Case] = &[
    // The direct read.
    Case {
        name: "direct",
        xml: "<Project>\n  <PropertyGroup>\n    <Out>$(Configuration)</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[],
        pins: &[("Out", "Debug")],
    },
    // The SDK's own idiom, written against a name the sweep actually supplies
    // (`<Configuration Condition="'$(Configuration)' == ''">…`). This is the
    // *read-only defaulting* rule, and it splits the sweep three ways where a
    // gate on an unsupplied name could not split it at all: with no
    // `Configuration` global the write fires; with `Configuration=Debug` MSBuild
    // discards the write unevaluated and the global stands; and with
    // `Configuration=""` the global is **empty but still read-only**, so the
    // write is discarded there too and the value stays empty — the one row that
    // distinguishes "absent" from "supplied empty".
    Case {
        name: "default-write",
        xml: "<Project>\n  <PropertyGroup>\n    <Configuration Condition=\"'$(Configuration)' == ''\">DefaultedInDocument</Configuration>\n    <Out>[$(Configuration)]-[$(Platform)]</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[],
        pins: &[("Out", "[Debug]-[AnyCPU]"), ("Configuration", "Debug")],
    },
    // A gate on the *group*, so the walk must decide whether to visit the writes
    // at all — the shape a property-walk that only inspects visited groups gets
    // wrong.
    Case {
        name: "group-condition",
        xml: "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Out>optimised</Out>\n  </PropertyGroup>\n  <PropertyGroup Condition=\"'$(Configuration)' != 'Release'\">\n    <Out>plain</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[],
        pins: &[("Out", "plain")],
    },
    // A gate on the individual write.
    Case {
        name: "property-condition",
        xml: "<Project>\n  <PropertyGroup>\n    <Out Condition=\"'$(Platform)' == 'x64'\">wide</Out>\n    <Out Condition=\"'$(Platform)' != 'x64'\">narrow</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[],
        pins: &[("Out", "narrow")],
    },
    // `<Choose>`: the unselected arm is never walked, so a value can depend on a
    // global through a branch the property walk does not visit.
    Case {
        name: "choose",
        xml: "<Project>\n  <Choose>\n    <When Condition=\"'$(Platform)' == 'x64'\">\n      <PropertyGroup><Out>wide</Out></PropertyGroup>\n    </When>\n    <Otherwise>\n      <PropertyGroup><Out>narrow</Out></PropertyGroup>\n    </Otherwise>\n  </Choose>\n</Project>\n",
        files: &[],
        pins: &[("Out", "narrow")],
    },
    // Nested arms, and a `When` whose selection depends on a *different* global
    // from the outer one.
    Case {
        name: "choose-nested",
        xml: "<Project>\n  <Choose>\n    <When Condition=\"'$(Configuration)' == 'Release'\">\n      <Choose>\n        <When Condition=\"'$(Platform)' == 'x64'\">\n          <PropertyGroup><Out>rel-x64</Out></PropertyGroup>\n        </When>\n        <Otherwise>\n          <PropertyGroup><Out>rel-other</Out></PropertyGroup>\n        </Otherwise>\n      </Choose>\n    </When>\n    <Otherwise>\n      <PropertyGroup><Out>dbg</Out></PropertyGroup>\n    </Otherwise>\n  </Choose>\n</Project>\n",
        files: &[],
        pins: &[("Out", "dbg")],
    },
    // A `When` with no `Otherwise`, so with the arm unselected the later
    // default-write decides.
    Case {
        name: "choose-no-otherwise",
        xml: "<Project>\n  <Choose>\n    <When Condition=\"'$(Platform)' == 'x64'\">\n      <PropertyGroup><Out>wide</Out></PropertyGroup>\n    </When>\n  </Choose>\n  <PropertyGroup>\n    <Out Condition=\"'$(Out)' == ''\">narrow</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[],
        pins: &[("Out", "narrow")],
    },
    // A global outranks a document write of the same name. Committing
    // `FromDocument` here would be a wrong commit under every non-empty
    // `Configuration` global.
    Case {
        name: "global-beats-write",
        xml: "<Project>\n  <PropertyGroup>\n    <Configuration>FromDocument</Configuration>\n    <Out>$(Configuration)</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[],
        pins: &[("Out", "Debug"), ("Configuration", "Debug")],
    },
    // …unless the entry project opts out, which flips the answer.
    Case {
        name: "treat-as-local",
        xml: "<Project TreatAsLocalProperty=\"Configuration\">\n  <PropertyGroup>\n    <Configuration>FromDocument</Configuration>\n    <Out>$(Configuration)</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[],
        pins: &[("Out", "FromDocument"), ("Configuration", "FromDocument")],
    },
    // `TreatAsLocalProperty` on an *imported* root also opts the name out — the
    // unprotection is not confined to the entry project, so a write in the
    // imported file beats the global, and everything downstream of it reads the
    // imported value. Pinned, because it is the opposite of what this case was
    // first written to claim.
    Case {
        name: "treat-as-local-in-import",
        xml: "<Project>\n  <Import Project=\"local.props\" />\n  <PropertyGroup>\n    <Out>$(Configuration)</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[(
            "local.props",
            "<Project TreatAsLocalProperty=\"Configuration\">\n  <PropertyGroup><Configuration>FromImport</Configuration></PropertyGroup>\n</Project>\n",
        )],
        pins: &[("Out", "FromImport"), ("Configuration", "FromImport")],
    },
    // A gated import: which *document* arrives depends on a global.
    Case {
        name: "import-condition",
        xml: "<Project>\n  <Import Project=\"rel.props\" Condition=\"'$(Configuration)' == 'Release'\" />\n  <PropertyGroup>\n    <Out Condition=\"'$(Out)' == ''\">default</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[("rel.props", IMPORT_SETS_OUT)],
        pins: &[("Out", "default")],
    },
    // A *property-selected* import: the same, but through the `Project`
    // attribute's expansion rather than a `Condition`.
    Case {
        name: "import-selected-by-property",
        xml: "<Project>\n  <Import Project=\"$(Configuration).props\" Condition=\"Exists('$(Configuration).props')\" />\n  <PropertyGroup>\n    <Out Condition=\"'$(Out)' == ''\">none</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[
            (
                "Debug.props",
                "<Project><PropertyGroup><Out>debug-import</Out></PropertyGroup></Project>\n",
            ),
            (
                "Release.props",
                "<Project><PropertyGroup><Out>release-import</Out></PropertyGroup></Project>\n",
            ),
        ],
        pins: &[("Out", "debug-import")],
    },
    // Laundered through a helper property, so the dependence is one hop away
    // from the read.
    Case {
        name: "indirect-helper",
        xml: "<Project>\n  <PropertyGroup>\n    <Helper>$(Configuration)-x</Helper>\n    <Out>[$(Helper)]</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[],
        pins: &[("Out", "[Debug-x]"), ("Helper", "Debug-x")],
    },
    // The dependence runs through a property *function* on the global, not a
    // comparison. The `!= ''` guard is load-bearing: a method call on an unset
    // property is an evaluation error, and this case is about the gate, not
    // about the reject branch (which `MSBuild rejects ⟹ we commit nothing`
    // covers wherever it does fire).
    Case {
        name: "condition-via-function",
        xml: "<Project>\n  <PropertyGroup>\n    <Out Condition=\"'$(Configuration)' != '' AND $(Configuration.StartsWith('Rel'))\">release-ish</Out>\n    <Out Condition=\"'$(Out)' == ''\">other</Out>\n  </PropertyGroup>\n</Project>\n",
        files: &[],
        pins: &[("Out", "other")],
    },
    // A global the entry document never names — only a file it imports does. So
    // "which globals can move this project's answers" is not readable off the
    // entry document's own `$(...)` references, which is exactly how the SDK's
    // props consume the caller-settable knobs.
    //
    // (MSBuild has no property-*name* indirection to test instead: `$($(Helper))`
    // is rejected outright with MSB4184, so an import is the real shape.)
    Case {
        name: "unmentioned-global",
        xml: "<Project>\n  <Import Project=\"probe.props\" />\n</Project>\n",
        files: &[(
            "probe.props",
            "<Project><PropertyGroup><Out>[$(BorzoiProbe)]</Out></PropertyGroup></Project>\n",
        )],
        pins: &[("Out", "[]")],
    },
];

/// The global sets swept. Every set is a plausible caller: `dotnet build -c
/// Release`, an IDE's guess, a `-p:` override.
///
/// The empty set and the LSP's own injection are both present because the
/// difference between them is exactly the risk being measured — everything else
/// in this crate only ever evaluates the first.
fn global_sets() -> Vec<Vec<(String, String)>> {
    let g = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    };
    vec![
        // No globals: the baseline every other harness in this crate uses.
        g(&[]),
        // What the LSP actually injects.
        g(&[("Configuration", "Debug"), ("Platform", "AnyCPU")]),
        // What the user may last have built.
        g(&[("Configuration", "Release"), ("Platform", "x64")]),
        // One axis at a time, so a case that depends on only one of them still
        // moves.
        g(&[("Configuration", "Release")]),
        g(&[("Platform", "x64")]),
        // An *empty* global. Distinct from "absent": it is still read-only, so a
        // `Condition="'$(X)' == ''"` default-write fires while the document's own
        // write of `X` is still suppressed.
        g(&[("Configuration", "")]),
        // A global the document does not mention.
        g(&[("BorzoiProbe", "probed")]),
        // An inner build. The LSP seeds this on every multi-targeted project
        // (`tfm_policy`, fsproj plan E1/E2), so it is not a hypothetical
        // perturbation but the evaluation a real user's `.fsproj` diagnostics
        // and defines come from — and it moves far more of the SDK chain than
        // `Configuration` does, because the whole TFM inference runs under it.
        g(&[
            ("Configuration", "Debug"),
            ("Platform", "AnyCPU"),
            ("TargetFramework", "net10.0"),
        ]),
    ]
}

/// Evaluate `dir/Demo.fsproj` under `globals`, both sides.
///
/// Returns our [`OurSide`] outcome for every read-back name, and MSBuild's value
/// for each — `None` when MSBuild rejects the document, which the contract turns
/// into "we must have committed nothing".
fn evaluate_both(
    oracle: &mut Oracle,
    dir: &Path,
    xml: &str,
    globals: &[(String, String)],
    read_names: &[&str],
    with_sdk: bool,
) -> (OurSide, Option<HashMap<String, String>>) {
    let project_path = dir.join("Demo.fsproj");
    let extra: HashMap<String, String> = globals.iter().cloned().collect();

    let dotnet_root = with_sdk.then(common::dotnet_root_from_env);
    let workload = with_sdk.then(common::workload_env_from_process);
    let resolver = |name: &str| {
        let (user_dotnet_root, overrides_present) =
            workload.as_ref().expect("only called when with_sdk");
        resolve_sdk(
            dotnet_root.as_ref().expect("only called when with_sdk"),
            None,
            name,
            None,
            None,
            &workloads::WorkloadEnvironment {
                user_dotnet_root: user_dotnet_root.as_deref(),
                overrides_present: *overrides_present,
                // The fixture tempdir has no global.json above it.
                global_json_pins_workload_set: false,
            },
        )
    };

    // `parse_fsproj_with_imports` rather than `parse_fsproj`: it is the entry
    // point the LSP itself calls, and half the routes above run through an
    // `<Import>`.
    let parsed = parse_fsproj_with_imports(
        xml,
        &project_path,
        &extra,
        &common::oracle_environment(),
        with_sdk.then_some(&resolver as &borzoi_msbuild::SdkResolver<'_>),
        None,
    )
    .expect("well-formed XML parses");

    // Per name, as in `fsproj_property_table_diff`: an untrusted provenance
    // withdraws that name's claim without withdrawing its neighbours'.
    let mut ours = OurSide::default();
    for name in read_names {
        match parsed.properties.get(*name) {
            None => {
                ours.absent.insert((*name).to_string());
            }
            Some(_) if parsed.property_provenance_untrusted(name) => {
                ours.untrusted.insert((*name).to_string());
            }
            Some(value) => {
                ours.committed.insert((*name).to_string(), value.clone());
            }
        }
    }

    let names: Vec<String> = read_names.iter().map(|n| (*n).to_string()).collect();
    let theirs = oracle.project(xml, &names, Some(&project_path), globals);

    if theirs.is_none() {
        assert!(
            ours.committed.is_empty(),
            "certain-implies-exact violated: MSBuild rejects this project under \
             globals {globals:?}, but we committed {:?}\n--- xml ---\n{xml}",
            ours.committed
        );
    }
    (ours, theirs)
}

/// One evaluation's outcome on our side, split three ways.
///
/// The split matters to the census, not to soundness: `untrusted` and `absent`
/// both make no claim, but they are different states of knowledge. "The walker
/// evaluated this name and marked the result untrustworthy" is a modelled
/// decline; "the name is not in the table at all" means the walk never reached
/// the write. Reporting them as one number would read as more discipline than
/// the evaluator actually exercises.
#[derive(Default)]
struct OurSide {
    committed: BTreeMap<String, String>,
    untrusted: BTreeSet<String>,
    absent: BTreeSet<String>,
}

/// What one case did across the whole sweep, for the census.
///
/// There is deliberately no "frozen" row — a name we committed the *same* value
/// for under two global sets that MSBuild disagreed about. That is the defect
/// this file exists to catch, and it cannot reach the census: the per-set
/// exactness assertion in [`sweep_case`] fires on the second set first.
#[derive(Default)]
struct CaseCensus {
    /// `(name, globals index)` pairs we committed a value for.
    committed: usize,
    /// Per name: how many global sets we committed under, and how many distinct
    /// values each side produced across the sweep.
    by_name: BTreeMap<String, NameCensus>,
}

/// One name's behaviour across the sweep.
struct NameCensus {
    /// Global sets we committed a value under, of [`global_sets`]'s length.
    committed_sets: usize,
    /// Global sets where the name was in our table but its provenance was
    /// untrusted — a modelled decline.
    untrusted_sets: usize,
    /// Global sets where the name never entered our table at all. Distinguished
    /// from `untrusted_sets` because they are different states of knowledge; see
    /// [`OurSide`].
    absent_sets: usize,
    /// Distinct values we committed. `0` when we never committed; `1` means we
    /// answered the same thing every time we answered at all.
    our_values: usize,
    /// Distinct values MSBuild produced. `>= 2` is what "this name depends on a
    /// caller-supplied global" means, stated by the reference rather than by us.
    their_values: usize,
}

/// The property names some global set in [`global_sets`] assigns.
///
/// These move across the sweep **by construction** — they are the input being
/// varied. Counting them as evidence that a document derives anything from a
/// global is circular, so every anti-vacuity floor here is computed over the
/// complement. (Left in the census's *report*, though: whether a document write
/// of one of these names wins is precisely the `TreatAsLocalProperty` question.)
fn swept_global_names() -> BTreeSet<String> {
    global_sets()
        .into_iter()
        .flatten()
        .map(|(name, _)| name)
        .collect()
}

impl CaseCensus {
    /// Names MSBuild moves across the sweep — the global-dependent ones.
    fn moving_in_msbuild(&self) -> impl Iterator<Item = (&String, &NameCensus)> {
        self.by_name.iter().filter(|(_, c)| c.their_values >= 2)
    }

    /// [`Self::moving_in_msbuild`] minus the swept inputs themselves: names the
    /// *document* derives from a global. The floors are on these.
    fn derived_moving(&self) -> usize {
        let swept = swept_global_names();
        self.moving_in_msbuild()
            .filter(|(name, _)| !swept.contains(*name))
            .count()
    }

    /// [`Self::tracked`] minus the swept inputs themselves.
    fn derived_tracked(&self) -> usize {
        let swept = swept_global_names();
        self.tracked().filter(|name| !swept.contains(*name)).count()
    }

    /// Moving names we committed at least two distinct values for: we tracked
    /// the dependence rather than declining. This is the number the anti-vacuity
    /// floors are on — a moving name we never commit exercises nothing, whereas
    /// one we commit two different values for is a name where the exactness
    /// assertion was genuinely tested against the global axis.
    fn tracked(&self) -> impl Iterator<Item = &String> {
        self.moving_in_msbuild()
            .filter(|(_, c)| c.our_values >= 2)
            .map(|(name, _)| name)
    }

    /// The rest: sound (nothing wrong was published), but the dependence cost us
    /// a value. This is the number that says what a "depends on a caller-supplied
    /// global" provenance dimension would buy.
    ///
    /// Over the **derived** domain, like its two siblings. Counting the swept
    /// names here would inflate it with something that is not a decline at all: a
    /// caller global is deliberately absent from `ParsedProject::properties`
    /// unless the document writes it, so `Configuration` and `Platform` would
    /// read as declined in almost every case and the census would report more
    /// pairs declined than moving.
    fn declined(&self) -> usize {
        self.derived_moving() - self.derived_tracked()
    }

    /// One entry per global-dependent name, for the report: how many distinct
    /// values MSBuild produced, and what we did — committed (with how many
    /// distinct values, over how many global sets), declined as untrusted, or
    /// never reached at all.
    fn render(&self) -> String {
        self.moving_in_msbuild()
            .map(|(name, c)| {
                format!(
                    "{name}(theirs {}; ours {} val in {} set, {} untrusted, {} absent)",
                    c.their_values, c.our_values, c.committed_sets, c.untrusted_sets, c.absent_sets
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Run one case over the whole global sweep, asserting the contract at every
/// point and returning its census row.
fn sweep_case(
    oracle: &mut Oracle,
    dir: &Path,
    xml: &str,
    read_names: &[&str],
    pins: &[(&str, &str)],
    with_sdk: bool,
) -> CaseCensus {
    let project_path = dir.join("Demo.fsproj");
    std::fs::write(&project_path, xml).expect("write project");

    let sets = global_sets();
    let mut ours_by_set: Vec<OurSide> = Vec::new();
    let mut theirs_by_set: Vec<Option<HashMap<String, String>>> = Vec::new();

    for globals in &sets {
        let (ours, theirs) = evaluate_both(oracle, dir, xml, globals, read_names, with_sdk);
        if let Some(theirs) = &theirs {
            for (name, our_value) in &ours.committed {
                let their_value = theirs
                    .get(name)
                    .expect("oracle answers for every requested name");
                assert_eq!(
                    our_value, their_value,
                    "certain-implies-exact violated for ${{{name}}} under globals \
                     {globals:?}: we evaluate it to {our_value:?}, MSBuild to \
                     {their_value:?}\n--- xml ---\n{xml}"
                );
            }
        }
        ours_by_set.push(ours);
        theirs_by_set.push(theirs);
    }

    // The case's own claim about what it demonstrates, checked against MSBuild
    // under the globals the LSP injects. `PIN_GLOBALS` is one of the swept sets,
    // so this costs no extra evaluation — it reads the answer already collected.
    if !pins.is_empty() {
        let pin_index = sets
            .iter()
            .position(|set| {
                set.iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .eq(PIN_GLOBALS.iter().copied())
            })
            .expect("PIN_GLOBALS is one of the swept global sets");
        let theirs = theirs_by_set[pin_index]
            .as_ref()
            .expect("MSBuild evaluates a pinned case under the LSP's own globals");
        for (name, want) in pins {
            assert_eq!(
                theirs.get(*name).map(String::as_str),
                Some(*want),
                "the pinned MSBuild value for ${{{name}}} under {PIN_GLOBALS:?} is \
                 wrong — this case does not demonstrate the route its comment \
                 claims\n--- xml ---\n{xml}"
            );
        }
    }

    let mut census = CaseCensus::default();
    for ours in &ours_by_set {
        census.committed += ours.committed.len();
    }
    for name in read_names {
        let their_values: BTreeSet<&String> = theirs_by_set
            .iter()
            .flatten()
            .filter_map(|t| t.get(*name))
            .collect();
        let our_values: BTreeSet<&String> = ours_by_set
            .iter()
            .filter_map(|o| o.committed.get(*name))
            .collect();
        census.by_name.insert(
            (*name).to_string(),
            NameCensus {
                committed_sets: ours_by_set
                    .iter()
                    .filter(|o| o.committed.contains_key(*name))
                    .count(),
                untrusted_sets: ours_by_set
                    .iter()
                    .filter(|o| o.untrusted.contains(*name))
                    .count(),
                absent_sets: ours_by_set
                    .iter()
                    .filter(|o| o.absent.contains(*name))
                    .count(),
                our_values: our_values.len(),
                their_values: their_values.len(),
            },
        );
    }
    census
}

/// The hand-written route list: every way a global was found to reach a
/// committed value while the output-directory work was being attempted.
#[test]
fn every_route_from_a_global_is_exact_or_declined() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();

    let mut committed = 0usize;
    let mut moving = 0usize;
    let mut tracked = 0usize;
    let mut declined = 0usize;
    for case in CASES {
        let dir = tmp.path().join(case.name);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in case.files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        let census = sweep_case(&mut oracle, &dir, case.xml, READ_NAMES, case.pins, false);
        eprintln!(
            "  {:<28} committed {:>3}  {}",
            case.name,
            census.committed,
            census.render()
        );
        committed += census.committed;
        moving += census.derived_moving();
        tracked += census.derived_tracked();
        declined += census.declined();
    }

    eprintln!(
        "global-perturbation routes: {} cases × {} global sets — {committed} committed \
         (name, globals) pairs; {moving} (case, name) pairs move in MSBuild, of which \
         we track {tracked} and decline {declined}",
        CASES.len(),
        global_sets().len(),
    );

    // Anti-vacuity. A harness that declined everything would satisfy the
    // one-sided contract while measuring nothing, and so would one whose globals
    // never changed an answer.
    assert!(
        committed >= 100,
        "too few committed (name, globals) pairs ({committed}) — the walker may \
         have started declining everything, which passes vacuously"
    );
    // The floor that matters is on `tracked`, not on `moving`: a name MSBuild
    // moves but we never commit exercises nothing, whereas one we commit *two
    // different values* for is a name where the exactness assertion above was
    // genuinely tested against the global axis. `moving` alone would stay
    // satisfied by a walker that declined every one of them.
    assert!(
        tracked >= 10,
        "only {tracked} (case, name) pairs were both committed and global-dependent \
         — the exactness check is no longer being exercised on the global axis, so \
         this harness is passing vacuously"
    );
}

/// The item side. The Compile order is what the semantic layer folds over, so a
/// global-gated `<Compile>` is a route from an editor-guessed global straight
/// into which files exist as far as the LSP is concerned.
#[test]
fn compile_items_are_exact_or_declined_under_every_global_set() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();

    // Literal includes only — globbing is a separate seam with its own
    // differential, and a resolver is not wired here.
    let documents: &[(&str, &str)] = &[
        (
            "item-condition",
            "<Project>\n  <ItemGroup>\n    <Compile Include=\"Always.fs\" />\n    <Compile Include=\"DebugOnly.fs\" Condition=\"'$(Configuration)' == 'Debug'\" />\n  </ItemGroup>\n</Project>\n",
        ),
        (
            "group-condition",
            "<Project>\n  <ItemGroup Condition=\"'$(Platform)' == 'x64'\">\n    <Compile Include=\"Wide.fs\" />\n  </ItemGroup>\n  <ItemGroup>\n    <Compile Include=\"Always.fs\" />\n  </ItemGroup>\n</Project>\n",
        ),
        (
            "include-interpolates-global",
            "<Project>\n  <ItemGroup>\n    <Compile Include=\"$(Configuration).fs\" />\n    <Compile Include=\"Always.fs\" />\n  </ItemGroup>\n</Project>\n",
        ),
        (
            "remove-gated-on-global",
            "<Project>\n  <ItemGroup>\n    <Compile Include=\"Always.fs\" />\n    <Compile Include=\"DebugOnly.fs\" />\n    <Compile Remove=\"DebugOnly.fs\" Condition=\"'$(Configuration)' == 'Release'\" />\n  </ItemGroup>\n</Project>\n",
        ),
    ];

    let sets = global_sets();
    let mut committed = 0usize;
    let mut moving = 0usize;
    for (name, xml) in documents {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let project_path = dir.join("Demo.fsproj");
        std::fs::write(&project_path, xml).unwrap();

        let mut theirs_by_set: BTreeSet<Vec<String>> = BTreeSet::new();
        for globals in &sets {
            let parsed = parse_fsproj_with_imports(
                xml,
                &project_path,
                &globals.iter().cloned().collect(),
                &common::oracle_environment(),
                None,
                None,
            )
            .expect("well-formed XML parses");

            let theirs: Vec<String> = oracle
                .items(xml, &project_path, "Compile", globals)
                .expect("MSBuild evaluates these documents")
                .iter()
                // `FullPath` on both sides, spelled the same way: the walker
                // yields the include relative to the project, so compare the
                // file names it resolves to.
                .map(|p| {
                    Path::new(p)
                        .file_name()
                        .expect("an item resolves to a file name")
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            theirs_by_set.insert(theirs.clone());

            // A diagnostic or an uncertain item set withdraws the claim.
            if !parsed.diagnostics.is_empty() || parsed.items_uncertain {
                continue;
            }
            let ours: Vec<String> = parsed
                .items
                .iter()
                .map(|i| {
                    i.include
                        .file_name()
                        .expect("an item resolves to a file name")
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            assert_eq!(
                ours, theirs,
                "certain-implies-exact violated for the Compile order under globals \
                 {globals:?}\n--- xml ---\n{xml}"
            );
            committed += 1;
        }
        eprintln!(
            "  {name:<28} MSBuild produces {} distinct Compile orders across {} global sets",
            theirs_by_set.len(),
            sets.len()
        );
        if theirs_by_set.len() >= 2 {
            moving += 1;
        }
    }

    eprintln!(
        "global-perturbation items: {committed} committed (document, globals) pairs; \
         {moving} of {} documents change Compile order across the sweep",
        documents.len()
    );
    assert!(
        committed >= 10,
        "too few committed (document, globals) pairs ({committed})"
    );
    assert_eq!(
        moving,
        documents.len(),
        "every item document here is written to depend on a global, so all of them \
         must move across the sweep — if one stopped, it is no longer testing the \
         axis"
    );
}

/// Fixed-seed sweep over generated documents that compose the routes above:
/// several gated writes, reading each other, with the gates drawn from a pool of
/// global-dependent and global-independent conditions.
///
/// The corner list pins the routes someone already thought of. This is the part
/// that can find one nobody did — which is the whole reason the design note asks
/// for a machine here rather than another review round.
#[test]
fn fixed_seed_composed_documents_are_exact_or_declined() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();
    let mut rng = SplitMix64(0x91ba_10c0_ffee);

    // Gates: some depend on a global, some do not, so a generated document may
    // be genuinely invariant — a sweep of only-dependent documents would never
    // exercise the "we correctly committed the same value" path.
    const GATES: &[&str] = &[
        "",
        "'$(Configuration)' == 'Release'",
        "'$(Configuration)' != 'Release'",
        "'$(Platform)' == 'x64'",
        "'$(Configuration)' == ''",
        "'$(Out)' == ''",
        "'$(Helper)' != ''",
        "'x' == 'x'",
        "Exists('rel.props')",
        "'$(Configuration)$(Platform)' == 'Releasex64'",
    ];
    const BODIES: &[&str] = &[
        "literal",
        "$(Configuration)",
        "$(Platform)",
        "$(Configuration)/$(Platform)",
        "$(Helper)",
        "[$(Out)]",
        "$(BorzoiProbe)",
        "$(Configuration.Length)",
    ];
    const WRITTEN: &[&str] = &["Out", "Helper", "Configuration", "Platform"];

    const CASES_N: usize = 250;
    let mut committed = 0usize;
    let mut moving = 0usize;
    let mut tracked = 0usize;
    for case in 0..CASES_N {
        let writes = 1 + rng.below(4);
        let mut xml = String::from("<Project>\n  <PropertyGroup>\n");
        for _ in 0..writes {
            let name = rng.pick(WRITTEN);
            let gate = rng.pick(GATES);
            let body = rng.pick(BODIES);
            if gate.is_empty() {
                xml.push_str(&format!("    <{name}>{body}</{name}>\n"));
            } else {
                // `&` first, or the ampersand of a just-inserted `&lt;` would be
                // escaped again.
                let gate = gate.replace('&', "&amp;").replace('<', "&lt;");
                xml.push_str(&format!(
                    "    <{name} Condition=\"{gate}\">{body}</{name}>\n"
                ));
            }
        }
        xml.push_str("  </PropertyGroup>\n</Project>\n");

        let dir = tmp.path().join(format!("case{case}"));
        std::fs::create_dir_all(&dir).unwrap();
        // Some gates probe for it; half the cases have it, so `Exists(...)`
        // splits the population rather than being constant.
        if case % 2 == 0 {
            std::fs::write(dir.join("rel.props"), IMPORT_SETS_OUT).unwrap();
        }

        let census = sweep_case(&mut oracle, &dir, &xml, READ_NAMES, &[], false);
        committed += census.committed;
        moving += census.derived_moving();
        tracked += census.derived_tracked();
    }

    eprintln!(
        "global-perturbation sweep: {CASES_N} generated documents × {} global sets — \
         {committed} committed (name, globals) pairs, {moving} (case, name) pairs \
         move in MSBuild, of which we track {tracked}",
        global_sets().len()
    );
    assert!(
        committed >= 1000,
        "too few committed (name, globals) pairs ({committed}) over {CASES_N} documents"
    );
    assert!(
        tracked >= 100,
        "only {tracked} generated (case, name) pairs were both committed and \
         global-dependent — the generator has drifted away from the axis this sweep \
         exists to test"
    );
}

/// The **real SDK chain**, swept over the same globals.
///
/// This is the part that matters most, and the part no synthetic document can
/// stand in for. Every route the output-directory work found leaking lived in
/// the SDK's own props, not in the user's project: `Microsoft.Common.props`
/// writes `OutputPath` from `$(Configuration)` and `$(Platform)` and then
/// `OutDir` from `OutputPath`, `DefineConstants` picks up `DEBUG`/`TRACE` from
/// the configuration, and hook-point imports are gated on properties a caller
/// can set. Those are the values the LSP publishes; whether they are fixed facts
/// or artefacts of the globals it guessed is exactly the question here.
///
/// Both sides resolve the same installed SDK — ours through [`resolve_sdk`] over
/// `DOTNET_ROOT`, MSBuild's through its own resolver under MSBuildLocator — so a
/// divergence is about the walk, not about which chain was walked.
#[test]
fn sdk_chain_properties_are_exact_or_declined_under_every_global_set() {
    // The names an SDK project derives from `Configuration`/`Platform`, plus the
    // ones the blocked output-directory feature would have had to commit.
    const SDK_NAMES: &[&str] = &[
        "OutDir",
        "OutputPath",
        "BaseOutputPath",
        "IntermediateOutputPath",
        "DefineConstants",
        "AssemblyName",
        "TargetFramework",
        "Configuration",
        "Platform",
        "PlatformTarget",
        "Optimize",
        "DebugSymbols",
    ];

    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();

    let cases: &[Case] = &[
        // The plain project: everything below comes from the SDK, not the
        // document.
        Case {
            name: "sdk-plain",
            xml: "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net10.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n",
            files: &[],
            pins: &[],
        },
        // The user redirects output through `OutputPath`, interpolating the
        // configuration — the shape the blocked feature had to answer for.
        Case {
            name: "sdk-outputpath",
            xml: "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net10.0</TargetFramework>\n    <OutputPath>artifacts/$(Configuration)/</OutputPath>\n  </PropertyGroup>\n</Project>\n",
            files: &[],
            pins: &[],
        },
        // …and through `BaseOutputPath`, which the SDK then composes with the
        // configuration itself.
        Case {
            name: "sdk-baseoutputpath",
            xml: "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net10.0</TargetFramework>\n    <BaseOutputPath>build/</BaseOutputPath>\n  </PropertyGroup>\n</Project>\n",
            files: &[],
            pins: &[],
        },
        // A `Directory.Build.props` above the project, itself gated on a global:
        // the file the walker must find *and* condition correctly.
        Case {
            name: "sdk-directory-build-props",
            xml: "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net10.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n",
            files: &[(
                "Directory.Build.props",
                "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <BaseOutputPath>shipping/</BaseOutputPath>\n  </PropertyGroup>\n</Project>\n",
            )],
            // The SDK's derived paths are installation- and separator-dependent,
            // so nothing here is pinned to a literal. That the sidecar file is
            // genuinely reached is asserted instead by the `BaseOutputPath`
            // movement floor below, which only this case can satisfy.
            pins: &[],
        },
        // `DefineConstants` is a value the LSP feeds straight into the lexer's
        // `#if` handling, and the SDK derives it from the configuration.
        Case {
            name: "sdk-define-constants",
            xml: "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net10.0</TargetFramework>\n    <DefineConstants>$(DefineConstants);MINE</DefineConstants>\n  </PropertyGroup>\n</Project>\n",
            files: &[],
            pins: &[],
        },
    ];

    let mut committed = 0usize;
    let mut moving = 0usize;
    let mut tracked = 0usize;
    let mut sidecar_route_live = false;
    for case in cases {
        let dir = tmp.path().join(case.name);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in case.files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        let census = sweep_case(&mut oracle, &dir, case.xml, SDK_NAMES, case.pins, true);
        eprintln!(
            "  {:<28} committed {:>3}  {}",
            case.name,
            census.committed,
            census.render()
        );
        committed += census.committed;
        moving += census.derived_moving();
        tracked += census.derived_tracked();
        // `BaseOutputPath` moves only where a `Directory.Build.props` gated on
        // the configuration wrote it, so this is the named proof that the sidecar
        // was found and conditioned — the one SDK route whose liveness a
        // literal pin cannot state.
        if case.name == "sdk-directory-build-props" {
            sidecar_route_live = census
                .moving_in_msbuild()
                .any(|(name, _)| name == "BaseOutputPath");
        }
    }

    eprintln!(
        "global-perturbation SDK chain: {} projects × {} global sets — {committed} \
         committed (name, globals) pairs; {moving} (case, name) pairs move in \
         MSBuild, of which we track {tracked}",
        cases.len(),
        global_sets().len(),
    );

    // The floor here is on *MSBuild's* movement, deliberately — unlike the
    // synthetic tests, whose inputs we author. What our side commits over the
    // real SDK chain is a measurement, not a target: it may legitimately be zero
    // (the chain trips uncertainty causes today), and pinning it would turn a
    // measurement into a ratchet nobody agreed to. What must not silently drift
    // is the *input*: an SDK whose properties stopped depending on the
    // configuration would make this whole test vacuous, and that is worth being
    // told about.
    //
    // `derived_moving` excludes `Configuration`/`Platform` themselves. They move
    // by construction — they *are* what the sweep varies — so counting them here
    // would let this floor stay green on five projects that had stopped reaching
    // the SDK chain entirely.
    assert!(
        moving >= 5,
        "the real SDK chain moved only {moving} SDK-derived (case, name) pairs \
         across the global sweep — either the SDK stopped deriving these from the \
         configuration, or the sweep is no longer reaching it"
    );
    assert!(
        sidecar_route_live,
        "`BaseOutputPath` did not move for `sdk-directory-build-props`, so its \
         configuration-gated `Directory.Build.props` is no longer being found and \
         conditioned — that case is testing nothing"
    );
}
