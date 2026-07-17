//! Stage 4–5: the type-definition table walk.
//!
//! [`read_types`] walks the `TypeDef`/`TypeRef`/`NestedClass`/`InterfaceImpl`/
//! `GenericParam`/`GenericParamConstraint`/`CustomAttribute` tables plus the
//! member tables (via [`super::members`]) and produces the owned [`TypeDef`]
//! arena (now including members), the `TypeRef` arena, and the top-level handle
//! list — the type slice of the eventual `Image`.
//!
//! It is total over structurally-valid metadata: it fails only on a structural
//! defect (a table index or heap offset out of range, an unsupported `TypeRef`
//! scope, a `CompilerControlled` member). Per-item signature failures in
//! `extends`/`implements`/constraints/members are stored as
//! `Result<_, SigError>`, so one unreadable base type or member never aborts the
//! whole assembly.

use super::Error;
use super::ids::{AssemblyRefId, MethodId, TypeDefId, TypeRefId};
use super::members;
use super::metadata::MetadataFile;
use super::model::{Accessibility, MemberRef, RawAttribute, RefScope, TypeDef, TypeName, TypeRef};
use super::signature::{
    ImageTables, ModifiedType, SigError, TypeScope, TypeSig, decode_type, resolve_token,
};
use super::tables::{Coded, Tables, table};

/// The type slice of an `Image`: the flat `TypeDef` arena (including nested and
/// the `<Module>` pseudo-type, in `TypeDef`-table order so a `TypeDefId`
/// equals RID − 1), the `TypeRef` arena, and the top-level handle list. Later
/// stages embed these into the public `Image` alongside the assembly identity,
/// references, members, and resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Types {
    pub(crate) type_defs: Vec<TypeDef>,
    pub(crate) top_level: Vec<TypeDefId>,
    pub(crate) type_refs: Vec<TypeRef>,
    /// The `MemberRef` arena, in table order (so a [`super::ids::MemberRefId`]
    /// indexes it directly). Built for stage 6's custom-attribute decoder, which
    /// reads a referenced constructor's signature from here.
    pub(crate) member_refs: Vec<MemberRef>,
    /// Raw custom attributes on the `Assembly` row — assembly-scoped markers
    /// (e.g. F#'s `FSharpInterfaceDataVersion`, the assembly-level
    /// `AutoOpen("ns")`) that consumers scan. Empty for a module with no
    /// `Assembly` row.
    pub(crate) assembly_attributes: Vec<RawAttribute>,
}

/// TypeAttributes visibility mask (§II.23.1.15).
const VISIBILITY_MASK: u32 = 0x0000_0007;
/// TypeAttributes `ClassSemanticsMask`; the `Interface` value is the same bit.
/// Shared with the member stage's `MethodImpl` post-pass, which classifies a
/// declaration parent by the flag when it is an in-module `TypeDef`.
pub(super) const INTERFACE_FLAG: u32 = 0x0000_0020;
const SEALED_FLAG: u32 = 0x0000_0100;

