//! Stage 5: the member table walk.
//!
//! [`read_members`] walks the `MethodDef`/`Field`/`Param`/`Property`/
//! `PropertyMap`/`Event`/`EventMap`/`MethodSemantics`/`Constant` tables and
//! produces, per `TypeDef`, the owned [`Method`]/[`Field`]/[`Property`]/
//! [`Event`] lists — including the `Param` metadata (names, `in`/`out`,
//! optional/default) and the per-position custom attributes (parameter, return,
//! and `GenericParam` rows) consumers later classify.
//!
//! Two cross-cutting pieces live here too, because members are their largest
//! consumer:
//! - [`CustomAttributeIndex`] walks the `CustomAttribute` table once and groups
//!   every attribute by its parent `(table, rid)`, resolving each constructor
//!   handle a single time. Every position (type, method, field, parameter,
//!   property, event, generic parameter) then pulls its own attributes from it.
//! - [`read_generic_params`] reads *both* the type- and method-owned
//!   `GenericParam` rows (with their attributes), so the type stage and the
//!   member stage share one walk.
//!
//! It is total over structurally-valid metadata: per-item signature failures are
//! stored as `Result<_, SigError>`, and the two member-level shapes the model
//! cannot express — `CompilerControlled` visibility (stored as a per-member
//! [`AccessDefect`]) and `Other`-semantics accessors (recorded on
//! [`Property::other_accessors`] / [`Event::other_accessors`]) — are likewise
//! stored in the data, so one unrepresentable member never aborts the assembly.

use std::collections::HashMap;

use super::Error;
use super::ids::{MemberRefId, MethodId, TypeDefId, TypeRefId};
use super::metadata::MetadataFile;
use super::model::{
    AccessDefect, AccessorOwner, Constant, DeclSemantics, Event, Field, GenericParam,
    InterfaceMemberImpl, MemberAccess, MemberHandle, MemberRef, MemberRefParent, Method, MethodSig,
    Param, Property, RawAttribute, TypeConstraint, UnclassifiedImpl, Variance,
};
use super::signature::{
    CustomMod, DecodedMethodSig, ImageTables, ModifiedType, RetType, TypeScope, TypeSig,
    decode_field_sig, decode_method_sig, decode_property_sig,
};
use super::tables::{Coded, Tables, table};
use super::typedefs::{
    INTERFACE_FLAG, checked_index, decode_type_def_or_ref, method_owner, read_interface_impls,
    typedef_index,
};

// --- ECMA-335 flag masks ---

/// MethodAttributes (§II.23.1.10).
const METHOD_ACCESS_MASK: u32 = 0x0007;
const METHOD_STATIC: u32 = 0x0010;
const METHOD_FINAL: u32 = 0x0020;
const METHOD_VIRTUAL: u32 = 0x0040;
const METHOD_HIDE_BY_SIG: u32 = 0x0080;
const METHOD_NEW_SLOT: u32 = 0x0100;
const METHOD_ABSTRACT: u32 = 0x0400;
const METHOD_RT_SPECIAL_NAME: u32 = 0x1000;

/// FieldAttributes (§II.23.1.5).
const FIELD_ACCESS_MASK: u32 = 0x0007;
const FIELD_STATIC: u32 = 0x0010;
const FIELD_INIT_ONLY: u32 = 0x0020;
const FIELD_LITERAL: u32 = 0x0040;

/// ParamAttributes (§II.23.1.13).
const PARAM_IN: u32 = 0x0001;
const PARAM_OUT: u32 = 0x0002;
const PARAM_OPTIONAL: u32 = 0x0010;

/// MethodSemantics semantics flags (§II.23.1.12).
const SEM_SETTER: u32 = 0x0001;
const SEM_GETTER: u32 = 0x0002;
const SEM_OTHER: u32 = 0x0004;
const SEM_ADD_ON: u32 = 0x0008;
const SEM_REMOVE_ON: u32 = 0x0010;
const SEM_FIRE: u32 = 0x0020;

/// GenericParamAttributes (§II.23.1.7).
const VARIANCE_MASK: u16 = 0x0003;
const COVARIANT: u16 = 0x0001;
const CONTRAVARIANT: u16 = 0x0002;
const REFERENCE_TYPE_CONSTRAINT: u16 = 0x0004;
const VALUE_TYPE_CONSTRAINT: u16 = 0x0008;
const DEFAULT_CTOR_CONSTRAINT: u16 = 0x0010;
/// `AllowByRefLike` — the C# 13 / F# 9 `allows ref struct` anti-constraint.
const ALLOWS_REF_STRUCT: u16 = 0x0020;

/// Fold the §II.23.1.10 member-visibility field (the low 3 bits of a method's or
/// field's flags) onto [`MemberAccess`]. Value 0 is `CompilerControlled`
/// (privatescope) and value 7 is reserved; neither has a variant, so both are
/// stored as a per-member [`AccessDefect`] (mirroring [`SigError`] handling —
/// one such member is dropped and recorded at projection, never sinking the
/// image) rather than mapped onto `Private`.
fn fold_member_access(flags: u32) -> Result<MemberAccess, AccessDefect> {
    match flags & METHOD_ACCESS_MASK {
        0 => Err(AccessDefect::CompilerControlled),
        1 => Ok(MemberAccess::Private),
        2 => Ok(MemberAccess::FamAndAssem),
        3 => Ok(MemberAccess::Assembly),
        4 => Ok(MemberAccess::Family),
        5 => Ok(MemberAccess::FamOrAssem),
        6 => Ok(MemberAccess::Public),
        _ => Err(AccessDefect::Reserved),
    }
}

// `METHOD_ACCESS_MASK == FIELD_ACCESS_MASK` (both 0x07); the assert documents
// that `fold_member_access` serves fields too.
const _: () = assert!(METHOD_ACCESS_MASK == FIELD_ACCESS_MASK);

/// Refuse the `*Ptr` indirection tables (`FieldPtr`/`MethodPtr`/`ParamPtr`/
/// `PropertyPtr`/`EventPtr`), which appear only in unoptimized (`#-`) metadata.
/// This reader targets optimized streams, where a member's RID *is* its physical
/// position — the range arithmetic in this module relies on that. An image that
/// populates a `*Ptr` table is refused rather than mis-walked.
pub(super) fn ensure_optimized_layout(tables: &Tables) -> Result<(), Error> {
    for t in [
        table::FIELD_PTR,
        table::METHOD_PTR,
        table::PARAM_PTR,
        table::PROPERTY_PTR,
        table::EVENT_PTR,
    ] {
        if tables.row_count(t) != 0 {
            return Err(Error::UnsupportedTableStream);
        }
    }
    Ok(())
}

// ============================================================================
// Custom-attribute index
// ============================================================================

/// Every `CustomAttribute` row grouped by its parent `(table, rid)`, the
/// constructor handle resolved once. Consumers [`take`](Self::take) their
/// position's attributes; each parent is unique, so taking removes the entry.
pub(super) struct CustomAttributeIndex {
    by_parent: HashMap<(usize, u32), Vec<RawAttribute>>,
}

impl CustomAttributeIndex {
    /// Walk the `CustomAttribute` table (§II.22.10) once. A null parent is not a
    /// row this reader attaches and is skipped; a parent whose RID dangles past
    /// the end of its table is refused with [`Error::TableIndexOutOfRange`]
    /// rather than stored under an unreachable key and silently dropped. The
    /// constructor (`Type` column) is resolved through [`resolve_attribute_ctor`].
    pub(super) fn build(
        md: &MetadataFile,
        tables: &Tables,
        method_starts: &[u32],
    ) -> Result<Self, Error> {
        let mut by_parent: HashMap<(usize, u32), Vec<RawAttribute>> = HashMap::new();
        for i in 0..tables.row_count(table::CUSTOM_ATTRIBUTE) {
            let row = tables.row(table::CUSTOM_ATTRIBUTE, i)?;
            let Some(parent) = tables.decode_coded(Coded::HasCustomAttribute, row.coded(0))? else {
                continue;
            };
            // `decode_coded` validates the tag but not the RID against its
            // table; a dangling parent is a structural defect, refused loudly.
            checked_index(parent.rid, tables.row_count(parent.table))?;
            let ctor = resolve_attribute_ctor(tables, method_starts, row.coded(1))?;
            let blob = md.blob_at(row.int(2))?.to_vec();
            by_parent
                .entry((parent.table, parent.rid))
                .or_default()
                .push(RawAttribute { ctor, blob });
        }
        Ok(Self { by_parent })
    }

    /// The attributes on the row at 1-based `rid` of `table`, removing them from
    /// the index (each parent is consumed by exactly one builder).
    pub(super) fn take(&mut self, table: usize, rid: u32) -> Vec<RawAttribute> {
        self.by_parent.remove(&(table, rid)).unwrap_or_default()
    }
}

/// Resolve a `CustomAttribute.Type` coded index (§II.22.10) to the constructor it
/// names: a `MethodDef` defined here (mapped to its owning type + local
/// `MethodId`) or a `MemberRef`. A null or otherwise non-constructor token is
/// malformed.
fn resolve_attribute_ctor(
    tables: &Tables,
    method_starts: &[u32],
    coded: u32,
) -> Result<MemberHandle, Error> {
    let tok = tables
        .decode_coded(Coded::CustomAttributeType, coded)?
        .ok_or(Error::TableIndexOutOfRange)?;
    match tok.table {
        table::METHOD_DEF => {
            let (owner, method) = method_owner(method_starts, tok.rid, tables)?;
            Ok(MemberHandle::MethodDef(owner, method))
        }
        table::MEMBER_REF => {
            let id = checked_index(tok.rid, tables.row_count(table::MEMBER_REF))?;
            Ok(MemberHandle::MemberRef(MemberRefId(id)))
        }
        // CustomAttributeType only decodes to MethodDef/MemberRef.
        _ => Err(Error::TableIndexOutOfRange),
    }
}

// ============================================================================
// MemberRef arena
// ============================================================================

