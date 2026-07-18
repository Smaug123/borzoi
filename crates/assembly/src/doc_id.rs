//! XML documentation comment IDs — the keys that index a referenced
//! assembly's sidecar `.xml` doc file.
//!
//! A .NET XML doc file (`System.Console.xml`, `FSharp.Core.xml`, …) records one
//! `<member name="…">` per documented type/member, keyed by a *documentation
//! comment ID string* (ECMA-334 / the C# spec, §"Processing the documentation
//! file"): `T:System.Console`, `M:System.Console.WriteLine(System.String)`,
//! `P:`, `F:`, `E:`. The format is a function of **metadata**, not source
//! language — Roslyn (for the BCL) and the F# compiler (for `FSharp.Core`) both
//! emit the same standard IDs for the same IL shape — so we can reconstruct an
//! ID from our own [`Entity`]/[`Member`] model and it matches the file's key
//! regardless of which compiler wrote the assembly. That decouples doc lookup
//! from FCS entirely. (The ID *format* is language-agnostic; the caveat is that
//! our F# member *projection* re-interprets some members away from their IL
//! kind — see "Limitations".)
//!
//! This module is the *generation* half (slice 1): a pure
//! [`Entity`]/[`Member`] → ID-string function. Finding and parsing the `.xml`,
//! and wiring the result into hover, are later slices.
//!
//! ## What the rules are
//!
//! Mirrors the F# compiler's IL reader path (`GetXmlDocSigOf*` in
//! `Checking/InfoReader.fs`, which keys off `ILTypeRef.FullName`), validated
//! against real Roslyn-emitted `.xml` files rather than against FCS's own
//! *generation* path — the latter encodes multidimensional arrays
//! nonconformantly (`[0:]` for a 2-D array) and so cannot find their docs.
//!
//! - **Prefixes**: `T:` type, `M:` method, `P:` property, `F:` field, `E:`
//!   event.
//! - **Type full name**: namespace segments and the enclosing-type chain joined
//!   with `.` (never `+`), each type segment keeping its `` `n `` arity suffix.
//!   The arity on a segment counts the generic parameters *introduced at that
//!   level* — a nested type subtracts its encloser's cumulative arity (so
//!   `Dictionary`2.Enumerator`, not `Dictionary`2.Enumerator`2`).
//! - **Member name**: `.` becomes `#`, so `.ctor` → `#ctor`; an explicit
//!   interface implementation, whose name embeds the constructed interface
//!   (`ICollection<System.Int32>.Add`), additionally maps `<`/`>` to `{`/`}`
//!   (`ICollection{System#Int32}#Add`).
//! - **Generic arity on a method**: `` ``n `` after the name (before the
//!   parameter list).
//! - **Parameter list**: `(t1,t2,…)`; absent entirely when there are no
//!   parameters.
//! - **Type references inside a signature** ([`type_enc`]): a type typar is
//!   `` `i ``, a method typar `` ``i `` (our [`TypeRef::Var`] already carries
//!   the index and which list it indexes); a generic instantiation is
//!   `Name{arg,arg}` (braces, *not* the `` `n `` suffix); a vector is `[]`, a
//!   rank-`r` array `[0:,0:,…]`; byref/`out` append `@`; a pointer appends `*`.
//!   A constructed *nested* generic distributes its arguments across the
//!   declaring segments by per-segment arity (`Outer{a,b}.Inner{c}`); see
//!   [`type_enc`].
//! - **Conversion operators** (`op_Implicit` / `op_Explicit` and their C# 11
//!   checked forms `op_CheckedImplicit` / `op_CheckedExplicit`) append
//!   `~ReturnType` after the parameter list — the one place a return type
//!   enters the ID.
//!
//! ## Limitations
//!
//! The generator is faithful whenever the [`Member`]/[`TypeRef`] it is handed
//! reflects the *IL metadata* shape — always the case for C#/BCL assemblies,
//! which the differential test (`tests/all/doc_id_diff.rs`) pins against Roslyn's
//! own `.xml`.
//!
//! For an **F#-projected** assembly the [`Member`] variant is the FCS
//! *source-level* kind, not the IL kind. The one place that mis-keyed a doc ID —
//! a module value (an IL *property* like `Operators.NaN`) rebranded to
//! [`Member::Method`] — is handled: `project_fsharp_members` marks the
//! getter-rebranded value with [`crate::MethodLike::module_value`], so this
//! generator keys it `P:` (the prefix the F# compiler's own XML uses), pinned by
//! `tests/all/doc_id_fsharp_core_diff.rs`. (The record/exception field-backed
//! property → [`Member::Field`] rebrand needs no such handling: the F# compiler
//! keys those `F:` too, so our rebrand already matches.)
//!
//! Remaining `FSharp.Core` doc-ID gaps are *not* member-rebranding and are
//! tracked separately in `docs/completed/fsharp-member-rebranding-docid-plan.md`: generic
//! module methods / F# array-bound encoding (`M:`), type-name keys (`T:`), and
//! FCS-surfaced type properties the projection drops.

use crate::model::{Entity, Member, Parameter, Primitive, TypeRef};

/// The XML-doc *type name* of a type: the text after the `T:` prefix, e.g.
/// `System.Collections.Generic.Dictionary`2`. Produced by [`type_doc_name`] and
/// consumed by [`member_doc_id`] to form a member's ID; it also carries the
/// type's *cumulative* generic arity (its own parameters plus every enclosing
/// type's) so a nested type can recover its own arity by subtraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDocName {
    full: String,
    cumulative_arity: usize,
}

