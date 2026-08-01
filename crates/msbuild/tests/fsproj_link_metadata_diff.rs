//! Differential test: the `Link` metadatum on `Compile` items, against the
//! real evaluator.
//!
//! ## The rules being diffed
//!
//! `Link` has more writers than an item element's own metadata, and the ones
//! that bite are the ones that are not where you are looking. Three, all probed
//! against dotnet 10.0.301:
//!
//! 1. the SDK's cone-gated synthesis, below;
//! 2. a metadata-only `<Compile Update="…">`, which reaches the item **whatever
//!    the cone** and **whatever the document order**, and overwrites a
//!    *declared* `Link` as readily as an absent one;
//! 3. an `<ItemDefinitionGroup><Compile><Link>` default, which reaches every
//!    item that declares none — again regardless of order — and loses to a
//!    declared `Link`.
//!
//! Only the first was in this file's original axes. A review round pointed at
//! the other two; adding a `Writer` axis turned up **228** wrong commits where
//! the pre-existing axes had shown none, which is the argument for widening the
//! generator over fixing the named instance.
//!
//! The .NET SDK's `Microsoft.NET.Sdk.DefaultItems.targets` carries a
//! metadata-bearing group
//!
//! ```xml
//! <ItemGroup Condition="'$(SetLinkMetadataAutomatically)' != 'false'">
//!   <Compile Update="@(Compile)">
//!     <LinkBase Condition="'%(LinkBase)' != ''">$([MSBuild]::EnsureTrailingSlash(%(LinkBase)))</LinkBase>
//!     <Link Condition="'%(Link)' == '' And … And !%(FullPath).StartsWith($(MSBuildProjectDirectory)/)"
//!       >%(LinkBase)%(RecursiveDir)%(Filename)%(Extension)</Link>
//!   </Compile>
//! </ItemGroup>
//! ```
//!
//! which fills an unset `Link` in for every item whose full path escapes the
//! project directory. It runs at *evaluation* time, so `-getItem` reports it —
//! it is not the `AssignLinkMetadata` target (which is gated on
//! `SynthesizeLinkMetadata` and does not cover `Compile` at all).
//!
//! This evaluator does not execute metadata-bearing `Update` groups, so it
//! cannot state that value. What it must do instead is *decline* it, and the
//! contract below is what says so out loud.
//!
//! ## Why the harness exists
//!
//! Because the failure it guards against is silent. `ResolvedItem::link` used
//! to be a bare `Option<String>` whose documentation said an unevaluable write
//! "degrades to `None`" — so "provably no link" and "we never worked it out"
//! were the same value, and an out-of-cone item read as `""` with no decline
//! anywhere to show for it. The whole-corpus sweep caught exactly one instance
//! of that (`FSharp.Build.fsproj`, a `..\Compiler\Utilities\` include); a
//! corpus of six projects is not a reason to believe there was only one shape.
//! Sweeping the axes is.
//!
//! ## The contract
//!
//! The crate's usual one, at the field: we commit a `Link`
//! (`ItemMetadataValue::Known`) ⟹ MSBuild evaluates the same document at the
//! same path to the byte-identical value; we decline (`Unknown`) ⟹ no claim;
//! MSBuild rejects the project ⟹ we committed nothing.
//!
//! One-sided contracts need an anti-vacuity guard, and here a *threshold* on
//! the commit count is the wrong one: declining is always sound, so widening
//! the product with declining shapes silently stops the threshold binding —
//! which is exactly what adding `Writer` did, taking commits from 216 to 100
//! while the sweep got strictly better. [`Point::must_commit`] is the guard
//! instead: it names the points where a decline would be giving up on
//! something fully in view (in the cone, no second writer, an evaluable
//! declaration) and fails on each individually.
//!
//! ## Glob includes, and why the resolver is handed the answer
//!
//! `Include="…/**/*.fs"` is swept, but this harness is not the place to
//! establish *which files a wildcard matches or in what order* — that crosses
//! the `GlobResolver` seam, which is the caller's to implement (the LSP's) and
//! is diffed elsewhere. So for glob cases the resolver is handed MSBuild's own
//! expansion verbatim. That is not the subject conceding a point to itself:
//! path and order become fixed inputs on both sides, leaving `Link` as the only
//! free variable, which is precisely what this file is about.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use borzoi_msbuild::{
    GlobRequest, ItemMetadataValue, parse_fsproj_with_imports, resolve_sdk, workloads,
};
use common::Oracle;
use tempfile::TempDir;