/// Project every `MemberRef` row (§II.22.25), in table order (so a
/// [`MemberRefId`] minted as a table-order index resolves directly). Each row's
/// `Signature` blob is decoded as a method-reference signature — the form a
/// custom-attribute constructor carries; a field-referencing `MemberRef` stores
/// the resulting `Err` (it is never consumed).
pub(super) fn read_member_refs(
    tables: &Tables,
    image_tables: &ImageTables,
) -> Result<Vec<MemberRef>, Error> {
    let count = tables.row_count(table::MEMBER_REF);
    let mut refs = Vec::with_capacity(count as usize);
    for i in 0..count {
        // Columns: Class(0) = MemberRefParent coded, Name(1), Signature(2) = blob.
        let row = tables.row(table::MEMBER_REF, i)?;
        let parent = resolve_member_ref_parent(tables, row.coded(0))?;
        let name = row.string(1)?.to_string();
        let signature = decode_method_sig(row.blob(2)?, image_tables);
        refs.push(MemberRef {
            name,
            parent,
            signature,
        });
    }
    Ok(refs)
}

/// Resolve a `MemberRefParent` coded index to the type that declares the member.
/// `TypeDef`/`TypeRef` parents (the attribute-constructor cases) are kept; the
/// `ModuleRef`/`MethodDef`/`TypeSpec` parents fold into `Other` (their RID is
/// still range-checked so a dangling index is refused loudly). A null parent is
/// malformed for a `MemberRef`.
fn resolve_member_ref_parent(tables: &Tables, coded: u32) -> Result<MemberRefParent, Error> {
    let tok = tables
        .decode_coded(Coded::MemberRefParent, coded)?
        .ok_or(Error::TableIndexOutOfRange)?;
    checked_index(tok.rid, tables.row_count(tok.table))?;
    Ok(match tok.table {
        table::TYPE_DEF => MemberRefParent::TypeDef(TypeDefId(tok.rid - 1)),
        table::TYPE_REF => MemberRefParent::TypeRef(TypeRefId(tok.rid - 1)),
        _ => MemberRefParent::Other,
    })
}

// ============================================================================
// Generic parameters (type- and method-owned)
// ============================================================================

/// The generic parameters of an image, bucketed by owner.
pub(super) struct GenericParams {
    /// Type-owned parameters, indexed by `TypeDefId` (length = `TypeDef` rows).
    pub(super) type_gps: Vec<Vec<GenericParam>>,
    /// Method-owned parameters, indexed by `MethodDef` RID − 1 (length =
    /// `MethodDef` rows), so the member stage takes a method's parameters by
    /// position.
    pub(super) method_gps: Vec<Vec<GenericParam>>,
}

/// One `GenericParam` row's data plus the raw constraint tokens that target it.
struct GpRow {
    /// 1-based `GenericParam` RID, for the attribute lookup.
    rid: u32,
    number: u16,
    flags: u16,
    name: String,
    /// Raw `Constraint` coded tokens (`TypeDefOrRef`-or-`TypeSpec`) paired with
    /// the 1-based `GenericParamConstraint` RID, for the row's attribute lookup.
    constraints: Vec<(u32, u32)>,
}

/// Read every `GenericParam` row (§II.22.20), bucket it onto its owning type or
/// method, sort each owner's parameters by `Number`, and attach the typed
/// constraints (`GenericParamConstraint`, §II.22.21) and per-parameter custom
/// attributes.
pub(super) fn read_generic_params(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    attrs: &mut CustomAttributeIndex,
    type_count: usize,
) -> Result<GenericParams, Error> {
    let gp_count = tables.row_count(table::GENERIC_PARAM);
    let method_count = tables.row_count(table::METHOD_DEF) as usize;

    // Group constraints by their owning GenericParam RID (1-based), keeping each
    // constraint's own RID so its row attributes can be looked up when built.
    let mut constraints: Vec<Vec<(u32, u32)>> = vec![Vec::new(); gp_count as usize];
    for i in 0..tables.row_count(table::GENERIC_PARAM_CONSTRAINT) {
        let row = tables.row(table::GENERIC_PARAM_CONSTRAINT, i)?;
        let owner = checked_index(row.int(0), gp_count)? as usize;
        constraints[owner].push((row.coded(1), i + 1));
    }

    let mut type_rows: Vec<Vec<GpRow>> = (0..type_count).map(|_| Vec::new()).collect();
    let mut method_rows: Vec<Vec<GpRow>> = (0..method_count).map(|_| Vec::new()).collect();
    for i in 0..gp_count {
        let row = tables.row(table::GENERIC_PARAM, i)?;
        let Some(tok) = tables.decode_coded(Coded::TypeOrMethodDef, row.coded(2))? else {
            continue;
        };
        let gp = GpRow {
            rid: i + 1,
            number: row.int(0) as u16,
            flags: row.int(1) as u16,
            name: row.string(3)?.to_string(),
            constraints: std::mem::take(&mut constraints[i as usize]),
        };
        match tok.table {
            table::TYPE_DEF => type_rows[typedef_index(tok.rid, type_count)?].push(gp),
            table::METHOD_DEF => {
                method_rows[checked_index(tok.rid, method_count as u32)? as usize].push(gp)
            }
            // TypeOrMethodDef decodes only to TypeDef/MethodDef.
            _ => {}
        }
    }

    let mut type_gps = Vec::with_capacity(type_count);
    for rows in type_rows {
        type_gps.push(build_generic_params(md, tables, image_tables, attrs, rows)?);
    }
    let mut method_gps = Vec::with_capacity(method_count);
    for rows in method_rows {
        method_gps.push(build_generic_params(md, tables, image_tables, attrs, rows)?);
    }
    Ok(GenericParams {
        type_gps,
        method_gps,
    })
}

/// Sort one owner's `GenericParam` rows by `Number` and build each.
fn build_generic_params(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    attrs: &mut CustomAttributeIndex,
    mut rows: Vec<GpRow>,
) -> Result<Vec<GenericParam>, Error> {
    rows.sort_by_key(|gp| gp.number);
    rows.into_iter()
        .map(|gp| build_generic_param(md, tables, image_tables, attrs, gp))
        .collect()
}

fn build_generic_param(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    attrs: &mut CustomAttributeIndex,
    gp: GpRow,
) -> Result<GenericParam, Error> {
    let variance = match gp.flags & VARIANCE_MASK {
        COVARIANT => Variance::Covariant,
        CONTRAVARIANT => Variance::Contravariant,
        _ => Variance::Invariant,
    };
    let constraints = gp
        .constraints
        .into_iter()
        .map(|(tok, crid)| TypeConstraint {
            ty: decode_type_def_or_ref(md, tables, image_tables, tok),
            attributes: attrs.take(table::GENERIC_PARAM_CONSTRAINT, crid),
        })
        .collect();
    Ok(GenericParam {
        name: gp.name,
        variance,
        reference_type: gp.flags & REFERENCE_TYPE_CONSTRAINT != 0,
        value_type: gp.flags & VALUE_TYPE_CONSTRAINT != 0,
        default_ctor: gp.flags & DEFAULT_CTOR_CONSTRAINT != 0,
        allows_ref_struct: gp.flags & ALLOWS_REF_STRUCT != 0,
        constraints,
        attributes: attrs.take(table::GENERIC_PARAM, gp.rid),
    })
}

// ============================================================================
// Members
// ============================================================================

/// The members of one `TypeDef`.
pub(super) struct TypeMembers {
    pub(super) methods: Vec<Method>,
    pub(super) fields: Vec<Field>,
    pub(super) properties: Vec<Property>,
    pub(super) events: Vec<Event>,
}

/// Build the per-type member lists for every `TypeDef`. `method_gps` is consumed
/// (a method's parameters are moved out of it); `attrs` is drained of every
/// member/parameter/return position.
pub(super) fn read_members(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    method_starts: &[u32],
    attrs: &mut CustomAttributeIndex,
    method_gps: &mut [Vec<GenericParam>],
    type_count: usize,
) -> Result<Vec<TypeMembers>, Error> {
    let method_count = tables.row_count(table::METHOD_DEF);
    let field_count = tables.row_count(table::FIELD);
    let param_count = tables.row_count(table::PARAM);

    let field_starts = read_starts(tables, table::TYPE_DEF, 4)?; // FieldList
    validate_list_starts(&field_starts, field_count)?;
    let param_starts = read_starts(tables, table::METHOD_DEF, 5)?; // ParamList
    validate_list_starts(&param_starts, param_count)?;
    let param_ctx = ParamCtx {
        starts: param_starts,
        constants: read_constant_values(tables, table::PARAM)?,
        count: param_count,
    };
    let semantics = read_method_semantics(tables)?;
    let prop_ranges = read_map_ranges(tables, table::PROPERTY_MAP, table::PROPERTY, type_count)?;
    let event_ranges = read_map_ranges(tables, table::EVENT_MAP, table::EVENT, type_count)?;

    // `method_starts`/`field_starts`/`param_ctx.starts` are validated partitions
    // (see `validate_list_starts`), so every run `[start, end)` produced by
    // `run_range` lies within `1..=count` — the per-row `tables.row` reads need
    // no further clamping.
    let mut out = Vec::with_capacity(type_count);
    for ti in 0..type_count {
        let m_range = run_range(method_starts, ti, method_count);
        let mut methods = Vec::new();
        for rid in m_range.0..m_range.1 {
            methods.push(build_method(
                tables,
                image_tables,
                attrs,
                method_gps,
                &param_ctx,
                rid,
            )?);
        }

        let (f_start, f_end) = run_range(&field_starts, ti, field_count);
        let mut fields = Vec::new();
        for rid in f_start..f_end {
            fields.push(build_field(tables, image_tables, attrs, rid)?);
        }

        let mut properties = Vec::new();
        if let Some((p_start, p_end)) = prop_ranges[ti] {
            for rid in p_start..p_end {
                properties.push(build_property(
                    tables,
                    image_tables,
                    attrs,
                    &semantics,
                    m_range,
                    rid,
                )?);
            }
        }

        let mut events = Vec::new();
        if let Some((e_start, e_end)) = event_ranges[ti] {
            for rid in e_start..e_end {
                events.push(build_event(
                    md,
                    tables,
                    image_tables,
                    attrs,
                    &semantics,
                    m_range,
                    rid,
                )?);
            }
        }

        out.push(TypeMembers {
            methods,
            fields,
            properties,
            events,
        });
    }
    apply_method_impls(md, tables, image_tables, method_starts, &mut out)?;
    Ok(out)
}