impl TypeDocName {
    /// The text after the `T:` prefix (`System.Console`,
    /// `System.Collections.Generic.List`1`).
    pub fn full(&self) -> &str {
        &self.full
    }

    /// This type's own documentation comment ID (`T:` prefix).
    pub fn type_id(&self) -> String {
        format!("T:{}", self.full)
    }
}

/// Compute a type's [`TypeDocName`]. `enclosing` is `None` for a top-level type
/// — its name is prefixed by its [`Entity::namespace`] — or `Some(parent)` for
/// a nested type, whose name is prefixed by the enclosing type's full name and
/// whose own arity is its cumulative arity minus the encloser's.
///
/// To enumerate a whole subtree (top-level type, then its nested types, then
/// theirs) thread the returned value down as the `enclosing` of each child;
/// [`walk_doc_ids`] does exactly that.
pub fn type_doc_name(entity: &Entity, enclosing: Option<&TypeDocName>) -> TypeDocName {
    let cumulative_arity = entity.generic_parameters.len();
    let (prefix, parent_arity) = match enclosing {
        Some(parent) => (parent.full.clone(), parent.cumulative_arity),
        // A top-level type's namespace is its prefix; the global namespace ([])
        // yields an empty prefix and the bare name.
        None => (entity.namespace.join("."), 0),
    };
    // A nested type's metadata generic-parameter list repeats every enclosing
    // type's parameters, so its *own* arity — the count the `` `n `` name suffix
    // encodes — is the difference. `saturating_sub` keeps malformed metadata
    // (a child with fewer parameters than its parent) from panicking.
    let own_arity = cumulative_arity.saturating_sub(parent_arity);
    let mut name = demangle_segment(&entity.name).to_string();
    if own_arity > 0 {
        name.push('`');
        name.push_str(&own_arity.to_string());
    }
    let full = if prefix.is_empty() {
        name
    } else {
        format!("{prefix}.{name}")
    };
    TypeDocName {
        full,
        cumulative_arity,
    }
}

/// The documentation comment ID of `member`, declared on the type named by
/// `decl`. Total over the four [`Member`] kinds: methods (incl. constructors
/// and conversion operators), fields, properties (incl. indexers), and events.
pub fn member_doc_id(decl: &TypeDocName, member: &Member) -> String {
    match member {
        Member::Method(m) => {
            let name = escape_member_name(&m.name);
            let generic_arity = if m.generic_parameters.is_empty() {
                String::new()
            } else {
                format!("``{}", m.generic_parameters.len())
            };
            let args = args_enc(&m.signature.parameters);
            // Conversion operators — and only these — encode their return type
            // as a `~`-suffix, disambiguating overloads that differ solely in
            // return type (`op_Explicit(System.Decimal)~System.Int32`).
            let conversion = if is_conversion_operator(&m.name) {
                format!("~{}", type_enc(&m.signature.return_type))
            } else {
                String::new()
            };
            // Ordinarily a method keys as `M:`. An F# module value is surfaced as
            // a method (FCS's source view) but its IL form is a property, which
            // the F# compiler's own XML keys `P:`; `module_value` marks the
            // getter-rebranded value. The body is unchanged — a rebranded module
            // value is zero-arg and non-generic, so only the prefix differs.
            let prefix = if m.module_value.is_some() { 'P' } else { 'M' };
            format!(
                "{prefix}:{}.{name}{generic_arity}{args}{conversion}",
                decl.full
            )
        }
        Member::Field(f) => format!("F:{}.{}", decl.full, escape_member_name(&f.name)),
        Member::Property(p) => {
            // An indexer's index parameters are encoded just like a method's
            // parameter list; an ordinary property has none and emits no parens.
            let args = if p.parameters.is_empty() {
                String::new()
            } else {
                let inner = p
                    .parameters
                    .iter()
                    .map(|ip| type_enc(&ip.ty.ty))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("({inner})")
            };
            format!("P:{}.{}{args}", decl.full, escape_member_name(&p.name))
        }
        Member::Event(e) => format!("E:{}.{}", decl.full, escape_member_name(&e.name)),
    }
}

/// Visit the documentation comment ID of `entity` and of each of its members,
/// then recurse into its nested types. `enclosing` is the entity's enclosing
/// [`TypeDocName`] (`None` at a top level). The IDs arrive in no guaranteed
/// order; callers that need a set should collect into one.
pub fn walk_doc_ids(entity: &Entity, enclosing: Option<&TypeDocName>, f: &mut impl FnMut(String)) {
    let decl = type_doc_name(entity, enclosing);
    f(decl.type_id());
    for member in &entity.members {
        f(member_doc_id(&decl, member));
    }
    for nested in &entity.nested_types {
        walk_doc_ids(nested, Some(&decl), f);
    }
}

/// Encode a parameter list as `(t1,t2,…)`, or the empty string when there are
/// none (a no-arg method's ID has no parentheses at all).
fn args_enc(parameters: &[Parameter]) -> String {
    if parameters.is_empty() {
        return String::new();
    }
    let inner = parameters
        .iter()
        .map(param_enc)
        .collect::<Vec<_>>()
        .join(",");
    format!("({inner})")
}

