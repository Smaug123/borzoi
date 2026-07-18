//! The public `EcmaView` implementor backed by the in-crate reader's [`Image`].
//!
//! [`Ecma335Assembly`] projects the raw [`Image`] into the `Entity`-level model
//! the crate exposes. Its ground-truth oracle is the `projector_*` /
//! `assembly_diff` differential against `tools/fcs-dump` (the F# compiler
//! service).

use crate::ImportError;
use crate::fsharp_pickle::model::PickledCcu;
use crate::fsharp_resource::{
    b_stream_signature_resource_for_host, classify_fsharp_resource, decompress_deflate,
    foreign_signature_data_present, is_compressed_kind, primary_signature_resource_for_host,
};
use crate::model::{
    Access, AssemblyIdentity, AssemblyProjectionSkips, Augmentation, CompilerFeatureRequired,
    ConstantValue, DefaultMember, Entity, EntityKind, Event, Experimental, Field,
    FsharpOverlayKind, ImplementedMember, IndexParameter, InterfaceMemberImpl, Member, MethodLike,
    MethodSignature, ModuleValue, Nullability, NullableType, Obsolete, ParamDefault, Parameter,
    Primitive, Property, SkippedFsharpOverlay, SkippedMember, SkippedProjectionItem, TypeParameter,
    TypeRef, UnclassifiedMethodImpl, Variance, Version,
};
use crate::reader::{
    AccessDefect, Accessibility, AccessorOwner, AssemblyIdentity as RawAssemblyIdentity,
    AssemblyRefId, CallConv, Constant, CustomMod, DeclSemantics, DecodedAttribute, EnumWidths,
    Event as RawEvent, Field as RawField, FixedArg, GenericParam, Image, IntegralParam,
    IntegralWidth, MemberAccess, Method, MethodSig, ModifiedType, NamedKind, Param,
    Primitive as SigPrimitive, Property as RawProperty, RawAttribute, RefScope, RetType, SigError,
    TypeDef, TypeDefId, TypeName, TypeRefId, TypeScope, TypeSig, Variance as RawVariance, parse,
};
use crate::view::{EcmaView, FSharpResource};
use std::collections::HashSet;

/// The F# pickle format version this reader understands
/// (`FSharpBinaryMetadataFormatRevision` = `"2.0.0.0"`, compared as
/// `Major.Minor.Build` with the revision ignored).
const EXPECTED_FSHARP_INTERFACE_DATA_VERSION: (u16, u16, u16) = (2, 0, 0);

fn skipped_host_signature_overlays(
    resource_name: String,
    error: ImportError,
) -> SkippedFsharpOverlay {
    SkippedFsharpOverlay {
        resource_name,
        overlays: vec![
            FsharpOverlayKind::SourceName,
            FsharpOverlayKind::Extension,
            FsharpOverlayKind::Measure,
            FsharpOverlayKind::AbbreviationMarkers,
            FsharpOverlayKind::UnionCases,
        ],
        reason: error.to_string(),
    }
}

/// An eagerly-projected view of an ECMA-335 assembly, backed by the in-crate
/// reader. Owns its data; no lifetime relationship to the input bytes. Query it
/// through the [`EcmaView`] trait.
///
/// `Clone` is a deep copy of the decoded image: the modifier metamorphic probe
/// (`crate::modifier_metamorphic`, test-support feature) clones a real assembly,
/// rewrites the copy's signatures, and re-projects. Nothing else needs it.
#[derive(Clone)]
pub struct Ecma335Assembly {
    image: Image,
    /// Pre-projected so [`EcmaView::identity`] can hand back a borrow.
    identity: AssemblyIdentity,
}

impl Ecma335Assembly {
    /// Parse a .NET assembly from raw bytes and project its manifest identity.
    pub fn parse(bytes: &[u8]) -> Result<Self, ImportError> {
        let image = parse(bytes).map_err(|e| ImportError::UnsupportedEcmaLayout {
            detail: format!("assembly reader: {e}"),
        })?;
        let identity = match &image.assembly {
            Some(asm) => project_identity(asm),
            None => {
                return Err(ImportError::UnsupportedEcmaLayout {
                    detail: "assembly has no Assembly manifest record".into(),
                });
            }
        };
        Ok(Self { image, identity })
    }

    /// The decoded image, mutably — the seam the modifier metamorphic probe
    /// (`crate::modifier_metamorphic`) rewrites through: it decorates every
    /// signature in a *clone* of a real assembly and re-projects it, which
    /// needs no re-encoding and so no metadata emitter.
    ///
    /// Test-support only. The projector is otherwise a pure function of the
    /// image, and nothing at runtime may perturb it.
    #[cfg(feature = "test-support")]
    pub(crate) fn image_mut(&mut self) -> &mut Image {
        &mut self.image
    }
}

impl EcmaView for Ecma335Assembly {
    fn identity(&self) -> &AssemblyIdentity {
        &self.identity
    }

    fn assembly_refs(&self) -> Vec<AssemblyIdentity> {
        self.image.references.iter().map(project_identity).collect()
    }

    fn enumerate_type_defs_with_skips(
        &self,
    ) -> Result<(Vec<Entity>, AssemblyProjectionSkips), ImportError> {
        self.enumerate_with_skips_impl()
    }

    fn assembly_auto_opens(&self) -> Result<Vec<String>, ImportError> {
        let widths = EnumWidths::new();
        let mut out = Vec::new();
        for raw in &self.image.assembly_attributes {
            // As in `fsharp_interface_data_version`: an attribute whose owner
            // the reader cannot name cannot be the one we want — skip it
            // rather than failing the enumeration on an unrelated attribute.
            let Ok(owning) = self.image.attribute_owning_type(raw) else {
                continue;
            };
            if owning.namespace != "Microsoft.FSharp.Core" || owning.name != "AutoOpenAttribute" {
                continue;
            }
            // FCS's `TryFindAutoOpenAttr` semantics: the single-string
            // constructor contributes its path; the no-arg form contributes
            // nothing; any other shape (including an undecodable blob) is
            // warn-and-skip in FCS — mirrored here as skip, since losing an
            // implicit open only reduces what resolves (sema defers on
            // unbound names; it never wrongly resolves because an open is
            // missing).
            let Ok(decoded) = self.image.decode_attribute(raw, &widths) else {
                continue;
            };
            if let [FixedArg::String(Some(s))] = decoded.fixed_args.as_slice() {
                out.push(s.clone());
            }
        }
        Ok(out)
    }

    fn fsharp_resources(&self) -> Result<Vec<FSharpResource>, ImportError> {
        let mut out = Vec::new();
        for r in &self.image.resources {
            let name = r.name.as_str();
            // Two-step gate: a resource is an F# pickle only if it begins with a
            // reserved family stem (`FSharpSignature*`/`FSharpOptimization*`) and
            // the stem is not acting as a root namespace (`FSharpSignature.…` is
            // user data, not a pickle prefix). `FSharp.Core.resources` and the
            // like fall through untouched.
            let after_family = match name.strip_prefix("FSharpSignature") {
                Some(rest) => rest,
                None => match name.strip_prefix("FSharpOptimization") {
                    Some(rest) => rest,
                    None => continue,
                },
            };
            if after_family.starts_with('.') {
                continue;
            }
            // Past the gate, an unrecognised pickle prefix is a format we have
            // not ported — fail loud (D5).
            let (kind, _suffix) = classify_fsharp_resource(name).ok_or_else(|| {
                ImportError::UnknownFSharpResource {
                    name: name.to_string(),
                }
            })?;
            // `Image` resources are already `CurrentFile`-embedded (the reader
            // refuses other implementations at parse time), so the bytes are in
            // hand.
            let payload = if is_compressed_kind(kind) {
                decompress_deflate(&r.bytes).map_err(|e| ImportError::UnsupportedEcmaLayout {
                    detail: format!("deflate of {name}: {e}"),
                })?
            } else {
                r.bytes.clone()
            };
            out.push(FSharpResource {
                name: name.to_string(),
                kind,
                payload,
            });
        }
        // Fail loud before any consumer unpickles: an assembly with F# pickle
        // resources must carry the expected `FSharpInterfaceDataVersionAttribute`
        // (and a non-F# assembly that somehow bumped a future version we'd miss
        // is also refused). A pure C# / BCL assembly has neither resources nor
        // the attribute and maps cleanly to `Ok`.
        match self.fsharp_interface_data_version()? {
            Some(EXPECTED_FSHARP_INTERFACE_DATA_VERSION) => Ok(out),
            Some((major, minor, build)) => Err(ImportError::UnsupportedPickleVersion {
                bytes: vec![major as u8, minor as u8, build as u8],
            }),
            None if out.is_empty() => Ok(out),
            None => Err(ImportError::UnsupportedPickleVersion { bytes: Vec::new() }),
        }
    }
}

impl Ecma335Assembly {
    /// The body of [`EcmaView::enumerate_type_defs_with_skips`]: enumerate the
    /// top-level type definitions *and* report every whole type the projector
    /// had to drop because its own shape (base type, interfaces,
    /// generic-parameter constraints, entity attributes) used a construct we do
    /// not model yet — a `where T : allows ref struct` type typar, say.
    ///
    /// A single undecodable type no longer sinks the whole assembly, so a
    /// modern BCL DLL that once yielded *zero* types now yields all but the
    /// handful it genuinely cannot represent. Returned
    /// [`AssemblyProjectionSkips::dropped_types`] records those
    /// (fully-qualified name + reason); *member*-level drops are recorded
    /// per-type on [`Entity::skipped_members`] instead.
    ///
    /// The order of the kept `Vec<Entity>` matches `enumerate_type_defs`; the
    /// dropped-type list is in top-level-then-nested discovery order.
    fn enumerate_with_skips_impl(
        &self,
    ) -> Result<(Vec<Entity>, AssemblyProjectionSkips), ImportError> {
        // Decode the host CCU's F# signature pickle once, up front. It drives
        // three independent enrichments: authoritative source names,
        // authoritative F#-native extension-member flags (threaded into
        // projection below), and the measure overlay (applied after). Only the
        // *host* CCU's pickle is consulted — a `--standalone` build copies
        // foreign CCUs' pickles verbatim.
        let resources = self.fsharp_resources()?;
        let decoded: Option<(String, Result<PickledCcu, ImportError>)> =
            primary_signature_resource_for_host(&resources, &self.identity.name).map(|sig| {
                let stream_b =
                    b_stream_signature_resource_for_host(&resources, &self.identity.name);
                (
                    sig.name.clone(),
                    crate::fsharp_pickle::unpickle_signature(&sig.payload, stream_b),
                )
            });

        // The host pickle is authoritative for source-name and F#-native
        // extension overlays only when it decoded *and* it describes the whole
        // image — a single-CCU assembly. When it did not decode, or the image
        // is an `fsc --standalone` build that also embeds *foreign* dependency
        // CCU pickles (copied TypeDefs the host pickle does not describe),
        // projection keeps the IL-name heuristic for the whole image — its
        // behaviour there unchanged from before this fix. The decode failure
        // is recorded below as a skipped F# overlay instead of silently
        // disappearing. When authoritative, the per-module source-name /
        // extension overlay sets the source name and extension flag instead,
        // so projection suppresses the heuristic.
        let authoritative = matches!(&decoded, Some((_, Ok(_))))
            && !foreign_signature_data_present(&resources, &self.identity.name);

        let mut out = Vec::new();
        let mut dropped_types = Vec::new();
        let mut skipped_fsharp_overlays = Vec::new();
        for &TypeDefId(i) in &self.image.top_level {
            let td = &self.image.type_defs[i as usize];
            if is_skipped_type(td) {
                continue;
            }
            // A top-level type whose own shape is undecodable is dropped and
            // recorded rather than propagated — bounding the blast radius to the
            // one type instead of the whole assembly (the reader plan's "bound
            // uncertainty"). `project_entity` records its nested-type and member
            // drops itself; only the top-level catch lives here.
            match self.project_entity(
                i as usize,
                0,
                !authoritative,
                &mut dropped_types,
                &td.name.namespace,
            ) {
                Ok(entity) => out.push(entity),
                // Corruption (a nesting cycle) stays fatal; an unsupported
                // feature is dropped and recorded.
                Err(e @ ImportError::CyclicTypeNesting { .. }) => return Err(e),
                Err(e) => dropped_types.push(SkippedProjectionItem {
                    name: qualified_type_name(&td.name),
                    reason: e.to_string(),
                }),
            }
        }

        // Curry uncertainty (OV-6.1): the IL projector stamped every method
        // `arg_group_count: Some(1)`, the C#/VB fact that a flattened parameter
        // list *is* a single argument group. That is unprovable for an **F#**
        // assembly — a curried `member x.M a b` and a tupled `member x.M(a, b)`
        // both project to two parameters — so blank every method to `None`
        // ("unknown"): the overload engine treats an unknown ≥2-parameter member
        // as possibly curried and defers, avoiding a wrong commit against a
        // curried overload (FCS's FS0816).
        //
        // The F#-ness signal is the assembly-level
        // `FSharpInterfaceDataVersionAttribute` (`fsharp_interface_data_version`),
        // *not* merely a decodable embedded signature pickle: a **marker-only** F#
        // assembly (external `.sigdata`, or resources stripped) carries the
        // attribute yet `decoded` is `None`, and its curried members must not be
        // read as C# single-group methods. `fsharp_resources()?` above already
        // validated the marker/resource pairing (an unexpected version or a
        // resource-without-marker both errored out), so this re-read cannot newly
        // fail. Refining `None` to a per-val group count from the pickle is a
        // documented follow-up. See `docs/completed/ov-6.1-curry-detection-plan.md`.
        if self.fsharp_interface_data_version()?.is_some() {
            fn blank_arg_group_counts(entities: &mut [Entity]) {
                for entity in entities {
                    for member in &mut entity.members {
                        if let Member::Method(m) = member {
                            m.arg_group_count = None;
                        }
                    }
                    blank_arg_group_counts(&mut entity.nested_types);
                }
            }
            blank_arg_group_counts(&mut out);
        }

        // An F# assembly's type abbreviations are **erased from IL** — the pickle is the
        // only place they exist. So the question is not "do I recognise a way the pickle
        // went bad?" (a blacklist — review round 13 found it missing the case where the
        // host signature resource is simply *absent*, leaving `decoded == None`, which
        // matched neither known failure) but "can I **prove** the pickle authoritative?".
        // That is exactly `authoritative`: decoded cleanly *and* describing the whole
        // image. Anything else — decode failure, foreign CCUs (`fsc --standalone`), no
        // pickle at all — means this assembly may export abbreviations the projection
        // cannot see. See [`AssemblyProjectionSkips::fsharp_abbreviations_unknowable`],
        // and §4b of `docs/assembly-module-open-plan.md` for why the whitelist shape is
        // the only one that holds.
        //
        // Gated on the assembly actually *being* F#: a C# / BCL image has no pickle and no
        // abbreviations, so its absent pickle is no uncertainty at all. FSharp.Core is
        // exempt for the same reason `apply_abbreviation_markers` exempts it: its
        // abbreviations are the primitive-alias semantics consumers hard-code, never a
        // shadow risk.
        let is_fsharp_assembly = self.fsharp_interface_data_version()?.is_some();
        let fsharp_abbreviations_unknowable =
            self.identity.name != "FSharp.Core" && is_fsharp_assembly && !authoritative;
        // The F#-native extension-member index is complete for an F# assembly only
        // when its host pickle is `authoritative` — decoded *and* describing the whole
        // image. `apply_extension_member_index` (below) runs on any decoded pickle but
        // reads **only the host CCU**, so a decoded-but-non-authoritative `--standalone`
        // image (foreign dependency CCUs present) leaves the foreign modules' extensions
        // unindexed just as a decode failure leaves everything unindexed. Unlike
        // abbreviations, FSharp.Core is NOT exempt here — its extension members are
        // ordinary pickle data — so the name-keyed gate must treat a non-authoritative
        // F# assembly's extensions as unknowable, independent of the abbreviation flag
        // above (which exempts FSharp.Core). `!authoritative` is exactly the
        // completeness predicate the source-name and declaration-order overlays gate on.
        let fsharp_extension_index_unknowable = is_fsharp_assembly && !authoritative;

        if let Some((resource_name, decoded)) = decoded {
            match decoded {
                Ok(ccu) => {
                    // F# source-name overlay and the pickle-driven module
                    // member list: set each entity's `source_name` from the
                    // host pickle, then rebuild every module's member list
                    // from its pickled vals (member source names and
                    // F#-native extension flags ride along per claimed
                    // member) — replacing the IL-name heuristics that
                    // projection skipped on the authoritative path. These
                    // borrow the CCU, so they run before the measure overlay.
                    if authoritative {
                        crate::fsharp_pickle_merge::apply_source_name_overlay(&mut out, &ccu)?;
                        crate::fsharp_pickle_merge::apply_module_member_projection(&mut out, &ccu)?;
                    }
                    // F# measure overlay (7.8b): use the pickled
                    // `TyparKind::Measure` markers to upgrade matching
                    // entities from `Class` to `Measure`. A decoded pickle
                    // that disagrees with the ECMA tree remains fatal.
                    crate::fsharp_pickle_merge::apply_measure_overlay(&mut out, &ccu)?;
                    // Declaration-order overlay: metadata row order is not
                    // source order (nested modules come out reversed), but
                    // FCS's later-wins rules — most visibly the recursive
                    // auto-open fold — are declaration-ordered, and the
                    // pickle preserves that order. Authoritative-only, like
                    // the source-name overlay: foreign copied TypeDefs are
                    // absent from the host pickle and must keep their
                    // metadata order.
                    if authoritative {
                        crate::fsharp_pickle_merge::apply_declaration_order(&mut out, &ccu)?;
                    }
                    // Abbreviation shadow markers: synthesise a name-only
                    // entity for each public metadata-invisible type
                    // abbreviation the host pickle declares, so name
                    // resolution can see the shadow. Host-CCU facts, so this
                    // runs even when the source-name overlay is
                    // non-authoritative (foreign pickles present).
                    crate::fsharp_pickle_merge::apply_abbreviation_markers(
                        &mut out,
                        &ccu,
                        &self.identity,
                    )?;
                    // F#-native instance extension-member name index (OV-0.5):
                    // record each module's extension-member source names from the
                    // pickle — the no-false-negative signal the overload
                    // extension-absence gate reads. Host-CCU facts like the
                    // abbreviation markers, so it runs even on the
                    // non-authoritative path; its cross-assembly completeness is
                    // bounded by `fsharp_abbreviations_unknowable` (set above).
                    crate::fsharp_pickle_merge::apply_extension_member_index(&mut out, &ccu)?;
                    // Union case names (module-open plan, Slice B): each
                    // union's case names from the pickle — the ECMA
                    // projection cannot see them, and the module-open fold
                    // needs them as bare-name/pattern-scope entries. Host-CCU
                    // facts like the two overlays above, so non-authoritative
                    // images still get their host unions' cases; foreign-CCU
                    // unions stay empty (= unknowable, bounded by
                    // `fsharp_abbreviations_unknowable`).
                    crate::fsharp_pickle_merge::apply_union_case_names(&mut out, &ccu)?;
                }
                Err(error) => {
                    skipped_fsharp_overlays
                        .push(skipped_host_signature_overlays(resource_name, error));
                }
            }
        }
        Ok((
            out,
            AssemblyProjectionSkips {
                dropped_types,
                skipped_fsharp_overlays,
                fsharp_abbreviations_unknowable,
                fsharp_extension_index_unknowable,
                fsharp_signature_non_authoritative: !authoritative,
            },
        ))
    }

    /// The assembly-level `FSharpInterfaceDataVersionAttribute` version triple,
    /// or `None` if the attribute is absent. The constructor signature is
    /// `(int32, int32, int32)`, so no enum widths are needed; a present-but-
    /// malformed attribute is a typed error rather than a silent "absent".
    fn fsharp_interface_data_version(&self) -> Result<Option<(u16, u16, u16)>, ImportError> {
        let widths = EnumWidths::new();
        for raw in &self.image.assembly_attributes {
            // We are searching for one specific, non-generic attribute whose
            // owning type always resolves to a plain `TypeRef`. An attribute
            // whose owner the reader cannot name (e.g. a generic attribute with
            // a `TypeSpec` parent, `MemberRefParent::Other`) therefore cannot be
            // it — treat it as a non-match rather than failing the whole
            // enumeration on an unrelated assembly-level attribute.
            let Ok(owning) = self.image.attribute_owning_type(raw) else {
                continue;
            };
            if owning.namespace != "Microsoft.FSharp.Core"
                || owning.name != "FSharpInterfaceDataVersionAttribute"
            {
                continue;
            }
            let decoded = self.image.decode_attribute(raw, &widths).map_err(|e| {
                ImportError::UnsupportedEcmaLayout {
                    detail: format!("FSharpInterfaceDataVersionAttribute decode: {e:?}"),
                }
            })?;
            return match decoded.fixed_args.as_slice() {
                [
                    FixedArg::Integral(IntegralParam::Int32(a)),
                    FixedArg::Integral(IntegralParam::Int32(b)),
                    FixedArg::Integral(IntegralParam::Int32(c)),
                ] => Ok(Some((*a as u16, *b as u16, *c as u16))),
                other => Err(ImportError::UnsupportedEcmaLayout {
                    detail: format!(
                        "FSharpInterfaceDataVersionAttribute expected three int32 args, got: {other:?}"
                    ),
                }),
            };
        }
        Ok(None)
    }
}

// ============================================================================
// Type-tree projection (stage 7.3)
// ============================================================================

/// Maximum type-nesting depth [`Ecma335Assembly::project_entity`] will follow.
/// Real assemblies nest only a handful of levels deep; a chain deeper than this
/// is corrupt (or adversarial) metadata whose `nested`/`enclosing` linkage
/// formed a cycle. A *fixed* cap (rather than the type-def count) bounds the
/// native-stack growth of the recursion as well as catching the cycle: a count-
/// based bound would still recurse thousands deep on a large assembly before
/// tripping and could overflow the stack first.
const MAX_NESTING_DEPTH: usize = 128;

/// The members projected from a single type: the ones we kept, plus the ones we
/// dropped (with why). Per the reader plan's "bound uncertainty", a member
/// whose signature the projector cannot decode is recorded here and dropped
/// *individually* rather than propagating an error that would sink the whole
/// enclosing type (and, through it, the whole assembly).
#[derive(Default)]
struct ProjectedMembers {
    kept: Vec<Member>,
    skipped: Vec<SkippedMember>,
}

impl ProjectedMembers {
    /// Record the outcome of projecting one member: keep it on `Ok`, or drop it
    /// — storing its `name` and the error's `Display` — on `Err`. The error is
    /// deliberately *not* propagated: that is what localizes the failure to this
    /// one member.
    fn push_or_skip(&mut self, name: &str, projected: Result<Member, ImportError>) {
        match projected {
            Ok(m) => self.kept.push(m),
            Err(e) => self.skipped.push(SkippedMember {
                name: name.to_string(),
                reason: e.to_string(),
            }),
        }
    }

    /// As [`Self::push_or_skip`], but the projector may also decide a member is
    /// *intentionally* absent (`Ok(None)` — an accessor, a compiler-synthesised
    /// method, an F# witness twin, a wholesale-dropped property): that is not a
    /// failure, so nothing is recorded. Only an `Err` counts as a drop.
    fn push_or_skip_opt(&mut self, name: &str, projected: Result<Option<Member>, ImportError>) {
        match projected {
            Ok(Some(m)) => self.kept.push(m),
            Ok(None) => {}
            Err(e) => self.skipped.push(SkippedMember {
                name: name.to_string(),
                reason: e.to_string(),
            }),
        }
    }
}

