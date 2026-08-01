//! Differential test: the **framework pair the SDK derives** from
//! `$(TargetFramework)`, swept over the dimensions that decide it, against the
//! real MSBuild evaluator.
//!
//! ## Why this harness exists
//!
//! `Microsoft.NET.TargetFrameworkInference.targets` computes
//! `TargetFrameworkIdentifier` / `TargetFrameworkVersion` **inside
//! `Sdk.targets`** — after the project body — and `Microsoft.Common.targets`
//! imports `Directory.Build.targets` after *that*. So one name has more than one
//! right answer in a single evaluation, depending on where it is read, and every
//! interesting question about it is a question about a *combination*: which SDK
//! resolved, how the TFM was spelled, whether the author pre-set either half,
//! and which position does the reading.
//!
//! That product is too big to adjudicate by hand, and hand-adjudicating it is
//! exactly what went wrong. Three review rounds on this feature each found the
//! same species of defect: a hand-written fixture asserting behaviour real
//! MSBuild does not have — a synthetic no-op SDK that "derives" (it does not: a
//! non-.NET SDK runs no inference), a whitespace-padded TFM normalised to a
//! classification (`" net472 "` is `Unsupported`, not `.NETFramework`), a pair
//! half-written where MSBuild writes both. Each was fixed as an instance; the
//! next round found the next one. The defect was never in any single
//! expectation, it was in adjudicating expectations by reasoning at all.
//!
//! So nothing here encodes what I believe MSBuild does. The matrix is generated
//! and **MSBuild is asked**, case by case.
//!
//! ## How a *position* is diffed at all
//!
//! MSBuild reports the property table at the *end* of evaluation, so it cannot
//! be asked "what was `$(TargetFrameworkIdentifier)` back in the body?".
//! The matrix therefore plants a **witness** at each position — an ordinary
//! property whose body is `[$(TargetFrameworkIdentifier)]`, written into
//! `Directory.Build.props`, the project body, and `Directory.Build.targets`.
//! Property writes are last-write-wins in evaluation order, so a witness's final
//! value is a verbatim record of what a condition at that position would have
//! seen. Witnesses are bracketed so the XML layer's whitespace rules cannot
//! collapse a whitespace-only expansion into a different node, which would make
//! the two sides disagree for a reason unrelated to the subject.
//!
//! ## The contract
//!
//! The crate's usual one, per name:
//!
//! - We **commit** a witness (it is in `ParsedProject::properties` with trusted
//!   provenance) ⟹ MSBuild must evaluate the same document at the same path to
//!   the byte-identical value.
//! - We **decline** ⟹ no claim. Partiality is the fail-safe superset.
//! - MSBuild **rejects** the document ⟹ we must not have committed anything.
//!
//! Anti-vacuity is the other half, and it is what makes this a test of the
//! derivation rather than of our willingness to decline: the corpus shape (a
//! real-SDK `net472` project reading the identifier from
//! `Directory.Build.targets`) is **required** to commit. Its expected value is
//! still never written down here — the floor asserts that we commit, and
//! certain-implies-exact asserts what to.
//!
//! ## The SDK axis, and what it can and cannot reach
//!
//! Only SDKs MSBuild itself can resolve are in the matrix: the real
//! `Microsoft.NET.Sdk`, and no SDK at all. A *synthetic* SDK — the shape the
//! earlier hand-written fixtures used, materialised in a tempdir and fed to our
//! walker through a resolver closure — is unreachable by the oracle, which has
//! no resolver hook and would fail to resolve the name. That is not a gap to
//! work around; it is the finding. The configuration those fixtures asserted
//! about is precisely the one whose behaviour cannot be checked, which is how
//! they came to assert a falsehood. Anything that must hold for a synthetic SDK
//! is a claim about *our* gate, and belongs in a unit test that says so.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use borzoi_msbuild::{parse_fsproj_with_imports, resolve_sdk, workloads};
use common::Oracle;
use tempfile::TempDir;

/// Where in the import order a condition reads the framework pair.
///
/// Our walker reaches `Directory.Build.targets` the way MSBuild does — through
/// the SDK chain's own import, from inside `Microsoft.Common.targets` — rather
/// than by splicing it after the body, so it lands on the far side of the
/// inference point. That correspondence is a claim, and it is this axis that
/// checks it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Position {
    Props,
    Body,
    Targets,
}