/// Where the included file sits relative to the project directory.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// Beside the project file.
    InDir,
    /// Under the project directory.
    SubDir,
    /// A sibling directory — out of the cone.
    Outside,
    /// Two levels down a sibling directory, so a `**` glob gives a non-empty
    /// `RecursiveDir` and the synthesised link acquires a directory part.
    OutsideDeep,
    /// A sibling whose *name* extends the project directory's, so a naive
    /// string-prefix cone test (`/proj` against `/proj-extra/…`) would call it
    /// inside. MSBuild compares against the directory with a trailing slash;
    /// this is the case that tells the two apart.
    PrefixSibling,
}

impl Placement {
    fn label(self) -> &'static str {
        match self {
            Placement::InDir => "in-dir",
            Placement::SubDir => "sub-dir",
            Placement::Outside => "outside",
            Placement::OutsideDeep => "outside-deep",
            Placement::PrefixSibling => "prefix-sibling",
        }
    }

    /// The file this placement owns, relative to the *parent* of the project
    /// directory, and the include spec that reaches it from the project.
    fn file(self) -> (&'static str, &'static str) {
        match self {
            Placement::InDir => ("proj/InDir.fs", "InDir.fs"),
            Placement::SubDir => ("proj/sub/Sub.fs", "sub/Sub.fs"),
            Placement::Outside => ("outside/Out.fs", "../outside/Out.fs"),
            Placement::OutsideDeep => ("outside/deep/Deep.fs", "../outside/deep/Deep.fs"),
            Placement::PrefixSibling => ("proj-extra/Extra.fs", "../proj-extra/Extra.fs"),
        }
    }

    /// The `**` spelling that matches exactly this placement's file. Anchoring
    /// each glob on its own directory keeps one case one item, so ordering
    /// across matches never enters into it.
    fn glob(self) -> &'static str {
        match self {
            Placement::InDir => "**/InDir.fs",
            Placement::SubDir => "sub/**/Sub.fs",
            Placement::Outside => "../outside/**/Out.fs",
            Placement::OutsideDeep => "../outside/**/Deep.fs",
            Placement::PrefixSibling => "../proj-extra/**/Extra.fs",
        }
    }
}

/// What the document itself says about `Link`.
#[derive(Clone, Copy)]
enum Decl {
    /// No metadata: the SDK rule decides.
    Bare,
    /// An explicit `Link`, which the SDK rule must leave alone.
    Explicit,
    /// `LinkBase` without a trailing slash — the SDK adds one.
    LinkBase,
    /// `LinkBase` already ending in a slash, so `EnsureTrailingSlash` is a
    /// no-op and a double slash would be visible.
    LinkBaseSlash,
    /// An explicit `Link` that cannot be evaluated. Independent of the cone: a
    /// value we could not compute is never ours to state, wherever the file
    /// sits.
    Unevaluable,
}

impl Decl {
    fn label(self) -> &'static str {
        match self {
            Decl::Bare => "bare",
            Decl::Explicit => "explicit",
            Decl::LinkBase => "linkbase",
            Decl::LinkBaseSlash => "linkbase-slash",
            Decl::Unevaluable => "unevaluable",
        }
    }

    fn attributes(self) -> &'static str {
        match self {
            Decl::Bare => "",
            Decl::Explicit => r#" Link="Custom/Explicit.fs""#,
            Decl::LinkBase => r#" LinkBase="BB""#,
            Decl::LinkBaseSlash => r#" LinkBase="BB/""#,
            // `TargetFramework` is carved out — never provably unset — so the
            // substitution cannot be pinned down and the write is unevaluable.
            Decl::Unevaluable => r#" Link="$(TargetFramework)/Mod.fs""#,
        }
    }
}

/// `$(SetLinkMetadataAutomatically)`, the documented opt-out.
#[derive(Clone, Copy)]
enum Gate {
    Unset,
    False,
    True,
    /// A case-variant spelling: MSBuild's `!=` is case-insensitive, so this
    /// turns the rule off just as `false` does.
    FalseUpper,
}

