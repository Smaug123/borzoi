//! Tests for [`super`].
//!
//! The load-bearing ones are the two properties in [`properties`]: *no silent
//! deferral* (anything that declines a capability produces a message) and *no
//! false alarm* (anything that declines nothing produces none). Everything else
//! checks that individual causes render usefully — that they name the thing the
//! user has to go and fix, rather than restating that something went wrong.

use super::*;
use borzoi_msbuild::{
    CompileConditionReason, CompileConditionUncertainty, CompileItemUncertaintyCause,
    CompileItemUncertaintyCauseKind, DefineConstantsUncertaintyCause, DiagnosticKind,
    DiagnosticOrigin, ImplicitImportKind, ImportFailReason, ParsedProject,
    StructuralCompileItemUncertainty,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A `ParsedProject` with every axis certain, produced by the real evaluator
/// rather than a struct literal — so it stays valid as fields are added, and so
/// the "nothing deferred" baseline is one the evaluator actually emits.
fn certain_project() -> ParsedProject {
    let parsed = borzoi_msbuild::parse_fsproj(
        r#"<Project><ItemGroup><Compile Include="A.fs" /></ItemGroup></Project>"#,
        Path::new("/w/Demo.fsproj"),
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect("the baseline project parses");
    assert!(
        deferrals(evaluated(&parsed), FoldOutcome::Unknown).is_empty(),
        "the baseline must defer nothing, or every test below is measuring the baseline"
    );
    parsed
}

/// The evaluated arm as the workspace builds it for an ordinary (inner-build)
/// project. `not_an_inner_build` gets its own tests below.
fn evaluated(parsed: &ParsedProject) -> ProjectEvaluation<'_> {
    ProjectEvaluation::Evaluated {
        parsed,
        not_an_inner_build: false,
    }
}

fn compile_cause(kind: CompileItemUncertaintyCauseKind) -> CompileItemUncertaintyCause {
    CompileItemUncertaintyCause {
        kind,
        span: 0..1,
        origin: DiagnosticOrigin::Buffer,
    }
}

fn message_for(parsed: &ParsedProject) -> Option<String> {
    deferral_message(
        Path::new("/w/Demo.fsproj"),
        &deferrals(evaluated(parsed), FoldOutcome::Unknown),
    )
}

// ---------------------------------------------------------------------------
// The properties
// ---------------------------------------------------------------------------

mod properties {
    use super::*;
    use proptest::prelude::*;

    /// The cause vocabulary, as values. One per variant of each cause enum,
    /// with payloads chosen to be recognisable in rendered text.
    ///
    /// A missing variant here costs *coverage*, not soundness: the renderers
    /// are wildcard-free, so a new `borzoi-msbuild` variant is already a
    /// compile error in `super`. `every_diagnostic_kind_has_a_sample` pins the
    /// count so the omission is at least noticed at the same time.
    fn diagnostic_kinds() -> Vec<DiagnosticKind> {
        vec![
            DiagnosticKind::UnresolvedImport {
                path: "$(Nope)/a.props".to_string(),
            },
            DiagnosticKind::ImportFailed {
                path: PathBuf::from("/w/missing.props"),
                reason: ImportFailReason::NotFound,
            },
            DiagnosticKind::ImportFailed {
                path: PathBuf::from("/w/deep.props"),
                reason: ImportFailReason::DepthLimit { depth: 64 },
            },
            DiagnosticKind::ImportFailed {
                path: PathBuf::from("/w/bad.props"),
                reason: ImportFailReason::MalformedXml {
                    message: "unexpected token".to_string(),
                },
            },
            DiagnosticKind::ImportFailed {
                path: PathBuf::from("/w/locked.props"),
                reason: ImportFailReason::Io {
                    message: "permission denied".to_string(),
                },
            },
            DiagnosticKind::UnsupportedConstruct {
                element: "UsingTask".to_string(),
            },
            DiagnosticKind::UnsupportedGlob {
                pattern: "src/**/*.fs".to_string(),
            },
            DiagnosticKind::UndefinedProperty {
                name: "TargetFramework".to_string(),
            },
            DiagnosticKind::UnsupportedPropertyExpression {
                expression: "$([System.IO.Path]::Combine(a, b))".to_string(),
            },
            DiagnosticKind::UnresolvedItemReference {
                reference: "@(Compile)".to_string(),
            },
            DiagnosticKind::UnresolvedMetadataReference {
                reference: "%(Identity)".to_string(),
            },
            DiagnosticKind::UnsupportedCondition {
                condition: "Exists($([MSBuild]::GetPathOfFileAbove('x')))".to_string(),
            },
            DiagnosticKind::UnsupportedItemOperation {
                operation: "Remove".to_string(),
            },
            DiagnosticKind::SdkNotFound {
                name: "Microsoft.Build.NoTargets/1.0.80".to_string(),
            },
            DiagnosticKind::SdkVersionNotSatisfied {
                name: "Microsoft.NET.Sdk".to_string(),
                spec: borzoi_msbuild::VersionSpec::with_version(
                    borzoi_msbuild::SdkVersion::parse("10.0.100").expect("a parseable version"),
                    borzoi_msbuild::RollForward::Disable,
                    false,
                ),
                available: vec![
                    borzoi_msbuild::SdkVersion::parse("9.0.100").expect("a parseable version"),
                ],
            },
            DiagnosticKind::SdkResolutionUnsupported {
                name: "Microsoft.NET.Sdk.WorkloadAutoImportPropsLocator".to_string(),
                reason: "workload set installed".to_string(),
            },
            DiagnosticKind::ImplicitImportPresent {
                path: PathBuf::from("/w/Directory.Packages.props"),
                kind: ImplicitImportKind::DirectoryPackagesProps,
            },
        ]
    }

    fn structural_causes() -> Vec<StructuralCompileItemUncertainty> {
        vec![
            StructuralCompileItemUncertainty::ProjectSdkUnsupported {
                sdk: "Microsoft.NET.Sdk".to_string(),
            },
            StructuralCompileItemUncertainty::ExplicitSdkUnsupported {
                sdk: "My.Sdk".to_string(),
            },
            StructuralCompileItemUncertainty::SdkImportProjectUnresolved {
                sdk: "My.Sdk".to_string(),
                project: "$(Nope).props".to_string(),
            },
            StructuralCompileItemUncertainty::SdkImportProjectRejected {
                sdk: "My.Sdk".to_string(),
                project: "../escape.props".to_string(),
            },
            StructuralCompileItemUncertainty::ImportProjectUnresolved {
                project: "$(Nope)/a.props".to_string(),
            },
            StructuralCompileItemUncertainty::UnsupportedChoose,
        ]
    }

    /// Every cause the evaluator can hand us, as the flat list the strategies
    /// index into.
    fn all_compile_causes() -> Vec<CompileItemUncertaintyCauseKind> {
        diagnostic_kinds()
            .into_iter()
            .map(CompileItemUncertaintyCauseKind::Diagnostic)
            .chain(
                structural_causes()
                    .into_iter()
                    .map(CompileItemUncertaintyCauseKind::Structural),
            )
            .collect()
    }

    /// A `ParsedProject` built by mutating the certain baseline: an arbitrary
    /// combination of the three flags the LSP reads and arbitrary cause
    /// vectors drawn from the real vocabulary.
    ///
    /// Mutating a real evaluator output rather than synthesising a literal
    /// keeps every *other* field consistent, and means the generator does not
    /// silently stop covering a field the evaluator later starts setting.
    fn arb_project() -> impl Strategy<Value = ParsedProject> {
        let causes = all_compile_causes();
        let n_causes = causes.len();
        (
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            proptest::collection::vec(0..n_causes, 0..6),
            proptest::collection::vec(0..n_causes, 0..3),
            proptest::collection::vec(any::<bool>(), 0..3),
            any::<bool>(),
        )
            .prop_map(
                move |(items, defines, projrefs, compile_idx, define_idx, cond_kinds, imported)| {
                    let mut parsed = certain_project();
                    let origin = if imported {
                        DiagnosticOrigin::Imported
                    } else {
                        DiagnosticOrigin::Buffer
                    };
                    parsed.items_uncertain = items;
                    parsed.define_constants_uncertain = defines;
                    parsed.project_references_uncertain = projrefs;
                    parsed.compile_item_uncertainties = compile_idx
                        .into_iter()
                        .map(|i| CompileItemUncertaintyCause {
                            kind: causes[i].clone(),
                            span: 0..1,
                            origin: origin.clone(),
                        })
                        .collect();
                    parsed.define_constants_uncertainties = define_idx
                        .into_iter()
                        .filter_map(|i| match &causes[i] {
                            CompileItemUncertaintyCauseKind::Diagnostic(kind) => {
                                Some(DefineConstantsUncertaintyCause {
                                    kind: kind.clone(),
                                    span: 0..1,
                                    origin: origin.clone(),
                                })
                            }
                            // The define axis has no structural arm.
                            CompileItemUncertaintyCauseKind::Structural(_) => None,
                        })
                        .collect();
                    parsed.compile_condition_uncertainties = cond_kinds
                        .into_iter()
                        .map(|undefined| CompileConditionUncertainty {
                            condition: "'$(Foo)' == 'x'".to_string(),
                            reason: if undefined {
                                CompileConditionReason::UndefinedProperties(vec!["Foo".to_string()])
                            } else {
                                CompileConditionReason::Unsupported
                            },
                            span: 0..1,
                            origin: origin.clone(),
                        })
                        .collect();
                    parsed
                },
            )
    }

    proptest! {
        /// **No silent deferral.** If any capability is declined there is a
        /// message, and it is non-trivial: it names the project and says
        /// something about every declined capability. This is the property the
        /// pre-existing code violated on every real project in the census.
        #[test]
        fn a_declined_capability_always_produces_a_message(parsed in arb_project()) {
            let ds = deferrals(evaluated(&parsed), FoldOutcome::Unknown);
            let message = deferral_message(Path::new("/w/Demo.fsproj"), &ds);
            prop_assert_eq!(!ds.is_empty(), message.is_some());
            if let Some(message) = message {
                prop_assert!(message.contains("Demo.fsproj"), "{}", message);
                for d in &ds {
                    prop_assert!(
                        message.contains(d.capability().consequence()),
                        "message omits {:?}: {}",
                        d.capability(),
                        message
                    );
                    // The head cause always survives the cap; an unrecorded
                    // one states its absence rather than saying nothing.
                    match d.causes() {
                        Causes::Recorded(causes) =>
                            prop_assert!(message.contains(&causes[0]), "{}", message),
                        Causes::Unrecorded =>
                            prop_assert!(message.contains("no specific cause"), "{}", message),
                    }
                }
            }
        }

        /// **No cause list is ever empty**, whatever the evaluator recorded —
        /// so "why" is always answerable, even for the axes that record
        /// nothing.
        #[test]
        fn every_deferral_names_at_least_one_cause(parsed in arb_project()) {
            for d in deferrals(evaluated(&parsed), FoldOutcome::Unknown) {
                // Recorded means non-empty, and every phrase says something.
                if let Causes::Recorded(causes) = d.causes() {
                    prop_assert!(!causes.is_empty());
                    prop_assert!(causes.iter().all(|c| !c.trim().is_empty()));
                }
                prop_assert!(!d.causes().render().trim().is_empty());
            }
        }

        /// **The deciding predicate and the reported capability are one fact.**
        /// `semantic::build_parses` calls `evaluation_declines_project_fold`; if
        /// it could disagree with the `ProjectFold` deferral the message would go
        /// back to being decorative.
        #[test]
        fn the_fold_predicate_agrees_with_the_reported_capability(parsed in arb_project()) {
            let eval = evaluated(&parsed);
            prop_assert_eq!(
                evaluation_declines_project_fold(eval),
                deferrals(eval, FoldOutcome::Unknown)
                    .iter()
                    .any(|d| d.capability() == DeferredCapability::ProjectFold)
            );
            // …and that predicate is exactly the pair of flags the fold reads.
            prop_assert_eq!(
                evaluation_declines_project_fold(eval),
                parsed.items_uncertain || parsed.define_constants_uncertain
            );
        }

        /// **No false alarm.** A project with none of the flags set says
        /// nothing, however many causes the evaluator recorded — SDK-tolerated
        /// causes are recorded without declining anything, and toasting them
        /// would make the message noise.
        #[test]
        fn recorded_causes_without_a_raised_flag_stay_quiet(parsed in arb_project()) {
            let mut parsed = parsed;
            parsed.items_uncertain = false;
            parsed.define_constants_uncertain = false;
            parsed.project_references_uncertain = false;
            prop_assert!(deferrals(evaluated(&parsed), FoldOutcome::Unknown).is_empty());
            prop_assert!(message_for(&parsed).is_none());
        }
    }

    /// Every cause renders to something that names the construct at fault. A
    /// renderer that dropped its payload would produce a message that is
    /// grammatical, plausible and useless.
    #[test]
    fn every_cause_names_its_payload() {
        let expectations: &[(CompileItemUncertaintyCauseKind, &[&str])] = &[
            (
                CompileItemUncertaintyCauseKind::Diagnostic(DiagnosticKind::UndefinedProperty {
                    name: "TargetFramework".to_string(),
                }),
                &["TargetFramework"],
            ),
            (
                CompileItemUncertaintyCauseKind::Diagnostic(DiagnosticKind::SdkNotFound {
                    name: "Microsoft.Build.NoTargets/1.0.80".to_string(),
                }),
                &["Microsoft.Build.NoTargets/1.0.80"],
            ),
            (
                CompileItemUncertaintyCauseKind::Diagnostic(DiagnosticKind::ImportFailed {
                    path: PathBuf::from("/w/missing.props"),
                    reason: ImportFailReason::NotFound,
                }),
                &["missing.props", "no such file"],
            ),
            (
                CompileItemUncertaintyCauseKind::Structural(
                    StructuralCompileItemUncertainty::ProjectSdkUnsupported {
                        sdk: "Microsoft.NET.Sdk".to_string(),
                    },
                ),
                &["Microsoft.NET.Sdk"],
            ),
            (
                CompileItemUncertaintyCauseKind::Structural(
                    StructuralCompileItemUncertainty::UnsupportedChoose,
                ),
                &["Choose"],
            ),
        ];
        for (kind, needles) in expectations {
            let rendered = super::super::render_compile_cause(&compile_cause(kind.clone()));
            for needle in *needles {
                assert!(
                    rendered.contains(needle),
                    "{kind:?} rendered as {rendered:?}, which omits {needle:?}"
                );
            }
        }
    }

    /// Each sampled cause renders to non-empty text with no leftover format
    /// placeholder — the cheap catch for a renderer that was added but left
    /// stubbed.
    #[test]
    fn every_sampled_cause_renders_non_empty() {
        for kind in all_compile_causes() {
            let rendered = super::super::render_compile_cause(&compile_cause(kind.clone()));
            assert!(!rendered.trim().is_empty(), "{kind:?} rendered as nothing");
            assert!(!rendered.contains("{}"), "{kind:?} → {rendered:?}");
        }
    }

    /// The sample list is coverage, not soundness (the renderers are
    /// wildcard-free), but a silently-shrinking sample list is still a loss.
    /// Bump this deliberately when `DiagnosticKind` grows — the compile error
    /// in `render_diagnostic_kind` is what will send you here.
    #[test]
    fn every_diagnostic_kind_has_a_sample() {
        const DIAGNOSTIC_KIND_VARIANTS: usize = 14;
        let distinct = diagnostic_kinds()
            .iter()
            .map(std::mem::discriminant)
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert_eq!(
            distinct, DIAGNOSTIC_KIND_VARIANTS,
            "the sample list no longer covers every DiagnosticKind variant"
        );
    }
}

// ---------------------------------------------------------------------------
// Shape of the rendered message
// ---------------------------------------------------------------------------

#[test]
fn an_unevaluable_project_is_reported_rather_than_ignored() {
    let ds = deferrals(ProjectEvaluation::Failed, FoldOutcome::Unknown);
    let message = deferral_message(Path::new("/w/Demo.fsproj"), &ds).expect("failure is reported");
    assert!(message.contains("Demo.fsproj"), "{message}");
    assert!(message.contains("could not be evaluated"), "{message}");
    assert!(evaluation_declines_project_fold(ProjectEvaluation::Failed));
}

#[test]
fn a_fully_certain_project_says_nothing() {
    assert!(message_for(&certain_project()).is_none());
}

/// The census's actual top cause: a `Directory.Build.props` gate written with
/// `GetPathOfFileAbove`, which the evaluator can't reduce. Before this module
/// the project declined the fold and said nothing at all.
#[test]
fn the_census_shape_produces_an_actionable_message() {
    let mut parsed = certain_project();
    parsed.items_uncertain = true;
    parsed.compile_item_uncertainties = vec![compile_cause(
        CompileItemUncertaintyCauseKind::Diagnostic(DiagnosticKind::UnsupportedCondition {
            condition: "Exists($([MSBuild]::GetPathOfFileAbove('Directory.Build.props')))"
                .to_string(),
        }),
    )];
    let message = message_for(&parsed).expect("a declined fold is reported");
    assert!(message.contains("single-file analysis"), "{message}");
    assert!(message.contains("GetPathOfFileAbove"), "{message}");
}

#[test]
fn duplicate_causes_are_reported_once() {
    let mut parsed = certain_project();
    parsed.items_uncertain = true;
    let kind = CompileItemUncertaintyCauseKind::Diagnostic(DiagnosticKind::UndefinedProperty {
        name: "Foo".to_string(),
    });
    parsed.compile_item_uncertainties =
        vec![compile_cause(kind.clone()), compile_cause(kind.clone())];
    let ds = deferrals(evaluated(&parsed), FoldOutcome::Unknown);
    assert_eq!(ds[0].causes().recorded().len(), 1, "{:?}", ds[0].causes());
}

/// A capped list must say that it is capped. A silent truncation reads as a
/// complete list, which is the same failure mode as no message at all.
#[test]
fn the_cause_cap_states_its_residual() {
    let mut parsed = certain_project();
    parsed.items_uncertain = true;
    parsed.compile_item_uncertainties = (0..MAX_RENDERED_CAUSES + 4)
        .map(|i| {
            compile_cause(CompileItemUncertaintyCauseKind::Diagnostic(
                DiagnosticKind::UndefinedProperty {
                    name: format!("Prop{i}"),
                },
            ))
        })
        .collect();
    let message = message_for(&parsed).expect("a declined fold is reported");
    assert!(message.contains("(and 4 more)"), "{message}");
    assert!(message.contains("Prop0"), "{message}");
    assert!(
        !message.contains("Prop6"),
        "the cap should have elided the tail: {message}"
    );
}

#[test]
fn an_imported_cause_says_so() {
    let mut parsed = certain_project();
    parsed.items_uncertain = true;
    parsed.compile_item_uncertainties = vec![CompileItemUncertaintyCause {
        kind: CompileItemUncertaintyCauseKind::Diagnostic(DiagnosticKind::UndefinedProperty {
            name: "Foo".to_string(),
        }),
        span: 0..1,
        origin: DiagnosticOrigin::Imported,
    }];
    let message = message_for(&parsed).expect("a declined fold is reported");
    assert!(message.contains("in an imported file"), "{message}");
}

/// Uncertain `#if` symbols decline the fold just as an uncertain Compile set
/// does, and must explain themselves through their own cause channel.
#[test]
fn define_uncertainty_alone_declines_and_explains() {
    let mut parsed = certain_project();
    parsed.define_constants_uncertain = true;
    parsed.define_constants_uncertainties = vec![DefineConstantsUncertaintyCause {
        kind: DiagnosticKind::UndefinedProperty {
            name: "TargetFramework".to_string(),
        },
        span: 0..1,
        origin: DiagnosticOrigin::Imported,
    }];
    assert!(evaluation_declines_project_fold(evaluated(&parsed)));
    let message = message_for(&parsed).expect("a declined fold is reported");
    assert!(message.contains("#if"), "{message}");
    assert!(message.contains("TargetFramework"), "{message}");
}

/// The reference axis records no causes of its own at most of its sites. It
/// must still be reported — with a stated absence rather than a blank.
#[test]
fn a_causeless_reference_deferral_states_the_absence() {
    let mut parsed = certain_project();
    parsed.project_references_uncertain = true;
    let ds = deferrals(evaluated(&parsed), FoldOutcome::Unknown);
    assert_eq!(ds.len(), 1);
    assert_eq!(
        ds[0].capability(),
        DeferredCapability::ProjectReferenceEdges
    );
    let message = message_for(&parsed).expect("a dropped edge set is reported");
    assert!(message.contains("<ProjectReference>"), "{message}");
    assert_eq!(ds[0].causes(), &Causes::Unrecorded);
    assert!(
        message.contains("no specific cause was recorded"),
        "{message}"
    );
}

/// Where the reference axis's cause *is* structural, it is the same construct
/// the Compile axis recorded, and saying so beats stating an absence.
#[test]
fn a_structural_reference_deferral_borrows_the_compile_cause() {
    let mut parsed = certain_project();
    parsed.project_references_uncertain = true;
    parsed.compile_item_uncertainties = vec![
        compile_cause(CompileItemUncertaintyCauseKind::Diagnostic(
            DiagnosticKind::ImportFailed {
                path: PathBuf::from("/w/missing.props"),
                reason: ImportFailReason::NotFound,
            },
        )),
        // Not structural: a bare undefined property hides no import, so it is
        // not evidence about the reference list.
        compile_cause(CompileItemUncertaintyCauseKind::Diagnostic(
            DiagnosticKind::UndefinedProperty {
                name: "Irrelevant".to_string(),
            },
        )),
    ];
    let ds = deferrals(evaluated(&parsed), FoldOutcome::Unknown);
    let refs = ds
        .iter()
        .find(|d| d.capability() == DeferredCapability::ProjectReferenceEdges)
        .expect("the reference axis is reported");
    assert_eq!(refs.causes().recorded().len(), 1, "{:?}", refs.causes());
    assert!(refs.causes().recorded()[0].contains("missing.props"));
}

#[test]
fn both_capabilities_are_reported_together() {
    let mut parsed = certain_project();
    parsed.items_uncertain = true;
    parsed.project_references_uncertain = true;
    parsed.compile_item_uncertainties =
        vec![compile_cause(CompileItemUncertaintyCauseKind::Structural(
            StructuralCompileItemUncertainty::ImportProjectUnresolved {
                project: "$(Nope)/a.props".to_string(),
            },
        ))];
    let message = message_for(&parsed).expect("both are reported");
    assert!(message.contains("single-file analysis"), "{message}");
    assert!(message.contains("<ProjectReference>"), "{message}");
}

/// A Compile-only `<Choose>` is **not** evidence about the reference list: the
/// evaluator descends a `<Choose>`'s still-possible branches looking for
/// reference mutations, and deliberately leaves `project_references_uncertain`
/// alone for it. Borrowing it anyway would name an innocent construct as the
/// reason edges were dropped — a confidently wrong explanation, which is worse
/// than the stated absence it displaced.
#[test]
fn a_compile_only_choose_is_not_blamed_for_dropped_edges() {
    let mut parsed = certain_project();
    parsed.items_uncertain = true;
    // Both flags set, but by *independent* causes: the `<Choose>` explains the
    // Compile set only, and whatever raised the reference flag recorded nothing.
    parsed.project_references_uncertain = true;
    parsed.compile_item_uncertainties =
        vec![compile_cause(CompileItemUncertaintyCauseKind::Structural(
            StructuralCompileItemUncertainty::UnsupportedChoose,
        ))];
    let ds = deferrals(evaluated(&parsed), FoldOutcome::Unknown);
    let refs = ds
        .iter()
        .find(|d| d.capability() == DeferredCapability::ProjectReferenceEdges)
        .expect("the reference axis is reported");
    assert_eq!(
        refs.causes(),
        &Causes::Unrecorded,
        "a Compile-only <Choose> must not be offered as the reason"
    );
    // …while the Compile axis, which it *does* explain, still names it.
    let fold = ds
        .iter()
        .find(|d| d.capability() == DeferredCapability::ProjectFold)
        .expect("the fold is reported");
    assert!(fold.causes().recorded()[0].contains("Choose"));
}

// ---------------------------------------------------------------------------
// The two declines the first cut of this module could not see
// ---------------------------------------------------------------------------

/// `workspace::references_suppressed` drops edges for the outer dispatch build
/// of a multi-targeted project — and *nothing in the evaluation flags it*, so a
/// verdict read off `ParsedProject` alone reports nothing while the assembly env
/// loses its references. Carrying `not_an_inner_build` in the evaluation is what
/// keeps the two in step.
#[test]
fn the_outer_dispatch_build_reports_its_dropped_edges() {
    let parsed = certain_project();
    assert!(
        !parsed.project_references_uncertain,
        "the point of this case is that the evaluation flags nothing"
    );
    let eval = ProjectEvaluation::Evaluated {
        parsed: &parsed,
        not_an_inner_build: true,
    };
    assert!(eval.drops_reference_edges());
    let ds = deferrals(eval, FoldOutcome::Unknown);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(
        ds[0].capability(),
        DeferredCapability::ProjectReferenceEdges
    );
    let message =
        deferral_message(Path::new("/w/Demo.fsproj"), &ds).expect("dropped edges are reported");
    assert!(message.contains("<ProjectReference>"), "{message}");
}

/// …and the graph predicate agrees with the reported one on every combination,
/// because there is only one predicate.
#[test]
fn the_edge_verdict_and_the_reported_capability_are_one_fact() {
    let mut parsed = certain_project();
    for uncertain in [false, true] {
        for outer in [false, true] {
            parsed.project_references_uncertain = uncertain;
            let eval = ProjectEvaluation::Evaluated {
                parsed: &parsed,
                not_an_inner_build: outer,
            };
            assert_eq!(
                eval.drops_reference_edges(),
                deferrals(eval, FoldOutcome::Unknown)
                    .iter()
                    .any(|d| d.capability() == DeferredCapability::ProjectReferenceEdges),
                "uncertain={uncertain} outer={outer}"
            );
        }
    }
}

/// The fold has exits of its own, reached only after the evaluation gate passed
/// — a Compile item that isn't on disk, a parser panic, an F# 8 shape straddle.
/// Each declines exactly the capability the evaluation-caused refusal declines,
/// so each must produce the same message with its own reason.
#[test]
fn a_fold_stage_refusal_is_reported_with_its_own_reason() {
    let parsed = certain_project();
    let cases: &[(FoldRefusal, &str)] = &[
        (
            FoldRefusal::UnreadableCompileItem {
                file: PathBuf::from("/w/Gone.fs"),
            },
            "Gone.fs",
        ),
        (
            FoldRefusal::ParserPanic {
                file: PathBuf::from("/w/Boom.fs"),
            },
            "Boom.fs",
        ),
        (
            FoldRefusal::LanguageVersionShape {
                file: PathBuf::from("/w/Straddle.fs"),
            },
            "Straddle.fs",
        ),
        (
            FoldRefusal::UnexpectedParseRoot {
                file: PathBuf::from("/w/Odd.fs"),
            },
            "Odd.fs",
        ),
    ];
    for (refusal, needle) in cases {
        let ds = deferrals(evaluated(&parsed), FoldOutcome::Refused(refusal));
        assert_eq!(ds.len(), 1, "{refusal:?}: {ds:?}");
        assert_eq!(ds[0].capability(), DeferredCapability::ProjectFold);
        let message =
            deferral_message(Path::new("/w/Demo.fsproj"), &ds).expect("a fold refusal is reported");
        assert!(
            message.contains(needle),
            "{refusal:?} must name the file: {message}"
        );
        assert!(message.contains("single-file analysis"), "{message}");
    }
}

/// The evaluation-caused arm adds nothing: it is the same decline
/// `deferrals` already explains from the evaluation, and reporting it twice
/// would bury the actual cause under a restatement.
#[test]
fn the_evaluation_caused_fold_refusal_does_not_double_report() {
    let mut parsed = certain_project();
    parsed.items_uncertain = true;
    parsed.compile_item_uncertainties =
        vec![compile_cause(CompileItemUncertaintyCauseKind::Structural(
            StructuralCompileItemUncertainty::UnsupportedChoose,
        ))];
    let ds = deferrals(
        evaluated(&parsed),
        FoldOutcome::Refused(&FoldRefusal::ProjectEvaluation),
    );
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].causes().recorded().len(), 1, "{:?}", ds[0].causes());
    assert!(ds[0].causes().recorded()[0].contains("Choose"));
}