pub(crate) fn read_types(md: &MetadataFile) -> Result<Types, Error> {
    let tables = Tables::new(md)?;
    members::ensure_optimized_layout(&tables)?;
    let image_tables = ImageTables {
        type_def_rows: tables.row_count(table::TYPE_DEF),
        type_ref_rows: tables.row_count(table::TYPE_REF),
    };
    let count = tables.row_count(table::TYPE_DEF) as usize;

    let type_refs = read_type_refs(md, &tables)?;
    let member_refs = members::read_member_refs(&tables, &image_tables)?;
    let method_starts = read_method_starts(&tables)?;
    // The `MethodList` partition underpins both the attribute-ctor resolution
    // (`method_owner`) and the member walk, so validate it once up front.
    members::validate_list_starts(&method_starts, tables.row_count(table::METHOD_DEF))?;

    // Index every custom attribute by parent `(table, rid)`, resolving each
    // constructor handle once; every position pulls its own attributes from it.
    let mut attrs = members::CustomAttributeIndex::build(md, &tables, &method_starts)?;

    // --- Per-type structural collections ---
    let (enclosing, nested) = read_nesting(&tables, count)?;
    let mut interface_tokens = read_interface_impls(&tables, count)?;

    // Generic parameters (type- and method-owned), each with its constraints and
    // attributes.
    let mut gps = members::read_generic_params(md, &tables, &image_tables, &mut attrs, count)?;
    let mut type_gps = std::mem::take(&mut gps.type_gps);

    // Members per type — consumes the method-owned generic params and drains the
    // attribute index of every member/parameter/return/generic-param position.
    let mut members = members::read_members(
        md,
        &tables,
        &image_tables,
        &method_starts,
        &mut attrs,
        &mut gps.method_gps,
        count,
    )?;

    // --- Build each TypeDef, consuming the per-type collections ---
    let mut type_defs = Vec::with_capacity(count);
    for i in 0..count {
        let row = tables.row(table::TYPE_DEF, i as u32)?;
        let flags = row.int(0);
        let name = md.string_at(row.int(1))?.to_string();
        let namespace = md.string_at(row.int(2))?.to_string();
        let extends_coded = row.coded(3);

        // The canonical null `Extends` (the all-zero coded index) means
        // System.Object or an interface. A nonzero value with a zero RID is
        // malformed, not null: route it through the decoder so it surfaces as a
        // stored `SigError::BadToken` rather than being mistaken for "no base".
        let extends = if extends_coded == 0 {
            None
        } else {
            Some(decode_type_def_or_ref(
                md,
                &tables,
                &image_tables,
                extends_coded,
            ))
        };

        let implements = std::mem::take(&mut interface_tokens[i])
            .into_iter()
            .map(|tok| decode_type_def_or_ref(md, &tables, &image_tables, tok))
            .collect();

        type_defs.push(TypeDef {
            name: TypeName { namespace, name },
            accessibility: fold_accessibility(flags),
            is_interface: flags & INTERFACE_FLAG != 0,
            is_sealed: flags & SEALED_FLAG != 0,
            extends,
            implements,
            generic_params: std::mem::take(&mut type_gps[i]),
            enclosing: enclosing[i],
            nested: nested[i].clone(),
            methods: std::mem::take(&mut members[i].methods),
            fields: std::mem::take(&mut members[i].fields),
            properties: std::mem::take(&mut members[i].properties),
            events: std::mem::take(&mut members[i].events),
            attributes: attrs.take(table::TYPE_DEF, (i + 1) as u32),
        });
    }

    let top_level = (0..count)
        .filter(|&i| enclosing[i].is_none())
        .map(|i| TypeDefId(i as u32))
        .collect();

    // The `Assembly` table holds at most one row (RID 1); its custom attributes
    // are the assembly-scoped markers. Empty for a module with no `Assembly` row
    // (no `CustomAttribute` parents that RID, so the index has nothing to give).
    let assembly_attributes = attrs.take(table::ASSEMBLY, 1);

    Ok(Types {
        type_defs,
        top_level,
        type_refs,
        member_refs,
        assembly_attributes,
    })
}

/// Build the `TypeRef` arena, refusing any `TypeRef` whose resolution scope is
/// outside the supported subset (§ refuse-loudly summary).
fn read_type_refs(md: &MetadataFile, tables: &Tables) -> Result<Vec<TypeRef>, Error> {
    let n = tables.row_count(table::TYPE_REF);
    let mut refs = Vec::with_capacity(n as usize);
    for i in 0..n {
        let row = tables.row(table::TYPE_REF, i)?;
        let scope = resolve_ref_scope(tables, row.coded(0))?;
        let name = md.string_at(row.int(1))?.to_string();
        let namespace = md.string_at(row.int(2))?.to_string();
        refs.push(TypeRef {
            name: TypeName { namespace, name },
            scope,
        });
    }
    Ok(refs)
}