const POSITIONS: [Position; 3] = [Position::Props, Position::Body, Position::Targets];

impl Position {
    fn tag(self) -> &'static str {
        match self {
            Position::Props => "Props",
            Position::Body => "Body",
            Position::Targets => "Targets",
        }
    }
}

/// The names `Microsoft.NET.TargetFrameworkInference.targets` derives.
const DERIVED: [&str; 2] = ["TargetFrameworkIdentifier", "TargetFrameworkVersion"];

/// The witness property recording `derived` as seen at `pos`.
///
/// The `Borzoi` prefix keeps these disjoint from MSBuild reserved names and from
/// anything the process environment might hold — MSBuild folds the environment
/// in as properties, and a collision would make the two sides disagree for a
/// reason unrelated to the subject.
fn witness_name(pos: Position, derived: &str) -> String {
    let suffix = derived
        .strip_prefix("TargetFramework")
        .expect("derived names are TargetFramework-prefixed");
    format!("BorzoiWitness{}{suffix}", pos.tag())
}

/// The positions that exist under `sdk`.
///
/// A bare `<Project>` with no `Sdk` attribute imports **nothing** — MSBuild
/// reaches `Directory.Build.props` / `.targets` only through
/// `Microsoft.Common.props` / `.targets`, which such a document never pulls in.
/// (Old-style projects import `Microsoft.Common.props` explicitly, which is why
/// the file works for them.) Our walker splices both unconditionally, so for an
/// SDK-less project it commits values for properties MSBuild's table does not
/// contain at all.
///
/// That is a real wrong commit and it is **not** the subject here: it is about
/// the splice's existence, not about the framework pair, and it is unchanged by
/// this file's fix. It is excluded by not *planting* the out-of-body witnesses
/// under [`Sdk::None`] — a restriction of the input to shapes MSBuild has a
/// position for, decided by MSBuild's behaviour rather than by ours. The
/// in-body comparison stays fully live for every SDK-less case.
///
/// Left as a gap on purpose: making the props splice conditional needs the
/// document's own `<Import>`s walked first, and the props splice has to happen
/// *before* the body that contains them — the trick that fixed the targets side
/// does not transfer. `docs/fsproj-tfm-selection-plan.md` records it.
fn positions_for(sdk: Sdk) -> &'static [Position] {
    match sdk {
        Sdk::Real => &POSITIONS,
        Sdk::None => &[Position::Body],
    }
}

/// Every witness name under `sdk`, in a stable order.
fn witness_names(sdk: Sdk) -> Vec<String> {
    positions_for(sdk)
        .iter()
        .flat_map(|pos| DERIVED.iter().map(move |d| witness_name(*pos, d)))
        .collect()
}

/// The `<PropertyGroup>` planting both witnesses for one position.
fn witness_group(pos: Position) -> String {
    let mut xml = String::from("  <PropertyGroup>\n");
    for derived in DERIVED {
        let name = witness_name(pos, derived);
        xml.push_str(&format!("    <{name}>[$({derived})]</{name}>\n"));
    }
    xml.push_str("  </PropertyGroup>\n");
    xml
}

/// How the project declares (or fails to declare) its framework.
#[derive(Clone, Copy)]
struct Declaration {
    name: &'static str,
    /// The `<PropertyGroup>` body declaring the framework, or `""` for none.
    body: &'static str,
}