/// A fold that did not refuse says nothing extra.
#[test]
fn a_successful_fold_adds_no_deferral() {
    let parsed = certain_project();
    assert!(deferrals(evaluated(&parsed), FoldOutcome::Unknown).is_empty());
}

/// A recorded fold outcome describes the inputs it ran on. When those change,
/// the outcome is [`FoldOutcome::Unknown`] — a third state, not a synonym for
/// success — and a clean evaluation paired with it is *not* a licence to say
/// "nothing is wrong".
///
/// Getting this wrong reads badly in both directions: treating Unknown as
/// success clears a still-valid message and re-sends it the moment anything
/// folds, and treating it as failure re-reports a problem that may already be
/// fixed. [`reconcile`] is what acts on it — per capability, so that an
/// unknowable *fold* never suppresses a known reference-edge loss.
#[test]
fn an_unknown_fold_makes_no_claim_about_a_clean_project() {
    let parsed = certain_project();
    let eval = evaluated(&parsed);
    assert!(deferrals(eval, FoldOutcome::Unknown).is_empty());
    assert!(
        !fold_verdict_known(eval, FoldOutcome::Unknown),
        "an empty deferral list from an unfolded project must not clear a message"
    );
    // A fold that has actually run does speak, either way.
    assert!(fold_verdict_known(eval, FoldOutcome::Succeeded));
    assert!(fold_verdict_known(
        eval,
        FoldOutcome::Refused(&FoldRefusal::ParserPanic {
            file: PathBuf::from("/w/A.fs")
        })
    ));
}