fn resolve_ref_scope(tables: &Tables, coded: u32) -> Result<RefScope, Error> {
    match tables.decode_coded(Coded::ResolutionScope, coded)? {
        Some(tok) if tok.table == table::ASSEMBLY_REF => {
            let id = checked_index(tok.rid, tables.row_count(table::ASSEMBLY_REF))?;
            Ok(RefScope::AssemblyRef(AssemblyRefId(id)))
        }
        Some(tok) if tok.table == table::TYPE_REF => {
            let id = checked_index(tok.rid, tables.row_count(table::TYPE_REF))?;
            Ok(RefScope::Nested(TypeRefId(id)))
        }
        // A module-self scope aliases a type defined in this image; F# emits
        // these for a record/union's own `IComparable<T>`/`IEquatable<T>` args.
        // The RID must still name a real `Module` row (the table holds exactly
        // one), validated like the sibling arms rather than silently discarded.
        Some(tok) if tok.table == table::MODULE => {
            checked_index(tok.rid, tables.row_count(table::MODULE))?;
            Ok(RefScope::Module)
        }
        // ModuleRef (multi-module assemblies) and a null scope (ExportedType
        // lookup) are outside the supported subset.
        _ => Err(Error::UnsupportedTypeRefScope),
    }
}

/// The `MethodList` RID (1-based) starting each `TypeDef`'s run of methods, in
/// table order. Non-decreasing, so it doubles as the search key that maps a
/// `MethodDef` RID back to its owning type.
pub(super) fn read_method_starts(tables: &Tables) -> Result<Vec<u32>, Error> {
    let count = tables.row_count(table::TYPE_DEF);
    let mut starts = Vec::with_capacity(count as usize);
    for i in 0..count {
        let row = tables.row(table::TYPE_DEF, i)?;
        starts.push(row.int(5)); // MethodList
    }
    Ok(starts)
}

/// `enclosing[i]` is the encloser of `TypeDefId(i)` (or `None`); `nested[i]` is
/// its directly-nested children, both from the `NestedClass` table.
#[allow(clippy::type_complexity)]
fn read_nesting(
    tables: &Tables,
    count: usize,
) -> Result<(Vec<Option<TypeDefId>>, Vec<Vec<TypeDefId>>), Error> {
    let mut enclosing = vec![None; count];
    let mut nested = vec![Vec::new(); count];
    for i in 0..tables.row_count(table::NESTED_CLASS) {
        let row = tables.row(table::NESTED_CLASS, i)?;
        let child = typedef_index(row.int(0), count)?;
        let parent = typedef_index(row.int(1), count)?;
        enclosing[child] = Some(TypeDefId(parent as u32));
        nested[parent].push(TypeDefId(child as u32));
    }
    Ok((enclosing, nested))
}

/// The raw `Interface` coded tokens (`TypeDefOrRef`-or-`TypeSpec`) grouped by
/// implementing `TypeDef`, in `InterfaceImpl`-table order. Also used by the
/// member stage's `MethodImpl` post-pass: a declaration parent that appears in
/// the implementing type's list is proven to be an interface (only interfaces
/// may appear in `InterfaceImpl`).
pub(super) fn read_interface_impls(tables: &Tables, count: usize) -> Result<Vec<Vec<u32>>, Error> {
    let mut per_type = vec![Vec::new(); count];
    for i in 0..tables.row_count(table::INTERFACE_IMPL) {
        let row = tables.row(table::INTERFACE_IMPL, i)?;
        let class = typedef_index(row.int(0), count)?;
        per_type[class].push(row.coded(1));
    }
    Ok(per_type)
}

/// Map a `MethodDef` RID (1-based) to its owning type and the method's local
/// index within that type's `MethodList` run.
pub(super) fn method_owner(
    method_starts: &[u32],
    rid: u32,
    tables: &Tables,
) -> Result<(TypeDefId, MethodId), Error> {
    if rid == 0 || rid > tables.row_count(table::METHOD_DEF) {
        return Err(Error::TableIndexOutOfRange);
    }
    // Owner is the last type whose MethodList start is <= rid (starts are
    // non-decreasing). `partition_point` counts the starts <= rid; the owner is
    // one before that boundary.
    let boundary = method_starts.partition_point(|&start| start <= rid);
    let owner = boundary.checked_sub(1).ok_or(Error::TableIndexOutOfRange)?;
    let local = rid - method_starts[owner];
    Ok((TypeDefId(owner as u32), MethodId(local)))
}