/// Resolve the ECMA-335 `MethodImpl` table (§II.22.27) and stamp each
/// implementing method with its [`InterfaceMemberImpl`]s. A `MethodImpl` row
/// names a `Class` (the implementing type), a `MethodBody` (the implementing
/// method — a `MethodDef` in this module) and a `MethodDeclaration` (the member
/// it satisfies). We map the body back to the already-built [`Method`] via
/// [`method_owner`], validate it against the row's `Class`, and record the
/// declaration's implemented interface (decoded to a `TypeSig`) alongside the
/// declaration method's name and what `MethodSemantics` says that method is
/// ([`resolve_method_decl`]). For instance methods these rows are exactly the
/// *explicit* interface implementations; static interface members (no vtable
/// slot) are always wired through `MethodImpl`, so implicit static impls appear
/// too.
///
/// `MethodImpl` is also emitted for non-interface override redirections — most
/// notably C# covariant-return overrides, which map e.g. `Derived.Clone` to
/// `Base.Clone` with an ancestor *class* as the declaration parent. Those must
/// not be surfaced as implemented interface members, so each row is classified
/// by its *declaration* target ([`classify_decl_parent`]) — never by the body
/// method's name: the interface-qualified (`IFace.Member`) body name C#/F# emit
/// is a compiler convention, not a CLR rule (VB freely emits plain-named
/// bodies via `Implements`). A `Reference`-scoped declaration parent provable
/// neither way — an interface reachable only through an *external* interface's
/// inheritance (ordinary F#/VB output) is in-image identical to a covariant
/// override of a non-direct external ancestor — lands on
/// [`Method::unclassified_impls`] instead, raw, for a multi-assembly consumer.
///
/// Total over structurally-valid metadata: a row whose body is not a resolvable
/// `MethodDef` in this module, whose body does not belong to the row's `Class`,
/// or whose declaration is decidably not a modellable implementation, is
/// skipped rather than aborting. An implementation we cannot model is simply
/// not surfaced — the fail-soft contract for this optional enrichment — but a
/// structurally out-of-range row index is still an `Err` (the reader never
/// reads past a table).
fn apply_method_impls(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    method_starts: &[u32],
    out: &mut [TypeMembers],
) -> Result<(), Error> {
    if tables.row_count(table::METHOD_IMPL) == 0 {
        return Ok(());
    }
    // `Interface` coded tokens per implementing type, for the membership arm of
    // the declaration-parent classification; expanded transitively (and
    // memoized) per implementing class below.
    let interface_impls = read_interface_impls(tables, method_starts.len())?;
    let mut expanded: HashMap<u32, ClassContext> = HashMap::new();

    for i in 0..tables.row_count(table::METHOD_IMPL) {
        let row = tables.row(table::METHOD_IMPL, i)?;
        // Class (TypeDef simple index, 1-based). Only ever *compared* below,
        // but an index past the table is structurally malformed metadata, and
        // the structural contract is a hard error, never a silent skip.
        let class_idx = checked_index(row.int(0), tables.row_count(table::TYPE_DEF))?;
        let body = row.coded(1); // MethodBody (MethodDefOrRef)
        let decl = row.coded(2); // MethodDeclaration (MethodDefOrRef)

        // The body must be a MethodDef in this module to correspond to a Method
        // we built; a MemberRef body (a forwarded/imported impl) we cannot place.
        let Some(body_tok) = tables.decode_coded(Coded::MethodDefOrRef, body)? else {
            continue;
        };
        if body_tok.table != table::METHOD_DEF {
            continue;
        }
        let (TypeDefId(ti), MethodId(mi)) = method_owner(method_starts, body_tok.rid, tables)?;

        // The body must belong to the row's `Class`. ECMA-335 §II.22.27 as
        // written also permits a *base class* of `Class`, but the CoreCLR
        // loader does not: it requires the body's parent to be `Class` itself
        // (`methodtablebuilder.cpp`, "IMPLEMENTATION LIMITATION … the body of
        // a methodImpl [must] belong to the current type", else
        // `IDS_CLASSLOAD_MI_ILLEGAL_BODY`), so a foreign-bodied row cannot
        // load. Our model hangs the entry on the body's [`Method`], which
        // lives on the body's owner — honouring such a row would attribute the
        // implementation to a type the row does not name, so skip it.
        if ti != class_idx {
            continue;
        }

        let Some((iface_coded, member, classification)) =
            resolve_method_decl(md, tables, image_tables, method_starts, out, decl)?
        else {
            continue;
        };
        // An undecodable declaration parent cannot be classified at all.
        let Ok(decl_sig) = decode_type_def_or_ref(md, tables, image_tables, iface_coded) else {
            continue;
        };
        let ctx = match expanded.entry(ti) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => v.insert(class_decl_context(
                md,
                tables,
                image_tables,
                &interface_impls,
                ti as usize,
            )?),
        };
        let verdict = classify_decl_parent(md, tables, ctx, &decl_sig)?;
        if matches!(verdict, DeclParent::AncestorClass | DeclParent::NotAnImpl) {
            continue;
        }

        // Locate the built method; skip a row that maps to none we surfaced.
        let Some(m) = out
            .get_mut(ti as usize)
            .and_then(|tm| tm.methods.get_mut(mi as usize))
        else {
            continue;
        };

        // §II.22.27 requires an *instance* body to be virtual. A *static* body
        // is the static-abstract-interface-member shape (C#11 generic math:
        // `INumberBase<T>.IsCanonical`, checked operators, …), which postdates
        // that clause — the runtime requires the body to be static there. A
        // body that is neither, or that is a `.ctor`/`.cctor` (the
        // `rtspecialname` flag, not the name text), is malformed metadata, not
        // an explicit impl. (An *abstract* virtual body is fine: DIM
        // reabstraction and VB `MustOverride … Implements` both emit one.)
        if !(m.is_virtual || m.is_static) || m.is_rt_special_name {
            continue;
        }

        match verdict {
            // One entry per declaration owner: an accessor claimed by several
            // properties/events implements a member of each (see
            // [`DeclClassification`]); the ordinary shape is a single entry.
            DeclParent::ImplementedInterface => {
                for decl_semantics in classification.into_semantics() {
                    m.implements.push(InterfaceMemberImpl {
                        interface: Ok(decl_sig.clone()),
                        member: member.clone(),
                        decl: decl_semantics,
                    });
                }
            }
            // Undecidable in this image: surface the row raw for a
            // multi-assembly consumer (see [`Method::unclassified_impls`]).
            DeclParent::ExternalUnproven => {
                m.unclassified_impls.push(UnclassifiedImpl {
                    parent: decl_sig,
                    member,
                });
            }
            DeclParent::AncestorClass | DeclParent::NotAnImpl => unreachable!(),
        }
    }
    Ok(())
}

/// What the in-module metadata proves about the `TypeDef` at a given index,
/// for `MethodImpl` declaration classification. See [`class_decl_context`].
struct ClassContext {
    /// The `TypeDefOrRef` interface tokens provably implemented by the type —
    /// the in-module transitive interface closure.
    interfaces: Vec<ModifiedType>,
    /// The `Extends` targets that leave the module along the walkable
    /// ancestor chain — each a provable ancestor *class*, whose own further
    /// ancestry (and interface list) is invisible in this image.
    external_ancestors: Vec<ModifiedType>,
}

/// Bound on the interface-closure walk: the number of (definition,
/// instantiation) frames visited. Valid metadata is far smaller; a
/// pathological chain whose instantiations grow at every step (`I<T> :
/// J<I<T>>`-style, which the visited set cannot terminate because each frame
/// differs) stops expanding here — fail-soft, a partial closure, never an
/// abort.
const MAX_CLOSURE_FRAMES: usize = 256;

/// The declaration-classification context of the `TypeDef` at 0-based `idx`:
/// its provable interfaces as decoded, *instantiated* signatures, expanded
/// transitively through *in-module* edges — the type's own `InterfaceImpl`
/// rows, those of every in-module ancestor (the `Extends` chain), and those
/// of every in-module interface reachable so far — plus the external
/// `Extends` targets met along that walk. The CLR places a `MethodImpl`
/// declaration against the full *interface map* — direct,
/// interface-inherited, and base-class-inherited interfaces
/// (`methodtablebuilder.cpp` searches `bmtInterface->pInterfaceMap`) — so a
/// declaration parent found anywhere in this closure is proven to be an
/// interface even though it is absent from the class's direct rows (Roslyn
/// flattens the closure into the direct rows; F#, for one, does not).
///
/// Each walk frame carries the instantiation its definition was reached
/// under, and the definition's own `InterfaceImpl`/`Extends` signatures are
/// [`substitute`]d through it: `C : IDerived<int32>` walking `IDerived`1 :
/// IBase<!0>` contributes the *constructed* `IBase<int32>`, matching the
/// constructed declaration real F# emits (`interface IDerived<int> with
/// member _.M()` declares against `IBase<int32>` while listing only
/// `IDerived<int32>`). The implementing type's own frame is the identity —
/// its rows' `!n` refer to its own parameters, the same frame the
/// declaration uses.
///
/// The remaining gap is inherent to single-module reading: an interface
/// reachable only through an *external* type's interface list cannot be
/// traversed (ordinary F#/VB output produces this too). It fails soft: the
/// affected row is surfaced on [`Method::unclassified_impls`], never
/// misclassified.
fn class_decl_context(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    interface_impls: &[Vec<u32>],
    idx: usize,
) -> Result<ClassContext, Error> {
    // The in-module definition a signature's head resolves to, with the
    // instantiation to walk it under (empty for a non-generic head).
    // The modifier run on the position is not part of the head's *identity*, so
    // this reads `mt.ty` — and, because the run sits beside the type rather than
    // in front of it, a modified interface row still resolves to its definition
    // instead of going opaque. (No compiler puts a modifier here; getting it
    // right for free is the point of the encoding.)
    let in_module_head = |mt: &ModifiedType| -> Option<(u32, Vec<ModifiedType>)> {
        match &mt.ty {
            TypeSig::Named {
                scope: TypeScope::Definition(TypeDefId(d)),
                ..
            } => Some((*d, Vec::new())),
            TypeSig::Generic {
                scope: TypeScope::Definition(TypeDefId(d)),
                args,
                ..
            } => Some((*d, args.clone())),
            _ => None,
        }
    };

    let mut interfaces: Vec<ModifiedType> = Vec::new();
    let mut external_ancestors: Vec<ModifiedType> = Vec::new();
    // A frame is a definition plus the instantiation it was reached under
    // (`None` = the implementing type's own identity frame).
    let mut visited: Vec<(u32, Option<Vec<ModifiedType>>)> = Vec::new();
    let mut worklist: Vec<(u32, Option<Vec<ModifiedType>>)> = vec![(idx as u32, None)];
    while let Some((t, frame)) = worklist.pop() {
        if visited.iter().any(|v| *v == (t, frame.clone())) {
            continue;
        }
        if visited.len() >= MAX_CLOSURE_FRAMES {
            break;
        }
        visited.push((t, frame.clone()));
        for &tok in interface_impls.get(t as usize).map_or(&[][..], |v| v) {
            // An undecodable or unsubstitutable row contributes nothing —
            // fail-soft, the closure is merely smaller.
            let Ok(sig) = decode_type_def_or_ref(md, tables, image_tables, tok) else {
                continue;
            };
            let Some(instantiated) = substitute_capped(&sig, frame.as_deref()) else {
                continue;
            };
            if interfaces.contains(&instantiated) {
                continue;
            }
            // An in-module interface contributes its own base interfaces,
            // walked under this row's instantiation.
            if let Some((d, args)) = in_module_head(&instantiated) {
                worklist.push((d, Some(args)));
            }
            interfaces.push(instantiated);
        }
        // An in-module base class contributes its interfaces (and its own
        // base's, transitively), likewise instantiated; an external one ends
        // the walk and is recorded as a provable ancestor. `Extends` is
        // column 3 of the `TypeDef` row.
        let extends = tables.row(table::TYPE_DEF, t)?.coded(3);
        if extends != 0
            && let Ok(sig) = decode_type_def_or_ref(md, tables, image_tables, extends)
            && let Some(instantiated) = substitute_capped(&sig, frame.as_deref())
        {
            match in_module_head(&instantiated) {
                Some((d, args)) => worklist.push((d, Some(args))),
                None => external_ancestors.push(instantiated),
            }
        }
    }
    Ok(ClassContext {
        interfaces,
        external_ancestors,
    })
}