impl Ecma335Assembly {
    /// Project one `TypeDef` (and, recursively, its kept nested types) into an
    /// `Entity`. Stage 7.3 fills the type *shape* — namespace, name, kind,
    /// access, base type, interfaces, and nesting; members, generic parameters,
    /// the marker flags, and attribute-derived facts are added by later
    /// sub-stages and left at their defaults here.
    fn project_entity(
        &self,
        idx: usize,
        depth: usize,
        il_heuristic: bool,
        entity_skips: &mut Vec<SkippedProjectionItem>,
        // The **top-level** enclosing type's namespace, threaded down the nesting
        // recursion. A nested `TypeDef` carries an *empty* `TypeNamespace`, so a
        // dropped nested type must be labelled under its top-level's namespace (not
        // the root) for the extension gate to attribute the uncertainty correctly.
        top_namespace: &str,
    ) -> Result<Entity, ImportError> {
        // A corrupt `nested`/`enclosing` linkage can form a cycle; recursing it
        // would overflow the native stack and abort the process — the very
        // thing D5 forbids. Bound the depth and fail loud instead (gospel P5).
        if depth > MAX_NESTING_DEPTH {
            return Err(ImportError::CyclicTypeNesting {
                detail: "nested-type chain exceeded the recursion bound".to_string(),
            });
        }
        let td = &self.image.type_defs[idx];
        let base_type = match &td.extends {
            None => None,
            Some(Ok(sig)) => Some(self.project_type_ref(sig)?),
            Some(Err(e)) => return Err(sig_error(e, "base type")),
        };
        let kind = self.project_entity_kind(td, base_type.as_ref())?;
        let interfaces = td
            .implements
            .iter()
            .map(|r| match r {
                Ok(sig) => self.project_type_ref(sig),
                Err(e) => Err(sig_error(e, "implemented interface")),
            })
            .collect::<Result<Vec<_>, _>>()?;

        // The type's `[NullableContext]` default is the outermost scope for both
        // the type typars' nullability (7.6b) and the member-position
        // nullability walk (7.6a).
        let type_context = self.detect_nullable_context(&td.attributes)?;

        let generic_parameters = td
            .generic_params
            .iter()
            .map(|gp| self.project_generic_param(gp, type_context))
            .collect::<Result<Vec<_>, _>>()?;

        // F#-kinded types (Module/Union/Record/Exception) hide the
        // compiler-generated tail of structural members FCS does not surface,
        // so they take the dedicated F# member path; everything else takes the
        // straight IL projection.
        let ProjectedMembers {
            kept: members,
            skipped: skipped_members,
        } = if is_fsharp_kind(kind) {
            self.project_fsharp_members(kind, td, type_context, il_heuristic)?
        } else {
            self.project_il_members(td, type_context)?
        };

        let mut nested_types = Vec::new();
        for &TypeDefId(child) in &td.nested {
            let child_td = &self.image.type_defs[child as usize];
            if is_skipped_type(child_td) {
                continue;
            }
            // A nested type whose own shape the projector cannot decode is
            // dropped like an unreadable member — recorded on the shared
            // `entity_skips` sink (it has no enclosing `Entity` field of its
            // own) so its siblings, and the parent, stay usable. The one
            // exception is a cyclic/over-deep nesting chain: that is corruption,
            // not an unsupported feature, so it stays fatal (see
            // [`ImportError::CyclicTypeNesting`]).
            match self.project_entity(
                child as usize,
                depth + 1,
                il_heuristic,
                entity_skips,
                top_namespace,
            ) {
                Ok(entity) => {
                    if keep_nested_type(kind, &entity) {
                        nested_types.push(entity);
                    }
                }
                Err(e @ ImportError::CyclicTypeNesting { .. }) => return Err(e),
                // A nested `TypeDef` has an empty namespace, so label the drop under
                // the top-level enclosing namespace — else the extension gate would
                // attribute the uncertainty to the root, not `top_namespace` (review).
                Err(e) => entity_skips.push(SkippedProjectionItem {
                    name: nested_drop_name(top_namespace, &child_td.name),
                    reason: e.to_string(),
                }),
            }
        }

        // Entity-level attribute-derived facts (7.7a). `is_struct` is the one
        // structural marker: a direct `extends System.ValueType`, excluding the
        // well-known BCL bases (`System.Enum` extends `ValueType` but is a plain
        // `Class`, already distinguished by `EntityKind::Enum`).
        let is_struct = !is_well_known_base(td) && extends_value_type(base_type.as_ref());

        // F# entity flags (7.8a).
        let (is_no_equality, is_no_comparison, is_structural_equality, is_structural_comparison) =
            self.detect_equality_comparison(&td.attributes)?;

        // F# source name (module-suffix strip). A module sharing its name with a
        // type compiles to `<Name>Module` carrying
        // `[CompilationRepresentation(ModuleSuffix)]`; the source name is the IL
        // name minus that suffix (`ListModule` ⇒ `List`).
        //
        // On the authoritative path the host pickle is the source of truth for
        // this (an entity's `IsType::FSharpModuleWithSuffix` / `compiled_name`),
        // so we leave it `None` and let `apply_source_name_overlay` fill it —
        // mirroring how the extension-member flag is left to its overlay. The
        // IL-name attribute heuristic is the fallback when no host pickle
        // applies (`il_heuristic`).
        let name = strip_arity(&td.name.name).to_string();
        let source_name = if il_heuristic {
            self.detect_module_suffix_source_name(&td.attributes, kind, &name)?
        } else {
            None
        };

        Ok(Entity {
            assembly: self.identity.clone(),
            namespace: split_namespace(&td.name.namespace),
            name,
            kind,
            access: project_access(td.accessibility),
            is_sealed: td.is_sealed,
            base_type,
            interfaces,
            generic_parameters,
            nested_types,
            members,
            // The type's *physical* method tokens (all of them, including the
            // accessor / synthesised methods the F# member projection drops) —
            // go-to-definition needs them to reach a source-mapped method on a
            // type whose `members` hides its only navigable one. See
            // [`Entity::method_def_tokens`].
            method_def_tokens: td.methods.iter().map(|m| m.token).collect(),
            // The members the member-projection helpers above dropped (an
            // unreadable signature) rather than propagating — recorded so the
            // rest of the type stays usable.
            skipped_members,
            is_readonly: self.has_attribute(
                &td.attributes,
                "System.Runtime.CompilerServices",
                "IsReadOnlyAttribute",
            )?,
            is_byref_like: self.has_attribute(
                &td.attributes,
                "System.Runtime.CompilerServices",
                "IsByRefLikeAttribute",
            )?,
            is_struct,
            is_auto_open: self.has_attribute(
                &td.attributes,
                "Microsoft.FSharp.Core",
                "AutoOpenAttribute",
            )?,
            is_require_qualified_access: self.has_attribute(
                &td.attributes,
                "Microsoft.FSharp.Core",
                "RequireQualifiedAccessAttribute",
            )?,
            is_no_equality,
            is_no_comparison,
            is_structural_equality,
            is_structural_comparison,
            is_allow_null_literal: self.detect_allow_null_literal(&td.attributes)?,
            obsolete: self.detect_obsolete(&td.attributes)?,
            experimental: self.detect_experimental(&td.attributes)?,
            default_member: self.detect_default_member(&td.attributes)?,
            compiler_feature_required: self.detect_compiler_feature_required(&td.attributes)?,
            source_name,
            // Populated later by the F#-native extension-member index overlay
            // (`apply_extension_member_index`) from the host signature pickle;
            // the ECMA projection alone cannot see the `IsExtensionMember` bit.
            extension_member_names: Vec::new(),
            union_case_names: None,
            static_extension_member_names: Vec::new(),
            // FCS's `IsTyconRefUsedForCSharpStyleExtensionMembers`: the container
            // marker on a C# `static class` of extension methods (or an F#
            // `[<Extension>]` type). Half of the C#-style extension predicate a
            // name-resolution consumer needs; the method flag is the other half.
            is_extension_container: self.has_attribute(
                &td.attributes,
                "System.Runtime.CompilerServices",
                "ExtensionAttribute",
            )?,
            // The catch-all bag for unclassified attributes; every attribute the
            // model surfaces has a typed field, so it is left empty.
            custom_attrs: Vec::new(),
            // Only synthesised abbreviation markers carry a target (fsc emits no
            // ECMA TypeDef for a plain abbreviation, so this projector never sees
            // one); `apply_abbreviation_markers` sets it there.
            abbreviation_target: None,
        })
    }

    /// The entity kind: the ECMA discriminant (class/interface/struct/enum/
    /// delegate) overlaid with the F# kind from `CompilationMappingAttribute`
    /// (Module/Record/Union/Exception). The kind is foundational — the nested
    /// filter and the member projection both depend on it.
    fn project_entity_kind(
        &self,
        td: &TypeDef,
        base: Option<&TypeRef>,
    ) -> Result<EntityKind, ImportError> {
        let il_kind = project_kind(td, base);
        // `SourceConstructFlags` low 5 bits are the kind tag (bit 5 is a
        // visibility hint we ignore).
        match self.decode_compilation_mapping_kind(td)? {
            Some(flags) => Ok(match flags & 31 {
                1 => EntityKind::Union,
                2 => EntityKind::Record,
                5 => EntityKind::Exception,
                7 => EntityKind::Module,
                _ => il_kind,
            }),
            None => Ok(il_kind),
        }
    }

    /// The `CompilationMappingAttribute.SourceConstructFlags` value on `td`, if
    /// present. Thin wrapper over [`Self::compilation_mapping_flags`] reading the
    /// type row's own attributes.
    fn decode_compilation_mapping_kind(&self, td: &TypeDef) -> Result<Option<i32>, ImportError> {
        self.compilation_mapping_flags(&td.attributes)
    }

    /// The `CompilationMappingAttribute.SourceConstructFlags` value carried by an
    /// attribute list, if present. The first constructor argument is the
    /// `SourceConstructFlags` enum (int32-backed); a `CompilationMappingAttribute`
    /// whose first argument is not an int32 (the rarely-used `(string, Type[])`
    /// overload) is refused loudly rather than silently treated as absent. Shared
    /// by the type-kind overlay and the F# record/exception field re-projection
    /// (which keys on `SourceConstructFlags.Field`).
    fn compilation_mapping_flags(
        &self,
        attributes: &[RawAttribute],
    ) -> Result<Option<i32>, ImportError> {
        let widths = source_construct_flags_widths();
        let Some(raw) = self.find_attribute(
            attributes,
            "Microsoft.FSharp.Core",
            "CompilationMappingAttribute",
        ) else {
            return Ok(None);
        };
        let decoded = self.decode_found_attribute(raw, &widths, "CompilationMappingAttribute")?;
        match decoded.fixed_args.first() {
            Some(FixedArg::Enum {
                underlying: IntegralParam::Int32(v),
                ..
            })
            | Some(FixedArg::Integral(IntegralParam::Int32(v))) => Ok(Some(*v)),
            other => Err(ImportError::UnsupportedSignature {
                detail: format!("CompilationMappingAttribute first arg not int32: {other:?}"),
            }),
        }
    }

    /// The F# source name from `[Microsoft.FSharp.Core.CompilationSourceNameAttribute(string)]`,
    /// if present. The attribute carries the F# identifier the compiler renamed
    /// away when emitting the IL method under a `[<CompiledName>]`-supplied name
    /// (`printfn`'s method is IL `PrintFormatLine` + `CompilationSourceName("printfn")`).
    /// A single non-null string ctor arg is the only legal shape; anything else
    /// is refused loud rather than silently dropped.
    fn detect_compilation_source_name(
        &self,
        attributes: &[RawAttribute],
    ) -> Result<Option<String>, ImportError> {
        let widths = EnumWidths::new();
        let Some(raw) = self.find_attribute(
            attributes,
            "Microsoft.FSharp.Core",
            "CompilationSourceNameAttribute",
        ) else {
            return Ok(None);
        };
        let decoded =
            self.decode_found_attribute(raw, &widths, "CompilationSourceNameAttribute")?;
        match decoded.fixed_args.as_slice() {
            [FixedArg::String(Some(s))] => Ok(Some(s.clone())),
            other => Err(ImportError::UnsupportedSignature {
                detail: format!("CompilationSourceNameAttribute unexpected ctor args: {other:?}"),
            }),
        }
    }

    /// The F# module source name for a `[CompilationRepresentation(ModuleSuffix)]`
    /// module: the IL `name` with its trailing `"Module"` stripped (`ListModule`
    /// ⇒ `List`). `None` unless the entity is a [`EntityKind::Module`] whose
    /// `CompilationRepresentationFlags` includes the `ModuleSuffix` bit *and*
    /// whose name actually ends in `"Module"` — we never fabricate a name the IL
    /// does not bear out.
    fn detect_module_suffix_source_name(
        &self,
        attributes: &[RawAttribute],
        kind: EntityKind,
        name: &str,
    ) -> Result<Option<String>, ImportError> {
        if kind != EntityKind::Module {
            return Ok(None);
        }
        // `CompilationRepresentationFlags.ModuleSuffix` = 4 (the only bit we read;
        // the others — Static/Instance/UseNullAsTrueValue/Event — do not bear on
        // the source name).
        const MODULE_SUFFIX: i32 = 4;
        match self.compilation_representation_flags(attributes)? {
            Some(flags) if flags & MODULE_SUFFIX != 0 => Ok(name
                .strip_suffix("Module")
                .filter(|stripped| !stripped.is_empty())
                .map(str::to_string)),
            _ => Ok(None),
        }
    }

    /// The `CompilationRepresentationAttribute.CompilationRepresentationFlags`
    /// value carried by an attribute list, if present. Same int32-enum ctor-arg
    /// shape as [`Self::compilation_mapping_flags`]; an unexpected first arg is
    /// refused loud.
    fn compilation_representation_flags(
        &self,
        attributes: &[RawAttribute],
    ) -> Result<Option<i32>, ImportError> {
        let widths = compilation_representation_flags_widths();
        let Some(raw) = self.find_attribute(
            attributes,
            "Microsoft.FSharp.Core",
            "CompilationRepresentationAttribute",
        ) else {
            return Ok(None);
        };
        let decoded =
            self.decode_found_attribute(raw, &widths, "CompilationRepresentationAttribute")?;
        match decoded.fixed_args.first() {
            Some(FixedArg::Enum {
                underlying: IntegralParam::Int32(v),
                ..
            })
            | Some(FixedArg::Integral(IntegralParam::Int32(v))) => Ok(Some(*v)),
            other => Err(ImportError::UnsupportedSignature {
                detail: format!(
                    "CompilationRepresentationAttribute first arg not int32: {other:?}"
                ),
            }),
        }
    }

    // ------------------------------------------------------------------
    // Typed attribute-derived facts (stage 7.7 — shared by entity + member)
    // ------------------------------------------------------------------
    //
    // These decoders do **not** degrade ≥128-byte strings: the reader's
    // `decode_attribute` reads `SerString` length prefixes correctly, so the
    // full payload is decoded. A genuinely malformed blob is refused
    // loud (the reader only errors on real corruption, never spuriously). The
    // marker bools (`is_readonly`/`is_byref_like`/…) reuse [`Self::has_attribute`].

    /// `[System.ObsoleteAttribute]`, if present. The `()` / `(string?)` /
    /// `(string?, bool)` ctors plus the `Message` / `IsError` named properties
    /// collapse onto [`Obsolete`]; a bare `[<Obsolete>]` is `{ None, false }`.
    fn detect_obsolete(
        &self,
        attributes: &[RawAttribute],
    ) -> Result<Option<Obsolete>, ImportError> {
        let widths = EnumWidths::new();
        let Some(raw) = self.find_attribute(attributes, "System", "ObsoleteAttribute") else {
            return Ok(None);
        };
        let decoded = self.decode_found_attribute(raw, &widths, "ObsoleteAttribute")?;
        let mut message = None;
        let mut is_error = false;
        match decoded.fixed_args.as_slice() {
            [] => {}
            [FixedArg::String(s)] => message = s.clone(),
            [FixedArg::String(s), FixedArg::Boolean(b)] => {
                message = s.clone();
                is_error = *b;
            }
            other => {
                return Err(ImportError::UnsupportedSignature {
                    detail: format!("ObsoleteAttribute unexpected ctor args: {other:?}"),
                });
            }
        }
        for na in &decoded.named_args {
            match (na.name.as_str(), &na.value) {
                ("Message", FixedArg::String(s)) => message = s.clone(),
                ("IsError", FixedArg::Boolean(b)) => is_error = *b,
                // DiagnosticId / UrlFormat / future: don't feed the decision.
                _ => {}
            }
        }
        Ok(Some(Obsolete { message, is_error }))
    }

    /// `[System.Diagnostics.CodeAnalysis.ExperimentalAttribute]`, if present.
    /// Ctor `(string diagnosticId)` plus the `UrlFormat` / `Message` (and the
    /// `DiagnosticId` overlay) named properties.
    fn detect_experimental(
        &self,
        attributes: &[RawAttribute],
    ) -> Result<Option<Experimental>, ImportError> {
        let widths = EnumWidths::new();
        let Some(raw) = self.find_attribute(
            attributes,
            "System.Diagnostics.CodeAnalysis",
            "ExperimentalAttribute",
        ) else {
            return Ok(None);
        };
        let decoded = self.decode_found_attribute(raw, &widths, "ExperimentalAttribute")?;
        let mut diagnostic_id = match decoded.fixed_args.as_slice() {
            [FixedArg::String(s)] => s.clone(),
            other => {
                return Err(ImportError::UnsupportedSignature {
                    detail: format!("ExperimentalAttribute unexpected ctor args: {other:?}"),
                });
            }
        };
        let mut url_format = None;
        let mut message = None;
        for na in &decoded.named_args {
            match (na.name.as_str(), &na.value) {
                ("DiagnosticId", FixedArg::String(s)) => diagnostic_id = s.clone(),
                ("UrlFormat", FixedArg::String(s)) => url_format = s.clone(),
                ("Message", FixedArg::String(s)) => message = s.clone(),
                _ => {}
            }
        }
        Ok(Some(Experimental {
            diagnostic_id,
            url_format,
            message,
        }))
    }

    /// `[System.Reflection.DefaultMemberAttribute(string)]`, if present. Strict:
    /// the only supported shape is a non-null member name with no named args.
    fn detect_default_member(
        &self,
        attributes: &[RawAttribute],
    ) -> Result<Option<DefaultMember>, ImportError> {
        let widths = EnumWidths::new();
        let Some(raw) =
            self.find_attribute(attributes, "System.Reflection", "DefaultMemberAttribute")
        else {
            return Ok(None);
        };
        let decoded = self.decode_found_attribute(raw, &widths, "DefaultMemberAttribute")?;
        if !decoded.named_args.is_empty() {
            return Err(ImportError::UnsupportedSignature {
                detail: "DefaultMemberAttribute carries named args".into(),
            });
        }
        let name = match decoded.fixed_args.as_slice() {
            [FixedArg::String(Some(s))] => s.clone(),
            [FixedArg::String(None)] => {
                return Err(ImportError::UnsupportedSignature {
                    detail: "DefaultMemberAttribute has a null ctor arg".into(),
                });
            }
            other => {
                return Err(ImportError::UnsupportedSignature {
                    detail: format!("DefaultMemberAttribute unexpected ctor args: {other:?}"),
                });
            }
        };
        Ok(Some(DefaultMember::Named(name)))
    }

    /// Every `[System.Runtime.CompilerServices.CompilerFeatureRequiredAttribute]`
    /// in CA order (`AllowMultiple = true`). Refuse-loud: the payload *is* the
    /// feature name, so there is no degraded fallback — a null/extra ctor arg or
    /// an unexpected named arg is an error.
    fn detect_compiler_feature_required(
        &self,
        attributes: &[RawAttribute],
    ) -> Result<Vec<CompilerFeatureRequired>, ImportError> {
        let widths = EnumWidths::new();
        let mut out = Vec::new();
        for raw in self.find_attributes(
            attributes,
            "System.Runtime.CompilerServices",
            "CompilerFeatureRequiredAttribute",
        ) {
            let decoded =
                self.decode_found_attribute(raw, &widths, "CompilerFeatureRequiredAttribute")?;
            let feature = match decoded.fixed_args.as_slice() {
                [FixedArg::String(Some(s))] => s.clone(),
                [FixedArg::String(None)] => {
                    return Err(ImportError::UnsupportedSignature {
                        detail: "CompilerFeatureRequiredAttribute has a null ctor arg".into(),
                    });
                }
                other => {
                    return Err(ImportError::UnsupportedSignature {
                        detail: format!(
                            "CompilerFeatureRequiredAttribute unexpected ctor args: {other:?}"
                        ),
                    });
                }
            };
            let mut is_optional = false;
            for na in &decoded.named_args {
                match (na.name.as_str(), &na.value) {
                    ("IsOptional", FixedArg::Boolean(b)) => is_optional = *b,
                    _ => {
                        return Err(ImportError::UnsupportedSignature {
                            detail: format!(
                                "CompilerFeatureRequiredAttribute unexpected named arg `{}`",
                                na.name
                            ),
                        });
                    }
                }
            }
            out.push(CompilerFeatureRequired {
                feature,
                is_optional,
            });
        }
        Ok(out)
    }

    /// The F# derived-impl policy cluster, in one sweep:
    /// `(NoEquality, NoComparison, StructuralEquality, StructuralComparison)`.
    /// All four are independent parameterless `Microsoft.FSharp.Core` markers
    /// (F# enforces consistency at compile time; the importer reports the IL).
    fn detect_equality_comparison(
        &self,
        attributes: &[RawAttribute],
    ) -> Result<(bool, bool, bool, bool), ImportError> {
        let (mut no_eq, mut no_cmp, mut struct_eq, mut struct_cmp) = (false, false, false, false);
        for raw in attributes {
            let Ok(owning) = self.image.attribute_owning_type(raw) else {
                continue;
            };
            if owning.namespace != "Microsoft.FSharp.Core" {
                continue;
            }
            match owning.name.as_str() {
                "NoEqualityAttribute" => no_eq = true,
                "NoComparisonAttribute" => no_cmp = true,
                "StructuralEqualityAttribute" => struct_eq = true,
                "StructuralComparisonAttribute" => struct_cmp = true,
                _ => {}
            }
        }
        Ok((no_eq, no_cmp, struct_eq, struct_cmp))
    }

    /// `[Microsoft.FSharp.Core.AllowNullLiteralAttribute]`: whether the type
    /// opts into null-literal acceptance. The parameterless ctor (and a blob
    /// that fails to decode) means `true`; the `(bool)` overload's `(false)`
    /// form is the deliberate opt-out. Absent → `false`.
    fn detect_allow_null_literal(&self, attributes: &[RawAttribute]) -> Result<bool, ImportError> {
        let widths = EnumWidths::new();
        let Some(raw) = self.find_attribute(
            attributes,
            "Microsoft.FSharp.Core",
            "AllowNullLiteralAttribute",
        ) else {
            return Ok(false);
        };
        // A present-but-undecodable blob falls back to the no-arg meaning
        // (`true`), as for the diagnostic markers.
        let Ok(decoded) = self.image.decode_attribute(raw, &widths) else {
            return Ok(true);
        };
        match decoded.fixed_args.as_slice() {
            [] => Ok(true),
            [FixedArg::Boolean(b)] => Ok(*b),
            other => Err(ImportError::UnsupportedSignature {
                detail: format!("AllowNullLiteralAttribute unexpected ctor args: {other:?}"),
            }),
        }
    }

    // ------------------------------------------------------------------
    // Nullability walk (stage 7.6a — member-position nullability)
    // ------------------------------------------------------------------

    /// Walk one member position's `TypeSig` under the nullable precedence
    /// ladder, producing a [`NullableType`] whose outer wrapper carries the
    /// position's own nullability and whose inner `TypeRef` carries per-node
    /// nullability for every generic arg / array element:
    ///
    /// 1. A direct `[NullableAttribute(byte | byte[])]` on the position wins.
    /// 2. Otherwise the enclosing scope's `[NullableContext(byte)]` default
    ///    applies as a broadcast scalar — the walk decides per node whether a
    ///    byte is consumed, so a non-annotable outer (a non-generic value type,
    ///    `System.Nullable<T>`) still gives all-`Oblivious` while a generic
    ///    value type propagates the context byte into its reference args.
    /// 3. When the scope default is `Oblivious`/absent, no walk is needed.
    ///
    /// A `[NullableAttribute(byte[])]` whose length does not match the
    /// pre-order annotable-position count is a structural error (Roslyn emits
    /// exactly one byte per annotable position).
    fn walk_position(
        &self,
        sig: &ModifiedType,
        attrs: &[RawAttribute],
        enclosing_context: Option<Nullability>,
    ) -> Result<NullableType, ImportError> {
        let mods = self.classify_mods(&sig.mods)?;
        mods.reject_at("a nested type position")?;
        self.walk_type(&sig.ty, attrs, enclosing_context)
    }

    /// [`Self::walk_position`] for a caller that has *already* interpreted the
    /// position's modifier run — [`Self::walk_byref_position`], which must hand
    /// a `volatile` back to [`Self::project_field`] rather than refuse it here.
    fn walk_type(
        &self,
        ty: &TypeSig,
        attrs: &[RawAttribute],
        enclosing_context: Option<Nullability>,
    ) -> Result<NullableType, ImportError> {
        let src = match self.decode_nullable_byte_source(attrs)? {
            Some(s) => s,
            None => {
                let scope_default = enclosing_context.unwrap_or(Nullability::Oblivious);
                if matches!(scope_default, Nullability::Oblivious) {
                    return Ok(NullableType::oblivious(self.project_type(ty, false)?));
                }
                NullableByteSource::Scalar(scope_default)
            }
        };
        let mut idx = 0usize;
        let walked = self.walk_nullable_ty(ty, &src, &mut idx)?;
        if let NullableByteSource::Vector(v) = &src
            && idx != v.len()
        {
            return Err(ImportError::UnsupportedSignature {
                detail: format!(
                    "NullableAttribute byte[] has {} byte(s) but the pre-order walk consumed {} \
                     — length mismatch is a structural error",
                    v.len(),
                    idx
                ),
            });
        }
        Ok(walked)
    }

    /// The pre-order DFS over a `TypeSig` tree, consuming one byte from `src`
    /// per node Roslyn's encoder visits. Byte-consuming: `Object`/`String`, a
    /// reference-typed named type (outer), a generic value type (outer byte
    /// consumed and discarded), a typar, an array, and a **pointer** (outer byte
    /// consumed and discarded — a pointer is never nullable but Roslyn still
    /// emits an oblivious flag for it, then walks the pointee). Non-consuming:
    /// the value-type primitives, a non-generic value type, and
    /// `System.Nullable<T>`'s outer (its inner `T` is still walked).
    fn walk_nullable_sig(
        &self,
        sig: &ModifiedType,
        src: &NullableByteSource,
        idx: &mut usize,
    ) -> Result<NullableType, ImportError> {
        let mods = self.classify_mods(&sig.mods)?;
        mods.reject_at("a nested type position")?;
        self.walk_nullable_ty(&sig.ty, src, idx)
    }