impl Gate {
    fn label(self) -> &'static str {
        match self {
            Gate::Unset => "gate-unset",
            Gate::False => "gate-false",
            Gate::True => "gate-true",
            Gate::FalseUpper => "gate-FALSE",
        }
    }

    fn property(self) -> &'static str {
        match self {
            Gate::Unset => "",
            Gate::False => {
                "    <SetLinkMetadataAutomatically>false</SetLinkMetadataAutomatically>\n"
            }
            Gate::True => "    <SetLinkMetadataAutomatically>true</SetLinkMetadataAutomatically>\n",
            Gate::FalseUpper => {
                "    <SetLinkMetadataAutomatically>FALSE</SetLinkMetadataAutomatically>\n"
            }
        }
    }
}

/// Whether the project uses the real .NET SDK. Without it nothing declares the
/// synthesis rule at all, so MSBuild leaves every `Link` unset — the arm that
/// proves the sweep is reading a real rule and not just agreeing with itself.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sdk {
    Real,
    None,
}

impl Sdk {
    fn label(self) -> &'static str {
        match self {
            Sdk::Real => "sdk",
            Sdk::None => "no-sdk",
        }
    }
}

/// How the file is named in the `Include`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    Literal,
    /// A `**` wildcard, which is what makes `%(RecursiveDir)` non-empty and so
    /// gives the synthesised link a directory part.
    Glob,
}

impl Form {
    fn label(self) -> &'static str {
        match self {
            Form::Literal => "literal",
            Form::Glob => "glob",
        }
    }
}

/// A second `Link` writer in the *document*, beyond the item element's own
/// metadata.
///
/// The SDK's group is not the only thing that writes this metadatum, and the
/// others are not gated on the project cone at all — which is what makes them a
/// separate axis rather than a variation on `Decl`. All four are probed against
/// real MSBuild: an `Update` overwrites a *declared* `Link` as readily as an
/// absent one, and an `<ItemDefinitionGroup>` default loses to a declared one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Writer {
    /// No second writer: the item element's own metadata is the whole story.
    None,
    /// `<Compile Update="…" Link="…" />` — the attribute form.
    UpdateAttribute,
    /// `<Compile Update="…"><Link>…</Link></Compile>` — the child-element form,
    /// which is what the SDK itself uses.
    UpdateChild,
    /// `<Compile Update="…" Link="" />`: a writer that *clears*. Distinct from
    /// absence — it must be able to turn a declared link back off.
    UpdateClearing,
    /// `<ItemDefinitionGroup><Compile><Link>…</Link></Compile>` — a default that
    /// applies to every item regardless of document order, and loses to a
    /// declared `Link`.
    Definition,
}

impl Writer {
    fn label(self) -> &'static str {
        match self {
            Writer::None => "no-writer",
            Writer::UpdateAttribute => "update-attr",
            Writer::UpdateChild => "update-child",
            Writer::UpdateClearing => "update-clearing",
            Writer::Definition => "item-definition",
        }
    }

    /// The extra XML this writer contributes, and where it goes.
    fn item_group(self, spec: &str) -> String {
        match self {
            Writer::None => String::new(),
            Writer::UpdateAttribute => format!(
                "  <ItemGroup>\n    <Compile Update=\"{spec}\" Link=\"Updated/Attr.fs\" />\n  \
                 </ItemGroup>\n"
            ),
            Writer::UpdateChild => format!(
                "  <ItemGroup>\n    <Compile Update=\"{spec}\">\n      \
                 <Link>Updated/Child.fs</Link>\n    </Compile>\n  </ItemGroup>\n"
            ),
            Writer::UpdateClearing => {
                format!(
                    "  <ItemGroup>\n    <Compile Update=\"{spec}\" Link=\"\" />\n  </ItemGroup>\n"
                )
            }
            Writer::Definition => String::new(),
        }
    }

    fn definition_group(self) -> &'static str {
        match self {
            Writer::Definition => {
                "  <ItemDefinitionGroup>\n    <Compile>\n      \
                 <Link>FromDefinition.fs</Link>\n    </Compile>\n  </ItemDefinitionGroup>\n"
            }
            _ => "",
        }
    }
}