/// Encode one parameter: its type, with a trailing `@` for a `ref`/`out`
/// parameter (the reader stores the referent type plus an `is_byref` flag
/// rather than wrapping it in [`TypeRef::ByRef`], so the `@` is applied here).
fn param_enc(p: &Parameter) -> String {
    let mut s = type_enc(&p.ty);
    if p.is_byref {
        s.push('@');
    }
    s
}

/// Encode a [`TypeRef`] in documentation-comment form. Recursive; carries no
/// context because [`TypeRef::Var`] already records the typar index and whether
/// it indexes the type's or the method's parameter list.
pub fn type_enc(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Primitive(p) => primitive_name(*p).to_string(),
        // A type typar is `` `i ``; a method typar `` ``i ``.
        TypeRef::Var {
            index,
            is_method: false,
        } => format!("`{index}"),
        TypeRef::Var {
            index,
            is_method: true,
        } => format!("``{index}"),
        TypeRef::Named {
            namespace,
            name,
            type_args,
            segment_arities,
            ..
        } => {
            let mut full = String::new();
            for seg in namespace {
                full.push_str(seg);
                full.push('.');
            }
            // The enclosing-type chain arrives as `Outer/Inner`, each segment
            // still carrying its `` `n `` arity; doc IDs join it with `.`,
            // demangling each segment.
            let segments: Vec<&str> = name.split('/').collect();
            // A constructed generic's arguments follow in braces, distributed
            // across the declaring segments by `segment_arities` the way Roslyn
            // keys nested generics: `Dictionary{K,V}.Enumerator` (args on the
            // encloser), `Outer{a}.Inner{b}` (one per level). This is exact only
            // when the arities line up with the segments *and* account for
            // exactly the args we hold. `segment_arities` is not enforced (corrupt
            // metadata can violate either relationship — see [`TypeRef::Named`]),
            // so when it doesn't line up we fall back to the naive rendering:
            // the dotted join with the whole arg list appended once at the end.
            //
            // The sum *saturates*: a corrupt arity can be near `usize::MAX`, so a
            // plain `sum()` would overflow (panic in debug, wrap in release —
            // then the slice below would index past `type_args`). A saturated
            // total can only equal `type_args.len()` (a real `Vec` length, never
            // `usize::MAX`) when no term overflowed, so the equality both rejects
            // the corrupt case *and* guarantees each `arity ≤ type_args.len()`,
            // keeping the per-segment slicing in bounds.
            let arity_total = segment_arities
                .iter()
                .copied()
                .fold(0usize, usize::saturating_add);
            let distributable =
                segment_arities.len() == segments.len() && arity_total == type_args.len();
            if distributable {
                let mut next = 0usize;
                let parts: Vec<String> = segments
                    .iter()
                    .zip(segment_arities)
                    .map(|(seg, &arity)| {
                        let mut s = demangle_segment(seg).to_string();
                        if arity > 0 {
                            let inner = type_args[next..next + arity]
                                .iter()
                                .map(|nt| type_enc(&nt.ty))
                                .collect::<Vec<_>>()
                                .join(",");
                            s.push('{');
                            s.push_str(&inner);
                            s.push('}');
                            next += arity;
                        }
                        s
                    })
                    .collect();
                full.push_str(&parts.join("."));
            } else {
                let dotted = segments
                    .iter()
                    .map(|s| demangle_segment(s))
                    .collect::<Vec<_>>()
                    .join(".");
                full.push_str(&dotted);
                if !type_args.is_empty() {
                    let inner = type_args
                        .iter()
                        .map(|nt| type_enc(&nt.ty))
                        .collect::<Vec<_>>()
                        .join(",");
                    full.push('{');
                    full.push_str(&inner);
                    full.push('}');
                }
            }
            full
        }
        TypeRef::Array {
            element,
            rank,
            sizes,
            lower_bounds,
        } => format!(
            "{}{}",
            type_enc(&element.ty),
            array_suffix(*rank, sizes, lower_bounds)
        ),
        TypeRef::Ptr(Some(inner)) => format!("{}*", type_enc(inner)),
        // `void*` — the one legal void pointee.
        TypeRef::Ptr(None) => "System.Void*".to_string(),
        // `@` for every byref, read-only or not: a doc-comment ID names the
        // signature Roslyn's `DocumentationCommentId` names, and that encoding
        // ignores custom modifiers — `in int` and `ref int` share `System.Int32@`
        // (they cannot overload each other in C#, so the ID stays unambiguous).
        TypeRef::ByRef { inner, .. } => format!("{}@", type_enc(inner)),
    }
}

/// The array suffix: `[]` for a vector (rank 1, no explicit bounds), otherwise
/// `[d,d,…]` with one `lowerBound:size` spec per dimension — `0:` for an
/// ordinary unbounded dimension, matching Roslyn's emission (`[0:,0:]` for a
/// 2-D array). A bounded dimension carries its declared lower bound and size.
///
/// Limitation: our model collapses a 1-D multidim array (`T[*]`,
/// `ELEMENT_TYPE_ARRAY` rank 1) onto the vector representation, so it is encoded
/// as `[]` rather than `[0:]`. Both are vanishingly rare in documented public
/// surface.
fn array_suffix(rank: u8, sizes: &[u32], lower_bounds: &[i32]) -> String {
    if rank <= 1 && sizes.is_empty() && lower_bounds.is_empty() {
        return "[]".to_string();
    }
    let dims = (0..rank as usize)
        .map(|i| {
            let lo = lower_bounds.get(i).copied().unwrap_or(0);
            match sizes.get(i) {
                Some(size) => format!("{lo}:{size}"),
                None => format!("{lo}:"),
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{dims}]")
}

/// Escape a member's IL name into its documentation-comment form. The name of
/// an ordinary member is unchanged except `.` → `#` (so `.ctor` → `#ctor`); an
/// explicit interface implementation, whose IL name embeds the *constructed*
/// interface (`System.Collections.Generic.ICollection<System.Int32>.Add`),
/// additionally maps `<` → `{` and `>` → `}`, yielding Roslyn's
/// `System#Collections#Generic#ICollection{System#Int32}#Add`. A
/// multi-argument interface instantiated with *concrete* types keeps the `,`
/// separator (`…ILookup{System#Int32,System#String}#Get`).
fn escape_member_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '.' => '#',
            '<' => '{',
            '>' => '}',
            other => other,
        })
        .collect()
}