/// …but an evaluation-level decline is knowable without folding, so it is
/// always reported. Only the *silence* is untrustworthy.
#[test]
fn an_unknown_fold_still_reports_what_the_evaluation_declines() {
    let mut parsed = certain_project();
    parsed.items_uncertain = true;
    parsed.compile_item_uncertainties =
        vec![compile_cause(CompileItemUncertaintyCauseKind::Structural(
            StructuralCompileItemUncertainty::UnsupportedChoose,
        ))];
    let eval = evaluated(&parsed);
    assert!(
        fold_verdict_known(eval, FoldOutcome::Unknown),
        "an evaluation-level decline needs no fold to be certain of"
    );
    assert!(!deferrals(eval, FoldOutcome::Unknown).is_empty());
}

/// An unknowable fold must not suppress what the evaluation *does* know. A
/// dropped `<ProjectReference>` edge set is a fact about the evaluation alone,
/// so it publishes immediately rather than waiting for some unrelated request
/// to happen to fold the project.
#[test]
fn an_unknown_fold_still_publishes_a_known_reference_loss() {
    let parsed = certain_project();
    let eval = ProjectEvaluation::Evaluated {
        parsed: &parsed,
        not_an_inner_build: true,
    };
    let fresh = deferrals(eval, FoldOutcome::Unknown);
    assert_eq!(fresh.len(), 1);
    assert_eq!(
        fresh[0].capability(),
        DeferredCapability::ProjectReferenceEdges
    );
    let reconciled = reconcile(fresh.clone(), &[], eval, FoldOutcome::Unknown);
    assert_eq!(
        reconciled.stated(),
        fresh,
        "the known loss must survive reconciliation"
    );
}