/// Node budget for one [`substitute`] result. Real instantiations are tens
/// of nodes; the cap exists for *hostile* metadata: an F-bounded chain like
/// `I<T> : I<Pair<T,T>>` doubles the instantiated tree per closure frame,
/// and the recursion-depth bound alone would not trip until the trees were
/// astronomically large — the depth of the *definition's row* stays
/// constant; the growth hides inside cloned replacement arguments. Exceeding
/// the budget drops the contribution, fail-soft, which also stops the
/// growing chain from enqueuing further frames.
const MAX_SUBSTITUTED_NODES: usize = 512;

/// [`substitute`] under a fresh [`MAX_SUBSTITUTED_NODES`] budget.
fn substitute_capped(mt: &ModifiedType, frame: Option<&[ModifiedType]>) -> Option<ModifiedType> {
    let mut budget = MAX_SUBSTITUTED_NODES;
    substitute(mt, frame, 0, &mut budget)
}

/// Instantiate `sig` under `frame`: each `!n` type parameter becomes
/// `frame[n]`, recursively through every containing position. A `None` frame
/// is the identity (the implementing type's own parameters stay as they
/// are); an out-of-range parameter index, a pathologically deep signature,
/// or a result exceeding `budget` nodes (every emitted node is charged, a
/// cloned replacement by its full [`sig_size`]) yields `None` — the caller
/// drops that contribution, fail-soft. Method parameters (`!!n`) cannot
/// appear in `InterfaceImpl`/`Extends` signatures but are left untouched if
/// met.
fn substitute(
    mt: &ModifiedType,
    frame: Option<&[ModifiedType]>,
    depth: u32,
    budget: &mut usize,
) -> Option<ModifiedType> {
    let Some(args) = frame else {
        return Some(mt.clone());
    };
    if depth >= EQUIV_MAX_DEPTH {
        return None;
    }
    // Charge the node *and its modifier run*: the run is cloned along with the
    // type, and this budget exists to bound exactly that cloning. (When a modifier
    // was a wrapper node, each one cost a unit on its own; a run must cost the
    // same, or a signature with a fat run at every position clones far more than
    // `MAX_SUBSTITUTED_NODES` implies while staying nominally within it.
    // `sig_size` charges runs for the same reason.)
    *budget = budget.checked_sub(1 + mt.mods.len())?;
    // The position's own run survives substitution — only the *type* is rewritten.
    let mods = || mt.mods.clone();
    Some(match &mt.ty {
        TypeSig::TypeVar(i) => {
            let replacement = args.get(*i as usize)?;
            // The clone reproduces the replacement's whole tree; charge it in
            // full (minus the node already charged above).
            *budget = budget.checked_sub(sig_size(replacement).saturating_sub(1))?;
            // Substituting a modified type into a modified position concatenates
            // the two runs — the position's modifiers stay outermost, exactly as
            // the wire would have them. (With modifiers as wrapper nodes this
            // happened implicitly, by nesting; here it is written down.)
            let mut mods = mods();
            mods.extend(replacement.mods.iter().copied());
            ModifiedType {
                mods,
                ty: replacement.ty.clone(),
            }
        }
        TypeSig::Generic {
            kind,
            scope,
            args: inner,
        } => ModifiedType {
            mods: mods(),
            ty: TypeSig::Generic {
                kind: *kind,
                scope: *scope,
                args: inner
                    .iter()
                    .map(|a| substitute(a, frame, depth + 1, budget))
                    .collect::<Option<Vec<_>>>()?,
            },
        },
        TypeSig::SzArray(inner) => ModifiedType {
            mods: mods(),
            ty: TypeSig::SzArray(Box::new(substitute(inner, frame, depth + 1, budget)?)),
        },
        TypeSig::ByRef(inner) => ModifiedType {
            mods: mods(),
            ty: TypeSig::ByRef(Box::new(substitute(inner, frame, depth + 1, budget)?)),
        },
        TypeSig::Array {
            element,
            rank,
            sizes,
            lower_bounds,
        } => ModifiedType {
            mods: mods(),
            ty: TypeSig::Array {
                element: Box::new(substitute(element, frame, depth + 1, budget)?),
                rank: *rank,
                sizes: sizes.clone(),
                lower_bounds: lower_bounds.clone(),
            },
        },
        TypeSig::Ptr(inner) => ModifiedType {
            mods: mods(),
            ty: TypeSig::Ptr(match inner {
                Some(p) => Some(Box::new(substitute(p, frame, depth + 1, budget)?)),
                None => None,
            }),
        },
        TypeSig::Primitive(_)
        | TypeSig::Named { .. }
        | TypeSig::MethodVar(_)
        | TypeSig::TypedByRef => mt.clone(),
    })
}

/// The node count of a decoded signature tree. Inputs are finite (decoded
/// from bounded blobs, or themselves budget-capped substitution results), so
/// plain recursion suffices.
fn sig_size(mt: &ModifiedType) -> usize {
    // The modifier run is charged too: it is cloned along with the type, and a
    // hostile blob can put `MAX_DEPTH` of them at every position.
    1 + mt.mods.len()
        + match &mt.ty {
            TypeSig::Generic { args, .. } => args.iter().map(sig_size).sum(),
            TypeSig::SzArray(inner) | TypeSig::ByRef(inner) => sig_size(inner),
            TypeSig::Array { element, .. } => sig_size(element),
            TypeSig::Ptr(Some(inner)) => sig_size(inner),
            TypeSig::Primitive(_)
            | TypeSig::Named { .. }
            | TypeSig::TypeVar(_)
            | TypeSig::MethodVar(_)
            | TypeSig::Ptr(None)
            | TypeSig::TypedByRef => 0,
        }
}

/// How a `MethodImpl` declaration parent relates to the implementing type —
/// the classification separating an explicit interface implementation from an
/// override redirection (a covariant-return override's declaration parent is
/// an ancestor class) and from the in-image-undecidable remainder.
enum DeclParent {
    /// A proven member of the type's interface map (§II.22.27's interface
    /// tree): the row is an explicit/static interface implementation.
    ImplementedInterface,
    /// Structurally equal to a known `Extends` target along the walkable
    /// ancestor chain: an override redirection, decidably *not* an interface
    /// implementation.
    AncestorClass,
    /// A `Reference`-scoped parent that is neither provable — either an
    /// interface reachable only through an *external* interface's inheritance
    /// (ordinary F#/VB output) or an ancestor class beyond the first external
    /// hop (a C# covariant-return override targets the original declarer).
    /// In-image the two are identical; the caller surfaces the row
    /// unclassified.
    ExternalUnproven,
    /// Decidably not an implementation the model can carry: a local
    /// non-interface parent, a local interface outside the type's interface
    /// map (unloadable metadata), or a non-named parent.
    NotAnImpl,
}

/// Classify a `MethodImpl` declaration parent (`decl_sig`, decoded) against
/// `ctx`, the implementing type's [`ClassContext`] — the §II.22.27
/// requirement that a declaration sit on `Class`'s ancestor chain or
/// interface tree, evaluated on in-module evidence only:
///
/// - Structural membership in `ctx.interfaces` (the in-module transitive,
///   *instantiated* closure, mirroring the CLR's interface map) proves an
///   implemented interface. For an in-module `TypeDef` parent the `Interface`
///   flag is additionally required — authoritative, and free. Roslyn flattens
///   the closure of declared interfaces into the direct rows (verified
///   empirically: `class Foo : IEnumerable<int>` lists the plain
///   `IEnumerable` too), so a compiler-emitted explicit impl of a *directly
///   declared* external interface is always found here.
/// - Equivalence with a member of `ctx.external_ancestors` proves an ancestor
///   class (the covariant-return-override shape on a direct external base).
/// - A `Reference`-scoped parent proving neither is undecidable in this
///   image ([`DeclParent::ExternalUnproven`]).
///
/// Comparison is structural ([`sigs_equivalent`], which also identifies
/// *duplicate* `TypeRef` rows naming the same type), so a non-deduplicated
/// `TypeSpec` or `TypeRef` still matches.
fn classify_decl_parent(
    md: &MetadataFile,
    tables: &Tables,
    ctx: &ClassContext,
    decl_sig: &ModifiedType,
) -> Result<DeclParent, Error> {
    // The head, not the modifier run: a modifier does not change *which* type a
    // declaration parent names. (It cannot hide the head here — it sits beside
    // it — which is the encoding doing the work a peel would otherwise have to.)
    let scope = match &decl_sig.ty {
        TypeSig::Named { scope, .. } | TypeSig::Generic { scope, .. } => *scope,
        // A non-named parent (an array, a type parameter, …) names no interface.
        _ => return Ok(DeclParent::NotAnImpl),
    };
    if let TypeScope::Definition(TypeDefId(index)) = scope {
        let row = tables.row(table::TYPE_DEF, index)?;
        if row.int(0) & INTERFACE_FLAG == 0 {
            return Ok(DeclParent::NotAnImpl);
        }
    }
    for sig in &ctx.interfaces {
        if sigs_equivalent(md, tables, sig, decl_sig, 0)? {
            return Ok(DeclParent::ImplementedInterface);
        }
    }
    match scope {
        // A local interface outside the type's interface map: the CLR
        // resolves declarations against the computed map, so this cannot
        // load, and the flag-proven interface-ness is beside the point.
        TypeScope::Definition(_) => Ok(DeclParent::NotAnImpl),
        TypeScope::Reference(_) => {
            for sig in &ctx.external_ancestors {
                if sigs_equivalent(md, tables, sig, decl_sig, 0)? {
                    return Ok(DeclParent::AncestorClass);
                }
            }
            Ok(DeclParent::ExternalUnproven)
        }
    }
}

