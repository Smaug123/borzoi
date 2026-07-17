//! Deterministic sync check between our assembly-reader's custom-attribute
//! catalogue and the F# compiler's upstream `WellKnownILAttributes` enum
//! (`src/Compiler/AbstractIL/il.fs`).
//!
//! The [`CATALOGUE`] below records the policy:
//!
//! - The F# compiler's enum is the *authoritative* set of CLR custom
//!   attributes it considers load-bearing.
//! - For every member of that enum, we either decode the attribute (and
//!   surface it in [`Entity`] / [`Member`]) or *deliberately* skip it.
//!   Both kinds of decision are recorded below.
//! - When the F# compiler adds a new well-known attribute, that's a
//!   signal to look at it. This test fires the moment our catalogue
//!   drifts away from upstream, so we can decide consciously whether to
//!   decode it, skip it, or queue it.
//!
//! The check is purely textual: it parses the `type WellKnownILAttributes`
//! block out of `il.fs` and compares the set of `| Name = (...)` member
//! names against [`CATALOGUE`]. It does **not** evaluate the enum or
//! depend on a built F# compiler — the file on disk is enough.

use std::collections::BTreeSet;
use std::fs;

use crate::common::corpus_root;

// ----------------------------------------------------------------------------
// What we do with each well-known attribute. Keep these entries in the same
// order as the F# enum so a side-by-side diff against `il.fs` is trivial.
// ----------------------------------------------------------------------------

/// How our model treats a member of the F# compiler's `WellKnownILAttributes`
/// enum.
///
/// `Sentinel` — the enum has flag-machinery values (`None`, `NotComputed`)
/// that are not attribute names. They don't need a decoder.
///
/// `Decoded` — we surface this attribute on the model. The `phase` field
/// pins the slice that added the decoder; `surface` names the model field
/// it feeds.
///
/// `Skipped` — the F# compiler classifies this attribute but our model
/// does not (yet) project it. The `reason` is the deliberate-omission note;
/// it must be specific. "future slice" is acceptable; "TODO" is not.
#[derive(Debug)]
enum CatalogueStatus {
    Sentinel,
    Decoded {
        phase: &'static str,
        surface: &'static str,
    },
    Skipped {
        reason: &'static str,
    },
}