/// The TFM spellings, reaching well past what our classifier models.
///
/// Under certain-implies-exact a decline is free and only a wrong *commit*
/// fails, so there is no reason to restrict the inputs to spellings we
/// understand — and the ones we do not are where the wrong commits were.
const DECLARATIONS: &[Declaration] = &[
    Declaration {
        name: "none",
        body: "",
    },
    Declaration {
        name: "net10.0",
        body: "<TargetFramework>net10.0</TargetFramework>",
    },
    Declaration {
        name: "net8.0",
        body: "<TargetFramework>net8.0</TargetFramework>",
    },
    // The corpus shape: `FSharp.Profiles.props` gates on the identifier being
    // `.NETFramework`, and published two `#if` symbols the real build never
    // defines because we read the identifier as empty at the targets position.
    Declaration {
        name: "net472",
        body: "<TargetFramework>net472</TargetFramework>",
    },
    Declaration {
        name: "netstandard2.0",
        body: "<TargetFramework>netstandard2.0</TargetFramework>",
    },
    Declaration {
        name: "netcoreapp3.1",
        body: "<TargetFramework>netcoreapp3.1</TargetFramework>",
    },
    // A platform-qualified TFM: the identifier/version pair comes from the
    // framework half, and a third name (`TargetPlatformIdentifier`) appears.
    Declaration {
        name: "net9.0-windows",
        body: "<TargetFramework>net9.0-windows</TargetFramework>",
    },
    // Whitespace padding. MSBuild does **not** trim before classifying, so this
    // is `Unsupported` — a normalising classifier invents a value here.
    Declaration {
        name: "padded",
        body: "<TargetFramework> net472 </TargetFramework>",
    },
    // Case variance, a dotted 4.x spelling, and outright garbage.
    Declaration {
        name: "upper",
        body: "<TargetFramework>NET472</TargetFramework>",
    },
    Declaration {
        name: "dotted",
        body: "<TargetFramework>net4.7.2</TargetFramework>",
    },
    Declaration {
        name: "garbage",
        body: "<TargetFramework>garbage</TargetFramework>",
    },
    Declaration {
        name: "bare-net",
        body: "<TargetFramework>net</TargetFramework>",
    },
    // A semicolon in the *singular* property: not a list, but everything
    // downstream that splits on `;` will treat it as one.
    Declaration {
        name: "semicolon",
        body: "<TargetFramework>net10.0;net8.0</TargetFramework>",
    },
    // Empty singular: the inference's own gate is `'$(TargetFramework)' != ''`.
    Declaration {
        name: "empty",
        body: "<TargetFramework></TargetFramework>",
    },
    // Cross-targeting outer build: plural with no singular, so the gate does not
    // fire and the pair stays unset for the whole outer evaluation.
    Declaration {
        name: "plural",
        body: "<TargetFrameworks>net8.0;net9.0</TargetFrameworks>",
    },
    // Plural *and* singular, the inner-build shape spelled in the document.
    Declaration {
        name: "plural-and-singular",
        body: "<TargetFrameworks>net8.0;net9.0</TargetFrameworks>\n    \
               <TargetFramework>net8.0</TargetFramework>",
    },
];

/// What the author pre-writes of the pair, in the body, before inference runs.
///
/// The inference gate is `('$(TargetFrameworkIdentifier)' == '' or
/// '$(TargetFrameworkVersion)' == '')` — an **or**, so a half-written pair still
/// fires it, and what happens to the written half is the question.
#[derive(Clone, Copy)]
struct Preset {
    name: &'static str,
    body: &'static str,
}

const PRESETS: &[Preset] = &[
    Preset {
        name: "none",
        body: "",
    },
    Preset {
        name: "identifier",
        body: "<TargetFrameworkIdentifier>.NETPortable</TargetFrameworkIdentifier>",
    },
    Preset {
        name: "version",
        body: "<TargetFrameworkVersion>v4.6</TargetFrameworkVersion>",
    },
    Preset {
        name: "both",
        body: "<TargetFrameworkIdentifier>.NETPortable</TargetFrameworkIdentifier>\n    \
               <TargetFrameworkVersion>v4.6</TargetFrameworkVersion>",
    },
    // Written but *empty*: distinct from absent to a `== ''` gate only in that
    // the write happened, which is invisible to the gate — so this must behave
    // as `none`, and a model keyed on "is the name present" gets it backwards.
    Preset {
        name: "empty-identifier",
        body: "<TargetFrameworkIdentifier></TargetFrameworkIdentifier>",
    },
    // Whitespace-only: MSBuild's XML layer stores this as `""`, so the gate
    // fires; a walker that stored `" "` would see a non-empty value.
    Preset {
        name: "blank-identifier",
        body: "<TargetFrameworkIdentifier>   </TargetFrameworkIdentifier>",
    },
];

/// Which SDK the document declares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sdk {
    /// The real `Microsoft.NET.Sdk`, resolved by both sides from `DOTNET_ROOT`.
    Real,
    /// No `Sdk` attribute at all: nothing runs the inference at any position.
    None,
}

impl Sdk {
    fn tag(self) -> &'static str {
        match self {
            Sdk::Real => "real-sdk",
            Sdk::None => "no-sdk",
        }
    }
}

/// One point of the matrix: the three input axes that build a document.
///
/// Bundled rather than passed loose so that adding an axis is one edit at the
/// definition instead of a signature change at every call site.
#[derive(Clone, Copy)]
struct Point {
    sdk: Sdk,
    decl: Declaration,
    preset: Preset,
}