#[derive(Clone, Copy)]
struct Point {
    sdk: Sdk,
    placement: Placement,
    decl: Decl,
    gate: Gate,
    form: Form,
    writer: Writer,
}

impl Point {
    fn label(self) -> String {
        format!(
            "{}/{}/{}/{}/{}/{}",
            self.sdk.label(),
            self.placement.label(),
            self.decl.label(),
            self.gate.label(),
            self.form.label(),
            self.writer.label(),
        )
    }

    /// Points where a decline would be the evaluator giving up on something it
    /// can see the whole of: the item is inside the project cone (so the SDK's
    /// synthesis rule cannot reach it), no second writer exists in the
    /// document, and the declaration evaluates. This is the anti-vacuity guard
    /// with teeth — declining everything is sound, so only a per-point
    /// obligation stops the sweep from passing while measuring nothing.
    fn must_commit(self) -> bool {
        self.writer == Writer::None
            && matches!(self.placement, Placement::InDir | Placement::SubDir)
            && !matches!(self.decl, Decl::Unevaluable)
    }

    fn document(self) -> String {
        let (_, literal) = self.placement.file();
        let include = match self.form {
            Form::Literal => literal.to_string(),
            Form::Glob => self.placement.glob().to_string(),
        };
        // Backslashes are MSBuild's canonical separator in an Include and are
        // legal on POSIX, so spell them that way: it is what real `.fsproj`
        // files carry, including the corpus project that motivated this file.
        let include = include.replace('/', "\\");
        // `Update` matches on the item's *identity* under path normalisation,
        // so the literal spelling reaches the item whichever form the `Include`
        // took — a glob-expanded item's identity is the same relative path.
        let update_spec = literal.replace('/', "\\");
        let sdk_attr = match self.sdk {
            Sdk::Real => r#" Sdk="Microsoft.NET.Sdk""#,
            Sdk::None => "",
        };
        let tfm = match self.sdk {
            Sdk::Real => "    <TargetFramework>net8.0</TargetFramework>\n",
            Sdk::None => "",
        };
        format!(
            "<Project{sdk_attr}>\n  <PropertyGroup>\n{tfm}    \
             <EnableDefaultCompileItems>false</EnableDefaultCompileItems>\n{}  \
             </PropertyGroup>\n{}  <ItemGroup>\n    <Compile Include=\"{include}\"{} />\n  \
             </ItemGroup>\n{}</Project>\n",
            self.gate.property(),
            self.writer.definition_group(),
            self.decl.attributes(),
            self.writer.item_group(&update_spec),
        )
    }
}

const PLACEMENTS: [Placement; 5] = [
    Placement::InDir,
    Placement::SubDir,
    Placement::Outside,
    Placement::OutsideDeep,
    Placement::PrefixSibling,
];

const WRITERS: [Writer; 5] = [
    Writer::None,
    Writer::UpdateAttribute,
    Writer::UpdateChild,
    Writer::UpdateClearing,
    Writer::Definition,
];

/// The swept product, plus a short tail for the axes that do not need to
/// multiply against everything.
///
/// Two axes are held down in the main product rather than dropped. `Gate` is
/// swept over its on/off pair, with the two remaining spellings (`true`, and
/// the case-variant `FALSE` that proves MSBuild's `!=` is case-insensitive)
/// covered in the tail; `Decl`'s two `LinkBase` shapes likewise. Neither
/// interacts with `Writer`, which is the axis this product exists to cross —
/// crossing everything with everything costs minutes for points that differ in
/// a spelling.
fn points() -> Vec<Point> {
    let mut out = Vec::new();
    for sdk in [Sdk::Real, Sdk::None] {
        for placement in PLACEMENTS {
            for decl in [Decl::Bare, Decl::Explicit, Decl::Unevaluable] {
                for gate in [Gate::Unset, Gate::False] {
                    for form in [Form::Literal, Form::Glob] {
                        for writer in WRITERS {
                            out.push(Point {
                                sdk,
                                placement,
                                decl,
                                gate,
                                form,
                                writer,
                            });
                        }
                    }
                }
            }
        }
    }
    // The tail: the spellings held down above, against both cone sides.
    for placement in PLACEMENTS {
        for decl in [Decl::LinkBase, Decl::LinkBaseSlash] {
            for gate in [Gate::Unset, Gate::True, Gate::FalseUpper] {
                for form in [Form::Literal, Form::Glob] {
                    out.push(Point {
                        sdk: Sdk::Real,
                        placement,
                        decl,
                        gate,
                        form,
                        writer: Writer::None,
                    });
                }
            }
        }
    }
    out
}