/// Catalogue of every name appearing in
/// `WellKnownILAttributes` (il.fs) as of the last sync. The test below
/// asserts this list matches the enum in the checked-out F# corpus.
const CATALOGUE: &[(&str, CatalogueStatus)] = &[
    ("None", CatalogueStatus::Sentinel),
    (
        "IsReadOnlyAttribute",
        CatalogueStatus::Decoded {
            phase: "4d",
            surface: "Entity::is_readonly",
        },
    ),
    (
        "IsUnmanagedAttribute",
        CatalogueStatus::Decoded {
            phase: "4l",
            surface: "TypeParameter::is_unmanaged \
                      (presence-only; emitted alongside the `struct` constraint bit, \
                       so the normaliser renders `unmanaged` as an additive token in \
                       the constraint set rather than replacing `struct`)",
        },
    ),
    (
        "IsByRefLikeAttribute",
        CatalogueStatus::Decoded {
            phase: "4d",
            surface: "Entity::is_byref_like",
        },
    ),
    (
        "ExtensionAttribute",
        CatalogueStatus::Decoded {
            phase: "4b",
            surface: "Member::is_extension (and Entity::is_extension via type-level mark)",
        },
    ),
    (
        "NullableAttribute",
        CatalogueStatus::Decoded {
            phase: "4m.3",
            surface: "TypeParameter::nullability (typar), \
                      Parameter::nullability + Parameter::ty inner NullableType nodes, \
                      Field::nullability + Field::ty inner NullableType nodes, \
                      Property::nullability + Property::ty inner NullableType nodes, \
                      Event::nullability + Event::delegate_type inner NullableType nodes, \
                      MethodSignature::return_nullability + MethodSignature::return_type inner \
                      NullableType nodes; both scalar and byte[] composite forms decoded via \
                      pre-order DFS mirroring F# compiler \
                      `Nullness.ImportILTypeWithNullness`.",
        },
    ),
    (
        "ParamArrayAttribute",
        CatalogueStatus::Decoded {
            phase: "4c",
            surface: "Parameter::is_param_array",
        },
    ),
    (
        "AllowNullLiteralAttribute",
        CatalogueStatus::Decoded {
            phase: "4k",
            surface: "Entity::is_allow_null_literal \
                      (decodes the bool ctor arg: `[<AllowNullLiteral>]` and \
                       `[<AllowNullLiteral(true)>]` set the flag, \
                       `[<AllowNullLiteral(false)>]` — the deliberate disable \
                       shape that opts a derived class out of an inherited \
                       `(true)` — clears it)",
        },
    ),
    (
        "ReflectedDefinitionAttribute",
        CatalogueStatus::Skipped {
            reason: "Future slice: quotation-of-decl marker. Affects F# Expr \
                     reflection only; no model field today.",
        },
    ),
    (
        "AutoOpenAttribute",
        CatalogueStatus::Decoded {
            phase: "4i",
            surface: "Entity::is_auto_open for the TypeDef-level marker (\"module is \
                      implicitly opened by its parent namespace\"); \
                      EcmaView::assembly_auto_opens for the assembly-level \
                      `[<assembly: AutoOpen(path)>]` overload (the manifest's \
                      implicit-open path list, FCS's GetAutoOpenAttributes)",
        },
    ),
    (
        "InternalsVisibleToAttribute",
        CatalogueStatus::Skipped {
            reason: "Future slice: assembly-level attribute (not entity/member). \
                     The assembly reader does not yet project assembly-level \
                     attributes.",
        },
    ),
    (
        "CallerMemberNameAttribute",
        CatalogueStatus::Skipped {
            reason: "Future slice: parameter-level marker for caller-info injection. \
                     Same parameter surface as ParamArray; not yet decoded because \
                     no model consumer needs caller-info today.",
        },
    ),
    (
        "CallerFilePathAttribute",
        CatalogueStatus::Skipped {
            reason: "Future slice: caller-info sibling of CallerMemberName; see above.",
        },
    ),
    (
        "CallerLineNumberAttribute",
        CatalogueStatus::Skipped {
            reason: "Future slice: caller-info sibling of CallerMemberName; see above.",
        },
    ),
    (
        "IDispatchConstantAttribute",
        CatalogueStatus::Skipped {
            reason: "COM-interop marker on `[<Optional>]` parameters of IDispatch type. \
                     No COM-interop surface in our model.",
        },
    ),
    (
        "IUnknownConstantAttribute",
        CatalogueStatus::Skipped {
            reason: "COM-interop sibling of IDispatchConstant; see above.",
        },
    ),
    (
        "RequiresLocationAttribute",
        CatalogueStatus::Skipped {
            reason: "Future slice: C# 12 `ref readonly` parameter marker \
                     (System.Runtime.CompilerServices). Parameter-level surface; \
                     not yet decoded because the parameter model is still minimal.",
        },
    ),
    (
        "SetsRequiredMembersAttribute",
        CatalogueStatus::Decoded {
            phase: "4h",
            surface: "MethodLike::sets_required_members \
                      (presence-only; meaningful on constructors; \
                       FCS accepts the attribute under both \
                       System.Diagnostics.CodeAnalysis and \
                       System.Runtime.CompilerServices, so both are \
                       matched here too)",
        },
    ),
    (
        "NoEagerConstraintApplicationAttribute",
        CatalogueStatus::Skipped {
            reason: "F#-internal SRTP inference-order knob. Lives in FSharp.Core; \
                     affects FCS type checking, not any surface we project.",
        },
    ),
    (
        "DefaultMemberAttribute",
        CatalogueStatus::Decoded {
            phase: "4n",
            surface: "Entity::default_member (Option<DefaultMember>; positional `string` \
                      ctor arg → DefaultMember::Named, usually \"Item\" from C# indexers). \
                      Refuses loud on named args and on a null ctor arg. \
                      The model carries a DefaultMember::Unknown slot for a future \
                      relaxation of those refusals, but the importer doesn't produce it yet.",
        },
    ),
    (
        "ObsoleteAttribute",
        CatalogueStatus::Decoded {
            phase: "4e",
            surface: "Entity::obsolete, MethodLike::obsolete \
                      (the message/error payload is decoded faithfully \
                       at any string length)",
        },
    ),
    (
        "CompilerFeatureRequiredAttribute",
        CatalogueStatus::Decoded {
            phase: "4o",
            surface: "Entity::compiler_feature_required, \
                      MethodLike::compiler_feature_required, \
                      Field::compiler_feature_required, \
                      Property::compiler_feature_required \
                      (Vec<CompilerFeatureRequired> { feature, is_optional }; \
                       AllowMultiple = true so one member may carry several; \
                       the feature name is decoded faithfully at any string \
                       length). Also drives the \
                       phase-4h Obsolete suppression: feature = \"RequiredMembers\" \
                       on a constructor drops the paired synthetic [Obsolete] \
                       (Roslyn's documented contract) — see project_method.",
        },
    ),
    (
        "ExperimentalAttribute",
        CatalogueStatus::Decoded {
            phase: "4g",
            surface: "Entity::experimental, MethodLike::experimental \
                      (the diagnostic id / url-format / message payload is \
                       decoded faithfully at any string length)",
        },
    ),
    (
        "RequiredMemberAttribute",
        CatalogueStatus::Decoded {
            phase: "4h",
            surface: "Field::is_required, Property::is_required \
                      (presence-only; the type-level marker C# also emits \
                       on the containing class is intentionally ignored — \
                       redundant with the per-member flag)",
        },
    ),
    (
        "NullableContextAttribute",
        CatalogueStatus::Decoded {
            phase: "4m.1",
            surface: "TypeParameter::nullability (consulted as scope-default fallback \
                      when a typar carries no direct NullableAttribute; not yet read \
                      for non-typar positions — that lands in phase 4m.2)",
        },
    ),
    (
        "AttributeUsageAttribute",
        CatalogueStatus::Skipped {
            reason: "Future slice: meta-marker on attribute classes (legal targets, \
                     inheritance, multi-use). Needed when we start projecting whether \
                     a type *is* an attribute; not yet.",
        },
    ),
    ("NotComputed", CatalogueStatus::Sentinel),
];