    /// [`Self::walk_nullable_sig`] on the type proper — the position's modifier
    /// run has been classified by the caller.
    fn walk_nullable_ty(
        &self,
        sig: &TypeSig,
        src: &NullableByteSource,
        idx: &mut usize,
    ) -> Result<NullableType, ImportError> {
        Ok(match sig {
            TypeSig::Primitive(SigPrimitive::Object) => NullableType {
                ty: TypeRef::Primitive(Primitive::Object),
                nullability: consume_nullable_byte(src, idx)?,
            },
            TypeSig::Primitive(SigPrimitive::String) => NullableType {
                ty: TypeRef::Primitive(Primitive::String),
                nullability: consume_nullable_byte(src, idx)?,
            },
            TypeSig::Primitive(p) => NullableType::oblivious(TypeRef::Primitive(map_primitive(*p))),
            TypeSig::TypeVar(n) => NullableType {
                ty: TypeRef::Var {
                    index: typar_index(*n)?,
                    is_method: false,
                },
                nullability: consume_nullable_byte(src, idx)?,
            },
            TypeSig::MethodVar(n) => NullableType {
                ty: TypeRef::Var {
                    index: typar_index(*n)?,
                    is_method: true,
                },
                nullability: consume_nullable_byte(src, idx)?,
            },
            TypeSig::SzArray(inner) => {
                let outer = consume_nullable_byte(src, idx)?;
                let element = self.walk_nullable_sig(inner, src, idx)?;
                NullableType {
                    ty: TypeRef::Array {
                        element: Box::new(element),
                        rank: 1,
                        // A vector is zero-based and unsized.
                        sizes: Vec::new(),
                        lower_bounds: Vec::new(),
                    },
                    nullability: outer,
                }
            }
            TypeSig::Array {
                element,
                rank,
                sizes,
                lower_bounds,
            } => {
                let outer = consume_nullable_byte(src, idx)?;
                let element = self.walk_nullable_sig(element, src, idx)?;
                NullableType {
                    ty: TypeRef::Array {
                        element: Box::new(element),
                        rank: array_rank(*rank)?,
                        sizes: sizes.clone(),
                        lower_bounds: lower_bounds.clone(),
                    },
                    nullability: outer,
                }
            }
            TypeSig::Named { kind, scope } => self.walk_named(*kind, scope, &[], src, idx)?,
            TypeSig::Generic { kind, scope, args } => {
                self.walk_named(*kind, scope, args, src, idx)?
            }
            // A pointer is never nullable, but Roslyn's pre-order `[Nullable]`
            // flag walk still *visits* the pointer node (emitting an
            // always-oblivious `0`) and then its pointee, so a `[Nullable(byte[])]`
            // covering a pointer position carries one flag for the pointer plus
            // the pointee's own. Consume the pointer's byte (discarded — the
            // position stays oblivious) and walk the pointee to keep `idx`
            // aligned with the byte[]; keep only its `.ty`, since a pointee's
            // nullability is meaningless behind a pointer. `None` is `void*` —
            // the pointer node's byte, no pointee. Skipping the pointer node
            // here previously left the walk short by one byte per pointer,
            // spuriously refusing (and, pre-#708, sinking) every such member —
            // e.g. `T*` / `T*[]` accessors throughout `System.Private.CoreLib`.
            TypeSig::Ptr(inner) => {
                let _ = consume_nullable_byte(src, idx)?;
                let pointee = match inner {
                    Some(p) => Some(Box::new(self.walk_nullable_sig(p, src, idx)?.ty)),
                    None => None,
                };
                NullableType::oblivious(TypeRef::Ptr(pointee))
            }
            // `System.TypedReference` is a value type — Roslyn's pre-order
            // `[Nullable]` walk never annotates it, so it consumes no byte and
            // stays oblivious (like any non-generic value type).
            TypeSig::TypedByRef => NullableType::oblivious(self.typed_reference_type_ref()?),
            // A byref nested in a walked position (e.g. a byref generic arg) is
            // not valid metadata.
            TypeSig::ByRef(_) => {
                return Err(ImportError::UnsupportedSignature {
                    detail: "byref nested in a walked type position".into(),
                });
            }
        })
    }

    /// Walk a named (possibly generic) position: decide whether the outer
    /// consumes a byte from the value-vs-reference (`NamedKind`) bit and the
    /// `System.Nullable<T>` special case, then recurse the generic args.
    fn walk_named(
        &self,
        kind: Option<NamedKind>,
        scope: &TypeScope,
        args: &[ModifiedType],
        src: &NullableByteSource,
        idx: &mut usize,
    ) -> Result<NullableType, ImportError> {
        let is_value_type = matches!(kind, Some(NamedKind::ValueType));
        let outer = if is_value_type && !args.is_empty() && self.scope_is_system_nullable(scope)? {
            // `System.Nullable<T>`: outer consumes no byte; the inner `T` is
            // still walked below.
            Nullability::Oblivious
        } else if is_value_type && args.is_empty() {
            // Non-generic value type: no byte.
            Nullability::Oblivious
        } else if is_value_type {
            // Generic value type (`KeyValuePair<…>`, …): one byte consumed and
            // discarded; the reference args below still pick up their bytes.
            let _ = consume_nullable_byte(src, idx)?;
            Nullability::Oblivious
        } else {
            // Reference type (generic or not): one byte, mapped normally.
            consume_nullable_byte(src, idx)?
        };
        let type_args = args
            .iter()
            .map(|a| self.walk_nullable_sig(a, src, idx))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(NullableType {
            ty: self.named_from_scope_nullable(scope, type_args)?,
            nullability: outer,
        })
    }

    /// Whether a resolved scope names `System.Nullable` (the byte-walk's
    /// "consume no outer byte" special case).
    fn scope_is_system_nullable(&self, scope: &TypeScope) -> Result<bool, ImportError> {
        let (namespace, name) = match *scope {
            TypeScope::Definition(TypeDefId(d)) => {
                let (ns, name, _) = self.qualified_typedef_name(d as usize)?;
                (ns, name)
            }
            TypeScope::Reference(TypeRefId(r)) => {
                let (ns, name, _, _) = self.qualified_typeref_name(r as usize)?;
                (ns, name)
            }
        };
        Ok(namespace == "System" && name == "Nullable")
    }

    /// The position's direct `[NullableAttribute]` decoded to a byte source, or
    /// `None` when absent (so the caller falls through to the scope default).
    /// Refuses named args, non-byte payloads, zero-length vectors, and
    /// duplicate rows; a length-1 vector broadcasts as a scalar.
    fn decode_nullable_byte_source(
        &self,
        attrs: &[RawAttribute],
    ) -> Result<Option<NullableByteSource>, ImportError> {
        let widths = EnumWidths::new();
        let mut found: Option<NullableByteSource> = None;
        for raw in self.find_attributes(
            attrs,
            "System.Runtime.CompilerServices",
            "NullableAttribute",
        ) {
            if found.is_some() {
                return Err(ImportError::UnsupportedSignature {
                    detail: "position carries multiple NullableAttribute rows".into(),
                });
            }
            let decoded = self.decode_found_attribute(raw, &widths, "NullableAttribute")?;
            if !decoded.named_args.is_empty() {
                return Err(ImportError::UnsupportedSignature {
                    detail: "NullableAttribute carries named args — Roslyn never emits any".into(),
                });
            }
            found = Some(match decoded.fixed_args.as_slice() {
                [FixedArg::Integral(IntegralParam::UInt8(b))] => {
                    NullableByteSource::Scalar(nullability_from_byte(*b)?)
                }
                [FixedArg::Array(elems)] => {
                    let mut bytes = Vec::with_capacity(elems.len());
                    for elem in elems {
                        match elem {
                            FixedArg::Integral(IntegralParam::UInt8(b)) => {
                                bytes.push(nullability_from_byte(*b)?);
                            }
                            other => {
                                return Err(ImportError::UnsupportedSignature {
                                    detail: format!(
                                        "NullableAttribute byte[] element not a UInt8: {other:?}"
                                    ),
                                });
                            }
                        }
                    }
                    match bytes.len() {
                        0 => {
                            return Err(ImportError::UnsupportedSignature {
                                detail: "NullableAttribute carries a zero-length byte[] payload"
                                    .into(),
                            });
                        }
                        // Broadcast equivalence: `[b]` ≡ scalar `b`.
                        1 => NullableByteSource::Scalar(bytes[0]),
                        _ => NullableByteSource::Vector(bytes),
                    }
                }
                other => {
                    return Err(ImportError::UnsupportedSignature {
                        detail: format!(
                            "NullableAttribute first arg not byte or byte[]: {other:?}"
                        ),
                    });
                }
            });
        }
        Ok(found)
    }

    /// The enclosing scope's `[NullableContextAttribute(byte)]` default, or
    /// `None` when absent. Refuses duplicate rows and non-byte payloads.
    fn detect_nullable_context(
        &self,
        attrs: &[RawAttribute],
    ) -> Result<Option<Nullability>, ImportError> {
        let widths = EnumWidths::new();
        let mut found: Option<Nullability> = None;
        for raw in self.find_attributes(
            attrs,
            "System.Runtime.CompilerServices",
            "NullableContextAttribute",
        ) {
            if found.is_some() {
                return Err(ImportError::UnsupportedSignature {
                    detail: "scope carries multiple NullableContextAttribute rows".into(),
                });
            }
            let decoded = self.decode_found_attribute(raw, &widths, "NullableContextAttribute")?;
            found = Some(match decoded.fixed_args.as_slice() {
                [FixedArg::Integral(IntegralParam::UInt8(b))] => nullability_from_byte(*b)?,
                other => {
                    return Err(ImportError::UnsupportedSignature {
                        detail: format!(
                            "NullableContextAttribute first arg not a single byte: {other:?}"
                        ),
                    });
                }
            });
        }
        Ok(found)
    }

    /// Project a decoded [`TypeSig`] into the `Entity`-level [`TypeRef`]. This is
    /// the shared, all-`Oblivious` projection (base types, interfaces, and the
    /// no-walk path of member signatures); nullability bytes are layered on by
    /// [`Self::walk_position`] where the model carries them.
    ///
    /// The position's modifier run is classified per [`Self::classify_mods`]. A
    /// recognised read-only-ref marker is folded into the [`TypeRef::ByRef`] it
    /// qualifies; a `volatile` marker is a *field*-only construct and is refused
    /// here.
    fn project_type_ref(&self, mt: &ModifiedType) -> Result<TypeRef, ImportError> {
        let mods = self.classify_mods(&mt.mods)?;
        if mods.volatile {
            return Err(ImportError::UnsupportedSignature {
                detail: "`volatile` modifier outside a field type".into(),
            });
        }
        if mods.readonly_ref && !matches!(mt.ty, TypeSig::ByRef(_)) {
            return Err(ImportError::UnsupportedSignature {
                detail: "read-only-ref modifier (`modreq(InAttribute)`) not over a byref".into(),
            });
        }
        self.project_type(&mt.ty, mods.readonly_ref)
    }

    /// [`Self::project_type_ref`] for a caller that has *already* interpreted the
    /// position's modifier run (and so passes the read-only bit in). Every
    /// recursive slot below is itself a position, so it goes back through
    /// `project_type_ref`.
    fn project_type(&self, ty: &TypeSig, readonly_ref: bool) -> Result<TypeRef, ImportError> {
        Ok(match ty {
            TypeSig::Primitive(p) => TypeRef::Primitive(map_primitive(*p)),
            TypeSig::Named { scope, .. } => self.named_from_scope(scope, Vec::new())?,
            TypeSig::Generic { scope, args, .. } => {
                let type_args = args
                    .iter()
                    .map(|a| self.project_type_ref(a))
                    .collect::<Result<Vec<_>, _>>()?;
                self.named_from_scope(scope, type_args)?
            }
            TypeSig::TypeVar(n) => TypeRef::Var {
                index: typar_index(*n)?,
                is_method: false,
            },
            TypeSig::MethodVar(n) => TypeRef::Var {
                index: typar_index(*n)?,
                is_method: true,
            },
            TypeSig::SzArray(inner) => TypeRef::Array {
                element: Box::new(NullableType::oblivious(self.project_type_ref(inner)?)),
                rank: 1,
                sizes: Vec::new(),
                lower_bounds: Vec::new(),
            },
            TypeSig::Array {
                element,
                rank,
                sizes,
                lower_bounds,
            } => TypeRef::Array {
                element: Box::new(NullableType::oblivious(self.project_type_ref(element)?)),
                rank: array_rank(*rank)?,
                sizes: sizes.clone(),
                lower_bounds: lower_bounds.clone(),
            },
            TypeSig::Ptr(inner) => TypeRef::Ptr(
                inner
                    .as_ref()
                    .map(|p| self.project_type_ref(p).map(Box::new))
                    .transpose()?,
            ),
            TypeSig::ByRef(inner) => TypeRef::ByRef {
                inner: Box::new(self.project_type_ref(inner)?),
                readonly: readonly_ref,
            },
            // `ELEMENT_TYPE_TYPEDBYREF` → the `System.TypedReference` value type
            // (FCS's `ILType.Value(System.TypedReference)`).
            TypeSig::TypedByRef => self.typed_reference_type_ref()?,
        })
    }

    /// Build a `TypeRef::Named` from a resolved scope with all-`Oblivious`
    /// generic args (the no-nullability path).
    fn named_from_scope(
        &self,
        scope: &TypeScope,
        type_args: Vec<TypeRef>,
    ) -> Result<TypeRef, ImportError> {
        self.named_from_scope_nullable(
            scope,
            type_args.into_iter().map(NullableType::oblivious).collect(),
        )
    }

    /// Build a `TypeRef::Named` from a resolved scope with already-walked
    /// generic args, attributing the assembly (same-assembly → `None`; a
    /// cross-assembly `TypeRef` → its `AssemblyRef`, with a self-reference
    /// collapsed to `None`).
    fn named_from_scope_nullable(
        &self,
        scope: &TypeScope,
        type_args: Vec<NullableType>,
    ) -> Result<TypeRef, ImportError> {
        let (namespace, name, assembly, segment_arities) = match *scope {
            TypeScope::Definition(TypeDefId(d)) => {
                let (ns, name, arities) = self.qualified_typedef_name(d as usize)?;
                (ns, name, None, arities)
            }
            TypeScope::Reference(TypeRefId(r)) => self.qualified_typeref_name(r as usize)?,
        };
        // Recorded faithfully, not validated: corrupt metadata can make the
        // arities disagree with `type_args` or `name`'s `/`-segments, and the
        // fail-loud contract forbids a panic here (a deferred-corruption walk
        // must Err or project, never detonate). Consumers tolerate a mismatch.
        Ok(TypeRef::Named {
            assembly,
            namespace: split_namespace(&namespace),
            name,
            type_args,
            segment_arities,
        })
    }

    /// The `(namespace, IL-nested name)` of a same-assembly `TypeDef`, walking
    /// the enclosing chain so a nested type reads as `Outer/Inner`.
    ///
    /// A well-formed `enclosing` chain visits each `TypeDef` row at most once,
    /// so it is no longer than the table; a longer walk means corrupt metadata
    /// formed a cycle (or a self-loop). Bound the walk and fail loud rather than
    /// allocate unboundedly (gospel P5: bound uncertainty; D5: fail loud).
    fn qualified_typedef_name(
        &self,
        idx: usize,
    ) -> Result<(String, String, Vec<usize>), ImportError> {
        let mut chain = Vec::new();
        let mut arities = Vec::new();
        let mut cur = idx;
        for _ in 0..=self.image.type_defs.len() {
            let td = self.image.type_defs.get(cur).ok_or_else(|| {
                ImportError::UnsupportedEcmaLayout {
                    detail: format!("TypeDef enclosing link out of range: {cur}"),
                }
            })?;
            chain.push(strip_arity(&td.name.name).to_string());
            arities.push(arity_suffix(&td.name.name));
            match td.enclosing {
                Some(TypeDefId(p)) => cur = p as usize,
                None => {
                    chain.reverse();
                    arities.reverse();
                    return Ok((td.name.namespace.clone(), chain.join("/"), arities));
                }
            }
        }
        Err(ImportError::UnsupportedEcmaLayout {
            detail: "cyclic TypeDef enclosing chain".to_string(),
        })
    }

    /// The `(namespace, IL-nested name, owning assembly)` of a `TypeRef`, walking
    /// the `Nested` scope chain to the anchoring assembly (or module-self).
    ///
    /// A well-formed `Nested` chain visits each `TypeRef` row at most once, so
    /// it is no longer than the table; a longer walk means corrupt metadata
    /// formed a cycle. Bound the walk and fail loud rather than allocate
    /// unboundedly (gospel P5: bound uncertainty; D5: fail loud).
    fn qualified_typeref_name(
        &self,
        idx: usize,
    ) -> Result<(String, String, Option<AssemblyIdentity>, Vec<usize>), ImportError> {
        let mut chain = Vec::new();
        let mut arities = Vec::new();
        let mut cur = idx;
        for _ in 0..=self.image.type_refs.len() {
            let r = self.image.type_refs.get(cur).ok_or_else(|| {
                ImportError::UnsupportedEcmaLayout {
                    detail: format!("TypeRef nesting scope out of range: {cur}"),
                }
            })?;
            chain.push(strip_arity(&r.name.name).to_string());
            arities.push(arity_suffix(&r.name.name));
            let (namespace, assembly) = match r.scope {
                RefScope::Nested(TypeRefId(p)) => {
                    cur = p as usize;
                    continue;
                }
                RefScope::AssemblyRef(AssemblyRefId(a)) => {
                    let aref = self.image.references.get(a as usize).ok_or_else(|| {
                        ImportError::UnsupportedEcmaLayout {
                            detail: format!("TypeRef AssemblyRef scope out of range: {a}"),
                        }
                    })?;
                    let id = project_identity(aref);
                    // A degenerate self-reference is collapsed to same-assembly.
                    let assembly = (id != self.identity).then_some(id);
                    (r.name.namespace.clone(), assembly)
                }
                // Module-self alias: a type defined in this image.
                RefScope::Module => (r.name.namespace.clone(), None),
            };
            chain.reverse();
            arities.reverse();
            return Ok((namespace, chain.join("/"), assembly, arities));
        }
        Err(ImportError::UnsupportedEcmaLayout {
            detail: "cyclic TypeRef nesting chain".to_string(),
        })
    }

    /// The `System.TypedReference` value type an `ELEMENT_TYPE_TYPEDBYREF`
    /// signature element ([`TypeSig::TypedByRef`]) projects to — the same
    /// unification FCS applies when it imports the element as
    /// `ILType.Value(System.TypedReference)` (`ilread.fs:2671`), so a `typedref`
    /// in a signature and an explicit `valuetype System.TypedReference`
    /// reference share one model node.
    ///
    /// The element carries no `TypeDefOrRef` token, so the assembly is inferred:
    /// `TypedReference` lives in the runtime's core library, so it is attributed
    /// to the core-library `AssemblyRef` this image references
    /// ([`Self::core_library_ref`]). When the image *is* the core library it
    /// defines the type itself, so same-assembly (`None`) is correct. An image
    /// that carries a `typedref` yet neither references a core library nor is
    /// one cannot resolve the type at all — that is malformed metadata (a real
    /// assembly always references the corlib whose `TypedReference` it uses), so
    /// refuse loudly rather than record a misleading same-assembly `TypeDef`.
    fn typed_reference_type_ref(&self) -> Result<TypeRef, ImportError> {
        let assembly = if let Some(id) = self.core_library_ref() {
            Some(id)
        } else if CORE_LIBRARY_NAMES.contains(&self.identity.name.as_str()) {
            None
        } else {
            return Err(ImportError::UnsupportedSignature {
                detail: "System.TypedReference: no core-library reference to attribute it to"
                    .into(),
            });
        };
        Ok(TypeRef::Named {
            assembly,
            namespace: split_namespace("System"),
            name: "TypedReference".to_string(),
            type_args: Vec::new(),
            segment_arities: vec![0],
        })
    }

    /// The identity of this image's core-library `AssemblyRef` — the one that
    /// provides `System.Object`/`System.TypedReference` — located by well-known
    /// name ([`CORE_LIBRARY_NAMES`]), or `None` when the image references no core
    /// library (it either *is* corlib, or is malformed — the callers disambiguate).
    fn core_library_ref(&self) -> Option<AssemblyIdentity> {
        CORE_LIBRARY_NAMES.iter().find_map(|&name| {
            self.image
                .references
                .iter()
                .find(|r| r.name == name)
                .map(project_identity)
        })
    }

    /// Project one generic parameter (`TypeParameter`): variance, the special
    /// constraints, the typed constraints, the `unmanaged` classification, and
    /// (7.6b) the typar's own nullability. `context` is the enclosing scope's
    /// `[NullableContext]` default — the method's for a method typar, the type's
    /// for a type typar — applied when the typar carries no direct
    /// `[NullableAttribute]`.
    fn project_generic_param(
        &self,
        gp: &GenericParam,
        context: Option<Nullability>,
    ) -> Result<TypeParameter, ImportError> {
        // The only typar attributes we model are `[IsUnmanaged]` (the
        // `unmanaged` refinement) and `[Nullable]` (the typar's nullability).
        // Any other attribute is refused loud rather than silently collapsed
        // into the bare special-constraint set.
        let mut is_unmanaged = false;
        let mut direct_nullability: Option<Nullability> = None;
        for raw in &gp.attributes {
            let owning = self.image.attribute_owning_type(raw).map_err(|e| {
                ImportError::UnsupportedSignature {
                    detail: format!("generic parameter `{}` attribute owner: {e:?}", gp.name),
                }
            })?;
            match (owning.namespace.as_str(), owning.name.as_str()) {
                ("System.Runtime.CompilerServices", "IsUnmanagedAttribute") => is_unmanaged = true,
                ("System.Runtime.CompilerServices", "NullableAttribute") => {
                    if direct_nullability.is_some() {
                        return Err(ImportError::UnsupportedSignature {
                            detail: format!(
                                "generic parameter `{}` carries multiple NullableAttribute rows",
                                gp.name
                            ),
                        });
                    }
                    direct_nullability = Some(self.decode_typar_nullable_byte(raw, &gp.name)?);
                }
                // F#'s *conditional* equality/comparison constraints — `'T` has
                // equality/comparison only when its type argument does (FSharp.Core
                // marks e.g. `dict`/`set` typars this way). They are an F#-pickle
                // concept (modelled there as `FSharpTyparConstraint`), not an IL
                // constraint the [`TypeParameter`] model carries, so the IL
                // projection recognises and discards them — like `modreq`, read but
                // not interpreted — rather than fabricating a field. Genuinely
                // unknown typar attributes still fail loud below.
                ("Microsoft.FSharp.Core", "EqualityConditionalOnAttribute")
                | ("Microsoft.FSharp.Core", "ComparisonConditionalOnAttribute") => {}
                // Trimmer/linker annotations on a typar (`[DynamicallyAccessedMembers]`
                // constrains which members survive trimming of the argument type).
                // Pure tooling metadata — no bearing on the type model — so it is
                // recognised and discarded, not modelled.
                ("System.Diagnostics.CodeAnalysis", "DynamicallyAccessedMembersAttribute") => {}
                (ns, name) => {
                    return Err(ImportError::UnsupportedSignature {
                        detail: format!(
                            "generic parameter `{}` carries unsupported attribute `{ns}.{name}`",
                            gp.name
                        ),
                    });
                }
            }
        }
        // Direct `[Nullable]` wins; otherwise the enclosing scope default; a
        // typar with neither reads `Oblivious`.
        let nullability = direct_nullability
            .or(context)
            .unwrap_or(Nullability::Oblivious);

        let variance = match gp.variance {
            RawVariance::Invariant => Variance::Invariant,
            RawVariance::Covariant => Variance::Covariant,
            RawVariance::Contravariant => Variance::Contravariant,
        };

        let mut type_constraints = Vec::with_capacity(gp.constraints.len());
        for c in &gp.constraints {
            // A `GenericParamConstraint` row carries its own custom attributes.
            // The claim this code was written on — "the rows real compilers emit
            // have none" — is **false on .NET 9+**: the BCL ref pack attaches
            // `[Nullable]` to the constraint rows of the generic-math and parsing
            // interfaces (`where TSelf : IParsable<TSelf>` — the constraint *type*
            // carries a nullability annotation), and refusing them dropped **38
            // types** from `System.Runtime` alone, `INumber`/`IParsable`/
            // `IComparisonOperators` among them.
            //
            // So classify, as [`Self::project_generic_param`] does for the typar's
            // own attributes: recognise the ones the model can account for, and
            // refuse anything genuinely unknown. `[Nullable]` is *read* — not merely
            // tolerated — by the `walk_type` below, which is both where the
            // constraint type's inner nullability comes from and where a malformed
            // payload is refused; a trimmer annotation carries nothing this model
            // holds and is discarded.
            for raw in &c.attributes {
                let owning = self.image.attribute_owning_type(raw).map_err(|e| {
                    ImportError::UnsupportedSignature {
                        detail: format!(
                            "generic-parameter constraint on `{}` attribute owner: {e:?}",
                            gp.name
                        ),
                    }
                })?;
                match (owning.namespace.as_str(), owning.name.as_str()) {
                    // Read below by `walk_type`, not here.
                    ("System.Runtime.CompilerServices", "NullableAttribute") => {}
                    ("System.Diagnostics.CodeAnalysis", "DynamicallyAccessedMembersAttribute") => {}
                    (ns, name) => {
                        return Err(ImportError::UnsupportedSignature {
                            detail: format!(
                                "generic-parameter constraint on `{}` carries unsupported \
                                 attribute `{ns}.{name}`",
                                gp.name
                            ),
                        });
                    }
                }
            }
            let sig =
                c.ty.as_ref()
                    .map_err(|e| sig_error(e, "generic-parameter constraint"))?;
            // Walk the constraint type under the nullable ladder — its row's
            // `[NullableAttribute]` first, then the enclosing scope's
            // `[NullableContext]` default (`context`). A constraint is a full
            // (possibly generic) *type* (`where TSelf : IParsable<TSelf>` carries a
            // byte[]), so [`Self::walk_type`] — the field/return-position validator,
            // which decodes the source AND length-checks it against the type tree —
            // is the one that matches it, not the single-byte
            // `decode_typar_nullable_byte`. The walk is both the *validation* and
            // the source of the constraint type's inner nullability:
            //
            // * As validation, accepting `[Nullable]` on owner name alone would let
            //   a malformed row — a bad byte[] element, named args, an out-of-range
            //   byte, a zero-length payload, a bad ctor signature, a duplicate, or a
            //   byte[] whose length does not match the type's annotatable positions —
            //   project successfully here while the same attribute fails loud in
            //   every other position.
            //
            // * As data, the walk's `TypeRef` carries a `NullableType` per generic
            //   argument and array element, which the model *can* hold and a bare
            //   `project_type_ref` would flatten to `Oblivious` — collapsing
            //   `IEquatable<string?>` and `IEquatable<string>` to one projection.
            //   Only the *outer* node's nullability is dropped (a constraint is not
            //   a value position — no slot for it); the inner annotations are kept
            //   at the push below. Both rungs are pinned against real Roslyn output
            //   in `tests/projector_generic_nullability.rs`: a direct row on
            //   `IEquatable<string?>`, and the context fallback on `IEquatable<string>`
            //   under `#nullable enable` (whose row Roslyn *omits* precisely because
            //   it equals the scope default — reading it bare would report
            //   `Oblivious`, i.e. unannotated, for a constraint the source annotated).
            //
            // `walk_type` ignores any non-`[Nullable]` attribute (the trimmer one,
            // refused-or-passed by the owner loop above), so the full attribute slice
            // is safe. The walk runs *before* the `unmanaged`-marker block below, so
            // being the marker is not a way to skip validation — its `System.ValueType`
            // is a non-generic value type, so a well-formed payload annotates nothing
            // and the marker path discards a genuinely empty result.
            let walked = self
                .walk_type(&sig.ty, &c.attributes, context)
                .map_err(|e| match e {
                    ImportError::UnsupportedSignature { detail } => {
                        ImportError::UnsupportedSignature {
                            detail: format!(
                                "generic-parameter constraint on `{}`: {detail}",
                                gp.name
                            ),
                        }
                    }
                    other => other,
                })?;
            // `where T : unmanaged` emits a synthetic `System.ValueType
            // modreq(System.Runtime.InteropServices.UnmanagedType)` constraint
            // *in addition to* the value-type bit + `[IsUnmanaged]`. It is
            // redundant — consume it (set the flag, drop the row) rather than
            // surface a stray `System.ValueType` interface. Any *other* modifier
            // falls through to `project_type_ref`, which drops an ignorable
            // `modopt` and refuses an unrecognised `modreq` by name (no silent
            // drop of a real constraint).
            //
            // This is the one `modreq` the projector recognises *positionally*
            // rather than through `classify_mods`, so it reads the run itself.
            // There is no longer any question of an ignorable `modopt` hiding the
            // marker — the run is a list, not a chain, so "is the marker in it" is
            // the only question there is to ask.
            //
            // Consuming the marker does **not** license ignoring the rest of the
            // run: an unrecognised `modreq` riding alongside it is still refused
            // (`classify_mods`), and a *recognised* one is meaningless on a
            // constraint, so it is refused too. Only the marker is consumed.
            let mut marks_unmanaged = false;
            let mut rest: Vec<CustomMod> = Vec::new();
            for m in &sig.mods {
                if m.required && self.is_unmanaged_modreq(&m.modifier)? {
                    marks_unmanaged = true;
                } else {
                    rest.push(*m);
                }
            }
            if marks_unmanaged && self.is_system_value_type(&sig.ty)? {
                self.classify_mods(&rest)?
                    .reject_at("a generic-parameter constraint")?;
                is_unmanaged = true;
                continue;
            }
            // `walk_type` reads the *type*, not the position's modifier run, so the
            // constraint's modifiers are still classified here (as
            // `project_type_ref` used to). A constraint carries no modifier a
            // compiler emits (bar the `unmanaged` marker, consumed above), so a
            // *recognised* one (`volatile`, the read-only-byref `modreq`) is
            // meaningless and refused, and an unrecognised required one is refused by
            // name — exactly as at any other position that can carry neither.
            self.classify_mods(&sig.mods)?
                .reject_at("a generic-parameter constraint")?;
            // Keep the walked type — inner nullability preserved — not a bare
            // re-projection that would flatten it to `Oblivious`.
            type_constraints.push(walked.ty);
        }

        // `unmanaged` is additive on `struct`, never standalone — a `[IsUnmanaged]`
        // or `modreq(UnmanagedType)` signal without the value-type bit is malformed.
        if is_unmanaged && !gp.value_type {
            return Err(ImportError::UnsupportedSignature {
                detail: format!(
                    "generic parameter `{}` carries the unmanaged signal without the \
                     value-type constraint",
                    gp.name
                ),
            });
        }

        Ok(TypeParameter {
            name: gp.name.clone(),
            variance,
            reference_type_constraint: gp.reference_type,
            value_type_constraint: gp.value_type,
            default_constructor_constraint: gp.default_ctor,
            is_unmanaged,
            allows_ref_struct: gp.allows_ref_struct,
            nullability,
            type_constraints,
        })
    }