/// Whether a method name is a user-defined conversion operator — the only
/// methods whose ID carries a `~ReturnType` suffix. Includes the C# 11 checked
/// forms (`op_CheckedImplicit` / `op_CheckedExplicit`), which modern reference
/// assemblies emit alongside the unchecked ones (e.g. `System.Int128`).
fn is_conversion_operator(name: &str) -> bool {
    matches!(
        name,
        "op_Implicit" | "op_Explicit" | "op_CheckedImplicit" | "op_CheckedExplicit"
    )
}

/// Strip a trailing `` `n `` generic-arity suffix from one name segment.
/// `List`1` → `List`; a name with no such suffix is returned unchanged.
fn demangle_segment(seg: &str) -> &str {
    match seg.rfind('`') {
        Some(tick) => {
            let suffix = &seg[tick + 1..];
            if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
                &seg[..tick]
            } else {
                seg
            }
        }
        None => seg,
    }
}

/// The documentation-comment full name of an ECMA-335 primitive — i.e. the
/// `System.*` type it aliases.
fn primitive_name(p: Primitive) -> &'static str {
    match p {
        Primitive::Void => "System.Void",
        Primitive::Bool => "System.Boolean",
        Primitive::Char => "System.Char",
        Primitive::I1 => "System.SByte",
        Primitive::U1 => "System.Byte",
        Primitive::I2 => "System.Int16",
        Primitive::U2 => "System.UInt16",
        Primitive::I4 => "System.Int32",
        Primitive::U4 => "System.UInt32",
        Primitive::I8 => "System.Int64",
        Primitive::U8 => "System.UInt64",
        Primitive::R4 => "System.Single",
        Primitive::R8 => "System.Double",
        Primitive::IntPtr => "System.IntPtr",
        Primitive::UIntPtr => "System.UIntPtr",
        Primitive::Object => "System.Object",
        Primitive::String => "System.String",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Access, AssemblyIdentity, Entity, EntityKind, Event, Field, IndexParameter, Member,
        MethodLike, MethodSignature, ModuleValue, Nullability, NullableType, ParamDefault,
        Parameter, Primitive, Property, TypeParameter, TypeRef, Variance, Version,
    };

    // ---- model builders (the model is wide; these keep the vectors legible) ----

    fn ai() -> AssemblyIdentity {
        AssemblyIdentity {
            name: "Fixture".into(),
            version: Version {
                major: 1,
                minor: 0,
                build: 0,
                revision: 0,
            },
            public_key_token: None,
        }
    }

    fn typar(name: &str) -> TypeParameter {
        TypeParameter {
            name: name.into(),
            variance: Variance::Invariant,
            reference_type_constraint: false,
            value_type_constraint: false,
            default_constructor_constraint: false,
            is_unmanaged: false,
            allows_ref_struct: false,
            nullability: Nullability::Oblivious,
            type_constraints: Vec::new(),
        }
    }

    /// A type entity with `arity` (dummy) generic parameters and no members.
    fn ent(namespace: &[&str], name: &str, arity: usize) -> Entity {
        Entity {
            extension_member_names: Vec::new(),
            union_case_names: None,
            static_extension_member_names: Vec::new(),
            is_extension_container: false,
            assembly: ai(),
            namespace: namespace.iter().map(|s| s.to_string()).collect(),
            name: name.into(),
            kind: EntityKind::Class,
            access: Access::Public,
            generic_parameters: (0..arity).map(|i| typar(&format!("T{i}"))).collect(),
            base_type: None,
            interfaces: Vec::new(),
            members: Vec::new(),
            skipped_members: Vec::new(),
            method_def_tokens: Vec::new(),
            is_sealed: false,
            nested_types: Vec::new(),
            is_readonly: false,
            is_byref_like: false,
            is_struct: false,
            is_auto_open: false,
            is_require_qualified_access: false,
            is_no_equality: false,
            is_no_comparison: false,
            is_structural_equality: false,
            is_structural_comparison: false,
            is_allow_null_literal: false,
            obsolete: None,
            experimental: None,
            default_member: None,
            compiler_feature_required: Vec::new(),
            source_name: None,
            custom_attrs: Vec::new(),
            abbreviation_target: None,
        }
    }

    fn decl(namespace: &[&str], name: &str, arity: usize) -> TypeDocName {
        type_doc_name(&ent(namespace, name, arity), None)
    }

    fn prim(p: Primitive) -> TypeRef {
        TypeRef::Primitive(p)
    }

    fn named(namespace: &[&str], name: &str, args: Vec<TypeRef>) -> TypeRef {
        // These callers use single-segment names, so the whole arg list belongs
        // to that one segment: `segment_arities = [args.len()]`. (Constructed
        // *nested* generics, which split the args across segments, are built
        // explicitly — see `constructed_nested_generic_distributes_args`.)
        let segment_count = name.split('/').count();
        let mut segment_arities = vec![0usize; segment_count];
        if let Some(last) = segment_arities.last_mut() {
            *last = args.len();
        }
        TypeRef::Named {
            assembly: None,
            namespace: namespace.iter().map(|s| s.to_string()).collect(),
            name: name.into(),
            type_args: args.into_iter().map(NullableType::oblivious).collect(),
            segment_arities,
        }
    }

    fn vector(element: TypeRef) -> TypeRef {
        TypeRef::Array {
            element: Box::new(NullableType::oblivious(element)),
            rank: 1,
            sizes: Vec::new(),
            lower_bounds: Vec::new(),
        }
    }

    fn md_array(element: TypeRef, rank: u8) -> TypeRef {
        TypeRef::Array {
            element: Box::new(NullableType::oblivious(element)),
            rank,
            sizes: Vec::new(),
            lower_bounds: Vec::new(),
        }
    }

    fn param(ty: TypeRef) -> Parameter {
        Parameter {
            name: Some("p".into()),
            ty,
            is_byref: false,
            is_out: false,
            is_readonly_ref: false,
            default: ParamDefault::None,
            is_param_array: false,
            nullability: Nullability::Oblivious,
        }
    }

    fn byref_param(ty: TypeRef) -> Parameter {
        Parameter {
            is_byref: true,
            ..param(ty)
        }
    }

    fn method(
        name: &str,
        arity: usize,
        parameters: Vec<Parameter>,
        return_type: TypeRef,
    ) -> Member {
        Member::Method(MethodLike {
            name: name.into(),
            access: Access::Public,
            signature: MethodSignature {
                parameters,
                return_type,
                return_nullability: Nullability::Oblivious,
            },
            arg_group_count: Some(1),
            is_static: false,
            is_virtual: false,
            is_abstract: false,
            is_constructor: name == ".ctor" || name == ".cctor",
            module_value: None,
            is_module_value_binding: false,
            is_extension_method: false,
            augmentation: crate::model::Augmentation::No,
            is_final: false,
            is_newslot: false,
            is_hide_by_sig: false,
            generic_parameters: (0..arity).map(|i| typar(&format!("M{i}"))).collect(),
            obsolete: None,
            experimental: None,
            sets_required_members: false,
            compiler_feature_required: Vec::new(),
            source_name: None,
            custom_attrs: Vec::new(),
            metadata_token: 0,
            implements: Vec::new(),
            unclassified_impls: Vec::new(),
        })
    }

    fn field(name: &str, ty: TypeRef) -> Member {
        Member::Field(Field {
            name: name.into(),
            access: Access::Public,
            ty,
            is_static: false,
            is_init_only: false,
            is_volatile: false,
            is_literal: false,
            is_required: false,
            compiler_feature_required: Vec::new(),
            nullability: Nullability::Oblivious,
            custom_attrs: Vec::new(),
        })
    }

    fn property(name: &str, ty: TypeRef, index_params: Vec<TypeRef>) -> Member {
        Member::Property(Property {
            name: name.into(),
            access: Access::Public,
            ty,
            parameters: index_params
                .into_iter()
                .map(|ty| IndexParameter {
                    name: None,
                    ty: NullableType::oblivious(ty),
                    is_param_array: false,
                })
                .collect(),
            is_static: false,
            has_getter: true,
            has_setter: false,
            getter_access: Some(Access::Public),
            is_required: false,
            compiler_feature_required: Vec::new(),
            nullability: Nullability::Oblivious,
            custom_attrs: Vec::new(),
            implements: Vec::new(),
            unclassified_impls: Vec::new(),
        })
    }

    fn event(name: &str, delegate_type: TypeRef) -> Member {
        Member::Event(Event {
            name: name.into(),
            access: Access::Public,
            delegate_type,
            is_static: false,
            has_fire: false,
            nullability: Nullability::Oblivious,
            custom_attrs: Vec::new(),
            implements: Vec::new(),
            unclassified_impls: Vec::new(),
        })
    }

    // ---------------------------------- types ----------------------------------

    #[test]
    fn type_id_top_level() {
        assert_eq!(
            decl(&["System"], "Console", 0).type_id(),
            "T:System.Console"
        );
    }

    #[test]
    fn type_id_global_namespace() {
        assert_eq!(decl(&[], "Program", 0).type_id(), "T:Program");
    }

    #[test]
    fn type_id_generic_keeps_arity_suffix() {
        assert_eq!(
            decl(&["System", "Collections", "Generic"], "List", 1).type_id(),
            "T:System.Collections.Generic.List`1"
        );
    }

    #[test]
    fn nested_type_subtracts_encloser_arity() {
        // `Dictionary`2.Enumerator` — the nested type introduces no parameters
        // of its own, but inherits both of the encloser's, so its name carries
        // no `` `n `` despite a cumulative arity of 2.
        let outer = decl(&["System", "Collections", "Generic"], "Dictionary", 2);
        let inner = type_doc_name(&ent(&[], "Enumerator", 2), Some(&outer));
        assert_eq!(
            inner.type_id(),
            "T:System.Collections.Generic.Dictionary`2.Enumerator"
        );
    }

    #[test]
    fn nested_generic_type_uses_own_arity() {
        // `Box`1.Inner`1` — Inner inherits Box's one parameter and adds one.
        let outer = decl(&["N"], "Box", 1);
        let inner = type_doc_name(&ent(&[], "Inner", 2), Some(&outer));
        assert_eq!(inner.type_id(), "T:N.Box`1.Inner`1");
    }

    // --------------------------------- methods ---------------------------------

    #[test]
    fn method_no_params_has_no_parens() {
        let d = decl(&["System"], "Console", 0);
        assert_eq!(
            member_doc_id(&d, &method("Beep", 0, vec![], prim(Primitive::Void))),
            "M:System.Console.Beep"
        );
    }

    #[test]
    fn method_primitive_param() {
        let d = decl(&["System"], "Console", 0);
        assert_eq!(
            member_doc_id(
                &d,
                &method(
                    "WriteLine",
                    0,
                    vec![param(prim(Primitive::String))],
                    prim(Primitive::Void)
                )
            ),
            "M:System.Console.WriteLine(System.String)"
        );
    }

    #[test]
    fn method_vector_array_param() {
        let d = decl(&["N"], "C", 0);
        assert_eq!(
            member_doc_id(
                &d,
                &method(
                    "M",
                    0,
                    vec![param(vector(prim(Primitive::I4)))],
                    prim(Primitive::Void)
                )
            ),
            "M:N.C.M(System.Int32[])"
        );
    }

    #[test]
    fn method_multidim_array_param() {
        let d = decl(&["N"], "C", 0);
        assert_eq!(
            member_doc_id(
                &d,
                &method(
                    "M",
                    0,
                    vec![param(md_array(prim(Primitive::I4), 2))],
                    prim(Primitive::Void)
                )
            ),
            "M:N.C.M(System.Int32[0:,0:])"
        );
    }

    #[test]
    fn method_byref_param_appends_at() {
        let d = decl(&["N"], "C", 0);
        assert_eq!(
            member_doc_id(
                &d,
                &method(
                    "TryGet",
                    0,
                    vec![byref_param(prim(Primitive::Bool))],
                    prim(Primitive::Void)
                )
            ),
            "M:N.C.TryGet(System.Boolean@)"
        );
    }

    #[test]
    fn method_byref_of_method_typar_array() {
        // The `System.Array.Resize``1(``0[]@,System.Int32)` shape: a byref to an
        // array of the method's own type parameter.
        let d = decl(&["System"], "Array", 0);
        let resize = method(
            "Resize",
            1,
            vec![
                byref_param(vector(TypeRef::Var {
                    index: 0,
                    is_method: true,
                })),
                param(prim(Primitive::I4)),
            ],
            prim(Primitive::Void),
        );
        assert_eq!(
            member_doc_id(&d, &resize),
            "M:System.Array.Resize``1(``0[]@,System.Int32)"
        );
    }

    #[test]
    fn generic_method_arity_and_method_typar() {
        let d = decl(&["N"], "C", 0);
        let m = method(
            "Id",
            1,
            vec![param(TypeRef::Var {
                index: 0,
                is_method: true,
            })],
            TypeRef::Var {
                index: 0,
                is_method: true,
            },
        );
        assert_eq!(member_doc_id(&d, &m), "M:N.C.Id``1(``0)");
    }

    #[test]
    fn method_generic_instantiation_param_uses_braces() {
        let d = decl(&["System", "Linq"], "Enumerable", 0);
        let m = method(
            "All",
            1,
            vec![param(named(
                &["System", "Collections", "Generic"],
                "IEnumerable",
                vec![TypeRef::Var {
                    index: 0,
                    is_method: true,
                }],
            ))],
            prim(Primitive::Bool),
        );
        assert_eq!(
            member_doc_id(&d, &m),
            "M:System.Linq.Enumerable.All``1(System.Collections.Generic.IEnumerable{``0})"
        );
    }

    #[test]
    fn method_on_generic_type_references_type_typar() {
        // `List`1.Add(`0)` — the parameter is the *type's* first typar.
        let d = decl(&["System", "Collections", "Generic"], "List", 1);
        let m = method(
            "Add",
            0,
            vec![param(TypeRef::Var {
                index: 0,
                is_method: false,
            })],
            prim(Primitive::Void),
        );
        assert_eq!(
            member_doc_id(&d, &m),
            "M:System.Collections.Generic.List`1.Add(`0)"
        );
    }

    #[test]
    fn constructor_names() {
        let d = decl(&["N"], "C", 0);
        assert_eq!(
            member_doc_id(&d, &method(".ctor", 0, vec![], prim(Primitive::Void))),
            "M:N.C.#ctor"
        );
        assert_eq!(
            member_doc_id(
                &d,
                &method(
                    ".ctor",
                    0,
                    vec![param(prim(Primitive::I4))],
                    prim(Primitive::Void)
                )
            ),
            "M:N.C.#ctor(System.Int32)"
        );
        assert_eq!(
            member_doc_id(&d, &method(".cctor", 0, vec![], prim(Primitive::Void))),
            "M:N.C.#cctor"
        );
    }

    #[test]
    fn conversion_operators_encode_return_type() {
        let d = decl(&["N"], "Money", 0);
        let op = method(
            "op_Explicit",
            0,
            vec![param(named(&["N"], "Money", vec![]))],
            prim(Primitive::I4),
        );
        assert_eq!(
            member_doc_id(&d, &op),
            "M:N.Money.op_Explicit(N.Money)~System.Int32"
        );
    }

    #[test]
    fn ordinary_operator_has_no_return_suffix() {
        let d = decl(&["N"], "Money", 0);
        let op = method(
            "op_Addition",
            0,
            vec![
                param(named(&["N"], "Money", vec![])),
                param(named(&["N"], "Money", vec![])),
            ],
            named(&["N"], "Money", vec![]),
        );
        assert_eq!(
            member_doc_id(&d, &op),
            "M:N.Money.op_Addition(N.Money,N.Money)"
        );
    }

    #[test]
    fn pointer_and_void_pointer() {
        let d = decl(&["N"], "C", 0);
        assert_eq!(
            member_doc_id(
                &d,
                &method(
                    "P",
                    0,
                    vec![param(TypeRef::Ptr(Some(Box::new(prim(Primitive::I4)))))],
                    prim(Primitive::Void)
                )
            ),
            "M:N.C.P(System.Int32*)"
        );
        assert_eq!(
            member_doc_id(
                &d,
                &method(
                    "V",
                    0,
                    vec![param(TypeRef::Ptr(None))],
                    prim(Primitive::Void)
                )
            ),
            "M:N.C.V(System.Void*)"
        );
    }

    #[test]
    fn checked_conversion_operator_encodes_return_type() {
        let d = decl(&["N"], "Money", 0);
        let op = method(
            "op_CheckedExplicit",
            0,
            vec![param(named(&["N"], "Money", vec![]))],
            prim(Primitive::I4),
        );
        assert_eq!(
            member_doc_id(&d, &op),
            "M:N.Money.op_CheckedExplicit(N.Money)~System.Int32"
        );
    }

    #[test]
    fn explicit_interface_method_name_is_escaped() {
        // The reader stores the constructed-interface-qualified IL name verbatim
        // (`Ns.IStore<System.Int32>.Store`); the ID escapes `.`→`#`, `<`→`{`,
        // `>`→`}`.
        let d = decl(&["Ns"], "IntStore", 0);
        let m = method(
            "Ns.IStore<System.Int32>.Store",
            0,
            vec![param(prim(Primitive::I4))],
            prim(Primitive::Void),
        );
        assert_eq!(
            member_doc_id(&d, &m),
            "M:Ns.IntStore.Ns#IStore{System#Int32}#Store(System.Int32)"
        );
    }

    #[test]
    fn explicit_interface_property_name_is_escaped() {
        let d = decl(&["Ns"], "IntStore", 0);
        let p = property("Ns.IStore<System.Int32>.Count", prim(Primitive::I4), vec![]);
        assert_eq!(
            member_doc_id(&d, &p),
            "P:Ns.IntStore.Ns#IStore{System#Int32}#Count"
        );
    }

    #[test]
    fn explicit_interface_name_keeps_concrete_multi_arg_separator() {
        // A *multi-argument* generic interface instantiated with concrete types
        // keeps the `,` separator between the constructed arguments — Roslyn does
        // *not* rewrite it (the `@` separator only appears when the arguments are
        // the implementing type's own type *parameters*; that case is a separate,
        // not-yet-handled shape). An explicit impl of `ILookup<int,string>.Get`
        // keys as `…ILookup{System#Int32,System#String}#Get`. (Verified end-to-end
        // against Roslyn by `doc_id_diff.rs`.)
        let d = decl(&["Ns"], "IntStringLookup", 0);
        let m = method(
            "Ns.ILookup<System.Int32,System.String>.Get",
            0,
            vec![param(prim(Primitive::I4))],
            prim(Primitive::String),
        );
        assert_eq!(
            member_doc_id(&d, &m),
            "M:Ns.IntStringLookup.Ns#ILookup{System#Int32,System#String}#Get(System.Int32)"
        );
    }

    #[test]
    fn constructed_nested_generic_distributes_args() {
        // A constructed nested generic distributes its type arguments across the
        // declaring segments by `segment_arities`, the way Roslyn keys them:
        // `Outer<String,Int32>.View<Bool>` → `Outer{…,…}.View{…}`, *not* the flat
        // `Outer.View{…,…,…}`. (Also covered end-to-end against Roslyn by
        // `doc_id_diff.rs`; this pins the encoder directly.)
        let d = decl(&["N"], "C", 0);
        let ty = TypeRef::Named {
            assembly: None,
            namespace: vec!["N".to_string()],
            name: "Outer`2/View`1".to_string(),
            type_args: vec![
                NullableType::oblivious(prim(Primitive::String)),
                NullableType::oblivious(prim(Primitive::I4)),
                NullableType::oblivious(prim(Primitive::Bool)),
            ],
            segment_arities: vec![2, 1],
        };
        let m = method("M", 0, vec![param(ty)], prim(Primitive::Void));
        assert_eq!(
            member_doc_id(&d, &m),
            "M:N.C.M(N.Outer{System.String,System.Int32}.View{System.Boolean})"
        );
    }

    #[test]
    fn constructed_nested_generic_with_inconsistent_arities_falls_back() {
        // `segment_arities` is *not* enforced to line up with the segments or the
        // arg count — corrupt metadata can violate it, and the projector records
        // what it read rather than panicking. When the arities don't account for
        // exactly the args present, the encoder must not panic: it falls back to
        // the naive "append the whole arg list after the dotted join" rendering.
        let d = decl(&["N"], "C", 0);
        let ty = TypeRef::Named {
            assembly: None,
            namespace: vec!["N".to_string()],
            name: "Outer`2/View`1".to_string(),
            // Sums to 2, but three args are present — a deliberate mismatch.
            type_args: vec![
                NullableType::oblivious(prim(Primitive::String)),
                NullableType::oblivious(prim(Primitive::I4)),
                NullableType::oblivious(prim(Primitive::Bool)),
            ],
            segment_arities: vec![1, 1],
        };
        let m = method("M", 0, vec![param(ty)], prim(Primitive::Void));
        assert_eq!(
            member_doc_id(&d, &m),
            "M:N.C.M(N.Outer.View{System.String,System.Int32,System.Boolean})"
        );
    }

    #[test]
    fn constructed_nested_generic_with_overflowing_arities_falls_back() {
        // `arity_suffix` parses a segment's `` `n `` into a `usize`, so a corrupt
        // assembly can carry per-segment arities whose *sum* overflows `usize`.
        // Summing them must not panic (debug) or wrap into a bogus "distributable"
        // verdict that then slices past `type_args` (release): the sum saturates,
        // fails the equality check, and takes the naive fallback like any other
        // inconsistent arity.
        let d = decl(&["N"], "C", 0);
        let ty = TypeRef::Named {
            assembly: None,
            namespace: vec!["N".to_string()],
            name: "Outer`2/View`1".to_string(),
            type_args: vec![
                NullableType::oblivious(prim(Primitive::String)),
                NullableType::oblivious(prim(Primitive::I4)),
                NullableType::oblivious(prim(Primitive::Bool)),
            ],
            segment_arities: vec![usize::MAX, usize::MAX],
        };
        let m = method("M", 0, vec![param(ty)], prim(Primitive::Void));
        assert_eq!(
            member_doc_id(&d, &m),
            "M:N.C.M(N.Outer.View{System.String,System.Int32,System.Boolean})"
        );
    }

    // --------------------------- fields / props / events -----------------------

    #[test]
    fn field_id() {
        let d = decl(&["N"], "C", 0);
        assert_eq!(
            member_doc_id(&d, &field("Value", prim(Primitive::I4))),
            "F:N.C.Value"
        );
    }

    #[test]
    fn fsharp_module_value_keys_as_property() {
        // An F# module value (`let nan = ...`) is an IL *property* rebranded to a
        // method (FCS's source view); the F# compiler's own XML keys it `P:`, so a
        // member marked `module_value` keys with `P:`, not the method's natural
        // `M:`. The rebranded value is zero-arg and non-generic, so only the
        // prefix differs.
        let d = decl(&["N"], "Module", 0);
        let Member::Method(mut value) = method("nan", 0, vec![], prim(Primitive::R8)) else {
            unreachable!("method() builds a Member::Method")
        };
        // Without the module-value mark it keys as an ordinary method.
        assert_eq!(
            member_doc_id(&d, &Member::Method(value.clone())),
            "M:N.Module.nan"
        );
        value.module_value = Some(ModuleValue { is_mutable: false });
        assert_eq!(member_doc_id(&d, &Member::Method(value)), "P:N.Module.nan");
    }

    #[test]
    fn property_id() {
        let d = decl(&["N"], "C", 0);
        assert_eq!(
            member_doc_id(&d, &property("Length", prim(Primitive::I4), vec![])),
            "P:N.C.Length"
        );
    }

    #[test]
    fn indexer_encodes_index_params() {
        let d = decl(&["N"], "C", 0);
        assert_eq!(
            member_doc_id(
                &d,
                &property("Item", prim(Primitive::String), vec![prim(Primitive::I4)])
            ),
            "P:N.C.Item(System.Int32)"
        );
    }

    #[test]
    fn event_id() {
        let d = decl(&["N"], "C", 0);
        assert_eq!(
            member_doc_id(
                &d,
                &event("Changed", named(&["System"], "EventHandler", vec![]))
            ),
            "E:N.C.Changed"
        );
    }

    #[test]
    fn nested_type_reference_joins_with_dot() {
        // A reference to `Outer/Inner` (reader's `/`-separated form) encodes
        // with `.` and demangled segments.
        let d = decl(&["N"], "C", 0);
        let m = method(
            "M",
            0,
            vec![param(named(
                &["System", "Collections", "Generic"],
                "List`1/Enumerator",
                vec![],
            ))],
            prim(Primitive::Void),
        );
        assert_eq!(
            member_doc_id(&d, &m),
            "M:N.C.M(System.Collections.Generic.List.Enumerator)"
        );
    }

    // ----------------------------------- walk ----------------------------------

    #[test]
    fn walk_collects_type_and_members_and_nested() {
        let mut outer = ent(&["N"], "Outer", 0);
        outer.members.push(field("F", prim(Primitive::I4)));
        outer.members.push(method(
            "M",
            0,
            vec![param(prim(Primitive::String))],
            prim(Primitive::Void),
        ));
        let mut inner = ent(&[], "Inner", 0);
        inner
            .members
            .push(method(".ctor", 0, vec![], prim(Primitive::Void)));
        outer.nested_types.push(inner);

        let mut ids = Vec::new();
        walk_doc_ids(&outer, None, &mut |id| ids.push(id));
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "F:N.Outer.F".to_string(),
                "M:N.Outer.Inner.#ctor".to_string(),
                "M:N.Outer.M(System.String)".to_string(),
                "T:N.Outer".to_string(),
                "T:N.Outer.Inner".to_string(),
            ]
        );
    }
}