/// Decode a `TypeDefOrRef`-or-`TypeSpec` coded token (as it appears in the
/// `Extends`/`Interface`/`Constraint`/`EventType` columns) into a [`TypeSig`].
///
/// A bare `TypeDef`/`TypeRef` token carries no class-vs-valuetype bit, so it
/// yields `Named { kind: None, .. }`; a `TypeSpec` is a signature blob and goes
/// through the stage-2 [`decode_type`] core. Bad tokens and unreadable
/// `TypeSpec` blobs surface as a stored [`SigError`], never a parse abort.
pub(super) fn decode_type_def_or_ref(
    md: &MetadataFile,
    tables: &Tables,
    image_tables: &ImageTables,
    coded: u32,
) -> Result<ModifiedType, SigError> {
    // TypeDefOrRef coded index: 2 tag bits, tags 0=TypeDef, 1=TypeRef,
    // 2=TypeSpec (3 reserved). Same encoding as the signature form, so the
    // TypeDef/TypeRef arms reuse the stage-2 resolver.
    const TYPE_SPEC_TAG: u32 = 2;
    if coded & 0b11 == TYPE_SPEC_TAG {
        let rid = coded >> 2;
        if rid == 0 {
            return Err(SigError::BadToken);
        }
        let blob = typespec_blob(md, tables, rid).ok_or(SigError::BadToken)?;
        decode_type(blob, image_tables)
    } else {
        // A bare token carries no modifier run — only a `TypeSpec` blob can.
        let scope: TypeScope = resolve_token(coded, image_tables)?;
        Ok(ModifiedType::plain(TypeSig::Named { kind: None, scope }))
    }
}

/// The `Signature` blob of the `TypeSpec` row at 1-based `rid`, or `None` if the
/// row or its blob is out of range.
fn typespec_blob<'a>(md: &'a MetadataFile, tables: &Tables, rid: u32) -> Option<&'a [u8]> {
    let row = tables.row(table::TYPE_SPEC, rid - 1).ok()?;
    md.blob_at(row.int(0)).ok()
}

/// Fold the seven-valued §II.23.1.15 type-visibility field onto the six-variant
/// [`Accessibility`]. Total over the field: `NotPublic` (0) folds onto
/// `Assembly`, so there is no `CompilerControlled` case to refuse for a type.
fn fold_accessibility(flags: u32) -> Accessibility {
    match flags & VISIBILITY_MASK {
        0 => Accessibility::Assembly,    // NotPublic (top-level internal)
        1 => Accessibility::Public,      // Public
        2 => Accessibility::Public,      // NestedPublic
        3 => Accessibility::Private,     // NestedPrivate
        4 => Accessibility::Family,      // NestedFamily
        5 => Accessibility::Assembly,    // NestedAssembly
        6 => Accessibility::FamAndAssem, // NestedFamANDAssem
        7 => Accessibility::FamOrAssem,  // NestedFamORAssem
        _ => unreachable!("masked with 0x07"),
    }
}

/// A 1-based RID into a table of `count` rows → a 0-based index, or
/// [`Error::TableIndexOutOfRange`] for rid 0 or rid past the end.
pub(super) fn checked_index(rid: u32, count: u32) -> Result<u32, Error> {
    let idx = rid.checked_sub(1).ok_or(Error::TableIndexOutOfRange)?;
    if idx < count {
        Ok(idx)
    } else {
        Err(Error::TableIndexOutOfRange)
    }
}

/// As [`checked_index`], specialised to the `TypeDef` arena and returning a
/// `usize` index.
pub(super) fn typedef_index(rid: u32, count: usize) -> Result<usize, Error> {
    let idx = rid.checked_sub(1).ok_or(Error::TableIndexOutOfRange)? as usize;
    if idx < count {
        Ok(idx)
    } else {
        Err(Error::TableIndexOutOfRange)
    }
}