    /// Decode a typar's direct `[NullableAttribute]` to its single byte. A
    /// typar position is single-valued, so the `byte[]` overload (even length-1)
    /// is refused — only the `NullableAttribute(byte)` form is valid here.
    fn decode_typar_nullable_byte(
        &self,
        raw: &RawAttribute,
        typar_name: &str,
    ) -> Result<Nullability, ImportError> {
        let widths = EnumWidths::new();
        let decoded = self.image.decode_attribute(raw, &widths).map_err(|e| {
            ImportError::UnsupportedSignature {
                detail: format!("typar `{typar_name}` NullableAttribute decode: {e:?}"),
            }
        })?;
        if !decoded.named_args.is_empty() {
            return Err(ImportError::UnsupportedSignature {
                detail: format!("typar `{typar_name}` NullableAttribute carries named args"),
            });
        }
        match decoded.fixed_args.as_slice() {
            [FixedArg::Integral(IntegralParam::UInt8(b))] => nullability_from_byte(*b),
            [FixedArg::Array(_)] => Err(ImportError::UnsupportedSignature {
                detail: format!(
                    "typar `{typar_name}` NullableAttribute carries a byte[] payload — a typar \
                     position is single-valued"
                ),
            }),
            other => Err(ImportError::UnsupportedSignature {
                detail: format!(
                    "typar `{typar_name}` NullableAttribute first arg not a single byte: {other:?}"
                ),
            }),
        }
    }

    /// Whether a `modreq` modifier names
    /// `System.Runtime.InteropServices.UnmanagedType` (the `unmanaged` marker).
    fn is_unmanaged_modreq(&self, modifier: &TypeScope) -> Result<bool, ImportError> {
        Ok(matches!(
            self.named_from_scope(modifier, Vec::new())?,
            TypeRef::Named { namespace, name, .. }
                if namespace.as_slice() == ["System", "Runtime", "InteropServices"]
                    && name == "UnmanagedType"
        ))
    }

    /// Classify a position's `CustomMod` run, per ECMA-335 II.7.1.1.
    ///
    /// The rule is ECMA-335 II.7.1.1's, and it is the whole reason `modreq` and
    /// `modopt` are different bytes: an **optional** modifier may be ignored by
    /// a tool that does not understand it, so an unrecognised `modopt` is
    /// *dropped*; a **required** one must be understood, so an unrecognised
    /// `modreq` is *refused* — by name, so the diagnostic says which construct
    /// the reader is missing rather than "custom modifier". (FCS is laxer: it
    /// drops both — `import.fs:305`, "All custom modifiers are ignored" — which
    /// would silently mismodel, say, a `volatile` field as an ordinary one.)
    ///
    /// The two recognised `modreq`s are position-sensitive, so this only reports
    /// them: the *caller* decides whether they are meaningful where it found
    /// them (a read-only marker only over a byref, `volatile` only on a field)
    /// and refuses if not.
    ///
    /// Note what this function does *not* do: it does not hand back a type.
    /// Modifiers are a run on the position ([`ModifiedType`]), so the type is
    /// already in hand — `mt.ty` — and no caller can be tricked into matching on
    /// a head that a modifier has displaced. That used to be possible, and it
    /// went wrong five times.
    fn classify_mods(&self, run: &[CustomMod]) -> Result<Modifiers, ImportError> {
        let mut mods = Modifiers::default();
        for m in run {
            if !m.required {
                // II.7.1.1: an optional modifier may be ignored by a tool that
                // does not understand it. We understand none of them.
                continue;
            }
            if self.is_readonly_ref_modreq(&m.modifier)? {
                mods.readonly_ref = true;
            } else if self.is_volatile_modreq(&m.modifier)? {
                mods.volatile = true;
            } else {
                return Err(ImportError::UnsupportedSignature {
                    detail: format!(
                        "unrecognised required custom modifier `{}`",
                        self.modifier_name(&m.modifier)?
                    ),
                });
            }
        }
        Ok(mods)
    }

    /// A modifier type's `Namespace.Name`, for diagnostics.
    fn modifier_name(&self, modifier: &TypeScope) -> Result<String, ImportError> {
        Ok(match self.named_from_scope(modifier, Vec::new())? {
            TypeRef::Named {
                namespace, name, ..
            } if namespace.is_empty() => name,
            TypeRef::Named {
                namespace, name, ..
            } => format!("{}.{name}", namespace.join(".")),
            other => format!("{other:?}"),
        })
    }

    /// Whether a `modreq` modifier names
    /// `System.Runtime.InteropServices.InAttribute` — the read-only-byref
    /// marker C# emits for an `in` / `ref readonly` parameter and for a
    /// `ref readonly` return, field, or indexer.
    fn is_readonly_ref_modreq(&self, modifier: &TypeScope) -> Result<bool, ImportError> {
        Ok(matches!(
            self.named_from_scope(modifier, Vec::new())?,
            TypeRef::Named { namespace, name, .. }
                if namespace.as_slice() == ["System", "Runtime", "InteropServices"]
                    && name == "InAttribute"
        ))
    }

    /// Whether a position's attributes mark its byref *read-only*.
    ///
    /// Read-only-ness has **two** encodings, and a faithful model must accept
    /// both — Roslyn picks between them by whether the CLI needs to *match* on
    /// it. `modreq(InAttribute)` goes in the signature only where an override or
    /// interface implementation must line up: a byref **return** (and the
    /// property/indexer type mirroring it), and an `in` parameter of a
    /// virtual/abstract/interface member. Everywhere else — an `in` parameter of
    /// an ordinary method, a `ref readonly` field — the signature is a plain
    /// byref and the read-only-ness rides an attribute on the position instead:
    /// `[IsReadOnly]` (C# 7.2 `in`, `ref readonly` field/return) or
    /// `[RequiresLocation]` (C# 12 `ref readonly` parameter).
    ///
    /// Projecting only the modifier would therefore make `in int` read as a
    /// writable `ref int` on a non-virtual method and as `inref` on a virtual
    /// one — the same source construct, two answers. The projector ORs the two
    /// encodings so the model's `readonly` bit means one thing: *the callee may
    /// not write through this reference*. Callers consult this only where they
    /// have already established the position is a byref, so an attribute on a
    /// non-byref position can never fabricate one.
    fn has_readonly_ref_attribute(&self, attrs: &[RawAttribute]) -> Result<bool, ImportError> {
        Ok(self.has_attribute(
            attrs,
            "System.Runtime.CompilerServices",
            "IsReadOnlyAttribute",
        )? || self.has_attribute(
            attrs,
            "System.Runtime.CompilerServices",
            "RequiresLocationAttribute",
        )?)
    }

    /// Whether a `modreq` modifier names
    /// `System.Runtime.CompilerServices.IsVolatile` — the marker that *is* the
    /// encoding of a C# `volatile` field (there is no flag bit for it).
    fn is_volatile_modreq(&self, modifier: &TypeScope) -> Result<bool, ImportError> {
        Ok(matches!(
            self.named_from_scope(modifier, Vec::new())?,
            TypeRef::Named { namespace, name, .. }
                if namespace.as_slice() == ["System", "Runtime", "CompilerServices"]
                    && name == "IsVolatile"
        ))
    }

    /// Whether a `modreq` modifier names
    /// `System.Runtime.CompilerServices.IsExternalInit` (the C# 9 `init`-setter
    /// marker, carried on the setter's `void` return).
    fn is_external_init_modreq(&self, modifier: &TypeScope) -> Result<bool, ImportError> {
        Ok(matches!(
            self.named_from_scope(modifier, Vec::new())?,
            TypeRef::Named { namespace, name, .. }
                if namespace.as_slice() == ["System", "Runtime", "CompilerServices"]
                    && name == "IsExternalInit"
        ))
    }

    /// Whether a constraint type is the *uninstantiated* `System.ValueType`.
    /// The canonical `unmanaged` marker is a bare `System.ValueType`; a
    /// (malformed) instantiation like `System.ValueType<int>` is not it, so it
    /// must not be consumed/dropped — the `type_args` guard routes it to the
    /// refuse-loud path instead.
    fn is_system_value_type(&self, ty: &TypeSig) -> Result<bool, ImportError> {
        Ok(matches!(
            self.project_type(ty, false)?,
            TypeRef::Named { namespace, name, type_args, .. }
                if namespace.as_slice() == ["System"] && name == "ValueType" && type_args.is_empty()
        ))
    }
}

// ============================================================================
// Member projection (stage 7.5a — IL/structural members)
// ============================================================================

impl Ecma335Assembly {
    /// Project the members of an IL-kinded type. Mirrors the reference
    /// `project_il_members`: every method (bar the property/event accessors),
    /// then every field, property, and event — in table order.
    ///
    /// The reader keeps accessor methods (getter/setter/add/remove/raise) in
    /// `td.methods` rather than re-homing them onto the owning property/event,
    /// so this derives the accessor-exclusion set from the property/event
    /// accessor handles and skips those positions, surfacing
    /// them only through the property/event projection. (Non-standard `Other`
    /// accessors, which the model can't carry, are in that exclusion set too:
    /// their owning property/event is dropped and recorded — see
    /// [`reject_other_accessors`] — and the accessor methods stay hidden with
    /// it.)
    fn project_il_members(
        &self,
        td: &TypeDef,
        type_context: Option<Nullability>,
    ) -> Result<ProjectedMembers, ImportError> {
        let accessors = accessor_ids(td);
        let real_method_names = non_witness_method_names(td);

        let mut out = ProjectedMembers::default();
        for (i, m) in td.methods.iter().enumerate() {
            if accessors.contains(&(i as u32)) {
                continue;
            }
            if is_fsharp_witness_duplicate(&m.name, &real_method_names) {
                continue;
            }
            out.push_or_skip(
                &m.name,
                self.project_method(m, type_context, false)
                    .map(Member::Method),
            );
        }
        for f in &td.fields {
            out.push_or_skip(
                &f.name,
                self.project_field(f, type_context).map(Member::Field),
            );
        }
        for p in &td.properties {
            out.push_or_skip(
                &p.name,
                self.project_property(td, p, type_context)
                    .map(Member::Property),
            );
        }
        for e in &td.events {
            out.push_or_skip(
                &e.name,
                self.project_event(td, e, type_context).map(Member::Event),
            );
        }
        Ok(out)
    }

    /// Project one `MethodDef` into a `MethodLike`. `type_context` is the
    /// enclosing type's `[NullableContext]` default; the method's own context
    /// (its `[NullableContext]` ?? the type's) governs the parameter/return
    /// and method-typar nullability. Attribute-derived facts (7.7) are left at
    /// their placeholders.
    /// `is_property_setter` gates acceptance of the `init`-setter's
    /// `modreq(IsExternalInit) void` return (see [`Self::project_return`]); it is
    /// `true` only when validating a property's set accessor, `false` for a
    /// standalone method, a getter, or an event accessor.
    fn project_method(
        &self,
        m: &Method,
        type_context: Option<Nullability>,
        is_property_setter: bool,
    ) -> Result<MethodLike, ImportError> {
        let sig = m
            .signature
            .as_ref()
            .map_err(|e| sig_error(e, "method signature"))?;
        let method_context = self
            .detect_nullable_context(&m.attributes)?
            .or(type_context);
        // Varargs methods carry a second (call-site) parameter list past the
        // sentinel; the model has no slot for it. Refuse rather than project a
        // truncated parameter list.
        if matches!(sig.calling_convention, CallConv::VarArg) {
            return Err(ImportError::UnsupportedSignature {
                detail: format!("method `{}` uses varargs", m.name),
            });
        }
        // ECMA-335 stores the generic arity in the calling-convention byte *and*
        // the `GenericParam` row count; they must agree, or the metadata was
        // mis-decoded. Refuse rather than guess.
        let cc_arity = match sig.calling_convention {
            CallConv::Generic { count } => count as usize,
            _ => 0,
        };
        let generic_parameters = m
            .generic_params
            .iter()
            .map(|gp| self.project_generic_param(gp, method_context))
            .collect::<Result<Vec<_>, _>>()?;
        if cc_arity != generic_parameters.len() {
            return Err(ImportError::UnsupportedSignature {
                detail: format!(
                    "method `{}` calling-convention arity {} disagrees with generic_parameters \
                     length {}",
                    m.name,
                    cc_arity,
                    generic_parameters.len()
                ),
            });
        }
        let parameters = sig
            .parameters
            .iter()
            .map(|p| self.project_parameter(p, method_context))
            .collect::<Result<Vec<_>, _>>()?;
        let (return_type, return_nullability) =
            self.project_return(sig, method_context, is_property_setter)?;
        // ECMA-335 §II.22.26: a constructor is a `.ctor`/`.cctor` method
        // *carrying* `rtspecialname` — the name alone is reserved but not
        // sufficient.
        let is_constructor = m.is_rt_special_name && (m.name == ".ctor" || m.name == ".cctor");

        // Attribute-derived facts (7.7b).
        let compiler_feature_required = self.detect_compiler_feature_required(&m.attributes)?;
        let raw_obsolete = self.detect_obsolete(&m.attributes)?;
        // Roslyn pairs `[CompilerFeatureRequired("RequiredMembers")]` with a
        // synthetic `[Obsolete(error)]` on every non-`[SetsRequiredMembers]`
        // constructor of a type with required members — a fallback signal for
        // pre-C#-11 compilers a modern one ignores. Drop the projected obsolete
        // in exactly that emission shape (read from the already-decoded gate, so
        // the suppression's input is visible) rather than misclassify valid C#11
        // callers as obsolete-error users.
        let obsolete = if raw_obsolete.is_some()
            && is_constructor
            && has_required_members_feature_gate(&compiler_feature_required)
        {
            None
        } else {
            raw_obsolete
        };

        let implements = self.project_implements(m);
        let unclassified_impls = self.project_unclassified(m);

        Ok(MethodLike {
            name: m.name.clone(),
            access: project_member_access(m.accessibility)?,
            signature: MethodSignature {
                parameters,
                return_type,
                return_nullability,
            },
            // The IL projector assumes a single argument group (the C#/VB fact).
            // `enumerate_with_skips_impl` resets this to `None` for every method
            // of an assembly whose host F# signature pickle decodes, since a
            // curried F# member is indistinguishable here. See the OV-6.1 plan.
            arg_group_count: Some(1),
            // The signature's `HASTHIS` bit is the canonical static-ness signal
            // (mirrors the reference's `!signature.instance`).
            is_static: !sig.has_this,
            is_virtual: m.is_virtual,
            is_abstract: m.is_abstract,
            is_final: m.is_final,
            is_newslot: m.is_new_slot,
            is_hide_by_sig: m.is_hide_by_sig,
            is_constructor,
            // A genuine method/function by default; the module property→method
            // rebrand in `project_fsharp_members` overrides this for module values.
            module_value: None,
            // The raw IL projection cannot see F# argument groups; the pickle
            // merge sets this from the host signature (`rebuild_module_member_list`).
            is_module_value_binding: false,
            // The pickled binding location is likewise merge-driven
            // (`rebuild_module_member_list`); IL metadata has no source ranges.
            definition_range: None,
            // The CLR `[Extension]` marker; `project_fsharp_members` additionally
            // sets this for F#-native name-mangled module extensions (7.5b).
            is_extension_method: self.has_attribute(
                &m.attributes,
                "System.Runtime.CompilerServices",
                "ExtensionAttribute",
            )?,
            // The F#-native augmentation fact is pickle-driven
            // (`apply_module_member_projection`) or, with no usable pickle,
            // guessed-with-uncertainty from the IL name mangling in
            // `project_fsharp_members` below.
            augmentation: Augmentation::No,
            generic_parameters,
            obsolete,
            experimental: self.detect_experimental(&m.attributes)?,
            sets_required_members: self.has_sets_required_members(&m.attributes)?,
            compiler_feature_required,
            source_name: self.detect_compilation_source_name(&m.attributes)?,
            custom_attrs: Vec::new(),
            metadata_token: m.token,
            implements,
            unclassified_impls,
        })
    }

    /// Project a method's explicit-interface implementations (from its
    /// `MethodImpl` rows) into the model. This is an *enrichment*: failing to
    /// decode an implemented interface (an unreadable `TypeSpec`, or a `modreq`
    /// in one of its arguments) drops that entry rather than failing the member
    /// — the member genuinely exists. Shared by method, property-accessor, and
    /// event-accessor projection (an explicit property/event surfaces its
    /// `MethodImpl` on the accessor, which is otherwise excluded from the
    /// projected member list).
    ///
    /// The surfaced [`ImplementedMember`] is the declaration side's own kind
    /// and name, straight from what `MethodSemantics` proved
    /// ([`DeclSemantics`]) — independent of what kind of member `m` is being
    /// projected *as*. An in-module accessor declaration carries its owning
    /// property's/event's name verbatim (never conventionally stripped — an
    /// interface property may itself be named `get_Value`); an ordinary
    /// method's name stands as written however accessor-like it looks; and a
    /// declaration whose `MethodSemantics` is out of reach stays
    /// [`ImplementedMember::Unresolved`] with its raw name — the projection
    /// never turns that uncertainty into a prefix-stripped guess.
    /// Project a method's in-assembly-undecidable `MethodImpl` rows (see
    /// [`UnclassifiedMethodImpl`]). The same enrichment posture as
    /// [`Self::project_implements`]: a parent that fails to project drops that
    /// entry rather than failing the member.
    fn project_unclassified(&self, m: &Method) -> Vec<UnclassifiedMethodImpl> {
        m.unclassified_impls
            .iter()
            .filter_map(|u| {
                self.project_type_ref(&u.parent)
                    .ok()
                    .map(|parent| UnclassifiedMethodImpl {
                        parent,
                        member: u.member.clone(),
                    })
            })
            .collect()
    }

    fn project_implements(&self, m: &Method) -> Vec<InterfaceMemberImpl> {
        m.implements
            .iter()
            .filter_map(|ei| {
                let member = match &ei.decl {
                    DeclSemantics::Accessor(AccessorOwner::Property, owner) => {
                        ImplementedMember::Property(owner.clone())
                    }
                    DeclSemantics::Accessor(AccessorOwner::Event, owner) => {
                        ImplementedMember::Event(owner.clone())
                    }
                    DeclSemantics::OrdinaryMethod => ImplementedMember::Method(ei.member.clone()),
                    DeclSemantics::Unresolved => ImplementedMember::Unresolved(ei.member.clone()),
                };
                ei.interface
                    .as_ref()
                    .ok()
                    .and_then(|sig| self.project_type_ref(sig).ok())
                    .map(|interface| InterfaceMemberImpl { interface, member })
            })
            .collect()
    }

    /// Project one method parameter. A `ref T` parameter is recorded as
    /// `is_byref` over the referent `T` (the byref wrapper is a flag, not part
    /// of the projected type), and a `modreq(InAttribute)` over that byref — C#
    /// `in` / `ref readonly` — as `is_readonly_ref`. `context` is the enclosing
    /// method's nullable context for the position walk.
    fn project_parameter(
        &self,
        p: &Param,
        context: Option<Nullability>,
    ) -> Result<Parameter, ImportError> {
        let mods = self.classify_mods(&p.ty.mods)?;
        // The head, straight out of the position — a modifier cannot be in front
        // of it to hide it.
        let (inner, is_byref) = match &p.ty.ty {
            TypeSig::ByRef(inner) => (inner.as_ref(), true),
            _ => (&p.ty, false),
        };
        if !is_byref {
            // `volatile` is a field-only marker, and a read-only marker means
            // nothing without the byref it qualifies.
            mods.reject_at("a parameter that is not a byref")?;
        } else if mods.volatile {
            return Err(ImportError::UnsupportedSignature {
                detail: "`volatile` modifier (`modreq(IsVolatile)`) on a parameter".into(),
            });
        }
        let walked = self.walk_position(inner, &p.attributes, context)?;
        // F# `?x` carries `[<OptionalArgument>]` (and is typed `FSharpOption<T>`);
        // a .NET optional is the `Optional` flag and/or a `Constant` default
        // value (a C# `x = <value>`; the flag alone is common in COM/VB
        // metadata). The two are distinct calling conventions, so keep them
        // apart, carrying the decoded default value for the .NET form.
        let default = if self.has_attribute(
            &p.attributes,
            "Microsoft.FSharp.Core",
            "OptionalArgumentAttribute",
        )? {
            ParamDefault::FSharpOptional
        } else if let Some(value) = &p.default_value {
            ParamDefault::Optional(Some(project_constant(value)))
        } else if p.optional {
            // An optional parameter's value may ride on a `Constant`-less
            // attribute: `decimal`/`DateTime` defaults have no `Constant` row, so
            // the value is on `[DecimalConstantAttribute]`/
            // `[DateTimeConstantAttribute]` beside the `Optional` flag. The
            // attribute is only a *default* when the parameter is optional —
            // `[DecimalConstant]` applied alone (no `Optional`) is just an
            // annotation on a still-required parameter, so it must be gated here
            // rather than projected as an unconditional default.
            ParamDefault::Optional(self.decode_attribute_default(&p.attributes))
        } else {
            ParamDefault::None
        };
        Ok(Parameter {
            name: p.name.clone(),
            ty: walked.ty,
            is_byref,
            // `[In, Out] ref` (COM-style) carries both flags and is logically
            // byref, not out; a pure `out` sets only `Out`.
            is_out: p.is_out && !p.is_in,
            // Both encodings (see `has_readonly_ref_attribute`), and only for a
            // byref. The `Param` row's `In` *flag* is neither: COM-style
            // `[In] ref` sets that bit on an ordinary writable byref.
            is_readonly_ref: is_byref
                && (mods.readonly_ref || self.has_readonly_ref_attribute(&p.attributes)?),
            default,
            is_param_array: self.has_attribute(&p.attributes, "System", "ParamArrayAttribute")?,
            nullability: walked.nullability,
        })
    }