// ----------------------------------------------------------------------------
// Parser. il.fs is hand-edited F# source; we read the enum block textually.
// ----------------------------------------------------------------------------

/// Extract every `| Name = ...` member of the `type WellKnownILAttributes`
/// block from il.fs source. Returns names in declaration order.
///
/// The parser intentionally stops at the *next* `type ...` declaration —
/// that's the structural marker that the enum block has ended. The block
/// header is `[<Flags>]\ntype WellKnownILAttributes =\n    | None = 0u\n...`.
fn parse_well_known_il_attributes(il_fs: &str) -> Vec<String> {
    let mut lines = il_fs.lines();
    // Find the enum header.
    let mut found = false;
    for line in lines.by_ref() {
        if line.trim_start().starts_with("type WellKnownILAttributes") {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "did not find `type WellKnownILAttributes` in il.fs — has the upstream \
         file moved or renamed the enum?"
    );

    let mut names = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        // The enum members all have the shape `| Name = (expr)`. Anything
        // else either blank, comment, or signals the end of the block.
        if let Some(rest) = trimmed.strip_prefix("| ") {
            let name = rest
                .split_whitespace()
                .next()
                .expect("non-empty `|`-prefixed line");
            names.push(name.to_string());
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        // Anything else (a `type`, `let`, `module`, ...) ends the block.
        break;
    }
    names
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

/// Sanity: every catalogue entry has a non-empty reason string (or is the
/// Sentinel/Decoded variant where the fields document themselves). Stops
/// future drift toward "TODO" entries.
#[test]
fn catalogue_entries_are_documented() {
    for (name, status) in CATALOGUE {
        match status {
            CatalogueStatus::Sentinel => {}
            CatalogueStatus::Decoded { phase, surface } => {
                assert!(
                    !phase.is_empty() && !surface.is_empty(),
                    "{name}: Decoded entry has empty phase/surface field"
                );
            }
            CatalogueStatus::Skipped { reason } => {
                assert!(
                    reason.len() >= 20,
                    "{name}: Skipped reason is too short ({} chars). Be specific: \
                     why is this not decoded, and what surface would it need?",
                    reason.len()
                );
                let lower = reason.to_ascii_lowercase();
                assert!(
                    !lower.starts_with("todo") && !lower.starts_with("fixme"),
                    "{name}: Skipped reason starts with TODO/FIXME. Replace with a \
                     specific justification or decode the attribute."
                );
            }
        }
    }
}

/// Sanity: catalogue names are unique. A typo could otherwise mask drift.
#[test]
fn catalogue_names_are_unique() {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (name, _) in CATALOGUE {
        assert!(
            seen.insert(name),
            "catalogue contains duplicate entry for {name}"
        );
    }
}

/// The load-bearing check. Compare the parsed F# enum against the catalogue
/// above and fail with an actionable message if they drift.
#[test]
fn catalogue_matches_well_known_il_attributes() {
    let corpus = corpus_root();
    let il_fs_path = corpus
        .join("src")
        .join("Compiler")
        .join("AbstractIL")
        .join("il.fs");
    assert!(
        il_fs_path.is_file(),
        "expected F# compiler source at {il_fs_path:?} — the corpus does not \
         contain `src/Compiler/AbstractIL/il.fs`. Check BORZOI_CORPUS or \
         initialise the submodule."
    );
    let source =
        fs::read_to_string(&il_fs_path).unwrap_or_else(|e| panic!("read {il_fs_path:?}: {e}"));

    let upstream = parse_well_known_il_attributes(&source);
    assert!(
        !upstream.is_empty(),
        "parsed zero members out of WellKnownILAttributes — parser is broken \
         or the upstream enum is empty"
    );

    let upstream_set: BTreeSet<&str> = upstream.iter().map(|s| s.as_str()).collect();
    let catalogue_set: BTreeSet<&str> = CATALOGUE.iter().map(|(n, _)| *n).collect();

    let new_upstream: Vec<&&str> = upstream_set.difference(&catalogue_set).collect();
    let stale_local: Vec<&&str> = catalogue_set.difference(&upstream_set).collect();

    if !new_upstream.is_empty() || !stale_local.is_empty() {
        let mut msg = String::new();
        msg.push_str("WellKnownILAttributes catalogue is out of sync with upstream.\n\n");
        if !new_upstream.is_empty() {
            msg.push_str("New members in il.fs that the catalogue does not list:\n");
            for n in &new_upstream {
                msg.push_str(&format!("  - {n}\n"));
            }
            msg.push_str(
                "\nAction: read each new name, decide whether to decode it or \
                 deliberately skip it, and add an entry to CATALOGUE in \
                 tests/all/well_known_attributes_sync.rs in the same order as il.fs.\n\n",
            );
        }
        if !stale_local.is_empty() {
            msg.push_str("Members in CATALOGUE that no longer appear in il.fs:\n");
            for n in &stale_local {
                msg.push_str(&format!("  - {n}\n"));
            }
            msg.push_str(
                "\nAction: the F# compiler has dropped these attributes from its \
                 well-known list. If we still decode them, that's fine — but the \
                 catalogue's claim that they're upstream-tracked is stale; remove \
                 the entries (or re-anchor them on a different motivation).\n",
            );
        }
        panic!("{msg}");
    }

    // Order is part of the contract: the catalogue mirrors il.fs declaration
    // order so a reviewer can read both side-by-side without re-sorting.
    let upstream_vec: Vec<&str> = upstream.iter().map(|s| s.as_str()).collect();
    let catalogue_vec: Vec<&str> = CATALOGUE.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        upstream_vec, catalogue_vec,
        "catalogue order differs from il.fs declaration order — reorder entries \
         in tests/all/well_known_attributes_sync.rs to match the upstream enum."
    );
}