/// …and an unknowable fold carries the *previous* fold verdict forward, so a
/// still-declined project keeps saying so rather than flickering to silence and
/// re-toasting the moment anything folds.
#[test]
fn an_unknown_fold_carries_the_previous_verdict_forward() {
    let parsed = certain_project();
    let eval = evaluated(&parsed);
    let previously = deferrals(
        eval,
        FoldOutcome::Refused(&FoldRefusal::UnreadableCompileItem {
            file: PathBuf::from("/w/Gone.fs"),
        }),
    );
    assert_eq!(previously.len(), 1);

    let reconciled = reconcile(
        deferrals(eval, FoldOutcome::Unknown),
        &previously,
        eval,
        FoldOutcome::Unknown,
    );
    assert_eq!(
        reconciled.record(),
        previously,
        "the verdict is retained for comparison"
    );
    assert!(
        reconciled.stated().is_empty(),
        "…but not restated: it describes inputs that have since changed"
    );

    // A fold that *has* run overrides it in both directions.
    assert!(
        reconcile(
            deferrals(eval, FoldOutcome::Succeeded),
            &previously,
            eval,
            FoldOutcome::Succeeded
        )
        .record()
        .is_empty(),
        "a successful fold clears the carried-forward verdict"
    );
}

/// An **evaluation-level** recovery is known without folding, so it clears its
/// own record even while the fold verdict stays unknown. Otherwise
/// reintroducing the same project problem would be deduped away as "already
/// reported".
#[test]
fn an_evaluation_recovery_clears_its_record_under_an_unknown_fold() {
    let mut broken = certain_project();
    broken.items_uncertain = true;
    broken.compile_item_uncertainties =
        vec![compile_cause(CompileItemUncertaintyCauseKind::Structural(
            StructuralCompileItemUncertainty::UnsupportedChoose,
        ))];
    let previously = deferrals(evaluated(&broken), FoldOutcome::Unknown);
    assert_eq!(previously.len(), 1);

    // The project is fixed; nothing has folded the fixed version yet.
    let fixed = certain_project();
    let reconciled = reconcile(
        deferrals(evaluated(&fixed), FoldOutcome::Unknown),
        &previously,
        evaluated(&fixed),
        FoldOutcome::Unknown,
    );
    assert!(
        reconciled.record().is_empty(),
        "an evaluation-level recovery needs no fold to be certain of: {reconciled:?}"
    );
}