    /// Walk a byref-capable outer position — a method return, a field, or a
    /// property/indexer type — into `(type, nullability, modifiers)`. An outer
    /// `ref T` (`ELEMENT_TYPE_BYREF`) is kept as `TypeRef::ByRef` over the
    /// referent's walked type, with the referent's nullability: the byref
    /// wrapper itself is never annotable and consumes no `[Nullable]` byte, so
    /// the position's suffix belongs to `T`. Anything else walks straight
    /// through. A nested byref (a byref referent) is refused.
    ///
    /// The position's modifier run is classified ([`Self::classify_mods`]): a
    /// `modreq(InAttribute)` over the byref is folded into the `ByRef` node as
    /// `readonly` (a `ref readonly` return/field/indexer), and the returned
    /// [`Modifiers`] carries whatever the *position* must still interpret — in
    /// practice just `volatile`, which only [`Self::project_field`] accepts. A
    /// caller that cannot interpret it must `reject_at` it.
    fn walk_byref_position(
        &self,
        sig: &ModifiedType,
        attrs: &[RawAttribute],
        context: Option<Nullability>,
    ) -> Result<(TypeRef, Nullability, Modifiers), ImportError> {
        let mods = self.classify_mods(&sig.mods)?;
        match &sig.ty {
            TypeSig::ByRef(inner) => {
                // A byref-to-byref (`ELEMENT_TYPE_BYREF` referent) is malformed —
                // ECMA-335 forbids it. The nullable-walk path refuses it, but the
                // oblivious fast path (`project_type_ref`) would fabricate a
                // nested `ByRef`, so refuse explicitly here to stay fail-loud on
                // both paths (correctness over availability). The referent's own
                // modifier run, if any, sits beside it and cannot hide it.
                if matches!(inner.ty, TypeSig::ByRef(_)) {
                    return Err(ImportError::UnsupportedSignature {
                        detail: "byref referent is itself a byref".into(),
                    });
                }
                let readonly = mods.readonly_ref || self.has_readonly_ref_attribute(attrs)?;
                let walked = self.walk_position(inner, attrs, context)?;
                Ok((
                    TypeRef::ByRef {
                        inner: Box::new(walked.ty),
                        readonly,
                    },
                    walked.nullability,
                    Modifiers {
                        readonly_ref: false,
                        ..mods
                    },
                ))
            }
            _ => {
                if mods.readonly_ref {
                    return Err(ImportError::UnsupportedSignature {
                        detail: "read-only-ref modifier (`modreq(InAttribute)`) not over a byref"
                            .into(),
                    });
                }
                // `walk_type`, not `walk_position`: this position's run is
                // already classified, and `mods` may carry a `volatile` that
                // only `project_field` is allowed to interpret — re-classifying
                // it here would refuse it as a nested-position modifier.
                let walked = self.walk_type(&sig.ty, attrs, context)?;
                Ok((walked.ty, walked.nullability, mods))
            }
        }
    }

    /// Project a method return into `(type, return-nullability)`. `void` →
    /// `Primitive::Void`; a `ref T` return is kept as `TypeRef::ByRef` with the
    /// nullability of the referent (the byref wrapper consumes no byte); a
    /// `modreq` return is refused. A `modreq(IsExternalInit) void` projects as
    /// `void` **only** when `is_property_setter` — that modifier is the C# 9
    /// `init`-setter marker and appears nowhere else, so on any other method
    /// (or an event accessor) it is a signature-significant modifier the model
    /// can't carry and the member is refused rather than silently flattened.
    /// The walk reads the return position's `[Nullable]` (the seq-0 `Param` row)
    /// under `context`.
    fn project_return(
        &self,
        sig: &MethodSig,
        context: Option<Nullability>,
        is_property_setter: bool,
    ) -> Result<(TypeRef, Nullability), ImportError> {
        match &sig.return_type {
            RetType::Void(modifiers) => {
                // An ignorable `modopt` before the `void` is dropped (II.7.1.1),
                // as anywhere else. What remains must be understood: accept the
                // `init`-setter's `modreq(IsExternalInit)` only on a property
                // setter (the sole compiler-emitted case), where it projects as
                // a plain void-returning accessor its property records via
                // `has_setter`. Anywhere else — a normal method, an event
                // accessor, or any other `modreq` — refuse rather than silently
                // drop an ABI-changing modifier.
                let required: Vec<TypeScope> = modifiers
                    .iter()
                    .filter(|m| m.required)
                    .map(|m| m.modifier)
                    .collect();
                let is_init = is_property_setter
                    && matches!(required.as_slice(), [m] if self.is_external_init_modreq(m)?);
                if is_init {
                    Ok((TypeRef::Primitive(Primitive::Void), Nullability::Oblivious))
                } else if required.is_empty() {
                    // Only `modopt`s: ignorable, so this is a plain `void`.
                    Ok((TypeRef::Primitive(Primitive::Void), Nullability::Oblivious))
                } else {
                    Err(ImportError::UnsupportedSignature {
                        detail: "required custom modifier on a void return (not an `init` setter)"
                            .into(),
                    })
                }
            }
            RetType::Type(t) => {
                let (ty, nullability, mods) =
                    self.walk_byref_position(t, &sig.return_attributes, context)?;
                mods.reject_at("a method return")?;
                Ok((ty, nullability))
            }
        }
    }

    /// Project one `Field`. A `ref T` field (a `ref` field in a `ref struct`)
    /// keeps `T&` as its type, like a byref return, and a `ref readonly` one
    /// keeps it as a `readonly` `T&`. The field type is also the one position
    /// where `modreq(IsVolatile)` is meaningful — it *is* the encoding of C#'s
    /// `volatile`, so it is peeled into [`Field::is_volatile`] rather than
    /// refused. `type_context` is the enclosing type's nullable context for the
    /// field type's position walk.
    fn project_field(
        &self,
        f: &RawField,
        type_context: Option<Nullability>,
    ) -> Result<Field, ImportError> {
        let sig = f
            .signature
            .as_ref()
            .map_err(|e| sig_error(e, "field type"))?;
        let (ty, nullability, mods) = self.walk_byref_position(sig, &f.attributes, type_context)?;
        Ok(Field {
            name: f.name.clone(),
            access: project_member_access(f.accessibility)?,
            ty,
            is_static: f.is_static,
            is_init_only: f.is_init_only,
            is_volatile: mods.volatile,
            is_literal: f.is_literal,
            is_required: self.has_attribute(
                &f.attributes,
                "System.Runtime.CompilerServices",
                "RequiredMemberAttribute",
            )?,
            compiler_feature_required: self.detect_compiler_feature_required(&f.attributes)?,
            nullability,
            custom_attrs: Vec::new(),
        })
    }

    /// Project one `Property`. Accessibility is the least-restrictive of the
    /// getter/setter; the index dimension's *types* come from the getter
    /// accessor (or the setter's parameters minus the trailing value), since
    /// the reader does not retain the `PropertySig` index parameters. The
    /// accessor signatures are re-validated here — they are excluded from the
    /// plain method list, so this is the only place a weird accessor signature
    /// would surface.
    fn project_property(
        &self,
        td: &TypeDef,
        p: &RawProperty,
        type_context: Option<Nullability>,
    ) -> Result<Property, ImportError> {
        reject_other_accessors("property", &p.name, &p.other_accessors)?;
        let sig = p
            .signature
            .as_ref()
            .map_err(|e| sig_error(e, "property type"))?;
        // A `ref`-returning property/indexer (`Span<T>.this[i]`, `List.ValueRef`)
        // keeps `T&` as its type, like a byref return — `readonly` when it is a
        // `ref readonly` one. `volatile` is a field-only marker, so a property
        // carrying it is refused.
        let (ty, nullability, mods) = self.walk_byref_position(sig, &p.attributes, type_context)?;
        mods.reject_at("a property type")?;

        let getter = p.getter.map(|id| &td.methods[id.0 as usize]);
        let setter = p.setter.map(|id| &td.methods[id.0 as usize]);
        let proj_getter = match getter {
            Some(g) => {
                self.reject_generic_accessor("property", &p.name, "getter", g)?;
                Some(self.project_method(g, type_context, false)?)
            }
            None => None,
        };
        let proj_setter = match setter {
            Some(s) => {
                self.reject_generic_accessor("property", &p.name, "setter", s)?;
                // The set accessor is the one position an `init` setter's
                // `modreq(IsExternalInit) void` return is accepted.
                Some(self.project_method(s, type_context, true)?)
            }
            None => None,
        };

        // The getter's *own* accessibility, recorded separately from the
        // property-level `access` below: a read (`recv.P`) goes through the
        // getter, so a consumer that types a read must gate on this, not on the
        // setter-inflatable `access`. `None` for a write-only property.
        let getter_access = getter
            .map(|g| project_member_access(g.accessibility))
            .transpose()?;

        let access = match (getter, setter) {
            (Some(g), Some(s)) => max_access(
                project_member_access(g.accessibility)?,
                project_member_access(s.accessibility)?,
            ),
            (Some(g), None) => project_member_access(g.accessibility)?,
            (None, Some(s)) => project_member_access(s.accessibility)?,
            (None, None) => {
                return Err(ImportError::UnsupportedSignature {
                    detail: format!("property `{}` has no accessors", p.name),
                });
            }
        };

        // The getter's parameters are exactly the index dimension; the setter's
        // are that dimension plus a trailing `value`.
        let index_params: &[Parameter] = match (proj_getter.as_ref(), proj_setter.as_ref()) {
            (Some(g), _) => g.signature.parameters.as_slice(),
            (None, Some(s)) => match s.signature.parameters.split_last() {
                Some((_value, idx)) => idx,
                None => &[],
            },
            (None, None) => &[],
        };
        let parameters = index_params_to_index_params(index_params, &p.name)?;

        // No `PropertySig` `HASTHIS` is retained; the accessor's static-ness is
        // the authoritative (and, for valid metadata, equivalent) signal.
        let is_static = match (&proj_getter, &proj_setter) {
            (Some(g), _) => g.is_static,
            (None, Some(s)) => s.is_static,
            // Unreachable: the no-accessor case already returned above.
            (None, None) => false,
        };

        // An explicit interface property carries its `MethodImpl` on its
        // accessors (which are excluded from the projected method list), so
        // the structured info is the *union* over both — the getter and setter
        // may satisfy different interfaces (VB's `Property P … Implements
        // IRead.P, IWrite.P` with a get-only IRead and a set-only IWrite),
        // while a get+set interface property resolved through MethodSemantics
        // contributes one row per accessor that dedups to a single entry. The
        // surfaced member is the declaration side's own kind and name, since
        // the implementing property's own name need not match the interface's.
        // One contribution per *distinct* accessor method: crafted IL may
        // claim one MethodDef for both roles, and its MethodImpl rows must
        // not be projected once per role — value-identical unresolved entries
        // are deliberately never collapsed, so a role-duplicated projection
        // would make one row look like two.
        let unique_accessors = distinct_accessors([getter, setter]);
        let implements = union_accessor_impls(
            unique_accessors
                .iter()
                .flat_map(|a| self.project_implements(a)),
        );
        // No dedup here: identical unclassified entries always denote
        // distinct declarations (see `union_accessor_impls`).
        let unclassified_impls: Vec<UnclassifiedMethodImpl> = unique_accessors
            .iter()
            .flat_map(|a| self.project_unclassified(a))
            .collect();

        Ok(Property {
            name: p.name.clone(),
            access,
            ty,
            parameters,
            is_static,
            has_getter: p.getter.is_some(),
            has_setter: p.setter.is_some(),
            getter_access,
            is_required: self.has_attribute(
                &p.attributes,
                "System.Runtime.CompilerServices",
                "RequiredMemberAttribute",
            )?,
            compiler_feature_required: self.detect_compiler_feature_required(&p.attributes)?,
            nullability,
            custom_attrs: Vec::new(),
            implements,
            unclassified_impls,
        })
    }

    /// Project one `Event`. Accessibility is the least-restrictive of add/remove
    /// (the raise accessor is observed-only); static-ness is per-accessor and
    /// add/remove must agree. ECMA-335 mandates both add and remove; a missing
    /// one is refused. The accessor signatures are re-validated, as for
    /// properties.
    fn project_event(
        &self,
        td: &TypeDef,
        e: &RawEvent,
        type_context: Option<Nullability>,
    ) -> Result<Event, ImportError> {
        reject_other_accessors("event", &e.name, &e.other_accessors)?;
        let sig = e
            .event_type
            .as_ref()
            .map_err(|err| sig_error(err, "event delegate type"))?;
        // A delegate type is always a class; a byref `EventType` (a `TypeSpec`
        // whose outer shape is byref) has no meaningful surface and no model
        // slot — refuse it loud, as for byref field/property types, rather than
        // project a `TypeRef::ByRef` delegate.
        //
        // `sig.ty` is the head, whatever modifiers the position carries — so this
        // guard cannot be walked past. (It could, when a modifier was a node in
        // front of the type: `modopt(X) BYREF D` presented as a `Modified`, this
        // read `false`, and six members that should have been refused were
        // projected. The run is beside the type now, so there is no "before" for
        // a modifier to hide in. Any unrecognised `modreq` on the position is
        // still refused, by `walk_position` below.)
        if matches!(sig.ty, TypeSig::ByRef(_)) {
            return Err(ImportError::UnsupportedSignature {
                detail: format!("event `{}` has a byref delegate type", e.name),
            });
        }
        let walked = self.walk_position(sig, &e.attributes, type_context)?;

        let (add, remove) = match (e.add, e.remove) {
            (Some(a), Some(r)) => (&td.methods[a.0 as usize], &td.methods[r.0 as usize]),
            _ => {
                return Err(ImportError::UnsupportedSignature {
                    detail: format!("event `{}` is missing an add or remove accessor", e.name),
                });
            }
        };
        self.reject_generic_accessor("event", &e.name, "add", add)?;
        self.reject_generic_accessor("event", &e.name, "remove", remove)?;
        self.project_method(add, type_context, false)?;
        self.project_method(remove, type_context, false)?;
        let fire = match e.raise {
            Some(raise) => {
                let fire = &td.methods[raise.0 as usize];
                self.reject_generic_accessor("event", &e.name, "raise", fire)?;
                self.project_method(fire, type_context, false)?;
                Some(fire)
            }
            None => None,
        };

        if add.is_static != remove.is_static {
            return Err(ImportError::UnsupportedSignature {
                detail: format!(
                    "event `{}` has add/remove disagreeing on static-ness",
                    e.name
                ),
            });
        }
        // As for properties, an explicit interface event carries its
        // `MethodImpl` on its accessors, unioned over add, remove, *and* fire
        // — fire is a first-class event semantic, and a fire-only mapping is
        // valid IL (a MethodSemantics-resolved implemented event dedups to one
        // entry regardless of which accessors carry rows); the surfaced member
        // is the declaration side's own kind and name. One contribution per
        // *distinct* accessor method: crafted IL may claim one MethodDef for
        // several roles, and its rows must not be projected once per role.
        let unique_accessors = distinct_accessors([Some(add), Some(remove), fire]);
        let implements = union_accessor_impls(
            unique_accessors
                .iter()
                .flat_map(|a| self.project_implements(a)),
        );
        // No dedup here: identical unclassified entries always denote
        // distinct declarations (see `union_accessor_impls`).
        let unclassified_impls: Vec<UnclassifiedMethodImpl> = unique_accessors
            .iter()
            .flat_map(|a| self.project_unclassified(a))
            .collect();

        Ok(Event {
            name: e.name.clone(),
            access: max_access(
                project_member_access(add.accessibility)?,
                project_member_access(remove.accessibility)?,
            ),
            delegate_type: walked.ty,
            is_static: add.is_static,
            has_fire: e.raise.is_some(),
            nullability: walked.nullability,
            custom_attrs: Vec::new(),
            implements,
            unclassified_impls,
        })
    }

    /// A property/event accessor with type parameters has no slot in the
    /// `Property`/`Event` model (accessor typars cannot be expressed). Refuse
    /// loud rather than silently model it as an ordinary accessor.
    fn reject_generic_accessor(
        &self,
        owner_kind: &str,
        owner_name: &str,
        role: &str,
        accessor: &Method,
    ) -> Result<(), ImportError> {
        let cc_generic = match &accessor.signature {
            Ok(sig) => matches!(sig.calling_convention, CallConv::Generic { .. }),
            // An unreadable signature is handled when the accessor is projected;
            // here we only gate on the generic shape we *can* observe.
            Err(_) => false,
        };
        if cc_generic || !accessor.generic_params.is_empty() {
            return Err(ImportError::UnsupportedSignature {
                detail: format!(
                    "{owner_kind} `{owner_name}` {role} accessor `{}` is generic — accessor type \
                     parameters have no slot in the model",
                    accessor.name
                ),
            });
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // F# member filtering (stage 7.5b — `project_fsharp_members`)
    // ------------------------------------------------------------------

    /// Project the members of an F#-kinded type (Module / Union / Record /
    /// Exception). FCS's `FSharpEntity` view hides the compiler-generated tail
    /// the F# compiler emits to satisfy ECMA-335; this mirrors the reference
    /// `project_fsharp_members` so the projection matches.
    ///
    /// - **Methods**: dropped if `[CompilerGenerated]` or named `.ctor`/`.cctor`
    ///   (the auto-generated record/exception constructors carry no attribute).
    ///   Surviving module methods are flagged `is_extension` when they are
    ///   F#-native instance extension members. When `il_heuristic` is set,
    ///   that uses the `<Type>.<Member>` single-dot IL-name mangling;
    ///   otherwise the flag is left for the pickle-driven member-list pass
    ///   (`apply_module_member_projection` in `fsharp_pickle_merge`), which
    ///   reads the authoritative pickle (see the `il_heuristic` parameter).
    /// - **Fields**: `@`-mangled backing fields dropped; on a module, the
    ///   remaining (literal) fields are dropped too (FCS surfaces literals as
    ///   methods, not fields).
    /// - **Properties**: on Record/Exception, a `CompilationMapping(Field)`
    ///   property is re-projected as an init-only field; on a module, a property
    ///   is rebranded as a getter-shaped method under the property's name; all
    ///   other F#-kind properties are dropped.
    /// - **Events**: dropped wholesale.
    ///
    /// `il_heuristic` selects how F#-native instance extension members are
    /// flagged. When `true` — no usable host pickle, or a multi-CCU
    /// `--standalone` image — the projector uses the `<Type>.<Member>` single-
    /// dot IL-name mangling. When `false` the host pickle is authoritative:
    /// the pickle-driven member list (`apply_module_member_projection` in
    /// `fsharp_pickle_merge`) rebuilds each module's members from its vals
    /// afterwards, setting the flag and source name per claimed member; the
    /// projector leaves both alone here (beyond the CLR `[Extension]`
    /// attribute that [`Self::project_method`] already reads).
    fn project_fsharp_members(
        &self,
        kind: EntityKind,
        td: &TypeDef,
        type_context: Option<Nullability>,
        il_heuristic: bool,
    ) -> Result<ProjectedMembers, ImportError> {
        // The reader keeps accessors in `td.methods` alongside ordinary
        // methods, so exclude them here — they are surfaced (or dropped) via the
        // property/event handling below, never as plain methods.
        let accessors = accessor_ids(td);
        let real_method_names = non_witness_method_names(td);
        let in_module = matches!(kind, EntityKind::Module);
        let project_field_flagged = matches!(kind, EntityKind::Record | EntityKind::Exception);
        let mut out = ProjectedMembers::default();

        for (i, m) in td.methods.iter().enumerate() {
            // Each member's projection is isolated: an unreadable one is dropped
            // and recorded (via `push_or_skip_opt`) rather than sinking the whole
            // type. `Ok(None)` marks a member F# deliberately hides (accessor,
            // synthesised, witness twin) — not a failure.
            let projected = (|| -> Result<Option<Member>, ImportError> {
                if accessors.contains(&(i as u32)) {
                    return Ok(None);
                }
                if self.method_is_fsharp_synthesized(m)? {
                    return Ok(None);
                }
                // F# emits a `$W` witness-passing duplicate for each inline member
                // with a statically-resolved (SRTP) type parameter — `ToSingle$W`
                // alongside `ToSingle`. Both carry the *same* `CompilationSourceName`
                // (`single`), so keeping the witness twin would make every such name
                // an overload set and the resolver would defer it. FCS never surfaces
                // them; drop them here (a `$W` with a real sibling) before the model.
                // On the authoritative module path this is now merely a
                // defence in depth: the pickle-driven member list would drop
                // an unclaimed twin anyway (it is not a val) — but it would
                // *record* it as a skip, and the twin is deliberate compiler
                // machinery, not uncertainty worth surfacing.
                if is_fsharp_witness_duplicate(&m.name, &real_method_names) {
                    return Ok(None);
                }
                // NB: generic module methods (generic module-level `let` bindings such
                // as `printfn`/`id`, and F#-native extensions on generic targets) are
                // *kept* — name resolution needs them. Since the pickle-driven
                // member-list cutover both differential sides project generic
                // `let`s; only *generic extension members* and IL-visibly-
                // constrained generic bindings stay elided from the diff
                // (`is_unmirrorable_generic_module_method` in `test_support`,
                // mirroring fcs-dump's rendering limits). The owned model the
                // LSP/sema consume carries the full set.
                let mut projected = self.project_method(m, type_context, false)?;
                // F#-native optional type extensions carry no CLR `[Extension]`; FCS
                // reports `IsExtensionMember` from F# metadata. With no usable host
                // pickle we approximate from the IL-name mangling — `<Type>.<Method>`
                // (one dot) is an instance extension; `<Type>.<Method>.Static` (two
                // dots) is static-only and stays unflagged. The heuristic cannot
                // tell a genuine augmentation from a plain `let` whose
                // `[<CompiledName>]` contains a dot, which is why — when the pickle
                // is authoritative — the pickle-driven member list
                // (`apply_module_member_projection`) sets the flag from the claimed
                // val instead, per declaring module. That covers *generic*
                // extension vals too (`TaskBuilder.MergeSources`, `Lazy`1.Force`),
                // which the IL-only heuristic here still cannot: on the
                // non-authoritative path a generic F#-native extension surfaces
                // flagged by the dot heuristic alone.
                // The name-mangled shape *suggests* an F#-native augmentation —
                // one dot (`<Type>.<Method>`) an instance one, two
                // (`<Type>.<Method>.Static`) a static one — but it cannot prove it:
                // `[<CompiledName("A.B")>]` on an ordinary `let` is legal and
                // produces the same IL name (fsi-verified, and FCS resolves it
                // normally). So this records [`Augmentation::Possible`], which a
                // name-resolution consumer defers on rather than hides (review
                // round 2). The surface extension flag keeps its historical
                // one-dot heuristic: it feeds the overload gate, which is
                // conservative under over-setting.
                if in_module && il_heuristic {
                    match m.name.matches('.').count() {
                        1 => {
                            projected.is_extension_method = true;
                            projected.augmentation = Augmentation::Possible;
                        }
                        2 => projected.augmentation = Augmentation::Possible,
                        _ => {}
                    }
                }
                // For a module member on the authoritative path the host pickle is
                // the source of truth for the F# source name (the matching val's
                // `logical_name` vs `compiled_name`), so drop the attribute-derived
                // one and let the pickle-driven member list set it — same
                // authoritative / fallback split as the extension flag above.
                // Non-module F# kinds (Union/Record/Exception members) keep the
                // attribute heuristic.
                if in_module && !il_heuristic {
                    projected.source_name = None;
                }
                // A generic 0-parameter module method is a *value* (`let empty<'T>
                // = …`) far more often than the vanishingly rare generic
                // 0-parameter unit-function (`let f<'T> () = …`); both share this
                // exact IL shape (a CLR property cannot be generic, so neither is
                // property-rebranded). Presume value here. The host-pickle merge is
                // the source of truth and *overwrites* `is_module_value_binding` for
                // every member it claims (from the val's argument-group count — 0 ⇒
                // value, ≥1 ⇒ function), so this presumption only survives where the
                // pickle did not cover the member: the `il_heuristic` fallback path,
                // or an unmatched member. There, value is the better default — it is
                // what the old display heuristic produced, and dropping it would
                // regress `Array.empty`-shaped values on pickle-less assemblies.
                if in_module
                    && projected.signature.parameters.is_empty()
                    && !projected.generic_parameters.is_empty()
                {
                    projected.is_module_value_binding = true;
                }
                Ok(Some(Member::Method(projected)))
            })();
            out.push_or_skip_opt(&m.name, projected);
        }

        for f in &td.fields {
            let projected = (|| -> Result<Option<Member>, ImportError> {
                if field_is_fsharp_synthesized(f) {
                    return Ok(None);
                }
                // A module class's non-synthesised fields are essentially always
                // `[<Literal>]` constants. FCS **does** bring them into scope — `open M`
                // then bare `TheAnswer` compiles (fsi-verified) — so dropping them left
                // an *invisible* bare name: a consumer could neither resolve it nor even
                // know to be conservative about it (the hole the Slice-A review of
                // `docs/assembly-module-open-plan.md` exposed). Keep the literal; the
                // pickle-driven member list claims it (`rebuild_module_member_list`),
                // and the differential normaliser elides it, mirroring what fcs-dump
                // renders.
                //
                // A *non-literal* module field is compiler scaffolding FCS does not
                // surface (a mutable `let`'s backing store is reached through its
                // property), and is still dropped.
                // A `decimal` literal is the exception to the CLI `Literal` flag: fsc
                // emits `[<Literal>] let D = 1.5M` as a *static init-only* field carrying
                // `[DecimalConstantAttribute]` (the CLI has no decimal constant form).
                // FCS still brings it into scope, so it must survive too (review round 7)
                // — the same invisible-bare-name hazard as the plain literal.
                let decimal_literal = f.is_static
                    && self.has_attribute(
                        &f.attributes,
                        "System.Runtime.CompilerServices",
                        "DecimalConstantAttribute",
                    )?;
                if in_module && !f.is_literal && !decimal_literal {
                    return Ok(None);
                }
                // …but a `[<CompiledName>]`-RENAMED literal must not reach a consumer
                // under its *compiled* name (review round 12). A renamed **method** is
                // safe: `IlxGen.fs` strips `CompiledNameAttribute` and emits
                // `CompilationSourceNameAttribute` carrying the F# name, which the
                // projector reads into `Method::source_name`. The literal-**field** path
                // does neither — it preserves every attribute on the field itself and adds
                // no source name — and `Field` has no `source_name` to hold one.
                //
                // On the authoritative path the pickle's `logical_name` supplies the F#
                // name (and `rebuild_module_member_list` records a skip where it cannot),
                // but on the IL-heuristic path — no usable host pickle, e.g. a multi-CCU
                // `fsc --standalone` image — nothing does. The surviving
                // `CompiledNameAttribute` is exactly the uncertainty marker: it says the
                // F# name is *not* the IL name without saying what it is. Refuse the
                // field, which records it as a skip: a name we cannot spell must be
                // *visibly* absent (so an `open` turns conservative), never present and
                // wrong. Keeping it would resolve `M.Renamed`, which F# does not expose.
                if in_module
                    && il_heuristic
                    && self.has_attribute(
                        &f.attributes,
                        "Microsoft.FSharp.Core",
                        "CompiledNameAttribute",
                    )?
                {
                    return Err(ImportError::UnsupportedEcmaLayout {
                        detail: format!(
                            "`[<CompiledName>]`-renamed `[<Literal>]` field `{}`: its F# name is \
                             recoverable only from the signature pickle, which is not \
                             authoritative here, and `Field` carries no source name",
                            f.name
                        ),
                    });
                }
                Ok(Some(Member::Field(self.project_field(f, type_context)?)))
            })();
            out.push_or_skip_opt(&f.name, projected);
        }

        for p in &td.properties {
            let projected = (|| -> Result<Option<Member>, ImportError> {
                // Same refusal as the IL path (`project_property`): a property
                // carrying an `Other`-semantics accessor is dropped and
                // recorded, even where the F# path would otherwise rebrand or
                // drop it silently — the record is the loud part.
                reject_other_accessors("property", &p.name, &p.other_accessors)?;
                if project_field_flagged && self.is_compilation_mapping_field(&p.attributes)? {
                    return Ok(Some(Member::Field(
                        self.property_as_synthetic_field(td, p)?,
                    )));
                }
                if in_module {
                    // A module value binding compiles to a static property whose
                    // getter holds the value's signature; FCS surfaces it as a
                    // method named after the property. Rebrand the getter; the
                    // setter (mutable `let`) is ignored, as FCS emits no setter
                    // method.
                    let getter = p.getter.ok_or_else(|| ImportError::UnsupportedSignature {
                        detail: format!("F# module property `{}` has no getter", p.name),
                    })?;
                    let mut projected =
                        self.project_method(&td.methods[getter.0 as usize], type_context, false)?;
                    projected.name = p.name.clone();
                    // The setter is consumed only as the `let mutable` signal
                    // and its method is hidden by the accessor-exclusion set —
                    // but a *defective* setter (compilercontrolled/reserved
                    // visibility) must still refuse the member rather than be
                    // silently laundered into a mutability bit: nothing else
                    // would ever inspect it.
                    if let Some(setter) = p.setter {
                        project_member_access(td.methods[setter.0 as usize].accessibility)?;
                    }
                    // This getter-shaped method is really a `let` *value*, not a
                    // function — mark it so, and recover `let mutable` from the
                    // dropped setter. (Distinguishes `let x = …` from `let f () = …`,
                    // which share the 0-parameter-method shape.) The mark is also how
                    // a value's IL-property identity survives the rebrand: `display`
                    // renders `val [mutable] x: T` and `doc_id` keys it `P:`.
                    projected.module_value = Some(ModuleValue {
                        is_mutable: p.setter.is_some(),
                    });
                    // Authoritative source name comes from the pickle (see the
                    // method loop above); `in_module` is necessarily true here.
                    if !il_heuristic {
                        projected.source_name = None;
                    }
                    return Ok(Some(Member::Method(projected)));
                }
                // All other F#-kind properties (Union, or non-`Field` on
                // Record/Exception) are dropped: FCS surfaces none.
                Ok(None)
            })();
            out.push_or_skip_opt(&p.name, projected);
        }
        // Events: dropped wholesale — FCS's F#-entity path surfaces none. The
        // one exception is an event carrying an `Other`-semantics accessor:
        // that is not "F# deliberately hides it" but "the model cannot carry
        // it", and its accessor method is hidden by the exclusion set above —
        // so it must be *recorded* (matching the IL path's
        // [`reject_other_accessors`]) rather than silently vanish.
        for e in &td.events {
            out.push_or_skip_opt(
                &e.name,
                reject_other_accessors("event", &e.name, &e.other_accessors).map(|()| None),
            );
        }
        Ok(out)
    }

    /// A method the F# compiler emitted to satisfy the runtime contract of the
    /// surrounding F#-kinded entity, rather than one the user wrote:
    /// `[CompilerGenerated]` (augmented `Equals`/`GetHashCode`/… and the union's
    /// internal `.ctor(int)`), or a `.ctor`/`.cctor` (the record primary ctor and
    /// the exception ctors, which carry no attribute). FCS hides all of these.
    fn method_is_fsharp_synthesized(&self, m: &Method) -> Result<bool, ImportError> {
        if m.name == ".ctor" || m.name == ".cctor" {
            return Ok(true);
        }
        self.has_attribute(
            &m.attributes,
            "System.Runtime.CompilerServices",
            "CompilerGeneratedAttribute",
        )
    }

    /// Every attribute in `attributes` owned by `namespace`.`name`. An
    /// attribute whose owner the reader cannot name (a generic-attribute
    /// `TypeSpec` parent) cannot be the one searched for, so it is skipped
    /// rather than failing the projection — the idiom every typed-fact
    /// detector shared as a hand-rolled loop before it was hoisted here.
    /// (Detectors that match *several* names in one pass, or that must refuse
    /// unknown attributes, still enumerate by hand.)
    fn find_attributes<'a>(
        &'a self,
        attributes: &'a [RawAttribute],
        namespace: &'a str,
        name: &'a str,
    ) -> impl Iterator<Item = &'a RawAttribute> {
        attributes.iter().filter(move |raw| {
            self.image
                .attribute_owning_type(raw)
                .is_ok_and(|owning| owning.namespace == namespace && owning.name == name)
        })
    }