/// Maximum nesting depth for the identity walks below (generic arguments,
/// nested-`TypeRef` chains, resolution scopes). Valid metadata nests far
/// shallower; the bound turns pathological or cyclic input into `false`
/// (fail-soft), never an abort or unbounded recursion.
const EQUIV_MAX_DEPTH: u32 = 64;

/// Structural equality of two decoded [`TypeSig`]s modulo *duplicate*
/// `TypeRef` rows: `Reference` scopes compare by the referenced type's
/// identity — resolution scope, namespace, name ([`typerefs_equivalent`]) —
/// rather than by row index, because ECMA-335 permits (and IL weavers produce)
/// several `TypeRef` rows naming the same type, and `InterfaceImpl` and
/// `MethodImpl` may each reach an interface through a different one. The
/// class-vs-valuetype `kind` hint is ignored: a bare `TypeDefOrRef` token
/// carries none, and the underlying row is the identity.
fn sigs_equivalent(
    md: &MetadataFile,
    tables: &Tables,
    a: &ModifiedType,
    b: &ModifiedType,
    depth: u32,
) -> Result<bool, Error> {
    if depth >= EQUIV_MAX_DEPTH {
        return Ok(false);
    }
    if !mods_equivalent(md, tables, &a.mods, &b.mods, depth)? {
        return Ok(false);
    }
    types_equivalent(md, tables, &a.ty, &b.ty, depth)
}