impl Point {
    /// A human-readable label, so a failure names the case.
    fn label(&self) -> String {
        format!(
            "{}/{}/preset={}",
            self.sdk.tag(),
            self.decl.name,
            self.preset.name
        )
    }
}

/// Assemble the project document for one matrix point.
fn project_xml(sdk: Sdk, decl: Declaration, preset: Preset) -> String {
    let open = match sdk {
        Sdk::Real => "<Project Sdk=\"Microsoft.NET.Sdk\">",
        Sdk::None => "<Project>",
    };
    let mut xml = String::from(open);
    xml.push('\n');
    xml.push_str("  <PropertyGroup>\n");
    if !decl.body.is_empty() {
        xml.push_str(&format!("    {}\n", decl.body));
    }
    if !preset.body.is_empty() {
        xml.push_str(&format!("    {}\n", preset.body));
    }
    xml.push_str("  </PropertyGroup>\n");
    // The body witness sits after the declaration, so it records what a
    // condition written alongside the author's own properties would see.
    xml.push_str(&witness_group(Position::Body));
    xml.push_str("</Project>\n");
    xml
}

/// One evaluation's outcome on our side, split three ways (as in
/// `fsproj_global_perturbation_diff`): `untrusted` and `absent` both make no
/// claim, but they are different states of knowledge and the census wants both.
#[derive(Default)]
struct OurSide {
    committed: BTreeMap<String, String>,
    untrusted: BTreeSet<String>,
    absent: BTreeSet<String>,
}