/// Lay the fixture tree out once: a project directory, a subdirectory, two
/// sibling directories, and the prefix-sharing sibling.
fn materialise(root: &Path) -> PathBuf {
    for (rel, _) in [
        Placement::InDir.file(),
        Placement::SubDir.file(),
        Placement::Outside.file(),
        Placement::OutsideDeep.file(),
        Placement::PrefixSibling.file(),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("fixture file has a parent"))
            .expect("create fixture directory");
        std::fs::write(&path, "module Fixture\n").expect("write fixture file");
    }
    root.join("proj")
}

struct Divergence {
    label: String,
    detail: String,
}

/// One point, evaluated both ways.
fn check(
    oracle: &mut Oracle,
    project_path: &Path,
    point: Point,
    census: &mut Census,
) -> Option<Divergence> {
    let source = point.document();
    let theirs = oracle.items_meta(&source, project_path, "Compile", &["Link", "FullPath"]);

    let dotnet_root = common::dotnet_root_from_env();
    let (user_dotnet_root, overrides_present) = common::workload_env_from_process();
    let sdk_resolver = |name: &str| {
        resolve_sdk(
            &dotnet_root,
            None,
            name,
            None,
            None,
            &workloads::WorkloadEnvironment {
                user_dotnet_root: user_dotnet_root.as_deref(),
                overrides_present,
                global_json_pins_workload_set: false,
            },
        )
    };
    // Glob cases are handed MSBuild's own expansion (see the module docs): the
    // wildcard seam is not this file's subject, and fixing path and order on
    // both sides leaves `Link` alone under test. Every glob here is anchored so
    // it matches exactly one file, so there is no ordering to get wrong.
    let expansion: Vec<PathBuf> = theirs
        .as_ref()
        .map(|items| {
            items
                .iter()
                .filter_map(|(_, meta)| meta.get("FullPath").map(PathBuf::from))
                .collect()
        })
        .unwrap_or_default();
    let glob_resolver = |_: &GlobRequest<'_>| expansion.clone();

    let parsed = parse_fsproj_with_imports(
        &source,
        project_path,
        &HashMap::new(),
        &common::oracle_environment(),
        Some(&sdk_resolver as &borzoi_msbuild::SdkResolver<'_>),
        Some(&glob_resolver as &borzoi_msbuild::GlobResolver<'_>),
    )
    .expect("well-formed XML parses");

    let Some(theirs) = theirs else {
        // MSBuild rejects the project: we must not have committed a Link.
        let committed: Vec<&ItemMetadataValue> = parsed
            .items
            .iter()
            .map(|item| &item.link)
            .filter(|link| matches!(link, ItemMetadataValue::Known(Some(_))))
            .collect();
        census.rejected += 1;
        return (!committed.is_empty()).then(|| Divergence {
            label: point.label(),
            detail: format!("MSBuild rejects the project, we committed {committed:?}"),
        });
    };

    if parsed.items_uncertain {
        census.item_set_declined += 1;
        return None;
    }
    if parsed.items.len() != theirs.len() {
        return Some(Divergence {
            label: point.label(),
            detail: format!(
                "item count: ours {} msbuild {}",
                parsed.items.len(),
                theirs.len()
            ),
        });
    }

    for (ours, (identity, meta)) in parsed.items.iter().zip(theirs.iter()) {
        let their_link = meta.get("Link").map(String::as_str).unwrap_or("");
        match &ours.link {
            ItemMetadataValue::Unknown => {
                census.declined += 1;
                if their_link.is_empty() {
                    census.declined_where_msbuild_had_none += 1;
                }
                // Declining is always *sound*, which is exactly why a floor on
                // the commit count rots: widen the product with declining
                // shapes and the floor stops binding without anyone noticing.
                // So name the points that must not decline and fail on them
                // individually — a property, not a threshold.
                if point.must_commit() {
                    return Some(Divergence {
                        label: point.label(),
                        detail: format!(
                            "{identity}: declined, but an in-cone item with no \
                             second writer and an evaluable declaration has \
                             nothing to decline about (msbuild {their_link:?})"
                        ),
                    });
                }
            }
            ItemMetadataValue::Known(link) => {
                census.committed += 1;
                if !their_link.is_empty() {
                    census.committed_non_empty += 1;
                }
                // MSBuild's separator in a synthesised link follows the
                // include's; compare on a single spelling so a case authored
                // with backslashes is not a divergence about slashes.
                let ours_link = link.as_deref().unwrap_or("").replace('\\', "/");
                if ours_link != their_link.replace('\\', "/") {
                    return Some(Divergence {
                        label: point.label(),
                        detail: format!(
                            "{identity}: ours {ours_link:?} msbuild {:?}",
                            their_link.replace('\\', "/")
                        ),
                    });
                }
            }
        }
    }
    None
}

#[derive(Default)]
struct Census {
    committed: usize,
    /// Commits where MSBuild's value was non-empty — the ones that could have
    /// been wrong and were not.
    committed_non_empty: usize,
    declined: usize,
    /// Declines MSBuild did not need: the price of the lexical, total rule.
    declined_where_msbuild_had_none: usize,
    item_set_declined: usize,
    rejected: usize,
}

#[test]
#[ignore = "sweeps ~660 real-SDK evaluations through the oracle; gated in ci.yml"]
fn link_metadata_is_exact_or_declined_across_the_matrix() {
    let mut oracle = Oracle::spawn();
    let tmp = TempDir::new().unwrap();
    let project_dir = materialise(tmp.path());
    let project_path = project_dir.join("Demo.fsproj");

    let mut census = Census::default();
    let mut divergences = Vec::new();
    let points = points();
    for point in &points {
        if let Some(divergence) = check(&mut oracle, &project_path, *point, &mut census) {
            divergences.push(divergence);
        }
    }

    eprintln!(
        "link metadata: {} points; {} committed ({} against a non-empty MSBuild value), \
         {} declined ({} of them MSBuild had none for), {} item sets declined, {} rejected",
        points.len(),
        census.committed,
        census.committed_non_empty,
        census.declined,
        census.declined_where_msbuild_had_none,
        census.item_set_declined,
        census.rejected,
    );

    assert!(
        divergences.is_empty(),
        "certain-implies-exact violated at {} of {} points:\n{}",
        divergences.len(),
        points.len(),
        divergences
            .iter()
            .map(|d| format!("  {}: {}", d.label, d.detail))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    // Anti-vacuity. The load-bearing guard is `Point::must_commit`, checked
    // per point above: declining is always sound, so a *threshold* on the
    // commit count stops binding the moment the product grows a declining axis
    // — which is exactly what happened when `Writer` was added and four fifths
    // of the points became legitimate declines. The two floors below are the
    // weaker backstop for the other direction: that the sweep still reaches
    // shapes where MSBuild puts a non-empty value in and we have to match it,
    // rather than agreeing on "" everywhere.
    // `must_commit` is only a guard if the product contains such points at all
    // — an axis change that stopped generating them would silence it exactly
    // like the threshold it replaced.
    let obligations = points.iter().filter(|p| p.must_commit()).count();
    assert!(
        obligations >= 50,
        "only {obligations} points carry a commit obligation — the product no \
         longer generates the in-cone, writer-free shapes that make \
         `must_commit` bind"
    );
    assert!(
        census.committed_non_empty >= 40,
        "only {} commits against a non-empty MSBuild link — the sweep is no \
         longer reaching the explicit-`Link` shapes it exists to pin",
        census.committed_non_empty
    );
    // And the declines must be real: if nothing declines, the out-of-cone rule
    // is not being exercised and a future wrong commit there goes unseen.
    assert!(
        census.declined >= 40,
        "only {} declines — the out-of-cone shapes are not being reached",
        census.declined
    );
}