/// Two modifier runs are equivalent when they are pairwise equivalent in order:
/// same `required` bit, and modifier types that name the same type (so a
/// duplicate `TypeRef` row does not split them — see [`scopes_equivalent`]).
fn mods_equivalent(
    md: &MetadataFile,
    tables: &Tables,
    a: &[CustomMod],
    b: &[CustomMod],
    depth: u32,
) -> Result<bool, Error> {
    if a.len() != b.len() {
        return Ok(false);
    }
    for (x, y) in a.iter().zip(b) {
        if x.required != y.required
            || !scopes_equivalent(md, tables, x.modifier, y.modifier, depth)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// See [`sigs_equivalent`] — the type proper, with the position's modifier run
/// already compared by the caller. Every recursive slot is a position, so it
/// goes back through [`sigs_equivalent`].
fn types_equivalent(
    md: &MetadataFile,
    tables: &Tables,
    a: &TypeSig,
    b: &TypeSig,
    depth: u32,
) -> Result<bool, Error> {
    if depth >= EQUIV_MAX_DEPTH {
        return Ok(false);
    }
    match (a, b) {
        (TypeSig::Primitive(x), TypeSig::Primitive(y)) => Ok(x == y),
        (TypeSig::Named { scope: sa, .. }, TypeSig::Named { scope: sb, .. }) => {
            scopes_equivalent(md, tables, *sa, *sb, depth)
        }
        (
            TypeSig::Generic {
                scope: sa,
                args: xa,
                ..
            },
            TypeSig::Generic {
                scope: sb,
                args: xb,
                ..
            },
        ) => {
            if xa.len() != xb.len() || !scopes_equivalent(md, tables, *sa, *sb, depth)? {
                return Ok(false);
            }
            for (x, y) in xa.iter().zip(xb) {
                if !sigs_equivalent(md, tables, x, y, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (TypeSig::TypeVar(x), TypeSig::TypeVar(y))
        | (TypeSig::MethodVar(x), TypeSig::MethodVar(y)) => Ok(x == y),
        (TypeSig::SzArray(x), TypeSig::SzArray(y)) | (TypeSig::ByRef(x), TypeSig::ByRef(y)) => {
            sigs_equivalent(md, tables, x, y, depth + 1)
        }
        (
            TypeSig::Array {
                element: ea,
                rank: ra,
                sizes: za,
                lower_bounds: la,
            },
            TypeSig::Array {
                element: eb,
                rank: rb,
                sizes: zb,
                lower_bounds: lb,
            },
        ) => {
            Ok(ra == rb && za == zb && la == lb && sigs_equivalent(md, tables, ea, eb, depth + 1)?)
        }
        (TypeSig::Ptr(None), TypeSig::Ptr(None)) => Ok(true),
        (TypeSig::Ptr(Some(x)), TypeSig::Ptr(Some(y))) => {
            sigs_equivalent(md, tables, x, y, depth + 1)
        }
        (TypeSig::TypedByRef, TypeSig::TypedByRef) => Ok(true),
        _ => Ok(false),
    }
}

/// See [`sigs_equivalent`]: `Definition`s compare by row (a `TypeDef` is its
/// own identity); `Reference`s by the referenced type's identity. A
/// `Definition` never equates to a `Reference` — a module-self `TypeRef` alias
/// *can* name an in-module `TypeDef`, but resolving that is a name-resolution
/// concern (see `RefScope::Module`); treating them as distinct is the
/// fail-soft direction.
fn scopes_equivalent(
    md: &MetadataFile,
    tables: &Tables,
    a: TypeScope,
    b: TypeScope,
    depth: u32,
) -> Result<bool, Error> {
    match (a, b) {
        (TypeScope::Definition(x), TypeScope::Definition(y)) => Ok(x == y),
        (TypeScope::Reference(TypeRefId(x)), TypeScope::Reference(TypeRefId(y))) => {
            typerefs_equivalent(md, tables, x, y, depth + 1)
        }
        _ => Ok(false),
    }
}

/// Whether two `TypeRef` rows (0-based indices) name the same type: equal
/// rows, or equal `Namespace`/`Name` strings under equivalent resolution
/// scopes.
fn typerefs_equivalent(
    md: &MetadataFile,
    tables: &Tables,
    a: u32,
    b: u32,
    depth: u32,
) -> Result<bool, Error> {
    if a == b {
        return Ok(true);
    }
    if depth >= EQUIV_MAX_DEPTH {
        return Ok(false);
    }
    let ra = tables.row(table::TYPE_REF, a)?;
    let rb = tables.row(table::TYPE_REF, b)?;
    Ok(ra.string(1)? == rb.string(1)?
        && ra.string(2)? == rb.string(2)?
        && resolution_scopes_equivalent(md, tables, ra.coded(0), rb.coded(0), depth)?)
}

/// Whether two `ResolutionScope` coded values anchor the same scope: equal
/// tokens; two `TypeRef` enclosers naming the same type; two `AssemblyRef`s
/// with the same identity; or both the current module. Mixed or unresolvable
/// forms compare unequal (fail-soft).
fn resolution_scopes_equivalent(
    md: &MetadataFile,
    tables: &Tables,
    a: u32,
    b: u32,
    depth: u32,
) -> Result<bool, Error> {
    if a == b {
        return Ok(true);
    }
    let (Some(ta), Some(tb)) = (
        tables.decode_coded(Coded::ResolutionScope, a)?,
        tables.decode_coded(Coded::ResolutionScope, b)?,
    ) else {
        return Ok(false);
    };
    match (ta.table, tb.table) {
        (table::TYPE_REF, table::TYPE_REF) => {
            typerefs_equivalent(md, tables, ta.rid - 1, tb.rid - 1, depth + 1)
        }
        (table::ASSEMBLY_REF, table::ASSEMBLY_REF) => {
            assembly_refs_equivalent(md, tables, ta.rid - 1, tb.rid - 1)
        }
        (table::MODULE, table::MODULE) => Ok(true),
        _ => Ok(false),
    }
}

/// Whether two `AssemblyRef` rows (0-based indices) name the same assembly:
/// equal version numbers, name, culture, and `PublicKeyOrToken` blob. `Flags`
/// and `HashValue` are not identity. A full-public-key row and a token row for
/// the same assembly compare unequal (fail-soft; no hashing here).
fn assembly_refs_equivalent(
    md: &MetadataFile,
    tables: &Tables,
    a: u32,
    b: u32,
) -> Result<bool, Error> {
    let ra = tables.row(table::ASSEMBLY_REF, a)?;
    let rb = tables.row(table::ASSEMBLY_REF, b)?;
    Ok((0..=3).all(|c| ra.int(c) == rb.int(c))
        && ra.string(6)? == rb.string(6)?
        && ra.string(7)? == rb.string(7)?
        && md.blob_at(ra.int(5))? == md.blob_at(rb.int(5))?)
}

/// What a `MethodImpl` declaration method *is*, with every owner when it is
/// an accessor. The per-entry [`DeclSemantics`] is derived from this by
/// [`apply_method_impls`], which emits one [`InterfaceMemberImpl`] per owner —
/// a declaration claimed by several properties/events yields several entries.
enum DeclClassification {
    /// The declaration is an accessor; one element per owning property/event,
    /// in [`accessor_owners`] order. Never empty (an empty owner list is
    /// [`Self::OrdinaryMethod`]).
    Accessors(Vec<(AccessorOwner, String)>),
    /// A resolved in-module `MethodDef` no `MethodSemantics` row claims.
    OrdinaryMethod,
    /// A declaration that could not be followed to an in-module `MethodDef`.
    Unresolved,
}

impl DeclClassification {
    /// Classify the in-module method at `local` by its `MethodSemantics`
    /// associations within `tm`.
    fn of_local(tm: &TypeMembers, local: MethodId) -> Self {
        let owners = accessor_owners(tm, local);
        if owners.is_empty() {
            DeclClassification::OrdinaryMethod
        } else {
            DeclClassification::Accessors(
                owners
                    .into_iter()
                    .map(|(kind, name)| (kind, name.to_string()))
                    .collect(),
            )
        }
    }

    /// The per-entry semantics this classification expands to, in owner order.
    fn into_semantics(self) -> Vec<DeclSemantics> {
        match self {
            DeclClassification::Accessors(owners) => owners
                .into_iter()
                .map(|(kind, name)| DeclSemantics::Accessor(kind, name))
                .collect(),
            DeclClassification::OrdinaryMethod => vec![DeclSemantics::OrdinaryMethod],
            DeclClassification::Unresolved => vec![DeclSemantics::Unresolved],
        }
    }
}

/// Resolve a `MethodImpl.MethodDeclaration` `MethodDefOrRef` token into the
/// implemented interface (as a `TypeDefOrRef`-coded value, ready for
/// [`decode_type_def_or_ref`]), the declaration method's verbatim `Name`, and
/// what that method *is* ([`DeclClassification`]) — or `None` when the token
/// names no resolvable type.
///
/// The declaration is classified against `MethodSemantics` whenever the
/// declaring type is *in this module* — that linkage, not the name text, is
/// what makes a method an accessor. A `MethodDef` declaration always is; a
/// `MemberRef` declaration is whenever its parent resolves to an in-module
/// `TypeDef` (the shape every compiler emits for an explicit impl of a
/// same-assembly *generic* interface: no `MethodDef` token can name an
/// instantiation, so the declaration must go through a `TypeSpec` `MemberRef`).
/// Only a declaration we cannot follow to a local `MethodDef` — one in another
/// assembly, or one whose name/signature matches no method of the resolved type
/// — is [`DeclClassification::Unresolved`], and the projection surfaces its raw
/// `Name` marked as such.
fn resolve_method_decl(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    method_starts: &[u32],
    members: &[TypeMembers],
    decl: u32,
) -> Result<Option<(u32, String, DeclClassification)>, Error> {
    let Some(tok) = tables.decode_coded(Coded::MethodDefOrRef, decl)? else {
        return Ok(None);
    };
    if tok.table == table::MEMBER_REF {
        // The usual case: the declaration is a `MemberRef` whose `Class` is the
        // (often generic-instantiated) interface and whose `Name` is the member.
        // `Class` is a `MemberRefParent` coded index; re-encode it as a
        // `TypeDefOrRef` for the shared type decoder.
        let mr = tables.row(table::MEMBER_REF, tok.rid - 1)?;
        let member = mr.string(1)?.to_string(); // Name
        let sig = mr.int(2); // Signature (blob index)
        let Some(parent) = tables.decode_coded(Coded::MemberRefParent, mr.coded(0))? else {
            return Ok(None);
        };
        let Some(coded) = type_def_or_ref_coded(parent.table, parent.rid) else {
            return Ok(None);
        };
        let semantics = member_ref_decl_semantics(
            md,
            tables,
            image_tables,
            method_starts,
            members,
            MemberRefDecl {
                parent_coded: coded,
                name: &member,
                sig,
            },
        )?;
        Ok(Some((coded, member, semantics)))
    } else if tok.table == table::METHOD_DEF {
        // A `MethodDef` declaration names a non-generic interface in this
        // module: the declaration's owning type is the interface.
        let (TypeDefId(d), decl_local) = method_owner(method_starts, tok.rid, tables)?;
        let member = tables
            .row(table::METHOD_DEF, tok.rid - 1)?
            .string(3)? // Name
            .to_string();
        let semantics = members
            .get(d as usize)
            .map(|tm| DeclClassification::of_local(tm, decl_local))
            .unwrap_or(DeclClassification::OrdinaryMethod);
        // `TypeDef` rid is 1-based, so the row index `d` re-encodes as `d + 1`.
        Ok(type_def_or_ref_coded(table::TYPE_DEF, d + 1).map(|coded| (coded, member, semantics)))
    } else {
        Ok(None)
    }
}

/// The parts of a `MemberRef` row a `MethodImpl` declaration is classified by:
/// its `Class` re-encoded as a `TypeDefOrRef` coded value, and the referenced
/// member's `Name` and `Signature` blob index.
struct MemberRefDecl<'a> {
    parent_coded: u32,
    name: &'a str,
    sig: u32,
}

/// Classify a `MemberRef` `MethodImpl` declaration.
///
/// A `MemberRef` is not automatically foreign: its parent may be an in-module
/// `TypeDef`, either named directly or as the head of a generic-instantiation
/// `TypeSpec`. That is exactly how an explicit impl of a same-assembly generic
/// interface must be spelled, since a `MethodDef` token cannot name an
/// instantiation. Where the parent is in-module we can — and so must — read the
/// real `MethodSemantics` rather than guess from the name.
///
/// The referent is the type's own method matching both `Name` and `Signature`.
/// ECMA-335 §II.22.25 requires a `MemberRef` signature to be the one from the
/// *definition* (generic parameters left as `VAR`, not substituted with the
/// instantiation's arguments), so the blobs compare byte-for-byte; the name
/// alone would be ambiguous under overloading. A referent we cannot pin down —
/// an external parent, or no matching method — is [`DeclClassification::Unresolved`]:
/// surfaced as such with its raw name, never misclassified.
fn member_ref_decl_semantics(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    method_starts: &[u32],
    members: &[TypeMembers],
    decl: MemberRefDecl<'_>,
) -> Result<DeclClassification, Error> {
    let owner =
        match decode_type_def_or_ref(md, tables, image_tables, decl.parent_coded).map(|mt| mt.ty) {
            Ok(TypeSig::Named { scope, .. }) | Ok(TypeSig::Generic { scope, .. }) => match scope {
                TypeScope::Definition(TypeDefId(d)) => d,
                TypeScope::Reference(_) => return Ok(DeclClassification::Unresolved),
            },
            _ => return Ok(DeclClassification::Unresolved),
        };
    let Some(local) = local_method_by_name_and_sig(
        md,
        tables,
        image_tables,
        method_starts,
        owner,
        decl.name,
        decl.sig,
    )?
    else {
        return Ok(DeclClassification::Unresolved);
    };
    Ok(members
        .get(owner as usize)
        .map(|tm| DeclClassification::of_local(tm, local))
        .unwrap_or(DeclClassification::OrdinaryMethod))
}

/// The [`MethodId`] (index within its type's method run) of the `MethodDef`
/// owned by the `TypeDef` at 0-based `owner` whose `Name` is `name` and whose
/// `Signature` denotes the same method signature as the blob at `sig` —
/// byte-identical (the fast path; same-module compiler output interns blobs
/// and reuses tokens) or semantically equivalent
/// ([`method_sigs_equivalent`], which tolerates *duplicate* `TypeRef` rows
/// naming the same type inside the signature, the way the CLR's `MemberRef`
/// resolution compares signatures by resolved identity rather than bytes).
/// `None` when no method matches; the first match wins (valid metadata has at
/// most one, since name plus signature identifies a method).
fn local_method_by_name_and_sig(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    method_starts: &[u32],
    owner: u32,
    name: &str,
    sig: u32,
) -> Result<Option<MethodId>, Error> {
    let Some(&start) = method_starts.get(owner as usize) else {
        return Ok(None);
    };
    // The run ends where the next type's begins; the last type runs to the end
    // of the table. `MethodDef` rids are 1-based, so the exclusive end of the
    // whole table is `row_count + 1`.
    let end = method_starts
        .get(owner as usize + 1)
        .copied()
        .unwrap_or(tables.row_count(table::METHOD_DEF) + 1);
    let wanted = md.blob_at(sig)?;
    for rid in start..end {
        let row = tables.row(table::METHOD_DEF, rid - 1)?;
        if row.string(3)? != name {
            continue;
        }
        let candidate = md.blob_at(row.int(4))?;
        if candidate == wanted
            || method_sigs_equivalent(md, tables, image_tables, candidate, wanted)?
        {
            return Ok(Some(MethodId(rid - start)));
        }
    }
    Ok(None)
}

/// Whether two method-signature blobs denote the same method signature: the
/// same shape (this-ness, calling convention and generic arity, parameter
/// count) with pairwise-equivalent return and parameter types
/// ([`sigs_equivalent`], so duplicate `TypeRef` rows naming the same type
/// compare equal). A blob that does not decode within the supported subset
/// compares unequal — fail-soft, and the byte-equality fast path has already
/// run by then, so an undecodable-but-identical pair still matched.
fn method_sigs_equivalent(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    a: &[u8],
    b: &[u8],
) -> Result<bool, Error> {
    let (Ok(da), Ok(db)) = (
        decode_method_sig(a, image_tables),
        decode_method_sig(b, image_tables),
    ) else {
        return Ok(false);
    };
    if da.has_this != db.has_this
        || da.explicit_this != db.explicit_this
        || da.calling_convention != db.calling_convention
        || da.param_types.len() != db.param_types.len()
    {
        return Ok(false);
    }
    match (&da.return_type, &db.return_type) {
        (RetType::Void(x), RetType::Void(y)) => {
            if !mods_equivalent(md, tables, x, y, 0)? {
                return Ok(false);
            }
        }
        (RetType::Type(x), RetType::Type(y)) => {
            if !sigs_equivalent(md, tables, x, y, 0)? {
                return Ok(false);
            }
        }
        _ => return Ok(false),
    }
    for (x, y) in da.param_types.iter().zip(&db.param_types) {
        if !sigs_equivalent(md, tables, x, y, 0)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The kind and name of every property and event — per the `MethodSemantics`
/// linkage resolved into `tm` — that claims the method at `id` (within the
/// type's method run) as one of its accessors, in properties-then-events
/// declaration order. Every semantics role counts: get/set/add/remove/raise
/// *and* the open-ended `Other` (§II.22.28 makes each row an authoritative
/// association, whatever the role). Usually zero entries (an ordinary method)
/// or one; `MethodSemantics` does not make `Method` unique, so hand-emitted
/// IL may attach one method to several owners, and all of them are returned.
fn accessor_owners(tm: &TypeMembers, id: MethodId) -> Vec<(AccessorOwner, &str)> {
    let hit = |acc: Option<MethodId>| acc == Some(id);
    let other_hit = |others: &[Option<MethodId>]| others.contains(&Some(id));
    let mut out: Vec<(AccessorOwner, &str)> = Vec::new();
    for p in &tm.properties {
        if hit(p.getter) || hit(p.setter) || other_hit(&p.other_accessors) {
            out.push((AccessorOwner::Property, &p.name));
        }
    }
    for e in &tm.events {
        if hit(e.add) || hit(e.remove) || hit(e.raise) || other_hit(&e.other_accessors) {
            out.push((AccessorOwner::Event, &e.name));
        }
    }
    out
}

/// Re-encode a `(table, 1-based rid)` type token as a `TypeDefOrRef` coded value
/// (2 tag bits: 0=TypeDef, 1=TypeRef, 2=TypeSpec), or `None` if the table is not
/// one of the three a `TypeDefOrRef` can name.
fn type_def_or_ref_coded(tbl: usize, rid: u32) -> Option<u32> {
    let tag = if tbl == table::TYPE_DEF {
        0
    } else if tbl == table::TYPE_REF {
        1
    } else if tbl == table::TYPE_SPEC {
        2
    } else {
        return None;
    };
    Some((rid << 2) | tag)
}

/// The 1-based RID run `[start, end)` of the member list owned by entry `idx`,
/// from a non-decreasing `start`-per-owner array: the next owner's start, or
/// `total + 1` for the last owner.
fn run_range(starts: &[u32], idx: usize, total: u32) -> (u32, u32) {
    let start = starts[idx];
    let end = starts.get(idx + 1).copied().unwrap_or(total + 1);
    (start, end)
}

/// Read column `col` (a simple-index "list start" RID) of every row of `table`.
fn read_starts(tables: &Tables, table: usize, col: usize) -> Result<Vec<u32>, Error> {
    let count = tables.row_count(table);
    let mut starts = Vec::with_capacity(count as usize);
    for i in 0..count {
        starts.push(tables.row(table, i)?.int(col));
    }
    Ok(starts)
}

/// Validate a member-list "start RID" array (`TypeDef.MethodList`/`FieldList`,
/// `MethodDef.ParamList`). The runs `[start_i, start_{i+1})` must partition the
/// member table `1..=count`, so the starts begin at 1, are non-decreasing, and
/// stay within `1..=count+1` (`count+1` is the empty-list-at-the-end sentinel).
/// An out-of-range or out-of-order start would otherwise drop or misattribute
/// rows while `read_types` still succeeded; it is refused loudly instead of
/// clamped into a valid-looking range.
pub(super) fn validate_list_starts(starts: &[u32], count: u32) -> Result<(), Error> {
    let limit = count + 1;
    let mut prev = 1;
    for (i, &start) in starts.iter().enumerate() {
        if start > limit || start < prev || (i == 0 && start != 1) {
            return Err(Error::TableIndexOutOfRange);
        }
        prev = start;
    }
    Ok(())
}

/// The `Param`-table context for pairing a method's signature parameter types
/// with their row metadata.
struct ParamCtx {
    /// `ParamList` start RID per `MethodDef` (index = `MethodDef` RID − 1).
    starts: Vec<u32>,
    /// Decoded `Constant` default value per `Param` RID that owns one.
    constants: HashMap<u32, Constant>,
    /// `Param` table row count.
    count: u32,
}

fn build_method(
    tables: &Tables,
    image_tables: &ImageTables,
    attrs: &mut CustomAttributeIndex,
    method_gps: &mut [Vec<GenericParam>],
    param_ctx: &ParamCtx,
    rid: u32,
) -> Result<Method, Error> {
    let row = tables.row(table::METHOD_DEF, rid - 1)?;
    let flags = row.int(2); // Flags
    let name = row.string(3)?.to_string(); // Name
    let decoded = decode_method_sig(row.blob(4)?, image_tables); // Signature

    let accessibility = fold_member_access(flags);
    let generic_params = std::mem::take(&mut method_gps[(rid - 1) as usize]);
    let signature = match decoded {
        Ok(d) => Ok(assemble_method_sig(
            tables,
            attrs,
            param_ctx,
            (rid - 1) as usize,
            d,
        )?),
        Err(e) => Err(e),
    };
    Ok(Method {
        // `MethodDef` token: table tag 0x06 in the high byte, 1-based rid below.
        token: 0x0600_0000 | rid,
        name,
        accessibility,
        is_static: flags & METHOD_STATIC != 0,
        is_abstract: flags & METHOD_ABSTRACT != 0,
        is_virtual: flags & METHOD_VIRTUAL != 0,
        is_final: flags & METHOD_FINAL != 0,
        is_new_slot: flags & METHOD_NEW_SLOT != 0,
        is_hide_by_sig: flags & METHOD_HIDE_BY_SIG != 0,
        is_rt_special_name: flags & METHOD_RT_SPECIAL_NAME != 0,
        generic_params,
        signature,
        attributes: attrs.take(table::METHOD_DEF, rid),
        // Both filled by `apply_method_impls` after all type member runs are
        // built; the `MethodImpl` table is keyed by `MethodDef` rid, so it is
        // resolved in one post-pass rather than per-method here.
        implements: Vec::new(),
        unclassified_impls: Vec::new(),
    })
}

/// One `Param` row's metadata.
struct ParamRow {
    rid: u32,
    flags: u32,
    name: Option<String>,
}

/// Pair the decoded signature's parameter types with the `Param`-table metadata,
/// keyed by sequence number (0 = return, 1..N = parameters). Parameters with no
/// `Param` row keep their type but carry no name/flags.
fn assemble_method_sig(
    tables: &Tables,
    attrs: &mut CustomAttributeIndex,
    param_ctx: &ParamCtx,
    method_idx: usize,
    d: DecodedMethodSig,
) -> Result<MethodSig, Error> {
    let (p_start, p_end) = run_range(&param_ctx.starts, method_idx, param_ctx.count);
    let mut by_seq: HashMap<u16, ParamRow> = HashMap::new();
    for rid in p_start..p_end {
        let row = tables.row(table::PARAM, rid - 1)?;
        let name = row.string(2)?; // Name
        by_seq.insert(
            row.int(1) as u16, // Sequence
            ParamRow {
                rid,
                flags: row.int(0), // Flags
                name: (!name.is_empty()).then(|| name.to_string()),
            },
        );
    }

    let return_attributes = match by_seq.remove(&0) {
        Some(r) => attrs.take(table::PARAM, r.rid),
        None => Vec::new(),
    };
    let mut parameters = Vec::with_capacity(d.param_types.len());
    for (i, ty) in d.param_types.into_iter().enumerate() {
        let seq = (i + 1) as u16;
        parameters.push(match by_seq.remove(&seq) {
            Some(r) => Param {
                name: r.name,
                ty,
                is_in: r.flags & PARAM_IN != 0,
                is_out: r.flags & PARAM_OUT != 0,
                optional: r.flags & PARAM_OPTIONAL != 0,
                default_value: param_ctx.constants.get(&r.rid).cloned(),
                attributes: attrs.take(table::PARAM, r.rid),
            },
            None => Param {
                name: None,
                ty,
                is_in: false,
                is_out: false,
                optional: false,
                default_value: None,
                attributes: Vec::new(),
            },
        });
    }
    Ok(MethodSig {
        has_this: d.has_this,
        explicit_this: d.explicit_this,
        calling_convention: d.calling_convention,
        return_type: d.return_type,
        return_attributes,
        parameters,
    })
}

fn build_field(
    tables: &Tables,
    image_tables: &ImageTables,
    attrs: &mut CustomAttributeIndex,
    rid: u32,
) -> Result<Field, Error> {
    let row = tables.row(table::FIELD, rid - 1)?;
    let flags = row.int(0); // Flags
    let name = row.string(1)?.to_string(); // Name
    let signature = decode_field_sig(row.blob(2)?, image_tables); // Signature
    Ok(Field {
        name,
        accessibility: fold_member_access(flags),
        is_static: flags & FIELD_STATIC != 0,
        is_literal: flags & FIELD_LITERAL != 0,
        is_init_only: flags & FIELD_INIT_ONLY != 0,
        signature,
        attributes: attrs.take(table::FIELD, rid),
    })
}

fn build_property(
    tables: &Tables,
    image_tables: &ImageTables,
    attrs: &mut CustomAttributeIndex,
    semantics: &Semantics,
    m_range: (u32, u32),
    rid: u32,
) -> Result<Property, Error> {
    let row = tables.row(table::PROPERTY, rid - 1)?;
    let name = row.string(1)?.to_string(); // Name
    let signature = decode_property_sig(row.blob(2)?, image_tables); // Type
    let acc = semantics.properties.get(&rid);
    Ok(Property {
        name,
        signature,
        getter: acc
            .and_then(|a| a.getter)
            .and_then(|r| accessor_method_id(r, m_range)),
        setter: acc
            .and_then(|a| a.setter)
            .and_then(|r| accessor_method_id(r, m_range)),
        // Each `Other` row is kept even when its method RID falls outside the
        // owning type's run (`None`): the row's *presence* is what the
        // projector must refuse, so a malformed RID must not silently launder
        // the property back into the projectable set.
        other_accessors: acc
            .map(|a| {
                a.other
                    .iter()
                    .map(|&r| accessor_method_id(r, m_range))
                    .collect()
            })
            .unwrap_or_default(),
        attributes: attrs.take(table::PROPERTY, rid),
    })
}

fn build_event(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    attrs: &mut CustomAttributeIndex,
    semantics: &Semantics,
    m_range: (u32, u32),
    rid: u32,
) -> Result<Event, Error> {
    let row = tables.row(table::EVENT, rid - 1)?;
    let name = row.string(1)?.to_string(); // Name
    // EventType is a `TypeDefOrRef` coded index; a null slot (which no real
    // compiler emits) decodes to `Err(BadToken)`, matching the model's lack of a
    // typeless-event slot.
    let event_type = decode_type_def_or_ref(md, tables, image_tables, row.coded(2));
    let acc = semantics.events.get(&rid);
    Ok(Event {
        name,
        event_type,
        add: acc
            .and_then(|a| a.add)
            .and_then(|r| accessor_method_id(r, m_range)),
        remove: acc
            .and_then(|a| a.remove)
            .and_then(|r| accessor_method_id(r, m_range)),
        raise: acc
            .and_then(|a| a.raise)
            .and_then(|r| accessor_method_id(r, m_range)),
        // As for properties: a malformed `Other` RID is kept as `None` so the
        // row still forces the drop-and-record.
        other_accessors: acc
            .map(|a| {
                a.other
                    .iter()
                    .map(|&r| accessor_method_id(r, m_range))
                    .collect()
            })
            .unwrap_or_default(),
        attributes: attrs.take(table::EVENT, rid),
    })
}

/// Map an accessor's `MethodDef` RID to its index within its owning type's
/// method list. The accessor belongs to the same type as the property/event it
/// serves, so its RID lies in `m_range` (`[start, end)`); a RID outside that run
/// (only possible in malformed metadata) drops the linkage rather than minting
/// an out-of-range [`MethodId`].
fn accessor_method_id(method_rid: u32, m_range: (u32, u32)) -> Option<MethodId> {
    (method_rid >= m_range.0 && method_rid < m_range.1).then(|| MethodId(method_rid - m_range.0))
}

// ============================================================================
// Supporting table walks
// ============================================================================

/// Accessor `MethodDef` RIDs gathered from `MethodSemantics` (§II.22.28), keyed
/// by the property or event RID they associate with.
struct Semantics {
    properties: HashMap<u32, PropAccessors>,
    events: HashMap<u32, EventAccessors>,
}

#[derive(Default)]
struct PropAccessors {
    getter: Option<u32>,
    setter: Option<u32>,
    /// `Other` (0x4) semantics rows — non-standard extra accessors. Recorded
    /// faithfully (see [`Property::other_accessors`]); the projector drops and
    /// records the owning property.
    other: Vec<u32>,
}

#[derive(Default)]
struct EventAccessors {
    add: Option<u32>,
    remove: Option<u32>,
    raise: Option<u32>,
    /// See [`PropAccessors::other`].
    other: Vec<u32>,
}

fn read_method_semantics(tables: &Tables) -> Result<Semantics, Error> {
    let method_count = tables.row_count(table::METHOD_DEF);
    let mut sem = Semantics {
        properties: HashMap::new(),
        events: HashMap::new(),
    };
    for i in 0..tables.row_count(table::METHOD_SEMANTICS) {
        let row = tables.row(table::METHOD_SEMANTICS, i)?;
        let flags = row.int(0); // Semantics
        let method_rid = row.int(1); // Method (simple MethodDef index)
        // Refuse a dangling accessor or association RID loudly rather than
        // storing it under an unreachable key (`decode_coded` and a simple index
        // both validate only that the value is in-table-space, not in-range).
        checked_index(method_rid, method_count)?;
        let Some(assoc) = tables.decode_coded(Coded::HasSemantics, row.coded(2))? else {
            continue;
        };
        checked_index(assoc.rid, tables.row_count(assoc.table))?;
        match assoc.table {
            table::PROPERTY => {
                let acc = sem.properties.entry(assoc.rid).or_default();
                if flags & SEM_GETTER != 0 {
                    acc.getter = Some(method_rid);
                }
                if flags & SEM_SETTER != 0 {
                    acc.setter = Some(method_rid);
                }
                // The `Other` semantics (0x4) associates an extra, non-standard
                // accessor (beyond get/set/add/remove/fire) — used by some F#
                // pickle output and a little interop. The projected model has
                // no slot for it, so it is recorded faithfully here and the
                // projector drops (and records) the owning member — a
                // per-member cost, not an image-fatal one.
                if flags & SEM_OTHER != 0 {
                    acc.other.push(method_rid);
                }
            }
            table::EVENT => {
                let acc = sem.events.entry(assoc.rid).or_default();
                if flags & SEM_ADD_ON != 0 {
                    acc.add = Some(method_rid);
                }
                if flags & SEM_REMOVE_ON != 0 {
                    acc.remove = Some(method_rid);
                }
                if flags & SEM_FIRE != 0 {
                    acc.raise = Some(method_rid);
                }
                // See the property arm above.
                if flags & SEM_OTHER != 0 {
                    acc.other.push(method_rid);
                }
            }
            // HasSemantics decodes only to Event/Property.
            _ => {}
        }
    }
    Ok(sem)
}

/// The decoded `Constant` value (§II.22.9) of every `parent_table` row that owns
/// one, keyed by 1-based parent RID. The `Type` column is an `ELEMENT_TYPE`; the
/// `Value` blob is the raw little-endian value (UTF-16 code units for strings, a
/// 4-byte zero for a null `CLASS`).
///
/// Structural failures (a dangling parent RID, a blob heap overrun) still abort,
/// but a *value* whose blob does not decode for its declared type is dropped
/// (the parent simply gets no default) rather than sinking the whole assembly —
/// per-item value decode is non-fatal, matching the reader's signature/attribute
/// handling (see [`super::Error`]).
fn read_constant_values(
    tables: &Tables,
    parent_table: usize,
) -> Result<HashMap<u32, Constant>, Error> {
    let mut map = HashMap::new();
    for i in 0..tables.row_count(table::CONSTANT) {
        let row = tables.row(table::CONSTANT, i)?;
        // Columns: Type(0), Parent(1) = HasConstant coded, Value(2).
        if let Some(parent) = tables.decode_coded(Coded::HasConstant, row.coded(1))? {
            // A dangling parent RID is refused loudly, not stored unreachably.
            checked_index(parent.rid, tables.row_count(parent.table))?;
            if parent.table == parent_table {
                let element_type = (row.int(0) & 0xff) as u8;
                if let Some(value) = decode_constant(element_type, row.blob(2)?) {
                    map.insert(parent.rid, value);
                }
            }
        }
    }
    Ok(map)
}

// ECMA-335 II.23.1.16 `ELEMENT_TYPE_*` codes legal in a `Constant` blob.
const ELEM_BOOLEAN: u8 = 0x02;
const ELEM_CHAR: u8 = 0x03;
const ELEM_I1: u8 = 0x04;
const ELEM_U1: u8 = 0x05;
const ELEM_I2: u8 = 0x06;
const ELEM_U2: u8 = 0x07;
const ELEM_I4: u8 = 0x08;
const ELEM_U4: u8 = 0x09;
const ELEM_I8: u8 = 0x0a;
const ELEM_U8: u8 = 0x0b;
const ELEM_R4: u8 = 0x0c;
const ELEM_R8: u8 = 0x0d;
const ELEM_STRING: u8 = 0x0e;
const ELEM_CLASS: u8 = 0x12;

/// Decode a `Constant` value blob for the given `ELEMENT_TYPE`, or `None` if the
/// element type is unsupported or the blob is the wrong length / malformed for
/// its declared type — never guesses. UTF-16 is preserved losslessly as raw code
/// units (`char`/string defaults may legally hold unpaired surrogates, which are
/// valid CLI metadata but not Rust `char`/`String`); rendering to a displayable
/// form happens later, at projection.
pub(super) fn decode_constant(element_type: u8, blob: &[u8]) -> Option<Constant> {
    // `TryFrom<&[u8]>` for a fixed array requires an *exact*-length slice, so a
    // fixed-width element type rejects an over- or under-long blob rather than
    // silently taking a prefix (II.22.9 gives each primitive a fixed width).
    fn bytes<const N: usize>(blob: &[u8]) -> Option<[u8; N]> {
        <[u8; N]>::try_from(blob).ok()
    }
    Some(match element_type {
        ELEM_BOOLEAN => Constant::Bool(bytes::<1>(blob)?[0] != 0),
        // A raw UTF-16 code unit — kept verbatim, even an unpaired surrogate.
        ELEM_CHAR => Constant::Char(u16::from_le_bytes(bytes::<2>(blob)?)),
        ELEM_I1 => Constant::Int(i64::from(i8::from_le_bytes(bytes::<1>(blob)?))),
        ELEM_U1 => Constant::UInt(u64::from(bytes::<1>(blob)?[0])),
        ELEM_I2 => Constant::Int(i64::from(i16::from_le_bytes(bytes::<2>(blob)?))),
        ELEM_U2 => Constant::UInt(u64::from(u16::from_le_bytes(bytes::<2>(blob)?))),
        ELEM_I4 => Constant::Int(i64::from(i32::from_le_bytes(bytes::<4>(blob)?))),
        ELEM_U4 => Constant::UInt(u64::from(u32::from_le_bytes(bytes::<4>(blob)?))),
        ELEM_I8 => Constant::Int(i64::from_le_bytes(bytes::<8>(blob)?)),
        ELEM_U8 => Constant::UInt(u64::from_le_bytes(bytes::<8>(blob)?)),
        ELEM_R4 => Constant::F32(u32::from_le_bytes(bytes::<4>(blob)?)),
        ELEM_R8 => Constant::F64(u64::from_le_bytes(bytes::<8>(blob)?)),
        ELEM_STRING => {
            // A string blob is a whole number of little-endian UTF-16 code units;
            // an odd length is malformed. The units are kept verbatim (unpaired
            // surrogates and all), decoded for display only at projection.
            if !blob.len().is_multiple_of(2) {
                return None;
            }
            Constant::Str(
                blob.chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect(),
            )
        }
        // A null reference default (`ELEMENT_TYPE_CLASS`) is exactly a 4-byte
        // zero (II.22.9); any other blob for `CLASS` is malformed, not a null.
        ELEM_CLASS if blob.len() == 4 && blob.iter().all(|&b| b == 0) => Constant::Null,
        _ => return None,
    })
}

/// Per-type member ranges from a `*Map` table (`PropertyMap`/`EventMap`): each
/// map row associates a `TypeDef` with the start of a contiguous run in the
/// member table, the run ending where the next map row's run begins. Returns,
/// per `TypeDefId`, the `[start, end)` 1-based RID range (or `None` for a type
/// with no map row).
fn read_map_ranges(
    tables: &Tables,
    map_table: usize,
    member_table: usize,
    type_count: usize,
) -> Result<Vec<Option<(u32, u32)>>, Error> {
    let member_count = tables.row_count(member_table);
    let mut entries = Vec::new();
    for i in 0..tables.row_count(map_table) {
        let row = tables.row(map_table, i)?;
        let ti = typedef_index(row.int(0), type_count)?; // Parent
        let start = row.int(1); // member-list start RID
        // A start outside `1..=member_count+1` is a dangling list index, refused
        // loudly rather than iterated into the wrong rows (or silently empty).
        if start == 0 || start > member_count + 1 {
            return Err(Error::TableIndexOutOfRange);
        }
        entries.push((ti, start));
    }
    // The runs partition the member table; sort by start so each run ends where
    // the next begins (real emitters already emit them in this order). After the
    // sort the starts are non-decreasing and in range, so every `[start, end)`
    // range lies within `1..=member_count`.
    entries.sort_by_key(|&(_, start)| start);
    let mut ranges = vec![None; type_count];
    for k in 0..entries.len() {
        let (ti, start) = entries[k];
        let end = entries
            .get(k + 1)
            .map(|&(_, s)| s)
            .unwrap_or(member_count + 1);
        ranges[ti] = Some((start, end));
    }
    Ok(ranges)
}