/// Materialise one matrix point in `dir` and evaluate it on both sides.
///
/// Returns our outcome and MSBuild's table, or `None` from MSBuild when it
/// rejects the document — which the contract turns into "we committed nothing".
fn evaluate_both(
    oracle: &mut Oracle,
    dir: &Path,
    point: Point,
    globals: &[(String, String)],
) -> (String, OurSide, Option<HashMap<String, String>>) {
    let Point { sdk, decl, preset } = point;
    let xml = project_xml(sdk, decl, preset);
    let project_path = dir.join("Demo.fsproj");
    std::fs::write(&project_path, &xml).expect("write project");
    // The two out-of-body witness positions. Both files are always present, so
    // the only thing varying across the matrix is the subject.
    std::fs::write(
        dir.join("Directory.Build.props"),
        format!("<Project>\n{}</Project>\n", witness_group(Position::Props)),
    )
    .expect("write Directory.Build.props");
    std::fs::write(
        dir.join("Directory.Build.targets"),
        format!(
            "<Project>\n{}</Project>\n",
            witness_group(Position::Targets)
        ),
    )
    .expect("write Directory.Build.targets");

    let with_sdk = sdk == Sdk::Real;
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

    let extra: HashMap<String, String> = globals.iter().cloned().collect();
    let parsed = parse_fsproj_with_imports(
        &xml,
        &project_path,
        &extra,
        &common::oracle_environment(),
        with_sdk.then_some(&resolver as &borzoi_msbuild::SdkResolver<'_>),
        None,
    )
    .expect("well-formed XML parses");

    let names = witness_names(sdk);
    let mut ours = OurSide::default();
    for name in &names {
        match parsed.properties.get(name) {
            None => {
                ours.absent.insert(name.clone());
            }
            Some(_) if parsed.property_provenance_untrusted(name) => {
                ours.untrusted.insert(name.clone());
            }
            Some(value) => {
                ours.committed.insert(name.clone(), value.clone());
            }
        }
    }

    let theirs = oracle.project(&xml, &names, Some(&project_path), globals);
    (xml, ours, theirs)
}

/// One certain-implies-exact violation, held rather than panicked on.
///
/// The sweep collects instead of failing at the first case deliberately: these
/// divergences come in families (one modelling mistake produces a whole column
/// of the matrix), and a harness that stops at the first row makes a family look
/// like an instance — which is how this feature came to be fixed one hand-picked
/// case at a time. One run should print the worklist.
struct Divergence {
    case: String,
    name: String,
    ours: String,
    theirs: String,
}

/// Check the contract for one matrix point, folding results into `census` and
/// any violations into `divergences`.
fn check_point(
    oracle: &mut Oracle,
    dir: &Path,
    point: Point,
    globals: &[(String, String)],
    census: &mut Census,
    divergences: &mut Vec<Divergence>,
) {
    let (xml, ours, theirs) = evaluate_both(oracle, dir, point, globals);
    let case = point.label();

    let Some(theirs) = theirs else {
        assert!(
            ours.committed.is_empty(),
            "certain-implies-exact violated for {case}: MSBuild rejects this \
             project, but we committed {:?}\n--- xml ---\n{xml}",
            ours.committed
        );
        census.msbuild_rejected += 1;
        return;
    };

    for (name, our_value) in &ours.committed {
        let their_value = theirs
            .get(name)
            .expect("oracle answers for every requested name");
        if our_value != their_value {
            divergences.push(Divergence {
                case: case.clone(),
                name: name.clone(),
                ours: our_value.clone(),
                theirs: their_value.clone(),
            });
        }
    }
    census.committed += ours.committed.len();
    census.untrusted += ours.untrusted.len();
    census.absent += ours.absent.len();
    // A witness whose value differs between two positions is the whole point of
    // the position axis; count the documents where MSBuild moves it, so a run in
    // which inference never fired anywhere is visible rather than silently
    // passing.
    for derived in DERIVED {
        let values: BTreeSet<&String> = POSITIONS
            .iter()
            .filter_map(|pos| theirs.get(&witness_name(*pos, derived)))
            .collect();
        if values.len() > 1 {
            census.msbuild_moved_across_positions += 1;
            let tracked = POSITIONS
                .iter()
                .all(|pos| ours.committed.contains_key(&witness_name(*pos, derived)));
            if tracked {
                census.tracked_across_positions += 1;
            }
        }
    }
}

/// What the sweep did, so a mass decline is visible rather than silently green.
#[derive(Default)]
struct Census {
    committed: usize,
    untrusted: usize,
    absent: usize,
    msbuild_rejected: usize,
    /// Documents where MSBuild's own value for a derived name differs between
    /// two witness positions — i.e. where inference actually fired mid-walk.
    msbuild_moved_across_positions: usize,
    /// …and we committed at *every* position for that name, so the exactness
    /// check above was genuinely exercised on the position axis.
    tracked_across_positions: usize,
}

/// Print every collected divergence and fail if there are any.
fn assert_no_divergences(what: &str, divergences: &[Divergence]) {
    if divergences.is_empty() {
        return;
    }
    let mut report = format!(
        "certain-implies-exact violated in {} of the {what} cases:\n",
        divergences.len()
    );
    for d in divergences {
        report.push_str(&format!(
            "  {}  ${}: ours {:?}  msbuild {:?}\n",
            d.case, d.name, d.ours, d.theirs
        ));
    }
    panic!("{report}");
}

impl Census {
    fn report(&self, what: &str) {
        eprintln!(
            "{what}: committed {}, untrusted {}, absent {}, msbuild-rejected {}; \
             positions moved {} of which tracked {}",
            self.committed,
            self.untrusted,
            self.absent,
            self.msbuild_rejected,
            self.msbuild_moved_across_positions,
            self.tracked_across_positions,
        );
    }
}

/// The full matrix: SDK kind × TFM spelling × pre-set pair, read at three
/// positions in one evaluation.
#[test]
fn derived_framework_pair_is_exact_or_declined_across_the_matrix() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut census = Census::default();
    let mut divergences = Vec::new();

    for sdk in [Sdk::Real, Sdk::None] {
        for decl in DECLARATIONS {
            for preset in PRESETS {
                let point = Point {
                    sdk,
                    decl: *decl,
                    preset: *preset,
                };
                check_point(&mut oracle, &dir, point, &[], &mut census, &mut divergences);
            }
        }
    }
    census.report("matrix");
    assert_no_divergences("matrix", &divergences);

    // Anti-vacuity, in the direction that matters. A walker that declined every
    // witness would satisfy certain-implies-exact perfectly; what proves the
    // derivation is checked is that MSBuild *moves* the pair mid-walk in a
    // decent number of documents and we commit at every position for them.
    assert!(
        census.msbuild_moved_across_positions >= 20,
        "MSBuild moved the pair across positions in only {} documents — the \
         matrix has stopped exercising inference at all",
        census.msbuild_moved_across_positions
    );
    assert!(
        census.tracked_across_positions >= 20,
        "we tracked MSBuild's mid-walk movement in only {} of {} documents; the \
         position axis is being carried by declines rather than by the model",
        census.tracked_across_positions,
        census.msbuild_moved_across_positions
    );
}