    /// The first `namespace`.`name` attribute, or `None`. For the (majority)
    /// `AllowMultiple = false` detectors.
    fn find_attribute<'a>(
        &'a self,
        attributes: &'a [RawAttribute],
        namespace: &'a str,
        name: &'a str,
    ) -> Option<&'a RawAttribute> {
        self.find_attributes(attributes, namespace, name).next()
    }

    /// Decode an attribute [`Self::find_attributes`] already matched,
    /// wrapping a decoder failure in the shared `"<Attr> decode: ..."` refusal
    /// every detector emits.
    fn decode_found_attribute(
        &self,
        raw: &RawAttribute,
        widths: &EnumWidths,
        attr: &str,
    ) -> Result<DecodedAttribute, ImportError> {
        self.image
            .decode_attribute(raw, widths)
            .map_err(|e| ImportError::UnsupportedSignature {
                detail: format!("{attr} decode: {e:?}"),
            })
    }

    /// Whether an attribute list carries the named attribute (by owning-type
    /// namespace + name).
    fn has_attribute(
        &self,
        attributes: &[RawAttribute],
        namespace: &str,
        name: &str,
    ) -> Result<bool, ImportError> {
        Ok(self.find_attribute(attributes, namespace, name).is_some())
    }

    /// A default-parameter value carried on an attribute rather than a `Constant`
    /// row: `[DecimalConstantAttribute]` for a `decimal` and
    /// `[DateTimeConstantAttribute]` for a `DateTime` (neither type is a
    /// primitive `ELEMENT_TYPE`, so neither can sit in a `Constant` row). `None`
    /// if no such attribute is present, or if one is present but does not decode
    /// to the expected shape — a default value is cosmetic (hover only), so a
    /// malformed one degrades to "no default" (the caller renders a value-less
    /// optional) rather than sinking the whole assembly, matching `Constant`-blob
    /// handling.
    fn decode_attribute_default(&self, attributes: &[RawAttribute]) -> Option<ConstantValue> {
        let widths = EnumWidths::new();
        for raw in attributes {
            let Ok(owning) = self.image.attribute_owning_type(raw) else {
                continue;
            };
            if owning.namespace != "System.Runtime.CompilerServices" {
                continue;
            }
            match owning.name.as_str() {
                "DecimalConstantAttribute" => {
                    if let Ok(decoded) = self.image.decode_attribute(raw, &widths)
                        && let Some(value) = decimal_from_constant_args(&decoded.fixed_args)
                    {
                        return Some(value);
                    }
                }
                "DateTimeConstantAttribute" => {
                    if let Ok(decoded) = self.image.decode_attribute(raw, &widths)
                        && let [FixedArg::Integral(IntegralParam::Int64(ticks))] =
                            decoded.fixed_args.as_slice()
                    {
                        return Some(ConstantValue::DateTime(*ticks));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Whether a method carries `SetsRequiredMembersAttribute`. FCS recognises
    /// it under *either* well-known namespace —
    /// `System.Diagnostics.CodeAnalysis` (what Roslyn emits) or
    /// `System.Runtime.CompilerServices` (polyfill / older-runtime) — so accept
    /// both.
    fn has_sets_required_members(&self, attributes: &[RawAttribute]) -> Result<bool, ImportError> {
        for raw in attributes {
            let Ok(owning) = self.image.attribute_owning_type(raw) else {
                continue;
            };
            if owning.name == "SetsRequiredMembersAttribute"
                && matches!(
                    owning.namespace.as_str(),
                    "System.Diagnostics.CodeAnalysis" | "System.Runtime.CompilerServices"
                )
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether a property carries `[CompilationMapping(SourceConstructFlags.Field,
    /// …)]` — F# records/exceptions tag their canonical-field-backed properties
    /// with it, and FCS surfaces those as `FSharpField`s, not properties.
    fn is_compilation_mapping_field(
        &self,
        attributes: &[RawAttribute],
    ) -> Result<bool, ImportError> {
        // `SourceConstructFlags.Field` = 4 in the low-5-bit `KindMask`.
        Ok(self
            .compilation_mapping_flags(attributes)?
            .is_some_and(|flags| flags & 31 == 4))
    }

    /// Re-project an F# canonical-field-backed property (a record/exception
    /// logical field) as an init-only [`Field`], mirroring FCS's `FSharpField`
    /// rendering. A logical field has no index dimension, so an indexer carrying
    /// the field flag is nonsensical and refused; byref/modreq property types are
    /// refused as elsewhere. `is_init_only` is `false` for a `mutable` field (the
    /// compiler emits a setter), matching `FSharpField.IsMutable`.
    fn property_as_synthetic_field(
        &self,
        td: &TypeDef,
        p: &RawProperty,
    ) -> Result<Field, ImportError> {
        let sig = p
            .signature
            .as_ref()
            .map_err(|e| sig_error(e, "F# field-flagged property type"))?;
        // F# cannot declare a byref record/exception field, and `project_type_ref`
        // below *accepts* byrefs — so without this the projector would synthesise
        // one from malformed metadata rather than fail loud. The guard reads the
        // head of the position directly: no modifier can be in front of it to hide
        // the byref (see `project_event`). `project_type_ref` refuses a `volatile`
        // marker, or a read-only marker with no byref under it, on its own; a
        // logical F# field carries neither.
        if matches!(sig.ty, TypeSig::ByRef(_)) {
            return Err(ImportError::UnsupportedSignature {
                detail: format!("F# field-flagged property `{}` returns byref", p.name),
            });
        }
        let ty = self.project_type_ref(sig)?;

        let getter = p.getter.map(|id| &td.methods[id.0 as usize]);
        let setter = p.setter.map(|id| &td.methods[id.0 as usize]);
        // A logical field has no index dimension: the getter takes no argument
        // (a setter takes only the value). Anything else is an indexer.
        let index_len = match (getter, setter) {
            (Some(g), _) => accessor_param_count(g, &p.name, "getter")?,
            (None, Some(s)) => accessor_param_count(s, &p.name, "setter")?.saturating_sub(1),
            (None, None) => {
                return Err(ImportError::UnsupportedSignature {
                    detail: format!("F# field-flagged property `{}` has no accessors", p.name),
                });
            }
        };
        if index_len != 0 {
            return Err(ImportError::UnsupportedSignature {
                detail: format!("F# field-flagged property `{}` is an indexer", p.name),
            });
        }

        let access = match (getter, setter) {
            (Some(g), Some(s)) => max_access(
                project_member_access(g.accessibility)?,
                project_member_access(s.accessibility)?,
            ),
            (Some(g), None) => project_member_access(g.accessibility)?,
            (None, Some(s)) => project_member_access(s.accessibility)?,
            (None, None) => unreachable!("no-accessor case returned above"),
        };
        // No `PropertySig` `HASTHIS` is retained; the accessor's static-ness is
        // the equivalent signal for valid metadata.
        let is_static = match (getter, setter) {
            (Some(g), _) => g.is_static,
            (None, Some(s)) => s.is_static,
            (None, None) => unreachable!("no-accessor case returned above"),
        };

        Ok(Field {
            name: p.name.clone(),
            access,
            ty,
            is_static,
            is_init_only: p.setter.is_none(),
            // An F# record/exception field is never `volatile` — the marker is
            // a C#-only construct, and `project_type_ref` below would refuse a
            // `modreq(IsVolatile)` on this property's type anyway.
            is_volatile: false,
            // A record field re-projected from its `CompilationMapping(Field)`
            // property is not a literal.
            is_literal: false,
            is_required: false,
            compiler_feature_required: Vec::new(),
            nullability: Nullability::Oblivious,
            custom_attrs: Vec::new(),
        })
    }
}

/// Refuse a property/event carrying `Other`-semantics accessor rows: the model
/// has no slot for a non-standard accessor, and surfacing the member while
/// silently ignoring one of its accessors would misrepresent it. The error is
/// recorded by the member-projection loops (`push_or_skip`), so the cost is
/// this one member — its accessor methods stay hidden with it (see
/// [`accessor_ids`]) — never the assembly.
fn reject_other_accessors(
    owner_kind: &str,
    owner_name: &str,
    other: &[Option<crate::reader::MethodId>],
) -> Result<(), ImportError> {
    if other.is_empty() {
        return Ok(());
    }
    Err(ImportError::UnsupportedEcmaLayout {
        detail: format!(
            "non-standard (Other) method-semantics accessor on {owner_kind} `{owner_name}`"
        ),
    })
}

/// The set of method indices (`MethodId` offsets into `td.methods`) that serve
/// as property/event accessors (getter/setter/add/remove/raise). The reader
/// keeps these in `td.methods`; both member paths exclude them so they surface
/// only through the property/event projection.
fn accessor_ids(td: &TypeDef) -> HashSet<u32> {
    let mut accessors: HashSet<u32> = HashSet::new();
    for p in &td.properties {
        accessors.extend(p.getter.iter().map(|m| m.0));
        accessors.extend(p.setter.iter().map(|m| m.0));
        // `Other`-semantics accessors: their owner is dropped (and recorded)
        // by the property/event projection, and the accessor methods stay
        // hidden with it — an accessor surfaces only through its owner. A
        // `None` slot (malformed RID, no local method to hide) contributes
        // nothing here but still forces the owner's drop.
        accessors.extend(p.other_accessors.iter().flatten().map(|m| m.0));
    }
    for e in &td.events {
        accessors.extend(e.add.iter().map(|m| m.0));
        accessors.extend(e.remove.iter().map(|m| m.0));
        accessors.extend(e.raise.iter().map(|m| m.0));
        accessors.extend(e.other_accessors.iter().flatten().map(|m| m.0));
    }
    accessors
}

/// Whether an IL field is a compiler-emitted backing field the F# member path
/// must drop *at this projection layer*. This keys only on the `@` substring
/// (`X@`, `init@`), which is reserved in F# identifiers and so unambiguous.
///
/// The other compiler-generated fields the F# compiler emits — a union's `_tag`
/// discriminator and `_unique_<Case>` singletons, `<…>k__BackingField` — are
/// **not** dropped here. They are emitted with `Assembly`/`Private` visibility
/// and are dropped by the downstream *accessibility* filter that consumers apply
/// to the projected tree (FCS's `AccessibleFromSomeFSharpCode`), not by the
/// `EcmaView` projection. `enumerate_type_defs` deliberately keeps them so the
/// projection matches the fcs-dump ground truth — the differential proves a
/// union (`MiniLibFs.Choice`) surfaces `_tag`/`_unique_*` identically on both
/// sides. (Mutable record fields are the case the `@` filter exists for: their
/// `X@` backing slot is emitted `Public`, so the accessibility filter would not
/// catch it.)
fn field_is_fsharp_synthesized(field: &RawField) -> bool {
    field.name.contains('@')
}

/// Union a property's/event's accessor-level [`InterfaceMemberImpl`]s into
/// the member-level list, preserving first-seen order. `MethodSemantics`-
/// resolved entries deduplicate — a get+set interface property satisfied by
/// both accessors contributes one row per accessor naming the *same* owner,
/// and the model's nominal granularity (interface + kind + owner name) makes
/// identical resolved entries one member — while accessors satisfying
/// *different* interfaces stay distinct. [`ImplementedMember::Unresolved`]
/// entries are **never** collapsed: name equality cannot prove two external
/// declarations are one member (interfaces may overload — a getter and a
/// setter can implement two distinct same-named overloads, one `MethodImpl`
/// row each), and §II.22.27 forbids duplicate `Class`+`MethodDeclaration`
/// rows, so identical unresolved entries always denote *distinct*
/// declarations. (The unclassified channel is unioned without any dedup for
/// the same reason.)
/// The distinct accessor *methods* among a member's role slots, in role
/// order, compared by identity (the slots all reference the owning type's
/// method run, so one `MethodDef` claimed by several `MethodSemantics` roles
/// — crafted IL — is one reference). Each `MethodImpl` row lives on the
/// method, not the role, so each distinct method contributes exactly once to
/// the accessor unions.
fn distinct_accessors<const N: usize>(slots: [Option<&Method>; N]) -> Vec<&Method> {
    let mut out: Vec<&Method> = Vec::new();
    for acc in slots.into_iter().flatten() {
        if !out.iter().any(|m| std::ptr::eq(*m, acc)) {
            out.push(acc);
        }
    }
    out
}

fn union_accessor_impls(
    impls: impl Iterator<Item = InterfaceMemberImpl>,
) -> Vec<InterfaceMemberImpl> {
    let mut out: Vec<InterfaceMemberImpl> = Vec::new();
    for ei in impls {
        if matches!(ei.member, ImplementedMember::Unresolved(_)) || !out.contains(&ei) {
            out.push(ei);
        }
    }
    out
}

/// The parameter count of an accessor method's signature, propagating an
/// unreadable signature as an error (a field-flagged property with an
/// undecodable accessor is refused rather than mis-sized).
fn accessor_param_count(m: &Method, property_name: &str, role: &str) -> Result<usize, ImportError> {
    match &m.signature {
        Ok(sig) => Ok(sig.parameters.len()),
        Err(e) => Err(sig_error(
            e,
            &format!("F# field-flagged property `{property_name}` {role}"),
        )),
    }
}

/// Project an indexer's index dimension (the accessor parameters that carry it)
/// into the `Property::parameters` shape — name, type, and nullability per
/// parameter. A byref (`ref`) index parameter has no slot in the model — an
/// [`IndexParameter`] type is value-typed — so it is refused loud rather than
/// projected as a value-typed index (which would silently change the
/// signature). Matches the reference's byref-index refusal.
fn index_params_to_index_params(
    index_params: &[Parameter],
    property_name: &str,
) -> Result<Vec<IndexParameter>, ImportError> {
    index_params
        .iter()
        .map(|param| {
            if param.is_byref {
                return Err(ImportError::UnsupportedSignature {
                    detail: format!("property `{property_name}` has a byref index parameter"),
                });
            }
            Ok(IndexParameter {
                name: param.name.clone(),
                ty: NullableType {
                    ty: param.ty.clone(),
                    nullability: param.nullability,
                },
                is_param_array: param.is_param_array,
            })
        })
        .collect()
}

/// Map a member's folded `MemberAccess` to the model's `Access`. The reader
/// stores the two §II.23.1.10 values with no variant (privatescope / reserved)
/// as a per-member [`AccessDefect`]; that surfaces here as an
/// `UnsupportedEcmaLayout`, which the member-projection loops record on
/// `Entity::skipped_members` — dropping the one member, not the assembly.
fn project_member_access(a: Result<MemberAccess, AccessDefect>) -> Result<Access, ImportError> {
    match a {
        Ok(MemberAccess::Public) => Ok(Access::Public),
        Ok(MemberAccess::Private) => Ok(Access::Private),
        Ok(MemberAccess::Family) => Ok(Access::Protected),
        Ok(MemberAccess::Assembly) => Ok(Access::Internal),
        Ok(MemberAccess::FamAndAssem) => Ok(Access::ProtectedAndInternal),
        Ok(MemberAccess::FamOrAssem) => Ok(Access::ProtectedOrInternal),
        Err(defect) => Err(ImportError::UnsupportedEcmaLayout {
            detail: defect.to_string(),
        }),
    }
}

/// Join (least upper bound) on the C# accessibility lattice — the
/// least-restrictive scope containing both inputs. The lattice is *partial*:
/// `Protected` and `Internal` are incomparable, and their join is their union
/// (`ProtectedOrInternal`), not whichever was passed first. Used to fold a
/// property's getter/setter (or an event's add/remove) visibility.
fn max_access(a: Access, b: Access) -> Access {
    use Access::{Internal, Private, Protected, ProtectedAndInternal, ProtectedOrInternal, Public};
    match (a, b) {
        (Public, _) | (_, Public) => Public,
        (ProtectedOrInternal, _) | (_, ProtectedOrInternal) => ProtectedOrInternal,
        (Protected, Internal) | (Internal, Protected) => ProtectedOrInternal,
        (Protected, _) | (_, Protected) => Protected,
        (Internal, _) | (_, Internal) => Internal,
        (ProtectedAndInternal, _) | (_, ProtectedAndInternal) => ProtectedAndInternal,
        (Private, Private) => Private,
    }
}

/// The custom modifiers [`Ecma335Assembly::classify_mods`] recognised on one
/// type position. Both are `modreq`s (a required modifier a reader *must*
/// understand), and both are position-sensitive — which is why they are reported
/// to the caller rather than acted on where the run is read.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Modifiers {
    /// `modreq(System.Runtime.InteropServices.InAttribute)` — the byref it
    /// precedes is read-only (C# `in` / `ref readonly`, F# `inref<'T>`).
    /// Meaningful only directly over an `ELEMENT_TYPE_BYREF`.
    readonly_ref: bool,
    /// `modreq(System.Runtime.CompilerServices.IsVolatile)` — a C# `volatile`
    /// field. Meaningful only on a field type.
    volatile: bool,
}

impl Modifiers {
    /// Refuse a recognised-but-misplaced modifier: a position that can carry
    /// *neither* marker (a generic argument, an array element, a pointee, …)
    /// calls this so an ABI-significant `modreq` is never silently dropped just
    /// because it turned up somewhere no compiler emits it.
    fn reject_at(self, position: &str) -> Result<(), ImportError> {
        let marker = if self.readonly_ref {
            "read-only-ref modifier (`modreq(InAttribute)`)"
        } else if self.volatile {
            "`volatile` modifier (`modreq(IsVolatile)`)"
        } else {
            return Ok(());
        };
        Err(ImportError::UnsupportedSignature {
            detail: format!("{marker} at {position}"),
        })
    }
}

/// The source of nullable bytes for a position's pre-order walk: one broadcast
/// byte (`NullableAttribute(byte)`, the length-1 vector form, or a scope-context
/// default) or one byte per annotable node (`NullableAttribute(byte[])`).
enum NullableByteSource {
    Scalar(Nullability),
    Vector(Vec<Nullability>),
}

impl NullableByteSource {
    /// The byte at walk index `idx`: `Scalar` broadcasts; `Vector` indexes (a
    /// `None` is an exhaustion the walker reports loud).
    fn nth(&self, idx: usize) -> Option<Nullability> {
        match self {
            NullableByteSource::Scalar(b) => Some(*b),
            NullableByteSource::Vector(v) => v.get(idx).copied(),
        }
    }
}

/// Map a `NullableAttribute`/`NullableContextAttribute` byte to [`Nullability`],
/// refusing any value outside the three documented states (Roslyn emits only
/// `0`/`1`/`2`).
fn nullability_from_byte(b: u8) -> Result<Nullability, ImportError> {
    match b {
        0 => Ok(Nullability::Oblivious),
        1 => Ok(Nullability::NotAnnotated),
        2 => Ok(Nullability::Annotated),
        other => Err(ImportError::UnsupportedSignature {
            detail: format!("NullableAttribute/NullableContextAttribute byte {other} is not 0/1/2"),
        }),
    }
}

/// Consume the next nullable byte and advance `idx`. An exhausted
/// [`NullableByteSource::Vector`] is a structural error — Roslyn emits one byte
/// per annotable position in the pre-order walk.
fn consume_nullable_byte(
    src: &NullableByteSource,
    idx: &mut usize,
) -> Result<Nullability, ImportError> {
    let n = src
        .nth(*idx)
        .ok_or_else(|| ImportError::UnsupportedSignature {
            detail: format!("NullableAttribute byte[] exhausted at walk index {}", *idx),
        })?;
    *idx += 1;
    Ok(n)
}

/// The F#-only entity kinds (those routed through `project_fsharp_members`
/// rather than `project_il_members`). Their member
/// projection is staged separately (7.5b); 7.5a leaves their members empty.
/// Whether any `[CompilerFeatureRequired]` gate names `"RequiredMembers"` — the
/// signal Roslyn pairs with a synthetic `[Obsolete]` on a required-members
/// constructor, which [`Ecma335Assembly::project_method`] suppresses.
fn has_required_members_feature_gate(gates: &[CompilerFeatureRequired]) -> bool {
    gates.iter().any(|g| g.feature == "RequiredMembers")
}

fn is_fsharp_kind(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Module | EntityKind::Union | EntityKind::Record | EntityKind::Exception
    )
}

/// The ECMA-335 kind discriminant from the type flags + `extends` chain.
fn project_kind(td: &TypeDef, base: Option<&TypeRef>) -> EntityKind {
    if td.is_interface {
        return EntityKind::Interface;
    }
    // When importing the BCL itself, the well-known base classes *are* the base
    // classes; their own `extends` would misclassify them. They are plain
    // classes — short-circuit.
    if is_well_known_base(td) {
        return EntityKind::Class;
    }
    match base {
        Some(TypeRef::Named {
            namespace, name, ..
        }) if namespace.as_slice() == ["System"] => match name.as_str() {
            "Enum" => EntityKind::Enum,
            "ValueType" => EntityKind::Struct,
            "MulticastDelegate" | "Delegate" => EntityKind::Delegate,
            _ => EntityKind::Class,
        },
        _ => EntityKind::Class,
    }
}

/// Whether this `TypeDef` is one of the BCL base classes used for kind
/// discrimination (so its own `extends` chain is not consulted).
fn is_well_known_base(td: &TypeDef) -> bool {
    td.name.namespace == "System"
        && matches!(
            td.name.name.as_str(),
            "Enum" | "ValueType" | "Delegate" | "MulticastDelegate"
        )
}

/// Whether a type's direct base is `System.ValueType` — the structural signal
/// behind [`Entity::is_struct`] (set on every CLR struct, including F#
/// `[<Struct>]` records whose `EntityKind` would otherwise hide it). Deliberately
/// the *direct* base, not transitive: enums extend `System.Enum` (which extends
/// `ValueType`) but are already distinguished by `EntityKind::Enum`.
fn extends_value_type(base: Option<&TypeRef>) -> bool {
    matches!(
        base,
        Some(TypeRef::Named { namespace, name, .. })
            if namespace.as_slice() == ["System"] && name == "ValueType"
    )
}

/// Map the reader's visibility fold onto the `Entity`-level [`Access`].
fn project_access(a: Accessibility) -> Access {
    match a {
        Accessibility::Public => Access::Public,
        Accessibility::Private => Access::Private,
        Accessibility::Family => Access::Protected,
        Accessibility::Assembly => Access::Internal,
        Accessibility::FamAndAssem => Access::ProtectedAndInternal,
        Accessibility::FamOrAssem => Access::ProtectedOrInternal,
    }
}

/// Map a reader-decoded [`Constant`] onto the projected [`ConstantValue`]. The
/// reader keeps `char`/string values as raw UTF-16 code units (losslessly,
/// surrogates and all); the presentation model renders them down to a Rust
/// `char`/`String`, substituting U+FFFD for any code unit that is not a scalar
/// value — the only displayable rendering of an unpaired surrogate.
fn project_constant(c: &Constant) -> ConstantValue {
    match c {
        Constant::Bool(b) => ConstantValue::Bool(*b),
        Constant::Char(u) => ConstantValue::Char(
            char::from_u32(u32::from(*u)).unwrap_or(char::REPLACEMENT_CHARACTER),
        ),
        Constant::Int(i) => ConstantValue::Int(*i),
        Constant::UInt(u) => ConstantValue::UInt(*u),
        Constant::F32(bits) => ConstantValue::F32(*bits),
        Constant::F64(bits) => ConstantValue::F64(*bits),
        Constant::Str(units) => ConstantValue::String(String::from_utf16_lossy(units)),
        Constant::Null => ConstantValue::Null,
    }
}

/// Reconstruct a `ConstantValue::Decimal` from a `DecimalConstantAttribute`'s
/// five constructor arguments `(byte scale, byte sign, word hi, word mid, word
/// low)`. The compiler emits one of two overloads — `uint` words (Roslyn) or
/// `int` words — so each of `hi`/`mid`/`low` is accepted as `UInt32` *or* `Int32`
/// (the 32-bit pattern is identical either way). `None` if the argument shape
/// does not match, leaving the caller to render a value-less optional.
fn decimal_from_constant_args(args: &[FixedArg]) -> Option<ConstantValue> {
    let word = |a: &FixedArg| match a {
        FixedArg::Integral(IntegralParam::UInt32(v)) => Some(*v),
        FixedArg::Integral(IntegralParam::Int32(v)) => Some(*v as u32),
        _ => None,
    };
    let [
        FixedArg::Integral(IntegralParam::UInt8(scale)),
        FixedArg::Integral(IntegralParam::UInt8(sign)),
        hi,
        mid,
        low,
    ] = args
    else {
        return None;
    };
    let mantissa =
        (u128::from(word(hi)?) << 64) | (u128::from(word(mid)?) << 32) | u128::from(word(low)?);
    Some(ConstantValue::Decimal {
        negative: *sign != 0,
        scale: *scale,
        mantissa,
    })
}

/// Map the reader's primitive onto the `Entity`-level [`Primitive`]. (`Void` is
/// not a [`TypeSig`] primitive — it appears only at a method's return boundary.)
fn map_primitive(p: SigPrimitive) -> Primitive {
    match p {
        SigPrimitive::Boolean => Primitive::Bool,
        SigPrimitive::Char => Primitive::Char,
        SigPrimitive::Int8 => Primitive::I1,
        SigPrimitive::UInt8 => Primitive::U1,
        SigPrimitive::Int16 => Primitive::I2,
        SigPrimitive::UInt16 => Primitive::U2,
        SigPrimitive::Int32 => Primitive::I4,
        SigPrimitive::UInt32 => Primitive::U4,
        SigPrimitive::Int64 => Primitive::I8,
        SigPrimitive::UInt64 => Primitive::U8,
        SigPrimitive::Float32 => Primitive::R4,
        SigPrimitive::Float64 => Primitive::R8,
        SigPrimitive::IntPtr => Primitive::IntPtr,
        SigPrimitive::UIntPtr => Primitive::UIntPtr,
        SigPrimitive::String => Primitive::String,
        SigPrimitive::Object => Primitive::Object,
    }
}

/// The names of a type's methods that are *not* `$W` witnesses — the candidate
/// "real" members a `$W` duplicate could shadow. Used by
/// [`is_fsharp_witness_duplicate`].
fn non_witness_method_names(td: &TypeDef) -> HashSet<&str> {
    td.methods
        .iter()
        .map(|m| m.name.as_str())
        .filter(|n| !n.ends_with("$W"))
        .collect()
}

/// Whether `name` is an F# `$W` witness-passing duplicate (the inline-SRTP
/// shadow of a real member). F# emits `X$W` *alongside* the real `X`, so the
/// `$W` suffix alone is not decisive — a lone `…$W` (hand-written IL, or a
/// deliberate `[<CompiledName("Foo$W")>]`) is a genuine member with no twin and
/// must be kept. We treat it as a witness only when its stripped name has a
/// sibling among the type's real methods; such a duplicate is hidden by FCS and
/// collides on the real member's `CompilationSourceName`, so the projector drops
/// it.
///
/// Residual heuristic limit (pickle-scoped): this cannot distinguish a genuine
/// witness `X$W` from a *deliberate* `[<CompiledName("X$W")>]` member declared
/// next to an unrelated `X` — the `$W`-suffix-plus-sibling shape is the only IL
/// signal, and F#'s witness convention *is* that shape (`ExtraWitnessMethodName
/// nm = nm + "$W"`). The witness method is pure codegen: it is *not* a separate
/// val in the F# signature pickle, so FCS — which builds its member list from the
/// pickle — never sees it (it derives the witness IL name on demand only when it
/// needs to *call* the inline member). The robust fix is therefore to drive the
/// F# member list from the pickle's vals rather than filter IL, which would also
/// fix source names, module suffixes, and extension flags in one move; deferred
/// with the rest of the pickle work. The collision this heuristic mishandles
/// needs source deliberately mimicking the compiler's witness naming, which does
/// not occur in practice.
fn is_fsharp_witness_duplicate(name: &str, real_method_names: &HashSet<&str>) -> bool {
    name.strip_suffix("$W")
        .is_some_and(|base| real_method_names.contains(base))
}

/// An `EnumWidths` covering the one enum the kind decode reads.
fn source_construct_flags_widths() -> EnumWidths {
    let mut widths = EnumWidths::new();
    widths.insert(
        TypeName {
            namespace: "Microsoft.FSharp.Core".to_string(),
            name: "SourceConstructFlags".to_string(),
        },
        IntegralWidth::Int32,
    );
    widths
}

/// An `EnumWidths` covering the `CompilationRepresentationFlags` enum the
/// module-suffix decode reads (int32-backed, like `SourceConstructFlags`).
fn compilation_representation_flags_widths() -> EnumWidths {
    let mut widths = EnumWidths::new();
    widths.insert(
        TypeName {
            namespace: "Microsoft.FSharp.Core".to_string(),
            name: "CompilationRepresentationFlags".to_string(),
        },
        IntegralWidth::Int32,
    );
    widths
}

/// A `TypeParameter` index must fit `u16` (the `TypeRef::Var` width); a real
/// typar index always does.
fn typar_index(n: u32) -> Result<u16, ImportError> {
    u16::try_from(n).map_err(|_| ImportError::UnsupportedSignature {
        detail: format!("generic parameter index {n} exceeds u16"),
    })
}

/// A stored signature error becomes an `ImportError` at the position it sits in.
fn sig_error(e: &SigError, position: &str) -> ImportError {
    ImportError::UnsupportedSignature {
        detail: format!("{position}: {e:?}"),
    }
}

/// A `TypeDef`'s dotted fully-qualified name (`System.Collections.Generic.List`1`),
/// or the bare name when it sits in the global namespace. Used only to *label* a
/// dropped whole-type in a [`SkippedProjectionItem`] record; the arity suffix is kept
/// verbatim (this is a diagnostic string, not a resolution key).
fn qualified_type_name(name: &TypeName) -> String {
    if name.namespace.is_empty() {
        name.name.clone()
    } else {
        format!("{}.{}", name.namespace, name.name)
    }
}

/// The label for a **dropped nested type** — its own name qualified by the
/// **top-level** enclosing namespace (`top_namespace`), since a nested `TypeDef`
/// carries an empty `TypeNamespace` of its own. This ensures
/// [`SkippedProjectionItem::enclosing_namespace`](crate::SkippedProjectionItem::enclosing_namespace)
/// reports `top_namespace`, not the root, so the OV-6 extension gate attributes
/// the uncertainty to the right namespace. A nested type carrying its *own*
/// namespace (unusual) keeps it.
fn nested_drop_name(top_namespace: &str, name: &TypeName) -> String {
    if !name.namespace.is_empty() {
        format!("{}.{}", name.namespace, name.name)
    } else if top_namespace.is_empty() {
        name.name.clone()
    } else {
        format!("{}.{}", top_namespace, name.name)
    }
}

/// Narrow a decoded `ELEMENT_TYPE_ARRAY` rank (a compressed `u32`) to the
/// model's `u8`. A rank beyond 255 is not real metadata; refuse rather than
/// truncate.
fn array_rank(rank: u32) -> Result<u8, ImportError> {
    u8::try_from(rank).map_err(|_| ImportError::UnsupportedSignature {
        detail: format!("array rank {rank} exceeds 255"),
    })
}

/// Types the projector does not surface: the `<Module>` pseudo-type and the F#
/// `<StartupCode$…>` static-initialiser helpers (FCS hides both).
fn is_skipped_type(td: &TypeDef) -> bool {
    (td.name.namespace.is_empty() && td.name.name == "<Module>")
        || td.name.namespace.starts_with("<StartupCode$")
}

/// Whether a nested type stays attached to its parent. F# unions emit a
/// synthetic `Tags` static class (the per-case integer tag constants) that FCS
/// does not surface; drop it so the projection agrees.
fn keep_nested_type(parent_kind: EntityKind, child: &Entity) -> bool {
    !(parent_kind == EntityKind::Union && child.name == "Tags")
}

/// Split a metadata namespace string into its dotted components; an empty
/// namespace is the global namespace (no components).
fn split_namespace(ns: &str) -> Vec<String> {
    if ns.is_empty() {
        Vec::new()
    } else {
        ns.split('.').map(str::to_string).collect()
    }
}

/// Well-known names of the runtime's core library — the assembly that provides
/// the token-free intrinsics (`System.Object`, `System.TypedReference`, …).
/// Used to locate the `AssemblyRef` a `typedref` resolves against and to
/// recognise the core library reading its own definitions. The impl corlibs
/// that *define* those types (`System.Private.CoreLib`, `mscorlib`) precede the
/// reference-assembly facades that forward them (`System.Runtime`,
/// `netstandard`), so the rare image referencing both is attributed to the
/// definer.
const CORE_LIBRARY_NAMES: [&str; 4] = [
    "System.Private.CoreLib",
    "mscorlib",
    "System.Runtime",
    "netstandard",
];

/// Drop the ECMA-335 backtick arity suffix from a metadata name (`List`1` →
/// `List`). Only strips a backtick followed by digits; a name ending in a
/// backtick-then-non-digit is left unchanged.
pub(crate) fn strip_arity(name: &str) -> &str {
    if let Some(i) = name.rfind('`')
        && !name[i + 1..].is_empty()
        && name[i + 1..].bytes().all(|b| b.is_ascii_digit())
    {
        return &name[..i];
    }
    name
}

/// The ECMA-335 backtick arity suffix of a metadata name, as a number
/// (`List`1` → 1, `Enumerator` → 0). For a nested type this is the count of
/// generic parameters the segment *introduces* — a delta, not the cumulative
/// total including enclosers (`Dictionary`2/Enumerator` → `2` then `0`). The
/// counterpart to [`strip_arity`], which drops the suffix; this keeps it.
fn arity_suffix(name: &str) -> usize {
    if let Some(i) = name.rfind('`')
        && !name[i + 1..].is_empty()
        && name[i + 1..].bytes().all(|b| b.is_ascii_digit())
    {
        return name[i + 1..].parse().unwrap_or(0);
    }
    0
}

/// Project the reader's raw assembly identity onto the `Entity`-level model,
/// deriving the `PublicKeyToken` (the model carries the token, not the key).
fn project_identity(raw: &RawAssemblyIdentity) -> AssemblyIdentity {
    AssemblyIdentity {
        name: raw.name.clone(),
        version: Version {
            major: raw.version.major,
            minor: raw.version.minor,
            build: raw.version.build,
            revision: raw.version.revision,
        },
        public_key_token: public_key_token(raw),
    }
}

/// The 8-byte `PublicKeyToken` for an identity, or `None` when unsigned.
///
/// The blob (`Assembly.PublicKey` / `AssemblyRef.PublicKeyOrToken`) is either
/// the full strong-name key (the `afPublicKey` flag set) or an already-computed
/// 8-byte token. A full key is hashed per ECMA-335 I.6.3; an 8-byte token is
/// used verbatim; anything else (including an empty/absent blob) is `None`.
fn public_key_token(raw: &RawAssemblyIdentity) -> Option<[u8; 8]> {
    if raw.public_key.is_empty() {
        return None;
    }
    if raw.has_full_key {
        Some(public_key_token_from_key(&raw.public_key))
    } else {
        <[u8; 8]>::try_from(raw.public_key.as_slice()).ok()
    }
}

/// Derive the 8-byte public-key token from a strong name's full public key
/// (ECMA-335 I.6.3): SHA-1 the key, take the last 8 bytes, reverse them.
fn public_key_token_from_key(key: &[u8]) -> [u8; 8] {
    use sha1::{Digest, Sha1};
    let digest = Sha1::digest(key);
    let mut token = [0u8; 8];
    for (i, b) in digest[digest.len() - 8..].iter().rev().enumerate() {
        token[i] = *b;
    }
    token
}

#[cfg(test)]
mod tests {
    //! `Ecma335Assembly` is validated against the independent fcs-dump ground
    //! truth by the `projector_*` / `assembly_diff` integration tests. These two
    //! unit tests pin refuse-loud paths the fixture corpus cannot reach (no real
    //! compiler emits them), driven directly against synthetic input.

    use super::Ecma335Assembly;
    use crate::ImportError;
    use crate::model::{
        FsharpOverlayKind, Nullability, ParamDefault, Parameter, Primitive, TypeRef,
    };
    use crate::reader::all_dlls;

    #[test]
    fn nested_drop_name_qualifies_under_the_top_level_namespace() {
        use crate::reader::TypeName;
        // A nested `TypeDef` carries an empty namespace; its drop is labelled under
        // the top-level enclosing namespace so `enclosing_namespace` reports it.
        let nested = TypeName {
            namespace: String::new(),
            name: "Inner".to_string(),
        };
        assert_eq!(super::nested_drop_name("Demo", &nested), "Demo.Inner");
        assert_eq!(
            crate::SkippedProjectionItem {
                name: super::nested_drop_name("Demo", &nested),
                reason: String::new(),
            }
            .enclosing_namespace(),
            vec!["Demo".to_string()],
            "the drop is attributed to `Demo`, not the root"
        );
        // A root-namespace top-level → the nested drop stays in root.
        assert_eq!(super::nested_drop_name("", &nested), "Inner");
        // A nested type that unusually carries its own namespace keeps it.
        let own = TypeName {
            namespace: "Other".to_string(),
            name: "T".to_string(),
        };
        assert_eq!(super::nested_drop_name("Demo", &own), "Other.T");
    }

    #[test]
    fn host_signature_decode_failure_records_all_fsharp_overlays() {
        let skip = super::skipped_host_signature_overlays(
            "FSharpSignatureData.MiniLibFs".to_string(),
            ImportError::UnsupportedPickleExpr {
                context: "u_expr (test: non-const attribute argument)",
                tag: 1,
            },
        );

        assert_eq!(skip.resource_name, "FSharpSignatureData.MiniLibFs");
        assert_eq!(
            skip.overlays,
            vec![
                FsharpOverlayKind::SourceName,
                FsharpOverlayKind::Extension,
                FsharpOverlayKind::Measure,
                FsharpOverlayKind::AbbreviationMarkers,
                FsharpOverlayKind::UnionCases,
            ]
        );
        assert!(
            skip.reason.contains("unsupported pickle expression tag 1"),
            "reason should surface the decode error, got: {}",
            skip.reason
        );
    }

    #[test]
    fn corruption_shaped_host_signature_decode_failure_is_recorded_not_reclassified() {
        let skip = super::skipped_host_signature_overlays(
            "FSharpSignatureData.MiniLibFs".to_string(),
            ImportError::MalformedPickleLazyFrame {
                expected: 5,
                actual: 3,
            },
        );

        assert_eq!(skip.resource_name, "FSharpSignatureData.MiniLibFs");
        assert_eq!(
            skip.overlays,
            vec![
                FsharpOverlayKind::SourceName,
                FsharpOverlayKind::Extension,
                FsharpOverlayKind::Measure,
                FsharpOverlayKind::AbbreviationMarkers,
                FsharpOverlayKind::UnionCases,
            ]
        );
        assert!(
            skip.reason.contains("u_lazy frame mismatch"),
            "reason should surface the decode error, got: {}",
            skip.reason
        );
    }

    /// Review round 12: a `[<CompiledName>]`-renamed `[<Literal>]` must not reach a
    /// consumer under its **compiled** name on the IL-heuristic path.
    ///
    /// A renamed *method* is safe: fsc strips `CompiledNameAttribute` and emits
    /// `CompilationSourceNameAttribute` carrying the F# name, which the projector reads
    /// into `Method::source_name`. The literal-*field* path in `IlxGen.fs` does neither —
    /// it preserves the attributes on the field itself and adds no source name — and
    /// `Field` has no `source_name` to carry one anyway. So on the authoritative path the
    /// pickle's `logical_name` recovers the F# name (and `rebuild_module_member_list`
    /// records a skip when it cannot), but on the **IL-heuristic** path (no usable host
    /// pickle — a multi-CCU `fsc --standalone` image) nothing does.
    ///
    /// The surviving `CompiledNameAttribute` is the uncertainty marker: it says "this
    /// field's F# name is *not* its IL name" without saying what it is. Skip the field
    /// rather than surface `Values.RenamedLit`, a name F# never exposes. The plain
    /// literals beside it are unaffected.
    #[test]
    fn il_heuristic_path_skips_a_compiled_name_renamed_literal() {
        use crate::model::Member;
        use crate::reader::TypeDefId;

        let dll = all_dlls()
            .into_iter()
            .find(|path| path.file_name().and_then(|n| n.to_str()) == Some("LiteralConsts.dll"))
            .expect("LiteralConsts fixture");
        let bytes = std::fs::read(&dll).expect("fixture");
        let view = Ecma335Assembly::parse(&bytes).expect("parse");

        let TypeDefId(values) = *view
            .image
            .top_level
            .iter()
            .find(|&&TypeDefId(i)| view.image.type_defs[i as usize].name.name == "Values")
            .expect("the LiteralConsts.Values module");

        // Force the non-authoritative path: this DLL *has* a good host pickle, so only a
        // direct call reaches the heuristic the `--standalone` image would take.
        let entity = view
            .project_entity(
                values as usize,
                0,
                true,
                &mut Vec::new(),
                &view.image.type_defs[values as usize].name.namespace,
            )
            .expect("LiteralConsts.Values projects");

        let member_names: Vec<&str> = entity
            .members
            .iter()
            .map(|m| match m {
                Member::Method(x) => x.name.as_str(),
                Member::Field(x) => x.name.as_str(),
                Member::Property(x) => x.name.as_str(),
                Member::Event(x) => x.name.as_str(),
            })
            .collect();
        assert!(
            member_names.contains(&"Int64Val"),
            "a plain literal keeps its name on the IL path — got {member_names:?}"
        );
        assert!(
            !member_names.contains(&"RenamedLit"),
            "the renamed literal's COMPILED name must not be exposed: F# calls it \
             `OriginalLit`, and nothing on this path can recover that — got {member_names:?}"
        );
        assert!(
            !member_names.contains(&"OriginalLit"),
            "and its F# name is not recoverable from IL either, so it must not be \
             invented — got {member_names:?}"
        );
        assert!(
            entity
                .skipped_members
                .iter()
                .any(|s| s.name == "RenamedLit"),
            "the drop must be RECORDED, so a consumer knows to be conservative rather \
             than treating the module as fully enumerable — got {:?}",
            entity.skipped_members
        );
    }

    /// Review round 13: `fsharp_abbreviations_unknowable` was a **blacklist** of the two
    /// ways a host pickle can go bad — it failed to decode, or the image carries foreign
    /// CCUs. A third way slipped through: the host signature resource being *absent
    /// altogether*, which leaves `decoded == None` and matches neither arm.
    ///
    /// An F# assembly's type abbreviations are erased from IL, so they are visible *only*
    /// through the pickle. With no pickle we cannot see them, yet the module would still
    /// be classified `Complete` — and an invisible constructible abbreviation imported by
    /// `open M` can shadow an earlier open's value, so sema would hand back the earlier,
    /// wrong target.
    ///
    /// Neither `--nointerfacedata` (it strips the marker attribute too, so the assembly
    /// stops looking like F# at all) nor a reference assembly (it keeps the signature
    /// data) produces this shape, so it is not reachable from stock fsc today — this is a
    /// *defensive* whitelist, not a live-bug fix. It is written as one anyway, because
    /// §4b of `docs/assembly-module-open-plan.md` is precisely the lesson that a
    /// blacklist cannot name what the model does not represent: the question to ask is
    /// "can I **prove** the pickle authoritative?", not "do I recognise this failure?".
    #[test]
    fn an_fsharp_assembly_with_no_host_pickle_cannot_prove_its_abbreviations_visible() {
        let dll = all_dlls()
            .into_iter()
            .find(|path| path.file_name().and_then(|n| n.to_str()) == Some("MiniLibFs.dll"))
            .expect("MiniLibFs fixture");
        let bytes = std::fs::read(&dll).expect("fixture");
        let mut view = Ecma335Assembly::parse(&bytes).expect("parse");

        // Sanity: as shipped, MiniLibFs has a decodable host pickle, so its abbreviations
        // ARE visible and the flag is down. Without this the test could pass vacuously.
        let (_, skips) = view
            .enumerate_with_skips_impl()
            .expect("enumerate as shipped");
        assert!(
            !skips.fsharp_abbreviations_unknowable,
            "as shipped, MiniLibFs's pickle decodes and its abbreviations are visible"
        );

        // Now strip every F# signature resource, leaving the assembly-level
        // `FSharpInterfaceDataVersionAttribute` in place: an assembly that still says "I
        // am F#" while offering nothing to read its abbreviations from.
        view.image
            .resources
            .retain(|r| !r.name.starts_with("FSharpSignature"));

        let (_, skips) = view
            .enumerate_with_skips_impl()
            .expect("a pickle-less F# assembly still projects its IL");
        assert!(
            skips.fsharp_abbreviations_unknowable,
            "an F# assembly with no host pickle cannot show us its abbreviations, so it \
             must not be treated as fully enumerable — an invisible one would outrank an \
             earlier open's value and produce a wrong target"
        );
    }

    #[test]
    fn pickle_less_generic_module_method_is_presumed_a_value() {
        // The value/function ambiguity of a generic 0-parameter module method
        // (`let empty<'T> = …` vs `let f<'T> () = …`) is resolved authoritatively by
        // the host pickle's argument-group count. When the pickle is absent — this
        // stripped assembly, or a `--standalone`/reference image — that count is
        // unavailable, so `project_fsharp_members` presumes *value*: the common case
        // (`Array.empty`-shaped bindings) and what the pre-`is_module_value_binding`
        // display heuristic produced, so a pickle-less generic value does not regress
        // into a `unit -> …` function on hover. The rare generic unit-function shares
        // the IL shape and is presumed a value too — unrecoverable without the pickle,
        // the accepted limitation.
        use crate::model::Member;
        let dll = all_dlls()
            .into_iter()
            .find(|path| path.file_name().and_then(|n| n.to_str()) == Some("MiniLibFs.dll"))
            .expect("MiniLibFs fixture");
        let bytes = std::fs::read(&dll).expect("fixture");
        let mut view = Ecma335Assembly::parse(&bytes).expect("parse");
        view.image
            .resources
            .retain(|r| !r.name.starts_with("FSharpSignature"));

        let (entities, _skips) = view
            .enumerate_with_skips_impl()
            .expect("a pickle-less F# assembly still projects its IL");
        let hello = entities
            .iter()
            .find(|e| e.name == "Hello" && e.kind == crate::EntityKind::Module)
            .expect("Hello module");
        let method = |name: &str| {
            hello
                .members
                .iter()
                .find_map(|m| match m {
                    Member::Method(mm) if mm.name == name => Some(mm),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("method {name} on Hello"))
        };

        // Generic 0-parameter module methods are presumed values without the pickle.
        assert!(
            method("genEmpty").is_module_value_binding,
            "a generic 0-parameter module value must stay a value on the pickle-less path"
        );
        assert!(
            method("genPingUnit").is_module_value_binding,
            "the presumption cannot distinguish the rare generic unit-function; value is the default"
        );
        // The presumption is scoped to the generic 0-parameter shape. A non-generic
        // unit-function (`let ping () = 1`) is not presumed a value (it stays a
        // function), and a generic function that *takes a parameter*
        // (`let identity<'a> (x: 'a)`) never matches the 0-parameter shape either.
        assert!(!method("ping").is_module_value_binding);
        assert!(!method("identity").is_module_value_binding);
    }

    #[test]
    fn enumerate_records_host_signature_decode_failure_and_keeps_base_projection() {
        let dll = all_dlls()
            .into_iter()
            .find(|path| path.file_name().and_then(|name| name.to_str()) == Some("MiniLibFs.dll"))
            .expect("MiniLibFs fixture");
        let bytes = std::fs::read(&dll).expect("fixture");
        let mut view = Ecma335Assembly::parse(&bytes).expect("parse");
        let host_name = view.identity.name.clone();
        let resource_name = format!("FSharpSignatureData.{host_name}");
        let host_signature = view
            .image
            .resources
            .iter_mut()
            .find(|resource| resource.name == format!("FSharpSignatureCompressedData.{host_name}"))
            .expect("MiniLibFs host signature resource");
        host_signature.name = resource_name.clone();
        host_signature.bytes = Vec::new();

        let (entities, skips) = view
            .enumerate_with_skips_impl()
            .expect("invalid host pickle is a recorded overlay skip, not fatal");

        assert!(
            !entities.is_empty(),
            "base ECMA projection should still be returned"
        );
        assert_eq!(skips.skipped_fsharp_overlays.len(), 1);
        let skip = &skips.skipped_fsharp_overlays[0];
        assert_eq!(skip.resource_name, resource_name);
        assert_eq!(
            skip.overlays,
            vec![
                FsharpOverlayKind::SourceName,
                FsharpOverlayKind::Extension,
                FsharpOverlayKind::Measure,
                FsharpOverlayKind::AbbreviationMarkers,
                FsharpOverlayKind::UnionCases,
            ]
        );
        assert!(
            !skip.reason.is_empty(),
            "skip should surface the decode reason"
        );
    }

    /// **Review (codex P2).** `fsharp_abbreviations_unknowable` exempts FSharp.Core —
    /// its abbreviations are hard-coded primitive aliases, never a shadow risk — but
    /// that exemption must NOT extend to its F#-native *extension* index, which is
    /// ordinary pickle data. When the host pickle cannot be read, the extension
    /// overlay never runs and the index is empty *because unread*; a name-keyed gate
    /// that trusts it would prove a colliding extension absent and commit a wrong
    /// overload. So the two knowability bits must diverge for a pickle-less
    /// FSharp.Core: abbreviations exempt (`false`), extensions unknowable (`true`).
    ///
    /// Simulated by renaming an F# fixture's manifest identity to `FSharp.Core`: the
    /// host signature resource (still named for the fixture) is then unfindable, so
    /// `decoded == None` and the extension overlay is skipped — exactly the shape a
    /// stripped or format-upgraded FSharp.Core would present.
    #[test]
    fn fsharp_core_with_unreadable_pickle_keeps_extensions_unknowable() {
        let dll = all_dlls()
            .into_iter()
            .find(|path| path.file_name().and_then(|n| n.to_str()) == Some("MiniLibFs.dll"))
            .expect("MiniLibFs fixture");
        let bytes = std::fs::read(&dll).expect("fixture");
        let mut view = Ecma335Assembly::parse(&bytes).expect("parse");

        // Baseline: as shipped, the pickle decodes — the extension index IS built, so
        // neither bit is raised. Without this the test could pass vacuously.
        let (_, shipped) = view
            .enumerate_with_skips_impl()
            .expect("enumerate as shipped");
        assert!(
            !shipped.fsharp_abbreviations_unknowable && !shipped.fsharp_extension_index_unknowable,
            "as shipped, MiniLibFs's pickle decodes: both knowability bits are down"
        );

        // Pose as FSharp.Core: the host signature resource (named for the fixture) is
        // now unfindable, so the extension overlay is skipped — yet FSharp.Core is
        // abbreviation-exempt.
        view.identity.name = "FSharp.Core".to_string();
        let (_, skips) = view
            .enumerate_with_skips_impl()
            .expect("a pickle-less F# assembly still projects its IL");
        assert!(
            !skips.fsharp_abbreviations_unknowable,
            "FSharp.Core is exempt from the abbreviation-unknowable signal"
        );
        assert!(
            skips.fsharp_extension_index_unknowable,
            "…but its extension index is unread, so the name-keyed gate must treat it \
             as unknowable — the whole point of the separate bit"
        );
    }

    /// A `GenericParamConstraint` row carrying an **unrecognised** custom attribute
    /// is still refused loud, rather than silently dropped — which would also let an
    /// attributed `System.ValueType modreq(UnmanagedType)` row masquerade as the
    /// canonical `unmanaged` marker and be consumed.
    ///
    /// What changed (EX-2): the projector used to refuse *every* attributed
    /// constraint, on the claim that "real compilers never emit them". That was true
    /// when written and is **false on .NET 9+** — the BCL annotates the constraint
    /// rows of the generic-math / parsing interfaces with `[Nullable]`
    /// (`where TSelf : IParsable<TSelf>`), and the blanket refusal silently dropped
    /// 38 types from `System.Runtime` alone. The recognised-and-discarded set is now
    /// classified exactly as the typar's *own* attributes are, and its acceptance is
    /// pinned on the real BCL (`bcl_ref_pack_sweep`: the whole .NET 10 reference pack
    /// projects with **zero** type drops; `projector_generics`: `INumber` / `IParsable`
    /// project). This test keeps the other half honest — an attribute nobody
    /// classified still fails loud.
    #[test]
    fn refuses_constraint_carrying_an_unrecognised_custom_attribute() {
        use crate::ImportError;
        use crate::reader::{
            GenericParam, MemberHandle, MemberRefId, ModifiedType, Primitive as RawPrimitive,
            RawAttribute, TypeConstraint, TypeSig, Variance as RawVariance,
        };

        let dll = all_dlls().into_iter().next().expect("a fixture");
        let bytes = std::fs::read(&dll).expect("fixture");
        let view = Ecma335Assembly::parse(&bytes).expect("parse");

        // A perfectly projectable constraint type (`int`): without the
        // attribute guard the projector would *accept* this constraint and
        // discard the attribute, so the only thing under test is the refusal.
        let constraint = TypeConstraint {
            ty: Ok(ModifiedType::plain(TypeSig::Primitive(RawPrimitive::Int32))),
            attributes: vec![RawAttribute {
                ctor: MemberHandle::MemberRef(MemberRefId(0)),
                blob: Vec::new(),
            }],
        };
        let gp = GenericParam {
            name: "T".to_string(),
            variance: RawVariance::Invariant,
            reference_type: false,
            value_type: false,
            default_ctor: false,
            allows_ref_struct: false,
            constraints: vec![constraint],
            attributes: Vec::new(),
        };

        let err = view
            .project_generic_param(&gp, None)
            .expect_err("attributed constraint must be refused");
        match err {
            ImportError::UnsupportedSignature { detail } => {
                assert!(
                    detail.contains("unsupported attribute"),
                    "expected the unrecognised-constraint-attribute refusal, got: {detail}"
                );
            }
            other => panic!("expected refuse-loud UnsupportedSignature, got {other:?}"),
        }
    }

    /// A **recognised** constraint attribute (`[Nullable]`) whose blob is malformed
    /// must still fail loud (GPT-5.6 review of the constraint-attribute PR). The
    /// nullability it carries has no per-constraint slot and is discarded — but
    /// *recognising the owner is not a licence to skip validation*: a byte outside
    /// 0/1/2, named args, or a bad ctor signature fails everywhere else it appears,
    /// and must here too, or the reader's fail-loud boundary quietly weakens for one
    /// attribute on one row kind.
    #[test]
    fn refuses_a_malformed_recognised_constraint_attribute() {
        use crate::ImportError;
        use crate::reader::{
            GenericParam, MemberHandle, MemberRefId, ModifiedType, Primitive as RawPrimitive,
            RawAttribute, TypeConstraint, TypeSig, Variance as RawVariance,
        };

        // A real `NullableAttribute` ctor member ref — some fixture references it
        // (the BCL puts `[Nullable]` on the generic-math constraint rows). Reusing a
        // genuine handle is what makes `attribute_owning_type` resolve the owner to
        // `NullableAttribute`, so the *recognised* arm — not the catch-all refusal —
        // is the one under test.
        // The decode is signature-driven, so the two `NullableAttribute` ctors —
        // `(byte)` and `(byte[])` — need distinct handles: a byte blob against the
        // vector ctor (or vice versa) is "Malformed" *before* any semantic check,
        // which is not the case under test. Find a fixture (the BCL has both) and
        // classify each `NullableAttribute` ctor memberref by its first param.
        let found = all_dlls().into_iter().find_map(|path| {
            let bytes = std::fs::read(&path).ok()?;
            let view = Ecma335Assembly::parse(&bytes).ok()?;
            let is_nullable = |m: &crate::reader::MemberRef| {
                matches!(
                    m.parent,
                    crate::reader::MemberRefParent::TypeRef(crate::reader::TypeRefId(r))
                        if view.image.type_refs[r as usize].name.name == "NullableAttribute"
                )
            };
            let first_param_is_array = |m: &crate::reader::MemberRef| {
                matches!(&m.signature, Ok(sig)
                    if matches!(sig.param_types.first().map(|p| &p.ty),
                        Some(TypeSig::SzArray(_))))
            };
            let scalar = view
                .image
                .member_refs
                .iter()
                .position(|m| is_nullable(m) && !first_param_is_array(m))?;
            let vector = view
                .image
                .member_refs
                .iter()
                .position(|m| is_nullable(m) && first_param_is_array(m))?;
            Some((view, MemberRefId(scalar as u32), MemberRefId(vector as u32)))
        });
        let (view, nullable_ctor, nullable_vec_ctor) =
            found.expect("a fixture references both NullableAttribute ctors (the BCL does)");

        let make_gp = |attrs: Vec<RawAttribute>| GenericParam {
            name: "T".to_string(),
            variance: RawVariance::Invariant,
            reference_type: false,
            value_type: false,
            default_ctor: false,
            allows_ref_struct: false,
            constraints: vec![TypeConstraint {
                ty: Ok(ModifiedType::plain(TypeSig::Primitive(RawPrimitive::Int32))),
                attributes: attrs,
            }],
            attributes: Vec::new(),
        };

        // (a) A structurally-valid `[Nullable(3uy)]` blob — prolog `01 00`, the byte
        //     `03`, then zero named args — decodes fine but 3 is not a nullability
        //     byte (only 0/1/2). It must fail loud, exactly as a typar's own would.
        let bad_byte = RawAttribute {
            ctor: MemberHandle::MemberRef(nullable_ctor),
            blob: vec![0x01, 0x00, 0x03, 0x00, 0x00],
        };
        match view.project_generic_param(&make_gp(vec![bad_byte.clone()]), None) {
            Err(ImportError::UnsupportedSignature { detail }) => assert!(
                detail.contains("not 0/1/2"),
                "expected the out-of-range nullability-byte refusal, got: {detail}"
            ),
            other => panic!("a malformed recognised [Nullable] must fail loud, got {other:?}"),
        }

        // (b) Two `[Nullable]` rows on one constraint — malformed metadata (Roslyn
        //     emits one), refused as the typar's own multiple-row case is.
        let good_byte = RawAttribute {
            ctor: MemberHandle::MemberRef(nullable_ctor),
            blob: vec![0x01, 0x00, 0x01, 0x00, 0x00],
        };
        match view.project_generic_param(&make_gp(vec![good_byte.clone(), good_byte]), None) {
            Err(ImportError::UnsupportedSignature { detail }) => assert!(
                detail.contains("multiple NullableAttribute"),
                "expected the duplicate-row refusal, got: {detail}"
            ),
            other => panic!("duplicate [Nullable] rows must fail loud, got {other:?}"),
        }

        // (c) A well-decoded `[Nullable(byte[])]` whose length does not match the
        //     constraint type's annotatable positions. `int` is a value-type — it has
        //     ZERO nullable positions — so a 2-element vector over-supplies, and the
        //     walk-vs-length check (the same one a field/return runs) must fire. This
        //     is the case a bare decode-and-discard would have missed: the length is
        //     only validated by *walking the source against the type*.
        //     Blob: prolog `01 00`, a `SzArray<U8>` of `[1, 1]` (elem-type tag `0x02`
        //     for U8, count `02 00 00 00`, then the two bytes), zero named args.
        let bad_len = RawAttribute {
            ctor: MemberHandle::MemberRef(nullable_vec_ctor),
            blob: vec![
                0x01, 0x00, // prolog
                0x02, 0x00, 0x00, 0x00, // array length 2
                0x01, 0x01, // two bytes
                0x00, 0x00, // 0 named args
            ],
        };
        match view.project_generic_param(&make_gp(vec![bad_len]), None) {
            Err(ImportError::UnsupportedSignature { detail }) => assert!(
                detail.contains("length mismatch") || detail.contains("byte(s) but"),
                "expected the byte[] length-mismatch refusal, got: {detail}"
            ),
            other => panic!("a length-mismatched [Nullable] byte[] must fail loud, got {other:?}"),
        }

        // (d) The single valid `[Nullable(1uy)]` still projects — the fix validates,
        //     it does not reject the shape the BCL actually emits.
        let ok = RawAttribute {
            ctor: MemberHandle::MemberRef(nullable_ctor),
            blob: vec![0x01, 0x00, 0x01, 0x00, 0x00],
        };
        view.project_generic_param(&make_gp(vec![ok]), None)
            .expect("a valid [Nullable] constraint attribute projects");
    }

    /// The F# member path drops events wholesale (FCS surfaces none) — but an
    /// event carrying an `Other`-semantics accessor is "the model can't carry
    /// it", not "F# deliberately hides it", and its accessor method is hidden
    /// by the exclusion set, so the drop must be *recorded*. No compiler emits
    /// an F#-kinded type with an Other accessor, so drive the private member
    /// path directly with a hand-built `TypeDef`.
    #[test]
    fn fsharp_kind_event_with_other_accessor_is_recorded() {
        use crate::EntityKind;
        use crate::reader::{
            Accessibility as RawAccessibility, Event as RawEvt, ModifiedType,
            Primitive as RawPrimitive, TypeDef, TypeName, TypeSig,
        };

        let dll = all_dlls().into_iter().next().expect("a fixture");
        let bytes = std::fs::read(&dll).expect("fixture");
        let view = Ecma335Assembly::parse(&bytes).expect("parse");

        let td = TypeDef {
            name: TypeName {
                namespace: String::new(),
                name: "M".to_string(),
            },
            accessibility: RawAccessibility::Public,
            is_interface: false,
            is_sealed: false,
            extends: None,
            implements: vec![],
            generic_params: vec![],
            enclosing: None,
            nested: vec![],
            methods: vec![],
            fields: vec![],
            properties: vec![],
            events: vec![RawEvt {
                name: "Tick".to_string(),
                event_type: Ok(ModifiedType::plain(TypeSig::Primitive(RawPrimitive::Int32))),
                add: None,
                remove: None,
                raise: None,
                other_accessors: vec![None],
                attributes: vec![],
            }],
            attributes: vec![],
        };

        let projected = view
            .project_fsharp_members(EntityKind::Module, &td, None, true)
            .expect("the F# member path itself succeeds");
        let skip = projected
            .skipped
            .iter()
            .find(|s| s.name == "Tick")
            .expect("the Other-accessor event is recorded, not silently vanished");
        assert!(
            skip.reason
                .contains("non-standard (Other) method-semantics accessor"),
            "the record names the cause, got: {}",
            skip.reason
        );
    }

    /// A module `let mutable` value's setter is consumed only as the
    /// mutability bit (its method is hidden by the accessor-exclusion set), so
    /// a *defective* setter — compilercontrolled visibility — must refuse the
    /// member rather than be laundered into `is_mutable: true` unrecorded.
    #[test]
    fn module_value_with_defective_setter_is_recorded() {
        use crate::EntityKind;
        use crate::reader::{
            AccessDefect, Accessibility as RawAccessibility, CallConv, MemberAccess, Method,
            MethodId, MethodSig, ModifiedType, Primitive as RawPrimitive, Property as RawProp,
            RetType, TypeDef, TypeName, TypeSig,
        };

        let dll = all_dlls().into_iter().next().expect("a fixture");
        let bytes = std::fs::read(&dll).expect("fixture");
        let view = Ecma335Assembly::parse(&bytes).expect("parse");

        let method = |name: &str, access: Result<MemberAccess, AccessDefect>| Method {
            token: 0x0600_0001,
            name: name.to_string(),
            accessibility: access,
            is_static: true,
            is_abstract: false,
            is_virtual: false,
            is_final: false,
            is_rt_special_name: false,
            is_new_slot: false,
            is_hide_by_sig: false,
            generic_params: vec![],
            signature: Ok(MethodSig {
                has_this: false,
                explicit_this: false,
                calling_convention: CallConv::Default,
                return_type: RetType::Type(ModifiedType::plain(TypeSig::Primitive(
                    RawPrimitive::Int32,
                ))),
                return_attributes: vec![],
                parameters: vec![],
            }),
            attributes: vec![],
            implements: Vec::new(),
            unclassified_impls: Vec::new(),
        };

        let td = TypeDef {
            name: TypeName {
                namespace: String::new(),
                name: "M".to_string(),
            },
            accessibility: RawAccessibility::Public,
            is_interface: false,
            is_sealed: false,
            extends: None,
            implements: vec![],
            generic_params: vec![],
            enclosing: None,
            nested: vec![],
            methods: vec![
                method("get_v", Ok(MemberAccess::Public)),
                method("set_v", Err(AccessDefect::CompilerControlled)),
            ],
            fields: vec![],
            properties: vec![RawProp {
                name: "v".to_string(),
                signature: Ok(ModifiedType::plain(TypeSig::Primitive(RawPrimitive::Int32))),
                getter: Some(MethodId(0)),
                setter: Some(MethodId(1)),
                other_accessors: vec![],
                attributes: vec![],
            }],
            events: vec![],
            attributes: vec![],
        };

        let projected = view
            .project_fsharp_members(EntityKind::Module, &td, None, true)
            .expect("the F# member path itself succeeds");
        assert!(
            projected.kept.is_empty(),
            "the value must not surface with a fabricated mutability bit: {:?}",
            projected.kept
        );
        let skip = projected
            .skipped
            .iter()
            .find(|s| s.name == "v")
            .expect("the defective-setter value is recorded");
        assert!(
            skip.reason.contains("compilercontrolled"),
            "the record names the cause, got: {}",
            skip.reason
        );
    }

    /// A `MethodSemantics` `Other` row whose method RID fell outside the
    /// owner's method run is stored as a `None` slot rather than dropped
    /// (`Property::other_accessors`) — the row's *presence* must still refuse
    /// the owner, or malformed metadata could launder a property with an
    /// unmodellable accessor back into the projectable set. No emitter helper
    /// produces the dangling shape, so assert the seam directly.
    #[test]
    fn other_accessor_with_dangling_rid_still_refuses_the_owner() {
        use crate::ImportError;

        let err = super::reject_other_accessors("property", "P", &[None])
            .expect_err("a dangling Other row must still refuse the owner");
        match err {
            ImportError::UnsupportedEcmaLayout { detail } => {
                assert!(
                    detail.contains("non-standard (Other) method-semantics accessor"),
                    "expected the Other-accessor refusal, got: {detail}"
                );
            }
            other => panic!("expected refuse-loud UnsupportedEcmaLayout, got {other:?}"),
        }
    }

    /// A byref (`ref`) index parameter on an indexer has no slot in the model
    /// (`Property::parameters` is value-typed), so it must be refused rather
    /// than projected as a value-typed index — which would silently change the
    /// signature. No corpus indexer is byref, so assert the refusal directly.
    #[test]
    fn refuses_byref_index_parameter() {
        use crate::ImportError;

        let byref = Parameter {
            name: Some("i".to_string()),
            ty: TypeRef::Primitive(Primitive::I4),
            is_byref: true,
            is_out: false,
            is_readonly_ref: false,
            default: ParamDefault::None,
            is_param_array: false,
            nullability: Nullability::Oblivious,
        };
        let err = super::index_params_to_index_params(std::slice::from_ref(&byref), "Item")
            .expect_err("a byref index parameter must be refused");
        assert!(
            matches!(err, ImportError::UnsupportedSignature { .. }),
            "expected refuse-loud, got {err:?}"
        );

        // A value-typed index parameter projects through with its name and type.
        let value = Parameter {
            is_byref: false,
            ..byref
        };
        let ok = super::index_params_to_index_params(std::slice::from_ref(&value), "Item")
            .expect("value-typed index parameter projects");
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].name.as_deref(), Some("i"));
        assert_eq!(ok[0].ty.ty, TypeRef::Primitive(Primitive::I4));
    }

    /// A byref-to-byref outer position (a field/property/return whose referent
    /// is itself `ELEMENT_TYPE_BYREF`) is malformed — ECMA-335 forbids it. With
    /// no `[Nullable]` attribute the position takes `walk_position`'s oblivious
    /// fast path (`project_type_ref`), which would otherwise fabricate a nested
    /// `TypeRef::ByRef`; `walk_byref_position` guards it so the refusal holds on
    /// both the fast and the nullable-walk paths. No real compiler emits it, so
    /// assert the refusal directly against a synthetic `ByRef(ByRef(int))`.
    #[test]
    fn refuses_byref_to_byref_position() {
        use crate::ImportError;
        use crate::reader::{ModifiedType, Primitive as RawPrimitive, TypeSig};

        let dll = all_dlls().into_iter().next().expect("a fixture");
        let bytes = std::fs::read(&dll).expect("fixture");
        let view = Ecma335Assembly::parse(&bytes).expect("parse");

        let nested = ModifiedType::plain(TypeSig::ByRef(Box::new(ModifiedType::plain(
            TypeSig::ByRef(Box::new(ModifiedType::plain(TypeSig::Primitive(
                RawPrimitive::Int32,
            )))),
        ))));
        // No attributes → the oblivious fast path, the one that used to fabricate
        // `ByRef(ByRef(int))` instead of refusing.
        match view
            .walk_byref_position(&nested, &[], None)
            .expect_err("a byref-to-byref position must be refused")
        {
            ImportError::UnsupportedSignature { detail } => {
                assert!(
                    detail.contains("byref referent is itself a byref"),
                    "expected the nested-byref refusal, got: {detail}"
                );
            }
            other => panic!("expected refuse-loud UnsupportedSignature, got {other:?}"),
        }

        // A single (well-formed) byref still projects: `ByRef(int)` → `int&`.
        let single = ModifiedType::plain(TypeSig::ByRef(Box::new(ModifiedType::plain(
            TypeSig::Primitive(RawPrimitive::Int32),
        ))));
        let (ty, _, mods) = view
            .walk_byref_position(&single, &[], None)
            .expect("a single byref projects");
        assert_eq!(
            ty,
            TypeRef::ByRef {
                inner: Box::new(TypeRef::Primitive(Primitive::I4)),
                readonly: false,
            }
        );
        assert_eq!(mods, crate::ecma335_assembly::Modifiers::default());
    }

    /// A corrupt nesting chain that forms a *cycle* must fail loud, not loop
    /// unboundedly. A single-byte mutation of a real F# DLL was observed to
    /// make the name walk follow such a cycle and allocate ~81 GiB before the
    /// OS killed it; the `Nested`/`enclosing` walks are now bounded by the
    /// table size and refuse loud. No real compiler emits a cyclic chain, so
    /// this is driven against synthetic cycles grafted onto a parsed fixture.
    #[test]
    fn refuses_cyclic_nesting_chains() {
        use crate::ImportError;
        use crate::reader::{RefScope, TypeDefId, TypeRefId};

        let dll = all_dlls().into_iter().next().expect("a fixture");
        let bytes = std::fs::read(&dll).expect("fixture");
        let mut view = Ecma335Assembly::parse(&bytes).expect("parse");

        // A `TypeRef` `Nested` self-loop: row 0 is its own enclosing scope.
        assert!(!view.image.type_refs.is_empty(), "fixture carries TypeRefs");
        view.image.type_refs[0].scope = RefScope::Nested(TypeRefId(0));
        match view
            .qualified_typeref_name(0)
            .expect_err("a cyclic TypeRef nesting chain must be refused")
        {
            ImportError::UnsupportedEcmaLayout { detail } => {
                assert!(
                    detail.contains("cyclic"),
                    "expected the cyclic-TypeRef refusal, got: {detail}"
                );
            }
            other => panic!("expected UnsupportedEcmaLayout, got {other:?}"),
        }

        // A `TypeDef` `enclosing` self-loop (row 0 is its own encloser).
        view.image.type_defs[0].enclosing = Some(TypeDefId(0));
        match view
            .qualified_typedef_name(0)
            .expect_err("a cyclic TypeDef enclosing chain must be refused")
        {
            ImportError::UnsupportedEcmaLayout { detail } => {
                assert!(
                    detail.contains("cyclic"),
                    "expected the cyclic-TypeDef refusal, got: {detail}"
                );
            }
            other => panic!("expected UnsupportedEcmaLayout, got {other:?}"),
        }

        // A `nested`-type self-loop: a projectable type that nests itself drives
        // `project_entity`'s recursion. Pick a *non-skipped* type that projects
        // cleanly (the `<Module>` pseudo-type is skipped as a nested child, so a
        // self-loop on it never recurses), so the only thing under test is the
        // recursion bound. Re-parse: the mutations above corrupted `view`'s arena.
        let mut view = Ecma335Assembly::parse(&bytes).expect("parse");
        let TypeDefId(top) = *view
            .image
            .top_level
            .iter()
            .find(|&&TypeDefId(i)| {
                !super::is_skipped_type(&view.image.type_defs[i as usize])
                    && view
                        .project_entity(i as usize, 0, true, &mut Vec::new(), "")
                        .is_ok()
            })
            .expect("a projectable, non-skipped top-level type");
        view.image.type_defs[top as usize].nested = vec![TypeDefId(top)];
        match view
            .project_entity(top as usize, 0, true, &mut Vec::new(), "")
            .expect_err("a cyclic nested-type chain must be refused")
        {
            ImportError::CyclicTypeNesting { detail } => {
                assert!(
                    detail.contains("recursion bound"),
                    "expected the cyclic-nested refusal, got: {detail}"
                );
            }
            other => panic!("expected CyclicTypeNesting, got {other:?}"),
        }
    }
}