/// The corpus shape, pinned to **commit** rather than decline.
///
/// `FSharp.Profiles.props` gates on `'$(TargetFrameworkIdentifier)' ==
/// '.NETFramework'` from a `Directory.Build.targets`-position file. Reading that
/// as empty makes the gate cleanly *false* rather than undecidable, so the walker
/// commits the wrong branch and publishes `#if` symbols the real build never
/// defines. A decline would be sound but would not fix the corpus, so the fix is
/// only real if this commits.
///
/// The expected value is deliberately absent: the assertion below is that we
/// commit, and [`check_point`]'s certain-implies-exact decides what to. An
/// expectation reasoned out here is exactly as untrustworthy as the code it
/// would be checking.
#[test]
fn the_corpus_shape_commits_the_derived_identifier_at_the_targets_position() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let decl = *DECLARATIONS
        .iter()
        .find(|d| d.name == "net472")
        .expect("the net472 declaration is in the matrix");
    let preset = *PRESETS
        .iter()
        .find(|p| p.name == "none")
        .expect("the empty preset is in the matrix");

    let point = Point {
        sdk: Sdk::Real,
        decl,
        preset,
    };
    let (xml, ours, theirs) = evaluate_both(&mut oracle, &dir, point, &[]);
    let theirs = theirs.expect("MSBuild evaluates a plain net472 SDK project");

    for derived in DERIVED {
        let name = witness_name(Position::Targets, derived);
        let our_value = ours.committed.get(&name).unwrap_or_else(|| {
            panic!(
                "the corpus fix requires committing ${name} at the \
                 Directory.Build.targets position, but we {}\n--- xml ---\n{xml}",
                if ours.untrusted.contains(&name) {
                    "marked it untrusted"
                } else {
                    "never reached it"
                }
            )
        });
        assert_eq!(
            our_value,
            theirs.get(&name).expect("oracle answers for every name"),
            "certain-implies-exact violated for the corpus shape, ${name}\
             \n--- xml ---\n{xml}"
        );
    }

    // …and the body position must still read it *unset*, or we have merely moved
    // the wrongness one position earlier. Same discipline: MSBuild decides the
    // value, this asserts only that the two positions disagree on their side and
    // that we reproduce whatever they are.
    let body = witness_name(Position::Body, "TargetFrameworkIdentifier");
    let targets = witness_name(Position::Targets, "TargetFrameworkIdentifier");
    assert_ne!(
        theirs.get(&body),
        theirs.get(&targets),
        "the fixture no longer distinguishes the two positions on MSBuild's own \
         side, so it cannot detect deriving too early\n--- xml ---\n{xml}"
    );
}

/// The pair as a **global property**, which is read-only to the document.
///
/// A global outranks every write of that name, so inference cannot overwrite it
/// — including a global set to the empty string, which must stay empty even
/// though the `== ''` gate it feeds is satisfied. That asymmetry (the gate
/// fires, the write is suppressed) is not something a model keyed on the value
/// alone can get right.
#[test]
fn a_global_framework_pair_outranks_the_derivation() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut census = Census::default();

    let global_sets: &[&[(&str, &str)]] = &[
        &[("TargetFrameworkIdentifier", ".NETPortable")],
        &[("TargetFrameworkIdentifier", "")],
        &[("TargetFrameworkVersion", "v9.9")],
        &[
            ("TargetFrameworkIdentifier", ".NETPortable"),
            ("TargetFrameworkVersion", "v9.9"),
        ],
    ];
    let decls: Vec<Declaration> = DECLARATIONS
        .iter()
        .filter(|d| matches!(d.name, "net472" | "net10.0" | "plural"))
        .copied()
        .collect();
    let preset = *PRESETS.iter().find(|p| p.name == "none").unwrap();
    let mut divergences = Vec::new();

    for globals in global_sets {
        let globals: Vec<(String, String)> = globals
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        for decl in &decls {
            let point = Point {
                sdk: Sdk::Real,
                decl: *decl,
                preset,
            };
            check_point(
                &mut oracle,
                &dir,
                point,
                &globals,
                &mut census,
                &mut divergences,
            );
        }
    }
    census.report("globals");
    assert_no_divergences("global-sweep", &divergences);
    assert!(
        census.committed >= 12,
        "too few committed under the global sweep ({}) — a wholesale decline \
         passes this vacuously",
        census.committed
    );
}
